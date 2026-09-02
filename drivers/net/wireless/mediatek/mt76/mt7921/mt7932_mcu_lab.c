// SPDX-License-Identifier: BSD-3-Clause-Clear
/* J700-only MT7932 MCU conformance checkpoint. */

#include <crypto/sha1.h>
#include <linux/capability.h>
#include <linux/delay.h>
#include <linux/fs.h>
#include <linux/firmware.h>
#include <linux/if_ether.h>
#include <linux/ip.h>
#include <linux/kref.h>
#include <linux/miscdevice.h>
#include <linux/netdevice.h>
#include <linux/pci.h>
#include <linux/poll.h>
#include <linux/rcupdate.h>
#include <linux/rtnetlink.h>
#include <linux/unaligned.h>
#include <linux/udp.h>
#include <linux/uaccess.h>
#include <linux/xarray.h>
#include <net/cfg80211.h>

#include "mt7921.h"
#include "mt7932_mcu_lab.h"

#define NEO_WIFID_MAX_RECORDS	256
#define NEO_WIFID_MAX_PENDING	32

#define NEO_WIFID_STATE_FW_RUNNING	BIT(0)
#define NEO_WIFID_STATE_RESET		BIT(1)
#define NEO_WIFID_STATE_MCU_RESET	BIT(2)
#define NEO_WIFID_STATE_SCANNING	BIT(3)
#define NEO_WIFID_STATE_HW_INIT		BIT(4)
#define NEO_WIFID_STATE_CHECKPOINT	BIT(5)
#define NEO_WIFID_STATE_DEAD		BIT(6)
#define NEO_WIFID_STATE_RESPONSE_TIMEOUT	BIT(7)
#define NEO_WIFID_STATE_FULLMAC_CONNECTED	BIT(8)

static bool mt7932_fullmac_cfg80211;
static bool mt7932_fullmac_private_vif;

struct mt7932_mcu_lab_pending {
	bool valid;
	bool owned;
	u8 expected_eid;
	u32 cookie;
	unsigned long expires;
};

struct mt7932_mcu_lab_skb_cb {
	u16 type;
};

struct mt7932_fullmac_assoc_cal_snapshot {
	u8 primary;
	u8 cbw;
	u8 band;
	u8 extension;
	bool valid;
};

#define NEO_WIFID_SKB_CB(_skb) \
	((struct mt7932_mcu_lab_skb_cb *)&((_skb)->cb[0]))

struct mt7932_mcu_lab {
	struct kref ref;
	struct mt792x_dev *dev;
	struct miscdevice misc;
	/* Serializes all operations that can change firmware or endpoint state. */
	struct mutex operation_lock;
	/* Protects response-sequence ownership against the MCU RX path. */
	spinlock_t pending_lock;
	struct mt7932_mcu_lab_pending pending[256];
	unsigned int pending_count;
	struct sk_buff_head records;
	wait_queue_head_t read_wait;
	struct completion connected;
	struct completion continued;
	struct completion disconnected;
	atomic_t opened;
	atomic_t dropped_events;
	atomic_t fullmac_scan_bss_count;
	enum neo_wifid_phase phase;
	u32 command_count;
	u8 fullmac_sta_ids[NEO_WIFID_FULLMAC_STA_IDS];
	u8 fullmac_wtbl_ids[NEO_WIFID_FULLMAC_WTBL_IDS];
	u8 fullmac_reserved_count;
	struct mt792x_sta fullmac_sta;
	struct ieee80211_vif *fullmac_vif;
	struct ieee80211_vif *fullmac_internal_vif;
	struct ieee80211_key_conf fullmac_key;
	struct net_device *fullmac_ndev;
	struct wiphy *fullmac_wiphy;
	struct ieee80211_supported_band *fullmac_bands[NUM_NL80211_BANDS];
	struct wireless_dev fullmac_wdev;
	struct completion fullmac_assoc_started;
	struct completion fullmac_cfg_connect_done;
	struct completion fullmac_cfg_cal_done;
	struct completion fullmac_hw_scan_done;
	struct work_struct fullmac_connect_work;
	struct work_struct fullmac_scan_work;
	struct cfg80211_scan_request *fullmac_scan_request;
	struct neo_wifid_fullmac_connect fullmac_cfg_request;
	u32 fullmac_cfg_cal_cookie;
	u16 fullmac_cfg_connect_status;
	bool fullmac_hw_scan_owned;
	bool fullmac_hw_scan_observing;
	bool fullmac_hw_scan_aborted;
	bool fullmac_cfg_connecting;
	bool fullmac_internal_started;
	bool fullmac_wcid_initialized;
	bool fullmac_connected;
	bool fullmac_peer_ready;
	bool fullmac_disconnect_pending;
	bool fullmac_cold_teardown_done;
	u16 fullmac_disconnect_status;
	u8 fullmac_bssid[ETH_ALEN];
	u8 fullmac_security;
	u8 fullmac_oper_band;
	u8 fullmac_oper_channel;
	struct mt7932_fullmac_assoc_cal_snapshot assoc_cal;
	bool checkpoint_waiting;
	bool misc_registered;
	bool response_timed_out;
	bool irq_disabled;
	bool tasklet_disabled;
	bool dead;
};

struct mt7932_fullmac_netdev_priv {
	struct mt7932_mcu_lab *lab;
};

static void mt7932_fullmac_scan_cancel(struct mt7932_mcu_lab *lab);

static bool mt7932_mcu_lab_fullmac_critical_data(struct sk_buff *skb)
{
	struct ethhdr eth;
	struct udphdr udp;
	struct iphdr ip;
	unsigned int offset;

	if (skb_copy_bits(skb, 0, &eth, sizeof(eth)))
		return false;
	if (eth.h_proto == htons(ETH_P_ARP))
		return true;
	if (eth.h_proto != htons(ETH_P_IP) ||
	    skb_copy_bits(skb, ETH_HLEN, &ip, sizeof(ip)) ||
	    ip.version != 4 || ip.ihl < 5 || ip.protocol != IPPROTO_UDP)
		return false;
	offset = ETH_HLEN + ip.ihl * 4;
	if (skb_copy_bits(skb, offset, &udp, sizeof(udp)))
		return false;

	return (udp.source == htons(67) && udp.dest == htons(68)) ||
	       (udp.source == htons(68) && udp.dest == htons(67));
}

static netdev_tx_t mt7932_mcu_lab_fullmac_xmit(struct sk_buff *skb,
					       struct net_device *ndev)
{
	struct mt7932_fullmac_netdev_priv *priv = netdev_priv(ndev);
	struct mt7932_mcu_lab *lab = READ_ONCE(priv->lab);
	struct ieee80211_tx_info *info = IEEE80211_SKB_CB(skb);
	struct mt76_wcid *wcid;
	unsigned int len = skb->len;

	if (!lab || READ_ONCE(lab->dead) ||
	    !READ_ONCE(lab->fullmac_connected) ||
	    !READ_ONCE(lab->fullmac_vif) || skb->len <= ETH_HLEN) {
		ndev->stats.tx_dropped++;
		dev_kfree_skb_any(skb);
		return NETDEV_TX_OK;
	}

	wcid = &lab->fullmac_sta.deflink.wcid;
	memset(info, 0, sizeof(*info));
	info->flags = IEEE80211_TX_CTL_HW_80211_ENCAP;
	info->control.vif = READ_ONCE(lab->fullmac_vif);
	if (READ_ONCE(lab->fullmac_security) !=
	    NEO_WIFID_FULLMAC_SECURITY_OPEN)
		info->control.hw_key = &lab->fullmac_key;
	if (mt7932_mcu_lab_fullmac_critical_data(skb))
		info->flags |= IEEE80211_TX_CTL_REQ_TX_STATUS;
	skb_set_queue_mapping(skb, IEEE80211_AC_BE);
	mt76_tx(&lab->dev->mphy, NULL, wcid, skb);

	ndev->stats.tx_packets++;
	ndev->stats.tx_bytes += len;
	return NETDEV_TX_OK;
}

static int mt7932_mcu_lab_fullmac_open(struct net_device *ndev)
{
	netif_start_queue(ndev);
	return 0;
}

static int mt7932_mcu_lab_fullmac_stop(struct net_device *ndev)
{
	struct mt7932_fullmac_netdev_priv *priv = netdev_priv(ndev);
	struct mt7932_mcu_lab *lab = READ_ONCE(priv->lab);
	struct cfg80211_scan_request *request;
	struct cfg80211_scan_info info = { .aborted = true };
	bool owned;

	netif_stop_queue(ndev);
	if (!lab)
		return 0;
	request = xchg(&lab->fullmac_scan_request, NULL);
	owned = xchg(&lab->fullmac_hw_scan_owned, false);
	WRITE_ONCE(lab->fullmac_hw_scan_observing, false);
	WRITE_ONCE(lab->fullmac_hw_scan_aborted, true);
	if (owned && lab->fullmac_vif)
		mt7932_fullmac_scan_cancel(lab);
	complete_all(&lab->fullmac_hw_scan_done);
	cancel_work_sync(&lab->fullmac_scan_work);
	if (request) {
		cfg80211_scan_done(request, &info);
		dev_info(lab->dev->mt76.dev,
			 "J700_MT7932_CFG80211_SCAN_NETDEV_STOP: owner=netdev-down hardware_owned=%u completion=driver-before-core\n",
			 owned);
	}

	return 0;
}

static int mt7932_mcu_lab_fullmac_set_mac_address(struct net_device *ndev,
						   void *address)
{
	struct mt7932_fullmac_netdev_priv *priv = netdev_priv(ndev);
	struct mt7932_mcu_lab *lab = priv->lab;
	struct sockaddr *sockaddr = address;
	int ret;

	ret = eth_prepare_mac_addr_change(ndev, address);
	if (ret)
		return ret;
	if (!lab || !mutex_trylock(&lab->operation_lock))
		return -EBUSY;
	if (READ_ONCE(lab->fullmac_cfg_connecting) ||
	    READ_ONCE(lab->fullmac_connected)) {
		ret = -EBUSY;
		goto out;
	}

	if (lab->fullmac_internal_vif) {
		ether_addr_copy(lab->fullmac_internal_vif->addr,
				 sockaddr->sa_data);
		ether_addr_copy(lab->fullmac_internal_vif->bss_conf.addr,
				 sockaddr->sa_data);
	}
	eth_commit_mac_addr_change(ndev, address);
	dev_info(lab->dev->mt76.dev,
		 "J700_MT7932_CFG80211_STATION_ADDRESS_SET: source=netdev permanent_unchanged=1 identity=redacted\n");
out:
	mutex_unlock(&lab->operation_lock);
	return ret;
}

static const struct net_device_ops mt7932_mcu_lab_fullmac_netdev_ops = {
	.ndo_open = mt7932_mcu_lab_fullmac_open,
	.ndo_stop = mt7932_mcu_lab_fullmac_stop,
	.ndo_start_xmit = mt7932_mcu_lab_fullmac_xmit,
	.ndo_set_mac_address = mt7932_mcu_lab_fullmac_set_mac_address,
};

static int mt7932_fullmac_cfg_connect(struct wiphy *wiphy,
				      struct net_device *ndev,
				      struct cfg80211_connect_params *params);
static int mt7932_fullmac_cfg_disconnect(struct wiphy *wiphy,
					 struct net_device *ndev,
					 u16 reason_code);
static void mt7932_mcu_lab_select_vif(void *data, u8 *mac,
				     struct ieee80211_vif *vif);
static struct mt7932_mcu_lab *mt7932_fullmac_wiphy_lab(struct wiphy *wiphy);

static struct ieee80211_scan_request *
mt7932_fullmac_scan_request_alloc(struct mt7932_mcu_lab *lab,
				  struct cfg80211_scan_request *request)
{
	struct ieee80211_scan_request *scan;
	struct cfg80211_scan_request *source;
	struct wiphy *source_wiphy = lab->dev->mt76.hw->wiphy;
	unsigned int i, n_channels = request->n_channels;

	lab->fullmac_vif = lab->fullmac_internal_vif;
	if (!lab->fullmac_vif)
		ieee80211_iterate_interfaces(mt76_hw(lab->dev),
					     IEEE80211_IFACE_ITER_NORMAL,
					     mt7932_mcu_lab_select_vif, lab);
	if (!lab->fullmac_vif)
		return NULL;
	if (READ_ONCE(lab->fullmac_connected) &&
	    READ_ONCE(lab->fullmac_oper_channel))
		n_channels = 1;
	scan = kzalloc(sizeof(*scan) +
		       n_channels * sizeof(source->channels[0]),
		       GFP_KERNEL);
	if (!scan)
		return NULL;
	source = &scan->req;
	source->ssids = request->ssids;
	source->n_ssids = request->n_ssids;
	source->n_channels = n_channels;
	source->ie = request->ie;
	source->ie_len = request->ie_len;
	source->flags = request->flags;
	source->wiphy = source_wiphy;
	source->wdev = lab->fullmac_internal_vif ? NULL :
		lab->fullmac_vif ? ieee80211_vif_to_wdev(lab->fullmac_vif) : NULL;
	memcpy(source->rates, request->rates, sizeof(source->rates));
	memcpy(source->mac_addr, request->mac_addr, ETH_ALEN);
	memcpy(source->mac_addr_mask, request->mac_addr_mask, ETH_ALEN);
	memcpy(source->bssid, request->bssid, ETH_ALEN);
	if (n_channels == 1 && READ_ONCE(lab->fullmac_connected)) {
		enum nl80211_band band = lab->fullmac_oper_band == 1 ?
			NL80211_BAND_2GHZ : lab->fullmac_oper_band == 2 ?
			NL80211_BAND_5GHZ : NL80211_BAND_6GHZ;
		int frequency = ieee80211_channel_to_frequency(
			lab->fullmac_oper_channel, band);

		source->channels[0] = ieee80211_get_channel(source_wiphy,
							     frequency);
		if (!source->channels[0]) {
			kfree(scan);
			return NULL;
		}
	} else for (i = 0; i < request->n_channels; i++) {
		source->channels[i] = ieee80211_get_channel(
			source_wiphy, request->channels[i]->center_freq);
		if (!source->channels[i]) {
			kfree(scan);
			return NULL;
		}
	}

	return scan;
}

static void mt7932_fullmac_scan_cancel(struct mt7932_mcu_lab *lab)
{
	struct mt76_vif_link *mvif =
		(struct mt76_vif_link *)lab->fullmac_vif->drv_priv;
	struct {
		u8 seq_num;
		u8 is_ext_channel;
		u8 reserved[2];
	} __packed request = {
		.seq_num = mvif->scan_seq_num,
	};

	mt792x_mutex_acquire(lab->dev);
	skb_queue_purge(&lab->dev->mphy.scan_pending);
	WRITE_ONCE(lab->dev->mphy.scan_active_seq, 0);
	clear_bit(MT76_HW_SCANNING, &lab->dev->mphy.state);
	mt76_mcu_send_msg(&lab->dev->mt76, MCU_CE_CMD(CANCEL_HW_SCAN),
			  &request, sizeof(request), false);
	mt792x_mutex_release(lab->dev);
}

