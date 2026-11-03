// SPDX-License-Identifier: GPL-2.0-only OR MIT
/* Copyright 2022 Sven Peter <sven@svenpeter.dev> */

#include <linux/bitfield.h>
#include <linux/completion.h>
#include <linux/phy/phy.h>
#include <linux/delay.h>

#include "afk.h"
#include "dcp.h"
#include "dptxep.h"
#include "parser.h"
#include "trace.h"

struct dcpdptx_connection_cmd {
	__le32 unk;
	__le32 target;
} __attribute__((packed));

struct dcpdptx_hotplug_cmd {
	u8 _pad0[16];
	__le32 unk;
} __attribute__((packed));

struct dptxport_apcall_link_rate {
	__le32 retcode;
	u8 _unk0[12];
	__le32 link_rate;
	u8 _unk1[12];
} __attribute__((packed));

struct dptxport_apcall_lane_count {
	__le32 retcode;
	u8 _unk0[12];
	__le64 lane_count;
	u8 _unk1[8];
} __attribute__((packed));

struct dptxport_apcall_set_active_lane_count {
	__le32 retcode;
	u8 _unk0[12];
	__le64 lane_count;
	u8 _unk1[8];
} __packed;

struct dptxport_apcall_get_support {
	__le32 retcode;
	u8 _unk0[12];
	__le32 supported;
	u8 _unk1[12];
} __attribute__((packed));

struct dptxport_apcall_max_drive_settings {
	__le32 retcode;
	u8 _unk0[12];
	__le32 max_drive_settings[3];
	u8 _unk1[4];
};

struct dptxport_apcall_drive_settings {
	__le32 retcode;
	u8 _unk0[12];
	__le32 unk1;
	__le32 unk2;
	__le32 unk3;
	__le32 unk4;
	__le32 unk5;
	__le32 unk6;
	__le32 unk7;
};

struct dptxport_apcall_set_tiled {
	__le32 retcode;
};

int dptxport_validate_connection(struct apple_epic_service *service, u8 core,
				 u8 atc, u8 die)
{
	struct dptx_port *dptx = service->cookie;
	struct dcpdptx_connection_cmd cmd, resp;
	int ret;
	u32 target = FIELD_PREP(DCPDPTX_REMOTE_PORT_CORE, core) |
		     FIELD_PREP(DCPDPTX_REMOTE_PORT_ATC, atc) |
		     FIELD_PREP(DCPDPTX_REMOTE_PORT_DIE, die) |
		     DCPDPTX_REMOTE_PORT_CONNECTED;

	trace_dptxport_validate_connection(dptx, core, atc, die);

	cmd.target = cpu_to_le32(target);
	cmd.unk = cpu_to_le32(0x100);
	ret = afk_service_call(service, 0, 12, &cmd, sizeof(cmd), 40, &resp,
			       sizeof(resp), 40);
	if (ret)
		return ret;

	if (le32_to_cpu(resp.target) != target)
		return -EINVAL;
	if (le32_to_cpu(resp.unk) != 0x100)
		return -EINVAL;

	return 0;
}

int dptxport_connect(struct apple_epic_service *service, u8 core, u8 atc,
		     u8 die, bool supports_hpd)
{
	struct dptx_port *dptx = service->cookie;
	struct dcpdptx_connection_cmd cmd, resp;
	u32 unk_field = supports_hpd ? DCPDPTX_REMOTE_PORT_SUPPORTS_HPD : 0;
	int ret;
	u32 target = FIELD_PREP(DCPDPTX_REMOTE_PORT_CORE, core) |
		     FIELD_PREP(DCPDPTX_REMOTE_PORT_ATC, atc) |
		     FIELD_PREP(DCPDPTX_REMOTE_PORT_DIE, die) |
		     DCPDPTX_REMOTE_PORT_CONNECTED;

	trace_dptxport_connect(dptx, core, atc, die);

	cmd.target = cpu_to_le32(target);
	cmd.unk = cpu_to_le32(unk_field);
	ret = afk_service_call(service, 0, 11, &cmd, sizeof(cmd), 24, &resp,
			       sizeof(resp), 24);
	if (ret)
		return ret;

	if (le32_to_cpu(resp.target) != target)
		return -EINVAL;
	if (le32_to_cpu(resp.unk) != unk_field)
		dev_notice(service->ep->dcp->dev, "unexpected unk field in reply: 0x%x (0x%x)\n",
			  le32_to_cpu(resp.unk), unk_field);

	return 0;
}

int dptxport_request_display(struct apple_epic_service *service)
{
	return afk_service_call(service, 0, 6, NULL, 0, 16, NULL, 0, 16);
}

