// SPDX-License-Identifier: GPL-2.0-only OR MIT

//! T8140 DRM frontend and userspace render queue.

use core::ops::Range;

use kernel::{
    bindings, c_str,
    dma_fence::{Fence, FenceContexts, FenceObject, FenceOps, RawDmaFence, UserFence},
    macros::vtable,
    prelude::*,
    static_lock_class,
    sync::{Arc, Mutex},
    uapi, xarray,
};

use crate::{
    alloc,
    driver::{AsahiDevRef, AsahiDevice},
    drm_gpu::{DrmGpu, DrmGpuParams},
    file, g17_adt_j700, g17_live_boot, g17_render, g17_uapi, gem, gpu, hw, mmu, queue,
};

const T8140_USER_START: u64 = mmu::UAT_PGSZ as u64;
const T8140_USER_TOP: u64 = 1 << 42;
const T8140_UNKNOWN_PAGE: u64 = T8140_USER_TOP - 2 * mmu::UAT_PGSZ as u64;
const T8140_TIMESTAMP_ALIAS_RANGE: Range<u64> = 0x70_0000_0000..0x71_0000_0000;

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

    fn mutex(&self) -> &Mutex<g17_live_boot::G17PLiveRuntime> {
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

    /// Give the hardware compute graph back when a client queue goes away.
    fn release_compute_owner(&self, owner: u64) {
        self.mutex().lock().release_compute_owner(owner);
    }

}

/// Ceiling on live DRM compute queue objects for this device.
///
/// The firmware's KSM restore set has 128 queue slots, and that is the number
/// of *hardware* QIDs a session could ever carry. This driver configures one
/// compute QID, so the real concurrency limit is the lease below -- one
/// submitting client at a time. The cap exists so a client that leaks queue
/// objects gets a clean `ENOSPC` from QUEUE_CREATE instead of exhausting
/// kernel memory one prepared graph at a time.
const G17P_MAX_COMPUTE_QUEUES: u32 = crate::g17_queue_limits::MAX_QUEUES;

/// Serializes the one hardware compute graph across DRM clients.
///
/// Held after input dependencies become ready, through GPU execution and
/// output-fence publication, including the doorbell wait -- which runs
/// with the runtime mutex released, so the runtime mutex alone does NOT keep
/// two clients out of each other's submission. Also taken when a queue object
/// is destroyed, so a release can never race a submit in flight.
///
/// Uncontended acquisition is one mutex round trip, so the single-client
/// steady-state path (which is the timing-sensitive one) is unchanged.
#[pin_data]
pub(crate) struct G17PQueueRegistry {
    /// Bounded physical ownership: two retained render pairs plus the one
    /// transient exclusive phase used while installing a compute graph.
    /// Steady-state compute readers do not occupy either render slot.
    #[pin]
    lease: Mutex<G17PQueueLeaseState>,
    #[pin]
    lease_free: CondVar,
    live_queues: AtomicU32,
    compute_readers: AtomicU32,
    compute_faulted: AtomicU32,
    /// Device-wide throughput accounting.
    ///
    /// `lease` bounds render to two physical pairs, so at most two threads
    /// reach this mutex and each holds it for a handful of
    /// integer updates -- no allocation, no formatting -- on a path that
    /// already takes several locks. It is skipped entirely when
    /// `g17p_perf_window` is 0.
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
/// QIDs 0/1 and 2/3 are independently retained physical queue pairs. Every
/// firmware-visible queue, stamp, QoS and completion field is derived from the
/// selected pair; the shared FList and outer-channel state remain device-wide.
const G17P_ACTIVE_RENDER_SLOTS: usize = G17P_RENDER_SLOTS;
const _: () = assert!(G17P_ACTIVE_RENDER_SLOTS > 0);
const _: () = assert!(G17P_ACTIVE_RENDER_SLOTS <= G17P_RENDER_SLOTS);

#[derive(Default)]
struct G17PQueueLeaseState {
    render_slots: u8,
    compute_install: bool,
    /// Logical queue that owns each render slot's retained storage, or 0 for a
    /// slot no client has claimed yet.
    ///
    /// The `G17PUserRenderStorage` behind a slot -- its descriptors, render
    /// state, TVB pool, parameter-management graph and application-GART root --
    /// belongs to one descriptor context. Every logical queue has its own
    /// descriptor context even when several queues share one VM. Handing the
    /// slot to a different queue forces
    /// `handoff_retained_render_vm` to tear that whole graph down and rebuild
    /// it for the incoming context. Recording the owner lets a queue come back to
    /// the storage it already built instead of taking whichever slot happens to
    /// be free, which is what made two concurrent clients rebuild each other's
    /// graph on every alternation.
    render_owner: [u64; G17P_RENDER_SLOTS],
    /// Acquisition order, used to pick a victim when more clients render than
    /// there are slots. Zero means never acquired.
    render_epoch: [u64; G17P_RENDER_SLOTS],
    /// Slots whose retained graph is known to be unusable: a render abandoned
    /// after a completion timeout leaves firmware potentially still running the
    /// group, and its host-side tracker set, so every later submission on that
    /// slot fails. Retiring the slot costs concurrency; continuing to hand it
    /// out costs the session, because each client that lands on it is told
    /// EBUSY and reports VK_ERROR_DEVICE_LOST.
    render_quarantined: u8,
    epoch: u64,
}

impl G17PQueueLeaseState {
    const fn render_mask() -> u8 {
        (1u8 << G17P_ACTIVE_RENDER_SLOTS) - 1
    }

