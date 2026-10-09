// SPDX-License-Identifier: GPL-2.0-only
/* Install two RAM overlays in order: power sequencer, then DART/firmware test.
 * This test module pins itself after publishing DARTs. Reboot to remove them.
 * A partially applied DART overlay is also retained; never tear down a DART
 * concurrently with a possible firmware start through another task.
 */
#include <linux/module.h>
#include <linux/of.h>
#include <linux/of_irq.h>
#include <linux/string.h>

#include "apple-ane-bringup.h"
#include "ane-fw-test-dtbo.h"

static bool enable_firmware_test;
module_param(enable_firmware_test, bool, 0400);
MODULE_PARM_DESC(enable_firmware_test, "Enable the reboot-only J700 firmware test overlays");
static bool resume_devices;
module_param(resume_devices, bool, 0400);
MODULE_PARM_DESC(resume_devices, "Reuse the verified power node only when all DART/test nodes are absent");
static int power_overlay = -1, firmware_overlay = -1;
static struct device *power;

static int __init ane_fw_overlay_init(void)
{
	static const char * const absent[] = {
		"/soc/ane@400000000", "/soc/ane-fw-test@401600040",
		"/soc/iommu@401800000", "/soc/iommu@401820000", "/soc/iommu@401840000",
	};
	struct device_node *node;
	unsigned int i;
	int ret;

	if (!enable_firmware_test)
		return -EPERM;
	if (!of_machine_is_compatible("apple,j700") ||
	    !of_machine_is_compatible("apple,t8140"))
		return -ENODEV;
	node = of_find_node_by_path(ANE_FW_AIC_PATH);
	if (!node)
		return -ENODEV;
	ret = of_device_is_compatible(node, "apple,t8140-aic3") &&
		of_device_is_compatible(node, "apple,t8122-aic3") &&
		node->phandle == ANE_FW_AIC_PHANDLE && of_property_read_bool(node, "interrupt-controller")
		? 0 : -EINVAL;
	of_node_put(node);
	if (ret)
		return ret;
	for (i = resume_devices ? 1 : 0; i < ARRAY_SIZE(absent); i++) {
		node = of_find_node_by_path(absent[i]);
		if (node) {
			of_node_put(node);
			return -EEXIST;
		}
	}
	if (!resume_devices) {
		ret = of_overlay_fdt_apply(ane_fw_power_blob, sizeof(ane_fw_power_blob),
					   &power_overlay, NULL);
		if (ret)
			goto remove_power;
	}
	ret = apple_ane_bringup_get_engine(&power);
	if (ret)
		goto remove_power;
	/* DART probe performs MMIO; the child rails and CE must already be ready. */
	ret = of_overlay_fdt_apply(ane_fw_devices_blob, sizeof(ane_fw_devices_blob),
				   &firmware_overlay, NULL);
	if (ret && firmware_overlay < 0) {
		apple_ane_bringup_put_engine(power);
		power = NULL;
		goto remove_power;
	}
	__module_get(THIS_MODULE);
	if (ret)
		pr_err("ANE firmware overlay partially applied: %d; retained until Neo reboot\n", ret);
	else
		pr_info("ANE firmware test devices installed with power held; reboot to remove\n");
	return 0;

remove_power:
	if (power_overlay >= 0)
		of_overlay_remove(&power_overlay);
	return ret;
}
module_init(ane_fw_overlay_init);
MODULE_DESCRIPTION("Ordered RAM-only J700 ANE power, DART and firmware test overlays");
MODULE_LICENSE("GPL");
