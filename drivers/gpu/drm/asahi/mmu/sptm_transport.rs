// SPDX-License-Identifier: GPL-2.0-only

//! Synchronous submission to the platform's SPTM service.
//!
//! Device unbind revokes the transport after draining its current batch.
//! Reference-counted client state can outlive unbind. GPUVM owns backing and
//! admission; neither transport errors nor confirmed command prefixes release
//! those owners. Complete plan validation and bounded command storage
//! allocation precede the first submission.

use super::sptm::{self, MapPages, MapPlan, Operation, UnmapPlan};
use core::sync::atomic::{AtomicU64, Ordering};
use kernel::{bindings, device, error::from_err_ptr, new_mutex, platform, prelude::*, sync::Mutex};

/// Read-only diagnostic accounting of provider private objects (opcode 1
/// create, opcode 7 destroy). Counters always run; logging is opt-in through
/// `sptm_space_trace`. `retained` counts address spaces whose owner dropped
/// them without a confirmed destroy (the provider object stays allocated).
static PRIVATE_CREATED: AtomicU64 = AtomicU64::new(0);
static PRIVATE_DESTROYED: AtomicU64 = AtomicU64::new(0);
static PRIVATE_RETAINED: AtomicU64 = AtomicU64::new(0);

pub(super) fn space_trace() -> bool {
    *crate::module_parameters::sptm_space_trace.value() != 0
}

pub(super) fn note_private(event: &str, handle: Option<u16>, detail: u64) {
    if !space_trace() { return; }
    let created = PRIVATE_CREATED.load(Ordering::Relaxed);
    let destroyed = PRIVATE_DESTROYED.load(Ordering::Relaxed);
    pr_info!("SPTM private {}: handle={:?} detail={:#x} created={} destroyed={} retained={} live={}\n",
        event, handle, detail, created, destroyed, PRIVATE_RETAINED.load(Ordering::Relaxed),
        created.saturating_sub(destroyed));
}

pub(super) fn note_private_retained(handle: Option<u16>, detail: u64) {
    PRIVATE_RETAINED.fetch_add(1, Ordering::Relaxed);
    note_private("retained", handle, detail);
}

/// Failure receipt. `completed` is a prefix of the complete submitted plan.
/// The next operation may also have changed provider state.
pub(super) struct Failure {
    pub(super) error: Error,
    pub(super) completed: usize,
    pub(super) submitted: bool,
    pub(super) may_have_mapped: bool,
}

// The existing provider transport copies a 16KiB batch with a 16-byte header.
// Keep that native wire geometry; only command storage is bounded, never the
// size or number of pages accepted by the complete mapping transaction.
const OPERATION_BATCH_MAX: usize = (0x4000 - 16) / core::mem::size_of::<Operation>();

/// Execute an already validated, allocation-free plan with reusable storage.
/// Failure receipts describe the whole plan, including earlier successful
/// batches. In particular, ENODEV after a prefix cannot release its backing.
///
/// The shared storage is locked per native batch, not for the whole plan. A
/// repeated (single-page) mapping is one provider operation per page, so a
/// sparse scratch bind of a large buffer is millions of operations; holding
/// the transport for all of them stalled every other address space's
/// provider work (VM creation, context binds needed by render publication)
/// for seconds. Plans for different address spaces are independent, and each
/// address space's own plans are already serialized by its owner.
fn submit_planned(
    mut plan: impl Iterator<Item = Operation>,
    storage: &Mutex<KVec<Operation>>,
    mut submit: impl FnMut(&mut [Operation]) -> core::result::Result<(), Failure>,
) -> core::result::Result<(), Failure> {
    let mut completed = 0usize;
    let mut submitted = false;
    let mut may_have_mapped = false;
    loop {
        let mut storage = storage.lock();
        assert!(!storage.is_empty());
        let mut count = 0;
        for slot in storage.iter_mut() {
            let Some(operation) = plan.next() else { break; };
            *slot = operation;
            count += 1;
        }
        if count == 0 { return Ok(()); }
        let operations = &mut storage[..count];
        if let Err(mut failure) = submit(operations) {
            failure.completed = failure.completed.saturating_add(completed);
            failure.submitted |= submitted;
            failure.may_have_mapped |= may_have_mapped;
            return Err(failure);
        }
        completed += count;
        submitted = true;
        may_have_mapped |= operations.iter().any(|op| matches!(op.opcode, 3 | 9));
    }
}

