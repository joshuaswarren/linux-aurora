// SPDX-License-Identifier: BSD-3-Clause-Clear
/* Copyright (C) 2020 MediaTek Inc. */

#include <linux/firmware.h>
#include <linux/delay.h>
#include <linux/ktime.h>
#include <linux/pci.h>
#include <linux/workqueue.h>
#include "mt76_connac2_mac.h"
#include "mt76_connac_mcu.h"
#include "mt792x_regs.h"

#define MT7932_SEC_BOOT_MODE		0x7c00e24c
#define MT7932_SEC_BOOT_STATUS		0x00040060
#define MT7932_SEC_BOOT_RELEASE		0x00040260
#define MT7932_SEC_BOOT_MODE_ENTER	0x18451807
#define MT7932_SEC_BOOT_MODE_EXIT	0x1845184f
#define MT7932_FW_START_READY_PERIOD_MS	10
#define MT7932_FW_START_READY_STOP_US	50000
#define MT7932_FW_PCIE_BUSDEV		0x74030144
#define MT7932_FW_PCIE_COMMAND		0x74031004
#define MT7932_FW_ENTRY_MARKER		0x7c053a5c
#define MT7932_PCI_MCU_STATE		0x48c
#define MT7932_PCI_MCU_STATE_MASK	GENMASK(19, 16)
#define MT7932_FW_SETTLE_MIN_US		20000
#define MT7932_FW_SETTLE_MAX_US		21000

static int mt7932_sec_protect_fwdl(struct mt76_dev *dev, bool get)
{
	int ret = 0;
	u32 status = 0;

	__mt76_wr(dev, MT7932_SEC_BOOT_MODE, MT7932_SEC_BOOT_MODE_ENTER);
	if (get) {
		if (!__mt76_poll_msec(dev, MT7932_SEC_BOOT_STATUS,
				      BIT(0), BIT(0), 5000))
			ret = -ETIMEDOUT;
		status = __mt76_rr(dev, MT7932_SEC_BOOT_STATUS);
		if (status == U32_MAX)
			ret = -ENODEV;
	} else {
		__mt76_wr(dev, MT7932_SEC_BOOT_RELEASE, 1);
	}
	__mt76_wr(dev, MT7932_SEC_BOOT_MODE, MT7932_SEC_BOOT_MODE_EXIT);

	dev_info(dev->dev,
		 "J700_MT7932_SEC_PROTECT_FWDL: action=%s status=0x%08x ret=%d\n",
		 get ? "get" : "release", status, ret);

	return ret;
}

int mt76_connac_mcu_start_firmware(struct mt76_dev *dev, u32 addr, u32 option)
{
	struct {
		__le32 option;
		__le32 addr;
	} req = {
		.option = cpu_to_le32(option),
		.addr = cpu_to_le32(addr),
	};

	if (is_mt7932(dev))
		dev_info(dev->dev,
			 "J700_MT7932_FW_START_REQUEST: option=0x%08x addr=0x%08x\n",
			 option, addr);

	return mt76_mcu_send_msg(dev, MCU_CMD(FW_START_REQ), &req,
				 sizeof(req), true);
}
EXPORT_SYMBOL_GPL(mt76_connac_mcu_start_firmware);

int mt76_connac_mcu_read_otp(struct mt76_dev *dev, u32 addr, u8 *value)
{
	struct {
		__le32 addr;
	} req = {
		.addr = cpu_to_le32(addr),
	};
	struct sk_buff *skb;
	int ret;

	ret = mt76_mcu_send_and_get_msg(dev, MCU_CMD(EFUSE_ACCESS),
					&req, sizeof(req), true, &skb);
	if (ret)
		return ret;
	if (skb->len < 4) {
		ret = -EPROTO;
		goto out;
	}

	*value = skb->data[0];
	if (addr == 0x200 || addr == 0x23f)
		dev_info(dev->dev,
			 "J700_MT7932_OTP_READ_PASS: addr=0x%04x value=0x%02x response=%02x/%02x/%02x/%02x len=%u\n",
			 addr, *value, skb->data[0], skb->data[1],
			 skb->data[2], skb->data[3], skb->len);
out:
	dev_kfree_skb(skb);

	return ret;
}
EXPORT_SYMBOL_GPL(mt76_connac_mcu_read_otp);

int mt76_connac_mcu_read_init_reg(struct mt76_dev *dev, u32 addr, u32 *value)
{
	struct {
		u8 query;
		u8 reserved[3];
		__le32 addr;
		__le32 data;
	} __packed req = {
		.query = 0,
		.addr = cpu_to_le32(addr),
	};
	struct {
		__le32 addr;
		__le32 data;
	} __packed *event;
	struct sk_buff *skb;
	int ret;

	ret = mt76_mcu_send_and_get_msg(dev, MCU_CMD(INIT_ACCESS_REG),
					&req, sizeof(req), true, &skb);
	if (ret)
		return ret;
	if (skb->len < sizeof(*event)) {
		ret = -EPROTO;
		goto out;
	}

	event = (void *)skb->data;
	if (le32_to_cpu(event->addr) != addr) {
		dev_err(dev->dev,
			"J700_MT7932_INIT_REG_READ_FAIL: addr=0x%08x expected=0x%08x len=%u\n",
			le32_to_cpu(event->addr), addr, skb->len);
		ret = -EPROTO;
		goto out;
	}

	*value = le32_to_cpu(event->data);
	dev_info(dev->dev,
		 "J700_MT7932_INIT_REG_READ_PASS: addr=0x%08x value=0x%08x\n",
		 addr, *value);
out:
	dev_kfree_skb(skb);

	return ret;
}
EXPORT_SYMBOL_GPL(mt76_connac_mcu_read_init_reg);

int mt76_connac_mcu_patch_sem_ctrl(struct mt76_dev *dev, bool get)
{
	u32 op = get ? (is_mt7932(dev) ? PATCH_SEM_GET_MT7932 :
				       PATCH_SEM_GET) : PATCH_SEM_RELEASE;
	struct {
		__le32 op;
	} req = {
		.op = cpu_to_le32(op),
	};

	if (is_mt7932(dev))
		dev_info(dev->dev,
			 "J700_MT7932_PATCH_SEMAPHORE: action=%s op=%u\n",
			 get ? "get" : "release", op);

	return mt76_mcu_send_msg(dev, MCU_CMD(PATCH_SEM_CONTROL),
				 &req, sizeof(req), true);
}
EXPORT_SYMBOL_GPL(mt76_connac_mcu_patch_sem_ctrl);

int mt76_connac_mcu_start_patch(struct mt76_dev *dev)
{
	struct {
		u8 check_crc;
		u8 reserved[3];
	} req = {
		.check_crc = 0,
	};

	return mt76_mcu_send_msg(dev, MCU_CMD(PATCH_FINISH_REQ),
				 &req, sizeof(req), true);
}
EXPORT_SYMBOL_GPL(mt76_connac_mcu_start_patch);

#define MCU_PATCH_ADDRESS	0x200000

int mt76_connac_mcu_init_download(struct mt76_dev *dev, u32 addr, u32 len,
				  u32 mode)
{
	struct {
		__le32 addr;
		__le32 len;
		__le32 mode;
	} req = {
		.addr = cpu_to_le32(addr),
		.len = cpu_to_le32(len),
		.mode = cpu_to_le32(mode),
	};
	int cmd;

	if ((!is_connac_v1(dev) && addr == MCU_PATCH_ADDRESS) ||
	    (is_connac2(dev) && addr == 0x900000) ||
	    (is_connac3(dev) && (addr == 0x900000 || addr == 0xe0002800)) ||
	    (is_mt799x(dev) && addr == 0x900000))
		cmd = MCU_CMD(PATCH_START_REQ);
	else
		cmd = MCU_CMD(TARGET_ADDRESS_LEN_REQ);

	return mt76_mcu_send_msg(dev, cmd, &req, sizeof(req), true);
}
EXPORT_SYMBOL_GPL(mt76_connac_mcu_init_download);

int mt76_connac_mcu_set_channel_domain(struct mt76_phy *phy)
{
	int len, i, n_max_channels, n_2ch = 0, n_5ch = 0, n_6ch = 0;
	struct mt76_connac_mcu_channel_domain {
		u8 alpha2[4]; /* regulatory_request.alpha2 */
		u8 bw_2g; /* BW_20_40M		0
			   * BW_20M		1
			   * BW_20_40_80M	2
			   * BW_20_40_80_160M	3
			   * BW_20_40_80_8080M	4
			   */
		u8 bw_5g;
		u8 bw_6g;
		u8 pad;
		u8 n_2ch;
		u8 n_5ch;
		u8 n_6ch;
		u8 pad2;
	} __packed hdr = {
		.bw_2g = 0,
		.bw_5g = 3, /* BW_20_40_80_160M */
		.bw_6g = 3,
	};
	struct mt76_connac_mcu_chan {
		__le16 hw_value;
		__le16 pad;
		__le32 flags;
	} __packed channel;
	struct mt76_dev *dev = phy->dev;
	struct ieee80211_channel *chan;
	struct sk_buff *skb;

	n_max_channels = phy->sband_2g.sband.n_channels +
			 phy->sband_5g.sband.n_channels +
			 phy->sband_6g.sband.n_channels;
	len = sizeof(hdr) + n_max_channels * sizeof(channel);

	skb = mt76_mcu_msg_alloc(dev, NULL, len);
	if (!skb)
		return -ENOMEM;

	skb_reserve(skb, sizeof(hdr));

	for (i = 0; i < phy->sband_2g.sband.n_channels; i++) {
		chan = &phy->sband_2g.sband.channels[i];
		if (chan->flags & IEEE80211_CHAN_DISABLED)
			continue;

		channel.hw_value = cpu_to_le16(chan->hw_value);
		channel.flags = cpu_to_le32(chan->flags);
		channel.pad = 0;

		skb_put_data(skb, &channel, sizeof(channel));
		n_2ch++;
	}
	for (i = 0; i < phy->sband_5g.sband.n_channels; i++) {
		chan = &phy->sband_5g.sband.channels[i];
		if (chan->flags & IEEE80211_CHAN_DISABLED)
			continue;

		channel.hw_value = cpu_to_le16(chan->hw_value);
		channel.flags = cpu_to_le32(chan->flags);
		channel.pad = 0;

		skb_put_data(skb, &channel, sizeof(channel));
		n_5ch++;
	}
	for (i = 0; i < phy->sband_6g.sband.n_channels; i++) {
		chan = &phy->sband_6g.sband.channels[i];
		if (chan->flags & IEEE80211_CHAN_DISABLED)
			continue;

		channel.hw_value = cpu_to_le16(chan->hw_value);
		channel.flags = cpu_to_le32(chan->flags);
		channel.pad = 0;

		skb_put_data(skb, &channel, sizeof(channel));
		n_6ch++;
	}

	BUILD_BUG_ON(sizeof(dev->alpha2) > sizeof(hdr.alpha2));
	memcpy(hdr.alpha2, dev->alpha2, sizeof(dev->alpha2));
	hdr.n_2ch = n_2ch;
	hdr.n_5ch = n_5ch;
	hdr.n_6ch = n_6ch;

	memcpy(__skb_push(skb, sizeof(hdr)), &hdr, sizeof(hdr));

	return mt76_mcu_skb_send_msg(dev, skb, MCU_CE_CMD(SET_CHAN_DOMAIN),
				     false);
}
EXPORT_SYMBOL_GPL(mt76_connac_mcu_set_channel_domain);

int mt76_connac_mcu_set_mac_enable(struct mt76_dev *dev, int band, bool enable,
				   bool hdr_trans)
{
	struct {
		u8 enable;
		u8 band;
		u8 rsv[2];
	} __packed req_mac = {
		.enable = enable,
		.band = band,
	};

	/* AppleSunriseWLAN 25G83 has no MT7932 caller for the generic connac2
	 * MAC_INIT_CTRL command before its first scan.  Sending it without an ACK
	 * still changes firmware state and is not equivalent to omitting it.
	 */
	if (is_mt7932(dev)) {
		dev_info(dev->dev,
			 "J700_MT7932_MAC_INIT_CTRL_SKIP: enable=%d band=%d hdr_trans=%d source=apple-25G83 command=0x000046ed sent=0\n",
			 enable, band, hdr_trans);
		return 0;
	}

	return mt76_mcu_send_msg(dev, MCU_EXT_CMD(MAC_INIT_CTRL), &req_mac,
				 sizeof(req_mac), true);
}
EXPORT_SYMBOL_GPL(mt76_connac_mcu_set_mac_enable);

int mt76_connac_mcu_set_vif_ps(struct mt76_dev *dev, struct ieee80211_vif *vif)
{
	struct mt76_vif_link *mvif = (struct mt76_vif_link *)vif->drv_priv;
	struct {
		u8 bss_idx;
		u8 ps_state; /* 0: device awake
			      * 1: static power save
			      * 2: dynamic power saving
			      */
	} req = {
		.bss_idx = mvif->idx,
		.ps_state = vif->cfg.ps ? 2 : 0,
	};

	if (vif->type != NL80211_IFTYPE_STATION)
		return -EOPNOTSUPP;

	return mt76_mcu_send_msg(dev, MCU_CE_CMD(SET_PS_PROFILE),
				 &req, sizeof(req), false);
}
EXPORT_SYMBOL_GPL(mt76_connac_mcu_set_vif_ps);

int mt76_connac_mcu_set_rts_thresh(struct mt76_dev *dev, u32 val, u8 band)
{
	struct {
		u8 prot_idx;
		u8 band;
		u8 rsv[2];
		__le32 len_thresh;
		__le32 pkt_thresh;
	} __packed req = {
		.prot_idx = 1,
		.band = band,
		.len_thresh = cpu_to_le32(val),
		.pkt_thresh = cpu_to_le32(0x2),
	};

	return mt76_mcu_send_msg(dev, MCU_EXT_CMD(PROTECT_CTRL), &req,
				 sizeof(req), true);
}
EXPORT_SYMBOL_GPL(mt76_connac_mcu_set_rts_thresh);

void mt76_connac_mcu_beacon_loss_iter(void *priv, u8 *mac,
				      struct ieee80211_vif *vif)
{
	struct mt76_vif_link *mvif = (struct mt76_vif_link *)vif->drv_priv;
	struct mt76_connac_beacon_loss_event *event = priv;

	if (mvif->idx != event->bss_idx)
		return;

	if (!(vif->driver_flags & IEEE80211_VIF_BEACON_FILTER))
		return;

	ieee80211_beacon_loss(vif);
}
EXPORT_SYMBOL_GPL(mt76_connac_mcu_beacon_loss_iter);

struct tlv *
mt76_connac_mcu_add_nested_tlv(struct sk_buff *skb, int tag, int len,
			       void *sta_ntlv, void *sta_wtbl)
{
	struct sta_ntlv_hdr *ntlv_hdr = sta_ntlv;
	struct tlv *sta_hdr = sta_wtbl;
	struct tlv *ptlv, tlv = {
		.tag = cpu_to_le16(tag),
		.len = cpu_to_le16(len),
	};
	u16 ntlv;

	ptlv = skb_put_zero(skb, len);
	memcpy(ptlv, &tlv, sizeof(tlv));

	ntlv = le16_to_cpu(ntlv_hdr->tlv_num);
	ntlv_hdr->tlv_num = cpu_to_le16(ntlv + 1);

	if (sta_hdr) {
		len += le16_to_cpu(sta_hdr->len);
		sta_hdr->len = cpu_to_le16(len);
	}

	return ptlv;
}
EXPORT_SYMBOL_GPL(mt76_connac_mcu_add_nested_tlv);

struct sk_buff *
__mt76_connac_mcu_alloc_sta_req(struct mt76_dev *dev, struct mt76_vif_link *mvif,
				struct mt76_wcid *wcid, int len)
{
	struct sta_req_hdr hdr = {
		.bss_idx = mvif->idx,
		.muar_idx = wcid ? mvif->omac_idx : 0,
		.is_tlv_append = 1,
	};
	struct sk_buff *skb;

	if (wcid && !wcid->sta && !wcid->sta_disabled)
		hdr.muar_idx = 0xe;

	mt76_connac_mcu_get_wlan_idx(dev, wcid, &hdr.wlan_idx_lo,
				     &hdr.wlan_idx_hi);
	skb = __mt76_mcu_msg_alloc(dev, NULL, len, len, GFP_ATOMIC);
	if (!skb)
		return ERR_PTR(-ENOMEM);

	skb_put_data(skb, &hdr, sizeof(hdr));

	return skb;
}
EXPORT_SYMBOL_GPL(__mt76_connac_mcu_alloc_sta_req);

struct wtbl_req_hdr *
mt76_connac_mcu_alloc_wtbl_req(struct mt76_dev *dev, struct mt76_wcid *wcid,
			       int cmd, void *sta_wtbl, struct sk_buff **skb)
{
	struct tlv *sta_hdr = sta_wtbl;
	struct wtbl_req_hdr hdr = {
		.operation = cmd,
	};
	struct sk_buff *nskb = *skb;

	mt76_connac_mcu_get_wlan_idx(dev, wcid, &hdr.wlan_idx_lo,
				     &hdr.wlan_idx_hi);
	if (!nskb) {
		nskb = mt76_mcu_msg_alloc(dev, NULL,
					  MT76_CONNAC_WTBL_UPDATE_MAX_SIZE);
		if (!nskb)
			return ERR_PTR(-ENOMEM);

		*skb = nskb;
	}

	if (sta_hdr)
		le16_add_cpu(&sta_hdr->len, sizeof(hdr));

	return skb_put_data(nskb, &hdr, sizeof(hdr));
}
EXPORT_SYMBOL_GPL(mt76_connac_mcu_alloc_wtbl_req);

void mt76_connac_mcu_bss_omac_tlv(struct sk_buff *skb,
				  struct ieee80211_vif *vif)
{
	struct mt76_vif_link *mvif = (struct mt76_vif_link *)vif->drv_priv;
	u8 omac_idx = mvif->omac_idx;
	struct bss_info_omac *omac;
	struct tlv *tlv;
	u32 type = 0;

	switch (vif->type) {
	case NL80211_IFTYPE_MONITOR:
	case NL80211_IFTYPE_MESH_POINT:
	case NL80211_IFTYPE_AP:
		if (vif->p2p)
			type = CONNECTION_P2P_GO;
		else
			type = CONNECTION_INFRA_AP;
		break;
	case NL80211_IFTYPE_STATION:
		if (vif->p2p)
			type = CONNECTION_P2P_GC;
		else
			type = CONNECTION_INFRA_STA;
		break;
	case NL80211_IFTYPE_ADHOC:
		type = CONNECTION_IBSS_ADHOC;
		break;
	default:
		WARN_ON(1);
		break;
	}

	tlv = mt76_connac_mcu_add_tlv(skb, BSS_INFO_OMAC, sizeof(*omac));

	omac = (struct bss_info_omac *)tlv;
	omac->conn_type = cpu_to_le32(type);
	omac->omac_idx = mvif->omac_idx;
	omac->band_idx = mvif->band_idx;
	omac->hw_bss_idx = omac_idx > EXT_BSSID_START ? HW_BSSID_0 : omac_idx;
}
EXPORT_SYMBOL_GPL(mt76_connac_mcu_bss_omac_tlv);

void mt76_connac_mcu_sta_basic_tlv(struct mt76_dev *dev, struct sk_buff *skb,
				   struct ieee80211_bss_conf *link_conf,
				   struct ieee80211_link_sta *link_sta,
				   int conn_state, bool newly)
{
	struct ieee80211_vif *vif = link_conf->vif;
	struct sta_rec_basic *basic;
	struct tlv *tlv;
	int conn_type;

	tlv = mt76_connac_mcu_add_tlv(skb, STA_REC_BASIC, sizeof(*basic));

	basic = (struct sta_rec_basic *)tlv;
	basic->extra_info = cpu_to_le16(EXTRA_INFO_VER);

	if (newly && conn_state != CONN_STATE_DISCONNECT)
		basic->extra_info |= cpu_to_le16(EXTRA_INFO_NEW);
	basic->conn_state = conn_state;

	if (!link_sta) {
		basic->conn_type = cpu_to_le32(CONNECTION_INFRA_BC);

		if (vif->type == NL80211_IFTYPE_STATION &&
		    !is_zero_ether_addr(link_conf->bssid)) {
			memcpy(basic->peer_addr, link_conf->bssid, ETH_ALEN);
			basic->aid = cpu_to_le16(vif->cfg.aid);
		} else {
			eth_broadcast_addr(basic->peer_addr);
		}
		return;
	}

	switch (vif->type) {
	case NL80211_IFTYPE_MESH_POINT:
	case NL80211_IFTYPE_AP:
		if (vif->p2p && !is_connac2(dev))
			conn_type = CONNECTION_P2P_GC;
		else
			conn_type = CONNECTION_INFRA_STA;
		basic->conn_type = cpu_to_le32(conn_type);
		basic->aid = cpu_to_le16(link_sta->sta->aid);
		break;
	case NL80211_IFTYPE_STATION:
		if (vif->p2p && !is_connac2(dev))
			conn_type = CONNECTION_P2P_GO;
		else
			conn_type = CONNECTION_INFRA_AP;
		basic->conn_type = cpu_to_le32(conn_type);
		basic->aid = cpu_to_le16(vif->cfg.aid);
		break;
	case NL80211_IFTYPE_ADHOC:
		basic->conn_type = cpu_to_le32(CONNECTION_IBSS_ADHOC);
		basic->aid = cpu_to_le16(link_sta->sta->aid);
		break;
	case NL80211_IFTYPE_NAN:
	case NL80211_IFTYPE_NAN_DATA:
		basic->conn_type = cpu_to_le32(CONNECTION_NAN);
		break;
	default:
		WARN_ON(1);
		break;
	}

	memcpy(basic->peer_addr, link_sta->addr, ETH_ALEN);
	basic->qos = link_sta->sta->wme;
}
EXPORT_SYMBOL_GPL(mt76_connac_mcu_sta_basic_tlv);

void mt76_connac_mcu_sta_uapsd(struct sk_buff *skb, struct ieee80211_vif *vif,
			       struct ieee80211_sta *sta)
{
	struct sta_rec_uapsd *uapsd;
	struct tlv *tlv;

	if (vif->type != NL80211_IFTYPE_AP || !sta->wme)
		return;

	tlv = mt76_connac_mcu_add_tlv(skb, STA_REC_APPS, sizeof(*uapsd));
	uapsd = (struct sta_rec_uapsd *)tlv;

	if (sta->uapsd_queues & IEEE80211_WMM_IE_STA_QOSINFO_AC_VO) {
		uapsd->dac_map |= BIT(3);
		uapsd->tac_map |= BIT(3);
	}
	if (sta->uapsd_queues & IEEE80211_WMM_IE_STA_QOSINFO_AC_VI) {
		uapsd->dac_map |= BIT(2);
		uapsd->tac_map |= BIT(2);
	}
	if (sta->uapsd_queues & IEEE80211_WMM_IE_STA_QOSINFO_AC_BE) {
		uapsd->dac_map |= BIT(1);
		uapsd->tac_map |= BIT(1);
	}
	if (sta->uapsd_queues & IEEE80211_WMM_IE_STA_QOSINFO_AC_BK) {
		uapsd->dac_map |= BIT(0);
		uapsd->tac_map |= BIT(0);
	}
	uapsd->max_sp = sta->max_sp;
}
EXPORT_SYMBOL_GPL(mt76_connac_mcu_sta_uapsd);

void mt76_connac_mcu_wtbl_hdr_trans_tlv(struct sk_buff *skb,
					struct ieee80211_vif *vif,
					struct mt76_wcid *wcid,
					void *sta_wtbl, void *wtbl_tlv)
{
	struct wtbl_hdr_trans *htr;
	struct tlv *tlv;

	tlv = mt76_connac_mcu_add_nested_tlv(skb, WTBL_HDR_TRANS,
					     sizeof(*htr),
					     wtbl_tlv, sta_wtbl);
	htr = (struct wtbl_hdr_trans *)tlv;
	htr->no_rx_trans = true;

	if (vif->type == NL80211_IFTYPE_STATION)
		htr->to_ds = true;
	else
		htr->from_ds = true;

	if (!wcid)
		return;

	htr->no_rx_trans = !test_bit(MT_WCID_FLAG_HDR_TRANS, &wcid->flags);
	if (test_bit(MT_WCID_FLAG_4ADDR, &wcid->flags)) {
		htr->to_ds = true;
		htr->from_ds = true;
	}

	if (test_bit(MT_WCID_FLAG_TDLS_PEER, &wcid->flags)) {
		htr->to_ds = false;
		htr->from_ds = false;
	}
}
EXPORT_SYMBOL_GPL(mt76_connac_mcu_wtbl_hdr_trans_tlv);

int mt76_connac_mcu_sta_update_hdr_trans(struct mt76_dev *dev,
					 struct ieee80211_vif *vif,
					 struct mt76_wcid *wcid, int cmd)
{
	struct mt76_vif_link *mvif = (struct mt76_vif_link *)vif->drv_priv;
	struct wtbl_req_hdr *wtbl_hdr;
	struct tlv *sta_wtbl;
	struct sk_buff *skb;

