// SPDX-License-Identifier: GPL-2.0-only OR MIT

//! SPTM address spaces and their confirmed mapping shadow.
//!
//! Each run shares GPUVM's counted SG lease. Reserving the shadow before
//! publication makes uncertain operations retain both the VA and its backing,
//! even when the GPUVA transaction cannot install its normal owner. Successful
//! unmap releases those references before GPUVM attempts idle page reclaim.

use super::{sptm::{ContextState, MapPages, MapPlan, Protection, TablePlan, PAGE}, sptm_transport::{AddressSpace, Transport}, table::BatchStats, MappingBacking};
use crate::{g17::status::VmStatus, pgtable::{prot, Prot}};
use core::ops::Range;
use kernel::{addr::PhysicalAddr, prelude::*, rbtree::{RBTree, RBTreeNode, RBTreeNodeReservation}, sync::Arc};
use core::sync::atomic::{AtomicU64, Ordering};

/// Private-space L2/L3 table frames x1n1 allocates from its own heap
/// (sptm_hv_uat_alloc_frame -> memalign). x1n1 never returns them to the heap:
/// DESTROY pools them for later spaces, so its heap holds the boot's peak of
/// live private tables, and an x1n1 heap allocation failure is a hypervisor
/// panic (dlmalloc MALLOC_FAILURE_ACTION). Keeping live tables under this
/// budget bounds that peak. 2048 frames (32 MiB): a 4 x 64 GiB concurrent fill
/// reset the machine at >= 7930 live frames, far below the heap's 1 GiB VA cap,
/// so the real headroom is unknown; keep a wide margin until x1n1 reports it.
const PRIVATE_TABLE_BUDGET: u64 = 2048;
static PRIVATE_TABLES: AtomicU64 = AtomicU64::new(0);

const L3_SPAN: u64 = 1 << 25;
const L3_PER_L2: u64 = 1 << 11;

/// Live private table frames, for diagnostics.
pub(super) fn private_tables() -> u64 { PRIVATE_TABLES.load(Ordering::Relaxed) }

fn refuse_tables(needed: u64) -> Error {
    static COUNT: AtomicU64 = AtomicU64::new(0);
    let n = COUNT.fetch_add(1, Ordering::Relaxed);
    if n < 8 || (n + 1).is_power_of_two() {
        pr_warn!("asahi: SPTM table budget refusal: need {} live {} budget {} (occurrence {})\n",
            needed, private_tables(), PRIVATE_TABLE_BUDGET, n + 1);
    }
    ENOMEM
}

/// Kernel-owned, physically contiguous scratch for sparse fills: one provider
/// MAP operation covers up to SPTM_UAT_SEG_LIMIT (64) consecutive pages, so a
/// 64-page tile repeated across a range needs 64 times fewer operations than
/// one repeated page. Zeroed at allocation.
pub(super) struct SparseTile {
    page: *mut kernel::bindings::page,
    physical: u64,
    /// Current provider mappings of each frame. A VA-aligned fill maps frame
    /// `(va / PAGE) % 64`, so short runs can concentrate on a few frames;
    /// per-frame counts keep the bound exact.
    frames: [core::sync::atomic::AtomicU32; 64],
}

// SAFETY: The pages are exclusively owned until Drop; only their physical
// address is shared, and it is immutable.
unsafe impl Send for SparseTile {}
// SAFETY: See Send. No interior mutability.
unsafe impl Sync for SparseTile {}

impl SparseTile {
    /// 64 pages: the provider's per-operation segment limit.
    pub(super) const ORDER: u32 = 6;

    pub(super) fn new() -> Result<Arc<Self>> {
        // SAFETY: A plain zeroed higher-order page allocation.
        let page = unsafe { kernel::bindings::alloc_pages(
            kernel::bindings::GFP_KERNEL | kernel::bindings::__GFP_ZERO, Self::ORDER) };
        if page.is_null() { return Err(ENOMEM); }
        // SAFETY: page is a live allocation returned above.
        let physical = unsafe { kernel::bindings::page_to_phys(page) } as u64;
        let tile = Arc::new(Self { page, physical,
            frames: [const { core::sync::atomic::AtomicU32::new(0) }; 64] }, GFP_KERNEL);
        if tile.is_err() {
            // SAFETY: The allocation above was not published anywhere.
            unsafe { kernel::bindings::__free_pages(page, Self::ORDER) };
        }
        Ok(tile?)
    }

