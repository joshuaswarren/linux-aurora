// SPDX-License-Identifier: GPL-2.0-only OR MIT
/* Copyright The Asahi Linux Contributors */

#include "dcp-link.h"
#include "iomfb_h17p.h"
#include "iomfb_v12_3.h"
#include "iomfb_v13_3.h"
#include "version_utils.h"

struct dcp_h17p_hotplug_request {
	__le64 connected;
	u8 tiled_display[0x4c];
	u8 tiled_display_null;
	u8 padding[3];
} __packed;

struct dcp_h17p_swap_info_request {
	u8 swap_info[0xe0];
	u8 swap_info_null;
	u8 padding[3];
} __packed;

static_assert(sizeof(struct dcp_h17p_hotplug_request) == 0x58);
static_assert(sizeof(struct dcp_h17p_swap_info_request) == 0xe4);
/* D590 swap_complete_ap_gated: 0x730 on the wire in 1919/1919 traced swaps. */
static_assert(sizeof(struct dc_swap_complete_resp_h17p) == 0x730);
static_assert(offsetof(struct dc_swap_complete_resp_h17p, swap_id) == 0);

static const struct dcp_method_entry dcp_methods[dcpep_num_methods] = {
	IOMFB_METHOD("A000", dcpep_late_init_signal),
	IOMFB_METHOD("A025", dcpep_setup_video_limits), /* boot chain: 14th call inside D121, right before flush_supports_power; (0,0) */
	IOMFB_METHOD("A131", iomfbep_a131_pmu_service_matched), /* issued from inside callback D206 */
	IOMFB_METHOD("A132", iomfbep_a132_backlight_service_matched), /* issued from inside callback D207 */
	IOMFB_METHOD("A385", dcpep_set_create_dfb), /* first call inside D121; (0,0) */
	IOMFB_METHOD("A386", iomfbep_a358_vi_set_temperature_hint), /* issued from inside callback D100 */
	IOMFB_METHOD("A401", dcpep_start_signal),
	IOMFB_METHOD("A406", dcpep_swap_start),
	/* swap_start moved A407 -> A406 on H17P, but swap_submit did NOT move
	 * with it: macOS 25G83 issues A406 then A408 for every swap.  Across 20
	 * traced boots A407 is never called once, and the A408 response is 0x0c
	 * bytes, matching dcp_swap_submit_resp_h17p exactly. */
	IOMFB_METHOD("A408", dcpep_swap_submit),
	IOMFB_METHOD("A411", dcpep_set_display_device),
	IOMFB_METHOD("A412", dcpep_is_main_display),
	IOMFB_METHOD("A413", dcpep_set_digital_out_mode),
	IOMFB_METHOD("A423", iomfbep_set_matrix),
	IOMFB_METHOD("A427", iomfbep_get_color_remap_mode),
	IOMFB_METHOD("A442", dcpep_set_parameter_dcp), /* unique (0x28,0x4) signature in the capture */
	IOMFB_METHOD("A446", dcpep_create_default_fb), /* immediately follows set_create_dfb inside D121 */
	/*
	 * The default-framebuffer block macOS runs inside D121, straight after
	 * A446 and before A025.  Sizes are the traced wire sizes.
	 */
	IOMFB_METHOD("A035", dcpep_get_dfb_compression_info), /* (0x8, 0x8) */
	IOMFB_METHOD("A034", dcpep_get_dfb_info),             /* (0x2c, 0x2c) */
	IOMFB_METHOD("A103", dcpep_get_dfb_layout),           /* (0xc, 0x8) */
	IOMFB_METHOD("A445", dcpep_get_dfb_state),            /* (0x4, 0x8) */
	IOMFB_METHOD("A104", dcpep_set_dfb_dimensions),       /* (0x8, 0) w,h */
	IOMFB_METHOD("A105", dcpep_commit_dfb_info),          /* (0, 0) */
	IOMFB_METHOD("A033", dcpep_dfb_query),                /* (0, 0x4) */
	IOMFB_METHOD("A380", dcpep_dfb_ready_query),          /* (0, 0x4) */
	IOMFB_METHOD("A353", dcpep_pipe_query_353),           /* (0, 0x8) */
	IOMFB_METHOD("A352", dcpep_pipe_set_352),             /* (0x8, 0x4) in 0x70 */
	IOMFB_METHOD("A415", dcpep_pipe_cfg_415),             /* (0xc, 0xc) zeros */
	IOMFB_METHOD("A031", dcpep_pipe_cfg_031),             /* (0x6c, 0) zeros */
	IOMFB_METHOD("A414", dcpep_pipe_cfg_414),             /* (0x8, 0x8) zeros */
	IOMFB_METHOD("A478", dcpep_pipe_query_478),           /* (0, 0x4) */
	IOMFB_METHOD("A474", dcpep_pipe_query_474),           /* (0, 0x4) */
	IOMFB_METHOD("A476", dcpep_pipe_set_476),             /* (0x4, 0) in 1 */
	IOMFB_METHOD("A438", dcpep_pipe_cfg_438),             /* (0x106c, 0x4) */
	IOMFB_METHOD("A428", dcpep_pipe_set_428),             /* (0x4, 0x4) 0x10000 */
	IOMFB_METHOD("A450", dcpep_enable_disable_video_power_savings), /* init_1 position, in = u32 0 */
	IOMFB_METHOD("A457", dcpep_first_client_open), /* init_2, between A450 and A412; (0,0) */
	/* UNRESOLVED: A466 is never called by macOS.  No capture contains a
	 * client teardown, so the real tag is unknown (the shift model suggests
	 * A458/A459).  Left as-is; only reached on teardown. */
	IOMFB_METHOD("A466", iomfbep_last_client_close),
	IOMFB_METHOD("A465", dcpep_set_display_refresh_properties), /* while it is in flight the DCP calls back D561 with displayMinRefreshInterval */
	IOMFB_METHOD("A468", dcpep_flush_supports_power), /* immediately follows setup_video_limits inside D121, in = 01 */
	/* UNRESOLVED: A478 is a live (0,0x4) method, so this entry would hand it
	 * a 0x14-byte record.  The real abort_swaps tag is unknown (probably
	 * A469); no capture aborts a swap.  Only reached on teardown. */
	IOMFB_METHOD("A478", iomfbep_abort_swaps_dcp),
	IOMFB_METHOD("A473", dcpep_set_power_state), /* the only OOBCMD call in the capture, matching this method's oob=true; out 0x8 */
	IOMFB_METHOD("A472", dcpep_register_dfb_surface), /* default FB surface, nested in D582 */
};

