// SPDX-License-Identifier: GPL-2.0-only OR MIT

//! Retained USC private-memory pools and their submission counters.
//!
//! A logical context owns one render pool, shared by its render pairs. A
//! physical compute queue owns a separate pool. All mutation and publication
//! runs under the device mutex. Pool aliases must outlive every firmware
//! reference and are released only after the owner's teardown is acknowledged.

use super::fw::freelist::*;
use super::fw::initdata::{FreeListArgs, FreeListClass, FreeListControl, FREE_LIST_BLOCK_SIZE};
use super::object::{Allocator, CpuMap, KernelObject};
use crate::mmu;
use core::sync::atomic::{fence, AtomicBool, AtomicU16, AtomicU32, AtomicU64, Ordering};
use kernel::{
    new_mutex,
    prelude::*,
    sync::{Arc, Mutex},
};

mod render_ids;

const NO_RENDER_ID: u16 = u16::MAX;
static NEXT_RENDER_OWNER: AtomicU64 = AtomicU64::new(1);

struct RenderIdCell {
    owner: u64,
    slot: AtomicU16,
    generation: AtomicU64,
    closed: AtomicBool,
    storage: Arc<RenderStorage>,
}
impl RenderIdCell {
    fn has_storage(&self) -> bool {
        self.storage.backing.lock().is_some() || self.storage.spare.lock().is_some()
    }
}

/// Detached after the device's final free-list release. Drop outside the
/// device and identity locks because mappings may take memory-management locks.
#[must_use]
pub(crate) struct ReclaimedRenderId {
    _backing: Option<RenderBacking>,
    spare: Option<Pages>,
}
impl ReclaimedRenderId {
    /// Caller is outside the device lock. Remove the previous client's alias
    /// before handing physical storage to another logical pool.
    fn into_pages(self) -> Option<Pages> {
        self._backing.map(RenderBacking::into_pages).or(self.spare)
    }
}

struct RenderIdState {
    leases: render_ids::State,
    cells: [Option<Arc<RenderIdCell>>; render_ids::COUNT],
}

/// Render pools occupy 129..255; compute uses its QID plus one, in 1..128.
/// First-free allocation keeps the shared firmware namespace deterministic.
#[pin_data]
pub(crate) struct RenderIds {
    used: [AtomicU64; 2],
    available: Arc<super::availability::Availability>,
    #[pin]
    state: Mutex<RenderIdState>,
}
impl RenderIds {
    #[inline(never)]
    pub(crate) fn new() -> Result<Arc<Self>> {
        Arc::pin_init(try_pin_init!(Self {
            used: [AtomicU64::new(0), AtomicU64::new(1 << 63)],
            available: super::availability::Availability::new()?,
            state <- new_mutex!(RenderIdState {
                leases: render_ids::State::new(),
                cells: [const { None }; render_ids::COUNT],
            }, "G17 render identities"),
        }), GFP_KERNEL)
    }

    pub(crate) fn availability(&self) -> &Arc<super::availability::Availability> {
        &self.available
    }

    fn pin(self: &Arc<Self>, cell: &Arc<RenderIdCell>, assigned: impl FnOnce()) -> Result<RenderIdPin> {
        let mut state = self.state.lock();
        let slot = cell.slot.load(Ordering::Acquire);
        if slot != NO_RENDER_ID {
            let lease = render_ids::Lease {
                slot, owner: cell.owner, generation: cell.generation.load(Ordering::Acquire),
            };
            match state.leases.pin(lease) {
                Ok(_) => return Ok(RenderIdPin { allocator: self.clone(), cell: cell.clone(), lease }),
                Err(render_ids::Error::Overflow) => return Err(EOVERFLOW),
                Err(_) => {}
            }
        }
        let claim = state.leases.claim(cell.owner, |_| false).map_err(|error| match error {
            render_ids::Error::Busy | render_ids::Error::PendingRelease => ENOSPC,
            render_ids::Error::Overflow => EOVERFLOW,
            _ => EIO,
        })?;
        let index = usize::from(claim.lease.slot - render_ids::FIRST);
        state.cells[index] = Some(cell.clone());
        assigned();
        cell.generation.store(claim.lease.generation, Ordering::Relaxed);
        cell.slot.store(claim.lease.slot, Ordering::Release);
        Ok(RenderIdPin { allocator: self.clone(), cell: cell.clone(), lease: claim.lease })
    }

    fn close(&self, cell: &Arc<RenderIdCell>) {
        cell.closed.store(true, Ordering::Release);
        let mut state = self.state.lock();
        let slot = cell.slot.load(Ordering::Acquire);
        if slot == NO_RENDER_ID { return; }
        let index = usize::from(slot - render_ids::FIRST);
        if state.cells[index].as_ref().is_some_and(|owner| Arc::ptr_eq(owner, cell))
            && state.leases.remove(cell.owner, |_| true).is_ok() {
            let old = state.cells[index].take();
            cell.slot.store(NO_RENDER_ID, Ordering::Release);
            drop(state);
            drop(old);
            self.available.notify();
        }
    }

    /// Caller holds the device mutex and supplies the exact final release proof.
    pub(crate) fn reclaim_one(&self, released: impl FnMut(u16) -> bool) -> Option<ReclaimedRenderId> {
        self.reclaim(false, released)
    }
    /// Reuse backing for a pool that already has an ID, without evicting an
    /// empty metadata-only owner. Caller supplies the same release proof.
    pub(crate) fn reclaim_backing(&self, released: impl FnMut(u16) -> bool) -> Option<ReclaimedRenderId> {
        self.reclaim(true, released)
    }
    fn reclaim(&self, only_storage: bool, mut released: impl FnMut(u16) -> bool) -> Option<ReclaimedRenderId> {
        let mut state = self.state.lock();
        let RenderIdState { leases, cells } = &mut *state;
        let mut select = |require_storage: bool| leases.reclaim_one(|lease| {
            cells[usize::from(lease.slot - render_ids::FIRST)].as_ref().is_some_and(|cell|
                cell.owner == lease.owner
                    && cell.slot.load(Ordering::Acquire) == lease.slot
                    && cell.generation.load(Ordering::Acquire) == lease.generation
                    && (!require_storage || cell.has_storage()))
                && released(lease.slot)
        });
        let lease = select(true).or_else(|| if only_storage { None } else { select(false) })?;
        let old = state.cells[usize::from(lease.slot - render_ids::FIRST)].take();
        let (backing, spare) = if let Some(cell) = old.as_ref() {
            let backing = cell.storage.backing.lock().take();
            let spare = cell.storage.spare.lock().take();
            cell.storage.page_list.store(0, Ordering::Release);
            cell.slot.store(NO_RENDER_ID, Ordering::Release);
            (backing, spare)
        } else { (None, None) };
        drop(state);
        drop(old);
        static RECLAIMS: AtomicU64 = AtomicU64::new(0);
        if let Some(count) = super::progress::sampled(&RECLAIMS) {
            pr_info!("asahi: idle render pool identity {} returned after firmware release (reclaim {})\n",
                lease.slot, count);
        }
        self.available.notify();
        Some(ReclaimedRenderId { _backing: backing, spare })
    }
}

