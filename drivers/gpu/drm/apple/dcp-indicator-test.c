// SPDX-License-Identifier: GPL-2.0-only OR MIT
/*
 * Behaviour of the software camera indicator state. The same file builds as
 * a KUnit suite in the kernel and as a plain program (-DDCP_IND_USERSPACE).
 */
#ifdef DCP_IND_USERSPACE
#include "kunit-shim.h"
#else
#include <kunit/test.h>
#include <linux/module.h>
#endif

#include "dcp-indicator.h"

static void indicator_starts_off(struct kunit *test)
{
	struct dcp_indicator ind = {};

	KUNIT_EXPECT_EQ(test, ind.state, DCP_IND_OFF);
	KUNIT_EXPECT_FALSE(test, dcp_ind_active(&ind));
	KUNIT_EXPECT_FALSE(test, dcp_ind_visible(&ind, true, 500));
}

/* The stream may start only after the panel presented the indicator. */
static void indicator_lit_only_after_present(struct kunit *test)
{
	struct dcp_indicator ind = {};

	KUNIT_ASSERT_EQ(test, dcp_ind_begin(&ind), 0);
	KUNIT_EXPECT_TRUE(test, dcp_ind_active(&ind));
	KUNIT_EXPECT_FALSE(test, dcp_ind_visible(&ind, true, 500));
	dcp_ind_presented(&ind, true);
	KUNIT_EXPECT_TRUE(test, dcp_ind_visible(&ind, true, 500));
}

static void indicator_failed_present_never_lit(struct kunit *test)
{
	struct dcp_indicator ind = {};

	KUNIT_ASSERT_EQ(test, dcp_ind_begin(&ind), 0);
	dcp_ind_presented(&ind, false);
	KUNIT_EXPECT_FALSE(test, dcp_ind_visible(&ind, true, 500));
	/* A later good present cannot revive a failed request. */
	dcp_ind_presented(&ind, true);
	KUNIT_EXPECT_FALSE(test, dcp_ind_visible(&ind, true, 500));
	dcp_ind_end(&ind);
	KUNIT_EXPECT_FALSE(test, dcp_ind_active(&ind));
	KUNIT_EXPECT_EQ(test, dcp_ind_begin(&ind), 0);
}

static void indicator_present_failure_while_lit(struct kunit *test)
{
	struct dcp_indicator ind = {};

	KUNIT_ASSERT_EQ(test, dcp_ind_begin(&ind), 0);
	dcp_ind_presented(&ind, true);
	dcp_ind_present_failed(&ind);
	KUNIT_EXPECT_FALSE(test, dcp_ind_visible(&ind, true, 500));
	KUNIT_EXPECT_TRUE(test, dcp_ind_active(&ind));
}

/* One owner at a time: a second request cannot reset a running stream. */
static void indicator_single_owner(struct kunit *test)
{
	struct dcp_indicator ind = {};

	KUNIT_ASSERT_EQ(test, dcp_ind_begin(&ind), 0);
	dcp_ind_presented(&ind, true);
	KUNIT_EXPECT_EQ(test, dcp_ind_begin(&ind), -EBUSY);
	KUNIT_EXPECT_TRUE(test, dcp_ind_visible(&ind, true, 500));
}

/* A dark or blanked panel, or a level below the floor, is not lit. */
static void indicator_visibility_conditions(struct kunit *test)
{
	struct dcp_indicator ind = {};

	KUNIT_ASSERT_EQ(test, dcp_ind_begin(&ind), 0);
	dcp_ind_presented(&ind, true);
	KUNIT_EXPECT_TRUE(test, dcp_ind_visible(&ind, true, DCP_IND_MIN_NITS));
	KUNIT_EXPECT_FALSE(test, dcp_ind_visible(&ind, true, DCP_IND_MIN_NITS - 1));
	KUNIT_EXPECT_FALSE(test, dcp_ind_visible(&ind, true, 0));
	KUNIT_EXPECT_FALSE(test, dcp_ind_visible(&ind, false, 500));
}

