// SPDX-License-Identifier: GPL-2.0-only OR MIT

//! Submission admission, preparation and the firmware visibility boundary.

use super::super::{Backend, DeferredBatch, Registry};
use super::{Site, RENDER_CAPACITY_PENDING};
use crate::g17::{
    job::Packet,
    progress::{self, Step, Watch},
    queue::admission::{RenderLease, Request},
};
use core::sync::atomic::Ordering;
use kernel::{
    prelude::*,
    sync::{Arc, CondVarTimeoutResult},
    time::{delay::fsleep, Delta},
};

/// Each relocation consumes one unbuildable slot; their number is finite.
/// Under render-slot pressure an unbuildable slot can recur; each move is bounded inside.
const MAX_RENDER_RELOCATIONS: u32 = 64;
/// vmfix6d B: a wait first keeps the previous 200 us poll latency for this
/// many polls (short waits behave as before), then sleeps on the device
/// change condition in one-jiffy slices.
const FAST_POLLS: u32 = 8;
/// Work-state nodes are freed off-lock by completion: bounded short retries
/// before the progress watch takes over (unchanged first-attempt latency).
const STATE_FAST_DELAY_US: i64 = 1000;

/// vmfix6d: what an unpublished render packet waits for.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Wait {
    /// The context has no free work-state node.
    WorkState,
    /// A pair lent from an idle live owner still has firmware work.
    Lend,
    /// ensure_render(): building or rebinding the leased pair refused (EBUSY).
    Build,
    /// prepare_render() / publication refused (EBUSY).
    Prepare,
    /// Both render channels still carry unretired work on other pairs.
    Channel,
    /// No buffer identity, metrics slot or retired-binding room (vmfix6d D).
    Capacity,
    /// This attempt found its installed pair borrowed off-lock.
    PairLoan,
}
impl Wait {
    fn name(self) -> &'static str {
        match self {
            Wait::WorkState => "work-state",
            Wait::Lend => "lend",
            Wait::Build => "build",
            Wait::Prepare => "prepare",
            Wait::PairLoan => "pair-loan",
            Wait::Channel => "channel",
            Wait::Capacity => "capacity",
        }
    }
}

impl Backend {
    pub(in crate::g17) fn render_packet(
        &self,
        packet: &Arc<Packet>,
        deferred: &mut DeferredBatch,
    ) -> Result {
        self.render_with_lease(packet, None, deferred)
    }

