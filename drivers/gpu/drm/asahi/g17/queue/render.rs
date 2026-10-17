// SPDX-License-Identifier: GPL-2.0-only AND MIT

//! Physical paired tiling and fragment queues and their retained command storage.

pub(in crate::g17) mod generation;
mod observation;
mod configuration;
pub(crate) use observation::Observed;
mod dependencies;
pub(in crate::g17) mod memory;
pub(crate) mod parked;
pub(in crate::g17) mod publication;
pub(crate) mod retirement;
mod warm;

use crate::g17::{
    buffer::{self, target},
    command::Validated,
    completion::Deferred,
    context::Context,
    freelist::RenderPool,
    fw::{
        channels::WorkSlot,
        kick::{KickDependency, KickTimestamp, McacheTable, RenderRegistration},
        queue::DataMaster,
        render as abi,
    },
    job::Packet,
    kick,
    object::{Allocator, Pool},
    qos, recovery,
    status::VmStatus,
};
use crate::{hw::t8140::queues, mmu};
pub(crate) use dependencies::Dependencies;
use kernel::{prelude::*, sync::Arc};
use retirement::{Ticket, Tracker};

/// Firmware dependencies of one render, kept by origin until the kick entries
/// are built: the covered earlier renders (their fragment completions) and the
/// covered compute completion. Which kick waits for the render prefix is the
/// command's choice (`RenderPass::fragment_barriers`); compute always orders
/// the tiling kick, since compute output may feed vertex work.
#[derive(Copy, Clone)]
pub(crate) struct RenderDependencies {
    pub(crate) render: [KickDependency; 2],
    pub(crate) render_count: usize,
    pub(crate) tiling: [KickDependency; 2],
    pub(crate) tiling_count: usize,
    pub(crate) compute: Option<KickDependency>,
}

impl RenderDependencies {
    pub(crate) const EMPTY: Self = Self {
        render: [KickDependency::ZERO; 2],
        render_count: 0,
        tiling: [KickDependency::ZERO; 2],
        tiling_count: 0,
        compute: None,
    };
    pub(crate) fn render(&self) -> &[KickDependency] {
        &self.render[..self.render_count]
    }
}

#[derive(Copy, Clone)]
pub(crate) struct RenderAddresses {
    /// Tiling then fragment; each pair contains the primary and auxiliary stamp.
    pub(crate) stamps: [[u64; 2]; 2],
}

#[derive(Copy, Clone)]
pub(crate) struct PublicationLease {
    pub(crate) free_list_generation: u64,
    pub(crate) buffer_token: u64,
}

/// Host services borrow the device-locked firmware state. Resource construction
/// and command geometry preparation do not borrow this interface.
pub(crate) trait Host {
    fn epoch(&self) -> Result<(u64, u32)>;
    /// The host's accepted recovery generation (no device access).
    fn generation(&self) -> u64;
    fn stamps(&self, qids: [u8; 2]) -> Result<[u32; 2]>;
    fn qos_publish(
        &mut self,
        owner: qos::Owner,
        scheduler: u64,
        policy: crate::g17::fw::queue::Policy,
    ) -> Result<qos::Publication>;
    fn qos_cancel(&mut self, publication: qos::Publication) -> Result;
    fn install_pair(&mut self, registration: &Registration) -> Result;
    fn publish_qid(&mut self, id: kick::Id) -> Result;
    fn publish_outer_pair(
        &mut self,
        priority: u8,
        fragment: &crate::g17::fw::channels::WorkSlot,
        tiling: &crate::g17::fw::channels::WorkSlot,
        commit: impl FnOnce() -> Result,
        late_tiling: impl FnOnce(),
    ) -> Result<[u32; 2]>;
    fn publish_configuration_pair(&mut self, priority: u8, fragment: &WorkSlot, tiling: &WorkSlot, prepare: impl FnOnce()) -> Result;
    fn activate_render_pool(&mut self, id: u16, generation: u64) -> Result;
    fn publish_retained_pair(
        &mut self,
        priority: u8,
        fragment: &WorkSlot,
        tiling: &WorkSlot,
        next: [u32; 2],
    ) -> Result;
    fn notify_render(&mut self, priority: u8) -> Result;
    fn note_submission(&mut self) -> Result;
    fn complete_render(
        &mut self,
        owners: Option<[qos::Owner; 2]>,
        pool_id: u16,
        generation: u64,
        buffer_id: u8,
    ) -> Result;
}

struct Prepared {
    packet: Arc<Packet>,
    ordinal: u64,
    mcache: Option<McacheTable>,
    register_arrays: [[Option<crate::g17::fw::kick::RegisterArrayBinding>; 4]; 2],
    kick_flags: crate::g17::fw::kick::StageKickFlags,
}
struct Active {
    /// The command is on this physical pair: its identity and QoS slot stay.
    activity: crate::g17::context::ActivityMark,
    qos_completed: bool,
    flist_completed: bool,
    outer_published: bool,
    generation: u64,
    packet: Arc<Packet>,
    ticket: Ticket,
    kicks: [KickTimestamp; 2],
    context_update: bool,
    failed: bool,
    drained_result: Option<Result<[u64; 4]>>,
    /// First observation of this group retired and unlinked without execution.
    abandoned_since: Option<u64>,
    /// The scheduler timeout of this group was deferred once to let the
    /// abandonment proof complete; a second timeout fails it as before.
    timeout_deferred: bool,
    /// Recovery generation when this group was published. Only a group that
    /// crossed a recovery is checked for abandonment on its pending polls.
    recovery_generation: u64,
}

/// A firmware-retired, unexecuted group must hold on two observations this far
/// apart before it is republished; a single sample may straddle an update.
const ABANDON_GRACE_NS: u64 = 2_000_000;

/// The firmware-specific host transaction; queue ownership remains shared.
pub(crate) enum Registration {
    Neo(RenderRegistration),
    M5(crate::g17::fw::m5::queue::HardwareSubmission),
}

/// vmfix6d: a retained graph is reinstallable when its pointer blocks are
/// drained (done == read == write) and no generation still owns an ordinal.
fn restorable_graph(binding: &RetiredBinding) -> Result<Option<[u32; 2]>> {
    let Some(memory) = binding.memory.as_ref() else {
        return Ok(None);
    };
    // vmfix6f: a graph whose pool mappings were moved out (same-VM context
    // change) cannot be reinstalled in place.
    if !memory.has_pool_mappings() {
        return Ok(None);
    }
    if memory.owners().iter().any(Option::is_some) {
        return Ok(None);
    }
    core::sync::atomic::fence(core::sync::atomic::Ordering::Acquire);
    let cursors = memory.graph.cursors()?;
    if cursors.iter().any(|c| c.done != c.read || c.read != c.write) {
        return Ok(None);
    }
    Ok(Some([cursors[0].write, cursors[1].write]))
}

/// vmfix6d A: a pair's admission-relevant state (see Pair::diag).
#[derive(Clone, Copy)]
pub(crate) struct PairDiag {
    pub(crate) owner: u64,
    pub(crate) context: u32,
    pub(crate) ordinal: u64,
    pub(crate) active: u32,
    pub(crate) prepared: bool,
    pub(crate) quarantined: bool,
    pub(crate) displaced: bool,
    /// Bit 0 present, 1 own memory, 2 published, 3 ready.
    pub(crate) previous: u32,
    pub(crate) context_update: bool,
    pub(crate) installed: [bool; 2],
    pub(crate) cursors: [u32; 2],
    /// done/read/write per stage (u32::MAX when unreadable).
    pub(crate) graph: [[u32; 3]; 2],
    /// Bit 0 pending aliases, bit 1 kick aliases.
    pub(crate) aliases: u32,
    pub(crate) registered: u32,
    pub(crate) qids: [u8; 2],
    pub(crate) buffer: u8,
    /// Target record and descriptor record claims at the next ordinal
    /// (u64::MAX = free); another ordinal there makes Pair::prepare EBUSY.
    pub(crate) claims: [u64; 2],
}
impl PairDiag {
    pub(crate) fn fingerprint(&self, mut hash: u64) -> u64 {
        use crate::g17::progress::mix;
        for word in [
            self.owner,
            u64::from(self.context),
            self.ordinal,
            u64::from(self.active),
            u64::from(self.prepared) | u64::from(self.quarantined) << 1
                | u64::from(self.displaced) << 2 | u64::from(self.context_update) << 3
                | u64::from(self.installed[0]) << 4 | u64::from(self.installed[1]) << 5,
            u64::from(self.previous),
            u64::from(self.cursors[0]) | u64::from(self.cursors[1]) << 32,
            u64::from(self.graph[0][0]) | u64::from(self.graph[0][1]) << 32,
            u64::from(self.graph[0][2]) | u64::from(self.graph[1][0]) << 32,
            u64::from(self.graph[1][1]) | u64::from(self.graph[1][2]) << 32,
            u64::from(self.aliases) | u64::from(self.registered) << 8,
            self.claims[0],
            self.claims[1],
        ] {
            hash = mix(hash, word);
        }
        hash
    }
}

