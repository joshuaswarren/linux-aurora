// SPDX-License-Identifier: GPL-2.0-only
/*
 * Apple DART (Device Address Resolution Table) IOMMU driver
 *
 * Copyright (C) 2021 The Asahi Linux Contributors
 *
 * Based on arm/arm-smmu/arm-ssmu.c and arm/arm-smmu-v3/arm-smmu-v3.c
 *  Copyright (C) 2013 ARM Limited
 *  Copyright (C) 2015 ARM Limited
 * and on exynos-iommu.c
 *  Copyright (c) 2011,2016 Samsung Electronics Co., Ltd.
 */

#include <linux/atomic.h>
#include <linux/bitfield.h>
#include <linux/clk.h>
#include <linux/dev_printk.h>
#include <linux/dma-mapping.h>
#include <linux/err.h>
#include <linux/interrupt.h>
#include <linux/io-pgtable.h>
#include <linux/iommu.h>
#include <linux/iopoll.h>
#include <linux/minmax.h>
#include <linux/module.h>
#include <linux/of.h>
#include <linux/of_address.h>
#include <linux/of_iommu.h>
#include <linux/of_platform.h>
#include <linux/pci.h>
#include <linux/platform_device.h>
#include <linux/pm_runtime.h>
#include <linux/slab.h>
#include <linux/swab.h>
#include <linux/types.h>

#if IS_ENABLED(CONFIG_APPLE_DART_KUNIT_TEST)
#include <kunit/test.h>
#endif

#include "dma-iommu.h"

#define DART_MAX_STREAMS 256
#define DART_MAX_TTBR 4
#define MAX_DARTS_PER_DEVICE 3

/* Common registers */

#define DART_PARAMS1 0x00
#define DART_PARAMS1_PAGE_SHIFT GENMASK(27, 24)

#define DART_PARAMS2 0x04
#define DART_PARAMS2_BYPASS_SUPPORT BIT(0)

/* T8020/T6000 registers */

#define DART_T8020_STREAM_COMMAND 0x20
#define DART_T8020_STREAM_COMMAND_BUSY BIT(2)
#define DART_T8020_STREAM_COMMAND_INVALIDATE BIT(20)

#define DART_T8020_STREAM_SELECT 0x34

#define DART_T8020_ERROR 0x40
#define DART_T8020_ERROR_STREAM GENMASK(27, 24)
#define DART_T8020_ERROR_CODE GENMASK(11, 0)
#define DART_T8020_ERROR_FLAG BIT(31)

#define DART_T8020_ERROR_READ_FAULT BIT(4)
#define DART_T8020_ERROR_WRITE_FAULT BIT(3)
#define DART_T8020_ERROR_NO_PTE BIT(2)
#define DART_T8020_ERROR_NO_PMD BIT(1)
#define DART_T8020_ERROR_NO_TTBR BIT(0)

#define DART_T8020_CONFIG 0x60
#define DART_T8020_CONFIG_LOCK BIT(15)

#define DART_STREAM_COMMAND_BUSY_TIMEOUT 100

/*
 * A FLUSH_SID on a live display stream has to drain that stream's in-flight
 * translations, and on T8140 disp0 that measurably exceeds the 100us upstream
 * allows -- always the post-write poll, never a busy engine beforehand
 * (TLB_CMD reads back 0x87008100: our own command with BUSY still set).  Giving
 * up early does not stop the flush, it just means we stop waiting for it and
 * tell the caller the invalidate failed, so a device read can race the
 * still-running invalidate.  Wait long enough for it to finish instead, and
 * report how long it really took.
 */
#define DART_T8110_FLUSH_BUSY_TIMEOUT 2000

#define DART_T8020_ERROR_ADDR_HI 0x54
#define DART_T8020_ERROR_ADDR_LO 0x50

#define DART_T8020_STREAMS_ENABLE 0xfc

#define DART_T8020_TCR                  0x100
#define DART_T8020_TCR_TRANSLATE_ENABLE BIT(7)
#define DART_T8020_TCR_BYPASS_DART      BIT(8)
#define DART_T8020_TCR_BYPASS_DAPF      BIT(12)

#define DART_T8020_TTBR       0x200
#define DART_T8020_USB4_TTBR  0x400
#define DART_T8020_TTBR_VALID BIT(31)
#define DART_T8020_TTBR_ADDR_FIELD_SHIFT 0
#define DART_T8020_TTBR_SHIFT 12

/* T8110 registers */

#define DART_T8110_PARAMS3 0x08
#define DART_T8110_PARAMS3_PA_WIDTH GENMASK(29, 24)
#define DART_T8110_PARAMS3_VA_WIDTH GENMASK(21, 16)
#define DART_T8110_PARAMS3_VER_MAJ GENMASK(15, 8)
#define DART_T8110_PARAMS3_VER_MIN GENMASK(7, 0)

#define DART_T8110_PARAMS4 0x0c
#define DART_T8110_PARAMS4_NUM_CLIENTS GENMASK(24, 16)
#define DART_T8110_PARAMS4_NUM_SIDS GENMASK(8, 0)

#define DART_T8110_TLB_CMD              0x80
#define DART_T8110_TLB_CMD_BUSY         BIT(31)
#define DART_T8110_TLB_CMD_OP           GENMASK(10, 8)
#define DART_T8110_TLB_CMD_OP_FLUSH_ALL 0
#define DART_T8110_TLB_CMD_OP_FLUSH_SID 1
#define DART_T8110_TLB_CMD_OP_FLUSH_UNLOCK 4
#define DART_T8110_TLB_CMD_STREAM       GENMASK(7, 0)
#define DART_T8110_TLB_CMD_V2           BIT(15)
#define DART_T8110_TLB_CMD_RANGE        BIT(14)
#define DART_T8110_TLB_CMD_STT          BIT(13)
#define DART_T8110_TLB_START            0x98
#define DART_T8110_TLB_END              0xa0

#define DART_T8110_ERROR 0x100
#define DART_T8110_ERROR_STREAM GENMASK(27, 20)
#define DART_T8110_ERROR_CODE GENMASK(14, 0)
#define DART_T8110_ERROR_FLAG BIT(31)

#define DART_T8110_ERROR_MASK 0x104

#define DART_T8110_ERROR_READ_FAULT BIT(5)
#define DART_T8110_ERROR_WRITE_FAULT BIT(4)
#define DART_T8110_ERROR_NO_PTE BIT(3)
#define DART_T8110_ERROR_NO_PMD BIT(2)
#define DART_T8110_ERROR_NO_PGD BIT(1)
#define DART_T8110_ERROR_NO_TTBR BIT(0)

#define DART_T8110_ERROR_ADDR_LO 0x170
#define DART_T8110_ERROR_ADDR_HI 0x174

#define DART_T8110_ERROR_STREAMS 0x1c0

/* DART v2.2 and later expose one exception record per SID. */
/*
 * T8110 v2 (version >= 0x202) exception reporting, from AppleT8110DART and the
 * SPTM firmware that performs its register writes.
 *
 * 0x4000 + 4*i is a *pending bitmap*, one word per 32 SIDs: bit (sid % 32) set
 * means SID sid has an exception.  It is NOT a per-SID code word -- reading it
 * as one made every "exception:0x00000001" in our logs mean "SID 0 pending",
 * which we misreported as code 1 "write protect".  The real code lives in the
 * per-SID record at 0x8000 + 0x40*sid.
 *
 * The bitmap is write-1-to-clear (SPTM: ldr w12,[x14,x13]; str w12,[x14,x13];
 * dsb sy). Nothing acked it before, so one latched bit re-fired indefinitely.
 *
 * The legacy ERROR register at 0x100 is deliberately unused on v2 hardware --
 * AppleT8110DART only consults it when version <= 0x201 -- which is why it
 * reads zero while these registers are live.
 */
#define DART_T8110_EXCEPTION(i) (0x4000 + 4 * (i))
#define DART_T8110_ERR_SID(sid) (0x8000 + 0x40 * (sid))
#define DART_T8110_ERR_SID_STATUS(sid) (DART_T8110_ERR_SID(sid) + 0x00)
#define DART_T8110_ERR_SID_STATUS_WRITE BIT(8)
#define DART_T8110_EXCEPTION_CODE GENMASK(3, 0)
#define DART_T8110_EXCEPTION_LEVEL GENMASK(13, 12)

/* DART v2.3 adds a block-level exception summary. */
#define DART_T8110_BLOCK_ERROR 0x704
#define DART_T8110_BLOCK_ERROR_MASK 0x777

#define DART_T8110_EXCEPTION_INFO(sid) (0x8000 + 0x40 * (sid))
#define DART_T8110_EXCEPTION_ADDR_LO(sid) (DART_T8110_ERR_SID(sid) + 0x8)
#define DART_T8110_EXCEPTION_ADDR_HI(sid) (DART_T8110_ERR_SID(sid) + 0xc)

#define DART_T8110_PROTECT 0x200
#define DART_T8110_UNPROTECT 0x204
#define DART_T8110_PROTECT_LOCK 0x208
#define DART_T8110_PROTECT_TTBR_TCR BIT(0)
#define DART_T8110_DIAG_LOCK 0x210
#define DART_T8110_DIAG_LOCK_ON_ERR BIT(1)

#define DART_T8110_ENABLE_STREAMS  0xc00
#define DART_T8110_DISABLE_STREAMS 0xc20

#define DART_T8110_TCR                  0x1000
#define DART_T8110_TCR_REMAP            GENMASK(11, 8)
#define DART_T8110_TCR_REMAP_EN         BIT(7)
#define DART_T8110_TCR_FOUR_LEVEL       BIT(3)
#define DART_T8110_TCR_BYPASS_DAPF      BIT(2)
#define DART_T8110_TCR_BYPASS_DART      BIT(1)
#define DART_T8110_TCR_TRANSLATE_ENABLE BIT(0)

#define DART_T8110_TTBR       0x1400
#define DART_T8110_TTBR_VALID BIT(0)
#define DART_T8110_TTBR_ADDR_FIELD_SHIFT 2
#define DART_T8110_TTBR_SHIFT 14

#define DART_TCR(dart, sid) ((dart)->hw->tcr + ((sid) << 2))

#define DART_TTBR(dart, sid, idx) ((dart)->hw->ttbr + \
				   (((dart)->hw->ttbr_count * (sid)) << 2) + \
				   ((idx) << 2))

struct apple_dart_stream_map;

enum dart_type {
	DART_T8020,
	DART_T6000,
	DART_T8110,
};

struct apple_dart_hw {
	enum dart_type type;
	irqreturn_t (*irq_handler)(int irq, void *dev);
	int (*invalidate_tlb)(struct apple_dart_stream_map *stream_map);

	u32 oas;
	enum io_pgtable_fmt fmt;

	int max_sid_count;

	u32 lock;
	u32 lock_bit;

	u32 error;

	u32 enable_streams;

	u32 tcr;
	u32 tcr_enabled;
	u32 tcr_disabled;
	u32 tcr_bypass;
	u32 tcr_4level;

	u32 ttbr;
	u32 ttbr_valid;
	u32 ttbr_addr_field_shift;
	u32 ttbr_shift;
	int ttbr_count;
};

/*
 * Private structure associated with each DART device.
 *
 * @dev: device struct
 * @hw: SoC-specific hardware data
 * @regs: mapped MMIO region
 * @irq: interrupt number, can be shared with other DARTs
 * @clks: clocks associated with this DART
 * @num_clks: number of @clks
 * @lock: lock for hardware operations involving this dart
 * @pgsize: pagesize supported by this DART
 * @supports_bypass: indicates if this DART supports bypass mode
 * @locked: indicates if this DART is locked
 * @tlb_cmd_v2: indicates the v2.2+ TLB command format
 * @force_available: keep the runtime-PM reference acquired during probe
 * @sid2group: maps stream ids to iommu_groups
 * @iommu: iommu core device
 */
struct apple_dart {
	struct device *dev;
	const struct apple_dart_hw *hw;

	void __iomem *regs;

	int irq;
	struct clk_bulk_data *clks;
	int num_clks;

	spinlock_t lock;

	u32 ias;
	u32 oas;
	u32 pgsize;
	u32 num_streams;
	u32 supports_bypass : 1;
	u32 four_level : 1;
	u32 locked : 1;
	u32 tlb_cmd_v2 : 1;
	u32 force_available : 1;
	u32 retained_bypass;
	u32 owned_streams;
	u32 static_dart_bypass;
	u16 version;

	dma_addr_t dma_min;
	dma_addr_t dma_max;

	struct iommu_group *sid2group[DART_MAX_STREAMS];
	struct iommu_device iommu;
	DECLARE_BITMAP(static_bypass, DART_MAX_STREAMS);
	DECLARE_BITMAP(static_enabled, DART_MAX_STREAMS);
	bool has_static_enabled;
	/* t8110 stream remap: source streams folded onto a target stream */
	DECLARE_BITMAP(remapped, DART_MAX_STREAMS);
	u8 remap_target[DART_MAX_STREAMS];

	u32 save_tcr[DART_MAX_STREAMS];
	u32 save_ttbr[DART_MAX_STREAMS][DART_MAX_TTBR];