    /// vmfix6d A: one publisher per logical queue. The early path requires an
    /// empty backlog (every scheduled render packet counts until its publish
    /// returns; drm_sched timeouts of unpublished packets are deferred), so a
    /// second concurrent publisher would be a protocol violation: logged.
    fn enter_publisher(&self) {
        if self.publishing.swap(true, Ordering::AcqRel) {
            static RACES: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);
            if let Some(n) = progress::sampled(&RACES) {
                pr_warn!("asahi: concurrent render publishers on owner {} (race {})\n", self.owner, n);
            }
        }
    }

    fn render_with_lease(
        &self,
        packet: &Arc<Packet>,
        early: Option<RenderLease>,
        deferred: &mut DeferredBatch,
    ) -> Result {
        self.enter_publisher();
        let result = self.render_with_lease_inner(packet, early, deferred);
        self.publishing.store(false, Ordering::Release);
        match result {
            // Kernel-internal codes never reach the client.
            Err(error) if error == RENDER_CAPACITY_PENDING => Err(ENOSPC),
            result => result,
        }
    }

    fn render_with_lease_inner(
        &self,
        packet: &Arc<Packet>,
        early: Option<RenderLease>,
        deferred: &mut DeferredBatch,
    ) -> Result {
        packet.check_dependencies()?;
        if packet.cancel_requested() {
            return Err(ECANCELED);
        }
        if packet.completion.status().get() != 0 {
            return Err(EIO);
        }
        let interruptible = early.is_none();
        // Move the credit token before taking the device mutex. The packet
        // mutex orders prepare/cancel; it is never acquired by a publisher
        // while holding the device mutex.
        let mut outer = packet.preparation.lock().outer.take();
        let prepared = packet.completion.render_slot();
        let mut slot = if let Some(slot) = prepared {
            // Preparation already reserved this pair and its depth. Keep that
            // exact lease through publication and eventual retirement.
            if early.is_some() { return Err(EINVAL); }
            slot
        } else {
            let admission = match early {
                Some(lease) => lease,
                None => {
                    let pool = {
                        let state = self.shared.state.lock();
                        (*state).as_deref().ok_or(ENODEV)?.queues.admission.clone()
                    };
                    pool.acquire_render(self.owner, Request::of(packet))?
                }
            };
            let slot = admission.slot();
            packet.completion.attach_render_lease(admission)?;
            slot
        };
        self.busy_site.store(Site::None as u32, Ordering::Relaxed);
        let mut relocations = 0u32;
        let mut recovery_polls = 0;
        let mut state_delay = 20i64;
        // vmfix6d B: one progress watch for all of this packet's waits.
        let mut watch: Option<Watch> = None;
        loop {
            if packet.cancel_requested() {
                return Err(ECANCELED);
            }
            if packet.completion.status().get() != 0 || self.shared.crashed.load(Ordering::Acquire)
            {
                return Err(EIO);
            }
            if !self.context.is_current() {
                return Err(EFAULT);
            }
            // Sampled before the attempt: a change notified after this point
            // never lets the wait below sleep through it.
            let generation = progress::changes();
            let work_state = {
                let preparation = self.shared.preparations.enter(interruptible)?;
                let result = if prepared.is_some() {
                    // The reversible aggregate claim belongs to Preparation;
                    // publication commits it only after firmware visibility.
                    packet.completion.work_state().and_then(|state| state.node_va()).map(|_| ())
                } else {
                    packet.completion.work_state().and_then(|state| state.acquire()).map(|_| ())
                };
                if preparation.is_current() {
                    result
                } else {
                    Err(EAGAIN)
                }
            };
            let blocked = match work_state {
                Err(error) if error == EAGAIN => continue,
                Err(error) if error == EBUSY => {
                    if state_delay < STATE_FAST_DELAY_US {
                        fsleep(Delta::from_micros(state_delay));
                        state_delay *= 2;
                        continue;
                    }
                    Wait::WorkState
                }
                result => {
                    result?;
                    let mut pair_loan = false;
                    match self.ensure_render(packet, slot, false, None, &mut pair_loan) {
                        Err(error) if error == EAGAIN => continue,
                        Err(error) if error == super::LEND_PENDING => Wait::Lend,
                        Err(error) if error == ENOSPC && !packet.is_published() && {
                            let state = self.shared.state.lock();
                            (*state)
                                .as_deref()
                                .is_some_and(|firmware| firmware.queues.admission.is_starved(slot))
                        } => {
                            // The leased slot has no pair and no QID is left to build
                            // one. Nothing of this packet reached firmware: give the
                            // lease back and take a built pair (bounded admission).
                            if relocations == MAX_RENDER_RELOCATIONS {
                                pr_warn!("asahi: render relocation limit: owner={} slot={}\n", self.owner, slot);
                                return Err(EBUSY);
                            }
                            relocations += 1;
                            let pool = {
                                let state = self.shared.state.lock();
                                (*state).as_deref().ok_or(ENODEV)?.queues.admission.clone()
                            };
                            drop(packet.completion.take_render_lease());
                            let lease = pool.readmit_render(self.owner, Request::of(packet), interruptible)?;
                            static MOVES: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);
                            if let Some(n) = progress::sampled(&MOVES) {
                                pr_info!("asahi: render packet owner={} moved from unbuildable slot {} to slot {} (move {})\n",
                                    self.owner, slot, lease.slot(), n);
                            }
                            slot = lease.slot();
                            packet.completion.attach_render_lease(lease)?;
                            continue;
                        }
                        Err(error) if error == RENDER_CAPACITY_PENDING && !packet.is_published() => {
                            Wait::Capacity
                        }
                        Err(error) if error == EBUSY => {
                            if pair_loan { Wait::PairLoan } else { Wait::Build }
                        }
                        result => {
                            result?;
                            match self.prepare_render(slot, packet, deferred, outer.as_mut(), &mut pair_loan) {
                                Err(error)
                                    if !packet.is_published()
                                        && error == crate::g17::context::RENDER_CHANNEL_PENDING =>
                                {
                                    Wait::Channel
                                }
                                Err(error)
                                    if !packet.is_published()
                                        && error == RENDER_CAPACITY_PENDING =>
                                {
                                    Wait::Capacity
                                }
                                Err(error)
                                    if !packet.is_published() && [EAGAIN, EBUSY].contains(&error) =>
                                {
                                    let recovering = {
                                        let state = self.shared.state.lock();
                                        let firmware = (*state).as_deref().ok_or(ENODEV)?;
                                        firmware.recovery.pending()
                                            || crate::g17::recovery::Memory::recovery_state(&firmware.init)? != 0
                                    };
                                    if recovering {
                                        if recovery_polls == super::super::RECOVERY_SETTLE_POLLS {
                                            if crate::g17::m5_render_submission::enabled() {
                                                dev_warn!(self.shared.dev.as_ref(), "M5 render admission timeout: recovery={} published={}\n",
                                                    recovery_polls, packet.is_published());
                                            }
                                            return Err(ETIMEDOUT);
                                        }
                                        recovery_polls += 1;
                                        self.shared.queue_events();
                                        fsleep(Delta::from_millis(1));
                                        continue;
                                    }
                                    if error == EAGAIN {
                                        continue;
                                    }
                                    if pair_loan { Wait::PairLoan } else { Wait::Prepare }
                                }
                                result => return result,
                            }
                        }
                    }
                }
            };
            self.render_wait(&mut watch, blocked, slot, packet, interruptible, generation)?;
        }
    }

    /// vmfix6d B: one poll of a progress-bounded render wait. Called with no
    /// lock and no preparation lease held (each attempt above drops its own).
    /// Pair-local waits count only a change of their blocker as progress;
    /// capacity waits (capacity, work state, channels, the firmware outer
    /// queue) also count any exact retirement. Either fails, after logging the
    /// full state, only when nothing relevant changed for progress::STALL_NS.
    #[inline(never)]
    fn render_wait(
        &self,
        watch: &mut Option<Watch>,
        kind: Wait,
        slot: u8,
        packet: &Arc<Packet>,
        interruptible: bool,
        generation: u64,
    ) -> Result {
        let now = crate::g17::now_ns();
        let site = self.busy_site.load(Ordering::Relaxed);
        let current = watch.get_or_insert_with(|| {
            static WAITS: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);
            if let Some(n) = progress::sampled(&WAITS) {
                pr_info!("asahi: render owner={} slot={} waits: {} site={} (wait {})\n",
                    self.owner, slot, kind.name(), Site::name(site), n);
            }
            Watch::new(now)
        });
        let global = matches!(kind, Wait::Capacity | Wait::WorkState | Wait::Channel)
            || (kind == Wait::Prepare && site == Site::Publish as u32);
        let step = current.poll(now, global, || self.render_fingerprint(slot, kind, site, global));
        match step {
            Step::Stalled if global => {
                // vmfix6d: a capacity stall never fails the client (a failed
                // submission poisons its whole VM: sticky error, lost device).
                // Keep waiting; report loudly once per stall window.
                self.report_render_wait(slot, packet, kind, site, current, now, true);
                current.rearm(now);
            }
            Step::Stalled => {
                self.report_render_wait(slot, packet, kind, site, current, now, true);
                return Err(ETIMEDOUT);
            }
            Step::Report => self.report_render_wait(slot, packet, kind, site, current, now, false),
            Step::Wait => {}
        }
        if matches!(kind, Wait::Channel | Wait::Capacity) {
            // Retired bindings and channel retirement are settled by the
            // event worker; make sure it runs.
            self.shared.queue_events();
        }
        if kind == Wait::PairLoan {
            return self.wait_render_pair(slot, packet, interruptible);
        }
        if !interruptible {
            // The submitting thread (early path) polls without sleeping
            // interruptibly, backing off to 1.6 ms.
            fsleep(Delta::from_micros(200i64 << current.polls.min(4).saturating_sub(1)));
            return Ok(());
        }
        if current.polls <= FAST_POLLS {
            fsleep(Delta::from_micros(200));
            return Ok(());
        }
        // `changed` is paired with the device mutex. It is the only lock held
        // here, and CondVar::wait_interruptible_timeout releases it for the
        // whole sleep (wait_internal: guard.do_unlocked(schedule_timeout)).
        // The scheduler kworker takes no signals; a spurious return re-polls.
        // Always sleep: under load the generation changes on nearly every
        // attempt, and skipping the wait then spins (hardware: 97k polls/s).
        // A change between the attempt and this wait costs at most one jiffy.
        let _ = generation;
        let mut state = self.shared.state.lock();
        let _ = self.shared.changed.wait_interruptible_timeout(&mut state, 1);
        drop(state);
        Ok(())
    }

    /// Pair restoration and this predicate share the device mutex. A return
    /// before the check skips the sleep; a later return wakes the queued wait.
    /// Unrelated notifications recheck only this predicate, never preparation.
    fn wait_render_pair(&self, slot: u8, packet: &Arc<Packet>, interruptible: bool) -> Result {
        // Unrelated notifications must not renew this helper's time budget.
        // Use the configured kernel tick, not a userspace HZ macro.
        let deadline = crate::g17::now_ns().saturating_add(u64::from(kernel::bindings::TICK_NSEC));
        let mut state = self.shared.state.lock();
        let mut remaining = 1;
        loop {
            let Some(firmware) = (*state).as_deref() else { return Ok(()); };
            let entry = firmware.queues.render.entry(slot)?;
            if entry.closed || entry.pending_error.is_some() || entry.pending_timeout
                || self.shared.crashed.load(Ordering::Acquire)
                || packet.cancel_requested() || packet.completion.status().get() != 0
                || entry.pair.is_some() || entry.preparing.is_none()
            {
                return Ok(());
            }
            if crate::g17::now_ns() >= deadline {
                return Ok(());
            }
            // The early submitter must retain its uninterruptible wait: a
            // pending signal cannot turn a borrowed pair into a busy retry.
            let result = if interruptible {
                self.shared.changed.wait_interruptible_timeout(&mut state, remaining)
            } else {
                self.shared.changed.wait_timeout(&mut state, remaining)
            };
            match result {
                CondVarTimeoutResult::Woken { jiffies } if jiffies != 0 => remaining = jiffies,
                _ => return Ok(()),
            }
        }
    }

    /// vmfix6d B: hash of the state this wait depends on (device mutex).
    #[inline(never)]
    fn render_fingerprint(&self, slot: u8, kind: Wait, site: u32, global: bool) -> u64 {
        let state = self.shared.state.lock();
        let Some(firmware) = (*state).as_deref() else {
            return 0;
        };
        let mut hash = progress::mix(progress::SEED, kind as u64 | u64::from(site) << 8);
        hash = firmware.render_slot_fingerprint(slot, self.context.vm(), hash);
        if global {
            hash = firmware.render_capacity_fingerprint(hash);
        }
        hash
    }

    /// vmfix6d A: the full blocker state of a waiting render publication.
    #[inline(never)]
    #[cold]
    fn report_render_wait(
        &self,
        slot: u8,
        packet: &Arc<Packet>,
        kind: Wait,
        site: u32,
        watch: &Watch,
        now: u64,
        stalled: bool,
    ) {
        if stalled {
            pr_warn!(
                "asahi: render wait STALLED owner={} pid={} ctx={} slot={} kind={} site={} waited={}ms idle={}ms polls={}{}\n",
                self.owner, packet.context.owner_pid(), self.context.id(), slot, kind.name(),
                Site::name(site), watch.elapsed_ms(now), watch.idle_ms(now), watch.polls,
                if matches!(kind, Wait::Capacity | Wait::WorkState | Wait::Channel)
                    || (kind == Wait::Prepare && site == Site::Publish as u32)
                {
                    " (capacity: still waiting)"
                } else {
                    " (failing this submission)"
                }
            );
        }
        pr_info!(
            "asahi: render wait{} owner={} pid={} ctx={} slot={} kind={} site={} waited={}ms idle={}ms polls={}\n",
            if stalled { " STALLED" } else { "" },
            self.owner, packet.context.owner_pid(), self.context.id(), slot, kind.name(),
            Site::name(site), watch.elapsed_ms(now), watch.idle_ms(now), watch.polls
        );
        let state = self.shared.state.lock();
        if let Some(firmware) = (*state).as_deref() {
            firmware.describe_render_blocker(slot, self.owner, &self.context,
                packet.context.policy().priority());
        }
    }

    pub(in crate::g17) fn try_render_early(&self, packet: &Arc<Packet>) -> bool {
        if self.backlog.load(Ordering::Acquire) != 0 || packet.has_dependencies() {
            return false;
        }
        let m5 = matches!(self.shared.firmware_profile, crate::g17::FirmwareProfile::M5Sptm);
        let had_pin = packet.has_pin();
        let _pool_claim = if m5 {
            if packet.ensure_pinned().is_err() { return false; }
            match self.claim_render_pool(packet) {
                Ok(claim) => Some(claim),
                Err(_) => {
                    if !had_pin { packet.unpin_retired(); }
                    return false;
                }
            }
        } else { None };
        let admission = {
            let state = self.shared.state.lock();
            (*state).as_deref().and_then(|firmware| firmware
                .queues
                .admission
                .try_acquire_render(self.owner, Request::of(packet))
                .ok().flatten())
        };
        let Some(admission) = admission else {
            if m5 && !had_pin { packet.unpin_retired(); }
            return false;
        };
        // The scheduler path waits for an identity; this fast path does not.
        if !m5 && packet.ensure_pinned().is_err() {
            return false;
        }
        if !packet.begin_publication() {
            packet.unpin_retired();
            packet.completion.fail_unpublished(ECANCELED);
            packet.set_early_result(Err(ECANCELED));
            return true;
        }
        let mut deferred = match DeferredBatch::publication() {
            Ok(deferred) => deferred,
            Err(error) => {
                if packet.completion.status().get() != 0 {
                    <Self as crate::g17::job::Backend>::fail_vm(self, error);
                }
                packet.unpin_retired();
                packet.completion.fail_unpublished(error);
                packet.set_early_result(Err(error));
                return true;
            }
        };
        let result = self.render_with_lease(packet, Some(admission), &mut deferred);
        let result = self
            .finish_publication(packet, result, &mut deferred)
            .map(|_| ());
        packet.set_early_result(result);
        true
    }
}

