// SPDX-License-Identifier: GPL-2.0-only OR MIT

//! Device ownership of retained render pairs and their shared free-list leases.
//! The device mutex protects identities and publication. A preparation gate
//! lease permits graph construction outside that mutex without session teardown.

mod exit;
mod dependencies;
mod grow;
mod observe;
mod pools;
mod physical;
mod prepare;
mod recovery;
mod replay;
mod settle;
mod submit;
pub(super) mod teardown;

use super::{DeferredBatch, Registry};
use crate::g17::{context::Context, job::Packet, kick, queue::render::Pair};
use crate::hw::t8140::queues::RENDER_SLOTS;
use kernel::{prelude::*, sync::Arc};

/// A lent pair still has firmware work outstanding: the publisher waits.
pub(super) const LEND_PENDING: Error = EJUKEBOX;
/// vmfix6e poison diagnostic: collected graphs kept out of reuse.
const POISONED_GRAPHS: usize = 64;
/// vmfix6d D: no buffer identity, metrics slot or retired-binding room is free
/// to build a graph. Capacity returns as work retires or owners close, so the
/// unpublished packet waits (progress-bounded) instead of failing its client.
/// Kernel-internal: render_with_lease never returns it.
pub(super) const RENDER_CAPACITY_PENDING: Error = ENOGRACE;


/// vmfix6d A: where a render publication was last refused with EBUSY.
#[derive(Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub(super) enum Site {
    None = 0,
    /// Entry has a pending error or timeout for the event worker.
    BuildPending,
    /// Entry is constructing, preparing or growing.
    BuildBusy,
    /// reserve_render() refused the slot.
    BuildReserve,
    /// The installed pair is not bindable for this owner.
    BuildBindable,
    /// Off-lock binding preparation (Pair::prepare_bind) failed.
    RebindPrepare,
    /// Device-locked Pair::bind_owner / install_memory failed.
    RebindBind,
    FreshInstall,
    ParkedInstall,
    BuildFinish,
    /// The slot has no installed pair at preparation.
    PrepareNoPair,
    /// take_pair(): closed, other reservation owner, preparing or pending.
    PrepareTake,
    /// host_growth_target(): the pair is not idle for its context.
    PrepareGrowth,
    /// Pending error or timeout after preparation.
    PreparePending,
    /// Pair::prepare(): no room or context mismatch.
    PreparePair,
    /// publish_render(): outer queue full or pair publication refused.
    Publish,
    /// vmfix6d restore of a retained binding is not possible yet.
    Restore,
    /// Capacity: no free parameter-buffer identity (127).
    CapacityBuffer,
    /// Capacity: no free performance-metrics slot.
    CapacityMetrics,
    /// Capacity: the retired-binding list is full.
    CapacityRetired,
    /// Capacity: graph construction refused for want of QIDs/identities.
    CapacityReserve,
    /// Capacity: no free render pool (USC free-list) identity (127); pools of
    /// closed queues return theirs after their teardown completes.
    CapacityRenderPool,
}
impl Site {
    pub(super) fn name(value: u32) -> &'static str {
        const NAMES: [&str; 22] = [
            "none", "build-pending", "build-busy", "build-reserve", "build-bindable",
            "rebind-prepare", "rebind-bind", "fresh-install", "parked-install", "build-finish",
            "prepare-no-pair", "prepare-take", "prepare-growth", "prepare-pending",
            "prepare-pair", "publish", "restore", "capacity-buffer-id", "capacity-metrics",
            "capacity-retired-list", "capacity-reserve", "capacity-render-pool",
        ];
        NAMES.get(value as usize).copied().unwrap_or("?")
    }
}

impl super::Backend {
    /// Records `site` and returns EBUSY.
    pub(super) fn busy(&self, site: Site) -> Error {
        self.busy_site.store(site as u32, core::sync::atomic::Ordering::Relaxed);
        EBUSY
    }
    /// vmfix6d: an ENOSPC during graph construction becomes a capacity wait
    /// that names what ran out.
    pub(super) fn capacity(&self, site: Site, error: Error) -> Error {
        if error == ENOSPC {
            self.busy_site.store(site as u32, core::sync::atomic::Ordering::Relaxed);
            return RENDER_CAPACITY_PENDING;
        }
        error
    }
    /// Records `site` when `error` is EBUSY; returns `error` unchanged.
    pub(super) fn tag(&self, site: Site, error: Error) -> Error {
        if error == EBUSY {
            self.busy_site.store(site as u32, core::sync::atomic::Ordering::Relaxed);
        }
        error
    }
}

#[derive(Copy, Clone)]
struct Config {
    clusters: u32,
    descriptor_flags: [u32; 2],
}

#[derive(Copy, Clone)]
struct Reservation {
    slot: u8,
    ids: [kick::Id; 2],
    owner: u64,
    buffer: (u8, u64),
}

struct Entry {
    reservation: Reservation,
    pair: Option<KBox<Pair>>,
    parked: Option<crate::g17::queue::render::parked::ParkedPair>,
    preparing: Option<Arc<Packet>>,
    context: Option<Arc<Context>>,
    pool: Option<Arc<crate::g17::freelist::RenderPool>>,
    pending_owner: Option<u64>,
    pending_owner_closed: bool,
    borrowed_active: bool,
    borrowed_dependencies: Option<crate::g17::queue::render::Dependencies>,
    event_masks: [u64; 2],
    closed: bool,
    /// The pair was taken from a live, idle owner (admission lending): its
    /// context is displaced as Deferred until that owner closes.
    lent_live: bool,
    pending_error: Option<(Arc<crate::g17::status::VmStatus>, Error)>,
    pending_timeout: bool,
    constructing: bool,
    growing: bool,
    published: bool,
}

pub(super) struct RenderState {
    config: Config,
    entries: KVec<Option<Entry>>,
    pools: pools::Pools,
    grow: grow::Grow,
    retired: KVec<crate::g17::queue::render::RetiredBinding>,
    /// vmfix6e poison diagnostic: eagerly collected graphs kept out of reuse
    /// (oldest freed first once full).
    poisoned: KVec<crate::g17::queue::render::RetiredBinding>,
    next_pm_generation: u64,
}

impl RenderState {
    pub(super) fn new(clusters: u32, descriptor_flags: [u32; 2]) -> Result<Self> {
        if clusters == 0 {
            return Err(EINVAL);
        }
        let mut entries = KVec::with_capacity(RENDER_SLOTS, GFP_KERNEL)?;
        for _ in 0..RENDER_SLOTS {
            entries.push(None, GFP_KERNEL)?;
        }
        Ok(Self {
            config: Config {
                clusters,
                descriptor_flags,
            },
            entries,
            pools: pools::Pools::new()?,
            grow: grow::Grow::new()?,
            retired: KVec::with_capacity(128, GFP_KERNEL)?,
            poisoned: KVec::with_capacity(POISONED_GRAPHS, GFP_KERNEL)?,
            next_pm_generation: 1,
        })
    }

