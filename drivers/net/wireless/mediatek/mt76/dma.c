// SPDX-License-Identifier: BSD-3-Clause-Clear
/*
 * Copyright (C) 2016 Felix Fietkau <nbd@nbd.name>
 */

#include <linux/dma-mapping.h>
#include "mt76.h"
#include "dma.h"
#include "mt76_connac.h"

#define MT7932_MCU_BOUNCE_STRIDE	8192
/* Apple 25G83's PCIe command ring has 512 entries.  A bounce pool may not be
 * smaller than the published descriptor ring even though the pre-runtime
 * stream currently contains only 77 acknowledged commands.
 */
/* J700's one-time calibration adds 560 commands after firmware boot. Keep the
 * complete batch distinct until TX17 is drained; the calibrated sender then
 * resets the cursor only after every descriptor reports DMA_DONE. */
#define MT7932_MCU_WM_BOUNCE_ENTRIES	1024
/* 25G83 needs 17 patch plus 583 RAM scatter payloads at 2 KiB. */
#define MT7932_MCU_FWDL_BOUNCE_ENTRIES	640

static struct mt76_txwi_cache *
mt76_alloc_txwi(struct mt76_dev *dev)
{
	struct mt76_txwi_cache *t;
	dma_addr_t addr;
	u8 *txwi;
	int size;

	size = L1_CACHE_ALIGN(dev->drv->txwi_size + sizeof(*t));
	txwi = kzalloc(size, GFP_ATOMIC);
	if (!txwi)
		return NULL;

	addr = dma_map_single(dev->dma_dev, txwi, dev->drv->txwi_size,
			      DMA_TO_DEVICE);
	if (unlikely(dma_mapping_error(dev->dma_dev, addr))) {
		kfree(txwi);
		return NULL;
	}

	t = (struct mt76_txwi_cache *)(txwi + dev->drv->txwi_size);
	t->dma_addr = addr;

	return t;
}

static struct mt76_txwi_cache *
mt76_alloc_rxwi(struct mt76_dev *dev)
{
	struct mt76_txwi_cache *t;

	t = kzalloc(L1_CACHE_ALIGN(sizeof(*t)), GFP_ATOMIC);
	if (!t)
		return NULL;

	t->ptr = NULL;
	return t;
}

static struct mt76_txwi_cache *
__mt76_get_txwi(struct mt76_dev *dev)
{
	struct mt76_txwi_cache *t = NULL;

	spin_lock(&dev->lock);
	if (!list_empty(&dev->txwi_cache)) {
		t = list_first_entry(&dev->txwi_cache, struct mt76_txwi_cache,
				     list);
		list_del(&t->list);
	}
	spin_unlock(&dev->lock);

	return t;
}

static struct mt76_txwi_cache *
__mt76_get_rxwi(struct mt76_dev *dev)
{
	struct mt76_txwi_cache *t = NULL;

	spin_lock_bh(&dev->wed_lock);
	if (!list_empty(&dev->rxwi_cache)) {
		t = list_first_entry(&dev->rxwi_cache, struct mt76_txwi_cache,
				     list);
		list_del(&t->list);
	}
	spin_unlock_bh(&dev->wed_lock);

	return t;
}

static struct mt76_txwi_cache *
mt76_get_txwi(struct mt76_dev *dev)
{
	struct mt76_txwi_cache *t = __mt76_get_txwi(dev);

	if (t)
		return t;

	return mt76_alloc_txwi(dev);
}

struct mt76_txwi_cache *
mt76_get_rxwi(struct mt76_dev *dev)
{
	struct mt76_txwi_cache *t = __mt76_get_rxwi(dev);

	if (t)
		return t;

	return mt76_alloc_rxwi(dev);
}
EXPORT_SYMBOL_GPL(mt76_get_rxwi);

void
mt76_put_txwi(struct mt76_dev *dev, struct mt76_txwi_cache *t)
{
	if (!t)
		return;

	spin_lock(&dev->lock);
	list_add(&t->list, &dev->txwi_cache);
	spin_unlock(&dev->lock);
}
EXPORT_SYMBOL_GPL(mt76_put_txwi);

int mt76_dma_prealloc_txwi(struct mt76_dev *dev, int count)
{
	struct mt76_txwi_cache *t;
	int i;

	for (i = 0; i < count; i++) {
		t = mt76_alloc_txwi(dev);
		if (!t)
			return -ENOMEM;
		mt76_put_txwi(dev, t);
	}

	dev_info(dev->dev,
		 "J700_MT7932_TXWI_PREALLOC_PASS: entries=%d bytes=%zu\n",
		 count, (size_t)count * dev->drv->txwi_size);

	return 0;
}
EXPORT_SYMBOL_GPL(mt76_dma_prealloc_txwi);

void
mt76_put_rxwi(struct mt76_dev *dev, struct mt76_txwi_cache *t)
{
	if (!t)
		return;

	spin_lock_bh(&dev->wed_lock);
	list_add(&t->list, &dev->rxwi_cache);
	spin_unlock_bh(&dev->wed_lock);
}
EXPORT_SYMBOL_GPL(mt76_put_rxwi);

static void
mt76_free_pending_txwi(struct mt76_dev *dev)
{
	struct mt76_txwi_cache *t;

	local_bh_disable();
	while ((t = __mt76_get_txwi(dev)) != NULL) {
		dma_unmap_single(dev->dma_dev, t->dma_addr, dev->drv->txwi_size,
				 DMA_TO_DEVICE);
		kfree(mt76_get_txwi_ptr(dev, t));
	}
	local_bh_enable();
}

void
mt76_free_pending_rxwi(struct mt76_dev *dev)
{
	struct mt76_txwi_cache *t;

	local_bh_disable();
	while ((t = __mt76_get_rxwi(dev)) != NULL) {
		if (t->ptr)
			mt76_put_page_pool_buf(t->ptr, false);
		kfree(t);
	}
	local_bh_enable();
}
EXPORT_SYMBOL_GPL(mt76_free_pending_rxwi);

static void
mt76_dma_queue_magic_cnt_init(struct mt76_dev *dev, struct mt76_queue *q)
{
	if (!mt76_queue_is_wed_rro(q))
		return;

	q->magic_cnt = 0;
	if (mt76_queue_is_wed_rro_ind(q)) {
		struct mt76_wed_rro_desc *rro_desc;
		u32 data1 = FIELD_PREP(RRO_IND_DATA1_MAGIC_CNT_MASK,
				       MT_DMA_WED_IND_CMD_CNT - 1);
		int i;

		rro_desc = (struct mt76_wed_rro_desc *)q->desc;
		for (i = 0; i < q->ndesc; i++) {
			struct mt76_wed_rro_ind *cmd;

			cmd = (struct mt76_wed_rro_ind *)&rro_desc[i];
			cmd->data1 = cpu_to_le32(data1);
		}
	} else if (mt76_queue_is_wed_rro_rxdmad_c(q)) {
		struct mt76_rro_rxdmad_c *dmad = (void *)q->desc;
		u32 data3 = FIELD_PREP(RRO_RXDMAD_DATA3_MAGIC_CNT_MASK,
				       MT_DMA_MAGIC_CNT - 1);
		int i;

		for (i = 0; i < q->ndesc; i++)
			dmad[i].data3 = cpu_to_le32(data3);
	}
}

/* A hung bus (e.g. after a PCIe AER error) reads 0xffffffff from every
 * register, so clamp an out-of-range index to the fallback to keep it from
 * corrupting q->head/q->tail.
 */
