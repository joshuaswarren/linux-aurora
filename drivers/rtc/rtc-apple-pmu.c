// SPDX-License-Identifier: GPL-2.0-only
/* Experimental J700 Baku PMU alarm, qualified with retained SPMI power. */
#include <linux/bitops.h>
#include <linux/interrupt.h>
#include <linux/module.h>
#include <linux/mutex.h>
#include <linux/of.h>
#include <linux/of_irq.h>
#include <linux/rtc.h>
#include <linux/platform_device.h>
#include <linux/regmap.h>
#include <linux/suspend.h>
#include <linux/unaligned.h>

#define PMU_CTRL		0xf800
#define PMU_COUNT	0xf802
#define PMU_ALARM	0xf808
#define PMU_EVENT	0xf80c
#define PMU_MASK		0xf80e
#define PMU_OFFSET_SEC	0xf704
#define PMU_OFFSET_FRAC	0xf715
#define PMU_ALARM_BIT	BIT(0)
#define PMU_MONITOR	BIT(0)
#define PMU_ENABLE	BIT(6)
#define TICK_SHIFT	15

static bool experimental;
module_param(experimental, bool, 0444);
MODULE_PARM_DESC(experimental, "Allow retained-power J700 RTC qualification");

struct apple_pmu_rtc {
	struct device *dev;
	struct regmap *regmap;
	struct rtc_device *rtc;
	/* Serialize alarm transactions and the threaded interrupt handler. */
	struct mutex lock;
	s64 offset;
	int irq;
	bool failed;
	bool irq_enabled;
};

static int pmu_read(struct apple_pmu_rtc *p, u16 addr, void *buf, size_t len)
{
	return regmap_bulk_read(p->regmap, addr, buf, len);
}

static int pmu_write(struct apple_pmu_rtc *p, u16 addr, const void *buf, size_t len)
{
	return regmap_bulk_write(p->regmap, addr, buf, len);
}

static int pmu_update(struct apple_pmu_rtc *p, u16 addr, u8 mask, u8 val)
{
	u8 old;
	int ret = pmu_read(p, addr, &old, 1);

	if (ret)
		return ret;
	old = (old & ~mask) | (val & mask);
	return pmu_write(p, addr, &old, 1);
}

static int pmu_ticks(struct apple_pmu_rtc *p, u64 *ticks)
{
	u8 buf[8] = {};
	int ret = pmu_read(p, PMU_COUNT, buf, 6);

	if (!ret)
		*ticks = get_unaligned_le64(buf) >> 1;
	return ret;
}

static int pmu_read_time(struct device *dev, struct rtc_time *tm)
{
	struct apple_pmu_rtc *p = dev_get_drvdata(dev);
	u64 ticks;
	int ret;

	guard(mutex)(&p->lock);
	ret = pmu_ticks(p, &ticks);
	if (!ret)
		rtc_time64_to_tm(((s64)ticks + p->offset) >> TICK_SHIFT, tm);
	return ret;
}

/* Caller holds lock. Mask before touching the comparator; preserve other bits. */
static int pmu_disarm(struct apple_pmu_rtc *p)
{
	u8 event = PMU_ALARM_BIT;
	int ret, err;

	ret = pmu_update(p, PMU_MASK, PMU_ALARM_BIT, PMU_ALARM_BIT);
	err = pmu_update(p, PMU_CTRL, PMU_ENABLE, 0);
	if (!ret)
		ret = err;
	err = pmu_write(p, PMU_EVENT, &event, 1);
	return ret ? ret : err;
}

static int pmu_read_alarm(struct device *dev, struct rtc_wkalrm *alarm)
{
	struct apple_pmu_rtc *p = dev_get_drvdata(dev);
	__le32 count;
	u8 ctrl, event, mask;
	int ret;

	guard(mutex)(&p->lock);
	ret = pmu_read(p, PMU_ALARM, &count, sizeof(count));
	if (ret)
		return ret;
	ret = pmu_read(p, PMU_CTRL, &ctrl, 1);
	if (ret)
		return ret;
	ret = pmu_read(p, PMU_EVENT, &event, 1);
	if (ret)
		return ret;
	ret = pmu_read(p, PMU_MASK, &mask, 1);
	if (ret)
		return ret;
	rtc_time64_to_tm((((s64)le32_to_cpu(count) << TICK_SHIFT) + p->offset) >>
			 TICK_SHIFT, &alarm->time);
	alarm->enabled = !!(ctrl & PMU_ENABLE) && !(mask & PMU_ALARM_BIT);
	alarm->pending = !!(event & PMU_ALARM_BIT);
	return 0;
}

