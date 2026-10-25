// SPDX-License-Identifier: GPL-2.0-only OR MIT

//! Retained physical compute queues. Logical ownership and command retirement never free an
//! installed graph; the device retains it until both firmware processors have stopped.

pub(crate) mod exit;
mod activation;
mod configuration;
pub(crate) mod retirement;

use crate::g17::{
    channel::Rings,
    context::Context,
    fw::{channels::ControlRecord, compute::COMPUTE_DESCRIPTOR_SIZE, queue::*},
    object::{Allocator, CpuMap, Family, KernelObject, Pool, PooledObject},
};
use crate::{hw::t8140::dynamic, mmu};
use core::sync::atomic::{fence, AtomicU32, AtomicUsize, Ordering};
use kernel::{prelude::*, sync::Arc};
use retirement::{Cursors, RECORD_SLOTS};

use crate::g17::now_ns;

/// Descriptor slots per queue. A kick's descriptor is rewritten when the kick 32 slots later
/// is published; with IN_FLIGHT kicks outstanding at most, that kick has completed by then.
const DESCRIPTORS: usize = 32;
const GRAPH_SIZE: usize = 0x8000;
const CONTEXT_SIZE: usize = 0x20000;
const RECORD_STRIDE: usize = 0x1000;
const CONFIG: usize = 0;
const ANNOUNCE: usize = 0x800;
const PREDECESSOR: usize = 0x400;
const POINTERS: usize = 0x100;
const ITEMS: usize = 0x4000;
const STAMP: usize = 0x300;

#[repr(C)]
struct GraphLayout {
    queue: [u8; 0x100],
    pointers: PointerBlock,
    unk_180: [u8; 0x80],
    jobs: JobListHead,
    unk_210: [u8; 0x3df0],
    items: [u64; ITEM_RING_ENTRIES as usize],
    unk_6800: [u8; 0x1800],
}
// SAFETY: The layout contains only integer storage and integer atomics, all valid when zeroed.
unsafe impl Zeroable for GraphLayout {}
static_assert!(core::mem::size_of::<GraphLayout>() == GRAPH_SIZE);
static_assert!(core::mem::offset_of!(GraphLayout, pointers) == POINTERS);
static_assert!(core::mem::offset_of!(GraphLayout, items) == ITEMS);

/// Initialize common queue storage and encode only the profile-specific header.
pub(in crate::g17) fn initialize_graph(graph: &mut KernelObject, context: &Context, m5: bool) -> Result {
    initialize_graph_at(graph, context, m5, POINTERS)
}

fn initialize_graph_at(graph: &mut KernelObject, context: &Context, m5: bool, pointers: usize) -> Result {
    let address = graph.gpu_va();
    graph.initialize::<GraphLayout>(0, |view| {
        view.pointers = PointerBlock::new();
        view.jobs = JobListHead::new(address + 0x200);
        Ok(())
    })?;
    if pointers != POINTERS {
        graph.initialize::<PointerBlock>(pointers, |view| {
            *view = PointerBlock::new();
            Ok(())
        })?;
    }
    let args = QueueRecordArgs {
        pointers_va: address + pointers as u64,
        items_va: address + ITEMS as u64,
        job_list_va: context.work_head_va(),
        scheduler_va: context.scheduler_va(),
        owner_pid: context.owner_pid(), policy: context.policy(),
    };
    if m5 {
        graph.write(0, crate::g17::fw::m5::queue::QueueRecord::new(&args)?)
    } else {
        graph.write(0, QueueRecord::new(&args))
    }
}

/// Aliases are destroyed before the logical root they name. Superseded bindings remain owned
/// until the first exact successful publication under their replacement has retired.
/// Backing of a vacated pool and descriptor ring, freed outside the device
/// mutex. Field order drops each client alias before the storage it names.
pub(crate) struct Vacated {
    _alias: Option<mmu::KernelMapping>,
    _pages: Option<crate::g17::freelist::Pages>,
    _descriptors: Option<mmu::KernelMapping>,
    _ring: Option<DescriptorRing>,
}

pub(crate) struct Binding {
    _kick_alias: mmu::KernelMapping,
    /// Client alias of the USC pool pages; dropped when the pool is vacated and
    /// recreated when an owner binds a re-armed pool (it pins the backing).
    pool_alias: Option<mmu::KernelMapping>,
    /// Client alias of the descriptor ring; dropped with the ring when the queue
    /// is vacated and recreated when an owner binds a re-armed queue.
    descriptors: Option<mmu::KernelMapping>,
    preempt: mmu::KernelMapping,
    usage: mmu::KernelMapping,
    operand: mmu::KernelMapping,
    _queue_context: Option<mmu::KernelMapping>,
    context: Arc<Context>,
}

/// Descriptor ring of one compute QID with its kernel-VM alias. Built without
/// the device mutex; installed into a vacant graph before an owner binds.
enum DescriptorStorage {
    Ordinary(KernelObject),
    Retained(PooledObject),
}
impl core::ops::Deref for DescriptorStorage {
    type Target = KernelObject;
    fn deref(&self) -> &KernelObject {
        match self { Self::Ordinary(object) => object, Self::Retained(object) => object }
    }
}
impl core::ops::DerefMut for DescriptorStorage {
    fn deref_mut(&mut self) -> &mut KernelObject {
        match self { Self::Ordinary(object) => object, Self::Retained(object) => object }
    }
}

pub(crate) struct DescriptorRing {
    // Drop the alias before returning retained firmware backing to its pool.
    alias: mmu::KernelMapping,
    object: DescriptorStorage,
    upper_primary: bool,
}

impl DescriptorRing {
    pub(crate) fn new(alloc: &Allocator<'_>, objects: &Arc<Pool>, m5: bool) -> Result<Self> {
        let page = mmu::UAT_PGSZ as u64;
        let prot = mmu::PROT_GPU_FW_SHARED_RW;
        let size = DESCRIPTORS * COMPUTE_DESCRIPTOR_SIZE;
        if m5 && *crate::module_parameters::m5_cached_objects.value() == 1 {
            // Native compute descriptors use cached firmware leaves. The pool
            // retains that upper mapping through processor stop; lower GPU
            // aliases keep the existing shared attributes and retirement rules.
            let object = objects.allocate(alloc, Family::Descriptor, size, size,
                prot, CpuMap::WriteCombined)?;
            let alias = object.alias_in(alloc.uat.kernel_lower_vm(),
                alloc.uat.kernel_lower_dynamic_range(), page, mmu::PROT_GPU_SHARED_RW)?;
            return Ok(Self { alias, object: DescriptorStorage::Retained(object), upper_primary: true });
        }
        let object = alloc.lower(size, page,
            if m5 { mmu::PROT_GPU_SHARED_RW } else { prot }, CpuMap::WriteCombined)?;
        let alias = object.alias_in(alloc.uat.kernel_vm(),
            alloc.uat.kernel_dynamic_range(), page, prot)?;
        Ok(Self { alias, object: DescriptorStorage::Ordinary(object), upper_primary: false })
    }

    fn firmware_va(&self) -> u64 {
        if self.upper_primary { self.object.gpu_va() } else { self.alias.iova() }
    }
    fn gpu_va(&self) -> u64 {
        if self.upper_primary { self.alias.iova() } else { self.object.gpu_va() }
    }
}

/// Firmware-shared storage of one compute QID, separate from its common kick ring.
/// `ring` is `None` while the queue is vacant (idle, no owner, pool given back).
struct Graph {
    m5: bool,
    ring: Option<DescriptorRing>,
    preempt: KernelObject,
    usage: KernelObject,
    operand: KernelObject,
    support: KernelObject,
    m5_timestamps: Option<Arc<KernelObject>>,
    graph: DescriptorStorage,
    pointers: usize,
    context_low: Option<KernelObject>,
    context_high: Option<KernelObject>,
    records: KernelObject,
    record_ends: [Option<u32>; RECORD_SLOTS],
    reclaimed: u32,
}

impl Graph {
    fn new(alloc: &Allocator<'_>, objects: &Arc<Pool>, context: &Context, m5: bool) -> Result<Self> {
        let page = mmu::UAT_PGSZ as u64;
        let prot = mmu::PROT_GPU_FW_SHARED_RW;
        let cpu = CpuMap::WriteCombined;
        let ring = DescriptorRing::new(alloc, objects, m5)?;
        let preempt = alloc.kernel(0x4000, page, prot, cpu)?;
        let usage = alloc.kernel(mmu::UAT_PGSZ, page, prot, cpu)?;
        let operand = alloc.lower(if m5 { crate::g17::fw::m5::compute::OPERAND_BYTES }
            else { 0x14000 }, page, prot, cpu)?;
        let mut support = alloc.kernel(if m5 { mmu::UAT_PGSZ } else { 0xc000 }, page, prot, cpu)?;
        if !m5 {
            let support_va = support.gpu_va();
            support.write(0, support_va + page)?;
            support.write(0x100, support_va + page + 4)?;
            support.write(0x110, 0x50u32)?;
            support.write(mmu::UAT_PGSZ + 4, 1u32)?;
        }
        // Native queue headers/items are cached while producer/consumer words
        // are uncached. Keep their pages separate when selecting initial mapping attributes.
        let cached_graph = m5;
        let pointers = if cached_graph { GRAPH_SIZE } else { POINTERS };
        let mut graph = if cached_graph {
            DescriptorStorage::Retained(objects.allocate(alloc, Family::QueueState,
                GRAPH_SIZE + mmu::UAT_PGSZ, GRAPH_SIZE, prot, cpu)?)
        } else {
            DescriptorStorage::Ordinary(alloc.kernel(GRAPH_SIZE, page, prot, cpu)?)
        };
        initialize_graph_at(&mut graph, context, m5, pointers)?;
        let (context_low, context_high) = if m5 { (None, None) } else {
            (Some(alloc.lower(CONTEXT_SIZE, page, prot, cpu)?),
             Some(alloc.kernel(CONTEXT_SIZE, page, prot, cpu)?))
        };
        let records = alloc.kernel(RECORD_SLOTS * RECORD_STRIDE, page, prot, cpu)?;
        let m5_timestamps = if m5 {
            Some(Arc::new(alloc.kernel(RECORD_SLOTS * 16, page, prot, cpu)?, GFP_KERNEL)?)
        } else { None };
        Ok(Self {
            m5,
            ring: Some(ring),
            preempt,
            usage,
            operand,
            support,
            m5_timestamps,
            graph,
            pointers,
            context_low,
            context_high,
            records,
            record_ends: [None; RECORD_SLOTS],
            reclaimed: 0,
        })
    }

    /// All fallible mapping work finishes before replacing a physical queue's logical owner.
    fn bind(
        &mut self,
        context: Arc<Context>,
        kick_alias: mmu::KernelMapping,
        pool_alias: mmu::KernelMapping,
    ) -> Result<Binding> {
        if !context.is_current() {
            return Err(EFAULT);
        }
        let vm = context.vm();
        if !self.m5 { vm.install_job_context_aliases()?; }
        let descriptors = self.descriptor_alias(vm)?;
        let prot = if self.m5 { mmu::PROT_GPU_SHARED_RW } else { mmu::PROT_GPU_FW_SHARED_RW };
        let page = mmu::UAT_PGSZ as u64;
        let preempt = self
            .preempt
            .alias_in(vm, dynamic::CLIENT_LOWER, page, prot)?;
        let usage = self.usage.alias_in(vm, dynamic::CLIENT_LOWER, page, prot)?;
        let operand = self
            .operand
            .alias_in(vm, dynamic::CLIENT_LOWER, page, prot)?;
        let queue_context = self.context_low.as_ref()
            .map(|context| context.alias_in(vm, dynamic::CLIENT_LOWER, page, prot)).transpose()?;
        Ok(Binding {
            _kick_alias: kick_alias,
            pool_alias: Some(pool_alias),
            descriptors: Some(descriptors),
            preempt,
            usage,
            operand,
            _queue_context: queue_context,
            context,
        })
    }

    /// Caller has excluded publication and proved the retained graph idle.
    fn set_owner(&mut self, context: &Context) -> Result {
        if self.m5 {
            let pointer = self.graph.pointer(0,
                core::mem::size_of::<crate::g17::fw::m5::queue::QueueRecord>())?
                .cast::<crate::g17::fw::m5::queue::QueueRecord>();
            // SAFETY: The checked packed record stays mapped. The device mutex
            // and idle witness exclude access by another owner during this update.
            return unsafe { (*pointer).set_compute_owner(context.work_head_va(),
                context.scheduler_va(), context.owner_pid(), context.policy()) };
        }
        let pointer = self
            .graph
            .pointer(0, core::mem::size_of::<QueueRecord>())?
            .cast::<QueueRecord>();
        // SAFETY: The checked record is in the retained mapping, has byte alignment and consists
        // only of integer storage. The device mutex and idle witness exclude simultaneous access.
        unsafe {
            (*pointer).set_compute_owner(
                context.work_head_va(),
                context.scheduler_va(),
                context.owner_pid(),
                context.policy(),
            )
        };
        Ok(())
    }

