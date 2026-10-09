// SPDX-License-Identifier: GPL-2.0-only
/* Inherited J700 DCP management qualification, no display takeover.
 * Explicit OSLOG endpoint start is available for awake qualification.
 * The opt-in scratch_dma cycle tests owned allocations in an empty DART
 * branch without advertising buffers to firmware. No firmware reset.
 * Pins itself after adoption: remove the trial by reboot, not forced unload.
 */
#include <linux/console.h>
#include <linux/delay.h>
#include <crypto/sha2.h>
#include <linux/firmware.h>
#include <linux/io.h>
#include <linux/iopoll.h>
#include <linux/mfd/syscon.h>
#include <linux/module.h>
#include <linux/mutex.h>
#include <linux/of_address.h>
#include <linux/platform_device.h>
#include <linux/regmap.h>
#include <linux/sizes.h>
#include <linux/suspend.h>
#include <linux/vmalloc.h>
#include <linux/soc/apple/rtkit.h>

static bool retained_ram(u64 pa, size_t size);
#include "neo-dcp-owned.h"
#include "neo-dcp-epic.h"

static bool sleep_enabled;
module_param(sleep_enabled, bool, 0644);
static bool epic_sleep;
module_param(epic_sleep, bool, 0644);
MODULE_PARM_DESC(epic_sleep, "Native display power cycle around AP-only system sleep");
static bool stop_cpu;
module_param(stop_cpu, bool, 0644);
static bool ap_only;
module_param(ap_only, bool, 0644);
MODULE_PARM_DESC(ap_only, "Notify AP sleep only; keep IOP running, forbids CPU/clock stop");
static unsigned int clock_depth;
module_param(clock_depth, uint, 0644);
MODULE_PARM_DESC(clock_depth, "0 retained firmware only; 1 CPU clocks, 2 plus FE, 3 plus SYS; requires stop_cpu");
static char *firmware;
module_param(firmware, charp, 0444);
static int overlay_id;
static struct platform_device *adopted;

struct neo_dcp {
	struct neo_dcp_owned owned;
	struct neo_epic epic;
	struct device *dev;
	void __iomem *control;
	void __iomem *dart;
	struct apple_rtkit *rtk;
	struct mutex lock;
	atomic_t failed;
	bool touched;
	bool pm_panel_touched;
	bool oslog_started;
	u32 sleeps, wakes;
	struct regmap *pmgr;
	void __iomem *retention;
	bool retention_prepared;
	bool stopped;
	bool transport_paused;
	u32 saved_control, saved_clocks[3];
	unsigned int clocks_touched;
	u64 requested_iova;
	size_t requested_size;
	u32 mapped_buffers;
	struct mutex log_lock;
	void *log_copy;
	size_t log_size;
	atomic_t log_flushes;
	u32 firmware_idle_mode;
	bool awake_trace;
};
static const unsigned int clock_offsets[] = { 0x10000, 0x318, 0x280 };

/* Only the explicit awake diagnostic delays here. Its userspace /dev/kmsg
 * collector stays runnable and fsyncs each marker before the next MMIO step.
 */
static void trace_step(struct neo_dcp *d, const char *step)
{
	if (!d->awake_trace)
		return;
	dev_info(d->dev, "DCP awake step: %s\n", step);
	msleep(200);
}

/* AppleT8140PMGR::_setPwrGateRetentionWorkaround, table at
 * 0xfffffe000c455710: DISP_CPU uses map 0x65, ADT reg[22].
 * Prepare before lowering the manual target; restore only after ACTIVE
 * and BUSY clear, before restoring AUTO. This is not an MMIO access unlock.
 */
static int retention_controls(struct neo_dcp *d, bool prepare)
{
	static const u32 offsets[] = { 0xc8c, 0xcd0 };
	static const u32 active[] = { 0, 1 }, retained[] = { 3, 0 };
	const u32 *values = prepare ? retained : active;
	unsigned int i;
	u32 value;

	if (!d->retention)
		return -ENODEV;
	if (prepare) {
		if (d->retention_prepared)
			return -EBUSY;
		for (i = 0; i < ARRAY_SIZE(offsets); i++)
			if (readl(d->retention + offsets[i]) != active[i])
				return -EBUSY;
		/* Also retain ownership of a partially written sequence. */
		d->retention_prepared = true;
	} else if (!d->retention_prepared) {
		return 0;
	}
	for (i = 0; i < ARRAY_SIZE(offsets); i++) {
		trace_step(d, prepare ? "prepare retention control" : "restore retention control");
		writel(values[i], d->retention + offsets[i]);
		value = readl(d->retention + offsets[i]);
		dev_info(d->dev, "DCP retention offset=%#x expected=%#x actual=%#x\n",
			 offsets[i], values[i], value);
		if (value != values[i])
			return -EIO;
	}
	if (!prepare)
		d->retention_prepared = false;
	return 0;
}

static int set_clock(struct neo_dcp *d, unsigned int i, bool on)
{
	u32 value, target = on ? d->saved_clocks[i] & 15 : 4;
	int ret;

	trace_step(d, on ? "clock-on read" : "clock-off read");
	ret = regmap_read(d->pmgr, clock_offsets[i], &value);

	if (ret)
		return ret;
	/* ApplePMGR::_setPSLevel first disables AUTO at target ACTIVE and
	 * waits for actual ACTIVE, before requesting a lower manual state.
	 * Combining the ownership change and clock drop in one write hung
	 * J700's DISP_CPU trial. Keep BUSY and sticky status out of writes.
	 */
	if (!on && (value & BIT(28))) {
		if ((value & 15) != 15 || (value & (BIT(31) | BIT(12) |
						       BIT(11) | BIT(10))))
			return -EBUSY;
		value &= ~(BIT(28) | BIT(11) | BIT(9) | BIT(8));
		dev_info(d->dev, "DCP manual ACTIVE offset=%#x write=%#x\n",
			 clock_offsets[i], value);
		trace_step(d, "disable AUTO at ACTIVE write");
		ret = regmap_write(d->pmgr, clock_offsets[i], value);
		trace_step(d, "disable AUTO at ACTIVE poll");
		if (!ret)
			ret = regmap_read_poll_timeout(d->pmgr, clock_offsets[i], value,
				(value & 0xff) == 0xff && !(value & (BIT(28) | BIT(11))),
				10, 10000);
		if (ret)
			return ret;
	}
	if (!on && i == 0) {
		ret = retention_controls(d, true);
		if (ret)
			return ret;
	}
	value &= ~(15 | BIT(28) | BIT(11) | BIT(9) | BIT(8));
	dev_info(d->dev, "DCP manual target offset=%#x write=%#x\n",
		 clock_offsets[i], value | target);
	trace_step(d, on ? "clock-on write" : "clock-off write");
	ret = regmap_write(d->pmgr, clock_offsets[i], value | target);
	trace_step(d, on ? "clock-on poll" : "clock-off poll");
	if (!ret)
		ret = regmap_read_poll_timeout(d->pmgr, clock_offsets[i], value,
			((value >> 4) & 15) == target && !(value & BIT(11)), 10, 10000);
	if (!ret && on && i == 0)
		ret = retention_controls(d, false);
	if (!ret && on && (d->saved_clocks[i] & BIT(28)))
		ret = regmap_write(d->pmgr, clock_offsets[i],
			(value & ~(BIT(11) | BIT(9) | BIT(8))) | BIT(28));
	dev_info(d->dev, "DCP retained clock offset=%#x target=%u result=%d\n", clock_offsets[i], target, ret);
	return ret;
}

