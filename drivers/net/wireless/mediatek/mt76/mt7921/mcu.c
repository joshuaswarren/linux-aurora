// SPDX-License-Identifier: BSD-3-Clause-Clear
/* Copyright (C) 2020 MediaTek Inc. */

#include <linux/fs.h>
#include <linux/firmware.h>
#include <linux/pci.h>
#include <linux/unaligned.h>
#include "mt7921.h"
#include "regd.h"
#include "mcu.h"
#include "../dma.h"
#include "../mt76_connac2_mac.h"
#include "../mt792x_trace.h"

#define MT_STA_BFER			BIT(0)
#define MT_STA_BFEE			BIT(1)

static bool mt7921_disable_clc;
module_param_named(disable_clc, mt7921_disable_clc, bool, 0644);
MODULE_PARM_DESC(disable_clc, "disable CLC support");

/* AppleSunriseWLAN 25G83 has two tables in wlanInitEventStatusCodeToStr().
 * MT7922 uses the public gen4-mt79xx 0..6 ABI.  Every other chip, including
 * J700's MT7932, uses this distinct 1..13 ABI.  wlanConfigWifiFuncStatus()
 * explicitly accepts status 1 for MT7932 and reads the result from byte 8 of
 * the compact INIT_WIFI_EVENT; byte 9 is reserved, not a response CID.
 *
 * The exact names matter here.  Cycles 346-348 returned status 4 after their
 * RAM entry bytes were changed, which is MT7932 "invalid crc", not the MT7922
 * table's "unknown cmd".  Authentic firmware has not yet returned a result,
 * but a later response must be decoded and accepted with the Apple ABI.
 */
static const char *mt7932_fw_start_status_str(u8 status)
{
	static const char * const names[] = {
		[1] = "success",
		[2] = "unknown",
		[3] = "invalid param",
		[4] = "invalid crc",
		[5] = "timeout",
		[6] = "sec boot check fail",
		[7] = "region check fail",
		[8] = "cmd size check fail",
		[9] = "RAM entry check fail",
		[10] = "Section check fail",
		[11] = "FW download flow check fail",
		[12] = "FW download cmd logic check fail",
		[13] = "automatic",
	};

	return status < ARRAY_SIZE(names) && names[status] ?
	       names[status] : "unknown status";
}

int mt7921_mcu_parse_response(struct mt76_dev *mdev, int cmd,
			      struct sk_buff *skb, int seq)
{
	int mcu_cmd = FIELD_GET(__MCU_CMD_FIELD_ID, cmd);
	struct mt76_connac2_mcu_rxd *rxd;
	int ret = 0;

	if (!skb) {
		dev_err(mdev->dev, "Message %08x (seq %d) timeout\n",
			cmd, seq);
		mt792x_reset(mdev);

		return -ETIMEDOUT;
	}

	rxd = (struct mt76_connac2_mcu_rxd *)skb->data;
	if (seq != rxd->seq)
		return -EAGAIN;

	if (cmd == MCU_CMD(PATCH_SEM_CONTROL) ||
	    cmd == MCU_CMD(PATCH_FINISH_REQ)) {
		skb_pull(skb, sizeof(*rxd) - 4);
		ret = *skb->data;
		if (is_mt7932(mdev) && cmd == MCU_CMD(PATCH_SEM_CONTROL))
			dev_info(mdev->dev,
				 "J700_MT7932_PATCH_SEMAPHORE_RESPONSE: eid=%u seq=%u option=%u status=%d\n",
				 rxd->eid, rxd->seq, rxd->option, ret);
		else if (is_mt7932(mdev) && cmd == MCU_CMD(PATCH_FINISH_REQ))
			dev_info(mdev->dev,
				 "J700_MT7932_PATCH_FINISH_RESPONSE: eid=%u seq=%u option=%u status=%d\n",
				 rxd->eid, rxd->seq, rxd->option, ret);
		if (is_mt7932(mdev) && cmd == MCU_CMD(PATCH_FINISH_REQ) && ret == 1) {
			dev_info(mdev->dev,
				 "J700_MT7932_PATCH_FINISH_ACCEPT: status=1 apple_semantics=success\n");
			ret = 0;
		}
	} else if (is_mt7932(mdev) && cmd == MCU_CMD(INIT_ACCESS_REG)) {
		if (rxd->eid != MCU_EVENT_ACCESS_REG)
			return -EPROTO;
		/* The 24-byte MT7932 RX descriptor is followed by the 8-byte
		 * INIT_WIFI_EVENT header.  The generic 36-byte Connac RXD view
		 * overlaps the first address word, so expose it like the compact
		 * patch/result events above.
		 */
		skb_pull(skb, sizeof(*rxd) - 4);
	} else if (is_mt7932(mdev) &&
		   (cmd == MCU_CMD(TARGET_ADDRESS_LEN_REQ) ||
		    cmd == MCU_CMD(FW_START_REQ))) {
		u8 result_reserved;
		u8 response_count = 0;

		/* MT7932 init events have a 32-byte RXD/event header followed by
		 * INIT_EVENT_CMD_RESULT.  The final four bytes represented as ext_eid
		 * by the normal connac2 RXD are the status, CID and two reserved bytes.
		 */
		skb_pull(skb, sizeof(*rxd) - 4);
		if (skb->len < 4)
			return -EPROTO;
		ret = skb->data[0];
		result_reserved = skb->data[1];
		if (cmd != MCU_CMD(TARGET_ADDRESS_LEN_REQ))
			dev_info(mdev->dev,
				 "J700_MT7932_INIT_CMD_RESULT: eid=%u seq=%u reserved=0x%02x cmd=0x%02x status=%d\n",
				 rxd->eid, rxd->seq, result_reserved, mcu_cmd, ret);
		if (cmd == MCU_CMD(FW_START_REQ) && rxd->eid == 1 && ret != 1)
			dev_err(mdev->dev,
				"J700_MT7932_FW_START_REJECTED: status=%d (%s) reserved=0x%02x\n",
				ret, mt7932_fw_start_status_str(ret),
				result_reserved);

		if (cmd == MCU_CMD(TARGET_ADDRESS_LEN_REQ) && rxd->eid == 1 &&
		    ret == 1) {
			if (READ_ONCE(mdev->mcu.mt7932_ram_download_active)) {
				response_count =
					READ_ONCE(mdev->mcu.mt7932_ram_download_responses) + 1;
				if (response_count == 1)
					WRITE_ONCE(mdev->mcu.mt7932_ram_download_first_seq,
						   rxd->seq);
				WRITE_ONCE(mdev->mcu.mt7932_ram_download_responses,
					   response_count);
			}
			if (response_count == 5) {
				dev_info(mdev->dev,
					 "J700_MT7932_INIT_CMD_RESULT: eid=%u seq=%u reserved=0x%02x cmd=0x%02x status=%d\n",
					 rxd->eid, rxd->seq, result_reserved,
					 mcu_cmd, ret);
				dev_info(mdev->dev,
					 "J700_MT7932_DOWNLOAD_CONFIG_SWEEP_ACCEPT: first_seq=%u last_seq=%u count=%u status=1 reserved=0x%02x\n",
					 READ_ONCE(mdev->mcu.mt7932_ram_download_first_seq),
					 rxd->seq, response_count, result_reserved);
			}
			ret = 0;
		} else if (cmd == MCU_CMD(FW_START_REQ) && rxd->eid == 1 &&
			   ret == 1) {
			dev_info(mdev->dev,
				 "J700_MT7932_FW_START_ACCEPT: status=1 reserved=0x%02x source=apple-25G83\n",
				 result_reserved);
			ret = 0;
		} else if (rxd->eid != 1) {
			ret = -EPROTO;
		}
	} else if (cmd == MCU_EXT_CMD(THERMAL_CTRL)) {
		skb_pull(skb, sizeof(*rxd) + 4);
		ret = le32_to_cpu(*(__le32 *)skb->data);
	} else if (cmd == MCU_UNI_CMD(DEV_INFO_UPDATE) ||
		   cmd == MCU_UNI_CMD(BSS_INFO_UPDATE) ||
		   cmd == MCU_UNI_CMD(STA_REC_UPDATE) ||
		   cmd == MCU_UNI_CMD(HIF_CTRL) ||
		   cmd == MCU_UNI_CMD(OFFLOAD) ||
		   cmd == MCU_UNI_CMD(SUSPEND)) {
		struct mt76_connac_mcu_uni_event *event;

		skb_pull(skb, sizeof(*rxd));
		event = (struct mt76_connac_mcu_uni_event *)skb->data;
		ret = le32_to_cpu(event->status);
		/* skip invalid event */
		if (mcu_cmd != event->cid)
			ret = -EAGAIN;
	} else if (cmd == MCU_CE_QUERY(REG_READ)) {
		struct mt76_connac_mcu_reg_event *event;

		skb_pull(skb, sizeof(*rxd));
		event = (struct mt76_connac_mcu_reg_event *)skb->data;
		ret = (int)le32_to_cpu(event->val);
	} else if (cmd == MCU_EXT_CMD(WF_RF_PIN_CTRL)) {
		struct mt7921_wf_rf_pin_ctrl_event *event;

		skb_pull(skb, sizeof(*rxd));
		event = (struct mt7921_wf_rf_pin_ctrl_event *)skb->data;
		ret = (int)event->result;
	} else {
		skb_pull(skb, sizeof(struct mt76_connac2_mcu_rxd));
	}

	return ret;
}
EXPORT_SYMBOL_GPL(mt7921_mcu_parse_response);

static int mt7921_mcu_read_eeprom(struct mt792x_dev *dev, u32 offset, u8 *val)
{
	struct mt7921_mcu_eeprom_info *res, req = {
		.addr = cpu_to_le32(round_down(offset,
				    MT7921_EEPROM_BLOCK_SIZE)),
	};
	struct sk_buff *skb;
	int ret;

	ret = mt76_mcu_send_and_get_msg(&dev->mt76, MCU_EXT_QUERY(EFUSE_ACCESS),
					&req, sizeof(req), true, &skb);
	if (ret)
		return ret;

	res = (struct mt7921_mcu_eeprom_info *)skb->data;
	*val = res->data[offset % MT7921_EEPROM_BLOCK_SIZE];
	dev_kfree_skb(skb);

	return 0;
}

#ifdef CONFIG_PM

static int
mt7921_mcu_set_ipv6_ns_filter(struct mt76_dev *dev,
			      struct ieee80211_vif *vif, bool suspend)
{
	struct mt792x_vif *mvif = (struct mt792x_vif *)vif->drv_priv;
	struct {
		struct {
			u8 bss_idx;
			u8 pad[3];
		} __packed hdr;
		struct mt76_connac_arpns_tlv arpns;
	} req = {
		.hdr = {
			.bss_idx = mvif->bss_conf.mt76.idx,
		},
		.arpns = {
			.tag = cpu_to_le16(UNI_OFFLOAD_OFFLOAD_ND),
			.len = cpu_to_le16(sizeof(struct mt76_connac_arpns_tlv)),
			.mode = suspend,
		},
	};

	return mt76_mcu_send_msg(dev, MCU_UNI_CMD_OFFLOAD, &req, sizeof(req),
				 true);
}

void mt7921_mcu_set_suspend_iter(void *priv, u8 *mac, struct ieee80211_vif *vif)
{
	if (IS_ENABLED(CONFIG_IPV6)) {
		struct mt76_phy *phy = priv;

		mt7921_mcu_set_ipv6_ns_filter(phy->dev, vif,
					      !test_bit(MT76_STATE_RUNNING,
					      &phy->state));
	}

	mt76_connac_mcu_set_suspend_iter(priv, mac, vif);
}

#endif /* CONFIG_PM */

static void
mt7921_mcu_uni_roc_event(struct mt792x_dev *dev, struct sk_buff *skb)
{
	struct mt7921_roc_grant_tlv *grant;
	struct mt76_connac2_mcu_rxd *rxd;
	int duration;

	rxd = (struct mt76_connac2_mcu_rxd *)skb->data;
	grant = (struct mt7921_roc_grant_tlv *)(rxd->tlv + 4);

	/* should never happen */
	WARN_ON_ONCE((le16_to_cpu(grant->tag) != UNI_EVENT_ROC_GRANT));

	if (grant->reqtype == MT7921_ROC_REQ_ROC)
		ieee80211_ready_on_channel(dev->mt76.phy.hw);

	dev->phy.roc_grant = true;
	wake_up(&dev->phy.roc_wait);
	duration = le32_to_cpu(grant->max_interval);
	mod_timer(&dev->phy.roc_timer,
		  jiffies + msecs_to_jiffies(duration));
}

static void
mt7921_mcu_scan_event(struct mt792x_dev *dev, struct sk_buff *skb)
{
	struct mt76_phy *mphy = &dev->mt76.phy;
	struct mt792x_phy *phy = mphy->priv;
	struct mt76_connac2_mcu_rxd *rxd = (void *)skb->data;

	if (is_mt7932(&dev->mt76))
		dev_info(dev->mt76.dev,
			 "J700_MT7932_SCAN_EVENT_INGRESS: eid=%u seq=%u len=%u\n",
			 rxd->eid, rxd->seq, skb->len);

	spin_lock_bh(&dev->mt76.lock);
	__skb_queue_tail(&phy->scan_event_list, skb);
	spin_unlock_bh(&dev->mt76.lock);

	ieee80211_queue_delayed_work(mphy->hw, &phy->scan_work,
				     MT792x_HW_SCAN_TIMEOUT);
}

static void
mt7921_mcu_connection_loss_iter(void *priv, u8 *mac,
				struct ieee80211_vif *vif)
{
	struct mt76_vif_link *mvif = (struct mt76_vif_link *)vif->drv_priv;
	struct mt76_connac_beacon_loss_event *event = priv;

	if (mvif->idx != event->bss_idx)
		return;

	if (!(vif->driver_flags & IEEE80211_VIF_BEACON_FILTER) ||
	    vif->type != NL80211_IFTYPE_STATION)
		return;

	ieee80211_connection_loss(vif);
}

static void
mt7921_mcu_connection_loss_event(struct mt792x_dev *dev, struct sk_buff *skb)
{
	struct mt76_connac_beacon_loss_event *event;
	struct mt76_phy *mphy = &dev->mt76.phy;

	skb_pull(skb, sizeof(struct mt76_connac2_mcu_rxd));
	event = (struct mt76_connac_beacon_loss_event *)skb->data;

	ieee80211_iterate_active_interfaces_atomic(mphy->hw,
					IEEE80211_IFACE_ITER_RESUME_ALL,
					mt7921_mcu_connection_loss_iter, event);
}

static void
mt7932_mcu_scan_idx_event(struct mt792x_dev *dev, const u8 *content, u16 len)
{
	static const struct {
		u32 idx;
		const char *stage;
	} scan_idx[] = {
		{ 0xca8c, "cid3-parse" },
		{ 0xca10, "cid3-fields" },
		{ 0xc9ac, "cid3-submit" },
		{ 0xdb04, "arbitrator" },
		{ 0xdab0, "blocked-monitor" },
		{ 0xda70, "blocked-generic" },
		{ 0xda30, "blocked-mlme" },
		{ 0xd9f0, "blocked-host" },
		{ 0xd9b0, "blocked-roam" },
		{ 0xdc50, "scan-start" },
		{ 0xdc08, "scan-start-detail" },
		{ 0xdbc4, "exclusive-channel-set" },
		{ 0xdb78, "scan-fsm-info" },
		{ 0xde34, "cnm-request-alloc-failed" },
		{ 0xde0c, "cnm-track-alloc-failed" },
		{ 0xddb0, "cnm-abort-message" },
		{ 0xdd6c, "cnm-abort-failed" },
		{ 0xdcc0, "grant-auto-error" },
		{ 0xdcfc, "exclusive-channel-begin" },
		{ 0xd900, "fsm-abort" },
		{ 0xd868, "scan-command-band" },
		{ 0xd840, "channel-state" },
		{ 0xd818, "channel-state-detail" },
		{ 0xd7b8, "channel-set" },
		{ 0xd794, "channel-anomaly" },
		{ 0x3a58, "cnm-open-channel-request" },
		{ 0x3728, "cnm-channel-request-consumed" },
		{ 0x3840, "cnm-open-grant" },
		{ 0x3a94, "cnm-open-wa" },
		{ 0x23e04, "cnm-fsm" },
		{ 0x24690, "cnm-timeline-blocked" },
		{ 0x25034, "cnm-grant-not-served" },
		{ 0x25080, "cnm-grant-resync" },
		{ 0xd940, "cnm-slot" },
		{ 0xd6ec, "fsm-timeout" },
		{ 0xd5ac, "scan-done" },
		{ 0xd580, "scan-complete" },
	};
	int i, j;

	/* Mode 2 batches variable-size A8 records behind 0x7f padding.  Search
	 * the bounded event payload for exact little-endian catalogue offsets;
	 * do not turn arbitrary binary records into console strings.
	 */
	for (i = 0; i + sizeof(__le32) <= len; i++) {
		u32 idx = get_unaligned_le32(content + i);

		for (j = 0; j < ARRAY_SIZE(scan_idx); j++) {
			int start, bytes;

			if (idx != scan_idx[j].idx)
				continue;
			start = max(i - 12, 0);
			bytes = min_t(int, len - start, 48);
			dev_info(dev->mt76.dev,
				 "J700_MT7932_SCAN_FW_IDX: stage=%s idx=0x%08x offset=%d raw=%*ph\n",
				 scan_idx[j].stage, idx, i, bytes,
				 content + start);
		}
	}
}

static void
mt7921_mcu_debug_msg_event(struct mt792x_dev *dev, struct sk_buff *skb)
{
	struct mt7932_debug_msg {
		__le16 id;
		u8 type;
		u8 flag;
		__le32 value;
		__le16 len;
		u8 reserved[2];
		u8 content[];
	} __packed * msg7932;
	struct mt7921_debug_msg {
		__le16 id;
		u8 type;
		u8 flag;
		__le32 value;
		__le16 len;
		u8 content[512];
	} __packed * msg;

	skb_pull(skb, sizeof(struct mt76_connac2_mcu_rxd));
	if (is_mt7932(&dev->mt76)) {
		u16 len;

		if (skb->len < sizeof(*msg7932))
			return;
		msg7932 = (void *)skb->data;
		len = min_t(u16, le16_to_cpu(msg7932->len),
			    skb->len - sizeof(*msg7932));
		mt7932_mcu_scan_idx_event(dev, msg7932->content, len);
		return;
	}

	msg = (struct mt7921_debug_msg *)skb->data;

	if (msg->type == 3) { /* fw log */
		u16 len = min_t(u16, le16_to_cpu(msg->len), 512);
		int i;

		for (i = 0 ; i < len; i++) {
			if (!msg->content[i])
				msg->content[i] = ' ';
		}
		wiphy_info(mt76_hw(dev)->wiphy, "%.*s", len, msg->content);
	}
}

static void
mt7921_mcu_low_power_event(struct mt792x_dev *dev, struct sk_buff *skb)
{
	struct mt7921_mcu_lp_event {
		u8 state;
		u8 reserved[3];
	} __packed * event;

	skb_pull(skb, sizeof(struct mt76_connac2_mcu_rxd));
	event = (struct mt7921_mcu_lp_event *)skb->data;

	trace_lp_event(dev, event->state);
}

static void
mt7921_mcu_tx_done_event(struct mt792x_dev *dev, struct sk_buff *skb)
{
	struct mt7921_mcu_tx_done_event *event;

	skb_pull(skb, sizeof(struct mt76_connac2_mcu_rxd));
	event = (struct mt7921_mcu_tx_done_event *)skb->data;

	mt7921_mac_add_txs(dev, event->txs);
}

static void
mt7921_mcu_rssi_monitor_iter(void *priv, u8 *mac,
			     struct ieee80211_vif *vif)
{
	struct mt792x_vif *mvif = (struct mt792x_vif *)vif->drv_priv;
	struct mt76_connac_rssi_notify_event *event = priv;
	enum nl80211_cqm_rssi_threshold_event nl_event;
	s32 rssi = le32_to_cpu(event->rssi[mvif->bss_conf.mt76.idx]);

	if (!rssi)
		return;

	if (!(vif->driver_flags & IEEE80211_VIF_SUPPORTS_CQM_RSSI))
		return;

	if (rssi > vif->bss_conf.cqm_rssi_thold)
		nl_event = NL80211_CQM_RSSI_THRESHOLD_EVENT_HIGH;
	else
		nl_event = NL80211_CQM_RSSI_THRESHOLD_EVENT_LOW;