    fn pointer_word(&self, offset: usize) -> Result<&AtomicU32> {
        self.graph.word(self.pointers + offset)
    }
    fn cursors(&self) -> Result<Cursors> {
        Ok(Cursors {
            producer: self.pointer_word(0x40)?.load(Ordering::Relaxed),
            consumer: self.pointer_word(0)?.load(Ordering::Relaxed),
            count: self.pointer_word(0x60)?.load(Ordering::Relaxed),
        })
    }
    fn stamp(&self) -> Result<u32> {
        fence(Ordering::Acquire);
        Ok(self.support.word(STAMP)?.load(Ordering::Relaxed))
    }
    /// EAGAIN while the queue is vacant: the owner must re-arm it first.
    fn descriptor_vas(&self, slot: u8) -> Result<(u64, u64)> {
        let ring = self.ring.as_ref().ok_or(EAGAIN)?;
        let offset = (usize::from(slot) % DESCRIPTORS * COMPUTE_DESCRIPTOR_SIZE) as u64;
        Ok((ring.firmware_va() + offset, ring.gpu_va() + offset))
    }
    fn descriptor_alias(&self, vm: &mmu::Vm) -> Result<mmu::KernelMapping> {
        let ring = self.ring.as_ref().ok_or(EAGAIN)?;
        ring.object
            .map_alias(vm, ring.gpu_va(), mmu::PROT_GPU_SHARED_RO)
    }
    /// Detaches the descriptor ring of an idle vacant queue; the caller frees it
    /// off-lock. The job list and the context item only name consumed
    /// descriptors at this point.
    fn vacate_ring(&mut self) -> Option<DescriptorRing> {
        self.ring.take()
    }
    fn rearm_ring(&mut self, ring: DescriptorRing) -> Result {
        if self.ring.is_some() {
            return Err(EBUSY);
        }
        self.ring = Some(ring);
        Ok(())
    }
    fn ensure_record_slot(&self, slot: usize) -> Result {
        if let Some(end) = *self.record_ends.get(slot).ok_or(EINVAL)? {
            let cursors = self.cursors()?;
            if !cursors.consumed(end, cursors.count)? {
                return Err(EBUSY);
            }
        }
        Ok(())
    }

    /// A killed queue's ring restarts empty at the firmware's consumer cursor: every
    /// item slot is cleared and the host producer is set to the consumer. Only
    /// valid after the firmware acknowledged the kill and no command is pending.
    fn reset_items(&mut self) -> Result {
        let cursors = self.cursors()?;
        if cursors.count != ITEM_RING_ENTRIES || cursors.consumer >= cursors.count {
            return Err(EIO);
        }
        for index in 0..ITEM_RING_ENTRIES as usize {
            self.graph.dword(ITEMS + index * 8)?.store(0, Ordering::Relaxed);
        }
        fence(Ordering::SeqCst);
        self.pointer_word(0x40)?.store(cursors.consumer, Ordering::Relaxed);
        self.reclaimed = cursors.consumer;
        fence(Ordering::SeqCst);
        Ok(())
    }
    /// Reclaim only consumed item pointers, with individual aligned stores and a release fence.
    /// This leaves records still named by firmware untouched across arbitrarily many wraps.
    fn recycle(&mut self, cursors: Cursors) -> Result {
        if cursors.count != ITEM_RING_ENTRIES {
            return Err(EIO);
        }
        if cursors.consumer >= cursors.count || self.reclaimed >= cursors.count {
            return Err(ERANGE);
        }
        let done = cursors.consumer;
        while self.reclaimed != done {
            self.graph
                .dword(ITEMS + self.reclaimed as usize * 8)?
                .store(0, Ordering::Relaxed);
            self.reclaimed = (self.reclaimed + 1) % cursors.count;
        }
        fence(Ordering::SeqCst);
        Ok(())
    }
}

use crate::g17::{
    completion::Deferred, context::WorkStateLease, freelist::ComputePool, job::Packet, kick, qos,
    recovery, status::VmStatus,
};
use retirement::{Batch, Observation, Recordless, Ticket, IN_FLIGHT};

struct Fifo<T> {
    entries: [Option<T>; IN_FLIGHT],
    head: usize,
    len: usize,
}
impl<T> Fifo<T> {
    fn new() -> Self {
        Self {
            entries: core::array::from_fn(|_| None),
            head: 0,
            len: 0,
        }
    }
    fn front(&self) -> Option<&T> {
        self.entries[self.head].as_ref()
    }
    fn back(&self) -> Option<&T> {
        if self.len == 0 {
            None
        } else {
            self.entries[(self.head + self.len - 1) % IN_FLIGHT].as_ref()
        }
    }
    fn back_mut(&mut self) -> Option<&mut T> {
        if self.len == 0 {
            None
        } else {
            self.entries[(self.head + self.len - 1) % IN_FLIGHT].as_mut()
        }
    }
    fn push(&mut self, value: T) -> core::result::Result<(), T> {
        if self.len == IN_FLIGHT {
            return Err(value);
        }
        self.entries[(self.head + self.len) % IN_FLIGHT] = Some(value);
        self.len += 1;
        Ok(())
    }
    fn pop(&mut self) -> Option<T> {
        if self.len == 0 {
            return None;
        }
        let value = self.entries[self.head].take();
        self.head = (self.head + 1) % IN_FLIGHT;
        self.len -= 1;
        value
    }
    fn pop_back(&mut self) -> Option<T> {
        if self.len == 0 {
            return None;
        }
        self.len -= 1;
        self.entries[(self.head + self.len) % IN_FLIGHT].take()
    }
    fn iter(&self) -> impl Iterator<Item = &T> {
        Iterator::chain(
            self.entries[self.head..].iter(),
            self.entries[..self.head].iter(),
        )
        .take(self.len)
        .filter_map(Option::as_ref)
    }
    fn iter_mut(&mut self) -> impl Iterator<Item = &mut T> {
        let (before, after) = self.entries.split_at_mut(self.head);
        Iterator::chain(after.iter_mut(), before.iter_mut())
            .take(self.len)
            .filter_map(Option::as_mut)
    }
}

struct Active {
    packet: Arc<Packet>,
    /// The command is on this physical queue: its identity and QoS slot stay.
    activity: crate::g17::context::ActivityMark,
    published: u64,
    work: Arc<WorkStateLease>,
    ticket: Option<Ticket>,
    observed: Option<Observation>,
    /// Host time the completion record was first observed.
    observed_at: Option<u64>,
    failure_deferred: bool,
    qos_pending: bool,
}
struct Previous {
    binding: Option<Binding>,
    context: Arc<Context>,
    publication: Option<(u64, u64)>,
}
/// Per-queue addresses of the bound owner, read under the publication preflight.
struct BindingAddresses {
    preempt: u64,
    operand: u64,
    usage: u64,
}

#[derive(Copy, Clone, PartialEq, Eq)]
struct ConfigIdentity {
    scheduler: u64,
    qos: u8,
    context: u32,
}

/// A private physical pool whose descriptor row is installed before allocating its kick ring.
/// The QID remains unpublished throughout construction.
pub(crate) struct PoolBacking {
    pool: ComputePool,
    id: kick::Id,
}

impl PoolBacking {
    pub(crate) fn new(alloc: &Allocator<'_>, id: kick::Id, m5: bool, required: Option<crate::g17::fw::m5::pool::PoolGeometry>) -> Result<Self> {
        let pool = ComputePool::new(alloc, id.qid(), m5, required)?;
        Ok(Self { pool, id })
    }

    /// Fresh page-pool row data, installed once while the QID is still unpublished.
    pub(crate) fn pool_descriptor(&self) -> Result<crate::g17::freelist::PoolDescriptor> {
        self.pool.descriptor()
    }
}

/// A physical pool and kick ring prepared before the device-global compute scheduler and
/// descriptor graph. No logical VM aliases or firmware publication exist at this stage.
pub(crate) struct Backing {
    pool: ComputePool,
    kick: kick::Queue,
}

impl Backing {
    /// `channel` is the spare ring of a recycled QID (vmfix6), already
    /// re-identified for `id`; it is adopted only here, after the pool exists.
    pub(crate) fn new(alloc: &Allocator<'_>, backing: PoolBacking,
        channel: &mut Option<kick::Queue>) -> Result<Self> {
        let PoolBacking { pool, id } = backing;
        let kick = match channel.take() {
            Some(queue) if queue.id() == id => queue,
            Some(queue) => {
                *channel = Some(queue);
                return Err(EINVAL);
            }
            None => kick::Queue::compute(alloc, id)?,
        };
        Ok(Self { pool, kick })
    }
}

/// Installed graph and its exact logical owner. A released idle graph may change owners only
/// after a retirement witness. Failed work retains its pointers independently of fence state.
/// USC pool backing of a retained queue. A release is published only for an idle
/// queue without an owner; the backing is dropped once the firmware consumed it.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum PoolPhase {
    Populated,
    ReleaseSent(u32),
    Vacant,
}

pub(crate) struct Queue {
    owner: Option<u64>,
    /// Diagnostic budget belongs to one physical queue binding, not its shared VM.
    entry_traces: u32,
    /// Separate from publication preparation slots: readers only prevent
    /// owner/priority reconfiguration and do not consume execution capacity.
    source_readers: Arc<AtomicUsize>,
    source_available: Arc<crate::g17::availability::Availability>,
    /// Scheduler/work pages of an exited last owner whose context was dropped;
    /// the installed queue record still names them until the next owner binds.
    retained_owner: Option<crate::g17::context::FirmwarePages>,
    pool_phase: PoolPhase,
    growing: bool,
    /// A killed queue was recycled: its pool/ring must be vacated and re-armed
    /// fresh before any owner binds it (set by `recycle_killed`, cleared by `rearm`).
    needs_fresh_backing: bool,
    /// Monotonic time the last owner released this queue (vacate order).
    released_at: u64,
    /// Free-list generation of the last published configuration.
    free_list_generation: u64,
    /// `None` after an exited owner was detached; a new owner binds afresh.
    binding: Option<Binding>,
    previous: Option<Previous>,
    kick: kick::Queue,
    pool: ComputePool,
    graph: Graph,
    active: Fifo<Active>,
    replays: Fifo<Arc<Packet>>,
    replay_retirement: bool,
    replay_error: Option<Error>,
    replay_prepared: bool,
    exit: Option<exit::Transaction>,
    installed: bool,
    configuration: Option<configuration::Configuration>,
    m5_install: Option<u32>,
    /// Bit `p` set once a work slot of priority class `p` named this queue.
    outer_started: u8,
    published_config: Option<ConfigIdentity>,
    ordinal: u32,
    submitted: u32,
    last_end: u64,
    released: bool,
    retirement_ready: bool,
    quarantined: bool,
    retired_by_teardown: bool,
    retire_pending: bool,
    awaiting_witness: bool,
    spared_quarantine: bool,
    spared_deferred: bool,
    terminal: Option<(Arc<VmStatus>, Error)>,
    quarantine_error: Option<Error>,
    failure_status_pending: bool,
    retirement_proved: bool,
    /// A replay of this queue waits (since this time, ns) for its owner's renders.
    consumer_wait_since: Option<u64>,

}

impl Queue {
    /// The caller reserves a QID first, cancels it on construction failure, and retains every
    /// successfully installed queue until processor stop. Shared VM apertures precede this call.
    pub(crate) fn new(
        alloc: &Allocator<'_>,
        objects: &Arc<Pool>,
        backing: Backing,
        owner: u64,
        context: Arc<Context>,
        m5: bool,
        source_available: Arc<crate::g17::availability::Availability>,
    ) -> Result<Self> {
        let Backing { mut pool, mut kick } = backing;
        let mut graph = Graph::new(alloc, objects, &context, m5).map_err(|error| {
            dev_info!(alloc.dev.as_ref(), "G17 compute backing failed: {:?}\n", error); error
        })?;
        let kick_alias = kick.map_client(context.vm())?;
        let pool_alias = pool.map_client(context.vm())?;
        let binding = graph.bind(context.clone(), kick_alias, pool_alias).map_err(|error| {
            dev_info!(alloc.dev.as_ref(), "G17 compute client aliases failed: {:?}\n", error); error
        })?;
        let source_readers = Arc::new(AtomicUsize::new(0), GFP_KERNEL)?;
        context.mark_published();
        Ok(Self {
            owner: Some(owner),
            entry_traces: 0,
            source_readers,
            source_available,
            retained_owner: None,
            pool_phase: PoolPhase::Populated,
            growing: false,
            needs_fresh_backing: false,
            released_at: 0,
            free_list_generation: 0,
            binding: Some(binding),
            previous: None,
            kick,
            pool,
            graph,
            active: Fifo::new(),
            replays: Fifo::new(),
            replay_retirement: false,
            replay_error: None,
            replay_prepared: false,
            exit: None,
            installed: false,
            configuration: None,
            m5_install: None,
            outer_started: 0,
            published_config: None,
            ordinal: 0,
            submitted: 0,
            last_end: 0,
            released: false,
            retirement_ready: false,
            quarantined: false,
            retired_by_teardown: false,
            retire_pending: false,
            awaiting_witness: false,
            spared_quarantine: false,
            spared_deferred: false,
            terminal: None,
            quarantine_error: None,
            failure_status_pending: false,
            retirement_proved: false,
            consumer_wait_since: None,
        })
    }
    pub(crate) fn qid(&self) -> u8 {
        self.kick.id().qid()
    }

