// SPDX-License-Identifier: GPL-2.0-only OR MIT

//! Logical queue and physical render-pair admission. A retained pair belongs
//! to one logical owner even while idle. Closing that owner makes it reusable
//! only after every active lease retires. Quarantine retains these leases until
//! a firmware drain witness or both processors stopping makes reuse safe.
//!
//! The pool lock protects host accounting only and is never held while building
//! a graph or waiting for GPU work. Capacity waiters sleep on the release event.

use crate::g17::{
    availability::Availability,
    hazard::{self, Footprint, Plan},
    job::Packet,
    progress::{self, Step, Watch},
};
use crate::{g17::context::MAX_QUEUES, hw::t8140::queues as cfg};
use core::sync::atomic::{AtomicU32, Ordering};
use kernel::{
    new_condvar, new_mutex,
    prelude::*,
    sync::{Arc, CondVar, CondVarTimeoutResult, Mutex},
    time::{delay::fsleep, Delta},
};

#[derive(Clone, Copy)]
pub(crate) struct Request<'a> {
    footprint: &'a Arc<Footprint>,
    sequence: u64,
    prefix: Option<u64>,
}

impl<'a> Request<'a> {
    pub(crate) fn of(packet: &'a Packet) -> Option<Self> {
        packet.footprint.as_ref().map(|footprint| Self {
            footprint,
            sequence: packet.order.sequence,
            prefix: packet.order.wait_through[0],
        })
    }
}

struct InFlight {
    owner: u64,
    sequence: u64,
    slot: Option<u8>,
    ticket: u64,
    footprint: Arc<Footprint>,
}

const QUARANTINE_WAIT_MS: u32 = 1000;
/// vmfix6d B: a capacity wait sleeps on the release condition in slices of
/// this length; it fails only after progress::STALL_NS with no retirement and
/// no change of the admission state (a deadlock), never on a fixed budget.
const SLOT_WAIT_SLICE_MS: u32 = 1000;

/// Diagnostic only: lend before fresh/released pairs (m5_render_lend_test).
fn lend_test() -> bool {
    *crate::module_parameters::m5_render_lend_test.value() != 0
}

/// First eight, then every power of two.
fn sampled(counter: &core::sync::atomic::AtomicU64) -> Option<u64> {
    let n = counter.fetch_add(1, Ordering::Relaxed) + 1;
    (n <= 8 || n.is_power_of_two()).then_some(n)
}

