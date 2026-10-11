// SPDX-License-Identifier: GPL-2.0-only AND MIT

//! Logical queue ownership of scheduler, QoS and work-state identities.
//!
//! Installed bindings retain this context until replacement is acknowledged.
//! A job keeps its work-state lease through exact retirement, even if its error
//! fence has already been signalled. Context-local accounting never waits for GPU work.

pub(crate) mod identity;
use super::{
    freelist::{RenderIds, RenderPool},
    fw::{context as fw, queue::Policy},
    object::{Allocator, CpuMap, Family, KernelObject, Pool, PooledObject},
    status::VmStatus,
};
use crate::mmu::{self, ExecutionContext, Vm, UAT_PGSZ};
use core::sync::atomic::{fence, AtomicBool, AtomicU32, Ordering};
use identity::{RenderSlots, WorkStates, FREE, NODE_STRIDE, STATE_COUNT};
use kernel::{
    new_mutex, new_condvar,
    prelude::*,
    sync::{Arc, CondVar, CondVarTimeoutResult, Mutex},
};

/// QoS IDs are retained until firmware can no longer name the old context.
struct QosState {
    used: u128,
    closing: u128,
}
#[pin_data]
pub(crate) struct QosIds {
    #[pin]
    state: Mutex<QosState>,
    #[pin]
    released: CondVar,
}
impl QosIds {
    pub(super) const CAPACITY: u32 = u128::BITS - 2;

    pub(crate) fn new() -> Result<Arc<Self>> {
        Arc::pin_init(pin_init!(Self {
            state <- new_mutex!(QosState { used: 3, closing: 0 }, "G17 QoS IDs"),
            released <- new_condvar!("G17 QoS retirement"),
        }), GFP_KERNEL)
    }
    pub(super) fn acquire(self: &Arc<Self>) -> Result<QosLease> {
        let mut state = self.state.lock();
        let id = state.used.trailing_ones();
        if id == 128 {
            // Only a closing owner can make capacity available without the
            // new application first destroying an active logical queue.
            return Err(if state.closing != 0 { EAGAIN } else { ENOSPC });
        }
        state.used |= 1u128 << id;
        Ok(QosLease { ids: self.clone(), id: id as u8 })
    }
    /// Called without a preparation lease or the device mutex. A firmware
    /// retirement, not elapsed time, is the event which makes an ID reusable.
    pub(super) fn wait_available(&self, millis: u32) -> Result {
        let mut state = self.state.lock();
        let mut budget = kernel::time::msecs_to_jiffies(millis);
        while state.used == u128::MAX {
            if state.closing == 0 || budget == 0 { return Err(ENOSPC); }
            budget = match self.released.wait_interruptible_timeout(&mut state, budget) {
                CondVarTimeoutResult::Signal { .. } => return Err(ERESTARTSYS),
                CondVarTimeoutResult::Timeout => 0,
                CondVarTimeoutResult::Woken { jiffies } => jiffies,
            };
        }
        Ok(())
    }
}
pub(super) struct QosLease {
    ids: Arc<QosIds>,
    id: u8,
}
impl QosLease {
    pub(super) fn id(&self) -> u8 { self.id }
    fn mark_closing(&self) { self.ids.state.lock().closing |= 1u128 << self.id; }
}
impl Drop for QosLease {
    fn drop(&mut self) {
        let mut state = self.ids.state.lock();
        state.used &= !(1u128 << self.id);
        state.closing &= !(1u128 << self.id);
        drop(state);
        self.ids.released.notify_all();
    }
}

struct WorkStorage {
    object: PooledObject,
    words: [Option<u32>; STATE_COUNT],
    counts: [u32; STATE_COUNT],
    render: RenderSlots,
}
impl WorkStorage {
    fn initialize(&mut self, word: u32) -> Result {
        let slot = (word & 63) as usize;
        if slot >= STATE_COUNT {
            return Err(EINVAL);
        }
        let counter = UAT_PGSZ + slot * 4;
        let node = fw::WorkNode::new(self.object.gpu_va() + counter as u64, word).ok_or(EINVAL)?;
        // The exact work-state lease excludes a previous firmware owner. Only
        // the node's 0xc8 initialized bytes are replaced; the stride tail is retained.
        self.object.write(slot * NODE_STRIDE, node)?;
        self.object.word(counter)?.store(0, Ordering::Relaxed);
        self.words[slot] = Some(word);
        self.counts[slot] = 0;
        fence(Ordering::Release);
        Ok(())
    }
    fn change_count(&mut self, word: u32, kicks: u32, add: bool) -> Result<u32> {
        let slot = (word & 63) as usize;
        if self.words.get(slot) != Some(&Some(word)) {
            return Err(EINVAL);
        }
        let value = if add {
            self.counts[slot].checked_add(kicks)
        } else {
            self.counts[slot].checked_sub(kicks)
        }
        .ok_or(EOVERFLOW)?;
        self.object
            .word(UAT_PGSZ + slot * 4)?
            .store(value, Ordering::Relaxed);
        self.counts[slot] = value;
        fence(Ordering::SeqCst);
        Ok(value)
    }
}