    pub(super) fn physical(&self) -> u64 { self.physical }

    pub(super) fn pages(&self) -> u64 { 1 << Self::ORDER }

    /// Highest current mapping count of any frame.
    pub(super) fn max_frame_maps(&self) -> u32 {
        self.frames.iter().map(|count| count.load(Ordering::Relaxed)).max().unwrap_or(0)
    }

    /// Add or remove the mappings of `pages` pages starting at VA page `first`.
    fn account(&self, first: u64, pages: u64, add: bool) {
        let period = self.pages();
        let base = (pages / period) as u32;
        let extra = pages % period;
        for frame in 0..period {
            let offset = (frame + period - first % period) % period;
            let count = base + u32::from(offset < extra);
            if count == 0 { continue; }
            let slot = &self.frames[frame as usize];
            if add { slot.fetch_add(count, Ordering::Relaxed); }
            else { slot.fetch_sub(count, Ordering::Relaxed); }
        }
    }
}

impl Drop for SparseTile {
    fn drop(&mut self) {
        // SAFETY: Runs hold an Arc for as long as the provider may map these
        // pages; an uncertain address space leaks its runs (and so the tile).
        unsafe { kernel::bindings::__free_pages(self.page, Self::ORDER) };
    }
}

#[derive(Clone)]
enum RunBacking {
    // Field kept only to retain the counted SG lease.
    Gem(#[allow(dead_code)] MappingBacking),
    Tile(Arc<SparseTile>),
}

#[derive(Clone)]
struct Run {
    end: u64,
    physical: u64,
    prot: Prot,
    repeat: bool,
    /// Nonzero for a tiled run: `physical` is the tile base and the page at
    /// VA `a` is tile page `(a / PAGE) % period`, independent of the run start.
    period: u64,
    _backing: Option<RunBacking>,
}

impl Run {
    fn physical_at(&self, start: u64, address: u64) -> u64 {
        if self.period != 0 {
            return self.physical + (address / PAGE) % self.period * PAGE;
        }
        self.physical + if self.repeat { 0 } else { address - start }
    }
}

fn protection(value: Prot) -> Result<Protection> {
    Ok(if value == prot::PROT_FW_PRIV_RW { Protection::FirmwarePrivateReadWrite }
        else if value == prot::PROT_GPU_SHARED_RW { Protection::GpuReadWrite }
        else if value == prot::PROT_GPU_SHARED_RO { Protection::GpuReadOnly }
        else if value == prot::PROT_GPU_FW_SHARED_RW { Protection::SharedReadWrite }
        else if value == prot::PROT_GPU_RO_FW_PRIV_RW { Protection::FirmwareWriteGpuRead }
        else if value == prot::PROT_FW_SHARED_RW { Protection::FirmwareReadWrite }
        else if value == prot::PROT_FW_SHARED_RO { Protection::FirmwareReadOnly }
        else if value == prot::PROT_FW_MMIO_RW { Protection::FirmwareMmioReadWrite }
        else if value == prot::PROT_FW_MMIO_RO { Protection::FirmwareMmioReadOnly }
        else { return Err(ENOTSUPP); })
}

pub(super) struct PageTable {
    space: AddressSpace,
    runs: RBTree<u64, Run>,
    status: Arc<VmStatus>,
    /// Private spaces: L3 tables known to exist, one bitmap per L2 span (word
    /// 32 bit 0 = the L2 table itself), and their count in PRIVATE_TABLES.
    tables: RBTree<u64, KBox<[u64; 33]>>,
    table_count: u64,
}

impl PageTable {
    pub(super) fn new(transport: &Arc<Transport>, status: Arc<VmStatus>) -> Result<Self> {
        Ok(Self { space: AddressSpace::private(transport)?, runs: RBTree::new(), status,
            tables: RBTree::new(), table_count: 0 })
    }

