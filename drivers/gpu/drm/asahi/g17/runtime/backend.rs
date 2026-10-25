// SPDX-License-Identifier: GPL-2.0-only OR MIT

//! Scheduler callbacks and the off-lock fence boundary of a logical queue.

use super::{Backend, ComputePublication, DeferredBatch};
use crate::g17::{
    completion::Deferred,
    job::{self, Engine, Packet},
};
use core::sync::atomic::Ordering;
use kernel::{dma_fence::Fence, drm::sched, prelude::*, sync::Arc};

impl Backend {
    pub(super) fn release_backlog(&self, packet: &Packet) {
        if packet.take_backlog() {
            self.backlog_for(packet.engine()).fetch_sub(1, Ordering::AcqRel);
        }
    }

    fn backlog_for(&self, engine: Engine) -> &core::sync::atomic::AtomicU32 {
        match engine {
            Engine::Render => &self.backlog,
            Engine::Compute => &self.compute_backlog,
        }
    }

    /// Prepare only a resident compute queue. A miss leaves the scheduler to
    /// handle identities, growth, recovery and capacity without an ioctl wait.
    fn try_prepare_compute_early(&self, packet: &Arc<Packet>) -> bool {
        let required = match &packet.command {
            crate::g17::command::Validated::Compute { plan, .. } => plan.map(|p| p.geometry),
            _ => return false,
        };
        let mut preparation = packet.preparation.lock();
        let had_pin = packet.has_pin();
        let had_compute = preparation.compute.is_some();
        let had_outer = preparation.outer.is_some();
        let prepared = (|| -> Result<bool> {
            if packet.cancel_requested() || packet.completion.status().get() != 0 {
                return Ok(false);
            }
            if packet.prepare_pins()?.is_some() { return Ok(false); }
            preparation.work_state = Some(packet.completion.work_state()?.claim()?);
            self.try_prepare_resident_compute(packet, &mut preparation, required)
        })();
        if matches!(prepared, Ok(true)) {
            preparation.ready = true;
            return true;
        }
        // These claims never became firmware-visible. As in scheduler prepare,
        // release them outside Shared::state, preserving pre-existing claims.
        if !had_outer { drop(preparation.outer.take()); }
        if !had_compute { drop(preparation.compute.take()); }
        drop(preparation.work_state.take());
        if !had_pin { packet.unpin_retired(); }
        false
    }

    fn try_compute_early(&self, packet: &Arc<Packet>) -> bool {
        if !matches!(self.shared.firmware_profile, crate::g17::FirmwareProfile::M5Sptm)
            || self.compute_backlog.load(Ordering::Acquire) != 0
            || packet.has_dependencies()
            || !self.try_prepare_compute_early(packet)
        {
            return false;
        }
        let mut deferred = match DeferredBatch::publication() {
            Ok(batch) => batch,
            Err(_) => {
                self.release_early_compute_preparation(packet, false);
                packet.unpin_retired();
                return false;
            }
        };
        let began = {
            let _preparation = packet.preparation.lock();
            packet.begin_publication()
        };
        let mut result = if !began { Err(ECANCELED) }
            else if packet.completion.status().get() != 0 { Err(EIO) }
            else { self.compute_packet(packet, &mut deferred, true) };
        let retry = began && !packet.is_published()
            && matches!(result, Err(EBUSY) | Err(EAGAIN) | Err(ENOENT));
        self.release_early_compute_preparation(packet, result.is_ok() || packet.is_published());
        if retry {
            packet.unpin_retired();
            let preparation = packet.preparation.lock();
            let reset = packet.retry_unpublished();
            drop(preparation);
            if reset {
                deferred.finish();
                return false;
            }
            result = Err(EIO);
        }
        let result = self.finish_publication(packet, result, &mut deferred).map(|_| ());
        packet.set_early_result(result);
        true
    }

    fn release_early_compute_preparation(&self, packet: &Arc<Packet>, commit: bool) {
        let mut preparation = packet.preparation.lock();
        preparation.ready = false;
        drop(preparation.outer.take());
        if let Some(claim) = preparation.work_state.take() {
            if commit { claim.commit(); }
        }
        drop(preparation.compute.take());
    }

