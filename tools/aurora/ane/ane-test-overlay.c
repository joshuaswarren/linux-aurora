// SPDX-License-Identifier: GPL-2.0-only
/* Temporary RAM overlay loader for the J700 ANE test. Never autoloaded. */
#include <linux/module.h>
#include <linux/of.h>
#include <linux/string.h>

#include "ane-test-dtbo.h"

static bool enable_power_test;
module_param(enable_power_test, bool, 0400);
MODULE_PARM_DESC(enable_power_test, "Explicitly enable the J700 ANE_SYS power test");

static unsigned int mode;
module_param(mode, uint, 0400);
MODULE_PARM_DESC(mode, "0=valid, 1=invalid cluster, 2=missing power domain, 3=invalid PWGATE, 4=invalid CE");

static int overlay_id = -1;

static int __init ane_test_init(void)
{
	struct device_node *node;
	const char *label;
	u32 reg[2];
	int ret = -EINVAL;

	if (!enable_power_test)
		return -EPERM;
	if (!of_machine_is_compatible("apple,j700") ||
	    !of_machine_is_compatible("apple,t8140"))
		return -ENODEV;
	if (mode >= ARRAY_SIZE(ane_test_blobs))
		return -EINVAL;
	node = of_find_node_by_path("/soc/ane@400000000");
	if (node) {
		of_node_put(node);
		return -EEXIST;
	}
	/* The existing provider does not need a phandle for this RAM test. */
	node = of_find_node_by_path(ANE_TEST_PD_PATH);
	if (!node)
		return -ENODEV;
	if (of_device_is_compatible(node, "apple,t8140-pmgr-pwrstate") &&
	    !of_property_read_string(node, "label", &label) &&
	    !strcmp(label, "ane_sys") &&
	    !of_property_read_u32_array(node, "reg", reg, 2) &&
	    reg[0] == 0x290 && reg[1] == 4)
		ret = 0;
	of_node_put(node);
	if (ret)
		return ret;
	ret = of_overlay_fdt_apply(ane_test_blobs[mode].data,
				   ane_test_blobs[mode].size, &overlay_id, NULL);
	if (ret && overlay_id >= 0)
		of_overlay_remove(&overlay_id);
	return ret;
}

static void __exit ane_test_exit(void)
{
	int ret = of_overlay_remove(&overlay_id);

	WARN_ON(ret);
}
module_init(ane_test_init);
module_exit(ane_test_exit);
MODULE_DESCRIPTION("Temporary J700 ANE hardware-test overlay");
MODULE_LICENSE("GPL");
