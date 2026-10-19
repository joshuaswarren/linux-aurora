// SPDX-License-Identifier: GPL-2.0-only OR MIT

//! Read-only cursor and timestamp snapshots while a physical pair is borrowed.
//! Final stamps, timestamp refresh and ownership changes belong to commit.

use super::{memory, retirement, Pair};
use crate::g17::job::Packet;
use core::sync::atomic::{fence, Ordering};
use kernel::{prelude::*, sync::Arc};

pub(crate) struct Observed {
    pub(super) ticket: retirement::Ticket,
    pub(super) snapshot: retirement::Snapshot,
}

/// Observations taken during one loan, in front-to-back order: the front (complete or not,
/// as observe_front() returns it) and then each following active render while it and every
/// render before it are complete.
pub(crate) struct ObservedRun {
    items: [Option<Observed>; OBSERVED_RUN_MAX],
    count: usize,
}

const OBSERVED_RUN_MAX: usize = crate::hw::t8140::queues::RENDER_DEPTH as usize;

impl ObservedRun {
    pub(crate) fn into_iter(self) -> impl Iterator<Item = Observed> {
        self.items.into_iter().take(self.count).flatten()
    }
}

impl Pair {
    /// Reject incomplete event/stamp prefixes before lending the graph. This
    /// is only a prefilter; commit repeats the final stamp and timestamp reads.
    pub(crate) fn observation_packet(
        &self,
        masks: [u64; 2],
        stamps: [u32; 2],
        state: u32,
    ) -> Result<Option<Arc<Packet>>> {
        if self.quarantined || self.prepared.is_some() || state != 0 || !self.selected(Some(masks))
        {
            return Ok(None);
        }
        let Some(front) = self.active.front() else {
            return Ok(None);
        };
        if !front.outer_published {
            return Ok(None);
        }
        let published = front.ticket.ordinal.checked_add(1).ok_or(EOVERFLOW)?;
        if !stamps
            .into_iter()
            .all(|stamp| retirement::stamp_covers(stamp, published))
        {
            return Ok(None);
        }
        Ok(Some(front.packet.clone()))
    }

    /// One loan for every render already complete. The front keeps observe_front()'s
    /// semantics (its observation is returned even when incomplete); a later render is
    /// added only while it and all renders before it are complete. Commit retires them
    /// in order, each against the then-current front ticket, and stops at the first that
    /// does not retire.
    pub(crate) fn observe_completed(&self) -> Result<ObservedRun> {
        let front = self.observe_front()?;
        let mut complete = retirement::observe(&front.ticket, front.snapshot)?.complete;
        let mut run = ObservedRun { items: [const { None }; OBSERVED_RUN_MAX], count: 1 };
        run.items[0] = Some(front);
        for active in self.active.iter().skip(1) {
            if !complete || run.count == OBSERVED_RUN_MAX || !active.outer_published
                || !Arc::ptr_eq(&active.packet.context, &self.memory.context)
            {
                break;
            }
            let ticket = active.ticket;
            let mut next = None;
            for _ in 0..2 {
                fence(Ordering::Acquire);
                let snapshot = snapshot(&self.memory, ticket.ordinal)?;
                if let Ok(observation) = retirement::observe(&ticket, snapshot) {
                    next = observation.complete.then_some(Observed { ticket, snapshot });
                    break;
                }
            }
            let Some(observed) = next else { break };
            complete = true;
            run.items[run.count] = Some(observed);
            run.count += 1;
        }
        Ok(run)
    }

    /// The observation belongs to the current front (so retiring it cannot mismatch).
    pub(crate) fn is_front(&self, observed: &Observed) -> bool {
        self.active.front().is_some_and(|front| front.ticket == observed.ticket)
    }

    pub(crate) fn observe_front(&self) -> Result<Observed> {
        let front = self.active.front().ok_or(EIO)?;
        if !Arc::ptr_eq(&front.packet.context, &self.memory.context) {
            return Err(EIO);
        }
        let ticket = front.ticket;
        for _ in 0..2 {
            fence(Ordering::Acquire);
            let snapshot = snapshot(&self.memory, ticket.ordinal)?;
            if retirement::observe(&ticket, snapshot).is_ok() {
                return Ok(Observed { ticket, snapshot });
            }
        }
        Err(EAGAIN)
    }
}

pub(super) fn snapshot(memory: &memory::Memory, ordinal: u64) -> Result<retirement::Snapshot> {
    if !memory.owns(ordinal) {
        return Err(EIO);
    }
    let cursors = memory.graph.cursors()?;
    // Sample the private head before the logical shared head, whose membership
    // may include other pairs. Individual completion does not require it empty.
    let _first = memory
        .graph
        .queues
        .dword(memory::JOB_LIST)?
        .load(Ordering::Relaxed);
    let _last = memory
        .graph
        .queues
        .dword(memory::JOB_LIST + 8)?
        .load(Ordering::Relaxed);
    let head = memory.context.work_head()?;
    Ok(retirement::Snapshot {
        cursors,
        job_list_empty: head == [0, memory.context.work_head_va()],
        pass: memory.pass(ordinal)?,
    })
}
