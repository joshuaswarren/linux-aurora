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

static const struct dcp_method_entry dcp_methods[dcpep_num_methods] = {
	IOMFB_METHOD("A000", dcpep_late_init_signal),
	IOMFB_METHOD("A024", dcpep_setup_video_limits),
	IOMFB_METHOD("A136", iomfbep_a131_pmu_service_matched),
	IOMFB_METHOD("A137", iomfbep_a132_backlight_service_matched),
	IOMFB_METHOD("A389", dcpep_set_create_dfb),
	IOMFB_METHOD("A390", iomfbep_a358_vi_set_temperature_hint),
	IOMFB_METHOD("A401", dcpep_start_signal),
	IOMFB_METHOD("A406", dcpep_swap_start),
	IOMFB_METHOD("A407", dcpep_swap_submit),
	IOMFB_METHOD("A411", dcpep_set_display_device),
	IOMFB_METHOD("A412", dcpep_is_main_display),
	IOMFB_METHOD("A413", dcpep_set_digital_out_mode),
	IOMFB_METHOD("A423", iomfbep_set_matrix),
	IOMFB_METHOD("A427", iomfbep_get_color_remap_mode),
	IOMFB_METHOD("A449", dcpep_set_parameter_dcp),
	IOMFB_METHOD("A453", dcpep_create_default_fb),
	IOMFB_METHOD("A457", dcpep_enable_disable_video_power_savings),
	IOMFB_METHOD("A465", dcpep_first_client_open),
	IOMFB_METHOD("A466", iomfbep_last_client_close),
	IOMFB_METHOD("A474", dcpep_set_display_refresh_properties),
	IOMFB_METHOD("A477", dcpep_flush_supports_power),
	IOMFB_METHOD("A478", iomfbep_abort_swaps_dcp),
	IOMFB_METHOD("A483", dcpep_set_power_state),
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
	u64 clock_request = dcp->disp_bw_doorbell_res.start;
	u64 scratch = dcp->disp_bw_scratch_res.start +
		      dcp->disp_bw_scratch_offset;

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

static const iomfb_cb_handler cb_handlers[IOMFB_MAX_CB] = {
	[0] = trampoline_true, /* did_boot_signal */
	[1] = trampoline_true, /* did_power_on_signal */
	[2] = trampoline_nop, /* will_power_off_signal */
	[3] = trampoline_rt_bandwidth_h17p,
	[6] = trampoline_set_frame_sync_props,
	[100] = iomfbep_cb_match_pmu_service,
	[101] = trampoline_zero, /* get_display_default_stride */
	[102] = trampoline_nop, /* set_number_property */
	[103] = trampoline_nop, /* trigger_user_cal_loader */
	[104] = trampoline_nop, /* set_boolean_property */
	[107] = trampoline_true, /* create_provider_service */
	[108] = trampoline_true, /* create_product_service */
	[109] = trampoline_true, /* create_pmu_service */
	[110] = trampoline_true, /* create_iomfb_service */
	[111] = trampoline_create_backlight_service,
	[112] = trampoline_true, /* create_nvram_service */
	[114] = trampoline_get_tiling_state,
	[115] = trampoline_false, /* set_tiling_state */
	[120] = dcpep_cb_boot_1,
	[121] = trampoline_false, /* is_dark_boot */
	[122] = trampoline_false, /* is_waking_from_hibernate */
	[124] = trampoline_read_edt_data,
	[126] = trampoline_prop_start,
	[127] = trampoline_prop_chunk,
	[128] = trampoline_prop_end,
	[129] = trampoline_allocate_bandwidth,
	[201] = trampoline_map_piodma,
	[202] = trampoline_unmap_piodma,
	[206] = iomfbep_cb_match_pmu_service_2,
	[207] = iomfbep_cb_match_backlight_service,
	[208] = trampoline_nop, /* update_backlight_factor_prop */
	[209] = trampoline_get_time,
	[300] = trampoline_pr_publish,
	[401] = trampoline_get_uint_prop,
	[404] = trampoline_nop, /* set_uint_prop */
	[406] = trampoline_set_fx_prop,
	[408] = trampoline_get_frequency_h17p,
	[411] = trampoline_map_reg,
	[413] = trampoline_true, /* set_property_dict */
	[414] = trampoline_sr_set_property_int,
	[415] = trampoline_true, /* set_property_bool */
	[451] = trampoline_allocate_buffer,
	[452] = trampoline_map_physical,
	[454] = trampoline_release_mem_desc,
	[552] = trampoline_true, /* set_property_dict_0 */
	[561] = trampoline_true, /* set_property_dict */
	[563] = trampoline_true, /* set_property_int */
	[565] = trampoline_true, /* set_property_bool */
	[567] = trampoline_true, /* set_property_str */
	[572] = trampoline_zero, /* power_up_dart */
	[575] = trampoline_hotplug_h17p,
	[576] = trampoline_nop, /* powerstate_notify */
	[586] = trampoline_true, /* create_default_fb_surface */
	[587] = trampoline_nop, /* clear_default_surface */
	[588] = trampoline_nop, /* swap_notify */
	[589] = trampoline_swap_info_h17p,
	[594] = trampoline_swap_complete,
	[596] = trampoline_swap_complete_intent_gated,
	[597] = trampoline_abort_swap_ap_gated,
	[598] = trampoline_enable_backlight_message_ap_gated,
	[599] = trampoline_nop, /* setSystemConsoleMode */
	[601] = trampoline_false, /* isDFBAllocated */
	[602] = trampoline_false, /* preserveContents */
	[603] = trampoline_nop, /* find_swap_function_gated */
};

void iomfb_start_h17p(struct apple_dcp *dcp)
{
	dcp->cb_handlers = cb_handlers;
	dcp_start_signal(dcp, false, dcp_started, NULL);
}

#undef DCP_FW_VER
#undef DCP_FW
