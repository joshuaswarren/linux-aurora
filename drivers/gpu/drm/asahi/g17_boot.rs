// SPDX-License-Identifier: GPL-2.0-only OR MIT

//! Evidence-backed G17 dual-firmware-role boot contract.
//!
//! G17 has two physical ASC roles. `GFX` is the scheduler/recovery ingress and
//! `GFX1` owns the observed internal power transitions. They have distinct
//! transports and init-data roots, but both roots belong to one GPU/UAT
//! address-space owner. This module models only the proven host-side gates; it
//! deliberately contains no endpoint numbers, MMIO writes, power-state names,
//! or recovery messages.

#[cfg(not(test))]
use crate::{g17_initdata, g17_rtkit};
#[cfg(test)]
#[path = "g17_initdata.rs"]
mod g17_initdata;
#[cfg(test)]
#[path = "g17_rtkit.rs"]
mod g17_rtkit;

/// Design generation that requires the dual-role contract.
const G17_GENERATION: u32 = 17;

/// Evidence required before Linux may hand init-data to either G17 role.
pub(crate) mod missing {
    pub(crate) const FIRMWARE_IDENTITY: u16 = 1 << 0;
    pub(crate) const ROLE_RESOURCES: u16 = 1 << 1;
    pub(crate) const TOPOLOGY_REGISTERS: u16 = 1 << 2;
    pub(crate) const UAT_HANDOFF: u16 = 1 << 3;
    pub(crate) const INITDATA_GRAPH: u16 = 1 << 4;
    pub(crate) const RTKIT_TRANSPORT: u16 = 1 << 5;
    pub(crate) const GFX1_POWER_SEQUENCE: u16 = 1 << 6;
    pub(crate) const RECOVERY_HANDSHAKE: u16 = 1 << 7;
    pub(crate) const SUBMISSION_TRANSPORT: u16 = 1 << 8;
}

const REQUIRED_PRE_HANDOFF_EVIDENCE: u16 = missing::FIRMWARE_IDENTITY
    | missing::ROLE_RESOURCES
    | missing::TOPOLOGY_REGISTERS
    | missing::UAT_HANDOFF
    | missing::INITDATA_GRAPH
    | missing::RTKIT_TRANSPORT
    | missing::GFX1_POWER_SEQUENCE
    | missing::RECOVERY_HANDSHAKE
    | missing::SUBMISSION_TRANSPORT;

/// Exact fail-closed result returned to the probe boundary.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) struct MissingEvidence(u16);

impl MissingEvidence {
    pub(crate) const fn bits(self) -> u16 {
        self.0
    }

    pub(crate) const fn contains(self, evidence: u16) -> bool {
        self.0 & evidence != 0
    }
}

const fn implemented_pre_handoff_evidence() -> u16 {
    let mut implemented = 0;

    // These constants deliberately distinguish useful offline codecs from an
    // executable firmware ABI. When either module becomes complete, this gate
    // changes with the implementation rather than a disconnected probe flag.
    if g17_initdata::GROUNDED_LAYOUT_AVAILABLE && g17_initdata::BOOTABLE_LAYOUT_AVAILABLE {
        implemented |= missing::INITDATA_GRAPH;
    }
    if g17_rtkit::GROUNDED_CODEC_AVAILABLE && g17_rtkit::EXECUTABLE_TRANSPORT_AVAILABLE {
        implemented |= missing::RTKIT_TRANSPORT;
    }

    implemented
}

/// Require all runtime evidence before any G17 ASC is started.
///
/// Older, already-supported generations do not use this gate. The caller maps
/// an error to `ENODEV` before MMIO initialization or firmware CPU startup.
pub(crate) const fn pre_handoff_gate(gpu_generation: u32) -> Result<(), MissingEvidence> {
    if gpu_generation != G17_GENERATION {
        return Ok(());
    }

    let missing_bits = REQUIRED_PRE_HANDOFF_EVIDENCE & !implemented_pre_handoff_evidence();
    if missing_bits == 0 {
        Ok(())
    } else {
        Err(MissingEvidence(missing_bits))
    }
}

/// The two mandatory G17 firmware roles, in host kick order.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) enum FirmwareRole {
    /// Scheduler and recovery-ingress role, kicked first.
    Gfx = 0,
    /// Power-management role, kicked second.
    Gfx1 = 1,
}

const ROLE_COUNT: usize = 2;
const KICK_ORDER: [FirmwareRole; ROLE_COUNT] = [FirmwareRole::Gfx, FirmwareRole::Gfx1];

impl FirmwareRole {
    const fn index(self) -> usize {
        self as usize
    }
}

/// One GPU allocator/UAT owner shared by both role records.
///
/// This is intentionally stored once in [`DualRoleBoot`]. A role never owns a
/// second UAT or allocator.
#[derive(Debug)]
pub(crate) struct SharedGpuUatOwner;

#[derive(Debug, Copy, Clone, Default)]
struct RoleState {
    initdata_root_ready: bool,
    kick_succeeded: bool,
    started_notified: bool,
    cold_boot_complete: bool,
}

