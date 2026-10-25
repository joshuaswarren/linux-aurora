// SPDX-License-Identifier: GPL-2.0-only OR MIT

//! Hardware completion fences and the aggregate fence of one submit ioctl.
//!
//! These fences can complete out of order, so each has an independent timeline.
//! Signalling belongs outside the device lock, after timestamp and VM-status writes.

use super::context::{Context, WorkStateLease};
use crate::file::SyncItem;
use core::sync::atomic::{AtomicBool, AtomicI32, AtomicU32, AtomicUsize, Ordering};
use kernel::{
    bindings, c_str,
    dma_fence::{Fence, FenceContexts, FenceObject, FenceOps, RawDmaFence, UserFence},
    new_mutex,
    prelude::*,
    str::CStr,
    sync::{Arc, Mutex},
};

pub(crate) struct CompletionFence;

pub(crate) struct ExportFence {
    source: Arc<super::sync_source::Source>,
}

static EXPORT_OPS: AtomicUsize = AtomicUsize::new(0);

#[vtable]
impl FenceOps for ExportFence {
    fn get_driver_name<'a>(self: &'a FenceObject<Self>) -> &'a CStr { c_str!("asahi") }
    fn get_timeline_name<'a>(self: &'a FenceObject<Self>) -> &'a CStr {
        c_str!("g17-compute-source")
    }
}

fn exported(contexts: &FenceContexts, source: Arc<super::sync_source::Source>) -> Result<UserFence<ExportFence>> {
    let fence = contexts.new_fence(0, ExportFence { source })?;
    // SAFETY: This is an owned, initialized fence which has not been published.
    let ops = unsafe {
        (*fence.raw()).context = bindings::dma_fence_context_alloc(1);
        (*fence.raw()).ops as usize
    };
    match EXPORT_OPS.compare_exchange(0, ops, Ordering::AcqRel, Ordering::Acquire) {
        Ok(_) => {},
        Err(previous) if previous == ops => {},
        Err(_) => return Err(EIO),
    }
    Ok(fence.into())
}

/// The exact registered vtable, rather than a driver/timeline name, identifies
/// our payload. The retained input fence owns the object during this clone.
pub(crate) fn exported_source(fence: &Fence) -> Option<Arc<super::sync_source::Source>> {
    // SAFETY: The owned Fence retains its object for the complete lookup.
    unsafe { exported_source_raw(fence.raw()) }
}

/// The caller retains a reference to this fence or its owning chain node.
pub(super) unsafe fn exported_source_raw(raw: *mut bindings::dma_fence) -> Option<Arc<super::sync_source::Source>> {
    let expected = EXPORT_OPS.load(Ordering::Acquire);
    if expected == 0 { return None; }
    // SAFETY: Fence owns a live dma_fence. FenceObject is repr(C) with that
    // fence as its first field. Only ExportFence allocations use this vtable;
    // its distinctive callback and nontrivial payload drop prevent aliasing a
    // different fence payload. No cast is made before the exact identity check.
    unsafe {
        if (*raw).ops as usize != expected { return None; }
        let object = &*(raw as *const FenceObject<ExportFence>);
        Some(object.source.clone())
    }
}

enum AggregateFence {
    Plain(UserFence<CompletionFence>),
    Export(UserFence<ExportFence>),
}

impl AggregateFence {
    fn fence(&self) -> Fence {
        match self {
            Self::Plain(fence) => Fence::from_fence(fence),
            Self::Export(fence) => Fence::from_fence(fence),
        }
    }
    fn set_error(&self, error: Error) {
        match self { Self::Plain(fence) => fence.set_error(error), Self::Export(fence) => fence.set_error(error) }
    }
    fn signal(&self) {
        match self { Self::Plain(fence) => fence.signal(), Self::Export(fence) => fence.signal() }
    }
    fn source(&self) -> Option<&Arc<super::sync_source::Source>> {
        match self { Self::Plain(_) => None, Self::Export(fence) => Some(&fence.source) }
    }
}

#[vtable]
impl FenceOps for CompletionFence {
    fn get_driver_name<'a>(self: &'a FenceObject<Self>) -> &'a CStr {
        c_str!("asahi")
    }

    fn get_timeline_name<'a>(self: &'a FenceObject<Self>) -> &'a CStr {
        c_str!("g17-submit")
    }
}

