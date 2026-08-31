// SPDX-License-Identifier: BSD-3-Clause-Clear
/* J700-only MT7932 MCU conformance checkpoint. */

#include <linux/capability.h>
#include <linux/fs.h>
#include <linux/kref.h>
#include <linux/miscdevice.h>
#include <linux/netdevice.h>
#include <linux/pci.h>
#include <linux/poll.h>
#include <linux/rcupdate.h>
#include <linux/unaligned.h>
#include <linux/uaccess.h>
#include <linux/xarray.h>

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

struct mt7932_mcu_lab_pending {
	bool valid;
	bool owned;
	u32 cookie;
	unsigned long expires;
};

struct mt7932_mcu_lab_skb_cb {
	u16 type;
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
	atomic_t opened;
	atomic_t dropped_events;
	enum neo_wifid_phase phase;
	u32 command_count;
	u8 fullmac_sta_ids[NEO_WIFID_FULLMAC_STA_IDS];
	u8 fullmac_wtbl_ids[NEO_WIFID_FULLMAC_WTBL_IDS];
	u8 fullmac_reserved_count;
	struct mt792x_sta fullmac_sta;
	struct ieee80211_vif *fullmac_vif;
	struct ieee80211_key_conf fullmac_key;
	struct net_device *fullmac_ndev;
	bool fullmac_wcid_initialized;
	bool fullmac_connected;
	bool checkpoint_waiting;
	bool response_timed_out;
	bool irq_disabled;
	bool tasklet_disabled;
	bool dead;
};

struct mt7932_fullmac_netdev_priv {
	struct mt7932_mcu_lab *lab;
};

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
	info->control.hw_key = &lab->fullmac_key;
	skb_set_queue_mapping(skb, IEEE80211_AC_BE);
	mt76_tx(&lab->dev->mphy, NULL, wcid, skb);

	ndev->stats.tx_packets++;
	ndev->stats.tx_bytes += len;
	return NETDEV_TX_OK;
}

static const struct net_device_ops mt7932_mcu_lab_fullmac_netdev_ops = {
	.ndo_start_xmit = mt7932_mcu_lab_fullmac_xmit,
};

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
	strscpy(ndev->name, "neo-wifi%d", IFNAMSIZ);
	SET_NETDEV_DEV(ndev, lab->dev->mt76.dev);
	eth_hw_addr_set(ndev, lab->dev->mphy.macaddr);
	netif_carrier_off(ndev);
	ret = register_netdev(ndev);
	if (ret) {
		free_netdev(ndev);
		return ret;
	}
	lab->fullmac_ndev = ndev;
	return 0;
}

static int mt7932_mcu_lab_prepare_fullmac_data_path(
	struct mt7932_mcu_lab *lab)
{
	struct mt792x_link_sta *mlink = &lab->fullmac_sta.deflink;
	struct mt792x_dev *dev = lab->dev;
	struct mt792x_vif *mvif;
	int i;

	lab->fullmac_vif = NULL;
	ieee80211_iterate_active_interfaces_atomic(
		mt76_hw(dev), IEEE80211_IFACE_ITER_NORMAL,
		mt7932_mcu_lab_select_vif, lab);
	if (!lab->fullmac_vif || !lab->fullmac_ndev)
		return -ENODEV;

	mvif = (struct mt792x_vif *)lab->fullmac_vif->drv_priv;
	memset(&lab->fullmac_sta, 0, sizeof(lab->fullmac_sta));
	mt76_wcid_init(&mlink->wcid, 0);
	mlink->wcid.idx = lab->fullmac_wtbl_ids[0];
	mlink->wcid.hw_key_idx = 0;
	mlink->wcid.tx_info |= MT_WCID_TX_INFO_SET;
	mlink->last_txs = jiffies;
	mlink->sta = &lab->fullmac_sta;
	lab->fullmac_sta.vif = mvif;
	lab->fullmac_wcid_initialized = true;

	memset(&lab->fullmac_key, 0, sizeof(lab->fullmac_key));
	lab->fullmac_key.cipher = WLAN_CIPHER_SUITE_CCMP;
	lab->fullmac_key.keyidx = 0;