static void mt7932_fullmac_cfg_scan_work(struct work_struct *work)
{
	struct mt7932_mcu_lab *lab = container_of(
		work, struct mt7932_mcu_lab, fullmac_scan_work);
	struct cfg80211_scan_request *request = READ_ONCE(
		lab->fullmac_scan_request);
	struct cfg80211_scan_info info = {};
	struct ieee80211_scan_request *scan;
	unsigned long timeout;
	unsigned int found = 0, directed = 0, i;
	int ret;

	if (!request)
		return;
	for (i = 0; i < request->n_ssids; i++)
		if (request->ssids[i].ssid_len)
			directed++;
	scan = mt7932_fullmac_scan_request_alloc(lab, request);
	if (!scan) {
		info.aborted = true;
		goto done;
	}
	reinit_completion(&lab->fullmac_hw_scan_done);
	WRITE_ONCE(lab->fullmac_hw_scan_aborted, false);
	if (test_bit(MT76_HW_SCANNING, &lab->dev->mphy.state)) {
		WRITE_ONCE(lab->fullmac_hw_scan_observing, true);
		for (i = 0; i < 750 &&
		     test_bit(MT76_HW_SCANNING, &lab->dev->mphy.state); i++)
			msleep(20);
		WRITE_ONCE(lab->fullmac_hw_scan_observing, false);
		if (test_bit(MT76_HW_SCANNING, &lab->dev->mphy.state)) {
			info.aborted = true;
			goto free_scan;
		}
		/* An internal mt76 campaign can have different SSID/channel
		 * parameters.  Waiting for it prevents concurrent firmware scans,
		 * but its results must never complete this cfg80211 request.  Reset
		 * the request-local accounting and submit the exact requested scan.
		 */
		atomic_set(&lab->fullmac_scan_bss_count, 0);
		reinit_completion(&lab->fullmac_hw_scan_done);
		dev_info(lab->dev->mt76.dev,
			 "J700_MT7932_CFG80211_SCAN_RETRY_AFTER_FOREIGN: directed_ssids=%u channels=%u source=internal-mt76-owner-drained\n",
			 directed, request->n_channels);
	}
	WRITE_ONCE(lab->fullmac_hw_scan_owned, true);
	mt792x_mutex_acquire(lab->dev);
	ret = mt7932_mcu_drain_tx17(lab->dev, "cfg80211-scan-pre", false);
	if (!ret)
		ret = mt76_connac_mcu_hw_scan(&lab->dev->mphy,
					     lab->fullmac_vif, scan);
	mt792x_mutex_release(lab->dev);
	if (ret) {
		WRITE_ONCE(lab->fullmac_hw_scan_owned, false);
		info.aborted = true;
		goto free_scan;
	}
	timeout = wait_for_completion_timeout(&lab->fullmac_hw_scan_done,
					       msecs_to_jiffies(60000));
	if (!timeout) {
		WRITE_ONCE(lab->fullmac_hw_scan_owned, false);
		mt7932_fullmac_scan_cancel(lab);
		info.aborted = true;
		dev_err(lab->dev->mt76.dev,
			"J700_MT7932_CFG80211_SCAN_TIMEOUT: source=typed-cid03\n");
		goto free_scan;
	}
	info.aborted = READ_ONCE(lab->fullmac_hw_scan_aborted);
	if (!info.aborted)
		found = atomic_read(&lab->fullmac_scan_bss_count);
free_scan:
	kfree(scan);
done:
	if (cmpxchg(&lab->fullmac_scan_request, request, NULL) == request) {
		cfg80211_scan_done(request, &info);
		dev_info(lab->dev->mt76.dev,
			 "J700_MT7932_CFG80211_SCAN_MIRROR: directed_ssids=%u channels=%u found=%u aborted=%u source=typed-cid03+direct-rx-publication\n",
			 directed, request->n_channels, found, info.aborted);
	}
}

static int mt7932_fullmac_cfg_scan(struct wiphy *wiphy,
				   struct cfg80211_scan_request *request)
{
	struct mt7932_mcu_lab *lab =
		*(struct mt7932_mcu_lab **)wiphy_priv(wiphy);
	struct cfg80211_scan_request *active;

	if (!lab)
		return -EBUSY;
	active = cmpxchg(&lab->fullmac_scan_request, NULL, request);
	if (active) {
		dev_info(lab->dev->mt76.dev,
			 "J700_MT7932_CFG80211_SCAN_REJECT: reason=request-active\n");
		return -EBUSY;
	}
	atomic_set(&lab->fullmac_scan_bss_count, 0);
	if (!schedule_work(&lab->fullmac_scan_work)) {
		cmpxchg(&lab->fullmac_scan_request, request, NULL);
		dev_info(lab->dev->mt76.dev,
			 "J700_MT7932_CFG80211_SCAN_REJECT: reason=worker-active rollback=1\n");
		return -EBUSY;
	}
	return 0;
}

static void mt7932_fullmac_cfg_abort_scan(struct wiphy *wiphy,
					  struct wireless_dev *wdev)
{
	struct mt7932_mcu_lab *lab = mt7932_fullmac_wiphy_lab(wiphy);
	struct cfg80211_scan_request *request;
	struct cfg80211_scan_info info = { .aborted = true };
	bool owned;

	if (!lab || wdev != &lab->fullmac_wdev)
		return;
	request = xchg(&lab->fullmac_scan_request, NULL);
	if (!request)
		return;
	owned = xchg(&lab->fullmac_hw_scan_owned, false);
	WRITE_ONCE(lab->fullmac_hw_scan_observing, false);
	WRITE_ONCE(lab->fullmac_hw_scan_aborted, true);
	if (owned && lab->fullmac_vif)
		mt7932_fullmac_scan_cancel(lab);
	complete_all(&lab->fullmac_hw_scan_done);
	cfg80211_scan_done(request, &info);
	dev_info(lab->dev->mt76.dev,
		 "J700_MT7932_CFG80211_SCAN_ABORT: owner=cfg80211 hardware_owned=%u completion=single\n",
		 owned);
}

static const struct cfg80211_ops mt7932_fullmac_cfg_ops = {
	.scan = mt7932_fullmac_cfg_scan,
	.abort_scan = mt7932_fullmac_cfg_abort_scan,
	.connect = mt7932_fullmac_cfg_connect,
	.disconnect = mt7932_fullmac_cfg_disconnect,
};

static const u32 mt7932_fullmac_cipher_suites[] = {
	WLAN_CIPHER_SUITE_CCMP,
};

static void mt7932_fullmac_free_bands(struct mt7932_mcu_lab *lab)
{
	int band;

	for (band = 0; band < NUM_NL80211_BANDS; band++) {
		if (!lab->fullmac_bands[band])
			continue;
		kfree(lab->fullmac_bands[band]->channels);
		kfree(lab->fullmac_bands[band]);
		lab->fullmac_bands[band] = NULL;
	}
}

static int mt7932_fullmac_clone_bands(struct mt7932_mcu_lab *lab,
				       struct wiphy *source)
{
	int band;

	for (band = 0; band < NUM_NL80211_BANDS; band++) {
		struct ieee80211_supported_band *clone;
		struct ieee80211_supported_band *original = source->bands[band];

		if (!original)
			continue;
		clone = kmemdup(original, sizeof(*clone), GFP_KERNEL);
		if (!clone)
			goto fail;
		clone->channels = kmemdup(
			original->channels,
			array_size(original->n_channels,
				   sizeof(*original->channels)), GFP_KERNEL);
		if (!clone->channels) {
			kfree(clone);
			goto fail;
		}
		lab->fullmac_bands[band] = clone;
		lab->fullmac_wiphy->bands[band] = clone;
	}

	return 0;
fail:
	mt7932_fullmac_free_bands(lab);
	return -ENOMEM;
}

static void mt7932_mcu_lab_select_vif(void *data, u8 *mac,
				      struct ieee80211_vif *vif)
{
	struct mt7932_mcu_lab *lab = data;

	if (vif->type == NL80211_IFTYPE_STATION && !lab->fullmac_vif)
		lab->fullmac_vif = vif;
}

static int mt7932_mcu_lab_register_fullmac_netdev(
	struct mt7932_mcu_lab *lab)
{
	struct mt7932_fullmac_netdev_priv *priv;
	struct net_device *ndev;
	int ret;

	if (lab->fullmac_ndev)
		return 0;
	ndev = alloc_etherdev(sizeof(*priv));
	if (!ndev)
		return -ENOMEM;
	priv = netdev_priv(ndev);
	priv->lab = lab;
	ndev->netdev_ops = &mt7932_mcu_lab_fullmac_netdev_ops;
	ndev->min_mtu = ETH_MIN_MTU;
	ndev->max_mtu = ETH_DATA_LEN;
	ndev->priv_flags |= IFF_LIVE_ADDR_CHANGE;
	strscpy(ndev->name, "neo-wifi%d", IFNAMSIZ);
	ndev->name_assign_type = NET_NAME_PREDICTABLE;
	SET_NETDEV_DEV(ndev, lab->dev->mt76.dev);
	eth_hw_addr_set(ndev, lab->dev->mphy.macaddr);
	ether_addr_copy(ndev->perm_addr, lab->dev->mphy.macaddr);
	netif_carrier_off(ndev);
	if (mt7932_fullmac_cfg80211) {
		struct wiphy *source = lab->dev->mt76.hw->wiphy;
		struct mt7932_mcu_lab **wiphy_lab;

		lab->fullmac_wiphy = wiphy_new(&mt7932_fullmac_cfg_ops,
					       sizeof(*wiphy_lab));
		if (!lab->fullmac_wiphy) {
			free_netdev(ndev);
			return -ENOMEM;
		}
		wiphy_lab = wiphy_priv(lab->fullmac_wiphy);
		*wiphy_lab = lab;
		set_wiphy_dev(lab->fullmac_wiphy, lab->dev->mt76.dev);
		lab->fullmac_wiphy->interface_modes = BIT(NL80211_IFTYPE_STATION);
		lab->fullmac_wiphy->max_scan_ssids = 4;
		lab->fullmac_wiphy->max_scan_ie_len = 2304;
		lab->fullmac_wiphy->signal_type = CFG80211_SIGNAL_TYPE_MBM;
		lab->fullmac_wiphy->features |=
			NL80211_FEATURE_SCAN_RANDOM_MAC_ADDR;
		lab->fullmac_wiphy->cipher_suites = mt7932_fullmac_cipher_suites;
		lab->fullmac_wiphy->n_cipher_suites =
			ARRAY_SIZE(mt7932_fullmac_cipher_suites);
		wiphy_ext_feature_set(lab->fullmac_wiphy,
				      NL80211_EXT_FEATURE_OWE_OFFLOAD);
		wiphy_ext_feature_set(lab->fullmac_wiphy,
				      NL80211_EXT_FEATURE_SAE_OFFLOAD);
		wiphy_ext_feature_set(lab->fullmac_wiphy,
				      NL80211_EXT_FEATURE_MFP_OPTIONAL);
		wiphy_ext_feature_set(
			lab->fullmac_wiphy,
			NL80211_EXT_FEATURE_4WAY_HANDSHAKE_STA_PSK);
		ret = mt7932_fullmac_clone_bands(lab, source);
		if (ret) {
			wiphy_free(lab->fullmac_wiphy);
			lab->fullmac_wiphy = NULL;
			free_netdev(ndev);
			return ret;
		}
		if (lab->dev->mt76.custom_regd) {
			lab->fullmac_wiphy->regulatory_flags |=
				REGULATORY_WIPHY_SELF_MANAGED;
		}
		ret = wiphy_register(lab->fullmac_wiphy);
		if (ret) {
			wiphy_free(lab->fullmac_wiphy);
			lab->fullmac_wiphy = NULL;
			mt7932_fullmac_free_bands(lab);
			free_netdev(ndev);
			return ret;
		}
		if (lab->dev->mt76.custom_regd) {
			ret = regulatory_set_wiphy_regd(
				lab->fullmac_wiphy,
				(struct ieee80211_regdomain *)
				lab->dev->mt76.custom_regd);
			if (ret) {
				wiphy_unregister(lab->fullmac_wiphy);
				wiphy_free(lab->fullmac_wiphy);
				lab->fullmac_wiphy = NULL;
				mt7932_fullmac_free_bands(lab);
				free_netdev(ndev);
				return ret;
			}
		}
		lab->fullmac_wdev.wiphy = lab->fullmac_wiphy;
		lab->fullmac_wdev.iftype = NL80211_IFTYPE_STATION;
		lab->fullmac_wdev.netdev = ndev;
		ndev->ieee80211_ptr = &lab->fullmac_wdev;
	}
	ret = register_netdev(ndev);
	if (ret) {
		if (lab->fullmac_wiphy) {
			wiphy_unregister(lab->fullmac_wiphy);
			wiphy_free(lab->fullmac_wiphy);
			lab->fullmac_wiphy = NULL;
			mt7932_fullmac_free_bands(lab);
		}
		free_netdev(ndev);
		return ret;
	}
	lab->fullmac_ndev = ndev;
	return 0;
}

static int mt7932_mcu_lab_enable_internal_scan_vif(
	struct mt7932_mcu_lab *lab)
{
	struct ieee80211_hw *hw = mt76_hw(lab->dev);
	struct ieee80211_vif *vif;
	struct wireless_dev *wdev;
	int ret;

	if (mt7932_mcu_lab_private_vif_enabled()) {
		const u8 *address = lab->fullmac_ndev ?
			lab->fullmac_ndev->dev_addr : lab->dev->mphy.macaddr;

		vif = kzalloc(sizeof(*vif) + hw->vif_data_size, GFP_KERNEL);
		if (!vif)
			return -ENOMEM;
		vif->type = NL80211_IFTYPE_STATION;
		vif->cfg.idle = true;
		vif->addr_valid = true;
		ether_addr_copy(vif->addr, address);
		ether_addr_copy(vif->bss_conf.addr, vif->addr);
		vif->bss_conf.bssid = vif->addr;
		vif->bss_conf.vif = vif;
		vif->bss_conf.link_id = 0;
		mt792x_mutex_acquire(lab->dev);
		ret = __mt7921_start(&lab->dev->phy);
		mt792x_mutex_release(lab->dev);
		if (ret) {
			kfree(vif);
			return ret;
		}
		lab->fullmac_internal_started = true;
		ret = mt7921_add_driver_internal_vif(lab->dev, vif);
		if (ret) {
			mt792x_stop(hw, false);
			lab->fullmac_internal_started = false;
			kfree(vif);
			return ret;
		}
		lab->fullmac_internal_vif = vif;
		lab->fullmac_vif = vif;
		dev_info(lab->dev->mt76.dev,
			 "J700_MT7932_CFG80211_PRIVATE_VIF_READY: source=driver-private state=up mac80211_netdev=0 ownership=driver-internal\n");
		return 0;
	}

	lab->fullmac_vif = NULL;
	ieee80211_iterate_interfaces(mt76_hw(lab->dev),
				     IEEE80211_IFACE_ITER_NORMAL,
				     mt7932_mcu_lab_select_vif, lab);
	if (!lab->fullmac_vif)
		return -ENODEV;
	wdev = ieee80211_vif_to_wdev(lab->fullmac_vif);
	if (!wdev || !wdev->netdev)
		return -ENODEV;
	rtnl_lock();
	ret = dev_open(wdev->netdev, NULL);
	rtnl_unlock();
	if (!ret)
		dev_info(lab->dev->mt76.dev,
			 "J700_MT7932_CFG80211_INTERNAL_SCAN_VIF_READY: source=mac80211 state=up ownership=driver-internal\n");

	return ret;
}

static void mt7932_mcu_lab_remove_internal_scan_vif(
	struct mt7932_mcu_lab *lab)
{
	struct ieee80211_vif *vif = lab->fullmac_internal_vif;

	if (!vif)
		return;
	mt7921_remove_driver_internal_vif(lab->dev, vif);
	if (lab->fullmac_internal_started) {
		mt792x_stop(mt76_hw(lab->dev), false);
		lab->fullmac_internal_started = false;
	}
	lab->fullmac_internal_vif = NULL;
	lab->fullmac_vif = NULL;
	kfree(vif);
}

static int mt7932_mcu_lab_prepare_fullmac_data_path(
	struct mt7932_mcu_lab *lab)
{
	struct mt792x_link_sta *mlink = &lab->fullmac_sta.deflink;
	struct mt792x_dev *dev = lab->dev;
	struct mt792x_vif *mvif;
	int i;

	lab->fullmac_vif = lab->fullmac_internal_vif;
	if (!lab->fullmac_vif)
		ieee80211_iterate_interfaces(mt76_hw(dev),
					     IEEE80211_IFACE_ITER_NORMAL,
					     mt7932_mcu_lab_select_vif, lab);
	if (!lab->fullmac_vif || !lab->fullmac_ndev)
		return -ENODEV;