struct State {
    active: u64,
    refs: [u8; cfg::RENDER_SLOTS],
    owners: [u64; cfg::RENDER_SLOTS],
    epochs: [u64; cfg::RENDER_SLOTS],
    quarantined: u64,
    kill_hold: u64,
    released: u64,
    /// Slots whose pair could not be built (no QID or identity left). Such a
    /// slot never received firmware work and is never offered again.
    starved: u64,
    /// Set by the first unbuildable pair: published identities are never
    /// recycled, so only already-built slots are offered from then on.
    fresh_blocked: bool,
    /// Slots whose pair identities exist. Pair entries are never removed.
    built: u64,
    /// The idle owner a slot was lent from by admission; that owner takes it
    /// back once it is idle again (vmfix6b: a compositor whose pair was lent
    /// while its display slept gets it back at wake).
    lent_from: [u64; cfg::RENDER_SLOTS],
    /// vmfix6d C: lent slots their lender has recalled. The borrower may not
    /// admit new work there (it no longer owns them); the lender reclaims a
    /// slot once the borrower's leases on it have retired.
    recall: u64,
    epoch: u64,
    footprints: KVec<InFlight>,
    next_ticket: u64,
}
impl State {
    const MASK: u64 = (u64::MAX >> (64 - cfg::RENDER_SLOTS)) & !1;
    const fn new() -> Self {
        Self {
            active: 0,
            refs: [0; cfg::RENDER_SLOTS],
            owners: [0; cfg::RENDER_SLOTS],
            epochs: [0; cfg::RENDER_SLOTS],
            quarantined: 0,
            kill_hold: 0,
            released: 0,
            starved: 0,
            fresh_blocked: false,
            built: 0,
            lent_from: [0; cfg::RENDER_SLOTS],
            recall: 0,
            epoch: 0,
            footprints: KVec::new(),
            next_ticket: 1,
        }
    }
    fn exhausted(&self) -> bool {
        self.quarantined & Self::MASK == Self::MASK
    }
    /// Slots that can still carry work: never unbuildable, and already built
    /// once no fresh pair can be constructed.
    fn usable(&self) -> u64 {
        if self.fresh_blocked {
            self.built & !self.starved
        } else {
            !self.starved
        }
    }
    /// `owner` holds `slot` and that slot can carry work. A recalled slot is
    /// no longer its borrower's (vmfix6d C).
    fn owns(&self, slot: usize, owner: u64) -> bool {
        self.owners[slot] == owner
            && self.usable() & (1u64 << slot) != 0
            && self.recall & (1u64 << slot) == 0
    }
    fn owns_any(&self, owner: u64) -> bool {
        (0..cfg::RENDER_SLOTS).any(|slot| self.owns(slot, owner))
    }
    fn refusal(&self, owner: u64) -> Result {
        if self.exhausted() {
            return Err(EIO);
        }
        match (0..cfg::RENDER_SLOTS).position(|slot| self.owns(slot, owner)) {
            Some(slot) if self.quarantined & (1u64 << slot) != 0 => Err(EIO),
            None if (self.fresh_blocked
                || !(1..cfg::RENDER_SLOTS)
                    .any(|slot| self.owners[slot] == 0 && self.starved & (1u64 << slot) == 0))
                && self.released
                    & !self.active
                    & !self.quarantined
                    & !self.kill_hold
                    & self.usable()
                    & Self::MASK
                    == 0
                && self.lendable(owner).is_none()
                && self.reclaimable(owner).is_none()
                && self.recallable(owner).is_none() =>
            {
                Err(enospc_site!("admission.rs:95 render owner slots full"))
            }
            _ => Ok(()),
        }
    }
    /// Once no fresh pair can be built: the least recently used built slot of
    /// another live owner that holds no lease on it (no work admitted there).
    /// A slot whose owner keeps another pair goes first. Same lock as every
    /// acquire, so the owner cannot be admitting on it concurrently.
    fn lendable(&self, owner: u64) -> Option<usize> {
        if !self.fresh_blocked && !lend_test() {
            return None;
        }
        let free = !self.active
            & !self.released
            & !self.quarantined
            & !self.kill_hold
            & !self.recall
            & self.usable()
            & Self::MASK;
        (1..cfg::RENDER_SLOTS)
            .filter(|slot| {
                free & (1u64 << slot) != 0
                    && self.refs[*slot] == 0
                    && self.owners[*slot] != 0
                    && self.owners[*slot] != owner
            })
            .min_by_key(|slot| {
                let holder = self.owners[*slot];
                let keeps_other = (0..cfg::RENDER_SLOTS)
                    .any(|other| other != *slot && self.owns(other, holder));
                (!keeps_other, self.epochs[*slot])
            })
    }
    /// An idle slot that admission lent away from `owner`, which owns no
    /// pair now.
    fn reclaimable(&self, owner: u64) -> Option<usize> {
        if self.owns_any(owner) {
            return None;
        }
        let free = !self.active & !self.quarantined & !self.kill_hold & self.usable() & Self::MASK;
        (1..cfg::RENDER_SLOTS).find(|slot| {
            self.lent_from[*slot] == owner && free & (1u64 << slot) != 0 && self.refs[*slot] == 0
        })
    }
    /// vmfix6d C: a slot lent away from `owner` (which owns no pair now) whose
    /// borrower still has leases on it: recalling it makes the lender wait for
    /// that work to retire rather than borrow (and build) elsewhere. With the
    /// eager restore the lender then reinstalls its retained graph there.
    fn recallable(&self, owner: u64) -> Option<usize> {
        if owner == 0 || self.owns_any(owner) {
            return None;
        }
        let usable = !self.quarantined & !self.kill_hold & self.usable() & Self::MASK;
        (1..cfg::RENDER_SLOTS)
            .find(|slot| self.lent_from[*slot] == owner && usable & (1u64 << slot) != 0)
    }
    fn advance(&mut self, slot: usize) {
        self.epoch = self.epoch.wrapping_add(1);
        self.epochs[slot] = self.epoch;
    }
    fn take_idle(&mut self, slot: usize, owner: u64) -> u8 {
        self.refs[slot] += 1;
        self.active |= 1u64 << slot;
        self.released &= !(1u64 << slot);
        self.owners[slot] = owner;
        self.advance(slot);
        self.drop_recalls(owner);
        slot as u8
    }
    /// vmfix6d: a lender that owns a pair again no longer waits for the
    /// slots it recalled; release them to their borrowers (a recall left set
    /// stranded the slot: neither its borrower nor anyone else could use it).
    fn drop_recalls(&mut self, owner: u64) {
        if self.recall == 0 {
            return;
        }
        for slot in 1..cfg::RENDER_SLOTS {
            if self.recall & (1u64 << slot) != 0 && self.lent_from[slot] == owner {
                self.recall &= !(1u64 << slot);
            }
        }
    }
    fn acquire(&mut self, owner: u64) -> Option<u8> {
        if owner == 0 {
            return None;
        }
        // Prefer the owner's idle pair. A missing sibling can be claimed
        // only while this owner has no live kick, before constructing it.
        for slot in 0..cfg::RENDER_SLOTS {
            if self.owns(slot, owner)
                && self.refs[slot] == 0
                && self.quarantined & (1u64 << slot) == 0
            {
                if !(0..cfg::RENDER_SLOTS)
                    .any(|other| self.owners[other] == owner && self.refs[other] != 0)
                {
                    self.claim_sibling(owner);
                }
                return Some(self.take_idle(slot, owner));
            }
        }
        if let Some(slot) = self.reclaimable(owner) {
            let borrower = self.owners[slot];
            self.lent_from[slot] = 0;
            self.recall &= !(1u64 << slot);
            // Other slots this owner recalled stay lent (drop_recalls below).
            static RECLAIMS: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);
            if let Some(n) = sampled(&RECLAIMS) {
                pr_info!("asahi: render slot {} reclaimed by owner {} from borrower {} (reclaim {})\n",
                    slot, owner, borrower, n);
            }
            let first = self.take_idle(slot, owner);
            self.claim_sibling(owner);
            return Some(first);
        }
        if let Some(slot) = self.recallable(owner) {
            // vmfix6d C: the borrower is still active on the lender's slot.
            // Stop new borrower work there and wait for its leases to retire.
            if self.recall & (1u64 << slot) == 0 {
                self.recall |= 1u64 << slot;
                static RECALLS: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);
                if let Some(n) = sampled(&RECALLS) {
                    pr_info!("asahi: render slot {} recalled by owner {} from borrower {} (refs {}, recall {})\n",
                        slot, owner, self.owners[slot], self.refs[slot], n);
                }
            }
            return None;
        }
        if self.owns_any(owner) {
            // Consecutive kicks are ordered by firmware dependencies. Stack
            // onto the least-loaded retained pair without a retirement wait.
            let slot = (0..cfg::RENDER_SLOTS)
                .filter(|slot| {
                    self.owns(*slot, owner)
                        && self.quarantined & (1u64 << slot) == 0
                        && self.active & (1u64 << slot) != 0
                        && self.refs[*slot] < cfg::RENDER_DEPTH
                })
                .min_by_key(|slot| (self.refs[*slot], u64::MAX - self.epochs[*slot]))?;
            self.refs[slot] += 1;
            self.advance(slot);
            return Some(slot as u8);
        }
        let free = !self.active & Self::MASK & !self.quarantined & !self.kill_hold & !self.recall
            & self.usable();
        let eligible = |slot: usize| free & (1u64 << slot) != 0;
        let fresh = !self.fresh_blocked;
        // Reuse a closed, exactly retired owner before consuming a new graph.
        // The diagnostic lend test lends first (forces live-owner churn).
        let lent_first = if lend_test() { self.lendable(owner) } else { None };
        let slot = lent_first.or_else(|| {
            (0..cfg::RENDER_SLOTS)
                .filter(|slot| eligible(*slot) && self.released & (1u64 << slot) != 0)
                .min_by_key(|slot| self.epochs[*slot])
        })
            .or_else(|| {
                (0..cfg::RENDER_SLOTS)
                    .find(|slot| fresh && eligible(*slot) && self.owners[*slot] == 0)
            });
        let slot = match slot {
            Some(slot) if lent_first != Some(slot) => slot,
            _ => {
                let slot = self.lendable(owner)?;
                let previous = self.owners[slot];
                // vmfix6d: keep the original lender through re-lends, so it
                // can still recall its slot (and restore its graph there).
                if self.lent_from[slot] == 0 {
                    self.lent_from[slot] = previous;
                }
                static LENDS: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);
                if let Some(n) = sampled(&LENDS) {
                    pr_info!("asahi: render slot {} lent by admission from idle owner {} to {} (lend {})\n",
                        slot, previous, owner, n);
                }
                slot
            }
        };
        let first = self.take_idle(slot, owner);
        self.claim_sibling(owner);
        Some(first)
    }
    fn claim_sibling(&mut self, owner: u64) {
        // Pairs are scarce once no fresh one can be built: one per owner.
        if self.fresh_blocked {
            return;
        }
        if (0..cfg::RENDER_SLOTS).filter(|slot| self.owns(*slot, owner)).count()
            >= cfg::RENDER_PAIRS_PER_OWNER
        {
            return;
        }
        let free = !self.active & Self::MASK & !self.quarantined & !self.kill_hold & !self.recall
            & self.usable();
        let eligible = |slot: usize| free & (1u64 << slot) != 0;
        let fresh = !self.fresh_blocked;
        let sibling = (0..cfg::RENDER_SLOTS)
            .filter(|slot| {
                eligible(*slot)
                    && self.owners[*slot] != owner
                    && self.released & (1u64 << slot) != 0
            })
            .min_by_key(|slot| self.epochs[*slot])
            .or_else(|| {
                (0..cfg::RENDER_SLOTS)
                    .find(|slot| fresh && eligible(*slot) && self.owners[*slot] == 0)
            });
        if let Some(slot) = sibling {
            self.released &= !(1u64 << slot);
            self.owners[slot] = owner;
            self.advance(slot);
            self.drop_recalls(owner);
        }
    }
    fn warmable_sibling(&self, owner: u64, publishing: u8) -> Option<u8> {
        if self.refs.get(usize::from(publishing)) != Some(&1) {
            return None;
        }
        (0..cfg::RENDER_SLOTS)
            .find(|slot| {
                *slot != usize::from(publishing)
                    && self.owns(*slot, owner)
                    && self.refs[*slot] == 0
                    && self.quarantined & (1u64 << slot) == 0
            })
            .map(|slot| slot as u8)
    }
    fn plan(&self, owner: u64, request: Option<Request<'_>>, ticket: Option<u64>) -> Plan {
        let Some(request) = request else {
            return Plan::Any;
        };
        hazard::plan(
            owner,
            request.prefix,
            ticket,
            request.footprint,
            self.footprints.iter().map(|entry| hazard::Live {
                owner: entry.owner,
                sequence: entry.sequence,
                slot: entry.slot,
                ticket: entry.ticket,
                footprint: &entry.footprint,
                quarantined: entry
                    .slot
                    .is_some_and(|slot| self.quarantined & (1u64 << slot) != 0),
            }),
        )
    }

    fn pick(&mut self, owner: u64, plan: Plan) -> Option<u8> {
        match plan {
            Plan::Any => self.acquire(owner),
            Plan::Wait => None,
            Plan::Pair(slot) => {
                let index = usize::from(slot);
                if self.owners.get(index) != Some(&owner)
                    || (self.quarantined | self.kill_hold | self.recall | !self.usable())
                        & (1u64 << slot)
                        != 0
                    || self.refs[index] >= cfg::RENDER_DEPTH
                {
                    return None;
                }
                if self.refs[index] == 0 {
                    Some(self.take_idle(index, owner))
                } else {
                    self.refs[index] += 1;
                    self.advance(index);
                    Some(slot)
                }
            }
        }
    }

    fn record(
        &mut self,
        owner: u64,
        slot: Option<u8>,
        ticket: u64,
        request: Request<'_>,
    ) -> Option<(u64, u64)> {
        self.footprints
            .push(
                InFlight {
                    owner,
                    sequence: request.sequence,
                    slot,
                    ticket,
                    footprint: request.footprint.clone(),
                },
                GFP_KERNEL,
            )
            .ok()?;
        Some((owner, request.sequence))
    }

    fn forget_waiter(&mut self, ticket: Option<u64>) {
        if let Some(ticket) = ticket {
            self.footprints
                .retain(|entry| entry.slot.is_some() || entry.ticket != ticket);
        }
    }

    fn release(&mut self, slot: u8) {
        let slot = usize::from(slot);
        if self.refs[slot] != 0 {
            self.refs[slot] -= 1;
        }
        if self.refs[slot] == 0 {
            self.active &= !(1u64 << slot);
        }
    }

    /// vmfix6d A/B: the whole admission state, hashed (blocker fingerprint).
    fn fingerprint(&self) -> u64 {
        let mut hash = progress::SEED;
        for word in [
            self.active,
            self.quarantined,
            self.kill_hold,
            self.released,
            self.starved,
            self.built,
            u64::from(self.fresh_blocked),
            self.footprints.len() as u64,
            self.recall,
        ] {
            hash = progress::mix(hash, word);
        }
        for slot in 0..cfg::RENDER_SLOTS {
            hash = progress::mix(hash, self.owners[slot] ^ u64::from(self.refs[slot]) << 56);
            hash = progress::mix(hash, self.lent_from[slot]);
        }
        hash
    }

    fn diag(&self, slot: u8, owner: u64) -> AdmissionDiag {
        let index = usize::from(slot).min(cfg::RENDER_SLOTS - 1);
        let bit = 1u64 << index;
        let flag = |mask: u64, shift: u32| u32::from(mask & bit != 0) << shift;
        AdmissionDiag {
            owner: self.owners[index],
            refs: self.refs[index],
            bits: flag(self.active, 0)
                | flag(self.released, 1)
                | flag(self.quarantined, 2)
                | flag(self.kill_hold, 3)
                | flag(self.starved, 4)
                | flag(self.built, 5)
                | flag(self.usable(), 6)
                | flag(self.recall, 7),
            lent_from: self.lent_from[index],
            owns_any: self.owns_any(owner),
            fresh_blocked: self.fresh_blocked,
            counts: [
                (self.active & Self::MASK).count_ones(),
                (self.released & Self::MASK).count_ones(),
                (self.starved & Self::MASK).count_ones(),
                (self.built & Self::MASK).count_ones(),
            ],
            footprints: self.footprints.len() as u32,
        }
    }

    /// One line for capacity-wait reports.
    fn report(&self, what: &str, owner: u64, watch: &Watch, now: u64) {
        let d = self.diag(0, owner);
        pr_info!(
            "asahi: render {} owner={} waited={}ms idle={}ms polls={} owns_any={} fresh_blocked={} active={} released={} starved={} built={} footprints={}\n",
            what, owner, watch.elapsed_ms(now), watch.idle_ms(now), watch.polls, d.owns_any,
            d.fresh_blocked, d.counts[0], d.counts[1], d.counts[2], d.counts[3], d.footprints
        );
    }
}

