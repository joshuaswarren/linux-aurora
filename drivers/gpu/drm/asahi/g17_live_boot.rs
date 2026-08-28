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
//! 4. The pair executor starts GFX then GFX1 and stops GFX1 then GFX on every
//!    exit, including partial acquisition and body errors.
//!
//! Generic RTKit separately exposes a discovery-only HELLO/EPMAP client. It
//! does not auto-start EP 1 (crashlog), allocate buffers, or send AP/IOP power
//! transitions. Wiring two such clients into the pair body remains the next
//! stage.

#![allow(dead_code)]

use kernel::{
    device::Core,
    platform,
    prelude::*,
    soc::apple::mailbox::{AscwrapV6Lifecycle, AscwrapV6Provider, AscwrapV6Role},
};

use crate::{driver::T8140LiveTopologyAdmission, g17_lifecycle, hw, identity};

/// Master opt-in. It remains false while the dual RTKit discovery body is not
/// wired.
pub(crate) const G17_LIVE_BOOT_ENABLED: bool = false;

/// Ordered boundary stages.
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

impl g17_lifecycle::AscCpuLifecycle for AscwrapV6Provider {
    type Error = Error;

    fn start_cpu(&self) -> Result {
        AscwrapV6Provider::start_cpu(self)
    }

    fn stop_cpu(&self) -> Result {
        AscwrapV6Provider::stop_cpu(self)
    }
}

/// Attempt the current G17P live-boot slice.
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

    // Check both roles before either CPU starts.
    let gfx_gate = gfx.require_safe_cpu_lifecycle();
    let gfx1_gate = gfx1.require_safe_cpu_lifecycle();
    let transaction_gate =
        g17_lifecycle::admit_cpu_start(gfx_lifecycle.missing, gfx1_lifecycle.missing);
    if gfx_gate.is_err() || gfx1_gate.is_err() || transaction_gate.is_err() {
        dev_err!(
            pdev.as_ref(),
            "G17P live-boot: {:?} incomplete (GFX {:?}, GFX1 {:?}, transaction {:?})\n",
            BootStage::ReversibleLifecycle,
            gfx_gate.err(),
            gfx1_gate.err(),
            transaction_gate.err()
        );
        return Err(ENOTSUPP);
    }

    g17_lifecycle::execute_cpu_pair(&gfx, &gfx1, || {
        dev_info!(
            pdev.as_ref(),
            "G17P live-boot: {:?} acquired GFX then GFX1\n",
            BootStage::StartCpus
        );
        dev_err!(
            pdev.as_ref(),
            "G17P live-boot: dual RTKit discovery is not wired at {:?}\n",
            BootStage::DiscoverEpmap
        );
        Err(ENOTSUPP)
    })
}