    /// Includes partially published retained commands without a final ticket.
    pub(crate) fn has_active_source_consumers(&self) -> bool {
        self.active.iter().any(|active| !active.packet.sources.is_empty())
    }

    pub(crate) fn source_held(&self) -> bool {
        self.source_readers.load(Ordering::Acquire) != 0
    }

    /// Called under the device mutex after current-epoch source resolution.
    pub(crate) fn hold_source(&self, source: &Arc<crate::g17::sync_source::Source>,
        token: crate::g17::sync_source::Token) -> Result<crate::g17::source_hold::SourceHold>
    {
        use crate::g17::sync_source::Snapshot;
        if !self.graph.m5 || !self.installed || self.released || self.quarantined
            || self.exit.is_some() || self.owner != Some(token.owner) || self.qid() != token.qid
        { return Err(EAGAIN); }
        let current = match source.snapshot(token.epoch)? {
            Snapshot::Published(current) => current,
            _ => return Err(EAGAIN),
        };
        if current.owner != token.owner || current.sequence != token.sequence
            || current.context != token.context || current.qid != token.qid
            || current.kick != token.kick || current.epoch != token.epoch
            || current.ordinal != token.ordinal
        { return Err(EAGAIN); }
        let active = self.active.iter().find(|active| {
            active.packet.completion.is_source(source)
                && active.ticket.is_some_and(|ticket| ticket.owner == token.owner
                    && ticket.submission == token.sequence && ticket.context_id == token.context
                    && ticket.qid == token.qid && ticket.kick == token.kick.get()
                    && ticket.epoch == token.epoch && ticket.ordinal == token.ordinal)
        }).ok_or(EAGAIN)?;
        let ticket = active.ticket.ok_or(EAGAIN)?;
        self.valid_ticket(active, &ticket)?;
        if !active.packet.context.is_current() || active.packet.completion.signalled()
            || self.binding.as_ref().is_none_or(|binding|
                !Arc::ptr_eq(&binding.context, &active.packet.context))
        { return Err(EAGAIN); }
        crate::g17::source_hold::SourceHold::new(&self.source_readers, &self.source_available)
    }
    pub(crate) fn owner(&self) -> Option<u64> {
        self.owner
    }
    /// The bound owner's context; `None` once an exited owner was detached.
    pub(crate) fn context(&self) -> Option<&Arc<Context>> {
        self.binding.as_ref().map(|binding| &binding.context)
    }
    /// Quarantined commands still retain the scheduler until their retirement witness.
    pub(crate) fn idle_for_context(&self, context: &Arc<Context>) -> bool {
        !self.context().is_some_and(|bound| Arc::ptr_eq(bound, context))
            || (self.active.len == 0 && self.replays.len == 0 && self.configuration.is_none())
    }
    pub(crate) fn in_flight(&self) -> bool {
        (self.active.len != 0 || self.configuration.is_some()) && !self.quarantined
    }
    pub(crate) fn room(&self) -> bool {
        !self.quarantined && !self.released && self.replays.len == 0 && self.active.len < IN_FLIGHT
    }
    /// Cold M5 installation publishes one configuration before the first kick.
    /// A prepared claim prevents reassignment while these credits are owned.
    pub(crate) fn outer_slots(&self) -> u32 {
        if self.graph.m5 && self.m5_install.is_none() { 2 } else { 1 }
    }
    /// Publication slots not occupied by retained commands. Registry subtracts
    /// unpublished packet claims under the device mutex before reserving one.
    pub(crate) fn preparation_slots(&self) -> usize {
        if self.room() && self.configuration.is_none() { IN_FLIGHT - self.active.len } else { 0 }
    }
    /// Quarantines without a pending witness no longer describe live pipe order.
    pub(crate) fn timeout_head(&self) -> Option<crate::g17::timeout::Head> {
        if self.quarantined && !self.awaiting_witness {
            return None;
        }
        self.active.front().map(|active| crate::g17::timeout::Head {
            vm: Arc::as_ptr(active.packet.completion.status()) as usize,
            published: active.published,
            finished: if self.graph.m5 {
                active.ticket.is_some_and(|ticket| self.graph.stamp()
                    .is_ok_and(|stamp| retirement::stamp_covers(stamp, ticket.kick)))
            } else { active.observed.is_some() },
        })
    }

    pub(crate) fn can_classify_timeout(&self, packet: &Arc<Packet>) -> bool {
        !self.quarantined && self.owns_packet(packet)
    }

    /// Device mutex held; read existing CPU mappings only. Never take context,
    /// preparation, lifetime or fence locks, nor release any retained resource.
    pub(crate) fn timeout_snapshot(&mut self, packet: &Arc<Packet>, out: &mut crate::g17::timeout_trace::Snapshot) {
        out.victim_index = u32::MAX;
        out.owner = self.owner.unwrap_or(0);
        out.pid = packet.context.owner_pid();
        out.context = u64::from(packet.context.id());
        out.packet = packet.timeout_trace_words();
        out.queue = [u64::from(self.qid()), u64::from(self.ordinal), u64::from(self.submitted),
            self.last_end, self.kick.timestamp().get(), self.kick.high_water().get(), self.kick.parent().get(),
            u64::from(self.quarantined), u64::from(self.awaiting_witness), u64::from(self.retire_pending),
            u64::from(self.retirement_proved), u64::from(self.released), u64::from(self.growing),
            u64::from(self.configuration.is_some()), u64::from(self.m5_install.is_some()),
            match self.pool_phase { PoolPhase::Populated=>0, PoolPhase::ReleaseSent(c)=>1|(u64::from(c)<<32), PoolPhase::Vacant=>2 }];
        match self.graph.cursors() {
            Ok(c) => out.cursors = [c.producer,c.consumer,c.count],
            Err(e) => out.cursor_error=e.to_errno(),
        }
        match self.graph.stamp() { Ok(s)=>out.stamp=s, Err(e)=>out.stamp_error=e.to_errno() }
        match self.pool.completed_submissions() { Ok(n)=>out.pool_completed=n, Err(e)=>out.pool_error=e.to_errno() }
        let mut victim = None;
        for (i,active) in self.active.iter().enumerate() {
            if i >= crate::g17::timeout_trace::ACTIVE { out.reason=crate::g17::timeout_trace::OVERFLOW;break; }
            out.active_count=i as u32+1;
            let row=&mut out.items[i];
            row[0]=Arc::as_ptr(&active.packet) as usize as u64;
            row[1]=active.packet.id;row[2]=active.packet.order.sequence;row[3]=active.published;
            row[4]=u64::from(active.ticket.is_some()) | (u64::from(active.observed.is_some())<<1)
                | (u64::from(active.qos_pending)<<2) | (u64::from(active.failure_deferred)<<3);
            if let Some(t)=active.ticket {
                row[5]=t.kick;row[6]=t.owner;row[7]=t.submission;row[8]=u64::from(t.context_id);
                row[9]=t.descriptor;row[10]=t.work_node;row[11]=u64::from(t.ordinal);
                row[12]=u64::from(t.item_producer);row[13]=u64::from(t.item_count);row[14]=t.epoch;
            }
            if let Some(o)=active.observed { row[15]=o.timestamps[0];row[16]=o.timestamps[1]; }
            row[17]=active.observed_at.unwrap_or(0);
            if Arc::ptr_eq(&active.packet,packet) { out.victim_index=i as u32;victim=active.ticket; }
        }
        if out.victim_index==u32::MAX {
            if out.reason == 0 { out.reason=crate::g17::timeout_trace::NOT_OWNED; }
            return;
        }
        out.kick_error=-61;out.descriptor_error=-61; // ENODATA until a ticket/map is validated.
        if let Some(t)=victim {
            if let Some(active) = self.active.iter().nth(out.victim_index as usize) {
                active.work.timeout_work_snapshot(t.work_node, &mut out.work);
            }
            out.raw_identity[0]=t.descriptor;
            if let Some(k)=crate::g17::fw::kick::KickTimestamp::new(t.kick) {
                match self.kick.timeout_words(k,&mut out.kick_words) {
                    Ok(())=>{
                        let word=|i| u64::from(out.kick_words[i])|(u64::from(out.kick_words[i+1])<<32);
                        let header=word(0);out.raw_identity[1]=word(4);
                        out.kick_error=if ((header>>2)&0xff_ffff_ffff)==t.kick
                            && ((header>>42)&0x7f)==u64::from(t.qid) && word(4)==t.descriptor {0} else {-(kernel::bindings::EUCLEAN as i32)};
                    },
                    Err(e)=>out.kick_error=e.to_errno(),
                }
                if let Some(ring)=self.graph.ring.as_ref() {
                    let slot=k.slot()%DESCRIPTORS;let offset=slot*COMPUTE_DESCRIPTOR_SIZE;
                    out.raw_identity[2]=ring.firmware_va()+offset as u64;
                    out.raw_identity[3]=ring.gpu_va()+offset as u64;
                    if out.raw_identity[2]!=t.descriptor { out.descriptor_error=-(kernel::bindings::EUCLEAN as i32); }
                    else {
                        out.descriptor_error=0;
                        for (i,word) in out.descriptor_words.iter_mut().enumerate() {
                            match ring.object.word(offset+i*4) { Ok(v)=>*word=v.load(Ordering::Relaxed), Err(e)=>{out.descriptor_error=e.to_errno();break;} }
                        }
                    }
                }
            } else { out.kick_error=EINVAL.to_errno(); }
        }
        out.valid=1;
    }

    /// Compact queue status for a dependency QID, with no nested ownership locks.
    pub(crate) fn timeout_peer(&self) -> [u64; 12] {
        let c=self.graph.cursors();let stamp=self.graph.stamp();let front=self.active.front();
        [self.owner.unwrap_or(0),u64::from(self.qid()),self.active.len as u64,u64::from(self.ordinal),
         stamp.map_or(u64::MAX,u64::from),c.as_ref().map_or(u64::MAX,|v|u64::from(v.producer)),
         c.as_ref().map_or(u64::MAX,|v|u64::from(v.consumer)),c.as_ref().map_or(u64::MAX,|v|u64::from(v.count)),
         front.and_then(|f|f.ticket).map_or(0,|t|t.kick),front.map_or(0,|f|f.published),
         u64::from(self.quarantined)|(u64::from(self.awaiting_witness)<<1)|(u64::from(self.growing)<<2),
         match self.pool_phase {PoolPhase::Populated=>0,PoolPhase::ReleaseSent(n)=>1|(u64::from(n)<<32),PoolPhase::Vacant=>2}]
    }

    pub(crate) fn oldest_spared(&self) -> Option<bool> {
        self.active
            .front()
            .map(|a| a.packet.completion.spared() && a.packet.completion.replay_count() == 0)
    }
    pub(crate) fn visit_owners(&self, visit: &mut dyn FnMut(&Arc<VmStatus>)) {
        for active in self.active.iter() {
            visit(active.packet.completion.status());
        }
    }
    pub(crate) fn classify(&self, class: &dyn Fn(&Arc<VmStatus>) -> recovery::Class) {
        for active in self.active.iter() {
            active
                .packet
                .completion
                .classify(class(active.packet.completion.status()));
        }
    }
    fn qos_owner(&self) -> qos::Owner {
        qos::Owner {
            qid: self.qid(),
            // Accounting completes only for bound owners; a detached queue has no
            // pending work and never reaches this.
            qos: self.binding.as_ref().map_or(0, |binding| binding.context.qos_id()),
            data_master: DataMaster::Compute as u8,
        }
    }
    /// A completion hint can require a second visibility/recordless pass even
    /// when firmware has no more events to send. Do not poll idle or failed queues.
    pub(crate) fn retirement_polling(&self) -> bool {
        self.configuration.is_some() || (!self.quarantined && self.retire_pending)
    }
    /// One diagnostic line describing the in-flight front after a recovery.
    pub(crate) fn describe_front(&self) {
        let Some(front) = self.active.front() else {
            return;
        };
        pr_info!(
            "G17 recovery compute qid {} owner {:?} front kick {:?} record {:?} live {:?} stamp {:?} active {}\n",
            self.qid(),
            self.owner,
            front.ticket.map(|t| t.kick),
            front.observed.map(|o| o.timestamps),
            front.packet.completion.compute_words(),
            self.graph.stamp(),
            self.active.len
        );
    }
    /// Per-queue consumer wait (see compute_replay).
    pub(crate) fn consumer_wait(&mut self, now: u64) -> u64 {
        now.saturating_sub(*self.consumer_wait_since.get_or_insert(now))
    }
    pub(crate) fn clear_consumer_wait(&mut self) {
        self.consumer_wait_since = None;
    }