	/* The lab Ethernet facade and firmware station must expose one identity.
	 * mac80211 can select a per-network/private station address after the
	 * facade was registered; keeping the old mphy address here makes DHCP and
	 * bridged data disagree with the address programmed by CID 0xb8.
	 */
	rtnl_lock();
	eth_hw_addr_set(lab->fullmac_ndev,
			lab->fullmac_vif->bss_conf.addr);
	rtnl_unlock();
	dev_info(dev->mt76.dev,
		 "J700_MT7932_FULLMAC_IDENTITY_SYNC_PASS: netdev=ethernet station=selected-vif identity=redacted\n");

	mvif = (struct mt792x_vif *)lab->fullmac_vif->drv_priv;
	memset(&lab->fullmac_sta, 0, sizeof(lab->fullmac_sta));
	mt76_wcid_init(&mlink->wcid, 0);
	mlink->wcid.idx = lab->fullmac_wtbl_ids[0];
	mlink->wcid.hw_key_idx = lab->fullmac_security ==
		NEO_WIFID_FULLMAC_SECURITY_OPEN ? -1 : 0;
	mlink->wcid.tx_info |= MT_WCID_TX_INFO_SET | MT_WCID_TX_INFO_ALTQ |
		MT_WCID_TX_INFO_J700_FULLMAC;
	mlink->last_txs = jiffies;
	mlink->sta = &lab->fullmac_sta;
	lab->fullmac_sta.vif = mvif;
	lab->fullmac_wcid_initialized = true;
	lab->fullmac_peer_ready = false;

	memset(&lab->fullmac_key, 0, sizeof(lab->fullmac_key));
	lab->fullmac_key.cipher = WLAN_CIPHER_SUITE_CCMP;
	lab->fullmac_key.keyidx = 0;

	mutex_lock(&dev->mt76.mutex);
	for (i = 0; i < NEO_WIFID_FULLMAC_WTBL_IDS; i++)
		rcu_assign_pointer(dev->mt76.wcid[lab->fullmac_wtbl_ids[i]],
				   &mlink->wcid);
	mutex_unlock(&dev->mt76.mutex);
	dev_info(dev->mt76.dev,
		 "J700_MT7932_FULLMAC_DATA_PATH_READY: netdev=%s wtbl_count=4 tx=802.3 rx=802.3 queue=ALTX0 txd=j700-fullmac hw_key=%s amsdu=off identity=redacted\n",
		 lab->fullmac_ndev->name,
		 lab->fullmac_security == NEO_WIFID_FULLMAC_SECURITY_OPEN ?
		 "none" : "firmware-installed");

	return 0;
}

static void mt7932_mcu_lab_release_fullmac_resources(
	struct mt7932_mcu_lab *lab)
{
	struct mt792x_dev *dev = lab->dev;
	int i;

	if (!lab->fullmac_reserved_count)
		return;
	WRITE_ONCE(lab->fullmac_connected, false);
	WRITE_ONCE(lab->fullmac_oper_band, 0);
	WRITE_ONCE(lab->fullmac_oper_channel, 0);
	WRITE_ONCE(lab->fullmac_security, 0);
	if (lab->fullmac_ndev) {
		netif_carrier_off(lab->fullmac_ndev);
		netif_tx_disable(lab->fullmac_ndev);
	}
	mutex_lock(&dev->mt76.mutex);
	if (lab->fullmac_wcid_initialized) {
		for (i = 0; i < NEO_WIFID_FULLMAC_WTBL_IDS; i++)
			rcu_assign_pointer(
				dev->mt76.wcid[lab->fullmac_wtbl_ids[i]], NULL);
	}
	for (i = 0; i < lab->fullmac_reserved_count; i++) {
		u8 id = i < NEO_WIFID_FULLMAC_STA_IDS ?
			lab->fullmac_sta_ids[i] :
			lab->fullmac_wtbl_ids[i - NEO_WIFID_FULLMAC_STA_IDS];

		mt76_wcid_mask_clear(dev->mt76.wcid_mask, id);
	}
	mutex_unlock(&dev->mt76.mutex);
	if (lab->fullmac_wcid_initialized) {
		synchronize_rcu();
		mt76_wcid_cleanup(&dev->mt76,
				  &lab->fullmac_sta.deflink.wcid);
		lab->fullmac_wcid_initialized = false;
	}
	lab->fullmac_vif = NULL;
	lab->fullmac_peer_ready = false;
	lab->fullmac_disconnect_pending = false;
	lab->fullmac_disconnect_status = 0xffff;
	eth_zero_addr(lab->fullmac_bssid);
	memset(lab->fullmac_sta_ids, 0, sizeof(lab->fullmac_sta_ids));
	memset(lab->fullmac_wtbl_ids, 0, sizeof(lab->fullmac_wtbl_ids));
	lab->fullmac_reserved_count = 0;
}

static int mt7932_mcu_lab_reserve_fullmac_resources(
	struct mt7932_mcu_lab *lab)
{
	struct mt792x_dev *dev = lab->dev;
	int id, i;

	if (lab->fullmac_reserved_count)
		return -EBUSY;

	mutex_lock(&dev->mt76.mutex);
	for (i = 0; i < NEO_WIFID_FULLMAC_STA_IDS; i++) {
		id = mt76_wcid_alloc(dev->mt76.wcid_mask, 15);
		if (id < 0)
			goto unwind;
		lab->fullmac_sta_ids[i] = id;
		lab->fullmac_reserved_count++;
	}
	for (i = 0; i < NEO_WIFID_FULLMAC_WTBL_IDS; i++) {
		id = mt76_wcid_alloc(dev->mt76.wcid_mask, MT792x_WTBL_STA - 1);
		if (id < 0)
			goto unwind;
		lab->fullmac_wtbl_ids[i] = id;
		lab->fullmac_reserved_count++;
	}
	mutex_unlock(&dev->mt76.mutex);

	return 0;

unwind:
	while (lab->fullmac_reserved_count) {
		int pos = --lab->fullmac_reserved_count;
		u8 allocated = pos < NEO_WIFID_FULLMAC_STA_IDS ?
			lab->fullmac_sta_ids[pos] :
			lab->fullmac_wtbl_ids[pos - NEO_WIFID_FULLMAC_STA_IDS];

		mt76_wcid_mask_clear(dev->mt76.wcid_mask, allocated);
	}
	mutex_unlock(&dev->mt76.mutex);
	memset(lab->fullmac_sta_ids, 0, sizeof(lab->fullmac_sta_ids));
	memset(lab->fullmac_wtbl_ids, 0, sizeof(lab->fullmac_wtbl_ids));

	return -ENOSPC;
}

static DEFINE_XARRAY(mt7932_mcu_labs);

static bool mt7932_mcu_lab_conformance;
module_param_named(j700_mcu_lab_conformance, mt7932_mcu_lab_conformance,
		   bool, 0444);
MODULE_PARM_DESC(j700_mcu_lab_conformance,
		 "pause J700 probe after firmware start for neo-wifid conformance");

static unsigned int mt7932_mcu_lab_timeout_ms = 120000;
module_param_named(j700_mcu_lab_timeout_ms, mt7932_mcu_lab_timeout_ms,
		   uint, 0444);
MODULE_PARM_DESC(j700_mcu_lab_timeout_ms,
		 "fail-closed J700 early-probe checkpoint timeout in milliseconds");

static bool mt7932_mcu_lab_runtime;
module_param_named(j700_mcu_lab_runtime, mt7932_mcu_lab_runtime, bool, 0444);
MODULE_PARM_DESC(j700_mcu_lab_runtime,
		 "create the J700 typed MCU lab endpoint after normal hardware init");

module_param_named(j700_fullmac_cfg80211, mt7932_fullmac_cfg80211, bool, 0444);
MODULE_PARM_DESC(j700_fullmac_cfg80211,
		 "register the experimental J700 cfg80211 fullmac station interface");

module_param_named(j700_fullmac_private_vif, mt7932_fullmac_private_vif,
		   bool, 0444);
MODULE_PARM_DESC(j700_fullmac_private_vif,
		 "use a driver-private MT7932 vif without a mac80211 netdev");

bool mt7932_mcu_lab_conformance_active(void)
{
	return READ_ONCE(mt7932_mcu_lab_conformance);
}

bool mt7932_mcu_lab_private_vif_enabled(void)
{
	return READ_ONCE(mt7932_fullmac_cfg80211) &&
	       READ_ONCE(mt7932_fullmac_private_vif);
}

static void mt7932_mcu_lab_release(struct kref *ref)
{
	struct mt7932_mcu_lab *lab =
		container_of(ref, struct mt7932_mcu_lab, ref);

	skb_queue_purge(&lab->records);
	if (lab->fullmac_ndev)
		free_netdev(lab->fullmac_ndev);
	if (lab->fullmac_wiphy)
		wiphy_free(lab->fullmac_wiphy);
	mt7932_fullmac_free_bands(lab);
	kfree(lab->misc.name);
	kfree(lab);
}

static struct mt7932_mcu_lab *mt7932_mcu_lab_get(struct mt792x_dev *dev)
{
	struct mt7932_mcu_lab *lab;

	rcu_read_lock();
	lab = xa_load(&mt7932_mcu_labs, (unsigned long)dev);
	if (lab && !kref_get_unless_zero(&lab->ref))
		lab = NULL;
	rcu_read_unlock();

	return lab;
}

static void mt7932_mcu_lab_put(struct mt7932_mcu_lab *lab)
{
	kref_put(&lab->ref, mt7932_mcu_lab_release);
}

static struct sk_buff *
mt7932_mcu_lab_record_alloc(u16 type, u32 cookie, int status, u32 command,
			    u32 sequence, const void *payload, size_t payload_len,
			    gfp_t gfp)
{
	struct neo_wifid_record *record;
	struct sk_buff *skb;

	if (payload_len > NEO_WIFID_MAX_RECORD)
		return NULL;
	skb = alloc_skb(sizeof(*record) + payload_len, gfp);
	if (!skb)
		return NULL;
	record = skb_put_zero(skb, sizeof(*record));
	record->magic = cpu_to_le32(NEO_WIFID_MAGIC);
	record->version = cpu_to_le16(NEO_WIFID_VERSION);
	record->type = cpu_to_le16(type);
	record->cookie = cpu_to_le32(cookie);
	record->status = cpu_to_le32((u32)status);
	record->command = cpu_to_le32(command);
	record->sequence = cpu_to_le32(sequence);
	record->payload_len = cpu_to_le32(payload_len);
	record->monotonic_ns = cpu_to_le64(ktime_get_mono_fast_ns());
	if (payload_len)
		skb_put_data(skb, payload, payload_len);
	NEO_WIFID_SKB_CB(skb)->type = type;

	return skb;
}

static void mt7932_mcu_lab_queue(struct mt7932_mcu_lab *lab,
				 struct sk_buff *skb)
{
	struct sk_buff *item, *drop = NULL;
	unsigned long flags;
	u16 type;

	if (!skb)
		return;
	type = NEO_WIFID_SKB_CB(skb)->type;
	spin_lock_irqsave(&lab->records.lock, flags);
	if (lab->records.qlen >= NEO_WIFID_MAX_RECORDS) {
		skb_queue_walk(&lab->records, item) {
			if (NEO_WIFID_SKB_CB(item)->type ==
			    NEO_WIFID_RECORD_EVENT) {
				drop = item;
				__skb_unlink(item, &lab->records);
				break;
			}
		}
		if (!drop && type == NEO_WIFID_RECORD_EVENT) {
			atomic_inc(&lab->dropped_events);
			spin_unlock_irqrestore(&lab->records.lock, flags);
			kfree_skb(skb);
			return;
		}
	}
	__skb_queue_tail(&lab->records, skb);
	spin_unlock_irqrestore(&lab->records.lock, flags);
	if (drop) {
		atomic_inc(&lab->dropped_events);
		kfree_skb(drop);
	}
	wake_up_interruptible(&lab->read_wait);
}

static void mt7932_mcu_lab_result(struct mt7932_mcu_lab *lab, u16 type,
				  u32 cookie, int status, u32 command,
				  u32 sequence, const void *payload,
				  size_t payload_len)
{
	struct sk_buff *skb;

	skb = mt7932_mcu_lab_record_alloc(type, cookie, status, command,
					    sequence, payload, payload_len,
					    GFP_KERNEL);
	mt7932_mcu_lab_queue(lab, skb);
}

static unsigned int mt7932_mcu_lab_pending_count(struct mt7932_mcu_lab *lab)
{
	unsigned long flags;
	unsigned int count;

	spin_lock_irqsave(&lab->pending_lock, flags);
	count = lab->pending_count;
	spin_unlock_irqrestore(&lab->pending_lock, flags);

	return count;
}

static void mt7932_mcu_lab_expire(struct mt7932_mcu_lab *lab)
{
	u32 expired[NEO_WIFID_MAX_PENDING];
	unsigned long flags;
	unsigned int count = 0;
	int i;

	spin_lock_irqsave(&lab->pending_lock, flags);
	for (i = 1; i < ARRAY_SIZE(lab->pending) &&
	     count < ARRAY_SIZE(expired); i++) {
		if (!lab->pending[i].valid ||
		    time_is_after_jiffies(lab->pending[i].expires))
			continue;
		expired[count++] = lab->pending[i].cookie;
		lab->pending[i].valid = false;
		lab->pending[i].owned = false;
		lab->pending_count--;
	}
	spin_unlock_irqrestore(&lab->pending_lock, flags);

	for (i = 0; i < count; i++)
		mt7932_mcu_lab_result(lab, NEO_WIFID_RECORD_RESPONSE,
					 expired[i], -ETIMEDOUT, 0, 0,
					 NULL, 0);
	if (count)
		WRITE_ONCE(lab->response_timed_out, true);
}

static void mt7932_mcu_lab_abort_epoch(struct mt7932_mcu_lab *lab,
				       const char *reason)
{
	struct mt792x_dev *dev = lab->dev;
	bool armed = lab->tasklet_disabled || lab->irq_disabled;

	if (lab->tasklet_disabled) {
		tasklet_enable(&dev->mt76.irq_tasklet);
		lab->tasklet_disabled = false;
	}
	if (lab->irq_disabled) {
		enable_irq(to_pci_dev(dev->mt76.dev)->irq);
		lab->irq_disabled = false;
	}
	if (armed)
		mt76_mmio_j700_reg_write_disarm(&dev->mt76, reason);
}

static int mt7932_mcu_lab_gate(struct mt7932_mcu_lab *lab)
{
	struct mt792x_dev *dev = lab->dev;
	unsigned long *state = &dev->mphy.state;

	if (lab->dead || test_bit(MT76_REMOVED, state))
		return -ENODEV;
	if (!test_bit(MT76_STATE_MCU_RUNNING, state))
		return -EAGAIN;
	if (test_bit(MT76_RESET, state) ||
	    test_bit(MT76_MCU_RESET, state) || dev->hw_full_reset ||
	    dev->pm.suspended)
		return -EBUSY;

	return 0;
}

