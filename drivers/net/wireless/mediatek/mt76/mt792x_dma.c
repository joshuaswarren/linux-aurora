// SPDX-License-Identifier: BSD-3-Clause-Clear
/* Copyright (C) 2023 MediaTek Inc. */

#include <linux/module.h>
#include <linux/firmware.h>

#include "mt792x.h"
#include "dma.h"
#include "trace.h"

#define MT7932_INT_RX_DONE_RING3	BIT(3)
#define MT7932_INT_RX_DONE_RING6	BIT(19)
#define MT7932_INT_RX_DONE_RING5	BIT(23)
#define MT7932_INT_RX_DONE_RING7	BIT(25)
#define MT7932_INT_RX_DONE_RING8	BIT(31)
#define MT7932_INT_RX_DONE_ALL	(BIT(0) | BIT(2) | \
					 MT7932_INT_RX_DONE_RING3 | \
					 MT7932_INT_RX_DONE_RING6 | BIT(22) | \
					 MT7932_INT_RX_DONE_RING5 | \
					 MT7932_INT_RX_DONE_RING7 | \
					 MT7932_INT_RX_DONE_RING8)

static u32 mt7932_dma_clear_tx_ddone(struct mt76_queue *q)
{
	u32 i;

	if (!q || !q->desc)
		return 0;

	for (i = 0; i < q->ndesc; i++) {
		__le32 ctrl = READ_ONCE(q->desc[i].ctrl);

		WRITE_ONCE(q->desc[i].ctrl,
			   ctrl & ~cpu_to_le32(MT_DMA_CTL_DMA_DONE));
	}

	return q->ndesc;
}

irqreturn_t mt792x_irq_handler(int irq, void *dev_instance)
{
	struct mt792x_dev *dev = dev_instance;

	if (test_bit(MT76_REMOVED, &dev->mt76.phy.state))
		return IRQ_NONE;
	mt76_wr(dev, dev->irq_map->host_irq_enable, 0);

	if (!test_bit(MT76_STATE_INITIALIZED, &dev->mphy.state))
		return IRQ_NONE;

	tasklet_schedule(&dev->mt76.irq_tasklet);

	return IRQ_HANDLED;
}
EXPORT_SYMBOL_GPL(mt792x_irq_handler);

void mt792x_irq_tasklet(unsigned long data)
{
	struct mt792x_dev *dev = (struct mt792x_dev *)data;
	const struct mt792x_irq_map *irq_map = dev->irq_map;
	u32 intr, mask = 0;

	mt76_wr(dev, irq_map->host_irq_enable, 0);

	intr = mt76_rr(dev, MT_WFDMA0_HOST_INT_STA);
	intr &= dev->mt76.mmio.irqmask;
	mt76_wr(dev, MT_WFDMA0_HOST_INT_STA, intr);

	trace_dev_irq(&dev->mt76, intr, dev->mt76.mmio.irqmask);

	mask |= intr & (irq_map->rx.data_complete_mask |
			irq_map->rx.wm_complete_mask |
			irq_map->rx.wm2_complete_mask);
	/* Apple polls every active queue from one synchronous RX handler.  Linux
	 * has one NAPI per queue, so disable and schedule only the rings whose
	 * bits asserted.  In particular, do not restore unrelated ring masks from
	 * an empty NAPI completion.
	 */
	if (is_mt7932(&dev->mt76))
		mask |= intr & MT7932_INT_RX_DONE_ALL;
	if (intr & dev->irq_map->tx.mcu_complete_mask)
		mask |= dev->irq_map->tx.mcu_complete_mask;

	if (intr & MT_INT_MCU_CMD) {
		u32 intr_sw;

		intr_sw = mt76_rr(dev, MT_MCU_CMD);
		/* ack MCU2HOST_SW_INT_STA */
		mt76_wr(dev, MT_MCU_CMD, intr_sw);
		if (intr_sw & MT_MCU_CMD_WAKE_RX_PCIE) {
			mask |= irq_map->rx.data_complete_mask;
			intr |= irq_map->rx.data_complete_mask;
		}
	}

	mt76_set_irq_mask(&dev->mt76, irq_map->host_irq_enable, mask, 0);

	if (intr & dev->irq_map->tx.all_complete_mask)
		napi_schedule(&dev->mt76.tx_napi);

	if (intr & irq_map->rx.wm_complete_mask)
		napi_schedule(&dev->mt76.napi[MT_RXQ_MCU]);

	if (intr & irq_map->rx.wm2_complete_mask)
		napi_schedule(&dev->mt76.napi[MT_RXQ_MCU_WA]);

	if (intr & irq_map->rx.data_complete_mask)
		napi_schedule(&dev->mt76.napi[MT_RXQ_MAIN]);

	/* Apple enables RX3 and RX5 in the exact MT7932 host mask.  The J700
	 * queue inventory also publishes both rings, but the inherited mt7921
	 * tasklet only dispatched RX0, RX2 and RX4.  That acknowledged RX3/RX5
	 * interrupts without draining their descriptors, dropping band-1 scan
	 * and auxiliary events before mac80211 could observe them.
	 */
	if (is_mt7932(&dev->mt76) &&
	    intr & MT7932_INT_RX_DONE_RING3) {
		dev_info_ratelimited(dev->mt76.dev,
				 "J700_MT7932_RX_RING3_IRQ_PASS: intr=0x%08x\n",
				 intr);
		napi_schedule(&dev->mt76.napi[MT_RXQ_BAND1]);
	}
	if (is_mt7932(&dev->mt76) &&
	    intr & MT7932_INT_RX_DONE_RING5) {
		dev_info_ratelimited(dev->mt76.dev,
				 "J700_MT7932_RX_RING5_IRQ_PASS: intr=0x%08x\n",
				 intr);
		napi_schedule(&dev->mt76.napi[MT_RXQ_BAND1_WA]);
	}
	if (is_mt7932(&dev->mt76) &&
	    intr & MT7932_INT_RX_DONE_RING6)
		napi_schedule(&dev->mt76.napi[MT_RXQ_MAIN_WA]);
	if (is_mt7932(&dev->mt76) &&
	    intr & MT7932_INT_RX_DONE_RING7)
		napi_schedule(&dev->mt76.napi[MT_RXQ_BAND2]);
	if (is_mt7932(&dev->mt76) &&
	    intr & MT7932_INT_RX_DONE_RING8)
		napi_schedule(&dev->mt76.napi[MT_RXQ_BAND2_WA]);
}
EXPORT_SYMBOL_GPL(mt792x_irq_tasklet);