    /// Plain host metadata under the existing device mutex; no pair/fence callbacks.
    pub(super) fn timeout_snapshot(&self, out: &mut crate::g17::timeout_trace::Snapshot) {
        for (slot, entry) in self.entries.iter().enumerate() {
            let Some(entry) = entry.as_ref() else { continue; };
            let i = out.render_count as usize;
            if i == out.renders.len() { out.reason = crate::g17::timeout_trace::OVERFLOW; break; }
            let flags = u64::from(entry.closed) | (u64::from(entry.constructing) << 1)
                | (u64::from(entry.growing) << 2) | (u64::from(entry.published) << 3)
                | (u64::from(entry.pending_timeout) << 4) | (u64::from(entry.borrowed_active) << 5)
                | (u64::from(entry.pending_owner_closed) << 6) | (u64::from(entry.lent_live) << 7);
            out.renders[i] = [slot as u64, entry.reservation.owner,
                u64::from(entry.reservation.ids[0].qid()), u64::from(entry.reservation.ids[1].qid()),
                flags, u64::from(entry.pair.is_some()), u64::from(entry.parked.is_some()),
                entry.preparing.as_ref().map_or(0, |p| Arc::as_ptr(p) as usize as u64),
                entry.pending_owner.unwrap_or(0), u64::from(entry.pending_owner.is_some()),
                u64::from(entry.pending_error.is_some()), entry.context.as_ref().map_or(0, |c| u64::from(c.id()))];
            out.render_count += 1;
        }
    }

    fn reserve_pm_generation(&mut self) -> Result<u64> {
        let generation = self.next_pm_generation;
        self.next_pm_generation = generation.checked_add(1).ok_or(EOVERFLOW)?;
        Ok(generation)
    }

    fn collect_retired(&mut self, slot: usize) -> Result {
        loop {
            if self.retired.len() == self.retired.capacity() {
                return Ok(());
            }
            let retired = self.entries[slot]
                .as_mut()
                .and_then(|entry| entry.pair.as_mut())
                .and_then(|pair| pair.take_retired_binding());
            let Some(retired) = retired else {
                return Ok(());
            };
            self.retired
                .push_within_capacity(retired)
                .map_err(|_| EIO)?;
        }
    }

    fn entry(&self, slot: u8) -> Result<&Entry> {
        self.entries
            .get(usize::from(slot))
            .and_then(Option::as_ref)
            .ok_or(EIO)
    }

    fn entry_mut(&mut self, slot: u8) -> Result<&mut Entry> {
        self.entries
            .get_mut(usize::from(slot))
            .and_then(Option::as_mut)
            .ok_or(EIO)
    }

    fn reserved(&mut self, reservation: Reservation) -> Result<&mut Entry> {
        let entry = self.entry_mut(reservation.slot)?;
        if entry.reservation.ids != reservation.ids
            || entry.reservation.owner != reservation.owner
            || entry.reservation.buffer != reservation.buffer
            || entry.pair.is_some()
            || entry.preparing.is_some()
            || entry.context.is_some()
            || !entry.constructing
        {
            return Err(EIO);
        }
        Ok(entry)
    }

    /// Keep metadata while graph preparation owns the pair outside the mutex.
    /// Completion scans skip it; cancellation is recorded and applied on return.
    fn take_pair(&mut self, slot: u8, owner: u64, packet: &Arc<Packet>) -> Result<KBox<Pair>> {
        let entry = self.entry_mut(slot)?;
        if entry.closed
            || entry.reservation.owner != owner
            || entry.preparing.is_some()
            || entry.pending_error.is_some()
            || entry.pending_timeout
        {
            return Err(EBUSY);
        }
        let pair = entry.pair.as_ref().ok_or(EIO)?;
        if !pair.matches(owner, &packet.context)? {
            return Err(EIO);
        }
        entry.borrowed_active = pair.published_in_flight();
        entry.borrowed_dependencies = Some(pair.dependency_snapshot());
        entry.preparing = Some(packet.clone());
        entry.pair.take().ok_or(EIO)
    }

    /// Validate before taking the owner from the caller. Even rejected returns
    /// keep their backing alive until the caller resolves the reservation.
    fn return_pair(&mut self, slot: u8, pair: &mut Option<KBox<Pair>>) -> Result {
        let returning = pair.as_ref().ok_or(EINVAL)?;
        let entry = self.entry_mut(slot)?;
        if entry.pair.is_some()
            || entry.preparing.is_none()
            || returning.slot() != slot
            || returning.qids() != entry.reservation.ids.map(kick::Id::qid)
            || returning.owner() != Some(entry.reservation.owner)
        {
            return Err(EIO);
        }
        entry.pair = pair.take();
        entry.borrowed_dependencies = None;
        entry.preparing = None;
        Ok(())
    }

    fn save_detaching_pool(&self, init: &crate::g17::initdata::InitData, slot: u8) -> Result {
        let pool = self.entry(slot)?.pool.as_ref().ok_or(EIO)?;
        let shared = self.entries.iter().enumerate().any(|(index, entry)| {
            index != usize::from(slot)
                && entry.as_ref().is_some_and(|entry| {
                    entry.pool.as_ref().is_some_and(|other| {
                        other.id() == pool.id() && other.control_va() == pool.control_va()
                    })
                })
        });
        if !shared {
            self.pools.save(init, pool)?;
        }
        Ok(())
    }
}

impl Registry {
    /// vmfix6d A: buffer identities [free, graph-owned, referenced, refs] and
    /// render graphs [installed pairs, parked, retired, retired with a buffer
    /// identity, retired of live contexts].
    pub(in crate::g17) fn render_census(&self) -> ([u32; 4], [u32; 5]) {
        let mut graphs = [0u32; 5];
        for entry in self.render.entries.iter().flatten() {
            graphs[0] += u32::from(entry.pair.is_some());
            graphs[1] += u32::from(entry.parked.is_some());
        }
        for old in self.render.retired.iter() {
            graphs[2] += 1;
            graphs[3] += u32::from(old.buffer_id().is_some());
            graphs[4] += u32::from(old.context().is_current());
        }
        (self.buffers.census(), graphs)
    }

