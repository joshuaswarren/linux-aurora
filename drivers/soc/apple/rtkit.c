// SPDX-License-Identifier: GPL-2.0-only OR MIT
/*
 * Apple RTKit IPC library
 * Copyright (C) The Asahi Linux Contributors
 */

#include "rtkit-internal.h"
#include <linux/of.h>

static_assert(APPLE_RTKIT_EPMAP_SNAPSHOT_WORDS * 64 ==
	      APPLE_RTKIT_MAX_ENDPOINTS);

enum {
	APPLE_RTKIT_PWR_STATE_OFF = 0x00, /* power off, cannot be restarted */
	APPLE_RTKIT_PWR_STATE_SLEEP = 0x01, /* sleeping, can be restarted */
	APPLE_RTKIT_PWR_STATE_IDLE = 0x201, /* sleeping, retain state */
	APPLE_RTKIT_PWR_STATE_QUIESCED = 0x10, /* running but no communication */
	APPLE_RTKIT_PWR_STATE_ON = 0x20, /* normal operating state */
	APPLE_RTKIT_PWR_STATE_INIT = 0x220, /* init after starting the coproc */
};

enum {
	APPLE_RTKIT_EP_MGMT = 0,
	APPLE_RTKIT_EP_CRASHLOG = 1,
	APPLE_RTKIT_EP_SYSLOG = 2,
	APPLE_RTKIT_EP_DEBUG = 3,
	APPLE_RTKIT_EP_IOREPORT = 4,
	APPLE_RTKIT_EP_PMP_AUX = 7,
	APPLE_RTKIT_EP_OSLOG = 8,
	APPLE_RTKIT_EP_TRACEKIT = 0xa,
};

#define APPLE_RTKIT_MGMT_TYPE GENMASK_ULL(59, 52)

enum {
	APPLE_RTKIT_MGMT_HELLO = 1,
	APPLE_RTKIT_MGMT_HELLO_REPLY = 2,
	APPLE_RTKIT_MGMT_STARTEP = 5,
	APPLE_RTKIT_MGMT_SET_IOP_PWR_STATE = 6,
	APPLE_RTKIT_MGMT_SET_IOP_PWR_STATE_ACK = 7,
	APPLE_RTKIT_MGMT_EPMAP = 8,
	APPLE_RTKIT_MGMT_EPMAP_REPLY = 8,
	APPLE_RTKIT_MGMT_SET_AP_PWR_STATE = 0xb,
	APPLE_RTKIT_MGMT_SET_AP_PWR_STATE_ACK = 0xb,
	APPLE_RTKIT_MGMT_IOP_IDLE = 0xc,
};

#define APPLE_RTKIT_MGMT_HELLO_MINVER GENMASK_ULL(15, 0)
#define APPLE_RTKIT_MGMT_HELLO_MAXVER GENMASK_ULL(31, 16)

#define APPLE_RTKIT_MGMT_EPMAP_LAST   BIT_ULL(51)
#define APPLE_RTKIT_MGMT_EPMAP_BASE   GENMASK_ULL(34, 32)
#define APPLE_RTKIT_MGMT_EPMAP_BITMAP GENMASK_ULL(31, 0)

#define APPLE_RTKIT_MGMT_EPMAP_REPLY_MORE BIT_ULL(0)

#define APPLE_RTKIT_MGMT_STARTEP_EP   GENMASK_ULL(39, 32)
#define APPLE_RTKIT_MGMT_STARTEP_FLAG BIT_ULL(1)

#define APPLE_RTKIT_MGMT_PWR_STATE GENMASK_ULL(15, 0)

#define APPLE_RTKIT_CRASHLOG_CRASH 1

#define APPLE_RTKIT_BUFFER_REQUEST	1
#define APPLE_RTKIT_BUFFER_REQUEST_SIZE GENMASK_ULL(51, 44)
#define APPLE_RTKIT_BUFFER_REQUEST_IOVA GENMASK_ULL(43, 0)

#define APPLE_RTKIT_SYSLOG_TYPE GENMASK_ULL(59, 52)

#define APPLE_RTKIT_SYSLOG_LOG 5

#define APPLE_RTKIT_SYSLOG_INIT	     8
#define APPLE_RTKIT_SYSLOG_N_ENTRIES GENMASK_ULL(7, 0)
#define APPLE_RTKIT_SYSLOG_MSG_SIZE  GENMASK_ULL(31, 24)

#define APPLE_RTKIT_OSLOG_TYPE GENMASK_ULL(63, 56)
#define APPLE_RTKIT_OSLOG_BUFFER_REQUEST 1
#define APPLE_RTKIT_OSLOG_SIZE GENMASK_ULL(55, 36)
#define APPLE_RTKIT_OSLOG_IOVA GENMASK_ULL(35, 0)

#define APPLE_RTKIT_MIN_SUPPORTED_VERSION 11
#define APPLE_RTKIT_MAX_SUPPORTED_VERSION 12

struct apple_rtkit_rx_work {
	struct apple_rtkit *rtk;
	u8 ep;
	u64 msg;
	struct work_struct work;
};

bool apple_rtkit_is_running(struct apple_rtkit *rtk)
{
	if (rtk->crashed)
		return false;
	if ((rtk->iop_power_state & 0xff) != APPLE_RTKIT_PWR_STATE_ON)
		return false;
	if ((rtk->ap_power_state & 0xff) != APPLE_RTKIT_PWR_STATE_ON)
		return false;
	return true;
}
EXPORT_SYMBOL_GPL(apple_rtkit_is_running);

bool apple_rtkit_is_crashed(struct apple_rtkit *rtk)
{
	return rtk->crashed;
}
EXPORT_SYMBOL_GPL(apple_rtkit_is_crashed);

static int apple_rtkit_management_send(struct apple_rtkit *rtk, u8 type,
					u64 msg)
{
	int ret;

	msg &= ~APPLE_RTKIT_MGMT_TYPE;
	msg |= FIELD_PREP(APPLE_RTKIT_MGMT_TYPE, type);
	ret = apple_rtkit_send_message(rtk, APPLE_RTKIT_EP_MGMT, msg, NULL, false);

	if (ret)
		dev_err(rtk->dev, "RTKit: Failed to send management message: %d\n", ret);

	return ret;
}

static void apple_rtkit_epmap_only_abort(struct apple_rtkit *rtk, int error)
{
	if (!rtk->epmap_only || rtk->epmap_finished)
		return;

	rtk->boot_result = error;
	rtk->epmap_finished = true;
	complete_all(&rtk->epmap_completion);
}

