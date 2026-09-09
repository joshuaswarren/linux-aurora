// SPDX-License-Identifier: GPL-2.0-only OR MIT
// Included in g17_live_boot so the runtime remains the sole firmware owner.

struct G17PAsyncComputeActive {
    packet: Arc<crate::g17_job::G17PComputeJobPacket>,
    _lease: crate::g17_drm::G17PComputeLeaseGuard,
}

/// Installed queue graphs survive logical queue destruction. Firmware retains
/// their addresses; exact command retirement permits rebinding, not freeing.
struct G17PAsyncComputeQueue {
    owner: Option<u64>,
    queue: g17_manager::G17PSksmQueue,
    binding: Option<g17_manager::G17PComputeUserBinding>,
    context: Option<Arc<mmu::T8140ComputeExecutionContext>>,
    active: Option<G17PAsyncComputeActive>,
    released: bool,
    quarantined: bool,
}

impl G17PLiveRuntime {
    /// Both firmware CPUs must have stopped before this is called. Queue
    /// destruction alone cannot settle the scheduler's retained hardware
    /// fences, and a later timeout cannot find a queue we have removed.
    fn release_async_compute_after_processor_stop(&mut self) {
        for q in &self.async_compute_queues {
            if let Some(active) = &q.active {
                if let Ok(completion) = active.packet.completion() {
                    completion.complete(Err(EIO));
                    completion.release_after_processor_stop();
                }
                active._lease.rehabilitate_after_processor_stop();
            }
        }
        self.async_compute_queues.clear();
    }

    pub(crate) fn prepare_queue_execution_context(
        &mut self, vm: &mmu::Vm,
    ) -> Result<Arc<mmu::T8140ComputeExecutionContext>> {
        if !self.accepting_submissions || self.is_crashed() { return Err(EIO); }
        self.manager.as_ref().ok_or(ENODEV)?
            .uat().allocate_t8140_compute_execution_context(vm)
    }

    pub(crate) fn prepare_async_compute_queue(
        &mut self, owner: u64, vm: &mmu::Vm,
        context: Arc<mmu::T8140ComputeExecutionContext>,
    ) -> Result<(u8, Arc<mmu::T8140ComputeExecutionContext>)> {
        if !self.accepting_submissions || self.is_crashed() { return Err(EIO); }
        if !context.matches(vm) || !context.is_current() { return Err(EFAULT); }
        if let Some(q) = self.async_compute_queues.iter().find(|q| q.owner == Some(owner)) {
            return Ok((q.queue.queue_id(), q.context.as_ref().ok_or(EIO)?.clone()));
        }
        let manager = self.manager.as_mut().ok_or(ENODEV)?;
        manager.stage_compute_flist_state()?;
        let free = self.async_compute_queues.iter().position(|q|
            crate::g17_queue_limits::reusable_idle_compute_graph(
                q.owner.is_none(),
                q.active.is_none(),
                q.binding.is_none(),
                q.context.is_none(),
                q.quarantined,
            ));
        let index = if let Some(index) = free { index } else {
            self.async_compute_queues.reserve(1, GFP_KERNEL)?;
            // Reuse the unsubmitted boot graph, preserving QID 4 as first queue.
            let queue = if self.compute_binding.is_none() && self.pending_compute.is_none() {
                self.queue.take()
            } else { None };
            let queue = match queue {
                Some(queue) => queue,
                None => manager.prepare_g17p_compute_queue(self.drm.as_ref().ok_or(ENODEV)?)?,
            };
            self.queue_setup_pending = false;
            let index = self.async_compute_queues.len();
            self.async_compute_queues.push(G17PAsyncComputeQueue {
                owner: None, queue, binding: None, context: None, active: None,
                released: false, quarantined: false,
            }, GFP_KERNEL)?;
            index
        };
        let q = &mut self.async_compute_queues[index];
        let reused_installed = !q.queue.requires_first_install();
        let binding = manager.bind_independent_compute_user_vm(
            &mut q.queue, vm, context.clone(),
        )?;
        q.binding = Some(binding);
        q.context = Some(context.clone());
        q.owner = Some(owner);
        q.released = false;
        if reused_installed {
            if let Some(pdev) = self.pdev.as_ref() {
                dev_info!(
                    pdev.as_ref(),
                    "G17P async compute: rebound retired installed graph qid={} owner={} context={} without reinstall\n",
                    q.queue.queue_id(),
                    owner,
                    context.context_id(),
                );
            }
        }
        Ok((q.queue.queue_id(), context))
    }

