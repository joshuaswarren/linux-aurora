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

use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};

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
use kernel::macros::vtable;

use crate::{
    driver::{AsahiDevice, T8140LiveTopologyAdmission},
    g17_lifecycle, g17_manager, g17_rtkit, hw, identity, regs,
};

/// The T8140 match now runs the reversible provider-owned bring-up slice.
/// Probe still returns `ENODEV` after the slice because no G17P DRM queue
/// backend is registered yet.
pub(crate) const G17_LIVE_BOOT_ENABLED: bool = true;

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
    RegisterComputeQueue,
}

const INITDATA_ACK_TYPE: u16 = 0x09;
const INITDATA_ACK_POLLS: usize = 200;

/// Per-role callback state retained until the corresponding RTKit client is
/// dropped. The callback only publishes atomics; probe owns every mutation of
/// manager and queue state.
struct G17PRoleRtkitState {
    acked: AtomicBool,
    crashed: AtomicBool,
    runtime_events: AtomicU32,
}

impl G17PRoleRtkitState {
    fn new() -> Self {
        Self {
            acked: AtomicBool::new(false),
            crashed: AtomicBool::new(false),
            runtime_events: AtomicU32::new(0),
        }
    }
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
        if endpoint == g17_rtkit::ENDPOINT_GFX_MESSAGES
            && (message >> 48) as u16 == INITDATA_ACK_TYPE
        {
            data.acked.store(true, Ordering::Release);
            true
        } else {
            false
        }
    }

    fn crashed(
        data: <Self::Data as kernel::types::ForeignOwnable>::Borrowed<'_>,
        _crashlog: Option<&[u8]>,
    ) {
        data.crashed.store(true, Ordering::Release);
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

/// Attempt the current G17P live-boot slice.
pub(crate) fn attempt(
    pdev: &platform::Device<Core>,
    dev: &AsahiDevice,
    registers: &regs::Resources,
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

    let mut manager = g17_manager::G17PManagerConstruction::new(
        dev,
        soc,
        g17_manager::T8140_G17P_PLATFORM_CONFIG,
    )?;

    g17_lifecycle::execute_cpu_pair(&gfx, &gfx1, || {
        dev_info!(
            pdev.as_ref(),
            "G17P live-boot: {:?} acquired GFX then GFX1\n",
            BootStage::StartCpus
        );

        let gfx_state = Arc::new(G17PRoleRtkitState::new(), GFP_KERNEL)?;
        let gfx1_state = Arc::new(G17PRoleRtkitState::new(), GFP_KERNEL)?;
        let mut gfx_rtkit = rtkit::RtKit::<G17PRtkitOps>::new(
            dev.as_ref(),
            None,
            0,
            gfx_state.clone(),
        )?;
        let mut gfx1_rtkit = rtkit::RtKit::<G17PRtkitOps>::new(
            dev.as_ref(),
            None,
            1,
            gfx1_state.clone(),
        )?;
        let mut gfx_rtkit = Pin::new(&mut gfx_rtkit);
        let mut gfx1_rtkit = Pin::new(&mut gfx1_rtkit);

        gfx_rtkit.as_mut().boot()?;
        gfx1_rtkit.as_mut().boot()?;
        dev_info!(
            pdev.as_ref(),
            "G17P live-boot: {:?} completed independently for GFX/GFX1\n",
            BootStage::BootRtkit
        );

        registers.t8140_axi_transition_workaround()?;
        registers.t8140_pre_initdata_clear()?;

        for rtkit in [&mut gfx_rtkit, &mut gfx1_rtkit] {
            rtkit
                .as_mut()
                .start_endpoint(g17_rtkit::ENDPOINT_GFX_MESSAGES)?;
            rtkit
                .as_mut()
                .start_endpoint(g17_rtkit::ENDPOINT_GFX_INTERRUPTS)?;
        }

        let handoff = manager.handoff();
        let gfx_init = g17_rtkit::encode_initdata_doorbell(handoff.primary.instance.root)
            .map_err(|_| EINVAL)?;
        let gfx1_init = g17_rtkit::encode_initdata_doorbell(handoff.secondary.instance.root)
            .map_err(|_| EINVAL)?;
        gfx_rtkit
            .as_mut()
            .send_message(g17_rtkit::ENDPOINT_GFX_MESSAGES, gfx_init)?;
        gfx1_rtkit
            .as_mut()
            .send_message(g17_rtkit::ENDPOINT_GFX_MESSAGES, gfx1_init)?;
        dev_info!(
            pdev.as_ref(),
            "G17P live-boot: {:?} sent roots {:#x}/{:#x}\n",
            BootStage::PublishInitdata,
            handoff.primary.instance.root,
            handoff.secondary.instance.root
        );

        for _ in 0..INITDATA_ACK_POLLS {
            if gfx_state.acked.load(Ordering::Acquire)
                && gfx1_state.acked.load(Ordering::Acquire)
            {
                break;
            }
            if gfx_state.crashed.load(Ordering::Acquire)
                || gfx1_state.crashed.load(Ordering::Acquire)
            {
                return Err(EIO);
            }
            fsleep(Delta::from_millis(10));
        }
        if !gfx_state.acked.load(Ordering::Acquire)
            || !gfx1_state.acked.load(Ordering::Acquire)
        {
            return Err(ETIMEDOUT);
        }
        dev_info!(
            pdev.as_ref(),
            "G17P live-boot: {:?} for both firmware roles\n",
            BootStage::InitdataAcknowledged
        );

        let _queue = manager.configure_compute_sksm_queue(dev, registers, 4, 2)?;
        dev_info!(
            pdev.as_ref(),
            "G17P live-boot: {:?} QID 4 / completion selector 2\n",
            BootStage::RegisterComputeQueue
        );
        Ok(())
    })
}