impl From<Error> for Failure {
    fn from(error: Error) -> Self {
        Self { error, completed: 0, submitted: false, may_have_mapped: false }
    }
}

#[repr(C)]
struct PlatformReboot {
    notifier: bindings::notifier_block,
    raw: *mut bindings::aurora_apif_gpu,
}

fn quiesce_platform(raw: *mut bindings::aurora_apif_gpu) {
    let mut gfx = Operation::new(17, [5, 0, 0, 0, 0]);
    let mut gfx_done = 0;
    // SAFETY: The notifier retains the transport client throughout this call.
    let gfx_status = unsafe { bindings::aurora_apif_gpu_submit(raw,
        core::ptr::addr_of_mut!(gfx).cast(), 1, &mut gfx_done) };
    pr_info!("M5 GFX voter quiesce: status={} completed={} native={}\n",
        gfx_status, gfx_done, gfx.result);
    let mut operation = Operation::new(17, [2, 0, 0, 0, 0]);
    let mut completed = 0;
    // SAFETY: The transport's client reference remains owned until its
    // blocking reboot notifier has been unregistered and drained.
    let status = unsafe { bindings::aurora_apif_gpu_submit(raw,
        core::ptr::addr_of_mut!(operation).cast(), 1, &mut completed) };
    pr_info!("M5 SLC quiesce: status={} completed={} native={} planes={}\n",
        status, completed, operation.result, operation.args[1]);
}

unsafe extern "C" fn platform_reboot(block: *mut bindings::notifier_block,
    _action: usize, _data: *mut core::ffi::c_void) -> core::ffi::c_int {
    // SAFETY: notifier is the first field of the registered, immovable box.
    // Transport::drop unregisters this blocking notifier before freeing it.
    let raw = unsafe { (*block.cast::<PlatformReboot>()).raw };
    quiesce_platform(raw);
    0
}

/// Provider features (op 18). Without negotiation the provider keeps its
/// legacy ABI: 255 handles and table frames from its own heap.
const FEATURES: u32 = 18;
const DONATION_QUERY: u32 = 19;
const FEATURE_DONATED_TABLES: u64 = 1;
const FEATURE_WIDE_HANDLES: u64 = 2;

pub(super) struct Transport {
    raw: *mut bindings::aurora_apif_gpu,
    reboot: Option<KBox<PlatformReboot>>,
    features: u64,
    // Mapping plans share one native-sized batch for this transport's lifetime.
    // The mutex covers one native batch; failure counts cover the whole plan.
    operations: Pin<KBox<Mutex<KVec<Operation>>>>,
}

// SAFETY: The C transport owns a client reference and serializes submission
// against devres revocation. It rejects access once its resources are revoked.
unsafe impl Send for Transport {}
// SAFETY: As above; no mutable transport state is accessed without that mutex.
unsafe impl Sync for Transport {}

const _: () = {
    assert!(core::mem::size_of::<Operation>() == core::mem::size_of::<bindings::aurora_apif_gpu_op>());
    assert!(core::mem::align_of::<Operation>() == core::mem::align_of::<bindings::aurora_apif_gpu_op>());
    assert!(core::mem::offset_of!(Operation, opcode) == core::mem::offset_of!(bindings::aurora_apif_gpu_op, op));
    assert!(core::mem::offset_of!(Operation, flags) == core::mem::offset_of!(bindings::aurora_apif_gpu_op, flags));
    assert!(core::mem::offset_of!(Operation, args) == core::mem::offset_of!(bindings::aurora_apif_gpu_op, arg));
    assert!(core::mem::offset_of!(Operation, result) == core::mem::offset_of!(bindings::aurora_apif_gpu_op, ret));
};

