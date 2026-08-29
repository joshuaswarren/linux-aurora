// SPDX-License-Identifier: GPL-2.0-only OR MIT
/* Copyright The Asahi Linux Contributors */

#include <kunit/test.h>
#include <linux/string.h>

#include "dcp-link.h"

static void apple_dcp_link_init_desc_test(struct kunit *test)
{
	struct apple_dcp_link_init_desc desc;

	KUNIT_EXPECT_EQ(test, APPLE_DCP_LINK_ENDPOINT, 0x37);
	apple_dcp_link_init_desc_encode(&desc, 0x123456789abcULL, true);
	KUNIT_EXPECT_EQ(test, le64_to_cpu(desc.shared_dva), 0x123456789abcULL);
	KUNIT_EXPECT_EQ(test, le32_to_cpu(desc.flags),
			APPLE_DCP_LINK_INIT_FLAG_64M);
	KUNIT_EXPECT_EQ(test, le32_to_cpu(desc.version),
			APPLE_DCP_LINK_PROTOCOL_VERSION);
}

static void apple_dcp_link_firmware_dva_test(struct kunit *test)
{
	const u64 aperture_start = 0x10000000000ULL;
	const u64 aperture_end = 0x10fffffffffULL;
	u64 firmware_dva = 0;
	u64 message = 0;
	int ret;

	ret = apple_dcp_link_firmware_dva(0x1de0000, aperture_start,
					  aperture_end, &firmware_dva);
	KUNIT_ASSERT_EQ(test, ret, 0);
	KUNIT_EXPECT_EQ(test, firmware_dva, 0x10001de0000ULL);
	KUNIT_ASSERT_EQ(test,
			apple_dcp_link_init_message(firmware_dva, &message), 0);
	KUNIT_EXPECT_EQ(test, message, 0x10001de00000040ULL);

	ret = apple_dcp_link_firmware_dva(0x10001de0000ULL, aperture_start,
					  aperture_end, &firmware_dva);
	KUNIT_ASSERT_EQ(test, ret, 0);
	KUNIT_EXPECT_EQ(test, firmware_dva, 0x10001de0000ULL);

	ret = apple_dcp_link_firmware_dva(0x1100000000ULL, aperture_start,
					  aperture_end, &firmware_dva);
	KUNIT_EXPECT_EQ(test, ret, -ERANGE);
	ret = apple_dcp_link_firmware_dva(0, aperture_end, aperture_start,
					  &firmware_dva);
	KUNIT_EXPECT_EQ(test, ret, -EINVAL);
}

static void apple_dcp_link_inherited_memory_test(struct kunit *test)
{
	struct apple_dcp_link_init_desc *desc;
	u8 *memory;
	bool idle;

	memory = kunit_kzalloc(test, APPLE_DCP_LINK_RPC_MEMORY_SIZE,
			       GFP_KERNEL);
	KUNIT_ASSERT_NOT_NULL(test, memory);
	desc = (struct apple_dcp_link_init_desc *)memory;
	KUNIT_EXPECT_TRUE(test, apple_dcp_link_init_desc_zero(desc));
	apple_dcp_link_init_desc_encode(desc, 0x10002000000ULL, true);

	KUNIT_EXPECT_FALSE(test, apple_dcp_link_init_desc_zero(desc));
	KUNIT_EXPECT_TRUE(test, apple_dcp_link_init_desc_valid(desc));
	idle = apple_dcp_link_stream_memory_idle(memory,
						 sizeof(*desc));
	KUNIT_EXPECT_FALSE(test, idle);
	idle = apple_dcp_link_stream_memory_idle(memory,
						 APPLE_DCP_LINK_RPC_MEMORY_SIZE);
	KUNIT_EXPECT_TRUE(test, idle);

	memory[0x4000] = 1;
	idle = apple_dcp_link_stream_memory_idle(memory,
						 APPLE_DCP_LINK_RPC_MEMORY_SIZE);
	KUNIT_EXPECT_FALSE(test, idle);
	memory[0x4000] = 0;

	desc->version = cpu_to_le32(3);
	KUNIT_EXPECT_FALSE(test, apple_dcp_link_init_desc_valid(desc));
	desc->version = cpu_to_le32(APPLE_DCP_LINK_PROTOCOL_VERSION);
	desc->flags = cpu_to_le32(1);
	KUNIT_EXPECT_FALSE(test, apple_dcp_link_init_desc_valid(desc));
	desc->flags = cpu_to_le32(0);
	desc->shared_dva = cpu_to_le64(0x10002000001ULL);
	KUNIT_EXPECT_FALSE(test, apple_dcp_link_init_desc_valid(desc));

	desc->shared_dva = 0;
	desc->flags = cpu_to_le32(APPLE_DCP_LINK_INIT_FLAG_64M);
	KUNIT_EXPECT_FALSE(test, apple_dcp_link_init_desc_valid(desc));
	desc->shared_dva = cpu_to_le64(0x10002000000ULL);
	desc->flags = 0;
	KUNIT_EXPECT_FALSE(test, apple_dcp_link_init_desc_valid(desc));
	desc->flags = cpu_to_le32(APPLE_DCP_LINK_INIT_FLAG_64M);
	memory[APPLE_DCP_LINK_RPC_MEMORY_SIZE - 1] = 1;
	idle = apple_dcp_link_stream_memory_idle(memory,
						 APPLE_DCP_LINK_RPC_MEMORY_SIZE);
	KUNIT_EXPECT_FALSE(test, idle);
}