	ieee80211_cqm_rssi_notify(vif, nl_event, rssi, GFP_KERNEL);
}

static void
mt7921_mcu_rssi_monitor_event(struct mt792x_dev *dev, struct sk_buff *skb)
{
	struct mt76_connac_rssi_notify_event *event;

	skb_pull(skb, sizeof(struct mt76_connac2_mcu_rxd));
	event = (struct mt76_connac_rssi_notify_event *)skb->data;

	ieee80211_iterate_active_interfaces_atomic(mt76_hw(dev),
						   IEEE80211_IFACE_ITER_RESUME_ALL,
						   mt7921_mcu_rssi_monitor_iter, event);
}

static void
mt7921_mcu_rx_unsolicited_event(struct mt792x_dev *dev, struct sk_buff *skb)
{
	struct mt76_connac2_mcu_rxd *rxd;

	rxd = (struct mt76_connac2_mcu_rxd *)skb->data;
	switch (rxd->eid) {
	case MCU_EVENT_BSS_BEACON_LOSS:
		mt7921_mcu_connection_loss_event(dev, skb);
		break;
	case MCU_EVENT_SCHED_SCAN_DONE:
	case MCU_EVENT_SCAN_DONE:
		mt7921_mcu_scan_event(dev, skb);
		return;
	case MCU_EVENT_DBG_MSG:
		mt7921_mcu_debug_msg_event(dev, skb);
		break;
	case MCU_EVENT_COREDUMP:
		dev->fw_assert = true;
		mt76_connac_mcu_coredump_event(&dev->mt76, skb,
					       &dev->coredump);
		return;
	case MCU_EVENT_LP_INFO:
		mt7921_mcu_low_power_event(dev, skb);
		break;
	case MCU_EVENT_TX_DONE:
		mt7921_mcu_tx_done_event(dev, skb);
		break;
	case MCU_EVENT_RSSI_NOTIFY:
		mt7921_mcu_rssi_monitor_event(dev, skb);
		break;
	default:
		break;
	}
	dev_kfree_skb(skb);
}

static void
mt7921_mcu_uni_rx_unsolicited_event(struct mt792x_dev *dev,
				    struct sk_buff *skb)
{
	struct mt76_connac2_mcu_rxd *rxd;

	rxd = (struct mt76_connac2_mcu_rxd *)skb->data;

	switch (rxd->eid) {
	case MCU_UNI_EVENT_ROC:
		mt7921_mcu_uni_roc_event(dev, skb);
		break;
	case MCU_UNI_EVENT_SCAN_DONE:
		mt7921_mcu_scan_event(dev, skb);
		return;
	default:
		break;
	}
	dev_kfree_skb(skb);
}

void mt7921_mcu_rx_event(struct mt792x_dev *dev, struct sk_buff *skb)
{
	struct mt76_connac2_mcu_rxd *rxd;

	if (skb_linearize(skb))
		return;

	rxd = (struct mt76_connac2_mcu_rxd *)skb->data;

	if (rxd->option & MCU_UNI_CMD_UNSOLICITED_EVENT) {
		mt7921_mcu_uni_rx_unsolicited_event(dev, skb);
		return;
	}

	if (rxd->eid == 0x6) {
		mt76_mcu_rx_event(&dev->mt76, skb);
		return;
	}

	if (rxd->ext_eid == MCU_EXT_EVENT_RATE_REPORT ||
	    rxd->eid == MCU_EVENT_BSS_BEACON_LOSS ||
	    rxd->eid == MCU_EVENT_SCHED_SCAN_DONE ||
	    rxd->eid == MCU_EVENT_RSSI_NOTIFY ||
	    rxd->eid == MCU_EVENT_SCAN_DONE ||
	    rxd->eid == MCU_EVENT_TX_DONE ||
	    rxd->eid == MCU_EVENT_DBG_MSG ||
	    rxd->eid == MCU_EVENT_COREDUMP ||
	    rxd->eid == MCU_EVENT_LP_INFO ||
	    !rxd->seq)
		mt7921_mcu_rx_unsolicited_event(dev, skb);
	else
		mt76_mcu_rx_event(&dev->mt76, skb);
}

/** starec & wtbl **/
int mt7921_mcu_uni_tx_ba(struct mt792x_dev *dev,
			 struct ieee80211_ampdu_params *params,
			 bool enable)
{
	struct mt792x_sta *msta = (struct mt792x_sta *)params->sta->drv_priv;

	if (enable && !params->amsdu)
		msta->deflink.wcid.amsdu = false;

	return mt76_connac_mcu_sta_ba(&dev->mt76, &msta->vif->bss_conf.mt76, params,
				      MCU_UNI_CMD(STA_REC_UPDATE),
				      enable, true);
}

int mt7921_mcu_uni_rx_ba(struct mt792x_dev *dev,
			 struct ieee80211_ampdu_params *params,
			 bool enable)
{
	struct mt792x_sta *msta = (struct mt792x_sta *)params->sta->drv_priv;

	return mt76_connac_mcu_sta_ba(&dev->mt76, &msta->vif->bss_conf.mt76, params,
				      MCU_UNI_CMD(STA_REC_UPDATE),
				      enable, false);
}

static int mt7921_load_clc(struct mt792x_dev *dev, const char *fw_name)
{
	const struct mt76_connac2_fw_trailer *hdr;
	const struct mt76_connac2_fw_region *region;
	const struct mt7921_clc *clc;
	struct mt76_dev *mdev = &dev->mt76;
	struct mt792x_phy *phy = &dev->phy;
	const struct firmware *fw;
	size_t clc_len, fw_data_len, len, offset = 0;
	int ret, i;
	u8 *clc_base = NULL, hw_encap = 0;

	dev->phy.clc_chan_conf = 0xff;
	dev->regd_user = false;
	if (!mt7921_regd_clc_supported(dev))
		return 0;

	if (mt76_is_mmio(&dev->mt76) && !is_mt7932(&dev->mt76)) {
		ret = mt7921_mcu_read_eeprom(dev, MT_EE_HW_TYPE, &hw_encap);
		if (ret)
			return ret;
		hw_encap = u8_get_bits(hw_encap, MT_EE_HW_TYPE_ENCAP);
	}
	if (is_mt7932(&dev->mt76))
		dev_info(dev->mt76.dev,
			 "J700_MT7932_CLC_EEPROM_QUERY_SKIP: generic-extra-command=0 trace-ordinal-alignment=1\n");

	ret = request_firmware(&fw, fw_name, mdev->dev);
	if (ret)
		return ret;

	if (!fw || !fw->data || fw->size < sizeof(*hdr)) {
		dev_err(mdev->dev, "Invalid firmware\n");
		ret = -EINVAL;
		goto out;
	}

	hdr = (const void *)(fw->data + fw->size - sizeof(*hdr));
	if (hdr->n_region > (fw->size - sizeof(*hdr)) / sizeof(*region)) {
		dev_err(mdev->dev, "Invalid firmware region table\n");
		ret = -EINVAL;
		goto out;
	}
	fw_data_len = fw->size - sizeof(*hdr) -
		      hdr->n_region * sizeof(*region);

	for (i = 0; i < hdr->n_region; i++) {
		region = (const void *)((const u8 *)hdr -
					(hdr->n_region - i) * sizeof(*region));
		len = le32_to_cpu(region->len);

		/* check if we have valid buffer size */
		if (len > fw_data_len - offset) {
			dev_err(mdev->dev, "Invalid firmware region\n");
			ret = -EINVAL;
			goto out;
		}

		if ((region->feature_set & FW_FEATURE_NON_DL) &&
		    region->type == FW_TYPE_CLC) {
			clc_base = (u8 *)(fw->data + offset);
			break;
		}
		offset += len;
	}

	if (!clc_base)
		goto out;

	for (offset = 0; offset < len; offset += clc_len) {
		if (len - offset < sizeof(*clc)) {
			dev_err(mdev->dev, "Invalid CLC record\n");
			ret = -EINVAL;
			goto out;
		}

		clc = (const struct mt7921_clc *)(clc_base + offset);
		clc_len = le32_to_cpu(clc->len);
		if (clc_len < sizeof(*clc) || clc_len > len - offset) {
			dev_err(mdev->dev, "Invalid CLC record\n");
			ret = -EINVAL;
			goto out;
		}

		/* Newer firmware may add records this driver does not use yet */
		if (clc->idx >= ARRAY_SIZE(phy->clc))
			continue;

		/* do not init buf again if chip reset triggered */
		if (phy->clc[clc->idx])
			continue;

		/* header content sanity */
		if (clc->idx == MT7921_CLC_POWER &&
		    u8_get_bits(clc->type, MT_EE_HW_TYPE_ENCAP) != hw_encap)
			continue;

		phy->clc[clc->idx] = devm_kmemdup(mdev->dev, clc,
						  clc_len,
						  GFP_KERNEL);

		if (!phy->clc[clc->idx]) {
			ret = -ENOMEM;
			goto out;
		}
	}
	/* AppleSunriseWLAN's MT7932 CLC ABI is three fixed 32-byte CID 0x6b
	 * commands (captured ordinals 84..86), not this generic Connac request
	 * builder.  Keep the parsed rules for Linux regulatory bookkeeping; the
	 * exact firmware commands are emitted by the sealed pre-cal transcript.
	 */
	if (is_mt7932(&dev->mt76))
		ret = 0;
	else
		ret = mt7921_mcu_set_clc(dev, "00", ENVIRON_INDOOR);
out:
	release_firmware(fw);

	return ret;
}

static void mt7921_mcu_parse_tx_resource(struct mt76_dev *dev,
					 struct sk_buff *skb)
{
	struct mt76_sdio *sdio = &dev->sdio;
	struct mt7921_tx_resource {
		__le32 version;
		__le32 pse_data_quota;
		__le32 pse_mcu_quota;
		__le32 ple_data_quota;
		__le32 ple_mcu_quota;
		__le16 pse_page_size;
		__le16 ple_page_size;
		u8 pp_padding;
		u8 pad[3];
	} __packed * tx_res;

	tx_res = (struct mt7921_tx_resource *)skb->data;
	sdio->sched.pse_data_quota = le32_to_cpu(tx_res->pse_data_quota);
	sdio->pse_mcu_quota_max = le32_to_cpu(tx_res->pse_mcu_quota);
	/* The mcu quota usage of this function itself must be taken into consideration */
	sdio->sched.pse_mcu_quota =
		sdio->sched.pse_mcu_quota ? sdio->pse_mcu_quota_max : sdio->pse_mcu_quota_max - 1;
	sdio->sched.ple_data_quota = le32_to_cpu(tx_res->ple_data_quota);
	sdio->sched.pse_page_size = le16_to_cpu(tx_res->pse_page_size);
	sdio->sched.deficit = tx_res->pp_padding;
}

static void mt7921_mcu_parse_phy_cap(struct mt76_dev *dev,
				     struct sk_buff *skb)
{
	struct mt7921_phy_cap {
		u8 ht;
		u8 vht;
		u8 _5g;
		u8 max_bw;
		u8 nss;
		u8 dbdc;
		u8 tx_ldpc;
		u8 rx_ldpc;
		u8 tx_stbc;
		u8 rx_stbc;
		u8 hw_path;
		u8 he;
	} __packed * cap;

	enum {
		WF0_24G,
		WF0_5G
	};

	cap = (struct mt7921_phy_cap *)skb->data;

	dev->phy.antenna_mask = BIT(cap->nss) - 1;
	dev->phy.chainmask = dev->phy.antenna_mask;
	dev->phy.cap.has_2ghz = cap->hw_path & BIT(WF0_24G);
	dev->phy.cap.has_5ghz = cap->hw_path & BIT(WF0_5G);
}

static int mt7921_mcu_get_nic_capability(struct mt792x_phy *mphy)
{
	struct mt76_connac_cap_hdr {
		__le16 n_element;
		u8 rsv[2];
	} __packed * hdr;
	struct sk_buff *skb;
	struct mt76_phy *phy = mphy->mt76;
	int ret, i;

	ret = mt76_mcu_send_and_get_msg(phy->dev,
					is_mt7932(phy->dev) ?
					MCU_CE_QUERY(GET_NIC_CAPAB) :
					MCU_CE_CMD(GET_NIC_CAPAB),
					NULL, 0, true, &skb);
	if (ret)
		return ret;

	hdr = (struct mt76_connac_cap_hdr *)skb->data;
	if (skb->len < sizeof(*hdr)) {
		ret = -EINVAL;
		goto out;
	}

	skb_pull(skb, sizeof(*hdr));

	for (i = 0; i < le16_to_cpu(hdr->n_element); i++) {
		struct tlv_hdr {
			__le32 type;
			__le32 len;
		} __packed * tlv = (struct tlv_hdr *)skb->data;
		int len;

		if (skb->len < sizeof(*tlv))
			break;

		skb_pull(skb, sizeof(*tlv));

		len = le32_to_cpu(tlv->len);
		if (skb->len < len)
			break;
		if (is_mt7932(phy->dev))
			dev_info(phy->dev->dev,
				 "J700_MT7932_NIC_CAP_TLV: index=%d type=%u len=%d\n",
				 i, le32_to_cpu(tlv->type), len);

		switch (le32_to_cpu(tlv->type)) {
		case MT_NIC_CAP_SINGLE_SKU:
			mphy->has_single_sku = true;
			break;
		case MT_NIC_CAP_6G:
			phy->cap.has_6ghz = skb->data[0];
			break;
		case MT_NIC_CAP_MAC_ADDR:
			memcpy(phy->macaddr, (void *)skb->data, ETH_ALEN);
			break;
		case MT_NIC_CAP_PHY:
			mt7921_mcu_parse_phy_cap(phy->dev, skb);
			break;
		case MT_NIC_CAP_TX_RESOURCE:
			if (mt76_is_sdio(phy->dev))
				mt7921_mcu_parse_tx_resource(phy->dev,
							     skb);
			break;
		case MT_NIC_CAP_CHIP_CAP:
			memcpy(&mphy->chip_cap, (void *)skb->data, sizeof(u64));
			break;
		default:
			break;
		}
		skb_pull(skb, len);
	}
out:
	dev_kfree_skb(skb);

	return ret;
}

int mt7921_mcu_fw_log_2_host(struct mt792x_dev *dev, u8 ctrl)
{
	struct {
		u8 ctrl_val;
		u8 pad[3];
	} data = {
		.ctrl_val = ctrl
	};

	return mt76_mcu_send_msg(&dev->mt76, MCU_CE_CMD(FWLOG_2_HOST),
				 &data, sizeof(data), false);
}

#define MT7932_CMD_SET_SW_DRBG	0xe2
static int mt7932_mcu_start_runtime(struct mt792x_dev *dev)
{
	struct {
		u8 mode;
		u8 reserved[3];
		u8 entropy[64];
	} __packed req = {};
	int err;

	/* AppleSunriseWLAN 25G83 wlanAdapterStart calls wlanSetSwDrbgCmd(0, 1)
	 * immediately after firmware ready.  Its exact 68-byte payload is a
	 * zero mode byte, three reserved bytes and 64 bytes from
	 * random_get_bytes().  The SET command deliberately requests no reply.
	 */
	get_random_bytes(req.entropy, sizeof(req.entropy));
	err = mt76_mcu_send_msg(&dev->mt76,
				 FIELD_PREP(__MCU_CMD_FIELD_ID,
					    MT7932_CMD_SET_SW_DRBG),
				 &req, sizeof(req), false);
	if (err)
		return err;
	dev_info(dev->mt76.dev,
		 "J700_MT7932_SW_DRBG_PASS: cid=0xe2 bytes=68 mode=0 response=none\n");

	/* wlanSendDummyCmd(adapter, 1) is the next unconditional call before
	 * NIC capability.  CID 0 selects the firmware packet format in the
	 * MT7932 TXD builder and carries no payload or response.
	 */
	err = mt76_mcu_send_msg(&dev->mt76, 0, NULL, 0, false);
	if (!err)
		dev_info(dev->mt76.dev,
			 "J700_MT7932_DUMMY_PASS: cid=0x00 bytes=0 response=none\n");

	return err;
}

#define MT7932_RUNTIME_EXT_CID		0xea
#define MT7932_RUNTIME_EXT_MLME_CONFIG	0x4e

static int mt7932_mcu_runtime_config(struct mt792x_dev *dev)
{
	/* wlanUpdateBasicConfig() in AppleSunriseWLAN 25G83 emits legacy CID
	 * 0x02, SET/no-response, with a 24-byte runtime structure.  These are
	 * the defaults loaded by its exact wlanInitFeatureOption call sites:
	 * StaHeRxMMSS=0, StaHeMcsMax=2 and Sta6gBw=3 remain defaults; the exact
	 * 25G83 IZUBA_wifi.cfg overrides Sta5gBw=3 and RxMaxMpduLen=2 and pins
	 * TxMaxAmsduInAmpduLen=0x2000.  AssertPath and AssertLevel both remain
	 * zero; byte 12 is written unconditionally.
	 */
	struct {
		u8 data[24];
	} __packed basic = {
		.data[3] = 3,
		.data[12] = 1,
		.data[14] = 2,
		.data[15] = 3,
		.data[17] = 0x20,
		.data[20] = 2,
	};
	/* The immediately following wlanUpdateMlmeConfig() emits main CID 0xea,
	 * ext CID 0x4e, SET/no-response.  Its bytes are not BSS indices: exact
	 * stores source them from AisJoinTimeout=7, AisJoinRetryCount=4 and
	 * the J700 Nss=2 feature default, followed by one zero byte.
	 */
	struct {
		u8 join_timeout;
		u8 join_retry_count;
		u8 nss;
		u8 reserved;
	} __packed mlme = {
		.join_timeout = 7,
		.join_retry_count = 4,
		.nss = 2,
	};
	int cmd, err;

	err = mt76_mcu_send_msg(&dev->mt76, MCU_CE_CMD(BASIC_CONFIG),
				 &basic, sizeof(basic), false);
	if (err)
		return err;
	dev_info(dev->mt76.dev,
		 "J700_MT7932_BASIC_CONFIG_PASS: cid=0x02 bytes=24 set=1 response=none sta5g=3 he_mcs=2 sta6g=3 amsdu=0x2000 rx_mpdu=2\n");

	cmd = FIELD_PREP(__MCU_CMD_FIELD_ID, MT7932_RUNTIME_EXT_CID) |
	      FIELD_PREP(__MCU_CMD_FIELD_EXT_ID,
			 MT7932_RUNTIME_EXT_MLME_CONFIG);
	err = mt76_mcu_send_msg(&dev->mt76, cmd, &mlme, sizeof(mlme), false);
	if (!err)
		dev_info(dev->mt76.dev,
			 "J700_MT7932_MLME_CONFIG_PASS: cid=0xea ext=0x4e bytes=4 join_timeout=7 retries=4 nss=2 set=1 response=none\n");

	return err;
}

int mt7932_mcu_init_dbdc(struct mt792x_dev *dev)
{
	/* AppleSunriseWLAN 25G83 calls cnmInitDbdcSetting() from
	 * wlanOnPreNetRegister(), immediately before it sends channel-domain
	 * information.  DbdcMode defaults to dynamic (2), so the first update
	 * disables DBDC through the legacy main CID 0x28 ABI while selecting
	 * command version 2.  The firmware uses this to initialize the CNM/WMM
	 * state consumed by the later scan channel-request path.
	 */
	struct {
		u8 enable;
		u8 wmm_bitmap;
		u8 update_next_ch_req;
		u8 pad0;
		u8 cmd_ver;
		u8 aa_mode;
		__le16 cmd_len;
		u8 primary_channel;
		u8 wmm_queue;
		u8 rf_band;
		u8 pad1;
		u8 reserved[24];
	} __packed req = {
		.cmd_ver = 2,
		.cmd_len = cpu_to_le16(36),
	};
	int err;

	BUILD_BUG_ON(sizeof(req) != 36);
	err = mt76_mcu_send_msg(&dev->mt76, MCU_CE_CMD(SET_DBDC_PARMS),
				 &req, sizeof(req), false);
	if (!err)
		dev_info(dev->mt76.dev,
			 "J700_MT7932_DBDC_INIT_PASS: cid=0x28 bytes=36 enable=0 wmm=0 update=0 version=2 aa=0 response=none\n");

	return err;
}
EXPORT_SYMBOL_GPL(mt7932_mcu_init_dbdc);

