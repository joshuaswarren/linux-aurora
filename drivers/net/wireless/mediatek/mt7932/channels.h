/* SPDX-License-Identifier: GPL-2.0-only */
#ifndef MT7932_CHANNELS_H
#define MT7932_CHANNELS_H

/* Bounded operational whitelist, not the full calibration channel namespace.
 * Firmware RF bands1/2 are distinct from single-interface MAC index0.
 * 5 GHz: UNII-1 (36-48) and the non-DFS UNII-3 block (149-165); the own OCA2
 * carries type-3 records for every one of them and type-2 records for their
 * 40 MHz centres (38/46 and 151/159/167).
 */
#define MT7932_CHANNELS_2G 13
#define MT7932_CHANNELS_5G 9
#define MT7932_CHANNELS (MT7932_CHANNELS_2G + MT7932_CHANNELS_5G)

static inline unsigned int mt7932_channel_band(unsigned int channel)
{
	if (channel >= 1 && channel <= 13)
		return 1;
	if ((channel >= 36 && channel <= 48 && !(channel % 4)) ||
	    (channel >= 149 && channel <= 165 && !((channel - 149) % 4)))
		return 2;
	return 0;
}

/* The 40 MHz centre whose own type-2 calibration records serve a 5 GHz channel. */
static inline unsigned int mt7932_channel_cal_centre(unsigned int channel)
{
	return channel <= 40 ? 38 : channel <= 48 ? 46 :
	       channel <= 153 ? 151 : channel <= 161 ? 159 : 167;
}

#endif