static int
mt76_dma_read_dma_idx(struct mt76_queue *q, int fallback)
{
	u32 idx = Q_READ(q, dma_idx);

	return idx < q->ndesc ? idx : fallback;
}

static void
mt76_dma_sync_idx(struct mt76_dev *dev, struct mt76_queue *q)
{
	if ((q->flags & MT_QFLAG_WED_RRO_EN) &&
	    (!is_mt7992(dev) || !mt76_npu_device_active(dev)))
		Q_WRITE(q, ring_size, MT_DMA_RRO_EN | q->ndesc);
	else
		Q_WRITE(q, ring_size, q->ndesc);

	if (mt76_queue_is_npu_tx(q)) {
		writel(q->ndesc, &q->regs->ring_size);
		writel(q->desc_dma, &q->regs->desc_base);
	}

	Q_WRITE(q, desc_base, q->desc_dma);

	q->head = mt76_dma_read_dma_idx(q, 0);
	q->tail = q->head;
}

void mt76_dma_queue_reset(struct mt76_dev *dev, struct mt76_queue *q,
			  bool reset_idx)
{
	if (!q || !q->ndesc)
		return;

	if (!mt76_queue_is_wed_rro_ind(q) &&
	    !mt76_queue_is_wed_rro_rxdmad_c(q) && !mt76_queue_is_npu(q)) {
		int i;

		/* clear descriptors */
		for (i = 0; i < q->ndesc; i++)
			q->desc[i].ctrl = cpu_to_le32(MT_DMA_CTL_DMA_DONE);
	}

	mt76_dma_queue_magic_cnt_init(dev, q);
	if (reset_idx && mt76_chip(dev) == 0x7932 &&
	    !mt76_queue_is_emi(q)) {
		/* Run 191 publishes every disabled MT7932 WFDMA ring in this exact
		 * order: descriptor base, producer index, then entry count.  DIDX is
		 * device-owned and is reset later by WPDMA_RST_DTX_PTR.  Generic mt76
		 * instead wrote CIDX, DIDX, count and base, which can seed a different
		 * hidden payload/prefetch q-status image while EXT0[15] backup/restore
		 * is enabled.
		 */
		Q_WRITE(q, desc_base, q->desc_dma);
		Q_WRITE(q, cpu_idx, 0);
		Q_WRITE(q, ring_size, q->ndesc);
		q->head = Q_READ(q, dma_idx);
		q->tail = q->head;
		return;
	}
	if (reset_idx) {
		if (mt76_queue_is_emi(q))
			*q->emi_cpu_idx = 0;
		else
			Q_WRITE(q, cpu_idx, 0);
		Q_WRITE(q, dma_idx, 0);
	}
	mt76_dma_sync_idx(dev, q);
}

static int
mt76_dma_add_rx_buf(struct mt76_dev *dev, struct mt76_queue *q,
		    struct mt76_queue_buf *buf, void *data)
{
	struct mt76_queue_entry *entry = &q->entry[q->head];
	struct mt76_txwi_cache *txwi = NULL;
	u32 buf1 = 0, ctrl, info = 0;
	struct mt76_desc *desc;
	int idx = q->head;
	int rx_token;

	if (mt76_queue_is_wed_rro_ind(q)) {
		struct mt76_wed_rro_desc *rro_desc;

		rro_desc = (struct mt76_wed_rro_desc *)q->desc;
		data = &rro_desc[q->head];
		goto done;
	} else if (mt76_queue_is_wed_rro_rxdmad_c(q)) {
		data = &q->desc[q->head];
		goto done;
	}

	desc = &q->desc[q->head];
	ctrl = FIELD_PREP(MT_DMA_CTL_SD_LEN0, buf[0].len);
#ifdef CONFIG_ARCH_DMA_ADDR_T_64BIT
	buf1 = FIELD_PREP(MT_DMA_CTL_SDP0_H, buf->addr >> 32);
#endif

	if (mt76_queue_is_wed_rx(q) || mt76_queue_is_wed_rro_data(q)) {
		txwi = mt76_get_rxwi(dev);
		if (!txwi)
			return -ENOMEM;

		rx_token = mt76_rx_token_consume(dev, data, txwi, buf->addr);
		if (rx_token < 0) {
			mt76_put_rxwi(dev, txwi);
			return -ENOMEM;
		}

		buf1 |= FIELD_PREP(MT_DMA_CTL_TOKEN, rx_token);
		ctrl |= MT_DMA_CTL_TO_HOST;

		txwi->qid = q - dev->q_rx;
	}

	if (mt76_queue_is_wed_rro_msdu_pg(q) &&
	    dev->drv->rx_rro_add_msdu_page) {
		if (dev->drv->rx_rro_add_msdu_page(dev, q, buf->addr, data))
			return -ENOMEM;
	}

	if (q->flags & MT_QFLAG_WED_RRO_EN) {
		info |= FIELD_PREP(MT_DMA_MAGIC_MASK, q->magic_cnt);
		if ((q->head + 1) == q->ndesc)
			q->magic_cnt = (q->magic_cnt + 1) % MT_DMA_MAGIC_CNT;
	}

	WRITE_ONCE(desc->buf0, cpu_to_le32(buf->addr));
	WRITE_ONCE(desc->buf1, cpu_to_le32(buf1));
	WRITE_ONCE(desc->ctrl, cpu_to_le32(ctrl));
	WRITE_ONCE(desc->info, cpu_to_le32(info));

done:
	entry->dma_addr[0] = buf->addr;
	entry->dma_len[0] = buf->len;
	entry->txwi = txwi;
	entry->buf = data;
	entry->wcid = 0xffff;
	entry->skip_buf1 = true;
	q->head = (q->head + 1) % q->ndesc;
	q->queued++;

	return idx;
}

static int
mt76_dma_add_buf(struct mt76_dev *dev, struct mt76_queue *q,
		 struct mt76_queue_buf *buf, int nbufs, u32 info,
		 struct sk_buff *skb, void *txwi)
{
	struct mt76_queue_entry *entry;
	struct mt76_desc *desc;
	int i, idx = -1;
	u32 ctrl, next;

	if (txwi) {
		q->entry[q->head].txwi = DMA_DUMMY_DATA;
		q->entry[q->head].skip_buf0 = true;
	}

	for (i = 0; i < nbufs; i += 2, buf += 2) {
		u32 buf0 = buf[0].addr, buf1 = 0;

		idx = q->head;
		next = (q->head + 1) % q->ndesc;

		desc = &q->desc[idx];
		entry = &q->entry[idx];

		if (buf[0].skip_unmap)
			entry->skip_buf0 = true;
		entry->skip_buf1 = i == nbufs - 1;

		entry->dma_addr[0] = buf[0].addr;
		entry->dma_len[0] = buf[0].len;

		ctrl = FIELD_PREP(MT_DMA_CTL_SD_LEN0, buf[0].len);
#ifdef CONFIG_ARCH_DMA_ADDR_T_64BIT
		info |= FIELD_PREP(MT_DMA_CTL_SDP0_H, buf[0].addr >> 32);
#endif
		if (i < nbufs - 1) {
			entry->dma_addr[1] = buf[1].addr;
			entry->dma_len[1] = buf[1].len;
			buf1 = buf[1].addr;
			ctrl |= FIELD_PREP(MT_DMA_CTL_SD_LEN1, buf[1].len);
#ifdef CONFIG_ARCH_DMA_ADDR_T_64BIT
			info |= FIELD_PREP(MT_DMA_CTL_SDP1_H,
					   buf[1].addr >> 32);
#endif
			if (buf[1].skip_unmap)
				entry->skip_buf1 = true;
		}

		if (i == nbufs - 1)
			ctrl |= MT_DMA_CTL_LAST_SEC0;
		else if (i == nbufs - 2)
			ctrl |= MT_DMA_CTL_LAST_SEC1;

		WRITE_ONCE(desc->buf0, cpu_to_le32(buf0));
		WRITE_ONCE(desc->buf1, cpu_to_le32(buf1));
		WRITE_ONCE(desc->info, cpu_to_le32(info));
		WRITE_ONCE(desc->ctrl, cpu_to_le32(ctrl));

		q->head = next;
		q->queued++;
	}