/// Installed kick backing remains owned even when this pair has no logical
/// owner. Commands retain their exact descriptor and allocation generations.
pub(crate) struct Pair {
    kick_aliases: Option<[Option<mmu::KernelMapping>; 2]>,
    memory: KBox<memory::Memory>,
    kicks: [kick::Queue; 2],
    free_list: Arc<RenderPool>,
    pending_aliases: Option<KVec<mmu::KernelMapping>>,
    prepared: Option<Prepared>,
    active: Tracker<Active>,
    slot: u8,
    owner: Option<u64>,
    ordinal: u64,
    initial_packet: u64,
    clusters: u32,
    descriptor_flags: [u32; 2],
    installed: [bool; 2],
    configs: [Option<crate::g17::fw::queue::QueueConfig>; 2],
    cursors: [u32; 2],
    recovery_generation: u64,
    /// Priority class of the pair's installed registration, if any. A pair
    /// rebound to an owner of another class is registered again.
    registered: Option<u8>,
    /// Bit `p` set once a work slot of priority class `p` named the pair.
    outer_started: u8,
    quarantined: bool,
    /// Diagnostic (sptm_space_trace): drain inputs of this quarantine were logged.
    drain_logged: bool,
    terminal: Option<(Arc<VmStatus>, Error)>,
    previous: Option<RetiredBinding>,
    displaced: Option<RetiredBinding>,
    context_update: bool,
    /// vmfix6f: the queues' firmware identity (logical context: UAT context,
    /// scheduler page, owner) changed since the last configuration firmware
    /// consumed. Only this sets the configuration's context/QoS update bits.
    /// `context_update` stays the host witness that retires `previous` and
    /// gates the completion seed. Native M5 (147 captures, 1,944 queue
    /// configurations) sets both update bits only on a new owner's first
    /// configuration; its same-owner free-list (51) and PB (10) switches on
    /// live render queues carry them clear, with a completion seed. Cleared
    /// once a publication carried it.
    identity_update: bool,
    /// Hardware identity named by the configurations firmware last took.
    /// A VM's identity can move while its pairs are idle (no command pinned
    /// it); the next publication then carries the update bits.
    configuration: Option<configuration::Configuration>,
    configured_context: Option<u32>,
    /// QoS slot named by those configurations (bound per publication too).
    configured_qos: Option<u8>,
    /// vmfix6e attribution: the last binding changes (kind, pair ordinal,
    /// logical owner); kinds 1 new, 2 same-VM rebind, 3 foreign rebind,
    /// 4 restore, 5 parked restore. Printed when a published packet times out.
    history: [(u8, u64, u32); 4],
    /// vmfix6e attribution: pass byte of the last publication on this pair
    /// (completion values carry it; a rebind can change the sequence).
    last_pass: Option<u8>,
    /// A retired, never-executed group awaiting republication on this pair.
    replay: Option<Arc<Packet>>,
    /// Host time the replay was parked (bounds waiting for compute replays).
    replay_since: u64,
    /// Recovery generation of this pair's last executed retirement.
    executed_generation: Option<u64>,
}
impl Pair {
    fn note_binding(&mut self, kind: u8) {
        self.history.rotate_right(1);
        self.history[0] = (kind, self.ordinal, self.owner.unwrap_or(0) as u32);
    }
    #[inline(never)]
    pub(crate) fn new(
        alloc: &Allocator<'_>,
        pool: &Arc<Pool>,
        ids: [kick::Id; 2],
        slot: u8,
        owner: u64,
        context: Arc<Context>,
        free_list: Arc<RenderPool>,
        metrics: buffer::MetricsLease,
        buffer_id: (u8, u64),
        pm_generation: u64,
        first: &Arc<Packet>,
        clusters: u32,
        descriptor_flags: [u32; 2],
        channels: &mut Option<KBox<[Option<kick::Queue>; 2]>>,
    ) -> Result<KBox<Self>> {
        if slot == 0
            || usize::from(slot) >= queues::RENDER_SLOTS
            || owner == 0
            || ids[0].engine() != DataMaster::Tiling
            || ids[1].engine() != DataMaster::Fragment
            || !Arc::ptr_eq(&context, &first.context)
            || !context.is_current()
        {
            return Err(EINVAL);
        }
        if let Err(error) = alloc.uat.vm_context_mask(context.vm()) {
            if error == ENOENT {
                context.status().record(error);
            }
            return Err(error);
        }
        let Validated::Render { pass, .. } = &first.command else {
            return Err(EINVAL);
        };
        let word = first.completion.work_state()?.word();
        if word == u32::MAX {
            return Err(EINVAL);
        }
        let layout = crate::g17::m5_render_submission::layout(&first.command, clusters)?;
        let storage = KBox::<Self>::new_uninit(GFP_KERNEL)?;
        let client_vm = context.vm().clone();
        // A pair's ordinal equals its rings' last timestamp (stamps cover
        // ordinals). Adopted spare rings (vmfix6) come as a matched pair and
        // continue their sequence; fresh rings start at FIRST, ordinal 0.
        let start_ordinal = match channels.as_ref() {
            None => 0,
            Some(spares) => match (spares[0].as_ref(), spares[1].as_ref()) {
                (Some(a), Some(b)) if a.timestamp() == b.timestamp() => a.high_water().get(),
                _ => return Err(EINVAL),
            },
        };
        let (memory, aliases) = memory::Memory::new(
            alloc,
            pool,
            context,
            [ids[0].qid(), ids[1].qid()],
            &layout,
            metrics,
            buffer_id,
            pm_generation,
            start_ordinal,
            word,
        )?;
        // Rings last: a recycled QID's spare ring (vmfix6) is adopted only once
        // nothing else can fail, and handed back if its client alias fails.
        let [(tiling, ta_alias), (fragment, fragment_alias)] =
            adopt_render_kicks(alloc, ids, &client_vm, channels)?;
        let first_owner = owner as u32;
        storage.write_init(kernel::try_init!(Self {
            kick_aliases: Some([Some(ta_alias), Some(fragment_alias)]),
            memory,
            kicks: [tiling, fragment],
            free_list,
            pending_aliases: Some(aliases),
            prepared: None,
            active: Tracker::new(),
            slot,
            owner: Some(owner),
            ordinal: start_ordinal,
            initial_packet: first.id,
            clusters,
            descriptor_flags,
            installed: [false; 2],
            configs: [None; 2],
            cursors: [0; 2],
            recovery_generation: 0,
            registered: None,
            outer_started: 0,
            quarantined: false,
            drain_logged: false,
            terminal: None,
            previous: None,
            displaced: None,
            context_update: true,
            identity_update: true,
            configuration: None,
            configured_context: None,
            configured_qos: None,
            history: [(1, start_ordinal, first_owner); 4],
            last_pass: None,
            replay: None,
            replay_since: 0,
            executed_generation: None,
        }))
    }
    pub(crate) fn qids(&self) -> [u8; 2] {
        self.kicks.each_ref().map(|kick| kick.id().qid())
    }
    pub(crate) fn slot(&self) -> u8 {
        self.slot
    }
    pub(crate) fn owner(&self) -> Option<u64> {
        self.owner
    }
    pub(crate) fn context(&self) -> &Arc<Context> {
        &self.memory.context
    }
    pub(crate) fn pool(&self) -> &Arc<RenderPool> {
        &self.free_list
    }
    pub(crate) fn pool_id(&self) -> u64 {
        u64::from(self.memory.lifecycle[0].predecessor)
    }
    pub(crate) fn buffer_id(&self) -> u8 {
        self.memory.manager.buffer_id()
    }
    pub(crate) fn manager(&mut self) -> &mut buffer::Manager {
        &mut self.memory.manager
    }
    pub(crate) fn in_flight(&self) -> bool {
        self.active.len() != 0 || self.replay.is_some() || self.configuration.is_some()
    }
    /// Cold M5 installation publishes two configuration items before its pair
    /// of kick items. A warm pair needs only the two kick items.
    pub(crate) fn outer_slots(&self) -> u32 {
        if self.memory.graph.is_m5() && self.registered.is_none() && self.installed != [true; 2] { 4 } else { 2 }
    }
    pub(crate) fn configuration_pending(&self) -> bool { self.configuration.is_some() }
    /// Configuration consumption, replay or abandonment needs worker polling.
    pub(crate) fn replay_polling(&self) -> bool {
        self.configuration.is_some() || self.replay.is_some()
            || self.active.iter().any(|a| a.abandoned_since.is_some())
    }
    pub(crate) fn replay_front(&self) -> Option<Arc<Packet>> {
        self.replay.clone()
    }
    /// The parked replay precedes a command waiting through `through`.
    pub(crate) fn replay_through(&self, through: Option<u64>) -> bool {
        self.replay
            .as_ref()
            .is_some_and(|packet| crate::g17::job::Order::contains(through, packet.order.sequence))
    }
    pub(crate) fn describe(&self) {
        let front = self.active.front().map(|a| (a.ticket.ordinal, a.outer_published, a.abandoned_since.is_some(),
            self.memory.pass(a.ticket.ordinal).ok()));
        pr_info!(
            "G17 render pair slot {} qids {:?} owner {:?} executed_generation {:?} active {} parked {} quarantined {} front {:?}\n",
            self.slot, self.qids(), self.owner, self.executed_generation, self.active.len(), self.replay.is_some(),
            self.quarantined, front
        );
    }
    pub(crate) fn replay_since(&self) -> u64 {
        self.replay_since
    }
    /// An executed group of this pair retired in `generation` or later.
    pub(crate) fn executed_since(&self, generation: u64) -> bool {
        self.executed_generation.is_some_and(|executed| executed >= generation)
    }
    /// Published work of this pair is still live: in flight and not parked.
    pub(crate) fn live_unparked(&self) -> bool {
        self.replay.is_none() && self.active.len() != 0
    }
    /// The front group's tiler started and its fragment pass never finished.
    pub(crate) fn front_started_unfinished(&self) -> bool {
        let Some(front) = self.active.front() else {
            return false;
        };
        if self.quarantined || !front.outer_published {
            return false;
        }
        match self.memory.pass(front.ticket.ordinal) {
            Ok(pass) => pass[0] != 0 && pass[1] == 0,
            Err(_) => false,
        }
    }
    /// The front group is inside its abandonment grace period.
    pub(crate) fn abandonment_pending(&self) -> bool {
        self.replay.is_none()
            && self.active.len() == 1
            && self.active.front().is_some_and(|a| a.abandoned_since.is_some())
    }
    /// A scheduler timeout reached a lone group that firmware retired without
    /// executing, before the abandonment proof completed (no completion event
    /// selected the pair). Defer that first timeout once and start the proof;
    /// the event worker completes it and republishes. Guilty work, any other
    /// state, and a second timeout keep the ordinary failure.
    pub(crate) fn defer_abandoned_timeout(
        &mut self,
        packet: &Arc<Packet>,
        host: &mut impl Host,
    ) -> Result<bool> {
        self.begin_abandonment_proof(packet, host, true)
    }
    /// The firmware recovery table cancelled this pair's spared front group.
    /// A group it retired without executing is proved and republished like any
    /// other abandoned group instead of failing with ECANCELED.
    pub(crate) fn defer_abandoned_cancel(
        &mut self,
        packet: &Arc<Packet>,
        host: &mut impl Host,
    ) -> Result<bool> {
        self.begin_abandonment_proof(packet, host, false)
    }
    fn begin_abandonment_proof(
        &mut self,
        packet: &Arc<Packet>,
        host: &mut impl Host,
        timeout: bool,
    ) -> Result<bool> {
        use retirement::Memory;
        if self.quarantined || self.active.len() != 1 || self.replay.is_some() {
            return Ok(false);
        }
        let front = self.active.front().ok_or(EIO)?;
        if !Arc::ptr_eq(&front.packet, packet)
            || !front.outer_published
            || front.failed
            || (timeout && front.timeout_deferred)
            || packet.completion.status().get() != 0
            || packet.completion.guilty()
            || packet.completion.replay_count() >= 3
        {
            return Ok(false);
        }
        let mut observer = Observer {
            memory: &self.memory,
            qids: self.qids(),
            host,
        };
        core::sync::atomic::fence(core::sync::atomic::Ordering::Acquire);
        let snapshot = observer.snapshot(front.ticket.ordinal)?;
        let stamps = observer.stamps()?;
        let (_, state) = observer.host.epoch()?;
        if snapshot.pass[1] != 0
            || !retirement::abandoned(&front.ticket, snapshot, stamps, state, false)?
        {
            return Ok(false);
        }
        let now = crate::g17::now_ns();
        let front = self.active.front_mut().ok_or(EIO)?;
        if timeout {
            front.timeout_deferred = true;
        }
        front.abandoned_since.get_or_insert(now);
        Ok(true)
    }
    pub(crate) fn finish_replay(&mut self, packet: &Arc<Packet>) -> Result {
        if !self.replay.as_ref().is_some_and(|front| Arc::ptr_eq(front, packet)) {
            return Err(EIO);
        }
        self.replay = None;
        Ok(())
    }
    pub(crate) fn published_in_flight(&self) -> bool {
        !self.quarantined && self.active.iter().any(|active| active.outer_published)
    }
    pub(crate) fn room(&self) -> bool {
        !self.quarantined && self.owner.is_some() && self.prepared.is_none() && self.active.room()
    }
    /// vmfix6d A: everything the render admission EBUSY sites test, sampled
    /// under the device mutex for capacity-wait reports and fingerprints.
    pub(crate) fn diag(&self) -> PairDiag {
        let graph = self.memory.graph.cursors().ok().map(|c| c.map(|c| [c.done, c.read, c.write]));
        PairDiag {
            owner: self.owner.unwrap_or(0),
            context: self.memory.context.id(),
            ordinal: self.ordinal,
            active: self.active.len() as u32,
            prepared: self.prepared.is_some(),
            quarantined: self.quarantined,
            displaced: self.displaced.is_some(),
            previous: self.previous.as_ref().map_or(0, |p| {
                1 | (u32::from(p.memory.is_some()) << 1)
                    | (u32::from(p.publication.is_some()) << 2)
                    | (u32::from(p.ready) << 3)
            }),
            context_update: self.context_update,
            installed: self.installed,
            cursors: self.cursors,
            graph: graph.unwrap_or([[u32::MAX; 3]; 2]),
            aliases: u32::from(self.pending_aliases.is_some())
                | (u32::from(self.kick_aliases.is_some()) << 1),
            registered: self.registered.map_or(u32::MAX, u32::from),
            qids: self.qids(),
            buffer: self.buffer_id(),
            claims: [
                self.memory.target.slot_owner(self.ordinal).unwrap_or(u64::MAX),
                self.memory.graph.record_owner(self.ordinal).unwrap_or(u64::MAX),
            ],
        }
    }
    pub(crate) fn take_terminal(&mut self) -> Option<(Arc<VmStatus>, Error)> {
        self.terminal.take()
    }

