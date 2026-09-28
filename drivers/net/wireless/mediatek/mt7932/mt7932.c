// SPDX-License-Identifier: GPL-2.0-only
/* J700 firmware-owned Wi-Fi: native transport and cfg80211 station support.
 * Clean-room contract: NEO_MT7932_DOWNLOAD_AND_NIC_CAP_CONTRACT.md.
 * All addresses here are BAR0 offsets, never chip/CPU physical addresses.
 */
#include "mt7932.h"

static int mt_ring_alloc(struct mt7932 *m, struct mt7932_ring *q,
			 unsigned int n, bool rx, unsigned int count);
static void mt_rx_post(struct mt7932_ring *q, unsigned int index);
static void mt_receive(struct mt7932 *m, const u8 *packet, size_t length);
static void mt_rx_drain(struct mt7932 *m, struct mt7932_ring *q);
static irqreturn_t mt_irq(int irq, void *data);
static void mt_service(struct mt7932 *m);
static irqreturn_t mt_irq_thread(int irq, void *data);
static int mt_submit(struct mt7932 *m, struct mt7932_ring *q, size_t length);
static int mt_command(struct mt7932 *m, u8 cid, const void *payload, size_t length);
static int mt_ownership(struct mt7932 *m);
static int mt_dma_setup(struct mt7932 *m);
static int mt_rom_gate(struct mt7932 *m);
static int mt_status(struct mt7932 *m, u8 cid, const void *payload, size_t length);
static int mt_scatter(struct mt7932 *m, const u8 *data, size_t length);
static int mt_download(struct mt7932 *m, const u8 *data, struct mt7932_region *r,
		       bool ram);
static int mt_secure_acquire(struct mt7932 *m);
static void mt_secure_release(struct mt7932 *m);
static int mt_otp_byte(struct mt7932 *m, u32 address, u8 *value);
static int mt_capability_gate(struct mt7932 *m);
static int mt_config_gate(struct mt7932 *m);
static int mt_board_gate(struct mt7932 *m);
static int mt_precal_gate(struct mt7932 *m);
static int mt_runtime_start(struct mt7932 *m);
static int mt_firmware_gate(struct mt7932 *m);
static int mt_initialize(struct mt7932 *m);
static int mt_probe(struct pci_dev *pdev, const struct pci_device_id *id);
static void mt_shutdown(struct pci_dev *pdev);

static bool batch_rx = true;
module_param(batch_rx, bool, 0644);
MODULE_PARM_DESC(batch_rx, "Batch runtime RX refill doorbells (disable for comparison)");
static unsigned int rx_batch_limit = 16;
module_param(rx_batch_limit, uint, 0644);
MODULE_PARM_DESC(rx_batch_limit, "Runtime RX refill batch size (1..32, default 16)");
static unsigned int rx_data_ring_entries = 512;
module_param(rx_data_ring_entries, uint, 0444);
MODULE_PARM_DESC(rx_data_ring_entries, "RX2/RX7 entries: 256, 512 (default), or 1024");
static unsigned int tx_data_ring_entries = 512;
module_param(tx_data_ring_entries, uint, 0444);
MODULE_PARM_DESC(tx_data_ring_entries, "TX0 entries: 256 or 512 (default)");

static bool legacy_irq_test;
module_param(legacy_irq_test, bool, 0444);
MODULE_PARM_DESC(legacy_irq_test, "Explicit diagnostic use of standard PCI INTx instead of MSI");
static bool poll_test;
module_param(poll_test, bool, 0444);
MODULE_PARM_DESC(poll_test, "Diagnostic ROM/firmware polling with endpoint IRQ generation masked");

bool mt_transport_polled(void)
{
	return poll_test;
}


bool mt_rf_allowed(struct mt7932 *m)
{
	return READ_ONCE(m->rf_ready) && !READ_ONCE(m->reg_pending) &&
	       !READ_ONCE(m->policy_failed);
}

u32 mt_read(struct mt7932 *m, u32 reg)
{
	return readl(m->bar + reg);
}

void mt_write(struct mt7932 *m, u32 reg, u32 value)
{
	writel(value, m->bar + reg);
}

void mt_rmw(struct mt7932 *m, u32 reg, u32 mask, u32 value)
{
	mt_write(m, reg, (mt_read(m, reg) & ~mask) | value);
}

int mt_poll(struct mt7932 *m, u32 reg, u32 mask, u32 expected, u32 timeout)
{
	u32 value;
	int ret = readl_poll_timeout(m->bar + reg, value,
				    value == U32_MAX || (value & mask) == expected,
				    1000, timeout);
	if (ret || value == U32_MAX) {
		dev_err(&m->pdev->dev, "poll %x got %08x wanted %08x/%08x\n",
			reg, value, expected, mask);
		return ret ?: -ENODEV;
	}
	return 0;
}

void *mt_alloc(struct mt7932 *m, size_t size, dma_addr_t *dma)
{
	void *p = dmam_alloc_coherent(&m->pdev->dev, size, dma, GFP_KERNEL);

	if (!p)
		return NULL;
	if (*dma >= BIT(27) || size > BIT(27) - *dma) {
		dev_err(&m->pdev->dev, "DMA allocation outside qualified 27-bit aperture\n");
		return NULL;
	}
	return p;
}

static int mt_ring_alloc(struct mt7932 *m, struct mt7932_ring *q,
			 unsigned int n, bool rx, unsigned int count)
{
	unsigned int i;

	q->count = count;
	q->reg = W + (rx ? 0x500 : 0x300) + 16 * n;
	q->desc = mt_alloc(m, count * sizeof(*q->desc), &q->dma);
	if (!q->desc)
		return -ENOMEM;
	for (i = 0; i < count; i++)
		q->desc[i].control = cpu_to_le32(DONE);
	q->slots = rx ? count : n == 16 ? 640 : n == 17 ? 1024 : 0;
	if (q->slots) {
		q->buffers = mt_alloc(m, q->slots * (rx ? RX_STRIDE : STRIDE),
				      &q->buffers_dma);
		if (!q->buffers)
			return -ENOMEM;
	}
	return 0;
}

static void mt_rx_post(struct mt7932_ring *q, unsigned int index)
{
	struct mt7932_desc *d = &q->desc[index];

	d->address = cpu_to_le32(q->buffers_dma + index * RX_STRIDE);
	d->second = 0;
	d->info = 0;
	d->control = cpu_to_le32(MT7932_RX_CAPACITY << 16);
}

