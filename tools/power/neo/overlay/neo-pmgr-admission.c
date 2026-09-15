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
#include <linux/regmap.h>

static bool same_value;
module_param(same_value, bool, 0400);
MODULE_PARM_DESC(same_value, "Explicitly admit one unchanged DISP_CPU PS write while awake");

static int __init neo_pmgr_admission(void)
{
	static const unsigned int offsets[] = { 0x280, 0x318, 0x10000 };
	struct device_node *node;
	struct regmap *pmgr;
	void __iomem *asc;
	u32 value, control, status;
	int ret, i;

	if (!of_machine_is_compatible("apple,j700"))
		return -EINVAL;
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
		if (ret || (value & 0xff) != 0xff || (value & 0x80001c00))
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
	if (!same_value) {
		msleep(5000);
		pr_info("NEO_PMGR_EL1: read-only admission complete\n");
		return 0;
	}
	pr_info("NEO_PMGR_EL1: before same-value regmap write 0x1f0000ff\n");
	msleep(500);
	ret = regmap_write(pmgr, 0x10000, 0x1f0000ff);
	pr_info("NEO_PMGR_EL1: write returned %d\n", ret);
	if (ret)
		return ret;
	ret = regmap_read(pmgr, 0x10000, &value);
	pr_info("NEO_PMGR_EL1: readback=%#x ret=%d\n", value, ret);
	if (ret || value != 0x1f0000ff)
		return ret ?: -EIO;
	msleep(500);
	pr_info("NEO_PMGR_EL1: admission complete\n");
	return 0;
}
static void __exit neo_pmgr_admission_exit(void)
{
	/* All mappings are released during init; no persistent state changed. */
}
module_init(neo_pmgr_admission);
module_exit(neo_pmgr_admission_exit);
MODULE_LICENSE("GPL");
MODULE_DESCRIPTION("J700 awake unchanged-value PMGR access at kernel privilege");