/// Fail-closed errors in the host-observed boot sequence.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) enum BootError {
    /// Both role roots must be built and validated before the first kick.
    InitdataRootsIncomplete,
    /// A role was kicked out of the exact `GFX`, then `GFX1` order.
    WrongKickOrder,
    /// A role start call failed; the boot attempt is permanently failed.
    KickFailed,
    /// A root was changed after the first role kick.
    RootMutationAfterKick,
    /// An earlier sequencing error already failed this boot attempt.
    AlreadyFailed,
}

/// Pure state model for one G17 dual-role cold boot.
///
/// Notifications may race with the host's start calls, so notification methods
/// only record flags. The three public gate predicates remain independent, and
/// [`Self::handoff_ready`] composes them before submission can be exposed.
#[cfg_attr(not(test), allow(dead_code))]
#[derive(Debug)]
pub(crate) struct DualRoleBoot {
    owner: SharedGpuUatOwner,
    roles: [RoleState; ROLE_COUNT],
    next_kick: usize,
    failed: bool,
}

#[cfg_attr(not(test), allow(dead_code))]
impl DualRoleBoot {
    /// Create an empty boot attempt with one shared GPU/UAT owner.
    pub(crate) const fn new() -> Self {
        Self {
            owner: SharedGpuUatOwner,
            roles: [
                RoleState {
                    initdata_root_ready: false,
                    kick_succeeded: false,
                    started_notified: false,
                    cold_boot_complete: false,
                },
                RoleState {
                    initdata_root_ready: false,
                    kick_succeeded: false,
                    started_notified: false,
                    cold_boot_complete: false,
                },
            ],
            next_kick: 0,
            failed: false,
        }
    }

    /// Return the single address-space owner used by either role.
    pub(crate) const fn owner_for(&self, _role: FirmwareRole) -> &SharedGpuUatOwner {
        &self.owner
    }

    /// Mark one role-specific init-data root ready for the shared UAT.
    pub(crate) fn mark_initdata_root_ready(&mut self, role: FirmwareRole) -> Result<(), BootError> {
        if self.failed {
            return Err(BootError::AlreadyFailed);
        }
        if self.next_kick != 0 {
            self.failed = true;
            return Err(BootError::RootMutationAfterKick);
        }

        self.roles[role.index()].initdata_root_ready = true;
        Ok(())
    }

    /// Record the return value of the next role start call.
    ///
    /// Both roots must already exist, and calls must be recorded in exact
    /// `GFX`, then `GFX1` order. Any failure permanently closes all gates.
    pub(crate) fn record_kick(
        &mut self,
        role: FirmwareRole,
        succeeded: bool,
    ) -> Result<(), BootError> {
        if self.failed {
            return Err(BootError::AlreadyFailed);
        }
        if self.roles.iter().any(|state| !state.initdata_root_ready) {
            self.failed = true;
            return Err(BootError::InitdataRootsIncomplete);
        }
        if self.next_kick >= ROLE_COUNT || KICK_ORDER[self.next_kick] != role {
            self.failed = true;
            return Err(BootError::WrongKickOrder);
        }
        if !succeeded {
            self.failed = true;
            return Err(BootError::KickFailed);
        }

        self.roles[role.index()].kick_succeeded = true;
        self.next_kick += 1;
        Ok(())
    }

    /// Record the role's started callback.
    ///
    /// Apple's combined started byte is set by either callback; it is not an
    /// AND of both role flags.
    pub(crate) fn notify_started(&mut self, role: FirmwareRole) {
        self.roles[role.index()].started_notified = true;
    }

    /// Record one role's later cold-boot-complete callback.
    pub(crate) fn notify_cold_boot_complete(&mut self, role: FirmwareRole) {
        self.roles[role.index()].cold_boot_complete = true;
    }

    /// Both ordered role start calls returned success.
    pub(crate) fn kick_gate_open(&self) -> bool {
        !self.failed
            && self.next_kick == ROLE_COUNT
            && self.roles.iter().all(|state| state.kick_succeeded)
    }

    /// At least one role delivered its started callback.
    pub(crate) fn early_start_gate_open(&self) -> bool {
        !self.failed && self.roles.iter().any(|state| state.started_notified)
    }

    /// Both per-role cold-boot flags are set.
    pub(crate) fn cold_boot_gate_open(&self) -> bool {
        !self.failed && self.roles.iter().all(|state| state.cold_boot_complete)
    }

