// SPDX-License-Identifier: GPL-2.0-only OR MIT

//! Installed kick storage retained after a logical scheduler release is acknowledged.

use super::{install_memory, memory, target, Pair, RetiredBinding, Tracker};
use crate::g17::{
    buffer,
    command::Validated,
    context::{Context, SchedulerStorage},
    freelist::RenderPool,
    job::Packet,
    kick,
    object::{Allocator, KernelObject, Pool},
};
use crate::mmu;
use kernel::{prelude::*, sync::Arc};

/// The empty physical pair retains installed addresses and the last scheduler page,
/// without retaining that scheduler's execution context or client root. A prior
/// context awaiting an executed replacement keeps its separate retirement owner.
pub(crate) struct ParkedPair {
    kicks: Option<[kick::Queue; 2]>,
    _scheduler: Arc<SchedulerStorage>,
    ids: [kick::Id; 2],
    slot: u8,
    ordinal: u64,
    clusters: u32,
    descriptor_flags: [u32; 2],
    installed: [bool; 2],
    recovery_generation: u64,
    registered: Option<u8>,
    outer_started: u8,
    previous: Option<RetiredBinding>,
}

/// All new graph ownership stays separate from the installed kick storage until commit.
pub(crate) struct Prepared {
    memory: Option<KBox<memory::Memory>>,
    pool: Arc<RenderPool>,
    aliases: Option<KVec<mmu::KernelMapping>>,
    kicks: Option<[Option<mmu::KernelMapping>; 2]>,
    ids: [kick::Id; 2],
    slot: u8,
    ordinal: u64,
    owner: u64,
    initial_packet: u64,
    installed: bool,
}

impl Pair {
    /// The registry also checks the shared free-list release and scheduler-release witness.
    /// Drain displaced owners before this preflight; they have their own logical releases.
    pub(crate) fn can_park(&self, context: &Arc<Context>) -> Result<bool> {
        Ok(Arc::ptr_eq(context, self.context())
            && self.owner.is_none()
            && self
                .previous
                .as_ref()
                .is_none_or(|previous| previous.memory.is_none() && !previous.ready)
            && self.displaced.is_none()
            && self.pending_aliases.is_none()
            && self.kick_aliases.is_none()
            && self.idle_for_context(context)?)
    }

    /// Consumes the pair only after can_park and all collector capacity checks succeeded
    /// under the same device mutex. Moving these owners performs no firmware operation.
    pub(crate) fn park(self) -> (ParkedPair, RetiredBinding) {
        let qids = self.qids();
        let context = self.memory.context.clone();
        let scheduler = context.scheduler_owner();
        let buffer_id = self.buffer_id();
        let parked = ParkedPair {
            ids: self.kicks.each_ref().map(|queue| queue.id()),
            kicks: Some(self.kicks),
            _scheduler: scheduler,
            slot: self.slot,
            ordinal: self.ordinal,
            clusters: self.clusters,
            descriptor_flags: self.descriptor_flags,
            installed: self.installed,
            recovery_generation: self.recovery_generation,
            registered: self.registered,
            outer_started: self.outer_started,
            previous: self.previous,
        };
        let retired = RetiredBinding {
            memory: Some(self.memory),
            context,
            pool: self.free_list,
            buffer_id,
            publication: None,
            ready: true,
            wait_for_pool_release: false,
            qids,
            displaced_at: None,
        };
        (parked, retired)
    }
}

impl ParkedPair {
    pub(crate) fn qids(&self) -> [u8; 2] {
        self.ids.map(kick::Id::qid)
    }

    /// Both rings are present and no prior owner awaits an executed replacement.
    pub(crate) fn recyclable(&self) -> bool {
        self.kicks.is_some() && self.previous.is_none()
    }

    /// The rings' last committed timestamps, by lane.
    pub(crate) fn high_water(&self) -> Option<[(u8, u64); 2]> {
        let kicks = self.kicks.as_ref()?;
        Some([0, 1].map(|lane| (kicks[lane].id().qid(), kicks[lane].high_water().get())))
    }

