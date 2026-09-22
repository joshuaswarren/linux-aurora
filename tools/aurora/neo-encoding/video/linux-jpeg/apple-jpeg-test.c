// SPDX-License-Identifier: GPL-2.0-only
/* T8140 JPEG diagnostic, dedicated translated DMA, no standard media ABI yet. */
#include <linux/debugfs.h>
#include <linux/dma-mapping.h>
#include <linux/io.h>
#include <linux/iommu.h>
#include <linux/interrupt.h>
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

#define JPG_BUFFER_SIZE SZ_1M
#define JPG_MAX_WRITES 2048
struct jpg_test {
    struct device *dev;
    void __iomem *regs;
    void *input, *output;
    dma_addr_t input_dma, output_dma;
    struct dentry *debug;
    struct mutex lock;
    bool started, failed;
    u32 frames, status, irq, bytes;
    int result;
};
/* JPG1: header, offset/value pairs, tight 320x240 NV12 input. */
struct jpg_packet { __le32 magic, writes, input_bytes, chroma_offset, output_bytes, reserved; };

static irqreturn_t jpg_irq(int irq, void *opaque)
{
    /* IRQF_NO_AUTOEN reserves this line while leaving the AIC masked. */
    return IRQ_NONE;
}

static bool jpg_address(u32 offset)
{
    return (offset >= 0x90 && offset <= 0xe4 && (offset & 0xf) <= 4);
}

static u32 jpg_address_value(struct jpg_test *jpg, u32 offset, u32 input_size, u32 chroma, u32 output_size)
{
    dma_addr_t addr;
    switch (offset & ~0xf) {
    case 0x90: addr = jpg->input_dma; break;
    case 0xa0: addr = jpg->input_dma + chroma; break;
    case 0xb0: addr = jpg->input_dma + input_size - 1; break;
    case 0xc0: case 0xd0: addr = jpg->output_dma; break;
    default: addr = jpg->output_dma + output_size - 1; break;
    }
    return offset & 4 ? upper_32_bits(addr) : lower_32_bits(addr);
}

static ssize_t jpg_submit(struct file *file, const char __user *user, size_t count, loff_t *pos)
{
    struct jpg_test *jpg = file->private_data;
    struct jpg_packet *packet;
    u8 *raw, *pairs;
    u32 writes, input_size, chroma, output_size, value = 0, address_seen = 0;
    bool width = false, height = false;
    int ret;
    if (*pos || count < sizeof(*packet) || count > JPG_BUFFER_SIZE + JPG_MAX_WRITES*8 + sizeof(*packet)) return -EINVAL;
    raw = memdup_user(user, count);
    if (IS_ERR(raw)) return PTR_ERR(raw);
    packet = (void *)raw; pairs = raw + sizeof(*packet);
    writes = le32_to_cpu(packet->writes); input_size = le32_to_cpu(packet->input_bytes);
    chroma = le32_to_cpu(packet->chroma_offset); output_size = le32_to_cpu(packet->output_bytes);
    ret = -EINVAL;
    if (le32_to_cpu(packet->magic) != 0x3147504a || packet->reserved || !writes || writes > JPG_MAX_WRITES ||
        input_size != 320*240*3/2 || chroma != 320*240 || !output_size || output_size > JPG_BUFFER_SIZE ||
        count != sizeof(*packet) + writes*8 + input_size) goto free;
    for (u32 i = 0; i < writes; i++) {
        u32 off = get_unaligned_le32(pairs+i*8), data = get_unaligned_le32(pairs+i*8+4);
        if ((off & 3) || off >= 0x4000 || (off == 0x1004 && data)) goto free;
        if (jpg_address(off)) {
            if (data) goto free; /* Captured addresses must already be erased. */
            address_seen |= BIT(((off-0x90)/0x10)*2+((off&4)?1:0));
        }
        if (off == 0x101c) { if (data != 320) goto free; width = true; }
        if (off == 0x1018) { if (data != 240) goto free; height = true; }
    }
    if (address_seen != 0xfff || !width || !height) goto free;
    mutex_lock(&jpg->lock);
    if (jpg->failed) { ret = -EBUSY; goto unlock; }
    if (!jpg->started) { __module_get(THIS_MODULE); jpg->started = true; }
    jpg->status = 0; jpg->irq = 0;
    memset(jpg->input, 0, JPG_BUFFER_SIZE); memset(jpg->output, 0, JPG_BUFFER_SIZE);
    memcpy(jpg->input, pairs+writes*8, input_size);
    for (u32 i = 0; i < writes; i++) {
        u32 off = get_unaligned_le32(pairs+i*8), data = get_unaligned_le32(pairs+i*8+4);
        if (jpg_address(off)) data = jpg_address_value(jpg,off,input_size,chroma,output_size);
        writel(data,jpg->regs+off);
    }
    dma_wmb();
    writel(1,jpg->regs+0x1004);
    ret = readl_poll_timeout(jpg->regs+0x102c,value,value & BIT(31),100,5000000);
    dma_rmb();
    jpg->irq = value;
    jpg->status = readl(jpg->regs+0x24);
    jpg->bytes = readl(jpg->regs+0x1020);
    if (!ret && (jpg->status != 1 || jpg->bytes < 4 || jpg->bytes > output_size ||
        ((u8 *)jpg->output)[0] != 0xff || ((u8 *)jpg->output)[1] != 0xd8 ||
        ((u8 *)jpg->output)[jpg->bytes-2] != 0xff || ((u8 *)jpg->output)[jpg->bytes-1] != 0xd9)) ret = -EIO;
    if (!ret) writel(0,jpg->regs+0x102c);
    jpg->result = ret; jpg->failed = ret != 0;
    if (!ret) jpg->frames++;
    dev_info(jpg->dev,"JPEG_TEST result=%d frames=%u status=%#x irq=%#x bytes=%u\n",ret,jpg->frames,jpg->status,jpg->irq,jpg->bytes);
unlock:
    mutex_unlock(&jpg->lock);
free:
    kfree(raw);
    return ret ? ret : count;
}