pub(super) struct RenderIdPin {
    allocator: Arc<RenderIds>,
    cell: Arc<RenderIdCell>,
    lease: render_ids::Lease,
}
impl Drop for RenderIdPin {
    fn drop(&mut self) {
        let available = self.allocator.state.lock().leases.unpin(self.lease).unwrap_or(false);
        if self.cell.closed.load(Ordering::Acquire) { self.allocator.close(&self.cell); }
        if available { self.allocator.available.notify(); }
    }
}

enum RenderIdKind {
    Fixed(u16),
    Rebindable(Arc<RenderIdCell>),
}
pub(super) struct RenderId {
    kind: RenderIdKind,
    allocator: Arc<RenderIds>,
}
impl RenderId {
    pub(super) fn slot(&self) -> u16 {
        match &self.kind {
            RenderIdKind::Fixed(slot) => *slot,
            RenderIdKind::Rebindable(cell) => cell.slot.load(Ordering::Acquire),
        }
    }
    pub(super) fn logical(allocator: &Arc<RenderIds>) -> Result<Self> {
        let owner = NEXT_RENDER_OWNER.fetch_update(Ordering::Relaxed, Ordering::Relaxed,
            |next| next.checked_add(1)).map_err(|_| EOVERFLOW)?;
        let storage = RenderStorage::new(None)?;
        Ok(Self { kind: RenderIdKind::Rebindable(Arc::new(RenderIdCell {
            owner, slot: AtomicU16::new(NO_RENDER_ID), generation: AtomicU64::new(0),
            closed: AtomicBool::new(false),
            storage,
        }, GFP_KERNEL)?), allocator: allocator.clone() })
    }
    fn storage(&self) -> Option<Arc<RenderStorage>> {
        match &self.kind {
            RenderIdKind::Rebindable(cell) => Some(cell.storage.clone()),
            RenderIdKind::Fixed(_) => None,
        }
    }
    fn pin(&self, assigned: impl FnOnce()) -> Result<Option<RenderIdPin>> {
        match &self.kind {
            RenderIdKind::Fixed(_) => Ok(None),
            RenderIdKind::Rebindable(cell) => self.allocator.pin(cell, assigned).map(Some),
        }
    }
    pub(super) fn new(allocator: &Arc<RenderIds>) -> Result<Self> {
        for (bank, used) in allocator.used.iter().enumerate() {
            let mut value = used.load(Ordering::Acquire);
            loop {
                let bit = value.trailing_ones();
                if bit == 64 {
                    break;
                }
                match used.compare_exchange_weak(
                    value,
                    value | (1u64 << bit),
                    Ordering::AcqRel,
                    Ordering::Acquire,
                ) {
                    Ok(_) => {
                        static PEAK: AtomicU32 = AtomicU32::new(0);
                        let used = allocator.used.iter()
                            .map(|bank| bank.load(Ordering::Relaxed).count_ones()).sum::<u32>() - 1;
                        if used > PEAK.fetch_max(used, Ordering::AcqRel) && used % 16 == 0 {
                            pr_info!("asahi: render pool identities in use: peak {}\n", used);
                        }
                        return Ok(Self {
                            kind: RenderIdKind::Fixed(129 + bank as u16 * 64 + bit as u16),
                            allocator: allocator.clone(),
                        })
                    }
                    Err(current) => value = current,
                }
            }
        }
        Err(enospc_site!("freelist.rs:63"))
    }
}
impl Drop for RenderId {
    fn drop(&mut self) {
        match &self.kind {
            RenderIdKind::Fixed(slot) => {
                let bit = *slot as usize - 129;
                self.allocator.used[bit / 64].fetch_and(!(1u64 << (bit % 64)), Ordering::Release);
            }
            RenderIdKind::Rebindable(cell) => self.allocator.close(cell),
        }
    }
}

