// SPDX-License-Identifier: GPL-2.0-only
/* Apply an explicitly supplied J700 RTC qualification overlay at runtime. */
#include <linux/firmware.h>
#include <linux/module.h>
#include <linux/of.h>

static char *firmware;
module_param(firmware, charp, 0444);
MODULE_PARM_DESC(firmware, "RTC qualification DTBO under /lib/firmware");
static int overlay_id;

static int __init neo_rtc_overlay_init(void)
{
	const struct firmware *fw;
	int ret;

	if (!firmware || !of_machine_is_compatible("apple,j700"))
		return -ENODEV;
	ret = request_firmware(&fw, firmware, NULL);
	if (ret)
		return ret;
	ret = of_overlay_fdt_apply(fw->data, fw->size, &overlay_id, NULL);
	release_firmware(fw);
	return ret;
}

static void __exit neo_rtc_overlay_exit(void)
{
	if (of_overlay_remove(&overlay_id))
		pr_err("neo RTC overlay could not be removed\n");
}

module_init(neo_rtc_overlay_init);
module_exit(neo_rtc_overlay_exit);
MODULE_LICENSE("GPL");
MODULE_DESCRIPTION("Manual J700 RTC qualification overlay loader");