static void apple_dcp_link_adopt_decision_test(struct kunit *test)
{
	u64 ready = FIELD_PREP(APPLE_DCP_LINK_MSG_VERSION, 4) |
		    FIELD_PREP(APPLE_DCP_LINK_MSG_HASH, 0xa1b2c3d4) |
		    APPLE_DCP_LINK_MSG_READY;

	KUNIT_EXPECT_EQ(test,
		apple_dcp_link_adopt_decide(APPLE_DCP_LINK_HANDOFF_NONE,
					    0, true, false, true),
		APPLE_DCP_LINK_ADOPT_START_ENDPOINT);
	KUNIT_EXPECT_EQ(test,
		apple_dcp_link_adopt_decide(APPLE_DCP_LINK_HANDOFF_NONE,
					    0, false, true, true),
		APPLE_DCP_LINK_ADOPT_INVALID);
	KUNIT_EXPECT_EQ(test,
		apple_dcp_link_adopt_decide(APPLE_DCP_LINK_HANDOFF_NONE,
					    1, true, false, true),
		APPLE_DCP_LINK_ADOPT_INVALID);
	KUNIT_EXPECT_EQ(test,
		apple_dcp_link_adopt_decide(
			APPLE_DCP_LINK_HANDOFF_FIRMWARE_INIT, 0,
			true, false, true),
		APPLE_DCP_LINK_ADOPT_SEND_INIT);
	KUNIT_EXPECT_EQ(test,
		apple_dcp_link_adopt_decide(
			APPLE_DCP_LINK_HANDOFF_FIRMWARE_INIT, 0,
			false, true, true),
		APPLE_DCP_LINK_ADOPT_SEND_INIT);
	KUNIT_EXPECT_EQ(test,
		apple_dcp_link_adopt_decide(
			APPLE_DCP_LINK_HANDOFF_FIRMWARE_INIT, BIT_ULL(6),
			true, false, true),
		APPLE_DCP_LINK_ADOPT_INVALID);
	KUNIT_EXPECT_EQ(test,
		apple_dcp_link_adopt_decide(APPLE_DCP_LINK_HANDOFF_READY,
					    ready, false, true, true),
		APPLE_DCP_LINK_ADOPT_START_IOMFB);
	KUNIT_EXPECT_EQ(test,
		apple_dcp_link_adopt_decide(APPLE_DCP_LINK_HANDOFF_READY,
					    ready, true, false, true),
		APPLE_DCP_LINK_ADOPT_INVALID);
	KUNIT_EXPECT_EQ(test,
		apple_dcp_link_adopt_decide(APPLE_DCP_LINK_HANDOFF_NONE,
					    0, true, false, false),
		APPLE_DCP_LINK_ADOPT_INVALID);
}