	u64 *locked_ttbr[DART_MAX_STREAMS][DART_MAX_TTBR];
	u64 *locked_ttbr_saved[DART_MAX_STREAMS][DART_MAX_TTBR];
};

/*
 * Convenience struct to identify streams.
 *
 * The normal variant is used inside apple_dart_master_cfg which isn't written
 * to concurrently.
 * The atomic variant is used inside apple_dart_domain where we have to guard
 * against races from potential parallel calls to attach/detach_device.
 * Note that even inside the atomic variant the apple_dart pointer is not
 * protected: This pointer is initialized once under the domain init mutex
 * and never changed again afterwards. Devices with different dart pointers
 * cannot be attached to the same domain.
 *
 * @dart dart pointer
 * @sid stream id bitmap
 */
struct apple_dart_stream_map {
	struct apple_dart *dart;
	DECLARE_BITMAP(sidmap, DART_MAX_STREAMS);
};
struct apple_dart_atomic_stream_map {
	struct apple_dart *dart;
	atomic_long_t sidmap[BITS_TO_LONGS(DART_MAX_STREAMS)];
};

/*
 * This structure is attached to each iommu domain handled by a DART.
 *
 * @pgtbl_ops: pagetable ops allocated by io-pgtable
 * @finalized: true if the domain has been completely initialized
 * @init_lock: protects domain initialization
 * @stream_maps: streams attached to this domain (valid for DMA/UNMANAGED only)
 * @domain: core iommu domain pointer
 */
struct apple_dart_domain {
	struct io_pgtable_ops *pgtbl_ops;

	bool finalized;
	u64 mask;
	dma_addr_t dma_min;
	dma_addr_t dma_max;
	struct mutex init_lock;
	struct apple_dart_atomic_stream_map stream_maps[MAX_DARTS_PER_DEVICE];

	struct iommu_domain domain;
};

/*
 * This structure is attached to devices with dev_iommu_priv_set() on of_xlate
 * and contains a list of streams bound to this device.
 * So far the worst case seen is a single device with two streams
 * from different darts, such that this simple static array is enough.
 *
 * @streams: streams for this device
 */
struct apple_dart_master_cfg {
	/* Intersection of DART capabilitles */
	u32 supports_bypass : 1;
	u32 locked : 1;

	struct apple_dart_stream_map stream_maps[MAX_DARTS_PER_DEVICE];
};

/*
 * Helper macro to iterate over apple_dart_master_cfg.stream_maps and
 * apple_dart_domain.stream_maps
 *
 * @i int used as loop variable
 * @base pointer to base struct (apple_dart_master_cfg or apple_dart_domain)
 * @stream pointer to the apple_dart_streams struct for each loop iteration
 */
#define for_each_stream_map(i, base, stream_map)                               \
	for (i = 0, stream_map = &(base)->stream_maps[0];                      \
	     i < MAX_DARTS_PER_DEVICE && stream_map->dart;                     \
	     stream_map = &(base)->stream_maps[++i])

static struct platform_driver apple_dart_driver;
static const struct iommu_ops apple_dart_iommu_ops;
static bool j700_deferred_probe_ready;
module_param_named(j700_deferred_probe_ready, j700_deferred_probe_ready,
		   bool, 0644);
MODULE_PARM_DESC(j700_deferred_probe_ready,
		 "Allow a J700 APCIe DART marked for deferred probe to bind");

static struct apple_dart_domain *to_dart_domain(struct iommu_domain *dom)
{
	return container_of(dom, struct apple_dart_domain, domain);
}

static void
apple_dart_hw_enable_translation(struct apple_dart_stream_map *stream_map, int levels)
{
	struct apple_dart *dart = stream_map->dart;
	u32 tcr = dart->hw->tcr_enabled;
	int sid;

	if (levels == 4)
		tcr |= dart->hw->tcr_4level;

	WARN_ON(levels != 3 && levels != 4);
	WARN_ON(levels == 4 && !dart->four_level);
	for_each_set_bit(sid, stream_map->sidmap, dart->num_streams)
		writel(tcr, dart->regs + DART_TCR(dart, sid));
}

static void apple_dart_hw_disable_dma(struct apple_dart_stream_map *stream_map)
{
	struct apple_dart *dart = stream_map->dart;
	int sid;

	for_each_set_bit(sid, stream_map->sidmap, dart->num_streams)
		writel(dart->hw->tcr_disabled, dart->regs + DART_TCR(dart, sid));
}

static void
apple_dart_hw_enable_bypass(struct apple_dart_stream_map *stream_map)
{
	struct apple_dart *dart = stream_map->dart;
	int sid;

	WARN_ON(!stream_map->dart->supports_bypass);
	for_each_set_bit(sid, stream_map->sidmap, dart->num_streams)
		writel(dart->hw->tcr_bypass,
		       dart->regs + DART_TCR(dart, sid));
}

static void apple_dart_hw_set_ttbr(struct apple_dart_stream_map *stream_map,
				   u8 idx, phys_addr_t paddr)
{
	struct apple_dart *dart = stream_map->dart;
	int sid;

	WARN_ON(paddr & ((1 << dart->hw->ttbr_shift) - 1));
	for_each_set_bit(sid, stream_map->sidmap, dart->num_streams)
		writel(dart->hw->ttbr_valid |
		       (paddr >> dart->hw->ttbr_shift) << dart->hw->ttbr_addr_field_shift,
		       dart->regs + DART_TTBR(dart, sid, idx));
}

static void apple_dart_hw_clear_ttbr(struct apple_dart_stream_map *stream_map,
				     u8 idx)
{
	struct apple_dart *dart = stream_map->dart;
	int sid;

	for_each_set_bit(sid, stream_map->sidmap, dart->num_streams)
		writel(0, dart->regs + DART_TTBR(dart, sid, idx));
}

static void
apple_dart_hw_clear_all_ttbrs(struct apple_dart_stream_map *stream_map)
{
	int i;

	for (i = 0; i < stream_map->dart->hw->ttbr_count; ++i)
		apple_dart_hw_clear_ttbr(stream_map, i);
}

static int
apple_dart_hw_map_locked_ttbr(struct apple_dart_stream_map *stream_map, u8 idx)
{
	struct apple_dart *dart = stream_map->dart;
	int sid;

	for_each_set_bit(sid, stream_map->sidmap, dart->num_streams) {
		u32 ttbr;
		phys_addr_t phys;
		u64 *l1_tbl;
		u64 *saved;

		if (dart->locked_ttbr[sid][idx])
			continue;

		ttbr = readl(dart->regs + DART_TTBR(dart, sid, idx));

		if (!(ttbr & dart->hw->ttbr_valid)) {
			dev_err(dart->dev, "Invalid ttbr[%u] for locked dart\n",
				idx);
			return -EIO;
		}

		ttbr &= ~dart->hw->ttbr_valid;

		if (dart->hw->ttbr_addr_field_shift)
			ttbr >>= dart->hw->ttbr_addr_field_shift;
		phys = ((phys_addr_t) ttbr) << dart->hw->ttbr_shift;

		l1_tbl = devm_memremap(dart->dev, phys, dart->pgsize,
				       MEMREMAP_WB);
		if (!l1_tbl)
			return -ENOMEM;

		saved = dart->locked_ttbr_saved[sid][idx];
		if (!saved) {
			saved = devm_kmalloc(dart->dev, dart->pgsize, GFP_KERNEL);
			if (!saved) {
				devm_memunmap(dart->dev, l1_tbl);
				return -ENOMEM;
			}
			dart->locked_ttbr_saved[sid][idx] = saved;
		}
		memcpy(saved, l1_tbl, dart->pgsize);
		dart->locked_ttbr[sid][idx] = l1_tbl;
	}

	return 0;
}

static void apple_dart_copy_locked_ttbr(u64 *dst, const u64 *src, size_t size)
{
	memcpy(dst, src, size);
	dma_wmb();
}

static bool
apple_dart_hw_restore_locked_ttbr(struct apple_dart_stream_map *stream_map,
				  u8 idx)
{
	struct apple_dart *dart = stream_map->dart;
	bool restored = false;
	int sid;

	for_each_set_bit(sid, stream_map->sidmap, dart->num_streams) {
		u64 *l1_tbl = dart->locked_ttbr[sid][idx];
		u64 *saved = dart->locked_ttbr_saved[sid][idx];

		if (!l1_tbl || !saved)
			continue;

		apple_dart_copy_locked_ttbr(l1_tbl, saved, dart->pgsize);
		restored = true;
	}

	return restored;
}

static int
apple_dart_hw_unmap_locked_ttbr(struct apple_dart_stream_map *stream_map,
				u8 idx)
{
	struct apple_dart *dart = stream_map->dart;
	int sid;

	for_each_set_bit(sid, stream_map->sidmap, dart->num_streams) {
		u64 *l1_tbl = dart->locked_ttbr[sid][idx];
		u64 *saved = dart->locked_ttbr_saved[sid][idx];

		if (l1_tbl) {
			if (saved)
				apple_dart_copy_locked_ttbr(l1_tbl, saved, dart->pgsize);
			devm_memunmap(dart->dev, l1_tbl);
		}
		dart->locked_ttbr[sid][idx] = NULL;
	}
	return 0;
}

/* Mirrors APPLE_DART_PTE_VALID in io-pgtable-dart.c. */
#define APPLE_DART_L1E_VALID	BIT(0)

/*
 * Publish Linux's L1 entries into a live inherited L1 without clobbering the
 * firmware's.
 *
 * This used to be a blanket memcpy of our cloned L1 over the hardware's, run
 * again on every IOTLB sync.  On a locked DART that is wrong: we are not the
 * only writer of these tables.  The DCP firmware remaps disp0 PTEs itself (the
 * ADT marks the DART "allow-pte-remap") and keeps editing the *original* L2s
 * after we have cloned them, so the copy froze the firmware's tables at attach
 * time and then re-stamped that stale snapshot over the live L1 forever after.
 * A page the firmware had promoted to writable but whose snapshot still said
 * NO_WRITE then faulted on the firmware's next write -- a write-protect
 * exception inside the display carveout, at whatever address the firmware
 * happened to have reached, on roughly one boot in four (whenever probe won the
 * race against the firmware's remap sequence).  The coprocessor took the AXI
 * reject, panicked, and never answered RTKit INIT.
 *
 * The two owners do not overlap: the firmware's mappings live in the low L1
 * slots (the carveout at DVA 0x1ea0000 is slot 0..3) while our DMA window sits
 * at the top (0x10fff000000 masked to ias=36 lands in slot 2040+).  So publish
 * an entry only where the live L1 has none, and leave every slot the firmware
 * owns pointing at the firmware's own L2s.
 */
static void apple_dart_publish_locked_ttbr(u64 *live, const u64 *ours,
					   size_t size)
{
	size_t i, n = size / sizeof(u64);
	unsigned int published = 0;

	for (i = 0; i < n; i++) {
		u64 mine = READ_ONCE(ours[i]);

		if (!(mine & APPLE_DART_L1E_VALID))
			continue;
		if (READ_ONCE(live[i]) & APPLE_DART_L1E_VALID)
			continue;	/* firmware owns this slot; hands off */

		WRITE_ONCE(live[i], mine);
		published++;
	}

	/*
	 * Unconditional: the steady-state case publishes nothing (our slots are
	 * install-once), but the caller issues a TLB invalidate straight after,
	 * and the leaf PTE writes still have to be visible to the DART first.
	 * The blanket-memcpy version this replaced always executed a dma_wmb(),
	 * so making it conditional would have quietly dropped that barrier.
	 */
	(void)published;
	dma_wmb();
}

static int
apple_dart_hw_sync_locked(struct io_pgtable_cfg *cfg,
			  struct apple_dart_stream_map *stream_map)
{
	struct apple_dart *dart = stream_map->dart;
	int sid;

	for_each_set_bit(sid, stream_map->sidmap, dart->num_streams) {
		for (int idx = 0; idx < dart->hw->ttbr_count; idx++) {
			u64 *ttbrep = dart->locked_ttbr[sid][idx];
			u64 *ptep = cfg->apple_dart_cfg.ttbr[idx];
			if (!ttbrep || !ptep)
				continue;
			/*
			 * Publish, never blanket-copy.  This runs from
			 * apple_dart_domain_flush_tlb() on *every* DMA map, so a
			 * memcpy here re-stamps the attach-time snapshot over all
			 * 2048 L1 entries of a table the DCP firmware is still
			 * walking and editing, reverting every L2 pointer it has
			 * installed since attach.
			 */
			apple_dart_publish_locked_ttbr(ttbrep, ptep, dart->pgsize);
		}
	}

	return 0;
}

