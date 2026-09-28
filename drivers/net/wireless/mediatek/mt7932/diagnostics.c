// SPDX-License-Identifier: GPL-2.0-only
#include "mt7932.h"
#include <linux/seq_file.h>

static bool perf_stats;
module_param(perf_stats, bool, 0444);
MODULE_PARM_DESC(perf_stats, "Expose optional host datapath profiling in debugfs");

static bool gro_rx;
module_param(gro_rx, bool, 0444);
MODULE_PARM_DESC(gro_rx, "Use GRO cells for received Ethernet frames");

bool mt_perf_enabled(void)
{
	return perf_stats;
}

bool mt_gro_enabled(void)
{
	return gro_rx;
}

static int mt_perf_show(struct seq_file *s, void *unused)
{
	struct mt7932 *m = s->private;
	unsigned int i;

	seq_printf(s, "irq_services %llu\nirq_total_ns %llu\nirq_max_ns %llu\n",
		   READ_ONCE(m->interrupts), READ_ONCE(m->perf_irq_ns),
		   READ_ONCE(m->perf_irq_max_ns));
	seq_printf(s, "rx_frames %llu\nrx_bytes %llu\nrx_copy_ns %llu\n",
		   READ_ONCE(m->perf_rx_frames), READ_ONCE(m->perf_rx_bytes),
		   READ_ONCE(m->perf_rx_copy_ns));
	seq_printf(s, "rx_drain_full %llu\nrx_drain_max %llu\nrx_gro_frames %llu\n",
		   READ_ONCE(m->perf_rx_drain_full), READ_ONCE(m->perf_rx_drain_max),
		   READ_ONCE(m->perf_rx_gro_frames));
	seq_printf(s, "tx_frames %llu\ntx_bytes %llu\ntx_copy_ns %llu\n",
		   READ_ONCE(m->perf_tx_frames), READ_ONCE(m->perf_tx_bytes),
		   READ_ONCE(m->perf_tx_copy_ns));
	seq_printf(s, "tx_queue_stops %llu\ntx_queued_max %llu\n",
		   READ_ONCE(m->perf_tx_queue_stops), READ_ONCE(m->perf_tx_queued_max));
	seq_printf(s, "rx_doorbells %llu\ntx_doorbells %llu\ntx_busy %llu\n",
		   READ_ONCE(m->rx_doorbells), READ_ONCE(m->tx_doorbells),
		   READ_ONCE(m->tx_busy));
	for (i = 0; i < ARRAY_SIZE(m->rx); i++)
		if (m->rx[i].count)
			seq_printf(s, "rx%u_size %u\n", i, m->rx[i].count);
	seq_printf(s, "tx0_size %u\n", m->tx[0].count);
	return 0;
}

DEFINE_SHOW_ATTRIBUTE(mt_perf);

void mt_perf_init(struct mt7932 *m)
{
	if (!perf_stats)
		return;
	m->perf_dir = debugfs_create_dir(dev_name(&m->pdev->dev), NULL);
	debugfs_create_file("mt7932_datapath", 0400, m->perf_dir, m,
			    &mt_perf_fops);
}

static const char mt_stat_names[][ETH_GSTRING_LEN] = {
	"irq_services", "rx_descriptors", "rx_refill_doorbells",
	"rx_ethernet", "rx_parse_errors", "rx_peer_or_duplicate_drops",
	"rx_amsdu_subframes", "rx_unicast_no_group3",
	"tx_doorbells", "tx_busy", "tx_tokens_released",
	"tx_ring_queued", "tx_live_slots", "bss_absent", "bss_quota",
	"last_rxv", "rx_rate_valid", "rx_rate_age_ms",
	"bw_requested_mhz", "bw_effective_limit_mhz", "bw_fallback_latched",
	"bw_policy_pending", "bw_assoc_mhz", "bw_assoc_center", "fw_asserted",
	"tx_free_events", "tx_kick_max",
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
	data[25] = 0;
	data[26] = 0;
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
		data[25] = m->tx_free_events;
		data[26] = m->tx_kick_max;
		spin_unlock_irqrestore(&m->data_lock, flags);
	}
	spin_lock_irqsave(&m->response_lock, flags);
	data[15] = m->last_rxv;
	data[16] = m->connected && m->rx_rate_valid &&
		time_before(jiffies, m->rx_rate_updated + 10 * HZ);
	data[17] = m->rx_rate_updated ?
		jiffies_to_msecs(jiffies - m->rx_rate_updated) : U32_MAX;
	data[18] = mt_bw_requested();
	data[19] = m->bw_fallback ? 20 : m->bw_limit;
	data[20] = m->bw_fallback;
	data[21] = m->bw_policy_pending;
	data[22] = m->connected ? (m->assoc_width ? 80 : m->assoc_sco ? 40 : 20) : 0;
	data[23] = m->connected ? m->assoc_center : 0;
	data[24] = m->fw_asserted;
	spin_unlock_irqrestore(&m->response_lock, flags);
}

const struct ethtool_ops mt_ethtool_ops = {
	.get_link = ethtool_op_get_link,
	.get_sset_count = mt_get_sset_count,
	.get_strings = mt_get_strings,
	.get_ethtool_stats = mt_get_stats,
};
