// SPDX-License-Identifier: GPL-2.0-only OR MIT

#![cfg_attr(not(test), allow(dead_code))]

//! Evidence-backed G15-G17 recovery and G17 internal-power contracts.
//!
//! This module contains only pure validation and host-side recovery writes.
//! It performs no MMIO, mailbox traffic, ASC reset, or firmware-state write.
//! The power-transition grammar is byte-confirmed only for the pinned G17S
//! GFX1 image. The recovery word, host writes, and no-op reset hook are
//! byte-confirmed in pinned G15G, G16G, G17G, G17S, and G17P host drivers.
//! Their shared values are modeled here without reusing generation-specific
//! structure offsets (`0x5190` on the pinned G15/G16 layouts and `0x4ae0` on
//! the pinned G17 layouts).

/// The grounded, pure validation model is available.
pub(crate) const GROUNDED_POWER_RECOVERY_MODEL_AVAILABLE: bool = true;

/// Exact targets whose host binaries establish the shared recovery grammar.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) enum RecoveryEvidenceTarget {
    G15G,
    G16G,
    G17P,
    G17G,
    G17S,
}

pub(crate) const RECOVERY_EVIDENCE_TARGETS: [RecoveryEvidenceTarget; 5] = [
    RecoveryEvidenceTarget::G15G,
    RecoveryEvidenceTarget::G16G,
    RecoveryEvidenceTarget::G17P,
    RecoveryEvidenceTarget::G17G,
    RecoveryEvidenceTarget::G17S,
];

/// G17S establishes the internal transition grammar, but G17G/G17P parity,
/// the PIO object mappings, polling, and settle timing remain unproved.
pub(crate) const EXECUTABLE_GFX1_POWER_SEQUENCE_AVAILABLE: bool = false;

/// The firmware writes and cross-role 0x22/0x23 protocol remain unproved, so
/// Linux must not execute G17 recovery from this host-write subset.
pub(crate) const EXECUTABLE_G17_RECOVERY_HANDSHAKE_AVAILABLE: bool = false;

/// G17S GFX1 internal power states whose numeric names are string-confirmed.
pub(crate) const GFX1_POWER_SLEEP: u8 = 0x08;
pub(crate) const GFX1_POWER_NAP: u8 = 0x10;

/// Mandatory intermediate state between SLEEP and NAP.
///
/// Its private Apple enum name is not known, so the numeric value is kept
/// without assigning a semantic name.
pub(crate) const GFX1_POWER_INTERMEDIATE_21: u8 = 0x21;

/// Fail-closed result for the bounded G17S GFX1 transition grammar.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) enum PowerTransitionError {
    /// Direct SLEEP-to-NAP and NAP-to-SLEEP paths reach firmware aborts.
    DirectSleepNapIsFatal,
    /// No other edge was established by the pinned transition callback.
    UnprovenTransition,
}

/// Validate only the four proven G17S bridge edges.
///
/// Same-state requests and unknown values are deliberately rejected. This
/// function must not be used as proof that G17G or G17P share the grammar.
pub(crate) const fn validate_g17s_gfx1_power_transition(
    old: u8,
    new: u8,
) -> Result<(), PowerTransitionError> {
    if (old == GFX1_POWER_SLEEP && new == GFX1_POWER_NAP)
        || (old == GFX1_POWER_NAP && new == GFX1_POWER_SLEEP)
    {
        return Err(PowerTransitionError::DirectSleepNapIsFatal);
    }

    if ((old == GFX1_POWER_SLEEP || old == GFX1_POWER_NAP) && new == GFX1_POWER_INTERMEDIATE_21)
        || (old == GFX1_POWER_INTERMEDIATE_21 && (new == GFX1_POWER_SLEEP || new == GFX1_POWER_NAP))
    {
        Ok(())
    } else {
        Err(PowerTransitionError::UnprovenTransition)
    }
}

