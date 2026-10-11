// SPDX-License-Identifier: GPL-2.0-only

//! Synchronous submission to the platform's SPTM service.
//!
//! Device unbind revokes the transport after draining its current batch.
//! Reference-counted client state can outlive unbind. GPUVM owns backing and
//! admission; neither transport errors nor confirmed command prefixes release
//! those owners. Complete plan validation and bounded command storage
//! allocation precede the first submission.

use super::sptm::{self, MapPages, MapPlan, Operation, UnmapPlan};
use kernel::{bindings, device, error::from_err_ptr, new_mutex, platform, prelude::*, sync::Mutex};

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
fn submit_planned(
    mut plan: impl Iterator<Item = Operation>,
    storage: &mut [Operation],
    mut submit: impl FnMut(&mut [Operation]) -> core::result::Result<(), Failure>,
) -> core::result::Result<(), Failure> {
    assert!(!storage.is_empty());
    let mut completed = 0usize;
    let mut submitted = false;
    let mut may_have_mapped = false;
    loop {
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
    let _gfx_status = unsafe { bindings::aurora_apif_gpu_submit(raw,
        core::ptr::addr_of_mut!(gfx).cast(), 1, &mut gfx_done) };
    let mut operation = Operation::new(17, [2, 0, 0, 0, 0]);
    let mut completed = 0;
    // SAFETY: The transport's client reference remains owned until its
    // blocking reboot notifier has been unregistered and drained.
    let _status = unsafe { bindings::aurora_apif_gpu_submit(raw,
        core::ptr::addr_of_mut!(operation).cast(), 1, &mut completed) };
}

unsafe extern "C" fn platform_reboot(block: *mut bindings::notifier_block,
    _action: usize, _data: *mut core::ffi::c_void) -> core::ffi::c_int {
    // SAFETY: notifier is the first field of the registered, immovable box.
    // Transport::drop unregisters this blocking notifier before freeing it.
    let raw = unsafe { (*block.cast::<PlatformReboot>()).raw };
    quiesce_platform(raw);
    0
}

pub(super) struct Transport {
    raw: *mut bindings::aurora_apif_gpu,
    reboot: Option<KBox<PlatformReboot>>,
    // Mapping plans share one native-sized batch for this transport's lifetime.
    // The mutex covers the complete plan, including its failure prefix.
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
        let mut transport = Self { raw, operations, reboot: None };
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
                result.map_err(|failure| failure.error)?;
            }
            // G17 has powered AFR/GFX before constructing this transport.
            // Enable native dynamic voting once, before starting firmware;
            // submission never calls the platform service for power votes.
            let mut operation = Operation::new(17, [4, 0, 0, 0, 0]);
            let result = transport.submit(core::slice::from_mut(&mut operation));
            result.map_err(|failure| failure.error)?;
        }
        if *crate::module_parameters::m5_platform_max.value() != 0 {
            if transport.reboot.is_none() { return Err(EINVAL); }
            let mut operation = Operation::new(17, [3, 0, 0, 0, 0]);
            let result = transport.submit(core::slice::from_mut(&mut operation));
            result.map_err(|failure| failure.error)?;
        }
        Ok(transport)
    }

    fn submit(&self, operations: &mut [Operation]) -> core::result::Result<(), Failure> {
        let count = u32::try_from(operations.len()).map_err(|_| Failure::from(EOVERFLOW))?;
        let mut completed = 0;
        // SAFETY: The assertions above prove wire layout equivalence; the
        // exclusive slice remains valid throughout synchronous copy-in/out.
        let status = unsafe {
            bindings::aurora_apif_gpu_submit(self.raw, operations.as_mut_ptr().cast(), count, &mut completed)
        };
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

    /// Borrow the retained native-sized batch for the complete mapping plan.
    /// No command-storage allocation occurs during mapping or unmapping.
    fn submit_plan(&self, plan: impl Iterator<Item = Operation> + Clone)
        -> core::result::Result<(), Failure>
    {
        if plan.clone().next().is_none() { return Ok(()); }
        let mut operations = self.operations.lock();
        submit_planned(plan, &mut operations, |batch| self.submit(batch))
    }

    /// MapPlan validates the whole range/backing before producing commands.
    pub(super) fn map(&self, plan: MapPlan<'_>) -> core::result::Result<(), Failure> {
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
        transport.submit(&mut operations).map_err(|failure| failure.error)?;
        let state = sptm::PrivateState::new(operations[0].result).map_err(|_| EIO)?;
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

    pub(super) fn destroy(&mut self) -> Result {
        self.state.destroy(|op| self.transport.submit(core::slice::from_mut(op)))
            .map_err(Self::transition_error)
    }

    pub(super) fn map(&mut self, address: u64, pages: MapPages<'_>, protection: sptm::Protection)
        -> core::result::Result<(), Failure>
    {
        let space = self.state.space().map_err(|_| Failure::from(EIO))?;
        let plan = MapPlan::new(space, address, pages, protection).map_err(|_| Failure::from(EINVAL))?;
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

    pub(super) fn destroy(&mut self) -> Result {
        match self {
            Self::Private(space) => space.destroy(),
            Self::Global { uncertain: false, .. } => Ok(()),
            Self::Global { uncertain: true, .. } => Err(EIO),
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
