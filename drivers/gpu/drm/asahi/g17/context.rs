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
use core::sync::atomic::{fence, AtomicBool, AtomicU16, AtomicU32, Ordering};
use identity::{RenderSlots, WorkStates, FREE, NODE_STRIDE, STATE_COUNT};

/// No render channel of the context can move to a new pair yet: one of its
/// publications has not retired. Kernel-internal; the submitter waits.
pub(crate) const RENDER_CHANNEL_PENDING: Error = ERECALLCONFLICT;
use kernel::{
    new_mutex,
    prelude::*,
    sync::{Arc, Mutex},
};

/// QoS slots belong to one device; the two lowest values are reserved.
const QOS_SLOTS: usize = 128;
const QOS_MASK: u128 = !3u128;
/// No slot: every encoder rejects it (rows and owners index below 128).
const QOS_NONE: u16 = 0xff;


/// Logical queues are no longer bounded by their QoS slot (bound per
/// publication); this bounds host bookkeeping only.
pub(crate) const MAX_QUEUES: u32 = u32::MAX;

struct QosState {
    used: u128,
    /// Published commands naming each slot that have not retired exactly.
    pins: [u32; QOS_SLOTS],
    /// The owning context's slot cell; the allocator never holds a context.
    owners: [Option<Arc<AtomicU16>>; QOS_SLOTS],
    /// That context's commands present on physical queues ([`QosLease`]).
    activity: [Option<Arc<AtomicU32>>; QOS_SLOTS],
    /// Steal candidates with no pin whose context still had commands on
    /// physical queues. Nonzero means a command ran without its pin.
    unpinned_active: u64,
    steals: u64,
    peak: u32,
}