    fn selected(&self, masks: Option<[u64; 2]>) -> bool {
        self.retired_by_teardown
            || self.retire_pending
            || masks.is_none_or(|m| m[self.qid() as usize / 64] & (1 << (self.qid() % 64)) != 0)
    }
    /// Stage every unlatched exact identity before the device's single shared-ring scan.
    pub(crate) fn request_observations(
        &self,
        batch: &mut Batch,
        masks: Option<[u64; 2]>,
    ) -> Result<bool> {
        if self.quarantined || !self.selected(masks) {
            return Ok(false);
        }
        let mut requested = false;
        for active in self.active.iter() {
            let Some(ticket) = active.ticket else {
                break;
            };
            if active.observed.is_none() {
                batch.request(&ticket)?;
                requested = true;
            }
        }
        Ok(requested)
    }
    pub(crate) fn latch_observations(
        &mut self,
        batch: &mut Batch,
        masks: Option<[u64; 2]>,
    ) -> Result {
        if self.quarantined || !self.selected(masks) {
            return Ok(());
        }
        for active in self.active.iter_mut() {
            let Some(ticket) = active.ticket else {
                break;
            };
            if active.observed.is_none() {
                active.observed = batch.take(&ticket)?;
                if active.observed.is_some() {
                    active.observed_at = Some(now_ns());
                }
            }
        }
        Ok(())
    }
    fn front_finished(&self) -> bool {
        self.active.front().is_some_and(|active| {
            active.observed.is_some()
                || active.ticket.is_some_and(|ticket| {
                    self.graph
                        .cursors()
                        .and_then(|c| c.consumed(ticket.item_producer, ticket.item_count))
                        .unwrap_or(false)
                })
        })
    }
    fn valid_ticket(&self, active: &Active, ticket: &Ticket) -> Result {
        if ticket.qid != self.qid()
            || ticket.descriptor == 0
            || ticket.item_count == 0
            || ticket.work_node != active.work.node_va()?
            || self.submitted == 0
            || self.owner != Some(ticket.owner)
            || active.packet.order.sequence != ticket.submission
            || active.packet.context.id() != ticket.context_id
            || self
                .binding
                .as_ref()
                .is_none_or(|binding| active.packet.context.id() != binding.context.id())
        {
            return Err(EIO);
        }
        Ok(())
    }

    /// Return VM identity for queue-local failure fanout. Deferred callbacks are collected under
    /// the mutex and invoked afterwards; failed packets stay retained until their witness.
    pub(crate) fn quarantine(
        &mut self,
        error: Error,
        defer: &mut impl FnMut(Deferred) -> Result,
    ) -> Result<Option<Arc<VmStatus>>> {
        if !self.quarantined {
            self.quarantined = true;
            self.quarantine_error = Some(error);
            self.failure_status_pending = true;
            self.retirement_proved = false;
            self.retired_by_teardown = false;
            let all_spared =
                self.active.len != 0 && self.active.iter().all(|a| a.packet.completion.spared());
            self.spared_quarantine = all_spared && error == ENODATA;
            self.spared_deferred = self.spared_quarantine;
        }
        let error = self.quarantine_error.ok_or(EIO)?;
        let mut terminal = None;
        for active in self.active.iter_mut() {
            if !self.spared_deferred && !active.failure_deferred {
                defer(Deferred::FailedOwned(
                    active.packet.completion.clone(),
                    error,
                ))?;
                active.failure_deferred = true;
            }
            if self.failure_status_pending
                && !active.packet.completion.spared()
                && terminal.is_none()
            {
                terminal = Some(active.packet.completion.status().clone());
            }
        }
        self.failure_status_pending = false;
        self.awaiting_witness =
            self.active.len != 0 && self.active.iter().all(|a| a.ticket.is_some());
        Ok(terminal)
    }

    /// Poll only the ordered prefix; the next pass may recheck a finished front on another QID's
    /// event. A final identity-less scan allows recordless retirement without the one-pass grace.
    pub(crate) fn retire(
        &mut self,
        masks: Option<[u64; 2]>,
        epoch: u64,
        recovery_state: u32,
        mut complete_qos: impl FnMut(qos::Owner) -> Result,
        defer: &mut impl FnMut(Deferred) -> Result,
    ) -> Result {
        if recovery_state == 0 { self.retire_configuration(epoch)?; }
        if self.quarantined || !self.selected(masks) {
            return Ok(());
        }
        let finished_before = self.retire_pending;
        self.retire_pending = false;
        let mut oldest = true;
        while let Some(front) = self.active.front() {
            let result = (|| {
                let ticket = front.ticket.ok_or(EIO)?;
                self.valid_ticket(front, &ticket)?;
                if recovery_state != 0 {
                    return Err(EAGAIN);
                }
                let recordless = if epoch != ticket.epoch {
                    Recordless::CrossedRecovery
                } else if masks.is_none() || (finished_before && oldest) {
                    Recordless::Allowed
                } else {
                    Recordless::NotYet
                };
                let next_end = self
                    .active
                    .iter()
                    .skip(1)
                    .find_map(|a| a.observed.map(|o| o.timestamps[1]));
                let stamp = self.graph.stamp()?;
                let record_aged = front
                    .observed_at
                    .is_some_and(|at| now_ns().saturating_sub(at) >= retirement::STAMP_GRACE_NS);
                // The M5 completion record is not sufficient to return buffer
                // ownership before its completion stamp. Keep the record-only
                // grace policy confined to the profile where it was qualified.
                if self.graph.m5 && record_aged && !retirement::stamp_covers(stamp, ticket.kick) {
                    static WAIT_LOGS: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);
                    if WAIT_LOGS.fetch_add(1, Ordering::Relaxed) < 16 {
                        pr_info!("M5 compute visibility wait: pid {} context {} qid {} kick {:#x} stamp {:#x} record {:?} live {:?} pool {:?}\n",
                            front.packet.context.owner_pid(), ticket.context_id, ticket.qid,
                            ticket.kick, stamp, front.observed.map(|o| o.timestamps),
                            front.packet.completion.compute_words(), self.pool.completed_submissions());
                    }
                }
                let timestamps = retirement::poll(
                    &ticket,
                    front.observed,
                    self.graph.cursors()?,
                    stamp,
                    recordless,
                    self.last_end,
                    next_end,
                    record_aged && !self.graph.m5,
                )?;
                if timestamps.is_some() && !retirement::stamp_covers(stamp, ticket.kick) {
                    retirement::note_stampless(self.qid(), ticket.kick);
                }
                if timestamps.is_some()
                    && self.previous.as_ref().is_some_and(|p| {
                        p.publication != Some((front.packet.order.sequence, ticket.epoch))
                    })
                {
                    return Err(EIO);
                }
                Ok(timestamps)
            })();
            oldest = false;
            match result {
                Ok(Some(timestamps)) => {
                    crate::g17::lat::note(crate::g17::lat::GPU_START, timestamps[0]);
                    crate::g17::lat::note(crate::g17::lat::GPU_END, timestamps[1]);
                    let submitted = self.submitted.checked_sub(1).ok_or(EIO)?;
                    if front.qos_pending {
                        complete_qos(self.qos_owner())?;
                        self.active.iter_mut().next().ok_or(EIO)?.qos_pending = false;
                    }
                    let front = self.active.front().ok_or(EIO)?;
                    if self.graph.m5 && front.packet.completion.has_timestamps() {
                        static TS_LOGS: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);
                        if TS_LOGS.fetch_add(1, Ordering::Relaxed) < 16 {
                            pr_info!("M5 timestamp747: qid={} kick={:#x} record={:?} execution={:?}\n",
                                self.qid(), front.ticket.ok_or(EIO)?.kick, timestamps,
                                front.packet.completion.compute_words());
                        }
                    }
                    self.retire_pending = true;
                    defer(Deferred::RetiredCompute(
                        front.packet.completion.clone(),
                        [timestamps[0], timestamps[1], 0, 0],
                        front.observed.map(|record| record.timestamps),
                    ))?;
                    self.retire_pending = false;
                    self.submitted = submitted;
                    let mut active = self.active.pop().ok_or(EIO)?;
                    // Exact retirement: the identity may move once nothing pins
                    // it and no command of its VM remains on a physical queue.
                    active.activity.end();
                    active.packet.unpin_retired();
                    if active.observed.is_some() {
                        self.last_end = self.last_end.max(timestamps[1]);
                    }
                    drop(self.previous.take());
                    self.retirement_ready = self.active.len == 0;
                    if self.released && self.retirement_ready {
                        self.owner = None;
                    }
                }
                Ok(None) | Err(EAGAIN) => {
                    self.retire_pending = self.front_finished();
                    break;
                }
                Err(error) => {
                    self.terminal = self.quarantine(error, defer)?.map(|status| (status, error));
                    return Ok(());
                }
            }
        }
        Ok(())
    }

    /// Exact terminal witness releases command pins, not installed graph ownership. Spared
    /// zero-duration work can return its queue to service after settling its accounting.
    pub(crate) fn release_witnessed(
        &mut self,
        recovery_state: u32,
        mut complete_qos: impl FnMut(qos::Owner) -> Result,
        defer: &mut impl FnMut(Deferred) -> Result,
    ) -> Result {
        self.release_witnessed_inner(recovery_state, false, &mut complete_qos, defer)
    }

    pub(crate) fn prepare_replays(
        &mut self,
        recovery_state: u32,
        complete_qos: impl FnMut(qos::Owner) -> Result,
        defer: &mut impl FnMut(Deferred) -> Result,
    ) -> Result {
        self.release_witnessed_inner(recovery_state, true, complete_qos, defer)
    }

    fn release_witnessed_inner(
        &mut self,
        recovery_state: u32,
        replay: bool,
        mut complete_qos: impl FnMut(qos::Owner) -> Result,
        defer: &mut impl FnMut(Deferred) -> Result,
    ) -> Result {
        if !self.awaiting_witness || recovery_state != 0 {
            return Ok(());
        }
        if !self.retirement_proved {
            for active in self.active.iter() {
                self.valid_ticket(active, &active.ticket.ok_or(EIO)?)?;
            }
            let newest = self.active.back().and_then(|a| a.ticket).ok_or(EIO)?;
            if !retirement::quarantined_retired(
                &newest,
                self.graph.cursors()?,
                self.graph.stamp()?,
                self.active.len as u32,
                self.submitted,
            )? {
                return Ok(());
            }
            // A render consumer of this work (firmware dependency) is checked
            // by the replay pass: it republishes only while every render of
            // the owner is parked or idle and none executed since recovery.
            let unexecuted = if self.graph.m5 {
                // M5 command words stay zero even for executed commands. The
                // front needs its exact zero-duration ring record; a later
                // command, chained to its predecessor by the kick parent, must
                // not show a record with a real duration.
                self.active.iter().enumerate().all(|(index, active)| {
                    !active.packet.completion.signalled()
                        && match active.observed {
                            // Only reached for spared work quarantined after a
                            // recovery: every retained command crossed it.
                            Some(record) => {
                                record.timestamps[0] != 0 && retirement::unexecuted(record, true)
                            }
                            None => index != 0,
                        }
                })
            } else {
                self.active.iter().all(|active| {
                    active
                        .packet
                        .completion
                        .replay_witness_words()
                        .is_some_and(|words| words[0] == words[1])
                })
            };
            let owner_match = self.owner == Some(newest.owner);
            let candidate = self.spared_deferred
                && self.spared_quarantine
                && !self.released
                && owner_match
                && unexecuted;
            if self.spared_deferred && !candidate {
                pr_info!(
                    "G17 compute qid {} spared work not replayable (spared_quarantine={} released={} owner_match={} unexecuted={})\n",
                    self.qid(),
                    self.spared_quarantine,
                    self.released,
                    owner_match,
                    unexecuted
                );
            }
            if candidate && !replay {
                return Ok(());
            }
            self.replay_retirement = candidate;
            self.retirement_proved = true;
        }
        while let Some(front) = self.active.front() {
            if front.qos_pending {
                complete_qos(self.qos_owner())?;
                self.active.iter_mut().next().ok_or(EIO)?.qos_pending = false;
            }
            let front = self.active.front().ok_or(EIO)?;
            let completion = front.packet.completion.clone();
            if self.replay_retirement {
                self.replays.push(front.packet.clone()).map_err(|_| EIO)?;
            } else if self.spared_deferred {
                defer(Deferred::Retired(completion, Err(ENODATA)))?;
            } else {
                defer(Deferred::Release(completion))?;
            }
            let mut active = self.active.pop().ok_or(EIO)?;
            active.activity.end();
            if !self.replay_retirement {
                active.packet.unpin_retired();
            }
        }
        self.awaiting_witness = false;
        self.retirement_proved = false;
        self.replay_retirement = false;
        self.spared_deferred = false;
        if self.quarantined && !self.spared_quarantine
            && *crate::module_parameters::sptm_space_trace.value() != 0
        {
            pr_info!("G17 compute qid {} stays quarantined after release (owner={:?} context={:?} error={:?})\n",
                self.qid(), self.owner, self.context().map(|c| c.id()), self.quarantine_error);
        }
        if self.spared_quarantine {
            self.spared_quarantine = false;
            self.quarantine_error = None;
            self.submitted = 0;
            self.kick.clear_parent_after_recovery();
            self.quarantined = false;
            self.retire_pending = false;
            self.retirement_ready = true;
            if self
                .previous
                .as_ref()
                .is_some_and(|p| p.publication.is_some())
            {
                drop(self.previous.take());
            }
            if self.released {
                self.owner = None;
            }
        }
        Ok(())
    }

    /// Both FIFOs are bounded by IN_FLIGHT. Count all retained commands of
    /// queues which this pass can release, replay or quarantine on failure.
    pub(crate) fn replay_commands(&self) -> usize {
        if self.awaiting_witness || self.replays.len != 0 {
            self.active.len + self.replays.len
        } else {
            0
        }
    }

    pub(crate) fn replay_front(&self) -> Option<Arc<Packet>> {
        self.replays.front().cloned()
    }

    /// Drop the witnessed owner only after either publication retained it or
    /// the off-lock completion collector accepted its terminal result.
    pub(crate) fn finish_replay(&mut self, packet: &Arc<Packet>) -> Result {
        if !self
            .replays
            .front()
            .is_some_and(|front| Arc::ptr_eq(front, packet))
        {
            return Err(EIO);
        }
        drop(self.replays.pop());
        self.replay_prepared = false;
        if self.replays.len == 0 {
            self.replay_error = None;
        }
        Ok(())
    }

    pub(crate) fn prepare_replay(&mut self, packet: &Arc<Packet>) -> Result {
        if !self
            .replays
            .front()
            .is_some_and(|front| Arc::ptr_eq(front, packet))
        {
            return Err(EIO);
        }
        if let Some(error) = self.replay_error {
            return Err(error);
        }
        if !self.replay_prepared {
            if !packet.completion.begin_replay() {
                return Err(EAGAIN);
            }
            packet.completion.reset_for_replay()?;
            self.replay_prepared = true;
        }
        Ok(())
    }

    pub(crate) fn fail_replays(&mut self, error: Error) {
        if self.replay_error.is_none() {
            self.replay_error = Some(error);
        }
    }

    /// Caller already stopped both processors. No firmware owner can access retained mappings.
    pub(crate) fn stopped(&mut self, defer: &mut impl FnMut(Deferred) -> Result) -> Result {
        if let Some(configuration) = self.configuration.take() { configuration.release(); }
        while let Some(packet) = self.replays.front() {
            defer(Deferred::Retired(packet.completion.clone(), Err(EIO)))?;
            let packet = self.replays.pop().ok_or(EIO)?;
            packet.unpin_retired();
        }
        while let Some(active) = self.active.front() {
            defer(Deferred::Retired(
                active.packet.completion.clone(),
                Err(EIO),
            ))?;
            let mut active = self.active.pop().ok_or(EIO)?;
            active.activity.end();
            active.packet.unpin_retired();
        }
        Ok(())
    }
}