int mt7932_mcu_set_6g_scan(struct mt792x_dev *dev, bool enable)
{
	u8 req = enable;
	int err;

	/* AppleSunriseWLAN 25G83 writes one to adapter+0x19c08f and sends
	 * rlm6gChannelScanSet() immediately after channel-domain setup.  This is
	 * firmware scan-policy state, independent of whether Linux advertises the
	 * 6 GHz band while its target-specific power-record ABI is unfinished.
	 */
	err = mt76_mcu_send_msg(&dev->mt76, MCU_CE_CMD(SET_6G_SCAN),
				 &req, sizeof(req), false);
	if (!err)
		dev_info(dev->mt76.dev,
			 "J700_MT7932_6G_SCAN_POLICY_PASS: cid=0xd7 enable=%u bytes=1 response=none\n",
			 enable);

	return err;
}
EXPORT_SYMBOL_GPL(mt7932_mcu_set_6g_scan);

#define MT7932_CUSTOMER_CFG_NAME	"mediatek/IZUBA_wifi.cfg"
#define MT7932_POWER_TRANSCRIPT_NAME \
	"mediatek/j700-mt7932-au-sp-power-transcript.bin"
MODULE_FIRMWARE(MT7932_POWER_TRANSCRIPT_NAME);
#define MT7932_POWER_TRANSCRIPT_VERSION	1
#define MT7932_POWER_TRANSCRIPT_RECORDS	48
#define MT7932_POWER_TRANSCRIPT_PACKETS	42
#define MT7932_POWER_RECORD_SIZE		52

#define MT7932_LIVE_START_TRANSCRIPT_NAME \
	"mediatek/j700-mt7932-live-start-transcript.bin"
MODULE_FIRMWARE(MT7932_LIVE_START_TRANSCRIPT_NAME);
#define MT7932_LIVE_START_VERSION	1
#define MT7932_LIVE_START_RECORDS	94
#define MT7932_LIVE_START_PHASES	2
#define MT7932_LIVE_START_RECORD_SIZE	48

struct mt7932_live_start_header {
	u8 magic[8];
	__le16 version;
	__le16 record_size;
	__le32 records;
	__le32 phases;
	u8 source_sha256[32];
} __packed;

struct mt7932_live_start_record {
	u8 phase;
	u8 transport;
	u8 cid;
	u8 ext_cid;
	u8 set_query;
	u8 ext_cid_ack;
	__le16 reserved;
	__le32 ordinal;
	__le32 data_len;
	u8 sha256[32];
} __packed;

static const u8 mt7932_live_source_sha256[32] = {
	0x23, 0xfb, 0xbe, 0x28, 0xa3, 0x08, 0x05, 0xcb,
	0xb6, 0x13, 0x53, 0x01, 0x35, 0x5b, 0x48, 0x49,
	0xab, 0x98, 0xc5, 0xd9, 0x91, 0x4d, 0x48, 0xf0,
	0xc9, 0xe4, 0x20, 0x39, 0x39, 0x75, 0xe0, 0x6e,
};

struct mt7932_power_transcript_header {
	u8 magic[8];
	__le16 version;
	__le16 record_size;
	__le32 records;
} __packed;

struct mt7932_power_transcript_record {
	u8 kind;
	u8 transport;
	__le16 flags;
	u8 cid;
	u8 ext_cid;
	u8 band;
	u8 reserved;
	__le32 order;
	__le32 name_len;
	__le32 data_len;
	u8 sha256[32];
} __packed;

static int
mt7932_validate_live_start_transcript(const struct firmware *fw)
{
	static const u8 magic[8] = { 'J', '7', 'C', 'M', 'D', 'T', 'R', '1' };
	static const u8 prefix_transport[] = { 1, 1, 1, 0, 1, 2 };
	static const u8 prefix_cid[] = { 0xed, 0xed, 0xed, 0xbf, 0xed, 0xea };
	static const u8 prefix_ext_cid[] = {
		0x81, 0x21, 0xa9, 0x00, 0xc1, 0x51,
	};
	static const u8 prefix_set_query[] = { 1, 0, 0, 0, 0, 1 };
	static const u8 prefix_ack[] = { 0, 1, 1, 1, 1, 0 };
	const struct mt7932_live_start_header *header;
	u32 phase_count[MT7932_LIVE_START_PHASES + 1] = {};
	size_t offset;
	u32 count, i;

	BUILD_BUG_ON(sizeof(struct mt7932_live_start_header) != 52);
	BUILD_BUG_ON(sizeof(struct mt7932_live_start_record) !=
		     MT7932_LIVE_START_RECORD_SIZE);
	if (fw->size < sizeof(*header))
		return -EINVAL;
	header = (const void *)fw->data;
	count = le32_to_cpu(header->records);
	if (memcmp(header->magic, magic, sizeof(magic)) ||
	    le16_to_cpu(header->version) != MT7932_LIVE_START_VERSION ||
	    le16_to_cpu(header->record_size) != MT7932_LIVE_START_RECORD_SIZE ||
	    count != MT7932_LIVE_START_RECORDS ||
	    le32_to_cpu(header->phases) != MT7932_LIVE_START_PHASES ||
	    memcmp(header->source_sha256, mt7932_live_source_sha256,
		   sizeof(mt7932_live_source_sha256)))
		return -EINVAL;

	offset = sizeof(*header);
	for (i = 0; i < count; i++) {
		const struct mt7932_live_start_record *record;
		const u8 *data;
		u32 ordinal, data_len, index;
		u8 expected_transport = 0, expected_cid, expected_ext_cid = 0;
		u8 expected_set_query = 1, expected_ack = 0;

		if (sizeof(*record) > fw->size - offset)
			return -EINVAL;
		record = (const void *)(fw->data + offset);
		offset += sizeof(*record);
		data_len = le32_to_cpu(record->data_len);
		ordinal = le32_to_cpu(record->ordinal);
		if ((!data_len && ordinal != 91) || data_len > 2048 ||
		    data_len > fw->size - offset ||
		    record->phase < MT7932_LIVE_PHASE_PRE_CAL ||
		    record->phase > MT7932_LIVE_PHASE_AU_UPDATE ||
		    record->transport > 2 ||
		    le16_to_cpu(record->reserved))
			return -EINVAL;
		data = fw->data + offset;
		index = phase_count[record->phase]++;

		switch (record->phase) {
		case MT7932_LIVE_PHASE_PRE_CAL:
			if (ordinal != 84 + index || index >= 51)
				return -EINVAL;
			if (index <= 2) {
				expected_cid = 107;
			} else if (ordinal >= 87 && ordinal <= 92) {
				u32 prefix = ordinal - 87;

				expected_transport = prefix_transport[prefix];
				expected_cid = prefix_cid[prefix];
				expected_ext_cid = prefix_ext_cid[prefix];
				expected_set_query = prefix_set_query[prefix];
				expected_ack = prefix_ack[prefix];
			} else if (index == 9) {
				expected_cid = 40;
			} else if (index <= 11) {
				expected_cid = 202;
			} else if (index == 12) {
				expected_cid = 15;
			} else if (index <= 25) {
				expected_cid = 93;
			} else if (index == 26) {
				expected_cid = 90;
			} else if (index <= 44) {
				expected_cid = 112;
			} else if (index == 45) {
				expected_cid = 146;
			} else if (index == 46) {
				expected_cid = 15;
			} else if (index == 47) {
				expected_cid = 215;
			} else if (index <= 49) {
				expected_cid = 155;
			} else {
				expected_cid = 197;
			}
			break;
		case MT7932_LIVE_PHASE_AU_UPDATE:
			if (ordinal != 601 + index || index >= 43)
				return -EINVAL;
			if (!index)
				expected_cid = 15;
			else if (index <= 28)
				expected_cid = 93;
			else
				expected_cid = 149;
			break;
		default:
			return -EINVAL;
		}
		if (record->transport != expected_transport ||
		    record->cid != expected_cid ||
		    record->ext_cid != expected_ext_cid ||
		    record->set_query != expected_set_query ||
		    record->ext_cid_ack != expected_ack)
			return -EINVAL;
		if ((ordinal == 96 || ordinal == 130) &&
		    (data_len < 2 || memcmp(data, "XZ", 2)))
			return -EINVAL;
		if (ordinal == 601 &&
		    (data_len < 2 || memcmp(data, "AU", 2)))
			return -EINVAL;
		offset += data_len;
	}

	if (offset != fw->size ||
	    phase_count[MT7932_LIVE_PHASE_PRE_CAL] != 51 ||
	    phase_count[MT7932_LIVE_PHASE_AU_UPDATE] != 43)
		return -EINVAL;
	return 0;
}

static int mt7932_mcu_ack_dma_state(struct mt792x_dev *dev, u32 state,
				     const char *name)
{
	u32 value;

	/* Run 191 uses the same STOP_DMA -> RESET_DONE -> RECOVERY_DONE ->
	 * NORMAL_STATE recovery handshake implemented by upstream mt7915/mt7996.
	 * MT792x defines the registers and state bits but has no generic caller.
	 */
	if (!mt76_poll_msec(dev, MT_MCU_CMD, state, state, 500)) {
		value = mt76_rr(dev, MT_MCU_CMD);
		dev_err(dev->mt76.dev,
			"J700_MT7932_RUNTIME_DMA_HANDSHAKE_FAIL: expected=%s bit=0x%08x observed=0x%08x\n",
			name, state, value);
		return -ETIMEDOUT;
	}
	value = mt76_rr(dev, MT_MCU_CMD);
	mt76_wr(dev, MT_MCU_CMD, state);
	dev_info(dev->mt76.dev,
		 "J700_MT7932_RUNTIME_DMA_STATE_ACK: state=%s bit=0x%08x observed=0x%08x\n",
		 name, state, value);

	return 0;
}

static void mt7932_runtime_irq_publish(struct mt792x_dev *dev,
				       const char *phase)
{
	/* Run 191 writes this complete three-register image after STOP_DMA,
	 * DMA_INIT and RESET_DONE, and once more after NORMAL_STATE.  Keep the
	 * Linux IRQ line disabled while reproducing the hardware waveform so the
	 * generic tasklet cannot consume the synchronously-polled MCU state bits.
	 */
	mt76_wr(dev, MT7932_HOST_INT_ENA, MT7932_RUNTIME_HOST_INT_MASK);
	mt76_wr(dev, MT_WFDMA0_HOST_INT_STA, MT7932_RUNTIME_HOST_INT_MASK);
	mt76_wr(dev, MT_MCU2HOST_SW_INT_ENA, MT7932_MCU2HOST_SW_INT_MASK);
	dev_info(dev->mt76.dev,
		 "J700_MT7932_RUNTIME_IRQ_WAVEFORM: phase=%s host=0x%08x status=0x%08x mcu2host=0x%08x source=run191-seq14275-14417\n",
		 phase, mt76_rr(dev, MT7932_HOST_INT_ENA),
		 MT7932_RUNTIME_HOST_INT_MASK,
		 mt76_rr(dev, MT_MCU2HOST_SW_INT_ENA));
}

static u32 mt7932_live_start_gap_us(u32 ordinal)
{
	/* Unperturbed Run-191 CIDX-to-CIDX timing.  The synchronous payload
	 * capture that supplied the command bytes stops the submitting CPU to read
	 * DART memory and therefore adds roughly 46 ms per command; its host
	 * timestamps are not device pacing.  In particular, Run 191 submits
	 * ordinals 93, 94 and 95 as one burst.  Keeping TX17 non-empty across that
	 * boundary is part of the measured firmware transaction.
	 *
	 * Ordinal 87 is followed by the synchronous WFDMA recovery below, whose
	 * state waits account for the measured epoch interval.  Ordinal 134 keeps
	 * the already-proven final phase settle before the caller drains TX17.
	 */
	static const u32 pre_cal[] = {
		/* 84-91 */ 116, 1773, 373, 0, 3563, 5413, 6008, 16927,
		/* 92-99 */ 151, 114, 904, 94573, 218, 126, 316, 117,
		/* 100-107 */ 112, 107, 105, 103, 100, 117, 99, 192,
		/* 108-115 */ 103, 945, 7300, 105, 135, 108, 163, 122,
		/* 116-123 */ 97, 289, 108, 108, 102, 100, 99, 93,
		/* 124-131 */ 103, 101, 102, 162, 1091, 1037, 101, 1567,
		/* 132-134 */ 2133, 1874, 76000,
	};
	static const u32 au_update[] = {
		/* 601-608 */ 14503, 3414, 122118, 140033, 169676, 382,
			      45467, 43,
		/* 609-616 */ 66, 66042, 142683, 40485, 3827, 2403, 3482,
			      4965,
		/* 617-624 */ 146731, 402079, 192525, 160, 119, 273453,
			      3389, 3810,
		/* 625-632 */ 2948, 5114, 6077, 423438, 11446, 15427, 3997,
			      2347,
		/* 633-640 */ 11819, 4001, 4192, 8084, 64602, 3715, 4912,
			      61034,
		/* 641-643 */ 1751, 2681, 4467,
	};

	if (ordinal >= 84 && ordinal <= 134)
		return pre_cal[ordinal - 84];
	if (ordinal >= 601 && ordinal <= 643)
		return au_update[ordinal - 601];

	return 0;
}

static void mt7932_live_start_wait(u32 ordinal)
{
	u32 delay = mt7932_live_start_gap_us(ordinal);

	if (!delay)
		return;
	if (delay <= 2000)
		udelay(delay);
	else
		usleep_range(delay, delay + 200);
}

static u8 mt7932_live_start_irq_cycles(u32 ordinal)
{
	/* Number of complete HOST_INT_ENA=0 / HOST_INT_STA acknowledge /
	 * HOST_INT_ENA restore / MCU2HOST enable cycles measured between this
	 * command's CIDX write and the next command in Run 191.  Ordinal 87 is
	 * excluded because the explicit WFDMA recovery handshake owns that window.
	 * These are needed only during pre-calibration, before mac80211 publishes
	 * the device and the normal IRQ path is allowed to run.
	 */
	static const u8 pre_cal[] = {
		/* 84-91 */ 0, 0, 0, 0, 2, 2, 2, 2,
		/* 92-99 */ 0, 0, 2, 2, 0, 0, 0, 0,
		/* 100-107 */ 0, 0, 0, 0, 0, 0, 0, 0,
		/* 108-115 */ 0, 2, 2, 0, 0, 0, 0, 0,
		/* 116-123 */ 0, 0, 0, 0, 0, 0, 0, 0,
		/* 124-131 */ 0, 0, 0, 0, 3, 1, 0, 1,
		/* 132-134 */ 2, 1, 0,
	};

	if (ordinal < 84 || ordinal > 134)
		return 0;

	return pre_cal[ordinal - 84];
}

static void mt7932_live_start_irq_service(struct mt792x_dev *dev,
					   u32 ordinal, u8 first, u8 count)
{
	struct mt76_dev *mdev = &dev->mt76;
	struct mt76_queue *q = mdev->q_mcu[MT_MCUQ_WM];
	u32 queued_before = q ? READ_ONCE(q->queued) : 0;
	u8 i;

	for (i = 0; i < count; i++) {
		mt76_set_irq_mask(mdev, MT7932_HOST_INT_ENA,
				  U32_MAX, 0);
		/* Run 191's interrupt handler calls halWpdmaProcessCmdDmaDone()
		 * while HOST_INT_ENA is zero and before acknowledging status.  Those
		 * DDONE reads and descriptor clears are DART memory traffic, so they
		 * are absent from the MMIO event list but are required queue work—not
		 * an optional host bookkeeping step.
		 */
		if (q && mdev->queue_ops->tx_cleanup)
			mdev->queue_ops->tx_cleanup(mdev, q, false);
		mt76_wr(dev, MT_WFDMA0_HOST_INT_STA,
			MT7932_RUNTIME_HOST_INT_MASK);
		mt76_set_irq_mask(mdev, MT7932_HOST_INT_ENA,
				  0, MT7932_RUNTIME_HOST_INT_MASK);
		mt76_wr(dev, MT_MCU2HOST_SW_INT_ENA,
			MT7932_MCU2HOST_SW_INT_MASK);
		if (i + 1 < count)
			udelay(50);
	}
	dev_info(dev->mt76.dev,
		 "J700_MT7932_PRE_REGISTER_IRQ_SERVICE: ordinal=%u cycles=%u-%u queued=%u->%u head=%u tail=%u host_mask=0x%08x mcu2host=0x%08x source=run191-mmio+ddone\n",
		 ordinal, first, first + count - 1,
		 queued_before, q ? READ_ONCE(q->queued) : 0,
		 q ? q->head : 0, q ? q->tail : 0,
		 MT7932_RUNTIME_HOST_INT_MASK,
		 MT7932_MCU2HOST_SW_INT_MASK);
}

