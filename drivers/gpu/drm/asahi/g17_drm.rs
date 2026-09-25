// SPDX-License-Identifier: GPL-2.0-only OR MIT

//! T8140 DRM frontend and userspace render queue.

use core::ops::Range;

use core::sync::atomic::{AtomicI32, AtomicU32, AtomicU64, Ordering};

use kernel::{
    bindings, c_str,
    dma_fence::{Fence, FenceContexts, FenceObject, FenceOps, RawDmaFence, UserFence},
    macros::vtable,
    new_condvar, new_mutex,
    prelude::*,
    static_lock_class,
    sync::{Arc, CondVar, Mutex},
    time::{delay::fsleep, Delta, Instant, Monotonic},
    uapi, xarray,
};

use crate::{
    alloc,
    driver::{AsahiDevRef, AsahiDevice},
    drm_gpu::{DrmGpu, DrmGpuParams},
    file, g17_job, g17_adt_j700, g17_completion, g17_live_boot, g17_perf, g17_render,
    g17_resources, g17_uapi, gem, gpu, hw, mmu, module_parameters, queue,
};

const T8140_USER_START: u64 = mmu::UAT_PGSZ as u64;
const T8140_USER_TOP: u64 = 1 << 42;
const T8140_UNKNOWN_PAGE: u64 = T8140_USER_TOP - 2 * mmu::UAT_PGSZ as u64;
const T8140_TIMESTAMP_ALIAS_RANGE: Range<u64> =
    crate::g17_queue_limits::CLIENT_LOW_VA_START..0x71_0000_0000;

/// Bounded opt-in render timing trace. `g17p_completion_trace` enables it and
/// `g17p_trace_depth` caps the number of records for the whole module lifetime.
static G17P_RENDER_TIMING_RECORDS: AtomicU32 = AtomicU32::new(0);
static G17P_RENDER_POLL_RECORDS: AtomicU32 = AtomicU32::new(0);

pub(crate) type SharedG17PRuntime = Arc<Mutex<g17_live_boot::G17PLiveRuntime>>;

/// Non-owning handle to the platform-owned G17P runtime.
///
/// The platform driver keeps the sole strong reference. DRM registration is
/// removed before that platform state is released, so adapter and queue
/// callbacks cannot outlive this pointer.
#[derive(Clone, Copy)]
pub(crate) struct G17PRuntimeRef {
    ptr: *const Mutex<g17_live_boot::G17PLiveRuntime>,
}

// SAFETY: the referenced mutex is shared by the platform-owned Arc, and all
// access is serialized through that mutex.
unsafe impl Send for G17PRuntimeRef {}
// SAFETY: see the Send implementation above.
unsafe impl Sync for G17PRuntimeRef {}

impl G17PRuntimeRef {
    pub(crate) fn new(runtime: &SharedG17PRuntime) -> Self {
        Self {
            ptr: Arc::as_ptr(runtime),
        }
    }

    pub(crate) fn mutex(&self) -> &Mutex<g17_live_boot::G17PLiveRuntime> {
        // SAFETY: AsahiDriver owns the runtime Arc for the complete registered
        // DRM lifetime. DRM teardown prevents new callbacks before dropping
        // that platform-owned Arc.
        unsafe { &*self.ptr }
    }

    pub(crate) fn service_primary_firmware_events(&self) -> Result {
        self.mutex().lock().service_primary_firmware_events()
    }

    pub(crate) fn service_primary_recovery(&self) -> Result {
        self.mutex().lock().service_primary_recovery()
    }

    pub(crate) fn primary_recovery_pending(&self) -> bool {
        self.mutex().lock().primary_recovery_pending()
    }

    pub(crate) fn primary_firmware_event_pending(&self) -> bool {
        self.mutex().lock().primary_firmware_event_pending()
    }

    pub(crate) fn primary_device_control_backpressured(&self) -> bool {
        self.mutex().lock().primary_device_control_backpressured()
    }

    /// Give the hardware compute graph back when a client queue goes away.
    fn release_compute_owner(&self, owner: u64) {
        self.mutex().lock().release_compute_owner(owner);
    }

}

/// Ceiling on live DRM compute queue objects for this device.
///
/// Bound logical queue objects before allocating roots and scheduler state.
/// Engine channels lazily acquire QIDs from the shared 128-entry table;
/// this logical-object limit does not describe execution concurrency.
const G17P_MAX_COMPUTE_QUEUES: u32 = crate::g17_queue_limits::MAX_QUEUES;

/// Admission leases for independently owned queue graphs.
///
/// Compute installation is exclusive against existing compute/render leases;
/// steady-state compute submissions overlap one another and independent renders.
/// Each submission retains its lease through retirement or quarantine. The
/// runtime mutex serializes host publication, not asynchronous GPU execution.
/// This policy does not establish the capacity of a hardware execution resource:
/// a recovery source mask or a passing client count is not such a contract.
#[pin_data]
pub(crate) struct G17PQueueRegistry {
    /// Stable logical render ownership and transient compute installation.
    /// Steady-state compute readers do not occupy render slots.
    #[pin]
    lease: Mutex<G17PQueueLeaseState>,
    #[pin]
    lease_free: CondVar,
    live_queues: AtomicU32,
    compute_readers: AtomicU32,
    compute_faulted: AtomicU32,
    /// Device-wide throughput accounting.
    ///
    /// Each update holds this mutex for integer accounting only. Disabled
    /// entirely when `g17p_perf_window` is zero.
    #[pin]
    perf: Mutex<G17PPerfState>,
}

/// The open reporting window and the instant it opened.
///
/// The kernel's `Instant` keeps its absolute nanoseconds `pub(crate)` to the
/// `kernel` crate, so the opening instant is held here and only the elapsed
/// span reaches `g17_perf`.
struct G17PPerfState {
    window: g17_perf::SubmitWindow,
    opened: Instant<Monotonic>,
}

const G17P_RENDER_SLOTS: usize =
    g17_resources::G17PRenderQueuePair::SLOT_COUNT as usize;
/// Host owner slots are independent of firmware QIDs. Slot zero retains the
/// bootstrap channels; other owners lazily lease IDs from the shared pool.
const G17P_ACTIVE_RENDER_SLOTS: usize = G17P_RENDER_SLOTS;
const _: () = assert!(G17P_ACTIVE_RENDER_SLOTS > 0);
const _: () = assert!(G17P_ACTIVE_RENDER_SLOTS <= G17P_RENDER_SLOTS);

struct G17PQueueLeaseState {
    render_slots: u64,
    render_refs: [u8; G17P_RENDER_SLOTS],
    /// A first installer has reserved the next empty admission interval.
    /// Existing render owners may finish publication/retirement, but no new
    /// render or steady-compute lease may enter and steal that interval.
    compute_install_pending: bool,
    compute_install: bool,
    /// Opt-in qualification: a render owner is draining compute or publishing
    /// a graph/context/PB transition. Not an execution-width limit.
    render_transition: bool,
    burst: G17PComputeBurstState,
    /// Logical queue that owns each render slot's retained storage, or 0 for a
    /// slot no client has claimed yet.
    ///
    /// Storage, descriptors, parameter buffers and the application root remain
    /// attached to that logical owner even while it has no active submission.
    render_owner: [u64; G17P_RENDER_SLOTS],
    /// Submission sequence for diagnostics. Zero means never acquired.
    render_epoch: [u64; G17P_RENDER_SLOTS],
    /// Slots whose retained graph is known to be unusable: a render abandoned
    /// after a completion timeout leaves firmware potentially still running the
    /// group, and its host-side tracker set, so every later submission on that
    /// slot fails. Retiring the slot costs concurrency; continuing to hand it
    /// out costs the session, because each client that lands on it is told
    /// EBUSY and reports VK_ERROR_DEVICE_LOST.
    render_quarantined: u64,
    render_released: u64,
    epoch: u64,
}

impl Default for G17PQueueLeaseState {
    fn default() -> Self {
        Self { render_slots: 0, render_refs: [0; G17P_RENDER_SLOTS], compute_install_pending: false,
            compute_install: false, render_transition: false,
            burst: G17PComputeBurstState::default(), render_owner: [0; G17P_RENDER_SLOTS],
            render_epoch: [0; G17P_RENDER_SLOTS], render_quarantined: 0,
            render_released: 0, epoch: 0 }
    }
}

/// Diagnostic rendezvous only. No participant owns a compute/render lease.
#[derive(Default)]
struct G17PComputeBurstState {
    generation: u32,
    observed: u32,
    owners: [u64; 3],
    arrived: u8,
    departed: u8,
    install_intervened: bool,
    render_epoch: u64,
    started: Option<Instant<Monotonic>>,
    released: Option<Instant<Monotonic>>,
    outcome: u8,
}

struct G17PComputeBurstTicket {
    cohort: u32,
    parties: u8,
    /// 0=full cohort, 1=timeout, 2=signal, 3=render/install, 4=fault,
    /// 5=observation only. An aborted cohort is not a successful burst.
    outcome: u8,
    wait_ns: u64,
    released: Instant<Monotonic>,
    render_epoch: u64,
    compute_readers: u32,
}

impl G17PQueueLeaseState {
    const fn render_mask() -> u64 {
        // Slot zero is the boot channel owner, not a userspace lease.
        (u64::MAX >> (64 - G17P_ACTIVE_RENDER_SLOTS)) & !1
    }

    /// True once no render slot can ever be handed out again.
    fn render_exhausted(&self) -> bool {
        self.render_quarantined & Self::render_mask() == Self::render_mask()
    }

    fn take_render_slot(&mut self, slot: u8, owner: u64) -> u8 {
        let bit = 1u64 << slot;
        // A B1 physical TA/3D queue pair admits one render at a time. Logical
        // multi-flight is provided by selecting another retained pair, matching
        // AGXCommandQueue's array of AGX3DWorkQueues, rather than by publishing
        // a second command onto the same QIDs.
        debug_assert!(self.render_refs[slot as usize] == 0);
        self.render_refs[slot as usize] += 1;
        self.render_slots |= bit;
        self.render_released &= !bit;
        self.render_owner[slot as usize] = owner;
        self.epoch = self.epoch.wrapping_add(1);
        self.render_epoch[slot as usize] = self.epoch;
        slot
    }

    /// Keep live owners stable while spreading one logical queue's bounded
    /// multi-flight window across distinct physical queue pairs. A different
    /// owner may take only a graph whose exact prior owner has closed and whose
    /// last completion released the physical slot. The graph and QID pair stay
    /// allocated; the manager retains outgoing mappings through handoff.
    fn acquire_render_slot(&mut self, owner: u64) -> Option<u8> {
        if owner == 0 { return None; }

        // Reuse an idle retained work queue first. An active pair is never a
        // candidate: B1 consumed the second same-QID publication but did not
        // re-admit it after the predecessor, despite satisfied dependencies.
        for slot in 0..G17P_RENDER_SLOTS {
            let bit = 1u64 << slot;
            if self.render_owner[slot] == owner
                && self.render_refs[slot] == 0
                && self.render_quarantined & bit == 0
            {
                return Some(self.take_render_slot(slot as u8, owner));
            }
        }

        // Bound each logical queue to the native configured array of two
        // independent physical pairs. Per-pair command-generation depth is a
        // separate retirement property and must not cap the live-pair count.
        let owned = self.render_owner.iter().filter(|id| **id == owner).count();
        if owned >= crate::g17_queue_limits::MAX_IN_FLIGHT_PER_QUEUE as usize {
            return None;
        }

        let free = (!self.render_slots) & Self::render_mask() & !self.render_quarantined;
        if free == 0 {
            return None;
        }
        let usable = |slot: usize| free & (1u64 << slot) != 0;
        // Reuse an exactly retired, closed owner before consuming another
        // graph/root lease. Fresh-first allocation pins all 58 app contexts
        // before the 63-slot render namespace fills, so QUEUE_CREATE fails
        // before the old fallback reuse branch can ever be reached.
        // Idle live owners and closed-but-active slots remain ineligible.
        if let Some(slot) = (0..G17P_RENDER_SLOTS)
            .filter(|slot| usable(*slot) && self.render_released & (1u64 << slot) != 0)
            .min_by_key(|slot| self.render_epoch[*slot])
        {
            return Some(self.take_render_slot(slot as u8, owner));
        }
        (0..G17P_RENDER_SLOTS)
            .find(|slot| usable(*slot) && self.render_owner[*slot] == 0)
            .map(|slot| self.take_render_slot(slot as u8, owner))
    }

    fn release_render_slot(&mut self, slot: u8) {
        debug_assert!(slot < g17_resources::G17PRenderQueuePair::SLOT_COUNT);
        let refs = &mut self.render_refs[slot as usize];
        debug_assert!(*refs != 0);
        if *refs != 0 {
            *refs -= 1;
        }
        if *refs == 0 {
            self.render_slots &= !(1u64 << slot);
        }
    }

}

impl G17PQueueRegistry {
    fn new() -> Result<Arc<Self>> {
        Arc::pin_init(
            pin_init!(G17PQueueRegistry {
                lease <- new_mutex!(G17PQueueLeaseState::default(), "G17PQueueRegistry::lease"),
                lease_free <- new_condvar!("G17PQueueRegistry::lease_free"),
                live_queues: AtomicU32::new(0),
                compute_readers: AtomicU32::new(0),
                compute_faulted: AtomicU32::new(0),
                perf <- new_mutex!(
                    G17PPerfState {
                        window: g17_perf::SubmitWindow::default(),
                        opened: Instant::<Monotonic>::now(),
                    },
                    "G17PQueueRegistry::perf"
                ),
            }),
            GFP_KERNEL,
        )
    }

