// SPDX-License-Identifier: GPL-2.0-only
/* J700 stopped-SIO clock-idle qualification. No firmware boot, RUN write,
 * DMA mapping, reset or power-off. The two optional ASC masks come from
 * 25F84 SIO's asio-ascwrap-tunables. Correct physical base is derived from
 * arm-io ranges (0x210000000) plus sio reg (0x175e00000), independently
 * confirmed by native IODeviceMemory. A live SIO driver must own this policy
 * if/when one is added; this helper refuses any available DT owner.
 */
#include <linux/bitfield.h>
#include <linux/io.h>
#include <linux/iopoll.h>
#include <linux/mfd/syscon.h>
#include <linux/module.h>
#include <linux/of_address.h>
#include <linux/regmap.h>
#include <linux/workqueue.h>

#define ASC_BASE 0x385e00000ULL
#define PS 0x2a8
#define AUTO BIT(28)
#define FLOOR GENMASK(19, 16)
#define HISTORY GENMASK(9, 8)
#define UNSAFE (BIT(31) | BIT(12) | BIT(11) | BIT(10))
#define POLICY (AUTO | FLOOR | GENMASK(3, 0))
#define CFG_MASK 0xff00
#define IDLE_MASK 0x60000001

static unsigned int seconds, before, during, after, config_before, idle_before;
static bool tunables, force_clock, pending, failed;
static void __iomem *asc;
static struct regmap *map;
module_param(seconds, uint, 0444);
module_param(tunables, bool, 0444);
module_param(force_clock, bool, 0444);
module_param(before, uint, 0444);
module_param(during, uint, 0444);
module_param(after, uint, 0444);
module_param(config_before, uint, 0444);
module_param(idle_before, uint, 0444);
module_param(pending, bool, 0444);
module_param(failed, bool, 0444);
MODULE_PARM_DESC(seconds, "0 read-only; 1..600 automatic clock-idle trial with timed restore");
MODULE_PARM_DESC(tunables, "Apply only the two native SIO ASC masks while the CPU remains stopped");
MODULE_PARM_DESC(force_clock, "Request manual CLKGATE for the admitted stopped CPU; excludes tunables");

static void restore(struct work_struct *work)
{
	u32 value;
	int ret;

	if (!pending)
		return;
	ret = regmap_update_bits(map, PS, AUTO | HISTORY | GENMASK(3, 0), 15);
	if (!ret)
		ret = regmap_read_poll_timeout(map, PS, after,
			(after & 0xff) == 0xff && !(after & BIT(11)), 1, 10000);
	if (!ret && tunables) {
		/* Only access ASC after its clock domain is ACTIVE again. */
		value = readl(asc + 0x80c);
		writel((value & ~IDLE_MASK) | (idle_before & IDLE_MASK), asc + 0x80c);
		value = readl(asc + 0x40);
		writel((value & ~CFG_MASK) | (config_before & CFG_MASK), asc + 0x40);
		if (readl(asc + 0x44) || readl(asc + 0x40) != config_before ||
		    readl(asc + 0x80c) != idle_before)
			ret = -EIO;
	}
	if (!ret)
		ret = regmap_update_bits(map, PS, FLOOR | HISTORY, before & FLOOR);
	if (!ret)
		ret = regmap_read(map, PS, &after);
	if (!ret && (((after ^ before) & POLICY) || (after & UNSAFE)))
		ret = -EIO;
	failed |= !!ret;
	pending = !!ret;
	pr_info("neo-sio-idle: restore=%d before=%#x after=%#x\n", ret, before, after);
}
static DECLARE_DELAYED_WORK(restore_work, restore);