impl Registry {
    fn render_dependencies(
        &self,
        owner: u64,
        packet: &Arc<Packet>,
        state: u32,
    ) -> Result<crate::g17::queue::render::RenderDependencies> {
        let mut dependencies = crate::g17::queue::render::RenderDependencies::EMPTY;
        let frontier = self.render_prefix(owner, packet)?;
        for dependency in frontier.as_slice() {
            *dependencies
                .render
                .get_mut(dependencies.render_count)
                .ok_or(EOVERFLOW)? = *dependency;
            dependencies.render_count += 1;
        }
        if dependencies.render_count != 0
            && matches!(&packet.command, crate::g17::command::Validated::Render { pass, .. }
                if pass.fragment_barriers)
        {
            let tiling = self.tiling_prefix(owner, packet)?;
            for dependency in tiling.as_slice() {
                *dependencies
                    .tiling
                    .get_mut(dependencies.tiling_count)
                    .ok_or(EOVERFLOW)? = *dependency;
                dependencies.tiling_count += 1;
            }
        }
        if let Some(queue) = self.compute(owner) {
            dependencies.compute = queue.dependency(
                owner,
                packet.completion.status(),
                packet.order.wait_through[1],
                state,
            )?;
        }
        Ok(dependencies)
    }
}

impl crate::g17::Firmware {
    pub(super) fn apply_render_deferred(
        &mut self,
        slot: u8,
        deferred: &mut DeferredBatch,
    ) -> Result {
        let observe_before_failure = {
            let entry = self.queues.render.entry_mut(slot)?;
            let pair = entry.pair.as_ref().ok_or(EIO)?;
            entry.pending_error.as_ref().is_some_and(|(status, _)| {
                Arc::ptr_eq(pair.context().status(), status)
            })
        };
        if observe_before_failure {
            // VM failure could not observe this pair while preparation owned
            // it. Restore the same final-observation boundary used by
            // settle_render_vm before quarantining any still-owned commands.
            // Keep pending_error intact if observation itself fails.
            self.retire_render_slot(usize::from(slot), None, deferred)?;
        }
        let entry = self.queues.render.entry_mut(slot)?;
        let pair = entry.pair.as_mut().ok_or(EIO)?;
        if let Some((status, error)) = entry.pending_error.take() {
            if Arc::ptr_eq(pair.context().status(), &status) {
                pair.quarantine(error, &mut |event| deferred.push(event))?;
            }
        }
        if entry.closed {
            pair.release_owner();
        }
        if !entry.pending_timeout {
            return Ok(());
        }
        for _ in 0..crate::hw::t8140::queues::RENDER_DEPTH {
            if self.recovery.pending() || self.primary.state.shared.crashed.load(Ordering::Acquire)
            {
                return Ok(());
            }
            let Some((packet, error)) = self
                .queues
                .render
                .entry(slot)?
                .pair
                .as_ref()
                .ok_or(EIO)?
                .take_deferred_timeout()
            else {
                self.queues.render.entry_mut(slot)?.pending_timeout = false;
                return Ok(());
            };
            if let Err(failure) = self.fail_render_packet(&packet, error, deferred) {
                packet.completion.defer_timeout(error);
                return Err(failure);
            }
        }
        self.queues.render.entry_mut(slot)?.pending_timeout = false;
        Ok(())
    }