static void apple_rtkit_management_rx_hello(struct apple_rtkit *rtk, u64 msg)
{
	u64 reply;
	int ret;

	int min_ver = FIELD_GET(APPLE_RTKIT_MGMT_HELLO_MINVER, msg);
	int max_ver = FIELD_GET(APPLE_RTKIT_MGMT_HELLO_MAXVER, msg);
	int want_ver = min(APPLE_RTKIT_MAX_SUPPORTED_VERSION, max_ver);

	if (rtk->epmap_only) {
		if (rtk->hello_seen || rtk->epmap_finished) {
			apple_rtkit_epmap_only_abort(rtk, -EPROTO);
			return;
		}

		rtk->raw_hello = msg;
		rtk->hello_min_version = min_ver;
		rtk->hello_max_version = max_ver;
		rtk->hello_seen = true;
	}

	dev_dbg(rtk->dev, "RTKit: Min ver %d, max ver %d\n", min_ver, max_ver);

	if (min_ver > APPLE_RTKIT_MAX_SUPPORTED_VERSION) {
		dev_err(rtk->dev, "RTKit: Firmware min version %d is too new\n",
			min_ver);
		goto abort_boot;
	}

	if (max_ver < APPLE_RTKIT_MIN_SUPPORTED_VERSION) {
		dev_err(rtk->dev, "RTKit: Firmware max version %d is too old\n",
			max_ver);
		goto abort_boot;
	}

	dev_info(rtk->dev, "RTKit: Initializing (protocol version %d)\n",
		 want_ver);
	rtk->version = want_ver;

	reply = FIELD_PREP(APPLE_RTKIT_MGMT_HELLO_MINVER, want_ver);
	reply |= FIELD_PREP(APPLE_RTKIT_MGMT_HELLO_MAXVER, want_ver);
	ret = apple_rtkit_management_send(rtk, APPLE_RTKIT_MGMT_HELLO_REPLY,
					 reply);
	if (ret && rtk->epmap_only)
		apple_rtkit_epmap_only_abort(rtk, ret);

	return;

abort_boot:
	if (rtk->epmap_only) {
		apple_rtkit_epmap_only_abort(rtk, -EINVAL);
	} else {
		rtk->boot_result = -EINVAL;
		complete_all(&rtk->epmap_completion);
	}
}

static void apple_rtkit_management_rx_epmap(struct apple_rtkit *rtk, u64 msg)
{
	int i, ep, ret;
	u64 reply;
	unsigned long bitmap = FIELD_GET(APPLE_RTKIT_MGMT_EPMAP_BITMAP, msg);
	u32 base = FIELD_GET(APPLE_RTKIT_MGMT_EPMAP_BASE, msg);

	if (rtk->epmap_only) {
		if (!rtk->hello_seen || rtk->epmap_finished ||
		    (rtk->epmap_bases_seen & BIT(base))) {
			apple_rtkit_epmap_only_abort(rtk, -EPROTO);
			return;
		}
		rtk->epmap_bases_seen |= BIT(base);
	}

	dev_dbg(rtk->dev,
		"RTKit: received endpoint bitmap 0x%lx with base 0x%x\n",
		bitmap, base);

	for_each_set_bit(i, &bitmap, 32) {
		ep = 32 * base + i;
		dev_dbg(rtk->dev, "RTKit: Discovered endpoint 0x%02x\n", ep);
		set_bit(ep, rtk->endpoints);
		if (rtk->epmap_only)
			set_bit(ep, rtk->discovery_endpoints);
	}

	/*
	 * G17P sends its firmware-owned crash buffer request as soon as EP1 is
	 * started.  Start EP1 before acknowledging the EPMAP fragment that
	 * advertises it, matching the ordering required by this firmware.
	 */
	if (rtk->firmware_owned_crashlog &&
	    test_bit(APPLE_RTKIT_EP_CRASHLOG, rtk->endpoints) &&
	    !rtk->crashlog_endpoint_started) {
		ret = apple_rtkit_start_ep(rtk, APPLE_RTKIT_EP_CRASHLOG);
		if (ret) {
			rtk->boot_result = ret;
			complete_all(&rtk->epmap_completion);
			return;
		}
		rtk->crashlog_endpoint_started = true;
	}

	reply = FIELD_PREP(APPLE_RTKIT_MGMT_EPMAP_BASE, base);
	if (msg & APPLE_RTKIT_MGMT_EPMAP_LAST)
		reply |= APPLE_RTKIT_MGMT_EPMAP_LAST;
	else
		reply |= APPLE_RTKIT_MGMT_EPMAP_REPLY_MORE;

	ret = apple_rtkit_management_send(rtk, APPLE_RTKIT_MGMT_EPMAP_REPLY,
					 reply);
	if (ret && rtk->epmap_only) {
		apple_rtkit_epmap_only_abort(rtk, ret);
		return;
	}

	if (!(msg & APPLE_RTKIT_MGMT_EPMAP_LAST))
		return;

	if (rtk->epmap_only) {
		rtk->boot_result = 0;
		rtk->epmap_finished = true;
		complete_all(&rtk->epmap_completion);
		return;
	}

	for_each_set_bit(ep, rtk->endpoints, APPLE_RTKIT_APP_ENDPOINT_START) {
		switch (ep) {
		/* the management endpoint is started by default */
		case APPLE_RTKIT_EP_MGMT:
			break;
		case APPLE_RTKIT_EP_PMP_AUX:
			if (rtk->t8152_pmp) {
				apple_rtkit_start_ep(rtk, ep);
				break;
			}
			dev_warn(rtk->dev, "RTKit: unqualified endpoint 7\n");
			break;

		/* without starting these RTKit refuses to boot */
		case APPLE_RTKIT_EP_SYSLOG:
		case APPLE_RTKIT_EP_CRASHLOG:
		case APPLE_RTKIT_EP_DEBUG:
		case APPLE_RTKIT_EP_IOREPORT:
		case APPLE_RTKIT_EP_OSLOG:
		case APPLE_RTKIT_EP_TRACEKIT:
			if (ep == APPLE_RTKIT_EP_CRASHLOG &&
			    rtk->crashlog_endpoint_started)
				break;
			dev_dbg(rtk->dev,
				"RTKit: Starting system endpoint 0x%02x\n", ep);
			apple_rtkit_start_ep(rtk, ep);
			break;

		default:
			dev_warn(rtk->dev,
				 "RTKit: Unknown system endpoint: 0x%02x\n",
				 ep);
		}
	}

	rtk->boot_result = 0;
	complete_all(&rtk->epmap_completion);
}

static void apple_rtkit_management_rx_iop_pwr_ack(struct apple_rtkit *rtk,
						  u64 msg)
{
	unsigned int new_state = FIELD_GET(APPLE_RTKIT_MGMT_PWR_STATE, msg);

	dev_dbg(rtk->dev, "RTKit: IOP power state transition: 0x%x -> 0x%x\n",
		rtk->iop_power_state, new_state);
	rtk->iop_power_state = new_state;

	complete_all(&rtk->iop_pwr_ack_completion);
}

static void apple_rtkit_management_rx_ap_pwr_ack(struct apple_rtkit *rtk,
						 u64 msg)
{
	unsigned int new_state = FIELD_GET(APPLE_RTKIT_MGMT_PWR_STATE, msg);

	dev_dbg(rtk->dev, "RTKit: AP power state transition: 0x%x -> 0x%x\n",
		rtk->ap_power_state, new_state);
	rtk->ap_power_state = new_state;

	complete_all(&rtk->ap_pwr_ack_completion);
}