    /// Gather three distinct already-installed logical queues without a runtime
    /// mutex or hardware lease. Timeout releases partial cohorts unchanged;
    /// signals cancel only their own submission and release the peers.
    fn wait_compute_burst(
        &self, owner: u64, first_install: bool,
    ) -> Result<Option<G17PComputeBurstTicket>> {
        let mode = *module_parameters::g17p_compute_burst_probe.value();
        if first_install || mode == 0 || mode > 2 || owner == 0 {
            return Ok(None);
        }
        let entered = Instant::<Monotonic>::now();
        let mut held = self.lease.lock();
        if mode == 1 {
            if held.burst.observed >= 192 { return Ok(None); }
            held.burst.observed += 1;
            return Ok(Some(G17PComputeBurstTicket {
                cohort: 0, parties: 1, outcome: 5, wait_ns: 0,
                released: entered, render_epoch: held.epoch,
                compute_readers: self.compute_readers.load(Ordering::Acquire),
            }));
        }
        if held.burst.generation >= 64 || held.burst.released.is_some()
            || held.burst.owners.contains(&owner)
            || held.render_slots != 0 || held.compute_install_pending
            || held.compute_install || held.render_transition
        {
            return Ok(None);
        }
        if self.compute_faulted.load(Ordering::Acquire) != 0 { return Err(EIO); }
        if held.burst.arrived == 0 {
            held.burst.started = Some(entered);
            held.burst.render_epoch = held.epoch;
        }
        let index = usize::from(held.burst.arrived);
        held.burst.owners[index] = owner;
        held.burst.arrived += 1;
        let mut interrupted = false;
        let mut notify = false;
        loop {
            if held.burst.released.is_some() { break; }
            let elapsed = held.burst.started.as_ref().ok_or(EIO)?
                .elapsed().as_nanos().max(0) as u64;
            let outcome = if interrupted { Some(2) }
                else if self.compute_faulted.load(Ordering::Acquire) != 0 { Some(4) }
                else if held.burst.install_intervened
                    || held.epoch != held.burst.render_epoch || held.render_slots != 0
                    || held.compute_install_pending || held.compute_install
                    || held.render_transition { Some(3) }
                else if elapsed >= 20_000_000 { Some(1) }
                else if held.burst.arrived == 3 { Some(0) }
                else { None };
            if let Some(outcome) = outcome {
                held.burst.outcome = outcome;
                held.burst.released = Some(Instant::<Monotonic>::now());
                notify = true;
                break;
            }
            let remaining_ms = (20_000_000 - elapsed).div_ceil(1_000_000) as u32;
            if matches!(self.lease_free.wait_interruptible_timeout(
                &mut held, kernel::time::msecs_to_jiffies(remaining_ms),
            ), kernel::sync::CondVarTimeoutResult::Signal { .. }) {
                interrupted = true;
            }
        }
        let ticket = G17PComputeBurstTicket {
            cohort: held.burst.generation + 1,
            parties: held.burst.arrived,
            outcome: held.burst.outcome,
            wait_ns: entered.elapsed().as_nanos().max(0) as u64,
            released: held.burst.released.ok_or(EIO)?,
            render_epoch: held.burst.render_epoch,
            compute_readers: self.compute_readers.load(Ordering::Acquire),
        };
        held.burst.departed += 1;
        if held.burst.departed == held.burst.arrived {
            let generation = held.burst.generation + 1;
            held.burst = G17PComputeBurstState { generation, ..Default::default() };
        }
        drop(held);
        if notify { self.lease_free.notify_all(); }
        if interrupted { return Err(ERESTARTSYS); }
        Ok(Some(ticket))
    }

    /// Independent compute jobs share the compute side of the gate. Only first
    /// installation excludes render; repeat work may overlap render slots.
    ///
    /// An installer must publish its writer intent before waiting for existing
    /// readers/render leases to drain.  Claiming `compute_install` only after
    /// observing an empty interval lets a continuous render stream admit a new
    /// lease on every wakeup and starve the installer indefinitely.
    ///
    /// The guard lives through exact retirement and does not own graph lifetime.
    fn acquire_compute(self: &Arc<Self>, first_install: bool) -> Result<G17PComputeLeaseGuard> {
        let mut held = self.lease.lock();
        while held.compute_install_pending || held.compute_install || held.render_transition {
            if self.compute_faulted.load(Ordering::Acquire) != 0 { return Err(EIO); }
            if self.lease_free.wait_interruptible(&mut held) { return Err(ERESTARTSYS); }
        }
        if self.compute_faulted.load(Ordering::Acquire) != 0 { return Err(EIO); }
        if first_install {
            // A quarantined render deliberately retains its active lease until
            // both processors stop. It can never satisfy this drain, so do not
            // publish writer intent and globally strand otherwise healthy work.
            if held.render_slots & held.render_quarantined != 0 {
                return Err(EIO);
            }
            // Writer intent is visible to acquire_lease() and steady compute
            // readers before we sleep. Existing guards can still finish their
            // transition and retire; no new guard can steal the empty interval.
            // Keep intent separate from active installation so a render which
            // already owns a lease can complete its transition without cycling
            // against this drain.
            held.compute_install_pending = true;
            // Even an installer which retires before gatherers wake makes
            // this cohort unsuitable for the steady-only experiment.
            if held.burst.arrived != 0 { held.burst.install_intervened = true; }
            while held.render_slots != 0
                || self.compute_readers.load(Ordering::Acquire) != 0
                || held.render_transition
            {
                if self.compute_faulted.load(Ordering::Acquire) != 0
                    || held.render_slots & held.render_quarantined != 0
                {
                    held.compute_install_pending = false;
                    drop(held);
                    self.lease_free.notify_all();
                    return Err(EIO);
                }
                if self.lease_free.wait_interruptible(&mut held) {
                    held.compute_install_pending = false;
                    drop(held);
                    self.lease_free.notify_all();
                    return Err(ERESTARTSYS);
                }
            }
            if self.compute_faulted.load(Ordering::Acquire) != 0 {
                held.compute_install_pending = false;
                drop(held);
                self.lease_free.notify_all();
                return Err(EIO);
            }
            held.compute_install_pending = false;
            held.compute_install = true;
        }
        self.compute_readers.fetch_add(1, Ordering::AcqRel);
        drop(held);
        Ok(G17PComputeLeaseGuard { registry: self.clone(), exclusive: first_install })
    }

    /// Caller already owns this render slot and has waited all input fences.
    /// Never call while holding the runtime mutex: retiring compute jobs need
    /// that mutex and their lease Drop needs this registry mutex.
    fn acquire_render_transition(
        self: &Arc<Self>, slot: u8,
    ) -> Result<G17PRenderTransitionGuard> {
        let started = Instant::<Monotonic>::now();
        {
            let mut held = self.lease.lock();
            while held.render_transition {
                if self.compute_faulted.load(Ordering::Acquire) != 0 { return Err(EIO); }
                if self.lease_free.wait_interruptible(&mut held) { return Err(ERESTARTSYS); }
            }
            if self.compute_faulted.load(Ordering::Acquire) != 0 { return Err(EIO); }
            if slot >= G17P_RENDER_SLOTS as u8
                || held.render_slots & (1u64 << slot) == 0
                || held.compute_install
            {
                return Err(EBUSY);
            }
            // Publish writer intent BEFORE draining readers. A compute
            // installer waiting on our render slot has not claimed install;
            // it remains blocked, so there is no slot/install lock cycle.
            held.render_transition = true;
        }
        // Construct before reacquiring the lock: interruption/fault drops the
        // inner lock before this guard rolls back writer intent and wakes peers.
        let transition = G17PRenderTransitionGuard {
            registry: self.clone(), slot, started,
        };
        let mut held = self.lease.lock();
        while self.compute_readers.load(Ordering::Acquire) != 0 {
            if self.compute_faulted.load(Ordering::Acquire) != 0 { return Err(EIO); }
            if self.lease_free.wait_interruptible(&mut held) { return Err(ERESTARTSYS); }
        }
        if self.compute_faulted.load(Ordering::Acquire) != 0 { return Err(EIO); }
        drop(held);
        pr_info!("G17P render transition probe: acquired slot={} drain-ns={}\n",
            slot, transition.started.elapsed().as_nanos().max(0));
        Ok(transition)
    }

    /// Fold one finished render into the open window.
    ///
    /// Returns a report only on the submission that fills the window; the
    /// caller prints it with this lock released.
    fn record_render(&self, sample: g17_perf::RenderSample) -> Option<g17_perf::SubmitReport> {
        let size = *module_parameters::g17p_perf_window.value();
        if size == 0 {
            return None;
        }
        let mut state = self.perf.lock();
        if !state.window.is_open() {
            state.window.open();
            state.opened = Instant::<Monotonic>::now();
        }
        state.window.record_render(sample);
        Self::close_full_window(&mut state, size)
    }

    /// Fold one queued asynchronous compute into the open window.
    ///
    /// Only the ioctl cost is recorded: the job has not run when the ioctl
    /// returns, by design, so there is no GPU interval to attribute to it here.
    fn record_compute(&self, ioctl_ns: u64) -> Option<g17_perf::SubmitReport> {
        let size = *module_parameters::g17p_perf_window.value();
        if size == 0 {
            return None;
        }
        let mut state = self.perf.lock();
        if !state.window.is_open() {
            state.window.open();
            state.opened = Instant::<Monotonic>::now();
        }
        state.window.record_compute(ioctl_ns);
        Self::close_full_window(&mut state, size)
    }

    fn close_full_window(
        state: &mut G17PPerfState,
        size: u32,
    ) -> Option<g17_perf::SubmitReport> {
        if state.window.submissions() < size {
            return None;
        }
        let wall_ns = state.opened.elapsed().as_nanos().max(0) as u64;
        Some(state.window.close(wall_ns))
    }

    /// Reserve one queue slot, or report a full device.
    fn reserve_queue(&self) -> Result {
        let mut live = self.live_queues.load(Ordering::Acquire);
        loop {
            if live >= G17P_MAX_COMPUTE_QUEUES {
                return Err(ENOSPC);
            }
            match self.live_queues.compare_exchange(
                live,
                live + 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return Ok(()),
                Err(observed) => live = observed,
            }
        }
    }

    fn release_queue(&self) {
        self.live_queues.fetch_sub(1, Ordering::AcqRel);
    }

    fn release_render_owner(&self, owner: u64) {
        let mut held = self.lease.lock();
        for slot in 0..G17P_RENDER_SLOTS {
            if held.render_owner[slot] == owner {
                held.render_released |= 1u64 << slot;
            }
        }
        drop(held);
        self.lease_free.notify_all();
    }

    pub(crate) fn release_retained_owners_after_processor_stop(&self) {
        let mut held = self.lease.lock();
        let released = held.render_released & !held.render_slots;
        for slot in 1..G17P_RENDER_SLOTS {
            if released & (1u64 << slot) != 0 {
                held.render_owner[slot] = 0;
                held.render_epoch[slot] = 0;
            }
        }
        held.render_released &= !released;
        held.render_quarantined &= !released;
        self.lease_free.notify_all();
    }

    /// Select one idle retained work queue from this logical queue's bounded
    /// set, allocating another physical pair when its other render is active.
    ///
    /// `owner` is the logical queue id. Retained descriptors name that queue's
    /// independently allocated application-GART descriptor context, so two
    /// queues in one VM cannot share one graph. Waiting here is interruptible
    /// for temporary contention. Exhaustion of retained owners
    /// returns ENOSPC because waiting cannot reclaim an installed channel.
    fn acquire_lease(self: &Arc<Self>, owner: u64) -> Result<G17PLeaseGuard> {
        if owner == 0 { return Err(EINVAL); }
        let mut held = self.lease.lock();
        let slot = loop {
            if self.compute_faulted.load(Ordering::Acquire) != 0 {
                return Err(EIO);
            }
            // Every slot retired: no wait can ever succeed, so report the dead
            // device instead of sleeping forever.
            if held.render_exhausted() {
                return Err(EIO);
            }
            match held.render_owner.iter().position(|id| *id == owner) {
                Some(slot) if held.render_quarantined & (1u64 << slot) != 0 => return Err(EIO),
                None if !held.render_owner[1..].contains(&0)
                    && (held.render_released
                        & !held.render_slots
                        & !held.render_quarantined
                        & G17PQueueLeaseState::render_mask()) == 0
                    => return Err(ENOSPC),
                _ => {},
            }
            if !held.compute_install_pending && !held.compute_install {
                if let Some(slot) = held.acquire_render_slot(owner) {
                    break slot;
                }
            }
            if self.lease_free.wait_interruptible(&mut held) {
                return Err(ERESTARTSYS);
            }
        };
        core::mem::drop(held);
        Ok(G17PLeaseGuard {
            registry: self.clone(),
            slot,
        })
    }

    /// Retire one render slot whose retained graph can no longer be trusted.
    ///
    /// Called after a completion timeout has abandoned a submission. Nothing is
    /// freed -- the firmware may still own the abandoned group -- but no client
    /// is handed this slot again for the rest of the session.
    fn quarantine_render_slot(&self, slot: u8) {
        let mut held = self.lease.lock();
        if held.render_quarantined & (1u64 << slot) != 0 {
            return;
        }
        held.render_quarantined |= 1u64 << slot;
        let remaining = (G17PQueueLeaseState::render_mask() & !held.render_quarantined)
            .count_ones();
        drop(held);
        pr_warn!(
            "G17P render: retiring slot {} after an abandoned submission; {} render slot(s) left\n",
            slot,
            remaining,
        );
        self.lease_free.notify_all();
    }

    /// Make one quarantined slot eligible after both firmware CPUs stop.
    ///
    /// The completion still owns its lease while this bit is cleared, so no
    /// waiter can acquire the slot until that guard drops immediately after
    /// this call. Both processors have relinquished the abandoned group, so it
    /// can no longer justify a permanent capacity loss in the next session.
    fn rehabilitate_render_slot_after_processor_stop(&self, slot: u8) {
        debug_assert!(slot < g17_resources::G17PRenderQueuePair::SLOT_COUNT);
        let mut held = self.lease.lock();
        held.render_quarantined &= !(1u64 << slot);
    }
}

