// SPDX-License-Identifier: GPL-2.0-only
#include <linux/module.h>
#include <linux/etherdevice.h>
#include <linux/hex.h>
#include <linux/of.h>
#include <linux/slab.h>
#include <linux/types.h>

#define J700_PCIE_PATH	"/soc/pcie@1cb0000000"
#define J700_DART_PATH	"/soc/iommu@390000000"
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
 * of_overlay will not add a `phandle` property to a node that is already part
 * of the live tree: add_changeset_property() in drivers/of/overlay.c skips
 * anything is_pseudo_property() matches (drivers/of/of_private.h), which
 * covers "name", "phandle" and "linux,phandle". This DTB already declares the
 * APCIe DART, so the iommu-map the overlay just installed on the host bridge
 * refers to a phandle that no node owns, of_find_node_by_phandle() returns
 * NULL, and of_iommu_configure() would decide the radio has no IOMMU -- which
 * is the exact failure this overlay exists to fix.
 *
 * Take the phandle the resolver assigned to the overlay's own copy of the node
 * and hand it to the live one. of_find_node_by_phandle() walks the tree on a
 * cache miss and compares np->phandle, so the assignment is enough to make it
 * resolvable; no cache surgery is required.
 */
static int j700_radio_adopt_dart_phandle(void)
{
	struct device_node *pcie, *dart, *owner;
	u32 handle;
	int ret;

	pcie = of_find_node_by_path(J700_PCIE_PATH);
	if (!pcie)
		return -ENODEV;

	dart = of_find_node_by_path(J700_DART_PATH);
	if (!dart) {
		of_node_put(pcie);
		return -ENODEV;
	}

	/* iommu-map = <rid-base &dart sid-base count>; cell 1 is the phandle. */
	ret = of_property_read_u32_index(pcie, "iommu-map", 1, &handle);
	if (ret)
		goto out;
	if (!handle) {
		ret = -EINVAL;
		goto out;
	}

	if (dart->phandle == handle) {
		ret = 0;
		goto out;
	}

	owner = of_find_node_by_phandle(handle);
	if (owner) {
		/* Somebody already owns it; adopting would corrupt the tree. */
		pr_err("J700_RADIO_OVERLAY: phandle 0x%x already held by %pOF\n",
		       handle, owner);
		of_node_put(owner);
		ret = -EEXIST;
		goto out;
	}

	if (dart->phandle) {
		pr_err("J700_RADIO_OVERLAY: %pOF already has phandle 0x%x, overlay wants 0x%x\n",
		       dart, dart->phandle, handle);
		ret = -EEXIST;
		goto out;
	}

	dart->phandle = handle;
	pr_info("J700_RADIO_OVERLAY_DART_PHANDLE: adopted 0x%x for %pOF\n",
		handle, dart);
	ret = 0;
out:
	of_node_put(dart);
	of_node_put(pcie);
	return ret;
}

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
		pr_warn("J700_RADIO_OVERLAY: ignoring unusable mac_address=\"%s\"\n",
			mac_address);
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
	if (ret)
		return ret;

	ret = j700_radio_adopt_dart_phandle();
	if (ret)
		goto out_overlay;

	if (have_mac) {
		ret = j700_radio_publish_mac(mac);
		if (ret)
			goto out_overlay;
	}

	pr_info("J700_RADIO_OVERLAY_PASS: bytes=%zu id=%d order=post-nvme-root mac=%s\n",
		size, j700_radio_overlay_id,
		have_mac ? (mac_address ? "module-param" : "loader-handoff")
			 : "chip-eeprom");
	return 0;

out_overlay:
	of_overlay_remove(&j700_radio_overlay_id);
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
