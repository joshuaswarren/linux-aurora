// SPDX-License-Identifier: GPL-2.0-only OR MIT
/* Copyright 2021 Alyssa Rosenzweig */

#include <linux/align.h>
#include <linux/bitfield.h>
#include <linux/bitmap.h>
#include <linux/clk.h>
#include <linux/completion.h>
#include <linux/delay.h>
#include <linux/dma-mapping.h>
#include <linux/iommu.h>
#include <linux/io.h>
#include <linux/kref.h>
#include <linux/module.h>
#include <linux/of.h>
#include <linux/of_device.h>
#include <linux/ratelimit.h>
#include <linux/sizes.h>
#include <linux/slab.h>
#include <linux/soc/apple/rtkit.h>

#include <drm/drm_edid.h>
#include <drm/drm_fb_dma_helper.h>
#include <drm/drm_fourcc.h>
#include <drm/drm_framebuffer.h>
#include <drm/drm_gem_dma_helper.h>
#include <drm/drm_probe_helper.h>
#include <drm/drm_vblank.h>

#include "dcp.h"
#include "dcp-internal.h"
#include "iomfb.h"
#include "iomfb_internal.h"
#include "parser.h"
#include "trace.h"

#define APPLE_DCP_LINK_RPC_DVA_PROPERTY	"apple,dcp-link-rpc-dva"
#define APPLE_DCP_LINK_RPC_PA_PROPERTY	"apple,dcp-link-rpc-pa"
#define APPLE_DCP_LINK_RPC_SIZE_PROPERTY	"apple,dcp-link-rpc-size"
#define APPLE_DCP_LINK_PHASE_PROPERTY	"apple,dcp-link-phase"
#define APPLE_DCP_LINK_MESSAGE_PROPERTY	"apple,dcp-link-message"

static int dcp_tx_offset(enum dcp_context_id id)
{
	switch (id) {
	case DCP_CONTEXT_CB:
	case DCP_CONTEXT_CMD:
		return 0x00000;
	case DCP_CONTEXT_OOBCB:
	case DCP_CONTEXT_OOBCMD:
		return 0x08000;
	default:
		return -EINVAL;
	}
}

static int dcp_channel_offset(enum dcp_context_id id)
{
	switch (id) {
	case DCP_CONTEXT_ASYNC:
		return 0x40000;
	case DCP_CONTEXT_OOBASYNC:
		return 0x48000;
	case DCP_CONTEXT_CB:
		return 0x60000;
	case DCP_CONTEXT_OOBCB:
		return 0x68000;
	default:
		return dcp_tx_offset(id);
	}
}

static inline u64 dcpep_set_shmem(u64 dart_va)
{
	return FIELD_PREP(IOMFB_MESSAGE_TYPE, IOMFB_MESSAGE_TYPE_SET_SHMEM) |
	       FIELD_PREP(IOMFB_SHMEM_FLAG, IOMFB_SHMEM_FLAG_VALUE) |
	       FIELD_PREP(IOMFB_SHMEM_DVA, dart_va);
}

static inline u64 dcpep_msg(enum dcp_context_id id, u32 length, u16 offset)
{
	return FIELD_PREP(IOMFB_MESSAGE_TYPE, IOMFB_MESSAGE_TYPE_MSG) |
		FIELD_PREP(IOMFB_MSG_CONTEXT, id) |
		FIELD_PREP(IOMFB_MSG_OFFSET, offset) |
		FIELD_PREP(IOMFB_MSG_LENGTH, length);
}

static inline u64 dcpep_ack(enum dcp_context_id id)
{
	return dcpep_msg(id, 0, 0) | IOMFB_MSG_ACK;
}

/*
 * A channel is busy if we have sent a message that has yet to be
 * acked. The driver must not sent a message to a busy channel.
 */
static bool dcp_channel_busy(struct dcp_channel *ch)
{
	return (ch->depth != 0);
}

/*
 * Get the context ID passed to the DCP for a command we push. The rule is
 * simple: callback contexts are used when replying to the DCP, command
 * contexts are used otherwise. That corresponds to a non/zero call stack
 * depth. This rule frees the caller from tracking the call context manually.
 */
static enum dcp_context_id dcp_call_context(struct apple_dcp *dcp, bool oob)
{
	u8 depth = oob ? dcp->ch_oobcmd.depth : dcp->ch_cmd.depth;

	if (depth)
		return oob ? DCP_CONTEXT_OOBCB : DCP_CONTEXT_CB;
	else
		return oob ? DCP_CONTEXT_OOBCMD : DCP_CONTEXT_CMD;
}

/* Get a channel for a context */
static struct dcp_channel *dcp_get_channel(struct apple_dcp *dcp,
					   enum dcp_context_id context)
{
	switch (context) {
	case DCP_CONTEXT_CB:
		return &dcp->ch_cb;
	case DCP_CONTEXT_CMD:
		return &dcp->ch_cmd;
	case DCP_CONTEXT_OOBCB:
		return &dcp->ch_oobcb;
	case DCP_CONTEXT_OOBCMD:
		return &dcp->ch_oobcmd;
	case DCP_CONTEXT_ASYNC:
		return &dcp->ch_async;
	case DCP_CONTEXT_OOBASYNC:
		return &dcp->ch_oobasync;
	default:
		return NULL;
	}
}