    /// Dismantles the parked pair for QID recycling (vmfix6): the rings go back
    /// to the QID table; the last scheduler page stays retained by the caller
    /// until the next owner of each QID installs its own.
    pub(crate) fn into_recycled(mut self) -> Option<([kick::Queue; 2], Arc<SchedulerStorage>)> {
        let kicks = self.kicks.take()?;
        Some((kicks, self._scheduler))
    }

    #[inline(never)]
    pub(crate) fn prepare(
        &self,
        alloc: &Allocator<'_>,
        pool: &Arc<Pool>,
        owner: u64,
        context: Arc<Context>,
        free_list: Arc<RenderPool>,
        metrics: buffer::MetricsLease,
        buffer_id: (u8, u64),
        pm_generation: u64,
        first: &Arc<Packet>,
    ) -> Result<Prepared> {
        if owner == 0 || !Arc::ptr_eq(&context, &first.context) || !context.is_current() {
            return Err(EINVAL);
        }
        let kicks = self.kicks.as_ref().ok_or(EBUSY)?;
        let Validated::Render { pass, .. } = &first.command else {
            return Err(EINVAL);
        };
        let word = first.completion.work_state()?.word();
        if word == u32::MAX {
            return Err(EINVAL);
        }
        alloc.uat.vm_context_mask(context.vm())?;
        let layout = crate::g17::m5_render_submission::layout(&first.command, self.clusters)?;
        let aliases = [
            kicks[0].prepare_client_alias(context.vm())?,
            kicks[1].prepare_client_alias(context.vm())?,
        ];
        let (memory, persistent) = memory::Memory::new(
            alloc,
            pool,
            context,
            self.qids(),
            &layout,
            metrics,
            buffer_id,
            pm_generation,
            self.ordinal,
            word,
        )?;
        Ok(Prepared {
            memory: Some(memory),
            pool: free_list,
            aliases: Some(persistent),
            kicks: Some(aliases),
            ids: self.ids,
            slot: self.slot,
            ordinal: self.ordinal,
            owner,
            initial_packet: first.id,
            installed: false,
        })
    }

    fn matches(&self, prepared: &Prepared) -> bool {
        self.kicks.is_some()
            && self.ids == prepared.ids
            && self.slot == prepared.slot
            && self.ordinal == prepared.ordinal
            && prepared
                .memory
                .as_ref()
                .is_some_and(|memory| memory.context.is_current())
    }

    /// All fallible graph installation precedes taking the installed kicks. Keep the empty
    /// parked owner until the caller drops it off-lock, including its old scheduler page.
    pub(crate) fn commit(
        &mut self,
        prepared: &mut Option<Prepared>,
        storage: KBox<core::mem::MaybeUninit<Pair>>,
    ) -> Result<KBox<Pair>> {
        let candidate = prepared.as_ref().ok_or(EINVAL)?;
        if !candidate.installed || !self.matches(candidate) {
            return Err(EBUSY);
        }
        let kicks = self.kicks.take().ok_or(EIO)?;
        // The candidate was validated above and cannot change through the exclusive borrow.
        let Some(prepared) = prepared.take() else {
            self.kicks = Some(kicks);
            return Err(EIO);
        };
        let mut next = prepared;
        let Some(memory) = next.memory.take() else {
            self.kicks = Some(kicks);
            return Err(EIO);
        };
        let first_owner = next.owner as u32;
        storage.write_init(kernel::try_init!(Pair {
            kick_aliases: None,
            memory,
            kicks,
            free_list: next.pool.clone(),
            pending_aliases: None,
            prepared: None,
            active: Tracker::new(),
            slot: self.slot,
            owner: Some(next.owner),
            ordinal: self.ordinal,
            initial_packet: next.initial_packet,
            clusters: self.clusters,
            descriptor_flags: self.descriptor_flags,
            installed: self.installed,
            configs: [None; 2],
            cursors: [0; 2],
            recovery_generation: self.recovery_generation,
            registered: self.registered,
            outer_started: self.outer_started,
            quarantined: false,
            drain_logged: false,
            terminal: None,
            previous: self.previous.take(),
            displaced: None,
            context_update: true,
            identity_update: true,
            configuration: None,
            configured_context: None,
            configured_qos: None,
            history: [(1, self.ordinal, first_owner); 4],
            last_pass: None,
            replay: None,
            replay_since: 0,
            executed_generation: None,
        }))
    }
}