static void mt_receive(struct mt7932 *m, const u8 *packet, size_t length)
{
	struct mt7932_event event;
	unsigned long flags;
	int ret;

	if (length >= 4 && (get_unaligned_le32(packet) >> 27) == 6) {
		mt_data_complete(m, packet, length);
		return;
	}
	ret = mt7932_event_parse(packet, length, &event);

	m->packets++;
	if (ret) {
		if (ret == -ENOMSG) {
			mt_packet_receive(m, packet, length);
			return;
		}
		dev_dbg_ratelimited(&m->pdev->dev, "RX framing result %d len %zu word %08x\n",
				     ret, length, length >= 4 ? get_unaligned_le32(packet) : 0);
		return;
	}
	spin_lock_irqsave(&m->response_lock, flags);
	/* Private 25G83 arEventTable entry 31: EIDf0 -> nicEventAssertDump.
	 * Keep the default path unchanged. Never print opaque dump contents.
	 * An assert is not a DMA fence or a successful command completion.
	 */
	if (event.eid == 0xf0 && m->bw_limit > 20) {
		if (!m->fw_asserted) {
			mt_bw_latch(m, "firmware-assert-EIDf0");
			m->fw_asserted = true;
			m->link_failed = true;
			m->cal_state.error = -EIO;
			m->connect_error = -EIO;
			WRITE_ONCE(m->rf_ready, false);
			if (m->netdev) {
				netif_stop_queue(m->netdev);
				netif_carrier_off(m->netdev);
			}
			complete(&m->response);
			complete(&m->cal_response);
			complete(&m->assoc_update);
			complete(&m->assoc_done);
			complete(&m->discovery_done);
			dev_err(&m->pdev->dev,
				"BW_FIRMWARE_ASSERT: EIDf0; 20 MHz latched, DMA retained; crash reset unavailable, reboot with mt7932_fullmac.max_bw_mhz=20\n");
		}
		goto unlock;
	}
	if (event.eid == 0xee) {
		mt_link_event(m, &event);
		goto unlock;
	}
	if (event.eid == 0x0d) {
		mt_scan_event(m, &event);
		goto unlock;
	}
	if (event.eid == 0x11) {
		mt_bss_presence(m, &event);
		goto unlock;
	}
	if (event.eid == 0xd6) {
		bool terminal = m->bw_cal_active && m->cal_state.active &&
			m->cal_state.expected == 1 && !m->cal_state.received;

		ret = mt7932_cal_null(&m->cal_state, packet + 36,
				     event.length >= 36 ? event.length - 36 : 0,
				     m->smart_version);
		if (terminal && ret == -EREMOTEIO)
			m->bw_cal_rejected = true;
		dev_info(&m->pdev->dev, "CAL_EVENT: seq=%u length=%zu result=%d credits=%u/%u\n",
			 event.seq, event.length, ret, m->cal_state.received, m->cal_state.expected);
		if (ret)
			complete(&m->cal_response);
		goto unlock;
	}
	if (event.eid == 0xd7) {
		if (event.length < 52 || m->cal_request_count == ARRAY_SIZE(m->cal_requests)) {
			m->cal_state.error = -EPROTO;
			complete(&m->cal_response);
		} else {
			unsigned int at = (m->cal_request_head + m->cal_request_count) % ARRAY_SIZE(m->cal_requests);

			memcpy(m->cal_requests[at], packet + 36, 16);
			m->cal_request_seq[at] = event.seq;
			m->cal_request_count++;
			dev_info(&m->pdev->dev, "CAL_REQUEST: seq=%u words=%08x/%08x/%08x/%08x queued=%u\n",
				 event.seq, get_unaligned_le32(packet + 36), get_unaligned_le32(packet + 40),
				 get_unaligned_le32(packet + 44), get_unaligned_le32(packet + 48), m->cal_request_count);
			if (READ_ONCE(m->rf_ready))
				schedule_work(&m->cal_work);
		}
		goto unlock;
	}
	if (mt7932_event_matches(&event, m->waiting) && !m->reply_length) {
		memcpy(m->reply, packet, event.length);
		m->reply_length = event.length;
		complete(&m->response);
	} else {
		dev_dbg_ratelimited(&m->pdev->dev, "event eid=%02x seq=%u option=%02x len=%zu\n",
				     event.eid, event.seq, event.option, event.length);
	}
unlock:
	spin_unlock_irqrestore(&m->response_lock, flags);
}

static void mt_rx_drain(struct mt7932 *m, struct mt7932_ring *q)
{
	unsigned int budget = q->count, pending = 0, drained = 0;
	unsigned int limit = clamp_val(READ_ONCE(rx_batch_limit), 1, 32);

	while (budget--) {
		u32 ctrl = le32_to_cpu(READ_ONCE(q->desc[q->tail].control));
		u32 length;

		if (!(ctrl & DONE))
			break;
		m->rx_descriptors++;
		drained++;
		dma_rmb();
		length = (ctrl >> 16) & 0x3fff;
		if (length <= MT7932_RX_CAPACITY && (ctrl & BIT(30)))
			mt_receive(m, q->buffers + q->tail * RX_STRIDE, length);
		else
			dev_err_ratelimited(&m->pdev->dev, "RX length overflow %u\n", length);
		if (READ_ONCE(m->ram_config) && length >= 33 && length <= MT7932_RX_CAPACITY) {
			u8 *packet = q->buffers + q->tail * RX_STRIDE;

			/* Source-specific RAM-config acknowledgment before RX0 refill. */
			if (q == &m->rx[0] && packet[28] == 1 && packet[29] == m->waiting &&
			    !packet[30] && packet[32] == 1) {
				mt_write(m, W + 0x204, 0);
				mt_write(m, W + 0x200, 0x0c000001);
				mt_write(m, W + 0x204, poll_test ? 0 : IRQ_MASK);
				mt_write(m, W + 0x1f4, 0xffff);
			}
		}
		/* Runtime owns all N slots; initial download reserves one empty. */
		mt_rx_post(q, m->runtime_epoch ? q->tail : q->head);
		q->head = (q->head + 1) % q->count;
		q->tail = (q->tail + 1) % q->count;
		pending++;
		/* Keep the download protocol unchanged. Runtime batches are smaller
		 * than the smallest ring, so a full wrap never hides returned slots.
		 */
		if (!m->runtime_epoch || !READ_ONCE(batch_rx) || pending == limit) {
			dma_wmb();
			mt_write(m, q->reg + 8, m->runtime_epoch ?
				 (q->head + q->count - 1) % q->count : q->head);
			m->rx_doorbells++;
			pending = 0;
		}
	}
	if (pending) {
		dma_wmb();
		mt_write(m, q->reg + 8, (q->head + q->count - 1) % q->count);
		m->rx_doorbells++;
	}
	if (mt_perf_enabled()) {
		m->perf_rx_drain_max = max(m->perf_rx_drain_max, (u64)drained);
		if (drained == q->count)
			m->perf_rx_drain_full++;
	}
}

static irqreturn_t mt_irq(int irq, void *data)
{
	struct mt7932 *m = data;

	if (!READ_ONCE(m->running))
		return IRQ_NONE;
	if (!(mt_read(m, W + 0x200) & m->irq_mask))
		return IRQ_NONE;
	mt_write(m, W + 0x204, 0);
	return IRQ_WAKE_THREAD;
}

static void mt_service(struct mt7932 *m)
{
	u32 causes = mt_read(m, W + 0x200) & m->irq_mask;
	unsigned int i;

	/* Acknowledge the snapshot before draining. A completion arriving after
	 * its ring was visited must leave a new cause pending for the next IRQ.
	 * Clearing the old snapshot after draining can erase that notification.
	 */
	mt_write(m, W + 0x200, causes);
	mt_data_clean(m);
	for (i = 0; i < ARRAY_SIZE(m->rx); i++)
		if (m->rx[i].count)
			mt_rx_drain(m, &m->rx[i]);
	if (causes & BIT(29)) {
		u32 sw = mt_read(m, W + 0x1f0);

		mt_write(m, W + 0x1f0, sw);
	}
	if (READ_ONCE(m->running) && !poll_test)
		mt_write(m, W + 0x204, m->irq_mask);
}

