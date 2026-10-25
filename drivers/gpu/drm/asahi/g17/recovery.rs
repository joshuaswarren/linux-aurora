// SPDX-License-Identifier: GPL-2.0-only OR MIT

//! Firmware restart handshake and per-VM failure classification.
//!
//! The device mutex serializes this state with publication. Preparations are closed and joined
//! without that mutex before `service`; no queue callback may signal a fence here. Queue
//! owners retain failed work until its retirement witness, a complete pair-drain witness, or
//! processor stop. A failed fence alone never releases mappings.

mod attribution;
mod slot;
pub(crate) use attribution::Sources;
use attribution::Attribution;

pub(super) fn slot_key(slot: usize) -> Option<u64> {
    slot::key(slot).ok()
}

use super::status::VmStatus;
use kernel::{prelude::*, sync::Arc};

const QIDS: u8 = 128;
const GUILTY: u8 = 1 << 1;
const CANCELLED: u8 = 1 << 2;
const MAX_GUILTY_VMS: usize = 8;
const RECOVERY_POLLS: usize = 500;

/// Per-QID classifications from the complete firmware recovery table.
/// Multiple records for a queue contribute all of their classification bits.
#[derive(Default)]
pub(super) struct Entries {
    guilty: u128,
    cancelled: u128,
}

impl Entries {
    pub(super) fn include(&mut self, flags: u32) {
        if flags & 1 == 0 {
            return;
        }
        let qid = (flags >> 4) & 0x7f;
        let bit = 1u128 << qid;
        if flags & u32::from(GUILTY) != 0 {
            self.guilty |= bit;
        }
        if flags & u32::from(CANCELLED) != 0 {
            self.cancelled |= bit;
        }
    }
}

/// How an error completion affects its VM's sticky status and other jobs.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(super) enum Class {
    /// Only this work's fence fails; VM status and unrelated work remain usable.
    Spared,
    /// This VM owns guilty work.
    Guilty,
}

/// Queue operations under the device mutex. Completion signalling belongs to the caller after
/// that mutex is dropped. `oldest_spared` returns None if the QID has no in-flight owner.
pub(super) trait Queues {
    fn render_scheduler_active(&self) -> bool;
    fn render_pass_started(&self, qid: u8, stamp: u64) -> Option<bool>;
    /// The compute queue's front command never executed (zero-duration record).
    fn compute_unexecuted(&self, qid: u8) -> Option<bool>;
    /// The compute queue's front command was already republished once.
    fn compute_front_replayed(&self, qid: u8) -> bool;
    /// Latch the compute queue's front completion record from the shared ring;
    /// whether it is present, None without a ticketed compute front.
    fn observe_compute_front(&mut self, qid: u8) -> Option<bool>;
    /// Describe every compute queue's in-flight front after the restart closed.
    fn describe_compute_fronts(&self);
    /// QIDs of render pairs whose front group started tiling and never finished.
    fn started_unfinished_renders(&self) -> u128;
    /// Copy every retained queue's live producer to its recovery target, then
    /// issue a full barrier. A failed graph must not skip subsequent queues.
    fn publish_recovery_targets(&self) -> Option<Error>;
    /// Active converted consumers require every recovery target to be copied.
    fn has_active_source_consumers(&self) -> bool { false }
    fn oldest_spared(&self, qid: u8) -> Option<bool>;
    fn visit_owners(&self, qid: u8, visit: &mut dyn FnMut(&Arc<VmStatus>));
    /// Classify every in-flight completion; return QIDs of render slots with spared work.
    fn classify_work(&mut self, class: &dyn Fn(&Arc<VmStatus>) -> Class) -> u128;
    fn settle_blamed_render(&mut self, qid: u8, error: Error);
    fn settle_cancelled_renders(&mut self, qids: u128);
    fn rescan_compute(&mut self) -> Result;
    fn release_retired_compute(&mut self);
}