pub(crate) struct G17PComputeLeaseGuard { registry: Arc<G17PQueueRegistry>, exclusive: bool }
impl G17PComputeLeaseGuard {
    /// Fault-only coherent host admission state. These are leases, not a
    /// hardware execution-slot count: admitted work may still be queued, and
    /// quarantined work retains its lease after userspace sees an error.
    pub(crate) fn admission_snapshot(&self) -> (u32, u64, bool, bool, u64) {
        let held = self.registry.lease.lock();
        (
            self.registry.compute_readers.load(Ordering::Acquire),
            held.render_slots,
            held.compute_install_pending || held.compute_install,
            held.render_transition,
            held.render_quarantined,
        )
    }

    pub(crate) fn quarantine(&self) {
        let held = self.registry.lease.lock();
        self.registry.compute_faulted.store(1, Ordering::Release);
        drop(held);
        self.registry.lease_free.notify_all();
    }

    /// Clear the device-wide admission poison only after both firmware
    /// processors stop. The runtime still owns this guard at that boundary,
    /// so the registry cannot lose the recovery transition when the active
    /// queue graph is dropped immediately afterward.
    pub(crate) fn rehabilitate_after_processor_stop(&self) {
        let held = self.registry.lease.lock();
        self.registry.compute_faulted.store(0, Ordering::Release);
        drop(held);
        self.registry.lease_free.notify_all();
    }
}

/// Qualification-only host mutation interval, separate from render execution.
/// Drop after host publication returns, including every prepare/publish failure.
struct G17PRenderTransitionGuard {
    registry: Arc<G17PQueueRegistry>,
    slot: u8,
    started: Instant<Monotonic>,
}

impl Drop for G17PRenderTransitionGuard {
    fn drop(&mut self) {
        let mut held = self.registry.lease.lock();
        held.render_transition = false;
        drop(held);
        self.registry.lease_free.notify_all();
        pr_info!("G17P render transition probe: released slot={} guard-ns={}\n",
            self.slot, self.started.elapsed().as_nanos().max(0));
    }
}
impl Drop for G17PComputeLeaseGuard {
    fn drop(&mut self) {
        let mut held = self.registry.lease.lock();
        if self.exclusive { held.compute_install = false; }
        self.registry.compute_readers.fetch_sub(1, Ordering::AcqRel);
        drop(held);
        self.registry.lease_free.notify_all();
    }
}

/// Releases the compute lease and wakes the next waiting client.
struct G17PLeaseGuard {
    registry: Arc<G17PQueueRegistry>,
    slot: u8,
}

fn append_render_prefix_dependencies(
    frontier: &[(u64, Fence)], prefix: Option<u64>,
    dependencies: &mut KVec<Fence>, checked_dependencies: &mut KVec<Fence>,
) -> Result {
    for (sequence, fence) in frontier {
        if crate::g17_submission::g17p_render_prefix_contains(prefix, *sequence) {
            // DRM may coalesce equal-context scheduler dependencies. Retain
            // every required fence for the existing pre-lease status/wait path.
            checked_dependencies.push(fence.clone(), GFP_KERNEL)?;
            dependencies.push(fence.clone(), GFP_KERNEL)?;
        }
    }
    Ok(())
}

/// Callback-driven completion of every hardware command in one ioctl. The
/// initial member is held by the output-publication guard until enqueue ends.
pub(crate) struct G17PSubmitCompletion {
    fence: UserFence<G17PSubmissionFence>,
    remaining: AtomicU32,
    error: AtomicI32,
}

impl G17PSubmitCompletion {
    fn new(fence: UserFence<G17PSubmissionFence>) -> Result<Arc<Self>> {
        Ok(Arc::new(Self { fence, remaining: AtomicU32::new(1), error: AtomicI32::new(0) }, GFP_KERNEL)?)
    }

    fn fence(&self) -> Fence { Fence::from_fence(&self.fence) }

    fn member_done(&self, result: Result) {
        if let Err(error) = result {
            let _ = self.error.compare_exchange(0, error.to_errno(), Ordering::AcqRel, Ordering::Acquire);
        }
        if self.remaining.fetch_sub(1, Ordering::AcqRel) == 1 {
            let error = self.error.load(Ordering::Acquire);
            if error != 0 { self.fence.set_error(Error::from_errno(error)); }
            self.fence.signal();
        }
    }

    fn finish_enqueue(&self, result: Result) {
        // Preserve the actual ioctl failure over a dropped unsubmitted member's
        // ECANCELED status. Published members still own their completion pins.
        if let Err(error) = result { self.error.store(error.to_errno(), Ordering::Release); }
        self.member_done(result);
    }
}

/// One counted command, including construction/enqueue failure. Dropping an
/// uncompleted member contributes cancellation instead of stranding the fence.
pub(crate) struct G17PSubmitCompletionMember {
    aggregate: Arc<G17PSubmitCompletion>,
    completed: AtomicU32,
}

impl G17PSubmitCompletionMember {
    fn new(aggregate: Arc<G17PSubmitCompletion>) -> Self {
        aggregate.remaining.fetch_add(1, Ordering::Relaxed);
        Self { aggregate, completed: AtomicU32::new(0) }
    }

    pub(crate) fn complete(&self, result: Result) {
        if self.completed.swap(1, Ordering::AcqRel) == 0 {
            self.aggregate.member_done(result);
        }
    }
}

impl Drop for G17PSubmitCompletionMember {
    fn drop(&mut self) { self.complete(Err(ECANCELED)); }
}

/// Exact hardware completion for one published render.  The lease is owned by
/// the completion packet, not the submit ioctl, so neither file close nor a
/// fast return can hand the physical slot to another client before retirement.
pub(crate) struct G17PRenderCompletion {
    fence: UserFence<G17PSubmissionFence>,
    /// Immutable host identity for fault-only correlation.  Keeping this on
    /// the already allocated completion avoids timing-distorting per-submit
    /// printk traffic while still joining a recovery to the exact UAPI work.
    submission_id: u64,
    vdm_base: u64,
    owner: u64,
    /// 0xff until the scheduler worker acquires the exact physical slot.
    render_slot: AtomicU32,
    render_order: crate::g17_submission::G17PRenderOrder,
    /// Immutable native QID/stamp, assigned before this completion is installed.
    fragment_dependency: AtomicU64,
    aggregate_member: G17PSubmitCompletionMember,
    /// Vertex start/end and fragment start/end UAPI destinations.
    timestamps: [Option<TimestampDestination>; 4],
    status: Arc<crate::g17_status::VmStatus>,
    lease: Arc<Mutex<Option<G17PLeaseGuard>>>,
    vm_job: Arc<Mutex<Option<mmu::T8140VmJobGuard>>>,
    created: Instant<Monotonic>,
    trace_created_ns: u64,
    trace_run_ns: AtomicU64,
    trace_attach_ns: AtomicU64,
    attached_ns: AtomicU64,
    published_ns: AtomicU64,
    signalled: AtomicU32,
}

impl G17PRenderCompletion {
    fn new(
        fence: UserFence<G17PSubmissionFence>,
        timestamps: [Option<TimestampDestination>; 4],
        status: Arc<crate::g17_status::VmStatus>,
        vm_job: mmu::T8140VmJobGuard,
        submission_id: u64,
        vdm_base: u64,
        owner: u64,
        render_order: crate::g17_submission::G17PRenderOrder,
        aggregate_member: G17PSubmitCompletionMember,
    ) -> Result<Arc<Self>> {
        let lease = Arc::pin_init(
            new_mutex!(None, "G17PRenderCompletion::lease"),
            GFP_KERNEL,
        )?;
        let vm_job = Arc::pin_init(
            new_mutex!(Some(vm_job), "G17PRenderCompletion::vm_job"),
            GFP_KERNEL,
        )?;
        Ok(Arc::new(Self {
            fence,
            submission_id,
            vdm_base,
            owner,
            render_slot: AtomicU32::new(u8::MAX as u32),
            render_order,
            fragment_dependency: AtomicU64::new(u64::MAX),
            aggregate_member,
            timestamps,
            status,
            lease,
            vm_job,
            created: Instant::now(),
            trace_created_ns: if crate::g17_timing_buffer::enabled() { crate::g17_timing_buffer::now() } else { 0 },
            trace_run_ns: AtomicU64::new(0),
            trace_attach_ns: AtomicU64::new(0),
            attached_ns: AtomicU64::new(0),
            published_ns: AtomicU64::new(0),
            signalled: AtomicU32::new(0),
        }, GFP_KERNEL)?)
    }

    pub(crate) fn fence(&self) -> Fence { Fence::from_fence(&self.fence) }

    pub(crate) fn set_fragment_dependency(&self, word: u64) {
        self.fragment_dependency.store(word, Ordering::Release);
    }

    /// Runtime publication and retirement share one mutex. Every preceding
    /// render has either retired or is in the pending table with its native
    /// token. Scheduler FIFO alone does not order different physical pairs.
    pub(crate) fn predecessor_dependency(
        &self, predecessor: &Self,
    ) -> Result<Option<crate::g17_submission::G17PClBarrierDependency>> {
        if self.owner != predecessor.owner || !Arc::ptr_eq(&self.status, &predecessor.status) {
            return Ok(None);
        }
        // SAFETY: predecessor owns its fence throughout this read.
        let status = unsafe { bindings::dma_fence_get_status(predecessor.fence().raw()) };
        crate::g17_submission::g17p_live_render_dependency(
            self.render_order, predecessor.render_order.sequence, status,
            predecessor.fragment_dependency.load(Ordering::Acquire),
        ).map_err(|_| EIO)
    }

    /// Attach the physical lease only when the scheduler has resolved every
    /// dependency and is ready to publish. The VM job was retained at ioctl
    /// time, so queued work remains pinned without consuming hardware state.
    fn attach_lease(&self, lease: G17PLeaseGuard) -> Result<u8> {
        if self.signalled.load(Ordering::Acquire) != 0 { return Err(ECANCELED); }
        let slot = lease.slot();
        let mut held = self.lease.lock();
        if held.is_some() || self.signalled.load(Ordering::Acquire) != 0 {
            return Err(ECANCELED);
        }
        *held = Some(lease);
        self.render_slot.store(slot as u32, Ordering::Release);
        if self.trace_created_ns != 0 {
            self.trace_attach_ns.store(crate::g17_timing_buffer::now(), Ordering::Relaxed);
        }
        self.attached_ns.store(
            self.created.elapsed().as_nanos().max(0) as u64,
            Ordering::Release,
        );
        Ok(slot)
    }

    fn mark_published(&self) {
        self.published_ns.store(
            self.created.elapsed().as_nanos().max(0) as u64,
            Ordering::Release,
        );
    }

    pub(crate) fn render_slot(&self) -> Option<u8> {
        let slot = self.render_slot.load(Ordering::Acquire);
        (slot != u8::MAX as u32).then_some(slot as u8)
    }

    pub(crate) fn diagnostic_identity(&self) -> (u64, u64, u8, u32) {
        (
            self.submission_id,
            self.vdm_base,
            self.render_slot.load(Ordering::Acquire) as u8,
            self.signalled.load(Ordering::Acquire),
        )
    }

    pub(crate) fn belongs_to_vm(
        &self,
        status: &Arc<crate::g17_status::VmStatus>,
    ) -> bool {
        Arc::ptr_eq(&self.status, status)
    }

    pub(crate) fn vm_status(&self) -> Arc<crate::g17_status::VmStatus> {
        self.status.clone()
    }

