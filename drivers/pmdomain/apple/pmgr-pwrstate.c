// SPDX-License-Identifier: GPL-2.0-only OR MIT
/*
 * Apple SoC PMGR device power state driver
 *
 * Copyright The Asahi Linux Contributors
 */

#include <linux/bitops.h>
#include <linux/bitfield.h>
#include <linux/err.h>
#include <linux/init.h>
#include <linux/io.h>
#include <linux/of.h>
#include <linux/of_address.h>
#include <linux/platform_device.h>
#include <linux/pm_domain.h>
#include <linux/regmap.h>
#include <linux/mfd/syscon.h>
#include <linux/reset-controller.h>
#include <linux/module.h>
#include <linux/suspend.h>

#define APPLE_PMGR_RESET        BIT(31)
#define APPLE_PMGR_AUTO_ENABLE  BIT(28)
#define APPLE_PMGR_PS_AUTO      GENMASK(27, 24)
#define APPLE_PMGR_PS_MIN       GENMASK(19, 16)
#define APPLE_PMGR_PS_RESET     BIT(12)
#define APPLE_PMGR_BUSY         BIT(11)
#define APPLE_PMGR_DEV_DISABLE  BIT(10)
#define APPLE_PMGR_WAS_CLKGATED BIT(9)
#define APPLE_PMGR_WAS_PWRGATED BIT(8)
#define APPLE_PMGR_PS_ACTUAL    GENMASK(7, 4)
#define APPLE_PMGR_PS_TARGET    GENMASK(3, 0)

#define APPLE_PMGR_FLAGS        (APPLE_PMGR_WAS_CLKGATED | APPLE_PMGR_WAS_PWRGATED)

#define APPLE_PMGR_PS_ACTIVE    0xf
#define APPLE_PMGR_PS_CLKGATE   0x4
#define APPLE_PMGR_PS_PWRGATE   0x0

#define APPLE_PMGR_PS_SET_TIMEOUT 100
#define APPLE_PMGR_RESET_TIME 1

struct apple_pmgr_ps {
	struct device *dev;
	struct generic_pm_domain genpd;
	struct reset_controller_dev rcdev;
	struct regmap *regmap;
	void __iomem *gfx_axi2af;
	u32 offset;
	u32 min_state;
	bool force_disable;
	bool force_reset;
	bool externally_clocked;
	bool neo_nvme_phy;
	bool neo_usb_leaf;
	u32 usb_sleep_saved;
};

/*
 * Diagnostics for newer Apple SoCs whose iBoot/firmware ownership contract is
 * not yet understood.  These leave every provider and its initial state
 * intact, but can suppress the otherwise unconditional probe-time AUTO_ENABLE
 * write on domains that firmware already left active.  Runtime genpd
 * transitions keep their existing behaviour.  Bits 0-3 select the four
 * T8140 display domains implicated by measured delayed faults, bit 4 selects
 * active Main PMGR domains outside the complete ANS/APCIe hierarchy, and bit
 * 5 selects active Nub PMGR domains.  Bits 6-11 select each member of the
 * T8140 ANS/APCIe hierarchy individually.  Bits 12-13 select the two observed
 * Nub SMC I2C domains individually.  Bit assignments are local to this
 * diagnostic and default to no behavioural change.
 */
static bool apple_pmgr_no_auto_enable_on_probe;
static unsigned int apple_pmgr_preserve_auto_probe_mask;
static bool apple_pmgr_trace_writes;
static unsigned int apple_pmgr_suppress_write_mask;
static bool retain_s2idle;
module_param(retain_s2idle, bool, 0644);
MODULE_PARM_DESC(retain_s2idle, "Keep T8140 domains powered during s2idle qualification");
static bool nvme_phy_s2idle;
module_param(nvme_phy_s2idle, bool, 0644);
MODULE_PARM_DESC(nvme_phy_s2idle, "Allow only J700's NVMe PHY domain to gate during retained s2idle qualification");

/* Clock-gate only the USB leaf after all genpd consumers have suspended. */
static bool usb_clock_s2idle, usb_clock_pending, usb_clock_failed;
static unsigned int usb_clock_offs, usb_clock_ons;
module_param(usb_clock_s2idle, bool, 0644);
module_param(usb_clock_pending, bool, 0444);
module_param(usb_clock_failed, bool, 0444);
module_param(usb_clock_offs, uint, 0444);
module_param(usb_clock_ons, uint, 0444);
MODULE_PARM_DESC(usb_clock_s2idle, "J700 USB leaf clock gating during retained s2idle qualification");

#define APPLE_PMGR_T8140_DISP_SYS       BIT(0)
#define APPLE_PMGR_T8140_DISPEXT0_SYS   BIT(1)
#define APPLE_PMGR_T8140_DISP_FE        BIT(2)
#define APPLE_PMGR_T8140_DISPEXT0_FE    BIT(3)
#define APPLE_PMGR_T8140_MAIN_NON_NVME  BIT(4)
#define APPLE_PMGR_T8140_NUB_ALL        BIT(5)
#define APPLE_PMGR_T8140_APCIE_GP       BIT(6)
#define APPLE_PMGR_T8140_ANS            BIT(7)
#define APPLE_PMGR_T8140_APCIE_SYS_GP   BIT(8)
#define APPLE_PMGR_T8140_APCIE_ST       BIT(9)
#define APPLE_PMGR_T8140_APCIE_SYS_ST   BIT(10)
#define APPLE_PMGR_T8140_APCIE_PHY_SW   BIT(11)
#define APPLE_PMGR_T8140_NUB_SMC_I2CM1  BIT(12)
#define APPLE_PMGR_T8140_NUB_SMC_I2CM2  BIT(13)
#define APPLE_PMGR_T8140_NVME_MASK      GENMASK(11, 6)
#define APPLE_PMGR_T8140_DIAGNOSTIC_MASK GENMASK(13, 0)

