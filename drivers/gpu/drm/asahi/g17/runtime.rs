// SPDX-License-Identifier: GPL-2.0-only OR MIT

//! Device ownership of physical queues and off-lock construction reservations.
//!
//! The device mutex protects this registry. Logical queues may disappear while
//! their published graphs remain here; only exact retirement permits rebinding,
//! and only processor stop permits dropping installed firmware addresses.
//! A preparation lease keeps Shared::state populated: shutdown and loss join
//! every lease before taking the session, including builders asleep on changed.

mod backend;
mod compute_control;
mod compute_replay;
mod compute_exit;
mod events;
mod feed;
mod render;
pub(super) mod teardown;

pub(super) use super::now_ns;
use super::{
    buffer::{BufferIds, MetricsIds},
    context::{Context, QosIds},
    fw::queue::DataMaster,
    kick, qos,
    queue::{self, admission, compute},
};
use core::sync::atomic::{fence, AtomicUsize, Ordering};
use kernel::{prelude::*, sync::Arc};

const QIDS: usize = 128;
const FIRST_COMPUTE_QID: u8 = 4;
const RECOVERY_SETTLE_POLLS: usize = 200;
const WORK_RING_WAIT_NS: u64 = super::job::TIMEOUT_MS as u64 * 1_000_000;
/// A physical compute queue frees when another owner's queue goes idle.
const COMPUTE_QUEUE_WAIT_NS: u64 = 30_000_000_000;

/// Before visibility, rollback occurs under the publication lock and cannot
/// invoke availability callbacks. Visible work transfers these to Completion.
struct PendingSourceHolds(KVec<super::source_hold::SourceHold>);
impl Drop for PendingSourceHolds {
    fn drop(&mut self) {
        for hold in self.0.drain(..) { hold.drop_quiet(); }
    }
}
impl PendingSourceHolds {
    fn into_holds(mut self) -> KVec<super::source_hold::SourceHold> {
        core::mem::replace(&mut self.0, KVec::new())
    }
}

/// A reservation remains identifiable while its backing is built without the
/// device mutex. Failed construction returns the slot, never its host identity.
#[derive(Clone, Copy)]
pub(super) struct ComputeReservation {
    slot: usize,
    pub(super) id: kick::Id,
    owner: u64,
}

/// An unpublished packet reserves one physical queue slot across prepare/run.
/// The counter also prevents an idle owner from being reassigned in that gap.
/// Drop must occur outside the device lock because it can signal waiters.
pub(crate) struct ComputeClaim {
    pending: Arc<AtomicUsize>,
    available: Arc<super::availability::Availability>,
    owner: u64,
    qid: u8,
}
impl Drop for ComputeClaim {
    fn drop(&mut self) {
        self.pending.fetch_sub(1, Ordering::AcqRel);
        self.available.notify();
    }
}

pub(super) struct ComputeGrowth {
    pages: Option<super::freelist::Pages>,
    alias: Option<crate::mmu::KernelMapping>,
}

struct ComputeEntry {
    growth: Option<ComputeGrowth>,
    reservation: ComputeReservation,
    queue: Option<KBox<compute::Queue>>,
}

pub(super) struct Registry {
    pub(super) qids: kick::QueueIds,
    pub(super) buffers: BufferIds,
    pub(super) accounting: qos::Accounting,
    pub(super) metrics: Arc<MetricsIds>,
    pub(super) qos_ids: Arc<QosIds>,
    pub(super) admission: Arc<admission::Pool>,
    render: render::RenderState,
    identities: queue::Identities,
    compute: KVec<Option<ComputeEntry>>,
    /// Fixed slot counters survive ordinary graph recycling. Prepared claims
    /// prevent a slot from changing owner until publication or cancellation.
    compute_prepared: KVec<Arc<AtomicUsize>>,
    pub(super) compute_available: Arc<super::availability::Availability>,
    pub(super) compute_building: bool,
    compute_scheduler_building: bool,
    /// Last scheduler pages of recycled render pairs (vmfix6), kept per QID
    /// until that QID's next owner installs its own queue.
    recycled_schedulers: KVec<(u8, Arc<crate::g17::context::SchedulerStorage>)>,
    /// A compute bind found no QID and no reusable fitting or vacant queue
    /// (set until this time, ns): idle closed-owner pools are vacated below
    /// the warm cap so the waiter can re-arm one.
    compute_pressure_until: u64,
    replay_outer_wait: Option<u64>,
    /// A compute replay with a render consumer waits (since this time, ns)
    /// until the owner's renders are parked for replay or idle.
    replay_consumer_wait: Option<u64>,
    /// A replay is awaiting a source's publication in the current epoch.
    replay_source_wait: bool,
    pub(super) observations: compute::retirement::Batch,
    teardown: super::teardown::Pending,
}

impl Registry {
    fn resolve_sources(&self, packet: &Arc<super::job::Packet>, epoch: u64,
        dependencies: &mut super::dependency::Frontier) -> Result<PendingSourceHolds>
    {
        use super::sync_source::Snapshot;
        packet.check_source_inputs()?;
        let mut holds = PendingSourceHolds(KVec::with_capacity(packet.sources.len(), GFP_KERNEL)?);
        for dependency in &packet.sources {
            // snapshot checks the source error and VM status in one critical section.
            match dependency.source.snapshot(epoch)? {
                Snapshot::Complete => {},
                Snapshot::Pending => return Err(EAGAIN),
                Snapshot::Published(token) => {
                    if packet.completion.source().as_ref().is_some_and(|source|
                        Arc::ptr_eq(source, &dependency.source)) { return Err(EINVAL); }
                    let queue = self.compute.iter().flatten()
                        .filter_map(|entry| entry.queue.as_deref())
                        .find(|queue| queue.qid() == token.qid).ok_or(EAGAIN)?;
                    let hold = match queue.hold_source(&dependency.source, token) {
                        Ok(hold) => hold,
                        Err(error) if error == EAGAIN && matches!(dependency.source.snapshot(epoch)?, Snapshot::Complete) => continue,
                        Err(error) => return Err(error),
                    };
                    holds.0.push_within_capacity(hold).map_err(|error| {
                        error.0.drop_quiet(); ENOMEM
                    })?;
                    dependencies.insert(token.sequence,
                        super::fw::kick::KickDependency::new(token.qid, token.kick).ok_or(EINVAL)?)?;
                }
            }
        }
        Ok(holds)
    }

    /// Initialize inside the firmware allocation instead of staging the whole
    /// registry on the probe stack. No additional allocation is required.
    pub(super) fn new(
        clusters: u32,
        descriptor_flags: [u32; 2],
        metrics: Arc<MetricsIds>,
    ) -> impl Init<Self, Error> {
        kernel::try_init!(Self {
            qids <- kick::QueueIds::new().chain(|qids| {
                qids.reserve(1, DataMaster::Tiling, Some(0))?;
                qids.reserve(2, DataMaster::Fragment, Some(1))?;
                Ok(())
            }),
            buffers <- BufferIds::new(),
            accounting: qos::Accounting::new(),
            metrics,
            qos_ids: QosIds::new()?,
            admission: admission::Pool::new()?,
            render: render::RenderState::new(clusters, descriptor_flags)?,
            identities: queue::Identities::new(),
            compute: {
                let mut compute = KVec::with_capacity(QIDS, GFP_KERNEL)?;
                for _ in 0..QIDS {
                    compute.push(None, GFP_KERNEL)?;
                }
                compute
            },
            compute_prepared: {
                let mut counters = KVec::with_capacity(QIDS, GFP_KERNEL)?;
                for _ in 0..QIDS {
                    counters.push(Arc::new(AtomicUsize::new(0), GFP_KERNEL)?, GFP_KERNEL)?;
                }
                counters
            },
            compute_available: super::availability::Availability::new()?,
            compute_building: false,
            compute_scheduler_building: false,
            recycled_schedulers: KVec::new(),
            compute_pressure_until: 0,
            replay_outer_wait: None,
            replay_consumer_wait: None,
            replay_source_wait: false,
            observations: compute::retirement::Batch::new()?,
            teardown: super::teardown::Pending::new()?,
        })
    }

    pub(super) fn compute(&self, owner: u64) -> Option<&compute::Queue> {
        self.compute
            .iter()
            .flatten()
            .filter_map(|entry| entry.queue.as_deref())
            .find(|queue| queue.owner() == Some(owner))
    }

    pub(super) fn compute_mut(&mut self, owner: u64) -> Option<&mut compute::Queue> {
        self.compute
            .iter_mut()
            .flatten()
            .filter_map(|entry| entry.queue.as_deref_mut())
            .find(|queue| queue.owner() == Some(owner))
    }

    /// Existing device mutex excludes registry mutation. No nested locks or callbacks.
    fn timeout_snapshot(&mut self, owner: u64, packet: &Arc<super::job::Packet>,
        out: &mut super::timeout_trace::Snapshot)
    {
        let Some(slot) = self.compute.iter().position(|entry| entry.as_ref()
            .and_then(|entry| entry.queue.as_deref()).is_some_and(|q| q.owner() == Some(owner))) else {
            out.reason = super::timeout_trace::NO_QUEUE;
            return;
        };
        let entry = self.compute[slot].as_mut().unwrap();
        out.reservation = [slot as u64, u64::from(entry.reservation.id.qid()),
            entry.reservation.owner, self.compute_prepared[slot].load(Ordering::Acquire) as u64,
            u64::from(entry.growth.is_some()), u64::from(self.compute_building)
                | (u64::from(self.compute_scheduler_building) << 1)];
        entry.queue.as_deref_mut().unwrap().timeout_snapshot(packet, out);
        // All resident compute queues, including vacant/quarantined ones. These rows
        // support cross-QID dependency lookup without decoding a possibly stale kick.
        for entry in self.compute.iter().flatten() {
            if let Some(queue) = entry.queue.as_deref() {
                let i = out.peer_count as usize;
                if i == out.peers.len() { out.reason = super::timeout_trace::OVERFLOW; break; }
                out.peers[i] = queue.timeout_peer(); out.peer_count += 1;
            }
        }
        self.render.timeout_snapshot(out);
    }

