/* SPDX-License-Identifier: GPL-2.0-only */
#ifndef MT7932_RATE_H
#define MT7932_RATE_H

#include <net/cfg80211.h>

/* Connac2 compact P-RXV, as in mt76_connac2_mac_fill_rx_rate() for
 * MT7921/7922 (not MT7915's group-5 C-RXV). Only whole-channel formats
 * whose rate_info representation is complete are reported. MU/TB need RU
 * information beyond this word; do not invent their rate from channel BW.
 */
static inline bool mt7932_rx_rate(u32 rxv, struct rate_info *rate)
{
	static const u16 ofdm[] = {480, 240, 120, 60, 540, 360, 180, 90};
	static const u16 cck[] = {10, 20, 55, 110};
	static const u8 widths[] = {
		RATE_INFO_BW_20, RATE_INFO_BW_40, RATE_INFO_BW_80, RATE_INFO_BW_160,
	};
	u8 mode = (rxv >> 24) & 15, bw = (rxv >> 12) & 7;
	u8 gi = (rxv >> 15) & 3, mcs = rxv & 127;
	u8 nss = ((rxv >> 7) & 7) + 1;

	memset(rate, 0, sizeof(*rate));
	if (bw >= ARRAY_SIZE(widths) || nss > 4)
		return false;
	rate->bw = widths[bw];
	switch (mode) {
	case 0: /* CCK: bit 2 selects short preamble (not valid at 1 Mbps). */
		if (bw || mcs > 7 || mcs == 4)
			return false;
		rate->legacy = cck[mcs & 3];
		return true;
	case 1: /* OFDM hardware codes 8..15. */
		if (bw || mcs < 8 || mcs > 15)
			return false;
		rate->legacy = ofdm[mcs - 8];
		return true;
	case 2: /* HT mixed mode */
	case 3: /* HT greenfield */
		if (bw > 1 || gi > 1 || mcs > 31)
			return false;
		rate->flags = RATE_INFO_FLAGS_MCS;
		rate->mcs = mcs;
		break;
	case 4: /* VHT */
		if (gi > 1 || mcs > 9)
			return false;
		rate->flags = RATE_INFO_FLAGS_VHT_MCS;
		rate->mcs = mcs;
		rate->nss = nss;
		break;
	case 8: /* HE SU, DCM in the Connac2 rate index */
		if (gi > 2 || (mcs & ~31) || (mcs & 15) > 11)
			return false;
		rate->flags = RATE_INFO_FLAGS_HE_MCS;
		rate->mcs = mcs & 15;
		rate->nss = nss;
		rate->he_gi = gi;
		rate->he_dcm = !!(mcs & 16);
		return true;
	default:
		return false;
	}
	if (gi)
		rate->flags |= RATE_INFO_FLAGS_SHORT_GI;
	return true;
}

#endif