static int
apple_dart_t8020_hw_stream_command(struct apple_dart_stream_map *stream_map,
			     u32 command)
{
	unsigned long flags;
	int ret, i;
	u32 command_reg;

	spin_lock_irqsave(&stream_map->dart->lock, flags);

	for (i = 0; i < BITS_TO_U32(stream_map->dart->num_streams); i++)
		writel(stream_map->sidmap[i],
		       stream_map->dart->regs + DART_T8020_STREAM_SELECT + 4 * i);
	writel(command, stream_map->dart->regs + DART_T8020_STREAM_COMMAND);

	ret = readl_poll_timeout_atomic(
		stream_map->dart->regs + DART_T8020_STREAM_COMMAND, command_reg,
		!(command_reg & DART_T8020_STREAM_COMMAND_BUSY), 1,
		DART_STREAM_COMMAND_BUSY_TIMEOUT);

	spin_unlock_irqrestore(&stream_map->dart->lock, flags);

	if (ret) {
		dev_err(stream_map->dart->dev,
			"busy bit did not clear after command %x for streams %lx\n",
			command, stream_map->sidmap[0]);
		return ret;
	}

	return 0;
}

static int
apple_dart_t8110_hw_tlb_command_range(struct apple_dart_stream_map *stream_map,
				u32 command, bool range, u64 first, u64 last)
{
	struct apple_dart *dart = stream_map->dart;
	bool busy_before = false;
	s64 slowest_us = 0, us = 0;
	int slowest_sid = -1, bad_sid = -1;
	u32 last_val = 0;
	unsigned long flags;
	ktime_t start;
	int ret = 0;
	int sid;

	spin_lock_irqsave(&dart->lock, flags);

	for_each_set_bit(sid, stream_map->sidmap, dart->num_streams) {
		u32 val = FIELD_PREP(DART_T8110_TLB_CMD_OP, command) |
			FIELD_PREP(DART_T8110_TLB_CMD_STREAM, sid);
		u32 pre;

		if (command == DART_T8110_TLB_CMD_OP_FLUSH_SID && dart->tlb_cmd_v2)
			val |= DART_T8110_TLB_CMD_V2;

		/*
		 * Wait for the engine before stamping the command.  TLB_CMD is a
		 * single shared command port: writing it while BUSY is set drops
		 * the write, so the invalidate silently never happens and the
		 * next access through the freshly mapped IOVA hits a stale
		 * negative TLB entry.  On disp0 the DCP firmware takes that
		 * reject as an AXI error and panics.  A dropped command also
		 * leaves BUSY set, so the next caller's command is dropped too --
		 * which is why these arrive in bursts of dozens rather than
		 * singly.
		 */
		ret = readl_poll_timeout_atomic(
			dart->regs + DART_T8110_TLB_CMD, pre,
			!(pre & DART_T8110_TLB_CMD_BUSY), 1,
			DART_STREAM_COMMAND_BUSY_TIMEOUT);
		if (ret) {
			last_val = pre;
			busy_before = true;
			bad_sid = sid;
			break;
		}

		if (range) {
			val |= DART_T8110_TLB_CMD_RANGE | DART_T8110_TLB_CMD_STT;
			writel((first >> 14) << 2, dart->regs + DART_T8110_TLB_START);
			writel((last >> 14) << 2, dart->regs + DART_T8110_TLB_END);
		}

		/* Page-table writes must land before the DART is told to flush. */
		dma_wmb();
		writel(val, dart->regs + DART_T8110_TLB_CMD);

		start = ktime_get();
		ret = readl_poll_timeout_atomic(
			dart->regs + DART_T8110_TLB_CMD, val,
			!(val & DART_T8110_TLB_CMD_BUSY), 1,
			DART_T8110_FLUSH_BUSY_TIMEOUT);
		us = ktime_to_us(ktime_sub(ktime_get(), start));

		last_val = val;
		if (us > slowest_us) {
			slowest_us = us;
			slowest_sid = sid;
		}
		if (ret) {
			bad_sid = sid;
			break;
		}

	}

	spin_unlock_irqrestore(&dart->lock, flags);

	if (ret) {
		dev_err_ratelimited(stream_map->dart->dev,
			"TLB engine %s for command %x stream %d after %lldus (TLB_CMD %#010x)\n",
			busy_before ? "still busy BEFORE the command" :
				      "busy bit did not clear",
			command, bad_sid, slowest_us, last_val);
		return ret;
	}

	/* Learn the real distribution: anything past the old budget is notable. */
	if (slowest_us > DART_STREAM_COMMAND_BUSY_TIMEOUT)
		dev_warn_ratelimited(stream_map->dart->dev,
			"TLB command %x stream %d took %lldus (over the %dus budget) but completed\n",
			command, slowest_sid, slowest_us,
			DART_STREAM_COMMAND_BUSY_TIMEOUT);

	return 0;
}

static int
apple_dart_t8020_hw_invalidate_tlb(struct apple_dart_stream_map *stream_map)
{
	return apple_dart_t8020_hw_stream_command(
		stream_map, DART_T8020_STREAM_COMMAND_INVALIDATE);
}

static int
apple_dart_t8110_hw_invalidate_tlb(struct apple_dart_stream_map *stream_map)
{
	return apple_dart_t8110_hw_tlb_command_range(
		stream_map, DART_T8110_TLB_CMD_OP_FLUSH_SID, false, 0, 0);
}

static const char *apple_dart_t8110_v2_fault_name(u32 exception);

/*
 * Drain exceptions latched before Linux took the DART over.
 *
 * apple_dart_hw_reset() clears pending errors only for unlocked DARTs. Drain
 * firmware-era records before enabling the interrupt on a locked DART so the
 * handler starts with live faults only.
 */
static void apple_dart_drain_stale_exceptions(struct apple_dart *dart)
{
	unsigned int i, drained = 0;

	if (dart->hw->type != DART_T8110 || dart->version < 0x0202)
		return;

	for (i = 0; i < DIV_ROUND_UP(dart->num_streams, 32) && i < 8; i++) {
		u32 pending = readl(dart->regs + DART_T8110_EXCEPTION(i));
		unsigned long bits = pending;
		unsigned int bit;

		if (!pending)
			continue;

		for_each_set_bit(bit, &bits, 32) {
			unsigned int sid = i * 32 + bit;
			u32 status;
			u64 addr;

			if (sid >= dart->num_streams)
				break;

			status = readl(dart->regs +
				       DART_T8110_ERR_SID_STATUS(sid));
			addr = readl(dart->regs +
				     DART_T8110_EXCEPTION_ADDR_LO(sid)) |
			       (u64)readl(dart->regs +
					  DART_T8110_EXCEPTION_ADDR_HI(sid)) << 32;
			dev_info(dart->dev,
				 "stale exception latched before probe: SID:%u %s %s at %#llx (status %#010x)\n",
				 sid,
				 status & DART_T8110_ERR_SID_STATUS_WRITE ?
					 "write" : "read",
				 apple_dart_t8110_v2_fault_name(status),
				 addr, status);
			drained++;
		}

		writel(pending, dart->regs + DART_T8110_EXCEPTION(i));
	}

	if (drained) {
		dsb(sy);
		readl(dart->regs + DART_T8110_EXCEPTION(0));
	}

	if (dart->version >= 0x0203)
		readl(dart->regs + DART_T8110_BLOCK_ERROR);

	if (drained)
		dev_info(dart->dev,
			 "%u stale exception(s) drained; these predate Linux\n",
			 drained);
}

static void apple_dart_hw_enable_global_streams(struct apple_dart *dart)
{
	if (dart->owned_streams) {
		writel(dart->owned_streams | dart->static_dart_bypass,
		       dart->regs + dart->hw->enable_streams);
		return;
	}
	for (int word = 0; word < BITS_TO_U32(dart->num_streams); word++) {
		u32 mask = U32_MAX;

		if (dart->has_static_enabled) {
			mask = 0;
			for (int bit = 0; bit < 32; bit++) {
				int stream = word * 32 + bit;

				if (stream < dart->num_streams &&
				    test_bit(stream, dart->static_enabled))
					mask |= BIT(bit);
			}
		}
		writel(mask, dart->regs + dart->hw->enable_streams + 4 * word);
	}
}

static int apple_dart_t8110_v2_unlock(struct apple_dart *dart)
{
	struct apple_dart_stream_map stream_map = { .dart = dart };

	if (!(readl(dart->regs + DART_T8110_DIAG_LOCK) & DART_T8110_DIAG_LOCK_ON_ERR))
		return 0;

	/* gen3dart_clear_exception follows bitmap acknowledgement with the
	 * global FLUSH_UNLOCK command when lock-on-error is configured.
	 */
	__set_bit(0, stream_map.sidmap);
	return apple_dart_t8110_hw_tlb_command_range(&stream_map,
					    DART_T8110_TLB_CMD_OP_FLUSH_UNLOCK,
					    false, 0, 0);
}

/*
 * t8110 DARTs can fold a stream onto another one: the source stream is
 * translated, filtered and TLB-tracked exactly as the target (the ADT
 * "remap" list; on J700 the ISP firmware reaches its data segment through
 * SID 10, which iBoot remaps onto SID 0).  The source keeps no TTBR of its
 * own and stays out of the enable bitmap.
 */
static void apple_dart_hw_apply_remaps(struct apple_dart *dart)
{
	int sid;

	if (dart->hw->type != DART_T8110)
		return;

	for_each_set_bit(sid, dart->remapped, dart->num_streams) {
		writel(0, dart->regs + DART_TTBR(dart, sid, 0));
		writel(DART_T8110_TCR_REMAP_EN |
		       FIELD_PREP(DART_T8110_TCR_REMAP, dart->remap_target[sid]),
		       dart->regs + DART_TCR(dart, sid));
	}
}

static int apple_dart_hw_reset(struct apple_dart *dart)
{
	struct apple_dart_stream_map stream_map;
	int ret;

	/* The MTP firmware is still using this inherited identity mapping. */
	if (dart->retained_bypass)
		return 0;
	stream_map.dart = dart;
	bitmap_zero(stream_map.sidmap, DART_MAX_STREAMS);
	bitmap_set(stream_map.sidmap, 0, dart->num_streams);
	if (dart->owned_streams) {
		bitmap_zero(stream_map.sidmap, DART_MAX_STREAMS);
		stream_map.sidmap[0] = dart->owned_streams;
		apple_dart_hw_disable_dma(&stream_map);
		apple_dart_hw_clear_all_ttbrs(&stream_map);
		apple_dart_hw_enable_global_streams(dart);
		return dart->hw->invalidate_tlb(&stream_map);
	}
	apple_dart_hw_disable_dma(&stream_map);
	apple_dart_hw_clear_all_ttbrs(&stream_map);
	apple_dart_hw_apply_remaps(dart);

	apple_dart_hw_enable_global_streams(dart);

	/* Clear pending records using the v2 SID bitmap before unmasking. */
	if (dart->hw->type == DART_T8110 && dart->version >= 0x0202) {
		apple_dart_drain_stale_exceptions(dart);
		if (dart->version >= 0x0203)
			writel(readl(dart->regs + DART_T8110_BLOCK_ERROR) &
			       DART_T8110_BLOCK_ERROR_MASK,
			       dart->regs + DART_T8110_BLOCK_ERROR);
		ret = apple_dart_t8110_v2_unlock(dart);
		if (ret)
			return ret;
	} else
		writel(readl(dart->regs + dart->hw->error),
		       dart->regs + dart->hw->error);
	}

	if (dart->hw->type == DART_T8110)
		writel(0,  dart->regs + DART_T8110_ERROR_MASK);

	return dart->hw->invalidate_tlb(&stream_map);
}

/*
 * A full SID invalidate disturbs active T8140 display translations even when
 * only an unrelated buffer changes. Native SPTM uses DVA-range invalidation
 * with STT flushing for these streams. Keep the full command for lifecycle
 * synchronization; ordinary map/unmap only invalidate their changed range.
 */
static int apple_dart_domain_flush_tlb_range(struct apple_dart_domain *domain,
					    bool range, u64 first, u64 last)
{
	int i, j, ret = 0;
	struct apple_dart_atomic_stream_map *domain_stream_map;
	struct apple_dart_stream_map stream_map;
	struct io_pgtable_cfg *pgtbl_cfg =
		&io_pgtable_ops_to_pgtable(domain->pgtbl_ops)->cfg;

	for_each_stream_map(i, domain, domain_stream_map) {
		int err;

		stream_map.dart = domain_stream_map->dart;

		for (j = 0; j < BITS_TO_LONGS(stream_map.dart->num_streams); j++)
			stream_map.sidmap[j] = atomic_long_read(&domain_stream_map->sidmap[j]);

		WARN_ON(pm_runtime_get_sync(stream_map.dart->dev) < 0);

		if (stream_map.dart->locked)
			apple_dart_hw_sync_locked(pgtbl_cfg, &stream_map);

		if (range && stream_map.dart->locked &&
		    stream_map.dart->pgsize == SZ_16K &&
		    stream_map.dart->tlb_cmd_v2 && first <= last && last < BIT_ULL(42)) {
			err = apple_dart_t8110_hw_tlb_command_range(&stream_map,
				DART_T8110_TLB_CMD_OP_FLUSH_SID, true, first, last);
		} else {
			err = stream_map.dart->hw->invalidate_tlb(&stream_map);
		}
		if (err && !ret)
			ret = err;
		pm_runtime_put(stream_map.dart->dev);
	}

	return ret;
}

static int apple_dart_domain_flush_tlb(struct apple_dart_domain *domain)
{
	return apple_dart_domain_flush_tlb_range(domain, false, 0, 0);
}