impl Transport {
    pub(super) fn new(device: &platform::Device<device::Bound>) -> Result<Self> {
        let mut operations = KVec::with_capacity(OPERATION_BATCH_MAX, GFP_KERNEL)?;
        for _ in 0..OPERATION_BATCH_MAX {
            operations.push_within_capacity(Operation::new(0, [0; 5]))
                .expect("reserved SPTM operation batch capacity");
        }
        let operations = KBox::pin_init(new_mutex!(operations, "SPTM operation batch"), GFP_KERNEL)?;
        // SAFETY: The typed platform device is bound throughout construction.
        // A successful call returns one client reference, released in Drop.
        let raw = unsafe {
            from_err_ptr(bindings::aurora_apif_gpu_get(
                device.as_ref().as_raw(), c"x1n1-uat".as_ptr(),
                bindings::AURORA_APIF_GPU_UAT_DOORBELL,
            ))
        }?;
        let mut transport = Self { raw, operations, reboot: None, features: 0 };
        if *crate::module_parameters::m5_platform_enable.value() != 0 {
            let mut reboot = KBox::new(PlatformReboot {
                notifier: bindings::notifier_block {
                    notifier_call: Some(platform_reboot), next: core::ptr::null_mut(), priority: -1000,
                }, raw,
            }, GFP_KERNEL)?;
            // SAFETY: The box's address remains fixed through registration;
            // drop unregisters and drains callbacks before releasing the client.
            let status = unsafe { bindings::register_reboot_notifier(
                core::ptr::addr_of_mut!(reboot.notifier)) };
            if status != 0 { return Err(Error::from_errno(status)); }
            transport.reboot = Some(reboot);
            for kind in 0..2 {
                let mut operation = Operation::new(17, [kind, 0, 0, 0, 0]);
                let result = transport.submit(core::slice::from_mut(&mut operation));
                pr_info!("M5 platform enable: kind={} result={:?} receipt={:x?} status={}\n",
                    kind, result.as_ref().map_err(|failure| failure.error), operation.args, operation.result);
                result.map_err(|failure| failure.error)?;
            }
            // G17 has powered AFR/GFX before constructing this transport.
            // Enable native dynamic voting once, before starting firmware;
            // submission never calls the platform service for power votes.
            let mut operation = Operation::new(17, [4, 0, 0, 0, 0]);
            let result = transport.submit(core::slice::from_mut(&mut operation));
            pr_info!("M5 GFX voter enable: result={:?} receipt={:x?} status={}\n",
                result.as_ref().map_err(|failure| failure.error), operation.args, operation.result);
            result.map_err(|failure| failure.error)?;
        }
        if *crate::module_parameters::m5_platform_max.value() != 0 {
            if transport.reboot.is_none() { return Err(EINVAL); }
            let mut operation = Operation::new(17, [3, 0, 0, 0, 0]);
            let result = transport.submit(core::slice::from_mut(&mut operation));
            pr_info!("M5 platform maximum: result={:?} receipt={:x?} status={}\n",
                result.as_ref().map_err(|failure| failure.error), operation.args, operation.result);
            result.map_err(|failure| failure.error)?;
        }
        // Native XNU allocates UAT table pages from ordinary kernel memory and
        // has no fixed address-space count. A provider without op 18 rejects
        // it; the legacy ABI then applies.
        let mut operation = Operation::new(FEATURES,
            [FEATURE_DONATED_TABLES | FEATURE_WIDE_HANDLES, 0, 0, 0, 0]);
        match transport.submit(core::slice::from_mut(&mut operation)) {
            Ok(()) => {
                transport.features = operation.result & (FEATURE_DONATED_TABLES | FEATURE_WIDE_HANDLES);
                pr_info!("SPTM UAT features {:#x}: handle limit {} heap headroom {} MiB donated live {} quarantined {} peak {}\n",
                    transport.features, operation.args[1], operation.args[2] >> 20, operation.args[3],
                    operation.args[4], operation.args[5]);
            }
            Err(_) => pr_info!("SPTM UAT features: legacy provider (status {:#x})\n", operation.result),
        }
        if *crate::module_parameters::m5_platform_probe.value() != 0 {
            for (kind, count) in [(0, 16), (1, 9), (2, 4), (3, 1), (4, 5), (6, 4)] {
                for index in 0..count {
                    let mut operation = Operation::new(16, [kind, index, 0, 0, 0]);
                    let result = transport.submit(core::slice::from_mut(&mut operation));
                    let failed = result.is_err();
                    pr_info!("M5 platform probe: kind={} index={} result={:?} receipt={:x?} status={}\n",
                        kind, index, result.map_err(|failure| failure.error), operation.args, operation.result);
                    if failed { break; }
                }
            }
        }
        Ok(transport)
    }

