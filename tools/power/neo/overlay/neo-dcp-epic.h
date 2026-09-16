/* SPDX-License-Identifier: GPL-2.0-only OR MIT */
/* AFK wire layouts/protocol adapted from the Asahi Linux and m1n1 AFK
 * implementations. Copyright 2022 Sven Peter <sven@svenpeter.dev>.
 * This bounded retained-DCP experiment initially captures announcements;
 * it sends no display operation until the service has been identified.
 */
#include <linux/completion.h>
#include <linux/unaligned.h>

/* Pinned 25F84 INIT mode 8 selects compatibility config {64,64,0}
 * at 0x4373747 (selection 0x404198c). Mode 0 instead uses {128,128,0}
 * and newer compact messages. Mailbox size/offset units remain 64 bytes.
 */
#define NEO_EPIC_BLOCK 64
#define NEO_EPIC_HEADER (3 * NEO_EPIC_BLOCK)

struct neo_epic_ring {
	u8 *header, *data;
	u32 size, offset, extent;
};

struct neo_epic {
	struct device *dev;
	struct apple_rtkit *rtk;
	struct neo_dcp_owned *owned;
	struct mutex lock;
	struct completion ready;
	struct completion reply;
	struct completion stopped;
	struct neo_epic_ring tx, rx;
	bool active, started, stopping;
	u16 tag;
	u32 ring_size, announcement_size;
	u64 last_message;
	u8 announcement[4096];
	u8 response[128];
	u32 channel, response_size, retcode;
	u16 sequence;
	bool service, waiting, queried;
	u32 power_stage, power_cycles;
	int power_off_result, power_on_result;
	bool ap_cycle;
	int ap_idle_result, ap_wake_result;
	bool endpoint_cycle, iop_cycle;
	int endpoint_stop_result, endpoint_start_result;
	int error;
};

/* Bounded subset of the DCP OSSerialize grammar. Adapted from the parser
 * by Alyssa Rosenzweig, Copyright 2021, GPL-2.0-only OR MIT. Unknown nested
 * values are skipped structurally; class matching never searches raw bytes.
 */
