// SPDX-License-Identifier: GPL-2.0-only OR MIT

//! Device-derived G17 ASC power/boot/teardown contract.
//!
//! This is a pure validator.  It does not map an ASC aperture, request a
//! power domain, write IORVBAR, release a CPU, or claim that Linux owns any
//! of those operations.  The ordering and register transforms below come
//! from the pinned 25G83 G17P/G17G/G17S AppleA7IOP, ASCWrap-v6, RTBuddy, and
//! AGX host images.  Unknown PMGR and resource-ownership steps remain closed.

#![cfg_attr(not(test), allow(dead_code))]

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) enum G17AscEvidenceTarget {
    G17P,
    G17G,
    G17S,
}

/// Text anchors for the common G17 ASC bridge in each pinned kernelcache.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) struct G17AscTextEvidence {
    pub(crate) rtbuddy_load_firmware: u64,
    pub(crate) a7iop_start_cpu_with_options: u64,
    pub(crate) a7iop_stop_cpu: u64,
    pub(crate) a7iop_reset_state: u64,
    pub(crate) ascwrap_set_iorvbar: u64,
    pub(crate) ascwrap_run_cpu: u64,
}

pub(crate) const fn g17_asc_text_evidence(target: G17AscEvidenceTarget) -> G17AscTextEvidence {
    match target {
        G17AscEvidenceTarget::G17P => G17AscTextEvidence {
            rtbuddy_load_firmware: 0xffff_fe00_0b1f_b288,
            a7iop_start_cpu_with_options: 0xffff_fe00_08ab_c6d0,
            a7iop_stop_cpu: 0xffff_fe00_08ab_ca30,
            a7iop_reset_state: 0xffff_fe00_08ab_dde0,
            ascwrap_set_iorvbar: 0xffff_fe00_08ab_a1f8,
            ascwrap_run_cpu: 0xffff_fe00_08ab_a4b4,
        },
        G17AscEvidenceTarget::G17G => G17AscTextEvidence {
            rtbuddy_load_firmware: 0xffff_fe00_0b5f_df08,
            a7iop_start_cpu_with_options: 0xffff_fe00_08be_fd20,
            a7iop_stop_cpu: 0xffff_fe00_08bf_0080,
            a7iop_reset_state: 0xffff_fe00_08bf_1430,
            ascwrap_set_iorvbar: 0xffff_fe00_08be_c448,
            ascwrap_run_cpu: 0xffff_fe00_08be_c704,
        },
        G17AscEvidenceTarget::G17S => G17AscTextEvidence {
            rtbuddy_load_firmware: 0xffff_fe00_0b2e_cab8,
            a7iop_start_cpu_with_options: 0xffff_fe00_08bb_6a50,
            a7iop_stop_cpu: 0xffff_fe00_08bb_6db0,
            a7iop_reset_state: 0xffff_fe00_08bb_8160,
            ascwrap_set_iorvbar: 0xffff_fe00_08bb_3178,
            ascwrap_run_cpu: 0xffff_fe00_08bb_3434,
        },
    }
}

/// The three low-level methods have instruction-identical bodies in all
/// pinned G17 kernelcaches.  The larger start/stop methods differ only in
/// diagnostic string addresses after normalizing PC-relative references.
pub(crate) const G17_ASC_BRIDGE_TARGET_PARITY_PROVEN: bool = true;

pub(crate) const ASC_CPU_CONTROL_OFFSET: u32 = 0x44;
pub(crate) const ASC_CPU_RUN_BIT: u32 = 0x10;
/// `_runCPU(false)` clears this second bit after clearing RUN and rereading.
/// Apple does not name its meaning in the recovered method, so neither do we.
pub(crate) const ASC_CPU_STOP_SECOND_CLEAR_BIT: u32 = 0x20;
pub(crate) const ASC_IORVBAR_VALID_BIT: u64 = 1;

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) enum AscMmioOp {
    ReadControl { offset: u32 },
    WriteControlSet { offset: u32, bits: u32 },
    WriteControlClear { offset: u32, bits: u32 },
    None,
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) struct AscMmioPlan {
    pub(crate) len: u8,
    pub(crate) ops: [AscMmioOp; 4],
}