    /// The registry owns this pair exclusively during potentially allocating
    /// preparation. No queue producer or shared submission counter changes.
    pub(crate) fn prepare(
        &mut self,
        alloc: &Allocator<'_>,
        packet: &Arc<Packet>,
        addresses: RenderAddresses,
    ) -> Result {
        if !self.room()
            || !Arc::ptr_eq(&self.memory.context, &packet.context)
            || !packet.context.is_current()
        {
            return Err(EBUSY);
        }
        let Validated::Render {
            pass, attachments, resources: programs, plan, ..
        } = &packet.command
        else {
            return Err(EINVAL);
        };
        if packet.completion.status().get() != 0 {
            return Err(EIO);
        }
        // Native binds the TA channel's mesh rendering buffer only to kicks
        // whose descriptor requests it (native726: the object→mesh draw), so
        // only API task (object-stage) kicks get it; plain mesh kicks, whose
        // object stage is the passthrough, do not. Resolve it before any
        // ordinal state is claimed so an allocation failure fails this kick
        // cleanly.
        let mesh_storage = if programs.as_ref().is_some_and(|p| p.uses_task()) {
            self.memory.mesh_storage_register(alloc)?
        } else { 0 };
        let ordinal = self.ordinal;
        ordinal.checked_add(1).ok_or(EOVERFLOW)?;
        let work = packet.completion.work_state()?;
        let node = work.node_va()?;
        let word = work.word();
        let layout = crate::g17::m5_render_submission::layout(&packet.command, self.clusters)?;
        let owners = self.memory.owners();
        self.memory
            .target
            .ensure_capacity(alloc.dev, packet.context.vm(), &layout, &owners)?;
        let target = self.memory.target.prepare(ordinal, &layout)?;
        let scene = self.memory.manager.prepare_scene(ordinal)?;
        self.memory.generation(ordinal)?.prepare(ordinal, pass)?;
        let [deflake, ta_status, fragment_status, fragment_fw, auxiliary] =
            self.memory.generation(ordinal)?.addresses();
        let lifecycle = if ordinal != 0 || packet.id != self.initial_packet {
            let (fragment, tiling) =
                crate::g17::ids::OBJECTS.render_pair(self.memory.lifecycle[0].predecessor)?;
            [tiling, fragment]
        } else {
            self.memory.lifecycle
        };
        let (scratch, metric) = self.memory.manager.scene_registers(scene)?;
        let timestamp_offset = self.memory.timestamp_offset(ordinal);
        let timestamp = self.memory.timestamps.gpu_va() + timestamp_offset as u64;
        let resources = abi::RenderResources {
            context_id: packet.context.id().try_into()?,
            context_generation: packet.context.render_generation(),
            pb_slot: self.buffer_id().into(),
            free_list_slot: self.free_list.id().into(),
            free_list_control_va: self.free_list.control_va(),
            kick_record_va: node,
            pb_va: self.memory.manager.state_va(),
            scene_va: self.memory.manager.scene_va(scene)?,
            scene_end_va: self.memory.manager.trailer_va(),
            pm_scratch: scratch,
            pm_metric: metric,
            tilemap_va: target.tilemap,
            layermeta_va: target.layer,
            heapmeta_va: target.heap,
            tpc_va: self.memory.target.tpc_va(),
            tpc_size: layout.tpc_size as u64,
            deflake_va: deflake,
            ta_status_va: ta_status,
            mesh_storage,
            mesh_class: programs.as_ref().map_or(0, |p| p.mesh_class()),
            fragment_status_va: fragment_status,
            fragment_status_fw_va: fragment_fw,
            aux_fb_va: auxiliary,
            timestamp_va: timestamp,
            ta_ids: lifecycle[0],
            fragment_ids: lifecycle[1],
            unk_2104: self.descriptor_flags[0],
            unk_210c: self.descriptor_flags[1],
        };
        let qids = self.qids();
        let args = abi::RenderArgs {
            pass,
            resources: &resources,
            ordinal,
            ta: abi::RenderStage {
                qid: qids[0],
                descriptor_va: self.memory.graph.descriptor_client(0, ordinal),
                state_word: word,
                stamp_va: addresses.stamps[0][0],
                aux_stamp_va: addresses.stamps[0][1],
            },
            fragment: abi::RenderStage {
                qid: qids[1],
                descriptor_va: self.memory.graph.descriptor_client(1, ordinal),
                state_word: word,
                stamp_va: addresses.stamps[1][0],
                aux_stamp_va: addresses.stamps[1][1],
            },
        };
        self.memory.graph.claim(ordinal)?;
        let result = self.memory.write_descriptors(
            alloc.dev,
            &args,
            attachments[1].as_slice(),
            packet.context.id().try_into()?,
            &layout,
            programs.as_ref().and(*plan),
        );
        let (mcache, register_arrays, kick_flags) = match result {
            Ok(encoded) => encoded,
            Err(error) => {
                self.memory.graph.release(ordinal)?;
                self.memory.generation(ordinal)?.ordinal = None;
                self.memory.target.cancel(ordinal);
                return Err(error);
            }
        };
        self.memory.clear_timestamps(ordinal)?;
        self.memory.manager.claim_scene(ordinal, scene)?;
        self.prepared = Some(Prepared {
            packet: packet.clone(),
            ordinal,
            mcache,
            register_arrays,
            kick_flags,
        });
        Ok(())
    }
}

/// Kick barriers of one render. The fragment kick always waits for its own tiling kick;
/// with the fragment-barriers flag the resolved prefix orders the fragment kick too and the
/// tiling kick preserves the TA prefix across physical pairs. Otherwise the render prefix
/// orders the tiling kick. A compute dependency always orders the tiling kick.
struct RenderBarriers {
    tiling: [KickDependency; 3],
    tiling_count: usize,
    fragment: [KickDependency; 3],
    fragment_count: usize,
    tiling_types: [u8; 4],
    fragment_types: [u8; 4],
}

fn render_barriers(
    fragment_stage: bool,
    dependencies: &RenderDependencies,
    own_tiling: KickDependency,
) -> RenderBarriers {
    let mut tiling_barriers = [KickDependency::ZERO; 3];
    let mut tiling_count = 0;
    let mut fragment_barriers = [own_tiling; 3];
    let mut fragment_count = 1;
    let mut tiling_types = [1; 4];
    let mut fragment_types = [3; 4];
    fragment_types[0] = 1;
    for dependency in dependencies.render() {
        if fragment_stage {
            fragment_barriers[fragment_count] = *dependency;
            fragment_count += 1;
        } else {
            tiling_barriers[tiling_count] = *dependency;
            // A full channel barrier uses the tiling encoder's class 2.
            // Class 3 belongs to the fragment encoder.
            tiling_types[tiling_count] = 2;
            tiling_count += 1;
        }
    }
    if fragment_stage {
        // The implicit parent covers only this physical ring. Keep foreign TA
        // predecessors without waiting for their fragment completion. Class 1
        // names the tiling encoder; full/compute barriers below retain class 2.
        for dependency in &dependencies.tiling[..dependencies.tiling_count] {
            if dependency.qid() != own_tiling.qid() {
                tiling_barriers[tiling_count] = *dependency;
                tiling_count += 1;
            }
        }
    }
    if let Some(compute) = dependencies.compute {
        tiling_barriers[tiling_count] = compute;
        tiling_types[tiling_count] = 2;
        tiling_count += 1;
    }
    RenderBarriers {
        tiling: tiling_barriers,
        tiling_count,
        fragment: fragment_barriers,
        fragment_count,
        tiling_types,
        fragment_types,
    }
}

