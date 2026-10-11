// SPDX-License-Identifier: GPL-2.0-only AND MIT

//! Scheduler callbacks and the off-lock fence boundary of a logical queue.

use super::{Backend, DeferredBatch};
use crate::g17::{
    completion::Deferred,
    job::{self, Engine, Packet},
};
use core::sync::atomic::Ordering;
use kernel::{dma_fence::Fence, drm::sched, prelude::*, sync::Arc};

impl Backend {
    fn release_preparation(&self, packet: &Packet) {
        if packet.take_preparation() { packet.context.end_preparation(); }
    }

    pub(super) fn release_backlog(&self, packet: &Packet) {
        if packet.take_backlog() {
            self.backlog.fetch_sub(1, Ordering::AcqRel);
        }
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
            if [EIO, ETIMEDOUT, ENODEV].contains(&error) || packet.completion.status().get() != 0 {
                <Self as job::Backend>::fail_vm(self, error);
            }
            deferred.finish();
            if !packet.is_published() {
                packet.completion.fail_unpublished(error);
            }
            Err(error)
        } else {
            deferred.finish();
            Ok(packet.completion.scheduler_fence())
        }
    }

    fn release_queue(&self) -> Result {
        self.context.mark_closing();
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
        firmware.queues.release_compute(self.owner)?;
        firmware.queues.release_render(self.owner)?;
        firmware.queues.admission.release_owner(self.owner);
        drop(state);
        self.shared.queue_events();
        Ok(())
    }
}

impl job::Backend for Backend {
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
        if let (Some(policy), crate::g17::command::Validated::Compute { resources, plan, .. })
            = (self.shared.render_policy, command) {
            if let Some(programs) = resources {
                *plan = Some(policy.plan_compute(programs, self.context.policy().priority())?);
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
    fn prepare(&self, packet: &Packet) -> Option<Fence> {
        if packet.engine() != Engine::Compute
            || !matches!(self.shared.firmware_profile, crate::g17::FirmwareProfile::M5Sptm)
            || packet.early_result().is_some() {
            return None;
        }
        // This callback runs after dependencies but before drm_sched_job_begin.
        // Allocation and physical-QID admission are host preparation, not GPU
        // execution, and must not consume the hardware watchdog's interval.
        packet.context.begin_preparation();
        if !packet.claim_preparation() {
            packet.context.end_preparation();
            return None;
        }
        if packet.cancel_requested() {
            self.release_preparation(packet);
            return None;
        }
        let required = match &packet.command {
            crate::g17::command::Validated::Compute { plan, .. } => plan.map(|p| p.geometry),
            _ => None,
        };
        if let Err(error) = self.ensure_compute(required) {
            self.release_preparation(packet);
            packet.set_early_result(Err(error));
            packet.completion.fail_unpublished(error);
        }
        None
    }

    fn publish(&self, packet: Arc<Packet>) -> Result<Fence> {
        if let Some(result) = packet.early_result() {
            self.release_preparation(&packet);
            return result.map(|()| packet.completion.scheduler_fence());
        }
        let mut deferred = match DeferredBatch::publication() {
            Ok(batch) => batch,
            Err(error) => {
                self.release_preparation(&packet);
                self.release_backlog(&packet);
                if packet.completion.status().get() != 0 {
                    <Self as job::Backend>::fail_vm(self, error);
                }
                packet.completion.fail_unpublished(error);
                return Err(error);
            }
        };
        let result = if !packet.begin_publication() {
            Err(ECANCELED)
        } else if packet.completion.status().get() != 0 {
            Err(EIO)
        } else {
            match packet.engine() {
                Engine::Compute => self.compute_packet(&packet, &mut deferred),
                Engine::Render => self.render_packet(&packet, &mut deferred),
            }
        };
        self.release_preparation(&packet);
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
        self.release_preparation(&packet);
        self.release_backlog(&packet);
        if packet.request_cancel() {
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
        let mut state = self.shared.state.lock();
        (*state)
            .as_deref_mut()
            .is_some_and(|firmware| firmware.queues.defer_render_timeout(packet, ETIMEDOUT))
    }

    fn try_early(&self, packet: &Arc<Packet>) -> bool {
        self.try_render_early(packet)
    }

    fn count_backlog(&self, packet: &Packet) {
        packet.count_backlog();
        self.backlog.fetch_add(1, Ordering::AcqRel);
    }
}