static void dcp_link_push(struct apple_dcp *dcp, bool oob,
			  const struct dcp_method_entry *call, u32 in_len,
			  u32 out_len, void *data, dcp_callback_t cb,
			  void *cookie);
static void dcp_link_ack(struct apple_dcp *dcp,
			 enum dcp_context_id context);
static void dcp_link_recv_msg(struct apple_dcp *dcp, u64 message);
static int dcp_link_start_rtkit(struct apple_dcp *dcp);

/* Cold RPC memory is coherent. Inherited no-map RAM is mapped Normal-NC. */
static void dcp_link_sync_for_cpu(void)
{
	dma_rmb();
}

static void dcp_link_sync_for_device(void)
{
	dma_wmb();
}

/* Get the start of a packet: after the end of the previous packet */
static u16 dcp_packet_start(struct dcp_channel *ch, u8 depth)
{
	if (depth > 0)
		return ch->end[depth - 1];
	else
		return 0;
}

/* Pushes and pops the depth of the call stack with safety checks */
static u8 dcp_push_depth(u8 *depth)
{
	u8 ret = (*depth)++;

	WARN_ON(ret >= DCP_MAX_CALL_DEPTH);
	return ret;
}

static u8 dcp_pop_depth(u8 *depth)
{
	WARN_ON((*depth) == 0);

	return --(*depth);
}

/* Call a DCP function given by a tag */
void dcp_push(struct apple_dcp *dcp, bool oob, const struct dcp_method_entry *call,
		     u32 in_len, u32 out_len, void *data, dcp_callback_t cb,
		     void *cookie)
{
	if (dcp->fw_compat == DCP_FIRMWARE_H17P) {
		dcp_link_push(dcp, oob, call, in_len, out_len, data, cb,
			      cookie);
		return;
	}

	enum dcp_context_id context = dcp_call_context(dcp, oob);
	struct dcp_channel *ch = dcp_get_channel(dcp, context);

	struct dcp_packet_header header = {
		.in_len = in_len,
		.out_len = out_len,

		/* Tag is reversed due to endianness of the fourcc */
		.tag[0] = call->tag[3],
		.tag[1] = call->tag[2],
		.tag[2] = call->tag[1],
		.tag[3] = call->tag[0],
	};

	u8 depth = dcp_push_depth(&ch->depth);
	u16 offset = dcp_packet_start(ch, depth);

	void *out = dcp->shmem + dcp_tx_offset(context) + offset;
	void *out_data = out + sizeof(header);
	size_t data_len = sizeof(header) + in_len + out_len;

	memcpy(out, &header, sizeof(header));

	if (in_len > 0)
		memcpy(out_data, data, in_len);

	trace_iomfb_push(dcp, call, context, offset, depth);

	ch->callbacks[depth] = cb;
	ch->cookies[depth] = cookie;
	ch->output[depth] = out + sizeof(header) + in_len;
	ch->end[depth] = offset + ALIGN(data_len, DCP_PACKET_ALIGNMENT);

	dcp_send_message(dcp, IOMFB_ENDPOINT,
			 dcpep_msg(context, data_len, offset));
}

/* Parse a callback tag "D123" into the ID 123. Returns -EINVAL on failure. */
int dcp_parse_tag(char tag[4])
{
	u32 d[3];
	int i;

	if (tag[3] != 'D')
		return -EINVAL;

	for (i = 0; i < 3; ++i) {
		d[i] = (u32)(tag[i] - '0');

		if (d[i] > 9)
			return -EINVAL;
	}

	return d[0] + (d[1] * 10) + (d[2] * 100);
}

static u8 dcp_link_context_stream(enum dcp_context_id context)
{
	return context == DCP_CONTEXT_OOBCB ||
	       context == DCP_CONTEXT_OOBCMD ? 1 : 0;
}

static bool dcp_link_context_is_remote(enum dcp_context_id context)
{
	return context == DCP_CONTEXT_CB || context == DCP_CONTEXT_OOBCB;
}

static u32 dcp_link_call_tag(const struct dcp_method_entry *call)
{
	return ((u32)(u8)call->tag[0] << 24) |
	       ((u32)(u8)call->tag[1] << 16) |
	       ((u32)(u8)call->tag[2] << 8) |
	       (u8)call->tag[3];
}