static int __init apple_pmgr_no_auto_probe_param(char *value)
{
	return kstrtobool(value, &apple_pmgr_no_auto_enable_on_probe);
}
early_param("apple_pmgr.no_auto_probe", apple_pmgr_no_auto_probe_param);

static int __init apple_pmgr_preserve_auto_probe_mask_param(char *value)
{
	unsigned int mask;
	int ret;

	ret = kstrtouint(value, 0, &mask);
	if (ret)
		return ret;
	if (mask & ~APPLE_PMGR_T8140_DIAGNOSTIC_MASK)
		return -EINVAL;

	apple_pmgr_preserve_auto_probe_mask = mask;
	return 0;
}
early_param("apple_pmgr.preserve_auto_probe_mask",
	    apple_pmgr_preserve_auto_probe_mask_param);

static int __init apple_pmgr_trace_writes_param(char *value)
{
	return kstrtobool(value, &apple_pmgr_trace_writes);
}
early_param("apple_pmgr.trace_writes", apple_pmgr_trace_writes_param);

static int __init apple_pmgr_suppress_write_mask_param(char *value)
{
	unsigned int mask;
	int ret;

	ret = kstrtouint(value, 0, &mask);
	if (ret)
		return ret;
	if (mask & ~APPLE_PMGR_T8140_DIAGNOSTIC_MASK)
		return -EINVAL;

	apple_pmgr_suppress_write_mask = mask;
	return 0;
}
early_param("apple_pmgr.suppress_write_mask",
	    apple_pmgr_suppress_write_mask_param);

#define genpd_to_apple_pmgr_ps(_genpd) container_of(_genpd, struct apple_pmgr_ps, genpd)
#define rcdev_to_apple_pmgr_ps(_rcdev) container_of(_rcdev, struct apple_pmgr_ps, rcdev)

static unsigned int apple_pmgr_t8140_diagnostic_bit(struct apple_pmgr_ps *ps)
{
	struct resource parent;

	if (!of_device_is_compatible(ps->dev->of_node,
				     "apple,t8140-pmgr-pwrstate"))
		return 0;

	switch (ps->offset) {
	case 0x280:
		return APPLE_PMGR_T8140_DISP_SYS;
	case 0x288:
		return APPLE_PMGR_T8140_DISPEXT0_SYS;
	case 0x318:
		return APPLE_PMGR_T8140_DISP_FE;
	case 0x320:
		return APPLE_PMGR_T8140_DISPEXT0_FE;
	case 0x258:
		return APPLE_PMGR_T8140_APCIE_GP;
	case 0x278:
		return APPLE_PMGR_T8140_ANS;
	case 0x2a0:
		return APPLE_PMGR_T8140_APCIE_SYS_GP;
	case 0x310:
		return APPLE_PMGR_T8140_APCIE_ST;
	case 0x3a0:
		return APPLE_PMGR_T8140_APCIE_SYS_ST;
	case 0x3c0:
		return APPLE_PMGR_T8140_APCIE_PHY_SW;
	default:
		break;
	}

	if (of_address_to_resource(ps->dev->of_node->parent, 0, &parent))
		return 0;
	if (parent.start == 0x308280000ULL && ps->offset == 0x8030)
		return APPLE_PMGR_T8140_NUB_ALL |
		       APPLE_PMGR_T8140_NUB_SMC_I2CM1;
	if (parent.start == 0x308280000ULL && ps->offset == 0x8038)
		return APPLE_PMGR_T8140_NUB_ALL |
		       APPLE_PMGR_T8140_NUB_SMC_I2CM2;
	if (parent.start == 0x308280000ULL)
		return APPLE_PMGR_T8140_NUB_ALL;
	if (parent.start == 0x300700000ULL)
		return APPLE_PMGR_T8140_MAIN_NON_NVME;

	return 0;
}

static bool apple_pmgr_is_t8140_gfx(struct apple_pmgr_ps *ps)
{
	struct resource parent;

	return ps->offset == 0x1f0 &&
	       of_device_is_compatible(ps->dev->of_node,
				       "apple,t8140-pmgr-pwrstate") &&
	       !of_address_to_resource(ps->dev->of_node->parent, 0, &parent) &&
	       parent.start == 0x300700000ULL;
}

/* The AXI2AF words are in the GPU's named SGX resource, not in PMGR. */
#define APPLE_T8140_SGX_BASE 0x480000000ULL
#define APPLE_T8140_AXI2AF_OFFSET 0x1000104
#define APPLE_T8140_AXI2AF_SIZE 8