impl Pair {
    /// All descriptor bytes and retained allocations are ready before entry.
    /// The first inner producer establishes firmware ownership, including on
    /// later errors. Such failures retain the active packet for recovery.
    pub(crate) fn publish(
        &mut self,
        packet: Arc<Packet>,
        dependencies: &RenderDependencies,
        lease: PublicationLease,
        host: &mut impl Host,
        defer: &mut impl FnMut(Deferred) -> Result,
    ) -> Result {
        use crate::g17::fw::{
            channels::WorkSlot,
            kick::{KickArgs, KickQos},
            queue::{
                FreeListBinding, QueueConfig, QueueConfigArgs, QueueConfigTarget,
                TilingBinding,
            },
        };
        use core::sync::atomic::{fence, Ordering};
        let (epoch, mcache, policy) = self.publication_preflight(&packet, host)?;
        let context_id = packet.context.id();
        let qos_slot = packet.context.qos_id();
        if self.configured_context.is_some_and(|configured| configured != context_id)
            || self.configured_qos.is_some_and(|configured| configured != qos_slot) {
            self.identity_update = true;
        }
        let m5 = self.memory.graph.is_m5();
        let ordinal = self.ordinal;
        let next_ordinal = ordinal.checked_add(1).ok_or(EOVERFLOW)?;
        let qids = self.qids();
        let kicks = self.kicks.each_ref().map(kick::Queue::timestamp);
        let parents = self.kicks.each_ref().map(kick::Queue::parent);
        let work = packet.completion.work_state()?;
        // A channel moves to this pair only after every publication made
        // through it is visible on both of its rings (stamp covers ordinal+1).
        let work_slot = work.render_channel(self.slot, |old, published| {
            host.stamps(old).is_ok_and(|stamps| {
                stamps.into_iter().all(|stamp| retirement::stamp_covers(stamp, published))
            })
        })?;
        self.memory.bind_work_slot(ordinal, qids, work_slot)?;
        let payload = work.channel_payload(self.slot, ordinal)?;
        let stamps = host.stamps(qids)?;
        fence(Ordering::Acquire);
        let covered = stamps.map(|stamp| retirement::stamp_covers(stamp, ordinal));
        if self.history[0].1 == ordinal && self.history[0].0 != 1 {
            // First publication after a rebind: does the pass sequence on
            // these rings continue, which update bits does the configuration
            // carry and is the previous publication complete (seeded)?
            // (vmfix6e/6f attribution, sampled.)
            static FIRSTS: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);
            if let Some(n) = crate::g17::progress::sampled(&FIRSTS) {
                pr_info!("asahi: render qids {:?} first publication after rebind kind {} at {}: pass {:?} -> {} identity_update={} covered={:?} (first {})\n",
                    qids, self.history[0].0, ordinal, self.last_pass, payload, self.identity_update, covered, n);
            }
        }
        self.memory
            .completion_links(ordinal, kicks, payload, !covered[1])?;
        let context = &packet.context;
        let ring = 1u8 << policy.priority();
        let qos = context.qos_id();
        let qos_owners = [
            qos::Owner {
                qid: qids[0],
                qos,
                data_master: DataMaster::Tiling as u8,
            },
            qos::Owner {
                qid: qids[1],
                qos,
                data_master: DataMaster::Fragment as u8,
            },
        ];
        let mut configs = [None; 2];
        for stage in 0..2 {
            let args = QueueConfigArgs {
                target: if stage == 0 {
                    QueueConfigTarget::Tiling(Some(TilingBinding {
                        pb_va: self.memory.manager.state_va(),
                        pb_token: lease.buffer_token,
                        pb_slot: self.buffer_id().into(),
                        fragment_qid: qids[1],
                    }))
                } else {
                    QueueConfigTarget::Fragment
                },
                kick_ring_va: self.kicks[stage].low_va(),
                kick_ring_fw_va: self.kicks[stage].firmware_va(),
                qid: qids[stage],
                install: !self.installed[stage],
                completion_seed: (covered[stage] && (!m5 || !self.installed[stage]
                    || self.context_update)).then(|| {
                    kicks[stage]
                        .prev()
                        .completion_value(payload.wrapping_sub(1))
                }),
                context_id: context.id().try_into()?,
                free_list: FreeListBinding {
                    va: self.free_list.control_va(),
                    generation: lease.free_list_generation,
                    slot: self.free_list.id().into(),
                },
                scheduler_va: context.scheduler_va(),
                policy,
                qos_slot: qos,
                qos_update: self.identity_update,
                owner_pid: context.owner_pid(),
                context_update: self.identity_update,
            };
            configs[stage] = Some(if m5 {
                crate::g17::fw::m5::queue::config(&args,
                    crate::g17::fw::m5::queue::ConfigProfile {
                        soft_fault_enabled: true,
                        extended: false,
                        parameter_manager: (stage == 0).then(|| u32::from(self.buffer_id())),
                    })?
            } else { QueueConfig::new(&args)? });
        }
        let configs = [configs[0].ok_or(EIO)?, configs[1].ok_or(EIO)?];
        if m5 && self.registered.is_none() {
            if self.installed == [true; 2] {
                let cursors = self.memory.graph.cursors()?;
                if cursors.iter().zip(self.cursors).any(|(c, end)| c.read != end) {
                    return Err(EBUSY);
                }
            } else {
                // Consume the initial queue records before installing hardware rings.
                let lanes = [
                    publication::Lane::new(&self.memory.graph, 0, ordinal, self.cursors[0], configs[0], true, kicks[0])?,
                    publication::Lane::new(&self.memory.graph, 1, ordinal, self.cursors[1], configs[1], true, kicks[1])?,
                ];
                let ends = self.cursors.map(|base| (base + 1) % crate::g17::fw::queue::ITEM_RING_ENTRIES);
                let slots = [0, 1].map(|stage| WorkSlot::new(DataMaster::Compute,
                    self.memory.graph.queue_va(qids[stage]), qids[stage], ends[stage] as u16, true));
                let [fragment, tiling] = [slots[1], slots[0]];
                let (fragment, tiling) = (fragment?, tiling?);
                for kick in &self.kicks { host.publish_qid(kick.id())?; }
                if self.configuration.is_some() { return Err(EBUSY); }
                self.configuration = Some(configuration::Configuration::new(&packet.context, ends, host.generation())?);
                let configuration = self.configuration.as_mut().ok_or(EIO)?;
                let configured = host.publish_configuration_pair(policy.priority(), &fragment, &tiling, || {
                    // Ownership exists before either item producer can become visible.
                    configuration.mark_visible();
                    for lane in &lanes { lane.publish_prefix(); }
                });
                if let Err(error) = configured {
                    if self.configuration.as_ref().is_some_and(|owner| !owner.visible()) {
                        self.configuration.take().ok_or(EIO)?.release();
                    }
                    return Err(error);
                }
                self.cursors = ends;
                self.installed = [true; 2];
                self.configured_context = Some(context_id);
                self.configured_qos = Some(qos_slot);
                self.configs = [Some(configs[0]), Some(configs[1])];
                self.context_update = false;
                self.identity_update = false;
                self.outer_started |= ring;
                packet.context.mark_published();
                host.notify_render(policy.priority())?;
                return Err(EBUSY);
            }
        }
        let cache = if self.recovery_generation == epoch {
            self.configs
        } else {
            [None; 2]
        };
        let present = [
            publication::required(cache[0], configs[0]),
            publication::required(cache[1], configs[1]),
        ];
        let cursors = self.memory.graph.cursors()?;
        if cursors[0].write != self.cursors[0] || cursors[1].write != self.cursors[1] {
            return Err(EBUSY);
        }
        let queues = qids.map(|qid| self.memory.graph.queue_va(qid));
        let Validated::Render { pass, .. } = &packet.command else {
            return Err(EINVAL);
        };
        let own_tiling = KickDependency::new(qids[0], kicks[0]).ok_or(EINVAL)?;
        let barriers = render_barriers(pass.fragment_barriers, dependencies, own_tiling);
        let tiling_barriers = &barriers.tiling[..barriers.tiling_count];
        let barrier_types = [
            &barriers.tiling_types[..barriers.tiling_count + 1],
            &barriers.fragment_types[..barriers.fragment_count + 1],
        ];
        let kick_flags = self.prepared.as_ref().ok_or(EIO)?.kick_flags;
        let args = |stage: usize, barriers, arrays, mcache| KickArgs {
            qid: qids[stage],
            timestamp: kicks[stage],
            parent: parents[stage],
            barriers,
            dependency_types: m5.then_some(barrier_types[stage]),
            descriptor_va: self.memory.graph.descriptor_va(stage, ordinal),
            queue_va: queues[stage],
            qos: KickQos {
                slot: qos,
                class: if stage == 0 {
                    abi::TA_QOS_CLASS
                } else {
                    abi::FRAGMENT_QOS_CLASS
                },
            },
            mcache,
            // Saved M5 native725 TA kicks all retain event254 at offset0x140.
            // Keep other engines/families unchanged while qualifying this parity.
            event_mask: if m5 && stage == 0 { [0, 0, 0, 1u64 << 62] }
                else { abi::RENDER_KICK_EVENT_MASK },
            register_arrays: arrays,
            compute_matrix: kick_flags.matrix[stage],
            compute_scratch: kick_flags.scratch[stage],
            priority: policy.priority(),
        };
        let ta_args = args(
            0,
            tiling_barriers,
            self.prepared.as_ref().ok_or(EIO)?.register_arrays[0],
            None,
        );
        let fragment_args = args(
            1,
            &barriers.fragment[..barriers.fragment_count],
            self.prepared.as_ref().ok_or(EIO)?.register_arrays[1],
            mcache,
        );
        let registration = if m5 { Registration::M5(
            crate::g17::fw::m5::queue::HardwareSubmission::new(
                (qids[1], self.kicks[1].low_va()),
                (qids[0], self.kicks[0].low_va()),
                policy.priority(), true,
            )?)
        } else { Registration::Neo(RenderRegistration::new(
            (qids[1], self.kicks[1].low_va()),
            (qids[0], self.kicks[0].low_va()),
            policy.priority(),
        )?) };
        let [tiling_kick, fragment_kick] = &mut self.kicks;
        let tiling_kick = tiling_kick.prepare_entry(&ta_args)?;
        let fragment_kick = fragment_kick.prepare_entry(&fragment_args)?;
        let memory = &mut *self.memory;
        let lanes = [
            publication::Lane::new(
                &memory.graph,
                0,
                ordinal,
                self.cursors[0],
                configs[0],
                present[0],
                kicks[0],
            )?,
            publication::Lane::new(
                &memory.graph,
                1,
                ordinal,
                self.cursors[1],
                configs[1],
                present[1],
                kicks[1],
            )?,
        ];
        let windows = [lanes[0].window, lanes[1].window];
        let previous = self.active.front().map_or(cursors, |active| {
            active.ticket.windows.map(|window| {
                let target = window.target();
                retirement::Cursors {
                    done: target,
                    read: target,
                    write: target,
                }
            })
        });
        let fragment_slot = WorkSlot::new(
            DataMaster::Fragment,
            queues[1],
            qids[1],
            windows[1].target().try_into()?,
            self.outer_started & ring == 0,
        )?;
        let tiling_slot = WorkSlot::new(
            DataMaster::Tiling,
            queues[0],
            qids[0],
            windows[0].target().try_into()?,
            self.outer_started & ring == 0,
        )?;
        let legacy_state = if m5 { None } else {
            let event_slot = memory.graph.support.word(abi::storage::EVENT_SLOTS + 4)?;
            let next_event = event_slot.load(Ordering::Relaxed).wrapping_add(1);
            let inner = memory.graph.support.word(0x28000)?;
            let next_inner = (next_ordinal as u32).wrapping_mul(2);
            Some((event_slot, next_event, inner, next_inner))
        };
        let flist_submission = self.free_list.prepare_submission(2)?;
        let pm_submission = memory.manager.prepare_submission()?;
        self.active
            .push(Active {
                activity: crate::g17::context::ActivityMark::begin(&packet.context),
                qos_completed: false,
                flist_completed: false,
                outer_published: false,
                generation: lease.free_list_generation,
                packet: packet.clone(),
                ticket: Ticket {
                    ordinal,
                    windows,
                    previous,
                },
                kicks,
                // vmfix6d: the first publication after a rebind is always the
                // witness that retires `previous`, even when the M5 initial
                // registration step already cleared context_update (a fresh
                // pair rebound before its first publication otherwise kept an
                // unready, published `previous` forever: state 0x5, every
                // later same-owner rebind EBUSY).
                context_update: self.context_update
                    || self.previous.as_ref().is_some_and(|p| p.publication.is_none()),
                failed: false,
                drained_result: None,
                abandoned_since: None,
                timeout_deferred: false,
                recovery_generation: host.generation(),
            })
            .map_err(|_| EBUSY)?;
        self.prepared = None;
        packet.mark_published();
        // The Active now retains this Packet and its original identity/QoS
        // pins; it replaces the configuration-only identity owner.
        if let Some(configuration) = self.configuration.take() { configuration.release(); }
        work.channel_published(self.slot, qids, next_ordinal);
        if m5 {
            for lane in &lanes { lane.prepare(); }
        } else { lanes[1].publish_prefix(); }
        fragment_kick.commit();
        if !m5 { lanes[1].publish_kick(); }
        tiling_kick.commit();
        let mut qos_publications = [None; 2];
        let result = (|| {
            qos_publications[1] =
                Some(host.qos_publish(qos_owners[1], context.scheduler_va(), policy)?);
            qos_publications[0] =
                Some(host.qos_publish(qos_owners[0], context.scheduler_va(), policy)?);
            // A class change happens only at a rebind, after the pair drained.
            if self.registered != Some(policy.priority()) {
                host.install_pair(&registration)?;
                self.registered = Some(policy.priority());
            }
            for kick in &self.kicks {
                host.publish_qid(kick.id())?;
            }
            host.publish_outer_pair(
                policy.priority(),
                &fragment_slot,
                &tiling_slot,
                || {
                    work.add_submitted_kicks(2)?;
                    flist_submission.commit();
                    pm_submission.commit();
                    Ok(())
                },
                || if let Some((event_slot, next_event, inner, next_inner)) = legacy_state {
                    lanes[0].publish_tiling(event_slot, next_event, inner, next_inner);
                } else {
                    lanes[1].publish_kick();
                    lanes[0].publish_kick();
                },
            )
        })();
        let outer_next = match result {
            Ok(next) => next,
            Err(error) => {
                self.quarantined = true;
                if !packet.completion.spared() && self.terminal.is_none() {
                    self.terminal = Some((packet.completion.status().clone(), error));
                }
                self.active
                    .back()
                    .ok_or(EIO)?
                    .packet
                    .completion
                    .quarantine_render();
                for publication in qos_publications.into_iter().flatten() {
                    host.qos_cancel(publication)?;
                }
                self.active.back_mut().ok_or(EIO)?.qos_completed = true;
                self.quarantined = true;
                self.record_publication_failure(&packet, error, defer)?;
                return Err(error);
            }
        };
        self.ordinal = next_ordinal;
        self.last_pass = Some(payload);
        self.cursors = windows.map(|window| window.target());
        for stage in 0..2 {
            if present[stage] {
                self.configs[stage] = Some(configs[stage]);
            }
        }
        self.recovery_generation = epoch;
        self.installed = [true; 2];
        self.configured_context = Some(packet.context.id());
        self.configured_qos = Some(packet.context.qos_id());
        // vmfix6f: both configurations of this publication carried the update
        // bits (publication::required), so firmware applies them before this
        // kick. Native sets them only on a new owner's first configuration;
        // the pre-6f flag repeated them on every publication until this one
        // retired, i.e. reconfigured the queue's context while its earlier
        // kick was still pending (the v18p timeouts: all items consumed,
        // the rebind's first kick never completes).
        if !rebind_legacy(LEGACY_UPDATE_UNTIL_RETIRED) {
            self.identity_update = false;
        }
        self.outer_started |= ring;
        if let Some(previous) = self.previous.as_mut().filter(|p| p.publication.is_none()) {
            previous.publication = Some(ordinal);
        }
        packet.context.mark_published();
        self.announce_publication(
            host,
            (&fragment_slot, &tiling_slot, outer_next),
            lease.free_list_generation,
            policy.priority(),
            &packet,
            defer,
        )
    }

    /// Everything that must hold before a publication changes any state: no alias work
    /// pending, a healthy device epoch, room in the active ring, this slot's packet, the
    /// prepared descriptors for this ordinal, a clean completion status and a registration
    /// class the pair can still take. Returns the epoch, the prepared memory-cache setting
    /// and the owner's policy.
    fn publication_preflight(
        &self,
        packet: &Arc<Packet>,
        host: &mut impl Host,
    ) -> Result<(u64, Option<McacheTable>, crate::g17::fw::queue::Policy)> {
        if self.pending_aliases.is_some() || self.kick_aliases.is_some() {
            return Err(EBUSY);
        }
        let (epoch, recovery_state) = host.epoch()?;
        if recovery_state != 0
            || self.quarantined
            || !self.active.room()
            || packet.completion.render_slot() != Some(self.slot)
        {
            return Err(EBUSY);
        }
        let prepared = self.prepared.as_ref().ok_or(EINVAL)?;
        if !Arc::ptr_eq(&prepared.packet, packet)
            || prepared.ordinal != self.ordinal
            || !self.memory.owns(self.ordinal)
            || !packet.context.is_current()
        {
            return Err(EIO);
        }
        if packet.completion.status().get() != 0 {
            return Err(EIO);
        }
        let policy = packet.context.policy();
        // A registration of another class can only follow a rebind, which
        // requires the pair to have drained.
        if self.registered.is_some_and(|class| class != policy.priority()) && self.in_flight() {
            return Err(EIO);
        }
        Ok((epoch, prepared.mcache, policy))
    }

    /// Records a failed publication on the retained active entry: the terminal status
    /// (once), the deferred failure and the entry's failed flag.
    fn record_publication_failure(
        &mut self,
        packet: &Arc<Packet>,
        error: Error,
        defer: &mut impl FnMut(Deferred) -> Result,
    ) -> Result {
        if !packet.completion.spared() && self.terminal.is_none() {
            self.terminal = Some((packet.completion.status().clone(), error));
        }
        defer(Deferred::FailedOwned(packet.completion.clone(), error))?;
        self.active.back_mut().ok_or(EIO)?.failed = true;
        Ok(())
    }

    /// Activates the pool lease and announces the retained pair to the firmware; a failure
    /// here quarantines the pair with the packet still retained for recovery.
    fn announce_publication(
        &mut self,
        host: &mut impl Host,
        (fragment_slot, tiling_slot, outer_next): (&WorkSlot, &WorkSlot, [u32; 2]),
        free_list_generation: u64,
        priority: u8,
        packet: &Arc<Packet>,
        defer: &mut impl FnMut(Deferred) -> Result,
    ) -> Result {
        let publish = (|| {
            host.activate_render_pool(self.free_list.id(), free_list_generation)?;
            host.publish_retained_pair(priority, fragment_slot, tiling_slot, outer_next)?;
            self.active.back_mut().ok_or(EIO)?.outer_published = true;
            host.notify_render(priority)?;
            host.note_submission()
        })();
        if let Err(error) = publish {
            self.quarantined = true;
            self.active
                .back()
                .ok_or(EIO)?
                .packet
                .completion
                .quarantine_render();
            self.record_publication_failure(packet, error, defer)?;
            return Err(error);
        }
        Ok(())
    }
}

