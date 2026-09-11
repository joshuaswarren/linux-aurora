// SPDX-License-Identifier: GPL-2.0-only OR MIT
//! M5-specific initialization transaction. No G17P manager or DRM admission.

use super::{validate_provider, ExpectedProvider, G17PAscwrapV6Role,
    G17PMailboxProvider, G17PNoRtkitBuffer};
use crate::{driver::AsahiDevice, g17_resources::m5::M5FirmwareMemory,
    g17_rtkit, mmu, regs};
use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use kernel::{device::Core, macros::vtable, platform, prelude::*,
    soc::apple::rtkit, sync::{aref::ARef, Arc}, time::{delay::fsleep, Delta}};

struct M5RoleState {
    dev: ARef<AsahiDevice>,
    role: usize,
    acked: AtomicBool,
    crashed: AtomicBool,
    events: AtomicU32,
}

struct M5RtkitOps;

#[vtable]
impl rtkit::Operations for M5RtkitOps {
    type Data = Arc<M5RoleState>;
    type Buffer = G17PNoRtkitBuffer;

    fn recv_message_early(
        data: <Self::Data as kernel::types::ForeignOwnable>::Borrowed<'_>,
        endpoint: u8,
        message: u64,
    ) -> bool {
        // Both native M5 roles reply with exactly 0x0009000000000000.
        if endpoint == 0x20 && message == 0x0009_0000_0000_0000 {
            data.acked.store(true, Ordering::Release);
            true
        } else {
            false
        }
    }

    fn recv_message(
        data: <Self::Data as kernel::types::ForeignOwnable>::Borrowed<'_>,
        endpoint: u8,
        message: u64,
    ) {
        let ordinal = data.events.fetch_add(1, Ordering::Relaxed);
        if ordinal < 8 {
            dev_info!(data.dev.as_ref(), "M5 message role={} ep={:#x} payload={:#x}\n",
                data.role, endpoint, message);
        }
    }

    fn crashed(
        data: <Self::Data as kernel::types::ForeignOwnable>::Borrowed<'_>,
        _crashlog: Option<&[u8]>,
    ) {
        data.crashed.store(true, Ordering::Release);
        dev_err!(data.dev.as_ref(), "M5 firmware crashed role={}\n", data.role);
    }
}

struct M5Role {
    provider: G17PMailboxProvider,
    rtkit: rtkit::RtKit<M5RtkitOps>,
    state: Arc<M5RoleState>,
    started: bool,
}

struct M5Session {
    roles: [M5Role; 2],
    memory: Option<M5FirmwareMemory>,
    uat: Arc<mmu::Uat>,
    registers: regs::Resources,
    dev: ARef<AsahiDevice>,
}

impl M5Session {
    fn stop_cpus(&mut self) -> Result {
        let mut failure = None;
        // Reverse the measured M5 GFX1/GFX start order.
        for index in [0, 1] {
            if self.roles[index].started {
                match self.roles[index].provider.stop_cpu() {
                    Ok(()) => self.roles[index].started = false,
                    Err(error) => failure = Some(error),
                }
            }
        }
        failure.map_or(Ok(()), Err)
    }