/// Live shared-memory operations. The handshake word uses aligned 32-bit accesses; transitions
/// check the expected value, issue a full barrier, store the next value, then another barrier.
pub(super) trait Memory {
    fn recovery_state(&self) -> Result<u32>;
    fn host_recovery(&self) -> Result<u32>;
    fn fault_sources(&self) -> Result<Sources>;
    fn transition(&self, expected: u32, next: u32) -> Result;
    fn clear_timestamps(&self) -> Result;
    fn entries(&self) -> Result<Entries>;
    /// One millisecond wait; this is polled because the firmware sends no state-3 notification.
    fn wait_tick(&self);
    /// Recovery diagnostic while waiting for state 1 (`polls` 1 ms ticks so far):
    /// firmware status words that show whether firmware is still progressing.
    fn describe_recovery_wait(&self, _polls: usize) {}
    /// A 100 us wait for a cancellation record firmware stores after resuming.
    fn wait_short(&self) {
        kernel::time::delay::fsleep(kernel::time::Delta::from_micros(100));
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
struct Request {
    generation: u64,
    blamed: Option<u8>,
}

/// Generation accepted by the host and the coalesced request from either coprocessor.
pub(super) struct State {
    accepted: u64,
    pending: Option<Request>,
    /// Host time of the last recovery that withdrew firmware blame from a
    /// compute command that never executed.
    withdrawn_at: Option<u64>,
}

/// Polls (100 us apart) for the blamed compute command's completion record.
/// Firmware stores its cancellation record after the host closes the restart.
const BLAMED_RECORD_POLLS: usize = 50;
/// A recovery within this interval of a withdrawn blame keeps the firmware's
/// blame, so work that repeatedly stalls the GPU cannot avoid it indefinitely.
const WITHDRAW_INTERVAL_NS: u64 = 2_000_000_000;

/// Non-fatal post-recovery failures. The firmware handshake has already closed successfully.
pub(super) struct ServiceResult {
    pub(super) scan_error: Option<Error>,
    pub(super) target_error: Option<Error>,
}

impl State {
    pub(super) const fn new() -> Self {
        Self {
            accepted: 0,
            pending: None,
            withdrawn_at: None,
        }
    }
    pub(super) fn generation(&self) -> u64 {
        self.accepted
    }
    pub(super) fn pending(&self) -> bool {
        self.pending.is_some()
    }

    /// Returns false for a consumed duplicate from an older generation, true when the event
    /// drain must stop until the current request is serviced. The first blamed QID wins.
    pub(super) fn request(&mut self, generation: u64, stamp: i32) -> Result<bool> {
        if generation < self.accepted {
            return Ok(false);
        }
        if generation != self.accepted {
            return Err(EIO);
        }
        if stamp != -1 && !(0..i32::from(QIDS)).contains(&stamp) {
            return Err(EIO);
        }
        let blamed = if (0..i32::from(QIDS)).contains(&stamp) {
            Some(stamp as u8)
        } else {
            None
        };
        if let Some(pending) = self.pending.as_mut() {
            if pending.generation != generation {
                return Err(EBUSY);
            }
            pending.blamed = pending.blamed.or(blamed);
        } else {
            self.pending = Some(Request { generation, blamed });
        }
        Ok(true)
    }
}

/// The firmware owner implements memory and queue access without splitting its
/// mutable borrow or duplicating the authoritative restart generation.
pub(super) trait Host: Memory + Queues {
    fn recovery(&self) -> &State;
    fn recovery_mut(&mut self) -> &mut State;
}

/// Performs one pending restart. The caller has closed preparation admission and joined all
/// builders off-lock. Work remains mapped throughout every callback, including on error.
pub(super) fn service<H: Host>(
    host: &mut H,
    crashed: impl Fn() -> bool,
) -> Result<ServiceResult> {
    let request = host.recovery().pending.ok_or(EINVAL)?;
    pr_err!("AGX firmware recovery requested: generation={} blamed={:?}\n", request.generation, request.blamed);
    if request.generation != host.recovery().accepted || host.host_recovery()? != 0 {
        return Err(EIO);
    }
    // The restart record is published before firmware announces state 1.
    // Its ring cursor is already consumed; wait only for that readiness word,
    // without advancing the host generation or acknowledging an early event.
    let mut ready = false;
    for poll in 0..RECOVERY_POLLS {
        if crashed() {
            return Err(EIO);
        }
        match host.recovery_state()? {
            1 => {
                ready = true;
                if poll != 0 {
                    host.describe_recovery_wait(poll);
                }
                break;
            }
            0 => {
                if poll % 25 == 0 {
                    host.describe_recovery_wait(poll);
                }
                host.wait_tick()
            }
            _ => return Err(EIO),
        }
    }
    if !ready {
        return Err(ETIMEDOUT);
    }
    // Recheck ownership after the off-CPU waits, before any handshake mutation.
    if host.host_recovery()? != 0 {
        return Err(EIO);
    }
    // A converted consumer may depend on a producer cancelled by this restart.
    // Do not resume that dependency graph without a proven cancellation order.
    // The caller's event failure path stops the processors and keeps M5-owned
    // storage retained; this is not an engine-retirement witness.
    if host.has_active_source_consumers() {
        pr_err!("GPU recovery with active cross-queue consumers requires stop containment\n");
        return Err(EIO);
    }
    host.recovery_mut().accepted = host.recovery().accepted.wrapping_add(1);
    if request.generation == 0 && !host.render_scheduler_active() {
        host.clear_timestamps()?;
    }
    let mut sources = host.fault_sources()?;
    pr_err!("AGX firmware recovery fault sources: {:?}\n", sources);
    let target_error = host.publish_recovery_targets();
    // Firmware is still halted and publication remains closed. Do not resume
    // a converted consumer when any target failed to join this cancellation.
    if host.has_active_source_consumers() {
        if let Some(error) = target_error { return Err(error); }
    }
    host.transition(1, 2)?;
    let mut recovered = false;
    for _ in 0..RECOVERY_POLLS {
        if host.recovery_state()? == 3 {
            recovered = true;
            break;
        }
        if crashed() {
            return Err(EIO);
        }
        host.wait_tick();
    }
    if !recovered {
        return Err(ETIMEDOUT);
    }
    // A failed table read leaves only the explicit blamed QID as evidence of guilt.
    let entries = host.entries().unwrap_or_default();
    pr_info!("AGX firmware recovery table: guilty={:#x} cancelled={:#x}\n", entries.guilty, entries.cancelled);
    host.transition(3, 0)?;
    if host.recovery_state()? != 0 {
        return Err(EIO);
    }
    host.recovery_mut().pending = None;

    if let Some(qid) = request.blamed {
        observe_blamed_compute(host, qid);
    }
    host.describe_compute_fronts();
    let now = super::now_ns();
    let may_withdraw = host
        .recovery()
        .withdrawn_at
        .is_none_or(|at| now.saturating_sub(at) >= WITHDRAW_INTERVAL_NS);
    let (cancelled, withdrawn) = classify(host, entries, request.blamed, may_withdraw, &mut sources);
    if withdrawn {
        host.recovery_mut().withdrawn_at = Some(now);
    }
    if let Some(qid) = request.blamed {
        host.settle_blamed_render(qid, EIO);
    }
    if cancelled != 0 {
        host.settle_cancelled_renders(cancelled);
    }
    let scan_error = host.rescan_compute().err();
    host.release_retired_compute();
    Ok(ServiceResult { scan_error, target_error })
}

/// Firmware stores a blamed compute command's cancellation record after the
/// restart closes. Wait briefly for it so classification sees the exact
/// record; this runs only on the recovery path.
fn observe_blamed_compute<H: Host>(host: &mut H, qid: u8) {
    for poll in 0..BLAMED_RECORD_POLLS {
        match host.observe_compute_front(qid) {
            None => return,
            Some(true) => {
                if poll != 0 {
                    pr_info!("AGX recovery: blamed compute qid {} record latched after {} polls\n", qid, poll);
                }
                return;
            }
            Some(false) => host.wait_short(),
        }
    }
    pr_info!("AGX recovery: blamed compute qid {} has no completion record after {} polls\n", qid, BLAMED_RECORD_POLLS);
}

/// Resolves guilty QIDs before changing any completion's classification. Unknown ownership or
/// too many distinct guilty VMs leaves all prior classifications intact. The oldest spared work
/// of a QID makes later blame collateral to its previous recovery, rather than new VM guilt.
/// Also returns whether the firmware's blame of a never-executed compute command was withdrawn.
fn classify<Q: Queues>(
    queues: &mut Q,
    entries: Entries,
    blamed: Option<u8>,
    may_withdraw: bool,
    sources: &mut Sources,
) -> (u128, bool) {
    let mut guilty_qids = entries.guilty | blamed.map_or(0, |qid| 1u128 << qid);
    let mut withdrawn = false;
    // A blamed compute command whose record proves it never executed cannot
    // have made the stall; it is the work that could not be switched in. No
    // VM is guilty then, as attribute() decides for a blamed render that never
    // started when firmware names no other culprit: the command is spared and
    // replayed. Renders whose tiler started and never finished are not blamed
    // instead: without firmware fault sources that is no evidence of guilt
    // (R336v4: such renders of an innocent loop and of the CTS were replayed
    // and completed). A command blamed again after its replay, or a recovery
    // shortly after a withdrawn blame, keeps the firmware's blame.
    if let Some(qid) = blamed {
        if queues.compute_unexecuted(qid) == Some(true) {
            let bit = 1u128 << qid;
            let started = queues.started_unfinished_renders() & !bit;
            pr_info!("AGX recovery: blamed compute qid {} never executed; started unfinished renders {:#x}\n", qid, started);
            if queues.compute_front_replayed(qid) {
                pr_info!("AGX recovery: blamed compute qid {} was already replayed; firmware blame stands\n", qid);
            } else if !may_withdraw {
                pr_info!("AGX recovery: blamed compute qid {} within the withdrawn-blame interval; firmware blame stands\n", qid);
            } else {
                guilty_qids = entries.guilty & !bit;
                withdrawn = true;
                pr_info!("AGX recovery: blamed compute qid {} blame withdrawn; remaining guilty {:#x}\n", qid, guilty_qids);
            }
        }
    }
    let cancelled = entries.cancelled;
    let attribution = sources.attribute(blamed, |qid, stamp| queues.render_pass_started(qid, stamp));
    match attribution {
        Attribution::Firmware => {}
        Attribution::Queues(qids) => {
            guilty_qids = qids;
            withdrawn = false;
        }
        Attribution::NoCulprit => guilty_qids = 0,
    }
    let no_culprit = attribution == Attribution::NoCulprit || (withdrawn && guilty_qids == 0);
    if guilty_qids == 0 && !no_culprit {
        return (0, withdrawn);
    }
    let mut guilty: [Option<Arc<VmStatus>>; MAX_GUILTY_VMS] = Default::default();
    let mut count = 0;
    let mut collateral = 0u128;
    let mut unresolved = false;
    for qid in 0..QIDS {
        if guilty_qids & (1u128 << qid) == 0 {
            continue;
        }
        match queues.oldest_spared(qid) {
            None => unresolved = true,
            Some(true) => collateral |= 1u128 << qid,
            Some(false) => {
                let mut found = false;
                queues.visit_owners(qid, &mut |status| {
                    found = true;
                    if guilty[..count]
                        .iter()
                        .flatten()
                        .any(|known| Arc::ptr_eq(known, status))
                    {
                        return;
                    }
                    if let Some(slot) = guilty.get_mut(count) {
                        *slot = Some(status.clone());
                        count += 1;
                    } else {
                        unresolved = true;
                    }
                });
                unresolved |= !found;
            }
        }
    }
    if unresolved || (count == 0 && collateral == 0 && !no_culprit) {
        return (0, false);
    }
    let spared = queues.classify_work(&|status| {
        guilty[..count]
            .iter()
            .find_map(|known| {
                known
                    .as_ref()
                    .filter(|known| Arc::ptr_eq(known, status))
                    .map(|_| Class::Guilty)
            })
            .unwrap_or(Class::Spared)
    });
    (cancelled & !(guilty_qids & !collateral) & spared, withdrawn)
}
