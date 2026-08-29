// SPDX-License-Identifier: GPL-2.0-only
#include <linux/module.h>
#include <linux/of.h>
#include <linux/types.h>

extern u8 __dtbo_j700_radio_begin[];
extern u8 __dtbo_j700_radio_end[];

static int j700_radio_overlay_id = -1;

static int __init j700_radio_overlay_init(void)
{
	size_t size;
	int ret;

	if (!of_machine_is_compatible("apple,j700"))
		return -ENODEV;

	size = __dtbo_j700_radio_end - __dtbo_j700_radio_begin;
	ret = of_overlay_fdt_apply(__dtbo_j700_radio_begin, size,
				   &j700_radio_overlay_id, NULL);
	if (ret)
		return ret;

	pr_info("J700_RADIO_OVERLAY_PASS: bytes=%zu id=%d order=post-nvme-root\n",
		size, j700_radio_overlay_id);
	return 0;
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
