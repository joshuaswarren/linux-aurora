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
/*
 * A failed GFX power-on fails the GPU's mailbox resume, so give the domain
 * longer to come up than the 100 us the other domains get.
 */
#define APPLE_PMGR_GFX_ON_TIMEOUT 1000
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
};

#define genpd_to_apple_pmgr_ps(_genpd) container_of(_genpd, struct apple_pmgr_ps, genpd)
#define rcdev_to_apple_pmgr_ps(_rcdev) container_of(_rcdev, struct apple_pmgr_ps, rcdev)

/* Only T8140's GFX domain owns the two AXI2AF words in the SGX aperture. */
#define APPLE_T8140_SGX_BASE 0x480000000ULL
#define APPLE_T8140_AXI2AF_OFFSET 0x1000104
#define APPLE_T8140_AXI2AF_SIZE 8

static bool apple_pmgr_is_t8140_gfx(struct apple_pmgr_ps *ps)
{
	struct resource parent;

	return ps->offset == 0x1f0 &&
	       of_device_is_compatible(ps->dev->of_node,
				       "apple,t8140-pmgr-pwrstate") &&
	       !of_address_to_resource(ps->dev->of_node->parent, 0, &parent) &&
	       parent.start == 0x300700000ULL;
}

static int apple_pmgr_map_gfx_axi2af(struct apple_pmgr_ps *ps)
{
	struct device_node *gpu, *domain;
	struct resource sgx;
	int index, ret;

	if (!apple_pmgr_is_t8140_gfx(ps))
		return 0;

	gpu = of_find_compatible_node(NULL, NULL, "apple,agx-t8140");
	if (!gpu)
		return dev_err_probe(ps->dev, -ENODEV,
				     "missing T8140 GPU AXI2AF resource\n");

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

	/* The GPU driver reserves SGX; the provider maps only its two words. */
	ps->gfx_axi2af = devm_ioremap(ps->dev,
			sgx.start + APPLE_T8140_AXI2AF_OFFSET,
			APPLE_T8140_AXI2AF_SIZE);
	ret = ps->gfx_axi2af ? 0 : -ENOMEM;
out:
	of_node_put(gpu);
	if (ret)
		return dev_err_probe(ps->dev, ret,
				     "invalid T8140 GPU AXI2AF resource\n");
	return 0;
}

enum apple_pmgr_gfx_axi_action {
	APPLE_PMGR_GFX_AXI_NONE,
	APPLE_PMGR_GFX_AXI_CLEAR_BEFORE,
	APPLE_PMGR_GFX_AXI_SET_AFTER,
};

