/* SPDX-License-Identifier: GPL-2.0 OR BSD-2-Clause */
/*
 * Apple T8152 (M6) HDMI DPTX PHY sequences
 *
 * The sequences follow AppleT8152DPTXPort / AppleDPTXPort from the 26A434
 * kernelcache and were qualified on a Mac mini (J873) driving a 1280x720@60
 * HDMI monitor from m1n1.
 *
 * "port" is the 0x10000 byte "dptx" window (ADT reg[0], AppleDPTXPort Block::1)
 * and "block0" the 0x4000 byte "core" window (ADT reg[1], Block::0).
 */

#ifndef PHY_APPLE_DPTX_T8152_H
#define PHY_APPLE_DPTX_T8152_H

#include <linux/device.h>
#include <linux/io.h>
#include <linux/types.h>

struct apple_tunable;

#define DPTX_T8152_MAX_LANES 4

/*
 * Source select, pll/aux/phy group enables, the dptx-phy tunables (applied to
 * "port"), then pll, aux and phy activation and impedance calibration.
 */
int dptx_t8152_activate(struct device *dev, void __iomem *port, void __iomem *block0,
			u32 dcp_index, struct apple_tunable *tunable);
/* Drops the lane enable (phyPowerDown). */
void dptx_t8152_deactivate(void __iomem *port);
/* @lanes is 1 to 4; 0 is a no-op as in AppleT8152DPTXPort. */
int dptx_t8152_set_active_lanes(void __iomem *port, u32 lanes);
/* @link_rate in Mbit/s per lane: 1620, 2700, 5400 or 8100. */
int dptx_t8152_set_link_rate(struct device *dev, void __iomem *port, void __iomem *block0,
			     u32 link_rate);

#endif /* PHY_APPLE_DPTX_T8152_H */
