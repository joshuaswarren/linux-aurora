// SPDX-License-Identifier: GPL-2.0-only
#include "mt7932.h"

static unsigned int max_bw_mhz = 20;

static int mt_bw_param_set(const char *value, const struct kernel_param *kp)
{
	unsigned int mhz;
	int ret = kstrtouint(value, 0, &mhz);

	if (ret || (mhz != 20 && mhz != 40 && mhz != 80)) {
		pr_warn("mt7932_fullmac: invalid max_bw_mhz; retaining 20 MHz\n");
		mhz = 20;
	}
	*(unsigned int *)kp->arg = mhz;
	return 0;
}

static const struct kernel_param_ops mt_bw_param_ops = {
	.set = mt_bw_param_set,
	.get = param_get_uint,
};
module_param_cb(max_bw_mhz, &mt_bw_param_ops, &max_bw_mhz, 0444);
MODULE_PARM_DESC(max_bw_mhz, "Experimental private 5 GHz maximum: 20 (default), 40, 80; boot-time only");

unsigned int mt_bw_requested(void)
{
	return max_bw_mhz;
}

void mt_bw_caps(struct mt7932 *m)
{
	struct ieee80211_supported_band *band = &m->band5;
	struct ieee80211_sta_he_cap *he = &m->band5_iftype.he_cap;
	unsigned int i, nss = min_t(unsigned int, 2, m->phy_cap[4]);
	u16 mcs = 0xffff;

	/* Keep the default wiphy byte-for-byte as before. Firmware builds the
	 * association IEs; these describe the opt-in host admission envelope.
	 * Conservative MCS subsets, no invented beamforming/160/80+80 support.
	 */
	if (m->bw_limit == 20)
		return;
	if (!m->phy_cap[0] || !nss) {
		m->bw_limit = 20;
		return;
	}
	band->ht_cap.ht_supported = true;
	band->ht_cap.cap = IEEE80211_HT_CAP_SUP_WIDTH_20_40;
	band->ht_cap.ampdu_factor = IEEE80211_HT_MAX_AMPDU_64K;
	band->ht_cap.ampdu_density = IEEE80211_HT_MPDU_DENSITY_16;
	band->ht_cap.mcs.tx_params = IEEE80211_HT_MCS_TX_DEFINED;
	for (i = 0; i < nss; i++) {
		band->ht_cap.mcs.rx_mask[i] = 0xff;
		mcs &= ~(3 << (2 * i)); /* VHT MCS 0-7 / HE MCS 0-7 */
	}
	if (m->phy_cap[1]) {
		band->vht_cap.vht_supported = true;
		band->vht_cap.vht_mcs.rx_mcs_map = cpu_to_le16(mcs);
		band->vht_cap.vht_mcs.tx_mcs_map = cpu_to_le16(mcs);
	} else if (m->bw_limit == 80) {
		m->bw_limit = 40;
	}
	if (m->phy_cap[11]) {
		m->band5_iftype.types_mask = BIT(NL80211_IFTYPE_STATION);
		he->has_he = true;
		he->he_cap_elem.phy_cap_info[0] =
			IEEE80211_HE_PHY_CAP0_CHANNEL_WIDTH_SET_40MHZ_80MHZ_IN_5G;
		he->he_mcs_nss_supp.rx_mcs_80 = cpu_to_le16(mcs);
		he->he_mcs_nss_supp.tx_mcs_80 = cpu_to_le16(mcs);
		he->he_mcs_nss_supp.rx_mcs_160 = cpu_to_le16(0xffff);
		he->he_mcs_nss_supp.tx_mcs_160 = cpu_to_le16(0xffff);
		he->he_mcs_nss_supp.rx_mcs_80p80 = cpu_to_le16(0xffff);
		he->he_mcs_nss_supp.tx_mcs_80p80 = cpu_to_le16(0xffff);
		_ieee80211_set_sband_iftype_data(band, &m->band5_iftype, 1);
	}
}