int mt7932_mcu_load_live_start_phase(struct mt792x_dev *dev, u8 phase)
{
	static const char * const names[] = {
		[MT7932_LIVE_PHASE_PRE_CAL] = "pre-cal",
		[MT7932_LIVE_PHASE_AU_UPDATE] = "au-update",
	};
	const struct mt7932_live_start_header *header;
	const struct firmware *fw;
	size_t offset;
	u32 count, i, queued = 0;
	bool runtime_irq_disabled = false;
	int ret;

	if (!is_mt7932(&dev->mt76) ||
	    phase < MT7932_LIVE_PHASE_PRE_CAL ||
	    phase > MT7932_LIVE_PHASE_AU_UPDATE)
		return -EINVAL;
	ret = request_firmware(&fw, MT7932_LIVE_START_TRANSCRIPT_NAME,
			       dev->mt76.dev);
	if (ret)
		return ret;
	ret = mt7932_validate_live_start_transcript(fw);
	if (ret) {
		dev_err(dev->mt76.dev,
			"J700_MT7932_LIVE_TRANSCRIPT_REJECT: source=%s bytes=%zu ret=%d\n",
			MT7932_LIVE_START_TRANSCRIPT_NAME, fw->size, ret);
		goto release_firmware;
	}

	header = (const void *)fw->data;
	count = le32_to_cpu(header->records);
	offset = sizeof(*header);
	for (i = 0; i < count; i++) {
		const struct mt7932_live_start_record *record;
		const u8 *data;
		u32 data_len, ordinal, gap_us;
		u8 irq_cycles;
		int cmd;

		record = (const void *)(fw->data + offset);
		offset += sizeof(*record);
		data_len = le32_to_cpu(record->data_len);
		ordinal = le32_to_cpu(record->ordinal);
		gap_us = mt7932_live_start_gap_us(ordinal);
		irq_cycles = mt7932_live_start_irq_cycles(ordinal);
		data = fw->data + offset;
		offset += data_len;
		if (record->phase != phase)
			continue;
		switch (record->transport) {
		case 0:
			cmd = __MCU_CMD_FIELD_CE |
			      FIELD_PREP(__MCU_CMD_FIELD_ID, record->cid);
			break;
		case 1:
			cmd = FIELD_PREP(__MCU_CMD_FIELD_ID, record->cid) |
			      FIELD_PREP(__MCU_CMD_FIELD_EXT_ID,
					 record->ext_cid);
			break;
		case 2:
			cmd = __MCU_CMD_FIELD_UNI |
			      FIELD_PREP(__MCU_CMD_FIELD_ID, record->ext_cid);
			break;
		default:
			ret = -EINVAL;
			goto release_firmware;
		}
		if (!record->set_query)
			cmd |= __MCU_CMD_FIELD_QUERY;
		/* Apple's captured ext_cid_ack bit requests a firmware event; it
		 * does not serialize the caller at this point in the multi-command
		 * transaction.  Keep the wire ACK while allowing the following
		 * measured command to be submitted on schedule.
		 */
		if (record->ext_cid_ack)
			cmd |= __MCU_CMD_FIELD_ACK;
		if (ordinal == 87) {
			/* Firmware requests the measured WFDMA recovery immediately after
			 * this mode-3 command.  Keep mt792x_irq_tasklet() from acknowledging
			 * the MCU state bits before the synchronous Run-191 handshake below
			 * observes and orders them.
			 */
			mt76_set_irq_mask(&dev->mt76, MT7932_HOST_INT_ENA,
					  MT_INT_MCU_CMD, 0);
			disable_irq(to_pci_dev(dev->mt76.dev)->irq);
			runtime_irq_disabled = true;
			dev_info(dev->mt76.dev,
				 "J700_MT7932_RUNTIME_DMA_HANDSHAKE_ARMED: before=ordinal87 mcu_cmd_irq=software-masked irq-line=disabled\n");
		}
		ret = mt76_mcu_send_msg(&dev->mt76, cmd, data, data_len, false);
		if (ret) {
			if (phase == MT7932_LIVE_PHASE_PRE_CAL && ret == -ENOMEM) {
				int drain_ret;

				drain_ret = mt7932_mcu_drain_tx17(
					dev, "live-enqueue-fail", false);
				dev_err(dev->mt76.dev,
					"J700_MT7932_LIVE_ENQUEUE_DIAGNOSTIC: ordinal=%u enqueue_ret=%d drain_ret=%d\n",
					ordinal, ret, drain_ret);
			}
			goto release_firmware;
		}
		queued++;
		dev_info(dev->mt76.dev,
			 "J700_MT7932_LIVE_COMMAND_QUEUED: phase=%s item=%u ordinal=%u transport=%u cid=0x%02x ext=0x%02x set=%u ack=%u bytes=%u response=%s next_gap_us=%u timing=run191-cidx\n",
			 names[phase], queued, ordinal, record->transport,
			 record->cid, record->ext_cid, record->set_query,
			 record->ext_cid_ack, data_len,
			 record->ext_cid_ack ? "requested-async" : "none",
			 gap_us);
		if (irq_cycles)
			mt7932_live_start_irq_service(dev, ordinal, 1, 1);
		mt7932_live_start_wait(ordinal);
		if (irq_cycles > 1)
			mt7932_live_start_irq_service(dev, ordinal, 2,
						   irq_cycles - 1);
		if (ordinal == 87) {
			struct mt76_queue *q =
				dev->mt76.q_mcu[MT_MCUQ_WM];
			u32 cidx, didx, ext0, glo, page, pktmax;

			/* The full Run-191 MMIO trace measures a WFDMA epoch boundary
			 * between mode-3 ordinal 87 and query ordinal 88: Apple drains the
			 * command, disables WFDMA, resets the published rings (including
			 * MT_WFDMA0_RST_DTX_PTR = ~0), restores WFDMA and submits query 88
			 * at TX17 slot zero.  The sealed command capture independently
			 * records the intervening CIDX reset and query 88 at slot zero.
			 *
			 * Cycle 501 omitted this transition.  Query 88 was therefore left
			 * live in pre-runtime slot 15 and the 24-entry ring filled at
			 * ordinal 111.  Use mt792x's existing non-WFSYS WPDMA reset path,
			 * but only after the measured command has retired; this preserves
			 * firmware state while beginning the runtime ring epoch exactly at
			 * CIDX/DIDX zero.
			 */
			ret = mt7932_mcu_drain_tx17(dev,
						 "live-before-query88", false);
			if (ret)
				goto release_firmware;
			if (!q) {
				ret = -ENODEV;
				goto release_firmware;
			}
			cidx = readl(&q->regs->cpu_idx);
			didx = readl(&q->regs->dma_idx);
			dev_info(dev->mt76.dev,
				 "J700_MT7932_RUNTIME_DMA_EPOCH_BEGIN: after=ordinal87 before_cidx=%u before_didx=%u head=%u tail=%u queued=%u source=run191-full-mmio\n",
				 cidx, didx, q->head, q->tail,
				 READ_ONCE(q->queued));
			mt76_set_irq_mask(&dev->mt76, MT7932_HOST_INT_ENA,
					  U32_MAX, 0);
			mt76_wr(dev, MT_WFDMA0_HOST_INT_STA, 0);
			ret = mt7932_mcu_ack_dma_state(dev, MT_MCU_CMD_STOP_DMA,
						       "stop-dma");
			if (ret)
				goto release_firmware;
			mt76_wr(dev, MT7932_MCU_INT_EVENT,
				MT_MCU_INT_EVENT_DMA_STOPPED);
			mt7932_runtime_irq_publish(dev, "after-stop-dma");
			ret = mt792xe_mcu_fw_pmctrl(dev);
			if (ret)
				goto release_firmware;
			mt76_wr(dev, MT_WFDMA0_HOST_INT_STA, 0);
			ret = __mt792xe_mcu_drv_pmctrl(dev);
			if (ret)
				goto release_firmware;
			dev_info(dev->mt76.dev,
				 "J700_MT7932_RUNTIME_DMA_OWNERSHIP_PASS: sequence=firmware-own,driver-own lpctl=0x%08x source=run191-full-mmio\n",
				 mt76_rr(dev, MT_CONN_ON_LPCTL));
			ret = mt7932_mcu_ack_dma_state(dev, MT_MCU_CMD_RESET_DONE,
						       "reset-done");
			if (ret)
				goto release_firmware;
			/* Run 191 records no EXT0 access and Run 541 reads selector=1 at
			 * the successful runtime boundary.  Cycles 550/551 proved the current
			 * full-epoch path already reaches this boundary with 0x28c004df.
			 * Require that value and avoid even a same-value RMW: final readback
			 * cannot prove that an untraced write was free of internal side effects.
			 */
			ext0 = mt76_rr(dev, MT_WFDMA0_GLO_CFG_EXT0);
			if (ext0 != 0x28c004df) {
				dev_err(dev->mt76.dev,
					"J700_MT7932_RUNTIME_DMASHDL_SELECTOR_FAIL: phase=pre-reset ext0=0x%08x expected=0x28c004df no-runtime-write=1\n",
					ext0);
				ret = -EIO;
				goto release_firmware;
			}
			dev_info(dev->mt76.dev,
				 "J700_MT7932_RUNTIME_DMASHDL_SELECTOR_ARMED: before=0x%08x after=0x28c004df action=preserve-no-write phase=before-wpdma-reset source=run191+run541\n",
				 ext0);
			/* AppleSunriseWLAN 25G83 halHwRecoveryFromError calls the
			 * mt7961DmashdlReInit bus callback at bus_info+0x2a0 before
			 * halWpdmaAllocRing and halWpdmaInitRing.  The latter performs the
			 * WFDMA disable, TX/RX ring publication, manual prefetch, RST_DTX
			 * and re-enable sequence.  Applying the same final values after
			 * RST_DTX matches a paused register snapshot but seeds Connac3's
			 * hidden q-status epoch in the wrong order.
			 *
			 * The callback does not merely repair fields whose readback drifted:
			 * exact static RE shows it republishes packet/page policy, all queue
			 * mappings and both user-priority words on every recovery.  Same-value
			 * writes are therefore part of the scheduler epoch contract even when
			 * a later paused snapshot converges.  Quota/refill remain outside ReInit
			 * and are deliberately untouched.
			 */
			page = mt76_rr(dev, MT_DMASHDL_PAGE);
			pktmax = mt76_rr(dev, MT_DMASHDL_PKT_MAX_SIZE);
			mt76_rmw(dev, MT_DMASHDL_PAGE, GENMASK(17, 16), BIT(16));
			mt76_rmw(dev, MT_DMASHDL_PKT_MAX_SIZE,
				 MT_DMASHDL_PKT_MAX_SIZE_PLE |
				 MT_DMASHDL_PKT_MAX_SIZE_PSE,
				 FIELD_PREP(MT_DMASHDL_PKT_MAX_SIZE_PLE, 1));
			mt76_wr(dev, MT_DMASHDL_Q_MAP(0), 0x76543210);
			mt76_wr(dev, MT_DMASHDL_Q_MAP(1), 0xfedcba98);
			mt76_wr(dev, MT_DMASHDL_Q_MAP(2), 0x11111000);
			mt76_wr(dev, MT_DMASHDL_Q_MAP(3), 0x11111000);
			mt76_wr(dev, MT_DMASHDL_SCHED_SET(0), 0x76543210);
			mt76_wr(dev, MT_DMASHDL_SCHED_SET(1), 0xfedcba98);
			if (mt76_rr(dev, MT_DMASHDL_PAGE) != 0x003d1000 ||
			    mt76_rr(dev, MT_DMASHDL_PKT_MAX_SIZE) != 0x00000001 ||
			    mt76_rr(dev, MT_DMASHDL_Q_MAP(0)) != 0x76543210 ||
			    mt76_rr(dev, MT_DMASHDL_Q_MAP(1)) != 0xfedcba98 ||
			    mt76_rr(dev, MT_DMASHDL_Q_MAP(2)) != 0x11111000 ||
			    mt76_rr(dev, MT_DMASHDL_Q_MAP(3)) != 0x11111000 ||
			    mt76_rr(dev, MT_DMASHDL_SCHED_SET(0)) != 0x76543210 ||
			    mt76_rr(dev, MT_DMASHDL_SCHED_SET(1)) != 0xfedcba98) {
				dev_err(dev->mt76.dev,
					"J700_MT7932_RUNTIME_DMASHDL_REINIT_FAIL: phase=pre-wpdma-reset page_before=0x%08x pkt_before=0x%08x page_after=0x%08x pkt_after=0x%08x qmap=%08x/%08x/%08x/%08x pri=%08x/%08x\n",
					page, pktmax,
					mt76_rr(dev, MT_DMASHDL_PAGE),
					mt76_rr(dev, MT_DMASHDL_PKT_MAX_SIZE),
					mt76_rr(dev, MT_DMASHDL_Q_MAP(0)),
					mt76_rr(dev, MT_DMASHDL_Q_MAP(1)),
					mt76_rr(dev, MT_DMASHDL_Q_MAP(2)),
					mt76_rr(dev, MT_DMASHDL_Q_MAP(3)),
					mt76_rr(dev, MT_DMASHDL_SCHED_SET(0)),
					mt76_rr(dev, MT_DMASHDL_SCHED_SET(1)));
				ret = -EIO;
				goto release_firmware;
			}
			dev_info(dev->mt76.dev,
				 "J700_MT7932_RUNTIME_DMASHDL_REINIT_PASS: phase=pre-wpdma-reset page_before=0x%08x pkt_before=0x%08x page_after=0x003d1000 pkt_after=0x00000001 qmap=76543210/fedcba98/11111000/11111000 pri=76543210/fedcba98 writes=page,pktmax,qmap0-3,pri0-1 source=apple25G83-mt7961DmashdlReInit+run543\n",
				 page, pktmax);
			dev->mt7932_runtime_dma_epoch = true;
			ret = mt792x_wpdma_reset(dev, false);
			dev->mt7932_runtime_dma_epoch = false;
			if (ret) {
				dev_err(dev->mt76.dev,
					"J700_MT7932_RUNTIME_DMA_EPOCH_FAIL: ret=%d\n",
					ret);
				goto release_firmware;
			}
			/* Run 191 keeps the full hardware mask published across ring reset.
			 * The Linux IRQ line remains disabled and the mt76 software mask stays
			 * zero until NORMAL_STATE, preventing the generic tasklet race without
			 * changing the device-visible register sequence.
			 */
			dev_info(dev->mt76.dev,
				 "J700_MT7932_RUNTIME_IRQ_READBACK: phase=post-wpdma-reset host_int=0x%08x source=cycle552\n",
				 mt76_rr(dev, MT7932_HOST_INT_ENA));
			cidx = readl(&q->regs->cpu_idx);
			didx = readl(&q->regs->dma_idx);
			if (cidx || didx || q->queued) {
				dev_err(dev->mt76.dev,
					"J700_MT7932_RUNTIME_DMA_EPOCH_FAIL: cidx=%u didx=%u head=%u tail=%u queued=%u\n",
					cidx, didx, q->head, q->tail,
					READ_ONCE(q->queued));
				ret = -EIO;
				goto release_firmware;
			}
			/* MT7932 resets hardware DIDX only when dma_enable() writes
			 * RST_DTX_PTR, after generic reset_q() has already sampled the old
			 * read-only DIDX into head/tail.  Cycle 502 measured hardware 0/0
			 * with software 15/15.  No descriptor is live here, so synchronize
			 * the two software cursors to the now-proven hardware epoch before
			 * allowing query 88 to enqueue.
			 */
			if (q->head || q->tail) {
				dev_info(dev->mt76.dev,
					 "J700_MT7932_RUNTIME_DMA_SOFTWARE_RESYNC: old_head=%u old_tail=%u new_head=0 new_tail=0 queued=0 source=cycle502\n",
					 q->head, q->tail);
				spin_lock_bh(&q->lock);
				q->head = 0;
				spin_unlock_bh(&q->lock);
				spin_lock_bh(&q->cleanup_lock);
				q->tail = 0;
				spin_unlock_bh(&q->cleanup_lock);
			}
			/* mt792x_dma_enable() has already emitted Run 191's only runtime
			 * GLO_CFG writes, 0x5030b870 followed by 0x5030b875.  Do not repeat
			 * a same-value RMW here: Apple proceeds directly to periodic
			 * interrupt policy 0x01fd0032 before notifying firmware that DMA was
			 * reinitialized.  Read back the installed image without mutating it.
			 */
			mt76_wr(dev, MT_WFDMA0_HOST_PER_DLY_INT_CFG, 0x01fd0032);
			glo = mt76_rr(dev, MT_WFDMA0_GLO_CFG);
			if ((glo & ~(MT_WFDMA0_GLO_CFG_TX_DMA_BUSY |
				     MT_WFDMA0_GLO_CFG_RX_DMA_BUSY)) != 0x5030b875 ||
			    mt76_rr(dev, MT_WFDMA0_HOST_PER_DLY_INT_CFG) !=
				0x01fd0032) {
				dev_err(dev->mt76.dev,
					"J700_MT7932_RUNTIME_WFDMA_IMAGE_FAIL: glo=0x%08x host_int=0x%08x per_dly=0x%08x\n",
					glo, mt76_rr(dev, MT7932_HOST_INT_ENA),
					mt76_rr(dev,
						MT_WFDMA0_HOST_PER_DLY_INT_CFG));
				ret = -EIO;
				goto release_firmware;
			}
			dev_info(dev->mt76.dev,
				 "J700_MT7932_RUNTIME_WFDMA_CORE_PASS: glo=0x%08x host_int=0x%08x per_dly=0x%08x irq_publish=after-normal-state source=run191-full-mmio\n",
				 glo, mt76_rr(dev, MT7932_HOST_INT_ENA),
				 mt76_rr(dev, MT_WFDMA0_HOST_PER_DLY_INT_CFG));
			mt76_wr(dev, MT7932_MCU_INT_EVENT,
				MT_MCU_INT_EVENT_DMA_INIT);
			mt7932_runtime_irq_publish(dev, "after-dma-init");
			mt76_wr(dev, MT_WFDMA0_HOST_INT_STA, 0);
			ret = mt7932_mcu_ack_dma_state(dev,
						       MT_MCU_CMD_RECOVERY_DONE,
						       "recovery-done");
			if (ret)
				goto release_firmware;
			mt76_wr(dev, MT7932_MCU_INT_EVENT,
				MT_MCU_INT_EVENT_RESET_DONE);
			mt7932_runtime_irq_publish(dev, "after-reset-done");
			mt76_wr(dev, MT_WFDMA0_HOST_INT_STA, 0);
			ret = mt7932_mcu_ack_dma_state(dev, MT_MCU_CMD_NORMAL_STATE,
						       "normal-state");
			if (ret)
				goto release_firmware;
			mt7932_runtime_irq_publish(dev, "after-normal-state");
			mt76_set_irq_mask(&dev->mt76, 0, 0,
					  MT7932_RUNTIME_HOST_INT_MASK);
			if (mt76_rr(dev, MT7932_HOST_INT_ENA) !=
			    MT7932_RUNTIME_HOST_INT_MASK) {
				dev_err(dev->mt76.dev,
					"J700_MT7932_RUNTIME_IRQ_IMAGE_FAIL: host_int=0x%08x expected=0x%08x\n",
					mt76_rr(dev, MT7932_HOST_INT_ENA),
					MT7932_RUNTIME_HOST_INT_MASK);
				ret = -EIO;
				goto release_firmware;
			}
			dev_info(dev->mt76.dev,
				 "J700_MT7932_RUNTIME_IRQ_IMAGE_PASS: host_int=0x%08x mcu2host=0x%08x publish=stop,dma-init,reset-done,normal-state source=run191-full-mmio\n",
				 mt76_rr(dev, MT7932_HOST_INT_ENA),
				 mt76_rr(dev, MT_MCU2HOST_SW_INT_ENA));
			enable_irq(to_pci_dev(dev->mt76.dev)->irq);
			runtime_irq_disabled = false;
			dev_info(dev->mt76.dev,
				 "J700_MT7932_RUNTIME_DMA_EPOCH_PASS: boundary=ordinal87-to-88 handshake=stop,reset,dma-init,recovery,normal tx17=cidx0,didx0,head0,tail0 descriptors=cleared pre_ring=disable-rmw(read-0x1010b870,write-0x00103870),rings,write-0x00103870 ring_publish=base,cidx,count no_didx_write=1 enable=runtime-disabled-then-live no_download_transient=1 reset_dtx=all dmashdl_selector=1 bounce=monotonic-no-reuse wfsys_reset=0\n");
		}
	}
	dev_info(dev->mt76.dev,
		 "J700_MT7932_LIVE_PHASE_QUEUED: phase=%s commands=%u pacing=trace-46ms-special-87,95,134 drain=phase-boundary source=%s capture_sha256=23fbbe28... target_writes=0\n",
		 names[phase], queued, MT7932_LIVE_START_TRANSCRIPT_NAME);
	ret = 0;

release_firmware:
	if (runtime_irq_disabled)
		enable_irq(to_pci_dev(dev->mt76.dev)->irq);
	release_firmware(fw);
	return ret;
}
EXPORT_SYMBOL_GPL(mt7932_mcu_load_live_start_phase);

static int
mt7932_validate_power_packet(const struct mt7932_power_transcript_record *record,
			     const u8 *data, u32 data_len)
{
	if (record->transport || le16_to_cpu(record->flags) != BIT(0) ||
	    record->ext_cid || record->reserved || data_len < 44 ||
	    data_len > 1020)
		return -EINVAL;

	switch (record->cid) {
	case 0x5d:
		if (get_unaligned_le16(data + 2) != data_len ||
		    data[4] > 20 || data[5] > 3)
			return -EINVAL;
		break;
	case 0x95:
		if (data[5] != 3 || data_len != 44 + data[4] * 33)
			return -EINVAL;
		break;
	default:
		return -EINVAL;
	}

	return 0;
}

static int mt7932_validate_power_transcript(const struct firmware *fw)
{
	static const u8 magic[8] = { 'J', '7', 'P', 'W', 'R', 'T', 'R', '1' };
	const struct mt7932_power_transcript_header *header;
	size_t offset;
	u32 count, i, packets = 0;
	bool derived = false;

	BUILD_BUG_ON(sizeof(struct mt7932_power_transcript_record) !=
		     MT7932_POWER_RECORD_SIZE);
	if (fw->size < sizeof(*header))
		return -EINVAL;
	header = (const void *)fw->data;
	count = le32_to_cpu(header->records);
	if (memcmp(header->magic, magic, sizeof(magic)) ||
	    le16_to_cpu(header->version) != MT7932_POWER_TRANSCRIPT_VERSION ||
	    le16_to_cpu(header->record_size) != MT7932_POWER_RECORD_SIZE ||
	    count != MT7932_POWER_TRANSCRIPT_RECORDS)
		return -EINVAL;

	offset = sizeof(*header);
	for (i = 0; i < count; i++) {
		const struct mt7932_power_transcript_record *record;
		const u8 *data;
		u32 name_len, data_len;
		int ret;

		if (sizeof(*record) > fw->size - offset)
			return -EINVAL;
		record = (const void *)(fw->data + offset);
		offset += sizeof(*record);
		name_len = le32_to_cpu(record->name_len);
		data_len = le32_to_cpu(record->data_len);
		if (le32_to_cpu(record->order) != i || !name_len ||
		    name_len > 64 || name_len > fw->size - offset)
			return -EINVAL;
		offset += name_len;
		if (data_len > fw->size - offset)
			return -EINVAL;
		data = fw->data + offset;

		switch (record->kind) {
		case 1:
			if (derived)
				return -EINVAL;
			ret = mt7932_validate_power_packet(record, data, data_len);
			if (ret)
				return ret;
			packets++;
			break;
		case 2:
			derived = true;
			if (record->transport != 2 ||
			    le16_to_cpu(record->flags) || record->cid ||
			    record->ext_cid || record->reserved ||
			    !data_len || data_len > 0x200)
				return -EINVAL;
			break;
		default:
			return -EINVAL;
		}
		offset += data_len;
	}

	return offset == fw->size && packets == MT7932_POWER_TRANSCRIPT_PACKETS ?
	       0 : -EINVAL;
}

