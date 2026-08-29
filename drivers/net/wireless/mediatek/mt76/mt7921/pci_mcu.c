// SPDX-License-Identifier: BSD-3-Clause-Clear
/* Copyright (C) 2021 MediaTek Inc. */

#include "mt7921.h"
#include "mcu.h"
#include "../dma.h"

#define MT7932_FW_ENTRY_WITNESS	0x7c053a5c

int mt7921e_driver_own(struct mt792x_dev *dev)
{
	u32 reg;
	int ret;

	if (is_mt7932(&dev->mt76)) {
		ret = __mt792xe_mcu_drv_pmctrl(dev);
		if (!ret)
			dev_info(dev->mt76.dev,
				 "J700_MT7932_DRIVER_OWN_PASS: register=0x%08x request=0x%08x sync_mask=0x%08x source=apple-connac2\n",
				 MT_CONN_ON_LPCTL,
				 (u32)PCIE_LPCR_HOST_CLR_OWN,
				 (u32)PCIE_LPCR_HOST_OWN_SYNC);
		return ret;
	}

	reg = mt7921_reg_map_l1(dev, MT_TOP_LPCR_HOST_BAND0);
	mt76_wr(dev, reg, MT_TOP_LPCR_HOST_DRV_OWN);
	if (!mt76_poll_msec(dev, reg, MT_TOP_LPCR_HOST_FW_OWN,
			    0, 500)) {
		dev_err(dev->mt76.dev, "Timeout for driver own\n");
		return -EIO;
	}

	return 0;
}