static int pmu_enable(struct apple_pmu_rtc *p)
{
	u8 event = PMU_ALARM_BIT;
	int ret;

	ret = pmu_write(p, PMU_EVENT, &event, 1);
	if (ret)
		return ret;
	ret = pmu_update(p, PMU_MASK, PMU_ALARM_BIT, 0);
	if (ret)
		return ret;
	return pmu_update(p, PMU_CTRL, PMU_MONITOR | PMU_ENABLE,
			  PMU_MONITOR | PMU_ENABLE);
}

static int pmu_alarm_irq_enable(struct device *dev, unsigned int enabled)
{
	struct apple_pmu_rtc *p = dev_get_drvdata(dev);
	__le32 alarm;
	u64 now;
	int ret;

	guard(mutex)(&p->lock);
	if (p->failed)
		return -EIO;
	if (!enabled)
		return pmu_disarm(p);
	ret = pmu_ticks(p, &now);
	if (ret)
		return ret;
	ret = pmu_read(p, PMU_ALARM, &alarm, sizeof(alarm));
	if (ret)
		return ret;
	if (le32_to_cpu(alarm) <= (now >> TICK_SHIFT) ||
	    le32_to_cpu(alarm) - (now >> TICK_SHIFT) > 3600)
		return -ETIME;
	ret = pmu_enable(p);
	if (ret)
		pmu_disarm(p);
	return ret;
}

static int pmu_set_alarm(struct device *dev, struct rtc_wkalrm *alarm)
{
	struct apple_pmu_rtc *p = dev_get_drvdata(dev);
	s64 ticks, seconds;
	__le32 count, verify;
	u64 now;
	int ret;

	guard(mutex)(&p->lock);
	if (p->failed)
		return -EIO;
	ticks = rtc_tm_to_time64(&alarm->time) * (1LL << TICK_SHIFT) - p->offset;
	seconds = (ticks + (1 << TICK_SHIFT) - 1) >> TICK_SHIFT;
	if (seconds <= 0 || seconds > U32_MAX)
		return -ERANGE;
	ret = pmu_ticks(p, &now);
	if (ret)
		return ret;
	/* Short qualification alarms only until long retention is established. */
	if (seconds <= (now >> TICK_SHIFT) || seconds - (now >> TICK_SHIFT) > 3600)
		return -ETIME;
	ret = pmu_disarm(p);
	if (ret)
		return ret;
	count = cpu_to_le32(seconds);
	ret = pmu_write(p, PMU_ALARM, &count, sizeof(count));
	if (ret)
		return ret;
	ret = pmu_read(p, PMU_ALARM, &verify, sizeof(verify));
	if (ret || verify != count)
		return ret ? ret : -EIO;
	if (alarm->enabled) {
		ret = pmu_enable(p);
		if (ret)
			pmu_disarm(p);
	}
	return ret;
}

static irqreturn_t pmu_irq(int irq, void *data)
{
	struct apple_pmu_rtc *p = data;
	u8 event;
	int ret;

	mutex_lock(&p->lock);
	ret = pmu_read(p, PMU_EVENT, &event, 1);
	if (!ret && (event & PMU_ALARM_BIT))
		ret = pmu_disarm(p);
	if (ret) {
		p->failed = true;
		disable_irq_nosync(irq);
	}
	mutex_unlock(&p->lock);
	if (ret) {
		dev_err(p->dev, "alarm IRQ I/O failed: %d; IRQ disabled\n", ret);
		pm_wakeup_dev_event(p->dev, 0, true);
		return IRQ_HANDLED;
	}
	if (!(event & PMU_ALARM_BIT))
		return IRQ_NONE;
	dev_info(p->dev, "PMU RTC alarm fired\n");
	if (device_may_wakeup(p->dev))
		pm_wakeup_dev_event(p->dev, 0, true);
	rtc_update_irq(p->rtc, 1, RTC_AF | RTC_IRQF);
	return IRQ_HANDLED;
}

static const struct rtc_class_ops pmu_ops = {
	.read_time = pmu_read_time,
	.read_alarm = pmu_read_alarm,
	.set_alarm = pmu_set_alarm,
	.alarm_irq_enable = pmu_alarm_irq_enable,
};

