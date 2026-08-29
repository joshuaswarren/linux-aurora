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

#include "afk.h"
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
static int dcp_link_ack(struct apple_dcp *dcp,
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
	/*
	 * AppleDCPLinkService releases RTBuddy ownership after updating shared
	 * memory. For the inherited Normal-NC mapping there are no AP cache lines
	 * to clean, but the stores must reach the point of coherency before a
	 * mailbox message can make the DCP consume them. dma_wmb() only orders the
	 * stores; wmb() also completes them before the message is enqueued.
	 */
	wmb();
}

static void dcp_link_log_init(struct apple_dcp *dcp,
			      const struct apple_dcp_link_init_desc *desc,
			      u64 message)
{
	dev_info(dcp->dev,
		 "DCPLink INIT %#llx descriptor shared %#llx flags %#x version %u\n",
		 message, le64_to_cpu(READ_ONCE(desc->shared_dva)),
		 le32_to_cpu(READ_ONCE(desc->flags)),
		 le32_to_cpu(READ_ONCE(desc->version)));
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

static int dcp_link_push_context(struct apple_dcp *dcp,
				 enum dcp_context_id context,
				 bool new_stream,
				 const struct dcp_method_entry *call, u32 in_len,
				 u32 out_len, void *data, dcp_callback_t cb,
				 void *cookie)
{
	struct dcp_channel *ch = dcp_get_channel(dcp, context);
	struct apple_dcp_link_stream_layout layout;
	struct apple_dcp_link_rpc_header *header;
	bool remote = dcp_link_context_is_remote(context);
	u8 stream = dcp_link_context_stream(context);
	u32 *top = &dcp->link_top[remote][stream];
	u32 next_top;
	u32 old_top;
	u32 offset;
	u32 payload_size;
	u32 call_tag;
	u64 message;
	u8 depth;
	void *buffer;
	int ret;

	if (!ch)
		return -EINVAL;

	depth = dcp_push_depth(&ch->depth);
	old_top = *top;
	call_tag = dcp_link_call_tag(call);
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
	apple_dcp_link_rpc_header_encode(header, call_tag,
					 in_len, out_len);
	if (in_len)
		memcpy(header + 1, data, in_len);
	if (out_len)
		memset((u8 *)(header + 1) + in_len, 0, out_len);

	/* The callback workloop is bound to an existing remote stream. */
	ret = apple_dcp_link_ap_call_message(offset, payload_size, stream,
					     remote, new_stream, &message);
	if (ret)
		goto err_pop;

	ch->callbacks[depth] = cb;
	ch->cookies[depth] = cookie;
	ch->output[depth] = (u8 *)(header + 1) + in_len;
	ch->link_message[depth] = message;
	ch->link_size[depth] = payload_size;
	*top = next_top;

	dcp_link_sync_for_device();
	if (remote)
		dev_info(dcp->dev,
			 "DCPLink callback-stream %.4s TX %#llx base %#x offset %#x size %#x top %#x->%#x depth %u header %#x/%u/%u\n",
			 call->tag, message, layout.local_offset, offset,
			 payload_size, old_top, next_top, depth, call_tag,
			 in_len, out_len);
	if (call_tag == APPLE_DCP_H17P_CALL_START_SIGNAL) {
		afk_log_tx_state(dcp->systemep, "before DCPLink A401");
		dev_info(dcp->dev,
			 "DCPLink A401 TX %#llx base %#x offset %#x size %#x top %#x->%#x depth %u header %#x/%u/%u\n",
			 message, layout.local_offset, offset, payload_size,
			 old_top, next_top, depth, call_tag, in_len, out_len);
	}
	ret = dcp_send_message_sync(dcp, APPLE_DCP_LINK_ENDPOINT, message);
	if (call_tag == APPLE_DCP_H17P_CALL_PMU_SERVICE_MATCHED ||
	    call_tag == APPLE_DCP_H17P_CALL_BACKLIGHT_SERVICE_MATCHED)
		dev_info(dcp->dev,
			 "DCPLink %.4s TX %#llx base %#x offset %#x size %#x depth %u mailbox result %d\n",
			 call->tag, message, layout.local_offset, offset,
			 payload_size, depth, ret);
	if (ret) {
		dev_err(dcp->dev,
			"DCPLink %.4s mailbox send failed: %d\n", call->tag, ret);
		*top = old_top;
		ch->callbacks[depth] = NULL;
		ch->cookies[depth] = NULL;
		ch->output[depth] = NULL;
		ch->link_message[depth] = 0;
		ch->link_size[depth] = 0;
		dcp_pop_depth(&ch->depth);
		return ret;
	}
	if (remote) {
		dcp_link_sync_for_cpu();
		dev_info(dcp->dev,
			 "DCPLink callback-stream %.4s mailbox send complete payload %*phN\n",
			 call->tag, min_t(u32, payload_size, 32), header);
	}
	return 0;

err_pop:
	dcp_pop_depth(&ch->depth);
	dev_err(dcp->dev, "DCPLink call %.4s could not be queued: %d\n",
		call->tag, ret);
	return ret;
}

static void dcp_link_push(struct apple_dcp *dcp, bool oob,
			  const struct dcp_method_entry *call, u32 in_len,
			  u32 out_len, void *data, dcp_callback_t cb,
			  void *cookie)
{
	dcp_link_push_context(dcp, dcp_call_context(dcp, oob), false, call,
			      in_len, out_len, data, cb, cookie);
}

static const struct dcp_method_entry dcp_link_temperature_hint = {
	.name = "vi_set_temperature_hint",
	.tag = { 'A', '3', '8', '6' },
};

static const struct dcp_method_entry dcp_link_pmu_service_matched = {
	.name = "pmu_service_matched",
	.tag = { 'A', '1', '3', '1' },
};

static const struct dcp_method_entry dcp_link_backlight_service_matched = {
	.name = "backlight_service_matched",
	.tag = { 'A', '1', '3', '2' },
};

enum dcp_link_service_match_bit {
	DCP_LINK_SERVICE_MATCH_TEMPERATURE,
	DCP_LINK_SERVICE_MATCH_PMU,
	DCP_LINK_SERVICE_MATCH_BACKLIGHT,
	DCP_LINK_SERVICE_MATCH_INFLIGHT,
};

#define DCP_LINK_SERVICE_NOTIFY_RETRIES 256

static bool dcp_link_service_match_pending(struct apple_dcp *dcp)
{
	return test_bit(DCP_LINK_SERVICE_MATCH_TEMPERATURE,
			&dcp->link_service_match_state) ||
	       test_bit(DCP_LINK_SERVICE_MATCH_PMU,
			&dcp->link_service_match_state) ||
	       test_bit(DCP_LINK_SERVICE_MATCH_BACKLIGHT,
			&dcp->link_service_match_state);
}

static void dcp_link_service_match_done(struct apple_dcp *dcp)
{
	clear_bit(DCP_LINK_SERVICE_MATCH_INFLIGHT,
		  &dcp->link_service_match_state);
	if (dcp_link_service_match_pending(dcp))
		schedule_work(&dcp->link_service_match_work);
}

static void dcp_link_renotify_service(struct apple_dcp *dcp,
				      const struct dcp_method_entry *call)
{
	struct dcp_channel *ch = &dcp->ch_oobcmd;
	u64 message;
	u8 depth;
	int attempt;
	int busy = 0;
	int sent = 1;
	int ret;

	depth = READ_ONCE(ch->depth);
	if (!depth)
		return;
	message = READ_ONCE(ch->link_message[depth - 1]);

	for (attempt = 1; attempt <= DCP_LINK_SERVICE_NOTIFY_RETRIES;
	     attempt++) {
		usleep_range(900, 1100);
		depth = READ_ONCE(ch->depth);
		if (!depth || READ_ONCE(ch->link_message[depth - 1]) != message) {
			dev_info(dcp->dev,
				 "DCPLink %.4s consumed after %d doorbell notifications\n",
				 call->tag, attempt);
			return;
		}

		dcp_link_sync_for_device();
		ret = dcp_try_send_message_if_empty(dcp,
						    APPLE_DCP_LINK_ENDPOINT,
						    message);
		if (ret == -EAGAIN) {
			busy++;
			continue;
		}
		if (ret) {
			dev_err(dcp->dev,
				"DCPLink %.4s re-notify %d failed: %d\n",
				call->tag, attempt, ret);
			return;
		}
		sent++;
		if (sent == 2)
			dev_info(dcp->dev,
				 "DCPLink %.4s still pending; polling and re-notifying an empty mailbox every 1 ms\n",
				 call->tag);
	}

	dev_warn(dcp->dev,
		 "DCPLink %.4s remained pending after %d doorbells (%d occupied polls)\n",
		 call->tag, sent, busy);
}

static void dcp_link_service_match_complete(struct apple_dcp *dcp, void *out,
					    void *cookie)
{
	const struct dcp_method_entry *call = cookie;

	dev_info(dcp->dev, "DCPLink %.4s reply %#x\n", call->tag,
		 le32_to_cpup((__le32 *)out));
	dcp_link_service_match_done(dcp);
}

static void dcp_link_temperature_hint_complete(struct apple_dcp *dcp, void *out,
					       void *cookie)
{
	int ret;

	dev_info(dcp->dev, "DCPLink A386 reply %#x\n",
		 le32_to_cpup((__le32 *)out));
	ret = dcp_link_ack(dcp, DCP_CONTEXT_CB);
	dev_info(dcp->dev,
		 "DCPLink A386 completed deferred D100 ACK result %d\n", ret);
	dcp_link_service_match_done(dcp);
}

static void dcp_link_service_match_work(struct work_struct *work)
{
	struct apple_dcp *dcp =
		container_of(work, struct apple_dcp, link_service_match_work);
	const struct dcp_method_entry *call;
	dcp_callback_t complete;
	int ret;

	if (test_and_set_bit(DCP_LINK_SERVICE_MATCH_INFLIGHT,
			     &dcp->link_service_match_state))
		return;

	if (test_and_clear_bit(DCP_LINK_SERVICE_MATCH_TEMPERATURE,
			       &dcp->link_service_match_state)) {
		call = &dcp_link_temperature_hint;
		complete = dcp_link_temperature_hint_complete;
	} else if (test_and_clear_bit(DCP_LINK_SERVICE_MATCH_PMU,
				      &dcp->link_service_match_state)) {
		call = &dcp_link_pmu_service_matched;
		complete = dcp_link_service_match_complete;
	} else if (test_and_clear_bit(DCP_LINK_SERVICE_MATCH_BACKLIGHT,
				      &dcp->link_service_match_state)) {
		call = &dcp_link_backlight_service_matched;
		complete = dcp_link_service_match_complete;
	} else {
		clear_bit(DCP_LINK_SERVICE_MATCH_INFLIGHT,
			  &dcp->link_service_match_state);
		return;
	}

	/* IOService matching notifiers run outside the firmware callback gate. */
	ret = dcp_link_push_context(dcp, DCP_CONTEXT_OOBCMD, true, call, 0,
				    sizeof(__le32), NULL,
				    complete,
				    (void *)call);
	if (ret) {
		dev_err(dcp->dev,
			"DCPLink %.4s service notifier could not be queued: %d\n",
			call->tag, ret);
		dcp_link_service_match_done(dcp);
	} else if (call == &dcp_link_pmu_service_matched ||
		   call == &dcp_link_backlight_service_matched) {
		dcp_link_renotify_service(dcp, call);
	}
}

static void dcp_link_queue_service_match(struct apple_dcp *dcp, int tag)
{
	if (tag == 100)
		set_bit(DCP_LINK_SERVICE_MATCH_TEMPERATURE,
			&dcp->link_service_match_state);
	else if (tag == 206)
		set_bit(DCP_LINK_SERVICE_MATCH_PMU,
			&dcp->link_service_match_state);
	else if (tag == 207)
		set_bit(DCP_LINK_SERVICE_MATCH_BACKLIGHT,
			&dcp->link_service_match_state);
	else
		return;

	schedule_work(&dcp->link_service_match_work);
}

bool dcp_link_defer_service_match(struct apple_dcp *dcp, int tag, void *out,
				  void *in)
{
	trace_iomfb_callback(dcp, tag, __func__);
	if (tag == 100) {
		dcp_link_queue_service_match(dcp, tag);
		return false;
	}

	/* Native D206/D207 return true before their IOService notifier runs. */
	*(__le32 *)out = cpu_to_le32(1);
	return true;
}

static int dcp_link_ack(struct apple_dcp *dcp,
			enum dcp_context_id context)
{
	struct dcp_channel *ch = dcp_get_channel(dcp, context);
	u64 request;
	u8 depth;
	u64 reply;
	int ret;

	if (!ch || !ch->depth) {
		dev_warn(dcp->dev, "DCPLink callback reply without an active call\n");
		return -EINVAL;
	}

	depth = dcp_pop_depth(&ch->depth);
	request = ch->link_message[depth];
	dcp_link_sync_for_device();
	reply = apple_dcp_link_rpc_reply(request);
	ch->link_message[depth] = 0;
	ret = dcp_send_message_sync(dcp, APPLE_DCP_LINK_ENDPOINT, reply);
	if (ret)
		dev_err(dcp->dev,
			"DCPLink callback ACK %#llx mailbox send failed: %d\n",
			reply, ret);

	return ret;
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
	struct apple_dcp_link_stream_layout request_layout;
	struct apple_dcp_link_rpc_header *request_header;
	bool remote = apple_dcp_link_message_is_remote(message);
	bool log_reply = dcp->link_reply_log_count < 256;
	u8 stream = apple_dcp_link_message_stream(message);
	u16 sequence = dcp->link_reply_log_count;
	u32 request_call = 0;
	u32 request_offset = 0;
	u32 top_before = 0;
	u64 request = 0;
	u32 *top;
	dcp_callback_t cb;
	void *cookie;
	u8 depth;
	int ret;

	if (stream >= APPLE_DCP_LINK_STREAMS_PER_SIDE) {
		if (log_reply) {
			dcp->link_reply_log_count++;
			dev_info(dcp->dev,
				 "DCPLink reply %u RX %#llx stream %u remote %u invalid\n",
				 sequence, message, stream, remote);
		}
		dev_warn(dcp->dev, "DCPLink reply stream %u is invalid\n", stream);
		return;
	}
	top = &dcp->link_top[remote][stream];
	top_before = *top;
	if (ch->depth) {
		request = ch->link_message[ch->depth - 1];
		request_offset = FIELD_GET(APPLE_DCP_LINK_MSG_OFFSET, request);
		if (!apple_dcp_link_stream_layout(
			    APPLE_DCP_LINK_SIDE_AP,
			    apple_dcp_link_message_stream(request),
			    apple_dcp_link_message_is_remote(request),
			    &request_layout) &&
		    request_offset <= request_layout.capacity -
					      sizeof(*request_header)) {
			dcp_link_sync_for_cpu();
			request_header = dcp->shmem + request_layout.local_offset +
					 request_offset;
			request_call = le32_to_cpu(request_header->call);
		}
	}
	if (log_reply) {
		dcp->link_reply_log_count++;
		dev_info(dcp->dev,
			 "DCPLink reply %u RX %#llx stream %u remote %u channel %s depth %u request %#llx call %#x top %#x\n",
			 sequence, message, stream, remote,
			 stream == 1 ? (remote ? "oobcb" : "oobcmd") :
				       (remote ? "cb" : "cmd"),
			 ch->depth, request, request_call, top_before);
	}

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
	if (cb && request_call == APPLE_DCP_H17P_CALL_START_SIGNAL)
		dev_info(dcp->dev,
			 "DCPLink A401 invoking completion %ps output %4phN\n",
			 cb, ch->output[depth]);
	if (cb)
		cb(dcp, ch->output[depth], cookie);
}

static void dcp_link_handle_callback(struct apple_dcp *dcp, u64 message)
{
	struct apple_dcp_link_stream_layout layout;
	struct apple_dcp_link_method method;
	struct apple_dcp_link_rpc_header *header;
	struct apple_dcp_link_rpc_view view;
	struct dcp_channel *ch = &dcp->ch_cb;
	iomfb_cb_handler handler;
	bool handled;
	bool log_callback = dcp->link_rx_log_count < 256;
	bool remote = apple_dcp_link_message_is_remote(message);
	void *buffer;
	char wire_tag[4];
	u8 stream = apple_dcp_link_message_stream(message);
	u16 sequence = dcp->link_rx_log_count;
	u32 offset = FIELD_GET(APPLE_DCP_LINK_MSG_OFFSET, message);
	u32 total_size = FIELD_GET(APPLE_DCP_LINK_MSG_SIZE, message);
	u64 reply = apple_dcp_link_rpc_reply(message);
	u8 depth;
	int ack_ret;
	int tag;
	int ret;

	ret = apple_dcp_link_stream_layout(APPLE_DCP_LINK_SIDE_AP, stream, remote,
					   &layout);
	if (ret) {
		dcp_send_message(dcp, APPLE_DCP_LINK_ENDPOINT,
				 apple_dcp_link_rpc_reply(message));
		return;
	}
	dcp_link_sync_for_cpu();
	buffer = dcp->shmem + layout.remote_offset;
	if (log_callback)
		dcp->link_rx_log_count++;
	if (log_callback && offset > layout.capacity - sizeof(*header))
		dev_info(dcp->dev,
			 "DCPLink callback %u RX %#llx stream %u remote %u base %#x offset %#x size %#x header out of range\n",
			 sequence, message, stream, remote, layout.remote_offset,
			 offset, total_size);
	ret = apple_dcp_link_rpc_decode(message, buffer,
					layout.capacity, &view);
	if (ret) {
		if (log_callback &&
		    offset <= layout.capacity - sizeof(*header)) {
			header = buffer + offset;
			dev_info(dcp->dev,
				 "DCPLink callback %u RX %#llx stream %u remote %u base %#x offset %#x size %#x malformed header %#x/%u/%u ack %#llx\n",
				 sequence, message, stream, remote,
				 layout.remote_offset, offset, total_size,
				 le32_to_cpu(header->call),
				 le32_to_cpu(header->input_size),
				 le32_to_cpu(header->output_size), reply);
		}
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
		if (log_callback)
			dev_info(dcp->dev,
				 "DCPLink callback %u RX %#llx D%03d base %#x offset %#x size %#x in/out %u/%u input %*phN handler none ack %#llx\n",
				 sequence, message, tag, layout.remote_offset,
				 view.offset, view.total_size, view.input_size,
				 view.output_size,
				 (int)min_t(u32, view.input_size, 72), view.input,
				 reply);
		dev_warn(dcp->dev, "DCPLink callback D%03d has no handler\n", tag);
		dcp_link_ack(dcp, DCP_CONTEXT_CB);
		return;
	}

	handler = dcp->cb_handlers[tag];
	handled = handler(dcp, tag, view.output, (void *)view.input);
	if (log_callback) {
		if (view.output_size)
			dev_info(dcp->dev,
				 "DCPLink callback %u RX %#llx D%03d base %#x offset %#x size %#x in/out %u/%u input %*phN handler %ps result %u out %*phN ack %#llx\n",
				 sequence, message, tag, layout.remote_offset,
				 view.offset, view.total_size, view.input_size,
				 view.output_size,
				 (int)min_t(u32, view.input_size, 72), view.input,
				 handler, handled,
				 min_t(u32, view.output_size, 8), view.output,
				 handled ? reply : 0);
		else
			dev_info(dcp->dev,
				 "DCPLink callback %u RX %#llx D%03d base %#x offset %#x size %#x in/out %u/%u input %*phN handler %ps result %u out - ack %#llx\n",
				 sequence, message, tag, layout.remote_offset,
				 view.offset, view.total_size, view.input_size,
				 view.output_size,
				 (int)min_t(u32, view.input_size, 72), view.input,
				 handler, handled,
				 handled ? reply : 0);
	}
	if (handled) {
		ack_ret = dcp_link_ack(dcp, DCP_CONTEXT_CB);
		if (log_callback && sequence >= 165)
			dev_info(dcp->dev,
				 "DCPLink callback %u D%03d ACK %#llx send result %d\n",
				 sequence, tag, reply, ack_ret);
		if (!ack_ret && (tag == 206 || tag == 207)) {
			dev_info(dcp->dev,
				 "DCPLink D%03d ACKed; queueing asynchronous service notifier\n",
				 tag);
			dcp_link_queue_service_match(dcp, tag);
		}
	}
}

static int dcp_link_publish_init(struct apple_dcp *dcp, bool firmware_rendezvous)
{
	struct apple_dcp_link_init_desc *desc;
	int ret = 0;

	mutex_lock(&dcp->link_init_lock);
	if (dcp->link_state != APPLE_DCP_LINK_WAIT_FIRMWARE_INIT)
		goto out_unlock;

	desc = dcp->shmem;
	apple_dcp_link_init_desc_encode(desc, 0, false);
	dcp_link_sync_for_device();
	dcp_link_log_init(dcp, desc, dcp->link_init_message);
	ret = dcp_send_message_sync(dcp, APPLE_DCP_LINK_ENDPOINT,
				    dcp->link_init_message);
	if (ret) {
		dev_err(dcp->dev, "DCPLink INIT mailbox send failed: %d\n", ret);
		goto out_unlock;
	}

	dcp->link_state = APPLE_DCP_LINK_WAIT_READY;
	if (firmware_rendezvous)
		dev_info(dcp->dev,
			 "DCPLink type0 received; descriptor published and host INIT sent\n");
	else
		dev_info(dcp->dev,
			 "DCPLink type0 absent after 100 ms; descriptor published and host INIT sent\n");

out_unlock:
	mutex_unlock(&dcp->link_init_lock);
	return ret;
}

static void dcp_link_recv_msg(struct apple_dcp *dcp, u64 message)
{
	enum apple_dcp_link_handshake_action action;
	enum apple_dcp_link_state old_state = dcp->link_state;
	u8 type = message & GENMASK_ULL(1, 0);

	action = apple_dcp_link_handshake_step(&dcp->link_state, message);
	if (action == APPLE_DCP_LINK_HANDSHAKE_SEND_INIT) {
		dcp_link_publish_init(dcp, true);
		return;
	}

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
		dev_dbg(dcp->dev, "duplicate DCPLink firmware rendezvous\n");
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

	INIT_WORK(&dcp->link_service_match_work, dcp_link_service_match_work);
	dcp->link_service_match_state = 0;

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
		u64 firmware_rpc_dva;
		u64 aperture_start;
		u64 aperture_end;
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
		if (!domain->geometry.force_aperture)
			return dev_err_probe(dcp->dev, -EINVAL,
					     "DCP IOMMU aperture is unavailable\n");
		aperture_start = domain->geometry.aperture_start;
		aperture_end = domain->geometry.aperture_end;
		ret = apple_dcp_link_firmware_dva(rpc_dva, aperture_start,
						  aperture_end, &firmware_rpc_dva);
		if (ret)
			return dev_err_probe(dcp->dev, ret,
					     "DCPLink RPC DVA is outside the firmware aperture\n");

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
			ret = apple_dcp_link_init_message(firmware_rpc_dva,
							  &captured_message);
			if (ret)
				goto err_unmap_inherited;
			dcp->link_init_message = captured_message;
			dcp->link_state = APPLE_DCP_LINK_WAIT_FIRMWARE_INIT;
			dcp->link_version = 0;
			dcp->link_firmware_hash = 0;
			dcp->link_endpoint_started = true;
			ret = dcp_start_rtkit_endpoint(dcp,
						       APPLE_DCP_LINK_ENDPOINT);
			if (ret) {
				dcp->link_endpoint_started = false;
				goto err_unmap_inherited;
			}
			dev_info(dcp->dev,
				 "adopted unused DCPLink RPC mapping local DVA %#llx firmware DVA %#llx PA %#llx; waiting for firmware rendezvous\n",
				 rpc_dva, firmware_rpc_dva, rpc_pa);
			msleep(100);
			ret = dcp_link_publish_init(dcp, false);
			if (ret)
				goto err_unmap_inherited;
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
		if (descriptor_zero)
			apple_dcp_link_init_desc_encode(desc, 0, false);
		ret = apple_dcp_link_init_message(firmware_rpc_dva,
						  &captured_message);
		if (ret)
			goto err_unmap_inherited;
		dcp->link_state = APPLE_DCP_LINK_WAIT_READY;
		dcp->link_init_message = captured_message;
		dcp->link_version = 0;
		dcp->link_firmware_hash = 0;
		dcp_link_sync_for_device();
		dcp_link_log_init(dcp, desc, captured_message);
		ret = dcp_send_message_sync(dcp, APPLE_DCP_LINK_ENDPOINT, captured_message);
		if (ret) {
			dev_err(dcp->dev,
				"inherited DCPLink INIT mailbox send failed: %d\n",
				ret);
			goto err_unmap_inherited;
		}
		dev_info(dcp->dev,
			 "adopted DCPLink firmware-init phase local DVA %#llx firmware DVA %#llx; inherited RPC INIT queued\n",
			 rpc_dva, firmware_rpc_dva);
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
	ret = apple_dcp_link_init_message(dcp->shmem_iova, &init_message);
	if (ret)
		return ret;

	dcp->link_init_message = init_message;
	dcp->link_state = APPLE_DCP_LINK_WAIT_FIRMWARE_INIT;
	memset(dcp->link_top, 0, sizeof(dcp->link_top));

	dcp->link_endpoint_started = true;
	ret = dcp_start_rtkit_endpoint(dcp, APPLE_DCP_LINK_ENDPOINT);
	if (ret) {
		dcp->link_endpoint_started = false;
		return ret;
	}

	dev_info(dcp->dev,
		 "DCPLink endpoint started; waiting for firmware rendezvous\n");
	msleep(100);
	ret = dcp_link_publish_init(dcp, false);
	if (ret)
		return ret;
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

	dcp_start_rtkit_endpoint(dcp, IOMFB_ENDPOINT);

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
	if (dcp->fw_compat == DCP_FIRMWARE_H17P)
		cancel_work_sync(&dcp->link_service_match_work);

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