    /// vmfix6d A: the census at most every 5 s while pairs are lent, so a
    /// lending test shows whether retained identities stay bounded.
    #[inline(never)]
    #[cold]
    fn log_render_census(&self, why: &str) {
        static LAST: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);
        let now = super::now_ns();
        let last = LAST.load(core::sync::atomic::Ordering::Relaxed);
        if last != 0 && now.saturating_sub(last) < 5_000_000_000 {
            return;
        }
        LAST.store(now.max(1), core::sync::atomic::Ordering::Relaxed);
        let (ids, graphs) = self.render_census();
        let (backing, peak) = crate::g17::freelist::RenderPool::backing_bytes();
        pr_info!("asahi: render census ({}): ids free={} owned={} referenced={} refs={} graphs installed={} parked={} retired={} retired_pb={} retired_live={} backing_bytes={} peak_backing_bytes={}\n",
            why, ids[0], ids[1], ids[2], ids[3], graphs[0], graphs[1], graphs[2], graphs[3], graphs[4], backing, peak);
    }

    fn ensure_render_ids(&mut self, slot: u8, owner: u64) -> Result {
        if owner == 0 || slot == 0 || usize::from(slot) >= RENDER_SLOTS {
            return Err(EINVAL);
        }
        if self.render.entries[usize::from(slot)].is_none() {
            let identity = self.identities.reserve(2)?.start;
            let [tiling, fragment] = self.qids.reserve_render_pair(identity)?;
            self.admission.mark_built(slot);
            self.render.entries[usize::from(slot)] = Some(Entry {
                reservation: Reservation {
                    slot,
                    ids: [tiling, fragment],
                    owner,
                    buffer: (0, 0),
                },
                pair: None,
                parked: None,
                preparing: None,
                context: None,
                pool: None,
                pending_owner: None,
                pending_owner_closed: false,
                borrowed_active: false,
                borrowed_dependencies: None,
                event_masks: [0; 2],
                closed: false,
                lent_live: false,
                pending_error: None,
                pending_timeout: false,
                constructing: false,
                growing: false,
                published: false,
            });
        }
        Ok(())
    }

    /// Spare rings of recycled QIDs for a fresh pair's two identities (vmfix6).
    pub(super) fn take_render_channels(
        &mut self,
        ids: [kick::Id; 2],
    ) -> Result<Option<KBox<[Option<kick::Queue>; 2]>>> {
        if !ids.iter().any(|id| self.qids.has_spare(id.qid())) {
            return Ok(None);
        }
        let mut channels = KBox::new([None, None], GFP_KERNEL)?;
        for (lane, id) in ids.into_iter().enumerate() {
            channels[lane] = self.qids.take_channel(id);
        }
        Ok(Some(channels))
    }

    pub(super) fn render_channels_installed(&mut self, ids: [kick::Id; 2]) {
        for id in ids {
            self.adopted_installed(id);
        }
    }

    /// After a failed fresh build: unused spare rings return to their QIDs; a
    /// ring adopted and then dropped with the pair poisons its QID, and the
    /// slot, which keeps that QID, can no longer be built.
    #[inline(never)]
    pub(super) fn settle_render_channels(
        &mut self,
        slot: u8,
        ids: [kick::Id; 2],
        channels: Option<KBox<[Option<kick::Queue>; 2]>>,
    ) {
        if let Some(mut channels) = channels {
            for lane in 0..2 {
                if let Some(queue) = channels[lane].take() {
                    self.qids.return_channel(queue);
                }
            }
        }
        let mut poisoned = false;
        for id in ids {
            if self.qids.adopted(id) {
                self.qids.poison(id);
                poisoned = true;
            }
        }
        if poisoned {
            self.admission.mark_unbuildable(slot);
        }
    }

    /// Admission lent `slot` to `owner` from a live owner that holds no lease
    /// on it. Release the pair from that owner only once the firmware has
    /// consumed all of its work (cursor witness); until then the publisher
    /// waits (LEND_PENDING). Its context is displaced as Deferred at rebind.
    #[inline(never)]
    pub(super) fn lend_live_pair(&mut self, slot: u8, owner: u64) -> Result {
        if self.admission.slot_owner(slot) != owner {
            return Ok(());
        }
        let entry = self.render.entry_mut(slot)?;
        if entry.closed || entry.constructing || entry.preparing.is_some() || entry.growing {
            return Ok(());
        }
        let Some(pair) = entry.pair.as_mut() else {
            return Ok(());
        };
        let Some(previous) = pair.owner() else {
            return Ok(());
        };
        if previous == owner {
            return Ok(());
        }
        if !pair.lendable()? {
            return Err(LEND_PENDING);
        }
        pair.release_owner();
        entry.lent_live = true;
        static LENDS: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);
        let n = LENDS.fetch_add(1, core::sync::atomic::Ordering::Relaxed) + 1;
        if n <= 8 || n.is_power_of_two() {
            pr_info!("asahi: render pair slot {} lent from idle owner {} to {} (lend {})\n",
                slot, previous, owner, n);
        }
        self.log_render_census("lend");
        Ok(())
    }

    fn reserve_render(&mut self, slot: u8, owner: u64) -> Result<Reservation> {
        self.ensure_render_ids(slot, owner)?;
        let entry = self.render.entry(slot)?;
        if entry.constructing
            || entry.pair.is_some()
            || entry.parked.is_some()
            || entry.preparing.is_some()
            || entry.growing
        {
            return Err(EBUSY);
        }
        let ids = self.render.entry(slot)?.reservation.ids;
        // Physical queue identities survive failed graph construction. Only
        // failure to acquire the second identity can return the first one.
        for id in ids {
            self.qids.publish(id)?;
        }
        let buffer = self.buffers.allocate_render()?;
        let entry = self.render.entry_mut(slot)?;
        entry.reservation.owner = owner;
        entry.reservation.buffer = buffer;
        entry.constructing = true;
        entry.closed = false;
        Ok(entry.reservation)
    }

    fn install_render(
        &mut self,
        reservation: Reservation,
        pair: &mut Option<KBox<Pair>>,
    ) -> Result {
        let built = pair.as_ref().ok_or(EINVAL)?;
        if built.slot() != reservation.slot
            || built.qids() != reservation.ids.map(kick::Id::qid)
            || built.owner() != Some(reservation.owner)
            || built.buffer_id() != reservation.buffer.0
        {
            return Err(EIO);
        }
        if self.render.reserved(reservation)?.closed {
            return Err(ECANCELED);
        }
        // Construction holds one PB reference; retain the owner independently
        // and return that reference before the first command acquires its own.
        self.buffers.reserve_owner(reservation.buffer.0)?;
        self.buffers.release(reservation.buffer.0)?;
        let entry = self.render.reserved(reservation)?;
        entry.context = Some(built.context().clone());
        entry.pool = Some(built.pool().clone());
        entry.pair = pair.take();
        entry.constructing = false;
        Ok(())
    }

    /// Drop the rejected graph outside the device mutex before returning its
    /// temporary PB reference. The physical queue identities remain reserved.
    fn cancel_render_reservation(&mut self, reservation: Reservation) -> Result {
        self.render.reserved(reservation)?;
        self.buffers.release(reservation.buffer.0)?;
        self.render.entry_mut(reservation.slot)?.constructing = false;
        Ok(())
    }

    pub(super) fn fail_render_vm(
        &mut self,
        status: &Arc<crate::g17::status::VmStatus>,
        error: Error,
        deferred: &mut DeferredBatch,
    ) -> Result {
        for entry in self.render.entries.iter_mut().flatten() {
            if !entry
                .context
                .as_ref()
                .is_some_and(|context| Arc::ptr_eq(context.status(), status))
            {
                continue;
            }
            if let Some(pair) = entry.pair.as_mut() {
                pair.quarantine(error, &mut |event| deferred.push(event))?;
            } else if entry.preparing.is_some() || entry.growing {
                entry.pending_error = Some((status.clone(), error));
            }
        }
        Ok(())
    }
}

