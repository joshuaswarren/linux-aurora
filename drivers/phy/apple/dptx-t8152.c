// SPDX-License-Identifier: GPL-2.0 OR BSD-2-Clause
/*
 * Apple T8152 (M6) HDMI DPTX PHY sequences
 *
 * Register offsets, field layout and ordering come from AppleT8152DPTXPort,
 * AppleT8112DPTXPort and AppleDPTXPort in the 26A434 kernelcache. The m1n1
 * implementation of the same sequences (src/dcp/t8152_dptx_port.c) was
 * qualified on hardware.
 */

#include <linux/bitfield.h>
#include <linux/bits.h>
#include <linux/bitops.h>
#include <linux/delay.h>
#include <linux/errno.h>
#include <linux/iopoll.h>
#include <linux/kernel.h>
#include <linux/soc/apple/tunable.h>

#include "dptx-t8152.h"

/* Block::0 ("core") */
#define CORE_SOURCE_SELECT	0x10
#define CORE_SOURCE_SELECT_MASK	GENMASK(3, 0)
#define CORE_GROUP_ENABLE	0x48
#define CORE_SHADOW		0xa0

/* Block::1 ("dptx") */
#define PORT_CTRL		0x0000
#define PORT_CTRL_PHY_ON	BIT(0)
#define PORT_CTRL_PLL_ON	BIT(1)
#define PORT_CTRL_FULL_RATE	BIT(17)

#define PLL_CTRL		0x408
#define PLL_CTRL_MODE		GENMASK(2, 0)
#define PLL_CTRL_REQUEST	BIT(3)
#define PLL_CTRL_SEQUENCE	BIT(6)
#define PLL_CTRL_DONE		BIT(19)
#define PLL_STATUS		0x438
#define PLL_STATUS_LOCKED	BIT(30)
#define PLL_MODE_UP		7
#define PLL_MODE_DOWN		0
#define PLL_POLLS		10
#define PLL_POLL_US		16

#define PLL_PCLKDRV		0x8d0
#define PLL_PCLKDRV_BIT		BIT(4)

#define AUX_CMN_CTRL		0x1400
#define AUX_CMN_CTRL_BIT	BIT(0)

#define CMN_CAL			0x1c00
#define CMN_CAL_MASK		GENMASK(9, 4)
#define CMN_CAL_ACTIVATE	0x3c
#define CMN_CAL_DONE		0x3f
#define CMN_CAL_STATUS		0x1c04
#define CMN_CAL_STATUS_DONE	BIT(0)
#define CMN_CAL_RESULT		0x1c18
#define CMN_CAL_RESULT_VALUE	GENMASK(21, 15)
#define CMN_CAL_RESULT_VALID	BIT(14)
#define CMN_CAL_READBACK	0x1c24
#define CMN_CAL_READBACK_VALUE	GENMASK(12, 6)
#define CAL_POLL_US		10
#define CAL_TIMEOUT_US		1000000

#define LANE_CTRL		0x2400
#define LANE_CTRL_ENABLE	BIT(0)
#define LANE_CTRL_UHBR		BIT(2)
#define LANE_CTRL_HALF_RATE	BIT(3)
#define LANE_UNUSED_MASK	0x2420
#define LANE_UNUSED_MASK_BITS	GENMASK(3, 0)

#define LANE0_OFFSET		0x4500
#define LANE_STRIDE		0x2800

struct dptx_t8152_field {
	u16 offset;
	u32 mask;
};

/* Per-lane fields written with the "lane unused" bit, relative to the lane origin. */
static const struct dptx_t8152_field lane_fields[] = {
	{ 0x64, BIT(4) }, { 0x24, BIT(4) }, { 0x24, BIT(1) }, { 0x48, BIT(0) },
	{ 0x48, BIT(1) }, { 0x5c, BIT(2) }, { 0x5c, BIT(3) }, { 0x48, BIT(4) },
};

/* configurePll fields (AppleT8152DPTXPort::configurePll), 21 per link rate. */
static const struct dptx_t8152_field pll_fields[] = {
	{ 0x6c0, GENMASK(10, 0) },  { 0x6c0, BIT(11) },        { 0x6c0, BIT(13) },
	{ 0x6c0, GENMASK(21, 18) }, { 0x6c4, GENMASK(27, 14) }, { 0x6c4, GENMASK(13, 0) },
	{ 0x6c8, GENMASK(15, 14) }, { 0x6c8, BIT(8) },          { 0x6c8, GENMASK(7, 0) },
	{ 0x6c8, GENMASK(13, 9) },  { 0x6c8, BIT(16) },         { 0x6c8, GENMASK(31, 26) },
	{ 0x6cc, GENMASK(6, 0) },   { 0x6cc, GENMASK(10, 9) },  { 0x6cc, GENMASK(14, 11) },
	{ 0x6cc, GENMASK(17, 15) }, { 0x6cc, BIT(20) },         { 0x6cc, BIT(21) },
	{ 0x6cc, GENMASK(23, 22) }, { 0x6cc, GENMASK(25, 24) }, { 0x6cc, GENMASK(27, 26) },
};

