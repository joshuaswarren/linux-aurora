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
    _registers: regs::Resources,
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

    fn initialize(&mut self) -> Result {
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
            &self.dev, self.uat.clone(), timestamp)?);
        self.uat.confirm_t8140_primary_high_root(roots)?;
        self.uat.mirror_t8140_secondary_high_root()?;
        dev_info!(self.dev.as_ref(), "M5_INIT retained memory graph mapped\n");

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
        Ok(())
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
            memory: None, uat, _registers: registers, dev,
        }, GFP_KERNEL)?;
        // From this point every path returns the retained platform owner,
        // even when firmware rejects the graph. Failed probe unwinding must
        // never release UAT or transports beneath a running firmware CPU.
        // Keep callback code resident even if a failed stop retains owners.
        // This explicit RAM-boot experiment is released by reboot only.
        unsafe { kernel::bindings::__module_get(crate::THIS_MODULE.as_ptr()); }
        let result = session.initialize();
        dev_info!(pdev.as_ref(), "M5_INIT transaction result={:?}; no GPU jobs submitted\n", result);
        let stopped = session.stop_cpus();
        dev_info!(pdev.as_ref(), "M5_INIT stop requests={:?}; owners retained\n", stopped);
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