/* Read inherited SID23 tables only. Never reset DART, allocate firmware DMA,
 * alter a PTE, or free loader-owned memory. This bounded diagnostic accepts
 * only the observed J700 two-level 16K layout and contiguous existing buffers.
 */
static bool retained_ram(u64 pa, size_t size)
{
	return pa >= 0x10000000000ULL && pa < 0x10200000000ULL &&
		size <= 0x10200000000ULL - pa;
}

static int retained_translate(u64 root, u64 va, u64 *pa)
{
	u64 *table, entry;
	int shift;

	for (shift = 25; shift >= 14; shift -= 11) {
		if (!IS_ALIGNED(root, SZ_16K) || !retained_ram(root, SZ_16K))
			return -ERANGE;
		table = memremap(root, SZ_16K, MEMREMAP_WB);
		if (!table)
			return -ENOMEM;
		entry = READ_ONCE(table[(va >> shift) & 2047]);
		memunmap(table);
		if (!(entry & 1))
			return -ENOENT;
		root = (entry & 0x3ffffffc00ULL) << 4;
	}
	*pa = root | (va & (SZ_16K - 1));
	return 0;
}

static int retained_shmem(void *cookie, struct apple_rtkit_shmem *buffer)
{
	struct neo_dcp *d = cookie;
	u64 root, first_pa = 0, pa;
	u32 ttbr, state;
	size_t offset, size;
	unsigned int i;
	int ret = -EOPNOTSUPP;

	d->requested_iova = buffer->iova;
	d->requested_size = buffer->size;
	if ((buffer->iova & ~GENMASK_ULL(35, 0)) != BIT_ULL(40) ||
	    !IS_ALIGNED(buffer->iova, SZ_16K) || !buffer->size ||
	    buffer->size > SZ_1M ||
	    (buffer->iova & GENMASK_ULL(35, 0)) + buffer->size > BIT_ULL(36) ||
	    d->mapped_buffers >= 8)
		goto fail;
	for (i = 0; i < ARRAY_SIZE(clock_offsets); i++) {
		ret = regmap_read(d->pmgr, clock_offsets[i], &state);
		if (ret || (state & 0xff) != 0xff || (state & 0x80001c00)) {
			ret = -EBUSY;
			goto fail;
		}
	}
	if (readl(d->dart + 8) != 0x2a2a0203 ||
	    readl(d->dart + 12) != 0x10020 ||
	    readl(d->dart + 0x1000 + 23 * 4) != 1) {
		ret = -EINVAL;
		goto fail;
	}
	ttbr = readl(d->dart + 0x1400 + 23 * 4);
	if ((ttbr & 3) != 1) {
		ret = -EINVAL;
		goto fail;
	}
	root = (u64)(ttbr >> 2) << 14;
	size = ALIGN(buffer->size, SZ_16K);
	for (offset = 0; offset < size; offset += SZ_16K) {
		ret = retained_translate(root, buffer->iova + offset, &pa);
		if (ret)
			goto fail;
		if (!offset)
			first_pa = pa;
		if (pa != first_pa + offset || !retained_ram(pa, SZ_16K)) {
			ret = -ERANGE;
			goto fail;
		}
	}
	if (readl(d->dart + 0x1400 + 23 * 4) != ttbr ||
	    readl(d->dart + 0x1000 + 23 * 4) != 1) {
		ret = -ESTALE;
		goto fail;
	}
	buffer->buffer = memremap(first_pa, size, MEMREMAP_WB);
	if (!buffer->buffer) {
		ret = -ENOMEM;
		goto fail;
	}
	buffer->is_mapped = true;
	d->mapped_buffers++;
	dev_info(d->dev, "Mapped existing DCP buffer DVA=%#llx PA=%#llx size=%#zx; SID23 tables unchanged\n",
		 (u64)buffer->iova, first_pa, buffer->size);
	return 0;
fail:
	atomic_set(&d->failed, 1);
	dev_err(d->dev, "Cannot adopt existing buffer DVA=%#llx size=%#zx: %d\n",
		 d->requested_iova, d->requested_size, ret);
	return ret;
}
static void retained_shmem_destroy(void *cookie, struct apple_rtkit_shmem *buffer)
{
	/* Unmap the host view only; never release firmware-owned backing pages. */
	memunmap(buffer->buffer);
}
static void application_message(void *cookie, u8 ep, u64 word)
{
	struct neo_dcp *d = cookie;

	if (ep == 0x23 && READ_ONCE(d->epic.active)) {
		if (neo_epic_message(&d->epic, word))
			atomic_set(&d->failed, 1);
		return;
	}
	if (!atomic_xchg(&d->failed, 1))
		dev_err(d->dev, "Unexpected application activity ep=%#x word=%#llx\n", ep, word);
}
static void crashed(void *cookie, const void *log, size_t size)
{
	struct neo_dcp *d = cookie;

	atomic_set(&d->failed, 1);
}