static irqreturn_t mt_irq_thread(int irq, void *data)
{
	struct mt7932 *m = data;
	u64 start = 0, elapsed;

	if (mt_perf_enabled())
		start = ktime_get_ns();
	m->interrupts++;
	mt_service(m);
	if (mt_perf_enabled()) {
		elapsed = ktime_get_ns() - start;
		m->perf_irq_ns += elapsed;
		m->perf_irq_max_ns = max(m->perf_irq_max_ns, elapsed);
	}
	return IRQ_HANDLED;
}

void mt_tx_clean(struct mt7932_ring *q)
{
	while (q->queued && (le32_to_cpu(READ_ONCE(q->desc[q->tail].control)) & DONE)) {
		dma_rmb();
		q->tail = (q->tail + 1) % q->count;
		q->queued--;
	}
}

static int mt_submit(struct mt7932 *m, struct mt7932_ring *q, size_t length)
{
	struct mt7932_desc *d;
	unsigned int retry;

	if (q->used >= q->slots || length > STRIDE || !length)
		return -ENOSPC;
	for (retry = 0; retry < 50; retry++) {
		mt_tx_clean(q);
		if (q->queued < q->count - 1)
			break;
		usleep_range(100, 200);
	}
	if (q->queued == q->count - 1)
		return -EBUSY;
	d = &q->desc[q->head];
	d->address = cpu_to_le32(q->buffers_dma + q->used * STRIDE);
	d->second = 0;
	d->info = 0;
	dma_wmb();
	d->control = cpu_to_le32((length << 16) | BIT(30));
	q->used++;
	q->head = (q->head + 1) % q->count;
	q->queued++;
	dma_wmb();
	mt_write(m, q->reg + 8, q->head);
	return 0;
}

/* One caller, one outstanding command, no retry after uncertain completion. */
int mt_request_ext(struct mt7932 *m, u8 cid, u8 ext, bool runtime, bool set, bool wait,
		      const void *payload, size_t length)
{
	struct mt7932_ring *q = &m->tx[17];
	unsigned long flags;
	int ret;

	if (READ_ONCE(m->fw_asserted))
		return -EIO;
	if (q->used >= q->slots) {
		/* Command buffers can wrap only after every published descriptor has
		 * retired. Never reclaim by elapsed time or by a reply alone.
		 */
		mt_tx_clean(q);
		if (q->queued)
			return -ENOSPC;
		q->used = 0;
	}
	if (!++m->seq)
		m->seq = 1;
	ret = mt7932_envelope(q->buffers + q->used * STRIDE, STRIDE,
			      cid, ext, m->seq, runtime, set, wait, payload, length);
	if (ret < 0)
		return ret;
	reinit_completion(&m->response);
	spin_lock_irqsave(&m->response_lock, flags);
	m->waiting = wait ? m->seq : 0;
	m->reply_length = 0;
	spin_unlock_irqrestore(&m->response_lock, flags);
	ret = mt_submit(m, q, ret);
	if (!ret && wait) {
		if (poll_test) {
			unsigned long deadline = jiffies + msecs_to_jiffies(5000);

			do {
				mt_service(m);
				if (completion_done(&m->response))
					break;
				usleep_range(100, 200);
			} while (time_before(jiffies, deadline));
			if (!completion_done(&m->response))
				ret = -ETIMEDOUT;
		} else if (!wait_for_completion_timeout(&m->response, msecs_to_jiffies(5000))) {
			ret = -ETIMEDOUT;
		}
	}
	if (READ_ONCE(m->fw_asserted))
		ret = -EIO;
	if (ret == -ETIMEDOUT) {
		/* Diagnostic only: never turn a polled late reply into IRQ success. */
		unsigned int i;
		u16 command;
		u32 mask, pending;
		unsigned int cap = pci_find_capability(m->pdev, PCI_CAP_ID_MSI);

		pci_read_config_word(m->pdev, PCI_COMMAND, &command);
		pci_read_config_dword(m->pdev, cap + PCI_MSI_MASK_64, &mask);
		pci_read_config_dword(m->pdev, cap + PCI_MSI_PENDING_64, &pending);
		dev_info(&m->pdev->dev, "timeout PCI command=%04x MSI mask=%08x pending=%08x\n",
			 command, mask, pending);

		dev_info(&m->pdev->dev, "timeout gates MAC=%08x WFDMA=%08x SW=%08x\n",
			 mt_read(m, 0x10188), mt_read(m, W + 0x204), mt_read(m, W + 0x1f4));
		disable_irq(pci_irq_vector(m->pdev, 0));
		for (i = 0; i < ARRAY_SIZE(m->rx); i++) {
			struct mt7932_ring *rx = &m->rx[i];

			if (!rx->count)
				continue;
			dev_info(&m->pdev->dev, "timeout RX%u ctrl=%08x CPU=%u DMA=%u\n", i,
				 le32_to_cpu(rx->desc[rx->tail].control),
				 mt_read(m, rx->reg + 8), mt_read(m, rx->reg + 12));
			if (le32_to_cpu(rx->desc[rx->tail].control) & DONE) {
				dma_rmb();
				print_hex_dump(KERN_INFO, "ROM RX prefix: ", DUMP_PREFIX_OFFSET,
					       16, 1, rx->buffers + rx->tail * RX_STRIDE, 40, false);
			}
			mt_rx_drain(m, rx);
		}
		dev_info(&m->pdev->dev, "late polled reply length=%zu eid=%02x (not IRQ success)\n",
			 m->reply_length, m->reply_length > 28 ? m->reply[28] : 0);
		enable_irq(pci_irq_vector(m->pdev, 0));
	}
	spin_lock_irqsave(&m->response_lock, flags);
	m->waiting = 0;
	spin_unlock_irqrestore(&m->response_lock, flags);
	if (ret)
		dev_err(&m->pdev->dev,
			"command %02x seq %u failed %d IRQ=%llu RX=%llu status=%08x TX=%u/%u\n",
			cid, m->seq, ret, m->interrupts, m->packets, mt_read(m, W + 0x200),
			mt_read(m, q->reg + 8), mt_read(m, q->reg + 12));
	return ret;
}

int mt_request(struct mt7932 *m, u8 cid, bool runtime, bool set, bool wait,
		      const void *payload, size_t length)
{
	return mt_request_ext(m, cid, 0, runtime, set, wait, payload, length);
}

static int mt_command(struct mt7932 *m, u8 cid, const void *payload, size_t length)
{
	return mt_request(m, cid, false, false, true, payload, length);
}