struct neo_prop_cursor { const u8 *bytes; u32 size, pos; };
static int neo_prop_take(struct neo_prop_cursor *c, u32 n, const u8 **out)
{
	if (c->pos > c->size || n > c->size - c->pos)
		return -EINVAL;
	*out = c->bytes + c->pos;
	c->pos += n;
	return 0;
}
static int neo_prop_tag(struct neo_prop_cursor *c, u32 *tag)
{
	const u8 *p;
	c->pos = ALIGN(c->pos, 4);
	if (neo_prop_take(c, 4, &p))
		return -EINVAL;
	*tag = get_unaligned_le32(p);
	return *tag & GENMASK(30, 29) ? -EINVAL : 0;
}
static int neo_prop_value(struct neo_prop_cursor *c, u32 tag, unsigned int depth)
{
	u32 type = (tag >> 24) & 31, size = tag & 0xffffff, child, n;
	const u8 *p;
	if (depth > 8)
		return -E2BIG;
	switch (type) {
	case 1:
	case 2:
		if (size > 64)
			return -E2BIG;
		for (n = 0; n < size * (type == 1 ? 2 : 1); n++)
			if (neo_prop_tag(c, &child) || neo_prop_value(c, child, depth + 1))
				return -EINVAL;
		return 0;
	case 4:
		return size == 64 ? neo_prop_take(c, 8, &p) : -EINVAL;
	case 9:
	case 10:
		return neo_prop_take(c, size, &p);
	case 11:
		return size <= 1 ? 0 : -EINVAL;
	default:
		return -EOPNOTSUPP;
	}
}
static bool neo_prop_string(const u8 *p, u32 len, const char *expected)
{
	if (len && p[len - 1] == 0)
		len--;
	return len == strlen(expected) && !memcmp(p, expected, len);
}
static int neo_epic_identify(u32 channel, const u8 *payload, u32 length)
{
	struct neo_prop_cursor c;
	const u8 *key, *value;
	u32 tag, n, pairs, key_size, value_size;
	bool found = false, iboot_interface = false;

	if (length < 32)
		return -EINVAL;
	if (length == 32)
		return strnlen(payload, 32) == strlen("disp0-service") &&
		       !memcmp(payload, "disp0-service", strlen("disp0-service")) ? 0 : -ENODEV;
	c = (struct neo_prop_cursor){ .bytes = payload + 32, .size = length - 32 };
	if (neo_prop_take(&c, 4, &value) || get_unaligned_le32(value) != 0xd3 ||
	    neo_prop_tag(&c, &tag) || ((tag >> 24) & 31) != 1)
		return -EINVAL;
	pairs = tag & 0xffffff;
	if (pairs > 64)
		return -E2BIG;
	for (n = 0; n < pairs; n++) {
		if (neo_prop_tag(&c, &tag) || ((tag >> 24) & 31) != 9)
			return -EINVAL;
		key_size = tag & 0xffffff;
		if (neo_prop_take(&c, key_size, &key) || neo_prop_tag(&c, &tag))
			return -EINVAL;
		if (neo_prop_string(key, key_size, "EPICProviderClass")) {
			if (found || ((tag >> 24) & 31) != 9)
				return -EINVAL;
			value_size = tag & 0xffffff;
			if (neo_prop_take(&c, value_size, &value) ||
			    !neo_prop_string(value, value_size, "disp0-service"))
				return -ENODEV;
			found = true;
		} else if (neo_prop_string(key, key_size, "interface-id")) {
			if (iboot_interface || ((tag >> 24) & 31) != 4 ||
			    (tag & 0xffffff) != 64 || neo_prop_take(&c, 8, &value) ||
			    (!channel || get_unaligned_le32(value) != channel) || get_unaligned_le32(value + 4))
				return -EINVAL;
			iboot_interface = true;
		} else if (neo_prop_value(&c, tag, 0)) {
			return -EINVAL;
		}
	}
	/* Native iBoot uses name + a 36-byte interface-id dictionary. The
	 * upstream AFK matcher likewise uses the name for properties <=36
	 * bytes; here additionally require interface-id to match the channel.
	 * Firmware assigns a new channel when this endpoint is restarted.
	 */
	return found || (iboot_interface && pairs == 1 && length == 68 &&
			strnlen(payload, 32) == strlen("disp0-service") &&
			!memcmp(payload, "disp0-service", strlen("disp0-service"))) ? 0 : -ENODEV;
}

static int neo_epic_send(struct neo_epic *e, u64 word)
{
	return apple_rtkit_send_message(e->rtk, 0x23, word, NULL, false);
}

static int neo_epic_ring_init(struct neo_epic *e, struct neo_epic_ring *r,
			      struct neo_epic_ring *other, u64 word)
{
	u32 off = ((word >> 32) & 0xffff) * 64;
	u32 size = ((word >> 16) & 0xffff) * 64, bufsz;
	u8 *header;

	if (r->header || (word & 0xffff) != e->tag || !e->ring_size ||
	    size <= NEO_EPIC_HEADER || off > e->ring_size || size > e->ring_size - off)
		return -EPROTO;
	if (other->header && off < other->offset + other->extent &&
	    other->offset < off + size)
		return -EPROTO;
	header = e->owned->pool + off;
	dma_rmb();
	bufsz = READ_ONCE(*(u32 *)header);
	if (get_unaligned_le32(header + 4) != 0x00070006 ||
	    bufsz != size - NEO_EPIC_HEADER || !IS_ALIGNED(bufsz, NEO_EPIC_BLOCK) ||
	    READ_ONCE(*(u32 *)(header + NEO_EPIC_BLOCK)) >= bufsz ||
	    READ_ONCE(*(u32 *)(header + 2 * NEO_EPIC_BLOCK)) >= bufsz ||
	    !IS_ALIGNED(READ_ONCE(*(u32 *)(header + NEO_EPIC_BLOCK)) |
			READ_ONCE(*(u32 *)(header + 2 * NEO_EPIC_BLOCK)), NEO_EPIC_BLOCK))
		return -EPROTO;
	r->header = header;
	r->data = header + NEO_EPIC_HEADER;
	r->offset = off;
	r->extent = size;
	r->size = bufsz;
	dev_info(e->dev, "DCP EPIC ring offset=%#x extent=%#x data=%#x\n", off, size, bufsz);
	return 0;
}

