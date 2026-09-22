// SPDX-License-Identifier: GPL-2.0-only OR MIT
// Included in g17_live_boot so the runtime remains the sole firmware owner.

struct G17PAsyncComputeActive {
    packet: Arc<crate::g17_job::G17PComputeJobPacket>,
    _lease: crate::g17_drm::G17PComputeLeaseGuard,
    /// `G17P_COMPLETIONS_SEEN` when this submission was published. Comparing it
    /// against the counter at quarantine time answers the one question the
    /// QUARANTINE line could not: was the firmware still retiring OTHER work
    /// when we gave up on this one? A non-zero delta means the 2000ms wall
    /// clock in `G17P_COMPUTE_COMPLETION_TIMEOUT_MS` expired on a firmware that
    /// was demonstrably alive and making progress, which is a different defect
    /// from a firmware that is stuck. Diagnostic only -- nothing branches on it.
    completions_at_publish: u64,
    retirement: Option<G17PAsyncComputeRetirementTicket>,
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
struct G17PAsyncComputeRetirementTicket {
    owner: u64,
    submission: u64,
    context: u32,
    recovery_epoch: u64,
    firmware: g17_manager::G17PComputeRetirementTicket,
}

/// A prepared alias set does not change the context retained by firmware.
/// Keep the superseded root and aliases until work published under the new
/// context has actually executed, or both firmware processors have stopped.
struct G17PAsyncComputePreviousBinding {
    _binding: Option<g17_manager::G17PComputeUserBinding>,
    _context: Arc<crate::g17_resources::G17PLogicalQueueContext>,
    /// Exact first publication using the replacement binding, and its
    /// firmware recovery epoch. An error/zero-duration fence is not a witness.
    publication: Option<(u64, u64)>,
}

/// Successful async-compute completions this boot. Read by
/// `g17p_quarantine_selftest` and by the quarantine progress witness.
static G17P_COMPLETIONS_SEEN: core::sync::atomic::AtomicU64 =
    core::sync::atomic::AtomicU64::new(0);

/// Installed queue graphs survive logical queue destruction. Firmware retains
/// their addresses; exact command retirement permits rebinding, not freeing.
struct G17PAsyncComputeQueue {
    owner: Option<u64>,
    queue: g17_manager::G17PSksmQueue,
    binding: Option<g17_manager::G17PComputeUserBinding>,
    context: Option<Arc<crate::g17_resources::G17PLogicalQueueContext>>,
    previous_binding: Option<G17PAsyncComputePreviousBinding>,
    active: Option<G17PAsyncComputeActive>,
    released: bool,
    /// The most recent publication passed every queue-local retirement gate.
    /// Consumed when ownership changes or another command is published.
    retirement_ready: bool,
    quarantined: bool,
    /// Quarantined because `drm_sched_fini()` cancelled a pending job when the
    /// client destroyed its queue -- not because anything faulted. The queue is
    /// still permanently retired, so its memory stays safe, but it must not
    /// refuse OTHER clients' submissions: the owner that tore it down is gone.
    retired_by_teardown: bool,
    terminal_fanout: Option<(Arc<crate::g17_status::VmStatus>, Error)>,
}

/// Type-1 firmware events identify completed stamp/QID slots in a 128-bit
/// mask. Keep that identity through dispatch instead of scanning every active
/// queue and its complete 0x800-entry completion ring for one queue's edge.
const fn g17p_async_completion_mask_contains(masks: [u64; 2], queue_id: u8) -> bool {
    if queue_id >= 128 {
        return false;
    }
    let word = queue_id as usize / 64;
    masks[word] & (1u64 << (queue_id as usize % 64)) != 0
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
        if let Some(registry) = &self.queue_registry {
            registry.release_retained_owners_after_processor_stop();
        }
    }

    pub(crate) fn prepare_queue_execution_context(
        &mut self, vm: &mmu::Vm,
    ) -> Result<Arc<crate::g17_resources::G17PLogicalQueueContext>> {
        if !self.accepting_submissions || self.is_crashed() { return Err(EIO); }
        let manager = self.manager.as_mut().ok_or(ENODEV)?;
        manager.prepare_logical_queue_context(self.drm.as_ref().ok_or(ENODEV)?, vm)
    }

