// SPDX-License-Identifier: GPL-2.0-only
#include <kunit/test.h>
#include "mt7932.h"

static void mt_bw_encodings(struct kunit *test)
{
	static const struct {
		u8 primary, cw, sco, limit, center;
		int mhz;
	} cases[] = {
		{149, 0, 0, 20, 149, 20},
		{149, 0, 1, 40, 151, 40},
		{153, 0, 3, 40, 151, 40},
		{36, 1, 1, 80, 42, 80},
		{40, 1, 3, 80, 42, 80},
		{149, 1, 0, 80, 155, 80},
		{161, 1, 3, 80, 155, 80},
		{1, 0, 0, 80, 1, 20},
	};
	u8 center;
	unsigned int i;

	for (i = 0; i < ARRAY_SIZE(cases); i++) {
		int mhz = mt7932_bw_decode(cases[i].primary, cases[i].cw,
					  cases[i].sco, cases[i].limit, &center);

		KUNIT_EXPECT_EQ(test, mhz, cases[i].mhz);
		KUNIT_EXPECT_EQ(test, center, cases[i].center);
	}
	KUNIT_EXPECT_LT(test, mt7932_bw_decode(149, 1, 0, 40, &center), 0);
	KUNIT_EXPECT_LT(test, mt7932_bw_decode(149, 0, 1, 20, &center), 0);
	KUNIT_EXPECT_LT(test, mt7932_bw_decode(1, 0, 1, 80, &center), 0);
	KUNIT_EXPECT_LT(test, mt7932_bw_decode(165, 1, 0, 80, &center), 0);
	KUNIT_EXPECT_LT(test, mt7932_bw_decode(165, 0, 3, 80, &center), 0);
	KUNIT_EXPECT_LT(test, mt7932_bw_decode(149, 2, 0, 80, &center), 0);
	KUNIT_EXPECT_LT(test, mt7932_bw_decode(149, 0, 2, 80, &center), 0);
	KUNIT_EXPECT_LT(test, mt7932_bw_decode(149, 1, 3, 80, &center), 0);
}

static void mt_bw_regulatory(struct kunit *test)
{
	struct mt7932 *m = kunit_kzalloc(test, sizeof(*m), GFP_KERNEL);
	struct ieee80211_channel *ch;
	unsigned int i;
	u32 flags;

	KUNIT_ASSERT_NOT_NULL(test, m);
	m->wiphy = kunit_kzalloc(test, sizeof(*m->wiphy), GFP_KERNEL);
	KUNIT_ASSERT_NOT_NULL(test, m->wiphy);
	m->bw_limit = 20;
	mt_bw_caps(m);
	KUNIT_EXPECT_FALSE(test, m->band5.ht_cap.ht_supported);
	KUNIT_EXPECT_FALSE(test, m->band5.vht_cap.vht_supported);
	KUNIT_EXPECT_EQ(test, m->band5.n_iftype_data, (u16)0);
	m->bw_limit = 80;
	m->phy_cap[0] = 1;
	m->phy_cap[1] = 1;
	m->phy_cap[11] = 1;
	m->phy_cap[4] = 2;
	mt_bw_caps(m);
	m->band5.channels = m->channels5;
	m->band5.n_channels = ARRAY_SIZE(m->channels5);
	m->wiphy->bands[NL80211_BAND_5GHZ] = &m->band5;
	for (i = 0; i < ARRAY_SIZE(m->channels5); i++) {
		ch = &m->channels5[i];
		ch->hw_value = i < 4 ? 36 + i * 4 : 149 + (i - 4) * 4;
		ch->center_freq = 5000 + 5 * ch->hw_value;
		ch->band = NL80211_BAND_5GHZ;
		ch->flags = IEEE80211_CHAN_NO_160MHZ;
	}
	ch = &m->channels5[4]; /* 149, secondary 153, 80 MHz center 155 */
	flags = mt_bw_channel_flags(m, ch);
	KUNIT_EXPECT_EQ(test, flags & IEEE80211_CHAN_NO_80MHZ, 0U);
	KUNIT_EXPECT_EQ(test, flags & IEEE80211_CHAN_NO_HT40PLUS, 0U);
	KUNIT_EXPECT_NE(test, flags & IEEE80211_CHAN_NO_HT40MINUS, 0U);
	/* A prohibited secondary must close the firmware gate as well. */
	m->channels5[5].flags |= IEEE80211_CHAN_NO_IR;
	flags = mt_bw_channel_flags(m, ch);
	KUNIT_EXPECT_NE(test, flags & IEEE80211_CHAN_NO_80MHZ, 0U);
	KUNIT_EXPECT_NE(test, flags & IEEE80211_CHAN_NO_HT40PLUS, 0U);
	m->channels5[5].flags = IEEE80211_CHAN_NO_160MHZ;
	m->channels5[7].flags |= IEEE80211_CHAN_DISABLED;
	flags = mt_bw_channel_flags(m, ch);
	KUNIT_EXPECT_NE(test, flags & IEEE80211_CHAN_NO_80MHZ, 0U);
	KUNIT_EXPECT_EQ(test, flags & IEEE80211_CHAN_NO_HT40PLUS, 0U);
	m->channels5[7].flags = IEEE80211_CHAN_NO_160MHZ;
	m->bw_limit = 40;
	KUNIT_EXPECT_NE(test, mt_bw_channel_flags(m, ch) & IEEE80211_CHAN_NO_80MHZ, 0U);
	m->bw_limit = 80;
	m->bw_fallback = true;
	mt_bw_narrow_host(m);
	KUNIT_EXPECT_EQ(test, mt_bw_channel_flags(m, ch) & 0x1b0, 0x1b0U);
	KUNIT_EXPECT_FALSE(test, m->band5.ht_cap.ht_supported);
	KUNIT_EXPECT_FALSE(test, m->band5.vht_cap.vht_supported);
	KUNIT_EXPECT_EQ(test, m->band5.n_iftype_data, (u16)0);
}