/* Native RTBuddy::_handleFlushRequest copies the coalescer before replying
 * with (message & 0xf0000000ffffffff) | (2ULL << 56). Retain a bounded raw
 * snapshot rather than decode or print firmware format strings. This trial
 * admits only coalescer zero and an in-range producer offset.
 */
static void oslog_message(void *cookie, u64 msg,
			  const struct apple_rtkit_shmem *buffer)
{
	struct neo_dcp *d = cookie;
	u64 producer = msg & GENMASK_ULL(35, 0);
	u8 type = (msg >> 56) & 15;
	int ret;

	if (type == 3 || type == 4 || type == 5) {
		dev_info(d->dev, "OSLOG source metadata %#llx\n", msg);
		return;
	}
	if (type != 2 || (msg >> 60) || !buffer->buffer ||
	    !buffer->size || buffer->size > SZ_1M || producer > buffer->size) {
		atomic_set(&d->failed, 1);
		dev_err(d->dev, "Unsupported OSLOG request %#llx size=%#zx\n",
			msg, buffer->size);
		return;
	}
	guard(mutex)(&d->log_lock);
	if (!d->log_copy) {
		d->log_copy = kvmalloc(buffer->size, GFP_KERNEL);
		d->log_size = buffer->size;
	}
	if (!d->log_copy || d->log_size != buffer->size) {
		atomic_set(&d->failed, 1);
		return;
	}
	dma_rmb();
	memcpy(d->log_copy, buffer->buffer, buffer->size);
	ret = apple_rtkit_send_message(d->rtk, 8,
		(msg & 0xf0000000ffffffffULL) | (2ULL << 56), NULL, false);
	if (ret)
		atomic_set(&d->failed, 1);
	else
		atomic_inc(&d->log_flushes);
	dev_info(d->dev, "OSLOG flush copied=%#zx producer=%#llx reply=%d\n",
		buffer->size, producer, ret);
}

static ssize_t oslog_snapshot_read(struct file *file, struct kobject *kobj,
	const struct bin_attribute *attr, char *buf, loff_t off, size_t count)
{
	struct neo_dcp *d = dev_get_drvdata(kobj_to_dev(kobj));

	guard(mutex)(&d->log_lock);
	if (!d->log_copy || off >= d->log_size)
		return 0;
	count = min_t(size_t, count, d->log_size - off);
	memcpy(buf, d->log_copy + off, count);
	return count;
}
static BIN_ATTR_ADMIN_RO(oslog_snapshot, SZ_1M);
static const struct apple_rtkit_ops ops = {
	.shmem_setup = retained_shmem,
	.shmem_destroy = retained_shmem_destroy,
	.recv_message = application_message,
	.crashed = crashed,
};

static int wake_dcp(struct neo_dcp *d)
{
	int ret;

	/* J700's first AP-ON callback asserts if endpoint 8 is not enabled. */
	if (!d->oslog_started)
		return -EAGAIN;
	while (d->clocks_touched) {
		ret = set_clock(d, d->clocks_touched - 1, true);
		if (ret) {
			atomic_set(&d->failed, 1);
			return ret; /* No ASC access until all its clocks are restored. */
		}
		d->clocks_touched--;
	}
	if (d->stopped) {
		trace_step(d, "restore RUN");
		writel(d->saved_control, d->control + 0x44);
		if (readl(d->control + 0x44) != d->saved_control) {
			atomic_set(&d->failed, 1);
			return -EIO;
		}
		d->stopped = false;
	}
	if (d->transport_paused) {
		trace_step(d, "resume mailbox");
		ret = apple_rtkit_resume_idle_transport(d->rtk);
		if (ret) {
			atomic_set(&d->failed, 1);
			return ret;
		}
		d->transport_paused = false;
	}
	trace_step(d, "AP wake");
	ret = apple_rtkit_unidle(d->rtk);

	dev_info(d->dev, "DCP retained wake result=%d control=%#x status=%#x\n", ret,
		 readl(d->control + 0x44), readl(d->control + 0x48));
	if (ret || !apple_rtkit_is_running(d->rtk)) {
		atomic_set(&d->failed, 1);
		return ret ?: -EIO;
	}
	d->touched = false;
	d->wakes++;
	return atomic_read(&d->failed) ? -EIO : 0;
}
static ssize_t status_show(struct device *dev, struct device_attribute *a, char *buf)
{
	struct neo_dcp *d = dev_get_drvdata(dev);

	guard(mutex)(&d->lock);
	if (d->clocks_touched)
		return sysfs_emit(buf, "failed=%d clocks_touched=%u (no gated ASC read)\n",
			atomic_read(&d->failed), d->clocks_touched);
	return sysfs_emit(buf, "failed=%d touched=%u sleeps=%u wakes=%u control=%#x status=%#x requested_iova=%#llx requested_size=%#zx oslog_started=%u log_flushes=%d firmware_idle_mode=%u pm_panel_touched=%u\n",
		atomic_read(&d->failed), d->touched, d->sleeps, d->wakes,
		readl(d->control + 0x44), readl(d->control + 0x48),
		d->requested_iova, d->requested_size, d->oslog_started,
		atomic_read(&d->log_flushes), d->firmware_idle_mode, d->pm_panel_touched);
}
static ssize_t start_oslog_store(struct device *dev, struct device_attribute *a,
				const char *buf, size_t count)
{
	struct neo_dcp *d = dev_get_drvdata(dev);
	int ret;

	if (!sysfs_streq(buf, "1"))
		return -EINVAL;
	guard(mutex)(&d->lock);
	if (atomic_read(&d->failed) || d->touched || d->oslog_started)
		return -EBUSY;
	ret = apple_rtkit_start_ep(d->rtk, 8);
	if (ret)
		return ret;
	d->oslog_started = true;
	dev_info(dev, "OSLOG start queued; buffer/awake-wake qualification still required\n");
	return count;
}
static ssize_t wake_store(struct device *dev, struct device_attribute *a, const char *buf, size_t count)
{
	struct neo_dcp *d = dev_get_drvdata(dev);
	int ret;

	if (!sysfs_streq(buf, "1"))
		return -EINVAL;
	guard(mutex)(&d->lock);
	if (atomic_read(&d->failed))
		return -EIO;
	/* Unlike a PM wake after IDLE, a second awake-adoption request repeats
	 * AP ON. This firmware asserts on an unchanged AP power state.
	 */
	if (d->wakes && !d->touched)
		return -EALREADY;
	ret = wake_dcp(d);
	return ret ? ret : count;
}