#define MT7932_CUSTOMER_CFG_BATCHES	16
#define MT7932_CUSTOMER_CFG_ITEMS	4
#define MT7932_CUSTOMER_CFG_KEY_LEN	32
#define MT7932_CUSTOMER_CFG_VALUE_LEN	32

struct mt7932_customer_cfg_item {
	u8 item_type;
	u8 key_len;
	u8 value_len;
	u8 pad;
	u8 key[MT7932_CUSTOMER_CFG_KEY_LEN];
	u8 value[MT7932_CUSTOMER_CFG_VALUE_LEN];
} __packed;

struct mt7932_customer_cfg_req {
	__le32 version;
	__le32 type;
	u8 item_num;
	u8 pad;
	__le16 buffer_len;
	struct mt7932_customer_cfg_item item[MT7932_CUSTOMER_CFG_ITEMS];
} __packed;

int mt7932_mcu_load_customer_cfg(struct mt792x_dev *dev)
{
	struct mt7932_customer_cfg_req *reqs;
	const struct firmware *fw;
	const u8 *pos, *end;
	int batch, err, total = 0;

	BUILD_BUG_ON(sizeof(struct mt7932_customer_cfg_item) != 0x44);
	BUILD_BUG_ON(sizeof(struct mt7932_customer_cfg_req) != 0x11c);

	err = request_firmware(&fw, MT7932_CUSTOMER_CFG_NAME, dev->mt76.dev);
	if (err)
		return err;

	reqs = kcalloc(MT7932_CUSTOMER_CFG_BATCHES, sizeof(*reqs),
			GFP_KERNEL);
	if (!reqs) {
		err = -ENOMEM;
		goto out_release;
	}

	pos = fw->data;
	end = fw->data + fw->size;
	while (pos < end) {
		struct mt7932_customer_cfg_item *item;
		const u8 *line_end, *key_end, *value, *value_end;
		size_t key_len, value_len;

		line_end = memchr(pos, '\n', end - pos);
		if (!line_end)
			line_end = end;
		while (pos < line_end && (*pos == ' ' || *pos == '\t'))
			pos++;
		if (pos == line_end || *pos == '#')
			goto next_line;

		key_end = pos;
		while (key_end < line_end && *key_end != ' ' &&
		       *key_end != '\t' && *key_end != '\r')
			key_end++;
		value = key_end;
		while (value < line_end && (*value == ' ' || *value == '\t'))
			value++;
		value_end = line_end;
		while (value_end > value &&
		       (value_end[-1] == ' ' || value_end[-1] == '\t' ||
			value_end[-1] == '\r'))
			value_end--;
		key_len = key_end - pos;
		value_len = value_end - value;
		if (!key_len || !value_len ||
		    key_len > MT7932_CUSTOMER_CFG_KEY_LEN ||
		    value_len > MT7932_CUSTOMER_CFG_VALUE_LEN) {
			err = -EINVAL;
			goto out_free;
		}
		if (total == MT7932_CUSTOMER_CFG_BATCHES *
			     MT7932_CUSTOMER_CFG_ITEMS) {
			err = -E2BIG;
			goto out_free;
		}

		item = &reqs[total / MT7932_CUSTOMER_CFG_ITEMS]
				.item[total % MT7932_CUSTOMER_CFG_ITEMS];
		item->item_type = 3;
		item->key_len = key_len;
		item->value_len = value_len;
		memcpy(item->key, pos, key_len);
		memcpy(item->value, value, value_len);
		total++;

next_line:
		pos = line_end < end ? line_end + 1 : end;
	}

	if (total != MT7932_CUSTOMER_CFG_BATCHES *
		     MT7932_CUSTOMER_CFG_ITEMS) {
		err = -EINVAL;
		goto out_free;
	}

	for (batch = 0; batch < MT7932_CUSTOMER_CFG_BATCHES; batch++) {
		struct mt7932_customer_cfg_req *req = &reqs[batch];

		req->type = cpu_to_le32(1);
		req->item_num = MT7932_CUSTOMER_CFG_ITEMS;
		req->buffer_len = cpu_to_le16(MT7932_CUSTOMER_CFG_ITEMS *
					       sizeof(req->item[0]));
		err = mt76_mcu_send_msg(&dev->mt76,
				 MCU_CE_CMD(GET_SET_CUSTOMER_CFG), req,
				 sizeof(*req), false);
		if (err)
			goto out_free;
		dev_info(dev->mt76.dev,
			 "J700_MT7932_CUSTOMER_CFG_BATCH_PASS: cid=0x70 batch=%d/16 items=4 bytes=284 first=%.*s last=%.*s response=none\n",
			 batch + 1,
			 req->item[0].key_len, req->item[0].key,
			 req->item[3].key_len, req->item[3].key);
	}
	dev_info(dev->mt76.dev,
		 "J700_MT7932_CUSTOMER_CFG_PASS: cid=0x70 batches=16 items=64 stream=continuous source=%s\n",
		 MT7932_CUSTOMER_CFG_NAME);

out_free:
	kfree(reqs);
out_release:
	release_firmware(fw);
	return err;
}
EXPORT_SYMBOL_GPL(mt7932_mcu_load_customer_cfg);

int mt7932_mcu_load_power_transcript(struct mt792x_dev *dev)
{
	static const u8 group_end[] = { 10, 11, 12, 13, 28, 42 };
	static const char * const group_name[] = {
		"normal-2g5", "sar", "common-2g", "sdb",
		"per-rate-6g", "legacy-6g",
	};
	const struct mt7932_power_transcript_header *header;
	const struct firmware *fw;
	size_t offset;
	u32 count, i, packet = 0, group = 0;
	int ret;

	if (!is_mt7932(&dev->mt76))
		return -EOPNOTSUPP;
	BUILD_BUG_ON(ARRAY_SIZE(group_end) != ARRAY_SIZE(group_name));

	ret = request_firmware(&fw, MT7932_POWER_TRANSCRIPT_NAME,
			       dev->mt76.dev);
	if (ret)
		return ret;
	ret = mt7932_validate_power_transcript(fw);
	if (ret) {
		dev_err(dev->mt76.dev,
			"J700_MT7932_POWER_TRANSCRIPT_REJECT: source=%s bytes=%zu ret=%d\n",
			MT7932_POWER_TRANSCRIPT_NAME, fw->size, ret);
		goto release_firmware;
	}

	/* Apple sends the complete country/power transaction once from
	 * wlanLoadManufactureData(), after calibration and the initial domain
	 * command but before PreNet DBDC and network registration.  The measured
	 * Run191 HIF lifecycle is one continuous 58-command stream: sixteen CID70
	 * customer batches followed immediately by these 42 power packets.  It
	 * wraps the 24-entry TX17 ring twice, relying on Apple's pressure-triggered
	 * DDONE reclamation, and does not empty the ring at transaction boundaries.
	 * Preserve that lifecycle exactly; the generic send path performs the same
	 * bounded reclaim when the ring reaches 23 live descriptors.
	 */
	header = (const void *)fw->data;
	count = le32_to_cpu(header->records);
	offset = sizeof(*header);
	for (i = 0; i < count; i++) {
		const struct mt7932_power_transcript_record *record;
		const char *name;
		const u8 *data;
		u32 name_len, data_len;
		int cmd;

		record = (const void *)(fw->data + offset);
		offset += sizeof(*record);
		name_len = le32_to_cpu(record->name_len);
		data_len = le32_to_cpu(record->data_len);
		name = (const void *)(fw->data + offset);
		offset += name_len;
		data = fw->data + offset;
		offset += data_len;
		if (record->kind != 1)
			continue;

		cmd = __MCU_CMD_FIELD_CE |
		      FIELD_PREP(__MCU_CMD_FIELD_ID, record->cid);
		ret = mt76_mcu_send_msg(&dev->mt76, cmd, data, data_len,
					false);
		if (ret)
			goto release_firmware;
		packet++;
		dev_info(dev->mt76.dev,
			 "J700_MT7932_POWER_PACKET_QUEUED: packet=%u/42 cid=0x%02x band=%u bytes=%u name=%.*s response=none\n",
			 packet, record->cid, record->band, data_len,
			 (int)name_len, name);
		if (group < ARRAY_SIZE(group_end) &&
		    packet == group_end[group]) {
			dev_info(dev->mt76.dev,
				 "J700_MT7932_POWER_GROUP_PASS: group=%u/6 name=%s end_packet=%u streaming=1 ring_drain=none\n",
				 group + 1, group_name[group], packet);
			group++;
		}
	}
	if (packet != MT7932_POWER_TRANSCRIPT_PACKETS ||
	    group != ARRAY_SIZE(group_end)) {
		ret = -EINVAL;
		goto release_firmware;
	}
	dev_info(dev->mt76.dev,
		 "J700_MT7932_POWER_TRANSCRIPT_PASS: country=AU mode6g=SP packets=42 cid5d=28 cid95=14 source=%s bytes=%zu\n",
		 MT7932_POWER_TRANSCRIPT_NAME, fw->size);

release_firmware:
	release_firmware(fw);
	return ret;
}
EXPORT_SYMBOL_GPL(mt7932_mcu_load_power_transcript);

int mt7921_run_firmware(struct mt792x_dev *dev)
{
	int err;
	bool mt7932 = is_mt7932(&dev->mt76);

	err = mt792x_load_firmware(dev);
	if (err)
		return err;
	if (mt7932) {
		err = mt7932_mcu_start_runtime(dev);
		if (err) {
			dev_err(dev->mt76.dev,
				"J700_MT7932_FIRMWARE_GATE_FAIL: stage=runtime-preamble ret=%d\n",
				err);
			return err;
		}
	}

	err = mt7921_mcu_get_nic_capability(&dev->phy);
	if (err) {
		if (mt7932)
			dev_err(dev->mt76.dev,
				"J700_MT7932_FIRMWARE_GATE_FAIL: stage=nic-capability ret=%d\n",
				err);
		return err;
	}
	if (mt7932)
		dev_info(dev->mt76.dev,
			 "J700_MT7932_NIC_CAPABILITY_PASS: single-sku=%u has-6ghz=%u\n",
			 dev->phy.has_single_sku, dev->mphy.cap.has_6ghz);
	if (mt7932 && dev->mphy.cap.has_6ghz)
		dev_info(dev->mt76.dev,
			 "J700_MT7932_6GHZ_CAP_PASS: power-path=apple-122-byte-au-sp transcript=pre-net\n");
	if (mt7932) {
		err = mt7932_mcu_runtime_config(dev);
		if (err) {
			dev_err(dev->mt76.dev,
				"J700_MT7932_FIRMWARE_GATE_FAIL: stage=runtime-config ret=%d\n",
				err);
			return err;
		}
	}

	err = mt7921_load_clc(dev, mt792x_ram_name(dev));
	if (err) {
		if (mt7932)
			dev_err(dev->mt76.dev,
				"J700_MT7932_FIRMWARE_GATE_FAIL: stage=clc ret=%d\n",
				err);
		return err;
	}
	if (mt7932)
		dev_info(dev->mt76.dev,
			 "J700_MT7932_CLC_HOST_RULES_PASS: firmware-send=deferred-to-ordinals-84-86\n");
	/* Runtime query responses after firmware start use the MCU event path.
	 * Arm it before the measured MT7932 pre-cal transaction: Cycle498 proved
	 * ordinals 84..87 retire, while query 88 receives no response when this
	 * state is left unset until after the transaction.
	 */
	set_bit(MT76_STATE_MCU_RUNNING, &dev->mphy.state);
	if (mt7932)
		dev_info(dev->mt76.dev,
			 "J700_MT7932_RUNTIME_EVENT_PATH_ARMED: before=pre-cal-query88\n");
	if (mt7932) {
		/* Commands 84..134 are one uninterrupted measured transaction:
		 * the MT7932-specific CLC records, mode-3 EFUSE selection, PPR/query
		 * prerequisites and initial XZ regulatory/power setup.  It precedes
		 * WCAL ordinal 135 in the successful macOS trace.
		 */
		err = mt7932_mcu_load_live_start_phase(
			dev, MT7932_LIVE_PHASE_PRE_CAL);
		if (err)
			return err;
		err = mt7932_mcu_drain_tx17(dev, "live-pre-cal-post", false);
		if (err)
			return err;
		dev_info(dev->mt76.dev,
			 "J700_MT7932_PRE_CAL_TRANSCRIPT_PASS: ordinals=84-134 commands=51 clc=3x32 efuse=fdr-mode3 country=XZ power=2g,5g next=wcal-135\n");
	}
	/* Apple selects MT7932 indexed FW-log mode 2 from
	 * wlanOnWhenProbeSuccess(), after calibration and MAC initialization.
	 * Defer it to mt7921_init_work(); enabling indexed logs here can flood
	 * RX while the 24-entry one-time-calibration command ring is active.
	 */
	if (mt7932) {
		dev_info(dev->mt76.dev,
			 "J700_MT7932_FW_LOG_DEFERRED: mode=2 until=post-hardware-init\n");
		err = 0;
	} else {
		err = mt7921_mcu_fw_log_2_host(dev, 1);
	}
	if (mt7932) {
		if (err)
			dev_err(dev->mt76.dev,
				"J700_MT7932_FIRMWARE_GATE_FAIL: stage=fw-log ret=%d\n",
				err);
		else
			dev_info(dev->mt76.dev,
				 "J700_MT7932_FIRMWARE_GATE_PASS: patch=accepted ram=started n9=ready mcu=responsive\n");
	}

	return err;
}
EXPORT_SYMBOL_GPL(mt7921_run_firmware);

int mt7921_mcu_radio_led_ctrl(struct mt792x_dev *dev, u8 value)
{
	struct {
		u8 ctrlid;
		u8 rsv[3];
	} __packed req = {
		.ctrlid = value,
	};

	return mt76_mcu_send_msg(&dev->mt76, MCU_EXT_CMD(ID_RADIO_ON_OFF_CTRL),
				&req, sizeof(req), false);
}
EXPORT_SYMBOL_GPL(mt7921_mcu_radio_led_ctrl);

int mt7921_mcu_set_tx(struct mt792x_dev *dev, struct ieee80211_vif *vif)
{
	struct mt792x_vif *mvif = (struct mt792x_vif *)vif->drv_priv;
	struct edca {
		__le16 cw_min;
		__le16 cw_max;
		__le16 txop;
		__le16 aifs;
		u8 guardtime;
		u8 acm;
	} __packed;
	struct mt7921_mcu_tx {
		struct edca edca[IEEE80211_NUM_ACS];
		u8 bss_idx;
		u8 qos;
		u8 wmm_idx;
		u8 pad;
	} __packed req = {
		.bss_idx = mvif->bss_conf.mt76.idx,
		.qos = vif->bss_conf.qos,
		.wmm_idx = mvif->bss_conf.mt76.wmm_idx,
	};
	struct mu_edca {
		u8 cw_min;
		u8 cw_max;
		u8 aifsn;
		u8 acm;
		u8 timer;
		u8 padding[3];
	};
	struct mt7921_mcu_mu_tx {
		u8 ver;
		u8 pad0;
		__le16 len;
		u8 bss_idx;
		u8 qos;
		u8 wmm_idx;
		u8 pad1;
		struct mu_edca edca[IEEE80211_NUM_ACS];
		u8 pad3[32];
	} __packed req_mu = {
		.bss_idx = mvif->bss_conf.mt76.idx,
		.qos = vif->bss_conf.qos,
		.wmm_idx = mvif->bss_conf.mt76.wmm_idx,
	};
	static const int to_aci[] = { 1, 0, 2, 3 };
	int ac, ret;

	for (ac = 0; ac < IEEE80211_NUM_ACS; ac++) {
		struct ieee80211_tx_queue_params *q = &mvif->bss_conf.queue_params[ac];
		struct edca *e = &req.edca[to_aci[ac]];

		e->aifs = cpu_to_le16(q->aifs);
		e->txop = cpu_to_le16(q->txop);

		if (q->cw_min)
			e->cw_min = cpu_to_le16(q->cw_min);
		else
			e->cw_min = cpu_to_le16(5);

		if (q->cw_max)
			e->cw_max = cpu_to_le16(q->cw_max);
		else
			e->cw_max = cpu_to_le16(10);
	}

	ret = mt76_mcu_send_msg(&dev->mt76, MCU_CE_CMD(SET_EDCA_PARMS), &req,
				sizeof(req), false);
	if (ret)
		return ret;

	if (!vif->bss_conf.he_support)
		return 0;

	for (ac = 0; ac < IEEE80211_NUM_ACS; ac++) {
		struct ieee80211_he_mu_edca_param_ac_rec *q;
		struct mu_edca *e;

		if (!mvif->bss_conf.queue_params[ac].mu_edca)
			break;

		q = &mvif->bss_conf.queue_params[ac].mu_edca_param_rec;
		e = &(req_mu.edca[to_aci[ac]]);

		e->cw_min = q->ecw_min_max & 0xf;
		e->cw_max = (q->ecw_min_max & 0xf0) >> 4;
		e->aifsn = q->aifsn;
		e->timer = q->mu_edca_timer;
	}

	return mt76_mcu_send_msg(&dev->mt76, MCU_CE_CMD(SET_MU_EDCA_PARMS),
				 &req_mu, sizeof(req_mu), false);
}

int mt7921_mcu_set_roc(struct mt792x_phy *phy, struct mt792x_vif *vif,
		       struct ieee80211_channel *chan, int duration,
		       enum mt7921_roc_req type, u8 token_id)
{
	int center_ch = ieee80211_frequency_to_channel(chan->center_freq);
	struct mt792x_dev *dev = phy->dev;
	struct {
		struct {
			u8 rsv[4];
		} __packed hdr;
		struct roc_acquire_tlv {
			__le16 tag;
			__le16 len;
			u8 bss_idx;
			u8 tokenid;
			u8 control_channel;
			u8 sco;
			u8 band;
			u8 bw;
			u8 center_chan;
			u8 center_chan2;
			u8 bw_from_ap;
			u8 center_chan_from_ap;
			u8 center_chan2_from_ap;
			u8 reqtype;
			__le32 maxinterval;
			u8 dbdcband;
			u8 rsv[3];
		} __packed roc;
	} __packed req = {
		.roc = {
			.tag = cpu_to_le16(UNI_ROC_ACQUIRE),
			.len = cpu_to_le16(sizeof(struct roc_acquire_tlv)),
			.tokenid = token_id,
			.reqtype = type,
			.maxinterval = cpu_to_le32(duration),
			.bss_idx = vif->bss_conf.mt76.idx,
			.control_channel = chan->hw_value,
			.bw = CMD_CBW_20MHZ,
			.bw_from_ap = CMD_CBW_20MHZ,
			.center_chan = center_ch,
			.center_chan_from_ap = center_ch,
			.dbdcband = 0xff, /* auto */
		},
	};

	if (chan->hw_value < center_ch)
		req.roc.sco = 1; /* SCA */
	else if (chan->hw_value > center_ch)
		req.roc.sco = 3; /* SCB */

	switch (chan->band) {
	case NL80211_BAND_6GHZ:
		req.roc.band = 3;
		break;
	case NL80211_BAND_5GHZ:
		req.roc.band = 2;
		break;
	default:
		req.roc.band = 1;
		break;
	}

	return mt76_mcu_send_msg(&dev->mt76, MCU_UNI_CMD(ROC),
				 &req, sizeof(req), false);
}

int mt7921_mcu_abort_roc(struct mt792x_phy *phy, struct mt792x_vif *vif,
			 u8 token_id)
{
	struct mt792x_dev *dev = phy->dev;
	struct {
		struct {
			u8 rsv[4];
		} __packed hdr;
		struct roc_abort_tlv {
			__le16 tag;
			__le16 len;
			u8 bss_idx;
			u8 tokenid;
			u8 dbdcband;
			u8 rsv[5];
		} __packed abort;
	} __packed req = {
		.abort = {
			.tag = cpu_to_le16(UNI_ROC_ABORT),
			.len = cpu_to_le16(sizeof(struct roc_abort_tlv)),
			.tokenid = token_id,
			.bss_idx = vif->bss_conf.mt76.idx,
			.dbdcband = 0xff, /* auto*/
		},
	};

	return mt76_mcu_send_msg(&dev->mt76, MCU_UNI_CMD(ROC),
				 &req, sizeof(req), false);
}