void mt792x_rx_poll_complete(struct mt76_dev *mdev, enum mt76_rxq_id q)
{
	struct mt792x_dev *dev = container_of(mdev, struct mt792x_dev, mt76);
	const struct mt792x_irq_map *irq_map = dev->irq_map;

	if (q == MT_RXQ_MAIN)
		mt76_connac_irq_enable(mdev, irq_map->rx.data_complete_mask);
	else if (q == MT_RXQ_MCU_WA)
		mt76_connac_irq_enable(mdev, irq_map->rx.wm2_complete_mask);
	else if (is_mt7932(mdev) && q == MT_RXQ_BAND1)
		mt76_connac_irq_enable(mdev, MT7932_INT_RX_DONE_RING3);
	else if (is_mt7932(mdev) && q == MT_RXQ_BAND1_WA)
		mt76_connac_irq_enable(mdev, MT7932_INT_RX_DONE_RING5);
	else if (is_mt7932(mdev) && q == MT_RXQ_MAIN_WA)
		mt76_connac_irq_enable(mdev, MT7932_INT_RX_DONE_RING6);
	else if (is_mt7932(mdev) && q == MT_RXQ_BAND2)
		mt76_connac_irq_enable(mdev, MT7932_INT_RX_DONE_RING7);
	else if (is_mt7932(mdev) && q == MT_RXQ_BAND2_WA)
		mt76_connac_irq_enable(mdev, MT7932_INT_RX_DONE_RING8);
	else
		mt76_connac_irq_enable(mdev, irq_map->rx.wm_complete_mask);
}
EXPORT_SYMBOL_GPL(mt792x_rx_poll_complete);

#define PREFETCH(base, depth)	((base) << 16 | (depth))
#define MT7932_WFDMA_EXT_CSR_3038	MT_WFDMA_EXT_CSR(0x38)
static void mt792x_dma_prefetch(struct mt792x_dev *dev)
{
	if (is_mt7925(&dev->mt76)) {
		/* rx ring */
		mt76_wr(dev, MT_WFDMA0_RX_RING0_EXT_CTRL, PREFETCH(0x0000, 0x4));
		mt76_wr(dev, MT_WFDMA0_RX_RING1_EXT_CTRL, PREFETCH(0x0040, 0x4));
		mt76_wr(dev, MT_WFDMA0_RX_RING2_EXT_CTRL, PREFETCH(0x0080, 0x4));
		mt76_wr(dev, MT_WFDMA0_RX_RING3_EXT_CTRL, PREFETCH(0x00c0, 0x4));
		/* tx ring */
		mt76_wr(dev, MT_WFDMA0_TX_RING0_EXT_CTRL, PREFETCH(0x0100, 0x10));
		mt76_wr(dev, MT_WFDMA0_TX_RING1_EXT_CTRL, PREFETCH(0x0200, 0x10));
		mt76_wr(dev, MT_WFDMA0_TX_RING2_EXT_CTRL, PREFETCH(0x0300, 0x10));
		mt76_wr(dev, MT_WFDMA0_TX_RING3_EXT_CTRL, PREFETCH(0x0400, 0x10));
		mt76_wr(dev, MT_WFDMA0_TX_RING15_EXT_CTRL, PREFETCH(0x0500, 0x4));
		mt76_wr(dev, MT_WFDMA0_TX_RING16_EXT_CTRL, PREFETCH(0x0540, 0x4));
	} else if (is_mt7902(&dev->mt76)) {
		/* rx ring */
		mt76_wr(dev, MT_WFDMA0_RX_RING0_EXT_CTRL, PREFETCH(0x0000, 0x4));
		mt76_wr(dev, MT_WFDMA0_RX_RING1_EXT_CTRL, PREFETCH(0x0040, 0x4));
		mt76_wr(dev, MT_WFDMA0_RX_RING2_EXT_CTRL, PREFETCH(0x0080, 0x4));
		mt76_wr(dev, MT_WFDMA0_RX_RING3_EXT_CTRL, PREFETCH(0x00c0, 0x4));
		/* tx ring */
		mt76_wr(dev, MT_WFDMA0_TX_RING0_EXT_CTRL, PREFETCH(0x0100, 0x4));
		mt76_wr(dev, MT_WFDMA0_TX_RING1_EXT_CTRL, PREFETCH(0x0140, 0x4));
		mt76_wr(dev, MT_WFDMA0_TX_RING2_EXT_CTRL, PREFETCH(0x0180, 0x4));
		mt76_wr(dev, MT_WFDMA0_TX_RING3_EXT_CTRL, PREFETCH(0x01c0, 0x4));
		mt76_wr(dev, MT_WFDMA0_TX_RING4_EXT_CTRL, PREFETCH(0x0200, 0x4));
		mt76_wr(dev, MT_WFDMA0_TX_RING5_EXT_CTRL, PREFETCH(0x0240, 0x4));
		mt76_wr(dev, MT_WFDMA0_TX_RING6_EXT_CTRL, PREFETCH(0x0280, 0x4));
		mt76_wr(dev, MT_WFDMA0_TX_RING15_EXT_CTRL, PREFETCH(0x02c0, 0x4));
		mt76_wr(dev, MT_WFDMA0_TX_RING16_EXT_CTRL, PREFETCH(0x0300, 0x4));
	} else if (is_mt7932(&dev->mt76)) {
		static const struct {
			u8 ring;
			u16 base;
			u8 depth;
		} rx[] = {
			{ 0, 0x000, 0x4 }, { 2, 0x040, 0x8 },
			{ 3, 0x0c0, 0x4 }, { 4, 0x100, 0x4 },
			{ 5, 0x140, 0x4 }, { 6, 0x180, 0x4 },
			{ 7, 0x1c0, 0x4 }, { 8, 0x200, 0x4 },
		}, tx[] = {
			{ 0,  0x240, 0x4 }, { 1,  0x280, 0x4 },
			{ 2,  0x2c0, 0x4 }, { 3,  0x300, 0x4 },
			{ 4,  0x340, 0xc }, { 5,  0x400, 0x4 },
			{ 6,  0x440, 0x4 }, { 7,  0x480, 0x4 },
			{ 8,  0x4c0, 0xc }, { 9,  0x580, 0x4 },
			{ 10, 0x5c0, 0x4 }, { 11, 0x600, 0x4 },
			{ 12, 0x640, 0xc }, { 13, 0x700, 0x4 },
			{ 14, 0x740, 0x4 }, { 16, 0x780, 0x4 },
			{ 17, 0x7c0, 0x4 }, { 18, 0x800, 0x4 },
		};
		int i;

		/* Exact complete Run-191 manual-prefetch publication.  The hardware
		 * contract requires every live ring's MAX_CNT to be installed before
		 * DMA enable, including rings that remain idle during firmware load.
		 */
		for (i = 0; i < ARRAY_SIZE(rx); i++)
			mt76_wr(dev, MT_WFDMA0_RX_RING_EXT_CTRL(rx[i].ring),
				 PREFETCH(rx[i].base, rx[i].depth));
		for (i = 0; i < ARRAY_SIZE(tx); i++)
			mt76_wr(dev, MT_WFDMA0_TX_RING_EXT_CTRL(tx[i].ring),
				 PREFETCH(tx[i].base, tx[i].depth));
		dev_info(dev->mt76.dev,
			 "J700_MT7932_PREFETCH_LAYOUT: full-run191 rx=0,2-8 tx=0-14,16-18 final_tx18=08000004\n");
	} else {
		/* rx ring */
		mt76_wr(dev, MT_WFDMA0_RX_RING0_EXT_CTRL, PREFETCH(0x0, 0x4));
		mt76_wr(dev, MT_WFDMA0_RX_RING2_EXT_CTRL, PREFETCH(0x40, 0x4));
		mt76_wr(dev, MT_WFDMA0_RX_RING3_EXT_CTRL, PREFETCH(0x80, 0x4));
		mt76_wr(dev, MT_WFDMA0_RX_RING4_EXT_CTRL, PREFETCH(0xc0, 0x4));
		mt76_wr(dev, MT_WFDMA0_RX_RING5_EXT_CTRL, PREFETCH(0x100, 0x4));
		/* tx ring */
		mt76_wr(dev, MT_WFDMA0_TX_RING0_EXT_CTRL, PREFETCH(0x140, 0x4));
		mt76_wr(dev, MT_WFDMA0_TX_RING1_EXT_CTRL, PREFETCH(0x180, 0x4));
		mt76_wr(dev, MT_WFDMA0_TX_RING2_EXT_CTRL, PREFETCH(0x1c0, 0x4));
		mt76_wr(dev, MT_WFDMA0_TX_RING3_EXT_CTRL, PREFETCH(0x200, 0x4));
		mt76_wr(dev, MT_WFDMA0_TX_RING4_EXT_CTRL, PREFETCH(0x240, 0x4));
		mt76_wr(dev, MT_WFDMA0_TX_RING5_EXT_CTRL, PREFETCH(0x280, 0x4));
		mt76_wr(dev, MT_WFDMA0_TX_RING6_EXT_CTRL, PREFETCH(0x2c0, 0x4));
		mt76_wr(dev, MT_WFDMA0_TX_RING16_EXT_CTRL, PREFETCH(0x340, 0x4));
		mt76_wr(dev, MT_WFDMA0_TX_RING17_EXT_CTRL, PREFETCH(0x380, 0x4));
	}
}