	q->entry[idx].txwi = txwi;
	q->entry[idx].skb = skb;
	q->entry[idx].wcid = 0xffff;

	return idx;
}

static void
mt76_dma_tx_cleanup_idx(struct mt76_dev *dev, struct mt76_queue *q, int idx,
			struct mt76_queue_entry *prev_e)
{
	struct mt76_queue_entry *e = &q->entry[idx];

	if (!e->skip_buf0)
		dma_unmap_single(dev->dma_dev, e->dma_addr[0], e->dma_len[0],
				 DMA_TO_DEVICE);

	if (!e->skip_buf1)
		dma_unmap_single(dev->dma_dev, e->dma_addr[1], e->dma_len[1],
				 DMA_TO_DEVICE);

	if (e->txwi == DMA_DUMMY_DATA)
		e->txwi = NULL;

	*prev_e = *e;
	memset(e, 0, sizeof(*e));
}

static void
mt76_dma_kick_queue(struct mt76_dev *dev, struct mt76_queue *q)
{
	u32 cidx = q->head;

	/* Apple publishes all MT7932 RX descriptors.  Its producer register is
	 * therefore the last device-owned slot, not generic mt76's next empty
	 * slot.  The first post-restart refill must write 0 after the initial
	 * ndesc-1 publication; writing 1 leaves firmware one response behind.
	 */
	if (q->rx_full_ring)
		cidx = (q->head + q->ndesc - 1) % q->ndesc;
	wmb();
	if (mt76_queue_is_emi(q))
		*q->emi_cpu_idx = cpu_to_le16(cidx);
	else
		Q_WRITE(q, cpu_idx, cidx);
}

static void
mt76_dma_tx_cleanup(struct mt76_dev *dev, struct mt76_queue *q, bool flush)
{
	struct mt76_queue_entry entry;
	bool mt7932_cmd;
	int last;

	if (!q || !q->ndesc)
		return;

	spin_lock_bh(&q->cleanup_lock);
	/* MT7932's 24-entry TX17 command ring can have DIDX equal the
	 * software tail after wrapping while the completed prefix is still
	 * visible through per-descriptor DDONE.  Apple ignores DIDX here and
	 * halWpdmaProcessCmdDmaDone() walks DDONE until the first live entry.
	 */
	mt7932_cmd = !flush && mt76_chip(dev) == 0x7932 &&
		     q->hw_idx == 17 && q->tx_bounce_buf;
	if (flush)
		last = -1;
	else
		last = mt76_dma_read_dma_idx(q, -1);

	while (q->queued > 0) {
		if (mt7932_cmd) {
			__le32 ctrl = READ_ONCE(q->desc[q->tail].ctrl);

			if (!(ctrl & cpu_to_le32(MT_DMA_CTL_DMA_DONE)))
				break;
			dma_rmb();
			WRITE_ONCE(q->desc[q->tail].ctrl,
				   ctrl & ~cpu_to_le32(MT_DMA_CTL_DMA_DONE));
		} else if (q->tail == last) {
			break;
		}

		mt76_dma_tx_cleanup_idx(dev, q, q->tail, &entry);
		mt76_npu_txdesc_cleanup(q, q->tail);
		mt76_queue_tx_complete(dev, q, &entry);

		if (entry.txwi) {
			if (!(dev->drv->drv_flags & MT_DRV_TXWI_NO_FREE))
				mt76_put_txwi(dev, entry.txwi);
		}

		if (!flush && !mt7932_cmd && q->tail == last)
			last = Q_READ(q, dma_idx);
	}
	spin_unlock_bh(&q->cleanup_lock);

	if (flush) {
		spin_lock_bh(&q->lock);
		mt76_dma_sync_idx(dev, q);
		mt76_dma_kick_queue(dev, q);
		spin_unlock_bh(&q->lock);
	}

	if (!q->queued)
		wake_up(&dev->tx_wait);
}

static void *
mt76_dma_get_rxdmad_c_buf(struct mt76_dev *dev, struct mt76_queue *q,
			  int idx, int *len, bool *more)
{
	struct mt76_queue_entry *e = &q->entry[idx];
	struct mt76_rro_rxdmad_c *dmad = e->buf;
	u32 data1 = le32_to_cpu(dmad->data1);
	u32 data2 = le32_to_cpu(dmad->data2);
	struct mt76_txwi_cache *t;
	u16 rx_token_id;
	u8 ind_reason;
	void *buf;

	rx_token_id = FIELD_GET(RRO_RXDMAD_DATA2_RX_TOKEN_ID_MASK, data2);
	t = mt76_rx_token_release(dev, rx_token_id);
	if (!t)
		return ERR_PTR(-EAGAIN);

	q = &dev->q_rx[t->qid];
	dma_sync_single_for_cpu(dev->dma_dev, t->dma_addr,
				SKB_WITH_OVERHEAD(q->buf_size),
				page_pool_get_dma_dir(q->page_pool));

	if (len)
		*len = FIELD_GET(RRO_RXDMAD_DATA1_SDL0_MASK, data1);
	if (more)
		*more = !FIELD_GET(RRO_RXDMAD_DATA1_LS_MASK, data1);

	buf = t->ptr;
	ind_reason = FIELD_GET(RRO_RXDMAD_DATA2_IND_REASON_MASK, data2);
	if (ind_reason == MT_DMA_WED_IND_REASON_REPEAT ||
	    ind_reason == MT_DMA_WED_IND_REASON_OLDPKT) {
		mt76_put_page_pool_buf(buf, false);
		buf = ERR_PTR(-EAGAIN);
	}
	t->ptr = NULL;
	t->dma_addr = 0;

	mt76_put_rxwi(dev, t);

	return buf;
}

