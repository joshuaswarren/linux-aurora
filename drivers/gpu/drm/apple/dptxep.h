#ifndef __APPLE_DCP_DPTXEP_H__
#define __APPLE_DCP_DPTXEP_H__

#include <linux/phy/phy.h>
#include <linux/mux/consumer.h>

enum dptx_apcall {
	DPTX_APCALL_ACTIVATE = 0,
	DPTX_APCALL_DEACTIVATE = 1,
	DPTX_APCALL_GET_MAX_DRIVE_SETTINGS = 2,
	DPTX_APCALL_SET_DRIVE_SETTINGS = 3,
	DPTX_APCALL_GET_DRIVE_SETTINGS = 4,
	DPTX_APCALL_WILL_CHANGE_LINKG_CONFIG = 5,
	DPTX_APCALL_DID_CHANGE_LINK_CONFIG = 6,
	DPTX_APCALL_GET_MAX_LINK_RATE = 7,
	DPTX_APCALL_GET_LINK_RATE = 8,
	DPTX_APCALL_SET_LINK_RATE = 9,
	DPTX_APCALL_GET_MAX_LANE_COUNT = 10,
	DPTX_APCALL_GET_ACTIVE_LANE_COUNT = 11,
	DPTX_APCALL_SET_ACTIVE_LANE_COUNT = 12,
	DPTX_APCALL_GET_SUPPORTS_DOWN_SPREAD = 13,
	DPTX_APCALL_GET_DOWN_SPREAD = 14,
	DPTX_APCALL_SET_DOWN_SPREAD = 15,
	DPTX_APCALL_GET_SUPPORTS_LANE_MAPPING = 16,
	DPTX_APCALL_SET_LANE_MAP = 17,
	DPTX_APCALL_GET_SUPPORTS_HPD = 18,
	DPTX_APCALL_FORCE_HOTPLUG_DETECT = 19,
	DPTX_APCALL_INACTIVE_SINK_DETECTED = 20,
	DPTX_APCALL_SET_TILED_DISPLAY_HINTS = 21,
	DPTX_APCALL_DEVICE_NOT_RESPONDING = 22,
	DPTX_APCALL_DEVICE_BUSY_TIMEOUT = 23,
	DPTX_APCALL_DEVICE_NOT_STARTED = 24,
	/*
	 * 25G83 sends this one on connect and it is not in the classic table.
	 * Eryk's M4 client (m4_dcpext_dptx.c, transcribed from the 25G83
	 * RemotePortProxy command table) accepts it exactly like 18:
	 *   else if ((command == 18 || command == 26) && data_size == 32)
	 *           ret = 0;
	 * -- accepted with success and no reply payload; only 18 carries the
	 * HPD boolean.  Left unhandled it falls to the default arm, which
	 * ACKs with whatever is in the reply buffer.
	 */
	DPTX_APCALL_SINK_QUERY_25G83 = 26,
};

#define DCPDPTX_REMOTE_PORT_CORE GENMASK(3, 0)
#define DCPDPTX_REMOTE_PORT_ATC GENMASK(7, 4)
#define DCPDPTX_REMOTE_PORT_DIE GENMASK(11, 8)
#define DCPDPTX_REMOTE_PORT_CONNECTED BIT(15)
#define DCPDPTX_REMOTE_PORT_SUPPORTS_HPD BIT(8)

enum dptx_link_rate {
	LINK_RATE_RBR = 0x06,
	LINK_RATE_HBR = 0x0a,
	LINK_RATE_HBR2 = 0x14,
	LINK_RATE_HBR3 = 0x1e,
};

struct apple_epic_service;

struct dptx_port {
	bool enabled, connected;
	struct completion enable_completion;
	struct completion linkcfg_completion;
	u32 unit;
	struct apple_epic_service *service;
	union phy_configure_opts phy_ops;
	struct phy *atcphy;
	struct mux_control *mux;
	u32 lane_count;
	u32 link_rate, pending_link_rate;
	u32 drive_settings[2];
	u32 training_settings[4][3];
	/*
	 * Bitmask of AP-call indices already reported as unhandled, so each one
	 * is named once per port instead of on every repetition. The firmware
	 * repeats these during link training, and an unhandled call is a real
	 * gap worth seeing exactly once.
	 */
	u32 unhandled_logged;
};

int dptxport_validate_connection(struct apple_epic_service *service, u8 core,
				 u8 atc, u8 die);
int dptxport_connect(struct apple_epic_service *service, u8 core, u8 atc,
		     u8 die, bool supports_hpd);
int dptxport_request_display(struct apple_epic_service *service);
int dptxport_release_display(struct apple_epic_service *service);
int dptxport_set_hpd(struct apple_epic_service *service, bool hpd);
#endif
