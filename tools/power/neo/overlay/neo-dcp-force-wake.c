// SPDX-License-Identifier: GPL-2.0-only
/* J700 automatic-idle admission: release the inherited DCP force-wake
 * assertion only after device/console suspend; restore it before resume.
 * Native AppleDCPExpert::_setDCPForceWakeup and the J700 dcp0-expert ADT
 * select PA 0x308008000, off=0/on=0x10. No PMGR target, ASC RUN, RTKit
 * power message, firmware memory or display mapping is changed here.
 * Releasing this assertion does not prove that DCP enters a low power state.
 */
#include <linux/console.h>
#include <linux/io.h>
#include <linux/module.h>
#include <linux/of.h>
#include <linux/platform_device.h>

static bool enabled, released, failed;
static unsigned int suspends, resumes;
module_param(enabled, bool, 0644);
module_param(released, bool, 0444);
module_param(failed, bool, 0444);
module_param(suspends, uint, 0444);
module_param(resumes, uint, 0444);
MODULE_PARM_DESC(enabled, "Release DCP force-wake during an explicitly configured sleep trial");
static struct platform_device *pdev;

static int restore(struct device *dev)
{
	void __iomem *control = dev_get_drvdata(dev);

	if (!released)
		return 0;
	writel(0x10, control);
	if (readl(control) != 0x10) {
		failed = true;
		return -EIO;
	}
	released = false;
	resumes++;
	dev_info(dev, "DCP force-wake restored\n");
	return 0;
}

static int suspend_noirq(struct device *dev)
{
	void __iomem *control = dev_get_drvdata(dev);

	if (!READ_ONCE(enabled))
		return 0;
	if (failed || released || !READ_ONCE(console_suspend_enabled) ||
	    readl(control) != 0x10)
		return -EBUSY;
	released = true;
	writel(0, control);
	if (readl(control) != 0) {
		failed = true;
		restore(dev);
		return -EIO;
	}
	suspends++;
	dev_info(dev, "DCP force-wake released\n");
	return 0;
}

static int probe(struct platform_device *dev)
{
	void __iomem *control;

	if (!of_machine_is_compatible("apple,j700"))
		return -ENODEV;
	control = devm_ioremap(&dev->dev, 0x308008000ULL, 4);
	if (!control)
		return -ENOMEM;
	if (readl(control) != 0x10)
		return -EBUSY;
	platform_set_drvdata(dev, control);
	return 0;
}

static void remove_device(struct platform_device *dev)
{
	restore(&dev->dev);
}

static const struct dev_pm_ops pm = {
	.suspend_noirq = suspend_noirq,
	.resume_noirq = restore,
};
static struct platform_driver driver = {
	.probe = probe,
	.remove = remove_device,
	.driver = { .name = "neo-dcp-force-wake", .pm = &pm },
};
static int __init init(void)
{
	int ret;

	if (!of_machine_is_compatible("apple,j700"))
		return -ENODEV;
	ret = platform_driver_register(&driver);
	if (ret)
		return ret;
	pdev = platform_device_register_simple("neo-dcp-force-wake", -1, NULL, 0);
	if (IS_ERR(pdev)) {
		platform_driver_unregister(&driver);
		return PTR_ERR(pdev);
	}
	if (!platform_get_drvdata(pdev)) {
		platform_device_unregister(pdev);
		platform_driver_unregister(&driver);
		return -ENODEV;
	}
	return 0;
}
static void __exit exit_module(void)
{
	platform_device_unregister(pdev);
	platform_driver_unregister(&driver);
}
module_init(init);
module_exit(exit_module);
MODULE_LICENSE("GPL");
MODULE_DESCRIPTION("J700 DCP force-wake release across suspend noirq");