    fn initialize(&mut self, probe_kind: u32) -> Result {
        self.registers.m5_match_pmgr_boot_control()?;
        for index in [1, 0] {
            let role = &mut self.roles[index];
            role.provider.start_cpu()?;
            role.started = true;
            Pin::new(&mut role.rtkit).boot()?;
            dev_info!(self.dev.as_ref(), "M5_INIT role={} rtkit-ready\n", index);
        }
        // These helpers operate only on the proven shared dual-root layout;
        // the Neo-specific contexts, metrics and manager remain unselected.
        let roots = self.uat.prepare_t8140_firmware_high_roots()?;
        let timestamp: u64;
        // SAFETY: Read-only architectural counter, available on this target.
        unsafe { core::arch::asm!("mrs {value}, cntvct_el0", value = out(reg) timestamp,
            options(nomem, nostack, preserves_flags)); }
        self.memory = Some(M5FirmwareMemory::new_with_uat(
            &self.dev, self.uat.clone(), timestamp, probe_kind)?);
        self.uat.confirm_t8140_primary_high_root(roots)?;
        self.uat.mirror_t8140_secondary_high_root()?;
        dev_info!(self.dev.as_ref(), "M5_INIT retained memory graph mapped\n");
        self.registers.m5_pre_initdata_clear()?;

        // M5's own trace publishes and ACKs GFX1 first (133/137), then GFX
        // (160/164). The Neo's opposite ordering is not used here.
        for index in [1, 0] {
            let address = self.memory.as_ref().ok_or(EIO)?.root(index == 1);
            let message = g17_rtkit::encode_initdata_doorbell(address).map_err(|_| EINVAL)?;
            let role = &mut self.roles[index];
            Pin::new(&mut role.rtkit).start_endpoint(0x20)?;
            Pin::new(&mut role.rtkit).start_endpoint(0x21)?;
            Pin::new(&mut role.rtkit).send_message(0x20, message)?;
            dev_info!(self.dev.as_ref(), "M5_INIT role={} published root={:#x}\n", index, address);
            for _ in 0..200 {
                if self.memory.as_mut().ok_or(EIO)?.drain_reports(&self.dev)? {
                    return Err(EIO);
                }
                if role.state.crashed.load(Ordering::Acquire) {
                    return Err(EIO);
                }
                if role.state.acked.load(Ordering::Acquire) {
                    break;
                }
                fsleep(Delta::from_millis(10));
            }
            if !role.state.acked.load(Ordering::Acquire) {
                return Err(ETIMEDOUT);
            }
            dev_info!(self.dev.as_ref(), "M5_INIT role={} initdata-ack\n", index);
        }
        if self.roles.iter().any(|role| role.state.crashed.load(Ordering::Acquire)) {
            return Err(EIO);
        }
        if probe_kind >= 2 {
            self.retire_openings()?;
        }
        if probe_kind >= 3 {
            self.install_queue_probe()?;
        }
        if probe_kind >= 5 {
            let roots = self.memory.as_mut().ok_or(EIO)?.validate_program_context()?;
            dev_info!(self.dev.as_ref(), "M5_CONTEXT prepared id3 roots={:x?}; six pages, 1024 untouched outputs, no kicks\n", roots);
        }
        if probe_kind >= 6 {
            self.retire_idle_policy()?;
            self.submit_compute(probe_kind != 7, probe_kind == 8, probe_kind >= 9,
                                probe_kind == 10)?;
        }
        Ok(())
    }

    fn retire_idle_policy(&mut self) -> Result {
        let memory = self.memory.as_mut().ok_or(EIO)?;
        dev_info!(self.dev.as_ref(), "M5_POWER before wake {:x?}\n", memory.power_status()?);
        // Retire the idle-policy request. Reserved DATA snapshots retain
        // pause2/powerflags4 during successful native GPU work as well;
        // those observations do not establish whether the GPU is awake.
        memory.publish_idle_wake()?;
        Pin::new(&mut self.roles[0].rtkit).send_message(0x21, 0x0084_0000_0000_0011)?;
        let mut awake = false;
        for _ in 0..1000 {
            if memory.drain_reports(&self.dev)? { return Err(EIO); }
            if self.roles.iter().any(|role| role.state.crashed.load(Ordering::Acquire)) {
                dev_err!(self.dev.as_ref(), "M5_POWER firmware crash {:x?}\n", memory.power_status()?);
                return Err(EIO);
            }
            let state = memory.power_status()?;
            if memory.control_counters()? == [[2; 3], [1; 3]] {
                dev_info!(self.dev.as_ref(), "M5_POWER idle policy retired; DATA observation {:x?}\n", state);
                awake = true;
                break;
            }
            fsleep(Delta::from_millis(1));
        }
        if !awake {
            dev_err!(self.dev.as_ref(), "M5_POWER wake timeout counters={:?} state={:x?}\n",
                memory.control_counters()?, memory.power_status()?);
            return Err(ETIMEDOUT);
        }
        Ok(())
    }

