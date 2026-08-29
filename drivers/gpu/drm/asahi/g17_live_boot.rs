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
use kernel::{devcoredump, new_mutex, sync::Mutex, time::msecs_to_jiffies};

use crate::{
    driver::{AsahiDevice, T8140LiveTopologyAdmission},
    g17_completion, g17_initdata, g17_lifecycle, g17_manager, g17_rtkit, g17_submission,
    g17_uapi, gem, hw, identity, mmu, module_parameters, regs,
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
const G17P_RTKIT_UAT_DVA_MASK: u64 = (1u64 << 40) - 1;

const fn g17p_rtkit_uat_address(dva: u64) -> u64 {
    dva & G17P_RTKIT_UAT_DVA_MASK
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
#[repr(u32)]
enum G17PProcessor {
    Gfx = 0,
    Gfx1 = 1,
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
struct G17PFirmwareCrashBuffer {
    dva: u64,
    size: usize,
}

impl G17PProcessor {
    const fn name(self) -> &'static str {
        match self {
            Self::Gfx => "GFX",
            Self::Gfx1 => "GFX1",
        }
    }

    const fn firmware_crash_buffer(self) -> G17PFirmwareCrashBuffer {
        // Both endpoint-1 buffer requests are stable across the retained
        // G17P captures. Message 0x0010810001e1c000 decodes to the primary
        // DVA below and size 0x8000; GFX1 advertises the second DVA with the
        // same size.
        match self {
            Self::Gfx => G17PFirmwareCrashBuffer {
                dva: 0x100_01e1_c000,
                size: 0x8000,
            },
            Self::Gfx1 => G17PFirmwareCrashBuffer {
                dva: 0x100_01f5_0000,
                size: 0x8000,
            },
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
    let gfx = G17PProcessor::Gfx.firmware_crash_buffer();
    let gfx1 = G17PProcessor::Gfx1.firmware_crash_buffer();
    assert!(gfx.dva == 0x100_01e1_c000 && gfx1.dva == 0x100_01f5_0000);
    assert!(gfx.dva == crate::g17_adt_j700::J700_GFX_DATA.base + 0xa_0000);
    assert!(gfx1.dva == crate::g17_adt_j700::J700_GFX1_DATA.base + 0xa_0000);
    assert!(g17p_rtkit_uat_address(gfx.dva) == 0x01e1_c000);
    assert!(g17p_rtkit_uat_address(gfx1.dva) == 0x01f5_0000);
    assert!(gfx.size == 0x8000 && gfx1.size == 0x8000);
    assert!(gfx.dva & (mmu::UAT_PGSZ as u64 - 1) == 0);
    assert!(gfx1.dva & (mmu::UAT_PGSZ as u64 - 1) == 0);
    assert!(gfx.dva + gfx.size as u64 <= 1u64 << 42);
    assert!(gfx1.dva + gfx1.size as u64 <= 1u64 << 42);
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
    firmware_contexts: mmu::FirmwareContextTableReader,
    #[cfg(CONFIG_DEV_COREDUMP)]
    crash_capture: Arc<Mutex<G17PCrashCaptureState>>,
    acked: AtomicBool,
    crashed: AtomicBool,
    runtime_events: AtomicU32,
}

impl G17PRoleRtkitState {
    fn new(
        dev: ARef<AsahiDevice>,
        processor: G17PProcessor,
        firmware_contexts: mmu::FirmwareContextTableReader,
        #[cfg(CONFIG_DEV_COREDUMP)] crash_capture: Arc<Mutex<G17PCrashCaptureState>>,
    ) -> Self {
        Self {
            dev,
            processor,
            firmware_contexts,
            #[cfg(CONFIG_DEV_COREDUMP)]
            crash_capture,
            acked: AtomicBool::new(false),
            crashed: AtomicBool::new(false),
            runtime_events: AtomicU32::new(0),
        }
    }
}

#[cfg(CONFIG_DEV_COREDUMP)]
struct G17PCrashRecord {
    processor: G17PProcessor,
    original_size: usize,
    crashlog: KVVec<u8>,
}

#[cfg(CONFIG_DEV_COREDUMP)]
struct G17PCrashCaptureState {
    records: [Option<G17PCrashRecord>; 2],
}

#[cfg(CONFIG_DEV_COREDUMP)]
fn new_g17p_crash_capture() -> Result<Arc<Mutex<G17PCrashCaptureState>>> {
    Arc::pin_init(
        new_mutex!(
            G17PCrashCaptureState {
                records: [None, None],
            },
            "G17PCrashCaptureState"
        ),
        GFP_KERNEL,
    )
}

#[cfg(CONFIG_DEV_COREDUMP)]
fn crash_qword(raw: &[u8], index: usize) -> u64 {
    let offset = index * 8;
    u64::from_le_bytes([
        raw[offset],
        raw[offset + 1],
        raw[offset + 2],
        raw[offset + 3],
        raw[offset + 4],
        raw[offset + 5],
        raw[offset + 6],
        raw[offset + 7],
    ])
}

#[cfg(CONFIG_DEV_COREDUMP)]
fn log_crash_buffer_observation(
    state: &G17PRoleRtkitState,
    source: &str,
    raw: &[u8],
    sentinel: u8,
) {
    let sentinel_qword = u64::from_le_bytes([sentinel; 8]);
    let mut first_changed = None;
    let mut first_nonzero = None;
    for index in 0..raw.len() / 8 {
        let value = crash_qword(raw, index);
        if first_changed.is_none() && value != sentinel_qword {
            first_changed = Some((index * 8, value));
        }
        if first_nonzero.is_none() && value != 0 {
            first_nonzero = Some((index * 8, value));
        }
        if first_changed.is_some() && first_nonzero.is_some() {
            break;
        }
    }
    let changed = first_changed.unwrap_or((raw.len(), sentinel_qword));
    let nonzero = first_nonzero.unwrap_or((raw.len(), 0));
    dev_info!(
        state.dev.as_ref(),
        "G17P {} crashlog {} sentinel {:#x}: changed={} off={:#x} value={:#x}; nonzero={} off={:#x} value={:#x}\n",
        state.processor.name(),
        source,
        sentinel_qword,
        first_changed.is_some(),
        changed.0,
        changed.1,
        first_nonzero.is_some(),
        nonzero.0,
        nonzero.1
    );
}

#[cfg(CONFIG_DEV_COREDUMP)]
fn publish_g17p_crashlogs(
    dev: &kernel::device::Device,
    records: &[G17PCrashRecord; 2],
) -> Result {
    let mut crashdump = crate::crashdump::CrashDumpBuilder::new(KVVec::new())?;
    for record in records {
        crashdump.add_rtkit_crash_info(
            record.processor as u32,
            record.processor.firmware_crash_buffer().dva,
            record.original_size,
            record.crashlog.len(),
        )?;
        crashdump.add_crashlog(&record.crashlog)?;
    }
    let crashdump = KBox::new(crashdump.finalize()?, GFP_KERNEL)?;
    devcoredump::dev_coredump(
        dev,
        &crate::THIS_MODULE,
        crashdump,
        GFP_KERNEL,
        msecs_to_jiffies(60 * 60 * 1000),
    );
    Ok(())
}

#[cfg(CONFIG_DEV_COREDUMP)]
fn retain_g17p_crashlog(
    state: &G17PRoleRtkitState,
    original_size: usize,
    crashlog: KVVec<u8>,
) -> Result {
    let mut capture = state.crash_capture.lock();
    capture.records[state.processor as usize] = Some(G17PCrashRecord {
        processor: state.processor,
        original_size,
        crashlog,
    });
    if capture.records[0].is_none() || capture.records[1].is_none() {
        dev_info!(
            state.dev.as_ref(),
            "G17P {} crashlog retained; waiting for the other processor\n",
            state.processor.name()
        );
        return Ok(());
    }
    let records = [
        capture.records[0].take().ok_or(EIO)?,
        capture.records[1].take().ok_or(EIO)?,
    ];
    core::mem::drop(capture);
    publish_g17p_crashlogs(state.dev.as_ref(), &records)
}

#[cfg(CONFIG_DEV_COREDUMP)]
fn retain_g17p_crashlog_slice(
    state: &G17PRoleRtkitState,
    original_size: usize,
    crashlog: &[u8],
) -> Result {
    let mut retained = KVVec::new();
    retained.extend_from_slice(crashlog, GFP_KERNEL)?;
    retain_g17p_crashlog(state, original_size, retained)
}

#[cfg(CONFIG_DEV_COREDUMP)]
fn capture_g17p_firmware_crash_dva(state: &G17PRoleRtkitState) -> Result {
    let buffer = state.processor.firmware_crash_buffer();
    let captured_size = bounded_crashlog_size(buffer.size);
    let physical = crate::pgtable::UatPageTable::copy_firmware_physical_range(
        buffer.dva,
        captured_size,
    )?;
    log_crash_buffer_observation(state, "firmware-physical", &physical, 0);
    dev_info!(
        state.dev.as_ref(),
        "G17P {} crashlog copied from firmware data PA {:#x} ({} bytes)\n",
        state.processor.name(),
        buffer.dva,
        physical.len()
    );
    retain_g17p_crashlog(state, buffer.size, physical)
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
        let firmware_owned = crashlog.is_none();
        #[cfg(CONFIG_DEV_COREDUMP)]
        let capture = match crashlog {
            Some(crashlog) => {
                let captured_size = bounded_crashlog_size(crashlog.len());
                retain_g17p_crashlog_slice(
                    &data,
                    crashlog.len(),
                    &crashlog[..captured_size],
                )
            }
            None => capture_g17p_firmware_crash_dva(&data),
        };
        #[cfg(CONFIG_DEV_COREDUMP)]
        if let Err(error) = capture {
            if firmware_owned {
                let buffer = data.processor.firmware_crash_buffer();
                let live_root = data
                    .firmware_contexts
                    .context0_root()
                    .map(|root| root.ttb())
                    .unwrap_or(0);
                dev_err!(
                    data.dev.as_ref(),
                    "G17P {} crashlog copy from DVA {:#x} via live context-0 root {:#x} VA {:#x} size {:#x} failed ({:?})\n",
                    data.processor.name(),
                    buffer.dva,
                    live_root,
                    g17p_rtkit_uat_address(buffer.dva),
                    buffer.size,
                    error
                );
            } else {
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

fn wait_initdata_ack(state: &G17PRoleRtkitState) -> Result {
    for _ in 0..INITDATA_ACK_POLLS {
        if state.acked.load(Ordering::Acquire) {
            return Ok(());
        }
        if state.crashed.load(Ordering::Acquire) {
            return Err(EIO);
        }
        fsleep(Delta::from_millis(10));
    }
    Err(ETIMEDOUT)
}

#[allow(clippy::too_many_arguments)]
fn finish_boot_session(
    pdev: &platform::Device,
    drm: &ARef<AsahiDevice>,
    registers: &regs::Resources,
    manager: &mut g17_manager::G17PManagerConstruction,
    test_stage: G17PTestStage,
    cpus: g17_lifecycle::StartedCpuPair<AscwrapV6Provider>,
    gfx_state: Arc<G17PRoleRtkitState>,
    mut gfx_rtkit: rtkit::RtKit<G17PRtkitOps>,
    gfx1_state: Arc<G17PRoleRtkitState>,
    gfx1_rtkit: rtkit::RtKit<G17PRtkitOps>,
) -> Result<G17PBootedSession> {
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

    if test_stage.configures_qid4() {
        // Native keeps the completed render graph reachable through both
        // active root slots while firmware consumes the opening 0x20 record.
        // Producers remain zero until control-done below.
        manager.stage_compute_bootstrap_render(drm)?;
        dev_info!(
            pdev.as_ref(),
            "G17P roots: bootstrap render graph bound to both slots before control start\n"
        );
    }

    let control_start = (CONTROL_START_TYPE as u64) << 48;
    Pin::new(&mut gfx_rtkit).send_message(g17_rtkit::ENDPOINT_GFX_INTERRUPTS, control_start)?;
    fsleep(Delta::from_millis(12));
    let mut gfx1_rtkit = gfx1_rtkit;
    Pin::new(&mut gfx1_rtkit).send_message(g17_rtkit::ENDPOINT_GFX_INTERRUPTS, control_start)?;
    dev_info!(pdev.as_ref(), "G17P control: both start messages sent\n");

    let mut counters = staged;
    for _ in 0..CONTROL_COUNTER_POLLS {
        counters = manager.control_counters()?;
        if counters.opening_retired() {
            break;
        }
        if gfx_state.crashed.load(Ordering::Acquire)
            || gfx1_state.crashed.load(Ordering::Acquire)
        {
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
    let opening_effect = manager.control_opening_effect()?;
    dev_info!(
        pdev.as_ref(),
        "G17P control: opening effect cursor={:#x} inner={:#x} operand-slot={:#x}\n",
        opening_effect.shared_cursor,
        opening_effect.inner_state,
        opening_effect.operand_slot
    );

    let control_done = ((CONTROL_DONE_TYPE as u64) << 48) | CONTROL_DONE_CHANNEL;
    if !test_stage.configures_qid4() {
        Pin::new(&mut gfx_rtkit).send_message(
            g17_rtkit::ENDPOINT_GFX_INTERRUPTS,
            control_done,
        )?;
        dev_info!(pdev.as_ref(), "G17P control: primary done message sent\n");
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

    manager
        .prepare_partial_opening_control_done()
        .inspect_err(|error| {
            dev_err!(
                pdev.as_ref(),
                "G17P compute control: pre-doorbell status failed ({:?})\n",
                error
            );
        })?;
    Pin::new(&mut gfx_rtkit)
        .send_message(g17_rtkit::ENDPOINT_GFX_INTERRUPTS, control_done)?;
    dev_info!(
        pdev.as_ref(),
        "G17P compute control: primary done sent before startup work publication\n"
    );

    manager.split_compute_bootstrap_render_contexts()?;
    dev_info!(
        pdev.as_ref(),
        "G17P roots: opening retired; split to context0/render before work\n"
    );

    // Restore and publish 3D first, then restore and publish TA. End with the
    // barrier required before the one paired channel-8 bell.
    let bootstrap = manager.publish_staged_compute_bootstrap_render()?;
    dev_info!(
        pdev.as_ref(),
        "G17P compute control: startup TA/3D group staged after primary done\n"
    );
    Pin::new(&mut gfx_rtkit).send_message(
        g17_rtkit::ENDPOINT_GFX_INTERRUPTS,
        bootstrap.paired_doorbell,
    )?;
    dev_info!(
        pdev.as_ref(),
        "G17P compute control: channel-8 startup work sent\n"
    );
    if let Err(error) = manager.wait_compute_bootstrap_render_retirement() {
        let (fault_info, fault_address_word) = registers.g17p_fault_registers();
        let fault_valid = fault_info & 1 != 0;
        let fault_reason = (fault_info >> 1) & 0x7;
        let fault_read = fault_info & (1 << 4) != 0;
        let fault_level = (fault_info >> 7) & 0x3;
        let fault_vm = (fault_info >> 17) & 0x3f;
        let fault_unit = (fault_info >> 9) & 0xff;
        let fault_address = fault_address_word << 6;
        dev_err!(
            pdev.as_ref(),
            "G17P startup fault: valid={} reason={} read={} level={} vm={} unit={:#x} address={:#x} raw_info={:#x} raw_addr={:#x}\n",
            fault_valid,
            fault_reason,
            fault_read,
            fault_level,
            fault_vm,
            fault_unit,
            fault_address,
            fault_info,
            fault_address_word
        );
        return Err(error);
    }
    if gfx_state.crashed.load(Ordering::Acquire) || gfx1_state.crashed.load(Ordering::Acquire) {
        return Err(EIO);
    }
    Pin::new(&mut gfx_rtkit)
        .send_message(g17_rtkit::ENDPOINT_GFX_INTERRUPTS, control_done)?;
    dev_info!(
        pdev.as_ref(),
        "G17P compute control: startup TA/3D retired at 3/3/3\n"
    );

    manager.prepare_compute_runtime_class1()?;
    dev_info!(
        pdev.as_ref(),
        "G17P compute control: class-1 closure ready; publishing live tail\n"
    );
    for index in 0..g17_initdata::COMPUTE_RUNTIME_CONTROL_RECORDS {
        if index == g17_initdata::COMPUTE_RUNTIME_CLASS2_RECORD_INDEX {
            manager.prepare_compute_runtime_class2()?;
            dev_info!(
                pdev.as_ref(),
                "G17P compute control: class-2 closure ready at sequence 56\n"
            );
        }
        let target = manager.publish_compute_runtime_control_record(index)?;
        let mut counters = manager.control_counters()?;
        let mut announcements = 0usize;
        for _ in 0..CONTROL_COUNTER_POLLS {
            if counters.primary == [target; 3]
                && counters.secondary
                    == [g17_initdata::CONTROL_OPENING_SECONDARY_PRODUCER; 3]
            {
                break;
            }
            if gfx_state.crashed.load(Ordering::Acquire)
                || gfx1_state.crashed.load(Ordering::Acquire)
            {
                return Err(EIO);
            }

            // The native live-control path re-announces 0x84/0x11 while the
            // producer is pending. One notification at this transition can
            // leave the state at [consumer, consumer, producer].
            Pin::new(&mut gfx_rtkit)
                .send_message(g17_rtkit::ENDPOINT_GFX_INTERRUPTS, control_done)?;
            announcements += 1;
            counters = manager.control_counters()?;
            if counters.primary == [target; 3]
                && counters.secondary
                    == [g17_initdata::CONTROL_OPENING_SECONDARY_PRODUCER; 3]
            {
                break;
            }
            fsleep(Delta::from_millis(1));
            counters = manager.control_counters()?;
        }
        if counters.primary != [target; 3]
            || counters.secondary
                != [g17_initdata::CONTROL_OPENING_SECONDARY_PRODUCER; 3]
        {
            dev_err!(
                pdev.as_ref(),
                "G17P compute control: record {} stopped at {:?}/{:?} after {} announcements, expected [{}, {}, {}]/[16, 16, 16]\n",
                index,
                counters.primary,
                counters.secondary,
                announcements,
                target,
                target,
                target
            );
            return Err(ETIMEDOUT);
        }
        if index == 0 {
            dev_info!(
                pdev.as_ref(),
                "G17P compute control: tick 0 retired after {} announcements\n",
                announcements
            );
        }
        if index == 53 {
            dev_info!(
                pdev.as_ref(),
                "G17P compute control: ticks 0..53 retired at 58\n"
            );
        } else if index == 56 {
            dev_info!(
                pdev.as_ref(),
                "G17P compute control: class-1 sequence 54 retired at 61\n"
            );
        }
    }
    let runtime_counters = manager.control_counters()?;
    if !runtime_counters.compute_runtime_ready() {
        return Err(EIO);
    }
    manager.rewrite_compute_dispatch_record()?;
    dev_info!(
        pdev.as_ref(),
        "G17P compute control: live tail retired at [69, 69, 69]/[16, 16, 16]\n"
    );

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

    let firmware_contexts = manager.uat().firmware_context_table_reader();
    #[cfg(CONFIG_DEV_COREDUMP)]
    let crash_capture = new_g17p_crash_capture()?;
    let gfx_state = Arc::new(
        G17PRoleRtkitState::new(
            drm.clone(),
            G17PProcessor::Gfx,
            firmware_contexts.clone(),
            #[cfg(CONFIG_DEV_COREDUMP)]
            crash_capture.clone(),
        ),
        GFP_KERNEL,
    )?;
    let gfx1_state = Arc::new(
        G17PRoleRtkitState::new(
            drm.clone(),
            G17PProcessor::Gfx1,
            firmware_contexts,
            #[cfg(CONFIG_DEV_COREDUMP)]
            crash_capture,
        ),
        GFP_KERNEL,
    )?;
    let mut gfx_rtkit =
        rtkit::RtKit::<G17PRtkitOps>::new_g17p(drm.as_ref(), None, 0, gfx_state.clone())?;
    let mut gfx1_rtkit =
        rtkit::RtKit::<G17PRtkitOps>::new_g17p(drm.as_ref(), None, 1, gfx1_state.clone())?;
    dev_info!(pdev.as_ref(), "G17P session: both RTKit receivers armed\n");

    registers.t8140_axi_transition_workaround()?;
    dev_info!(pdev.as_ref(), "G17P session: AXI transition configured\n");
    let handoff = manager.handoff();
    let gfx_init =
        g17_rtkit::encode_initdata_doorbell(handoff.primary.instance.root).map_err(|_| EINVAL)?;
    let gfx1_init =
        g17_rtkit::encode_initdata_doorbell(handoff.secondary.instance.root).map_err(|_| EINVAL)?;
    let cpus = g17_lifecycle::StartedCpuPair::start_staged(gfx, gfx1, || {
        dev_info!(pdev.as_ref(), "G17P session: GFX processor started\n");
        Pin::new(&mut gfx_rtkit).boot()?;
        dev_info!(pdev.as_ref(), "G17P session: GFX RTKit handshake complete\n");
        let root_snapshot = manager.uat().prepare_t8140_firmware_high_roots()?;
        manager
            .uat()
            .confirm_t8140_primary_high_root(root_snapshot)?;
        registers.t8140_pre_initdata_clear()?;
        Pin::new(&mut gfx_rtkit).start_endpoint(g17_rtkit::ENDPOINT_GFX_MESSAGES)?;
        Pin::new(&mut gfx_rtkit).start_endpoint(g17_rtkit::ENDPOINT_GFX_INTERRUPTS)?;
        Pin::new(&mut gfx_rtkit).send_message(g17_rtkit::ENDPOINT_GFX_MESSAGES, gfx_init)?;
        dev_info!(pdev.as_ref(), "G17P session: primary initdata sent\n");
        wait_initdata_ack(&gfx_state)?;
        dev_info!(pdev.as_ref(), "G17P session: primary initdata ACK received\n");
        Ok(())
    })?;
    dev_info!(pdev.as_ref(), "G17P session: GFX1 processor started\n");
    Pin::new(&mut gfx1_rtkit).boot()?;
    dev_info!(pdev.as_ref(), "G17P session: GFX1 RTKit handshake complete\n");
    manager.uat().mirror_t8140_secondary_high_root()?;
    Pin::new(&mut gfx1_rtkit).start_endpoint(g17_rtkit::ENDPOINT_GFX_MESSAGES)?;
    Pin::new(&mut gfx1_rtkit).start_endpoint(g17_rtkit::ENDPOINT_GFX_INTERRUPTS)?;
    Pin::new(&mut gfx1_rtkit).send_message(g17_rtkit::ENDPOINT_GFX_MESSAGES, gfx1_init)?;
    dev_info!(pdev.as_ref(), "G17P session: secondary initdata sent\n");
    wait_initdata_ack(&gfx1_state)?;
    dev_info!(pdev.as_ref(), "G17P session: secondary initdata ACK received\n");

    finish_boot_session(
        pdev,
        drm,
        registers,
        manager,
        test_stage,
        cpus,
        gfx_state,
        gfx_rtkit,
        gfx1_state,
        gfx1_rtkit,
    )
}

fn boot_initial_session(
    pdev: &platform::Device,
    drm: &ARef<AsahiDevice>,
    registers: &regs::Resources,
    soc: &'static hw::agx3::SocConfig,
    num_clusters: u32,
    test_stage: G17PTestStage,
) -> Result<(g17_manager::G17PManagerConstruction, G17PBootedSession)> {
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

    let mut uat = Some(KBox::new(mmu::Uat::new_t8140(drm, true)?, GFP_KERNEL)?);
    let firmware_contexts = uat
        .as_ref()
        .ok_or(EIO)?
        .firmware_context_table_reader();
    #[cfg(CONFIG_DEV_COREDUMP)]
    let crash_capture = new_g17p_crash_capture()?;
    let gfx_state = Arc::new(
        G17PRoleRtkitState::new(
            drm.clone(),
            G17PProcessor::Gfx,
            firmware_contexts.clone(),
            #[cfg(CONFIG_DEV_COREDUMP)]
            crash_capture.clone(),
        ),
        GFP_KERNEL,
    )?;
    let gfx1_state = Arc::new(
        G17PRoleRtkitState::new(
            drm.clone(),
            G17PProcessor::Gfx1,
            firmware_contexts,
            #[cfg(CONFIG_DEV_COREDUMP)]
            crash_capture,
        ),
        GFP_KERNEL,
    )?;
    let mut gfx_rtkit =
        rtkit::RtKit::<G17PRtkitOps>::new_g17p(drm.as_ref(), None, 0, gfx_state.clone())?;
    let mut gfx1_rtkit =
        rtkit::RtKit::<G17PRtkitOps>::new_g17p(drm.as_ref(), None, 1, gfx1_state.clone())?;
    dev_info!(pdev.as_ref(), "G17P session: both RTKit receivers armed\n");

    registers.t8140_axi_transition_workaround()?;
    dev_info!(pdev.as_ref(), "G17P session: AXI transition configured\n");
    let mut manager = None;
    let cpus = g17_lifecycle::StartedCpuPair::start_staged(gfx, gfx1, || {
        dev_info!(pdev.as_ref(), "G17P session: GFX processor started\n");
        Pin::new(&mut gfx_rtkit).boot()?;
        dev_info!(pdev.as_ref(), "G17P session: GFX RTKit handshake complete\n");
        let root_snapshot = uat
            .as_ref()
            .ok_or(EIO)?
            .prepare_t8140_firmware_high_roots()?;

        let built = g17_manager::G17PManagerConstruction::new_with_uat(
            drm,
            soc,
            g17_manager::T8140_G17P_PLATFORM_CONFIG,
            num_clusters,
            uat.take().ok_or(EIO)?,
        )?;
        built
            .uat()
            .confirm_t8140_primary_high_root(root_snapshot)?;
        let gfx_init = g17_rtkit::encode_initdata_doorbell(
            built.handoff().primary.instance.root,
        )
        .map_err(|_| EINVAL)?;
        registers.t8140_pre_initdata_clear()?;
        Pin::new(&mut gfx_rtkit).start_endpoint(g17_rtkit::ENDPOINT_GFX_MESSAGES)?;
        Pin::new(&mut gfx_rtkit).start_endpoint(g17_rtkit::ENDPOINT_GFX_INTERRUPTS)?;
        Pin::new(&mut gfx_rtkit).send_message(g17_rtkit::ENDPOINT_GFX_MESSAGES, gfx_init)?;
        dev_info!(pdev.as_ref(), "G17P session: primary initdata sent\n");
        wait_initdata_ack(&gfx_state)?;
        dev_info!(pdev.as_ref(), "G17P session: primary initdata ACK received\n");
        manager = Some(built);
        Ok(())
    })?;

    let mut manager = manager.ok_or(EIO)?;
    dev_info!(pdev.as_ref(), "G17P session: GFX1 processor started\n");
    Pin::new(&mut gfx1_rtkit).boot()?;
    dev_info!(pdev.as_ref(), "G17P session: GFX1 RTKit handshake complete\n");
    manager.uat().mirror_t8140_secondary_high_root()?;
    Pin::new(&mut gfx1_rtkit).start_endpoint(g17_rtkit::ENDPOINT_GFX_MESSAGES)?;
    Pin::new(&mut gfx1_rtkit).start_endpoint(g17_rtkit::ENDPOINT_GFX_INTERRUPTS)?;
    let gfx1_init = g17_rtkit::encode_initdata_doorbell(
        manager.handoff().secondary.instance.root,
    )
    .map_err(|_| EINVAL)?;
    Pin::new(&mut gfx1_rtkit).send_message(g17_rtkit::ENDPOINT_GFX_MESSAGES, gfx1_init)?;
    dev_info!(pdev.as_ref(), "G17P session: secondary initdata sent\n");
    wait_initdata_ack(&gfx1_state)?;
    dev_info!(pdev.as_ref(), "G17P session: secondary initdata ACK received\n");

    let session = finish_boot_session(
        pdev,
        drm,
        registers,
        &mut manager,
        test_stage,
        cpus,
        gfx_state,
        gfx_rtkit,
        gfx1_state,
        gfx1_rtkit,
    )?;
    Ok((manager, session))
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
    pub(crate) fn drm_ref(&self) -> Result<ARef<AsahiDevice>> {
        self.drm.as_ref().cloned().ok_or(ENODEV)
    }

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
                    context_id: 2,
                    dispatch_identity: 0x0100_01d7_0200_01dc,
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
        let doorbell = match submit_result {
            Ok(doorbell) => doorbell,
            Err(error) => {
                return match g17_lifecycle::recover_persistent_runtime(self) {
                    Ok(()) => Err(error),
                    Err(recovery_error) => Err(recovery_error),
                };
            }
        };
        let channel_path = doorbell.is_some();
        if let Some(doorbell) = doorbell {
            dev_info!(
                self.pdev.as_ref().ok_or(ENODEV)?.as_ref(),
                "G17P compute: CL_2 group published; ringing work channel 10\n"
            );
            if let Err(error) = Pin::new(self.gfx_rtkit.as_mut().ok_or(ENODEV)?)
                .send_message(g17_rtkit::ENDPOINT_GFX_INTERRUPTS, doorbell)
            {
                return match g17_lifecycle::recover_persistent_runtime(self) {
                    Ok(()) => Err(error),
                    Err(recovery_error) => Err(recovery_error),
                };
            }
        }

        let completion = owned_timestamps.with_cpu_bytes(|raw| {
            const POLLS: usize = 500;
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
                if channel_path {
                    let user_timestamps = owned_timestamps.with_cpu_bytes(|raw| {
                        Ok([
                            u64::from_le_bytes(raw[0..8].try_into().map_err(|_| ERANGE)?),
                            u64::from_le_bytes(raw[8..16].try_into().map_err(|_| ERANGE)?),
                        ])
                    });
                    let snapshot = {
                        let manager = self.manager.as_mut().ok_or(ENODEV)?;
                        let queue = self.queue.as_mut().ok_or(ENODEV)?;
                        manager.snapshot_compute_timeout(queue)
                    };
                    let pdev = self.pdev.as_ref().ok_or(ENODEV)?;
                    match (snapshot, user_timestamps) {
                        (Ok(snapshot), Ok(user_timestamps)) => {
                            let control = snapshot.control;
                            let channel = snapshot.channel;
                            let graph = snapshot.graph;
                            dev_err!(
                                pdev.as_ref(),
                                "G17P compute timeout: control primary={:?} secondary={:?}\n",
                                control.primary,
                                control.secondary
                            );
                            dev_err!(
                                pdev.as_ref(),
                                "G17P compute timeout: CL_2 table={} ring={:#x} states={:#x?} cursors={:?} slot={} queue={:#x} kind={} flags={:#x} doorbell={:#x}\n",
                                channel.table_index,
                                channel.ring,
                                channel.state_addresses,
                                channel.cursors,
                                channel.slot_index,
                                channel.slot_queue,
                                channel.slot_kind,
                                channel.slot_flags,
                                g17_submission::G17P_COMPUTE_WORK_DOORBELL
                            );
                            dev_err!(
                                pdev.as_ref(),
                                "G17P compute timeout: inner queue={:#x} pointers={:#x} ring={:#x} done/read/write={:?} gpu-read={:?} uuid={:#x} context={:#x}\n",
                                graph.queue,
                                graph.pointers,
                                graph.item_ring,
                                graph.cursors,
                                graph.gpu_read_cursors,
                                graph.uuid,
                                graph.queue_context
                            );
                            dev_err!(
                                pdev.as_ref(),
                                "G17P compute timeout: items={:#x?} event={:#x?} status={:#x?}\n",
                                graph.items,
                                graph.event,
                                graph.status
                            );
                            dev_err!(
                                pdev.as_ref(),
                                "G17P compute timeout: descriptor={:#x} selector={:#x} context={} grid={} timestamp-pointers={:#x?} values={:#x?}\n",
                                graph.descriptor,
                                graph.descriptor_selector,
                                graph.descriptor_context,
                                graph.descriptor_grid,
                                graph.descriptor_timestamps,
                                user_timestamps
                            );
                            dev_err!(
                                pdev.as_ref(),
                                "G17P compute timeout: optional-contexts={:#x?} grid={} uuid={:#x} shared={:#x} channel={:#x}; high-context={:#x} record={:#x?}\n",
                                graph.optional_contexts,
                                graph.optional_grid,
                                graph.optional_uuid,
                                graph.optional_shared,
                                graph.optional_channel,
                                graph.context_high,
                                graph.context_record
                            );
                        }
                        (Err(snapshot_error), _) => dev_err!(
                            pdev.as_ref(),
                            "G17P compute timeout snapshot failed ({:?})\n",
                            snapshot_error
                        ),
                        (_, Err(timestamp_error)) => dev_err!(
                            pdev.as_ref(),
                            "G17P compute timeout timestamp read failed ({:?})\n",
                            timestamp_error
                        ),
                    }
                }
                return match g17_lifecycle::recover_persistent_runtime(self) {
                    Ok(()) => Err(error),
                    Err(recovery_error) => Err(recovery_error),
                };
            }
        };
        if !channel_path {
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
        let (manager, session) = match boot_initial_session(
            &pdev_ref,
            &drm,
            &registers,
            soc,
            num_clusters,
            test_stage,
        ) {
            Ok(runtime) => runtime,
            Err(error) => {
                dev_err!(
                    pdev.as_ref(),
                    "G17P live-boot: initial session failed ({:?})\n",
                    error
                );
                return Err(error);
            }
        };
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
    fn stop_cpu_pair(&mut self) -> bool {
        let result = match self.cpus.as_mut() {
            Some(cpus) => cpus.stop(),
            None => Ok(()),
        };
        match result {
            Ok(()) => true,
            Err(error) => {
                if let Some(pdev) = self.pdev.as_ref() {
                    dev_err!(
                        pdev.as_ref(),
                        "G17P teardown: CPU stop failed ({:?}); retaining firmware-owned state\n",
                        error
                    );
                } else {
                    pr_err!(
                        "G17P teardown: CPU stop failed ({:?}); retaining firmware-owned state\n",
                        error
                    );
                }
                false
            }
        }
    }

    fn retain_after_stop_failure(&mut self) {
        self.accepting_submissions = false;
        core::mem::forget(self.compute_binding.take());
        core::mem::forget(self.queue.take());
        core::mem::forget(self.gfx1_rtkit.take());
        core::mem::forget(self.gfx1_state.take());
        core::mem::forget(self.gfx_rtkit.take());
        core::mem::forget(self.gfx_state.take());
        core::mem::forget(self.cpus.take());
        core::mem::forget(self.manager.take());
        core::mem::forget(self.drm.take());
        core::mem::forget(self.registers.take());
        core::mem::forget(self.pdev.take());
    }

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
