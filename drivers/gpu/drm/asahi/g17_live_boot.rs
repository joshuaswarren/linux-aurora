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

use core::{
    mem::MaybeUninit,
    sync::atomic::{fence, AtomicBool, AtomicU32, Ordering},
};

use kernel::macros::vtable;
use kernel::{
    bindings,
    device::Core,
    error::{from_err_ptr, to_result},
    io::{Io, Mmio, MmioRaw},
    iosys_map::IoSysMapRef,
    platform,
    prelude::*,
    soc::apple::rtkit,
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
const INITDATA_ACK_POLLS: usize = 200;
const CONTROL_COUNTER_POLLS: usize = 100;
const FIRMWARE_RECOVERY_POLLS: usize = 500;
const G17P_CRASHLOG_MAX_BYTES: usize = 256 * 1024;
const G17P_RTKIT_UAT_DVA_MASK: u64 = (1u64 << 40) - 1;

/// Module-local, read-only view of one existing ASCWrap mailbox provider.
///
/// The C provider owns the mapping and all lifecycle transitions. This view
/// neither maps an alias nor changes callback ownership; it only borrows the
/// provider's established mailbox-register pointer for two control reads.
struct G17PMailboxProvider {
    mbox: *mut bindings::apple_mbox,
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
enum G17PAscwrapV6Role {
    Gfx,
    Gfx1,
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
struct G17PAscwrapV6Lifecycle {
    role: G17PAscwrapV6Role,
    wrapper: (u64, u64),
    iop_vbar: (u64, u64),
    missing: u32,
}

impl G17PMailboxProvider {
    fn get(dev: &kernel::device::Device, index: usize) -> Result<Self> {
        // SAFETY: `dev` is live and apple_mbox_get() establishes a device link
        // to keep the resolved provider alive for this consumer.
        let mbox = unsafe {
            from_err_ptr(bindings::apple_mbox_get(
                dev.as_raw(),
                index.try_into()?,
            ))?
        };
        Ok(Self { mbox })
    }

    fn lifecycle(&self) -> Result<G17PAscwrapV6Lifecycle> {
        let mut raw = MaybeUninit::<bindings::apple_mbox_ascwrap_v6_lifecycle>::uninit();
        // SAFETY: `self.mbox` is the live provider returned by
        // apple_mbox_get() and `raw` points to writable storage.
        to_result(unsafe {
            bindings::apple_mbox_ascwrap_v6_get_lifecycle(self.mbox, raw.as_mut_ptr())
        })?;
        // SAFETY: the successful C call initialized every field.
        let raw = unsafe { raw.assume_init() };
        let role = match raw.role as u32 {
            1 => G17PAscwrapV6Role::Gfx,
            2 => G17PAscwrapV6Role::Gfx1,
            _ => return Err(EINVAL),
        };
        Ok(G17PAscwrapV6Lifecycle {
            role,
            wrapper: (raw.wrapper_start, raw.wrapper_size),
            iop_vbar: (raw.iop_vbar_start, raw.iop_vbar_size),
            missing: raw.missing,
        })
    }

    fn require_safe_cpu_lifecycle(&self) -> Result {
        // SAFETY: this is a capability check on the live provider pointer.
        to_result(unsafe {
            bindings::apple_mbox_ascwrap_v6_require_safe_cpu_lifecycle(self.mbox)
        })
    }

    fn start_cpu(&self) -> Result {
        // SAFETY: lifecycle serialization and MMIO ownership remain in C.
        to_result(unsafe { bindings::apple_mbox_ascwrap_v6_start_cpu(self.mbox) })
    }

    fn stop_cpu(&self) -> Result {
        // SAFETY: lifecycle serialization and MMIO ownership remain in C.
        to_result(unsafe { bindings::apple_mbox_ascwrap_v6_stop_cpu(self.mbox) })
    }

    fn a2i_control(&self) -> Result<u32> {
        const A2I_CONTROL: usize = 0x110;
        const CONTROL_WINDOW_SIZE: usize = A2I_CONTROL + core::mem::size_of::<u32>();

        // SAFETY: the provider owns the live ASC mailbox mapping. The device
        // link established by apple_mbox_get() keeps it valid, and this view
        // is bounded to the standard read-only A2I control register.
        let raw = MmioRaw::<CONTROL_WINDOW_SIZE>::new(
            unsafe { (*self.mbox).regs as usize },
            CONTROL_WINDOW_SIZE,
        )?;
        // SAFETY: `raw` covers the complete 32-bit control register read.
        Ok(unsafe { Mmio::from_raw(&raw) }
            .relaxed()
            .read32(A2I_CONTROL))
    }

}

// SAFETY: the provider mapping is device-link protected and the reader issues
// only relaxed MMIO reads. Mailbox mutation remains synchronized in C.
unsafe impl Send for G17PMailboxProvider {}
// SAFETY: as above; concurrent snapshots do not change either FIFO.
unsafe impl Sync for G17PMailboxProvider {}

fn log_g17p_a2i_control(
    dev: &kernel::device::Device,
    mailbox: &G17PMailboxProvider,
    checkpoint: &'static str,
) {
    match mailbox.a2i_control() {
        Ok(control) => dev_info!(
            dev,
            "G17P A2I checkpoint={} raw={:#x} count={} empty={} full={} read={} write={} enabled={}\n",
            checkpoint,
            control,
            (control >> 20) & 0xf,
            (control >> 17) & 1,
            (control >> 16) & 1,
            (control >> 12) & 0xf,
            (control >> 8) & 0xf,
            control & 1,
        ),
        Err(error) => dev_err!(
            dev,
            "G17P A2I checkpoint={} read failed ({:?})\n",
            checkpoint,
            error,
        ),
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
struct G17PWorkScanObservation {
    first: u32,
    last: u32,
    observed_or: u32,
    transitions: u32,
    samples: u32,
    failures: u32,
}

impl G17PWorkScanObservation {
    const fn new() -> Self {
        Self {
            first: 0,
            last: 0,
            observed_or: 0,
            transitions: 0,
            samples: 0,
            failures: 0,
        }
    }

    fn observe(&mut self, value: u32) {
        if self.samples == 0 {
            self.first = value;
        } else if self.last != value {
            self.transitions = self.transitions.saturating_add(1);
        }
        self.last = value;
        self.observed_or |= value;
        self.samples = self.samples.saturating_add(1);
    }

    fn observe_failure(&mut self) {
        self.failures = self.failures.saturating_add(1);
    }
}

fn log_g17p_status_a(
    dev: &kernel::device::Device,
    manager: &mut g17_manager::G17PManagerConstruction,
    checkpoint: &'static str,
) {
    match manager.status_a_snapshot() {
        Ok(snapshot) => dev_info!(
            dev,
            "G17P Status-A checkpoint={} scan-address={:#x} scan-active={} runtime-address={:#x} raw={:#x} active={} scheduler-constructed={}\n",
            checkpoint,
            snapshot.scan_address,
            snapshot.scan_active,
            snapshot.address,
            snapshot.raw,
            snapshot.active,
            snapshot.scheduler_constructed,
        ),
        Err(error) => dev_err!(
            dev,
            "G17P Status-A checkpoint={} read failed ({:?})\n",
            checkpoint,
            error,
        ),
    }
}

fn log_g17p_firmware_recovery_handshake(
    dev: &kernel::device::Device,
    manager: &mut g17_manager::G17PManagerConstruction,
    checkpoint: &'static str,
) {
    match manager.firmware_recovery_handshake_snapshot() {
        Ok(snapshot) => dev_info!(
            dev,
            "G17P firmware-recovery handshake checkpoint={} status-b={:#x} epoch-address={:#x} epoch={} state-address={:#x} state={} callback-known-unregistered={} waiting-first-response={} waiting-final-clear={} power-callback-count-address={:#x} power-callback-count={}\n",
            checkpoint,
            snapshot.status_b,
            snapshot.epoch_address,
            snapshot.epoch,
            snapshot.state_address,
            snapshot.state,
            snapshot.callback_known_unregistered(),
            snapshot.waiting_for_first_response(),
            snapshot.waiting_for_final_clear(),
            snapshot.power_callback_count_address,
            snapshot.power_callback_count,
        ),
        Err(error) => dev_err!(
            dev,
            "G17P firmware-recovery handshake checkpoint={} read failed ({:?})\n",
            checkpoint,
            error,
        ),
    }
}

/// Drain a firmware-initiated recovery before Linux has registered or
/// published any user queue. macOS consumes the type-4 event, accounts for the
/// host recovery generation, then calls the ArmFirmware pre/post recovery methods. Those
/// methods validate state 1/3 and publish 2/0 with barriers around each write.
fn complete_primary_empty_recovery(
    pdev: &platform::Device,
    manager: &mut g17_manager::G17PManagerConstruction,
    state: &G17PRoleRtkitState,
) -> Result {
    let notification_count = state
        .firmware_event_notifications
        .swap(0, Ordering::AcqRel);
    let initial_handshake = manager.firmware_recovery_handshake_snapshot()?;
    if initial_handshake.state != 0 || notification_count != 0 {
        dev_info!(
            pdev.as_ref(),
            "G17P recovery check: notifications={} epoch={} state={}\n",
            notification_count,
            initial_handshake.epoch,
            initial_handshake.state
        );
    }
    let record = loop {
        let Some(record) = manager.drain_primary_firmware_event()? else {
            let handshake = manager.firmware_recovery_handshake_snapshot()?;
            if handshake.state == 0 {
                if notification_count != 0 {
                    dev_info!(
                        pdev.as_ref(),
                        "G17P recovery: cleared {} stale event notifications with an empty ring\n",
                        notification_count
                    );
                }
                return Ok(());
            }
            dev_err!(
                pdev.as_ref(),
                "G17P recovery: notification count {} / state {} has no primary event record\n",
                notification_count,
                handshake.state
            );
            return Err(EIO);
        };
        let event_type = u32::from_le_bytes(record.raw[0..4].try_into().map_err(|_| EIO)?);
        if event_type == g17_completion::G17P_FIRMWARE_EVENT_RECOVERY_TYPE {
            break record;
        }
        if event_type != g17_completion::G17P_FIRMWARE_EVENT_UMA_GROW_TYPE {
            return Err(EIO);
        }
        let uma = g17_completion::decode_g17p_firmware_uma_grow_event(&record.raw)
            .map_err(|_| EIO)?;
        let expected_operand_table = g17_initdata::CONTROL_OPERAND_TABLE_ADDRESS;
        let expected_cookie = expected_operand_table.checked_add(0x640).ok_or(EOVERFLOW)?;
        if uma.stamp_index != 1
            || uma.pool_index != 0
            || uma.shared_control != g17_initdata::PARTIAL_OPENING_CONTROL_SHARED_ADDRESS
            || uma.operand_table != expected_operand_table
            || uma.cookie != expected_cookie
            || uma.result != 0x18
        {
            dev_err!(
                pdev.as_ref(),
                "G17P UMA grow: rejected orphan completion stamp={} pool={} shared={:#x} operand={:#x} cookie={:#x} result={:#x}\n",
                uma.stamp_index,
                uma.pool_index,
                uma.shared_control,
                uma.operand_table,
                uma.cookie,
                uma.result
            );
            return Err(EIO);
        }
        dev_info!(
            pdev.as_ref(),
            "G17P UMA grow: consumed unmatched bootstrap completion stamp={} pool={} cookie={:#x} result={:#x} cursor={}->{} producer={}\n",
            uma.stamp_index,
            uma.pool_index,
            uma.cookie,
            uma.result,
            record.consumer_before,
            record.consumer_published,
            record.producer_snapshot
        );
    };
    let recovery = g17_completion::decode_g17p_firmware_recovery_event(&record.raw)
        .map_err(|_| EIO)?;
    let handshake = manager.firmware_recovery_handshake_snapshot()?;
    if recovery.stamp_index != -1
        || recovery.generation.wrapping_add(1) != handshake.epoch
        || handshake.state != 1
    {
        dev_err!(
            pdev.as_ref(),
            "G17P recovery: rejected type-4 event generation={} stamp={} epoch={} state={}\n",
            recovery.generation,
            recovery.stamp_index,
            handshake.epoch,
            handshake.state
        );
        return Err(EIO);
    }
    dev_info!(
        pdev.as_ref(),
        "G17P recovery: consumed event generation={} cursor={}->{} producer={} notifications={}\n",
        recovery.generation,
        record.consumer_before,
        record.consumer_published,
        record.producer_snapshot,
        notification_count
    );
    let cause = manager.primary_firmware_recovery_cause_snapshot()?;
    dev_info!(
        pdev.as_ref(),
        "G17P recovery cause: zero0={:#x} detected-source-count={} zero1={:#x} command-word={:#x} raw48cc={:#x} raw48d0={:#x} fault-status={:#x} host-recovery={}\n",
        cause.zero_0,
        cause.detected_source_count,
        cause.zero_1,
        cause.command_word,
        cause.raw_48cc,
        cause.raw_48d0,
        cause.fault_status,
        cause.host_recovery
    );
    if cause.host_recovery != 0 {
        dev_err!(
            pdev.as_ref(),
            "G17P recovery: event-backed path has host-recovery flag {}\n",
            cause.host_recovery
        );
        return Err(EIO);
    }
    let accepted_generation =
        manager.accept_empty_primary_firmware_recovery(recovery.generation)?;
    dev_info!(
        pdev.as_ref(),
        "G17P recovery: accepted host generation {} before empty scheduler recovery\n",
        accepted_generation
    );

    manager.acknowledge_primary_firmware_recovery(1, 2)?;
    let mut post_state = 2;
    for _ in 0..FIRMWARE_RECOVERY_POLLS {
        post_state = manager.firmware_recovery_handshake_snapshot()?.state;
        if post_state == 3 {
            break;
        }
        if state.crashed.load(Ordering::Acquire) {
            return Err(EIO);
        }
        fsleep(Delta::from_millis(1));
    }
    if post_state != 3 {
        dev_err!(
            pdev.as_ref(),
            "G17P recovery: firmware stopped at state {}, expected post-recovery state 3\n",
            post_state
        );
        return Err(ETIMEDOUT);
    }
    let recovery_info = manager.primary_firmware_recovery_info_snapshot()?;
    dev_info!(
        pdev.as_ref(),
        "G17P recovery info: emitted={} valid={} first-valid-index={:?} first-valid-flags={:#x}\n",
        recovery_info.emitted_entry_count,
        recovery_info.valid_entry_count,
        recovery_info.first_valid_index,
        recovery_info.first_valid_flags
    );
    if recovery_info.emitted_entry_count != 0 || recovery_info.valid_entry_count != 0 {
        dev_err!(
            pdev.as_ref(),
            "G17P recovery: non-empty recovery-info table cannot use the prepublication path\n"
        );
        return Err(EBUSY);
    }
    manager.acknowledge_primary_firmware_recovery(3, 0)?;
    dev_info!(
        pdev.as_ref(),
        "G17P recovery: native pre/post handshake completed at epoch {}\n",
        handshake.epoch
    );
    Ok(())
}

fn log_g17p_scheduler_gate(
    dev: &kernel::device::Device,
    registers: &regs::Resources,
    checkpoint: &'static str,
) {
    match registers.g17p_scheduler_gate_registers() {
        Ok(snapshot) => dev_info!(
            dev,
            "G17P scheduler gate checkpoint={} fender={:#x} armed={} gpc-state={:#x} host-irq-enable={:#x} host-irq-status={:#x}\n",
            checkpoint,
            snapshot.fender_dynamic_gating,
            snapshot.fender_wake_armed(),
            snapshot.host_irq_summary,
            snapshot.host_irq_enable,
            snapshot.host_irq_status,
        ),
        Err(error) => dev_err!(
            dev,
            "G17P scheduler gate checkpoint={} read failed ({:?})\n",
            checkpoint,
            error,
        ),
    }
}

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
    firmware_event_notifications: AtomicU32,
    submission_phase: AtomicU32,
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
            firmware_event_notifications: AtomicU32::new(0),
            submission_phase: AtomicU32::new(g17_manager::G17P_SUBMIT_PHASE_IDLE),
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
fn crash_u32(raw: &[u8], offset: usize) -> Option<u32> {
    Some(u32::from_le_bytes(raw.get(offset..offset + 4)?.try_into().ok()?))
}

#[cfg(CONFIG_DEV_COREDUMP)]
fn crash_u64(raw: &[u8], offset: usize) -> Option<u64> {
    Some(u64::from_le_bytes(raw.get(offset..offset + 8)?.try_into().ok()?))
}

#[cfg(CONFIG_DEV_COREDUMP)]
fn crash_fourcc(a: u8, b: u8, c: u8, d: u8) -> u32 {
    ((a as u32) << 24) | ((b as u32) << 16) | ((c as u32) << 8) | d as u32
}

#[cfg(CONFIG_DEV_COREDUMP)]
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
struct G17PCrashRegisters {
    pc: u64,
    far: u64,
    esr: u64,
    sp: u64,
    psr: u64,
}

#[cfg(CONFIG_DEV_COREDUMP)]
fn g17p_crash_registers(raw: &[u8]) -> Option<G17PCrashRegisters> {
    let total = usize::try_from(crash_u32(raw, 8)?).ok()?.min(raw.len());
    let mut offset = 0x20usize;
    for _ in 0..64 {
        let kind = crash_u32(raw, offset)?;
        let size = usize::try_from(crash_u32(raw, offset + 12)?).ok()?;
        if kind == crash_fourcc(b'C', b'L', b'H', b'E') {
            return None;
        }
        if size < 16 || offset.checked_add(size)? > total {
            return None;
        }
        if kind == crash_fourcc(b'C', b'r', b'g', b'8') {
            let body = offset + 16;
            if size < 16 + 0x350 {
                return None;
            }
            return Some(G17PCrashRegisters {
                sp: crash_u64(raw, body + 0x100)?,
                pc: crash_u64(raw, body + 0x108)?,
                psr: crash_u64(raw, body + 0x110)?,
                far: crash_u64(raw, body + 0x330)?,
                esr: crash_u64(raw, body + 0x340)?,
            });
        }
        offset += size;
    }
    None
}

#[cfg(CONFIG_DEV_COREDUMP)]
fn log_g17p_crash_sections(state: &G17PRoleRtkitState, raw: &[u8]) {
    let Some(header) = crash_u32(raw, 0) else {
        return;
    };
    let Some(total) = crash_u32(raw, 8).and_then(|value| usize::try_from(value).ok()) else {
        return;
    };
    if header != crash_fourcc(b'C', b'L', b'H', b'E') || total > raw.len() {
        dev_err!(
            state.dev.as_ref(),
            "G17P {} crashlog header invalid: fourcc={:#x} size={:#x}/{:#x}\n",
            state.processor.name(),
            header,
            total,
            raw.len()
        );
        return;
    }
    if let Some(regs) = g17p_crash_registers(raw) {
        dev_err!(
            state.dev.as_ref(),
            "G17P {} firmware exception: pc={:#x} far={:#x} esr={:#x} sp={:#x} psr={:#x}\n",
            state.processor.name(),
            regs.pc,
            regs.far,
            regs.esr,
            regs.sp,
            regs.psr
        );
    }

    let mut offset = 0x20usize;
    for _ in 0..64 {
        let Some(kind) = crash_u32(raw, offset) else {
            break;
        };
        let Some(size) = crash_u32(raw, offset + 12)
            .and_then(|value| usize::try_from(value).ok())
        else {
            break;
        };
        if kind == crash_fourcc(b'C', b'L', b'H', b'E') {
            break;
        }
        let Some(end) = offset.checked_add(size) else {
            break;
        };
        if size < 16 || end > total {
            break;
        }
        if kind == crash_fourcc(b'C', b's', b't', b'r') && size >= 20 {
            let body = offset + 16;
            let id = crash_u32(raw, body).unwrap_or(0);
            let bytes = &raw[body + 4..end];
            let length = bytes.iter().position(|byte| *byte == 0).unwrap_or(bytes.len());
            let length = length.min(192);
            if let Ok(message) = core::str::from_utf8(&bytes[..length]) {
                dev_err!(
                    state.dev.as_ref(),
                    "G17P {} firmware message {}: {}\n",
                    state.processor.name(),
                    id,
                    message
                );
            }
        }
        offset = end;
    }
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
    records: &[G17PCrashRecord],
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
    let record = G17PCrashRecord {
        processor: state.processor,
        original_size,
        crashlog,
    };
    publish_g17p_crashlogs(state.dev.as_ref(), core::slice::from_ref(&record))?;
    let mut capture = state.crash_capture.lock();
    capture.records[state.processor as usize] = Some(record);
    dev_info!(
        state.dev.as_ref(),
        "G17P {} crashlog published and retained\n",
        state.processor.name()
    );
    Ok(())
}

#[cfg(CONFIG_DEV_COREDUMP)]
fn retain_g17p_crashlog_slice(
    state: &G17PRoleRtkitState,
    original_size: usize,
    crashlog: &[u8],
) -> Result {
    log_g17p_crash_sections(state, crashlog);
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
    log_g17p_crash_sections(state, &physical);
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
        } else if g17_completion::decode_g17p_firmware_event_notification(endpoint, message).is_ok()
        {
            // This callback records ingress and the current publication phase.
            // The serialized runtime owner drains and dispatches the 0x48-byte
            // ring; the IRQ callback never mutates its consumer or queue state.
            let count = data
                .firmware_event_notifications
                .fetch_add(1, Ordering::Relaxed)
                .wrapping_add(1);
            dev_info!(
                data.dev.as_ref(),
                "G17P firmware-event notification role={} count={} submit-phase={}\n",
                data.processor.name(),
                count,
                data.submission_phase.load(Ordering::Acquire)
            );
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
    role: G17PAscwrapV6Role,
    wrapper: (u64, u64),
    iop_vbar: (u64, u64),
}

const GFX_PROVIDER: ExpectedProvider = ExpectedProvider {
    role: G17PAscwrapV6Role::Gfx,
    wrapper: (0x4826_00000, 0x88000),
    iop_vbar: (0x4820_50000, 0x8),
};

const GFX1_PROVIDER: ExpectedProvider = ExpectedProvider {
    role: G17PAscwrapV6Role::Gfx1,
    wrapper: (0x482e_00000, 0x88000),
    iop_vbar: (0x4828_50000, 0x8),
};

fn validate_provider(
    provider: &G17PMailboxProvider,
    expected: ExpectedProvider,
) -> Result<G17PAscwrapV6Lifecycle> {
    let lifecycle = provider.lifecycle()?;
    if lifecycle.role != expected.role
        || lifecycle.wrapper != expected.wrapper
        || lifecycle.iop_vbar != expected.iop_vbar
    {
        return Err(EINVAL);
    }

    Ok(lifecycle)
}

impl g17_lifecycle::AscCpuLifecycle for G17PMailboxProvider {
    type Error = Error;

    fn start_cpu(&self) -> Result {
        G17PMailboxProvider::start_cpu(self)
    }

    fn stop_cpu(&self) -> Result {
        G17PMailboxProvider::stop_cpu(self)
    }
}

struct G17PBootedSession {
    cpus: g17_lifecycle::StartedCpuPair<G17PMailboxProvider>,
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
    manager: &mut g17_manager::G17PManagerConstruction,
    test_stage: G17PTestStage,
    cpus: g17_lifecycle::StartedCpuPair<G17PMailboxProvider>,
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
    log_g17p_firmware_recovery_handshake(
        pdev.as_ref(),
        manager,
        "native-opening-retired-baseline",
    );
    complete_primary_empty_recovery(pdev, manager, &gfx_state)?;
    log_g17p_firmware_recovery_handshake(
        pdev.as_ref(),
        manager,
        "native-opening-recovery-complete",
    );

    if !g17_submission::G17P_NATIVE_PRE_USER_EP21_MESSAGES.is_empty() {
        return Err(EINVAL);
    }
    manager.validate_native_pre_user_roots()?;
    if !test_stage.configures_qid4() {
        dev_info!(
            pdev.as_ref(),
            "G17P session: native control stage complete; no pre-user EP21 work/control message; QID4 remains untouched\n"
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

    let queue = manager.prepare_g17p_compute_queue(drm)?;
    dev_info!(
        pdev.as_ref(),
        "G17P session: SKSM QID 4 / selector 2 memory prepared; hardware registration deferred to first direct submitBuffer; stage {}\n",
        test_stage.name()
    );
    dev_info!(
        pdev.as_ref(),
        "G17P diagnostic: j700-dual-initdata-publication-v1 armed\n"
    );
    dev_info!(
        pdev.as_ref(),
        "G17P compute: native-first-submit-v2 armed; pre-user EP21 quiet, first real CL sends 0x0a then 0x10\n"
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
    let gfx = G17PMailboxProvider::get(pdev.as_ref(), 0)?;
    let gfx_lifecycle = validate_provider(&gfx, GFX_PROVIDER)?;
    let gfx1 = G17PMailboxProvider::get(pdev.as_ref(), 1)?;
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
        Ok(())
    })?;
    dev_info!(pdev.as_ref(), "G17P session: GFX1 processor started\n");
    Pin::new(&mut gfx1_rtkit).boot()?;
    dev_info!(pdev.as_ref(), "G17P session: GFX1 RTKit handshake complete\n");

    let root_snapshot = manager.uat().prepare_t8140_firmware_high_roots()?;
    manager
        .uat()
        .confirm_t8140_primary_high_root(root_snapshot)?;
    manager.uat().mirror_t8140_secondary_high_root()?;
    registers.t8140_pre_initdata_clear()?;

    Pin::new(&mut gfx_rtkit).start_endpoint(g17_rtkit::ENDPOINT_GFX_MESSAGES)?;
    Pin::new(&mut gfx_rtkit).start_endpoint(g17_rtkit::ENDPOINT_GFX_INTERRUPTS)?;
    Pin::new(&mut gfx1_rtkit).start_endpoint(g17_rtkit::ENDPOINT_GFX_MESSAGES)?;
    Pin::new(&mut gfx1_rtkit).start_endpoint(g17_rtkit::ENDPOINT_GFX_INTERRUPTS)?;
    Pin::new(&mut gfx_rtkit).send_message(g17_rtkit::ENDPOINT_GFX_MESSAGES, gfx_init)?;
    Pin::new(&mut gfx1_rtkit).send_message(g17_rtkit::ENDPOINT_GFX_MESSAGES, gfx1_init)?;
    dev_info!(
        pdev.as_ref(),
        "G17P session: primary and secondary initdata published\n"
    );
    wait_initdata_ack(&gfx_state)?;
    dev_info!(pdev.as_ref(), "G17P session: primary initdata ACK received\n");
    wait_initdata_ack(&gfx1_state)?;
    dev_info!(pdev.as_ref(), "G17P session: secondary initdata ACK received\n");

    finish_boot_session(
        pdev,
        drm,
        manager,
        test_stage,
        cpus,
        gfx_state,
        gfx_rtkit,
        gfx1_state,
        gfx1_rtkit,
    )
}

#[inline(never)]
fn boot_initial_session(
    pdev: &platform::Device,
    drm: &ARef<AsahiDevice>,
    registers: &regs::Resources,
    soc: &'static hw::agx3::SocConfig,
    num_clusters: u32,
    test_stage: G17PTestStage,
) -> Result<(KBox<g17_manager::G17PManagerConstruction>, G17PBootedSession)> {
    let gfx = G17PMailboxProvider::get(pdev.as_ref(), 0)?;
    let gfx_lifecycle = validate_provider(&gfx, GFX_PROVIDER)?;
    let gfx1 = G17PMailboxProvider::get(pdev.as_ref(), 1)?;
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
    let cpus = g17_lifecycle::StartedCpuPair::start_staged(gfx, gfx1, || {
        dev_info!(pdev.as_ref(), "G17P session: GFX processor started\n");
        Pin::new(&mut gfx_rtkit).boot()?;
        dev_info!(pdev.as_ref(), "G17P session: GFX RTKit handshake complete\n");
        Ok(())
    })?;
    dev_info!(pdev.as_ref(), "G17P session: GFX1 processor started\n");
    Pin::new(&mut gfx1_rtkit).boot()?;
    dev_info!(pdev.as_ref(), "G17P session: GFX1 RTKit handshake complete\n");

    let root_snapshot = uat
        .as_ref()
        .ok_or(EIO)?
        .prepare_t8140_firmware_high_roots()?;
    let mut manager = g17_manager::G17PManagerConstruction::new_with_uat(
        drm,
        soc,
        g17_manager::T8140_G17P_PLATFORM_CONFIG,
        num_clusters,
        uat.take().ok_or(EIO)?,
    )?;
    manager
        .uat()
        .confirm_t8140_primary_high_root(root_snapshot)?;
    manager.uat().mirror_t8140_secondary_high_root()?;
    registers.t8140_pre_initdata_clear()?;

    let handoff = manager.handoff();
    let gfx_init =
        g17_rtkit::encode_initdata_doorbell(handoff.primary.instance.root).map_err(|_| EINVAL)?;
    let gfx1_init = g17_rtkit::encode_initdata_doorbell(handoff.secondary.instance.root)
        .map_err(|_| EINVAL)?;
    Pin::new(&mut gfx_rtkit).start_endpoint(g17_rtkit::ENDPOINT_GFX_MESSAGES)?;
    Pin::new(&mut gfx_rtkit).start_endpoint(g17_rtkit::ENDPOINT_GFX_INTERRUPTS)?;
    Pin::new(&mut gfx1_rtkit).start_endpoint(g17_rtkit::ENDPOINT_GFX_MESSAGES)?;
    Pin::new(&mut gfx1_rtkit).start_endpoint(g17_rtkit::ENDPOINT_GFX_INTERRUPTS)?;
    Pin::new(&mut gfx_rtkit).send_message(g17_rtkit::ENDPOINT_GFX_MESSAGES, gfx_init)?;
    Pin::new(&mut gfx1_rtkit).send_message(g17_rtkit::ENDPOINT_GFX_MESSAGES, gfx1_init)?;
    dev_info!(
        pdev.as_ref(),
        "G17P session: primary and secondary initdata published\n"
    );
    wait_initdata_ack(&gfx_state)?;
    dev_info!(pdev.as_ref(), "G17P session: primary initdata ACK received\n");
    wait_initdata_ack(&gfx1_state)?;
    dev_info!(pdev.as_ref(), "G17P session: secondary initdata ACK received\n");

    let session = finish_boot_session(
        pdev,
        drm,
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
    manager: Option<KBox<g17_manager::G17PManagerConstruction>>,
    cpus: Option<g17_lifecycle::StartedCpuPair<G17PMailboxProvider>>,
    gfx_state: Option<Arc<G17PRoleRtkitState>>,
    gfx_rtkit: Option<rtkit::RtKit<G17PRtkitOps>>,
    gfx1_state: Option<Arc<G17PRoleRtkitState>>,
    gfx1_rtkit: Option<rtkit::RtKit<G17PRtkitOps>>,
    queue: Option<g17_manager::G17PSksmQueue>,
    compute_binding: Option<g17_manager::G17PComputeUserBinding>,
    compute_timestamps: Option<mmu::KernelMapping>,
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

    /// Publish one translated compute command and wait for its execution
    /// timestamps. The direct first-bind path retains the mapping and VM bind
    /// beyond this return until correlated ordinal-0 completion or stopped-CPU
    /// recovery, so timestamp visibility alone cannot release firmware state.
    pub(crate) fn submit_translated_compute(
        &mut self,
        vm: &mmu::Vm,
        command: &g17_uapi::TranslatedComputeCommand,
        owned_timestamps: mmu::KernelMapping,
    ) -> Result<[u64; 2]> {
        if !self.accepting_submissions {
            return Err(ENOTSUPP);
        }
        if self.is_crashed()
            || self.compute_binding.is_some()
            || self.compute_timestamps.is_some()
        {
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
        self.manager
            .as_ref()
            .ok_or(ENODEV)?
            .validate_prepublication_compute_queue(self.queue.as_ref().ok_or(ENODEV)?)?;
        let pdev = self.pdev.as_ref().ok_or(ENODEV)?.clone();
        let gfx_state = self.gfx_state.as_ref().ok_or(ENODEV)?.clone();
        complete_primary_empty_recovery(
            &pdev,
            self.manager.as_mut().ok_or(ENODEV)?,
            &gfx_state,
        )?;
        if g17_submission::require_g17p_running_client_power_transition(
            g17_submission::G17P_RUNNING_CLIENT_POWER_CONTRACT,
            self.cpus.is_some(),
            self.gfx1_rtkit.is_some(),
        )
        .is_err()
        {
            dev_err!(
                self.pdev.as_ref().ok_or(ENODEV)?.as_ref(),
                "G17P compute power: retained target1 CPU/RTKit owner missing before VM bind\n"
            );
            return Err(ENOTSUPP);
        }
        dev_info!(
            self.pdev.as_ref().ok_or(ENODEV)?.as_ref(),
            "G17P compute power: retained GFX1 target1 owners active; no submit power wire\n"
        );
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
            let gfx_state = self.gfx_state.as_ref().ok_or(ENODEV)?;
            gfx_state
                .submission_phase
                .store(g17_manager::G17P_SUBMIT_PHASE_IDLE, Ordering::Release);
            manager.submit_translated_compute(
                registers,
                queue,
                &gfx_state.submission_phase,
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
        let notifications = match submit_result {
            Ok(notifications) => notifications,
            Err(error) => {
                return match g17_lifecycle::recover_persistent_runtime(self) {
                    Ok(()) => Err(error),
                    Err(recovery_error) => Err(recovery_error),
                };
            }
        };
        let channel_path = notifications.is_some();
        if let Some(notifications) = notifications {
            if let Some(activation) = notifications.activation {
                log_g17p_scheduler_gate(
                    self.pdev.as_ref().ok_or(ENODEV)?.as_ref(),
                    self.registers.as_ref().ok_or(ENODEV)?,
                    "native-first-pre-0x0a",
                );
                log_g17p_status_a(
                    self.pdev.as_ref().ok_or(ENODEV)?.as_ref(),
                    self.manager.as_mut().ok_or(ENODEV)?,
                    "native-first-pre-0x0a",
                );
                log_g17p_firmware_recovery_handshake(
                    self.pdev.as_ref().ok_or(ENODEV)?.as_ref(),
                    self.manager.as_mut().ok_or(ENODEV)?,
                    "native-first-pre-0x0a",
                );
                log_g17p_a2i_control(
                    self.pdev.as_ref().ok_or(ENODEV)?.as_ref(),
                    self.cpus.as_ref().ok_or(ENODEV)?.gfx(),
                    "native-first-pre-0x0a",
                );
                dev_info!(
                    self.pdev.as_ref().ok_or(ENODEV)?.as_ref(),
                    "G17P compute: CL_2 published; sending priority-2 activation {:#x}\n",
                    activation
                );
                if let Err(error) = Pin::new(self.gfx_rtkit.as_mut().ok_or(ENODEV)?)
                    .send_message(g17_rtkit::ENDPOINT_GFX_INTERRUPTS, activation)
                {
                    return match g17_lifecycle::recover_persistent_runtime(self) {
                        Ok(()) => Err(error),
                        Err(recovery_error) => Err(recovery_error),
                    };
                }
                log_g17p_a2i_control(
                    self.pdev.as_ref().ok_or(ENODEV)?.as_ref(),
                    self.cpus.as_ref().ok_or(ENODEV)?.gfx(),
                    "native-first-post-0x0a-pre-0x10",
                );
                log_g17p_status_a(
                    self.pdev.as_ref().ok_or(ENODEV)?.as_ref(),
                    self.manager.as_mut().ok_or(ENODEV)?,
                    "native-first-post-0x0a-pre-0x10",
                );
            }
            if let Some(direct_kick) = notifications.direct_kick {
                fence(Ordering::SeqCst);
                dev_info!(
                    self.pdev.as_ref().ok_or(ENODEV)?.as_ref(),
                    "G17P compute: direct EP kick {:#x} after activation\n",
                    direct_kick
                );
                if let Err(error) = Pin::new(self.gfx_rtkit.as_mut().ok_or(ENODEV)?)
                    .send_message(g17_rtkit::ENDPOINT_GFX_INTERRUPTS, direct_kick)
                {
                    return match g17_lifecycle::recover_persistent_runtime(self) {
                        Ok(()) => Err(error),
                        Err(recovery_error) => Err(recovery_error),
                    };
                }
                log_g17p_scheduler_gate(
                    self.pdev.as_ref().ok_or(ENODEV)?.as_ref(),
                    self.registers.as_ref().ok_or(ENODEV)?,
                    "native-first-post-0x10",
                );
                log_g17p_a2i_control(
                    self.pdev.as_ref().ok_or(ENODEV)?.as_ref(),
                    self.cpus.as_ref().ok_or(ENODEV)?.gfx(),
                    "native-first-post-0x10",
                );
                log_g17p_status_a(
                    self.pdev.as_ref().ok_or(ENODEV)?.as_ref(),
                    self.manager.as_mut().ok_or(ENODEV)?,
                    "native-first-post-0x10",
                );
                log_g17p_firmware_recovery_handshake(
                    self.pdev.as_ref().ok_or(ENODEV)?.as_ref(),
                    self.manager.as_mut().ok_or(ENODEV)?,
                    "native-first-post-0x10",
                );
            }
        }

        let mut scan_observation = G17PWorkScanObservation::new();
        let completion = owned_timestamps.with_cpu_bytes(|raw| {
            const POLLS: usize = 500;
            for _ in 0..POLLS {
                match self
                    .manager
                    .as_mut()
                    .ok_or(ENODEV)
                    .and_then(|manager| manager.status_a_snapshot())
                {
                    Ok(snapshot) => scan_observation.observe(snapshot.scan_active),
                    Err(_) => scan_observation.observe_failure(),
                }
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
        dev_info!(
            self.pdev.as_ref().ok_or(ENODEV)?.as_ref(),
            "G17P Status-A scan observation first={} last={} or={} transitions={} samples={} failures={}\n",
            scan_observation.first,
            scan_observation.last,
            scan_observation.observed_or,
            scan_observation.transitions,
            scan_observation.samples,
            scan_observation.failures,
        );
        let completion = match completion {
            Ok(completion) => completion,
            Err(error) => {
                if channel_path {
                    log_g17p_scheduler_gate(
                        self.pdev.as_ref().ok_or(ENODEV)?.as_ref(),
                        self.registers.as_ref().ok_or(ENODEV)?,
                        "native-first-timeout",
                    );
                    log_g17p_status_a(
                        self.pdev.as_ref().ok_or(ENODEV)?.as_ref(),
                        self.manager.as_mut().ok_or(ENODEV)?,
                        "native-first-timeout",
                    );
                    log_g17p_firmware_recovery_handshake(
                        self.pdev.as_ref().ok_or(ENODEV)?.as_ref(),
                        self.manager.as_mut().ok_or(ENODEV)?,
                        "native-first-timeout",
                    );
                    log_g17p_a2i_control(
                        self.pdev.as_ref().ok_or(ENODEV)?.as_ref(),
                        self.cpus.as_ref().ok_or(ENODEV)?.gfx(),
                        "native-first-timeout",
                    );
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
                            let channel_scan = snapshot.channel_scan;
                            let channel_control_prefix = snapshot.channel_control_prefix;
                            let graph = snapshot.graph;
                            dev_err!(
                                pdev.as_ref(),
                                "G17P compute timeout: control primary={:?} secondary={:?}\n",
                                control.primary,
                                control.secondary
                            );
                            dev_err!(
                                pdev.as_ref(),
                                "G17P compute timeout: CL_2 table={} ring={:#x} state-ptrs(actual,mirror,producer)={:#x?} runtime-pointers(+120,+130)={:#x?} cursors(actual,mirror,producer)={:?} slot={} queue={:#x} kind={} flags={:#x} doorbell={:#x}\n",
                                channel.table_index,
                                channel.ring,
                                channel.state_addresses,
                                channel.runtime_descriptor_pointers,
                                channel.cursors,
                                channel.slot_index,
                                channel.slot_queue,
                                channel.slot_kind,
                                channel.slot_flags,
                                g17_submission::G17P_COMPUTE_WORK_DOORBELL
                            );
                            dev_err!(
                                pdev.as_ref(),
                                "G17P compute timeout: inner queue={:#x} pointers={:#x} ring={:#x} pointer[+0,+30,+40]={:?} queue[+18,+1c,+20]={:?} uuid={:#x} context={:#x}\n",
                                graph.queue,
                                graph.pointers,
                                graph.item_ring,
                                graph.cursors,
                                graph.gpu_read_cursors,
                                graph.uuid,
                                graph.queue_context
                            );
                            for scan in channel_scan {
                                dev_err!(
                                    pdev.as_ref(),
                                    "G17P compute timeout: scan table={} ring={:#x} state-ptrs(actual,mirror,producer)={:#x?} cursors(actual,mirror,producer)={:?} current-slot={} queue={:#x} kind={}\n",
                                    scan.table_index,
                                    scan.ring,
                                    scan.state_addresses,
                                    scan.cursors,
                                    scan.current_slot_index,
                                    scan.current_slot_queue,
                                    scan.current_slot_kind,
                                );
                            }
                            dev_err!(
                                pdev.as_ref(),
                                "G17P compute timeout: channel-control[0..0x38]={:x?}\n",
                                channel_control_prefix,
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
        if channel_path {
            self.compute_timestamps = Some(owned_timestamps);
            dev_info!(
                self.pdev.as_ref().ok_or(ENODEV)?.as_ref(),
                "G17P compute: first direct CL timestamps complete; retaining timestamp mapping, VM binding, and QID state until native ordinal-0 completion is wired\n"
            );
        } else {
            drop(self.compute_binding.take());
        }
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
        dev_info!(
            pdev.as_ref(),
            "G17P diagnostic: scheduler-root-checkpoints-v1 armed\n"
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
            compute_timestamps: None,
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
    /// pair-zero/channel-0 work notification through the retained primary
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

    /// Apply one fully routed normal-CL completion to the retained queue.
    ///
    /// The physical IRQ, ordinal-0 descriptor, MMIO acknowledgement, and
    /// EP0x20 stamp-event route remain closed. This legacy boundary therefore
    /// rejects the incomplete selector-2-only input before queue mutation.
    pub(crate) fn observe_compute_completion(
        &mut self,
        raw: &[u8],
        current: g17_completion::QueueIndices,
        published_prefix: u32,
    ) -> Result<g17_completion::QueuePrefixState> {
        let _ = (raw, current, published_prefix);
        if g17_completion::require_g17p_normal_cl_completion_route(
            g17_completion::G17P_NORMAL_CL_COMPLETION_ROUTE,
        )
        .is_err()
        {
            dev_err!(
                self.pdev.as_ref().ok_or(ENODEV)?.as_ref(),
                "G17P completion: live ordinal0 IRQ/EP0x20 route unavailable; outstanding unchanged\n"
            );
            return Err(ENOTSUPP);
        }
        dev_err!(
            self.pdev.as_ref().ok_or(ENODEV)?.as_ref(),
            "G17P completion: legacy selector2 input lacks ordinal0 stamp correlation; outstanding unchanged\n"
        );
        Err(ENOTSUPP)
    }

    /// Commit only a transaction which already passed ordinal-0 entry and
    /// EP0x20 stamp correlation. The route gate remains closed until Linux
    /// owns the physical IRQ, firmware-event ring, and completion ACK writer.
    pub(crate) fn apply_prepared_normal_cl_completion(
        &mut self,
        prepared: g17_completion::PreparedG17PNormalClCompletion,
    ) -> Result {
        g17_completion::require_g17p_normal_cl_completion_route(
            g17_completion::G17P_NORMAL_CL_COMPLETION_ROUTE,
        )
        .map_err(|_| ENOTSUPP)?;
        let manager = self.manager.as_ref().ok_or(ENODEV)?;
        let queue = self.queue.as_mut().ok_or(ENODEV)?;
        if prepared.entry.queue_id != manager.compute_queue_id(queue)? {
            return Err(EINVAL);
        }
        manager.complete_compute_entries(queue, prepared.completed_generation)
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
        drop(self.compute_timestamps.take());
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
        core::mem::forget(self.compute_timestamps.take());
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
        drop(self.compute_timestamps.take());
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