    /// Scheduler dependencies and host pins precede this bounded resident attempt.
    /// A miss restores invisible claims before the ordinary preparation path runs.
    fn try_publish_resident_compute(
        &self,
        packet: &Arc<Packet>,
        preparation: &mut job::Preparation,
        deferred: &mut DeferredBatch,
    ) -> Result<Option<Result>> {
        if !matches!(self.shared.firmware_profile, crate::g17::FirmwareProfile::M5Sptm) {
            return Ok(None);
        }
        let required = match &packet.command {
            crate::g17::command::Validated::Compute { plan, .. } => plan.map(|p| p.geometry),
            _ => return Ok(None),
        };
        let had_compute = preparation.compute.is_some();
        let had_outer = preparation.outer.is_some();
        let result = (|| -> Result<Option<Result>> {
            if packet.cancel_requested() { return Err(ECANCELED); }
            if packet.completion.status().get() != 0 { return Err(EIO); }
            // Match compute_packet's final dependency and work-state checks,
            // before taking Shared or beginning a publication attempt.
            packet.check_dependencies()?;
            let work_word = packet.completion.work_state()?.word();
            if work_word == u32::MAX { return Err(EINVAL); }
            let lease = match self.shared.preparations.try_enter() {
                Ok(lease) => lease,
                Err(error) if matches!(error, EBUSY | EAGAIN) => return Ok(None),
                Err(error) => return Err(error),
            };
            let mut state = self.shared.state.lock();
            let firmware = (*state).as_deref_mut().ok_or(ENODEV)?;
            if !lease.is_current() || self.shared.crashed.load(Ordering::Acquire)
                || firmware.recovery.pending()
                || crate::g17::recovery::Memory::recovery_state(&firmware.init)? != 0
            {
                return Ok(None);
            }
            if !self.prepare_resident_compute_locked(firmware, packet, preparation, required)? {
                return Ok(None);
            }
            if !packet.begin_publication() { return Err(ECANCELED); }
            let result = (|| -> Result {
                if packet.completion.status().get() != 0 { return Err(EIO); }
                if preparation.outer.is_none() { return Err(EINVAL); }
                if !lease.is_current() { return Err(EAGAIN); }
                if packet.cancel_requested() { return Err(ECANCELED); }
                if self.shared.crashed.load(Ordering::Acquire) { return Err(EIO); }
                if firmware.recovery.pending()
                    || crate::g17::recovery::Memory::recovery_state(&firmware.init)? != 0
                {
                    return Err(EAGAIN);
                }
                match self.publish_compute_locked(
                    firmware, packet, deferred, &mut preparation.outer)?
                {
                    ComputePublication::SourcePending => Err(EAGAIN),
                    ComputePublication::Attempted(result) => result,
                }
            })();
            Ok(Some(result))
        })();
        // Release Shared and its lease before this attempt's preparation
        // claims return credits or notify waiters. Cancellation retains its flag.
        let result = match result {
            Ok(Some(Err(error))) if !packet.is_published()
                && matches!(error, EBUSY | EAGAIN | ENOENT) =>
            {
                if packet.retry_unpublished() { Ok(None) }
                else { Ok(Some(Err(EIO))) }
            }
            other => other,
        };
        if matches!(result, Ok(None)) {
            if !had_outer { drop(preparation.outer.take()); }
            if !had_compute { drop(preparation.compute.take()); }
        }
        result
    }

    fn stop_after_error(&self) {
        self.shared.lose_device_quietly();
        self.shared.stop_queues();
    }

    /// Complete all failure fanout before signalling this packet's unpublished
    /// error. Visible failures keep the engine's retained owner through a witness.
    pub(super) fn finish_publication(
        &self,
        packet: &Arc<Packet>,
        result: Result,
        deferred: &mut DeferredBatch,
    ) -> Result<Fence> {
        if let Err(error) = result {
            dev_warn!(self.shared.dev.as_ref(), "GPU publication failed: owner={} pid={} published={} error={:?}\n",
                self.owner, packet.context.owner_pid(), packet.is_published(), error);
            match &packet.command {
                crate::g17::command::Validated::Compute { sampler_count, sampler_heap_va, .. } =>
                    pr_warn!("M5 publication origin: compute sampler_count={} heap={:#x}\n", sampler_count, sampler_heap_va),
                crate::g17::command::Validated::Render { pass, .. } =>
                    pr_warn!("M5 publication origin: render {}x{} layers={} samples={} sample_bytes={} utile={}x{} zls={:#x} depth={:#x}/{:#x} stencil={:#x}/{:#x}\n", pass.width, pass.height, pass.layers, 1u32 << pass.samples_log2, pass.sample_size, pass.utile_width, pass.utile_height, pass.zls_control, pass.depth.va, pass.depth.compression_va, pass.stencil.va, pass.stencil.compression_va),
            }
            if [EIO, ETIMEDOUT, ENODEV].contains(&error) || packet.completion.status().get() != 0 {
                <Self as job::Backend>::fail_vm(self, error);
            }
            deferred.finish();
            if !packet.is_published() {
                // Return the unpublished physical lease before its identity
                // and aggregate work-state claims can become reusable.
                drop(packet.completion.take_render_lease());
                packet.unpin_retired();
                packet.completion.fail_unpublished(error);
            }
            Err(error)
        } else {
            deferred.finish();
            Ok(packet.completion.scheduler_fence())
        }
    }