	skb = mt76_connac_mcu_alloc_sta_req(dev, mvif, wcid);
	if (IS_ERR(skb))
		return PTR_ERR(skb);

	sta_wtbl = mt76_connac_mcu_add_tlv(skb, STA_REC_WTBL,
					   sizeof(struct tlv));

	wtbl_hdr = mt76_connac_mcu_alloc_wtbl_req(dev, wcid, WTBL_SET,
						  sta_wtbl, &skb);
	if (IS_ERR(wtbl_hdr))
		return PTR_ERR(wtbl_hdr);

	mt76_connac_mcu_wtbl_hdr_trans_tlv(skb, vif, wcid, sta_wtbl, wtbl_hdr);

	return mt76_mcu_skb_send_msg(dev, skb, cmd, true);
}
EXPORT_SYMBOL_GPL(mt76_connac_mcu_sta_update_hdr_trans);

int mt76_connac_mcu_wtbl_update_hdr_trans(struct mt76_dev *dev,
					  struct ieee80211_vif *vif,
					  struct ieee80211_sta *sta)
{
	struct mt76_wcid *wcid = (struct mt76_wcid *)sta->drv_priv;
	struct wtbl_req_hdr *wtbl_hdr;
	struct sk_buff *skb = NULL;

	wtbl_hdr = mt76_connac_mcu_alloc_wtbl_req(dev, wcid, WTBL_SET, NULL,
						  &skb);
	if (IS_ERR(wtbl_hdr))
		return PTR_ERR(wtbl_hdr);

	mt76_connac_mcu_wtbl_hdr_trans_tlv(skb, vif, wcid, NULL, wtbl_hdr);

	return mt76_mcu_skb_send_msg(dev, skb, MCU_EXT_CMD(WTBL_UPDATE), true);
}
EXPORT_SYMBOL_GPL(mt76_connac_mcu_wtbl_update_hdr_trans);

void mt76_connac_mcu_wtbl_generic_tlv(struct mt76_dev *dev,
				      struct sk_buff *skb,
				      struct ieee80211_vif *vif,
				      struct ieee80211_sta *sta,
				      void *sta_wtbl, void *wtbl_tlv)
{
	struct mt76_vif_link *mvif = (struct mt76_vif_link *)vif->drv_priv;
	struct wtbl_generic *generic;
	struct wtbl_rx *rx;
	struct wtbl_spe *spe;
	struct tlv *tlv;

	tlv = mt76_connac_mcu_add_nested_tlv(skb, WTBL_GENERIC,
					     sizeof(*generic),
					     wtbl_tlv, sta_wtbl);

	generic = (struct wtbl_generic *)tlv;

	if (sta) {
		if (vif->type == NL80211_IFTYPE_STATION)
			generic->partial_aid = cpu_to_le16(vif->cfg.aid);
		else
			generic->partial_aid = cpu_to_le16(sta->aid);
		memcpy(generic->peer_addr, sta->addr, ETH_ALEN);
		generic->muar_idx = mvif->omac_idx;
		generic->qos = sta->wme;
	} else {
		if (!is_connac_v1(dev) && vif->type == NL80211_IFTYPE_STATION)
			memcpy(generic->peer_addr, vif->bss_conf.bssid,
			       ETH_ALEN);
		else
			eth_broadcast_addr(generic->peer_addr);

		generic->muar_idx = 0xe;
	}

	tlv = mt76_connac_mcu_add_nested_tlv(skb, WTBL_RX, sizeof(*rx),
					     wtbl_tlv, sta_wtbl);

	rx = (struct wtbl_rx *)tlv;
	rx->rca1 = sta ? vif->type != NL80211_IFTYPE_AP : 1;
	rx->rca2 = 1;
	rx->rv = 1;

	if (!is_connac_v1(dev))
		return;

	tlv = mt76_connac_mcu_add_nested_tlv(skb, WTBL_SPE, sizeof(*spe),
					     wtbl_tlv, sta_wtbl);
	spe = (struct wtbl_spe *)tlv;
	spe->spe_idx = 24;
}
EXPORT_SYMBOL_GPL(mt76_connac_mcu_wtbl_generic_tlv);

static void
mt76_connac_mcu_sta_amsdu_tlv(struct sk_buff *skb, struct ieee80211_sta *sta,
			      struct ieee80211_vif *vif)
{
	struct mt76_wcid *wcid = (struct mt76_wcid *)sta->drv_priv;
	struct sta_rec_amsdu *amsdu;
	struct tlv *tlv;

	if (vif->type != NL80211_IFTYPE_AP &&
	    vif->type != NL80211_IFTYPE_STATION)
		return;

	if (!sta->deflink.agg.max_amsdu_len)
		return;

	tlv = mt76_connac_mcu_add_tlv(skb, STA_REC_HW_AMSDU, sizeof(*amsdu));
	amsdu = (struct sta_rec_amsdu *)tlv;
	amsdu->max_amsdu_num = 8;
	amsdu->amsdu_en = true;
	amsdu->max_mpdu_size = sta->deflink.agg.max_amsdu_len >=
			       IEEE80211_MAX_MPDU_LEN_VHT_7991;

	wcid->amsdu = true;
}

#define HE_PHY(p, c)	u8_get_bits(c, IEEE80211_HE_PHY_##p)
#define HE_MAC(m, c)	u8_get_bits(c, IEEE80211_HE_MAC_##m)
static void
mt76_connac_mcu_sta_he_tlv(struct sk_buff *skb, struct ieee80211_sta *sta)
{
	struct ieee80211_sta_he_cap *he_cap = &sta->deflink.he_cap;
	struct ieee80211_he_cap_elem *elem = &he_cap->he_cap_elem;
	struct sta_rec_he *he;
	struct tlv *tlv;
	u32 cap = 0;

	tlv = mt76_connac_mcu_add_tlv(skb, STA_REC_HE, sizeof(*he));

	he = (struct sta_rec_he *)tlv;

	if (elem->mac_cap_info[0] & IEEE80211_HE_MAC_CAP0_HTC_HE)
		cap |= STA_REC_HE_CAP_HTC;

	if (elem->mac_cap_info[2] & IEEE80211_HE_MAC_CAP2_BSR)
		cap |= STA_REC_HE_CAP_BSR;

	if (elem->mac_cap_info[3] & IEEE80211_HE_MAC_CAP3_OMI_CONTROL)
		cap |= STA_REC_HE_CAP_OM;

	if (elem->mac_cap_info[4] & IEEE80211_HE_MAC_CAP4_AMSDU_IN_AMPDU)
		cap |= STA_REC_HE_CAP_AMSDU_IN_AMPDU;

	if (elem->mac_cap_info[4] & IEEE80211_HE_MAC_CAP4_BQR)
		cap |= STA_REC_HE_CAP_BQR;

	if (elem->phy_cap_info[0] &
	    (IEEE80211_HE_PHY_CAP0_CHANNEL_WIDTH_SET_RU_MAPPING_IN_2G |
	     IEEE80211_HE_PHY_CAP0_CHANNEL_WIDTH_SET_RU_MAPPING_IN_5G))
		cap |= STA_REC_HE_CAP_BW20_RU242_SUPPORT;

	if (elem->phy_cap_info[1] &
	    IEEE80211_HE_PHY_CAP1_LDPC_CODING_IN_PAYLOAD)
		cap |= STA_REC_HE_CAP_LDPC;

	if (elem->phy_cap_info[1] &
	    IEEE80211_HE_PHY_CAP1_HE_LTF_AND_GI_FOR_HE_PPDUS_0_8US)
		cap |= STA_REC_HE_CAP_SU_PPDU_1LTF_8US_GI;

	if (elem->phy_cap_info[2] &
	    IEEE80211_HE_PHY_CAP2_NDP_4x_LTF_AND_3_2US)
		cap |= STA_REC_HE_CAP_NDP_4LTF_3DOT2MS_GI;

	if (elem->phy_cap_info[2] &
	    IEEE80211_HE_PHY_CAP2_STBC_TX_UNDER_80MHZ)
		cap |= STA_REC_HE_CAP_LE_EQ_80M_TX_STBC;

	if (elem->phy_cap_info[2] &
	    IEEE80211_HE_PHY_CAP2_STBC_RX_UNDER_80MHZ)
		cap |= STA_REC_HE_CAP_LE_EQ_80M_RX_STBC;

	if (elem->phy_cap_info[6] &
	    IEEE80211_HE_PHY_CAP6_PARTIAL_BW_EXT_RANGE)
		cap |= STA_REC_HE_CAP_PARTIAL_BW_EXT_RANGE;

	if (elem->phy_cap_info[7] &
	    IEEE80211_HE_PHY_CAP7_HE_SU_MU_PPDU_4XLTF_AND_08_US_GI)
		cap |= STA_REC_HE_CAP_SU_MU_PPDU_4LTF_8US_GI;

	if (elem->phy_cap_info[7] &
	    IEEE80211_HE_PHY_CAP7_STBC_TX_ABOVE_80MHZ)
		cap |= STA_REC_HE_CAP_GT_80M_TX_STBC;

	if (elem->phy_cap_info[7] &
	    IEEE80211_HE_PHY_CAP7_STBC_RX_ABOVE_80MHZ)
		cap |= STA_REC_HE_CAP_GT_80M_RX_STBC;

	if (elem->phy_cap_info[8] &
	    IEEE80211_HE_PHY_CAP8_HE_ER_SU_PPDU_4XLTF_AND_08_US_GI)
		cap |= STA_REC_HE_CAP_ER_SU_PPDU_4LTF_8US_GI;

	if (elem->phy_cap_info[8] &
	    IEEE80211_HE_PHY_CAP8_HE_ER_SU_1XLTF_AND_08_US_GI)
		cap |= STA_REC_HE_CAP_ER_SU_PPDU_1LTF_8US_GI;

	if (elem->phy_cap_info[9] &
	    IEEE80211_HE_PHY_CAP9_NON_TRIGGERED_CQI_FEEDBACK)
		cap |= STA_REC_HE_CAP_TRIG_CQI_FK;

	if (elem->phy_cap_info[9] &
	    IEEE80211_HE_PHY_CAP9_TX_1024_QAM_LESS_THAN_242_TONE_RU)
		cap |= STA_REC_HE_CAP_TX_1024QAM_UNDER_RU242;

	if (elem->phy_cap_info[9] &
	    IEEE80211_HE_PHY_CAP9_RX_1024_QAM_LESS_THAN_242_TONE_RU)
		cap |= STA_REC_HE_CAP_RX_1024QAM_UNDER_RU242;

	he->he_cap = cpu_to_le32(cap);

	switch (sta->deflink.bandwidth) {
	case IEEE80211_STA_RX_BW_160:
		if (elem->phy_cap_info[0] &
		    IEEE80211_HE_PHY_CAP0_CHANNEL_WIDTH_SET_80PLUS80_MHZ_IN_5G)
			he->max_nss_mcs[CMD_HE_MCS_BW8080] =
				he_cap->he_mcs_nss_supp.rx_mcs_80p80;

		he->max_nss_mcs[CMD_HE_MCS_BW160] =
				he_cap->he_mcs_nss_supp.rx_mcs_160;
		fallthrough;
	default:
		he->max_nss_mcs[CMD_HE_MCS_BW80] =
				he_cap->he_mcs_nss_supp.rx_mcs_80;
		break;
	}

	he->t_frame_dur =
		HE_MAC(CAP1_TF_MAC_PAD_DUR_MASK, elem->mac_cap_info[1]);
	he->max_ampdu_exp =
		HE_MAC(CAP3_MAX_AMPDU_LEN_EXP_MASK, elem->mac_cap_info[3]);

	he->bw_set =
		HE_PHY(CAP0_CHANNEL_WIDTH_SET_MASK, elem->phy_cap_info[0]);
	he->device_class =
		HE_PHY(CAP1_DEVICE_CLASS_A, elem->phy_cap_info[1]);
	he->punc_pream_rx =
		HE_PHY(CAP1_PREAMBLE_PUNC_RX_MASK, elem->phy_cap_info[1]);

	he->dcm_tx_mode =
		HE_PHY(CAP3_DCM_MAX_CONST_TX_MASK, elem->phy_cap_info[3]);
	he->dcm_tx_max_nss =
		HE_PHY(CAP3_DCM_MAX_TX_NSS_2, elem->phy_cap_info[3]);
	he->dcm_rx_mode =
		HE_PHY(CAP3_DCM_MAX_CONST_RX_MASK, elem->phy_cap_info[3]);
	he->dcm_rx_max_nss =
		HE_PHY(CAP3_DCM_MAX_RX_NSS_2, elem->phy_cap_info[3]);
	he->dcm_max_ru =
		HE_PHY(CAP8_DCM_MAX_RU_MASK, elem->phy_cap_info[8]);

	he->pkt_ext = 2;
}

void
mt76_connac_mcu_sta_he_tlv_v2(struct sk_buff *skb, struct ieee80211_sta *sta)
{
	struct ieee80211_sta_he_cap *he_cap = &sta->deflink.he_cap;
	struct ieee80211_he_cap_elem *elem = &he_cap->he_cap_elem;
	struct sta_rec_he_v2 *he;
	struct tlv *tlv;

	tlv = mt76_connac_mcu_add_tlv(skb, STA_REC_HE_V2, sizeof(*he));

	he = (struct sta_rec_he_v2 *)tlv;
	memcpy(he->he_phy_cap, elem->phy_cap_info, sizeof(he->he_phy_cap));
	memcpy(he->he_mac_cap, elem->mac_cap_info, sizeof(he->he_mac_cap));

	switch (sta->deflink.bandwidth) {
	case IEEE80211_STA_RX_BW_160:
		if (elem->phy_cap_info[0] &
		    IEEE80211_HE_PHY_CAP0_CHANNEL_WIDTH_SET_80PLUS80_MHZ_IN_5G)
			he->max_nss_mcs[CMD_HE_MCS_BW8080] =
				he_cap->he_mcs_nss_supp.rx_mcs_80p80;

		he->max_nss_mcs[CMD_HE_MCS_BW160] =
				he_cap->he_mcs_nss_supp.rx_mcs_160;
		fallthrough;
	default:
		he->max_nss_mcs[CMD_HE_MCS_BW80] =
				he_cap->he_mcs_nss_supp.rx_mcs_80;
		break;
	}

	he->pkt_ext = IEEE80211_HE_PHY_CAP9_NOMINAL_PKT_PADDING_16US;
}
EXPORT_SYMBOL_GPL(mt76_connac_mcu_sta_he_tlv_v2);

u8
mt76_connac_get_phy_mode_v2(struct mt76_phy *mphy, struct ieee80211_vif *vif,
			    enum nl80211_band band,
			    struct ieee80211_link_sta *link_sta)
{
	struct ieee80211_sta_ht_cap *ht_cap;
	struct ieee80211_sta_vht_cap *vht_cap;
	const struct ieee80211_sta_he_cap *he_cap;
	const struct ieee80211_sta_eht_cap *eht_cap;
	u8 mode = 0;

	if (link_sta) {
		ht_cap = &link_sta->ht_cap;
		vht_cap = &link_sta->vht_cap;
		he_cap = &link_sta->he_cap;
		eht_cap = &link_sta->eht_cap;
	} else {
		struct ieee80211_supported_band *sband;

		sband = mphy->hw->wiphy->bands[band];
		ht_cap = &sband->ht_cap;
		vht_cap = &sband->vht_cap;
		he_cap = ieee80211_get_he_iftype_cap(sband, vif->type);
		eht_cap = ieee80211_get_eht_iftype_cap(sband, vif->type);
	}

	if (band == NL80211_BAND_2GHZ) {
		mode |= PHY_TYPE_BIT_HR_DSSS | PHY_TYPE_BIT_ERP;

		if (ht_cap->ht_supported)
			mode |= PHY_TYPE_BIT_HT;

		if (he_cap && he_cap->has_he)
			mode |= PHY_TYPE_BIT_HE;

		if (eht_cap && eht_cap->has_eht)
			mode |= PHY_TYPE_BIT_BE;
	} else if (band == NL80211_BAND_5GHZ || band == NL80211_BAND_6GHZ) {
		mode |= PHY_TYPE_BIT_OFDM;

		if (ht_cap->ht_supported)
			mode |= PHY_TYPE_BIT_HT;

		if (vht_cap->vht_supported)
			mode |= PHY_TYPE_BIT_VHT;

		if (he_cap && he_cap->has_he)
			mode |= PHY_TYPE_BIT_HE;

		if (eht_cap && eht_cap->has_eht)
			mode |= PHY_TYPE_BIT_BE;
	}

	return mode;
}
EXPORT_SYMBOL_GPL(mt76_connac_get_phy_mode_v2);

void mt76_connac_mcu_sta_tlv(struct mt76_phy *mphy, struct sk_buff *skb,
			     struct ieee80211_sta *sta,
			     struct ieee80211_vif *vif,
			     u8 rcpi, u8 sta_state)
{
	struct mt76_vif_link *mvif = (struct mt76_vif_link *)vif->drv_priv;
	struct cfg80211_chan_def *chandef = mvif->ctx ?
					    &mvif->ctx->def : &mphy->chandef;
	enum nl80211_band band = chandef->chan->band;
	struct mt76_dev *dev = mphy->dev;
	struct sta_rec_ra_info *ra_info;
	struct sta_rec_state *state;
	struct sta_rec_phy *phy;
	struct tlv *tlv;
	u16 supp_rates;

	/* starec ht */
	if (sta->deflink.ht_cap.ht_supported) {
		struct sta_rec_ht *ht;

		tlv = mt76_connac_mcu_add_tlv(skb, STA_REC_HT, sizeof(*ht));
		ht = (struct sta_rec_ht *)tlv;
		ht->ht_cap = cpu_to_le16(sta->deflink.ht_cap.cap);
	}

	/* starec vht */
	if (sta->deflink.vht_cap.vht_supported) {
		struct sta_rec_vht *vht;
		int len;

		len = is_connac2(dev) ? sizeof(*vht) : sizeof(*vht) - 4;
		tlv = mt76_connac_mcu_add_tlv(skb, STA_REC_VHT, len);
		vht = (struct sta_rec_vht *)tlv;
		vht->vht_cap = cpu_to_le32(sta->deflink.vht_cap.cap);
		vht->vht_rx_mcs_map = sta->deflink.vht_cap.vht_mcs.rx_mcs_map;
		vht->vht_tx_mcs_map = sta->deflink.vht_cap.vht_mcs.tx_mcs_map;
	}

	/* starec uapsd */
	mt76_connac_mcu_sta_uapsd(skb, vif, sta);

	if (!is_connac2(dev))
		return;

	if (sta->deflink.ht_cap.ht_supported || sta->deflink.he_cap.has_he)
		mt76_connac_mcu_sta_amsdu_tlv(skb, sta, vif);

	/* starec he */
	if (sta->deflink.he_cap.has_he) {
		mt76_connac_mcu_sta_he_tlv(skb, sta);
		mt76_connac_mcu_sta_he_tlv_v2(skb, sta);
		if (band == NL80211_BAND_6GHZ &&
		    sta_state == MT76_STA_INFO_STATE_ASSOC) {
			struct sta_rec_he_6g_capa *he_6g_capa;

			tlv = mt76_connac_mcu_add_tlv(skb, STA_REC_HE_6G,
						      sizeof(*he_6g_capa));
			he_6g_capa = (struct sta_rec_he_6g_capa *)tlv;
			he_6g_capa->capa = sta->deflink.he_6ghz_capa.capa;
		}
	}

	tlv = mt76_connac_mcu_add_tlv(skb, STA_REC_PHY, sizeof(*phy));
	phy = (struct sta_rec_phy *)tlv;
	phy->phy_type = mt76_connac_get_phy_mode_v2(mphy, vif, band,
						    &sta->deflink);
	phy->basic_rate = cpu_to_le16((u16)vif->bss_conf.basic_rates);
	phy->rcpi = rcpi;
	phy->ampdu = FIELD_PREP(IEEE80211_HT_AMPDU_PARM_FACTOR,
				sta->deflink.ht_cap.ampdu_factor) |
		     FIELD_PREP(IEEE80211_HT_AMPDU_PARM_DENSITY,
				sta->deflink.ht_cap.ampdu_density);

	tlv = mt76_connac_mcu_add_tlv(skb, STA_REC_RA, sizeof(*ra_info));
	ra_info = (struct sta_rec_ra_info *)tlv;

	supp_rates = sta->deflink.supp_rates[band];
	if (band == NL80211_BAND_2GHZ)
		supp_rates = FIELD_PREP(RA_LEGACY_OFDM, supp_rates >> 4) |
			     FIELD_PREP(RA_LEGACY_CCK, supp_rates & 0xf);
	else
		supp_rates = FIELD_PREP(RA_LEGACY_OFDM, supp_rates);

	ra_info->legacy = cpu_to_le16(supp_rates);

	if (sta->deflink.ht_cap.ht_supported)
		memcpy(ra_info->rx_mcs_bitmask,
		       sta->deflink.ht_cap.mcs.rx_mask,
		       HT_MCS_MASK_NUM);

	tlv = mt76_connac_mcu_add_tlv(skb, STA_REC_STATE, sizeof(*state));
	state = (struct sta_rec_state *)tlv;
	state->state = sta_state;

	if (sta->deflink.vht_cap.vht_supported) {
		state->vht_opmode = sta->deflink.bandwidth;
		state->vht_opmode |= (sta->deflink.rx_nss - 1) <<
			IEEE80211_OPMODE_NOTIF_RX_NSS_SHIFT;
	}
}
EXPORT_SYMBOL_GPL(mt76_connac_mcu_sta_tlv);

void mt76_connac_mcu_wtbl_smps_tlv(struct sk_buff *skb,
				   struct ieee80211_sta *sta,
				   void *sta_wtbl, void *wtbl_tlv)
{
	struct wtbl_smps *smps;
	struct tlv *tlv;

	tlv = mt76_connac_mcu_add_nested_tlv(skb, WTBL_SMPS, sizeof(*smps),
					     wtbl_tlv, sta_wtbl);
	smps = (struct wtbl_smps *)tlv;
	smps->smps = (sta->deflink.smps_mode == IEEE80211_SMPS_DYNAMIC);
}
EXPORT_SYMBOL_GPL(mt76_connac_mcu_wtbl_smps_tlv);

void mt76_connac_mcu_wtbl_ht_tlv(struct mt76_dev *dev, struct sk_buff *skb,
				 struct ieee80211_sta *sta, void *sta_wtbl,
				 void *wtbl_tlv, bool ht_ldpc, bool vht_ldpc)
{
	struct wtbl_ht *ht = NULL;
	struct tlv *tlv;
	u32 flags = 0;

	if (sta->deflink.ht_cap.ht_supported || sta->deflink.he_6ghz_capa.capa) {
		tlv = mt76_connac_mcu_add_nested_tlv(skb, WTBL_HT, sizeof(*ht),
						     wtbl_tlv, sta_wtbl);
		ht = (struct wtbl_ht *)tlv;
		ht->ldpc = ht_ldpc &&
			   !!(sta->deflink.ht_cap.cap & IEEE80211_HT_CAP_LDPC_CODING);

		if (sta->deflink.ht_cap.ht_supported) {
			ht->af = sta->deflink.ht_cap.ampdu_factor;
			ht->mm = sta->deflink.ht_cap.ampdu_density;
		} else {
			ht->af = le16_get_bits(sta->deflink.he_6ghz_capa.capa,
					       IEEE80211_HE_6GHZ_CAP_MAX_AMPDU_LEN_EXP);
			ht->mm = le16_get_bits(sta->deflink.he_6ghz_capa.capa,
					       IEEE80211_HE_6GHZ_CAP_MIN_MPDU_START);
		}

		ht->ht = true;
	}

	if (sta->deflink.vht_cap.vht_supported || sta->deflink.he_6ghz_capa.capa) {
		struct wtbl_vht *vht;
		u8 af;

		tlv = mt76_connac_mcu_add_nested_tlv(skb, WTBL_VHT,
						     sizeof(*vht), wtbl_tlv,
						     sta_wtbl);
		vht = (struct wtbl_vht *)tlv;
		vht->ldpc = vht_ldpc &&
			    !!(sta->deflink.vht_cap.cap & IEEE80211_VHT_CAP_RXLDPC);
		vht->vht = true;

		af = FIELD_GET(IEEE80211_VHT_CAP_MAX_A_MPDU_LENGTH_EXPONENT_MASK,
			       sta->deflink.vht_cap.cap);
		if (ht)
			ht->af = max(ht->af, af);
	}

	mt76_connac_mcu_wtbl_smps_tlv(skb, sta, sta_wtbl, wtbl_tlv);

	if (is_connac_v1(dev) && sta->deflink.ht_cap.ht_supported) {
		/* sgi */
		u32 msk = MT_WTBL_W5_SHORT_GI_20 | MT_WTBL_W5_SHORT_GI_40 |
			  MT_WTBL_W5_SHORT_GI_80 | MT_WTBL_W5_SHORT_GI_160;
		struct wtbl_raw *raw;

		tlv = mt76_connac_mcu_add_nested_tlv(skb, WTBL_RAW_DATA,
						     sizeof(*raw), wtbl_tlv,
						     sta_wtbl);

		if (sta->deflink.ht_cap.cap & IEEE80211_HT_CAP_SGI_20)
			flags |= MT_WTBL_W5_SHORT_GI_20;
		if (sta->deflink.ht_cap.cap & IEEE80211_HT_CAP_SGI_40)
			flags |= MT_WTBL_W5_SHORT_GI_40;

		if (sta->deflink.vht_cap.vht_supported) {
			if (sta->deflink.vht_cap.cap & IEEE80211_VHT_CAP_SHORT_GI_80)
				flags |= MT_WTBL_W5_SHORT_GI_80;
			if (sta->deflink.vht_cap.cap & IEEE80211_VHT_CAP_SHORT_GI_160)
				flags |= MT_WTBL_W5_SHORT_GI_160;
		}
		raw = (struct wtbl_raw *)tlv;
		raw->val = cpu_to_le32(flags);
		raw->msk = cpu_to_le32(~msk);
		raw->wtbl_idx = 1;
		raw->dw = 5;
	}
}
EXPORT_SYMBOL_GPL(mt76_connac_mcu_wtbl_ht_tlv);

