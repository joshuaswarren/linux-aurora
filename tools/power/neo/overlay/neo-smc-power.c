// SPDX-License-Identifier: GPL-2.0-only
/* Read-only J700 power-key diagnostics; names are not qualified rail labels. */
#include <linux/debugfs.h>
#include <linux/device.h>
#include <linux/mfd/macsmc.h>
#include <linux/module.h>
#include <linux/of.h>
#include <linux/platform_device.h>
#include <linux/seq_file.h>

static struct device *child;
static struct apple_smc *smc;
static struct dentry *root;
static smc_key keys[128];
static unsigned int count;

static int power_show(struct seq_file *s, void *unused)
{
	unsigned int i;

	for (i = 0; i < count; i++) {
		smc_key key = keys[i];
		u32 bits = 0;
		int ret = apple_smc_read_u32(smc, key, &bits);

		seq_printf(s, "%c%c%c%c %08x %d\n", key >> 24,
			   (key >> 16) & 255, (key >> 8) & 255, key & 255,
			   bits, ret);
	}
	return 0;
}
DEFINE_SHOW_ATTRIBUTE(power);

static int __init power_init(void)
{
	struct apple_smc_key_info info;
	unsigned int i;
	smc_key key;
	int ret;

	if (!of_machine_is_compatible("apple,j700"))
		return -ENODEV;
	child = bus_find_device_by_name(&platform_bus_type, NULL, "macsmc-power");
	if (!child)
		return -ENODEV;
	smc = dev_get_drvdata(child->parent);
	if (!smc || smc->key_count > 8192) {
		ret = -EINVAL;
		goto fail;
	}
	for (i = 0; i < smc->key_count; i++) {
		ret = apple_smc_get_key_by_index(smc, i, &key);
		if (ret)
			goto fail;
		if (key >> 24 != 'P')
			continue;
		ret = apple_smc_get_key_info(smc, key, &info);
		if (ret)
			goto fail;
		if (info.type_code != __SMC_KEY('f', 'l', 't', ' ') ||
		    info.size != 4 || !(info.flags & APPLE_SMC_READABLE) ||
		    (info.flags & (APPLE_SMC_WRITABLE | APPLE_SMC_FUNCTION)))
			continue;
		if (count == ARRAY_SIZE(keys)) {
			ret = -E2BIG;
			goto fail;
		}
		keys[count++] = key;
	}
	root = debugfs_create_dir("neo-smc-power", NULL);
	if (IS_ERR(root)) {
		ret = PTR_ERR(root);
		goto fail;
	}
	debugfs_create_file("raw", 0400, root, NULL, &power_fops);
	pr_info("neo-smc-power: %u read-only float keys; no rail attribution\n", count);
	return 0;
fail:
	put_device(child);
	return ret;
}

static void __exit power_exit(void)
{
	debugfs_remove_recursive(root);
	put_device(child);
}
module_init(power_init);
module_exit(power_exit);
MODULE_LICENSE("GPL");
MODULE_DESCRIPTION("Read-only J700 SMC float power-key diagnostics");