static void dcp_link_push(struct apple_dcp *dcp, bool oob,
			  const struct dcp_method_entry *call, u32 in_len,
			  u32 out_len, void *data, dcp_callback_t cb,
			  void *cookie)
{
	enum dcp_context_id context = dcp_call_context(dcp, oob);
	struct dcp_channel *ch = dcp_get_channel(dcp, context);
	struct apple_dcp_link_stream_layout layout;
	struct apple_dcp_link_rpc_header *header;
	bool remote = dcp_link_context_is_remote(context);
	u8 stream = dcp_link_context_stream(context);
	u32 *top = &dcp->link_top[remote][stream];
	u32 next_top;
	u32 offset;
	u32 payload_size;
	u64 message;
	u8 depth;
	void *buffer;
	int ret;

	depth = dcp_push_depth(&ch->depth);
	ret = apple_dcp_link_rpc_payload_size(in_len, out_len,
					      &payload_size);
	if (ret)
		goto err_pop;
	ret = apple_dcp_link_stream_layout(APPLE_DCP_LINK_SIDE_AP, stream,
					   remote, &layout);
	if (ret)
		goto err_pop;
	ret = apple_dcp_link_stack_push(*top, layout.capacity, payload_size,
					APPLE_DCP_LINK_RPC_ALIGNMENT, &offset,
					&next_top);
	if (ret)
		goto err_pop;

	dcp_link_sync_for_cpu();
	buffer = dcp->shmem + layout.local_offset;
	header = buffer + offset;
	apple_dcp_link_rpc_header_encode(header, dcp_link_call_tag(call),
					 in_len, out_len);
	if (in_len)
		memcpy(header + 1, data, in_len);
	if (out_len)
		memset((u8 *)(header + 1) + in_len, 0, out_len);

	ret = apple_dcp_link_rpc_message(offset, payload_size, remote, &message);
	if (ret)
		goto err_pop;
	ret = apple_dcp_link_stream_message(message, stream, remote, &message);
	if (ret)
		goto err_pop;

	ch->callbacks[depth] = cb;
	ch->cookies[depth] = cookie;
	ch->output[depth] = (u8 *)(header + 1) + in_len;
	ch->link_message[depth] = message;
	ch->link_size[depth] = payload_size;
	*top = next_top;

	dcp_link_sync_for_device();
	dcp_send_message(dcp, APPLE_DCP_LINK_ENDPOINT, message);
	return;

err_pop:
	dcp_pop_depth(&ch->depth);
	dev_err(dcp->dev, "DCPLink call %.4s could not be queued: %d\n",
		call->tag, ret);
}

static void dcp_link_ack(struct apple_dcp *dcp,
			 enum dcp_context_id context)
{
	struct dcp_channel *ch = dcp_get_channel(dcp, context);
	u64 request;
	u8 depth;
	u64 reply;

	if (!ch || !ch->depth) {
		dev_warn(dcp->dev, "DCPLink callback reply without an active call\n");
		return;
	}

	depth = dcp_pop_depth(&ch->depth);
	request = ch->link_message[depth];
	dcp_link_sync_for_device();
	reply = apple_dcp_link_rpc_reply(request);
	ch->link_message[depth] = 0;
	dcp_send_message(dcp, APPLE_DCP_LINK_ENDPOINT, reply);
}

static struct dcp_channel *dcp_link_reply_channel(struct apple_dcp *dcp,
						   u64 message)
{
	bool remote = apple_dcp_link_message_is_remote(message);
	u8 stream = apple_dcp_link_message_stream(message);

	if (stream == 1)
		return remote ? &dcp->ch_oobcb : &dcp->ch_oobcmd;
	return remote ? &dcp->ch_cb : &dcp->ch_cmd;
}

static void dcp_link_handle_reply(struct apple_dcp *dcp, u64 message)
{
	struct dcp_channel *ch = dcp_link_reply_channel(dcp, message);
	bool remote = apple_dcp_link_message_is_remote(message);
	u8 stream = apple_dcp_link_message_stream(message);
	u32 *top;
	dcp_callback_t cb;
	void *cookie;
	u8 depth;
	int ret;

	if (stream >= APPLE_DCP_LINK_STREAMS_PER_SIDE) {
		dev_warn(dcp->dev, "DCPLink reply stream %u is invalid\n", stream);
		return;
	}
	top = &dcp->link_top[remote][stream];

	if (!ch->depth) {
		dev_warn(dcp->dev, "DCPLink reply without an active call\n");
		return;
	}

	depth = dcp_pop_depth(&ch->depth);
	ret = apple_dcp_link_stack_pop(*top, ch->link_size[depth],
				       APPLE_DCP_LINK_RPC_ALIGNMENT, top);
	if (ret)
		dev_warn(dcp->dev, "DCPLink stream %u stack mismatch: %d\n",
			 stream, ret);

	dcp_link_sync_for_cpu();
	cb = ch->callbacks[depth];
	cookie = ch->cookies[depth];
	ch->callbacks[depth] = NULL;
	ch->cookies[depth] = NULL;
	ch->link_message[depth] = 0;
	ch->link_size[depth] = 0;
	if (cb)
		cb(dcp, ch->output[depth], cookie);
}

