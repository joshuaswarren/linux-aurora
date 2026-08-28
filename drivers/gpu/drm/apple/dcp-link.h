/* SPDX-License-Identifier: GPL-2.0-only OR MIT */
/* Copyright The Asahi Linux Contributors */

#ifndef __APPLE_DCP_LINK_H__
#define __APPLE_DCP_LINK_H__

#include <linux/bitfield.h>
#include <linux/byteorder/generic.h>
#include <linux/errno.h>
#include <linux/overflow.h>
#include <linux/types.h>

#define APPLE_DCP_LINK_PROTOCOL_VERSION		4
#define APPLE_DCP_LINK_STREAM_COUNT		8
#define APPLE_DCP_LINK_STREAM_STATE_SIZE	0x68

#define APPLE_DCP_LINK_INIT_FLAG_64M		0x04000000

#define APPLE_DCP_LINK_MSG_READY		1
#define APPLE_DCP_LINK_MSG_RPC			2
#define APPLE_DCP_LINK_MSG_RPC_REPLY		0x42
#define APPLE_DCP_LINK_MSG_INIT			0x40
#define APPLE_DCP_LINK_MSG_NESTED		BIT_ULL(9)
#define APPLE_DCP_LINK_MSG_OFFSET		GENMASK_ULL(31, 16)
#define APPLE_DCP_LINK_MSG_SIZE			GENMASK_ULL(47, 32)
#define APPLE_DCP_LINK_MSG_VERSION		GENMASK_ULL(63, 48)

struct apple_dcp_link_init_desc {
	__le64 shared_dva;
	__le32 flags;
	__le32 version;
} __packed;

struct apple_dcp_link_rpc_header {
	__le32 call;
	__le32 input_size;
	__le32 output_size;
} __packed;

struct apple_dcp_h17p_rt_bw_config {
	__le64 scratch;
	__le64 clock_request;
} __packed;

static_assert(sizeof(struct apple_dcp_link_init_desc) == 0x10);
static_assert(sizeof(struct apple_dcp_link_rpc_header) == 0x0c);
static_assert(sizeof(struct apple_dcp_h17p_rt_bw_config) == 0x10);

static inline void
apple_dcp_link_init_desc_encode(struct apple_dcp_link_init_desc *desc,
				u64 shared_dva, bool use_64m_window)
{
	desc->shared_dva = cpu_to_le64(shared_dva);
	desc->flags = cpu_to_le32(use_64m_window ? APPLE_DCP_LINK_INIT_FLAG_64M : 0);
	desc->version = cpu_to_le32(APPLE_DCP_LINK_PROTOCOL_VERSION);
}

static inline int apple_dcp_link_init_message(u64 shared_dva, u64 *message)
{
	if (!message)
		return -EINVAL;
	if (shared_dva & ~GENMASK_ULL(47, 0))
		return -ERANGE;

	*message = (shared_dva << 16) | APPLE_DCP_LINK_MSG_INIT;
	return 0;
}

static inline u64 apple_dcp_link_ready_message(void)
{
	return FIELD_PREP(APPLE_DCP_LINK_MSG_VERSION,
			  APPLE_DCP_LINK_PROTOCOL_VERSION) |
	       APPLE_DCP_LINK_MSG_READY;
}

static inline int apple_dcp_link_rpc_payload_size(u32 input_size,
						  u32 output_size, u32 *size)
{
	u32 total;

	if (!size)
		return -EINVAL;
	if (check_add_overflow(input_size, output_size, &total) ||
	    check_add_overflow(total,
			       (u32)sizeof(struct apple_dcp_link_rpc_header),
			       &total))
		return -EOVERFLOW;

	*size = total;
	return 0;
}

static inline int apple_dcp_link_align_payload(u32 size, u32 alignment,
					       u32 *aligned_size)
{
	u32 padded;

	if (!aligned_size || !is_power_of_2(alignment))
		return -EINVAL;
	if (check_add_overflow(size, alignment - 1, &padded))
		return -EOVERFLOW;

	*aligned_size = padded & -alignment;
	return 0;
}

static inline int apple_dcp_link_rpc_message(u32 offset, u32 size, bool nested,
					     u64 *message)
{
	if (!message)
		return -EINVAL;
	if (offset > U16_MAX || size > U16_MAX)
		return -ERANGE;

	*message = APPLE_DCP_LINK_MSG_RPC |
		   FIELD_PREP(APPLE_DCP_LINK_MSG_OFFSET, offset) |
		   FIELD_PREP(APPLE_DCP_LINK_MSG_SIZE, size);
	if (nested)
		*message |= APPLE_DCP_LINK_MSG_NESTED;

	return 0;
}

static inline void
apple_dcp_link_rpc_header_encode(struct apple_dcp_link_rpc_header *header,
				 u32 call, u32 input_size, u32 output_size)
{
	header->call = cpu_to_le32(call);
	header->input_size = cpu_to_le32(input_size);
	header->output_size = cpu_to_le32(output_size);
}

static inline void
apple_dcp_h17p_rt_bw_encode(struct apple_dcp_h17p_rt_bw_config *config,
			    u64 scratch, u64 clock_request)
{
	config->scratch = cpu_to_le64(scratch);
	config->clock_request = cpu_to_le64(clock_request);
}

#endif /* __APPLE_DCP_LINK_H__ */
