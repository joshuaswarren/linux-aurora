/* SPDX-License-Identifier: GPL-2.0-only */
#ifndef MT7932_SCAN_H
#define MT7932_SCAN_H

#include "protocol.h"
#include "channels.h"

/* Selected ordinary TX17/fullmac wildcard scan, not passive or a pre-RF gate.
 * Caller must own BSS0 and filter cfg80211 DISABLED/NO_IR channels first.
 * NEO_FIRST_CFG80211_SCAN_CONTRACT: 20ms dwell and meaningful MT7932 tail.
 */
static inline int mt7932_scan_body(u8 *body, size_t capacity, u8 seq,
				   const u8 *channels, unsigned int count)
{
	static const u8 ies[] = {0x7f,0x0a,1,0,8,0x8c,1,0x40,0,0,0,0,
				0x6b,7,0x0f,0xff,0xff,0xff,0xff,0xff,0xff};
	unsigned int i, j;

	if (capacity < 0x4d4 || !seq || seq > 127 || !count || count > 13)
		return -EINVAL;
	for (i = 0; i < count; i++) {
		if (!mt7932_channel_band(channels[i]) ||
		    mt7932_channel_band(channels[i]) != mt7932_channel_band(channels[0]))
			return -EINVAL;
		for (j = 0; j < i; j++)
			if (channels[i] == channels[j])
				return -EINVAL;
	}
	memset(body, 0, 0x4d4);
	body[0] = seq;
	body[2] = 1;
	body[3] = 1;
	body[6] = 0x12;
	body[7] = 1;
	put_unaligned_le16(20, body + 0x9a);
	body[0x9e] = 4;
	body[0x9f] = count;
	for (i = 0; i < count; i++) {
		body[0xa0 + 2 * i] = mt7932_channel_band(channels[i]);
		body[0xa1 + 2 * i] = channels[i];
	}
	put_unaligned_le16(sizeof(ies), body + 0xe0);
	memcpy(body + 0xe2, ies, sizeof(ies));
	put_unaligned_le16(20, body + 0x33c);
	memset(body + 0x458, 0xff, 6);
	put_unaligned_le32(0x1800, body + 0x4b8);
	put_unaligned_le32(45, body + 0x4bc);
	put_unaligned_le16(110, body + 0x4c2);
	put_unaligned_le16(110, body + 0x4ca);
	return 0x4d4;
}

/* Bounded standard caller-IE carriage, not the reference's silent override. */
static inline int mt7932_scan_options(u8 body[0x4d4], const u8 *ssid,
				      size_t ssid_length, const u8 *ies, size_t ie_length)
{
	size_t offset = 0;

	if (ssid_length > 32 || ie_length > 600)
		return -EINVAL;
	while (offset < ie_length) {
		if (ie_length - offset < 2 || ies[offset + 1] > ie_length - offset - 2)
			return -EINVAL;
		offset += 2 + ies[offset + 1];
	}
	if (ssid_length) {
		body[3] = 8;
		body[4] = 1;
		put_unaligned_le32(ssid_length, body + 8);
		memcpy(body + 12, ssid, ssid_length);
	}
	if (ie_length || ssid_length) {
		memset(body + 0xe2, 0, 600);
		put_unaligned_le16(ie_length, body + 0xe0);
		if (ie_length)
			memcpy(body + 0xe2, ies, ie_length);
	}
	return 0;
}

/* Original online-scan state retains the BSS/keys and supplies its home
 * channel at the end of CID03. This never narrows the requested scan list.
 * Keep the qualified 20/20/0/110/45/110 timing profile for every batch.
 */
static inline int mt7932_scan_home(u8 body[0x4d4], u8 channel)
{
	u8 band = mt7932_channel_band(channel);

	if (channel && !band)
		return -EINVAL;
	body[0x4cc] = channel;
	body[0x4cd] = band;
	return 0;
}

#endif
