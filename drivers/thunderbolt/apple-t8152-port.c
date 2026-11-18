// SPDX-License-Identifier: GPL-2.0-only OR MIT
/*
 * Apple T8152 (J873g) rear USB-C ports in USB4 mode
 *
 * The J873g's rear ports have no Type-C port controller driver: the display
 * driver (apple_dcpib) talks to the ACE3 PD controllers itself for DP alt
 * mode. This driver watches the same controllers for a USB4 cable, powers
 * the port's ATC PHY up in CIO mode with the ADT tunables, and hands the
 * cable to the ACIO router through its Thunderbolt switch. On unplug it
 * stops the router and takes the PHY down with the firmware's CIO shutdown
 * sequence, so that the next cable, in either orientation, starts from a
 * cold PHY and a cold router.
 *
 * The PHY sequences follow AppleT8152TypeCPhy. The ATC register blocks are
 * shared with apple_dcpib, which drives the same PHY for DisplayPort: the
 * PHY is only taken over when its control register shows it idle, and the
 * PD controller's register window is used under its device lock.
 */

#include <linux/bits.h>
#include <linux/delay.h>
#include <linux/device.h>
#include <linux/io.h>
#include <linux/iopoll.h>
#include <linux/jiffies.h>
#include <linux/module.h>
#include <linux/mutex.h>
#include <linux/of.h>
#include <linux/platform_device.h>
#include <linux/reboot.h>
#include <linux/soc/apple/tunable.h>
#include <linux/spmi.h>
#include <linux/sysfs.h>
#include <linux/unaligned.h>
#include <linux/usb/typec_mux.h>
#include <linux/workqueue.h>

/* ATC PHY: lane controls in the AON block, CIO power control in CFG. */
#define ATC_XBAR_CFG		0x4c
#define ATC_XBAR_CIO_MASK	GENMASK(4, 0)
#define ATC_LANE_CTRL(n)	(0x1e18 + 4 * (n))
#define ATC_LANE_PMA_EN		BIT(5)
#define ATC_LANE_PMA_CLAMP	BIT(0)
#define ATC_LANE_PMA_RST_N	BIT(2)
#define ATC_LANE_PMA_PWR	BIT(1)
#define ATC_LANE_CLK		0x1e24
#define ATC_LANE_CLK_REQ(n)	BIT(12 + 2 * (n))
#define ATC_LANE_CLK_EN(n)	BIT(16 + 2 * (n))
#define ATC_CIO_CTRL		0x60000
#define ATC_CIO_CTRL_SB		BIT(0)
#define ATC_CIO_CTRL_PLL	BIT(1)
#define ATC_CIO_CTRL_ISO	BIT(2)
#define ATC_CIO_CTRL_BUS	BIT(3)
#define ATC_CIO_CTRL_READY	BIT(4)
#define ATC_CIO_CTRL_IDLE	ATC_CIO_CTRL_ISO
#define ATC_CIO_STATUS		0x60004
#define ATC_CIO_SWAP		0x60008
#define ATC_CIO_SWAP_FLIP	BIT(1)
#define ATC_CIO_CLK		0x60030
#define ATC_CIO_CLK_EN		BIT(0)
#define ATC_TIMEOUT_US		10000

/* PMGR power states of the port: DP_0 (+0), DP_1 (+0x20), USB AON (+0x100). */
#define PS_DP0			0x000
#define PS_DP1			0x020
#define PS_USB_AON		0x100
#define PS_TARGET		0xf
#define PS_ACTUAL		GENMASK(7, 4)
#define PS_RESET_MASK		0x30f

/* PMGR2 PSD service of the ATC: state 0 powered, 6 cold. */
#define PSD_STATE		0x80
#define PSD_STATE_MASK		GENMASK(2, 0)
#define PSD_TARGET		0x90
#define PSD_ACK			0xbc
#define PSD_ON			0
#define PSD_COLD		6