static int mt7932_mcu_lab_command(const struct neo_wifid_request *request,
				  u32 *command)
{
	u32 transport = le32_to_cpu(request->transport);
	u32 cid = le32_to_cpu(request->cid);
	u32 ext_cid = le32_to_cpu(request->ext_cid);
	u32 flags = le32_to_cpu(request->flags);
	u32 cmd;

	if (cid > U8_MAX || ext_cid > U8_MAX)
		return -ERANGE;
	switch (transport) {
	case NEO_WIFID_TRANSPORT_CE:
		if (ext_cid)
			return -EINVAL;
		cmd = __MCU_CMD_FIELD_CE |
		      FIELD_PREP(__MCU_CMD_FIELD_ID, cid);
		break;
	case NEO_WIFID_TRANSPORT_EXT_RAW:
		cmd = FIELD_PREP(__MCU_CMD_FIELD_ID, cid) |
		      FIELD_PREP(__MCU_CMD_FIELD_EXT_ID, ext_cid);
		break;
	case NEO_WIFID_TRANSPORT_UNI:
		if (ext_cid)
			return -EINVAL;
		cmd = __MCU_CMD_FIELD_UNI |
		      FIELD_PREP(__MCU_CMD_FIELD_ID, cid);
		break;
	case NEO_WIFID_TRANSPORT_LEGACY_RAW:
		if (ext_cid)
			return -EINVAL;
		cmd = FIELD_PREP(__MCU_CMD_FIELD_ID, cid);
		break;
	default:
		return -EINVAL;
	}
	if (flags & NEO_WIFID_F_QUERY)
		cmd |= __MCU_CMD_FIELD_QUERY;
	if (flags & NEO_WIFID_F_WIRE_ACK)
		cmd |= __MCU_CMD_FIELD_ACK;
	*command = cmd;

	return 0;
}

static u8 mt7932_mcu_lab_next_sequence(struct mt792x_dev *dev)
{
	u8 sequence = READ_ONCE(dev->mt76.mcu.msg_seq) + 1;

	if (!sequence)
		sequence++;

	return sequence;
}

static int mt7932_mcu_lab_submit(struct mt7932_mcu_lab *lab,
				 const struct neo_wifid_request *request,
				 const void *payload, u32 payload_len,
				 u32 *command_out, u32 *sequence_out)
{
	struct mt792x_dev *dev = lab->dev;
	struct mt7932_mcu_lab_pending *pending = NULL;
	u32 flags = le32_to_cpu(request->flags);
	u32 cookie = le32_to_cpu(request->cookie);
	u32 timeout_ms = le32_to_cpu(request->timeout_ms);
	struct sk_buff *skb;
	unsigned long irq_flags;
	u8 expected_sequence = 0;
	int actual_sequence = 0;
	int *sequence = NULL;
	u32 command;
	int ret;

	ret = mt7932_mcu_lab_gate(lab);
	if (ret)
		return ret;
	ret = mt7932_mcu_lab_command(request, &command);
	if (ret)
		return ret;
	if ((flags & NEO_WIFID_F_WAIT_RESPONSE) &&
	    !(flags & NEO_WIFID_F_WIRE_ACK))
		return -EINVAL;
	if ((flags & NEO_WIFID_F_WAIT_EVENT) &&
	    (flags & (NEO_WIFID_F_WIRE_ACK | NEO_WIFID_F_WAIT_RESPONSE)))
		return -EINVAL;
	if (lab->phase == NEO_WIFID_PHASE_EPOCH_ARMED) {
		if (le32_to_cpu(request->transport) !=
			    NEO_WIFID_TRANSPORT_EXT_RAW ||
		    le32_to_cpu(request->cid) != 0xed ||
		    le32_to_cpu(request->ext_cid) != 0x81 ||
		    flags || payload_len != 4)
			return -EPROTO;
	} else if (lab->phase == NEO_WIFID_PHASE_EPOCH_COMMAND ||
		   lab->phase == NEO_WIFID_PHASE_ERROR) {
		return -EBUSY;
	}

	if (flags & NEO_WIFID_F_DRAIN_TX) {
		ret = mt7932_mcu_drain_tx17(dev, "neo-wifid", false);
		if (ret)
			return ret;
	}

	skb = mt76_mcu_msg_alloc(&dev->mt76, payload, payload_len);
	if (!skb)
		return -ENOMEM;

	mutex_lock(&dev->mt76.mcu.mutex);
	if (flags & (NEO_WIFID_F_WIRE_ACK | NEO_WIFID_F_WAIT_EVENT)) {
		spin_lock_irqsave(&lab->pending_lock, irq_flags);
		if (lab->pending_count >= NEO_WIFID_MAX_PENDING) {
			spin_unlock_irqrestore(&lab->pending_lock, irq_flags);
			ret = -ENOSPC;
			goto unlock_free;
		}
		expected_sequence = mt7932_mcu_lab_next_sequence(dev);
		pending = &lab->pending[expected_sequence];
		if (pending->owned) {
			spin_unlock_irqrestore(&lab->pending_lock, irq_flags);
			ret = -EOVERFLOW;
			goto unlock_free;
		}
		pending->valid = true;
		pending->owned = true;
		pending->expected_eid =
			flags & NEO_WIFID_F_WAIT_EVENT ?
			le32_to_cpu(request->cid) : 0;
		pending->cookie = cookie;
		pending->expires = jiffies +
			msecs_to_jiffies(clamp_t(u32, timeout_ms, 1, 60000));
		lab->pending_count++;
		spin_unlock_irqrestore(&lab->pending_lock, irq_flags);
		sequence = &actual_sequence;
	}

	/* mt7921_mcu_send_message() owns TXD construction, sequence allocation,
	 * WFDMA queueing and TX17 pressure cleanup.  Userspace never supplies a
	 * TXD, descriptor, DMA address, ring index or MMIO write.
	 */
	ret = dev->mt76.mcu_ops->mcu_skb_send_msg(&dev->mt76, skb, command,
						  sequence);
	skb = NULL;
	if (!ret && sequence && actual_sequence != expected_sequence)
		ret = -EPROTO;
	if (ret && pending) {
		spin_lock_irqsave(&lab->pending_lock, irq_flags);
		if (pending->valid && pending->cookie == cookie) {
			pending->valid = false;
			pending->owned = false;
			lab->pending_count--;
		}
		spin_unlock_irqrestore(&lab->pending_lock, irq_flags);
	}
	mutex_unlock(&dev->mt76.mcu.mutex);
	if (ret)
		return ret;

	lab->command_count++;
	if (lab->phase == NEO_WIFID_PHASE_EPOCH_ARMED)
		lab->phase = NEO_WIFID_PHASE_EPOCH_COMMAND;
	*command_out = command;
	*sequence_out = sequence ? actual_sequence : 0;

	return 0;

unlock_free:
	mutex_unlock(&dev->mt76.mcu.mutex);
	kfree_skb(skb);
	return ret;
}

static void mt7932_mcu_lab_state(struct mt7932_mcu_lab *lab,
				 struct neo_wifid_state *state)
{
	struct mt792x_dev *dev = lab->dev;
	struct mt76_queue *queue = dev->mt76.q_mcu[MT_MCUQ_WM];
	unsigned long phy_state = READ_ONCE(dev->mphy.state);
	u32 flags = 0;

	if (test_bit(MT76_STATE_MCU_RUNNING, &phy_state))
		flags |= NEO_WIFID_STATE_FW_RUNNING;
	if (test_bit(MT76_RESET, &phy_state))
		flags |= NEO_WIFID_STATE_RESET;
	if (test_bit(MT76_MCU_RESET, &phy_state))
		flags |= NEO_WIFID_STATE_MCU_RESET;
	if (test_bit(MT76_HW_SCANNING, &phy_state))
		flags |= NEO_WIFID_STATE_SCANNING;
	if (dev->hw_init_done)
		flags |= NEO_WIFID_STATE_HW_INIT;
	if (lab->checkpoint_waiting)
		flags |= NEO_WIFID_STATE_CHECKPOINT;
	if (lab->dead)
		flags |= NEO_WIFID_STATE_DEAD;
	if (READ_ONCE(lab->response_timed_out))
		flags |= NEO_WIFID_STATE_RESPONSE_TIMEOUT;
	if (READ_ONCE(lab->fullmac_connected))
		flags |= NEO_WIFID_STATE_FULLMAC_CONNECTED;

	memset(state, 0, sizeof(*state));
	state->phase = cpu_to_le32(lab->phase);
	state->flags = cpu_to_le32(flags);
	state->phy_state = cpu_to_le64(phy_state);
	state->command_count = cpu_to_le32(lab->command_count);
	state->pending_responses =
		cpu_to_le32(mt7932_mcu_lab_pending_count(lab));
	state->queued_records = cpu_to_le32(skb_queue_len(&lab->records));
	state->dropped_events =
		cpu_to_le32(atomic_read(&lab->dropped_events));
	if (queue) {
		state->tx17_cidx = cpu_to_le32(readl(&queue->regs->cpu_idx));
		state->tx17_didx = cpu_to_le32(readl(&queue->regs->dma_idx));
		state->tx17_head = cpu_to_le32(queue->head);
		state->tx17_tail = cpu_to_le32(queue->tail);
		state->tx17_queued = cpu_to_le32(READ_ONCE(queue->queued));
	}
}

static int mt7932_mcu_lab_arm_epoch(struct mt7932_mcu_lab *lab)
{
	struct mt792x_dev *dev = lab->dev;
	int ret;

	ret = mt7932_mcu_lab_gate(lab);
	if (ret)
		return ret;
	if (lab->phase != NEO_WIFID_PHASE_CHECKPOINT ||
	    mt7932_mcu_lab_pending_count(lab))
		return -EBUSY;
	ret = mt76_mmio_j700_reg_write_arm(&dev->mt76);
	if (ret)
		return ret;
	disable_irq(to_pci_dev(dev->mt76.dev)->irq);
	lab->irq_disabled = true;
	tasklet_disable(&dev->mt76.irq_tasklet);
	lab->tasklet_disabled = true;
	lab->phase = NEO_WIFID_PHASE_EPOCH_ARMED;

	return 0;
}

static int mt7932_mcu_lab_runtime_epoch(struct mt7932_mcu_lab *lab)
{
	int ret;

	if (lab->phase != NEO_WIFID_PHASE_EPOCH_COMMAND ||
	    mt7932_mcu_lab_pending_count(lab))
		return -EBUSY;
	ret = mt7932_mcu_runtime_epoch(lab->dev);
	/* The typed helper owns the matching disarm/tasklet/IRQ unwind. */
	lab->irq_disabled = false;
	lab->tasklet_disabled = false;
	if (ret) {
		lab->phase = NEO_WIFID_PHASE_ERROR;
		return ret;
	}
	lab->phase = NEO_WIFID_PHASE_EPOCH_DONE;

	return 0;
}

static int mt7932_mcu_lab_continue(struct mt7932_mcu_lab *lab)
{
	if (lab->phase != NEO_WIFID_PHASE_EPOCH_DONE ||
	    mt7932_mcu_lab_pending_count(lab) ||
	    READ_ONCE(lab->response_timed_out))
		return -EBUSY;
	lab->phase = NEO_WIFID_PHASE_CONTINUED;
	complete_all(&lab->continued);

	return 0;
}

static int mt7932_mcu_lab_reset(struct mt7932_mcu_lab *lab)
{
	struct mt792x_dev *dev = lab->dev;
	int ret;

	if (lab->checkpoint_waiting || !dev->hw_init_done ||
	    mt7932_mcu_lab_pending_count(lab))
		return -EBUSY;
	ret = mt7932_mcu_lab_gate(lab);
	if (ret)
		return ret;
	mt7932_mcu_lab_release_fullmac_resources(lab);
	/* mt7921_mac_reset_work() invokes mt7932_mcu_lab_reset_prepare(),
	 * which must take operation_lock to clear the endpoint's pending state.
	 * The write path normally holds that lock across an opcode, so hand it
	 * off while the reset work runs and reacquire it before returning the
	 * typed result.  Holding it across flush_work() self-deadlocks reset.
	 */
	mutex_unlock(&lab->operation_lock);
	mt792x_reset(&dev->mt76);
	flush_work(&dev->reset_work);
	mutex_lock(&lab->operation_lock);
	if (test_bit(MT76_RESET, &dev->mphy.state) ||
	    test_bit(MT76_MCU_RESET, &dev->mphy.state) ||
	    !test_bit(MT76_STATE_MCU_RUNNING, &dev->mphy.state))
		return -EIO;
	spin_lock_irq(&lab->pending_lock);
	memset(lab->pending, 0, sizeof(lab->pending));
	lab->pending_count = 0;
	spin_unlock_irq(&lab->pending_lock);
	lab->response_timed_out = false;
	if (lab->phase == NEO_WIFID_PHASE_ERROR)
		lab->phase = NEO_WIFID_PHASE_CONTINUED;

	return 0;
}

void mt7932_mcu_lab_reset_prepare(struct mt792x_dev *dev)
{
	struct mt7932_mcu_lab *lab = mt7932_mcu_lab_get(dev);
	unsigned long flags;

	if (!lab)
		return;
	mutex_lock(&lab->operation_lock);
	mt7932_mcu_lab_release_fullmac_resources(lab);
	spin_lock_irqsave(&lab->pending_lock, flags);
	memset(lab->pending, 0, sizeof(lab->pending));
	lab->pending_count = 0;
	lab->assoc_cal.valid = false;
	spin_unlock_irqrestore(&lab->pending_lock, flags);
	lab->response_timed_out = false;
	lab->fullmac_cold_teardown_done = false;
	lab->phase = NEO_WIFID_PHASE_CONTINUED;
	mutex_unlock(&lab->operation_lock);
	dev_info(dev->mt76.dev,
		 "J700_MT7932_LAB_RESET_PREPARE_PASS: pending=0 fullmac_resources=0 phase=continued\n");
	mt7932_mcu_lab_put(lab);
}
EXPORT_SYMBOL_GPL(mt7932_mcu_lab_reset_prepare);

static int mt7932_mcu_lab_teardown_fullmac(
	struct mt7932_mcu_lab *lab, const u8 *bssid, u32 *command,
	const char *phase)
{
	struct mt792x_vif *old_mvif;
	u8 old_sta;
	int ret;

	if (!lab->fullmac_reserved_count || !lab->fullmac_vif ||
	    !bssid || is_zero_ether_addr(bssid))
		return -EINVAL;

	old_mvif = (struct mt792x_vif *)lab->fullmac_vif->drv_priv;
	old_sta = lab->fullmac_sta_ids[0];
	reinit_completion(&lab->disconnected);
	WRITE_ONCE(lab->fullmac_disconnect_status, 0xffff);
	WRITE_ONCE(lab->fullmac_disconnect_pending, true);
	ret = mt7932_mcu_fullmac_disconnect(lab->dev, bssid, command);
	if (ret)
		goto fail;
	if (!wait_for_completion_timeout(&lab->disconnected,
					 msecs_to_jiffies(10000))) {
		ret = -ETIMEDOUT;
		goto fail;
	}
	if (READ_ONCE(lab->fullmac_disconnect_status)) {
		ret = -EREMOTEIO;
		goto fail;
	}
	ret = mt7932_mcu_fullmac_queue_unbind(
		lab->dev, old_mvif->bss_conf.mt76.idx, old_sta, command);
	if (ret)
		goto fail;

	dev_info(lab->dev->mt76.dev,
		 "J700_MT7932_FULLMAC_TEARDOWN_PASS: phase=%s disconnect=ea42 queue_actions=5,1 old_bss=%u old_sta=%u wait=event-gated source=apple25G83\n",
		 phase, old_mvif->bss_conf.mt76.idx, old_sta);

	return 0;

fail:
	WRITE_ONCE(lab->fullmac_disconnect_pending, false);
	dev_err(lab->dev->mt76.dev,
		"J700_MT7932_FULLMAC_TEARDOWN_FAIL: err=%d phase=%s stage=disconnect-or-queue-cleanup\n",
		ret, phase);

	return ret;
}

static int mt7932_mcu_lab_cold_teardown(
	struct mt7932_mcu_lab *lab, const u8 *bssid, u32 payload_len,
	u32 *command)
{
	int ret;

	if (payload_len != ETH_ALEN || !bssid || is_zero_ether_addr(bssid))
		return -EINVAL;
	if (lab->phase != NEO_WIFID_PHASE_CONTINUED ||
	    lab->checkpoint_waiting || !lab->dev->hw_init_done ||
	    lab->fullmac_reserved_count || lab->fullmac_cold_teardown_done)
		return -EBUSY;

	ret = mt7932_mcu_lab_reserve_fullmac_resources(lab);
	if (ret)
		return ret;
	ret = mt7932_mcu_lab_prepare_fullmac_data_path(lab);
	if (ret)
		goto release;
	ret = mt7932_mcu_lab_teardown_fullmac(
		lab, bssid, command, "cold-preconnect");

release:
	mt7932_mcu_lab_release_fullmac_resources(lab);
	if (!ret)
		lab->fullmac_cold_teardown_done = true;

	return ret;
}

