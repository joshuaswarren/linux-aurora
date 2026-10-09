// SPDX-License-Identifier: GPL-2.0-only
/* Bounded T8140 APR diagnostic. This is not a V4L2 encoder driver. */
#include <linux/debugfs.h>
#include <linux/dma-mapping.h>
#include <linux/io.h>
#include <linux/iommu.h>
#include <linux/iopoll.h>
#include <linux/module.h>
#include <linux/mutex.h>
#include <linux/of.h>
#include <linux/platform_device.h>
#include <linux/pm_runtime.h>
#include <linux/seq_file.h>
#include <linux/sizes.h>
#include <linux/slab.h>
#include <linux/uaccess.h>
#include <linux/unaligned.h>

#define APR_RING_SIZE SZ_128K
#define APR_OUTPUT_SIZE SZ_4M
#define APR_PLANE_SIZE SZ_1M
#define APR_STATS_SIZE SZ_1M
#define APR_MAX_WRITES 2048
#define APR_PACKET_MAX (4 * SZ_1M)

struct apr_buffer { void *cpu; dma_addr_t dma; size_t size; };
struct apr_test {
	struct device *dev;
	void __iomem *regs;
	void __iomem *wrapper;
	struct dentry *debug;
	struct mutex lock;
	/* 0 output, 1..3 input planes, 4 statistics, 5 ring. */
	struct apr_buffer buffers[6];
	bool started, failed;
	u32 frames, irq, tail, output_bytes;
	int result;
};

/* APR1 header, then register pairs, descriptor, relocation triples, planes.
 * All words are little endian. Relocations select only our owned DMA buffers.
 */
struct apr_packet {
	__le32 magic, descriptor_bytes, writes, relocations;
	__le32 plane_bytes[3];
	__le32 reserved;
};

static bool apr_pointer_slot(u32 off)
{
	switch (off) {
	case 8: case 0x28: case 0x30: case 0x38: case 0x40: case 0x48:
	case 0x50: case 0x58: case 0x60: case 0x68: case 0x70:
		return true;
	default:
		return false;
	}
}

