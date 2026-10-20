// SPDX-License-Identifier: GPL-2.0-only OR MIT
#include <linux/bitfield.h>
#include <linux/delay.h>
#include <linux/io.h>
#include <linux/module.h>
#include <linux/of.h>
#include <linux/phy/phy.h>
#include <linux/platform_device.h>
#include <linux/pm_domain.h>
#include <linux/pm_runtime.h>
#include <linux/soc/apple/tunable.h>

#define USB2_CONTROL		0x00
#define USB2_RESET		0x04
#define USB2_VBUS		0x08
#define USB2_CLOCK		0x1c
#define USB2_MODE		GENMASK(2, 0)
#define USB2_RUN		2
#define USB2_STOP		4
#define USB2_RESET_ASSERT	BIT(0)
#define USB2_PORT_RESET		BIT(1)
#define USB2_APB_RESET_N	BIT(2)
#define USB2_SIDDQ		BIT(3)
#define USB2_REFCLK_GATE		BIT(29)
#define USB2_PHYCLOCK_GATE	BIT(30)

struct apple_usb2 {
	void __iomem *usb2;
	void __iomem *eusb2;
	struct apple_tunable *dflt;
	struct apple_tunable *host;
};

static void apple_usb2_update(void __iomem *reg, u32 clear, u32 set)
{
	writel((readl(reg) & ~clear) | set, reg);
}

static int apple_usb2_power_on(struct phy *phy)
{
	struct apple_usb2 *usb = phy_get_drvdata(phy);

	apple_tunable_apply(usb->usb2, usb->dflt);
	apple_tunable_apply(usb->usb2, usb->host);
	apple_usb2_update(usb->usb2 + USB2_VBUS, 0, BIT(2));
	apple_usb2_update(usb->usb2 + USB2_VBUS, 0, BIT(3));
	msleep(10);
	apple_usb2_update(usb->usb2 + USB2_RESET, USB2_SIDDQ, 0);
	udelay(10);
	apple_usb2_update(usb->usb2 + USB2_RESET, USB2_RESET_ASSERT, 0);
	apple_usb2_update(usb->usb2 + USB2_RESET, USB2_PORT_RESET, 0);
	apple_usb2_update(usb->eusb2, 0, BIT(0) | BIT(3));
	apple_usb2_update(usb->usb2 + USB2_RESET, 0, USB2_APB_RESET_N);
	apple_usb2_update(usb->usb2 + USB2_CLOCK, USB2_REFCLK_GATE, 0);
	apple_usb2_update(usb->usb2 + USB2_CLOCK, USB2_PHYCLOCK_GATE, 0);
	udelay(30);
	msleep(5);
	apple_usb2_update(usb->usb2 + USB2_CONTROL, USB2_MODE, USB2_RUN);

	return 0;
}

static int apple_usb2_power_off(struct phy *phy)
{
	struct apple_usb2 *usb = phy_get_drvdata(phy);

	if (!(readl(usb->usb2 + USB2_RESET) & USB2_PORT_RESET)) {
		apple_usb2_update(usb->usb2 + USB2_RESET, 0, USB2_PORT_RESET);
		msleep(5);
	}
	apple_usb2_update(usb->usb2 + USB2_CONTROL, USB2_MODE, USB2_STOP);
	apple_usb2_update(usb->usb2 + USB2_RESET, 0, USB2_SIDDQ);
	apple_usb2_update(usb->usb2 + USB2_RESET, 0, USB2_RESET_ASSERT);
	apple_usb2_update(usb->usb2 + USB2_CLOCK, 0, USB2_REFCLK_GATE);
	apple_usb2_update(usb->usb2 + USB2_CLOCK, 0, USB2_PHYCLOCK_GATE);

	return 0;
}

static const struct phy_ops apple_usb2_ops = {
	.power_on = apple_usb2_power_on,
	.power_off = apple_usb2_power_off,
	.owner = THIS_MODULE,
};

static int apple_usb2_probe(struct platform_device *pdev)
{
	static const char * const pd_names[] = { "usb-aon", "usb" };
	const struct dev_pm_domain_attach_data pd_data = {
		.pd_names = pd_names,
		.num_pd_names = ARRAY_SIZE(pd_names),
	};
	struct device *dev = &pdev->dev;
	struct dev_pm_domain_list *domains;
	struct phy_provider *provider;
	struct resource *res;
	struct apple_usb2 *usb;
	struct phy *phy;
	int ret;

	if (of_count_phandle_with_args(dev->of_node, "power-domains",
				       "#power-domain-cells") != ARRAY_SIZE(pd_names))
		return dev_err_probe(dev, -EINVAL, "Both USB2 power domains are required\n");

	usb = devm_kzalloc(dev, sizeof(*usb), GFP_KERNEL);
	if (!usb)
		return -ENOMEM;

	res = platform_get_resource_byname(pdev, IORESOURCE_MEM, "usb2");
	usb->usb2 = devm_ioremap_resource(dev, res);
	if (IS_ERR(usb->usb2))
		return PTR_ERR(usb->usb2);
	if (resource_size(res) < USB2_CLOCK + sizeof(u32))
		return -EINVAL;

	usb->dflt = devm_apple_tunable_parse(dev, dev->of_node,
					   "apple,tunable-usb2phy-dflt", res);
	if (IS_ERR(usb->dflt))
		return dev_err_probe(dev, PTR_ERR(usb->dflt), "Invalid default tunables\n");
	usb->host = devm_apple_tunable_parse(dev, dev->of_node,
					   "apple,tunable-usb2phy-host", res);
	if (IS_ERR(usb->host))
		return dev_err_probe(dev, PTR_ERR(usb->host), "Invalid host tunables\n");

	res = platform_get_resource_byname(pdev, IORESOURCE_MEM, "eusb2");
	usb->eusb2 = devm_ioremap_resource(dev, res);
	if (IS_ERR(usb->eusb2))
		return PTR_ERR(usb->eusb2);
	if (resource_size(res) < sizeof(u32))
		return -EINVAL;

	ret = devm_pm_domain_attach_list(dev, &pd_data, &domains);
	if (ret < 0)
		return dev_err_probe(dev, ret, "Failed to attach USB2 power domains\n");
	if (ret != ARRAY_SIZE(pd_names))
		return -ENODEV;

	ret = devm_pm_runtime_enable(dev);
	if (ret)
		return ret;

	phy = devm_phy_create(dev, NULL, &apple_usb2_ops);
	if (IS_ERR(phy))
		return PTR_ERR(phy);
	phy_set_drvdata(phy, usb);

	provider = devm_of_phy_provider_register(dev, of_phy_simple_xlate);
	return PTR_ERR_OR_ZERO(provider);
}

static const struct of_device_id apple_usb2_match[] = {
	{ .compatible = "apple,t8152-usb2-phy" },
	{ }
};
MODULE_DEVICE_TABLE(of, apple_usb2_match);

static struct platform_driver apple_usb2_driver = {
	.probe = apple_usb2_probe,
	.driver = {
		.name = "apple-t8152-usb2-phy",
		.of_match_table = apple_usb2_match,
	},
};
module_platform_driver(apple_usb2_driver);

MODULE_DESCRIPTION("Apple T8152 fixed-host USB2 PHY");
MODULE_LICENSE("Dual MIT/GPL");
