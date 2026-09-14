// SPDX-License-Identifier: GPL-2.0-only
/* Awake-only X3200/Melbourne pmBL qualification; restore independently of SSH.
 * Native DCP 25F84: X3200 properties at 0x0435f698..0x0435f774,
 * ControllerPMU enable at 0x0409d8ac writes a native-endian u32 boolean.
 * pmBL is write-only: state below records acknowledged requests, not readback.
 */
#include <linux/device.h>
#include <linux/mfd/macsmc.h>
#include <linux/module.h>
#include <linux/of.h>
#include <linux/platform_device.h>
#include <linux/workqueue.h>

static unsigned int seconds;
module_param(seconds, uint, 0444);
MODULE_PARM_DESC(seconds, "0 validates only; 1..600 blanks backlight then restores on");
static int state = -1;
module_param(state, int, 0444);
MODULE_PARM_DESC(state, "Last acknowledged request: -1 untouched, 0 off, 1 on, negative errno on error");
static struct apple_smc *smc;
static struct device *child;
static bool touched;

static int set_backlight(u32 on)
{
	int ret = apple_smc_write(smc, SMC_KEY(pmBL), &on, sizeof(on));

	WRITE_ONCE(state, ret < 0 ? ret : on);
	pr_info("neo-backlight-trial: pmBL request=%u return=%d\n", on, ret);
	return ret < 0 ? ret : 0;
}

static void restore(struct work_struct *work)
{
	set_backlight(1);
}
static DECLARE_DELAYED_WORK(restore_work, restore);

static int __init trial_init(void)
{
	struct apple_smc_key_info info;
	int ret;

	if (!of_machine_is_compatible("apple,j700") || seconds > 600)
		return -EINVAL;
	child = bus_find_device_by_name(&platform_bus_type, NULL, "macsmc-power");
	if (!child)
		return -ENODEV;
	smc = dev_get_drvdata(child->parent);
	if (!smc) {
		ret = -ENODEV;
		goto fail;
	}
	ret = apple_smc_get_key_info(smc, SMC_KEY(pmBL), &info);
	if (ret)
		goto fail;
	if (info.type_code != SMC_KEY(ui32) || info.size != 4 ||
	    info.flags != (APPLE_SMC_WRITABLE | APPLE_SMC_FUNCTION)) {
		ret = -EINVAL;
		goto fail;
	}
	pr_info("neo-backlight-trial: J700 pmBL metadata verified; seconds=%u\n", seconds);
	if (!seconds)
		return 0;
	touched = true;
	ret = set_backlight(0);
	if (ret) {
		set_backlight(1);
		goto fail;
	}
	schedule_delayed_work(&restore_work, msecs_to_jiffies(seconds * 1000));
	return 0;
fail:
	put_device(child);
	return ret;
}

static void __exit trial_exit(void)
{
	cancel_delayed_work_sync(&restore_work);
	if (touched && READ_ONCE(state) != 1)
		set_backlight(1);
	put_device(child);
}
module_init(trial_init);
module_exit(trial_exit);
MODULE_LICENSE("GPL");
MODULE_DESCRIPTION("J700 bounded awake backlight power qualification");