static int apple_pmgr_map_gfx_axi2af(struct apple_pmgr_ps *ps)
{
	struct device_node *gpu, *domain;
	struct resource sgx;
	int index, ret;

	if (!apple_pmgr_is_t8140_gfx(ps))
		return 0;

	gpu = of_find_compatible_node(NULL, NULL, "apple,agx-t8140");
	if (!gpu)
		return dev_err_probe(ps->dev, -ENODEV, "missing T8140 GPU AXI2AF resource\n");

	domain = of_parse_phandle(gpu, "power-domains", 0);
	ret = domain == ps->dev->of_node ? 0 : -EINVAL;
	of_node_put(domain);
	if (ret)
		goto out;

	index = of_property_match_string(gpu, "reg-names", "sgx");
	if (index < 0) {
		ret = index;
		goto out;
	}
	ret = of_address_to_resource(gpu, index, &sgx);
	if (ret)
		goto out;
	if (sgx.start != APPLE_T8140_SGX_BASE ||
	    resource_size(&sgx) < APPLE_T8140_AXI2AF_OFFSET + APPLE_T8140_AXI2AF_SIZE) {
		ret = -EINVAL;
		goto out;
	}

	/*
	 * The GPU driver reserves the enclosing SGX resource. Map only these
	 * two words without a conflicting exclusive reservation: the GFX power
	 * provider owns their writes, and DRM only validates them after power-on.
	 * No register is accessed here while the domain may still be off.
	 */
	ps->gfx_axi2af = devm_ioremap(ps->dev, sgx.start + APPLE_T8140_AXI2AF_OFFSET,
				    APPLE_T8140_AXI2AF_SIZE);
	ret = ps->gfx_axi2af ? 0 : -ENOMEM;
out:
	of_node_put(gpu);
	if (ret)
		return dev_err_probe(ps->dev, ret, "invalid T8140 GPU AXI2AF resource\n");
	return 0;
}

enum apple_pmgr_gfx_axi_action {
	APPLE_PMGR_GFX_AXI_NONE,
	APPLE_PMGR_GFX_AXI_CLEAR_BEFORE,
	APPLE_PMGR_GFX_AXI_SET_AFTER,
};

static enum apple_pmgr_gfx_axi_action apple_pmgr_gfx_axi_action(u32 before, u32 requested)
{
	u32 old_target = FIELD_GET(APPLE_PMGR_PS_TARGET, before);
	u32 new_target = FIELD_GET(APPLE_PMGR_PS_TARGET, requested);
	bool old_disable = before & APPLE_PMGR_DEV_DISABLE;
	bool new_disable = requested & APPLE_PMGR_DEV_DISABLE;

	/* AppleT8140PMGR::_applyAxi2afWorkaround(index, requested, false, false). */
	if ((new_target == APPLE_PMGR_PS_ACTIVE && old_target != new_target) ||
	    (old_disable && !new_disable))
		return APPLE_PMGR_GFX_AXI_SET_AFTER;
	if ((new_target == APPLE_PMGR_PS_PWRGATE && old_target != new_target) ||
	    (!old_disable && new_disable))
		return APPLE_PMGR_GFX_AXI_CLEAR_BEFORE;
	return APPLE_PMGR_GFX_AXI_NONE;
}

static int apple_pmgr_apply_gfx_axi2af(struct apple_pmgr_ps *ps, bool enable)
{
	unsigned int offset;

	if (!ps->gfx_axi2af)
		return -ENODEV;

	/*
	 * Native clears bit 0 before OFF/DEV_DISABLE and sets it after ON has
	 * reached ACTIVE with BUSY clear. Both halves use this word order.
	 * Complete the accesses before subsequent PMGR writes and preserve
	 * every unrelated AXI bit. The caller owns the power-state preconditions.
	 */
	for (offset = 0; offset < APPLE_T8140_AXI2AF_SIZE; offset += sizeof(u32)) {
		u32 before = readl(ps->gfx_axi2af + offset);
		u32 requested = enable ? before | BIT(0) : before & ~BIT(0);
		u32 after;

		writel(requested, ps->gfx_axi2af + offset);
		after = readl(ps->gfx_axi2af + offset);
		dev_info(ps->dev,
			 "GFX AXI transition: offset=0x%x before=0x%08x requested=0x%08x after=0x%08x\n",
			 APPLE_T8140_AXI2AF_OFFSET + offset, before, requested, after);
		if (!!(after & BIT(0)) != enable)
			return -EIO;
	}
	return 0;
}

static void apple_pmgr_log_gfx_write(struct apple_pmgr_ps *ps,
				     const char *reason, u32 mask, u32 requested,
				     u32 before, int before_ret, int write_ret)
{
	u32 after = 0;
	int after_ret = regmap_read(ps->regmap, ps->offset, &after);

	/* Observation only: do not change requested values or operation results. */
	dev_info(ps->dev,
		 "GFX PMGR observe reason=%s offset=0x%x mask=0x%08x before=0x%08x requested=0x%08x after=0x%08x before_rc=%d write_rc=%d after_rc=%d\n",
		 reason, ps->offset, mask, before, requested, after,
		 before_ret, write_ret, after_ret);
}

static int apple_pmgr_finish_gfx_on(struct apple_pmgr_ps *ps)
{
	u32 cur;
	int ret;

	/* Native writeReg32 waits for both ACTUAL=ACTIVE and BUSY=0, in order. */
	ret = regmap_read_poll_timeout_atomic(ps->regmap, ps->offset, cur,
		FIELD_GET(APPLE_PMGR_PS_ACTUAL, cur) == APPLE_PMGR_PS_ACTIVE,
		1, APPLE_PMGR_PS_SET_TIMEOUT);
	if (ret)
		return ret;
	ret = regmap_read_poll_timeout_atomic(ps->regmap, ps->offset, cur,
		!(cur & APPLE_PMGR_BUSY), 1, APPLE_PMGR_PS_SET_TIMEOUT);
	if (ret)
		return ret;
	return apple_pmgr_apply_gfx_axi2af(ps, true);
}

