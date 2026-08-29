// SPDX-License-Identifier: GPL-2.0-only OR MIT

//! G17P live-boot lifecycle boundary.
//!
//! T8140 has two independent ASCWrap v6 providers. The provider owns the
//! wrapper and IOP VBAR mappings; DRM must never map an `asc1` alias or write
//! `CPU_RUN` directly. A previous stage did exactly that and then freed RTKit
//! transports while both CPUs could still be running.
//!
//! The current path keeps each hardware owner explicit:
//!
//! 1. [`crate::driver`] admits the exact live DT topology before any target
//!    effect and passes an unforgeable [`T8140LiveTopologyAdmission`] token.
//! 2. This module resolves both mailbox providers and verifies their roles and
//!    exact resources through the provider-owned lifecycle surface.
//! 3. Both providers report a matching CPU start and stop implementation.
//! 4. Both RTKit receivers are armed before CPU start. GFX completes its
//!    handshake before GFX1 starts.
//! 5. The retained runtime drops the registered queue, GFX1/GFX transports,
//!    provider CPUs, and manager resources in strict reverse order.

#![allow(dead_code)]

use core::sync::atomic::{fence, AtomicBool, AtomicU32, Ordering};

use kernel::macros::vtable;
use kernel::{
    device::Core,
    iosys_map::IoSysMapRef,
    platform,
    prelude::*,
    soc::apple::{
        mailbox::{AscwrapV6Lifecycle, AscwrapV6Provider, AscwrapV6Role},
        rtkit,
    },
    sync::Arc,
    time::{delay::fsleep, Delta},
};
#[cfg(CONFIG_DEV_COREDUMP)]
use kernel::{devcoredump, time::msecs_to_jiffies};

use crate::{
    driver::{AsahiDevice, T8140LiveTopologyAdmission},
    g17_completion, g17_lifecycle, g17_manager, g17_rtkit, g17_uapi, gem, hw, identity,
    mmu, module_parameters, regs,
};
use kernel::sync::aref::ARef;

/// Ordered boundary stages.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) enum BootStage {
    ExactTopology,
    ProviderOwnership,
    ReversibleLifecycle,
    StartCpus,
    BootRtkit,
    PublishInitdata,
    InitdataAcknowledged,
    DeviceControlPublished,
    RegisterComputeQueue,
}

const INITDATA_ACK_TYPE: u16 = 0x09;
const CONTROL_START_TYPE: u16 = 0x89;
const CONTROL_DONE_TYPE: u16 = 0x84;
const CONTROL_DONE_CHANNEL: u64 = 0x11;
const INITDATA_ACK_POLLS: usize = 200;
const CONTROL_COUNTER_POLLS: usize = 100;
const G17P_CRASHLOG_MAX_BYTES: usize = 256 * 1024;

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
#[repr(u32)]
enum G17PProcessor {
    Gfx = 0,
    Gfx1 = 1,
}

impl G17PProcessor {
    const fn name(self) -> &'static str {
        match self {
            Self::Gfx => "GFX",
            Self::Gfx1 => "GFX1",
        }
    }
}

const fn bounded_crashlog_size(size: usize) -> usize {
    if size < G17P_CRASHLOG_MAX_BYTES {
        size
    } else {
        G17P_CRASHLOG_MAX_BYTES
    }
}

const _: () = {
    assert!(bounded_crashlog_size(0) == 0);
    assert!(bounded_crashlog_size(4096) == 4096);
    assert!(bounded_crashlog_size(G17P_CRASHLOG_MAX_BYTES + 1) == G17P_CRASHLOG_MAX_BYTES);
};

/// Load-time boundary for short, independent hardware experiments.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
enum G17PTestStage {
    Control = 0,
    Qid4 = 1,
    ComputeUapi = 2,
}

impl G17PTestStage {
    fn from_parameter(value: u32) -> Result<Self> {
        match value {
            0 => Ok(Self::Control),
            1 => Ok(Self::Qid4),
            2 => Ok(Self::ComputeUapi),
            _ => Err(EINVAL),
        }
    }

    const fn configures_qid4(self) -> bool {
        !matches!(self, Self::Control)
    }

    const fn enables_compute_uapi(self) -> bool {
        matches!(self, Self::ComputeUapi)
    }

    const fn name(self) -> &'static str {
        match self {
            Self::Control => "control",
            Self::Qid4 => "QID4",
            Self::ComputeUapi => "compute UAPI",
        }
    }
}

