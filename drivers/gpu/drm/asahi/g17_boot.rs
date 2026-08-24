// SPDX-License-Identifier: GPL-2.0-only OR MIT

//! Evidence-backed G17 dual-firmware-role boot contract.
//!
//! G17 has two physical ASC roles. `GFX` is the scheduler/recovery ingress and
//! `GFX1` owns the observed internal power transitions. They have distinct
//! transports and init-data roots, but both roots belong to one GPU/UAT
//! address-space owner. This module models only the proven host-side gates; it
//! deliberately contains no endpoint numbers or MMIO writes. Grounded power
//! and recovery validation is kept in the disconnected
//! [`agx_power_recovery`] module.

#[cfg(not(test))]
use crate::{agx_power_recovery, g17_initdata, g17_rtkit};
#[cfg(test)]
#[path = "agx_power_recovery.rs"]
mod agx_power_recovery;
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
    if agx_power_recovery::GROUNDED_POWER_RECOVERY_MODEL_AVAILABLE
        && agx_power_recovery::EXECUTABLE_GFX1_POWER_SEQUENCE_AVAILABLE
    {
        implemented |= missing::GFX1_POWER_SEQUENCE;
    }
    if agx_power_recovery::GROUNDED_POWER_RECOVERY_MODEL_AVAILABLE
        && agx_power_recovery::EXECUTABLE_G17_RECOVERY_HANDSHAKE_AVAILABLE
    {
        implemented |= missing::RECOVERY_HANDSHAKE;
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

/// G17 variants with exact paired nested-firmware evidence.
///
/// G17G/G17P and other variants are deliberately absent because the pinned
/// corpus contains no equivalent role-paired nested images for them.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
#[repr(u8)]
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) enum G17FirmwareVariant {
    G17S = b'S',
    G17C = b'C',
}

/// Fail-closed variant decoding for diagnostic firmware selection.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) enum FirmwareIdentityError {
    UnknownVariant,
}

#[cfg_attr(not(test), allow(dead_code))]
pub(crate) const fn decode_g17_firmware_variant(
    raw: u8,
) -> Result<G17FirmwareVariant, FirmwareIdentityError> {
    match raw {
        b'S' => Ok(G17FirmwareVariant::G17S),
        b'C' => Ok(G17FirmwareVariant::G17C),
        _ => Err(FirmwareIdentityError::UnknownVariant),
    }
}

/// One exact nested Mach-O identity from the pinned role-paired FTAB corpus.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) struct G17NestedFirmwareIdentity {
    pub(crate) entry_tag: [u8; 4],
    pub(crate) uuid: [u8; 16],
    pub(crate) sha256: [u8; 32],
}

#[cfg_attr(not(test), allow(dead_code))]
const G17S_GFX_FIRMWARE_IDENTITY: G17NestedFirmwareIdentity = G17NestedFirmwareIdentity {
    entry_tag: *b"a010",
    uuid: [
        0x05, 0x23, 0x65, 0x20, 0x33, 0xf1, 0x3d, 0x41, 0x83, 0xcb, 0x16, 0x9f, 0xaa, 0xad,
        0xbc, 0xa7,
    ],
    sha256: [
        0x31, 0x7d, 0xe3, 0x5b, 0x44, 0x7e, 0x2a, 0x4c, 0xce, 0x0f, 0xb4, 0x67, 0xf0, 0xab,
        0x04, 0xba, 0xa4, 0x2f, 0xce, 0xee, 0x8a, 0x3f, 0x55, 0x99, 0x8e, 0xcf, 0x3c, 0x62,
        0x4d, 0x30, 0x03, 0x1b,
    ],
};

#[cfg_attr(not(test), allow(dead_code))]
const G17C_GFX_FIRMWARE_IDENTITY: G17NestedFirmwareIdentity = G17NestedFirmwareIdentity {
    entry_tag: *b"a000",
    uuid: [
        0x21, 0xa1, 0x77, 0xa0, 0xc2, 0x7b, 0x30, 0x59, 0xa3, 0xb0, 0x05, 0x6c, 0xdd, 0xac,
        0x65, 0x8b,
    ],
    sha256: [
        0x61, 0xa3, 0x59, 0x3e, 0x78, 0x68, 0x67, 0x29, 0x40, 0x5b, 0x77, 0xe1, 0x67, 0xdd,
        0x74, 0xbf, 0xb2, 0x5b, 0x0c, 0xe5, 0x82, 0x29, 0x65, 0x08, 0x54, 0x40, 0x4b, 0x3f,
        0xd5, 0x3f, 0xd8, 0x9a,
    ],
};

#[cfg_attr(not(test), allow(dead_code))]
const G17S_GFX1_FIRMWARE_IDENTITY: G17NestedFirmwareIdentity = G17NestedFirmwareIdentity {
    entry_tag: *b"a010",
    uuid: [
        0x0e, 0x98, 0xaf, 0xb8, 0x06, 0x80, 0x3b, 0x15, 0x93, 0x2e, 0x91, 0x0d, 0x0a, 0x1c,
        0xcd, 0x78,
    ],
    sha256: [
        0x5d, 0x71, 0x92, 0x08, 0xe1, 0x2b, 0xe6, 0xbf, 0xa7, 0x54, 0xca, 0x84, 0x86, 0xf3,
        0x67, 0x39, 0xfc, 0xe7, 0x4b, 0x0c, 0x45, 0x61, 0x8a, 0xd9, 0xa0, 0xf4, 0x4a, 0x18,
        0x9f, 0xee, 0xb3, 0x45,
    ],
};