/// Profile-specific descriptor data for a fresh physical pool. The runtime
/// installs this only before publication, or after an acknowledged release.
pub(crate) enum PoolDescriptor {
    Neo { slot: u16, page_list: u64 },
    M5 { slot: u16, words: [u64; 8] },
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum PageLayout {
    Neo { blocks: usize, runs: RunLayout },
    M5(super::fw::m5::pool::PoolGeometry),
}

pub(crate) struct Pages {
    backing: KernelObject,
    layout: PageLayout,
    render_accounting: Option<RenderAccounting>,
}
static RENDER_LIVE_BYTES: AtomicU64 = AtomicU64::new(0);
static RENDER_PEAK_BYTES: AtomicU64 = AtomicU64::new(0);
struct RenderAccounting(u64);
impl RenderAccounting {
    fn new(bytes: u64) -> Self {
        let live = RENDER_LIVE_BYTES.fetch_add(bytes, Ordering::Relaxed) + bytes;
        RENDER_PEAK_BYTES.fetch_max(live, Ordering::Relaxed);
        Self(bytes)
    }
}
impl Drop for RenderAccounting {
    fn drop(&mut self) { RENDER_LIVE_BYTES.fetch_sub(self.0, Ordering::Relaxed); }
}
impl Pages {
    fn new(alloc: &Allocator<'_>, blocks: usize, runs: RunLayout) -> Result<Self> {
        let mut backing = alloc.lower(
            TABLE_SIZE + blocks * FREE_LIST_BLOCK_SIZE,
            mmu::UAT_PGSZ as u64,
            mmu::PROT_GPU_SHARED_RW,
            CpuMap::WriteCombined,
        )?;
        let base = backing.gpu_va();
        backing.initialize::<PoolTables>(0, |tables| tables.init(base, blocks, runs))?;
        Ok(Self { backing, layout: PageLayout::Neo { blocks, runs }, render_accounting: None })
    }
    /// Fresh zeroed compute backing, built without the device mutex. M5 keeps
    /// its directory, block list and initial 32 MiB in one retained allocation.
    pub(crate) fn compute(alloc: &Allocator<'_>, m5: bool, required: Option<super::fw::m5::pool::PoolGeometry>) -> Result<Self> {
        if !m5 { return Self::new(alloc, COMPUTE_BLOCKS, RunLayout::Compute); }
        use super::fw::m5::pool;
        let geometry = match required { Some(geometry) => geometry,
            None => pool::PoolGeometry::g17s(8).map_err(|_| EINVAL)?, };
        Self::m5(alloc, geometry)
    }
    fn m5(alloc: &Allocator<'_>, geometry: super::fw::m5::pool::PoolGeometry) -> Result<Self> {
        let tables = geometry.directory_bytes.checked_add(geometry.freelist_bytes).ok_or(EOVERFLOW)?;
        let size = geometry.blocks.checked_mul(geometry.block_bytes())
            .and_then(|n| n.checked_add(tables)).ok_or(EOVERFLOW)?;
        let backing = alloc.lower(size, mmu::UAT_PGSZ as u64,
            mmu::PROT_GPU_SHARED_RW, CpuMap::WriteCombined)?;
        let mut pages = Self { backing, layout: PageLayout::M5(geometry), render_accounting: None };
        pages.populate_m5()?;
        Ok(pages)
    }
    fn populate_m5(&mut self) -> Result {
        use super::fw::m5::pool;
        let PageLayout::M5(geometry) = self.layout else { return Err(EINVAL); };
        let tables = geometry.directory_bytes.checked_add(geometry.freelist_bytes).ok_or(EOVERFLOW)?;
        let base = self.backing.gpu_va();
        let mut blocks = KVec::with_capacity(geometry.blocks, GFP_KERNEL)?;
        for index in 0..geometry.blocks {
            blocks.push(base + (tables + index * geometry.block_bytes()) as u64, GFP_KERNEL)?;
        }
        pool::encode_directory(geometry, &blocks,
            &mut self.backing.bytes_mut()[..geometry.directory_bytes]).map_err(|_| EINVAL)?;
        pool::encode_freelist(geometry, &blocks,
            &mut self.backing.bytes_mut()[geometry.directory_bytes..tables]).map_err(|_| EINVAL)?;
        Ok(())
    }
    fn reset_for_client(&mut self) -> Result {
        // Released storage may contain the previous client's private data.
        // Clear it before exposing an alias in a different client VM.
        self.backing.bytes_mut().fill(0);
        self.populate_m5()?;
        fence(Ordering::SeqCst);
        Ok(())
    }
    pub(crate) fn map_client(&self, vm: &mmu::Vm) -> Result<mmu::KernelMapping> {
        self.backing.map_alias(vm, self.page_list_va(), mmu::PROT_GPU_SHARED_RW)
    }
    fn fits(&self, required: Option<super::fw::m5::pool::PoolGeometry>) -> bool {
        match (self.layout, required) {
            (PageLayout::M5(current), Some(required)) =>
                current.block_bytes() == required.block_bytes() && current.blocks >= required.blocks,
            (_, None) => true,
            _ => false,
        }
    }
    fn is_m5(&self) -> bool { matches!(self.layout, PageLayout::M5(_)) }
    fn page_list_va(&self) -> u64 { self.backing.gpu_va() }
    fn run_list_va(&self) -> u64 {
        self.page_list_va() + match self.layout {
            PageLayout::Neo { .. } => PAGE_LIST_SIZE,
            PageLayout::M5(g) => g.directory_bytes,
        } as u64
    }
    fn m5_layout(&self, slot: u16) -> Result<super::fw::m5::pool::PoolLayout> {
        let PageLayout::M5(geometry) = self.layout else { return Err(EINVAL); };
        Ok(super::fw::m5::pool::PoolLayout { geometry, directory: self.page_list_va(),
            freelist: self.run_list_va(), hardware_id: u32::from(slot) })
    }
    fn descriptor(&self, slot: u16) -> Result<PoolDescriptor> {
        if self.is_m5() {
            let words = super::fw::m5::pool::initial_uma_descriptor(self.m5_layout(slot)?)
                .map_err(|_| EINVAL)?;
            Ok(PoolDescriptor::M5 { slot, words })
        } else { Ok(PoolDescriptor::Neo { slot, page_list: self.page_list_va() }) }
    }
    fn repopulate(&self) -> Result {
        let PageLayout::Neo { blocks, runs } = self.layout else { return Err(EINVAL); };
        let table = self.backing.pointer(0, TABLE_SIZE)?.cast::<u64>();
        PoolTables::for_each_word(self.page_list_va(), blocks, runs, |index, value| {
            // SAFETY: Every index is bounded by the checked tables; the caller
            // has observed release before reusing their firmware-owned words.
            unsafe { table.add(index).write_volatile(value) };
        })
    }
}

/// Pool-object identities are independent of recycled firmware IDs and VAs.
static RENDER_POOL_SERIAL: AtomicU64 = AtomicU64::new(0);

/// The USC backing of a render pool and its two aliases. Field order drops
/// both aliases before the backing.
pub(crate) struct RenderBacking {
    _client: mmu::KernelMapping,
    _global: Option<mmu::KernelMapping>,
    pages: Pages,
}

impl RenderBacking {
    fn into_pages(self) -> Pages {
        let Self { _client, _global, pages } = self;
        drop(_client);
        drop(_global);
        pages
    }
    /// Fresh populated backing, built without the device mutex.
    pub(crate) fn new(
        alloc: &Allocator<'_>,
        render_global: &mmu::Vm,
        client: &mmu::Vm,
    ) -> Result<Self> {
        Self::with_layout(alloc, Some(render_global), client,
            PageLayout::Neo { blocks: RENDER_BLOCKS, runs: RunLayout::Render })
    }
    fn with_layout(alloc: &Allocator<'_>, render_global: Option<&mmu::Vm>,
        client: &mmu::Vm, layout: PageLayout) -> Result<Self> {
        let mut pages = match layout {
            PageLayout::Neo { blocks, runs } => Pages::new(alloc, blocks, runs)?,
            PageLayout::M5(geometry) => Pages::m5(alloc, geometry)?,
        };
        pages.render_accounting = Some(RenderAccounting::new(pages.backing.bytes_mut().len() as u64));
        let address = pages.page_list_va();
        let global = render_global.map(|vm|
            pages.backing.map_alias(vm, address, mmu::PROT_GPU_SHARED_RW)).transpose()?;
        let client = pages
            .backing
            .map_alias(client, address, mmu::PROT_GPU_SHARED_RW)?;
        Ok(Self {
            _client: client,
            _global: global,
            pages,
        })
    }
}

#[pin_data]
struct RenderStorage {
    #[pin]
    backing: Mutex<Option<RenderBacking>>,
    #[pin]
    spare: Mutex<Option<Pages>>,
    page_list: AtomicU64,
}
impl RenderStorage {
    fn new(backing: Option<RenderBacking>) -> Result<Arc<Self>> {
        let address = backing.as_ref().map_or(0, |backing| backing.pages.page_list_va());
        Arc::pin_init(try_pin_init!(Self {
            backing <- new_mutex!(backing, "G17 render pool backing"),
            spare <- new_mutex!(None, "G17 render pool spare"),
            page_list: AtomicU64::new(address),
        }), GFP_KERNEL)
    }
}

/// A render pool retained by a logical context and every graph referencing it.
/// Its backing can be given back while the pool's free-list release has been
/// consumed and the client VM is evicted; a fresh backing is installed before
/// the pool is staged again.
pub(crate) struct RenderPool {
    storage: Arc<RenderStorage>,
    /// Counts installed backings, so a re-armed backing is a new free-list
    /// owner even if its addresses repeat.
    epoch: AtomicU64,
    /// Residency of the client VM the backing is aliased into.
    client_residency: Option<Arc<mmu::ResidencyGate>>,
    control: KernelObject,
    state: KernelObject,
    served: AtomicBool,
    serial: u64,
    id: RenderId,
    layout: PageLayout,
}

/// Keeps both a physical identity and its logical pool alive during use.
/// Drop the identity pin before releasing the retained pool reference.
pub(crate) struct PoolPin {
    _identity: RenderIdPin,
    _pool: Arc<RenderPool>,
}

impl RenderPool {
    pub(crate) fn backing_bytes() -> (u64, u64) {
        (RENDER_LIVE_BYTES.load(Ordering::Relaxed), RENDER_PEAK_BYTES.load(Ordering::Relaxed))
    }
    /// The caller retains this guard across construction and all firmware use.
    pub(crate) fn pin_id(self: &Arc<Self>) -> Result<Option<PoolPin>> {
        let pin = self.id.pin(|| {
            self.epoch.fetch_add(1, Ordering::AcqRel);
            self.served.store(true, Ordering::Release);
        })?;
        Ok(pin.map(|pin| PoolPin { _identity: pin, _pool: self.clone() }))
    }

