// SPDX-License-Identifier: GPL-2.0-only
/* Manual J700 qualification module, using the inherited framebuffer.
 * X3200: pmBL controls backlight; gP16 controls LCD enable.
 * Keep the SMC parent alive until both suspend operations finish.
 * See ../display-power.md for firmware evidence and measurement boundaries.
 */
#include <linux/delay.h>
#include <linux/console.h>
#include <linux/gpio/consumer.h>
#include <linux/gpio/machine.h>
#include <linux/ktime.h>
#include <linux/mfd/macsmc.h>
#include <linux/mfd/syscon.h>
#include <linux/io.h>
#include <linux/iopoll.h>
#include <linux/module.h>
#include <linux/of.h>
#include <linux/of_address.h>
#include <linux/platform_device.h>
#include <linux/pm.h>
#include <linux/regmap.h>

static bool experimental;
module_param(experimental, bool, 0444);
static int state = -1;
module_param(state, int, 0444);
static unsigned int off_count, on_count;
module_param(off_count, uint, 0444);
module_param(on_count, uint, 0444);
MODULE_PARM_DESC(state, "-1 unbound; 0 LCD low/backlight-off acknowledged; 1 restored; other negative errno");
static unsigned int clock_depth, clock_off_count, clock_on_count;
static bool pause_cpu;
module_param(pause_cpu, bool, 0644);
MODULE_PARM_DESC(pause_cpu, "Power-isolation trial: pause an unowned, hardware-idle DCP CPU without a firmware sleep request");
module_param(clock_depth, uint, 0644);
module_param(clock_off_count, uint, 0444);
module_param(clock_on_count, uint, 0444);
MODULE_PARM_DESC(clock_depth, "Manual clock-retention trial: 0 off, 1 display CPU, 2 plus FE, 3 plus SYS; requires stopped DCP");
static const unsigned int clock_offsets[] = { 0x10000, 0x318, 0x280 };
#define CLOCK_TARGET GENMASK(3, 0)
#define CLOCK_ACTUAL GENMASK(7, 4)
#define CLOCK_AUTO BIT(28)
#define CLOCK_STATUS (BIT(8) | BIT(9) | BIT(11))

struct neo_display {
	struct apple_smc *smc;
	struct gpio_desc *lcd;
	ktime_t off_time;
	bool touched;
	struct regmap *pmgr;
	void __iomem *coproc;
	u32 clocks[3];
	unsigned int clocks_touched;
	bool cpu_paused;
	u32 saved_cpu_control;
};
static struct platform_device *display_device;
static struct gpiod_lookup_table display_gpio = {
	.dev_id = "neo-display-sleep",
	.table = {
		GPIO_LOOKUP("macsmc-pmu-gpio", 22, "lcd-enable", GPIO_ACTIVE_HIGH),
		{ }
	},
};

static int backlight(struct neo_display *d, u32 on)
{
	return apple_smc_write(d->smc, SMC_KEY(pmBL), &on, sizeof(on));
}

/* This deliberately never requests power state 0: scanout/DART context must
 * remain intact. Only the always-accessible PMGR is touched while clocked off.
 * Native/Linux PMGR definitions identify retained clock state 4. Whether the
 * stopped J700 display pipeline admits it is the experiment, not an assumption.
 */
static int clock_set(struct neo_display *d, unsigned int index, u32 target, bool restore)
{
	u32 value, after;
	int ret = regmap_read(d->pmgr, clock_offsets[index], &value);

	if (ret)
		return ret;
	value &= ~(CLOCK_TARGET | CLOCK_AUTO | CLOCK_STATUS);
	value |= target;
	ret = regmap_write(d->pmgr, clock_offsets[index], value);
	if (!ret)
		ret = regmap_read_poll_timeout(d->pmgr, clock_offsets[index], after,
			((after & CLOCK_ACTUAL) >> 4) == target && !(after & BIT(11)), 10, 10000);
	if (!ret && restore && (d->clocks[index] & CLOCK_AUTO)) {
		after &= ~CLOCK_STATUS;
		ret = regmap_write(d->pmgr, clock_offsets[index], after | CLOCK_AUTO);
	}
	pr_info("neo-display clock: offset=%#x target=%u result=%d\n", clock_offsets[index], target, ret);
	return ret;
}

static int clocks_on(struct device *dev)
{
	struct neo_display *d = dev_get_drvdata(dev);
	bool touched = d->clocks_touched;
	int ret;

	while (d->clocks_touched) {
		unsigned int i = d->clocks_touched - 1;

		ret = clock_set(d, i, d->clocks[i] & CLOCK_TARGET, true);
		if (ret)
			return ret;
		d->clocks_touched--;
	}
	if (d->cpu_paused) {
		writel(d->saved_cpu_control, d->coproc + 0x44);
		if (readl(d->coproc + 0x44) != d->saved_cpu_control)
			return -EIO;
		d->cpu_paused = false;
		dev_info(dev, "DCP isolation pause ended; RUN restored (no firmware sleep claim)\n");
	}
	if (touched)
		clock_on_count++;
	return 0;
}