    /// Table frames are guest pages (native pmap_alloc_iommu_page model).
    pub(super) fn donated_tables(&self) -> bool { self.features & FEATURE_DONATED_TABLES != 0 }

    fn wide_handles(&self) -> bool { self.features & FEATURE_WIDE_HANDLES != 0 }

    /// Where a donated page is now: 0 guest, 1 provider table, 2 quarantined.
    pub(super) fn donation_state(&self, physical: u64) -> Result<u64> {
        let mut operation = Operation::new(DONATION_QUERY, [physical, 0, 0, 0, 0]);
        self.submit(core::slice::from_mut(&mut operation)).map_err(|failure| failure.error)?;
        Ok(operation.result)
    }

    /// Submit independent operations and report how many completed. Every
    /// completed operation's receipt is in `result`; the operation at index
    /// `completed` (if any) failed or never ran, and later ones never ran.
    pub(super) fn submit_receipts(&self, operations: &mut [Operation]) -> (usize, Result) {
        match self.submit(operations) {
            Ok(()) => (operations.len(), Ok(())),
            Err(failure) => (failure.completed.min(operations.len()), Err(failure.error)),
        }
    }

    fn submit(&self, operations: &mut [Operation]) -> core::result::Result<(), Failure> {
        let count = u32::try_from(operations.len()).map_err(|_| Failure::from(EOVERFLOW))?;
        let mut completed = 0;
        let trace = *crate::module_parameters::sptm_trace.value() != 0;
        let start = if trace { unsafe { bindings::ktime_get() } } else { 0 };
        let opcodes = if trace { operations.iter().fold(0u64, |mask, op|
            mask | 1u64.checked_shl(op.opcode).unwrap_or(0)) } else { 0 };
        // SAFETY: The assertions above prove wire layout equivalence; the
        // exclusive slice remains valid throughout synchronous copy-in/out.
        let status = unsafe {
            bindings::aurora_apif_gpu_submit(self.raw, operations.as_mut_ptr().cast(), count, &mut completed)
        };
        if trace {
            let elapsed = unsafe { bindings::ktime_get() }.saturating_sub(start);
            pr_info!("SPTM submit: count={} opcodes={:#x} arg1={:#x} ns={} status={} completed={}\n",
                count, opcodes, operations.first().map_or(0, |op| op.args[1]), elapsed, status, completed);
        }
        if status == 0 && completed == count {
            return Ok(());
        }
        // ENODEV is returned before issuing any operation after revocation.
        if status == ENODEV.to_errno() && completed == 0 {
            return Err(ENODEV.into());
        }
        Err(Failure {
            error: EIO,
            completed: completed as usize,
            submitted: true,
            may_have_mapped: sptm::may_have_mapped(operations, completed as usize),
        })
    }

    /// Borrow retained command storage separately for each native batch.
    /// No command-storage allocation occurs during mapping or unmapping.
    fn submit_plan(&self, plan: impl Iterator<Item = Operation> + Clone)
        -> core::result::Result<(), Failure>
    {
        if plan.clone().next().is_none() { return Ok(()); }
        submit_planned(plan, &self.operations, |batch| self.submit(batch))
    }

    /// MapPlan validates the whole range/backing before producing commands.
    pub(super) fn map(&self, plan: MapPlan<'_>) -> core::result::Result<(), Failure> {
        self.submit_plan(plan)
    }

    /// A validated multi-run mapping batch (table operations first, then the
    /// runs' leaf operations), sharing the native-sized command batches.
    pub(super) fn map_ops(&self, plan: impl Iterator<Item = Operation> + Clone)
        -> core::result::Result<(), Failure>
    {
        self.submit_plan(plan)
    }

    /// Backing remains owned by the caller until the whole unmap succeeds.
    pub(super) fn unmap(&self, plan: UnmapPlan) -> core::result::Result<(), Failure> {
        self.submit_plan(plan)
    }