    fn claim_compute(&self, owner: u64, context: &Arc<Context>,
        existing: &Option<ComputeClaim>) -> Result<Option<ComputeClaim>>
    {
        let (slot, queue) = self.compute.iter().enumerate().find_map(|(slot, entry)| {
            let queue = entry.as_ref()?.queue.as_deref()?;
            (queue.owner() == Some(owner)).then_some((slot, queue))
        }).ok_or(EBUSY)?;
        // A spared queue waiting for its ownership witness is backpressure,
        // not an identity error. Its event-worker replay/retirement wakes us.
        if existing.is_none() && queue.preparation_slots() == 0 { return Err(EBUSY); }
        if !queue.matches(owner, queue.qid(), context)? { return Err(EBUSY); }
        let pending = &self.compute_prepared[slot];
        if let Some(claim) = existing {
            return if claim.owner == owner && claim.qid == queue.qid()
                && Arc::ptr_eq(&claim.pending, pending) { Ok(None) } else { Err(EIO) };
        }
        if self.compute[slot].as_ref().is_some_and(|entry| entry.growth.is_some()) { return Err(EBUSY); }
        let count = pending.load(Ordering::Acquire);
        if count >= queue.preparation_slots() { return Err(EBUSY); }
        pending.fetch_add(1, Ordering::AcqRel);
        Ok(Some(ComputeClaim { pending: pending.clone(),
            available: self.compute_available.clone(), owner, qid: queue.qid() }))
    }

    /// An installed idle queue keeps its original QID and backing. Mapping a
    /// replacement context must succeed before its old binding is displaced.
    /// A vacant queue (USC backing given back) is offered for re-arming only
    /// when no populated idle queue exists.
    pub(super) fn rebind_compute(&mut self, owner: u64, context: &Arc<Context>, required: Option<super::fw::m5::pool::PoolGeometry>) -> Result<Rebind> {
        let prepared = &self.compute_prepared;
        if let Some(queue) = self.compute(owner) {
            return Ok(if queue.matches(owner, queue.qid(), context)? {
                if queue.pool_fits(required) { Rebind::Done } else { Rebind::Grow }
            } else {
                Rebind::Fresh
            });
        }
        let mut vacant = None;
        for queue in self
            .compute
            .iter_mut()
            .enumerate()
            .filter(|(slot, entry)| prepared[*slot].load(Ordering::Acquire) == 0
                && entry.as_ref().is_some_and(|entry| entry.growth.is_none()))
            .filter_map(|(_, entry)| entry.as_mut().and_then(|entry| entry.queue.as_deref_mut()))
            .filter(|queue| !queue.source_held() && queue.reusable())
        {
            if queue.pool_vacant() {
                vacant.get_or_insert(queue.qid());
                continue;
            }
            if !queue.pool_fits(required) { continue; }
            queue.bind_owner(owner, context.clone())?;
            return Ok(Rebind::Done);
        }
        Ok(vacant.map_or(Rebind::Fresh, Rebind::Rearm))
    }

    /// `id`'s queue is installed in the registry: an adopted spare ring is owned
    /// again, and the scheduler page its previous owner left installed is no
    /// longer named once this owner's installation replaces it.
    pub(super) fn adopted_installed(&mut self, id: kick::Id) {
        self.qids.adopted_installed(id);
        self.recycled_schedulers.retain(|(qid, _)| *qid != id.qid());
    }

    /// With no QID left, hand the least recently released idle queue of another
    /// live owner to `requester`: release it exactly as an owner close does,
    /// then let rebind_compute bind it. The previous owner re-acquires a queue
    /// on its next submission (ensure_compute / the publication rebind).
    /// Caller holds the device mutex, which also serializes every submission's
    /// queue check and publication, so the candidate cannot be mid-publish.
    pub(super) fn reclaim_idle_compute(
        &mut self,
        requester: u64,
        required: Option<super::fw::m5::pool::PoolGeometry>,
    ) -> Result<Option<u8>> {
        let prepared = &self.compute_prepared;
        let Some(queue) = self
            .compute
            .iter_mut()
            .enumerate()
            .filter(|(slot, entry)| prepared[*slot].load(Ordering::Acquire) == 0
                && entry.as_ref().is_some_and(|entry| entry.growth.is_none()))
            .filter_map(|(_, entry)| entry.as_mut().and_then(|entry| entry.queue.as_deref_mut()))
            .filter(|queue| {
                !queue.source_held() && queue.idle_owned()
                    && queue.owner() != Some(requester) && queue.pool_fits(required)
            })
            .min_by_key(|queue| queue.released_at())
        else {
            return Ok(None);
        };
        let previous = queue.owner();
        if queue.release_owner()? || !queue.reusable() {
            // Not reachable for an idle_owned queue; it stays released and is
            // reused through the normal close path once reusable.
            pr_warn!("asahi: compute qid {} released from idle owner {:?} but not reusable\n",
                queue.qid(), previous);
            return Ok(None);
        }
        static RECLAIMS: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);
        let n = RECLAIMS.fetch_add(1, Ordering::Relaxed);
        if n < 8 || (n + 1).is_power_of_two() {
            pr_info!("asahi: compute qid {} reassigned from idle owner {:?} to {} (reassignment {})\n",
                queue.qid(), previous, requester, n + 1);
        }
        Ok(Some(queue.qid()))
    }

    /// Installs fresh USC backing into the vacant queue `qid`; the caller writes
    /// the returned descriptor row before binding. EAGAIN if the queue was taken.
    pub(super) fn rearm_compute(
        &mut self,
        qid: u8,
        pages: crate::g17::freelist::Pages,
        ring: compute::DescriptorRing,
    ) -> Result<crate::g17::freelist::PoolDescriptor> {
        let queue = self
            .compute
            .iter_mut()
            .flatten()
            .filter_map(|entry| entry.queue.as_deref_mut())
            .find(|queue| queue.qid() == qid)
            .ok_or(EAGAIN)?;
        queue.rearm(pages, ring)
    }

    pub(super) fn bind_rearmed(&mut self, qid: u8, owner: u64, context: &Arc<Context>) -> Result {
        let queue = self
            .compute
            .iter_mut()
            .flatten()
            .filter_map(|entry| entry.queue.as_deref_mut())
            .find(|queue| queue.qid() == qid && queue.reusable())
            .ok_or(EAGAIN)?;
        queue.bind_owner(owner, context.clone())
    }

    /// Publishes releases for the oldest idle retained pools beyond `cap`, at
    /// most four per pass. Returns the number published.
    pub(super) fn vacate_idle_pools(
        &mut self,
        init: &super::initdata::InitData,
        cap: usize,
    ) -> Result<usize> {
        // Count first: event turns are frequent and the steady state has no excess.
        let forced = self
            .compute
            .iter()
            .flatten()
            .filter_map(|entry| entry.queue.as_deref())
            .filter(|queue| queue.vacate_candidate() && queue.needs_fresh_backing())
            .count();
        let count = self
            .compute
            .iter()
            .flatten()
            .filter_map(|entry| entry.queue.as_deref())
            .filter(|queue| queue.vacate_candidate())
            .count();
        if count <= cap && forced == 0 {
            return Ok(0);
        }
        let mut candidates: KVec<(u64, u8)> = KVec::with_capacity(count, GFP_KERNEL)?;
        for queue in self
            .compute
            .iter()
            .flatten()
            .filter_map(|entry| entry.queue.as_deref())
            .filter(|queue| queue.vacate_candidate())
        {
            // Recycled killed queues sort first (time 0) and count beyond the cap.
            let key = if queue.needs_fresh_backing() { 0 } else { queue.released_at() };
            candidates.push((key, queue.qid()), GFP_KERNEL)?;
        }
        candidates.sort_unstable();
        let excess = candidates.len().saturating_sub(cap).max(forced);
        let mut published = 0;
        for (_, qid) in candidates.iter().take(excess.min(4)) {
            let queue = self
                .compute
                .iter_mut()
                .flatten()
                .filter_map(|entry| entry.queue.as_deref_mut())
                .find(|queue| queue.qid() == *qid)
                .ok_or(EIO)?;
            match queue.vacate(init) {
                Err(error) if error == EAGAIN => break,
                result => result?,
            }
            published += 1;
        }
        Ok(published)
    }

    /// Frees the backing of every pool whose release the firmware consumed.
    pub(super) fn observe_pool_releases(
        &mut self,
        init: &super::initdata::InitData,
        deferred: &mut DeferredBatch,
    ) -> Result<usize> {
        fence(Ordering::Acquire);
        let consumer = init.control_consumer()?.load(Ordering::Relaxed);
        let producer = init.control_producer()?.load(Ordering::Relaxed);
        let mut freed = 0;
        for queue in self
            .compute
            .iter_mut()
            .flatten()
            .filter_map(|entry| entry.queue.as_deref_mut())
        {
            if let Some(vacated) = queue.observe_vacancy(consumer, producer)? {
                deferred.defer_vacated(vacated);
                freed += 1;
            }
        }
        Ok(freed)
    }

    /// The diagnostics and runtime share one non-recycling channel namespace.
    pub(super) fn reserve_channel_ids(&mut self, count: u64) -> Result<core::ops::Range<u64>> {
        self.identities.reserve(count)
    }

    pub(super) fn reserve_compute(&mut self, owner: u64) -> Result<ComputeReservation> {
        if owner == 0
            || self.compute.iter().flatten().any(|entry| {
                entry
                    .queue
                    .as_ref()
                    .map_or(entry.reservation.owner == owner, |queue| {
                        queue.owner() == Some(owner)
                    })
            })
        {
            return Err(EINVAL);
        }
        let slot = self
            .compute
            .iter()
            .position(Option::is_none)
            .ok_or_else(|| enospc_site!("runtime.rs:284 compute slot table"))?;
        let identity = self.identities.reserve(1)?.start;
        let id = self
            .qids
            .reserve(identity, DataMaster::Compute, Some(FIRST_COMPUTE_QID))?;
        let reservation = ComputeReservation { slot, id, owner };
        self.compute[slot] = Some(ComputeEntry {
            growth: None,
            reservation,
            queue: None,
        });
        Ok(reservation)
    }

    fn reserved(&mut self, reservation: ComputeReservation) -> Result<&mut ComputeEntry> {
        let entry = self
            .compute
            .get_mut(reservation.slot)
            .and_then(Option::as_mut)
            .ok_or(EIO)?;
        if entry.reservation.id != reservation.id
            || entry.reservation.owner != reservation.owner
            || entry.queue.is_some()
        {
            return Err(EIO);
        }
        Ok(entry)
    }

    /// The caller checked its preparation epoch and retained the same device
    /// session before placing this graph where publication can find it.
    pub(super) fn install_compute(
        &mut self,
        reservation: ComputeReservation,
        queue: &mut Option<KBox<compute::Queue>>,
    ) -> Result {
        let built = queue.as_ref().ok_or(EINVAL)?;
        if built.qid() != reservation.id.qid() || built.owner() != Some(reservation.owner) {
            return Err(EIO);
        }
        let entry = self.reserved(reservation)?;
        entry.queue = queue.take();
        Ok(())
    }

    pub(super) fn cancel_compute(&mut self, reservation: ComputeReservation) -> Result {
        self.reserved(reservation)?;
        self.qids.cancel_unpublished(reservation.id)?;
        self.compute[reservation.slot] = None;
        Ok(())
    }

    pub(super) fn release_compute(&mut self, owner: u64) -> Result<Option<ComputeGrowth>> {
        let Some(slot) = self.compute.iter().position(|entry| {
            entry
                .as_ref()
                .and_then(|entry| entry.queue.as_ref())
                .is_some_and(|queue| queue.owner() == Some(owner))
        }) else {
            return Ok(None);
        };
        let entry = self.compute[slot].as_mut().ok_or(EIO)?;
        // Unstarted backing has never entered a firmware descriptor. Return
        // it to the caller for destruction after dropping the device mutex.
        // A published pool release keeps its growth owner until its ack.
        let retired = if !entry.queue.as_ref().ok_or(EIO)?.growth_started() {
            entry.growth.take()
        } else { None };
        if entry.queue.as_mut().ok_or(EIO)?.release_owner()? {
            self.qids.cancel_unpublished(entry.reservation.id)?;
            self.compute[slot] = None;
        }
        Ok(retired)
    }
}