    fn submit_compute(&mut self, submit_kick: bool, freeze_after_admission: bool,
                      live_snapshot: bool, pipe_monitor: bool) -> Result {
        if (freeze_after_admission || live_snapshot)
            && *crate::module_parameters::m5_fw_trace.value() & 3 != 3 {
            return Err(EINVAL);
        }
        let memory = self.memory.as_mut().ok_or(EIO)?;
        let state = memory.compute_pool_state()?;
        if !M5FirmwareMemory::compute_pool_ready(state) {
            dev_err!(self.dev.as_ref(), "M5_POOL host preparation invalid {:x?}\n", state);
            return Err(EIO);
        }
        dev_info!(self.dev.as_ref(), "M5_POOL host directory and UMA slot prepared {:x?}; no growth request\n", state);
        memory.publish_compute_job(submit_kick)?;
        Pin::new(&mut self.roles[0].rtkit).send_message(0x21, 0x0083_0000_0000_000a)?;
        if !submit_kick {
            for _ in 0..1000 {
                if memory.drain_reports(&self.dev)? { return Err(EIO); }
                if self.roles.iter().any(|role| role.state.crashed.load(Ordering::Acquire)) {
                    return Err(EIO);
                }
                let (outer, inner, _) = memory.queue_probe_status()?;
                let result = memory.compute_result()?;
                if result.inputs_changed != 0 || result.outputs_untouched != 1024 {
                    return Err(EIO);
                }
                if outer == [2; 3] && inner == [3; 3] {
                    dev_info!(self.dev.as_ref(), "M5_COMPUTE setup retired outer={:?} inner={:?}; 1024 untouched outputs, SKSM kick unpublished\n",
                        outer, inner);
                    return Ok(());
                }
                fsleep(Delta::from_millis(1));
            }
            dev_err!(self.dev.as_ref(), "M5_COMPUTE setup timeout queues={:?}\n", memory.queue_probe_status()?);
            return Err(ETIMEDOUT);
        }
        dev_info!(self.dev.as_ref(), "M5_COMPUTE first SKSM kick submitted qid11 stamp1 context3\n");
        if freeze_after_admission {
            // Preserve the ordinary kick but return to the existing CPU-stop
            // path after the kick trace and its command callback return trace.
            // This checkpoint proves neither shader completion nor recovery.
            for _ in 0..10 {
                if memory.drain_reports(&self.dev)? { return Err(EIO); }
                if self.roles.iter().any(|role| role.state.crashed.load(Ordering::Acquire)) {
                    return Err(EIO);
                }
                let (outer, inner, _) = memory.queue_probe_status()?;
                let result = memory.compute_result()?;
                if result.inputs_changed != 0 { return Err(EIO); }
                if memory.compute_kick_observed() {
                    dev_info!(self.dev.as_ref(), "M5_KSM admission checkpoint outer={:?} inner={:?} outputs_wrong={} untouched={}\n",
                        outer, inner, result.outputs_wrong, result.outputs_untouched);
                    return Ok(());
                }
                fsleep(Delta::from_millis(1));
            }
            return Err(ETIMEDOUT);
        }
        let mut live_snapshots = 0;
        for poll in 0..10000 {
            if memory.drain_reports(&self.dev)? {
                dev_err!(self.dev.as_ref(), "M5_COMPUTE firmware requested GPU restart\n");
                break;
            }
            if self.roles.iter().any(|role| role.state.crashed.load(Ordering::Acquire)) { break; }
            // Retain the first four observations, then follow DM2 progress
            // at polls32/64/128/256 before its watchdog expires. An early
            // zero alone cannot distinguish a delayed launch from a stall.
            let live_limit = if pipe_monitor { 8 } else { 4 };
            if live_snapshot && memory.compute_kick_observed() && live_snapshots < live_limit
                && (live_snapshots < 4 || poll >= (1 << (live_snapshots + 1))) {
                let started = kernel::time::Instant::<kernel::time::Monotonic>::now();
                let observed = self.registers.m5_live_sksm_state();
                let monitor = if pipe_monitor { Some(self.registers.m5_live_compute_monitor()) }
                              else { None };
                let elapsed = started.elapsed().as_micros_ceil();
                let recovery = memory.drain_reports(&self.dev)?;
                dev_info!(self.dev.as_ref(),
                    "M5_LIVE_SNAPSHOT sample={} poll={} took_us={} recovery_during_read={} state={:x?} monitor={:x?}\n",
                    live_snapshots, poll, elapsed, recovery, observed, monitor);
                live_snapshots += 1;
                if recovery { break; }
            }
            if [0, 100, 1000, 5000].contains(&poll) {
                dev_info!(self.dev.as_ref(), "M5_COMPUTE poll={} firmware/UMA/completion={:x?}\n",
                    poll, memory.compute_observations()?);
            }
            let result = memory.compute_result()?;
            if result.inputs_changed != 0 { break; }
            let (outer, inner, _) = memory.queue_probe_status()?;
            if result.outputs_wrong == 0 && outer == [2; 3] && inner == [4; 3] {
                dev_info!(self.dev.as_ref(), "M5_COMPUTE verified 1024 exact outputs, inputs unchanged, queues retired; progress={:x?}\n",
                    memory.compute_progress()?);
                return Ok(());
            }
            fsleep(Delta::from_millis(1));
        }
        for _ in 0..4 { memory.drain_reports(&self.dev)?; }
        let result = memory.compute_result()?;
        dev_err!(self.dev.as_ref(), "M5_COMPUTE final firmware/UMA/completion={:x?}\n",
            memory.compute_observations()?);
        dev_err!(self.dev.as_ref(), "M5_COMPUTE failed inputs_changed={} outputs_wrong={} untouched={} queues={:?} progress={:x?}\n",
            result.inputs_changed, result.outputs_wrong, result.outputs_untouched,
            memory.queue_probe_status()?, memory.compute_progress()?);
        Err(EIO)
    }

