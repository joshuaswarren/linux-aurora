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

/// One role's iBoot-placed RTKit image, from the live J700 ADT.
///
/// `/arm-io/gfx-asc` and `/arm-io/gfx1-asc` each carry a `segment-ranges`
/// property of 32-byte records `{u64 phys, u64 iova, u64 remap, u32 size,
/// u32 flags}`. On T8140 there are two segments per role, `__TEXT;__DATA`,
/// and `remap == phys`. Everything here is below the start of System RAM, so
/// Linux never maps it and cannot corrupt it -- but the firmware writes its
/// own `__DATA` the whole time it runs, and nothing puts it back.
#[derive(Debug, Copy, Clone)]
struct G17PFirmwareImage {
    text_phys: u64,
    data_phys: u64,
    data_size: usize,
}

const G17P_GFX_FIRMWARE_IMAGE: G17PFirmwareImage = G17PFirmwareImage {
    text_phys: 0x1_0000_974000,
    data_phys: 0x1_0001_d7c000,
    data_size: 0x13_4000,
};

const G17P_GFX1_FIRMWARE_IMAGE: G17PFirmwareImage = G17PFirmwareImage {
    text_phys: 0x1_0000_9c0000,
    data_phys: 0x1_0001_eb0000,
    data_size: 0x12_c000,
};

/// Pristine `__DATA` for both roles, taken before either processor ever ran.
#[cfg(CONFIG_DEV_COREDUMP)]
struct G17PFirmwareDataSnapshot {
    gfx: KVVec<u8>,
    gfx1: KVVec<u8>,
}

/// Snapshot both roles' `__DATA` while it is still exactly as iBoot left it.
///
/// Refuses unless each role's live IORVBAR names precisely the `__TEXT` base
/// this geometry expects, with its valid bit set. That reset vector is the one
/// on-machine witness that the ADT-derived addresses describe THIS board, and
/// it is read from the provider's own mapping, so a wrong table can never send
/// a write somewhere it does not belong.
#[cfg(CONFIG_DEV_COREDUMP)]
fn snapshot_g17p_firmware_data(pdev: &platform::Device) -> Option<G17PFirmwareDataSnapshot> {
    let images = [G17P_GFX_FIRMWARE_IMAGE, G17P_GFX1_FIRMWARE_IMAGE];
    for (index, image) in images.iter().enumerate() {
        let provider = G17PMailboxProvider::get(pdev.as_ref(), index).ok()?;
        let observed = provider.asc_snapshot().ok()?;
        let expected = image.text_phys | 1;
        if observed.iorvbar != expected {
            dev_warn!(
                pdev.as_ref(),
                "G17PMARK fw-snapshot role={} REFUSED: iorvbar={:#x} expected={:#x}; the pinned carveout geometry does not describe this board\n",
                index,
                observed.iorvbar,
                expected
            );
            return None;
        }
    }
    // The invalidating read, for the same reason the drift count uses it: the
    // baseline has to come from DRAM, not from whatever the AP happens to be
    // holding for these physical addresses after iBoot placed the image.
    let gfx = crate::pgtable::UatPageTable::copy_live_firmware_physical_range(
        G17P_GFX_FIRMWARE_IMAGE.data_phys,
        G17P_GFX_FIRMWARE_IMAGE.data_size,
    )
    .ok()?;
    let gfx1 = crate::pgtable::UatPageTable::copy_live_firmware_physical_range(
        G17P_GFX1_FIRMWARE_IMAGE.data_phys,
        G17P_GFX1_FIRMWARE_IMAGE.data_size,
    )
    .ok()?;
    dev_info!(
        pdev.as_ref(),
        "G17PMARK fw-snapshot armed gfx __DATA {:#x}+{:#x} gfx1 __DATA {:#x}+{:#x}; both reset vectors match the pinned __TEXT bases\n",
        G17P_GFX_FIRMWARE_IMAGE.data_phys,
        G17P_GFX_FIRMWARE_IMAGE.data_size,
        G17P_GFX1_FIRMWARE_IMAGE.data_phys,
        G17P_GFX1_FIRMWARE_IMAGE.data_size,
    );
    Some(G17PFirmwareDataSnapshot { gfx, gfx1 })
}

/// How many bytes of a role's live `__DATA` differ from its pristine copy.
///
/// Read through the cache-invalidating path, not the plain one. The probe-time
/// snapshot pulled all of `__DATA` into the AP's PIPT caches as clean lines,
/// and the firmware writes its globals over a path that does not reliably
/// invalidate them, so a plain write-back read here can hand back the pristine
/// bytes this is trying to compare against and report drift=0 for a segment
/// the firmware has been scribbling on for minutes. Reporting zero drift is
/// the one wrong answer that would retire the whole hypothesis.
#[cfg(CONFIG_DEV_COREDUMP)]
fn count_g17p_firmware_data_drift(image: &G17PFirmwareImage, pristine: &[u8]) -> Result<usize> {
    let live = crate::pgtable::UatPageTable::copy_live_firmware_physical_range(
        image.data_phys,
        image.data_size,
    )?;
    Ok(live
        .iter()
        .zip(pristine.iter())
        .filter(|(a, b)| a != b)
        .count())
}

/// Wait for both ASC wrappers to report idle after their CPUs were stopped.
///
/// `AppleASCWrapV6::stopCPU` clears RUN and then waits about 10 ms for
/// `IDLE_STATUS & 3` before anything downstream gates the power domain. The
/// Linux provider clears RUN and returns immediately, so a gate that follows
/// can land while the block still has AXI traffic in flight -- which is what a
/// GFX PMGR register latched BUSY with PS_ACTUAL=0, deaf to every later target
/// write, looks like. Logs what each role actually reached either way, so the
/// wait is a measurement and not just a delay.
fn wait_for_g17p_asc_idle(pdev: &platform::Device, checkpoint: &'static str) {
    const BUDGET_US: u64 = 10_000;
    const STEP_US: u64 = 50;
    for index in 0..2usize {
        let Ok(provider) = G17PMailboxProvider::get(pdev.as_ref(), index) else {
            continue;
        };
        let mut waited = 0u64;
        let mut observed = 0u32;
        let mut idle = false;
        loop {
            match provider.asc_snapshot() {
                Ok(state) => {
                    observed = state.idle_status;
                    // AppleASCWrapV6::_isIdle is `(IDLE_STATUS & 3) != 0`.
                    // Not `== 3`: bit 0 and bit 1 are separate idle witnesses
                    // and either one alone means idle.
                    if state.idle_status & 3 != 0 {
                        idle = true;
                        break;
                    }
                }
                Err(_) => break,
            }
            if waited >= BUDGET_US {
                break;
            }
            fsleep(Delta::from_micros(STEP_US as i64));
            waited += STEP_US;
        }
        dev_info!(
            pdev.as_ref(),
            "G17PMARK asc-idle role={} checkpoint={} idle={} waited_us={} idle_word={:#x}\n",
            index,
            checkpoint,
            idle,
            waited,
            observed
        );
    }
}

/// Runtime-PM entry points the Rust `kernel` crate does not wrap.
///
/// All of these are `EXPORT_SYMBOL_GPL` in `drivers/base/power/runtime.c`.
/// The inline helpers drivers normally call -- `pm_runtime_set_active`,
/// `pm_runtime_get_sync`, `pm_runtime_put_sync_suspend`,
/// `pm_runtime_resume_and_get` -- are exactly the compositions spelled out at
/// each call site below, so nothing is invented here.
extern "C" {
    fn devm_pm_runtime_enable(dev: *mut bindings::device) -> c_int;
    fn __pm_runtime_set_status(dev: *mut bindings::device, status: c_uint) -> c_int;
    fn __pm_runtime_suspend(dev: *mut bindings::device, rpmflags: c_int) -> c_int;
    fn __pm_runtime_resume(dev: *mut bindings::device, rpmflags: c_int) -> c_int;
    fn pm_runtime_barrier(dev: *mut bindings::device) -> c_int;
}

