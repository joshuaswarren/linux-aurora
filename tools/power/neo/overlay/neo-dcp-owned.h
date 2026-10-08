/* SPDX-License-Identifier: GPL-2.0-only */
/* Bounded J700 DCP SID23 DMA ownership for the retained transport trial.
 * DART2 encodings follow drivers/iommu/io-pgtable-dart.c and apple-dart.c.
 * Reserve one entirely empty L1 branch; preserve all inherited entries,
 * TTBRs, TCRs and other streams. Never release an advertised DMA buffer.
 */
#include <linux/dma-mapping.h>
#include <linux/iommu.h>
#include <linux/scatterlist.h>
#include <asm/barrier.h>
#include <asm/sysreg.h>

#define NEO_DCP_POOL_SIZE SZ_1M
#define NEO_DCP_SID 23

struct neo_dcp_owned {
	struct device *dev;
	void __iomem *dart;
	u64 *root, *before, *leaf;
	void *pool;
	dma_addr_t leaf_dma, pool_dma;
	phys_addr_t root_pa, leaf_pa, pool_pa;
	u64 iova, entry;
	u32 ttbr;
	unsigned int slot, cache_line;
	bool mapped, published, advertised, failed;
};

/* The bootloader root is reserved NOMAP RAM, not a DMA-API allocation or
 * linear-map address. Clean the one modified cache line through its WB
 * memremap alias. This is the DC CVAC + barrier operation used by arm64's
 * dcache_clean_poc (arch/arm64/mm/cache.S, ARM Ltd., GPL-2.0-only), narrowed
 * to one aligned u64 and this explicitly admitted J700 experiment.
 */
static void neo_dcp_dma_root_write(struct neo_dcp_owned *o, u64 entry)
{
	unsigned long line = (unsigned long)&o->root[o->slot] & ~(o->cache_line - 1UL);

	WRITE_ONCE(o->root[o->slot], entry);
	asm volatile("dc cvac, %0" : : "r" (line) : "memory");
	dsb(sy);
}

static int neo_dcp_dma_physical(struct device *dev, void *cpu, dma_addr_t dma,
			      size_t size, phys_addr_t *physical)
{
	struct sg_table sg;
	int ret = dma_get_sgtable(dev, &sg, cpu, dma, size);

	if (ret)
		return ret;
	if (sg.orig_nents != 1 || sg.sgl->length < size) {
		ret = -ERANGE;
	} else {
		*physical = sg_phys(sg.sgl);
		if (!IS_ALIGNED(*physical, SZ_16K) ||
		    *physical < 0x10000000000ULL ||
		    *physical + size > 0x10200000000ULL)
			ret = -ERANGE;
	}
	sg_free_table(&sg);
	return ret;
}

static int neo_dcp_dma_flush(struct neo_dcp_owned *o)
{
	u32 value;
	int ret = readl_poll_timeout(o->dart + 0x80, value, !(value & BIT(31)), 1, 10000);

	if (ret)
		return ret;
	/* T8140 DART revision 0x0203 uses the v2 stream command flag. */
	dma_wmb();
	writel(BIT(15) | BIT(8) | NEO_DCP_SID, o->dart + 0x80);
	return readl_poll_timeout(o->dart + 0x80, value, !(value & BIT(31)), 1, 10000);
}

static int neo_dcp_dma_preserved(struct neo_dcp_owned *o)
{
	unsigned int i;

	if (readl(o->dart + 0x1000 + 4*NEO_DCP_SID) != 1 ||
	    readl(o->dart + 0x1400 + 4*NEO_DCP_SID) != o->ttbr)
		return -ESTALE;
	for (i = 0; i < 2048; i++)
		if (READ_ONCE(o->root[i]) !=
		    (i == o->slot && o->published ? o->entry : o->before[i]))
			return -ESTALE;
	return 0;
}

static int neo_dcp_dma_release(struct neo_dcp_owned *o)
{
	int ret;

	if (o->advertised || o->failed)
		return -EBUSY;
	if (o->published) {
		ret = neo_dcp_dma_preserved(o);
		if (ret)
			goto fail;
		neo_dcp_dma_root_write(o, 0);
		o->published = false;
		ret = neo_dcp_dma_flush(o);
		if (ret)
			goto fail;
		ret = neo_dcp_dma_preserved(o);
		if (ret)
			goto fail;
	}
	if (o->pool)
		dma_free_coherent(o->dev, NEO_DCP_POOL_SIZE, o->pool, o->pool_dma);
	if (o->leaf)
		dma_free_coherent(o->dev, SZ_16K, o->leaf, o->leaf_dma);
	if (o->root)
		memunmap(o->root);
	kfree(o->before);
	memset(o, 0, sizeof(*o));
	return 0;
fail:
	/* A failed invalidation can leave a live hardware reference. Pin all
	 * allocations until reboot; do not reclaim possibly DMA-visible RAM.
	 */
	o->failed = true;
	return ret;
}

