// SPDX-License-Identifier: GPL-2.0-only OR MIT

//! G17 (Apple M3/M4/M5/A18) live-boot executor — first executable slice.
//!
//! Everything else in the G17 tree is an offline *validator*: it grounds what
//! the firmware expects (ASC power/run plans, init-data graph, RTKit codec,
//! submission records) but never touches silicon. This module is the first
//! piece that actually executes, turning the diagnostic path from "refuse
//! before any MMIO" into "attempt the grounded boot sequence, then fail closed
//! (`ENODEV`) at the first stage that is not yet wired or on a timeout".
//!
//! # Safety / status
//!
//! This has **never run on hardware**. It is gated OFF by
//! [`G17_LIVE_BOOT_ENABLED`] so a normal AGX3 probe still refuses exactly as
//! before. It should be enabled (a one-line flip + rebuild) only on a real M5
//! you intend to boot bare-metal under m1n1/Linux, where a wedged ASC is
//! recoverable by reboot.
//!
//! # Boot sequence ([`BootStage`])
//!
//! The order mirrors the proven AGX2 prologue in [`crate::driver`] and the
//! grounded G17 plans:
//!
//! 1. `DmaMask`   — program the coherent DMA mask from the grounded OAS width.
//! 2. `MapMmio`   — map the SGX/ASC register apertures ([`regs::Resources`]).
//! 3. `InitMmio`  — misc MMIO init.
//! 4. `StartAsc`  — release the ASC coprocessor CPU (`CPU_RUN`), the grounded
//!                  [`g17_asc_bringup::asc_run_cpu_plan`] first live step.
//! 5. `RtkitHandshake` — bring up the shared `apple-rtkit` transport and let
//!                  the firmware advertise its endpoints (HELLO/EPMAP). *Not
//!                  yet wired.*
//! 6. `UatHandoff` — poll the UAT [`Handoff`] Dekker exchange for
//!                  `magic_fw == PPL_MAGIC` with a bounded timeout. *Not yet
//!                  wired: depends on full UAT setup.*
//! 7. `Initdata`  — DMA-upload the grounded init-data graph
//!                  ([`g17_initdata::encode_grounded_subset`]). *Not yet
//!                  wired.*
//! 8. `ChannelsDoorbell` — publish channel rings and ring the EP 0x21 doorbell
//!                  ([`g17_submission`]). *Not yet wired.*
//!
//! Stages 1–4 execute here today; 5–8 return `ENODEV` with a precise stage
//! name so a hardware boot fails legibly rather than silently.

// Scaffold module: stages 5-8 and their helpers (poll_until, later BootStage
// variants, HANDOFF_TIMEOUT_MS) are wired but not yet reached, so dead code is
// allowed unconditionally until the remaining stages land.
#![allow(dead_code)]

use kernel::{
    c_str,
    device::Core,
    dma::{Device, DmaMask},
    platform,
    prelude::*,
    time::{delay::fsleep, Delta, Instant, Monotonic},
};

use crate::{hw, identity, regs};

/// Master opt-in. **Default `false`**: a normal AGX3 probe refuses before any
/// MMIO, unchanged. Set to `true` and rebuild only on a real M5 that you are
/// deliberately booting bare-metal; releasing the ASC CPU on live hardware is
/// not reversible without a reboot.
pub(crate) const G17_LIVE_BOOT_ENABLED: bool = false;

/// Milliseconds to let the released ASC settle before the (not-yet-wired)
/// handoff poll. Grounded ASC run plans report a 100us..250us window; a few
/// milliseconds is a conservative super-set for a first attempt.
const ASC_SETTLE_MS: i64 = 5;

/// Bounded wait for the firmware to answer the UAT handoff, once that stage is
/// wired. The firmware publishes `magic_fw == PPL_MAGIC` within a boot; a live
/// boot that does not answer in this window is a dead ASC.
const HANDOFF_TIMEOUT_MS: i64 = 1000;

/// The ordered boot stages. `Complete` is only reachable once every stage is
/// wired and the firmware has answered the handoff.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) enum BootStage {
    DmaMask,
    MapMmio,
    InitMmio,
    StartAsc,
    RtkitHandshake,
    UatHandoff,
    Initdata,
    ChannelsDoorbell,
    Complete,
}

/// Poll `read` until it returns `true` or `timeout_ms` elapses. Mirrors the
/// two-phase (busy then sleeping) poll used by [`crate::channel`]. Kept ready
/// for the `UatHandoff` stage; the caller supplies the liveness predicate.
fn poll_until(mut ready: impl FnMut() -> bool, timeout_ms: i64) -> Result {
    const MAX_FAST_POLL_MS: i64 = 10;
    let start = Instant::<Monotonic>::now();
    let timeout_ms = timeout_ms.max(1);
    let fast = Delta::from_millis(timeout_ms.min(MAX_FAST_POLL_MS));
    let slow = Delta::from_millis(timeout_ms);
    while start.elapsed() < fast {
        if ready() {
            return Ok(());
        }
    }
    while start.elapsed() < slow {
        if ready() {
            return Ok(());
        }
        fsleep(Delta::from_millis(5));
    }
    Err(ETIMEDOUT)
}