/// vmfix6d A: admission view of one slot (Pool::diag).
#[derive(Clone, Copy)]
pub(crate) struct AdmissionDiag {
    pub(crate) owner: u64,
    pub(crate) refs: u8,
    /// Bits: active, released, quarantined, kill_hold, starved, built, usable,
    /// recalled.
    pub(crate) bits: u32,
    pub(crate) lent_from: u64,
    pub(crate) owns_any: bool,
    pub(crate) fresh_blocked: bool,
    /// Slots active, released, starved, built.
    pub(crate) counts: [u32; 4],
    pub(crate) footprints: u32,
}

#[pin_data]
pub(crate) struct Pool {
    #[pin]
    state: Mutex<State>,
    #[pin]
    released: CondVar,
    queues: AtomicU32,
    available: Arc<Availability>,
}
impl Pool {
    pub(crate) fn new() -> Result<Arc<Self>> {
        let available = Availability::new()?;
        Arc::pin_init(
            pin_init!(Self {
                state <- new_mutex!(State::new(), "G17 render admission"),
                released <- new_condvar!("G17 render pair release"),
                queues: AtomicU32::new(0),
                available,
            }),
            GFP_KERNEL,
        )
    }
    pub(crate) fn reserve_queue(self: &Arc<Self>) -> Result<QueueSlot> {
        let mut count = self.queues.load(Ordering::Acquire);
        loop {
            // Hardware identities and QoS slots are bound per publication;
            // this bounds host bookkeeping only.
            if count == MAX_QUEUES {
                return Err(EOVERFLOW);
            }
            match self.queues.compare_exchange(
                count,
                count + 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return Ok(QueueSlot { pool: self.clone() }),
                Err(current) => count = current,
            }
        }
    }
    pub(crate) fn acquire_render(
        self: &Arc<Self>,
        owner: u64,
        request: Option<Request<'_>>,
    ) -> Result<RenderLease> {
        if owner == 0 {
            return Err(EINVAL);
        }
        let mut state = self.state.lock();
        let mut conflict_budget = kernel::time::msecs_to_jiffies(hazard::WAIT_MS);
        let mut quarantine_budget = kernel::time::msecs_to_jiffies(QUARANTINE_WAIT_MS);
        // vmfix6d B: one progress watch for this acquisition's capacity waits.
        let mut watch: Option<Watch> = None;
        let mut expired = false;
        let mut ticket = None;
        let result = (|| loop {
            if state.exhausted() {
                return Err(EIO);
            }
            // The first retained pair's quarantine is not bypassed through its
            // healthy sibling. Reclamation wakes this bounded admission wait.
            if state
                .owners
                .iter()
                .position(|id| *id == owner)
                .is_some_and(|slot| state.quarantined & (1u64 << slot) != 0)
            {
                if quarantine_budget == 0 {
                    return Err(EIO);
                }
                quarantine_budget = match self
                    .released
                    .wait_interruptible_timeout(&mut state, quarantine_budget)
                {
                    CondVarTimeoutResult::Signal { .. } => return Err(ERESTARTSYS),
                    CondVarTimeoutResult::Timeout => 0,
                    CondVarTimeoutResult::Woken { jiffies } => jiffies,
                };
                continue;
            }
            match state.refusal(owner) {
                Err(error) if error == ENOSPC => {
                    // Same-engine reuse: wait for an owner to close or retire a
                    // built pair. Only a stall (no retirement, no admission
                    // change for progress::STALL_NS) fails this packet (EBUSY).
                    if watch.is_none() {
                        static WAITS: core::sync::atomic::AtomicU64 =
                            core::sync::atomic::AtomicU64::new(0);
                        if let Some(n) = sampled(&WAITS) {
                            pr_info!("asahi: render slot wait: owner={} no free built pair (wait {})\n",
                                owner, n);
                        }
                    }
                    self.capacity_wait(&mut state, &mut watch, owner, "slot wait")?;
                    continue;
                }
                result => result?,
            }
            let plan = if expired {
                Plan::Any
            } else {
                state.plan(owner, request, ticket)
            };
            if let Some(slot) = state.pick(owner, plan) {
                state.forget_waiter(ticket.take());
                let record =
                    request.and_then(|request| state.record(owner, Some(slot), 0, request));
                return Ok(RenderLease {
                    pool: self.clone(),
                    slot,
                    record,
                });
            }
            if plan != Plan::Any {
                if ticket.is_none() {
                    if let Some(request) = request {
                        let next = state.next_ticket;
                        state.next_ticket = next.wrapping_add(1);
                        if state.record(owner, None, next, request).is_some() {
                            ticket = Some(next);
                        }
                    }
                }
                match self
                    .released
                    .wait_interruptible_timeout(&mut state, conflict_budget)
                {
                    CondVarTimeoutResult::Signal { .. } => return Err(ERESTARTSYS),
                    CondVarTimeoutResult::Timeout => {
                        conflict_budget = 0;
                        expired = true;
                    }
                    CondVarTimeoutResult::Woken { jiffies } => conflict_budget = jiffies,
                }
            } else {
                self.capacity_wait(&mut state, &mut watch, owner, "pair wait")?;
            }
        })();
        state.forget_waiter(ticket);
        result
    }