int mt792x_dma_enable(struct mt792x_dev *dev)
{
	u32 cfg_mask, cfg_set;

	if (is_mt7932(&dev->mt76) && dev->mt7932_runtime_dma_epoch) {
		/* Run 191 repeats the disabled pre-runtime image after publishing all
		 * ring BASE/CIDX/COUNT fields and before the extension-control table.
		 */
		mt76_wr(dev, MT_WFDMA0_GLO_CFG, 0x00103870);
		if (mt76_rr(dev, MT_WFDMA0_GLO_CFG) != 0x00103870) {
			dev_err(dev->mt76.dev,
				"J700_MT7932_RUNTIME_PRERING_FAIL: phase=post-ring glo=0x%08x expected=0x00103870\n",
				mt76_rr(dev, MT_WFDMA0_GLO_CFG));
			return -EIO;
		}
	}

	/* configure perfetch settings */
	mt792x_dma_prefetch(dev);

	/* reset dma idx */
	mt76_wr(dev, MT_WFDMA0_RST_DTX_PTR, ~0);
	if (is_mt7925(&dev->mt76))
		mt76_wr(dev, MT_WFDMA0_RST_DRX_PTR, ~0);

	if (is_mt7932(&dev->mt76) && dev->mt7932_runtime_dma_epoch) {
		u32 ext0;

		/* Run 191 does not transiently restart the post-download engine with
		 * the earlier 0x50209045 image.  It installs the complete runtime
		 * image while TX/RX DMA are disabled, then changes only the enable
		 * bits: 0x5030b870 -> 0x5030b875.  Publishing the download image and
		 * interrupt mask first seeds Connac3's hidden payload/BKRS state even
		 * though the final register readback later converges.
		 */
		/* HOST_INT_ENA is command-like on this block: Cycle552 wrote the
		 * complete Run-191 mask and read it back immediately, then observed only
		 * MCU_CMD (0x20000000) after RST_DTX without any host write.  The trace
		 * specifies the writes, not persistent readback, so record rather than
		 * reject the hardware-consumed value here.
		 */
		dev_info(dev->mt76.dev,
			 "J700_MT7932_RUNTIME_IRQ_READBACK: phase=post-rstdtx host_int=0x%08x source=cycle552\n",
			 mt76_rr(dev, MT7932_HOST_INT_ENA));

		/* Run 541 measures GLO_CFG_EXT0=0x28c004df and the complete Run-191
		 * trace contains no EXT0 access, so Apple's selector remains asserted
		 * across WFDMA disable, ring reset and RST_DTX.  Linux asserts it at the
		 * runtime boundary before entering mt792x_wpdma_reset(); verify that it
		 * survived the disable/reset path instead of reasserting it too late.
		 */
		ext0 = mt76_rr(dev, MT_WFDMA0_GLO_CFG_EXT0);
		if (ext0 != 0x28c004df) {
			dev_err(dev->mt76.dev,
				"J700_MT7932_RUNTIME_DMASHDL_SELECTOR_FAIL: phase=post-rstdtx ext0=0x%08x expected=0x28c004df\n",
				ext0);
			return -EIO;
		}
		dev_info(dev->mt76.dev,
			 "J700_MT7932_RUNTIME_DMASHDL_SELECTOR_PASS: ext0=0x28c004df selector=1 phase=pre-disable-through-rstdtx source=run191-no-ext0-write+run541-snapshot\n");
		mt76_wr(dev, MT_WFDMA0_GLO_CFG, 0x5030b870);
		if ((mt76_rr(dev, MT_WFDMA0_GLO_CFG) &
		     ~(MT_WFDMA0_GLO_CFG_TX_DMA_BUSY |
		       MT_WFDMA0_GLO_CFG_RX_DMA_BUSY)) != 0x5030b870) {
			dev_err(dev->mt76.dev,
				"J700_MT7932_RUNTIME_ENABLE_FAIL: disabled_glo=0x%08x expected=0x5030b870\n",
				mt76_rr(dev, MT_WFDMA0_GLO_CFG));
			return -EIO;
		}
		mt76_wr(dev, MT_WFDMA0_GLO_CFG, 0x5030b875);
		mt76_wr(dev, MT_WFDMA0_INT_RX_PRI, 0x0000000c);
		/* Cycle555 proves reset changes EXT_CSR+0x38 from the initial 0x13
		 * to 0x7d.  Restore the static-RE value before runtime commands; omitting
		 * this write fails closed before the Run-191 handshake completes.
		 */
		mt76_wr(dev, MT7932_WFDMA_EXT_CSR_3038, 0x00000013);
		mt76_wr(dev, MT_WFDMA0_PRI_DLY_INT_CFG0, 0x8032800a);
		if ((mt76_rr(dev, MT_WFDMA0_GLO_CFG) &
		     ~(MT_WFDMA0_GLO_CFG_TX_DMA_BUSY |
		       MT_WFDMA0_GLO_CFG_RX_DMA_BUSY)) != 0x5030b875 ||
		    mt76_rr(dev, MT_WFDMA0_INT_RX_PRI) != 0x0000000c ||
		    mt76_rr(dev, MT7932_WFDMA_EXT_CSR_3038) != 0x00000013 ||
		    mt76_rr(dev, MT_WFDMA0_PRI_DLY_INT_CFG0) != 0x8032800a) {
			dev_err(dev->mt76.dev,
				"J700_MT7932_RUNTIME_ENABLE_FAIL: enabled_glo=0x%08x host_int=0x%08x rxpri=0x%08x ext3038=0x%08x dly=0x%08x\n",
				mt76_rr(dev, MT_WFDMA0_GLO_CFG),
				mt76_rr(dev, MT7932_HOST_INT_ENA),
				mt76_rr(dev, MT_WFDMA0_INT_RX_PRI),
				mt76_rr(dev, MT7932_WFDMA_EXT_CSR_3038),
				mt76_rr(dev, MT_WFDMA0_PRI_DLY_INT_CFG0));
			return -EIO;
		}
		dev_info(dev->mt76.dev,
			 "J700_MT7932_RUNTIME_ENABLE_PASS: sequence=0x5030b870,0x5030b875 ext3038=0x7d-to-0x13 host_int=0x%08x transient_download_enable=0 source=run191-full-mmio+cycle555\n",
			 mt76_rr(dev, MT7932_HOST_INT_ENA));
		return 0;
	}

	/* Apple programs MT7932's delayed-interrupt register after WFDMA enable. */
	if (!is_mt7932(&dev->mt76))
		mt76_wr(dev, MT_WFDMA0_PRI_DLY_INT_CFG0, 0);

	cfg_mask = MT_WFDMA0_GLO_CFG_TX_WB_DDONE |
		   MT_WFDMA0_GLO_CFG_FIFO_LITTLE_ENDIAN |
		   MT_WFDMA0_GLO_CFG_CLK_GAT_DIS |
		   MT_WFDMA0_GLO_CFG_OMIT_TX_INFO |
		   MT_WFDMA0_GLO_CFG_DMA_SIZE |
		   MT_WFDMA0_GLO_CFG_FIFO_DIS_CHECK |
		   MT_WFDMA0_GLO_CFG_RX_WB_DDONE |
		   MT_WFDMA0_GLO_CFG_CSR_DISP_BASE_PTR_CHAIN_EN |
		   MT_WFDMA0_GLO_CFG_OMIT_RX_INFO_PFET2;
	if (is_mt7932(&dev->mt76))
		/* Run 191 writes 0x50209045 exactly.  iBoot leaves bit 20 set,
		 * but Apple's normal WFDMA setup clears the force-loopback RX queue
		 * selector before enabling DMA.  Preserving it yields 0x50309045
		 * and can divert the first post-FW_START receive event.
		 */
		cfg_mask |= MT_WFDMA0_GLO_CFG_CSR_LBK_RX_Q_SEL_EN;
	cfg_set = MT_WFDMA0_GLO_CFG_TX_WB_DDONE |
		  MT_WFDMA0_GLO_CFG_FIFO_LITTLE_ENDIAN |
		  MT_WFDMA0_GLO_CFG_CLK_GAT_DIS |
		  MT_WFDMA0_GLO_CFG_OMIT_TX_INFO |
		  MT_WFDMA0_GLO_CFG_CSR_DISP_BASE_PTR_CHAIN_EN |
		  MT_WFDMA0_GLO_CFG_OMIT_RX_INFO_PFET2;
	if (!is_mt7932(&dev->mt76))
		cfg_set |= FIELD_PREP(MT_WFDMA0_GLO_CFG_DMA_SIZE, 3) |
			   MT_WFDMA0_GLO_CFG_FIFO_DIS_CHECK |
			   MT_WFDMA0_GLO_CFG_RX_WB_DDONE;
	mt76_rmw(dev, MT_WFDMA0_GLO_CFG, cfg_mask, cfg_set);

	/* Run 191 contains no GLO_CFG_EXT0 access from the beginning of the
	 * captured Apple driver bring-up, while the working paused snapshots read
	 * TX_DMASHDL_ENABLE set.  Cycle126 needed round-robin only before Linux had
	 * reconstructed Apple's complete DMASHDL image.  That image is now installed
	 * before this call, so keep the scheduler connected for the initial firmware
	 * download as Apple does instead of changing routes at the runtime boundary.
	 */
	if (is_mt7932(&dev->mt76)) {
		mt76_set(dev, MT_WFDMA0_GLO_CFG_EXT0,
			 MT_WFDMA0_CSR_TX_DMASHDL_ENABLE);
		dev_info(dev->mt76.dev,
			 "J700_MT7932_CYCLE551_TRANSPORT: dmashdl_selector=1 phase=initial-firmware-download source=run191-no-ext0-write+run541\n");
	}

	mt76_set(dev, MT_WFDMA0_GLO_CFG,
		 MT_WFDMA0_GLO_CFG_TX_DMA_EN | MT_WFDMA0_GLO_CFG_RX_DMA_EN);
	if (is_mt7932(&dev->mt76)) {
		/* Exact enable-time writes from AppleSunriseWLAN 25G83
		 * _mt7922WpdmaConfig after its common WFDMA control call.  They
		 * are independent of the DMASHDL selector, which remains connected
		 * to the complete Apple-equivalent scheduler image above.
		 */
		mt76_wr(dev, MT_WFDMA0_INT_RX_PRI, 0x0000000c);
		mt76_wr(dev, MT7932_WFDMA_EXT_CSR_3038, 0x00000013);
		mt76_wr(dev, MT_WFDMA0_PRI_DLY_INT_CFG0, 0x8032800a);
		if (mt76_rr(dev, MT_WFDMA0_INT_RX_PRI) != 0x0000000c ||
		    mt76_rr(dev, MT7932_WFDMA_EXT_CSR_3038) != 0x00000013 ||
		    mt76_rr(dev, MT_WFDMA0_PRI_DLY_INT_CFG0) != 0x8032800a)
			return -EIO;
		dev_info(dev->mt76.dev,
			 "J700_MT7932_WFDMA_APPLE_ENABLE_PASS: rxpri=0x%08x ext3038=0x%08x dly=0x%08x\n",
			 mt76_rr(dev, MT_WFDMA0_INT_RX_PRI),
			 mt76_rr(dev, MT7932_WFDMA_EXT_CSR_3038),
			 mt76_rr(dev, MT_WFDMA0_PRI_DLY_INT_CFG0));
		dev_info(dev->mt76.dev,
			 "J700_MT7932_WFDMA_GLO_CFG_PASS: value=0x%08x apple_exact=0x50209045 loopback_rx_sel=0\n",
			 mt76_rr(dev, MT_WFDMA0_GLO_CFG));
	}

	if (is_mt7925(&dev->mt76)) {
		mt76_rmw(dev, MT_UWFDMA0_GLO_CFG_EXT1, BIT(28), BIT(28));
		mt76_set(dev, MT_WFDMA0_INT_RX_PRI, 0x0F00);
		mt76_set(dev, MT_WFDMA0_INT_TX_PRI, 0x7F00);
	}
	mt76_set(dev, MT_WFDMA_DUMMY_CR, MT_WFDMA_NEED_REINIT);

	/* enable interrupts for TX/RX rings */
	if (is_mt7932(&dev->mt76)) {
		/* Run 191 writes these exact masks after WFDMA enable and restores
		 * them after every init response, including the FW_START response.
		 * Keep the complete mask in mt76's software irqmask as well as in
		 * hardware so the tasklet does not narrow it after the first IRQ.
		 */
		mt76_set_irq_mask(&dev->mt76, MT7932_HOST_INT_ENA, 0,
				  MT7932_HOST_INT_MASK);
		mt76_wr(dev, MT_MCU2HOST_SW_INT_ENA,
			 MT7932_MCU2HOST_SW_INT_MASK);
		dev_info(dev->mt76.dev,
			 "J700_MT7932_INTERRUPT_SOFTWARE_ARMED: host_now=0x%08x mcu2host=0x%08x source=run191 deferred=1\n",
			 mt76_rr(dev, dev->irq_map->host_irq_enable),
			 mt76_rr(dev, MT_MCU2HOST_SW_INT_ENA));
	} else {
		mt76_connac_irq_enable(&dev->mt76,
				       dev->irq_map->tx.all_complete_mask |
				       dev->irq_map->rx.data_complete_mask |
				       dev->irq_map->rx.wm2_complete_mask |
				       dev->irq_map->rx.wm_complete_mask |
				       MT_INT_MCU_CMD);
		mt76_set(dev, MT_MCU2HOST_SW_INT_ENA,
			 MT_MCU_CMD_WAKE_RX_PCIE);
	}

	return 0;
}
EXPORT_SYMBOL_GPL(mt792x_dma_enable);