/// Native AGXQOSManager registers a queue's QoS slot in prepare() and
/// unregisters it in complete() once the queue is idle; a full table makes
/// prepare() wait. Slots here are bound per published command the same way:
/// an idle context keeps its slot until another needs one.
#[pin_data]
pub(crate) struct QosIds {
    available: Arc<super::availability::Availability>,
    #[pin]
    state: Mutex<QosState>,
}
impl QosIds {
    pub(crate) fn new() -> Result<Arc<Self>> {
        Arc::pin_init(
            try_pin_init!(Self {
                available: super::availability::Availability::new()?,
                state <- new_mutex!(QosState {
                    used: 3, pins: [0; QOS_SLOTS], owners: [const { None }; QOS_SLOTS],
                    activity: [const { None }; QOS_SLOTS], unpinned_active: 0,
                    steals: 0, peak: 0,
                }, "G17 QoS slots"),
            }),
            GFP_KERNEL,
        )
    }
    /// A queue's lease; it names no slot until its first publication.
    pub(super) fn acquire(self: &Arc<Self>) -> Result<QosLease> {
        Ok(QosLease {
            ids: self.clone(),
            cell: Arc::new(AtomicU16::new(QOS_NONE), GFP_KERNEL)?,
            activity: Arc::new(AtomicU32::new(0), GFP_KERNEL)?,
        })
    }
    fn pin(&self, cell: &Arc<AtomicU16>, activity: &Arc<AtomicU32>) -> Result<u8> {
        let mut state = self.state.lock();
        let current = cell.load(Ordering::Acquire);
        if current != QOS_NONE {
            let slot = usize::from(current);
            if state.owners[slot].as_ref().is_some_and(|owner| Arc::ptr_eq(owner, cell)) {
                state.pins[slot] = state.pins[slot].checked_add(1).ok_or(EOVERFLOW)?;
                return Ok(slot as u8);
            }
            cell.store(QOS_NONE, Ordering::Release);
        }
        let free = !state.used & QOS_MASK;
        let slot = if free != 0 {
            free.trailing_zeros() as usize
        } else {
            // Steal the lowest slot whose context has no unretired command:
            // no pin and no command on any physical queue.
            let mut unpinned_active = None;
            let candidate = (2..QOS_SLOTS).find(|&slot| {
                if state.pins[slot] != 0 || state.owners[slot].is_none() {
                    return false;
                }
                let active = state.activity[slot].as_ref()
                    .map_or(0, |activity| activity.load(Ordering::Acquire));
                if active != 0 {
                    unpinned_active.get_or_insert((slot, active));
                }
                active == 0
            });
            if let Some((slot, active)) = unpinned_active {
                state.unpinned_active += 1;
                let n = state.unpinned_active;
                if n <= 8 || n.is_power_of_two() {
                    pr_warn!("asahi: QoS slot {} has no pin but {} command(s) on physical queues; not taken (occurrence {})\n",
                        slot, active, n);
                }
            }
            let Some(slot) = candidate else { return Err(EBUSY); };
            if let Some(victim) = state.owners[slot].take() {
                victim.store(QOS_NONE, Ordering::Release);
            }
            drop(state.activity[slot].take());
            state.steals += 1;
            let steals = state.steals;
            if steals <= 8 || steals.is_power_of_two() {
                pr_info!("asahi: QoS slot {} taken from an idle queue (steal {})\n", slot, steals);
            }
            slot
        };
        let bit = 1u128 << slot;
        state.used |= bit;
        state.pins[slot] = 1;
        state.owners[slot] = Some(cell.clone());
        state.activity[slot] = Some(activity.clone());
        cell.store(slot as u16, Ordering::Release);
        let in_use = state.used.count_ones() - 2;
        if in_use > state.peak {
            state.peak = in_use;
            if in_use % 16 == 0 {
                pr_info!("asahi: QoS slots in use: peak {}\n", in_use);
            }
        }
        Ok(slot as u8)
    }
    fn unpin(&self, cell: &Arc<AtomicU16>, slot: u8) {
        let mut state = self.state.lock();
        let index = usize::from(slot);
        if index < QOS_SLOTS && state.owners[index].as_ref().is_some_and(|owner| Arc::ptr_eq(owner, cell))
            && state.pins[index] != 0 {
            state.pins[index] -= 1;
            let available = state.pins[index] == 0;
            drop(state);
            if available { self.available.notify(); }
        }
    }
    fn release(&self, cell: &Arc<AtomicU16>) {
        let mut state = self.state.lock();
        let current = cell.load(Ordering::Acquire);
        if current == QOS_NONE { return; }
        let slot = usize::from(current);
        if state.owners[slot].as_ref().is_some_and(|owner| Arc::ptr_eq(owner, cell)) {
            let active = state.activity[slot].as_ref()
                .map_or(0, |activity| activity.load(Ordering::Acquire));
            if state.pins[slot] != 0 || active != 0 {
                pr_warn!("asahi: retaining closed QoS slot {}: pins={} active={}\n",
                    slot, state.pins[slot], active);
                return;
            }
            let owner = state.owners[slot].take();
            let activity = state.activity[slot].take();
            state.used &= !(1u128 << slot);
            state.pins[slot] = 0;
            cell.store(QOS_NONE, Ordering::Release);
            drop(state);
            drop(owner);
            drop(activity);
            self.available.notify();
        }
    }

}
/// Logical contexts alive, including closed ones still in teardown (their
/// scheduler release and installed-graph detach), for capacity diagnostics.
static LIVE_CONTEXTS: AtomicU32 = AtomicU32::new(0);
static LIVE_CONTEXTS_PEAK: AtomicU32 = AtomicU32::new(0);

fn note_live_context(added: i32) {
    if added < 0 {
        LIVE_CONTEXTS.fetch_sub(1, Ordering::AcqRel);
        return;
    }
    let live = LIVE_CONTEXTS.fetch_add(1, Ordering::AcqRel) + 1;
    let peak = LIVE_CONTEXTS_PEAK.fetch_max(live, Ordering::AcqRel);
    if live > peak && live % 32 == 0 {
        pr_info!("asahi: logical contexts alive: peak {}\n", live);
    }
}

