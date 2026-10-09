// SPDX-License-Identifier: GPL-2.0-only OR MIT
/* Pre-fix function excerpts for regression checks; firmware calls are mocked. */

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
	if (req->index == DCP_H17P_DISP0_REG_INDEX) {
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