    /// Finish a packet which never became firmware-visible. No quarantine or
    /// processor-stop boundary is needed because the scheduler still owns the
    /// only publication right and the physical lease has already been dropped.
    pub(crate) fn fail_unpublished(&self, error: Error) -> bool {
        if self.signalled.compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire).is_err() {
            return false;
        }
        self.status.record(error.to_errno());
        self.fence.set_error(error);
        drop(self.lease.lock().take());
        drop(self.vm_job.lock().take());
        self.fence.signal();
        self.aggregate_member.complete(Err(error));
        true
    }

    /// Report failure without claiming that firmware stopped using this job.
    /// The runtime must retain this completion through queue retirement or
    /// processor stop. In particular, an error fence is not an unmap witness.
    pub(crate) fn fail_while_owned(&self, error: Error) -> bool {
        if self.signalled.compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire).is_err() {
            return false;
        }
        if let Some(vm_job) = self.vm_job.lock().as_ref() {
            vm_job.quarantine();
        }
        // Publish the registry quarantine before waking userspace or dropping
        // the runtime mutex. Recovery may stop the processors immediately
        // after this failure; recording the bit here ensures its post-stop
        // rehabilitation cannot race an older caller which marks it later.
        if let Some(lease) = self.lease.lock().as_ref() {
            lease.registry.quarantine_render_slot(lease.slot);
        }
        self.status.record(error.to_errno());
        self.fence.set_error(error);
        // Retain BOTH guards: dropping the lease here also lets a waiter
        // reacquire this physical slot before quarantine is recorded.
        self.fence.signal();
        self.aggregate_member.complete(Err(error));
        true
    }

    /// Signal at most once even when both RTKit roles report the same event.
    /// The caller must prove retirement, processor stop, or no publication.
    /// Unresolved hardware failures must use `fail_while_owned` instead.
    pub(crate) fn complete(
        &self,
        result: Result<g17_completion::PairedRenderCompletion>,
    ) {
        if self.signalled.compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire).is_err() {
            return;
        }
        let gpu_durations = result.as_ref().ok().map(|completion| [
            completion.execution.tiling_timestamps[1]
                .wrapping_sub(completion.execution.tiling_timestamps[0]),
            completion.execution.fragment_timestamps[1]
                .wrapping_sub(completion.execution.fragment_timestamps[0]),
        ]);
        let completed_ns = self.created.elapsed().as_nanos().max(0) as u64;
        if self.trace_created_ns != 0 {
            let mut words = [0; crate::g17_timing_buffer::WORDS];
            words[..10].copy_from_slice(&[2, self.owner, self.submission_id, self.vdm_base,
                self.trace_created_ns, crate::g17_timing_buffer::now(),
                self.render_slot.load(Ordering::Relaxed) as u64,
                gpu_durations.unwrap_or([0; 2])[0], gpu_durations.unwrap_or([0; 2])[1],
                result.as_ref().map(|r| r.complete).unwrap_or(false) as u64]);
            crate::g17_timing_buffer::record(words);
        }
        let result = result.and_then(|completion| {
            if !completion.complete {
                return Err(EIO);
            }
            // The tiler stamps the pass start and the fragment stage its end;
            // publish them before the fence lets userspace read the results.
            let execution = completion.execution;
            let observed = [
                execution.tiling_timestamps[0],
                execution.tiling_timestamps[1],
                execution.fragment_timestamps[0],
                execution.fragment_timestamps[1],
            ];
            for (destination, timestamp) in self.timestamps.iter().zip(observed) {
                if let Some(destination) = destination {
                    destination.write(timestamp)?;
                }
            }
            Ok(())
        });
        let trace_limit = *module_parameters::g17p_trace_depth.value();
        if *module_parameters::g17p_completion_trace.value() != 0
            && trace_limit != 0
            && G17P_RENDER_TIMING_RECORDS.fetch_add(1, Ordering::Relaxed) < trace_limit
        {
            let attached_ns = self.attached_ns.load(Ordering::Acquire);
            let published_ns = self.published_ns.load(Ordering::Acquire);
            let durations = gpu_durations.unwrap_or([0; 2]);
            pr_info!(
                "G17P render timing: owner={} submission={} vdm={:#x} slot={} attach={}ns publish={}ns complete={}ns dependency={}ns publication={}ns retirement={}ns gpu-ta={} gpu-3d={} ok={}\n",
                self.owner,
                self.submission_id,
                self.vdm_base,
                self.render_slot.load(Ordering::Acquire),
                attached_ns,
                published_ns,
                completed_ns,
                attached_ns,
                published_ns.saturating_sub(attached_ns),
                completed_ns.saturating_sub(published_ns),
                durations[0],
                durations[1],
                result.is_ok(),
            );
        }
        if let Err(error) = result {
            self.status.record(error.to_errno());
            self.fence.set_error(error);
        }
        // Release the physical slot before waking userspace. The scheduler
        // intentionally retains this completion packet until its finished
        // fence retires; tying lease Drop to packet Drop lets the woken client
        // race ahead, take the other slot and collide with this VM's fixed
        // render-state aliases.
        drop(self.lease.lock().take());
        // A signalled output fence is userspace's permission to recycle BOs
        // and unbind transient VDM aliases. Retire the VM-job guard first;
        // leaving it in the scheduler-owned completion packet made those
        // post-fence unbinds return EBUSY until an unrelated worker freed the
        // packet, after which the already-recycled VA collided with the stale
        // mapping.
        drop(self.vm_job.lock().take());
        self.fence.signal();
        self.aggregate_member.complete(result);
    }

    /// Release guards retained by `fail_while_owned` after both ASCs stop.
    ///
    /// The output fence may already be signalled, so `complete` deliberately
    /// returns early and cannot serve as this ownership boundary. Retire the VM
    /// job before restoring the physical slot, then clear its quarantine while
    /// the lease still excludes waiters; dropping the lease publishes both
    /// changes and wakes them.
    pub(crate) fn release_after_processor_stop(&self) {
        drop(self.vm_job.lock().take());
        let lease = self.lease.lock().take();
        if let Some(lease) = lease.as_ref() {
            lease
                .registry
                .rehabilitate_render_slot_after_processor_stop(lease.slot);
        }
        drop(lease);
    }
}

impl G17PLeaseGuard {
    fn slot(&self) -> u8 { self.slot }
}

impl Drop for G17PLeaseGuard {
    fn drop(&mut self) {
        let mut held = self.registry.lease.lock();
        held.release_render_slot(self.slot);
        drop(held);
        self.registry.lease_free.notify_all();
    }
}

const _: () = assert!(!core::mem::needs_drop::<G17PRuntimeRef>());

/// DRM-visible T8140 manager. Hardware lifetime stays in the retained runtime.
pub(crate) struct G17PDrmGpu {
    runtime: G17PRuntimeRef,
    soc: &'static hw::agx3::SocConfig,
    observed: hw::GpuIdConfig,
    ids: gpu::SequenceIDs,
    registry: Arc<G17PQueueRegistry>,
}

impl G17PDrmGpu {
    pub(crate) fn new(
        runtime: G17PRuntimeRef,
        soc: &'static hw::agx3::SocConfig,
        observed: hw::GpuIdConfig,
    ) -> Result<Self> {
        let registry = G17PQueueRegistry::new()?;
        runtime.mutex().lock().attach_queue_registry(registry.clone())?;
        Ok(Self {
            runtime,
            soc,
            observed,
            ids: Default::default(),
            registry,
        })
    }
}

impl DrmGpu for G17PDrmGpu {
    fn init(&self) -> Result {
        Ok(())
    }

    fn ids(&self) -> &gpu::SequenceIDs {
        &self.ids
    }

    fn is_crashed(&self) -> bool {
        // `file.rs::get_params` turns this into ENODEV, and it runs before any
        // ioctl that could reach the submit path's own recovery -- so a crashed
        // session used to lock every LATER process out of the device until the
        // machine was rebooted. Try once (bounded inside) to rebuild instead,
        // then answer with the state that leaves.
        self.runtime.mutex().lock().recover_crashed_session_once()
    }

    fn supports_vm_status(&self) -> bool { true }
    fn independent_queue_limits(&self) -> Option<[u8; 16]> {
        Some(crate::g17_queue_limits::encode_limits())
    }

    fn submission_error(&self) -> i32 {
        if self.registry.compute_faulted.load(Ordering::Acquire) != 0 { EIO.to_errno() } else { 0 }
    }

    fn update_globals(&self) {}

    fn params(&self) -> Result<DrmGpuParams> {
        let mut core_masks = [0u32; uapi::DRM_ASAHI_MAX_CLUSTERS as usize];
        for (index, mask) in self.observed.core_masks.iter().enumerate() {
            *core_masks.get_mut(index).ok_or(EIO)? = *mask;
        }
        Ok(DrmGpuParams {
            gpu_generation: self.observed.gpu_gen as u32,
            gpu_variant: self.observed.gpu_variant as u32,
            gpu_revision: self.observed.gpu_rev as u32,
            chip_id: self.soc.chip_id,
            num_dies: self.observed.num_dies,
            num_clusters_total: self.observed.num_clusters,
            num_cores_per_cluster: self.observed.num_cores,
            core_masks,
            max_frequency_khz: g17_adt_j700::J700_PERF_STATES
                [g17_adt_j700::J700_GPU_NUM_PERF_STATES as usize]
                .freq_hz
                / 1_000,
            usc_generation: self.observed.usc_generation,
            gpu_hal_generation: self.observed.gpu_hal_generation as u32,
            max_commands_per_submission: file::MAX_COMMANDS_PER_SUBMISSION,
        })
    }

    fn user_range(&self) -> Result<Range<u64>> {
        Ok(T8140_USER_START..T8140_UNKNOWN_PAGE)
    }

    fn unknown_page(&self) -> Result<u64> {
        Ok(T8140_UNKNOWN_PAGE)
    }

    fn base_clock_hz(&self) -> u32 {
        // `DRM_IOCTL_ASAHI_GET_TIME` scales CNTPCT_EL0 by this rate. T8140
        // runs the architectural counter at its own CNTFRQ_EL0, not at the
        // GPU's 24 MHz command timestamp rate.
        let frequency: u64;
        // SAFETY: CNTFRQ_EL0 is a read-only system register.
        unsafe {
            core::arch::asm!(
                "mrs {x}, CNTFRQ_EL0",
                x = out(reg) frequency
            );
        }
        frequency as u32
    }

    fn new_vm(&self, kernel_range: Range<u64>) -> Result<mmu::Vm> {
        self.runtime
            .mutex()
            .lock()
            .new_user_vm(self.ids.vm.next(), kernel_range)
    }

    fn new_queue(
        &self,
        vm: mmu::Vm,
        _ualloc: Arc<Mutex<alloc::DefaultAllocator>>,
        _ualloc_priv: Arc<Mutex<alloc::DefaultAllocator>>,
        priority: u32,
        usc_exec_base: u64,
    ) -> Result<KBox<dyn queue::Queue>> {
        let dev = self.runtime.mutex().lock().drm_ref()?;
        // Reserved before the queue object exists so the failure is a clean
        // ENOSPC from QUEUE_CREATE. `G17PQueue::drop` gives the slot back.
        self.registry.reserve_queue()?;
        let queue = G17PQueue::new(
            &dev,
            self.runtime,
            self.registry.clone(),
            self.observed.gpc_perf_state_map,
            self.observed.gpc_perf_state_map_low,
            self.observed.gpc_perf_state_control,
            vm,
            self.ids.queue.next(),
            priority,
            usc_exec_base,
        )
        .inspect_err(|_| self.registry.release_queue())?;
        match KBox::new(queue, GFP_KERNEL) {
            Ok(boxed) => Ok(boxed),
            Err(error) => Err(error.into()),
        }
    }

    fn map_timestamp_buffer(
        &self,
        bo: gem::ObjectRef,
        range: Range<usize>,
    ) -> Result<mmu::KernelMapping> {
        self.runtime
            .mutex()
            .lock()
            .map_timestamp_buffer(bo, range)
    }
}

struct UserAddressSpace<'a> {
    vm: &'a mmu::Vm,
    range: Range<u64>,
}

impl g17_uapi::GpuAddressSpace for UserAddressSpace<'_> {
    fn covers(&self, address: u64, size: u64, access: g17_uapi::GpuAccess) -> bool {
        let Some(end) = address.checked_add(size) else {
            return false;
        };
        let (need_read, need_write) = match access {
            g17_uapi::GpuAccess::Read => (true, false),
            g17_uapi::GpuAccess::Write => (false, true),
            g17_uapi::GpuAccess::ReadWrite => (true, true),
        };
        size != 0
            && self.range.start <= address
            && end <= self.range.end
            && self.vm.covers_range(address, size, need_read, need_write)
    }
}

#[derive(Default)]
pub(crate) struct G17PSubmissionFence;

#[vtable]
impl FenceOps for G17PSubmissionFence {
    fn get_driver_name<'a>(self: &'a FenceObject<Self>) -> &'a CStr {
        c_str!("asahi")
    }

    fn get_timeline_name<'a>(self: &'a FenceObject<Self>) -> &'a CStr {
        c_str!("g17p-submit")
    }
}

static QUEUE_NAME: &CStr = c_str!("g17p_queue");
static QUEUE_CLASS_KEY: core::pin::Pin<&'static kernel::sync::LockClassKey> = static_lock_class!();

/// One userspace queue bound to the retained G17P runtime.
///
/// One of these exists per DRM `queue_create`, i.e. per client, and its `id`
/// is what the runtime records as the owner of the hardware compute graph.
/// Dropping it -- `queue_destroy`, or the file close the kernel performs for a
/// client that died mid-submission -- releases that ownership.
struct G17PQueue {
    dev: AsahiDevRef,
    runtime: G17PRuntimeRef,
    registry: Arc<G17PQueueRegistry>,
    /// Cached during the safe SGX GPU-ID probe; never sampled at submission.
    gpc_perf_state_map: u32,
    gpc_perf_state_map_low: u32,
    gpc_perf_state_control: u32,
    vm: mmu::Vm,
    user_range: Range<u64>,
    usc_exec_base: u64,
    fence_contexts: FenceContexts,
    /// Per-logical-queue VM context. Render needs the context immediately,
    /// but the hardware compute graph is deliberately allocated only if this
    /// logical queue actually submits compute.
    context: Arc<crate::g17_resources::G17PLogicalQueueContext>,
    async_backend: Option<Arc<G17PAsyncQueueBackend>>,
    async_scheduler: Option<g17_job::G17PJobScheduler<G17PAsyncQueueBackend>>,
    render_scheduler: Option<g17_job::G17PRenderScheduler<G17PRenderQueueBackend>>,
    /// Most recent job submitted through this logical DRM queue, regardless
    /// of engine. Vulkan queue order crosses the render/compute boundary: a
    /// compute following a render must not publish before the render retires,
    /// just as a render following compute must not overtake it. Independent
    /// logical queues retain their own timelines and can still overlap.
    last_submission: Option<Fence>,
    /// Preserve the existing cross-engine fence in addition to exact indexed
    /// render-prefix coverage; same-engine FIFO alone orders publication, not
    /// completion across two physical render pairs.
    last_submission_engine: Option<G17PSubmissionEngine>,
    /// Monotonic logical render index; independent of physical pair rotation.
    render_sequence: u64,
    /// Only accepted renders which have not yet signalled. Queued work counts
    /// until its real completion; physical-pair admission remains unchanged.
    render_frontier: KVec<(u64, Fence)>,
    id: u64,
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
enum G17PSubmissionEngine {
    Compute,
    Render,
}

impl Drop for G17PQueue {
    fn drop(&mut self) {
        // Entity teardown handles pending scheduler jobs before the frontend
        // reference goes away. Active hardware packets are retained by runtime.
        //
        // Do not leave async ownership release to G17PAsyncQueueBackend::drop:
        // DRM scheduler teardown may retain that Arc past the file/queue close
        // boundary.  An idle queue would then pin its QID and execution context
        // even though no DRM file still owns the render node, forcing the next
        // client onto a needlessly fresh hardware queue.  Release explicitly
        // after the entity is torn down; the backend's later Drop is idempotent.
        drop(self.render_scheduler.take());
        drop(self.async_scheduler.take());
        // Do not publish the physical pairs as reusable before the manager has
        // retired this queue's descriptor-context ownership.  A fresh client
        // can acquire a released registry slot immediately; exposing it first
        // lets that client enter VM handoff while the retained QIDs still name
        // this context.  Completion already establishes runtime -> registry
        // as the nested lock order when it drops the physical lease, so retire
        // under the runtime mutex first and wake registry waiters only after.
        self.runtime
            .mutex()
            .lock()
            .release_render_owner(self.id, self.context.context_id() as u16);
        self.registry.release_render_owner(self.id);
        self.runtime
            .mutex()
            .lock()
            .release_async_compute_queue(self.id);
        drop(self.async_backend.take());
        self.runtime.release_compute_owner(self.id);
        self.registry.release_queue();
    }
}

pub(crate) struct G17PRenderQueueBackend {
    runtime: G17PRuntimeRef,
    registry: Arc<G17PQueueRegistry>,
    owner: u64,
    vm: mmu::Vm,
    context: Arc<crate::g17_resources::G17PLogicalQueueContext>,
}

/// Scheduler teardown is not evidence that a published GPU job failed.
/// Keep its owner alive and let the RTKit worker retire the hardware fence.
/// This runs only at queue destruction, with no runtime mutex held, and is
/// uninterruptible because the exiting task already has a fatal signal.
/// Use the same finite bound as the render scheduler's watchdog; a genuinely
/// stuck job still reaches the existing final-observation/quarantine path.
fn wait_for_render_teardown(fence: &Fence) -> bool {
    // SAFETY: The retained Fence reference lives across the entire wait.
    unsafe {
        bindings::dma_fence_wait_timeout(
            fence.raw(),
            false,
            kernel::time::msecs_to_jiffies(2000) as _,
        ) > 0
    }
}

impl g17_job::G17PRenderQueueBackend for G17PRenderQueueBackend {
    fn prepare(&self, _packet: &g17_job::G17PRenderJobPacket) -> Option<Fence> {
        None
    }