static int apple_pmgr_gfx_write(struct apple_pmgr_ps *ps, u32 mask, u32 value,
				const char *reason)
{
	enum apple_pmgr_gfx_axi_action action;
	u32 before, requested;
	int ret;

	if (!ps->gfx_axi2af)
		return -ENODEV;
	ret = regmap_read(ps->regmap, ps->offset, &before);
	if (ret)
		return ret;
	requested = (before & ~mask) | (value & mask);
	action = apple_pmgr_gfx_axi_action(before, requested);
	if (action == APPLE_PMGR_GFX_AXI_CLEAR_BEFORE) {
		ret = apple_pmgr_apply_gfx_axi2af(ps, false);
		if (ret)
			return ret;
	}

	if (mask == ~0U)
		ret = regmap_write(ps->regmap, ps->offset, value);
	else
		ret = regmap_update_bits(ps->regmap, ps->offset, mask, value);
	apple_pmgr_log_gfx_write(ps, reason, mask, value, before, 0, ret);
	if (ret)
		return ret;
	if (action == APPLE_PMGR_GFX_AXI_SET_AFTER) {
		ret = apple_pmgr_finish_gfx_on(ps);
		if (ret)
			dev_err(ps->dev, "GFX AXI post-ON handshake failed: %d\n", ret);
	}
	return ret;
}

static int apple_pmgr_write(struct apple_pmgr_ps *ps, u32 value,
			    const char *reason)
{
	unsigned int bit = apple_pmgr_t8140_diagnostic_bit(ps);

	if (apple_pmgr_suppress_write_mask & bit) {
		dev_info(ps->dev,
			 "diagnostic: PMGR write suppressed reason=%s offset=0x%x value=0x%08x mask=0x%x\n",
			 reason, ps->offset, value,
			 apple_pmgr_suppress_write_mask);
		return 0;
	}

	if (apple_pmgr_trace_writes)
		dev_info(ps->dev,
			 "diagnostic: PMGR write reason=%s offset=0x%x value=0x%08x\n",
			 reason, ps->offset, value);

	if (apple_pmgr_is_t8140_gfx(ps))
		return apple_pmgr_gfx_write(ps, ~0U, value, reason);
	return regmap_write(ps->regmap, ps->offset, value);
}

static int apple_pmgr_update_bits(struct apple_pmgr_ps *ps, u32 mask,
				  u32 value, const char *reason)
{
	unsigned int bit = apple_pmgr_t8140_diagnostic_bit(ps);

	if (apple_pmgr_suppress_write_mask & bit) {
		dev_info(ps->dev,
			 "diagnostic: PMGR update suppressed reason=%s offset=0x%x mask=0x%08x value=0x%08x suppress_mask=0x%x\n",
			 reason, ps->offset, mask, value,
			 apple_pmgr_suppress_write_mask);
		return 0;
	}

	if (apple_pmgr_trace_writes)
		dev_info(ps->dev,
			 "diagnostic: PMGR update reason=%s offset=0x%x mask=0x%08x value=0x%08x\n",
			 reason, ps->offset, mask, value);

	if (apple_pmgr_is_t8140_gfx(ps))
		return apple_pmgr_gfx_write(ps, mask, value, reason);
	return regmap_update_bits(ps->regmap, ps->offset, mask, value);
}

static int apple_pmgr_ps_set(struct generic_pm_domain *genpd, u32 pstate, bool auto_enable)
{
	int ret;
	struct apple_pmgr_ps *ps = genpd_to_apple_pmgr_ps(genpd);
	u32 reg, cur;

	ret = regmap_read(ps->regmap, ps->offset, &reg);
	if (ret < 0)
		return ret;

	/* Resets are synchronous, and only work if the device is powered and clocked. */
	if (reg & APPLE_PMGR_RESET && pstate != APPLE_PMGR_PS_ACTIVE)
		dev_err(ps->dev, "PS %s: powering off with RESET active\n",
			genpd->name);

	if (pstate != APPLE_PMGR_PS_ACTIVE && (ps->force_disable || ps->force_reset)) {
		u32 reg_pre = reg & ~(APPLE_PMGR_AUTO_ENABLE | APPLE_PMGR_FLAGS |
				      APPLE_PMGR_BUSY);

		if (ps->force_disable)
			reg_pre |= APPLE_PMGR_DEV_DISABLE;
		if (ps->force_reset)
			reg_pre |= APPLE_PMGR_PS_RESET;

		ret = apple_pmgr_write(ps, reg_pre, "set-pre-disable-reset");
		if (ret && ps->gfx_axi2af)
			return ret;

		ret = regmap_read_poll_timeout_atomic(
			ps->regmap, ps->offset, cur,
			(cur & (APPLE_PMGR_DEV_DISABLE | APPLE_PMGR_PS_RESET)) ==
			(reg_pre & (APPLE_PMGR_DEV_DISABLE | APPLE_PMGR_PS_RESET)), 1,
			APPLE_PMGR_PS_SET_TIMEOUT);

		if (ret < 0)
			dev_err(ps->dev, "PS %s: Failed to set reset/disable bits (now: 0x%x)\n",
				genpd->name, reg);
	}

	/*
	 * BUSY is hardware-owned status, so never write it back. This is the
	 * only site that composes a whole word and writes all 32 bits; every
	 * other write here goes through regmap_update_bits() with an explicit
	 * mask and so cannot touch it. m1n1's pmgr_set_mode() is the reference
	 * and writes PS_TARGET alone, via mask32(), polling PS_ACTUAL for
	 * completion -- it puts no status bit back.
	 *
	 * Measured on J700 (T8140): a gfx power-down leaves 0x3007001f0 at
	 * 0x0f002b00, PS_TARGET=0 PS_ACTUAL=0 with BUSY set, and BUSY clears on
	 * its own a while later. An ON issued before then read that word back,
	 * re-wrote BUSY=1 with it (requested=0x0f00280f), and the register did
	 * not move -- PS_TARGET stayed 0, the post-ON handshake returned -110,
	 * and every later write returned -22 for the rest of the boot. The
	 * control is in the same boot: the identical power-down at t=0.8s also
	 * set BUSY, nothing wrote the register, BUSY cleared by itself, and the
	 * ON at t=16s succeeded.
	 *
	 * Whenever BUSY reads 0 -- which is every domain in the normal case --
	 * the composed word is bit-for-bit what it was before this change.
	 */
	reg &= ~(APPLE_PMGR_DEV_DISABLE | APPLE_PMGR_PS_RESET |
		 APPLE_PMGR_AUTO_ENABLE | APPLE_PMGR_FLAGS | APPLE_PMGR_BUSY |
		 APPLE_PMGR_PS_TARGET);
	reg |= FIELD_PREP(APPLE_PMGR_PS_TARGET, pstate);

	dev_dbg(ps->dev, "PS %s: pwrstate = 0x%x: 0x%x\n", genpd->name, pstate, reg);

	ret = apple_pmgr_write(ps, reg, "set-target");
	if (ret < 0 && ps->gfx_axi2af)
		return ret;

	if (ps->externally_clocked && pstate == APPLE_PMGR_PS_ACTIVE) {
		/*
		 * If this clock domain requires an external clock, then
		 * consider the "clock gated" state to be good enough.
		 */
		ret = regmap_read_poll_timeout_atomic(
			ps->regmap, ps->offset, cur,
			FIELD_GET(APPLE_PMGR_PS_ACTUAL, cur) >= APPLE_PMGR_PS_CLKGATE, 1,
			APPLE_PMGR_PS_SET_TIMEOUT);
	} else {
		ret = regmap_read_poll_timeout_atomic(
			ps->regmap, ps->offset, cur,
			FIELD_GET(APPLE_PMGR_PS_ACTUAL, cur) == pstate, 1,
			APPLE_PMGR_PS_SET_TIMEOUT);
	}

	if (ret < 0)
		dev_err(ps->dev, "PS %s: Failed to reach power state 0x%x (requested 0x%x, now: 0x%x)\n",
			genpd->name, pstate, reg, cur);

	if (auto_enable) {
		int write_ret;

		/* Not all devices implement this; this is a no-op where not implemented. */
		reg |= APPLE_PMGR_AUTO_ENABLE;
		write_ret = apple_pmgr_write(ps, reg, "set-auto-enable");

		if (write_ret && ps->gfx_axi2af)
			return write_ret;
	}

	return ret;
}