    /// Validate every sparse rollback range before the first unmap. Iteration
    /// then reconstructs those infallible plans without allocating storage.
    fn unmap_ranges(&self, space: sptm::Space, ranges: &[core::ops::Range<u64>])
        -> core::result::Result<(), Failure>
    {
        if ranges.is_empty() { return Ok(()); }
        for range in ranges {
            let size = range.end.checked_sub(range.start).ok_or(Failure::from(EINVAL))?;
            if size % sptm::PAGE != 0 { return Err(EINVAL.into()); }
            UnmapPlan::new(space, range.start, size / sptm::PAGE)
                .map_err(|_| Failure::from(EINVAL))?;
        }
        let plan = ranges.iter().flat_map(|range| {
            UnmapPlan::new(space, range.start, (range.end - range.start) / sptm::PAGE)
                .expect("SPTM unmap ranges validated before publication")
        });
        self.submit_plan(plan)
    }
}

impl Drop for Transport {
    fn drop(&mut self) {
        if let Some(mut reboot) = self.reboot.take() {
            // SAFETY: This is the exact registered block. The blocking chain
            // drains any running callback before returning success.
            let status = unsafe { bindings::unregister_reboot_notifier(
                core::ptr::addr_of_mut!(reboot.notifier)) };
            if status != 0 {
                pr_err!("M5 platform notifier retained after unregister failure {}\n", status);
                core::mem::forget(reboot);
                return;
            }
            quiesce_platform(self.raw);
        }

        // SAFETY: Construction acquired this client reference. No submission
        // can borrow self after its final owner has begun dropping it.
        unsafe { bindings::aurora_apif_gpu_put(self.raw) };
    }
}

/// A private provider object and the transport that owns its identity.
/// Page-table owners must explicitly retire/unmap/unbind before `destroy`.
/// Dropping this token does not issue potentially unsafe implicit cleanup.
#[must_use]
pub(super) struct PrivateSpace {
    transport: kernel::sync::Arc<Transport>,
    state: sptm::PrivateState,
}

impl Transport {
    pub(super) fn create_private(transport: &kernel::sync::Arc<Self>) -> Result<PrivateSpace> {
        let mut operations = [Operation::new(1, [0; 5])];
        if let Err(failure) = transport.submit(&mut operations) {
            note_private("create-failed", None, operations[0].result);
            return Err(failure.error);
        }
        let state = sptm::PrivateState::new(operations[0].result, transport.wide_handles()).map_err(|_| {
            note_private("create-invalid", None, operations[0].result);
            EIO
        })?;
        PRIVATE_CREATED.fetch_add(1, Ordering::Relaxed);
        note_private("create", Some(state.handle()), 0);
        Ok(PrivateSpace { transport: transport.clone(), state })
    }
}

impl PrivateSpace {
    fn transition_error(error: sptm::TransitionError<Failure>) -> Error {
        match error {
            sptm::TransitionError::Invalid => EINVAL,
            sptm::TransitionError::Provider(failure) => failure.error,
        }
    }

    pub(super) fn state(&self) -> sptm::ContextState { self.state.state() }

    pub(super) fn bind(&mut self, context: u8) -> Result {
        self.state.bind(context, |op| self.transport.submit(core::slice::from_mut(op)))
            .map_err(Self::transition_error)?;
        self.sync_context()
    }

    pub(super) fn unbind(&mut self) -> Result {
        self.state.unbind(|op| self.transport.submit(core::slice::from_mut(op)))
            .map_err(Self::transition_error)
    }

    pub(super) fn handle(&self) -> u16 { self.state.handle() }

    pub(super) fn transport(&self) -> &Transport { &self.transport }

    /// Returns the provider receipt (see [`sptm::PrivateState::destroy`]).
    pub(super) fn destroy(&mut self) -> Result<u64> {
        let handle = self.state.handle();
        let result = self.state.destroy(|op| self.transport.submit(core::slice::from_mut(op)))
            .map_err(Self::transition_error);
        match &result {
            Ok(_) => {
                PRIVATE_DESTROYED.fetch_add(1, Ordering::Relaxed);
                note_private("destroy", Some(handle), 0);
            }
            Err(error) => note_private("destroy-failed", Some(handle), error.to_errno() as u64),
        }
        result
    }

    /// Install tables backed by donated guest pages ([`sptm::donated_table`]).
    /// Preflight refusals leave pages with the guest. A failed native transition
    /// may quarantine the first incomplete operation's page; the page-table
    /// owner queries that page before releasing it.
    pub(super) fn map_tables(&mut self, operations: &mut [Operation]) -> (usize, Result) {
        if self.state.space().is_err() { return (0, Err(EIO)); }
        self.transport.submit_receipts(operations)
    }