static int
mt792x_dma_reset(struct mt792x_dev *dev, bool force)
{
	u32 tx_desc_cleared = 0;
	u32 tx_ring_seen = 0;
	int i, err;

	err = mt792x_dma_disable(dev, force);
	if (err)
		return err;
	if (is_mt7932(&dev->mt76) && dev->mt7932_runtime_dma_epoch) {
		/* mt792x_dma_disable() has already reproduced Run 191's read of
		 * 0x1010b870 followed by its one RMW destination write, 0x00103870.
		 * Verify the destination here without looking for the consumed source.
		 */
		if (mt76_rr(dev, MT_WFDMA0_GLO_CFG) != 0x00103870) {
			dev_err(dev->mt76.dev,
				"J700_MT7932_RUNTIME_PRERING_FAIL: phase=destination glo=0x%08x expected=0x00103870\n",
				mt76_rr(dev, MT_WFDMA0_GLO_CFG));
			return -EIO;
		}
		dev_info(dev->mt76.dev,
			 "J700_MT7932_RUNTIME_PRERING_PASS: sequence=disable-rmw(read-0x1010b870,write-0x00103870),rings,write-0x00103870 dma_enabled=0 source=run191-seq14285-14368\n");
	}

	/* Reset each physical MT7932 TX ring once. All four mac80211 ACs alias
	 * hardware ring 0, and the retained idle-ring inventory aliases it once
	 * more. Cycle622 recorded five BASE/CIDX/COUNT publications for ring 0;
	 * successful Run191 publishes it exactly once. Repeated writes can seed a
	 * different hidden prefetch/BKRS image even while WFDMA is disabled.
	 */
	if (is_mt7932(&dev->mt76) && dev->mt7932_runtime_dma_epoch) {
		for (i = 0; i < __MT_TXQ_MAX; i++) {
			struct mt76_queue *q = dev->mphy.q_tx[i];

			if (!q || tx_ring_seen & BIT(q->hw_idx))
				continue;
			tx_ring_seen |= BIT(q->hw_idx);
			mt76_queue_reset(dev, q, true);
		}
		for (i = 0; i < ARRAY_SIZE(dev->mt7932_idle_tx); i++) {
			struct mt76_queue *q = dev->mt7932_idle_tx[i];

			if (!q || tx_ring_seen & BIT(q->hw_idx))
				continue;
			tx_ring_seen |= BIT(q->hw_idx);
			dev->mt76.queue_ops->reset_q(&dev->mt76, q, true);
		}
		for (i = 0; i < __MT_MCUQ_MAX; i++) {
			struct mt76_queue *q = dev->mt76.q_mcu[i];

			if (!q || tx_ring_seen & BIT(q->hw_idx))
				continue;
			tx_ring_seen |= BIT(q->hw_idx);
			mt76_queue_reset(dev, q, true);
		}
		dev_info(dev->mt76.dev,
			 "J700_MT7932_RUNTIME_UNIQUE_TX_RING_PASS: mask=0x%08x expected=0x00077fff ring0_writes=1 source=cycle622-write-diff+run191\n",
			 tx_ring_seen);
	} else {
		for (i = 0; i < __MT_TXQ_MAX; i++)
			mt76_queue_reset(dev, dev->mphy.q_tx[i], true);
		if (is_mt7932(&dev->mt76))
			for (i = 0; i < ARRAY_SIZE(dev->mt7932_idle_tx); i++)
				if (dev->mt7932_idle_tx[i])
					dev->mt76.queue_ops->reset_q(
						&dev->mt76,
						dev->mt7932_idle_tx[i], true);
		for (i = 0; i < __MT_MCUQ_MAX; i++)
			mt76_queue_reset(dev, dev->mt76.q_mcu[i], true);
	}
	if (is_mt7932(&dev->mt76) && dev->mt7932_runtime_dma_epoch) {
		/* AppleSunriseWLAN 25G83 halHwRecoveryFromError calls
		 * halWpdmaAllocRing(..., false) before halWpdmaInitRing.  Exact
		 * disassembly at 0x1000dabe0..0x1000dabe8 clears descriptor control
		 * bit 31 for every existing TX cell.  Generic mt76_queue_reset()
		 * instead marks every empty descriptor DDONE, so the unused portion
		 * of the post-reset ring presents a different ownership image to the
		 * Connac3 prefetch/BKRS engine even though live descriptors are later
		 * overwritten correctly.
		 */
		for (i = 0; i < __MT_TXQ_MAX; i++)
			tx_desc_cleared += mt7932_dma_clear_tx_ddone(
				dev->mphy.q_tx[i]);
		for (i = 0; i < ARRAY_SIZE(dev->mt7932_idle_tx); i++)
			tx_desc_cleared += mt7932_dma_clear_tx_ddone(
				dev->mt7932_idle_tx[i]);
		for (i = 0; i < __MT_MCUQ_MAX; i++)
			tx_desc_cleared += mt7932_dma_clear_tx_ddone(
				dev->mt76.q_mcu[i]);
		dma_wmb();
		dev_info(dev->mt76.dev,
			 "J700_MT7932_RUNTIME_TX_OWNER_RESET_PASS: descriptors=%u ddone=0 phase=before-ring-publish source=apple25G83-halWpdmaAllocRing-0x1000dabe0\n",
			 tx_desc_cleared);
	}

	if (!(is_mt7932(&dev->mt76) && dev->mt7932_runtime_dma_epoch))
		mt76_for_each_q_rx(&dev->mt76, i)
			mt76_queue_reset(dev, &dev->mt76.q_rx[i], true);

	mt76_tx_status_check(&dev->mt76, true);

	if (is_mt7932(&dev->mt76) && dev->mt7932_runtime_dma_epoch) {
		/* halWpdmaAllocRxRing(..., false) has already preserved every mapped
		 * buffer and rebuilt all descriptors before WFDMA disable.  Apple then
		 * publishes each ring here, while disabled, as BASE -> CIDX(count-1) ->
		 * COUNT.  Do not clean up, refill or remap anything at this point.
		 */
		mt76_for_each_q_rx(&dev->mt76, i) {
			struct mt76_queue *q = &dev->mt76.q_rx[i];
			u32 cidx;

			if (!q->ndesc)
				continue;
			cidx = q->ndesc - 1;
			if (q->head || q->tail || q->queued != q->ndesc) {
				dev_err(dev->mt76.dev,
					"J700_MT7932_RUNTIME_RX_PRERING_FAIL: qid=%d hw=%u head=%u tail=%u queued=%u expected=0/0/%u\n",
					i, q->hw_idx, q->head, q->tail,
					q->queued, q->ndesc);
				return -EIO;
			}
			Q_WRITE(q, desc_base, q->desc_dma);
			Q_WRITE(q, cpu_idx, cidx);
			Q_WRITE(q, ring_size, q->ndesc);
			dev_info(dev->mt76.dev,
				 "J700_MT7932_RUNTIME_RX_PRERING_PASS: qid=%d hw=%u base=%pad cidx=%u count=%u buffers=%u mappings=preserved prepublish_writes=0 order=memory-before-disable,base,cidx,count source=apple25G83-halWpdmaAllocRxRing(false)+halWpdmaInitRxRing+run191-seq14343-14366\n",
				 i, q->hw_idx, &q->desc_dma, cidx, q->ndesc,
				 q->queued);
		}
	}

	return mt792x_dma_enable(dev);
}

