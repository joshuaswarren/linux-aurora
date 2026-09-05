// SPDX-License-Identifier: GPL-2.0-only OR MIT
/*
 * Copyright 2021 Alyssa Rosenzweig
 * Copyright The Asahi Linux Contributors
 */

#include <linux/align.h>
#include <linux/bitmap.h>
#include <linux/clk.h>
#include <linux/completion.h>
#include <linux/delay.h>
#include <linux/dma-mapping.h>
#include <linux/iommu.h>
#include <linux/kref.h>
#include <linux/module.h>
#include <linux/of_device.h>
#include <linux/pm_runtime.h>
#include <linux/slab.h>

#include <drm/drm_fb_dma_helper.h>
#include <drm/drm_fourcc.h>

/* macOS uses 6 for its single layer in the captured swaps */
#define DCP_SWAP_SURFACE_ID_BASE 6
/* from the captured macOS swap records */
#define DCP_SWAP_FLAGS1 0x34865202ULL
#define DCP_SWAP_FLAGS2 0x4ULL
#include <drm/drm_framebuffer.h>
#include <drm/drm_probe_helper.h>
#include <drm/drm_vblank.h>

#include "dcp.h"
#include "dcp-internal.h"
#include "iomfb.h"
#include "iomfb_internal.h"
#include "parser.h"
#include "trace.h"
#include "version_utils.h"

/* Register defines used in bandwidth setup structure */
#define REG_DOORBELL_BIT(idx) (2 + (idx))

struct dcp_wait_cookie {
	struct kref refcount;
	struct completion done;
};

static void release_wait_cookie(struct kref *ref)
{
	struct dcp_wait_cookie *cookie;
	cookie = container_of(ref, struct dcp_wait_cookie, refcount);

        kfree(cookie);
}

DCP_THUNK_OUT(iomfb_a131_pmu_service_matched, iomfbep_a131_pmu_service_matched, u32);
DCP_THUNK_OUT(iomfb_a132_backlight_service_matched, iomfbep_a132_backlight_service_matched, u32);
DCP_THUNK_OUT(iomfb_a358_vi_set_temperature_hint, iomfbep_a358_vi_set_temperature_hint, u32);

IOMFB_THUNK_INOUT(set_matrix);
IOMFB_THUNK_INOUT(get_color_remap_mode);
IOMFB_THUNK_INOUT(last_client_close);
IOMFB_THUNK_INOUT(abort_swaps_dcp);

DCP_THUNK_INOUT(dcp_swap_submit, dcpep_swap_submit,
		struct DCP_FW_NAME(dcp_swap_submit_req),
		struct DCP_FW_NAME(dcp_swap_submit_resp));

DCP_THUNK_INOUT(dcp_swap_start, dcpep_swap_start, struct DCP_FW_NAME(dcp_swap_start_req),
		struct DCP_FW_NAME(dcp_swap_start_resp));

DCP_THUNK_INOUT(dcp_set_power_state, dcpep_set_power_state,
		struct DCP_FW_NAME(dcp_set_power_state_req),
		struct dcp_set_power_state_resp);

/*
 * macOS issues A473 out-of-band: the traced call is "OOBCMD 0x0 A473 0x10 0x8"
 * with unklong = 1.  Sent on the ordinary command channel the firmware simply
 * never answers it, which strands the power transition and every later call
 * behind it.
 */
#if DCP_FW_VER >= DCP_FW_VERSION(26, 0, 0)
#define DCP_POWER_OOB	true
#else
#define DCP_POWER_OOB	false
#endif

DCP_THUNK_INOUT(dcp_set_digital_out_mode, dcpep_set_digital_out_mode,
		struct dcp_set_digital_out_mode_req, u32);

DCP_THUNK_INOUT(dcp_set_display_device, dcpep_set_display_device, u32, u32);

DCP_THUNK_OUT(dcp_set_display_refresh_properties,
	      dcpep_set_display_refresh_properties, u32);

#if DCP_FW_VER >= DCP_FW_VERSION(13, 2, 0)
DCP_THUNK_INOUT(dcp_late_init_signal, dcpep_late_init_signal, u32, u32);
#else
DCP_THUNK_OUT(dcp_late_init_signal, dcpep_late_init_signal, u32);
#endif
DCP_THUNK_IN(dcp_flush_supports_power, dcpep_flush_supports_power, u32);
DCP_THUNK_OUT(dcp_create_default_fb, dcpep_create_default_fb, u32);
DCP_THUNK_OUT(dcp_start_signal, dcpep_start_signal, u32);
DCP_THUNK_VOID(dcp_setup_video_limits, dcpep_setup_video_limits);
DCP_THUNK_VOID(dcp_set_create_dfb, dcpep_set_create_dfb);

#if DCP_FW_VER >= DCP_FW_VERSION(26, 0, 0)
/*
 * The default-framebuffer interrogation macOS performs inside D121.  The
 * getters are called with a zeroed input of the traced size; macOS leaves the
 * tail as poison, so only the size matters.
 */
struct dcp_dfb_info_h17p {
	u8 data[0x2c];
} __packed;

struct dcp_dfb_layout_req_h17p {
	u8 data[0xc];
} __packed;

struct dcp_dfb_dimensions_h17p {
	u32 width;
	u32 height;
} __packed;

DCP_THUNK_INOUT(dcp_get_dfb_compression_info, dcpep_get_dfb_compression_info,
		u64, u64);
DCP_THUNK_INOUT(dcp_get_dfb_info, dcpep_get_dfb_info,
		struct dcp_dfb_info_h17p, struct dcp_dfb_info_h17p);
DCP_THUNK_INOUT(dcp_get_dfb_layout, dcpep_get_dfb_layout,
		struct dcp_dfb_layout_req_h17p, u64);
DCP_THUNK_INOUT(dcp_get_dfb_state, dcpep_get_dfb_state, u32, u64);
DCP_THUNK_IN(dcp_set_dfb_dimensions, dcpep_set_dfb_dimensions,
	     struct dcp_dfb_dimensions_h17p);
DCP_THUNK_VOID(dcp_commit_dfb_info, dcpep_commit_dfb_info);
DCP_THUNK_OUT(dcp_dfb_query, dcpep_dfb_query, u32);
DCP_THUNK_OUT(dcp_dfb_ready_query, dcpep_dfb_ready_query, u32);

/*
 * macOS runs a fixed sequence of pipe-configuration calls between the boot
 * chain and its first swap.  We do not have names for most of them, but the
 * capture gives exact sizes and payloads, and replaying it verbatim is what
 * takes the firmware from "booted" to "willing to scan out".
 */
struct dcp_pipe_cfg_415 {
	u8 data[0xc];
} __packed;

/*
 * A031 is update_notify_clients_dcp(u32[27]): a bitmap-as-array of the DCP
 * notification clients the AP wants delivered.  macOS enables them
 * cumulatively over nine calls, ending at {6,7,8,10,11,12,25,26}; we used to
 * send nine zeroed records, so we finished with no notify clients registered
 * at all and never received the callbacks they gate.
 */
struct dcp_pipe_cfg_031 {
	__le32 clients[27];
} __packed;
static_assert(sizeof(struct dcp_pipe_cfg_031) == 0x6c);

DCP_THUNK_OUT(dcp_pipe_query_353, dcpep_pipe_query_353, u64);
DCP_THUNK_INOUT(dcp_pipe_set_352, dcpep_pipe_set_352, u64, u32);
DCP_THUNK_INOUT(dcp_pipe_cfg_415, dcpep_pipe_cfg_415,
		struct dcp_pipe_cfg_415, struct dcp_pipe_cfg_415);
DCP_THUNK_IN(dcp_pipe_cfg_031, dcpep_pipe_cfg_031, struct dcp_pipe_cfg_031);
DCP_THUNK_INOUT(dcp_pipe_cfg_414, dcpep_pipe_cfg_414, u64, u64);
DCP_THUNK_OUT(dcp_pipe_query_478, dcpep_pipe_query_478, u32);
DCP_THUNK_OUT(dcp_pipe_query_474, dcpep_pipe_query_474, u32);
DCP_THUNK_IN(dcp_pipe_set_476, dcpep_pipe_set_476, u32);
DCP_THUNK_INOUT(dcp_pipe_set_428, dcpep_pipe_set_428, u32, u32);

/*
 * A352 and the five A438 pushes macOS runs between A353 and the client-open
 * sequence.  Every byte is taken from the 25G83 capture; the 8-byte host
 * leading 8 bytes are sent
 * verbatim: zeroing them made the firmware take a CPU exception
 * (exception.c:59) on the third push.
 */
struct dcp_pipe_cfg_438 {
	u8 data[0x106c];
} __packed;

DCP_THUNK_INOUT(dcp_pipe_cfg_438, dcpep_pipe_cfg_438,
		struct dcp_pipe_cfg_438, u32);

/* Generated from the macOS 25G83 capture: five A438 pushes, 0x106c each,
 * byte-exact including the leading 8 bytes. */

#endif
DCP_THUNK_VOID(dcp_first_client_open, dcpep_first_client_open);

DCP_THUNK_INOUT(dcp_set_parameter_dcp, dcpep_set_parameter_dcp,
		struct dcp_set_parameter_dcp, u32);

DCP_THUNK_INOUT(dcp_enable_disable_video_power_savings,
		dcpep_enable_disable_video_power_savings, u32, int);

DCP_THUNK_OUT(dcp_is_main_display, dcpep_is_main_display, u32);

/* DCP callback handlers */
static void dcpep_cb_nop(struct apple_dcp *dcp)
{
	/* No operation */
}

static u8 dcpep_cb_true(struct apple_dcp *dcp)
{
	return true;
}

static u8 dcpep_cb_false(struct apple_dcp *dcp)
{
	return false;
}

static u32 dcpep_cb_zero(struct apple_dcp *dcp)
{
	return 0;
}

/*
 * Bind every framebuffer displaced by the commit that produced this swap to
 * that swap's id.
 *
 * The id only exists once the firmware answers swap_start, after the atomic
 * commit has queued its entries, so they are queued unarmed and stamped here.
 * Swap-start requests are serialized by the DCP command stack.
 */
static void dcp_arm_swapped_out_fbs(struct apple_dcp *dcp, u32 swap_id)
{
	struct dcp_fb_reference *entry;
	unsigned long flags;

	spin_lock_irqsave(&dcp->swapped_out_lock, flags);
	list_for_each_entry(entry, &dcp->swapped_out_fbs, head) {
		if (entry->armed)
			continue;
		entry->swap_id = swap_id;
		entry->armed = true;
	}
	spin_unlock_irqrestore(&dcp->swapped_out_lock, flags);
}

/*
 * Drop framebuffers whose unbinding swap has completed.
 *
 * Entries must be keyed to the swap that unbinds them, rather than the last
 * swap that happened to have completed when the atomic commit ran.  A later
 * successful completion also retires entries whose own completion was lost;
 * the signed difference keeps that comparison valid across a 32-bit wrap.
 */