/* ACE3 PD controller: a register selector with a 32-byte data window. */
#define ACE3_SELECT		0x00
#define ACE3_DATA		0x20
#define ACE3_CMD1		0x08
#define ACE3_DATA1		0x09
#define ACE3_STATUS		0x1a
#define ACE3_STATUS_PLUG	BIT(0)
#define ACE3_STATUS_FLIP	BIT(4)
#define ACE3_POWER_STATE	0x20
#define ACE3_USB4_STATUS	0x24
#define ACE3_DATA_STATUS	0x5f
#define ACE3_DS_UPSTREAM	BIT(7)
#define ACE3_DS_USB4_TRAINING	BIT(20)
#define ACE3_DS_USB4		BIT(23)

#define POLL_MS			250
#define RETRY_MS		1000
#define MAX_RETRIES		3

struct apple_t8152_port {
	struct device *dev;
	void __iomem *phy;
	void __iomem *ps;
	void __iomem *psd;
	struct apple_tunable *tunable_pre, *tunable_post;
	struct spmi_device *pd;
	struct typec_thunderbolt_switch *sw;
	struct typec_thunderbolt_switch_data cable;
	struct notifier_block reboot_nb, switch_nb;
	struct delayed_work work;
	struct mutex lock;		/* port state, PHY and router transitions */
	unsigned int retries, connections, disconnects;
	unsigned long healthy_since;
	u32 status, data_status;
	int last_error;
	bool enabled, stopping, active, phy_owned;
};

static struct workqueue_struct *apple_t8152_port_wq;

static void port_update(void __iomem *reg, u32 mask, u32 set)
{
	writel((readl(reg) & ~mask) | set, reg);
	udelay(1);
}

static void port_apply_tunable(void __iomem *base, const struct apple_tunable *t)
{
	for (size_t i = 0; i < t->sz; i++)
		port_update(base + t->values[i].offset, t->values[i].mask,
			    t->values[i].value);
}

static int port_psd_set(struct apple_t8152_port *port, u32 target)
{
	u32 state = readl(port->psd + PSD_STATE), ack;
	int ret;

	if ((state & PSD_STATE_MASK) == target)
		return 0;
	writel(target, port->psd + PSD_TARGET);
	ret = readl_poll_timeout(port->psd + PSD_ACK, ack, ack & 1, 10, 1000000);
	state = readl(port->psd + PSD_STATE);
	if (ret || (state & PSD_STATE_MASK) != target) {
		dev_err(port->dev, "PSD state %u not reached: state %#x ack %#x\n",
			target, state, ack);
		return ret ?: -EIO;
	}
	return 0;
}

static int port_ps_on(struct apple_t8152_port *port, unsigned int offset)
{
	u32 val;

	port_update(port->ps + offset, PS_RESET_MASK, PS_TARGET);
	return readl_poll_timeout(port->ps + offset, val,
				  (val & PS_ACTUAL) == PS_ACTUAL, 10, 100000);
}

/* The firmware's CIO shutdown; the router must already be stopped. */
static int port_phy_off(struct apple_t8152_port *port)
{
	void __iomem *phy = port->phy;
	u32 val;
	int ret;

	/* A failed power-up may not have released the register bus yet. */
	if (readl(phy + ATC_CIO_CTRL) & ATC_CIO_CTRL_BUS) {
		port_update(phy + ATC_LANE_CLK, 0, BIT(10));
		port_update(phy + ATC_LANE_CLK, BIT(6), 0);
		port_update(phy + ATC_LANE_CLK, BIT(8), 0);
		for (int lane = 0; lane < 2; lane++) {
			port_update(phy + ATC_LANE_CLK, ATC_LANE_CLK_REQ(lane), 0);
			port_update(phy + ATC_LANE_CLK, ATC_LANE_CLK_EN(lane), 0);
		}
		for (int lane = 0; lane < 2; lane++) {
			void __iomem *ctrl = phy + ATC_LANE_CTRL(lane);

			port_update(ctrl, ATC_LANE_PMA_EN, 0);
			port_update(ctrl, 0, ATC_LANE_PMA_CLAMP);
			port_update(ctrl, ATC_LANE_PMA_RST_N, 0);
			port_update(ctrl, ATC_LANE_PMA_PWR, 0);
		}
	}
	port_update(phy + ATC_CIO_CTRL, ATC_CIO_CTRL_READY, 0);
	port_update(phy + ATC_CIO_CTRL, ATC_CIO_CTRL_BUS, 0);
	port_update(phy + ATC_CIO_CTRL, 0, ATC_CIO_CTRL_ISO);
	port_update(phy + ATC_CIO_CTRL, ATC_CIO_CTRL_PLL, 0);
	ret = readl_poll_timeout(phy + ATC_CIO_STATUS, val,
				 !(val & ATC_CIO_CTRL_PLL), 1, ATC_TIMEOUT_US);
	if (ret)
		return ret;
	port_update(phy + ATC_CIO_CTRL, ATC_CIO_CTRL_SB, 0);
	ret = readl_poll_timeout(phy + ATC_CIO_STATUS, val,
				 !(val & ATC_CIO_CTRL_SB), 1, ATC_TIMEOUT_US);
	if (ret)
		return ret;
	port_update(phy + ATC_CIO_CLK, ATC_CIO_CLK_EN, 0);
	if (readl(phy + ATC_CIO_CTRL) != ATC_CIO_CTRL_IDLE)
		return -EIO;
	/* Retraining from a powered common block asserts in the router firmware. */
	return port_psd_set(port, PSD_COLD);
}

