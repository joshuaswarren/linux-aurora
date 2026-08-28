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
#include <linux/kref.h>
#include <linux/module.h>
#include <linux/of_device.h>
#include <linux/ratelimit.h>
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


/*
 * A476 payload.  macOS sends 1 (dcp-calls.log line 1648); we sent 0 on the
 * theory that A476 is set_dcp_clamshellstate and 1 meant "lid shut".  That
 * attribution is unverified -- A476's shape (in 4, out 0) equally fits
 * set_has_frame_swap_function(bool) -- and the black panel it appeared to fix
 * was really the surf_iova misalignment, so the evidence for 0 was
 * coincidental.  Follow the capture.
 */
u32 dcp_clamshell_state = 1;
module_param_named(clamshell_state, dcp_clamshell_state, uint, 0644);

bool dcp_send_modeset;   /* macOS never issues A413 */
module_param_named(send_modeset, dcp_send_modeset, bool, 0644);



/*
 * A353 / A352(0x70,0) / A438 x5 / A352(0x70,1) form one bracket: the A352 pair
 * parks and resumes the APT timing sequencer around the A438 config push
 * ("apt_normal_mode_changed: requesting transtition to auto mode" ->
 * set_run_mode_safe 4 -> 2 -> power_down_M3).  A438 makes our firmware take a
 * CPU exception, and opening the bracket without closing it leaves M3 powered
 * down, so the swap is accepted but never scans out.  Off = skip all of it and
 * leave the sequencer in normal mode.
 */
/*
 * The scanout engine reads through disp0_dart stream 0 (display-subsystem),
 * not through the DCP's own stream.  The only surface memory mapped in that
 * stream is the display carveout: reserved-memory/framebuffer@101eb4b8000
 * advertises iommu-addresses = <&dcp 0x1ea0000 ...>, <&display-subsystem
 * 0x1ea0000 ...>.  A GEM buffer's DVA exists only in the DCP domain, so a swap
 * naming one validates and retires while the scanout engine reads an address
 * it cannot translate -- a black panel with no visible fault, because the
 * DART's error IRQ is masked once the fault budget is spent.
 */
/*
 * macOS's full StartEP set; see iomfb_start_rtkit().  Off: bringing all sixteen
 * up made the firmware NMI (panic(app): app NMI, pidx: 1) before the first swap
 * could retire, so the endpoints past 0x2d/0x37 need to be introduced one at a
 * time rather than as a block.
 */

/*
 * Bitmask of the endpoints in 0x20..0x3f to start beyond 0x20/0x2d/0x37, bit n
 * meaning endpoint 0x20+n.  Starting macOS's whole set at once made the
 * firmware NMI, so bring them up one at a time: disp0 (0x23) is extra_eps=0x8.
 */
static unsigned int dcp_extra_eps;
module_param_named(extra_eps, dcp_extra_eps, uint, 0644);
MODULE_PARM_DESC(extra_eps, "bitmask of extra endpoints to start, bit n = 0x20+n");

static void dcp_start_extra_eps(struct apple_dcp *dcp)
{
	unsigned int n;

	for (n = 0; n < 32; n++) {
		if (!(dcp_extra_eps & BIT(n)))
			continue;
		dev_info(dcp->dev, "starting extra endpoint %#04x\n", 0x20 + n);
		dcp_start_rtkit_endpoint(dcp, 0x20 + n);
	}
}


bool dcp_park_display = true;
module_param_named(park_display, dcp_park_display, bool, 0644);
MODULE_PARM_DESC(park_display, "H17P: power the display down after boot until a modeset");

bool dcp_defer_client_open;
module_param_named(defer_client_open, dcp_defer_client_open, bool, 0644);
MODULE_PARM_DESC(defer_client_open, "H17P: do not send first_client_open during boot");

static bool skip_hotplug;
module_param(skip_hotplug, bool, 0644);
MODULE_PARM_DESC(skip_hotplug, "do not raise the DRM hotplug event (bring-up bisect)");

