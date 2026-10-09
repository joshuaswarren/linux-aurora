// SPDX-License-Identifier: GPL-2.0-only
#include <kunit/test.h>
#include "packet.h"
#include "rate.h"

static void mt_rate_formats(struct kunit *test)
{
	struct rate_info rate;

	/* HT20 MCS15 SGI: 144.4 Mbps, VHT80 2SS MCS9 SGI: 866.7 Mbps. */
	KUNIT_ASSERT_TRUE(test, mt7932_rx_rate((2U << 24) | BIT(15) | 15, &rate));
	KUNIT_EXPECT_EQ(test, rate.flags, (u16)(RATE_INFO_FLAGS_MCS | RATE_INFO_FLAGS_SHORT_GI));
	KUNIT_EXPECT_EQ(test, rate.mcs, (u8)15);
	KUNIT_EXPECT_EQ(test, rate.bw, (u8)RATE_INFO_BW_20);
	KUNIT_EXPECT_EQ(test, cfg80211_calculate_bitrate(&rate), 1444U);
	KUNIT_ASSERT_TRUE(test, mt7932_rx_rate((4U << 24) | (2U << 12) |
					     BIT(15) | BIT(7) | 9, &rate));
	KUNIT_EXPECT_EQ(test, rate.nss, (u8)2);
	KUNIT_EXPECT_EQ(test, rate.bw, (u8)RATE_INFO_BW_80);
	KUNIT_EXPECT_EQ(test, cfg80211_calculate_bitrate(&rate), 8667U);
	KUNIT_ASSERT_TRUE(test, mt7932_rx_rate((8U << 24) | BIT(7) | 11, &rate));
	KUNIT_EXPECT_EQ(test, rate.flags, (u16)RATE_INFO_FLAGS_HE_MCS);
	KUNIT_EXPECT_EQ(test, rate.mcs, (u8)11);
	KUNIT_EXPECT_EQ(test, rate.nss, (u8)2);
	KUNIT_EXPECT_EQ(test, rate.he_gi, (u8)NL80211_RATE_INFO_HE_GI_0_8);
	KUNIT_ASSERT_TRUE(test, mt7932_rx_rate((1U << 24) | 12, &rate));
	KUNIT_EXPECT_EQ(test, rate.legacy, (u16)540);
	KUNIT_ASSERT_TRUE(test, mt7932_rx_rate(7, &rate));
	KUNIT_EXPECT_EQ(test, rate.legacy, (u16)110);
}

static void mt_rate_rejects_incomplete(struct kunit *test)
{
	struct rate_info rate;
	static const u32 invalid[] = {
		(10U << 24) | 7, /* HE MU without RU information */
		(11U << 24) | 7, /* HE TB without RU information */
		(9U << 24) | 7,  /* HE extended SU */
		(4U << 24) | 10, /* nonstandard VHT MCS */
		(4U << 24) | (4U << 12), /* reserved width */
		(2U << 24) | (2U << 12), /* HT at 80 MHz */
		(8U << 24) | (3U << 15), /* reserved HE GI */
		(8U << 24) | 12, /* reserved HE MCS */
		(4U << 24) | (7U << 7), /* more streams than this parser supports */
		4, /* short-preamble 1 Mbps */
	};
	unsigned int i;

	for (i = 0; i < ARRAY_SIZE(invalid); i++)
		KUNIT_EXPECT_FALSE(test, mt7932_rx_rate(invalid[i], &rate));
}

static void mt_rate_descriptor_bounds(struct kunit *test)
{
	u8 packet[46] = {};
	struct mt7932_rx_frame frame;
	u32 rxv = (4U << 24) | BIT(7) | 8;

	put_unaligned_le32((2U << 27) | sizeof(packet), packet);
	put_unaligned_le32(BIT(13), packet + 4); /* group 3 */
	put_unaligned_le32(BIT(13), packet + 8); /* Ethernet translation */
	put_unaligned_le32((149U << 8) | BIT(16), packet + 12);
	put_unaligned_le32(rxv, packet + 24);
	KUNIT_ASSERT_EQ(test, mt7932_rx_frame(packet, sizeof(packet), &frame), 0);
	KUNIT_EXPECT_TRUE(test, frame.group3);
	KUNIT_EXPECT_EQ(test, frame.rxv, rxv);
	KUNIT_EXPECT_EQ(test, frame.length, (size_t)14);
	KUNIT_EXPECT_EQ(test, mt7932_rx_frame(packet, 31, &frame), -EMSGSIZE);
	put_unaligned_le32(0, packet + 4); /* no group 3: never invent a vector */
	KUNIT_ASSERT_EQ(test, mt7932_rx_frame(packet, sizeof(packet), &frame), 0);
	KUNIT_EXPECT_FALSE(test, frame.group3);
}

static struct kunit_case mt_rate_cases[] = {
	KUNIT_CASE(mt_rate_formats),
	KUNIT_CASE(mt_rate_rejects_incomplete),
	KUNIT_CASE(mt_rate_descriptor_bounds),
	{}
};

static struct kunit_suite mt_rate_suite = {
	.name = "mt7932-rate",
	.test_cases = mt_rate_cases,
};

kunit_test_suite(mt_rate_suite);
MODULE_LICENSE("GPL");