static int neo_dcp_dma_prepare(struct neo_dcp_owned *o, struct device *dev,
			       void __iomem *dart)
{
	unsigned int i;
	int ret;

	if (o->dev || iommu_get_domain_for_dev(dev))
		return -EBUSY;
	if (PAGE_SIZE != SZ_16K || readl(dart + 8) != 0x2a2a0203 ||
	    readl(dart + 12) != 0x10020 ||
	    readl(dart + 0x1000 + 4*NEO_DCP_SID) != 1)
		return -EINVAL;
	o->ttbr = readl(dart + 0x1400 + 4*NEO_DCP_SID);
	if ((o->ttbr & 3) != 1)
		return -EINVAL;
	o->root_pa = (u64)(o->ttbr >> 2) << 14;
	if (!retained_ram(o->root_pa, SZ_16K))
		return -ERANGE;
	for (i = 0; i < 32; i++)
		if (i != NEO_DCP_SID && readl(dart + 0x1400 + 4*i) == o->ttbr)
			return -EBUSY;
	ret = dma_set_mask_and_coherent(dev, DMA_BIT_MASK(42));
	if (ret)
		return ret;
	o->dev = dev;
	o->dart = dart;
	o->cache_line = 4U << ((read_sysreg(ctr_el0) >> 16) & 15);
	if (o->cache_line < 16 || o->cache_line > 2048) {
		ret = -EINVAL;
		goto fail;
	}
	o->root = memremap(o->root_pa, SZ_16K, MEMREMAP_WB);
	if (!o->root) {
		ret = -ERANGE;
		goto fail;
	}
	o->before = kmemdup(o->root, SZ_16K, GFP_KERNEL);
	if (!o->before) {
		ret = -ENOMEM;
		goto fail;
	}
	/* Reserve a whole 32 MiB hole, away from inherited low IOVAs. */
	for (i = 128; i < 256; i++)
		if (!o->before[i])
			break;
	if (i == 256) {
		ret = -ENOSPC;
		goto fail;
	}
	o->slot = i;
	o->iova = (u64)i << 25;
	o->leaf = dma_alloc_coherent(dev, SZ_16K, &o->leaf_dma, GFP_KERNEL);
	o->pool = dma_alloc_coherent(dev, NEO_DCP_POOL_SIZE, &o->pool_dma, GFP_KERNEL);
	if (!o->leaf || !o->pool) {
		ret = -ENOMEM;
		goto fail;
	}
	ret = neo_dcp_dma_physical(dev, o->leaf, o->leaf_dma, SZ_16K, &o->leaf_pa);
	if (!ret)
		ret = neo_dcp_dma_physical(dev, o->pool, o->pool_dma, NEO_DCP_POOL_SIZE, &o->pool_pa);
	if (ret)
		goto fail;
	memset(o->leaf, 0, SZ_16K);
	memset(o->pool, 0, NEO_DCP_POOL_SIZE);
	for (i = 0; i < NEO_DCP_POOL_SIZE / SZ_16K; i++)
		o->leaf[i] = ((o->pool_pa + i*SZ_16K) >> 4) | 3 | (0xfffULL << 40);
	o->entry = (o->leaf_pa >> 4) | 1;
	ret = neo_dcp_dma_preserved(o);
	if (ret)
		goto fail;
	dma_wmb();
	neo_dcp_dma_root_write(o, o->entry);
	o->published = true;
	ret = neo_dcp_dma_flush(o);
	if (!ret)
		ret = neo_dcp_dma_preserved(o);
	if (ret) {
		o->failed = true;
		return ret;
	}
	o->mapped = true;
	dev_info(dev, "DCP owned DMA root=%pa slot=%u leaf=%pa pool=%pa iova=%#llx size=%#x; inherited entries preserved\n",
		 &o->root_pa, o->slot, &o->leaf_pa, &o->pool_pa, o->iova, NEO_DCP_POOL_SIZE);
	return 0;
fail:
	neo_dcp_dma_release(o);
	return ret;
}