static void dcp_link_handle_callback(struct apple_dcp *dcp, u64 message)
{
	struct apple_dcp_link_stream_layout layout;
	struct apple_dcp_link_method method;
	struct apple_dcp_link_rpc_view view;
	struct dcp_channel *ch = &dcp->ch_cb;
	char wire_tag[4];
	u8 stream = apple_dcp_link_message_stream(message);
	u8 depth;
	int tag;
	int ret;

	ret = apple_dcp_link_stream_layout(APPLE_DCP_LINK_SIDE_AP, stream, true,
					   &layout);
	if (ret) {
		dcp_send_message(dcp, APPLE_DCP_LINK_ENDPOINT,
				 apple_dcp_link_rpc_reply(message));
		return;
	}
	dcp_link_sync_for_cpu();
	ret = apple_dcp_link_rpc_decode(message,
					dcp->shmem + layout.remote_offset,
					layout.capacity, &view);
	if (ret) {
		dev_warn(dcp->dev, "DCPLink callback could not be decoded: %d\n",
			 ret);
		dcp_send_message(dcp, APPLE_DCP_LINK_ENDPOINT,
				 apple_dcp_link_rpc_reply(message));
		return;
	}

	ret = apple_dcp_h17p_callback_method(view.call, &method);
	if (!ret && (method.input_size != view.input_size ||
		     method.output_size != view.output_size)) {
		dev_warn(dcp->dev,
			 "DCPLink callback %#x size %u/%u, expected %u/%u\n",
			 view.call, view.input_size, view.output_size,
			 method.input_size, method.output_size);
	}

	wire_tag[0] = view.call;
	wire_tag[1] = view.call >> 8;
	wire_tag[2] = view.call >> 16;
	wire_tag[3] = view.call >> 24;
	tag = dcp_parse_tag(wire_tag);
	if (tag < 0 || tag >= IOMFB_MAX_CB) {
		dev_warn(dcp->dev, "DCPLink callback tag %#x is invalid\n",
			 view.call);
		dcp_send_message(dcp, APPLE_DCP_LINK_ENDPOINT,
				 apple_dcp_link_rpc_reply(message));
		return;
	}

	if (view.output_size)
		memset(view.output, 0, view.output_size);
	depth = dcp_push_depth(&ch->depth);
	ch->output[depth] = view.output;
	ch->link_message[depth] = message;
	ch->link_size[depth] = view.total_size;

	if (!dcp->cb_handlers || !dcp->cb_handlers[tag]) {
		dev_warn(dcp->dev, "DCPLink callback D%03d has no handler\n", tag);
		dcp_link_ack(dcp, DCP_CONTEXT_CB);
		return;
	}

	if (dcp->cb_handlers[tag](dcp, tag, view.output, (void *)view.input))
		dcp_link_ack(dcp, DCP_CONTEXT_CB);
}

static void dcp_link_recv_msg(struct apple_dcp *dcp, u64 message)
{
	enum apple_dcp_link_handshake_action action;
	enum apple_dcp_link_state old_state = dcp->link_state;
	u8 type = message & GENMASK_ULL(1, 0);

	action = apple_dcp_link_handshake_step(&dcp->link_state, message);

	if (type == APPLE_DCP_LINK_MSG_READY) {
		u16 remote_version;

		if (action != APPLE_DCP_LINK_HANDSHAKE_START_IOMFB) {
			if (old_state == APPLE_DCP_LINK_WAIT_READY)
				dev_warn(dcp->dev,
					 "DCPLink ready message is invalid\n");
			else
				dev_dbg(dcp->dev, "duplicate DCPLink ready\n");
			return;
		}

		if (apple_dcp_link_ready_reply(message, &remote_version,
					       &dcp->link_firmware_hash))
			return;
		if (!remote_version) {
			dcp->link_state = old_state;
			dev_err(dcp->dev, "DCPLink ready has version zero\n");
			return;
		}
		dcp->link_version =
			apple_dcp_link_negotiate_version(remote_version);
		dev_info(dcp->dev,
			 "DCPLink ready: remote version %u negotiated %u hash %#x\n",
			 remote_version, dcp->link_version,
			 dcp->link_firmware_hash);
		iomfb_start_h17p(dcp);
		return;
	}

	if (type == APPLE_DCP_LINK_MSG_FIRMWARE_INIT) {
		dev_dbg(dcp->dev, "duplicate DCPLink firmware init\n");
		return;
	}

	if (type != APPLE_DCP_LINK_MSG_RPC) {
		dev_warn(dcp->dev, "unknown DCPLink message %#llx\n", message);
		return;
	}

	if ((message & GENMASK_ULL(7, 6)) == BIT_ULL(6))
		dcp_link_handle_reply(dcp, message);
	else
		dcp_link_handle_callback(dcp, message);
}