static void apple_dart_domain_restore_locked(struct apple_dart_domain *domain)
{
	struct apple_dart_atomic_stream_map *domain_stream_map;
	struct apple_dart_stream_map stream_map;
	int i, j, idx;

	for_each_stream_map(i, domain, domain_stream_map) {
		stream_map.dart = domain_stream_map->dart;
		if (!stream_map.dart->locked)
			continue;

		for (j = 0; j < BITS_TO_LONGS(stream_map.dart->num_streams); j++)
			stream_map.sidmap[j] =
				atomic_long_read(&domain_stream_map->sidmap[j]);

		WARN_ON(pm_runtime_get_sync(stream_map.dart->dev) < 0);
		for (idx = 0; idx < stream_map.dart->hw->ttbr_count; idx++)
			apple_dart_hw_restore_locked_ttbr(&stream_map, idx);
		stream_map.dart->hw->invalidate_tlb(&stream_map);
		pm_runtime_put(stream_map.dart->dev);
	}
}

static void apple_dart_flush_iotlb_all(struct iommu_domain *domain)
{
	(void)apple_dart_domain_flush_tlb(to_dart_domain(domain));
}

static void apple_dart_iotlb_sync(struct iommu_domain *domain,
				  struct iommu_iotlb_gather *gather)
{
	(void)apple_dart_domain_flush_tlb_range(to_dart_domain(domain),
					       gather->start <= gather->end,
					       gather->start, gather->end);
}

static int apple_dart_iotlb_sync_map(struct iommu_domain *domain,
				     unsigned long iova, size_t size)
{
	return apple_dart_domain_flush_tlb_range(to_dart_domain(domain),
					       size && iova + size - 1 >= iova,
					       iova, iova + size - 1);
}

static phys_addr_t apple_dart_iova_to_phys(struct iommu_domain *domain,
					   dma_addr_t iova)
{
	struct apple_dart_domain *dart_domain = to_dart_domain(domain);
	struct io_pgtable_ops *ops = dart_domain->pgtbl_ops;

	if (!ops)
		return 0;

	return ops->iova_to_phys(ops, iova & dart_domain->mask);
}

static int apple_dart_map_pages(struct iommu_domain *domain, unsigned long iova,
				phys_addr_t paddr, size_t pgsize,
				size_t pgcount, int prot, gfp_t gfp,
				size_t *mapped)
{
	struct apple_dart_domain *dart_domain = to_dart_domain(domain);
	struct io_pgtable_ops *ops = dart_domain->pgtbl_ops;
	struct apple_dart *dart = dart_domain->stream_maps[0].dart;

	if (!ops)
		return -ENODEV;

	/*
	 * A locked DART's page tables are shared with hardware we do not fully
	 * drive.  The display pipe writes into buffers the DMA layer only ever
	 * declares DMA_TO_DEVICE, and a read-only PTE there does not produce a
	 * recoverable error: the DART raises a write-protect exception per
	 * access, tens of thousands a second, until the CPUs starve.  Nothing
	 * on these DARTs relies on write protection, so never install it.
	 */
	if (dart && dart->locked && !(prot & IOMMU_WRITE)) {
		dev_info_ratelimited(
			dart->dev,
			"forcing writable mapping at %#llx (%zu x %#zx, prot %#x)\n",
			(unsigned long long)(iova & dart_domain->mask),
			pgcount, pgsize, prot);
		prot |= IOMMU_WRITE;
	}

	return ops->map_pages(ops, iova & dart_domain->mask, paddr, pgsize,
			      pgcount, prot, gfp, mapped);
}

static size_t apple_dart_unmap_pages(struct iommu_domain *domain,
				     unsigned long iova, size_t pgsize,
				     size_t pgcount,
				     struct iommu_iotlb_gather *gather)
{
	struct apple_dart_domain *dart_domain = to_dart_domain(domain);
	struct io_pgtable_ops *ops = dart_domain->pgtbl_ops;
	size_t unmapped;

	unmapped = ops->unmap_pages(ops, iova & dart_domain->mask, pgsize,
				   pgcount, gather);
	if (unmapped)
		iommu_iotlb_gather_add_range(gather, iova, unmapped);
	return unmapped;
}

static void
apple_dart_setup_translation(struct apple_dart_domain *domain,
			     struct apple_dart_stream_map *stream_map)
{
	int i;
	struct apple_dart *dart = stream_map->dart;
	struct io_pgtable_cfg *pgtbl_cfg =
		&io_pgtable_ops_to_pgtable(domain->pgtbl_ops)->cfg;

	apple_dart_hw_enable_global_streams(dart);

	for (i = 0; i < pgtbl_cfg->apple_dart_cfg.n_ttbrs; ++i) {
		u64 ttbr = virt_to_phys(pgtbl_cfg->apple_dart_cfg.ttbr[i]);
		apple_dart_hw_set_ttbr(stream_map, i, ttbr);
	}
	for (; i < stream_map->dart->hw->ttbr_count; ++i)
		apple_dart_hw_clear_ttbr(stream_map, i);

	apple_dart_hw_enable_translation(stream_map,
					 pgtbl_cfg->apple_dart_cfg.n_levels);
	stream_map->dart->hw->invalidate_tlb(stream_map);
}

static void
apple_dart_setup_translation_locked(struct apple_dart_domain *domain,
				    struct apple_dart_stream_map *stream_map)
{
	int i;
	struct io_pgtable_cfg *pgtbl_cfg =
		&io_pgtable_ops_to_pgtable(domain->pgtbl_ops)->cfg;

	/* Locked DARTs are set up by the bootloader. */
	for (i = 0; i < pgtbl_cfg->apple_dart_cfg.n_ttbrs; ++i)
		apple_dart_hw_map_locked_ttbr(stream_map, i);
	for (; i < stream_map->dart->hw->ttbr_count; ++i)
		apple_dart_hw_unmap_locked_ttbr(stream_map, i);

	apple_dart_hw_sync_locked(pgtbl_cfg, stream_map);
	stream_map->dart->hw->invalidate_tlb(stream_map);
}

static int apple_dart_finalize_domain(struct apple_dart_domain *dart_domain,
				      struct apple_dart_master_cfg *cfg)
{
	struct apple_dart *dart = cfg->stream_maps[0].dart;
	struct io_pgtable_cfg pgtbl_cfg;
	dma_addr_t dma_min = dart_domain->dma_min ?: dart->dma_min;
	dma_addr_t dma_max = dart_domain->dma_max ?: dart->dma_max;
	u32 ias = min_t(u32, dart->ias, fls64(dma_max));
	int locked_sid = -1;
	int ret = 0;
	int i, j;

	if (dart->pgsize > PAGE_SIZE)
		return -EINVAL;

	mutex_lock(&dart_domain->init_lock);

	if (dart_domain->finalized)
		goto done;

	for (i = 0; i < MAX_DARTS_PER_DEVICE; ++i) {
		dart_domain->stream_maps[i].dart = cfg->stream_maps[i].dart;
		for (j = 0; j < BITS_TO_LONGS(dart->num_streams); j++)
			atomic_long_set(&dart_domain->stream_maps[i].sidmap[j],
					cfg->stream_maps[i].sidmap[j]);
	}

	pgtbl_cfg = (struct io_pgtable_cfg){
		.pgsize_bitmap = dart->pgsize,
		.ias = ias,
		.oas = dart->oas,
		.coherent_walk = 1,
		.iommu_dev = dart->dev,
	};

	if (dart->locked) {
		unsigned long *sidmap;
		u32 ttbr;

		/* Locked DARTs can only have a single stream bound */
		sidmap = cfg->stream_maps[0].sidmap;
		locked_sid = find_first_bit(sidmap, dart->num_streams);

		if (WARN_ON(locked_sid >= dart->num_streams ||
			    bitmap_weight(sidmap, dart->num_streams) != 1)) {
			ret = -EINVAL;
			goto done;
		}
		ttbr = readl(dart->regs + DART_TTBR(dart, locked_sid, 0));

		WARN_ON(!(ttbr & dart->hw->ttbr_valid));

		/* If the DART is locked, we need to keep the translation level count. */
		if (dart->hw->tcr_4level && dart->ias > 36) {
			if (readl(dart->regs + DART_TCR(dart, locked_sid)) &
			    dart->hw->tcr_4level) {
				if (ias < 37) {
					dev_info(dart->dev, "Expanded to ias=37 due to lock\n");
					pgtbl_cfg.ias = 37;
				}
			} else if (ias > 36) {
				dev_info(dart->dev, "Limited to ias=36 due to lock\n");
				pgtbl_cfg.ias = 36;
				if (dart->dma_min == 0 && dma_max == DMA_BIT_MASK(dart->ias)) {
					dma_max = DMA_BIT_MASK(pgtbl_cfg.ias);
				} else if ((dart->dma_min ^ dma_max) & ~DMA_BIT_MASK(36)) {
					dev_err(dart->dev,
						"Invalid DMA range for locked 3-level PT\n");
					ret = -ENOMEM;
					goto done;
				}
			}
		}
	}

	dart_domain->pgtbl_ops = alloc_io_pgtable_ops(dart->hw->fmt, &pgtbl_cfg,
						      &dart_domain->domain);
	if (!dart_domain->pgtbl_ops) {
		ret = -ENOMEM;
		goto done;
	}

	if (dart->locked) {
		for (i = 0; i < pgtbl_cfg.apple_dart_cfg.n_ttbrs; i++) {
			phys_addr_t phys;
			u32 ttbr;

			ttbr = readl(dart->regs +
				     DART_TTBR(dart, locked_sid, i));
			if (!(ttbr & dart->hw->ttbr_valid)) {
				ret = -EIO;
				goto err_free_pgtable;
			}

			ttbr &= ~dart->hw->ttbr_valid;
			if (dart->hw->ttbr_addr_field_shift)
				ttbr >>= dart->hw->ttbr_addr_field_shift;
			phys = (phys_addr_t)ttbr << dart->hw->ttbr_shift;

			ret = io_pgtable_apple_dart_import(dart_domain->pgtbl_ops, i, phys);
			if (ret) {
				dev_err(dart->dev,
					"Failed to import locked TTBR[%d] for SID %d: %d\n",
					i, locked_sid, ret);
				goto err_free_pgtable;
			}
		}
	}

	if (pgtbl_cfg.pgsize_bitmap == SZ_4K)
		dart_domain->mask = DMA_BIT_MASK(min_t(u32, dart->ias, 32));
	else if (pgtbl_cfg.apple_dart_cfg.n_levels == 3)
		dart_domain->mask = DMA_BIT_MASK(min_t(u32, dart->ias, 36));
	else if (pgtbl_cfg.apple_dart_cfg.n_levels == 4)
		dart_domain->mask = DMA_BIT_MASK(min_t(u32, dart->ias, 47));

	dart_domain->domain.pgsize_bitmap = pgtbl_cfg.pgsize_bitmap;
	dart_domain->domain.geometry.aperture_start = dma_min;
	dart_domain->domain.geometry.aperture_end = dma_max;
	dart_domain->domain.geometry.force_aperture = true;

	dart_domain->finalized = true;
	goto done;

err_free_pgtable:
	free_io_pgtable_ops(dart_domain->pgtbl_ops);
	dart_domain->pgtbl_ops = NULL;

done:
	mutex_unlock(&dart_domain->init_lock);
	return ret;
}

static int
apple_dart_reconcile_locked_translated(struct apple_dart_domain *dart_domain,
				       struct device *dev)
{
	struct apple_dart_master_cfg *cfg = dev_iommu_priv_get(dev);
	struct apple_dart *dart = cfg->stream_maps[0].dart;
	struct iommu_resv_region *entry;
	unsigned long pgsize = dart_domain->domain.pgsize_bitmap;
	LIST_HEAD(regions);
	int ret = 0;

	if (!cfg->locked)
		return 0;

	iommu_get_resv_regions(dev, &regions);
	list_for_each_entry(entry, &regions, list) {
		struct io_pgtable_apple_dart_reconcile result;
		phys_addr_t phys_start, phys_end;
		dma_addr_t dva;
		size_t size;

		if (entry->type != IOMMU_RESV_TRANSLATED)
			continue;
		if (check_add_overflow(entry->start, entry->length, &phys_end)) {
			ret = -EOVERFLOW;
			break;
		}

		phys_start = ALIGN(entry->start, pgsize);
		phys_end = ALIGN(phys_end, pgsize);
		dva = ALIGN(entry->dva, pgsize);
		if (phys_end <= phys_start)
			continue;
		size = phys_end - phys_start;

		ret = io_pgtable_apple_dart_reconcile(
			dart_domain->pgtbl_ops, dva & dart_domain->mask,
			phys_start, size, entry->prot, &result);
		if (ret) {
			dev_err(dart->dev,
				"locked translated mapping DVA %pad PA %pa size %#zx failed permission reconciliation: %d\n",
				&dva, &phys_start, size, ret);
			break;
		}

		dev_info(dart->dev,
			 "locked translated mapping DVA %pad PA %pa size %#zx inherited first PTE %#018llx (no-read=%u no-write=%u), reconciled %#018llx, changed %u pages\n",
			 &dva, &phys_start, size, result.first_pte_before,
			 !!(result.first_pte_before & BIT_ULL(3)),
			 !!(result.first_pte_before & BIT_ULL(2)),
			 result.first_pte_after, result.changed);
	}
	iommu_put_resv_regions(dev, &regions);