    pub(crate) fn new(
        alloc: &Allocator<'_>,
        ids: &Arc<RenderIds>,
        render_global: &mmu::Vm,
        client: &mmu::Vm,
    ) -> Result<Self> {
        let id = RenderId::new(ids)?;
        let backing = RenderBacking::new(alloc, render_global, client)?;
        Self::with_backing(alloc, id, client, backing)
    }

    /// Explicit initial geometry; allocation policy remains with the caller.
    /// M5 firmware uses the canonical and client mappings of these pages.
    pub(super) fn m5(alloc: &Allocator<'_>, id: RenderId, client: &mmu::Vm,
        geometry: super::fw::m5::pool::PoolGeometry) -> Result<Self> {
        if !alloc.uat.is_sptm() { return Err(EINVAL); }
        // Logical metadata is cheap; physical backing is installed only once
        // preparation owns both an identity pin and admission capacity.
        let storage = id.storage().ok_or(EINVAL)?;
        let state = alloc.kernel(CONTROL_SIZE, mmu::UAT_PGSZ as u64,
            mmu::PROT_GPU_FW_SHARED_RW, CpuMap::WriteCombined)?;
        let control = alloc.kernel(CONTROL_SIZE, mmu::UAT_PGSZ as u64,
            mmu::PROT_GPU_FW_SHARED_RW, CpuMap::WriteCombined)?;
        let serial = RENDER_POOL_SERIAL.fetch_update(Ordering::Relaxed, Ordering::Relaxed,
            |value| value.checked_add(1)).map_err(|_| EOVERFLOW)? + 1;
        Ok(Self { storage, epoch: AtomicU64::new(0), client_residency: client.residency_gate(),
            control, state, served: AtomicBool::new(true), serial, id,
            layout: PageLayout::M5(geometry) })
    }

    fn with_backing(alloc: &Allocator<'_>, id: RenderId, client: &mmu::Vm,
        backing: RenderBacking) -> Result<Self> {
        let layout = backing.pages.layout;
        let address = backing.pages.page_list_va();
        let run_list_va = backing.pages.run_list_va();
        let state = alloc.kernel(
            CONTROL_SIZE,
            mmu::UAT_PGSZ as u64,
            mmu::PROT_GPU_FW_SHARED_RW,
            CpuMap::WriteCombined,
        )?;
        let mut control = alloc.kernel(
            CONTROL_SIZE,
            mmu::UAT_PGSZ as u64,
            mmu::PROT_GPU_FW_SHARED_RW,
            CpuMap::WriteCombined,
        )?;
        if let PageLayout::M5(_) = layout {
            super::fw::m5::pool::encode_g17s_render_state(backing.pages.m5_layout(id.slot())?,
                state.gpu_va(), control.bytes_mut()).map_err(|_| EINVAL)?;
        } else { control.write(
            0,
            FreeListControl::render(
                &FreeListArgs {
                    buffer_slot: u32::from(id.slot()),
                    class: FreeListClass::Two,
                    page_list_va: address,
                    run_list_va,
                    blocks: RENDER_BLOCKS as u32,
                    state_va: state.gpu_va(),
                },
                0,
            ),
        )?; }
        fence(Ordering::SeqCst);
        let serial = RENDER_POOL_SERIAL
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| value.checked_add(1))
            .map_err(|_| EOVERFLOW)? + 1;
        if serial <= 8 || serial.is_power_of_two() {
            let bytes = match layout {
                PageLayout::M5(geometry) => geometry.blocks * geometry.block_bytes(),
                PageLayout::Neo { blocks, .. } => blocks * FREE_LIST_BLOCK_SIZE,
            };
            pr_info!("asahi: render pool allocation {}: {} backing bytes, identity {}\n", serial, bytes, id.slot());
        }
        let storage = RenderStorage::new(Some(backing))?;
        Ok(Self {
            storage,
            epoch: AtomicU64::new(0),
            client_residency: client.residency_gate(),
            control,
            state,
            served: AtomicBool::new(false),
            serial,
            id,
            layout,
        })
    }

