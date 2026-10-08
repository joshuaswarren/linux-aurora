// SPDX-License-Identifier: GPL-2.0-only
/* J700 awake EL1 access admission. Unlike /dev/mem, regmap MMIO runs at
 * kernel privilege with the live DT's mapping attributes. No lower target,
 * firmware adoption, RUN stop or retention-control write is performed.
 */
#include <linux/delay.h>
#include <linux/io.h>
#include <linux/mfd/syscon.h>
#include <linux/module.h>
#include <linux/of.h>
#include <linux/panic_notifier.h>
#include <asm/sysreg.h>
#include <linux/regmap.h>

static bool same_value;
static bool panic_registered;
static bool nonposted;
module_param(nonposted, bool, 0400);
MODULE_PARM_DESC(nonposted, "Use ioremap_np for the unchanged write instead of syscon regmap");
static unsigned int clock;
module_param(clock, uint, 0400);
MODULE_PARM_DESC(clock, "Unchanged write target: 0 DISP_CPU, 1 DISP_FE, 2 DISP_SYS");
module_param(same_value, bool, 0400);
MODULE_PARM_DESC(same_value, "Explicitly admit one unchanged display PS write while awake");

/* Read-only Apple L2 error registers, as named by m1n1 cpu_regs.h.
 * Keep the error latched: this probe never writes or clears these registers.
 */
static void report_bus_error(const char *stage)
{
	u64 status = read_sysreg_s(sys_reg(3, 3, 15, 8, 0));
	u64 address = read_sysreg_s(sys_reg(3, 3, 15, 9, 0));
	u64 info = read_sysreg_s(sys_reg(3, 3, 15, 10, 0));

	pr_emerg("NEO_PMGR_EL1: %s L2 status=%#llx address=%#llx info=%#llx\n",
		 stage, status, address, info);
}

static int admission_panic(struct notifier_block *nb, unsigned long event,
			   void *data)
{
	report_bus_error("panic");
	return NOTIFY_DONE;
}

static struct notifier_block admission_notifier = {
	.notifier_call = admission_panic,
};

static int __init neo_pmgr_admission(void)
{
	static const unsigned int offsets[] = { 0x280, 0x318, 0x10000 };
	struct device_node *node;
	struct regmap *pmgr;
	void __iomem *asc, *np = NULL;
	u32 value, control, status, offset, expected;
	int ret, i;

	if (!of_machine_is_compatible("apple,j700") || clock > 2)
		return -EINVAL;
	offset = offsets[2 - clock];
	expected = clock ? 0xff : 0x1f0000ff;
	node = of_find_node_by_path("/soc/power-management@300700000");
	if (!node)
		return -ENODEV;
	pmgr = syscon_node_to_regmap(node);
	of_node_put(node);
	if (IS_ERR(pmgr))
		return PTR_ERR(pmgr);
	for (i = 0; i < ARRAY_SIZE(offsets); i++) {
		value = 0;
		ret = regmap_read(pmgr, offsets[i], &value);
		pr_info("NEO_PMGR_EL1: offset=%#x value=%#x ret=%d\n", offsets[i], value, ret);
		if (ret || value != (i == 2 ? 0x1f0000ff : 0xff))
			return ret ?: -EBUSY;
	}
	if (value != 0x1f0000ff)
		return -EBUSY;
	asc = ioremap_np(0x412e00000ULL, 0x4c);
	if (!asc)
		return -ENOMEM;
	control = readl(asc + 0x44);
	status = readl(asc + 0x48);
	iounmap(asc);
	pr_info("NEO_PMGR_EL1: control=%#x status=%#x\n", control, status);
	if (control != 0x10 || status != 0x6d)
		return -EBUSY;
	report_bus_error("awake");
	if (!same_value) {
		msleep(5000);
		pr_info("NEO_PMGR_EL1: read-only admission complete\n");
		return 0;
	}
	pr_info("NEO_PMGR_EL1: before same-value regmap write offset=%#x value=%#x\n",
		offset, expected);
	msleep(500);
	if (nonposted) {
		np = ioremap_np(0x300700000ULL + offset, 4);
		if (!np)
			return -ENOMEM;
		value = readl(np);
		pr_info("NEO_PMGR_EL1: nonposted readback=%#x\n", value);
		if (value != expected) {
			iounmap(np);
			return -EBUSY;
		}
		msleep(500);
	}
	ret = atomic_notifier_chain_register(&panic_notifier_list, &admission_notifier);
	if (ret) {
		if (np)
			iounmap(np);
		return ret;
	}
	panic_registered = true;
	if (np) {
		writel(expected, np);
		ret = 0;
	} else {
		ret = regmap_write(pmgr, offset, expected);
	}
	dsb(sy);
	pr_info("NEO_PMGR_EL1: write returned %d\n", ret);
	if (ret)
		goto remove_observer;
	ret = regmap_read(pmgr, offset, &value);
	pr_info("NEO_PMGR_EL1: readback=%#x ret=%d\n", value, ret);
	if (ret || value != expected) {
		ret = ret ?: -EIO;
		goto remove_observer;
	}
	msleep(500);
	if (np)
		iounmap(np);
	pr_info("NEO_PMGR_EL1: admission complete\n");
	return 0;
remove_observer:
	if (np)
		iounmap(np);
	atomic_notifier_chain_unregister(&panic_notifier_list, &admission_notifier);
	panic_registered = false;
	return ret;
}
static void __exit neo_pmgr_admission_exit(void)
{
	if (panic_registered)
		atomic_notifier_chain_unregister(&panic_notifier_list, &admission_notifier);
	/* All mappings are released during init; no persistent state changed. */
}
module_init(neo_pmgr_admission);
module_exit(neo_pmgr_admission_exit);
MODULE_LICENSE("GPL");
MODULE_DESCRIPTION("J700 awake unchanged-value display PMGR access at kernel privilege");