static int port_phy_on(struct apple_t8152_port *port, bool reversed)
{
	void __iomem *phy = port->phy;
	u32 val;
	int ret;

	ret = port_ps_on(port, PS_USB_AON);
	if (!ret)
		ret = port_psd_set(port, PSD_ON);
	if (!ret)
		ret = port_ps_on(port, PS_DP0);
	if (!ret)
		ret = port_ps_on(port, PS_DP1);
	if (ret)
		return ret;
	/* Anything else means the display driver or a USB3 link owns it. */
	if (readl(phy + ATC_CIO_CTRL) != ATC_CIO_CTRL_IDLE)
		return -EBUSY;
	port->phy_owned = true;
	port_update(phy + ATC_CIO_CLK, 0, ATC_CIO_CLK_EN);
	port_update(phy + ATC_CIO_CTRL, 0, ATC_CIO_CTRL_SB);
	ret = readl_poll_timeout(phy + ATC_CIO_STATUS, val, val & ATC_CIO_CTRL_SB,
				 1, ATC_TIMEOUT_US);
	if (ret)
		return ret;
	port_update(phy + ATC_CIO_CTRL, 0, ATC_CIO_CTRL_PLL);
	ret = readl_poll_timeout(phy + ATC_CIO_STATUS, val, val & ATC_CIO_CTRL_PLL,
				 1, ATC_TIMEOUT_US);
	if (ret)
		return ret;
	port_update(phy + ATC_CIO_CTRL, ATC_CIO_CTRL_ISO, 0);
	port_update(phy + ATC_CIO_CTRL, 0, ATC_CIO_CTRL_BUS);
	port_apply_tunable(phy, port->tunable_pre);
	for (int lane = 0; lane < 2; lane++) {
		void __iomem *ctrl = phy + ATC_LANE_CTRL(lane);

		port_update(phy + ATC_LANE_CLK, 0, ATC_LANE_CLK_REQ(lane));
		port_update(phy + ATC_LANE_CLK, 0, ATC_LANE_CLK_EN(lane));
		port_update(ctrl, 0, ATC_LANE_PMA_PWR);
		port_update(ctrl, 0, ATC_LANE_PMA_RST_N);
		port_update(ctrl, ATC_LANE_PMA_CLAMP, 0);
		port_update(ctrl, 0, ATC_LANE_PMA_EN);
	}
	port_update(phy + ATC_XBAR_CFG, ATC_XBAR_CIO_MASK, reversed ? 1 : 0);
	port_apply_tunable(phy, port->tunable_post);
	port_update(phy + ATC_CIO_SWAP, ATC_CIO_SWAP_FLIP,
		    reversed ? ATC_CIO_SWAP_FLIP : 0);
	port_update(phy + ATC_CIO_CTRL, 0, ATC_CIO_CTRL_READY);
	dev_dbg(port->dev, "PHY in CIO mode: ctrl %#x xbar %#x lanes %#x %#x\n",
		readl(phy + ATC_CIO_CTRL), readl(phy + ATC_XBAR_CFG),
		readl(phy + ATC_LANE_CTRL(0)), readl(phy + ATC_LANE_CTRL(1)));
	return 0;
}

