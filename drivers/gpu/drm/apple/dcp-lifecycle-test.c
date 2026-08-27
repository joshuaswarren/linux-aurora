// SPDX-License-Identifier: GPL-2.0-only OR MIT
/* Copyright The Asahi Linux Contributors */

#include <kunit/test.h>

#include "dcp-lifecycle.h"

static void apple_dcp_cold_coprocessor_test(struct kunit *test)
{
	u32 status = APPLE_DCP_COPROC_CPU_STATUS_STOPPED;

	KUNIT_EXPECT_FALSE(test, apple_dcp_coprocessor_is_active(0, status));
}

static void apple_dcp_control_run_test(struct kunit *test)
{
	u32 control = APPLE_DCP_COPROC_CPU_CONTROL_RUN;

	KUNIT_EXPECT_TRUE(test, apple_dcp_coprocessor_is_active(control, 0));
}

static void apple_dcp_status_running_test(struct kunit *test)
{
	u32 status = APPLE_DCP_COPROC_CPU_STATUS_RUNNING;

	KUNIT_EXPECT_TRUE(test, apple_dcp_coprocessor_is_active(0, status));
}

static void apple_dcp_j700_live_state_test(struct kunit *test)
{
	/* Values captured read-only from the running J700 DCP ASC wrapper. */
	KUNIT_EXPECT_TRUE(test, apple_dcp_coprocessor_is_active(0x10, 0x6d));
	/* RUNNING can transiently clear while RUN remains asserted. */
	KUNIT_EXPECT_TRUE(test, apple_dcp_coprocessor_is_active(0x10, 0x6c));
	/* Without an explicit STOPPED bit, the same status still fails closed. */
	KUNIT_EXPECT_TRUE(test, apple_dcp_coprocessor_is_active(0, 0x6c));
}

static void apple_dcp_ambiguous_state_fails_closed_test(struct kunit *test)
{
	KUNIT_EXPECT_TRUE(test, apple_dcp_coprocessor_is_active(0, 0));
}

static struct kunit_case apple_dcp_lifecycle_test_cases[] = {
	KUNIT_CASE(apple_dcp_cold_coprocessor_test),
	KUNIT_CASE(apple_dcp_control_run_test),
	KUNIT_CASE(apple_dcp_status_running_test),
	KUNIT_CASE(apple_dcp_j700_live_state_test),
	KUNIT_CASE(apple_dcp_ambiguous_state_fails_closed_test),
	{}
};

static struct kunit_suite apple_dcp_lifecycle_test_suite = {
	.name = "apple-dcp-lifecycle",
	.test_cases = apple_dcp_lifecycle_test_cases,
};

kunit_test_suite(apple_dcp_lifecycle_test_suite);

MODULE_AUTHOR("The Asahi Linux Contributors");
MODULE_LICENSE("Dual MIT/GPL");
MODULE_DESCRIPTION("Apple DCP lifecycle admission KUnit tests");
