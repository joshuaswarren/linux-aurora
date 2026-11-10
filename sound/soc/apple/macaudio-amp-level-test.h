/* SPDX-License-Identifier: GPL-2.0-only */
/*
 * KUnit tests of the SN012776 amp levels: each amp's level is the second byte
 * of its apple,speaker-config, which the boot loader copies from the Mac's
 * ADT, and the speakers stay off when any amp lacks it.
 */
#include <kunit/of.h>
#include <kunit/test.h>

static struct device_node *macaudio_level_test_node(struct kunit *test, const char *name)
{
	struct device_node *np = of_find_node_by_name(NULL, name);

	KUNIT_ASSERT_NOT_NULL(test, np);
	of_node_put_kunit(test, np);
	return np;
}

/* The level is the row's second byte, its low five bits */
static void macaudio_level_from_row(struct kunit *test)
{
	KUNIT_EXPECT_EQ(test, macaudio_sn012776_level(
		macaudio_level_test_node(test, "macaudio-kunit-amp-a")), 0x0f);
	KUNIT_EXPECT_EQ(test, macaudio_sn012776_level(
		macaudio_level_test_node(test, "macaudio-kunit-amp-b")), 0x07);
}

/* A level above the amp's range, or no full row, is no level */
static void macaudio_level_rejects(struct kunit *test)
{
	KUNIT_EXPECT_EQ(test, macaudio_sn012776_level(
		macaudio_level_test_node(test, "macaudio-kunit-amp-high")), -ERANGE);
	KUNIT_EXPECT_EQ(test, macaudio_sn012776_level(
		macaudio_level_test_node(test, "macaudio-kunit-amp-short")), -ENOENT);
	KUNIT_EXPECT_EQ(test, macaudio_sn012776_level(
		macaudio_level_test_node(test, "macaudio-kunit-amp-none")), -ENOENT);
}

/* The speakers run only when every amp of the link has its level, with no table gain */
static void macaudio_levels_every_amp(struct kunit *test)
{
	struct device *dev = kunit_kzalloc(test, sizeof(*dev), GFP_KERNEL);

	KUNIT_ASSERT_NOT_NULL(test, dev);
	KUNIT_EXPECT_TRUE(test, macaudio_sn012776_levels_present(dev,
		macaudio_level_test_node(test, "macaudio-kunit-codec-good"), 0, NULL));
	KUNIT_EXPECT_FALSE(test, macaudio_sn012776_levels_present(dev,
		macaudio_level_test_node(test, "macaudio-kunit-codec-one-missing"), 0, NULL));
	KUNIT_EXPECT_FALSE(test, macaudio_sn012776_levels_present(dev,
		macaudio_level_test_node(test, "macaudio-kunit-codec-one-high"), 0, NULL));
}

/* A board table gain stands in for a missing row, never for an out-of-range one */
static void macaudio_level_table_fallback(struct kunit *test)
{
	struct device *dev = kunit_kzalloc(test, sizeof(*dev), GFP_KERNEL);
	bool from_table = false;

	KUNIT_ASSERT_NOT_NULL(test, dev);
	KUNIT_EXPECT_EQ(test, macaudio_sn012776_amp_level(
		macaudio_level_test_node(test, "macaudio-kunit-amp-none"), 15), 15);
	KUNIT_EXPECT_EQ(test, macaudio_sn012776_amp_level(
		macaudio_level_test_node(test, "macaudio-kunit-amp-a"), 15), 0x0f);
	KUNIT_EXPECT_EQ(test, macaudio_sn012776_amp_level(
		macaudio_level_test_node(test, "macaudio-kunit-amp-high"), 15), -ERANGE);
	KUNIT_EXPECT_TRUE(test, macaudio_sn012776_levels_present(dev,
		macaudio_level_test_node(test, "macaudio-kunit-codec-one-missing"), 15, &from_table));
	KUNIT_EXPECT_TRUE(test, from_table);
}

static int macaudio_level_test_init(struct kunit *test)
{
	return of_overlay_apply_kunit(test, macaudio_kunit);
}

static struct kunit_case macaudio_level_cases[] = {
	KUNIT_CASE(macaudio_level_from_row),
	KUNIT_CASE(macaudio_level_rejects),
	KUNIT_CASE(macaudio_levels_every_amp),
	KUNIT_CASE(macaudio_level_table_fallback),
	{}
};

static struct kunit_suite macaudio_level_suite = {
	.name = "apple-speaker-amp-level",
	.init = macaudio_level_test_init,
	.test_cases = macaudio_level_cases,
};

kunit_test_suite(macaudio_level_suite);