    /// Scheduler preparation already reserves these identities. The early
    /// path is a nonblocking try; no hardware-capacity wait runs under the
    /// scheduler's execution watchdog.
    pub(super) fn pin_packet(&self, packet: &Arc<Packet>) -> Result {
        packet.ensure_pinned()
    }

    fn release_queue(&self) -> Result {
        let mut state = self.shared.state.lock();
        let Some(firmware) = (*state).as_deref_mut() else {
            return Ok(());
        };
        // Retain a published scheduler before a never-kicked physical graph is
        // removed. The teardown entry owns its root until control consumption.
        firmware
            .queues
            .teardown
            .close(&self.context, super::now_ns())?;
        let growth = firmware.queues.release_compute(self.owner)?;
        let result = firmware.queues.release_render(self.owner);
        if result.is_ok() { firmware.queues.admission.release_owner(self.owner); }
        drop(state);
        drop(growth);
        self.shared.notify_changed();
        self.shared.queue_events();
        result
    }
}

impl job::Backend for Backend {
    fn firmware_compute_sources(&self) -> bool {
        matches!(self.shared.firmware_profile, crate::g17::FirmwareProfile::M5Sptm)
            && *crate::module_parameters::m5_cross_queue_fw_wait.value() & 3 != 0
    }
    fn max_tile_buffer_bytes(&self) -> u64 {
        if matches!(self.shared.firmware_profile, crate::g17::FirmwareProfile::M5Sptm) {
            crate::g17::command::M5_MAX_TILE_BUFFER
        } else {
            crate::g17::command::MAX_TILE_BUFFER
        }
    }
    fn supports_submission(&self) -> bool {
        self.shared.submission_enabled
    }
    fn supports_engine(&self, engine: crate::g17::job::Engine) -> bool {
        !self.shared.firmware_test || engine == crate::g17::job::Engine::Compute
            || crate::g17::m5_render_submission::enabled()
    }
    fn prepare_command(&self, command: &mut crate::g17::command::Validated) -> Result {
        if let (Some(policy), crate::g17::command::Validated::Render { resources, plan, pass, .. })
            = (self.shared.render_policy, &mut *command) {
            let diagnostic;
            let programs = if let Some(programs) = resources.as_ref() { &**programs }
            else if crate::g17::m5_render_submission::enabled() {
                crate::g17::m5_render_submission::validate(pass)?;
                diagnostic = crate::g17::render_resources::Programs::from_entries(&[
                    kernel::uapi::drm_asahi_render_program::default(),
                    kernel::uapi::drm_asahi_render_program { stage: 1,
                        imageblock_bytes: u32::from(pass.sample_size) * 1024,
                        ..Default::default() },
                ])?;
                &diagnostic
            } else { return Err(EINVAL); };
            *plan = Some(policy.plan(programs, self.context.policy().priority())?);
        }
        if let (Some(policy), crate::g17::command::Validated::Compute { resources, plan, scratch, .. })
            = (self.shared.render_policy, command) {
            if let Some(programs) = resources {
                *plan = Some(policy.plan_compute_scs(programs, self.context.policy().priority(),
                    scratch.layout())?);
            } else if crate::g17::fw::m5::compute::ScratchRequest::new(scratch.layout())?.enabled() {
                // SCS needs the dispatch storage declarations to size its pool.
                return Err(EINVAL);
            }
        }
        Ok(())
    }
    fn feed(&self) -> Option<Arc<crate::g17::feed::Feed>> {
        Some(self.shared.feed.clone())
    }

    fn ensure_compute(&self, required: Option<crate::g17::fw::m5::pool::PoolGeometry>) -> Result {
        Backend::ensure_compute(self, required)
    }