static void mt_bw_calibration(struct kunit *test)
{
	/* Synthetic container: no own-device calibration bytes in this test. */
	u8 blob[80] = {};
	struct mt7932_cal_piece pieces[2];
	unsigned int i, j;

	memcpy(blob, "BLOB", 4);
	put_unaligned_be32(56, blob + 4);
	put_unaligned_be16(12, blob + 8);
	put_unaligned_be16(2, blob + 10);
	for (i = 0; i < 2; i++) {
		u8 *entry = blob + 16 + 20 * i;
		u8 *segment = blob + 56 + 12 * i;
		u32 sum = 0;

		put_unaligned_be16(i ? 0x2007 : 0x2001, entry);
		put_unaligned_be16(12, entry + 2);
		put_unaligned_be32(56 + 12 * i, entry + 4);
		put_unaligned_be32(12, entry + 8);
		memcpy(segment, entry, 4);
		put_unaligned_be32(12, segment + 4);
		for (j = 0; j < 12; j++)
			sum += segment[j];
		put_unaligned_be32(sum, entry + 12);
	}
	KUNIT_ASSERT_EQ(test, mt7932_cal_association(blob, sizeof(blob), 149, pieces), 2);
	KUNIT_EXPECT_EQ(test, pieces[0].parameter, 0x052a0001U); /* legacy unchanged */
	KUNIT_ASSERT_EQ(test, mt7932_cal_association_wide(blob, sizeof(blob), 151, pieces), 2);
	KUNIT_EXPECT_EQ(test, pieces[0].parameter, 0x052e0007U);
	KUNIT_EXPECT_PTR_EQ(test, pieces[0].data, blob + 76);
	KUNIT_ASSERT_EQ(test, mt7932_cal_association_wide(blob, sizeof(blob), 155, pieces), 2);
	KUNIT_EXPECT_EQ(test, pieces[0].parameter, 0x05360007U);
	KUNIT_ASSERT_EQ(test, mt7932_cal_association_wide(blob, sizeof(blob), 42, pieces), 2);
	KUNIT_EXPECT_EQ(test, pieces[0].parameter, 0x04540001U);
	KUNIT_EXPECT_LT(test, mt7932_cal_association_wide(blob, sizeof(blob), 167, pieces), 0);
}

static struct kunit_case mt_bw_cases[] = {
	KUNIT_CASE(mt_bw_encodings),
	KUNIT_CASE(mt_bw_regulatory),
	KUNIT_CASE(mt_bw_calibration),
	{}
};

static struct kunit_suite mt_bw_suite = {
	.name = "mt7932-bandwidth",
	.test_cases = mt_bw_cases,
};

kunit_test_suite(mt_bw_suite);
MODULE_LICENSE("GPL");