    /// No backing is installed: the pool must be re-armed before staging.
    pub(crate) fn vacant(&self) -> bool {
        self.storage.page_list.load(Ordering::Acquire) == 0
    }
    pub(crate) fn has_id(&self) -> bool { self.id() != NO_RENDER_ID }
    pub(crate) fn needs_storage(&self) -> bool {
        self.vacant() && self.storage.spare.lock().is_none()
    }
    /// Pressure reclaim evicted the client VM, which is idle.
    pub(crate) fn client_evicted(&self) -> bool {
        self.client_residency.as_ref().is_some_and(|gate| gate.evicted())
    }
    /// Removes the backing. The caller holds the device mutex and observed this
    /// pool's free-list release consumed, so no firmware reference remains; the
    /// next staging requires a re-armed backing. The caller drops the result
    /// after releasing the device mutex.
    pub(crate) fn vacate(&self) -> Option<RenderBacking> {
        let backing = self.storage.backing.lock().take();
        if backing.is_some() {
            self.storage.page_list.store(0, Ordering::Release);
        }
        backing
    }
    /// Keep released physical storage for the incoming owner, without keeping
    /// the old VM alias or assigning backing to every logical context.
    pub(crate) fn cache_reclaimed(&self, reclaimed: ReclaimedRenderId) {
        let pages = reclaimed.into_pages();
        let backing = self.storage.backing.lock();
        let mut spare = self.storage.spare.lock();
        let mut pages = pages;
        if backing.is_none() && spare.is_none()
            && pages.as_ref().is_some_and(|pages| pages.layout == self.layout) {
            *spare = pages.take();
        }
        drop(spare);
        drop(backing);
        drop(pages);
    }
    /// Re-arms a vacant pool with fresh backing built here, without the device
    /// mutex. `client` is the VM of the context that owns this pool.
    pub(crate) fn ensure_backing(
        &self,
        alloc: &Allocator<'_>,
        render_global: &mmu::Vm,
        client: &mmu::Vm,
    ) -> Result {
        let mut backing = self.storage.backing.lock();
        if backing.is_some() { return Ok(()); }
        let global = if matches!(self.layout, PageLayout::M5(_)) { None } else { Some(render_global) };
        let spare = self.storage.spare.lock().take();
        let fresh = if let Some(mut pages) = spare {
            pages.reset_for_client()?;
            let alias = pages.map_client(client)?;
            RenderBacking { _client: alias, _global: None, pages }
        } else {
            RenderBacking::with_layout(alloc, global, client, self.layout)?
        };
        let address = fresh.pages.page_list_va();
        *backing = Some(fresh);
        self.epoch.fetch_add(1, Ordering::AcqRel);
        self.served.store(true, Ordering::Release);
        self.storage.page_list.store(address, Ordering::Release);
        Ok(())
    }

    pub(crate) fn serial(&self) -> u64 {
        self.serial
    }
    pub(crate) fn epoch(&self) -> u64 {
        self.epoch.load(Ordering::Acquire)
    }
    pub(crate) fn id(&self) -> u16 {
        self.id.slot()
    }
    pub(crate) fn control_va(&self) -> u64 {
        self.control.gpu_va()
    }
    /// Zero while vacant.
    pub(crate) fn page_list_va(&self) -> u64 {
        self.storage.page_list.load(Ordering::Acquire)
    }
    pub(crate) fn run_list_va(&self) -> u64 {
        self.page_list_va() + match self.layout {
            PageLayout::Neo { .. } => PAGE_LIST_SIZE,
            PageLayout::M5(g) => g.directory_bytes,
        } as u64
    }
    pub(crate) fn end_va(&self) -> u64 {
        self.run_list_va() + match self.layout {
            PageLayout::Neo { blocks, .. } => blocks * size_of::<super::fw::initdata::FreeListRunSlot>(),
            PageLayout::M5(g) => g.blocks * 8,
        } as u64
    }

    /// Fresh pools are already populated. Reuse requires the previous release
    /// to have been consumed; command completion alone does not establish it.
    pub(crate) fn repopulate(&self) -> Result {
        if !self.served.swap(true, Ordering::AcqRel) {
            return Ok(());
        }
        let submitted = self.state.dword(0)?.load(Ordering::Relaxed);
        if let PageLayout::M5(_) = self.layout {
            let backing = self.storage.backing.lock();
            let pages = &backing.as_ref().ok_or(EAGAIN)?.pages;
            if pages.page_list_va() != self.page_list_va() { return Err(EIO); }
            let mut bytes = [0; super::fw::m5::pool::STATE_BYTES];
            super::fw::m5::pool::encode_g17s_render_state(pages.m5_layout(self.id())?,
                self.state.gpu_va(), &mut bytes).map_err(|_| EINVAL)?;
            bytes[super::fw::m5::pool::COMPLETED..super::fw::m5::pool::COMPLETED + 8]
                .copy_from_slice(&submitted.to_le_bytes());
            let raw = self.control.pointer(0, bytes.len())?;
            for (index, byte) in bytes.into_iter().enumerate() {
                // SAFETY: The whole retained range was checked and the caller
                // observed final release before installing fresh pool backing.
                unsafe { raw.add(index).write_volatile(byte) };
            }
            fence(Ordering::SeqCst);
            return Ok(());
        }
        let header = FreeListControl::render(
            &FreeListArgs {
                buffer_slot: u32::from(self.id()),
                class: FreeListClass::Two,
                page_list_va: self.page_list_va(),
                run_list_va: self.run_list_va(),
                blocks: RENDER_BLOCKS as u32,
                state_va: self.state.gpu_va(),
            },
            submitted,
        );
        let mut image = KVVec::from_elem(0u64, CONTROL_SIZE / 8, GFP_KERNEL)?;
        // SAFETY: `image` owns a zeroed control page, is large enough for the
        // header, and every header byte is initialized integer data.
        unsafe {
            image
                .as_mut_ptr()
                .cast::<FreeListControl>()
                .write_unaligned(header)
        };
        let control = self.control.pointer(0, CONTROL_SIZE)?.cast::<u64>();
        let backing = self.storage.backing.lock();
        let pages = &backing.as_ref().ok_or(EAGAIN)?.pages;
        if pages.page_list_va() != self.page_list_va() {
            return Err(EIO);
        }
        pages.repopulate()?;
        for (index, value) in image.iter().enumerate() {
            // SAFETY: The entire retained page was checked before any store;
            // the allocation is page-aligned and release excludes the firmware.
            unsafe { control.add(index).write_volatile(*value) };
        }
        fence(Ordering::SeqCst);
        Ok(())
    }