int mt792x_wpdma_reset(struct mt792x_dev *dev, bool force)
{
	bool apple_runtime = is_mt7932(&dev->mt76) &&
			     dev->mt7932_runtime_dma_epoch;
	u32 tx_desc_cleared = 0;
	int i, err;

	/* AppleSunriseWLAN's halWpdmaAllocRing(..., false) performs memory-only
	 * cleanup before halWpdmaInitRing disables and republishes WFDMA.  Generic
	 * mt76 flush cleanup calls mt76_dma_sync_idx() and kicks each CIDX before
	 * the disable, a device-visible doorbell sequence absent from Run 191.
	 * The measured ordinal-87 drain guarantees no live TX entry here; require
	 * that invariant, clear Apple's DDONE ownership image, and issue no CIDX.
	 */
	if (apple_runtime) {
		for (i = 0; i < ARRAY_SIZE(dev->mt76.phy.q_tx); i++) {
			struct mt76_queue *q = dev->mphy.q_tx[i];

			if (q && READ_ONCE(q->queued))
				return -EBUSY;
			tx_desc_cleared += mt7932_dma_clear_tx_ddone(q);
		}
		for (i = 0; i < ARRAY_SIZE(dev->mt7932_idle_tx); i++)
			if (dev->mt7932_idle_tx[i]) {
				struct mt76_queue *q = dev->mt7932_idle_tx[i];

				if (READ_ONCE(q->queued))
					return -EBUSY;
				tx_desc_cleared +=
					mt7932_dma_clear_tx_ddone(q);
			}
		for (i = 0; i < ARRAY_SIZE(dev->mt76.q_mcu); i++) {
			struct mt76_queue *q = dev->mt76.q_mcu[i];

			if (q && READ_ONCE(q->queued))
				return -EBUSY;
			tx_desc_cleared += mt7932_dma_clear_tx_ddone(q);
		}
		dma_wmb();
		dev_info(dev->mt76.dev,
			 "J700_MT7932_RUNTIME_TX_MEMORY_CLEAN_PASS: descriptors=%u queued=0 cidx_writes=0 phase=before-wfdma-disable source=apple25G83-halWpdmaAllocRing\n",
			 tx_desc_cleared);
	} else {
		/* clean up hw queues */
		for (i = 0; i < ARRAY_SIZE(dev->mt76.phy.q_tx); i++)
			mt76_queue_tx_cleanup(dev, dev->mphy.q_tx[i], true);
		if (is_mt7932(&dev->mt76))
			for (i = 0; i < ARRAY_SIZE(dev->mt7932_idle_tx); i++)
				if (dev->mt7932_idle_tx[i])
					dev->mt76.queue_ops->tx_cleanup(
						&dev->mt76,
						dev->mt7932_idle_tx[i], true);

		for (i = 0; i < ARRAY_SIZE(dev->mt76.q_mcu); i++)
			mt76_queue_tx_cleanup(dev, dev->mt76.q_mcu[i], true);
	}

	if (apple_runtime) {
		/* Apple rebuilds every existing RX descriptor in
		 * halWpdmaAllocRing(..., false), before halWpdmaInitRing disables
		 * WFDMA.  Preserve those mappings and add only mt76's structural
		 * empty slot; no queue register is written in this phase.
		 */
		mt76_for_each_q_rx(&dev->mt76, i) {
			struct mt76_queue *q = &dev->mt76.q_rx[i];

			err = mt76_dma_rx_rebuild_existing(&dev->mt76, i);
			if (err)
				return err;
			dev_info(dev->mt76.dev,
				 "J700_MT7932_RUNTIME_RX_MEMORY_PASS: qid=%d hw=%u buffers=%u mappings=preserved new=1 phase=before-wpdma-disable source=apple25G83-halWpdmaAllocRxRing(false)\n",
				 i, q->hw_idx, q->queued);
		}
	} else {
		mt76_for_each_q_rx(&dev->mt76, i)
			mt76_queue_rx_cleanup(dev, &dev->mt76.q_rx[i]);
	}

	if (force) {
		err = mt792x_wfsys_reset(dev);
		if (err)
			return err;
	}
	err = mt792x_dma_reset(dev, force);
	if (err)
		return err;
	if (is_mt7932(&dev->mt76) && (apple_runtime || force)) {
		/* WFDMA has been reset at this boundary; a forced recovery has reset
		 * WFSYS as well.  The device can no longer observe the retained
		 * firmware and command payloads, so reclaim their software cursors
		 * before mt7921_run_firmware() enters a second download epoch.  Without
		 * this, the exactly-sized TX16 pool fails at RAM upload on the first
		 * normal mac80211 recovery and every later retry fails during the patch.
		 */
		for (i = 0; i < ARRAY_SIZE(dev->mt76.q_mcu); i++)
			mt76_dma_reset_tx_pools(&dev->mt76,
						  dev->mt76.q_mcu[i]);
	}

	if (is_mt7932(&dev->mt76) && force && !apple_runtime) {
		u32 queues = 0;

		/* The measured runtime epoch temporarily publishes every RX slot.
		 * Generic forced recovery subsequently cleans those buffers and must
		 * restore mt76's one-empty-slot geometry before refilling.  Leaving
		 * rx_full_ring set fills all slots, so the next measured runtime rebuild
		 * correctly rejects the queue with -EINVAL.
		 */
		mt76_for_each_q_rx(&dev->mt76, i) {
			struct mt76_queue *q = &dev->mt76.q_rx[i];

			if (!q->ndesc)
				continue;
			q->rx_full_ring = false;
			queues++;
		}
		dev_info(dev->mt76.dev,
			 "J700_MT7932_RX_GEOMETRY_RESET_PASS: queues=%u full_ring=0 structural_empty=1 boundary=post-forced-wpdma\n",
			 queues);
	}

	if (!(is_mt7932(&dev->mt76) && dev->mt7932_runtime_dma_epoch))
		mt76_for_each_q_rx(&dev->mt76, i)
			mt76_queue_rx_reset(dev, i);

	return 0;
}
EXPORT_SYMBOL_GPL(mt792x_wpdma_reset);