static void dcp_release_swapped_out_fbs(struct apple_dcp *dcp)
{
	struct dcp_fb_reference *entry, *tmp;
	unsigned long flags;
	LIST_HEAD(done);

	/*
	 * last_swap_id / have_swap_complete are only ever touched by the RTKit
	 * worker, which is also the only caller of this function, so the lock
	 * is here for the list alone.
	 */
	spin_lock_irqsave(&dcp->swapped_out_lock, flags);
	while (!list_empty(&dcp->swapped_out_fbs)) {
		entry = list_first_entry(&dcp->swapped_out_fbs,
					 struct dcp_fb_reference, head);

		/* Newest entries; their swap has not been started yet. */
		if (!entry->armed)
			break;
		/* last_swap_id is 0 from kzalloc until a swap really lands. */
		if (!dcp->have_swap_complete)
			break;
		/* Hold the old framebuffer for one additional completed swap. */
		if ((s32)(dcp->last_swap_id - entry->swap_id) <= 0)
			break;

		list_move_tail(&entry->head, &done);
	}
	spin_unlock_irqrestore(&dcp->swapped_out_lock, flags);

	/* drm_framebuffer_put() can sleep, so never call it under the lock. */
	list_for_each_entry_safe(entry, tmp, &done, head) {
		if (entry->fb)
			drm_framebuffer_put(entry->fb);
		list_del(&entry->head);
		kfree(entry);
	}
}

static void dcpep_cb_swap_complete(struct apple_dcp *dcp,
				   struct DCP_FW_NAME(dc_swap_complete_resp) *resp)
{
	ktime_t now = ktime_get();
	trace_iomfb_swap_complete(dcp, resp->swap_id);
	dcp->last_swap_id = resp->swap_id;
	dcp->have_swap_complete = true;

	dcp_drm_crtc_page_flip(dcp, now);
	dcp_release_swapped_out_fbs(dcp);
	if (dcp->crc_enabled) {
		u32 crc32 = 0;
		drm_crtc_add_crc_entry(&dcp->crtc->base, true, resp->swap_id, &crc32);
	}
}

/* special */
static void complete_vi_set_temperature_hint(struct apple_dcp *dcp, void *out, void *cookie)
{
	// ack D100 cb_match_pmu_service
	dcp_ack(dcp, DCP_CONTEXT_CB);
}

static bool iomfbep_cb_match_pmu_service(struct apple_dcp *dcp, int tag, void *out, void *in)
{
	trace_iomfb_callback(dcp, tag, __func__);
	iomfb_a358_vi_set_temperature_hint(dcp, false,
					   complete_vi_set_temperature_hint,
					   NULL);

	// return false for deferred ACK
	return false;
}

/*
 * H17P asks the AP to make the default framebuffer surface (D582).  Answering
 * with a bare boolean is not enough: macOS registers an actual surface from
 * inside that callback with a nested A472 carrying a 0x22c descriptor, and
 * only then reports success.  Without it the firmware has no surface, programs
 * the display engine from a null base and the piodma DART faults forever.
 *
 * The descriptor below is the one macOS 25G83 sends on this panel; every byte
 * it did not write was left as tracer poison, so the rest is zero.  ARGB at
 * +0x0b is the format field of struct dcp_surface.
 */
struct dcp_h17p_dfb_surface_req {
	u8 surface[0x22c];
	u8 surface_null;
	u8 padding[3];
} __packed;
static_assert(sizeof(struct dcp_h17p_dfb_surface_req) == 0x230);

/* macOS sends this with a 4-byte output; with out_len 0 the firmware has
 * nowhere to write its result and simply stops answering. */
DCP_THUNK_INOUT(dcp_register_dfb_surface, dcpep_register_dfb_surface,
		struct dcp_h17p_dfb_surface_req, u32);

/* Fill the common SurfaceContents fields, matching the traced macOS bytes. */

static void dcp_ack_dfb_surface(struct apple_dcp *dcp)
{
	struct dcp_channel *ch = &dcp->ch_cb;
	u8 *succ = ch->output[ch->depth - 1];

	*succ = true;
	dcp_ack(dcp, DCP_CONTEXT_CB);
}

static void dcp_fill_dfb_surface(struct apple_dcp *dcp, u8 *s)
{
	s[0x002] = 0x01;
	s[0x00b] = 'A';
	s[0x00c] = 'R';
	s[0x00d] = 'G';
	s[0x00e] = 'B';
	s[0x019] = 0x01;
	s[0x01b] = 0x01;
	s[0x01c] = 0x01;
	s[0x02a] = 0x40;
	s[0x02b] = 0xe4;
	s[0x035] = 0x04;	/* IOSurface.surface_id; macOS writes 4, we wrote 3 */
	s[0x051] = 0x01;
	s[0x149] = 0x01;

	if (dcp->hw.iomfb_method_profile == DCP_IOMFB_METHODS_H17G) {
		/* H17G 25G76 A472: b3a8 surface and the native default surface ID.
		 * The word at0x29 remains opaque; these are its observed bytes,
		 * not a size inferred from panel geometry. No addresses are copied.
		 */
		memcpy(s + 0x00b, "8a3b", 4);
		s[0x02a] = 0;
		s[0x02b] = 0x4d;
		s[0x02c] = 1;
		s[0x035] = 2;
	}
}

/*
 * A471 (update_dfb) is never sent.  It hands the firmware a default-framebuffer
 * surface descriptor, and on H17P it makes the coprocessor take an AXI read
 * error and NMI immediately (axi_rd_err -> "panic(app): app NMI").  Scanout does
 * not need it: the swap carries its own surface, whose DVA comes from the DRM
 * GEM object on the display-subsystem device and is therefore already mapped in
 * the stream the pipe reads.  Just acknowledge the callback.
 */
static void complete_dfb_surface(struct apple_dcp *dcp, void *out, void *cookie)
{
	dcp_ack_dfb_surface(dcp);
}

static bool iomfbep_cb_create_dfb_surface(struct apple_dcp *dcp, int tag,
					  void *out, void *in)
{
	struct dcp_h17p_dfb_surface_req *req;

	trace_iomfb_callback(dcp, tag, __func__);

	/*
	 * 0x230 bytes is far too much for the kernel stack here: this runs deep
	 * inside the RTKit receive workqueue (rx work -> iomfb_recv_msg ->
	 * dcpep_handle_cb -> here -> dcp_push), and putting it on the stack
	 * overflows it and panics the machine.  dcp_push() copies the payload
	 * into shared memory before returning, so a short-lived allocation is
	 * all we need.
	 */
	req = kzalloc(sizeof(*req), GFP_KERNEL);
	if (!req) {
		dev_err(dcp->dev, "no memory for the default FB surface\n");
		return true;
	}

	dcp_fill_dfb_surface(dcp, req->surface);

	dcp_register_dfb_surface(dcp, false, req, complete_dfb_surface, NULL);
	kfree(req);

	/* deferred ACK: the A472 completion reports success for us */
	return false;
}

static void complete_pmu_service_matched(struct apple_dcp *dcp, void *out, void *cookie)
{
	struct dcp_channel *ch = &dcp->ch_cb;
	u8 *succ = ch->output[ch->depth - 1];

	*succ = true;

	// ack D206 cb_match_pmu_service_2
	dcp_ack(dcp, DCP_CONTEXT_CB);
}

static bool iomfbep_cb_match_pmu_service_2(struct apple_dcp *dcp, int tag, void *out, void *in)
{
	trace_iomfb_callback(dcp, tag, __func__);

	iomfb_a131_pmu_service_matched(dcp, false, complete_pmu_service_matched,
				       out);

	// return false for deferred ACK
	return false;
}

static void complete_backlight_service_matched(struct apple_dcp *dcp, void *out, void *cookie)
{
	struct dcp_channel *ch = &dcp->ch_cb;
	u8 *succ = ch->output[ch->depth - 1];

	*succ = true;

	// ack D206 cb_match_backlight_service
	dcp_ack(dcp, DCP_CONTEXT_CB);
}

static bool iomfbep_cb_match_backlight_service(struct apple_dcp *dcp, int tag, void *out, void *in)
{
	trace_iomfb_callback(dcp, tag, __func__);

	if (!dcp_has_panel(dcp)) {
		u8 *succ = out;
		*succ = true;
		return true;
	}

	iomfb_a132_backlight_service_matched(dcp, false, complete_backlight_service_matched, out);

	// return false for deferred ACK
	return false;
}

static void iomfb_cb_pr_publish(struct apple_dcp *dcp, struct iomfb_property *prop)
{
	switch (prop->id) {
	case IOMFB_PROPERTY_NITS:
	{
		if (dcp_has_panel(dcp)) {
			dcp->brightness.nits = prop->value / dcp->brightness.scale;
			/* notify backlight device of the initial brightness */
			if (!dcp->brightness.bl_dev && dcp->brightness.maximum > 0)
				schedule_work(&dcp->bl_register_wq);
			trace_iomfb_brightness(dcp, prop->value);
		}
		break;
	}
	default:
		dev_dbg(dcp->dev, "pr_publish: id: %d = %u\n", prop->id, prop->value);
	}
}

static struct dcp_get_uint_prop_resp
dcpep_cb_get_uint_prop(struct apple_dcp *dcp, struct dcp_get_uint_prop_req *req)
{
	struct dcp_get_uint_prop_resp resp = (struct dcp_get_uint_prop_resp){
	    .value = 0
	};

	if (dcp->panel.has_mini_led &&
	    memcmp(req->obj, "SUMP", sizeof(req->obj)) == 0) { /* "PMUS */
	    if (strncmp(req->key, "Temperature", sizeof(req->key)) == 0) {
		/*
		 * TODO: value from j314c, find out if it is temperature in
		 *       centigrade C and which temperature sensor reports it
		 */
		resp.value = 3029;
		resp.ret = true;
	    }
	}

	return resp;
}

static u8 iomfbep_cb_sr_set_property_int(struct apple_dcp *dcp,
					 struct iomfb_sr_set_property_int_req *req)
{
	if (memcmp(req->obj, "FMOI", sizeof(req->obj)) == 0) { /* "IOMF */
		if (strncmp(req->key, "Brightness_Scale", sizeof(req->key)) == 0) {
			if (!req->value_null)
				dcp->brightness.scale = req->value;
		}
	}

	return 1;
}

static void iomfbep_cb_set_fx_prop(struct apple_dcp *dcp, struct iomfb_set_fx_prop_req *req)
{
    // TODO: trace this, see if there properties which needs to used later
}

/*
 * Callback to map a buffer allocated with allocate_buf for PIODMA usage.
 * PIODMA is separate from the main DCP and uses own IOVA space on a dedicated
 * stream of the display DART, rather than the expected DCP DART.
 */
static struct DCP_FW_NAME(dcp_map_buf_resp) dcpep_cb_map_piodma(struct apple_dcp *dcp,
						   struct dcp_map_buf_req *req)
{
	struct dcp_mem_descriptor *memdesc;
	struct sg_table *map;
	ssize_t ret;

	if (req->buffer >= ARRAY_SIZE(dcp->memdesc))
		goto reject;

	memdesc = &dcp->memdesc[req->buffer];
	map = &memdesc->map;

	if (!map->sgl)
		goto reject;

	/* use the piodma iommu domain to map against the right IOMMU */
	ret = iommu_map_sgtable(dcp->iommu_dom, memdesc->dva, map,
				IOMMU_READ | IOMMU_WRITE);

	/* HACK: expect size to be 16K aligned since the iommu API only maps
	 *       full pages
	 */
	if (ret < 0 || ret != ALIGN(memdesc->size, SZ_16K)) {
		dev_err(dcp->dev, "iommu_map_sgtable() returned %zd instead of expected buffer size of %zu\n", ret, memdesc->size);
		goto reject;
	}