static int mt_ownership(struct mt7932 *m)
{
	unsigned int attempt;
	int ret = -ETIMEDOUT;

	mt_write(m, 0xe0018, 1);
	for (attempt = 0; attempt < 10; attempt++) {
		mt_write(m, 0xe0010, 2);
		ret = mt_poll(m, 0xe0010, BIT(2), 0, 50000);
		if (!ret || ret == -ENODEV)
			break;
	}
	if (ret)
		return ret;
	mt_write(m, 0xfe250, 0x70001846);
	udelay(2);
	if ((mt_read(m, 0xfe250) >> 16) != 0x7000)
		return -EIO;
	dev_info(&m->pdev->dev, "DRIVER_OWN_OK power=%08x\n", mt_read(m, 0xe00f0));
	return 0;
}

static int mt_dma_setup(struct mt7932 *m)
{
	static const u16 rx_counts[] = {32, 0, 512, 32, 32, 224, 128, 512, 64};
	static const u16 rx_prefetch[] = {0, 0, 0x40, 0xc0, 0x100, 0x140, 0x180, 0x1c0, 0x200};
	static const u16 tx_prefetch[] = {0x240, 0x280, 0x2c0, 0x300, 0x340, 0x400,
		0x440, 0x480, 0x4c0, 0x580, 0x5c0, 0x600, 0x640, 0x700, 0x740,
		0, 0x780, 0x7c0, 0x800};
	unsigned int i, j;
	int ret;
	u32 glo;
	unsigned int rx_data_entries = READ_ONCE(rx_data_ring_entries);
	unsigned int tx_data_entries = READ_ONCE(tx_data_ring_entries);

	if (rx_data_entries != 256 && rx_data_entries != 512 && rx_data_entries != 1024) {
		dev_warn(&m->pdev->dev, "invalid RX data ring size %u, using 512\n",
			 rx_data_entries);
		rx_data_entries = 512;
	}
	if (tx_data_entries != 256 && tx_data_entries != 512) {
		dev_warn(&m->pdev->dev, "invalid TX data ring size %u, using 512\n",
			 tx_data_entries);
		tx_data_entries = 512;
	}

	mt_write(m, W + 0x204, 0);
	mt_write(m, 0x10188, poll_test ? 0 : 0xff);
	mt_rmw(m, W + 0x208, BIT(0) | BIT(2) | BIT(15) | BIT(21) | BIT(27) | BIT(28), 0);
	ret = mt_poll(m, W + 0x208, BIT(1) | BIT(3), 0, 100000);
	if (ret)
		return ret;
	mt_rmw(m, W + 0x2b0, BIT(6), 0);
	if (mt_read(m, H + 4) & BIT(28))
		return -EINVAL;
	mt_rmw(m, W + 0x100, BIT(4) | BIT(5), 0);
	mt_rmw(m, W + 0x100, 0, BIT(4) | BIT(5));
	mt_rmw(m, H + 0x1c, 0x0fff0fff, 1);
	mt_rmw(m, H + 0x10, 0xffff0000, 0x80000000);
	for (i = 0; i < 16; i++) {
		u32 quota = i < 4 ? 0x02000028 : i == 15 ? 0 :
			    !(i % 4) ? 0x01000014 : 0x01000028;

		mt_write(m, H + 0x20 + 4 * i, quota);
		if ((mt_read(m, H + 0x20 + 4 * i) & 0x0fff0fff) != quota)
			return -EIO;
	}
	mt_write(m, H + 0x60, 0x76543210);
	mt_write(m, H + 0x64, 0xfedcba98);
	mt_write(m, H + 0x68, 0x11111000);
	mt_write(m, H + 0x6c, 0x11111000);
	mt_write(m, H + 0x70, 0x76543210);
	mt_write(m, H + 0x74, 0xfedcba98);
	mt_rmw(m, H + 0x0c, 3 << 16, BIT(16));
	mt_rmw(m, H + 4, BIT(28), 0);
	for (i = 0; i < ARRAY_SIZE(m->tx); i++) {
		if (i == 15)
			continue;
		ret = mt_ring_alloc(m, &m->tx[i], i, false,
				    i == 0 ? tx_data_entries :
				    i < 15 ? 512 : i == 16 ? 256 : i == 17 ? 24 : 16);
		if (ret)
			return ret;
	}
	for (i = 0; i < ARRAY_SIZE(m->rx); i++) {
		if (!rx_counts[i])
			continue;
		ret = mt_ring_alloc(m, &m->rx[i], i, true,
				    (i == 2 || i == 7) ? rx_data_entries : rx_counts[i]);
		if (ret)
			return ret;
	}
	m->ipc = mt_alloc(m, 0x300, &m->ipc_dma);
	m->aux = mt_alloc(m, 0x320, &m->aux_dma);
	if (!m->ipc || !m->aux)
		return -ENOMEM;
	for (i = 0; i < ARRAY_SIZE(m->tx); i++) {
		struct mt7932_ring *q = &m->tx[i];

		if (!q->count)
			continue;
		mt_write(m, q->reg, q->dma);
		mt_write(m, q->reg + 8, 0);
		mt_write(m, q->reg + 4, q->count);
		mt_write(m, W + 0x600 + 4 * i, (tx_prefetch[i] << 16) | (i == 4 || i == 8 || i == 12 ? 12 : 4));
	}
	for (i = 0; i < ARRAY_SIZE(m->rx); i++) {
		struct mt7932_ring *q = &m->rx[i];

		if (!q->count)
			continue;
		mt_write(m, q->reg, q->dma);
		mt_write(m, q->reg + 8, 0);
		mt_write(m, q->reg + 4, q->count);
		for (j = 0; j < q->count - 1; j++)
			mt_rx_post(q, j);
		q->head = q->count - 1;
		dma_wmb();
		mt_write(m, q->reg + 8, q->head);
		mt_write(m, W + 0x680 + 4 * i, (rx_prefetch[i] << 16) | (i == 2 ? 8 : 4));
	}
	mt_write(m, W + 0x20c, U32_MAX);
	glo = (mt_read(m, W + 0x208) & ~(GENMASK(5, 4) | BIT(11) | BIT(13) | BIT(20))) |
		BIT(6) | BIT(12) | BIT(15) | BIT(21) | BIT(28) | BIT(30);
	mt_write(m, W + 0x208, glo);
	mt_rmw(m, W + 0x2b0, 0, BIT(6));
	/* All endpoint-visible storage and interrupt state now have an owner. */
	dma_wmb();
	pci_set_master(m->pdev);
	WRITE_ONCE(m->running, true);
	mt_rmw(m, W + 0x208, 0, BIT(0) | BIT(2));
	mt_write(m, W + 0x298, 0xc);
	mt_write(m, C + 0x38, 0x13);
	mt_write(m, W + 0x2f0, 0x8032800a);
	mt_rmw(m, 0x2120, 0, BIT(1));
	mt_write(m, W + 0x1f4, 0xffff);
	mt_write(m, W + 0x204, poll_test ? 0 : IRQ_MASK);
	dev_info(&m->pdev->dev, "DMA_PUBLISHED glo=%08x TX17=%pad RX0=%pad IPC=%pad\n",
		 mt_read(m, W + 0x208), &m->tx[17].dma, &m->rx[0].dma, &m->ipc_dma);
	return 0;
}