pub(super) struct QosLease {
    ids: Arc<QosIds>,
    cell: Arc<AtomicU16>,
    /// This context's commands present on physical queues: its slot is never
    /// taken while nonzero, pinned or not.
    activity: Arc<AtomicU32>,
}
impl QosLease {
    /// The slot published commands name now; QOS_NONE (rejected by every
    /// encoder) while unbound. Stable while a command is pinned.
    pub(super) fn id(&self) -> u8 {
        self.cell.load(Ordering::Acquire) as u8
    }
}
impl Drop for QosLease {
    fn drop(&mut self) {
        self.ids.release(&self.cell);
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
        note_live_context(1);
        Ok(Self {
            execution,
            status,
            scheduler,
            scheduler_generation,
            published: AtomicBool::new(false),
            qos,
            owner_pid,
            states,
            work,
            work_base,
            render,
            policy,
        })
    }
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
    pub(crate) fn pin(&self) -> Result<mmu::ContextPin> {
        self.execution.pin()
    }
    pub(crate) fn unpin(&self, pin: mmu::ContextPin) {
        self.execution.unpin(pin)
    }
    pub(crate) fn is_sptm(&self) -> bool {
        self.execution.is_sptm()
    }
    pub(crate) fn execution_availability(&self) -> Option<&Arc<super::availability::Availability>> {
        self.execution.availability()
    }
    pub(crate) fn qos_availability(&self) -> &Arc<super::availability::Availability> {
        &self.qos.ids.available
    }
    pub(crate) fn work_state_availability(&self) -> &Arc<super::availability::Availability> {
        &self.states.available
    }
    pub(crate) fn owner_pid(&self) -> u32 {
        self.owner_pid
    }
    pub(crate) fn policy(&self) -> Policy {
        self.policy
    }
    pub(crate) fn qos_id(&self) -> u8 {
        self.qos.id()
    }
    /// Claim the queue's QoS slot for one published command (see [`QosIds`]).
    pub(crate) fn pin_qos(&self) -> Result<u8> {
        self.qos.ids.pin(&self.qos.cell, &self.qos.activity)
    }
    /// A command of this context entered (`true`) or left a physical queue.
    /// Neither its hardware identity nor its QoS slot moves while it is there.
    pub(crate) fn note_activity(&self, entered: bool) {
        if entered {
            self.qos.activity.fetch_add(1, Ordering::AcqRel);
        } else {
            if self.qos.activity.fetch_sub(1, Ordering::AcqRel) == 1 {
                self.qos.ids.available.notify();
            }
        }
        self.execution.note_activity(entered);
    }
    pub(crate) fn unpin_qos(&self, slot: u8) {
        self.qos.ids.unpin(&self.qos.cell, slot)
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
    /// vmfix6d: `pool` is this context's current render pool (so dropping
    /// another reference to it cannot free it).
    pub(crate) fn render_pool_is(&self, pool: &Arc<RenderPool>) -> bool {
        self.render.lock().as_ref().is_some_and(|current| Arc::ptr_eq(current, pool))
    }
    pub(crate) fn render_pools_released(&self, released: impl Fn(u32, u64) -> bool) -> bool {
        self.render
            .lock()
            .as_ref()
            .is_none_or(|pool| released(u32::from(pool.id()), pool.control_va()))
    }
}

/// Shared by all engines of one ioctl. Queued work initially consumes no node.
struct WorkClaims {
    preparing: u32,
    retained: bool,
}

#[pin_data(PinnedDrop)]
pub(crate) struct WorkStateLease {
    context: Arc<Context>,
    word: AtomicU32,
    #[pin]
    claims: Mutex<WorkClaims>,
}
impl WorkStateLease {
    pub(crate) fn deferred(context: Arc<Context>) -> impl PinInit<Self, Error> {
        try_pin_init!(Self {
            context,
            word: AtomicU32::new(FREE),
            claims <- new_mutex!(WorkClaims { preparing: 0, retained: false }, "G17 work-state claims"),
        })
    }

    /// Existing publication/replay callers retain the aggregate node. A retry
    /// must never return a sibling's node while that sibling can still use it.
    pub(crate) fn acquire(&self) -> Result<u32> {
        let (result, unused) = {
            let mut claims = self.claims.lock();
            let result = self.acquire_inner();
            if result.0.is_ok() { claims.retained = true; }
            result
        };
        if let Some(word) = unused { self.context.states.release(word); }
        result
    }