/* Exact J700 25F84 retained DATA page, located read-only in boot region 50.
 * Its first 1 KiB and client version match the original Mach-O DATA page at
 * VA 0x445d000. The native setter at 0x402fac0 is a single aligned store of
 * 2 to 0x445d90c; state 8 then selects the returning idle path. No TEXT edit,
 * CPU reset, page-table change or new firmware allocation is involved.
 */
static void *firmware_data_page(void)
{
	static const u8 expected_hash[SHA256_DIGEST_SIZE] = {
		0x0a, 0x93, 0x75, 0x1f, 0x08, 0x99, 0x99, 0xa9,
		0xb9, 0x41, 0x45, 0x98, 0x8a, 0x4e, 0x6a, 0x5b,
		0x4e, 0x41, 0x68, 0xec, 0x40, 0xd5, 0xad, 0xcc,
		0x71, 0x5d, 0xeb, 0xac, 0xa5, 0x4a, 0x58, 0x85,
	};
	static const char version[] = "AppleDCP-1041.120.7~1815-t8140dcp.RELEASE";
	u8 hash[SHA256_DIGEST_SIZE];
	void *page = memremap(0x101ea7e7000ULL, SZ_4K, MEMREMAP_WB);

	if (!page)
		return ERR_PTR(-ENOMEM);
	sha256(page, 0x400, hash);
	if (memcmp(hash, expected_hash, sizeof(hash)) ||
	    memcmp(page + 0x87c, version, sizeof(version))) {
		memunmap(page);
		return ERR_PTR(-ESTALE);
	}
	return page;
}

static ssize_t returning_idle_store(struct device *dev,
	struct device_attribute *a, const char *buf, size_t count)
{
	struct neo_dcp *d = dev_get_drvdata(dev);
	void *page;
	u32 state;
	int i, ret = -ESTALE;

