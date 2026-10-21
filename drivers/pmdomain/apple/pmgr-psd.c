// SPDX-License-Identifier: GPL-2.0-only OR MIT
/*
 * Apple T8152 PSD (power state director) service slot power domains
 *
 * On the M6 the ATC USB blocks are not usable (even their DART registers hang the
 * bus) until two things have happened: the real PMGR power states ATC<n>_USB_AON and
 * ATC<n>_USB are ACTIVE, and the PSD service slot for the port has been requested to
 * run (P0) and has acknowledged. This driver models the second step as a genpd whose
 * parents are the two PMGR domains: genpd powers the parents first, then this domain's
 * power_on writes the slot request and waits for the ack.
 *
 * Source: AppleT8152PMGR 26A434, PSD::stateChange / PSD::getPsdAck; hardware recipe
 * proven on the Mac mini J873 with the m1n1 proxy (ATC0, PSD slot 71, 2026-10-21).
 * Per-slot window (reg, 0x100 bytes at base + (slot << 8)):
 *   0x80 status  bits 2:0 = current PSD state
 *   0x90 request (write a PSD state)
 *   0xa0 diag
 *   0xbc valid   bit 0 = status is valid
 * PSD states: 0 = P0 RUN_MAX ... 6 = P6 RAIL_GATE (the kernel asserts that range).
 *
 * Ack as in getPsdAck: valid bit 0 set and (status & 7) == requested state.
 *
 * Lifetime: this domain sleeps in power_on (usleep_range polling), so it is not IRQ-safe.
 * The platform bus powers a consumer's single power domain on at attach time, before its
 * probe runs, which issues the request in process context. A consumer that marks its
 * runtime PM irq_safe (the Apple DART does) keeps this domain powered: genpd never powers off
 * a sleepable domain holding an irq-safe device and skips the (atomic-context) power_on on
 * its runtime resume (drivers/pmdomain/core.c irq_safe_dev_in_sleep_domain users). Other
 * consumers may let it power off to state 6.
 */

#include <linux/bitfield.h>
#include <linux/delay.h>
#include <linux/err.h>
#include <linux/io.h>
#include <linux/jiffies.h>
#include <linux/module.h>
#include <linux/of.h>
#include <linux/platform_device.h>
#include <linux/pm_domain.h>

#define APPLE_PSD_STATUS	0x80
#define APPLE_PSD_REQUEST	0x90
#define APPLE_PSD_DIAG		0xa0
#define APPLE_PSD_VALID		0xbc
#define APPLE_PSD_STATE		GENMASK(2, 0)
#define APPLE_PSD_VALID_BIT	BIT(0)

#define APPLE_PSD_P0_RUN_MAX	0
#define APPLE_PSD_P6_RAIL_GATE	6

#define APPLE_PSD_DEFAULT_TIMEOUT_MS	1000	/* ADT pmgr-child psd-timeout-ms */

struct apple_psd {
	struct device *dev;
	void __iomem *regs;
	struct generic_pm_domain genpd;
	unsigned int timeout_ms;
};

#define to_apple_psd(d) container_of(d, struct apple_psd, genpd)

static int apple_psd_request(struct apple_psd *psd, u32 state)
{
	unsigned long timeout = jiffies + msecs_to_jiffies(psd->timeout_ms);
	u32 valid, status;

	writel(state, psd->regs + APPLE_PSD_REQUEST);

	for (;;) {
		valid = readl(psd->regs + APPLE_PSD_VALID);
		status = readl(psd->regs + APPLE_PSD_STATUS);
		if ((valid & APPLE_PSD_VALID_BIT) &&
		    FIELD_GET(APPLE_PSD_STATE, status) == state)
			return 0;
		if (time_after(jiffies, timeout))
			break;
		usleep_range(50, 200);
	}

	dev_err(psd->dev,
		"PSD request %u not acknowledged: valid %#x status %#x diag %#x\n", state,
		valid, status, readl(psd->regs + APPLE_PSD_DIAG));
	return -ETIMEDOUT;
}