static ssize_t apr_submit(struct file *file, const char __user *user,
			  size_t count, loff_t *position)
{
	struct apr_test *apr = file->private_data;
	struct apr_packet *packet;
	u8 *raw, *desc, *relocs, *planes;
	u32 writes, size, reloc_count, plane_size[3], seen = 0, value, head;
	size_t needed;
	int ret = 0;
	bool version1;

	if (*position || count < sizeof(*packet) || count > APR_PACKET_MAX)
		return -EINVAL;
	raw = memdup_user(user, count);
	if (IS_ERR(raw))
		return PTR_ERR(raw);
	packet = (void *)raw;
	version1 = le32_to_cpu(packet->magic) == 0x31525041;
	size = le32_to_cpu(packet->descriptor_bytes);
	writes = le32_to_cpu(packet->writes);
	reloc_count = le32_to_cpu(packet->relocations);
	if ((!version1 && le32_to_cpu(packet->magic) != 0x30525041) || packet->reserved ||
	    size < 0x180 || size > 0x400 || (size & 0x1f) ||
	    writes > APR_MAX_WRITES || reloc_count > 11) {
		ret = -EINVAL;
		goto free;
	}
	needed = sizeof(*packet) + writes * 8 + size + reloc_count * 12;
	for (unsigned int i = 0; i < 3; i++) {
		plane_size[i] = le32_to_cpu(packet->plane_bytes[i]);
		if (plane_size[i] > APR_PLANE_SIZE) { ret = -EINVAL; goto free; }
		needed += plane_size[i];
	}
	if (needed != count) { ret = -EINVAL; goto free; }
	desc = raw + sizeof(*packet) + writes * 8;
	relocs = desc + size;
	planes = relocs + reloc_count * 12;
	/* This experiment is restricted to the captured 320x240 v1 descriptor.
	 * The frame header moved from 0x58 to 0x7c in this version. */
	if (get_unaligned_le16(desc + 0x18) != 320 ||
	    get_unaligned_le16(desc + 0x1a) != 240 ||
	    get_unaligned_be16(desc + (version1 ? 0x84 : 0x60)) != 320 ||
	    get_unaligned_be16(desc + (version1 ? 0x86 : 0x62)) != 240) {
		ret = -EINVAL;
		goto free;
	}
	for (u32 off = 8; off <= (version1 ? 0x70 : 0x50); off += 8)
		if (apr_pointer_slot(off) && get_unaligned_le64(desc + off)) {
			ret = -EINVAL; goto free;
		}
	for (unsigned int i = 0; i < writes; i++) {
		u32 off = get_unaligned_le32(raw + sizeof(*packet) + i * 8);
		/* Only the existing APR register map and quantization tables. */
		if ((off & 3) || (off > 0x19fc && (off < 0x10000 || off > 0x17ffc))) {
			ret = -EINVAL; goto free;
		}
		if (off == 0x10c && get_unaligned_le32(raw + sizeof(*packet) + i * 8 + 4)) {
			ret = -EINVAL; goto free;
		}
	}
	for (unsigned int i = 0; i < reloc_count; i++) {
		u32 off = get_unaligned_le32(relocs + i * 12);
		u32 index = get_unaligned_le32(relocs + i * 12 + 4);
		u32 delta = get_unaligned_le32(relocs + i * 12 + 8);
		if (!apr_pointer_slot(off) || (!version1 && off > 0x50) || index >= 5 ||
		    delta >= apr->buffers[index].size || (seen & BIT(off / 8))) {
			ret = -EINVAL; goto free;
		}
		if ((off == 8 && index != 0) || (off >= 0x58 && index != 4) ||
		    (off >= 0x28 && off <= 0x50 && (index < 1 || index > 3))) {
			ret = -EINVAL; goto free;
		}
		seen |= BIT(off / 8);
		put_unaligned_le64(apr->buffers[index].dma + delta, desc + off);
	}
	if (!(seen & BIT(1)) || !(seen & BIT(0x28 / 8)) || !plane_size[0]) {
		ret = -EINVAL; goto free;
	}
	mutex_lock(&apr->lock);
	if (apr->failed || (apr->started && writes) || (!apr->started && !writes)) {
		ret = -EBUSY; goto unlock;
	}
	head = (apr->frames + 1) * size;
	if (head > APR_RING_SIZE) { ret = -ENOSPC; goto unlock; }
	for (unsigned int i = 0; i < ARRAY_SIZE(apr->buffers); i++)
		memset(apr->buffers[i].cpu, 0, apr->buffers[i].size);
	for (unsigned int i = 0; i < 3; i++) {
		memcpy(apr->buffers[i + 1].cpu, planes, plane_size[i]);
		planes += plane_size[i];
	}
	put_unaligned_le32(APR_OUTPUT_SIZE, desc + 0x10);
	memcpy(apr->buffers[5].cpu + head - size, desc, size);
	if (!apr->started) {
		/* Once registers can launch DMA, pin all resources until reboot,
		 * including after a timeout. Never unload an uncertain engine. */
		__module_get(THIS_MODULE);
		apr->started = true;
		for (unsigned int i = 0; i < writes; i++) {
			u32 off = get_unaligned_le32(raw + sizeof(*packet) + i * 8);
			u32 data = get_unaligned_le32(raw + sizeof(*packet) + i * 8 + 4);
			if (off >= 0x10000) {
				writel(data, apr->wrapper + off - 0x10000);
				continue;
			}
			if (off == 0xc) data = 0; /* Poll; do not enable an unhandled IRQ. */
			/* Preserve the observed initialization order: the ring must be
			 * installed before MODE enables descriptor execution. */
			if (off == 0x100) data = APR_RING_SIZE;
			if (off == 0x104) data = lower_32_bits(apr->buffers[5].dma);
			if (off == 0x108) data = upper_32_bits(apr->buffers[5].dma);
			if (off == 0x10c || off == 0x110) data = 0;
			writel(data, apr->regs + off);
		}
	}
	writel(0, apr->regs + 0xc);
	writel(readl(apr->regs + 0x10), apr->regs + 0x10);
	dma_wmb();
	writel(head, apr->regs + 0x10c);
	ret = readl_poll_timeout(apr->regs + 0x10, value, value != 0, 100, 5000000);
	/* An interrupt status, completed ring entry and valid frame are required. */
	dma_rmb();
	apr->tail = readl(apr->regs + 0x110);
	apr->irq = value;
	apr->output_bytes = get_unaligned_be32(apr->buffers[0].cpu);
	if (!ret && (value != 1 || (apr->tail & 0xffffff) != head ||
		    apr->output_bytes < 32 || apr->output_bytes > APR_OUTPUT_SIZE ||
		    memcmp(apr->buffers[0].cpu + 4, "icpf", 4)))
		ret = -EIO;
	apr->result = ret;
	apr->failed = ret != 0;
	if (!ret) apr->frames++;
	dev_info(apr->dev, "APR_TEST result=%d frames=%u tail=%#x irq=%#x bytes=%u\n",
		 ret, apr->frames, apr->tail, apr->irq, apr->output_bytes);
unlock:
	mutex_unlock(&apr->lock);
free:
	kfree(raw);
	return ret ? ret : count;
}

static ssize_t apr_output(struct file *file, char __user *user, size_t count, loff_t *pos)
{
	struct apr_test *apr = file->private_data;
	ssize_t ret;
	mutex_lock(&apr->lock);
	ret = !apr->frames || apr->failed ? -ENODATA :
		simple_read_from_buffer(user, count, pos, apr->buffers[0].cpu, apr->output_bytes);
	mutex_unlock(&apr->lock);
	return ret;
}