/* Consume at most 64 bounded frames per mailbox notification. Retain the
 * whole first announce frame for independent parser/identity review.
 */
static int neo_epic_receive(struct neo_epic *e)
{
	struct neo_epic_ring *r = &e->rx;
	u32 rp, wp, size, next, type, channel;
	unsigned int n;
	u8 *q, *sub;

	if (!r->header)
		return -EPROTO;
	for (n = 0; n < 64; n++) {
		dma_rmb();
		rp = READ_ONCE(*(u32 *)(r->header + NEO_EPIC_BLOCK));
		wp = READ_ONCE(*(u32 *)(r->header + 2 * NEO_EPIC_BLOCK));
		if (rp >= r->size || wp >= r->size || !IS_ALIGNED(rp | wp, NEO_EPIC_BLOCK))
			return -EPROTO;
		if (rp == wp)
			return 0;
		q = r->data + rp;
		if (get_unaligned_le32(q) != 0x20504f49)
			return -EPROTO;
		size = get_unaligned_le32(q + 4);
		if (size < 40 || size > sizeof(e->announcement) - 16 || size + 16 >= r->size)
			return -EMSGSIZE;
		if (size + 16 > r->size - rp) {
			if (wp >= rp)
				return -EPROTO;
			rp = 0;
			q = r->data;
			if (get_unaligned_le32(q) != 0x20504f49 ||
			    get_unaligned_le32(q + 4) != size)
				return -EPROTO;
		}
		next = ALIGN(rp + 16 + size, NEO_EPIC_BLOCK);
		if (next > r->size || (rp < wp && next > wp))
			return -EPROTO;
		type = get_unaligned_le32(q + 12);
		channel = get_unaligned_le32(q + 8);
		sub = q + 32;
		if (q[16] != 2 || sub[4] != 4 || get_unaligned_le32(sub) > size - 40)
			return -EPROTO;
		dev_info(e->dev, "DCP EPIC RX channel=%u type=%u category=%#x subtype=%#x size=%u\n",
			 channel, type, sub[5], get_unaligned_le16(sub + 6), size);
		if ((type == 0 || type == 4) && sub[5] == 0 &&
		    get_unaligned_le16(sub + 6) == 0x30) {
			if (e->announcement_size)
				return -EEXIST;
			memcpy(e->announcement, q, size + 16);
			e->announcement_size = size + 16;
			if (!neo_epic_identify(channel, sub + 24, get_unaligned_le32(sub))) {
				e->service = true;
				e->channel = channel;
			}
		} else if ((type == 0 || type == 4) && sub[5] == 0x20 &&
			   get_unaligned_le16(sub + 6) == 0xc0 &&
			   channel == e->channel && e->waiting) {
			const u8 *cmd = sub + 24;
			u64 expected = BIT_ULL(40) | (e->owned->iova + NEO_DCP_POOL_SIZE - SZ_16K);
			u32 received;
			if (get_unaligned_le32(sub) < 30 || get_unaligned_le16(sub + 16))
				return -EPROTO;
			e->retcode = get_unaligned_le32(cmd);
			received = get_unaligned_le32(cmd + 20);
			if (received > SZ_16K ||
			    (received && get_unaligned_le64(cmd + 4) != expected))
				return -EPROTO;
			e->response_size = min_t(u32, received, sizeof(e->response));
			dma_rmb();
			memcpy(e->response, e->owned->pool + NEO_DCP_POOL_SIZE - SZ_16K, e->response_size);
			e->waiting = false;
			complete_all(&e->reply);
		} else {
			return -EOPNOTSUPP;
		}
		dma_mb();
		WRITE_ONCE(*(u32 *)(r->header + NEO_EPIC_BLOCK), next == r->size ? 0 : next);
		if (e->started && e->announcement_size)
			complete_all(&e->ready);
	}
	return -EOVERFLOW;
}

