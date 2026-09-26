// SPDX-License-Identifier: GPL-2.0-only OR MIT
/* Copyright The Asahi Linux Contributors */

#include <kunit/test.h>

#include "iomfb-surface.h"

static void apple_dcp_legacy_surface_order_test(struct kunit *test)
{
	struct apple_dcp_surface_order order = {};

	KUNIT_ASSERT_EQ(test, apple_dcp_surface_order_init(&order, NULL, 0), 0);
	KUNIT_EXPECT_EQ(test, order.count, 2U);
	KUNIT_EXPECT_EQ(test, order.id[0], 2U);
	KUNIT_EXPECT_EQ(test, order.id[1], 1U);
}

static void apple_dcp_j700_surface_order_test(struct kunit *test)
{
	const u32 ids[] = { 0 };
	struct apple_dcp_surface_order order = {};

	KUNIT_ASSERT_EQ(test,
			apple_dcp_surface_order_init(&order, ids, ARRAY_SIZE(ids)),
			0);
	KUNIT_EXPECT_EQ(test, order.count, 1U);
	KUNIT_EXPECT_EQ(test, order.id[0], 0U);
}

static void apple_dcp_invalid_surface_order_test(struct kunit *test)
{
	const u32 duplicate[] = { 2, 2 };
	const u32 out_of_range[] = { APPLE_DCP_MAX_SURFACES };
	struct apple_dcp_surface_order order = {};

	KUNIT_EXPECT_EQ(test,
			apple_dcp_surface_order_init(&order, duplicate,
						     ARRAY_SIZE(duplicate)),
			-EINVAL);
	KUNIT_EXPECT_EQ(test,
			apple_dcp_surface_order_init(&order, out_of_range,
						     ARRAY_SIZE(out_of_range)),
			-EINVAL);
}

static void apple_dcp_j700_pitch_test(struct kunit *test)
{
	KUNIT_EXPECT_EQ(test, apple_dcp_linear_pitch(2408, 32), 9664U);
}

static struct kunit_case apple_dcp_surface_test_cases[] = {
	KUNIT_CASE(apple_dcp_legacy_surface_order_test),
	KUNIT_CASE(apple_dcp_j700_surface_order_test),
	KUNIT_CASE(apple_dcp_invalid_surface_order_test),
	KUNIT_CASE(apple_dcp_j700_pitch_test),
	{}
};

static struct kunit_suite apple_dcp_surface_test_suite = {
	.name = "apple-dcp-surface",
	.test_cases = apple_dcp_surface_test_cases,
};

kunit_test_suite(apple_dcp_surface_test_suite);

MODULE_AUTHOR("The Asahi Linux Contributors");
MODULE_LICENSE("Dual MIT/GPL");
MODULE_DESCRIPTION("Apple DCP surface order KUnit tests");