/// `RPM_GET_PUT` from `include/linux/pm_runtime.h`.
const RPM_GET_PUT: c_int = 0x04;
/// `RPM_ACTIVE` from `enum rpm_status`.
const RPM_ACTIVE: c_uint = 0;

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

    /// The registers that decide whether a restarted ASC will re-run its
    /// RTKit handshake.
    ///
    /// `apple_rtkit_boot()` is passive: it waits for the IOP to volunteer a
    /// HELLO and never pokes it. A rebuild therefore depends entirely on the
    /// ASC actually restarting from its reset vector when `CPU_RUN` is set
    /// again. Nothing in the driver has ever looked at whether it does, so
    /// read the deciding state rather than infer it from the timeout.
    /// The provider's own `struct device`, which is what carries the runtime-PM
    /// reference that keeps the shared `gfx` power domain up.
    fn provider_device(&self) -> *mut bindings::device {
        // SAFETY: the device link established by `get()` keeps the provider
        // and therefore this pointer valid for this consumer's lifetime.
        unsafe { (*self.mbox).dev }
    }

    /// A second handle on the same provider, for diagnostics that must read
    /// ASC state while the lifecycle owner holds the real one.
    ///
    /// `get()` established a device link that keeps the provider alive for the
    /// consumer's whole bound lifetime, and this type owns nothing else, so an
    /// alias cannot outlive what it points at.
    fn diagnostic_alias(&self) -> Self {
        Self { mbox: self.mbox }
    }

    fn asc_snapshot(&self) -> Result<G17PAscSnapshot> {
        /// ASCWrap v6 CPU control/status pair, from the provider's own
        /// `APPLE_ASCWRAP_V6_CPU_CONTROL`.
        const CPU_CONTROL: usize = 0x44;
        const CPU_STATUS: usize = 0x48;
        const CPU_WINDOW: usize = CPU_STATUS + core::mem::size_of::<u32>();
        /// The mailbox block starts at wrapper + 0x8000; its first word is the
        /// wrapper idle status `_isIdle` reads, and the two FIFO control words
        /// follow at the standard ASC offsets.
        const IDLE_STATUS: usize = 0x0;
        const A2I_CONTROL: usize = 0x110;
        const I2A_CONTROL: usize = 0x114;
        const MBOX_WINDOW: usize = I2A_CONTROL + core::mem::size_of::<u32>();
        /// The IOP reset vector lives in its own eight-byte window.
        const VBAR_WINDOW: usize = 0x8;

        // SAFETY: the provider owns all three mappings and the device link
        // established by apple_mbox_get() keeps them valid for this consumer.
        let (wrapper, regs, vbar) = unsafe {
            (
                (*self.mbox).wrapper_regs as usize,
                (*self.mbox).regs as usize,
                (*self.mbox).iop_vbar_regs as usize,
            )
        };
        if wrapper == 0 || regs == 0 || vbar == 0 {
            return Err(ENODEV);
        }
        let cpu_raw = MmioRaw::<CPU_WINDOW>::new(wrapper, CPU_WINDOW)?;
        let mbox_raw = MmioRaw::<MBOX_WINDOW>::new(regs, MBOX_WINDOW)?;
        let vbar_raw = MmioRaw::<VBAR_WINDOW>::new(vbar, VBAR_WINDOW)?;
        // SAFETY: each window covers exactly the relaxed reads issued below.
        let cpu = unsafe { Mmio::from_raw(&cpu_raw) };
        // SAFETY: as above.
        let mailbox = unsafe { Mmio::from_raw(&mbox_raw) };
        // SAFETY: as above.
        let iop_vbar = unsafe { Mmio::from_raw(&vbar_raw) };
        Ok(G17PAscSnapshot {
            cpu_control: cpu.relaxed().read32(CPU_CONTROL),
            cpu_status: cpu.relaxed().read32(CPU_STATUS),
            idle_status: mailbox.relaxed().read32(IDLE_STATUS),
            iorvbar: iop_vbar.relaxed().read64(0),
            a2i_control: mailbox.relaxed().read32(A2I_CONTROL),
            i2a_control: mailbox.relaxed().read32(I2A_CONTROL),
        })
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

/// One read of the ASC state a restart depends on.
#[derive(Debug, Copy, Clone, Default)]
struct G17PAscSnapshot {
    cpu_control: u32,
    cpu_status: u32,
    idle_status: u32,
    iorvbar: u64,
    a2i_control: u32,
    i2a_control: u32,
}

/// Report the deciding ASC state at one named point of a start or restart.
///
/// `run` is `CPU_CONTROL` bit 4, the only bit this driver has ever written.
/// `iorvbar` is the reset vector with its valid bit in bit 0: a zero here
/// after a restart means the ASC has nowhere to boot from, which no amount of
/// waiting for HELLO can fix.
fn log_g17p_asc_state(
    dev: &kernel::device::Device,
    provider: &G17PMailboxProvider,
    role: &'static str,
    checkpoint: &'static str,
) {
    match provider.asc_snapshot() {
        Ok(state) => dev_info!(
            dev,
            "G17PMARK asc-state role={} checkpoint={} cpu_control={:#x} run={} cpu_status={:#x} idle_word={:#x} iorvbar={:#x} a2i={:#x} i2a={:#x}\n",
            role,
            checkpoint,
            state.cpu_control,
            (state.cpu_control >> 4) & 1,
            state.cpu_status,
            state.idle_status,
            state.iorvbar,
            state.a2i_control,
            state.i2a_control,
        ),
        Err(error) => dev_warn!(
            dev,
            "G17PMARK asc-state role={} checkpoint={} unavailable ({:?})\n",
            role,
            checkpoint,
            error
        ),
    }
}

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

/// Accumulated KTrace drain outcome for one role across a submit poll loop.
#[derive(Debug, Copy, Clone, Default)]
struct G17PKtraceTotals {
    consumed: u32,
    printed: u32,
    skipped: u32,
    lost_records: u64,
    lost_windows: u32,
    saw_add_kicks: bool,
    last_pause_mask: u64,
    last_producer: u32,
}

impl G17PKtraceTotals {
    fn accumulate(&mut self, stats: &g17_resources::G17PKtraceDrainStats) {
        self.consumed = self.consumed.saturating_add(stats.consumed);
        self.printed = self.printed.saturating_add(stats.printed);
        self.skipped = self.skipped.saturating_add(stats.skipped);
        self.lost_records = self.lost_records.saturating_add(stats.lost_records);
        self.lost_windows = self.lost_windows.saturating_add(stats.lost_windows);
        self.saw_add_kicks |= stats.saw_add_kicks;
        if stats.last_pause_mask != 0 {
            self.last_pause_mask = stats.last_pause_mask;
        }
        self.last_producer = stats.producer;
    }
}

/// The KTrace drain is deliberately gated on the same parameter that arms the
/// class mask: arming without a consumer makes the firmware busy-wait once and
/// then silently drop records, so "armed but undrained" must be impossible.
fn g17p_ktrace_enabled() -> bool {
    *module_parameters::g17p_fw_trace.value() != 0
}

fn g17p_ktrace_role_name(role: g17_initdata::InstanceRole) -> &'static str {
    match role {
        g17_initdata::InstanceRole::Primary => "primary",
        g17_initdata::InstanceRole::Secondary => "secondary",
    }
}

/// Checkpoint probe: dump the firmware log ring. DRAM only, like the KTrace
/// drain -- neither touches the sgx window, so both are safe on a path where
/// the GPU cores may never have powered.
fn dump_g17p_firmware_log(
    dev: &kernel::device::Device,
    manager: &mut g17_manager::G17PManagerConstruction,
    checkpoint: &'static str,
) {
    if *module_parameters::g17p_fw_log.value() == 0 {
        return;
    }
    match manager.dump_firmware_log(60) {
        Ok(count) => dev_info!(
            dev,
            "G17P fwlog checkpoint={} records={}\n",
            checkpoint,
            count
        ),
        Err(error) => dev_warn!(
            dev,
            "G17P fwlog checkpoint={} dump failed ({:?})\n",
            checkpoint,
            error
        ),
    }
}

/// Checkpoint probe: verify the published channel-14 pointer pair and drain
/// whatever the firmware has already traced. Note this consumes what it reads.
fn probe_g17p_ktrace(
    dev: &kernel::device::Device,
    manager: &mut g17_manager::G17PManagerConstruction,
    checkpoint: &'static str,
) {
    if !g17p_ktrace_enabled() {
        return;
    }
    let verbose = *module_parameters::g17p_fw_ktrace_verbose.value() != 0;
    let budget = *module_parameters::g17p_fw_ktrace_budget.value();
    for role in [
        g17_initdata::InstanceRole::Primary,
        g17_initdata::InstanceRole::Secondary,
    ] {
        let role_name = g17p_ktrace_role_name(role);
        match manager.check_ktrace_ring_pointers(role) {
            Ok((state, ring)) => dev_info!(
                dev,
                "G17P ktrace pointers checkpoint={} role={} state={:#x} ring={:#x}\n",
                checkpoint,
                role_name,
                state,
                ring
            ),
            Err(error) => dev_warn!(
                dev,
                "G17P ktrace pointers checkpoint={} role={} check failed ({:?})\n",
                checkpoint,
                role_name,
                error
            ),
        }
        match manager.drain_ktrace_ring(role, verbose, budget) {
            Ok(stats) => dev_info!(
                dev,
                "G17P ktrace armed check checkpoint={} role={} producer-snapshot={} consumed-count={} consumer-before={} consumer-after={} printed={} skipped={} lost={}/{} addkicks={} pause-mask={:#x}\n",
                checkpoint,
                role_name,
                stats.producer,
                stats.consumed,
                stats.consumer_before,
                (stats.consumer_before + stats.consumed) % g17_completion::G17P_KTRACE_CAPACITY,
                stats.printed,
                stats.skipped,
                stats.lost_records,
                stats.lost_windows,
                stats.saw_add_kicks,
                stats.last_pause_mask
            ),
            Err(error) => dev_warn!(
                dev,
                "G17P ktrace armed check checkpoint={} role={} failed ({:?})\n",
                checkpoint,
                role_name,
                error
            ),
        }
    }
}

/// Checkpoint probe: read the firmware EVENT ring (channel-table entry 13).
///
/// Read-only unless `g17p_report_drain` is 2. This ring is where the firmware
/// asks the host to grow the tiling parameter buffer (event type 7), and the
/// G17P path has never read it, so a request would sit in DRAM unanswered
/// while the tiler waited. DRAM only -- no sgx MMIO, so it is safe at both
/// checkpoints including the timeout, where the cores have powered back down.
fn probe_g17p_reports_with_mode(
    dev: &kernel::device::Device,
    manager: &mut g17_manager::G17PManagerConstruction,
    checkpoint: &'static str,
    advance: bool,
    budget: u32,
) {
    for role in [
        g17_initdata::InstanceRole::Primary,
        g17_initdata::InstanceRole::Secondary,
    ] {
        let role_name = g17p_ktrace_role_name(role);
        match manager.check_report_ring_pointers(role) {
            Ok((state, ring)) => dev_info!(
                dev,
                "G17P report pointers checkpoint={} role={} state={:#x} ring={:#x}\n",
                checkpoint,
                role_name,
                state,
                ring
            ),
            Err(error) => dev_warn!(
                dev,
                "G17P report pointers checkpoint={} role={} check failed ({:?})\n",
                checkpoint,
                role_name,
                error
            ),
        }
        match manager.drain_report_ring(role, advance, budget) {
            Ok(stats) => dev_info!(
                dev,
                "G17P report ring checkpoint={} role={} producer={} consumer={} pending={} read={} printed={} undecoded={} grow-requests={} restart-requests={} history-records={} history-grow-requests={} history-restart-requests={} out-of-range={} advanced={} fwlog-write={}\n",
                checkpoint,
                role_name,
                stats.producer,
                stats.consumer_before,
                stats.producer.wrapping_sub(stats.consumer_before) & 0xff,
                stats.consumed,
                stats.printed,
                stats.skipped,
                stats.grow_requests,
                stats.restart_requests,
                stats.history_records,
                stats.history_grow_requests,
                stats.history_restart_requests,
                stats.cursor_out_of_range,
                advance,
                stats.fwlog_write
            ),
            Err(error) => dev_warn!(
                dev,
                "G17P report ring checkpoint={} role={} failed ({:?})\n",
                checkpoint,
                role_name,
                error
            ),
        }
    }
}

fn probe_g17p_reports(
    dev: &kernel::device::Device,
    manager: &mut g17_manager::G17PManagerConstruction,
    checkpoint: &'static str,
) {
    let mode = *module_parameters::g17p_report_drain.value();
    if mode == 0 {
        return;
    }
    probe_g17p_reports_with_mode(
        dev,
        manager,
        checkpoint,
        mode >= 2,
        *module_parameters::g17p_report_drain_budget.value(),
    );
}

/// Recovery-only report-ring history. Unlike the checkpoint diagnostic this
/// never runs on a successful submission and never advances either consumer.
fn probe_g17p_recovery_report_history(
    dev: &kernel::device::Device,
    manager: &mut g17_manager::G17PManagerConstruction,
) {
    if *module_parameters::g17p_recovery_snapshot.value() == 0 {
        return;
    }
    probe_g17p_reports_with_mode(dev, manager, "recovery-pre-ack-history", false, 8);
}