	/* The PIODMA mapping belongs only to the DCP IOMMU domain. */
	memdesc->piodma_mapped = true;

#if DCP_FW_VER >= DCP_FW_VERSION(26, 0, 0)
	/* echo the index back, as macOS does */
	return (struct DCP_FW_NAME(dcp_map_buf_resp)){
		.buffer = req->buffer,
		.dva = memdesc->dva,
	};
#else
	return (struct DCP_FW_NAME(dcp_map_buf_resp)){ .dva = memdesc->dva };
#endif

reject:
	dev_err(dcp->dev, "denying map of invalid buffer %llx for piodma\n",
		req->buffer);
#if DCP_FW_VER >= DCP_FW_VERSION(26, 0, 0)
#if DCP_FW_VER >= DCP_FW_VERSION(26, 0, 0)
	return (struct DCP_FW_NAME(dcp_map_buf_resp)){
		.buffer = req->buffer,
		.dva = 0,
	};
#else
	return (struct DCP_FW_NAME(dcp_map_buf_resp)){ .dva = 0 };
#endif
#else
	return (struct DCP_FW_NAME(dcp_map_buf_resp)){ .ret = EINVAL };
#endif
}

static void dcpep_cb_unmap_piodma(struct apple_dcp *dcp,
				  struct dcp_unmap_buf_resp *resp)
{
	struct dcp_mem_descriptor *memdesc;

	if (resp->buffer >= ARRAY_SIZE(dcp->memdesc)) {
		dev_warn(dcp->dev, "unmap request for out of range buffer %llu\n",
			 resp->buffer);
		return;
	}

	memdesc = &dcp->memdesc[resp->buffer];

	if (!memdesc->buf) {
		dev_warn(dcp->dev,
			 "unmap for non-mapped buffer %llu iova:0x%08llx\n",
			 resp->buffer, resp->dva);
		return;
	}

	if (memdesc->dva != resp->dva) {
		dev_warn(dcp->dev, "unmap buffer %llu address mismatch "
			 "memdesc.dva:%llx dva:%llx\n", resp->buffer,
			 memdesc->dva, resp->dva);
		return;
	}

	/* use the piodma iommu domain to unmap from the right IOMMU */
	/* HACK: expect size to be 16K aligned since the iommu API only maps
	 *       full pages
	 */
	iommu_unmap(dcp->iommu_dom, memdesc->dva, ALIGN(memdesc->size, SZ_16K));
	memdesc->piodma_mapped = false;
}

/*
 * Allocate an IOVA contiguous buffer mapped to the DCP. The buffer need not be
 * physically contiguous, however we should save the sgtable in case the
 * buffer needs to be later mapped for PIODMA.
 */
static struct dcp_allocate_buffer_resp
dcpep_cb_allocate_buffer(struct apple_dcp *dcp,
			 struct dcp_allocate_buffer_req *req)
{
	struct dcp_allocate_buffer_resp resp = { 0 };
	struct dcp_mem_descriptor *memdesc;
	size_t size;
	u32 id;

	resp.dva_size = ALIGN(req->size, 4096);
	resp.mem_desc_id =
		find_first_zero_bit(dcp->memdesc_map, DCP_MAX_MAPPINGS);

	if (resp.mem_desc_id >= DCP_MAX_MAPPINGS) {
		dev_warn(dcp->dev, "DCP overflowed mapping table, ignoring\n");
		resp.dva_size = 0;
		resp.mem_desc_id = 0;
		return resp;
	}
	id = resp.mem_desc_id;
	set_bit(id, dcp->memdesc_map);

	memdesc = &dcp->memdesc[id];

	memdesc->piodma_mapped = false;
	memdesc->size = resp.dva_size;
	/* HACK: align size to 16K since the iommu API only maps full pages */
	size = ALIGN(resp.dva_size, SZ_16K);
	memdesc->buf = dma_alloc_coherent(dcp->dev, size,
					  &memdesc->dva, GFP_KERNEL);

	dma_get_sgtable(dcp->dev, &memdesc->map, memdesc->buf, memdesc->dva,
			size);
	resp.dva = memdesc->dva;

	/*
	 * Report the physical address too.  We only ever filled in the device
	 * address, leaving paddr zero; macOS answers D451 with a real physical
	 * address (e.g. 0x100_2651_4000) alongside the DVA.  The firmware loads
	 * its APT M3 microcode -- "load APT M3 IMem/DMem" appears immediately
	 * after these allocations -- through that address, so with zero there
	 * the timing sequencer comes up and is powered straight back down.
	 */
	if (memdesc->map.sgl)
		resp.paddr = sg_phys(memdesc->map.sgl);

	return resp;
}

static u8 dcpep_cb_release_mem_desc(struct apple_dcp *dcp, u32 *mem_desc_id)
{
	struct dcp_mem_descriptor *memdesc;
	size_t size;
	u32 id = *mem_desc_id;

	if (id >= DCP_MAX_MAPPINGS) {
		dev_warn(dcp->dev,
			 "unmap request for out of range mem_desc_id %u", id);
		return 0;
	}

	if (!test_and_clear_bit(id, dcp->memdesc_map)) {
		dev_warn(dcp->dev, "unmap request for unused mem_desc_id %u\n",
			 id);
		return 0;
	}

	memdesc = &dcp->memdesc[id];
	size = ALIGN(memdesc->size, SZ_16K);

	/*
	 * Tear down any translation still standing before the pages go back to
	 * the allocator.  The firmware is expected to unmap piodma first, but
	 * if it releases a descriptor without doing so we would hand the pages
	 * back while the DART still resolves them -- the display engine would
	 * then read whoever owns them next.
	 */
	if (memdesc->piodma_mapped) {
		dev_warn(dcp->dev,
			 "releasing buffer %u while still mapped for piodma; unmapping first\n",
			 id);
		iommu_unmap(dcp->iommu_dom, memdesc->dva, size);
		memdesc->piodma_mapped = false;
	}

	if (memdesc->buf) {
		dma_free_coherent(dcp->dev, size, memdesc->buf, memdesc->dva);
		memdesc->buf = NULL;
		memset(&memdesc->map, 0, sizeof(memdesc->map));
	} else {
		memdesc->reg = 0;
	}

	memdesc->size = 0;

	return 1;
}

/* Validate that the specified region is a display register */
static bool is_disp_register(struct apple_dcp *dcp, u64 start, u64 end)
{
	int i;

	for (i = 0; i < dcp->nr_disp_registers; ++i) {
		struct resource *r = dcp->disp_registers[i];

		if ((start >= r->start) && (end <= r->end))
			return true;
	}

	return false;
}

/*
 * Map contiguous physical memory into the DCP's address space. The firmware
 * uses this to map the display registers we advertise in
 * sr_map_device_memory_with_index, so we bounds check against that to guard
 * safe against malicious coprocessors.
 */
static struct dcp_map_physical_resp
dcpep_cb_map_physical(struct apple_dcp *dcp, struct dcp_map_physical_req *req)
{
	int size = ALIGN(req->size, 4096);
	dma_addr_t dva;
	u32 id;

	if (!is_disp_register(dcp, req->paddr, req->paddr + size - 1)) {
		dev_err(dcp->dev, "refusing to map phys address %llx size %llx\n",
			req->paddr, req->size);
		return (struct dcp_map_physical_resp){};
	}

	id = find_first_zero_bit(dcp->memdesc_map, DCP_MAX_MAPPINGS);
	set_bit(id, dcp->memdesc_map);
	dcp->memdesc[id].size = size;
	dcp->memdesc[id].reg = req->paddr;

	dva = dma_map_resource(dcp->dev, req->paddr, size, DMA_BIDIRECTIONAL, 0);
	WARN_ON(dva == DMA_MAPPING_ERROR);

	return (struct dcp_map_physical_resp){
		.dva_size = size,
		.mem_desc_id = id,
		.dva = dva,
	};
}

static u64 dcpep_cb_get_frequency(struct apple_dcp *dcp)
{
	return clk_get_rate(dcp->clk);
}

static struct DCP_FW_NAME(dcp_map_reg_resp) dcpep_cb_map_reg(struct apple_dcp *dcp,
						struct DCP_FW_NAME(dcp_map_reg_req) *req)
{
#if DCP_FW_VER >= DCP_FW_VERSION(26, 0, 0)
	/*
	 * H17P asks for one aperture the device tree does not describe on the
	 * dcp node: index 5, the disp0 window 0x302800000..0x3028bbfff.  That
	 * window holds the PMC block the firmware drives while bringing the
	 * display up, and macOS answers this exact request with
	 * addr = dva = 0x302800000, length = 0xbc000.  Refusing it (our
	 * reg-names stop at disp-4) leaves the firmware unable to program that
	 * hardware.  Answer it the way macOS does; these apertures are reached
	 * physically, so dva == addr.
	 */
	if (dcp->hw.legacy_pmc_binding && req->index == DCP_H17P_DISP0_REG_INDEX) {
		dev_info(dcp->dev, "map_reg[5]: disp0 window %#llx len %#llx\n",
			 (u64)DCP_H17P_DISP0_REG_BASE,
			 (u64)DCP_H17P_DISP0_REG_SIZE);
		return (struct DCP_FW_NAME(dcp_map_reg_resp)){
			.addr = DCP_H17P_DISP0_REG_BASE,
			.length = DCP_H17P_DISP0_REG_SIZE,
			.dva = DCP_H17P_DISP0_REG_BASE,
		};
	}
#endif

	if (req->index >= dcp->nr_disp_registers) {
		dev_warn(dcp->dev, "attempted to read invalid reg index %u\n",
			 req->index);

		return (struct DCP_FW_NAME(dcp_map_reg_resp)){ .ret = 1 };
	} else {
		struct resource *rsrc = dcp->disp_registers[req->index];
#if DCP_FW_VER >= DCP_FW_VERSION(26, 0, 0)
		/*
		 * H17P reaches these apertures physically, not through its
		 * DART: macOS 25G83 answers this callback with dva == addr
		 * (0x302800000 for index 5, length 0xbc000).  Trying to
		 * dma_map_resource() them instead fails and hands the firmware
		 * DMA_MAPPING_ERROR as a device address.
		 */
		dma_addr_t dva = rsrc->start;
#elif DCP_FW_VER >= DCP_FW_VERSION(13, 2, 0)
		dma_addr_t dva = dma_map_resource(dcp->dev, rsrc->start, resource_size(rsrc),
						  DMA_BIDIRECTIONAL, 0);
		WARN_ON(dva == DMA_MAPPING_ERROR);
		dev_info(dcp->dev, "map_reg[%u]: addr %pa len %#llx dva %#llx\n",
			 req->index, &rsrc->start,
			 (u64)resource_size(rsrc), (u64)dva);
#else
		dev_info(dcp->dev, "map_reg[%u]: addr %pa len %#llx\n",
			 req->index, &rsrc->start, (u64)resource_size(rsrc));
#endif

		return (struct DCP_FW_NAME(dcp_map_reg_resp)){
			.addr = rsrc->start,
			.length = resource_size(rsrc),
#if DCP_FW_VER >= DCP_FW_VERSION(13, 2, 0)
			.dva = dva,
#endif
		};
	}
}

static struct dcp_read_edt_data_resp
dcpep_cb_read_edt_data(struct apple_dcp *dcp, struct dcp_read_edt_data_req *req)
{
	return (struct dcp_read_edt_data_resp){
		.value[0] = req->value[0],
		.ret = 0,
	};
}