/// Outcome of offering an owner a retained compute queue.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(super) enum Rebind {
    /// The owner now holds an installed, populated queue.
    Done,
    /// The current owner needs additional private-memory backing.
    Grow,
    /// Only a vacant queue is available; re-arm `qid` with fresh backing.
    Rearm(u8),
    /// No retained queue suits the owner: reserve a fresh QID.
    Fresh,
}

/// An immutable logical owner shared by its two scheduler entities. Installed
/// physical graphs retain the context independently of this frontend lifetime.
pub(super) struct Backend {
    shared: Arc<super::Shared>,
    pub(super) context: Arc<Context>,
    owner: u64,
    backlog: core::sync::atomic::AtomicU32,
    compute_backlog: core::sync::atomic::AtomicU32,
    /// vmfix6d attribution: the last site that returned EBUSY to this queue's
    /// render publication (render::submit::Site), reported by capacity waits.
    busy_site: core::sync::atomic::AtomicU32,
    /// vmfix6d A: a render publication of this queue is in progress.
    publishing: core::sync::atomic::AtomicBool,
}

enum ComputePublication {
    SourcePending,
    Attempted(Result),
}

impl Backend {
    /// Queue creation failures reach userspace as a lost device at its first
    /// submission (hk creates kernel queues lazily); never fail silently.
    pub(super) fn new(
        shared: &Arc<super::Shared>,
        vm: &crate::mmu::Vm,
        owner: u64,
        policy: super::fw::queue::Policy,
    ) -> Result<(Arc<Self>, admission::QueueSlot)> {
        Self::new_inner(shared, vm, owner, policy).inspect_err(|error| {
            static FAILURES: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);
            let n = FAILURES.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
            if *error != EAGAIN && (n < 8 || (n + 1).is_power_of_two()) {
                pr_warn!("asahi: GPU queue creation failed: owner={} error={:?} (failure {})\n", owner, error, n + 1);
            }
        })
    }

    fn new_inner(
        shared: &Arc<super::Shared>,
        vm: &crate::mmu::Vm,
        owner: u64,
        policy: super::fw::queue::Policy,
    ) -> Result<(Arc<Self>, admission::QueueSlot)> {
        let lease = shared.preparations.enter(true)?;
        let dev = shared.drm()?;
        let (uat, pool, qos, execution, slot) = {
            let mut state = shared.state.lock();
            let firmware = (*state).as_deref_mut().ok_or(ENODEV)?;
            if shared.crashed.load(core::sync::atomic::Ordering::Acquire) {
                return Err(EIO);
            }
            let slot = firmware.queues.admission.reserve_queue()?;
            let execution = firmware.uat.new_execution_context(vm)?;
            (
                firmware.uat.clone(),
                firmware.pool.clone(),
                firmware.queues.qos_ids.clone(),
                execution,
                slot,
            )
        };
        let alloc = super::object::Allocator {
            dev: &dev,
            uat: &uat,
        };
        let owner_pid =
            u32::try_from(kernel::current!().group_leader().pid()).map_err(|_| EOVERFLOW)?;
        let context = Arc::new(
            Context::new(&alloc, &pool, &qos, execution, owner_pid, policy)?,
            GFP_KERNEL,
        )?;
        let state = shared.state.lock();
        if !lease.is_current() || state.is_none() {
            return Err(EAGAIN);
        }
        drop(state);
        Ok((
            Arc::new(
                Self {
                    shared: shared.clone(),
                    context,
                    owner,
                    backlog: core::sync::atomic::AtomicU32::new(0),
                    compute_backlog: core::sync::atomic::AtomicU32::new(0),
                    busy_site: core::sync::atomic::AtomicU32::new(0),
                    publishing: core::sync::atomic::AtomicBool::new(false),
                },
                GFP_KERNEL,
            )?,
            slot,
        ))
    }

    /// Builder rollback wakes other physical waiters, but must not make the
    /// same preparation retry unchanged capacity. Preserve external progress.
    fn notify_compute_attempt(&self, observed: Option<&mut u64>) {
        if let Some(observed) = observed {
            crate::g17::progress::note_change();
            self.shared.changed.notify_all();
            self.shared.physical_available.notify_own(observed);
        } else {
            self.shared.notify_changed();
        }
    }

    /// The shared aperture has exactly one builder. Other queues sleep without
    /// holding the device mutex; recovery joins its lease before closing roots.
    fn shared_compute(&self, nonblocking: bool, mut observed: Option<&mut u64>) -> Result {
        if matches!(self.shared.firmware_profile, super::FirmwareProfile::M5Sptm) { return Ok(()); }
        loop {
            let lease = if nonblocking { self.shared.preparations.try_enter()? } else { self.shared.preparations.enter(false)? };
            let dev = self.shared.drm()?;
            let uat = {
                let mut state = self.shared.state.lock();
                loop {
                    let firmware = (*state).as_deref_mut().ok_or(ENODEV)?;
                    if self
                        .shared
                        .crashed
                        .load(core::sync::atomic::Ordering::Acquire)
                    {
                        return Err(EIO);
                    }
                    if !lease.is_current() {
                        return Err(EAGAIN);
                    }
                    if firmware.init.has_compute_shared() {
                        return firmware.init.cache_compute_vm(self.context.vm());
                    }
                    if !firmware.queues.compute_building {
                        firmware.queues.compute_building = true;
                        break firmware.uat.clone();
                    }
                    if nonblocking { return Err(EBUSY); }
                    self.shared.changed.wait(&mut state);
                }
            };
            let alloc = super::object::Allocator {
                dev: &dev,
                uat: &uat,
            };
            let mut built = Some(super::initdata::ComputeShared::new(&alloc));
            let result = {
                let mut state = self.shared.state.lock();
                let firmware = (*state).as_deref_mut().ok_or(ENODEV)?;
                if !lease.is_current() {
                    Err(EAGAIN)
                } else {
                    built
                        .take()
                        .ok_or(EIO)?
                        .and_then(|built| firmware.init.install_compute_shared(built))
                        .and_then(|()| firmware.init.cache_compute_vm(self.context.vm()))
                }
            };
            drop(built);
            {
                let mut state = self.shared.state.lock();
                (*state)
                    .as_deref_mut()
                    .ok_or(ENODEV)?
                    .queues
                    .compute_building = false;
            }
            self.notify_compute_attempt(observed.as_deref_mut());
            drop(lease);
            if nonblocking || result != Err(EAGAIN) {
                return result;
            }
        }
    }

    /// Uses the queue builder's existing lease. Entering another closed epoch
    /// here would make recovery wait for the very builder blocked on reopening.
    fn compute_scheduler(
        &self,
        alloc: &super::object::Allocator<'_>,
        lease: &super::preparation::Lease,
        nonblocking: bool,
        observed: Option<&mut u64>,
    ) -> Result {
        {
            let mut state = self.shared.state.lock();
            loop {
                let firmware = (*state).as_deref_mut().ok_or(ENODEV)?;
                if !lease.is_current() {
                    return Err(EAGAIN);
                }
                if firmware.init.has_compute_scheduler() {
                    return Ok(());
                }
                if !firmware.queues.compute_scheduler_building {
                    firmware.queues.compute_scheduler_building = true;
                    break;
                }
                if nonblocking { return Err(EBUSY); }
                self.shared.changed.wait(&mut state);
            }
        }
        let mut built = Some(super::initdata::ComputeScheduler::new(alloc));
        let result = {
            let mut state = self.shared.state.lock();
            let firmware = (*state).as_deref_mut().ok_or(ENODEV)?;
            if !lease.is_current() {
                Err(EAGAIN)
            } else {
                built
                    .take()
                    .ok_or(EIO)?
                    .and_then(|built| firmware.init.install_compute_scheduler(built))
            }
        };
        drop(built);
        {
            let mut state = self.shared.state.lock();
            (*state)
                .as_deref_mut()
                .ok_or(ENODEV)?
                .queues
                .compute_scheduler_building = false;
        }
        self.notify_compute_attempt(observed);
        result
    }

    /// Stage larger backing only. The event worker publishes the old pool's
    /// release after all prepared and active work has drained.
    fn prepare_compute_growth(&self, required: super::fw::m5::pool::PoolGeometry) -> Result {
        let lease = self.shared.preparations.try_enter()?;
        let (uat, qid) = {
            let state = self.shared.state.lock();
            let firmware = (*state).as_deref().ok_or(ENODEV)?;
            let entry = firmware.queues.compute.iter().flatten()
                .find(|entry| entry.queue.as_ref().is_some_and(|q| q.owner() == Some(self.owner)))
                .ok_or(EBUSY)?;
            if entry.growth.is_some() { return Err(EBUSY); }
            if entry.queue.as_ref().ok_or(EIO)?.pool_fits(Some(required)) { return Ok(()); }
            (firmware.uat.clone(), entry.queue.as_ref().ok_or(EIO)?.qid())
        };
        let dev = self.shared.drm()?;
        let alloc = super::object::Allocator { dev: &dev, uat: &uat };
        let pages = super::freelist::Pages::compute(&alloc, true, Some(required))?;
        let alias = pages.map_client(self.context.vm())?;
        let mut growth = Some(ComputeGrowth { pages: Some(pages), alias: Some(alias) });
        let result = {
            let mut state = self.shared.state.lock();
            let firmware = (*state).as_deref_mut().ok_or(ENODEV)?;
            if !lease.is_current() { Err(EAGAIN) } else {
                let entry = firmware.queues.compute.iter_mut().flatten()
                    .find(|entry| entry.queue.as_ref().is_some_and(|q|
                        q.owner() == Some(self.owner) && q.qid() == qid))
                    .ok_or(EBUSY)?;
                if entry.growth.is_some() { Err(EBUSY) } else {
                    entry.growth = growth.take();
                    Ok(())
                }
            }
        };
        drop(growth);
        if result.is_ok() { self.shared.queue_events(); }
        result.and(Err(EBUSY))
    }

    pub(super) fn prepare_physical(&self, packet: &Arc<super::job::Packet>,
        preparation: &mut super::job::Preparation) -> Result<Option<super::availability::Wait>>
    {
        match packet.engine() {
            super::job::Engine::Compute => self.prepare_compute_physical(packet, preparation),
            super::job::Engine::Render => self.prepare_render_physical(packet, preparation),
        }
    }

    /// A capacity miss returns a scheduler dependency, never a timed wait in
    /// run_job. The packet's claim prevents an idle physical queue being stolen
    /// between this callback and its publication.
    fn prepare_compute_physical(&self, packet: &Arc<super::job::Packet>,
        preparation: &mut super::job::Preparation) -> Result<Option<super::availability::Wait>>
    {
        let required = match &packet.command {
            super::command::Validated::Compute { plan, .. } => plan.map(|plan| plan.geometry),
            _ => return Err(EINVAL),
        };
        if self.try_prepare_resident_compute(packet, preparation, required)? {
            return Ok(None);
        }
        loop {
            if packet.cancel_requested() { return Err(ECANCELED); }
            if packet.completion.status().get() != 0 { return Err(EIO); }
            let physical = self.shared.physical_available.clone();
            let mut physical_observed = physical.observe();
            let compute = self.shared.state.lock().as_ref().ok_or(ENODEV)?
                .queues.compute_available.clone();
            let compute_observed = compute.observe();
            let attempt = (|| {
                let lease = self.shared.preparations.try_enter()?;
                let ready = {
                    let state = self.shared.state.lock();
                    let firmware = (*state).as_deref().ok_or(ENODEV)?;
                    if !lease.is_current() || firmware.recovery.pending()
                        || super::recovery::Memory::recovery_state(&firmware.init)? != 0 {
                        return Err(EAGAIN);
                    }
                    firmware.queues.compute(self.owner).is_some_and(|queue| queue.pool_fits(required))
                };
                drop(lease);
                if !ready { self.build_compute(required, true, Some(&mut physical_observed))?; }
                let final_lease = self.shared.preparations.try_enter()?;
                let state = self.shared.state.lock();
                let firmware = (*state).as_deref().ok_or(ENODEV)?;
                if !final_lease.is_current() || self.shared.crashed.load(Ordering::Acquire)
                    || firmware.recovery.pending() { return Err(EAGAIN); }
                match firmware.queues.claim_compute(self.owner, &self.context, &preparation.compute) {
                    Ok(claim) => {
                        if claim.is_some() { preparation.compute = claim; }
                        Ok(true)
                    }
                    Err(error) if error == EBUSY => Ok(false),
                    Err(error) => Err(error),
                }
            })();
            let (domain, observed) = match attempt {
                Ok(true) => return self.prepare_compute_outer(packet, preparation),
                Ok(false) => (compute, compute_observed),
                Err(error) if matches!(error, EBUSY | EAGAIN | ENOSPC) => (physical, physical_observed),
                Err(error) => return Err(error),
            };
            if let Some(wait) = domain.wait(observed)? {
                self.shared.queue_events();
                return Ok(Some(wait));
            }
        }
    }

    /// Reserve a resident M5 queue and its outer credits in one transaction.
    /// Misses retain the existing preparation/wait path. In particular, a
    /// newly acquired physical claim stays with Preparation until the caller
    /// can release it outside the device mutex.
    fn try_prepare_resident_compute(&self, packet: &Arc<super::job::Packet>,
        preparation: &mut super::job::Preparation,
        required: Option<super::fw::m5::pool::PoolGeometry>) -> Result<bool>
    {
        if !matches!(self.shared.firmware_profile, super::FirmwareProfile::M5Sptm) {
            return Ok(false);
        }
        if packet.cancel_requested() { return Err(ECANCELED); }
        if packet.completion.status().get() != 0 { return Err(EIO); }
        let lease = match self.shared.preparations.try_enter() {
            Ok(lease) => lease,
            Err(error) if matches!(error, EBUSY | EAGAIN) => return Ok(false),
            Err(error) => return Err(error),
        };
        let mut state = self.shared.state.lock();
        let firmware = (*state).as_deref_mut().ok_or(ENODEV)?;
        if !lease.is_current() || self.shared.crashed.load(Ordering::Acquire)
            || firmware.recovery.pending()
            || super::recovery::Memory::recovery_state(&firmware.init)? != 0
        {
            return Ok(false);
        }
        self.prepare_resident_compute_locked(firmware, packet, preparation, required)
    }

    /// The caller holds the device mutex and a current preparation lease.
    /// Claims stay in Preparation so every release can occur after unlocking.
    fn prepare_resident_compute_locked(&self, firmware: &mut super::Firmware,
        packet: &Arc<super::job::Packet>, preparation: &mut super::job::Preparation,
        required: Option<super::fw::m5::pool::PoolGeometry>) -> Result<bool>
    {
        let Some(queue) = firmware.queues.compute(self.owner) else {
            return Ok(false);
        };
        if !queue.pool_fits(required) { return Ok(false); }
        let slots = queue.outer_slots();
        match firmware.queues.claim_compute(self.owner, &self.context, &preparation.compute) {
            Ok(Some(claim)) => preparation.compute = Some(claim),
            Ok(None) => {},
            Err(error) if error == EBUSY => return Ok(false),
            Err(error) => return Err(error),
        }
        if preparation.outer.is_none() {
            match firmware.init.reserve_outer(packet.context.policy().priority(), slots) {
                Ok(claim) => preparation.outer = Some(claim),
                Err(error) if matches!(error, EBUSY | EAGAIN) => return Ok(false),
                Err(error) => return Err(error),
            }
        }
        Ok(true)
    }

    /// The physical claim remains held while checking the shared outer ring.
    /// A miss returns its independent domain; backend rolls back new claims.
    fn prepare_compute_outer(&self, packet: &Arc<super::job::Packet>,
        preparation: &mut super::job::Preparation) -> Result<Option<super::availability::Wait>>
    {
        if !matches!(self.shared.firmware_profile, super::FirmwareProfile::M5Sptm) { return Ok(None); }
        if preparation.outer.is_some() { return Ok(None); }
        loop {
            if packet.cancel_requested() { return Err(ECANCELED); }
            let available = {
                let mut state = self.shared.state.lock();
                let firmware = (*state).as_deref_mut().ok_or(ENODEV)?;
                firmware.init.outer_available()?
            };
            let observed = available.observe();
            let result = {
                let mut state = self.shared.state.lock();
                let firmware = (*state).as_deref_mut().ok_or(ENODEV)?;
                let queue = firmware.queues.compute(self.owner).ok_or(EIO)?;
                let slots = queue.outer_slots();
                firmware.init.reserve_outer(packet.context.policy().priority(), slots)
            };
            match result {
                Ok(claim) => { preparation.outer = Some(claim); return Ok(None); }
                Err(error) if error == EBUSY || error == EAGAIN => {
                    if let Some(wait) = available.wait(observed)? {
                        self.shared.queue_events();
                        return Ok(Some(wait));
                    }
                }
                Err(error) => return Err(error),
            }
        }
    }

    fn grow_compute(&self, required: super::fw::m5::pool::PoolGeometry) -> Result {
        use kernel::time::{delay::fsleep, Delta};
        let lease = self.shared.preparations.enter(false)?;
        let dev = self.shared.drm()?;
        let uat = self.shared.state.lock().as_ref().ok_or(ENODEV)?.uat.clone();
        let alloc = super::object::Allocator { dev: &dev, uat: &uat };
        let mut pages = Some(super::freelist::Pages::compute(&alloc, true, Some(required))?);
        let mut alias = Some(pages.as_ref().ok_or(EIO)?.map_client(self.context.vm())?);
        let mut deferred = DeferredBatch::publication()?;
        let started = now_ns();
        loop {
            let result = (|| {
                let mut state = self.shared.state.lock();
                let firmware = (*state).as_deref_mut().ok_or(ENODEV)?;
                if !lease.is_current() { return Err(EAGAIN); }
                if self.shared.crashed.load(Ordering::Acquire) { return Err(EIO); }
                firmware.scan_compute(None, &mut deferred)?;
                let queue = firmware.queues.compute_mut(self.owner).ok_or(EIO)?;
                if queue.pool_fits(Some(required)) { return Ok(None); }
                if queue.begin_growth(&firmware.init)? {
                    fence(Ordering::SeqCst);
                    firmware.primary.notify(crate::g17::MSG_CONTROL_NOTIFY)?;
                }
                queue.finish_growth(&firmware.init, &mut pages, &mut alias).map(Some)
            })();
            deferred.finish();
            match result {
                Ok(old) => { drop(old); return Ok(()); }
                Err(error) if error == EAGAIN && lease.is_current()
                    && now_ns().saturating_sub(started) < WORK_RING_WAIT_NS => {
                        self.shared.queue_events();
                        fsleep(Delta::from_millis(1));
                    }
                Err(error) => return Err(error),
            }
        }
    }

    fn build_compute(&self, required: Option<super::fw::m5::pool::PoolGeometry>,
        nonblocking: bool, mut observed: Option<&mut u64>) -> Result {
        loop {
            match self.shared_compute(nonblocking, observed.as_deref_mut()) {
                Err(error) if error == EAGAIN && !nonblocking => continue,
                result => {
                    result?;
                    break;
                }
            }
        }
        let mut wait = None;
        loop {
            let lease = if nonblocking { self.shared.preparations.try_enter()? } else { self.shared.preparations.enter(false)? };
            let dev = self.shared.drm()?;
            let mut channel: Option<kick::Queue> = None;
            let (uat, objects, reservation) = {
                let mut state = self.shared.state.lock();
                let firmware = (*state).as_deref_mut().ok_or(ENODEV)?;
                if self
                    .shared
                    .crashed
                    .load(core::sync::atomic::Ordering::Acquire)
                {
                    return Err(EIO);
                }
                match firmware.queues.rebind_compute(self.owner, &self.context, required)? {
                    Rebind::Done => return Ok(()),
                    Rebind::Grow => {
                        drop(state);
                        return if nonblocking { self.prepare_compute_growth(required.ok_or(EINVAL)?) }
                                else { self.grow_compute(required.ok_or(EINVAL)?) };
                    }
                    Rebind::Rearm(qid) => (firmware.uat.clone(), firmware.pool.clone(), Err(qid)),
                    Rebind::Fresh => match firmware.queues.reserve_compute(self.owner) {
                        Ok(reservation) => {
                            channel = firmware.queues.qids.take_channel(reservation.id);
                            (firmware.uat.clone(), firmware.pool.clone(), Ok(reservation))
                        }
                        Err(error) if error == ENOSPC => {
                            // Native-like recycling first (vmfix6): a parked
                            // render pair of a released owner returns its QIDs.
                            if firmware.recycle_parked_render(None)? {
                                continue;
                            }
                            // Same-engine reuse next: an idle queue of another
                            // live owner. Otherwise wait, bounded, for any queue
                            // to retire or close; never a lost device for this.
                            if firmware.queues.reclaim_idle_compute(self.owner, required)?.is_some() {
                                continue;
                            }
                            let now = now_ns();
                            firmware.queues.compute_pressure_until = now + 50_000_000;
                            if nonblocking {
                                drop(state);
                                self.shared.queue_events();
                                return Err(EBUSY);
                            }
                            let since = *wait.get_or_insert(now);
                            // Native waits without a deadline for a queue; bound the wait
                            // like the identity wait (the scheduler timer is renewed).
                            if now.saturating_sub(since) >= COMPUTE_QUEUE_WAIT_NS {
                                pr_warn!("asahi: compute queue wait expired: owner={} waited_ms={}\n",
                                    self.owner, (now - since) / 1_000_000);
                                return Err(EBUSY);
                            }
                            if since == now {
                                static WAITS: core::sync::atomic::AtomicU64 =
                                    core::sync::atomic::AtomicU64::new(0);
                                let n = WAITS.fetch_add(1, Ordering::Relaxed);
                                if n < 8 || (n + 1).is_power_of_two() {
                                    pr_info!("asahi: compute queue wait: owner={} no QID or idle queue (wait {})\n",
                                        self.owner, n + 1);
                                }
                            }
                            drop(state);
                            drop(lease);
                            self.shared.queue_events();
                            kernel::time::delay::fsleep(kernel::time::Delta::from_millis(1));
                            continue;
                        }
                        Err(error) => return Err(error),
                    },
                }
            };
            let alloc = super::object::Allocator {
                dev: &dev,
                uat: &uat,
            };
            let reservation = match reservation {
                Ok(reservation) => reservation,
                Err(qid) => {
                    // Fresh zeroed backing is built without the device mutex; the
                    // queue may have been taken or re-armed by another owner since.
                    let pages = crate::g17::freelist::Pages::compute(&alloc,
                        matches!(self.shared.firmware_profile, super::FirmwareProfile::M5Sptm), required)?;
                    let ring = compute::DescriptorRing::new(&alloc, &objects,
                        matches!(self.shared.firmware_profile, super::FirmwareProfile::M5Sptm))?;
                    let mut state = self.shared.state.lock();
                    let firmware = (*state).as_deref_mut().ok_or(ENODEV)?;
                    if !lease.is_current() {
                        if nonblocking { return Err(EAGAIN); }
                        continue;
                    }
                    let descriptor = match firmware.queues.rearm_compute(qid, pages, ring) {
                        Err(error) if error == EAGAIN && !nonblocking => continue,
                        result => result?,
                    };
                    // The firmware consumed this slot's release before the backing
                    // was dropped; the row is rewritten before the queue is kicked.
                    firmware.init.install_compute_pool(descriptor)?;
                    firmware.queues.bind_rearmed(qid, self.owner, &self.context)?;
                    return Ok(());
                }
            };
            let built = (|| {
                let pool = compute::PoolBacking::new(&alloc, reservation.id,
                    matches!(self.shared.firmware_profile, super::FirmwareProfile::M5Sptm), required)?;
                {
                    let mut state = self.shared.state.lock();
                    let firmware = (*state).as_deref_mut().ok_or(ENODEV)?;
                    if !lease.is_current() {
                        return Err(EAGAIN);
                    }
                    firmware.queues.reserved(reservation)?;
                    let descriptor = pool.pool_descriptor()?;
                    firmware.init.install_compute_pool(descriptor)?;
                }
                let backing = compute::Backing::new(&alloc, pool, &mut channel)?;
                // The global scheduler is created after pool/kick backing and
                // retained even if this queue's later descriptor graph fails.
                if !matches!(self.shared.firmware_profile, super::FirmwareProfile::M5Sptm) {
                    self.compute_scheduler(&alloc, &lease, nonblocking, observed.as_deref_mut())?;
                }
                let queue = compute::Queue::new(&alloc, &objects, backing, self.owner, self.context.clone(),
                    matches!(self.shared.firmware_profile, super::FirmwareProfile::M5Sptm),
                    self.shared.physical_available.clone())?;
                Ok(KBox::new(queue, GFP_KERNEL)?)
            })();
            let (mut built, build_error) = match built {
                Ok(queue) => (Some(queue), None),
                Err(error) => (None, Some(error)),
            };
            let result = {
                let mut state = self.shared.state.lock();
                let firmware = (*state).as_deref_mut().ok_or(ENODEV)?;
                if !lease.is_current() {
                    Err(EAGAIN)
                } else if let Some(error) = build_error {
                    Err(error)
                } else {
                    let installed = firmware.queues.install_compute(reservation, &mut built);
                    if installed.is_ok() {
                        firmware.queues.adopted_installed(reservation.id);
                    }
                    installed
                }
            };
            // The old aliases must disappear before a waiting constructor can
            // reuse the QID and its queue-local free-list identity. Keep the
            // preparation lease until both mappings and reservation are gone.
            drop(built);
            if result.is_err() {
                let mut state = self.shared.state.lock();
                let firmware = (*state).as_mut().ok_or(ENODEV)?;
                // A spare ring not yet adopted returns to its QID; an adopted
                // ring lost with the failed queue poisons it (cancel_unpublished).
                if let Some(queue) = channel.take() {
                    firmware.queues.qids.return_channel(queue);
                }
                firmware.queues.cancel_compute(reservation)?;
                drop(state);
                self.notify_compute_attempt(observed.as_deref_mut());
            }
            drop(lease);
            if nonblocking || result != Err(EAGAIN) {
                return result;
            }
        }
    }
}

