// SPDX-License-Identifier: GPL-2.0-only
/* Inherited J700 DCP management-only qualification, no display/DMA takeover.
 * No endpoint start, firmware reset, mapping change or new shared buffer.
 * Pins itself after adoption: remove the trial by reboot, not forced unload.
 */
#include <linux/firmware.h>
#include <linux/io.h>
#include <linux/mfd/syscon.h>
#include <linux/module.h>
#include <linux/mutex.h>
#include <linux/of_address.h>
#include <linux/platform_device.h>
#include <linux/regmap.h>
#include <linux/soc/apple/rtkit.h>

static bool sleep_enabled;
module_param(sleep_enabled, bool, 0644);
static char *firmware;
module_param(firmware, charp, 0444);
static int overlay_id;
static struct platform_device *adopted;

struct neo_dcp {
	struct device *dev;
	void __iomem *control;
	struct apple_rtkit *rtk;
	struct mutex lock;
	atomic_t failed;
	bool touched;
	u32 sleeps, wakes;
};

static int deny_shmem(void *cookie, struct apple_rtkit_shmem *buffer)
{
	struct neo_dcp *d = cookie;

	atomic_set(&d->failed, 1);
	dev_err(d->dev, "Unexpected shared-buffer request; refusing adoption side effect\n");
	return -EOPNOTSUPP;
}
static void application_message(void *cookie, u8 ep, u64 word)
{
	struct neo_dcp *d = cookie;

	atomic_set(&d->failed, 1);
	dev_err(d->dev, "Unexpected application activity ep=%#x word=%#llx\n", ep, word);
}
static void crashed(void *cookie, const void *log, size_t size)
{
	struct neo_dcp *d = cookie;

	atomic_set(&d->failed, 1);
}
static const struct apple_rtkit_ops ops = {
	.shmem_setup = deny_shmem,
	.recv_message = application_message,
	.crashed = crashed,
};

static int wake_dcp(struct neo_dcp *d)
{
	int ret = apple_rtkit_unidle(d->rtk);

	dev_info(d->dev, "DCP retained wake result=%d control=%#x status=%#x\n", ret,
		 readl(d->control + 0x44), readl(d->control + 0x48));
	if (ret || !apple_rtkit_is_running(d->rtk)) {
		atomic_set(&d->failed, 1);
		return ret ?: -EIO;
	}
	d->touched = false;
	d->wakes++;
	return atomic_read(&d->failed) ? -EIO : 0;
}
static ssize_t status_show(struct device *dev, struct device_attribute *a, char *buf)
{
	struct neo_dcp *d = dev_get_drvdata(dev);

	guard(mutex)(&d->lock);
	return sysfs_emit(buf, "failed=%d touched=%u sleeps=%u wakes=%u control=%#x status=%#x\n",
		atomic_read(&d->failed), d->touched, d->sleeps, d->wakes,
		readl(d->control + 0x44), readl(d->control + 0x48));
}
static ssize_t wake_store(struct device *dev, struct device_attribute *a, const char *buf, size_t count)
{
	struct neo_dcp *d = dev_get_drvdata(dev);
	int ret;

	if (!sysfs_streq(buf, "1"))
		return -EINVAL;
	guard(mutex)(&d->lock);
	if (atomic_read(&d->failed))
		return -EIO;
	ret = wake_dcp(d);
	return ret ? ret : count;
}
static DEVICE_ATTR_RO(status);
static DEVICE_ATTR_WO(wake);
static struct attribute *neo_dcp_attrs[] = { &dev_attr_status.attr, &dev_attr_wake.attr, NULL };
ATTRIBUTE_GROUPS(neo_dcp);