	return ret;
}

static int
apple_dart_mod_streams(struct apple_dart_atomic_stream_map *domain_maps,
		       struct apple_dart_stream_map *master_maps,
		       bool add_streams)
{
	int i, j;

	for (i = 0; i < MAX_DARTS_PER_DEVICE; ++i) {
		if (domain_maps[i].dart != master_maps[i].dart)
			return -EINVAL;
	}

	for (i = 0; i < MAX_DARTS_PER_DEVICE; ++i) {
		if (!domain_maps[i].dart)
			break;
		for (j = 0; j < BITS_TO_LONGS(domain_maps[i].dart->num_streams); j++) {
			if (add_streams)
				atomic_long_or(master_maps[i].sidmap[j],
					       &domain_maps[i].sidmap[j]);
			else
				atomic_long_and(~master_maps[i].sidmap[j],
						&domain_maps[i].sidmap[j]);
		}
	}

	return 0;
}

static int apple_dart_domain_add_streams(struct apple_dart_domain *domain,
					 struct apple_dart_master_cfg *cfg)
{
	return apple_dart_mod_streams(domain->stream_maps, cfg->stream_maps,
				      true);
}

static int apple_dart_attach_dev_paging(struct iommu_domain *domain,
					struct device *dev,
					struct iommu_domain *old)
{
	int ret, i;
	struct apple_dart_stream_map *stream_map;
	struct apple_dart_master_cfg *cfg = dev_iommu_priv_get(dev);
	struct apple_dart_domain *dart_domain = to_dart_domain(domain);

	for_each_stream_map(i, cfg, stream_map)
		if (stream_map->dart->retained_bypass)
			return -EBUSY;

	for_each_stream_map(i, cfg, stream_map)
		WARN_ON(pm_runtime_get_sync(stream_map->dart->dev) < 0);

	ret = apple_dart_finalize_domain(dart_domain, cfg);
	if (ret)
		goto err;

	ret = apple_dart_reconcile_locked_translated(dart_domain, dev);
	if (ret)
		goto err;

	ret = apple_dart_domain_add_streams(dart_domain, cfg);
	if (ret)
		goto err;

	for_each_stream_map(i, cfg, stream_map) {
		if (!stream_map->dart->locked)
			apple_dart_setup_translation(dart_domain, stream_map);
		else
			apple_dart_setup_translation_locked(dart_domain,
							    stream_map);
	}

err:
	for_each_stream_map(i, cfg, stream_map)
		pm_runtime_put(stream_map->dart->dev);
	return ret;
}

static int apple_dart_attach_dev_identity(struct iommu_domain *domain,
					  struct device *dev,
					  struct iommu_domain *old)
{
	struct apple_dart_master_cfg *cfg = dev_iommu_priv_get(dev);
	struct apple_dart_stream_map *stream_map;
	int i;

	for_each_stream_map(i, cfg, stream_map)
		if (stream_map->dart->owned_streams)
			return -EINVAL;

	if (!cfg->supports_bypass)
		return -EINVAL;

	if (cfg->locked)
		return -EINVAL;

	for_each_stream_map(i, cfg, stream_map)
		WARN_ON(pm_runtime_get_sync(stream_map->dart->dev) < 0);

	for_each_stream_map(i, cfg, stream_map)
		if (!stream_map->dart->retained_bypass)
			apple_dart_hw_enable_bypass(stream_map);

	for_each_stream_map(i, cfg, stream_map)
		pm_runtime_put(stream_map->dart->dev);
	return 0;
}

static const struct iommu_domain_ops apple_dart_identity_ops = {
	.attach_dev = apple_dart_attach_dev_identity,
};

static struct iommu_domain apple_dart_identity_domain = {
	.type = IOMMU_DOMAIN_IDENTITY,
	.ops = &apple_dart_identity_ops,
};

static int apple_dart_attach_dev_blocked(struct iommu_domain *domain,
					 struct device *dev,
					 struct iommu_domain *old)
{
	struct apple_dart_master_cfg *cfg = dev_iommu_priv_get(dev);
	struct apple_dart_stream_map *stream_map;
	int i;

	if (cfg->locked)
		return -EINVAL;

	for_each_stream_map(i, cfg, stream_map)
		if (stream_map->dart->retained_bypass)
			return -EBUSY;

	for_each_stream_map(i, cfg, stream_map)
		WARN_ON(pm_runtime_get_sync(stream_map->dart->dev) < 0);

	for_each_stream_map(i, cfg, stream_map)
		apple_dart_hw_disable_dma(stream_map);

	for_each_stream_map(i, cfg, stream_map)
		pm_runtime_put(stream_map->dart->dev);
	return 0;
}

static const struct iommu_domain_ops apple_dart_blocked_ops = {
	.attach_dev = apple_dart_attach_dev_blocked,
};

static struct iommu_domain apple_dart_blocked_domain = {
	.type = IOMMU_DOMAIN_BLOCKED,
	.ops = &apple_dart_blocked_ops,
};

static struct iommu_device *apple_dart_probe_device(struct device *dev)
{
	struct apple_dart_master_cfg *cfg = dev_iommu_priv_get(dev);
	struct apple_dart_stream_map *stream_map;
	int i;

	if (!dev_iommu_fwspec_get(dev) || !cfg)
		return ERR_PTR(-ENODEV);

	for_each_stream_map(i, cfg, stream_map)
		device_link_add(dev, stream_map->dart->dev,
			DL_FLAG_PM_RUNTIME | DL_FLAG_AUTOREMOVE_SUPPLIER |
			DL_FLAG_RPM_ACTIVE);

	return &cfg->stream_maps[0].dart->iommu;
}

static void apple_dart_release_device(struct device *dev)
{
	int i, j;
	struct apple_dart_stream_map *stream_map;
	struct apple_dart_master_cfg *cfg = dev_iommu_priv_get(dev);

	for_each_stream_map(j, cfg, stream_map) {
		if (stream_map->dart->locked) {
			WARN_ON(pm_runtime_get_sync(stream_map->dart->dev) < 0);
			for (i = 0; i < stream_map->dart->hw->ttbr_count; ++i)
				apple_dart_hw_unmap_locked_ttbr(stream_map, i);
			stream_map->dart->hw->invalidate_tlb(stream_map);
			pm_runtime_put(stream_map->dart->dev);
		}
	}

	kfree(cfg);
}

static struct iommu_domain *apple_dart_domain_alloc_paging(struct device *dev)
{
	struct apple_dart_domain *dart_domain;
	u32 iova_bits;

	dart_domain = kzalloc_obj(*dart_domain);
	if (!dart_domain)
		return NULL;

	mutex_init(&dart_domain->init_lock);

	if (dev) {
		struct apple_dart_master_cfg *cfg = dev_iommu_priv_get(dev);
		struct device_node *iova_node = dev->of_node;
		int ret;

		if (dev_is_pci(dev)) {
			struct pci_host_bridge *bridge;
			struct device_node *bridge_node;

			bridge = pci_find_host_bridge(to_pci_dev(dev)->bus);
			bridge_node = bridge && bridge->dev.parent ?
				bridge->dev.parent->of_node : NULL;
			if (bridge_node &&
			    of_property_read_bool(bridge_node,
						  "apple,endpoint-iova-address-bits"))
				iova_node = bridge_node;
		}
		if (!of_property_read_u32(iova_node,
					  dev_is_pci(dev) ?
					  "apple,endpoint-iova-address-bits" :
					  "apple,iova-address-bits",
					  &iova_bits)) {
			if (!iova_bits || iova_bits >= 64) {
				kfree(dart_domain);
				return ERR_PTR(-EINVAL);
			}
			dart_domain->dma_max = DMA_BIT_MASK(iova_bits);
		}

		/*
		 * A consumer may carry its own window (base, size): on a locked
		 * DART every stream root is the firmware's and each stream has
		 * its own free 64 GiB slots and per-stream VM base, e.g. the J700
		 * AOP's primary/setup/source streams.
		 */
		if (of_property_present(iova_node, "apple,dma-range")) {
			u64 dma_range[2];

			ret = of_property_read_u64_array(iova_node, "apple,dma-range",
							 dma_range, 2);
			if (ret || !dma_range[1] ||
			    dma_range[0] + dma_range[1] - 1 < dma_range[0]) {
				kfree(dart_domain);
				return ERR_PTR(ret ?: -EINVAL);
			}
			dart_domain->dma_min = dma_range[0];
			dart_domain->dma_max = dma_range[0] + dma_range[1] - 1;
		}

		ret = apple_dart_finalize_domain(dart_domain, cfg);
		if (ret) {
			kfree(dart_domain);
			return ERR_PTR(ret);
		}
	}
	return &dart_domain->domain;
}

static void apple_dart_domain_free(struct iommu_domain *domain)
{
	struct apple_dart_domain *dart_domain = to_dart_domain(domain);

	apple_dart_domain_restore_locked(dart_domain);
	free_io_pgtable_ops(dart_domain->pgtbl_ops);

	kfree(dart_domain);
}

static int apple_dart_of_xlate(struct device *dev,
			       const struct of_phandle_args *args)
{
	struct apple_dart_master_cfg *cfg = dev_iommu_priv_get(dev);
	struct platform_device *iommu_pdev = of_find_device_by_node(args->np);
	struct apple_dart *dart = platform_get_drvdata(iommu_pdev);
	struct apple_dart *cfg_dart;
	int i, sid;

	put_device(&iommu_pdev->dev);

	if (args->args_count != 1)
		return -EINVAL;
	sid = args->args[0];
	if (sid < 0 || sid >= dart->num_streams)
		return -EINVAL;
	if (dart->retained_bypass && sid != 0)
		return -EINVAL;
	if (dart->owned_streams && (sid >= 32 || !(dart->owned_streams & BIT(sid))))
		return -EINVAL;

	if (!cfg) {
		cfg = kzalloc_obj(*cfg);
		if (!cfg)
			return -ENOMEM;
		/* Will be ANDed with DART capabilities */
		cfg->supports_bypass = true;
		/* Will be ORed with DART capabilities*/
		cfg->locked = false;
	}
	dev_iommu_priv_set(dev, cfg);

	cfg_dart = cfg->stream_maps[0].dart;
	if (cfg_dart) {
		if (cfg_dart->pgsize != dart->pgsize)
			return -EINVAL;
		if (cfg_dart->ias != dart->ias)
			return -EINVAL;
	}

	cfg->supports_bypass &= dart->supports_bypass;
	cfg->locked |= dart->locked;

	for (i = 0; i < MAX_DARTS_PER_DEVICE; ++i) {
		if (cfg->stream_maps[i].dart == dart) {
			set_bit(sid, cfg->stream_maps[i].sidmap);
			return 0;
		}
	}
	for (i = 0; i < MAX_DARTS_PER_DEVICE; ++i) {
		if (!cfg->stream_maps[i].dart) {
			cfg->stream_maps[i].dart = dart;
			set_bit(sid, cfg->stream_maps[i].sidmap);
			return 0;
		}
	}

	return -EINVAL;
}

static DEFINE_MUTEX(apple_dart_groups_lock);

static void apple_dart_release_group(void *iommu_data)
{
	int i, sid;
	struct apple_dart_stream_map *stream_map;
	struct apple_dart_master_cfg *group_master_cfg = iommu_data;

	mutex_lock(&apple_dart_groups_lock);

	for_each_stream_map(i, group_master_cfg, stream_map)
		for_each_set_bit(sid, stream_map->sidmap, stream_map->dart->num_streams)
			stream_map->dart->sid2group[sid] = NULL;

	kfree(iommu_data);
	mutex_unlock(&apple_dart_groups_lock);
}

static int apple_dart_merge_master_cfg(struct apple_dart_master_cfg *dst,
				       struct apple_dart_master_cfg *src)
{
	/*
	 * We know that this function is only called for groups returned from
	 * pci_device_group and that all Apple Silicon platforms never spread
	 * PCIe devices from the same bus across multiple DARTs such that we can
	 * just assume that both src and dst only have the same single DART.
	 */
	if (src->stream_maps[1].dart)
		return -EINVAL;
	if (dst->stream_maps[1].dart)
		return -EINVAL;
	if (src->stream_maps[0].dart != dst->stream_maps[0].dart)
		return -EINVAL;

	bitmap_or(dst->stream_maps[0].sidmap,
		  dst->stream_maps[0].sidmap,
		  src->stream_maps[0].sidmap,
		  dst->stream_maps[0].dart->num_streams);
	return 0;
}

static struct iommu_group *apple_dart_device_group(struct device *dev)
{
	int i, sid;
	struct apple_dart_master_cfg *cfg = dev_iommu_priv_get(dev);
	struct apple_dart_stream_map *stream_map;
	struct apple_dart_master_cfg *group_master_cfg;
	struct iommu_group *group = NULL;
	struct iommu_group *res = ERR_PTR(-EINVAL);