int mt7921_mcu_set_chan_info(struct mt792x_phy *phy, int cmd)
{
	struct mt792x_dev *dev = phy->dev;
	struct cfg80211_chan_def *chandef = &phy->mt76->chandef;
	int freq1 = chandef->center_freq1;
	struct {
		u8 control_ch;
		u8 center_ch;
		u8 bw;
		u8 tx_streams_num;
		u8 rx_streams;	/* mask or num */
		u8 switch_reason;
		u8 band_idx;
		u8 center_ch2;	/* for 80+80 only */
		__le16 cac_case;
		u8 channel_band;
		u8 rsv0;
		__le32 outband_freq;
		u8 txpower_drop;
		u8 ap_bw;
		u8 ap_center_ch;
		u8 rsv1[57];
	} __packed req = {
		.control_ch = chandef->chan->hw_value,
		.center_ch = ieee80211_frequency_to_channel(freq1),
		.bw = mt76_connac_chan_bw(chandef),
		.tx_streams_num = hweight8(phy->mt76->antenna_mask),
		.rx_streams = phy->mt76->antenna_mask,
		.band_idx = phy != &dev->phy,
	};

	if (chandef->chan->band == NL80211_BAND_6GHZ)
		req.channel_band = 2;
	else
		req.channel_band = chandef->chan->band;

	if (cmd == MCU_EXT_CMD(SET_RX_PATH) ||
	    dev->mt76.hw->conf.flags & IEEE80211_CONF_MONITOR)
		req.switch_reason = CH_SWITCH_NORMAL;
	else if (phy->mt76->offchannel)
		req.switch_reason = CH_SWITCH_SCAN_BYPASS_DPD;
	else if (!cfg80211_reg_can_beacon(dev->mt76.hw->wiphy, chandef,
					  NL80211_IFTYPE_AP))
		req.switch_reason = CH_SWITCH_DFS;
	else
		req.switch_reason = CH_SWITCH_NORMAL;

	if (cmd == MCU_EXT_CMD(CHANNEL_SWITCH))
		req.rx_streams = hweight8(req.rx_streams);

	if (chandef->width == NL80211_CHAN_WIDTH_80P80) {
		int freq2 = chandef->center_freq2;

		req.center_ch2 = ieee80211_frequency_to_channel(freq2);
	}

	if (is_mt7932(&dev->mt76) && cmd == MCU_EXT_CMD(SET_RX_PATH)) {
		/* AppleSunriseWLAN 25G83 does not issue generic SET_RX_PATH during
		 * adapter start; CID 0x03 owns scan channel traversal.  Preserve
		 * later CHANNEL_SWITCH commands for association/channel changes.
		 */
		dev_info(dev->mt76.dev,
			 "J700_MT7932_SET_RX_PATH_SKIP: cmd=0x%08x source=apple-25G83 sent=0\n",
			 cmd);
		return 0;
	}

	if (is_mt7932(&dev->mt76)) {
		dev_info(dev->mt76.dev,
			 "J700_MT7932_CHAN_INFO_NOACK: cmd=0x%08x control=%u center=%u band=%u reason=%u\n",
			 cmd, req.control_ch, req.center_ch, req.channel_band,
			 req.switch_reason);
		return mt76_mcu_send_msg(&dev->mt76, cmd, &req, sizeof(req),
					 false);
	}

	return mt76_mcu_send_msg(&dev->mt76, cmd, &req, sizeof(req), true);
}

int mt7921_mcu_set_eeprom(struct mt792x_dev *dev)
{
	struct req_hdr {
		u8 buffer_mode;
		u8 format;
		__le16 len;
	} __packed req = {
		.buffer_mode = EE_MODE_EFUSE,
		.format = EE_FORMAT_WHOLE,
	};

	return mt76_mcu_send_msg(&dev->mt76, MCU_EXT_CMD(EFUSE_BUFFER_MODE),
				 &req, sizeof(req), true);
}
EXPORT_SYMBOL_GPL(mt7921_mcu_set_eeprom);

int mt7932_mcu_load_wcal(struct mt792x_dev *dev)
{
	struct mt7932_wcal_req {
		u8 buffer_mode;
		u8 format;
		__le16 len;
		u8 data[];
	} __packed;
	struct mt7932_wcal_req *req;
	const struct firmware *firmware;
	size_t request_size;
	int cmd = MCU_EXT_CMD(EFUSE_BUFFER_MODE);
	int ret;

	if (!is_mt7932(&dev->mt76))
		return -EOPNOTSUPP;

	ret = request_firmware(&firmware, MT7932_WCAL, dev->mt76.dev);
	if (ret)
		return ret;
	if (!firmware->size || firmware->size > 1024) {
		ret = -EINVAL;
		goto release_firmware;
	}

	request_size = sizeof(*req) + firmware->size;
	req = kzalloc(request_size, GFP_KERNEL);
	if (!req) {
		ret = -ENOMEM;
		goto release_firmware;
	}
	req->buffer_mode = 3;
	req->format = 0;
	req->len = cpu_to_le16(firmware->size);
	memcpy(req->data, firmware->data, firmware->size);

	ret = mt76_mcu_send_msg(&dev->mt76, cmd, req, request_size, true);
	if (!ret)
		dev_info(dev->mt76.dev,
			 "J700_MT7932_WCAL_PASS: cid=ed ext=21 mode=3 format=0 bytes=%zu rf_enabled=0\n",
			 firmware->size);
	kfree(req);

release_firmware:
	release_firmware(firmware);
	return ret;
}
EXPORT_SYMBOL_GPL(mt7932_mcu_load_wcal);

#define MT7932_CAL_STREAM_VERSION	1
#define MT7932_CAL_STREAM_RECORD_MAX	4096
#define MT7932_CAL_WIRE_HEADER_SIZE	20
#define MT7932_CAL_WIRE_DATA_MAX		1080

struct mt7932_cal_stream_header {
	u8 magic[8];
	__le16 version;
	__le16 records;
	__le32 size;
} __packed;

struct mt7932_cal_sequence {
	bool seen;
	u8 type;
	u8 block_current;
	u8 block_total;
	u32 cal_param;
};

static int mt7932_validate_cal_record(const u8 *record, size_t length,
				      struct mt7932_cal_sequence *sequence)
{
	u32 data_length, cache_mark, temperature, cal_param;
	u8 type, block_current, block_total;

	if (length <= MT7932_CAL_WIRE_HEADER_SIZE ||
	    length > MT7932_CAL_WIRE_HEADER_SIZE + MT7932_CAL_WIRE_DATA_MAX)
		return -EINVAL;
	type = record[0];
	block_current = record[2] & 0xf;
	block_total = record[2] >> 4;
	if (type > 3 || record[1] ||
	    !block_total || block_current >= block_total)
		return -EINVAL;

	data_length = get_unaligned_le32(record + 4);
	if (data_length != length - MT7932_CAL_WIRE_HEADER_SIZE)
		return -EINVAL;
	cache_mark = get_unaligned_le32(record + 12);
	temperature = get_unaligned_le32(record + 16);
	if (type < 2 && (cache_mark || temperature))
		return -EINVAL;
	cal_param = get_unaligned_le32(record + 8);

	if (!sequence->seen || type != sequence->type ||
	    cal_param != sequence->cal_param) {
		if (sequence->seen &&
		    (sequence->block_current + 1 != sequence->block_total ||
		     type < sequence->type || type > sequence->type + 1 ||
		     (type == sequence->type &&
		      cal_param <= sequence->cal_param)))
			return -EINVAL;
		if ((!sequence->seen && type != 0) || block_current)
			return -EINVAL;
	} else {
		if (block_total != sequence->block_total ||
		    block_current != sequence->block_current + 1)
			return -EINVAL;
	}

	sequence->seen = true;
	sequence->type = type;
	sequence->block_current = block_current;
	sequence->block_total = block_total;
	sequence->cal_param = cal_param;
	return 0;
}

static int mt7932_validate_cal_stream(const struct firmware *fw, u16 *countp)
{
	static const u8 magic[8] = { 'J', '7', 'C', 'A', 'L', 'D', '6', 0 };
	const struct mt7932_cal_stream_header *header;
	struct mt7932_cal_sequence sequence = {};
	size_t offset;
	u16 count, i;
	int ret;

	if (fw->size < sizeof(*header))
		return -EINVAL;
	header = (const void *)fw->data;
	count = le16_to_cpu(header->records);
	if (memcmp(header->magic, magic, sizeof(magic)) ||
	    le16_to_cpu(header->version) != MT7932_CAL_STREAM_VERSION ||
	    !count || count > MT7932_CAL_STREAM_RECORD_MAX ||
	    le32_to_cpu(header->size) != fw->size)
		return -EINVAL;

	offset = sizeof(*header);
	for (i = 0; i < count; i++) {
		u16 length, reserved;

		if (offset + 4 > fw->size)
			return -EINVAL;
		length = get_unaligned_le16(fw->data + offset);
		reserved = get_unaligned_le16(fw->data + offset + 2);
		offset += 4;
		if (reserved || length > fw->size - offset)
			return -EINVAL;
		ret = mt7932_validate_cal_record(fw->data + offset, length,
						 &sequence);
		if (ret)
			return ret;
		offset += length;
	}
	if (offset != fw->size || !sequence.seen ||
	    sequence.block_current + 1 != sequence.block_total)
		return -EINVAL;
	*countp = count;
	return 0;
}