static void iomfbep_cb_enable_backlight_message_ap_gated(struct apple_dcp *dcp,
							 u8 *enabled)
{
	/*
	 * update backlight brightness on next swap, on non mini-LED displays
	 * DCP seems to set an invalid iDAC value after coming out of DPMS.
	 * syslog: "[BrightnessLCD.cpp:743][AFK]nitsToDBV: iDAC out of range"
	 */
	dcp->brightness.update = true;
	schedule_work(&dcp->bl_update_wq);
}

/* Chunked data transfer for property dictionaries */
static u8 dcpep_cb_prop_start(struct apple_dcp *dcp, u32 *length)
{
	if (dcp->chunks.data != NULL) {
		dev_warn(dcp->dev, "ignoring spurious transfer start\n");
		return false;
	}

	/*
	 * This length comes straight off the wire.  A bogus value turns the
	 * allocation below into a multi-gigabyte GFP_KERNEL request, and the
	 * box vanishes into reclaim with no oops and no fault -- it just stops.
	 * Property blobs here are a few KiB, so bound it and never let the
	 * allocator retry.
	 */
	if (*length == 0 || *length > SZ_1M) {
		dev_warn(dcp->dev, "rejecting %u byte property transfer\n",
			 *length);
		return false;
	}

	dcp->chunks.length = *length;
	dcp->chunks.data = devm_kzalloc(dcp->dev, *length,
					GFP_KERNEL | __GFP_NORETRY |
					__GFP_NOWARN);

	if (!dcp->chunks.data) {
		dev_warn(dcp->dev, "failed to allocate %u chunk bytes\n",
			 *length);
		return false;
	}
	dev_info(dcp->dev, "property transfer: %u bytes\n", *length);

	return true;
}

static u8 dcpep_cb_prop_chunk(struct apple_dcp *dcp,
			      struct dcp_set_dcpav_prop_chunk_req *req)
{
	if (!dcp->chunks.data) {
		dev_warn(dcp->dev, "ignoring spurious chunk\n");
		return false;
	}

	if (req->offset + req->length > dcp->chunks.length) {
		dev_warn(dcp->dev, "ignoring overflowing chunk\n");
		return false;
	}

	memcpy(dcp->chunks.data + req->offset, req->data, req->length);
	return true;
}

static bool dcpep_process_chunks(struct apple_dcp *dcp,
				 struct dcp_set_dcpav_prop_end_req *req)
{
	struct dcp_parse_ctx ctx;
	int ret;

	if (!dcp->chunks.data) {
		dev_warn(dcp->dev, "ignoring spurious end\n");
		return false;
	}

	/* used just as opaque pointer for tracing */
	ctx.dcp = dcp;

	ret = parse(dcp->chunks.data, dcp->chunks.length, &ctx);

	if (ret) {
		dev_warn(dcp->dev, "bad header on dcpav props\n");
		return false;
	}

	if (!strcmp(req->key, "TimingElements")) {
		dcp->modes = enumerate_modes(&ctx, &dcp->nr_modes,
					     dcp->width_mm, dcp->height_mm,
					     dcp->notch_height);

		if (IS_ERR(dcp->modes)) {
			dev_warn(dcp->dev, "failed to parse modes\n");
			dcp->modes = NULL;
			dcp->nr_modes = 0;
			return false;
		}
		if (dcp->nr_modes == 0)
			dev_warn(dcp->dev, "TimingElements without valid modes!\n");
	} else if (!strcmp(req->key, "DisplayAttributes")) {
		ret = parse_display_attributes(&ctx, &dcp->width_mm,
					&dcp->height_mm);

		if (ret) {
			dev_warn(dcp->dev, "failed to parse display attribs\n");
			return false;
		}

		dcp_set_dimensions(dcp);
	}

	return true;
}

static u8 dcpep_cb_prop_end(struct apple_dcp *dcp,
			    struct dcp_set_dcpav_prop_end_req *req)
{
	u8 resp = dcpep_process_chunks(dcp, req);

	/* move chunked data to connector to provide it via debugfs */
	dcp_connector_update_dict(dcp->connector, req->key, &dcp->chunks);
	dcp->chunks.data = NULL;
	dcp->chunks.length = 0;

	return resp;
}

/* Boot sequence */
static void boot_done(struct apple_dcp *dcp, void *out, void *cookie)
{
	struct dcp_channel *ch = &dcp->ch_cb;
	u8 *succ = ch->output[ch->depth - 1];
	dev_dbg(dcp->dev, "boot done\n");

	*succ = true;
	dcp_ack(dcp, DCP_CONTEXT_CB);
}

static void boot_5b(struct apple_dcp *dcp, void *out, void *cookie)
{
	dcp_set_display_refresh_properties(dcp, false, boot_done, NULL);
}

static void boot_5(struct apple_dcp *dcp, void *out, void *cookie)
{
#if DCP_FW_VER >= DCP_FW_VERSION(26, 0, 0)
	/* macOS issues A380 right before set_display_refresh_properties */
	dcp_dfb_ready_query(dcp, false, boot_5b, NULL);
#else
	boot_5b(dcp, out, cookie);
#endif
}

static void boot_4b(struct apple_dcp *dcp, void *out, void *cookie)
{
#if DCP_FW_VER >= DCP_FW_VERSION(13, 2, 0)
	/*
	 * macOS 25G83 sends 0 here, not 1: the traced A000 payload is
	 * "00 aa aa aa", one meaningful byte followed by poison.
	 */
	u32 arg = dcp->hw.iomfb_method_profile == DCP_IOMFB_METHODS_H17G;

	dcp_late_init_signal(dcp, false, &arg, boot_5, NULL);
#else
	dcp_late_init_signal(dcp, false, boot_5, NULL);
#endif
}

static void boot_4(struct apple_dcp *dcp, void *out, void *cookie)
{
#if DCP_FW_VER >= DCP_FW_VERSION(26, 0, 0)
	/* macOS issues A033 between flush_supports_power and late_init_signal */
	dcp_dfb_query(dcp, false, boot_4b, NULL);
#else
	boot_4b(dcp, out, cookie);
#endif
}

static void boot_3(struct apple_dcp *dcp, void *out, void *cookie)
{
	u32 v_true = true;

	dcp_flush_supports_power(dcp, false, &v_true, boot_4, NULL);
}

static void boot_2(struct apple_dcp *dcp, void *out, void *cookie)
{
	dcp_setup_video_limits(dcp, false, boot_3, NULL);
}

#if DCP_FW_VER >= DCP_FW_VERSION(26, 0, 0)
/*
 * macOS 25G83 runs this between A446 and A025, every boot:
 *   A035 -> A034 -> A103 -> A445 -> A104(w,h) -> A105
 * We were jumping straight from A446 to A025, so the firmware never got told
 * the default framebuffer's geometry and never finished its own DFB setup.
 */
static void boot_1_11(struct apple_dcp *dcp, void *out, void *cookie)
{
	dcp_commit_dfb_info(dcp, false, boot_2, NULL);
}

static void boot_1_10(struct apple_dcp *dcp, void *out, void *cookie)
{
	struct dcp_dfb_dimensions_h17p dim;

	/* A445 returns the dimensions that native macOS passes to A104.
	 * The H17G capture is2560x1664; the H17P capture is2408x1506.
	 */
	memcpy(&dim, out, sizeof(dim));
	if (!dim.width || !dim.height || dim.width > 16384 || dim.height > 16384) {
		struct dcp_channel *ch = &dcp->ch_cb;

		dev_err(dcp->dev, "invalid default framebuffer dimensions %ux%u\n",
			dim.width, dim.height);
		*(u8 *)ch->output[ch->depth - 1] = false;
		dcp_ack(dcp, DCP_CONTEXT_CB);
		return;
	}
	dev_info(dcp->dev, "default framebuffer dimensions %ux%u\n",
		 dim.width, dim.height);

	dcp_set_dfb_dimensions(dcp, false, &dim, boot_1_11, NULL);
}

static void boot_1_9(struct apple_dcp *dcp, void *out, void *cookie)
{
	u32 zero = 0;

	dcp_get_dfb_state(dcp, false, &zero, boot_1_10, NULL);
}

static void boot_1_8(struct apple_dcp *dcp, void *out, void *cookie)
{
	struct dcp_dfb_layout_req_h17p req = {};

	dcp_get_dfb_layout(dcp, false, &req, boot_1_9, NULL);
}

static void boot_1_7(struct apple_dcp *dcp, void *out, void *cookie)
{
	struct dcp_dfb_info_h17p info = {};

	dcp_get_dfb_info(dcp, false, &info, boot_1_8, NULL);
}

static void boot_1_6(struct apple_dcp *dcp, void *out, void *cookie)
{
	u64 zero = 0;

	dcp_get_dfb_compression_info(dcp, false, &zero, boot_1_7, NULL);
}
#endif

static void boot_1_5(struct apple_dcp *dcp, void *out, void *cookie)
{
#if DCP_FW_VER >= DCP_FW_VERSION(26, 0, 0)
	dcp_create_default_fb(dcp, false, boot_1_6, NULL);
#else
	dcp_create_default_fb(dcp, false, boot_2, NULL);
#endif
}

#if DCP_FW_VER >= DCP_FW_VERSION(26, 0, 0)
static void d000_done(struct apple_dcp *dcp, void *out, void *cookie)
{
	struct dcp_channel *ch = &dcp->ch_cb;
	u8 *succ = ch->output[ch->depth - 1];

	*succ = true;
	dcp_ack(dcp, DCP_CONTEXT_CB);
}

/*
 * macOS does not answer D000 straight away: it issues A033 first and only then
 * acks with 1.  Defer the ACK the same way.
 */
static bool dcpep_cb_d000_h17p(struct apple_dcp *dcp, int tag, void *out, void *in)
{
	trace_iomfb_callback(dcp, tag, __func__);
	dcp_dfb_query(dcp, false, d000_done, NULL);
	return false;
}
#endif

#if DCP_FW_VER >= DCP_FW_VERSION(26, 0, 0)
static void boot_1_after_park(struct apple_dcp *dcp, void *out, void *cookie)
{
	dcp_set_create_dfb(dcp, false, boot_1_5, NULL);
}
#endif

/* Use special function signature to defer the ACK */
static bool dcpep_cb_boot_1(struct apple_dcp *dcp, int tag, void *out, void *in)
{
	trace_iomfb_callback(dcp, tag, __func__);

#if DCP_FW_VER >= DCP_FW_VERSION(26, 0, 0)
	/*
	 * Park the display before the firmware configures the pipe, not after.
	 *
	 * m1n1 stops the ASC, so our firmware boots with no surface (macOS
	 * adopts the live generation and always has one).  It enables the pipe
	 * anyway while finishing start_hardware_boot and fetches from a null
	 * base -- an address below the DART's window, so it cannot be mapped,
	 * only prevented.  The reject becomes an AXI error and NMIs the
	 * coprocessor before a modeset can ever run.  Powering the display down
	 * first keeps the pipe quiet until we drive a real mode.
	 */
	/*
	 * Do not park here.  macOS never powers the display down at all --
	 * start_hardware_boot has no blank or pipe-disable step, and its only
	 * A473 in a full capture is unklong=1 (power on).  Parking at the head
	 * of the boot chain stops the firmware booting outright.
	 */
#endif

	dcp_set_create_dfb(dcp, false, boot_1_5, NULL);
	return false;
}

static struct dcp_allocate_bandwidth_resp dcpep_cb_allocate_bandwidth(struct apple_dcp *dcp,
						struct dcp_allocate_bandwidth_req *req)
{
	return (struct dcp_allocate_bandwidth_resp){
		.unk1 = req->unk1,
		.unk2 = req->unk2,
		.ret = 1,
	};
}