int mt76_connac_mcu_sta_cmd(struct mt76_phy *phy,
			    struct mt76_sta_cmd_info *info)
{
	struct mt76_vif_link *mvif = (struct mt76_vif_link *)info->vif->drv_priv;
	struct ieee80211_link_sta *link_sta;
	struct mt76_dev *dev = phy->dev;
	struct wtbl_req_hdr *wtbl_hdr;
	struct tlv *sta_wtbl;
	struct sk_buff *skb;
	int conn_state;

	if (!info->link_conf)
		info->link_conf = &info->vif->bss_conf;

	skb = mt76_connac_mcu_alloc_sta_req(dev, mvif, info->wcid);
	if (IS_ERR(skb))
		return PTR_ERR(skb);

	conn_state = info->enable ? CONN_STATE_PORT_SECURE :
				    CONN_STATE_DISCONNECT;
	link_sta = info->sta ? &info->sta->deflink : NULL;
	if (info->sta || !info->offload_fw)
		mt76_connac_mcu_sta_basic_tlv(dev, skb, info->link_conf,
					      link_sta, conn_state,
					      info->newly);
	if (info->sta && info->enable)
		mt76_connac_mcu_sta_tlv(phy, skb, info->sta,
					info->vif, info->rcpi,
					info->state);

	sta_wtbl = mt76_connac_mcu_add_tlv(skb, STA_REC_WTBL,
					   sizeof(struct tlv));

	wtbl_hdr = mt76_connac_mcu_alloc_wtbl_req(dev, info->wcid,
						  WTBL_RESET_AND_SET,
						  sta_wtbl, &skb);
	if (IS_ERR(wtbl_hdr))
		return PTR_ERR(wtbl_hdr);

	if (info->enable) {
		mt76_connac_mcu_wtbl_generic_tlv(dev, skb, info->vif,
						 info->sta, sta_wtbl,
						 wtbl_hdr);
		mt76_connac_mcu_wtbl_hdr_trans_tlv(skb, info->vif, info->wcid,
						   sta_wtbl, wtbl_hdr);
		if (info->sta)
			mt76_connac_mcu_wtbl_ht_tlv(dev, skb, info->sta,
						    sta_wtbl, wtbl_hdr,
						    true, true);
	}

	return mt76_mcu_skb_send_msg(dev, skb, info->cmd, true);
}
EXPORT_SYMBOL_GPL(mt76_connac_mcu_sta_cmd);

void mt76_connac_mcu_wtbl_ba_tlv(struct mt76_dev *dev, struct sk_buff *skb,
				 struct ieee80211_ampdu_params *params,
				 bool enable, bool tx, void *sta_wtbl,
				 void *wtbl_tlv)
{
	struct wtbl_ba *ba;
	struct tlv *tlv;

	tlv = mt76_connac_mcu_add_nested_tlv(skb, WTBL_BA, sizeof(*ba),
					     wtbl_tlv, sta_wtbl);

	ba = (struct wtbl_ba *)tlv;
	ba->tid = params->tid;

	if (tx) {
		ba->ba_type = MT_BA_TYPE_ORIGINATOR;
		ba->sn = enable ? cpu_to_le16(params->ssn) : 0;
		ba->ba_winsize = enable ? cpu_to_le16(params->buf_size) : 0;
		ba->ba_en = enable;
	} else {
		memcpy(ba->peer_addr, params->sta->addr, ETH_ALEN);
		ba->ba_type = MT_BA_TYPE_RECIPIENT;
		ba->rst_ba_tid = params->tid;
		ba->rst_ba_sel = RST_BA_MAC_TID_MATCH;
		ba->rst_ba_sb = 1;
	}

	if (!is_connac_v1(dev)) {
		ba->ba_winsize = enable ? cpu_to_le16(params->buf_size) : 0;
		return;
	}

	if (enable && tx) {
		static const u8 ba_range[] = { 4, 8, 12, 24, 36, 48, 54, 64 };
		int i;

		for (i = 7; i > 0; i--) {
			if (params->buf_size >= ba_range[i])
				break;
		}
		ba->ba_winsize_idx = i;
	}
}
EXPORT_SYMBOL_GPL(mt76_connac_mcu_wtbl_ba_tlv);

int mt76_connac_mcu_uni_add_dev(struct mt76_phy *phy,
				struct ieee80211_bss_conf *bss_conf,
				struct mt76_vif_link *mvif,
				struct mt76_wcid *wcid,
				bool enable)
{
	struct mt76_dev *dev = phy->dev;
	struct {
		struct {
			u8 omac_idx;
			u8 band_idx;
			__le16 pad;
		} __packed hdr;
		struct req_tlv {
			__le16 tag;
			__le16 len;
			u8 active;
			u8 link_idx; /* not link_id */
			u8 omac_addr[ETH_ALEN];
		} __packed tlv;
	} dev_req = {
		.hdr = {
			.omac_idx = mvif->omac_idx,
			.band_idx = mvif->band_idx,
		},
		.tlv = {
			.tag = cpu_to_le16(DEV_INFO_ACTIVE),
			.len = cpu_to_le16(sizeof(struct req_tlv)),
			.active = enable,
			.link_idx = mvif->link_idx,
		},
	};
	struct {
		struct {
			u8 bss_idx;
			u8 pad[3];
		} __packed hdr;
		struct mt76_connac_bss_basic_tlv basic;
	} basic_req = {
		.hdr = {
			.bss_idx = mvif->idx,
		},
		.basic = {
			.tag = cpu_to_le16(UNI_BSS_INFO_BASIC),
			.len = cpu_to_le16(sizeof(struct mt76_connac_bss_basic_tlv)),
			.omac_idx = mvif->omac_idx,
			.band_idx = mvif->band_idx,
			.wmm_idx = mvif->wmm_idx,
			.active = enable,
			.bmc_tx_wlan_idx = cpu_to_le16(wcid->idx),
			.sta_idx = cpu_to_le16(wcid->idx),
			.conn_state = 1,
			.link_idx = mvif->link_idx,
		},
	};
	int err, idx, cmd, len;
	void *data;

	switch (bss_conf->vif->type) {
	case NL80211_IFTYPE_MESH_POINT:
	case NL80211_IFTYPE_MONITOR:
	case NL80211_IFTYPE_AP:
		basic_req.basic.conn_type = cpu_to_le32(CONNECTION_INFRA_AP);
		break;
	case NL80211_IFTYPE_STATION:
		basic_req.basic.conn_type = cpu_to_le32(CONNECTION_INFRA_STA);
		break;
	case NL80211_IFTYPE_P2P_DEVICE:
		basic_req.basic.conn_type = cpu_to_le32(CONNECTION_P2P_GO);
		break;
	case NL80211_IFTYPE_ADHOC:
		basic_req.basic.conn_type = cpu_to_le32(CONNECTION_IBSS_ADHOC);
		break;
	case NL80211_IFTYPE_NAN:
	case NL80211_IFTYPE_NAN_DATA:
		basic_req.basic.conn_type = cpu_to_le32(CONNECTION_NAN);
		basic_req.basic.conn_state = !enable;
		break;
	default:
		WARN_ON(1);
		break;
	}

	idx = mvif->omac_idx > EXT_BSSID_START ? HW_BSSID_0 : mvif->omac_idx;
	basic_req.basic.hw_bss_idx = idx;

	memcpy(dev_req.tlv.omac_addr, bss_conf->addr, ETH_ALEN);
	if (is_mt7932(dev)) {
		/* AppleSunriseWLAN 25G83 has no unified-command sender.  Its
		 * nicActivateNetwork() submits legacy CID 0x11 before the first
		 * scan.  Sending connac2's unified DEV_INFO/BSS_INFO pair left the
		 * firmware without the BSS context consumed by CMD_SCAN_REQ_V2.
		 */
		struct {
			u8 bss_idx;
			u8 active;
			u8 network_type;
			u8 own_mac_idx;
			u8 bss_mac_addr[ETH_ALEN];
			u8 bmc_wlan_idx;
			u8 mt7932_net_active;
		} __packed activate = {
			.bss_idx = mvif->idx,
			.active = enable,
			.network_type = bss_conf->vif->type ==
					NL80211_IFTYPE_P2P_DEVICE ? 1 :
					bss_conf->vif->type == NL80211_IFTYPE_AP ? 3 : 0,
			.own_mac_idx = mvif->omac_idx,
			/* The synchronized 25G83 AIS sequence uses 0xff for every
			 * activation/deactivation at ordinals 191, 193, 194, 196, 197
			 * and 199. It is the firmware's unassigned BMC/WTBL sentinel,
			 * not mt76's later reserved host WCID.
			 */
			.bmc_wlan_idx = 0xff,
			/* AppleSunriseWLAN 25G83 writes 1 to byte 11.  The exact
			 * MT7932 CID 0x11 handler reads this byte and uses it to
			 * select the internal BSS-state activation path.
			 */
			.mt7932_net_active = 1,
		};
		BUILD_BUG_ON(sizeof(activate) != 12);
		/* 25G83 copies the interface address on activation and sends an
		 * all-zero address on deactivation while retaining the stored BMC
		 * index and state-control byte.
		 */
		if (enable)
			memcpy(activate.bss_mac_addr, bss_conf->addr, ETH_ALEN);
		dev_info(dev->dev,
			 "J700_MT7932_LEGACY_BSS_ACTIVATE: bss=%u active=%u type=%u omac=%u bmc=%u net_active=%u identity=live payload_bytes=%zu\n",
			 activate.bss_idx, activate.active, activate.network_type,
			 activate.own_mac_idx, activate.bmc_wlan_idx,
			 activate.mt7932_net_active, sizeof(activate));
		return mt76_mcu_send_msg(dev, MCU_CE_CMD(BSS_ACTIVATE_CTRL),
					 &activate, sizeof(activate), false);
	}

	cmd = enable ? MCU_UNI_CMD(DEV_INFO_UPDATE) : MCU_UNI_CMD(BSS_INFO_UPDATE);
	data = enable ? (void *)&dev_req : (void *)&basic_req;
	len = enable ? sizeof(dev_req) : sizeof(basic_req);

	err = mt76_mcu_send_msg(dev, cmd, data, len, true);
	if (err && cmd == MCU_UNI_CMD(BSS_INFO_UPDATE))
		err = mt76_connac_mcu_bss_deact_err(dev, err, enable);

	if (err < 0)
		return err;

	cmd = enable ? MCU_UNI_CMD(BSS_INFO_UPDATE) : MCU_UNI_CMD(DEV_INFO_UPDATE);
	data = enable ? (void *)&basic_req : (void *)&dev_req;
	len = enable ? sizeof(basic_req) : sizeof(dev_req);

	err = mt76_mcu_send_msg(dev, cmd, data, len, true);
	if (err && cmd == MCU_UNI_CMD(BSS_INFO_UPDATE))
		err = mt76_connac_mcu_bss_deact_err(dev, err, enable);

	return err;
}
EXPORT_SYMBOL_GPL(mt76_connac_mcu_uni_add_dev);

void mt76_connac_mcu_sta_ba_tlv(struct sk_buff *skb,
				struct ieee80211_ampdu_params *params,
				bool enable, bool tx)
{
	struct sta_rec_ba *ba;
	struct tlv *tlv;

	tlv = mt76_connac_mcu_add_tlv(skb, STA_REC_BA, sizeof(*ba));

	ba = (struct sta_rec_ba *)tlv;
	ba->ba_type = tx ? MT_BA_TYPE_ORIGINATOR : MT_BA_TYPE_RECIPIENT;
	ba->winsize = cpu_to_le16(params->buf_size);
	ba->ssn = cpu_to_le16(params->ssn);
	ba->ba_en = enable << params->tid;
	ba->amsdu = params->amsdu;
	ba->tid = params->tid;
}
EXPORT_SYMBOL_GPL(mt76_connac_mcu_sta_ba_tlv);

int mt76_connac_mcu_sta_wed_update(struct mt76_dev *dev, struct sk_buff *skb)
{
	if (!mt76_is_mmio(dev))
		return 0;

	if (!mtk_wed_device_active(&dev->mmio.wed))
		return 0;

	return mtk_wed_device_update_msg(&dev->mmio.wed, WED_WO_STA_REC,
					 skb->data, skb->len);
}
EXPORT_SYMBOL_GPL(mt76_connac_mcu_sta_wed_update);

int mt76_connac_mcu_sta_ba(struct mt76_dev *dev, struct mt76_vif_link *mvif,
			   struct ieee80211_ampdu_params *params,
			   int cmd, bool enable, bool tx)
{
	struct mt76_wcid *wcid = (struct mt76_wcid *)params->sta->drv_priv;
	struct wtbl_req_hdr *wtbl_hdr;
	struct tlv *sta_wtbl;
	struct sk_buff *skb;
	int ret;

	skb = mt76_connac_mcu_alloc_sta_req(dev, mvif, wcid);
	if (IS_ERR(skb))
		return PTR_ERR(skb);

	sta_wtbl = mt76_connac_mcu_add_tlv(skb, STA_REC_WTBL,
					   sizeof(struct tlv));

	wtbl_hdr = mt76_connac_mcu_alloc_wtbl_req(dev, wcid, WTBL_SET,
						  sta_wtbl, &skb);
	if (IS_ERR(wtbl_hdr))
		return PTR_ERR(wtbl_hdr);

	mt76_connac_mcu_wtbl_ba_tlv(dev, skb, params, enable, tx, sta_wtbl,
				    wtbl_hdr);

	ret = mt76_connac_mcu_sta_wed_update(dev, skb);
	if (ret) {
		dev_kfree_skb(skb);
		return ret;
	}

	ret = mt76_mcu_skb_send_msg(dev, skb, cmd, true);
	if (ret)
		return ret;

	skb = mt76_connac_mcu_alloc_sta_req(dev, mvif, wcid);
	if (IS_ERR(skb))
		return PTR_ERR(skb);

	mt76_connac_mcu_sta_ba_tlv(skb, params, enable, tx);

	ret = mt76_connac_mcu_sta_wed_update(dev, skb);
	if (ret) {
		dev_kfree_skb(skb);
		return ret;
	}

	return mt76_mcu_skb_send_msg(dev, skb, cmd, true);
}
EXPORT_SYMBOL_GPL(mt76_connac_mcu_sta_ba);

u8 mt76_connac_get_phy_mode(struct mt76_phy *phy, struct ieee80211_vif *vif,
			    enum nl80211_band band,
			    struct ieee80211_link_sta *link_sta)
{
	struct mt76_dev *dev = phy->dev;
	const struct ieee80211_sta_he_cap *he_cap;
	struct ieee80211_sta_vht_cap *vht_cap;
	struct ieee80211_sta_ht_cap *ht_cap;
	u8 mode = 0;

	if (is_connac_v1(dev))
		return 0x38;

	if (link_sta) {
		ht_cap = &link_sta->ht_cap;
		vht_cap = &link_sta->vht_cap;
		he_cap = &link_sta->he_cap;
	} else {
		struct ieee80211_supported_band *sband;

		sband = phy->hw->wiphy->bands[band];
		ht_cap = &sband->ht_cap;
		vht_cap = &sband->vht_cap;
		he_cap = ieee80211_get_he_iftype_cap(sband, vif->type);
	}

	if (band == NL80211_BAND_2GHZ) {
		mode |= PHY_MODE_B | PHY_MODE_G;

		if (ht_cap->ht_supported)
			mode |= PHY_MODE_GN;

		if (he_cap && he_cap->has_he)
			mode |= PHY_MODE_AX_24G;
	} else if (band == NL80211_BAND_5GHZ) {
		mode |= PHY_MODE_A;

		if (ht_cap->ht_supported)
			mode |= PHY_MODE_AN;

		if (vht_cap->vht_supported)
			mode |= PHY_MODE_AC;

		if (he_cap && he_cap->has_he)
			mode |= PHY_MODE_AX_5G;
	} else if (band == NL80211_BAND_6GHZ) {
		mode |= PHY_MODE_A | PHY_MODE_AN |
			PHY_MODE_AC | PHY_MODE_AX_5G;
	}

	return mode;
}
EXPORT_SYMBOL_GPL(mt76_connac_get_phy_mode);

u8 mt76_connac_get_phy_mode_ext(struct mt76_phy *phy, struct ieee80211_bss_conf *conf,
				enum nl80211_band band)
{
	const struct ieee80211_sta_eht_cap *eht_cap;
	struct ieee80211_supported_band *sband;
	u8 mode = 0;

	if (band == NL80211_BAND_6GHZ)
		mode |= PHY_MODE_AX_6G;

	sband = phy->hw->wiphy->bands[band];
	eht_cap = ieee80211_get_eht_iftype_cap(sband, conf->vif->type);

	if (!eht_cap || !eht_cap->has_eht || !conf->eht_support)
		return mode;

	switch (band) {
	case NL80211_BAND_6GHZ:
		mode |= PHY_MODE_BE_6G;
		break;
	case NL80211_BAND_5GHZ:
		mode |= PHY_MODE_BE_5G;
		break;
	case NL80211_BAND_2GHZ:
		mode |= PHY_MODE_BE_24G;
		break;
	default:
		break;
	}

	return mode;
}
EXPORT_SYMBOL_GPL(mt76_connac_get_phy_mode_ext);

const struct ieee80211_sta_he_cap *
mt76_connac_get_he_phy_cap(struct mt76_phy *phy, struct ieee80211_vif *vif)
{
	struct mt76_vif_link *mvif = (struct mt76_vif_link *)vif->drv_priv;
	struct cfg80211_chan_def *chandef = mvif->ctx ?
					    &mvif->ctx->def : &phy->chandef;
	enum nl80211_band band = chandef->chan->band;
	struct ieee80211_supported_band *sband;

	sband = phy->hw->wiphy->bands[band];

	return ieee80211_get_he_iftype_cap(sband, vif->type);
}
EXPORT_SYMBOL_GPL(mt76_connac_get_he_phy_cap);

const struct ieee80211_sta_eht_cap *
mt76_connac_get_eht_phy_cap(struct mt76_phy *phy, struct ieee80211_vif *vif)
{
	enum nl80211_band band = phy->chandef.chan->band;
	struct ieee80211_supported_band *sband;

	sband = phy->hw->wiphy->bands[band];

	return ieee80211_get_eht_iftype_cap(sband, vif->type);
}
EXPORT_SYMBOL_GPL(mt76_connac_get_eht_phy_cap);

#define DEFAULT_HE_PE_DURATION		4
#define DEFAULT_HE_DURATION_RTS_THRES	1023
static void
mt76_connac_mcu_uni_bss_he_tlv(struct mt76_phy *phy, struct ieee80211_vif *vif,
			       struct tlv *tlv)
{
	const struct ieee80211_sta_he_cap *cap;
	struct bss_info_uni_he *he;

	cap = mt76_connac_get_he_phy_cap(phy, vif);
	if (!cap)
		return;

	he = (struct bss_info_uni_he *)tlv;
	he->he_pe_duration = vif->bss_conf.htc_trig_based_pkt_ext;
	if (!he->he_pe_duration)
		he->he_pe_duration = DEFAULT_HE_PE_DURATION;

	he->he_rts_thres = cpu_to_le16(vif->bss_conf.frame_time_rts_th);
	if (!he->he_rts_thres)
		he->he_rts_thres = cpu_to_le16(DEFAULT_HE_DURATION_RTS_THRES);

	he->max_nss_mcs[CMD_HE_MCS_BW80] = cap->he_mcs_nss_supp.tx_mcs_80;
	he->max_nss_mcs[CMD_HE_MCS_BW160] = cap->he_mcs_nss_supp.tx_mcs_160;
	he->max_nss_mcs[CMD_HE_MCS_BW8080] = cap->he_mcs_nss_supp.tx_mcs_80p80;
}

int mt76_connac_mcu_uni_set_chctx(struct mt76_phy *phy, struct mt76_vif_link *mvif,
				  struct ieee80211_chanctx_conf *ctx)
{
	struct cfg80211_chan_def *chandef = ctx ? &ctx->def : &phy->chandef;
	int freq1 = chandef->center_freq1, freq2 = chandef->center_freq2;
	enum nl80211_band band = chandef->chan->band;
	struct mt76_dev *mdev = phy->dev;
	struct {
		struct {
			u8 bss_idx;
			u8 pad[3];
		} __packed hdr;
		struct rlm_tlv {
			__le16 tag;
			__le16 len;
			u8 control_channel;
			u8 center_chan;
			u8 center_chan2;
			u8 bw;
			u8 tx_streams;
			u8 rx_streams;
			u8 short_st;
			u8 ht_op_info;
			u8 sco;
			u8 band;
			u8 pad[2];
		} __packed rlm;
	} __packed rlm_req = {
		.hdr = {
			.bss_idx = mvif->idx,
		},
		.rlm = {
			.tag = cpu_to_le16(UNI_BSS_INFO_RLM),
			.len = cpu_to_le16(sizeof(struct rlm_tlv)),
			.control_channel = chandef->chan->hw_value,
			.center_chan = ieee80211_frequency_to_channel(freq1),
			.center_chan2 = ieee80211_frequency_to_channel(freq2),
			.tx_streams = hweight8(phy->antenna_mask),
			.ht_op_info = 4, /* set HT 40M allowed */
			.rx_streams = phy->chainmask,
			.short_st = true,
			.band = band,
		},
	};

	switch (chandef->width) {
	case NL80211_CHAN_WIDTH_40:
		rlm_req.rlm.bw = CMD_CBW_40MHZ;
		break;
	case NL80211_CHAN_WIDTH_80:
		rlm_req.rlm.bw = CMD_CBW_80MHZ;
		break;
	case NL80211_CHAN_WIDTH_80P80:
		rlm_req.rlm.bw = CMD_CBW_8080MHZ;
		break;
	case NL80211_CHAN_WIDTH_160:
		rlm_req.rlm.bw = CMD_CBW_160MHZ;
		break;
	case NL80211_CHAN_WIDTH_5:
		rlm_req.rlm.bw = CMD_CBW_5MHZ;
		break;
	case NL80211_CHAN_WIDTH_10:
		rlm_req.rlm.bw = CMD_CBW_10MHZ;
		break;
	case NL80211_CHAN_WIDTH_20_NOHT:
	case NL80211_CHAN_WIDTH_20:
	default:
		rlm_req.rlm.bw = CMD_CBW_20MHZ;
		rlm_req.rlm.ht_op_info = 0;
		break;
	}

	if (rlm_req.rlm.control_channel < rlm_req.rlm.center_chan)
		rlm_req.rlm.sco = 1; /* SCA */
	else if (rlm_req.rlm.control_channel > rlm_req.rlm.center_chan)
		rlm_req.rlm.sco = 3; /* SCB */

	return mt76_mcu_send_msg(mdev, MCU_UNI_CMD(BSS_INFO_UPDATE), &rlm_req,
				 sizeof(rlm_req), true);
}
EXPORT_SYMBOL_GPL(mt76_connac_mcu_uni_set_chctx);

int mt76_connac_mcu_uni_add_bss(struct mt76_phy *phy,
				struct ieee80211_vif *vif,
				struct mt76_wcid *wcid,
				bool enable,
				struct ieee80211_chanctx_conf *ctx)
{
	struct mt76_vif_link *mvif = (struct mt76_vif_link *)vif->drv_priv;
	struct cfg80211_chan_def *chandef = ctx ? &ctx->def : &phy->chandef;
	enum nl80211_band band = chandef->chan->band;
	struct mt76_dev *mdev = phy->dev;
	struct {
		struct {
			u8 bss_idx;
			u8 pad[3];
		} __packed hdr;
		struct mt76_connac_bss_basic_tlv basic;
		struct mt76_connac_bss_qos_tlv qos;
	} basic_req = {
		.hdr = {
			.bss_idx = mvif->idx,
		},
		.basic = {
			.tag = cpu_to_le16(UNI_BSS_INFO_BASIC),
			.len = cpu_to_le16(sizeof(struct mt76_connac_bss_basic_tlv)),
			.bcn_interval = cpu_to_le16(vif->bss_conf.beacon_int),
			.dtim_period = vif->bss_conf.dtim_period,
			.omac_idx = mvif->omac_idx,
			.band_idx = mvif->band_idx,
			.wmm_idx = mvif->wmm_idx,
			.active = true, /* keep bss deactivated */
			.phymode = mt76_connac_get_phy_mode(phy, vif, band, NULL),
		},
		.qos = {
			.tag = cpu_to_le16(UNI_BSS_INFO_QBSS),
			.len = cpu_to_le16(sizeof(struct mt76_connac_bss_qos_tlv)),
			.qos = vif->bss_conf.qos,
		},
	};
	int err, conn_type;
	u8 idx, basic_phy;

