// SPDX-License-Identifier: GPL-2.0-only
/* Read only the always-accessible Main PMGR state words, never gated devices. */
#include <linux/debugfs.h>
#include <linux/ktime.h>
#include <linux/mfd/syscon.h>
#include <linux/module.h>
#include <linux/of_address.h>
#include <linux/platform_device.h>
#include <linux/regmap.h>
#include <linux/seq_file.h>

static const struct { const char *name; u32 offset; } words[] = {
	{ "gfx", 0x1f0 }, { "ans", 0x278 }, { "disp_sys", 0x280 },
	{ "disp_fe", 0x318 }, { "pmp", 0x3d0 }, { "atc0_usb", 0x3f0 },
	{ "disp_cpu", 0x10000 },
};
static struct regmap *map;
static struct platform_device *pdev;
static struct dentry *root;
static u32 before[ARRAY_SIZE(words)], after[ARRAY_SIZE(words)];
static u64 begin_ns, end_ns;

static int sample(u32 *values)
{
	unsigned int i;
	int ret;

	for (i = 0; i < ARRAY_SIZE(words); i++) {
		ret = regmap_read(map, words[i].offset, &values[i]);
		if (ret)
			return ret;
	}
	return 0;
}

static int snapshot_suspend(struct device *dev)
{
	begin_ns = ktime_get_boottime_ns();
	end_ns = 0;
	return sample(before);
}

static int snapshot_resume(struct device *dev)
{
	end_ns = ktime_get_boottime_ns();
	return sample(after);
}

static int stats_show(struct seq_file *s, void *unused)
{
	u32 now[ARRAY_SIZE(words)];
	unsigned int i;
	int ret = sample(now);

	if (ret)
		return ret;
	seq_printf(s, "begin_ns %llu end_ns %llu (noirq callback endpoints)\n", begin_ns, end_ns);
	seq_puts(s, "domain offset before after now\n");
	for (i = 0; i < ARRAY_SIZE(words); i++)
		seq_printf(s, "%s %#x %#010x %#010x %#010x\n", words[i].name,
			   words[i].offset, before[i], after[i], now[i]);
	return 0;
}
DEFINE_SHOW_ATTRIBUTE(stats);

static const struct dev_pm_ops snapshot_pm = {
	.suspend_noirq = snapshot_suspend,
	.resume_noirq = snapshot_resume,
};
static int snapshot_probe(struct platform_device *dev) { return 0; }
static struct platform_driver snapshot_driver = {
	.probe = snapshot_probe,
	.driver = { .name = "neo-pmgr-snapshot", .pm = &snapshot_pm },
};

static int __init snapshot_init(void)
{
	struct device_node *node;
	struct resource res;
	int ret;

	if (!of_machine_is_compatible("apple,j700"))
		return -ENODEV;
	node = of_find_node_by_path("/soc/power-management@300700000");
	if (!node)
		return -ENODEV;
	ret = of_address_to_resource(node, 0, &res);
	if (!ret && (res.start != 0x300700000ULL || resource_size(&res) < 0x10004))
		ret = -EINVAL;
	map = ret ? ERR_PTR(ret) : syscon_node_to_regmap(node);
	of_node_put(node);
	if (IS_ERR(map))
		return PTR_ERR(map);
	ret = platform_driver_register(&snapshot_driver);
	if (ret)
		return ret;
	pdev = platform_device_register_simple("neo-pmgr-snapshot", -1, NULL, 0);
	if (IS_ERR(pdev)) {
		platform_driver_unregister(&snapshot_driver);
		return PTR_ERR(pdev);
	}
	root = debugfs_create_dir("neo-pmgr-snapshot", NULL);
	debugfs_create_file("stats", 0400, root, NULL, &stats_fops);
	return 0;
}
static void __exit snapshot_exit(void)
{
	debugfs_remove_recursive(root);
	platform_device_unregister(pdev);
	platform_driver_unregister(&snapshot_driver);
}
module_init(snapshot_init);
module_exit(snapshot_exit);
MODULE_LICENSE("GPL");
MODULE_DESCRIPTION("Read-only J700 Main PMGR state snapshots");