#define DCP_FW h17p
#define DCP_FW_VER DCP_FW_VERSION(26, 6, 0)

#include "iomfb_template.c"

static bool trampoline_rt_bandwidth_h17p(struct apple_dcp *dcp, int tag,
					 void *out, void *in)
{
	struct apple_dcp_link_rpc_view view = {
		.call = APPLE_DCP_LINK_CALL_D003,
		.input_size = sizeof(struct apple_dcp_h17p_rt_bw_request),
		.output_size = sizeof(struct apple_dcp_h17p_rt_bw_reply),
		.input = in,
		.output = out,
	};
	/*
	 * Take these from the macOS capture rather than from the DT resources.
	 * macOS answers D003 with one stable record,
	 *   0000820203000000 0080860203000000 00000000
	 * -> scratch 0x302820000, doorbell 0x302868000 -- which is
	 * DISP0_REG_BASE + 0x20000 and + 0x68000, both inside the 0xbc000
	 * window we already hand out for map_reg index 5.  The DT lookup lands
	 * 0x5a80000 higher, so the firmware was being pointed at the wrong
	 * block for its bandwidth scratch and doorbell.
	 */
	u64 clock_request = DCP_H17P_DISP0_REG_BASE + 0x68000;
	u64 scratch = DCP_H17P_DISP0_REG_BASE + 0x20000;

	trace_iomfb_callback(dcp, tag, __func__);
	if (apple_dcp_h17p_rt_bw_encode_reply(&view, scratch,
						     clock_request))
		dev_warn(dcp->dev, "D003 bandwidth request is malformed\n");
	return true;
}

