// SPDX-License-Identifier: BSD-3-Clause-Clear
/*
 * Copyright (C) 2016 Felix Fietkau <nbd@nbd.name>
 */

#include "mt76.h"
#include "dma.h"
#include "trace.h"

static u32 mt76_mmio_rr(struct mt76_dev *dev, u32 offset)
{
	u32 val;

	val = readl(dev->mmio.regs + offset);
	trace_reg_rr(dev, offset, val);

	return val;
}

static void mt76_mmio_wr(struct mt76_dev *dev, u32 offset, u32 val)
{
	trace_reg_wr(dev, offset, val);
	mt76_mmio_j700_reg_write(dev, offset, val);
	writel(val, dev->mmio.regs + offset);
}

static u32 mt76_mmio_rmw(struct mt76_dev *dev, u32 offset, u32 mask, u32 val)
{
	val |= mt76_mmio_rr(dev, offset) & ~mask;
	mt76_mmio_wr(dev, offset, val);
	return val;
}

static void mt76_mmio_write_copy(struct mt76_dev *dev, u32 offset,
				 const void *data, int len)
{
	int i;

	for (i = 0; i < ALIGN(len, 4); i += 4)
		mt76_mmio_wr(dev, offset + i,
			     get_unaligned_le32(data + i));
}

int mt76_mmio_j700_reg_write_arm(struct mt76_dev *dev)
{
	unsigned long flags;

	if (!dev->j700_reg_writes) {
		dev->j700_reg_writes = devm_kcalloc(dev->dev,
						   MT76_J700_REG_WRITE_MAX,
						   sizeof(*dev->j700_reg_writes),
						   GFP_KERNEL);
		if (!dev->j700_reg_writes)
			return -ENOMEM;
	}

	spin_lock_irqsave(&dev->j700_reg_write_lock, flags);
	dev->j700_reg_write_count = 0;
	dev->j700_reg_write_overflow = false;
	dev->j700_reg_write_start_ns = ktime_get_mono_fast_ns();
	dev->j700_reg_write_active = true;
	spin_unlock_irqrestore(&dev->j700_reg_write_lock, flags);

	dev_info(dev->dev,
		 "J700_MT7932_REG_WRITE_TRACE_ARMED: capacity=%u console_inside=0 source=linux-mt76+queue\n",
		 MT76_J700_REG_WRITE_MAX);

	return 0;
}
EXPORT_SYMBOL_GPL(mt76_mmio_j700_reg_write_arm);

void mt76_mmio_j700_reg_write(struct mt76_dev *dev, u32 reg, u32 val)
{
	struct mt76_j700_reg_write *entry;
	unsigned long flags;

	if (!READ_ONCE(dev->j700_reg_write_active))
		return;

	spin_lock_irqsave(&dev->j700_reg_write_lock, flags);
	if (!dev->j700_reg_write_active)
		goto unlock;
	if (dev->j700_reg_write_count >= MT76_J700_REG_WRITE_MAX) {
		dev->j700_reg_write_overflow = true;
		goto unlock;
	}

	entry = &dev->j700_reg_writes[dev->j700_reg_write_count++];
	entry->delta_ns = ktime_get_mono_fast_ns() -
			  dev->j700_reg_write_start_ns;
	entry->reg = reg;
	entry->val = val;

unlock:
	spin_unlock_irqrestore(&dev->j700_reg_write_lock, flags);
}
EXPORT_SYMBOL_GPL(mt76_mmio_j700_reg_write);

