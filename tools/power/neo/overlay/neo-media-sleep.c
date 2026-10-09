// SPDX-License-Identifier: GPL-2.0-only
/* J700 PRORES/MSR sleep-only qualification. These two physical leaf domains
 * passed an awake off/on trial with exact restoration. No DT consumer or
 * firmware quiescence is inferred for other media blocks. Default disabled;
 * do not combine with the userspace gate helper on the same domains.
 */
#include <linux/console.h>
#include <linux/mfd/syscon.h>
#include <linux/module.h>
#include <linux/of_address.h>
#include <linux/platform_device.h>
#include <linux/regmap.h>
#include <linux/suspend.h>

#define AUTO BIT(28)
#define STATUS (GENMASK(7, 4) | GENMASK(9, 8) | BIT(11))
#define UNSAFE (BIT(31) | BIT(12) | BIT(11) | BIT(10))
static const u32 offsets[] = { 0x250, 0x238 }; /* PRORES, MSR; parent AFISOCNI0 */
static bool enabled, failed;
static unsigned int pending, suspends, resumes;
module_param(enabled, bool, 0644);
module_param(failed, bool, 0444);
module_param(pending, uint, 0444);
module_param(suspends, uint, 0444);
module_param(resumes, uint, 0444);
static struct regmap *map;
static struct platform_device *pdev;
static u32 saved[ARRAY_SIZE(offsets)];

static int target(unsigned int i, u32 value)
{
	u32 now;
	int ret;

	ret = regmap_update_bits(map, offsets[i], AUTO | GENMASK(9, 8) | BIT(11) | 0xf, value);
	if (ret)
		return ret;
	return regmap_read_poll_timeout_atomic(map, offsets[i], now,
		((now >> 4) & 0xf) == value && !(now & BIT(11)), 1, 1000);
}

static int restore(struct device *dev)
{
	u32 now;
	int i, ret;

	if (!pending)
		return 0;
	for (i = ARRAY_SIZE(offsets) - 1; i >= 0; i--) {
		if (!(pending & BIT(i)))
			continue;
		ret = target(i, 0xf);
		if (ret)
			goto fail;
		ret = regmap_update_bits(map, offsets[i], AUTO, saved[i] & AUTO);
		if (ret)
			goto fail;
		ret = regmap_read(map, offsets[i], &now);
		if (ret)
			goto fail;
		if ((now & ~STATUS) != (saved[i] & ~STATUS) ||
		    (!(saved[i] & AUTO) && ((now >> 4) & 0xf) != 0xf)) {
			ret = -EIO;
			goto fail;
		}
		pending &= ~BIT(i);
		dev_info(dev, "media restored offset=%#x value=%#x\n", offsets[i], now);
	}
	resumes++;
	return 0;
fail:
	failed = true;
	dev_err(dev, "media restore failed offset=%#x error=%d pending=%#x\n", offsets[i], ret, pending);
	return ret;
}

static int suspend_noirq(struct device *dev)
{
	unsigned int i;
	int ret;

	if (!READ_ONCE(enabled))
		return 0;
	if (failed || pending || !READ_ONCE(console_suspend_enabled) ||
	    pm_suspend_target_state != PM_SUSPEND_TO_IDLE)
		return -EBUSY;
	/* Leave AFISOCNI0 and every other parent unchanged. Its ADT descriptor
	 * is virtual: do not infer physical parent state from its offset field.
	 */
	for (i = 0; i < ARRAY_SIZE(offsets); i++) {
		ret = regmap_read(map, offsets[i], &saved[i]);
		if (ret)
			return ret;
		if (saved[i] & UNSAFE || (saved[i] & 0xff) != 0xff)
			return -EBUSY;
	}
	for (i = 0; i < ARRAY_SIZE(offsets); i++) {
		pending |= BIT(i); /* A failed write may still have changed hardware. */
		ret = target(i, 0);
		if (ret) {
			failed = true;
			restore(dev);
			return ret;
		}
		dev_info(dev, "media gated offset=%#x\n", offsets[i]);
	}
	suspends++;
	return 0;
}

static int probe(struct platform_device *p)
{
	struct device_node *node, *child;
	struct resource res;
	u32 off;
	int ret;

	node = of_find_node_by_path("/soc/power-management@300700000");
	if (!node)
		return -ENODEV;
	ret = of_address_to_resource(node, 0, &res);
	if (ret || res.start != 0x300700000ULL || resource_size(&res) < 0x254) {
		of_node_put(node);
		return -ENODEV;
	}
	for_each_available_child_of_node(node, child) {
		if (!of_property_read_u32(child, "reg", &off) &&
		    (off == offsets[0] || off == offsets[1])) {
			of_node_put(child);
			of_node_put(node);
			return -EBUSY; /* Do not bypass a DT-described provider. */
		}
	}
	map = syscon_node_to_regmap(node);
	of_node_put(node);
	return PTR_ERR_OR_ZERO(map);
}

static void remove_device(struct platform_device *p) { restore(&p->dev); }
static const struct dev_pm_ops pm = {
	.suspend_noirq = suspend_noirq,
	.resume_noirq = restore,
};
static struct platform_driver driver = {
	.probe = probe, .remove = remove_device,
	.driver = { .name = "neo-media-sleep", .pm = &pm },
};
static int __init media_init(void)
{
	int ret;

	if (!of_machine_is_compatible("apple,j700"))
		return -ENODEV;
	ret = platform_driver_register(&driver);
	if (ret)
		return ret;
	pdev = platform_device_register_simple("neo-media-sleep", -1, NULL, 0);
	if (IS_ERR(pdev)) {
		platform_driver_unregister(&driver);
		return PTR_ERR(pdev);
	}
	if (!map || IS_ERR(map)) {
		platform_device_unregister(pdev);
		platform_driver_unregister(&driver);
		return -ENODEV;
	}
	return 0;
}
static void __exit media_exit(void)
{
	platform_device_unregister(pdev);
	platform_driver_unregister(&driver);
}
module_init(media_init);
module_exit(media_exit);
MODULE_LICENSE("GPL");
MODULE_DESCRIPTION("J700 PRORES/MSR suspend-only gate and verified restore qualification");