struct Host<'a> {
    firmware: &'a mut crate::g17::Firmware,
    registration_powered: bool,
    m5_registration: Option<crate::g17::fw::m5::queue::HardwareSubmission>,
}
impl Host<'_> {
    fn release_registration(&mut self) {
        if !self.registration_powered && !self.firmware.registration_power_release_pending {
            return;
        }
        if let Err(error) = self.firmware.release_registration_power() {
            dev_warn!(
                self.firmware.primary.state.shared.dev.as_ref(),
                "Could not restore render idle policy: {:?}\n",
                error
            );
        } else {
            self.registration_powered = false;
        }
    }
}
impl crate::g17::queue::render::Host for Host<'_> {
    fn generation(&self) -> u64 {
        self.firmware.recovery.generation()
    }
    fn epoch(&self) -> Result<(u64, u32)> {
        Ok((
            self.firmware.recovery.generation(),
            crate::g17::recovery::Memory::recovery_state(&self.firmware.init)?,
        ))
    }
    fn stamps(&self, qids: [u8; 2]) -> Result<[u32; 2]> {
        use core::sync::atomic::Ordering;
        let stamps = [
            self.firmware.init.stamp(qids[0])?.load(Ordering::Relaxed),
            self.firmware.init.stamp(qids[1])?.load(Ordering::Relaxed),
        ];
        core::sync::atomic::fence(Ordering::Acquire);
        Ok(stamps)
    }
    fn qos_publish(
        &mut self,
        owner: crate::g17::qos::Owner,
        scheduler: u64,
        policy: crate::g17::fw::queue::Policy,
    ) -> Result<crate::g17::qos::Publication> {
        use crate::g17::qos;
        self.firmware.queues.accounting.publish(
            &qos::View::new(self.firmware.init.qos()?)?,
            owner,
            scheduler,
            policy,
            qos::clock,
        )
    }
    fn qos_cancel(&mut self, publication: crate::g17::qos::Publication) -> Result {
        use crate::g17::qos;
        self.firmware
            .queues
            .accounting
            .cancel(&qos::View::new(self.firmware.init.qos()?)?, publication)
    }
    /// Neo's direct configuration writes require a core-power hold. M5's
    /// host command port atomically installs and kicks the pair while idle;
    /// its registration is retained until outer publication without waiting
    /// for the Neo core-power witness.
    fn install_pair(&mut self, registration: &crate::g17::queue::render::Registration) -> Result {
        let registration = match registration {
            crate::g17::queue::render::Registration::M5(submission) => {
                self.m5_registration = Some(*submission);
                return Ok(());
            }
            crate::g17::queue::render::Registration::Neo(registration) => registration,
        };
        if !self.registration_powered {
            self.firmware.acquire_registration_power()?;
            self.registration_powered = true;
        }
        for _ in 0..3 {
            if self
                .firmware
                .bridge
                .regs
                .install_render_pair(registration)?
            {
                return Ok(());
            }
        }
        Err(EAGAIN)
    }
    fn publish_qid(&mut self, id: kick::Id) -> Result {
        self.firmware.queues.qids.publish(id)
    }
    fn publish_outer_pair(
        &mut self,
        priority: u8,
        fragment: &crate::g17::fw::channels::WorkSlot,
        tiling: &crate::g17::fw::channels::WorkSlot,
        commit: impl FnOnce() -> Result,
        late_tiling: impl FnOnce(),
    ) -> Result<[u32; 2]> {
        if self.firmware.uat.is_sptm() {
            let submission = match self.m5_registration.take() {
                Some(submission) => submission,
                None => crate::g17::fw::m5::queue::HardwareSubmission::advance_pair(
                    fragment.qid, tiling.qid)?,
            };
            return self.firmware.submit_m5_render_pair(priority, &[*fragment, *tiling],
                &submission, commit, late_tiling, true);
        }
        self.firmware
            .init
            .publish_work_pair(priority, fragment, tiling, true, commit, late_tiling)
    }
    fn publish_configuration_pair(&mut self, priority: u8,
        fragment: &crate::g17::fw::channels::WorkSlot,
        tiling: &crate::g17::fw::channels::WorkSlot, prepare: impl FnOnce()) -> Result {
        let pair = self.firmware.init.m5_work_pair(priority, fragment, tiling)?;
        pair.prepare();
        prepare();
        pair.publish();
        if crate::g17::m5_render_submission::enabled() {
            dev_info!(self.firmware.primary.state.shared.dev.as_ref(),
                "M5 render configuration submitted: queues=[{}, {}] targets=[{}, {}]\n",
                tiling.qid, fragment.qid, tiling.items_end, fragment.items_end);
        }
        Ok(())
    }
    fn activate_render_pool(&mut self, id: u16, generation: u64) -> Result {
        if !self.firmware.uat.is_sptm() { self.release_registration(); }
        let result = self.firmware.queues.render.pools.activate(id, generation);
        if result.is_err() {
            self.firmware.primary.state.shared.lose_device_quietly();
        }
        result
    }
    fn publish_retained_pair(
        &mut self,
        priority: u8,
        fragment: &crate::g17::fw::channels::WorkSlot,
        tiling: &crate::g17::fw::channels::WorkSlot,
        next: [u32; 2],
    ) -> Result {
        let result = self
            .firmware
            .init
            .publish_retained_work_pair(priority, fragment, tiling, next);
        if result.is_err() {
            self.firmware.primary.state.shared.lose_device_quietly();
        }
        result
    }
    /// M5 uses the common compute work ring; Neo uses the tiling work ring.
    /// Priority remains in bits 3:2.
    fn notify_render(&mut self, priority: u8) -> Result {
        if self.firmware.uat.is_sptm() {
            // A paired publication corresponds to native TA and 3D calls.
            for _ in 0..2 {
                if let Err(error) = self.firmware.init.note_m5_submission_history() {
                    pr_warn!("M5 submit-history skipped: {:?}\n", error);
                }
            }
        }
        let result = self
            .firmware
            .primary
            .notify((0x83 << crate::g17::MSG_TYPE_SHIFT) | u64::from(priority) << 2
                | if self.firmware.uat.is_sptm() { 2 } else { 0 });
        self.release_registration();
        if result.is_err() {
            self.firmware.primary.state.shared.lose_device_quietly();
        }
        result
    }
    fn note_submission(&mut self) -> Result {
        self.firmware.note_submission()
    }
    fn complete_render(
        &mut self,
        owners: Option<[crate::g17::qos::Owner; 2]>,
        pool_id: u16,
        generation: u64,
        buffer_id: u8,
    ) -> Result {
        use crate::g17::qos;
        let queues = &mut self.firmware.queues;
        let next = queues.render.pools.completed(pool_id, generation)?;
        queues.buffers.can_release(buffer_id)?;
        let view = qos::View::new(self.firmware.init.qos()?)?;
        if let Some(owners) = owners {
            queues.accounting.complete(&view, &owners)?;
        }
        queues.render.pools.commit_completed(pool_id, next);
        queues.buffers.release(buffer_id)?;
        // Actual retirement returns buffer/pool capacity. Lease rollback uses
        // a separate admission domain and cannot manufacture this progress.
        self.firmware.primary.state.shared.physical_available.notify();
        Ok(())
    }
}

