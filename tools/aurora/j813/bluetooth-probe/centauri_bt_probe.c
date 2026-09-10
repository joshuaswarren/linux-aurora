// SPDX-License-Identifier: GPL-2.0-only
/* Bounded N1 Beta transport probe. The existing Control driver boots firmware.
 * This separate module owns only function 2 and never resets the PCIe port.
 * It does not register an HCI device yet: first qualify Read Local Version.
 */
#include <linux/debugfs.h>
#include <linux/delay.h>
#include <linux/dma-mapping.h>
#include <linux/io.h>
#include <linux/iopoll.h>
#include <linux/module.h>
#include <linux/mutex.h>
#include <linux/of.h>
#include <linux/pci.h>
#include <linux/seq_file.h>
#include <linux/sizes.h>
#include <linux/unaligned.h>

#define BT_ARENA_SIZE SZ_1M
#define BT_MTR 0x2000
#define BT_MCR 0x2200
#define BT_MESSAGES 0x4000
#define BT_RX 0x8000
#define BT_TX 0xa000
#define BT_FOOTER 260
#define BT_STRIDE (16 + BT_FOOTER)
#define BT_RX_SLOTS 17
#define BT_TX_SLOTS 5

struct cen_bt_probe {
	struct pci_dev *pdev;
	void __iomem *bar;
	u8 *arena;
	dma_addr_t dma;
	struct dentry *debug;
	struct mutex lock;
	u16 index[4];
	u16 message_head, tx_head, rx_head, rx_post;
	u16 event_length;
	u8 event[BT_FOOTER];
	u8 last_mcr[32];
	bool published, ready, version_valid;
	int last_result;
};

static u16 bt_index(struct cen_bt_probe *bt, unsigned int array, u16 ring)
{
	return le16_to_cpu(READ_ONCE(*(__le16 *)(bt->arena + bt->index[array] + ring * 2)));
}

static void bt_post(struct cen_bt_probe *bt, u16 ring, u16 value)
{
	put_unaligned_le16(value, bt->arena + bt->index[3] + ring * 2);
	dma_wmb();
	writel(value, bt->bar + 0x1000 + ring * 4);
}

static int bt_message(struct cen_bt_probe *bt, const void *data, size_t length)
{
	u16 head = bt->message_head, tail;
	u8 *descriptor = bt->arena + BT_MTR + head * 16;
	int ret;

	if (head >= 15 || !length || length > 512)
		return -ENOSPC;
	memcpy(bt->arena + BT_MESSAGES + head * 512, data, length);
	put_unaligned_le32(1 | (length << 8), descriptor);
	put_unaligned_le64(bt->dma + BT_MESSAGES + head * 512, descriptor + 4);
	put_unaligned_le16(head + 1, descriptor + 12);
	bt_post(bt, 0, head + 1);
	ret = read_poll_timeout(bt_index, tail, tail == head + 1,
		100, 1500000, false, bt, 1, 0);
	dma_rmb();
	memcpy(bt->last_mcr, bt->arena + BT_MCR + head * 32, 32);
	if (!ret)
		bt->message_head++;
	dev_info(&bt->pdev->dev, "Beta message op=%u consumed=%u expected=%u rc=%d completion=%*ph\n",
		 *(const u8 *)data, tail, head + 1, ret, 32, bt->last_mcr);
	return ret;
}

static int bt_open_ring(struct cen_bt_probe *bt, u16 id, u16 slots, u32 offset)
{
	u8 message[0x34] = { 1 };

	/* Native in-place rings carry the packet in a 260-byte footer.
	 * The ACIPC header/footer scaling erratum encodes 65 four-byte words.
	 */
	message[2] = BT_FOOTER / 4;
	put_unaligned_le16(id, message + 4);
	put_unaligned_le64(bt->dma + offset, message + 8);
	put_unaligned_le64(~0ULL, message + 0x10);
	put_unaligned_le16(slots, message + 0x18);
	put_unaligned_le16(0xffff, message + 0x1a);
	put_unaligned_le16(id, message + 0x1c);
	put_unaligned_le16(0x40, message + 0x1e);
	return bt_message(bt, message, sizeof(message));
}

static void bt_rx_buffer(struct cen_bt_probe *bt, u16 slot)
{
	u8 *descriptor = bt->arena + BT_RX + slot * BT_STRIDE;

	memset(descriptor, 0, BT_STRIDE);
	put_unaligned_le32(1 | (BT_FOOTER << 8), descriptor);
	put_unaligned_le16(slot + 1, descriptor + 12);
}