int mt7932_mcu_drain_tx17(struct mt792x_dev *dev, const char *phase,
			  bool reset_bounce)
{
	struct mt76_dev *mdev = &dev->mt76;
	struct mt76_queue *q = mdev->q_mcu[MT_MCUQ_WM];
	u32 used, last_queued, stagnant = 0, wake_rekicks = 0;
	int i;

	if (!q || !mdev->queue_ops->tx_cleanup)
		return -EOPNOTSUPP;
	last_queued = READ_ONCE(q->queued);

	for (i = 0; i < 5000; i++) {
		u32 queued;

		mdev->queue_ops->tx_cleanup(mdev, q, false);
		queued = READ_ONCE(q->queued);
		if (!queued) {
			used = READ_ONCE(q->tx_bounce_next);
			if (reset_bounce)
				WRITE_ONCE(q->tx_bounce_next, 0);
			dev_info(mdev->dev,
				 "J700_MT7932_TX17_DRAIN_PASS: phase=%s hw=%u head=%u tail=%u cidx=%u didx=%u polls=%d bounce_used=%u bounce_reset=%u wake_rekicks=%u\n",
				 phase, q->hw_idx, q->head, q->tail,
				 readl(&q->regs->cpu_idx), readl(&q->regs->dma_idx),
				 i + 1, used, reset_bounce, wake_rekicks);
			return 0;
		}
		if (queued != last_queued) {
			last_queued = queued;
			stagnant = 0;
		} else if (!strcmp(phase, "power-group") &&
			   ++stagnant >= 100 && wake_rekicks < 16) {
			u32 cidx = readl(&q->regs->cpu_idx);
			u32 didx = readl(&q->regs->dma_idx);

			dev_info(mdev->dev,
				 "J700_MT7932_TX17_WAKE_REKICK: attempt=%u queued=%u head=%u tail=%u cidx=%u didx=%u pse_n9=0x%08x pse_err=0x%08x pse_err1=0x%08x\n",
				 wake_rekicks + 1, queued, q->head, q->tail,
				 cidx, didx,
				 mt76_rr(dev, MT_PSE_BASE + 0x30),
				 mt76_rr(dev, MT_PSE_BASE + 0x34),
				 mt76_rr(dev, MT_PSE_BASE + 0x38));
			mt76_queue_kick(dev, q);
			dev_info(mdev->dev,
				 "J700_MT7932_TX17_WAKE_REKICK_PASS: attempt=%u cidx=%u didx=%u\n",
				 wake_rekicks + 1, readl(&q->regs->cpu_idx),
				 readl(&q->regs->dma_idx));
			wake_rekicks++;
			stagnant = 0;
		}
		usleep_range(100, 200);
	}

	if (q->tx_bounce_buf && q->tx_bounce_stride) {
		struct mt76_queue_entry *entry = &q->entry[q->tail];
		dma_addr_t dma = entry->dma_addr[0];
		u64 pool_bytes = (u64)q->tx_bounce_entries *
				 q->tx_bounce_stride;

		if (dma >= q->tx_bounce_dma &&
		    dma - q->tx_bounce_dma < pool_bytes) {
			u32 slot = div_u64(dma - q->tx_bounce_dma,
					   q->tx_bounce_stride);
			const u8 *payload = q->tx_bounce_buf +
					    slot * q->tx_bounce_stride;
			const struct mt76_connac2_mcu_txd *txd =
					(const void *)payload;
			u16 bytes = min_t(u16, entry->dma_len[0], 64);

			if (bytes >= sizeof(*txd))
				dev_err(mdev->dev,
					"J700_MT7932_TX17_DRAIN_BLOCKER: phase=%s tail=%u didx=%u slot=%u dma_len=%u cid=0x%02x ext=0x%02x seq=%u set=%u ack=%u txd_len=%u raw=%*ph\n",
					phase, q->tail, readl(&q->regs->dma_idx),
					slot, entry->dma_len[0], txd->cid,
					txd->ext_cid, txd->seq, txd->set_query,
					txd->ext_cid_ack,
					le16_to_cpu(txd->len), bytes, payload);
			else
				dev_err(mdev->dev,
					"J700_MT7932_TX17_DRAIN_BLOCKER: phase=%s tail=%u didx=%u slot=%u dma_len=%u raw=%*ph\n",
					phase, q->tail, readl(&q->regs->dma_idx),
					slot, entry->dma_len[0], bytes, payload);
		}
	}
	{
		static const u16 wfdma_aux_offsets[] = {
			0x100, 0x108, 0x10c, 0x13c, 0x140,
			0x1ec, 0x1f8, 0x1fc, 0x228, 0x22c,
			0x240, 0x260, 0x264, 0x268, 0x26c,
			0x280, 0x298, 0x29c, 0x2a8, 0x2ac,
			0x2b8, 0x2d0, 0x2d4, 0x2e0, 0x2e4,
			0x2e8, 0x2f0, 0x2f4, 0x2f8, 0x2fc,
			0x800, 0x804, 0x808, 0x80c, 0x810,
			0x814, 0x820, 0x900, 0x904, 0x908,
			0x90c, 0x910, 0x914, 0x920, 0x924,
			0x928, 0x92c, 0x930,
		};
		u32 ddone = 0, live = READ_ONCE(q->queued);
		u32 pc[8], misc[8], rgu[8];
		u32 pse_n9, pse_err, pse_err1, pse_freepg;
		u32 pse_hif0_pg, pse_cpu_pg, irq_ctl0, irq_ctl1;
		u32 pc_ring[32], lr_ring[32];
		u32 wfdma_dbg[0x56];
		u32 tx_pf_sta0, tx_pf_sta1, tx_pf_sel, tx_pf_dmad[4];
		u32 tx_dbg0, tx_dbg1, pause0, pause1, hif_busy, err_int;
		u32 glo_ext0, glo_ext1, glo_ext2, info, info_ext;
		u32 wfdma_aux[ARRAY_SIZE(wfdma_aux_offsets)];
		u32 ext_wrap[13];
		u32 tx_bkrs[3][3];
		u32 dma_ranges[8], mcu_int_sta, mcu_int_ena;
		u32 timeout_cfg, misc_cfg, glo_cfg2;
		u32 dmashdl_status, dmashdl_status02, dmashdl_status0;
		u32 dmashdl_gp[16], dmashdl_pkt[8];
		u32 pse_queue_empty, pse_refill, pse_group[10], pse_enq[5];
		u32 pse_wfdma_buf, pse_misc_func, pse_ckg, pse_seek[7];
		u32 r2a[2][13];
		struct mt76_desc *blocked = &q->desc[q->tail];
		bool tx_pf_match;
		u32 j, k;

		pse_n9 = mt76_rr(dev, MT_PSE_BASE + 0x30);
		pse_err = mt76_rr(dev, MT_PSE_BASE + 0x34);
		pse_err1 = mt76_rr(dev, MT_PSE_BASE + 0x38);
		pse_freepg = mt76_rr(dev, MT_PSE_BASE + 0x380);
		pse_hif0_pg = mt76_rr(dev, MT_PSE_BASE + 0x150);
		pse_cpu_pg = mt76_rr(dev, MT_PSE_BASE + 0x158);
		irq_ctl0 = mt7921_l1_rr(dev, 0x89050100);
		irq_ctl1 = mt7921_l1_rr(dev, 0x89050200);
		for (j = 0; j < ARRAY_SIZE(pc_ring); j++) {
			pc_ring[j] = mt7921_l1_rr(dev, 0x89050104 + j * 4);
			lr_ring[j] = mt7921_l1_rr(dev, 0x89050204 + j * 4);
		}
		for (j = 0; j < q->ndesc; j++)
			if (le32_to_cpu(READ_ONCE(q->desc[j].ctrl)) &
			    MT_DMA_CTL_DMA_DONE)
				ddone |= BIT(j);
		for (j = 0; j < ARRAY_SIZE(pc); j++) {
			pc[j] = mt76_rr(dev, MT7932_WM_MCU_PC_DBG);
			misc[j] = mt76_rr(dev, MT_CONN_ON_MISC);
			rgu[j] = mt76_rr(dev, MT7932_WF_TOP_RGU_ON_DEBUG);
			udelay(100);
		}
		/* MediaTek's MT7933 vendor dump selects 0x100..0x155 through
		 * WPDMA_DBG_IDX (+0x124) and reads WPDMA_DBG_PROBE (+0x128).
		 * Run 191 contains the same Apple selector sweep, so retain the raw
		 * values for a direct successful-macOS comparison.
		 */
		for (j = 0; j < ARRAY_SIZE(wfdma_dbg); j++) {
			mt76_wr(dev, MT_WFDMA0(0x124), 0x100 + j);
			wfdma_dbg[j] = mt76_rr(dev, MT_WFDMA0(0x128));
		}
		/* MT7933 CODA exposes the TX descriptor-prefetch boundary directly.
		 * PF_STA0 reports valid/active TX queues and PF_STA1 reports queues
		 * paused because their last descriptor is still inside the TX engine.
		 * Selecting ring 17 in PF_STA1 makes 0x908..0x914 return its last
		 * prefetched descriptor.  This bounded failure-only read distinguishes
		 * a doorbell/prefetch stop from a descriptor fetched and blocked farther
		 * downstream without mutating the command path.
		 */
		tx_pf_sta0 = mt76_rr(dev, MT_WFDMA0(0x900));
		tx_pf_sta1 = mt76_rr(dev, MT_WFDMA0(0x904));
		tx_pf_sel = tx_pf_sta1 & GENMASK(8, 0);
		mt76_rmw(dev, MT_WFDMA0(0x904), GENMASK(8, 0),
			 BIT(8) | 17);
		for (j = 0; j < ARRAY_SIZE(tx_pf_dmad); j++)
			tx_pf_dmad[j] = mt76_rr(dev, MT_WFDMA0(0x908 + j * 4));
		mt76_rmw(dev, MT_WFDMA0(0x904), GENMASK(8, 0), tx_pf_sel);
		tx_pf_match = tx_pf_dmad[0] ==
			le32_to_cpu(READ_ONCE(blocked->buf0)) &&
			tx_pf_dmad[1] == le32_to_cpu(READ_ONCE(blocked->ctrl)) &&
			tx_pf_dmad[2] == le32_to_cpu(READ_ONCE(blocked->buf1)) &&
			tx_pf_dmad[3] == le32_to_cpu(READ_ONCE(blocked->info));
		tx_dbg0 = mt76_rr(dev, MT_WFDMA0(0x2a0));
		tx_dbg1 = mt76_rr(dev, MT_WFDMA0(0x2a4));
		pause0 = mt76_rr(dev, MT_WFDMA0(0x220));
		pause1 = mt76_rr(dev, MT_WFDMA0(0x224));
		hif_busy = mt76_rr(dev, MT_WFDMA0(0x138));
		err_int = mt76_rr(dev, MT_WFDMA0(0x1e8));
		glo_ext0 = mt76_rr(dev, MT_WFDMA0(0x2b0));
		glo_ext1 = mt76_rr(dev, MT_WFDMA0(0x2b4));
		glo_ext2 = mt76_rr(dev, MT_WFDMA0(0x2b8));
		info = mt76_rr(dev, MT_WFDMA0(0x284));
		info_ext = mt76_rr(dev, MT_WFDMA0(0x288));
		for (j = 0; j < ARRAY_SIZE(wfdma_aux); j++)
			wfdma_aux[j] = mt76_rr(dev,
				MT_WFDMA0(wfdma_aux_offsets[j]));
		/* MT7933's wrapper owns the AXI sleep, clock and dispatch controls
		 * immediately upstream of R2A.  The public core snapshot used through
		 * Cycle 555 omitted these registers and GLO_CFG_EXT2, even though the
		 * latter can halt or flush TX dispatch.  Keep this failure-only probe
		 * read-only and use the same offsets in the paused macOS capture.
		 */
		{
			static const u16 ext_wrap_offsets[] = {
				0x000, 0x038, 0x03c, 0x040, 0x044, 0x050,
				0x054, 0x074, 0x094, 0x098, 0x09c, 0x0a0,
				0x0a4,
			};

			for (j = 0; j < ARRAY_SIZE(ext_wrap); j++)
				ext_wrap[j] = mt76_rr(dev,
					MT_WFDMA_EXT_CSR(ext_wrap_offsets[j]));
		}
		/* Connac3 WFDMA places live internal queue state for TX rings
		 * 16..18 at 0xb00/0xb10/0xb20.  Each triplet exposes the prefetch
		 * and payload DIDX, dispatch CIDX/DIDX, payload CIDX, empty flags and
		 * dispatch-valid bit.  These registers expose useful live state even
		 * when GLO_CFG_EXT0[15] backup/restore enable is clear, and remain
		 * independent of the public ring CIDX/DIDX registers reset by
		 * MT_WFDMA0_RST_DTX_PTR.
		 */
		for (j = 0; j < ARRAY_SIZE(tx_bkrs); j++)
			for (k = 0; k < ARRAY_SIZE(tx_bkrs[j]); k++)
				tx_bkrs[j][k] = mt76_rr(dev,
					MT_WFDMA0(0xb00 + j * 0x10 + k * 4));
		/* MT7933 checks descriptor and payload addresses against four optional
		 * 36-bit aperture pairs.  MCU_INT_STA bits 13..16 latch violations
		 * regardless of whether the corresponding interrupt is enabled.  Read
		 * them without clearing the W1C status so a payload-stage stop cannot be
		 * mistaken for firmware or command backpressure.
		 */
		for (j = 0; j < ARRAY_SIZE(dma_ranges); j++)
			dma_ranges[j] = mt76_rr(dev,
				MT_WFDMA0(0x0b0 + (j / 2) * 0x10 + (j % 2) * 8));
		mcu_int_sta = mt76_rr(dev, MT_WFDMA0(0x110));
		mcu_int_ena = mt76_rr(dev, MT_WFDMA0(0x114));
		timeout_cfg = mt76_rr(dev, MT_WFDMA0(0x230));
		misc_cfg = mt76_rr(dev, MT_WFDMA0(0x234));
		glo_cfg2 = mt76_rr(dev, MT_WFDMA0(0x25c));
		dmashdl_status02 = mt76_rr(dev, MT_DMA_SHDL(0x0c8));
		dmashdl_status = mt76_rr(dev, MT_DMA_SHDL(0x100));
		dmashdl_status0 = mt76_rr(dev, MT_DMA_SHDL(0x110));
		for (j = 0; j < ARRAY_SIZE(dmashdl_gp); j++)
			dmashdl_gp[j] = mt76_rr(dev,
				MT_DMA_SHDL(0x140 + j * 4));
		for (j = 0; j < ARRAY_SIZE(dmashdl_pkt); j++)
			dmashdl_pkt[j] = mt76_rr(dev,
				MT_DMA_SHDL(0x180 + j * 4));
		pse_queue_empty = mt76_rr(dev, MT_PSE_BASE + 0x0b0);
		pse_refill = mt76_rr(dev, MT_PSE_BASE + 0x108);
		for (j = 0; j < ARRAY_SIZE(pse_group); j++)
			pse_group[j] = mt76_rr(dev,
				MT_PSE_BASE + 0x110 + j * 4);
		for (j = 0; j < ARRAY_SIZE(pse_enq); j++)
			pse_enq[j] = mt76_rr(dev,
				MT_PSE_BASE + 0x1f0 + j * 4);
		pse_wfdma_buf = mt76_rr(dev, MT_PSE_BASE + 0x250);
		pse_misc_func = mt76_rr(dev, MT_PSE_BASE + 0x254);
		pse_ckg = mt76_rr(dev, MT_PSE_BASE + 0x258);
		for (j = 0; j < ARRAY_SIZE(pse_seek); j++)
			pse_seek[j] = mt76_rr(dev,
				MT_PSE_BASE + 0x3d0 + j * 4);
		/* Failure-only snapshots of the WFDMA-to-AXI bridge.  The first
		 * runtime packet stops between payload fetch and PSE submission, so
		 * two samples distinguish an exhausted/non-retiring read-credit state
		 * from a bridge response error without perturbing the live path.
		 */
		for (k = 0; k < ARRAY_SIZE(r2a); k++) {
			for (j = 0; j < ARRAY_SIZE(r2a[k]); j++)
				r2a[k][j] = mt76_rr(dev,
					MT_WFDMA_EXT_CSR(0x500 + j * 4));
			if (!k)
				udelay(100);
		}
		dev_err(mdev->dev,
			"J700_MT7932_TX17_DRAIN_FAIL: phase=%s hw=%u head=%u tail=%u cidx=%u didx=%u queued=%u bounce_used=%u ddone=0x%06x glo=0x%08x int=0x%08x\n",
			phase, q->hw_idx, q->head, q->tail,
			readl(&q->regs->cpu_idx), readl(&q->regs->dma_idx), live,
			READ_ONCE(q->tx_bounce_next), ddone,
			mt76_rr(dev, MT_WFDMA0_GLO_CFG),
			mt76_rr(dev, MT_WFDMA0_HOST_INT_STA));
		dev_err(mdev->dev,
			"J700_MT7932_TX17_STALL_FW_STATE: pc=%08x/%08x/%08x/%08x/%08x/%08x/%08x/%08x misc=%08x/%08x/%08x/%08x/%08x/%08x/%08x/%08x rgu=%08x/%08x/%08x/%08x/%08x/%08x/%08x/%08x\n",
			pc[0], pc[1], pc[2], pc[3], pc[4], pc[5], pc[6], pc[7],
			misc[0], misc[1], misc[2], misc[3], misc[4], misc[5],
			misc[6], misc[7], rgu[0], rgu[1], rgu[2], rgu[3],
			rgu[4], rgu[5], rgu[6], rgu[7]);
		dev_err(mdev->dev,
			"J700_MT7932_TX17_STALL_PSE: n9=0x%08x err=0x%08x err1=0x%08x freepg=0x%08x hif0_pg=0x%08x cpu_pg=0x%08x irq_ctl0=0x%08x irq_ctl1=0x%08x\n",
			pse_n9, pse_err, pse_err1, pse_freepg, pse_hif0_pg,
			pse_cpu_pg, irq_ctl0, irq_ctl1);
		dev_err(mdev->dev,
			"J700_MT7932_TX17_PREFETCH_STATE: sta0=0x%08x q_vid=0x%02x dmad_vld=0x%02x sta1=0x%08x q_pause=0x%02x ring=17 dmad=%08x/%08x/%08x/%08x blocked=%08x/%08x/%08x/%08x match=%u source=mt7933-coda\n",
			tx_pf_sta0, (u32)FIELD_GET(GENMASK(7, 0), tx_pf_sta0),
			(u32)FIELD_GET(GENMASK(23, 16), tx_pf_sta0), tx_pf_sta1,
			(u32)FIELD_GET(GENMASK(23, 16), tx_pf_sta1), tx_pf_dmad[0],
			tx_pf_dmad[1], tx_pf_dmad[2], tx_pf_dmad[3],
			le32_to_cpu(READ_ONCE(blocked->buf0)),
			le32_to_cpu(READ_ONCE(blocked->ctrl)),
			le32_to_cpu(READ_ONCE(blocked->buf1)),
			le32_to_cpu(READ_ONCE(blocked->info)), tx_pf_match);
		dev_err(mdev->dev,
			"J700_MT7932_TX17_ENGINE_STATE: tx_dbg=0x%08x/0x%08x pause=0x%08x/0x%08x hif_busy=0x%08x err_int=0x%08x glo_ext=0x%08x/0x%08x/0x%08x info=0x%08x/0x%08x source=mt7933-coda\n",
			tx_dbg0, tx_dbg1, pause0, pause1, hif_busy, err_int,
			glo_ext0, glo_ext1, glo_ext2, info, info_ext);
		dev_err(mdev->dev,
			"J700_MT7932_EXT_WRAP_STATE: top=%08x tick=%08x swrst=%08x dbgsel=%08x hifmisc=%08x slpprot=%08x vio=%08x dcm=%08x apb0=%08x apb1=%08x apbdbg=%08x dispatch=%08x freq=%08x source=mt7933-coda\n",
			ext_wrap[0], ext_wrap[1], ext_wrap[2], ext_wrap[3],
			ext_wrap[4], ext_wrap[5], ext_wrap[6], ext_wrap[7],
			ext_wrap[8], ext_wrap[9], ext_wrap[10], ext_wrap[11],
			ext_wrap[12]);
		for (j = 0; j < ARRAY_SIZE(wfdma_aux); j++)
			dev_err(mdev->dev,
				 "J700_MT7932_WFDMA_AUX_STATE: offset=0x%03x value=0x%08x source=mt7933-coda\n",
				 wfdma_aux_offsets[j], wfdma_aux[j]);
		for (j = 0; j < ARRAY_SIZE(tx_bkrs); j++)
			dev_err(mdev->dev,
				 "J700_MT7932_TX_BKRS_STATE: ring=%u ctrl=%08x/%08x/%08x pf_didx=%u pld_didx=%u disp_didx=%u disp_cidx=%u pld_cidx=%u empty=pld:%u,pf:%u,pf_dq:%u disp_vld=%u source=connac3-coda\n",
				 16 + j, tx_bkrs[j][0], tx_bkrs[j][1],
				 tx_bkrs[j][2], tx_bkrs[j][0] >> 16,
				 tx_bkrs[j][0] & 0xffff,
				 tx_bkrs[j][1] >> 16,
				 tx_bkrs[j][1] & 0xffff,
				 tx_bkrs[j][2] & 0xffff,
				 !!(tx_bkrs[j][2] & BIT(16)),
				 !!(tx_bkrs[j][2] & BIT(17)),
				 !!(tx_bkrs[j][2] & BIT(18)),
				 !!(tx_bkrs[j][2] & BIT(19)));
		dev_err(mdev->dev,
			"J700_MT7932_DMA_RANGE_STATE: tx_dmad=%08x..%08x rx_dmad=%08x..%08x tx_pld=%08x..%08x rx_pld=%08x..%08x mcu_int=0x%08x ena=0x%08x range_err=tx_dmad:%u,rx_dmad:%u,tx_pld:%u,rx_pld:%u fifo_ovf=0x%x timeout=0x%08x misc=0x%08x glo2=0x%08x source=mt7933-coda\n",
			dma_ranges[0], dma_ranges[1], dma_ranges[2], dma_ranges[3],
			dma_ranges[4], dma_ranges[5], dma_ranges[6], dma_ranges[7],
			mcu_int_sta, mcu_int_ena, !!(mcu_int_sta & BIT(13)),
			!!(mcu_int_sta & BIT(14)), !!(mcu_int_sta & BIT(15)),
			!!(mcu_int_sta & BIT(16)),
			(u32)FIELD_GET(GENMASK(12, 10), mcu_int_sta),
			timeout_cfg, misc_cfg, glo_cfg2);
		dev_err(mdev->dev,
			"J700_MT7932_DMASHDL_LIVE_STATE: status02=%08x status=%08x free=%u ffa=%u status0=%08x sw=%08x page=%08x refill=%08x pktmax=%08x source=connac3-coda\n",
			dmashdl_status02, dmashdl_status,
			(u32)FIELD_GET(GENMASK(27, 16), dmashdl_status),
			(u32)FIELD_GET(GENMASK(11, 0), dmashdl_status),
			dmashdl_status0, mt76_rr(dev, MT_DMASHDL_SW_CONTROL),
			mt76_rr(dev, MT_DMASHDL_PAGE),
			mt76_rr(dev, MT_DMASHDL_REFILL),
			mt76_rr(dev, MT_DMASHDL_PKT_MAX_SIZE));
		for (j = 0; j < ARRAY_SIZE(dmashdl_pkt); j++)
			dev_err(mdev->dev,
				"J700_MT7932_DMASHDL_GROUP_STATE: pair=%u gp_even=%08x gp_odd=%08x pkt=%08x ask_even=%u in_even=%u ask_odd=%u in_odd=%u source=connac3-coda\n",
				j, dmashdl_gp[j * 2], dmashdl_gp[j * 2 + 1],
				dmashdl_pkt[j], dmashdl_pkt[j] & 0xff,
				(dmashdl_pkt[j] >> 8) & 0xff,
				(dmashdl_pkt[j] >> 16) & 0xff,
				(dmashdl_pkt[j] >> 24) & 0xff);
		dev_err(mdev->dev,
			"J700_MT7932_PSE_LIVE_STATE: queue_empty=%08x refill=%08x groups=%08x/%08x/%08x/%08x/%08x/%08x/%08x/%08x/%08x/%08x enq=%08x/%08x/%08x/%08x/%08x source=mt7933-coda\n",
			pse_queue_empty, pse_refill, pse_group[0], pse_group[1],
			pse_group[2], pse_group[3], pse_group[4], pse_group[5],
			pse_group[6], pse_group[7], pse_group[8], pse_group[9],
			pse_enq[0], pse_enq[1], pse_enq[2], pse_enq[3],
			pse_enq[4]);
		dev_err(mdev->dev,
			"J700_MT7932_PSE_WFDMA_BRIDGE_STATE: buf=%08x rx_act=%u txcmd_vld=%u txs_vld=%u misc=%08x ckg=%08x seek=%08x/%08x/%08x/%08x/%08x/%08x/%08x source=mt7933-coda\n",
			pse_wfdma_buf,
			(u32)FIELD_GET(GENMASK(27, 16), pse_wfdma_buf),
			(u32)FIELD_GET(GENMASK(15, 8), pse_wfdma_buf),
			(u32)FIELD_GET(GENMASK(7, 0), pse_wfdma_buf),
			pse_misc_func, pse_ckg, pse_seek[0], pse_seek[1],
			pse_seek[2], pse_seek[3], pse_seek[4], pse_seek[5],
			pse_seek[6]);
		for (j = 0; j < ARRAY_SIZE(r2a); j++)
			dev_err(mdev->dev,
				 "J700_MT7932_R2A_STATE: sample=%u ctrl=%08x/%08x/%08x/%08x sts=%08x dmard=%08x rd_out=%08x/%08x slp=%08x fsm=%08x/%08x/%08x/%08x source=mt7933-coda\n",
				 j, r2a[j][0], r2a[j][1], r2a[j][2], r2a[j][3],
				 r2a[j][4], r2a[j][5], r2a[j][6], r2a[j][7],
				 r2a[j][8], r2a[j][9], r2a[j][10], r2a[j][11],
				 r2a[j][12]);
		for (j = 0; j < ARRAY_SIZE(wfdma_dbg); j++)
			dev_err(mdev->dev,
				 "J700_MT7932_WFDMA_DBG: selector=0x%03x value=0x%08x source=mediatek-mt7933-vendor\n",
				 0x100 + j, wfdma_dbg[j]);
		for (j = 0; j < ARRAY_SIZE(pc_ring); j += 8)
			dev_err(mdev->dev,
				 "J700_MT7932_TX17_STALL_PC_LR_RING: first=%u pc=%08x/%08x/%08x/%08x/%08x/%08x/%08x/%08x lr=%08x/%08x/%08x/%08x/%08x/%08x/%08x/%08x\n",
				 j, pc_ring[j], pc_ring[j + 1], pc_ring[j + 2],
				 pc_ring[j + 3], pc_ring[j + 4], pc_ring[j + 5],
				 pc_ring[j + 6], pc_ring[j + 7], lr_ring[j],
				 lr_ring[j + 1], lr_ring[j + 2], lr_ring[j + 3],
				 lr_ring[j + 4], lr_ring[j + 5], lr_ring[j + 6],
				 lr_ring[j + 7]);
		for (j = 0; j < live; j++) {
			u32 idx = (q->tail + j) % q->ndesc;
			struct mt76_queue_entry *entry = &q->entry[idx];
			dma_addr_t dma = entry->dma_addr[0];
			u64 pool_bytes = (u64)q->tx_bounce_entries *
					 q->tx_bounce_stride;

			if (q->tx_bounce_buf && q->tx_bounce_stride &&
			    dma >= q->tx_bounce_dma &&
			    dma - q->tx_bounce_dma < pool_bytes) {
				u32 slot = div_u64(dma - q->tx_bounce_dma,
						   q->tx_bounce_stride);
				const u8 *payload = q->tx_bounce_buf +
						    slot * q->tx_bounce_stride;
				u16 txd_bytes = min_t(u16, entry->dma_len[0], 64);
				u16 body_bytes = entry->dma_len[0] > 64 ?
					min_t(u16, entry->dma_len[0] - 64, 16) : 0;

				dev_err(mdev->dev,
					"J700_MT7932_TX17_LIVE_DESC: order=%u idx=%u slot=%u ctrl=0x%08x info=0x%08x buf0=0x%08x buf1=0x%08x dma_len=%u txd=%*ph body=%*ph\n",
					j, idx, slot,
					le32_to_cpu(READ_ONCE(q->desc[idx].ctrl)),
					le32_to_cpu(READ_ONCE(q->desc[idx].info)),
					le32_to_cpu(READ_ONCE(q->desc[idx].buf0)),
					le32_to_cpu(READ_ONCE(q->desc[idx].buf1)),
					entry->dma_len[0], txd_bytes, payload,
					body_bytes, payload + 64);
			} else {
				dev_err(mdev->dev,
					"J700_MT7932_TX17_LIVE_DESC: order=%u idx=%u slot=invalid ctrl=0x%08x info=0x%08x buf0=0x%08x buf1=0x%08x dma_len=%u\n",
					j, idx,
					le32_to_cpu(READ_ONCE(q->desc[idx].ctrl)),
					le32_to_cpu(READ_ONCE(q->desc[idx].info)),
					le32_to_cpu(READ_ONCE(q->desc[idx].buf0)),
					le32_to_cpu(READ_ONCE(q->desc[idx].buf1)),
					entry->dma_len[0]);
			}
		}
	}
	return -ETIMEDOUT;
}

static int mt7932_mcu_wait_calibration_space(struct mt792x_dev *dev)
{
	struct mt76_dev *mdev = &dev->mt76;
	struct mt76_queue *q = mdev->q_mcu[MT_MCUQ_WM];
	int i;

	if (!q || !mdev->queue_ops->tx_cleanup)
		return -EOPNOTSUPP;

	for (i = 0; i < 5000; i++) {
		mdev->queue_ops->tx_cleanup(mdev, q, false);
		if (READ_ONCE(q->queued) + 1 < q->ndesc - 1)
			return 0;
		usleep_range(100, 200);
	}

	dev_err(mdev->dev,
		"J700_MT7932_CAL_TX17_SPACE_FAIL: hw=%u head=%u tail=%u queued=%d depth=%u\n",
		q->hw_idx, q->head, q->tail, q->queued, q->ndesc);
	return -ETIMEDOUT;
}