impl Queue {
    pub(crate) fn matches(&self, owner: u64, qid: u8, context: &Arc<Context>) -> Result<bool> {
        if self.owner != Some(owner) {
            return Ok(false);
        }
        if self.released
            || self.quarantined
            || self.qid() != qid
            || !self
                .binding
                .as_ref()
                .is_some_and(|binding| Arc::ptr_eq(&binding.context, context))
            || !context.is_current()
        {
            return Err(EIO);
        }
        Ok(true)
    }
    pub(crate) fn reusable(&self) -> bool {
        self.installed
            && !self.source_held()
            && self.configuration.is_none()
            // A pool release in flight must be witnessed before any successor.
            && !matches!(self.pool_phase, PoolPhase::ReleaseSent(_))
            // A recycled killed queue is offered only once its backing is vacant (re-arm path).
            && (!self.needs_fresh_backing || self.pool_phase == PoolPhase::Vacant)
            // An issued kill still names this graph even if its commands retire
            // before the acknowledgement. It cannot acquire a successor owner.
            && self.exit.is_none()
            && self.owner.is_none()
            && self.released
            && self.retirement_ready
            && !self.quarantined
            && self.active.len == 0
            && self.replays.len == 0
            && self.previous.is_none()
    }
    pub(crate) fn bind_owner(&mut self, owner: u64, context: Arc<Context>) -> Result {
        if !self.reusable() {
            return Err(EBUSY);
        }
        if !context.is_current() {
            return Err(EFAULT);
        }
        let old = self.binding.as_ref().map(|binding| binding.context.clone());
        let same_vm = old
            .as_ref()
            .is_some_and(|old| Arc::ptr_eq(old.status(), context.status()) && old.is_current());
        if same_vm {
            let binding = self.binding.as_mut().ok_or(EIO)?;
            if binding.pool_alias.is_none() {
                // The same VM takes back a re-armed pool: alias the new backing.
                binding.pool_alias = Some(self.pool.map_client(context.vm())?);
            }
            if binding.descriptors.is_none() {
                binding.descriptors = Some(self.graph.descriptor_alias(context.vm())?);
            }
            self.graph.set_owner(&context)?;
            binding.context = context.clone();
            self.previous = Some(Previous {
                binding: None,
                context: old.ok_or(EIO)?,
                publication: None,
            });
        } else {
            let kick_alias = self.kick.map_client(context.vm())?;
            let pool_alias = self.pool.map_client(context.vm())?;
            let binding = self.graph.bind(context.clone(), kick_alias, pool_alias)?;
            self.graph.set_owner(&context)?;
            let old_binding = self.binding.replace(binding);
            // A detached queue (exited owner already dropped) has nothing to supersede;
            // the new owner's record fields replace the retained pages' references.
            self.previous = old.map(|old| Previous {
                binding: old_binding,
                context: old,
                publication: None,
            });
            self.retained_owner = None;
        }
        context.mark_published();
        self.owner = Some(owner);
        self.entry_traces = 0;
        self.released = false;
        self.retirement_ready = false;
        self.retired_by_teardown = false;
        Ok(())
    }
    /// True allows the registry to cancel the never-published QID and drop this graph. An
    /// installed graph remains retained even after the logical owner exits without work.
    pub(crate) fn release_owner(&mut self) -> Result<bool> {
        if !self.installed && self.active.len == 0 && !self.quarantined && self.previous.is_none() {
            return Ok(true);
        }
        self.released = true;
        if self.active.len == 0 && !self.quarantined && self.configuration.is_none() {
            if let Some(previous) = self.previous.as_ref().filter(|p| p.publication.is_none()) {
                if let Err(error) = self.graph.set_owner(&previous.context) {
                    self.quarantined = true;
                    self.retired_by_teardown = true;
                    return Err(error);
                }
                let mut previous = self.previous.take().ok_or(EIO)?;
                if let Some(binding) = previous.binding.take() {
                    self.binding = Some(binding);
                } else {
                    self.binding.as_mut().ok_or(EIO)?.context = previous.context;
                }
                self.retirement_ready = true;
            }
            if self.retirement_ready {
                self.owner = None;
                self.released_at = now_ns();
            }
        }
        Ok(false)
    }
    /// The last owner's file and VM are gone: its execution root was released
    /// after the scheduler-state release and command retirement were witnessed
    /// (`Context::release_execution`), and this queue is idle without an owner.
    /// Drop the binding (every alias into that VM) and the context, keeping only
    /// the scheduler page and work storage the installed queue record names.
    /// The returned binding (aliases into the exited VM and the last reference
    /// to its context) is dropped by the caller after the device mutex is
    /// released: unmapping and VM teardown must not delay other clients.
    pub(crate) fn detach_exited_owner(&mut self) -> Option<Binding> {
        if !self.reusable() {
            return None;
        }
        if self.binding.as_ref()?.context.is_current() {
            return None;
        }
        let binding = self.binding.take()?;
        self.retained_owner = Some(binding.context.retain_firmware_pages());
        Some(binding)
    }
    /// An idle retained queue whose USC backing may be given back.
    pub(crate) fn vacate_candidate(&self) -> bool {
        (self.reusable() || self.recycled_pending()) && self.pool_phase == PoolPhase::Populated && self.pool.populated()
    }
    /// A recycled killed queue still holding the backing its killed work used.
    pub(crate) fn recycled_pending(&self) -> bool {
        self.needs_fresh_backing
            && self.installed
            && self.exit.is_none()
            && self.owner.is_none()
            && !self.quarantined
            && self.active.len == 0
            && self.replays.len == 0
            && self.previous.is_none()
    }
    /// Recycled queues are vacated regardless of the warm cap.
    pub(crate) fn needs_fresh_backing(&self) -> bool {
        self.needs_fresh_backing
    }
    /// The firmware acknowledged the kill of this queue's closed owner (`ContextKilled`)
    /// and every killed command was settled: return the QID to service. The item
    /// ring is reset to empty at the firmware's consumer cursor; the USC pool and
    /// descriptor ring are vacated and re-armed fresh before the next owner binds.
    pub(crate) fn recycle_killed(&mut self) -> Result<bool> {
        if self.source_held() || self.configuration.is_some() || !self.installed
            || !self.exit.as_ref().is_some_and(|exit| exit.released())
            || self.active.len != 0
            || self.replays.len != 0
            || self.previous.is_some()
        {
            return Ok(false);
        }
        self.recycle_retired()?;
        Ok(true)
    }

    /// A failed (non-spared) quarantine has no other exit: `release_witnessed`
    /// returns its commands' pins but only a spared quarantine is cleared, and
    /// a kill is only issued while commands are queued. Without this the queue
    /// is never reusable, `detach_exited_owner` never drops its binding, and
    /// the binding's aliases (each an `ARef<GpuVm<VmInner>>`) and context keep
    /// the exited client's VM -- and its SPTM private address space -- alive
    /// forever (one leaked VM and compute QID per faulted client).
    ///
    /// Recycle it exactly like an acknowledged kill once the same ownership
    /// facts hold: every command was retired by the exact quarantine witness
    /// (`quarantined_retired`, which also required recovery state 0), nothing is
    /// queued or replayed, the logical owner closed, and the scheduler teardown
    /// already released the owner's execution root (context no longer current),
    /// so no firmware owner can reach this graph's work or the exited VM. The
    /// item ring restarts at the firmware consumer and the USC pool/ring are
    /// vacated and re-armed before any successor binds.
    pub(crate) fn recycle_failed(&mut self, recovery_state: u32) -> Result<bool> {
        if self.source_held() || self.configuration.is_some() || recovery_state != 0
            || !self.installed
            || !self.quarantined
            || self.spared_quarantine
            || self.quarantine_error.is_none()
            || self.retired_by_teardown
            || self.awaiting_witness
            || self.exit.is_some()
            || !self.released
            || self.active.len != 0
            || self.replays.len != 0
            || self.previous.is_some()
            || self.binding.as_ref().map_or(true, |binding| binding.context.is_current())
        {
            return Ok(false);
        }
        if *crate::module_parameters::sptm_space_trace.value() != 0 {
            pr_info!("G17 compute qid {} recycled after failed quarantine (error={:?} context={:?})\n",
                self.qid(), self.quarantine_error, self.context().map(|c| c.id()));
        }
        self.recycle_retired()?;
        Ok(true)
    }