	if (!sysfs_streq(buf, "2"))
		return -EINVAL;
	guard(mutex)(&d->lock);
	if (atomic_read(&d->failed) || d->touched || READ_ONCE(sleep_enabled) ||
	    d->sleeps || d->wakes != 1 || d->firmware_idle_mode ||
	    !d->oslog_started || !apple_rtkit_is_running(d->rtk))
		return -EBUSY;
	for (i = 0; i < ARRAY_SIZE(clock_offsets); i++) {
		if (regmap_read(d->pmgr, clock_offsets[i], &state) ||
		    (state & 0xff) != 0xff || (state & 0x80001c00))
			return -EBUSY;
	}
	if (readl(d->control + 0x44) != BIT(4))
		return -EBUSY;
	page = firmware_data_page();
	if (IS_ERR(page))
		return PTR_ERR(page);
	if (READ_ONCE(*(u32 *)(page + 0x90c)) != 1)
		goto out;
	if (atomic_read(&d->failed)) {
		ret = -EIO;
		goto out;
	}
	/* The management mutex excludes our PM requests; a fresh AP-ON ACK
	 * above established the awake epoch. Match the firmware's own setter.
	 */
	WRITE_ONCE(*(u32 *)(page + 0x90c), 2);
	dma_wmb();
	if (READ_ONCE(*(u32 *)(page + 0x90c)) != 2) {
		atomic_set(&d->failed, 1);
		ret = -EIO;
		goto out;
	}
	d->firmware_idle_mode = 2;
	dev_info(dev, "DCP 25F84 retained DATA idle mode 1 -> 2; wake qualification required\n");
	ret = 0;
out:
	memunmap(page);
	return ret ? ret : count;
}
static ssize_t scratch_dma_store(struct device *dev, struct device_attribute *a,
				const char *buf, size_t count)
{
	struct neo_dcp *d = dev_get_drvdata(dev);
	u64 pa;
	unsigned int i;
	int ret, restore;

	if (!sysfs_streq(buf, "cycle"))
		return -EINVAL;
	guard(mutex)(&d->lock);
	if (atomic_read(&d->failed) || d->touched || READ_ONCE(sleep_enabled))
		return -EBUSY;
	ret = neo_dcp_dma_prepare(&d->owned, dev, d->dart);
	if (ret) {
		if (d->owned.failed)
			atomic_set(&d->failed, 1);
		return ret;
	}
	for (i = 0; i < NEO_DCP_POOL_SIZE / SZ_16K; i++) {
		ret = retained_translate(d->owned.root_pa, d->owned.iova + i*SZ_16K, &pa);
		if (ret || pa != d->owned.pool_pa + i*SZ_16K) {
			ret = ret ?: -ESTALE;
			break;
		}
	}
	restore = neo_dcp_dma_release(&d->owned);
	dev_info(dev, "DCP scratch DMA cycle map=%d restore=%d; no buffer advertised or firmware command sent\n",
		 ret, restore);
	if (restore)
		atomic_set(&d->failed, 1);
	return ret ?: restore ?: count;
}
static DEVICE_ATTR_WO(scratch_dma);
static ssize_t epic_start_store(struct device *dev, struct device_attribute *a,
			       const char *buf, size_t count)
{
	struct neo_dcp *d = dev_get_drvdata(dev);
	struct neo_epic *e = &d->epic;
	void *page;
	int ret;

	if (!sysfs_streq(buf, "1"))
		return -EINVAL;
	guard(mutex)(&d->lock);
	if (atomic_read(&d->failed) || d->touched || !d->wakes ||
	    READ_ONCE(sleep_enabled) || e->active)
		return -EBUSY;
	page = firmware_data_page();
	if (IS_ERR(page))
		return PTR_ERR(page);
	memunmap(page);
	ret = neo_dcp_dma_prepare(&d->owned, dev, d->dart);
	if (ret)
		return ret;
	e->dev = dev;
	e->rtk = d->rtk;
	e->owned = &d->owned;
	WRITE_ONCE(e->active, true);
	ret = apple_rtkit_start_adopted_ep(d->rtk, 0x23);
	if (!ret)
		ret = neo_epic_send(e, (0x80ULL << 48) | 8);
	if (!ret && !wait_for_completion_timeout(&e->ready, msecs_to_jiffies(15000)))
		ret = -ETIMEDOUT;
	mutex_lock(&e->lock);
	ret = ret ?: e->error;
	if (ret)
		e->error = ret;
	mutex_unlock(&e->lock);
	if (ret)
		atomic_set(&d->failed, 1);
	dev_info(dev, "DCP EPIC start result=%d; buffers pinned, no display command sent\n", ret);
	return ret ?: count;
}
static ssize_t epic_status_show(struct device *dev, struct device_attribute *a, char *buf)
{
	struct neo_dcp *d = dev_get_drvdata(dev);
	struct neo_epic *e = &d->epic;

	guard(mutex)(&e->lock);
	return sysfs_emit(buf, "active=%u started=%u error=%d last=%#llx ring_size=%u announcement_size=%u advertised=%u iova=%#llx service=%u channel=%u queried=%u response_size=%u retcode=%#x power_stage=%u cycles=%u off_result=%d on_result=%d ap_cycle=%u ap_idle_result=%d ap_wake_result=%d endpoint_cycle=%u endpoint_stop_result=%d endpoint_start_result=%d iop_cycle=%u\n",
		e->active, e->started, e->error, e->last_message, e->ring_size,
		e->announcement_size, d->owned.advertised, d->owned.iova,
		e->service, e->channel, e->queried, e->response_size, e->retcode,
		e->power_stage, e->power_cycles, e->power_off_result, e->power_on_result,
		e->ap_cycle, e->ap_idle_result, e->ap_wake_result,
		e->endpoint_cycle, e->endpoint_stop_result, e->endpoint_start_result, e->iop_cycle);
}
static ssize_t epic_query_store(struct device *dev, struct device_attribute *a,
			       const char *buf, size_t count)
{
	struct neo_dcp *d = dev_get_drvdata(dev);
	struct neo_epic *e = &d->epic;
	int ret;

	if (!sysfs_streq(buf, "hpd"))
		return -EINVAL;
	guard(mutex)(&d->lock);
	if (atomic_read(&d->failed) || !e->active || !e->service)
		return -EBUSY;
	ret = neo_epic_command(e, 3, false);
	if (ret)
		return ret;
	guard(mutex)(&e->lock);
	if (e->response_size < 20 || get_unaligned_le32(e->response) != 3 ||
	    get_unaligned_le32(e->response + 4) < 20 ||
	    get_unaligned_le32(e->response + 4) > e->response_size ||
	    e->response[8] > 1)
		return -EPROTO;
	e->queried = true;
	dev_info(dev, "DCP iBoot HPD=%u timing modes=%u color modes=%u\n", e->response[8],
		 get_unaligned_le32(e->response + 12), get_unaligned_le32(e->response + 16));
	return count;
}
static ssize_t epic_cycle(struct device *dev, const char *buf, size_t count,
			  bool ap_idle, bool endpoint_cycle, bool iop_idle)
{
	struct neo_dcp *d = dev_get_drvdata(dev);
	struct neo_epic *e = &d->epic;
	u32 seconds;
	int off, on, idle = 0, wake = 0, stop = 0, start = 0;

	if (kstrtou32(buf, 0, &seconds) || !seconds || seconds > 300)
		return -EINVAL;
	guard(mutex)(&d->lock);
	if (atomic_read(&d->failed) || !e->queried || e->error ||
	    READ_ONCE(sleep_enabled) || d->touched ||
	    !apple_rtkit_is_running(d->rtk))
		return -EBUSY;
	/* Full idle is a separate experiment, only after panel shutdown and
	 * acknowledged endpoint stop, with the pinned returning-mode opt-in.
	 * It never writes CPU domains or stops clocks/RUN/mailbox transport.
	 */
	if (iop_idle && (!endpoint_cycle || !ap_idle ||
			d->firmware_idle_mode != 2 || stop_cpu || clock_depth))
		return -EINVAL;
	WRITE_ONCE(e->iop_cycle, iop_idle);
	WRITE_ONCE(e->ap_cycle, ap_idle);
	WRITE_ONCE(e->endpoint_cycle, endpoint_cycle);
	WRITE_ONCE(e->endpoint_stop_result, 0);
	WRITE_ONCE(e->endpoint_start_result, 0);
	WRITE_ONCE(e->ap_idle_result, 0);
	WRITE_ONCE(e->ap_wake_result, 0);
	WRITE_ONCE(e->power_stage, 1);
	off = neo_epic_command(e, 2, false);
	WRITE_ONCE(e->power_off_result, off);
	dev_info(dev, "DCP iBoot power-off result=%d hold=%us\n", off, seconds);
	if (!off && endpoint_cycle) {
		stop = neo_epic_stop(e);
		WRITE_ONCE(e->endpoint_stop_result, stop);
		dev_info(dev, "DCP endpoint stop result=%d\n", stop);
	}
	if (!off && !stop && ap_idle) {
		/* The AP-only path is qualified. Full IOP idle additionally
		 * requires endpoint shutdown and returning-mode admission above.
		 * Keep clocks, RUN, transport and DMA untouched in both paths.
		 */
		d->touched = true;
		idle = iop_idle ? apple_rtkit_idle(d->rtk) : apple_rtkit_idle_ap(d->rtk);
		WRITE_ONCE(e->ap_idle_result, idle);
		dev_info(dev, "DCP pipeline-off AP idle result=%d full_iop=%u\n", idle, iop_idle);
		if (!idle)
			d->sleeps++;
	}
	if (!off && !stop && !idle && !atomic_read(&d->failed)) {
		WRITE_ONCE(e->power_stage, 2);
		/* Always return to the power-on attempt, even if the userspace
		 * writer receives a signal or closes its SSH session.
		 */
		msleep(seconds * 1000);
	}
	WRITE_ONCE(e->power_stage, 3);
	if (d->touched) {
		/* A failed request may still have changed firmware state. Attempt
		 * the same recovery as the retained PM path, then fail the trial.
		 */
		wake = wake_dcp(d);
		WRITE_ONCE(e->ap_wake_result, wake);
	}
	if (!off && endpoint_cycle && !stop && !wake) {
		start = neo_epic_restart(e);
		WRITE_ONCE(e->endpoint_start_result, start);
		dev_info(dev, "DCP endpoint restart result=%d\n", start);
	}
	on = wake ?: start ?: neo_epic_command(e, 2, true);
	WRITE_ONCE(e->power_on_result, on);
	WRITE_ONCE(e->power_stage, off || stop || idle || on ? 5 : 4);
	if (!off && !stop && !idle && !on)
		e->power_cycles++;
	else
		atomic_set(&d->failed, 1);
	dev_info(dev, "DCP iBoot power-on result=%d completed cycles=%u\n", on, e->power_cycles);
	return off ?: stop ?: idle ?: on ?: count;
}
static ssize_t epic_power_cycle_store(struct device *dev, struct device_attribute *a,
				     const char *buf, size_t count)
{
	return epic_cycle(dev, buf, count, false, false, false);
}
static ssize_t epic_ap_cycle_store(struct device *dev, struct device_attribute *a,
				  const char *buf, size_t count)
{
	return epic_cycle(dev, buf, count, true, false, false);
}
static ssize_t epic_endpoint_cycle_store(struct device *dev, struct device_attribute *a,
					const char *buf, size_t count)
{
	return epic_cycle(dev, buf, count, false, true, false);
}
static ssize_t epic_iop_cycle_store(struct device *dev, struct device_attribute *a,
				   const char *buf, size_t count)
{
	return epic_cycle(dev, buf, count, true, true, true);
}
static ssize_t epic_response_read(struct file *file, struct kobject *kobj,
	const struct bin_attribute *attr, char *buf, loff_t off, size_t count)
{
	struct neo_dcp *d = dev_get_drvdata(kobj_to_dev(kobj));
	struct neo_epic *e = &d->epic;

	guard(mutex)(&e->lock);
	if (off >= e->response_size)
		return 0;
	count = min_t(size_t, count, e->response_size - off);
	memcpy(buf, e->response + off, count);
	return count;
}
static DEVICE_ATTR_WO(epic_query);
static DEVICE_ATTR_WO(epic_power_cycle);
static DEVICE_ATTR_WO(epic_ap_cycle);
static DEVICE_ATTR_WO(epic_endpoint_cycle);
static DEVICE_ATTR_WO(epic_iop_cycle);
static BIN_ATTR_ADMIN_RO(epic_response, 128);
static ssize_t epic_announcement_read(struct file *file, struct kobject *kobj,
	const struct bin_attribute *attr, char *buf, loff_t off, size_t count)
{
	struct neo_dcp *d = dev_get_drvdata(kobj_to_dev(kobj));
	struct neo_epic *e = &d->epic;

	guard(mutex)(&e->lock);
	if (off >= e->announcement_size)
		return 0;
	count = min_t(size_t, count, e->announcement_size - off);
	memcpy(buf, e->announcement + off, count);
	return count;
}
/* The advertised allocation is pinned until reboot. Expose only our own
 * coherent buffer, never arbitrary DART/physical memory. Firmware can still
 * update it, so retain pre/post captures when a stable snapshot is needed.
 */