static void *
mt76_dma_get_buf(struct mt76_dev *dev, struct mt76_queue *q, int idx,
		 int *len, u32 *info, bool *more, bool *drop, bool flush)
{
	struct mt76_queue_entry *e = &q->entry[idx];
	struct mt76_desc *desc = &q->desc[idx];
	u32 ctrl, desc_info, buf1;
	void *buf = e->buf;

	if (mt76_queue_is_wed_rro_rxdmad_c(q) && !flush)
		buf = mt76_dma_get_rxdmad_c_buf(dev, q, idx, len, more);

	if (mt76_queue_is_wed_rro(q))
		goto done;

	ctrl = le32_to_cpu(READ_ONCE(desc->ctrl));
	if (len) {
		*len = FIELD_GET(MT_DMA_CTL_SD_LEN0, ctrl);
		*more = !(ctrl & MT_DMA_CTL_LAST_SEC0);
	}

	desc_info = le32_to_cpu(desc->info);
	if (info)
		*info = desc_info;

	buf1 = le32_to_cpu(desc->buf1);
	mt76_dma_should_drop_buf(drop, ctrl, buf1, desc_info);

	if (mt76_queue_is_wed_rx(q)) {
		u32 token = FIELD_GET(MT_DMA_CTL_TOKEN, buf1);
		struct mt76_txwi_cache *t = mt76_rx_token_release(dev, token);

		if (!t)
			return NULL;

		dma_sync_single_for_cpu(dev->dma_dev, t->dma_addr,
				SKB_WITH_OVERHEAD(q->buf_size),
				page_pool_get_dma_dir(q->page_pool));

		buf = t->ptr;
		t->dma_addr = 0;
		t->ptr = NULL;

		mt76_put_rxwi(dev, t);
#ifdef CONFIG_NET_MEDIATEK_SOC_WED
		/* the WO MCU owns the RX path only on WED v2, on newer
		 * versions this buf1 bit carries no drop information
		 */
		if (drop && dev->mmio.wed.version == 2)
			*drop |= !!(buf1 & MT_DMA_CTL_WO_DROP);
#endif
	} else {
		dma_sync_single_for_cpu(dev->dma_dev, e->dma_addr[0],
				SKB_WITH_OVERHEAD(q->buf_size),
				page_pool_get_dma_dir(q->page_pool));
	}

done:
	e->buf = NULL;
	return buf;
}

static void *
mt76_dma_dequeue(struct mt76_dev *dev, struct mt76_queue *q, bool flush,
		 int *len, u32 *info, bool *more, bool *drop)
{
	int idx = q->tail;

	*more = false;
	if (!q->queued)
		return NULL;

	if (mt76_queue_is_wed_rro_data(q) || mt76_queue_is_wed_rro_msdu_pg(q))
		goto done;

	if (mt76_queue_is_wed_rro_ind(q)) {
		struct mt76_wed_rro_ind *cmd;
		u8 magic_cnt;

		if (flush)
			goto done;

		cmd = q->entry[idx].buf;
		magic_cnt = FIELD_GET(RRO_IND_DATA1_MAGIC_CNT_MASK,
				      le32_to_cpu(cmd->data1));
		if (magic_cnt != q->magic_cnt)
			return NULL;

		if (q->tail == q->ndesc - 1)
			q->magic_cnt = (q->magic_cnt + 1) % MT_DMA_WED_IND_CMD_CNT;
	} else if (mt76_queue_is_wed_rro_rxdmad_c(q)) {
		struct mt76_rro_rxdmad_c *dmad;
		u16 magic_cnt;

		if (flush)
			goto done;

		dmad = q->entry[idx].buf;
		magic_cnt = FIELD_GET(RRO_RXDMAD_DATA3_MAGIC_CNT_MASK,
				      le32_to_cpu(dmad->data3));
		if (magic_cnt != q->magic_cnt)
			return NULL;

		if (q->tail == q->ndesc - 1)
			q->magic_cnt = (q->magic_cnt + 1) % MT_DMA_MAGIC_CNT;
	} else {
		if (flush)
			q->desc[idx].ctrl |= cpu_to_le32(MT_DMA_CTL_DMA_DONE);
		else if (!(q->desc[idx].ctrl & cpu_to_le32(MT_DMA_CTL_DMA_DONE)))
			return NULL;
#ifdef CONFIG_NET_MEDIATEK_SOC_WED
		/* on WED v3 the M_DONE bit signals that WED is done reading
		 * the txfree descriptor; WED v2 does not set it
		 */
		else if (dev->mmio.wed.version > 2 &&
			 mt76_queue_is_wed_tx_free(q) &&
			 !(q->desc[idx].ctrl & cpu_to_le32(MT_DMA_CTL_M_DONE)))
			return NULL;
#endif
	}
done:
	q->tail = (q->tail + 1) % q->ndesc;
	q->queued--;

	return mt76_dma_get_buf(dev, q, idx, len, info, more, drop, flush);
}

static int
mt76_dma_tx_queue_skb_raw(struct mt76_dev *dev, struct mt76_queue *q,
			  struct sk_buff *skb, u32 tx_info)
{
	struct mt76_queue_buf buf = {};
	dma_addr_t addr;
	void *bounce;
	u32 bounce_slot;

	if (test_bit(MT76_MCU_RESET, &dev->phy.state))
		goto error;

	if (mt76_chip(dev) == 0x7932 && q->hw_idx == 17) {
		/* Apple accepts the 24-entry command ring while used + 1 is
		 * strictly below 24, allowing 23 live descriptors.  Generic mt76
		 * reserves a second slot and rejects at 22.
		 */
		if (q->queued + 1 >= q->ndesc)
			goto error;
	} else if (q->queued + 1 >= q->ndesc - 1) {
		goto error;
	}

	if (q->tx_stream_active) {
		if (skb->len > q->tx_stream_stride)
			goto error;

		/* Run 582 shows Apple retaining the earlier TX17 payload mappings
		 * after their descriptors have completed.  A normal skb mapping has
		 * the right DART2 read-only attribute, but mt76 cleanup unmaps it and
		 * frees the skb too early.  Copy each runtime command into a unique,
		 * stable slot, map it DMA_TO_DEVICE once, and retain that mapping for
		 * the lifetime of the device.
		 */
		bounce_slot = READ_ONCE(q->tx_stream_next);
		if (bounce_slot >= q->tx_stream_entries ||
		    q->tx_stream_dma_len[bounce_slot])
			goto error;
		bounce = q->tx_stream_buf[bounce_slot];
		memcpy(bounce, skb->data, skb->len);
		addr = dma_map_single(dev->dma_dev, bounce, skb->len,
				      DMA_TO_DEVICE);
		if (unlikely(dma_mapping_error(dev->dma_dev, addr)))
			goto error;
		q->tx_stream_dma[bounce_slot] = addr;
		q->tx_stream_dma_len[bounce_slot] = skb->len;
		WRITE_ONCE(q->tx_stream_next, bounce_slot + 1);
		buf.skip_unmap = true;
		if (!q->tx_stream_logged) {
			dev_info(dev->dev,
				 "J700_MT7932_MCU_STREAM_POOL_ACTIVE: hw=%u entries=%u stride=%u first_dma=%pad direction=to-device lifetime=device\n",
				 q->hw_idx, q->tx_stream_entries,
				 q->tx_stream_stride, &addr);
			q->tx_stream_logged = true;
		}
	} else if (q->tx_bounce_buf) {
		if (skb->len > q->tx_bounce_stride)
			goto error;

		/* Cycle126 assigned each MCU command a fresh coherent slot for the
		 * entire firmware-download lifetime.  Descriptor cleanup retires the
		 * ring entry, but does not prove the device has stopped observing the
		 * payload through its internal pipeline.
		 */
		bounce_slot = READ_ONCE(q->tx_bounce_next);
		if (bounce_slot >= q->tx_bounce_entries)
			goto error;
		WRITE_ONCE(q->tx_bounce_next, bounce_slot + 1);
		bounce = (u8 *)q->tx_bounce_buf +
			 bounce_slot * q->tx_bounce_stride;
		memcpy(bounce, skb->data, skb->len);
		addr = q->tx_bounce_dma + bounce_slot * q->tx_bounce_stride;
		buf.skip_unmap = true;
		if (!q->tx_bounce_logged) {
			dev_info(dev->dev,
				 "J700_MT7932_MCU_BOUNCE_ACTIVE: hw=%u stride=%u\n",
				 q->hw_idx, q->tx_bounce_stride);
			q->tx_bounce_logged = true;
		}
	} else {
		addr = dma_map_single(dev->dma_dev, skb->data, skb->len,
				      DMA_TO_DEVICE);
		if (unlikely(dma_mapping_error(dev->dma_dev, addr)))
			goto error;
	}