    fn recycle_retired(&mut self) -> Result {
        self.graph.reset_items()?;
        self.exit = None;
        self.quarantined = false;
        self.quarantine_error = None;
        self.failure_status_pending = false;
        self.retired_by_teardown = false;
        self.spared_quarantine = false;
        self.spared_deferred = false;
        self.retire_pending = false;
        self.awaiting_witness = false;
        self.retirement_proved = false;
        self.replay_retirement = false;
        self.replay_error = None;
        self.replay_prepared = false;
        self.submitted = 0;
        self.kick.clear_parent_after_recovery();
        self.owner = None;
        self.released = true;
        self.retirement_ready = true;
        self.released_at = now_ns();
        self.needs_fresh_backing = true;
        Ok(())
    }
    pub(crate) fn released_at(&self) -> u64 {
        self.released_at
    }
    /// Owned by a live logical queue, yet fully quiescent: every published
    /// command retired and witnessed, nothing replayed or retained from a
    /// previous owner, no exit, growth, pool release or quarantine in flight.
    /// Such a queue can be handed to another owner exactly as if its owner had
    /// closed (release_owner + the normal bind_owner rebinding).
    pub(crate) fn idle_owned(&self) -> bool {
        self.installed
            && !self.source_held()
            && self.configuration.is_none()
            && self.owner.is_some()
            && !self.released
            && self.retirement_ready
            && self.exit.is_none()
            && !self.quarantined
            && !self.awaiting_witness
            && !self.retire_pending
            && !self.growing
            && !self.needs_fresh_backing
            && self.pool_phase == PoolPhase::Populated
            && self.active.len == 0
            && self.submitted == 0
            && self.replays.len == 0
            && self.previous.is_none()
    }
    pub(crate) fn pool_vacant(&self) -> bool {
        self.pool_phase == PoolPhase::Vacant
    }
    /// Publishes the release of this idle queue's free list. The caller holds the
    /// device mutex and rings the control doorbell afterwards.
    pub(crate) fn vacate(&mut self, init: &crate::g17::initdata::InitData) -> Result {
        if !self.vacate_candidate() {
            return Err(EBUSY);
        }
        let record = ControlRecord::FreeListRelease(
            self.pool.release_record(self.free_list_generation),
        );
        let cursor = Rings::publish_control(init, &record)?;
        self.pool_phase = PoolPhase::ReleaseSent(cursor);
        fence(Ordering::SeqCst);
        Ok(())
    }
    /// Detaches the USC backing once the firmware's control consumer passed the
    /// release. The caller frees the returned backing after releasing the
    /// device mutex.
    pub(crate) fn observe_vacancy(
        &mut self,
        consumer: u32,
        producer: u32,
    ) -> Result<Option<Vacated>> {
        if self.growing { return Ok(None); }
        let PoolPhase::ReleaseSent(cursor) = self.pool_phase else {
            return Ok(None);
        };
        if !crate::g17::freelist::control_consumed(consumer, producer, cursor)? {
            return Ok(None);
        }
        // The retained owner binding's client alias pins the backing: detach it
        // first (the owner has exited; `reusable()` proved no previous binding).
        let alias = self.binding.as_mut().and_then(|binding| binding.pool_alias.take());
        let pages = self.pool.take_pages();
        let descriptors = self.binding.as_mut().and_then(|binding| binding.descriptors.take());
        let ring = self.graph.vacate_ring();
        self.pool_phase = PoolPhase::Vacant;
        Ok(Some(Vacated {
            _alias: alias,
            _pages: pages,
            _descriptors: descriptors,
            _ring: ring,
        }))
    }
    /// Installs fresh backing into a vacant pool; returns the descriptor row data
    /// the caller writes before binding an owner.
    pub(crate) fn rearm(
        &mut self,
        pages: crate::g17::freelist::Pages,
        ring: DescriptorRing,
    ) -> Result<crate::g17::freelist::PoolDescriptor> {
        if self.pool_phase != PoolPhase::Vacant || !(self.reusable() || self.recycled_pending()) {
            return Err(EAGAIN);
        }
        self.graph.rearm_ring(ring)?;
        self.pool.rearm(pages)?;
        self.pool_phase = PoolPhase::Populated;
        self.needs_fresh_backing = false;
        self.pool.descriptor()
    }
    pub(crate) fn pool_fits(&self, required: Option<crate::g17::fw::m5::pool::PoolGeometry>) -> bool {
        !self.growing && self.pool_phase == PoolPhase::Populated && self.pool.fits(required)
    }

    pub(crate) fn growth_started(&self) -> bool { self.growing }

    /// Only a capacity increase waits for this queue's previously published work.
    /// Other queues and ordinary submissions keep their existing concurrency.
    pub(crate) fn begin_growth(&mut self, init: &crate::g17::initdata::InitData) -> Result<bool> {
        if self.growing { return Ok(false); }
        if !self.graph.m5 || self.quarantined || self.released || self.exit.is_some()
            || self.active.len != 0 || self.replays.len != 0 || self.previous.is_some()
            || (self.installed && !self.retirement_ready)
            || self.pool_phase != PoolPhase::Populated { return Err(EAGAIN); }
        if self.installed {
            let record = ControlRecord::FreeListRelease(self.pool.release_record(self.free_list_generation));
            let cursor = Rings::publish_control(init, &record)?;
            self.pool_phase = PoolPhase::ReleaseSent(cursor);
        } else { self.pool_phase = PoolPhase::Vacant; }
        self.growing = true;
        Ok(self.installed)
    }

    pub(crate) fn finish_growth(&mut self, init: &crate::g17::initdata::InitData,
        pages: &mut Option<crate::g17::freelist::Pages>,
        alias: &mut Option<mmu::KernelMapping>) -> Result<Vacated> {
        if !self.growing { return Err(EINVAL); }
        if let PoolPhase::ReleaseSent(cursor) = self.pool_phase {
            let consumer = init.control_consumer()?.load(Ordering::Acquire);
            let producer = init.control_producer()?.load(Ordering::Acquire);
            if !crate::g17::freelist::control_consumed(consumer, producer, cursor)? {
                return Err(EAGAIN);
            }
        }
        let binding = self.binding.as_mut().ok_or(EFAULT)?;
        let old_alias = binding.pool_alias.take();
        let old_pages = self.pool.take_pages();
        self.pool.rearm(pages.take().ok_or(EINVAL)?)?;
        init.install_compute_pool(self.pool.descriptor()?)?;
        binding.pool_alias = alias.take();
        self.pool_phase = PoolPhase::Populated;
        self.growing = false;
        Ok(Vacated { _alias: old_alias, _pages: old_pages, _descriptors: None, _ring: None })
    }

    /// Exact host identity remains valid even when several commands share an ioctl ID.
    pub(crate) fn owns_packet(&self, packet: &Arc<Packet>) -> bool {
        self.active
            .iter()
            .any(|active| Arc::ptr_eq(&active.packet, packet))
    }

    /// A watchdog settles spared, deferred waiters without changing their VM status.
    /// Fully ticketed spared work can still return this queue after its drain witness.
    pub(crate) fn timeout(
        &mut self,
        defer: &mut impl FnMut(Deferred) -> Result,
    ) -> Result<Option<Arc<VmStatus>>> {
        let fresh = !self.quarantined;
        let return_spared = self.active.len != 0
            && self
                .active
                .iter()
                .all(|active| active.packet.completion.spared() && active.ticket.is_some());
        if self.spared_deferred {
            for active in self.active.iter_mut() {
                if !active.failure_deferred {
                    defer(Deferred::FailedOwned(
                        active.packet.completion.clone(),
                        ETIMEDOUT,
                    ))?;
                    active.failure_deferred = true;
                }
            }
            self.spared_deferred = false;
        }
        let result = self.quarantine(ETIMEDOUT, defer);
        if fresh {
            self.spared_quarantine = return_spared;
        }
        result
    }

    pub(crate) fn cancel(
        &mut self,
        packet: &Arc<Packet>,
        defer: &mut impl FnMut(Deferred) -> Result,
    ) -> Result<bool> {
        if !self.owns_packet(packet) {
            return Ok(false);
        }
        if self.quarantined {
            if self.spared_deferred {
                for active in self.active.iter_mut() {
                    if !active.failure_deferred {
                        defer(Deferred::FailedOwned(
                            active.packet.completion.clone(),
                            ECANCELED,
                        ))?;
                        active.failure_deferred = true;
                    }
                }
                self.spared_deferred = false;
            }
            return Ok(true);
        }
        self.released = true;
        self.retired_by_teardown = true;
        if let Some(active) = self
            .active
            .iter_mut()
            .find(|a| Arc::ptr_eq(&a.packet, packet))
        {
            if !active.failure_deferred {
                defer(Deferred::FailedOwned(
                    active.packet.completion.clone(),
                    ECANCELED,
                ))?;
                active.failure_deferred = true;
            }
        }
        Ok(true)
    }
    /// Firmware is halted and publication is closed while its target is copied.
    pub(crate) fn publish_recovery_target(&self) -> Result {
        let producer = self
            .graph
            .pointer_word(PointerBlock::PRODUCER)?
            .load(Ordering::Relaxed);
        self.graph
            .pointer_word(PointerBlock::RECOVERY_TARGET)?
            .store(producer, Ordering::Relaxed);
        Ok(())
    }

    /// The newest covered kick of a QID subsumes its earlier prefix; later independent work
    /// must not become a dependency. Errors are observed before returning any firmware token.
    pub(crate) fn dependency(
        &self,
        owner: u64,
        status: &Arc<VmStatus>,
        through: Option<u64>,
        recovery_state: u32,
    ) -> Result<Option<crate::g17::fw::kick::KickDependency>> {
        if through.is_none() || self.owner != Some(owner) {
            return Ok(None);
        }
        let Some(active) = self
            .active
            .iter()
            .filter(|a| crate::g17::job::Order::contains(through, a.packet.order.sequence))
            .last()
        else {
            return Ok(None);
        };
        if self.quarantined && self.spared_quarantine {
            return Err(ECANCELED);
        }
        if !Arc::ptr_eq(active.packet.completion.status(), status)
            || self.quarantined
            || recovery_state != 0
        {
            return Err(EIO);
        }
        let ticket = active.ticket.ok_or(EIO)?;
        let state = crate::g17::job::fence_status(&active.packet.completion.fence());
        if state < 0 {
            return Err(crate::g17::dependency::inherited_error(Error::from_errno(
                state,
            )));
        }
        if state > 0 {
            return Ok(None);
        }
        active.packet.completion.note_firmware_consumer();
        let kick = crate::g17::fw::kick::KickTimestamp::new(ticket.kick).ok_or(EIO)?;
        Ok(Some(
            crate::g17::fw::kick::KickDependency::new(self.qid(), kick).ok_or(EIO)?,
        ))
    }
}

impl Graph {
    fn record<T: Copy>(
        &mut self,
        slot: Option<usize>,
        offset: usize,
        window: usize,
        value: T,
    ) -> Result<u64> {
        let (object, start): (&mut KernelObject, usize) = if let Some(slot) = slot {
            if slot >= RECORD_SLOTS {
                return Err(EINVAL);
            }
            (&mut self.records, slot * RECORD_STRIDE + offset)
        } else {
            (&mut *self.graph, 0x6800 + offset)
        };
        if core::mem::size_of::<T>() > window {
            return Err(EINVAL);
        }
        let pointer = object.pointer(start, window)?;
        // SAFETY: The checked record window is not named by any unconsumed item pointer. The
        // device mutex excludes other host publishers, and no Rust reference to it is formed.
        unsafe { pointer.write_bytes(0, window) };
        object.write(start, value)?;
        fence(Ordering::SeqCst);
        Ok(object.gpu_va() + start as u64)
    }
    fn append(&self, expected: u32, address: u64, before_publish: impl FnOnce()) -> Result<u32> {
        let cursors = self.cursors()?;
        if cursors.producer != expected
            || cursors.count != ITEM_RING_ENTRIES
            || expected >= ITEM_RING_ENTRIES
        {
            return Err(EBUSY);
        }
        let next = (expected + 1) % ITEM_RING_ENTRIES;
        if next == cursors.consumer {
            return Err(EBUSY);
        }
        let slot = self.graph.dword(ITEMS + expected as usize * 8)?;
        let producer = self.pointer_word(0x40)?;
        if slot.load(Ordering::Relaxed) != 0 {
            return Err(EBUSY);
        }
        before_publish();
        slot.store(address, Ordering::Relaxed);
        fence(Ordering::SeqCst);
        producer.store(next, Ordering::Relaxed);
        Ok(next)
    }
}

/// Borrowed device operations used during one publication. Implementations borrow the initdata,
/// coprocessor and accounting fields separately from the Registry that owns physical queues.
pub(crate) trait Host {
    fn next_compute_publication(&mut self) -> Result<u64>;
    fn epoch(&self) -> Result<(u64, u32)>;
    fn prepare_compute_shared(&mut self) -> Result;
    fn qos_publish(
        &mut self,
        owner: qos::Owner,
        scheduler: u64,
        policy: crate::g17::fw::queue::Policy,
    ) -> Result<qos::Publication>;
    fn qos_cancel(&mut self, publication: qos::Publication) -> Result;
    fn publish_qid(&mut self, id: kick::Id) -> Result;
    fn publish_outer(&mut self, priority: u8, slot: &crate::g17::fw::channels::WorkSlot)
        -> Result;
    fn notify(&mut self, message: u64) -> Result;
    fn note_submission(&mut self) -> Result;
    fn submit_m5(&mut self, submission: &crate::g17::fw::m5::queue::HardwareSubmission) -> Result;

}

impl Queue {
    /// The front command's completion record shows it never executed: firmware
    /// wrote equal, nonzero start and end words (a zero-duration record).
    /// On M5 the per-command words stay zero even for executed commands, so the
    /// witness is the command's exact completion-ring record; None until it is
    /// latched.
    pub(crate) fn front_unexecuted(&self) -> Option<bool> {
        let front = self.active.front()?;
        if self.graph.m5 {
            let record = front.observed?;
            // Recovery only: the front was in flight across it.
            return Some(record.timestamps[0] != 0 && retirement::unexecuted(record, true));
        }
        let words = front.packet.completion.compute_words().ok()?;
        Some(words[0] != 0 && words[0] == words[1])
    }

    /// The front command was already republished after an earlier recovery.
    pub(crate) fn front_replayed(&self) -> bool {
        self.active
            .front()
            .is_some_and(|front| front.packet.completion.replay_count() != 0)
    }