/// Give independently retired work a timeline that fence merging cannot subsume.
pub(crate) fn independent(contexts: &FenceContexts) -> Result<UserFence<CompletionFence>> {
    let fence = contexts.new_fence(0, CompletionFence)?;
    // SAFETY: The fence is unique and unpublished: there are no waiters, callbacks or
    // shared references. The DMA-fence allocator supplies a globally unique context.
    unsafe { (*fence.raw()).context = bindings::dma_fence_context_alloc(1) };
    Ok(fence.into())
}

/// An immutable failure fence allocated before the queue is exposed to ioctls.
/// Reusing it for rejected submissions cannot retain or subsume accepted work.
pub(crate) fn allocation_failure(contexts: &FenceContexts) -> Result<Fence> {
    let fence = independent(contexts)?;
    fence.set_error(ENOMEM);
    fence.signal();
    Ok(Fence::from_fence(&fence))
}

/// All command members of an ioctl share its work-state node and aggregate fence.
/// The initial member keeps the aggregate unsignalled until enqueue finishes.
#[pin_data]
pub(crate) struct Submission {
    fence: AggregateFence,
    /// Successful retirement drops only this bookkeeping owner; runtime owners
    /// and failed submissions keep their work-state allocation.
    #[pin]
    work_state: Mutex<Option<Arc<WorkStateLease>>>,
    remaining: AtomicU32,
    error: AtomicI32,
}

impl Submission {
    fn new(fence: AggregateFence, context: Arc<Context>) -> Result<Arc<Self>> {
        let work_state = Arc::pin_init(WorkStateLease::deferred(context), GFP_KERNEL)?;
        Arc::pin_init(
            pin_init!(Self {
                fence,
                work_state <- new_mutex!(Some(work_state), "G17 submission work state"),
                remaining: AtomicU32::new(1),
                error: AtomicI32::new(0),
            }),
            GFP_KERNEL,
        )
    }

    pub(crate) fn fence(&self) -> Fence {
        self.fence.fence()
    }

    /// Add one command before enqueue ends. The parser bounds the count to 64.
    pub(crate) fn member(self: &Arc<Self>) -> Member {
        self.remaining.fetch_add(1, Ordering::Relaxed);
        Member {
            submission: self.clone(),
            complete: AtomicBool::new(false),
        }
    }

    fn member_done(&self, result: Result) {
        if let Err(error) = result {
            let _ = self.error.compare_exchange(
                0,
                error.to_errno(),
                Ordering::AcqRel,
                Ordering::Acquire,
            );
        }
        if self.remaining.fetch_sub(1, Ordering::AcqRel) == 1 {
            let error = self.error.load(Ordering::Acquire);
            if let Some(source) = self.fence.source() {
                source.complete(if error == 0 { Ok(()) } else { Err(Error::from_errno(error)) });
            }
            if error != 0 {
                // Failure-path diagnostic: the ioctl's out-syncobj fence
                // signals with this error (first 8, then powers of two).
                static ERRORS: AtomicU32 = AtomicU32::new(0);
                let n = ERRORS.fetch_add(1, Ordering::Relaxed) + 1;
                if n <= 8 || n.is_power_of_two() {
                    pr_info!("asahi: submission fence signalled with error {} (error fence {})\n",
                        error, n);
                }
                self.fence.set_error(Error::from_errno(error));
            } else {
                // Scheduler cleanup can wait behind submission on the same worker.
                // Once every member and enqueue have succeeded, retained packets
                // must not prevent new submissions from acquiring work-state nodes.
                let retired = self.work_state.lock().take();
                drop(retired);
            }
            self.fence.signal();
        }
    }

    fn finish_enqueue(&self, result: Result) {
        // A dropped, never-enqueued member reports cancellation. The ioctl's actual
        // failure takes precedence, while published members still retain their work.
        if let Err(error) = result {
            self.error.store(error.to_errno(), Ordering::Release);
        }
        self.member_done(result);
    }
}

/// One command's contribution to its submission, including construction failure.
pub(crate) struct Member {
    submission: Arc<Submission>,
    complete: AtomicBool,
}