	buf.addr = addr;
	buf.len = skb->len;

	spin_lock_bh(&q->lock);
	if (mt76_dma_add_buf(dev, q, &buf, 1, tx_info, skb, NULL) >= 0)
		mt76_dma_kick_queue(dev, q);
	spin_unlock_bh(&q->lock);

	return 0;

error:
	dev_kfree_skb(skb);
	return -ENOMEM;
}

static void
mt76_dma_free_tx_stream_pool(struct mt76_dev *dev, struct mt76_queue *q)
{
	u32 i;

	if (!q || !q->tx_stream_buf)
		return;

	for (i = 0; i < q->tx_stream_entries; i++) {
		if (!q->tx_stream_dma_len[i])
			continue;
		dma_unmap_single(dev->dma_dev, q->tx_stream_dma[i],
				 q->tx_stream_dma_len[i], DMA_TO_DEVICE);
		q->tx_stream_dma_len[i] = 0;
	}

	q->tx_stream_buf = NULL;
	q->tx_stream_dma = NULL;
	q->tx_stream_dma_len = NULL;
	q->tx_stream_stride = 0;
	q->tx_stream_entries = 0;
	q->tx_stream_next = 0;
	q->tx_stream_active = false;
	q->tx_stream_logged = false;
}

static int
mt76_dma_alloc_tx_stream_pool(struct mt76_dev *dev, struct mt76_queue *q,
			      u32 entries, u32 stride)
{
	u32 i;

	if (q->tx_stream_buf)
		return 0;
	if (entries < q->ndesc || !stride)
		return -EINVAL;

	q->tx_stream_buf = devm_kcalloc(dev->dev, entries,
					 sizeof(*q->tx_stream_buf), GFP_KERNEL);
	q->tx_stream_dma = devm_kcalloc(dev->dev, entries,
					 sizeof(*q->tx_stream_dma), GFP_KERNEL);
	q->tx_stream_dma_len = devm_kcalloc(dev->dev, entries,
					     sizeof(*q->tx_stream_dma_len),
					     GFP_KERNEL);
	if (!q->tx_stream_buf || !q->tx_stream_dma ||
	    !q->tx_stream_dma_len)
		goto error;

	q->tx_stream_stride = stride;
	q->tx_stream_entries = entries;
	for (i = 0; i < entries; i++) {
		q->tx_stream_buf[i] = devm_kzalloc(dev->dev, stride,
						   GFP_KERNEL);
		if (!q->tx_stream_buf[i])
			goto error;
	}

	dev_info(dev->dev,
		 "J700_MT7932_MCU_STREAM_POOL_READY: hw=%u ring_entries=%d pool_entries=%u stride=%u mapping=lazy-retained direction=to-device\n",
		 q->hw_idx, q->ndesc, q->tx_stream_entries,
		 q->tx_stream_stride);

	return 0;

error:
	mt76_dma_free_tx_stream_pool(dev, q);
	return -ENOMEM;
}

int mt76_dma_alloc_tx_bounce(struct mt76_dev *dev, struct mt76_queue *q,
			     u32 entries, u32 stride)
{
	size_t bounce_size;

	if (q->tx_bounce_buf)
		return 0;
	if (entries < q->ndesc || !stride)
		return -EINVAL;

	q->tx_bounce_stride = stride;
	q->tx_bounce_entries = entries;
	bounce_size = q->tx_bounce_entries * q->tx_bounce_stride;
	q->tx_bounce_buf = dmam_alloc_coherent(dev->dma_dev, bounce_size,
					       &q->tx_bounce_dma, GFP_KERNEL);
	if (!q->tx_bounce_buf)
		return -ENOMEM;

	dev_info(dev->dev,
		 "J700_MT7932_MCU_BOUNCE_POOL_PASS: hw=%u ring_entries=%d pool_entries=%u stride=%u dma=%pad bytes=%zu\n",
		 q->hw_idx, q->ndesc, q->tx_bounce_entries,
		 q->tx_bounce_stride, &q->tx_bounce_dma, bounce_size);

	return 0;
}
EXPORT_SYMBOL_GPL(mt76_dma_alloc_tx_bounce);

static int
mt76_dma_tx_queue_skb(struct mt76_phy *phy, struct mt76_queue *q,
		      enum mt76_txq_id qid, struct sk_buff *skb,
		      struct mt76_wcid *wcid, struct ieee80211_sta *sta)
{
	struct ieee80211_tx_status status = {
		.sta = sta,
	};
	struct mt76_tx_info tx_info = {
		.skb = skb,
	};
	struct mt76_dev *dev = phy->dev;
	struct ieee80211_hw *hw;
	int len, n = 0, ret = -ENOMEM;
	struct mt76_txwi_cache *t;
	struct sk_buff *iter;
	dma_addr_t addr;
	void *bounce;
	u32 bounce_slot;
	u8 *txwi;

	if (test_bit(MT76_RESET, &phy->state))
		goto free_skb;

	/* TODO: Take into account unlinear skbs */
	if (mt76_npu_device_active(dev) && skb_linearize(skb))
		goto free_skb;

	t = mt76_get_txwi(dev);
	if (!t)
		goto free_skb;

	t->phy_idx = phy->band_idx;
	t->qid = qid;
	txwi = mt76_get_txwi_ptr(dev, t);

	skb->prev = skb->next = NULL;
	if (dev->drv->drv_flags & MT_DRV_TX_ALIGNED4_SKBS)
		mt76_insert_hdr_pad(skb);

	if (q->tx_bounce_buf && mt76_chip(dev) == 0x7932) {
		if (skb_linearize(skb))
			goto free;
		len = skb->len;
		if (len > q->tx_bounce_stride ||
		    q->queued + 1 >= q->ndesc - 1)
			goto free;
		bounce_slot = READ_ONCE(q->head);
		if (bounce_slot >= q->tx_bounce_entries)
			goto free;
		bounce = (u8 *)q->tx_bounce_buf +
			 bounce_slot * q->tx_bounce_stride;
		memcpy(bounce, skb->data, len);
		addr = q->tx_bounce_dma + bounce_slot * q->tx_bounce_stride;
	} else {
		len = skb_headlen(skb);
		addr = dma_map_single(dev->dma_dev, skb->data, len,
				      DMA_TO_DEVICE);
		if (unlikely(dma_mapping_error(dev->dma_dev, addr)))
			goto free;
	}

	tx_info.buf[n].addr = t->dma_addr;
	tx_info.buf[n++].len = dev->drv->txwi_size;
	tx_info.buf[n].addr = addr;
	tx_info.buf[n++].len = len;
	if (q->tx_bounce_buf && mt76_chip(dev) == 0x7932)
		tx_info.buf[n - 1].skip_unmap = true;

	skb_walk_frags(skb, iter) {
		if (n == ARRAY_SIZE(tx_info.buf))
			goto unmap;

		addr = dma_map_single(dev->dma_dev, iter->data, iter->len,
				      DMA_TO_DEVICE);
		if (unlikely(dma_mapping_error(dev->dma_dev, addr)))
			goto unmap;

		tx_info.buf[n].addr = addr;
		tx_info.buf[n++].len = iter->len;
	}
	tx_info.nbuf = n;