/// One device transaction can fail each retained kick once and then retire it.
/// The physical namespace bounds the batch independently of open logical queues.
/// Collection failure leaves the engine's Active owner intact for a stopped retry.
pub(super) struct DeferredBatch {
    values: KVVec<super::completion::Deferred>,
    /// Exited owners detached from idle retained compute queues and the backing
    /// of vacated pools: unmapping and freeing happen in `finish`, off-lock.
    bindings: KVVec<super::queue::compute::Binding>,
    vacated: KVVec<super::queue::compute::Vacated>,
    /// Backing of render pools vacated for evicted idle clients.
    render: KVVec<super::freelist::RenderBacking>,
    compute_notification: Option<Arc<super::availability::Availability>>,
    physical_notification: Option<Arc<super::availability::Availability>>,
    outer_notification: Option<Arc<super::availability::Availability>>,
}
impl DeferredBatch {
    const DEPTH: usize = if crate::hw::t8140::queues::RENDER_DEPTH as usize
        > crate::hw::t8140::queues::COMPUTE_DEPTH
    {
        crate::hw::t8140::queues::RENDER_DEPTH as usize
    } else {
        crate::hw::t8140::queues::COMPUTE_DEPTH
    };

    /// Reused by the sole event work item; no allocation on a completion pass.
    pub(super) fn worker() -> Result<Self> {
        Self::with_capacity(2 * QIDS * Self::DEPTH, QIDS)
    }
    /// One publication can quarantine only its selected physical queue/pair.
    /// VM-wide settlement is a separate transaction before publishing its error.
    fn publication() -> Result<Self> {
        Self::with_capacity(2 * Self::DEPTH + 1, 0)
    }
    fn with_capacity(capacity: usize, detached: usize) -> Result<Self> {
        Ok(Self {
            compute_notification: None,
            physical_notification: None,
            outer_notification: None,
            values: KVVec::with_capacity(capacity, GFP_KERNEL)?,
            bindings: KVVec::with_capacity(detached, GFP_KERNEL)?,
            vacated: KVVec::with_capacity(detached, GFP_KERNEL)?,
            render: KVVec::with_capacity(
                if detached != 0 { crate::hw::t8140::queues::RENDER_SLOTS } else { 0 },
                GFP_KERNEL,
            )?,
        })
    }
    /// Keeps the detached owner's unmapping and VM teardown out of the device
    /// mutex. Without reserved room the binding is dropped here instead.
    pub(super) fn defer_binding(&mut self, binding: super::queue::compute::Binding) {
        if let Err(binding) = self.bindings.push_within_capacity(binding) {
            drop(binding);
        }
    }
    /// Room for one more vacated render backing; the worker vacates no more.
    pub(super) fn render_backing_room(&self) -> bool {
        self.render.len() < self.render.capacity()
    }
    pub(super) fn defer_render_backing(&mut self, backing: super::freelist::RenderBacking) {
        if let Err(backing) = self.render.push_within_capacity(backing) {
            drop(backing);
        }
    }
    pub(super) fn defer_vacated(&mut self, vacated: super::queue::compute::Vacated) {
        if let Err(vacated) = self.vacated.push_within_capacity(vacated) {
            drop(vacated);
        }
    }
    /// A replay can publish its source and later fail and retire in this same
    /// transaction. Reserve all three events before changing queues, plus a
    /// full publication and source event for the caller.
    /// Healthy submissions keep the small publication collector unchanged.
    fn reserve_replays(&mut self, commands: usize) -> Result {
        self.values
            .reserve(3 * commands + 2 * Self::DEPTH + 1, GFP_KERNEL)?;
        Ok(())
    }
    pub(super) fn push(&mut self, value: super::completion::Deferred) -> Result {
        use super::completion::Deferred;
        // Publishers may acquire the device mutex as soon as this pass drops
        // it. Expose terminal VM status now, before off-lock fence callbacks.
        // Recovery-spared and cancelled commands never poison their VM.
        if let Deferred::FailedOwned(completion, error) = &value {
            if *error != ECANCELED && !completion.spared() {
                completion.status().record(*error);
            }
        }
        self.values.push_within_capacity(value).map_err(|_| EIO)?;
        // A later publisher can borrow this pair before finish signals its old
        // fence. Publish the exact completion's witness only after collection
        // succeeds; failure fences and rejected events are not retirement.
        if let Some(
            Deferred::Retired(completion, _)
            | Deferred::RetiredCompute(completion, _, _)
            | Deferred::Drained(completion, _)
            | Deferred::Release(completion),
        ) = self.values.last()
        {
            completion.note_retirement_queued();
        }
        match self.values.last() {
            Some(Deferred::RetiredCompute(completion, _, _))
            | Some(Deferred::Retired(completion, Ok(_)))
            | Some(Deferred::Drained(completion, Ok(_))) => {
                if let Some(source) = completion.source() { source.note_successful_retirement(); }
            }
            _ => {},
        }
        Ok(())
    }
    pub(super) fn notify_compute(&mut self, domain: &Arc<super::availability::Availability>) {
        self.compute_notification = Some(domain.clone());
    }
    pub(super) fn notify_physical(&mut self, domain: &Arc<super::availability::Availability>) {
        self.physical_notification = Some(domain.clone());
    }
    pub(super) fn notify_outer(&mut self, domain: &Arc<super::availability::Availability>) {
        self.outer_notification = Some(domain.clone());
    }
    /// Called only after releasing the device mutex.
    pub(super) fn finish(&mut self) {
        for value in self.values.drain_all() {
            value.finish();
        }
        for binding in self.bindings.drain_all() {
            drop(binding);
        }
        for vacated in self.vacated.drain_all() {
            drop(vacated);
        }
        for backing in self.render.drain_all() {
            drop(backing);
        }
        if let Some(domain) = self.compute_notification.take() { domain.notify(); }
        if let Some(domain) = self.physical_notification.take() { domain.notify(); }
        if let Some(domain) = self.outer_notification.take() { domain.notify(); }
    }
}