    fn publish(&self, packet: Arc<g17_job::G17PRenderJobPacket>) -> Result<Fence> {
        let completion = packet.completion();
        if completion.trace_created_ns != 0 {
            completion.trace_run_ns.store(crate::g17_timing_buffer::now(), Ordering::Relaxed);
        }
        let submission_id = packet.submission_id();
        if !packet.begin_publication() {
            completion.fail_unpublished(ECANCELED);
            return Err(ECANCELED);
        }
        let mut failure_stage = "render-vm-status";
        let result = (|| -> Result<Fence> {
            if completion.vm_status().get() != 0 { return Err(EIO); }
            failure_stage = "render-input-fence";
            for fence in packet.input_fences() {
                G17PQueue::wait_fence(fence)?;
            }
            if packet.cancel_requested() { return Err(ECANCELED); }

            // Dependencies are retired before this potentially sleeping slot
            // acquisition. A stable owner normally reacquires its idle slot
            // immediately; only first-use namespace contention waits here.
            failure_stage = "render-acquire-physical-lease";
            let lease = self.registry.acquire_lease(self.owner)?;
            if packet.cancel_requested() { return Err(ECANCELED); }
            failure_stage = "render-attach-physical-lease";
            let render_slot = completion.attach_lease(lease)?;
            let mut transition = None;
            let mut transition_attempted = false;
            let mut control_backpressure_polls = 0usize;
            let mut flist_release_polls = 0usize;
            let mut device_control_polls = 0usize;
            let trace_limit = *module_parameters::g17p_trace_depth.value();
            let mut stages = crate::g17_perf::RenderStageTimes::default();
            stages.enabled = completion.trace_created_ns != 0 ||
                (*module_parameters::g17p_completion_trace.value() != 0
                && trace_limit != 0
                && G17P_RENDER_POLL_RECORDS.load(Ordering::Relaxed) < trace_limit);
            let submit = loop {
                if packet.cancel_requested() { break Err(ECANCELED); }
                let lock_started = stages.enabled.then(Instant::<Monotonic>::now);
                let mut runtime = self.runtime.mutex().lock();
                if let Some(started) = lock_started {
                    stages.runtime_lock_ns = stages.runtime_lock_ns.saturating_add(
                        started.elapsed().as_nanos().max(0) as u64);
                }
                failure_stage = "render-query-freelist-release";
                if runtime.render_usc_freelist_release_pending_for_slot(render_slot)? {
                    drop(runtime);
                    drop(transition.take());
                    transition_attempted = false;
                    if control_backpressure_polls >= 1000 { break Err(ETIMEDOUT); }
                    control_backpressure_polls += 1;
                    flist_release_polls += 1;
                    // A consumed release usually arrives before the old 1 ms
                    // sleep ends. Bound the short-poll phase, then preserve
                    // the coarse wait for delayed firmware and recovery.
                    if *module_parameters::g17p_flist_fast_poll.value() != 0
                        && flist_release_polls <= 8
                    {
                        fsleep(Delta::from_micros(100));
                    } else {
                        fsleep(Delta::from_millis(1));
                    }
                    continue;
                }
                if runtime.primary_device_control_backpressured() {
                    drop(runtime);
                    drop(transition.take());
                    transition_attempted = false;
                    if control_backpressure_polls >= 1000 { break Err(ETIMEDOUT); }
                    control_backpressure_polls += 1;
                    device_control_polls += 1;
                    fsleep(Delta::from_millis(1));
                    continue;
                }
                if *module_parameters::g17p_render_transition_probe.value() != 0 {
                    let class = *module_parameters::g17p_render_transition_class.value();
                    let reasons = runtime.render_transition_reasons(
                        &self.vm, render_slot, &self.context, packet.command(),
                    ).unwrap_or(crate::g17_manager::render_transition::UNKNOWN);
                    let selected = crate::g17_manager::render_transition::selected(reasons, class);
                    if reasons != 0 || transition.is_some() {
                        pr_info!("G17P render transition probe: submission={} slot={} reasons={:#x} class={:#x} selected={} held={}\n",
                            submission_id, render_slot, reasons, class, selected, transition.is_some());
                    }
                    if transition.is_some() && !selected {
                        drop(runtime);
                        drop(transition.take());
                        continue;
                    }
                    if selected && !transition_attempted {
                        drop(runtime);
                        transition_attempted = true;
                        failure_stage = "render-acquire-transition";
                        transition = Some(self.registry.acquire_render_transition(render_slot)?);
                        continue;
                    }
                }
                failure_stage = "render-submit-translated";
                break runtime.submit_translated_render(
                    &self.vm,
                    render_slot,
                    true,
                    self.context.clone(),
                    packet.command(),
                    completion.clone(),
                    &mut stages,
                );
            };
            // Capture before optional legacy formatting; no completion-side copy.
            // Completion may already be recorded: userspace joins immutable IDs.
            if completion.trace_created_ns != 0 {
                let mut words = [0; crate::g17_timing_buffer::WORDS];
                words.copy_from_slice(&[1, self.owner, submission_id, completion.vdm_base,
                    completion.trace_created_ns, crate::g17_timing_buffer::now(), render_slot as u64,
                    completion.trace_run_ns.load(Ordering::Relaxed),
                    completion.trace_attach_ns.load(Ordering::Relaxed),
                    stages.runtime_lock_ns, stages.handoff_ns, stages.prepare_ns,
                    stages.restage_ns, stages.reachability_ns, stages.flist_request_ns,
                    flist_release_polls as u64, device_control_polls as u64,
                    stages.tvb_blocks as u64, stages.retained as u64,
                    stages.reachability_cache_hit as u64, submit.is_ok() as u64, 0, 0, 0]);
                crate::g17_timing_buffer::record(words);
            }
            if *module_parameters::g17p_completion_trace.value() != 0
                && trace_limit != 0
                && G17P_RENDER_POLL_RECORDS.fetch_add(1, Ordering::Relaxed) < trace_limit
            {
                pr_info!(
                    "G17P render admission: owner={} submission={} vdm={:#x} slot={} flist-polls={} control-polls={} fast-poll={} lock={}ns handoff={}ns prepare={}ns restage={}ns reachability={}ns cache-hit={} flist-request={}ns tvb-blocks={} retained={}\n",
                    self.owner, submission_id, completion.vdm_base, render_slot, flist_release_polls,
                    device_control_polls,
                    *module_parameters::g17p_flist_fast_poll.value(),
                    stages.runtime_lock_ns, stages.handoff_ns, stages.prepare_ns,
                    stages.restage_ns, stages.reachability_ns,
                    stages.reachability_cache_hit,
                    stages.flist_request_ns, stages.tvb_blocks, stages.retained,
                );
            }
            drop(transition);
            match submit {
                Ok(()) => {
                    completion.mark_published();
                    packet.mark_published();
                    Ok(packet.fence())
                }
                Err(error) => {
                    // submit_translated_render may fail after the first
                    // firmware-visible write. Let its exact tracker decide
                    // whether this is an unpublished release or quarantine.
                    self.runtime.mutex().lock().timeout_deferred_render(
                        render_slot, submission_id, completion, error,
                    );
                    Err(error)
                }
            }
        })();
        if let Err(error) = result.as_ref() {
            completion.fail_unpublished(*error);
            G17PQueue::stage_fail(submission_id, failure_stage, *error);
        }
        result
    }

    fn timed_out(
        &self,
        packet: Arc<g17_job::G17PRenderJobPacket>,
    ) -> kernel::drm::sched::Status {
        if packet.request_cancel() {
            packet.completion().fail_unpublished(ETIMEDOUT);
        } else if packet.is_published() {
            if let Some(render_slot) = packet.completion().render_slot() {
                self.runtime.mutex().lock().timeout_deferred_render(
                    render_slot, packet.submission_id(), packet.completion(), ETIMEDOUT,
                );
            }
        }
        kernel::drm::sched::Status::Nominal
    }

    fn cancel(&self, packet: Arc<g17_job::G17PRenderJobPacket>) {
        if packet.request_cancel() {
            packet.completion().fail_unpublished(ECANCELED);
            return;
        }
        if wait_for_render_teardown(&packet.fence()) {
            return;
        }
        if packet.is_published() {
            if let Some(render_slot) = packet.completion().render_slot() {
                self.runtime.mutex().lock().timeout_deferred_render(
                    render_slot, packet.submission_id(), packet.completion(), ETIMEDOUT,
                );
            }
        } else {
            packet.completion().fail_unpublished(ECANCELED);
        }
    }
}

enum G17PPendingCommand {
    Render(g17_uapi::TranslatedRenderCommand),
    Compute(g17_uapi::TranslatedComputeCommand),
}

type TimestampDestination = g17_job::TimestampDestination;

pub(crate) struct G17PAsyncQueueBackend {
    runtime: G17PRuntimeRef,
    registry: Arc<G17PQueueRegistry>,
    owner: u64,
    hardware_queue: u8,
    context: Arc<crate::g17_resources::G17PLogicalQueueContext>,
}

impl Drop for G17PAsyncQueueBackend {
    fn drop(&mut self) { self.runtime.mutex().lock().release_async_compute_queue(self.owner); }
}

impl g17_job::G17PQueueBackend for G17PAsyncQueueBackend {
    fn prepare(&self, _packet: &g17_job::G17PComputeJobPacket) -> Option<Fence> {
        self.runtime.mutex().lock().async_compute_dependency(self.owner)
    }
    fn publish(&self, packet: Arc<g17_job::G17PComputeJobPacket>) -> Result<Fence> {
        let status = packet.completion()?.status.clone();
        let result = (|| -> Result<Fence> {
            let completion = packet.completion()?;
            // Several compute jobs may already be queued behind one predecessor.
            // If that predecessor failed, its scheduler fence wakes this job, but
            // a signalled error dependency is still "ready" to DRM sched. Refuse
            // the queued successor before acquiring a lease or publishing it.
            if completion.status.get() != 0 {
                completion.complete(Err(EIO));
                return Err(EIO);
            }
            // A dma_fence carrying an error is still signalled, so DRM sched has
            // legitimately made this job runnable. It does not mean the GPU
            // dependency succeeded. Consume every retained dependency status
            // before taking a hardware lease or writing queue state.
            for fence in packet.input_fences() {
                G17PQueue::wait_fence(fence)?;
            }
            let hardware_fence = completion.fence();
            let (first_install, burst_steady) = {
                let runtime = self.runtime.mutex().lock();
                let first_install = runtime.async_compute_needs_install(self.owner)?;
                let steady = *module_parameters::g17p_compute_burst_probe.value() != 0
                    && runtime.async_compute_burst_steady(self.owner);
                (first_install, steady)
            };
            let burst = if burst_steady {
                self.registry.wait_compute_burst(self.owner, first_install)?
            } else { None };
            let submission_id = packet.submission_id();
            let published = (|| -> Result {
                let lease = self.registry.acquire_compute(first_install)?;
                self.runtime.mutex().lock().publish_async_compute(self.owner, packet, lease)
            })();
            if let Some(burst) = burst {
                // Log after dropping the runtime mutex; never add console
                // latency between individual producer writes or RTKit sends.
                pr_info!("G17P compute burst probe: cohort={} parties={} outcome={} owner={} qid={} submission={} gather-ns={} release-to-published-ns={} render-epoch={} readers-at-departure={} errno={}\n",
                    burst.cohort, burst.parties, burst.outcome, self.owner,
                    self.hardware_queue, submission_id, burst.wait_ns,
                    burst.released.elapsed().as_nanos().max(0), burst.render_epoch,
                    burst.compute_readers, published.as_ref().err().map_or(0, |error| error.to_errno()));
            }
            published?;
            Ok(hardware_fence)
        })();
        if let Err(error) = result.as_ref() {
            let lost = *error == EIO
                || *error == ETIMEDOUT
                || *error == ENODEV
                || status.get() != 0;
            if lost {
                self.runtime
                    .mutex()
                    .lock()
                    .fail_outstanding_jobs_for_vm(&status, *error);
            }
        }
        result
    }
    fn timed_out(&self, packet: Arc<g17_job::G17PComputeJobPacket>) -> kernel::drm::sched::Status {
        self.runtime.mutex().lock().quarantine_async_compute(self.owner, packet.submission_id(), ETIMEDOUT);
        kernel::drm::sched::Status::Nominal
    }
    fn cancel(&self, packet: Arc<g17_job::G17PComputeJobPacket>) {
        let active = self.runtime.mutex().lock()
            .cancel_async_compute(self.owner, packet.submission_id());
        if !active {
            if let Ok(completion) = packet.completion() { completion.complete(Err(ECANCELED)); }
        }
    }
}

impl G17PQueue {
    fn new(
        dev: &AsahiDevice,
        runtime: G17PRuntimeRef,
        registry: Arc<G17PQueueRegistry>,
        gpc_perf_state_map: u32,
        gpc_perf_state_map_low: u32,
        gpc_perf_state_control: u32,
        vm: mmu::Vm,
        id: u64,
        priority: u32,
        usc_exec_base: u64,
    ) -> Result<Self> {
        if priority > 3 {
            return Err(EINVAL);
        }
        let user_range = T8140_USER_START..T8140_UNKNOWN_PAGE;
        g17_uapi::QueueUscWindow {
            base: usc_exec_base,
            user_start: user_range.start,
            user_end: user_range.end,
        }
        .validate()
        .map_err(|_| EINVAL)?;
        let fence_contexts = FenceContexts::new(1, QUEUE_NAME, QUEUE_CLASS_KEY)?;
        // Every logical queue owns a VM execution context, since render also
        // names it. Do not allocate a hardware compute graph here: desktop
        // stacks routinely create idle/render-only logical queues, and eager
        // allocation made those queues consume QID4 before the first real
        // compute submit. The graph and scheduler are created atomically by
        // ensure_async_compute_queue on first compute use.
        let context = runtime.mutex().lock().prepare_queue_execution_context(&vm)?;
        let render_backend = Arc::new(G17PRenderQueueBackend {
            runtime,
            registry: registry.clone(),
            owner: id,
            vm: vm.clone(),
            context: context.clone(),
        }, GFP_KERNEL)?;
        let render_scheduler = g17_job::G17PRenderScheduler::new(
            dev, render_backend.clone(), crate::g17_queue_limits::MAX_IN_FLIGHT_PER_QUEUE,
        )?;
        Ok(Self {
            dev: dev.into(),
            runtime,
            registry,
            gpc_perf_state_map,
            gpc_perf_state_map_low,
            gpc_perf_state_control,
            vm,
            user_range,
            usc_exec_base,
            fence_contexts,
            context,
            async_backend: None,
            async_scheduler: None,
            render_scheduler: Some(render_scheduler),
            last_submission: None,
            render_sequence: 0,
            render_frontier: KVec::new(),
            last_submission_engine: None,
            id,
        })
    }