static int dcp_link_start_rtkit(struct apple_dcp *dcp)
{
	struct apple_dcp_link_init_desc *desc;
	u64 init_message;
	int ret;

	if (dcp->adopted_session) {
		struct iommu_domain *domain;
		enum apple_dcp_link_adopt_action action;
		u64 captured_message;
		phys_addr_t translated;
		u64 rpc_dva, rpc_pa, rpc_size;
		u64 shared_dva;
		u32 phase;
		u32 ready_hash;
		u16 remote_version;
		u64 offset;
		bool descriptor_zero;
		bool descriptor_valid;
		bool streams_idle;

		if (!of_chosen ||
		    of_property_read_u64(of_chosen,
					 APPLE_DCP_LINK_RPC_DVA_PROPERTY,
					 &rpc_dva) ||
		    of_property_read_u64(of_chosen,
					 APPLE_DCP_LINK_RPC_PA_PROPERTY,
					 &rpc_pa) ||
		    of_property_read_u64(of_chosen,
					 APPLE_DCP_LINK_RPC_SIZE_PROPERTY,
					 &rpc_size) ||
		    of_property_read_u32(of_chosen,
					 APPLE_DCP_LINK_PHASE_PROPERTY, &phase) ||
		    of_property_read_u64(of_chosen,
					 APPLE_DCP_LINK_MESSAGE_PROPERTY,
					 &captured_message))
			return dev_err_probe(dcp->dev, -ENOENT,
					     "inherited DCPLink handoff data is missing\n");

		if (!rpc_dva || !rpc_pa ||
		    rpc_size != APPLE_DCP_LINK_RPC_MEMORY_SIZE ||
		    !IS_ALIGNED(rpc_dva, SZ_16K) ||
		    !IS_ALIGNED(rpc_pa, SZ_16K) ||
		    rpc_dva & ~GENMASK_ULL(47, 0))
			return dev_err_probe(dcp->dev, -EINVAL,
					     "inherited DCPLink RPC mapping is invalid\n");
		domain = iommu_get_domain_for_dev(dcp->dev);
		if (!domain)
			return dev_err_probe(dcp->dev, -ENODEV,
					     "DCP IOMMU domain is unavailable\n");

		for (offset = 0; offset < rpc_size; offset += SZ_16K) {
			translated = iommu_iova_to_phys(domain, rpc_dva + offset);
			if (translated != rpc_pa + offset)
				return dev_err_probe(dcp->dev, -EINVAL,
						     "inherited DCPLink RPC translation differs at %#llx\n",
						     offset);
		}

		dcp->shmem = (__force void *)ioremap_wc(rpc_pa, rpc_size);
		if (!dcp->shmem)
			return dev_err_probe(dcp->dev, -ENOMEM,
					     "could not map inherited DCPLink RPC memory\n");

		dcp->shmem_iova = rpc_dva;
		dcp->shmem_inherited = true;
		dcp_link_sync_for_cpu();
		desc = dcp->shmem;
		descriptor_zero = apple_dcp_link_init_desc_zero(desc);
		descriptor_valid = apple_dcp_link_init_desc_valid(desc);
		streams_idle = apple_dcp_link_stream_memory_idle(dcp->shmem,
							 rpc_size);
		action = apple_dcp_link_adopt_decide(phase, captured_message,
						     descriptor_zero,
						     descriptor_valid,
						     streams_idle);
		if (action == APPLE_DCP_LINK_ADOPT_INVALID) {
			dev_err(dcp->dev,
				"inherited DCPLink state is invalid: phase %u message %#llx zero %u valid %u idle %u\n",
				phase, captured_message, descriptor_zero,
				descriptor_valid, streams_idle);
			ret = -EINVAL;
			goto err_unmap_inherited;
		}
		if (descriptor_zero)
			apple_dcp_link_init_desc_encode(desc, 0, false);

		shared_dva = le64_to_cpu(desc->shared_dva);
		for (offset = 0;
		     shared_dva &&
		     offset < APPLE_DCP_LINK_SHARED_VAR_MEMORY_SIZE;
		     offset += SZ_16K) {
			if (!iommu_iova_to_phys(domain, shared_dva + offset)) {
				dev_err(dcp->dev,
					"inherited DCPLink shared-variable mapping differs at %#llx\n",
					offset);
				ret = -EINVAL;
				goto err_unmap_inherited;
			}
		}
		if (shared_dva)
			dev_info(dcp->dev,
				 "inherited DCPLink uses a validated 64MiB shared-variable DVA %#llx\n",
				 shared_dva);

		memset(dcp->link_top, 0, sizeof(dcp->link_top));

		if (action == APPLE_DCP_LINK_ADOPT_START_ENDPOINT) {
			if (dcp->link_endpoint_started) {
				ret = -EALREADY;
				goto err_unmap_inherited;
			}
			ret = apple_dcp_link_init_message(rpc_dva,
							  &captured_message);
			if (ret)
				goto err_unmap_inherited;
			dcp->link_state = APPLE_DCP_LINK_WAIT_READY;
			dcp->link_version = 0;
			dcp->link_firmware_hash = 0;
			dcp->link_endpoint_started = true;
			ret = dcp_start_rtkit_endpoint(dcp,
						       APPLE_DCP_LINK_ENDPOINT);
			if (ret) {
				dcp->link_endpoint_started = false;
				goto err_unmap_inherited;
			}
			dcp_link_sync_for_device();
			dev_info(dcp->dev,
				 "adopted unused DCPLink RPC mapping DVA %#llx PA %#llx; sending host INIT\n",
				 rpc_dva, rpc_pa);
			dcp_send_message(dcp, APPLE_DCP_LINK_ENDPOINT,
					 captured_message);
			return 0;
		}

		dcp->link_endpoint_started = true;
		if (action == APPLE_DCP_LINK_ADOPT_START_IOMFB) {
			ret = apple_dcp_link_ready_reply(captured_message,
							 &remote_version, &ready_hash);
			if (ret || !remote_version) {
				dev_err(dcp->dev,
					"inherited DCPLink ready version is zero\n");
				ret = ret ?: -EPROTO;
				goto err_unmap_inherited;
			}

			dcp->link_version =
				apple_dcp_link_negotiate_version(remote_version);
			dcp->link_firmware_hash = ready_hash;
			dcp->link_state = APPLE_DCP_LINK_READY;
			dcp_link_sync_for_device();
			dev_info(dcp->dev,
				 "adopted DCPLink ready remote version %u negotiated %u hash %#x DVA %#llx PA %#llx\n",
				 remote_version, dcp->link_version,
				 dcp->link_firmware_hash, rpc_dva, rpc_pa);
			iomfb_start_h17p(dcp);
			return 0;
		}

		if (action != APPLE_DCP_LINK_ADOPT_SEND_INIT) {
			ret = -EINVAL;
			goto err_unmap_inherited;
		}
		ret = apple_dcp_link_init_message(rpc_dva, &captured_message);
		if (ret)
			goto err_unmap_inherited;
		dcp->link_state = APPLE_DCP_LINK_WAIT_READY;
		dcp->link_version = 0;
		dcp->link_firmware_hash = 0;
		dcp_link_sync_for_device();
		dev_info(dcp->dev,
			 "adopted DCPLink firmware-init phase; sending inherited RPC INIT\n");
		dcp_send_message(dcp, APPLE_DCP_LINK_ENDPOINT, captured_message);
		return 0;

err_unmap_inherited:
		dcp_link_sync_for_device();
		iounmap((__force void __iomem *)dcp->shmem);
		dcp->shmem = NULL;
		dcp->shmem_iova = 0;
		dcp->shmem_inherited = false;
		return ret;
	}

	dcp->shmem = dma_alloc_coherent(dcp->dev, APPLE_DCP_LINK_RPC_MEMORY_SIZE,
					&dcp->shmem_iova, GFP_KERNEL);
	if (!dcp->shmem)
		return -ENOMEM;

	memset(dcp->shmem, 0, APPLE_DCP_LINK_RPC_MEMORY_SIZE);
	desc = dcp->shmem;
	apple_dcp_link_init_desc_encode(desc, 0, false);
	ret = apple_dcp_link_init_message(dcp->shmem_iova, &init_message);
	if (ret)
		return ret;

	dcp->link_state = APPLE_DCP_LINK_WAIT_READY;
	memset(dcp->link_top, 0, sizeof(dcp->link_top));

	dcp->link_endpoint_started = true;
	ret = dcp_start_rtkit_endpoint(dcp, APPLE_DCP_LINK_ENDPOINT);
	if (ret) {
		dcp->link_endpoint_started = false;
		return ret;
	}

	dcp_link_sync_for_device();
	/* DCPLink startup is host-driven: START_EP, INIT, then firmware READY. */
	dev_info(dcp->dev, "DCPLink endpoint started; sending host INIT\n");
	dcp_send_message(dcp, APPLE_DCP_LINK_ENDPOINT, init_message);
	return 0;
}

