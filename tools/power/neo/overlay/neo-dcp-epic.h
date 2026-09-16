/* SPDX-License-Identifier: GPL-2.0-only OR MIT */
/* AFK wire layouts/protocol adapted from the Asahi Linux and m1n1 AFK
 * implementations. Copyright 2022 Sven Peter <sven@svenpeter.dev>.
 * This bounded retained-DCP experiment initially captures announcements;
 * it sends no display operation until the service has been identified.
 */
#include <linux/completion.h>
#include <linux/unaligned.h>

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
	struct neo_epic_ring tx, rx;
	bool active, started;
	u16 tag;
	u32 ring_size, announcement_size;
	u64 last_message;
	u8 announcement[4096];
	int error;
};

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
	    size <= 192 || off > e->ring_size || size > e->ring_size - off)
		return -EPROTO;
	if (other->header && off < other->offset + other->extent &&
	    other->offset < off + size)
		return -EPROTO;
	header = e->owned->pool + off;
	dma_rmb();
	bufsz = READ_ONCE(*(u32 *)header);
	if (bufsz != size - 192 || !IS_ALIGNED(bufsz, 64) ||
	    READ_ONCE(*(u32 *)(header + 64)) >= bufsz ||
	    READ_ONCE(*(u32 *)(header + 128)) >= bufsz ||
	    !IS_ALIGNED(READ_ONCE(*(u32 *)(header + 64)) |
			READ_ONCE(*(u32 *)(header + 128)), 64))
		return -EPROTO;
	r->header = header;
	r->data = header + 192;
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
		rp = READ_ONCE(*(u32 *)(r->header + 64));
		wp = READ_ONCE(*(u32 *)(r->header + 128));
		if (rp >= r->size || wp >= r->size || !IS_ALIGNED(rp | wp, 64))
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
		next = ALIGN(rp + 16 + size, 64);
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
		} else {
			return -EOPNOTSUPP;
		}
		dma_mb();
		WRITE_ONCE(*(u32 *)(r->header + 64), next == r->size ? 0 : next);
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
	case 0x85:
		ret = neo_epic_receive(e);
		break;
	default:
		ret = -EOPNOTSUPP;
	}
	if (ret) {
		e->error = ret;
		complete_all(&e->ready);
	}
	return ret;
}