	mutex_lock(&dev->mt76.mutex);
	for (i = 0; i < NEO_WIFID_FULLMAC_WTBL_IDS; i++)
		rcu_assign_pointer(dev->mt76.wcid[lab->fullmac_wtbl_ids[i]],
				   &mlink->wcid);
	mutex_unlock(&dev->mt76.mutex);
	dev_info(dev->mt76.dev,
		 "J700_MT7932_FULLMAC_DATA_PATH_READY: netdev=%s wtbl_count=4 tx=802.3 rx=802.3 ac=BE amsdu=off identity=redacted\n",
		 lab->fullmac_ndev->name);

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

bool mt7932_mcu_lab_conformance_active(void)
{
	return READ_ONCE(mt7932_mcu_lab_conformance);
}

static void mt7932_mcu_lab_release(struct kref *ref)
{
	struct mt7932_mcu_lab *lab =
		container_of(ref, struct mt7932_mcu_lab, ref);

	skb_queue_purge(&lab->records);
	if (lab->fullmac_ndev)
		free_netdev(lab->fullmac_ndev);
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
	if (flags & NEO_WIFID_F_WIRE_ACK) {
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
	mt792x_reset(&dev->mt76);
	flush_work(&dev->reset_work);
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

static int mt7932_mcu_lab_fullmac_connect(
	struct mt7932_mcu_lab *lab,
	const struct neo_wifid_fullmac_connect *request,
	u32 payload_len, u32 *command)
{
	struct mt7932_fullmac_connect_params params = {};
	int ret;

	BUILD_BUG_ON(sizeof(*request) != 176);
	if (payload_len != sizeof(*request) ||
	    memchr_inv(request->reserved0, 0, sizeof(request->reserved0)) ||
	    memchr_inv(request->reserved1, 0, sizeof(request->reserved1)) ||
	    !request->ssid_len || request->ssid_len > sizeof(request->ssid) ||
	    request->key_len > sizeof(request->key) ||
	    !request->pmk_len || request->pmk_len > sizeof(request->pmk) ||
	    request->band < 1 || request->band > 3 || !request->channel ||
	    is_zero_ether_addr(request->bssid))
		return -EINVAL;
	if (lab->phase != NEO_WIFID_PHASE_CONTINUED ||
	    lab->checkpoint_waiting || !lab->dev->hw_init_done)
		return -EBUSY;

	ret = mt7932_mcu_lab_reserve_fullmac_resources(lab);
	if (ret)
		return ret;
	ret = mt7932_mcu_lab_prepare_fullmac_data_path(lab);
	if (ret) {
		mt7932_mcu_lab_release_fullmac_resources(lab);
		return ret;
	}
	params.ssid = request->ssid;
	params.key = request->key;
	params.pmk = request->pmk;
	params.bssid = request->bssid;
	params.ssid_len = request->ssid_len;
	params.key_len = request->key_len;
	params.pmk_len = request->pmk_len;
	params.band = request->band;
	params.channel = request->channel;
	memcpy(params.sta_ids, lab->fullmac_sta_ids, sizeof(params.sta_ids));
	memcpy(params.wtbl_ids, lab->fullmac_wtbl_ids, sizeof(params.wtbl_ids));
	ret = mt7932_mcu_fullmac_connect(lab->dev, &params, command);
	memzero_explicit(&params, sizeof(params));
	if (ret)
		mt7932_mcu_lab_release_fullmac_resources(lab);
	else
		lab->command_count++;

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
				 struct mt7932_mcu_lab **result)
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
	atomic_set(&lab->opened, 0);
	atomic_set(&lab->dropped_events, 0);
	lab->phase = NEO_WIFID_PHASE_CHECKPOINT;
	lab->misc.minor = MISC_DYNAMIC_MINOR;
	lab->misc.name = kasprintf(GFP_KERNEL, "neo-wifi-mcu-%s",
				   dev_name(dev->mt76.dev));
	if (!lab->misc.name) {
		ret = -ENOMEM;
		goto free;
	}
	lab->misc.fops = &mt7932_mcu_lab_fops;
	lab->misc.parent = dev->mt76.dev;
	lab->misc.mode = 0600;
	ret = xa_insert(&mt7932_mcu_labs, (unsigned long)dev, lab,
			GFP_KERNEL);
	if (ret)
		goto free;
	ret = misc_register(&lab->misc);
	if (ret)
		goto erase;
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
		ret = mt7932_mcu_lab_create(dev, &lab);
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

	if (!mt7932_mcu_lab_runtime)
		return 0;
	if (!is_mt7932(&dev->mt76) || !dev->hw_init_done)
		return -ENODEV;
	if (mt7932_mcu_lab_conformance)
		return -EINVAL;

	lab = mt7932_mcu_lab_get(dev);
	if (!lab) {
		ret = mt7932_mcu_lab_create(dev, &lab);
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
	if (!ret)
		dev_info(dev->mt76.dev,
			 "J700_MT7932_MCU_LAB_RUNTIME_PASS: phase=continued netdev=%s credentials=never-logged\n",
			 lab->fullmac_ndev->name);
	mt7932_mcu_lab_put(lab);

	return ret;
}

void mt7932_mcu_lab_destroy(struct mt792x_dev *dev)
{
	struct mt7932_mcu_lab *lab;

	lab = xa_erase(&mt7932_mcu_labs, (unsigned long)dev);
	if (!lab)
		return;
	synchronize_rcu();
	misc_deregister(&lab->misc);
	if (lab->fullmac_ndev)
		unregister_netdev(lab->fullmac_ndev);
	mutex_lock(&lab->operation_lock);
	mt7932_mcu_lab_abort_epoch(lab, "neo-wifid-destroy");
	mt7932_mcu_lab_release_fullmac_resources(lab);
	lab->dead = true;
	complete_all(&lab->connected);
	complete_all(&lab->continued);
	mutex_unlock(&lab->operation_lock);
	wake_up_interruptible(&lab->read_wait);
	mt7932_mcu_lab_put(lab);
}
EXPORT_SYMBOL_GPL(mt7932_mcu_lab_destroy);

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
	if (rxd->eid == 0xea &&
	    skb->len >= sizeof(*rxd) + 16) {
		struct mt76_queue *q = &dev->mt76.q_rx[0];
		u32 subevent = get_unaligned_le32(rxd->tlv + 4);
		u16 status = subevent == 0x42 ?
			get_unaligned_le16(rxd->tlv + 12) : 0xffff;
		size_t body_len = skb->len - sizeof(*rxd) - 12;
		const u8 *body = rxd->tlv + 12;

		if (subevent == 0x40 && body_len >= 0x34)
			dev_info(dev->mt76.dev,
				 "J700_MT7932_FULLMAC_ASSOC_COMPLETE: status=%u band=%u channel=%u pairwise=0x%08x bytes=%zu identity=redacted\n",
				 get_unaligned_le16(body), body[0xb], body[0xc],
				 get_unaligned_le32(body + 0x30), body_len);
		else if (subevent == 0x5a && body_len >= 0x58)
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

		if (subevent == 0x42 && lab->fullmac_ndev) {
			if (!status) {
				WRITE_ONCE(lab->fullmac_connected, true);
				netif_carrier_on(lab->fullmac_ndev);
				netif_wake_queue(lab->fullmac_ndev);
			} else {
				WRITE_ONCE(lab->fullmac_connected, false);
				netif_carrier_off(lab->fullmac_ndev);
				netif_tx_disable(lab->fullmac_ndev);
			}
		}

		if (status == 0xffff)
			dev_info(dev->mt76.dev,
				 "J700_MT7932_FULLMAC_EVENT: eid=0xea subevent=0x%02x status=na q0_cidx=%u q0_didx=%u q0_head=%u q0_tail=%u q0_queued=%u\n",
				 subevent, readl(&q->regs->cpu_idx),
				 readl(&q->regs->dma_idx), q->head, q->tail,
				 READ_ONCE(q->queued));
		else
			dev_info(dev->mt76.dev,
				 "J700_MT7932_FULLMAC_EVENT: eid=0xea subevent=0x%02x status=%u q0_cidx=%u q0_didx=%u q0_head=%u q0_tail=%u q0_queued=%u\n",
				 subevent, status, readl(&q->regs->cpu_idx),
				 readl(&q->regs->dma_idx), q->head, q->tail,
				 READ_ONCE(q->queued));
	}
	sequence = rxd->seq;
	if (sequence) {
		spin_lock_irqsave(&lab->pending_lock, flags);
		if (lab->pending[sequence].owned) {
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

	if (!(status->flag & RX_FLAG_8023))
		return false;
	lab = mt7932_mcu_lab_get(dev);
	if (!lab)
		return false;
	ndev = READ_ONCE(lab->fullmac_ndev);
	if (!READ_ONCE(lab->fullmac_connected) || !ndev ||
	    unlikely(!pskb_may_pull(skb, ETH_HLEN)))
		goto out;

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