impl crate::g17::Firmware {
    /// Native-like QID recycling (vmfix6). macOS returns a QID to its pool after
    /// the owner's scheduler release and hands it to any engine, keeping the
    /// QID's kick ring and continuing its stamp sequence (v70: 83 processes on
    /// QIDs 0..3, stamps continuous across processes and data masters).
    ///
    /// A parked render pair is the Linux equivalent of a released owner: it
    /// exists only after that owner's scheduler release was consumed and the
    /// pair drained (detach_context). Its rings return to the QID table with
    /// their timestamps; its last scheduler page stays retained until each
    /// QID's next owner installs its own. Every precondition is checked under
    /// the device mutex, and admission is reset atomically last.
    #[inline(never)]
    pub(in crate::g17::runtime) fn recycle_parked_render(&mut self, exclude: Option<u8>) -> Result<bool> {
        for slot in 1..RENDER_SLOTS {
            let slot = slot as u8;
            if Some(slot) == exclude {
                continue;
            }
            let Some(entry) = self.queues.render.entries[usize::from(slot)].as_ref() else {
                continue;
            };
            let Some(parked) = entry.parked.as_ref() else {
                continue;
            };
            // A retired binding of a live context still names the QIDs of the
            // pair it was retired from (RetiredBinding::qids: its graph's two
            // queue records, commands, configurations and completion links,
            // and its context's last publications). vmfix6d: keep exactly
            // those QIDs, not every parked pair, so one retained binding no
            // longer disables all recycling.
            let qids = parked.qids();
            if self
                .queues
                .render
                .retired
                .iter()
                .any(|old| old.context().is_current() && old.qids().iter().any(|qid| qids.contains(qid)))
            {
                continue;
            }
            if entry.pair.is_some()
                || !parked.recyclable()
                || entry.constructing
                || entry.preparing.is_some()
                || entry.growing
                || entry.pending_error.is_some()
                || entry.pending_timeout
                || entry.pending_owner.is_some()
            {
                continue;
            }
            let Some(high_water) = parked.high_water() else {
                continue;
            };
            // Both lanes of a pair advance together; recycled rings are reused
            // only as such a matched pair (or singly by compute).
            if high_water[0].1 != high_water[1].1 {
                continue;
            }
            // No stamp waiter may still read the old sequence: the firmware
            // must have stamped each ring's last committed kick.
            let mut covered = true;
            for (qid, hwm) in high_water {
                if hwm != 0 {
                    let word = self.init.stamp(qid)?.load(core::sync::atomic::Ordering::Acquire);
                    let delta = word.wrapping_sub((hwm as u32).wrapping_shl(8));
                    covered &= delta & 0xff == 0 && delta < 0x8000_0000;
                }
            }
            if !covered {
                continue;
            }
            self.queues.recycled_schedulers.reserve(2, GFP_KERNEL)?;
            if !self.queues.admission.unbuild(slot) {
                continue;
            }
            let entry = self.queues.render.entries[usize::from(slot)].take().ok_or(EIO)?;
            let Some((kicks, scheduler)) = entry.parked.and_then(|parked| parked.into_recycled()) else {
                return Err(EIO);
            };
            for kick in kicks {
                let qid = kick.id().qid();
                if let Err(kick) = self.queues.qids.recycle(kick, "parked render pair") {
                    // Identity mismatch: never free a ring the firmware may name.
                    pr_err!("asahi: qid {} not recyclable; ring retained\n", qid);
                    core::mem::forget(kick);
                    continue;
                }
                self.queues
                    .recycled_schedulers
                    .push_within_capacity((qid, scheduler.clone()))
                    .map_err(|_| ENOMEM)?;
            }
            pr_info!("asahi: render slot {} recycled (qids {}/{})\n", slot, high_water[0].0, high_water[1].0);
            self.primary.state.shared.physical_available.notify();
            self.queues.compute_available.notify();
            return Ok(true);
        }
        Ok(false)
    }
}

impl crate::g17::Firmware {
    /// The pair is returned even when transport, accounting or collection fails.
    /// No off-lock operation or callback which can re-enter the device runs here.
    fn with_render<R>(
        &mut self,
        slot: usize,
        operation: impl FnOnce(&mut Pair, &mut Host<'_>) -> Result<R>,
    ) -> Result<R> {
        let mut pair = self
            .queues
            .render
            .entries
            .get_mut(slot)
            .and_then(Option::as_mut)
            .and_then(|entry| entry.pair.take())
            .ok_or(EBUSY)?;
        let mut host = Host {
            firmware: self,
            registration_powered: false,
            m5_registration: None,
        };
        let result = operation(&mut pair, &mut host);
        host.release_registration();
        self.queues.render.entries[slot].as_mut().ok_or(EIO)?.pair = Some(pair);
        result
    }

    fn release_render_lists(&mut self) -> Result {
        if self.queues.render.grow.reply.is_some() {
            return Err(EAGAIN);
        }
        let primary = &mut self.primary;
        self.queues.render.pools.publish_releases(&self.init, || {
            primary.notify(crate::g17::MSG_CONTROL_NOTIFY)
        })
    }

    /// Events have already been consumed by the caller. Each healthy completion
    /// services its final pool release before examining the next retained job.
    fn retire_render_slot(
        &mut self,
        slot: usize,
        masks: Option<[u64; 2]>,
        deferred: &mut DeferredBatch,
    ) -> Result {
        let generation = self.recovery.generation();
        let state = crate::g17::recovery::Memory::recovery_state(&self.init)?;
        loop {
            let progressed = self.with_render(slot, |pair, host| {
                pair.retire(masks, generation, state, host, &mut |event| {
                    deferred.push(event)
                })
            })?;
            if !progressed {
                break;
            }
            match self.release_render_lists() {
                Err(error) if error == EAGAIN => break,
                result => result?,
            }
        }
        Ok(())
    }

    pub(in crate::g17) fn settle_render_vm(
        &mut self,
        status: &Arc<crate::g17::status::VmStatus>,
        error: Error,
        deferred: &mut DeferredBatch,
    ) -> Result {
        for slot in 1..self.queues.render.entries.len() {
            if self.queues.render.entries[slot]
                .as_ref()
                .is_some_and(|entry| {
                    entry.pair.is_some()
                        && entry
                            .context
                            .as_ref()
                            .is_some_and(|context| Arc::ptr_eq(context.status(), status))
                })
            {
                self.retire_render_slot(slot, None, deferred)?;
            }
        }
        self.queues.fail_render_vm(status, error, deferred)
    }

    pub(in crate::g17) fn stop_render(&mut self) {
        for entry in self.queues.render.entries.iter_mut().flatten() {
            if let Some(pair) = entry.pair.as_mut() {
                if let Err(error) = pair.stopped(&mut |event| {
                    event.finish();
                    Ok(())
                }) {
                    dev_err!(
                        self.primary.state.shared.dev.as_ref(),
                        "Could not settle stopped render pair: {:?}\n",
                        error
                    );
                }
            }
        }
    }
}

impl Registry {
    pub(in crate::g17) fn render_oldest_spared(&self, qid: u8) -> Option<bool> {
        self.render
            .entries
            .iter()
            .flatten()
            .filter_map(|entry| entry.pair.as_ref())
            .find(|pair| pair.qids().contains(&qid))
            .and_then(|pair| pair.oldest_spared())
    }
    pub(in crate::g17) fn visit_render_owners(
        &self,
        qid: u8,
        visit: &mut dyn FnMut(&Arc<crate::g17::status::VmStatus>),
    ) {
        for pair in self
            .render
            .entries
            .iter()
            .flatten()
            .filter_map(|entry| entry.pair.as_ref())
        {
            if pair.qids().contains(&qid) {
                pair.visit_owners(visit);
            }
        }
    }
    pub(in crate::g17) fn classify_render(
        &self,
        classify: &dyn Fn(&Arc<crate::g17::status::VmStatus>) -> crate::g17::recovery::Class,
    ) -> u128 {
        let mut spared = 0;
        for pair in self
            .render
            .entries
            .iter()
            .flatten()
            .filter_map(|entry| entry.pair.as_ref())
        {
            pair.classify(classify);
            if pair.oldest_spared() == Some(true) {
                for qid in pair.qids() {
                    spared |= 1u128 << qid;
                }
            }
        }
        spared
    }

