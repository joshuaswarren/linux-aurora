// SPDX-License-Identifier: GPL-2.0-only OR MIT

//! Republication of render groups that firmware retired without executing.
//! The pair proves the abandonment (retired, unlinked, stamps covering it, no
//! recovery in progress, fragment end never written) and parks the packet;
//! this pass prepares and publishes the same command again under the device
//! mutex, next to the compute replay and never during a recovery.

use super::super::DeferredBatch;
use crate::g17::{object::Allocator, queue::render::RenderAddresses, Firmware};
use kernel::prelude::*;

impl super::RenderState {
    /// (an executed group of the owner retired in `generation` or later,
    ///  a published group of the owner is still live and not parked).
    pub(in crate::g17) fn owner_render_state(&self, owner: u64, generation: u64) -> (bool, bool) {
        let mut executed = false;
        let mut live = false;
        for pair in self.entries.iter().flatten().filter_map(|entry| entry.pair.as_ref()) {
            if pair.owner() != Some(owner) {
                continue;
            }
            executed |= pair.executed_since(generation);
            live |= pair.live_unparked();
        }
        (executed, live)
    }
    /// A parked render of the owner precedes a command waiting through `through`.
    pub(in crate::g17) fn owner_parked_through(&self, owner: u64, through: Option<u64>) -> bool {
        self.entries
            .iter()
            .flatten()
            .filter_map(|entry| entry.pair.as_ref())
            .any(|pair| pair.owner() == Some(owner) && pair.replay_through(through))
    }
    /// Diagnostic: every pair of the owner (executed generation, in-flight state).
    pub(in crate::g17) fn describe_owner(&self, owner: u64) {
        for pair in self.entries.iter().flatten().filter_map(|entry| entry.pair.as_ref()) {
            if pair.owner() == Some(owner) {
                pair.describe();
            }
        }
    }
    /// QIDs of pairs whose front group started tiling and never finished.
    pub(in crate::g17) fn started_unfinished(&self) -> u128 {
        let mut qids = 0u128;
        for pair in self.entries.iter().flatten().filter_map(|entry| entry.pair.as_ref()) {
            if pair.front_started_unfinished() {
                for qid in pair.qids() {
                    qids |= 1u128 << qid;
                }
            }
        }
        qids
    }
    pub(in crate::g17) fn replay_polling(&self) -> bool {
        self.entries
            .iter()
            .flatten()
            .filter_map(|entry| entry.pair.as_ref())
            .any(|pair| pair.replay_polling())
    }
}

impl Firmware {
    pub(in crate::g17) fn replay_render(&mut self, deferred: &mut DeferredBatch) -> Result {
        // Healthy path: no parked replay and no abandonment proof in progress
        // means nothing to do, without reading device memory.
        if !self.queues.render.replay_polling() {
            return Ok(());
        }
        if self.recovery.pending()
            || crate::g17::recovery::Memory::recovery_state(&self.init)? != 0
        {
            return Ok(());
        }
        // A group whose abandonment proof started without a completion event
        // selecting its pair (for example after a recovery or a deferred
        // scheduler timeout) is re-observed here until the proof completes.
        let generation = self.recovery.generation();
        for slot in 0..self.queues.render.entries.len() {
            let recheck = self.queues.render.entries[slot].as_ref().is_some_and(|entry| {
                !(entry.preparing.is_some() || entry.growing || entry.constructing)
                    && entry.pair.as_ref().is_some_and(|pair| pair.abandonment_pending())
            });
            if recheck {
                let progressed = self.with_render(slot, |pair, host| {
                    pair.retire(None, generation, 0, host, &mut |event| deferred.push(event))
                })?;
                if progressed {
                    match self.release_render_lists() {
                        Err(error) if error == EAGAIN => {}
                        result => result?,
                    }
                }
                self.queues.render.collect_retired(slot)?;
            }
        }
        for slot in 0..self.queues.render.entries.len() {
            let Some((packet, owner, qids, closed, busy)) =
                self.queues.render.entries[slot].as_ref().and_then(|entry| {
                    let pair = entry.pair.as_ref()?;
                    let packet = pair.replay_front()?;
                    Some((
                        packet,
                        entry.reservation.owner,
                        pair.qids(),
                        entry.closed,
                        entry.preparing.is_some()
                            || entry.growing
                            || entry.constructing
                            || entry.pending_timeout
                            || entry.pending_error.is_some(),
                    ))
                })
            else {
                continue;
            };
            if busy {
                continue;
            }
            // Republish after the owner's witnessed compute replays, so this
            // render depends on the new commands instead of the dead tokens.
            let through = packet.order.wait_through[1];
            let compute_pending = self.queues.compute.iter().flatten()
                .filter_map(|entry| entry.queue.as_deref())
                .any(|queue| queue.owner() == Some(owner) && queue.replay_pending_through(through));
            if compute_pending {
                let since = self.queues.render.entries[slot].as_ref()
                    .and_then(|entry| entry.pair.as_ref()).map_or(0, |pair| pair.replay_since());
                if crate::g17::now_ns().saturating_sub(since) < 300_000_000 {
                    continue;
                }
            }
            if closed || packet.cancel_requested() || !packet.context.is_current() {
                self.with_render(slot, |pair, _| {
                    pair.fail_replay(ECANCELED, &mut |event| deferred.push(event))
                })?;
                continue;
            }
            // Never republish work of a VM that has recorded an error.
            if packet.completion.status().get() != 0 || packet.completion.guilty() {
                self.with_render(slot, |pair, _| {
                    pair.fail_replay(EIO, &mut |event| deferred.push(event))
                })?;
                continue;
            }
            let addresses = RenderAddresses {
                stamps: [
                    self.init.stamp_addresses(qids[0])?,
                    self.init.stamp_addresses(qids[1])?,
                ],
            };
            let dev = self.primary.state.shared.drm()?;
            let uat = self.uat.clone();
            let prepared = self.with_render(slot, |pair, _| {
                if !pair.room() {
                    return Err(EBUSY);
                }
                let alloc = Allocator {
                    dev: &dev,
                    uat: &uat,
                };
                pair.prepare(&alloc, &packet, addresses)
            });
            match prepared {
                Ok(()) => {}
                Err(error) if error == EBUSY || error == EAGAIN => continue,
                Err(error) => {
                    self.with_render(slot, |pair, _| {
                        pair.fail_replay(error, &mut |event| deferred.push(event))
                    })?;
                    continue;
                }
            }
            match self.publish_render(owner, slot as u8, &packet, deferred) {
                Ok(()) => {
                    if crate::g17::m5_render_submission::enabled() {
                        pr_info!(
                            "render slot {} qids {:?} republished after unexecuted retirement (replay {})\n",
                            slot,
                            qids,
                            packet.completion.replay_count()
                        );
                    }
                    self.with_render(slot, |pair, _| pair.finish_replay(&packet))?;
                }
                Err(error) => {
                    // An unpublished preparation is discarded so the next pass
                    // (or the failure settlement) finds the pair with room.
                    if !packet.is_published() {
                        self.with_render(slot, |pair, _| pair.discard_prepared(&packet))?;
                    }
                    if error != EBUSY && error != EAGAIN {
                        self.with_render(slot, |pair, _| {
                            pair.fail_replay(error, &mut |event| deferred.push(event))
                        })?;
                    }
                }
            }
        }
        Ok(())
    }
}