struct Observer<'a, H> {
    memory: &'a memory::Memory,
    qids: [u8; 2],
    host: &'a mut H,
}
impl<H: Host> retirement::Memory for Observer<'_, H> {
    fn snapshot(&mut self, ordinal: u64) -> Result<retirement::Snapshot> {
        observation::snapshot(self.memory, ordinal)
    }

    fn stamps(&mut self) -> Result<[u32; 2]> {
        self.host.stamps(self.qids)
    }
    fn pass(&mut self, ordinal: u64) -> Result<[u64; 2]> {
        self.memory.pass(ordinal)
    }
    fn stage_timestamps(&mut self, ordinal: u64) -> Result<[u64; 4]> {
        self.memory.stage_timestamps(ordinal)
    }
}

impl Pair {
    fn qos_owners(&self) -> [qos::Owner; 2] {
        let qids = self.qids();
        let qos = self.context().qos_id();
        [
            qos::Owner {
                qid: qids[0],
                qos,
                data_master: DataMaster::Tiling as u8,
            },
            qos::Owner {
                qid: qids[1],
                qos,
                data_master: DataMaster::Fragment as u8,
            },
        ]
    }
    fn selected(&self, masks: Option<[u64; 2]>) -> bool {
        masks.is_none_or(|masks| {
            self.qids()
                .into_iter()
                .all(|qid| masks[usize::from(qid) / 64] & (1u64 << (qid % 64)) != 0)
        })
    }
    pub(crate) fn contains_packet(&self, packet: &Arc<Packet>) -> bool {
        self.active
            .iter()
            .any(|active| Arc::ptr_eq(&active.packet, packet))
            || self
                .prepared
                .as_ref()
                .is_some_and(|prepared| Arc::ptr_eq(&prepared.packet, packet))
    }
    pub(crate) fn replay_contains(&self, packet: &Arc<Packet>) -> bool {
        self.replay.as_ref().is_some_and(|replay| Arc::ptr_eq(replay, packet))
    }
    pub(crate) fn take_deferred_timeout(&self) -> Option<(Arc<Packet>, Error)> {
        self.active.iter().find_map(|active| {
            active
                .packet
                .completion
                .take_deferred_timeout()
                .map(|error| (active.packet.clone(), error))
        })
    }

    pub(crate) fn diagnose_m5(&mut self, dev: &crate::driver::AsahiDevice) {
        if !crate::g17::m5_render_submission::enabled() { return; }
        if let Some(active) = self.active.front() {
            let ordinal = active.ticket.ordinal;
            let kicks = active.kicks;
            dev_warn!(dev.as_ref(), "M5 render progress: ordinal={} cursors={:?} timestamps={:?} pool={:?} head={:?}\n",
                ordinal, self.memory.graph.cursors(), self.memory.pass(ordinal),
                self.free_list.counters(), self.memory.context.work_head());
            dev_warn!(dev.as_ref(), "M5 render binding history (kind, ordinal, owner): {:?} pb={} ctx_update={} identity_update={} configs={:?} last_pass={:?}\n",
                self.history, self.buffer_id(), self.context_update, self.identity_update,
                [self.configs[0].is_some(), self.configs[1].is_some()], self.last_pass);
            for stage in 0..2 {
                dev_warn!(dev.as_ref(), "M5 render kick stage={} words={:x?}\n", stage,
                    self.kicks[stage].read_entry_words(kicks[stage]));
            }
            // vmfix6f attribution: the queue records (owner fields) and the
            // private job-list head the firmware sees for these rings.
            for qid in self.qids() {
                let at = self.memory.graph.queue_offset(qid);
                let mut words = [0u32; 32];
                for (index, word) in words.iter_mut().enumerate() {
                    *word = self.memory.graph.queues.word(at + index * 4)
                        .map_or(u32::MAX, |w| w.load(core::sync::atomic::Ordering::Relaxed));
                }
                dev_warn!(dev.as_ref(), "M5 render queue record qid={} words={:x?}\n", qid, words);
            }
            let head = [
                self.memory.graph.queues.dword(memory::JOB_LIST).map_or(u64::MAX, |w| w.load(core::sync::atomic::Ordering::Relaxed)),
                self.memory.graph.queues.dword(memory::JOB_LIST + 8).map_or(u64::MAX, |w| w.load(core::sync::atomic::Ordering::Relaxed)),
            ];
            dev_warn!(dev.as_ref(), "M5 render private job list={:x?} shared={:x?} work_head_va={:#x} scheduler_va={:#x}\n",
                head, self.memory.context.work_head(), self.memory.context.work_head_va(),
                self.memory.context.scheduler_va());
        }
    }

    pub(crate) fn oldest_packet(&self) -> Option<Arc<Packet>> {
        self.active.front().map(|active| active.packet.clone())
    }
    /// Sample PM feedback only after this pair's exact retirement. Busy pairs
    /// keep their current capacity and let firmware request growth as needed.
    pub(crate) fn host_growth_target(&mut self, packet: &Packet) -> Result<Option<usize>> {
        let Validated::Render { pass, .. } = &packet.command else {
            return Err(EINVAL);
        };
        let required = crate::g17::m5_render_submission::layout(&packet.command, self.clusters)?.tvb_blocks;
        self.memory.manager.note_required(required, crate::g17::now_ns());
        if self.in_flight() {
            return Ok(None);
        }
        if !self.idle_for_context(self.context())? {
            return Err(EBUSY);
        }
        let feedback = if self.ordinal != 0 {
            self.memory.manager.automatic_target()?.unwrap_or(0)
        } else {
            0
        };
        let required = required.max(feedback);
        Ok((required > self.memory.manager.blocks()).then_some(required))
    }

    pub(crate) fn needs_drain(&self) -> bool {
        self.quarantined
    }

    pub(crate) fn oldest_spared(&self) -> Option<bool> {
        self.active
            .front()
            .map(|active| active.packet.completion.spared())
    }
    pub(crate) fn visit_owners(&self, visit: &mut dyn FnMut(&Arc<VmStatus>)) {
        for active in self.active.iter() {
            visit(active.packet.completion.status());
        }
    }
    pub(crate) fn classify(&self, classify: &dyn Fn(&Arc<VmStatus>) -> recovery::Class) {
        for active in self.active.iter() {
            active
                .packet
                .completion
                .classify(classify(active.packet.completion.status()));
        }
    }
    /// A slot stamp must name retained work before its execution markers are meaningful.
    pub(crate) fn pass_started(&self, stamp: u64) -> Option<bool> {
        const MASK: u64 = (1 << 40) - 1;
        let ordinal = self.active.iter().map(|active| active.ticket.ordinal)
            .find(|ordinal| ordinal.wrapping_add(1) & MASK == stamp & MASK)?;
        Some(observation::snapshot(&self.memory, ordinal).ok()?.pass[0] != 0)
    }

    /// Firmware is halted and all graph borrowers have joined before this copy.
    pub(crate) fn dump_m5_failure(&self, dev: &kernel::device::Device) {
        for active in self.active.iter().take(4) {
            let ordinal = active.ticket.ordinal;
            dev_info!(dev, "M5 render failure context={} qids={:?} ordinal={} pass={:?}\n",
                self.memory.context.id(), self.qids(), ordinal,
                observation::snapshot(&self.memory, ordinal).map(|s| s.pass));
            for stage in 0..2 {
                if let Err(error) = self.kicks[stage].dump_m5_entry(dev,
                    if stage == 0 { "fault-kick-ta" } else { "fault-kick-fragment" }, active.kicks[stage]) {
                    dev_warn!(dev, "M5 fault kick read failed: {:?}\n", error);
                }
            }
            if let Err(error) = self.memory.dump_m5_failure(dev, ordinal) {
                dev_warn!(dev, "M5 render failure state read failed: {:?}\n", error);
            }
        }
    }

    pub(crate) fn publish_recovery_targets(&self) -> Result {
        self.memory.graph.publish_recovery_targets()
    }

    pub(crate) fn resync(&mut self) {
        self.configs = [None; 2];
        for kick in &mut self.kicks {
            kick.clear_parent_after_recovery();
        }
    }
    pub(crate) fn matches(&self, owner: u64, context: &Arc<Context>) -> Result<bool> {
        Ok(self.owner == Some(owner)
            && Arc::ptr_eq(self.context(), context)
            && context.is_current())
    }
    /// Classification controls VM poisoning. Every failed render fence settles,
    /// while command memory remains owned until paired retirement or stop.
    pub(crate) fn quarantine(
        &mut self,
        error: Error,
        defer: &mut impl FnMut(Deferred) -> Result,
    ) -> Result {
        if *crate::module_parameters::sptm_space_trace.value() != 0 {
            pr_info!("G17 render pair quarantined: qids={:?} context={} vm={} ordinal={} active={} error={:?} already={}\n",
                self.qids(), self.context().id(), self.context().vm().id(), self.ordinal,
                self.active.len(), error, self.quarantined);
        }
        self.quarantined = true;
        self.drain_logged = false;
        // Successors published after recovery can still fail behind its spared
        // head. Classify the whole failing group before any fence can settle.
        if self.active.iter().any(|active| active.packet.completion.spared()) {
            for active in self.active.iter() {
                active.packet.completion.classify(recovery::Class::Spared);
            }
        }
        for active in self.active.iter_mut() {
            active.packet.completion.quarantine_render();
            if !active.failed {
                defer(Deferred::FailedOwned(
                    active.packet.completion.clone(),
                    error,
                ))?;
                active.failed = true;
                if !active.packet.completion.spared() && self.terminal.is_none() {
                    self.terminal = Some((active.packet.completion.status().clone(), error));
                }
            }
        }
        Ok(())
    }
    pub(crate) fn retire(
        &mut self,
        masks: Option<[u64; 2]>,
        _epoch: u64,
        state: u32,
        host: &mut impl Host,
        defer: &mut impl FnMut(Deferred) -> Result,
    ) -> Result<bool> {
        self.retire_front(masks, state, None, host, defer)
    }

    pub(crate) fn retire_observed(
        &mut self,
        observed: Observed,
        state: u32,
        host: &mut impl Host,
        defer: &mut impl FnMut(Deferred) -> Result,
    ) -> Result<bool> {
        self.retire_front(None, state, Some(observed), host, defer)
    }

    fn retire_configuration(&mut self, host: &impl Host) -> Result<bool> {
        let Some(configuration) = self.configuration.as_ref() else { return Ok(false); };
        if !configuration.consumed(self.memory.graph.cursors()?, host.generation()) { return Ok(false); }
        self.configuration.take().ok_or(EIO)?.release();
        Ok(true)
    }