static int
mt7921_mcu_send_message(struct mt76_dev *mdev, struct sk_buff *skb,
			int cmd, int *seq)
{
	struct mt792x_dev *dev = container_of(mdev, struct mt792x_dev, mt76);
	enum mt76_mcuq_id txq = MT_MCUQ_WM;
	struct mt76_queue *q;
	int ret;

	ret = mt76_connac2_mcu_fill_message(mdev, skb, cmd, seq);
	if (ret)
		return ret;

	mdev->mcu.timeout = is_mt7932(mdev) ? 5 * HZ : 3 * HZ;

	if (cmd == MCU_CMD(FW_SCATTER))
		txq = MT_MCUQ_FWDL;
	q = mdev->q_mcu[txq];
	/*
	 * Apple 25G83 kalDevPortWrite() does not enlarge the 24-entry TX17
	 * command ring.  When used + 1 reaches the ring depth it first calls
	 * halWpdmaProcessCmdDmaDone(), which reclaims the completed prefix, and
	 * only rejects the new command if the ring remains full afterwards.
	 *
	 * The generic raw mt76 path checks capacity before doing any cleanup.
	 * That made the earlier exact-depth experiment stop at the first wrap,
	 * while Run 191 proves Apple wraps the same 24 slots thousands of times.
	 * Keep ordinary interrupt-driven cleanup, but reproduce Apple's bounded
	 * pressure fallback before the generic capacity check.
	 */
	if (is_mt7932(mdev) && txq == MT_MCUQ_WM &&
	    READ_ONCE(q->queued) + 1 >= q->ndesc &&
	    mdev->queue_ops->tx_cleanup) {
		u32 before = READ_ONCE(q->queued);
		u32 tail_ctrl = le32_to_cpu(READ_ONCE(q->desc[q->tail].ctrl));
		u32 didx = readl(&q->regs->dma_idx);
		unsigned int polls = 0;

		mdev->queue_ops->tx_cleanup(mdev, q, false);
		/* Linux can issue the post-registration command burst faster than
		 * AppleSunriseWLAN's workloop.  Do not silently lose a channel or
		 * interface command while DIDX is one scheduling quantum behind.
		 * This only waits for hardware-owned descriptors to retire; it never
		 * overwrites a full ring or fabricates completion.
		 */
		while (READ_ONCE(q->queued) + 1 >= q->ndesc &&
		       polls++ < 50) {
			udelay(100);
			mdev->queue_ops->tx_cleanup(mdev, q, false);
		}
		dev_info(mdev->dev,
			 "J700_MT7932_TX17_PRESSURE_RECLAIM: before=%u after=%u head=%u tail=%u didx=%u tail_ctrl=0x%08x depth=%u wait_us=%u\n",
			 before, READ_ONCE(q->queued), q->head, q->tail,
			 didx, tail_ctrl, q->ndesc, polls * 100);
		if (READ_ONCE(q->queued) + 1 >= q->ndesc &&
		    q->tx_bounce_buf && q->tx_bounce_stride) {
			struct mt76_queue_entry *entry = &q->entry[q->tail];
			dma_addr_t dma = entry->dma_addr[0];
			u64 bounce_bytes = (u64)q->tx_bounce_entries *
					   q->tx_bounce_stride;

			if (dma >= q->tx_bounce_dma &&
			    dma - q->tx_bounce_dma < bounce_bytes) {
				u32 slot = div_u64(dma - q->tx_bounce_dma,
						   q->tx_bounce_stride);
				const u8 *payload = q->tx_bounce_buf +
						    slot * q->tx_bounce_stride;
				u16 bytes = min_t(u16, entry->dma_len[0], 64);

				dev_info_ratelimited(mdev->dev,
					 "J700_MT7932_TX17_STALL_TAIL: tail=%u slot=%u dma_len=%u desc_ctrl=0x%08x desc_info=0x%08x payload=%*ph\n",
					 q->tail, slot, entry->dma_len[0],
					 le32_to_cpu(READ_ONCE(q->desc[q->tail].ctrl)),
					 le32_to_cpu(READ_ONCE(q->desc[q->tail].info)),
					 bytes, payload);
			}
		}
	}

	if (is_mt7932(mdev) && cmd == MCU_CMD(FW_START_REQ))
		mt76_wr(dev, MT7932_FW_ENTRY_WITNESS, 1);

	ret = mt76_tx_queue_skb_raw(dev, q, skb, 0);
	if (!ret && is_mt7932(mdev) && cmd == MCU_CMD(FW_START_REQ)) {
		static const unsigned int delays_us[] = {
			500, 500, 1000, 2000, 4000, 8000,
			8000, 8000, 1000, 1000, 1000, 1000, 1000, 1000,
			1000, 1000,
		};
		u32 pc[ARRAY_SIZE(delays_us)];
		u32 witness[ARRAY_SIZE(delays_us)];
		u32 fw_state[ARRAY_SIZE(delays_us)];
		unsigned int i;

		/* Cycles 363-365 proved that the WM-PC/RGU debug domain can return
		 * all ones while the fixed DRV_FW_STAT_SYNC register is still live.
		 * Keep the Cycle-350 nonzero main-entry witness armed and continue
		 * sampling both fixed registers after PC visibility is lost.  Only
		 * stop when both fixed registers are dead.
		 */
		for (i = 0; i < ARRAY_SIZE(pc); i++) {
			pc[i] = U32_MAX;
			witness[i] = U32_MAX;
			fw_state[i] = U32_MAX;
		}
		for (i = 0; i < ARRAY_SIZE(delays_us); i++) {
			udelay(delays_us[i]);
			pc[i] = mt76_rr(dev, MT7932_WM_MCU_PC_DBG);
			witness[i] = mt76_rr(dev, MT7932_FW_ENTRY_WITNESS);
			fw_state[i] = mt76_rr(dev, MT_CONN_ON_MISC);
			if (witness[i] == U32_MAX && fw_state[i] == U32_MAX)
				break;
		}
		dev_info(mdev->dev,
			 "J700_MT7932_FW_HANDOFF_PC: us=500/1000/2000/4000/8000/16000/24000/32000/33000/34000/35000/36000/37000/38000/39000/40000 pc=%08x/%08x/%08x/%08x/%08x/%08x/%08x/%08x/%08x/%08x/%08x/%08x/%08x/%08x/%08x/%08x selector=0x3f stop_on_fixed_dead=1\n",
			 pc[0], pc[1], pc[2], pc[3], pc[4], pc[5], pc[6],
			 pc[7], pc[8], pc[9], pc[10], pc[11], pc[12], pc[13],
			 pc[14], pc[15]);
		dev_info(mdev->dev,
			 "J700_MT7932_FW_HANDOFF_WITNESS: us=500/1000/2000/4000/8000/16000/24000/32000/33000/34000/35000/36000/37000/38000/39000/40000 marker=%08x/%08x/%08x/%08x/%08x/%08x/%08x/%08x/%08x/%08x/%08x/%08x/%08x/%08x/%08x/%08x state=%08x/%08x/%08x/%08x/%08x/%08x/%08x/%08x/%08x/%08x/%08x/%08x/%08x/%08x/%08x/%08x seed=1 main-entry-writes=3\n",
			 witness[0], witness[1], witness[2], witness[3],
			 witness[4], witness[5], witness[6], witness[7],
			 witness[8], witness[9], witness[10], witness[11],
			 witness[12], witness[13], witness[14], witness[15],
			 fw_state[0], fw_state[1], fw_state[2], fw_state[3],
			 fw_state[4], fw_state[5], fw_state[6], fw_state[7],
			 fw_state[8], fw_state[9], fw_state[10], fw_state[11],
			 fw_state[12], fw_state[13], fw_state[14], fw_state[15]);
	}

	return ret;
}

