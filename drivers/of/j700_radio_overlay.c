// SPDX-License-Identifier: GPL-2.0-only
#include <linux/module.h>
#include <linux/etherdevice.h>
#include <linux/of.h>
#include <linux/slab.h>
#include <linux/types.h>

extern u8 __dtbo_j700_radio_begin[];
extern u8 __dtbo_j700_radio_end[];

static int j700_radio_overlay_id = -1;

static int __init j700_radio_overlay_init(void)
{
	static const char wifi_path[] =
		"/soc/pcie@1cb0000000/pci@0,0/wifi@0,0";
	struct device_node *chosen, *wifi;
	struct property *property;
	const u8 *mac;
	int length;
	size_t size;
	int ret;

	if (!of_machine_is_compatible("apple,j700"))
		return -ENODEV;

	size = __dtbo_j700_radio_end - __dtbo_j700_radio_begin;
	chosen = of_find_node_by_path("/chosen");
	if (!chosen)
		return -ENODEV;
	mac = of_get_property(chosen, "asahi,wifi-mac-address", &length);
	if (!mac || length != ETH_ALEN || !is_valid_ether_addr(mac)) {
		of_node_put(chosen);
		return -EINVAL;
	}
	ret = of_overlay_fdt_apply(__dtbo_j700_radio_begin, size,
				   &j700_radio_overlay_id, NULL);
	if (ret)
		goto out_chosen;

	wifi = of_find_node_by_path(wifi_path);
	if (!wifi) {
		ret = -ENODEV;
		goto out_overlay;
	}
	property = kzalloc(sizeof(*property), GFP_KERNEL);
	if (!property) {
		ret = -ENOMEM;
		goto out_wifi;
	}
	property->name = kstrdup("local-mac-address", GFP_KERNEL);
	property->value = kmemdup(mac, ETH_ALEN, GFP_KERNEL);
	property->length = ETH_ALEN;
	if (!property->name || !property->value) {
		kfree(property->value);
		kfree(property->name);
		kfree(property);
		ret = -ENOMEM;
		goto out_wifi;
	}
	ret = of_add_property(wifi, property);
	if (ret) {
		kfree(property->value);
		kfree(property->name);
		kfree(property);
		goto out_wifi;
	}

	of_node_put(wifi);
	of_node_put(chosen);

	pr_info("J700_RADIO_OVERLAY_PASS: bytes=%zu id=%d order=post-nvme-root mac=loader-handoff\n",
		size, j700_radio_overlay_id);
	return 0;

out_wifi:
	of_node_put(wifi);
out_overlay:
	of_overlay_remove(&j700_radio_overlay_id);
out_chosen:
	of_node_put(chosen);
	return ret;
}
module_init(j700_radio_overlay_init);

static void __exit j700_radio_overlay_exit(void)
{
	if (j700_radio_overlay_id >= 0)
		of_overlay_remove(&j700_radio_overlay_id);
}
module_exit(j700_radio_overlay_exit);

MODULE_DESCRIPTION("Post-root J700 wireless PCIe device-tree overlay");
MODULE_LICENSE("GPL");