static bool mt_bw_usable(struct mt7932 *m, struct ieee80211_channel *channel,
			 unsigned int mhz, unsigned int center)
{
	struct cfg80211_chan_def def = {
		.chan = channel,
		.width = mhz == 80 ? NL80211_CHAN_WIDTH_80 :
			 mhz == 40 ? NL80211_CHAN_WIDTH_40 : NL80211_CHAN_WIDTH_20_NOHT,
		.center_freq1 = ieee80211_channel_to_frequency(center, channel->band),
	};

	return cfg80211_chandef_valid(&def) &&
		cfg80211_chandef_usable(m->wiphy, &def, IEEE80211_CHAN_DISABLED |
			IEEE80211_CHAN_NO_IR | IEEE80211_CHAN_RADAR | IEEE80211_CHAN_NO_OFDM);
}

u32 mt_bw_channel_flags(struct mt7932 *m, struct ieee80211_channel *channel)
{
	u32 flags = channel->flags | IEEE80211_CHAN_NO_160MHZ;
	unsigned int primary = channel->hw_value;

	/* Never clear a regulatory flag. cfg80211 checks every constituent
	 * 20 MHz channel, directional HT40 restrictions and the band caps.
	 */
	if (channel->band != NL80211_BAND_5GHZ || primary == 165 || m->bw_limit == 20 ||
	    READ_ONCE(m->bw_fallback))
		return flags | 0x1b0;
	if (!mt_bw_usable(m, channel, 40, primary + 2))
		flags |= IEEE80211_CHAN_NO_HT40PLUS;
	if (!mt_bw_usable(m, channel, 40, primary - 2))
		flags |= IEEE80211_CHAN_NO_HT40MINUS;
	if (m->bw_limit < 80 || primary == 165 ||
	    !mt_bw_usable(m, channel, 80, primary <= 48 ? 42 : 155))
		flags |= IEEE80211_CHAN_NO_80MHZ;
	return flags;
}

/* response_lock held. All channels/caps remain allocated until reboot. */
int mt_bw_assoc_start(struct mt7932 *m, const u8 *body, size_t length)
{
	struct ieee80211_channel *channel;
	unsigned int limit = READ_ONCE(m->bw_fallback) ? 20 : m->bw_limit;
	u8 center;
	int mhz;

	if (length < 20 || get_unaligned_le32(body + 12) !=
	    mt7932_channel_band(m->connect_channel) || body[17] != m->connect_channel)
		return -EPROTO;
	/* Preserve the exact legacy guard at 20 (unused event bytes ignored). */
	if (limit == 20) {
		if (body[16] || body[19])
			return -EPROTO;
		m->assoc_center = m->connect_channel;
		return 0;
	}
	mhz = mt7932_bw_decode(body[17], body[16], body[19], limit, &center);
	if (mhz < 0)
		return mhz;
	if (!m->connect_bss)
		return -EPROTO;
	channel = m->connect_bss->channel;
	if (!mt_bw_usable(m, channel, mhz, center))
		return -EINVAL;
	m->assoc_width = body[16];
	m->assoc_sco = body[19];
	m->assoc_center = center;
	dev_info(&m->pdev->dev,
		 "BW_ASSOC: primary=%u MHz=%d CW=%u SCO=%u center=%u event-S1=%u event-S2=%u segment=%u\n",
		 body[17], mhz, body[16], body[19], center, body[10], body[11], body[18]);
	return 0;
}

/* Callable from RX or the serialized connection worker. Boot-sticky. */
void mt_bw_latch(struct mt7932 *m, const char *reason)
{
	if (m->bw_limit == 20 || READ_ONCE(m->bw_fallback))
		return;
	WRITE_ONCE(m->bw_fallback, true);
	WRITE_ONCE(m->bw_policy_pending, true);
	dev_warn(&m->pdev->dev,
		 "BW_FALLBACK_LATCHED: requested=%u effective-next=20 reason=%s; clean firmware retirement required\n",
		 m->bw_limit, reason);
}

/* cfg80211 connect callback holds wiphy mutex; no active association/scan. */
void mt_bw_narrow_host(struct mt7932 *m)
{
	unsigned int i;

	for (i = 0; i < ARRAY_SIZE(m->channels5); i++)
		m->channels5[i].flags |= 0x1b0;
	memset(&m->band5.ht_cap, 0, sizeof(m->band5.ht_cap));
	memset(&m->band5.vht_cap, 0, sizeof(m->band5.vht_cap));
	_ieee80211_set_sband_iftype_data(&m->band5, NULL, 0);
}