static int mt_rom_gate(struct mt7932 *m)
{
	static const u32 addresses[] = {0x70010204, 0x88000004};
	u8 request[12] = {};
	unsigned int i;
	int ret = mt_poll(m, 0xe00f0, 7, 1, 1000000);

	if (ret)
		return ret;
	mt_write(m, 0xfe254, 0x18051848);
	udelay(2);
	if ((mt_read(m, 0xfe254) >> 16) != 0x1805)
		return -EIO;
	dma_wmb();
	mt_write(m, 0x93c28, 1);
	mt_write(m, 0x93a38, 2);
	mt_write(m, 0x93a3c, 0x300);
	mt_write(m, 0x93a30, m->ipc_dma);
	mt_write(m, 0x93a54, m->aux_dma);
	if (mt_read(m, 0x93a30) != m->ipc_dma || mt_read(m, 0x93a54) != m->aux_dma)
		return -EIO;
	for (i = 0; i < ARRAY_SIZE(addresses); i++) {
		put_unaligned_le32(addresses[i], request + 4);
		ret = mt_command(m, 3, request, sizeof(request));
		if (ret)
			return ret;
		if (m->reply_length < 40 || m->reply[28] != 2 ||
		    get_unaligned_le32(m->reply + 32) != addresses[i])
			return -EPROTO;
		dev_info(&m->pdev->dev, "ROM_READ_OK %08x=%08x seq=%u IRQ=%llu\n",
			 addresses[i], get_unaligned_le32(m->reply + 36), m->seq, m->interrupts);
	}
	dev_info(&m->pdev->dev, "ROM_TRANSPORT_OK mode=%s IRQ=%llu (no firmware download or RF yet)\n",
		 poll_test ? "polled-test" : "interrupt", m->interrupts);
	return 0;
}

static int mt_status(struct mt7932 *m, u8 cid, const void *payload, size_t length)
{
	int ret = mt_command(m, cid, payload, length);

	if (ret)
		return ret;
	if (m->reply_length < 33 || m->reply[28] != 1 || m->reply[32] != 1) {
		dev_err(&m->pdev->dev, "boot status CID=%02x EID=%02x status=%u len=%zu\n",
			cid, m->reply[28], m->reply[32], m->reply_length);
		return -EPROTO;
	}
	return 0;
}

static int mt_scatter(struct mt7932 *m, const u8 *data, size_t length)
{
	struct mt7932_ring *q = &m->tx[16];

	while (length) {
		size_t bytes = min_t(size_t, length, 2048);
		int ret;

		if (q->used >= q->slots)
			return -ENOSPC;
		memcpy(q->buffers + q->used * STRIDE, data, bytes);
		ret = mt_submit(m, q, bytes);
		if (ret)
			return ret;
		mt_tx_clean(q);
		data += bytes;
		length -= bytes;
	}
	return 0;
}

static int mt_download(struct mt7932 *m, const u8 *data, struct mt7932_region *r,
		       bool ram)
{
	u8 request[12];
	u8 cid = r->target == 0x900000 || r->target == 0x200000 ? 5 : 1;
	int ret;

	put_unaligned_le32(r->target, request);
	put_unaligned_le32(r->length, request + 4);
	put_unaligned_le32(r->mode, request + 8);
	WRITE_ONCE(m->ram_config, ram);
	ret = mt_status(m, cid, request, sizeof(request));
	WRITE_ONCE(m->ram_config, false);
	if (ret)
		return ret;
	ret = mt_scatter(m, data + r->offset, r->length);
	dev_info(&m->pdev->dev, "%s region target=%08x length=%u scatter=%d\n",
		 ram ? "RAM" : "PATCH", r->target, r->length, ret);
	return ret;
}

static int mt_secure_acquire(struct mt7932 *m)
{
	int ret;

	mt_write(m, 0xfe24c, 0x18451807);
	ret = mt_poll(m, 0x40060, 1, 1, 5000000);
	if (!ret)
		mt_write(m, 0xfe24c, 0x1845184f);
	return ret;
}

static void mt_secure_release(struct mt7932 *m)
{
	mt_write(m, 0xfe24c, 0x18451807);
	mt_write(m, 0x40260, 1);
	mt_write(m, 0xfe24c, 0x1845184f);
}

static int mt_otp_byte(struct mt7932 *m, u32 address, u8 *value)
{
	u8 request[4];
	int ret;

	put_unaligned_le32(address, request);
	ret = mt_command(m, 0x50, request, sizeof(request));
	if (ret)
		return ret;
	if (m->reply_length < 40)
		return -EPROTO;
	*value = m->reply[36];
	return 0;
}

static int mt_capability_gate(struct mt7932 *m)
{
	u8 entropy[68] = {};
	const u8 *body;
	size_t offset = 4;
	int ret, count, i;
	bool mac = false, phy = false;

	get_random_bytes(entropy + 4, 64);
	ret = mt_request(m, 0xe2, false, false, false, entropy, sizeof(entropy));
	memzero_explicit(entropy, sizeof(entropy));
	if (ret)
		return ret;
	ret = mt_request(m, 0, false, false, false, NULL, 0);
	if (ret)
		return ret;
	ret = mt_request(m, 0x8a, true, false, true, NULL, 0);
	if (ret)
		return ret;
	if (m->reply_length < 40)
		return -EPROTO;
	body = m->reply + 36;
	if (m->reply[28] != 0xec)
		return -EPROTO;
	count = mt7932_capabilities(body, m->reply_length - 36);
	if (count < 0) {
		dev_err(&m->pdev->dev, "invalid capability reply len=%zu parse=%d\n", m->reply_length, count);
		print_hex_dump(KERN_INFO, "NIC_CAP_REJECTED: ", DUMP_PREFIX_OFFSET,
			       16, 1, m->reply, m->reply_length, false);
		return count;
	}
	for (i = 0; i < count; i++) {
		u32 type = get_unaligned_le32(body + offset);
		u32 length = get_unaligned_le32(body + offset + 4);
		const u8 *value = body + offset + 8;

		dev_info(&m->pdev->dev, "capability TLV type=%x bytes=%u\n", type, length);
		if (type == 7) {
			dev_info(&m->pdev->dev, "firmware MAC %pM\n", value);
			mac = true;
		} else if (type == 8) {
			if (!value[4] || value[4] > 4)
				return -EOPNOTSUPP;
			memcpy(m->phy_cap, value, 12);
			m->antenna_mask = (1U << value[4]) - 1;
			dev_info(&m->pdev->dev, "firmware PHY NSS=%u paths=%02x HT=%u VHT=%u HE=%u\n",
				 value[4], value[10], value[0], value[1], value[11]);
			phy = true;
		} else if (type == 0x38 && length) {
			m->runtime_cal = value[0];
			m->runtime_cal_valid = true;
			dev_info(&m->pdev->dev, "RUNTIME_CAL_CAPABILITY: %u\n", value[0]);
		} else if (type == 0x18 && length >= 4) {
			m->six_ghz = value[0];
			m->six_ghz_valid = value[0] <= 1;
		} else if (type == 0x34 && length >= 2) {
			m->stof_supported = value[0] || value[1];
		}
		offset += 8 + length;
	}
	if (!mac || !phy)
		return -ENODATA;
	print_hex_dump(KERN_INFO, "NIC_CAP: ", DUMP_PREFIX_OFFSET, 16, 1,
		       m->reply, m->reply_length, false);
	dev_info(&m->pdev->dev, "MCU_CAPABILITY_OK eid=%02x seq=%u (no calibration or RF yet)\n",
		 m->reply[28], m->seq);
	return 0;
}

