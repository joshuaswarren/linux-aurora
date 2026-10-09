// SPDX-License-Identifier: GPL-2.0-only OR MIT
/* Pre-fix function excerpts for regression checks; firmware calls are mocked. */

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