static ssize_t epic_pool_read(struct file *file, struct kobject *kobj,
	const struct bin_attribute *attr, char *buf, loff_t off, size_t count)
{
	struct neo_dcp *d = dev_get_drvdata(kobj_to_dev(kobj));

	guard(mutex)(&d->epic.lock);
	if (!d->epic.active || !d->owned.advertised || !d->owned.pool)
		return -ENODATA;
	if (off >= NEO_DCP_POOL_SIZE)
		return 0;
	count = min_t(size_t, count, NEO_DCP_POOL_SIZE - off);
	dma_rmb();
	memcpy(buf, d->owned.pool + off, count);
	return count;
}
static BIN_ATTR_ADMIN_RO(epic_pool, NEO_DCP_POOL_SIZE);
static DEVICE_ATTR_WO(epic_start);
static DEVICE_ATTR_RO(epic_status);
static BIN_ATTR_ADMIN_RO(epic_announcement, 4096);
static DEVICE_ATTR_RO(status);
static DEVICE_ATTR_WO(wake);
static DEVICE_ATTR_WO(start_oslog);
static DEVICE_ATTR_WO(returning_idle);
static ssize_t awake_cycle_store(struct device *dev, struct device_attribute *a,
				const char *buf, size_t count);
static DEVICE_ATTR_WO(awake_cycle);
static struct attribute *neo_dcp_attrs[] = {
	&dev_attr_status.attr, &dev_attr_wake.attr, &dev_attr_start_oslog.attr,
	&dev_attr_returning_idle.attr, &dev_attr_awake_cycle.attr,
	&dev_attr_scratch_dma.attr, &dev_attr_epic_start.attr, &dev_attr_epic_status.attr,
	&dev_attr_epic_query.attr, &dev_attr_epic_power_cycle.attr,
	&dev_attr_epic_ap_cycle.attr, &dev_attr_epic_endpoint_cycle.attr, &dev_attr_epic_iop_cycle.attr, NULL
};
static const struct bin_attribute *const neo_dcp_bin_attrs[] = {
	&bin_attr_oslog_snapshot, &bin_attr_epic_announcement, &bin_attr_epic_response,
	&bin_attr_epic_pool, NULL
};
static const struct attribute_group neo_dcp_group = {
	.attrs = neo_dcp_attrs,
	.bin_attrs = neo_dcp_bin_attrs,
};
__ATTRIBUTE_GROUPS(neo_dcp);

/* Called with the lifecycle mutex held and consoles suspended. Retain all
 * endpoint buffers and IOP state; only AP IDLE has a qualified wake path.
 */