    fn fail_vm(&self, error: Error) {
        self.context.status().record(error);
        let mut deferred = match DeferredBatch::worker() {
            Ok(batch) => batch,
            Err(_) => {
                self.stop_after_error();
                return;
            }
        };
        let result = (|| {
            let mut state = self.shared.state.lock();
            let Some(firmware) = (*state).as_deref_mut() else {
                return Ok(());
            };
            // Final observations win over this VM's terminal fanout.
            firmware.scan_compute(None, &mut deferred)?;
            firmware.settle_render_vm(self.context.status(), error, &mut deferred)?;
            firmware
                .queues
                .fail_compute_vm(self.context.status(), error, &mut deferred)
        })();
        deferred.finish();
        if result.is_err() {
            self.stop_after_error();
        }
    }

    fn release_owner(&self) {
        if self.release_queue().is_err() {
            self.stop_after_error();
        }
    }

    /// Firmware kick parents order the same engine. Explicit cross-engine and
    /// input dependencies were already attached to the DRM scheduled job.
    fn prepare(&self, packet: &Arc<Packet>) -> Option<Fence> {
        let mut resident_deferred = if packet.early_result().is_none()
            && packet.engine() == Engine::Compute
            && matches!(self.shared.firmware_profile, crate::g17::FirmwareProfile::M5Sptm)
        {
            DeferredBatch::publication().ok()
        } else { None };
        let mut resident_result = None;
        let mut preparation = packet.preparation.lock();
        let stop_observed = self.shared.admission_stop.observe();
        // The scheduler has consumed dependencies before this callback. Early
        // publication and replay already own their original reservations.
        if packet.early_result().is_some() || preparation.ready { return None; }
        drop(preparation.wait.take());
        let had_pin = packet.has_pin();
        let had_compute = preparation.compute.is_some();
        let had_outer = preparation.outer.is_some();
        let had_render = packet.completion.render_slot().is_some();
        let result = (|| -> Result<Option<crate::g17::availability::Wait>> {
            if packet.cancel_requested() { return Err(ECANCELED); }
            if packet.completion.status().get() != 0
                || self.shared.crashed.load(Ordering::Acquire) { return Err(EIO); }
            packet.check_dependencies()?;
            if let Some(wait) = packet.prepare_pins()? { return Ok(Some(wait)); }
            loop {
                let domain = packet.context.work_state_availability();
                let observed = domain.observe();
                match packet.completion.work_state()?.claim() {
                    Ok(claim) => {
                        preparation.work_state = Some(claim);
                        break;
                    }
                    Err(error) if error == EBUSY => {
                        if let Some(wait) = domain.wait(observed)? { return Ok(Some(wait)); }
                    }
                    Err(error) => return Err(error),
                }
            }
            if let Some(deferred) = resident_deferred.as_mut() {
                if let Some(result) = self.try_publish_resident_compute(
                    packet, &mut preparation, deferred)?
                {
                    resident_result = Some(result);
                    return Ok(None);
                }
            }
            self.prepare_physical(packet, &mut preparation)
        })().and_then(|wait| {
            if let Some(mut wait) = wait {
                wait.also_wake(&self.shared.admission_stop, stop_observed)?;
                Ok(Some(wait))
            } else { Ok(None) }
        });
        if let Some(result) = resident_result {
            preparation.ready = false;
            drop(preparation.outer.take());
            if let Some(claim) = preparation.work_state.take() {
                if result.is_ok() || packet.is_published() { claim.commit(); }
            }
            drop(preparation.compute.take());
            drop(preparation);
            self.release_backlog(packet);
            let result = self.finish_publication(
                packet, result, resident_deferred.as_mut().expect("resident collector"))
                .map(|_| ());
            packet.set_early_result(result);
            return None;
        }
        let wait = match result {
            Ok(None) => { preparation.ready = true; None }
            Ok(Some(wait)) => {
                // These claims never reached firmware. An aggregate node with
                // a committed or still-preparing sibling remains retained.
                if !had_outer { drop(preparation.outer.take()); }
                if !had_render { drop(packet.completion.take_render_lease()); }
                if !had_compute { drop(preparation.compute.take()); }
                drop(preparation.work_state.take());
                if !had_pin { packet.unpin_retired(); }
                let fence = wait.fence();
                preparation.wait = Some(wait);
                Some(fence)
            }
            Err(error) => {
                if !had_outer { drop(preparation.outer.take()); }
                if !had_render { drop(packet.completion.take_render_lease()); }
                if !had_compute { drop(preparation.compute.take()); }
                drop(preparation.work_state.take());
                if !had_pin { packet.unpin_retired(); }
                packet.set_early_result(Err(error));
                self.release_backlog(packet);
                packet.completion.fail_unpublished(error);
                None
            }
        };
        drop(preparation);
        if let Some(deferred) = resident_deferred.as_mut() { deferred.finish(); }
        wait
    }