static struct dcp_rt_bandwidth dcpep_cb_rt_bandwidth(struct apple_dcp *dcp)
{
	struct dcp_rt_bandwidth rt_bw = (struct dcp_rt_bandwidth){
			.reg_scratch = 0,
			.reg_doorbell = 0,
			.doorbell_bit = 0,
	};


	if (dcp->disp_bw_scratch_index) {
		u32 offset = dcp->disp_bw_scratch_offset;
		u32 index = dcp->disp_bw_scratch_index;
		rt_bw.reg_scratch = dcp->disp_registers[index]->start + offset;
	}

	if (dcp->disp_bw_doorbell_index) {
		u32 index = dcp->disp_bw_doorbell_index;
		rt_bw.reg_doorbell = dcp->disp_registers[index]->start;
		rt_bw.doorbell_bit = REG_DOORBELL_BIT(dcp->index);
		/*
		 * This is most certainly not padding. t8103-dcp crashes without
		 * setting this immediately during modeset on 12.3 and 13.5
		 * firmware.
		 */
		rt_bw.padding[3] = 0x4;
	}

	return rt_bw;
}

#if DCP_FW_VER >= DCP_FW_VERSION(26, 0, 0)
/*
 * H17P's D006 is 0x54 in / 0x50 out, far wider than the 28-byte
 * frame_sync_props of older firmware, so the fields that matter sit past
 * anything the old struct could describe.
 *
 * macOS 25G83 answers it with zeros except three u32 at 0x24, 0x28 and 0x2c,
 * each 0x10000 -- 1.0 in 16.16 fixed point, the identity scale.  Answering all
 * zeros hands the pipe a zero scale, and it then programs a null surface base:
 * the display DART immediately faults at DVA 0, that reject becomes an AXI
 * error, and the firmware takes an NMI (apiodma_irq_AxiErr).
 */
#define DCP_FRAME_SYNC_IDENTITY	0x10000

struct dcp_set_frame_sync_props_req_h17p {
	u8 unk[0x54];
} __packed;

struct dcp_set_frame_sync_props_resp_h17p {
	u8 unk[0x50];
} __packed;

static struct dcp_set_frame_sync_props_resp_h17p
dcpep_cb_set_frame_sync_props_h17p(struct apple_dcp *dcp,
				   struct dcp_set_frame_sync_props_req_h17p *req)
{
	struct dcp_set_frame_sync_props_resp_h17p resp = {};
	static const u16 identity_at[] = { 0x24, 0x28, 0x2c };
	unsigned int i;

	for (i = 0; i < ARRAY_SIZE(identity_at); i++) {
		u32 v = DCP_FRAME_SYNC_IDENTITY;

		memcpy(&resp.unk[identity_at[i]], &v, sizeof(v));
	}

	return resp;
}
#endif

static struct dcp_set_frame_sync_props_resp
dcpep_cb_set_frame_sync_props(struct apple_dcp *dcp,
			      struct dcp_set_frame_sync_props_req *req)
{
	return (struct dcp_set_frame_sync_props_resp){};
}

/* Callback to get the current time as milliseconds since the UNIX epoch */
static u64 dcpep_cb_get_time(struct apple_dcp *dcp)
{
	return ktime_to_ms(ktime_get_real());
}

struct dcp_swap_cookie {
	struct kref refcount;
	struct completion done;
	u32 swap_id;
};

static void release_swap_cookie(struct kref *ref)
{
	struct dcp_swap_cookie *cookie;
	cookie = container_of(ref, struct dcp_swap_cookie, refcount);

        kfree(cookie);
}

#if DCP_FW_VER >= DCP_FW_VERSION(26, 0, 0)
/*
 * H17P's swap surface has no is_tearing_allowed and no is_premultiplied:
 * plane_cnt sits at +1, plane_cnt2 at +5 and format at +9, two bytes earlier
 * than every other firmware.
 *
 * Proof from the capture: macOS's A408 carries its plane counts at surface+1
 * and +5 and its format at +9, and reading a u32 at +9 out of *our* record
 * gives 00 00 41 52, which the firmware prints MSB-first as "RA" -- exactly
 * the text it rejects us with ("verify_surfaces: No support for format RA").
 *
 * Shift each surface's tail down by two rather than fork the shared struct,
 * which the A472 default-framebuffer path also uses.
 */
static void dcp_h17p_fix_swap_surfaces(struct apple_dcp *dcp)
{
	struct DCP_FW_NAME(dcp_swap_submit_req) *req = &DCP_FW_UNION(dcp->swap);
	unsigned int i;

	/*
	 * The swap's own flag words.  Our driver never sets either; macOS's
	 * captured swaps carry flags1 = 0x34865202 and flags2 = 4 in every
	 * record, at +0x40 and +0x48.
	 */
	req->swap.flags1 = DCP_SWAP_FLAGS1;
	req->swap.flags2 = DCP_SWAP_FLAGS2;

	/*
	 * The two counters inside the H17P gap that we never modelled: macOS
	 * carries 2 at +0x50 and 3 at +0x78 in every captured swap.  The gap
	 * starts at +0x50, so these are indices 0 and 0x28 of the blob.
	 */
	req->swap.h17p_pre_swap_id[0x00] = 2;
	req->swap.h17p_pre_swap_id[0x28] = 3;

	for (i = 0; i < SWAP_SURFACES; i++) {
		u8 *p = (u8 *)&req->surf[i];
		size_t n = sizeof(req->surf[i]);

		memmove(p + 1, p + 3, n - 3);
		memset(p + n - 2, 0, 2);
	}

	/*
	 * Our trailing null-flag block lands two bytes late.  In the capture
	 * macOS's flags sit at +0xe94/+0xe95/+0xe99 of the 0xe9c record while
	 * ours are at +0xe96/+0xe97/+0xe98/+0xe9a/+0xe9b -- so the last of them
	 * run off the end and the firmware reads the whole block misaligned.
	 * It then believes layers are present that are not, verifies their
	 * empty rectangles, and rejects the swap with
	 * "verify_scaling: error: null rectangle: src:0x0+0,0 dst:0x0+0,0".
	 */
	{
		u8 *r = (u8 *)req;
		size_t n = sizeof(*req);
		size_t off = offsetof(typeof(*req), surf_iova);

		/*
		 * The two bytes are not just in the tail: everything from
		 * surf_iova onward is late.  macOS's A408 carries
		 * surf_iova[0] at +0xe38 of the 0xe9c record, ours sat at
		 * +0xe3a, so the firmware read our DVA 0x100075fc000 two bytes
		 * early as 0x0100075fc0000000 and faulted on its low 36 bits,
		 * 0xfc0000000 -- an address with no L1 entry at all.  Shift the
		 * whole block, which subsumes the null-flag fixup.
		 */
		/* Preserve the complete 16K-aligned IOVA, including its low bits. */
		static_assert(offsetof(typeof(*req), surf_iova) == 0xe3a,
			      "H17P source IOVA offset");
		static_assert(sizeof(*req) == 0xe9c, "H17P request size");
		memmove(r + off - 2, r + off, n - off);
		memset(r + n - 2, 0, 2);
	}
}
#else
static void dcp_h17p_fix_swap_surfaces(struct apple_dcp *dcp) { }
#endif

static void dcp_swap_cleared(struct apple_dcp *dcp, void *data, void *cookie)
{
	struct DCP_FW_NAME(dcp_swap_submit_resp) *resp = data;

	if (cookie) {
		struct dcp_swap_cookie *info = cookie;
		complete(&info->done);
		kref_put(&info->refcount, release_swap_cookie);
	}

	if (resp->ret) {
		dev_err(dcp->dev, "swap_clear failed! status %u\n", resp->ret);
		dcp_drm_crtc_vblank(dcp->crtc);
		return;
	}

	dcp_release_swapped_out_fbs(dcp);
}

static void dcp_swap_clear_started(struct apple_dcp *dcp, void *data,
				   void *cookie)
{
	struct DCP_FW_NAME(dcp_swap_start_resp) *resp = data;
	DCP_FW_UNION(dcp->swap).swap.swap_id = resp->swap_id;
	dcp_arm_swapped_out_fbs(dcp, resp->swap_id);

	if (cookie) {
		struct dcp_swap_cookie *info = cookie;
		info->swap_id = resp->swap_id;
	}

	dcp_h17p_fix_swap_surfaces(dcp);
	dcp_swap_submit(dcp, false, &DCP_FW_UNION(dcp->swap), dcp_swap_cleared, cookie);
}

static void dcp_on_final(struct apple_dcp *dcp, void *out, void *cookie)
{
	struct dcp_wait_cookie *wait = cookie;

	if (wait) {
		complete(&wait->done);
		kref_put(&wait->refcount, release_wait_cookie);
	}
}

static void dcp_on_set_power_state(struct apple_dcp *dcp, void *out, void *cookie)
{
	struct DCP_FW_NAME(dcp_set_power_state_req) req = {
		.unklong = 1,
	};

	dcp_set_power_state(dcp, DCP_POWER_OOB, &req, dcp_on_final, cookie);
}

static void dcp_on_set_parameter(struct apple_dcp *dcp, void *out, void *cookie)
{
	struct dcp_set_parameter_dcp param = {
		.param = IOMFBPARAM_ADAPTIVE_SYNC,
		.value = { 0 },
#if DCP_FW_VER >= DCP_FW_VERSION(13, 2, 0)
		.count = 3,
#else
		.count = 1,
#endif
	};

	dcp_set_parameter_dcp(dcp, false, &param, dcp_on_set_power_state, cookie);
}

void DCP_FW_NAME(iomfb_poweron)(struct apple_dcp *dcp)
{
	struct dcp_wait_cookie *cookie;
	int ret;
	u32 handle;
	dev_info(dcp->dev, "dcp_poweron() starting\n");

	cookie = kzalloc(sizeof(*cookie), GFP_KERNEL);
	if (!cookie)
		return;

	init_completion(&cookie->done);
	kref_init(&cookie->refcount);
	/* increase refcount to ensure the receiver has a reference */
	kref_get(&cookie->refcount);

	if (dcp->main_display) {
#if DCP_FW_VER >= DCP_FW_VERSION(26, 0, 0)
		/*
		 * macOS never repeats A411 at power-on -- it issues it once, in
		 * the client-open sequence, and goes straight to the power
		 * state from here.  Repeating it after the client is open is a
		 * call the firmware simply stops answering, which wedges the
		 * command channel and with it every later swap.
		 */
		dcp_on_set_power_state(dcp, NULL, cookie);
#else
		handle = 0;
		dcp_set_display_device(dcp, false, &handle, dcp_on_set_power_state,
				       cookie);
#endif
	} else {
		handle = 2;
		dcp_set_display_device(dcp, false, &handle,
				       dcp_on_set_parameter, cookie);
	}
	ret = wait_for_completion_timeout(&cookie->done, msecs_to_jiffies(10000));

	if (ret == 0) {
		dev_warn(dcp->dev, "wait for power timed out, connector will be broken\n");
	} else if (ret > 0) {
		int msecs = jiffies_to_msecs(ret);
		if (msecs > 6000)
			dev_info(dcp->dev, "dcp_set_power_state_req returned, %d ms remaining\n", msecs);
		else
			dev_warn(dcp->dev, "dcp_set_power_state_req returned, %d ms remaining\n", msecs);
	} else {
		drm_connector_set_link_status_property(&dcp->connector->base,
						       DRM_MODE_LINK_STATUS_BAD);
		dev_warn(dcp->dev, "wait for completion error: %d\n", ret);
	}

	kref_put(&cookie->refcount, release_wait_cookie);;

	/* Force a brightness update after poweron, to restore the brightness */
	dcp->brightness.update = true;
}

