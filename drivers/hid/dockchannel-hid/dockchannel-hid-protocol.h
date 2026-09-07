/* SPDX-License-Identifier: GPL-2.0 OR MIT */
#ifndef _DOCKCHANNEL_HID_PROTOCOL_H
#define _DOCKCHANNEL_HID_PROTOCOL_H

#include <linux/string.h>
#include <linux/types.h>

#define DCHID_CMD_RESET_INTERFACE	0x40
#define DCHID_POWER_METHOD_1		1
#define DCHID_POWER_METHOD_2		2
#define DCHID_POWER_STATE_OFF		0
#define DCHID_POWER_STATE_ON		2
#define DCHID_POWER_METHOD_1_CMD_SIZE	4
#define DCHID_POWER_METHOD_2_CMD_SIZE	9

static inline u8 dchid_default_pm(bool no_stm, bool multi_touch,
				  bool j700_firmware)
{
	return no_stm && multi_touch && j700_firmware ?
		DCHID_POWER_METHOD_2 : DCHID_POWER_METHOD_1;
}

static inline void
dchid_pm1_command(u8 iface, u8 state,
		  u8 command[DCHID_POWER_METHOD_1_CMD_SIZE])
{
	command[0] = DCHID_CMD_RESET_INTERFACE;
	command[1] = DCHID_POWER_METHOD_1;
	command[2] = iface;
	command[3] = state;
}

static inline void
dchid_pm2_state_command(u8 iface, u8 state, bool has_changed,
			u8 command[DCHID_POWER_METHOD_2_CMD_SIZE])
{
	memset(command, 0, DCHID_POWER_METHOD_2_CMD_SIZE);
	command[0] = DCHID_CMD_RESET_INTERFACE;
	command[1] = DCHID_POWER_METHOD_2;
	command[2] = iface;
	command[3] = state;
	command[4] = has_changed;
}

static inline void
dchid_pm2_command(u8 iface, bool has_changed,
		  u8 command[DCHID_POWER_METHOD_2_CMD_SIZE])
{
	dchid_pm2_state_command(iface, DCHID_POWER_STATE_ON, has_changed,
				command);
}

#endif /* _DOCKCHANNEL_HID_PROTOCOL_H */