static int clocks_off(struct device *dev)
{
	static const char * const owners[] = {
		"412e00000.dcp", "412e08000.mailbox", "412300000.iommu",
		"412320000.iommu", "412340000.iommu",
	};
	struct neo_display *d = dev_get_drvdata(dev);
	unsigned int depth = READ_ONCE(clock_depth), i;
	u32 control;
	int ret;

	if (!depth)
		return 0;
	/* The inherited framebuffer and dockchannel console are not owned by
	 * this diagnostic. Never leave console writes live across clock gating.
	 * Enable /sys/module/printk/parameters/console_suspend before the test.
	 */
	if (!READ_ONCE(console_suspend_enabled)) {
		dev_err(dev, "display clock trial requires console suspension\n");
		return -EBUSY;
	}
	if (depth > ARRAY_SIZE(clock_offsets) || !d->touched || READ_ONCE(state) != 0)
		return -EINVAL;
	for (i = 0; i < ARRAY_SIZE(owners); i++) {
		struct device *owner = bus_find_device_by_name(&platform_bus_type, NULL, owners[i]);
		bool bound = owner && owner->driver;

		put_device(owner);
		if (bound)
			return -EBUSY;
	}
	/* Validate the complete inherited hierarchy before reading the ASC.
	 * The three actual states must be ACTIVE; no speculative gated reads.
	 */
	for (i = 0; i < ARRAY_SIZE(clock_offsets); i++) {
		ret = regmap_read(d->pmgr, clock_offsets[i], &d->clocks[i]);
		if (ret)
			return ret;
		if ((d->clocks[i] & 0xff) != 0xff ||
		    (d->clocks[i] & (BIT(31) | BIT(12) | BIT(11) | BIT(10))))
			return -EBUSY;
	}
	control = readl(d->coproc + 0x44);
	dev_info(dev, "display clock trial depth=%u DCP CPU_CONTROL=%#x\n", depth, control);
	if (control & BIT(4)) {
		u32 idle;

		if (!READ_ONCE(pause_cpu))
			return -EBUSY;
		/* Power characterization only: no firmware sleep assertion. The
		 * entire context stays powered, mappings stay untouched, all AP
		 * clients are frozen and no Linux DCP/mailbox/DART owner exists.
		 * Native ASC idle must be observed before pausing execution.
		 */
		ret = readl_poll_timeout(d->coproc + 0x48, idle, idle & 3, 10, 10000);
		if (ret)
			return ret;
		d->saved_cpu_control = control;
		d->cpu_paused = true;
		writel(control & ~BIT(4), d->coproc + 0x44);
		control = readl(d->coproc + 0x44);
		writel(control & ~BIT(5), d->coproc + 0x44);
		control = readl(d->coproc + 0x44);
		dev_info(dev, "DCP isolation pause: idle=%#x control=%#x\n", idle, control);
		if (control & (BIT(4) | BIT(5))) {
			clocks_on(dev);
			return -EIO;
		}
	}
	for (i = 0; i < depth; i++) {
		d->clocks_touched = i + 1;
		ret = clock_set(d, i, 4, false);
		if (ret) {
			clocks_on(dev);
			return ret;
		}
	}
	clock_off_count++;
	return 0;
}

static int display_on(struct device *dev)
{
	struct neo_display *d = dev_get_drvdata(dev);
	s64 elapsed;
	int ret = 0, bl_ret;

	if (!d->touched)
		return 0;
	/* A failed clock restore must never illuminate a dead scanout engine. */
	ret = clocks_on(dev);
	if (ret) {
		WRITE_ONCE(state, ret);
		return ret;
	}
	/* Firmware requires 260 ms between LCD off and on, including aborts. */
	elapsed = ktime_ms_delta(ktime_get_boottime(), d->off_time);
	if (elapsed < 300)
		msleep(300 - elapsed);
	gpiod_set_value_cansleep(d->lcd, 1);
	if (gpiod_get_value_cansleep(d->lcd) != 1)
		ret = -EIO;
	/* Conservative settling delay before illuminating the restored panel. */
	msleep(100);
	bl_ret = backlight(d, 1);
	if (bl_ret < 0 && !ret)
		ret = bl_ret;
	WRITE_ONCE(state, ret ? ret : 1);
	if (!ret) {
		d->touched = false;
		on_count++;
	}
	dev_info(dev, "LCD/backlight restore result=%d\n", ret);
	return ret;
}