/// Per-role callback state retained until the corresponding RTKit client is
/// dropped. The callback only publishes atomics; probe owns every mutation of
/// manager and queue state.
struct G17PRoleRtkitState {
    dev: ARef<AsahiDevice>,
    processor: G17PProcessor,
    acked: AtomicBool,
    crashed: AtomicBool,
    runtime_events: AtomicU32,
}

impl G17PRoleRtkitState {
    fn new(dev: ARef<AsahiDevice>, processor: G17PProcessor) -> Self {
        Self {
            dev,
            processor,
            acked: AtomicBool::new(false),
            crashed: AtomicBool::new(false),
            runtime_events: AtomicU32::new(0),
        }
    }
}

#[cfg(CONFIG_DEV_COREDUMP)]
fn publish_g17p_crashlog(state: &G17PRoleRtkitState, crashlog: &[u8]) -> Result {
    let captured_size = bounded_crashlog_size(crashlog.len());
    let mut crashdump = crate::crashdump::CrashDumpBuilder::new(KVVec::new())?;
    crashdump.add_rtkit_crash_info(
        state.processor as u32,
        crashlog.len(),
        captured_size,
    )?;
    crashdump.add_crashlog(&crashlog[..captured_size])?;
    let crashdump = KBox::new(crashdump.finalize()?, GFP_KERNEL)?;
    devcoredump::dev_coredump(
        state.dev.as_ref(),
        &crate::THIS_MODULE,
        crashdump,
        GFP_KERNEL,
        msecs_to_jiffies(60 * 60 * 1000),
    );
    Ok(())
}

/// Active G17P boot does not ask Rust to back system-endpoint buffers. The
/// generic RTKit core uses its coherent-DMA fallback when these callbacks are
/// absent.
struct G17PNoRtkitBuffer;

impl rtkit::Buffer for G17PNoRtkitBuffer {
    fn iova(&self) -> Result<usize> {
        Err(ENOTSUPP)
    }

    fn buf(&mut self) -> Result<IoSysMapRef<'_, u8>> {
        Err(ENOTSUPP)
    }
}

struct G17PRtkitOps;

#[vtable]
impl rtkit::Operations for G17PRtkitOps {
    type Data = Arc<G17PRoleRtkitState>;
    type Buffer = G17PNoRtkitBuffer;

    fn recv_message(
        data: <Self::Data as kernel::types::ForeignOwnable>::Borrowed<'_>,
        endpoint: u8,
        message: u64,
    ) {
        if endpoint == g17_rtkit::ENDPOINT_GFX_MESSAGES
            && (message >> 48) as u16 == INITDATA_ACK_TYPE
        {
            data.acked.store(true, Ordering::Release);
        } else if endpoint == g17_rtkit::ENDPOINT_GFX_INTERRUPTS {
            data.runtime_events.fetch_add(1, Ordering::Relaxed);
        }
    }

    fn recv_message_early(
        data: <Self::Data as kernel::types::ForeignOwnable>::Borrowed<'_>,
        endpoint: u8,
        message: u64,
    ) -> bool {
        match (endpoint, (message >> 48) as u16) {
            (g17_rtkit::ENDPOINT_GFX_MESSAGES, INITDATA_ACK_TYPE) => {
                data.acked.store(true, Ordering::Release);
                true
            }
            _ => false,
        }
    }