static int epic_pm_restore(struct neo_dcp *d)
{
	struct neo_epic *e = &d->epic;
	int ret;
	unsigned long deadline;

	ret = d->touched ? wake_dcp(d) : 0;
	if (ret)
		return ret;
	ret = neo_epic_command(e, 2, true);
	if (ret)
		goto failed;
	/* Power-on acknowledges before panel detection has finished. Restore
	 * detection before framebuffer console and ordinary clients resume.
	 */
	deadline = jiffies + msecs_to_jiffies(30000);
	do {
		ret = neo_epic_command(e, 3, false);
		if (ret)
			goto failed;
		mutex_lock(&e->lock);
		if (e->response_size == 20 && get_unaligned_le32(e->response) == 3 &&
		    get_unaligned_le32(e->response + 4) == 20 && e->response[8] == 1) {
			mutex_unlock(&e->lock);
			d->pm_panel_touched = false;
			return 0;
		}
		mutex_unlock(&e->lock);
		msleep(100);
	} while (time_before(jiffies, deadline));
	ret = -ETIMEDOUT;
failed:
	atomic_set(&d->failed, 1);
	return ret;
}

static int dcp_suspend(struct device *dev)
{
	struct neo_dcp *d = dev_get_drvdata(dev);
	unsigned int depth = READ_ONCE(clock_depth), i;
	bool stop = READ_ONCE(stop_cpu);
	bool ap = READ_ONCE(ap_only);
	u32 value;
	int ret;

	guard(mutex)(&d->lock);
	if (d->epic.active && (!READ_ONCE(epic_sleep) || !READ_ONCE(sleep_enabled)))
		return -EBUSY;
	if (!READ_ONCE(sleep_enabled))
		return 0;
	if (!READ_ONCE(console_suspend_enabled))
		return -EBUSY;
	if (atomic_read(&d->failed) || d->touched)
		return -EIO;
	if (!d->oslog_started || !d->wakes)
		return -EAGAIN;
	if (depth > 3 || (depth && !stop) || (ap && (depth || stop)))
		return -EINVAL;
	if (d->epic.active) {
		if (!ap || stop || depth || d->pm_panel_touched ||
		    !d->epic.queried || d->epic.error || !apple_rtkit_is_running(d->rtk))
			return -EBUSY;
		d->pm_panel_touched = true;
		ret = neo_epic_command(&d->epic, 2, false);
		if (!ret) {
			d->touched = true;
			ret = apple_rtkit_idle_ap(d->rtk);
		}
		if (!ret && atomic_read(&d->failed))
			ret = -EIO;
		if (ret) {
			epic_pm_restore(d);
			atomic_set(&d->failed, 1);
		} else {
			d->sleeps++;
		}
		dev_info(dev, "DCP native panel/AP suspend result=%d\n", ret);
		return ret;
	}
	/* Map only for an explicitly requested gate trial. This window lies
	 * outside the main PMGR syscon. No mapping or control writes for the
	 * already qualified AP-only sleep path.
	 */
	if (depth && !d->retention) {
		d->retention = devm_ioremap(dev, 0x300724000ULL, SZ_4K);
		if (!d->retention)
			return -ENOMEM;
	}
	if (depth && (d->retention_prepared ||
		      readl(d->retention + 0xc8c) != 0 ||
		      readl(d->retention + 0xcd0) != 1))
		return -EBUSY;
	for (i = 0; i < depth; i++) {
		ret = regmap_read(d->pmgr, clock_offsets[i], &d->saved_clocks[i]);
		if (ret || (d->saved_clocks[i] & 0xff) != 0xff ||
		    (d->saved_clocks[i] & (BIT(31) | BIT(12) | BIT(11) | BIT(10))))
			return -EBUSY;
	}
	d->saved_control = readl(d->control + 0x44);
	if (!(d->saved_control & BIT(4)))
		return -EBUSY;
	d->touched = true;
	trace_step(d, "firmware idle request");
	ret = ap ? apple_rtkit_idle_ap(d->rtk) : apple_rtkit_idle(d->rtk);
	dev_info(dev, "DCP retained idle ap_only=%u result=%d control=%#x status=%#x\n", ap, ret,
		 readl(d->control + 0x44), readl(d->control + 0x48));
	if (!ret && atomic_read(&d->failed))
		ret = -EIO;
	if (!ret && stop) {
		d->transport_paused = true;
		trace_step(d, "pause mailbox");
		ret = apple_rtkit_pause_idle_transport(d->rtk);
		dev_info(dev, "DCP retained transport pause after idle-gated: %d\n", ret);
		if (!ret && atomic_read(&d->failed))
			ret = -EIO;
	}
	if (!ret && stop) {
		ret = readl_poll_timeout(d->control + 0x48, value, value & 3, 10, 10000);
		dev_info(dev, "DCP firmware idle before ASC stop: status=%#x result=%d\n", value, ret);
	}
	if (!ret && stop) {
		d->stopped = true;
		trace_step(d, "stop RUN");
		writel(d->saved_control & ~BIT(4), d->control + 0x44);
		value = readl(d->control + 0x44);
		writel(value & ~BIT(5), d->control + 0x44);
		ret = readl_poll_timeout(d->control + 0x48, value, value & 3, 10, 10000);
		dev_info(dev, "DCP retained ASC stop: idle=%#x result=%d\n", value, ret);
	}
	for (i = 0; !ret && i < depth; i++) {
		d->clocks_touched = i + 1;
		ret = set_clock(d, i, false);
	}
	if (ret)
		wake_dcp(d);
	else
		d->sleeps++;
	return ret;
}
static int dcp_resume(struct device *dev)
{
	struct neo_dcp *d = dev_get_drvdata(dev);

	guard(mutex)(&d->lock);
	if (d->pm_panel_touched)
		return epic_pm_restore(d);
	return d->touched ? wake_dcp(d) : 0;
}

/* Diagnostic only: keep the host awake so a userspace log collector can
 * persist the gate boundary. It is not a suspend/power measurement. The
 * built-in VT and retained early dockchannel consoles have static lifetime;
 * do not generalize this pointer handling to removable consoles.
 * The caller must stop display clients and run the qualified panel-off helper.
 */