static void apple_dcp_link_handoff_phase_test(struct kunit *test)
{
	enum apple_dcp_link_handoff_phase phase;
	u64 ready = FIELD_PREP(APPLE_DCP_LINK_MSG_VERSION, 7) |
		    FIELD_PREP(APPLE_DCP_LINK_MSG_HASH, 0xa1b2c3d4) |
		    APPLE_DCP_LINK_MSG_READY;
	bool valid;

	phase = APPLE_DCP_LINK_HANDOFF_FIRMWARE_INIT;
	valid = apple_dcp_link_handoff_message_valid(phase, 0);
	KUNIT_EXPECT_TRUE(test, valid);
	valid = apple_dcp_link_handoff_message_valid(phase, BIT_ULL(6));
	KUNIT_EXPECT_FALSE(test, valid);
	phase = APPLE_DCP_LINK_HANDOFF_READY;
	valid = apple_dcp_link_handoff_message_valid(phase, ready);
	KUNIT_EXPECT_TRUE(test, valid);
	valid = apple_dcp_link_handoff_message_valid(phase, 0);
	KUNIT_EXPECT_FALSE(test, valid);
	phase = APPLE_DCP_LINK_HANDOFF_NONE;
	valid = apple_dcp_link_handoff_message_valid(phase, ready);
	KUNIT_EXPECT_FALSE(test, valid);
	KUNIT_EXPECT_EQ(test, apple_dcp_link_negotiate_version(1), (u16)1);
	KUNIT_EXPECT_EQ(test, apple_dcp_link_negotiate_version(4), (u16)4);
	KUNIT_EXPECT_EQ(test, apple_dcp_link_negotiate_version(7), (u16)4);
}

static void apple_dcp_link_stream_layout_test(struct kunit *test)
{
	struct apple_dcp_link_stream_layout layout;

	KUNIT_ASSERT_EQ(test,
			apple_dcp_link_stream_layout(APPLE_DCP_LINK_SIDE_AP, 0,
						     false, &layout),
			0);
	KUNIT_EXPECT_EQ(test, layout.local_offset, 0U);
	KUNIT_EXPECT_EQ(test, layout.remote_offset, 0x60000U);

	KUNIT_ASSERT_EQ(test,
			apple_dcp_link_stream_layout(APPLE_DCP_LINK_SIDE_AP, 0,
						     true, &layout),
			0);
	KUNIT_EXPECT_EQ(test, layout.local_offset, 0x20000U);
	KUNIT_EXPECT_EQ(test, layout.remote_offset, 0x40000U);

	KUNIT_ASSERT_EQ(test,
			apple_dcp_link_stream_layout(APPLE_DCP_LINK_SIDE_AP, 3,
						     false, &layout),
			0);
	KUNIT_EXPECT_EQ(test, layout.local_offset, 0x18000U);
	KUNIT_EXPECT_EQ(test, layout.remote_offset, 0x78000U);
	KUNIT_EXPECT_EQ(test, layout.capacity, 0x8000U);

	KUNIT_ASSERT_EQ(test,
			apple_dcp_link_stream_layout(APPLE_DCP_LINK_SIDE_AP, 3,
						     true, &layout),
			0);
	KUNIT_EXPECT_EQ(test, layout.local_offset, 0x38000U);
	KUNIT_EXPECT_EQ(test, layout.remote_offset, 0x58000U);

	KUNIT_ASSERT_EQ(test,
			apple_dcp_link_stream_layout(APPLE_DCP_LINK_SIDE_DCP, 0,
						     false, &layout),
			0);
	KUNIT_EXPECT_EQ(test, layout.local_offset, 0x40000U);
	KUNIT_EXPECT_EQ(test, layout.remote_offset, 0x20000U);

	KUNIT_EXPECT_EQ(test,
			apple_dcp_link_stream_layout(APPLE_DCP_LINK_SIDE_AP, 4,
						     false, &layout),
			-EINVAL);
	KUNIT_EXPECT_EQ(test,
			apple_dcp_link_stream_layout(2, 0, false, &layout),
			-EINVAL);
}