static void complete_set_powerstate(struct apple_dcp *dcp, void *out,
				    void *cookie)
{
	struct dcp_wait_cookie *wait = cookie;

	if (wait) {
		complete(&wait->done);
		kref_put(&wait->refcount, release_wait_cookie);
	}
}

static void last_client_closed_poff(struct apple_dcp *dcp, void *out, void *cookie)
{
	struct DCP_FW_NAME(dcp_set_power_state_req) power_req = {
		.unklong = 0,
	};
	dcp_set_power_state(dcp, DCP_POWER_OOB, &power_req, complete_set_powerstate,
			    cookie);
}

static void aborted_swaps_dcp_poff(struct apple_dcp *dcp, void *out, void *cookie)
{
	struct iomfb_last_client_close_req last_client_req = {};
	iomfb_last_client_close(dcp, false, &last_client_req,
				last_client_closed_poff, cookie);
}

void DCP_FW_NAME(iomfb_poweroff)(struct apple_dcp *dcp)
{
	int ret, swap_id;
	struct iomfb_abort_swaps_dcp_req abort_req = {
		.client = {
			.flag2 = 1,
		},
	};
	struct dcp_swap_cookie *cookie;
	struct dcp_wait_cookie *poff_cookie;
	struct DCP_FW_NAME(dcp_swap_start_req) swap_req = { 0 };
	struct DCP_FW_NAME(dcp_swap_submit_req) *swap = &DCP_FW_UNION(dcp->swap);

	cookie = kzalloc(sizeof(*cookie), GFP_KERNEL);
	if (!cookie)
		return;
	init_completion(&cookie->done);
	kref_init(&cookie->refcount);
	/* increase refcount to ensure the receiver has a reference */
	kref_get(&cookie->refcount);

	// clear surfaces
	memset(swap, 0, sizeof(*swap));

	swap->swap.swap_enabled =
		swap->swap.swap_completed = IOMFB_SET_BACKGROUND | 0x7;
	swap->swap.bg_color = 0xFF000000;

	/*
	 * Turn off the backlight. This matters because the DCP's idea of
	 * backlight brightness gets desynced after a power change, and it
	 * needs to be told it's going to turn off so it will consider the
	 * subsequent update on poweron an actual change and restore the
	 * brightness.
	 */
	if (dcp_has_panel(dcp)) {
		swap->swap.bl_unk = 1;
		swap->swap.bl_value = 0;
		swap->swap.bl_power = 0;
	}

	/* Null all surfaces */
	for (int l = 0; l < SWAP_SURFACES; l++)
		swap->surf_null[l] = true;
#if DCP_FW_VER >= DCP_FW_VERSION(13, 2, 0)
#if DCP_FW_VER < DCP_FW_VERSION(26, 0, 0)
	for (int l = 0; l < ARRAY_SIZE(swap->surf2_null); l++)
		swap->surf2_null[l] = true;
#endif
	swap->unkU32Ptr_null = true;
	swap->unkU32out_null = true;
#endif

	dcp_swap_start(dcp, false, &swap_req, dcp_swap_clear_started, cookie);

	ret = wait_for_completion_timeout(&cookie->done, msecs_to_jiffies(50));
	swap_id = cookie->swap_id;
	kref_put(&cookie->refcount, release_swap_cookie);
	if (ret <= 0) {
		dcp->crashed = true;
		return;
	}

	dev_dbg(dcp->dev, "%s: clear swap submitted: %u\n", __func__, swap_id);

	poff_cookie = kzalloc(sizeof(*poff_cookie), GFP_KERNEL);
	if (!poff_cookie)
		return;
	init_completion(&poff_cookie->done);
	kref_init(&poff_cookie->refcount);
	/* increase refcount to ensure the receiver has a reference */
	kref_get(&poff_cookie->refcount);

	iomfb_abort_swaps_dcp(dcp, false, &abort_req,
				aborted_swaps_dcp_poff, poff_cookie);
	ret = wait_for_completion_timeout(&poff_cookie->done,
					  msecs_to_jiffies(1000));

	if (ret == 0)
		dev_warn(dcp->dev, "setPowerState(0) timeout %u ms\n", 1000);
	else if (ret > 0)
		dev_dbg(dcp->dev,
			"setPowerState(0) finished with %d ms to spare",
			jiffies_to_msecs(ret));

	kref_put(&poff_cookie->refcount, release_wait_cookie);

	dev_info(dcp->dev, "dcp_poweroff() done\n");
}

static void last_client_closed_sleep(struct apple_dcp *dcp, void *out, void *cookie)
{
	struct DCP_FW_NAME(dcp_set_power_state_req) power_req = {
		.unklong = 0,
	};
	dcp_set_power_state(dcp, DCP_POWER_OOB, &power_req, complete_set_powerstate, cookie);
}

static void aborted_swaps_dcp_sleep(struct apple_dcp *dcp, void *out, void *cookie)
{
	struct iomfb_last_client_close_req req = { 0 };
	iomfb_last_client_close(dcp, false, &req, last_client_closed_sleep, cookie);
}

void DCP_FW_NAME(iomfb_sleep)(struct apple_dcp *dcp)
{
	int ret;
	struct iomfb_abort_swaps_dcp_req req = {
		.client = {
			.flag2 = 1,
		},
	};

	struct dcp_wait_cookie *cookie;

	cookie = kzalloc(sizeof(*cookie), GFP_KERNEL);
	if (!cookie)
		return;
	init_completion(&cookie->done);
	kref_init(&cookie->refcount);
	/* increase refcount to ensure the receiver has a reference */
	kref_get(&cookie->refcount);

	iomfb_abort_swaps_dcp(dcp, false, &req, aborted_swaps_dcp_sleep,
				cookie);
	ret = wait_for_completion_timeout(&cookie->done,
					  msecs_to_jiffies(1000));

	if (ret == 0)
		dev_warn(dcp->dev, "setDCPPower(0) timeout %u ms\n", 1000);

	kref_put(&cookie->refcount, release_wait_cookie);
	dev_info(dcp->dev, "dcp_sleep() done\n");
}

static void dcpep_cb_hotplug(struct apple_dcp *dcp, u64 *connected)
{
	struct apple_connector *connector = dcp->connector;

	/* DCP issues hotplug_gated callbacks after SetPowerState() calls on
	 * devices with display (macbooks, imacs). This must not result in
	 * connector state changes on DRM side. Some applications won't enable
	 * a CRTC with a connector in disconnected state. Weston after DPMS off
	 * is one example. dcp_is_main_display() returns true on devices with
	 * integrated display. Ignore the hotplug_gated() callbacks there.
	 */
	if (dcp->main_display)
		return;

	if (dcp->during_modeset) {
		dev_info(dcp->dev,
			 "cb_hotplug() ignored during modeset connected:%llu\n",
			 *connected);
		return;
	}

	dev_info(dcp->dev, "cb_hotplug() connected:%llu, valid_mode:%d\n",
		 *connected, dcp->valid_mode);

	/* Hotplug invalidates mode. DRM doesn't always handle this. */
	if (!(*connected)) {
		dcp->valid_mode = false;
		/* after unplug swap will not complete until the next
		 * set_digital_out_mode */
		schedule_work(&dcp->vblank_wq);
	}

	if (connector && connector->connected != !!(*connected)) {
		connector->connected = !!(*connected);
		dcp->valid_mode = false;
		schedule_work(&connector->hotplug_wq);
	}
}

static void
dcpep_cb_swap_complete_intent_gated(struct apple_dcp *dcp,
				    struct dcp_swap_complete_intent_gated *info)
{
	trace_iomfb_swap_complete_intent_gated(dcp, info->swap_id,
		info->width, info->height);
}

static void
dcpep_cb_abort_swap_ap_gated(struct apple_dcp *dcp, u32 *swap_id)
{
	trace_iomfb_abort_swap_ap_gated(dcp, *swap_id);
}

static struct dcpep_get_tiling_state_resp
dcpep_cb_get_tiling_state(struct apple_dcp *dcp,
			  struct dcpep_get_tiling_state_req *req)
{
	return (struct dcpep_get_tiling_state_resp){
		.value = 0,
		.ret = 1,
	};
}

static u8 dcpep_cb_create_backlight_service(struct apple_dcp *dcp)
{
	/*
	 * dcp_has_panel() only becomes true once the panel dimensions have been
	 * parsed out of the EDID, which happens long after this callback: the
	 * firmware asks during start_ios_gated, before any mode is known.  An
	 * internal panel always has a backlight, so answer for the connector.
	 * macOS 25G83 answers true here in both captured boots; answering false
	 * makes the firmware log "Could not find Backlight service".
	 */
	return dcp_has_panel(dcp) ||
	       dcp->connector_type == DRM_MODE_CONNECTOR_eDP;
}

TRAMPOLINE_VOID(trampoline_nop, dcpep_cb_nop);
TRAMPOLINE_OUT(trampoline_true, dcpep_cb_true, u8);
TRAMPOLINE_OUT(trampoline_false, dcpep_cb_false, u8);
TRAMPOLINE_OUT(trampoline_zero, dcpep_cb_zero, u32);
TRAMPOLINE_IN(trampoline_swap_complete, dcpep_cb_swap_complete,
	      struct DCP_FW_NAME(dc_swap_complete_resp));
TRAMPOLINE_INOUT(trampoline_get_uint_prop, dcpep_cb_get_uint_prop,
		 struct dcp_get_uint_prop_req, struct dcp_get_uint_prop_resp);
TRAMPOLINE_IN(trampoline_set_fx_prop, iomfbep_cb_set_fx_prop,
	      struct iomfb_set_fx_prop_req)
TRAMPOLINE_INOUT(trampoline_map_piodma, dcpep_cb_map_piodma,
		 struct dcp_map_buf_req, struct DCP_FW_NAME(dcp_map_buf_resp));
TRAMPOLINE_IN(trampoline_unmap_piodma, dcpep_cb_unmap_piodma,
	      struct dcp_unmap_buf_resp);
TRAMPOLINE_INOUT(trampoline_sr_set_property_int, iomfbep_cb_sr_set_property_int,
		 struct iomfb_sr_set_property_int_req, u8);
TRAMPOLINE_INOUT(trampoline_allocate_buffer, dcpep_cb_allocate_buffer,
		 struct dcp_allocate_buffer_req,
		 struct dcp_allocate_buffer_resp);
TRAMPOLINE_INOUT(trampoline_map_physical, dcpep_cb_map_physical,
		 struct dcp_map_physical_req, struct dcp_map_physical_resp);
TRAMPOLINE_INOUT(trampoline_release_mem_desc, dcpep_cb_release_mem_desc, u32,
		 u8);
TRAMPOLINE_INOUT(trampoline_map_reg, dcpep_cb_map_reg,
		 struct DCP_FW_NAME(dcp_map_reg_req),
		 struct DCP_FW_NAME(dcp_map_reg_resp));
TRAMPOLINE_INOUT(trampoline_read_edt_data, dcpep_cb_read_edt_data,
		 struct dcp_read_edt_data_req, struct dcp_read_edt_data_resp);
TRAMPOLINE_INOUT(trampoline_prop_start, dcpep_cb_prop_start, u32, u8);
TRAMPOLINE_INOUT(trampoline_prop_chunk, dcpep_cb_prop_chunk,
		 struct dcp_set_dcpav_prop_chunk_req, u8);