    /// vmfix6d B: one slice of a capacity wait on the release condition.
    /// `released` is paired with the admission mutex, which the wait drops for
    /// the whole sleep (CondVar::wait_internal runs schedule_timeout through
    /// guard.do_unlocked); no device lock is held by any caller. A stall (no
    /// retirement and an unchanged admission state for progress::STALL_NS) is
    /// reported loudly and the wait continues: failing a submission poisons
    /// the client's whole VM.
    #[inline(never)]
    fn capacity_wait(
        &self,
        state: &mut kernel::sync::MutexGuard<'_, State>,
        watch: &mut Option<Watch>,
        owner: u64,
        what: &str,
    ) -> Result {
        let now = crate::g17::now_ns();
        let current = watch.get_or_insert_with(|| Watch::new(now));
        let fingerprint = state.fingerprint();
        match current.poll(now, true, || fingerprint) {
            Step::Stalled => {
                // vmfix6d: a capacity stall never fails the client (that
                // poisons its VM); keep waiting and report loudly per window.
                pr_warn!("asahi: render {} STALLED: owner={} (no retirement or admission change for {} s; still waiting)\n",
                    what, owner, progress::STALL_NS / 1_000_000_000);
                state.report(what, owner, current, now);
                current.rearm(now);
            }
            Step::Report => state.report(what, owner, current, now),
            Step::Wait => {}
        }
        match self.released.wait_interruptible_timeout(
            state,
            kernel::time::msecs_to_jiffies(SLOT_WAIT_SLICE_MS),
        ) {
            CondVarTimeoutResult::Signal { .. } => Err(ERESTARTSYS),
            _ => Ok(()),
        }
    }