static void apple_dcp_h17p_clock_rate_test(struct kunit *test)
{
	struct apple_dcp_h17p_clock_request request = {
		.service = cpu_to_le32(7),
		.clock_id = cpu_to_le32(APPLE_DCP_H17P_CLOCK_ID_154),
	};
	u64 rate;

	KUNIT_ASSERT_EQ(test,
			apple_dcp_h17p_clock_rate(&request, 300000000,
						   450000000, &rate),
			0);
	KUNIT_EXPECT_EQ(test, rate, 300000000ULL);

	request.clock_id = cpu_to_le32(APPLE_DCP_H17P_CLOCK_ID_194);
	KUNIT_ASSERT_EQ(test,
			apple_dcp_h17p_clock_rate(&request, 300000000,
						   450000000, &rate),
			0);
	KUNIT_EXPECT_EQ(test, rate, 450000000ULL);

	request.clock_id = cpu_to_le32(0);
	KUNIT_EXPECT_EQ(test,
			apple_dcp_h17p_clock_rate(&request, 300000000,
						   450000000, &rate),
			-ENOENT);
}

static void apple_dcp_h17p_method_test(struct kunit *test)
{
	struct apple_dcp_link_method method;
	static const struct {
		u32 call;
		u32 input_size;
		u32 output_size;
	} post_start_methods[] = {
		{ 0x41343432, 40, 4 },
		{ 0x41343530, 4, 4 },
		{ 0x41343635, 0, 4 },
		{ 0x41343537, 0, 0 },
		{ 0x41343538, 4, 4 },
		{ 0x41343436, 0, 4 },
		{ 0x41343638, 4, 0 },
		{ 0x41343639, 8, 4 },
		{ 0x41343733, 16, 8 },
	};
	u32 service_call;
	int i;

	KUNIT_ASSERT_EQ(test,
			apple_dcp_h17p_method(APPLE_DCP_H17P_CALL_START_SIGNAL,
						 &method),
			0);
	KUNIT_EXPECT_EQ(test, method.input_size, 0U);
	KUNIT_EXPECT_EQ(test, method.output_size, 4U);

	service_call = APPLE_DCP_H17P_CALL_PMU_SERVICE_MATCHED;
	KUNIT_ASSERT_EQ(test, apple_dcp_h17p_method(service_call, &method), 0);
	KUNIT_EXPECT_EQ(test, method.call, 0x41313331U);
	KUNIT_EXPECT_EQ(test, method.input_size, 0U);
	KUNIT_EXPECT_EQ(test, method.output_size, 4U);

	service_call = APPLE_DCP_H17P_CALL_BACKLIGHT_SERVICE_MATCHED;
	KUNIT_ASSERT_EQ(test, apple_dcp_h17p_method(service_call, &method), 0);
	KUNIT_EXPECT_EQ(test, method.call, 0x41313332U);
	KUNIT_EXPECT_EQ(test, method.input_size, 0U);
	KUNIT_EXPECT_EQ(test, method.output_size, 4U);

	for (i = 0; i < ARRAY_SIZE(post_start_methods); i++) {
		KUNIT_ASSERT_EQ(test,
				apple_dcp_h17p_method(post_start_methods[i].call,
						       &method),
				0);
		KUNIT_EXPECT_EQ(test, method.input_size,
				post_start_methods[i].input_size);
		KUNIT_EXPECT_EQ(test, method.output_size,
				post_start_methods[i].output_size);
	}

	KUNIT_ASSERT_EQ(test,
			apple_dcp_h17p_method(APPLE_DCP_H17P_CALL_SWAP_SUBMIT,
						 &method),
			0);
	KUNIT_EXPECT_EQ(test, method.input_size, 0x1d30U);
	KUNIT_EXPECT_EQ(test, method.output_size, 12U);

	KUNIT_ASSERT_EQ(test,
			apple_dcp_h17p_method(APPLE_DCP_H17P_CALL_UP_SET_CREATE_DFB,
						 &method),
			0);
	KUNIT_EXPECT_EQ(test, method.input_size, 0U);
	KUNIT_EXPECT_EQ(test, method.output_size, 0U);

	KUNIT_ASSERT_EQ(test,
			apple_dcp_h17p_method(APPLE_DCP_H17P_CALL_LATE_INIT,
						 &method),
			0);
	KUNIT_EXPECT_EQ(test, method.input_size, 4U);
	KUNIT_EXPECT_EQ(test, method.output_size, 4U);

	KUNIT_ASSERT_EQ(test,
			apple_dcp_h17p_method(
				APPLE_DCP_H17P_CALL_SETUP_VIDEO_LIMITS, &method),
			0);
	KUNIT_EXPECT_EQ(test, method.input_size, 0U);
	KUNIT_EXPECT_EQ(test, method.output_size, 0U);

	KUNIT_ASSERT_EQ(test,
			apple_dcp_h17p_method(
				APPLE_DCP_H17P_CALL_VI_SET_TEMPERATURE_HINT,
				&method),
			0);
	KUNIT_EXPECT_EQ(test, method.input_size, 0U);
	KUNIT_EXPECT_EQ(test, method.output_size, 4U);
	KUNIT_EXPECT_EQ(test, apple_dcp_h17p_method(0, &method), -ENOENT);
}