static const struct {
	u32 rate;
	u16 values[ARRAY_SIZE(pll_fields)];
} pll_configs[] = {
	{ 1620, { 0x21c, 1, 1, 3, 0, 0, 1, 0, 0, 0x13, 0, 0x1a, 0x3c, 1, 0, 0, 1, 0, 0, 1, 1 } },
	{ 2700, { 0x1c2, 1, 1, 3, 0x1fff, 0x3ffe, 1, 0, 0, 0x13, 0, 0x1a, 0x32, 1, 0, 0, 1, 0, 0, 1, 1 } },
	{ 5400, { 0x1c2, 1, 1, 3, 0x1fff, 0x3ffe, 0, 0, 0, 9, 0, 0x1a, 0x32, 0, 0, 1, 0, 0, 1, 1, 1 } },
	{ 8100, { 0x2a3, 1, 1, 3, 0x2ffd, 0x3ffc, 0, 0, 0, 9, 0, 0x1a, 0x5a, 0, 0, 1, 0, 0, 1, 1, 1 } },
};

static void set_field(void __iomem *base, u32 offset, u32 mask, u32 value)
{
	u32 old = readl(base + offset);

	writel((old & ~mask) | ((value << __ffs(mask)) & mask), base + offset);
}

static int calibrate(struct device *dev, void __iomem *port)
{
	u32 val;
	int ret;

	/* Bit 0 of 0x1c04 is the done flag. */
	ret = readl_poll_timeout(port + CMN_CAL_STATUS, val, val & CMN_CAL_STATUS_DONE,
				 CAL_POLL_US, CAL_TIMEOUT_US);
	if (ret) {
		dev_err(dev, "impedance calibration timed out\n");
		return ret;
	}

	set_field(port, CMN_CAL, CMN_CAL_MASK, CMN_CAL_DONE);
	set_field(port, CMN_CAL_RESULT, CMN_CAL_RESULT_VALUE,
		  FIELD_GET(CMN_CAL_READBACK_VALUE, readl(port + CMN_CAL_READBACK)));
	set_field(port, CMN_CAL_RESULT, CMN_CAL_RESULT_VALID, 1);

	return 0;
}

/* pllPowerUp / pllPowerDown: request, wait for done, drop the request; up needs lock. */
static int pll_power(struct device *dev, void __iomem *port, bool up)
{
	unsigned int polls = 0;
	bool done;

	set_field(port, PLL_CTRL, PLL_CTRL_SEQUENCE, 1);
	set_field(port, PLL_CTRL, PLL_CTRL_MODE, up ? PLL_MODE_UP : PLL_MODE_DOWN);
	set_field(port, PLL_CTRL, PLL_CTRL_REQUEST, 1);
	do
		udelay(PLL_POLL_US);
	while (!(readl(port + PLL_CTRL) & PLL_CTRL_DONE) && ++polls < PLL_POLLS);
	done = readl(port + PLL_CTRL) & PLL_CTRL_DONE;
	set_field(port, PLL_CTRL, PLL_CTRL_REQUEST, 0);
	set_field(port, PLL_CTRL, PLL_CTRL_SEQUENCE, 0);
	if (up)
		done = done && (readl(port + PLL_STATUS) & PLL_STATUS_LOCKED);
	if (!done) {
		dev_err(dev, "pll power-%s failed\n", up ? "up" : "down");
		return -ETIMEDOUT;
	}

	return 0;
}

/* Shadow register strobe that latches the PLL fields (block 0 + 0xa0). */
static void shadow_update(void __iomem *block0)
{
	static const struct {
		u32 mask;
		bool set;
	} steps[] = {
		{ BIT(3), true }, { BIT(2), true }, { BIT(18), true }, { BIT(18), false },
		{ BIT(19), true }, { BIT(19), false }, { BIT(2), false }, { BIT(3), false },
	};
	size_t i;

	for (i = 0; i < ARRAY_SIZE(steps); i++)
		set_field(block0, CORE_SHADOW, steps[i].mask, steps[i].set);
}