static int dcp_suspend(struct device *dev)
{
	struct neo_dcp *d = dev_get_drvdata(dev);
	int ret;

	guard(mutex)(&d->lock);
	if (!READ_ONCE(sleep_enabled))
		return 0;
	if (atomic_read(&d->failed) || d->touched)
		return -EIO;
	d->touched = true;
	ret = apple_rtkit_idle(d->rtk);
	dev_info(dev, "DCP retained idle result=%d control=%#x status=%#x\n", ret,
		 readl(d->control + 0x44), readl(d->control + 0x48));
	if (ret)
		wake_dcp(d);
	else
		d->sleeps++;
	return ret;
}
static int dcp_resume(struct device *dev)
{
	struct neo_dcp *d = dev_get_drvdata(dev);

	guard(mutex)(&d->lock);
	return d->touched ? wake_dcp(d) : 0;
}
static const struct dev_pm_ops pm = {
	.suspend_late = dcp_suspend,
	.resume_early = dcp_resume,
};
static int probe(struct platform_device *pdev)
{
	struct device_node *node;
	struct regmap *pmgr;
	struct resource *res;
	struct neo_dcp *d;
	u32 state;
	int ret, i;
	static const unsigned int offsets[] = { 0x280, 0x318, 0x10000 };

	if (!of_machine_is_compatible("apple,j700") || adopted)
		return -ENODEV;
	node = of_find_node_by_path("/soc/power-management@300700000");
	if (!node)
		return -ENODEV;
	pmgr = syscon_node_to_regmap(node);
	of_node_put(node);
	if (IS_ERR(pmgr))
		return PTR_ERR(pmgr);
	for (i = 0; i < ARRAY_SIZE(offsets); i++) {
		ret = regmap_read(pmgr, offsets[i], &state);
		if (ret || (state & 0xff) != 0xff || (state & BIT(11)))
			return -EBUSY;
	}
	res = platform_get_resource(pdev, IORESOURCE_MEM, 0);
	if (!res || res->start != 0x412e00000ULL || resource_size(res) != 0x4c)
		return -EINVAL;
	d = devm_kzalloc(&pdev->dev, sizeof(*d), GFP_KERNEL);
	if (!d)
		return -ENOMEM;
	d->dev = &pdev->dev;
	mutex_init(&d->lock);
	d->control = devm_platform_ioremap_resource(pdev, 0);
	if (IS_ERR(d->control))
		return PTR_ERR(d->control);
	if (!(readl(d->control + 0x44) & BIT(4)))
		return -EINVAL;
	platform_set_drvdata(pdev, d);
	d->rtk = apple_rtkit_init_adopted(&pdev->dev, d, NULL, 0, &ops);
	if (IS_ERR(d->rtk))
		return PTR_ERR(d->rtk);
	__module_get(THIS_MODULE);
	adopted = pdev;
	dev_info(d->dev, "Inherited DCP management transport adopted; module pinned until reboot\n");
	return 0;
}
static const struct of_device_id match[] = { { .compatible = "apple,j700-dcp-retained-trial" }, {} };
static struct platform_driver driver = {
	.probe = probe,
	.driver = { .name = "neo-dcp-retained", .of_match_table = match, .pm = &pm,
		.dev_groups = neo_dcp_groups, .suppress_bind_attrs = true },
};
static int __init init(void)
{
	const struct firmware *fw;
	struct device_node *node;
	int ret;

	if (!firmware || !of_machine_is_compatible("apple,j700"))
		return -ENODEV;
	node = of_find_node_by_path("/soc/mailbox@412e08000");
	if (node) {
		of_node_put(node);
		return -EEXIST;
	}
	ret = request_firmware(&fw, firmware, NULL);
	if (ret)
		return ret;
	ret = platform_driver_register(&driver);
	if (!ret) {
		ret = of_overlay_fdt_apply(fw->data, fw->size, &overlay_id, NULL);
		if (ret)
			platform_driver_unregister(&driver);
	}
	release_firmware(fw);
	return ret;
}
static void __exit fini(void)
{
	of_overlay_remove(&overlay_id);
	platform_driver_unregister(&driver);
}
module_init(init);
module_exit(fini);
MODULE_LICENSE("GPL");
MODULE_DESCRIPTION("J700 inherited DCP retained-sleep transport qualification");