static int apr_status_show(struct seq_file *s, void *unused)
{
	struct apr_test *apr = s->private;
	mutex_lock(&apr->lock);
	seq_printf(s, "started=%u\nfailed=%u\nresult=%d\nframes=%u\ntail=%#x\nirq=%#x\nbytes=%u\n",
		apr->started, apr->failed, apr->result, apr->frames, apr->tail, apr->irq, apr->output_bytes);
	for (unsigned int i = 0; i < ARRAY_SIZE(apr->buffers); i++)
		seq_printf(s, "buffer%u=%pad size=%zu\n", i, &apr->buffers[i].dma, apr->buffers[i].size);
	mutex_unlock(&apr->lock);
	return 0;
}
DEFINE_SHOW_ATTRIBUTE(apr_status);
static const struct file_operations apr_submit_fops = {
	.owner = THIS_MODULE, .open = simple_open, .write = apr_submit,
};
static const struct file_operations apr_output_fops = {
	.owner = THIS_MODULE, .open = simple_open, .read = apr_output, .llseek = default_llseek,
};

static void apr_free(struct apr_test *apr)
{
	for (unsigned int i = 0; i < ARRAY_SIZE(apr->buffers); i++)
		if (apr->buffers[i].cpu)
			dma_free_coherent(apr->dev, apr->buffers[i].size,
				apr->buffers[i].cpu, apr->buffers[i].dma);
}

static int apr_probe(struct platform_device *pdev)
{
	struct device *dev = &pdev->dev;
	struct apr_test *apr;
	struct iommu_domain *domain;
	int ret;
	if (!of_machine_is_compatible("apple,t8140") || PAGE_SIZE != SZ_16K) return -ENODEV;
	domain = iommu_get_domain_for_dev(dev);
	if (!domain || !(domain->type & __IOMMU_DOMAIN_PAGING)) return -ENODEV;
	apr = devm_kzalloc(dev, sizeof(*apr), GFP_KERNEL);
	if (!apr) return -ENOMEM;
	apr->dev = dev;
	mutex_init(&apr->lock);
	apr->regs = devm_platform_ioremap_resource(pdev, 0);
	if (IS_ERR(apr->regs)) return PTR_ERR(apr->regs);
	apr->wrapper = devm_platform_ioremap_resource(pdev, 1);
	if (IS_ERR(apr->wrapper)) return PTR_ERR(apr->wrapper);
	ret = dma_set_mask_and_coherent(dev, DMA_BIT_MASK(64));
	if (ret) return ret;
	ret = devm_pm_runtime_enable(dev);
	if (ret) return ret;
	ret = pm_runtime_resume_and_get(dev);
	if (ret < 0) return ret;
	for (unsigned int i = 0; i < ARRAY_SIZE(apr->buffers); i++) {
		struct apr_buffer *buffer = &apr->buffers[i];
		buffer->size = i == 0 ? APR_OUTPUT_SIZE : i == 5 ? APR_RING_SIZE : APR_PLANE_SIZE;
		buffer->cpu = dma_alloc_coherent(dev, buffer->size, &buffer->dma, GFP_KERNEL);
		if (!buffer->cpu) { ret = -ENOMEM; goto free; }
		for (size_t offset = 0; offset < buffer->size; offset += PAGE_SIZE)
			if (!iommu_iova_to_phys(domain, buffer->dma + offset)) {
				ret = -EFAULT; goto free;
			}
	}
	platform_set_drvdata(pdev, apr);
	apr->debug = debugfs_create_dir("apple-prores-test", NULL);
	debugfs_create_file("status", 0400, apr->debug, apr, &apr_status_fops);
	debugfs_create_file("submit", 0200, apr->debug, apr, &apr_submit_fops);
	debugfs_create_file("output", 0400, apr->debug, apr, &apr_output_fops);
	dev_info(dev, "APR_TEST_READY translated DMA, idle until submit\n");
	return 0;
free:
	apr_free(apr);
	pm_runtime_put_sync(dev);
	return ret;
}

static void apr_remove(struct platform_device *pdev)
{
	struct apr_test *apr = platform_get_drvdata(pdev);
	if (WARN_ON(apr->started)) return;
	debugfs_remove_recursive(apr->debug);
	apr_free(apr);
	pm_runtime_put_sync(apr->dev);
}
static const struct of_device_id apr_match[] = {
	{ .compatible = "apple,t8140-prores-test" }, { }
};
MODULE_DEVICE_TABLE(of, apr_match);
static struct platform_driver apr_driver = {
	.probe = apr_probe, .remove = apr_remove,
	.driver = { .name = "apple-prores-test", .of_match_table = apr_match, .suppress_bind_attrs = true },
};
module_platform_driver(apr_driver);
MODULE_DESCRIPTION("Bounded Apple T8140 ProRes hardware experiment");
MODULE_LICENSE("GPL");