int mt792x_wpdma_reinit_cond(struct mt792x_dev *dev)
{
	struct mt76_connac_pm *pm = &dev->pm;
	int err;

	/* check if the wpdma must be reinitialized */
	if (mt792x_dma_need_reinit(dev)) {
		/* disable interrutpts */
		mt76_wr(dev, dev->irq_map->host_irq_enable, 0);
		mt76_wr(dev, MT_PCIE_MAC_INT_ENABLE, 0x0);

		err = mt792x_wpdma_reset(dev, false);
		if (err) {
			dev_err(dev->mt76.dev, "wpdma reset failed\n");
			return err;
		}

		/* enable interrutpts */
		mt76_wr(dev, MT_PCIE_MAC_INT_ENABLE, 0xff);
		pm->stats.lp_wake++;
	}

	return 0;
}
EXPORT_SYMBOL_GPL(mt792x_wpdma_reinit_cond);

int mt792x_dma_disable(struct mt792x_dev *dev, bool force)
{
	if (is_mt7932(&dev->mt76) && dev->mt7932_runtime_dma_epoch) {
		u32 source = mt76_rr(dev, MT_WFDMA0_GLO_CFG);

		if (source != 0x1010b870) {
			dev_err(dev->mt76.dev,
				"J700_MT7932_RUNTIME_PRERING_FAIL: phase=source glo=0x%08x expected=0x1010b870\n",
				source);
			return -EIO;
		}
		dev_info(dev->mt76.dev,
			 "J700_MT7932_RUNTIME_PRERING_SOURCE_PASS: source=0x1010b870 operation=read-before-disable-rmw trace=run191-seq14285\n");
	}

	/* disable WFDMA0 */
	mt76_clear(dev, MT_WFDMA0_GLO_CFG,
		   MT_WFDMA0_GLO_CFG_TX_DMA_EN | MT_WFDMA0_GLO_CFG_RX_DMA_EN |
		   MT_WFDMA0_GLO_CFG_CSR_DISP_BASE_PTR_CHAIN_EN |
		   MT_WFDMA0_GLO_CFG_OMIT_TX_INFO |
		   MT_WFDMA0_GLO_CFG_OMIT_RX_INFO |
		   MT_WFDMA0_GLO_CFG_OMIT_RX_INFO_PFET2);

	if (!mt76_poll_msec_tick(dev, MT_WFDMA0_GLO_CFG,
				 MT_WFDMA0_GLO_CFG_TX_DMA_BUSY |
				 MT_WFDMA0_GLO_CFG_RX_DMA_BUSY, 0, 100, 1))
		return -ETIMEDOUT;

	/* Apple preserves the runtime TX-DMASHDL route through its WFDMA-only
	 * recovery.  Firmware download still enters here with the selector off.
	 */
	if (is_mt7932(&dev->mt76) && dev->mt7932_runtime_dma_epoch) {
		if (!(mt76_rr(dev, MT_WFDMA0_GLO_CFG_EXT0) &
		      MT_WFDMA0_CSR_TX_DMASHDL_ENABLE)) {
			dev_err(dev->mt76.dev,
				"J700_MT7932_RUNTIME_DMASHDL_SELECTOR_FAIL: phase=disable ext0=0x%08x expected-selector=1\n",
				mt76_rr(dev, MT_WFDMA0_GLO_CFG_EXT0));
			return -EIO;
		}
	} else {
		mt76_clear(dev, MT_WFDMA0_GLO_CFG_EXT0,
			   MT_WFDMA0_CSR_TX_DMASHDL_ENABLE);
	}
	if (is_mt7932(&dev->mt76)) {
		/* AppleSunriseWLAN's runtime WFDMA recovery controls and rebuilds
		 * WFDMA without bypassing the DMASHDL instance initialized by
		 * _mt7922DmashdlInit/_mt7961DmashdlInit.  The generic mt792x path
		 * sets bypass here and never clears it during dma_enable(), leaving
		 * the post-reset command epoch with a different scheduler state.
		 */
		if (mt76_rr(dev, MT_DMASHDL_SW_CONTROL) &
		    MT_DMASHDL_DMASHDL_BYPASS) {
			dev_err(dev->mt76.dev,
				"J700_MT7932_DMASHDL_RUNTIME_PRESERVE_FAIL: sw_control=0x%08x\n",
				mt76_rr(dev, MT_DMASHDL_SW_CONTROL));
			return -EIO;
		}
		dev_info(dev->mt76.dev,
			 "J700_MT7932_DMASHDL_RUNTIME_PRESERVE_PASS: sw_control=0x%08x bypass=0 source=apple-25G83-static-re\n",
			 mt76_rr(dev, MT_DMASHDL_SW_CONTROL));
	} else {
		mt76_set(dev, MT_DMASHDL_SW_CONTROL,
			 MT_DMASHDL_DMASHDL_BYPASS);
	}

	if (force) {
		/* reset */
		mt76_clear(dev, MT_WFDMA0_RST,
			   MT_WFDMA0_RST_DMASHDL_ALL_RST |
			   MT_WFDMA0_RST_LOGIC_RST);

		mt76_set(dev, MT_WFDMA0_RST,
			 MT_WFDMA0_RST_DMASHDL_ALL_RST |
			 MT_WFDMA0_RST_LOGIC_RST);
	}

	return 0;
}
EXPORT_SYMBOL_GPL(mt792x_dma_disable);