static bool apple_pmgr_ps_is_active(struct apple_pmgr_ps *ps)
{
	u32 reg = 0;
	int ret;

	ret = regmap_read(ps->regmap, ps->offset, &reg);
	if (apple_pmgr_is_t8140_gfx(ps))
		dev_info(ps->dev,
			 "GFX PMGR inherited offset=0x%x value=0x%08x read_rc=%d\n",
			 ps->offset, reg, ret);
	/*
	 * We consider domains as active if they are actually on, or if they have auto-PM
	 * enabled and the intended target is on.
	 */
	return (FIELD_GET(APPLE_PMGR_PS_ACTUAL, reg) == APPLE_PMGR_PS_ACTIVE ||
		(FIELD_GET(APPLE_PMGR_PS_TARGET, reg) == APPLE_PMGR_PS_ACTIVE &&
		 reg & APPLE_PMGR_AUTO_ENABLE));
}

static bool apple_pmgr_preserve_auto_on_probe(struct apple_pmgr_ps *ps)
{
	if (apple_pmgr_no_auto_enable_on_probe)
		return true;

	return apple_pmgr_preserve_auto_probe_mask &
	       apple_pmgr_t8140_diagnostic_bit(ps);
}

static void apple_pmgr_log_t8140_nvme_probe_state(struct apple_pmgr_ps *ps)
{
	struct resource parent;
	u32 reg;

	if (!(apple_pmgr_preserve_auto_probe_mask &
	      APPLE_PMGR_T8140_NVME_MASK) ||
	    !of_device_is_compatible(ps->dev->of_node,
				     "apple,t8140-pmgr-pwrstate") ||
	    of_address_to_resource(ps->dev->of_node->parent, 0, &parent) ||
	    parent.start != 0x300700000ULL)
		return;

	switch (ps->offset) {
	case 0x258:
	case 0x278:
	case 0x2a0:
	case 0x310:
	case 0x3a0:
	case 0x3c0:
		break;
	default:
		return;
	}

	if (regmap_read(ps->regmap, ps->offset, &reg))
		return;

	dev_info(ps->dev,
		 "diagnostic: inherited NVMe PMGR state 0x%08x actual=0x%x target=0x%x auto=%u was_clk=%u was_pwr=%u\n",
		 reg, (unsigned int)FIELD_GET(APPLE_PMGR_PS_ACTUAL, reg),
		 (unsigned int)FIELD_GET(APPLE_PMGR_PS_TARGET, reg),
		 !!(reg & APPLE_PMGR_AUTO_ENABLE),
		 !!(reg & APPLE_PMGR_WAS_CLKGATED),
		 !!(reg & APPLE_PMGR_WAS_PWRGATED));
}