    /// A preparation owns one reversible claim. Only the last failed claim
    /// may return a node, and only if no member has committed it for execution.
    pub(crate) fn claim(self: &Arc<Self>) -> Result<WorkStateClaim> {
        let (result, unused) = {
            let mut claims = self.claims.lock();
            let next = claims.preparing.checked_add(1).ok_or(EOVERFLOW)?;
            let result = self.acquire_inner();
            if result.0.is_ok() { claims.preparing = next; }
            result
        };
        if let Some(word) = unused { self.context.states.release(word); }
        result.map(|_| WorkStateClaim { lease: self.clone() })
    }

    /// Return an unused allocation separately so its availability notification
    /// always runs after the aggregate claim mutex has been dropped.
    fn acquire_inner(&self) -> (Result<u32>, Option<u32>) {
        let current = self.word();
        if current != FREE { return (Ok(current), None); }
        let Some(candidate) = self.context.states.allocate() else {
            return (Err(EBUSY), None);
        };
        if let Err(error) = self.context.work.lock().initialize(candidate) {
            return (Err(error), Some(candidate));
        }
        match self.word.compare_exchange(FREE, candidate, Ordering::AcqRel, Ordering::Acquire) {
            Ok(_) => (Ok(candidate), None),
            Err(installed) => (Ok(installed), Some(candidate)),
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
        self.context.work.lock().render.get(pair).ok_or_else(|| enospc_site!("context.rs:450 render work slot"))
    }
    /// The render channel of `pair` for a publication. When both channels
    /// are bound to other pairs, the least recently used one whose every
    /// publication `retired(qids, ordinal)` moves to `pair` (vmfix6b);
    /// otherwise RENDER_CHANNEL_PENDING: the caller waits for retirement
    /// instead of failing the client.
    pub(crate) fn render_channel(
        &self,
        pair: u8,
        retired: impl Fn([u8; 2], u64) -> bool,
    ) -> Result<u32> {
        let (index, previous) = self
            .context
            .work
            .lock()
            .render
            .bind(pair, retired)
            .ok_or(RENDER_CHANNEL_PENDING)?;
        if let Some(previous) = previous {
            static REBINDS: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);
            let n = REBINDS.fetch_add(1, Ordering::Relaxed) + 1;
            if n <= 8 || n.is_power_of_two() {
                pr_info!("asahi: render channel {} of context {} moved from pair {} to pair {} (rebind {})\n",
                    index, self.context.id(), previous, pair, n);
            }
        }
        Ok(index)
    }
    /// Record a publication through `pair`'s channel once it reached firmware.
    pub(crate) fn channel_published(&self, pair: u8, qids: [u8; 2], ordinal: u64) {
        self.context.work.lock().render.published(pair, qids, ordinal);
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
pub(crate) struct WorkStateClaim {
    lease: Arc<WorkStateLease>,
}
impl WorkStateClaim {
    pub(crate) fn commit(&self) { self.lease.claims.lock().retained = true; }
}
impl Drop for WorkStateClaim {
    fn drop(&mut self) {
        let word = {
            let mut claims = self.lease.claims.lock();
            claims.preparing -= 1;
            if claims.preparing == 0 && !claims.retained {
                self.lease.word.swap(FREE, Ordering::AcqRel)
            } else {
                FREE
            }
        };
        if word != FREE { self.lease.context.states.release(word); }
    }
}

#[pinned_drop]
impl PinnedDrop for WorkStateLease {
    fn drop(self: core::pin::Pin<&mut Self>) {
        let word = self.word();
        if word != FREE {
            self.context.states.release(word);
        }
    }
}

impl Drop for Context {
    fn drop(&mut self) {
        note_live_context(-1);
    }
}

/// A command's presence on a physical queue (see [`Context::note_activity`]):
/// begun when the queue takes the command at publication, ended when exact
/// retirement removes it (before its pins are returned) or when the queue
/// drops it on any other path.
pub(crate) struct ActivityMark(Option<Arc<Context>>);
impl ActivityMark {
    pub(crate) fn begin(context: &Arc<Context>) -> Self {
        context.note_activity(true);
        Self(Some(context.clone()))
    }
    pub(crate) fn end(&mut self) {
        if let Some(context) = self.0.take() {
            context.note_activity(false);
        }
    }
}
impl Drop for ActivityMark {
    fn drop(&mut self) {
        self.end();
    }
}