void mt792x_dma_cleanup(struct mt792x_dev *dev)
{
	/* disable */
	mt76_clear(dev, MT_WFDMA0_GLO_CFG,
		   MT_WFDMA0_GLO_CFG_TX_DMA_EN |
		   MT_WFDMA0_GLO_CFG_RX_DMA_EN |
		   MT_WFDMA0_GLO_CFG_CSR_DISP_BASE_PTR_CHAIN_EN |
		   MT_WFDMA0_GLO_CFG_OMIT_TX_INFO |
		   MT_WFDMA0_GLO_CFG_OMIT_RX_INFO |
		   MT_WFDMA0_GLO_CFG_OMIT_RX_INFO_PFET2);

	mt76_poll_msec_tick(dev, MT_WFDMA0_GLO_CFG,
			    MT_WFDMA0_GLO_CFG_TX_DMA_BUSY |
			    MT_WFDMA0_GLO_CFG_RX_DMA_BUSY, 0, 100, 1);

	/* reset */
	mt76_clear(dev, MT_WFDMA0_RST,
		   MT_WFDMA0_RST_DMASHDL_ALL_RST |
		   MT_WFDMA0_RST_LOGIC_RST);

	mt76_set(dev, MT_WFDMA0_RST,
		 MT_WFDMA0_RST_DMASHDL_ALL_RST |
		 MT_WFDMA0_RST_LOGIC_RST);

	mt76_dma_cleanup(&dev->mt76);
}
EXPORT_SYMBOL_GPL(mt792x_dma_cleanup);