    /// vmfix6d A: admission view of `slot` for render-wait reports.
    pub(crate) fn diag(&self, slot: u8, owner: u64) -> AdmissionDiag {
        self.state.lock().diag(slot, owner)
    }
    /// vmfix6d A/B: hash of the admission state.
    pub(crate) fn fingerprint(&self) -> u64 {
        self.state.lock().fingerprint()
    }

    /// Early publication never waits for overlap; the scheduler retries with
    /// the same footprint after its dependencies have become ready.
    pub(crate) fn try_acquire_render(
        self: &Arc<Self>,
        owner: u64,
        request: Option<Request<'_>>,
    ) -> Result<Option<RenderLease>> {
        if owner == 0 {
            return Ok(None);
        }
        let mut state = self.state.lock();
        if state.refusal(owner).is_err() || !state.owns_any(owner) {
            return Ok(None);
        }
        let plan = state.plan(owner, request, None);
        let Some(slot) = state.pick(owner, plan) else {
            return Ok(None);
        };
        let record = request.and_then(|request| state.record(owner, Some(slot), 0, request));
        Ok(Some(RenderLease {
            pool: self.clone(),
            slot,
            record,
        }))
    }
    pub(crate) fn availability(&self) -> &Arc<Availability> {
        &self.available
    }