#[cfg_attr(not(test), allow(dead_code))]
const G17C_GFX1_FIRMWARE_IDENTITY: G17NestedFirmwareIdentity = G17NestedFirmwareIdentity {
    entry_tag: *b"a000",
    uuid: [
        0xfe, 0xb8, 0x1f, 0x4a, 0xf0, 0x75, 0x34, 0x49, 0xa6, 0x62, 0x3c, 0x01, 0xc0, 0x99,
        0x4b, 0x4f,
    ],
    sha256: [
        0xcf, 0x81, 0x13, 0xa8, 0xbc, 0x4a, 0x62, 0xc3, 0x76, 0xd1, 0x1a, 0x4d, 0xc5, 0xd5,
        0xc5, 0x42, 0x99, 0x14, 0xb9, 0x5a, 0xb2, 0xd2, 0x19, 0x0a, 0x0e, 0xba, 0xd3, 0x27,
        0x8a, 0x32, 0x34, 0x68,
    ],
};

/// Select only an exact role/variant nested-firmware identity.
///
/// This proves the contents of the pinned containers, not a live G17C target
/// or Linux ownership of FTAB extraction, fixups, or firmware loading.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) const fn g17_nested_firmware_identity(
    role: FirmwareRole,
    variant: G17FirmwareVariant,
) -> &'static G17NestedFirmwareIdentity {
    match (role, variant) {
        (FirmwareRole::Gfx, G17FirmwareVariant::G17S) => &G17S_GFX_FIRMWARE_IDENTITY,
        (FirmwareRole::Gfx, G17FirmwareVariant::G17C) => &G17C_GFX_FIRMWARE_IDENTITY,
        (FirmwareRole::Gfx1, G17FirmwareVariant::G17S) => &G17S_GFX1_FIRMWARE_IDENTITY,
        (FirmwareRole::Gfx1, G17FirmwareVariant::G17C) => &G17C_GFX1_FIRMWARE_IDENTITY,
    }
}

/// Validate tag, UUID, and hash together; no field is sufficient by itself.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn validate_g17_nested_firmware_identity(
    role: FirmwareRole,
    variant: G17FirmwareVariant,
    observed: &G17NestedFirmwareIdentity,
) -> bool {
    observed == g17_nested_firmware_identity(role, variant)
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
    fn nested_firmware_identity_is_exact_per_role_and_variant() {
        for role in [FirmwareRole::Gfx, FirmwareRole::Gfx1] {
            let g17s = g17_nested_firmware_identity(role, G17FirmwareVariant::G17S);
            let g17c = g17_nested_firmware_identity(role, G17FirmwareVariant::G17C);
            assert_eq!(g17s.entry_tag, *b"a010");
            assert_eq!(g17c.entry_tag, *b"a000");
            assert_ne!(g17s.uuid, g17c.uuid);
            assert_ne!(g17s.sha256, g17c.sha256);
            assert!(validate_g17_nested_firmware_identity(
                role,
                G17FirmwareVariant::G17S,
                g17s,
            ));
            assert!(validate_g17_nested_firmware_identity(
                role,
                G17FirmwareVariant::G17C,
                g17c,
            ));

            let mut changed = *g17s;
            changed.sha256[0] ^= 1;
            assert!(!validate_g17_nested_firmware_identity(
                role,
                G17FirmwareVariant::G17S,
                &changed,
            ));
            assert!(!validate_g17_nested_firmware_identity(
                role,
                G17FirmwareVariant::G17C,
                g17s,
            ));
        }

        assert_eq!(
            decode_g17_firmware_variant(b'S'),
            Ok(G17FirmwareVariant::G17S)
        );
        assert_eq!(
            decode_g17_firmware_variant(b'C'),
            Ok(G17FirmwareVariant::G17C)
        );
        for raw in [b'G', b'P', b'A', 0, u8::MAX] {
            assert_eq!(
                decode_g17_firmware_variant(raw),
                Err(FirmwareIdentityError::UnknownVariant)
            );
        }

        let missing = pre_handoff_gate(G17_GENERATION).unwrap_err();
        assert!(missing.contains(missing::FIRMWARE_IDENTITY));
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
        assert!(agx_power_recovery::GROUNDED_POWER_RECOVERY_MODEL_AVAILABLE);
        assert!(
            agx_power_recovery::validate_g17s_gfx1_sleep_to_21_pio_prefix(
                &agx_power_recovery::G17S_GFX1_SLEEP_TO_21_PIO_PREFIX,
            )
        );
        assert!(
            agx_power_recovery::validate_g17s_gfx1_pre_idle_firmware_ops(
                &agx_power_recovery::G17S_GFX1_PRE_IDLE_FIRMWARE_OPS,
            )
        );
        assert_eq!(
            agx_power_recovery::g17s_pio_mapped_pointer_offset(
                agx_power_recovery::G17PioSlot::PushTelemetryDashboardConfigSpace,
            ),
            0x1c0
        );
        assert!(agx_power_recovery::g17s_gfx1_idle_poll_satisfied(1));
        assert_eq!(
            agx_power_recovery::g17s_gfx1_report_timebase_delta_us(
                agx_power_recovery::G17S_GFX1_IDLE_POLL_TIMEOUT_TICKS,
            ),
            agx_power_recovery::G17S_GFX1_IDLE_POLL_REPORTED_MAX_US
        );
        assert!(!agx_power_recovery::EXECUTABLE_GFX1_POWER_SEQUENCE_AVAILABLE);
        assert!(!agx_power_recovery::EXECUTABLE_G17_RECOVERY_HANDSHAKE_AVAILABLE);
    }
}