static ssize_t jpg_output(struct file *file, char __user *user, size_t count, loff_t *pos)
{
    struct jpg_test *jpg = file->private_data;
    ssize_t ret;
    mutex_lock(&jpg->lock);
    ret = !jpg->frames || jpg->failed ? -ENODATA : simple_read_from_buffer(user,count,pos,jpg->output,jpg->bytes);
    mutex_unlock(&jpg->lock);
    return ret;
}
static int jpg_status_show(struct seq_file *s, void *unused)
{
    struct jpg_test *jpg = s->private;
    mutex_lock(&jpg->lock);
    seq_printf(s,"started=%u\nfailed=%u\nresult=%d\nframes=%u\nstatus=%#x\nirq=%#x\nbytes=%u\ninput=%pad\noutput=%pad\n",
        jpg->started,jpg->failed,jpg->result,jpg->frames,jpg->status,jpg->irq,jpg->bytes,&jpg->input_dma,&jpg->output_dma);
    mutex_unlock(&jpg->lock);
    return 0;
}
DEFINE_SHOW_ATTRIBUTE(jpg_status);
static const struct file_operations jpg_submit_fops = {.owner=THIS_MODULE,.open=simple_open,.write=jpg_submit};
static const struct file_operations jpg_output_fops = {.owner=THIS_MODULE,.open=simple_open,.read=jpg_output,.llseek=default_llseek};
static void jpg_free(struct jpg_test *jpg)
{
    if (jpg->input) dma_free_coherent(jpg->dev,JPG_BUFFER_SIZE,jpg->input,jpg->input_dma);
    if (jpg->output) dma_free_coherent(jpg->dev,JPG_BUFFER_SIZE,jpg->output,jpg->output_dma);
}
static int jpg_probe(struct platform_device *pdev)
{
    struct device *dev = &pdev->dev;
    struct iommu_domain *domain = iommu_get_domain_for_dev(dev);
    struct jpg_test *jpg;
    int ret;
    if (!of_machine_is_compatible("apple,t8140") || PAGE_SIZE != SZ_16K ||
        !domain || !(domain->type & __IOMMU_DOMAIN_PAGING)) return -ENODEV;
    jpg = devm_kzalloc(dev,sizeof(*jpg),GFP_KERNEL);
    if (!jpg) return -ENOMEM;
    jpg->dev = dev; mutex_init(&jpg->lock);
    jpg->regs = devm_platform_ioremap_resource(pdev,0);
    if (IS_ERR(jpg->regs)) return PTR_ERR(jpg->regs);
    ret = dma_set_mask_and_coherent(dev,DMA_BIT_MASK(64)); if (ret) return ret;
    ret = devm_pm_runtime_enable(dev); if (ret) return ret;
    ret = pm_runtime_resume_and_get(dev); if (ret < 0) return ret;
    jpg->input = dma_alloc_coherent(dev,JPG_BUFFER_SIZE,&jpg->input_dma,GFP_KERNEL);
    jpg->output = dma_alloc_coherent(dev,JPG_BUFFER_SIZE,&jpg->output_dma,GFP_KERNEL);
    ret = -ENOMEM; if (!jpg->input || !jpg->output) goto free;
    for (size_t off = 0; off < JPG_BUFFER_SIZE; off += PAGE_SIZE)
        if (!iommu_iova_to_phys(domain,jpg->input_dma+off) || !iommu_iova_to_phys(domain,jpg->output_dma+off)) {
            ret = -EFAULT; goto free;
        }
    ret = platform_get_irq(pdev,0);
    if (ret < 0) goto free;
    ret = devm_request_irq(dev,ret,jpg_irq,IRQF_NO_AUTOEN,dev_name(dev),jpg);
    if (ret) goto free;
    platform_set_drvdata(pdev,jpg);
    jpg->debug = debugfs_create_dir("apple-jpeg-test",NULL);
    debugfs_create_file("status",0400,jpg->debug,jpg,&jpg_status_fops);
    debugfs_create_file("submit",0200,jpg->debug,jpg,&jpg_submit_fops);
    debugfs_create_file("output",0400,jpg->debug,jpg,&jpg_output_fops);
    dev_info(dev,"JPEG_TEST_READY translated DMA; idle until submit\n");
    return 0;
free:
    jpg_free(jpg); pm_runtime_put_sync(dev); return ret;
}
static void jpg_remove(struct platform_device *pdev)
{
    struct jpg_test *jpg = platform_get_drvdata(pdev);
    if (WARN_ON(jpg->started)) return;
    debugfs_remove_recursive(jpg->debug); jpg_free(jpg); pm_runtime_put_sync(jpg->dev);
}
static const struct of_device_id jpg_match[] = {{.compatible="apple,t8140-jpeg-test"},{}};
MODULE_DEVICE_TABLE(of,jpg_match);
static struct platform_driver jpg_driver = {.probe=jpg_probe,.remove=jpg_remove,
    .driver={.name="apple-jpeg-test",.of_match_table=jpg_match,.suppress_bind_attrs=true}};
module_platform_driver(jpg_driver);
MODULE_DESCRIPTION("Bounded Apple T8140 JPEG hardware diagnostic");
MODULE_LICENSE("GPL");