/*
 * AppleDPTXPort::handleActivate: enableDevicePower (source select, then the
 * pll, aux and phy group enables), handleSetTunables, configurePll,
 * activatePll, auxSetupCmnControl, activatePhy, one microsecond, then the
 * once-only impedance calibration. The DCP sets the lane count next.
 */
int dptx_t8152_activate(struct device *dev, void __iomem *port, void __iomem *block0,
			u32 dcp_index, struct apple_tunable *tunable)
{
	static const u8 groups[] = { 4, 8, 0 };
	size_t i;

	set_field(block0, CORE_SOURCE_SELECT, CORE_SOURCE_SELECT_MASK, dcp_index);

	for (i = 0; i < ARRAY_SIZE(groups); i++) {
		u32 shift = groups[i];

		set_field(block0, CORE_GROUP_ENABLE, 0x1 << shift, 1);
		set_field(block0, CORE_GROUP_ENABLE, 0x2 << shift, 1);
		set_field(block0, CORE_GROUP_ENABLE, 0x4 << shift, 0);
	}

	apple_tunable_apply(port, tunable);

	/* configurePll: the M6 dptx-phy node has no pclkdrv-select, so the bit is set. */
	set_field(port, PLL_PCLKDRV, PLL_PCLKDRV_BIT, 1);
	/* activatePll */
	set_field(port, PORT_CTRL, PORT_CTRL_PLL_ON, 1);
	/* auxSetupCmnControl */
	set_field(port, AUX_CMN_CTRL, AUX_CMN_CTRL_BIT, 0);
	/* activatePhy */
	set_field(port, CMN_CAL, CMN_CAL_MASK, CMN_CAL_ACTIVATE);
	set_field(port, PORT_CTRL, PORT_CTRL_PHY_ON, 1);
	udelay(1);

	return calibrate(dev, port);
}

/* phyPowerDown */
void dptx_t8152_deactivate(void __iomem *port)
{
	set_field(port, LANE_CTRL, LANE_CTRL_ENABLE, 0);
}

/* phySetActiveLaneCount: unused lanes get every per-lane "off" bit, used lanes clear them. */
int dptx_t8152_set_active_lanes(void __iomem *port, u32 lanes)
{
	u32 unused;
	u32 lane;
	size_t i;

	if (!lanes)
		return 0;
	if (lanes > DPTX_T8152_MAX_LANES)
		return -EINVAL;

	unused = ~GENMASK(lanes - 1, 0) & LANE_UNUSED_MASK_BITS;

	set_field(port, LANE_CTRL, LANE_CTRL_ENABLE, 0);
	for (lane = 0; lane < DPTX_T8152_MAX_LANES; lane++) {
		u32 origin = LANE0_OFFSET + lane * LANE_STRIDE;
		u32 off = (unused >> lane) & 1;

		for (i = 0; i < ARRAY_SIZE(lane_fields); i++)
			set_field(port, origin + lane_fields[i].offset, lane_fields[i].mask, off);
	}
	set_field(port, LANE_UNUSED_MASK, LANE_UNUSED_MASK_BITS, unused);
	set_field(port, LANE_CTRL, LANE_CTRL_ENABLE, 1);

	return 0;
}

/*
 * AppleDPTXPort::setLinkRate: phyPowerDown, pllPowerDown, configurePll fields,
 * shadow strobe, UHBR off, half-link bits, pllPowerUp. There is no calibration;
 * the next SetActiveLaneCount enables the lanes again.
 */
int dptx_t8152_set_link_rate(struct device *dev, void __iomem *port, void __iomem *block0,
			     u32 link_rate)
{
	const u16 *values = NULL;
	bool full = link_rate < 2500;
	size_t i;
	int ret;

	for (i = 0; i < ARRAY_SIZE(pll_configs); i++)
		if (pll_configs[i].rate == link_rate)
			values = pll_configs[i].values;
	if (!values)
		return -EINVAL;

	set_field(port, LANE_CTRL, LANE_CTRL_ENABLE, 0);
	ret = pll_power(dev, port, false);
	if (ret)
		return ret;
	for (i = 0; i < ARRAY_SIZE(pll_fields); i++)
		set_field(port, pll_fields[i].offset, pll_fields[i].mask, values[i]);
	shadow_update(block0);
	set_field(port, LANE_CTRL, LANE_CTRL_UHBR, 0);
	set_field(port, LANE_CTRL, LANE_CTRL_HALF_RATE, full);
	set_field(port, PORT_CTRL, PORT_CTRL_FULL_RATE, full);

	return pll_power(dev, port, true);
}