    /// Validate the frontend's cached scheduler/backend against the runtime
    /// graph it names. A successful processor-stop recovery drops every
    /// firmware-owned graph, but logical DRM queues can survive that boundary.
    /// In that case an absent owner means the frontend must create a fresh
    /// graph before its next compute submission.
    ///
    /// An owner that still exists but names different hardware state is not a
    /// recovery case: tearing it down here could release another live graph.
    /// Treat that as an internal ownership error instead.
    pub(crate) fn async_compute_queue_matches(
        &self, owner: u64, hardware_queue: u8,
        context: &Arc<mmu::T8140ComputeExecutionContext>,
    ) -> Result<bool> {
        let Some(q) = self.async_compute_queues.iter().find(|q| q.owner == Some(owner)) else {
            return Ok(false);
        };
        let installed_context = q.context.as_ref().ok_or(EIO)?;
        if q.released
            || q.quarantined
            || q.binding.is_none()
            || q.queue.queue_id() != hardware_queue
            || !Arc::ptr_eq(installed_context, context)
            || !context.is_current()
        {
            return Err(EIO);
        }
        Ok(true)
    }

    /// A signaled error fence is still signaled to DRM syncobj waiters. Surface
    /// device failure at the next submit instead of accepting work whose
    /// hardware owner is quarantined and can never make queue progress.
    pub(crate) fn check_async_compute_submission(&self) -> Result {
        if !self.accepting_submissions || self.is_crashed()
            || self.async_compute_queues.iter().any(|q| q.quarantined)
        { return Err(EIO); }
        Ok(())
    }

    pub(crate) fn async_compute_needs_install(&self, owner: u64) -> Result<bool> {
        let q = self.async_compute_queues.iter().find(|q| q.owner == Some(owner)).ok_or(EINVAL)?;
        Ok(q.queue.requires_first_install())
    }

    pub(crate) fn async_compute_dependency(&self, owner: u64) -> Option<kernel::dma_fence::Fence> {
        self.async_compute_queues.iter().find(|q| q.owner == Some(owner) && !q.quarantined)?
            .active.as_ref()?.packet.completion().ok().map(|c| c.fence())
    }