/// Exact ASCWrap-v6 `_runCPU` register transform.
///
/// The wrapper first tests its object flag at +0x132.  When that flag is
/// clear the method returns without MMIO; this is modeled explicitly instead
/// of assuming the aperture is live.
pub(crate) const fn asc_run_cpu_plan(run: bool, wrapper_mmio_enabled: bool) -> AscMmioPlan {
    if !wrapper_mmio_enabled {
        return AscMmioPlan {
            len: 0,
            ops: [AscMmioOp::None; 4],
        };
    }

    if run {
        AscMmioPlan {
            len: 2,
            ops: [
                AscMmioOp::ReadControl {
                    offset: ASC_CPU_CONTROL_OFFSET,
                },
                AscMmioOp::WriteControlSet {
                    offset: ASC_CPU_CONTROL_OFFSET,
                    bits: ASC_CPU_RUN_BIT,
                },
                AscMmioOp::None,
                AscMmioOp::None,
            ],
        }
    } else {
        AscMmioPlan {
            len: 4,
            ops: [
                AscMmioOp::ReadControl {
                    offset: ASC_CPU_CONTROL_OFFSET,
                },
                AscMmioOp::WriteControlClear {
                    offset: ASC_CPU_CONTROL_OFFSET,
                    bits: ASC_CPU_RUN_BIT,
                },
                AscMmioOp::ReadControl {
                    offset: ASC_CPU_CONTROL_OFFSET,
                },
                AscMmioOp::WriteControlClear {
                    offset: ASC_CPU_CONTROL_OFFSET,
                    bits: ASC_CPU_STOP_SECOND_CLEAR_BIT,
                },
            ],
        }
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) enum IorvbarError {
    AddressAlreadyMarkedValid,
    WrongEncodedValue,
}

/// Encode the exact `_setIORVBAR` write while refusing an already-tagged
/// input address at the Linux validation boundary.
pub(crate) const fn encode_iorvbar(firmware_address: u64) -> Result<u64, IorvbarError> {
    if firmware_address & ASC_IORVBAR_VALID_BIT != 0 {
        Err(IorvbarError::AddressAlreadyMarkedValid)
    } else {
        Ok(firmware_address | ASC_IORVBAR_VALID_BIT)
    }
}

pub(crate) const fn validate_iorvbar_write(
    firmware_address: u64,
    observed: u64,
) -> Result<(), IorvbarError> {
    match encode_iorvbar(firmware_address) {
        Ok(expected) if expected == observed => Ok(()),
        Ok(_) => Err(IorvbarError::WrongEncodedValue),
        Err(e) => Err(e),
    }
}

/// Named start operations recovered by resolving the AppleA7IOP virtual
/// slots against the ASCWrap-v6 vtable.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) enum A7IopStartStep {
    RtBuddySetPowerState3,
    RequireNotRunning,
    IfNormalEnablePowerLateRequireZero,
    IfMapperFlagSetMapperActive,
    IfFirmwareAndNormalMapFirmware,
    SetRunningFlag,
    IfMailboxSetupQueryApInitializes,
    IfMailboxSetupAndApInitializesDisableAllInterrupts,
    IfMailboxSetupAndApInitializesEnableInboxInterrupt,
    IfMailboxSetupAndApInitializesEnableOutboxInterrupt,
    IfMailboxSetupEnableInboxEventSource,
    IfMailboxSetupEnableOutboxEventSource,
    IfMailboxSetupAndApInitializesEnableOutbox,
    IfRequestedSyncIopTimebase,
    IfNormalCreateBootCompletion,
    IfNormalRunCpu,
}

pub(crate) const G17_A7IOP_START_ORDER: [A7IopStartStep; 16] = [
    A7IopStartStep::RtBuddySetPowerState3,
    A7IopStartStep::RequireNotRunning,
    A7IopStartStep::IfNormalEnablePowerLateRequireZero,
    A7IopStartStep::IfMapperFlagSetMapperActive,
    A7IopStartStep::IfFirmwareAndNormalMapFirmware,
    A7IopStartStep::SetRunningFlag,
    A7IopStartStep::IfMailboxSetupQueryApInitializes,
    A7IopStartStep::IfMailboxSetupAndApInitializesDisableAllInterrupts,
    A7IopStartStep::IfMailboxSetupAndApInitializesEnableInboxInterrupt,
    A7IopStartStep::IfMailboxSetupAndApInitializesEnableOutboxInterrupt,
    A7IopStartStep::IfMailboxSetupEnableInboxEventSource,
    A7IopStartStep::IfMailboxSetupEnableOutboxEventSource,
    A7IopStartStep::IfMailboxSetupAndApInitializesEnableOutbox,
    A7IopStartStep::IfRequestedSyncIopTimebase,
    A7IopStartStep::IfNormalCreateBootCompletion,
    A7IopStartStep::IfNormalRunCpu,
];

