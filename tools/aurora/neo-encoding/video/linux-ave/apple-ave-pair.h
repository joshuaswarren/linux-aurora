/* SPDX-License-Identifier: GPL-2.0-only */
/* Bounded diagnostic MV transaction: two Process packets, then one Complete
 * for each successful Process reply. No persistent queue or unowned memory.
 * Included after apple-ave-ipc.h. Every operation holds ave->lock.
 */
#define AVE_PAIR_MAGIC 0x32505641 /* AVP2, little endian */
#define AVE_PAIR_PROCESS_BYTES 0xb1c0
#define AVE_PAIR_COMPLETE_BYTES 0x48
#define AVE_PAIR_HEADER_BYTES 20
#define AVE_PAIR_SLOT_BYTES SZ_64K

static int ave_pair_send(struct apple_ave *ave, unsigned int slot, u32 bytes)
{
	u32 offset = AVE_COMMAND_OFF + slot * AVE_PAIR_SLOT_BYTES;
	u8 *cmd = ave->ipc.cpu + offset;
	int ret;

	put_unaligned_le64(3000000, cmd + 0x30);
	put_unaligned_le64(div_u64(ktime_get_ns(), 1000), cmd + 0x38);
	ret = ave_ring_send(ave, &ave->channel[0], ave->ipc_fw_base + offset, bytes, 0);
	if (!ret)
		ave->commands++;
	return ret;
}

static bool ave_pair_reply_valid(const u8 *data, u32 bytes, u32 client, u32 opcode, u32 kind)
{
	for (u32 offset = 0; offset < bytes; offset += 4) {
		u32 expected = offset == 0 ? opcode : offset == 0x10 ? client :
			       offset == 0x20 ? kind : 0;

		if (get_unaligned_le32(data + offset) != expected)
			return false;
	}
	return true;
}