    pub(in crate::g17) fn render_scheduler_active(&self) -> bool {
        self.render.entries.iter().any(Option::is_some)
            || !self.render.retired.is_empty()
            || !self.teardown.is_empty()
    }
    /// Gives back the USC backing of render pools whose client VM pressure
    /// reclaim evicted (an idle VM) and whose final free-list release was
    /// consumed. The vacated backings go to the worker's deferred batch, which
    /// frees them after the device mutex; a later job of that client re-arms
    /// the pool first.
    pub(in crate::g17) fn vacate_evicted_render_pools(
        &mut self,
        init: &crate::g17::initdata::InitData,
        deferred: &mut super::DeferredBatch,
    ) -> Result {
        let render = &mut self.render;
        render.pools.observe(init)?;
        for entry in render.entries.iter().flatten() {
            if !deferred.render_backing_room() {
                break;
            }
            if entry.constructing || entry.growing || entry.preparing.is_some() {
                continue;
            }
            let Some(pool) = entry.pool.as_ref() else {
                continue;
            };
            if pool.vacant() || !render.pools.idle(pool.id()) || !pool.client_evicted() {
                continue;
            }
            if let Some(backing) = pool.vacate() {
                deferred.defer_render_backing(backing);
            }
        }
        Ok(())
    }
    pub(in crate::g17) fn render_in_flight(&self) -> bool {
        self.render.entries.iter().flatten().any(|entry| {
            entry.pair.as_ref().map_or(
                (entry.preparing.is_some() || entry.growing) && entry.borrowed_active,
                |pair| pair.published_in_flight(),
            )
        })
    }
    pub(in crate::g17) fn retain_render_completions(&mut self, masks: [u64; 2]) {
        for entry in self.render.entries.iter_mut().flatten() {
            for id in entry.reservation.ids {
                let qid = usize::from(id.qid());
                entry.event_masks[qid / 64] |= masks[qid / 64] & (1u64 << (qid % 64));
            }
        }
    }
    pub(in crate::g17) fn render_needs_rescan(&self) -> bool {
        self.render.entries.iter().flatten().any(|entry| {
            let active = entry
                .pair
                .as_ref()
                .map_or(
                    (entry.preparing.is_some() || entry.growing) && entry.borrowed_active,
                    |pair| pair.published_in_flight(),
                );
            active
                && entry.reservation.ids.into_iter().all(|id| {
                    let qid = usize::from(id.qid());
                    entry.event_masks[qid / 64] & (1u64 << (qid % 64)) != 0
                })
        })
    }
    pub(in crate::g17) fn defer_render_timeout(
        &mut self,
        packet: &Arc<Packet>,
        error: Error,
    ) -> bool {
        // Its tracker may already be gone while off-lock completion still
        // owns the fence and admission lease. A loan for a subsequent packet
        // cannot turn this exact accepted retirement into a missing-owner error.
        if packet.completion.retirement_queued() {
            return true;
        }
        let Some(slot) = packet.completion.render_slot() else {
            return false;
        };
        let Ok(entry) = self.render.entry_mut(slot) else {
            return false;
        };
        if entry.preparing.is_none() && !entry.growing {
            return false;
        }
        if !entry
            .borrowed_dependencies
            .as_ref()
            .is_some_and(|dependencies| dependencies.contains_packet(packet))
        {
            return false;
        }
        packet.completion.defer_timeout(error);
        entry.pending_timeout = true;
        true
    }
}

/// vmfix6d A: capacity-wait attribution. All of this runs under the device
/// mutex, off the publication fast path (only for waits, at most once a second
/// per waiter, or at the 1/10/30/60 s reports).
impl crate::g17::Firmware {
    /// Everything a pair-local wait on `slot` depends on.
    #[inline(never)]
    pub(super) fn render_slot_fingerprint(&self, slot: u8, vm: &crate::mmu::Vm, mut hash: u64) -> u64 {
        use crate::g17::progress::mix;
        if let Some(entry) = self.queues.render.entries.get(usize::from(slot)).and_then(Option::as_ref) {
            hash = mix(hash, entry.reservation.owner);
            hash = mix(hash, u64::from(entry.reservation.buffer.0));
            hash = mix(hash, entry_flags(entry));
            hash = mix(hash, entry.pending_owner.unwrap_or(0));
            if let Some(pair) = entry.pair.as_ref() {
                hash = pair.diag().fingerprint(hash);
            }
        }
        hash = mix(hash, vm.render_pool_owner(slot));
        let admission = self.queues.admission.diag(slot, 0);
        hash = mix(hash, admission.owner ^ u64::from(admission.refs) << 56);
        hash = mix(hash, u64::from(admission.bits));
        mix(hash, admission.lent_from)
    }

    /// Device-wide capacity: buffer identities, retired bindings, admission.
    #[inline(never)]
    pub(super) fn render_capacity_fingerprint(&self, mut hash: u64) -> u64 {
        use crate::g17::progress::mix;
        let [free, owned, referenced, _refs] = self.queues.buffers.census();
        hash = mix(hash, u64::from(free) | u64::from(owned) << 16 | u64::from(referenced) << 32);
        hash = mix(hash, self.queues.render.retired.len() as u64);
        mix(hash, self.queues.admission.fingerprint())
    }

    pub(in crate::g17) fn render_census(&self) -> ([u32; 4], [u32; 5]) {
        self.queues.render_census()
    }