    /// Full readiness requires every independent host-observed gate.
    pub(crate) fn handoff_ready(&self) -> bool {
        self.kick_gate_open() && self.early_start_gate_open() && self.cold_boot_gate_open()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roots_ready() -> DualRoleBoot {
        let mut boot = DualRoleBoot::new();
        boot.mark_initdata_root_ready(FirmwareRole::Gfx).unwrap();
        boot.mark_initdata_root_ready(FirmwareRole::Gfx1).unwrap();
        boot
    }

    fn roles_kicked() -> DualRoleBoot {
        let mut boot = roots_ready();
        boot.record_kick(FirmwareRole::Gfx, true).unwrap();
        boot.record_kick(FirmwareRole::Gfx1, true).unwrap();
        boot
    }

    #[test]
    fn both_roles_resolve_to_one_gpu_uat_owner() {
        let boot = DualRoleBoot::new();
        assert!(core::ptr::eq(
            boot.owner_for(FirmwareRole::Gfx),
            boot.owner_for(FirmwareRole::Gfx1)
        ));
    }

    #[test]
    fn both_roots_are_required_before_first_kick() {
        let mut boot = DualRoleBoot::new();
        boot.mark_initdata_root_ready(FirmwareRole::Gfx).unwrap();
        assert_eq!(
            boot.record_kick(FirmwareRole::Gfx, true),
            Err(BootError::InitdataRootsIncomplete)
        );
        assert!(!boot.kick_gate_open());
        assert_eq!(
            boot.mark_initdata_root_ready(FirmwareRole::Gfx1),
            Err(BootError::AlreadyFailed)
        );
    }

    #[test]
    fn kick_order_is_exact_and_both_calls_must_succeed() {
        let mut wrong_order = roots_ready();
        assert_eq!(
            wrong_order.record_kick(FirmwareRole::Gfx1, true),
            Err(BootError::WrongKickOrder)
        );
        assert!(!wrong_order.kick_gate_open());

        let mut second_failed = roots_ready();
        second_failed.record_kick(FirmwareRole::Gfx, true).unwrap();
        assert_eq!(
            second_failed.record_kick(FirmwareRole::Gfx1, false),
            Err(BootError::KickFailed)
        );
        assert!(!second_failed.kick_gate_open());

        assert!(roles_kicked().kick_gate_open());
    }

    #[test]
    fn early_gate_is_either_role_not_both_roles() {
        for role in [FirmwareRole::Gfx, FirmwareRole::Gfx1] {
            let mut boot = roles_kicked();
            assert!(!boot.early_start_gate_open());
            boot.notify_started(role);
            assert!(boot.early_start_gate_open());
            assert!(!boot.cold_boot_gate_open());
            assert!(!boot.handoff_ready());
        }
    }

    #[test]
    fn cold_gate_requires_both_roles() {
        let mut boot = roles_kicked();
        boot.notify_started(FirmwareRole::Gfx);
        boot.notify_cold_boot_complete(FirmwareRole::Gfx);
        assert!(!boot.cold_boot_gate_open());
        assert!(!boot.handoff_ready());

        boot.notify_cold_boot_complete(FirmwareRole::Gfx1);
        assert!(boot.cold_boot_gate_open());
        assert!(boot.handoff_ready());
    }

    #[test]
    fn notifications_do_not_bypass_failed_kick_gate() {
        let mut boot = roots_ready();
        boot.record_kick(FirmwareRole::Gfx, false).unwrap_err();
        for role in [FirmwareRole::Gfx, FirmwareRole::Gfx1] {
            boot.notify_started(role);
            boot.notify_cold_boot_complete(role);
        }
        assert!(!boot.kick_gate_open());
        assert!(!boot.early_start_gate_open());
        assert!(!boot.cold_boot_gate_open());
        assert!(!boot.handoff_ready());
    }

    #[test]
    fn root_mutation_after_kick_fails_closed() {
        let mut boot = roots_ready();
        boot.record_kick(FirmwareRole::Gfx, true).unwrap();
        assert_eq!(
            boot.mark_initdata_root_ready(FirmwareRole::Gfx1),
            Err(BootError::RootMutationAfterKick)
        );
        assert!(!boot.kick_gate_open());
    }

    #[test]
    fn g17_pre_handoff_is_blocked_but_supported_generations_are_unchanged() {
        assert!(pre_handoff_gate(13).is_ok());
        assert!(pre_handoff_gate(14).is_ok());
        assert!(pre_handoff_gate(15).is_ok());
        assert!(pre_handoff_gate(16).is_ok());
        let missing = pre_handoff_gate(17).unwrap_err();
        assert_eq!(missing.bits(), REQUIRED_PRE_HANDOFF_EVIDENCE);
        for evidence in [
            missing::FIRMWARE_IDENTITY,
            missing::ROLE_RESOURCES,
            missing::TOPOLOGY_REGISTERS,
            missing::UAT_HANDOFF,
            missing::INITDATA_GRAPH,
            missing::RTKIT_TRANSPORT,
            missing::GFX1_POWER_SEQUENCE,
            missing::RECOVERY_HANDSHAKE,
            missing::SUBMISSION_TRANSPORT,
        ] {
            assert!(missing.contains(evidence));
        }
        assert!(g17_initdata::GROUNDED_LAYOUT_AVAILABLE);
        assert!(!g17_initdata::BOOTABLE_LAYOUT_AVAILABLE);
        assert!(g17_rtkit::GROUNDED_CODEC_AVAILABLE);
        assert!(!g17_rtkit::EXECUTABLE_TRANSPORT_AVAILABLE);
    }
}