    /// Submitted is host-owned and aligned. Completed is the packed firmware
    /// counter, so retain the byte accesses required by that interface.
    pub(crate) fn counters(&self) -> Result<[u64; 2]> {
        let submitted = self.state.dword(0)?.load(Ordering::Relaxed);
        let offset = if self.is_m5() { super::fw::m5::pool::COMPLETED }
            else { FreeListControl::COMPLETED };
        let completed = self.control.pointer(offset, 8)?;
        let mut bytes = [0; 8];
        for (index, byte) in bytes.iter_mut().enumerate() {
            // SAFETY: The checked eight-byte range stays mapped; byte accesses
            // permit the unaligned counter without widening shared accesses.
            *byte = unsafe { completed.add(index).read_volatile() };
        }
        Ok([submitted, u64::from_le_bytes(bytes)])
    }

    pub(crate) fn fits(&self, geometry: Option<super::fw::m5::pool::PoolGeometry>) -> bool {
        match (self.layout, geometry) {
            (PageLayout::M5(current), Some(required)) =>
                current.block_bytes() == required.block_bytes() && current.blocks >= required.blocks,
            (PageLayout::Neo { .. }, None) => true,
            _ => false,
        }
    }

    pub(crate) fn is_m5(&self) -> bool { matches!(self.layout, PageLayout::M5(_)) }

    fn m5_state(&self) -> Result<super::fw::m5::pool::PoolState> {
        if !self.is_m5() { return Err(EINVAL); }
        let mut state = super::fw::m5::pool::PoolState([0; super::fw::m5::pool::STATE_BYTES]);
        let raw = self.control.pointer(0, state.0.len())?;
        fence(Ordering::Acquire);
        for (index, byte) in state.0.iter_mut().enumerate() {
            // SAFETY: The entire retained header range was checked above.
            *byte = unsafe { raw.add(index).read_volatile() };
        }
        Ok(state)
    }

    pub(crate) fn m5_descriptor(&self) -> Result<[u64; 8]> { Ok(self.m5_state()?.descriptor()) }

    pub(crate) fn store_m5_descriptor(&self, words: [u64; 8]) -> Result {
        let mut state = self.m5_state()?;
        if !state.matches(&words) { return Err(EIO); }
        state.restore(words);
        for offset in super::fw::m5::pool::SAVED_FIELDS {
            let raw = self.control.pointer(offset, 4)?;
            for index in 0..4 {
                // SAFETY: Each mutable field was checked; the caller owns the
                // retired descriptor, and immutable header bytes stay intact.
                unsafe { raw.add(index).write_volatile(state.0[offset + index]) };
            }
        }
        fence(Ordering::SeqCst);
        Ok(())
    }

    pub(super) fn control_object(&self) -> &KernelObject { &self.control }

    /// Snapshot the unaligned shared fields before loading their descriptor.
    pub(crate) fn page_pool_state(&self) -> Result<PagePoolState> {
        let raw = self.control.pointer(0x14, 0x1c)?;
        fence(Ordering::Acquire);
        let read = |offset: usize, count: usize| -> u64 {
            let mut bytes = [0; 8];
            for (index, byte) in bytes.iter_mut().take(count).enumerate() {
                // SAFETY: The complete 0x14..0x30 span was checked. These fixed
                // field ranges lie inside it; byte loads permit unaligned words.
                *byte = unsafe { raw.add(offset + index).read_volatile() };
            }
            u64::from_le_bytes(bytes)
        };
        Ok(PagePoolState {
            page_list_va: read(0, 8),
            unk_1c: read(8, 4) as u32,
            unk_20: read(12, 4) as u32,
            unk_24: read(16, 4) as u32,
            unk_28: read(20, 4) as u32,
            page_count: read(24, 4) as u32,
        })
    }

    /// Save mutable descriptor state only after both engines and free-list
    /// references retire, before the physical slot changes owners.
    pub(crate) fn store_page_pool_state(&self, state: PagePoolState) -> Result {
        let raw = self.control.pointer(0x20, 0x10)?;
        for (word, value) in [state.unk_20, state.unk_24, state.unk_28, state.page_count]
            .into_iter()
            .enumerate()
        {
            for (index, byte) in value.to_le_bytes().into_iter().enumerate() {
                // SAFETY: The entire mutable range was checked, and each
                // enumerated byte lies inside it. Caller owns the retired pool.
                unsafe { raw.add(word * 4 + index).write_volatile(byte) };
            }
        }
        fence(Ordering::SeqCst);
        Ok(())
    }

    /// Prepares the counter before any producer is written. Commit belongs to
    /// the publication transaction; dropping this plan does not reserve work.
    pub(crate) fn prepare_submission(&self, commands: u32) -> Result<Submitted<'_>> {
        if commands > 2 {
            return Err(EINVAL);
        }
        let counter = self.state.dword(0)?;
        Ok(Submitted {
            counter,
            next: counter
                .load(Ordering::Relaxed)
                .wrapping_add(u64::from(commands)),
        })
    }
}

