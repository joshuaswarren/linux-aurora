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
	u32 firmware_hash;
	u16 version;
	u64 reply;

	KUNIT_EXPECT_EQ(test, apple_dcp_link_ready_message(),
			0x0004000000000001ULL);

	reply = FIELD_PREP(APPLE_DCP_LINK_MSG_VERSION, 4) |
		FIELD_PREP(APPLE_DCP_LINK_MSG_HASH, 0xa1b2c3d4) |
		APPLE_DCP_LINK_MSG_READY;
	KUNIT_ASSERT_EQ(test,
			apple_dcp_link_ready_reply(reply, &version, &firmware_hash),
			0);
	KUNIT_EXPECT_EQ(test, version, (u16)4);
	KUNIT_EXPECT_EQ(test, firmware_hash, 0xa1b2c3d4U);
	KUNIT_EXPECT_EQ(test,
			apple_dcp_link_ready_reply(APPLE_DCP_LINK_MSG_RPC, &version,
						   &firmware_hash),
			-EPROTO);
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
	u64 encoded, message;

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

	KUNIT_ASSERT_EQ(test,
			apple_dcp_link_stream_message(message, 5, true, &encoded), 0);
	KUNIT_EXPECT_EQ(test, encoded & GENMASK_ULL(15, 0), 0x1702ULL);
	KUNIT_EXPECT_EQ(test,
			FIELD_GET(APPLE_DCP_LINK_MSG_STREAM, encoded), 5ULL);
	KUNIT_EXPECT_EQ(test, apple_dcp_link_message_stream(encoded), (u8)5);
	KUNIT_EXPECT_TRUE(test, encoded & APPLE_DCP_LINK_MSG_REMOTE);
	KUNIT_EXPECT_TRUE(test, apple_dcp_link_message_is_remote(encoded));
	KUNIT_EXPECT_TRUE(test, encoded & APPLE_DCP_LINK_MSG_NESTED);
}

static void apple_dcp_link_stack_test(struct kunit *test)
{
	u32 next, offset;

	KUNIT_ASSERT_EQ(test,
			apple_dcp_link_stack_push(0x80, 0x1000, 0x41, 0x40,
						   &offset, &next),
			0);
	KUNIT_EXPECT_EQ(test, offset, 0x80U);
	KUNIT_EXPECT_EQ(test, next, 0x100U);

	KUNIT_ASSERT_EQ(test, apple_dcp_link_stack_pop(next, 0x41, 0x40, &next),
			0);
	KUNIT_EXPECT_EQ(test, next, 0x80U);
	KUNIT_EXPECT_EQ(test,
			apple_dcp_link_stack_push(0xff0, 0x1000, 0x20, 0x40,
						   &offset, &next),
			-ENOSPC);
}

static void apple_dcp_link_rpc_decode_test(struct kunit *test)
{
	struct apple_dcp_link_rpc_header *header;
	struct apple_dcp_link_rpc_view view;
	u8 buffer[0x100] = {};
	u64 message;

	header = (void *)(buffer + 0x20);
	apple_dcp_link_rpc_header_encode(header, 0x44303033, 4, 8);
	KUNIT_ASSERT_EQ(test,
			apple_dcp_link_rpc_message(0x20, sizeof(*header) + 12,
						   false, &message),
			0);
	KUNIT_ASSERT_EQ(test,
			apple_dcp_link_rpc_decode(message, buffer, sizeof(buffer),
						  &view),
			0);
	KUNIT_EXPECT_EQ(test, view.call, 0x44303033U);
	KUNIT_EXPECT_EQ(test, view.input_size, 4U);
	KUNIT_EXPECT_EQ(test, view.output_size, 8U);
	KUNIT_EXPECT_PTR_EQ(test, view.input, buffer + 0x20 + sizeof(*header));
	KUNIT_EXPECT_PTR_EQ(test, view.output,
			    buffer + 0x20 + sizeof(*header) + 4);

	header->output_size = cpu_to_le32(9);
	KUNIT_EXPECT_EQ(test,
			apple_dcp_link_rpc_decode(message, buffer, sizeof(buffer),
						  &view),
			-EMSGSIZE);
	KUNIT_EXPECT_EQ(test,
			apple_dcp_link_rpc_decode(APPLE_DCP_LINK_MSG_RPC_REPLY, buffer,
						  sizeof(buffer), &view),
			-EPROTO);
}

static void apple_dcp_link_rpc_reply_test(struct kunit *test)
{
	u64 request, reply;

	KUNIT_ASSERT_EQ(test,
			apple_dcp_link_rpc_message(0x1234, 0x5678, true, &request),
			0);
	KUNIT_ASSERT_EQ(test,
			apple_dcp_link_stream_message(request, 5, true, &request), 0);
	reply = apple_dcp_link_rpc_reply(request);
	KUNIT_EXPECT_EQ(test, reply, 0x0000000000001742ULL);
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
	KUNIT_CASE(apple_dcp_link_stack_test),
	KUNIT_CASE(apple_dcp_link_rpc_decode_test),
	KUNIT_CASE(apple_dcp_link_rpc_reply_test),
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