static int bt_start(struct cen_bt_probe *bt)
{
	u8 config[4] = { 9, 0, 0xff, 0xff };
	u32 value;
	int i, ret;

	if (bt->published || readl(bt->bar + 0x18) != 2 || readl(bt->bar) != 0)
		return -EBUSY;
	bt->index[0] = 0x78;
	bt->index[1] = 0x7a;
	bt->index[2] = 0xa6;
	bt->index[3] = 0xa8;
	put_unaligned_le16(0x300, bt->arena);
	put_unaligned_le16(0x68, bt->arena + 2);
	put_unaligned_le64(bt->dma + 0x68, bt->arena + 8);
	for (i = 0; i < 4; i++)
		put_unaligned_le64(bt->dma + bt->index[i], bt->arena + 0x10 + i * 8);
	put_unaligned_le16(1, bt->arena + 0x30);
	put_unaligned_le16(22, bt->arena + 0x32);
	put_unaligned_le64(bt->dma + BT_MCR, bt->arena + 0x34);
	put_unaligned_le64(bt->dma + BT_MTR, bt->arena + 0x3c);
	put_unaligned_le16(16, bt->arena + 0x44);
	put_unaligned_le16(16, bt->arena + 0x46);
	/* Exact 25G76 setup_main_ipc_stage writes MCR footer words at 0x53. */
	bt->arena[0x53] = 16 / 4;
	put_unaligned_le64(bt->dma + 0x10000, bt->arena + 0x58);
	pci_set_master(bt->pdev);
	writel(lower_32_bits(bt->dma), bt->bar + 0xc);
	writel(upper_32_bits(bt->dma), bt->bar + 0x10);
	writel(BT_ARENA_SIZE, bt->bar + 0x14);
	writel(1, bt->bar + 0x107c);
	ret = readl_poll_timeout(bt->bar, value, value == 1, 100, 2000000);
	if (ret)
		return ret;
	dma_wmb();
	bt->published = true;
	writel(lower_32_bits(bt->dma), bt->bar + 4);
	writel(upper_32_bits(bt->dma), bt->bar + 8);
	writel(2, bt->bar + 0x107c);
	ret = readl_poll_timeout(bt->bar, value, value == 2, 100, 2000000);
	if (ret)
		return ret;
	dev_info(&bt->pdev->dev, "Beta IPC handshake complete\n");
	ret = bt_message(bt, config, sizeof(config));
	if (!ret)
		ret = bt_open_ring(bt, 7, BT_RX_SLOTS, BT_RX);
	if (!ret)
		ret = bt_open_ring(bt, 1, BT_TX_SLOTS, BT_TX);
	if (ret)
		return ret;
	for (i = 0; i < BT_RX_SLOTS - 1; i++)
		bt_rx_buffer(bt, i);
	bt->rx_post = BT_RX_SLOTS - 1;
	bt_post(bt, 7, bt->rx_post);
	bt->ready = true;
	return 0;
}

static int bt_read_version(struct cen_bt_probe *bt)
{
	u8 *descriptor;
	u16 tail;
	u32 length;
	unsigned long deadline;

	if (!bt->ready || bt->tx_head >= BT_TX_SLOTS - 1)
		return -EBUSY;
	bt->version_valid = false;
	bt->event_length = 0;
	descriptor = bt->arena + BT_TX + bt->tx_head * BT_STRIDE;
	memset(descriptor, 0, BT_STRIDE);
	put_unaligned_le32(1 | (3 << 8), descriptor);
	put_unaligned_le16(bt->tx_head + 1, descriptor + 12);
	/* HCI Read Local Version Information, opcode 0x1001, no parameters. */
	put_unaligned_le16(0x1001, descriptor + 16);
	bt->tx_head++;
	bt_post(bt, 1, bt->tx_head);
	deadline = jiffies + msecs_to_jiffies(3000);
	do {
		tail = bt_index(bt, 1, 7);
		if (tail >= BT_RX_SLOTS)
			return -EPROTO;
		if (tail != bt->rx_head) {
			dma_rmb();
			descriptor = bt->arena + BT_RX + bt->rx_head * BT_STRIDE;
			length = get_unaligned_le32(descriptor) >> 8;
			if (length < 2 || length > BT_FOOTER)
				return -EPROTO;
			memcpy(bt->event, descriptor + 16, length);
			bt->event_length = length;
			bt->rx_head = (bt->rx_head + 1) % BT_RX_SLOTS;
			bt_rx_buffer(bt, bt->rx_post);
			bt->rx_post = (bt->rx_post + 1) % BT_RX_SLOTS;
			bt_post(bt, 7, bt->rx_post);
			dev_info(&bt->pdev->dev, "Beta HCI event length=%u header=%*ph\n",
				 length, min_t(u32, length, 20), bt->event);
			if (length == 14 && bt->event[0] == 0x0e && bt->event[1] == 12 &&
			    get_unaligned_le16(bt->event + 3) == 0x1001) {
				bt->version_valid = bt->event[5] == 0;
				return bt->version_valid ? 0 : -EREMOTEIO;
			}
		}
		usleep_range(1000, 2000);
	} while (time_before(jiffies, deadline));
	return -ETIMEDOUT;
}

