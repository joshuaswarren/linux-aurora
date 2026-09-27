// SPDX-License-Identifier: GPL-2.0-only
#include "mt7932.h"

static const char mt_stat_names[][ETH_GSTRING_LEN] = {
	"irq_services", "rx_descriptors", "rx_refill_doorbells",
	"rx_ethernet", "rx_parse_errors", "rx_peer_or_duplicate_drops",
	"rx_amsdu_subframes", "rx_unicast_no_group3",
	"tx_doorbells", "tx_busy", "tx_tokens_released",
	"tx_ring_queued", "tx_live_slots", "bss_absent", "bss_quota",
	"last_rxv", "rx_rate_valid", "rx_rate_age_ms",
};

static int mt_get_sset_count(struct net_device *netdev, int set)
{
	return set == ETH_SS_STATS ? ARRAY_SIZE(mt_stat_names) : -EOPNOTSUPP;
}

static void mt_get_strings(struct net_device *netdev, u32 set, u8 *data)
{
	if (set == ETH_SS_STATS)
		memcpy(data, mt_stat_names, sizeof(mt_stat_names));
}

static void mt_get_stats(struct net_device *netdev, struct ethtool_stats *stats,
			 u64 *data)
{
	struct mt7932 *m = *(struct mt7932 **)netdev_priv(netdev);
	unsigned long flags;
	unsigned int i, live = 0;

	/* Single-writer lifetime counters, sampled independently on arm64.
	 * A snapshot is not a firmware query and never touches MMIO.
	 */
	data[0] = READ_ONCE(m->interrupts);
	data[1] = READ_ONCE(m->rx_descriptors);
	data[2] = READ_ONCE(m->rx_doorbells);
	data[3] = READ_ONCE(m->rx_ethernet);
	data[4] = READ_ONCE(m->rx_parse_errors);
	data[5] = READ_ONCE(m->rx_filtered);
	data[6] = READ_ONCE(m->rx_amsdu);
	data[7] = READ_ONCE(m->rx_rate_missing);
	memset(data + 8, 0, 7 * sizeof(*data));
	if (READ_ONCE(m->data_ready)) {
		spin_lock_irqsave(&m->data_lock, flags);
		data[8] = m->tx_doorbells;
		data[9] = m->tx_busy;
		data[10] = m->tx_released;
		data[11] = m->tx[0].queued;
		for (i = 0; i < ARRAY_SIZE(m->data_live); i++)
			live += m->data_live[i];
		data[12] = live;
		data[13] = m->bss_absent;
		data[14] = m->bss_quota;
		spin_unlock_irqrestore(&m->data_lock, flags);
	}
	spin_lock_irqsave(&m->response_lock, flags);
	data[15] = m->last_rxv;
	data[16] = m->connected && m->rx_rate_valid &&
		time_before(jiffies, m->rx_rate_updated + 10 * HZ);
	data[17] = m->rx_rate_updated ?
		jiffies_to_msecs(jiffies - m->rx_rate_updated) : U32_MAX;
	spin_unlock_irqrestore(&m->response_lock, flags);
}

const struct ethtool_ops mt_ethtool_ops = {
	.get_link = ethtool_op_get_link,
	.get_sset_count = mt_get_sset_count,
	.get_strings = mt_get_strings,
	.get_ethtool_stats = mt_get_stats,
};