    pub(crate) fn prepare_compute_execution_context(
        &mut self,
        context: &Arc<crate::g17_resources::G17PLogicalQueueContext>,
    ) -> Result<Arc<crate::g17_resources::G17PLogicalQueueContext>> {
        if !self.accepting_submissions || self.is_crashed() { return Err(EIO); }
        self.manager.as_mut().ok_or(ENODEV)?.prepare_compute_execution_context(
            context, *module_parameters::g17p_split_execution_id_probe.value() != 0,
        )
    }

    pub(crate) fn prepare_async_compute_queue(
        &mut self, owner: u64, vm: &mmu::Vm,
        context: Arc<crate::g17_resources::G17PLogicalQueueContext>,
    ) -> Result<(u8, Arc<crate::g17_resources::G17PLogicalQueueContext>)> {
        if !self.accepting_submissions || self.is_crashed() { return Err(EIO); }
        if !context.matches(vm) || !context.is_current() { return Err(EFAULT); }
        if let Some(q) = self.async_compute_queues.iter().find(|q| q.owner == Some(owner)) {
            return Ok((q.queue.queue_id(), q.context.as_ref().ok_or(EIO)?.clone()));
        }
        let manager = self.manager.as_mut().ok_or(ENODEV)?;
        manager.stage_compute_flist_state()?;
        let free = self.async_compute_queues.iter().position(|q|
            q.queue.requires_first_install() && q.previous_binding.is_none() &&
            crate::g17_queue_limits::reusable_idle_compute_graph(
                q.owner.is_none(),
                q.active.is_none(),
                q.binding.is_none(),
                q.context.is_none(),
                q.quarantined,
            )).or_else(|| {
                if crate::g17_config::ASYNC_COMPUTE_REBIND == 0 {
                    return None;
                }
                self.async_compute_queues.iter().position(|q|
                    !q.queue.requires_first_install()
                        && q.previous_binding.is_none()
                        && q.binding.is_some()
                        && q.context.is_some()
                        && q.retirement_ready
                        && crate::g17_queue_limits::rebindable_released_compute_graph(
                            q.owner.is_none(),
                            q.active.is_none(),
                            q.released,
                            q.quarantined,
                        )
                )
            });
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
                previous_binding: None,
                released: false, retirement_ready: false,
                quarantined: false, retired_by_teardown: false,
                terminal_fanout: None,
            }, GFP_KERNEL)?;
            index
        };
        let q = &mut self.async_compute_queues[index];
        if q.binding.is_some() != q.context.is_some() {
            return Err(EIO);
        }
        // Finish QID/graph allocation before retaining the incoming page:
        // ENOSPC must not pin a new unused control on every failed prepare.
        // This still precedes fresh alias binding and same-VM retargeting;
        // any failure after pointer exposure keeps the page through stop.
        manager.retain_logical_channel_control(&context.channel_control)?;
        // Same-VM reuse keeps the existing aliases, but logical queue identity
        // is independent of VM identity. Install the incoming root/scheduler
        // owner and retain the old root until replacement work completes.
        if let Some(current) = q.context.as_ref().filter(|current|
            current.matches(vm) && current.is_current()
        ) {
            let previous = current.clone();
            let binding = q.binding.as_mut().ok_or(EIO)?;
            if !binding.matches(vm) { return Err(EFAULT); }
            q.queue.bind_logical_channel_control(&context)?;
            binding.retarget_logical_context(context.clone())?;
            q.previous_binding = Some(G17PAsyncComputePreviousBinding {
                _binding: None, _context: previous, publication: None,
            });
            q.context = Some(context.clone());
            q.owner = Some(owner);
            q.released = false;
            q.retirement_ready = false;
            return Ok((q.queue.queue_id(), context));
        }
        let reused_installed = !q.queue.requires_first_install();
        // All fallible mapping work precedes the ownership transition. An
        // allocation/validation failure leaves the old queue fully bound.
        let binding = manager.bind_independent_compute_user_vm(
            &mut q.queue, vm, context.clone(),
        )?;
        q.queue.bind_logical_channel_control(&context)?;
        if let (Some(binding), Some(context)) = (q.binding.take(), q.context.take()) {
            q.previous_binding = Some(G17PAsyncComputePreviousBinding {
                _binding: Some(binding),
                _context: context,
                publication: None,
            });
        }
        q.binding = Some(binding);
        q.context = Some(context.clone());
        q.owner = Some(owner);
        q.released = false;
        q.retirement_ready = false;
        if reused_installed {
            if let Some(pdev) = self.pdev.as_ref().filter(|_| crate::util::g17p_verbose()) {
                dev_info!(
                    pdev.as_ref(),
                    "G17P async compute: prepared installed graph rebind qid={} owner={} context={} retaining previous binding until completion\n",
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
        context: &Arc<crate::g17_resources::G17PLogicalQueueContext>,
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
    pub(crate) fn check_async_compute_submission(&self, owner: u64) -> Result {
        if !self.accepting_submissions || self.is_crashed() {
            return Err(EIO);
        }
        // A quarantined queue can never make progress, so its own client must
        // be told. Whether every OTHER client is told too is the question:
        // quarantine retires the queue permanently -- nothing it owns is freed
        // or reused -- so the device-wide refusal protects nothing that the
        // retirement does not already protect. See `g17p_quarantine_scope`.
        let scoped = crate::g17_config::QUARANTINE_SCOPE & 1 != 0;
        let blocked = self.async_compute_queues.iter().any(|q| {
            // A queue retired by client teardown never refuses anyone but its
            // own (already departed) owner. Leaving it in the device-wide test
            // was the second half of the kill: even with the session flag not
            // raised, one exited process holding a quarantined queue would
            // refuse EVERY later compute submit on the device for the rest of
            // the boot.
            q.quarantined
                && (!scoped || q.owner == Some(owner))
                && (!q.retired_by_teardown || q.owner == Some(owner))
        });
        if blocked {
            return Err(EIO);
        }
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

    /// A burst experiment must not include first install or a newly rebound
    /// graph. These checks are diagnostic eligibility, not execution capacity.
    pub(crate) fn async_compute_burst_steady(&self, owner: u64) -> bool {
        self.async_compute_queues.iter().find(|q| q.owner == Some(owner))
            .is_some_and(|q| !q.queue.requires_first_install()
                && q.previous_binding.is_none() && q.binding.is_some()
                && q.context.as_ref().is_some_and(|context| context.is_current())
                && q.active.is_none() && !q.released && !q.quarantined)
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
        if crate::g17_config::POWER_WIRE == 1 { self.wire_g17p_submit_gpu_power()?; }
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
        let publication_handshake = self.manager.as_mut().ok_or(ENODEV)?
            .firmware_recovery_handshake_snapshot()?;
        if publication_handshake.state != 0 { return Err(EBUSY); }
        // From this point, every error retains the packet, mappings and root.
        // A failed notification does not prove the firmware missed the kick.
        q.retirement_ready = false;
        q.active = Some(G17PAsyncComputeActive {
            packet: packet.clone(),
            _lease: lease,
            completions_at_publish: G17P_COMPLETIONS_SEEN.load(Ordering::Acquire),
            retirement: None,
        });
        let result = (|| {
            let manager = self.manager.as_mut().ok_or(ENODEV)?;
            let notifications = manager.submit_translated_compute(
                self.registers.as_ref().ok_or(ENODEV)?, &mut q.queue,
                &state.submission_phase, q.binding.as_ref().ok_or(ENODEV)?, packet.command(),
                g17_manager::G17PComputeSubmissionContext {
                    context_id: packet.context().context_id(),
                    dispatch_identity: 0x0100_01d7_0200_01dc,
                    timestamps: g17_uapi::ComputeTimestampAddresses { start: timestamp_start, end: timestamp_end },
                    descriptor_flag_4c: false,
                    converted_command_timestamp: 1, barriers: &[], mcache: None,
                    rce_kind: 0, auxiliary: 0x003f_ffff_ffff_ffff,
                },
            )?;
            let firmware = manager
                .capture_independent_compute_retirement_ticket(&mut q.queue)?;
            let accepted = manager.firmware_recovery_handshake_snapshot()?;
            if accepted.state != 0 || accepted.epoch != publication_handshake.epoch {
                return Err(EIO);
            }
            let retirement = G17PAsyncComputeRetirementTicket {
                owner,
                submission: packet.submission_id(),
                context: packet.context().context_id(),
                recovery_epoch: accepted.epoch,
                firmware,
            };
            let publication = (retirement.submission, retirement.recovery_epoch);
            q.active.as_mut().ok_or(EIO)?.retirement = Some(retirement);
            if let Some(previous) = q.previous_binding.as_mut() {
                previous.publication = Some(publication);
            }
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
                if let Some(previous) = self.async_compute_queues[index].previous_binding.as_mut() {
                    previous.publication = None;
                }
            } else {
                self.quarantine_async_compute(owner, packet.submission_id(), error);
            }
            return Err(error);
        }
        if crate::g17_config::POWER_WIRE >= 2 {
            if let Err(error) = self.wire_g17p_submit_gpu_power() {
                self.quarantine_async_compute(owner, packet.submission_id(), error);
                return Err(error);
            }
        }
        let pdev = self.pdev.as_ref().ok_or(ENODEV)?;
        if crate::util::g17p_verbose()
            && crate::g17_manager::g17p_compute_trace_enabled(packet.submission_id() as u32)
        {
            let outstanding = self.async_compute_queues.iter().filter(|q| q.active.is_some()).count();
            dev_info!(pdev.as_ref(),
                "G17P async compute: PUBLISHED submission={} owner={} qid={} context={} root={:#x} outstanding={}\n",
                packet.submission_id(), owner, packet.hardware_queue_id(),
                packet.context().context_id(), packet.context().root(), outstanding);
        }
        Ok(())
    }

    fn service_async_compute_completions_for_event(&mut self, masks: [u64; 2]) -> Result {
        self.service_async_compute_completions_matching(Some(masks))
    }

    /// Dispatch one drained firmware-event batch. A zero mask has no QID
    /// identity, so it deliberately retains the broad lost-edge/watchdog
    /// observation used before event routing was introduced.
    fn service_async_compute_completion_masks(&mut self, masks: [u64; 2]) -> Result {
        if masks == [0; 2] {
            self.service_async_compute_completions()
        } else {
            self.service_async_compute_completions_for_event(masks)
        }
    }

    /// Full-scan fallback for a final watchdog observation or an event batch
    /// whose record did not carry a usable QID mask. These paths deliberately
    /// preserve the old lost/coalesced-edge recovery behaviour.
    fn service_async_compute_completions(&mut self) -> Result {
        self.service_async_compute_completions_matching(None)
    }

    fn service_async_compute_completions_matching(
        &mut self,
        masks: Option<[u64; 2]>,
    ) -> Result {
        let manager = self.manager.as_mut().ok_or(ENODEV)?;
        manager.begin_independent_compute_completion_batch();
        let mut requested = false;
        for q in &self.async_compute_queues {
            if q.active.is_none() || q.quarantined { continue; }
            if masks.is_some_and(|masks| {
                !g17p_async_completion_mask_contains(masks, q.queue.queue_id())
            }) {
                continue;
            }
            requested |= manager.stage_independent_compute_completion(&q.queue)?;
        }
        if !requested {
            return Ok(());
        }
        manager.scan_independent_compute_completion_batch()?;
        for q in &mut self.async_compute_queues {
            if q.active.is_none() || q.quarantined { continue; }
            if masks.is_some_and(|masks| {
                !g17p_async_completion_mask_contains(masks, q.queue.queue_id())
            }) {
                continue;
            }
            let completion = q.active.as_ref().and_then(|active| active.retirement)
                .ok_or(EIO)
                .and_then(|ticket| {
                    let active = q.active.as_ref().ok_or(EIO)?;
                    if q.owner != Some(ticket.owner)
                        || active.packet.submission_id() != ticket.submission
                        || active.packet.context().context_id() != ticket.context
                    {
                        return Err(EIO);
                    }
                    let handshake = manager.firmware_recovery_handshake_snapshot()?;
                    if handshake.state != 0 || handshake.epoch != ticket.recovery_epoch {
                        return Err(EIO);
                    }
                    let completion = manager.poll_staged_independent_compute_completion(
                        &mut q.queue, &ticket.firmware,
                    )?;
                    if completion.is_some()
                        && q.previous_binding.as_ref().is_some_and(|previous|
                            previous.publication
                                != Some((ticket.submission, ticket.recovery_epoch)))
                    {
                        return Err(EIO);
                    }
                    Ok(completion)
                });
            match completion {
                Ok(Some(timestamps)) => {
                    let active = q.active.take().ok_or(EIO)?;
                    q.retirement_ready = true;
                    // Only the poller's exact descriptor/fresh timestamp/final
                    // stamp success in the publication epoch can release the
                    // superseded aliases. Current binding stays queue-owned.
                    drop(q.previous_binding.take());
                    if let Some(pdev) = self.pdev.as_ref().filter(|_| {
                        crate::util::g17p_verbose()
                            && crate::g17_manager::g17p_compute_trace_enabled(
                                active.packet.submission_id() as u32)
                    }) {
                        dev_info!(pdev.as_ref(),
                            "G17P async compute: COMPLETED submission={} qid={} context={} start={} end={}\n",
                            active.packet.submission_id(), q.queue.queue_id(),
                            active.packet.context().context_id(), timestamps[0], timestamps[1]);
                    }
                    // Reproduction oracle for the quarantine blast radius; see
                    // `g17p_quarantine_selftest`. Off unless the parameter is
                    // set, and it fires at most once per boot.
                    let selftest = *crate::module_parameters::g17p_quarantine_selftest.value();
                    if selftest != 0 {
                        let seen = G17P_COMPLETIONS_SEEN.fetch_add(1, Ordering::AcqRel) + 1;
                        if seen == selftest as u64 {
                            active._lease.quarantine();
                            q.quarantined = true;
                            if let Some(pdev) = self.pdev.as_ref() {
                                dev_err!(pdev.as_ref(),
                                    "G17PMARK quarantine-selftest fired at completion {} qid={} -- the device-wide arm ran; this client still completes\n",
                                    seen, q.queue.queue_id());
                            }
                        }
                    }
                    active.packet.completion()?.complete(Ok(timestamps));
                    drop(active);
                    if q.released {
                        // Retain `binding` and `context`. The firmware still has
                        // this QID installed, so its SKSM entry mapping and
                        // context root may not be dropped here -- see
                        // `rebindable_released_compute_graph`.
                        q.owner = None;
                    }
                }
                // An RTKit edge may precede the final descriptor/stamp cache
                // lines. Leave the job armed; the scheduler's finite timeout
                // performs one final DRAM observation before quarantine.
                Ok(None) => {}
                Err(error) if error == EAGAIN => {}
                Err(error) => {
                    // Zero-duration recovery may retire this queue without
                    // entering quarantine_async_compute. Capture admission
                    // before that path consumes the active packet/lease too.
                    if let (Some(pdev), Some(active)) = (self.pdev.as_ref(), q.active.as_ref()) {
                        dev_info!(pdev.as_ref(),
                            "G17P async compute: admission-at-retirement-error qid={} submission={} errno={} leases(compute,render-mask,install,render-transition,render-quarantine)={:?}\n",
                            q.queue.queue_id(), active.packet.submission_id(), error.to_errno(),
                            active._lease.admission_snapshot());
                    }
                    // This queue is still firmware-owned and cannot be made
                    // reusable from an error fence. Make the proven
                    // processor-stop recovery reachable by the next client.
                    //
                    // Measured 2026-09-11: this killed a device 90s into a boot
                    // whose recovery had just reported sources=0, dm-slot-mask=0,
                    // both render queues at 1057/1057/1057, job list empty, both
                    // fwctl pointers matching and `final clear acknowledged;
                    // state=0 epoch=1`. The hardware was fine. `g17p_quarantine_scope`
                    // bit 0 already means "retire this queue and fail its client,
                    // leave the session up for everyone else"; that is the same
                    // judgement this route needs, so let the one flag cover both.
                    // ENODATA is the zero-duration retire: a submission that
                    // retired without executing, which `g17p_zero_duration`
                    // documents as the expected shape after a fault-blamed
                    // firmware recovery and whose default of 1 means "fail the
                    // submission but do NOT rebuild the firmware session". The
                    // poller used to return EIO for it, so this arm escalated a
                    // documented-expected outcome into a device-wide crash --
                    // every new client ENODEV, and a rebuild known to end in
                    // apple_rtkit_boot ETIME. The queue is still firmware-owned,
                    // so it is retired exactly as before; only the session-wide
                    // escalation is skipped.
                    let submission_scoped = error == ENODATA;
                    if submission_scoped {
                        if let Some(pdev) = &self.pdev {
                            dev_info!(pdev.as_ref(),
                                "G17P async compute: queue retired after a zero-duration retire qid={} -- the client fails, the session stays up\n",
                                q.queue.queue_id());
                        }
                    } else if crate::g17_config::QUARANTINE_SCOPE & 1 == 0 {
                        crate::g17_live_boot::note_session_recovery_pending(
                            &mut self.session_recovery_pending,
                            self.pdev.as_ref(),
                            "async-compute-completion-unclassifiable",
                        );
                    } else if let Some(pdev) = &self.pdev {
                        dev_err!(pdev.as_ref(),
                            "G17PMARK quarantine-scoped unclassifiable-completion qid={} ({:?}) -- queue retired, client fails, session stays up\n",
                            q.queue.queue_id(), error);
                    }
                    q.quarantined = true;
                    q.active.as_ref().ok_or(EIO)?._lease.quarantine();
                    let completion = q.active.as_ref().ok_or(EIO)?.packet.completion()?;
                    completion.fail_while_owned(error);
                    q.terminal_fanout = Some((completion.status.clone(), error));
                }
            }
        }
        // Finish the acquired batch before failing peers. A later queue may
        // have a valid completion in this same snapshot, and proven success
        // must win over another queue's terminal observation. Per-queue slots
        // retain every failed VM identity without allocation or a QID cap.
        for index in 0..self.async_compute_queues.len() {
            let pending = {
                let q = &mut self.async_compute_queues[index];
                let Some(pending) = q.terminal_fanout.take() else { continue; };
                pending
            };
            self.fail_outstanding_jobs_for_vm_after_compute_observation(&pending.0, pending.1);
        }
        Ok(())
    }

    /// Settle one VM after its compute completion state was already observed.
    /// This split lets the completion dispatcher fan out a poll failure after
    /// releasing the manager borrow, without recursion or a second ring scan.
    fn fail_outstanding_jobs_for_vm_after_compute_observation(
        &mut self,
        status: &Arc<crate::g17_status::VmStatus>,
        error: Error,
    ) -> usize {
        for slot in 0..g17_resources::G17PRenderQueuePair::SLOT_COUNT {
            let matches = self.pending_render[slot as usize]
                .as_ref()
                .is_some_and(|completion| completion.belongs_to_vm(status));
            if !matches {
                continue;
            }
            if let Ok(Some(observed)) = self.poll_deferred_render_completion(slot) {
                let _ = self.settle_deferred_render_completion(slot, observed);
            }
        }

        let mut failed = 0usize;
        for slot in 0..g17_resources::G17PRenderQueuePair::SLOT_COUNT {
            let matches = self.pending_render[slot as usize]
                .as_ref()
                .is_some_and(|completion| completion.belongs_to_vm(status));
            if !matches {
                continue;
            }
            crate::g17_live_boot::note_session_recovery_pending(
                &mut self.session_recovery_pending,
                self.pdev.as_ref(),
                "render-completion-failed-with-vm",
            );
            let newly_failed = self.pending_render[slot as usize]
                .as_ref()
                .is_some_and(|completion| completion.fail_while_owned(error));
            if !newly_failed {
                continue;
            }
            failed += 1;
            if let Some(pdev) = &self.pdev {
                dev_err!(
                    pdev.as_ref(),
                    "G17P render: FAILING slot={} after its VM was lost ({:?})\n",
                    slot,
                    error,
                );
            }
        }

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
                crate::g17_live_boot::note_session_recovery_pending(
                    &mut self.session_recovery_pending,
                    self.pdev.as_ref(),
                    "async-compute-vm-fanout-quarantine",
                );
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

    /// Fail every outstanding render or compute job that belongs to one VM.
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
        // Sticky first: both completion kinds publish a signalled error fence,
        // and the woken client must already be able to observe why its VM died.
        status.record(error.to_errno());

        // A completion may already be sitting in DRAM; take it rather than
        // discarding a job the GPU actually finished. Render and compute have
        // independent completion stores, so observe both before the sweep.
        let _ = self.service_async_compute_completions();
        self.fail_outstanding_jobs_for_vm_after_compute_observation(status, error)
    }

    /// Give up a client's claim on its compute graph.
    ///
    /// Reached from `G17PQueue::drop`, i.e. ordinary `queue_destroy` and file
    /// close -- no processor stop, no session rebuild. This used to drop
    /// `binding` and `context` here, which released the client's SKSM entry
    /// `KernelMapping` and, through `T8140ContextRootLease::drop`, retracted its
    /// TTB root pair out of the UAT context table. The firmware, meanwhile,
    /// still had the QID installed: `unregister_compute_sksm_queue` is not even
    /// called on this path, and would be a suppressed no-op if it were, because
    /// `g17p_sksm_mmio` defaults to 0. Its periodic per-slot sweep then read
    /// byte 0 of a record whose page had just been unmapped, which is a data
    /// abort inside the GFX firmware and the end of the session.
    ///
    /// `g17_live_boot::release_compute_owner` states the rule for the
    /// synchronous path in as many words -- "freeing the page tables here would
    /// leave the firmware's compute context pointing at memory the allocator
    /// has taken back" -- and releases the binding immediately before the next
    /// bind reinstalls those roots, or after both CPUs have stopped. This path
    /// gives up the claim but retains everything firmware can still name.
    /// A prepared replacement is also retained until its exact successful
    /// publication completes; preparing aliases alone does not switch roots.
    pub(crate) fn release_async_compute_queue(&mut self, owner: u64) {
        if let Some(index) = self.async_compute_queues.iter().position(|q| q.owner == Some(owner)) {
            let q = &self.async_compute_queues[index];
            if q.active.is_none() && !q.quarantined && q.previous_binding.is_none()
                && self.manager.as_mut().is_some_and(|manager|
                    manager.cancel_unpublished_compute_queue(&q.queue))
            {
                // No firmware-visible publication used this exact lease. Its
                // private graph and client aliases can be dropped immediately.
                // Published or ambiguous owners still follow retention below.
                drop(self.async_compute_queues.remove(index));
                return;
            }
            let q = &mut self.async_compute_queues[index];
            q.released = true;
            if q.active.is_none() && !q.quarantined {
                // A client may disappear before its first publication (or
                // after a definitely-unpublished failure). Cancel only that
                // preparation, restoring the still-installed old binding.
                if q.previous_binding.as_ref().is_some_and(|previous|
                    previous.publication.is_none()
                ) {
                    let previous = q.previous_binding.as_ref().unwrap();
                    let restored = q.queue.bind_logical_channel_control(&previous._context)
                        .and_then(|()| {
                            if previous._binding.is_none() {
                                q.binding.as_mut().ok_or(EIO)?
                                    .retarget_logical_context(previous._context.clone())?;
                            }
                            Ok(())
                        });
                    if restored.is_err() {
                        // Keep both owners and their mappings if restoring an
                        // unpublished handoff cannot be proven. No reuse.
                        q.quarantined = true;
                        q.retired_by_teardown = true;
                        return;
                    }
                    if let Some(previous) = q.previous_binding.take() {
                        if let Some(binding) = previous._binding {
                            q.binding = Some(binding);
                        }
                        q.context = Some(previous._context);
                    }
                    q.retirement_ready = true;
                }
                if q.retirement_ready {
                    q.owner = None;
                }
            }
        }
    }

    pub(crate) fn quarantine_async_compute(&mut self, owner: u64, submission: u64, error: Error) -> bool {
        // One final DRAM-only retirement attempt distinguishes an event-service
        // race from a job whose completion really is absent. Never throw away
        // a proven successful hardware completion merely because a timer ran.
        let _ = self.service_async_compute_completions();
        let mut matched = false;
        let mut terminal_status = None;
        if let Some(q) = self.async_compute_queues.iter_mut().find(|q| q.owner == Some(owner)) {
            if let Some(active) = &q.active {
                if active.packet.submission_id() == submission {
                    matched = true;
                    if !q.quarantined {
                        if let Some(pdev) = &self.pdev {
                            dev_info!(pdev.as_ref(),
                                "G17P async compute: admission-at-quarantine qid={} submission={} leases(compute,render-mask,install,render-transition,render-quarantine)={:?}\n",
                                q.queue.queue_id(), submission,
                                active._lease.admission_snapshot());
                        }
                        // ECANCELED is not a fault. `drm_sched_fini()` is its
                        // only source: `drm_sched_cancel_remaining_jobs()` walks
                        // the pending list and calls `cancel_job` on each entry
                        // purely so their fences are signalled before the
                        // scheduler is torn down. So this errno means "the
                        // client destroyed its queue while a submission was
                        // still pending", which is ordinary lifecycle -- a
                        // process exiting with work in flight -- and says
                        // nothing about the firmware or the hardware.
                        //
                        // Measured on G17P, two boots, both ECANCELED:
                        //   qid=7 submission=448    owner=6    PUBLISHED +271us
                        //   qid=8 submission=297575 owner=1921 PUBLISHED +274us
                        // Both carried `root-current=true`, so the VM root was
                        // still live; neither was preceded by a `publishing
                        // target=` lag line in any boot; and the scheduler's
                        // timeout is 2000ms, three orders of magnitude away.
                        // Nothing timed out and the firmware was never behind.
                        //
                        // Escalating it killed the device: `note_session_
                        // recovery_pending` makes `is_crashed()` true, so every
                        // new client gets ENODEV and every existing one EIO,
                        // and it arms a firmware rebuild that is known to end in
                        // `apple_rtkit_boot` ETIME and ENODEV for the rest of
                        // the boot. A CTS sweep exits a deqp process thousands
                        // of times, so one of them eventually races an unretired
                        // submission and takes the whole GPU with it.
                        //
                        // The QUEUE still quarantines, which is what keeps
                        // firmware-referenced memory safe: a quarantined queue
                        // is permanently retired, `release_async_compute_queue`
                        // will not strip it, `reusable_idle_compute_graph` and
                        // `rebindable_released_compute_graph` exclude it, and
                        // `prepare_async_compute_queue` skips it. The session
                        // flag never contributed to that safety -- see the
                        // `g17p_quarantine_scope` description in asahi.rs.
                        //
                        // Scope: ECANCELED only. ETIMEDOUT and EIO still
                        // escalate exactly as before.
                        let client_teardown = error == ECANCELED;
                        q.retired_by_teardown = client_teardown;
                        if client_teardown {
                            if let Some(pdev) = &self.pdev {
                                dev_info!(pdev.as_ref(),
                                    "G17P async compute: queue retired after client teardown owner={} qid={} submission={} -- drm_sched_fini cancelled a pending job; the session stays up\n",
                                    owner, q.queue.queue_id(), submission);
                            }
                        } else if crate::g17_config::QUARANTINE_SCOPE & 1 == 0 {
                            crate::g17_live_boot::note_session_recovery_pending(
                                &mut self.session_recovery_pending,
                                self.pdev.as_ref(),
                                "async-compute-submission-quarantined",
                            );
                        } else if let Some(pdev) = &self.pdev {
                            dev_err!(pdev.as_ref(),
                                "G17PMARK quarantine-scoped owner={} qid={} -- this queue is retired and its client fails, but the session stays up for every other client\n",
                                owner, q.queue.queue_id());
                        }
                        q.quarantined = true;
                        active._lease.quarantine();
                        if let Some(pdev) = &self.pdev {
                            dev_err!(pdev.as_ref(),
                                "G17P async compute: QUARANTINE submission={} owner={} qid={} context={} vm={} root={:#x} root-current={} global-phase={:?} error={:?}\n",
                                submission, owner, q.queue.queue_id(), active.packet.context().context_id(),
                                active.packet.context().vm_id(), active.packet.context().root(),
                                active.packet.context().is_current(),
                                self.gfx_state.as_ref().map(|s| s.submission_phase.load(Ordering::Acquire)), error);
                            let now = G17P_COMPLETIONS_SEEN.load(Ordering::Acquire);
                            let retired_since = now.saturating_sub(active.completions_at_publish);
                            dev_err!(pdev.as_ref(),
                                "G17PMARK quarantine-progress submission={} qid={} other-completions-retired-since-publish={} -- non-zero means the firmware was alive and retiring work when the {}ms timer gave up on this submission\n",
                                submission, q.queue.queue_id(), retired_since,
                                crate::g17_drm::G17P_COMPUTE_COMPLETION_TIMEOUT_MS);
                        }
                        if let Some(manager) = self.manager.as_mut() {
                            // Sample the actual retained owner, not the legacy
                            // boot queue. Do this before the completion dump's
                            // console output delays the cursor observation.
                            manager.log_async_compute_timeout(&mut q.queue);
                            let _ = manager.dump_compute_completion_records("async-timeout");
                        }
                        if let Ok(completion) = active.packet.completion() {
                            terminal_status = Some(completion.status.clone());
                            completion.fail_while_owned(error);
                        }
                    }
                }
            }
        }
        if let Some(status) = terminal_status {
            self.fail_outstanding_jobs_for_vm(&status, error);
        }
        matched
    }
}