static int apple_pmgr_usb_restore(struct apple_pmgr_ps *ps)
{
	u32 val;
	int ret;

	/* Reach ACTIVE before restoring the original automatic policy. */
	ret = regmap_update_bits(ps->regmap, ps->offset,
		APPLE_PMGR_PS_TARGET | APPLE_PMGR_AUTO_ENABLE | APPLE_PMGR_FLAGS,
		APPLE_PMGR_PS_ACTIVE);
	if (!ret)
		ret = regmap_read_poll_timeout_atomic(ps->regmap, ps->offset, val,
			FIELD_GET(APPLE_PMGR_PS_ACTUAL, val) == APPLE_PMGR_PS_ACTIVE,
			1, APPLE_PMGR_PS_SET_TIMEOUT);
	if (!ret)
		ret = regmap_update_bits(ps->regmap, ps->offset,
			APPLE_PMGR_AUTO_ENABLE | APPLE_PMGR_FLAGS,
			ps->usb_sleep_saved & APPLE_PMGR_AUTO_ENABLE);
	if (!ret)
		ret = regmap_read(ps->regmap, ps->offset, &val);
	if (!ret && ((val ^ ps->usb_sleep_saved) &
		     (APPLE_PMGR_PS_TARGET | APPLE_PMGR_AUTO_ENABLE)))
		ret = -EIO;
	if (ret)
		usb_clock_failed = true;
	else
		usb_clock_pending = false;
	dev_info(ps->dev, "J700 USB clock restore result=%d\n", ret);
	return ret;
}

static int apple_pmgr_usb_clock_off(struct apple_pmgr_ps *ps)
{
	u32 val;
	int ret;

	if (usb_clock_pending || usb_clock_failed)
		return -EBUSY;
	ret = regmap_read(ps->regmap, ps->offset, &val);
	if (ret)
		return ret;
	if ((val & (APPLE_PMGR_RESET | APPLE_PMGR_PS_RESET |
		    APPLE_PMGR_DEV_DISABLE | APPLE_PMGR_BUSY)) ||
	    FIELD_GET(APPLE_PMGR_PS_TARGET, val) != APPLE_PMGR_PS_ACTIVE ||
	    FIELD_GET(APPLE_PMGR_PS_ACTUAL, val) != APPLE_PMGR_PS_ACTIVE ||
	    FIELD_GET(APPLE_PMGR_PS_MIN, val) > APPLE_PMGR_PS_CLKGATE)
		return -EBUSY;
	ps->usb_sleep_saved = val;
	usb_clock_pending = true;
	ret = regmap_update_bits(ps->regmap, ps->offset,
		APPLE_PMGR_PS_TARGET | APPLE_PMGR_AUTO_ENABLE | APPLE_PMGR_FLAGS,
		APPLE_PMGR_PS_CLKGATE);
	if (!ret)
		ret = regmap_read_poll_timeout_atomic(ps->regmap, ps->offset, val,
			FIELD_GET(APPLE_PMGR_PS_ACTUAL, val) == APPLE_PMGR_PS_CLKGATE,
			1, APPLE_PMGR_PS_SET_TIMEOUT);
	dev_info(ps->dev, "J700 USB clock gate result=%d word=%#x\n", ret, val);
	if (ret) {
		/* genpd will not call power_on if power_off failed. */
		usb_clock_failed = true;
		apple_pmgr_usb_restore(ps);
	} else {
		usb_clock_offs++;
	}
	return ret;
}

static int apple_pmgr_ps_power_on(struct generic_pm_domain *genpd)
{
	struct apple_pmgr_ps *ps = genpd_to_apple_pmgr_ps(genpd);

	if (ps->neo_usb_leaf && usb_clock_pending) {
		int ret = apple_pmgr_usb_restore(ps);

		if (!ret)
			usb_clock_ons++;
		return ret;
	}

	return apple_pmgr_ps_set(genpd, APPLE_PMGR_PS_ACTIVE, true);
}

static int apple_pmgr_ps_power_off(struct generic_pm_domain *genpd)
{
	struct apple_pmgr_ps *ps = genpd_to_apple_pmgr_ps(genpd);
	int ret;

	if (READ_ONCE(retain_s2idle) && pm_suspend_target_state == PM_SUSPEND_TO_IDLE &&
	    of_device_is_compatible(ps->dev->of_node, "apple,t8140-pmgr-pwrstate")) {
		if (ps->neo_usb_leaf && READ_ONCE(usb_clock_s2idle))
			return apple_pmgr_usb_clock_off(ps);

		/*
		 * ANS already drains NVMe, shuts down RTKit and stops its CPU in
		 * the normal device suspend callback. Qualify only its PHY leaf;
		 * retain the parent domains and all unrelated device domains.
		 * genpd still requires every consumer to have suspended first.
		 */
		if (ps->neo_nvme_phy && READ_ONCE(nvme_phy_s2idle)) {
			ret = apple_pmgr_ps_set(genpd, APPLE_PMGR_PS_PWRGATE, false);
			dev_info(ps->dev, "J700 NVMe PHY sleep gate: result=%d min_state=%u\n",
				 ret, ps->min_state);
			return ret;
		}
		/* genpd keeps its state ON when the provider rejects power-off.
		 * Driver suspend callbacks still run; runtime PM outside system
		 * suspend retains its normal behaviour.
		 */
		dev_info(ps->dev, "Retaining %s power during s2idle qualification\n", genpd->name);
		return -EBUSY;
	}

	return apple_pmgr_ps_set(genpd, APPLE_PMGR_PS_PWRGATE, false);
}

static int apple_pmgr_reset_assert(struct reset_controller_dev *rcdev, unsigned long id)
{
	struct apple_pmgr_ps *ps = rcdev_to_apple_pmgr_ps(rcdev);
	unsigned long flags;
	int ret;

	spin_lock_irqsave(&ps->genpd.slock, flags);

	if (ps->genpd.status == GENPD_STATE_OFF)
		dev_err(ps->dev, "PS 0x%x: asserting RESET while powered down\n", ps->offset);

	dev_dbg(ps->dev, "PS 0x%x: assert reset\n", ps->offset);
	/* Quiesce device before asserting reset */
	ret = apple_pmgr_update_bits(ps, APPLE_PMGR_FLAGS | APPLE_PMGR_DEV_DISABLE,
				   APPLE_PMGR_DEV_DISABLE, "reset-assert-disable");
	if (ret && ps->gfx_axi2af)
		goto out;
	ret = apple_pmgr_update_bits(ps, APPLE_PMGR_FLAGS | APPLE_PMGR_RESET,
				   APPLE_PMGR_RESET, "reset-assert");

out:
	spin_unlock_irqrestore(&ps->genpd.slock, flags);

	return ps->gfx_axi2af ? ret : 0;
}