static enum apple_pmgr_gfx_axi_action apple_pmgr_gfx_axi_action(u32 before,
							 u32 requested)
{
	u32 old_target = FIELD_GET(APPLE_PMGR_PS_TARGET, before);
	u32 new_target = FIELD_GET(APPLE_PMGR_PS_TARGET, requested);
	bool old_disable = before & APPLE_PMGR_DEV_DISABLE;
	bool new_disable = requested & APPLE_PMGR_DEV_DISABLE;

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

static bool apple_pmgr_gfx_axi2af_enabled(struct apple_pmgr_ps *ps)
{
	unsigned int offset;

	for (offset = 0; offset < APPLE_T8140_AXI2AF_SIZE; offset += sizeof(u32))
		if (!(readl(ps->gfx_axi2af + offset) & BIT(0)))
			return false;
	return true;
}

static int apple_pmgr_finish_gfx_on(struct apple_pmgr_ps *ps)
{
	u32 cur;
	int ret;

	ret = regmap_read_poll_timeout_atomic(ps->regmap, ps->offset, cur,
		FIELD_GET(APPLE_PMGR_PS_ACTUAL, cur) == APPLE_PMGR_PS_ACTIVE,
		1, APPLE_PMGR_GFX_ON_TIMEOUT);
	if (!ret)
		ret = regmap_read_poll_timeout_atomic(ps->regmap, ps->offset, cur,
			!(cur & APPLE_PMGR_BUSY), 1, APPLE_PMGR_GFX_ON_TIMEOUT);
	if (ret) {
		dev_err_ratelimited(ps->dev,
			"PS %s: GFX power-on did not settle: %d (now: 0x%x)\n",
			ps->genpd.name, ret, cur);
		return ret;
	}
	return apple_pmgr_apply_gfx_axi2af(ps, true);
}

static int apple_pmgr_gfx_write(struct apple_pmgr_ps *ps, u32 mask, u32 value)
{
	enum apple_pmgr_gfx_axi_action action;
	u32 before, requested;
	int ret;

	ret = regmap_read(ps->regmap, ps->offset, &before);
	if (ret)
		return ret;
	requested = (before & ~mask) | (value & mask);
	action = apple_pmgr_gfx_axi_action(before, requested);
	/*
	 * A power-on that failed leaves the target at ACTIVE, but genpd still
	 * counts the domain as off and its next power-on writes the same
	 * target.  Finish the handshake then too.  The AXI2AF words are only
	 * read once the domain is on.
	 */
	if (action == APPLE_PMGR_GFX_AXI_NONE &&
	    FIELD_GET(APPLE_PMGR_PS_TARGET, requested) == APPLE_PMGR_PS_ACTIVE &&
	    !(requested & APPLE_PMGR_DEV_DISABLE) &&
	    (FIELD_GET(APPLE_PMGR_PS_ACTUAL, before) != APPLE_PMGR_PS_ACTIVE ||
	     !apple_pmgr_gfx_axi2af_enabled(ps)))
		action = APPLE_PMGR_GFX_AXI_SET_AFTER;
	if (action == APPLE_PMGR_GFX_AXI_CLEAR_BEFORE) {
		ret = apple_pmgr_apply_gfx_axi2af(ps, false);
		if (ret)
			return ret;
	}

	if (mask == ~0U)
		ret = regmap_write(ps->regmap, ps->offset, value);
	else
		ret = regmap_update_bits(ps->regmap, ps->offset, mask, value);
	if (ret)
		return ret;
	if (action == APPLE_PMGR_GFX_AXI_SET_AFTER)
		return apple_pmgr_finish_gfx_on(ps);
	return 0;
}

static int apple_pmgr_write(struct apple_pmgr_ps *ps, u32 value)
{
	if (ps->gfx_axi2af)
		return apple_pmgr_gfx_write(ps, ~0U, value);
	return regmap_write(ps->regmap, ps->offset, value);
}

static int apple_pmgr_update_bits(struct apple_pmgr_ps *ps, u32 mask, u32 value)
{
	if (ps->gfx_axi2af)
		return apple_pmgr_gfx_write(ps, mask, value);
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
		dev_err_ratelimited(ps->dev, "PS %s: powering off with RESET active\n",
			genpd->name);

	if (pstate != APPLE_PMGR_PS_ACTIVE && (ps->force_disable || ps->force_reset)) {
		u32 reg_pre = reg & ~(APPLE_PMGR_AUTO_ENABLE | APPLE_PMGR_FLAGS |
				      APPLE_PMGR_BUSY);

		if (ps->force_disable)
			reg_pre |= APPLE_PMGR_DEV_DISABLE;
		if (ps->force_reset)
			reg_pre |= APPLE_PMGR_PS_RESET;

		ret = apple_pmgr_write(ps, reg_pre);
		if (ret && ps->gfx_axi2af)
			return ret;

		ret = regmap_read_poll_timeout_atomic(
			ps->regmap, ps->offset, cur,
			(cur & (APPLE_PMGR_DEV_DISABLE | APPLE_PMGR_PS_RESET)) ==
			(reg_pre & (APPLE_PMGR_DEV_DISABLE | APPLE_PMGR_PS_RESET)), 1,
			APPLE_PMGR_PS_SET_TIMEOUT);

		if (ret < 0)
			dev_err_ratelimited(ps->dev, "PS %s: Failed to set reset/disable bits (now: 0x%x)\n",
				genpd->name, reg);
	}

	/* Do not replay the hardware-owned BUSY bit in this whole-word write. */
	reg &= ~(APPLE_PMGR_DEV_DISABLE | APPLE_PMGR_PS_RESET |
		 APPLE_PMGR_AUTO_ENABLE | APPLE_PMGR_FLAGS | APPLE_PMGR_BUSY |
		 APPLE_PMGR_PS_TARGET);
	reg |= FIELD_PREP(APPLE_PMGR_PS_TARGET, pstate);

	dev_dbg(ps->dev, "PS %s: pwrstate = 0x%x: 0x%x\n", genpd->name, pstate, reg);

	ret = apple_pmgr_write(ps, reg);
	if (ret && ps->gfx_axi2af)
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
		dev_err_ratelimited(ps->dev, "PS %s: Failed to reach power state 0x%x (requested 0x%x, now: 0x%x)\n",
			genpd->name, pstate, reg, cur);

	if (auto_enable) {
		int err;

		/* Not all devices implement this; this is a no-op where not implemented. */
		reg |= APPLE_PMGR_AUTO_ENABLE;
		err = apple_pmgr_write(ps, reg);
		if (err && ps->gfx_axi2af)
			return err;
	}

	/* A power state that was not reached fails the transition. */
	return ret;
}

static bool apple_pmgr_ps_is_active(struct apple_pmgr_ps *ps)
{
	u32 reg = 0;

	regmap_read(ps->regmap, ps->offset, &reg);
	/*
	 * We consider domains as active if they are actually on, or if they have auto-PM
	 * enabled and the intended target is on.
	 */
	return (FIELD_GET(APPLE_PMGR_PS_ACTUAL, reg) == APPLE_PMGR_PS_ACTIVE ||
		(FIELD_GET(APPLE_PMGR_PS_TARGET, reg) == APPLE_PMGR_PS_ACTIVE &&
		 reg & APPLE_PMGR_AUTO_ENABLE));
}

static int apple_pmgr_ps_power_on(struct generic_pm_domain *genpd)
{
	return apple_pmgr_ps_set(genpd, APPLE_PMGR_PS_ACTIVE, true);
}

static int apple_pmgr_ps_power_off(struct generic_pm_domain *genpd)
{
	return apple_pmgr_ps_set(genpd, APPLE_PMGR_PS_PWRGATE, false);
}

static int apple_pmgr_reset_assert(struct reset_controller_dev *rcdev, unsigned long id)
{
	struct apple_pmgr_ps *ps = rcdev_to_apple_pmgr_ps(rcdev);
	unsigned long flags;
	int ret;

	spin_lock_irqsave(&ps->genpd.slock, flags);

	if (ps->genpd.status == GENPD_STATE_OFF)
		dev_err_ratelimited(ps->dev,
			"PS 0x%x: asserting RESET while powered down\n", ps->offset);

	dev_dbg(ps->dev, "PS 0x%x: assert reset\n", ps->offset);
	/* Quiesce device before asserting reset */
	ret = apple_pmgr_update_bits(ps, APPLE_PMGR_FLAGS | APPLE_PMGR_DEV_DISABLE,
				     APPLE_PMGR_DEV_DISABLE);
	if (ret && ps->gfx_axi2af)
		goto out;
	ret = apple_pmgr_update_bits(ps, APPLE_PMGR_FLAGS | APPLE_PMGR_RESET,
				     APPLE_PMGR_RESET);

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
	ret = apple_pmgr_update_bits(ps, APPLE_PMGR_FLAGS | APPLE_PMGR_RESET, 0);
	if (ret && ps->gfx_axi2af)
		goto out;
	ret = apple_pmgr_update_bits(ps, APPLE_PMGR_FLAGS | APPLE_PMGR_DEV_DISABLE, 0);

	if (ps->genpd.status == GENPD_STATE_OFF)
		dev_err_ratelimited(ps->dev,
			"PS 0x%x: RESET was deasserted while powered down\n", ps->offset);

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

	ps->genpd.flags |= GENPD_FLAG_IRQ_SAFE;
	ps->genpd.name = name;
	ps->genpd.power_on = apple_pmgr_ps_power_on;
	ps->genpd.power_off = apple_pmgr_ps_power_off;
	ret = apple_pmgr_map_gfx_axi2af(ps);
	if (ret)
		return ret;

	ret = of_property_read_u32(node, "apple,min-state", &ps->min_state);
	if (ret == 0 && ps->min_state <= APPLE_PMGR_PS_ACTIVE)
		regmap_update_bits(regmap, ps->offset, APPLE_PMGR_FLAGS | APPLE_PMGR_PS_MIN,
				   FIELD_PREP(APPLE_PMGR_PS_MIN, ps->min_state));

	if (of_property_read_bool(node, "apple,force-disable"))
		ps->force_disable = true;

	if (of_property_read_bool(node, "apple,force-reset"))
		ps->force_reset = true;

	if (of_property_read_bool(node, "apple,externally-clocked"))
		ps->externally_clocked = true;

	active = apple_pmgr_ps_is_active(ps);
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

	/* Turn on auto-PM if the domain is already on */
	if (active) {
		if (ps->gfx_axi2af) {
			u32 inherited;

			ret = regmap_read(ps->regmap, ps->offset, &inherited);
			if (ret)
				return ret;
			ret = apple_pmgr_ps_set(&ps->genpd,
						 APPLE_PMGR_PS_ACTIVE, false);
			if (ret)
				return ret;
			if (FIELD_GET(APPLE_PMGR_PS_TARGET, inherited) == APPLE_PMGR_PS_ACTIVE &&
			    !(inherited & APPLE_PMGR_DEV_DISABLE)) {
				ret = apple_pmgr_finish_gfx_on(ps);
				if (ret)
					return ret;
			}
		}
		ret = apple_pmgr_update_bits(ps,
					     APPLE_PMGR_FLAGS | APPLE_PMGR_AUTO_ENABLE,
					     APPLE_PMGR_AUTO_ENABLE);
		if (ret && ps->gfx_axi2af)
			return ret;
	}

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

static struct platform_driver apple_pmgr_ps_driver = {
	.probe = apple_pmgr_ps_probe,
	.driver = {
		.name = "apple-pmgr-pwrstate",
		.of_match_table = apple_pmgr_ps_of_match,
	},
};

MODULE_AUTHOR("Hector Martin <marcan@marcan.st>");
MODULE_DESCRIPTION("PMGR power state driver for Apple SoCs");

module_platform_driver(apple_pmgr_ps_driver);