/// Exact values observed in the shared G15-G17 host recovery word.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
#[repr(u32)]
pub(crate) enum RecoveryWordState {
    RunningOrPostRecoveryResumed = 0,
    FirmwareStoppedForPreRecovery = 1,
    HostResumedPreRecovery = 2,
    FirmwareStoppedForPostRecovery = 3,
}

/// Host-owned recovery phase whose write is directly established.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) enum HostResumePhase {
    PreRecovery,
    PostRecovery,
}

/// Fail-closed recovery-word validation errors.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) enum RecoveryError {
    UnknownWordValue,
    FirmwareNotStoppedForPhase,
}

/// Decode only the four values checked by every pinned evidence target.
pub(crate) const fn decode_recovery_word(raw: u32) -> Result<RecoveryWordState, RecoveryError> {
    match raw {
        0 => Ok(RecoveryWordState::RunningOrPostRecoveryResumed),
        1 => Ok(RecoveryWordState::FirmwareStoppedForPreRecovery),
        2 => Ok(RecoveryWordState::HostResumedPreRecovery),
        3 => Ok(RecoveryWordState::FirmwareStoppedForPostRecovery),
        _ => Err(RecoveryError::UnknownWordValue),
    }
}

/// Return the exact host value for one directly established resume edge.
///
/// This deliberately exposes neither inferred firmware edge (`0 -> 1` or
/// `2 -> 3`). The caller must observe the matching stopped value first.
pub(crate) const fn host_resume_recovery(
    phase: HostResumePhase,
    observed_raw: u32,
) -> Result<RecoveryWordState, RecoveryError> {
    let observed = match decode_recovery_word(observed_raw) {
        Ok(value) => value,
        Err(error) => return Err(error),
    };

    match (phase, observed) {
        (HostResumePhase::PreRecovery, RecoveryWordState::FirmwareStoppedForPreRecovery) => {
            Ok(RecoveryWordState::HostResumedPreRecovery)
        }
        (HostResumePhase::PostRecovery, RecoveryWordState::FirmwareStoppedForPostRecovery) => {
            Ok(RecoveryWordState::RunningOrPostRecoveryResumed)
        }
        _ => Err(RecoveryError::FirmwareNotStoppedForPhase),
    }
}

/// Apple's host reset hook is exactly `bti c; ret` in every evidence target.
pub(crate) const HOST_RESET_FOR_RECOVERY_IS_NOOP: bool = true;

/// Recovery inspection enters through role 0/GFX and drains both role logs.
/// These role facts are specific to the dual-role G17 implementation.
pub(crate) const G17_RECOVERY_INSPECTION_INGRESS_ROLE: u8 = 0;
pub(crate) const G17_RECOVERY_LOG_ROLE_MASK: u8 = 0b11;

/// Only these two values reach recognized recovery power-cycle paths in the
/// pinned G17S GFX1 image. Their private names and wire ordering are unknown.
pub(crate) const G17S_GFX1_RECOVERY_POWER_EVENTS: [u8; 2] = [0x22, 0x23];