int mt792x_poll_tx(struct napi_struct *napi, int budget)
{
	struct mt792x_dev *dev;

	dev = container_of(napi, struct mt792x_dev, mt76.tx_napi);

	if (!mt76_connac_pm_ref(&dev->mphy, &dev->pm)) {
		napi_complete(napi);
		queue_work(dev->mt76.wq, &dev->pm.wake_work);
		return 0;
	}

	mt76_connac_tx_cleanup(&dev->mt76);
	if (napi_complete(napi))
		mt76_connac_irq_enable(&dev->mt76,
				       dev->irq_map->tx.all_complete_mask);
	mt76_connac_pm_unref(&dev->mphy, &dev->pm);

	return 0;
}
EXPORT_SYMBOL_GPL(mt792x_poll_tx);

int mt792x_poll_rx(struct napi_struct *napi, int budget)
{
	struct mt792x_dev *dev;
	int done;

	dev = mt76_priv(napi->dev);

	if (!mt76_connac_pm_ref(&dev->mphy, &dev->pm)) {
		napi_complete(napi);
		queue_work(dev->mt76.wq, &dev->pm.wake_work);
		return 0;
	}
	done = mt76_dma_rx_poll(napi, budget);
	mt76_connac_pm_unref(&dev->mphy, &dev->pm);

	return done;
}
EXPORT_SYMBOL_GPL(mt792x_poll_rx);

int mt792x_wfsys_reset(struct mt792x_dev *dev)
{
	u32 addr = is_connac2(&dev->mt76) ? 0x18000140 : 0x7c000140;

	mt76_clear(dev, addr, WFSYS_SW_RST_B);
	msleep(50);
	mt76_set(dev, addr, WFSYS_SW_RST_B);

	if (!__mt76_poll_msec(&dev->mt76, addr, WFSYS_SW_INIT_DONE,
			      WFSYS_SW_INIT_DONE, 500))
		return -ETIMEDOUT;

	return 0;
}
EXPORT_SYMBOL_GPL(mt792x_wfsys_reset);