	idx = mvif->omac_idx > EXT_BSSID_START ? HW_BSSID_0 : mvif->omac_idx;
	basic_req.basic.hw_bss_idx = idx;
	if (band == NL80211_BAND_6GHZ)
		basic_req.basic.phymode_ext = PHY_MODE_AX_6G;

	basic_phy = mt76_connac_get_phy_mode_v2(phy, vif, band, NULL);
	basic_req.basic.nonht_basic_phy = cpu_to_le16(basic_phy);

	switch (vif->type) {
	case NL80211_IFTYPE_MESH_POINT:
	case NL80211_IFTYPE_AP:
		if (vif->p2p)
			conn_type = CONNECTION_P2P_GO;
		else
			conn_type = CONNECTION_INFRA_AP;
		basic_req.basic.conn_type = cpu_to_le32(conn_type);
		/* Fully active/deactivate BSS network in AP mode only */
		basic_req.basic.active = enable;
		break;
	case NL80211_IFTYPE_STATION:
		if (vif->p2p)
			conn_type = CONNECTION_P2P_GC;
		else
			conn_type = CONNECTION_INFRA_STA;
		basic_req.basic.conn_type = cpu_to_le32(conn_type);
		break;
	case NL80211_IFTYPE_ADHOC:
		basic_req.basic.conn_type = cpu_to_le32(CONNECTION_IBSS_ADHOC);
		break;
	case NL80211_IFTYPE_NAN:
	case NL80211_IFTYPE_NAN_DATA:
		basic_req.basic.conn_type = cpu_to_le32(CONNECTION_NAN);
		basic_req.basic.active = enable;
		break;
	default:
		WARN_ON(1);
		break;
	}

	memcpy(basic_req.basic.bssid, vif->bss_conf.bssid, ETH_ALEN);
	basic_req.basic.bmc_tx_wlan_idx = cpu_to_le16(wcid->idx);
	basic_req.basic.sta_idx = cpu_to_le16(wcid->idx);
	basic_req.basic.conn_state = !enable;

	err = mt76_mcu_send_msg(mdev, MCU_UNI_CMD(BSS_INFO_UPDATE), &basic_req,
				sizeof(basic_req), true);
	if (err < 0)
		return err;

	if (vif->bss_conf.he_support) {
		struct {
			struct {
				u8 bss_idx;
				u8 pad[3];
			} __packed hdr;
			struct bss_info_uni_he he;
			struct bss_info_uni_bss_color bss_color;
		} he_req = {
			.hdr = {
				.bss_idx = mvif->idx,
			},
			.he = {
				.tag = cpu_to_le16(UNI_BSS_INFO_HE_BASIC),
				.len = cpu_to_le16(sizeof(struct bss_info_uni_he)),
			},
			.bss_color = {
				.tag = cpu_to_le16(UNI_BSS_INFO_BSS_COLOR),
				.len = cpu_to_le16(sizeof(struct bss_info_uni_bss_color)),
				.enable = 0,
				.bss_color = 0,
			},
		};

		if (enable) {
			he_req.bss_color.enable =
				vif->bss_conf.he_bss_color.enabled;
			he_req.bss_color.bss_color =
				vif->bss_conf.he_bss_color.color;
		}

		mt76_connac_mcu_uni_bss_he_tlv(phy, vif,
					       (struct tlv *)&he_req.he);
		err = mt76_mcu_send_msg(mdev, MCU_UNI_CMD(BSS_INFO_UPDATE),
					&he_req, sizeof(he_req), true);
		if (err < 0)
			return err;
	}

	if (enable && vif->bss_conf.bssid_indicator) {
		struct {
			struct {
				u8 bss_idx;
				u8 pad[3];
			} __packed hdr;
			struct bss_info_uni_mbssid mbssid;
		} mbssid_req = {
			.hdr = {
				.bss_idx = mvif->idx,
			},
			.mbssid = {
				.tag = cpu_to_le16(UNI_BSS_INFO_11V_MBSSID),
				.len = cpu_to_le16(sizeof(struct bss_info_uni_mbssid)),
				.max_indicator = vif->bss_conf.bssid_indicator,
				.mbss_idx =  vif->bss_conf.bssid_index,
			},
		};

		err = mt76_mcu_send_msg(mdev, MCU_UNI_CMD(BSS_INFO_UPDATE),
					&mbssid_req, sizeof(mbssid_req), true);
		if (err < 0)
			return err;
	}

	return mt76_connac_mcu_uni_set_chctx(phy, mvif, ctx);
}
EXPORT_SYMBOL_GPL(mt76_connac_mcu_uni_add_bss);

void mt76_connac_mcu_build_rnr_scan_param(struct mt76_dev *mdev,
					  struct cfg80211_scan_request *sreq)
{
	struct ieee80211_channel **scan_list = sreq->channels;
	int i, bssid_index = 0;

	/* clear 6G active Scan BSSID table */
	memset(&mdev->rnr, 0, sizeof(mdev->rnr));

	for (i = 0; i < sreq->n_6ghz_params; i++) {
		u8 ch_idx = sreq->scan_6ghz_params[i].channel_idx;
		int k = 0;

		/* Remove the duplicated BSSID */
		for (k = 0; k < bssid_index; k++) {
			if (!memcmp(&mdev->rnr.bssid[k],
				    sreq->scan_6ghz_params[i].bssid,
				    ETH_ALEN))
				break;
		}

		if (k == bssid_index &&
		    bssid_index < MT76_RNR_SCAN_MAX_BSSIDS) {
			memcpy(&mdev->rnr.bssid[bssid_index++],
			       sreq->scan_6ghz_params[i].bssid, ETH_ALEN);
			mdev->rnr.channel[k] = scan_list[ch_idx]->hw_value;
		}
	}

	mdev->rnr.bssid_num = bssid_index;

	if (sreq->flags & NL80211_SCAN_FLAG_RANDOM_ADDR) {
		memcpy(mdev->rnr.random_mac, sreq->mac_addr, ETH_ALEN);
		mdev->rnr.sreq_flag = sreq->flags;
	}
}
EXPORT_SYMBOL_GPL(mt76_connac_mcu_build_rnr_scan_param);

#define MT76_CONNAC_SCAN_CHANNEL_TIME		60
#define MT7932_SCAN_REQ_V2_SIZE			0x4d4

/* MT7932's public CMD_SCAN_REQ_V2 and AppleSunriseWLAN 25G83 both place a
 * naturally aligned PARAM_SSID[6] after the extended channel list.  mt76's
 * older connac2 request is fully packed, so its BSSID and every following
 * field are two bytes early.  Padding the old structure to 0x4d4 preserved
 * the wrong internal offsets and corrupted MT7932's last extended SSID.
 */
struct mt7932_connac_hw_scan_req {
	u8 seq_num;
	u8 bss_idx;
	u8 scan_type;
	u8 ssid_type;
	u8 ssids_num;
	u8 probe_req_num;
	u8 scan_func;
	u8 version;
	struct mt76_connac_mcu_scan_ssid ssids[4];
	__le16 probe_delay_time;
	__le16 channel_dwell_time;
	__le16 timeout_value;
	u8 channel_type;
	u8 channels_num;
	struct mt76_connac_mcu_scan_channel channels[32];
	__le16 ies_len;
	u8 ies[MT76_CONNAC_SCAN_IE_LEN];
	u8 ext_channels_num;
	u8 ext_ssids_num;
	__le16 channel_min_dwell_time;
	struct mt76_connac_mcu_scan_channel ext_channels[32];
	u8 ext_ssids_align[2];
	struct mt76_connac_mcu_scan_ssid ext_ssids[6];
	u8 bssid[ETH_ALEN];
	u8 random_mac[ETH_ALEN];
	u8 ext_bssid[10][ETH_ALEN];
	u8 short_ssids_num;
	u8 bssid_match_channel[10];
	u8 bssid_match_ssid_index[10];
	/* AppleSunriseWLAN 25G83 still allocates 0x4d4 bytes, but its final
	 * 31-byte region is no longer padding.  gl_io80211_scan_request fills
	 * the ordinary unassociated scan timings as 110 ms passive dwell,
	 * 45 ms operating-channel stay, and 110 ms home/away time.  The dext's
	 * scnSendScanReqV2 copies them to 0x4c2, 0x4bc and 0x4ca respectively.
	 */
	u8 tail_align[3];
	__le32 scan_func_ext;
	__le32 op_channel_stay_time;
	u8 tail_reserved0[2];
	__le16 passive_channel_dwell_time;
	u8 tail_reserved1[6];
	__le16 home_away_time;
	u8 operating_channel;
	u8 operating_band;
	u8 tail_reserved2[6];
} __packed;

static const u8 mt7932_scan_ie[] = {
	0x7f, 0x0a, 0x01, 0x00, 0x08, 0x8c, 0x01,
	0x40, 0x00, 0x00, 0x00, 0x00, 0x6b, 0x07,
	0x0f, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
};

#define MT7932_CMD_SCAN_POLICY_PREP	0x9e
#define MT7932_CMD_SCAN_PROFILE		0x7b
#define MT7932_PRE_SCAN_TRANSCRIPT_NAME \
	"mediatek/j700-mt7932-pre-scan-transcript.bin"
MODULE_FIRMWARE(MT7932_PRE_SCAN_TRANSCRIPT_NAME);
#define MT7932_PRE_SCAN_RECORDS	31
#define MT7932_PRE_SCAN_RECORD_SIZE	48

struct mt7932_pre_scan_header {
	u8 magic[8];
	__le16 version;
	__le16 record_size;
	__le32 records;
	u8 source_sha256[32];
} __packed;

struct mt7932_pre_scan_record {
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

static const u8 mt7932_pre_scan_source_sha256[32] = {
	0x23, 0xfb, 0xbe, 0x28, 0xa3, 0x08, 0x05, 0xcb,
	0xb6, 0x13, 0x53, 0x01, 0x35, 0x5b, 0x48, 0x49,
	0xab, 0x98, 0xc5, 0xd9, 0x91, 0x4d, 0x48, 0xf0,
	0xc9, 0xe4, 0x20, 0x39, 0x39, 0x75, 0xe0, 0x6e,
};

static int mt7932_scan_profile;
module_param_named(j700_scan_profile, mt7932_scan_profile, int, 0644);
MODULE_PARM_DESC(j700_scan_profile,
		 "J700 scan path: 0=native bounded batches, 1=ordinal192, 2=ordinal195, 3=ordinal198");

static bool mt7932_scan_policy_prepared;

static bool mt7932_scan_policy_prefix = true;
module_param_named(j700_scan_policy_prefix, mt7932_scan_policy_prefix,
		   bool, 0644);
MODULE_PARM_DESC(j700_scan_policy_prefix,
		 "send captured J700 scan-policy CIDs 0x9e,0x9e,0x7b");

static int mt7932_pre_scan_first;
module_param_named(j700_pre_scan_first, mt7932_pre_scan_first, int, 0644);
MODULE_PARM_DESC(j700_pre_scan_first,
		 "first captured J700 pre-scan ordinal to replay (160..190, zero disables)");

static int mt7932_pre_scan_last;
module_param_named(j700_pre_scan_last, mt7932_pre_scan_last, int, 0644);
MODULE_PARM_DESC(j700_pre_scan_last,
		 "last captured J700 pre-scan ordinal to replay (160..190)");

static bool mt7932_pre_scan_replayed;

static int mt7932_connac_mcu_pre_scan_replay(struct mt76_dev *mdev)
{
	static const u8 magic[8] = { 'J', '7', 'S', 'C', 'N', 'T', 'R', '1' };
	const struct mt7932_pre_scan_header *header;
	const struct firmware *fw;
	size_t offset;
	int first = READ_ONCE(mt7932_pre_scan_first);
	int last = READ_ONCE(mt7932_pre_scan_last);
	u32 i, count, sent = 0;
	int ret;

	if (!first)
		return 0;
	if (mt7932_pre_scan_replayed)
		return 0;
	if (first < 160 || last < first || last > 190)
		return -EINVAL;

	ret = request_firmware(&fw, MT7932_PRE_SCAN_TRANSCRIPT_NAME,
			       mdev->dev);
	if (ret)
		return ret;
	if (fw->size < sizeof(*header)) {
		ret = -EINVAL;
		goto out;
	}
	header = (const void *)fw->data;
	count = le32_to_cpu(header->records);
	if (memcmp(header->magic, magic, sizeof(magic)) ||
	    le16_to_cpu(header->version) != 1 ||
	    le16_to_cpu(header->record_size) != MT7932_PRE_SCAN_RECORD_SIZE ||
	    count != MT7932_PRE_SCAN_RECORDS ||
	    memcmp(header->source_sha256, mt7932_pre_scan_source_sha256,
		   sizeof(mt7932_pre_scan_source_sha256))) {
		ret = -EINVAL;
		goto out;
	}

	offset = sizeof(*header);
	for (i = 0; i < count; i++) {
		const struct mt7932_pre_scan_record *record;
		const u8 *data;
		u32 ordinal, data_len;
		int cmd;

		if (sizeof(*record) > fw->size - offset) {
			ret = -EINVAL;
			goto out;
		}
		record = (const void *)(fw->data + offset);
		offset += sizeof(*record);
		ordinal = le32_to_cpu(record->ordinal);
		data_len = le32_to_cpu(record->data_len);
		if (record->phase != 3 || ordinal != 160 + i ||
		    data_len > 2048 || data_len > fw->size - offset ||
		    le16_to_cpu(record->reserved) || record->transport > 2) {
			ret = -EINVAL;
			goto out;
		}
		data = fw->data + offset;
		offset += data_len;
		if (ordinal < first || ordinal > last)
			continue;
		/* The runtime range selector intentionally accepts only captured SET,
		 * no-ACK records.  Query/response records need separate event ordering
		 * and are rejected rather than silently desynchronizing the stream.
		 */
		if (!record->set_query || record->ext_cid_ack) {
			ret = -EOPNOTSUPP;
			goto out;
		}
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
			      FIELD_PREP(__MCU_CMD_FIELD_ID,
					 record->ext_cid);
			break;
		default:
			ret = -EINVAL;
			goto out;
		}
		ret = mt76_mcu_send_msg(mdev, cmd, data, data_len, false);
		if (ret)
			goto out;
		sent++;
	}
	if (offset != fw->size || sent != last - first + 1) {
		ret = -EINVAL;
		goto out;
	}
	mt7932_pre_scan_replayed = true;
	dev_info(mdev->dev,
		 "J700_MT7932_PRE_SCAN_REPLAY_PASS: ordinals=%d-%d commands=%u source=25G83-macos-ssh-tx17 target_writes=0\n",
		 first, last, sent);
	ret = 0;
out:
	release_firmware(fw);
	return ret;
}

static int
mt7932_connac_mcu_scan_prefix(struct mt76_phy *phy,
			      struct ieee80211_vif *vif)
{
	struct mt76_vif_link *mvif = (struct mt76_vif_link *)vif->drv_priv;
	struct mt76_dev *mdev = phy->dev;
	static const u8 policy_prep[] = { 0x1a, 0x05, 0x10, 0x00 };
	static const u8 profile[] = {
		0x03, 0x00, 0x28, 0x01, 0x02, 0x00, 0x01, 0x00,
		0x00, 0x00, 0xba, 0xbf, 0xba, 0xbf, 0xba, 0xbf,
		0xba, 0xbf, 0xba, 0xba, 0x00, 0x00, 0x00, 0x00,
		0x08, 0x63, 0x63, 0x63, 0x63, 0x1e, 0x1e, 0x00,
		0x08, 0x63, 0x63, 0x08, 0x1e, 0x1e, 0x1e, 0x00,
		0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
		0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
		0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
	};
	struct {
		u8 bss_idx;
		u8 active;
		u8 network_type;
		u8 own_mac_idx;
		u8 bss_mac_addr[ETH_ALEN];
		u8 bmc_wlan_idx;
		u8 mt7932_net_active;
	} __packed activate = {
		.bss_idx = mvif->idx,
		.active = 1,
		.network_type = vif->type == NL80211_IFTYPE_P2P_DEVICE ? 1 :
				vif->type == NL80211_IFTYPE_AP ? 3 : 0,
		.own_mac_idx = mvif->omac_idx,
		.bmc_wlan_idx = 0,
		.mt7932_net_active = 1,
	};
	int cmd, err;

	BUILD_BUG_ON(sizeof(profile) != 64);
	BUILD_BUG_ON(sizeof(activate) != 12);
	memcpy(activate.bss_mac_addr, vif->addr, ETH_ALEN);

	if (mt7932_scan_policy_prefix && !mt7932_scan_policy_prepared) {
		cmd = __MCU_CMD_FIELD_CE |
		      FIELD_PREP(__MCU_CMD_FIELD_ID,
				 MT7932_CMD_SCAN_POLICY_PREP);
		err = mt76_mcu_send_msg(mdev, cmd, policy_prep,
					 sizeof(policy_prep), false);
		if (err)
			return err;
		err = mt76_mcu_send_msg(mdev, cmd, policy_prep,
					 sizeof(policy_prep), false);
		if (err)
			return err;

		cmd = __MCU_CMD_FIELD_CE |
		      FIELD_PREP(__MCU_CMD_FIELD_ID,
				 MT7932_CMD_SCAN_PROFILE);
		err = mt76_mcu_send_msg(mdev, cmd, profile,
					 sizeof(profile), false);
		if (err)
			return err;
		mt7932_scan_policy_prepared = true;
	}

	err = mt76_mcu_send_msg(mdev, MCU_CE_CMD(BSS_ACTIVATE_CTRL),
				 &activate, sizeof(activate), false);
	if (!err) {
		dev_info(mdev->dev,
			 "J700_MT7932_SCAN_PREFIX_PASS: policy_prepared=%u cids=9e,9e,7b-once,11-per-scan profile_sha256=26954cdd... bss=%u identity=live source=25G83-ordinals188-198\n",
			 mt7932_scan_policy_prepared, activate.bss_idx);
	}

	return err;
}

enum mt7932_uni_scan_tag {
	MT7932_UNI_SCAN_REQ = 1,
	MT7932_UNI_SCAN_CANCEL = 2,
	MT7932_UNI_SCAN_SSID = 10,
	MT7932_UNI_SCAN_BSSID = 11,
	MT7932_UNI_SCAN_CHANNEL = 12,
	MT7932_UNI_SCAN_IE = 13,
	MT7932_UNI_SCAN_MISC = 14,
};

struct mt7932_uni_scan_hdr {
	u8 seq_num;
	u8 bss_idx;
	u8 pad[2];
} __packed;

struct mt7932_uni_scan_req {
	__le16 tag;
	__le16 len;
	u8 scan_type;
	u8 probe_req_num;
	u8 scan_func;
	u8 pad;
	__le16 channel_min_dwell_time;
	__le16 channel_dwell_time;
	__le16 timeout_value;
	__le16 probe_delay_time;
} __packed;

struct mt7932_uni_scan_ssid {
	__le16 tag;
	__le16 len;
	u8 ssid_type;
	u8 ssids_num;
	u8 pad[2];
	struct mt76_connac_mcu_scan_ssid ssids[];
} __packed;

struct mt7932_uni_scan_bssid {
	__le16 tag;
	__le16 len;
	u8 bssid[ETH_ALEN];
	u8 match_ch;
	u8 match_ssid_ind;
} __packed;

struct mt7932_uni_scan_channel {
	__le16 tag;
	__le16 len;
	u8 channel_type;
	u8 channels_num;
	u8 pad[2];
	struct mt76_connac_mcu_scan_channel channels[];
} __packed;

struct mt7932_uni_scan_ie {
	__le16 tag;
	__le16 len;
	__le16 ies_len;
	u8 pad[2];
	u8 ies[];
} __packed;

struct mt7932_uni_scan_misc {
	__le16 tag;
	__le16 len;
	u8 random_mac[ETH_ALEN];
	u8 short_ssids_num;
	u8 reserved;
} __packed;

static int __maybe_unused
mt7932_connac_mcu_hw_scan_uni(struct mt76_phy *phy,
			      struct ieee80211_vif *vif,
			      struct ieee80211_scan_request *scan_req)
{
	struct mt76_vif_link *mvif = (struct mt76_vif_link *)vif->drv_priv;
	struct cfg80211_scan_request *sreq = &scan_req->req;
	struct mt76_dev *mdev = phy->dev;
	struct mt7932_uni_scan_channel *channel;
	struct mt7932_uni_scan_bssid *bssid;
	struct mt7932_uni_scan_ssid *ssid;
	struct mt7932_uni_scan_misc *misc;
	struct mt7932_uni_scan_req *req;
	struct mt7932_uni_scan_hdr *hdr;
	struct mt7932_uni_scan_ie *ie;
	struct sk_buff *skb;
	int duration = 40;
	int n_channels;
	int n_ssids = 0;
	int len, err, i;

	BUILD_BUG_ON(sizeof(*hdr) != 4);
	BUILD_BUG_ON(sizeof(*req) != 16);
	BUILD_BUG_ON(sizeof(*ssid) != 8);
	BUILD_BUG_ON(sizeof(*bssid) != 12);
	BUILD_BUG_ON(sizeof(*channel) != 8);
	BUILD_BUG_ON(sizeof(*ie) != 8);
	BUILD_BUG_ON(sizeof(*misc) != 12);

	if (test_bit(MT76_HW_SCANNING, &phy->state))
		return -EBUSY;

	if (sreq->ie_len > MT76_CONNAC_SCAN_IE_LEN)
		return -EINVAL;

	for (i = 0; i < sreq->n_ssids && n_ssids < 10; i++)
		if (sreq->ssids[i].ssid_len)
			n_ssids++;

	/* Cycle611 proved the unified command starts real channel traversal, but
	 * the 39-channel request had not completed before userspace's 20-second
	 * retry aborted it.  Bound this discriminator to the 2.4 GHz subset so a
	 * completion event distinguishes channel-plan/dwell policy from a channel
	 * grant failure without changing radio power or association state.
	 */
	n_channels = 0;
	for (i = 0; i < sreq->n_channels; i++)
		if (sreq->channels[i]->band == NL80211_BAND_2GHZ)
			n_channels++;
	len = sizeof(*hdr) + sizeof(*req) + sizeof(*ssid) +
	      n_ssids * sizeof(struct mt76_connac_mcu_scan_ssid) +
	      sizeof(*bssid) + sizeof(*channel) +
	      n_channels * sizeof(struct mt76_connac_mcu_scan_channel) +
	      sizeof(*ie) + sreq->ie_len + sizeof(*misc);

	skb = mt76_mcu_msg_alloc(mdev, NULL, len);
	if (!skb)
		return -ENOMEM;

	set_bit(MT76_HW_SCANNING, &phy->state);
	mvif->scan_seq_num = (mvif->scan_seq_num + 1) & 0x7f;

	/* The 25G83 driver passes CMD_SCAN_REQ_V2 to MediaTek's public
	 * nicUniCmdScanReqV2 converter.  Reproduce its actual wire format here:
	 * a four-byte fixed header followed by six variable-length TLVs.  The
	 * legacy 0x4d4-byte structure is the converter input, not firmware ABI.
	 */
	hdr = skb_put_zero(skb, sizeof(*hdr));
	hdr->seq_num = mvif->scan_seq_num | mvif->band_idx << 7;
	hdr->bss_idx = mvif->idx;

	req = skb_put_zero(skb, sizeof(*req));
	req->tag = cpu_to_le16(MT7932_UNI_SCAN_REQ);
	req->len = cpu_to_le16(sizeof(*req));
	/* Apple 25G83's ordinary wildcard AIS scan is active type 1 while leaving
	 * the host probe count and timeout at zero.  Keep the values measured in
	 * the 0x4d4 converter input when projecting it into the unified TLVs.
	 */
	req->scan_type = 1;
	req->probe_req_num = 0;
	req->scan_func = BIT(1); /* Apple ENUM_SCN_DBDC_SCAN_DIS */
	req->channel_min_dwell_time = cpu_to_le16(20);
	req->channel_dwell_time = cpu_to_le16(duration);
	req->timeout_value = 0;