    /// True once no render slot can ever be handed out again.
    fn render_exhausted(&self) -> bool {
        self.render_quarantined & Self::render_mask() == Self::render_mask()
    }

    fn take_render_slot(&mut self, slot: u8, owner: u64) -> u8 {
        self.render_slots |= 1u8 << slot;
        self.render_owner[slot as usize] = owner;
        self.epoch = self.epoch.wrapping_add(1);
        self.render_epoch[slot as usize] = self.epoch;
        slot
    }

    /// Pick a render slot for `owner`, preferring the one holding its own
    /// retained graph.
    ///
    /// Order: this owner's own slot, then a slot no client has claimed, then
    /// the least recently used free slot. The last case is a real handoff and
    /// is only reached when more clients render concurrently than there are
    /// slots; it is still strictly better than failing, because the handoff
    /// path works and an EBUSY does not.
    fn acquire_render_slot(&mut self, owner: u64) -> Option<u8> {
        let free = (!self.render_slots) & Self::render_mask() & !self.render_quarantined;
        if free == 0 {
            return None;
        }
        let usable = |slot: usize| free & (1u8 << slot) != 0;
        if owner != 0 {
            for slot in 0..G17P_RENDER_SLOTS {
                if usable(slot) && self.render_owner[slot] == owner {
                    return Some(self.take_render_slot(slot as u8, owner));
                }
            }
        }
        for slot in 0..G17P_RENDER_SLOTS {
            if usable(slot) && self.render_owner[slot] == 0 {
                return Some(self.take_render_slot(slot as u8, owner));
            }
        }
        let victim = (0..G17P_RENDER_SLOTS)
            .filter(|slot| usable(*slot))
            .min_by_key(|slot| self.render_epoch[*slot])?;
        Some(self.take_render_slot(victim as u8, owner))
    }