struct ComputeHost<'a> {
    regs: &'a crate::regs::Resources,
    m5: bool,
    init: &'a super::initdata::InitData,
    primary: &'a mut super::Coprocessor,
    effort: &'a mut super::power::Effort,
    qids: &'a mut kick::QueueIds,
    accounting: &'a mut qos::Accounting,
    generation: u64,
}
impl compute::Host for ComputeHost<'_> {
    fn next_compute_publication(&mut self) -> Result<u64> {
        self.qids.next_compute_publication()
    }
    fn epoch(&self) -> Result<(u64, u32)> {
        Ok((
            self.generation,
            super::recovery::Memory::recovery_state(self.init)?,
        ))
    }
    fn prepare_compute_shared(&mut self) -> Result {
        if self.m5 { Ok(()) } else { self.init.activate_compute() }
    }
    fn qos_publish(
        &mut self,
        owner: qos::Owner,
        scheduler: u64,
        policy: super::fw::queue::Policy,
    ) -> Result<qos::Publication> {
        self.accounting.publish(
            &qos::View::new(self.init.qos()?)?,
            owner,
            scheduler,
            policy,
            qos::clock,
        )
    }
    fn qos_cancel(&mut self, publication: qos::Publication) -> Result {
        self.accounting
            .cancel(&qos::View::new(self.init.qos()?)?, publication)
    }
    fn publish_qid(&mut self, id: kick::Id) -> Result {
        self.qids.publish(id)
    }
    fn publish_outer(&mut self, priority: u8, slot: &super::fw::channels::WorkSlot) -> Result {
        self.init.publish_work(priority, slot).map(|_| ())
    }
    fn notify(&mut self, message: u64) -> Result {
        if self.m5 && message >> 48 == 0x83 && message & 3 == 2 {
            if let Err(error) = self.init.note_m5_submission_history() {
                pr_warn!("M5 submit-history skipped: {:?}\n", error);
            }
        }
        self.primary.notify(message)
    }
    fn note_submission(&mut self) -> Result {
        self.effort.submit();
        super::Firmware::apply_shared_effort(self.init, self.effort, super::power::FULL_EFFORT)
    }
    fn submit_m5(&mut self, submission: &super::fw::m5::queue::HardwareSubmission) -> Result {
        super::lat::note(super::lat::KICK, 0);
        self.regs.submit_m5_work(submission)
    }
}