int dptxport_release_display(struct apple_epic_service *service)
{
	/*
	 * Native RemotePortProxy::displayRelease uses service group 8 (command
	 * 7), unlike the older Linux group 0.  Matched to the J713 25G83
	 * capture; displayRequest stays group 0.
	 */
	return afk_service_call(service, 8, 7, NULL, 0, 16, NULL, 0, 16);
}

int dptxport_set_hpd(struct apple_epic_service *service, bool hpd)
{
	struct dcpdptx_hotplug_cmd cmd, resp;
	int ret;

	memset(&cmd, 0, sizeof(cmd));

	if (hpd)
		cmd.unk = cpu_to_le32(1);

	ret = afk_service_call(service, 8, 8, &cmd, sizeof(cmd), 12, &resp,
			       sizeof(resp), 12);
	if (ret)
		return ret;
	if (le32_to_cpu(resp.unk) != hpd)
		return -EINVAL;
	return 0;
}

struct dptxport_training_entry {
	__le32 unknown;
	__le32 voltage;
	__le32 preemphasis;
} __packed;

struct dptxport_apcall_training {
	__le32 retcode;
	u8 pad0[12];
	__le32 count;
	u8 pad1[12];
	struct dptxport_training_entry entries[];
} __packed;

static bool dptxport_has_native_training(struct apple_epic_service *service)
{
	return of_device_is_compatible(service->ep->dcp->dev->of_node,
				       "apple,t8140-dcpext");
}

static int dptxport_set_native_training(struct dptx_port *dptx,
				       const void *data, size_t size,
				       void *reply_data, size_t reply_size)
{
	const struct dptxport_apcall_training *request = data;
	struct dptxport_apcall_training *reply = reply_data;
	union phy_configure_opts opts = {};
	u32 count;
	size_t bytes;
	int ret;

	if (size < sizeof(*request) || reply_size < sizeof(*reply))
		return -EINVAL;
	count = le32_to_cpu(request->count);
	if (count > ARRAY_SIZE(dptx->training_settings) || count > dptx->lane_count ||
	    count == 3)
		return -EINVAL;
	bytes = sizeof(*request) + count * sizeof(request->entries[0]);
	if (size < bytes || reply_size < bytes)
		return -EINVAL;
	for (u32 i = 0; i < count; i++) {
		opts.dp.voltage[i] = le32_to_cpu(request->entries[i].voltage);
		opts.dp.pre[i] = le32_to_cpu(request->entries[i].preemphasis);
		if (opts.dp.voltage[i] > 3 || opts.dp.pre[i] > 3)
			return -EINVAL;
	}
	if (count) {
		if (!dptx->atcphy)
			return -ENODEV;
		opts.dp.link_rate = dptx->phy_ops.dp.link_rate;
		opts.dp.lanes = count;
		opts.dp.set_voltages = 1;
		ret = phy_configure(dptx->atcphy, &opts);
		if (ret)
			return ret;
	}
	for (u32 i = 0; i < count; i++) {
		dptx->training_settings[i][0] = le32_to_cpu(request->entries[i].unknown);
		dptx->training_settings[i][1] = opts.dp.voltage[i];
		dptx->training_settings[i][2] = opts.dp.pre[i];
		dptx->phy_ops.dp.voltage[i] = opts.dp.voltage[i];
		dptx->phy_ops.dp.pre[i] = opts.dp.pre[i];
	}
	memmove(reply, request, bytes);
	reply->retcode = cpu_to_le32(0);
	return 0;
}

static int dptxport_get_native_training(struct dptx_port *dptx,
				       const void *data, size_t size,
				       void *reply_data, size_t reply_size)
{
	const struct dptxport_apcall_training *request = data;
	struct dptxport_apcall_training *reply = reply_data;
	u32 count;
	size_t bytes;

	if (size < sizeof(*request) || reply_size < sizeof(*reply))
		return -EINVAL;
	/* Get takes the requested count at body+0 and returns status at body+0x10. */
	count = le32_to_cpu(request->retcode);
	if (count > ARRAY_SIZE(dptx->training_settings))
		return -EINVAL;
	bytes = sizeof(*reply) + count * sizeof(reply->entries[0]);
	if (reply_size < bytes)
		return -EINVAL;
	memmove(reply, request, sizeof(*reply));
	for (u32 i = 0; i < count; i++) {
		reply->entries[i].unknown = cpu_to_le32(dptx->training_settings[i][0]);
		reply->entries[i].voltage = cpu_to_le32(dptx->training_settings[i][1]);
		reply->entries[i].preemphasis = cpu_to_le32(dptx->training_settings[i][2]);
	}
	reply->count = cpu_to_le32(0);
	return 0;
}

