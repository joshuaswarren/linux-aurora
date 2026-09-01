// SPDX-License-Identifier: BSD-3-Clause-Clear
/* Copyright (C) 2021 MediaTek Inc. */

#include "mt7921.h"
#include "../dma.h"
#include "../mt76_connac2_mac.h"

int mt7921e_tx_prepare_skb(struct mt76_dev *mdev, void *txwi_ptr,
			   enum mt76_txq_id qid, struct mt76_wcid *wcid,
			   struct ieee80211_sta *sta,
			   struct mt76_tx_info *tx_info)
{
	struct mt792x_dev *dev = container_of(mdev, struct mt792x_dev, mt76);
	struct ieee80211_tx_info *info = IEEE80211_SKB_CB(tx_info->skb);
	struct ieee80211_key_conf *key = info->control.hw_key;
	struct mt76_connac_hw_txp *txp;
	struct mt76_txwi_cache *t;
	int id, pid;
	u8 *txwi = (u8 *)txwi_ptr;
	int i;

	if (unlikely(tx_info->skb->len <= ETH_HLEN))
		return -EINVAL;

	if (!wcid)
		wcid = &dev->mt76.global_wcid;
	if (is_mt7932(mdev)) {
		for (i = 1; i < tx_info->nbuf; i++)
			if (tx_info->buf[i].addr >> 35) {
				dev_err_ratelimited(mdev->dev,
					"J700_MT7932_TXP_ADDRESS_REJECT: buffer=%d dma=%pad limit=35-bit\n",
					i, &tx_info->buf[i].addr);
				return -ERANGE;
			}
	}

	t = (struct mt76_txwi_cache *)(txwi + mdev->drv->txwi_size);
	t->skb = tx_info->skb;

	id = mt76_token_consume(mdev, &t);
	if (id < 0)
		return id;

	if (sta) {
		struct mt792x_sta *msta = (struct mt792x_sta *)sta->drv_priv;

		if (time_after(jiffies, msta->deflink.last_txs + HZ / 4)) {
			info->flags |= IEEE80211_TX_CTL_REQ_TX_STATUS;
			msta->deflink.last_txs = jiffies;
		}
	}

	pid = mt76_tx_status_skb_add(mdev, wcid, tx_info->skb);
	mt76_connac2_mac_write_txwi(mdev, txwi_ptr, tx_info->skb, wcid, key,
				    pid, qid, 0);
	if (wcid->tx_info & MT_WCID_TX_INFO_J700_FULLMAC) {
		__le32 *txd = txwi_ptr;

		dev_info_ratelimited(mdev->dev,
			 "J700_MT7932_FULLMAC_TX_PREPARE: qid=%u wcid=%u token=%d pid=%u bytes=%u txd=%08x/%08x/%08x/%08x/%08x/%08x/%08x/%08x payload=redacted\n",
			 qid, wcid->idx, id, pid, tx_info->skb->len,
			 le32_to_cpu(txd[0]), le32_to_cpu(txd[1]),
			 le32_to_cpu(txd[2]), le32_to_cpu(txd[3]),
			 le32_to_cpu(txd[4]), le32_to_cpu(txd[5]),
			 le32_to_cpu(txd[6]), le32_to_cpu(txd[7]));
	}
	if (is_mt7932(mdev) &&
	    !(info->flags & IEEE80211_TX_CTL_HW_80211_ENCAP) &&
	    ieee80211_is_auth(((struct ieee80211_hdr *)
			      tx_info->skb->data)->frame_control)) {
		__le32 *txd = txwi_ptr;
		struct ieee80211_mgmt *mgmt =
			(struct ieee80211_mgmt *)tx_info->skb->data;
		bool da_is_bssid = ether_addr_equal(mgmt->da, mgmt->bssid);
		bool sa_is_vif = info->control.vif &&
			ether_addr_equal(mgmt->sa, info->control.vif->addr);

		dev_info(mdev->dev,
			 "J700_MT7932_AUTH_TX_PREPARE: qid=%u wcid=%u pid=%u bytes=%u nbuf=%u alg=%u transaction=%u status=%u da_is_bssid=%u sa_is_vif=%u txd=%08x/%08x/%08x/%08x/%08x/%08x/%08x/%08x\n",
			 qid, wcid->idx, pid, tx_info->skb->len, tx_info->nbuf,
			 le16_to_cpu(mgmt->u.auth.auth_alg),
			 le16_to_cpu(mgmt->u.auth.auth_transaction),
			 le16_to_cpu(mgmt->u.auth.status_code),
			 da_is_bssid, sa_is_vif,
			 le32_to_cpu(txd[0]), le32_to_cpu(txd[1]),
			 le32_to_cpu(txd[2]), le32_to_cpu(txd[3]),
			 le32_to_cpu(txd[4]), le32_to_cpu(txd[5]),
			 le32_to_cpu(txd[6]), le32_to_cpu(txd[7]));
	}