static int mt7932_mcu_lab_disconnect_fullmac(
	struct mt7932_mcu_lab *lab, u32 *command)
{
	int ret;

	if (lab->phase != NEO_WIFID_PHASE_CONTINUED ||
	    lab->checkpoint_waiting || !lab->dev->hw_init_done ||
	    !lab->fullmac_reserved_count || !lab->fullmac_vif ||
	    is_zero_ether_addr(lab->fullmac_bssid))
		return -EBUSY;

	ret = mt7932_mcu_lab_teardown_fullmac(
		lab, lab->fullmac_bssid, command, "explicit-disconnect");
	if (ret)
		return ret;
	mt7932_mcu_lab_release_fullmac_resources(lab);
	dev_info(lab->dev->mt76.dev,
		 "J700_MT7932_FULLMAC_EXPLICIT_DISCONNECT_PASS: carrier=off resources=0 source=typed-ea42+cid6b-actions5,1\n");

	return 0;
}

static int mt7932_mcu_lab_fullmac_connect(
	struct mt7932_mcu_lab *lab,
	const struct neo_wifid_fullmac_connect *request,
	u32 payload_len, u32 *command)
{
	struct mt7932_fullmac_connect_params params = {};
	struct mt792x_vif *mvif;
	unsigned long flags;
	int ret;

	BUILD_BUG_ON(sizeof(*request) != 176);
	if (payload_len != sizeof(*request) ||
	    memchr_inv(request->reserved1, 0, sizeof(request->reserved1)) ||
	    request->pre_assoc_cbw > 4 || request->pre_assoc_extension > 3 ||
	    !request->ssid_len || request->ssid_len > sizeof(request->ssid) ||
	    request->key_len > sizeof(request->key) ||
	    request->pmk_len > sizeof(request->pmk) ||
	    request->band < 1 || request->band > 3 || !request->channel ||
	    is_zero_ether_addr(request->bssid))
		return -EINVAL;
	if (request->security == NEO_WIFID_FULLMAC_SECURITY_WPA2_PSK) {
		if (!request->pmk_len)
			return -EINVAL;
	} else if (request->security == NEO_WIFID_FULLMAC_SECURITY_WPA3_SAE) {
		if (request->key_len < 8 || request->key_len > 63 ||
		    request->pmk_len != 32)
			return -EINVAL;
	} else if (request->security == NEO_WIFID_FULLMAC_SECURITY_OWE) {
		if (request->key_len || request->pmk_len)
			return -EINVAL;
	} else if (request->security == NEO_WIFID_FULLMAC_SECURITY_OPEN) {
		if (request->key_len || request->pmk_len)
			return -EINVAL;
	} else {
		return -EINVAL;
	}
	if (lab->phase != NEO_WIFID_PHASE_CONTINUED ||
	    lab->checkpoint_waiting || !lab->dev->hw_init_done)
		return -EBUSY;
	spin_lock_irqsave(&lab->pending_lock, flags);
	lab->assoc_cal.valid = false;
	if (request->pre_assoc_cbw || request->pre_assoc_extension) {
		lab->assoc_cal.primary = request->channel;
		lab->assoc_cal.cbw = request->pre_assoc_cbw;
		lab->assoc_cal.band = request->band;
		lab->assoc_cal.extension = request->pre_assoc_extension;
		lab->assoc_cal.valid = request->band == 3;
	}
	spin_unlock_irqrestore(&lab->pending_lock, flags);
	if ((request->pre_assoc_cbw || request->pre_assoc_extension) &&
	    request->band != 3)
		return -EINVAL;
	if (request->pre_assoc_cbw || request->pre_assoc_extension)
		dev_info(lab->dev->mt76.dev,
			 "J700_MT7932_FULLMAC_PRE_ASSOC_CAL_ARMED: band=%u primary=%u cbw=%u extension=%u source=live-bss-diagnostic\n",
			 request->band, request->channel,
			 request->pre_assoc_cbw,
			 request->pre_assoc_extension);

	/* The live 25G83 OWE trace performs EA/0x42 and both CID 0x6b
	 * removal actions before its first visible connect request.  A fresh
	 * Linux firmware session otherwise transmits the first authentication
	 * frame without receiving an ACK; the byte-identical retry succeeds only
	 * after this measured cleanup.  Allocate temporary live IDs so the cold
	 * path uses the typed queue builder rather than captured station numbers.
	 */
	if (!lab->fullmac_reserved_count &&
	    !lab->fullmac_cold_teardown_done) {
		ret = mt7932_mcu_lab_cold_teardown(
			lab, request->bssid, ETH_ALEN, command);
		if (ret)
			return ret;
	}

	if (lab->fullmac_reserved_count) {
		ret = mt7932_mcu_lab_teardown_fullmac(
			lab, lab->fullmac_bssid, command, "previous-connect");
		if (ret)
			return ret;
		mt7932_mcu_lab_release_fullmac_resources(lab);
	}

	lab->fullmac_security = request->security;
	ret = mt7932_mcu_lab_reserve_fullmac_resources(lab);
	if (ret) {
		lab->fullmac_security = 0;
		return ret;
	}
	ret = mt7932_mcu_lab_prepare_fullmac_data_path(lab);
	if (ret) {
		mt7932_mcu_lab_release_fullmac_resources(lab);
		return ret;
	}
	mvif = (struct mt792x_vif *)lab->fullmac_vif->drv_priv;
	params.ssid = request->ssid;
	params.key = request->key;
	params.pmk = request->pmk;
	params.bssid = request->bssid;
	params.vif_mac = lab->fullmac_vif->bss_conf.addr;
	params.ssid_len = request->ssid_len;
	params.key_len = request->key_len;
	params.pmk_len = request->pmk_len;
	params.band = request->band;
	params.channel = request->channel;
	params.security = request->security;
	params.listen_interval =
		READ_ONCE(lab->dev->mphy.hw->conf.listen_interval);
	/* Apple's fullmac connect path requests the asynchronous extended report
	 * stream which carries EA auth/association/connect lifecycle events.
	 */
	params.ext_report_info = 1;
	params.bss_idx = mvif->bss_conf.mt76.idx;
	memcpy(params.sta_ids, lab->fullmac_sta_ids, sizeof(params.sta_ids));
	memcpy(params.wtbl_ids, lab->fullmac_wtbl_ids, sizeof(params.wtbl_ids));
	dev_info(lab->dev->mt76.dev,
		 "J700_MT7932_FULLMAC_RESOURCES: bss=%u sta=%u,%u wtbl=%u,%u,%u,%u source=live-mt76-allocation\n",
		 params.bss_idx, params.sta_ids[0], params.sta_ids[1],
		 params.wtbl_ids[0], params.wtbl_ids[1],
		 params.wtbl_ids[2], params.wtbl_ids[3]);
	ret = mt7932_mcu_fullmac_connect(lab->dev, &params, command);
	if (!ret)
		ether_addr_copy(lab->fullmac_bssid, request->bssid);
	memzero_explicit(&params, sizeof(params));
	if (ret)
		mt7932_mcu_lab_release_fullmac_resources(lab);
	else
		lab->command_count++;

	return ret;
}

struct mt7932_lab_cal_stream_header {
	u8 magic[8];
	__le16 version;
	__le16 records;
	__le32 size;
} __packed;

static int mt7932_fullmac_assoc_center_channel(
	const struct mt7932_fullmac_assoc_cal_snapshot *snapshot, u8 *center)
{
	u8 primary = snapshot->primary;

	if (!primary || snapshot->band < 1 || snapshot->band > 3)
		return -EINVAL;
	if (!snapshot->cbw) {
		if (snapshot->extension == 1) {
			if (primary > U8_MAX - 2)
				return -EINVAL;
			primary += 2;
		} else if (snapshot->extension == 3) {
			if (primary <= 2)
				return -EINVAL;
			primary -= 2;
		}
		*center = primary;
		return 0;
	}
	if (snapshot->band == 1)
		return -EOPNOTSUPP;
	if (snapshot->band == 2) {
		if (snapshot->cbw == 2) {
			if (primary >= 36 && primary <= 64)
				*center = 50;
			else if (primary >= 100 && primary <= 128)
				*center = 114;
			else if (primary >= 149 && primary <= 177)
				*center = 163;
			else
				return -EINVAL;
			return 0;
		}
		if (snapshot->cbw != 1 && snapshot->cbw != 3)
			return -EOPNOTSUPP;
		if (primary >= 36 && primary <= 48)
			*center = 42;
		else if (primary >= 52 && primary <= 64)
			*center = 58;
		else if (primary >= 100 && primary <= 112)
			*center = 106;
		else if (primary >= 116 && primary <= 128)
			*center = 122;
		else if (primary >= 132 && primary <= 144)
			*center = 138;
		else if (primary >= 149 && primary <= 161)
			*center = 155;
		else
			return -EINVAL;
		return 0;
	}

	if (snapshot->cbw == 2) {
		*center = 15 + ((primary - 1) / 32) * 32;
		return *center ? 0 : -EINVAL;
	}
	if (snapshot->cbw == 1 || snapshot->cbw == 3) {
		*center = 7 + ((primary - 1) / 16) * 16;
		return *center ? 0 : -EINVAL;
	}

	return -EOPNOTSUPP;
}

static int mt7932_fullmac_assoc_cal_group(u8 band, u8 center, u8 *group)
{
	static const struct {
		u16 frequency;
		u8 group;
	} singleton[] = {
		{ 6825, 33 }, { 6665, 32 }, { 6505, 31 }, { 6345, 30 },
		{ 6185, 29 }, { 6025, 28 }, { 5815, 27 }, { 5570, 26 },
		{ 5410, 25 }, { 5250, 24 },
	};
	static const struct {
		u16 first;
		u16 last;
	} broad[] = {
		{ 2400, 2494 }, { 5170, 5250 }, { 5251, 5330 },
		{ 5331, 5490 }, { 5491, 5570 }, { 5571, 5650 },
		{ 5651, 5734 }, { 5735, 5815 }, { 5816, 5925 },
		{ 5926, 6025 }, { 6026, 6105 }, { 6106, 6185 },
		{ 6186, 6265 }, { 6266, 6345 }, { 6346, 6425 },
		{ 6426, 6505 }, { 6506, 6585 }, { 6586, 6665 },
		{ 6666, 6745 }, { 6746, 6825 }, { 6826, 6905 },
		{ 6906, 6985 }, { 6986, 7065 }, { 7066, 7125 },
	};
	u16 frequency;
	int i;

	if (band == 1)
		frequency = center == 14 ? 2484 : 2407 + 5 * center;
	else if (band == 2)
		frequency = 5000 + 5 * center;
	else if (band == 3)
		frequency = center == 2 ? 5935 : 5950 + 5 * center;
	else
		return -EINVAL;

	for (i = 0; i < ARRAY_SIZE(singleton); i++)
		if (frequency == singleton[i].frequency) {
			*group = singleton[i].group;
			return 0;
		}
	for (i = 0; i < ARRAY_SIZE(broad); i++)
		if (frequency >= broad[i].first && frequency <= broad[i].last) {
			*group = i;
			return 0;
		}

	return -ERANGE;
}

static int mt7932_mcu_lab_fullmac_assoc_cal(
	struct mt7932_mcu_lab *lab, u32 cookie, u32 *command_out,
	u32 *sequence_out)
{
	const struct mt7932_lab_cal_stream_header *header;
	struct mt7932_fullmac_assoc_cal_snapshot snapshot;
	struct neo_wifid_request request = {
		.magic = cpu_to_le32(NEO_WIFID_MAGIC),
		.version = cpu_to_le16(NEO_WIFID_VERSION),
		.opcode = cpu_to_le16(NEO_WIFID_OP_SUBMIT),
		.cookie = cpu_to_le32(cookie),
		.transport = cpu_to_le32(NEO_WIFID_TRANSPORT_CE),
		.cid = cpu_to_le32(MCU_CMD_ONE_TIME_CAL),
		.timeout_ms = cpu_to_le32(60000),
	};
	const struct firmware *firmware;
	const u8 *cal_data[2] = {};
	u16 cal_data_len[2] = {};
	u8 *record[2] = {};
	unsigned long flags;
	size_t offset;
	u32 packed, wire_param;
	u32 command, sequence;
	u16 count, i;
	u8 center, group;
	int ret, block;

	spin_lock_irqsave(&lab->pending_lock, flags);
	snapshot = lab->assoc_cal;
	lab->assoc_cal.valid = false;
	spin_unlock_irqrestore(&lab->pending_lock, flags);
	if (!snapshot.valid)
		return -EAGAIN;
	ret = mt7932_fullmac_assoc_center_channel(&snapshot, &center);
	if (ret)
		return ret;
	ret = mt7932_fullmac_assoc_cal_group(snapshot.band, center, &group);
	if (ret)
		return ret;

	ret = request_firmware(&firmware, MT7932_ONE_TIME_CAL,
			       lab->dev->mt76.dev);
	if (ret)
		return ret;
	ret = mt7932_validate_cal_stream(firmware, &count);
	if (ret)
		goto out_release;
	header = (const void *)firmware->data;
	if (count != le16_to_cpu(header->records)) {
		ret = -EINVAL;
		goto out_release;
	}

	offset = sizeof(*header);
	for (i = 0; i < count; i++) {
		const u8 *source;
		u16 length = get_unaligned_le16(firmware->data + offset);

		offset += 4;
		source = firmware->data + offset;
		offset += length;
		if (length <= 20 || source[0] != 1 || source[1] ||
		    source[2] >> 4 != 2 ||
		    get_unaligned_le32(source + 8) != group)
			continue;
		block = source[2] & 0xf;
		if (block >= ARRAY_SIZE(cal_data) || cal_data[block]) {
			ret = -EINVAL;
			goto out_release;
		}
		cal_data[block] = source + 20;
		cal_data_len[block] = length - 20;
	}
	if (!cal_data[0] || !cal_data[1] ||
	    cal_data_len[0] != cal_data_len[1]) {
		ret = -ENOENT;
		goto out_release;
	}

	packed = BIT(26) | ((snapshot.band - 1) << 12) | center;
	wire_param = (packed & 0xfe000000) | (packed & 0x00010000) |
		     ((packed & 0xff) << 17) | group;
	for (block = 0; block < ARRAY_SIZE(record); block++) {
		u16 length = 20 + cal_data_len[block];

		record[block] = kzalloc(length, GFP_KERNEL);
		if (!record[block]) {
			ret = -ENOMEM;
			goto out_free;
		}
		record[block][0] = 1;
		record[block][2] = 0x20 | block;
		record[block][3] = 1;
		put_unaligned_le32(cal_data_len[block], record[block] + 4);
		put_unaligned_le32(wire_param, record[block] + 8);
		memcpy(record[block] + 20, cal_data[block], cal_data_len[block]);
	}

	ret = mt7932_mcu_lab_submit(lab, &request, record[0],
				    20 + cal_data_len[0], &command, &sequence);
	if (ret)
		goto out_free;
	request.flags = cpu_to_le32(NEO_WIFID_F_WAIT_EVENT);
	ret = mt7932_mcu_lab_submit(lab, &request, record[1],
				    20 + cal_data_len[1], &command, &sequence);
	if (ret)
		goto out_free;
	*command_out = command;
	*sequence_out = sequence;
	dev_info(lab->dev->mt76.dev,
		 "J700_MT7932_FULLMAC_ASSOC_CAL_TYPED: band=%u primary=%u center=%u group=%u blocks=2 bytes=%u wire_param=0x%08x source=selector0x4d+oca-stream\n",
		 snapshot.band, snapshot.primary, center, group,
		 20 + cal_data_len[0], wire_param);

out_free:
	kfree(record[0]);
	kfree(record[1]);
out_release:
	release_firmware(firmware);
	return ret;
}