	mutex_lock(&apple_dart_groups_lock);

	for_each_stream_map(i, cfg, stream_map) {
		for_each_set_bit(sid, stream_map->sidmap, stream_map->dart->num_streams) {
			struct iommu_group *stream_group =
				stream_map->dart->sid2group[sid];

			if (group && group != stream_group) {
				res = ERR_PTR(-EINVAL);
				goto out;
			}

			group = stream_group;
		}
	}

	if (group) {
		res = iommu_group_ref_get(group);
		goto out;
	}

#ifdef CONFIG_PCI
	if (dev_is_pci(dev))
		group = pci_device_group(dev);
	else
#endif
		group = generic_device_group(dev);

	res = ERR_PTR(-ENOMEM);
	if (!group)
		goto out;

	group_master_cfg = iommu_group_get_iommudata(group);
	if (group_master_cfg) {
		int ret;

		ret = apple_dart_merge_master_cfg(group_master_cfg, cfg);
		if (ret) {
			dev_err(dev, "Failed to merge DART IOMMU groups.\n");
			iommu_group_put(group);
			res = ERR_PTR(ret);
			goto out;
		}
	} else {
		group_master_cfg = kmemdup(cfg, sizeof(*group_master_cfg),
					   GFP_KERNEL);
		if (!group_master_cfg) {
			iommu_group_put(group);
			goto out;
		}

		iommu_group_set_iommudata(group, group_master_cfg,
			apple_dart_release_group);
	}

	for_each_stream_map(i, cfg, stream_map)
		for_each_set_bit(sid, stream_map->sidmap, stream_map->dart->num_streams)
			stream_map->dart->sid2group[sid] = group;

	res = group;

out:
	mutex_unlock(&apple_dart_groups_lock);
	return res;
}

static int apple_dart_def_domain_type(struct device *dev)
{
	struct apple_dart_master_cfg *cfg = dev_iommu_priv_get(dev);

	if (cfg->stream_maps[0].dart->retained_bypass)
		return IOMMU_DOMAIN_IDENTITY;
	if (cfg->stream_maps[0].dart->owned_streams)
		return IOMMU_DOMAIN_DMA;
	if (cfg->stream_maps[0].dart->pgsize > PAGE_SIZE)
		return IOMMU_DOMAIN_IDENTITY;
	if (!cfg->supports_bypass)
		return IOMMU_DOMAIN_DMA;
	if (cfg->locked)
		return IOMMU_DOMAIN_DMA;

	return 0;
}

#ifndef CONFIG_PCIE_APPLE_MSI_DOORBELL_ADDR
/* Keep things compiling when CONFIG_PCIE_APPLE isn't selected */
#define CONFIG_PCIE_APPLE_MSI_DOORBELL_ADDR	0
#endif
#define DOORBELL_ADDR	(CONFIG_PCIE_APPLE_MSI_DOORBELL_ADDR & PAGE_MASK)

/*
 * No scanout-window reservation here.
 *
 * A 256 MB reservation at the top of this stream's window was added so that
 * apple-dcp could mirror DCP-allocated buffers into the scanout domain without
 * colliding with the DMA API.  The mirror itself turned out to be destructive
 * -- every iommu_map() on this domain flushes SID 0 while the display pipe is
 * mid-frame, which kills the firmware -- so the mirror was reverted and the
 * reservation has no user.  Do not re-add one without the mirror.
 */
static void apple_dart_get_resv_regions(struct device *dev,
					struct list_head *head)
{
	if (IS_ENABLED(CONFIG_PCIE_APPLE) && dev_is_pci(dev)) {
		struct iommu_resv_region *region;
		int prot = IOMMU_WRITE | IOMMU_NOEXEC | IOMMU_MMIO;

		region = iommu_alloc_resv_region(DOORBELL_ADDR,
						 PAGE_SIZE, prot,
						 IOMMU_RESV_MSI, GFP_KERNEL);
		if (!region)
			return;

		list_add_tail(&region->list, head);
	}

	iommu_dma_get_resv_regions(dev, head);
}

static const struct iommu_ops apple_dart_iommu_ops = {
	.identity_domain = &apple_dart_identity_domain,
	.blocked_domain = &apple_dart_blocked_domain,
	.def_domain_type = apple_dart_def_domain_type,
	.domain_alloc_paging = apple_dart_domain_alloc_paging,
	.probe_device = apple_dart_probe_device,
	.release_device = apple_dart_release_device,
	.device_group = apple_dart_device_group,
	.of_xlate = apple_dart_of_xlate,
	.get_resv_regions = apple_dart_get_resv_regions,
	.owner = THIS_MODULE,
	.default_domain_ops = &(const struct iommu_domain_ops) {
		.attach_dev	= apple_dart_attach_dev_paging,
		.map_pages	= apple_dart_map_pages,
		.unmap_pages	= apple_dart_unmap_pages,
		.flush_iotlb_all = apple_dart_flush_iotlb_all,
		.iotlb_sync	= apple_dart_iotlb_sync,
		.iotlb_sync_map	= apple_dart_iotlb_sync_map,
		.iova_to_phys	= apple_dart_iova_to_phys,
		.free		= apple_dart_domain_free,
	}
};

static irqreturn_t apple_dart_t8020_irq(int irq, void *dev)
{
	struct apple_dart *dart = dev;
	const char *fault_name = NULL;
	u32 error = readl(dart->regs + DART_T8020_ERROR);
	u32 error_code = FIELD_GET(DART_T8020_ERROR_CODE, error);
	u32 addr_lo = readl(dart->regs + DART_T8020_ERROR_ADDR_LO);
	u32 addr_hi = readl(dart->regs + DART_T8020_ERROR_ADDR_HI);
	u64 addr = addr_lo | (((u64)addr_hi) << 32);
	u8 stream_idx = FIELD_GET(DART_T8020_ERROR_STREAM, error);

	if (!(error & DART_T8020_ERROR_FLAG))
		return IRQ_NONE;

	/* there should only be a single bit set but let's use == to be sure */
	if (error_code == DART_T8020_ERROR_READ_FAULT)
		fault_name = "READ FAULT";
	else if (error_code == DART_T8020_ERROR_WRITE_FAULT)
		fault_name = "WRITE FAULT";
	else if (error_code == DART_T8020_ERROR_NO_PTE)
		fault_name = "NO PTE FOR IOVA";
	else if (error_code == DART_T8020_ERROR_NO_PMD)
		fault_name = "NO PMD FOR IOVA";
	else if (error_code == DART_T8020_ERROR_NO_TTBR)
		fault_name = "NO TTBR FOR IOVA";
	else
		fault_name = "unknown";

	dev_err_ratelimited(
		dart->dev,
		"translation fault: status:0x%x stream:%d code:0x%x (%s) at 0x%llx",
		error, stream_idx, error_code, fault_name, addr);

	writel(error, dart->regs + DART_T8020_ERROR);
	return IRQ_HANDLED;
}

static irqreturn_t apple_dart_t8110_legacy_irq(int irq, void *dev)
{
	struct apple_dart *dart = dev;
	const char *fault_name = NULL;
	u32 error = readl(dart->regs + DART_T8110_ERROR);
	u32 error_code = FIELD_GET(DART_T8110_ERROR_CODE, error);
	u32 addr_lo = readl(dart->regs + DART_T8110_ERROR_ADDR_LO);
	u32 addr_hi = readl(dart->regs + DART_T8110_ERROR_ADDR_HI);
	u64 addr = addr_lo | (((u64)addr_hi) << 32);
	u8 stream_idx = FIELD_GET(DART_T8110_ERROR_STREAM, error);

	if (!(error & DART_T8110_ERROR_FLAG))
		return IRQ_NONE;

	/* there should only be a single bit set but let's use == to be sure */
	if (error_code == DART_T8110_ERROR_READ_FAULT)
		fault_name = "READ FAULT";
	else if (error_code == DART_T8110_ERROR_WRITE_FAULT)
		fault_name = "WRITE FAULT";
	else if (error_code == DART_T8110_ERROR_NO_PTE)
		fault_name = "NO PTE FOR IOVA";
	else if (error_code == DART_T8110_ERROR_NO_PMD)
		fault_name = "NO PMD FOR IOVA";
	else if (error_code == DART_T8110_ERROR_NO_PGD)
		fault_name = "NO PGD FOR IOVA";
	else if (error_code == DART_T8110_ERROR_NO_TTBR)
		fault_name = "NO TTBR FOR IOVA";
	else
		fault_name = "unknown";

	dev_err_ratelimited(
		dart->dev,
		"translation fault: status:0x%x stream:%d code:0x%x (%s) at 0x%llx",
		error, stream_idx, error_code, fault_name, addr);

	writel(error, dart->regs + DART_T8110_ERROR);
	for (int i = 0; i < BITS_TO_U32(dart->num_streams); i++)
		writel(U32_MAX, dart->regs + DART_T8110_ERROR_STREAMS + 4 * i);

	return IRQ_HANDLED;
}

static const char *apple_dart_t8110_v2_fault_name(u32 exception)
{
	switch (FIELD_GET(DART_T8110_EXCEPTION_CODE, exception)) {
	case 0:
		switch (FIELD_GET(DART_T8110_EXCEPTION_LEVEL, exception)) {
		case 0:
			return "TTBR invalid";
		case 1:
			return "CTE invalid";
		case 2:
			return "STE invalid";
		case 3:
			return "PTE invalid";
		}
		break;
	case 1:
		return "write protect";
	case 2:
		return "read protect";
	case 3:
		return "drop protect";
	case 4:
		return "AXI decode";
	case 5:
		return "AXI error";
	case 6:
		return "fill region";
	case 7:
		return "region protect";
	case 8:
		return "CTRR write protect";
	case 9:
		return "APF reject";
	case 10:
		return "BPF reject";
	case 11:
		return "STT mismatch";
	case 12:
		return "STT flush";
	case 13:
		return "external";
	case 14:
		return "illegal SID";
	}

	return "unknown";
}

static bool apple_dart_t8110_v2_block_pending(u16 version, u32 block_error)
{
	return version >= 0x0203 &&
	       (block_error & DART_T8110_BLOCK_ERROR_MASK);
}

static irqreturn_t apple_dart_t8110_v2_irq(int irq, void *dev)
{
	struct apple_dart *dart = dev;
	irqreturn_t ret = IRQ_NONE;
	unsigned int words = DIV_ROUND_UP(dart->num_streams, 32);
	unsigned int i;
	u32 block_error;

	/*
	 * 0x4000 + 4*i is a bitmap of SIDs with a pending exception, one word
	 * per 32 SIDs -- not a per-SID code.  Iterating it per SID (as this
	 * used to) runs off the end of the array into the register arrays at
	 * 0x4020/0x4040 for every sid >= ceil(nsid/32).
	 */
	for (i = 0; i < words && i < 8; i++) {
		u32 pending = readl(dart->regs + DART_T8110_EXCEPTION(i));
		unsigned long bits = pending;
		unsigned int bit;

		if (!pending)
			continue;

		for_each_set_bit(bit, &bits, 32) {
			unsigned int sid = i * 32 + bit;
			u32 status;
			u64 addr;

			if (sid >= dart->num_streams)
				break;

			status = readl(dart->regs +
				       DART_T8110_ERR_SID_STATUS(sid));
			addr = readl(dart->regs +
				     DART_T8110_EXCEPTION_ADDR_LO(sid)) |
			       (u64)readl(dart->regs +
					  DART_T8110_EXCEPTION_ADDR_HI(sid)) << 32;

			dev_err_ratelimited(dart->dev,
					    "translation fault: SID:%u %s %s at %#llx (status %#010x)\n",
					    sid,
					    status & DART_T8110_ERR_SID_STATUS_WRITE ?
						    "write" : "read",
						    apple_dart_t8110_v2_fault_name(status),
						    addr, status);
		}

		/*
		 * Acknowledge: the bitmap is write-1-to-clear.  Without this the
		 * same latched bit re-fires immediately and one historical
		 * exception looks like a fault storm.
		 */
		writel(pending, dart->regs + DART_T8110_EXCEPTION(i));
		ret = IRQ_HANDLED;
	}

	if (ret == IRQ_HANDLED) {
		dsb(sy);
		readl(dart->regs + DART_T8110_EXCEPTION(0));
	}

	if (dart->version >= 0x0203) {
		block_error = readl(dart->regs + DART_T8110_BLOCK_ERROR);
		if (apple_dart_t8110_v2_block_pending(dart->version, block_error)) {
			dev_err_ratelimited(dart->dev,
					    "block exception: status:0x%08x\n",
					    block_error);
			writel(block_error & DART_T8110_BLOCK_ERROR_MASK,
			       dart->regs + DART_T8110_BLOCK_ERROR);
			ret = IRQ_HANDLED;
		}
	}
	if (ret == IRQ_HANDLED)
		apple_dart_t8110_v2_unlock(dart);

	return ret;
}

static irqreturn_t apple_dart_t8110_irq(int irq, void *dev)
{
	struct apple_dart *dart = dev;

	if (dart->version >= 0x0202)
		return apple_dart_t8110_v2_irq(irq, dev);

	return apple_dart_t8110_legacy_irq(irq, dev);
}