static int apple_pmgr_reset_deassert(struct reset_controller_dev *rcdev, unsigned long id)
{
	struct apple_pmgr_ps *ps = rcdev_to_apple_pmgr_ps(rcdev);
	unsigned long flags;
	int ret;

	spin_lock_irqsave(&ps->genpd.slock, flags);

	dev_dbg(ps->dev, "PS 0x%x: deassert reset\n", ps->offset);
	ret = apple_pmgr_update_bits(ps, APPLE_PMGR_FLAGS | APPLE_PMGR_RESET, 0,
				   "reset-deassert");
	if (ret && ps->gfx_axi2af)
		goto out;
	ret = apple_pmgr_update_bits(ps, APPLE_PMGR_FLAGS | APPLE_PMGR_DEV_DISABLE, 0,
				   "reset-deassert-enable");

	if (ps->genpd.status == GENPD_STATE_OFF)
		dev_err(ps->dev, "PS 0x%x: RESET was deasserted while powered down\n", ps->offset);

out:
	spin_unlock_irqrestore(&ps->genpd.slock, flags);

	return ps->gfx_axi2af ? ret : 0;
}

static int apple_pmgr_reset_reset(struct reset_controller_dev *rcdev, unsigned long id)
{
	int ret;

	ret = apple_pmgr_reset_assert(rcdev, id);
	if (ret)
		return ret;

	usleep_range(APPLE_PMGR_RESET_TIME, 2 * APPLE_PMGR_RESET_TIME);

	return apple_pmgr_reset_deassert(rcdev, id);
}

static int apple_pmgr_reset_status(struct reset_controller_dev *rcdev, unsigned long id)
{
	struct apple_pmgr_ps *ps = rcdev_to_apple_pmgr_ps(rcdev);
	u32 reg = 0;

	regmap_read(ps->regmap, ps->offset, &reg);

	return !!(reg & APPLE_PMGR_RESET);
}

static const struct reset_control_ops apple_pmgr_reset_ops = {
	.assert		= apple_pmgr_reset_assert,
	.deassert	= apple_pmgr_reset_deassert,
	.reset		= apple_pmgr_reset_reset,
	.status		= apple_pmgr_reset_status,
};

static int apple_pmgr_reset_xlate(struct reset_controller_dev *rcdev,
				  const struct of_phandle_args *reset_spec)
{
	return 0;
}