static int mt7932_mcu_lab_fullmac_queue_bind(
	struct mt7932_mcu_lab *lab, u32 *command)
{
	if (lab->phase != NEO_WIFID_PHASE_CONTINUED ||
	    !lab->fullmac_reserved_count || !lab->fullmac_vif ||
	    !lab->fullmac_ndev)
		return -EBUSY;

	return mt7932_mcu_fullmac_queue_bind(
		lab->dev,
		((struct mt792x_vif *)lab->fullmac_vif->drv_priv)->bss_conf.mt76.idx,
		lab->fullmac_sta_ids[0], command);
}

static int mt7932_mcu_lab_fullmac_assoc_power(
	struct mt7932_mcu_lab *lab, u32 *command)
{
	if (lab->phase != NEO_WIFID_PHASE_CONTINUED ||
	    !lab->fullmac_reserved_count || !lab->fullmac_vif ||
	    !lab->fullmac_ndev)
		return -EBUSY;

	return mt7932_mcu_load_assoc_power(lab->dev, command);
}

static struct mt7932_mcu_lab *mt7932_fullmac_wiphy_lab(struct wiphy *wiphy)
{
	return *(struct mt7932_mcu_lab **)wiphy_priv(wiphy);
}

static struct cfg80211_bss *
mt7932_fullmac_cfg_seed_bss(struct wiphy *wiphy,
			   struct ieee80211_channel *channel, bool privacy,
			   const u8 *bssid, const u8 *ssid, size_t ssid_len)
{
	u16 capability = WLAN_CAPABILITY_ESS;
	u8 ie[2 + IEEE80211_MAX_SSID_LEN] = {
		WLAN_EID_SSID, ssid_len,
	};

	if (privacy)
		capability |= WLAN_CAPABILITY_PRIVACY;
	memcpy(ie + 2, ssid, ssid_len);
	return cfg80211_inform_bss(
		wiphy, channel, CFG80211_BSS_FTYPE_UNKNOWN, bssid, 0,
		capability,
		100, ie, 2 + ssid_len, 0, GFP_KERNEL);
}

static const char *mt7932_fullmac_security_name(u8 security)
{
	return security == NEO_WIFID_FULLMAC_SECURITY_OPEN ? "open" :
	       security == NEO_WIFID_FULLMAC_SECURITY_OWE ? "owe" :
	       security == NEO_WIFID_FULLMAC_SECURITY_WPA3_SAE ? "wpa3-sae" :
	       "wpa2-psk";
}

static void mt7932_fullmac_derive_sae_pmk(const u8 *password,
					  size_t password_len,
					  const u8 *ssid, size_t ssid_len,
					  u8 pmk[WLAN_PMK_LEN])
{
	struct hmac_sha1_key key;
	u8 salt[IEEE80211_MAX_SSID_LEN + sizeof(__be32)];
	u8 u[SHA1_DIGEST_SIZE], next[SHA1_DIGEST_SIZE];
	u8 block_value[SHA1_DIGEST_SIZE];
	unsigned int block, iteration, i, copied = 0;

	hmac_sha1_preparekey(&key, password, password_len);
	memcpy(salt, ssid, ssid_len);
	for (block = 1; copied < WLAN_PMK_LEN; block++) {
		size_t length = min_t(size_t, SHA1_DIGEST_SIZE,
				      WLAN_PMK_LEN - copied);

		put_unaligned_be32(block, salt + ssid_len);
		hmac_sha1(&key, salt, ssid_len + sizeof(__be32), u);
		memcpy(block_value, u, sizeof(block_value));
		for (iteration = 1; iteration < 4096; iteration++) {
			hmac_sha1(&key, u, sizeof(u), next);
			memcpy(u, next, sizeof(u));
			for (i = 0; i < sizeof(block_value); i++)
				block_value[i] ^= u[i];
		}
		memcpy(pmk + copied, block_value, length);
		copied += length;
	}
	memzero_explicit(&key, sizeof(key));
	memzero_explicit(salt, sizeof(salt));
	memzero_explicit(u, sizeof(u));
	memzero_explicit(next, sizeof(next));
	memzero_explicit(block_value, sizeof(block_value));
}

static int mt7932_fullmac_cfg_attempt(struct mt7932_mcu_lab *lab,
				      struct neo_wifid_fullmac_connect *request,
				      unsigned int attempt)
{
	u32 command = 0, sequence = 0;
	unsigned long timeout;
	int ret;

	reinit_completion(&lab->fullmac_assoc_started);
	reinit_completion(&lab->fullmac_cfg_connect_done);
	WRITE_ONCE(lab->fullmac_cfg_connect_status, 0xffff);
	mutex_lock(&lab->operation_lock);
	ret = mt7932_mcu_lab_fullmac_connect(
		lab, request, sizeof(*request), &command);
	mutex_unlock(&lab->operation_lock);
	if (ret)
		return ret;
	timeout = wait_for_completion_timeout(
		&lab->fullmac_assoc_started, msecs_to_jiffies(5000));
	if (!timeout)
		return -ETIMEDOUT;

	reinit_completion(&lab->fullmac_cfg_cal_done);
	WRITE_ONCE(lab->fullmac_cfg_cal_cookie, 0x4e430000 | attempt);
	mutex_lock(&lab->operation_lock);
	ret = mt7932_mcu_lab_fullmac_assoc_cal(
		lab, lab->fullmac_cfg_cal_cookie, &command, &sequence);
	mutex_unlock(&lab->operation_lock);
	if (ret)
		return ret;
	timeout = wait_for_completion_timeout(
		&lab->fullmac_cfg_cal_done, msecs_to_jiffies(5000));
	if (!timeout)
		return -ETIMEDOUT;

	mutex_lock(&lab->operation_lock);
	ret = mt7932_mcu_lab_fullmac_queue_bind(lab, &command);
	if (!ret)
		ret = mt7932_mcu_lab_fullmac_assoc_power(lab, &command);
	mutex_unlock(&lab->operation_lock);
	if (ret)
		return ret;

	timeout = wait_for_completion_timeout(
		&lab->fullmac_cfg_connect_done,
		msecs_to_jiffies(attempt ? 10000 : 3000));
	if (!timeout)
		return -ETIMEDOUT;
	return READ_ONCE(lab->fullmac_cfg_connect_status) ? -EREMOTEIO : 0;
}

static void mt7932_fullmac_cfg_connect_work(struct work_struct *work)
{
	struct mt7932_mcu_lab *lab = container_of(
		work, struct mt7932_mcu_lab, fullmac_connect_work);
	struct neo_wifid_fullmac_connect request = lab->fullmac_cfg_request;
	struct cfg80211_connect_resp_params response = {};
	struct cfg80211_bss *bss = NULL;
	enum nl80211_band band;
	struct ieee80211_channel *channel;
	unsigned int attempt;
	int frequency, ret = -EINVAL;

	for (attempt = 0; attempt < 2; attempt++) {
		ret = mt7932_fullmac_cfg_attempt(lab, &request, attempt);
		if (!ret)
			break;
	}
	WRITE_ONCE(lab->fullmac_cfg_connecting, false);
	band = request.band == 1 ? NL80211_BAND_2GHZ :
	       request.band == 2 ? NL80211_BAND_5GHZ : NL80211_BAND_6GHZ;
	frequency = ieee80211_channel_to_frequency(request.channel, band);
	channel = ieee80211_get_channel(lab->fullmac_wiphy, frequency);
	if (channel)
		bss = mt7932_fullmac_cfg_seed_bss(
			lab->fullmac_wiphy, channel,
			request.security != NEO_WIFID_FULLMAC_SECURITY_OPEN,
			request.bssid, request.ssid, request.ssid_len);
	response.status = ret ? WLAN_STATUS_UNSPECIFIED_FAILURE :
				WLAN_STATUS_SUCCESS;
	response.links[0].bssid = request.bssid;
	response.links[0].bss = bss;
	cfg80211_connect_done(lab->fullmac_ndev, &response, GFP_KERNEL);
	if (!ret) {
		WRITE_ONCE(lab->fullmac_oper_band, request.band);
		WRITE_ONCE(lab->fullmac_oper_channel, request.channel);
		cfg80211_port_authorized(lab->fullmac_ndev, request.bssid,
					 NULL, 0, GFP_KERNEL);
		dev_info(lab->dev->mt76.dev,
			 "J700_MT7932_CFG80211_CONNECT_PASS: security=%s attempts=%u port=firmware-authorized identity=redacted\n",
			 mt7932_fullmac_security_name(request.security), attempt + 1);
	}
	memzero_explicit(&request, sizeof(request));
	memzero_explicit(&lab->fullmac_cfg_request,
			 sizeof(lab->fullmac_cfg_request));
}

static int mt7932_fullmac_cfg_connect(struct wiphy *wiphy,
				      struct net_device *ndev,
				      struct cfg80211_connect_params *params)
{
	struct mt7932_mcu_lab *lab = mt7932_fullmac_wiphy_lab(wiphy);
	struct ieee80211_channel *channel = params->channel ?: params->channel_hint;
	const u8 *bssid = params->bssid ?: params->bssid_hint;
	struct neo_wifid_fullmac_connect request = {};
	bool open, owe, psk, sae;
	int ret = 0;

	open = !params->privacy && !params->crypto.n_akm_suites;
	/* cfg80211 may retain the transition-network alternatives after the
	 * selected AKM.  The first suite is the selected policy for this request.
	 */
	owe = params->crypto.n_akm_suites >= 1 &&
	      params->crypto.akm_suites[0] == WLAN_AKM_SUITE_OWE;
	psk = params->crypto.n_akm_suites >= 1 &&
	      params->crypto.akm_suites[0] == WLAN_AKM_SUITE_PSK;
	sae = params->crypto.n_akm_suites >= 1 &&
	      params->crypto.akm_suites[0] == WLAN_AKM_SUITE_SAE;
	if (psk && lab)
		dev_info(lab->dev->mt76.dev,
			 "J700_MT7932_CFG80211_PSK_INPUT: password=%u pmk=%u wpa_versions=0x%x auth_type=%u secrets=redacted\n",
			 !!params->crypto.sae_pwd, !!params->crypto.psk,
			 params->crypto.wpa_versions, params->auth_type);
	if (sae && lab)
		dev_info(lab->dev->mt76.dev,
			 "J700_MT7932_CFG80211_SAE_INPUT: password=%u pmk=%u auth_type=%u secrets=redacted\n",
			 !!params->crypto.sae_pwd, !!params->crypto.psk,
			 params->auth_type);
	if (!lab || ndev != lab->fullmac_ndev || !channel || !bssid ||
	    !params->ssid || !params->ssid_len ||
	    params->ssid_len > sizeof(request.ssid) ||
	    (!open && !owe && !psk && !sae) ||
	    (psk && (!params->crypto.psk ||
		     params->crypto.wpa_versions != NL80211_WPA_VERSION_2)) ||
	    (sae && (!params->crypto.sae_pwd ||
		     params->crypto.sae_pwd_len < 8 ||
		     params->crypto.sae_pwd_len > sizeof(request.key))) ||
	    (params->auth_type != NL80211_AUTHTYPE_AUTOMATIC &&
	     params->auth_type != NL80211_AUTHTYPE_OPEN_SYSTEM &&
	     (!sae || params->auth_type != NL80211_AUTHTYPE_SAE))) {
		if (lab)
			dev_info(lab->dev->mt76.dev,
				 "J700_MT7932_CFG80211_CONNECT_REJECT: channel=%u bssid=%u ssid_len=%zu privacy=%u akm_count=%u akm0=0x%08x wpa_versions=0x%x psk=%u sae_pwd=%u auth_type=%u secrets=redacted\n",
				 !!channel, !!bssid, params->ssid_len,
				 params->privacy, params->crypto.n_akm_suites,
				 params->crypto.n_akm_suites ?
				 params->crypto.akm_suites[0] : 0,
				 params->crypto.wpa_versions,
				 !!params->crypto.psk, !!params->crypto.sae_pwd,
				 params->auth_type);
		return -EINVAL;
	}

	request.ssid_len = params->ssid_len;
	request.band = channel->band == NL80211_BAND_2GHZ ? 1 :
		       channel->band == NL80211_BAND_5GHZ ? 2 : 3;
	request.channel = ieee80211_frequency_to_channel(channel->center_freq);
	request.security = open ? NEO_WIFID_FULLMAC_SECURITY_OPEN :
			   sae ? NEO_WIFID_FULLMAC_SECURITY_WPA3_SAE :
			   psk ? NEO_WIFID_FULLMAC_SECURITY_WPA2_PSK :
				 NEO_WIFID_FULLMAC_SECURITY_OWE;
	memcpy(request.bssid, bssid, ETH_ALEN);
	memcpy(request.ssid, params->ssid, params->ssid_len);
	if (psk) {
		if (params->crypto.sae_pwd &&
		    params->crypto.sae_pwd_len >= 8 &&
		    params->crypto.sae_pwd_len <= sizeof(request.key)) {
			request.key_len = params->crypto.sae_pwd_len;
			memcpy(request.key, params->crypto.sae_pwd,
			       request.key_len);
		}
		request.pmk_len = WLAN_PMK_LEN;
		memcpy(request.pmk, params->crypto.psk, request.pmk_len);
	} else if (sae) {
		request.key_len = params->crypto.sae_pwd_len;
		request.pmk_len = WLAN_PMK_LEN;
		memcpy(request.key, params->crypto.sae_pwd, request.key_len);
		if (params->crypto.psk) {
			memcpy(request.pmk, params->crypto.psk,
			       request.pmk_len);
		} else {
			mt7932_fullmac_derive_sae_pmk(
				params->crypto.sae_pwd,
				params->crypto.sae_pwd_len,
				params->ssid, params->ssid_len, request.pmk);
			dev_info(lab->dev->mt76.dev,
				 "J700_MT7932_CFG80211_SAE_PMK_DERIVED: method=pbkdf2-hmac-sha1 iterations=4096 bytes=32 secrets=redacted\n");
		}
	}

	mutex_lock(&lab->operation_lock);
	if (lab->fullmac_cfg_connecting || lab->dead) {
		ret = -EBUSY;
	} else {
		lab->fullmac_cfg_request = request;
		WRITE_ONCE(lab->fullmac_cfg_connecting, true);
	}
	mutex_unlock(&lab->operation_lock);
	memzero_explicit(&request, sizeof(request));
	if (ret)
		return ret;
	if (!schedule_work(&lab->fullmac_connect_work)) {
		WRITE_ONCE(lab->fullmac_cfg_connecting, false);
		memzero_explicit(&lab->fullmac_cfg_request,
				 sizeof(lab->fullmac_cfg_request));
		return -EBUSY;
	}

	return 0;
}

static int mt7932_fullmac_cfg_disconnect(struct wiphy *wiphy,
					 struct net_device *ndev,
					 u16 reason_code)
{
	struct mt7932_mcu_lab *lab = mt7932_fullmac_wiphy_lab(wiphy);
	int ret;

	if (!lab || ndev != lab->fullmac_ndev)
		return -ENODEV;
	mutex_lock(&lab->operation_lock);
	ret = mt7932_mcu_lab_disconnect_fullmac(lab, NULL);
	mutex_unlock(&lab->operation_lock);
	if (!ret)
		cfg80211_disconnected(ndev, reason_code, NULL, 0, true,
				      GFP_KERNEL);
	return ret;
}