static int dcp_trace_budget;
module_param_named(trace_rpc, dcp_trace_budget, int, 0644);
static bool dcp_trace_calls;
module_param_named(trace_calls, dcp_trace_calls, bool, 0644);
MODULE_PARM_DESC(trace_calls, "Trace AP calls after the bounded RPC trace budget is consumed");
MODULE_PARM_DESC(trace_rpc,
		 "log the first N DCP RPC messages (0 = off; the vblank callback\n"
		 "\t\t alone is 60/s, so anything non-zero fills the kernel ring)");

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

	/*
	 * A nested call goes on the callback channel, past the record we are
	 * answering.  This matches macOS: its A401 carries context 2 in the
	 * wire word while every call nested inside it carries context 0 with
	 * offset 0x40, which is exactly what this produces.
	 */
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
	/* Explicit AP tracing can outlast the callback/property trace budget.
	 * Leave it disabled for normal desktop operation.
	 */
	if (dcp_trace_budget > 0 || dcp_trace_calls) {
		if (dcp_trace_budget > 0)
			dcp_trace_budget--;
		dev_info(dcp->dev, "CALL %.4s ctx=%x off=%#x in=%u out=%u\n",
			 call->tag, context, offset, in_len, out_len);
	}


	memcpy(ch->tag[depth], call->tag, sizeof(ch->tag[depth]));
	ch->pushed_at[depth] = jiffies;
	if (context == DCP_CONTEXT_CMD && depth == 0) {
		dcp->warned_stall = false;
		mod_delayed_work(system_wq, &dcp->stall_wq,
				 msecs_to_jiffies(1000));
	}
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

	dma_wmb();
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
	u8 depth;
	u64 reply;

	if (!ch || !ch->depth) {
		dev_warn(dcp->dev, "DCPLink callback reply without an active call\n");
		return;
	}

	depth = dcp_pop_depth(&ch->depth);
	reply = apple_dcp_link_rpc_reply(ch->link_message[depth]);
	ch->link_message[depth] = 0;
	dma_wmb();
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

	dma_rmb();
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
	dma_rmb();
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
	u8 type = message & GENMASK_ULL(1, 0);

	if (type == APPLE_DCP_LINK_MSG_READY) {
		if (apple_dcp_link_ready_reply(message, &dcp->link_version,
					       &dcp->link_firmware_hash))
			return;
		if (dcp->link_ready)
			return;
		dcp->link_ready = true;
		dev_info(dcp->dev, "DCPLink ready: version %u hash %#x\n",
			 dcp->link_version, dcp->link_firmware_hash);
		iomfb_start_h17p(dcp);
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

	apple_rtkit_start_ep(dcp->rtk, APPLE_DCP_LINK_ENDPOINT);
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

	dcp->link_ready = false;
	memset(dcp->link_top, 0, sizeof(dcp->link_top));
	dma_wmb();
	dcp_send_message(dcp, APPLE_DCP_LINK_ENDPOINT, init_message);
	dcp_send_message(dcp, APPLE_DCP_LINK_ENDPOINT,
			 apple_dcp_link_ready_message());
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

	if (skip_hotplug) {
		dev_info(dcp->dev, "skip_hotplug set; not notifying DRM\n");
		return;
	}

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

	if (dcp_trace_budget > 0 && tag != 300) {
		dcp_trace_budget--;
		char nm[41] = "";
		const char *ib = (const char *)(hdr + 1);
		u32 k;

		/* most property callbacks lead with a printable key */
		for (k = 0; k < hdr->in_len && k < sizeof(nm) - 1; k++) {
			if (ib[k] == '\0')
				break;
			nm[k] = (ib[k] >= 0x20 && ib[k] < 0x7f) ? ib[k] : '.';
		}
		nm[k] = '\0';
		dev_info(dev, "CB %c%c%c%c ctx=%x in=%u out=%u %s\n",
			 hdr->tag[3], hdr->tag[2], hdr->tag[1], hdr->tag[0],
			 context, hdr->in_len, hdr->out_len, nm);
		dev_info(dev, "CB input %*ph\n", min_t(u32, hdr->in_len, 32), ib);
	}

	in = data + sizeof(*hdr);
	out = in + hdr->in_len;
	if (dcp->hw.adopt_live_session && length >= sizeof(*hdr)) {
		struct dcp_callback_trace *r;
		unsigned long flags;
		u32 size = min_t(u32, hdr->in_len, length - sizeof(*hdr));

		spin_lock_irqsave(&dcp->callback_trace_lock, flags);
		r = &dcp->callback_trace[dcp->callback_trace_count % DCP_CALLBACK_TRACE_COUNT];
		memset(r, 0, sizeof(*r));
		memcpy(r->tag, hdr->tag, sizeof(r->tag));
		r->in_len = hdr->in_len;
		r->out_len = hdr->out_len;
		memcpy(r->input, in, min_t(u32, size, sizeof(r->input)));
		dcp->callback_trace_count++;
		spin_unlock_irqrestore(&dcp->callback_trace_lock, flags);
	}
	/* Keep the suspect frequency callback visible even when the startup
	 * property traffic has consumed the optional RPC trace budget.
	 */
	if (tag == 408 && dcp->hw.adopt_live_session)
		dev_info(dev, "D408 clock request in=%u out=%u data=%*ph\n",
			 hdr->in_len, hdr->out_len, min_t(u32, hdr->in_len, 16), in);

	if (tag < 0 || tag >= IOMFB_MAX_CB || !dcp->cb_handlers ||
	    !dcp->cb_handlers[tag]) {
		dev_warn(dev, "received unknown callback %c%c%c%c\n",
			 hdr->tag[3], hdr->tag[2], hdr->tag[1], hdr->tag[0]);
		/*
		 * Leaving a callback unanswered wedges the coprocessor: it
		 * waits for the ack forever and the outer call never returns.
		 * On H17P answer it with a zeroed output instead, so the
		 * warning above names the tag we still have to implement
		 * rather than costing a reboot to discover one tag at a time.
		 */
		if (dcp->fw_compat == DCP_FIRMWARE_H17P) {
			if (hdr->out_len)
				memset(out, 0, hdr->out_len);
			depth = dcp_push_depth(&ch->depth);
			ch->output[depth] = out;
			ch->end[depth] = offset +
					 ALIGN(length, DCP_PACKET_ALIGNMENT);
			dcp_ack(dcp, context);
		}
		return;
	}

	// TODO: verify that in_len and out_len match our prototypes
	// for now just clear the out data to have at least consistent results
	if (hdr->out_len)
		memset(out, 0, hdr->out_len);

	depth = dcp_push_depth(&ch->depth);
	ch->output[depth] = out;
	ch->end[depth] = offset + ALIGN(length, DCP_PACKET_ALIGNMENT);
	ch->tag[depth][0] = hdr->tag[3];
	ch->tag[depth][1] = hdr->tag[2];
	ch->tag[depth][2] = hdr->tag[1];
	ch->tag[depth][3] = hdr->tag[0];
	ch->pushed_at[depth] = jiffies;

	{
		bool acked = dcp->cb_handlers[tag](dcp, tag, out, in);

		if (dcp_trace_budget > 0 && hdr->out_len && tag != 300) {
			dcp_trace_budget--;
			dev_info(dev, "RPL %c%c%c%c %*ph\n",
				 hdr->tag[3], hdr->tag[2], hdr->tag[1],
				 hdr->tag[0],
				 (int)min_t(u32, hdr->out_len, 16), out);
		}
		if (acked)
			dcp_ack(dcp, context);
	}
}

