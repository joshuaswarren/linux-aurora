// SPDX-License-Identifier: GPL-2.0-only OR MIT

//! G17P live-boot lifecycle boundary.
//!
//! T8140 has two independent ASCWrap v6 providers. The provider owns the
//! wrapper and IOP VBAR mappings; DRM must never map an `asc1` alias or write
//! `CPU_RUN` directly. A previous stage did exactly that and then freed RTKit
//! transports while both CPUs could still be running.
//!
//! The replacement is deliberately smaller and safe:
//!
//! 1. [`crate::driver`] admits the exact live DT topology before any target
//!    effect and passes an unforgeable [`T8140LiveTopologyAdmission`] token.
//! 2. This module resolves both mailbox providers and verifies their roles and
//!    exact resources through the provider-owned lifecycle surface.
//! 3. Both providers must report a complete, reversible CPU lifecycle before
//!    any DMA setup, SGX write, transport arm, IOP VBAR write, or `CPU_RUN`.
//! 4. Today that gate is closed because safe stop/reset is not implemented.
//!    The attempt therefore fails before CPU start. There is no partial CPU
//!    state to clean up.
//!
//! Generic RTKit separately exposes a discovery-only HELLO/EPMAP client. It
//! does not auto-start EP 1 (crashlog), allocate buffers, or send AP/IOP power
//! transitions. Once provider-owned stop/reset exists, the next stage may arm
//! two such clients, start both CPUs transactionally, validate exact raw
//! `0x0c/0x0c` plus `{0,1,0x20,0x21,0x23}`, and unwind GFX1 before GFX on every
//! error. None of that is claimed executable while the lifecycle gate is
//! closed.

#![allow(dead_code)]

use kernel::{
    device::Core,
    platform,
    prelude::*,
    soc::apple::mailbox::{AscwrapV6Lifecycle, AscwrapV6Provider, AscwrapV6Role},
};

use crate::{driver::T8140LiveTopologyAdmission, g17_lifecycle, hw, identity};

/// Master opt-in. It remains false; even a local true build stops before
/// either CPU while the provider reports missing stop/reset capabilities.
pub(crate) const G17_LIVE_BOOT_ENABLED: bool = false;

/// Ordered boundary stages. `StartCpus` cannot be reached today.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) enum BootStage {
    ExactTopology,
    ProviderOwnership,
    ReversibleLifecycle,
    StartCpus,
    DiscoverEpmap,
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

/// Attempt the first safe G17P live-boot slice.
///
/// This function currently performs provider queries only. It returns
/// `EOPNOTSUPP` before any CPU start because the provider's reversible
/// lifecycle gate is intentionally closed.
pub(crate) fn attempt(
    pdev: &platform::Device<Core>,
    soc: &'static hw::agx3::SocConfig,
    id: &identity::GpuIdentity,
    topology: Option<&T8140LiveTopologyAdmission>,
) -> Result {
    if !G17_LIVE_BOOT_ENABLED {
        return Err(ENODEV);
    }
    if soc.chip_id != 0x8140
        || id.gpu_gen != hw::GpuGen::G17
        || id.gpu_variant != identity::GpuVariant::P
        || topology.is_none()
    {
        return Err(EINVAL);
    }

    dev_info!(
        pdev.as_ref(),
        "G17P live-boot: {:?} admitted before effects; querying provider-owned lifecycle\n",
        BootStage::ExactTopology
    );

    let gfx = AscwrapV6Provider::get(pdev.as_ref(), 0)?;
    let gfx_lifecycle = validate_provider(&gfx, GFX_PROVIDER)?;
    let gfx1 = AscwrapV6Provider::get(pdev.as_ref(), 1)?;
    let gfx1_lifecycle = validate_provider(&gfx1, GFX1_PROVIDER)?;

    dev_info!(
        pdev.as_ref(),
        "G17P live-boot: {:?} admitted for GFX/GFX1; DRM mapped no ASC wrapper\n",
        BootStage::ProviderOwnership
    );

    // Check both roles before any CPU start. There are no live resources to
    // unwind if either check fails. Once this gate can succeed, the executor
    // must acquire transports primary then secondary and explicitly unwind
    // secondary then primary on every subsequent error.
    let gfx_gate = gfx.require_safe_cpu_lifecycle();
    let gfx1_gate = gfx1.require_safe_cpu_lifecycle();
    let transaction_gate =
        g17_lifecycle::admit_cpu_start(gfx_lifecycle.missing, gfx1_lifecycle.missing);
    if gfx_gate.is_err() || gfx1_gate.is_err() || transaction_gate.is_err() {
        dev_err!(
            pdev.as_ref(),
            "G17P live-boot: {:?} blocked (GFX {:?}, GFX1 {:?}, transaction {:?}); \
             refusing before IOP VBAR, CPU_RUN, DMA, SGX writes, or RTKit transport\n",
            BootStage::ReversibleLifecycle,
            gfx_gate.err(),
            gfx1_gate.err(),
            transaction_gate.err()
        );
        return Err(ENOTSUPP);
    }

    // The provider must grow an actual start+reverse-stop transaction before
    // this boundary may move. A capability check opening by itself is not
    // authority to fall back to DRM MMIO.
    dev_err!(
        pdev.as_ref(),
        "G17P live-boot: provider lifecycle gate opened without a transactional executor; \
         refusing at {:?}\n",
        BootStage::StartCpus
    );
    Err(ENOTSUPP)
}
