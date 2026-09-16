// SPDX-License-Identifier: GPL-2.0-only
/* Bounded J700 automatic-idle trial. Native awake display-off
 * capture: 0x1000430f at Main PMGR+0x1b8; Linux inherits 0x0f0040ff.
 * Additional clock-only trials: SIO_CPU+0x2a8 and PMC+0x138. Native
 * AUTO/CLKGATE observations are 0x1400034f and 0x1400024f respectively.
 * Their Linux policies are manual ACTIVE, and live DT has no providers.
 * Keep a floor of 4: never allow these firmware domains to power off.
 * Never request a lower manual target. Let hardware arbitrate idleness.
 * This is qualification tooling, not a substitute for a genpd provider.
 */
#include <linux/bitfield.h>
#include <linux/mfd/syscon.h>
#include <linux/module.h>
#include <linux/of_address.h>
#include <linux/regmap.h>
#include <linux/workqueue.h>

static const struct {
	const char *name;
	u64 base;
	u32 offset;
} candidates[] = {
	{ "AFGGFXNI0", 0x300700000ULL, 0x1b8 },
	{ "SIO_CPU", 0x300700000ULL, 0x2a8 },
	{ "PMC", 0x300700000ULL, 0x138 },
	/* Native awake AUTO/CLKGATE. No Linux provider in the test DT. */
	{ "DEBUG_GATED", 0x308280000ULL, 0 },
	{ "NUB_SPMI0", 0x308280000ULL, 0x58 },
	{ "NUB_SPMI1", 0x308280000ULL, 0x60 },
};
#define AUTO BIT(28)
#define FLOOR GENMASK(19, 16)
#define HISTORY GENMASK(9, 8)
#define UNSAFE (BIT(31) | BIT(12) | BIT(11) | BIT(10))
#define POLICY (AUTO | FLOOR | GENMASK(3, 0))

static unsigned int seconds, min_state = 4, domain;
static u32 ps_offset;
static const char *domain_name;
static unsigned int before, during, after;
static bool pending, failed;
module_param(domain, uint, 0444);
module_param(seconds, uint, 0444);
module_param(min_state, uint, 0444);
module_param(before, uint, 0444);
module_param(during, uint, 0444);
module_param(after, uint, 0444);
module_param(pending, bool, 0444);
module_param(failed, bool, 0444);
MODULE_PARM_DESC(domain, "0 GPU fabric; 1 SIO CPU; 2 PMC; 3 DEBUG; 4 SPMI0; 5 SPMI1 (nonzero clock-only)");
MODULE_PARM_DESC(seconds, "0 read-only admission; 1..600 enable automatic idle then restore");
MODULE_PARM_DESC(min_state, "4 automatic clock gating only; 0 permits automatic power gating");
static struct regmap *map;

static void restore(struct work_struct *work)
{
	int ret;

	if (!pending)
		return;
	/* Disable AUTO at the unchanged ACTIVE target; wait for actual ACTIVE
	 * before restoring the original minimum floor. AUTO_STATE is read-only
	 * observation and is excluded from policy verification.
	 */
	ret = regmap_update_bits(map, ps_offset, AUTO | HISTORY, 0);
	if (!ret)
		ret = regmap_read_poll_timeout(map, ps_offset, after,
			(after & 0xff) == 0xff && !(after & BIT(11)), 1, 10000);
	if (!ret)
		ret = regmap_update_bits(map, ps_offset, FLOOR | HISTORY, before & FLOOR);
	if (!ret)
		ret = regmap_read(map, ps_offset, &after);
	if (!ret && (((after ^ before) & POLICY) || (after & UNSAFE)))
		ret = -EIO;
	failed |= !!ret;
	pending = !!ret;
	pr_info("neo-auto-idle: %s restore=%d before=%#x after=%#x\n",
		domain_name, ret, before, after);
}
static DECLARE_DELAYED_WORK(restore_work, restore);

static int __init trial_init(void)
{
	struct device_node *node, *child;
	struct resource res;
	u32 offset, gfx = 0;
	int ret;

	if (!of_machine_is_compatible("apple,j700") || seconds > 600 ||
	    (min_state != 0 && min_state != 4) || domain >= ARRAY_SIZE(candidates) ||
	    (domain && min_state != 4))
		return -EINVAL;
	ps_offset = candidates[domain].offset;
	domain_name = candidates[domain].name;
	node = of_find_node_by_path(domain < 3 ?
		"/soc/power-management@300700000" :
		"/soc/power-management@308280000");
	if (!node)
		return -ENODEV;
	ret = of_address_to_resource(node, 0, &res);
	if (ret || res.start != candidates[domain].base ||
	    resource_size(&res) < max_t(u32, ps_offset + 4, domain ? 0 : 0x1f4)) {
		of_node_put(node);
		return -ENODEV;
	}
	for_each_available_child_of_node(node, child) {
		if (!of_property_read_u32(child, "reg", &offset) && offset == ps_offset) {
			of_node_put(child);
			of_node_put(node);
			return -EBUSY;
		}
	}
	map = syscon_node_to_regmap(node);
	of_node_put(node);
	if (IS_ERR(map))
		return PTR_ERR(map);
	ret = regmap_read(map, ps_offset, &before);
	if (!ret && !domain)
		ret = regmap_read(map, 0x1f0, &gfx);
	if (ret)
		return ret;
	if (before & (UNSAFE | AUTO) || (before & 0xff) != 0xff ||
	    FIELD_GET(FLOOR, before) ||
	    (!domain && ((gfx & UNSAFE) || !(gfx & AUTO) || (gfx & 0xff) != 0x0f)))
		return -EBUSY;
	pr_info("neo-auto-idle: %s offset=%#x word=%#x seconds=%u floor=%u\n",
		domain_name, ps_offset, before, seconds, min_state);
	if (!seconds)
		return 0;
	pending = true;
	ret = regmap_update_bits(map, ps_offset, FLOOR | HISTORY,
				 FIELD_PREP(FLOOR, min_state));
	if (!ret)
		ret = regmap_update_bits(map, ps_offset, AUTO | HISTORY, AUTO);
	if (!ret)
		ret = regmap_read(map, ps_offset, &during);
	if (!ret && ((during & POLICY) != (AUTO | FIELD_PREP(FLOOR, min_state) | 15) ||
		     (during & UNSAFE)))
		ret = -EIO;
	pr_info("neo-auto-idle: %s enable=%d word=%#x\n", domain_name, ret, during);
	if (ret) {
		failed = true;
		restore(NULL);
		/* Keep failed restoration visible for recovery and another attempt. */
		return pending ? 0 : ret;
	}
	schedule_delayed_work(&restore_work, msecs_to_jiffies(seconds * 1000));
	return 0;
}

static void __exit trial_exit(void)
{
	cancel_delayed_work_sync(&restore_work);
	restore(NULL);
}
module_init(trial_init);
module_exit(trial_exit);
MODULE_LICENSE("GPL");
MODULE_DESCRIPTION("Bounded J700 automatic-idle qualification");
