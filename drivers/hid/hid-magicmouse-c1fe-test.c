// SPDX-License-Identifier: GPL-2.0-or-later
#include <kunit/test.h>

#include "hid-magicmouse-c1fe.h"

#define TEST_CONTACT_SIZE 30
#define TEST_MAX_CONTACTS 16

static void c1fe_init(u8 *data, unsigned int count)
{
	memset(data, 0, MAGICMOUSE_C1FE_HEADER_SIZE +
		       count * TEST_CONTACT_SIZE + MAGICMOUSE_C1FE_SUFFIX_SIZE);
	data[0] = MAGICMOUSE_C1FE_REPORT_ID;
	data[2] = MAGICMOUSE_C1FE_HEADER_SIZE;
	data[3] = 4;
	put_unaligned_le32(count * TEST_CONTACT_SIZE, data + 16);
	put_unaligned_le16(MAGICMOUSE_C1FE_SUFFIX_SIZE, data + 20);
	data[22] = count;
}

static void magicmouse_c1fe_valid_test(struct kunit *test)
{
	u8 zero[40];
	u8 two[100];

	c1fe_init(zero, 0);
	c1fe_init(two, 2);
	KUNIT_EXPECT_TRUE(test, magicmouse_c1fe_layout_valid(
		zero, sizeof(zero), TEST_MAX_CONTACTS, TEST_CONTACT_SIZE));
	KUNIT_EXPECT_TRUE(test, magicmouse_c1fe_layout_valid(
		two, sizeof(two), TEST_MAX_CONTACTS, TEST_CONTACT_SIZE));
}

static void magicmouse_c1fe_rejects_near_misses_test(struct kunit *test)
{
	u8 data[100];

	c1fe_init(data, 2);
	data[3] = 3;
	KUNIT_EXPECT_FALSE(test, magicmouse_c1fe_layout_valid(
		data, sizeof(data), TEST_MAX_CONTACTS, TEST_CONTACT_SIZE));
	c1fe_init(data, 2);
	put_unaligned_le32(TEST_CONTACT_SIZE, data + 16);
	KUNIT_EXPECT_FALSE(test, magicmouse_c1fe_layout_valid(
		data, sizeof(data), TEST_MAX_CONTACTS, TEST_CONTACT_SIZE));
	c1fe_init(data, 2);
	put_unaligned_le16(0, data + 20);
	KUNIT_EXPECT_FALSE(test, magicmouse_c1fe_layout_valid(
		data, sizeof(data), TEST_MAX_CONTACTS, TEST_CONTACT_SIZE));
	c1fe_init(data, 2);
	KUNIT_EXPECT_FALSE(test, magicmouse_c1fe_layout_valid(
		data, sizeof(data) - 1, TEST_MAX_CONTACTS, TEST_CONTACT_SIZE));
	c1fe_init(data, 2);
	data[22] = TEST_MAX_CONTACTS + 1;
	KUNIT_EXPECT_FALSE(test, magicmouse_c1fe_layout_valid(
		data, sizeof(data), TEST_MAX_CONTACTS, TEST_CONTACT_SIZE));
}

static struct kunit_case magicmouse_c1fe_test_cases[] = {
	KUNIT_CASE(magicmouse_c1fe_valid_test),
	KUNIT_CASE(magicmouse_c1fe_rejects_near_misses_test),
	{}
};

static struct kunit_suite magicmouse_c1fe_test_suite = {
	.name = "hid-magicmouse-c1fe",
	.test_cases = magicmouse_c1fe_test_cases,
};

kunit_test_suite(magicmouse_c1fe_test_suite);

MODULE_DESCRIPTION("Apple J700 C1FE multitouch framing tests");
MODULE_LICENSE("GPL");