/// One checked, infallible counter update in a submission transaction.
pub(crate) struct Submitted<'a> {
    counter: &'a AtomicU64,
    next: u64,
}
impl Submitted<'_> {
    pub(crate) fn commit(self) {
        self.counter.store(self.next, Ordering::Relaxed);
        fence(Ordering::SeqCst);
    }
}

/// A pool owned by one physical compute queue, with fixed QID-derived slot.
/// Idle retained queues may give their USC backing back (`pages` is `None`)
/// once the firmware consumed the pool's release; the control and state
/// objects the queue record names stay until processor stop.
pub(crate) struct ComputePool {
    pages: Option<Pages>,
    control: KernelObject,
    state: KernelObject,
    id: u16,
    m5: bool,
}
impl ComputePool {
    pub(crate) fn new(alloc: &Allocator<'_>, qid: u8, m5: bool, required: Option<super::fw::m5::pool::PoolGeometry>) -> Result<Self> {
        if qid > super::fw::kick::QID_MAX {
            return Err(EINVAL);
        }
        let id = u16::from(qid) + 1;
        let pages = Pages::compute(alloc, m5, required)?;
        let mut control = alloc.kernel(
            CONTROL_SIZE,
            mmu::UAT_PGSZ as u64,
            mmu::PROT_GPU_FW_SHARED_RW,
            CpuMap::WriteCombined,
        )?;
        let state = alloc.kernel(
            CONTROL_SIZE,
            mmu::UAT_PGSZ as u64,
            mmu::PROT_GPU_FW_SHARED_RW,
            CpuMap::WriteCombined,
        )?;
        Self::initialize_control(&mut control, &pages, id, state.gpu_va(), [0; 8])?;
        fence(Ordering::SeqCst);
        Ok(Self { pages: Some(pages), control, state, id, m5 })
    }
    fn initialize_control(control: &mut KernelObject, pages: &Pages, id: u16,
        submitted: u64, completed: [u8; 8]) -> Result {
        if pages.is_m5() {
            use super::fw::m5::pool;
            pool::encode_g17s_compute_state(pages.m5_layout(id)?, submitted,
                &mut control.bytes_mut()[..pool::STATE_BYTES]).map_err(|_| EINVAL)?;
            control.bytes_mut()[pool::COMPLETED..pool::COMPLETED + 8].copy_from_slice(&completed);
            Ok(())
        } else {
            let mut header = FreeListControl::compute(&FreeListArgs {
                buffer_slot: u32::from(id), class: FreeListClass::One,
                page_list_va: pages.page_list_va(), run_list_va: pages.run_list_va(),
                blocks: COMPUTE_BLOCKS as u32, state_va: submitted,
            });
            header.unk_54[..8].copy_from_slice(&completed);
            control.write(0, header)
        }
    }
    pub(crate) fn descriptor(&self) -> Result<PoolDescriptor> {
        self.pages.as_ref().ok_or(EAGAIN)?.descriptor(self.id)
    }
    pub(crate) fn completed_submissions(&self) -> Result<u64> {
        Ok(u64::from_le_bytes(self.completed_bytes()?))
    }
    fn completed_bytes(&self) -> Result<[u8; 8]> {
        let offset = if self.m5 { super::fw::m5::pool::COMPLETED } else { FreeListControl::COMPLETED };
        let pointer = self.control.pointer(offset, 8)?;
        let mut bytes = [0; 8];
        for (index, byte) in bytes.iter_mut().enumerate() {
            // SAFETY: The checked packed counter remains mapped. Firmware may
            // update it; callers use it only with the queue's completion proof.
            *byte = unsafe { pointer.add(index).read_volatile() };
        }
        Ok(bytes)
    }
    pub(crate) fn id(&self) -> u16 {
        self.id
    }
    /// EAGAIN while the pool is vacant: the owner must re-arm it first.
    pub(crate) fn page_list_va(&self) -> Result<u64> {
        self.pages.as_ref().map(Pages::page_list_va).ok_or(EAGAIN)
    }
    pub(crate) fn control_va(&self) -> u64 {
        self.control.gpu_va()
    }
    pub(crate) fn fits(&self, required: Option<super::fw::m5::pool::PoolGeometry>) -> bool {
        self.pages.as_ref().is_some_and(|pages| pages.fits(required))
    }
    pub(crate) fn populated(&self) -> bool {
        self.pages.is_some()
    }
    /// The release record for this pool's buffer slot under the free-list
    /// generation the queue last published.
    pub(crate) fn release_record(&self, generation: u64) -> super::fw::channels::FreeListRelease {
        super::fw::channels::FreeListRelease::new(generation, u32::from(self.id))
    }
    /// The caller has observed the firmware consume this pool's release and
    /// frees the returned backing after releasing the device mutex.
    pub(crate) fn take_pages(&mut self) -> Option<Pages> {
        self.pages.take()
    }
    /// Rewrites the shared control header for fresh backing. The caller then
    /// rewrites the page-pool descriptor row before the queue is kicked again.
    /// As render repopulation does, the firmware-completed counter is carried
    /// over so it stays consistent with the host submitted counter in `state`.
    pub(crate) fn rearm(&mut self, pages: Pages) -> Result {
        if self.pages.is_some() {
            return Err(EBUSY);
        }
        if pages.is_m5() != self.m5 { return Err(EINVAL); }
        let completed = self.completed_bytes()?;
        Self::initialize_control(&mut self.control, &pages, self.id, self.state.gpu_va(), completed)?;
        fence(Ordering::SeqCst);
        self.pages = Some(pages);
        Ok(())
    }
    pub(crate) fn map_client(&mut self, vm: &mmu::Vm) -> Result<mmu::KernelMapping> {
        let pages = self.pages.as_mut().ok_or(EAGAIN)?;
        let address = pages.page_list_va();
        pages
            .backing
            .map_alias(vm, address, mmu::PROT_GPU_SHARED_RW)
    }
    pub(crate) fn reserve_submission(&self) -> Result<u64> {
        let counter = self.state.dword(0)?;
        let next = counter
            .load(Ordering::Relaxed)
            .checked_add(1)
            .ok_or(EOVERFLOW)?;
        counter.store(next, Ordering::Relaxed);
        fence(Ordering::SeqCst);
        Ok(next)
    }
    /// The caller has proved no pointer from this submission was published.
    pub(crate) fn cancel_unpublished(&self, target: u64) -> Result {
        let counter = self.state.dword(0)?;
        if target == 0 || counter.load(Ordering::Acquire) != target {
            return Err(EINVAL);
        }
        counter.store(target - 1, Ordering::Relaxed);
        fence(Ordering::SeqCst);
        Ok(())
    }
}