    /// Dependency-ready admission reserves depth for the packet, including new
    /// logical owners. A lease counts both prepared and published work, so an
    /// idle pair cannot be lent between preparation and publication.
    pub(crate) fn prepare_render(
        self: &Arc<Self>,
        owner: u64,
        request: Option<Request<'_>>,
    ) -> Result<Option<RenderLease>> {
        if owner == 0 { return Err(EINVAL); }
        let mut state = self.state.lock();
        match state.refusal(owner) {
            Err(error) if error == ENOSPC => return Ok(None),
            result => result?,
        }
        let plan = state.plan(owner, request, None);
        let Some(slot) = state.pick(owner, plan) else { return Ok(None); };
        let record = match request {
            Some(request) => match state.record(owner, Some(slot), 0, request) {
                Some(record) => Some(record),
                None => {
                    state.release(slot);
                    drop(state);
                    self.available.notify();
                    return Err(ENOMEM);
                }
            },
            None => None,
        };
        Ok(Some(RenderLease { pool: self.clone(), slot, record }))
    }

    /// Reserve the idle sibling while its graph is built off-lock. Otherwise
    /// admission could lend that zero-ref slot to a different logical owner.
    pub(crate) fn reserve_warmable_sibling(
        self: &Arc<Self>, owner: u64, publishing: u8,
    ) -> Option<RenderLease> {
        let mut state = self.state.lock();
        let slot = state.warmable_sibling(owner, publishing)?;
        state.take_idle(usize::from(slot), owner);
        Some(RenderLease { pool: self.clone(), slot, record: None })
    }