/* Only the kernel's own framebuffer may sit on the reserved plane. */
static void indicator_plane_reservation(struct kunit *test)
{
	/* An ordinary plane is never restricted. */
	KUNIT_EXPECT_TRUE(test, dcp_ind_plane_ok(false, true, false));
	KUNIT_EXPECT_TRUE(test, dcp_ind_plane_ok(false, true, true));
	/* The reserved plane: disabled or the kernel framebuffer only. */
	KUNIT_EXPECT_TRUE(test, dcp_ind_plane_ok(true, false, false));
	KUNIT_EXPECT_TRUE(test, dcp_ind_plane_ok(true, true, true));
	KUNIT_EXPECT_FALSE(test, dcp_ind_plane_ok(true, false, true));
}

/* While the indicator is active, colour management cannot change. */
static void indicator_colour_changes(struct kunit *test)
{
	struct dcp_indicator ind = {};

	KUNIT_EXPECT_TRUE(test, dcp_ind_color_change_ok(&ind, true));
	KUNIT_ASSERT_EQ(test, dcp_ind_begin(&ind), 0);
	KUNIT_EXPECT_TRUE(test, dcp_ind_color_change_ok(&ind, false));
	KUNIT_EXPECT_FALSE(test, dcp_ind_color_change_ok(&ind, true));
	dcp_ind_end(&ind);
	KUNIT_EXPECT_TRUE(test, dcp_ind_color_change_ok(&ind, true));
}

static void indicator_lut_identity(struct kunit *test)
{
	static struct drm_color_lut lut[256];
	unsigned int i;

	for (i = 0; i < 256; i++)
		lut[i].red = lut[i].green = lut[i].blue = i * 0xffff / 255;
	KUNIT_EXPECT_TRUE(test, dcp_ind_lut_identity(lut, 256));
	KUNIT_EXPECT_TRUE(test, dcp_ind_lut_identity(NULL, 0));
	lut[100].green += DCP_IND_LUT_TOLERANCE + 1;
	KUNIT_EXPECT_FALSE(test, dcp_ind_lut_identity(lut, 256));
	lut[100].green -= DCP_IND_LUT_TOLERANCE + 1;
	lut[255].blue = 0x8000;
	KUNIT_EXPECT_FALSE(test, dcp_ind_lut_identity(lut, 256));
	/* A single-entry table cannot describe a ramp. */
	KUNIT_EXPECT_FALSE(test, dcp_ind_lut_identity(lut, 1));
}

static void indicator_ctm_identity(struct kunit *test)
{
	struct drm_color_ctm ctm = {};

	ctm.matrix[0] = ctm.matrix[4] = ctm.matrix[8] = 1ULL << 32;
	KUNIT_EXPECT_TRUE(test, dcp_ind_ctm_identity(&ctm));
	KUNIT_EXPECT_TRUE(test, dcp_ind_ctm_identity(NULL));
	ctm.matrix[8] = 1ULL << 31;
	KUNIT_EXPECT_FALSE(test, dcp_ind_ctm_identity(&ctm));
	ctm.matrix[8] = 1ULL << 32;
	ctm.matrix[1] = 1;
	KUNIT_EXPECT_FALSE(test, dcp_ind_ctm_identity(&ctm));
}

static struct kunit_case indicator_cases[] = {
	KUNIT_CASE(indicator_starts_off),
	KUNIT_CASE(indicator_lit_only_after_present),
	KUNIT_CASE(indicator_failed_present_never_lit),
	KUNIT_CASE(indicator_present_failure_while_lit),
	KUNIT_CASE(indicator_single_owner),
	KUNIT_CASE(indicator_visibility_conditions),
	KUNIT_CASE(indicator_plane_reservation),
	KUNIT_CASE(indicator_colour_changes),
	KUNIT_CASE(indicator_lut_identity),
	KUNIT_CASE(indicator_ctm_identity),
	{}
};

static struct kunit_suite indicator_suite = {
	.name = "apple-dcp-indicator",
	.test_cases = indicator_cases,
};

#ifdef DCP_IND_USERSPACE
KUNIT_SHIM_MAIN(indicator_suite)
#else
kunit_test_suite(indicator_suite);
MODULE_DESCRIPTION("Software camera indicator state tests");
MODULE_LICENSE("Dual MIT/GPL");
#endif