static bool trampoline_get_frequency_h17p(struct apple_dcp *dcp, int tag,
					   void *out, void *in)
{
	const struct apple_dcp_h17p_clock_request *request = in;
	u64 rate;
	int ret;

	trace_iomfb_callback(dcp, tag, __func__);
	ret = apple_dcp_h17p_clock_rate(request, clk_get_rate(dcp->clk),
					  clk_get_rate(dcp->clk_194), &rate);
	if (ret) {
		dev_warn(dcp->dev, "unknown display clock ID %#x\n",
			 le32_to_cpu(request->clock_id));
		rate = 0;
	}

	*(__le64 *)out = cpu_to_le64(rate);
	return true;
}

static bool trampoline_hotplug_h17p(struct apple_dcp *dcp, int tag,
				    void *out, void *in)
{
	const struct dcp_h17p_hotplug_request *request = in;

	if (!(request->tiled_display_null & 1))
		memcpy(out, request->tiled_display,
		       sizeof(request->tiled_display));
	return trampoline_hotplug(dcp, tag, out, in);
}

static bool trampoline_swap_info_h17p(struct apple_dcp *dcp, int tag,
				      void *out, void *in)
{
	const struct dcp_h17p_swap_info_request *request = in;

	trace_iomfb_callback(dcp, tag, __func__);
	if (!(request->swap_info_null & 1))
		memcpy(out, request->swap_info, sizeof(request->swap_info));
	return true;
}

/*
 * D574 (in 4, out 8) has no analogue in the v12.3/v13.5 tables.  macOS 25G83
 * answers 0x435b0000 == 219.0f for the only selector it is ever called with
 * (in == 0), identically in both captured boots.  219 is 235 - 16, the
 * BT.601/709 studio-swing luma range, so this feeds the DCP's colour matrix
 * setup; answering zero would put a zero scale into it.
 */
static bool iomfbep_cb_video_range_h17p(struct apple_dcp *dcp, int tag,
					void *out, void *in)
{
	__le32 *resp = out;

	resp[0] = cpu_to_le32(0x435b0000); /* 219.0f */
	resp[1] = 0;
	return true;
}

/*
 * Tag numbering below is the one the H17P firmware actually uses, taken from
 * macOS 25G83 traces on this machine rather than by shifting the v13.5 table:
 * the service-creation block is D108..D113 exactly as on v13.5, and the swap
 * completion is D590.  A callback with no entry here is never acked, so a
 * missing tag stalls the DCP rather than degrading it.
 */
/*
 * D400 -- iOSSvcProx::get_property(name, DataBuffer *, bool).
 *
 * The firmware asks the "PROV" (provider) service for the panel's power model
 * during init_power_lut(); answering "not found" makes it log
 *   IOMFB init_power_lut: failed to read power-lut-data-x
 * and then fail to create the default framebuffer surface.  The request
 * carries a capacity at +0x44 and the reply is { u8 data[0xc00]; __le32
 * length; }, where length is the byte count written and 0 means "no such
 * property".  These are the values macOS 25G83 returns on this machine,
 * byte-identical across both captured boots.
 */
struct dcp_h17p_get_data_prop_req {
	char obj[4];
	char key[0x40];
	__le32 length;
	u8 data_null;
	u8 length_null;
	u8 padding[2];
} __packed;
static_assert(sizeof(struct dcp_h17p_get_data_prop_req) == 0x4c);