	ssid = skb_put_zero(skb, sizeof(*ssid) +
			    n_ssids * sizeof(struct mt76_connac_mcu_scan_ssid));
	ssid->tag = cpu_to_le16(MT7932_UNI_SCAN_SSID);
	ssid->len = cpu_to_le16(sizeof(*ssid) +
				n_ssids * sizeof(struct mt76_connac_mcu_scan_ssid));
	ssid->ssid_type = n_ssids ? BIT(2) : BIT(0);
	ssid->ssids_num = n_ssids;
	for (i = 0, n_ssids = 0; i < sreq->n_ssids && n_ssids < 10; i++) {
		if (!sreq->ssids[i].ssid_len)
			continue;
		ssid->ssids[n_ssids].ssid_len =
			cpu_to_le32(sreq->ssids[i].ssid_len);
		memcpy(ssid->ssids[n_ssids].ssid, sreq->ssids[i].ssid,
		       sreq->ssids[i].ssid_len);
		n_ssids++;
	}

	bssid = skb_put_zero(skb, sizeof(*bssid));
	bssid->tag = cpu_to_le16(MT7932_UNI_SCAN_BSSID);
	bssid->len = cpu_to_le16(sizeof(*bssid));
	memcpy(bssid->bssid, sreq->bssid, ETH_ALEN);

	channel = skb_put_zero(skb, sizeof(*channel) +
			       n_channels * sizeof(*channel->channels));
	channel->tag = cpu_to_le16(MT7932_UNI_SCAN_CHANNEL);
	channel->len = cpu_to_le16(sizeof(*channel) +
				   n_channels * sizeof(*channel->channels));
	channel->channel_type = n_channels ? 4 : 0;
	channel->channels_num = n_channels;
	for (i = 0, n_channels = 0; i < sreq->n_channels; i++) {
		if (sreq->channels[i]->band != NL80211_BAND_2GHZ)
			continue;
		channel->channels[n_channels].band = 1;
		channel->channels[n_channels].channel_num =
			sreq->channels[i]->hw_value;
		n_channels++;
	}

	ie = skb_put_zero(skb, sizeof(*ie) + sreq->ie_len);
	ie->tag = cpu_to_le16(MT7932_UNI_SCAN_IE);
	ie->len = cpu_to_le16(sizeof(*ie) + sreq->ie_len);
	ie->ies_len = cpu_to_le16(sreq->ie_len);
	if (sreq->ie_len)
		memcpy(ie->ies, sreq->ie, sreq->ie_len);

	misc = skb_put_zero(skb, sizeof(*misc));
	misc->tag = cpu_to_le16(MT7932_UNI_SCAN_MISC);
	misc->len = cpu_to_le16(sizeof(*misc));
	if (sreq->flags & NL80211_SCAN_FLAG_RANDOM_ADDR) {
		get_random_mask_addr(misc->random_mac, sreq->mac_addr,
				     sreq->mac_addr_mask);
		req->scan_func |= SCAN_FUNC_RANDOM_MAC;
	}

	dev_info(mdev->dev,
		 "J700_MT7932_UNI_SCAN_REQ: bytes=0x%x seq=%u bss=%u type=%u ssids=%u channels=%u ie=%zu func=0x%02x plan=2g-bounded-diagnostic\n",
		 skb->len, hdr->seq_num, hdr->bss_idx, req->scan_type,
		 ssid->ssids_num, channel->channels_num, sreq->ie_len,
		 req->scan_func);
	err = mt76_mcu_skb_send_msg(mdev, skb, MCU_UNI_CMD(SCAN_REQ), false);
	dev_info(mdev->dev, "J700_MT7932_UNI_SCAN_SUBMIT: ret=%d\n", err);
	if (err < 0)
		clear_bit(MT76_HW_SCANNING, &phy->state);

	return err;
}

#define MT7932_SCAN_BATCH_CHANNELS 25

static u8 mt7932_scan_band(enum nl80211_band band)
{
	switch (band) {
	case NL80211_BAND_2GHZ:
		return 1;
	case NL80211_BAND_6GHZ:
		return 3;
	default:
		return 2;
	}
}

static struct mt7932_connac_hw_scan_req *
mt7932_scan_batch_alloc(struct mt76_phy *phy,
			const struct mt7932_connac_hw_scan_req *template,
			struct sk_buff **skbp, u8 seq)
{
	struct mt7932_connac_hw_scan_req *req;
	struct sk_buff *skb;

	skb = mt76_mcu_msg_alloc(phy->dev, NULL, sizeof(*req));
	if (!skb)
		return NULL;
	skb_put_data(skb, template, sizeof(*req));
	req = (void *)skb->data;
	req->seq_num = seq;
	req->channel_type = 4;
	req->channels_num = 0;
	req->ext_channels_num = 0;
	memset(req->channels, 0, sizeof(req->channels));
	memset(req->ext_channels, 0, sizeof(req->ext_channels));
	*skbp = skb;

	return req;
}

static int
mt7932_scan_queue_batches(struct mt76_phy *phy, struct mt76_vif_link *mvif,
			  const struct mt7932_connac_hw_scan_req *template,
			  struct ieee80211_channel **channels,
			  int n_channels)
{
	struct mt7932_connac_hw_scan_req *req = NULL;
	struct sk_buff *skb = NULL;
	u8 seq = template->seq_num ?: 1;
	u8 last_seq = seq;
	int band, i, batches = 0;

	skb_queue_purge(&phy->scan_pending);
	for (band = 0; band < NUM_NL80211_BANDS; band++) {
		for (i = 0; i < n_channels; i++) {
			struct mt76_connac_mcu_scan_channel *channel;

			if (channels[i]->band != band)
				continue;
			/* Apple 25G83's complete measured 2.4 GHz campaign is
			 * channels 1..13.  cfg80211's world-domain list can retain
			 * channel 14 even though this firmware campaign does not.
			 */
			if (band == NL80211_BAND_2GHZ &&
			    channels[i]->hw_value > 13)
				continue;
			if (!req || req->channels_num == MT7932_SCAN_BATCH_CHANNELS) {
				if (skb) {
					skb_queue_tail(&phy->scan_pending, skb);
					batches++;
				}
				req = mt7932_scan_batch_alloc(phy, template, &skb,
							seq);
				if (!req)
					goto nomem;
				last_seq = seq;
				seq = (seq + 1) & 0x7f;
				if (!seq)
					seq = 1;
			}
			channel = &req->channels[req->channels_num++];
			channel->band = mt7932_scan_band(channels[i]->band);
			channel->channel_num = channels[i]->hw_value;
		}
		if (skb) {
			skb_queue_tail(&phy->scan_pending, skb);
			batches++;
			skb = NULL;
			req = NULL;
		}
	}

	if (!batches) {
		req = mt7932_scan_batch_alloc(phy, template, &skb, seq);
		if (!req)
			goto nomem;
		last_seq = seq;
		req->channel_type = 0;
		skb_queue_tail(&phy->scan_pending, skb);
		batches = 1;
	}
	mvif->scan_seq_num = last_seq;
	dev_info(phy->dev->dev,
		 "J700_MT7932_SCAN_BATCH_QUEUE: batches=%d channels=%d max=%u source=apple25G83-ordinals257,259\n",
		 batches, n_channels, MT7932_SCAN_BATCH_CHANNELS);

	return 0;

nomem:
	dev_kfree_skb(skb);
	skb_queue_purge(&phy->scan_pending);
	return -ENOMEM;
}

int mt76_connac_mcu_hw_scan_next(struct mt76_phy *phy)
{
	struct mt7932_connac_hw_scan_req *req;
	struct sk_buff *skb;
	u8 seq, channels, first_band, first_channel;
	int err, remaining;

	skb = skb_dequeue(&phy->scan_pending);
	if (!skb)
		return 0;
	req = (void *)skb->data;
	remaining = skb_queue_len(&phy->scan_pending);
	seq = req->seq_num;
	channels = req->channels_num;
	first_band = req->channels[0].band;
	first_channel = req->channels[0].channel_num;
	WRITE_ONCE(phy->scan_active_seq, seq);
	err = mt76_mcu_skb_send_msg(phy->dev, skb,
				       MCU_CE_CMD(START_HW_SCAN), false);
	if (err) {
		WRITE_ONCE(phy->scan_active_seq, 0);
		skb_queue_purge(&phy->scan_pending);
		return err;
	}
	dev_info(phy->dev->dev,
		 "J700_MT7932_SCAN_BATCH_SUBMIT: seq=%u channels=%u first=%u/%u remaining=%d\n",
		 seq, channels, first_band, first_channel, remaining);

	return 1;
}
EXPORT_SYMBOL_GPL(mt76_connac_mcu_hw_scan_next);

int mt76_connac_mcu_hw_scan(struct mt76_phy *phy, struct ieee80211_vif *vif,
			    struct ieee80211_scan_request *scan_req)
{
	struct mt76_vif_link *mvif = (struct mt76_vif_link *)vif->drv_priv;
	struct cfg80211_scan_request *sreq = &scan_req->req;
	int n_ssids = 0, err, i, duration;
	int ext_channels_num = max_t(int, sreq->n_channels - 32, 0);
	struct ieee80211_channel **scan_list = sreq->channels;
	struct mt76_dev *mdev = phy->dev;
	struct mt76_connac_mcu_scan_channel *chan;
	struct mt76_connac_hw_scan_req *req;
	struct mt7932_connac_hw_scan_req *req7932 = NULL;
	struct sk_buff *skb;
	int req_len = sizeof(*req);
	bool mt7932 = mt76_chip(mdev) == 0x7932;
	bool mt7932_only_6ghz = false;
	bool mt7932_directed_ssid = false;

	if (mt7932 && sreq->n_channels) {
		mt7932_only_6ghz = true;
		for (i = 0; i < sreq->n_channels; i++)
			if (scan_list[i]->band != NL80211_BAND_6GHZ) {
				mt7932_only_6ghz = false;
				break;
			}
	}
	if (mt7932)
		for (i = 0; i < sreq->n_ssids; i++)
			if (sreq->ssids[i].ssid_len) {
				mt7932_directed_ssid = true;
				break;
			}

	/* The sealed 25G83 TX17 capture is post-conversion device-visible traffic:
	 * Apple places the complete 0x4d4-byte CMD_SCAN_REQ_V2 on TX17 as legacy
	 * CID 0x03.  The public gen4 unified converter is an optional framework
	 * path and does not describe this Apple build.  Cycles 611--614 followed
	 * that wrong path; Cycle610's legacy request was the only one to reach the
	 * firmware scan FSM.  Keep unified support above as reference while using
	 * the measured wire ABI here.
	 */

	/* AppleSunriseWLAN 25G83 sends the MT7932 CMD_SCAN_REQ_V2 as 0x4d4
	 * bytes.  The older mt76 connac2 layout ends after the 63-byte pad and
	 * ssid_type_ext; MT7932 instead carries ten extended BSSIDs, short-SSID
	 * count, two ten-byte match arrays, 31 bytes of padding and the explicit
	 * alignment hole before extended SSIDs.  Use that exact internal layout,
	 * not just the correct total allocation size.
	 */
	if (mt7932) {
		BUILD_BUG_ON(sizeof(*req7932) != MT7932_SCAN_REQ_V2_SIZE);
		BUILD_BUG_ON(offsetof(struct mt7932_connac_hw_scan_req,
				     ext_ssids) != 0x380);
		BUILD_BUG_ON(offsetof(struct mt7932_connac_hw_scan_req,
				     bssid) != 0x458);
		BUILD_BUG_ON(offsetof(struct mt7932_connac_hw_scan_req,
				     random_mac) != 0x45e);
		BUILD_BUG_ON(offsetof(struct mt7932_connac_hw_scan_req,
				     ext_bssid) != 0x464);
		BUILD_BUG_ON(offsetof(struct mt7932_connac_hw_scan_req,
				     scan_func_ext) != 0x4b8);
		BUILD_BUG_ON(offsetof(struct mt7932_connac_hw_scan_req,
				     op_channel_stay_time) != 0x4bc);
		BUILD_BUG_ON(offsetof(struct mt7932_connac_hw_scan_req,
				     passive_channel_dwell_time) != 0x4c2);
		BUILD_BUG_ON(offsetof(struct mt7932_connac_hw_scan_req,
				     home_away_time) != 0x4ca);
		req_len = sizeof(*req7932);
	}

	if (test_bit(MT76_HW_SCANNING, &phy->state))
		return -EBUSY;

	skb = mt76_mcu_msg_alloc(mdev, NULL, req_len);
	if (!skb)
		return -ENOMEM;

	set_bit(MT76_HW_SCANNING, &phy->state);
	mvif->scan_seq_num = (mvif->scan_seq_num + 1) & 0x7f;

	req = (struct mt76_connac_hw_scan_req *)skb_put_zero(skb, req_len);
	if (mt7932)
		req7932 = (struct mt7932_connac_hw_scan_req *)req;
	if (mt7932) {
		static const u8 random_mac[][ETH_ALEN] = {
			[1] = { 0xfa, 0x3f, 0xe5, 0xb5, 0x93, 0xc8 },
			[2] = { 0xb8, 0x96, 0xcd, 0x31, 0x27, 0x05 },
			[3] = { 0x8a, 0xf2, 0x58, 0xd5, 0x43, 0xb8 },
		};
		int profile = READ_ONCE(mt7932_scan_profile);

		/* Reproduce any of the three successful Apple scan requests without a
		 * rebuild.  Profile zero falls through to the generic legacy builder;
		 * profiles 1..3 are exact 25G83 ordinals 192, 195 and 198.
		 */
		if (!profile)
			goto generic_scan;
		if (profile < 1 || profile > 3) {
			err = -EINVAL;
			goto scan_error;
		}
		mvif->scan_seq_num = profile;
		req->seq_num = profile;
		req->bss_idx = mvif->idx;
		req->scan_type = profile == 2 ? 0 : 1;
		req->ssid_type = profile == 2 ? 0 : BIT(0);
		req->scan_func = 0x13;
		req->version = 1;
		req->channel_dwell_time = cpu_to_le16(profile == 1 ? 110 : 40);
		req->channel_type = 4;
		if (profile == 2) {
			req->channels_num = 3;
			req->channels[0].band = 1;
			req->channels[0].channel_num = 1;
			req->channels[1].band = 1;
			req->channels[1].channel_num = 6;
			req->channels[2].band = 1;
			req->channels[2].channel_num = 11;
		} else {
			req->channels_num = 1;
			req->channels[0].band = 2;
			req->channels[0].channel_num = 157;
		}
		req->ies_len = cpu_to_le16(sizeof(mt7932_scan_ie));
		memcpy(req->ies, mt7932_scan_ie, sizeof(mt7932_scan_ie));
		req->channel_min_dwell_time =
			cpu_to_le16(profile == 1 ? 20 : 40);
		memcpy(req7932->random_mac, random_mac[profile], ETH_ALEN);
		req7932->scan_func_ext =
			cpu_to_le32(profile == 2 ? 0x1800 : 0x1900);
		req7932->op_channel_stay_time = cpu_to_le32(45);
		req7932->passive_channel_dwell_time = cpu_to_le16(110);
		req7932->home_away_time = cpu_to_le16(110);

		err = mt7932_connac_mcu_pre_scan_replay(mdev);
		if (err)
			goto scan_error;
		err = mt7932_connac_mcu_scan_prefix(phy, vif);
		if (err)
			goto scan_error;

		dev_info(mdev->dev,
			 "J700_MT7932_SCAN_REQ_V2: source=25G83-ordinal%u profile=%d bytes=0x%x seq=%u bss=%u type=%u ssids=0 probes=0 channels=%u first=%u/%u dwell=%u min=%u timeout=0 ie=21 func=0x13/0x%08x random_identity=generated\n",
			 189 + profile * 3, profile, req_len, req->seq_num,
			 req->bss_idx, req->scan_type, req->channels_num,
			 req->channels[0].band, req->channels[0].channel_num,
			 le16_to_cpu(req->channel_dwell_time),
			 le16_to_cpu(req->channel_min_dwell_time),
			 le32_to_cpu(req7932->scan_func_ext));
		goto submit;
	}

generic_scan:
	req->seq_num = mvif->scan_seq_num | mvif->band_idx << 7;
	req->bss_idx = mvif->idx;
	/* Apple 25G83's measured wildcard request uses scan type 1 with zero host
	 * probes.  Type 0 left CID3 live at TX17 DIDX in Cycles 481/482.  Restore
	 * the proven parser ABI while the scan-only userspace gate still exits
	 * before association.
	 */
	/* A standalone 6 GHz PSC wildcard scan without RNR targets is passive.
	 * A populated SSID is itself a directed request and uses the measured
	 * active AIS selector; it must not be downgraded to a contradictory
	 * passive request merely because cfg80211 supplied no RNR list.  Keep the
	 * hardware-proven active wildcard behavior for ordinary 2.4/5 GHz scans.
	 */
	req->scan_type = mt7932 ?
		(mt7932_only_6ghz && !mt7932_directed_ssid &&
		 !sreq->n_6ghz_params ? 0 : 1) :
		(sreq->n_ssids ? 1 : 0);
	/* 25G83's ordinary active-wildcard AIS request leaves both the probe
	 * count and firmware timeout zero, selecting the MT7932 firmware defaults.
	 * Linux's generic connac values (2 probes and channels*dwell timeout)
	 * diverge from the exact request before the first scan is accepted.
	 */
	req->probe_req_num = mt7932 ? 0 : (sreq->n_ssids ? 2 : 0);
	req->version = 1;

	for (i = 0; i < sreq->n_ssids; i++) {
		if (!sreq->ssids[i].ssid_len)
			continue;

		req->ssids[n_ssids].ssid_len = cpu_to_le32(sreq->ssids[i].ssid_len);
		memcpy(req->ssids[n_ssids].ssid, sreq->ssids[i].ssid,
		       sreq->ssids[i].ssid_len);
		n_ssids++;
	}
	/* Apple 25G83's directed AIS scans (ordinals 552..590) use selector
	 * bit 3 with one populated SSID.  The public generic bit-2 selector
	 * does not find the requested BSS on this firmware.
	 */
	req->ssid_type = mt7932_only_6ghz && !mt7932_directed_ssid &&
			 !sreq->n_6ghz_params ? 0 :
			 mt7932 && n_ssids ? BIT(3) :
			 n_ssids ? BIT(2) : BIT(0);
	if (!mt7932)
		req->ssid_type_ext = n_ssids ? BIT(0) : 0;
	req->ssids_num = n_ssids;

	/* AppleSunriseWLAN's unassociated active scan computes a 20 ms minimum
	 * and 40 ms normal dwell.  Generic connac2 leaves these fields zero and
	 * relies on a firmware default that the MT7932 25G83 build does not
	 * demonstrate.  Give MT7932 the bounded values recovered from the exact
	 * build instead of submitting an unbounded all-zero scan.
	 */
	if (mt7932)
		duration = 20;
	else
		duration = is_connac2(phy->dev) ? 0 :
			   MT76_CONNAC_SCAN_CHANNEL_TIME;
	/* increase channel time for passive scan */
	if (!mt7932 && !sreq->n_ssids)
		duration *= 2;
	req->timeout_value = cpu_to_le16(mt7932 ? 0 :
					  sreq->n_channels * duration);
	req->channel_min_dwell_time = cpu_to_le16(mt7932 ? 20 : duration);
	req->channel_dwell_time = cpu_to_le16(duration);
	if (mt7932) {
		req7932->passive_channel_dwell_time = cpu_to_le16(110);
		req7932->op_channel_stay_time = cpu_to_le32(45);
		req7932->home_away_time = cpu_to_le16(110);
	}

	if (sreq->n_channels == 0 || sreq->n_channels > 64) {
		req->channel_type = 0;
		req->channels_num = 0;
		req->ext_channels_num = 0;
	} else {
		req->channel_type = 4;
		req->channels_num = min_t(u8, sreq->n_channels, 32);
		req->ext_channels_num = min_t(u8, ext_channels_num, 32);
	}

	for (i = 0; i < req->channels_num + req->ext_channels_num; i++) {
		if (i >= 32)
			chan = &req->ext_channels[i - 32];
		else
			chan = &req->channels[i];

		switch (scan_list[i]->band) {
		case NL80211_BAND_2GHZ:
			chan->band = 1;
			break;
		case NL80211_BAND_6GHZ:
			chan->band = 3;
			break;
		default:
			chan->band = 2;
			break;
		}
		chan->channel_num = scan_list[i]->hw_value;
	}

	if (sreq->ie_len > 0) {
		memcpy(req->ies, sreq->ie, sreq->ie_len);
		req->ies_len = cpu_to_le16(sreq->ie_len);
	}
	if (mt7932 && !n_ssids) {
		memcpy(req->ies, mt7932_scan_ie, sizeof(mt7932_scan_ie));
		req->ies_len = cpu_to_le16(sizeof(mt7932_scan_ie));
	} else if (mt7932) {
		memset(req->ies, 0, sizeof(req->ies));
		req->ies_len = 0;
	}

	if (is_connac2(phy->dev) && !mt7932)
		req->scan_func |= SCAN_FUNC_SPLIT_SCAN;
	else if (mt7932)
		req->scan_func = 0x13;

	if (mt7932)
		memcpy(req7932->bssid, sreq->bssid, ETH_ALEN);
	else
		memcpy(req->bssid, sreq->bssid, ETH_ALEN);
	if (sreq->flags & NL80211_SCAN_FLAG_RANDOM_ADDR) {
		if (mt7932)
			get_random_mask_addr(req7932->random_mac, sreq->mac_addr,
					     sreq->mac_addr_mask);
		else
			get_random_mask_addr(req->random_mac, sreq->mac_addr,
					     sreq->mac_addr_mask);
		req->scan_func |= SCAN_FUNC_RANDOM_MAC;
	}
	if (mt7932)
		req7932->scan_func_ext = cpu_to_le32(0x1800);
	if (mt7932)
		dev_info(mdev->dev,
			 "J700_MT7932_SCAN_REQ_V2: bytes=0x%x seq=%u bss=%u type=%u ssids=%u probes=%u channels=%u+%u dwell=%u min=%u timeout=%u passive=%u opstay=%u away=%u func=0x%02x/0x%08x pure6=%u rnr=%u\n",
			 req_len, req->seq_num, req->bss_idx, req->scan_type,
			 req->ssids_num, req->probe_req_num, req->channels_num,
			 req->ext_channels_num,
			 le16_to_cpu(req->channel_dwell_time),
			 le16_to_cpu(req->channel_min_dwell_time),
			 le16_to_cpu(req->timeout_value),
			 le16_to_cpu(req7932->passive_channel_dwell_time),
			 le32_to_cpu(req7932->op_channel_stay_time),
			 le16_to_cpu(req7932->home_away_time), req->scan_func,
			 le32_to_cpu(req7932->scan_func_ext), mt7932_only_6ghz,
			 !!sreq->n_6ghz_params);
	if (mt7932) {
		err = mt7932_scan_queue_batches(phy, mvif, req7932,
						 scan_list, sreq->n_channels);
		dev_kfree_skb(skb);
		if (err) {
			clear_bit(MT76_HW_SCANNING, &phy->state);
			return err;
		}
		err = mt76_connac_mcu_hw_scan_next(phy);
		if (err < 0) {
			clear_bit(MT76_HW_SCANNING, &phy->state);
			return err;
		}
		return 0;
	}

submit:
	if (mt7932)
		WRITE_ONCE(phy->scan_active_seq, req->seq_num);
	err = mt76_mcu_skb_send_msg(mdev, skb, MCU_CE_CMD(START_HW_SCAN),
				    false);
	if (mt7932) {
		struct mt76_queue *q = mdev->q_mcu[MT_MCUQ_WM];

		dev_info(mdev->dev,
			 "J700_MT7932_SCAN_SUBMIT: ret=%d tx17_head=%u tx17_tail=%u tx17_queued=%u cidx=%u didx=%u\n",
			 err, q->head, q->tail, READ_ONCE(q->queued),
			 readl(&q->regs->cpu_idx), readl(&q->regs->dma_idx));
	}
	if (err < 0) {
		if (mt7932)
			WRITE_ONCE(phy->scan_active_seq, 0);
		clear_bit(MT76_HW_SCANNING, &phy->state);
	}

	return err;

scan_error:
	dev_kfree_skb(skb);
	clear_bit(MT76_HW_SCANNING, &phy->state);
	return err;
}
EXPORT_SYMBOL_GPL(mt76_connac_mcu_hw_scan);

int mt76_connac_mcu_cancel_hw_scan(struct mt76_phy *phy,
				   struct ieee80211_vif *vif)
{
	struct mt76_vif_link *mvif = (struct mt76_vif_link *)vif->drv_priv;
	struct mt76_dev *mdev = phy->dev;
	struct {
		u8 seq_num;
		u8 is_ext_channel;
		u8 rsv[2];
	} __packed req = {
		.seq_num = mvif->scan_seq_num,
	};

	skb_queue_purge(&phy->scan_pending);
	WRITE_ONCE(phy->scan_active_seq, 0);

	if (test_and_clear_bit(MT76_HW_SCANNING, &phy->state)) {
		struct cfg80211_scan_info info = {
			.aborted = true,
		};

		ieee80211_scan_completed(phy->hw, &info);
	}

	return mt76_mcu_send_msg(mdev, MCU_CE_CMD(CANCEL_HW_SCAN),
				 &req, sizeof(req), false);
}
EXPORT_SYMBOL_GPL(mt76_connac_mcu_cancel_hw_scan);