    fn ensure_async_compute_queue(&mut self) -> Result {
        match (&self.async_backend, &self.async_scheduler) {
            (Some(backend), Some(_)) => {
                if self.runtime.mutex().lock().async_compute_queue_matches(
                    self.id, backend.hardware_queue, &backend.context,
                )? {
                    return Ok(());
                }
            }
            (None, None) => {}
            _ => return Err(EIO),
        }

        // Processor-stop recovery removes every runtime graph after settling
        // its active fence. Drop the now-stale entity first so any jobs queued
        // behind that fence are cancelled before a new graph is published.
        // Backend Drop is harmless here: the runtime just proved this owner is
        // absent. Keep the same teardown ordering as G17PQueue::drop.
        drop(self.async_scheduler.take());
        drop(self.async_backend.take());

        // Default returns the original Arc. The opt-in probe allocates once
        // per backend creation, never on each render/compute engine switch.
        let context = self.runtime.mutex().lock()
            .prepare_compute_execution_context(&self.context)?;
        let (hardware_queue, context) = self.runtime.mutex().lock()
            .prepare_async_compute_queue(self.id, &self.vm, context)?;
        let backend = Arc::new(G17PAsyncQueueBackend {
            runtime: self.runtime,
            registry: self.registry.clone(),
            owner: self.id,
            hardware_queue,
            context,
        }, GFP_KERNEL).inspect_err(|_| {
            self.runtime.mutex().lock().release_async_compute_queue(self.id);
        })?;
        let scheduler = g17_job::G17PJobScheduler::new(
            &self.dev, backend.clone(),
            crate::g17_queue_limits::MAX_COMPUTE_IN_FLIGHT_PER_QUEUE,
        )?;
        self.async_backend = Some(backend);
        self.async_scheduler = Some(scheduler);
        Ok(())
    }

    /// Print one throughput window.
    ///
    /// This is the only line the submit path emits at steady state, and it
    /// carries every number the driver can honestly report:
    ///
    /// * `renders`/`computes`/`gpu-intervals` -- what the window contained,
    ///   and how many renders published a native pass envelope.
    /// * `renders/s` -- end-to-end submission throughput.
    /// * `ioctl`/`publish`/`wait` -- host microseconds per render, and the
    ///   split between building and publishing the work and waiting for it.
    /// * `gpu` -- the firmware's own pass envelope, converted with the 24 MHz
    ///   rate this driver already reports through `DRM_IOCTL_ASAHI_GET_TIME`.
    /// * `occupancy` -- GPU busy ticks over the tick span the window covered.
    ///   Both terms are GPU-written, so no timebase enters this ratio.
    /// * `ioctl-share` -- how much of wall time the client spent inside the
    ///   ioctl. With a synchronous render wait this is the number that says
    ///   how much throughput a client is losing to the driver.
    /// * `tick-khz` -- the tick rate MEASURED from this window. It exists so a
    ///   wrong timebase assumption is visible instead of silently scaling
    ///   every microsecond above.
    fn report_perf(&self, report: g17_perf::SubmitReport) {
        // Async publication records host costs before the job has completed.
        // Missing completion samples do not establish zero GPU utilisation.
        if report.intervals == 0 {
            dev_info!(
                self.dev.as_ref(),
                "G17P perf[{}]: renders={} computes={} gpu-intervals=0 wall={}us renders/s={}.{:02} ioctl={}.{}us(max {}.{}us) publish={}.{}us compute-ioctl={}.{}us ioctl-share={}.{}% gpu=unmeasured\n",
                report.window_index,
                report.renders,
                report.computes,
                report.wall_us,
                report.renders_per_s_x100 / 100,
                report.renders_per_s_x100 % 100,
                report.ioctl_avg_us_x10 / 10,
                report.ioctl_avg_us_x10 % 10,
                report.ioctl_max_us_x10 / 10,
                report.ioctl_max_us_x10 % 10,
                report.publish_avg_us_x10 / 10,
                report.publish_avg_us_x10 % 10,
                report.compute_avg_us_x10 / 10,
                report.compute_avg_us_x10 % 10,
                report.ioctl_pct_x10 / 10,
                report.ioctl_pct_x10 % 10,
            );
            return;
        }
        dev_info!(
            self.dev.as_ref(),
            "G17P perf[{}]: renders={} computes={} gpu-intervals={} wall={}us renders/s={}.{:02} ioctl={}.{}us(max {}.{}us) publish={}.{}us wait={}.{}us polls={}.{} compute-ioctl={}.{}us gpu={}.{}us(max {}.{}us) occupancy={}.{}% ioctl-share={}.{}% tick-khz={}\n",
            report.window_index,
            report.renders,
            report.computes,
            report.intervals,
            report.wall_us,
            report.renders_per_s_x100 / 100,
            report.renders_per_s_x100 % 100,
            report.ioctl_avg_us_x10 / 10,
            report.ioctl_avg_us_x10 % 10,
            report.ioctl_max_us_x10 / 10,
            report.ioctl_max_us_x10 % 10,
            report.publish_avg_us_x10 / 10,
            report.publish_avg_us_x10 % 10,
            report.wait_avg_us_x10 / 10,
            report.wait_avg_us_x10 % 10,
            report.polls_avg_x10 / 10,
            report.polls_avg_x10 % 10,
            report.compute_avg_us_x10 / 10,
            report.compute_avg_us_x10 % 10,
            report.gpu_avg_us_x10 / 10,
            report.gpu_avg_us_x10 % 10,
            report.gpu_max_us_x10 / 10,
            report.gpu_max_us_x10 % 10,
            report.occupancy_pct_x10 / 10,
            report.occupancy_pct_x10 % 10,
            report.ioctl_pct_x10 / 10,
            report.ioctl_pct_x10 % 10,
            report.tick_khz,
        );
    }

    /// Name the UAPI check that refused a submission. Without this the ioctl
    /// only reports a bare EINVAL, and every validation in `g17_uapi` looks
    /// alike from userspace.
    fn reject_uapi(
        submission_id: u64,
        stage: &str,
        error: g17_uapi::UapiTranslateError,
    ) -> kernel::error::Error {
        pr_info!(
            "G17P submit: UAPI REJECT submission {} at {} check={}\n",
            submission_id,
            stage,
            error.name()
        );
        EINVAL
    }

    /// Log and return any error raised on the submit path outside UAPI
    /// translation. `reject_uapi` only covers parse/translate; everything from
    /// the sync-count check to the manager publish used to fail silently, which
    /// is why a submission could die with a bare EINVAL and no marker at all.
    fn stage_fail(submission_id: u64, stage: &str, error: kernel::error::Error) -> kernel::error::Error {
        pr_info!(
            "G17P submit: FAIL submission {} stage={} errno={}\n",
            submission_id,
            stage,
            error.to_errno()
        );
        error
    }

    /// Walk the raw command buffer and report every command header, without
    /// validating anything. This says what a submission actually CONTAINS --
    /// render vs compute vs attachment-set, sizes and barriers -- so a
    /// submission that dies before the parse loop is still identifiable.
    fn log_cmdbuf_headers(submission_id: u64, raw: &[u8]) {
        // Every line inside is already gated, but the walk itself ran on every
        // submission to produce nothing. Leave the whole thing out of the
        // steady-state path.
        if !crate::util::g17p_verbose() {
            return;
        }
        let mut offset = 0usize;
        let mut index = 0u32;
        while offset + g17_uapi::UAPI_COMMAND_HEADER_SIZE <= raw.len() {
            let header = &raw[offset..offset + g17_uapi::UAPI_COMMAND_HEADER_SIZE];
            let command_type = u16::from_le_bytes([header[0], header[1]]);
            let size = u16::from_le_bytes([header[2], header[3]]);
            let vdm_barrier = u16::from_le_bytes([header[4], header[5]]);
            let cdm_barrier = u16::from_le_bytes([header[6], header[7]]);
            if crate::util::g17p_verbose() {
                pr_info!(
                    "G17P submit: submission {} cmd[{}] type={} size={} vdm_barrier={:#x} cdm_barrier={:#x} at offset {}\n",
                    submission_id,
                    index,
                    command_type,
                    size,
                    vdm_barrier,
                    cdm_barrier,
                    offset,
                );
            }
            offset += g17_uapi::UAPI_COMMAND_HEADER_SIZE + size as usize;
            index += 1;
            if index > 8 {
                if crate::util::g17p_verbose() {
                    pr_info!("G17P submit: submission {} cmd list truncated\n", submission_id);
                }
                return;
            }
        }
        if offset != raw.len() {
            if crate::util::g17p_verbose() {
                pr_info!(
                    "G17P submit: submission {} cmd walk ended at {} of {} bytes (ragged)\n",
                    submission_id,
                    offset,
                    raw.len(),
                );
            }
        }
    }

    fn prune_render_frontier(&mut self) {
        // All command failure paths publish sticky VM status before signalling;
        // an errored render may be dropped here without treating it as success.
        self.render_frontier.retain(|(_, fence)| {
            // SAFETY: the retained Fence owns the pointer for this status read.
            unsafe { bindings::dma_fence_get_status(fence.raw()) == 0 }
        });
    }

    /// Queue a compute command with its required render prefix and an owned
    /// member of the ioctl output aggregate. Input fence scope is unchanged.
    fn submit_compute_async(
        &mut self, submission_id: u64, translated: g17_uapi::TranslatedComputeCommand,
        mut syncs: KVec<file::SyncItem>, in_sync_count: usize,
        objects: core::pin::Pin<&xarray::XArray<KBox<file::Object>>>,
        aggregate_member: G17PSubmitCompletionMember,
        vm_job: mmu::T8140VmJobGuard,
        entered: Option<Instant<Monotonic>>,
    ) -> Result {
        self.ensure_async_compute_queue()?;
        self.runtime.mutex().lock().check_async_compute_submission(self.id)?;
        let destinations = [
            Self::timestamp_destination(objects, translated.timestamps.start)?,
            Self::timestamp_destination(objects, translated.timestamps.end)?,
        ];
        let timestamps = self.owned_compute_timestamps()?;
        let output = self.fence_contexts.new_fence(0, G17PSubmissionFence)?.into();
        self.prune_render_frontier();
        let backend = self.async_backend.as_ref().ok_or(EIO)?;
        // One extra dependency carries this logical queue's preceding render
        // or compute completion. DRM entities serialize compute jobs with one
        // another, but render publication uses a separate watch entity; the
        // explicit fence is what preserves Vulkan queue order across engines.
        let prefix_count = self.render_frontier.iter().filter(|(sequence, _)|
            crate::g17_submission::g17p_render_prefix_contains(translated.render_wait_through, *sequence)
        ).count();
        let mut dependencies = KVec::with_capacity(in_sync_count + 1 + prefix_count, GFP_KERNEL)?;
        let mut checked_dependencies = KVec::with_capacity(in_sync_count + 1 + prefix_count, GFP_KERNEL)?;
        append_render_prefix_dependencies(
            &self.render_frontier, translated.render_wait_through,
            &mut dependencies, &mut checked_dependencies,
        )?;
        for sync in syncs.drain(0..in_sync_count) {
            if let Some(fence) = sync.fence {
                checked_dependencies.push(fence.clone(), GFP_KERNEL)?;
                dependencies.push(fence, GFP_KERNEL)?;
            }
        }
        if self.last_submission_engine != Some(G17PSubmissionEngine::Compute) {
            if let Some(prior) = self.last_submission.as_ref() {
                checked_dependencies.push(prior.clone(), GFP_KERNEL)?;
                dependencies.push(prior.clone(), GFP_KERNEL)?;
            }
        }
        let completion = g17_job::G17PJobCompletion::new(
            output,
            timestamps,
            destinations,
            self.vm.status().clone(),
            vm_job,
            aggregate_member,
        )?;
        let packet = g17_job::G17PComputeJobPacket::new(
            submission_id, backend.hardware_queue, backend.context.clone(), translated,
            KVec::new(), KVec::new(), checked_dependencies,
            Some(completion),
        )?;
        let finished = self.async_scheduler.as_mut().ok_or(EIO)?.enqueue(packet, dependencies)?;
        // The job is queued and owns its completion from here: give userspace
        // the fence that signals when the GPU has actually run it.
        self.last_submission = Some(finished.clone());
        self.last_submission_engine = Some(G17PSubmissionEngine::Compute);
        if crate::util::g17p_verbose() {
            pr_info!("G17P async submit: QUEUED submission {} owner={} qid={} context={}\n",
                submission_id, self.id, backend.hardware_queue, backend.context.context_id());
        }
        if let Some(entered) = entered {
            if let Some(report) = self
                .registry
                .record_compute(entered.elapsed().as_nanos().max(0) as u64)
            {
                self.report_perf(report);
            }
        }
        Ok(())
    }