static void apple_dcp_h17p_callback_method_test(struct kunit *test)
{
	struct apple_dcp_link_method method;

	KUNIT_ASSERT_EQ(test,
			apple_dcp_h17p_callback_method(
				APPLE_DCP_H17P_CB_RT_BANDWIDTH, &method),
			0);
	KUNIT_EXPECT_EQ(test, method.input_size, 4U);
	KUNIT_EXPECT_EQ(test, method.output_size, 20U);

	KUNIT_ASSERT_EQ(test,
			apple_dcp_h17p_callback_method(
				APPLE_DCP_H17P_CB_HOTPLUG, &method),
			0);
	KUNIT_EXPECT_EQ(test, method.input_size, 88U);
	KUNIT_EXPECT_EQ(test, method.output_size, 76U);

	KUNIT_ASSERT_EQ(test,
			apple_dcp_h17p_callback_method(
				APPLE_DCP_H17P_CB_SWAP_INFO, &method),
			0);
	KUNIT_EXPECT_EQ(test, method.input_size, 228U);
	KUNIT_EXPECT_EQ(test, method.output_size, 224U);

	KUNIT_ASSERT_EQ(test,
			apple_dcp_h17p_callback_method(
				APPLE_DCP_H17P_CB_SWAP_COMPLETE, &method),
			0);
	KUNIT_EXPECT_EQ(test, method.input_size, 1840U);
	KUNIT_EXPECT_EQ(test, method.output_size, 0U);

	KUNIT_ASSERT_EQ(test,
			apple_dcp_h17p_callback_method(
				APPLE_DCP_H17P_CB_CREATE_NVRAM, &method),
			0);
	KUNIT_EXPECT_EQ(test, method.input_size, 0U);
	KUNIT_EXPECT_EQ(test, method.output_size, 4U);

	KUNIT_ASSERT_EQ(test,
			apple_dcp_h17p_callback_method(
				APPLE_DCP_H17P_CB_MATCH_PMU_2, &method),
			0);
	KUNIT_EXPECT_EQ(test, method.call, 0x44323036U);
	KUNIT_EXPECT_EQ(test, method.input_size, 0U);
	KUNIT_EXPECT_EQ(test, method.output_size, 4U);

	KUNIT_ASSERT_EQ(test,
			apple_dcp_h17p_callback_method(
				APPLE_DCP_H17P_CB_MATCH_BACKLIGHT, &method),
			0);
	KUNIT_EXPECT_EQ(test, method.call, 0x44323037U);
	KUNIT_EXPECT_EQ(test, method.input_size, 0U);
	KUNIT_EXPECT_EQ(test, method.output_size, 4U);

	KUNIT_EXPECT_EQ(test, apple_dcp_h17p_callback_method(0, &method),
			-ENOENT);
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

static void apple_dcp_link_ready_reply_test(struct kunit *test)
{
	u32 firmware_hash;
	u16 version;
	u64 reply;

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

static void apple_dcp_link_handshake_test(struct kunit *test)
{
	enum apple_dcp_link_state state = APPLE_DCP_LINK_WAIT_FIRMWARE_INIT;
	u64 ready = APPLE_DCP_LINK_MSG_READY;
	u64 firmware_init = APPLE_DCP_LINK_MSG_FIRMWARE_INIT;

	KUNIT_EXPECT_EQ(test,
			apple_dcp_link_handshake_step(&state, firmware_init),
			APPLE_DCP_LINK_HANDSHAKE_SEND_INIT);
	KUNIT_EXPECT_EQ(test, state, APPLE_DCP_LINK_WAIT_FIRMWARE_INIT);
	state = APPLE_DCP_LINK_WAIT_READY;

	KUNIT_EXPECT_EQ(test, apple_dcp_link_handshake_step(&state, ready),
			APPLE_DCP_LINK_HANDSHAKE_START_IOMFB);
	KUNIT_EXPECT_EQ(test, state, APPLE_DCP_LINK_READY);

	KUNIT_EXPECT_EQ(test,
			apple_dcp_link_handshake_step(&state, firmware_init),
			APPLE_DCP_LINK_HANDSHAKE_NONE);
	KUNIT_EXPECT_EQ(test, apple_dcp_link_handshake_step(&state, ready),
			APPLE_DCP_LINK_HANDSHAKE_NONE);
	KUNIT_EXPECT_EQ(test,
			apple_dcp_link_handshake_step(&state,
						      APPLE_DCP_LINK_MSG_RPC),
			APPLE_DCP_LINK_HANDSHAKE_NONE);
	KUNIT_EXPECT_EQ(test, state, APPLE_DCP_LINK_READY);

	state = APPLE_DCP_LINK_WAIT_FIRMWARE_INIT;
	KUNIT_EXPECT_EQ(test,
			apple_dcp_link_handshake_step(&state,
						      APPLE_DCP_LINK_MSG_INIT),
			APPLE_DCP_LINK_HANDSHAKE_NONE);
	KUNIT_EXPECT_EQ(test, state, APPLE_DCP_LINK_WAIT_FIRMWARE_INIT);
	KUNIT_EXPECT_EQ(test, apple_dcp_link_handshake_step(&state, ready),
			APPLE_DCP_LINK_HANDSHAKE_NONE);
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

	KUNIT_ASSERT_EQ(test,
			apple_dcp_link_align_payload(0x41,
						     APPLE_DCP_LINK_RPC_ALIGNMENT,
						     &size),
			0);
	KUNIT_EXPECT_EQ(test, size, 0x44U);

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
			apple_dcp_link_stream_message(message, 3, true, &encoded), 0);
	KUNIT_EXPECT_EQ(test, encoded & GENMASK_ULL(15, 0), 0x0f02ULL);
	KUNIT_EXPECT_EQ(test,
			FIELD_GET(APPLE_DCP_LINK_MSG_STREAM, encoded), 3ULL);
	KUNIT_EXPECT_EQ(test, apple_dcp_link_message_stream(encoded), (u8)3);
	KUNIT_EXPECT_TRUE(test, encoded & APPLE_DCP_LINK_MSG_REMOTE);
	KUNIT_EXPECT_TRUE(test, apple_dcp_link_message_is_remote(encoded));
	KUNIT_EXPECT_TRUE(test, encoded & APPLE_DCP_LINK_MSG_NEW_STREAM);
	KUNIT_EXPECT_EQ(test,
			apple_dcp_link_stream_message(message, 4, true, &encoded),
			-ERANGE);

	KUNIT_ASSERT_EQ(test,
			apple_dcp_link_ap_call_message(0, 0x10, 0, true, false,
						       &encoded),
			0);
	KUNIT_EXPECT_EQ(test, encoded, 0x0000001000000102ULL);
	KUNIT_EXPECT_TRUE(test, apple_dcp_link_message_is_remote(encoded));
	KUNIT_EXPECT_FALSE(test, encoded & APPLE_DCP_LINK_MSG_NEW_STREAM);
	KUNIT_EXPECT_EQ(test, apple_dcp_link_rpc_reply(encoded), 0x142ULL);

	KUNIT_ASSERT_EQ(test,
			apple_dcp_link_ap_call_message(0x10, 0x10, 0, false,
						       false, &encoded),
			0);
	KUNIT_EXPECT_EQ(test, encoded, 0x0000001000100002ULL);
	KUNIT_EXPECT_FALSE(test, apple_dcp_link_message_is_remote(encoded));
	KUNIT_EXPECT_FALSE(test, encoded & APPLE_DCP_LINK_MSG_NEW_STREAM);

	KUNIT_ASSERT_EQ(test,
			apple_dcp_link_ap_call_message(0, 0x10, 1, false, true,
						       &encoded),
			0);
	KUNIT_EXPECT_EQ(test, encoded, 0x0000001000000602ULL);
	KUNIT_EXPECT_EQ(test, apple_dcp_link_message_stream(encoded), (u8)1);
	KUNIT_EXPECT_FALSE(test, apple_dcp_link_message_is_remote(encoded));
	KUNIT_EXPECT_TRUE(test, encoded & APPLE_DCP_LINK_MSG_NEW_STREAM);
}

static void apple_dcp_h17p_service_match_wire_test(struct kunit *test)
{
	static const u8 temperature_payload[0x10] = {
		0x36, 0x38, 0x33, 0x41,
		0x00, 0x00, 0x00, 0x00,
		0x04, 0x00, 0x00, 0x00,
		0x00, 0x00, 0x00, 0x00,
	};
	static const u8 pmu_payload[0x10] = {
		0x31, 0x33, 0x31, 0x41,
		0x00, 0x00, 0x00, 0x00,
		0x04, 0x00, 0x00, 0x00,
		0x00, 0x00, 0x00, 0x00,
	};
	static const u8 backlight_payload[0x10] = {
		0x32, 0x33, 0x31, 0x41,
		0x00, 0x00, 0x00, 0x00,
		0x04, 0x00, 0x00, 0x00,
		0x00, 0x00, 0x00, 0x00,
	};
	struct apple_dcp_link_stream_layout layout;
	struct apple_dcp_link_rpc_header *header;
	u8 payload[0x10] = {};
	u64 message;

	KUNIT_ASSERT_EQ(test,
			apple_dcp_link_stream_layout(APPLE_DCP_LINK_SIDE_AP, 0,
						     false, &layout),
			0);
	KUNIT_EXPECT_EQ(test, layout.local_offset, 0U);
	KUNIT_EXPECT_EQ(test, layout.capacity, 0x8000U);

	header = (void *)payload;
	apple_dcp_link_rpc_header_encode(
		header, APPLE_DCP_H17P_CALL_VI_SET_TEMPERATURE_HINT, 0, 4);
	KUNIT_EXPECT_MEMEQ(test, payload, temperature_payload, sizeof(payload));
	KUNIT_ASSERT_EQ(test,
			apple_dcp_link_ap_call_message(0, sizeof(payload), 1, false,
						       true, &message),
			0);
	KUNIT_EXPECT_EQ(test, message, 0x0000001000000602ULL);

	memset(payload, 0, sizeof(payload));
	apple_dcp_link_rpc_header_encode(
		header, APPLE_DCP_H17P_CALL_PMU_SERVICE_MATCHED, 0, 4);
	KUNIT_EXPECT_MEMEQ(test, payload, pmu_payload, sizeof(payload));

	memset(payload, 0, sizeof(payload));
	apple_dcp_link_rpc_header_encode(
		header, APPLE_DCP_H17P_CALL_BACKLIGHT_SERVICE_MATCHED, 0, 4);
	KUNIT_EXPECT_MEMEQ(test, payload, backlight_payload, sizeof(payload));

	KUNIT_ASSERT_EQ(test,
			apple_dcp_link_ap_call_message(0x10, sizeof(payload), 0,
						       false, false, &message),
			0);
	KUNIT_EXPECT_EQ(test, message, 0x0000001000100002ULL);
	KUNIT_EXPECT_EQ(test, apple_dcp_link_message_stream(message), (u8)0);
	KUNIT_EXPECT_FALSE(test, apple_dcp_link_message_is_remote(message));
	KUNIT_EXPECT_FALSE(test, message & APPLE_DCP_LINK_MSG_NEW_STREAM);
}

static void apple_dcp_h17p_service_match_output_test(struct kunit *test)
{
	__le32 output = cpu_to_le32(0xa5a5a5a5);

	apple_dcp_h17p_service_match_output(&output, false);
	KUNIT_EXPECT_EQ(test, le32_to_cpu(output), 0xa5a5a5a5U);

	apple_dcp_h17p_service_match_output(&output, true);
	KUNIT_EXPECT_EQ(test, le32_to_cpu(output), 1U);
}

static void apple_dcp_h17p_start_signal_wire_test(struct kunit *test)
{
	static const u8 payload[0x10] = {
		0x31, 0x30, 0x34, 0x41,
		0x00, 0x00, 0x00, 0x00,
		0x04, 0x00, 0x00, 0x00,
		0x00, 0x00, 0x00, 0x00,
	};
	struct apple_dcp_link_rpc_header header = {};
	u64 message;

	apple_dcp_link_rpc_header_encode(
		&header, APPLE_DCP_H17P_CALL_START_SIGNAL, 0, 4);
	KUNIT_EXPECT_MEMEQ(test, &header, payload, sizeof(header));

	KUNIT_ASSERT_EQ(test,
			apple_dcp_link_ap_call_message(0, sizeof(payload), 0, false,
					       true, &message),
			0);
	KUNIT_EXPECT_EQ(test, message, 0x0000001000000202ULL);
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
			apple_dcp_link_stream_message(request, 3, true, &request), 0);
	reply = apple_dcp_link_rpc_reply(request);
	KUNIT_EXPECT_EQ(test, reply, 0x0000000000000f42ULL);
}