int mt7921e_mcu_init(struct mt792x_dev *dev)
{
	static const struct mt76_mcu_ops mt7921_mcu_ops = {
		.headroom = sizeof(struct mt76_connac2_mcu_txd),
		.mcu_skb_send_msg = mt7921_mcu_send_message,
		.mcu_parse_response = mt7921_mcu_parse_response,
	};
	struct mt76_queue *cleanup_q;
	int err;

	dev->mt76.mcu_ops = &mt7921_mcu_ops;

	/* MT7932 ownership was already claimed before WFDMA setup in PCI probe,
	 * matching AppleSunriseWLAN's single pre-initialization transition.
	 */
	if (!is_mt7932(&dev->mt76)) {
		err = mt7921e_driver_own(dev);
		if (err)
			return err;
	}

	/* MT_PCIE_MAC_PM is BAR 0x10194, which __mt7921_reg_addr() maps from
	 * chip address 0x74030000 (PCIE_MAC_IREG) - the device's own PCIe MAC
	 * control block.  mt76's window assumptions have already been shown
	 * wrong for MT7932 once, at 0x7c050000 (Cycles 196-198), and the
	 * endpoint leaves the link cleanly with no uncorrectable AER errors.
	 * Skip the write on MT7932 until the layout is confirmed.
	 */
	if (!is_mt7932(&dev->mt76))
		mt76_rmw_field(dev, MT_PCIE_MAC_PM, MT_PCIE_MAC_PM_L0S_DIS, 1);
	else
		dev_info(dev->mt76.dev,
			 "J700_MT7932_PCIE_MAC_PM_SKIP: pm=0x%08x left untouched\n",
			 mt76_rr(dev, MT_PCIE_MAC_PM));

	err = mt7921_run_firmware(dev);
	cleanup_q = is_mt7932(&dev->mt76) ? dev->mphy.q_tx[MT_TXQ_BE] :
						  dev->mt76.q_mcu[MT_MCUQ_FWDL];

	if (!is_mt7932(&dev->mt76) || !err)
		mt76_queue_tx_cleanup(dev, cleanup_q, false);
	else
		dev_warn(dev->mt76.dev,
			 "J700_MT7932_FWDL_CLEANUP_SKIPPED: preserving diagnostic state after err=%d\n",
			 err);

	return err;
}