impl super::Firmware {
    fn publish_compute(
        &mut self,
        owner: u64,
        packet: Arc<super::job::Packet>,
        dependencies: &[super::fw::kick::KickDependency],
        deferred: &mut DeferredBatch,
    ) -> Result {
        if self.recovery.pending() {
            return Err(EAGAIN);
        }
        // No descriptor, accounting or inner producer changes precede this
        // capacity check. The same device lock covers queue.publish below.
        if !self
            .init
            .work_ready(packet.context.policy().priority(), DataMaster::Compute)?
        {
            return Err(EAGAIN);
        }
        let queues = &mut self.queues;
        let queue = queues
            .compute
            .iter_mut()
            .flatten()
            .filter_map(|entry| entry.queue.as_deref_mut())
            .find(|queue| queue.owner() == Some(owner))
            // Reassigned while idle (reclaim_idle_compute): the publisher
            // re-acquires a queue instead of failing the VM.
            .ok_or(ENOENT)?;
        let mut host = ComputeHost {
            regs: &self.bridge.regs,
            m5: matches!(self.primary.state.shared.firmware_profile, super::FirmwareProfile::M5Sptm),
            init: &self.init,
            primary: &mut self.primary,
            effort: &mut self.effort,
            qids: &mut queues.qids,
            accounting: &mut queues.accounting,
            generation: self.recovery.generation(),
        };
        queue.publish(packet, dependencies, &mut host, &mut |event| {
            deferred.push(event)
        })
    }
}