struct dcp_h17p_get_data_prop_resp {
	u8 data[0xc00];
	__le32 length;
} __packed;
static_assert(sizeof(struct dcp_h17p_get_data_prop_resp) == 0xc04);

static const u32 h17p_power_lut_x[] = { 13 };
static const u32 h17p_power_lut_y[] = { 1 };
static const u32 h17p_power_lut_yindex[] = { 100 };
static const u32 h17p_power_lut_xindex[] = {
	2, 10, 50, 100, 150, 200, 250, 300, 350, 400, 450, 500, 525,
};
static const u32 h17p_power_lut_values[] = {
	820, 900, 1316, 1874, 2321, 2782, 3282, 3810, 4347, 4964, 5589,
	6248, 6562,
};

static const struct {
	const char *key;
	const u32 *vals;
	u32 count;
} h17p_prov_props[] = {
	{ "power-lut-data-x", h17p_power_lut_x, ARRAY_SIZE(h17p_power_lut_x) },
	{ "power-lut-data-y", h17p_power_lut_y, ARRAY_SIZE(h17p_power_lut_y) },
	{ "power-lut-data-xindex", h17p_power_lut_xindex,
	  ARRAY_SIZE(h17p_power_lut_xindex) },
	{ "power-lut-data-yindex", h17p_power_lut_yindex,
	  ARRAY_SIZE(h17p_power_lut_yindex) },
	{ "power-lut-data-lut", h17p_power_lut_values,
	  ARRAY_SIZE(h17p_power_lut_values) },
};

static bool trampoline_get_data_prop_h17p(struct apple_dcp *dcp, int tag,
					  void *out, void *in)
{
	const struct dcp_h17p_get_data_prop_req *req = in;
	struct dcp_h17p_get_data_prop_resp *resp = out;
	u32 cap = min_t(u32, le32_to_cpu(req->length), sizeof(resp->data));
	unsigned int i, j;

	/* dcpep_handle_cb() already zeroed out, so length == 0 == not found */
	if (memcmp(req->obj, "VORP", sizeof(req->obj)))	/* fourcc 'PROV' */
		return true;

	for (i = 0; i < ARRAY_SIZE(h17p_prov_props); i++) {
		u32 bytes = h17p_prov_props[i].count * sizeof(u32);

		if (strncmp(req->key, h17p_prov_props[i].key,
			    sizeof(req->key)))
			continue;
		if (bytes > cap)
			return true;

		for (j = 0; j < h17p_prov_props[i].count; j++)
			((__le32 *)resp->data)[j] =
				cpu_to_le32(h17p_prov_props[i].vals[j]);
		resp->length = cpu_to_le32(bytes);
		return true;
	}

	return true;
}