static int apple_psd_power_on(struct generic_pm_domain *genpd)
{
	return apple_psd_request(to_apple_psd(genpd), APPLE_PSD_P0_RUN_MAX);
}

static int apple_psd_power_off(struct generic_pm_domain *genpd)
{
	return apple_psd_request(to_apple_psd(genpd), APPLE_PSD_P6_RAIL_GATE);
}

static int apple_psd_probe(struct platform_device *pdev)
{
	struct device *dev = &pdev->dev;
	struct device_node *node = dev->of_node;
	struct of_phandle_iterator it;
	struct apple_psd *psd;
	const char *name;
	u32 status, valid;
	int ret;

	psd = devm_kzalloc(dev, sizeof(*psd), GFP_KERNEL);
	if (!psd)
		return -ENOMEM;
	psd->dev = dev;
	psd->timeout_ms = APPLE_PSD_DEFAULT_TIMEOUT_MS;
	of_property_read_u32(node, "apple,psd-timeout-ms", &psd->timeout_ms);

	psd->regs = devm_platform_ioremap_resource(pdev, 0);
	if (IS_ERR(psd->regs))
		return PTR_ERR(psd->regs);

	ret = of_property_read_string(node, "label", &name);
	if (ret)
		name = node->name;
	psd->genpd.name = name;
	psd->genpd.power_on = apple_psd_power_on;
	psd->genpd.power_off = apple_psd_power_off;

	/*
	 * Always start OFF: an inherited status of state 0 without the valid bit is not an
	 * acknowledged P0, and the parents' state is not known here. The first consumer's
	 * power_on then issues the request (idempotent if the slot already runs P0).
	 * A status read is safe (proven on hardware); no write happens at probe.
	 */
	status = readl(psd->regs + APPLE_PSD_STATUS);
	valid = readl(psd->regs + APPLE_PSD_VALID);

	ret = pm_genpd_init(&psd->genpd, NULL, true);
	if (ret)
		return dev_err_probe(dev, ret, "pm_genpd_init failed\n");

	ret = of_genpd_add_provider_simple(node, &psd->genpd);
	if (ret) {
		dev_err(dev, "of_genpd_add_provider_simple failed: %d\n", ret);
		goto err_remove;
	}

	/* The PMGR parents are brought up before power_on runs. */
	of_for_each_phandle(&it, ret, node, "power-domains", "#power-domain-cells", -1) {
		struct of_phandle_args parent, child;

		parent.np = it.node;
		parent.args_count = of_phandle_iterator_args(&it, parent.args, MAX_PHANDLE_ARGS);
		child.np = node;
		child.args_count = 0;
		ret = of_genpd_add_subdomain(&parent, &child);
		if (ret < 0) {
			if (ret != -EPROBE_DEFER)
				dev_err(dev, "failed to add parent domain %s: %d\n", it.node->name, ret);
			of_node_put(parent.np);
			goto err_unlink;
		}
	}

	/* Parent domains are handled through the genpd hierarchy, not as a device. */
	pm_genpd_remove_device(dev);

	dev_info(dev, "PSD slot domain %s (status %#x valid %#x at probe, starting off)\n", name,
		 status, valid);
	return 0;

err_unlink:
	/* pm_genpd_remove() drops any parent links added so far. */
	of_genpd_del_provider(node);
err_remove:
	pm_genpd_remove(&psd->genpd);
	return ret;
}

static const struct of_device_id apple_psd_of_match[] = {
	{ .compatible = "apple,t8152-psd" },
	{ }
};
MODULE_DEVICE_TABLE(of, apple_psd_of_match);

static struct platform_driver apple_psd_driver = {
	.probe = apple_psd_probe,
	.driver = {
		.name = "apple-pmgr-psd",
		.of_match_table = apple_psd_of_match,
		.suppress_bind_attrs = true,
	},
};
builtin_platform_driver(apple_psd_driver);

MODULE_DESCRIPTION("Apple T8152 PSD service slot power domains");
MODULE_LICENSE("Dual MIT/GPL");
