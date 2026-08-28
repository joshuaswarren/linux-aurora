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
	/* Without a STOPPED bit, the same status remains active. */
	KUNIT_EXPECT_TRUE(test, apple_dcp_coprocessor_is_active(0, 0x6c));
}

static void apple_dcp_zero_status_is_active_test(struct kunit *test)
{
	KUNIT_EXPECT_TRUE(test, apple_dcp_coprocessor_is_active(0, 0));
}

static void apple_dcp_j700_adoption_test(struct kunit *test)
{
	KUNIT_EXPECT_EQ(test,
		apple_dcp_coprocessor_start_mode(APPLE_DCP_LOADER_HANDOFF_NONE,
						 true, 0x10, 0x6d),
		APPLE_DCP_COPROCESSOR_ADOPT_AND_RESTART);
	KUNIT_EXPECT_EQ(test,
		apple_dcp_coprocessor_start_mode(APPLE_DCP_LOADER_HANDOFF_NONE,
						 true, 0x10, 0x6c),
		APPLE_DCP_COPROCESSOR_ADOPT_AND_RESTART);
}

static void apple_dcp_j700_quiesced_handoff_test(struct kunit *test)
{
	KUNIT_EXPECT_EQ(test,
		apple_dcp_coprocessor_start_mode(
			APPLE_DCP_LOADER_HANDOFF_RTKIT_QUIESCED,
			true, 0x10, 0x6d),
		APPLE_DCP_COPROCESSOR_QUIESCED_RESTART);
}

static void apple_dcp_j700_idle_handoff_test(struct kunit *test)
{
	KUNIT_EXPECT_EQ(test,
		apple_dcp_coprocessor_start_mode(
			APPLE_DCP_LOADER_HANDOFF_RTKIT_IDLE,
			true, 0x10, 0x6d),
		APPLE_DCP_COPROCESSOR_IDLE_RESTART);
}

static void apple_dcp_j700_stopped_handoff_test(struct kunit *test)
{
	enum apple_dcp_loader_handoff handoff =
		APPLE_DCP_LOADER_HANDOFF_ASC_STOPPED;
	enum apple_dcp_coprocessor_start start_mode;

	/* Live loader-stopped value from J700 after the RUN bit was cleared. */
	start_mode = apple_dcp_coprocessor_start_mode(handoff, true, 0, 0x48);
	KUNIT_EXPECT_EQ(test, start_mode,
			APPLE_DCP_COPROCESSOR_ASC_STOPPED_RESTART);
	KUNIT_EXPECT_FALSE(test, apple_dcp_needs_adoption(start_mode));
	KUNIT_EXPECT_TRUE(test, apple_dcp_needs_fresh_boot(start_mode));
}

static void apple_dcp_coprocessor_stop_sequence_test(struct kunit *test)
{
	u32 original = 0xa5a5c3ff;
	u32 first = apple_dcp_coprocessor_stop_first(original);
	u32 second = apple_dcp_coprocessor_stop_second(first);

	KUNIT_EXPECT_EQ(test, first, original & ~BIT(4));
	KUNIT_EXPECT_EQ(test, second, original & ~(BIT(4) | BIT(5)));
	KUNIT_EXPECT_EQ(test, second & ~(BIT(4) | BIT(5)),
			original & ~(BIT(4) | BIT(5)));
}

static void apple_dcp_cold_start_does_not_adopt_test(struct kunit *test)
{
	bool adopt, fresh_boot;

	adopt = apple_dcp_needs_adoption(APPLE_DCP_COPROCESSOR_COLD_START);
	fresh_boot = apple_dcp_needs_fresh_boot(
		APPLE_DCP_COPROCESSOR_COLD_START);
	KUNIT_EXPECT_FALSE(test, adopt);
	KUNIT_EXPECT_FALSE(test, fresh_boot);
}

static void apple_dcp_cold_start_selection_test(struct kunit *test)
{
	u32 stopped = APPLE_DCP_COPROC_CPU_STATUS_STOPPED;

	KUNIT_EXPECT_EQ(test,
		apple_dcp_coprocessor_start_mode(APPLE_DCP_LOADER_HANDOFF_NONE,
						 true, 0, stopped),
		APPLE_DCP_COPROCESSOR_COLD_START);
	KUNIT_EXPECT_EQ(test,
		apple_dcp_coprocessor_start_mode(APPLE_DCP_LOADER_HANDOFF_NONE,
						 false, 0x10, 0x6d),
		APPLE_DCP_COPROCESSOR_COLD_START);
}

static struct kunit_case apple_dcp_lifecycle_test_cases[] = {
	KUNIT_CASE(apple_dcp_cold_coprocessor_test),
	KUNIT_CASE(apple_dcp_control_run_test),
	KUNIT_CASE(apple_dcp_status_running_test),
	KUNIT_CASE(apple_dcp_j700_live_state_test),
	KUNIT_CASE(apple_dcp_zero_status_is_active_test),
	KUNIT_CASE(apple_dcp_j700_adoption_test),
	KUNIT_CASE(apple_dcp_j700_quiesced_handoff_test),
	KUNIT_CASE(apple_dcp_j700_idle_handoff_test),
	KUNIT_CASE(apple_dcp_j700_stopped_handoff_test),
	KUNIT_CASE(apple_dcp_coprocessor_stop_sequence_test),
	KUNIT_CASE(apple_dcp_cold_start_does_not_adopt_test),
	KUNIT_CASE(apple_dcp_cold_start_selection_test),
	{}
};

static struct kunit_suite apple_dcp_lifecycle_test_suite = {
	.name = "apple-dcp-lifecycle",
	.test_cases = apple_dcp_lifecycle_test_cases,
};

kunit_test_suite(apple_dcp_lifecycle_test_suite);

MODULE_AUTHOR("The Asahi Linux Contributors");
MODULE_LICENSE("Dual MIT/GPL");
MODULE_DESCRIPTION("Apple DCP lifecycle selection KUnit tests");