static ssize_t mt7932_mcu_lab_write(struct file *file, const char __user *buf,
				    size_t count, loff_t *ppos)
{
	struct mt7932_mcu_lab *lab = file->private_data;
	struct neo_wifid_request request;
	struct neo_wifid_state state;
	void *payload = NULL;
	u32 command = 0, sequence = 0;
	u32 payload_len, cookie, flags;
	u16 opcode, record_type = NEO_WIFID_RECORD_SUBMIT;
	int ret;

	if (!capable(CAP_SYS_RAWIO))
		return -EPERM;
	if (count < sizeof(request))
		return -EINVAL;
	if (copy_from_user(&request, buf, sizeof(request)))
		return -EFAULT;
	payload_len = le32_to_cpu(request.payload_len);
	flags = le32_to_cpu(request.flags);
	cookie = le32_to_cpu(request.cookie);
	opcode = le16_to_cpu(request.opcode);
	if (le32_to_cpu(request.magic) != NEO_WIFID_MAGIC ||
	    le16_to_cpu(request.version) != NEO_WIFID_VERSION ||
	    le32_to_cpu(request.reserved) ||
	    flags & ~NEO_WIFID_REQUEST_FLAGS ||
	    payload_len > NEO_WIFID_MAX_REQUEST ||
	    count != sizeof(request) + payload_len)
		return -EPROTO;
	if (payload_len) {
		payload = memdup_user(buf + sizeof(request), payload_len);
		if (IS_ERR(payload))
			return PTR_ERR(payload);
	}

	mutex_lock(&lab->operation_lock);
	mt7932_mcu_lab_expire(lab);
	if (lab->dead) {
		ret = -ENODEV;
		goto result;
	}
	if (skb_queue_len(&lab->records) >=
	    NEO_WIFID_MAX_RECORDS + NEO_WIFID_MAX_PENDING &&
	    opcode != NEO_WIFID_OP_STATE) {
		ret = -ENOSPC;
		goto result;
	}
	switch (opcode) {
	case NEO_WIFID_OP_SUBMIT:
		ret = mt7932_mcu_lab_submit(lab, &request, payload,
					    payload_len, &command, &sequence);
		break;
	case NEO_WIFID_OP_RESET:
		if (payload_len || flags)
			ret = -EINVAL;
		else
			ret = mt7932_mcu_lab_reset(lab);
		break;
	case NEO_WIFID_OP_STATE:
		if (payload_len || flags) {
			ret = -EINVAL;
			break;
		}
		mt7932_mcu_lab_state(lab, &state);
		record_type = NEO_WIFID_RECORD_STATE;
		ret = 0;
		break;
	case NEO_WIFID_OP_ARM_RUNTIME_EPOCH:
		if (payload_len || flags)
			ret = -EINVAL;
		else
			ret = mt7932_mcu_lab_arm_epoch(lab);
		break;
	case NEO_WIFID_OP_RUNTIME_EPOCH:
		if (payload_len || flags)
			ret = -EINVAL;
		else
			ret = mt7932_mcu_lab_runtime_epoch(lab);
		break;
	case NEO_WIFID_OP_CONTINUE:
		if (payload_len || flags)
			ret = -EINVAL;
		else
			ret = mt7932_mcu_lab_continue(lab);
		break;
	case NEO_WIFID_OP_FULLMAC_CONNECT:
		if (flags)
			ret = -EINVAL;
		else
			ret = mt7932_mcu_lab_fullmac_connect(
				lab, payload, payload_len, &command);
		break;
	case NEO_WIFID_OP_FULLMAC_QUEUE_BIND:
		if (payload_len || flags)
			ret = -EINVAL;
		else
			ret = mt7932_mcu_lab_fullmac_queue_bind(lab, &command);
		break;
	case NEO_WIFID_OP_FULLMAC_ASSOC_CAL:
		if (payload_len || flags)
			ret = -EINVAL;
		else
			ret = mt7932_mcu_lab_fullmac_assoc_cal(
				lab, cookie, &command, &sequence);
		break;
	case NEO_WIFID_OP_FULLMAC_COLD_TEARDOWN:
		if (flags)
			ret = -EINVAL;
		else
			ret = mt7932_mcu_lab_cold_teardown(
				lab, payload, payload_len, &command);
		break;
	case NEO_WIFID_OP_FULLMAC_DISCONNECT:
		if (payload_len || flags)
			ret = -EINVAL;
		else
			ret = mt7932_mcu_lab_disconnect_fullmac(lab, &command);
		break;
	case NEO_WIFID_OP_FULLMAC_ASSOC_POWER:
		if (payload_len || flags)
			ret = -EINVAL;
		else
			ret = mt7932_mcu_lab_fullmac_assoc_power(lab, &command);
		break;
	default:
		ret = -EOPNOTSUPP;
		break;
	}

result:
	if (record_type == NEO_WIFID_RECORD_STATE && !ret)
		mt7932_mcu_lab_result(lab, record_type, cookie, 0, 0, 0,
					 &state, sizeof(state));
	else
		mt7932_mcu_lab_result(lab, record_type, cookie, ret,
					 command, sequence, NULL, 0);
	mutex_unlock(&lab->operation_lock);
	kfree_sensitive(payload);

	return count;
}

static ssize_t mt7932_mcu_lab_read(struct file *file, char __user *buf,
				   size_t count, loff_t *ppos)
{
	struct mt7932_mcu_lab *lab = file->private_data;
	struct neo_wifid_record dropped = {};
	struct sk_buff *skb;
	int lost, ret;

	for (;;) {
		mt7932_mcu_lab_expire(lab);
		lost = atomic_xchg(&lab->dropped_events, 0);
		if (lost) {
			if (count < sizeof(dropped)) {
				atomic_add(lost, &lab->dropped_events);
				return -EMSGSIZE;
			}
			dropped.magic = cpu_to_le32(NEO_WIFID_MAGIC);
			dropped.version = cpu_to_le16(NEO_WIFID_VERSION);
			dropped.type = cpu_to_le16(NEO_WIFID_RECORD_DROP);
			dropped.command = cpu_to_le32(lost);
			dropped.monotonic_ns = cpu_to_le64(ktime_get_mono_fast_ns());
			if (copy_to_user(buf, &dropped, sizeof(dropped)))
				return -EFAULT;
			return sizeof(dropped);
		}
		skb = skb_dequeue(&lab->records);
		if (skb)
			break;
		if (READ_ONCE(lab->dead))
			return 0;
		if (file->f_flags & O_NONBLOCK)
			return -EAGAIN;
		ret = wait_event_interruptible_timeout(lab->read_wait,
				READ_ONCE(lab->dead) ||
				!skb_queue_empty(&lab->records) ||
				atomic_read(&lab->dropped_events),
				msecs_to_jiffies(100));
		if (ret < 0)
			return ret;
	}
	if (count < skb->len) {
		skb_queue_head(&lab->records, skb);
		return -EMSGSIZE;
	}
	if (copy_to_user(buf, skb->data, skb->len)) {
		skb_queue_head(&lab->records, skb);
		return -EFAULT;
	}
	ret = skb->len;
	kfree_skb(skb);

	return ret;
}

static __poll_t mt7932_mcu_lab_poll(struct file *file, poll_table *wait)
{
	struct mt7932_mcu_lab *lab = file->private_data;
	__poll_t mask = 0;

	poll_wait(file, &lab->read_wait, wait);
	if (!skb_queue_empty(&lab->records) ||
	    atomic_read(&lab->dropped_events))
		mask |= EPOLLIN | EPOLLRDNORM;
	if (READ_ONCE(lab->dead))
		mask |= EPOLLHUP | EPOLLERR;
	else
		mask |= EPOLLOUT | EPOLLWRNORM;

	return mask;
}

static int mt7932_mcu_lab_open(struct inode *inode, struct file *file)
{
	struct miscdevice *misc = file->private_data;
	struct mt7932_mcu_lab *lab =
		container_of(misc, struct mt7932_mcu_lab, misc);

	if (!capable(CAP_SYS_RAWIO))
		return -EPERM;
	if (atomic_cmpxchg(&lab->opened, 0, 1))
		return -EBUSY;
	if (!kref_get_unless_zero(&lab->ref)) {
		atomic_set(&lab->opened, 0);
		return -ENODEV;
	}
	if (READ_ONCE(lab->dead)) {
		atomic_set(&lab->opened, 0);
		mt7932_mcu_lab_put(lab);
		return -ENODEV;
	}
	file->private_data = lab;
	complete_all(&lab->connected);

	return nonseekable_open(inode, file);
}

static int mt7932_mcu_lab_file_release(struct inode *inode, struct file *file)
{
	struct mt7932_mcu_lab *lab = file->private_data;

	atomic_set(&lab->opened, 0);
	mutex_lock(&lab->operation_lock);
	if (lab->checkpoint_waiting &&
	    lab->phase != NEO_WIFID_PHASE_CONTINUED && !lab->dead) {
		lab->phase = NEO_WIFID_PHASE_ERROR;
		complete_all(&lab->continued);
	}
	mutex_unlock(&lab->operation_lock);
	wake_up_interruptible(&lab->read_wait);
	mt7932_mcu_lab_put(lab);

	return 0;
}

static const struct file_operations mt7932_mcu_lab_fops = {
	.owner = THIS_MODULE,
	.open = mt7932_mcu_lab_open,
	.release = mt7932_mcu_lab_file_release,
	.read = mt7932_mcu_lab_read,
	.write = mt7932_mcu_lab_write,
	.poll = mt7932_mcu_lab_poll,
};

static int mt7932_mcu_lab_create(struct mt792x_dev *dev,
				 struct mt7932_mcu_lab **result,
				 bool register_misc)
{
	struct mt7932_mcu_lab *lab;
	int ret;

	lab = kzalloc_obj(*lab);
	if (!lab)
		return -ENOMEM;
	kref_init(&lab->ref);
	lab->dev = dev;
	mutex_init(&lab->operation_lock);
	spin_lock_init(&lab->pending_lock);
	skb_queue_head_init(&lab->records);
	init_waitqueue_head(&lab->read_wait);
	init_completion(&lab->connected);
	init_completion(&lab->continued);
	init_completion(&lab->disconnected);
	init_completion(&lab->fullmac_assoc_started);
	init_completion(&lab->fullmac_cfg_connect_done);
	init_completion(&lab->fullmac_cfg_cal_done);
	init_completion(&lab->fullmac_hw_scan_done);
	INIT_WORK(&lab->fullmac_connect_work, mt7932_fullmac_cfg_connect_work);
	INIT_WORK(&lab->fullmac_scan_work, mt7932_fullmac_cfg_scan_work);
	atomic_set(&lab->opened, 0);
	atomic_set(&lab->dropped_events, 0);
	atomic_set(&lab->fullmac_scan_bss_count, 0);
	lab->phase = NEO_WIFID_PHASE_CHECKPOINT;
	ret = xa_insert(&mt7932_mcu_labs, (unsigned long)dev, lab,
			GFP_KERNEL);
	if (ret)
		goto free;
	if (register_misc) {
		lab->misc.minor = MISC_DYNAMIC_MINOR;
		lab->misc.name = kasprintf(GFP_KERNEL, "neo-wifi-mcu-%s",
					   dev_name(dev->mt76.dev));
		if (!lab->misc.name) {
			ret = -ENOMEM;
			goto erase;
		}
		lab->misc.fops = &mt7932_mcu_lab_fops;
		lab->misc.parent = dev->mt76.dev;
		lab->misc.mode = 0600;
		ret = misc_register(&lab->misc);
		if (ret)
			goto erase;
		lab->misc_registered = true;
	}
	*result = lab;

	return 0;

erase:
	xa_erase(&mt7932_mcu_labs, (unsigned long)dev);
	synchronize_rcu();
free:
	mt7932_mcu_lab_put(lab);
	return ret;
}

int mt7932_mcu_lab_checkpoint(struct mt792x_dev *dev)
{
	struct mt7932_mcu_lab *lab;
	unsigned long deadline, remaining, timeout;
	int ret;

	if (!mt7932_mcu_lab_conformance)
		return 0;
	if (!is_mt7932(&dev->mt76))
		return -ENODEV;
	lab = mt7932_mcu_lab_get(dev);
	if (!lab) {
		ret = mt7932_mcu_lab_create(dev, &lab, true);
		if (ret)
			return ret;
		/* create returned the registry/base reference; take a local one. */
		kref_get(&lab->ref);
	}
	if (mt7932_replay_au_policy) {
		ret = -EINVAL;
		goto fail;
	}

	mutex_lock(&lab->operation_lock);
	lab->checkpoint_waiting = true;
	mutex_unlock(&lab->operation_lock);
	timeout = msecs_to_jiffies(clamp_t(unsigned int,
					 mt7932_mcu_lab_timeout_ms,
					 1000, 600000));
	deadline = jiffies + timeout;
	if (!wait_for_completion_timeout(&lab->connected, timeout)) {
		ret = -ETIMEDOUT;
		goto fail;
	}
	remaining = time_is_after_jiffies(deadline) ? deadline - jiffies : 0;
	if (!remaining ||
	    !wait_for_completion_timeout(&lab->continued, remaining)) {
		ret = -ETIMEDOUT;
		goto fail;
	}
	if (READ_ONCE(lab->dead) ||
	    READ_ONCE(lab->phase) != NEO_WIFID_PHASE_CONTINUED) {
		ret = -ECANCELED;
		goto fail;
	}

	mutex_lock(&lab->operation_lock);
	lab->checkpoint_waiting = false;
	mutex_unlock(&lab->operation_lock);
	mt7932_mcu_lab_put(lab);

	return 0;

fail:
	mutex_lock(&lab->operation_lock);
	mt7932_mcu_lab_abort_epoch(lab, "neo-wifid-checkpoint-fail");
	lab->checkpoint_waiting = false;
	lab->phase = NEO_WIFID_PHASE_ERROR;
	mutex_unlock(&lab->operation_lock);
	mt7932_mcu_lab_put(lab);

	return ret;
}

int mt7932_mcu_lab_runtime_create(struct mt792x_dev *dev)
{
	struct mt7932_mcu_lab *lab;
	int ret;

	if (!mt7932_mcu_lab_runtime && !mt7932_fullmac_cfg80211)
		return 0;
	if (!is_mt7932(&dev->mt76) || !dev->hw_init_done)
		return -ENODEV;
	if (mt7932_mcu_lab_conformance)
		return -EINVAL;

	lab = mt7932_mcu_lab_get(dev);
	if (!lab) {
		ret = mt7932_mcu_lab_create(
			dev, &lab, mt7932_mcu_lab_runtime);
		if (ret)
			return ret;
		kref_get(&lab->ref);
	}
	mutex_lock(&lab->operation_lock);
	if (lab->dead) {
		ret = -ENODEV;
	} else if (lab->checkpoint_waiting ||
		   lab->phase != NEO_WIFID_PHASE_CHECKPOINT) {
		ret = -EBUSY;
	} else {
		lab->phase = NEO_WIFID_PHASE_CONTINUED;
		ret = 0;
	}
	mutex_unlock(&lab->operation_lock);
	if (!ret)
		ret = mt7932_mcu_lab_register_fullmac_netdev(lab);
	if (!ret && mt7932_fullmac_cfg80211)
		ret = mt7932_mcu_lab_enable_internal_scan_vif(lab);
	if (!ret)
		dev_info(dev->mt76.dev,
			 "J700_MT7932_MCU_LAB_RUNTIME_PASS: phase=continued netdev=%s cfg80211=%u credentials=never-logged\n",
			 lab->fullmac_ndev->name, mt7932_fullmac_cfg80211);
	mt7932_mcu_lab_put(lab);

	return ret;
}