int mt76_connac_mcu_sched_scan_req(struct mt76_phy *phy,
				   struct ieee80211_vif *vif,
				   struct cfg80211_sched_scan_request *sreq)
{
	struct mt76_vif_link *mvif = (struct mt76_vif_link *)vif->drv_priv;
	struct ieee80211_channel **scan_list = sreq->channels;
	struct mt76_connac_mcu_scan_channel *chan;
	struct mt76_connac_sched_scan_req *req;
	struct mt76_dev *mdev = phy->dev;
	struct cfg80211_match_set *match;
	struct cfg80211_ssid *ssid;
	struct sk_buff *skb;
	int i;

	skb = mt76_mcu_msg_alloc(mdev, NULL, sizeof(*req) + sreq->ie_len);
	if (!skb)
		return -ENOMEM;

	mvif->scan_seq_num = (mvif->scan_seq_num + 1) & 0x7f;

	req = (struct mt76_connac_sched_scan_req *)skb_put_zero(skb, sizeof(*req));
	req->version = 1;
	req->seq_num = mvif->scan_seq_num | mvif->band_idx << 7;

	if (sreq->flags & NL80211_SCAN_FLAG_RANDOM_ADDR) {
		u8 *addr = is_mt7663(phy->dev) ? req->mt7663.random_mac
					       : req->mt7921.random_mac;

		req->scan_func = 1;
		get_random_mask_addr(addr, sreq->mac_addr,
				     sreq->mac_addr_mask);
	}
	if (is_connac2(phy->dev)) {
		req->mt7921.bss_idx = mvif->idx;
		req->mt7921.delay = cpu_to_le32(sreq->delay);
	}

	req->ssids_num = sreq->n_ssids;
	for (i = 0; i < req->ssids_num; i++) {
		ssid = &sreq->ssids[i];
		memcpy(req->ssids[i].ssid, ssid->ssid, ssid->ssid_len);
		req->ssids[i].ssid_len = cpu_to_le32(ssid->ssid_len);
	}

	req->match_num = sreq->n_match_sets;
	for (i = 0; i < req->match_num; i++) {
		match = &sreq->match_sets[i];
		memcpy(req->match[i].ssid, match->ssid.ssid,
		       match->ssid.ssid_len);
		req->match[i].rssi_th = cpu_to_le32(match->rssi_thold);
		req->match[i].ssid_len = match->ssid.ssid_len;
	}

	req->channel_type = sreq->n_channels ? 4 : 0;
	req->channels_num = min_t(u8, sreq->n_channels, 64);
	for (i = 0; i < req->channels_num; i++) {
		chan = &req->channels[i];

		switch (scan_list[i]->band) {
		case NL80211_BAND_2GHZ:
			chan->band = 1;
			break;
		case NL80211_BAND_6GHZ:
			chan->band = 3;
			break;
		default:
			chan->band = 2;
			break;
		}
		chan->channel_num = scan_list[i]->hw_value;
	}

	req->intervals_num = sreq->n_scan_plans;
	for (i = 0; i < req->intervals_num; i++)
		req->intervals[i] = cpu_to_le16(sreq->scan_plans[i].interval);

	if (sreq->ie_len > 0) {
		req->ie_len = cpu_to_le16(sreq->ie_len);
		memcpy(skb_put(skb, sreq->ie_len), sreq->ie, sreq->ie_len);
	}

	return mt76_mcu_skb_send_msg(mdev, skb, MCU_CE_CMD(SCHED_SCAN_REQ),
				     false);
}
EXPORT_SYMBOL_GPL(mt76_connac_mcu_sched_scan_req);

int mt76_connac_mcu_sched_scan_enable(struct mt76_phy *phy,
				      struct ieee80211_vif *vif,
				      bool enable)
{
	struct {
		u8 active; /* 0: enabled 1: disabled */
		u8 rsv[3];
	} __packed req = {
		.active = !enable,
	};

	if (enable)
		set_bit(MT76_HW_SCHED_SCANNING, &phy->state);
	else
		clear_bit(MT76_HW_SCHED_SCANNING, &phy->state);

	return mt76_mcu_send_msg(phy->dev, MCU_CE_CMD(SCHED_SCAN_ENABLE),
				 &req, sizeof(req), false);
}
EXPORT_SYMBOL_GPL(mt76_connac_mcu_sched_scan_enable);

int mt76_connac_mcu_chip_config(struct mt76_dev *dev)
{
	struct mt76_connac_config req = {
		.resp_type = 0,
	};

	strscpy(req.data, "assert");

	return mt76_mcu_send_msg(dev, MCU_CE_CMD(CHIP_CONFIG),
				 &req, sizeof(req), false);
}
EXPORT_SYMBOL_GPL(mt76_connac_mcu_chip_config);

int mt76_connac_mcu_set_deep_sleep(struct mt76_dev *dev, bool enable)
{
	struct mt76_connac_config req = {
		.resp_type = 0,
	};

	snprintf(req.data, sizeof(req.data), "KeepFullPwr %d", !enable);

	return mt76_mcu_send_msg(dev, MCU_CE_CMD(CHIP_CONFIG),
				 &req, sizeof(req), false);
}
EXPORT_SYMBOL_GPL(mt76_connac_mcu_set_deep_sleep);

int mt76_connac_sta_state_dp(struct mt76_dev *dev,
			     enum ieee80211_sta_state old_state,
			     enum ieee80211_sta_state new_state)
{
	if ((old_state == IEEE80211_STA_ASSOC &&
	     new_state == IEEE80211_STA_AUTHORIZED) ||
	    (old_state == IEEE80211_STA_NONE &&
	     new_state == IEEE80211_STA_NOTEXIST))
		mt76_connac_mcu_set_deep_sleep(dev, true);

	if ((old_state == IEEE80211_STA_NOTEXIST &&
	     new_state == IEEE80211_STA_NONE) ||
	    (old_state == IEEE80211_STA_AUTHORIZED &&
	     new_state == IEEE80211_STA_ASSOC))
		mt76_connac_mcu_set_deep_sleep(dev, false);

	return 0;
}
EXPORT_SYMBOL_GPL(mt76_connac_sta_state_dp);

void mt76_connac_mcu_coredump_event(struct mt76_dev *dev, struct sk_buff *skb,
				    struct mt76_connac_coredump *coredump)
{
	spin_lock_bh(&dev->lock);
	__skb_queue_tail(&coredump->msg_list, skb);
	spin_unlock_bh(&dev->lock);

	coredump->last_activity = jiffies;

	queue_delayed_work(dev->wq, &coredump->work,
			   MT76_CONNAC_COREDUMP_TIMEOUT);
}
EXPORT_SYMBOL_GPL(mt76_connac_mcu_coredump_event);

static void
mt76_connac_mcu_build_sku(struct mt76_dev *dev, s8 *sku,
			  struct mt76_power_limits *limits,
			  enum nl80211_band band)
{
	int max_power = is_connac2(dev) ? 127 : 63;
	int i, offset = sizeof(limits->cck);

	memset(sku, max_power, MT_SKU_POWER_LIMIT);

	if (band == NL80211_BAND_2GHZ) {
		/* cck */
		memcpy(sku, limits->cck, sizeof(limits->cck));
	}

	/* ofdm */
	memcpy(&sku[offset], limits->ofdm, sizeof(limits->ofdm));
	offset += sizeof(limits->ofdm);

	/* ht */
	for (i = 0; i < 2; i++) {
		memcpy(&sku[offset], limits->mcs[i], 8);
		offset += 8;
	}
	sku[offset++] = limits->mcs[0][0];

	/* vht */
	for (i = 0; i < ARRAY_SIZE(limits->mcs); i++) {
		memcpy(&sku[offset], limits->mcs[i],
		       ARRAY_SIZE(limits->mcs[i]));
		offset += 12;
	}

	if (!is_connac2(dev))
		return;

	/* he */
	for (i = 0; i < ARRAY_SIZE(limits->ru); i++) {
		memcpy(&sku[offset], limits->ru[i], ARRAY_SIZE(limits->ru[i]));
		offset += ARRAY_SIZE(limits->ru[i]);
	}
}

s8 mt76_connac_get_ch_power(struct mt76_phy *phy,
			    struct ieee80211_channel *chan,
			    s8 target_power)
{
	struct mt76_dev *dev = phy->dev;
	struct ieee80211_supported_band *sband;
	int i;

	switch (chan->band) {
	case NL80211_BAND_2GHZ:
		sband = &phy->sband_2g.sband;
		break;
	case NL80211_BAND_5GHZ:
		sband = &phy->sband_5g.sband;
		break;
	case NL80211_BAND_6GHZ:
		sband = &phy->sband_6g.sband;
		break;
	default:
		return target_power;
	}

	for (i = 0; i < sband->n_channels; i++) {
		struct ieee80211_channel *ch = &sband->channels[i];

		if (ch->hw_value == chan->hw_value) {
			if (!(ch->flags & IEEE80211_CHAN_DISABLED)) {
				int power = 2 * ch->max_reg_power;

				if (is_mt7663(dev) && (power > 63 || power < -64))
					power = 63;
				target_power = min_t(s8, power, target_power);
			}
			break;
		}
	}

	return target_power;
}
EXPORT_SYMBOL_GPL(mt76_connac_get_ch_power);

static int
mt76_connac_mcu_rate_txpower_band(struct mt76_phy *phy,
				  enum nl80211_band band)
{
	struct mt76_dev *dev = phy->dev;
	int sku_len, batch_len = is_connac2(dev) ? 8 : 16;
	static const u8 chan_list_2ghz[] = {
		1, 2,  3,  4,  5,  6,  7,
		8, 9, 10, 11, 12, 13, 14
	};
	static const u8 chan_list_5ghz[] = {
		 36,  38,  40,  42,  44,  46,  48,
		 50,  52,  54,  56,  58,  60,  62,
		 64, 100, 102, 104, 106, 108, 110,
		112, 114, 116, 118, 120, 122, 124,
		126, 128, 132, 134, 136, 138, 140,
		142, 144, 149, 151, 153, 155, 157,
		159, 161, 165, 169, 173, 177
	};
	static const u8 chan_list_6ghz[] = {
		  1,   3,   5,   7,   9,  11,  13,
		 15,  17,  19,  21,  23,  25,  27,
		 29,  33,  35,  37,  39,  41,  43,
		 45,  47,  49,  51,  53,  55,  57,
		 59,  61,  65,  67,  69,  71,  73,
		 75,  77,  79,  81,  83,  85,  87,
		 89,  91,  93,  97,  99, 101, 103,
		105, 107, 109, 111, 113, 115, 117,
		119, 121, 123, 125, 129, 131, 133,
		135, 137, 139, 141, 143, 145, 147,
		149, 151, 153, 155, 157, 161, 163,
		165, 167, 169, 171, 173, 175, 177,
		179, 181, 183, 185, 187, 189, 193,
		195, 197, 199, 201, 203, 205, 207,
		209, 211, 213, 215, 217, 219, 221,
		225, 227, 229, 233
	};
	int i, n_chan, batch_size, idx = 0, tx_power, last_ch, err = 0;
	struct mt76_connac_sku_tlv sku_tlbv;
	struct mt76_power_limits *limits;
	const u8 *ch_list;

	limits = devm_kmalloc(dev->dev, sizeof(*limits), GFP_KERNEL);
	if (!limits)
		return -ENOMEM;

	sku_len = is_connac2(dev) ? sizeof(sku_tlbv) : sizeof(sku_tlbv) - 92;
	tx_power = 2 * phy->hw->conf.power_level;
	if (!tx_power)
		tx_power = 127;

	if (band == NL80211_BAND_2GHZ) {
		n_chan = ARRAY_SIZE(chan_list_2ghz);
		ch_list = chan_list_2ghz;
	} else if (band == NL80211_BAND_6GHZ) {
		n_chan = ARRAY_SIZE(chan_list_6ghz);
		ch_list = chan_list_6ghz;
	} else {
		n_chan = ARRAY_SIZE(chan_list_5ghz);
		ch_list = chan_list_5ghz;
	}
	batch_size = DIV_ROUND_UP(n_chan, batch_len);

	if (phy->cap.has_6ghz)
		last_ch = chan_list_6ghz[ARRAY_SIZE(chan_list_6ghz) - 1];
	else if (phy->cap.has_5ghz)
		last_ch = chan_list_5ghz[ARRAY_SIZE(chan_list_5ghz) - 1];
	else
		last_ch = chan_list_2ghz[ARRAY_SIZE(chan_list_2ghz) - 1];

	for (i = 0; i < batch_size; i++) {
		struct mt76_connac_tx_power_limit_tlv tx_power_tlv = {};
		int j, msg_len, num_ch;
		struct sk_buff *skb;

		num_ch = i == batch_size - 1 ? n_chan - i * batch_len : batch_len;
		msg_len = sizeof(tx_power_tlv) + num_ch * sizeof(sku_tlbv);
		skb = mt76_mcu_msg_alloc(dev, NULL, msg_len);
		if (!skb) {
			err = -ENOMEM;
			goto out;
		}

		skb_reserve(skb, sizeof(tx_power_tlv));

		BUILD_BUG_ON(sizeof(dev->alpha2) > sizeof(tx_power_tlv.alpha2));
		memcpy(tx_power_tlv.alpha2, dev->alpha2, sizeof(dev->alpha2));
		tx_power_tlv.n_chan = num_ch;

		switch (band) {
		case NL80211_BAND_2GHZ:
			tx_power_tlv.band = 1;
			break;
		case NL80211_BAND_6GHZ:
			tx_power_tlv.band = 3;
			break;
		default:
			tx_power_tlv.band = 2;
			break;
		}

		for (j = 0; j < num_ch; j++, idx++) {
			struct ieee80211_channel chan = {
				.hw_value = ch_list[idx],
				.band = band,
			};
			s8 max_power;

			max_power = mt76_connac_get_rate_power_limit(phy, &chan,
								     limits);
			if (phy->chandef.chan &&
			    phy->chandef.chan->hw_value == ch_list[idx] &&
			    phy->chandef.chan->band == band)
				mt76_connac_set_txpower_cur(phy, max_power);

			tx_power_tlv.last_msg = ch_list[idx] == last_ch;
			sku_tlbv.channel = ch_list[idx];

			mt76_connac_mcu_build_sku(dev, sku_tlbv.pwr_limit,
						  limits, band);
			skb_put_data(skb, &sku_tlbv, sku_len);
		}
		__skb_push(skb, sizeof(tx_power_tlv));
		memcpy(skb->data, &tx_power_tlv, sizeof(tx_power_tlv));

		err = mt76_mcu_skb_send_msg(dev, skb,
					    MCU_CE_CMD(SET_RATE_TX_POWER),
					    false);
		if (err < 0)
			goto out;

		/* Generic connac parts pace the next batch with a firmware-mediated
		 * PSE register read.  AppleSunriseWLAN does not issue that command on
		 * MT7932, and the 25G83 firmware does not answer CE REG_READ (0xc0).
		 * J700 already restores Apple's exact DMASHDL image before publishing
		 * its rings, so the foreign underflow workaround is neither required
		 * nor safe here.
		 */
		if (is_mt7932(dev)) {
			dev_info_once(dev->dev,
				      "J700_MT7932_PSE_REG_READ_SKIP: source=apple-runtime-trace command=0x000500c0\n");
		} else {
			/* read a CR to avoid PSE buffer underflow */
			mt76_connac_mcu_reg_rr(dev, MT_PSE_BASE);
		}
	}

out:
	devm_kfree(dev->dev, limits);
	return err;
}

int mt76_connac_mcu_set_rate_txpower(struct mt76_phy *phy)
{
	int err;

	/* J700's complete Apple-format AU/SP power transcript uses 122-byte
	 * records.  Generic connac2
	 * emits 162-byte records instead of 25G83's 122-byte ABI, and hardware
	 * Cycles 473/480 proved those foreign packets stop TX17.  Never replay a
	 * second power transaction from the generic per-interface/SAR callback.
	 */
	if (is_mt7932(phy->dev)) {
		dev_info_once(phy->dev->dev,
			      "J700_MT7932_RATE_POWER_GENERIC_SKIP: cid=0x5d reason=generic-abi-incompatible apple-policy=runtime-pending records=122 generic_records=162\n");
		return 0;
	}

	if (phy->cap.has_2ghz) {
		err = mt76_connac_mcu_rate_txpower_band(phy,
							NL80211_BAND_2GHZ);
		if (err < 0)
			return err;
	}
	if (phy->cap.has_5ghz) {
		err = mt76_connac_mcu_rate_txpower_band(phy,
							NL80211_BAND_5GHZ);
		if (err < 0)
			return err;
	}
	if (phy->cap.has_6ghz) {
		err = mt76_connac_mcu_rate_txpower_band(phy,
							NL80211_BAND_6GHZ);
		if (err < 0)
			return err;
	}

	return 0;
}
EXPORT_SYMBOL_GPL(mt76_connac_mcu_set_rate_txpower);

int mt76_connac_mcu_update_arp_filter(struct mt76_dev *dev,
				      struct mt76_vif_link *vif,
				      struct ieee80211_bss_conf *info)
{
	struct ieee80211_vif *mvif = container_of(info, struct ieee80211_vif,
						  bss_conf);
	struct sk_buff *skb;
	int i, len = min_t(int, mvif->cfg.arp_addr_cnt,
			   IEEE80211_BSS_ARP_ADDR_LIST_LEN);
	struct {
		struct {
			u8 bss_idx;
			u8 pad[3];
		} __packed hdr;
		struct mt76_connac_arpns_tlv arp;
	} req_hdr = {
		.hdr = {
			.bss_idx = vif->idx,
		},
		.arp = {
			.tag = cpu_to_le16(UNI_OFFLOAD_OFFLOAD_ARP),
			.len = cpu_to_le16(sizeof(struct mt76_connac_arpns_tlv)),
			.ips_num = len,
			.mode = 2,  /* update */
			.option = 1,
		},
	};

	skb = mt76_mcu_msg_alloc(dev, NULL,
				 sizeof(req_hdr) + len * sizeof(__be32));
	if (!skb)
		return -ENOMEM;

	skb_put_data(skb, &req_hdr, sizeof(req_hdr));
	for (i = 0; i < len; i++)
		skb_put_data(skb, &mvif->cfg.arp_addr_list[i], sizeof(__be32));

	return mt76_mcu_skb_send_msg(dev, skb, MCU_UNI_CMD(OFFLOAD), true);
}
EXPORT_SYMBOL_GPL(mt76_connac_mcu_update_arp_filter);

int mt76_connac_mcu_set_p2p_oppps(struct ieee80211_hw *hw,
				  struct ieee80211_vif *vif)
{
	struct mt76_vif_link *mvif = (struct mt76_vif_link *)vif->drv_priv;
	int ct_window = vif->bss_conf.p2p_noa_attr.oppps_ctwindow;
	struct mt76_phy *phy = hw->priv;
	struct {
		__le32 ct_win;
		u8 bss_idx;
		u8 rsv[3];
	} __packed req = {
		.ct_win = cpu_to_le32(ct_window),
		.bss_idx = mvif->idx,
	};

	return mt76_mcu_send_msg(phy->dev, MCU_CE_CMD(SET_P2P_OPPPS),
				 &req, sizeof(req), false);
}
EXPORT_SYMBOL_GPL(mt76_connac_mcu_set_p2p_oppps);

#ifdef CONFIG_PM

const struct wiphy_wowlan_support mt76_connac_wowlan_support = {
	.flags = WIPHY_WOWLAN_MAGIC_PKT | WIPHY_WOWLAN_DISCONNECT |
		 WIPHY_WOWLAN_SUPPORTS_GTK_REKEY | WIPHY_WOWLAN_NET_DETECT,
	.n_patterns = 1,
	.pattern_min_len = 1,
	.pattern_max_len = MT76_CONNAC_WOW_PATTEN_MAX_LEN,
	.max_nd_match_sets = 10,
};
EXPORT_SYMBOL_GPL(mt76_connac_wowlan_support);

static void
mt76_connac_mcu_key_iter(struct ieee80211_hw *hw,
			 struct ieee80211_vif *vif,
			 struct ieee80211_sta *sta,
			 struct ieee80211_key_conf *key,
			 void *data)
{
	struct mt76_connac_gtk_rekey_tlv *gtk_tlv = data;
	u32 cipher;

	if (key->cipher != WLAN_CIPHER_SUITE_AES_CMAC &&
	    key->cipher != WLAN_CIPHER_SUITE_CCMP &&
	    key->cipher != WLAN_CIPHER_SUITE_TKIP)
		return;

	if (key->cipher == WLAN_CIPHER_SUITE_TKIP)
		cipher = BIT(3);
	else
		cipher = BIT(4);

	/* we are assuming here to have a single pairwise key */
	if (key->flags & IEEE80211_KEY_FLAG_PAIRWISE) {
		if (key->cipher == WLAN_CIPHER_SUITE_TKIP)
			gtk_tlv->proto = cpu_to_le32(NL80211_WPA_VERSION_1);
		else
			gtk_tlv->proto = cpu_to_le32(NL80211_WPA_VERSION_2);

		gtk_tlv->pairwise_cipher = cpu_to_le32(cipher);
		gtk_tlv->keyid = key->keyidx;
	} else {
		gtk_tlv->group_cipher = cpu_to_le32(cipher);
	}
}

int mt76_connac_mcu_update_gtk_rekey(struct ieee80211_hw *hw,
				     struct ieee80211_vif *vif,
				     struct cfg80211_gtk_rekey_data *key)
{
	struct mt76_vif_link *mvif = (struct mt76_vif_link *)vif->drv_priv;
	struct mt76_connac_gtk_rekey_tlv *gtk_tlv;
	struct mt76_phy *phy = hw->priv;
	struct sk_buff *skb;
	struct {
		u8 bss_idx;
		u8 pad[3];
	} __packed hdr = {
		.bss_idx = mvif->idx,
	};

	skb = mt76_mcu_msg_alloc(phy->dev, NULL,
				 sizeof(hdr) + sizeof(*gtk_tlv));
	if (!skb)
		return -ENOMEM;

	skb_put_data(skb, &hdr, sizeof(hdr));
	gtk_tlv = (struct mt76_connac_gtk_rekey_tlv *)skb_put_zero(skb,
							 sizeof(*gtk_tlv));
	gtk_tlv->tag = cpu_to_le16(UNI_OFFLOAD_OFFLOAD_GTK_REKEY);
	gtk_tlv->len = cpu_to_le16(sizeof(*gtk_tlv));
	gtk_tlv->rekey_mode = 2;
	gtk_tlv->option = 1;

	rcu_read_lock();
	ieee80211_iter_keys_rcu(hw, vif, mt76_connac_mcu_key_iter, gtk_tlv);
	rcu_read_unlock();

	memcpy(gtk_tlv->kek, key->kek, NL80211_KEK_LEN);
	memcpy(gtk_tlv->kck, key->kck, NL80211_KCK_LEN);
	memcpy(gtk_tlv->replay_ctr, key->replay_ctr, NL80211_REPLAY_CTR_LEN);

	return mt76_mcu_skb_send_msg(phy->dev, skb,
				     MCU_UNI_CMD(OFFLOAD), true);
}
EXPORT_SYMBOL_GPL(mt76_connac_mcu_update_gtk_rekey);

static int
mt76_connac_mcu_set_arp_filter(struct mt76_dev *dev, struct ieee80211_vif *vif,
			       bool suspend)
{
	struct mt76_vif_link *mvif = (struct mt76_vif_link *)vif->drv_priv;
	struct {
		struct {
			u8 bss_idx;
			u8 pad[3];
		} __packed hdr;
		struct mt76_connac_arpns_tlv arpns;
	} req = {
		.hdr = {
			.bss_idx = mvif->idx,
		},
		.arpns = {
			.tag = cpu_to_le16(UNI_OFFLOAD_OFFLOAD_ARP),
			.len = cpu_to_le16(sizeof(struct mt76_connac_arpns_tlv)),
			.mode = suspend,
		},
	};

	return mt76_mcu_send_msg(dev, MCU_UNI_CMD(OFFLOAD), &req,
				 sizeof(req), true);
}

int
mt76_connac_mcu_set_gtk_rekey(struct mt76_dev *dev, struct ieee80211_vif *vif,
			      bool suspend)
{
	struct mt76_vif_link *mvif = (struct mt76_vif_link *)vif->drv_priv;
	struct {
		struct {
			u8 bss_idx;
			u8 pad[3];
		} __packed hdr;
		struct mt76_connac_gtk_rekey_tlv gtk_tlv;
	} __packed req = {
		.hdr = {
			.bss_idx = mvif->idx,
		},
		.gtk_tlv = {
			.tag = cpu_to_le16(UNI_OFFLOAD_OFFLOAD_GTK_REKEY),
			.len = cpu_to_le16(sizeof(struct mt76_connac_gtk_rekey_tlv)),
			.rekey_mode = !suspend,
		},
	};

	return mt76_mcu_send_msg(dev, MCU_UNI_CMD(OFFLOAD), &req,
				 sizeof(req), true);
}
EXPORT_SYMBOL_GPL(mt76_connac_mcu_set_gtk_rekey);