    pub(super) fn map(&mut self, address: u64, pages: MapPages<'_>, protection: sptm::Protection)
        -> core::result::Result<(), Failure>
    {
        let space = self.state.space().map_err(|_| Failure::from(EIO))?;
        // Donated tables are installed by the page table before the leaves.
        let plan = if self.transport.donated_tables() {
            MapPlan::without_tables(space, address, pages, protection)
        } else {
            MapPlan::new(space, address, pages, protection)
        }.map_err(|_| Failure::from(EINVAL))?;
        let result = self.transport.map(plan).and_then(|()| {
            self.sync_context().map_err(|error| Failure {
                error, completed: 0, submitted: true, may_have_mapped: true,
            })
        });
        if result.as_ref().is_err_and(|failure| failure.may_have_mapped) { self.state.poison(); }
        result
    }

    /// The platform performs ASID visibility maintenance in its native host
    /// translation regime. Guest direct-UAT invalidations are not equivalent.
    /// The receipt proves the requested identity and unchanged host regime.
    pub(super) fn sync_context(&mut self) -> Result {
        let context = match self.state.state() {
            sptm::ContextState::Unbound => return Ok(()),
            sptm::ContextState::Bound(context) => context,
            _ => return Err(EIO),
        };
        let sptm::Space::Private(handle) = self.state.space().map_err(|_| EIO)? else {
            return Err(EINVAL);
        };
        let mut operation = Operation::new(14, [handle.into(), context.into(), 0, 0, 0]);
        let result = self.transport.submit(core::slice::from_mut(&mut operation))
            .map_err(|failure| failure.error).and_then(|()| {
                let args = operation.args;
                let required = (1u64 << 34) | (1u64 << 27);
                if operation.result != 0 || args[0] != u64::from(handle)
                    || args[1] != u64::from(context) || args[2] & required != required
                    || args[3] != 0 || args[4] != args[2] || args[5] != 0 {
                    return Err(EIO);
                }
                Ok(())
            });
        if result.is_err() { self.state.poison(); }
        result
    }

    /// Several runs of one mapping in shared provider batches, followed by a
    /// single ASID visibility sync. The caller validated every run and built
    /// `plan` for this space; a failure poisons exactly as [`Self::map`] does.
    pub(super) fn map_batch(&mut self, plan: impl Iterator<Item = Operation> + Clone)
        -> core::result::Result<(), Failure>
    {
        self.state.space().map_err(|_| Failure::from(EIO))?;
        let result = self.transport.map_ops(plan).and_then(|()| {
            self.sync_context().map_err(|error| Failure {
                error, completed: 0, submitted: true, may_have_mapped: true,
            })
        });
        if result.as_ref().is_err_and(|failure| failure.may_have_mapped) { self.state.poison(); }
        result
    }

    pub(super) fn unmap(&mut self, address: u64, pages: u64) -> Result {
        let space = self.state.space().map_err(|_| EIO)?;
        let plan = UnmapPlan::new(space, address, pages).map_err(|_| EINVAL)?;
        if let Err(failure) = self.transport.unmap(plan) {
            // Planning allocation can fail before any provider operation.
            // Such a rejection leaves the existing mappings authoritative.
            if failure.submitted { self.state.poison(); }
            return Err(failure.error);
        }
        Ok(())
    }

    pub(super) fn unmap_ranges(&mut self, ranges: &[core::ops::Range<u64>]) -> Result {
        let space = self.state.space().map_err(|_| EIO)?;
        if let Err(failure) = self.transport.unmap_ranges(space, ranges) {
            if failure.submitted { self.state.poison(); }
            return Err(failure.error);
        }
        Ok(())
    }
}

/// A mapping owner in either the firmware's global object or a private object.
/// Global ownership covers only this driver's mappings, never the provider's
/// object or its roots. One UAT owns this token and serializes its mapping shadow.
pub(super) enum AddressSpace {
    Private(PrivateSpace),
    Global { transport: kernel::sync::Arc<Transport>, uncertain: bool },
}

impl AddressSpace {
    pub(super) fn sync_context(&mut self) -> Result {
        match self {
            Self::Private(space) => space.sync_context(),
            Self::Global { .. } => Err(EINVAL),
        }
    }