    /// Whether the front command's exact completion record is latched; None
    /// without an in-flight ticketed front.
    pub(crate) fn front_observed(&self) -> Option<bool> {
        let front = self.active.front()?;
        front.ticket?;
        Some(front.observed.is_some())
    }

    /// One diagnostic line per in-flight compute front when firmware first
    /// announces a recovery, before the host acknowledges it: whether firmware
    /// had consumed the front's ring items (`consumer` vs `end`) before any
    /// cancellation.
    pub(crate) fn describe_recovery_entry(&self) {
        let Some(front) = self.active.front() else {
            return;
        };
        pr_info!(
            "G17 recovery entry compute qid {} owner {:?} front kick {:?} end {:?} cursors {:?} stamp {:?} record {:?} active {}\n",
            self.qid(),
            self.owner,
            front.ticket.map(|t| t.kick),
            front.ticket.map(|t| t.item_producer),
            self.graph.cursors().map(|c| (c.producer, c.consumer, c.count)),
            self.graph.stamp(),
            front.observed.map(|o| o.timestamps),
            self.active.len
        );
    }

    /// Witnessed or replay-pending work of this queue that a later command
    /// waiting through `through` must follow.
    pub(crate) fn replay_pending_through(&self, through: Option<u64>) -> bool {
        use crate::g17::job::Order;
        (self.awaiting_witness || self.replays.len != 0)
            && (self.active.iter().any(|a| Order::contains(through, a.packet.order.sequence))
                || self.replays.iter().any(|p| Order::contains(through, p.order.sequence)))
    }

    pub(crate) fn spared_pending(&self) -> bool {
        self.quarantined && self.spared_quarantine && self.awaiting_witness
    }