    /// The pair for `slot` cannot be built: no QID or identity is left. Never
    /// offer it again, and from now on offer only already-built slots.
    #[inline(never)]
    pub(crate) fn mark_unbuildable(&self, slot: u8) {
        if usize::from(slot) >= cfg::RENDER_SLOTS {
            return;
        }
        let mut state = self.state.lock();
        let first = !state.fresh_blocked;
        state.starved |= 1u64 << slot;
        state.fresh_blocked = true;
        let starved = state.starved.count_ones();
        drop(state);
        if first {
            pr_info!("asahi: render pair slot {} unbuildable: reusing built pairs only\n", slot);
        } else {
            static MARKS: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);
            if let Some(n) = sampled(&MARKS) {
                pr_info!("asahi: render pair slot {} unbuildable ({} slots, mark {})\n", slot, starved, n);
            }
        }
        self.released.notify_all();
        self.available.notify();
    }

    /// QID recycling (vmfix6) dismantles `slot`'s parked pair: make the slot
    /// never-built again, atomically with respect to every acquire. Refused
    /// while anything holds, waits on, kills or quarantines the slot.
    #[inline(never)]
    pub(crate) fn unbuild(&self, slot: u8) -> bool {
        let index = usize::from(slot);
        if index == 0 || index >= cfg::RENDER_SLOTS {
            return false;
        }
        let bit = 1u64 << slot;
        let mut state = self.state.lock();
        if state.refs[index] != 0
            || (state.active | state.kill_hold | state.quarantined) & bit != 0
            || state.footprints.iter().any(|entry| entry.slot == Some(slot))
        {
            return false;
        }
        state.owners[index] = 0;
        state.epochs[index] = 0;
        state.lent_from[index] = 0;
        state.recall &= !bit;
        state.released &= !bit;
        state.built &= !bit;
        state.starved = 0;
        // QIDs are free again: fresh pairs can be built until they run out.
        state.fresh_blocked = false;
        drop(state);
        self.released.notify_all();
        self.available.notify();
        true
    }

    /// The pair identities for `slot` exist (set once, at construction).
    #[inline(never)]
    pub(crate) fn mark_built(&self, slot: u8) {
        if usize::from(slot) < cfg::RENDER_SLOTS {
            self.state.lock().built |= 1u64 << slot;
        }
    }

    /// The logical owner admission currently assigns `slot` to (0 = none).
    #[inline(never)]
    pub(crate) fn slot_owner(&self, slot: u8) -> u64 {
        self.state.lock().owners.get(usize::from(slot)).copied().unwrap_or(0)
    }

    #[inline(never)]
    pub(crate) fn is_starved(&self, slot: u8) -> bool {
        usize::from(slot) < cfg::RENDER_SLOTS && self.state.lock().starved & (1u64 << slot) != 0
    }

    /// Admit again an unpublished packet whose pair could not be built. The
    /// scheduler path uses the normal bounded admission wait; the early
    /// (submitting thread) path polls without sleeping interruptibly.
    #[inline(never)]
    pub(crate) fn readmit_render(
        self: &Arc<Self>,
        owner: u64,
        request: Option<Request<'_>>,
        interruptible: bool,
    ) -> Result<RenderLease> {
        if interruptible {
            return self.acquire_render(owner, request);
        }
        // vmfix6d B: progress-bounded, 1 ms polls (the submitting thread does
        // not sleep interruptibly). Hazard ordering yields to Plan::Any after
        // hazard::WAIT_MS polls, as before.
        let mut watch: Option<Watch> = None;
        let mut poll = 0u32;
        loop {
            {
                let mut state = self.state.lock();
                match state.refusal(owner) {
                    Err(error) if error == ENOSPC => {}
                    Err(error) => return Err(error),
                    Ok(()) => {
                        let plan = if poll >= hazard::WAIT_MS {
                            Plan::Any
                        } else {
                            state.plan(owner, request, None)
                        };
                        if let Some(slot) = state.pick(owner, plan) {
                            let record = request
                                .and_then(|request| state.record(owner, Some(slot), 0, request));
                            return Ok(RenderLease {
                                pool: self.clone(),
                                slot,
                                record,
                            });
                        }
                    }
                }
                let now = crate::g17::now_ns();
                let current = watch.get_or_insert_with(|| Watch::new(now));
                let fingerprint = state.fingerprint();
                match current.poll(now, true, || fingerprint) {
                    Step::Stalled => {
                        pr_warn!("asahi: render readmission wait STALLED: owner={} (still waiting)\n", owner);
                        state.report("readmission", owner, current, now);
                        current.rearm(now);
                    }
                    Step::Report => state.report("readmission", owner, current, now),
                    Step::Wait => {}
                }
            }
            poll = poll.saturating_add(1);
            fsleep(Delta::from_millis(1));
        }
    }
    pub(crate) fn release_owner(&self, owner: u64) {
        let mut state = self.state.lock();
        for slot in 0..cfg::RENDER_SLOTS {
            if state.owners[slot] == owner {
                state.released |= 1u64 << slot;
            }
            // A closed lender recalls nothing (vmfix6d C).
            if owner != 0 && state.lent_from[slot] == owner {
                state.lent_from[slot] = 0;
                state.recall &= !(1u64 << slot);
            }
        }
        drop(state);
        self.released.notify_all();
        self.available.notify();
    }
    /// Active leases survive this transition. Their later drop is still the
    /// event which releases capacity, so a stopped owner is never double-freed.
    pub(crate) fn release_retained_owners_after_processor_stop(&self) {
        let mut state = self.state.lock();
        let released = state.released & !state.active;
        for slot in 1..cfg::RENDER_SLOTS {
            if released & (1u64 << slot) != 0 {
                state.owners[slot] = 0;
                state.epochs[slot] = 0;
                state.lent_from[slot] = 0;
                state.recall &= !(1u64 << slot);
            }
        }
        state.released &= !released;
        state.quarantined &= !released;
        state.kill_hold = 0;
        drop(state);
        self.released.notify_all();
        self.available.notify();
    }
}