	txp = (struct mt76_connac_hw_txp *)(txwi + MT_TXD_SIZE);
	memset(txp, 0, sizeof(struct mt76_connac_hw_txp));
	mt76_connac_write_hw_txp(mdev, tx_info, txp, id);

	tx_info->skb = NULL;

	return 0;
}

int mt7921e_mac_reset(struct mt792x_dev *dev)
{
	int i, err;

	mt792xe_mcu_drv_pmctrl(dev);

	mt76_connac_free_pending_tx_skbs(&dev->pm, NULL);

	mt76_wr(dev, dev->irq_map->host_irq_enable, 0);
	mt76_wr(dev, MT_PCIE_MAC_INT_ENABLE, 0x0);

	set_bit(MT76_MCU_RESET, &dev->mphy.state);
	wake_up(&dev->mt76.mcu.wait);
	skb_queue_purge(&dev->mt76.mcu.res_q);

	mt76_txq_schedule_all(&dev->mphy);

	mt76_worker_disable(&dev->mt76.tx_worker);
	mt76_for_each_q_rx(&dev->mt76, i) {
		napi_disable(&dev->mt76.napi[i]);
	}
	napi_disable(&dev->mt76.tx_napi);

	mt76_connac2_tx_token_put(&dev->mt76);
	idr_init(&dev->mt76.token);

	mt792x_wpdma_reset(dev, true);

	mt76_for_each_q_rx(&dev->mt76, i) {
		napi_enable(&dev->mt76.napi[i]);
	}

	local_bh_disable();
	mt76_for_each_q_rx(&dev->mt76, i) {
		napi_schedule(&dev->mt76.napi[i]);
	}
	local_bh_enable();

	dev->fw_assert = false;
	clear_bit(MT76_MCU_RESET, &dev->mphy.state);

	mt76_wr(dev, dev->irq_map->host_irq_enable,
		dev->irq_map->tx.all_complete_mask |
		MT_INT_RX_DONE_ALL | MT_INT_MCU_CMD);
	mt76_wr(dev, MT_PCIE_MAC_INT_ENABLE, 0xff);

	err = mt7921e_driver_own(dev);
	if (err)
		goto out;

	err = mt7921_run_firmware(dev);
	if (err)
		goto out;

	if (is_mt7932(&dev->mt76)) {
		err = mt7921_reset_hardware(dev);
		if (err)
			goto out;
		err = mt7932_apply_manufacture_policy(dev);
		if (err)
			goto out;
	} else {
		err = mt7921_mcu_set_eeprom(dev);
		if (err)
			goto out;

		err = mt7921_mac_init(dev);
		if (err)
			goto out;
	}

	err = __mt7921_start(&dev->phy);
out:

	napi_enable(&dev->mt76.tx_napi);
	local_bh_disable();
	napi_schedule(&dev->mt76.tx_napi);
	local_bh_enable();

	mt76_worker_enable(&dev->mt76.tx_worker);

	return err;
}