static int apple_pmgr_ps_probe(struct platform_device *pdev)
{
	struct device *dev = &pdev->dev;
	struct device_node *node = dev->of_node;
	struct apple_pmgr_ps *ps;
	struct regmap *regmap;
	struct of_phandle_iterator it;
	struct resource parent;
	int ret;
	const char *name;
	bool active;

	regmap = syscon_node_to_regmap(node->parent);
	if (IS_ERR(regmap))
		return PTR_ERR(regmap);

	ps = devm_kzalloc(dev, sizeof(*ps), GFP_KERNEL);
	if (!ps)
		return -ENOMEM;

	ps->dev = dev;
	ps->regmap = regmap;

	ret = of_property_read_string(node, "label", &name);
	if (ret < 0) {
		dev_err(dev, "missing label property\n");
		return ret;
	}

	ret = of_property_read_u32(node, "reg", &ps->offset);
	if (ret < 0) {
		dev_err(dev, "missing reg property\n");
		return ret;
	}
	ps->neo_nvme_phy = of_machine_is_compatible("apple,j700") &&
		of_device_is_compatible(node, "apple,t8140-pmgr-pwrstate") &&
		ps->offset == 0x3c0 &&
		!of_address_to_resource(node->parent, 0, &parent) &&
		parent.start == 0x300700000ULL;

	ps->neo_usb_leaf = of_machine_is_compatible("apple,j700") &&
		of_device_is_compatible(node, "apple,t8140-pmgr-pwrstate") &&
		ps->offset == 0x3f0 &&
		!of_address_to_resource(node->parent, 0, &parent) &&
		parent.start == 0x300700000ULL;

	ps->genpd.flags |= GENPD_FLAG_IRQ_SAFE;
	ps->genpd.name = name;
	ps->genpd.power_on = apple_pmgr_ps_power_on;
	ps->genpd.power_off = apple_pmgr_ps_power_off;

	ret = apple_pmgr_map_gfx_axi2af(ps);
	if (ret)
		return ret;

	ret = of_property_read_u32(node, "apple,min-state", &ps->min_state);
	if (ret == 0 && ps->min_state <= APPLE_PMGR_PS_ACTIVE)
		apple_pmgr_update_bits(ps, APPLE_PMGR_FLAGS | APPLE_PMGR_PS_MIN,
				       FIELD_PREP(APPLE_PMGR_PS_MIN, ps->min_state),
				       "probe-min-state");

	if (of_property_read_bool(node, "apple,force-disable"))
		ps->force_disable = true;

	if (of_property_read_bool(node, "apple,force-reset"))
		ps->force_reset = true;

	if (of_property_read_bool(node, "apple,externally-clocked"))
		ps->externally_clocked = true;

	active = apple_pmgr_ps_is_active(ps);
	apple_pmgr_log_t8140_nvme_probe_state(ps);
	if (of_property_read_bool(node, "apple,always-on")) {
		ps->genpd.flags |= GENPD_FLAG_ALWAYS_ON;
		if (!active) {
			dev_warn(dev, "always-on domain %s is not on at boot\n", name);
			/* Turn it on so pm_genpd_init does not fail */
			active = apple_pmgr_ps_power_on(&ps->genpd) == 0;
		}
	} else if (active) {
		ps->genpd.flags |= GENPD_FLAG_DEFER_OFF | GENPD_FLAG_ACTIVE_WAKEUP;
	}

	/* Turn on auto-PM if the domain is already on. */
	if (active && !apple_pmgr_preserve_auto_on_probe(ps)) {
		if (ps->gfx_axi2af) {
			u32 inherited;

			/*
			 * An inherited target=ACTIVE has no target transition for
			 * writeReg32 to detect. Native's explicit restore path also
			 * sets AXI2AF for these domains. Hold auto-PM disabled until
			 * that restore is complete, then publish this provider.
			 */
			ret = regmap_read(ps->regmap, ps->offset, &inherited);
			if (ret)
				return ret;
			ret = apple_pmgr_ps_set(&ps->genpd, APPLE_PMGR_PS_ACTIVE, false);
			if (ret)
				return ret;
			/* A target/enable transition already ran the post-write half. */
			if (FIELD_GET(APPLE_PMGR_PS_TARGET, inherited) == APPLE_PMGR_PS_ACTIVE &&
			    !(inherited & APPLE_PMGR_DEV_DISABLE)) {
				ret = apple_pmgr_finish_gfx_on(ps);
				if (ret)
					return ret;
			}
		}
		ret = apple_pmgr_update_bits(ps, APPLE_PMGR_FLAGS | APPLE_PMGR_AUTO_ENABLE,
					     APPLE_PMGR_AUTO_ENABLE, "probe-auto-enable");
		if (ret && ps->gfx_axi2af)
			return ret;
	} else if (active)
		dev_info(dev,
			 "diagnostic: preserving firmware AUTO_ENABLE state for %s (mask=0x%x)\n",
			 name, apple_pmgr_preserve_auto_probe_mask);

	ret = pm_genpd_init(&ps->genpd, NULL, !active);
	if (ret < 0) {
		dev_err(dev, "pm_genpd_init failed\n");
		return ret;
	}

	ret = of_genpd_add_provider_simple(node, &ps->genpd);
	if (ret < 0) {
		dev_err(dev, "of_genpd_add_provider_simple failed\n");
		return ret;
	}

	of_for_each_phandle(&it, ret, node, "power-domains", "#power-domain-cells", -1) {
		struct of_phandle_args parent, child;

		parent.np = it.node;
		parent.args_count = of_phandle_iterator_args(&it, parent.args, MAX_PHANDLE_ARGS);
		child.np = node;
		child.args_count = 0;
		ret = of_genpd_add_subdomain(&parent, &child);

		if (ret == -EPROBE_DEFER) {
			of_node_put(parent.np);
			goto err_remove;
		} else if (ret < 0) {
			dev_err(dev, "failed to add to parent domain: %d (%s -> %s)\n",
				ret, it.node->name, node->name);
			of_node_put(parent.np);
			goto err_remove;
		}
	}

	/*
	 * Do not participate in regular PM; parent power domains are handled via the
	 * genpd hierarchy.
	 */
	pm_genpd_remove_device(dev);

	ps->rcdev.owner = THIS_MODULE;
	ps->rcdev.nr_resets = 1;
	ps->rcdev.ops = &apple_pmgr_reset_ops;
	ps->rcdev.of_node = dev->of_node;
	ps->rcdev.of_reset_n_cells = 0;
	ps->rcdev.of_xlate = apple_pmgr_reset_xlate;

	ret = devm_reset_controller_register(dev, &ps->rcdev);
	if (ret < 0)
		goto err_remove;

	return 0;
err_remove:
	of_genpd_del_provider(node);
	pm_genpd_remove(&ps->genpd);
	return ret;
}

static const struct of_device_id apple_pmgr_ps_of_match[] = {
	{ .compatible = "apple,t8103-pmgr-pwrstate" },
	{ .compatible = "apple,pmgr-pwrstate" },
	{}
};

MODULE_DEVICE_TABLE(of, apple_pmgr_ps_of_match);

/*
 * genpd's provider sync_state powers off every domain firmware left active
 * that has no Linux consumer.  On J700 that sweep (at ~1 s) gated the sensor
 * rails (ispsens0/1) and dispext0_sys/fe, which also feed the 0x302800000
 * bandwidth-scratch block the ISP firmware writes at CH_START (SError from
 * the ISP, and from the AP on a read).  With apple_pmgr.no_auto_probe the
 * boot preserves the firmware power state instead; otherwise defer to genpd.
 */
static void apple_pmgr_ps_sync_state(struct device *dev)
{
	if (apple_pmgr_no_auto_enable_on_probe) {
		dev_info_once(dev, "diagnostic: keeping firmware-active domains on across sync_state\n");
		return;
	}
	of_genpd_sync_state(dev->of_node);
}

static struct platform_driver apple_pmgr_ps_driver = {
	.probe = apple_pmgr_ps_probe,
	.driver = {
		.name = "apple-pmgr-pwrstate",
		.of_match_table = apple_pmgr_ps_of_match,
		.sync_state = apple_pmgr_ps_sync_state,
	},
};

MODULE_AUTHOR("Hector Martin <marcan@marcan.st>");
MODULE_DESCRIPTION("PMGR power state driver for Apple SoCs");

module_platform_driver(apple_pmgr_ps_driver);