    fn retire_front(
        &mut self,
        masks: Option<[u64; 2]>,
        state: u32,
        observed: Option<Observed>,
        host: &mut impl Host,
        defer: &mut impl FnMut(Deferred) -> Result,
    ) -> Result<bool> {
        let configuration_retired = state == 0 && self.retire_configuration(host)?;
        if self.quarantined || state != 0 || !self.selected(masks) {
            return Ok(configuration_retired);
        }
        let qids = self.qids();
        let qos = self.qos_owners();
        let pool = self.free_list.id();
        let buffer = self.buffer_id();
        loop {
            let Some(front) = self.active.front_mut() else {
                return Ok(configuration_retired);
            };
            if !front.outer_published {
                return Ok(configuration_retired);
            }
            if !Arc::ptr_eq(&front.packet.context, &self.memory.context)
                || !self.memory.owns(front.ticket.ordinal)
            {
                return Err(EIO);
            }
            let mut observer = Observer {
                memory: &self.memory,
                qids,
                host,
            };
            let result = if let Some(observed) = observed {
                if observed.ticket != front.ticket {
                    return Err(EIO);
                }
                retirement::poll_observed(&mut front.ticket, &mut observer, observed.snapshot)
            } else {
                retirement::poll(&mut front.ticket, &mut observer)
            };
            let timestamps = match result {
                Ok(retirement::Poll::Complete(timestamps)) => timestamps,
                Ok(retirement::Poll::Pending) => return self.replay_abandoned(host, defer),
                Ok(retirement::Poll::Retry) => return Ok(configuration_retired),
                Err(error) => {
                    if crate::g17::m5_render_submission::enabled() {
                        pr_warn!("M5 render retirement rejected: queues={:?} ticket={:?} snapshot={:?} error={:?}\n",
                            qids, front.ticket, retirement::Memory::snapshot(&mut observer, front.ticket.ordinal), error);
                    }
                    self.quarantine(error, defer)?;
                    return Ok(configuration_retired);
                }
            };
            let ordinal = front.ticket.ordinal;
            let context_update = front.context_update;
            self.note_retirement(ordinal, context_update);
            let front = self.active.front_mut().ok_or(EIO)?;
            if !front.flist_completed {
                host.complete_render(
                    (!front.qos_completed).then_some(qos),
                    pool,
                    front.generation,
                    buffer,
                )?;
                front.qos_completed = true;
                front.flist_completed = true;
            }
            defer(Deferred::Retired(
                front.packet.completion.clone(),
                Ok(timestamps),
            ))?;
            self.memory.retire(ordinal)?;
            // Exact retirement: the identity may move once nothing pins it
            // and no command of its VM remains on a physical queue.
            let mut retired = self.active.pop().ok_or(EIO)?;
            retired.activity.end();
            retired.packet.unpin_retired();
            let generation = host.generation();
            if generation != 0 && self.executed_generation.is_some_and(|g| g < generation) {
                // First executed retirement of this pair after a recovery.
                static AFTER_LOGS: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);
                if AFTER_LOGS.fetch_add(1, core::sync::atomic::Ordering::Relaxed) < 32 {
                    pr_info!("G17 render pair slot {} qids {:?} owner {:?} first executed retirement in generation {} ordinal {} pass {:?}\n",
                        self.slot, qids, self.owner, generation, ordinal, timestamps);
                }
            }
            self.executed_generation = Some(generation);
            return Ok(true);
        }
    }

    /// The front group was retired and unlinked by firmware without executing
    /// (job list empty, stamps cover it, no recovery in progress, fragment end
    /// never written). Nothing of this group reached its attachments, so the
    /// same command is published again instead of failing the owner's VM. The
    /// proof is the strict timeout witness, taken twice ABANDON_GRACE_NS apart,
    /// and only for a lone group of an unquarantined pair whose completion is
    /// not guilty and still accepts a replay.
    fn replay_abandoned(
        &mut self,
        host: &mut impl Host,
        defer: &mut impl FnMut(Deferred) -> Result,
    ) -> Result<bool> {
        use retirement::Memory;
        if self.quarantined || self.active.len() != 1 || self.replay.is_some() {
            return Ok(false);
        }
        let front = self.active.front().ok_or(EIO)?;
        // Healthy path: a group published in the current recovery generation
        // takes no extra reads here. Firmware retires groups without executing
        // them across a restart; a scheduler timeout starts the same proof
        // through defer_abandoned_timeout for any other case.
        if !front.outer_published
            || front.failed
            || (front.recovery_generation == host.generation() && front.abandoned_since.is_none())
        {
            return Ok(false);
        }
        let mut observer = Observer {
            memory: &self.memory,
            qids: self.qids(),
            host,
        };
        core::sync::atomic::fence(core::sync::atomic::Ordering::Acquire);
        let snapshot = observer.snapshot(front.ticket.ordinal)?;
        let stamps = observer.stamps()?;
        let (_, state) = observer.host.epoch()?;
        // Only a group whose fragment end record was never written qualifies.
        let abandoned = snapshot.pass[1] == 0
            && retirement::abandoned(&front.ticket, snapshot, stamps, state, false)?;
        let now = crate::g17::now_ns();
        let front = self.active.front_mut().ok_or(EIO)?;
        if !abandoned {
            front.abandoned_since = None;
            return Ok(false);
        }
        let Some(since) = front.abandoned_since else {
            front.abandoned_since = Some(now);
            return Ok(false);
        };
        if now.saturating_sub(since) < ABANDON_GRACE_NS {
            return Ok(false);
        }
        let guilty = front.packet.completion.status().get() != 0
            || front.packet.completion.guilty();
        if crate::debug::debug_enabled(crate::debug::DebugFlags::VerboseFaults)
            || crate::g17::m5_render_submission::enabled()
        {
            pr_info!(
                "render slot {} qids {:?} retired without execution (pass {:?}, stamps {:?}); {}\n",
                self.slot,
                self.qids(),
                snapshot.pass,
                stamps,
                if guilty { "owner is guilty, not replaying" } else { "replaying" }
            );
        }
        let qos = self.qos_owners();
        let buffer = self.buffer_id();
        let pool = self.free_list.id();
        let front = self.active.front_mut().ok_or(EIO)?;
        if !front.flist_completed {
            host.complete_render(
                (!front.qos_completed).then_some(qos),
                pool,
                front.generation,
                buffer,
            )?;
            front.flist_completed = true;
            front.qos_completed = true;
        }
        let ordinal = front.ticket.ordinal;
        let packet = front.packet.clone();
        // Guilty work is never replayed: a VM that already recorded an error
        // takes the same EIO settlement the scheduler timeout would have.
        if guilty || !packet.completion.begin_replay() {
            if !packet.completion.spared() {
                packet.completion.status().record(EIO);
                if self.terminal.is_none() {
                    self.terminal = Some((packet.completion.status().clone(), EIO));
                }
            }
            defer(Deferred::Retired(packet.completion.clone(), Err(EIO)))?;
            self.memory.retire(ordinal)?;
            let mut active = self.active.pop().ok_or(EIO)?;
            active.activity.end();
            active.packet.unpin_retired();
            return Ok(true);
        }
        self.memory.retire(ordinal)?;
        self.active.pop().ok_or(EIO)?;
        packet.reset_publication_for_replay();
        self.replay = Some(packet);
        self.replay_since = now;
        Ok(true)
    }

    /// The pending replay cannot proceed (closed owner, cancellation or a
    /// publication refusal). ECANCELED leaves the VM usable; any other error
    /// is the ordinary device-loss outcome the replay tried to avoid.
    pub(crate) fn fail_replay(
        &mut self,
        error: Error,
        defer: &mut impl FnMut(Deferred) -> Result,
    ) -> Result {
        let Some(packet) = self.replay.as_ref().cloned() else {
            return Ok(());
        };
        if self.active.iter().any(|active| Arc::ptr_eq(&active.packet, &packet)) {
            // Publication may have failed after firmware gained visibility.
            // Keep the active owner and its pins until retirement or stop;
            // only the redundant replay reference can be removed here.
            // Even a refused collector leaves Active as the precise owner.
            // Do not leave a duplicate replay available after Active retires.
            self.replay = None;
            return self.quarantine(error, defer);
        } else {
            // replay_abandoned reset publication only after the exact old
            // retirement witness. A new publication without its active owner
            // is ambiguous: retain this last packet/token owner for stop.
            if packet.is_published() {
                return Err(EIO);
            }
            self.discard_prepared(&packet)?;
            defer(Deferred::Retired(packet.completion.clone(), Err(error)))?;
            packet.unpin_retired();
        }
        self.replay = None;
        Ok(())
    }

    /// A closed preparation epoch invalidates only this unpublished descriptor.
    /// Its completion remains available for a retry under the next epoch.
    pub(crate) fn discard_prepared(&mut self, packet: &Arc<Packet>) -> Result {
        let Some(prepared) = self.prepared.as_ref() else {
            return Ok(());
        };
        if !Arc::ptr_eq(&prepared.packet, packet) {
            return Err(EBUSY);
        }
        self.memory.retire(prepared.ordinal)?;
        self.prepared = None;
        Ok(())
    }
}

impl Pair {
    /// Call before and again after servicing firmware events. FList completion
    /// counters are full-width, including beyond the low-word wrap boundary.
    pub(crate) fn firmware_drained(&self, host: &impl Host) -> Result<bool> {
        if self.prepared.is_some() || self.active.iter().any(|a| !a.outer_published) {
            return Ok(false);
        }
        let counters = self.free_list.counters()?;
        let stamps = host.stamps(self.qids())?;
        let (_, state) = host.epoch()?;
        Ok(retirement::drained(self.ordinal, stamps, counters, state))
    }

    /// Account one witnessed tracker, preserving all completion ownership.
    /// The caller services its pending FList release before accounting another
    /// tracker and invokes release_witnessed only after that control stage.
    pub(crate) fn account_drained(&mut self, host: &mut impl Host) -> Result<bool> {
        use retirement::Memory;
        if !self.firmware_drained(host)? {
            return Ok(false);
        }
        let qids = self.qids();
        let qos = self.qos_owners();
        let buffer = self.buffer_id();
        let Some(active) = self.active.iter_mut().find(|a| !a.flist_completed) else {
            return Ok(false);
        };
        let mut observer = Observer {
            memory: &self.memory,
            qids,
            host,
        };
        let result = match retirement::poll(&mut active.ticket, &mut observer)? {
            retirement::Poll::Complete(ticks) => Ok(ticks),
            retirement::Poll::Pending | retirement::Poll::Retry => {
                core::sync::atomic::fence(core::sync::atomic::Ordering::Acquire);
                let snapshot = observer.snapshot(active.ticket.ordinal)?;
                let stamps = observer.stamps()?;
                let (_, state) = observer.host.epoch()?;
                if !retirement::abandoned(&active.ticket, snapshot, stamps, state, true)? {
                    return Ok(false);
                }
                Err(EIO)
            }
        };
        let ordinal = active.ticket.ordinal;
        let context_update = result.is_ok() && active.context_update;
        self.note_retirement(ordinal, context_update);
        let active = self
            .active
            .iter_mut()
            .find(|active| active.ticket.ordinal == ordinal)
            .ok_or(EIO)?;
        host.complete_render(
            (!active.qos_completed).then_some(qos),
            self.free_list.id(),
            active.generation,
            buffer,
        )?;
        active.qos_completed = true;
        active.flist_completed = true;
        active.drained_result = Some(result);
        Ok(true)
    }

    /// Fresh proof after asynchronous control publication is the ownership
    /// boundary. A refused deferred item leaves the precise owner available.
    pub(crate) fn release_witnessed(
        &mut self,
        host: &impl Host,
        defer: &mut impl FnMut(Deferred) -> Result,
    ) -> Result {
        if self.active.iter().any(|a| !a.flist_completed) || !self.firmware_drained(host)? {
            if self.quarantined && !self.drain_logged
                && *crate::module_parameters::sptm_space_trace.value() != 0
            {
                self.drain_logged = true;
                let stamps = host.stamps(self.qids());
                let epoch = host.epoch();
                pr_info!("G17 quarantined pair not drained: qids={:?} context={} ordinal={} prepared={} active={} unpublished={} flist_pending={} stamps={:x?} free_list={:x?} epoch={:x?}\n",
                    self.qids(), self.context().id(), self.ordinal, self.prepared.is_some(),
                    self.active.len(), self.active.iter().filter(|a| !a.outer_published).count(),
                    self.active.iter().filter(|a| !a.flist_completed).count(),
                    stamps, self.free_list.counters(), epoch);
            }
            return Ok(());
        }
        if self.quarantined && *crate::module_parameters::sptm_space_trace.value() != 0 {
            pr_info!("G17 quarantined pair drained: qids={:?} context={} ordinal={}\n",
                self.qids(), self.context().id(), self.ordinal);
        }
        while let Some(active) = self.active.front() {
            let result = active.drained_result.ok_or(EIO)?;
            defer(Deferred::Drained(active.packet.completion.clone(), result))?;
            let ordinal = active.ticket.ordinal;
            self.memory.retire(ordinal)?;
            let mut active = self.active.pop().ok_or(EIO)?;
            active.activity.end();
            active.packet.unpin_retired();
        }
        self.quarantined = false;
        self.resync();
        Ok(())
    }