static int dptxport_set_native_lanes(struct dptx_port *dptx, u32 lanes)
{
	union phy_configure_opts opts = {};
	int ret;

	if (lanes > 4 || lanes == 3)
		return -EINVAL;
	if (!dptx->atcphy)
		return -ENODEV;
	opts.dp.lanes = lanes;
	opts.dp.set_lanes = 1;
	ret = phy_configure(dptx->atcphy, &opts);
	if (ret)
		return ret;
	dptx->lane_count = lanes;
	dptx->phy_ops.dp.lanes = lanes;
	dptx->phy_ops.dp.set_lanes = 0;
	return 0;
}

static int
dptxport_call_get_max_drive_settings(struct apple_epic_service *service,
				     void *reply_, size_t reply_size)
{
	struct dptxport_apcall_max_drive_settings *reply = reply_;

	if (reply_size < sizeof(*reply))
		return -EINVAL;

	if (dptxport_has_native_training(service)) {
		memset(reply, 0, sizeof(*reply));
		reply->max_drive_settings[1] = cpu_to_le32(3);
		reply->max_drive_settings[2] = cpu_to_le32(3);
		return 0;
	}

	reply->retcode = cpu_to_le32(0);
	reply->max_drive_settings[0] = cpu_to_le32(0x3);
	reply->max_drive_settings[1] = cpu_to_le32(0x3);

	return 0;
}

static int
dptxport_call_get_drive_settings(struct apple_epic_service *service,
				     const void *request_, size_t request_size,
				     void *reply_, size_t reply_size)
{
	struct dptx_port *dptx = service->cookie;
	const struct dptxport_apcall_drive_settings *request = request_;
	struct dptxport_apcall_drive_settings *reply = reply_;

	if (dptxport_has_native_training(service))
		return dptxport_get_native_training(dptx, request_, request_size,
						   reply_, reply_size);

	if (reply_size < sizeof(*reply) || request_size < sizeof(*request))
		return -EINVAL;

	*reply = *request;

	/* Clear the rest of the buffer */
	memset(reply_ + sizeof(*reply), 0, reply_size - sizeof(*reply));

	/*
	 * retcode appears to be lane count, seeing 2 for USB-C dp alt mode
	 * with lanes splitted for DP/USB3.
	 */
	if (le32_to_cpu(reply->retcode) != dptx->lane_count)
		dev_err(service->ep->dcp->dev,
			"get_drive_settings: unexpected retcode %d\n",
			reply->retcode);

	reply->retcode = cpu_to_le32(dptx->lane_count);
	reply->unk5 = cpu_to_le32(dptx->drive_settings[0]);
	reply->unk6 = cpu_to_le32(0);
	reply->unk7 = cpu_to_le32(dptx->drive_settings[1]);

	return 0;
}

static int
dptxport_call_set_drive_settings(struct apple_epic_service *service,
				     const void *request_, size_t request_size,
				     void *reply_, size_t reply_size)
{
	struct dptx_port *dptx = service->cookie;
	const struct dptxport_apcall_drive_settings *request = request_;
	struct dptxport_apcall_drive_settings *reply = reply_;

	if (dptxport_has_native_training(service))
		return dptxport_set_native_training(dptx, request_, request_size,
						   reply_, reply_size);

	if (reply_size < sizeof(*reply) || request_size < sizeof(*request))
		return -EINVAL;

	*reply = *request;
	reply->retcode = cpu_to_le32(0);

	dev_info(service->ep->dcp->dev, "set_drive_settings: %d:%d:%d:%d:%d:%d:%d\n",
		 request->unk1, request->unk2, request->unk3, request->unk4,
		 request->unk5, request->unk6, request->unk7);

	dptx->drive_settings[0] = le32_to_cpu(reply->unk5);
	dptx->drive_settings[1] = le32_to_cpu(reply->unk7);

	return 0;
}

static int dptxport_call_get_max_link_rate(struct apple_epic_service *service,
					   void *reply_, size_t reply_size)
{
	struct dptxport_apcall_link_rate *reply = reply_;

	if (reply_size < sizeof(*reply))
		return -EINVAL;

	reply->retcode = cpu_to_le32(0);
	reply->link_rate = cpu_to_le32(LINK_RATE_HBR3);

	return 0;
}