fn log_g17p_status_a(
    dev: &kernel::device::Device,
    manager: &mut g17_manager::G17PManagerConstruction,
    checkpoint: &'static str,
) {
    if !crate::util::g17p_verbose() {
        return;
    }
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

/// Log the firmware's own fault report next to the MMU fault bank.
///
/// `main_config + 0x26c` is bundle view 3, so the report is in host memory we
/// already own; the firmware names the blamed kick queue there, that queue's
/// data master, and the recovery state. Nothing else in the driver reads it.
fn log_g17p_fault_report(
    dev: &kernel::device::Device,
    manager: &mut g17_manager::G17PManagerConstruction,
    checkpoint: &'static str,
) {
    // This report is validated host-owned WC memory, not the diagnostic MMIO
    // fault bank. Keep it available with register probing disabled, especially
    // before recovery acknowledgement clears the firmware's blame fields.
    let mut blamed_slot: Option<usize> = None;
    let mut blamed_data_master: Option<u32> = None;
    match manager.firmware_fault_report_snapshot() {
        Ok(snapshot) => dev_info!(
            dev,
            "G17P fault report checkpoint={} report={:#x} state={} host-requested={} reason={} sources={} blamed-qid={}/{} ksm-slot={}/{} data-master={}/{}\n",
            checkpoint,
            snapshot.report,
            snapshot.state,
            snapshot.host_requested,
            snapshot.reason,
            snapshot.source_count,
            snapshot.qid_present,
            snapshot.qid,
            snapshot.slot_present,
            snapshot.slot,
            snapshot.data_master_present,
            snapshot.data_master,
        ),
        Err(error) => dev_warn!(
            dev,
            "G17P fault report checkpoint={} read failed ({:?})\n",
            checkpoint,
            error,
        ),
    }
    if let Ok(snapshot) = manager.firmware_fault_report_snapshot() {
        if snapshot.slot_present != 0 {
            blamed_slot = Some(snapshot.slot as usize);
        }
        if snapshot.data_master_present != 0 {
            blamed_data_master = Some(snapshot.data_master);
        }
    }
    // Keep this independent of the report read: failure of either bounded
    // host-memory diagnostic must not suppress the other or touch MMIO.
    //
    // Slot 0 is NOT the blamed slot. `FUN_5450` blames the first set bit of
    // the DM fault mask, and two live faults blamed slots 1 and 0. Reading
    // only slot 0 printed an all-zero record for both and said nothing about
    // either. Scan the whole array the view can hold, name every record the
    // firmware actually wrote, and always print the blamed one.
    let blamed = blamed_slot.filter(|slot| *slot < g17_resources::DM1_WATCHDOG_SLOT_COUNT);
    let mut populated = 0u32;
    let mut failures = 0u32;
    for slot in 0..g17_resources::DM1_WATCHDOG_SLOT_COUNT {
        match manager.firmware_dm1_slot_snapshot(slot) {
            Ok(snapshot) => {
                if snapshot.is_blank() {
                    continue;
                }
                populated |= 1u32 << slot;
                let blame_tag = match blamed == Some(slot) {
                    true => " BLAMED",
                    false => "",
                };
                dev_info!(
                    dev,
                    "G17P DM1 monitor checkpoint={} slot={}{} address={:#x} raw={:08x?} monitor-state={} monitor-timestamp={:#x}\n",
                    checkpoint,
                    slot,
                    blame_tag,
                    snapshot.address,
                    snapshot.raw,
                    snapshot.state(),
                    snapshot.timestamp(),
                );
            }
            Err(_) => failures = failures.saturating_add(1),
        }
    }
    // A blamed slot the firmware left blank is itself the result: it says the
    // progress monitor never observed that slot, so the blame came from the
    // fault mask alone. Print it explicitly rather than leaving it implied.
    if let Some(slot) = blamed {
        if populated & (1u32 << slot) == 0 {
            dev_info!(
                dev,
                "G17P DM1 monitor checkpoint={} slot={} BLAMED but blank: firmware wrote no progress record for it\n",
                checkpoint,
                slot,
            );
        }
    }
    // The record array offset 0xa40 is DM1's. A blame naming any other data
    // master is not described by these records at all, so say so instead of
    // letting DM1 rows be read as evidence about it.
    let dm_note = match blamed_data_master {
        Some(1) | None => "",
        Some(_) => " (records above are DM1's; the blamed data master is not DM1)",
    };
    dev_info!(
        dev,
        "G17P DM1 monitor checkpoint={} scanned={} populated-mask={:#x} unreadable={} blamed-slot={:?} blamed-data-master={:?}{}\n",
        checkpoint,
        g17_resources::DM1_WATCHDOG_SLOT_COUNT,
        populated,
        failures,
        blamed_slot,
        blamed_data_master,
        dm_note,
    );
}

/// A narrowly scoped, already-halted slot discriminator. Failure to observe
/// it must never prevent the host acknowledgement that resumes recovery.
fn log_g17p_halted_slot0(
    dev: &kernel::device::Device,
    manager: &mut g17_manager::G17PManagerConstruction,
    registers: Option<&regs::Resources>,
    expected_epoch: u64,
) {
    if *module_parameters::g17p_recovery_snapshot.value() == 0 {
        return;
    }
    let Some(registers) = registers else { return };
    let selected = match (
        manager.firmware_recovery_handshake_snapshot(),
        manager.firmware_fault_report_snapshot(),
    ) {
        (Ok(handshake), Ok(report)) => {
            let gate = g17_completion::G17PHaltedSlot0Gate {
                handshake_state: handshake.state,
                epoch: handshake.epoch,
                expected_epoch,
                report_state: report.state,
                host_requested: report.host_requested,
                slot_present: report.slot_present,
                slot: report.slot,
                data_master_present: report.data_master_present,
                data_master: report.data_master,
            };
            if gate.allows_generic_sample() {
                Some((report.data_master, gate.allows_dm1_progress_sample()))
            } else {
                None
            }
        }
        _ => None,
    };
    let Some((data_master, read_dm1_progress)) = selected else { return };
    match registers.g17p_halted_slot0_snapshot(read_dm1_progress) {
        Ok(Some(snapshot)) => {
            let stable = snapshot.state_before == snapshot.state_after;
            dev_info!(
                dev,
                "G17P halted slot0 epoch={} data-master={} gpc={:#x} c020-before={:#018x} c078={:#018x} c020-after={:#018x} stable={} progress-selector={} selector={} key-qid={} key-stamp={:#x}\n",
                expected_epoch,
                data_master,
                snapshot.gpc_state,
                snapshot.state_before,
                snapshot.key,
                snapshot.state_after,
                stable,
                g17_completion::g17p_slot_has_progress_selector(snapshot.state_before),
                (snapshot.state_before >> 24) & 3,
                (snapshot.key >> 40) & 0x7f,
                snapshot.key & 0xff_ffff_ffff,
            );
            if read_dm1_progress {
                if let Some(selector) =
                    g17_completion::g17p_dm1_monitor_selector_index(snapshot.state_before)
                {
                    dev_info!(
                        dev,
                        "G17P halted DM1 selected-progress epoch={} selector-index={} c020-stable={} count={}/8 errno={} regs=1668/13020/15150/15388/15390/15510/15518/a068 raw={:016x?}\n",
                        expected_epoch,
                        selector,
                        stable,
                        snapshot.progress_count,
                        snapshot.progress_errno,
                        &snapshot.progress[..snapshot.progress_count],
                    );
                }
            }
        }
        Ok(None) => dev_info!(dev, "G17P halted slot0: GPC off; no low-SGX access\n"),
        Err(error) => dev_warn!(dev, "G17P halted slot0: read failed ({:?}); continuing recovery\n", error),
    }
}

/// Preserve the fault selected by B1's reason-3 classifier before ACK. This
/// does not use Status-B's copied slot mask as a GMMU-fault predicate.
fn log_g17p_fault_address_candidates(
    dev: &kernel::device::Device,
    manager: &g17_manager::G17PManagerConstruction,
    async_compute_queues: &KVec<G17PAsyncComputeQueue>,
    label: &'static str,
    address_word: u64,
    vm_slot: u8,
) {
    if manager.log_fault_address_candidates(label, address_word, vm_slot) {
        return;
    }

    let matching_context = async_compute_queues
        .iter()
        .find_map(|queue| {
            let context = queue.context.as_deref()?;
            (queue.active.is_some() && context.context_id() == u32::from(vm_slot))
                .then_some((context, "active-compute"))
        })
        .or_else(|| {
            async_compute_queues.iter().find_map(|queue| {
                let context = queue.context.as_deref()?;
                (context.context_id() == u32::from(vm_slot))
                    .then_some((context, "retained-compute"))
            })
        });
    if let Some((context, owner)) = matching_context {
        manager.log_compute_fault_address_candidates(
            label,
            address_word,
            vm_slot,
            context,
            owner,
        );
        return;
    }

    dev_info!(
        dev,
        "G17P fault address[{}]: no retained render or compute owner for descriptor context {}\n",
        label,
        vm_slot,
    );
}

fn log_g17p_halted_gmmu(
    dev: &kernel::device::Device,
    manager: &mut g17_manager::G17PManagerConstruction,
    async_compute_queues: &KVec<G17PAsyncComputeQueue>,
    registers: Option<&regs::Resources>,
    expected_epoch: u64,
) {
    let Some(registers) = registers else { return };
    let allowed = match (
        manager.firmware_recovery_handshake_snapshot(),
        manager.firmware_fault_report_snapshot(),
    ) {
        (Ok(handshake), Ok(report)) => g17_completion::g17p_halted_gmmu_sample_allowed(
            handshake.state, handshake.epoch, expected_epoch,
            report.state, report.host_requested, report.reason,
        ),
        _ => false,
    };
    if !allowed {
        return;
    }
    match registers.g17p_halted_gmmu_snapshot() {
        Ok(Some(snapshot)) => {
            let stable = snapshot.info_before == snapshot.info_after;
            if let Some(address_word) = snapshot.address_word {
                let address = address_word << 6;
                dev_info!(
                    dev,
                    "G17P halted GMMU reason3 epoch={} gpc={:#x} info-before={:#018x} addr-word={:#018x} va={:#x} info-after={:#018x} stable={} reason={} read={} level={} unit={:#x} vm-slot={}\n",
                    expected_epoch, snapshot.gpc_state, snapshot.info_before,
                    address_word, address, snapshot.info_after, stable,
                    (snapshot.info_before >> 1) & 7,
                    (snapshot.info_before >> 4) & 1,
                    (snapshot.info_before >> 7) & 3,
                    (snapshot.info_before >> 9) & 0xff,
                    (snapshot.info_before >> 17) & 0x3f,
                );
                log_g17p_fault_address_candidates(
                    dev,
                    manager,
                    async_compute_queues,
                    "halted-reason3",
                    address_word,
                    ((snapshot.info_before >> 17) & 0x3f) as u8,
                );
            } else {
                dev_info!(
                    dev,
                    "G17P halted GMMU reason3 epoch={} gpc={:#x} info-before={:#018x} info-after={:#018x} stable={} no-valid-fault; address not read\n",
                    expected_epoch, snapshot.gpc_state,
                    snapshot.info_before, snapshot.info_after, stable,
                );
            }
        }
        Ok(None) => dev_info!(dev, "G17P halted GMMU reason3: GPC off; no low-SGX access\n"),
        Err(error) => dev_warn!(dev, "G17P halted GMMU reason3: read failed ({:?}); continuing recovery\n", error),
    }
}

fn validate_primary_bootstrap_uma_completion(
    pdev: &platform::Device,
    record: &g17_resources::G17PPrimaryFirmwareEventRecord,
) -> Result<g17_manager::G17PBootstrapUmaCompletion> {
    let uma = g17_completion::decode_g17p_firmware_uma_grow_event(&record.raw)
        .map_err(|_| EIO)?;
    let expected_operand_table = g17_initdata::CONTROL_OPERAND_TABLE_ADDRESS;
    let expected_cookie = g17_submission::G17P_USC_FREELIST_LOW_END;
    if uma.stamp_index != 1
        || uma.pool_index != 0
        || uma.shared_control != g17_initdata::CONTROL_SHARED_ADDRESS
        || uma.operand_table != expected_operand_table
        || uma.cookie != expected_cookie
        || uma.result != g17_submission::G17P_USC_FREELIST_GROW_DESCRIPTOR_BYTES
    {
        dev_err!(
            pdev.as_ref(),
            "G17P UMA grow: rejected bootstrap completion stamp={} pool={} shared={:#x} operand={:#x} cookie={:#x} result={:#x}\n",
            uma.stamp_index,
            uma.pool_index,
            uma.shared_control,
            uma.operand_table,
            uma.cookie,
            uma.result
        );
        return Err(EIO);
    }
    Ok(g17_manager::G17PBootstrapUmaCompletion {
        stamp: uma.stamp_index,
        hardware_buffer_id: uma.pool_index,
        shared_control: uma.shared_control,
        operand_table: uma.operand_table,
        cookie: uma.cookie,
        count: uma.result,
    })
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
struct G17PPrimaryFirmwareEventBatch {
    recovery_generation: Option<u64>,
    recovery_records: u32,
    uma_release: Option<g17_manager::G17PBootstrapUmaRelease>,
    blamed_stamp: Option<u8>,
    unknown_records: u32,
    completion_records: u32,
    completion_masks: [u64; 2],
    blamed_recovery_records: u32,
    /// Type-4 records naming a generation the host has already serviced.
    /// Counted and skipped, never a drain failure -- see the type-4 arm.
    stale_recovery_records: u32,
    notification_count: u32,
}

/// Drain the complete primary event-ring batch in native cursor order. Multiple
/// type-4 records for the same host generation are one scheduler restart
/// request; the accepted generation advances only when that coalesced request
/// is serviced.
fn drain_primary_firmware_event_batch(
    pdev: &platform::Device,
    manager: &mut g17_manager::G17PManagerConstruction,
    state: &G17PRoleRtkitState,
    signal: &G17PCompletionSignal,
    announce_device_control: &mut impl FnMut() -> Result,
) -> Result<G17PPrimaryFirmwareEventBatch> {
    let notification_count = state
        .firmware_event_notifications
        .swap(0, Ordering::AcqRel);
    let mut batch = G17PPrimaryFirmwareEventBatch {
        recovery_generation: None,
        recovery_records: 0,
        uma_release: None,
        blamed_stamp: None,
        unknown_records: 0,
        completion_records: 0,
        completion_masks: [0; 2],
        blamed_recovery_records: 0,
        stale_recovery_records: 0,
        notification_count,
    };
    let role = match state.processor {
        G17PProcessor::Gfx => g17_initdata::InstanceRole::Primary,
        G17PProcessor::Gfx1 => g17_initdata::InstanceRole::Secondary,
    };
    for _ in 0..g17_completion::G17P_FIRMWARE_EVENT_CAPACITY {
        let Some(record) = manager.drain_firmware_event(role)? else { break; };
        let event_type = u32::from_le_bytes(record.raw[0..4].try_into().map_err(|_| EIO)?);
        match event_type {
            g17_completion::G17P_FIRMWARE_EVENT_RECOVERY_TYPE => {
                let recovery =
                    g17_completion::decode_g17p_firmware_recovery_event(&record.raw)
                        .map_err(|_| EIO)?;
                if recovery.stamp_index != -1 {
                    // A stamp-blamed recovery is a real firmware request, not a
                    // malformed record. Returning EIO here aborted the whole
                    // batch drain and, because the caller treats that as a hard
                    // failure, left every later record in the ring unreachable --
                    // the same stall that unmodelled record types used to cause.
                    // Report it and keep draining; the recovery bookkeeping below
                    // is only valid for the empty (stamp -1) form, so skip it.
                    // Do NOT skip this. Draining past a stamp-blamed recovery
                    // fixed the ring stall but broke the handshake: skipping left
                    // `recovery_generation == None`, and every caller keys off
                    // that field, so nothing ever scheduled the recovery. The
                    // firmware raises halt unconditionally and then spins until
                    // the host writes 2 into status_b[0x4900]; never answering
                    // leaves KSM halted forever, which is the ETIMEDOUT we saw.
                    // Fall through to the same generation bookkeeping the empty
                    // form gets -- those checks are stamp-independent.
                    dev_err!(
                        pdev.as_ref(),
                        "G17P recovery: firmware requests stamp-blamed recovery generation={} stamp={}; servicing\n",
                        recovery.generation,
                        recovery.stamp_index
                    );
                    batch.blamed_recovery_records =
                        batch.blamed_recovery_records.saturating_add(1);
                    let stamp = u8::try_from(recovery.stamp_index).map_err(|_| EIO)?;
                    if stamp >= 128 {
                        return Err(EIO);
                    }
                    batch.blamed_stamp = Some(stamp);
                }
                dev_info!(
                    pdev.as_ref(),
                    "G17P event worker: type4 generation={} cursor={}->{} producer={}\n",
                    recovery.generation,
                    record.consumer_before,
                    record.consumer_published,
                    record.producer_snapshot
                );
                let accepted = manager.accepted_firmware_recovery_generation();
                if recovery.generation < accepted {
                    // A restart the host has ALREADY serviced. Both RTKit roles
                    // see one firmware request, so the second ring's copy is
                    // routinely read after `accept_empty_primary_firmware_recovery`
                    // has advanced the host generation past it. Failing the
                    // drain here was fatal twice over: the caller turns any
                    // error into `crashed`, and aborting the batch strands every
                    // later record in the ring -- the exact stall the
                    // stamp-blamed arm above and the `unknown` arm below were
                    // both already fixed for. Count it, skip the bookkeeping,
                    // keep draining. There is no restart to service and both
                    // command gates were released by the real one, so the
                    // type-4 `break` does not apply either.
                    batch.stale_recovery_records =
                        batch.stale_recovery_records.saturating_add(1);
                    if batch.stale_recovery_records <= 4 {
                        dev_warn!(
                            pdev.as_ref(),
                            "G17P event worker: type4 generation={} is behind the accepted host generation {}; already serviced, skipping\n",
                            recovery.generation,
                            accepted
                        );
                    }
                    continue;
                }
                if let Err(error) =
                    manager.validate_empty_primary_firmware_recovery(recovery.generation)
                {
                    // Ahead of the accepted generation: the host and the
                    // firmware genuinely disagree about how many restarts have
                    // been served, which is not something to drain past.
                    dev_err!(
                        pdev.as_ref(),
                        "G17P event worker: type4 generation={} is AHEAD of the accepted host generation {} ({:?})\n",
                        recovery.generation,
                        accepted,
                        error
                    );
                    return Err(error);
                }
                if batch
                    .recovery_generation
                    .is_some_and(|generation| generation != recovery.generation)
                {
                    dev_err!(
                        pdev.as_ref(),
                        "G17P recovery: one IRQ batch contains different generations {:?} and {}\n",
                        batch.recovery_generation,
                        recovery.generation
                    );
                    return Err(EIO);
                }
                batch.recovery_generation = Some(recovery.generation);
                batch.recovery_records = batch.recovery_records.checked_add(1).ok_or(EOVERFLOW)?;
                // Type4 schedules restartWorkQueue on a separate gate turn.
                // Do not consume the following ring record until that turn
                // has fully returned and released both native command gates.
                break;
            }
            g17_completion::G17P_FIRMWARE_EVENT_TVB_GROW_TYPE => {
                let request = match g17_completion::G17PFirmwareTvbGrowEvent::decode(&record.raw) {
                    Ok(request) => request,
                    Err(error) => {
                        dev_warn!(pdev.as_ref(), "G17P TVB grow: malformed request ({:?})\n", error);
                        // Native asserts here. Stop event service through the
                        // caller's existing failure path; a malformed request
                        // cannot safely identify an allocation or its reply.
                        return Err(EIO);
                    }
                };
                if manager.service_render_tvb_grow(request)? {
                    announce_device_control()?;
                }
            }
            g17_completion::G17P_FIRMWARE_EVENT_UMA_GROW_TYPE => {
                let completion = validate_primary_bootstrap_uma_completion(pdev, &record)?;
                manager
                    .match_render_usc_freelist_completion(completion)
                    .map_err(|error| {
                        dev_err!(
                            pdev.as_ref(),
                            "G17P UMA grow: type13 has no matching live FList owner ({:?})\n",
                            error
                        );
                        error
                    })?;
                dev_info!(
                    pdev.as_ref(),
                    "G17P UMA grow: matched retained FList owner stamp={} ID={} cookie={:#x} count={:#x}; retaining TA/3D references until paired completion\n",
                    completion.stamp,
                    completion.hardware_buffer_id,
                    completion.cookie,
                    completion.count,
                );
            }
            g17_completion::G17P_FIRMWARE_EVENT_COMPLETION_TYPE => {
                // Type 1 is published exactly once per executed submission, but
                // only its first 0x18 bytes are real: everything beyond that is
                // the 8-byte repeating poison "RTKSTACK". An earlier reading of
                // uuid/context/stamp at +0x28..+0x40 was KTrace spill into the
                // same pages -- with KTrace disabled those offsets are pure
                // poison, which is how the misread was caught. Log the real
                // prefix only; do NOT synthesise a completion from bytes whose
                // encoding is not yet established.
                let qword = |offset: usize| {
                    u64::from_le_bytes(record.raw[offset..offset + 8].try_into().unwrap_or([0; 8]))
                };
                if crate::util::g17p_verbose() {
                    dev_info!(
                        pdev.as_ref(),
                        "G17P completion record: head={:#x} {:#x} {:#x} cursor={}->{} producer={}\n",
                        qword(0x00),
                        qword(0x08),
                        qword(0x10),
                        record.consumer_before,
                        record.consumer_published,
                        record.producer_snapshot
                    );
                }
                batch.completion_records = batch.completion_records.saturating_add(1);
                let event = g17_completion::decode_g17p_firmware_stamp_event(&record.raw)
                    .map_err(|_| EIO)?;
                batch.completion_masks[0] |= event.masks[0];
                batch.completion_masks[1] |= event.masks[1];
            }
            unknown => {
                // Only recovery (type 4) and UMA-grow (type 13) were ever
                // reachable while the firmware never executed work. Now that a
                // kick actually runs, the firmware publishes further record
                // kinds here; rejecting them with EIO aborted the whole drain
                // and left the submission unretired. Log the record and keep
                // draining -- `drain_primary_firmware_event` has already
                // published the consumer cursor for it.
                batch.unknown_records = batch.unknown_records.saturating_add(1);
                if batch.unknown_records <= 4 {
                    dev_info!(
                        pdev.as_ref(),
                        "G17P event worker: unhandled firmware event type={} cursor={}->{} producer={} raw={:02x?}\n",
                        unknown,
                        record.consumer_before,
                        record.consumer_published,
                        record.producer_snapshot,
                        &record.raw[..32.min(record.raw.len())]
                    );
                }
            }
        }
    }
    if batch.unknown_records != 0 {
        dev_info!(
            pdev.as_ref(),
            "G17P event worker: {} unhandled firmware event record(s) this batch\n",
            batch.unknown_records
        );
    }
    if batch.recovery_records != 0 || batch.stale_recovery_records != 0 {
        dev_info!(
            pdev.as_ref(),
            "G17P event worker: notifications={} type4={} stale-type4={} generation={:?}\n",
            batch.notification_count,
            batch.recovery_records,
            batch.stale_recovery_records,
            batch.recovery_generation,
        );
    }
    Ok(batch)
}

/// Service one coalesced firmware-initiated recovery before Linux has
/// published user work. Native increments its host generation once for the
/// scheduled restart, even if the event-ring batch contained duplicates.
fn complete_primary_real_recovery(
    pdev: &platform::Device,
    manager: &mut g17_manager::G17PManagerConstruction,
    async_compute_queues: &KVec<G17PAsyncComputeQueue>,
    registers: Option<&regs::Resources>,
    state: &G17PRoleRtkitState,
    generation: u64,
    duplicate_records: u32,
) -> Result {
    let handshake = manager.firmware_recovery_handshake_snapshot()?;
    if handshake.state != 1 {
        dev_err!(
            pdev.as_ref(),
            "G17P recovery: type-4 generation={} arrived at shared epoch={} state={}, expected state 1\n",
            generation,
            handshake.epoch,
            handshake.state
        );
        return Err(EIO);
    }
    manager.validate_empty_primary_firmware_recovery(generation)?;
    // The scheduler-empty precondition is UNSATISFIABLE for a render-blamed
    // recovery. The firmware raises the restart request during the render
    // wait, so `render_storage`/`render_tracker` are populated by construction
    // and this returns EBUSY every time -- measured, after the inline drain
    // started reaching it. The guard was written when only compute could
    // trigger a recovery, and for compute the scheduler genuinely was empty.
    //
    // The precedent for relaxing it is a few lines below, in this same
    // function: the non-empty recovery-info table used to abort here too,
    // until it was understood that "refusing to clear is strictly worse than
    // clearing" -- refusing leaves the firmware pinned waiting for the host
    // forever. The same argument holds exactly: never answering leaves KSM
    // halted, which is the ETIMEDOUT.
    //
    // What the guard actually protects is the generation-0 SKSM timestamp
    // wipe, which would erase a live render's progress markers. So scope the
    // refusal to that wipe rather than to the whole handshake: acknowledge and
    // clear regardless, skip the wipe while render state is live.
    let scheduler_active = manager.render_scheduler_active();
    if scheduler_active {
        if *module_parameters::g17p_recovery_allow_active_scheduler.value() == 0 {
            manager.validate_empty_primary_firmware_recovery_scheduler()?;
        }
        dev_warn!(
            pdev.as_ref(),
            "G17P recovery: servicing generation={} with a LIVE render scheduler; the generation-0 SKSM timestamp wipe is skipped so the render's progress markers survive\n",
            generation
        );
    }
    dev_info!(
        pdev.as_ref(),
        "G17P recovery: servicing generation={} coalesced-records={} shared-epoch={} state={}\n",
        generation,
        duplicate_records,
        handshake.epoch,
        handshake.state
    );
    // The firmware's own fault report is bundle view 3 -- HOST memory we
    // already own -- and it is complete before the firmware writes 1 into
    // status_b[0x4900], i.e. before this ack. Reading it here costs nothing
    // and names the recovery's reason: 3 is the GMMU page fault (b1 0x5f1c),
    // 2 and 4 are the other two arms. Every prior sample was taken at
    // pre-0x0a/post-0x10, both strictly BEFORE the fault existed, which is
    // why every report so far read reason=0.
    //
    // The faulting VA is not copied into any host-DRAM recovery structure.
    // macOS reads it from the requestor-selected 0xd8c0/0xd8c8 window while
    // firmware is halted at state 1, before the host's 1 -> 2 acknowledge.
    // Match that exact window below, guarded by the always-on GPC state.
    log_g17p_fault_report(pdev.as_ref(), manager, "recovery-blamed-cause");
    probe_g17p_recovery_report_history(pdev.as_ref(), manager);
    log_g17p_halted_slot0(pdev.as_ref(), manager, registers, handshake.epoch);
    log_g17p_halted_gmmu(
        pdev.as_ref(),
        manager,
        async_compute_queues,
        registers,
        handshake.epoch,
    );
    let cause = manager.primary_firmware_recovery_cause_snapshot()?;
    let slot_mask_population = cause.slot_mask.count_ones();
    let lowest_blamed_slot = match cause.slot_mask {
        0 => None,
        mask => Some(mask.trailing_zeros()),
    };
    dev_info!(
        pdev.as_ref(),
        "G17P recovery cause: zero0={:#x} detected-source-count={} zero1={:#x} command-word={:#x} raw48cc={:#x} raw48d0={:#x} dm-slot-mask={:#x} (slots={} lowest={:?}) host-recovery={}\n",
        cause.zero_0,
        cause.detected_source_count,
        cause.zero_1,
        cause.command_word,
        cause.raw_48cc,
        cause.raw_48d0,
        cause.slot_mask,
        slot_mask_population,
        lowest_blamed_slot,
        cause.host_recovery
    );
    // `detected-source-count` must equal the mask population and the blamed
    // slot must be its lowest set bit. When they disagree the mask is not the
    // whole story for this fault, which is worth one line to know.
    if slot_mask_population != cause.detected_source_count {
        dev_warn!(
            pdev.as_ref(),
            "G17P recovery cause: source count {} does not match dm-slot-mask population {} ({:#x})\n",
            cause.detected_source_count,
            slot_mask_population,
            cause.slot_mask,
        );
    }
    if cause.slot_mask != 0 && *module_parameters::g17p_fault_report.value() != 0 {
        let mut faulting_address = None;
        if let Some(registers) = registers {
            match registers.g17p_render_fault_irq_snapshot() {
                Ok(Some(snapshot)) => {
                    if snapshot.fault_info & 1 != 0 {
                        faulting_address = Some((
                            snapshot.fault_addr_word,
                            ((snapshot.fault_info >> 17) & 0x3f) as u8,
                        ));
                    }
                    dev_info!(
                    pdev.as_ref(),
                    "G17P fault IRQ @recovery-state-1: gpc-state={:#x} info={:#018x} addr-word={:#018x} gate={:#018x} sub-status={:#018x} irq-status={:#018x} bit35={}\n",
                    snapshot.gpc_state,
                    snapshot.fault_info,
                    snapshot.fault_addr_word,
                    snapshot.requestor_gate,
                    snapshot.sub_status,
                    snapshot.irq_status,
                    (snapshot.irq_status >> 35) & 1,
                    )
                }
                Ok(None) => dev_warn!(
                    pdev.as_ref(),
                    "G17P fault IRQ @recovery-state-1: skipped because gpc-state=0\n"
                ),
                Err(error) => dev_warn!(
                    pdev.as_ref(),
                    "G17P fault IRQ @recovery-state-1: snapshot failed ({:?})\n",
                    error,
                ),
            }
            if let Err(error) =
                registers.g17p_log_fault_requestors_pre_ack("recovery-state-1")
            {
                dev_warn!(
                    pdev.as_ref(),
                    "G17P recovery: pre-ack fault-requestor sweep failed ({:?})\n",
                    error
                );
            }
        }
        if let Some((addr_word, vm_slot)) = faulting_address {
            log_g17p_fault_address_candidates(
                pdev.as_ref(),
                manager,
                async_compute_queues,
                "recovery-state-1",
                addr_word,
                vm_slot,
            );
        }
    }
    if scheduler_active {
        manager.log_render_recovery_pre_ack("recovery-state-1");
    }
    if cause.host_recovery != 0 {
        dev_err!(
            pdev.as_ref(),
            "G17P recovery: event-backed path has host-recovery flag {}\n",
            cause.host_recovery
        );
        return Err(EIO);
    }
    let accepted_generation = manager.accept_empty_primary_firmware_recovery(generation)?;
    dev_info!(
        pdev.as_ref(),
        "G17P recovery: accepted host generation {} before empty scheduler recovery\n",
        accepted_generation
    );

    // Only the first recovery of a session may wipe the SKSM last-submitted
    // timestamps. Doing it on every recovery erases the queue's progress and
    // makes the firmware immediately request another one: observed on J700 as
    // 105 identical fault-free recoveries (cause word 0x3, no fault, no
    // detected source) inside a single 625ms submit window.
    if generation == 0 && !scheduler_active {
        if let Err(error) = manager.dump_primary_recovery_cause_block() {
            dev_warn!(pdev.as_ref(), "G17P recovery: cause-block dump failed ({:?})\n", error);
        }
        manager.reset_empty_sksm_last_submitted_hw_timestamps()?;
        dev_info!(
            pdev.as_ref(),
            "G17P recovery: published 128 invalid/zero last-submitted HW timestamps\n"
        );
    }
    // Full binary observation while the validated state-1 halt still holds.
    // The archive owns copies; exposing it does not expose mutable GPU memory.
    finish_g17p_render_trace(pdev.as_ref(), manager, crate::g17_trace_capture::Phase::RecoveryBeforeAck);
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
    // A non-empty recovery-info table used to abort here, back when only the
    // empty prepublication case was understood. That left the firmware pinned
    // at state 3 ("waiting for final clear") forever, because the final
    // acknowledge below is what clears it. On J700 the firmware emits exactly
    // one valid entry on the first real submission, with no fault recorded
    // (fault-status 0, host-recovery 0), so refusing to clear is strictly
    // worse than clearing: log the table and complete the handshake.
    if recovery_info.emitted_entry_count != 0 || recovery_info.valid_entry_count != 0 {
        dev_warn!(
            pdev.as_ref(),
            "G17P recovery: completing final clear with a non-empty recovery-info table (emitted={} valid={} first-index={:?} first-flags={:#x})\n",
            recovery_info.emitted_entry_count,
            recovery_info.valid_entry_count,
            recovery_info.first_valid_index,
            recovery_info.first_valid_flags
        );
        // The generation-0 dump above runs BEFORE the firmware reaches state
        // 3, so it always caught this table empty. Re-dump it now that the
        // firmware has published: each entry is 8 bytes and the driver's
        // snapshot only decodes the first word, so the second word of entry 0
        // -- the half that plausibly names what was blamed -- has never been
        // looked at. Host memory only; no MMIO.
        if let Err(error) = manager.dump_primary_recovery_cause_block() {
            dev_warn!(
                pdev.as_ref(),
                "G17P recovery: populated cause-block dump failed ({:?})\n",
                error
            );
        }
    }
    manager.acknowledge_primary_firmware_recovery(3, 0)?;
    let completed = manager.firmware_recovery_handshake_snapshot()?;
    dev_info!(
        pdev.as_ref(),
        "G17P recovery: final clear acknowledged; state={} epoch={}\n",
        completed.state,
        completed.epoch
    );
    if completed.state != 0 {
        return Err(EIO);
    }
    Ok(())
}

static INLINE_RECOVERIES_SERVICED: AtomicU32 = AtomicU32::new(0);

/// Offer only a sealed first-render archive through the established coredump
/// mechanism. Existing coredumps can occupy the device slot; the host must
/// verify the G17TRC01 magic rather than assume every devcd contains this data.
fn finish_g17p_render_trace(
    dev: &kernel::device::Device,
    manager: &mut g17_manager::G17PManagerConstruction,
    phase: crate::g17_trace_capture::Phase,
) {
    #[cfg(CONFIG_DEV_COREDUMP)]
    match manager.finish_render_trace(phase) {
        Ok(Some(capture)) => {
            let size = capture.len();
            devcoredump::dev_coredump(dev, &crate::THIS_MODULE, capture, GFP_KERNEL,
                msecs_to_jiffies(60 * 60 * 1000));
            dev_info!(dev, "G17P binary capture: sealed {} bytes phase={:?}; offered to devcoredump (G17TRC01)\n", size, phase);
        }
        Ok(None) => {}
        Err(error) => dev_warn!(dev, "G17P binary capture finalization failed: {:?}\n", error),
    }
}

fn settle_primary_empty_recovery(
    pdev: &platform::Device,
    manager: &mut g17_manager::G17PManagerConstruction,
    state: &G17PRoleRtkitState,
    signal: &G17PCompletionSignal,
    announce_device_control: &mut impl FnMut() -> Result,
) -> Result<(Option<g17_manager::G17PBootstrapUmaRelease>, [u64; 2])> {
    let mut uma_release = None;
    let mut completion_masks = [0u64; 2];
    for _ in 0..g17_completion::G17P_FIRMWARE_EVENT_CAPACITY {
        let batch = drain_primary_firmware_event_batch(pdev, manager, state, &signal, announce_device_control)?;
        completion_masks[0] |= batch.completion_masks[0];
        completion_masks[1] |= batch.completion_masks[1];
        if let Some(release) = batch.uma_release {
            if uma_release.replace(release).is_some() {
                return Err(EIO);
            }
        }
        if let Some(generation) = batch.recovery_generation {
            // Coalesce rather than refuse: one firmware restart request can be
            // observed more than once (a second drain pass here, or the gfx1
            // ring in the same worker pass), and the caller turns the bare
            // EBUSY into a permanently crashed session.
            manager.queue_or_coalesce_primary_recovery(
                generation,
                batch.recovery_records,
                batch.blamed_stamp,
            )?;
            return Ok((uma_release, completion_masks));
        }
        let handshake = manager.firmware_recovery_handshake_snapshot()?;
        if handshake.state == 0 {
            return Ok((uma_release, completion_masks));
        }
        if handshake.state == 1 {
            // Firmware publishes type4 before state 1, but the producer can
            // race this snapshot. Reload without running recovery inline.
            continue;
        }
        dev_err!(
            pdev.as_ref(),
            "G17P event worker: shared epoch={} state={} has no schedulable type-4 record\n",
            handshake.epoch,
            handshake.state
        );
        return Err(EIO);
    }
    Err(EIO)
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
    manager.publish_partial_opening_operand_table()?;
    dev_info!(
        pdev.as_ref(),
        "G17P compute control: 28-entry partial operand table published\n"
    );

    manager.split_compute_bootstrap_render_contexts()?;
    dev_info!(
        pdev.as_ref(),
        "G17P roots: opening retired; split to context0/render before work\n"
    );

    // Restore and publish 3D first, then restore and publish TA. End with the
    // barrier required before the one paired channel-0 bell.
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
        "G17P compute control: channel-0 startup work sent\n"
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

    // The output-positive paths never drain a runtime control tail here.
    // A runtime 0x2e tick is staged immediately before a work kick, without
    // any doorbell, and firmware consumes it together with that kick
    // ("pending for next work kick"). The proxy hardware experiments proved
    // the co-processor parks after the startup group (CPU_STATUS reads
    // idle) and 0x84 announcements alone never wake it, which is exactly
    // the [4, 4, 5] stall this synchronous drain produced. The 65-record
    // tail in the native capture is the accumulated history of ticks
    // coupled to 64 workloads, not a boot-time prerequisite. Tick zero is
    // staged by the manager at the first compute submission instead.
    dev_info!(
        pdev.as_ref(),
        "G17P compute control: runtime tick deferred to first work kick\n"
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

#[inline(never)]
fn boot_initial_session(
    pdev: &platform::Device,
    drm: &ARef<AsahiDevice>,
    registers: &regs::Resources,
    soc: &'static hw::agx3::SocConfig,
    num_clusters: u32,
    test_stage: G17PTestStage,
) -> Result<(KBox<g17_manager::G17PManagerConstruction>, G17PBootedSession)> {
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
    manager: Option<KBox<g17_manager::G17PManagerConstruction>>,
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
    /// Close every path which can enqueue device-owning firmware work.
    /// Platform unbind calls this before synchronously cancelling the work
    /// items; runtime Drop then performs the firmware-visible teardown.
    pub(crate) fn disarm_firmware_event_worker(&mut self) {
        self.accepting_submissions = false;
        if let Some(state) = self.gfx_state.as_ref() {
            state.event_worker_armed.store(false, Ordering::Release);
        }
        if let Some(state) = self.gfx1_state.as_ref() {
            state.event_worker_armed.store(false, Ordering::Release);
        }
    }

    pub(crate) fn arm_primary_firmware_event_worker(&self) -> Result {
        let state = self.gfx_state.as_ref().ok_or(ENODEV)?;
        state.event_worker_armed.store(true, Ordering::Release);
        // Arm the secondary role too. `recv_message` gates queueing the worker
        // on the flag of whichever role received the notification, and the
        // firmware raises compute completions on GFX1 as well as GFX -- arming
        // only the primary silently dropped every GFX1 notification, which with
        // the doorbell-driven completion path would mean the waiter is never
        // woken at all.
        if let Some(secondary) = self.gfx1_state.as_ref() {
            secondary.event_worker_armed.store(true, Ordering::Release);
        }
        if self.primary_firmware_event_pending()
        {
            crate::driver::queue_g17p_firmware_event_worker(state.dev.clone());
        }
        Ok(())
    }

    /// The interrupt-driven completion handoff for this runtime.
    ///
    /// Cloned out before the caller drops the runtime mutex, so the wait can
    /// happen with that mutex released.
    pub(crate) fn completion_signal(&self) -> Arc<G17PCompletionSignal> {
        self.completion_signal.clone()
    }

    pub(crate) fn primary_firmware_event_pending(&self) -> bool {
        if self.is_crashed() {
            return false;
        }
        self.gfx_state.as_ref().is_some_and(|state| {
            state.firmware_event_notifications.load(Ordering::Acquire) != 0
        }) || self.gfx1_state.as_ref().is_some_and(|state| {
            state.firmware_event_notifications.load(Ordering::Acquire) != 0
        }) || self.queue_setup_pending
    }

    pub(crate) fn primary_recovery_pending(&self) -> bool {
        self.manager
            .as_ref()
            .is_some_and(|manager| manager.pending_primary_recovery().is_some())
    }

    /// Acquire this pair's FList lease before any render doorbell. Only a
    /// pending cold grow publishes 0x20 and waits for type 13; retained pairs
    /// still acquire command references but return ready without a grow.
    fn publish_render_usc_freelist_request(&mut self) -> Result {
        if *module_parameters::g17p_render_usc_freelist.value() == 0 {
            return Ok(());
        }
        let Some(publication) = self
            .manager
            .as_mut()
            .ok_or(ENODEV)?
            .publish_render_usc_freelist_request()?
        else {
            return Ok(());
        };
        fence(Ordering::SeqCst);
        let announcement = g17_rtkit::encode_primary_device_control();
        if crate::util::g17p_verbose() {
            dev_info!(
                self.pdev.as_ref().ok_or(ENODEV)?.as_ref(),
                "G17P render USC FList: staged opcode={:#x} stamp={} slot={} record={:#x} producer {}->{} consumer={}; announcing {:#x}\n",
                publication.opcode,
                publication.arg,
                publication.slot,
                publication.record_address,
                publication.producer_before,
                publication.producer_after,
                publication.consumer_before,
                announcement,
            );
        }
        Pin::new(self.gfx_rtkit.as_mut().ok_or(ENODEV)?)
            .send_message(g17_rtkit::ENDPOINT_GFX_INTERRUPTS, announcement)?;

        // Native's first work snapshot has the opcode-0x20 record fully
        // consumed before the qid0/qid1 doorbell: ch12 is [2,2,2]. The
        // control reply and its asynchronous grow completion are distinct.
        let target = publication.producer_after;
        let mut counters = self.manager.as_mut().ok_or(ENODEV)?.control_counters()?;
        let mut control_retired = false;
        for poll in 0..CONTROL_COUNTER_POLLS {
            if counters.primary == [target; 3] {
                if crate::util::g17p_verbose() {
                    dev_info!(
                        self.pdev.as_ref().ok_or(ENODEV)?.as_ref(),
                        "G17P render USC FList: control record retired before work after {}ms counters={:?}\n",
                        poll,
                        counters.primary,
                    );
                }
                control_retired = true;
                break;
            }
            if self.is_crashed() {
                return Err(EIO);
            }
            fsleep(Delta::from_millis(1));
            counters = self.manager.as_mut().ok_or(ENODEV)?.control_counters()?;
        }
        if !control_retired {
            dev_err!(
                self.pdev.as_ref().ok_or(ENODEV)?.as_ref(),
                "G17P render USC FList: control retirement timed out target={} counters={:?}\n",
                target,
                counters.primary,
            );
            return Err(ETIMEDOUT);
        }

        // B1 marks c083 +0x44 busy while opcode 0x20 drives UMA. Registering
        // render qids against that busy owner suspends them with reason 4;
        // the verbose firmware trace shows the later type-13 event resuming
        // qids0/1 immediately before qid1 is NOP-retired. Service that event
        // now, while this thread owns the runtime mutex, and do not publish a
        // render doorbell until the retained lifecycle reaches Matched.
        const UMA_GROW_POLLS: usize = 200;
        let mut serviced = 0u32;
        for poll in 0..UMA_GROW_POLLS {
            if self
                .manager
                .as_ref()
                .is_some_and(|manager| manager.render_usc_freelist_grow_matched())
            {
                if crate::util::g17p_verbose() {
                    dev_info!(
                        self.pdev.as_ref().ok_or(ENODEV)?.as_ref(),
                        "G17P render USC FList: type13 grow completed before work after {}ms serviced={}\n",
                        poll,
                        serviced,
                    );
                }
                return Ok(());
            }
            if self.is_crashed() {
                return Err(EIO);
            }
            serviced = serviced.saturating_add(self.service_firmware_events_inline()?);
            fsleep(Delta::from_millis(1));
        }
        dev_err!(
            self.pdev.as_ref().ok_or(ENODEV)?.as_ref(),
            "G17P render USC FList: type13 grow did not complete before work (serviced={})\n",
            serviced,
        );
        Err(ETIMEDOUT)
    }

    /// Drop the final host reference named by a matched type-13 event. Native
    /// emits this exact 0x2e control asynchronously from releaseResources.
    fn service_render_usc_freelist_release(
        &mut self,
        release: g17_manager::G17PBootstrapUmaRelease,
    ) -> Result {
        let publication = self
            .manager
            .as_mut()
            .ok_or(ENODEV)?
            .publish_render_usc_freelist_release(release)?;
        fence(Ordering::SeqCst);
        let announcement = g17_rtkit::encode_primary_device_control();
        if crate::util::g17p_verbose() {
            dev_info!(
                self.pdev.as_ref().ok_or(ENODEV)?.as_ref(),
                "G17P render USC FList: staged release opcode={:#x} sequence={:#x} ID={} slot={} producer {}->{}; announcing {:#x}\n",
                publication.opcode,
                release.sequence,
                release.hardware_buffer_id,
                publication.slot,
                publication.producer_before,
                publication.producer_after,
                announcement,
            );
        }
        Pin::new(self.gfx_rtkit.as_mut().ok_or(ENODEV)?)
            .send_message(g17_rtkit::ENDPOINT_GFX_INTERRUPTS, announcement)?;
        self.manager
            .as_mut()
            .ok_or(ENODEV)?
            .mark_render_usc_freelist_release_sent(publication.producer_after)?;

        // Observation is bounded and non-gating, matching native's async
        // release. If firmware has not consumed it yet the lifecycle remains
        // retained in Sent state for exact settlement at the next admission
        // (or the stopped-processor teardown boundary).
        for _ in 0..20 {
            fence(Ordering::SeqCst);
            if self
                .manager
                .as_mut()
                .ok_or(ENODEV)?
                .complete_render_usc_freelist_release_if_consumed(
                    publication.producer_after,
                )?
            {
                if crate::util::g17p_verbose() {
                    dev_info!(
                        self.pdev.as_ref().ok_or(ENODEV)?.as_ref(),
                        "G17P render USC FList: release consumed at producer {}\n",
                        publication.producer_after,
                    );
                }
                return Ok(());
            }
            fsleep(Delta::from_micros(50));
        }
        dev_warn!(
            self.pdev.as_ref().ok_or(ENODEV)?.as_ref(),
            "G17P render USC FList: release remains asynchronous at producer {}; retaining owner for later settlement\n",
            publication.producer_after,
        );
        Ok(())
    }

    /// A release that missed the short completion-path observation window is
    /// ordinary control-ring backpressure, not a corrupt retained owner. Wait
    /// for the exact published producer before acquiring the next command's
    /// FList references. The same bounded 100ms retirement contract is used by
    /// the synchronous opcode-0x20 path above.
    fn settle_render_usc_freelist_release_before_submission(&mut self) -> Result {
        let Some(target) = self
            .manager
            .as_ref()
            .and_then(|manager| manager.pending_render_usc_freelist_release_producer())
        else {
            return Ok(());
        };

        for poll in 0..CONTROL_COUNTER_POLLS {
            fence(Ordering::SeqCst);
            if self
                .manager
                .as_mut()
                .ok_or(ENODEV)?
                .complete_render_usc_freelist_release_if_consumed(target)?
            {
                if crate::util::g17p_verbose() {
                    dev_info!(
                        self.pdev.as_ref().ok_or(ENODEV)?.as_ref(),
                        "G17P render USC FList: settled pending release producer {} before next submission after {}ms\n",
                        target,
                        poll,
                    );
                }
                return Ok(());
            }
            if self.is_crashed() {
                return Err(EIO);
            }
            fsleep(Delta::from_millis(1));
        }

        dev_err!(
            self.pdev.as_ref().ok_or(ENODEV)?.as_ref(),
            "G17P render USC FList: pending release producer {} did not retire before next submission\n",
            target,
        );
        Err(ETIMEDOUT)
    }

    pub(crate) fn service_primary_firmware_events(&mut self) -> Result {
        if self.is_crashed() {
            return Ok(());
        }
        let pdev = self.pdev.as_ref().cloned().ok_or(ENODEV)?;
        let state = self.gfx_state.as_ref().cloned().ok_or(ENODEV)?;
        let signal = self.completion_signal.clone();
        if let Some(manager) = self.manager.as_mut() {
            // The doorbell that woke this worker is also the signal that a
            // submission may have retired; publish it to any waiter.
            if let Ok(Some(timestamps)) = manager.read_compute_completion() {
                signal.record(G17PComputeCompletion { timestamps });
            }
        }
        // Both RTKit roles notify the shared completion/event resources.
        // Consume GFX1 ingress too; otherwise a coalesced secondary-only edge
        // disappears when WorkItem1 is already running and no primary pending
        // count asks it to run again.
        let secondary_notifications = self.gfx1_state.as_ref().map_or(0, |secondary| {
            secondary.firmware_event_notifications.load(Ordering::Acquire)
        });
        let notifications_pending = secondary_notifications != 0 ||
            state.firmware_event_notifications.load(Ordering::Acquire) != 0;
        let recovery_pending = self
            .manager
            .as_ref()
            .ok_or(ENODEV)?
            .pending_primary_recovery()
            .is_some();
        let mut uma_release = None;
        let mut completion_masks = [0u64; 2];
        // Every step below can end the session, and they all surface the same
        // handful of errnos, so carry the step's name alongside the error. The
        // crashed line is the last thing this role ever logs; an unattributed
        // EIO there is not enough to tell eight different failures apart.
        let step = |name: &'static str| move |error: Error| (name, error);
        let result: core::result::Result<(), (&'static str, Error)> = if recovery_pending {
            Ok(())
        } else if self.queue_setup_pending && !notifications_pending {
            let result = self.ensure_compute_queue();
            dev_info!(
                pdev.as_ref(),
                "G17P session: deferred SKSM QID 4 / selector 2 memory prepared after recovery-gate work; submissions={}\n",
                self.accepting_submissions
            );
            result.map_err(step("ensure-compute-queue"))
        } else {
            // Not inside the labelled chain yet: this `?` leaves the function.
            let manager = self.manager.as_mut().ok_or(ENODEV)?;
            let rtkit = self.gfx_rtkit.as_mut().ok_or(ENODEV)?;
            let mut announce = || Pin::new(&mut *rtkit).send_message(
                g17_rtkit::ENDPOINT_GFX_INTERRUPTS, g17_rtkit::encode_primary_device_control());
            match settle_primary_empty_recovery(&pdev, manager, &state, &signal, &mut announce) {
                Ok((release, masks)) => {
                    uma_release = release;
                    completion_masks = masks;
                    Ok(())
                }
                Err(error) => Err(("settle-primary-empty-recovery", error)),
            }
        };
        let result = result.and_then(|()| {
            if let Some(release) = uma_release {
                self.service_render_usc_freelist_release(release)
                    .map_err(step("render-usc-freelist-release"))
            } else {
                Ok(())
            }
        });
        let result = result.and_then(|()| {
            if let Some(secondary) = self.gfx1_state.as_ref().cloned() {
                let rtkit = self.gfx_rtkit.as_mut().ok_or(("no-gfx-rtkit", ENODEV))?;
                let mut announce = || Pin::new(&mut *rtkit).send_message(
                    g17_rtkit::ENDPOINT_GFX_INTERRUPTS, g17_rtkit::encode_primary_device_control());
                let batch = drain_primary_firmware_event_batch(&pdev,
                    self.manager.as_mut().ok_or(("no-manager", ENODEV))?, &secondary, &signal,
                    &mut announce)
                    .map_err(step("drain-gfx1-event-ring"))?;
                completion_masks[0] |= batch.completion_masks[0];
                completion_masks[1] |= batch.completion_masks[1];
                if let Some(release) = batch.uma_release {
                    self.service_render_usc_freelist_release(release)
                        .map_err(step("gfx1-render-usc-freelist-release"))?;
                }
                if let Some(generation) = batch.recovery_generation {
                    // The firmware raises ONE restart request and both roles can
                    // see it, so this is routinely the same request the gfx ring
                    // queued a few lines ago. `queue_primary_recovery` answers
                    // that with EBUSY, and EBUSY here killed the session.
                    let queued = self.manager.as_mut().ok_or(("no-manager", ENODEV))?
                        .queue_or_coalesce_primary_recovery(
                            generation,
                            batch.recovery_records,
                            batch.blamed_stamp,
                        )
                        .map_err(step("queue-gfx1-recovery"))?;
                    if !queued {
                        dev_info!(
                            pdev.as_ref(),
                            "G17P recovery: gfx1 type4 generation={} records={} folded into the request the gfx ring already queued\n",
                            generation,
                            batch.recovery_records,
                        );
                    }
                }
            }
            self.service_async_render_completions(completion_masks)
                .map_err(step("async-render-completions"))?;
            self.service_async_compute_completion_masks(completion_masks)
                .map_err(step("async-compute-completions"))
        });
        match result {
            Ok(()) => Ok(()),
            Err((reason, error)) => {
                note_state_crashed(&pdev, &state, reason, error, None);
                Err(error)
            }
        }
    }

    /// Service firmware events inline while a submission is in flight.
    ///
    /// The submit path holds the runtime mutex for the whole wait, so the
    /// workqueue event worker cannot acquire it until submit returns. The
    /// firmware's handshake (`state == 1`, "waiting for first response",
    /// raised together with the GPC power-up) therefore never gets an answer
    /// inside the submit window -- observed on J700 as the worker running
    /// 1.6s late, after the 625ms timeout had already expired. Drain and
    /// service here, where the lock is already held.
    fn service_firmware_events_inline(&mut self) -> Result<u32> {
        let pdev = self.pdev.as_ref().cloned().ok_or(ENODEV)?;
        let state = self.gfx_state.as_ref().cloned().ok_or(ENODEV)?;
        if state.firmware_event_notifications.load(Ordering::Acquire) == 0 {
            return Ok(0);
        }
        let signal = self.completion_signal.clone();
        let batch = {
            let manager = self.manager.as_mut().ok_or(ENODEV)?;
            let rtkit = self.gfx_rtkit.as_mut().ok_or(ENODEV)?;
            let mut announce = || Pin::new(&mut *rtkit).send_message(
                g17_rtkit::ENDPOINT_GFX_INTERRUPTS, g17_rtkit::encode_primary_device_control());
            drain_primary_firmware_event_batch(&pdev, manager, &state, &signal, &mut announce)?
        };
        let completion_masks = batch.completion_masks;
        for slot in 0..g17_resources::G17PRenderQueuePair::SLOT_COUNT as usize {
            if self.pending_render[slot].is_some() {
                self.render_event_masks[slot][0] |= completion_masks[0];
                self.render_event_masks[slot][1] |= completion_masks[1];
            }
        }
        // This path owns the runtime mutex and has consumed the primary event
        // records plus their notification count. Route compute completions
        // here as well; the ordinary worker cannot rediscover this edge after
        // the inline drain returns. Do this before any other fallible service
        // or diagnostic recovery early-return so a mixed batch loses no QID.
        self.service_async_compute_completion_masks(completion_masks)?;
        if let Some(release) = batch.uma_release {
            self.service_render_usc_freelist_release(release)?;
        }
        if let Some(generation) = batch.recovery_generation {
            // Diagnostic cap: the J700 firmware re-raises a fault-free
            // recovery immediately after every final clear, ~500 times per
            // submit window. Capping lets us tell whether our own servicing
            // sustains the loop or the firmware would raise it regardless.
            let limit = *module_parameters::g17p_max_recoveries.value();
            // 0xffff = service none at all. Discriminates "the firmware halts
            // KSM when it *requests* recovery" from "our answer is what leaves
            // it halted".
            if limit == 0xffff {
                return Ok(batch.notification_count);
            }
            let serviced = INLINE_RECOVERIES_SERVICED.load(Ordering::Relaxed);
            if limit != 0 && serviced >= limit {
                if serviced == limit {
                    INLINE_RECOVERIES_SERVICED.store(limit + 1, Ordering::Relaxed);
                    dev_info!(
                        pdev.as_ref(),
                        "G17P recovery: inline servicing capped at {}; ignoring further requests\n",
                        limit
                    );
                }
                return Ok(batch.notification_count);
            }
            INLINE_RECOVERIES_SERVICED.fetch_add(1, Ordering::Relaxed);
            self.manager
                .as_mut()
                .ok_or(ENODEV)?
                .queue_or_coalesce_primary_recovery(
                    generation,
                    batch.recovery_records,
                    batch.blamed_stamp,
                )?;
            self.service_primary_recovery()?;
        }
        self.service_async_render_completions([0; 2])?;
        Ok(batch.notification_count)
    }

    pub(crate) fn service_primary_recovery(&mut self) -> Result {
        let pdev = self.pdev.as_ref().cloned().ok_or(ENODEV)?;
        let state = self.gfx_state.as_ref().cloned().ok_or(ENODEV)?;
        // Sample the queue/firmware agreement either side of the recovery.
        // A boot with no fault hands the retained graph to any number of
        // sequential clients; one GMMU fault in between and every later kick is
        // accepted, retired and never executed. Whatever moves across this
        // boundary is the mechanism, so measure it rather than infer it. Host
        // DRAM only -- see `log_compute_recovery_alignment`.
        self.log_recovery_alignment("pre-recovery");
        let registers = self.registers.as_ref();
        let async_compute_queues = &self.async_compute_queues;
        let manager = self.manager.as_mut().ok_or(ENODEV)?;
        let Some((generation, records, blamed_stamp)) = manager.pending_primary_recovery() else {
            return Ok(());
        };
        let result = complete_primary_real_recovery(
            &pdev,
            manager,
            async_compute_queues,
            registers,
            &state,
            generation,
            records,
        )
        .map_err(|error| ("complete-real-recovery", error))
        .and_then(|()| {
            manager
                .complete_pending_primary_recovery(generation, records, blamed_stamp)
                .map_err(|error| ("complete-pending-recovery", error))
        });
        let result = match result {
            Ok(()) => Ok(()),
            Err((reason, error)) => {
                // Same permanent-deafness transition as the event worker, and
                // this one also skips the event-drain re-arm at the end of this
                // function.
                note_state_crashed(&pdev, &state, reason, error, Some(generation));
                return Err(error);
            }
        };
        if let Some(stamp) = blamed_stamp {
            self.settle_render_blamed_by_recovery(stamp, EIO);
        }
        self.log_recovery_alignment("post-recovery");
        // Realign before the next kick. Continuity across a recovery is exactly
        // the assumption the evidence contradicts.
        if let (Some(manager), Some(queue)) = (self.manager.as_mut(), self.queue.as_mut()) {
            if let Err(error) = manager.resynchronise_compute_queue_after_recovery(queue) {
                dev_warn!(
                    pdev.as_ref(),
                    "G17P recovery: post-recovery queue resync failed ({:?}); the graph keeps its pre-recovery state\n",
                    error
                );
            }
        }
        // The batch drain deliberately stops at type 4 so restartWorkQueue can
        // release both native command gates before the next record is read.
        // Its notification count was already swapped to zero, however, and a
        // type-1/type-13 record may already follow type 4 in the same producer
        // snapshot.  Schedule one continuation after every successful actual
        // recovery; queue_work coalescing makes a concurrent fresh IRQ share
        // this pass, while an empty ring costs one bounded drain.  Keeping the
        // continuation here covers both the workqueue and inline callers.
        crate::driver::queue_g17p_firmware_event_worker(state.dev.clone());
        result
    }

    /// One recovery-boundary sample of the retained queue, if there is one.
    fn log_recovery_alignment(&mut self, label: &'static str) {
        if let (Some(manager), Some(queue)) = (self.manager.as_mut(), self.queue.as_mut()) {
            manager.log_compute_recovery_alignment(queue, label);
        }
    }

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
        completion: Arc<crate::g17_drm::G17PRenderCompletion>,
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
            .select_render_slot(render_slot)?;
        let drm = self.drm.as_ref().ok_or(ENODEV)?.clone();
        // Temporarily own the manager and register aperture separately so the
        // manager can ask the live runtime to perform ensurePoweredHardware at
        // the exact configureHardware boundary.  The previous preamble did the
        // power transaction before graph construction; verbose construction
        // alone was long enough for firmware to close the aperture again.
        let mut manager = self.manager.take().ok_or(ENODEV)?;
        let registers = self.registers.take().ok_or(ENODEV)?;
        let mut registration_powered = false;
        let prepared = manager.begin_translated_render(
            &drm,
            vm,
            queue_pair,
            execution_context,
            command,
            &registers,
            &mut |manager| {
                registration_powered = false;
                self.ensure_g17p_render_registration_power(manager, &registers)?;
                registration_powered = true;
                Ok(())
            },
        );
        // `ensurePoweredHardware(true)` is a scoped native prerequisite. The
        // IdlePowerOff(0) disables firmware idle descent around the direct
        // KSMFE registration. Restore ordinary idle policy before any work
        // doorbell.
        if registration_powered {
            if let Err(error) = self.release_g17p_render_registration_power(
                &mut manager,
                &registers,
            ) {
                if let Some(pdev) = self.pdev.as_ref() {
                    dev_warn!(
                        pdev.as_ref(),
                        "G17P render registration: failed to restore ordinary idle policy ({:?}); idle descent remains disabled\n",
                        error,
                    );
                }
            }
        }
        self.manager = Some(manager);
        self.registers = Some(registers);
        let prepared = prepared?;
        // Arm the exact fence after the tracker has been reset for this group,
        // but before any control or work doorbell can report its retirement.
        self.install_render_completion(render_slot, completion)?;
        // Copy only before the FList wake and while both outers are deferred.
        // Never insert this scan between producer publication and doorbell.
        if let Some(manager) = self.manager.as_mut() {
            if let Err(error) = manager.capture_prepared_render_trace(
                native_doorbell_mode == 0 && !split_launch,
            ) {
                if let Some(pdev) = self.pdev.as_ref() {
                    dev_warn!(pdev.as_ref(), "G17P prepared binary capture failed: {:?}\n", error);
                }
            }
        }
        // Native retires AGXUSCPrivMemFList::updateSubmitInfo and its type-13
        // grow before exposing either render outer producer. The control-ring
        // notification itself wakes the scheduler, so a merely staged final
        // work doorbell is not a visibility barrier: the old ordering let TA
        // run here against a half-grown FList and emit only an empty header.
        if let Err(error) = self.publish_render_usc_freelist_request() {
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