static const iomfb_cb_handler cb_handlers[IOMFB_MAX_CB] = {
	[0] = dcpep_cb_d000_h17p, /* macOS calls A033 inside D000 before acking */
	[1] = trampoline_true,
	[2] = trampoline_nop,
	[3] = trampoline_rt_bandwidth_h17p,
	[6] = trampoline_set_frame_sync_props_h17p, /* 0x54/0x50, identity scale */
	[100] = iomfbep_cb_match_pmu_service, /* match_pmu_service */
	[101] = trampoline_zero,
	[102] = trampoline_nop, /* set_number_property */
	[104] = trampoline_nop, /* set_boolean_property */
	[107] = trampoline_nop,
	[108] = trampoline_true, /* create_provider_service */
	[109] = trampoline_true, /* create_product_service */
	[110] = trampoline_true, /* create_PMU_service */
	[111] = trampoline_true, /* create_iomfb_service */
	[112] = trampoline_create_backlight_service, /* create_backlight_service */
	[113] = trampoline_true, /* create_nvram_service */
	[114] = trampoline_zero, /* CoreAnalyticsSendEvent */
	[117] = trampoline_nop, /* set_idle_caching_state_ap */
	[118] = trampoline_zero, /* upload_trace_start */
	[119] = trampoline_zero, /* upload_trace_chunk */
	[120] = trampoline_zero, /* upload_trace_end */
	[121] = dcpep_cb_boot_1, /* start_hardware_boot */
	[122] = trampoline_false, /* is_dark_boot */
	[123] = trampoline_false, /* is_waking_from_hibernate */
	[124] = trampoline_zero, /* detect_fastsim */
	[125] = trampoline_read_edt_data, /* read_edt_data */
	[127] = trampoline_prop_start, /* setDCPAVPropStart */
	[128] = trampoline_prop_chunk, /* setDCPAVPropChunk */
	[129] = trampoline_prop_end, /* setDCPAVPropEnd */
	[130] = trampoline_allocate_bandwidth, /* allocate_bandwidth */
	[201] = trampoline_map_piodma, /* map_buf */
	[202] = trampoline_unmap_piodma, /* unmap_buf */
	[206] = iomfbep_cb_match_pmu_service_2, /* match_pmu_service */
	[207] = iomfbep_cb_match_backlight_service, /* match_backlight_service */
	[208] = trampoline_nop, /* update_backlight_factor_prop */
	[209] = trampoline_get_time, /* get_calendar_time_ms */
	[300] = trampoline_pr_publish,
	[400] = trampoline_get_data_prop_h17p, /* get_property */
	[401] = trampoline_get_uint_prop, /* get_uint_prop */
	[404] = trampoline_nop, /* set_uint_prop */
	[406] = trampoline_set_fx_prop, /* set_fx_prop */
	[408] = trampoline_get_frequency_h17p, /* getClockFrequency */
	[411] = trampoline_map_reg, /* mapDeviceMemoryWithIndex */
	[413] = trampoline_true, /* setProperty */
	[414] = trampoline_sr_set_property_int, /* setProperty */
	[415] = trampoline_true, /* setProperty */
	[451] = trampoline_allocate_buffer, /* allocate_buffer */
	[452] = trampoline_map_physical, /* prepare */
	[454] = trampoline_release_mem_desc, /* release_descriptor */
	[552] = trampoline_true,
	[561] = trampoline_true,
	[563] = trampoline_true,
	[565] = trampoline_true,
	[567] = trampoline_true,
	[572] = trampoline_zero, /* powerUpDART */
	[574] = iomfbep_cb_video_range_h17p,
	[575] = trampoline_hotplug_h17p,
	[576] = trampoline_nop,
	/*
	 * We do not create a default framebuffer surface, so answering "yes"
	 * makes the firmware go on to program the display engine for a surface
	 * that does not exist: it then DMAs from a null base and the piodma
	 * DART (412300000) faults continuously on SID 0, which starves the
	 * system into an RCU stall.  Decline instead.
	 */
	[582] = iomfbep_cb_create_dfb_surface, /* create_default_fb_surface */
	[583] = trampoline_nop, /* clear_default_surface */
	[584] = trampoline_nop, /* swap_notify_gated */
	[585] = trampoline_swap_info_h17p, /* swap_info_notify_dispatch */
	[590] = trampoline_swap_complete, /* swap_complete_ap_gated */
	[592] = trampoline_swap_complete_intent_gated, /* swap_complete_intent_gated */
	[593] = trampoline_abort_swap_ap_gated, /* abort_swap_ap_gated */
	[594] = trampoline_enable_backlight_message_ap_gated, /* enable_backlight_message_ap_gated */
	[595] = trampoline_nop, /* setSystemConsoleMode */
	[597] = trampoline_false, /* isDFBAllocated */
	[598] = trampoline_false,
	[599] = trampoline_nop, /* find_swap_function_gated */
};

void iomfb_start_h17p(struct apple_dcp *dcp)
{
	dcp->cb_handlers = cb_handlers;
	dcp_start_signal(dcp, false, dcp_started, NULL);
}

#undef DCP_FW_VER
#undef DCP_FW