    /// The registry first polls the completed prefix through this exact Arc.
    /// With successors, source policy quarantines every remaining group. A
    /// single unexecuted group may use the strict ownership witness instead.
    pub(crate) fn fail_after_observation(
        &mut self,
        packet: &Arc<Packet>,
        error: Error,
        host: &mut impl Host,
        defer: &mut impl FnMut(Deferred) -> Result,
    ) -> Result<bool> {
        use retirement::Memory;
        if !self.active.iter().any(|a| Arc::ptr_eq(&a.packet, packet)) {
            return Ok(false);
        }
        if !self.quarantined && self.active.len() == 1 {
            let active = self.active.front().ok_or(EIO)?;
            let mut observer = Observer {
                memory: &self.memory,
                qids: self.qids(),
                host,
            };
            core::sync::atomic::fence(core::sync::atomic::Ordering::Acquire);
            let snapshot = observer.snapshot(active.ticket.ordinal)?;
            let stamps = observer.stamps()?;
            let (_, state) = observer.host.epoch()?;
            if active.outer_published
                && retirement::abandoned(&active.ticket, snapshot, stamps, state, false)?
            {
                let qos = self.qos_owners();
                let buffer = self.buffer_id();
                let active = self.active.front_mut().ok_or(EIO)?;
                if !active.flist_completed {
                    host.complete_render(
                        (!active.qos_completed).then_some(qos),
                        self.free_list.id(),
                        active.generation,
                        buffer,
                    )?;
                    active.flist_completed = true;
                    active.qos_completed = true;
                }
                if !active.failed && !active.packet.completion.spared() {
                    active.packet.completion.status().record(EIO);
                    if self.terminal.is_none() {
                        self.terminal = Some((active.packet.completion.status().clone(), error));
                    }
                }
                defer(Deferred::Retired(
                    active.packet.completion.clone(),
                    Err(EIO),
                ))?;
                let ordinal = active.ticket.ordinal;
                self.memory.retire(ordinal)?;
                let mut active = self.active.pop().ok_or(EIO)?;
                active.activity.end();
                active.packet.unpin_retired();
                return Ok(true);
            }
        }
        self.quarantine(error, defer)?;
        Ok(true)
    }

    /// Both processors have stopped before the caller reaches this method.
    /// VM ownership settles before quarantined admission is rehabilitated.
    pub(crate) fn stopped(&mut self, defer: &mut impl FnMut(Deferred) -> Result) -> Result {
        if let Some(configuration) = self.configuration.take() { configuration.release(); }
        if let Some(prepared) = self.prepared.as_ref() {
            defer(Deferred::Unpublished(
                prepared.packet.completion.clone(),
                EIO,
            ))?;
            self.memory.retire(prepared.ordinal)?;
            let prepared = self.prepared.take().ok_or(EIO)?;
            prepared.packet.unpin_retired();
        }
        while let Some(active) = self.active.front() {
            defer(Deferred::Drained(
                active.packet.completion.clone(),
                Err(EIO),
            ))?;
            let ordinal = active.ticket.ordinal;
            self.memory.retire(ordinal)?;
            let mut active = self.active.pop().ok_or(EIO)?;
            active.activity.end();
            active.packet.unpin_retired();
        }
        if let Some(packet) = self.replay.as_ref() {
            defer(Deferred::Drained(packet.completion.clone(), Err(EIO)))?;
            let packet = self.replay.take().ok_or(EIO)?;
            packet.unpin_retired();
        }
        if let Some(previous) = self.previous.as_mut() {
            previous.ready = true;
        }
        Ok(())
    }
}

impl Pair {
    /// Commit a cold graph only after the registry revalidates its preparation
    /// lease. The original PB row is restored on every later failure.
    pub(crate) fn finish_install(
        &mut self,
        alloc: &Allocator<'_>,
        descriptors: &crate::g17::object::KernelObject,
    ) -> Result {
        install_memory(
            alloc,
            descriptors,
            self.slot,
            &mut self.memory,
            &mut self.pending_aliases,
            &mut self.kick_aliases,
        )
    }
}

/// vmfix6f diagnostic (m5_render_rebind_legacy): restore one pre-6f
/// same-VM rebind behaviour for attribution. Default 0 (all native-aligned).
pub(crate) const LEGACY_UPDATE_BITS: u32 = 1 << 0;
pub(crate) const LEGACY_RECORD_WRITE: u32 = 1 << 1;
pub(crate) const LEGACY_IN_PLACE_RETARGET: u32 = 1 << 2;
pub(crate) const LEGACY_UPDATE_UNTIL_RETIRED: u32 = 1 << 3;
pub(crate) fn rebind_legacy(bit: u32) -> bool {
    *crate::module_parameters::m5_render_rebind_legacy.value() & bit != 0
}

fn install_memory(
    alloc: &Allocator<'_>,
    descriptors: &crate::g17::object::KernelObject,
    slot: u8,
    memory: &mut memory::Memory,
    pending_aliases: &mut Option<KVec<mmu::KernelMapping>>,
    kick_aliases: &mut Option<[Option<mmu::KernelMapping>; 2]>,
) -> Result {
    use core::sync::atomic::{fence, Ordering};
    if pending_aliases.is_none() && kick_aliases.is_none() {
        return Ok(());
    }
    if !memory.context.is_current() || kick_aliases.is_none() {
        return Err(EIO);
    }
    let mut driver = KVec::new();
    driver.reserve(2, GFP_KERNEL)?;
    let aliases = memory.take_persistent(pending_aliases.as_mut().ok_or(EIO)?)?;
    let context = memory.context.clone();
    let vm = context.vm();
    let pool_id = u64::from(memory.lifecycle[0].predecessor);
    let mut staged: Option<[u32; 4]> = None;
    let offset = usize::from(memory.manager.buffer_id()) * 16;
    let result: Result = (|| {
        vm.install_render_pool_mappings(slot, pool_id, memory.manager.blocks(), aliases)?;
        memory.installed_pool = Some(slot);
        alloc.uat.flush_vm_contexts(vm)?;
        let words = [
            descriptors.word(offset)?,
            descriptors.word(offset + 4)?,
            descriptors.word(offset + 8)?,
            descriptors.word(offset + 12)?,
        ];
        let previous = words.map(|word| word.load(Ordering::Relaxed));
        let pages = memory.manager.page_count()?;
        let next = crate::g17::fw::buffer::reload_descriptor(
            previous,
            memory.manager.page_list_va(),
            pages,
        );
        for (word, value) in words.into_iter().zip(next) {
            word.store(value, Ordering::Relaxed);
        }
        fence(Ordering::SeqCst);
        staged = Some(previous);
        for mapping in kick_aliases.take().ok_or(EIO)?.into_iter().flatten() {
            driver.push(mapping, GFP_KERNEL)?;
        }
        vm.append_driver_mappings(driver)?;
        Ok(())
    })();
    if let Err(error) = result {
        if error == ENOENT {
            context.status().record(error);
        }
        if let Some(previous) = staged {
            for (index, value) in previous.into_iter().enumerate() {
                descriptors
                    .word(offset + index * 4)?
                    .store(value, Ordering::Relaxed);
            }
            fence(Ordering::SeqCst);
        }
        vm.clear_render_pool_mappings_if_owner(slot, pool_id);
        memory.installed_pool = None;
        return Err(error);
    }
    *pending_aliases = None;
    Ok(())
}

/// A binding prepared while the registry exclusively lends the idle pair.
/// Its frontier prevents a stale off-lock result from replacing another owner.
pub(crate) struct PreparedBinding {
    previous_context: Arc<Context>,
    previous_owner: Option<u64>,
    ordinal: u64,
    owner: u64,
    context: Arc<Context>,
    pool: Arc<RenderPool>,
    memory: Option<KBox<memory::Memory>>,
    aliases: Option<KVec<mmu::KernelMapping>>,
    kicks: Option<[Option<mmu::KernelMapping>; 2]>,
    initial_packet: u64,
}

/// The registry retains displaced backing until this exact context's release
/// is acknowledged. A warm binding retains the old context without its graph.
/// Kick rings for a fresh pair: a recycled QID's spare ring when one was taken
/// for it, otherwise a new ring. On failure every adopted ring goes back to
/// `channels`, so the caller can return it to the QID table.
#[inline(never)]
fn adopt_render_kicks(
    alloc: &Allocator<'_>,
    ids: [kick::Id; 2],
    vm: &crate::mmu::Vm,
    channels: &mut Option<KBox<[Option<kick::Queue>; 2]>>,
) -> Result<[(kick::Queue, crate::mmu::KernelMapping); 2]> {
    let mut adopted = [false; 2];
    let mut first: Option<(kick::Queue, crate::mmu::KernelMapping)> = None;
    for lane in 0..2 {
        let spare = channels.as_mut().and_then(|spares| spares[lane].take());
        let result = match spare {
            Some(mut queue) if queue.id() == ids[lane] => match queue.adopt_client(vm) {
                Ok(alias) => {
                    adopted[lane] = true;
                    Ok((queue, alias))
                }
                Err(error) => {
                    if let Some(spares) = channels.as_mut() {
                        spares[lane] = Some(queue);
                    }
                    Err(error)
                }
            },
            Some(queue) => {
                if let Some(spares) = channels.as_mut() {
                    spares[lane] = Some(queue);
                }
                Err(EINVAL)
            }
            None => kick::Queue::render(alloc, ids[lane], vm),
        };
        match result {
            Ok(value) if lane == 0 => first = Some(value),
            Ok(value) => return Ok([first.ok_or(EIO)?, value]),
            Err(error) => {
                if let Some((queue, alias)) = first.take() {
                    drop(alias);
                    if adopted[0] {
                        if let Some(spares) = channels.as_mut() {
                            spares[0] = Some(queue);
                        }
                    }
                }
                return Err(error);
            }
        }
    }
    Err(EIO)
}

pub(crate) struct RetiredBinding {
    memory: Option<KBox<memory::Memory>>,
    context: Arc<Context>,
    pool: Arc<RenderPool>,
    buffer_id: u8,
    publication: Option<u64>,
    ready: bool,
    wait_for_pool_release: bool,
    /// vmfix6d: the QIDs of the physical pair this binding was retired from.
    /// Its graph (when retained) holds exactly these two queue records, and its
    /// commands, configurations and completion links name only these rings;
    /// its context last published through them.
    qids: [u8; 2],
    /// vmfix6e: the pair ordinal at displacement. Once the QID stamps cover
    /// it, a later binding's first publication on these rings has retired.
    displaced_at: Option<u64>,
}
impl RetiredBinding {
    pub(crate) fn qids(&self) -> [u8; 2] {
        self.qids
    }
    /// vmfix6e eager collection gate: a retained graph (own memory and PB
    /// identity) whose rings have retired a later binding's publication and
    /// whose own state is drained and unlinked (Memory::collectable).
    pub(crate) fn collectable(&self, stamps: [u32; 2]) -> Result<bool> {
        let (Some(memory), Some(at)) = (self.memory.as_ref(), self.displaced_at) else {
            return Ok(false);
        };
        let Some(next) = at.checked_add(1) else {
            return Ok(false);
        };
        if !stamps.into_iter().all(|stamp| retirement::stamp_covers(stamp, next)) {
            return Ok(false);
        }
        memory.collectable()
    }
    pub(crate) fn poison(&mut self) {
        if let Some(memory) = self.memory.as_mut() {
            memory.poison();
        }
    }
    pub(crate) fn context(&self) -> &Arc<Context> {
        &self.context
    }
    pub(crate) fn pool(&self) -> &Arc<RenderPool> {
        &self.pool
    }
    pub(crate) fn wait_for_pool_release(&self) -> bool { self.wait_for_pool_release }
    pub(crate) fn buffer_id(&self) -> Option<u8> {
        self.memory.as_ref().map(|_| self.buffer_id)
    }
    /// vmfix6f: a retained graph that still owns its VM pool mappings.
    pub(crate) fn restorable_in_place(&self) -> bool {
        self.memory.as_ref().is_some_and(|memory| memory.has_pool_mappings())
    }
}