void mt76_mmio_j700_reg_write_stop_dump(struct mt76_dev *dev,
					const char *reason)
{
	unsigned long flags;
	bool overflow;
	u32 count, i;

	spin_lock_irqsave(&dev->j700_reg_write_lock, flags);
	if (!dev->j700_reg_write_active) {
		spin_unlock_irqrestore(&dev->j700_reg_write_lock, flags);
		return;
	}
	dev->j700_reg_write_active = false;
	count = dev->j700_reg_write_count;
	overflow = dev->j700_reg_write_overflow;
	spin_unlock_irqrestore(&dev->j700_reg_write_lock, flags);

	dev_info(dev->dev,
		 "J700_MT7932_REG_WRITE_TRACE_BEGIN: reason=%s records=%u overflow=%u source=linux-mt76+queue\n",
		 reason, count, overflow);
	for (i = 0; i < count; i++) {
		const struct mt76_j700_reg_write *entry =
			&dev->j700_reg_writes[i];

		dev_info(dev->dev,
			 "J700_MT7932_REG_WRITE_TRACE: order=%u delta_ns=%llu offset=0x%08x value=0x%08x\n",
			 i + 1, (unsigned long long)entry->delta_ns,
			 entry->reg, entry->val);
	}
	dev_info(dev->dev,
		 "J700_MT7932_REG_WRITE_TRACE_END: reason=%s records=%u overflow=%u\n",
		 reason, count, overflow);
}
EXPORT_SYMBOL_GPL(mt76_mmio_j700_reg_write_stop_dump);

void mt76_mmio_j700_reg_write_disarm(struct mt76_dev *dev,
				     const char *reason)
{
	unsigned long flags;
	u32 count;

	spin_lock_irqsave(&dev->j700_reg_write_lock, flags);
	dev->j700_reg_write_active = false;
	count = dev->j700_reg_write_count;
	spin_unlock_irqrestore(&dev->j700_reg_write_lock, flags);

	dev_info(dev->dev,
		 "J700_MT7932_REG_WRITE_TRACE_DISARMED: reason=%s records=%u dump=0\n",
		 reason, count);
}
EXPORT_SYMBOL_GPL(mt76_mmio_j700_reg_write_disarm);

static void mt76_mmio_read_copy(struct mt76_dev *dev, u32 offset,
				void *data, int len)
{
	int i;

	for (i = 0; i < ALIGN(len, 4); i += 4)
		put_unaligned_le32(readl(dev->mmio.regs + offset + i),
				   data + i);
}

static int mt76_mmio_wr_rp(struct mt76_dev *dev, u32 base,
			   const struct mt76_reg_pair *data, int len)
{
	while (len > 0) {
		mt76_mmio_wr(dev, data->reg, data->value);
		data++;
		len--;
	}

	return 0;
}

static int mt76_mmio_rd_rp(struct mt76_dev *dev, u32 base,
			   struct mt76_reg_pair *data, int len)
{
	while (len > 0) {
		data->value = mt76_mmio_rr(dev, data->reg);
		data++;
		len--;
	}

	return 0;
}

void mt76_set_irq_mask(struct mt76_dev *dev, u32 addr,
		       u32 clear, u32 set)
{
	unsigned long flags;

	spin_lock_irqsave(&dev->mmio.irq_lock, flags);
	dev->mmio.irqmask &= ~clear;
	dev->mmio.irqmask |= set;
	if (addr) {
		if (mtk_wed_device_active(&dev->mmio.wed))
			mtk_wed_device_irq_set_mask(&dev->mmio.wed,
						    dev->mmio.irqmask);
		else
			mt76_mmio_wr(dev, addr, dev->mmio.irqmask);
	}
	spin_unlock_irqrestore(&dev->mmio.irq_lock, flags);
}
EXPORT_SYMBOL_GPL(mt76_set_irq_mask);

void mt76_mmio_init(struct mt76_dev *dev, void __iomem *regs)
{
	static const struct mt76_bus_ops mt76_mmio_ops = {
		.rr = mt76_mmio_rr,
		.rmw = mt76_mmio_rmw,
		.wr = mt76_mmio_wr,
		.write_copy = mt76_mmio_write_copy,
		.read_copy = mt76_mmio_read_copy,
		.wr_rp = mt76_mmio_wr_rp,
		.rd_rp = mt76_mmio_rd_rp,
		.type = MT76_BUS_MMIO,
	};

	dev->bus = &mt76_mmio_ops;
	dev->mmio.regs = regs;

	spin_lock_init(&dev->mmio.irq_lock);
	spin_lock_init(&dev->j700_reg_write_lock);
}
EXPORT_SYMBOL_GPL(mt76_mmio_init);