/* Ack a callback from the DCP */
void dcp_ack(struct apple_dcp *dcp, enum dcp_context_id context)
{
	struct dcp_channel *ch = dcp_get_channel(dcp, context);

	if (dcp->fw_compat == DCP_FIRMWARE_H17P) {
		dcp_link_ack(dcp, context);
		return;
	}

	dcp_pop_depth(&ch->depth);
	dcp_send_message(dcp, IOMFB_ENDPOINT,
			 dcpep_ack(context));
}

/*
 * Helper to send a DRM hotplug event. The DCP is accessed from a single
 * (RTKit) thread. To handle hotplug callbacks, we need to call
 * drm_kms_helper_hotplug_event, which does an atomic commit (via DCP) and
 * waits for vblank (a DCP callback). That means we deadlock if we call from
 * the RTKit thread! Instead, move the call to another thread via a workqueue.
 */
void dcp_hotplug(struct work_struct *work)
{
	struct apple_connector *connector;
	struct apple_dcp *dcp;

	connector = container_of(work, struct apple_connector, hotplug_wq);

	dcp = platform_get_drvdata(connector->dcp);
	dev_info(dcp->dev, "%s() connected:%d valid_mode:%d nr_modes:%u\n", __func__,
		 connector->connected, dcp->valid_mode, dcp->nr_modes);

	if (!connector->connected) {
		drm_edid_free(connector->drm_edid);
		connector->drm_edid = NULL;
	}

	/*
	 * DCP defers link training until we set a display mode. But we set
	 * display modes from atomic_flush, so userspace needs to trigger a
	 * flush, or the CRTC gets no signal.
	 */
	if (connector->base.state && !dcp->valid_mode && connector->connected)
		drm_connector_set_link_status_property(&connector->base,
						       DRM_MODE_LINK_STATUS_BAD);

	drm_kms_helper_connector_hotplug_event(&connector->base);
}

static void dcpep_handle_cb(struct apple_dcp *dcp, enum dcp_context_id context,
			    void *data, u32 length, u16 offset)
{
	struct device *dev = dcp->dev;
	struct dcp_packet_header *hdr = data;
	void *in, *out;
	int tag = dcp_parse_tag(hdr->tag);
	struct dcp_channel *ch = dcp_get_channel(dcp, context);
	u8 depth;

	if (tag < 0 || tag >= IOMFB_MAX_CB || !dcp->cb_handlers || !dcp->cb_handlers[tag]) {
		dev_warn(dev, "received unknown callback %c%c%c%c\n",
			 hdr->tag[3], hdr->tag[2], hdr->tag[1], hdr->tag[0]);
		return;
	}

	in = data + sizeof(*hdr);
	out = in + hdr->in_len;

	// TODO: verify that in_len and out_len match our prototypes
	// for now just clear the out data to have at least consistent results
	if (hdr->out_len)
		memset(out, 0, hdr->out_len);

	depth = dcp_push_depth(&ch->depth);
	ch->output[depth] = out;
	ch->end[depth] = offset + ALIGN(length, DCP_PACKET_ALIGNMENT);

	if (dcp->cb_handlers[tag](dcp, tag, out, in))
		dcp_ack(dcp, context);
}

