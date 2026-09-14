// SPDX-License-Identifier: GPL-2.0-only
/* Inherited J700 DCP management-only qualification, no display/DMA takeover.
 * No endpoint start, firmware reset, mapping change or new shared buffer.
 * Pins itself after adoption: remove the trial by reboot, not forced unload.
 */
#include <linux/firmware.h>
#include <linux/io.h>
#include <linux/iopoll.h>
#include <linux/mfd/syscon.h>
#include <linux/module.h>
#include <linux/mutex.h>
#include <linux/of_address.h>
#include <linux/platform_device.h>
#include <linux/regmap.h>
#include <linux/soc/apple/rtkit.h>

static bool sleep_enabled;
module_param(sleep_enabled, bool, 0644);
static bool stop_cpu;
module_param(stop_cpu, bool, 0644);
static unsigned int clock_depth;
module_param(clock_depth, uint, 0644);
MODULE_PARM_DESC(clock_depth, "0 retained firmware only; 1 CPU clocks, 2 plus FE, 3 plus SYS; requires stop_cpu");
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
	struct regmap *pmgr;
	bool stopped;
	u32 saved_control, saved_clocks[3];
	unsigned int clocks_touched;
	u64 requested_iova;
	size_t requested_size;
};
static const unsigned int clock_offsets[] = { 0x10000, 0x318, 0x280 };

static int set_clock(struct neo_dcp *d, unsigned int i, bool on)
{
	u32 value, target = on ? d->saved_clocks[i] & 15 : 4;
	int ret = regmap_read(d->pmgr, clock_offsets[i], &value);

	if (ret)
		return ret;
	value &= ~(15 | BIT(28) | BIT(11) | BIT(9) | BIT(8));
	ret = regmap_write(d->pmgr, clock_offsets[i], value | target);
	if (!ret)
		ret = regmap_read_poll_timeout(d->pmgr, clock_offsets[i], value,
			((value >> 4) & 15) == target && !(value & BIT(11)), 10, 10000);
	if (!ret && on && (d->saved_clocks[i] & BIT(28)))
		ret = regmap_write(d->pmgr, clock_offsets[i],
			(value & ~(BIT(11) | BIT(9) | BIT(8))) | BIT(28));
	dev_info(d->dev, "DCP retained clock offset=%#x target=%u result=%d\n", clock_offsets[i], target, ret);
	return ret;
}

static int deny_shmem(void *cookie, struct apple_rtkit_shmem *buffer)
{
	struct neo_dcp *d = cookie;

	d->requested_iova = buffer->iova;
	d->requested_size = buffer->size;
	atomic_set(&d->failed, 1);
	dev_err(d->dev, "Shared-buffer request DVA=%#llx size=%#zx; refusing adoption side effect\n",
		 d->requested_iova, d->requested_size);
	return -EOPNOTSUPP;
}
static void application_message(void *cookie, u8 ep, u64 word)
{
	struct neo_dcp *d = cookie;

	if (!atomic_xchg(&d->failed, 1))
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
	int ret;

	while (d->clocks_touched) {
		ret = set_clock(d, d->clocks_touched - 1, true);
		if (ret) {
			atomic_set(&d->failed, 1);
			return ret; /* No ASC access until all its clocks are restored. */
		}
		d->clocks_touched--;
	}
	if (d->stopped) {
		writel(d->saved_control, d->control + 0x44);
		if (readl(d->control + 0x44) != d->saved_control) {
			atomic_set(&d->failed, 1);
			return -EIO;
		}
		d->stopped = false;
	}
	ret = apple_rtkit_unidle(d->rtk);

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
	if (d->clocks_touched)
		return sysfs_emit(buf, "failed=%d clocks_touched=%u (no gated ASC read)\n",
			atomic_read(&d->failed), d->clocks_touched);
	return sysfs_emit(buf, "failed=%d touched=%u sleeps=%u wakes=%u control=%#x status=%#x requested_iova=%#llx requested_size=%#zx\n",
		atomic_read(&d->failed), d->touched, d->sleeps, d->wakes,
		readl(d->control + 0x44), readl(d->control + 0x48),
		d->requested_iova, d->requested_size);
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
	unsigned int depth = READ_ONCE(clock_depth), i;
	bool stop = READ_ONCE(stop_cpu);
	u32 value;
	int ret;

	guard(mutex)(&d->lock);
	if (!READ_ONCE(sleep_enabled))
		return 0;
	if (atomic_read(&d->failed) || d->touched)
		return -EIO;
	if (depth > 3 || (depth && !stop))
		return -EINVAL;
	for (i = 0; i < depth; i++) {
		ret = regmap_read(d->pmgr, clock_offsets[i], &d->saved_clocks[i]);
		if (ret || (d->saved_clocks[i] & 0xff) != 0xff ||
		    (d->saved_clocks[i] & (BIT(31) | BIT(12) | BIT(11) | BIT(10))))
			return -EBUSY;
	}
	d->saved_control = readl(d->control + 0x44);
	if (!(d->saved_control & BIT(4)))
		return -EBUSY;
	d->touched = true;
	ret = apple_rtkit_idle(d->rtk);
	dev_info(dev, "DCP retained idle result=%d control=%#x status=%#x\n", ret,
		 readl(d->control + 0x44), readl(d->control + 0x48));
	if (!ret && atomic_read(&d->failed))
		ret = -EIO;
	if (!ret && stop) {
		ret = readl_poll_timeout(d->control + 0x48, value, value & 3, 10, 10000);
		dev_info(dev, "DCP firmware idle before ASC stop: status=%#x result=%d\n", value, ret);
	}
	if (!ret && stop) {
		d->stopped = true;
		writel(d->saved_control & ~BIT(4), d->control + 0x44);
		value = readl(d->control + 0x44);
		writel(value & ~BIT(5), d->control + 0x44);
		ret = readl_poll_timeout(d->control + 0x48, value, value & 3, 10, 10000);
		dev_info(dev, "DCP retained ASC stop: idle=%#x result=%d\n", value, ret);
	}
	for (i = 0; !ret && i < depth; i++) {
		d->clocks_touched = i + 1;
		ret = set_clock(d, i, false);
	}
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
	d->pmgr = pmgr;
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
