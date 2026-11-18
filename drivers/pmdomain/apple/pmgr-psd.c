// SPDX-License-Identifier: GPL-2.0-only OR MIT
/* T8152 PSD services: switch CIO domains, retain the AVD domain. */
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

static bool apple_psd_state_reached(struct apple_psd *psd, u32 target,
				    u32 *ack, u32 *state)
{
	*ack = readl(psd->regs + 0xbc);
	if (!(*ack & 1))
		return false;
	*state = readl(psd->regs + 0x80);
	return (*state & 7) == target;
}

static int apple_psd_set_state(struct generic_pm_domain *genpd, u32 target)
{
	struct apple_psd *psd = container_of(genpd, struct apple_psd, genpd);
	u32 ack, state;
	bool done;
	int ret;

	state = readl(psd->regs + 0x80);
	if ((state & 7) == target)
		return 0;
	writel(target, psd->regs + 0x90);
	/* Reading STATE consumes ACK; wait for ACK before reading it. */
	ret = read_poll_timeout(apple_psd_state_reached, done, done, 10, 1000000,
			       false, psd, target, &ack, &state);
	if (ret || (state & 7) != target) {
		pr_err("apple-psd: %s failed to enter state %u: ack=%#x state=%#x\n",
		       genpd->name, target, ack, state);
		return ret ?: -EIO;
	}
	return 0;
}

static int apple_psd_on(struct generic_pm_domain *genpd)
{
	return apple_psd_set_state(genpd, 0);
}

static int apple_psd_off(struct generic_pm_domain *genpd)
{
	/* CIO must return to its cold state before the next reconfiguration. */
	return apple_psd_set_state(genpd, 6);
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
	bool cio = of_device_is_compatible(pdev->dev.of_node, "apple,t8152-cio-psd");

	psd = devm_kzalloc(&pdev->dev, sizeof(*psd), GFP_KERNEL);
	if (!psd)
		return -ENOMEM;
	psd->regs = devm_platform_ioremap_resource(pdev, 0);
	if (IS_ERR(psd->regs))
		return PTR_ERR(psd->regs);
	platform_set_drvdata(pdev, psd);
	psd->genpd.name = dev_name(&pdev->dev);
	psd->genpd.power_on = apple_psd_on;
	if (cio) {
		/* The PHY must be configured before the router powers up. */
		psd->genpd.power_off = apple_psd_off;
	} else {
		psd->genpd.flags = GENPD_FLAG_ALWAYS_ON;
		ret = apple_psd_on(&psd->genpd);
		if (ret)
			return ret;
	}
	ret = pm_genpd_init(&psd->genpd, NULL, cio);
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
	{ .compatible = "apple,t8152-cio-psd" },
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
MODULE_DESCRIPTION("Apple T8152 PSD power domains");