/// Cached scheduler mappings use the existing firmware-stop retention pool.
/// The ordinary variant preserves the non-M5 allocation path.
pub(crate) enum SchedulerStorage {
    Ordinary(KernelObject),
    Retained(PooledObject),
}
impl core::ops::Deref for SchedulerStorage {
    type Target = KernelObject;
    fn deref(&self) -> &KernelObject {
        match self { Self::Ordinary(object) => object, Self::Retained(object) => object }
    }
}
impl core::ops::DerefMut for SchedulerStorage {
    fn deref_mut(&mut self) -> &mut KernelObject {
        match self { Self::Ordinary(object) => object, Self::Retained(object) => object }
    }
}

/// Firmware-visible pages of a context, retained by an installed queue whose
/// last owner has exited (see [`Context::retain_firmware_pages`]).
pub(crate) struct FirmwarePages {
    _scheduler: Arc<SchedulerStorage>,
    _work: Arc<Mutex<WorkStorage>>,
}

pub(crate) struct Context {
    execution: Arc<ExecutionContext>,
    status: Arc<VmStatus>,
    scheduler: Arc<SchedulerStorage>,
    scheduler_generation: u32,
    published: AtomicBool,
    preparations: AtomicU32,
    qos: QosLease,
    owner_pid: u32,
    states: Arc<WorkStates>,
    work: Arc<Mutex<WorkStorage>>,
    work_base: u64,
    render: Arc<Mutex<Option<Arc<RenderPool>>>>,
    /// Fixed for the context's life. Physical queues bound to it carry this
    /// profile in every record, ring, doorbell and registration.
    policy: Policy,
}
impl Context {
    pub(crate) fn new(
        alloc: &Allocator<'_>,
        pool: &Arc<Pool>,
        qos: &Arc<QosIds>,
        execution: Arc<ExecutionContext>,
        owner_pid: u32,
        policy: Policy,
    ) -> Result<Self> {
        let status = execution.vm().status().ok_or(EINVAL)?.clone();
        let qos = qos.acquire()?;
        let mut scheduler = if alloc.uat.is_sptm() {
            SchedulerStorage::Retained(pool.allocate(alloc, Family::Scheduler,
                UAT_PGSZ, UAT_PGSZ, mmu::PROT_GPU_FW_SHARED_RW, CpuMap::WriteCombined)?)
        } else {
            SchedulerStorage::Ordinary(alloc.kernel(UAT_PGSZ, UAT_PGSZ as u64,
                mmu::PROT_GPU_FW_SHARED_RW, CpuMap::WriteCombined)?)
        };
        scheduler.initialize::<fw::Scheduler>(0, |value| {
            value.init();
            Ok(())
        })?;
        fence(Ordering::SeqCst);
        let scheduler_generation = super::ids::OBJECTS.object()?;
        let scheduler = Arc::new(scheduler, GFP_KERNEL)?;
        let mut object = pool.allocate(
            alloc,
            Family::WorkState,
            2 * UAT_PGSZ,
            UAT_PGSZ,
            mmu::PROT_GPU_FW_SHARED_RW,
            CpuMap::WriteCombined,
        )?;
        let work_base = object.gpu_va();
        object.write(
            UAT_PGSZ + 0x400,
            fw::WorkHead {
                next: 0,
                previous: work_base + (UAT_PGSZ + 0x400) as u64,
                unk10: 0,
                unk14: 0,
            },
        )?;
        fence(Ordering::Release);
        let work = Arc::pin_init(
            new_mutex!(
                WorkStorage {
                    object,
                    words: [None; STATE_COUNT],
                    counts: [0; STATE_COUNT],
                    render: RenderSlots::new()
                },
                "G17 work states"
            ),
            GFP_KERNEL,
        )?;
        let render = Arc::pin_init(new_mutex!(None, "G17 context private memory"), GFP_KERNEL)?;
        let states = execution.work_states()?;
        Ok(Self {
            execution,
            status,
            scheduler,
            scheduler_generation,
            published: AtomicBool::new(false),
            preparations: AtomicU32::new(0),
            qos,
            owner_pid,
            states,
            work,
            work_base,
            render,
            policy,
        })
    }
    pub(crate) fn begin_preparation(&self) {
        self.preparations.fetch_add(1, Ordering::AcqRel);
    }
    pub(crate) fn end_preparation(&self) {
        let old = self.preparations.fetch_sub(1, Ordering::AcqRel);
        debug_assert!(old != 0);
    }
    pub(crate) fn has_preparations(&self) -> bool {
        self.preparations.load(Ordering::Acquire) != 0
    }