static int ace3_select(struct spmi_device *pd, u8 reg)
{
	u8 sel;
	int ret = spmi_register_zero_write(pd, reg);

	if (ret)
		return ret;
	for (int i = 0; i < 100; i++) {
		ret = spmi_ext_register_read(pd, ACE3_SELECT, &sel, 1);
		if (ret)
			return ret;
		if (sel == reg)
			return 0;
		usleep_range(1000, 2000);
	}
	return -ETIMEDOUT;
}

static int ace3_read(struct spmi_device *pd, u8 reg, u8 *buf, size_t len)
{
	int ret = ace3_select(pd, reg);

	return ret ?: spmi_ext_register_read(pd, ACE3_DATA, buf, len);
}

static int ace3_write(struct spmi_device *pd, u8 reg, u8 *buf, size_t len)
{
	int ret = ace3_select(pd, reg);

	return ret ?: spmi_ext_register_write(pd, ACE3_DATA, buf, len);
}

/* A sleeping controller only detects a cable after SSPS 0. */
static int port_pd_wake(struct apple_t8152_port *port)
{
	u8 buf[4] = {}, cmd[4] = "SSPS";
	int ret;

	ret = ace3_read(port->pd, ACE3_POWER_STATE, buf, 1);
	if (ret || !buf[0])
		return ret;
	dev_dbg(port->dev, "waking PD power state %#x\n", buf[0]);
	buf[0] = 0;
	ret = ace3_write(port->pd, ACE3_DATA1, buf, 1);
	if (!ret)
		ret = ace3_write(port->pd, ACE3_CMD1, cmd, 4);
	if (ret)
		return ret;
	for (int i = 0; i < 1000; i++) {
		ret = ace3_read(port->pd, ACE3_CMD1, buf, 4);
		if (ret)
			return ret;
		if (!get_unaligned_le32(buf))
			return 0;
		if (!memcmp(buf, "!CMD", 4))
			return -EIO;
		usleep_range(2000, 3000);
	}
	return -ETIMEDOUT;
}

/* Remove tunnelled devices and DMA users before resetting the PHY. */
static int port_stop(struct apple_t8152_port *port)
{
	struct typec_thunderbolt_switch_data off = {
		.state = TYPEC_THUNDERBOLT_SWITCH_OFF,
	};
	int ret;

	lockdep_assert_held(&port->lock);
	if (port->active) {
		ret = typec_thunderbolt_switch_set(port->sw, &off);
		if (ret)
			return ret;
		port->active = false;
		port->disconnects++;
	}
	if (port->phy_owned) {
		ret = port_phy_off(port);
		if (ret)
			return ret;
		port->phy_owned = false;
		dev_info(port->dev, "router stopped, PHY reset for the next cable\n");
	}
	return 0;
}

static int port_start(struct apple_t8152_port *port,
		      const struct typec_thunderbolt_switch_data *data)
{
	int ret;

	lockdep_assert_held(&port->lock);
	if (port->phy_owned) {
		ret = port_stop(port);
		if (ret)
			return ret;
	}
	ret = port_phy_on(port, data->orientation == TYPEC_ORIENTATION_REVERSE);
	if (!ret)
		ret = typec_thunderbolt_switch_set(port->sw, data);
	if (ret) {
		/* The switch unwinds a failed start before it returns. */
		if (port->phy_owned) {
			int err = port_phy_off(port);

			if (err)
				dev_err(port->dev, "PHY cleanup failed: %d\n", err);
			else
				port->phy_owned = false;
		}
		return ret;
	}
	port->cable = *data;
	port->active = true;
	port->healthy_since = jiffies;
	port->connections++;
	dev_info(port->dev, "USB4 cable: EUDO %#x, flipped %d, connection %u\n",
		 data->usb4.eudo, data->orientation == TYPEC_ORIENTATION_REVERSE,
		 port->connections);
	return 0;
}

