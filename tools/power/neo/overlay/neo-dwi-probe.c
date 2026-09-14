// SPDX-License-Identifier: GPL-2.0-only
/* Read-only J700 DWI inventory after acquiring its power domain. */
#include <linux/io.h>
#include <linux/module.h>
#include <linux/of.h>
#include <linux/platform_device.h>
#include <linux/pm_runtime.h>

static void neo_dwi_put(void *data)
{
	pm_runtime_put_sync(data);
}

static int neo_dwi_probe(struct platform_device *pdev)
{
	static const unsigned int offsets[] = { 0, 4, 0x80, 0x84, 0xc0, 0xc4, 0xd0 };
	struct device *dev = &pdev->dev;
	void __iomem *base;
	int ret, i;

	if (!of_machine_is_compatible("apple,j700") || !dev->pm_domain)
		return -ENODEV;
	ret = devm_pm_runtime_enable(dev);
	if (ret)
		return ret;
	ret = pm_runtime_resume_and_get(dev);
	if (ret < 0)
		return ret;
	ret = devm_add_action_or_reset(dev, neo_dwi_put, dev);
	if (ret)
		return ret;
	base = devm_platform_ioremap_resource(pdev, 0);
	if (IS_ERR(base))
		return PTR_ERR(base);
	for (i = 0; i < ARRAY_SIZE(offsets); i++) {
		dev_info(dev, "reading DWI offset %#x with domain acquired\n", offsets[i]);
		dev_info(dev, "DWI[%#x] = %#010x\n", offsets[i], readl(base + offsets[i]));
	}
	return 0;
}

static const struct of_device_id neo_dwi_match[] = {
	{ .compatible = "apple,j700-dwi-qualification" },
	{ }
};
MODULE_DEVICE_TABLE(of, neo_dwi_match);

static struct platform_driver neo_dwi_driver = {
	.probe = neo_dwi_probe,
	.driver = {
		.name = "neo-dwi-probe",
		.of_match_table = neo_dwi_match,
	},
};
module_platform_driver(neo_dwi_driver);
MODULE_LICENSE("GPL");
MODULE_DESCRIPTION("Manual powered J700 DWI read-only qualification");