int
mt76_connac_mcu_set_suspend_mode(struct mt76_dev *dev,
				 struct ieee80211_vif *vif,
				 bool enable, u8 mdtim,
				 bool wow_suspend)
{
	struct mt76_vif_link *mvif = (struct mt76_vif_link *)vif->drv_priv;
	struct {
		struct {
			u8 bss_idx;
			u8 pad[3];
		} __packed hdr;
		struct mt76_connac_suspend_tlv suspend_tlv;
	} req = {
		.hdr = {
			.bss_idx = mvif->idx,
		},
		.suspend_tlv = {
			.tag = cpu_to_le16(UNI_SUSPEND_MODE_SETTING),
			.len = cpu_to_le16(sizeof(struct mt76_connac_suspend_tlv)),
			.enable = enable,
			.mdtim = mdtim,
			.wow_suspend = wow_suspend,
		},
	};

	return mt76_mcu_send_msg(dev, MCU_UNI_CMD(SUSPEND), &req,
				 sizeof(req), true);
}
EXPORT_SYMBOL_GPL(mt76_connac_mcu_set_suspend_mode);

static int
mt76_connac_mcu_set_wow_pattern(struct mt76_dev *dev,
				struct ieee80211_vif *vif,
				u8 index, bool enable,
				struct cfg80211_pkt_pattern *pattern)
{
	struct mt76_vif_link *mvif = (struct mt76_vif_link *)vif->drv_priv;
	struct mt76_connac_wow_pattern_tlv *ptlv;
	struct sk_buff *skb;
	struct req_hdr {
		u8 bss_idx;
		u8 pad[3];
	} __packed hdr = {
		.bss_idx = mvif->idx,
	};

	skb = mt76_mcu_msg_alloc(dev, NULL, sizeof(hdr) + sizeof(*ptlv));
	if (!skb)
		return -ENOMEM;

	skb_put_data(skb, &hdr, sizeof(hdr));
	ptlv = (struct mt76_connac_wow_pattern_tlv *)skb_put_zero(skb, sizeof(*ptlv));
	ptlv->tag = cpu_to_le16(UNI_SUSPEND_WOW_PATTERN);
	ptlv->len = cpu_to_le16(sizeof(*ptlv));
	ptlv->data_len = pattern->pattern_len;
	ptlv->enable = enable;
	ptlv->index = index;

	memcpy(ptlv->pattern, pattern->pattern, pattern->pattern_len);
	memcpy(ptlv->mask, pattern->mask, DIV_ROUND_UP(pattern->pattern_len, 8));

	return mt76_mcu_skb_send_msg(dev, skb, MCU_UNI_CMD(SUSPEND), true);
}

int
mt76_connac_mcu_set_wow_ctrl(struct mt76_phy *phy, struct ieee80211_vif *vif,
			     bool suspend, struct cfg80211_wowlan *wowlan)
{
	struct mt76_vif_link *mvif = (struct mt76_vif_link *)vif->drv_priv;
	struct mt76_dev *dev = phy->dev;
	struct {
		struct {
			u8 bss_idx;
			u8 pad[3];
		} __packed hdr;
		struct mt76_connac_wow_ctrl_tlv wow_ctrl_tlv;
		struct mt76_connac_wow_gpio_param_tlv gpio_tlv;
	} req = {
		.hdr = {
			.bss_idx = mvif->idx,
		},
		.wow_ctrl_tlv = {
			.tag = cpu_to_le16(UNI_SUSPEND_WOW_CTRL),
			.len = cpu_to_le16(sizeof(struct mt76_connac_wow_ctrl_tlv)),
			.cmd = suspend ? 1 : 2,
		},
		.gpio_tlv = {
			.tag = cpu_to_le16(UNI_SUSPEND_WOW_GPIO_PARAM),
			.len = cpu_to_le16(sizeof(struct mt76_connac_wow_gpio_param_tlv)),
			.gpio_pin = 0xff, /* follow fw about GPIO pin */
		},
	};

	if (wowlan->magic_pkt)
		req.wow_ctrl_tlv.trigger |= UNI_WOW_DETECT_TYPE_MAGIC;
	if (wowlan->disconnect)
		req.wow_ctrl_tlv.trigger |= (UNI_WOW_DETECT_TYPE_DISCONNECT |
					     UNI_WOW_DETECT_TYPE_BCN_LOST);
	if (wowlan->nd_config) {
		mt76_connac_mcu_sched_scan_req(phy, vif, wowlan->nd_config);
		req.wow_ctrl_tlv.trigger |= UNI_WOW_DETECT_TYPE_SCH_SCAN_HIT;
		mt76_connac_mcu_sched_scan_enable(phy, vif, suspend);
	}
	if (wowlan->n_patterns)
		req.wow_ctrl_tlv.trigger |= UNI_WOW_DETECT_TYPE_BITMAP;

	if (mt76_is_mmio(dev))
		req.wow_ctrl_tlv.wakeup_hif = WOW_PCIE;
	else if (mt76_is_usb(dev))
		req.wow_ctrl_tlv.wakeup_hif = WOW_USB;
	else if (mt76_is_sdio(dev))
		req.wow_ctrl_tlv.wakeup_hif = WOW_GPIO;

	return mt76_mcu_send_msg(dev, MCU_UNI_CMD(SUSPEND), &req,
				 sizeof(req), true);
}
EXPORT_SYMBOL_GPL(mt76_connac_mcu_set_wow_ctrl);

int mt76_connac_mcu_set_hif_suspend(struct mt76_dev *dev, bool suspend, bool wait_resp)
{
	struct {
		struct {
			u8 hif_type; /* 0x0: HIF_SDIO
				      * 0x1: HIF_USB
				      * 0x2: HIF_PCIE
				      */
			u8 pad[3];
		} __packed hdr;
		struct hif_suspend_tlv {
			__le16 tag;
			__le16 len;
			u8 suspend;
			u8 pad[7];
		} __packed hif_suspend;
	} req = {
		.hif_suspend = {
			.tag = cpu_to_le16(0), /* 0: UNI_HIF_CTRL_BASIC */
			.len = cpu_to_le16(sizeof(struct hif_suspend_tlv)),
			.suspend = suspend,
		},
	};

	if (mt76_is_mmio(dev))
		req.hdr.hif_type = 2;
	else if (mt76_is_usb(dev))
		req.hdr.hif_type = 1;
	else if (mt76_is_sdio(dev))
		req.hdr.hif_type = 0;

	return mt76_mcu_send_msg(dev, MCU_UNI_CMD(HIF_CTRL), &req,
				 sizeof(req), wait_resp);
}
EXPORT_SYMBOL_GPL(mt76_connac_mcu_set_hif_suspend);

void mt76_connac_mcu_set_suspend_iter(void *priv, u8 *mac,
				      struct ieee80211_vif *vif)
{
	struct mt76_phy *phy = priv;
	bool suspend = !test_bit(MT76_STATE_RUNNING, &phy->state);
	struct ieee80211_hw *hw = phy->hw;
	struct cfg80211_wowlan *wowlan = hw->wiphy->wowlan_config;
	int i;

	mt76_connac_mcu_set_gtk_rekey(phy->dev, vif, suspend);
	mt76_connac_mcu_set_arp_filter(phy->dev, vif, suspend);

	mt76_connac_mcu_set_suspend_mode(phy->dev, vif, suspend, 1, true);

	for (i = 0; i < wowlan->n_patterns; i++)
		mt76_connac_mcu_set_wow_pattern(phy->dev, vif, i, suspend,
						&wowlan->patterns[i]);
	mt76_connac_mcu_set_wow_ctrl(phy, vif, suspend, wowlan);
}
EXPORT_SYMBOL_GPL(mt76_connac_mcu_set_suspend_iter);
#endif /* CONFIG_PM */

u32 mt76_connac_mcu_reg_rr(struct mt76_dev *dev, u32 offset)
{
	struct {
		__le32 addr;
		__le32 val;
	} __packed req = {
		.addr = cpu_to_le32(offset),
	};

	return mt76_mcu_send_msg(dev, MCU_CE_QUERY(REG_READ), &req,
				 sizeof(req), true);
}
EXPORT_SYMBOL_GPL(mt76_connac_mcu_reg_rr);

void mt76_connac_mcu_reg_wr(struct mt76_dev *dev, u32 offset, u32 val)
{
	struct {
		__le32 addr;
		__le32 val;
	} __packed req = {
		.addr = cpu_to_le32(offset),
		.val = cpu_to_le32(val),
	};

	mt76_mcu_send_msg(dev, MCU_CE_CMD(REG_WRITE), &req,
			  sizeof(req), false);
}
EXPORT_SYMBOL_GPL(mt76_connac_mcu_reg_wr);

static int
mt76_connac_mcu_sta_key_tlv(struct mt76_connac_sta_key_conf *sta_key_conf,
			    struct sk_buff *skb,
			    struct ieee80211_key_conf *key,
			    enum set_key_cmd cmd)
{
	struct sta_rec_sec *sec;
	u32 len = sizeof(*sec);
	struct tlv *tlv;

	tlv = mt76_connac_mcu_add_tlv(skb, STA_REC_KEY_V2, sizeof(*sec));
	sec = (struct sta_rec_sec *)tlv;
	sec->add = cmd;

	if (cmd == SET_KEY) {
		struct sec_key *sec_key;
		u8 cipher;

		cipher = mt76_connac_mcu_get_cipher(key->cipher);
		if (cipher == MCU_CIPHER_NONE)
			return -EOPNOTSUPP;

		sec_key = &sec->key[0];
		sec_key->cipher_len = sizeof(*sec_key);

		if (cipher == MCU_CIPHER_BIP_CMAC_128) {
			sec_key->cipher_id = MCU_CIPHER_AES_CCMP;
			sec_key->key_id = sta_key_conf->keyidx;
			sec_key->key_len = 16;
			memcpy(sec_key->key, sta_key_conf->key, 16);

			sec_key = &sec->key[1];
			sec_key->cipher_id = MCU_CIPHER_BIP_CMAC_128;
			sec_key->cipher_len = sizeof(*sec_key);
			sec_key->key_len = 16;
			memcpy(sec_key->key, key->key, 16);
			sec->n_cipher = 2;
		} else {
			sec_key->cipher_id = cipher;
			sec_key->key_id = key->keyidx;
			sec_key->key_len = key->keylen;
			memcpy(sec_key->key, key->key, key->keylen);

			if (cipher == MCU_CIPHER_TKIP) {
				/* Rx/Tx MIC keys are swapped */
				memcpy(sec_key->key + 16, key->key + 24, 8);
				memcpy(sec_key->key + 24, key->key + 16, 8);
			}

			/* store key_conf for BIP batch update */
			if (cipher == MCU_CIPHER_AES_CCMP) {
				memcpy(sta_key_conf->key, key->key, key->keylen);
				sta_key_conf->keyidx = key->keyidx;
			}

			len -= sizeof(*sec_key);
			sec->n_cipher = 1;
		}
	} else {
		len -= sizeof(sec->key);
		sec->n_cipher = 0;
	}
	sec->len = cpu_to_le16(len);

	return 0;
}

int mt76_connac_mcu_add_key(struct mt76_dev *dev, struct ieee80211_vif *vif,
			    struct mt76_connac_sta_key_conf *sta_key_conf,
			    struct ieee80211_key_conf *key, int mcu_cmd,
			    struct mt76_wcid *wcid, enum set_key_cmd cmd)
{
	struct mt76_vif_link *mvif = (struct mt76_vif_link *)vif->drv_priv;
	struct sk_buff *skb;
	int ret;

	skb = mt76_connac_mcu_alloc_sta_req(dev, mvif, wcid);
	if (IS_ERR(skb))
		return PTR_ERR(skb);

	ret = mt76_connac_mcu_sta_key_tlv(sta_key_conf, skb, key, cmd);
	if (ret) {
		dev_kfree_skb(skb);
		return ret;
	}

	ret = mt76_connac_mcu_sta_wed_update(dev, skb);
	if (ret) {
		dev_kfree_skb(skb);
		return ret;
	}

	return mt76_mcu_skb_send_msg(dev, skb, mcu_cmd, true);
}
EXPORT_SYMBOL_GPL(mt76_connac_mcu_add_key);

/* SIFS 20us + 512 byte beacon transmitted by 1Mbps (3906us) */
#define BCN_TX_ESTIMATE_TIME (4096 + 20)
void mt76_connac_mcu_bss_ext_tlv(struct sk_buff *skb, struct mt76_vif_link *mvif)
{
	struct bss_info_ext_bss *ext;
	int ext_bss_idx, tsf_offset;
	struct tlv *tlv;

	ext_bss_idx = mvif->omac_idx - EXT_BSSID_START;
	if (ext_bss_idx < 0)
		return;

	tlv = mt76_connac_mcu_add_tlv(skb, BSS_INFO_EXT_BSS, sizeof(*ext));

	ext = (struct bss_info_ext_bss *)tlv;
	tsf_offset = ext_bss_idx * BCN_TX_ESTIMATE_TIME;
	ext->mbss_tsf_offset = cpu_to_le32(tsf_offset);
}
EXPORT_SYMBOL_GPL(mt76_connac_mcu_bss_ext_tlv);

int mt76_connac_mcu_bss_basic_tlv(struct sk_buff *skb,
				  struct ieee80211_vif *vif,
				  struct ieee80211_sta *sta,
				  struct mt76_phy *phy, u16 wlan_idx,
				  bool enable)
{
	struct mt76_vif_link *mvif = (struct mt76_vif_link *)vif->drv_priv;
	u32 type = vif->p2p ? NETWORK_P2P : NETWORK_INFRA;
	struct bss_info_basic *bss;
	struct tlv *tlv;

	tlv = mt76_connac_mcu_add_tlv(skb, BSS_INFO_BASIC, sizeof(*bss));
	bss = (struct bss_info_basic *)tlv;

	switch (vif->type) {
	case NL80211_IFTYPE_MESH_POINT:
	case NL80211_IFTYPE_MONITOR:
		break;
	case NL80211_IFTYPE_AP:
		if (ieee80211_hw_check(phy->hw, SUPPORTS_MULTI_BSSID)) {
			u8 bssid_id = vif->bss_conf.bssid_indicator;
			struct wiphy *wiphy = phy->hw->wiphy;

			if (bssid_id > ilog2(wiphy->mbssid_max_interfaces))
				return -EINVAL;

			bss->non_tx_bssid = vif->bss_conf.bssid_index;
			bss->max_bssid = bssid_id;
		}
		break;
	case NL80211_IFTYPE_STATION:
		if (enable) {
			rcu_read_lock();
			if (!sta)
				sta = ieee80211_find_sta(vif,
							 vif->bss_conf.bssid);
			/* TODO: enable BSS_INFO_UAPSD & BSS_INFO_PM */
			if (sta) {
				struct mt76_wcid *wcid;

				wcid = (struct mt76_wcid *)sta->drv_priv;
				wlan_idx = wcid->idx;
			}
			rcu_read_unlock();
		}
		break;
	case NL80211_IFTYPE_ADHOC:
		type = NETWORK_IBSS;
		break;
	default:
		WARN_ON(1);
		break;
	}

	bss->network_type = cpu_to_le32(type);
	bss->bmc_wcid_lo = to_wcid_lo(wlan_idx);
	bss->bmc_wcid_hi = to_wcid_hi(wlan_idx);
	bss->wmm_idx = mvif->wmm_idx;
	bss->active = enable;
	bss->cipher = mvif->cipher;

	if (vif->type != NL80211_IFTYPE_MONITOR) {
		struct cfg80211_chan_def *chandef = &phy->chandef;

		memcpy(bss->bssid, vif->bss_conf.bssid, ETH_ALEN);
		bss->bcn_interval = cpu_to_le16(vif->bss_conf.beacon_int);
		bss->dtim_period = vif->bss_conf.dtim_period;
		bss->phy_mode = mt76_connac_get_phy_mode(phy, vif,
							 chandef->chan->band, NULL);
	} else {
		memcpy(bss->bssid, phy->macaddr, ETH_ALEN);
	}

	return 0;
}
EXPORT_SYMBOL_GPL(mt76_connac_mcu_bss_basic_tlv);

#define ENTER_PM_STATE		1
#define EXIT_PM_STATE		2
int mt76_connac_mcu_set_pm(struct mt76_dev *dev, int band, int enter)
{
	struct {
		u8 pm_number;
		u8 pm_state;
		u8 bssid[ETH_ALEN];
		u8 dtim_period;
		u8 wlan_idx_lo;
		__le16 bcn_interval;
		__le32 aid;
		__le32 rx_filter;
		u8 band_idx;
		u8 wlan_idx_hi;
		u8 rsv[2];
		__le32 feature;
		u8 omac_idx;
		u8 wmm_idx;
		u8 bcn_loss_cnt;
		u8 bcn_sp_duration;
	} __packed req = {
		.pm_number = 5,
		.pm_state = enter ? ENTER_PM_STATE : EXIT_PM_STATE,
		.band_idx = band,
	};

	return mt76_mcu_send_msg(dev, MCU_EXT_CMD(PM_STATE_CTRL), &req,
				 sizeof(req), true);
}
EXPORT_SYMBOL_GPL(mt76_connac_mcu_set_pm);

int mt76_connac_mcu_restart(struct mt76_dev *dev)
{
	struct {
		u8 power_mode;
		u8 rsv[3];
	} req = {
		.power_mode = 1,
	};

	return mt76_mcu_send_msg(dev, MCU_CMD(NIC_POWER_CTRL), &req,
				 sizeof(req), false);
}
EXPORT_SYMBOL_GPL(mt76_connac_mcu_restart);

int mt76_connac_mcu_del_wtbl_all(struct mt76_dev *dev)
{
	struct wtbl_req_hdr req = {
		.operation = WTBL_RESET_ALL,
	};

	return mt76_mcu_send_msg(dev, MCU_EXT_CMD(WTBL_UPDATE),
				 &req, sizeof(req), true);
}
EXPORT_SYMBOL_GPL(mt76_connac_mcu_del_wtbl_all);

int mt76_connac_mcu_rdd_cmd(struct mt76_dev *dev, int cmd, u8 index,
			    u8 rx_sel, u8 val)
{
	struct {
		u8 ctrl;
		u8 rdd_idx;
		u8 rdd_rx_sel;
		u8 val;
		u8 rsv[4];
	} __packed req = {
		.ctrl = cmd,
		.rdd_idx = index,
		.rdd_rx_sel = rx_sel,
		.val = val,
	};

	return mt76_mcu_send_msg(dev, MCU_EXT_CMD(SET_RDD_CTRL), &req,
				 sizeof(req), true);
}
EXPORT_SYMBOL_GPL(mt76_connac_mcu_rdd_cmd);

static int mt7932_mcu_drain_fwdl(struct mt76_dev *dev)
{
	struct mt76_queue *q = dev->q_mcu[MT_MCUQ_FWDL];
	int i;

	if (!q || !dev->queue_ops->tx_cleanup)
		return -EOPNOTSUPP;

	for (i = 0; i < 1000; i++) {
		dev->queue_ops->tx_cleanup(dev, q, false);
		if (!READ_ONCE(q->queued)) {
			dev_info(dev->dev,
				 "J700_MT7932_FWDL_DRAIN_PASS: hw=%u head=%u tail=%u polls=%d\n",
				 q->hw_idx, q->head, q->tail, i + 1);
			return 0;
		}
		usleep_range(100, 200);
	}

	dev_err(dev->dev,
		"J700_MT7932_FWDL_DRAIN_FAIL: hw=%u head=%u tail=%u queued=%d\n",
		q->hw_idx, q->head, q->tail, q->queued);

	return -ETIMEDOUT;
}

static int mt7932_mcu_require_host_ready(struct mt76_dev *dev)
{
	static const u16 cfg_offsets[] = {
		0x088, 0x090, 0x0a8, 0x0fc, 0x10c,
		0x118, 0x11c, 0x1d0, 0x200, 0x204,
	};
	struct pci_dev *pdev;
	ktime_t settle_start;
	u32 fw_busdev, fw_command, cfg48c, pc;
	u32 cfg[ARRAY_SIZE(cfg_offsets)];
	s64 settle_us;
	u16 command = 0xffff;
	unsigned int i;
	int ret;

	if (!dev_is_pci(dev->dev))
		return -ENODEV;

	pdev = to_pci_dev(dev->dev);
	/* Run 191's unpaused trace leaves 31.628 ms between the fifth
	 * download-config response and CID-2.  Linux already spends about
	 * 12.17 ms draining TX16 and preparing the secure/IRQ state, so add
	 * the measured missing 20 ms here.  Cycle 367 proved that issuing CID-2
	 * while WM PC was still in 0x0090xxxx patch code only reached state 2.
	 */
	if (READ_ONCE(dev->mcu.mt7932_skip_prestart_settle)) {
		settle_us = 0;
	} else {
		settle_start = ktime_get();
		usleep_range(MT7932_FW_SETTLE_MIN_US,
			     MT7932_FW_SETTLE_MAX_US);
		settle_us = ktime_us_delta(ktime_get(), settle_start);
	}
	ret = pci_read_config_dword(pdev, MT7932_PCI_MCU_STATE, &cfg48c);
	if (ret || cfg48c == U32_MAX)
		return ret ?: -ENODEV;
	pc = __mt76_rr(dev, MT7932_WM_MCU_PC_DBG);
	if (pc == U32_MAX)
		return -ENODEV;
	for (i = 0; i < ARRAY_SIZE(cfg_offsets); i++) {
		ret = pci_read_config_dword(pdev, cfg_offsets[i], &cfg[i]);
		if (ret || cfg[i] == U32_MAX)
			return ret ?: -ENODEV;
	}

	ret = pci_read_config_word(pdev, PCI_COMMAND, &command);
	if (ret || command == 0xffff ||
	    (command & (PCI_COMMAND_MEMORY | PCI_COMMAND_MASTER)) !=
	    (PCI_COMMAND_MEMORY | PCI_COMMAND_MASTER)) {
		dev_err(dev->dev,
			"J700_MT7932_HOST_READY_FAIL: command=0x%04x ret=%d memory=%u bme=%u\n",
			command, ret, !!(command & PCI_COMMAND_MEMORY),
			!!(command & PCI_COMMAND_MASTER));
		return ret ?: -ENODEV;
	}

	/* The NDS32 image's hal_hif host-ready path at 0x0091ab46 reads these
	 * device-internal PCIe shadows, derives bus/device from 0x74030144 and
	 * tests BME at bit 2 of 0x74031004.  Host ECAM command 0x0406 alone does
	 * not prove that the running firmware observes the same state.
	 */
	fw_busdev = __mt76_rr(dev, MT7932_FW_PCIE_BUSDEV);
	fw_command = __mt76_rr(dev, MT7932_FW_PCIE_COMMAND);
	if (fw_busdev == U32_MAX || fw_command == U32_MAX)
		return -ENODEV;
	dev_info(dev->dev,
		 "J700_MT7932_FW_HOST_READY_VIEW: busdev=0x%08x bus=%u device=%u command=0x%08x bme=%u host_command=0x%04x cfg48c=0x%08x mcu_state=%u wm_pc=0x%08x settle_us=%lld devctl=0x%08x lnkctl=0x%08x devctl2=0x%08x pmcsr=0x%08x ltr=0x%08x l1ss1=0x%08x l1ss2=0x%08x ptm=0x%08x aer=0x%08x aer_status=0x%08x source=run226-config-snapshot nds32=0x0091ab46\n",
		 fw_busdev, (fw_busdev >> 8) & 0xff,
		 (fw_busdev & 0xf8) >> 3, fw_command,
		 !!(fw_command & BIT(2)), command, cfg48c,
		 (u32)FIELD_GET(MT7932_PCI_MCU_STATE_MASK, cfg48c), pc,
		 settle_us, cfg[0], cfg[1], cfg[2], cfg[3], cfg[4],
		 cfg[5], cfg[6], cfg[7], cfg[8], cfg[9]);

	return 0;
}

/* Cycle 250 discriminator: MT7932 endpoint liveness across the FW_START wait.
 *
 * Between submitting the FW_START descriptor and the MCU response the host
 * issues no PCIe transaction to the endpoint at all - mt76_mcu_wait_response()
 * sleeps on a completion.  Every recorded J700 run loses the endpoint inside
 * that silent window (26-56 ms after the last completed MCU response on the
 * Cycle 207-214 baseline, 100-142 ms after FW_START on the Cycle 230-249
 * baseline), and the surviving root port always reports a clean link-down with
 * no uncorrectable AER status.
 *
 * macOS never produces that silence on this port.  The j700ap ADT keeps the
 * link in a managed power state rather than an unmanaged one:
 * /arm-io/apcie/pci-bridge0 and its wlan child both carry pci-aspm-default = 2
 * and pci-l1pm-control, and AppleEmbeddedPCIEPort owns a bus-probe timer, so
 * configuration traffic to the function keeps flowing after the port is
 * enabled.
 *
 * This poller reads only the endpoint's vendor ID.  Configuration space is
 * routed independently of the BAR window, so the read touches no MCU state, no
 * ring and no doorbell, and it cannot perturb the command stream that has
 * already been submitted.  It answers one question - does the endpoint survive
 * when the link is not left completely idle - and records the exact moment the
 * endpoint stops answering, which the root port's link-down interrupt only
 * reports tens of milliseconds later.
 *
 * 0xffffffff is always treated as dead, never as a valid identity.
 */