static void port_poll(struct work_struct *work)
{
	struct apple_t8152_port *port =
		container_of(to_delayed_work(work), struct apple_t8152_port, work);
	struct typec_thunderbolt_switch_data data = {
		.state = TYPEC_THUNDERBOLT_SWITCH_USB4,
	};
	unsigned int delay = POLL_MS;
	u32 status, ds;
	u8 buf[12];
	int ret;

	mutex_lock(&port->lock);
	if (READ_ONCE(port->stopping) || !port->enabled)
		goto unlock;
	/* One register selector, shared with apple_dcpib's DP alt mode. */
	device_lock(&port->pd->dev);
	ret = ace3_read(port->pd, ACE3_STATUS, buf, 5);
	if (ret)
		goto error;
	status = get_unaligned_le32(buf);
	ret = ace3_read(port->pd, ACE3_DATA_STATUS, buf, 5);
	if (ret)
		goto error;
	ds = get_unaligned_le32(buf);
	port->status = status;
	port->data_status = ds;

	/* Never take the port from an upstream host (the debug USB link). */
	if (ds & ACE3_DS_UPSTREAM) {
		ret = port_stop(port);
		if (ret)
			goto error;
		port->retries = 0;
		goto again;
	}
	if (!(status & ACE3_STATUS_PLUG) || !(ds & ACE3_DS_USB4)) {
		ret = port_stop(port);
		if (ret)
			goto error;
		port->retries = 0;
		ret = port_pd_wake(port);
		if (ret)
			goto error;
		goto again;
	}

	ret = ace3_read(port->pd, ACE3_USB4_STATUS, buf, 9);
	if (ret)
		goto error;
	data.orientation = status & ACE3_STATUS_FLIP ?
			   TYPEC_ORIENTATION_REVERSE : TYPEC_ORIENTATION_NORMAL;
	data.usb4.eudo = get_unaligned_le32(buf + 1);
	data.usb4.active_link_training = !!(ds & ACE3_DS_USB4_TRAINING);
	if (port->active) {
		if (data.orientation != port->cable.orientation ||
		    data.usb4.eudo != port->cable.usb4.eudo ||
		    data.usb4.active_link_training !=
		    port->cable.usb4.active_link_training) {
			ret = port_stop(port);
			if (ret)
				goto error;
			goto again;	/* read a fresh snapshot after teardown */
		}
		/* Revalidation catches a disconnect shorter than a poll. */
		ret = typec_thunderbolt_switch_set(port->sw, &data);
		if (ret == -EAGAIN)
			goto again;
		if (!ret) {
			if (time_after(jiffies, port->healthy_since + 10 * HZ))
				port->retries = 0;
			goto again;
		}
		dev_warn(port->dev, "connection lost (%d), restarting\n", ret);
		port->retries++;
		ret = port_stop(port);
		if (ret)
			goto error;
		delay = RETRY_MS;
		goto again;
	}
	if (port->retries >= MAX_RETRIES)
		goto queue;
	ret = port_start(port, &data);
	if (ret) {
		port->retries++;
		goto error;
	}
again:
	port->last_error = 0;
	goto queue;
error:
	if (ret != port->last_error)
		dev_warn(port->dev, "port transition failed: %d (attempt %u)\n",
			 ret, port->retries);
	port->last_error = ret;
	delay = RETRY_MS;
queue:
	device_unlock(&port->pd->dev);
	if (!READ_ONCE(port->stopping) && port->enabled)
		queue_delayed_work(apple_t8152_port_wq, &port->work,
				   msecs_to_jiffies(delay));
unlock:
	mutex_unlock(&port->lock);
}

static int port_switch_notify(struct notifier_block *nb, unsigned long event,
			      void *data)
{
	struct apple_t8152_port *port =
		container_of(nb, struct apple_t8152_port, switch_nb);

	/* Called with router locks held: only ask for a fresh poll. */
	if (!READ_ONCE(port->stopping) && READ_ONCE(port->enabled))
		mod_delayed_work(apple_t8152_port_wq, &port->work, 0);
	return NOTIFY_OK;
}

static void port_shutdown(struct apple_t8152_port *port)
{
	WRITE_ONCE(port->stopping, true);
	cancel_delayed_work_sync(&port->work);
	mutex_lock(&port->lock);
	if (port_stop(port))
		dev_err(port->dev, "port shutdown failed\n");
	mutex_unlock(&port->lock);
}