impl Member {
    pub(crate) fn source(&self) -> Option<Arc<super::sync_source::Source>> {
        self.submission.fence.source().cloned()
    }
    pub(crate) fn is_source(&self, source: &Arc<super::sync_source::Source>) -> bool {
        self.submission.fence.source().is_some_and(|known| Arc::ptr_eq(known, source))
    }
    pub(crate) fn work_state(&self) -> Result<Arc<WorkStateLease>> {
        self.submission
            .work_state
            .lock()
            .as_ref()
            .cloned()
            .ok_or(ECANCELED)
    }

    pub(crate) fn complete(&self, result: Result) {
        if !self.complete.swap(true, Ordering::AcqRel) {
            self.submission.member_done(result);
        }
    }
}

impl Drop for Member {
    fn drop(&mut self) {
        self.complete(Err(ECANCELED));
    }
}

fn install(outputs: impl Iterator<Item = SyncItem>, fence: &Fence) {
    for mut sync in outputs {
        if let Some(chain) = sync.chain_fence.take() {
            sync.syncobj.add_point(chain, fence, sync.timeline_value);
        } else {
            sync.syncobj.replace_fence(Some(fence));
        }
    }
}

/// Keeps resolved output syncobjs reachable on every fallible submission path.
/// Input syncobjs stay in the same vector, avoiding a separate output allocation.
pub(crate) struct Outputs {
    fallback: UserFence<CompletionFence>,
    syncs: KVec<SyncItem>,
    input_count: usize,
    submission: Option<Arc<Submission>>,
    result: Result,
}

impl Outputs {
    pub(crate) fn new(
        contexts: &FenceContexts,
        mut syncs: KVec<SyncItem>,
        input_count: usize,
        allocation_failure: &Fence,
    ) -> Result<Self> {
        if input_count > syncs.len() {
            return Err(EINVAL);
        }
        let fallback = match independent(contexts) {
            Ok(fence) => fence,
            Err(error) => {
                // Syncobjs and timeline-chain storage are already resolved.
                // No work has been accepted; publish the pre-signaled ENOMEM
                // fence without another allocation before returning the error.
                install(syncs.drain(input_count..), allocation_failure);
                return Err(error);
            }
        };
        Ok(Self {
            fallback,
            syncs,
            input_count,
            submission: None,
            result: Err(ECANCELED),
        })
    }

    pub(crate) fn inputs(&self) -> &[SyncItem] {
        &self.syncs[..self.input_count]
    }

    /// Publish the actual aggregate before enqueueing any command.
    pub(crate) fn publish(&mut self, context: Arc<Context>) -> Result<Arc<Submission>> {
        self.publish_fence(context, AggregateFence::Plain(self.fallback.clone()))
    }

    pub(crate) fn publish_source(&mut self, context: Arc<Context>, contexts: &FenceContexts) -> Result<Arc<Submission>> {
        // The fallback is private until publication succeeds. Exported outputs
        // use their distinct aggregate fence; this private fence becomes ready
        // at hardware publication. On failure Outputs still publishes its error.
        let source = super::sync_source::Source::new(self.fallback.clone(), context.clone())?;
        let fence = AggregateFence::Export(exported(contexts, source)?);
        self.publish_fence(context, fence)
    }

    fn publish_fence(&mut self, context: Arc<Context>, fence: AggregateFence) -> Result<Arc<Submission>> {
        let submission = Submission::new(fence, context)?;
        install(self.syncs.drain(self.input_count..), &submission.fence());
        self.submission = Some(submission.clone());
        Ok(submission)
    }

    /// The caller first records terminal VM errors and settles prior failed work.
    pub(crate) fn finish(mut self, result: Result) -> Result {
        self.result = result;
        result
    }
}

impl Drop for Outputs {
    fn drop(&mut self) {
        if let Some(submission) = self.submission.take() {
            submission.finish_enqueue(self.result);
            return;
        }
        if let Err(error) = self.result {
            self.fallback.set_error(error);
        }
        install(
            self.syncs.drain(self.input_count..),
            &Fence::from_fence(&self.fallback),
        );
        self.fallback.signal();
    }
}
