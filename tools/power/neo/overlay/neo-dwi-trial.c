// SPDX-License-Identifier: GPL-2.0-only
/* Manual J700 DWI WLED command trial; panel wiring is not yet qualified.
 * Register sequence: native AppleS8000DWI::sendWLED, version 1, immediate.
 * This is deliberately not registered as a Linux backlight device.
 */
#include <linux/io.h>
#include <linux/iopoll.h>
#include <linux/module.h>
#include <linux/mutex.h>
#include <linux/of.h>
#include <linux/platform_device.h>
#include <linux/pm_runtime.h>

struct neo_dwi {
	void __iomem *base;
	struct mutex lock;
	int level;
};

static int send_level(struct neo_dwi *dwi, unsigned int level)
{
	u32 control;
	int ret;

	ret = readl_poll_timeout(dwi->base + 0x80, control, !(control & 1), 10, 10000);
	if (ret)
		return ret;
	writel(0xa0000000 | level, dwi->base + 0x84);
	writel(0x71, dwi->base + 0x80);
	ret = readl_poll_timeout(dwi->base + 0x80, control, !(control & 1), 10, 10000);
	if (!ret)
		dwi->level = level;
	return ret;
}

static ssize_t level_show(struct device *dev, struct device_attribute *attr, char *buf)
{
	struct neo_dwi *dwi = dev_get_drvdata(dev);

	return sysfs_emit(buf, "%d\n", READ_ONCE(dwi->level));
}

static ssize_t level_store(struct device *dev, struct device_attribute *attr,
			   const char *buf, size_t count)
{
	struct neo_dwi *dwi = dev_get_drvdata(dev);
	unsigned int level;
	int ret;

	ret = kstrtouint(buf, 0, &level);
	/* Trial restricted to at most half the native 11-bit range. */
	if (ret || level > 1024)
		return -EINVAL;
	mutex_lock(&dwi->lock);
	ret = send_level(dwi, level);
	dev_info(dev, "WLED level=%u command=%#x control=%#x result=%d\n",
		 level, readl(dwi->base + 0x84), readl(dwi->base + 0x80), ret);
	mutex_unlock(&dwi->lock);
	return ret ? ret : count;
}
static DEVICE_ATTR_RW(level);
static struct attribute *neo_dwi_attrs[] = { &dev_attr_level.attr, NULL };
ATTRIBUTE_GROUPS(neo_dwi);

static void neo_dwi_put(void *data)
{
	pm_runtime_put_sync(data);
}

static int neo_dwi_probe(struct platform_device *pdev)
{
	struct device *dev = &pdev->dev;
	struct neo_dwi *dwi;
	int ret;

	if (!of_machine_is_compatible("apple,j700") || !dev->pm_domain)
		return -ENODEV;
	dwi = devm_kzalloc(dev, sizeof(*dwi), GFP_KERNEL);
	if (!dwi)
		return -ENOMEM;
	mutex_init(&dwi->lock);
	dwi->level = -1;
	platform_set_drvdata(pdev, dwi);
	ret = devm_pm_runtime_enable(dev);
	if (ret)
		return ret;
	ret = pm_runtime_resume_and_get(dev);
	if (ret < 0)
		return ret;
	ret = devm_add_action_or_reset(dev, neo_dwi_put, dev);
	if (ret)
		return ret;
	dwi->base = devm_platform_ioremap_resource(pdev, 0);
	if (IS_ERR(dwi->base))
		return PTR_ERR(dwi->base);
	dev_info(dev, "powered DWI: config=%#x control=%#x data=%#x; no command sent\n",
		 readl(dwi->base), readl(dwi->base + 0x80), readl(dwi->base + 0x84));
	return 0;
}

static const struct of_device_id neo_dwi_match[] = {
	{ .compatible = "apple,j700-dwi-trial" }, { }
};
MODULE_DEVICE_TABLE(of, neo_dwi_match);
static struct platform_driver neo_dwi_driver = {
	.probe = neo_dwi_probe,
	.driver = {
		.name = "neo-dwi-trial",
		.of_match_table = neo_dwi_match,
		.dev_groups = neo_dwi_groups,
	},
};
module_platform_driver(neo_dwi_driver);
MODULE_LICENSE("GPL");
MODULE_DESCRIPTION("Manual powered J700 WLED command trial");