impl ParkedPair {
    /// vmfix6d eager restore into a parked pair: whether `binding` (a graph
    /// retained after it was displaced from this pair) can be reinstalled.
    /// Parking proves the successor drained and was released; the stamps must
    /// still cover its last publication. Returns the graph's write cursors.
    pub(crate) fn restorable(&self, binding: &RetiredBinding, stamps: [u32; 2]) -> Result<Option<[u32; 2]>> {
        if self.kicks.is_none()
            || binding.qids != self.qids()
            || self.previous.as_ref().is_some_and(|p| p.memory.is_some())
        {
            return Ok(None);
        }
        if self.ordinal != 0
            && !stamps
                .into_iter()
                .all(|stamp| super::retirement::stamp_covers(stamp, self.ordinal))
        {
            return Ok(None);
        }
        super::restorable_graph(binding)
    }

    /// Rebuild the installed pair around the retained graph (checked by
    /// restorable() under the same device mutex). The rings, registration and
    /// ordinal continue; channel configurations are reset as for a foreign
    /// bind, so the first publication carries the restored graph's PB binding
    /// and a completion seed. The emptied parked owner (with its last
    /// scheduler page) stays with the caller, who drops it off-lock. On
    /// failure the binding is returned untouched.
    pub(crate) fn restore(
        &mut self,
        mut binding: RetiredBinding,
        cursors: [u32; 2],
        storage: KBox<core::mem::MaybeUninit<Pair>>,
    ) -> core::result::Result<KBox<Pair>, RetiredBinding> {
        if self.kicks.is_none() || binding.memory.is_none() || binding.qids != self.qids() {
            return Err(binding);
        }
        let (Some(kicks), Some(mut memory)) = (self.kicks.take(), binding.memory.take()) else {
            return Err(binding);
        };
        memory.clear_stale_claims();
        let pool = binding.pool.clone();
        let result = storage.write_init(kernel::try_init!(Pair {
            kick_aliases: None,
            memory,
            kicks,
            free_list: pool,
            pending_aliases: None,
            prepared: None,
            active: Tracker::new(),
            slot: self.slot,
            owner: None,
            ordinal: self.ordinal,
            initial_packet: 0,
            clusters: self.clusters,
            descriptor_flags: self.descriptor_flags,
            installed: self.installed,
            configs: [None; 2],
            cursors,
            recovery_generation: self.recovery_generation,
            registered: self.registered,
            outer_started: self.outer_started,
            quarantined: false,
            drain_logged: false,
            terminal: None,
            previous: self.previous.take(),
            displaced: None,
            context_update: true,
            identity_update: true,
            configuration: None,
            configured_context: None,
            configured_qos: None,
            history: [(5, self.ordinal, 0); 4],
            last_pass: None,
            replay: None,
            replay_since: 0,
            executed_generation: None,
        }));
        match result {
            Ok(pair) => Ok(pair),
            // try_init! of plain moves cannot fail; keep the graph if it did.
            Err(_) => Err(binding),
        }
    }
}

impl Prepared {
    /// Called after the registry revalidates its preparation epoch and parked pair identity.
    pub(crate) fn finish_install(
        &mut self,
        parked: &ParkedPair,
        alloc: &Allocator<'_>,
        descriptors: &KernelObject,
    ) -> Result {
        if !parked.matches(self) {
            return Err(EBUSY);
        }
        install_memory(
            alloc,
            descriptors,
            self.slot,
            self.memory.as_mut().ok_or(EIO)?,
            &mut self.aliases,
            &mut self.kicks,
        )?;
        self.installed = true;
        Ok(())
    }
}