struct mt7932_fw_start_watch {
	struct delayed_work work;
	struct pci_dev *pdev;
	struct mt76_dev *dev;
	ktime_t start;
	ktime_t gone_at;
	unsigned int polls;
	u32 last_id;
	u32 ready_first;
	u32 ready_last;
	s64 ready_first_us;
	s64 ready_last_us;
	unsigned int ready_samples;
	bool gone;
};

static void mt7932_fw_start_watch_fn(struct work_struct *work)
{
	struct mt7932_fw_start_watch *w = container_of(to_delayed_work(work),
						       struct mt7932_fw_start_watch,
						       work);
	ktime_t now;
	s64 elapsed_us;
	u32 id = 0xffffffff;

	if (pci_read_config_dword(w->pdev, PCI_VENDOR_ID, &id) ||
	    id == 0xffffffff) {
		w->last_id = id;
		w->gone_at = ktime_get();
		WRITE_ONCE(w->gone, true);
		return;
	}

	now = ktime_get();
	elapsed_us = ktime_us_delta(now, w->start);
	/*
	 * Run 191 completed Apple's final FW_START-to-runtime transition in less
	 * than 16 ms.  Cycle 252 then measured a shorter 65 ms removal while its
	 * ordinary workqueue observer was delayed past the useful window.  Sample
	 * only the first 50 ms on the high-priority workqueue, always after a
	 * successful config-space identity read.  This distinguishes
	 * "N9 started but the response path failed" from "N9 never started"
	 * while leaving at least the directly measured 15 ms pre-removal margin.
	 */
	if (elapsed_us <= MT7932_FW_START_READY_STOP_US) {
		u32 ready = __mt76_rr(w->dev, MT_CONN_ON_MISC);

		if (!w->ready_samples) {
			w->ready_first = ready;
			w->ready_first_us = elapsed_us;
		}
		w->ready_last = ready;
		w->ready_last_us = elapsed_us;
		w->ready_samples++;
	}

	w->last_id = id;
	w->polls++;
	queue_delayed_work(system_highpri_wq, &w->work,
			   msecs_to_jiffies(MT7932_FW_START_READY_PERIOD_MS));
}

static bool mt7932_fw_start_watch_begin(struct mt76_dev *dev,
					struct mt7932_fw_start_watch *w)
{
	if (!is_mt7932(dev) || !dev_is_pci(dev->dev))
		return false;

	memset(w, 0, sizeof(*w));
	w->pdev = to_pci_dev(dev->dev);
	w->dev = dev;
	w->ready_first = U32_MAX;
	w->ready_last = U32_MAX;
	w->start = ktime_get();
	INIT_DELAYED_WORK_ONSTACK(&w->work, mt7932_fw_start_watch_fn);
	queue_delayed_work(system_highpri_wq, &w->work,
			   msecs_to_jiffies(MT7932_FW_START_READY_PERIOD_MS));

	return true;
}

static void mt7932_fw_start_watch_end(struct mt76_dev *dev,
				      struct mt7932_fw_start_watch *w)
{
	u32 entry_marker = U32_MAX;

	cancel_delayed_work_sync(&w->work);
	destroy_delayed_work_on_stack(&w->work);
	if (!READ_ONCE(w->gone))
		entry_marker = __mt76_rr(dev, MT7932_FW_ENTRY_MARKER);
	dev_info(dev->dev,
		 "J700_MT7932_FW_START_EARLY_READY: samples=%u first_us=%lld first=0x%08x first_n9=0x%x last_us=%lld last=0x%08x last_n9=0x%x stop_us=%u\n",
		 w->ready_samples, w->ready_first_us, w->ready_first,
		 w->ready_first & 0x3, w->ready_last_us, w->ready_last,
		 w->ready_last & 0x3, MT7932_FW_START_READY_STOP_US);
	dev_info(dev->dev,
		 "J700_MT7932_FW_ENTRY_MARKER: addr=0x%08x value=0x%08x endpoint_present=%u\n",
		 MT7932_FW_ENTRY_MARKER, entry_marker, !READ_ONCE(w->gone));

	if (READ_ONCE(w->gone))
		dev_err(dev->dev,
			"J700_MT7932_FW_START_LIVENESS: ENDPOINT_GONE polls=%u alive_us=%lld cfg_id=0x%08x\n",
			w->polls, ktime_us_delta(w->gone_at, w->start),
			w->last_id);
	else
		dev_info(dev->dev,
			 "J700_MT7932_FW_START_LIVENESS: ENDPOINT_PRESENT polls=%u window_us=%lld cfg_id=0x%08x\n",
			 w->polls, ktime_us_delta(ktime_get(), w->start),
			 w->last_id);
}

static int
mt76_connac_mcu_send_ram_firmware(struct mt76_dev *dev,
				  const struct mt76_connac2_fw_trailer *hdr,
				  const u8 *data, bool is_wa)
{
	int i, offset = 0, max_len = mt76_is_sdio(dev) ? 2048 : 4096;
	struct mt7932_fw_start_watch watch;
	u32 override = 0, option = 0;
	bool watching = false;
	int ret;

	if (is_mt7932(dev)) {
		max_len = 2048;
		WRITE_ONCE(dev->mcu.mt7932_ram_download_active, false);
		WRITE_ONCE(dev->mcu.mt7932_ram_download_responses, 0);
		WRITE_ONCE(dev->mcu.mt7932_ram_download_first_seq, 0);
		dev_info(dev->dev,
			 "J700_MT7932_SCATTER_LIMIT: phase=ram bytes=%d\n",
			 max_len);
	}

	for (i = 0; i < hdr->n_region; i++) {
		const struct mt76_connac2_fw_region *region;
		u32 len, addr, mode;
		int err;

		region = (const void *)((const u8 *)hdr -
					(hdr->n_region - i) * sizeof(*region));
		mode = mt76_connac_mcu_gen_dl_mode(dev, region->feature_set,
						   is_wa);
		len = le32_to_cpu(region->len);
		addr = le32_to_cpu(region->addr);

		if (region->feature_set & FW_FEATURE_NON_DL)
			goto next;

		if (region->feature_set & FW_FEATURE_OVERRIDE_ADDR)
			override = addr;

		if (is_mt7932(dev))
			WRITE_ONCE(dev->mcu.mt7932_ram_download_active, true);
		err = mt76_connac_mcu_init_download(dev, addr, len, mode);
		if (is_mt7932(dev))
			WRITE_ONCE(dev->mcu.mt7932_ram_download_active, false);
		if (err) {
			dev_err(dev->dev, "Download request failed\n");
			return err;
		}

		err = __mt76_mcu_send_firmware(dev, MCU_CMD(FW_SCATTER),
					       data + offset, len, max_len);
		if (err) {
			dev_err(dev->dev, "Failed to send firmware.\n");
			return err;
		}

next:
		offset += len;
	}
	if (is_mt7932(dev)) {
		ret = mt7932_mcu_drain_fwdl(dev);
		if (ret)
			return ret;
	}

	if (override)
		option |= FW_START_OVERRIDE;
	if (is_wa)
		option |= FW_START_WORKING_PDA_CR4;

	if (is_mt7932(dev)) {
		u32 pc_sel, pc_before;

		ret = mt7932_mcu_require_host_ready(dev);
		if (ret)
			return ret;

		pc_sel = __mt76_rr(dev, MT7932_WM_MCU_PC_LOG_SEL);
		pc_sel &= ~MT7932_WM_MCU_PC_LOG_MASK;
		pc_sel |= MT7932_WM_MCU_PC_LOG_CURRENT;
		__mt76_wr(dev, MT7932_WM_MCU_PC_LOG_SEL, pc_sel);
		pc_sel = __mt76_rr(dev, MT7932_WM_MCU_PC_LOG_SEL);
		pc_before = __mt76_rr(dev, MT7932_WM_MCU_PC_DBG);
		dev_info(dev->dev,
			 "J700_MT7932_WM_PC_ARMED: selector=0x%08x current_pc=0x%08x source=mt7933-host-csr\n",
			 pc_sel, pc_before);

		ret = mt7932_sec_protect_fwdl(dev, true);
		if (ret)
			return ret;

	}

	watching = mt7932_fw_start_watch_begin(dev, &watch);

	ret = mt76_connac_mcu_start_firmware(dev, override, option);

	if (watching)
		mt7932_fw_start_watch_end(dev, &watch);

	if (is_mt7932(dev)) {
		int release_ret;

		if (ret == -ETIMEDOUT) {
			dev_warn(dev->dev,
				 "J700_MT7932_FW_START_RESPONSE_TIMEOUT: release skipped\n");
			return ret;
		}

		release_ret = mt7932_sec_protect_fwdl(dev, false);
		if (!ret)
			ret = release_ret;
	}

	return ret;
}

int mt76_connac2_load_ram(struct mt76_dev *dev, const char *fw_wm,
			  const char *fw_wa)
{
	const struct mt76_connac2_fw_trailer *hdr;
	const struct firmware *fw;
	int ret;

	ret = request_firmware(&fw, fw_wm, dev->dev);
	if (ret)
		return ret;

	if (!fw || !fw->data || fw->size < sizeof(*hdr)) {
		dev_err(dev->dev, "Invalid firmware\n");
		ret = -EINVAL;
		goto out;
	}

	hdr = (const void *)(fw->data + fw->size - sizeof(*hdr));
	dev_info(dev->dev, "WM Firmware Version: %.10s, Build Time: %.15s",
		 hdr->fw_ver, hdr->build_date);

	ret = mt76_connac_mcu_send_ram_firmware(dev, hdr, fw->data, false);
	if (ret) {
		dev_err(dev->dev, "Failed to start WM firmware\n");
		goto out;
	}

	snprintf(dev->hw->wiphy->fw_version,
		 sizeof(dev->hw->wiphy->fw_version),
		 "%.10s-%.15s", hdr->fw_ver, hdr->build_date);

	release_firmware(fw);

	if (!fw_wa)
		return 0;

	ret = request_firmware(&fw, fw_wa, dev->dev);
	if (ret)
		return ret;

	if (!fw || !fw->data || fw->size < sizeof(*hdr)) {
		dev_err(dev->dev, "Invalid firmware\n");
		ret = -EINVAL;
		goto out;
	}

	hdr = (const void *)(fw->data + fw->size - sizeof(*hdr));
	dev_info(dev->dev, "WA Firmware Version: %.10s, Build Time: %.15s",
		 hdr->fw_ver, hdr->build_date);

	ret = mt76_connac_mcu_send_ram_firmware(dev, hdr, fw->data, true);
	if (ret) {
		dev_err(dev->dev, "Failed to start WA firmware\n");
		goto out;
	}

	snprintf(dev->hw->wiphy->fw_version,
		 sizeof(dev->hw->wiphy->fw_version),
		 "%.10s-%.15s", hdr->fw_ver, hdr->build_date);

out:
	release_firmware(fw);

	return ret;
}
EXPORT_SYMBOL_GPL(mt76_connac2_load_ram);

static u32 mt76_connac2_get_data_mode(struct mt76_dev *dev, u32 info)
{
	u32 mode = DL_MODE_NEED_RSP;

	if ((!is_connac2(dev) && !is_connac3(dev)) || info == PATCH_SEC_NOT_SUPPORT)
		return mode;

	switch (FIELD_GET(PATCH_SEC_ENC_TYPE_MASK, info)) {
	case PATCH_SEC_ENC_TYPE_PLAIN:
		break;
	case PATCH_SEC_ENC_TYPE_AES:
		mode |= DL_MODE_ENCRYPT;
		mode |= FIELD_PREP(DL_MODE_KEY_IDX,
				(info & PATCH_SEC_ENC_AES_KEY_MASK)) & DL_MODE_KEY_IDX;
		mode |= DL_MODE_RESET_SEC_IV;
		break;
	case PATCH_SEC_ENC_TYPE_SCRAMBLE:
		mode |= DL_MODE_ENCRYPT;
		mode |= DL_CONFIG_ENCRY_MODE_SEL;
		mode |= DL_MODE_RESET_SEC_IV;
		break;
	default:
		dev_err(dev->dev, "Encryption type not support!\n");
	}

	return mode;
}

int mt76_connac2_load_patch(struct mt76_dev *dev, const char *fw_name)
{
	int i, ret, sem, max_len = mt76_is_sdio(dev) ? 2048 : 4096;
	bool secure_held = false;
	const struct mt76_connac2_patch_hdr *hdr;
	const struct firmware *fw = NULL;

	if (is_mt7932(dev)) {
		max_len = 2048;
		dev_info(dev->dev,
			 "J700_MT7932_SCATTER_LIMIT: phase=patch bytes=%d\n",
			 max_len);
	}

	sem = mt76_connac_mcu_patch_sem_ctrl(dev, true);
	switch (sem) {
	case PATCH_IS_DL:
		return 0;
	case PATCH_NOT_DL_SEM_SUCCESS:
		break;
	case PATCH_REL_SEM_SUCCESS:
		if (is_mt7932(dev)) {
			dev_info(dev->dev,
				 "J700_MT7932_PATCH_SEMAPHORE_ACCEPT: status=%d action=download\n",
				 sem);
			break;
		}
		fallthrough;
	default:
		dev_err(dev->dev, "Failed to get patch semaphore\n");
		return -EAGAIN;
	}

	ret = request_firmware(&fw, fw_name, dev->dev);
	if (ret)
		goto out;

	if (!fw || !fw->data || fw->size < sizeof(*hdr)) {
		dev_err(dev->dev, "Invalid firmware\n");
		ret = -EINVAL;
		goto out;
	}

	hdr = (const void *)fw->data;
	dev_info(dev->dev, "HW/SW Version: 0x%x, Build Time: %.16s",
		 be32_to_cpu(hdr->hw_sw_ver), hdr->build_date);

	for (i = 0; i < be32_to_cpu(hdr->desc.n_region); i++) {
		struct mt76_connac2_patch_sec *sec;
		u32 len, addr, mode;
		const u8 *dl;
		u32 sec_info;

		sec = (void *)(fw->data + sizeof(*hdr) + i * sizeof(*sec));
		if ((be32_to_cpu(sec->type) & PATCH_SEC_TYPE_MASK) !=
		    PATCH_SEC_TYPE_INFO) {
			ret = -EINVAL;
			goto out;
		}

		addr = be32_to_cpu(sec->info.addr);
		len = be32_to_cpu(sec->info.len);
		dl = fw->data + be32_to_cpu(sec->offs);
		sec_info = be32_to_cpu(sec->info.sec_key_idx);
		mode = mt76_connac2_get_data_mode(dev, sec_info);

		ret = mt76_connac_mcu_init_download(dev, addr, len, mode);
		if (ret) {
			dev_err(dev->dev, "Download request failed\n");
			goto out;
		}

		ret = __mt76_mcu_send_firmware(dev, MCU_CMD(FW_SCATTER),
					       dl, len, max_len);
		if (ret) {
			dev_err(dev->dev, "Failed to send patch\n");
			goto out;
		}
	}
	if (is_mt7932(dev)) {
		ret = mt7932_sec_protect_fwdl(dev, true);
		if (ret)
			goto out;
		secure_held = true;
	}

	ret = mt76_connac_mcu_start_patch(dev);
	if (ret)
		dev_err(dev->dev, "Failed to start patch\n");
	if (secure_held) {
		int release_ret = mt7932_sec_protect_fwdl(dev, false);

		secure_held = false;
		if (!ret)
			ret = release_ret;
	}

out:
	if (secure_held)
		mt7932_sec_protect_fwdl(dev, false);
	if (is_mt7932(dev) && !ret) {
		/* AppleSunriseWLAN 25G83 does not send a release command after
		 * PATCH_FINISH on its successful MT7932 path.  Secure-FWDL release
		 * is the direct MMIO operation above, and PATCH_FINISH has already
		 * completed the ROM patch transaction.
		 */
		dev_info(dev->dev,
			 "J700_MT7932_PATCH_SEMAPHORE_RELEASE_SKIP: source=apple-25G83\n");
		release_firmware(fw);
		return 0;
	}
	sem = mt76_connac_mcu_patch_sem_ctrl(dev, false);
	switch (sem) {
	case PATCH_REL_SEM_SUCCESS:
		break;
	case PATCH_REL_SEM_SUCCESS_MT7932:
		if (is_mt7932(dev)) {
			dev_info(dev->dev,
				 "J700_MT7932_PATCH_SEMAPHORE_RELEASE_ACCEPT: status=%d\n",
				 sem);
			break;
		}
		fallthrough;
	default:
		ret = -EAGAIN;
		dev_err(dev->dev, "Failed to release patch semaphore\n");
		break;
	}

	release_firmware(fw);

	return ret;
}
EXPORT_SYMBOL_GPL(mt76_connac2_load_patch);

int mt76_connac2_mcu_fill_message(struct mt76_dev *dev, struct sk_buff *skb,
				  int cmd, int *wait_seq)
{
	int txd_len, mcu_cmd = FIELD_GET(__MCU_CMD_FIELD_ID, cmd);
	bool mt7932 = is_mt7932(dev);
	struct mt76_connac2_mcu_uni_txd *uni_txd;
	struct mt76_connac2_mcu_txd *mcu_txd;
	__le32 *txd;
	u32 val;
	u8 seq;

	/* TODO: make dynamic based on msg type */
	dev->mcu.timeout = 20 * HZ;

	/* Apple's MT7932 init ABI uses the complete nonzero sequence byte and
	 * allocates it only for packets that carry an init header.  Raw scatter
	 * payloads have no header and must not consume 600 sequence numbers.
	 */
	if (mt7932 && cmd == MCU_CMD(FW_SCATTER)) {
		seq = dev->mcu.msg_seq;
		goto exit;
	}
	if (mt7932) {
		seq = ++dev->mcu.msg_seq;
		if (!seq)
			seq = ++dev->mcu.msg_seq;
	} else {
		seq = ++dev->mcu.msg_seq & 0xf;
		if (!seq)
			seq = ++dev->mcu.msg_seq & 0xf;
	}

	if (cmd == MCU_CMD(FW_SCATTER))
		goto exit;

	txd_len = cmd & __MCU_CMD_FIELD_UNI ? sizeof(*uni_txd) : sizeof(*mcu_txd);
	txd = (__le32 *)skb_push(skb, txd_len);

	/* AppleSunriseWLAN 25G83's asicConnac2xFillInitCmdTxd selects packet
	 * format 3 only when the CID is zero (the runtime dummy command).  A
	 * nonzero CID selects packet format 2 (MT_TX_TYPE_CMD); raw scatter has
	 * no header and exits above.
	 */
	val = FIELD_PREP(MT_TXD0_TX_BYTES, skb->len) |
	      FIELD_PREP(MT_TXD0_PKT_FMT,
			 mt7932 && !mcu_cmd ? MT_TX_TYPE_FW :
						 MT_TX_TYPE_CMD);
	if (!mt7932)
		val |= FIELD_PREP(MT_TXD0_Q_IDX, MT_TX_MCU_PORT_RX_Q0);
	txd[0] = cpu_to_le32(val);

	val = FIELD_PREP(MT_TXD1_HDR_FORMAT, MT_HDR_FORMAT_CMD);
	if (!mt7932)
		val |= MT_TXD1_LONG_FORMAT;
	txd[1] = cpu_to_le32(val);

	if (cmd & __MCU_CMD_FIELD_UNI) {
		uni_txd = (struct mt76_connac2_mcu_uni_txd *)txd;
		uni_txd->len = cpu_to_le16(skb->len - sizeof(uni_txd->txd));
		/* MediaTek's unified-command builder only sets ACK when the caller
		 * requested a response.  Apple submits scan and cancel as SET+UNI
		 * with no immediate response; completion arrives asynchronously as
		 * UNI_EVENT_ID_SCAN_DONE.  The earlier MT7932 no-wait path still put
		 * 0x7 (ACK+UNI+SET) on wire because this field was unconditional.
		 */
		if (mt7932) {
			uni_txd->option = MCU_CMD_UNI;
			if (!(cmd & __MCU_CMD_FIELD_QUERY))
				uni_txd->option |= MCU_CMD_SET;
			if (wait_seq || (cmd & __MCU_CMD_FIELD_ACK))
				uni_txd->option |= MCU_CMD_ACK;
		} else {
			uni_txd->option = MCU_CMD_UNI_EXT_ACK;
		}
		uni_txd->cid = cpu_to_le16(mcu_cmd);
		uni_txd->s2d_index = MCU_S2D_H2N;
		uni_txd->pkt_type = MCU_PKT_ID;
		uni_txd->seq = seq;
		if (mt7932 && mcu_cmd == MCU_UNI_CMD_SCAN_REQ)
			dev_info(dev->dev,
				 "J700_MT7932_UNI_SCAN_TXD: txd0=0x%08x txd1=0x%08x len=%u cid=0x%04x pkt=%u seq=%u s2d=%u option=0x%02x wait=%u\n",
				 le32_to_cpu(txd[0]), le32_to_cpu(txd[1]),
				 le16_to_cpu(uni_txd->len),
				 le16_to_cpu(uni_txd->cid), uni_txd->pkt_type,
				 uni_txd->seq, uni_txd->s2d_index,
				 uni_txd->option, !!wait_seq);

		goto exit;
	}

	mcu_txd = (struct mt76_connac2_mcu_txd *)txd;
	mcu_txd->len = cpu_to_le16(skb->len - sizeof(mcu_txd->txd));
	if (!mt7932)
		mcu_txd->pq_id = cpu_to_le16(MCU_PQ_ID(MT_TX_PORT_IDX_MCU,
						       MT_TX_MCU_PORT_RX_Q0));
	mcu_txd->pkt_type = MCU_PKT_ID;
	mcu_txd->seq = seq;
	mcu_txd->cid = mcu_cmd;
	mcu_txd->ext_cid = FIELD_GET(__MCU_CMD_FIELD_EXT_ID, cmd);
	if (mcu_txd->ext_cid || (cmd & __MCU_CMD_FIELD_CE)) {
		if (cmd & __MCU_CMD_FIELD_QUERY)
			mcu_txd->set_query = MCU_Q_QUERY;
		else
			mcu_txd->set_query = MCU_Q_SET;
		mcu_txd->ext_cid_ack = mt7932 ?
					!!wait_seq || !!(cmd & __MCU_CMD_FIELD_ACK) :
					!!mcu_txd->ext_cid;
	} else if (mt7932) {
		/* AppleSunriseWLAN's init-header builder leaves this byte zero;
		 * only the later one-time-calibration command requests SET.
		 */
		if (!mcu_cmd || mcu_cmd == MCU_CMD_ONE_TIME_CAL)
			mcu_txd->set_query = MCU_Q_SET;
	} else {
		mcu_txd->set_query = MCU_Q_NA;
	}

	if (cmd & __MCU_CMD_FIELD_WA)
		mcu_txd->s2d_index = MCU_S2D_H2C;
	else
		mcu_txd->s2d_index = MCU_S2D_H2N;
	if (mt7932 && (cmd & __MCU_CMD_FIELD_CE) &&
	    mcu_cmd == MCU_CE_CMD_START_HW_SCAN)
		dev_info(dev->dev,
			 "J700_MT7932_LEGACY_SCAN_TXD: txd0=0x%08x txd1=0x%08x len=%u cid=0x%02x pkt=%u set=%u seq=%u ext=%u s2d=%u ack=%u\n",
			 le32_to_cpu(txd[0]), le32_to_cpu(txd[1]),
			 le16_to_cpu(mcu_txd->len), mcu_txd->cid,
			 mcu_txd->pkt_type, mcu_txd->set_query,
			 mcu_txd->seq, mcu_txd->ext_cid,
			 mcu_txd->s2d_index, mcu_txd->ext_cid_ack);

	if (mt7932 && (cmd == MCU_CMD(PATCH_SEM_CONTROL) ||
		       cmd == MCU_CMD(FW_START_REQ)))
		dev_info(dev->dev,
			 "J700_MT7932_INIT_TXD: cmd=0x%02x txd0=0x%08x txd1=0x%08x pq_id=0x%04x len=%u cid=0x%02x pkt=%u query=%u seq=%u s2d=%u\n",
			 mcu_cmd,
			 le32_to_cpu(txd[0]), le32_to_cpu(txd[1]),
			 le16_to_cpu(mcu_txd->pq_id),
			 le16_to_cpu(mcu_txd->len), mcu_txd->cid,
			 mcu_txd->pkt_type, mcu_txd->set_query,
			 mcu_txd->seq, mcu_txd->s2d_index);

exit:
	if (wait_seq)
		*wait_seq = seq;

	return 0;
}
EXPORT_SYMBOL_GPL(mt76_connac2_mcu_fill_message);

MODULE_AUTHOR("Lorenzo Bianconi <lorenzo@kernel.org>");
MODULE_DESCRIPTION("MediaTek MT76x connac layer helpers");
MODULE_LICENSE("Dual BSD/GPL");