    pub(crate) fn mark_closing(&self) { self.qos.mark_closing(); }

    pub(crate) fn vm(&self) -> &Vm {
        self.execution.vm()
    }
    /// Place one shared metrics view in currently free compact VA. The VM
    /// retains it for reuse by every context and protects it from user binds.
    pub(crate) fn metrics_client(&self, backing: &Arc<KernelObject>) -> Result<u64> {
        self.vm().metrics_alias(backing.gpu_va(), || {
            backing.alias_in(self.vm(), super::buffer::BlockGeometry::M5_COMPACT,
                UAT_PGSZ as u64, mmu::PROT_GPU_SHARED_RW)
        })
    }
    pub(crate) fn status(&self) -> &Arc<VmStatus> {
        &self.status
    }
    pub(crate) fn id(&self) -> u32 {
        self.execution.id()
    }

    pub(crate) fn generation(&self) -> u8 {
        self.execution.generation()
    }
    pub(crate) fn render_generation(&self) -> u64 {
        self.execution.native_generation()
    }
    pub(crate) fn is_current(&self) -> bool {
        self.execution.is_current()
    }
    pub(crate) fn is_sptm(&self) -> bool {
        self.execution.is_sptm()
    }
    pub(crate) fn owner_pid(&self) -> u32 {
        self.owner_pid
    }
    pub(crate) fn policy(&self) -> Policy {
        self.policy
    }
    pub(crate) fn qos_id(&self) -> u8 {
        self.qos.id
    }
    pub(crate) fn scheduler_va(&self) -> u64 {
        self.scheduler.gpu_va()
    }
    /// Installed physical channels retain this page after the logical execution
    /// context and its VM lease have completed their scheduler release.
    pub(crate) fn scheduler_owner(&self) -> Arc<SchedulerStorage> {
        self.scheduler.clone()
    }
    /// Scheduler release and command retirement have both been witnessed.
    /// Keep installed storage alive while returning the finite TTBAT identity.
    pub(crate) fn release_execution(&self) {
        self.execution.release();
    }
    pub(crate) fn scheduler_generation(&self) -> u32 {
        self.scheduler_generation
    }
    /// The pages an installed queue record of this context still names: the
    /// scheduler page and the work storage holding the job-list head. A retained
    /// physical queue keeps these after dropping the context itself.
    pub(crate) fn retain_firmware_pages(&self) -> FirmwarePages {
        FirmwarePages {
            _scheduler: self.scheduler.clone(),
            _work: self.work.clone(),
        }
    }
    pub(crate) fn work_head_va(&self) -> u64 {
        self.work_base + (UAT_PGSZ + 0x400) as u64
    }
    /// Shared scheduler list head, sampled under the context's storage lock.
    pub(crate) fn work_head(&self) -> Result<[u64; 2]> {
        let work = self.work.lock();
        let head = [
            work.object.dword(UAT_PGSZ + 0x400)?.load(Ordering::Relaxed),
            work.object.dword(UAT_PGSZ + 0x408)?.load(Ordering::Relaxed),
        ];
        fence(Ordering::Acquire);
        Ok(head)
    }

    pub(crate) fn is_published(&self) -> bool {
        self.published.load(Ordering::Acquire)
    }
    pub(crate) fn mark_published(&self) {
        self.published.store(true, Ordering::Release);
    }