    fn retire_openings(&mut self) -> Result {
        let mut counters = self.memory.as_mut().ok_or(EIO)?.control_counters()?;
        dev_info!(self.dev.as_ref(), "M5_OPEN before power-transition-end {:?}\n", counters);
        // M5's own cold trace sends one 0x89 to GFX, then GFX1 (events
        // 167/171), immediately after the two initdata ACKs. No other control
        // record or work item is published in this bounded experiment.
        for index in [0, 1] {
            Pin::new(&mut self.roles[index].rtkit)
                .send_message(0x21, g17_rtkit::encode_power_transition_end())?;
        }
        for _ in 0..1000 {
            if self.memory.as_mut().ok_or(EIO)?.drain_reports(&self.dev)? {
                return Err(EIO);
            }
            if self.roles.iter().any(|role| role.state.crashed.load(Ordering::Acquire)) {
                return Err(EIO);
            }
            counters = self.memory.as_mut().ok_or(EIO)?.control_counters()?;
            if counters == [[1; 3]; 2] {
                dev_info!(self.dev.as_ref(), "M5_OPEN both native control openings retired {:?}\n", counters);
                return Ok(());
            }
            fsleep(Delta::from_millis(1));
        }
        dev_err!(self.dev.as_ref(), "M5_OPEN control openings did not retire {:?}\n", counters);
        Err(ETIMEDOUT)
    }

    fn install_queue_probe(&mut self) -> Result {
        let memory = self.memory.as_mut().ok_or(EIO)?;
        dev_info!(self.dev.as_ref(), "M5_QUEUE before tag15-only {:?}\n", memory.queue_probe_status()?);
        dev_info!(self.dev.as_ref(), "M5_QUEUE scheduler before {:x?}\n", memory.scheduler_probe_status()?);
        memory.publish_queue_probe()?;
        // Native M5 encodes CL_2 as 2 | (2 << 2), on GFX endpoint 0x21.
        Pin::new(&mut self.roles[0].rtkit).send_message(0x21, 0x0083_0000_0000_000a)?;
        for _ in 0..1000 {
            if memory.drain_reports(&self.dev)? { return Err(EIO); }
            if self.roles.iter().any(|role| role.state.crashed.load(Ordering::Acquire)) {
                dev_err!(self.dev.as_ref(), "M5_QUEUE firmware crash after {:?}\n", memory.queue_probe_status()?);
                return Err(EIO);
            }
            let (outer, inner, slot) = memory.queue_probe_status()?;
            let scheduler = memory.scheduler_probe_status()?;
            if outer == [1; 3] && inner == [1; 3]
                && M5FirmwareMemory::queue_probe_installed(slot)
                && M5FirmwareMemory::scheduler_probe_bound(scheduler) {
                dev_info!(self.dev.as_ref(), "M5_QUEUE installed qid11 outer={:?} inner={:?} slot={:x?}; no kicks\n",
                    outer, inner, slot);
                dev_info!(self.dev.as_ref(), "M5_QUEUE scheduler bound {:x?}\n", scheduler);
                return Ok(());
            }
            fsleep(Delta::from_millis(1));
        }
        dev_err!(self.dev.as_ref(), "M5_QUEUE install timeout {:?}\n", memory.queue_probe_status()?);
        dev_err!(self.dev.as_ref(), "M5_QUEUE scheduler timeout {:x?}\n", memory.scheduler_probe_status()?);
        Err(ETIMEDOUT)
    }
}