static void apple_rtkit_management_rx(struct apple_rtkit *rtk, u64 msg)
{
	u8 type = FIELD_GET(APPLE_RTKIT_MGMT_TYPE, msg);

	/* H17P sends this separate idle fence after acknowledging IOP OFF. */
	if (type == APPLE_RTKIT_MGMT_IOP_IDLE &&
	    READ_ONCE(rtk->awaiting_iop_idle) && !(msg & ~APPLE_RTKIT_MGMT_TYPE)) {
		complete_all(&rtk->iop_idle_completion);
		return;
	}

	if (rtk->epmap_only && type != APPLE_RTKIT_MGMT_HELLO &&
	    type != APPLE_RTKIT_MGMT_EPMAP) {
		dev_err(rtk->dev,
			"RTKit: discovery-only client received management type 0x%02x\n",
			type);
		apple_rtkit_epmap_only_abort(rtk, -EPROTO);
		return;
	}

	switch (type) {
	case APPLE_RTKIT_MGMT_HELLO:
		apple_rtkit_management_rx_hello(rtk, msg);
		break;
	case APPLE_RTKIT_MGMT_EPMAP:
		apple_rtkit_management_rx_epmap(rtk, msg);
		break;
	case APPLE_RTKIT_MGMT_SET_IOP_PWR_STATE_ACK:
		apple_rtkit_management_rx_iop_pwr_ack(rtk, msg);
		break;
	case APPLE_RTKIT_MGMT_SET_AP_PWR_STATE_ACK:
		apple_rtkit_management_rx_ap_pwr_ack(rtk, msg);
		break;
	default:
		dev_warn(
			rtk->dev,
			"RTKit: unknown management message: 0x%llx (type: 0x%02x)\n",
			msg, type);
	}
}

static int apple_rtkit_common_rx_get_buffer(struct apple_rtkit *rtk,
					    struct apple_rtkit_shmem *buffer,
					    u8 ep, u64 msg)
{
	u64 reply;
	int err;

	/* The different size vs. IOVA shifts look odd but are indeed correct this way */
	if (ep == APPLE_RTKIT_EP_OSLOG) {
		buffer->size = FIELD_GET(APPLE_RTKIT_OSLOG_SIZE, msg);
		buffer->iova = FIELD_GET(APPLE_RTKIT_OSLOG_IOVA, msg) << 12;
	} else {
		buffer->size = FIELD_GET(APPLE_RTKIT_BUFFER_REQUEST_SIZE, msg) << 12;
		buffer->iova = FIELD_GET(APPLE_RTKIT_BUFFER_REQUEST_IOVA, msg);
	}

	buffer->buffer = NULL;
	buffer->iomem = NULL;
	buffer->is_mapped = false;

	dev_dbg(rtk->dev, "RTKit: buffer request for 0x%zx bytes at %pad\n",
		buffer->size, &buffer->iova);

	/*
	 * G17P places its crash log in the GPU UAT and sends that DVA in the
	 * request.  Linux cannot map it through the device DMA domain.  Retain the
	 * DVA for diagnostics and echo the request exactly as firmware expects.
	 */
	if (rtk->firmware_owned_crashlog &&
	    ep == APPLE_RTKIT_EP_CRASHLOG && buffer->iova) {
		buffer->is_mapped = true;
		err = apple_rtkit_send_message(rtk, ep, msg, NULL, false);
		if (err)
			goto error;
		return 0;
	}

	if (buffer->iova && !rtk->ops->shmem_setup) {
		err = -EINVAL;
		goto error;
	}

	if (rtk->ops->shmem_setup) {
		err = rtk->ops->shmem_setup(rtk->cookie, buffer);
		if (err)
			goto error;
	} else {
		buffer->buffer = dma_alloc_coherent(rtk->dev, buffer->size,
						    &buffer->iova, GFP_KERNEL);
		if (!buffer->buffer) {
			err = -ENOMEM;
			goto error;
		}
	}

	if (!buffer->is_mapped) {
		/* oslog uses different fields and needs a shifted IOVA instead of size */
		if (ep == APPLE_RTKIT_EP_OSLOG) {
			reply = FIELD_PREP(APPLE_RTKIT_OSLOG_TYPE,
					   APPLE_RTKIT_OSLOG_BUFFER_REQUEST);
			reply |= FIELD_PREP(APPLE_RTKIT_OSLOG_SIZE, buffer->size);
			reply |= FIELD_PREP(APPLE_RTKIT_OSLOG_IOVA,
					    buffer->iova >> 12);
		} else {
			reply = FIELD_PREP(APPLE_RTKIT_SYSLOG_TYPE,
					   ep == APPLE_RTKIT_EP_PMP_AUX ? 5 :
					   APPLE_RTKIT_BUFFER_REQUEST);
			reply |= FIELD_PREP(APPLE_RTKIT_BUFFER_REQUEST_SIZE,
					    buffer->size >> 12);
			reply |= FIELD_PREP(APPLE_RTKIT_BUFFER_REQUEST_IOVA,
					    buffer->iova);
		}
		apple_rtkit_send_message(rtk, ep, reply, NULL, false);
	}

	return 0;

error:
	dev_err(rtk->dev, "RTKit: failed buffer request for 0x%zx bytes (%d)\n",
		buffer->size, err);

	buffer->buffer = NULL;
	buffer->iomem = NULL;
	buffer->iova = 0;
	buffer->size = 0;
	buffer->is_mapped = false;
	return err;
}

static void apple_rtkit_free_buffer(struct apple_rtkit *rtk,
				    struct apple_rtkit_shmem *bfr)
{
	if (bfr->size == 0)
		return;

	if (rtk->ops->shmem_destroy)
		rtk->ops->shmem_destroy(rtk->cookie, bfr);
	else if (bfr->buffer)
		dma_free_coherent(rtk->dev, bfr->size, bfr->buffer, bfr->iova);

	bfr->buffer = NULL;
	bfr->iomem = NULL;
	bfr->iova = 0;
	bfr->size = 0;
	bfr->is_mapped = false;
}

static void apple_rtkit_memcpy(struct apple_rtkit *rtk, void *dst,
			       struct apple_rtkit_shmem *bfr, size_t offset,
			       size_t len)
{
	if (bfr->iomem)
		memcpy_fromio(dst, bfr->iomem + offset, len);
	else
		memcpy(dst, bfr->buffer + offset, len);
}