static int display_off(struct device *dev)
{
	struct neo_display *d = dev_get_drvdata(dev);
	int ret;

	if (gpiod_get_value_cansleep(d->lcd) != 1)
		return -EINVAL;
	d->touched = true;
	d->off_time = ktime_get_boottime();
	ret = backlight(d, 0);
	if (ret < 0)
		goto restore;
	gpiod_set_value_cansleep(d->lcd, 0);
	d->off_time = ktime_get_boottime();
	if (gpiod_get_value_cansleep(d->lcd) != 0) {
		ret = -EIO;
		goto restore;
	}
	WRITE_ONCE(state, 0);
	off_count++;
	dev_info(dev, "backlight off acknowledged, LCD-enable low\n");
	return 0;
restore:
	display_on(dev);
	return ret;
}

static int display_probe(struct platform_device *pdev)
{
	struct apple_smc_key_info info;
	struct neo_display *d;
	int ret;
	struct device_node *node;
	struct resource res;

	d = devm_kzalloc(&pdev->dev, sizeof(*d), GFP_KERNEL);
	if (!d)
		return -ENOMEM;
	node = of_find_node_by_path("/soc/power-management@300700000");
	if (!node)
		return -ENODEV;
	ret = of_address_to_resource(node, 0, &res);
	if (!ret && (res.start != 0x300700000ULL || resource_size(&res) < 0x10004))
		ret = -EINVAL;
	d->pmgr = ret ? ERR_PTR(ret) : syscon_node_to_regmap(node);
	of_node_put(node);
	if (IS_ERR(d->pmgr))
		return PTR_ERR(d->pmgr);
	d->coproc = devm_ioremap(&pdev->dev, 0x412e00000ULL, 0x4c);
	if (!d->coproc)
		return -ENOMEM;
	d->smc = dev_get_drvdata(pdev->dev.parent);
	if (!d->smc)
		return -ENODEV;
	ret = apple_smc_get_key_info(d->smc, SMC_KEY(pmBL), &info);
	if (ret)
		return ret;
	if (info.type_code != SMC_KEY(ui32) || info.size != 4 ||
	    info.flags != (APPLE_SMC_WRITABLE | APPLE_SMC_FUNCTION))
		return -EINVAL;
	d->lcd = devm_gpiod_get(&pdev->dev, "lcd-enable", GPIOD_ASIS);
	if (IS_ERR(d->lcd))
		return PTR_ERR(d->lcd);
	if (gpiod_get_direction(d->lcd) != 0 ||
	    gpiod_get_value_cansleep(d->lcd) != 1)
		return -EINVAL;
	platform_set_drvdata(pdev, d);
	WRITE_ONCE(state, 1);
	dev_info(&pdev->dev, "qualified controls acquired; no power change at probe\n");
	return 0;
}

static void display_remove(struct platform_device *pdev)
{
	display_on(&pdev->dev);
}

/* s2idle uses the regular suspend/resume callbacks; hibernation is unqualified. */
static const struct dev_pm_ops display_pm = {
	.suspend = display_off,
	.resume = display_on,
	.suspend_late = clocks_off,
	.resume_early = clocks_on,
};
static struct platform_driver display_driver = {
	.probe = display_probe,
	.remove = display_remove,
	.driver = {
		.name = "neo-display-sleep",
		.pm = &display_pm,
	},
};

static int __init display_init(void)
{
	struct device *child;
	struct platform_device_info info = {
		.name = "neo-display-sleep",
		.id = PLATFORM_DEVID_NONE,
	};
	int ret;

	if (!experimental || !of_machine_is_compatible("apple,j700"))
		return -ENODEV;
	child = bus_find_device_by_name(&platform_bus_type, NULL, "macsmc-power");
	if (!child)
		return -ENODEV;
	info.parent = child->parent;
	gpiod_add_lookup_table(&display_gpio);
	ret = platform_driver_register(&display_driver);
	if (ret)
		goto remove_lookup;
	display_device = platform_device_register_full(&info);
	if (IS_ERR(display_device)) {
		ret = PTR_ERR(display_device);
		goto remove_driver;
	}
	/* Treat a failed/deferred probe as failure, not a working sleep hook. */
	if (READ_ONCE(state) != 1) {
		ret = -ENODEV;
		platform_device_unregister(display_device);
		goto remove_driver;
	}
	put_device(child);
	return 0;
remove_driver:
	platform_driver_unregister(&display_driver);
remove_lookup:
	gpiod_remove_lookup_table(&display_gpio);
	put_device(child);
	return ret;
}

static void __exit display_exit(void)
{
	platform_device_unregister(display_device);
	platform_driver_unregister(&display_driver);
	gpiod_remove_lookup_table(&display_gpio);
}
module_init(display_init);
module_exit(display_exit);
MODULE_LICENSE("GPL");
MODULE_DESCRIPTION("J700 experimental LCD and backlight system sleep sequencing");