    pub(super) fn new_global(transport: &Arc<Transport>, status: Arc<VmStatus>) -> Self {
        Self { space: AddressSpace::global(transport), runs: RBTree::new(), status,
            tables: RBTree::new(), table_count: 0 }
    }

    pub(super) fn private_handle(&self) -> Option<u8> { self.space.private_handle() }

    pub(super) fn ensure_certain(&self) -> Result {
        match self.space.state() {
            ContextState::Unbound | ContextState::Bound(_) => Ok(()),
            _ => Err(EIO),
        }
    }

    fn report_uncertain(&self) {
        if matches!(self.space.state(), ContextState::Uncertain(_)) {
            self.status.record(EIO);
        }
    }

    pub(super) fn validate_range(&self, range: Range<u64>) -> Result {
        self.ensure_certain()?;
        if range.start >= range.end || (range.start | range.end) & (PAGE - 1) != 0
            || range.start < self.space.range().start || range.end > self.space.range().end { return Err(EINVAL); }
        Ok(())
    }

    fn run_at(&self, address: u64) -> Option<(u64, &Run)> {
        let start = match self.runs.cursor_lower_bound(&address) {
            Some(cursor) if *cursor.current().0 == address => Some(address),
            Some(cursor) => cursor.peek_prev().map(|(key, _)| *key),
            None => self.runs.cursor_back().map(|cursor| *cursor.current().0),
        }?;
        let run = self.runs.get(&start)?;
        (address < run.end).then_some((start, run))
    }

    pub(super) fn prepare_map(&self, range: Range<u64>) -> Result {
        self.validate_range(range.clone())?;
        if self.run_at(range.start).is_some()
            || self.runs.cursor_lower_bound(&range.start)
                .is_some_and(|cursor| *cursor.current().0 < range.end) { return Err(EBUSY); }
        Ok(())
    }

    fn l3_spans(range: &Range<u64>) -> impl Iterator<Item = (u64, usize)> {
        let first = range.start / L3_SPAN;
        let last = (range.end - 1) / L3_SPAN;
        (first..=last).map(|span| (span / L3_PER_L2, (span % L3_PER_L2) as usize))
    }

    /// Reserve budget for every private table frame `range` may create
    /// before any provider operation. Returns the count to commit or release.
    fn reserve_tables(&mut self, range: &Range<u64>) -> Result<u64> {
        if !matches!(self.space, AddressSpace::Private(_)) { return Ok(0); }
        let mut needed = 0u64;
        let mut last_l2 = None;
        for (l2, l3) in Self::l3_spans(range) {
            let bits = self.tables.get(&l2);
            if last_l2 != Some(l2) {
                last_l2 = Some(l2);
                if !bits.is_some_and(|bits| bits[32] & 1 != 0) { needed += 1; }
            }
            if !bits.is_some_and(|bits| bits[l3 / 64] & (1 << (l3 % 64)) != 0) { needed += 1; }
        }
        if needed == 0 { return Ok(0); }
        let live = PRIVATE_TABLES.fetch_add(needed, Ordering::AcqRel) + needed;
        if live > PRIVATE_TABLE_BUDGET {
            PRIVATE_TABLES.fetch_sub(needed, Ordering::AcqRel);
            return Err(refuse_tables(needed));
        }
        // Bitmap storage is allocated before publication; marking cannot fail.
        let mut last_l2 = None;
        for (l2, _) in Self::l3_spans(range) {
            if last_l2 == Some(l2) { continue; }
            last_l2 = Some(l2);
            if self.tables.get(&l2).is_none() {
                let node = KBox::new([0u64; 33], GFP_KERNEL).map_err(Error::from)
                    .and_then(|bits| RBTreeNode::new(l2, bits, GFP_KERNEL).map_err(Error::from));
                match node {
                    Ok(node) => { self.tables.insert(node); }
                    Err(error) => {
                        PRIVATE_TABLES.fetch_sub(needed, Ordering::AcqRel);
                        return Err(error);
                    }
                }
            }
        }
        Ok(needed)
    }