    pub(crate) fn release_identity(&self) -> Result<[u8; 4]> {
        let mut result = [0; 4];
        fence(Ordering::Acquire);
        for (value, offset) in result.iter_mut().zip([0, 1, 4, 0x26]) {
            let pointer = self.scheduler.pointer(offset, 1)?;
            // SAFETY: pointer checked against retained scheduler allocation;
            // a byte read observes firmware state without borrowing its fields.
            *value = unsafe { core::ptr::read_volatile(pointer) };
        }
        Ok(result)
    }
    pub(crate) fn render_pool(
        &self,
        alloc: &Allocator<'_>,
        ids: &Arc<RenderIds>,
        render_global: &Vm,
        plan: Option<super::render_resources::Plan>,
    ) -> Result<Arc<RenderPool>> {
        if !self.is_current() {
            return Err(EFAULT);
        }
        if alloc.uat.is_sptm() != plan.is_some() { return Err(EINVAL); }
        let mut render = self.render.lock();
        if let Some(pool) = &*render {
            if pool.fits(plan.as_ref().map(|p| p.geometry)) {
                // A pool vacated under memory pressure is re-armed before reuse.
                pool.ensure_backing(alloc, render_global, self.vm())?;
                return Ok(pool.clone());
            }
            // Existing pairs retain the old pool through replacement and its
            // release acknowledgement. Allocation failure leaves it installed.
        }
        let pool = Arc::new(
            match plan {
                Some(plan) => RenderPool::m5(alloc, super::freelist::RenderId::new(ids)?,
                    self.vm(), plan.geometry)?,
                None => RenderPool::new(alloc, ids, render_global, self.vm())?,
            },
            GFP_KERNEL,
        )?;
        let previous = render.replace(pool.clone());
        drop(render);
        drop(previous);
        Ok(pool)
    }
    /// Drops the pool once its free-list release is consumed and the closed
    /// scheduler is detached. Installed compute bindings may retain the
    /// context itself until replacement; they never use the render pool.
    pub(crate) fn take_render_pool(&self) -> Option<Arc<RenderPool>> {
        self.render.lock().take()
    }
    pub(crate) fn render_pools_released(&self, released: impl Fn(u32, u64) -> bool) -> bool {
        self.render
            .lock()
            .as_ref()
            .is_none_or(|pool| released(u32::from(pool.id()), pool.control_va()))
    }
}

/// Shared by all engines of one ioctl. Queued work initially consumes no node.
pub(crate) struct WorkStateLease {
    context: Arc<Context>,
    word: AtomicU32,
}
impl WorkStateLease {
    pub(crate) fn deferred(context: Arc<Context>) -> Self {
        Self {
            context,
            word: AtomicU32::new(FREE),
        }
    }
    pub(crate) fn acquire(&self) -> Result<u32> {
        let current = self.word();
        if current != FREE {
            return Ok(current);
        }
        let Some(candidate) = self.context.states.allocate() else {
            let current = self.word();
            return if current != FREE {
                Ok(current)
            } else {
                Err(EBUSY)
            };
        };
        if let Err(error) = self.context.work.lock().initialize(candidate) {
            self.context.states.release(candidate);
            return Err(error);
        }
        match self
            .word
            .compare_exchange(FREE, candidate, Ordering::AcqRel, Ordering::Acquire)
        {
            Ok(_) => Ok(candidate),
            Err(installed) => {
                self.context.states.release(candidate);
                Ok(installed)
            }
        }
    }
    pub(crate) fn word(&self) -> u32 {
        self.word.load(Ordering::Acquire)
    }
    pub(crate) fn node_va(&self) -> Result<u64> {
        let word = self.word();
        if word == FREE {
            return Err(EINVAL);
        }
        Ok(self.context.work_base + u64::from(word & 63) * NODE_STRIDE as u64)
    }
    /// Read only this lease's completed counter, after publication. This is
    /// supplementary progress information, not a command-retirement witness.
    pub(crate) fn completed_kicks(&self) -> Result<u32> {
        self.with_node(|object, offset| {
            Ok(object.word(offset + 0xc)?.load(Ordering::Acquire))
        })
    }
    /// Inspect a live node while retaining its storage and exact lease identity.
    /// The callback cannot replace the node or release its backing.
    pub(crate) fn with_node<T>(&self,
        read: impl FnOnce(&KernelObject, usize) -> Result<T>) -> Result<T> {
        let word = self.word();
        let slot = (word & 63) as usize;
        let storage = self.context.work.lock();
        if word == FREE || storage.words.get(slot) != Some(&Some(word)) {
            return Err(EINVAL);
        }
        read(&storage.object, slot * NODE_STRIDE)
    }
    pub(crate) fn add_submitted_kicks(&self, kicks: u32) -> Result<u32> {
        self.context
            .work
            .lock()
            .change_count(self.word(), kicks, true)
    }
    pub(crate) fn cancel_unpublished_kicks(&self, kicks: u32) -> Result<u32> {
        self.context
            .work
            .lock()
            .change_count(self.word(), kicks, false)
    }

    pub(crate) fn render_slot(&self, pair: u8) -> Result<u32> {
        self.context.work.lock().render.get(pair).ok_or(ENOSPC)
    }
    pub(crate) fn channel_payload(&self, pair: u8, ordinal: u64) -> Result<u8> {
        self.context
            .work
            .lock()
            .render
            .payload(pair, ordinal)
            .ok_or(EINVAL)
    }
}
impl Drop for WorkStateLease {
    fn drop(&mut self) {
        let word = self.word();
        if word != FREE {
            self.context.states.release(word);
        }
    }
}