static int mt_config_gate(struct mt7932 *m)
{
	u8 basic[24], mlme[4];
	unsigned long deadline;
	int ret;

	mt7932_basic_config(basic);
	mt7932_mlme_config(mlme);
	ret = mt_request(m, 2, true, true, false, basic, sizeof(basic));
	if (ret)
		return ret;
	ret = mt_request_ext(m, 0xea, 0x4e, true, true, false, mlme, sizeof(mlme));
	if (ret)
		return ret;
	deadline = jiffies + msecs_to_jiffies(500);
	do {
		mt_tx_clean(&m->tx[17]);
		if (!m->tx[17].queued) {
			dev_info(&m->pdev->dev, "BASIC_MLME_SUBMITTED: TX retired, no semantic ACK or RF startup\n");
			return 0;
		}
		usleep_range(100, 200);
	} while (time_before(jiffies, deadline));
	return -ETIMEDOUT;
}


static int mt_board_gate(struct mt7932 *m)
{
	const u32 addresses[] = {0x70, 0x160};
	u8 request[24] = {}, module = 0, board_byte = 0;
	u32 status[2];
	unsigned int i, candidate;
	int ret;

	if (!m->runtime_epoch)
		return -EINVAL;
	ret = mt_request_ext(m, 0xed, 0xc1, true, false, true, NULL, 0);
	if (ret)
		return ret;
	if (m->reply_length < 44 || m->reply[28] != 0xed || m->reply[32] != 0xc1)
		return -EPROTO;
	dev_info(&m->pdev->dev, "CHIP_UID_QUERY_OK: 8 opaque bytes received, not published\n");
	for (i = 0; i < ARRAY_SIZE(addresses); i++) {
		put_unaligned_le32(addresses[i], request);
		ret = mt_request_ext(m, 0xed, 1, true, false, true, request, sizeof(request));
		if (ret)
			return ret;
		if (m->reply_length < 60 || m->reply[28] != 0xed || m->reply[32] != 1 ||
		    get_unaligned_le32(m->reply + 36) != addresses[i])
			return -EPROTO;
		status[i] = get_unaligned_le32(m->reply + 40);
		if (!i)
			module = m->reply[54];
		else
			board_byte = m->reply[48];
	}
	candidate = !(module & 15) ? 2 : !board_byte ? 3 : 1;
	m->board_type = candidate;
	m->module_byte = module;
	dev_info(&m->pdev->dev, "BOARD_CLASSIFICATION_CAPTURED: candidate=%u module=%02x board-byte=%02x status=%08x/%08x; no buffer branch selected\n",
		 candidate, module, board_byte, status[0], status[1]);
	return 0;
}

static int mt_precal_gate(struct mt7932 *m)
{
	const struct firmware *ppr;
	u8 request[416] = {2, 0, 0x9c, 1};
	u32 status;
	int ret;

	if (!m->runtime_epoch || m->board_type != 3 || !m->runtime_cal_valid)
		return -EINVAL;
	/* Stock EfuseBufferModeCal=3, own eFuse board3: original PPR source2.
	 * This is not the external EEPROM/source0 or own-WCAL/source3 path.
	 */
	ret = request_firmware_direct(&ppr, "mediatek/mt7932/ppr.bin", &m->pdev->dev);
	if (ret)
		return ret;
	if (ppr->size != sizeof(request) - 4) {
		release_firmware(ppr);
		return -EINVAL;
	}
	memcpy(request + 4, ppr->data, ppr->size);
	release_firmware(ppr);
	ret = mt_request_ext(m, 0xed, 0x21, true, false, true, request, sizeof(request));
	if (ret)
		return ret;
	if (m->reply_length < 44 || m->reply[28] != 0xed || m->reply[32])
		return -EPROTO;
	status = get_unaligned_le32(m->reply + 40);
	dev_info(&m->pdev->dev, "PPR_SOURCE2_RESULT: status=%08x\n", status);
	if (status)
		return -EREMOTEIO;
	memset(request, 0, sizeof(request));
	ret = mt_request_ext(m, 0xed, 0xa9, true, false, true, request, 4);
	if (ret)
		return ret;
	if (m->reply_length < 40 || m->reply[28] != 0xed || m->reply[32] != 0xaa)
		return -EPROTO;
	m->smart_version = m->reply[38] & 15;
	dev_info(&m->pdev->dev, "SMART_VERSION_REPLY: eid=%02x ext=%02x version=%u\n",
		 m->reply[28], m->reply[32], m->smart_version);
	request[3] = 9;
	ret = mt_request(m, 0xbf, true, false, true, request, 20);
	if (ret)
		return ret;
	if (m->reply_length < 56)
		return -EPROTO;
	dev_info(&m->pdev->dev, "CALTYPE_REPLY: eid=%02x ext=%02x H=%u P=%u O=%u V=%u marker=%08x\n",
		 m->reply[28], m->reply[32], m->runtime_cal, m->reply[37],
		 m->reply[44], m->reply[38], get_unaligned_le32(m->reply + 48));
	/* M=0/T=3 exact host predicate. Stage metadata only after acceptance;
	 * do not select the legacy absent-marker D6 compatibility fallback.
	 */
	if (get_unaligned_le32(m->reply + 48) != 0xbf || m->runtime_cal ||
	    m->reply[37] != 1 || m->reply[44] != 1)
		return -EPROTO;
	m->preload_version = m->reply[38];
	dev_info(&m->pdev->dev, "PRECAL_HANDSHAKE_OK: own calibration and RF policy still required\n");
	return 0;
}

static int mt_runtime_start(struct mt7932 *m)
{
	int ret;

	ret = mt_capability_gate(m);
	if (!ret)
		ret = mt_config_gate(m);
	if (!ret)
		ret = mt_recovery_gate(m);
	if (!ret)
		ret = mt_board_gate(m);
	if (!ret)
		ret = mt_precal_gate(m);

	return ret;
}

