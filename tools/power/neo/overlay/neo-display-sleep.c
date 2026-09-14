// SPDX-License-Identifier: GPL-2.0-only
/* Manual J700 qualification module, using the inherited framebuffer.
 * X3200: pmBL controls backlight; gP16 controls LCD enable.
 * Keep the SMC parent alive until both suspend operations finish.
 * See ../display-power.md for firmware evidence and measurement boundaries.
 */
#include <linux/delay.h>
#include <linux/gpio/consumer.h>
#include <linux/gpio/machine.h>
#include <linux/ktime.h>
#include <linux/mfd/macsmc.h>
#include <linux/module.h>
#include <linux/of.h>
#include <linux/platform_device.h>
#include <linux/pm.h>

static bool experimental;
module_param(experimental, bool, 0444);
static int state = -1;
module_param(state, int, 0444);
static unsigned int off_count, on_count;
module_param(off_count, uint, 0444);
module_param(on_count, uint, 0444);
MODULE_PARM_DESC(state, "-1 unbound; 0 LCD low/backlight-off acknowledged; 1 restored; other negative errno");

struct neo_display {
	struct apple_smc *smc;
	struct gpio_desc *lcd;
	ktime_t off_time;
	bool touched;
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

static int display_on(struct device *dev)
{
	struct neo_display *d = dev_get_drvdata(dev);
	s64 elapsed;
	int ret = 0, bl_ret;

	if (!d->touched)
		return 0;
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

	d = devm_kzalloc(&pdev->dev, sizeof(*d), GFP_KERNEL);
	if (!d)
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