/* Stop the router before device_shutdown() gates the PCIe and DART devices. */
static int port_reboot_notify(struct notifier_block *nb, unsigned long event,
			      void *data)
{
	port_shutdown(container_of(nb, struct apple_t8152_port, reboot_nb));
	return NOTIFY_DONE;
}

static ssize_t enabled_show(struct device *dev, struct device_attribute *attr,
			    char *buf)
{
	struct apple_t8152_port *port = dev_get_drvdata(dev);

	return sysfs_emit(buf, "%u\n", READ_ONCE(port->enabled));
}

static ssize_t enabled_store(struct device *dev, struct device_attribute *attr,
			     const char *buf, size_t count)
{
	struct apple_t8152_port *port = dev_get_drvdata(dev);
	bool enabled;
	int ret = kstrtobool(buf, &enabled);

	if (ret)
		return ret;
	mutex_lock(&port->lock);
	if (READ_ONCE(port->stopping)) {
		mutex_unlock(&port->lock);
		return -ESHUTDOWN;
	}
	port->enabled = enabled;
	port->retries = 0;
	if (!enabled)
		ret = port_stop(port);
	else
		mod_delayed_work(apple_t8152_port_wq, &port->work, 0);
	mutex_unlock(&port->lock);
	return ret ?: count;
}
static DEVICE_ATTR_RW(enabled);

static ssize_t state_show(struct device *dev, struct device_attribute *attr,
			  char *buf)
{
	struct apple_t8152_port *port = dev_get_drvdata(dev);
	ssize_t n;

	mutex_lock(&port->lock);
	n = sysfs_emit(buf,
		       "active=%u phy=%u status=%#x data=%#x connects=%u disconnects=%u retries=%u error=%d\n",
		       port->active, port->phy_owned, port->status,
		       port->data_status, port->connections, port->disconnects,
		       port->retries, port->last_error);
	mutex_unlock(&port->lock);
	return n;
}
static DEVICE_ATTR_RO(state);

static struct attribute *apple_t8152_port_attrs[] = {
	&dev_attr_enabled.attr,
	&dev_attr_state.attr,
	NULL,
};
ATTRIBUTE_GROUPS(apple_t8152_port);

static void __iomem *port_map(struct platform_device *pdev, const char *name,
			      struct resource **res)
{
	void __iomem *base;

	*res = platform_get_resource_byname(pdev, IORESOURCE_MEM, name);
	if (!*res)
		return IOMEM_ERR_PTR(-EINVAL);
	/* Shared with apple_dcpib: map without claiming. */
	base = devm_ioremap(&pdev->dev, (*res)->start, resource_size(*res));
	return base ?: IOMEM_ERR_PTR(-ENOMEM);
}

static void port_put_pd(void *data)
{
	put_device(data);
}

static void port_put_switch(void *data)
{
	typec_thunderbolt_switch_put(data);
}