    /// Retains the packet before any potentially visible store. An error before publication
    /// rolls back its work count; every ambiguous error quarantines the graph and packet.
    pub(crate) fn publish(
        &mut self,
        packet: Arc<Packet>,
        dependencies: &[crate::g17::fw::kick::KickDependency],
        host: &mut impl Host,
        defer: &mut impl FnMut(Deferred) -> Result,
    ) -> Result {
        use crate::g17::fw::{
            channels::WorkSlot,
            compute::{ComputeArgs, ComputeDescriptor, COMPUTE_KICK_EVENT_MASK, COMPUTE_QOS_CLASS},
            kick::{KickArgs, KickQos},
        };
        let (epoch, addresses) = self.publication_preflight(&packet, host)?;
        let crate::g17::command::Validated::Compute {
            matrix_ops: requested_matrix_ops,
            scratch,
            cdm_auxiliary_allocation,
            intersection,
            usc_base,
            cdm_va,
            cdm_end_va,
            sampler_heap_va,
            sampler_count,
            plan,
            ..
        } = &packet.command
        else {
            return Err(EINVAL);
        };
        let m5_scratch = if self.graph.m5 {
            crate::g17::fw::m5::compute::ScratchRequest::new(scratch.layout())?
        } else { crate::g17::fw::m5::compute::ScratchRequest::NONE };
        if !self.graph.m5 && (*requested_matrix_ops || *cdm_auxiliary_allocation != 0 || intersection.enabled()) { return Err(EINVAL); }
        if self.graph.m5 {
            if *sampler_count != 0 { return Err(crate::g17::unsupported(file!(), line!())); }
            if plan.is_some_and(|plan| !self.pool.fits(Some(plan.geometry))) { return Err(EINVAL); }
            self.install_m5(&packet, host)?;
        }
        let work = packet.completion.work_state()?;
        let work_word = work.word();
        if work_word == u32::MAX {
            return Err(EINVAL);
        }
        let work_node = work.node_va()?;
        let kick_timestamp = self.kick.timestamp();
        let kick_parent = self.kick.parent();
        let qid = self.qid();
        let ordinal = self.ordinal.checked_add(1).ok_or(EOVERFLOW)?;
        let (descriptor_high, descriptor_low) =
            self.graph.descriptor_vas(kick_timestamp.slot() as u8)?;
        let first = self.ordinal == 0 && (self.graph.m5 || !self.installed);
        let entry_signal = first
            || dependencies
                .iter()
                .any(|dependency| dependency.qid() != qid);
        let record_slot = (self.graph.m5 || !first).then_some(self.ordinal as usize % RECORD_SLOTS);
        if let Some(slot) = record_slot {
            self.graph.ensure_record_slot(slot)?;
        }
        let timestamp_va = if self.graph.m5 {
            packet.completion.bind_m5_compute_timestamps(
                self.graph.m5_timestamps.as_ref().ok_or(EINVAL)?,
                record_slot.ok_or(EINVAL)? * 16)?
        } else { packet.completion.compute_timestamp_va()? };
        let args = ComputeArgs {
            scratch: *scratch,
            usc_base: *usc_base,
            cdm_va: *cdm_va,
            cdm_end_va: *cdm_end_va,
            sampler_heap_va: *sampler_heap_va,
            sampler_count: *sampler_count,
            descriptor_va: descriptor_low,
            context_id: packet.context.id().try_into()?,
            context_generation: packet.context.generation(),
            work_state: work_word,
            work_state_va: work_node,
            qid,
            kick_count: self.ordinal,
            kick: kick_timestamp,
            preempt_va: addresses.preempt,
            operand_state_va: addresses.operand,
            usage_va: addresses.usage,
            usage_fw_va: self.graph.usage.gpu_va(),
            free_list_slot: u32::from(self.pool.id()),
            free_list_control_va: self.pool.control_va(),
            stamp_va: self.graph.support.gpu_va() + STAMP as u64,
            aux_stamp_va: self.graph.support.gpu_va() + STAMP as u64 + 8,
            status_va: [
                self.graph.support.gpu_va() + 0x310,
                self.graph.support.gpu_va() + 0x318,
            ],
            timestamp_va,
        };
        let identity = ConfigIdentity {
            scheduler: packet.context.scheduler_va(),
            qos: packet.context.qos_id(),
            context: packet.context.id(),
        };
        self.free_list_generation = u64::from(packet.context.scheduler_generation());
        let policy = packet.context.policy();
        let activation = activation::Publication::new(
            self.outer_started, policy.priority(), self.graph.m5);
        let config = self.queue_config(&identity, first, activation.refresh,
            args.context_id, policy, &packet)?;
        let announce =
            KickAnnounce::new(qid, DataMaster::Compute, kick_timestamp, policy.priority())?;
        let predecessor = KickPredecessor::new(qid, kick_parent)?;
        let matrix_test = *crate::module_parameters::m5_matrix_submit_test.value();
        if matrix_test > 1 { return Err(EINVAL); }
        let matrix_ops = self.graph.m5 && (*requested_matrix_ops || matrix_test == 1);
        if matrix_ops && first {
            pr_info!("M5 native matrix submission controls enabled for compute context {}\n", args.context_id);
        }
        let m5_args = crate::g17::fw::m5::compute::DescriptorArgs {
            registers: crate::g17::fw::m5::compute::RegisterArgs {
                matrix_ops,
                scratch: m5_scratch,
                intersection: *intersection,
                auxiliary_allocation: *cdm_auxiliary_allocation,
                preempt_va: addresses.preempt, cdm_va: *cdm_va, operand_va: addresses.operand,
                usage_va: addresses.usage, free_list_slot: u32::from(self.pool.id()),
                context_id: args.context_id, work_state: work_word,
                object_ids: 0x0000_0001_0000_0001,
                resource_policy: plan.map_or(0xf5_0040_0020, |p| u64::from(p.pressure) << 32 | 0x0040_0020),
                preempt_state_offset: 0x1b80, operand_region_bytes: 0x1e000,
                operand_aux_offset: 0x3d800, resource_control: plan.map_or(0x54b, |p| p.resource_flags),
                dispatch_control: 0x14c08a60, auxiliary_control: 1,
            },
            descriptor_va: descriptor_low, submit_sequence: 0, work_state_va: work_node,
            cdm_end_va: *cdm_end_va, stamp_va: [args.stamp_va, args.aux_stamp_va],
            stamp_value: crate::g17::fw::stamp_value(kick_timestamp.get() as u32), qid: u32::from(qid),
            kick_timestamp: kick_timestamp.get(), completion_ordinal: 0,
            status_va: args.status_va, timestamp_va: [timestamp_va, timestamp_va + 8],
            shared_control_va: self.pool.control_va(), event_metadata: [0;16],
            usage_fw_va: self.graph.usage.gpu_va(), completion_identity: 1,
        };
        let m5_dependency_types = [2u8; crate::g17::fw::kick::QID_MAX as usize + 1];
        let kick_args = KickArgs {
            qid,
            timestamp: kick_timestamp,
            parent: kick_parent,
            barriers: dependencies,
            dependency_types: self.graph.m5.then_some(&m5_dependency_types[..dependencies.len()+1]),
            descriptor_va: descriptor_high,
            queue_va: self.graph.graph.gpu_va(),
            qos: KickQos {
                slot: identity.qos,
                class: COMPUTE_QOS_CLASS,
            },
            mcache: None,
            event_mask: if self.graph.m5 { [0,0,8,0] } else { COMPUTE_KICK_EVENT_MASK },
            register_arrays: if self.graph.m5 { crate::g17::fw::m5::compute::bindings(descriptor_low, m5_scratch, matrix_ops)? }
                else { ComputeDescriptor::register_bindings(descriptor_low, *scratch)? },
            compute_matrix: matrix_ops,
            compute_scratch: if self.graph.m5 { m5_scratch.enabled() } else { scratch.enabled() },
            priority: policy.priority(),
        };
        let published = host.next_compute_publication()?;
        self.active
            .push(Active {
                activity: crate::g17::context::ActivityMark::begin(&packet.context),
                packet: packet.clone(),
                published,
                work: work.clone(),
                ticket: None,
                observed: None,
                observed_at: None,
                failure_deferred: false,
                qos_pending: false,
            })
            .map_err(|_| EBUSY)?;
        self.retirement_ready = false;
        let mut visible = false;
        let mut accounted = false;
        let mut entry_trace = None;
        let result = (|| -> Result {
            work.add_submitted_kicks(1)?;
            accounted = true;
            let object = &mut self.graph.ring.as_mut().ok_or(EAGAIN)?.object;
            let offset = kick_timestamp.slot() % DESCRIPTORS * COMPUTE_DESCRIPTOR_SIZE;
            if self.graph.m5 {
                object.initialize::<crate::g17::fw::m5::compute::Descriptor>(offset,
                    |descriptor| descriptor.write_zeroed(&m5_args))?;
            } else {
                object.initialize::<ComputeDescriptor>(offset, |descriptor| descriptor.write(&args))?;
            }
            if !self.graph.m5 && first {
                self.graph.context_high.as_mut().ok_or(EIO)?.write(
                    0x200,
                    crate::g17::fw::compute::QueueContextItem::new(
                        descriptor_high,
                        self.graph.graph.gpu_va(),
                        qid,
                    ),
                )?;
            } else if !self.graph.m5 {
                // The retained context item remains at index one; only its descriptor changes.
                self.graph.context_high.as_mut().ok_or(EIO)?.write(0x210, descriptor_high)?;
                let cursors = self.graph.cursors()?;
                let _ = self.graph.recycle(cursors);
            }
            if self.graph.m5 { let cursors = self.graph.cursors()?; self.graph.recycle(cursors)?; }
            let config_va = self.graph.record(record_slot, CONFIG, 0xc0, config)?;
            let cursors = self.graph.cursors()?;
            if cursors.count != ITEM_RING_ENTRIES {
                return Err(EIO);
            }
            if cursors.producer >= cursors.count || cursors.consumer >= cursors.count {
                return Err(ERANGE);
            }
            let free = (cursors.consumer + cursors.count - cursors.producer - 1) % cursors.count;
            if free < if self.graph.m5 { 2 } else if entry_signal { 4 } else { 3 } {
                return Err(enospc_site!("queue/compute.rs:1906"));
            }
            let prepared_kick = self.kick.prepare_entry(&kick_args)?;
            let mut producer;
            if self.graph.m5 {
                let target = self.pool.reserve_submission()?;
                let qos = host.qos_publish(qos::Owner { qid, qos: identity.qos, data_master: 2 },
                    identity.scheduler, policy).or_else(|error| {
                        self.pool.cancel_unpublished(target)?; Err(error)
                    })?;
                self.active.back_mut().ok_or(EIO)?.qos_pending = true;
                producer = self.graph.append(cursors.producer, config_va, || packet.mark_published())
                    .or_else(|error| {
                        self.pool.cancel_unpublished(target)?;
                        host.qos_cancel(qos)?;
                        self.active.back_mut().ok_or(EIO)?.qos_pending = false;
                        Err(error)
                    })?;
                visible = true;
                // This Active and its Packet pins now own all visible records.
                if let Some(configuration) = self.configuration.take() { configuration.release(); }
            } else if first {
                producer = self.graph.first_descriptor(descriptor_high)?;
                host.prepare_compute_shared()?;
                packet.mark_published();
                visible = true;
                let qos = host.qos_publish(
                    qos::Owner {
                        qid,
                        qos: identity.qos,
                        data_master: 2,
                    },
                    identity.scheduler,
                    policy,
                )?;
                self.active.back_mut().ok_or(EIO)?.qos_pending = true;
                let target = self.pool.reserve_submission().or_else(|error| {
                    host.qos_cancel(qos)?;
                    self.active.back_mut().ok_or(EIO)?.qos_pending = false;
                    Err(error)
                })?;
                producer = self
                    .graph
                    .append(producer, config_va, || {})
                    .or_else(|error| {
                        self.pool.cancel_unpublished(target)?;
                        host.qos_cancel(qos)?;
                        self.active.back_mut().ok_or(EIO)?.qos_pending = false;
                        Err(error)
                    })?;
            } else {
                let target = self.pool.reserve_submission()?;
                producer = self
                    .graph
                    .append(cursors.producer, descriptor_high, || {
                        packet.mark_published()
                    })
                    .or_else(|error| {
                        self.pool.cancel_unpublished(target)?;
                        Err(error)
                    })?;
                visible = true;
                let qos = host.qos_publish(
                    qos::Owner {
                        qid,
                        qos: identity.qos,
                        data_master: 2,
                    },
                    identity.scheduler,
                    policy,
                )?;
                self.active.back_mut().ok_or(EIO)?.qos_pending = true;
                producer = self
                    .graph
                    .append(producer, config_va, || {})
                    .or_else(|error| {
                        host.qos_cancel(qos)?;
                        self.active.back_mut().ok_or(EIO)?.qos_pending = false;
                        Err(error)
                    })?;
            }
            if first {
                prepared_kick.prepare_install()?;
            }
            if self.graph.m5 && *crate::module_parameters::m5_cross_queue_fw_wait.value() & 4 != 0 {
                entry_trace = Some(prepared_kick.trace_dependencies());
            }
            prepared_kick.commit();
            self.submitted = self.submitted.checked_add(1).ok_or(EOVERFLOW)?;
            let announce_va = if self.graph.m5 {
                self.graph.record(record_slot, ANNOUNCE, 0x40,
                    crate::g17::fw::m5::queue::notification(qid, kick_timestamp)?)?
            } else { self.graph.record(record_slot, ANNOUNCE, 0x40, announce)? };
            producer = self.graph.append(producer, announce_va, || {})?;
            fence(Ordering::SeqCst);
            if entry_signal && !self.graph.m5 {
                let predecessor_va =
                    self.graph
                        .record(record_slot, PREDECESSOR, 0x400, predecessor)?;
                producer = self.graph.append(producer, predecessor_va, || {})?;
            }
            if let Some(slot) = record_slot {
                self.graph.record_ends[slot] = Some(producer);
            }
            host.publish_qid(self.kick.id())?;
            host.publish_outer(
                policy.priority(),
                &WorkSlot::new(
                    DataMaster::Compute,
                    self.graph.graph.gpu_va(),
                    qid,
                    producer.try_into()?,
                    activation.first,
                )?,
            )?;
            let notify_activation = activation.first;
            self.outer_started = activation.next_started;
            self.installed = true;
            self.published_config = Some(identity);
            self.ordinal = ordinal;
            let (accepted, state) = host.epoch()?;
            if state != 0 || accepted != epoch {
                return Err(EIO);
            }
            let ticket = Ticket {
                owner: self.owner.ok_or(EIO)?,
                submission: packet.order.sequence,
                context_id: packet.context.id(),
                qid,
                descriptor: descriptor_high,
                work_node,
                kick: kick_timestamp.get(),
                ordinal,
                item_producer: producer,
                item_count: ITEM_RING_ENTRIES,
                epoch,
            };
            self.active.back_mut().ok_or(EIO)?.ticket = Some(ticket);
            if let Some(previous) = self.previous.as_mut() {
                if previous.publication.is_none() {
                    previous.publication = Some((packet.order.sequence, epoch));
                }
            }
            let notify_every_kick = self.graph.m5
                && *crate::module_parameters::m5_kick_notify.value() == 0;
            if notify_activation || notify_every_kick {
                fence(Ordering::SeqCst);
                // Pipe 2 (compute) in bits 1:0, priority class in bits 3:2.
                host.notify((0x83 << 48) | 0x02 | u64::from(policy.priority()) << 2)?;
            }
            if self.graph.m5 {
                // The firmware work notification must precede a deferred host
                // doorbell: an idle-policy hold alone does not wake gated cores.
                host.submit_m5(&crate::g17::fw::m5::queue::HardwareSubmission::compute(
                    qid, self.kick.low_va(), policy.priority(),
                    first || notify_activation)?)?;
            }
            fence(Ordering::SeqCst);
            if !self.graph.m5 { host.notify((0x83 << 48) | 0x10)?; }
            host.note_submission()?;
            if let Some(source) = packet.completion.source() {
                source.published(crate::g17::sync_source::Token {
                    owner: self.owner.ok_or(EIO)?,
                    sequence: packet.order.sequence,
                    context: packet.context.id(),
                    qid,
                    kick: kick_timestamp,
                    epoch,
                    ordinal,
                })?;
                defer(Deferred::SourceReady(source))?;
            }
            Ok(())
        })();
        if let Err(error) = result {
            if !visible {
                if accounted {
                    if let Err(error) = work.cancel_unpublished_kicks(1) {
                        self.terminal =
                            self.quarantine(error, defer)?.map(|status| (status, error));
                        return Err(error);
                    }
                }
                drop(self.active.pop_back());
                return Err(error);
            }
            self.terminal = self.quarantine(error, defer)?.map(|status| (status, error));
            return Err(error);
        }
        if let Some((header, control, types, words)) = entry_trace {
            static ENTRY_TRACES: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);
            if self.entry_traces < 16
                || (self.entry_traces < 20 && kick_timestamp.get() & 0xff == 0)
            {
                self.entry_traces += 1;
                let index = ENTRY_TRACES.fetch_add(1, Ordering::Relaxed);
                pr_info!("M5 compute entry trace {} owner={:?} sequence={} context={} qid={} kick={:#x} epoch={} header={:#x} control={:#x} types={:#x} dependencies={:x?}\n",
                    index + 1, self.owner, packet.order.sequence, packet.context.id(),
                    qid, kick_timestamp.get(), epoch, header, control, types, words);
            }
        }
        Ok(())
    }

    /// The firmware consumes an empty channel installation before host-port
    /// configuration. EAGAIN leaves the packet unpublished and is retried by
    /// the common runtime with the device mutex released.
    fn retire_configuration(&mut self, epoch: u64) -> Result {
        let Some(configuration) = self.configuration.as_ref() else { return Ok(()); };
        if !configuration.consumed(self.graph.cursors()?, epoch) { return Ok(()); }
        self.configuration.take().ok_or(EIO)?.release();
        if self.ordinal == 0 && self.active.len == 0 && self.replays.len == 0 {
            self.retirement_ready = true;
            if self.released { self.owner = None; }
        }
        Ok(())
    }

    fn install_m5(&mut self, packet: &Arc<Packet>, host: &mut impl Host) -> Result {
        let (epoch, state) = host.epoch()?;
        if state == 0 { self.retire_configuration(epoch)?; }
        if let Some(end) = self.m5_install {
            return if self.graph.cursors()?.consumer == end || self.ordinal != 0 { Ok(()) }
                else { Err(EAGAIN) };
        }
        let context = &packet.context;
        let policy = context.policy();
        let config = QueueConfig::new(&QueueConfigArgs {
            target: QueueConfigTarget::Compute, kick_ring_va: self.kick.low_va(),
            kick_ring_fw_va: self.kick.firmware_va(), qid: self.qid(), install: true,
            completion_seed: None, context_id: context.id().try_into()?,
            free_list: FreeListBinding { va: 0, generation: u64::MAX, slot: u32::MAX },
            scheduler_va: context.scheduler_va(), policy, qos_slot: context.qos_id(),
            qos_update: true, owner_pid: context.owner_pid(), context_update: false,
        })?;
        if self.configuration.is_some() { return Err(EBUSY); }
        self.configuration = Some(configuration::Configuration::new(context, 1, epoch)?);
        let result = (|| {
            let va = self.graph.record(None, CONFIG, 0xc0, config)?;
            let configuration = self.configuration.as_mut().ok_or(EIO)?;
            let producer = self.graph.append(0, va, || configuration.mark_visible())?;
            self.m5_install = Some(producer);
            self.installed = true;
            host.publish_qid(self.kick.id())?;
            host.publish_outer(policy.priority(), &crate::g17::fw::channels::WorkSlot::new(DataMaster::Compute,
                self.graph.graph.gpu_va(), self.qid(), producer.try_into()?, true)?)?;
            self.outer_started = 1 << policy.priority();
            host.notify((0x83 << 48) | 0x02 | u64::from(policy.priority()) << 2)?;
            Err(EAGAIN)
        })();
        if result.is_err() && self.configuration.as_ref().is_some_and(|owner| !owner.visible()) {
            self.configuration.take().ok_or(EIO)?.release();
        }
        result
    }

    /// Everything that must hold before a publication changes any state: a healthy device
    /// epoch, room in the active ring (or this packet at the replay front), the bound
    /// owner's current context and a clean completion status. Returns the epoch and the
    /// bound owner's per-queue addresses.
    fn publication_preflight(
        &self,
        packet: &Arc<Packet>,
        host: &mut impl Host,
    ) -> Result<(u64, BindingAddresses)> {
        if self.growing || self.pool_phase != PoolPhase::Populated { return Err(EAGAIN); }
        // An outstanding consumer must not observe this physical queue's
        // 40-bit progress counter wrap past its retained dependency token.
        if self.source_held() && self.kick.timestamp().get() == 0 {
            return Err(EAGAIN);
        }
        let (epoch, state) = host.epoch()?;
        if state != 0 {
            return Err(EBUSY);
        }
        let replay_front = self
            .replays
            .front()
            .is_some_and(|front| Arc::ptr_eq(front, packet));
        if !self.room()
            && !(replay_front && !self.quarantined && !self.released && self.active.len < IN_FLIGHT)
        {
            return Err(EBUSY);
        }
        if !packet.context.is_current()
            || !self
                .binding
                .as_ref()
                .is_some_and(|binding| Arc::ptr_eq(&packet.context, &binding.context))
        {
            return Err(EFAULT);
        }
        let binding = self.binding.as_ref().ok_or(EFAULT)?;
        if packet.completion.status().get() != 0 {
            return Err(EIO);
        }
        Ok((
            epoch,
            BindingAddresses {
                preempt: binding.preempt.iova(),
                operand: binding.operand.iova(),
                usage: binding.usage.iova(),
            },
        ))
    }

    /// The queue configuration record for this publication: a fresh install on the first
    /// one, and QoS/context updates only when the identity changed since the last publication.
    fn queue_config(
        &self,
        identity: &ConfigIdentity,
        first: bool,
        priority_refresh: bool,
        context_id: u16,
        policy: crate::g17::fw::queue::Policy,
        packet: &Arc<Packet>,
    ) -> Result<QueueConfig> {
        QueueConfig::new(&QueueConfigArgs {
            target: QueueConfigTarget::Compute,
            kick_ring_va: self.kick.low_va(),
            kick_ring_fw_va: self.kick.firmware_va(),
            qid: self.qid(),
            // An existing M5 queue needs the firmware install/update path to
            // replace its private scheduling state when its class changes.
            // The ordinary binding fields below are still processed afterward;
            // no completion seed is supplied for a retained queue.
            install: (first && !self.graph.m5) || priority_refresh,
            completion_seed: (first && self.graph.m5).then_some(0),
            context_id,
            free_list: FreeListBinding {
                va: self.pool.control_va(),
                generation: u64::from(packet.context.scheduler_generation()),
                slot: u32::from(self.pool.id()),
            },
            scheduler_va: identity.scheduler,
            policy,
            qos_slot: identity.qos,
            qos_update: self
                .published_config
                .is_none_or(|p| p.scheduler != identity.scheduler || p.qos != identity.qos),
            owner_pid: packet.context.owner_pid(),
            context_update: self
                .published_config
                .is_none_or(|p| p.scheduler != identity.scheduler || p.context != identity.context),
        })
    }
}

impl Graph {
    /// The queue has never been installed. Retry staging replaces its initial three item slots;
    /// no firmware reader can name this graph until its first complete outer publication.
    fn first_descriptor(&self, address: u64) -> Result<u32> {
        let first = self.graph.dword(ITEMS)?;
        let second = self.graph.dword(ITEMS + 8)?;
        let third = self.graph.dword(ITEMS + 16)?;
        let producer = self.pointer_word(0x40)?;
        first.store(address, Ordering::Relaxed);
        second.store(0, Ordering::Relaxed);
        third.store(0, Ordering::Relaxed);
        producer.store(1, Ordering::Relaxed);
        Ok(1)
    }
}
impl Queue {
    pub(crate) fn take_terminal(&mut self) -> Option<(Arc<VmStatus>, Error)> {
        self.terminal.take()
    }
}