pub(crate) fn validate_a7iop_start_order(observed: &[A7IopStartStep]) -> bool {
    observed == G17_A7IOP_START_ORDER
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) enum A7IopStopStep {
    RequireRunning,
    IfMailboxSetupDisableOutbox,
    IfMailboxSetupDisableMailboxInterrupts,
    IfWrapperMmioBoundedDisablePowerAttempts,
    RunCpuFalse,
    UnmapFirmware,
    IfMapperFlagClearMapperActive,
    UnlessPowerRetainedTurnOffDomain,
    ClearRunningFlag,
}

/// Owner-invoked `stopCPU(bool, bool)` teardown order.  When wrapper MMIO is
/// enabled, `_disablePower` is attempted at most eleven times (the initial
/// call plus up to ten delayed retries) before the stop sequence continues.
pub(crate) const G17_A7IOP_STOP_ORDER: [A7IopStopStep; 9] = [
    A7IopStopStep::RequireRunning,
    A7IopStopStep::IfMailboxSetupDisableOutbox,
    A7IopStopStep::IfMailboxSetupDisableMailboxInterrupts,
    A7IopStopStep::IfWrapperMmioBoundedDisablePowerAttempts,
    A7IopStopStep::RunCpuFalse,
    A7IopStopStep::UnmapFirmware,
    A7IopStopStep::IfMapperFlagClearMapperActive,
    A7IopStopStep::UnlessPowerRetainedTurnOffDomain,
    A7IopStopStep::ClearRunningFlag,
];

pub(crate) const A7IOP_STOP_DISABLE_POWER_MAX_CALLS: u8 = 11;

pub(crate) fn validate_a7iop_stop_order(observed: &[A7IopStopStep]) -> bool {
    observed == G17_A7IOP_STOP_ORDER
}

/// `AppleA7IOP::resetState()` is software bookkeeping only.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) struct A7IopSoftwareResetState {
    pub(crate) state_offset: u16,
    pub(crate) state_value: u8,
    pub(crate) running_offset: u16,
    pub(crate) completion_offset: u16,
    pub(crate) performs_mmio: bool,
}

pub(crate) const G17_A7IOP_SOFTWARE_RESET_STATE: A7IopSoftwareResetState =
    A7IopSoftwareResetState {
        state_offset: 0x148,
        state_value: 8,
        running_offset: 0x108,
        completion_offset: 0x160,
        performs_mmio: false,
    };

pub(crate) const EXECUTABLE_G17_HARDWARE_RESET_AVAILABLE: bool = false;

/// Host combined-started wait constants recovered from all three AGX hosts.
pub(crate) const G17_HOST_WAIT_QUANTUM_ARG0: u32 = 50;
pub(crate) const G17_HOST_WAIT_QUANTUM_ARG1: u32 = 1_000_000;
pub(crate) const G17_HOST_WAIT_CONTINUE_RESULT: u32 = 1;
pub(crate) const G17_HOST_WAIT_SCALE: u64 = 1_000;
pub(crate) const G17_HOST_WAIT_MINIMUM: u64 = 50;

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) enum HostWaitError {
    CounterOverflow,
}