/// Counted once per userspace logical queue, independently of installed graphs.
pub(crate) struct QueueSlot {
    pool: Arc<Pool>,
}
impl Drop for QueueSlot {
    fn drop(&mut self) {
        self.pool.queues.fetch_sub(1, Ordering::AcqRel);
    }
}

pub(crate) struct RenderLease {
    pool: Arc<Pool>,
    slot: u8,
    record: Option<(u64, u64)>,
}
impl RenderLease {
    pub(crate) fn slot(&self) -> u8 {
        self.slot
    }
    pub(crate) fn quarantine(&self) {
        self.pool.state.lock().quarantined |= 1u64 << self.slot;
        self.pool.released.notify_all();
        self.pool.available.notify();
    }
    /// The VM pin must be retired first. Keep this lease until after clearing
    /// quarantine; its drop is what permits waiting publications to proceed.
    pub(crate) fn rehabilitate_after_processor_stop(&self) {
        self.pool.state.lock().quarantined &= !(1u64 << self.slot);
    }
    /// Requires the runtime's closed-recovery, exact pair-drain witness.
    pub(crate) fn rehabilitate_after_firmware_drain(&self) {
        self.rehabilitate_after_processor_stop();
    }
}
impl Drop for RenderLease {
    fn drop(&mut self) {
        let mut state = self.pool.state.lock();
        if let Some((owner, sequence)) = self.record {
            if let Some(index) = state.footprints.iter().position(|entry| {
                entry.slot == Some(self.slot) && entry.owner == owner && entry.sequence == sequence
            }) {
                state.footprints.swap_remove(index);
            }
        }
        state.release(self.slot);
        drop(state);
        self.pool.released.notify_all();
        self.pool.available.notify();
    }
}

impl State {
    fn hold_for_exit_kill(&mut self, owner: u64, slots: u64) -> bool {
        if owner == 0 || slots & !Self::MASK != 0 {
            return false;
        }
        for slot in 0..cfg::RENDER_SLOTS {
            let bit = 1u64 << slot;
            if slots & bit == 0 {
                continue;
            }
            if self.owners[slot] != owner
                || self.refs[slot] != 0
                || self.active & bit != 0
                || self.released & bit == 0
            {
                return false;
            }
        }
        self.kill_hold |= slots;
        true
    }
}
impl Pool {
    /// Hold the departed owner's idle pairs before publishing its exit record.
    /// Successful publication retains these bits until both processors stop.
    pub(crate) fn hold_render_slots_for_exit_kill(&self, owner: u64, slots: u64) -> bool {
        self.state.lock().hold_for_exit_kill(owner, slots)
    }

    /// Undo only a hold whose control record was not published.
    pub(crate) fn release_exit_kill_hold(&self, slots: u64) {
        if slots == 0 {
            return;
        }
        self.state.lock().kill_hold &= !slots;
        self.released.notify_all();
        self.available.notify();
    }
}