static irqreturn_t apple_dart_irq(int irq, void *dev)
{
	irqreturn_t ret;
	struct apple_dart *dart = dev;

	WARN_ON(pm_runtime_get_sync(dart->dev) < 0);
	ret = dart->hw->irq_handler(irq, dev);
	pm_runtime_put(dart->dev);
	return ret;
}

static bool apple_dart_is_locked(struct apple_dart *dart)
{
	return !!(readl(dart->regs + dart->hw->lock) & dart->hw->lock_bit);
}

/* Legacy J700 handoff: preserve APF while enabling the declared radio streams. */
static int apple_dart_static_bypass_test(struct apple_dart *dart)
{
	struct apple_dart_stream_map map = { .dart = dart };
	unsigned int sid;
	int ret;

	if (!dart->static_dart_bypass)
		return 0;
	if (dart->owned_streams != BIT(1) || dart->num_streams != 19 ||
	    !dart->supports_bypass || dart->locked ||
	    dart->static_dart_bypass != (BIT(16) | BIT(18)))
		return -EINVAL;
	map.sidmap[0] = dart->static_dart_bypass;
	/* Validate every target before changing any; preserve TTBR contents. */
	for_each_set_bit(sid, map.sidmap, dart->num_streams) {
		u32 tcr = readl(dart->regs + DART_TCR(dart, sid));
		u32 ttbr = readl(dart->regs + DART_TTBR(dart, sid, 0));

		if ((ttbr & dart->hw->ttbr_valid) || (tcr != 0 && tcr != 1 && tcr != 2))
			return -EBUSY;
	}
	for_each_set_bit(sid, map.sidmap, dart->num_streams)
		writel(DART_T8110_TCR_BYPASS_DART, dart->regs + DART_TCR(dart, sid));
	writel(dart->static_dart_bypass, dart->regs + dart->hw->enable_streams);
	ret = dart->hw->invalidate_tlb(&map);
	if (ret)
		return ret;
	for_each_set_bit(sid, map.sidmap, dart->num_streams)
		if (readl(dart->regs + DART_TCR(dart, sid)) != DART_T8110_TCR_BYPASS_DART)
			return -EIO;
	dev_info(dart->dev, "SID16/18 ADT DART-only bypass test active; APF retained, SID17 untouched\n");
	return 0;
}

static int apple_dart_probe(struct platform_device *pdev)
{
	int bypass_count, enabled_count, remap_count, ret;
	u32 dart_params[4], sid;
	struct resource *res;
	struct apple_dart *dart;
	struct device *dev = &pdev->dev;
	u64 dma_range[2];

	if (of_property_read_bool(dev->of_node,
				  "apple,j700-defer-probe") &&
	    !j700_deferred_probe_ready) {
		dev_info(dev,
			 "J700_APCIE_DART_DEFERRED: waiting for post-NVMe-root release\n");
		return -EPROBE_DEFER;
	}

	dart = devm_kzalloc(dev, sizeof(*dart), GFP_KERNEL);
	if (!dart)
		return -ENOMEM;

	dart->dev = dev;
	dart->hw = of_device_get_match_data(dev);
	dart->pmp_sid6_only = of_device_is_compatible(dev->of_node,
						     "apple,t8152-pmp-dart");
	dart->force_available =
		dart->pmp_sid6_only ||
		of_property_read_bool(dev->of_node, "apple,force-available");
	spin_lock_init(&dart->lock);

	dart->regs = devm_platform_get_and_ioremap_resource(pdev, 0, &res);
	if (IS_ERR(dart->regs))
		return PTR_ERR(dart->regs);

	if (resource_size(res) < 0x4000) {
		dev_err(dev, "MMIO region too small (%pr)\n", res);
		return -EINVAL;
	}

	if (dart->pmp_sid6_only) {
		if (res->start != 0x318100000ULL || resource_size(res) != 0xc000)
			return -EINVAL;
		/* Do not claim or acknowledge the shared controller's fault IRQ. */
		dart->irq = -1;
	} else {
		dart->irq = platform_get_irq(pdev, 0);
		if (dart->irq < 0)
			return -ENODEV;
	}

	ret = devm_clk_bulk_get_all(dev, &dart->clks);
	if (ret < 0)
		return ret;
	dart->num_clks = ret;

	ret = clk_bulk_prepare_enable(dart->num_clks, dart->clks);
	if (ret)
		return ret;

	pm_runtime_get_noresume(dev);
	pm_runtime_set_active(dev);
	pm_runtime_irq_safe(dev);

	ret = devm_pm_runtime_enable(dev);
	if (ret)
		goto err_clk_disable;

	dart_params[0] = readl(dart->regs + DART_PARAMS1);
	dart_params[1] = readl(dart->regs + DART_PARAMS2);
	dart->pgsize = 1 << FIELD_GET(DART_PARAMS1_PAGE_SHIFT, dart_params[0]);
	dart->supports_bypass = dart_params[1] & DART_PARAMS2_BYPASS_SUPPORT;

	switch (dart->hw->type) {
	case DART_T8020:
	case DART_T6000:
		dart->ias = 32;
		dart->oas = dart->hw->oas;
		dart->num_streams = dart->hw->max_sid_count;
		break;

	case DART_T8110:
		dart_params[2] = readl(dart->regs + DART_T8110_PARAMS3);
		dart_params[3] = readl(dart->regs + DART_T8110_PARAMS4);
		dart->ias = FIELD_GET(DART_T8110_PARAMS3_VA_WIDTH, dart_params[2]);
		dart->oas = FIELD_GET(DART_T8110_PARAMS3_PA_WIDTH, dart_params[2]);
		dart->num_streams = FIELD_GET(DART_T8110_PARAMS4_NUM_SIDS, dart_params[3]);
		dart->four_level = dart->ias > 36;
		dart->version = dart_params[2] & GENMASK(15, 0);
		dart->tlb_cmd_v2 = dart->version >= 0x0202;
		if (dart->tlb_cmd_v2)
			dev_info(dev, "DART v%lu.%lu uses v2 TLB commands\n",
				 FIELD_GET(DART_T8110_PARAMS3_VER_MAJ, dart_params[2]),
				 FIELD_GET(DART_T8110_PARAMS3_VER_MIN, dart_params[2]));
		break;
	}

	dart->dma_min = 0;
	dart->dma_max = DMA_BIT_MASK(dart->ias);

	ret = of_property_read_u64_array(dev->of_node, "apple,dma-range", dma_range, 2);
	if (ret == -EINVAL) {
		ret = 0;
	} else if (ret) {
		goto err_clk_disable;
	} else {
		dart->dma_min = dma_range[0];
		dart->dma_max = dma_range[0] + dma_range[1] - 1;
		if ((dart->dma_min ^ dart->dma_max) & ~DMA_BIT_MASK(dart->ias)) {
			dev_err(&pdev->dev, "Invalid DMA range for ias=%d\n",
				dart->ias);
			ret = -EINVAL;
			goto err_clk_disable;
		}
		dev_info(&pdev->dev, "Limiting DMA range to %pad..%pad\n",
			 &dart->dma_min, &dart->dma_max);
	}

	if (dart->num_streams > DART_MAX_STREAMS) {
		dev_err(&pdev->dev, "Too many streams (%d > %d)\n",
			dart->num_streams, DART_MAX_STREAMS);
		ret = -EINVAL;
		goto err_clk_disable;
	}
	bypass_count = of_property_count_u32_elems(dev->of_node,
						    "apple,bypass-streams");
	if (bypass_count > 0) {
		if (!dart->supports_bypass) {
			ret = -EOPNOTSUPP;
			goto err_clk_disable;
		}
		for (int i = 0; i < bypass_count; i++) {
			ret = of_property_read_u32_index(dev->of_node,
							 "apple,bypass-streams",
							 i, &sid);
			if (ret || sid >= dart->num_streams) {
				ret = ret ?: -ERANGE;
				goto err_clk_disable;
			}
			set_bit(sid, dart->static_bypass);
		}
	} else if (bypass_count != -EINVAL) {
		ret = bypass_count;
		goto err_clk_disable;
	}
	enabled_count = of_property_count_u32_elems(dev->of_node,
						     "apple,enabled-streams");
	if (enabled_count > 0) {
		dart->has_static_enabled = true;
		for (int i = 0; i < enabled_count; i++) {
			ret = of_property_read_u32_index(dev->of_node,
							 "apple,enabled-streams",
							 i, &sid);
			if (ret || sid >= dart->num_streams) {
				ret = ret ?: -ERANGE;
				goto err_clk_disable;
			}
			set_bit(sid, dart->static_enabled);
		}
	} else if (enabled_count != -EINVAL) {
		ret = enabled_count;
		goto err_clk_disable;
	}
	remap_count = of_property_count_u32_elems(dev->of_node,
						   "apple,remap-streams");
	if (remap_count > 0) {
		u32 src, dst;

		if (dart->hw->type != DART_T8110 || remap_count % 2) {
			ret = -EINVAL;
			goto err_clk_disable;
		}
		for (int i = 0; i < remap_count; i += 2) {
			ret = of_property_read_u32_index(dev->of_node,
							 "apple,remap-streams",
							 i, &src);
			ret = ret ?: of_property_read_u32_index(dev->of_node,
								"apple,remap-streams",
								i + 1, &dst);
			if (ret || src >= dart->num_streams ||
			    dst >= dart->num_streams || src == dst) {
				ret = ret ?: -ERANGE;
				goto err_clk_disable;
			}
			set_bit(src, dart->remapped);
			dart->remap_target[src] = dst;
		}
	} else if (remap_count != -EINVAL) {
		ret = remap_count;
		goto err_clk_disable;
	}

	dart->locked = apple_dart_is_locked(dart);
	of_property_read_u32(dev->of_node, "linux-enablement-mac,owned-streams",
			     &dart->owned_streams);
	of_property_read_u32(dev->of_node, "linux-enablement-mac,static-dart-bypass-test",
			     &dart->static_dart_bypass);
	of_property_read_u32(dev->of_node, "linux-enablement-mac,retained-bypass",
			     &dart->retained_bypass);
	if (dart->owned_streams || dart->retained_bypass || dart->static_dart_bypass) {
		if (!of_machine_is_compatible("apple,j700") ||
		    !of_device_is_compatible(dev->of_node, "apple,t8140-dart") ||
		    dart->locked || (dart->owned_streams && dart->retained_bypass)) {
			ret = -EINVAL;
			goto err_clk_disable;
		}
		dart->force_available = 1;
	}
	if (dart->owned_streams) {
		if (res->start != 0x390000000ULL || dart->owned_streams != BIT(1) ||
		    dart->num_streams != 19 || dart->pgsize > PAGE_SIZE) {
			ret = -EINVAL;
			goto err_clk_disable;
		}
		dev_info(dev, "Legacy radio handoff: owning SID1 only\n");
	} else if (dart->static_dart_bypass) {
		ret = -EINVAL;
		goto err_clk_disable;
	}
	if (dart->retained_bypass) {
		if (res->start != 0x314800000ULL || dart->retained_bypass != 1 ||
		    !dart->supports_bypass ||
		    (readl(dart->regs + DART_TCR(dart, 0)) &
		     (DART_T8110_TCR_BYPASS_DART | DART_T8110_TCR_TRANSLATE_ENABLE)) !=
		    DART_T8110_TCR_BYPASS_DART ||
		    (readl(dart->regs + DART_TTBR(dart, 0, 0)) & dart->hw->ttbr_valid)) {
			ret = -EINVAL;
			goto err_clk_disable;
		}
		dev_info(dev, "Preserving inherited SID0 bypass for live MTP\n");
	}
	ret = apple_dart_static_bypass_test(dart);
	if (ret)
		goto err_clk_disable;

	/*
	 * A locked DART is one the bootloader handed over still translating for
	 * hardware we do not drive -- on J700 the display pipe scans out
	 * through stream 0 of dart-disp0 the whole time Linux is booting.
	 * Letting runtime PM suspend it drops the TTBRs underneath that
	 * traffic, and the engine then faults "TTBR invalid" forever.  Hold the
	 * reference for the DART's lifetime, as apple,force-available does.
	 */
	if (dart->locked && !dart->force_available) {
		dev_info(dev, "locked DART: holding it available for its lifetime\n");
		dart->force_available = 1;
	}

	if (!dart->locked) {
		ret = apple_dart_hw_reset(dart);
		if (ret)
			goto err_clk_disable;
	} else {
		apple_dart_drain_stale_exceptions(dart);
	}

	if (!dart->pmp_sid6_only) {
		ret = request_irq(dart->irq, apple_dart_irq, IRQF_SHARED,
				  "apple-dart fault handler", dart);
		if (ret)
			goto err_clk_disable;
	}

	platform_set_drvdata(pdev, dart);

	ret = iommu_device_sysfs_add(&dart->iommu, dev, NULL, "apple-dart.%s",
				     dev_name(&pdev->dev));
	if (ret)
		goto err_free_irq;

	ret = iommu_device_register(&dart->iommu, &apple_dart_iommu_ops, dev);
	if (ret)
		goto err_sysfs_remove;

	if (dart->force_available)
		dev_info(dev, "forcing DART available for its lifetime\n");
	else
		pm_runtime_put(dev);

	dev_info(
		&pdev->dev,
		"DART [pagesize %x, %d streams, bypass support: %d, bypass forced: %d, locked: %d, AS %d -> %d] initialized\n",
		dart->pgsize, dart->num_streams, dart->supports_bypass,
		dart->pgsize > PAGE_SIZE, dart->locked, dart->ias, dart->oas);
	return 0;

err_sysfs_remove:
	iommu_device_sysfs_remove(&dart->iommu);
err_free_irq:
	if (!dart->pmp_sid6_only)
		free_irq(dart->irq, dart);
err_clk_disable:
	pm_runtime_put(dev);
	clk_bulk_disable_unprepare(dart->num_clks, dart->clks);

	return ret;
}