static void dcpep_handle_ack(struct apple_dcp *dcp, enum dcp_context_id context,
			     void *data, u32 length)
{
	struct dcp_packet_header *header = data;
	struct dcp_channel *ch = dcp_get_channel(dcp, context);
	void *cookie;
	dcp_callback_t cb;

	if (!ch) {
		dev_warn(dcp->dev, "ignoring ack on context %X\n", context);
		return;
	}

	dcp_pop_depth(&ch->depth);

	cb = ch->callbacks[ch->depth];
	cookie = ch->cookies[ch->depth];

	ch->callbacks[ch->depth] = NULL;
	ch->cookies[ch->depth] = NULL;

	if (cb)
		cb(dcp, data + sizeof(*header) + header->in_len, cookie);
}

static void dcpep_got_msg(struct apple_dcp *dcp, u64 message)
{
	enum dcp_context_id ctx_id;
	u16 offset;
	u32 length;
	int channel_offset;
	void *data;

	ctx_id = FIELD_GET(IOMFB_MSG_CONTEXT, message);
	offset = FIELD_GET(IOMFB_MSG_OFFSET, message);
	length = FIELD_GET(IOMFB_MSG_LENGTH, message);

	channel_offset = dcp_channel_offset(ctx_id);

	if (channel_offset < 0) {
		dev_warn(dcp->dev, "invalid context received %u\n", ctx_id);
		return;
	}

	data = dcp->shmem + channel_offset + offset;

	if (FIELD_GET(IOMFB_MSG_ACK, message))
		dcpep_handle_ack(dcp, ctx_id, data, length);
	else
		dcpep_handle_cb(dcp, ctx_id, data, length, offset);
}

int dcp_get_modes(struct drm_connector *connector)
{
	struct apple_connector *apple_connector = to_apple_connector(connector);
	struct platform_device *pdev = apple_connector->dcp;
	struct apple_dcp *dcp = platform_get_drvdata(pdev);

	struct drm_device *dev = connector->dev;
	struct drm_display_mode *mode;
	int i;

	for (i = 0; i < dcp->nr_modes; ++i) {
		mode = drm_mode_duplicate(dev, &dcp->modes[i].mode);

		if (!mode) {
			dev_err(dev->dev, "Failed to duplicate display mode\n");
			return 0;
		}

		drm_mode_probed_add(connector, mode);
	}

	if (dcp->nr_modes && dcp->dcpavserv.enabled &&
	    !apple_connector->drm_edid) {
		const struct drm_edid *edid;
		edid = dcpavserv_copy_edid(dcp->dcpavserv.service);
		if (IS_ERR_OR_NULL(edid)) {
			dev_info(dcp->dev, "copy_edid failed: %pe\n", edid);
		} else {
			drm_edid_free(apple_connector->drm_edid);
			apple_connector->drm_edid = edid;
		}
	}
	if (dcp->nr_modes && apple_connector->drm_edid)
		drm_edid_connector_update(connector, apple_connector->drm_edid);

	return dcp->nr_modes;
}

/* The user may own drm_display_mode, so we need to search for our copy */
struct dcp_display_mode *lookup_mode(struct apple_dcp *dcp,
					    const struct drm_display_mode *mode)
{
	int i;

	for (i = 0; i < dcp->nr_modes; ++i) {
		if (drm_mode_match(mode, &dcp->modes[i].mode,
				   DRM_MODE_MATCH_TIMINGS |
					   DRM_MODE_MATCH_CLOCK))
			return &dcp->modes[i];
	}

	return NULL;
}

enum drm_mode_status dcp_mode_valid(struct drm_connector *connector,
				    const struct drm_display_mode *mode)
{
	struct apple_connector *apple_connector = to_apple_connector(connector);
	struct platform_device *pdev = apple_connector->dcp;
	struct apple_dcp *dcp = platform_get_drvdata(pdev);

	return lookup_mode(dcp, mode) ? MODE_OK : MODE_BAD;
}

int dcp_crtc_atomic_modeset(struct drm_crtc *crtc,
			    struct drm_atomic_state *state)
{
	struct apple_crtc *apple_crtc = to_apple_crtc(crtc);
	struct apple_dcp *dcp = platform_get_drvdata(apple_crtc->dcp);
	struct drm_crtc_state *crtc_state;
	int ret = -EIO;
	bool modeset;

	crtc_state = drm_atomic_get_new_crtc_state(state, crtc);
	if (!crtc_state)
		return 0;

	modeset = drm_atomic_crtc_needs_modeset(crtc_state) || !dcp->valid_mode;

	if (!modeset)
		return 0;

	/* ignore no mode, poweroff is handled elsewhere */
	if (crtc_state->mode.hdisplay == 0 && crtc_state->mode.vdisplay == 0)
		return 0;