    /// Queue render publication behind its explicit syncs and this Vulkan
    /// queue's preceding engine work. No physical render slot is consumed by
    /// the ioctl; the scheduler worker acquires the stable owner slot only
    /// after every dependency has retired successfully.
    fn submit_render_async(
        &mut self, submission_id: u64, translated: g17_uapi::TranslatedRenderCommand,
        mut syncs: KVec<file::SyncItem>, in_sync_count: usize,
        objects: core::pin::Pin<&xarray::XArray<KBox<file::Object>>>,
        aggregate_member: G17PSubmitCompletionMember,
        vm_job: mmu::T8140VmJobGuard,
        entered: Option<Instant<Monotonic>>,
    ) -> Result {
        self.prune_render_frontier();
        self.render_frontier.reserve(1, GFP_KERNEL)?;
        let timestamps = [
            Self::timestamp_destination(objects, translated.vertex_timestamps.start)
                .map_err(|error| Self::stage_fail(submission_id, "ts-vtx-start", error))?,
            Self::timestamp_destination(objects, translated.vertex_timestamps.end)
                .map_err(|error| Self::stage_fail(submission_id, "ts-vtx-end", error))?,
            Self::timestamp_destination(objects, translated.fragment_timestamps.start)
                .map_err(|error| Self::stage_fail(submission_id, "ts-frag-start", error))?,
            Self::timestamp_destination(objects, translated.fragment_timestamps.end)
                .map_err(|error| Self::stage_fail(submission_id, "ts-frag-end", error))?,
        ];
        let hardware_fence: UserFence<G17PSubmissionFence> = self
            .fence_contexts
            .new_fence(0, G17PSubmissionFence)
            .map_err(|error| Self::stage_fail(submission_id, "new-render-fence", error))?
            .into();
        let completion = G17PRenderCompletion::new(
            hardware_fence,
            timestamps,
            self.vm.status().clone(),
            vm_job,
            submission_id,
            translated.vdm_base,
            self.id,
            translated.render_order,
            aggregate_member,
        )?;
        let mut dependencies = KVec::with_capacity(in_sync_count + 1, GFP_KERNEL)?;
        let mut checked_dependencies = KVec::with_capacity(in_sync_count + 1, GFP_KERNEL)?;
        for sync in syncs.drain(0..in_sync_count) {
            if let Some(fence) = sync.fence {
                checked_dependencies.push(fence.clone(), GFP_KERNEL)?;
                dependencies.push(fence, GFP_KERNEL)?;
            }
        }
        if self.last_submission_engine != Some(G17PSubmissionEngine::Render) {
            if let Some(prior) = self.last_submission.as_ref() {
                checked_dependencies.push(prior.clone(), GFP_KERNEL)?;
                dependencies.push(prior.clone(), GFP_KERNEL)?;
            }
        }
        let packet = g17_job::G17PRenderJobPacket::new(
            submission_id, translated, checked_dependencies, completion,
        )?;
        let finished = self.render_scheduler.as_mut().ok_or(ENODEV)?
            .enqueue(packet, dependencies)?;
        self.last_submission = Some(finished.clone());
        self.last_submission_engine = Some(G17PSubmissionEngine::Render);
        self.render_sequence = translated.render_order.sequence;
        self.render_frontier.push_within_capacity((self.render_sequence, finished.clone()))
            .map_err(|_| ENOMEM)?;
        if crate::util::g17p_verbose() {
            pr_info!("G17P render submit: QUEUED submission {} owner={} context={}\n",
                submission_id, self.id, self.context.context_id());
        }
        if let Some(entered) = entered {
            let mut sample = g17_perf::RenderSample::default();
            sample.ioctl_ns = entered.elapsed().as_nanos().max(0) as u64;
            if let Some(report) = self.registry.record_render(sample) {
                self.report_perf(report);
            }
        }
        Ok(())
    }

    /// Dump the render payload exactly as userspace supplied it, before any
    /// validation runs. A rejected submit never reaches the GPU, so this is
    /// the only place the offending field is visible -- and printing it
    /// unconditionally means one hardware run diagnoses any check, not just
    /// the one that happens to fire first.
    fn log_render_payload(submission_id: u64, payload: &g17_uapi::UapiRenderCommand) {
        if crate::util::g17p_verbose() {
            pr_info!(
                "G17P render uapi[{}]: flags={:#x} {}x{} layers={} utile={}x{} samples={} sample_size={} tib_cfg={:#x}\n",
                submission_id,
                payload.flags,
                payload.width,
                payload.height,
                payload.layers,
                payload.utile_width,
                payload.utile_height,
                payload.samples,
                payload.sample_size,
                payload.ppp_control,
            );
        }
        if crate::util::g17p_verbose() {
            pr_info!(
                "G17P render uapi[{}]: vdm={:#x} scissor={:#x} dbias={:#x} oclqry={:#x} sampler={:#x}/{} bgobjdepth={:#x} bgobjvals={:#x}\n",
                submission_id,
                payload.vdm_base,
                payload.scissor_base,
                payload.depth_bias_base,
                payload.occlusion_query_base,
                payload.sampler_heap,
                payload.sampler_count,
                payload.depth_clear,
                payload.stencil_clear,
            );
        }
        if crate::util::g17p_verbose() {
            pr_info!(
                "G17P render uapi[{}]: depth={:#x}/{:#x} stride={:#x}/{:#x} stencil={:#x}/{:#x} stride={:#x}/{:#x} zls={:#x}\n",
                submission_id,
                payload.depth.base,
                payload.depth.compression_base,
                payload.depth.stride,
                payload.depth.compression_stride,
                payload.stencil.base,
                payload.stencil.compression_base,
                payload.stencil.stride,
                payload.stencil.compression_stride,
                payload.zls_control,
            );
        }
        if crate::util::g17p_verbose() {
            pr_info!(
                "G17P render uapi[{}]: bg={:#x}/{:#x} eot={:#x}/{:#x} pbg={:#x}/{:#x} peot={:#x}/{:#x}\n",
                submission_id,
                payload.background.usc,
                payload.background.resource_spec,
                payload.end_of_tile.usc,
                payload.end_of_tile.resource_spec,
                payload.partial_background.usc,
                payload.partial_background.resource_spec,
                payload.partial_end_of_tile.usc,
                payload.partial_end_of_tile.resource_spec,
            );
        }
        if crate::util::g17p_verbose() {
            pr_info!(
                "G17P render uapi[{}]: vhelper={:#x}/{:#x}/{:#x} fhelper={:#x}/{:#x}/{:#x}\n",
                submission_id,
                payload.vertex_helper.binary,
                payload.vertex_helper.config,
                payload.vertex_helper.data,
                payload.fragment_helper.binary,
                payload.fragment_helper.config,
                payload.fragment_helper.data,
            );
        }
    }

    fn wait_fence(fence: &Fence) -> Result {
        // SAFETY: `fence.raw()` remains valid for both calls. The completed
        // fence status must be consumed as well as its signal bit: an error
        // fence counts as signalled, but it does not satisfy a GPU dependency.
        let ret = unsafe {
            bindings::dma_fence_wait_timeout(
                fence.raw(),
                true,
                kernel::task::MAX_SCHEDULE_TIMEOUT,
            )
        };
        if ret < 0 {
            return Err(Error::from_errno(ret as i32));
        }
        if ret == 0 {
            return Err(ETIMEDOUT);
        }
        let status = unsafe { bindings::dma_fence_get_status(fence.raw()) };
        if status < 0 {
            return Err(Error::from_errno(status));
        }
        Ok(())
    }

    fn timestamp_destination(
        objects: core::pin::Pin<&xarray::XArray<KBox<file::Object>>>,
        timestamp: g17_uapi::UapiTimestamp,
    ) -> Result<Option<TimestampDestination>> {
        if timestamp.handle == 0 {
            return Ok(None);
        }
        let guard = objects.lock();
        let object = guard
            .get(timestamp.handle.try_into()?)
            .ok_or(ENOENT)?
            .clone();
        core::mem::drop(guard);
        match object {
            file::Object::TimestampBuffer(mapping) => {
                let offset: usize = timestamp.offset.try_into()?;
                let end = offset.checked_add(8).ok_or(EINVAL)?;
                if end > mapping.size() {
                    return Err(ERANGE);
                }
                Ok(Some(TimestampDestination { mapping, offset }))
            }
        }
    }

    fn owned_compute_timestamps(&self) -> Result<mmu::KernelMapping> {
        // Firmware writes both completion words. A WB CPU alias can keep the
        // polling core on stale zeroes even after the GPU has completed.
        let mut object = gem::new_kernel_object_wc(&self.dev, mmu::UAT_PGSZ)?;
        object.vmap()?.memset(0);
        object.map_into_range(
            &self.vm,
            T8140_TIMESTAMP_ALIAS_RANGE.clone(),
            mmu::UAT_PGSZ as u64,
            mmu::PROT_GPU_FW_SHARED_RW,
            false,
        )
    }

    fn install_output_fences(mut syncs: impl Iterator<Item = file::SyncItem>, fence: &Fence) {
        for mut sync in syncs.by_ref() {
            if let Some(chain) = sync.chain_fence.take() {
                sync.syncobj.add_point(chain, fence, sync.timeline_value);
            } else {
                sync.syncobj.replace_fence(Some(fence));
            }
        }
    }
}

/// Diagnostic byte dumps for submitted command streams.
impl G17PQueue {
    /// Compare a TTBAT entry against a page-table root.
    ///
    /// TTB0 carries the root physical address plus a valid bit and the
    /// context's ASID, so only the address field is comparable.  The mask is
    /// the firmware's own declared level mask (`g17_initdata::UAT_LEVEL_PHYS_MASK`).
    const TTB_ADDR_MASK: u64 = 0x0000_03ff_ffff_c000;

    fn log_hex(label: &str, base: u64, bytes: &[u8]) {
        for (index, chunk) in bytes.chunks(16).enumerate() {
            pr_info!(
                "G17P vm-audit: {} {:#x} {:02x?}\n",
                label,
                base.wrapping_add((index * 16) as u64),
                chunk,
            );
        }
    }

}

/// Publish the scheduled job's fence, or a signalled error fence on failure.
/// Once output syncobjs are resolved, every exit must give waiters a fence.
struct G17POutputFencePublication<'a> {
    fence: &'a UserFence<G17PSubmissionFence>,
    outputs: KVec<file::SyncItem>,
    failure: Error,
    succeeded: bool,
    aggregate: Option<Arc<G17PSubmitCompletion>>,
}

impl G17POutputFencePublication<'_> {
    /// Preserve the actual submit error in the fence userspace will wait on.
    fn failed(&mut self, error: Error) {
        self.failure = error;
    }

    /// Hand the output syncobjs to an asynchronous completion fence.
    ///
    /// An asynchronously scheduled submission has *not* run when the ioctl
    /// returns, so neither of this guard's own answers is correct for it:
    /// signalling success would tell userspace the GPU had finished work it
    /// has not started, and the ECANCELED failure fence would claim work that
    /// is queued and will run was thrown away. Both read as "signalled" to a
    /// DRM syncobj wait, so either one lets the client observe the destination
    /// buffer before the job that fills it has executed.
    ///
    /// Install the job's real completion fence instead, and disarm the guard:
    /// from here the async job owns the promise made to userspace.
    fn publish_async(&mut self, fence: &Fence) {
        G17PQueue::install_output_fences(self.outputs.drain(..), fence);
        self.succeeded = true;
    }
}

impl Drop for G17POutputFencePublication<'_> {
    fn drop(&mut self) {
        if let Some(aggregate) = self.aggregate.take() {
            aggregate.finish_enqueue(if self.succeeded { Ok(()) } else { Err(self.failure) });
        }
        if !self.succeeded {
            self.fence.set_error(self.failure);
        }
        let fence = Fence::from_fence(self.fence);
        G17PQueue::install_output_fences(self.outputs.drain(..), &fence);
        self.fence.signal();
    }
}

impl queue::Queue for G17PQueue {
    /// Every synchronous submission funnels through here so that no exit path
    /// can leave the client's own outstanding work unaccounted for.
    ///
    /// `submit_guarded` already promises an output fence for THIS ioctl on
    /// every path. What it cannot promise is anything about work this client
    /// handed the hardware earlier: once a failure is terminal the client's
    /// device is lost, Mesa tears its context down, and the jobs already in
    /// flight will never complete or signal. Settle them here.
    fn submit(
        &mut self,
        submission_id: u64,
        syncs: KVec<file::SyncItem>,
        in_sync_count: usize,
        cmdbuf_raw: &[u8],
        objects: core::pin::Pin<&xarray::XArray<KBox<file::Object>>>,
    ) -> Result {
        self.submit_guarded(submission_id, syncs, in_sync_count, cmdbuf_raw, objects)
    }
}