static void apple_rtkit_crashlog_rx(struct apple_rtkit *rtk, u64 msg)
{
	u8 type = FIELD_GET(APPLE_RTKIT_SYSLOG_TYPE, msg);
	u8 *bfr __free(kfree) = NULL;

	if (type != APPLE_RTKIT_CRASHLOG_CRASH) {
		dev_warn(rtk->dev, "RTKit: Unknown crashlog message: %llx\n",
			 msg);
		return;
	}

	if (!rtk->crashlog_buffer.size) {
		apple_rtkit_common_rx_get_buffer(rtk, &rtk->crashlog_buffer,
						 APPLE_RTKIT_EP_CRASHLOG, msg);
		return;
	}

	dev_err(rtk->dev, "RTKit: co-processor has crashed\n");
	if (rtk->firmware_owned_crashlog && !rtk->crashlog_buffer.buffer &&
	    !rtk->crashlog_buffer.iomem) {
		dev_err(rtk->dev,
			"RTKit: firmware crash log remains at DVA %pad (0x%zx bytes)\n",
			&rtk->crashlog_buffer.iova, rtk->crashlog_buffer.size);
		rtk->crashed = true;
		if (rtk->ops->crashed)
			rtk->ops->crashed(rtk->cookie, NULL, 0);
		return;
	}

	/*
	 * create a shadow copy here to make sure the co-processor isn't able
	 * to change the log while we're dumping it. this also ensures
	 * the buffer is in normal memory and not iomem for e.g. the SMC
	 */
	bfr = kzalloc(rtk->crashlog_buffer.size, GFP_KERNEL);
	if (bfr) {
		apple_rtkit_memcpy(rtk, bfr, &rtk->crashlog_buffer, 0,
				   rtk->crashlog_buffer.size);
		apple_rtkit_crashlog_dump(rtk, bfr, rtk->crashlog_buffer.size);
	} else {
		dev_err(rtk->dev,
			"RTKit: Couldn't allocate crashlog shadow buffer\n");
	}

	rtk->crashed = true;
	if (rtk->ops->crashed)
		rtk->ops->crashed(rtk->cookie, bfr, bfr ? rtk->crashlog_buffer.size : 0);
}

static void apple_rtkit_ioreport_rx(struct apple_rtkit *rtk, u64 msg)
{
	u8 type = FIELD_GET(APPLE_RTKIT_SYSLOG_TYPE, msg);

	switch (type) {
	case APPLE_RTKIT_BUFFER_REQUEST:
		apple_rtkit_common_rx_get_buffer(rtk, &rtk->ioreport_buffer,
						 APPLE_RTKIT_EP_IOREPORT, msg);
		break;
	/* unknown, must be ACKed or the co-processor will hang */
	case 0x8:
	case 0xc:
		apple_rtkit_send_message(rtk, APPLE_RTKIT_EP_IOREPORT, msg,
					 NULL, false);
		break;
	default:
		dev_warn(rtk->dev, "RTKit: Unknown ioreport message: %llx\n",
			 msg);
	}
}

static void apple_rtkit_syslog_rx_init(struct apple_rtkit *rtk, u64 msg)
{
	rtk->syslog_n_entries = FIELD_GET(APPLE_RTKIT_SYSLOG_N_ENTRIES, msg);
	rtk->syslog_msg_size = FIELD_GET(APPLE_RTKIT_SYSLOG_MSG_SIZE, msg);

	rtk->syslog_msg_buffer = kzalloc(rtk->syslog_msg_size, GFP_KERNEL);

	dev_dbg(rtk->dev,
		"RTKit: syslog initialized: entries: %zd, msg_size: %zd\n",
		rtk->syslog_n_entries, rtk->syslog_msg_size);
}

static bool should_crop_syslog_char(char c)
{
	return c == '\n' || c == '\r' || c == ' ' || c == '\0';
}

static void apple_rtkit_syslog_rx_log(struct apple_rtkit *rtk, u64 msg)
{
	u8 idx = msg & 0xff;
	char log_context[24];
	size_t entry_size = 0x20 + rtk->syslog_msg_size;
	int msglen;

	if (!rtk->syslog_msg_buffer) {
		if (!rtk->adopted)
			dev_warn(rtk->dev,
				 "RTKit: received syslog message but no syslog_msg_buffer\n");
		goto done;
	}
	if (!rtk->syslog_buffer.size) {
		dev_warn(
			rtk->dev,
			"RTKit: received syslog message but syslog_buffer.size is zero\n");
		goto done;
	}
	if (!rtk->syslog_buffer.buffer && !rtk->syslog_buffer.iomem) {
		dev_warn(
			rtk->dev,
			"RTKit: received syslog message but no syslog_buffer.buffer or syslog_buffer.iomem\n");
		goto done;
	}
	if (idx > rtk->syslog_n_entries) {
		dev_warn(rtk->dev, "RTKit: syslog index %d out of range\n",
			 idx);
		goto done;
	}

	apple_rtkit_memcpy(rtk, log_context, &rtk->syslog_buffer,
			   idx * entry_size + 8, sizeof(log_context));
	apple_rtkit_memcpy(rtk, rtk->syslog_msg_buffer, &rtk->syslog_buffer,
			   idx * entry_size + 8 + sizeof(log_context),
			   rtk->syslog_msg_size);

	log_context[sizeof(log_context) - 1] = 0;

	msglen = strnlen(rtk->syslog_msg_buffer, rtk->syslog_msg_size - 1);
	while (msglen > 0 &&
		   should_crop_syslog_char(rtk->syslog_msg_buffer[msglen - 1]))
		msglen--;

	rtk->syslog_msg_buffer[msglen] = 0;
	dev_info(rtk->dev, "RTKit: syslog message: %s: %s\n", log_context,
		 rtk->syslog_msg_buffer);

done:
	apple_rtkit_send_message(rtk, APPLE_RTKIT_EP_SYSLOG, msg, NULL, false);
}

static void apple_rtkit_syslog_rx(struct apple_rtkit *rtk, u64 msg)
{
	u8 type = FIELD_GET(APPLE_RTKIT_SYSLOG_TYPE, msg);

	switch (type) {
	case APPLE_RTKIT_BUFFER_REQUEST:
		apple_rtkit_common_rx_get_buffer(rtk, &rtk->syslog_buffer,
						 APPLE_RTKIT_EP_SYSLOG, msg);
		break;
	case APPLE_RTKIT_SYSLOG_INIT:
		apple_rtkit_syslog_rx_init(rtk, msg);
		break;
	case APPLE_RTKIT_SYSLOG_LOG:
		apple_rtkit_syslog_rx_log(rtk, msg);
		break;
	default:
		dev_warn(rtk->dev, "RTKit: Unknown syslog message: %llx\n",
			 msg);
	}
}

static void apple_rtkit_oslog_rx(struct apple_rtkit *rtk, u64 msg)
{
	u8 type = FIELD_GET(APPLE_RTKIT_OSLOG_TYPE, msg);

	switch (type) {
	case APPLE_RTKIT_OSLOG_BUFFER_REQUEST:
		apple_rtkit_common_rx_get_buffer(rtk, &rtk->oslog_buffer,
						 APPLE_RTKIT_EP_OSLOG, msg);
		break;
	default:
		dev_warn(rtk->dev, "RTKit: Unknown oslog message: %llx\n",
			 msg);
	}
}