    #[inline(never)]
    #[cold]
    pub(super) fn describe_render_blocker(
        &self,
        slot: u8,
        owner: u64,
        context: &Arc<Context>,
        priority: u8,
    ) {
        use crate::g17::fw::queue::DataMaster;
        let ready = [DataMaster::Tiling, DataMaster::Fragment]
            .map(|engine| self.init.work_ready(priority, engine).ok());
        let recovery = crate::g17::recovery::Memory::recovery_state(&self.init).ok();
        let (ids, graphs) = self.render_census();
        pr_info!(
            "asahi:  ids free={} owned={} referenced={} refs={} graphs installed={} parked={} retired={} retired_pb={} retired_live={} work_ready={:?} recovery={:?}/{}\n",
            ids[0], ids[1], ids[2], ids[3], graphs[0], graphs[1], graphs[2], graphs[3], graphs[4],
            ready, recovery, self.recovery.pending()
        );
        let admission = self.queues.admission.diag(slot, owner);
        pr_info!(
            "asahi:  admission slot_owner={} refs={} bits={:#x} lent_from={} owns_any={} fresh_blocked={} active={} released={} starved={} built={} footprints={}\n",
            admission.owner, admission.refs, admission.bits, admission.lent_from, admission.owns_any,
            admission.fresh_blocked, admission.counts[0], admission.counts[1], admission.counts[2],
            admission.counts[3], admission.footprints
        );
        let vm_slot = context.vm().render_pool_owner(slot);
        let retained = self
            .queues
            .render
            .retired
            .iter()
            .filter(|old| {
                old.buffer_id().is_some()
                    && Arc::ptr_eq(old.context().status(), context.status())
            })
            .count();
        let Some(entry) = self.queues.render.entries.get(usize::from(slot)).and_then(Option::as_ref)
        else {
            pr_info!("asahi:  entry slot={} absent vm_slot_owner={} retained_same_vm={}\n",
                slot, vm_slot, retained);
            return;
        };
        pr_info!(
            "asahi:  entry slot={} res_owner={} pb={} flags={:#x} pending_owner={:?} vm_slot_owner={} retained_same_vm={}\n",
            slot, entry.reservation.owner, entry.reservation.buffer.0, entry_flags(entry),
            entry.pending_owner, vm_slot, retained
        );
        let Some(pair) = entry.pair.as_ref() else {
            pr_info!("asahi:  pair absent (parked={})\n", entry.parked.is_some());
            return;
        };
        let d = pair.diag();
        let same_context = Arc::ptr_eq(pair.context(), context);
        let same_vm = Arc::ptr_eq(pair.context().status(), context.status());
        let bindable = pair.bindable(owner, context).ok();
        let reusable = pair.reusable().ok();
        let lendable = pair.lendable().ok();
        let idle = pair.idle_for_context(pair.context()).ok();
        pr_info!(
            "asahi:  pair qids={:?} owner={} ctx={} same_ctx={} same_vm={} ordinal={} active={} prepared={} quarantined={} displaced={} previous={:#x} ctx_update={} room={} idle={:?} bindable={:?} reusable={:?} lendable={:?}\n",
            d.qids, d.owner, d.context, same_context, same_vm, d.ordinal, d.active, d.prepared,
            d.quarantined, d.displaced, d.previous, d.context_update, pair.room(), idle, bindable,
            reusable, lendable
        );
        pr_info!(
            "asahi:  pair pb={} installed={:?} registered={} host_cursors={:?} graph={:?} aliases={:#x} claims={:x?}\n",
            d.buffer, d.installed, d.registered, d.cursors, d.graph, d.aliases, d.claims
        );
    }
}

/// Bit 0 closed, 1 lent_live, 2 constructing, 3 preparing, 4 growing,
/// 5 pending_error, 6 pending_timeout, 7 parked, 8 published,
/// 9 pending_owner_closed, 10 borrowed_active.
fn entry_flags(entry: &Entry) -> u64 {
    u64::from(entry.closed)
        | u64::from(entry.lent_live) << 1
        | u64::from(entry.constructing) << 2
        | u64::from(entry.preparing.is_some()) << 3
        | u64::from(entry.growing) << 4
        | u64::from(entry.pending_error.is_some()) << 5
        | u64::from(entry.pending_timeout) << 6
        | u64::from(entry.parked.is_some()) << 7
        | u64::from(entry.published) << 8
        | u64::from(entry.pending_owner_closed) << 9
        | u64::from(entry.borrowed_active) << 10
}

/// vmfix6d eager restore outcome.
pub(super) enum Restore {
    /// Nothing retained for the requesting VM on this slot.
    None,
    /// Reinstalled; the emptied parked owner (if any) is dropped off-lock.
    Done(Option<crate::g17::queue::render::parked::ParkedPair>),
    /// A parked pair needs pair storage, allocated off-lock: retry with it.
    NeedStorage,
}

impl crate::g17::Firmware {
    /// vmfix6d eager restore. A VM keeps the pool mappings of a graph it has
    /// on a physical slot (Vm::render[slot]) until that graph is dropped, so a
    /// retained (displaced, Deferred or Draining) graph of the requesting VM
    /// makes any new graph's install_render_pool_mappings() on the same slot
    /// return EBUSY until the retaining context closes. When such a graph
    /// exists, reinstall it instead of building a new one. The successor's
    /// binding is retained in turn (as a foreign rebind displaces it), and the
    /// pair is left ownerless for the normal same-VM rebind to bind it.
    ///
    /// EBUSY: a retained graph exists but the successor has not fully retired.
    /// vmfix6d: the index of a graph of `context`'s VM retained after it was
    /// displaced from `slot`, when the slot currently holds another VM's pair
    /// or a parked pair. Cheap unless the slot changed VM (scans the retired
    /// list only then); the build path checks it on every build.
    fn retained_index(&self, slot: u8, context: &Arc<Context>) -> Option<usize> {
        let entry = self.queues.render.entries.get(usize::from(slot)).and_then(Option::as_ref)?;
        let qids = match (entry.pair.as_ref(), entry.parked.as_ref()) {
            (Some(pair), _) => {
                if Arc::ptr_eq(pair.context().status(), context.status()) {
                    return None;
                }
                pair.qids()
            }
            (None, Some(parked)) => parked.qids(),
            (None, None) => return None,
        };
        self.queues.render.retired.iter().position(|old| {
            old.restorable_in_place()
                && old.qids() == qids
                && Arc::ptr_eq(old.context().status(), context.status())
        })
    }
    pub(super) fn render_retained_for(&self, slot: u8, context: &Arc<Context>) -> bool {
        self.retained_index(slot, context).is_some()
    }

