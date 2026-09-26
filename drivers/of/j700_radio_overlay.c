// SPDX-License-Identifier: GPL-2.0-only
#include <linux/module.h>
#include <linux/etherdevice.h>
#include <linux/hex.h>
#include <linux/of.h>
#include <linux/slab.h>
#include <linux/types.h>

#define J700_PCIE_PATH	"/soc/pcie@1cb0000000"
#define J700_WIFI_PATH	J700_PCIE_PATH "/pci@0,0/wifi@0,0"

extern u8 __dtbo_j700_radio_begin[];
extern u8 __dtbo_j700_radio_end[];

static int j700_radio_overlay_id = -1;

static char *mac_address;
module_param(mac_address, charp, 0444);
MODULE_PARM_DESC(mac_address,
		 "Wi-Fi MAC as xx:xx:xx:xx:xx:xx. Overrides /chosen/asahi,wifi-mac-address. "
		 "With neither present no local-mac-address is published and mt76 falls "
		 "back to the MT7932's own EEPROM address, which is normally what you want.");

/*
 * Resolve the MAC to publish, if any. The loader hands one over on J700 as
 * /chosen/asahi,wifi-mac-address; U-Boot on this machine does not, hence the
 * module parameter. Returns 0 and leaves @out untouched when there is none --
 * that is not an error, mt76 then uses the chip's own address.
 */
static bool j700_radio_resolve_mac(u8 *out)
{
	struct device_node *chosen;
	const u8 *mac;
	int length;
	bool found = false;

	if (mac_address) {
		if (mac_pton(mac_address, out) && is_valid_ether_addr(out))
			return true;
		pr_warn("J700_RADIO_OVERLAY: invalid mac_address parameter ignored\n");
	}

	chosen = of_find_node_by_path("/chosen");
	if (!chosen)
		return false;

	mac = of_get_property(chosen, "asahi,wifi-mac-address", &length);
	if (mac && length == ETH_ALEN && is_valid_ether_addr(mac)) {
		ether_addr_copy(out, mac);
		found = true;
	}
	of_node_put(chosen);

	return found;
}

static int j700_radio_publish_mac(const u8 *mac)
{
	struct property *property;
	struct device_node *wifi;
	int ret;

	wifi = of_find_node_by_path(J700_WIFI_PATH);
	if (!wifi)
		return -ENODEV;

	property = kzalloc(sizeof(*property), GFP_KERNEL);
	if (!property) {
		of_node_put(wifi);
		return -ENOMEM;
	}
	property->name = kstrdup("local-mac-address", GFP_KERNEL);
	property->value = kmemdup(mac, ETH_ALEN, GFP_KERNEL);
	property->length = ETH_ALEN;
	if (!property->name || !property->value) {
		ret = -ENOMEM;
		goto err;
	}

	ret = of_add_property(wifi, property);
	if (ret)
		goto err;

	of_node_put(wifi);
	return 0;
err:
	kfree(property->value);
	kfree(property->name);
	kfree(property);
	of_node_put(wifi);
	return ret;
}

static int __init j700_radio_overlay_init(void)
{
	u8 mac[ETH_ALEN];
	bool have_mac;
	size_t size;
	int ret;

	if (!of_machine_is_compatible("apple,j700"))
		return -ENODEV;

	have_mac = j700_radio_resolve_mac(mac);

	size = __dtbo_j700_radio_end - __dtbo_j700_radio_begin;
	ret = of_overlay_fdt_apply(__dtbo_j700_radio_begin, size,
				   &j700_radio_overlay_id, NULL);
	if (ret) {
		struct device_node *symbols =
			of_find_node_by_path("/__symbols__");
		const char *aic_path = NULL;

		if (symbols)
			of_property_read_string(symbols, "aic", &aic_path);
		pr_err("J700_RADIO_OVERLAY: apply failed %d (symbols=%u aic=%s)\n",
		       ret, !!symbols, aic_path ?: "missing");
		of_node_put(symbols);
		return ret;
	}

	if (have_mac) {
		ret = j700_radio_publish_mac(mac);
		if (ret) {
			pr_warn("J700_RADIO_OVERLAY: MAC publication failed (%d); using chip address\n",
				ret);
			have_mac = false;
		}
	}

	pr_info("J700_RADIO_OVERLAY_PASS: bytes=%zu id=%d order=post-nvme-root mac=%s\n",
		size, j700_radio_overlay_id,
			have_mac ? (mac_address ? "module-param" : "loader-handoff")
			 : "chip-eeprom");
	return 0;
}
/* The overlay modifies live nodes used by bound PCI/IOMMU clients. Leave it
 * for the lifetime of this boot; ordinary module removal must be refused. */
module_init(j700_radio_overlay_init);

MODULE_DESCRIPTION("Post-root J700 wireless PCIe device-tree overlay");
MODULE_LICENSE("GPL");