/* Native J873 26A428 capture074: EP7 requests 8KiB with type1 and
 * receives type5, the same size and an unshifted 44-bit DVA. Keep the
 * buffer alive for the RTKit session; never replace a live allocation.
 */
static void apple_rtkit_pmp_aux_rx(struct apple_rtkit *rtk, u64 msg)
{
	int ret;

	if (!rtk->t8152_pmp || msg != 0x10200000000000ULL ||
	    rtk->pmp_aux_buffer.size) {
		dev_err(rtk->dev, "RTKit: refused PMP EP7 request %#llx\n", msg);
		return;
	}
	ret = apple_rtkit_common_rx_get_buffer(rtk, &rtk->pmp_aux_buffer,
					     APPLE_RTKIT_EP_PMP_AUX, msg);
	dev_info(rtk->dev, "RTKit: PMP EP7 8KiB buffer handshake result=%d\n", ret);
}

static void apple_rtkit_rx_work(struct work_struct *work)
{
	struct apple_rtkit_rx_work *rtk_work =
		container_of(work, struct apple_rtkit_rx_work, work);
	struct apple_rtkit *rtk = rtk_work->rtk;

	if (READ_ONCE(rtk->shutting_down))
		goto out_free;

	switch (rtk_work->ep) {
	case APPLE_RTKIT_EP_MGMT:
		apple_rtkit_management_rx(rtk, rtk_work->msg);
		break;
	case APPLE_RTKIT_EP_CRASHLOG:
		apple_rtkit_crashlog_rx(rtk, rtk_work->msg);
		break;
	case APPLE_RTKIT_EP_SYSLOG:
		apple_rtkit_syslog_rx(rtk, rtk_work->msg);
		break;
	case APPLE_RTKIT_EP_IOREPORT:
		apple_rtkit_ioreport_rx(rtk, rtk_work->msg);
		break;
	case APPLE_RTKIT_EP_OSLOG:
		apple_rtkit_oslog_rx(rtk, rtk_work->msg);
		break;
	case APPLE_RTKIT_EP_PMP_AUX:
		apple_rtkit_pmp_aux_rx(rtk, rtk_work->msg);
		break;
	case APPLE_RTKIT_APP_ENDPOINT_START ... 0xff:
		if (rtk->ops->recv_message)
			rtk->ops->recv_message(rtk->cookie, rtk_work->ep,
					       rtk_work->msg);
		else
			dev_warn(
				rtk->dev,
				"Received unexpected message to EP%02d: %llx\n",
				rtk_work->ep, rtk_work->msg);
		break;
	default:
		dev_warn(rtk->dev,
			 "RTKit: message to unknown endpoint %02x: %llx\n",
				 rtk_work->ep, rtk_work->msg);
	}

out_free:
	kfree(rtk_work);
}

static void apple_rtkit_rx(struct apple_mbox *mbox, struct apple_mbox_msg msg,
			   void *cookie)
{
	struct apple_rtkit *rtk = cookie;
	struct apple_rtkit_rx_work *work;
	u8 ep = msg.msg1;

	/*
	 * The message was read from a MMIO FIFO and we have to make
	 * sure all reads from buffers sent with that message happen
	 * afterwards.
	 */
	dma_rmb();
	if (READ_ONCE(rtk->shutting_down))
		return;

	if (!test_bit(ep, rtk->endpoints))
		dev_warn(rtk->dev,
			 "RTKit: Message to undiscovered endpoint 0x%02x\n",
			 ep);

	/*
	 * Passive discovery owns endpoint zero only.  Never queue a crashlog,
	 * syslog, or application callback from this mode: doing so could allocate
	 * shared memory or acknowledge a protocol that has not been admitted.
	 */
	if (rtk->epmap_only && ep != APPLE_RTKIT_EP_MGMT) {
		dev_err(rtk->dev,
			"RTKit: discovery-only client received endpoint 0x%02x traffic\n",
			ep);
		apple_rtkit_epmap_only_abort(rtk, -EPROTO);
		return;
	}

	if (ep >= APPLE_RTKIT_APP_ENDPOINT_START &&
	    rtk->ops->recv_message_early &&
	    rtk->ops->recv_message_early(rtk->cookie, ep, msg.msg0))
		return;

	work = kzalloc_obj(*work, GFP_ATOMIC);
	if (!work)
		return;

	work->rtk = rtk;
	work->ep = ep;
	work->msg = msg.msg0;
	INIT_WORK(&work->work, apple_rtkit_rx_work);
	queue_work(rtk->wq, &work->work);
}

int apple_rtkit_send_message(struct apple_rtkit *rtk, u8 ep, u64 message,
			     struct completion *completion, bool atomic)
{
	struct apple_mbox_msg msg = {
		.msg0 = message,
		.msg1 = ep,
	};

	if (rtk->crashed) {
		dev_warn(rtk->dev,
			 "RTKit: Device is crashed, cannot send message\n");
		return -EINVAL;
	}

	if (rtk->epmap_only) {
		u8 type = FIELD_GET(APPLE_RTKIT_MGMT_TYPE, message);

		if (ep != APPLE_RTKIT_EP_MGMT ||
		    (type != APPLE_RTKIT_MGMT_HELLO_REPLY &&
		     type != APPLE_RTKIT_MGMT_EPMAP_REPLY)) {
			dev_warn(rtk->dev,
				 "RTKit: discovery-only client refused endpoint/power side effect\n");
			return -EPERM;
		}
	}

	if (ep >= APPLE_RTKIT_APP_ENDPOINT_START &&
	    !apple_rtkit_is_running(rtk)) {
		dev_warn(rtk->dev,
			 "RTKit: Endpoint 0x%02x is not running, cannot send message\n", ep);
		return -EINVAL;
	}

	/*
	 * The message will be sent with a MMIO write. We need the barrier
	 * here to ensure any previous writes to buffers are visible to the
	 * device before that MMIO write happens.
	 */
	dma_wmb();

	return apple_mbox_send(rtk->mbox, msg, atomic);
}
EXPORT_SYMBOL_GPL(apple_rtkit_send_message);

int apple_rtkit_poll(struct apple_rtkit *rtk)
{
	return apple_mbox_poll(rtk->mbox);
}
EXPORT_SYMBOL_GPL(apple_rtkit_poll);

bool apple_rtkit_has_endpoint(struct apple_rtkit *rtk, u8 ep)
{
	return test_bit(ep, rtk->endpoints);
}
EXPORT_SYMBOL_GPL(apple_rtkit_has_endpoint);