/// Read the GPU performance-state clock column (MHz) from the device tree.
///
/// This is the idiomatic runtime source for the init-data `hw_globals`
/// perf-state table: the same `operating-points-v2` OPP table the AGX2 path
/// reads in `hw::PwrConfig::load_opp`, but self-contained (G17 has no
/// `HwConfig`, so the per-die voltage-count check is deferred to the not-yet
/// wired init-data stage — only the clock column is grounded here). The
/// voltage/power columns and the feature words elsewhere in the 0x2828-byte
/// `hw_globals` struct still require firmware RE, so
/// `g17_initdata::ALL_HW_GLOBALS_VALUES_GROUNDED` remains false.
fn read_perf_state_clocks_mhz(pdev: &platform::Device<Core>) -> Result<KVec<u32>> {
    let node = pdev.as_ref().of_node().ok_or(EIO)?;
    let opps = node
        .parse_phandle(c_str!("operating-points-v2"), 0)
        .ok_or(EIO)?;
    let mut clocks: KVec<u32> = KVec::new();
    for opp in opps.children() {
        let freq_hz: u64 = opp.get_property(c_str!("opp-hz"))?;
        clocks.push((freq_hz / 1_000_000) as u32, GFP_KERNEL)?;
    }
    if clocks.is_empty() {
        return Err(EINVAL);
    }
    Ok(clocks)
}

/// Attempt the grounded G17 boot sequence.
///
/// Returns `Ok(())` only if a full boot is ever wired and the firmware answers.
/// Today it executes stages 1–4 and returns `Err(ENODEV)` at `RtkitHandshake`,
/// the first unwired stage. Every stage is logged so a hardware run is legible.
pub(crate) fn attempt(
    pdev: &platform::Device<Core>,
    soc: &'static hw::agx3::SocConfig,
    id: &identity::GpuIdentity,
) -> Result {
    if !G17_LIVE_BOOT_ENABLED {
        return Err(ENODEV);
    }

    dev_info!(
        pdev.as_ref(),
        "G17 live-boot: attempting grounded prologue for {:?}{:?} (chip {:#x}); UNTESTED on hardware\n",
        id.gpu_gen,
        id.gpu_variant,
        soc.chip_id
    );

    // Stage 1 — DmaMask. Grounded OAS width from the SoC identity.
    // SAFETY: identical to the proven AGX2 boot prologue in `driver::probe`;
    // called once, before any DMA is set up.
    unsafe { pdev.dma_set_mask_and_coherent(DmaMask::try_new(soc.uat_oas)?)? };
    dev_info!(pdev.as_ref(), "G17 live-boot: [1/8] DMA mask set ({}-bit OAS)\n", soc.uat_oas);

    // Stage 2 — MapMmio. Same SGX/ASC aperture map the read-only ID path uses.
    let res = regs::Resources::new(pdev)?;
    dev_info!(pdev.as_ref(), "G17 live-boot: [2/8] SGX/ASC apertures mapped\n");

    // Stage 3 — InitMmio.
    res.init_mmio()?;
    dev_info!(pdev.as_ref(), "G17 live-boot: [3/8] misc MMIO initialized\n");

    // Source the grounded perf-state clock column from the device tree now, so
    // a hardware run confirms the OPP table is present before the ASC starts.
    // The init-data stage (not yet wired) will feed these to
    // `g17_initdata::encode_grounded_subset`.
    match read_perf_state_clocks_mhz(pdev) {
        Ok(clocks) => {
            let lo = clocks.first().copied().unwrap_or(0);
            let hi = clocks.last().copied().unwrap_or(0);
            dev_info!(
                pdev.as_ref(),
                "G17 live-boot: [dt] {} GPU perf-states ({}..{} MHz) sourced from the device tree\n",
                clocks.len(),
                lo,
                hi
            );
        }
        Err(e) => {
            dev_warn!(
                pdev.as_ref(),
                "G17 live-boot: [dt] no GPU operating-points-v2 table ({:?}); the init-data stage will require it\n",
                e
            );
        }
    }

    // Stage 4 — StartAsc. Release the coprocessor CPU so it can begin the
    // firmware boot and, eventually, the UAT handoff. This is the grounded
    // first live step (`g17_asc_bringup::asc_run_cpu_plan`) executed through
    // the proven `start_cpu` primitive.
    //
    // NOTE: A18 Pro / dual-role parts (GFX + GFX1) have two ASC transports;
    // this releases the primary `asc` region only. The second role is a
    // `RtkitHandshake`-stage concern once that stage is wired.
    regs::Resources::start_cpu(pdev)?;
    dev_info!(pdev.as_ref(), "G17 live-boot: [4/8] ASC CPU released (CPU_RUN)\n");
    fsleep(Delta::from_millis(ASC_SETTLE_MS));

    // Stage 5+ — RtkitHandshake / UatHandoff / Initdata / ChannelsDoorbell.
    // These require the shared apple-rtkit client, full UAT setup (for the
    // `Handoff` region and `magic_fw == PPL_MAGIC` poll via `poll_until`), the
    // init-data DMA upload, and channel/doorbell publication. They are
    // grounded offline but not yet executed. Fail closed, legibly.
    dev_err!(
        pdev.as_ref(),
        "G17 live-boot: prologue reached {:?}; RTKit/UAT-handoff/init-data/doorbell stages are not yet wired, stopping cleanly\n",
        BootStage::RtkitHandshake
    );
    Err(ENODEV)
}