static void dcpep_handle_ack(struct apple_dcp *dcp, enum dcp_context_id context)
{
	struct dcp_channel *ch = dcp_get_channel(dcp, context);
	void *cookie, *out;
	dcp_callback_t cb;

	if (!ch || !ch->depth) {
		dev_warn(dcp->dev, "ignoring ack on empty/invalid context %X\n", context);
		return;
	}

	dcp_pop_depth(&ch->depth);

	cb = ch->callbacks[ch->depth];
	cookie = ch->cookies[ch->depth];
	/* ACKs carry no payload and have offset zero, including nested calls.
	 * Their output is in the original AP command record, not the receive
	 * callback region selected by context0. dcp_push saved that address.
	 */
	out = ch->output[ch->depth];

	ch->callbacks[ch->depth] = NULL;
	ch->cookies[ch->depth] = NULL;
	ch->output[ch->depth] = NULL;

	if (cb)
		cb(dcp, out, cookie);
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

	if (dcp_trace_budget > 0) {
		dcp_trace_budget--;
		dev_info(dcp->dev, "RX %#018llx ctx=%x off=%#x len=%u ack=%d\n",
			 message, ctx_id, offset, length,
			 !!FIELD_GET(IOMFB_MSG_ACK, message));
	}

	channel_offset = dcp_channel_offset(ctx_id);

	if (channel_offset < 0) {
		dev_warn(dcp->dev, "invalid context received %u\n", ctx_id);
		return;
	}

	data = dcp->shmem + channel_offset + offset;

	if (FIELD_GET(IOMFB_MSG_ACK, message))
		dcpep_handle_ack(dcp, ctx_id);
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
			u8 d = dcp->ch_cmd.depth;

			dev_err(dcp->dev,
				"unexpected busy command channel: depth %u, oldest outstanding %.4s pushed %u ms ago\n",
				d, d ? dcp->ch_cmd.tag[d - 1] : "----",
				d ? jiffies_to_msecs(jiffies -
						     dcp->ch_cmd.pushed_at[d - 1])
				  : 0);
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
	} else if (type == IOMFB_MESSAGE_TYPE_MSG)
		dcpep_got_msg(dcp, message);
	else
		dev_warn(dcp->dev, "Ignoring unknown message %llx\n", message);
}

int iomfb_start_rtkit(struct apple_dcp *dcp)
{
	dma_addr_t shmem_iova;

	if (dcp->fw_compat == DCP_FIRMWARE_H17P)
		return dcp_link_start_rtkit(dcp);

	apple_rtkit_start_ep(dcp->rtk, IOMFB_ENDPOINT);

	dcp->shmem = dma_alloc_coherent(dcp->dev, DCP_SHMEM_SIZE, &shmem_iova,
					GFP_KERNEL);
	if (!dcp->shmem)
		return -ENOMEM;

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
}