static int pmu_prepare(struct device *dev)
{
	struct apple_pmu_rtc *p = dev_get_drvdata(dev);

	if (pm_suspend_target_state != PM_SUSPEND_TO_IDLE || p->failed)
		return -EBUSY;
	return 0;
}

static const struct dev_pm_ops pmu_pm_ops = {
	.prepare = pmu_prepare,
};

static void pmu_cleanup(void *data)
{
	struct apple_pmu_rtc *p = data;

	if (p->irq_enabled)
		disable_irq(p->irq);
	mutex_lock(&p->lock);
	if (pmu_disarm(p))
		dev_err(p->dev, "failed to disarm alarm during cleanup\n");
	mutex_unlock(&p->lock);
	device_init_wakeup(p->dev, false);
}

static int pmu_probe(struct platform_device *pdev)
{
	struct device *dev = &pdev->dev;
	struct apple_pmu_rtc *p;
	__le32 sec;
	__le16 frac;
	u8 ctrl;
	int ret;

	if (!experimental || !of_machine_is_compatible("apple,j700"))
		return -ENODEV;
	p = devm_kzalloc(dev, sizeof(*p), GFP_KERNEL);
	if (!p)
		return -ENOMEM;
	p->dev = dev;
	p->regmap = dev_get_regmap(dev->parent, NULL);
	if (!p->regmap)
		return -EPROBE_DEFER;
	mutex_init(&p->lock);
	dev_set_drvdata(dev, p);
	p->irq = of_irq_get(dev->of_node, 0);
	if (p->irq < 0)
		return p->irq;
	ret = pmu_read(p, PMU_CTRL, &ctrl, 1);
	if (ret)
		return ret;
	/* Do not replace an alarm inherited from another owner. */
	if (ctrl & PMU_ENABLE)
		return -EBUSY;
	ret = pmu_read(p, PMU_OFFSET_SEC, &sec, sizeof(sec));
	if (ret)
		return ret;
	ret = pmu_read(p, PMU_OFFSET_FRAC, &frac, sizeof(frac));
	if (ret)
		return ret;
	p->offset = sign_extend64(((u64)le32_to_cpu(sec) << TICK_SHIFT) |
				  (le16_to_cpu(frac) & 0x7fff), 46);
	p->rtc = devm_rtc_allocate_device(dev);
	if (IS_ERR(p->rtc))
		return PTR_ERR(p->rtc);
	p->rtc->ops = &pmu_ops;
	p->rtc->range_min = max_t(s64, 0, p->offset >> TICK_SHIFT);
	p->rtc->range_max = (s64)U32_MAX + (p->offset >> TICK_SHIFT);
	clear_bit(RTC_FEATURE_UPDATE_INTERRUPT, p->rtc->features);
	ret = device_init_wakeup(dev, true);
	if (ret)
		return ret;
	/*
	 * Retained s2idle only: AIC/SPMI stay powered and the chained parent
	 * continues serving RX completions. The threaded handler acknowledges
	 * the PMU and explicitly reports a system wake event. No claim of
	 * off-domain wake routing is made by this experimental driver.
	 */
	ret = devm_request_threaded_irq(dev, p->irq, NULL, pmu_irq,
					IRQF_ONESHOT | IRQF_NO_SUSPEND | IRQF_NO_AUTOEN,
					"apple-pmu-rtc", p);
	if (ret) {
		device_init_wakeup(dev, false);
		return ret;
	}
	ret = pmu_disarm(p);
	if (ret)
		goto err_wakeup;
	ret = devm_rtc_register_device(p->rtc);
	if (ret)
		goto err_wakeup;
	ret = devm_add_action_or_reset(dev, pmu_cleanup, p);
	if (ret)
		return ret;
	p->irq_enabled = true;
	enable_irq(p->irq);
	dev_info(dev, "experimental retained PMU RTC ready, irq=%d\n", p->irq);
	return 0;

err_wakeup:
	device_init_wakeup(dev, false);
	return ret;
}

static const struct of_device_id pmu_match[] = {
	{ .compatible = "apple,j700-pmu-rtc" },
	{}
};
MODULE_DEVICE_TABLE(of, pmu_match);

static struct platform_driver pmu_driver = {
	.probe = pmu_probe,
	.driver = {
		.name = "apple-pmu-rtc",
		.of_match_table = pmu_match,
		.pm = &pmu_pm_ops,
	},
};
module_platform_driver(pmu_driver);

MODULE_DESCRIPTION("Experimental Apple J700 retained-power PMU RTC");
MODULE_LICENSE("GPL");