    /// A submitted plan may have created any table of its range: count them
    /// all until the space is destroyed. An unsubmitted plan created none.
    fn settle_tables(&mut self, range: &Range<u64>, reserved: u64, submitted: bool) {
        if reserved == 0 { return; }
        if !submitted {
            PRIVATE_TABLES.fetch_sub(reserved, Ordering::AcqRel);
            return;
        }
        for (l2, l3) in Self::l3_spans(range) {
            if let Some(bits) = self.tables.get_mut(&l2) {
                bits[32] |= 1;
                bits[l3 / 64] |= 1 << (l3 % 64);
            }
        }
        self.table_count += reserved;
    }

    pub(super) fn map_pages(&mut self, range: Range<u64>, physical: PhysicalAddr,
        prot: Prot, repeat: bool, backing: Option<&MappingBacking>) -> Result
    {
        self.prepare_map(range.clone())?;
        // Only firmware MMIO in the global window can lack a pinned RAM
        // owner. The provider independently validates its physical allowlist.
        let io = prot == prot::PROT_FW_MMIO_RW || prot == prot::PROT_FW_MMIO_RO;
        if io {
            if !matches!(self.space, AddressSpace::Global { .. }) || backing.is_some() || repeat {
                return Err(EINVAL);
            }
        } else if backing.is_none() {
            return Err(EINVAL);
        }
        let protection = protection(prot)?;
        let count = (range.end - range.start) / PAGE;
        if physical & (PAGE - 1) != 0 { return Err(EINVAL); }
        physical.checked_add(if repeat { PAGE } else { range.end - range.start }).ok_or(EOVERFLOW)?;
        let map_pages = if repeat { MapPages::Repeated { physical, count } }
            else { MapPages::Contiguous { physical, count } };
        let node = RBTreeNode::new(range.start, Run {
            end: range.end, physical, prot, repeat, period: 0,
            _backing: backing.cloned().map(RunBacking::Gem),
        }, GFP_KERNEL)?;
        let reserved = self.reserve_tables(&range)?;
        match self.space.map(range.start, map_pages, protection) {
            Ok(()) => {
                self.settle_tables(&range, reserved, true);
                self.runs.insert(node);
                Ok(())
            }
            Err(failure) => {
                self.settle_tables(&range, reserved, failure.submitted);
                if failure.may_have_mapped { self.runs.insert(node); }
                self.report_uncertain();
                Err(failure.error)
            }
        }
    }

    /// Map the physically contiguous chunks of one GPU mapping, which together
    /// cover `chunks[0].start..chunks.last().end` without gaps, as one provider
    /// plan: the range's table operations once, then every chunk's leaf
    /// operations, in native-sized batches and (private spaces) one ASID sync.
    ///
    /// Shadow bookkeeping matches mapping the chunks one by one: every run
    /// node and table reservation exists before the first operation; success
    /// records every run. On failure that may have published any page, every
    /// run is retained, so no backing page can be released while it might
    /// still be mapped (the space is poisoned exactly as for a single run).
    pub(super) fn map_chunks(&mut self, chunks: &[(Range<u64>, PhysicalAddr, Prot)],
        backing: &MappingBacking) -> Result<BatchStats>
    {
        let (Some(first), Some(last)) = (chunks.first(), chunks.last()) else { return Err(EINVAL); };
        let union = first.0.start..last.0.end;
        let space = self.space.plan_space()?;
        let mut at = union.start;
        let mut nodes = KVVec::with_capacity(chunks.len(), GFP_KERNEL)?;
        let mut plans = KVVec::with_capacity(chunks.len(), GFP_KERNEL)?;
        let mut pages = 0u64;
        for (range, physical, chunk_prot) in chunks {
            if range.start != at || range.end <= range.start { return Err(EINVAL); }
            at = range.end;
            if *chunk_prot == prot::PROT_FW_MMIO_RW || *chunk_prot == prot::PROT_FW_MMIO_RO {
                return Err(EINVAL);
            }
            self.prepare_map(range.clone())?;
            let count = (range.end - range.start) / PAGE;
            if *physical & (PAGE - 1) != 0 { return Err(EINVAL); }
            physical.checked_add(range.end - range.start).ok_or(EOVERFLOW)?;
            let plan = MapPlan::without_tables(space, range.start,
                MapPages::Contiguous { physical: *physical, count }, protection(*chunk_prot)?)
                .map_err(|_| EINVAL)?;
            plans.push(plan, GFP_KERNEL)?;
            nodes.push(RBTreeNode::new(range.start, Run {
                end: range.end, physical: *physical, prot: *chunk_prot, repeat: false, period: 0,
                _backing: Some(RunBacking::Gem(backing.clone())),
            }, GFP_KERNEL)?, GFP_KERNEL)?;
            pages += count;
        }
        let tables = TablePlan::new(space, &union).map_err(|_| EINVAL)?;
        let plan = Iterator::chain(tables, plans.iter().cloned().flatten());
        let operations = plan.clone().count() as u64;
        let reserved = self.reserve_tables(&union)?;
        let result = self.space.map_batch(plan);
        let submitted = match &result { Ok(()) => true, Err(failure) => failure.submitted };
        self.settle_tables(&union, reserved, submitted);
        let keep = match &result { Ok(()) => true, Err(failure) => failure.may_have_mapped };
        if keep {
            for node in nodes { self.runs.insert(node); }
        }
        if let Err(failure) = result {
            self.report_uncertain();
            return Err(failure.error);
        }
        Ok(BatchStats { chunks: chunks.len() as u64, pages, operations })
    }