static int neo_epic_message(struct neo_epic *e, u64 word)
{
	int ret = 0;
	u32 size;

	guard(mutex)(&e->lock);
	if (!e->active || e->error)
		return -EIO;
	e->last_message = word;
	dev_info(e->dev, "DCP EPIC mailbox RX %#llx\n", word);
	switch (word >> 48) {
	case 0xa0:
		break;
	case 0x89:
		size = ((word >> 16) & 0xffff) * 64;
		if (e->ring_size || !size || size > NEO_DCP_POOL_SIZE - 2*SZ_16K) {
			ret = -EMSGSIZE;
			break;
		}
		e->ring_size = size;
		e->tag = word;
		/* Once the address may have reached firmware, the pool must stay
		 * allocated/mapped even if an acknowledgement times out.
		 */
		e->owned->advertised = true;
		dma_wmb();
		ret = neo_epic_send(e, (0xa1ULL << 48) | BIT_ULL(40) | e->owned->iova);
		break;
	case 0x8a:
	case 0x8b:
		ret = word >> 48 == 0x8a ? neo_epic_ring_init(e, &e->tx, &e->rx, word) :
			neo_epic_ring_init(e, &e->rx, &e->tx, word);
		if (!ret && e->tx.header && e->rx.header)
			ret = neo_epic_send(e, 0xa3ULL << 48);
		break;
	case 0x86:
		if (!e->tx.header || !e->rx.header) {
			ret = -EPROTO;
			break;
		}
		e->started = true;
		ret = neo_epic_receive(e);
		if (!ret && e->announcement_size)
			complete_all(&e->ready);
		break;
	case 0xc1:
		if (!e->stopping || !e->started) {
			ret = -EPROTO;
			break;
		}
		e->started = false;
		e->stopping = false;
		complete_all(&e->stopped);
		break;
	case 0x85:
		ret = neo_epic_receive(e);
		break;
	default:
		ret = -EOPNOTSUPP;
	}
	if (ret) {
		e->error = ret;
		complete_all(&e->ready);
		complete_all(&e->reply);
		complete_all(&e->stopped);
	}
	return ret;
}

/* Exactly one command may be outstanding; the caller holds its lifecycle
 * mutex across this function. A timeout poisons the session so a late reply
 * cannot be mistaken for another request. Shared buffers remain pinned.
 */