    #[inline(never)]
    pub(super) fn restore_render_binding(
        &mut self,
        slot: u8,
        context: &Arc<Context>,
        storage: &mut Option<KBox<core::mem::MaybeUninit<Pair>>>,
        pool: &Arc<crate::g17::freelist::RenderPool>,
        pool_pin: &mut Option<crate::g17::freelist::PoolPin>,
    ) -> Result<Restore> {
        let Some(index) = self.retained_index(slot, context) else {
            return Ok(Restore::None);
        };
        let entry = self.queues.render.entry(slot)?;
        let qids = match (entry.pair.as_ref(), entry.parked.as_ref()) {
            (Some(pair), _) => pair.qids(),
            (None, Some(parked)) => parked.qids(),
            (None, None) => return Ok(Restore::None),
        };
        if entry.constructing
            || entry.preparing.is_some()
            || entry.growing
            || entry.pending_error.is_some()
            || entry.pending_timeout
        {
            return Err(EBUSY);
        }
        let stamps = [
            self.init.stamp(qids[0])?.load(core::sync::atomic::Ordering::Relaxed),
            self.init.stamp(qids[1])?.load(core::sync::atomic::Ordering::Relaxed),
        ];
        core::sync::atomic::fence(core::sync::atomic::Ordering::Acquire);
        let binding = &self.queues.render.retired[index];
        if !Arc::ptr_eq(binding.pool(), pool) || pool.vacant() {
            return Err(EAGAIN);
        }
        let installed = entry.pair.is_some();
        let cursors = match (entry.pair.as_ref(), entry.parked.as_ref()) {
            (Some(pair), _) => pair.restorable(binding, stamps)?,
            (None, Some(parked)) => parked.restorable(binding, stamps)?,
            (None, None) => None,
        };
        let Some(cursors) = cursors else {
            return Err(EBUSY);
        };
        if !installed && storage.is_none() {
            return Ok(Restore::NeedStorage);
        }
        let buffer = binding.buffer_id().ok_or(EIO)?;
        let token = self.queues.buffers.owner_token(buffer)?;
        let restored_context = binding.context().clone();
        if installed {
            // Exactly the foreign-displacement bookkeeping of rebind_pair():
            // save the detaching pool and retain the successor's scheduler
            // (Deferred while its owner lives, Draining once it closed).
            self.queues.render.save_detaching_pool(&self.init, slot)?;
            let entry = self.queues.render.entry(slot)?;
            let owner_closed = entry.closed || !entry.lent_live;
            let successor = entry.pair.as_ref().ok_or(EIO)?.context().clone();
            self.queues.teardown.displace(&successor, owner_closed, super::now_ns())?;
        }
        let binding = self.queues.render.retired.remove(index).map_err(|_| EIO)?;
        let render = &mut self.queues.render;
        let entry = render.entries[usize::from(slot)].as_mut().ok_or(EIO)?;
        let parked = if installed {
            let pair = entry.pair.as_mut().ok_or(EIO)?;
            let successor = pair.restore(binding, cursors, pool_pin.take());
            if render.retired.push_within_capacity(successor).is_err() {
                // Capacity was freed by the removal above.
                return Err(EIO);
            }
            None
        } else {
            let mut parked = entry.parked.take().ok_or(EIO)?;
            match parked.restore(binding, cursors, storage.take().ok_or(EIO)?, pool_pin.take()) {
                Ok(pair) => entry.pair = Some(pair),
                Err(binding) => {
                    entry.parked = Some(parked);
                    if binding.buffer_id().is_some() {
                        render.retired.push_within_capacity(binding).map_err(|_| EIO)?;
                    }
                    return Err(EIO);
                }
            }
            Some(parked)
        };
        let entry = render.entries[usize::from(slot)].as_mut().ok_or(EIO)?;
        entry.reservation.buffer = (buffer, token);
        entry.pool = entry.pair.as_ref().map(|pair| pair.pool().clone());
        entry.context = Some(restored_context);
        // The restored context is live or Draining already; the same-VM rebind
        // that follows must not arm a release for it (owner_closed = false).
        entry.closed = false;
        entry.lent_live = true;
        static RESTORES: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);
        if let Some(n) = crate::g17::progress::sampled(&RESTORES) {
            let (ids, graphs) = self.render_census();
            pr_info!("asahi: render slot {} restored a retained graph (pb {}, {}) for ctx {} (restore {}); ids free={} owned={} retired_pb={}\n",
                slot, buffer, if installed { "installed" } else { "parked" }, context.id(), n,
                ids[0], ids[1], graphs[3]);
        }
        self.queues.log_render_census("restore");
        Ok(Restore::Done(parked))
    }
}

impl crate::g17::Firmware {
    /// vmfix6e eager collection. A graph displaced from a live context's pair
    /// is retained (with its PB identity, graph storage and the VM's pool
    /// mappings for that slot) until that context closes; under lending churn
    /// those graphs exhausted all 127 PB identities. Collect one once:
    /// - its rings' stamps cover the ordinal it was displaced at, so a later
    ///   binding's first publication there has retired (and every publication
    ///   any channel of its context made through that pair before);
    /// - Memory::collectable: no generation owns an ordinal, pointer blocks
    ///   drained, private job list empty, the context's shared job list neither
    ///   starts nor ends in its queue storage;
    /// - its free list stays alive: it is the context's current pool, or its
    ///   release was consumed.
    /// These are driver-visible witnesses only; firmware-internal references
    /// are not observable (m5_render_collect_poison makes one fault loudly).
    #[inline(never)]
    pub(in crate::g17::runtime) fn collect_retained_eagerly(
        &mut self,
        reclaim: &mut super::teardown::ReclaimBatch,
    ) -> Result {
        let poison = *crate::module_parameters::m5_render_collect_poison.value() != 0;
        let mut index = 0;
        while index < self.queues.render.retired.len() {
            let old = &self.queues.render.retired[index];
            if old.buffer_id().is_none() || !old.context().is_current() {
                index += 1;
                continue;
            }
            let qids = old.qids();
            let stamps = [
                self.init.stamp(qids[0])?.load(core::sync::atomic::Ordering::Relaxed),
                self.init.stamp(qids[1])?.load(core::sync::atomic::Ordering::Relaxed),
            ];
            core::sync::atomic::fence(core::sync::atomic::Ordering::Acquire);
            let pool = old.pool();
            let pool_alive = old.context().render_pool_is(pool)
                || self.queues.render.pools.released(u32::from(pool.id()), pool.control_va());
            if !pool_alive || !old.collectable(stamps)? {
                index += 1;
                continue;
            }
            // Off-lock destruction needs a collector slot: for the graph itself,
            // or (poison) for the oldest poisoned graph once the FIFO is full.
            let poison_full = self.queues.render.poisoned.len() == self.queues.render.poisoned.capacity();
            if (!poison || poison_full) && !reclaim.has_room(1, 0) {
                break;
            }
            let mut binding = self.queues.render.retired.remove(index).map_err(|_| EIO)?;
            let id = binding.buffer_id().ok_or(EIO)?;
            let context = binding.context().id();
            if poison {
                binding.poison();
                // Until the identity is reused, its PB descriptor names an
                // unmapped page list.
                let table = self.init.pb_descriptor_table()?;
                let offset = usize::from(id) * 16;
                let words = [
                    table.word(offset)?, table.word(offset + 4)?,
                    table.word(offset + 8)?, table.word(offset + 12)?,
                ];
                let previous = words.map(|word| word.load(core::sync::atomic::Ordering::Relaxed));
                let next = crate::g17::fw::buffer::reload_descriptor(previous, 0xdead_0000_0000, previous[1] & 0x003f_ffff);
                for (word, value) in words.into_iter().zip(next) {
                    word.store(value, core::sync::atomic::Ordering::Relaxed);
                }
                core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
                let render = &mut self.queues.render;
                if poison_full {
                    let oldest = render.poisoned.remove(0).map_err(|_| EIO)?;
                    *reclaim.binding_slot()? = Some(oldest);
                }
                render.poisoned.push_within_capacity(binding).map_err(|_| EIO)?;
            } else {
                *reclaim.binding_slot()? = Some(binding);
            }
            self.queues.buffers.unreserve_owner(id)?;
            self.primary.state.shared.physical_available.notify();
            static COLLECTS: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);
            if let Some(n) = crate::g17::progress::sampled(&COLLECTS) {
                let (ids, graphs) = self.queues.render_census();
                pr_info!("asahi: render retained graph collected (pb {}, ctx {}, qids {:?}{}) (collect {}); ids free={} retired_pb={}\n",
                    id, context, qids, if poison { ", poisoned" } else { "" }, n, ids[0], graphs[3]);
            }
        }
        Ok(())
    }
}