    /// Fill `range` with `tile` repeated at its own period, VA-aligned so that
    /// any later split keeps the same page-to-tile correspondence.
    pub(super) fn map_tiled(&mut self, range: Range<u64>, tile: &Arc<SparseTile>, prot: Prot)
        -> Result
    {
        self.prepare_map(range.clone())?;
        if prot == prot::PROT_FW_MMIO_RW || prot == prot::PROT_FW_MMIO_RO
            || !matches!(self.space, AddressSpace::Private(_)) {
            return Err(EINVAL);
        }
        let protection = protection(prot)?;
        let count = (range.end - range.start) / PAGE;
        let period = tile.pages();
        let pages = MapPages::Tiled {
            base: tile.physical(), period, first: range.start / PAGE % period, count,
        };
        let node = RBTreeNode::new(range.start, Run {
            end: range.end, physical: tile.physical(), prot, repeat: false, period,
            _backing: Some(RunBacking::Tile(tile.clone())),
        }, GFP_KERNEL)?;
        let reserved = self.reserve_tables(&range)?;
        match self.space.map(range.start, pages, protection) {
            Ok(()) => {
                self.settle_tables(&range, reserved, true);
                tile.account(range.start / PAGE, count, true);
                self.runs.insert(node);
                Ok(())
            }
            Err(failure) => {
                self.settle_tables(&range, reserved, failure.submitted);
                if failure.may_have_mapped {
                    tile.account(range.start / PAGE, count, true);
                    self.runs.insert(node);
                }
                self.report_uncertain();
                Err(failure.error)
            }
        }
    }