    pub(crate) fn publish_async_compute(
        &mut self, owner: u64, packet: Arc<crate::g17_job::G17PComputeJobPacket>,
        lease: crate::g17_drm::G17PComputeLeaseGuard,
    ) -> Result {
        if !self.accepting_submissions || self.is_crashed() { return Err(EIO); }
        self.settle_open_recovery_before_publication()?;
        // Render jobs now retain independent application-GART roots and
        // job-private low views.  Compute QID4+ has no render context lease to
        // reclaim, so publication may proceed while either render slot runs.
        if *module_parameters::g17p_power_wire.value() == 1 { self.wire_g17p_submit_gpu_power()?; }
        let index = self.async_compute_queues.iter().position(|q| q.owner == Some(owner)).ok_or(EINVAL)?;
        let q = &mut self.async_compute_queues[index];
        if q.active.is_some() || q.quarantined || q.released { return Err(EBUSY); }
        if q.queue.queue_id() != packet.hardware_queue_id()
            || q.context.as_ref().ok_or(EIO)?.context_id() != packet.context().context_id()
        { return Err(EINVAL); }
        let timestamp_start = packet.completion()?.gpu_address();
        let timestamp_end = timestamp_start.checked_add(8).ok_or(EOVERFLOW)?;
        let state = self.gfx_state.as_ref().cloned().ok_or(ENODEV)?;
        state.submission_phase.store(g17_manager::G17P_SUBMIT_PHASE_IDLE, Ordering::Release);
        // From this point, every error retains the packet, mappings and root.
        // A failed notification does not prove the firmware missed the kick.
        q.active = Some(G17PAsyncComputeActive { packet: packet.clone(), _lease: lease });
        let result = (|| {
            let notifications = self.manager.as_mut().ok_or(ENODEV)?.submit_translated_compute(
                self.registers.as_ref().ok_or(ENODEV)?, &mut q.queue,
                &state.submission_phase, q.binding.as_ref().ok_or(ENODEV)?, packet.command(),
                g17_manager::G17PComputeSubmissionContext {
                    context_id: packet.context().context_id(),
                    dispatch_identity: 0x0100_01d7_0200_01dc, execution_gate: 1,
                    timestamps: g17_uapi::ComputeTimestampAddresses { start: timestamp_start, end: timestamp_end },
                    descriptor_flag_4c: false, descriptor_flag_5c8: false,
                    converted_command_timestamp: 1, barriers: &[], mcache: None,
                    rce_kind: 0, auxiliary: 0x003f_ffff_ffff_ffff,
                },
            )?;
            if let Some(notifications) = notifications {
                for message in [notifications.activation, notifications.direct_kick].into_iter().flatten() {
                    fence(Ordering::SeqCst);
                    Pin::new(self.gfx_rtkit.as_mut().ok_or(ENODEV)?)
                        .send_message(g17_rtkit::ENDPOINT_GFX_INTERRUPTS, message)?;
                }
            }
            Ok(())
        })();
        if let Err(error) = result {
            if state.submission_phase.load(Ordering::Acquire) == g17_manager::G17P_SUBMIT_PHASE_IDLE {
                // No firmware-facing publication occurred: release this packet
                // and permit a later valid job to retry the prepared queue.
                self.async_compute_queues[index].active = None;
            } else {
                self.quarantine_async_compute(owner, packet.submission_id(), error);
            }
            return Err(error);
        }
        if *module_parameters::g17p_power_wire.value() >= 2 {
            if let Err(error) = self.wire_g17p_submit_gpu_power() {
                self.quarantine_async_compute(owner, packet.submission_id(), error);
                return Err(error);
            }
        }
        let outstanding = self.async_compute_queues.iter().filter(|q| q.active.is_some()).count();
        dev_info!(self.pdev.as_ref().ok_or(ENODEV)?.as_ref(),
            "G17P async compute: PUBLISHED submission={} owner={} qid={} context={} root={:#x} outstanding={}\n",
            packet.submission_id(), owner, packet.hardware_queue_id(),
            packet.context().context_id(), packet.context().root(), outstanding);
        Ok(())
    }

    fn service_async_compute_completions(&mut self) -> Result {
        let manager = self.manager.as_mut().ok_or(ENODEV)?;
        for q in &mut self.async_compute_queues {
            if q.active.is_none() || q.quarantined { continue; }
            match manager.poll_independent_compute_completion(&mut q.queue) {
                Ok(Some(timestamps)) => {
                    let active = q.active.take().ok_or(EIO)?;
                    dev_info!(self.pdev.as_ref().ok_or(ENODEV)?.as_ref(),
                        "G17P async compute: COMPLETED submission={} qid={} context={} start={} end={}\n",
                        active.packet.submission_id(), q.queue.queue_id(),
                        active.packet.context().context_id(), timestamps[0], timestamps[1]);
                    active.packet.completion()?.complete(Ok(timestamps));
                    drop(active);
                    if q.released {
                        q.binding = None; q.context = None; q.owner = None;
                    }
                }
                // An RTKit edge may precede the final descriptor/stamp cache
                // lines. Leave the job armed; the scheduler's finite timeout
                // performs one final DRAM observation before quarantine.
                Ok(None) => {}
                Err(error) if error == EAGAIN => {}
                Err(error) => {
                    q.quarantined = true;
                    q.active.as_ref().ok_or(EIO)?._lease.quarantine();
                    q.active.as_ref().ok_or(EIO)?.packet.completion()?.fail_while_owned(error);
                }
            }
        }
        Ok(())
    }