impl Registry {
    fn release_compute_witness(
        &mut self,
        init: &super::initdata::InitData,
        owner: u64,
        deferred: &mut DeferredBatch,
    ) -> Result<bool> {
        let Some(queue) = self
            .compute
            .iter_mut()
            .flatten()
            .filter_map(|entry| entry.queue.as_deref_mut())
            .find(|queue| queue.owner() == Some(owner))
        else {
            return Ok(false);
        };
        if !queue.spared_pending() {
            return Ok(false);
        }
        let view = qos::View::new(init.qos()?)?;
        queue.release_witnessed(
            super::recovery::Memory::recovery_state(init)?,
            |owner| self.accounting.complete(&view, &[owner]),
            &mut |event| deferred.push(event),
        )?;
        Ok(queue.spared_pending())
    }

    fn fail_compute_vm(
        &mut self,
        status: &Arc<super::status::VmStatus>,
        error: Error,
        deferred: &mut DeferredBatch,
    ) -> Result {
        for queue in self
            .compute
            .iter_mut()
            .flatten()
            .filter_map(|entry| entry.queue.as_deref_mut())
        {
            if queue.in_flight()
                && queue
                    .context()
                    .is_some_and(|context| Arc::ptr_eq(context.status(), status))
            {
                queue.quarantine(error, &mut |event| deferred.push(event))?;
            }
        }
        Ok(())
    }
}

impl super::Firmware {
    /// Recovery only: latch the exact completion record of one compute queue's
    /// in-flight commands without retiring or settling anything.
    pub(super) fn observe_compute_front(&mut self, qid: u8) -> Result<Option<bool>> {
        let mut masks = [0u64; 2];
        masks[qid as usize / 64] |= 1u64 << (qid % 64);
        let registry = &mut self.queues;
        registry.observations.clear();
        let mut requested = false;
        for queue in registry
            .compute
            .iter()
            .flatten()
            .filter_map(|entry| entry.queue.as_deref())
            .filter(|queue| queue.qid() == qid)
        {
            requested |= queue.request_observations(&mut registry.observations, Some(masks))?;
        }
        if requested {
            self.init
                .scan_compute_completions(|record| registry.observations.fold(record))?;
        }
        let mut observed = None;
        for queue in registry
            .compute
            .iter_mut()
            .flatten()
            .filter_map(|entry| entry.queue.as_deref_mut())
            .filter(|queue| queue.qid() == qid)
        {
            if requested {
                queue.latch_observations(&mut registry.observations, Some(masks))?;
            }
            observed = queue.front_observed();
        }
        registry.observations.clear();
        Ok(observed)
    }

    /// Latch one shared snapshot for all selected queues before settling any VM.
    /// A later queue's exact success in this batch wins over a peer's failure.
    fn scan_compute(&mut self, masks: Option<[u64; 2]>, deferred: &mut DeferredBatch) -> Result {
        let state = super::recovery::Memory::recovery_state(&self.init)?;
        let generation = self.recovery.generation();
        let registry = &mut self.queues;
        registry.observations.clear();
        let mut requested = false;
        for queue in registry
            .compute
            .iter()
            .flatten()
            .filter_map(|entry| entry.queue.as_deref())
        {
            requested |= queue.request_observations(&mut registry.observations, masks)?;
        }
        if requested {
            self.init
                .scan_compute_completions(|record| registry.observations.fold(record))?;
        }
        let view = qos::View::new(self.init.qos()?)?;
        for queue in registry
            .compute
            .iter_mut()
            .flatten()
            .filter_map(|entry| entry.queue.as_deref_mut())
        {
            let before = queue.preparation_slots();
            queue.latch_observations(&mut registry.observations, masks)?;
            queue.retire(
                masks,
                generation,
                state,
                |owner| registry.accounting.complete(&view, &[owner]),
                &mut |event| deferred.push(event),
            )?;
            if queue.preparation_slots() != before {
                deferred.notify_compute(&registry.compute_available);
                deferred.notify_physical(&self.primary.state.shared.physical_available);
            }
        }
        // All compute observations in this batch are latched before a VM
        // failure can settle its render peers. Exact successes win first.
        for index in 0..self.queues.compute.len() {
            let terminal = self.queues.compute[index]
                .as_mut()
                .and_then(|entry| entry.queue.as_deref_mut())
                .and_then(compute::Queue::take_terminal);
            if let Some((status, error)) = terminal {
                status.record(error);
                self.settle_render_vm(&status, error, deferred)?;
                self.queues.fail_compute_vm(&status, error, deferred)?;
            }
        }
        let view = qos::View::new(self.init.qos()?)?;
        let registry = &mut self.queues;
        for queue in registry
            .compute
            .iter_mut()
            .flatten()
            .filter_map(|entry| entry.queue.as_deref_mut())
        {
            let before = queue.preparation_slots();
            queue.release_witnessed(
                state,
                |owner| registry.accounting.complete(&view, &[owner]),
                &mut |event| deferred.push(event),
            )?;
            if queue.preparation_slots() != before {
                deferred.notify_compute(&registry.compute_available);
                deferred.notify_physical(&self.primary.state.shared.physical_available);
            }
        }
        Ok(())
    }
}