static int __init sio_trial_init(void)
{
	struct device_node *node;
	struct resource res;
	u32 status;
	int ret;

	if (!of_machine_is_compatible("apple,j700") || seconds > 600 || (force_clock && tunables))
		return -EINVAL;
	for_each_node_with_property(node, "reg") {
		if (of_device_is_available(node) && !of_address_to_resource(node, 0, &res) &&
		    res.start == ASC_BASE) {
			of_node_put(node);
			return -EBUSY;
		}
	}
	node = of_find_node_by_path("/soc/power-management@300700000");
	if (!node)
		return -ENODEV;
	map = syscon_node_to_regmap(node);
	of_node_put(node);
	if (IS_ERR(map))
		return PTR_ERR(map);
	ret = regmap_read(map, PS, &before);
	if (ret || before & (UNSAFE | AUTO | FLOOR) || (before & 0xff) != 0xff)
		return ret ?: -EBUSY;
	if (!request_mem_region(ASC_BASE, SZ_4K, "neo-sio-idle"))
		return -EBUSY;
	asc = ioremap(ASC_BASE, SZ_4K);
	if (!asc) {
		ret = -ENOMEM;
		goto release;
	}
	config_before = readl(asc + 0x40);
	idle_before = readl(asc + 0x80c);
	status = readl(asc + 0x48);
	/* Exact inherited configuration and stopped idle CPU; never adopt an
	 * arbitrary firmware epoch or assume that a RUN clear means safe DMA.
	 * This experiment only permits automatic clock gating, not power-off.
	 */
	if (readl(asc + 0x44) || config_before != 0xa0000 || idle_before || status != 0x6a) {
		ret = -EBUSY;
		goto unmap;
	}
	pr_info("neo-sio-idle: admission ps=%#x config=%#x idle=%#x status=%#x seconds=%u tunables=%u force_clock=%u\n",
		before, config_before, idle_before, status, seconds, tunables, force_clock);
	if (!seconds)
		return 0;
	pending = true;
	if (tunables) {
		writel((config_before & ~CFG_MASK) | 0x400, asc + 0x40);
		writel(idle_before | IDLE_MASK, asc + 0x80c);
		if (readl(asc + 0x40) != ((config_before & ~CFG_MASK) | 0x400) ||
		    readl(asc + 0x80c) != (idle_before | IDLE_MASK)) {
			ret = -EIO;
			goto rollback;
		}
	}
	if (force_clock) {
		ret = regmap_update_bits(map, PS, GENMASK(3, 0) | HISTORY, 4);
		if (!ret)
			ret = regmap_read_poll_timeout(map, PS, during,
				(during & 0xff) == 0x44 && !(during & BIT(11)), 1, 10000);
	} else {
		ret = regmap_update_bits(map, PS, FLOOR | HISTORY, FIELD_PREP(FLOOR, 4));
		if (!ret)
			ret = regmap_update_bits(map, PS, AUTO | HISTORY, AUTO);
		if (!ret)
			ret = regmap_read(map, PS, &during);
	}
	if (!ret && ((during & POLICY) != (force_clock ? 4 : (AUTO | FIELD_PREP(FLOOR, 4) | 15)) || (during & UNSAFE)))
		ret = -EIO;
	pr_info("neo-sio-idle: enable=%d during=%#x\n", ret, during);
	if (!ret) {
		schedule_delayed_work(&restore_work, msecs_to_jiffies(seconds * 1000));
		return 0;
	}
rollback:
	failed = true;
	restore(NULL);
	if (pending)
		return 0; /* Preserve visibility and permit unload recovery retry. */
unmap:
	iounmap(asc);
release:
	release_mem_region(ASC_BASE, SZ_4K);
	return ret;
}

static void __exit sio_trial_exit(void)
{
	cancel_delayed_work_sync(&restore_work);
	restore(NULL);
	iounmap(asc);
	release_mem_region(ASC_BASE, SZ_4K);
}
module_init(sio_trial_init);
module_exit(sio_trial_exit);
MODULE_LICENSE("GPL");
MODULE_DESCRIPTION("Bounded stopped-SIO automatic clock-idle qualification on J700");