int apple_rtkit_start_ep(struct apple_rtkit *rtk, u8 endpoint)
{
	u64 msg;

	if (rtk->epmap_only)
		return -EPERM;

	if (!test_bit(endpoint, rtk->endpoints))
		return -EINVAL;
	if (endpoint >= APPLE_RTKIT_APP_ENDPOINT_START &&
	    !apple_rtkit_is_running(rtk))
		return -EINVAL;

	msg = FIELD_PREP(APPLE_RTKIT_MGMT_STARTEP_EP, endpoint);
	msg |= APPLE_RTKIT_MGMT_STARTEP_FLAG;
	apple_rtkit_management_send(rtk, APPLE_RTKIT_MGMT_STARTEP, msg);

	return 0;
}
EXPORT_SYMBOL_GPL(apple_rtkit_start_ep);

static void apple_rtkit_mark_running(struct apple_rtkit *rtk)
{
	static const u8 system_endpoints[] = {
		APPLE_RTKIT_EP_CRASHLOG,
		APPLE_RTKIT_EP_SYSLOG,
		APPLE_RTKIT_EP_DEBUG,
		APPLE_RTKIT_EP_IOREPORT,
		APPLE_RTKIT_EP_OSLOG,
		APPLE_RTKIT_EP_TRACEKIT,
	};
	int i;

	/* The previous owner already completed and acknowledged this EPMAP. */
	for (i = 0; i < ARRAY_SIZE(system_endpoints); i++)
		set_bit(system_endpoints[i], rtk->endpoints);

	rtk->iop_power_state = APPLE_RTKIT_PWR_STATE_ON;
	rtk->ap_power_state = APPLE_RTKIT_PWR_STATE_ON;
	rtk->adopted = true;
}

static struct apple_rtkit *__apple_rtkit_init(
	struct device *dev, void *cookie, const char *mbox_name, int mbox_idx,
	const struct apple_rtkit_ops *ops, bool adopted, bool epmap_only,
	bool firmware_owned_crashlog)
{
	struct apple_rtkit *rtk;
	int ret;

	if (!ops || (adopted && epmap_only))
		return ERR_PTR(-EINVAL);

	rtk = kzalloc_obj(*rtk);
	if (!rtk)
		return ERR_PTR(-ENOMEM);

	rtk->dev = dev;
	rtk->cookie = cookie;
	rtk->ops = ops;
	rtk->epmap_only = epmap_only;
	rtk->firmware_owned_crashlog = firmware_owned_crashlog;
	rtk->t8152_pmp = of_device_is_compatible(dev->of_node, "apple,t8152-pmp-v2");

	init_completion(&rtk->epmap_completion);
	init_completion(&rtk->iop_pwr_ack_completion);
	init_completion(&rtk->ap_pwr_ack_completion);
	init_completion(&rtk->iop_idle_completion);

	bitmap_zero(rtk->endpoints, APPLE_RTKIT_MAX_ENDPOINTS);
	bitmap_zero(rtk->discovery_endpoints, APPLE_RTKIT_MAX_ENDPOINTS);
	set_bit(APPLE_RTKIT_EP_MGMT, rtk->endpoints);

	if (mbox_name)
		rtk->mbox = apple_mbox_get_byname(dev, mbox_name);
	else
		rtk->mbox = apple_mbox_get(dev, mbox_idx);

	if (IS_ERR(rtk->mbox)) {
		ret = PTR_ERR(rtk->mbox);
		goto free_rtk;
	}
	if (rtk->mbox->rx) {
		ret = -EBUSY;
		goto free_rtk;
	}

	rtk->wq = alloc_ordered_workqueue("rtkit-%s", WQ_HIGHPRI | WQ_MEM_RECLAIM,
					  dev_name(rtk->dev));
	if (!rtk->wq) {
		ret = -ENOMEM;
		goto free_rtk;
	}

	rtk->mbox->cookie = rtk;
	rtk->mbox->rx = apple_rtkit_rx;
	if (adopted)
		apple_rtkit_mark_running(rtk);

	ret = apple_mbox_start(rtk->mbox);
	if (ret)
		goto destroy_wq;

	return rtk;

destroy_wq:
	rtk->mbox->rx = NULL;
	rtk->mbox->cookie = NULL;
	destroy_workqueue(rtk->wq);
free_rtk:
	kfree(rtk);
	return ERR_PTR(ret);
}

struct apple_rtkit *apple_rtkit_init(struct device *dev, void *cookie,
					    const char *mbox_name, int mbox_idx,
					    const struct apple_rtkit_ops *ops)
{
	return __apple_rtkit_init(dev, cookie, mbox_name, mbox_idx, ops, false,
				  false, false);
}
EXPORT_SYMBOL_GPL(apple_rtkit_init);

struct apple_rtkit *apple_rtkit_init_g17p(
	struct device *dev, void *cookie, const char *mbox_name, int mbox_idx,
	const struct apple_rtkit_ops *ops)
{
	return __apple_rtkit_init(dev, cookie, mbox_name, mbox_idx, ops, false,
				  false, true);
}
EXPORT_SYMBOL_GPL(apple_rtkit_init_g17p);

struct apple_rtkit *apple_rtkit_init_adopted(
	struct device *dev, void *cookie, const char *mbox_name, int mbox_idx,
	const struct apple_rtkit_ops *ops)
{
	return __apple_rtkit_init(dev, cookie, mbox_name, mbox_idx, ops, true,
				  false, false);
}
EXPORT_SYMBOL_GPL(apple_rtkit_init_adopted);

struct apple_rtkit *apple_rtkit_init_epmap_only(
	struct device *dev, void *cookie, const char *mbox_name, int mbox_idx,
	const struct apple_rtkit_ops *ops)
{
	return __apple_rtkit_init(dev, cookie, mbox_name, mbox_idx, ops, false,
				  true, false);
}
EXPORT_SYMBOL_GPL(apple_rtkit_init_epmap_only);

static int apple_rtkit_wait_for_completion(struct completion *c)
{
	long t;

	t = wait_for_completion_interruptible_timeout(c,
						      msecs_to_jiffies(1000));
	if (t < 0)
		return t;
	else if (t == 0)
		return -ETIME;
	else
		return 0;
}