static void apple_dcp_h17p_rt_bw_test(struct kunit *test)
{
	struct apple_dcp_h17p_rt_bw_config config;

	apple_dcp_h17p_rt_bw_encode(&config, 0x3082a0000ULL, 0x3082e8000ULL);
	KUNIT_EXPECT_EQ(test, le64_to_cpu(config.scratch), 0x3082a0000ULL);
	KUNIT_EXPECT_EQ(test, le64_to_cpu(config.clock_request), 0x3082e8000ULL);
}

static void apple_dcp_h17p_rt_bw_reply_test(struct kunit *test)
{
	struct apple_dcp_h17p_rt_bw_request request = {};
	struct apple_dcp_h17p_rt_bw_reply reply = {};
	struct apple_dcp_link_rpc_view view = {
		.call = APPLE_DCP_LINK_CALL_D003,
		.input_size = sizeof(request),
		.output_size = sizeof(reply),
		.input = &request,
		.output = &reply,
	};

	KUNIT_ASSERT_EQ(test,
			apple_dcp_h17p_rt_bw_encode_reply(&view, 0x3082a0000ULL,
							     0x3082e8000ULL),
			0);
	KUNIT_EXPECT_EQ(test, le64_to_cpu(reply.config.scratch),
			0x3082a0000ULL);
	KUNIT_EXPECT_EQ(test, le64_to_cpu(reply.config.clock_request),
			0x3082e8000ULL);
	KUNIT_EXPECT_EQ(test, le32_to_cpu(reply.status), 0U);

	request.config_null = cpu_to_le32(1);
	KUNIT_EXPECT_EQ(test,
			apple_dcp_h17p_rt_bw_encode_reply(&view, 0x3082a0000ULL,
							     0x3082e8000ULL),
			-EINVAL);
}

