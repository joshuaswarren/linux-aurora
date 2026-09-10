// SPDX-License-Identifier: GPL-2.0 OR MIT
#include <kunit/test.h>

#include "dockchannel-hid-protocol.h"

static void dchid_power_method_1_on_test(struct kunit *test)
{
	const u8 expected[DCHID_POWER_METHOD_1_CMD_SIZE] = {
		DCHID_CMD_RESET_INTERFACE, DCHID_POWER_METHOD_1, 0x5a,
		DCHID_POWER_STATE_ON,
	};
	u8 command[DCHID_POWER_METHOD_1_CMD_SIZE];

	dchid_pm1_command(0x5a, DCHID_POWER_STATE_ON, command);

	KUNIT_EXPECT_MEMEQ(test, command, expected, sizeof(expected));
}

static void dchid_power_method_2_will_change_test(struct kunit *test)
{
	const u8 expected[DCHID_POWER_METHOD_2_CMD_SIZE] = {
		DCHID_CMD_RESET_INTERFACE, DCHID_POWER_METHOD_2, 0x5a,
		DCHID_POWER_STATE_ON, 0, 0, 0, 0, 0,
	};
	u8 command[DCHID_POWER_METHOD_2_CMD_SIZE];

	dchid_pm2_command(0x5a, false, command);

	KUNIT_EXPECT_MEMEQ(test, command, expected, sizeof(expected));
}

static void dchid_power_method_2_has_changed_test(struct kunit *test)
{
	const u8 expected[DCHID_POWER_METHOD_2_CMD_SIZE] = {
		DCHID_CMD_RESET_INTERFACE, DCHID_POWER_METHOD_2, 0xff,
		DCHID_POWER_STATE_ON, 1, 0, 0, 0, 0,
	};
	u8 command[DCHID_POWER_METHOD_2_CMD_SIZE];

	dchid_pm2_command(0xff, true, command);

	KUNIT_EXPECT_MEMEQ(test, command, expected, sizeof(expected));
}

static void dchid_power_method_2_off_transaction_test(struct kunit *test)
{
	const u8 will[DCHID_POWER_METHOD_2_CMD_SIZE] = {
		DCHID_CMD_RESET_INTERFACE, DCHID_POWER_METHOD_2, 0x5a,
		DCHID_POWER_STATE_OFF, 0, 0, 0, 0, 0,
	};
	const u8 has[DCHID_POWER_METHOD_2_CMD_SIZE] = {
		DCHID_CMD_RESET_INTERFACE, DCHID_POWER_METHOD_2, 0x5a,
		DCHID_POWER_STATE_OFF, 1, 0, 0, 0, 0,
	};
	u8 command[DCHID_POWER_METHOD_2_CMD_SIZE];

	dchid_pm2_state_command(0x5a, DCHID_POWER_STATE_OFF, false, command);
	KUNIT_EXPECT_MEMEQ(test, command, will, sizeof(will));
	dchid_pm2_state_command(0x5a, DCHID_POWER_STATE_OFF, true, command);
	KUNIT_EXPECT_MEMEQ(test, command, has, sizeof(has));
}

static void dchid_j700_default_power_method_test(struct kunit *test)
{
	KUNIT_EXPECT_EQ(test, dchid_default_pm(true, true, true),
			DCHID_POWER_METHOD_2);
	KUNIT_EXPECT_EQ(test, dchid_default_pm(false, true, true),
			DCHID_POWER_METHOD_1);
	KUNIT_EXPECT_EQ(test, dchid_default_pm(true, false, true),
			DCHID_POWER_METHOD_1);
	KUNIT_EXPECT_EQ(test, dchid_default_pm(true, true, false),
			DCHID_POWER_METHOD_1);
}

static struct kunit_case dchid_protocol_test_cases[] = {
	KUNIT_CASE(dchid_power_method_1_on_test),
	KUNIT_CASE(dchid_power_method_2_will_change_test),
	KUNIT_CASE(dchid_power_method_2_has_changed_test),
	KUNIT_CASE(dchid_power_method_2_off_transaction_test),
	KUNIT_CASE(dchid_j700_default_power_method_test),
	{}
};

static struct kunit_suite dchid_protocol_test_suite = {
	.name = "dockchannel-hid-protocol",
	.test_cases = dchid_protocol_test_cases,
};

kunit_test_suite(dchid_protocol_test_suite);

MODULE_DESCRIPTION("Apple DockChannel HID protocol KUnit tests");
MODULE_LICENSE("Dual MIT/GPL");