TRAMPOLINE_INOUT(trampoline_prop_end, dcpep_cb_prop_end,
		 struct dcp_set_dcpav_prop_end_req, u8);
TRAMPOLINE_INOUT(trampoline_allocate_bandwidth, dcpep_cb_allocate_bandwidth,
	       struct dcp_allocate_bandwidth_req, struct dcp_allocate_bandwidth_resp);
TRAMPOLINE_OUT(trampoline_rt_bandwidth, dcpep_cb_rt_bandwidth,
	       struct dcp_rt_bandwidth);
TRAMPOLINE_INOUT(trampoline_set_frame_sync_props, dcpep_cb_set_frame_sync_props,
	       struct dcp_set_frame_sync_props_req,
	       struct dcp_set_frame_sync_props_resp);
#if DCP_FW_VER >= DCP_FW_VERSION(26, 0, 0)
TRAMPOLINE_INOUT(trampoline_set_frame_sync_props_h17p,
	       dcpep_cb_set_frame_sync_props_h17p,
	       struct dcp_set_frame_sync_props_req_h17p,
	       struct dcp_set_frame_sync_props_resp_h17p);
#endif
TRAMPOLINE_OUT(trampoline_get_frequency, dcpep_cb_get_frequency, u64);
TRAMPOLINE_OUT(trampoline_get_time, dcpep_cb_get_time, u64);
TRAMPOLINE_IN(trampoline_hotplug, dcpep_cb_hotplug, u64);
TRAMPOLINE_IN(trampoline_swap_complete_intent_gated,
	      dcpep_cb_swap_complete_intent_gated,
	      struct dcp_swap_complete_intent_gated);
TRAMPOLINE_IN(trampoline_abort_swap_ap_gated, dcpep_cb_abort_swap_ap_gated, u32);
TRAMPOLINE_IN(trampoline_enable_backlight_message_ap_gated,
	      iomfbep_cb_enable_backlight_message_ap_gated, u8);
TRAMPOLINE_IN(trampoline_pr_publish, iomfb_cb_pr_publish,
	      struct iomfb_property);
TRAMPOLINE_INOUT(trampoline_get_tiling_state, dcpep_cb_get_tiling_state,
		 struct dcpep_get_tiling_state_req, struct dcpep_get_tiling_state_resp);
TRAMPOLINE_OUT(trampoline_create_backlight_service, dcpep_cb_create_backlight_service, u8);

/*
 * Callback for swap requests. If a swap failed, we'll never get a swap
 * complete event so we need to fake a vblank event early to avoid a hang.
 */

static void dcp_swapped(struct apple_dcp *dcp, void *data, void *cookie)
{
	struct DCP_FW_NAME(dcp_swap_submit_resp) *resp = data;

	if (resp->ret) {
		dev_err(dcp->dev, "swap failed! status %u\n", resp->ret);
		dcp_drm_crtc_vblank(dcp->crtc);
		return;
	}
	dcp->swap_start = ktime_get();

	dcp_release_swapped_out_fbs(dcp);
}

static void dcp_swap_started(struct apple_dcp *dcp, void *data, void *cookie)
{
	struct DCP_FW_NAME(dcp_swap_start_resp) *resp = data;

	DCP_FW_UNION(dcp->swap).swap.swap_id = resp->swap_id;
	dcp_arm_swapped_out_fbs(dcp, resp->swap_id);

	trace_iomfb_swap_submit(dcp, resp->swap_id);
	dcp_h17p_fix_swap_surfaces(dcp);
	dcp_swap_submit(dcp, false, &DCP_FW_UNION(dcp->swap), dcp_swapped, NULL);
}

/* Helpers to modeset and swap, used to flush */
static void do_swap(struct apple_dcp *dcp, void *data, void *cookie)
{
	struct DCP_FW_NAME(dcp_swap_start_req) start_req = { 0 };

	if (dcp->connector && dcp->connector->connected)
		dcp_swap_start(dcp, false, &start_req, dcp_swap_started, NULL);
	else
		dcp_drm_crtc_vblank(dcp->crtc);
}

#if DCP_FW_VER >= DCP_FW_VERSION(26, 0, 0)
/*
 * macOS issues A428 between set_matrix and the first swap: CMD, in 4, out 4,
 * payload 00 00 01 00 -- 0x10000, 1.0 in 16.16, the same identity value it
 * answers D006 with.  We went straight from A423 to A406.
 */
static void dcp_pre_swap_a428(struct apple_dcp *dcp, void *out, void *cookie)
{
	u32 identity = 0x10000;

	dcp_pipe_set_428(dcp, false, &identity, do_swap, cookie);
}
#endif

static void complete_set_digital_out_mode(struct apple_dcp *dcp, void *data,
					  void *cookie)
{
	struct dcp_wait_cookie *wait = cookie;

	if (wait) {
		complete(&wait->done);
		kref_put(&wait->refcount, release_wait_cookie);
	}
}

int DCP_FW_NAME(iomfb_modeset)(struct apple_dcp *dcp,
			       struct drm_crtc_state *crtc_state)
{
	struct dcp_display_mode *mode;
	struct dcp_wait_cookie *cookie;
	struct dcp_color_mode *cmode = NULL;
	int ret;

	mode = lookup_mode(dcp, &crtc_state->mode);
	if (!mode) {
		dev_err(dcp->dev, "no match for " DRM_MODE_FMT "\n",
			DRM_MODE_ARG(&crtc_state->mode));
		return -EIO;
	}

	dev_info(dcp->dev,
		 "set_digital_out_mode(color:%d timing:%d) " DRM_MODE_FMT "\n",
		 mode->color_mode_id, mode->timing_mode_id,
		 DRM_MODE_ARG(&crtc_state->mode));
	if (mode->color_mode_id == mode->sdr_rgb.id)
		cmode = &mode->sdr_rgb;
	else if (mode->color_mode_id == mode->sdr_444.id)
		cmode = &mode->sdr_444;
	else if (mode->color_mode_id == mode->sdr.id)
		cmode = &mode->sdr;
	else if (mode->color_mode_id == mode->best.id)
		cmode = &mode->best;
	if (cmode)
		dev_info(dcp->dev,
			"set_digital_out_mode() color mode depth:%hhu format:%u "
			"colorimetry:%u eotf:%u range:%u vrr:%u\n", cmode->depth,
			cmode->format, cmode->colorimetry, cmode->eotf,
			cmode->range, mode->vrr);

	dcp->mode = (struct dcp_set_digital_out_mode_req){
		.color_mode_id = mode->color_mode_id,
		.timing_mode_id = mode->timing_mode_id
	};

	/* Keep track of suspected vrr modes */
	dcp->use_timestamps = mode->vrr;

	cookie = kzalloc(sizeof(*cookie), GFP_KERNEL);
	if (!cookie) {
		return -ENOMEM;
	}

	init_completion(&cookie->done);
	kref_init(&cookie->refcount);
	/* increase refcount to ensure the receiver has a reference */
	kref_get(&cookie->refcount);

	dcp->during_modeset = true;

#if DCP_FW_VER >= DCP_FW_VERSION(26, 0, 0)
	/*
	 * Inherit the mode iBoot already programmed instead of asking for one.
	 *
	 * macOS never issues A413 in a full capture -- it attaches to a display
	 * iBoot has already configured.  Ours asks for colorID 1 / timingID 2
	 * and the firmware answers 0x8000000b, so the timing generator never
	 * starts: no vblank, no swap completion, black panel.  The panel is
	 * already running at its native 2408x1506, so skip the modeset and let
	 * that timing stand.
	 */
	if (!dcp_send_modeset) {
		dev_info(dcp->dev, "inheriting the bootloader's display mode\n");
		complete_set_digital_out_mode(dcp, NULL, cookie);
	} else
#endif
	dcp_set_digital_out_mode(dcp, false, &dcp->mode,
				 complete_set_digital_out_mode, cookie);

	/*
	 * The DCP firmware has an internal timeout of ~8 seconds for
	 * modesets. Add an extra 500ms to safe side that the modeset
	 * call has returned.
	 */
	ret = wait_for_completion_timeout(&cookie->done,
					  msecs_to_jiffies(8500));

	kref_put(&cookie->refcount, release_wait_cookie);
	dcp->during_modeset = false;
	dev_info(dcp->dev, "set_digital_out_mode finished:%d\n", ret);

	if (ret == 0) {
		dev_info(dcp->dev, "set_digital_out_mode timed out\n");
		return -EIO;
	} else if (ret < 0) {
		dev_info(dcp->dev,
			 "waiting on set_digital_out_mode failed:%d\n", ret);
		return -EIO;

	} else if (ret > 0) {
		dev_dbg(dcp->dev,
			"set_digital_out_mode finished with %d to spare\n",
			jiffies_to_msecs(ret));
	}
	dcp->valid_mode = true;

	return 0;
}