static int neo_epic_command(struct neo_epic *e, u32 op, bool power)
{
	u8 message[86] = {}, *hdr = message + 16, *sub = message + 32, *cmd = message + 56;
	struct neo_epic_ring *r = &e->tx;
	u8 *request = e->owned->pool + NEO_DCP_POOL_SIZE - 2*SZ_16K;
	u32 rp, wp, next, txsize = op == 2 ? 17 : 16;
	u64 tx = BIT_ULL(40) | (e->owned->iova + NEO_DCP_POOL_SIZE - 2*SZ_16K);
	u64 rx = tx + SZ_16K;
	int ret = 0;

	mutex_lock(&e->lock);
	if (!e->service || !e->started || e->error || e->waiting || (op != 2 && op != 3)) {
		ret = -EBUSY;
		goto unlock;
	}
	dma_rmb();
	rp = READ_ONCE(*(u32 *)(r->header + NEO_EPIC_BLOCK));
	wp = READ_ONCE(*(u32 *)(r->header + 2 * NEO_EPIC_BLOCK));
	if (rp >= r->size || wp >= r->size || !IS_ALIGNED(rp | wp, NEO_EPIC_BLOCK)) {
		ret = -EPROTO;
		goto unlock;
	}
	/* Admission requires an empty TX ring. Handle the single wrap form
	 * used by AFK without overwriting unread data or tail padding.
	 */
	if (rp != wp || r->size < 256) {
		ret = -EBUSY;
		goto unlock;
	}
	memset(request, 0, 2*SZ_16K);
	put_unaligned_le32(op, request);
	put_unaligned_le32(txsize, request + 4);
	request[16] = power;
	put_unaligned_le32(0x20504f49, message);
	put_unaligned_le32(70, message + 4);
	put_unaligned_le32(e->channel, message + 8);
	put_unaligned_le32(3, message + 12);
	hdr[0] = 2;
	put_unaligned_le16(e->sequence++, hdr + 1);
	put_unaligned_le32(30, sub);
	sub[4] = 4;
	sub[5] = 0x30;
	put_unaligned_le16(0xc0, sub + 6);
	/* SET_POWER sets response length to zero (25F84 0x42e16d8).
	 * Do not offer an RX buffer: AFKEPCommandContext's buffer-copy path
	 * asserts on a zero-byte copy at 0x404c224. HPD returns 20 bytes.
	 */
	put_unaligned_le64(op == 3 ? rx : 0, cmd + 4);
	put_unaligned_le64(tx, cmd + 12);
	put_unaligned_le32(op == 3 ? SZ_16K : 0, cmd + 20);
	put_unaligned_le32(txsize, cmd + 24);
	if (r->size - wp < sizeof(message)) {
		memcpy(r->data + wp, message, 16);
		wp = 0;
	}
	memcpy(r->data + wp, message, sizeof(message));
	next = ALIGN(wp + sizeof(message), NEO_EPIC_BLOCK);
	if (next == r->size)
		next = 0;
	reinit_completion(&e->reply);
	e->response_size = 0;
	e->retcode = 0;
	e->waiting = true;
	dma_wmb();
	WRITE_ONCE(*(u32 *)(r->header + 2 * NEO_EPIC_BLOCK), next);
	ret = neo_epic_send(e, (0xa2ULL << 48) | next);
	if (ret)
		e->error = ret;
unlock:
	mutex_unlock(&e->lock);
	if (ret)
		return ret;
	if (!wait_for_completion_timeout(&e->reply, msecs_to_jiffies(5000))) {
		guard(mutex)(&e->lock);
		e->error = -ETIMEDOUT;
		return e->error;
	}
	guard(mutex)(&e->lock);
	return e->error ?: (e->retcode ? -EREMOTEIO : 0);
}

/* AFK SHUTDOWN/ACK follows m1n1 afk_epic_shutdown_ep and Linux afk_shutdown.
 * This is endpoint teardown, not the shared-memory notifications visible
 * before native macOS idle. Keep DMA mapped even after acknowledgement.
 * Caller serializes the complete lifecycle using the device mutex.
 */
static int neo_epic_stop(struct neo_epic *e)
{
	int ret;

	mutex_lock(&e->lock);
	if (!e->started || e->waiting || e->stopping || e->error) {
		mutex_unlock(&e->lock);
		return -EBUSY;
	}
	reinit_completion(&e->stopped);
	e->stopping = true;
	ret = neo_epic_send(e, 0xc0ULL << 48);
	mutex_unlock(&e->lock);
	if (!ret && !wait_for_completion_timeout(&e->stopped, msecs_to_jiffies(5000)))
		ret = -ETIMEDOUT;
	guard(mutex)(&e->lock);
	ret = ret ?: e->error;
	if (ret)
		e->error = ret;
	return ret;
}

static int neo_epic_restart(struct neo_epic *e)
{
	int ret;

	mutex_lock(&e->lock);
	if (e->started || e->stopping || e->waiting || e->error) {
		mutex_unlock(&e->lock);
		return -EBUSY;
	}
	/* Only an acknowledged shutdown allows forgetting the old ring layout.
	 * The mapping and pool stay alive; GETBUF re-advertises the same pool.
	 */
	e->tx = (struct neo_epic_ring){};
	e->rx = (struct neo_epic_ring){};
	e->ring_size = e->announcement_size = e->response_size = 0;
	e->channel = e->retcode = e->sequence = e->tag = 0;
	e->service = e->queried = false;
	reinit_completion(&e->ready);
	ret = neo_epic_send(e, (0x80ULL << 48) | 8);
	mutex_unlock(&e->lock);
	if (!ret && !wait_for_completion_timeout(&e->ready, msecs_to_jiffies(15000)))
		ret = -ETIMEDOUT;
	guard(mutex)(&e->lock);
	ret = ret ?: e->error;
	if (!ret && (!e->started || !e->service))
		ret = -ENODEV;
	if (ret)
		e->error = ret;
	return ret;
}