static int mt_firmware_gate(struct mt7932 *m)
{
	const struct firmware *patch, *ram;
	struct mt7932_region region;
	u8 request[8] = {}, value;
	u32 entry = 0;
	int ret, patches, regions, i;

	ret = request_firmware_direct(&patch, "mediatek/mt7932/IZUBA_WIFI_MT7932_patch_mcu_1_2_hdr.bin", &m->pdev->dev);
	if (ret)
		return ret;
	ret = request_firmware_direct(&ram, "mediatek/mt7932/IZUBA_W7932_2.bin", &m->pdev->dev);
	if (ret)
		goto out_patch;
	/* Validate all regions before emitting the first download command. */
	patches = mt7932_patch_region(patch->data, patch->size, 0, &region);
	regions = mt7932_ram_region(ram->data, ram->size, 0, &region);
	ret = -EINVAL;
	if (patches < 0 || regions < 0)
		goto out;
	for (i = 0; i < patches; i++)
		if (mt7932_patch_region(patch->data, patch->size, i, &region) < 0)
			goto out;
	for (i = 0; i < regions; i++)
		if (mt7932_ram_region(ram->data, ram->size, i, &region) < 0)
			goto out;
	dev_info(&m->pdev->dev, "stock containers patch=%zu/%d regions RAM=%zu/%d regions\n",
		 patch->size, patches, ram->size, regions);
	put_unaligned_le32(2, request);
	ret = mt_command(m, 0x10, request, 4);
	if (ret)
		goto out;
	if (m->reply_length < 33 || m->reply[32] < 1 || m->reply[32] > 3) {
		ret = -EPROTO;
		goto out;
	}
	if (m->reply[32] != 1) {
		for (i = 0; i < patches; i++) {
			mt7932_patch_region(patch->data, patch->size, i, &region);
			ret = mt_download(m, patch->data, &region, false);
			if (ret)
				goto out;
		}
		ret = mt_secure_acquire(m);
		if (ret)
			goto out;
		memset(request, 0, sizeof(request));
		ret = mt_status(m, 7, request, 4);
		if (ret)
			goto out;
		mt_secure_release(m);
	}
	dev_info(&m->pdev->dev, "PATCH_READY\n");
	ret = mt_otp_byte(m, 0x7a, &value);
	if (!ret)
		ret = mt_otp_byte(m, 0x164, &value);
	if (!ret)
		ret = mt_otp_byte(m, 0x200, &value);
	if (ret)
		goto out;
	if (value != 0x15) {
		dev_err(&m->pdev->dev, "OTP selector mismatch: %02x\n", value);
		ret = -EPROTO;
		goto out;
	}
	for (i = 0x200; i <= 0x23f; i++) {
		ret = mt_otp_byte(m, i, &value);
		if (ret)
			goto out;
	}
	for (i = 0; i < regions; i++) {
		mt7932_ram_region(ram->data, ram->size, i, &region);
		if (region.features & BIT(5))
			entry = region.target;
		if (region.features & BIT(6))
			continue;
		ret = mt_download(m, ram->data, &region, true);
		if (ret)
			goto out;
	}
	for (i = 0; i < 1000; i++) {
		mt_tx_clean(&m->tx[16]);
		if (!m->tx[16].queued)
			break;
		usleep_range(100, 200);
	}
	if (m->tx[16].queued) {
		ret = -ETIMEDOUT;
		goto out;
	}
	usleep_range(20000, 21000);
	ret = mt_secure_acquire(m);
	if (ret)
		goto out;
	put_unaligned_le32(!!entry, request);
	put_unaligned_le32(entry, request + 4);
	ret = mt_status(m, 2, request, sizeof(request));
	if (ret)
		goto out;
	mt_secure_release(m);
	ret = mt_poll(m, 0xe00f0, 3, 3, 5000000);
	if (ret)
		goto out;
	dev_info(&m->pdev->dev, "FIRMWARE_READY entry=%08x\n", entry);
	ret = mt_runtime_start(m);
out:
	release_firmware(ram);
out_patch:
	release_firmware(patch);
	return ret;
}

void mt_transport_snapshot(struct mt7932 *m)
{
	unsigned int i;
	struct mt7932_ring *tx = &m->tx[17];

	dev_info(&m->pdev->dev, "TRANSPORT: interrupts=%llu packets=%llu pending=%08x mask=%08x TX17 head=%u tail=%u queued=%u DMA=%u\n",
		 m->interrupts, m->packets, mt_read(m, W + 0x200), mt_read(m, W + 0x204),
		 tx->head, tx->tail, tx->queued, mt_read(m, tx->reg + 12));
	for (i = 0; i < ARRAY_SIZE(m->rx); i++) {
		struct mt7932_ring *rx = &m->rx[i];

		if (rx->count)
			dev_info(&m->pdev->dev, "TRANSPORT_RX%u: tail=%u producer=%u DMA=%u control=%08x\n",
				 i, rx->tail, mt_read(m, rx->reg + 8), mt_read(m, rx->reg + 12),
				 le32_to_cpu(READ_ONCE(rx->desc[rx->tail].control)));
	}
}


static int mt_initialize(struct mt7932 *m)
{
	int ret;

	ret = mt_dma_setup(m);
	if (!ret)
		ret = mt_rom_gate(m);
	if (!ret)
		ret = mt_firmware_gate(m);
	if (!ret)
		ret = mt_register_regulatory_gate(m);
	return ret;
}

static int mt_probe(struct pci_dev *pdev, const struct pci_device_id *id)
{
	struct mt7932 *m;
	struct iommu_domain *domain;
	int ret;

	if (!of_machine_is_compatible("apple,j700") || pdev->devfn != 0)
		return -ENODEV;
	domain = iommu_get_domain_for_dev(&pdev->dev);
	if (!domain || (domain->type != IOMMU_DOMAIN_DMA && domain->type != IOMMU_DOMAIN_DMA_FQ))
		return dev_err_probe(&pdev->dev, -EINVAL, "translated DMA domain required\n");
	ret = pcim_enable_device(pdev);
	if (ret)
		return ret;
	pci_clear_master(pdev);
	if (pci_resource_len(pdev, 0) < 0x100000)
		return -EINVAL;
	ret = dma_set_mask_and_coherent(&pdev->dev, DMA_BIT_MASK(27));
	if (ret)
		return ret;
	m = devm_kzalloc(&pdev->dev, sizeof(*m), GFP_KERNEL);
	if (!m)
		return -ENOMEM;
	m->pdev = pdev;
	m->irq_mask = IRQ_MASK;
	ret = of_get_mac_address(pdev->dev.of_node, m->mac);
	if (ret || !is_valid_ether_addr(m->mac))
		return dev_err_probe(&pdev->dev, ret ?: -EINVAL, "valid own-device DT MAC required\n");
	dev_dbg(&pdev->dev, "ADT_WIFI_IDENTITY_OK MAC=%pM\n", m->mac);
	m->bar = pcim_iomap_region(pdev, 0, "mt7932");
	if (IS_ERR(m->bar))
		return PTR_ERR(m->bar);
	pci_set_drvdata(pdev, m);
	init_completion(&m->response);
	init_completion(&m->cal_response);
	mutex_init(&m->command_mutex);
	spin_lock_init(&m->response_lock);
	mt_perf_init(m);
	ret = mt_ownership(m);
	if (ret)
		goto retain;
	mt_write(m, W + 0x204, 0);
	ret = pci_alloc_irq_vectors(pdev, 1, 1, legacy_irq_test ? PCI_IRQ_INTX : PCI_IRQ_ALL_TYPES);
	if (ret < 0)
		goto retain;
	m->irq_vectors_allocated = true;
	ret = devm_request_threaded_irq(&pdev->dev, pci_irq_vector(pdev, 0),
					mt_irq, mt_irq_thread, IRQF_ONESHOT | IRQF_SHARED, "mt7932", m);
	if (ret)
		goto retain;
	m->irq_requested = true;
	{
		struct irq_data *irq = irq_get_irq_data(pci_irq_vector(pdev, 0));
		unsigned int cap = pci_find_capability(pdev, PCI_CAP_ID_MSI);
		u16 control, command;
		u32 lo, hi, data, mask;

		pci_read_config_word(pdev, PCI_COMMAND, &command);
		pci_read_config_word(pdev, cap + PCI_MSI_FLAGS, &control);
		pci_read_config_dword(pdev, cap + PCI_MSI_ADDRESS_LO, &lo);
		pci_read_config_dword(pdev, cap + PCI_MSI_ADDRESS_HI, &hi);
		pci_read_config_dword(pdev, cap + PCI_MSI_DATA_64, &data);
		pci_read_config_dword(pdev, cap + PCI_MSI_MASK_64, &mask);
		dev_dbg(&pdev->dev, "PCI command=%04x MSI cap=%x control=%04x addr=%08x:%08x data=%08x\n",
			 command, cap, control, hi, lo, data);
		dev_dbg(&pdev->dev, "MSI mask=%08x MSI-X cap=%x chosen=%s\n", mask,
			 pci_find_capability(pdev, PCI_CAP_ID_MSIX),
			 pdev->msix_enabled ? "MSI-X" : pdev->msi_enabled ? "MSI" : "INTx");
		for (; irq; irq = irq->parent_data)
			dev_dbg(&pdev->dev, "IRQ %u chip=%s hwirq=%lu\n", irq->irq,
				 irq->chip->name, irq->hwirq);
	}
	ret = mt_initialize(m);
	if (!ret)
		m->initialized = true;
retain:
	if (ret)
		dev_err(&pdev->dev, "initialization failed: %d; DMA storage retained while bound\n", ret);
	/* A failed epoch retains its DMA storage until the device is removed. */
	return 0;
}