static int bt_status_show(struct seq_file *s, void *unused)
{
	struct cen_bt_probe *bt = s->private;

	mutex_lock(&bt->lock);
	seq_printf(s, "exec_stage=%u ipc_status=%u published=%u ready=%u result=%d version_valid=%u\n",
		   readl(bt->bar + 0x18), readl(bt->bar), bt->published, bt->ready,
		   bt->last_result, bt->version_valid);
	if (bt->published)
		seq_printf(s, "messages=%u tx_post=%u tx_consumed=%u rx_consumed=%u rx_head=%u event_length=%u\n",
			   bt->message_head, bt->tx_head, bt_index(bt, 1, 1),
			   bt_index(bt, 1, 7), bt->rx_head, bt->event_length);
	if (bt->event_length)
		seq_printf(s, "event=%*ph\n", min_t(u16, bt->event_length, 20), bt->event);
	mutex_unlock(&bt->lock);
	return 0;
}
DEFINE_SHOW_ATTRIBUTE(bt_status);

static ssize_t bt_probe_write(struct file *file, const char __user *buffer,
			      size_t count, loff_t *pos)
{
	struct cen_bt_probe *bt = file->private_data;
	u32 action;
	int ret = kstrtou32_from_user(buffer, count, 0, &action);

	if (ret || action < 1 || action > 2)
		return ret ?: -EINVAL;
	mutex_lock(&bt->lock);
	ret = action == 1 ? bt_start(bt) : bt_read_version(bt);
	bt->last_result = ret;
	mutex_unlock(&bt->lock);
	dev_info(&bt->pdev->dev, "Beta bounded probe action=%u rc=%d\n", action, ret);
	return ret ?: count;
}

static const struct file_operations bt_probe_fops = {
	.owner = THIS_MODULE,
	.open = simple_open,
	.write = bt_probe_write,
};

static int bt_probe(struct pci_dev *pdev, const struct pci_device_id *id)
{
	struct cen_bt_probe *bt;
	int ret;

	if (!of_machine_is_compatible("apple,j813") || PCI_FUNC(pdev->devfn) != 2)
		return -ENODEV;
	bt = devm_kzalloc(&pdev->dev, sizeof(*bt), GFP_KERNEL);
	if (!bt)
		return -ENOMEM;
	bt->pdev = pdev;
	mutex_init(&bt->lock);
	ret = pcim_enable_device(pdev);
	if (ret)
		return ret;
	if (pci_resource_len(pdev, 0) != SZ_64K)
		return -ENODEV;
	ret = pcim_iomap_regions(pdev, BIT(0), "apple_centauri_bt_probe");
	if (ret)
		return ret;
	bt->bar = pcim_iomap_table(pdev)[0];
	if (readl(bt->bar + 0x18) != 2 || readl(bt->bar) != 0)
		return dev_err_probe(&pdev->dev, -EBUSY, "Beta must have running firmware and unowned IPC\n");
	ret = dma_set_mask_and_coherent(&pdev->dev, DMA_BIT_MASK(40));
	if (ret)
		return ret;
	bt->arena = dma_alloc_coherent(&pdev->dev, BT_ARENA_SIZE, &bt->dma, GFP_KERNEL);
	if (!bt->arena)
		return -ENOMEM;
	memset(bt->arena, 0, BT_ARENA_SIZE);
	pci_set_drvdata(pdev, bt);
	bt->debug = debugfs_create_dir("apple_centauri_bt_probe", NULL);
	debugfs_create_file("status", 0400, bt->debug, bt, &bt_status_fops);
	debugfs_create_file("probe", 0200, bt->debug, bt, &bt_probe_fops);
	dev_info(&pdev->dev, "Beta isolated probe ready; no HCI command sent\n");
	return 0;
}

static void bt_remove(struct pci_dev *pdev)
{
	struct cen_bt_probe *bt = pci_get_drvdata(pdev);

	debugfs_remove_recursive(bt->debug);
	mutex_lock(&bt->lock);
	pci_clear_master(pdev);
	/* Hardware showed IPC control 0 triggers an abort. Until coordinated
	 * teardown is understood, keep published DMA valid through detachment.
	 */
	if (bt->published || !pci_wait_for_pending_transaction(pdev))
		dev_warn(&pdev->dev, "Beta detach requires fresh firmware boot; retaining DMA arena\n");
	else
		dma_free_coherent(&pdev->dev, BT_ARENA_SIZE, bt->arena, bt->dma);
	mutex_unlock(&bt->lock);
}

static const struct pci_device_id bt_ids[] = {
	{ PCI_DEVICE(PCI_VENDOR_ID_APPLE, 0x1903) },
	{ }
};
MODULE_DEVICE_TABLE(pci, bt_ids);

static struct pci_driver bt_driver = {
	.name = "apple_centauri_bt_probe",
	.id_table = bt_ids,
	.probe = bt_probe,
	.remove = bt_remove,
};
module_pci_driver(bt_driver);
MODULE_LICENSE("GPL");
MODULE_DESCRIPTION("Bounded Apple N1 Beta IPC and HCI version probe for J813");
