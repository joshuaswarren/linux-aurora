// SPDX-License-Identifier: GPL-2.0-only OR MIT
/* Pre-fix function excerpts for regression checks; firmware calls are mocked. */

bool dcp_is_typec_output(struct apple_dcp *dcp)
{
	return dcp->active_typec_route ||
	       dcp->fixed_connector_type == DRM_MODE_CONNECTOR_USB;
}

int dcp_dptx_connect_oob(struct platform_device *pdev, u32 port)
{
	struct apple_dcp *dcp = platform_get_drvdata(pdev);
	int ret;

	if (dcp_is_typec_output(dcp)) {
		WRITE_ONCE(dcp->typec_cable_connected, true);
		dcp->typec_reconnect_tries = 0;
		cancel_delayed_work(&dcp->typec_reconnect_wq);
	}

	ret = dcp_dptx_connect(dcp, port);
	if (ret && dcp_is_typec_output(dcp))
		mod_delayed_work(system_freezable_wq, &dcp->typec_reconnect_wq,
				 DPTX_RECONNECT_DELAY);

	return ret;
}

int dcp_dptx_disconnect_oob(struct platform_device *pdev, u32 port)
{
	struct apple_dcp *dcp = platform_get_drvdata(pdev);

	if (dcp_is_typec_output(dcp)) {
		WRITE_ONCE(dcp->typec_unplugged, true);
		WRITE_ONCE(dcp->typec_cable_connected, false);
		cancel_delayed_work(&dcp->typec_reconnect_wq);
	}

	disconnected_hpd_event(dcp->connector);

	if (dcp->avep)
		av_service_disconnect(dcp);

	if (dcp->dptxport[port].enabled)
		dptxport_set_hpd(dcp->dptxport[port].service, false);

	return dcp_dptx_disconnect(dcp, port);
}

void dcp_poweron(struct platform_device *pdev)
{
	struct apple_dcp *dcp = platform_get_drvdata(pdev);
	int ret;

	/* CRTC commits serialize the old disable before this new enable. */
	WRITE_ONCE(dcp->typec_unplugged, false);

	if (dcp_is_typec_output(dcp)) {
		/*
		 * A Type-C CRTC disable releases its DPTX session. Re-establish it
		 * synchronously before IOMFB is powered back on.
		 */
		if (READ_ONCE(dcp->typec_cable_connected)) {
			cancel_delayed_work(&dcp->typec_reconnect_wq);
			dcp->typec_reconnect_tries = 0;
			ret = dcp_dptx_connect(dcp, 0);
			if (ret)
				mod_delayed_work(system_freezable_wq,
						 &dcp->typec_reconnect_wq,
						 DPTX_RECONNECT_DELAY);
		}
	} else if (dcp->hdmi_hpd) {
		bool connected = gpiod_get_value_cansleep(dcp->hdmi_hpd);
		dev_info(dcp->dev, "%s: DP2HDMI HPD connected:%d\n", __func__, connected);

		if (connected)
			dcp_dptx_connect(dcp, 0);
	}

	switch (dcp->fw_compat) {
	case DCP_FIRMWARE_V_12_3:
		iomfb_poweron_v12_3(dcp);
		break;
	case DCP_FIRMWARE_V_13_5:
		iomfb_poweron_v13_3(dcp);
		break;
	case DCP_FIRMWARE_H17P:
		iomfb_poweron_h17p(dcp);
		break;
	default:
		WARN_ONCE(true, "Unexpected firmware version: %u\n", dcp->fw_compat);
		break;
	}

	if (dcp->avep)
		av_service_connect(dcp);
}

void dcp_poweroff(struct platform_device *pdev)
{
	struct apple_dcp *dcp = platform_get_drvdata(pdev);
	int ret;

	/*
	 * Cable removal already deasserts HPD and releases the DPTX display in
	 * dcp_dptx_disconnect_oob(). H17P powers the pipe down as part of that
	 * operation. Starting a surface-clear swap in parallel wedges A406
	 * during the shutdown and prevents every later reconnect.
	 *
	 * Remember the unplug until the next CRTC enable, not merely until the
	 * next cable event: userspace may disable the old CRTC after the route
	 * has been released, or after a quick replug has marked the cable live.
	 * The atomic-disable path retains the old framebuffers until a later
	 * completed swap, so bypassing the clear does not free a live surface.
	 */
	if (dcp->fw_compat == DCP_FIRMWARE_H17P &&
	    READ_ONCE(dcp->typec_unplugged)) {
		dev_info(dcp->dev,
			 "Type-C unplug: DPTX owns shutdown; skipping CRTC clear swap\n");
		return;
	}

	if (dcp->avep)
		av_service_disconnect(dcp);

	_dcp_poweroff(dcp);

	if (dcp_is_typec_output(dcp)) {
		/*
		 * DCP owns a synthetic HPD for Type-C. Release it with the CRTC,
		 * then recreate the session while the cable remains present.
		 */
		if (dcp->dptxport[0].enabled && dcp->dptxport[0].connected) {
			ret = dptxport_set_hpd(dcp->dptxport[0].service, false);
			if (ret)
				dev_warn(dcp->dev,
					 "failed to deassert Type-C DPTX HPD: %d\n", ret);
			dcp_dptx_disconnect(dcp, 0);

			if (READ_ONCE(dcp->typec_cable_connected)) {
				dcp->typec_reconnect_tries = 0;
				mod_delayed_work(system_freezable_wq,
						 &dcp->typec_reconnect_wq,
						 DPTX_RECONNECT_DELAY);
			}
		}
	} else if (dcp->hdmi_hpd) {
		bool connected = gpiod_get_value_cansleep(dcp->hdmi_hpd);
		if (!connected) {
			disconnected_hpd_event(dcp->connector);
			dcp_dptx_disconnect(dcp, 0);
		}
	}
}