void mt7932_mcu_lab_destroy(struct mt792x_dev *dev)
{
	struct mt7932_mcu_lab *lab;
	bool scan_owned;

	lab = xa_erase(&mt7932_mcu_labs, (unsigned long)dev);
	if (!lab)
		return;
	synchronize_rcu();
	/* unregister_netdev() runs ndo_stop before cfg80211's NETDEV_DOWN
	 * notifier retires its scan request.  ndo_stop synchronously removes our
	 * copy and drains the worker, so destroy must never report scan_done()
	 * against a request cfg80211 may already have freed.
	 */
	if (lab->fullmac_ndev)
		unregister_netdev(lab->fullmac_ndev);
	xchg(&lab->fullmac_scan_request, NULL);
	scan_owned = xchg(&lab->fullmac_hw_scan_owned, false);
	WRITE_ONCE(lab->fullmac_hw_scan_observing, false);
	WRITE_ONCE(lab->fullmac_hw_scan_aborted, true);
	if (scan_owned && lab->fullmac_vif)
		mt7932_fullmac_scan_cancel(lab);
	complete_all(&lab->fullmac_hw_scan_done);
	complete_all(&lab->fullmac_assoc_started);
	complete_all(&lab->fullmac_cfg_cal_done);
	complete_all(&lab->fullmac_cfg_connect_done);
	cancel_work_sync(&lab->fullmac_connect_work);
	cancel_work_sync(&lab->fullmac_scan_work);
	if (lab->misc_registered)
		misc_deregister(&lab->misc);
	if (lab->fullmac_wiphy)
		wiphy_unregister(lab->fullmac_wiphy);
	mutex_lock(&lab->operation_lock);
	mt7932_mcu_lab_abort_epoch(lab, "neo-wifid-destroy");
	mt7932_mcu_lab_release_fullmac_resources(lab);
	lab->dead = true;
	complete_all(&lab->connected);
	complete_all(&lab->continued);
	mutex_unlock(&lab->operation_lock);
	mt7932_mcu_lab_remove_internal_scan_vif(lab);
	wake_up_interruptible(&lab->read_wait);
	mt7932_mcu_lab_put(lab);
}
EXPORT_SYMBOL_GPL(mt7932_mcu_lab_destroy);

bool mt7932_mcu_lab_scan_done(struct mt792x_dev *dev, bool aborted)
{
	struct mt7932_mcu_lab *lab;
	bool owned;

	lab = mt7932_mcu_lab_get(dev);
	if (!lab)
		return false;
	owned = READ_ONCE(lab->fullmac_hw_scan_owned);
	if (owned) {
		WRITE_ONCE(lab->fullmac_hw_scan_aborted, aborted);
		WRITE_ONCE(lab->fullmac_hw_scan_owned, false);
		complete(&lab->fullmac_hw_scan_done);
	}
	mt7932_mcu_lab_put(lab);

	return owned;
}

bool mt7932_mcu_lab_rx(struct mt792x_dev *dev, struct sk_buff *skb)
{
	struct mt76_connac2_mcu_rxd *rxd;
	struct mt7932_mcu_lab_pending pending = {};
	struct mt7932_mcu_lab *lab;
	unsigned long flags;
	bool matched = false;
	u8 sequence;

	lab = mt7932_mcu_lab_get(dev);
	if (!lab)
		return false;
	if (unlikely(skb->len < sizeof(*rxd)))
		goto out;
	rxd = (void *)skb->data;
	/* J700 hardware delivers Apple's fullmac notification envelope as EID
	 * 0xee.  The Apple selector (0x40..0x5e) is the u32 at TLV+4; 0xea is
	 * the command-side layer-1 CID and must not be reused as the RX EID.
	 */
	if (rxd->eid == 0xee &&
	    skb->len >= sizeof(*rxd) + 16) {
		struct mt76_queue *q = &dev->mt76.q_rx[0];
		u32 subevent = get_unaligned_le32(rxd->tlv + 4);
		u16 status = (subevent == 0x40 || subevent == 0x42) ?
			get_unaligned_le16(rxd->tlv + 12) : 0xffff;
		size_t body_len = skb->len - sizeof(*rxd) - 12;
		const u8 *body = rxd->tlv + 12;

		if (subevent == 0x4d && body_len >= 0x14) {
			u32 band = get_unaligned_le32(body + 0x0c);
			bool valid = band >= 1 && band <= 3 && body[0x11];

			spin_lock_irqsave(&lab->pending_lock, flags);
			lab->assoc_cal.primary = body[0x11];
			lab->assoc_cal.cbw = body[0x10];
			lab->assoc_cal.band = band;
			lab->assoc_cal.extension = body[0x13];
			lab->assoc_cal.valid = valid;
			spin_unlock_irqrestore(&lab->pending_lock, flags);
			dev_info(dev->mt76.dev,
				 "J700_MT7932_FULLMAC_ASSOC_CAL_SNAPSHOT: valid=%u band=%u primary=%u cbw=%u extension=%u source=selector0x4d\n",
				 valid, band, body[0x11],
				 body[0x10], body[0x13]);
			if (READ_ONCE(lab->fullmac_cfg_connecting))
				complete(&lab->fullmac_assoc_started);
		}
		if (subevent == 0x40 && body_len >= 0x34) {
			struct mt792x_link_sta *mlink = &lab->fullmac_sta.deflink;
			struct mt792x_vif *mvif = lab->fullmac_vif ?
				(struct mt792x_vif *)lab->fullmac_vif->drv_priv : NULL;
			u8 bss_idx = body[0x10];
			u8 bmc_wlan = body[0x11];
			u8 sta_idx = body[0x14];
			u8 peer_wlan = body[0x15];
			bool peer_reserved = false;
			int i;

			dev_info(dev->mt76.dev,
				 "J700_MT7932_FULLMAC_ASSOC_COMPLETE: status=%u band=%u channel=%u pairwise=0x%08x bytes=%zu identity=redacted\n",
				 status, body[0xb], body[0xc],
				 get_unaligned_le32(body + 0x30), body_len);
			if (status) {
				WRITE_ONCE(lab->fullmac_peer_ready, false);
				dev_info(dev->mt76.dev,
					 "J700_MT7932_FULLMAC_CANDIDATE_REJECTED: status=%u source=selector0x40\n",
					 status);
			} else {
				for (i = 0; i < NEO_WIFID_FULLMAC_WTBL_IDS; i++)
					peer_reserved |=
						peer_wlan == lab->fullmac_wtbl_ids[i];
			}
			if (!status && mvif &&
			    bss_idx == mvif->bss_conf.mt76.idx &&
			    bmc_wlan == lab->fullmac_wtbl_ids[0] &&
			    sta_idx == lab->fullmac_sta_ids[0] &&
			    peer_reserved) {
				WRITE_ONCE(mlink->wcid.idx, peer_wlan);
				WRITE_ONCE(lab->fullmac_peer_ready, true);
				dev_info(dev->mt76.dev,
					 "J700_MT7932_FULLMAC_PEER_ADOPT_PASS: bss=%u sta=%u bmc_wlan=%u peer_wlan=%u source=selector0x40\n",
					 bss_idx, sta_idx, bmc_wlan,
					 peer_wlan);
				dev_info(dev->mt76.dev,
					 "J700_MT7932_FULLMAC_WTBL_SAFE: stage=selector0x40 peer=%u dw7=0x%08x dw20=0x%08x dw30=0x%08x\n",
					 peer_wlan,
					 mt76_rr(dev, MT_WTBL_LMAC_OFFS(peer_wlan, 7)),
					 mt76_rr(dev, MT_WTBL_LMAC_OFFS(peer_wlan, 20)),
					 mt76_rr(dev, MT_WTBL_LMAC_OFFS(peer_wlan, 30)));
			} else if (!status) {
				dev_err(dev->mt76.dev,
					"J700_MT7932_FULLMAC_PEER_ADOPT_FAIL: bss=%u sta=%u bmc_wlan=%u peer_wlan=%u reserved=%u\n",
					bss_idx, sta_idx, bmc_wlan,
					peer_wlan, peer_reserved);
			}
		} else if (subevent == 0x5a && body_len >= 0x58)
			dev_info(dev->mt76.dev,
				 "J700_MT7932_FULLMAC_AUTH_RECEIVED: status=%u reason=%u pmk_len=%u pmkid_len=%u bytes=%zu secrets=redacted\n",
				 get_unaligned_le32(body),
				 get_unaligned_le32(body + 4),
				 get_unaligned_le32(body + 0x10),
				 get_unaligned_le32(body + 0x54), body_len);
		else if (subevent == 0x5b && body_len >= 8)
			dev_info(dev->mt76.dev,
				 "J700_MT7932_FULLMAC_ASSOC_RECEIVED: status=%u reason=%u bytes=%zu\n",
				 get_unaligned_le32(body),
				 get_unaligned_le32(body + 4), body_len);

		if (subevent == 0x44 &&
		    READ_ONCE(lab->fullmac_disconnect_pending)) {
			/* Apple 25G83 completes rDisconnectComp from the firmware
			 * disconnect-event selector 0x44.  Selector 0x42 is the
			 * separate connect-complete notification.
			 */
			WRITE_ONCE(lab->fullmac_disconnect_status, 0);
			WRITE_ONCE(lab->fullmac_disconnect_pending, false);
			WRITE_ONCE(lab->fullmac_connected, false);
			if (lab->fullmac_ndev) {
				netif_carrier_off(lab->fullmac_ndev);
				netif_tx_disable(lab->fullmac_ndev);
			}
			complete(&lab->disconnected);
			dev_info(dev->mt76.dev,
				 "J700_MT7932_FULLMAC_DISCONNECT_COMPLETE: status=0 source=eid0xee-selector0x44\n");
		} else if (subevent == 0x42 && lab->fullmac_ndev) {
			if (!status && READ_ONCE(lab->fullmac_peer_ready)) {
				bool was_offchannel = READ_ONCE(dev->mphy.offchannel);

				/* The firmware-owned connect scan has returned to its
				 * associated channel at connect-complete.  Unlike the
				 * ordinary mac80211 scan path, the lab fullmac path has
				 * no host scan callback left to clear this software-only
				 * scheduling gate.  Leaving it set strands every normal
				 * data skb in wcid->tx_pending before WFDMA.
				 */
				WRITE_ONCE(dev->mphy.offchannel, false);
				WRITE_ONCE(lab->fullmac_connected, true);
				netif_carrier_on(lab->fullmac_ndev);
				netif_wake_queue(lab->fullmac_ndev);
				mt76_worker_schedule(&dev->mt76.tx_worker);
				dev_info(dev->mt76.dev,
					 "J700_MT7932_FULLMAC_HOST_CHANNEL_GATE: offchannel=%u->0 source=connect-complete\n",
					 was_offchannel);
				dev_info(dev->mt76.dev,
					 "J700_MT7932_FULLMAC_WTBL_SAFE: stage=connect-complete peer=%u dw7=0x%08x dw20=0x%08x dw30=0x%08x\n",
					 lab->fullmac_sta.deflink.wcid.idx,
					 mt76_rr(dev, MT_WTBL_LMAC_OFFS(
						lab->fullmac_sta.deflink.wcid.idx, 7)),
					 mt76_rr(dev, MT_WTBL_LMAC_OFFS(
						lab->fullmac_sta.deflink.wcid.idx, 20)),
					 mt76_rr(dev, MT_WTBL_LMAC_OFFS(
						lab->fullmac_sta.deflink.wcid.idx, 30)));
			} else {
				WRITE_ONCE(lab->fullmac_connected, false);
				netif_carrier_off(lab->fullmac_ndev);
				netif_tx_disable(lab->fullmac_ndev);
			}
			if (READ_ONCE(lab->fullmac_cfg_connecting)) {
				WRITE_ONCE(lab->fullmac_cfg_connect_status, status);
				complete(&lab->fullmac_cfg_connect_done);
			}
		}

		if (status == 0xffff)
			dev_info(dev->mt76.dev,
				 "J700_MT7932_FULLMAC_EVENT: eid=0xee subevent=0x%02x status=na q0_cidx=%u q0_didx=%u q0_head=%u q0_tail=%u q0_queued=%u\n",
				 subevent, readl(&q->regs->cpu_idx),
				 readl(&q->regs->dma_idx), q->head, q->tail,
				 READ_ONCE(q->queued));
		else
			dev_info(dev->mt76.dev,
				 "J700_MT7932_FULLMAC_EVENT: eid=0xee subevent=0x%02x status=%u q0_cidx=%u q0_didx=%u q0_head=%u q0_tail=%u q0_queued=%u\n",
				 subevent, status, readl(&q->regs->cpu_idx),
				 readl(&q->regs->dma_idx), q->head, q->tail,
				 READ_ONCE(q->queued));
	}
	sequence = rxd->seq;
	if (sequence) {
		spin_lock_irqsave(&lab->pending_lock, flags);
		if (lab->pending[sequence].owned &&
		    (!lab->pending[sequence].expected_eid ||
		     lab->pending[sequence].expected_eid == rxd->eid)) {
			pending = lab->pending[sequence];
			lab->pending[sequence].valid = false;
			lab->pending[sequence].owned = false;
			if (pending.valid)
				lab->pending_count--;
			matched = true;
		}
		spin_unlock_irqrestore(&lab->pending_lock, flags);
	}
	if (matched) {
		struct sk_buff *record;

		if (pending.cookie == READ_ONCE(lab->fullmac_cfg_cal_cookie))
			complete(&lab->fullmac_cfg_cal_done);

		record = mt7932_mcu_lab_record_alloc(
			NEO_WIFID_RECORD_RESPONSE, pending.cookie, 0, 0,
			sequence, skb->data, skb->len, GFP_ATOMIC);
		mt7932_mcu_lab_queue(lab, record);
		kfree_skb(skb);
	} else if (atomic_read(&lab->opened)) {
		struct sk_buff *record;

		/* Observation is a clone-by-value.  The original still follows the
		 * normal mt7921 dispatcher and therefore retains scan/BSS semantics.
		 */
		record = mt7932_mcu_lab_record_alloc(
			NEO_WIFID_RECORD_EVENT, 0, 0, 0, sequence,
			skb->data, skb->len, GFP_ATOMIC);
		mt7932_mcu_lab_queue(lab, record);
	}
out:
	mt7932_mcu_lab_put(lab);

	return matched;
}

bool mt7932_mcu_lab_fullmac_rx(struct mt792x_dev *dev, struct sk_buff *skb)
{
	struct mt76_rx_status *status = (void *)skb->cb;
	struct mt7932_mcu_lab *lab;
	struct net_device *ndev;
	unsigned int len;
	bool consumed = false;

	lab = mt7932_mcu_lab_get(dev);
	if (!lab)
		return false;
	if (!lab->fullmac_wiphy)
		goto out;
	if (!(status->flag & RX_FLAG_8023)) {
		struct ieee80211_mgmt *mgmt = (void *)skb->data;
		struct ieee80211_channel *channel;
		struct cfg80211_bss *bss;

		if ((READ_ONCE(lab->fullmac_hw_scan_owned) ||
		     READ_ONCE(lab->fullmac_hw_scan_observing)) &&
		    likely(skb->len >= offsetof(struct ieee80211_mgmt,
						u.probe_resp.variable)) &&
		    (ieee80211_is_beacon(mgmt->frame_control) ||
		     ieee80211_is_probe_resp(mgmt->frame_control))) {
			channel = ieee80211_get_channel(lab->fullmac_wiphy,
						      status->freq);
			if (channel) {
				bss = cfg80211_inform_bss_frame(
					lab->fullmac_wiphy, channel, mgmt, skb->len,
					status->signal * 100, GFP_ATOMIC);
				if (bss) {
					cfg80211_put_bss(lab->fullmac_wiphy, bss);
					atomic_inc(&lab->fullmac_scan_bss_count);
				}
			}
		}
		dev_kfree_skb(skb);
		consumed = true;
		goto out;
	}
	ndev = READ_ONCE(lab->fullmac_ndev);
	if (!READ_ONCE(lab->fullmac_connected) || !ndev ||
	    unlikely(!pskb_may_pull(skb, ETH_HLEN))) {
		dev_kfree_skb(skb);
		consumed = true;
		goto out;
	}

	len = skb->len;
	memset(skb->cb, 0, sizeof(skb->cb));
	skb->protocol = eth_type_trans(skb, ndev);
	skb_reset_network_header(skb);
	ndev->stats.rx_packets++;
	ndev->stats.rx_bytes += len;
	netif_receive_skb(skb);
	consumed = true;
out:
	mt7932_mcu_lab_put(lab);
	return consumed;
}
EXPORT_SYMBOL_GPL(mt7932_mcu_lab_fullmac_rx);
