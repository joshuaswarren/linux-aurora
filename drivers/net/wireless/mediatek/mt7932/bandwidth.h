/* SPDX-License-Identifier: GPL-2.0-only */
#ifndef MT7932_BANDWIDTH_H
#define MT7932_BANDWIDTH_H

#include <linux/errno.h>
#include <linux/types.h>
#include "channels.h"

/* Private/tainted 25G83 research: ASSOC_START uses ENUM_CHANNEL_WIDTH
 * (CW_20_40MHZ=0, CW_80MHZ=1), NOT mt7921 CMD_CBW (20=0,40=1,80=2).
 * SCO uses the public MediaTek CHNL_EXT enum: none=0, above=1, below=3.
 * AisPreloadCal derives S1 from primary/CW/SCO; it does not trust event S1.
 */
static inline int mt7932_bw_decode(u8 primary, u8 cw, u8 sco,
				   unsigned int limit, u8 *center)
{
	unsigned int band = mt7932_channel_band(primary);
	int mhz;

	if (!band || (sco != 0 && sco != 1 && sco != 3))
		return -EINVAL;
	if (!cw) {
		mhz = sco ? 40 : 20;
		*center = primary + (sco == 1 ? 2 : sco == 3 ? -2 : 0);
	} else if (cw == 1 && band == 2 && primary != 165) {
		mhz = 80;
		*center = primary <= 48 ? 42 : 155;
		/* A supplied SCO must agree with the primary's 40 MHz pair. */
		if (sco && sco != ((primary < 149 ? (primary - 36) :
				   (primary - 149)) % 8 ? 3 : 1))
			return -EINVAL;
	} else {
		return -EOPNOTSUPP;
	}
	if (mhz > limit || ((band == 1 || primary == 165) && mhz != 20))
		return -EOPNOTSUPP;
	return mhz;
}

#endif