    pub(super) fn publish_render(
        &mut self,
        owner: u64,
        slot: u8,
        packet: &Arc<Packet>,
        deferred: &mut DeferredBatch,
    ) -> Result {
        use crate::g17::queue::render::PublicationLease;
        let state = crate::g17::recovery::Memory::recovery_state(&self.init)?;
        if self.recovery.pending() || state != 0 {
            return Err(EAGAIN);
        }
        if packet.completion.status().get() != 0 {
            return Err(EIO);
        }
        // Both outer destinations must have room before pool accounting or
        // either inner queue becomes visible. All host producers share this lock.
        let priority = packet.context.policy().priority();
        if !self.init.work_ready(priority, crate::g17::fw::queue::DataMaster::Fragment)?
            || !self.init.work_ready(priority, crate::g17::fw::queue::DataMaster::Tiling)?
        {
            return Err(EBUSY);
        }
        let dependencies = self.queues.render_dependencies(owner, packet, state)?;
        let pair = self.queues.render.entry(slot)?.pair.as_ref().ok_or(EIO)?;
        let pool = pair.pool().clone();
        let buffer_id = pair.buffer_id();
        let pb_state = pair.m5_pb_state_va();
        let buffer_context = pb_state.map(|_| packet.context.id());
        let (buffer_id, buffer_token) = match (pb_state, buffer_context) {
            (Some(pb_va), Some(context)) =>
                self.queues.buffers.acquire_owner_bound(buffer_id, context, pb_va)?,
            _ => self.queues.buffers.acquire_owner(buffer_id)?,
        };
        let preparation = match self.queues.render.pools.prepare(&self.init, &pool) {
            Ok(preparation) => preparation,
            Err(error) => {
                self.queues.buffers.release(buffer_id)?;
                return Err(error);
            }
        };
        let lease = PublicationLease {
            free_list_generation: preparation.generation,
            buffer_token,
            buffer_context,
        };
        let result = self.with_render(usize::from(slot), |pair, host| {
            pair.publish(
                packet.clone(),
                &dependencies,
                lease,
                host,
                &mut |event| deferred.push(event),
            )
        });
        if packet.is_published() {
            self.queues.render.entry_mut(slot)?.published = true;
        } else if result.is_err() {
            self.queues.render.pools.rollback(preparation);
            self.queues.buffers.release(buffer_id)?;
            self.queues
                .render
                .entry_mut(slot)?
                .pair
                .as_mut()
                .ok_or(EIO)?
                .discard_prepared(packet)?;
        }
        result
    }
}
