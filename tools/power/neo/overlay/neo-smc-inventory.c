// SPDX-License-Identifier: GPL-2.0-only
/* Read key names and metadata only, through the bound SMC transport. */
#include <linux/device.h>
#include <linux/mfd/macsmc.h>
#include <linux/module.h>
#include <linux/of.h>
#include <linux/platform_device.h>

static int __init inventory_init(void)
{
	struct apple_smc_key_info info;
	struct apple_smc *smc;
	struct device *child;
	smc_key key;
	unsigned int i;
	int ret = 0;
	char name[5], type[5];

	if (!of_machine_is_compatible("apple,j700"))
		return -ENODEV;
	child = bus_find_device_by_name(&platform_bus_type, NULL, "macsmc-power");
	if (!child)
		return -ENODEV;
	smc = dev_get_drvdata(child->parent);
	if (!smc || smc->key_count > 8192) {
		ret = -EINVAL;
		goto out;
	}
	pr_info("neo-smc-inventory: %u keys\n", smc->key_count);
	for (i = 0; i < smc->key_count; i++) {
		ret = apple_smc_get_key_by_index(smc, i, &key);
		if (ret)
			break;
		ret = apple_smc_get_key_info(smc, key, &info);
		if (ret)
			break;
		snprintf(name, sizeof(name), "%c%c%c%c", key >> 24, key >> 16,
			 key >> 8, key);
		snprintf(type, sizeof(type), "%c%c%c%c", info.type_code >> 24,
			 info.type_code >> 16, info.type_code >> 8, info.type_code);
		pr_info("neo-smc-inventory: %s %s size=%u flags=%#x\n",
			name, type, info.size, info.flags);
		cond_resched();
	}
out:
	put_device(child);
	return ret;
}
static void __exit inventory_exit(void) {}
module_init(inventory_init);
module_exit(inventory_exit);
MODULE_LICENSE("GPL");
MODULE_DESCRIPTION("J700 read-only SMC key metadata inventory");