static int dptxport_call_get_max_lane_count(struct apple_epic_service *service,
					   void *reply_, size_t reply_size)
{
	struct dptxport_apcall_lane_count *reply = reply_;
	struct dptx_port *dptx = service->cookie;
	struct apple_dcp *dcp = service->ep->dcp;
	union phy_configure_opts phy_ops;
	int ret;

	if (reply_size < sizeof(*reply))
		return -EINVAL;

	ret = phy_validate(dptx->atcphy, PHY_MODE_DP, 0, &phy_ops);
	if (ret < 0) {
		dev_err(dcp->dev, "phy_validate failed: %d\n", ret);
		reply->retcode = cpu_to_le32(1);
		reply->lane_count = cpu_to_le64(0);
	} else {
		if (phy_ops.dp.lanes < 2) {
			// phy_validate might return 0 lanes if atc phy is not
			// yet switched to DP mode
			dev_dbg(dcp->dev, "get_max_lane_count: phy lanes: %d\n",
				phy_ops.dp.lanes);
			// default to 4 lanes
			dptx->lane_count = 4;
		} else {
			dptx->lane_count = phy_ops.dp.lanes;
		}
		reply->retcode = cpu_to_le32(0);
		reply->lane_count = cpu_to_le64(dptx->lane_count);
	}

	return 0;
}

static int dptxport_call_set_active_lane_count(struct apple_epic_service *service,
					       const void *data, size_t data_size,
					       void *reply_, size_t reply_size)
{
	struct dptx_port *dptx = service->cookie;
	struct apple_dcp *dcp = service->ep->dcp;
	const struct dptxport_apcall_set_active_lane_count *request = data;
	struct dptxport_apcall_set_active_lane_count *reply = reply_;
	int ret = 0;
	int retcode = 0;

	if (reply_size < sizeof(*reply))
		return -1;
	if (data_size < sizeof(*request))
		return -1;

	u64 lane_count = le64_to_cpu(request->lane_count);

	if (dptxport_has_native_training(service)) {
		if (lane_count > 4)
			return -EINVAL;
		ret = dptxport_set_native_lanes(dptx, lane_count);
		if (ret)
			return ret;
		reply->retcode = cpu_to_le32(0);
		reply->lane_count = cpu_to_le64(lane_count);
		if (lane_count)
			complete(&dptx->linkcfg_completion);
		return 0;
	}

	if (dptx->lane_count < lane_count)
		dev_err(dcp->dev, "set_active_lane_count: unexpected lane "
			"count:%llu phy: %d\n", lane_count, dptx->lane_count);

	switch (lane_count) {
	case 0 ... 2:
	case 4:
		dptx->phy_ops.dp.lanes = lane_count;
		// Use dptx phy index > 3 as indication for dptx-phy or
		// lpdptx-phy and configure the number of lanes for those
		dptx->phy_ops.dp.set_lanes = (dcp->dptx_phy > 3);
		break;
	default:
		dev_err(dcp->dev, "set_active_lane_count: invalid lane count:%llu\n", lane_count);
		retcode = 1;
		lane_count = 0;
		break;
	}

	if (dptx->phy_ops.dp.set_lanes) {
		if (dptx->atcphy) {
			ret = phy_configure(dptx->atcphy, &dptx->phy_ops);
			if (ret)
				return ret;
		}
		dptx->phy_ops.dp.set_lanes = 0;
		dptx->lane_count = lane_count;
	}

	reply->retcode = cpu_to_le32(retcode);
	reply->lane_count = cpu_to_le64(lane_count);

	if (lane_count > 0)
		complete(&dptx->linkcfg_completion);

	return ret;
}

static int dptxport_call_get_link_rate(struct apple_epic_service *service,
				       void *reply_, size_t reply_size)
{
	struct dptx_port *dptx = service->cookie;
	struct dptxport_apcall_link_rate *reply = reply_;

	if (reply_size < sizeof(*reply))
		return -EINVAL;

	reply->retcode = cpu_to_le32(0);
	reply->link_rate = cpu_to_le32(dptx->link_rate);

	return 0;
}

static int
dptxport_call_will_change_link_config(struct apple_epic_service *service)
{
	struct dptx_port *dptx = service->cookie;

	dptx->phy_ops.dp.set_lanes = 0;
	dptx->phy_ops.dp.set_rate = 0;
	dptx->phy_ops.dp.set_voltages = 0;

	return 0;
}

static int
dptxport_call_did_change_link_config(struct apple_epic_service *service)
{
	/* assume the link config did change and wait a little bit */
	mdelay(10);

	return 0;
}