pub(crate) const fn validate_g17s_gfx1_recovery_power_event(event: u8) -> bool {
    event == G17S_GFX1_RECOVERY_POWER_EVENTS[0] || event == G17S_GFX1_RECOVERY_POWER_EVENTS[1]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sleep_and_nap_require_the_numeric_0x21_bridge() {
        for edge in [
            (GFX1_POWER_SLEEP, GFX1_POWER_INTERMEDIATE_21),
            (GFX1_POWER_NAP, GFX1_POWER_INTERMEDIATE_21),
            (GFX1_POWER_INTERMEDIATE_21, GFX1_POWER_SLEEP),
            (GFX1_POWER_INTERMEDIATE_21, GFX1_POWER_NAP),
        ] {
            assert_eq!(validate_g17s_gfx1_power_transition(edge.0, edge.1), Ok(()));
        }

        for edge in [
            (GFX1_POWER_SLEEP, GFX1_POWER_NAP),
            (GFX1_POWER_NAP, GFX1_POWER_SLEEP),
        ] {
            assert_eq!(
                validate_g17s_gfx1_power_transition(edge.0, edge.1),
                Err(PowerTransitionError::DirectSleepNapIsFatal)
            );
        }
    }

    #[test]
    fn unproved_power_edges_fail_closed() {
        for edge in [
            (GFX1_POWER_SLEEP, GFX1_POWER_SLEEP),
            (GFX1_POWER_NAP, GFX1_POWER_NAP),
            (GFX1_POWER_INTERMEDIATE_21, GFX1_POWER_INTERMEDIATE_21),
            (0, GFX1_POWER_INTERMEDIATE_21),
            (GFX1_POWER_INTERMEDIATE_21, 0xff),
        ] {
            assert_eq!(
                validate_g17s_gfx1_power_transition(edge.0, edge.1),
                Err(PowerTransitionError::UnprovenTransition)
            );
        }
    }

    #[test]
    fn recovery_word_decoder_has_no_fifth_state() {
        for raw in 0..=3 {
            assert_eq!(decode_recovery_word(raw).unwrap() as u32, raw);
        }
        for raw in [4, 0x22, 0x23, u32::MAX] {
            assert_eq!(
                decode_recovery_word(raw),
                Err(RecoveryError::UnknownWordValue)
            );
        }
    }

    #[test]
    fn host_owns_only_the_two_proven_resume_writes() {
        assert_eq!(
            host_resume_recovery(HostResumePhase::PreRecovery, 1),
            Ok(RecoveryWordState::HostResumedPreRecovery)
        );
        assert_eq!(
            host_resume_recovery(HostResumePhase::PostRecovery, 3),
            Ok(RecoveryWordState::RunningOrPostRecoveryResumed)
        );

        for raw in 0..=3 {
            if raw != 1 {
                assert_eq!(
                    host_resume_recovery(HostResumePhase::PreRecovery, raw),
                    Err(RecoveryError::FirmwareNotStoppedForPhase)
                );
            }
            if raw != 3 {
                assert_eq!(
                    host_resume_recovery(HostResumePhase::PostRecovery, raw),
                    Err(RecoveryError::FirmwareNotStoppedForPhase)
                );
            }
        }
    }

    #[test]
    fn recovery_topology_and_g17s_events_are_bounded() {
        assert_eq!(G17_RECOVERY_INSPECTION_INGRESS_ROLE, 0);
        assert_eq!(G17_RECOVERY_LOG_ROLE_MASK, 0b11);
        assert!(HOST_RESET_FOR_RECOVERY_IS_NOOP);
        assert_eq!(G17S_GFX1_RECOVERY_POWER_EVENTS, [0x22, 0x23]);
        assert!(validate_g17s_gfx1_recovery_power_event(0x22));
        assert!(validate_g17s_gfx1_recovery_power_event(0x23));
        for event in [0, 0x21, 0x24, u8::MAX] {
            assert!(!validate_g17s_gfx1_recovery_power_event(event));
        }
    }

    #[test]
    fn grounded_model_does_not_open_runtime_gates() {
        assert!(GROUNDED_POWER_RECOVERY_MODEL_AVAILABLE);
        assert!(!EXECUTABLE_GFX1_POWER_SEQUENCE_AVAILABLE);
        assert!(!EXECUTABLE_G17_RECOVERY_HANDSHAKE_AVAILABLE);
        assert_eq!(
            RECOVERY_EVIDENCE_TARGETS,
            [
                RecoveryEvidenceTarget::G15G,
                RecoveryEvidenceTarget::G16G,
                RecoveryEvidenceTarget::G17P,
                RecoveryEvidenceTarget::G17G,
                RecoveryEvidenceTarget::G17S,
            ]
        );
    }
}