/// Return the exact host counter limit.  A non-zero diagnostic override
/// replaces the configured value before multiplication and selects the fatal
/// deadline behavior below.
pub(crate) const fn host_wait_counter_limit(
    configured: u64,
    diagnostic_override: u64,
) -> Result<u64, HostWaitError> {
    let selected = if diagnostic_override != 0 {
        diagnostic_override
    } else {
        configured
    };
    match selected.checked_mul(G17_HOST_WAIT_SCALE) {
        Some(value) if value >= G17_HOST_WAIT_MINIMUM => Ok(value),
        Some(_) => Ok(G17_HOST_WAIT_MINIMUM),
        None => Err(HostWaitError::CounterOverflow),
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) enum HostWaitAction {
    Started,
    Continue,
    ReturnFalse,
    FatalTimeout,
}

pub(crate) const fn classify_host_wait_result(
    combined_started: bool,
    event_wait_result: u32,
) -> HostWaitAction {
    if combined_started {
        HostWaitAction::Started
    } else if event_wait_result == G17_HOST_WAIT_CONTINUE_RESULT {
        HostWaitAction::Continue
    } else {
        HostWaitAction::ReturnFalse
    }
}

pub(crate) const fn classify_host_wait_deadline(diagnostic_override: u64) -> HostWaitAction {
    if diagnostic_override == 0 {
        HostWaitAction::ReturnFalse
    } else {
        HostWaitAction::FatalTimeout
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) enum AscBringupPhase {
    Off,
    PowerStateRequested,
    PowerLateAccepted,
    IorvbarProgrammed,
    RunningMarked,
    CpuReleased,
    Failed,
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) enum AscBringupError {
    WrongOrder,
    WrongPowerState,
    EnablePowerLateFailed(u32),
    Iorvbar(IorvbarError),
    RunCpuTraceMismatch,
    WrapperMmioDisabled,
    AlreadyFailed,
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) enum AscRollbackRequirement {
    None,
    /// The AGX host failure paths do not call stopCPU.  An owner must invoke
    /// the separately recovered stop sequence before retrying or releasing
    /// resources.
    OwnerStopCpuRequired,
}

/// Essential, fail-closed ASC launch observer.  It records already-observed
/// actions and permanently fails on an ordering or value mismatch.
#[derive(Debug, Copy, Clone)]
pub(crate) struct G17AscBringup {
    phase: AscBringupPhase,
    mutated: bool,
}

impl G17AscBringup {
    pub(crate) const fn new() -> Self {
        Self {
            phase: AscBringupPhase::Off,
            mutated: false,
        }
    }

    pub(crate) const fn phase(&self) -> AscBringupPhase {
        self.phase
    }

    fn fail(&mut self, error: AscBringupError) -> Result<(), AscBringupError> {
        self.phase = AscBringupPhase::Failed;
        Err(error)
    }

    pub(crate) fn observe_rtbuddy_power_state(&mut self, state: u8) -> Result<(), AscBringupError> {
        if self.phase == AscBringupPhase::Failed {
            return Err(AscBringupError::AlreadyFailed);
        }
        if self.phase != AscBringupPhase::Off {
            return self.fail(AscBringupError::WrongOrder);
        }
        if state != 3 {
            return self.fail(AscBringupError::WrongPowerState);
        }
        self.mutated = true;
        self.phase = AscBringupPhase::PowerStateRequested;
        Ok(())
    }

    pub(crate) fn record_enable_power_late(&mut self, result: u32) -> Result<(), AscBringupError> {
        if self.phase == AscBringupPhase::Failed {
            return Err(AscBringupError::AlreadyFailed);
        }
        if self.phase != AscBringupPhase::PowerStateRequested {
            return self.fail(AscBringupError::WrongOrder);
        }
        if result != 0 {
            return self.fail(AscBringupError::EnablePowerLateFailed(result));
        }
        self.phase = AscBringupPhase::PowerLateAccepted;
        Ok(())
    }

    pub(crate) fn observe_iorvbar_write(
        &mut self,
        firmware_address: u64,
        observed: u64,
    ) -> Result<(), AscBringupError> {
        if self.phase == AscBringupPhase::Failed {
            return Err(AscBringupError::AlreadyFailed);
        }
        if self.phase != AscBringupPhase::PowerLateAccepted {
            return self.fail(AscBringupError::WrongOrder);
        }
        if let Err(e) = validate_iorvbar_write(firmware_address, observed) {
            return self.fail(AscBringupError::Iorvbar(e));
        }
        self.phase = AscBringupPhase::IorvbarProgrammed;
        Ok(())
    }

    pub(crate) fn observe_running_marked(&mut self) -> Result<(), AscBringupError> {
        if self.phase == AscBringupPhase::Failed {
            return Err(AscBringupError::AlreadyFailed);
        }
        if self.phase != AscBringupPhase::IorvbarProgrammed {
            return self.fail(AscBringupError::WrongOrder);
        }
        self.phase = AscBringupPhase::RunningMarked;
        Ok(())
    }

    pub(crate) fn observe_run_cpu_trace(
        &mut self,
        wrapper_mmio_enabled: bool,
        observed: &AscMmioPlan,
    ) -> Result<(), AscBringupError> {
        if self.phase == AscBringupPhase::Failed {
            return Err(AscBringupError::AlreadyFailed);
        }
        if self.phase != AscBringupPhase::RunningMarked {
            return self.fail(AscBringupError::WrongOrder);
        }
        if !wrapper_mmio_enabled {
            return self.fail(AscBringupError::WrapperMmioDisabled);
        }
        if *observed != asc_run_cpu_plan(true, true) {
            return self.fail(AscBringupError::RunCpuTraceMismatch);
        }
        self.phase = AscBringupPhase::CpuReleased;
        Ok(())
    }

    pub(crate) fn record_host_wait_failure(&mut self) {
        if self.phase != AscBringupPhase::Off {
            self.phase = AscBringupPhase::Failed;
        }
    }

    pub(crate) const fn rollback_requirement(&self) -> AscRollbackRequirement {
        if self.mutated {
            AscRollbackRequirement::OwnerStopCpuRequired
        } else {
            AscRollbackRequirement::None
        }
    }
}

/// Named live gaps.  Static recovery of a register transform is not ownership
/// to execute it from Linux.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) struct G17AscLiveGaps {
    pub(crate) pmgr_set_power_state_gate_order_owned: bool,
    pub(crate) asc_control_aperture_owned: bool,
    pub(crate) iorvbar_aperture_and_dma_visibility_owned: bool,
    pub(crate) wrapper_mmio_enable_source_proven: bool,
    pub(crate) stop_cpu_teardown_owner_wired: bool,
}

pub(crate) const G17_ASC_LIVE_GAPS: G17AscLiveGaps = G17AscLiveGaps {
    pmgr_set_power_state_gate_order_owned: false,
    asc_control_aperture_owned: false,
    iorvbar_aperture_and_dma_visibility_owned: false,
    wrapper_mmio_enable_source_proven: false,
    stop_cpu_teardown_owner_wired: false,
};

pub(crate) const EXECUTABLE_G17_ASC_BRINGUP_AVAILABLE: bool = false;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_targets_pin_distinct_text_but_identical_bridge_contract() {
        let p = g17_asc_text_evidence(G17AscEvidenceTarget::G17P);
        let g = g17_asc_text_evidence(G17AscEvidenceTarget::G17G);
        let s = g17_asc_text_evidence(G17AscEvidenceTarget::G17S);
        assert_ne!(p, g);
        assert_ne!(p, s);
        assert_ne!(g, s);
        assert!(G17_ASC_BRIDGE_TARGET_PARITY_PROVEN);
    }

    #[test]
    fn iorvbar_is_address_or_valid_bit_and_rejects_tagged_input() {
        assert_eq!(encode_iorvbar(0x2800_0000).unwrap(), 0x2800_0001);
        assert_eq!(
            encode_iorvbar(0x2800_0001),
            Err(IorvbarError::AddressAlreadyMarkedValid)
        );
        assert_eq!(
            validate_iorvbar_write(0x2800_0000, 0x2800_1001),
            Err(IorvbarError::WrongEncodedValue)
        );
    }

    #[test]
    fn run_and_stop_cpu_mmio_transforms_are_not_symmetric_guesses() {
        let run = asc_run_cpu_plan(true, true);
        let stop = asc_run_cpu_plan(false, true);
        assert_eq!(run.len, 2);
        assert_eq!(stop.len, 4);
        assert_eq!(
            stop.ops[3],
            AscMmioOp::WriteControlClear {
                offset: 0x44,
                bits: 0x20,
            }
        );
        assert_eq!(asc_run_cpu_plan(true, false).len, 0);
    }

    #[test]
    fn start_and_stop_order_validators_reject_reordering() {
        assert!(validate_a7iop_start_order(&G17_A7IOP_START_ORDER));
        let mut wrong_start = G17_A7IOP_START_ORDER;
        wrong_start.swap(4, 5);
        assert!(!validate_a7iop_start_order(&wrong_start));

        assert!(validate_a7iop_stop_order(&G17_A7IOP_STOP_ORDER));
        let mut wrong_stop = G17_A7IOP_STOP_ORDER;
        wrong_stop.swap(4, 5);
        assert!(!validate_a7iop_stop_order(&wrong_stop));
        assert_eq!(A7IOP_STOP_DISABLE_POWER_MAX_CALLS, 11);
    }

    #[test]
    fn reset_state_is_explicitly_software_only() {
        assert_eq!(G17_A7IOP_SOFTWARE_RESET_STATE.state_offset, 0x148);
        assert_eq!(G17_A7IOP_SOFTWARE_RESET_STATE.state_value, 8);
        assert_eq!(G17_A7IOP_SOFTWARE_RESET_STATE.running_offset, 0x108);
        assert_eq!(G17_A7IOP_SOFTWARE_RESET_STATE.completion_offset, 0x160);
        assert!(!G17_A7IOP_SOFTWARE_RESET_STATE.performs_mmio);
        assert!(!EXECUTABLE_G17_HARDWARE_RESET_AVAILABLE);
    }

    #[test]
    fn timeout_budget_floors_checks_overflow_and_bifurcates_deadline() {
        assert_eq!(host_wait_counter_limit(0, 0), Ok(50));
        assert_eq!(G17_HOST_WAIT_QUANTUM_ARG0, 50);
        assert_eq!(G17_HOST_WAIT_QUANTUM_ARG1, 1_000_000);
        assert_eq!(host_wait_counter_limit(2, 0), Ok(2_000));
        assert_eq!(host_wait_counter_limit(2, 7), Ok(7_000));
        assert_eq!(
            host_wait_counter_limit(u64::MAX, 0),
            Err(HostWaitError::CounterOverflow)
        );
        assert_eq!(classify_host_wait_deadline(0), HostWaitAction::ReturnFalse);
        assert_eq!(classify_host_wait_deadline(1), HostWaitAction::FatalTimeout);
        assert_eq!(
            classify_host_wait_result(false, G17_HOST_WAIT_CONTINUE_RESULT),
            HostWaitAction::Continue
        );
        assert_eq!(
            classify_host_wait_result(false, 0),
            HostWaitAction::ReturnFalse
        );
        assert_eq!(classify_host_wait_result(true, 0), HostWaitAction::Started);
    }

    fn advanced_bringup() -> G17AscBringup {
        let mut boot = G17AscBringup::new();
        boot.observe_rtbuddy_power_state(3).unwrap();
        boot.record_enable_power_late(0).unwrap();
        boot.observe_iorvbar_write(0x2800_0000, 0x2800_0001)
            .unwrap();
        boot.observe_running_marked().unwrap();
        boot
    }

    #[test]
    fn bringup_state_machine_requires_exact_order_and_trace() {
        let mut boot = advanced_bringup();
        boot.observe_run_cpu_trace(true, &asc_run_cpu_plan(true, true))
            .unwrap();
        assert_eq!(boot.phase(), AscBringupPhase::CpuReleased);

        let mut wrong = G17AscBringup::new();
        assert_eq!(
            wrong.observe_running_marked(),
            Err(AscBringupError::WrongOrder)
        );
        assert_eq!(wrong.phase(), AscBringupPhase::Failed);
        assert_eq!(
            wrong.observe_rtbuddy_power_state(3),
            Err(AscBringupError::AlreadyFailed)
        );
    }

    #[test]
    fn partial_start_and_timeout_require_owner_teardown_not_reset_state() {
        let mut failed_power = G17AscBringup::new();
        failed_power.observe_rtbuddy_power_state(3).unwrap();
        assert_eq!(
            failed_power.record_enable_power_late(0xe000_02bc),
            Err(AscBringupError::EnablePowerLateFailed(0xe000_02bc))
        );
        assert_eq!(
            failed_power.rollback_requirement(),
            AscRollbackRequirement::OwnerStopCpuRequired
        );

        let mut timed_out = advanced_bringup();
        timed_out.record_host_wait_failure();
        assert_eq!(timed_out.phase(), AscBringupPhase::Failed);
        assert_eq!(
            timed_out.rollback_requirement(),
            AscRollbackRequirement::OwnerStopCpuRequired
        );
        assert!(!G17_A7IOP_SOFTWARE_RESET_STATE.performs_mmio);
    }

    #[test]
    fn wrapper_no_mmio_path_never_opens_cpu_released_gate() {
        let mut boot = advanced_bringup();
        assert_eq!(
            boot.observe_run_cpu_trace(false, &asc_run_cpu_plan(true, false)),
            Err(AscBringupError::WrapperMmioDisabled)
        );
        assert_eq!(boot.phase(), AscBringupPhase::Failed);
    }

    #[test]
    fn every_live_ownership_gate_remains_closed() {
        let gaps = G17_ASC_LIVE_GAPS;
        assert!(!gaps.pmgr_set_power_state_gate_order_owned);
        assert!(!gaps.asc_control_aperture_owned);
        assert!(!gaps.iorvbar_aperture_and_dma_visibility_owned);
        assert!(!gaps.wrapper_mmio_enable_source_proven);
        assert!(!gaps.stop_cpu_teardown_owner_wired);
        assert!(!EXECUTABLE_G17_ASC_BRINGUP_AVAILABLE);
    }
}