	if (q->queued + (tx_info.nbuf + 1) / 2 >= q->ndesc - 1) {
		ret = -ENOMEM;
		goto unmap;
	}

	dma_sync_single_for_cpu(dev->dma_dev, t->dma_addr, dev->drv->txwi_size,
				DMA_TO_DEVICE);
	ret = dev->drv->tx_prepare_skb(dev, txwi, qid, wcid, sta, &tx_info);
	dma_sync_single_for_device(dev->dma_dev, t->dma_addr, dev->drv->txwi_size,
				   DMA_TO_DEVICE);
	if (ret < 0)
		goto unmap;

	if (mt76_npu_device_active(dev))
		return mt76_npu_dma_add_buf(phy, q, skb, &tx_info.buf[1], txwi);

	return mt76_dma_add_buf(dev, q, tx_info.buf, tx_info.nbuf,
				tx_info.info, tx_info.skb, t);

unmap:
	for (n--; n > 0; n--)
		if (!tx_info.buf[n].skip_unmap)
			dma_unmap_single(dev->dma_dev, tx_info.buf[n].addr,
					 tx_info.buf[n].len, DMA_TO_DEVICE);

free:
#ifdef CONFIG_NL80211_TESTMODE
	/* fix tx_done accounting on queue overflow */
	if (mt76_is_testmode_skb(dev, skb, &hw)) {
		struct mt76_phy *phy = hw->priv;

		if (tx_info.skb == phy->test.tx_skb)
			phy->test.tx_done--;
	}
#endif

	mt76_put_txwi(dev, t);

free_skb:
	status.skb = tx_info.skb;
	hw = mt76_tx_status_get_hw(dev, tx_info.skb);
	spin_lock_bh(&dev->rx_lock);
	ieee80211_tx_status_ext(hw, &status);
	spin_unlock_bh(&dev->rx_lock);

	return ret;
}

static int
mt76_dma_rx_fill_buf(struct mt76_dev *dev, struct mt76_queue *q,
		     bool allow_direct)
{
	int len = SKB_WITH_OVERHEAD(q->buf_size);
	int frames = 0;

	if (!q->ndesc)
		return 0;

	while (q->queued < q->ndesc - !q->rx_full_ring) {
		struct mt76_queue_buf qbuf = {};
		void *buf = NULL;
		int offset;

		if (mt76_queue_is_wed_rro_ind(q) ||
		    mt76_queue_is_wed_rro_rxdmad_c(q))
			goto done;

		buf = mt76_get_page_pool_buf(q, &offset, q->buf_size);
		if (!buf)
			break;

		qbuf.addr = page_pool_get_dma_addr(virt_to_head_page(buf)) +
			    offset + q->buf_offset;
done:
		qbuf.len = len - q->buf_offset;
		qbuf.skip_unmap = false;
		if (mt76_dma_add_rx_buf(dev, q, &qbuf, buf) < 0) {
			mt76_put_page_pool_buf(buf, allow_direct);
			break;
		}
		frames++;
	}

	if (frames || mt76_queue_is_wed_rx(q))
		mt76_dma_kick_queue(dev, q);

	return frames;
}

int mt76_dma_rx_fill(struct mt76_dev *dev, struct mt76_queue *q,
		     bool allow_direct)
{
	int frames;

	spin_lock_bh(&q->lock);
	frames = mt76_dma_rx_fill_buf(dev, q, allow_direct);
	spin_unlock_bh(&q->lock);

	return frames;
}

static int
mt76_dma_alloc_queue(struct mt76_dev *dev, struct mt76_queue *q,
		     int idx, int n_desc, int bufsize,
		     u32 ring_base)
{
	int ret, size;

	spin_lock_init(&q->lock);
	spin_lock_init(&q->cleanup_lock);

	q->regs = dev->mmio.regs + ring_base + idx * MT_RING_SIZE;
	q->ndesc = n_desc;
	q->buf_size = bufsize;
	q->hw_idx = idx;
	q->dev = dev;

	if (mt76_queue_is_wed_rro_ind(q))
		size = sizeof(struct mt76_wed_rro_desc);
	else if (mt76_queue_is_npu_tx(q))
		size = sizeof(struct airoha_npu_tx_dma_desc);
	else if (mt76_queue_is_npu_rx(q))
		size = sizeof(struct airoha_npu_rx_dma_desc);
	else
		size = sizeof(struct mt76_desc);

	q->desc = dmam_alloc_coherent(dev->dma_dev, q->ndesc * size,
				      &q->desc_dma, GFP_KERNEL);
	if (!q->desc)
		return -ENOMEM;

	mt76_dma_queue_magic_cnt_init(dev, q);
	size = q->ndesc * sizeof(*q->entry);
	q->entry = devm_kzalloc(dev->dev, size, GFP_KERNEL);
	if (!q->entry)
		return -ENOMEM;

	/* Cycle126 installed the coherent MCU payload mapping before publishing
	 * and resetting either firmware ring.  Preserve that ordering: mapping a
	 * pool after the ring is visible is not equivalent once ROM firmware has
	 * cached the device-side translation path.
	 */
	if (mt76_chip(dev) == 0x7932 && (idx == 16 || idx == 17)) {
		u32 entries = idx == 16 ? MT7932_MCU_FWDL_BOUNCE_ENTRIES :
					 MT7932_MCU_WM_BOUNCE_ENTRIES;

		ret = mt76_dma_alloc_tx_bounce(dev, q, entries,
					       MT7932_MCU_BOUNCE_STRIDE);
		if (ret)
			return ret;

		if (idx == 17) {
			ret = mt76_dma_alloc_tx_stream_pool(dev, q, entries,
						      max_t(u32,
							    MT7932_MCU_BOUNCE_STRIDE,
							    PAGE_SIZE));
			if (ret)
				return ret;
		}
	}

	ret = mt76_create_page_pool(dev, q);
	if (ret)
		return ret;

	mt76_npu_queue_setup(dev, q);
	ret = mt76_wed_dma_setup(dev, q, false);
	if (ret)
		return ret;

	if (mtk_wed_device_active(&dev->mmio.wed)) {
		if ((mtk_wed_get_rx_capa(&dev->mmio.wed) && mt76_queue_is_wed_rro(q)) ||
		    mt76_queue_is_wed_tx_free(q))
			return 0;
	}

	/* HW specific driver is supposed to reset brand-new EMI queues since
	 * it needs to set cpu index pointer.
	 */
	mt76_dma_queue_reset(dev, q, !mt76_queue_is_emi(q));

	return 0;
}

static void
mt76_dma_rx_cleanup(struct mt76_dev *dev, struct mt76_queue *q)
{
	void *buf;
	bool more;

	if (!q->ndesc)
		return;

	if (mt76_queue_is_npu(q)) {
		mt76_npu_queue_cleanup(dev, q);
		return;
	}

	do {
		spin_lock_bh(&q->lock);
		buf = mt76_dma_dequeue(dev, q, true, NULL, NULL, &more, NULL);
		spin_unlock_bh(&q->lock);

		if (!buf)
			break;

		if (mtk_wed_device_active(&dev->mmio.wed) &&
		    mt76_queue_is_wed_rro(q))
			continue;

		if (mt76_npu_device_active(dev) &&
		    mt76_queue_is_wed_rro(q))
			continue;

		if (!mt76_queue_is_wed_rro_rxdmad_c(q) &&
		    !mt76_queue_is_wed_rro_ind(q))
			mt76_put_page_pool_buf(buf, false);
	} while (1);

	spin_lock_bh(&q->lock);
	if (q->rx_head) {
		dev_kfree_skb(q->rx_head);
		q->rx_head = NULL;
	}

	spin_unlock_bh(&q->lock);
}

