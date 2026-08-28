// SPDX-License-Identifier: GPL-2.0-only OR MIT
/* Copyright The Asahi Linux Contributors */

#include <kunit/test.h>

#include "dcp-link.h"

static void apple_dcp_link_init_desc_test(struct kunit *test)
{
	struct apple_dcp_link_init_desc desc;

	apple_dcp_link_init_desc_encode(&desc, 0x123456789abcULL, true);
	KUNIT_EXPECT_EQ(test, le64_to_cpu(desc.shared_dva), 0x123456789abcULL);
	KUNIT_EXPECT_EQ(test, le32_to_cpu(desc.flags),
			APPLE_DCP_LINK_INIT_FLAG_64M);
	KUNIT_EXPECT_EQ(test, le32_to_cpu(desc.version),
			APPLE_DCP_LINK_PROTOCOL_VERSION);
}

static void apple_dcp_link_init_message_test(struct kunit *test)
{
	u64 message;

	KUNIT_ASSERT_EQ(test,
			apple_dcp_link_init_message(0x123456789abcULL, &message), 0);
	KUNIT_EXPECT_EQ(test, message, 0x123456789abc0040ULL);
	KUNIT_EXPECT_EQ(test,
			apple_dcp_link_init_message(BIT_ULL(48), &message), -ERANGE);
}

static void apple_dcp_link_ready_message_test(struct kunit *test)
{
	KUNIT_EXPECT_EQ(test, apple_dcp_link_ready_message(),
			0x0004000000000001ULL);
}

static void apple_dcp_link_rpc_header_test(struct kunit *test)
{
	struct apple_dcp_link_rpc_header header;

	apple_dcp_link_rpc_header_encode(&header, 0x44303033, 0x24, 0x10);
	KUNIT_EXPECT_EQ(test, le32_to_cpu(header.call), 0x44303033U);
	KUNIT_EXPECT_EQ(test, le32_to_cpu(header.input_size), 0x24U);
	KUNIT_EXPECT_EQ(test, le32_to_cpu(header.output_size), 0x10U);
}

static void apple_dcp_link_rpc_size_test(struct kunit *test)
{
	u32 size;

	KUNIT_ASSERT_EQ(test, apple_dcp_link_rpc_payload_size(0x24, 0x10, &size), 0);
	KUNIT_EXPECT_EQ(test, size, 0x40U);
	KUNIT_EXPECT_EQ(test,
			apple_dcp_link_rpc_payload_size(U32_MAX, 1, &size),
			-EOVERFLOW);

	KUNIT_ASSERT_EQ(test, apple_dcp_link_align_payload(0x41, 0x40, &size), 0);
	KUNIT_EXPECT_EQ(test, size, 0x80U);
	KUNIT_EXPECT_EQ(test, apple_dcp_link_align_payload(0x40, 0, &size),
			-EINVAL);
}

static void apple_dcp_link_rpc_message_test(struct kunit *test)
{
	u64 message;

	KUNIT_ASSERT_EQ(test,
			apple_dcp_link_rpc_message(0x1234, 0x5678, false, &message),
			0);
	KUNIT_EXPECT_EQ(test, message, 0x0000567812340002ULL);

	KUNIT_ASSERT_EQ(test,
			apple_dcp_link_rpc_message(0x1234, 0x5678, true, &message),
			0);
	KUNIT_EXPECT_EQ(test, message, 0x0000567812340202ULL);
	KUNIT_EXPECT_EQ(test,
			apple_dcp_link_rpc_message(0x10000, 0, false, &message),
			-ERANGE);
}

static void apple_dcp_h17p_rt_bw_test(struct kunit *test)
{
	struct apple_dcp_h17p_rt_bw_config config;

	apple_dcp_h17p_rt_bw_encode(&config, 0x3082a0000ULL, 0x3082e8000ULL);
	KUNIT_EXPECT_EQ(test, le64_to_cpu(config.scratch), 0x3082a0000ULL);
	KUNIT_EXPECT_EQ(test, le64_to_cpu(config.clock_request), 0x3082e8000ULL);
}

static struct kunit_case apple_dcp_link_test_cases[] = {
	KUNIT_CASE(apple_dcp_link_init_desc_test),
	KUNIT_CASE(apple_dcp_link_init_message_test),
	KUNIT_CASE(apple_dcp_link_ready_message_test),
	KUNIT_CASE(apple_dcp_link_rpc_header_test),
	KUNIT_CASE(apple_dcp_link_rpc_size_test),
	KUNIT_CASE(apple_dcp_link_rpc_message_test),
	KUNIT_CASE(apple_dcp_h17p_rt_bw_test),
	{}
};

static struct kunit_suite apple_dcp_link_test_suite = {
	.name = "apple-dcp-link",
	.test_cases = apple_dcp_link_test_cases,
};

kunit_test_suite(apple_dcp_link_test_suite);

MODULE_AUTHOR("The Asahi Linux Contributors");
MODULE_LICENSE("Dual MIT/GPL");
MODULE_DESCRIPTION("Apple DCPLink message encoder KUnit tests");
