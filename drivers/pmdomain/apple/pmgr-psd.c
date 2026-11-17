// SPDX-License-Identifier: GPL-2.0-only OR MIT
/* T8152 AVD PSD service. Keep powered during initial decoder bring-up. */
#include <linux/io.h>
#include <linux/iopoll.h>
#include <linux/module.h>
#include <linux/of.h>
#include <linux/platform_device.h>
#include <linux/pm_domain.h>

struct apple_psd {
	struct generic_pm_domain genpd;
	void __iomem *regs;
};

static int apple_psd_on(struct generic_pm_domain *genpd)
{
	struct apple_psd *psd = container_of(genpd, struct apple_psd, genpd);
	u32 ack, state;
	int ret;

	if (!(readl(psd->regs + 0x80) & 7))
		return 0;
	writel(0, psd->regs + 0x90);
	/* Reading STATE consumes ACK; wait for ACK before reading it. */
	ret = readl_poll_timeout(psd->regs + 0xbc, ack, ack & 1, 10, 1000000);
	state = readl(psd->regs + 0x80);
	if (ret || (state & 7)) {
		pr_err("apple-psd: %s failed to power on: ack=%#x state=%#x\n",
		       genpd->name, ack, state);
		return ret ?: -EIO;
	}
	return 0;
}

static void apple_psd_remove_provider(void *data)
{
	struct platform_device *pdev = data;
	struct apple_psd *psd = platform_get_drvdata(pdev);

	of_genpd_del_provider(pdev->dev.of_node);
	pm_genpd_remove(&psd->genpd);
}

static int apple_psd_probe(struct platform_device *pdev)
{
	struct apple_psd *psd;
	int ret;

	psd = devm_kzalloc(&pdev->dev, sizeof(*psd), GFP_KERNEL);
	if (!psd)
		return -ENOMEM;
	psd->regs = devm_platform_ioremap_resource(pdev, 0);
	if (IS_ERR(psd->regs))
		return PTR_ERR(psd->regs);
	platform_set_drvdata(pdev, psd);
	psd->genpd.name = dev_name(&pdev->dev);
	psd->genpd.power_on = apple_psd_on;
	psd->genpd.flags = GENPD_FLAG_ALWAYS_ON;
	ret = apple_psd_on(&psd->genpd);
	if (ret)
		return ret;
	ret = pm_genpd_init(&psd->genpd, NULL, false);
	if (ret)
		return ret;
	ret = of_genpd_add_provider_simple(pdev->dev.of_node, &psd->genpd);
	if (ret) {
		pm_genpd_remove(&psd->genpd);
		return ret;
	}
	return devm_add_action_or_reset(&pdev->dev, apple_psd_remove_provider,
				       pdev);
}

static const struct of_device_id apple_psd_match[] = {
	{ .compatible = "apple,t8152-avd-psd" },
	{}
};
MODULE_DEVICE_TABLE(of, apple_psd_match);
static struct platform_driver apple_psd_driver = {
	.probe = apple_psd_probe,
	.driver = { .name = "apple-pmgr-psd", .of_match_table = apple_psd_match,
		    .suppress_bind_attrs = true },
};
module_platform_driver(apple_psd_driver);
MODULE_LICENSE("Dual MIT/GPL");
MODULE_DESCRIPTION("Apple T8152 AVD PSD power domain");