static ssize_t awake_cycle_store(struct device *dev, struct device_attribute *a,
				const char *buf, size_t count)
{
	struct neo_dcp *d = dev_get_drvdata(dev);
	struct console *con, *consoles[2] = {};
	unsigned int flags;
	int cookie, i, slot, ret = -EBUSY;

	if (!IS_BUILTIN(CONFIG_VT) || !sysfs_streq(buf, "1"))
		return -EINVAL;
	flags = lock_system_sleep();
	mutex_lock(&d->lock);
	if (READ_ONCE(sleep_enabled) || !READ_ONCE(stop_cpu) ||
	    READ_ONCE(ap_only) || READ_ONCE(clock_depth) != 1 ||
	    atomic_read(&d->failed) || d->touched || d->sleeps ||
	    d->wakes != 1 || d->firmware_idle_mode != 2 || d->awake_trace)
		goto unlock;
	cookie = console_srcu_read_lock();
	for_each_console_srcu(con) {
		short state = console_srcu_read_flags(con);

		if (!(state & CON_ENABLED))
			continue;
		slot = -1;
		if (!strcmp(con->name, "tty") && con->index == 0 &&
		    !(state & CON_BOOT))
			slot = 0;
		else if (!strcmp(con->name, "dockchannel") && con->index == 0 &&
			 (state & CON_BOOT))
			slot = 1;
		if (slot < 0 || (state & CON_SUSPENDED) || consoles[slot]) {
			consoles[0] = NULL;
			break;
		}
		consoles[slot] = con;
	}
	console_srcu_read_unlock(cookie);
	if (!consoles[0])
		goto unlock;
	d->awake_trace = true;
	mutex_unlock(&d->lock);
	for (i = 0; i < ARRAY_SIZE(consoles); i++)
		if (consoles[i])
			console_suspend(consoles[i]);
	WRITE_ONCE(sleep_enabled, true);
	ret = dcp_suspend(dev);
	WRITE_ONCE(sleep_enabled, false);
	if (!ret) {
		trace_step(d, "clock gated; one-second awake hold");
		msleep(1000);
		ret = dcp_resume(dev);
	}
	trace_step(d, "cycle returned");
	mutex_lock(&d->lock);
	d->awake_trace = false;
	/* Do not start framebuffer console activity after a failed gate restore. */
	if (!d->clocks_touched)
		for (i = 0; i < ARRAY_SIZE(consoles); i++)
			if (consoles[i])
				console_resume(consoles[i]);
unlock:
	mutex_unlock(&d->lock);
	unlock_system_sleep(flags);
	return ret ? ret : count;
}
static const struct dev_pm_ops pm = {
	.suspend_late = dcp_suspend,
	.resume_early = dcp_resume,
};
static int probe(struct platform_device *pdev)
{
	struct device_node *node;
	struct regmap *pmgr;
	struct resource *res;
	struct neo_dcp *d;
	u32 state;
	int ret, i;
	static const unsigned int offsets[] = { 0x280, 0x318, 0x10000 };

	if (!of_machine_is_compatible("apple,j700") || adopted)
		return -ENODEV;
	node = of_find_node_by_path("/soc/power-management@300700000");
	if (!node)
		return -ENODEV;
	pmgr = syscon_node_to_regmap(node);
	of_node_put(node);
	if (IS_ERR(pmgr))
		return PTR_ERR(pmgr);
	for (i = 0; i < ARRAY_SIZE(offsets); i++) {
		ret = regmap_read(pmgr, offsets[i], &state);
		if (ret || (state & 0xff) != 0xff || (state & BIT(11)))
			return -EBUSY;
	}
	res = platform_get_resource(pdev, IORESOURCE_MEM, 0);
	if (!res || res->start != 0x412e00000ULL || resource_size(res) != 0x4c)
		return -EINVAL;
	d = devm_kzalloc(&pdev->dev, sizeof(*d), GFP_KERNEL);
	if (!d)
		return -ENOMEM;
	d->dev = &pdev->dev;
	d->pmgr = pmgr;
	mutex_init(&d->lock);
	mutex_init(&d->log_lock);
	mutex_init(&d->epic.lock);
	init_completion(&d->epic.ready);
	init_completion(&d->epic.reply);
	init_completion(&d->epic.stopped);
	d->control = devm_platform_ioremap_resource(pdev, 0);
	if (IS_ERR(d->control))
		return PTR_ERR(d->control);
	if (!devm_request_mem_region(&pdev->dev, 0x412340000ULL, SZ_16K,
				     "neo-dcp-retained-dart"))
		return -EBUSY;
	d->dart = devm_ioremap(&pdev->dev, 0x412340000ULL, SZ_16K);
	if (!d->dart)
		return -ENOMEM;
	if (!(readl(d->control + 0x44) & BIT(4)))
		return -EINVAL;
	platform_set_drvdata(pdev, d);
	d->rtk = apple_rtkit_init_adopted(&pdev->dev, d, NULL, 0, &ops);
	if (IS_ERR(d->rtk))
		return PTR_ERR(d->rtk);
	apple_rtkit_set_oslog_handler(d->rtk, oslog_message);
	__module_get(THIS_MODULE);
	adopted = pdev;
	dev_info(d->dev, "Inherited DCP management transport adopted; module pinned until reboot\n");
	return 0;
}
static const struct of_device_id match[] = { { .compatible = "apple,j700-dcp-retained-trial" }, {} };
static struct platform_driver driver = {
	.probe = probe,
	.driver = { .name = "neo-dcp-retained", .of_match_table = match, .pm = &pm,
		.dev_groups = neo_dcp_groups, .suppress_bind_attrs = true },
};
static int __init init(void)
{
	const struct firmware *fw;
	struct device_node *node;
	int ret;

	if (!firmware || !of_machine_is_compatible("apple,j700"))
		return -ENODEV;
	node = of_find_node_by_path("/soc/iommu@412340000");
	if (node) {
		of_node_put(node);
		return -EEXIST;
	}
	node = of_find_node_by_path("/soc/mailbox@412e08000");
	if (node) {
		of_node_put(node);
		return -EEXIST;
	}
	ret = request_firmware(&fw, firmware, NULL);
	if (ret)
		return ret;
	ret = platform_driver_register(&driver);
	if (!ret) {
		ret = of_overlay_fdt_apply(fw->data, fw->size, &overlay_id, NULL);
		if (ret)
			platform_driver_unregister(&driver);
	}
	release_firmware(fw);
	return ret;
}
static void __exit fini(void)
{
	of_overlay_remove(&overlay_id);
	platform_driver_unregister(&driver);
}
module_init(init);
module_exit(fini);
MODULE_LICENSE("GPL");
MODULE_DESCRIPTION("J700 inherited DCP retained-sleep transport qualification");