static int apple_t8152_port_probe(struct platform_device *pdev)
{
	struct device *dev = &pdev->dev;
	struct apple_t8152_port *port;
	struct device_node *pd_np;
	struct resource *phy_res, *res;
	const char *label = dev_name(dev);
	int ret;

	port = devm_kzalloc(dev, sizeof(*port), GFP_KERNEL);
	if (!port)
		return -ENOMEM;
	port->dev = dev;
	mutex_init(&port->lock);
	INIT_DELAYED_WORK(&port->work, port_poll);
	platform_set_drvdata(pdev, port);

	port->phy = port_map(pdev, "phy", &phy_res);
	if (IS_ERR(port->phy))
		return PTR_ERR(port->phy);
	port->ps = port_map(pdev, "ps", &res);
	if (IS_ERR(port->ps))
		return PTR_ERR(port->ps);
	if (resource_size(res) < PS_USB_AON + 4)
		return -EINVAL;
	port->psd = port_map(pdev, "psd", &res);
	if (IS_ERR(port->psd))
		return PTR_ERR(port->psd);
	if (resource_size(res) < PSD_ACK + 4)
		return -EINVAL;
	port->tunable_pre = devm_apple_tunable_parse(dev, dev->of_node,
						     "apple,tunable-phy-pre", phy_res);
	if (IS_ERR(port->tunable_pre))
		return dev_err_probe(dev, PTR_ERR(port->tunable_pre),
				     "invalid apple,tunable-phy-pre\n");
	port->tunable_post = devm_apple_tunable_parse(dev, dev->of_node,
						      "apple,tunable-phy-post", phy_res);
	if (IS_ERR(port->tunable_post))
		return dev_err_probe(dev, PTR_ERR(port->tunable_post),
				     "invalid apple,tunable-phy-post\n");

	pd_np = of_parse_phandle(dev->of_node, "apple,usb-pd", 0);
	if (!pd_np)
		return dev_err_probe(dev, -EINVAL, "no apple,usb-pd\n");
	port->pd = spmi_find_device_by_of_node(pd_np);
	of_node_put(pd_np);
	if (!port->pd)
		return -EPROBE_DEFER;
	ret = devm_add_action_or_reset(dev, port_put_pd, &port->pd->dev);
	if (ret)
		return ret;
	/* A Type-C driver bound to the controller would own the port. */
	if (device_is_bound(&port->pd->dev))
		return dev_err_probe(dev, -EBUSY, "PD controller has a driver\n");

	port->sw = typec_thunderbolt_switch_get(dev);
	if (IS_ERR(port->sw))
		return dev_err_probe(dev, PTR_ERR(port->sw), "no USB4 switch\n");
	if (!port->sw)
		return dev_err_probe(dev, -ENODEV, "no USB4 switch connection\n");
	ret = devm_add_action_or_reset(dev, port_put_switch, port->sw);
	if (ret)
		return ret;

	ret = spmi_command_wakeup(port->pd);
	if (ret)
		return ret;
	usleep_range(10000, 11000);

	port->switch_nb.notifier_call = port_switch_notify;
	ret = typec_thunderbolt_switch_register_notifier(port->sw, &port->switch_nb);
	if (ret)
		return ret;
	port->reboot_nb.notifier_call = port_reboot_notify;
	ret = register_reboot_notifier(&port->reboot_nb);
	if (ret) {
		typec_thunderbolt_switch_unregister_notifier(port->sw, &port->switch_nb);
		return ret;
	}
	port->enabled = true;
	queue_delayed_work(apple_t8152_port_wq, &port->work, 0);
	of_property_read_string(dev->of_node, "label", &label);
	dev_info(dev, "watching %s for USB4 cables\n", label);
	return 0;
}

static void apple_t8152_port_remove(struct platform_device *pdev)
{
	struct apple_t8152_port *port = platform_get_drvdata(pdev);

	unregister_reboot_notifier(&port->reboot_nb);
	typec_thunderbolt_switch_unregister_notifier(port->sw, &port->switch_nb);
	port_shutdown(port);
}

static const struct of_device_id apple_t8152_port_of_match[] = {
	{ .compatible = "apple,t8152-usb4-port" },
	{}
};
MODULE_DEVICE_TABLE(of, apple_t8152_port_of_match);

static struct platform_driver apple_t8152_port_driver = {
	.probe = apple_t8152_port_probe,
	.remove = apple_t8152_port_remove,
	.driver = {
		.name = "apple-t8152-usb4-port",
		.of_match_table = apple_t8152_port_of_match,
		.dev_groups = apple_t8152_port_groups,
		.suppress_bind_attrs = true,
	},
};

static int __init apple_t8152_port_init(void)
{
	int ret;

	/* Freezable: no PHY or router transitions across system sleep. */
	apple_t8152_port_wq = alloc_ordered_workqueue("apple-t8152-usb4", WQ_FREEZABLE);
	if (!apple_t8152_port_wq)
		return -ENOMEM;
	ret = platform_driver_register(&apple_t8152_port_driver);
	if (ret)
		destroy_workqueue(apple_t8152_port_wq);
	return ret;
}
module_init(apple_t8152_port_init);

static void __exit apple_t8152_port_exit(void)
{
	platform_driver_unregister(&apple_t8152_port_driver);
	destroy_workqueue(apple_t8152_port_wq);
}
module_exit(apple_t8152_port_exit);

MODULE_DESCRIPTION("Apple T8152 rear USB-C ports in USB4 mode");
MODULE_LICENSE("Dual MIT/GPL");