static int dptxport_call_set_link_rate(struct apple_epic_service *service,
				       const void *data, size_t data_size,
				       void *reply_, size_t reply_size)
{
	struct dptx_port *dptx = service->cookie;
	const struct dptxport_apcall_link_rate *request = data;
	struct dptxport_apcall_link_rate *reply = reply_;
	u32 link_rate, phy_link_rate;
	bool phy_set_rate = false;
	int ret;

	if (reply_size < sizeof(*reply))
		return -EINVAL;
	if (data_size < sizeof(*request))
		return -EINVAL;

	link_rate = le32_to_cpu(request->link_rate);
	trace_dptxport_call_set_link_rate(dptx, link_rate);

	switch (link_rate) {
	case LINK_RATE_RBR:
		phy_link_rate = 1620;
		phy_set_rate = true;
		break;
	case LINK_RATE_HBR:
		phy_link_rate = 2700;
		phy_set_rate = true;
		break;
	case LINK_RATE_HBR2:
		phy_link_rate = 5400;
		phy_set_rate = true;
		break;
	case LINK_RATE_HBR3:
		phy_link_rate = 8100;
		phy_set_rate = true;
		break;
	case 0:
		phy_link_rate = 0;
		phy_set_rate = true;
		break;
	default:
		dev_err(service->ep->dcp->dev,
			"DPTXPort: Unsupported link rate 0x%x requested\n",
			link_rate);
		link_rate = 0;
		phy_set_rate = false;
		break;
	}

	if (phy_set_rate) {
		dptx->phy_ops.dp.link_rate = phy_link_rate;
		dptx->phy_ops.dp.set_rate = 1;

		if (dptx->atcphy) {
			ret = phy_configure(dptx->atcphy, &dptx->phy_ops);
			if (ret)
				return ret;
		}

		//if (dptx->phy_ops.dp.set_rate)
		dptx->link_rate = dptx->pending_link_rate = link_rate;

	}

	//dptx->pending_link_rate = link_rate;
	reply->retcode = cpu_to_le32(0);
	reply->link_rate = cpu_to_le32(link_rate);

	return 0;
}

static int dptxport_call_get_supports_hpd(struct apple_epic_service *service,
					  void *reply_, size_t reply_size)
{
	struct apple_dcp *dcp = service->ep->dcp;
	struct dptxport_apcall_get_support *reply = reply_;

	if (reply_size < sizeof(*reply))
		return -EINVAL;

	reply->retcode = cpu_to_le32(0);
	reply->supported = cpu_to_le32(dcp_is_typec_output(dcp));
	return 0;
}

static int
dptxport_call_get_supports_downspread(struct apple_epic_service *service,
				      void *reply_, size_t reply_size)
{
	struct dptxport_apcall_get_support *reply = reply_;

	if (reply_size < sizeof(*reply))
		return -EINVAL;

	reply->retcode = cpu_to_le32(0);
	reply->supported = cpu_to_le32(0);
	return 0;
}

static int dptxport_call_set_tiled_display_hint(void *reply_,
						 size_t reply_size)
{
	struct dptxport_apcall_set_tiled *reply = reply_;

	if (reply_size < sizeof(*reply))
		return -EINVAL;

	reply->retcode = cpu_to_le32(1);
	return 0;
}

static int
dptxport_call_activate(struct apple_epic_service *service,
		       const void *data, size_t data_size,
		       void *reply, size_t reply_size)
{
	struct dptx_port *dptx = service->cookie;
	const struct apple_dcp *dcp = service->ep->dcp;

	/* Standalone PHYs need DCP input selection here. Type-C owns ATC PHY mode. */
	if (!dcp->phy_managed_by_typec)
		phy_set_mode_ext(dptx->atcphy, PHY_MODE_DP, dcp->index);

	memcpy(reply, data, min(reply_size, data_size));
	if (reply_size >= 4)
		memset(reply, 0, 4);

	return 0;
}

static int
dptxport_call_deactivate(struct apple_epic_service *service,
		       const void *data, size_t data_size,
		       void *reply, size_t reply_size)
{
	struct dptx_port *dptx = service->cookie;
	const struct apple_dcp *dcp = service->ep->dcp;

	if (!dcp->phy_managed_by_typec)
		phy_set_mode_ext(dptx->atcphy, PHY_MODE_INVALID, 0);

	memcpy(reply, data, min(reply_size, data_size));
	if (reply_size >= 4)
		memset(reply, 0, 4);

	return 0;
}