static void mt_shutdown(struct pci_dev *pdev)
{
	struct mt7932 *m = pci_get_drvdata(pdev);

	WRITE_ONCE(m->running, false);
	mt_write(m, W + 0x204, 0);
	pci_clear_master(pdev);
	/* Storage remains allocated through reset, even if the engine is stuck. */
}

static void mt_remove(struct pci_dev *pdev)
{
	struct mt7932 *m = pci_get_drvdata(pdev);
	bool dma_active;
	int ret = 0;

	/* Close cfg80211 admission first, while responses and DMA are available
	 * to retire the connection.  No work may use the netdev or wiphy after
	 * their storage is released.
	 */
	WRITE_ONCE(m->removing, true);
	WRITE_ONCE(m->interface_registered, false);
	if (m->wiphy)
		cancel_work_sync(&m->startup_work);
	if (m->netdev)
		unregister_netdev(m->netdev);
	dma_active = READ_ONCE(m->running);
	if (m->irq_requested) {
		WRITE_ONCE(m->running, false);
		mt_write(m, W + 0x204, 0);
		synchronize_irq(pci_irq_vector(pdev, 0));
	}
	if (m->wiphy)
		cancel_work_sync(&m->startup_work);
	if (m->netdev) {
		cancel_delayed_work_sync(&m->scan_timeout_work);
		cancel_work_sync(&m->scan_finish_work);
		cancel_work_sync(&m->cal_work);
		cancel_work_sync(&m->connect_work);
		cancel_work_sync(&m->disconnect_work);
		free_netdev(m->netdev);
		m->netdev = NULL;
	}
	if (m->wiphy) {
		wiphy_unregister(m->wiphy);
		wiphy_free(m->wiphy);
		m->wiphy = NULL;
	}
	/* Stop new DMA at the engine, then revoke bus mastering before the
	 * device-managed ring allocations are released.  Even a failed idle
	 * poll must never leave the old descriptors PCI-accessible.
	 */
	if (dma_active) {
		mt_rmw(m, W + 0x208, BIT(0) | BIT(2), 0);
		ret = mt_poll(m, W + 0x208, BIT(1) | BIT(3), 0, 100000);
		if (!pci_wait_for_pending_transaction(pdev))
			dev_warn(&pdev->dev, "pending PCI transaction during remove\n");
	}
	pci_clear_master(pdev);
	if (ret)
		dev_warn(&pdev->dev, "DMA idle timeout during remove: %d; bus mastering disabled\n", ret);
	if (m->irq_requested)
		devm_free_irq(&pdev->dev, pci_irq_vector(pdev, 0), m);
	if (m->irq_vectors_allocated)
		pci_free_irq_vectors(pdev);
}

static int __maybe_unused mt_suspend(struct device *dev)
{
	struct pci_dev *pdev = to_pci_dev(dev);
	struct mt7932 *m = pci_get_drvdata(pdev);
	int ret;

	/* The firmware and its DMA rings have no restartable epoch.  Save the
	 * configuration here so pci_pm_suspend_noirq() leaves this function and
	 * its upstream bridge in D0 during s2idle.  wiphy_suspend() has already
	 * retired the association; retain the firmware and ring storage.
	 */
	ret = pci_save_state(pdev);
	if (ret)
		return ret;
	if (!m->irq_requested)
		return 0;
	m->suspend_running = READ_ONCE(m->running);

	/*
	 * mt_irq() reads W + 0x200 to decide whether the interrupt is ours.
	 * That read is fatal once the device has left D0: this platform reports
	 * it as an asynchronous SError rather than returning all-ones, and an
	 * SError panics the kernel.  Measured 2026-09-21 -- a devices-level
	 * pm_test cycle aborted on apple-pmp-v2 returning -EBUSY and then died
	 * in mt_irq() during the resume rollback with "SError Interrupt on CPU0,
	 * code 0xbe000000", taking the machine with it.
	 *
	 * Silence the source while the device is still in D0, then stop the
	 * handler entirely; disable_irq() also waits for the threaded half to
	 * finish, so nothing is left in flight across the transition.
	 */
	WRITE_ONCE(m->running, false);
	mt_write(m, W + 0x204, 0);
	disable_irq(pci_irq_vector(pdev, 0));
	dev_info(dev, "SUSPEND_D0: firmware and DMA retained\n");
	return 0;
}

static int __maybe_unused mt_resume(struct device *dev)
{
	struct pci_dev *pdev = to_pci_dev(dev);
	struct mt7932 *m = pci_get_drvdata(pdev);

	if (!m->irq_requested)
		return 0;
	/* Drain completions accumulated while masked before IRQ delivery resumes.
	 * The PCI core restores the saved configuration on the rollback path too.
	 */
	WRITE_ONCE(m->running, m->suspend_running);
	if (m->initialized && m->suspend_running)
		mt_service(m);
	enable_irq(pci_irq_vector(pdev, 0));
	dev_info(dev, "RESUME_D0: firmware and DMA retained\n");
	return 0;
}

static SIMPLE_DEV_PM_OPS(mt_pm_ops, mt_suspend, mt_resume);

static const struct pci_device_id mt_ids[] = {
	{ PCI_DEVICE(PCI_VENDOR_ID_MEDIATEK, 0x7932) },
	{}
};

static struct pci_driver mt_driver = {
	.name = "mt7932-fullmac",
	.id_table = mt_ids,
	.probe = mt_probe,
	.remove = mt_remove,
	.shutdown = mt_shutdown,
	.driver.suppress_bind_attrs = true,
	.driver.pm = &mt_pm_ops,
};
module_pci_driver(mt_driver);
MODULE_LICENSE("GPL");
MODULE_DESCRIPTION("J700 MT7932 firmware-owned cfg80211 station driver");