	switch (dcp->fw_compat) {
	case DCP_FIRMWARE_V_12_3:
		ret = iomfb_modeset_v12_3(dcp, crtc_state);
		break;
	case DCP_FIRMWARE_V_13_5:
		ret = iomfb_modeset_v13_3(dcp, crtc_state);
		break;
	case DCP_FIRMWARE_H17P:
		ret = iomfb_modeset_h17p(dcp, crtc_state);
		break;
	default:
		WARN_ONCE(true, "Unexpected firmware version: %u\n",
			  dcp->fw_compat);
		break;
	}

	return ret;
}

bool dcp_crtc_mode_fixup(struct drm_crtc *crtc,
			 const struct drm_display_mode *mode,
			 struct drm_display_mode *adjusted_mode)
{
	struct apple_crtc *apple_crtc = to_apple_crtc(crtc);
	struct platform_device *pdev = apple_crtc->dcp;
	struct apple_dcp *dcp = platform_get_drvdata(pdev);

	/* TODO: support synthesized modes through scaling */
	return lookup_mode(dcp, mode) != NULL;
}


void dcp_flush(struct drm_crtc *crtc, struct drm_atomic_state *state)
{
	struct platform_device *pdev = to_apple_crtc(crtc)->dcp;
	struct apple_dcp *dcp = platform_get_drvdata(pdev);

	if (dcp_channel_busy(&dcp->ch_cmd))
	{
		if (!dcp->ch_cmd.warned_busy) {
			dev_err(dcp->dev, "unexpected busy command channel\n");
			dcp->ch_cmd.warned_busy = true;
		}
		/* HACK: issue a delayed vblank event to avoid timeouts in
		 * drm_atomic_helper_wait_for_vblanks().
		 */
		schedule_work(&dcp->vblank_wq);
		return;
	} else if (dcp->ch_cmd.warned_busy) {
		dcp->ch_cmd.warned_busy = false;
	}

	switch (dcp->fw_compat) {
	case DCP_FIRMWARE_V_12_3:
		iomfb_flush_v12_3(dcp, crtc, state);
		break;
	case DCP_FIRMWARE_V_13_5:
		iomfb_flush_v13_3(dcp, crtc, state);
		break;
	case DCP_FIRMWARE_H17P:
		iomfb_flush_h17p(dcp, crtc, state);
		break;
	default:
		WARN_ONCE(true, "Unexpected firmware version: %u\n", dcp->fw_compat);
		break;
	}
}

static void iomfb_start(struct apple_dcp *dcp)
{
	switch (dcp->fw_compat) {
	case DCP_FIRMWARE_V_12_3:
		iomfb_start_v12_3(dcp);
		break;
	case DCP_FIRMWARE_V_13_5:
		iomfb_start_v13_3(dcp);
		break;
	case DCP_FIRMWARE_H17P:
		iomfb_start_h17p(dcp);
		break;
	default:
		WARN_ONCE(true, "Unexpected firmware version: %u\n", dcp->fw_compat);
		break;
	}
}

bool dcp_is_initialized(struct platform_device *pdev)
{
	struct apple_dcp *dcp = platform_get_drvdata(pdev);

	return dcp->active;
}

void iomfb_recv_msg(struct apple_dcp *dcp, u64 message)
{
	enum dcpep_type type = FIELD_GET(IOMFB_MESSAGE_TYPE, message);

	if (dcp->fw_compat == DCP_FIRMWARE_H17P) {
		dcp_link_recv_msg(dcp, message);
		return;
	}

	if (type == IOMFB_MESSAGE_TYPE_INITIALIZED)
		iomfb_start(dcp);
	else if (type == IOMFB_MESSAGE_TYPE_MSG)
		dcpep_got_msg(dcp, message);
	else
		dev_warn(dcp->dev, "Ignoring unknown message %llx\n", message);
}

int iomfb_start_rtkit(struct apple_dcp *dcp)
{
	dma_addr_t shmem_iova;

	if (dcp->fw_compat == DCP_FIRMWARE_H17P)
		return dcp_link_start_rtkit(dcp);

	dcp_start_rtkit_endpoint(dcp, IOMFB_ENDPOINT);

	dcp->shmem = dma_alloc_coherent(dcp->dev, DCP_SHMEM_SIZE, &shmem_iova,
					GFP_KERNEL);

	dcp_send_message(dcp, IOMFB_ENDPOINT, dcpep_set_shmem(shmem_iova));

	return 0;
}

void iomfb_shutdown(struct apple_dcp *dcp)
{
	/* We're going down */
	dcp->active = false;
	dcp->valid_mode = false;

	switch (dcp->fw_compat) {
	case DCP_FIRMWARE_V_12_3:
		iomfb_shutdown_v12_3(dcp);
		break;
	case DCP_FIRMWARE_V_13_5:
		iomfb_shutdown_v13_3(dcp);
		break;
	case DCP_FIRMWARE_H17P:
		iomfb_shutdown_h17p(dcp);
		break;
	default:
		WARN_ONCE(true, "Unexpected firmware version: %u\n", dcp->fw_compat);
		break;
	}

	if (dcp->shmem_inherited) {
		iounmap((__force void __iomem *)dcp->shmem);
		dcp->shmem = NULL;
		dcp->shmem_iova = 0;
		dcp->shmem_inherited = false;
	}
}