    /// Fail every outstanding job that belongs to one VM.
    ///
    /// A submission that fails terminally loses the client's device: Mesa turns
    /// it into `VK_ERROR_DEVICE_LOST` and tears the context down. Work this VM
    /// had ALREADY handed to the hardware is not covered by the submit ioctl's
    /// output-fence guard -- that guard only promises a fence for the ioctl it
    /// runs in -- and once the VM is dead nothing will ever complete it. Its
    /// client then waits on that syncobj forever, which is how one refused
    /// frame ends with gnome-shell alive but permanently stopped in
    /// `drm_syncobj_array_wait_timeout`.
    ///
    /// Two invariants this must not break:
    ///
    ///  * The packet is NOT released. Firmware may still be executing it, and
    ///    freeing its mappings and root under a live GPU turns a lost client
    ///    into a DMA fault. The queue is quarantined and the packet stays
    ///    retained, exactly as the completion-service and scheduler-timeout
    ///    paths already do.
    ///  * The error is recorded on the VM's sticky status BEFORE the fence
    ///    signals. A DRM syncobj treats an error fence as signalled, so a
    ///    waiter wakes and immediately asks why; if the status were still clean
    ///    it would read the destination buffer as if the job had run.
    ///
    /// Jobs belonging to other VMs are untouched: they are independently
    /// rooted, their client is healthy, and completing them here would be the
    /// same "signalled but never executed" lie for someone else.
    pub(crate) fn fail_outstanding_jobs_for_vm(
        &mut self,
        status: &Arc<crate::g17_status::VmStatus>,
        error: Error,
    ) -> usize {
        // A completion may already be sitting in DRAM; take it rather than
        // discarding a job the GPU actually finished.
        let _ = self.service_async_compute_completions();
        let mut failed = 0usize;
        for index in 0..self.async_compute_queues.len() {
            let identity = {
                let q = &self.async_compute_queues[index];
                if q.quarantined {
                    None
                } else {
                    q.active.as_ref().and_then(|active| {
                        let completion = active.packet.completion().ok()?;
                        if !Arc::ptr_eq(&completion.status, status) {
                            return None;
                        }
                        Some((
                            active.packet.submission_id(),
                            q.queue.queue_id(),
                            active.packet.context().context_id(),
                        ))
                    })
                }
            };
            let Some((submission, queue_id, context)) = identity else { continue };
            {
                let q = &mut self.async_compute_queues[index];
                q.quarantined = true;
                let Some(active) = q.active.as_ref() else { continue };
                active._lease.quarantine();
                if let Ok(completion) = active.packet.completion() {
                    completion.fail_while_owned(error);
                }
            }
            failed += 1;
            if let Some(pdev) = &self.pdev {
                dev_err!(
                    pdev.as_ref(),
                    "G17P async compute: FAILING submission={} qid={} context={} after its VM was lost ({:?})\n",
                    submission,
                    queue_id,
                    context,
                    error,
                );
            }
        }
        failed
    }

    pub(crate) fn release_async_compute_queue(&mut self, owner: u64) {
        if let Some(q) = self.async_compute_queues.iter_mut().find(|q| q.owner == Some(owner)) {
            q.released = true;
            if q.active.is_none() && !q.quarantined {
                q.binding = None; q.context = None; q.owner = None;
            }
        }
    }

    pub(crate) fn quarantine_async_compute(&mut self, owner: u64, submission: u64, error: Error) -> bool {
        // One final DRAM-only retirement attempt distinguishes an event-service
        // race from a job whose completion really is absent. Never throw away
        // a proven successful hardware completion merely because a timer ran.
        let _ = self.service_async_compute_completions();
        if let Some(q) = self.async_compute_queues.iter_mut().find(|q| q.owner == Some(owner)) {
            if let Some(active) = &q.active {
                if active.packet.submission_id() == submission {
                    if !q.quarantined {
                        q.quarantined = true;
                        active._lease.quarantine();
                        if let Some(pdev) = &self.pdev {
                            dev_err!(pdev.as_ref(),
                                "G17P async compute: QUARANTINE submission={} qid={} context={} error={:?}\n",
                                submission, q.queue.queue_id(), active.packet.context().context_id(), error);
                        }
                        if let Some(manager) = self.manager.as_mut() {
                            let _ = manager.dump_compute_completion_records("async-timeout");
                        }
                        if let Ok(completion) = active.packet.completion() { completion.fail_while_owned(error); }
                    }
                    return true;
                }
            }
        }
        false
    }
}