void DCP_FW_NAME(iomfb_flush)(struct apple_dcp *dcp, struct drm_crtc *crtc, struct drm_atomic_state *state)
{
	struct drm_plane *plane;
	struct drm_plane_state *new_state, *old_state;
	struct drm_crtc_state *crtc_state;
	struct DCP_FW_NAME(dcp_swap_submit_req) *req = &DCP_FW_UNION(dcp->swap);
	int plane_idx, l;
	int has_surface = 0;

	crtc_state = drm_atomic_get_new_crtc_state(state, crtc);

	/* Reset all surfaces to defaults */
	memset(req, 0, sizeof(*req));
	for (l = 0; l < SWAP_SURFACES; l++)
		req->surf_null[l] = true;
#if DCP_FW_VER >= DCP_FW_VERSION(13, 2, 0)
#if DCP_FW_VER < DCP_FW_VERSION(26, 0, 0)
	for (l = 0; l < ARRAY_SIZE(req->surf2_null); l++)
		req->surf2_null[l] = true;
#endif
	req->unkU32Ptr_null = true;
	req->unkU32out_null = true;
#endif

	/*
	 * Clear all surfaces on startup. The boot framebuffer in surface 0
	 * sticks around.
	 */
	if (!dcp->surfaces_cleared) {
		req->swap.swap_enabled = IOMFB_SET_BACKGROUND | 0x7;
		req->swap.bg_color = 0xFF000000;
		dcp->surfaces_cleared = true;
	}

	for_each_oldnew_plane_in_state(state, plane, old_state, new_state, plane_idx) {
		struct apple_plane_state *apple_state = to_apple_plane_state(new_state);
		struct apple_plane *apple_plane = to_apple_plane(plane);

		/* skip planes not for this crtc */
		if (old_state->crtc != crtc && new_state->crtc != crtc)
			continue;

		l = apple_plane->iomfb_surface;
		req->swap.swap_enabled |= BIT(l);

		if (old_state->fb && new_state->fb != old_state->fb) {
			/* Retain the old framebuffer until its replacement is stable. */
			struct dcp_fb_reference *entry =
				kzalloc(sizeof(*entry), GFP_KERNEL);
			unsigned long flags;

			if (entry) {
				entry->fb = old_state->fb;
				/*
				 * Left unarmed: the id of the swap that
				 * unbinds this framebuffer is not known until
				 * the firmware answers swap_start.
				 */
				spin_lock_irqsave(&dcp->swapped_out_lock,
						  flags);
				list_add_tail(&entry->head,
					      &dcp->swapped_out_fbs);
				spin_unlock_irqrestore(&dcp->swapped_out_lock,
						       flags);
			}
			drm_framebuffer_get(old_state->fb);
		}

		if (!new_state->fb || !new_state->visible) {
			continue;
		}
		req->surf_null[l] = false;
		has_surface = 1;

		req->swap.src_rect[l] = apple_state->src_rect;
		req->swap.dst_rect[l] = apple_state->dst_rect;

		if (dcp->notch_height > 0)
			req->swap.dst_rect[l].y += dcp->notch_height;

		req->surf_iova[l] = apple_state->iova;
		req->surf[l].base = apple_state->surf;

		/* Use sRGB colorspace only for internal panels. External
		 * displays are expected to have EDID and user space can use
		 * the contained colorimetry information to provide native
		 * colors.
		 */
		if (dcp->connector_type == DRM_MODE_CONNECTOR_eDP &&
		    req->surf[l].base.colorspace == DCP_COLORSPACE_BG_SRGB)
			req->surf[l].base.colorspace = DCP_COLORSPACE_NATIVE;
	}

	if (!has_surface && !crtc_state->color_mgmt_changed) {
		if (crtc_state->enable && crtc_state->active &&
		    !crtc_state->planes_changed) {
			schedule_work(&dcp->vblank_wq);
			return;
		}

		/* Set black background */
		req->swap.swap_enabled |= IOMFB_SET_BACKGROUND;
		req->swap.bg_color = 0xFF000000;
		req->clear = 1;
	}

	if (has_surface && dcp->use_timestamps) {
		/*
		 * Fake timstamps to get 120hz refresh rate. It looks
		 * like the actual value does not matter, as long  as it is non zero.
		 */
		req->swap.ts1 = 120;
		req->swap.ts2 = 120;
		req->swap.ts3 = 120;
	}

	/* These fields should be set together */
	req->swap.swap_completed = req->swap.swap_enabled;

	/* update brightness if changed */
	if (dcp_has_panel(dcp) && dcp->brightness.update) {
		req->swap.bl_unk = 1;
		req->swap.bl_value = dcp->brightness.dac;
		req->swap.bl_power = 0x40;
		dcp->brightness.update = false;
	}

	if (crtc_state->color_mgmt_changed) {
		struct iomfb_set_matrix_req mat = {
			.location = 9,
		};

		if (crtc_state->ctm) {
			struct drm_color_ctm *ctm = (struct drm_color_ctm *)crtc_state->ctm->data;
			memcpy(mat.matrix, ctm->matrix, sizeof(mat.matrix));
		} else {
			mat.matrix[0] = mat.matrix[4] = mat.matrix[8] = 1LLU << 32;
		}

#if DCP_FW_VER >= DCP_FW_VERSION(26, 0, 0)
		iomfb_set_matrix(dcp, false, &mat, dcp_pre_swap_a428, NULL);
#else
		iomfb_set_matrix(dcp, false, &mat, do_swap, NULL);
#endif
	} else
		do_swap(dcp, NULL, NULL);
}

static void res_is_main_display(struct apple_dcp *dcp, void *out, void *cookie)
{
	struct apple_connector *connector;
	int result = *(int *)out;
	dev_info(dcp->dev, "DCP is_main_display: %d\n", result);

	dcp->main_display = result != 0;
	dev_info(dcp->dev, "DCP services ready: main display %d, modes %u\n",
		 dcp->main_display, dcp->nr_modes);


	connector = dcp->connector;
	if (connector) {
		connector->connected = dcp->nr_modes > 0;
		schedule_work(&connector->hotplug_wq);
	}

	dcp->active = true;
	complete(&dcp->start_done);
}

static void init_3(struct apple_dcp *dcp, void *out, void *cookie);

static void init_3(struct apple_dcp *dcp, void *out, void *cookie)
{
	dcp_is_main_display(dcp, false, res_is_main_display, NULL);
}

static void init_2(struct apple_dcp *dcp, void *out, void *cookie)
{
#if DCP_FW_VER >= DCP_FW_VERSION(26, 0, 0)
	/*
	 * macOS always calls first_client_open here; skipping it leaves the
	 * firmware with no client and it never accepts a swap.  The skip is
	 * kept only as a bisect lever and defaults off.
	 */
	if (dcp_defer_client_open) {
		init_3(dcp, NULL, NULL);
		return;
	}
#endif
	dcp_first_client_open(dcp, false, init_3, NULL);
}

#if DCP_FW_VER >= DCP_FW_VERSION(26, 0, 0)
/*
 * The client-open sequence, in the order macOS 25G83 issues it:
 *
 *   A445 A411 A445 A415 A450 A031 x9 A414 A478 A457 A474 A478 A476 A478 A412
 *
 * Every payload below is the traced one.  A412 is not in the table: its result
 * feeds res_is_main_display(), so it stays on the existing path.
 */
enum dcp_client_step_kind {
	DCP_STEP_A445, DCP_STEP_A411, DCP_STEP_A415, DCP_STEP_A450,
	DCP_STEP_A031, DCP_STEP_A414, DCP_STEP_A478, DCP_STEP_A457,
	DCP_STEP_A474, DCP_STEP_A476,
};

static const u8 dcp_client_open_seq[] = {
	/*
	 * macOS reaches the client phase only after A353 and two A352s (with
	 * five A438 property pushes in between, which we cannot replay yet).
	 * Without them the firmware never answers the first A445.
	 */
	/*
	 * A352 is left out: the firmware stops answering it here, and macOS
	 * only reaches it after five A438 property pushes we cannot replay yet.
	 */
	DCP_STEP_A445, DCP_STEP_A411, DCP_STEP_A445, DCP_STEP_A415,
	DCP_STEP_A450,
	DCP_STEP_A031, DCP_STEP_A031, DCP_STEP_A031, DCP_STEP_A031,
	DCP_STEP_A031, DCP_STEP_A031, DCP_STEP_A031, DCP_STEP_A031,
	DCP_STEP_A031,
	DCP_STEP_A414, DCP_STEP_A478, DCP_STEP_A457, DCP_STEP_A474,
	DCP_STEP_A478, DCP_STEP_A476, DCP_STEP_A478,
};


static void dcp_client_open_run(struct apple_dcp *dcp, void *out, void *cookie)
{
	unsigned long idx = (unsigned long)cookie;
	dcp_callback_t next = dcp_client_open_run;
	void *ncookie = (void *)(idx + 1);

	if (idx >= ARRAY_SIZE(dcp_client_open_seq)) {
		init_3(dcp, NULL, NULL);
		return;
	}

	switch (dcp_client_open_seq[idx]) {
		case DCP_STEP_A445: {
		u32 zero = 0;

		dcp_get_dfb_state(dcp, false, &zero, next, ncookie);
		break;
	}
	case DCP_STEP_A411: {
		u32 handle = 0;

		dcp_set_display_device(dcp, false, &handle, next, ncookie);
		break;
	}
	case DCP_STEP_A415: {
		struct dcp_pipe_cfg_415 cfg = {};

		dcp_pipe_cfg_415(dcp, false, &cfg, next, ncookie);
		break;
	}
	case DCP_STEP_A450: {
		u32 val = 0;

		dcp_enable_disable_video_power_savings(dcp, false, &val, next,
						       ncookie);
		break;
	}
	case DCP_STEP_A031: {
		/*
		 * The nine cumulative states macOS sends, from the 25G83
		 * capture (dcp-calls.log lines 1507-1531).  Each call repeats
		 * the previous set and adds one client.
		 */
		static const u8 notify_clients[9][8] = {
			{},
			{ 6 },
			{ 6, 7 },
			{ 6, 7, 8 },
			{ 6, 7, 8, 10 },
			{ 6, 7, 8, 10, 11 },
			{ 6, 7, 8, 10, 11, 12 },
			{ 6, 7, 8, 10, 11, 12, 25 },
			{ 6, 7, 8, 10, 11, 12, 25, 26 },
		};
		static const u8 notify_count[9] = { 0, 1, 2, 3, 4, 5, 6, 7, 8 };
		struct dcp_pipe_cfg_031 cfg = {};
		unsigned int step = dcp->a031_step;
		unsigned int i;

		if (step >= ARRAY_SIZE(notify_clients))
			step = ARRAY_SIZE(notify_clients) - 1;
		for (i = 0; i < notify_count[step]; i++)
			cfg.clients[i] = cpu_to_le32(notify_clients[step][i]);
		dcp->a031_step++;

		dcp_pipe_cfg_031(dcp, false, &cfg, next, ncookie);
		break;
	}
	case DCP_STEP_A414: {
		u64 val = 0;

		dcp_pipe_cfg_414(dcp, false, &val, next, ncookie);
		break;
	}
	case DCP_STEP_A478:
		dcp_pipe_query_478(dcp, false, next, ncookie);
		break;
	case DCP_STEP_A457:
		dcp_first_client_open(dcp, false, next, ncookie);
		break;
	case DCP_STEP_A474:
		dcp_pipe_query_474(dcp, false, next, ncookie);
		break;
	case DCP_STEP_A476: {
		/*
		 * A476 is IOMobileFramebufferAP::set_dcp_clamshellstate(uint).
		 * We were sending 1 -- copied from the capture without knowing
		 * what it was -- which tells the firmware the lid is shut, so
		 * it parks the internal pipe (power_down_M3) and never scans
		 * out even though the panel and backlight are on.
		 */
		u32 state = dcp_clamshell_state;

		dcp_pipe_set_476(dcp, false, &state, next, ncookie);
		break;
	}
	default:
		dev_warn(dcp->dev, "unknown client-open step %u\n",
			 dcp_client_open_seq[idx]);
		init_3(dcp, NULL, NULL);
		break;
	}
}
#endif

static void init_1b(struct apple_dcp *dcp, void *out, void *cookie)
{
	u32 val = 0;
	dcp_enable_disable_video_power_savings(dcp, false, &val, init_2, NULL);
}

static void init_1(struct apple_dcp *dcp, void *out, void *cookie)
{
#if DCP_FW_VER >= DCP_FW_VERSION(26, 0, 0)
	/*
	 * macOS opens the client sequence with A411 set_display_device(0),
	 * before A450: traced as "CMD 0x0 A411 0x4 0x4 00000000".
	 */
	/* the sequence starts with A445, then A411 */
	dcp_client_open_run(dcp, NULL, (void *)0UL);
#else
	init_1b(dcp, out, cookie);
#endif
}

static void dcp_started(struct apple_dcp *dcp, void *data, void *cookie)
{
	struct iomfb_get_color_remap_mode_req color_remap =
		(struct iomfb_get_color_remap_mode_req){
			.mode = 6,
		};

	dev_info(dcp->dev, "DCP booted\n");

#if DCP_FW_VER >= DCP_FW_VERSION(26, 0, 0)
	/*
	 * H17P starts scanning out the moment it finishes booting, but it has
	 * no surface yet and the stream it uses has no translation table, so
	 * the DART faults continuously and starves the machine.  Park the
	 * display first thing; the normal poweron path brings it back with a
	 * real framebuffer when a mode is set.
	 */
	/* the display is parked at the head of the boot chain, not here */
#endif

	iomfb_get_color_remap_mode(dcp, false, &color_remap, init_1, cookie);
}

void DCP_FW_NAME(iomfb_shutdown)(struct apple_dcp *dcp)
{
	struct DCP_FW_NAME(dcp_set_power_state_req) req = {
		/* defaults are ok */
	};

	dcp_set_power_state(dcp, DCP_POWER_OOB, &req, NULL, NULL);
}