    /// Rollback/reclaim can skip holes; normal unmap must be wholly covered.
    /// All shadow splits and command storage are reserved before publication.
    pub(super) fn unmap_pages(&mut self, range: Range<u64>, allow_holes: bool) -> Result {
        self.validate_range(range.clone())?;
        let mut touched = KVec::new();
        let mut ranges = KVec::new();
        let mut at = range.start;
        while at < range.end {
            let Some((start, run)) = self.run_at(at) else {
                if !allow_holes { return Err(EFAULT); }
                at = self.runs.cursor_lower_bound(&at)
                    .map_or(range.end, |cursor| (*cursor.current().0).min(range.end));
                continue;
            };
            let end = run.end.min(range.end);
            ranges.push(at..end, GFP_KERNEL)?;
            touched.push((start, run.clone()), GFP_KERNEL)?;
            at = end;
        }
        if touched.is_empty() { return Ok(()); }
        let mut spare = KVec::with_capacity(2, GFP_KERNEL)?;
        // Only the first and last runs can have surviving fragments.
        for _ in 0..2 { spare.push(RBTreeNodeReservation::<u64, Run>::new(GFP_KERNEL)?, GFP_KERNEL)?; }
        if let Err(error) = self.space.unmap_ranges(&ranges) {
            self.report_uncertain();
            return Err(error);
        }
        for (start, run) in touched {
            if let Some(RunBacking::Tile(tile)) = &run._backing {
                let from = start.max(range.start);
                tile.account(from / PAGE, (run.end.min(range.end) - from) / PAGE, false);
            }
            self.runs.remove(&start);
            if start < range.start {
                let left = Run { end: range.start, ..run.clone() };
                self.runs.insert(spare.pop().expect("reserved left fragment").into_node(start, left));
            }
            if run.end > range.end {
                let physical = if run.period != 0 { run.physical } else { run.physical_at(start, range.end) };
                let right = Run { physical, ..run };
                self.runs.insert(spare.pop().expect("reserved right fragment").into_node(range.end, right));
            }
        }
        Ok(())
    }

    pub(super) fn covers_range(&self, range: Range<u64>, read: bool, write: bool) -> Result<bool> {
        self.ensure_certain()?;
        if self.validate_range(range.clone()).is_err() { return Ok(false); }
        let mut at = range.start;
        while at < range.end {
            let Some((_, run)) = self.run_at(at) else { return Ok(false); };
            if !run.prot.allows_gpu(read, write) { return Ok(false); }
            at = run.end;
        }
        Ok(true)
    }

    pub(super) fn translate_iova(&self, address: u64) -> Result<u64> {
        self.ensure_certain()?;
        let (start, run) = self.run_at(address).ok_or(EFAULT)?;
        Ok(run.physical_at(start, address & !(PAGE - 1)) + (address & (PAGE - 1)))
    }

    pub(super) fn bind(&mut self, context: u8) -> Result {
        self.ensure_certain()?;
        let result = self.space.bind(context);
        self.report_uncertain();
        result
    }

    pub(super) fn context_mask(&self) -> Result<u64> {
        self.ensure_certain()?;
        Ok(match self.space.state() {
            ContextState::Bound(id) => 1u64 << id,
            _ => 0,
        })
    }

    pub(super) fn sync_context(&mut self) -> Result<u64> {
        let mask = self.context_mask()?;
        if mask == 0 { return Err(ENOENT); }
        let result = self.space.sync_context();
        self.report_uncertain();
        result.map(|()| mask)
    }

    /// The execution owner must prove every queue using the context retired.
    pub(super) fn unbind(&mut self) -> Result {
        let result = self.space.unbind();
        self.report_uncertain();
        result
    }
}

impl Drop for PageTable {
    fn drop(&mut self) {
        // Only the execution owner can withdraw a live context after all its
        // queues retire. Never infer retirement from the final host reference.
        if self.space.state() == ContextState::Unbound {
            loop {
                let next = self.runs.iter().next().map(|(start, run)| (*start, run.end));
                let Some((start, end)) = next else {
                    if self.space.destroy().is_err() {
                        pr_err!("SPTM: empty address space could not be released\n");
                    } else {
                        // x1n1 pools the frames for later spaces: no longer live.
                        PRIVATE_TABLES.fetch_sub(self.table_count, Ordering::AcqRel);
                    }
                    return;
                };
                if self.space.unmap(start, (end - start) / PAGE).is_err() {
                    break;
                }
                self.runs.remove(&start);
            }
        }
        self.status.record(EIO);
        core::mem::forget(core::mem::replace(&mut self.runs, RBTree::new()));
        if let Some(handle) = self.space.private_handle() {
            let state = match self.space.state() {
                ContextState::Unbound => 0,
                ContextState::Bound(id) => 0x100 | u64::from(id),
                ContextState::Uncertain(id) => 0x200 | id.map_or(0xff, u64::from),
                ContextState::Destroyed => 0x300,
            };
            super::sptm_transport::note_private_retained(Some(handle), state);
        }
        pr_err!("SPTM: address space retained without confirmed retirement\n");
    }
}