/// Registration identity held by a free-list lease. The cookie is the end of
/// the run list; it remains stable while overlapping command pairs share it.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) struct Owner {
    /// Identifies the host pool object even when every firmware address recycles.
    pub(crate) serial: u64,
    /// Identifies the pool's installed backing (render pools are re-armed).
    pub(crate) epoch: u64,
    pub(crate) buffer_slot: u32,
    pub(crate) control_va: u64,
    pub(crate) ring_va: u64,
    pub(crate) cookie: u64,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum LeasePhase {
    Preparing,
    Ready,
    ReleaseSent(u32),
    Released,
}

/// References to one prepopulated owner: preparation and two engine commands
/// per render. Published releases do not gate reacquisition; their consumption
/// witnesses are carried into the successor.
#[derive(Copy, Clone, Debug)]
pub(crate) struct Lease {
    owner: Owner,
    generation: u64,
    references: u32,
    commands: u32,
    phase: LeasePhase,
    previous_release: Option<u32>,
}

impl Lease {
    /// Acquires an unchanged overlapping owner, or an owner whose previous
    /// release was sent. A changed owner additionally requires caller-side
    /// proof of old-pool release before its backing can be repopulated.
    pub(crate) fn acquire(previous: Option<&Self>, owner: Owner) -> Result<Self> {
        let Some(previous) = previous else {
            return Ok(Self {
                owner,
                generation: 0,
                references: 3,
                commands: 2,
                phase: LeasePhase::Preparing,
                previous_release: None,
            });
        };
        if previous.owner.buffer_slot != owner.buffer_slot {
            return Err(EINVAL);
        }
        let generation = previous.generation.checked_add(1).ok_or(EOVERFLOW)?;
        let same_owner = previous.owner == owner;
        let overlapping = same_owner
            && previous.phase == LeasePhase::Ready
            && previous.references >= 2
            && previous.commands >= 2;
        let reacquire = previous.reacquirable();
        if !overlapping && !reacquire {
            return Err(EBUSY);
        }
        let references = previous.references.checked_add(3).ok_or(EOVERFLOW)?;
        let commands = previous.commands.checked_add(2).ok_or(EOVERFLOW)?;
        Ok(Self {
            owner,
            generation: if overlapping {
                previous.generation
            } else {
                generation
            },
            references,
            commands,
            phase: LeasePhase::Preparing,
            previous_release: previous.release_witness(),
        })
    }

    pub(crate) fn owner(&self) -> Owner {
        self.owner
    }
    pub(crate) fn generation(&self) -> u64 {
        self.generation
    }
    pub(crate) fn reacquirable(&self) -> bool {
        matches!(
            self.phase,
            LeasePhase::ReleaseSent(_) | LeasePhase::Released
        ) && self.references == 0
            && self.commands == 0
    }
    pub(crate) fn released(&self) -> bool {
        self.phase == LeasePhase::Released && self.previous_release.is_none()
    }
    pub(crate) fn release_witness(&self) -> Option<u32> {
        match self.phase {
            LeasePhase::ReleaseSent(cursor) => Some(cursor),
            _ => self.previous_release,
        }
    }

    /// Publication retires preparation independently of the command references.
    pub(crate) fn retire_prepare(&mut self) -> Result {
        if self.phase != LeasePhase::Preparing || self.references < 3 {
            return Err(EINVAL);
        }
        self.references -= 1;
        self.phase = LeasePhase::Ready;
        Ok(())
    }
    /// Fixed prepopulated pools never submit an asynchronous growth request.
    /// An incoming growth completion therefore cannot name a live request.
    pub(crate) fn complete_grow(
        &mut self,
        _event: super::fw::channels::FreeListCompletion,
    ) -> Result {
        Err(EIO)
    }
    /// Both engines have retired this render's exact retained submission.
    pub(crate) fn complete_render(
        &mut self,
    ) -> Result<Option<super::fw::channels::FreeListRelease>> {
        if self.phase != LeasePhase::Ready || self.references < 2 || self.commands < 2 {
            return Err(EINVAL);
        }
        self.references -= 2;
        self.commands -= 2;
        Ok(self.pending_release())
    }
    pub(crate) fn pending_release(&self) -> Option<super::fw::channels::FreeListRelease> {
        (self.phase == LeasePhase::Ready && self.references == 0).then(|| {
            super::fw::channels::FreeListRelease::new(self.generation, self.owner.buffer_slot)
        })
    }
    /// Records the producer-after cursor once the final release was published.
    pub(crate) fn mark_release_sent(&mut self, cursor: u32) -> Result {
        if self.pending_release().is_none() || cursor >= super::fw::channels::DEVICE_CONTROL_SLOTS {
            return Err(EINVAL);
        }
        self.phase = LeasePhase::ReleaseSent(cursor);
        Ok(())
    }
    /// Observes all older release records passed by one consumer snapshot.
    pub(crate) fn observe_releases(&mut self, consumer: u32, producer: u32) -> Result {
        if let Some(cursor) = self.previous_release {
            if control_consumed(consumer, producer, cursor)? {
                self.previous_release = None;
            }
        }
        if let LeasePhase::ReleaseSent(cursor) = self.phase {
            if control_consumed(consumer, producer, cursor)? {
                self.phase = LeasePhase::Released;
            }
        }
        Ok(())
    }
}

/// A publication is retired once its producer-after cursor leaves the
/// current `(consumer, producer]` interval, including wrapped intervals.
pub(crate) fn control_consumed(consumer: u32, producer: u32, cursor: u32) -> Result<bool> {
    let slots = super::fw::channels::DEVICE_CONTROL_SLOTS;
    if consumer >= slots || producer >= slots || cursor >= slots {
        return Err(EIO);
    }
    let pending = producer.wrapping_sub(consumer) & (slots - 1);
    let ahead = cursor.wrapping_sub(consumer) & (slots - 1);
    Ok(ahead == 0 || ahead > pending)
}