static void
mt76_dma_rx_reset(struct mt76_dev *dev, enum mt76_rxq_id qid)
{
	struct mt76_queue *q = &dev->q_rx[qid];

	if (!q->ndesc)
		return;

	if (!mt76_queue_is_wed_rro_ind(q) &&
	    !mt76_queue_is_wed_rro_rxdmad_c(q) && !mt76_queue_is_npu(q)) {
		int i;

		for (i = 0; i < q->ndesc; i++)
			q->desc[i].ctrl = cpu_to_le32(MT_DMA_CTL_DMA_DONE);
	}

	mt76_dma_rx_cleanup(dev, q);

	/* reset WED rx queues */
	mt76_wed_dma_setup(dev, q, true);

	if (mt76_queue_is_wed_tx_free(q))
		return;

	if (mtk_wed_device_active(&dev->mmio.wed) &&
	    mt76_queue_is_wed_rro(q))
		return;

	if (mt76_npu_device_active(dev) &&
	    mt76_queue_is_wed_rro(q))
		return;

	if (mt76_queue_is_npu_txfree(q))
		return;

	mt76_dma_sync_idx(dev, q);
	if (mt76_queue_is_npu(q))
		mt76_npu_fill_rx_queue(dev, q);
	else
		mt76_dma_rx_fill(dev, q, false);
}

int mt76_dma_rx_rebuild_existing(struct mt76_dev *dev,
				 enum mt76_rxq_id qid)
{
	struct mt76_queue *q = &dev->q_rx[qid];
	struct mt76_queue_buf qbuf = {};
	void *buf;
	int i, missing = -1, offset, ret;

	if (!q->ndesc)
		return 0;
	if (mt76_queue_is_wed_rro_ind(q) ||
	    mt76_queue_is_wed_rro_rxdmad_c(q) || mt76_queue_is_npu(q) ||
	    mt76_queue_is_wed_tx_free(q) || mt76_queue_is_npu_txfree(q))
		return -EOPNOTSUPP;

	/* AppleSunriseWLAN halWpdmaAllocRxRing(..., false) runs before
	 * halWpdmaInitRing disables WFDMA.  It keeps every existing RX DMA mapping,
	 * rebuilds all descriptors in place and supplies all ring entries.  Generic
	 * mt76 instead keeps one empty entry, then rx_cleanup unmaps every buffer and
	 * rx_fill allocates replacements after WFDMA is disabled.
	 *
	 * Preserve the existing mapped buffers and allocate only mt76's structural
	 * empty slot.  No queue MMIO is permitted here; the caller publishes
	 * BASE/CIDX/COUNT later, after WFDMA disable, exactly like Apple.
	 */
	spin_lock_bh(&q->lock);
	if (q->queued != q->ndesc - 1 || q->head >= q->ndesc) {
		ret = -EINVAL;
		goto unlock;
	}
	for (i = 0; i < q->ndesc; i++) {
		if (q->entry[i].buf)
			continue;
		if (missing >= 0) {
			ret = -EINVAL;
			goto unlock;
		}
		missing = i;
	}
	if (missing < 0 || missing != q->head) {
		ret = -EINVAL;
		goto unlock;
	}

	buf = mt76_get_page_pool_buf(q, &offset, q->buf_size);
	if (!buf) {
		ret = -ENOMEM;
		goto unlock;
	}
	qbuf.addr = page_pool_get_dma_addr(virt_to_head_page(buf)) + offset +
		    q->buf_offset;
	qbuf.len = SKB_WITH_OVERHEAD(q->buf_size) - q->buf_offset;
	if (mt76_dma_add_rx_buf(dev, q, &qbuf, buf) < 0) {
		mt76_put_page_pool_buf(buf, false);
		ret = -ENOMEM;
		goto unlock;
	}

	for (i = 0; i < q->ndesc; i++) {
		struct mt76_queue_entry *entry = &q->entry[i];
		struct mt76_desc *desc = &q->desc[i];
		u32 ctrl, buf1 = 0;

		if (!entry->buf || !entry->dma_len[0]) {
			ret = -EINVAL;
			goto unlock;
		}
		ctrl = le32_to_cpu(READ_ONCE(desc->ctrl));
		ctrl &= BIT(30) | GENMASK(15, 0);
		ctrl |= FIELD_PREP(MT_DMA_CTL_SD_LEN0, entry->dma_len[0]);
#ifdef CONFIG_ARCH_DMA_ADDR_T_64BIT
		buf1 = FIELD_PREP(MT_DMA_CTL_SDP0_H,
				  entry->dma_addr[0] >> 32);
#endif
		WRITE_ONCE(desc->buf0, cpu_to_le32(entry->dma_addr[0]));
		WRITE_ONCE(desc->ctrl, cpu_to_le32(ctrl));
		WRITE_ONCE(desc->buf1, cpu_to_le32(buf1));
		WRITE_ONCE(desc->info, 0);
	}
	q->head = 0;
	q->tail = 0;
	q->queued = q->ndesc;
	q->rx_full_ring = true;
	ret = 0;

unlock:
	spin_unlock_bh(&q->lock);
	if (!ret)
		dma_wmb();
	return ret;
}
EXPORT_SYMBOL_GPL(mt76_dma_rx_rebuild_existing);

static void
mt76_add_fragment(struct mt76_dev *dev, struct mt76_queue *q, void *data,
		  int len, bool more, u32 info, bool allow_direct)
{
	struct sk_buff *skb = q->rx_head;
	struct skb_shared_info *shinfo = skb_shinfo(skb);
	int nr_frags = shinfo->nr_frags;

	if (nr_frags < ARRAY_SIZE(shinfo->frags)) {
		struct page *page = virt_to_head_page(data);
		int offset = data - page_address(page) + q->buf_offset;

		skb_add_rx_frag(skb, nr_frags, page, offset, len, q->buf_size);
	} else {
		mt76_put_page_pool_buf(data, allow_direct);
	}

	if (more)
		return;

	q->rx_head = NULL;
	if (nr_frags < ARRAY_SIZE(shinfo->frags))
		dev->drv->rx_skb(dev, q - dev->q_rx, skb, &info);
	else
		dev_kfree_skb(skb);
}