static int dptxport_call(struct apple_epic_service *service, u32 idx,
			 const void *data, size_t data_size, void *reply,
			 size_t reply_size)
{
	struct dptx_port *dptx = service->cookie;
	trace_dptxport_apcall(dptx, idx, data_size);

	switch (idx) {
	case DPTX_APCALL_WILL_CHANGE_LINKG_CONFIG:
		return dptxport_call_will_change_link_config(service);
	case DPTX_APCALL_DID_CHANGE_LINK_CONFIG:
		return dptxport_call_did_change_link_config(service);
	case DPTX_APCALL_GET_MAX_LINK_RATE:
		return dptxport_call_get_max_link_rate(service, reply,
						       reply_size);
	case DPTX_APCALL_GET_LINK_RATE:
		return dptxport_call_get_link_rate(service, reply, reply_size);
	case DPTX_APCALL_SET_LINK_RATE:
		return dptxport_call_set_link_rate(service, data, data_size,
						   reply, reply_size);
	case DPTX_APCALL_GET_MAX_LANE_COUNT:
		return dptxport_call_get_max_lane_count(service, reply, reply_size);
        case DPTX_APCALL_SET_ACTIVE_LANE_COUNT:
		return dptxport_call_set_active_lane_count(service, data, data_size,
							   reply, reply_size);
	case DPTX_APCALL_GET_SUPPORTS_HPD:
		return dptxport_call_get_supports_hpd(service, reply,
						      reply_size);
	case DPTX_APCALL_GET_SUPPORTS_DOWN_SPREAD:
		return dptxport_call_get_supports_downspread(service, reply,
							     reply_size);
	case DPTX_APCALL_GET_MAX_DRIVE_SETTINGS:
		return dptxport_call_get_max_drive_settings(service, reply,
							    reply_size);
	case DPTX_APCALL_SET_TILED_DISPLAY_HINTS:
		memcpy(reply, data, min(reply_size, data_size));
		return dptxport_call_set_tiled_display_hint(reply, reply_size);
	case DPTX_APCALL_GET_DRIVE_SETTINGS:
		return dptxport_call_get_drive_settings(service, data, data_size,
							reply, reply_size);
	case DPTX_APCALL_SET_DRIVE_SETTINGS:
		return dptxport_call_set_drive_settings(service, data, data_size,
							reply, reply_size);
        case DPTX_APCALL_ACTIVATE:
		return dptxport_call_activate(service, data, data_size,
					      reply, reply_size);
	case DPTX_APCALL_DEACTIVATE:
		return dptxport_call_deactivate(service, data, data_size,
						reply, reply_size);
	case DPTX_APCALL_DEVICE_NOT_RESPONDING:
	case DPTX_APCALL_DEVICE_BUSY_TIMEOUT:
	case DPTX_APCALL_DEVICE_NOT_STARTED:
		/*
		 * These three are the firmware telling us the link failed.
		 * They were reaching the default arm and being logged as
		 * "acking unhandled call 22" at info level, which reads like
		 * an unimplemented opcode rather than a dead display.
		 */
		dev_warn(service->ep->dcp->dev,
			 "DPTXPort: firmware reports link fault %u on %u:%u\n",
			 idx, service->ep->dcp->dptx_die,
			 service->ep->dcp->dptx_phy);
		memcpy(reply, data, min(reply_size, data_size));
		if (reply_size >= 4)
			memset(reply, 0, 4);
		return 0;
	case DPTX_APCALL_SINK_QUERY_25G83:
		/*
		 * Accepted with success and a zeroed retcode, as the native
		 * client does.  See the comment on the enum.
		 */
		memcpy(reply, data, min(reply_size, data_size));
		if (reply_size >= 4)
			memset(reply, 0, 4);
		return 0;
	default:
		/*
		 * Not implemented here. The reply echoes the request with a zero
		 * retcode, which is what the native client does for the calls it
		 * accepts without a payload -- but for a call that genuinely
		 * wants an answer this is a guess, so say which one it was.
		 *
		 * Reported once per index per port: the firmware repeats these
		 * throughout link training, and the useful artefact is the set of
		 * calls we do not implement, not one line per repetition.
		 */
		if (idx < 32 && !(dptx->unhandled_logged & BIT(idx))) {
			dptx->unhandled_logged |= BIT(idx);
			dev_warn(service->ep->dcp->dev,
				 "DPTXPort: AP call %u is not implemented; echoing the request with a zero retcode\n",
				 idx);
		}
		fallthrough;
	case DPTX_APCALL_GET_DOWN_SPREAD:
	case DPTX_APCALL_SET_DOWN_SPREAD:
		memcpy(reply, data, min(reply_size, data_size));
		if (reply_size >= 4)
			memset(reply, 0, 4);
		return 0;
	}
}