    fn release_render_slot(&mut self, slot: u8) {
        debug_assert!(slot < g17_resources::G17PRenderQueuePair::SLOT_COUNT);
        self.render_slots &= !(1u8 << slot);
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

    /// Independent compute jobs share the compute side of the gate; legacy
    /// render owns it exclusively until its context0 view routing is split.
    /// This protects the qualified render path without serializing compute
    /// queues against each other. The guard lives through exact retirement.
    fn acquire_compute(self: &Arc<Self>, first_install: bool) -> Result<G17PComputeLeaseGuard> {
        let mut held = self.lease.lock();
        while held.compute_install
            || (first_install
                && (held.render_slots != 0
                    || self.compute_readers.load(Ordering::Acquire) != 0))
        {
            if self.compute_faulted.load(Ordering::Acquire) != 0 { return Err(EIO); }
            if self.lease_free.wait_interruptible(&mut held) { return Err(ERESTARTSYS); }
        }
        if self.compute_faulted.load(Ordering::Acquire) != 0 { return Err(EIO); }
        self.compute_readers.fetch_add(1, Ordering::AcqRel);
        if first_install { held.compute_install = true; }
        drop(held);
        Ok(G17PComputeLeaseGuard { registry: self.clone(), exclusive: first_install })
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

    /// Block until this logical queue owns a render slot, preferring the slot that already
    /// holds its retained graph.
    ///
    /// `owner` is the logical queue id. Retained descriptors name that queue's
    /// independently allocated application-GART descriptor context, so two
    /// queues in one VM cannot share one graph. Waiting here is interruptible and
    /// unbounded by design -- a client that has to wait loses latency, whereas
    /// an EBUSY from this path reaches Mesa as VK_ERROR_DEVICE_LOST and takes
    /// the client's whole context with it.
    fn acquire_lease(self: &Arc<Self>, owner: u64) -> Result<G17PLeaseGuard> {
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
            if !held.compute_install {
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
        if held.render_quarantined & (1u8 << slot) != 0 {
            return;
        }
        held.render_quarantined |= 1u8 << slot;
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
        held.render_quarantined &= !(1u8 << slot);
    }
}

pub(crate) struct G17PComputeLeaseGuard { registry: Arc<G17PQueueRegistry>, exclusive: bool }
impl G17PComputeLeaseGuard {
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

/// Exact hardware completion for one published render.  The lease is owned by
/// the completion packet, not the submit ioctl, so neither file close nor a
/// fast return can hand the physical slot to another client before retirement.
pub(crate) struct G17PRenderCompletion {
    fence: UserFence<G17PSubmissionFence>,
    /// Vertex start/end and fragment start/end UAPI destinations.
    timestamps: [Option<TimestampDestination>; 4],
    status: Arc<crate::g17_status::VmStatus>,
    lease: Arc<Mutex<Option<G17PLeaseGuard>>>,
    vm_job: Arc<Mutex<Option<mmu::T8140VmJobGuard>>>,
    signalled: AtomicU32,
}

impl G17PRenderCompletion {
    fn new(
        fence: UserFence<G17PSubmissionFence>,
        timestamps: [Option<TimestampDestination>; 4],
        status: Arc<crate::g17_status::VmStatus>,
        lease: G17PLeaseGuard,
        vm_job: mmu::T8140VmJobGuard,
    ) -> Result<Arc<Self>> {
        let lease = Arc::pin_init(
            new_mutex!(Some(lease), "G17PRenderCompletion::lease"),
            GFP_KERNEL,
        )?;
        let vm_job = Arc::pin_init(
            new_mutex!(Some(vm_job), "G17PRenderCompletion::vm_job"),
            GFP_KERNEL,
        )?;
        Ok(Arc::new(Self {
            fence,
            timestamps,
            status,
            lease,
            vm_job,
            signalled: AtomicU32::new(0),
        }, GFP_KERNEL)?)
    }

    pub(crate) fn fence(&self) -> Fence { Fence::from_fence(&self.fence) }

    pub(crate) fn belongs_to_vm(
        &self,
        status: &Arc<crate::g17_status::VmStatus>,
    ) -> bool {
        Arc::ptr_eq(&self.status, status)
    }

    pub(crate) fn vm_status(&self) -> Arc<crate::g17_status::VmStatus> {
        self.status.clone()
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
}

impl G17PDrmGpu {
    pub(crate) fn new(
        runtime: G17PRuntimeRef,
        soc: &'static hw::agx3::SocConfig,
        observed: hw::GpuIdConfig,
    ) -> Self {
        Self {
            runtime,
            soc,
            observed,
            ids: Default::default(),
        }
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
        self.runtime.mutex().lock().is_crashed()
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
            max_commands_per_submission: 1,
        })
    }

    fn user_range(&self) -> Result<Range<u64>> {
        Ok(T8140_USER_START..T8140_UNKNOWN_PAGE)
    }

    fn unknown_page(&self) -> Result<u64> {
        Ok(T8140_UNKNOWN_PAGE)
    }

    fn base_clock_hz(&self) -> u32 {
        24_000_000
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
        Ok(KBox::new(
            G17PQueue::new(
                &dev,
                self.runtime,
                vm,
                self.ids.queue.next(),
                priority,
                usc_exec_base,
            )?,
            GFP_KERNEL,
        )?)
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
struct G17PSubmissionFence;

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
struct G17PQueue {
    dev: AsahiDevRef,
    runtime: G17PRuntimeRef,
    vm: mmu::Vm,
    user_range: Range<u64>,
    usc_exec_base: u64,
    fence_contexts: FenceContexts,
    id: u64,
}

enum G17PPendingCommand {
    Render(g17_uapi::TranslatedRenderCommand),
    Compute(g17_uapi::TranslatedComputeCommand),
}

struct TimestampDestination {
    mapping: Arc<mmu::KernelMapping>,
    offset: usize,
}

impl G17PQueue {
    fn new(
        dev: &AsahiDevice,
        runtime: G17PRuntimeRef,
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
        Ok(Self {
            dev: dev.into(),
            runtime,
            vm,
            user_range,
            usc_exec_base,
            fence_contexts: FenceContexts::new(1, QUEUE_NAME, QUEUE_CLASS_KEY)?,
            id,
        })
    }

    fn wait_input_fences(syncs: &[file::SyncItem]) -> Result {
        for sync in syncs {
            let Some(fence) = sync.fence.as_ref() else {
                continue;
            };
            // SAFETY: `fence.raw()` remains valid for the duration of this call.
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
        }
        Ok(())
    }

    fn acquire_ready_lease(
        &self, submission_id: u64, syncs: &[file::SyncItem],
    ) -> Result<G17PLeaseGuard> {
        Self::wait_input_fences(syncs)
            .map_err(|error| Self::stage_fail(submission_id, "wait-input-fences", error))?;
        // Key the slot on this logical queue, whose independently allocated
        // descriptor context is encoded throughout the retained render graph.
        // Sharing one VM does not make two queue contexts interchangeable.
        // Owner zero claims nothing, which is the historical lowest-free policy.
        let owner = if *module_parameters::g17p_render_slot_affinity.value() != 0 {
            self.id
        } else {
            0
        };
        self.registry.acquire_lease(owner)
            .map_err(|error| Self::stage_fail(submission_id, "acquire-lease", error))
    }

    fn join_queue_timeline(&mut self, submission_id: u64) -> Result {
        let Some(fence) = self.last_submission.as_ref() else {
            return Ok(());
        };
        Self::wait_fence(fence)
            .map_err(|error| Self::stage_fail(submission_id, "wait-queue-timeline", error))?;
        self.last_submission = None;
        // A poisoned VM status is how a settled-lost render tells every later
        // submission on this VM that the device is gone. It used to return a
        // bare EIO: every other failure in this function goes through
        // stage_fail, so a client whose VM had been poisoned by a recovery
        // stopped submitting with no kernel message at all, which is what made
        // a wedge look like a hang with an empty log.
        let status = self.vm.status().get();
        if status != 0 {
            return Err(Self::stage_fail(submission_id, "vm-status-poisoned", EIO));
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

    fn copy_timestamp(destination: Option<TimestampDestination>, value: u64) -> Result {
        destination.map_or(Ok(()), |destination| destination.write(value))
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

impl queue::Queue for G17PQueue {
    fn submit(
        &mut self,
        submission_id: u64,
        mut syncs: KVec<file::SyncItem>,
        in_sync_count: usize,
        cmdbuf_raw: &[u8],
        objects: core::pin::Pin<&xarray::XArray<KBox<file::Object>>>,
    ) -> Result {
        if in_sync_count > syncs.len() {
            return Err(EINVAL);
        }
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
        let mut pending = None;
        while let Some(command) = parser.next_hardware().map_err(|_| EINVAL)? {
            if pending.is_some() {
                return Err(ENOTSUPP);
            }
            pending = Some(match command {
                g17_uapi::ParsedHardwareCommand::Render {
                    payload,
                    vertex_attachments,
                    fragment_attachments,
                    ..
                } => G17PPendingCommand::Render(
                    g17_uapi::translate_render_command(
                        payload,
                        vertex_attachments,
                        fragment_attachments,
                        window,
                        &address_space,
                        g17_uapi::RenderInternalState {
                            parameters: g17_render::G17pRenderParameters::source_rsrc8(),
                        },
                    )
                    .map_err(|_| EINVAL)?,
                ),
                g17_uapi::ParsedHardwareCommand::Compute {
                    payload,
                    attachments,
                    ..
                } => G17PPendingCommand::Compute(
                    g17_uapi::translate_compute_command(
                        payload,
                        attachments,
                        window,
                        &address_space,
                    )
                    .map_err(|_| EINVAL)?,
                ),
            });
        }
        parser.finish().map_err(|_| EINVAL)?;
        let pending = pending.ok_or(EINVAL)?;

        Self::wait_input_fences(&syncs[..in_sync_count])?;
        let output_fence: UserFence<G17PSubmissionFence> = self
            .fence_contexts
            .new_fence(0, G17PSubmissionFence)?
            .into();
        match pending {
            G17PPendingCommand::Render(translated) => {
                // Firmware writes caller timestamp pointers only inside its
                // dedicated timestamp aperture, which this driver does not
                // configure. Publish the pass envelope the completion observes
                // instead, as compute publishes its completion record.
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
                if crate::util::g17p_verbose() {
                    pr_info!(
                        "G17P submit: submission {} render timestamps resolved, entering manager publish\n",
                        submission_id
                    );
                }
                let render_slot = lease.slot();
                let hardware_fence: UserFence<G17PSubmissionFence> = self
                    .fence_contexts
                    .new_fence(0, G17PSubmissionFence)
                    .map_err(|error| Self::stage_fail(submission_id, "new-render-fence", error))?
                    .into();
                let completion = G17PRenderCompletion::new(
                    hardware_fence,
                    timestamps,
                    self.vm.status().clone(),
                    lease,
                    vm_job,
                )?;
                let publish_started = Instant::<Monotonic>::now();
                let submit = self.runtime
                    .mutex()
                    .lock()
                    .submit_translated_render(
                        &self.vm,
                        render_slot,
                        true,
                        self.context.clone(),
                        &translated,
                        completion.clone(),
                    );
                sample.publish_ns = publish_started.elapsed().as_nanos().max(0) as u64;
                if let Err(error) = submit {
                    self.runtime.mutex().lock().timeout_deferred_render(
                        render_slot, submission_id, error,
                    );
                    return Err(Self::stage_fail(
                        submission_id, "submit-translated-render", error,
                    ));
                }
                let packet = match g17_job::G17PRenderWatchPacket::new(
                    submission_id, render_slot, completion,
                ) {
                    Ok(packet) => packet,
                    Err(error) => {
                        self.runtime.mutex().lock().timeout_deferred_render(
                            render_slot, submission_id, error,
                        );
                        return Err(Self::stage_fail(
                            submission_id, "allocate-render-watch", error,
                        ));
                    }
                };
                let finished = match self.render_scheduler.as_mut().ok_or(ENODEV)?.enqueue(packet) {
                    Ok(fence) => fence,
                    Err(error) => {
                        self.runtime.mutex().lock().timeout_deferred_render(
                            render_slot, submission_id, error,
                        );
                        return Err(Self::stage_fail(
                            submission_id, "schedule-render-watch", error,
                        ));
                    }
                };
                // Record queue order before exposing the fence through an
                // output syncobj. A woken peer may submit immediately.
                self.last_submission = Some(finished.clone());
                publication.publish_async(&finished);
                sample.ioctl_ns = entered.elapsed().as_nanos().max(0) as u64;
                if let Some(report) = self.registry.record_render(sample) {
                    self.report_perf(report);
                }
                return Ok(());
            }
            G17PPendingCommand::Compute(translated) => {
                let start = Self::timestamp_destination(objects, translated.timestamps.start)?;
                let end = Self::timestamp_destination(objects, translated.timestamps.end)?;
                let owned_timestamps = self.owned_compute_timestamps()?;
                let completion = self
                    .runtime
                    .mutex()
                    .lock()
                    .submit_translated_compute(&self.vm, &translated, owned_timestamps)?;
                Self::copy_timestamp(start, completion[0])?;
                Self::copy_timestamp(end, completion[1])?;
            }
        }

        output_fence.signal();
        let fence = Fence::from_fence(&output_fence);
        let outputs = syncs.drain(in_sync_count..);
        Self::install_output_fences(outputs, &fence);
        dev_dbg!(
            self.dev.as_ref(),
            "G17P queue {} completed synchronous submission {} (one hardware command)\n",
            self.id,
            submission_id
        );
        Ok(())
    }
}