static void apple_dart_remove(struct platform_device *pdev)
{
	struct apple_dart *dart = platform_get_drvdata(pdev);

	if (!dart->locked)
		apple_dart_hw_reset(dart);

	if (!dart->pmp_sid6_only)
		free_irq(dart->irq, dart);

	iommu_device_unregister(&dart->iommu);
	iommu_device_sysfs_remove(&dart->iommu);
	if (dart->force_available)
		pm_runtime_put_noidle(dart->dev);

	clk_bulk_disable_unprepare(dart->num_clks, dart->clks);
}

static const struct apple_dart_hw apple_dart_hw_t8103 = {
	.type = DART_T8020,
	.irq_handler = apple_dart_t8020_irq,
	.invalidate_tlb = apple_dart_t8020_hw_invalidate_tlb,
	.oas = 36,
	.fmt = APPLE_DART,
	.max_sid_count = 16,

	.enable_streams = DART_T8020_STREAMS_ENABLE,
	.lock = DART_T8020_CONFIG,
	.lock_bit = DART_T8020_CONFIG_LOCK,

	.error = DART_T8020_ERROR,

	.tcr = DART_T8020_TCR,
	.tcr_enabled = DART_T8020_TCR_TRANSLATE_ENABLE,
	.tcr_disabled = 0,
	.tcr_bypass = DART_T8020_TCR_BYPASS_DAPF | DART_T8020_TCR_BYPASS_DART,

	.ttbr = DART_T8020_TTBR,
	.ttbr_valid = DART_T8020_TTBR_VALID,
	.ttbr_addr_field_shift = DART_T8020_TTBR_ADDR_FIELD_SHIFT,
	.ttbr_shift = DART_T8020_TTBR_SHIFT,
	.ttbr_count = 4,
};

static const struct apple_dart_hw apple_dart_hw_t8103_usb4 = {
	.type = DART_T8020,
	.irq_handler = apple_dart_t8020_irq,
	.invalidate_tlb = apple_dart_t8020_hw_invalidate_tlb,
	.oas = 36,
	.fmt = APPLE_DART,
	.max_sid_count = 64,

	.enable_streams = DART_T8020_STREAMS_ENABLE,
	.lock = DART_T8020_CONFIG,
	.lock_bit = DART_T8020_CONFIG_LOCK,

	.error = DART_T8020_ERROR,

	.tcr = DART_T8020_TCR,
	.tcr_enabled = DART_T8020_TCR_TRANSLATE_ENABLE,
	.tcr_disabled = 0,
	.tcr_bypass = 0,

	.ttbr = DART_T8020_USB4_TTBR,
	.ttbr_valid = DART_T8020_TTBR_VALID,
	.ttbr_addr_field_shift = DART_T8020_TTBR_ADDR_FIELD_SHIFT,
	.ttbr_shift = DART_T8020_TTBR_SHIFT,
	.ttbr_count = 4,
};

static const struct apple_dart_hw apple_dart_hw_t6000 = {
	.type = DART_T6000,
	.irq_handler = apple_dart_t8020_irq,
	.invalidate_tlb = apple_dart_t8020_hw_invalidate_tlb,
	.oas = 42,
	.fmt = APPLE_DART2,
	.max_sid_count = 16,

	.enable_streams = DART_T8020_STREAMS_ENABLE,
	.lock = DART_T8020_CONFIG,
	.lock_bit = DART_T8020_CONFIG_LOCK,

	.error = DART_T8020_ERROR,

	.tcr = DART_T8020_TCR,
	.tcr_enabled = DART_T8020_TCR_TRANSLATE_ENABLE,
	.tcr_disabled = 0,
	.tcr_bypass = DART_T8020_TCR_BYPASS_DAPF | DART_T8020_TCR_BYPASS_DART,

	.ttbr = DART_T8020_TTBR,
	.ttbr_valid = DART_T8020_TTBR_VALID,
	.ttbr_addr_field_shift = DART_T8020_TTBR_ADDR_FIELD_SHIFT,
	.ttbr_shift = DART_T8020_TTBR_SHIFT,
	.ttbr_count = 4,
};

static const struct apple_dart_hw apple_dart_hw_t8110 = {
	.type = DART_T8110,
	.irq_handler = apple_dart_t8110_irq,
	.invalidate_tlb = apple_dart_t8110_hw_invalidate_tlb,
	.fmt = APPLE_DART2,
	.max_sid_count = 256,

	.enable_streams = DART_T8110_ENABLE_STREAMS,
	.lock = DART_T8110_PROTECT,
	.lock_bit = DART_T8110_PROTECT_TTBR_TCR,

	.error = DART_T8110_ERROR,

	.tcr = DART_T8110_TCR,
	.tcr_enabled = DART_T8110_TCR_TRANSLATE_ENABLE,
	.tcr_disabled = 0,
	.tcr_bypass = DART_T8110_TCR_BYPASS_DAPF | DART_T8110_TCR_BYPASS_DART,
	.tcr_4level = DART_T8110_TCR_FOUR_LEVEL,

	.ttbr = DART_T8110_TTBR,
	.ttbr_valid = DART_T8110_TTBR_VALID,
	.ttbr_addr_field_shift = DART_T8110_TTBR_ADDR_FIELD_SHIFT,
	.ttbr_shift = DART_T8110_TTBR_SHIFT,
	.ttbr_count = 1,
};

static __maybe_unused int apple_dart_suspend(struct device *dev)
{
	struct apple_dart *dart = dev_get_drvdata(dev);
	unsigned int sid, idx;

	if (dart->retained_bypass || dart->owned_streams)
		return -EBUSY;

	/* Locked DARTs can't be restored so skip saving their registers. */
	if (dart->locked || dart->pmp_sid6_only)
		return 0;

	for (sid = 0; sid < dart->num_streams; sid++) {
		dart->save_tcr[sid] = readl(dart->regs + DART_TCR(dart, sid));
		for (idx = 0; idx < dart->hw->ttbr_count; idx++)
			dart->save_ttbr[sid][idx] =
				readl(dart->regs + DART_TTBR(dart, sid, idx));
	}

	return 0;
}

static __maybe_unused int apple_dart_resume(struct device *dev)
{
	struct apple_dart *dart = dev_get_drvdata(dev);
	unsigned int sid, idx;
	int ret;

	if (dart->retained_bypass || dart->owned_streams)
		return -EBUSY;

	/* Locked DARTs can't be restored, and they should not need it */
	if (dart->locked || dart->pmp_sid6_only)
		return 0;

	ret = apple_dart_hw_reset(dart);
	if (ret) {
		dev_err(dev, "Failed to reset DART on resume\n");
		return ret;
	}

	for (sid = 0; sid < dart->num_streams; sid++) {
		for (idx = 0; idx < dart->hw->ttbr_count; idx++)
			writel(dart->save_ttbr[sid][idx],
			       dart->regs + DART_TTBR(dart, sid, idx));
		writel(dart->save_tcr[sid], dart->regs + DART_TCR(dart, sid));
	}

	return 0;
}

static DEFINE_RUNTIME_DEV_PM_OPS(apple_dart_pm_ops, apple_dart_suspend, apple_dart_resume, NULL);

static const struct of_device_id apple_dart_of_match[] = {
	{ .compatible = "apple,t8152-pmp-dart", .data = &apple_dart_hw_t8110 },
	{ .compatible = "apple,t8103-dart", .data = &apple_dart_hw_t8103 },
	{ .compatible = "apple,t8103-usb4-dart", .data = &apple_dart_hw_t8103_usb4 },
	{ .compatible = "apple,t8110-dart", .data = &apple_dart_hw_t8110 },
	{ .compatible = "apple,t6000-dart", .data = &apple_dart_hw_t6000 },
	{},
};
MODULE_DEVICE_TABLE(of, apple_dart_of_match);

#if IS_ENABLED(CONFIG_APPLE_DART_KUNIT_TEST)
static void apple_dart_t8110_v2_register_layout_test(struct kunit *test)
{
	KUNIT_EXPECT_EQ(test, DART_T8110_EXCEPTION(0), 0x4000);
	KUNIT_EXPECT_EQ(test, DART_T8110_EXCEPTION(28), 0x4070);
	KUNIT_EXPECT_EQ(test, DART_T8110_EXCEPTION_ADDR_LO(0), 0x8008);
	KUNIT_EXPECT_EQ(test, DART_T8110_EXCEPTION_ADDR_HI(28), 0x870c);
}

static void apple_dart_t8110_v2_fault_decode_test(struct kunit *test)
{
	static const char * const level_names[] = {
		"TTBR invalid", "CTE invalid", "STE invalid", "PTE invalid",
	};
	static const char * const code_names[] = {
		NULL, "write protect", "read protect", "drop protect",
		"AXI decode", "AXI error", "fill region", "region protect",
		"CTRR write protect", "APF reject", "BPF reject",
		"STT mismatch", "STT flush", "external", "illegal SID",
	};
	unsigned int i;

	for (i = 0; i < ARRAY_SIZE(level_names); i++)
		KUNIT_EXPECT_STREQ(test,
				   apple_dart_t8110_v2_fault_name(i << 12),
				   level_names[i]);

	for (i = 1; i < ARRAY_SIZE(code_names); i++)
		KUNIT_EXPECT_STREQ(test, apple_dart_t8110_v2_fault_name(i),
				   code_names[i]);

	KUNIT_EXPECT_STREQ(test, apple_dart_t8110_v2_fault_name(15), "unknown");
}

static void apple_dart_t8110_v2_shared_irq_test(struct kunit *test)
{
	KUNIT_EXPECT_FALSE(test,
			   apple_dart_t8110_v2_block_pending(0x0202, 0x777));
	KUNIT_EXPECT_TRUE(test,
			  apple_dart_t8110_v2_block_pending(0x0203, BIT(0)));
	KUNIT_EXPECT_TRUE(test,
			  apple_dart_t8110_v2_block_pending(0x0203, BIT(10)));
	KUNIT_EXPECT_FALSE(test,
			   apple_dart_t8110_v2_block_pending(0x0203, BIT(3)));
}

static void apple_dart_locked_ttbr_sync_restore_test(struct kunit *test)
{
	u64 inherited[] = {
		0x101f0cd0001, /* region94 subtree */
		0x101f0c10001, /* region95 subtree */
		0x101eb4b8003, /* framebuffer, valid and writable */
	};
	u64 saved[ARRAY_SIZE(inherited)];
	u64 imported[] = {
		0x20200000001,
		0x20200004001,
		0x20200008001,
	};

	memcpy(saved, inherited, sizeof(saved));
	apple_dart_copy_locked_ttbr(inherited, imported, sizeof(inherited));
	KUNIT_EXPECT_MEMEQ(test, inherited, imported, sizeof(inherited));

	apple_dart_copy_locked_ttbr(inherited, saved, sizeof(inherited));
	KUNIT_EXPECT_MEMEQ(test, inherited, saved, sizeof(inherited));
	KUNIT_EXPECT_FALSE(test, inherited[2] & BIT(2));
}

static struct kunit_case apple_dart_t8110_v2_test_cases[] = {
	KUNIT_CASE(apple_dart_t8110_v2_register_layout_test),
	KUNIT_CASE(apple_dart_t8110_v2_fault_decode_test),
	KUNIT_CASE(apple_dart_t8110_v2_shared_irq_test),
	KUNIT_CASE(apple_dart_locked_ttbr_sync_restore_test),
	{}
};

static struct kunit_suite apple_dart_t8110_v2_test_suite = {
	.name = "apple-dart-t8110-v2",
	.test_cases = apple_dart_t8110_v2_test_cases,
};

kunit_test_suite(apple_dart_t8110_v2_test_suite);
#endif

static struct platform_driver apple_dart_driver = {
	.driver	= {
		.name			= "apple-dart",
		.of_match_table		= apple_dart_of_match,
		.suppress_bind_attrs    = true,
		.pm			= pm_ptr(&apple_dart_pm_ops),
	},
	.probe	= apple_dart_probe,
	.remove = apple_dart_remove,
};

module_platform_driver(apple_dart_driver);

MODULE_DESCRIPTION("IOMMU API for Apple's DART");
MODULE_AUTHOR("Sven Peter <sven@svenpeter.dev>");
MODULE_LICENSE("GPL v2");