static void dptxport_init(struct apple_epic_service *service, const char *name,
			  const char *class, s64 unit)
{
	struct apple_dcp *dcp = service->ep->dcp;
	s64 port;

	/*
	 * Both mismatches used to return silently, and the only symptom was
	 * dptxep_init() timing out a second later on a port that never
	 * enabled.  Say which string did not match.
	 */
	if (strcmp(name, "dcpdptx-port-epic")) {
		dev_warn(dcp->dev,
			 "DPTXPort: ignoring service named '%s', expected 'dcpdptx-port-epic'\n",
			 name);
		return;
	}
	if (strcmp(class, "AppleDCPDPTXRemotePort")) {
		dev_warn(dcp->dev,
			 "DPTXPort: service '%s' has class '%s', expected 'AppleDCPDPTXRemotePort'\n",
			 name, class);
		return;
	}

	trace_dptxport_init(dcp, unit);

	/*
	 * On H17P the announced "unit" is the dynamic EPIC interface id, not a
	 * zero-based port index (the two 0x24 controllers announce as 1 and 3).
	 * The connect path only ever uses dptxport[0..num_dptx_ports); on this
	 * single-port controller map the sole RemotePort to index 0 so a
	 * nonzero interface id does not make it miss dptxport[0].  Classic
	 * firmware keeps its 0/1 unit semantics.
	 */
	if (dcp->fw_compat == DCP_FIRMWARE_H17P && dcp->hw.num_dptx_ports == 1)
		port = 0;
	else
		port = unit;

	switch (port) {
	case 0:
	case 1:
		if (dcp->dptxport[port].enabled) {
			dev_err(dcp->dev,
				"DPTXPort: unit %lld (port %lld) already exists\n",
				unit, port);
			return;
		}
		dcp->dptxport[port].unit = port;
		dcp->dptxport[port].service = service;
		dcp->dptxport[port].enabled = true;
		service->cookie = (void *)&dcp->dptxport[port];
		complete(&dcp->dptxport[port].enable_completion);
		dev_info(dcp->dev,
			 "DPTXPort: enabled port %lld from announced unit %lld\n",
			 port, unit);
		break;
	default:
		dev_err(dcp->dev, "DPTXPort: invalid unit %lld\n", unit);
	}
}

/*
 * The DisplayPort controller services.  We do not drive them yet -- starting
 * their endpoints at all is the point, because the firmware will not bring up
 * the DP stack (and so never announces "dcpdptx-port-epic" on 0x2a) until
 * they are up.  Log whatever turns up so the set can be confirmed on real
 * hardware; matching is by EPICProviderClass, so both the class and the
 * service name are listed for each.
 */
static void dp_control_init(struct apple_epic_service *service,
			    const char *name, const char *class, s64 unit)
{
	struct apple_dcp *dcp = service->ep->dcp;

	dev_info(dcp->dev,
		 "J700_DP_CONTROL: ep:%02x service '%s' class '%s' unit %lld\n",
		 service->ep->endpoint, name, class, unit);

	/*
	 * Keep them apart by EPIC name: both announce as AppleDCPDPTXController,
	 * on units 1 ("dcpav-controller-epic") and 3 ("dcpdp-controller-epic").
	 * Do not call into them from here -- this runs on the AFK work queue
	 * that would have to deliver the reply.
	 */
	if (!strcmp(name, "dcpav-controller-epic"))
		dcp->dptxctl_av = service;
	else if (!strcmp(name, "dcpdp-controller-epic"))
		dcp->dptxctl_dp = service;
}

/*
 * Command numbers on the controller service, from m1n1's
 * DCPAVControllerService (proxyclient/m1n1/fw/dcp/dcpav.py): group 8, with
 * setPower=0x8, getPower=0x9, wakeDisplay=0xa, sleepDisplay=0xb,
 * forceHotPlugDetect=0xc, setVirtualDeviceMode=0xd.  The payload shape is the
 * same <16xI12x> that dcpdptx_hotplug_cmd already describes.
 */
#define DPTXCTL_SET_POWER		0x8
#define DPTXCTL_WAKE_DISPLAY		0xa
#define DPTXCTL_FORCE_HPD		0xc

static int dptxctl_call(struct apple_dcp *dcp, struct apple_epic_service *svc,
			const char *what, u32 command, u32 arg)
{
	struct dcpdptx_hotplug_cmd cmd, resp;
	int ret;

	memset(&cmd, 0, sizeof(cmd));
	cmd.unk = cpu_to_le32(arg);

	ret = afk_service_call(svc, 8, command, &cmd, sizeof(cmd), 12, &resp,
			       sizeof(resp), 12);
	dev_info(dcp->dev, "J700_DPTXCTL: %s(cmd=%#x arg=%u) -> %d resp=%#x\n",
		 what, command, arg, ret, le32_to_cpu(resp.unk));
	return ret;
}

/*
 * Nudge the DisplayPort controller after IOMFB is up.  Binding the controller
 * services is not on its own enough to make the firmware publish
 * "dcpdptx-port-epic" on 0x2a: it powers the external pipe straight back down
 * ("HPD not present, skipping mode export" -> "IOMFB: power on -> off").  Ask
 * it to power up and re-run hotplug detection.
 */