impl G17PQueue {
    /// Settle a client whose submission failed terminally.
    ///
    /// Only failures that mean "this work will never run on this device" count.
    /// Contention and rejection do not: an EBUSY from the render lease, an
    /// interrupted wait, or a malformed command buffer all leave a perfectly
    /// healthy VM, and recording a sticky error for one of those would retire a
    /// client that merely lost a frame.
    fn note_submission_failure(&mut self, submission_id: u64, error: Error) {
        let lost = error == EIO
            || error == ETIMEDOUT
            || error == ENODEV
            || self.vm.status().get() != 0;
        if !lost {
            return;
        }
        // Sticky first: a syncobj waiter woken by an error fence asks the VM
        // status why, and must not find a clean one.
        self.vm.status().record(error.to_errno());
        let failed = self
            .runtime
            .mutex()
            .lock()
            .fail_outstanding_jobs_for_vm(self.vm.status(), error);
        if failed != 0 {
            dev_err!(
                self.dev.as_ref(),
                "G17P submit: submission {} lost the device ({:?}); failed {} outstanding job(s) on vm {}\n",
                submission_id,
                error,
                failed,
                self.vm.id(),
            );
        }
    }

    fn submit_guarded(
        &mut self,
        submission_id: u64,
        mut syncs: KVec<file::SyncItem>,
        in_sync_count: usize,
        cmdbuf_raw: &[u8],
        objects: core::pin::Pin<&xarray::XArray<KBox<file::Object>>>,
    ) -> Result {
        // Keep diagnostic timing entirely out of the production hot path.
        // The parameter has no runtime permissions, so this branch is stable
        // for the lifetime of the module.
        let perf_enabled = *module_parameters::g17p_perf_window.value() != 0;
        let mut entered = if perf_enabled {
            Some(Instant::<Monotonic>::now())
        } else {
            None
        };
        if crate::util::g17p_verbose() {
            pr_info!(
                "G17P submit: ENTER submission {} syncs={} in_sync={} cmdbuf_bytes={}\n",
                submission_id,
                syncs.len(),
                in_sync_count,
                cmdbuf_raw.len()
            );
        }
        Self::log_cmdbuf_headers(submission_id, cmdbuf_raw);
        if in_sync_count > syncs.len() {
            return Err(Self::stage_fail(submission_id, "sync-count", EINVAL));
        }
        // Split the output syncobjs off immediately and hand them to a
        // publication guard, so every failure from here on still publishes a
        // fence. Arming this only after the queue lease was acquired left the
        // whole class of "another client owns the render slot" failures
        // publishing nothing, and a compositor that hit one waited on an
        // unsignalled syncobj forever instead of losing a frame.
        let early_output_fence: UserFence<G17PSubmissionFence> = self
            .fence_contexts
            .new_fence(0, G17PSubmissionFence)
            .map_err(|error| Self::stage_fail(submission_id, "new-fence", error))?
            .into();
        let output_count = syncs.len() - in_sync_count;
        let deferred_outputs: KVec<file::SyncItem> =
            match KVec::with_capacity(output_count, GFP_KERNEL) {
                Ok(outputs) => outputs,
                Err(_) => {
                    // Output syncobjs are already resolved and userspace may
                    // wait on them even when this bookkeeping allocation
                    // fails. Publish the fence directly from the original
                    // vector before returning the allocation error.
                    early_output_fence.set_error(ENOMEM);
                    let fence = Fence::from_fence(&early_output_fence);
                    Self::install_output_fences(syncs.drain(in_sync_count..), &fence);
                    early_output_fence.signal();
                    return Err(Self::stage_fail(
                        submission_id, "output-sync-list", ENOMEM,
                    ));
                }
            };
        let mut publication = G17POutputFencePublication {
            fence: &early_output_fence,
            outputs: deferred_outputs,
            failure: ECANCELED,
            succeeded: false,
            aggregate: None,
        };
        while syncs.len() > in_sync_count {
            // Removing the first output keeps timeline points in userspace
            // order and, unlike a Drain iterator, leaves every later output
            // reachable if the supposedly infallible capacity invariant is
            // ever violated.
            let item = match syncs.remove(in_sync_count) {
                Ok(item) => item,
                Err(_) => {
                    let failure = Self::stage_fail(submission_id, "output-sync-index", EIO);
                    publication.failed(failure);
                    let fence = Fence::from_fence(&early_output_fence);
                    Self::install_output_fences(syncs.drain(in_sync_count..), &fence);
                    self.note_submission_failure(submission_id, failure);
                    return Err(failure);
                }
            };
            if let Err(error) = publication.outputs.push_within_capacity(item) {
                let failure = Self::stage_fail(submission_id, "output-sync-capacity", EIO);
                publication.failed(failure);
                let fence = Fence::from_fence(&early_output_fence);
                Self::install_output_fences(core::iter::once(error.0), &fence);
                Self::install_output_fences(syncs.drain(in_sync_count..), &fence);
                self.note_submission_failure(submission_id, failure);
                return Err(failure);
            }
        }
        // Keep the publication guard alive until the exact result is known.
        // Terminal VM state and outstanding jobs must be settled before an
        // error fence wakes userspace; wrapping the body also makes every `?`
        // and explicit return obey that ordering without duplicating cleanup.
        let result = (|| -> Result {
        // A VM that has already faulted rejects every later submission. That
        // check used to run before any of the above, so a client whose VM died
        // got EIO with no fence published at all and then waited forever on the
        // syncobj it had just handed us. Keep the rejection, but make it
        // publish like every other failure.
        if self.vm.status().get() != 0 {
            return Err(Self::stage_fail(submission_id, "vm-status", EIO));
        }
        // Pin every userspace mapping before translating raw GPU addresses.
        // GEM-handle close and VM teardown defer their unmaps behind this
        // guard; a successfully published render/compute packet inherits it.
        let parse_guard = self.vm.retain_t8140_job()?;
        let address_space = UserAddressSpace {
            vm: &self.vm,
            range: self.user_range.clone(),
        };
        let window = g17_uapi::QueueUscWindow {
            base: self.usc_exec_base,
            user_start: self.user_range.start,
            user_end: self.user_range.end,
        };
        let mut parser = g17_uapi::UapiCommandParser::new(cmdbuf_raw);
        let mut render_preceding = 0u64;
        let mut pending = KVec::with_capacity(
            file::MAX_COMMANDS_PER_SUBMISSION as usize,
            GFP_KERNEL,
        )?;
        while let Some(command) = parser
            .next_hardware()
            .map_err(|error| Self::reject_uapi(submission_id, "parse", error))?
        {
            let command = match command {
                g17_uapi::ParsedHardwareCommand::Render {
                    header,
                    payload,
                    vertex_attachments,
                    fragment_attachments,
                    ..
                } => {
                    Self::log_render_payload(submission_id, &payload);
                    // Dump Mesa's VDM control stream. The tiler provably
                    // FETCHES it -- pipe-vdm-stream reads back the encoded
                    // pointer on pipes 0 and 2 -- and then bins 60 non-zero
                    // bytes into a 1.28 MB tile buffer, whose content is an
                    // empty bounding box (0x7fc00000 NaN and 0x7fffffff
                    // extremes). So the stream is reached and executed and
                    // yields zero primitives. Its first bytes say whether it
                    // contains a draw at all.
                    if *module_parameters::g17p_vdm_dump.value() != 0 {
                        // 1 KiB, not 128 bytes: the G17P native bind tail
                        // (hk_g17p_emit_native_bind_tail) emits its bind0
                        // structure INLINE into this stream and reaches past
                        // offset 0x340, so a 128-byte window stops before the
                        // PPP/graphics-core state that decides whether
                        // primitives survive.
                        let mut head = [0u8; 1024];
                        match self.vm.read_bytes(payload.vdm_base, &mut head) {
                            Ok(()) => Self::log_hex("vdm", payload.vdm_base, &head),
                            Err(error) => pr_info!(
                                "G17P vdm dump: read {:#x} failed ({:?})\n",
                                payload.vdm_base,
                                error
                            ),
                        }
                        // g17p_vm_probe_va was compute-only. The render path
                        // needs it more: the VDM stream embeds the vertex
                        // buffer address, and NaN bounds in the tile buffer
                        // point at what the shader READS rather than at the
                        // stream itself.
                        let probe = *module_parameters::g17p_vm_probe_va.value();
                        if probe != 0 {
                            let mut buf = [0u8; 64];
                            match self.vm.read_bytes(probe, &mut buf) {
                                Ok(()) => Self::log_hex("vdm-probe", probe, &buf),
                                Err(error) => pr_info!(
                                    "G17P vdm probe: read {:#x} failed ({:?})\n",
                                    probe,
                                    error
                                ),
                            }
                        }
                    }
                    let mut translated = g17_uapi::translate_render_command(
                            payload,
                            vertex_attachments,
                            fragment_attachments,
                            window,
                            &address_space,
                            g17_uapi::RenderInternalState {
                                parameters: {
                                    let mut p =
                                        g17_render::G17pRenderParameters::source_rsrc8();
                                    if crate::g17_config::RENDER_BG_PREFIX != 0 {
                                        p.bg_resource_prefix = 0xffff_8000_0000_0000;
                                    }
                                    p.native_pm_bytes =
                                        crate::g17_resources::g17p_render_native_pm_bytes();
                                    p.native_ta_registers =
                                        crate::g17_config::RENDER_NATIVE_REGS;
                                    p.gpc_perf_state_map = self.gpc_perf_state_map;
                                    p.gpc_perf_state_map_low = self.gpc_perf_state_map_low;
                                    p.gpc_perf_state_control = self.gpc_perf_state_control;
                                    p
                                },
                            },
                        )
                        .map_err(|error| Self::reject_uapi(submission_id, "render", error))?;
                    translated.render_barrier = header.render_barrier;
                    translated.compute_barrier = header.compute_barrier;
                    translated.render_order = crate::g17_submission::g17p_render_order(
                        self.render_sequence, render_preceding, header.render_barrier,
                    ).map_err(|_| Self::stage_fail(submission_id, "render-index", EOVERFLOW))?;
                    render_preceding += 1;
                    G17PPendingCommand::Render(translated)
                }
                g17_uapi::ParsedHardwareCommand::Compute {
                    payload, attachments, header, ..
                } => {
                    let mut translated = g17_uapi::translate_compute_command(
                        payload, attachments, window, &address_space,
                    ).map_err(|error| Self::reject_uapi(submission_id, "compute", error))?;
                    translated.render_wait_through = crate::g17_submission::g17p_render_order(
                        self.render_sequence, render_preceding, header.render_barrier,
                    ).map_err(|_| Self::stage_fail(submission_id, "compute-render-index", EOVERFLOW))?
                        .wait_through;
                    G17PPendingCommand::Compute(translated)
                },
            };
            pending.push_within_capacity(command).map_err(|_| ENOMEM)?;
        }
        parser
            .finish()
            .map_err(|error| Self::reject_uapi(submission_id, "finish", error))?;
        if pending.is_empty() {
            return Err(Self::stage_fail(
                submission_id,
                "no-hardware-command",
                EINVAL,
            ));
        }
        if crate::util::g17p_verbose() {
            let kind = if pending.len() == 1 { "single" } else { "batch" };
            pr_info!(
                "G17P submit: submission {} translated as {}\n",
                submission_id,
                kind
            );
        }

        // Keep every mapping pinned before publishing the first job.  The
        // final completion would be sufficient for a totally ordered queue,
        // but one guard per command also preserves exact failure quarantine
        // ownership if a later command cannot be published.
        let command_count = pending.len();
        let mut vm_jobs = KVec::with_capacity(command_count, GFP_KERNEL)?;
        vm_jobs
            .push_within_capacity(parse_guard)
            .map_err(|_| ENOMEM)?;
        while vm_jobs.len() < command_count {
            let guard = self.vm.retain_t8140_job()?;
            vm_jobs.push_within_capacity(guard).map_err(|_| ENOMEM)?;
        }

        let aggregate = G17PSubmitCompletion::new(
            self.fence_contexts.new_fence(0, G17PSubmissionFence)?.into(),
        )?;
        // Export the aggregate before enqueue: partial failure still leaves a
        // fence which covers every already queued command plus the enqueue gate.
        Self::install_output_fences(publication.outputs.drain(..), &aggregate.fence());
        publication.aggregate = Some(aggregate.clone());

        for (index, command) in pending.drain(..).enumerate() {
            let first = index == 0;
            let last = index + 1 == command_count;
            let command_syncs = if first {
                core::mem::replace(&mut syncs, KVec::new())
            } else {
                KVec::new()
            };
            let command_in_sync_count = if first { in_sync_count } else { 0 };
            let vm_job = vm_jobs.remove(0).map_err(|_| EIO)?;
            let command_entered = if last { entered.take() } else { None };

            match command {
                G17PPendingCommand::Render(translated) => self.submit_render_async(
                    submission_id,
                    translated,
                    command_syncs,
                    command_in_sync_count,
                    objects,
                    G17PSubmitCompletionMember::new(aggregate.clone()),
                    vm_job,
                    command_entered,
                )?,
                G17PPendingCommand::Compute(translated) => self.submit_compute_async(
                    submission_id,
                    translated,
                    command_syncs,
                    command_in_sync_count,
                    objects,
                    G17PSubmitCompletionMember::new(aggregate.clone()),
                    vm_job,
                    command_entered,
                )?,
            }
        }
        publication.publish_async(&aggregate.fence());
        Ok(())
        })();
        if let Err(error) = result {
            self.note_submission_failure(submission_id, error);
            publication.failed(error);
        }
        result
    }
}