static ssize_t ave_pair_write(struct file *file, const char __user *data, size_t len, loff_t *pos)
{
	struct apple_ave *ave = file->private_data;
	u32 header[5], sizes[4] = {AVE_PAIR_PROCESS_BYTES, AVE_PAIR_PROCESS_BYTES,
		AVE_PAIR_COMPLETE_BYTES, AVE_PAIR_COMPLETE_BYTES};
	u32 cursor = AVE_PAIR_HEADER_BYTES, client = 0, submitted = 0, acks = 0;
	u32 processes = 0, completes = 0;
	u64 deadline, requests[4];
	int ret = 0;

	if (!capable(CAP_SYS_RAWIO))
		return -EPERM;
	static_assert(AVE_COMMAND_OFF + 4 * AVE_PAIR_SLOT_BYTES == AVE_ARENA_OFF);
	if (len != AVE_PAIR_HEADER_BYTES + 2 * AVE_PAIR_PROCESS_BYTES + 2 * AVE_PAIR_COMPLETE_BYTES)
		return -EINVAL;
	if (copy_from_user(header, data, sizeof(header)))
		return -EFAULT;
	if (le32_to_cpu((__force __le32)header[0]) != AVE_PAIR_MAGIC)
		return -EINVAL;
	for (unsigned int i = 0; i < 4; i++)
		if (le32_to_cpu((__force __le32)header[i + 1]) != sizes[i])
			return -EINVAL;
	mutex_lock(&ave->lock);
	if (!ave->ipc_ready || ave->command_failed) {
		ret = -EBUSY;
		goto out;
	}
	/* Validate all four packets before publishing any firmware command. */
	for (unsigned int i = 0; i < 4; i++) {
		u8 *cmd = ave->ipc.cpu + AVE_COMMAND_OFF + i * AVE_PAIR_SLOT_BYTES;
		u64 id;

		if (copy_from_user(cmd, data + cursor, sizes[i])) {
			ret = -EFAULT;
			goto out;
		}
		cursor += sizes[i];
		id = get_unaligned_le64(cmd + 0x10);
		if (!id || id > 0xffff || (i && id != client) ||
		    get_unaligned_le32(cmd) != (i < 2 ? 8 : 9) ||
		    get_unaligned_le32(cmd + 0x20) != (i < 2 ? 0x15 + i : 9)) {
			ret = -EINVAL;
			goto out;
		}
		client = id;
		requests[i] = get_unaligned_le64(cmd + 8);
		for (unsigned int j = 0; j < i; j++)
			if (requests[j] == requests[i]) {
				ret = -EINVAL;
				goto out;
			}
		if (i < 2 && (get_unaligned_le32(cmd + 0xac84) != i ||
			      get_unaligned_le32(cmd + 0xad34) != i)) {
			ret = -EINVAL;
			goto out;
		}
	}
	ave->response_bytes = 0;
	for (unsigned int i = 0; i < 2; i++) {
		ret = ave_pair_send(ave, i, sizes[i]);
		if (ret)
			goto failed;
		submitted |= BIT(i);
	}
	deadline = ktime_get_ns() + 5ULL * NSEC_PER_SEC;
	do {
		u32 pending = readl(ave->mailbox + 0x10);

		if (pending)
			writel(pending, ave->mailbox + 0x10);
		for (unsigned int ch = 0; ch < 2; ch++) {
			u64 address;
			u32 bytes, flags, offset;
			u8 *reply, *dst;

			ret = ave_ring_receive(ave, &ave->channel[ch], &address, &bytes, &flags);
			if (ret == -EAGAIN)
				continue;
			if (ret || ave_fw_offset(ave, address, bytes, &offset)) {
				ret = -EPROTO;
				goto failed;
			}
			if (!ch) {
				u32 delta = offset - AVE_COMMAND_OFF, slot = delta / AVE_PAIR_SLOT_BYTES;

				if (bytes || offset < AVE_COMMAND_OFF || delta % AVE_PAIR_SLOT_BYTES ||
				    slot >= 4 || !(submitted & BIT(slot)) || (acks & BIT(slot))) {
					ret = -EPROTO;
					goto failed;
				}
				acks |= BIT(slot);
				ave->transport_acks++;
				continue;
			}
			if ((bytes != 72 && bytes != 80) ||
			    ave->response_bytes > sizeof(ave->response) - 24 - bytes) {
				ret = -EPROTO;
				goto failed;
			}
			reply = ave->ipc.cpu + offset;
			dst = ave->response + ave->response_bytes;
			put_unaligned_le32(2, dst);
			put_unaligned_le32(bytes, dst + 4);
			put_unaligned_le32(flags, dst + 8);
			put_unaligned_le32(0, dst + 12);
			put_unaligned_le64(address, dst + 16);
			memcpy(dst + 24, reply, bytes);
			ave->response_bytes += 24 + bytes;
			ave->replies++;
			ret = ave_ring_send(ave, &ave->channel[ch], address, bytes, 0);
			if (ret)
				goto failed;
			/* Inspect our copied reply after returning the firmware slot. */
			reply = dst + 24;
			if (bytes == 80) {
				u32 view = get_unaligned_le32(reply + 0x20) - 0x15;

				if (view > 1 || (processes & BIT(view)) ||
				    !ave_pair_reply_valid(reply, bytes, client, 8, 0x15 + view)) {
					ret = -EPROTO;
					goto failed;
				}
				processes |= BIT(view);
				ret = ave_pair_send(ave, view + 2, sizes[view + 2]);
				if (ret)
					goto failed;
				submitted |= BIT(view + 2);
			} else {
				if (completes >= hweight32(submitted >> 2) ||
				    !ave_pair_reply_valid(reply, bytes, client, 9, 9)) {
					ret = -EPROTO;
					goto failed;
				}
				completes++;
			}
		}
		if (acks == 0xf && processes == 3 && completes == 2) {
			ret = 0;
			goto out;
		}
		usleep_range(100, 200);
	} while (ktime_get_ns() < deadline);
	ret = -ETIMEDOUT;
failed:
	ave->command_failed = true;
out:
	mutex_unlock(&ave->lock);
	return ret ?: len;
}

static const struct file_operations ave_pair_fops = {
	.owner = THIS_MODULE,
	.open = simple_open,
	.write = ave_pair_write,
	.read = ave_command_read,
};