void dptx_control_kick(struct apple_dcp *dcp)
{
	if (dcp->dptxctl_av) {
		dptxctl_call(dcp, dcp->dptxctl_av, "av.setPower",
			     DPTXCTL_SET_POWER, 1);
		dptxctl_call(dcp, dcp->dptxctl_av, "av.wakeDisplay",
			     DPTXCTL_WAKE_DISPLAY, 0);
		dptxctl_call(dcp, dcp->dptxctl_av, "av.forceHotPlugDetect",
			     DPTXCTL_FORCE_HPD, 0);
	} else {
		dev_info(dcp->dev, "J700_DPTXCTL: no dcpav controller bound\n");
	}

	if (dcp->dptxctl_dp)
		dptxctl_call(dcp, dcp->dptxctl_dp, "dp.setPower",
			     DPTXCTL_SET_POWER, 1);
	else
		dev_info(dcp->dev, "J700_DPTXCTL: no dcpdp controller bound\n");
}

/*
 * Matching is by EPICProviderClass, not by the EPIC service name.  On J700
 * endpoint 0x24 announces two services, both of class "AppleDCPDPTXController",
 * on channels 1 and 3.
 */
static const struct apple_epic_service_ops dptx_control_ops[] = {
	{ .name = "AppleDCPDPTXController", .init = dp_control_init },
	{}
};

/*
 * Start the two DisplayPort controller endpoints.  Errors are not fatal: the
 * caller carries on to dptxep_init() either way, so a machine whose firmware
 * does not have these endpoints behaves exactly as before.
 */
int dptx_control_init(struct apple_dcp *dcp)
{
	int ret;

	dcp->dptxcontrolep = afk_init(dcp, DPTX_CONTROL_ENDPOINT,
				      dptx_control_ops);
	if (IS_ERR(dcp->dptxcontrolep))
		return PTR_ERR(dcp->dptxcontrolep);

	ret = afk_start(dcp->dptxcontrolep);
	if (ret) {
		dev_warn(dcp->dev, "Failed to start DPTX control endpoint: %d\n",
			 ret);
		return ret;
	}

	return 0;
}

static const struct apple_epic_service_ops dptxep_ops[] = {
	{
		.name = "AppleDCPDPTXRemotePort",
		.init = dptxport_init,
		.call = dptxport_call,
	},
	{}
};

int dptxep_init(struct apple_dcp *dcp)
{
	int ret;

	init_completion(&dcp->dptxport[0].enable_completion);
	init_completion(&dcp->dptxport[1].enable_completion);
	init_completion(&dcp->dptxport[0].linkcfg_completion);
	init_completion(&dcp->dptxport[1].linkcfg_completion);

	/*
	 * H17P publishes "dcpdptx-port-epic" on 0x2c, not 0x2a -- see the
	 * comment on DPTX_PORT_ENDPOINT_H17P.  Traced against this machine's
	 * own macOS on the same firmware build.
	 */
	dcp->dptxep = afk_init(dcp,
			       dcp->fw_compat == DCP_FIRMWARE_H17P ?
				       DPTX_PORT_ENDPOINT_H17P :
				       DPTX_ENDPOINT,
			       dptxep_ops);
	if (IS_ERR(dcp->dptxep))
		return PTR_ERR(dcp->dptxep);

	ret = afk_start(dcp->dptxep);
	if (ret)
		return ret;

	return 0;
}

/*
 * Wait for the firmware to publish its DisplayPort port service.
 *
 * This used to sit at the end of dptxep_init(), which runs before
 * iomfb_start_rtkit().  On T8140 that inverts the dependency: the firmware
 * does not announce "dcpdptx-port-epic" until IOMFB is running, and IOMFB
 * cannot start while this wait holds the caller.  Measured on J700
 * 2026-09-22, the two events are 3 ms apart in exactly the wrong order:
 *
 *   [4.390] AFK[ep:2a]: getbuf size 0x4000 tag 0xcafe
 *   [5.411] Failed to start DPTX endpoint: -110     <- 1 s wait gives up
 *   [5.414] IOMFB: init complete (version 64)       <- 3 ms later
 *
 * dptxport[0].enabled therefore stayed false for the rest of the boot, and
 * every later dcp_dptx_connect() refused with "dptx service for port 0 not
 * enabled" -- including the one raised by a real DisplayPort hotplug.
 *
 * Starting the endpoint is still done early, so a service announcement always
 * has somewhere to land; only the wait moves after IOMFB.
 */
int dptxep_wait_ports(struct apple_dcp *dcp)
{
	unsigned long timeout = msecs_to_jiffies(1000);
	u32 port;
	int ret;

	if (IS_ERR_OR_NULL(dcp->dptxep))
		return -ENODEV;

	for (port = 0; port < dcp->hw.num_dptx_ports; port++) {
		if (dcp->dptxport[port].enabled)
			continue;
		ret = wait_for_completion_timeout(&dcp->dptxport[port].enable_completion,
						timeout);
		if (!ret)
			return -ETIMEDOUT;
		else if (ret < 0)
			return ret;
		timeout = ret;
	}

	return 0;
}