pub(crate) struct M5Runtime {
    session: Option<KBox<M5Session>>,
}

impl M5Runtime {
    pub(crate) fn start(
        pdev: &platform::Device<Core>,
        dev: ARef<AsahiDevice>,
        registers: regs::Resources,
    ) -> Result<Self> {
        let gfx = G17PMailboxProvider::get(pdev.as_ref(), 0)?;
        let gfx1 = G17PMailboxProvider::get(pdev.as_ref(), 1)?;
        for (provider, expected) in [
            (&gfx, ExpectedProvider { role: G17PAscwrapV6Role::Gfx,
                wrapper: (0x582600000, 0x88000), iop_vbar: (0x582050000, 8) }),
            (&gfx1, ExpectedProvider { role: G17PAscwrapV6Role::Gfx1,
                wrapper: (0x582e00000, 0x88000), iop_vbar: (0x582850000, 8) }),
        ] {
            validate_provider(provider, expected)?;
            provider.require_safe_cpu_lifecycle()?;
        }
        let uat = Arc::new(mmu::Uat::new_t8142(&dev)?, GFP_KERNEL)?;
        let make_role = |index, provider| -> Result<M5Role> {
            let state = Arc::new(M5RoleState { dev: dev.clone(), role: index,
                acked: AtomicBool::new(false), crashed: AtomicBool::new(false),
                events: AtomicU32::new(0) }, GFP_KERNEL)?;
            let rtkit = rtkit::RtKit::<M5RtkitOps>::new_g17p(
                dev.as_ref(), None, index, state.clone())?;
            Ok(M5Role { provider, rtkit, state, started: false })
        };
        let mut session = KBox::new(M5Session {
            roles: [make_role(0, gfx)?, make_role(1, gfx1)?],
            memory: None, uat, registers, dev,
        }, GFP_KERNEL)?;
        // From this point every path returns the retained platform owner,
        // even when firmware rejects the graph. Failed probe unwinding must
        // never release UAT or transports beneath a running firmware CPU.
        // Keep callback code resident even if a failed stop retains owners.
        // This explicit RAM-boot experiment is released by reboot only.
        unsafe { kernel::bindings::__module_get(crate::THIS_MODULE.as_ptr()); }
        let result = session.initialize(*crate::module_parameters::m5_initdata_test.value());
        dev_info!(pdev.as_ref(), "M5_INIT transaction result={:?}; requested probe mode={}\n",
            result, *crate::module_parameters::m5_initdata_test.value());
        let stopped = session.stop_cpus();
        dev_info!(pdev.as_ref(), "M5_INIT stop requests={:?}; owners retained\n", stopped);
        if *crate::module_parameters::m5_initdata_test.value() == 8
            && result.is_ok() && stopped.is_ok() {
            // Observe stop status, but do not qualify a frozen snapshot:
            // the exact M5 halt predicate remains unverified. Never unwind
            // the retained owner on a diagnostic error.
            let frozen: Result = (|| {
                let session = &mut *session;
                for (index, role) in session.roles.iter().enumerate() {
                    let (control, status) = role.provider.cpu_state()?;
                    dev_info!(pdev.as_ref(),
                        "M5_KSM stop hardware role={} control={:#x} status={:#x}\n",
                        index, control, status);
                    // Conservative status mask only. Native _isIdle tests
                    // status&3; neither predicate establishes a debug halt.
                    if control & 0x30 != 0 || status & 0x23 != 0x22 {
                        return Err(EBUSY);
                    }
                }
                Err(ENOTSUPP)
            })();
            if let Err(error) = frozen {
                dev_err!(pdev.as_ref(),
                    "M5_KSM admission snapshot rejected {:?}; quiescence unqualified, owners retained\n", error);
            }
        }
        Ok(Self { session: Some(session) })
    }
}

impl Drop for M5Runtime {
    fn drop(&mut self) {
        if let Some(mut session) = self.session.take() {
            if session.stop_cpus().is_err() {
                dev_err!(session.dev.as_ref(), "M5 stop failed; retaining the full session\n");
                core::mem::forget(session);
            }
        }
    }
}