    /// Diagnostic identity of a private provider object.
    pub(super) fn private_handle(&self) -> Option<u16> {
        match self { Self::Private(space) => Some(space.handle()), Self::Global { .. } => None }
    }
    pub(super) fn private(transport: &kernel::sync::Arc<Transport>) -> Result<Self> {
        Ok(Self::Private(Transport::create_private(transport)?))
    }

    pub(super) fn global(transport: &kernel::sync::Arc<Transport>) -> Self {
        Self::Global { transport: transport.clone(), uncertain: false }
    }

    pub(super) fn range(&self) -> core::ops::Range<u64> {
        match self {
            Self::Private(_) => 0..(1 << 42),
            Self::Global { .. } => sptm::GLOBAL_RANGE,
        }
    }

    pub(super) fn state(&self) -> sptm::ContextState {
        match self {
            Self::Private(space) => space.state(),
            Self::Global { uncertain: true, .. } => sptm::ContextState::Uncertain(None),
            Self::Global { uncertain: false, .. } => sptm::ContextState::Unbound,
        }
    }

    pub(super) fn bind(&mut self, context: u8) -> Result {
        match self { Self::Private(space) => space.bind(context), Self::Global { .. } => Err(EINVAL) }
    }

    pub(super) fn unbind(&mut self) -> Result {
        match self { Self::Private(space) => space.unbind(), Self::Global { .. } => Err(EINVAL) }
    }

    /// Returns the provider receipt (0 for the global space).
    pub(super) fn destroy(&mut self) -> Result<u64> {
        match self {
            Self::Private(space) => space.destroy(),
            Self::Global { uncertain: false, .. } => Ok(0),
            Self::Global { uncertain: true, .. } => Err(EIO),
        }
    }

    /// The private space when its tables are guest-donated frames.
    pub(super) fn donating(&mut self) -> Option<&mut PrivateSpace> {
        match self {
            Self::Private(space) if space.transport.donated_tables() => Some(space),
            _ => None,
        }
    }

    pub(super) fn map(&mut self, address: u64, pages: MapPages<'_>, protection: sptm::Protection)
        -> core::result::Result<(), Failure>
    {
        match self {
            Self::Private(space) => space.map(address, pages, protection),
            Self::Global { transport, uncertain } => {
                if *uncertain { return Err(EIO.into()); }
                let plan = MapPlan::new(sptm::Space::Global, address, pages, protection)
                    .map_err(|_| Failure::from(EINVAL))?;
                let result = transport.map(plan);
                if result.as_ref().is_err_and(|failure| failure.may_have_mapped) { *uncertain = true; }
                result
            }
        }
    }

    /// The provider object this space's plans must name.
    pub(super) fn plan_space(&self) -> Result<sptm::Space> {
        match self {
            Self::Private(space) => space.state.space().map_err(|_| EIO),
            Self::Global { uncertain: false, .. } => Ok(sptm::Space::Global),
            Self::Global { uncertain: true, .. } => Err(EIO),
        }
    }

    /// See [`PrivateSpace::map_batch`]; the shared space needs no ASID sync.
    pub(super) fn map_batch(&mut self, plan: impl Iterator<Item = Operation> + Clone)
        -> core::result::Result<(), Failure>
    {
        match self {
            Self::Private(space) => space.map_batch(plan),
            Self::Global { transport, uncertain } => {
                if *uncertain { return Err(EIO.into()); }
                let result = transport.map_ops(plan);
                if result.as_ref().is_err_and(|failure| failure.may_have_mapped) { *uncertain = true; }
                result
            }
        }
    }

    pub(super) fn unmap(&mut self, address: u64, pages: u64) -> Result {
        let end = address.checked_add(pages.checked_mul(sptm::PAGE).ok_or(EINVAL)?).ok_or(EINVAL)?;
        self.unmap_ranges(&[address..end])
    }

    pub(super) fn unmap_ranges(&mut self, ranges: &[core::ops::Range<u64>]) -> Result {
        match self {
            Self::Private(space) => space.unmap_ranges(ranges),
            Self::Global { transport, uncertain } => {
                if *uncertain { return Err(EIO); }
                if let Err(failure) = transport.unmap_ranges(sptm::Space::Global, ranges) {
                    if failure.submitted { *uncertain = true; }
                    return Err(failure.error);
                }
                Ok(())
            }
        }
    }
}