impl Pair {
    fn note_retirement(&mut self, ordinal: u64, context_update: bool) {
        if !context_update {
            return;
        }
        self.context_update = false;
        self.identity_update = false;
        if let Some(previous) = self.previous.as_mut() {
            if previous
                .publication
                .is_some_and(|published| published <= ordinal)
            {
                previous.ready = true;
            }
        }
    }
    pub(crate) fn take_retired_binding(&mut self) -> Option<RetiredBinding> {
        if self.displaced.is_some() {
            return self.displaced.take();
        }
        if self.previous.as_ref().is_some_and(|p| p.ready) {
            self.previous.take()
        } else {
            None
        }
    }
    pub(crate) fn idle_for_context(&self, context: &Arc<Context>) -> Result<bool> {
        if !Arc::ptr_eq(context, self.context()) {
            return Ok(true);
        }
        if self.prepared.is_some() || self.in_flight() || self.quarantined {
            return Ok(false);
        }
        if self.ordinal == 0 {
            return Ok(true);
        }
        core::sync::atomic::fence(core::sync::atomic::Ordering::Acquire);
        Ok(self
            .memory
            .graph
            .cursors()?
            .into_iter()
            .all(|c| c.done == c.read && c.read == c.write))
    }
    /// Everything `reusable` requires except a released owner: the firmware
    /// consumed all of this pair's work and no earlier rebind is pending.
    pub(crate) fn lendable(&self) -> Result<bool> {
        let previous_ready = self
            .previous
            .as_ref()
            .is_none_or(|p| p.memory.is_none() && self.context_update && !p.ready);
        Ok(self.owner.is_some()
            && self.displaced.is_none()
            && previous_ready
            && self.idle_for_context(self.context())?)
    }
    pub(crate) fn reusable(&self) -> Result<bool> {
        let previous_ready = self
            .previous
            .as_ref()
            .is_none_or(|p| p.memory.is_none() && self.context_update && !p.ready);
        Ok(self.owner.is_none()
            && self.displaced.is_none()
            && previous_ready
            && self.idle_for_context(self.context())?)
    }
    /// An M5 pool may grow without changing its live logical owner. Commands
    /// already using this pair must drain, and the previous replacement must
    /// retire before another generation is installed.
    ///
    /// vmfix6d: unless nothing was published since that replacement. Then the
    /// current binding never reached firmware (no publication carried its
    /// pool), so it is superseded directly: bind_owner keeps the unpublished
    /// `previous` (the last binding firmware saw) and drops the current one.
    /// Requiring `previous.is_none()` here deadlocked a pair whose rebind was
    /// never followed by a publication (sibling warm-up, failed publication):
    /// only a publication on that pair can retire `previous`, and every packet
    /// that needs another pool geometry there waits for this rebind.
    pub(crate) fn bindable(&self, owner: u64, context: &Arc<Context>) -> Result<bool> {
        if self.memory.graph.is_m5() && self.owner == Some(owner)
            && Arc::ptr_eq(context, self.context()) {
            return Ok(self.displaced.is_none() && self.previous_supersedable()
                && self.idle_for_context(context)?);
        }
        self.reusable()
    }
    /// vmfix6d eager restore: whether `binding`, a graph of another VM retained
    /// after it was displaced from this pair, can be reinstalled now. The
    /// current (successor) binding must have released its owner and fully
    /// retired: no host tracker, no preparation, the QID stamps covering its
    /// last publication and its pointer blocks drained. The retained graph
    /// must be drained and own no ordinal. Returns its write cursors.
    pub(crate) fn restorable(&self, binding: &RetiredBinding, stamps: [u32; 2]) -> Result<Option<[u32; 2]>> {
        if binding.qids != self.qids()
            || binding.memory.is_none()
            || self.owner.is_some()
            || self.in_flight()
            || self.prepared.is_some()
            || self.quarantined
            || self.terminal.is_some()
            || self.displaced.is_some()
            || self.pending_aliases.is_some()
            || self.kick_aliases.is_some()
            || self.previous.as_ref().is_some_and(|p| p.memory.is_some())
        {
            return Ok(None);
        }
        if self.ordinal != 0
            && !stamps.into_iter().all(|stamp| retirement::stamp_covers(stamp, self.ordinal))
        {
            return Ok(None);
        }
        if !self.idle_for_context(self.context())? {
            return Ok(None);
        }
        restorable_graph(binding)
    }

    /// Reinstall `binding` (checked by restorable() under the same device
    /// mutex) and return the successor's binding, retained in turn exactly as
    /// a foreign rebind displaces it. Like a foreign bind, the first
    /// publication afterwards carries fresh channel configurations (TilingBinding
    /// with the restored graph's PB state, slot and token, the free list and
    /// the scheduler page) and a completion seed (context_update). The pair is
    /// left without a logical owner; the normal same-VM rebind then binds it.
    pub(crate) fn restore(&mut self, mut binding: RetiredBinding, cursors: [u32; 2]) -> RetiredBinding {
        let qids = self.qids();
        let successor = RetiredBinding {
            memory: None,
            context: self.context().clone(),
            pool: self.free_list.clone(),
            buffer_id: self.buffer_id(),
            publication: None,
            ready: true,
            wait_for_pool_release: false,
            qids,
            displaced_at: Some(self.ordinal),
        };
        let mut successor = successor;
        if let Some(mut memory) = binding.memory.take() {
            memory.clear_stale_claims();
            successor.memory = Some(core::mem::replace(&mut self.memory, memory));
            successor.pool = core::mem::replace(&mut self.free_list, binding.pool.clone());
        }
        self.owner = None;
        self.cursors = cursors;
        self.configs = [None; 2];
        self.context_update = true;
        self.identity_update = true;
        self.note_binding(4);
        successor
    }

    /// No `previous`, or one that nothing has been published after: a warm
    /// (memoryless) replacement with no publication ordinal and not ready.
    fn previous_supersedable(&self) -> bool {
        self.previous.as_ref().is_none_or(|p| {
            p.memory.is_none() && p.publication.is_none() && !p.ready && self.context_update
        })
    }

    /// Logical close leaves the physical channel identity and backing retained.
    pub(crate) fn release_owner(&mut self) {
        self.owner = None;
    }
    pub(crate) fn kick_ids(&self) -> [kick::Id; 2] {
        self.kicks.each_ref().map(kick::Queue::id)
    }

    /// A fresh pair that failed before any installation or publication gives
    /// back its rings (vmfix6: adopted spare rings must never be dropped).
    /// Client aliases of the rings and the graph memory go first.
    #[inline(never)]
    pub(crate) fn into_unused_kicks(pair: KBox<Self>) -> Option<KBox<[Option<kick::Queue>; 2]>> {
        if pair.installed != [false; 2] || pair.outer_started != 0 || pair.prepared.is_some() {
            return None;
        }
        let mut out = KBox::new([None, None], GFP_KERNEL).ok()?;
        let Pair { kick_aliases, pending_aliases, memory, kicks, .. } = KBox::into_inner(pair);
        drop(kick_aliases);
        drop(pending_aliases);
        drop(memory);
        let [tiling, fragment] = kicks;
        out[0] = Some(tiling);
        out[1] = Some(fragment);
        Some(out)
    }
    #[inline(never)]
    pub(crate) fn prepare_bind(
        &mut self,
        alloc: &Allocator<'_>,
        pool: &Arc<Pool>,
        owner: u64,
        context: Arc<Context>,
        free_list: Arc<RenderPool>,
        metrics: Option<buffer::MetricsLease>,
        buffer_id: Option<(u8, u64)>,
        pm_generation: Option<u64>,
        first: &Arc<Packet>,
    ) -> Result<PreparedBinding> {
        if !self.bindable(owner, &context)?
            || owner == 0
            || !Arc::ptr_eq(&context, &first.context)
            || !context.is_current()
        {
            return Err(EBUSY);
        }
        alloc.uat.vm_context_mask(context.vm())?;
        // vmfix6f: only the same logical context (a pool-geometry rebind)
        // keeps the graph; any other context gets its own graph and queue
        // records, like native, which never retargets a live queue record.
        let keep_graph = if rebind_legacy(LEGACY_IN_PLACE_RETARGET) {
            Arc::ptr_eq(self.context().status(), context.status())
        } else {
            Arc::ptr_eq(self.context(), &context)
        };
        let (memory, aliases, kicks) = if keep_graph {
            if metrics.is_some() || buffer_id.is_some() || pm_generation.is_some() {
                return Err(EINVAL);
            }
            (None, None, None)
        } else {
            let metrics = metrics.ok_or(EINVAL)?;
            let buffer_id = buffer_id.ok_or(EINVAL)?;
            let pm_generation = pm_generation.ok_or(EINVAL)?;
            let Validated::Render { pass, .. } = &first.command else {
                return Err(EINVAL);
            };
            let layout = crate::g17::m5_render_submission::layout(&first.command, self.clusters)?;
            let aliases = [
                self.kicks[0].prepare_client_alias(context.vm())?,
                self.kicks[1].prepare_client_alias(context.vm())?,
            ];
            let (memory, persistent) = memory::Memory::new(
                alloc,
                pool,
                context.clone(),
                self.qids(),
                &layout,
                metrics,
                buffer_id,
                pm_generation,
                self.ordinal,
                first.completion.work_state()?.word(),
            )?;
            (Some(memory), Some(persistent), Some(aliases))
        };
        Ok(PreparedBinding {
            previous_context: self.context().clone(),
            previous_owner: self.owner,
            ordinal: self.ordinal,
            owner,
            context,
            pool: free_list,
            memory,
            aliases,
            kicks,
            initial_packet: first.id,
        })
    }
    pub(crate) fn bind_owner(
        &mut self,
        binding: &mut PreparedBinding,
        alloc: &Allocator<'_>,
        descriptors: &crate::g17::object::KernelObject,
    ) -> Result {
        if !self.bindable(binding.owner, &binding.context)?
            || binding.ordinal != self.ordinal
            || binding.previous_owner != self.owner
            || !Arc::ptr_eq(&binding.previous_context, self.context())
            || !binding.context.is_current()
        {
            return Err(EBUSY);
        }
        let foreign = binding.memory.is_some();
        let same_context = Arc::ptr_eq(self.context(), &binding.context);
        if let Some(memory) = binding.memory.as_mut() {
            // vmfix6f: a new context of the same VM gets its own graph (queue
            // records), never an in-place owner retarget. The VM holds one
            // graph's pool mappings per slot: move the drained outgoing
            // graph's out first, and back if the install fails.
            let same_vm = Arc::ptr_eq(self.context().status(), binding.context.status());
            let moved = if same_vm { self.memory.take_pool_mappings() } else { None };
            let installed = install_memory(
                alloc,
                descriptors,
                self.slot,
                memory,
                &mut binding.aliases,
                &mut binding.kicks,
            );
            if let Err(error) = installed {
                if let Some((slot, mappings)) = moved {
                    self.memory.put_pool_mappings(slot, mappings)?;
                }
                return Err(error);
            }
            drop(moved);
        } else if rebind_legacy(LEGACY_RECORD_WRITE) || !same_context {
            // Same graph, another logical context (only with
            // LEGACY_IN_PLACE_RETARGET), or the opt-in legacy rewrite.
            self.memory.set_owner(self.qids(), &binding.context, true)?;
        } else {
            // vmfix6f: the same logical context keeps its queue records.
            // Their host-owned words already name it; the old unconditional
            // read-modify-write stored the whole 0x80-byte record back,
            // including firmware-owned words, while the previous kick could
            // still be executing (drained pointer blocks precede completion).
            self.memory.set_owner(self.qids(), &binding.context, false)?;
        }
        let wait_for_pool_release = self.memory.graph.is_m5()
            && Arc::ptr_eq(self.context(), &binding.context)
            && !Arc::ptr_eq(&self.free_list, &binding.pool);
        let old_context = self.context().clone();
        let old_buffer = self.buffer_id();
        let old_pool = core::mem::replace(&mut self.free_list, binding.pool.clone());
        let old_memory = binding
            .memory
            .take()
            .map(|memory| core::mem::replace(&mut self.memory, memory));
        let retired = RetiredBinding {
            memory: old_memory,
            context: old_context,
            pool: old_pool,
            buffer_id: old_buffer,
            publication: None,
            ready: foreign,
            wait_for_pool_release,
            qids: self.qids(),
            displaced_at: foreign.then_some(self.ordinal),
        };
        if foreign {
            self.displaced = Some(retired);
        } else if self.previous.is_none() {
            self.previous = Some(retired);
        } else {
            // vmfix6d: `previous` is unpublished (bindable checked it), so the
            // binding being replaced was never named by any publication: it
            // is dropped here and `previous` stays the last one firmware saw.
            drop(retired);
        }
        self.memory.context = binding.context.clone();
        self.owner = Some(binding.owner);
        self.initial_packet = binding.initial_packet;
        self.context_update = true;
        if foreign || !same_context || rebind_legacy(LEGACY_UPDATE_BITS) {
            self.identity_update = true;
        }
        self.note_binding(if foreign { 3 } else { 2 });
        if foreign {
            self.cursors = [0; 2];
            self.configs = [None; 2];
        }
        Ok(())
    }
}