    fn crashed(
        data: <Self::Data as kernel::types::ForeignOwnable>::Borrowed<'_>,
        crashlog: Option<&[u8]>,
    ) {
        data.crashed.store(true, Ordering::Release);
        #[cfg(CONFIG_DEV_COREDUMP)]
        if let Some(crashlog) = crashlog {
            if let Err(error) = publish_g17p_crashlog(&data, crashlog) {
                dev_err!(
                    data.dev.as_ref(),
                    "G17P {} crashlog coredump failed ({:?})\n",
                    data.processor.name(),
                    error
                );
            }
        }
        #[cfg(not(CONFIG_DEV_COREDUMP))]
        let _ = crashlog;
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
struct ExpectedProvider {
    role: AscwrapV6Role,
    wrapper: (u64, u64),
    iop_vbar: (u64, u64),
}

const GFX_PROVIDER: ExpectedProvider = ExpectedProvider {
    role: AscwrapV6Role::Gfx,
    wrapper: (0x4826_00000, 0x88000),
    iop_vbar: (0x4820_50000, 0x8),
};

const GFX1_PROVIDER: ExpectedProvider = ExpectedProvider {
    role: AscwrapV6Role::Gfx1,
    wrapper: (0x482e_00000, 0x88000),
    iop_vbar: (0x4828_50000, 0x8),
};

fn validate_provider(
    provider: &AscwrapV6Provider,
    expected: ExpectedProvider,
) -> Result<AscwrapV6Lifecycle> {
    let lifecycle = provider.lifecycle()?;
    if lifecycle.role != expected.role
        || lifecycle.wrapper != expected.wrapper
        || lifecycle.iop_vbar != expected.iop_vbar
    {
        return Err(EINVAL);
    }

    Ok(lifecycle)
}

impl g17_lifecycle::AscCpuLifecycle for AscwrapV6Provider {
    type Error = Error;

    fn start_cpu(&self) -> Result {
        AscwrapV6Provider::start_cpu(self)
    }

    fn stop_cpu(&self) -> Result {
        AscwrapV6Provider::stop_cpu(self)
    }
}

struct G17PBootedSession {
    cpus: g17_lifecycle::StartedCpuPair<AscwrapV6Provider>,
    gfx_state: Arc<G17PRoleRtkitState>,
    gfx_rtkit: rtkit::RtKit<G17PRtkitOps>,
    gfx1_state: Arc<G17PRoleRtkitState>,
    gfx1_rtkit: rtkit::RtKit<G17PRtkitOps>,
    queue: Option<g17_manager::G17PSksmQueue>,
    compute_uapi_enabled: bool,
}

fn boot_session(
    pdev: &platform::Device,
    drm: &ARef<AsahiDevice>,
    registers: &regs::Resources,
    manager: &mut g17_manager::G17PManagerConstruction,
    test_stage: G17PTestStage,
) -> Result<G17PBootedSession> {
    let gfx = AscwrapV6Provider::get(pdev.as_ref(), 0)?;
    let gfx_lifecycle = validate_provider(&gfx, GFX_PROVIDER)?;
    let gfx1 = AscwrapV6Provider::get(pdev.as_ref(), 1)?;
    let gfx1_lifecycle = validate_provider(&gfx1, GFX1_PROVIDER)?;
    let gfx_gate = gfx.require_safe_cpu_lifecycle();
    let gfx1_gate = gfx1.require_safe_cpu_lifecycle();
    let transaction_gate =
        g17_lifecycle::admit_cpu_start(gfx_lifecycle.missing, gfx1_lifecycle.missing);
    if gfx_gate.is_err() || gfx1_gate.is_err() || transaction_gate.is_err() {
        return Err(ENOTSUPP);
    }
    dev_info!(pdev.as_ref(), "G17P session: providers ready\n");

    let gfx_state = Arc::new(
        G17PRoleRtkitState::new(drm.clone(), G17PProcessor::Gfx),
        GFP_KERNEL,
    )?;
    let gfx1_state = Arc::new(
        G17PRoleRtkitState::new(drm.clone(), G17PProcessor::Gfx1),
        GFP_KERNEL,
    )?;
    let mut gfx_rtkit =
        rtkit::RtKit::<G17PRtkitOps>::new_g17p(drm.as_ref(), None, 0, gfx_state.clone())?;
    let mut gfx1_rtkit =
        rtkit::RtKit::<G17PRtkitOps>::new_g17p(drm.as_ref(), None, 1, gfx1_state.clone())?;
    dev_info!(pdev.as_ref(), "G17P session: both RTKit receivers armed\n");

    registers.t8140_axi_transition_workaround()?;
    dev_info!(pdev.as_ref(), "G17P session: AXI transition configured\n");
    let cpus = g17_lifecycle::StartedCpuPair::start_staged(gfx, gfx1, || {
        dev_info!(pdev.as_ref(), "G17P session: GFX processor started\n");
        Pin::new(&mut gfx_rtkit).boot()?;
        dev_info!(pdev.as_ref(), "G17P session: GFX RTKit handshake complete\n");
        Ok(())
    })?;
    dev_info!(pdev.as_ref(), "G17P session: GFX1 processor started\n");
    Pin::new(&mut gfx1_rtkit).boot()?;
    dev_info!(pdev.as_ref(), "G17P session: GFX1 RTKit handshake complete\n");
    registers.t8140_pre_initdata_clear()?;
    for rtkit in [&mut gfx_rtkit, &mut gfx1_rtkit] {
        Pin::new(&mut *rtkit).start_endpoint(g17_rtkit::ENDPOINT_GFX_MESSAGES)?;
        Pin::new(&mut *rtkit).start_endpoint(g17_rtkit::ENDPOINT_GFX_INTERRUPTS)?;
    }

    let handoff = manager.handoff();
    let gfx_init =
        g17_rtkit::encode_initdata_doorbell(handoff.primary.instance.root).map_err(|_| EINVAL)?;
    let gfx1_init =
        g17_rtkit::encode_initdata_doorbell(handoff.secondary.instance.root).map_err(|_| EINVAL)?;
    Pin::new(&mut gfx_rtkit).send_message(g17_rtkit::ENDPOINT_GFX_MESSAGES, gfx_init)?;
    Pin::new(&mut gfx1_rtkit).send_message(g17_rtkit::ENDPOINT_GFX_MESSAGES, gfx1_init)?;
    dev_info!(pdev.as_ref(), "G17P session: both initdata doorbells sent\n");

    for _ in 0..INITDATA_ACK_POLLS {
        if gfx_state.acked.load(Ordering::Acquire) && gfx1_state.acked.load(Ordering::Acquire) {
            break;
        }
        if gfx_state.crashed.load(Ordering::Acquire) || gfx1_state.crashed.load(Ordering::Acquire) {
            return Err(EIO);
        }
        fsleep(Delta::from_millis(10));
    }
    if !gfx_state.acked.load(Ordering::Acquire) || !gfx1_state.acked.load(Ordering::Acquire) {
        return Err(ETIMEDOUT);
    }
    dev_info!(
        pdev.as_ref(),
        "G17P session: GFX and GFX1 initdata ACKs received\n"
    );

    let staged = manager.control_counters()?;
    if !staged.opening_staged() {
        dev_err!(
            pdev.as_ref(),
            "G17P control: staged counters are {:?}/{:?}, expected [0, 0, 4]/[0, 0, 16]\n",
            staged.primary,
            staged.secondary
        );
        return Err(EIO);
    }
    dev_info!(
        pdev.as_ref(),
        "G17P control: 4 primary and 16 secondary records staged\n"
    );

    let control_start = (CONTROL_START_TYPE as u64) << 48;
    Pin::new(&mut gfx_rtkit).send_message(g17_rtkit::ENDPOINT_GFX_INTERRUPTS, control_start)?;
    fsleep(Delta::from_millis(12));
    Pin::new(&mut gfx1_rtkit).send_message(g17_rtkit::ENDPOINT_GFX_INTERRUPTS, control_start)?;
    dev_info!(pdev.as_ref(), "G17P control: both start messages sent\n");

    let mut counters = staged;
    for _ in 0..CONTROL_COUNTER_POLLS {
        counters = manager.control_counters()?;
        if counters.opening_retired() {
            break;
        }
        if gfx_state.crashed.load(Ordering::Acquire) || gfx1_state.crashed.load(Ordering::Acquire) {
            return Err(EIO);
        }
        fsleep(Delta::from_millis(1));
    }
    if !counters.opening_retired() {
        dev_err!(
            pdev.as_ref(),
            "G17P control: counters stopped at {:?}/{:?}, expected [4, 4, 4]/[16, 16, 16]\n",
            counters.primary,
            counters.secondary
        );
        return Err(ETIMEDOUT);
    }
    dev_info!(
        pdev.as_ref(),
        "G17P control: counters retired at [4, 4, 4]/[16, 16, 16]\n"
    );

    let control_done = ((CONTROL_DONE_TYPE as u64) << 48) | CONTROL_DONE_CHANNEL;
    Pin::new(&mut gfx_rtkit).send_message(g17_rtkit::ENDPOINT_GFX_INTERRUPTS, control_done)?;
    dev_info!(pdev.as_ref(), "G17P control: primary done message sent\n");

    if !test_stage.configures_qid4() {
        dev_info!(
            pdev.as_ref(),
            "G17P session: control stage complete; QID4 remains untouched\n"
        );
        return Ok(G17PBootedSession {
            cpus,
            gfx_state,
            gfx_rtkit,
            gfx1_state,
            gfx1_rtkit,
            queue: None,
            compute_uapi_enabled: false,
        });
    }

    let queue = manager.configure_g17p_compute_queue(drm, registers)?;

    dev_info!(
        pdev.as_ref(),
        "G17P session: SKSM QID 4 / selector 2 ready; stage {}\n",
        test_stage.name()
    );
    Ok(G17PBootedSession {
        cpus,
        gfx_state,
        gfx_rtkit,
        gfx1_state,
        gfx1_rtkit,
        queue: Some(queue),
        compute_uapi_enabled: test_stage.enables_compute_uapi(),
    })
}

/// State retained for the full lifetime of an exact T8140 platform device.
/// Hardware/runtime owner used by the T8140 DRM frontend.
pub(crate) struct G17PLiveRuntime {
    pdev: Option<ARef<platform::Device>>,
    drm: Option<ARef<AsahiDevice>>,
    registers: Option<regs::Resources>,
    manager: Option<g17_manager::G17PManagerConstruction>,
    cpus: Option<g17_lifecycle::StartedCpuPair<AscwrapV6Provider>>,
    gfx_state: Option<Arc<G17PRoleRtkitState>>,
    gfx_rtkit: Option<rtkit::RtKit<G17PRtkitOps>>,
    gfx1_state: Option<Arc<G17PRoleRtkitState>>,
    gfx1_rtkit: Option<rtkit::RtKit<G17PRtkitOps>>,
    queue: Option<g17_manager::G17PSksmQueue>,
    compute_binding: Option<g17_manager::G17PComputeUserBinding>,
    completion_indices: g17_completion::QueueIndices,
    render_tracker: Option<g17_manager::G17PPartialOpeningRenderTracker>,
    test_stage: G17PTestStage,
    accepting_submissions: bool,
}

impl G17PLiveRuntime {
    pub(crate) fn is_crashed(&self) -> bool {
        self.gfx_state
            .as_ref()
            .is_some_and(|state| state.crashed.load(Ordering::Acquire))
            || self
                .gfx1_state
                .as_ref()
                .is_some_and(|state| state.crashed.load(Ordering::Acquire))
    }

    pub(crate) fn mark_firmware_cache_flush_ready(&self) -> Result {
        self.manager
            .as_ref()
            .ok_or(ENODEV)?
            .mark_firmware_cache_flush_ready();
        Ok(())
    }

    pub(crate) fn new_user_vm(
        &mut self,
        id: u64,
        kernel_range: core::ops::Range<u64>,
    ) -> Result<mmu::Vm> {
        if !self.accepting_submissions {
            return Err(ENOTSUPP);
        }
        self.manager
            .as_mut()
            .ok_or(ENODEV)?
            .new_user_vm(id, kernel_range)
    }

    pub(crate) fn map_timestamp_buffer(
        &self,
        bo: gem::ObjectRef,
        range: core::ops::Range<usize>,
    ) -> Result<mmu::KernelMapping> {
        if !self.accepting_submissions {
            return Err(ENOTSUPP);
        }
        self.manager
            .as_ref()
            .ok_or(ENODEV)?
            .map_timestamp_buffer(bo, range)
    }

    /// Build, publish, notify, and synchronously retire one translated user
    /// render through the Linux-owned G17P queue graph.
    pub(crate) fn submit_translated_render(
        &mut self,
        vm: &mmu::Vm,
        command: &g17_uapi::TranslatedRenderCommand,
        timestamps: [u64; 4],
        timestamp_aliases: KVec<mmu::KernelMapping>,
    ) -> Result {
        if !self.accepting_submissions {
            return Err(ENOTSUPP);
        }
        if self.is_crashed() {
            g17_lifecycle::recover_persistent_runtime(self)?;
        }
        let recycle = self
            .manager
            .as_ref()
            .ok_or(ENODEV)?
            .retained_render_needs_recycle(vm);
        if recycle {
            g17_lifecycle::recover_persistent_runtime(self)?;
        }
        let drm = self.drm.as_ref().ok_or(ENODEV)?.clone();
        let prepared = self
            .manager
            .as_mut()
            .ok_or(ENODEV)?
            .begin_translated_render(&drm, vm, command, timestamps, timestamp_aliases)?;
        if let Err(error) = Pin::new(self.gfx_rtkit.as_mut().ok_or(ENODEV)?)
            .send_message(g17_rtkit::ENDPOINT_GFX_INTERRUPTS, prepared.paired_doorbell)
        {
            return match g17_lifecycle::recover_persistent_runtime(self) {
                Ok(()) => Err(error),
                Err(recovery_error) => Err(recovery_error),
            };
        }
        let completion = match self
            .manager
            .as_mut()
            .ok_or(ENODEV)?
            .wait_translated_render_completion()
        {
            Ok(completion) => completion,
            Err(error) => {
                return match g17_lifecycle::recover_persistent_runtime(self) {
                    Ok(()) => Err(error),
                    Err(recovery_error) => Err(recovery_error),
                };
            }
        };
        if !completion.complete {
            return match g17_lifecycle::recover_persistent_runtime(self) {
                Ok(()) => Err(EIO),
                Err(recovery_error) => Err(recovery_error),
            };
        }
        Ok(())
    }

    /// Publish one translated compute command and wait for the start and end
    /// timestamps owned by its descriptor. The caller retains the mapping
    /// until this method returns, so firmware cannot outlive its completion
    /// words.
    pub(crate) fn submit_translated_compute(
        &mut self,
        vm: &mmu::Vm,
        command: &g17_uapi::TranslatedComputeCommand,
        owned_timestamps: mmu::KernelMapping,
    ) -> Result<[u64; 2]> {
        if !self.accepting_submissions {
            return Err(ENOTSUPP);
        }
        if self.is_crashed() || self.compute_binding.is_some() {
            g17_lifecycle::recover_persistent_runtime(self)?;
        }
        if self
            .manager
            .as_ref()
            .ok_or(ENODEV)?
            .retained_render_needs_recycle(vm)
        {
            g17_lifecycle::recover_persistent_runtime(self)?;
        }
        if owned_timestamps.size() < 16 {
            return Err(ERANGE);
        }
        let timestamp_start = owned_timestamps.iova();
        let timestamp_end = timestamp_start.checked_add(8).ok_or(EOVERFLOW)?;
        let binding = {
            let manager = self.manager.as_mut().ok_or(ENODEV)?;
            let queue = self.queue.as_mut().ok_or(ENODEV)?;
            manager.bind_compute_user_vm(queue, vm)?
        };
        self.compute_binding = Some(binding);
        let submit_result = {
            let registers = self.registers.as_ref().ok_or(ENODEV)?;
            let manager = self.manager.as_mut().ok_or(ENODEV)?;
            let queue = self.queue.as_mut().ok_or(ENODEV)?;
            let binding = self.compute_binding.as_ref().ok_or(ENODEV)?;
            manager.submit_translated_compute(
                registers,
                queue,
                binding,
                command,
                g17_manager::G17PComputeSubmissionContext {
                    context_id: 3,
                    dispatch_identity: 0x0200_026c_0300_028e,
                    execution_gate: 1,
                    timestamps: g17_uapi::ComputeTimestampAddresses {
                        start: timestamp_start,
                        end: timestamp_end,
                    },
                    descriptor_flag_4c: false,
                    descriptor_flag_5c8: false,
                    converted_command_timestamp: 1,
                    barriers: &[],
                    mcache: None,
                    rce_kind: 0,
                    auxiliary: 0x003f_ffff_ffff_ffff,
                },
            )
        };
        if let Err(error) = submit_result {
            return match g17_lifecycle::recover_persistent_runtime(self) {
                Ok(()) => Err(error),
                Err(recovery_error) => Err(recovery_error),
            };
        }

        let completion = owned_timestamps.with_cpu_bytes(|raw| {
            const POLLS: usize = 2000;
            for _ in 0..POLLS {
                fence(Ordering::Acquire);
                let start = u64::from_le_bytes(raw[0..8].try_into().map_err(|_| ERANGE)?);
                let end = u64::from_le_bytes(raw[8..16].try_into().map_err(|_| ERANGE)?);
                if let Some(completion) = g17_uapi::completed_compute_timestamps(start, end) {
                    return Ok(completion);
                }
                fsleep(Delta::from_millis(1));
            }
            Err(ETIMEDOUT)
        });
        let completion = match completion {
            Ok(completion) => completion,
            Err(error) => {
                return match g17_lifecycle::recover_persistent_runtime(self) {
                    Ok(()) => Err(error),
                    Err(recovery_error) => Err(recovery_error),
                };
            }
        };
        let retire_result = self
            .manager
            .as_ref()
            .ok_or(ENODEV)?
            .complete_compute_entries(self.queue.as_mut().ok_or(ENODEV)?, 1);
        if let Err(error) = retire_result {
            return match g17_lifecycle::recover_persistent_runtime(self) {
                Ok(()) => Err(error),
                Err(recovery_error) => Err(recovery_error),
            };
        }
        drop(self.compute_binding.take());
        Ok(completion)
    }

    /// Build and retain the exact T8140 dual-role runtime.
    pub(crate) fn start(
        pdev: &platform::Device<Core>,
        drm: ARef<AsahiDevice>,
        registers: regs::Resources,
        soc: &'static hw::agx3::SocConfig,
        id: &identity::GpuIdentity,
        _topology: T8140LiveTopologyAdmission,
    ) -> Result<Self> {
        if soc.chip_id != 0x8140
            || id.gpu_gen != hw::GpuGen::G17
            || id.gpu_variant != identity::GpuVariant::P
        {
            return Err(EINVAL);
        }
        let test_stage =
            G17PTestStage::from_parameter(*module_parameters::g17p_test_stage.value())?;
        let num_clusters = registers.get_gpu_id()?.num_clusters;

        dev_info!(
            pdev.as_ref(),
            "G17P live-boot: {:?} admitted before effects; selected {} stage\n",
            BootStage::ExactTopology,
            test_stage.name()
        );

        let pdev_ref: ARef<platform::Device> = pdev.into();
        let mut manager = match g17_manager::G17PManagerConstruction::new(
            &drm,
            soc,
            g17_manager::T8140_G17P_PLATFORM_CONFIG,
            num_clusters,
        ) {
            Ok(manager) => manager,
            Err(error) => {
                dev_err!(
                    pdev.as_ref(),
                    "G17P live-boot: resource construction failed ({:?})\n",
                    error
                );
                return Err(error);
            }
        };
        let session = boot_session(&pdev_ref, &drm, &registers, &mut manager, test_stage)?;
        let accepting_submissions = session.compute_uapi_enabled && session.queue.is_some();

        Ok(Self {
            pdev: Some(pdev_ref),
            drm: Some(drm),
            registers: Some(registers),
            manager: Some(manager),
            cpus: Some(session.cpus),
            gfx_state: Some(session.gfx_state),
            gfx_rtkit: Some(session.gfx_rtkit),
            gfx1_state: Some(session.gfx1_state),
            gfx1_rtkit: Some(session.gfx1_rtkit),
            queue: session.queue,
            compute_binding: None,
            completion_indices: g17_completion::QueueIndices {
                done: 0,
                read: 0,
                write: 0,
            },
            render_tracker: None,
            test_stage,
            accepting_submissions,
        })
    }

    /// Publish first graphics in the measured 3D/TA order and send its one
    /// pair-two/channel-8 work notification through the retained primary
    /// RTKit endpoint.
    pub(crate) fn submit_partial_opening_render<F>(
        &mut self,
        submission: g17_manager::G17PPartialOpeningRenderSubmission,
        publish_late_tiling_state: F,
    ) -> Result
    where
        F: FnOnce(),
    {
        if self.render_tracker.is_some() {
            return Err(EBUSY);
        }
        let manager = self.manager.as_mut().ok_or(ENODEV)?;
        let gfx_rtkit = self.gfx_rtkit.as_mut().ok_or(ENODEV)?;
        let (tracker, prepared) =
            manager.submit_partial_opening_render(submission, publish_late_tiling_state)?;
        Pin::new(gfx_rtkit)
            .send_message(g17_rtkit::ENDPOINT_GFX_INTERRUPTS, prepared.paired_doorbell)?;
        self.render_tracker = Some(tracker);
        Ok(())
    }

    /// Consume queue progress together with the scheduler job-list state and
    /// the owned start/end timestamps required for execution completion.
    pub(crate) fn observe_partial_opening_render_completion(
        &mut self,
        tiling_current: g17_completion::QueueIndices,
        fragment_current: g17_completion::QueueIndices,
        execution: g17_completion::RenderExecutionState,
    ) -> Result<g17_completion::PairedRenderCompletion> {
        let manager = self.manager.as_ref().ok_or(ENODEV)?;
        let tracker = self.render_tracker.as_mut().ok_or(ENODEV)?;
        let completion = manager.observe_partial_opening_render_completion(
            tracker,
            tiling_current,
            fragment_current,
            execution,
        )?;
        if completion.complete {
            self.render_tracker = None;
        }
        Ok(completion)
    }

    /// Apply one selector-2 completion report to the retained compute queue.
    ///
    /// The IRQ/ring reader supplies the current cumulative queue counters and
    /// the prefix published to firmware. This boundary decodes the exact
    /// compute completion shape, rejects another QID, and subtracts only the
    /// newly completed count from the retained producer state.
    pub(crate) fn observe_compute_completion(
        &mut self,
        raw: &[u8],
        current: g17_completion::QueueIndices,
        published_prefix: u32,
    ) -> Result<g17_completion::QueuePrefixState> {
        let manager = self.manager.as_ref().ok_or(ENODEV)?;
        let queue = self.queue.as_mut().ok_or(ENODEV)?;
        let report = g17_completion::observe_compute_completion_report(
            raw,
            self.completion_indices,
            current,
            published_prefix,
        )
        .map_err(|_| EINVAL)?;
        if report.entry.queue_id != manager.compute_queue_id(queue)? {
            return Err(EINVAL);
        }

        let completed = current
            .done
            .checked_sub(self.completion_indices.done)
            .ok_or(EINVAL)?;
        manager.complete_compute_entries(queue, completed)?;
        self.completion_indices = current;
        Ok(report.queue)
    }
}

impl g17_lifecycle::PersistentRuntimeRecovery for G17PLiveRuntime {
    type Error = Error;

    fn pause_submissions(&mut self) {
        self.accepting_submissions = false;
    }

    fn stop_cpu_pair(&mut self) -> Result {
        match self.cpus.as_mut() {
            Some(cpus) => cpus.stop(),
            None => Ok(()),
        }
    }

    fn drop_gfx1_rtkit(&mut self) {
        drop(self.gfx1_rtkit.take());
        drop(self.gfx1_state.take());
    }

    fn drop_gfx_rtkit(&mut self) {
        drop(self.gfx_rtkit.take());
        drop(self.gfx_state.take());
    }

    fn disable_sksm_queue(&mut self) -> Result {
        if let Some(queue) = self.queue.as_mut() {
            self.manager
                .as_ref()
                .ok_or(ENODEV)?
                .unregister_compute_sksm_queue(self.registers.as_ref().ok_or(ENODEV)?, queue)?;
        }
        Ok(())
    }

    fn release_runtime_objects(&mut self) {
        drop(self.compute_binding.take());
        drop(self.queue.take());
        if let Some(manager) = self.manager.as_mut() {
            manager.release_runtime_objects_after_processor_stop();
        }
        self.render_tracker = None;
        self.completion_indices = g17_completion::QueueIndices {
            done: 0,
            read: 0,
            write: 0,
        };
    }

    fn rebuild_session_in_place(&mut self) -> Result {
        drop(self.cpus.take());
        let pdev = self.pdev.as_ref().ok_or(ENODEV)?.clone();
        let drm = self.drm.as_ref().ok_or(ENODEV)?.clone();
        let registers = self.registers.as_ref().ok_or(ENODEV)?;
        let manager = self.manager.as_mut().ok_or(ENODEV)?;
        manager.reset_session_after_processor_stop()?;
        let session = boot_session(&pdev, &drm, registers, manager, self.test_stage)?;
        self.cpus = Some(session.cpus);
        self.gfx_state = Some(session.gfx_state);
        self.gfx_rtkit = Some(session.gfx_rtkit);
        self.gfx1_state = Some(session.gfx1_state);
        self.gfx1_rtkit = Some(session.gfx1_rtkit);
        self.queue = session.queue;
        Ok(())
    }

    fn resume_submissions(&mut self) {
        self.accepting_submissions =
            self.test_stage.enables_compute_uapi() && self.queue.is_some();
    }
}

impl g17_lifecycle::PersistentRuntimeTeardown for G17PLiveRuntime {
    fn unregister_queue(&mut self) {
        if let (Some(manager), Some(registers), Some(queue)) = (
            self.manager.as_ref(),
            self.registers.as_ref(),
            self.queue.as_mut(),
        ) {
            let _ = manager.unregister_compute_sksm_queue(registers, queue);
        }
    }

    fn drop_queue(&mut self) {
        drop(self.compute_binding.take());
        drop(self.queue.take());
    }

    fn drop_gfx1_rtkit(&mut self) {
        drop(self.gfx1_rtkit.take());
        drop(self.gfx1_state.take());
    }

    fn drop_gfx_rtkit(&mut self) {
        drop(self.gfx_rtkit.take());
        drop(self.gfx_state.take());
    }

    fn drop_cpu_pair(&mut self) {
        drop(self.cpus.take());
    }

    fn drop_manager(&mut self) {
        drop(self.manager.take());
    }
}

impl Drop for G17PLiveRuntime {
    fn drop(&mut self) {
        g17_lifecycle::teardown_persistent_runtime(self);
        drop(self.drm.take());
        drop(self.registers.take());
        drop(self.pdev.take());
    }
}
