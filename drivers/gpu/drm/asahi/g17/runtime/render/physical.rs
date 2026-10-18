// SPDX-License-Identifier: GPL-2.0-only OR MIT

//! Dependency-ready render admission. No command or configuration publication
//! occurs here; the Completion owns the reserved pair until retirement.

use crate::g17::{availability::Wait, job::{Packet, Preparation}, queue::admission::Request};
use core::sync::atomic::Ordering;
use kernel::{prelude::*, sync::Arc};

impl super::super::Backend {
    pub(in crate::g17::runtime) fn prepare_render_physical(
        &self,
        packet: &Arc<Packet>,
        preparation: &mut Preparation,
    ) -> Result<Option<Wait>> {
        if packet.completion.render_slot().is_some() { return Ok(None); }
        let pool = {
            let state = self.shared.state.lock();
            (*state).as_deref().ok_or(ENODEV)?.queues.admission.clone()
        };
        loop {
            if packet.cancel_requested() { return Err(ECANCELED); }
            if self.shared.crashed.load(Ordering::Acquire)
                || packet.completion.status().get() != 0 { return Err(EIO); }
            // Reserve a pool ID before admission can transfer a physical pair.
            // Dropping this temporary pin after success is safe: the pair owns
            // its own pin by then. A capacity miss has changed no pair owner.
            let _pool_claim = if matches!(self.shared.firmware_profile, crate::g17::FirmwareProfile::M5Sptm) {
                let ids = self.shared.state.lock().as_ref().ok_or(ENODEV)?.render_ids.clone();
                let ids_observed = ids.availability().observe();
                let physical_observed = self.shared.physical_available.observe();
                match self.claim_render_pool(packet) {
                    Ok(claim) => Some(claim),
                    Err(error) if error == super::RENDER_CAPACITY_PENDING => {
                        if let Some(mut wait) = ids.availability().wait(ids_observed)? {
                            wait.also_wake(&self.shared.physical_available, physical_observed)?;
                            self.shared.queue_events();
                            return Ok(Some(wait));
                        }
                        continue;
                    }
                    Err(error) => return Err(error),
                }
            } else { None };
            let available = pool.availability();
            let observed = available.observe();
            let Some(lease) = pool.prepare_render(self.owner, Request::of(packet))? else {
                if let Some(wait) = available.wait(observed)? { return Ok(Some(wait)); }
                continue;
            };
            let slot = lease.slot();
            // Slot rollback notifies the admission domain, never this domain.
            // Consequently our own rollback cannot continuously wake this wait.
            let physical = &self.shared.physical_available;
            let mut observed = physical.observe();
            let mut outer_wait = None;
            let result = self.ensure_render(packet, slot, true, Some(&mut observed)).and_then(|()| {
                let mut state = self.shared.state.lock();
                let firmware = (*state).as_deref_mut().ok_or(ENODEV)?;
                if firmware.recovery.pending()
                    || crate::g17::recovery::Memory::recovery_state(&firmware.init)? != 0 {
                    return Err(EAGAIN);
                }
                let entry = firmware.queues.render.entry(slot)?;
                if entry.closed { return Err(ECANCELED); }
                if entry.constructing || entry.growing || entry.preparing.is_some()
                    || entry.pending_error.is_some() || entry.pending_timeout {
                    return Err(EBUSY);
                }
                let pair = entry.pair.as_ref().ok_or(EBUSY)?;
                if !pair.matches(self.owner, &packet.context)? || !pair.room() {
                    return Err(EBUSY);
                }
                let count = pair.outer_slots();
                if matches!(self.shared.firmware_profile, crate::g17::FirmwareProfile::M5Sptm) {
                    if preparation.outer.is_some() { return Err(EIO); }
                    let available = firmware.init.outer_available()?;
                    let observed = available.observe();
                    match firmware.init.reserve_outer(packet.context.policy().priority(), count) {
                        Ok(claim) => preparation.outer = Some(claim),
                        Err(error) if error == EBUSY => {
                            outer_wait = Some((available, observed));
                            return Err(EBUSY);
                        }
                        Err(error) => return Err(error),
                    }
                }
                Ok(())
            });
            match result {
                Ok(()) => {
                    // Admission refs include all prepared and active packets;
                    // the depth limit is the pair's actual retirement capacity.
                    packet.completion.attach_render_lease(lease)?;
                    return Ok(None);
                }
                Err(error) if error == ENOSPC && pool.is_starved(slot) => {
                    // No graph was built. Retry against the now-updated pool,
                    // which chooses existing pairs or waits for their release.
                    drop(lease);
                    continue;
                }
                Err(error) if error == EBUSY || error == EAGAIN
                    || error == super::LEND_PENDING
                    || error == super::RENDER_CAPACITY_PENDING => {
                    drop(lease);
                    let (domain, observed) = outer_wait.unwrap_or_else(|| (physical.clone(), observed));
                    if let Some(wait) = domain.wait(observed)? {
                        // The event worker owns retained-binding collection and
                        // free-list release acknowledgments, even without a new IRQ.
                        self.shared.queue_events();
                        return Ok(Some(wait));
                    }
                }
                Err(error) => return Err(error),
            }
        }
    }
}