    fn publish(&self, packet: Arc<Packet>) -> Result<Fence> {
        if let Some(result) = packet.early_result() {
            return result.map(|()| packet.completion.scheduler_fence());
        }
        let mut deferred = match DeferredBatch::publication() {
            Ok(batch) => batch,
            Err(error) => {
                {
                    let mut preparation = packet.preparation.lock();
                    drop(preparation.outer.take());
                    drop(packet.completion.take_render_lease());
                    drop(preparation.compute.take());
                    drop(preparation.work_state.take());
                    drop(preparation.wait.take());
                    packet.unpin_retired();
                }
                self.release_backlog(&packet);
                if packet.completion.status().get() != 0 {
                    <Self as job::Backend>::fail_vm(self, error);
                }
                packet.completion.fail_unpublished(error);
                return Err(error);
            }
        };
        let began = {
            let mut preparation = packet.preparation.lock();
            let began = packet.begin_publication();
            drop(preparation.wait.take());
            began
        };
        let result = if !began {
            Err(ECANCELED)
        } else if packet.completion.status().get() != 0 {
            Err(EIO)
        } else if let Err(error) = self.pin_packet(&packet) {
            Err(error)
        } else {
            match packet.engine() {
                Engine::Compute => self.compute_packet(&packet, &mut deferred, false),
                Engine::Render => self.render_packet(&packet, &mut deferred),
            }
        };
        // Physical prepare claims protect the checked queue until publication
        // has either installed its Active owner or failed without visibility.
        {
            let mut preparation = packet.preparation.lock();
            drop(preparation.outer.take());
            if result.is_err() && !packet.is_published() { drop(packet.completion.take_render_lease()); }
            if let Some(claim) = preparation.work_state.take() {
                if result.is_ok() || packet.is_published() { claim.commit(); }
            }
            drop(preparation.compute.take());
        }
        self.release_backlog(&packet);
        self.finish_publication(&packet, result, &mut deferred)
    }

    fn timed_out(&self, packet: Arc<Packet>) -> sched::Status {
        self.release_backlog(&packet);
        let result = match packet.engine() {
            Engine::Compute => self.timeout_compute(&packet),
            Engine::Render => self.timeout_render(&packet),
        };
        if result.is_err() {
            self.stop_after_error();
        }
        sched::Status::NoHang
    }

    fn cancel(&self, packet: Arc<Packet>) {
        self.release_backlog(&packet);
        let cancelled = {
            let mut preparation = packet.preparation.lock();
            let cancelled = packet.request_cancel();
            if cancelled {
                drop(preparation.outer.take());
                drop(packet.completion.take_render_lease());
                drop(preparation.compute.take());
                drop(preparation.work_state.take());
                packet.unpin_retired();
            }
            drop(preparation.wait.take());
            cancelled
        };
        if cancelled {
            Deferred::Unpublished(packet.completion.clone(), ECANCELED).finish();
            return;
        }
        let result = match packet.engine() {
            Engine::Compute => self.cancel_compute(&packet),
            Engine::Render => self.cancel_render(&packet),
        };
        if result.is_err() {
            self.stop_after_error();
        }
    }

    fn defer_timeout(&self, packet: &Arc<Packet>, renewals: u8) -> bool {
        if packet.engine() == Engine::Compute {
            return self.defer_compute_timeout(packet, renewals);
        }
        // drm_sched starts the job timer before run_job. A render publisher
        // can still be waiting for admission or holding its pair off-lock
        // (provider work, restaging) when the timer expires; nothing of this
        // packet is on the GPU yet. Renew until publication completes or the
        // publisher fails the packet itself. Timing out here made
        // timeout_render find the pair checked out (EBUSY) and stop the whole
        // runtime while the publisher was still running.
        if !packet.is_published() {
            return true;
        }
        let mut state = self.shared.state.lock();
        (*state)
            .as_deref_mut()
            .is_some_and(|firmware| firmware.queues.defer_render_timeout(packet, ETIMEDOUT))
    }

    fn try_early(&self, packet: &Arc<Packet>) -> bool {
        match packet.engine() {
            Engine::Render => self.try_render_early(packet),
            Engine::Compute => self.try_compute_early(packet),
        }
    }

    fn count_backlog(&self, packet: &Packet) {
        packet.count_backlog();
        self.backlog_for(packet.engine()).fetch_add(1, Ordering::AcqRel);
    }
}