int apple_rtkit_reinit(struct apple_rtkit *rtk)
{
	/* make sure we don't handle any messages while reinitializing */
	apple_mbox_stop(rtk->mbox);
	flush_workqueue(rtk->wq);

	apple_rtkit_free_buffer(rtk, &rtk->ioreport_buffer);
	apple_rtkit_free_buffer(rtk, &rtk->crashlog_buffer);
	apple_rtkit_free_buffer(rtk, &rtk->oslog_buffer);
	apple_rtkit_free_buffer(rtk, &rtk->syslog_buffer);
	apple_rtkit_free_buffer(rtk, &rtk->pmp_aux_buffer);

	kfree(rtk->syslog_msg_buffer);

	rtk->syslog_msg_buffer = NULL;
	rtk->syslog_n_entries = 0;
	rtk->syslog_msg_size = 0;
	rtk->adopted = false;
	WRITE_ONCE(rtk->awaiting_iop_idle, false);

	bitmap_zero(rtk->endpoints, APPLE_RTKIT_MAX_ENDPOINTS);
	bitmap_zero(rtk->discovery_endpoints, APPLE_RTKIT_MAX_ENDPOINTS);
	set_bit(APPLE_RTKIT_EP_MGMT, rtk->endpoints);

	reinit_completion(&rtk->epmap_completion);
	reinit_completion(&rtk->iop_pwr_ack_completion);
	reinit_completion(&rtk->ap_pwr_ack_completion);

	rtk->crashed = false;
	rtk->boot_result = 0;
	rtk->raw_hello = 0;
	rtk->hello_min_version = 0;
	rtk->hello_max_version = 0;
	rtk->epmap_bases_seen = 0;
	rtk->hello_seen = false;
	rtk->epmap_finished = false;
	rtk->crashlog_endpoint_started = false;
	rtk->iop_power_state = APPLE_RTKIT_PWR_STATE_OFF;
	rtk->ap_power_state = APPLE_RTKIT_PWR_STATE_OFF;

	return apple_mbox_start(rtk->mbox);
}
EXPORT_SYMBOL_GPL(apple_rtkit_reinit);

static int apple_rtkit_set_ap_power_state(struct apple_rtkit *rtk,
					  unsigned int state)
{
	u64 msg;
	int ret;

	if (rtk->epmap_only)
		return -EPERM;

	reinit_completion(&rtk->ap_pwr_ack_completion);

	msg = FIELD_PREP(APPLE_RTKIT_MGMT_PWR_STATE, state);
	ret = apple_rtkit_management_send(rtk, APPLE_RTKIT_MGMT_SET_AP_PWR_STATE,
					  msg);
	if (ret)
		return ret;

	ret = apple_rtkit_wait_for_completion(&rtk->ap_pwr_ack_completion);
	if (ret)
		return ret;

	if (rtk->ap_power_state != state)
		return -EINVAL;
	return 0;
}

static int apple_rtkit_set_iop_power_state(struct apple_rtkit *rtk,
					   unsigned int state)
{
	u64 msg;
	int ret;

	if (rtk->epmap_only)
		return -EPERM;

	reinit_completion(&rtk->iop_pwr_ack_completion);

	msg = FIELD_PREP(APPLE_RTKIT_MGMT_PWR_STATE, state);
	ret = apple_rtkit_management_send(rtk, APPLE_RTKIT_MGMT_SET_IOP_PWR_STATE,
					  msg);
	if (ret)
		return ret;

	ret = apple_rtkit_wait_for_completion(&rtk->iop_pwr_ack_completion);
	if (ret)
		return ret;

	if (rtk->iop_power_state != state)
		return -EINVAL;
	return 0;
}

static int apple_rtkit_boot_common(struct apple_rtkit *rtk, bool early_ap_power)
{
	int ret;

	if (rtk->epmap_only)
		return -EOPNOTSUPP;

	if (apple_rtkit_is_running(rtk))
		return 0;
	if (rtk->crashed)
		return -EINVAL;

	dev_dbg(rtk->dev, "RTKit: waiting for boot to finish\n");
	ret = apple_rtkit_wait_for_completion(&rtk->epmap_completion);
	if (ret)
		return ret;
	if (rtk->boot_result)
		return rtk->boot_result;

	/* Some helpers wait for AP=ON before acknowledging their own ON state. */
	if (early_ap_power) {
		ret = apple_rtkit_set_ap_power_state(rtk, APPLE_RTKIT_PWR_STATE_ON);
		if (ret)
			return ret;
	}

	dev_dbg(rtk->dev, "RTKit: waiting for IOP power state ACK\n");
	ret = apple_rtkit_wait_for_completion(&rtk->iop_pwr_ack_completion);
	if (ret)
		return ret;

	return early_ap_power ? 0 :
		apple_rtkit_set_ap_power_state(rtk, APPLE_RTKIT_PWR_STATE_ON);
}

int apple_rtkit_boot(struct apple_rtkit *rtk)
{
	return apple_rtkit_boot_common(rtk, false);
}
EXPORT_SYMBOL_GPL(apple_rtkit_boot);

int apple_rtkit_discover_epmap(struct apple_rtkit *rtk)
{
	int ret;

	if (!rtk->epmap_only)
		return -EOPNOTSUPP;

	ret = apple_rtkit_wait_for_completion(&rtk->epmap_completion);
	if (ret)
		return ret;

	return rtk->boot_result;
}
EXPORT_SYMBOL_GPL(apple_rtkit_discover_epmap);

int apple_rtkit_get_epmap_snapshot(
	struct apple_rtkit *rtk, struct apple_rtkit_epmap_snapshot *snapshot)
{
	if (!rtk->epmap_only)
		return -EOPNOTSUPP;
	if (!snapshot)
		return -EINVAL;
	if (!completion_done(&rtk->epmap_completion))
		return -EAGAIN;
	if (rtk->boot_result)
		return rtk->boot_result;

	snapshot->raw_hello = rtk->raw_hello;
	snapshot->hello_min_version = rtk->hello_min_version;
	snapshot->hello_max_version = rtk->hello_max_version;
	bitmap_to_arr64(snapshot->endpoints, rtk->discovery_endpoints,
			APPLE_RTKIT_MAX_ENDPOINTS);

	return 0;
}
EXPORT_SYMBOL_GPL(apple_rtkit_get_epmap_snapshot);

int apple_rtkit_adopt_running(struct apple_rtkit *rtk)
{
	if (rtk->epmap_only)
		return -EPERM;
	if (rtk->crashed)
		return -EINVAL;

	apple_rtkit_mark_running(rtk);

	return 0;
}
EXPORT_SYMBOL_GPL(apple_rtkit_adopt_running);

int apple_rtkit_shutdown(struct apple_rtkit *rtk)
{
	int ret;

	/* if OFF is used here the co-processor will not wake up again */
	ret = apple_rtkit_set_ap_power_state(rtk,
					     APPLE_RTKIT_PWR_STATE_QUIESCED);
	if (ret)
		return ret;

	ret = apple_rtkit_set_iop_power_state(rtk, APPLE_RTKIT_PWR_STATE_SLEEP);
	if (ret)
		return ret;

	return apple_rtkit_reinit(rtk);
}
EXPORT_SYMBOL_GPL(apple_rtkit_shutdown);

int apple_rtkit_poweroff(struct apple_rtkit *rtk)
{
	int ret;

	ret = apple_rtkit_set_ap_power_state(rtk, APPLE_RTKIT_PWR_STATE_OFF);
	if (ret)
		return ret;

	ret = apple_rtkit_set_iop_power_state(rtk, APPLE_RTKIT_PWR_STATE_OFF);
	if (ret)
		return ret;

	return apple_rtkit_reinit(rtk);
}
EXPORT_SYMBOL_GPL(apple_rtkit_poweroff);