int mt7932_mcu_load_one_time_cal(struct mt792x_dev *dev)
{
	const struct mt7932_cal_stream_header *header;
	const struct firmware *firmware;
	size_t offset;
	u16 count, i;
	int cmd = MCU_CMD(ONE_TIME_CAL);
	int ret;

	if (!is_mt7932(&dev->mt76))
		return -EOPNOTSUPP;
	ret = request_firmware(&firmware, MT7932_ONE_TIME_CAL, dev->mt76.dev);
	if (ret)
		return ret;
	ret = mt7932_validate_cal_stream(firmware, &count);
	if (ret)
		goto release_firmware;
	ret = mt7932_mcu_drain_tx17(dev, "cal-pre", true);
	if (ret)
		goto release_firmware;

	header = (const void *)firmware->data;
	offset = sizeof(*header);
	for (i = 0; i < count; i++) {
		const u8 *data;
		u16 length = get_unaligned_le16(firmware->data + offset);

		offset += 4;
		data = firmware->data + offset;
		ret = mt7932_mcu_wait_calibration_space(dev);
		if (ret)
			goto release_firmware;
		ret = mt76_mcu_send_msg(&dev->mt76, cmd, data, length, false);
		if (ret)
			goto release_firmware;
		if (dev->mt76.queue_ops->tx_cleanup)
			dev->mt76.queue_ops->tx_cleanup(&dev->mt76,
				dev->mt76.q_mcu[MT_MCUQ_WM], false);
		offset += length;
	}
	ret = mt7932_mcu_drain_tx17(dev, "cal-post", true);
	if (ret)
		goto release_firmware;
	dev_info(dev->mt76.dev,
		 "J700_MT7932_ONE_TIME_CAL_PASS: cid=d6 records=%u wire_header=20 data_max=1080 rf_enabled=0\n",
		 count);

release_firmware:
	release_firmware(firmware);
	return ret;
}
EXPORT_SYMBOL_GPL(mt7932_mcu_load_one_time_cal);

int mt7921_mcu_uni_bss_ps(struct mt792x_dev *dev, struct ieee80211_vif *vif)
{
	struct mt792x_vif *mvif = (struct mt792x_vif *)vif->drv_priv;
	struct {
		struct {
			u8 bss_idx;
			u8 pad[3];
		} __packed hdr;
		struct ps_tlv {
			__le16 tag;
			__le16 len;
			u8 ps_state; /* 0: device awake
				      * 1: static power save
				      * 2: dynamic power saving
				      * 3: enter TWT power saving
				      * 4: leave TWT power saving
				      */
			u8 pad[3];
		} __packed ps;
	} __packed ps_req = {
		.hdr = {
			.bss_idx = mvif->bss_conf.mt76.idx,
		},
		.ps = {
			.tag = cpu_to_le16(UNI_BSS_INFO_PS),
			.len = cpu_to_le16(sizeof(struct ps_tlv)),
			.ps_state = vif->cfg.ps ? 2 : 0,
		},
	};

	if (vif->type != NL80211_IFTYPE_STATION)
		return -EOPNOTSUPP;

	return mt76_mcu_send_msg(&dev->mt76, MCU_UNI_CMD(BSS_INFO_UPDATE),
				 &ps_req, sizeof(ps_req), true);
}

static int
mt7921_mcu_uni_bss_bcnft(struct mt792x_dev *dev, struct ieee80211_vif *vif,
			 bool enable)
{
	struct mt792x_vif *mvif = (struct mt792x_vif *)vif->drv_priv;
	struct {
		struct {
			u8 bss_idx;
			u8 pad[3];
		} __packed hdr;
		struct bcnft_tlv {
			__le16 tag;
			__le16 len;
			__le16 bcn_interval;
			u8 dtim_period;
			u8 pad;
		} __packed bcnft;
	} __packed bcnft_req = {
		.hdr = {
			.bss_idx = mvif->bss_conf.mt76.idx,
		},
		.bcnft = {
			.tag = cpu_to_le16(UNI_BSS_INFO_BCNFT),
			.len = cpu_to_le16(sizeof(struct bcnft_tlv)),
			.bcn_interval = cpu_to_le16(vif->bss_conf.beacon_int),
			.dtim_period = vif->bss_conf.dtim_period,
		},
	};

	if (vif->type != NL80211_IFTYPE_STATION)
		return 0;

	return mt76_mcu_send_msg(&dev->mt76, MCU_UNI_CMD(BSS_INFO_UPDATE),
				 &bcnft_req, sizeof(bcnft_req), true);
}

int
mt7921_mcu_set_bss_pm(struct mt792x_dev *dev, struct ieee80211_vif *vif,
		      bool enable)
{
	struct mt792x_vif *mvif = (struct mt792x_vif *)vif->drv_priv;
	struct {
		u8 bss_idx;
		u8 dtim_period;
		__le16 aid;
		__le16 bcn_interval;
		__le16 atim_window;
		u8 uapsd;
		u8 bmc_delivered_ac;
		u8 bmc_triggered_ac;
		u8 pad;
	} req = {
		.bss_idx = mvif->bss_conf.mt76.idx,
		.aid = cpu_to_le16(vif->cfg.aid),
		.dtim_period = vif->bss_conf.dtim_period,
		.bcn_interval = cpu_to_le16(vif->bss_conf.beacon_int),
	};
	struct {
		u8 bss_idx;
		u8 pad[3];
	} req_hdr = {
		.bss_idx = mvif->bss_conf.mt76.idx,
	};
	int err;

	err = mt76_mcu_send_msg(&dev->mt76, MCU_CE_CMD(SET_BSS_ABORT),
				&req_hdr, sizeof(req_hdr), false);
	if (err < 0 || !enable)
		return err;

	return mt76_mcu_send_msg(&dev->mt76, MCU_CE_CMD(SET_BSS_CONNECTED),
				 &req, sizeof(req), false);
}

int mt7921_mcu_sta_update(struct mt792x_dev *dev, struct ieee80211_sta *sta,
			  struct ieee80211_vif *vif, bool enable,
			  enum mt76_sta_info_state state)
{
	struct mt792x_vif *mvif = (struct mt792x_vif *)vif->drv_priv;
	int rssi = -ewma_rssi_read(&mvif->bss_conf.rssi);
	struct mt76_sta_cmd_info info = {
		.sta = sta,
		.vif = vif,
		.enable = enable,
		.cmd = MCU_UNI_CMD(STA_REC_UPDATE),
		.state = state,
		.offload_fw = true,
		.rcpi = to_rcpi(rssi),
	};
	struct mt792x_sta *msta;

	msta = sta ? (struct mt792x_sta *)sta->drv_priv : NULL;
	info.wcid = msta ? &msta->deflink.wcid : &mvif->sta.deflink.wcid;
	info.newly = msta ? state != MT76_STA_INFO_STATE_ASSOC : true;

	return mt76_connac_mcu_sta_cmd(&dev->mphy, &info);
}

int mt7921_mcu_set_beacon_filter(struct mt792x_dev *dev,
				 struct ieee80211_vif *vif,
				 bool enable)
{
#define MT7921_FIF_BIT_CLR		BIT(1)
#define MT7921_FIF_BIT_SET		BIT(0)
	int err;

	if (enable) {
		err = mt7921_mcu_uni_bss_bcnft(dev, vif, true);
		if (err)
			return err;

		err = mt7921_mcu_set_rxfilter(dev, 0,
					      MT7921_FIF_BIT_SET,
					      MT_WF_RFCR_DROP_OTHER_BEACON);
		if (err)
			return err;

		return 0;
	}

	err = mt7921_mcu_set_bss_pm(dev, vif, false);
	if (err)
		return err;

	err = mt7921_mcu_set_rxfilter(dev, 0,
				      MT7921_FIF_BIT_CLR,
				      MT_WF_RFCR_DROP_OTHER_BEACON);
	if (err)
		return err;

	return 0;
}

int mt7921_get_txpwr_info(struct mt792x_dev *dev, struct mt7921_txpwr *txpwr)
{
	struct mt7921_txpwr_event *event;
	struct mt7921_txpwr_req req = {
		.dbdc_idx = 0,
	};
	struct sk_buff *skb;
	int ret;

	ret = mt76_mcu_send_and_get_msg(&dev->mt76, MCU_CE_CMD(GET_TXPWR),
					&req, sizeof(req), true, &skb);
	if (ret)
		return ret;

	event = (struct mt7921_txpwr_event *)skb->data;
	WARN_ON(skb->len != le16_to_cpu(event->len));
	memcpy(txpwr, &event->txpwr, sizeof(event->txpwr));

	dev_kfree_skb(skb);

	return 0;
}

int mt7921_mcu_set_sniffer(struct mt792x_dev *dev, struct ieee80211_vif *vif,
			   bool enable)
{
	struct mt76_vif_link *mvif = (struct mt76_vif_link *)vif->drv_priv;
	struct {
		struct {
			u8 band_idx;
			u8 pad[3];
		} __packed hdr;
		struct sniffer_enable_tlv {
			__le16 tag;
			__le16 len;
			u8 enable;
			u8 pad[3];
		} __packed enable;
	} req = {
		.hdr = {
			.band_idx = mvif->band_idx,
		},
		.enable = {
			.tag = cpu_to_le16(0),
			.len = cpu_to_le16(sizeof(struct sniffer_enable_tlv)),
			.enable = enable,
		},
	};

	return mt76_mcu_send_msg(&dev->mt76, MCU_UNI_CMD(SNIFFER), &req, sizeof(req),
				 true);
}

int mt7921_mcu_config_sniffer(struct mt792x_vif *vif,
			      struct ieee80211_chanctx_conf *ctx)
{
	struct cfg80211_chan_def *chandef = &ctx->def;
	int freq1 = chandef->center_freq1, freq2 = chandef->center_freq2;
	static const u8 ch_band[] = {
		[NL80211_BAND_2GHZ] = 1,
		[NL80211_BAND_5GHZ] = 2,
		[NL80211_BAND_6GHZ] = 3,
	};
	static const u8 ch_width[] = {
		[NL80211_CHAN_WIDTH_20_NOHT] = 0,
		[NL80211_CHAN_WIDTH_20] = 0,
		[NL80211_CHAN_WIDTH_40] = 0,
		[NL80211_CHAN_WIDTH_80] = 1,
		[NL80211_CHAN_WIDTH_160] = 2,
		[NL80211_CHAN_WIDTH_80P80] = 3,
		[NL80211_CHAN_WIDTH_5] = 4,
		[NL80211_CHAN_WIDTH_10] = 5,
		[NL80211_CHAN_WIDTH_320] = 6,
	};
	struct {
		struct {
			u8 band_idx;
			u8 pad[3];
		} __packed hdr;
		struct config_tlv {
			__le16 tag;
			__le16 len;
			u16 aid;
			u8 ch_band;
			u8 bw;
			u8 control_ch;
			u8 sco;
			u8 center_ch;
			u8 center_ch2;
			u8 drop_err;
			u8 pad[3];
		} __packed tlv;
	} __packed req = {
		.hdr = {
			.band_idx = vif->bss_conf.mt76.band_idx,
		},
		.tlv = {
			.tag = cpu_to_le16(1),
			.len = cpu_to_le16(sizeof(req.tlv)),
			.control_ch = chandef->chan->hw_value,
			.center_ch = ieee80211_frequency_to_channel(freq1),
			.drop_err = 1,
		},
	};
	if (chandef->chan->band < ARRAY_SIZE(ch_band))
		req.tlv.ch_band = ch_band[chandef->chan->band];
	if (chandef->width < ARRAY_SIZE(ch_width))
		req.tlv.bw = ch_width[chandef->width];

	if (freq2)
		req.tlv.center_ch2 = ieee80211_frequency_to_channel(freq2);

	if (req.tlv.control_ch < req.tlv.center_ch)
		req.tlv.sco = 1; /* SCA */
	else if (req.tlv.control_ch > req.tlv.center_ch)
		req.tlv.sco = 3; /* SCB */

	return mt76_mcu_send_msg(vif->phy->mt76->dev, MCU_UNI_CMD(SNIFFER),
				 &req, sizeof(req), true);
}

int
mt7921_mcu_uni_add_beacon_offload(struct mt792x_dev *dev,
				  struct ieee80211_hw *hw,
				  struct ieee80211_vif *vif,
				  bool enable)
{
	struct mt792x_vif *mvif = (struct mt792x_vif *)vif->drv_priv;
	struct mt76_wcid *wcid = &dev->mt76.global_wcid;
	struct ieee80211_mutable_offsets offs;
	struct {
		struct req_hdr {
			u8 bss_idx;
			u8 pad[3];
		} __packed hdr;
		struct bcn_content_tlv {
			__le16 tag;
			__le16 len;
			__le16 tim_ie_pos;
			__le16 csa_ie_pos;
			__le16 bcc_ie_pos;
			/* 0: disable beacon offload
			 * 1: enable beacon offload
			 * 2: update probe respond offload
			 */
			u8 enable;
			/* 0: legacy format (TXD + payload)
			 * 1: only cap field IE
			 */
			u8 type;
			__le16 pkt_len;
			u8 pkt[512];
		} __packed beacon_tlv;
	} req = {
		.hdr = {
			.bss_idx = mvif->bss_conf.mt76.idx,
		},
		.beacon_tlv = {
			.tag = cpu_to_le16(UNI_BSS_INFO_BCN_CONTENT),
			.len = cpu_to_le16(sizeof(struct bcn_content_tlv)),
			.enable = enable,
		},
	};
	struct sk_buff *skb;

	/* support enable/update process only
	 * disable flow would be handled in bss stop handler automatically
	 */
	if (!enable)
		return -EOPNOTSUPP;

	skb = ieee80211_beacon_get_template(mt76_hw(dev), vif, &offs, 0);
	if (!skb)
		return -EINVAL;

	if (skb->len > 512 - MT_TXD_SIZE) {
		dev_err(dev->mt76.dev, "beacon size limit exceed\n");
		dev_kfree_skb(skb);
		return -EINVAL;
	}

	mt76_connac2_mac_write_txwi(&dev->mt76, (__le32 *)(req.beacon_tlv.pkt),
				    skb, wcid, NULL, 0, 0, BSS_CHANGED_BEACON);
	memcpy(req.beacon_tlv.pkt + MT_TXD_SIZE, skb->data, skb->len);
	req.beacon_tlv.pkt_len = cpu_to_le16(MT_TXD_SIZE + skb->len);
	req.beacon_tlv.tim_ie_pos = cpu_to_le16(MT_TXD_SIZE + offs.tim_offset);

	if (offs.cntdwn_counter_offs[0]) {
		u16 csa_offs;

		csa_offs = MT_TXD_SIZE + offs.cntdwn_counter_offs[0] - 4;
		req.beacon_tlv.csa_ie_pos = cpu_to_le16(csa_offs);
	}
	dev_kfree_skb(skb);

	return mt76_mcu_send_msg(&dev->mt76, MCU_UNI_CMD(BSS_INFO_UPDATE),
				 &req, sizeof(req), true);
}

static
int __mt7921_mcu_set_clc(struct mt792x_dev *dev, u8 *alpha2,
			 enum environment_cap env_cap,
			 struct mt7921_clc *clc,
			 u8 idx)
{
#define CLC_CAP_EVT_EN BIT(0)
#define CLC_CAP_DTS_EN BIT(1)
	struct sk_buff *skb, *ret_skb = NULL;
	struct {
		u8 ver;
		u8 pad0;
		__le16 len;
		u8 idx;
		u8 env;
		u8 acpi_conf;
		u8 cap;
		u8 alpha2[2];
		u8 type[2];
		u8 env_6g;
		u8 mtcl_conf;
		u8 rsvd[62];
	} __packed req = {
		.ver = 1,
		.idx = idx,
		.env = env_cap,
		.env_6g = dev->phy.power_type,
		.acpi_conf = mt792x_acpi_get_flags(&dev->phy),
		.mtcl_conf = mt792x_acpi_get_mtcl_conf(&dev->phy, alpha2),
	};
	int ret, valid_cnt = 0;
	u32 buf_len = 0;
	u8 *pos;

	if (!clc)
		return 0;

	if (dev->phy.chip_cap & MT792x_CHIP_CAP_CLC_EVT_EN)
		req.cap |= CLC_CAP_EVT_EN;
	if (mt76_find_power_limits_node(&dev->mt76))
		req.cap |= CLC_CAP_DTS_EN;

	buf_len = le32_to_cpu(clc->len) - sizeof(*clc);
	pos = clc->data;
	while (buf_len > 16) {
		struct mt7921_clc_rule *rule = (struct mt7921_clc_rule *)pos;
		u16 len = le16_to_cpu(rule->len);
		u16 offset = len + sizeof(*rule);

		if (buf_len < offset)
			break;

		pos += offset;
		buf_len -= offset;
		if (rule->alpha2[0] != alpha2[0] ||
		    rule->alpha2[1] != alpha2[1])
			continue;

		memcpy(req.alpha2, rule->alpha2, 2);
		memcpy(req.type, rule->type, 2);

		req.len = cpu_to_le16(sizeof(req) + len);
		skb = __mt76_mcu_msg_alloc(&dev->mt76, &req,
					   le16_to_cpu(req.len),
					   sizeof(req), GFP_KERNEL);
		if (!skb)
			return -ENOMEM;
		skb_put_data(skb, rule->data, len);

		ret = mt76_mcu_skb_send_and_get_msg(&dev->mt76, skb,
						    MCU_CE_CMD(SET_CLC),
						    !!(req.cap & CLC_CAP_EVT_EN),
						    &ret_skb);
		if (ret < 0)
			return ret;

		if (ret_skb) {
			struct mt7921_clc_info_tlv *info;

			info = (struct mt7921_clc_info_tlv *)(ret_skb->data + 4);
			dev->phy.clc_chan_conf = info->chan_conf;
			dev_kfree_skb(ret_skb);
		}

		valid_cnt++;
	}

	if (!valid_cnt)
		return -ENOENT;

	return 0;
}

int mt7921_mcu_set_clc(struct mt792x_dev *dev, u8 *alpha2,
		       enum environment_cap env_cap)
{
	struct mt792x_phy *phy = (struct mt792x_phy *)&dev->phy;
	int i, ret;

	/* submit all clc config */
	for (i = 0; i < ARRAY_SIZE(phy->clc); i++) {
		ret = __mt7921_mcu_set_clc(dev, alpha2, env_cap,
					   phy->clc[i], i);

		/* If no country found, set "00" as default */
		if (ret == -ENOENT)
			ret = __mt7921_mcu_set_clc(dev, "00",
						   ENVIRON_INDOOR,
						   phy->clc[i], i);
		if (ret < 0)
			return ret;
	}
	return 0;
}

int mt7921_mcu_get_temperature(struct mt792x_phy *phy)
{
	struct mt792x_dev *dev = phy->dev;
	struct {
		u8 ctrl_id;
		u8 action;
		u8 band_idx;
		u8 rsv[5];
	} req = {
		.ctrl_id = THERMAL_SENSOR_TEMP_QUERY,
		.band_idx = phy->mt76->band_idx,
	};

	return mt76_mcu_send_msg(&dev->mt76, MCU_EXT_CMD(THERMAL_CTRL), &req,
				 sizeof(req), true);
}

int mt7921_mcu_wf_rf_pin_ctrl(struct mt792x_phy *phy, u8 action)
{
	struct mt792x_dev *dev = phy->dev;
	struct {
		u8 action;
		u8 value;
	} req = {
		.action = action,
		.value = 0,
	};

	return mt76_mcu_send_msg(&dev->mt76, MCU_EXT_CMD(WF_RF_PIN_CTRL), &req,
				 sizeof(req), action ? true : false);
}

int mt7921_mcu_set_rxfilter(struct mt792x_dev *dev, u32 fif,
			    u8 bit_op, u32 bit_map)
{
	struct {
		u8 rsv[4];
		u8 mode;
		u8 rsv2[3];
		__le32 fif;
		__le32 bit_map; /* bit_* for bitmap update */
		u8 bit_op;
		u8 pad[51];
	} __packed data = {
		.mode = fif ? 1 : 2,
		.fif = cpu_to_le32(fif),
		.bit_map = cpu_to_le32(bit_map),
		.bit_op = bit_op,
	};

	return mt76_mcu_send_msg(&dev->mt76, MCU_CE_CMD(SET_RX_FILTER),
				 &data, sizeof(data), false);
}

int mt7921_mcu_set_rssimonitor(struct mt792x_dev *dev, struct ieee80211_vif *vif)
{
	struct mt792x_vif *mvif = (struct mt792x_vif *)vif->drv_priv;
	struct {
		u8 enable;
		s8 cqm_rssi_high;
		s8 cqm_rssi_low;
		u8 bss_idx;
		u16 duration;
		u8 rsv2[2];
	} __packed data = {
		.enable = vif->cfg.assoc,
		.cqm_rssi_high = vif->bss_conf.cqm_rssi_thold + vif->bss_conf.cqm_rssi_hyst,
		.cqm_rssi_low = vif->bss_conf.cqm_rssi_thold - vif->bss_conf.cqm_rssi_hyst,
		.bss_idx = mvif->bss_conf.mt76.idx,
	};

	return mt76_mcu_send_msg(&dev->mt76, MCU_CE_CMD(RSSI_MONITOR),
				 &data, sizeof(data), false);
}