static struct kunit_case apple_dcp_link_test_cases[] = {
	KUNIT_CASE(apple_dcp_link_init_desc_test),
	KUNIT_CASE(apple_dcp_link_firmware_dva_test),
	KUNIT_CASE(apple_dcp_link_inherited_memory_test),
	KUNIT_CASE(apple_dcp_link_handoff_phase_test),
	KUNIT_CASE(apple_dcp_link_adopt_decision_test),
	KUNIT_CASE(apple_dcp_link_stream_layout_test),
	KUNIT_CASE(apple_dcp_h17p_clock_rate_test),
	KUNIT_CASE(apple_dcp_h17p_method_test),
	KUNIT_CASE(apple_dcp_h17p_callback_method_test),
	KUNIT_CASE(apple_dcp_link_init_message_test),
	KUNIT_CASE(apple_dcp_link_ready_reply_test),
	KUNIT_CASE(apple_dcp_link_handshake_test),
	KUNIT_CASE(apple_dcp_link_rpc_header_test),
	KUNIT_CASE(apple_dcp_link_rpc_size_test),
	KUNIT_CASE(apple_dcp_link_rpc_message_test),
	KUNIT_CASE(apple_dcp_h17p_service_match_wire_test),
	KUNIT_CASE(apple_dcp_h17p_service_match_output_test),
	KUNIT_CASE(apple_dcp_h17p_start_signal_wire_test),
	KUNIT_CASE(apple_dcp_link_stack_test),
	KUNIT_CASE(apple_dcp_link_rpc_decode_test),
	KUNIT_CASE(apple_dcp_link_rpc_reply_test),
	KUNIT_CASE(apple_dcp_h17p_rt_bw_test),
	KUNIT_CASE(apple_dcp_h17p_rt_bw_reply_test),
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