int apple_rtkit_idle(struct apple_rtkit *rtk)
{
	int ret;

	/* if OFF is used here the co-processor will not wake up again */
	ret = apple_rtkit_set_ap_power_state(rtk,
					     APPLE_RTKIT_PWR_STATE_IDLE);
	if (ret)
		return ret;

	ret = apple_rtkit_set_iop_power_state(rtk, APPLE_RTKIT_PWR_STATE_IDLE);
	if (ret)
		return ret;

	rtk->iop_power_state = APPLE_RTKIT_PWR_STATE_IDLE;
	rtk->ap_power_state = APPLE_RTKIT_PWR_STATE_IDLE;
	return 0;
}
EXPORT_SYMBOL_GPL(apple_rtkit_idle);

int apple_rtkit_quiesce(struct apple_rtkit *rtk)
{
	int ret;

	ret = apple_rtkit_set_ap_power_state(rtk,
					     APPLE_RTKIT_PWR_STATE_QUIESCED);
	if (ret)
		return ret;

	ret = apple_rtkit_set_iop_power_state(rtk,
					      APPLE_RTKIT_PWR_STATE_QUIESCED);
	if (ret)
		return ret;

	ret = apple_rtkit_reinit(rtk);
	if (ret)
		return ret;

	rtk->iop_power_state = APPLE_RTKIT_PWR_STATE_QUIESCED;
	rtk->ap_power_state = APPLE_RTKIT_PWR_STATE_QUIESCED;
	return 0;
}
EXPORT_SYMBOL_GPL(apple_rtkit_quiesce);

int apple_rtkit_poweroff_adopted(struct apple_rtkit *rtk)
{
	int ret;

	if (!rtk->adopted || rtk->epmap_only)
		return -EINVAL;

	/*
	 * Native H17P RTBuddy waits for the separate type-0xc idle notification
	 * after its power transition. An OFF ACK alone does not establish that
	 * the firmware has finished retiring its work and can be stopped.
	 */
	reinit_completion(&rtk->iop_idle_completion);
	WRITE_ONCE(rtk->awaiting_iop_idle, true);
	ret = apple_rtkit_set_iop_power_state(rtk, APPLE_RTKIT_PWR_STATE_OFF);
	if (!ret)
		ret = apple_rtkit_wait_for_completion(&rtk->iop_idle_completion);
	WRITE_ONCE(rtk->awaiting_iop_idle, false);
	if (ret)
		return ret;

	dev_info(rtk->dev, "RTKit: inherited IOP OFF and idle acknowledged\n");
	return apple_rtkit_reinit(rtk);
}
EXPORT_SYMBOL_GPL(apple_rtkit_poweroff_adopted);

static int apple_rtkit_wake_common(struct apple_rtkit *rtk, bool early_ap_power)
{
	u64 msg;
	int ret;

	if (rtk->epmap_only)
		return -EPERM;

	if (apple_rtkit_is_running(rtk))
		return -EINVAL;

	reinit_completion(&rtk->iop_pwr_ack_completion);

	/*
	 * Use open-coded apple_rtkit_set_iop_power_state since apple_rtkit_boot
	 * will wait for the completion anyway.
	 */
	msg = FIELD_PREP(APPLE_RTKIT_MGMT_PWR_STATE, APPLE_RTKIT_PWR_STATE_INIT);
	ret = apple_rtkit_management_send(rtk, APPLE_RTKIT_MGMT_SET_IOP_PWR_STATE,
					  msg);
	if (ret)
		return ret;

	return apple_rtkit_boot_common(rtk, early_ap_power);
}

int apple_rtkit_wake(struct apple_rtkit *rtk)
{
	return apple_rtkit_wake_common(rtk, false);
}
EXPORT_SYMBOL_GPL(apple_rtkit_wake);

int apple_rtkit_wake_early_ap(struct apple_rtkit *rtk)
{
	return apple_rtkit_wake_common(rtk, true);
}
EXPORT_SYMBOL_GPL(apple_rtkit_wake_early_ap);

void apple_rtkit_free(struct apple_rtkit *rtk)
{
	/*
	 * Let already-running callbacks finish while the transport is still
	 * powered, but make every callback observed after this point a no-op.
	 * Work queued by an IRQ racing the first flush is drained by
	 * destroy_workqueue() after apple_mbox_stop() has synchronously disabled
	 * that IRQ.
	 */
	WRITE_ONCE(rtk->shutting_down, true);
	flush_workqueue(rtk->wq);
	apple_mbox_stop(rtk->mbox);
	rtk->mbox->rx = NULL;
	destroy_workqueue(rtk->wq);
	rtk->mbox->cookie = NULL;

	apple_rtkit_free_buffer(rtk, &rtk->ioreport_buffer);
	apple_rtkit_free_buffer(rtk, &rtk->crashlog_buffer);
	apple_rtkit_free_buffer(rtk, &rtk->oslog_buffer);
	apple_rtkit_free_buffer(rtk, &rtk->syslog_buffer);
	apple_rtkit_free_buffer(rtk, &rtk->pmp_aux_buffer);

	kfree(rtk->syslog_msg_buffer);
	kfree(rtk);
}
EXPORT_SYMBOL_GPL(apple_rtkit_free);

static void apple_rtkit_free_wrapper(void *data)
{
	apple_rtkit_free(data);
}

struct apple_rtkit *devm_apple_rtkit_init(struct device *dev, void *cookie,
					  const char *mbox_name, int mbox_idx,
					  const struct apple_rtkit_ops *ops)
{
	struct apple_rtkit *rtk;
	int ret;

	rtk = apple_rtkit_init(dev, cookie, mbox_name, mbox_idx, ops);
	if (IS_ERR(rtk))
		return rtk;

	ret = devm_add_action_or_reset(dev, apple_rtkit_free_wrapper, rtk);
	if (ret)
		return ERR_PTR(ret);

	return rtk;
}
EXPORT_SYMBOL_GPL(devm_apple_rtkit_init);

struct apple_rtkit *devm_apple_rtkit_init_adopted(
	struct device *dev, void *cookie, const char *mbox_name, int mbox_idx,
	const struct apple_rtkit_ops *ops)
{
	struct apple_rtkit *rtk;
	int ret;

	rtk = apple_rtkit_init_adopted(dev, cookie, mbox_name, mbox_idx, ops);
	if (IS_ERR(rtk))
		return rtk;

	ret = devm_add_action_or_reset(dev, apple_rtkit_free_wrapper, rtk);
	if (ret)
		return ERR_PTR(ret);

	return rtk;
}
EXPORT_SYMBOL_GPL(devm_apple_rtkit_init_adopted);

void devm_apple_rtkit_free(struct device *dev, struct apple_rtkit *rtk)
{
	devm_release_action(dev, apple_rtkit_free_wrapper, rtk);
}
EXPORT_SYMBOL_GPL(devm_apple_rtkit_free);

MODULE_LICENSE("Dual MIT/GPL");
MODULE_AUTHOR("Sven Peter <sven@svenpeter.dev>");
MODULE_DESCRIPTION("Apple RTKit driver");