impl Backend {
    /// Out of line: keeps the graph build frame off the common publish path.
    #[inline(never)]
    fn rebind_reassigned_compute(&self, packet: &Arc<super::job::Packet>) -> Result {
        let required = match &packet.command {
            super::command::Validated::Compute { plan, .. } => plan.map(|p| p.geometry),
            _ => None,
        };
        static REBINDS: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);
        let n = REBINDS.fetch_add(1, Ordering::Relaxed) + 1;
        if n <= 8 || n.is_power_of_two() {
            pr_info!("asahi: compute owner={} rebinding after idle reassignment (rebind {})\n",
                self.owner, n);
        }
        self.ensure_compute(required)
    }

    fn ensure_compute(&self, required: Option<super::fw::m5::pool::PoolGeometry>) -> Result {
        use kernel::time::{delay::fsleep, Delta};
        const SPARED_WAIT_MS: usize = 200;
        let mut deferred = DeferredBatch::publication()?;
        for _ in 0..SPARED_WAIT_MS {
            // This lease preserves the builder's recovery exclusion while
            // checking an already-bound M5 queue under the witness lock.
            // Enter before the device lock: recovery joins with it dropped.
            let lease = if matches!(self.shared.firmware_profile, super::FirmwareProfile::M5Sptm)
            {
                Some(self.shared.preparations.enter(false)?)
            } else { None };
            let decision = (|| {
                let mut state = self.shared.state.lock();
                let firmware = (*state).as_deref_mut().ok_or(ENODEV)?;
                firmware.replay_compute(&mut deferred)?;
                let pending = firmware.queues.release_compute_witness(
                    &firmware.init, self.owner, &mut deferred)?;
                let ready = if !pending
                    && lease.as_ref().is_some_and(|lease| lease.is_current())
                    && !self.shared.crashed.load(Ordering::Acquire)
                {
                    // Match rebind_compute's existing-owner branch exactly.
                    // Missing, growing or mismatched queues retain the builder.
                    if let Some(queue) = firmware.queues.compute(self.owner) {
                        queue.matches(self.owner, queue.qid(), &self.context)?
                            && queue.pool_fits(required)
                    } else { false }
                } else { false };
                Ok::<_, Error>((pending, ready))
            })();
            drop(lease);
            deferred.finish();
            let (pending, ready) = decision?;
            if ready { return Ok(()); }
            if !pending {
                return self.build_compute(required, false, None);
            }
            // Other owners continue submitting while this owner gives its
            // queue-local drain witness time to arrive.
            fsleep(Delta::from_millis(1));
        }
        // No new command has been accepted. Keep the spared queue and its
        // retained work intact until a later admission observes its witness.
        Err(EBUSY)
    }

    /// Resolve and retain firmware sources in the same device transaction as publication.
    fn publish_compute_locked(&self, firmware: &mut super::Firmware,
        packet: &Arc<super::job::Packet>, deferred: &mut DeferredBatch,
        outer: &mut Option<super::initdata::OuterClaim>) -> Result<ComputePublication>
    {
        firmware.replay_compute(deferred)?;
        let mut dependencies = firmware.queues.render_prefix(self.owner, packet)?;
        let mut source_holds = Some(match firmware.queues.resolve_sources(
            packet, firmware.recovery.generation(), &mut dependencies)
        {
            Ok(holds) => holds,
            Err(error) if error == EAGAIN => return Ok(ComputePublication::SourcePending),
            Err(error) => return Err(error),
        });
        let result = {
            let _scope = outer.as_mut().map(|claim| firmware.init.outer_scope(claim)).transpose()?;
            firmware.publish_compute(
                self.owner,
                packet.clone(),
                dependencies.as_slice(),
                deferred,
            )
        };
        if packet.is_published() {
            packet.completion.retain_source_holds(source_holds.take().ok_or(EIO)?.into_holds());
        }
        drop(source_holds);
        Ok(ComputePublication::Attempted(result))
    }

    /// Called after taking this packet's publication claim. All waits and work
    /// state allocation precede the device mutex; only the checked epoch commits.
    fn compute_packet(
        &self,
        packet: &Arc<super::job::Packet>,
        deferred: &mut DeferredBatch,
        nonblocking: bool,
    ) -> Result {
        packet.check_dependencies()?;
        // Never acquire Packet::preparation while holding Shared::state.
        // The token outlives individual locked publication attempts, including
        // the cold queue's configuration-consumption handshake.
        let mut outer = packet.preparation.lock().outer.take();
        if matches!(self.shared.firmware_profile, super::FirmwareProfile::M5Sptm) && outer.is_none() {
            return Err(EINVAL);
        }
        let mut recovery_polls = 0;
        let mut outer_wait = None;
        let mut rebinds = 0;
        loop {
            if packet.completion.status().get() != 0 {
                return Err(EIO);
            }
            let lease = if nonblocking { self.shared.preparations.try_enter()? }
                        else { self.shared.preparations.enter(false)? };
            if packet.completion.work_state()?.word() == u32::MAX { return Err(EINVAL); }
            let mut state = self.shared.state.lock();
            let firmware = (*state).as_deref_mut().ok_or(ENODEV)?;
            if !lease.is_current() {
                drop(state);
                if nonblocking { return Err(EAGAIN); }
                continue;
            }
            if packet.cancel_requested() {
                return Err(ECANCELED);
            }
            if self
                .shared
                .crashed
                .load(core::sync::atomic::Ordering::Acquire)
            {
                return Err(EIO);
            }
            let recovery_state = super::recovery::Memory::recovery_state(&firmware.init)?;
            if firmware.recovery.pending() || recovery_state != 0 {
                if nonblocking { return Err(EAGAIN); }
                if recovery_polls == RECOVERY_SETTLE_POLLS {
                    return Err(EBUSY);
                }
                recovery_polls += 1;
                drop(state);
                drop(lease);
                self.shared.queue_events();
                kernel::time::delay::fsleep(kernel::time::Delta::from_millis(1));
                continue;
            }
            let result = match self.publish_compute_locked(firmware, packet, deferred, &mut outer)? {
                ComputePublication::Attempted(result) => result,
                ComputePublication::SourcePending if !nonblocking => {
                    drop(state);
                    drop(lease);
                    self.shared.queue_events();
                    kernel::time::delay::fsleep(kernel::time::Delta::from_millis(1));
                    continue;
                }
                ComputePublication::SourcePending => return Err(EAGAIN),
            };
            if matches!(result, Err(EBUSY) | Err(EAGAIN))
                && !packet.is_published()
                && (firmware.recovery.pending()
                    || super::recovery::Memory::recovery_state(&firmware.init)? != 0)
            {
                if nonblocking { return result; }
                if recovery_polls == RECOVERY_SETTLE_POLLS {
                    return Err(EBUSY);
                }
                recovery_polls += 1;
                drop(state);
                drop(lease);
                self.shared.queue_events();
                kernel::time::delay::fsleep(kernel::time::Delta::from_millis(1));
                continue;
            }
            if matches!(result, Err(ENOENT)) && !packet.is_published() {
                if nonblocking { return result; }
                // This owner's idle queue was reassigned after its submission
                // was accepted: bind a queue again (bounded inside), then retry.
                // Under QID pressure an idle queue can be reassigned again
                // while this packet rebinds; each attempt is bounded inside.
                if rebinds == 64 {
                    return Err(EBUSY);
                }
                rebinds += 1;
                drop(state);
                drop(lease);
                self.rebind_reassigned_compute(packet)?;
                continue;
            }
            if matches!(result, Err(EAGAIN)) && !packet.is_published() {
                if nonblocking { return result; }
                let now = now_ns();
                let since = *outer_wait.get_or_insert(now);
                if now.saturating_sub(since) >= COMPUTE_QUEUE_WAIT_NS {
                    pr_warn!("asahi: compute outer ring wait expired: owner={} waited_ms={}\n",
                        self.owner, (now - since) / 1_000_000);
                    return Err(EBUSY);
                }
                // Backpressure owns no firmware-visible attempt. Other queues
                // can submit and the event worker can retire while we sleep.
                drop(state);
                drop(lease);
                self.shared.queue_events();
                kernel::time::delay::fsleep(kernel::time::Delta::from_millis(1));
                continue;
            }
            return result;
        }
    }
}

impl super::Firmware {
    /// Both processors are stopped and their owner detached before this call.
    /// Error fences release their command mappings without the device mutex
    /// or an allocation-dependent collector.
    fn finish_stopped_queues(&mut self) {
        self.stop_render();
        for queue in self
            .queues
            .compute
            .iter_mut()
            .flatten()
            .filter_map(|entry| entry.queue.as_deref_mut())
        {
            let result = queue.stopped(&mut |event| {
                event.finish();
                Ok(())
            });
            if let Err(error) = result {
                dev_err!(
                    self.primary.state.shared.dev.as_ref(),
                    "Could not settle stopped compute queue: {:?}\n",
                    error
                );
            }
        }
        self.queues
            .admission
            .release_retained_owners_after_processor_stop();
    }
}

impl super::Shared {
    /// Joins construction and complete host teardown, including RTKit drops.
    /// The stop-only mutex outlives detached firmware destruction, so unbind
    /// cannot return while another stop caller still owns mailbox clients.
    /// Completion callbacks remain outside the device mutex. Direct handlers
    /// do not synchronously request or join stop; this path does not join
    /// scheduler workers.
    pub(super) fn stop_queues(&self) {
        self.crashed.store(true, Ordering::Release);
        self.admission_stop.notify();
        self.preparations.wait_drained();
        let _stopping = self.stop.lock();
        let firmware = {
            let mut state = self.state.lock();
            if let Some(firmware) = (*state).as_deref_mut() {
                firmware.secondary.stop();
                firmware.primary.stop();
            }
            state.take()
        };
        if let Some(mut firmware) = firmware {
            if self.firmware_test {
                // This diagnostic has no engine-retirement witness yet.
                core::mem::forget(firmware);
                return;
            }
            firmware.finish_stopped_queues();
            // RTKit destruction still dereferences the mailbox provider.
            drop(firmware);
        }
    }
}

/// Idle retained compute queues that keep their USC pool populated; older idle
/// pools give their backing back.
const COMPUTE_WARM_CAP: usize = 4;

impl super::Firmware {
    /// Only the event worker may publish staged growth's release controls.
    /// The queue retains its Context/VM while the old pool drains. Prepared
    /// commands must publish or cancel before its backing can change.
    fn service_compute_growth(&mut self, deferred: &mut DeferredBatch) -> Result {
        if self.recovery.pending()
            || super::recovery::Memory::recovery_state(&self.init)? != 0 { return Ok(()); }
        for slot in 0..self.queues.compute.len() {
            if self.queues.compute_prepared[slot].load(Ordering::Acquire) != 0 { continue; }
            let Some(entry) = self.queues.compute[slot].as_mut() else { continue; };
            let Some(growth) = entry.growth.as_mut() else { continue; };
            let queue = entry.queue.as_mut().ok_or(EIO)?;
            match queue.begin_growth(&self.init) {
                Ok(true) => {
                    fence(Ordering::SeqCst);
                    self.primary.notify(crate::g17::MSG_CONTROL_NOTIFY)?;
                }
                Ok(false) => {},
                Err(error) if error == EAGAIN => continue,
                Err(error) => return Err(error),
            }
            match queue.finish_growth(&self.init, &mut growth.pages, &mut growth.alias) {
                Ok(old) => {
                    entry.growth = None;
                    deferred.defer_vacated(old);
                    deferred.notify_compute(&self.queues.compute_available);
                    deferred.notify_physical(&self.primary.state.shared.physical_available);
                }
                Err(error) if error == EAGAIN => {},
                Err(error) => return Err(error),
            }
        }
        Ok(())
    }
    pub(super) fn compute_growth_pending(&self) -> bool {
        self.queues.compute.iter().flatten().any(|entry| entry.growth.is_some())
    }

    /// Event-worker service step: witness consumed pool releases, then release the
    /// oldest idle retained pools beyond the warm cap.
    /// Detached owner bindings and vacated pool backing go to `deferred`, which
    /// the worker finishes after releasing the device mutex.
    pub(in crate::g17) fn service_compute_pools(
        &mut self,
        deferred: &mut DeferredBatch,
    ) -> Result {
        self.service_compute_growth(deferred)?;
        let recovery_state = super::recovery::Memory::recovery_state(&self.init)?;
        for queue in self
            .queues
            .compute
            .iter_mut()
            .flatten()
            .filter_map(|entry| entry.queue.as_deref_mut())
        {
            queue.recycle_killed()?;
            queue.recycle_failed(recovery_state)?;
            if let Some(binding) = queue.detach_exited_owner() {
                deferred.defer_binding(binding);
            }
        }
        self.queues.observe_pool_releases(&self.init, deferred)?;
        if self.render_control_backpressured() {
            return Ok(());
        }
        let cap = if now_ns() < self.queues.compute_pressure_until { 0 } else { COMPUTE_WARM_CAP };
        let published = self.queues.vacate_idle_pools(&self.init, cap)?;
        if published != 0 {
            fence(Ordering::SeqCst);
            self.primary.notify(crate::g17::MSG_CONTROL_NOTIFY)?;
        }
        Ok(())
    }
}