static int
mt76_dma_rx_process(struct mt76_dev *dev, struct mt76_queue *q, int budget)
{
	int len, data_len, done = 0, dma_idx;
	struct sk_buff *skb;
	unsigned char *data;
	bool check_ddone = false;
	bool allow_direct = !mt76_queue_is_wed_rx(q) &&
			    !mt76_queue_is_wed_rro_rxdmad_c(q);
	bool more;

	if ((q->flags & MT_QFLAG_WED_RRO_EN) ||
	    (IS_ENABLED(CONFIG_NET_MEDIATEK_SOC_WED) &&
	     mt76_queue_is_wed_tx_free(q))) {
		dma_idx = mt76_dma_read_dma_idx(q, q->tail);
		check_ddone = true;
	}

	while (done < budget) {
		bool drop = false;
		u32 info = 0;

		if (check_ddone) {
			if (q->tail == dma_idx)
				dma_idx = mt76_dma_read_dma_idx(q, q->tail);

			if (q->tail == dma_idx)
				break;
		}

		data = mt76_dma_dequeue(dev, q, false, &len, &info, &more,
					&drop);
		if (!data)
			break;

		if (PTR_ERR(data) == -EAGAIN) {
			done++;
			continue;
		}

		if (mt76_queue_is_wed_rro_ind(q) && dev->drv->rx_rro_ind_process)
			dev->drv->rx_rro_ind_process(dev, data);

		if (mt76_queue_is_wed_rro(q) &&
		    !mt76_queue_is_wed_rro_rxdmad_c(q)) {
			done++;
			continue;
		}

		if (drop)
			goto free_frag;

		if (q->rx_head)
			data_len = q->buf_size;
		else
			data_len = SKB_WITH_OVERHEAD(q->buf_size);

		if (data_len < len + q->buf_offset) {
			dev_kfree_skb(q->rx_head);
			q->rx_head = NULL;
			goto free_frag;
		}

		if (q->rx_head) {
			mt76_add_fragment(dev, q, data, len, more, info,
					  allow_direct);
			continue;
		}

		if (!more && dev->drv->rx_check &&
		    !(dev->drv->rx_check(dev, data, len)))
			goto free_frag;

		skb = napi_build_skb(data, q->buf_size);
		if (!skb)
			goto free_frag;

		skb_reserve(skb, q->buf_offset);
		skb_mark_for_recycle(skb);

		*(u32 *)skb->cb = info;

		__skb_put(skb, len);
		done++;

		if (more) {
			q->rx_head = skb;
			continue;
		}

		dev->drv->rx_skb(dev, q - dev->q_rx, skb, &info);
		continue;

free_frag:
		mt76_put_page_pool_buf(data, allow_direct);
	}

	mt76_dma_rx_fill(dev, q, true);
	return done;
}

int mt76_dma_rx_poll(struct napi_struct *napi, int budget)
{
	struct mt76_dev *dev;
	int qid, done = 0, cur;

	dev = mt76_priv(napi->dev);
	qid = napi - dev->napi;

	rcu_read_lock();

	do {
		cur = mt76_dma_rx_process(dev, &dev->q_rx[qid], budget - done);
		mt76_rx_poll_complete(dev, qid, napi);
		done += cur;
	} while (cur && done < budget);

	rcu_read_unlock();

	if (done < budget && napi_complete(napi))
		dev->drv->rx_poll_complete(dev, qid);

	return done;
}
EXPORT_SYMBOL_GPL(mt76_dma_rx_poll);

int mt76_dma_rx_poll_sync(struct mt76_dev *dev, int budget)
{
	int qid, total = 0;

	rcu_read_lock();
	for (qid = 0; qid < ARRAY_SIZE(dev->q_rx); qid++) {
		struct mt76_queue *q = &dev->q_rx[qid];
		int done;

		if (!q->ndesc)
			continue;
		done = mt76_dma_rx_process(dev, q, budget);
		if (done)
			mt76_rx_poll_complete(dev, qid, NULL);
		total += done;
	}
	rcu_read_unlock();

	return total;
}
EXPORT_SYMBOL_GPL(mt76_dma_rx_poll_sync);

static void
mt76_dma_rx_queue_init(struct mt76_dev *dev, enum mt76_rxq_id qid,
		       int (*poll)(struct napi_struct *napi, int budget))
{
	netif_napi_add(dev->napi_dev, &dev->napi[qid], poll);
	mt76_dma_rx_fill_buf(dev, &dev->q_rx[qid], false);
	napi_enable(&dev->napi[qid]);
}

static int
mt76_dma_init(struct mt76_dev *dev,
	      int (*poll)(struct napi_struct *napi, int budget))
{
	struct mt76_dev **priv;
	int i;

	dev->napi_dev = alloc_netdev_dummy(sizeof(struct mt76_dev *));
	if (!dev->napi_dev)
		return -ENOMEM;

	/* napi_dev private data points to mt76_dev parent, so, mt76_dev
	 * can be retrieved given napi_dev
	 */
	priv = netdev_priv(dev->napi_dev);
	*priv = dev;

	dev->tx_napi_dev = alloc_netdev_dummy(sizeof(struct mt76_dev *));
	if (!dev->tx_napi_dev) {
		free_netdev(dev->napi_dev);
		return -ENOMEM;
	}
	priv = netdev_priv(dev->tx_napi_dev);
	*priv = dev;

	snprintf(dev->napi_dev->name, sizeof(dev->napi_dev->name), "%s",
		 wiphy_name(dev->hw->wiphy));
	dev->napi_dev->threaded = 1;
	init_completion(&dev->mmio.wed_reset);
	init_completion(&dev->mmio.wed_reset_complete);

	mt76_for_each_q_rx(dev, i) {
		if (mt76_queue_is_wed_rro(&dev->q_rx[i]))
			continue;

		mt76_dma_rx_queue_init(dev, i, poll);
	}

	return 0;
}

static const struct mt76_queue_ops mt76_dma_ops = {
	.init = mt76_dma_init,
	.alloc = mt76_dma_alloc_queue,
	.reset_q = mt76_dma_queue_reset,
	.tx_queue_skb_raw = mt76_dma_tx_queue_skb_raw,
	.tx_queue_skb = mt76_dma_tx_queue_skb,
	.tx_cleanup = mt76_dma_tx_cleanup,
	.rx_queue_init = mt76_dma_rx_queue_init,
	.rx_cleanup = mt76_dma_rx_cleanup,
	.rx_reset = mt76_dma_rx_reset,
	.kick = mt76_dma_kick_queue,
};

void mt76_dma_attach(struct mt76_dev *dev)
{
	dev->queue_ops = &mt76_dma_ops;
}
EXPORT_SYMBOL_GPL(mt76_dma_attach);

void mt76_dma_cleanup(struct mt76_dev *dev)
{
	int i;

	mt76_worker_disable(&dev->tx_worker);
	napi_disable(&dev->tx_napi);
	netif_napi_del(&dev->tx_napi);

	for (i = 0; i < ARRAY_SIZE(dev->phys); i++) {
		struct mt76_phy *phy = dev->phys[i];
		int j;

		if (!phy)
			continue;

		for (j = 0; j < ARRAY_SIZE(phy->q_tx); j++)
			mt76_dma_tx_cleanup(dev, phy->q_tx[j], true);
	}

	for (i = 0; i < ARRAY_SIZE(dev->q_mcu); i++) {
		mt76_dma_tx_cleanup(dev, dev->q_mcu[i], true);
		mt76_dma_free_tx_stream_pool(dev, dev->q_mcu[i]);
	}

	mt76_for_each_q_rx(dev, i) {
		struct mt76_queue *q = &dev->q_rx[i];

		netif_napi_del(&dev->napi[i]);
		mt76_dma_rx_cleanup(dev, q);

		page_pool_destroy(q->page_pool);
	}

	if (mtk_wed_device_active(&dev->mmio.wed))
		mtk_wed_device_detach(&dev->mmio.wed);

	if (mtk_wed_device_active(&dev->mmio.wed_hif2))
		mtk_wed_device_detach(&dev->mmio.wed_hif2);

	mt76_free_pending_txwi(dev);
	mt76_free_pending_rxwi(dev);
	free_netdev(dev->napi_dev);
	free_netdev(dev->tx_napi_dev);
}
EXPORT_SYMBOL_GPL(mt76_dma_cleanup);
