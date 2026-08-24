// SPDX-License-Identifier: GPL-2.0-only OR MIT

#![cfg_attr(not(test), allow(dead_code))]

//! Evidence-backed G15-G17 recovery and G17 internal-power contracts.
//!
//! This module contains only pure validation and host-side recovery writes.
//! It performs no MMIO, mailbox traffic, ASC reset, or firmware-state write.
//! The power-transition grammar and idle-poll behavior are independently
//! byte-confirmed in pinned G17G/G17P/G17S GFX1 images. Only G17S establishes
//! the ordered direct PIO prefix and following firmware operations. The
//! recovery word, host writes, and no-op reset hook are byte-confirmed in
//! pinned G15G/G15S/G15C, G16G/G16X, and G17G/G17S/G17P host drivers. Their
//! shared values are modeled here without reusing target-specific structure
//! offsets (`0x5190` on G15 and G16G, but `0x4ae0` on G16X and G17).

/// The grounded, pure validation model is available.
pub(crate) const GROUNDED_POWER_RECOVERY_MODEL_AVAILABLE: bool = true;

/// Exact targets whose host binaries establish the shared recovery grammar.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) enum RecoveryEvidenceTarget {
    G15G,
    G15S,
    G15C,
    G15D,
    G16G,
    G16S,
    G16C,
    G17P,
    G17G,
    G17S,
}

pub(crate) const RECOVERY_EVIDENCE_TARGETS: [RecoveryEvidenceTarget; 10] = [
    RecoveryEvidenceTarget::G15G,
    RecoveryEvidenceTarget::G15S,
    RecoveryEvidenceTarget::G15C,
    RecoveryEvidenceTarget::G15D,
    RecoveryEvidenceTarget::G16G,
    RecoveryEvidenceTarget::G16S,
    RecoveryEvidenceTarget::G16C,
    RecoveryEvidenceTarget::G17P,
    RecoveryEvidenceTarget::G17G,
    RecoveryEvidenceTarget::G17S,
];

/// G17 GFX1 images establish the internal transition grammar and one idle
/// poll. Only G17S establishes the direct PIO prefix and intervening
/// firmware-operation order. All three targets establish physical type-0
/// mappings. G17G/G17P leave type 0x0b unbound, while G17S installs it in a
/// target override; none of those facts proves Linux MMIO ownership.
pub(crate) const EXECUTABLE_GFX1_POWER_SEQUENCE_AVAILABLE: bool = false;

/// The firmware writes and cross-role 0x22/0x23 protocol remain unproved, so
/// Linux must not execute G17 recovery from this host-write subset.
pub(crate) const EXECUTABLE_G17_RECOVERY_HANDSHAKE_AVAILABLE: bool = false;

/// Pinned G17 host targets whose `halGetPMConfig()` return block is exact.
///
/// The six fields remain semantically opaque. In particular, the G17P block
/// follows its HAL200 implementation rather than the dual-role G17 topology,
/// while the pinned G17G and G17S hosts share the HAL300 block.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) enum G17PmConfigEvidenceTarget {
    G17P,
    G17G,
    G17S,
}

pub(crate) const G17_PM_CONFIG_EVIDENCE_TARGETS: [G17PmConfigEvidenceTarget; 3] = [
    G17PmConfigEvidenceTarget::G17P,
    G17PmConfigEvidenceTarget::G17G,
    G17PmConfigEvidenceTarget::G17S,
];

/// Exact opaque six-u64 blocks returned by the pinned host implementations.
///
/// These constants are diagnostic evidence only. Unknown field semantics and
/// the absence of pinned G17A/G17C host witnesses keep them out of executable
/// power or init-data paths.
pub(crate) const G17_HAL200_PM_CONFIG_OPAQUE: [u64; 6] = [0x4, 0x4, 0x1800, 0x8, 0x8, 0x10];
pub(crate) const G17_HAL300_PM_CONFIG_OPAQUE: [u64; 6] = [0x10, 0x40, 0x1800, 0x8, 0x8, 0x40];

pub(crate) const fn g17_pm_config_for_evidence_target(
    target: G17PmConfigEvidenceTarget,
) -> &'static [u64; 6] {
    match target {
        G17PmConfigEvidenceTarget::G17P => &G17_HAL200_PM_CONFIG_OPAQUE,
        G17PmConfigEvidenceTarget::G17G | G17PmConfigEvidenceTarget::G17S => {
            &G17_HAL300_PM_CONFIG_OPAQUE
        }
    }
}

/// One raw `{ frequency, voltage }` record from the captured T6050 ADT.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) struct T6050PerfState {
    pub(crate) freq_hz: u32,
    pub(crate) volt_mv: u32,
}

/// Exact shape of the captured T6050/G17S main performance-state properties.
pub(crate) const T6050_G17S_PERF_STATE_COUNT: u32 = 14;
pub(crate) const T6050_G17S_PERF_STATE_TABLE_COUNT: u32 = 2;
const T6050_G17S_MAIN_PERF_STATE_RECORDS: usize =
    T6050_G17S_PERF_STATE_COUNT as usize * T6050_G17S_PERF_STATE_TABLE_COUNT as usize;

/// Fail-closed validation errors for the captured T6050 `*1` mirror contract.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) enum T6050PerfStateMirrorError {
    UnexpectedPerfStateCount,
    UnexpectedPerfStateTableCount,
    UnexpectedMainCoreShape,
    UnexpectedMainSramShape,
    UnexpectedCoreMirrorShape,
    UnexpectedSramMirrorShape,
    CoreMirrorMismatch,
    SramMirrorMismatch,
}

/// Validate the exact captured T6050/G17S `perf-states1` mirror relationship.
///
/// `perf-states1` and `perf-states-sram1` duplicate table 1, the second half
/// of their corresponding two-table main properties. This proves neither a
/// consumer nor a third power domain, so this function only validates input.
pub(crate) fn validate_t6050_g17s_perf_state_mirrors(
    perf_state_count: u32,
    perf_state_table_count: u32,
    perf_states: &[T6050PerfState],
    perf_states_sram: &[T6050PerfState],
    perf_states1: &[T6050PerfState],
    perf_states_sram1: &[T6050PerfState],
) -> Result<(), T6050PerfStateMirrorError> {
    if perf_state_count != T6050_G17S_PERF_STATE_COUNT {
        return Err(T6050PerfStateMirrorError::UnexpectedPerfStateCount);
    }
    if perf_state_table_count != T6050_G17S_PERF_STATE_TABLE_COUNT {
        return Err(T6050PerfStateMirrorError::UnexpectedPerfStateTableCount);
    }
    if perf_states.len() != T6050_G17S_MAIN_PERF_STATE_RECORDS {
        return Err(T6050PerfStateMirrorError::UnexpectedMainCoreShape);
    }
    if perf_states_sram.len() != T6050_G17S_MAIN_PERF_STATE_RECORDS {
        return Err(T6050PerfStateMirrorError::UnexpectedMainSramShape);
    }
    if perf_states1.len() != T6050_G17S_PERF_STATE_COUNT as usize {
        return Err(T6050PerfStateMirrorError::UnexpectedCoreMirrorShape);
    }
    if perf_states_sram1.len() != T6050_G17S_PERF_STATE_COUNT as usize {
        return Err(T6050PerfStateMirrorError::UnexpectedSramMirrorShape);
    }

    let table1_start = T6050_G17S_PERF_STATE_COUNT as usize;
    if perf_states1 != &perf_states[table1_start..] {
        return Err(T6050PerfStateMirrorError::CoreMirrorMismatch);
    }
    if perf_states_sram1 != &perf_states_sram[table1_start..] {
        return Err(T6050PerfStateMirrorError::SramMirrorMismatch);
    }

    Ok(())
}

/// G17 GFX1 internal power states whose numeric names are string-confirmed.
pub(crate) const GFX1_POWER_SLEEP: u8 = 0x08;
pub(crate) const GFX1_POWER_NAP: u8 = 0x10;

/// Mandatory intermediate state between SLEEP and NAP.
///
/// Its private Apple enum name is not known, so the numeric value is kept
/// without assigning a semantic name.
pub(crate) const GFX1_POWER_INTERMEDIATE_21: u8 = 0x21;

/// G17 targets with independently pinned GFX1 transition and idle-poll bytes.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) enum G17Gfx1PowerEvidenceTarget {
    G17P,
    G17G,
    G17S,
}

pub(crate) const G17_GFX1_POWER_EVIDENCE_TARGETS: [G17Gfx1PowerEvidenceTarget; 3] = [
    G17Gfx1PowerEvidenceTarget::G17P,
    G17Gfx1PowerEvidenceTarget::G17G,
    G17Gfx1PowerEvidenceTarget::G17S,
];

/// Exact target-specific call site for the shared pre-idle CAS helper.
///
/// The three helper bodies have the same 53-instruction normalized dataflow:
/// their only body-local atomic access compares `1` and replaces it with `0`
/// at the passed firmware-global object. G17S executes its direct PIO prefix
/// before the same helper, while G17G/G17P reach the helper without that
/// prefix. Thus the helper cannot account for the missing target PIO writes.
/// Its private name and the ownership of its scheduler slow path stay opaque.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) struct G17Gfx1PreIdleEntryEvidence {
    pub(crate) helper_text_offset: u32,
    pub(crate) helper_object_vm_offset: u32,
    pub(crate) direct_pio_prefix_before_helper: bool,
}

pub(crate) const G17_GFX1_PRE_IDLE_HELPER_WINDOW_SIZE: u32 = 0xd4;
pub(crate) const G17_GFX1_PRE_IDLE_HELPER_INSTRUCTION_COUNT: u32 = 53;
pub(crate) const G17_GFX1_PRE_IDLE_HELPER_CAS_COMPARE: u64 = 1;
pub(crate) const G17_GFX1_PRE_IDLE_HELPER_CAS_REPLACEMENT: u64 = 0;
/// No helper body directly resolves or accesses a PIO descriptor.
pub(crate) const G17_GFX1_PRE_IDLE_HELPER_CONTAINS_DIRECT_PIO_ACCESS: bool = false;

pub(crate) const fn g17_gfx1_pre_idle_entry_evidence(
    target: G17Gfx1PowerEvidenceTarget,
) -> G17Gfx1PreIdleEntryEvidence {
    match target {
        G17Gfx1PowerEvidenceTarget::G17P => G17Gfx1PreIdleEntryEvidence {
            helper_text_offset: 0x0003_5f5c,
            helper_object_vm_offset: 0x000f_90f0,
            direct_pio_prefix_before_helper: false,
        },
        G17Gfx1PowerEvidenceTarget::G17G => G17Gfx1PreIdleEntryEvidence {
            helper_text_offset: 0x0003_5db4,
            helper_object_vm_offset: 0x000f_3c30,
            direct_pio_prefix_before_helper: false,
        },
        G17Gfx1PowerEvidenceTarget::G17S => G17Gfx1PreIdleEntryEvidence {
            helper_text_offset: 0x0003_b1d8,
            helper_object_vm_offset: 0x0010_0df0,
            direct_pio_prefix_before_helper: true,
        },
    }
}

/// Semantic G17 PIO types used by the pinned G17S GFX1 transition callback.
///
/// The firmware's 52-entry name/type table and type-indexed 0x28-byte PIO
/// descriptor copier directly pair these names and numeric values. Independently
/// pinned G17G/G17P/G17S host mappings are modeled below.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
#[repr(u8)]
pub(crate) enum G17PioSlot {
    GpcAicSwInt = 0x00,
    PushTelemetryDashboardConfigSpace = 0x0b,
    FenderRegsMcw = 0x24,
}

/// Exact PIO descriptor layout used by the pinned firmware's type copier.
pub(crate) const G17S_PIO_DESCRIPTOR_SIZE: u32 = 0x28;
pub(crate) const G17S_PIO_DESCRIPTOR_POINTER_OFFSET: u32 = 0x08;

/// Offset of one type's descriptor from the firmware PIO descriptor mirror.
pub(crate) const fn g17s_pio_descriptor_offset(slot: G17PioSlot) -> u32 {
    slot as u32 * G17S_PIO_DESCRIPTOR_SIZE
}

/// Offset of the callback's mapped-pointer field from the descriptor mirror.
pub(crate) const fn g17s_pio_mapped_pointer_offset(slot: G17PioSlot) -> u32 {
    g17s_pio_descriptor_offset(slot) + G17S_PIO_DESCRIPTOR_POINTER_OFFSET
}

/// One exact 32-bit PIO operation in firmware execution order.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) enum G17PioOp {
    Write {
        slot: G17PioSlot,
        offset: u32,
        value: u32,
    },
    SetBits {
        slot: G17PioSlot,
        offset: u32,
        mask: u32,
    },
}

/// Ordered PIO prefix for G17S GFX1 `0x21 -> SLEEP`.
///
/// Firmware-internal synchronization and state checks occur after this prefix
/// and before the idle poll. Consequently this is not an executable sequence.
pub(crate) const G17S_GFX1_21_TO_SLEEP_PIO_PREFIX: [G17PioOp; 2] = [
    G17PioOp::Write {
        slot: G17PioSlot::GpcAicSwInt,
        offset: 0x430,
        value: 0x0800_0000,
    },
    G17PioOp::Write {
        slot: G17PioSlot::PushTelemetryDashboardConfigSpace,
        offset: 0x354,
        value: 0x0020_0000,
    },
];

/// Ordered PIO prefix for G17S GFX1 `SLEEP/NAP -> 0x21`.
pub(crate) const G17S_GFX1_LOW_POWER_TO_21_PIO_PREFIX: [G17PioOp; 3] = [
    G17PioOp::Write {
        slot: G17PioSlot::GpcAicSwInt,
        offset: 0x630,
        value: 0x0800_0000,
    },
    G17PioOp::SetBits {
        slot: G17PioSlot::PushTelemetryDashboardConfigSpace,
        offset: 0x758,
        mask: 0x0000_0b00,
    },
    G17PioOp::Write {
        slot: G17PioSlot::PushTelemetryDashboardConfigSpace,
        offset: 0x254,
        value: 0x0020_0000,
    },
];

/// Compatibility alias retained for the original, reversed direction name.
pub(crate) const G17S_GFX1_SLEEP_TO_21_PIO_PREFIX: [G17PioOp; 2] =
    G17S_GFX1_21_TO_SLEEP_PIO_PREFIX;
/// Compatibility alias retained for the original, reversed direction name.
pub(crate) const G17S_GFX1_21_TO_LOW_POWER_PIO_PREFIX: [G17PioOp; 3] =
    G17S_GFX1_LOW_POWER_TO_21_PIO_PREFIX;

/// Validate only the exact observed `0x21 -> SLEEP` PIO prefix.
pub(crate) fn validate_g17s_gfx1_21_to_sleep_pio_prefix(ops: &[G17PioOp]) -> bool {
    ops == G17S_GFX1_21_TO_SLEEP_PIO_PREFIX
}

/// Validate only the exact observed `SLEEP/NAP -> 0x21` PIO prefix.
pub(crate) fn validate_g17s_gfx1_low_power_to_21_pio_prefix(ops: &[G17PioOp]) -> bool {
    ops == G17S_GFX1_LOW_POWER_TO_21_PIO_PREFIX
}

/// Compatibility wrapper retained for the original, reversed direction name.
pub(crate) fn validate_g17s_gfx1_sleep_to_21_pio_prefix(ops: &[G17PioOp]) -> bool {
    validate_g17s_gfx1_21_to_sleep_pio_prefix(ops)
}

/// Compatibility wrapper retained for the original, reversed direction name.
pub(crate) fn validate_g17s_gfx1_21_to_low_power_pio_prefix(ops: &[G17PioOp]) -> bool {
    validate_g17s_gfx1_low_power_to_21_pio_prefix(ops)
}

/// One exact firmware-internal operation between the G17S PIO writes and poll.
///
/// Addresses are offsets from the firmware text or global-data virtual base.
/// Helper calls deliberately remain opaque: their machine-code targets and
/// object addresses are proven, but Linux ownership and equivalent semantics
/// are not.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) enum G17SFirmwareOp {
    OpaqueHelperCall {
        helper_text_offset: u32,
        object_global_offset: u32,
    },
    AtomicAdd32 {
        global_offset: u32,
        addend: i32,
    },
    RejectBothNonZero32 {
        root_global_offset: u32,
        pointer_offset: u32,
        field_offset: u32,
        global_offset: u32,
    },
    AtomicSetBits64 {
        global_offset: u32,
        mask: u64,
    },
}

/// Ordered G17S operations after the `0x21 -> SLEEP` PIO writes and before the
/// type-0x24 idle poll.
pub(crate) const G17S_GFX1_PRE_IDLE_FIRMWARE_OPS: [G17SFirmwareOp; 6] = [
    G17SFirmwareOp::OpaqueHelperCall {
        helper_text_offset: 0x0003_b1d8,
        object_global_offset: 0x0000_0df0,
    },
    G17SFirmwareOp::AtomicAdd32 {
        global_offset: 0x0000_21e0,
        addend: -1,
    },
    G17SFirmwareOp::RejectBothNonZero32 {
        root_global_offset: 0x0000_0e08,
        pointer_offset: 0x481,
        field_offset: 0x38,
        global_offset: 0x0000_2d10,
    },
    G17SFirmwareOp::AtomicSetBits64 {
        global_offset: 0x0000_11a0,
        mask: 0x0800_0000,
    },
    G17SFirmwareOp::OpaqueHelperCall {
        helper_text_offset: 0x0003_a8e8,
        object_global_offset: 0x0000_11a0,
    },
    G17SFirmwareOp::OpaqueHelperCall {
        helper_text_offset: 0x0003_b1d8,
        object_global_offset: 0x0000_0dd8,
    },
];

/// Validate only the byte-confirmed G17S pre-idle firmware-operation order.
pub(crate) fn validate_g17s_gfx1_pre_idle_firmware_ops(ops: &[G17SFirmwareOp]) -> bool {
    ops == G17S_GFX1_PRE_IDLE_FIRMWARE_OPS
}

/// G17 host targets with the byte-identical slot-0x24 relative-map record.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) enum G17PioMapEvidenceTarget {
    G17P,
    G17G,
    G17S,
}

pub(crate) const G17_PIO_SLOT_24_MAP_EVIDENCE_TARGETS: [G17PioMapEvidenceTarget; 3] = [
    G17PioMapEvidenceTarget::G17P,
    G17PioMapEvidenceTarget::G17G,
    G17PioMapEvidenceTarget::G17S,
];

/// Exact common slot-0x24 relative mapping; the complete family tables differ.
pub(crate) const G17_PIO_SLOT_24_RELATIVE_OFFSET: u32 = 0x00d4_0000;
pub(crate) const G17_PIO_SLOT_24_SIZE: u32 = 0x4000;

/// Complete ordered type list of the pinned G17S 19-record relative table.
///
/// Type 0x0b is absent; G17S installs that descriptor later in its target
/// configure override instead of obtaining it from the SGX-relative table.
pub(crate) const G17S_HOST_PIO_RELATIVE_TABLE_TYPES: [u8; 19] = [
    0x11, 0x35, 0x1f, 0x22, 0x24, 0x26, 0x27, 0x21, 0x25, 0x28, 0x2a, 0x30, 0x14, 0x15,
    0x12, 0x13, 0x18, 0x17, 0x32,
];

/// G17 targets whose physical SGX and host-created PIO bindings are pinned.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) enum G17HostPioPhysicalEvidenceTarget {
    G17P,
    G17G,
    G17S,
}

pub(crate) const G17_HOST_PIO_PHYSICAL_EVIDENCE_TARGETS: [G17HostPioPhysicalEvidenceTarget; 3] =
    [
        G17HostPioPhysicalEvidenceTarget::G17P,
        G17HostPioPhysicalEvidenceTarget::G17G,
        G17HostPioPhysicalEvidenceTarget::G17S,
    ];

/// Exact opaque `meta-sw-interrupt` tuple copied by the pinned host.
///
/// Only the first u64 has a proven use here: the host aligns it down to the
/// host I/O page size and installs it as PIO type 0. The second u64 and
/// trailing u32 deliberately remain unnamed.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) struct G17MetaSwInterruptEvidence {
    pub(crate) first_u64: u64,
    pub(crate) second_u64: u64,
    pub(crate) trailing_u32: u32,
}

/// Source from which the pinned host constructs one physical PIO binding.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) enum G17HostPioBindingSource {
    MetaSwInterrupt,
    SgxRelativeOffsetTable,
    TargetConfigureConstant,
}

/// One exact physical binding created by the pinned macOS host.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) struct G17HostPioPhysicalBinding {
    pub(crate) phys_base: u64,
    pub(crate) size: u32,
    pub(crate) source: G17HostPioBindingSource,
}

/// Exact absolute PIO assignments made by a target configure override.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) struct G17HostPioTargetConfigureEvidence {
    pub(crate) slot_0_phys_base: u64,
    pub(crate) slot_0b_phys_base: u64,
    pub(crate) slot_size: u32,
}

/// Exact target input used to derive the pinned host's physical PIO map.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) struct G17HostPioPhysicalEvidence {
    pub(crate) sgx_phys_base: u64,
    pub(crate) host_io_page_size: u32,
    pub(crate) meta_sw_interrupt: G17MetaSwInterruptEvidence,
    pub(crate) target_configure: Option<G17HostPioTargetConfigureEvidence>,
}

/// The macOS host constructs these mappings, but Linux ownership is unproved.
pub(crate) const G17_HOST_PIO_LINUX_MMIO_OWNERSHIP_PROVEN: bool = false;

pub(crate) const fn g17_host_pio_physical_evidence(
    target: G17HostPioPhysicalEvidenceTarget,
) -> G17HostPioPhysicalEvidence {
    match target {
        G17HostPioPhysicalEvidenceTarget::G17P => G17HostPioPhysicalEvidence {
            sgx_phys_base: 0x2_7000_0000,
            host_io_page_size: 0x4000,
            meta_sw_interrupt: G17MetaSwInterruptEvidence {
                first_u64: 0x3_0101_4048,
                second_u64: 0x3_0101_4248,
                trailing_u32: 0x4000,
            },
            target_configure: None,
        },
        G17HostPioPhysicalEvidenceTarget::G17G => G17HostPioPhysicalEvidence {
            sgx_phys_base: 0x3_7000_0000,
            host_io_page_size: 0x4000,
            meta_sw_interrupt: G17MetaSwInterruptEvidence {
                first_u64: 0x3_8101_4048,
                second_u64: 0x3_8101_4248,
                trailing_u32: 0x4000,
            },
            target_configure: None,
        },
        G17HostPioPhysicalEvidenceTarget::G17S => G17HostPioPhysicalEvidence {
            sgx_phys_base: G17S_CAPTURED_SGX_PHYS_BASE,
            host_io_page_size: 0x4000,
            meta_sw_interrupt: G17MetaSwInterruptEvidence {
                first_u64: 0x2_8041_4084,
                second_u64: 0x2_8041_4284,
                trailing_u32: 0x80,
            },
            target_configure: Some(G17HostPioTargetConfigureEvidence {
                slot_0_phys_base: 0x2_8041_4000,
                slot_0b_phys_base: 0x2_8425_8000,
                slot_size: 0x4000,
            }),
        },
    }
}

/// Derive only the physical bindings established by the pinned target host.
///
/// G17G/G17P type 0 comes from `meta-sw-interrupt`; the G17S generic path does
/// the same, then its target override reasserts the same aligned address as an
/// absolute constant. Type 0x24 comes from the target SGX base plus its exact
/// relative-table record. Type 0x0b remains `None` on G17G/G17P, but the G17S
/// target override installs its exact absolute address after the 19-record
/// relative table omits it.
pub(crate) const fn g17_host_pio_physical_binding(
    target: G17HostPioPhysicalEvidenceTarget,
    slot: G17PioSlot,
) -> Option<G17HostPioPhysicalBinding> {
    let evidence = g17_host_pio_physical_evidence(target);

    match slot {
        G17PioSlot::GpcAicSwInt => match evidence.target_configure {
            Some(target_configure) => Some(G17HostPioPhysicalBinding {
                phys_base: target_configure.slot_0_phys_base,
                size: target_configure.slot_size,
                source: G17HostPioBindingSource::TargetConfigureConstant,
            }),
            None => Some(G17HostPioPhysicalBinding {
                phys_base: evidence.meta_sw_interrupt.first_u64
                    & !((evidence.host_io_page_size as u64) - 1),
                size: evidence.host_io_page_size,
                source: G17HostPioBindingSource::MetaSwInterrupt,
            }),
        },
        G17PioSlot::PushTelemetryDashboardConfigSpace => match evidence.target_configure {
            Some(target_configure) => Some(G17HostPioPhysicalBinding {
                phys_base: target_configure.slot_0b_phys_base,
                size: target_configure.slot_size,
                source: G17HostPioBindingSource::TargetConfigureConstant,
            }),
            None => None,
        },
        G17PioSlot::FenderRegsMcw => Some(G17HostPioPhysicalBinding {
            phys_base: evidence.sgx_phys_base + G17_PIO_SLOT_24_RELATIVE_OFFSET as u64,
            size: G17_PIO_SLOT_24_SIZE,
            source: G17HostPioBindingSource::SgxRelativeOffsetTable,
        }),
    }
}

/// Validate an observed binding, including the proven absence of type 0x0b.
pub(crate) fn validate_g17_host_pio_physical_binding(
    target: G17HostPioPhysicalEvidenceTarget,
    slot: G17PioSlot,
    binding: Option<G17HostPioPhysicalBinding>,
) -> bool {
    binding == g17_host_pio_physical_binding(target, slot)
}

/// Captured T6050/J714s SGX base and the derived G17S poll address.
///
/// G17G/G17P physical bases are deliberately not inferred from this G17S ADT.
pub(crate) const G17S_CAPTURED_SGX_PHYS_BASE: u64 = 0x21_0000_0000;
pub(crate) const G17S_GFX1_IDLE_STATUS_PHYS: u64 =
    G17S_CAPTURED_SGX_PHYS_BASE + G17_PIO_SLOT_24_RELATIVE_OFFSET as u64;

/// Exact common G17 GFX1 idle-poll fields.
pub(crate) const G17_GFX1_IDLE_POLL_MASK: u32 = 1;
pub(crate) const G17_GFX1_IDLE_POLL_EXPECTED: u32 = 1;
pub(crate) const G17_GFX1_IDLE_POLL_TIMEOUT_TICKS: u32 = 0x1770;
/// Maximum printed by the pinned firmware for the `0x1770`-tick deadline.
pub(crate) const G17_GFX1_IDLE_POLL_REPORTED_MAX_US: u32 = 250;

/// G17S-compatible names retained for the captured physical-address witness.
pub(crate) const G17S_GFX1_IDLE_POLL_MASK: u32 = G17_GFX1_IDLE_POLL_MASK;
pub(crate) const G17S_GFX1_IDLE_POLL_EXPECTED: u32 = G17_GFX1_IDLE_POLL_EXPECTED;
pub(crate) const G17S_GFX1_IDLE_POLL_TIMEOUT_TICKS: u32 = G17_GFX1_IDLE_POLL_TIMEOUT_TICKS;
pub(crate) const G17S_GFX1_IDLE_POLL_REPORTED_MAX_US: u32 = G17_GFX1_IDLE_POLL_REPORTED_MAX_US;

/// Apply the pinned G17 firmware's timebase-delta reporting conversion.
///
/// This is evidence for its diagnostic output, not a Linux timeout source.
pub(crate) const fn g17_gfx1_report_timebase_delta_us(delta_ticks: u32) -> u32 {
    ((delta_ticks as u64 * 0xaaaa_aaab) >> 36) as u32
}

/// Apply the shared conversion to the original G17S witness.
pub(crate) const fn g17s_gfx1_report_timebase_delta_us(delta_ticks: u32) -> u32 {
    g17_gfx1_report_timebase_delta_us(delta_ticks)
}

/// Test the exact success predicate used by the common G17 GFX1 idle poll.
pub(crate) const fn g17_gfx1_idle_poll_satisfied(
    _target: G17Gfx1PowerEvidenceTarget,
    status: u32,
) -> bool {
    status & G17_GFX1_IDLE_POLL_MASK == G17_GFX1_IDLE_POLL_EXPECTED
}

/// Test the G17S witness through the common G17 idle-poll predicate.
pub(crate) const fn g17s_gfx1_idle_poll_satisfied(status: u32) -> bool {
    g17_gfx1_idle_poll_satisfied(G17Gfx1PowerEvidenceTarget::G17S, status)
}

/// Fail-closed result for the bounded G17 GFX1 transition grammar.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) enum PowerTransitionError {
    /// Direct SLEEP-to-NAP and NAP-to-SLEEP paths reach firmware aborts.
    DirectSleepNapIsFatal,
    /// No other edge was established by the pinned transition callback.
    UnprovenTransition,
}

/// Validate only the four independently proven G17 bridge edges.
///
/// Same-state requests and unknown values are deliberately rejected. This
/// proves the transition grammar, not target helper equivalence or ownership.
pub(crate) const fn validate_g17_gfx1_power_transition(
    _target: G17Gfx1PowerEvidenceTarget,
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

/// Validate the G17S witness through the common G17 transition grammar.
pub(crate) const fn validate_g17s_gfx1_power_transition(
    old: u8,
    new: u8,
) -> Result<(), PowerTransitionError> {
    validate_g17_gfx1_power_transition(G17Gfx1PowerEvidenceTarget::G17S, old, new)
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

/// G15/G16 host targets with an independently pinned `+0x5190` recovery word.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) enum G15G16RecoveryLayoutEvidenceTarget {
    G15G,
    G15S,
    G15C,
    G15D,
    G16G,
    G16S,
    G16C,
}

pub(crate) const G15_G16_RECOVERY_LAYOUT_EVIDENCE_TARGETS:
    [G15G16RecoveryLayoutEvidenceTarget; 7] = [
        G15G16RecoveryLayoutEvidenceTarget::G15G,
        G15G16RecoveryLayoutEvidenceTarget::G15S,
        G15G16RecoveryLayoutEvidenceTarget::G15C,
        G15G16RecoveryLayoutEvidenceTarget::G15D,
        G15G16RecoveryLayoutEvidenceTarget::G16G,
        G15G16RecoveryLayoutEvidenceTarget::G16S,
        G15G16RecoveryLayoutEvidenceTarget::G16C,
    ];

/// Offset from the mapped firmware-state base, not from the host object.
pub(crate) const G15_AND_G16G_RECOVERY_WORD_OFFSET: u32 = 0x5190;

/// G16X uses the later mapped-state location for both proven target variants.
pub(crate) const G16X_RECOVERY_WORD_OFFSET: u32 = 0x4ae0;

pub(crate) const fn g15_g16_recovery_word_offset(
    target: G15G16RecoveryLayoutEvidenceTarget,
) -> u32 {
    match target {
        G15G16RecoveryLayoutEvidenceTarget::G15G
        | G15G16RecoveryLayoutEvidenceTarget::G15S
        | G15G16RecoveryLayoutEvidenceTarget::G15C
        | G15G16RecoveryLayoutEvidenceTarget::G15D
        | G15G16RecoveryLayoutEvidenceTarget::G16G => G15_AND_G16G_RECOVERY_WORD_OFFSET,
        G15G16RecoveryLayoutEvidenceTarget::G16S
        | G15G16RecoveryLayoutEvidenceTarget::G16C => G16X_RECOVERY_WORD_OFFSET,
    }
}

/// G17 host targets with an independently pinned `+0x4ae0` recovery word.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) enum G17RecoveryLayoutEvidenceTarget {
    G17P,
    G17G,
    G17S,
}

pub(crate) const G17_RECOVERY_LAYOUT_EVIDENCE_TARGETS: [G17RecoveryLayoutEvidenceTarget; 3] = [
    G17RecoveryLayoutEvidenceTarget::G17P,
    G17RecoveryLayoutEvidenceTarget::G17G,
    G17RecoveryLayoutEvidenceTarget::G17S,
];

/// Offset from the mapped firmware-state base, not from the host object.
pub(crate) const G17_RECOVERY_WORD_OFFSET: u32 = 0x4ae0;

pub(crate) const fn g17_recovery_word_offset(
    target: G17RecoveryLayoutEvidenceTarget,
) -> u32 {
    match target {
        G17RecoveryLayoutEvidenceTarget::G17P
        | G17RecoveryLayoutEvidenceTarget::G17G
        | G17RecoveryLayoutEvidenceTarget::G17S => G17_RECOVERY_WORD_OFFSET,
    }
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
    UnexpectedWordOffset,
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

/// Decode a G15/G16 recovery word only at the exact target-backed location.
pub(crate) const fn decode_g15_g16_recovery_word_at_offset(
    target: G15G16RecoveryLayoutEvidenceTarget,
    observed_offset: u32,
    raw: u32,
) -> Result<RecoveryWordState, RecoveryError> {
    if observed_offset != g15_g16_recovery_word_offset(target) {
        return Err(RecoveryError::UnexpectedWordOffset);
    }

    decode_recovery_word(raw)
}

/// Decode a G17 recovery word only at the exact target-backed location.
pub(crate) const fn decode_g17_recovery_word_at_offset(
    target: G17RecoveryLayoutEvidenceTarget,
    observed_offset: u32,
    raw: u32,
) -> Result<RecoveryWordState, RecoveryError> {
    if observed_offset != g17_recovery_word_offset(target) {
        return Err(RecoveryError::UnexpectedWordOffset);
    }

    decode_recovery_word(raw)
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
    fn g17_pre_idle_helper_is_shared_but_direct_pio_prefix_is_not() {
        assert_eq!(
            G17_GFX1_POWER_EVIDENCE_TARGETS,
            [
                G17Gfx1PowerEvidenceTarget::G17P,
                G17Gfx1PowerEvidenceTarget::G17G,
                G17Gfx1PowerEvidenceTarget::G17S,
            ]
        );
        assert_eq!(
            g17_gfx1_pre_idle_entry_evidence(G17Gfx1PowerEvidenceTarget::G17P),
            G17Gfx1PreIdleEntryEvidence {
                helper_text_offset: 0x0003_5f5c,
                helper_object_vm_offset: 0x000f_90f0,
                direct_pio_prefix_before_helper: false,
            }
        );
        assert_eq!(
            g17_gfx1_pre_idle_entry_evidence(G17Gfx1PowerEvidenceTarget::G17G),
            G17Gfx1PreIdleEntryEvidence {
                helper_text_offset: 0x0003_5db4,
                helper_object_vm_offset: 0x000f_3c30,
                direct_pio_prefix_before_helper: false,
            }
        );
        assert_eq!(
            g17_gfx1_pre_idle_entry_evidence(G17Gfx1PowerEvidenceTarget::G17S),
            G17Gfx1PreIdleEntryEvidence {
                helper_text_offset: 0x0003_b1d8,
                helper_object_vm_offset: 0x0010_0df0,
                direct_pio_prefix_before_helper: true,
            }
        );
        assert_eq!(G17_GFX1_PRE_IDLE_HELPER_WINDOW_SIZE, 0xd4);
        assert_eq!(G17_GFX1_PRE_IDLE_HELPER_INSTRUCTION_COUNT, 53);
        assert_eq!(G17_GFX1_PRE_IDLE_HELPER_CAS_COMPARE, 1);
        assert_eq!(G17_GFX1_PRE_IDLE_HELPER_CAS_REPLACEMENT, 0);
        assert!(!G17_GFX1_PRE_IDLE_HELPER_CONTAINS_DIRECT_PIO_ACCESS);

        let valid_edges = [
            (GFX1_POWER_SLEEP, GFX1_POWER_INTERMEDIATE_21),
            (GFX1_POWER_NAP, GFX1_POWER_INTERMEDIATE_21),
            (GFX1_POWER_INTERMEDIATE_21, GFX1_POWER_SLEEP),
            (GFX1_POWER_INTERMEDIATE_21, GFX1_POWER_NAP),
        ];
        for target in G17_GFX1_POWER_EVIDENCE_TARGETS {
            for edge in valid_edges {
                assert_eq!(
                    validate_g17_gfx1_power_transition(target, edge.0, edge.1),
                    Ok(())
                );
            }
            assert_eq!(
                validate_g17_gfx1_power_transition(
                    target,
                    GFX1_POWER_SLEEP,
                    GFX1_POWER_NAP,
                ),
                Err(PowerTransitionError::DirectSleepNapIsFatal)
            );
            assert_eq!(
                validate_g17_gfx1_power_transition(
                    target,
                    GFX1_POWER_INTERMEDIATE_21,
                    GFX1_POWER_INTERMEDIATE_21,
                ),
                Err(PowerTransitionError::UnprovenTransition)
            );
            assert!(!g17_gfx1_idle_poll_satisfied(target, 0));
            assert!(g17_gfx1_idle_poll_satisfied(target, 1));
            assert!(!g17_gfx1_idle_poll_satisfied(target, 2));
            assert!(g17_gfx1_idle_poll_satisfied(target, u32::MAX));
        }

        assert_eq!(G17_GFX1_IDLE_POLL_MASK, 1);
        assert_eq!(G17_GFX1_IDLE_POLL_EXPECTED, 1);
        assert_eq!(G17_GFX1_IDLE_POLL_TIMEOUT_TICKS, 0x1770);
        assert_eq!(G17_GFX1_IDLE_POLL_REPORTED_MAX_US, 250);
        assert_eq!(
            g17_gfx1_report_timebase_delta_us(G17_GFX1_IDLE_POLL_TIMEOUT_TICKS),
            G17_GFX1_IDLE_POLL_REPORTED_MAX_US
        );
        assert!(!EXECUTABLE_GFX1_POWER_SEQUENCE_AVAILABLE);
    }

    #[test]
    fn g17s_power_pio_prefixes_require_exact_direction_order_and_kind() {
        assert!(validate_g17s_gfx1_21_to_sleep_pio_prefix(
            &G17S_GFX1_21_TO_SLEEP_PIO_PREFIX
        ));
        assert!(validate_g17s_gfx1_low_power_to_21_pio_prefix(
            &G17S_GFX1_LOW_POWER_TO_21_PIO_PREFIX
        ));
        assert_eq!(
            G17S_GFX1_SLEEP_TO_21_PIO_PREFIX,
            G17S_GFX1_21_TO_SLEEP_PIO_PREFIX
        );
        assert_eq!(
            G17S_GFX1_21_TO_LOW_POWER_PIO_PREFIX,
            G17S_GFX1_LOW_POWER_TO_21_PIO_PREFIX
        );
        assert!(validate_g17s_gfx1_sleep_to_21_pio_prefix(
            &G17S_GFX1_21_TO_SLEEP_PIO_PREFIX
        ));
        assert!(validate_g17s_gfx1_21_to_low_power_pio_prefix(
            &G17S_GFX1_LOW_POWER_TO_21_PIO_PREFIX
        ));

        let mut wrong_order = G17S_GFX1_21_TO_SLEEP_PIO_PREFIX;
        wrong_order.swap(0, 1);
        assert!(!validate_g17s_gfx1_21_to_sleep_pio_prefix(&wrong_order));

        let mut wrong_value = G17S_GFX1_21_TO_SLEEP_PIO_PREFIX;
        wrong_value[1] = G17PioOp::Write {
            slot: G17PioSlot::PushTelemetryDashboardConfigSpace,
            offset: 0x354,
            value: 0x0020_0001,
        };
        assert!(!validate_g17s_gfx1_21_to_sleep_pio_prefix(&wrong_value));

        let mut wrong_kind = G17S_GFX1_LOW_POWER_TO_21_PIO_PREFIX;
        wrong_kind[1] = G17PioOp::Write {
            slot: G17PioSlot::PushTelemetryDashboardConfigSpace,
            offset: 0x758,
            value: 0x0000_0b00,
        };
        assert!(!validate_g17s_gfx1_low_power_to_21_pio_prefix(&wrong_kind));
        assert!(!validate_g17s_gfx1_21_to_sleep_pio_prefix(&[]));
    }

    #[test]
    fn g17s_pio_type_names_and_descriptor_indices_are_exact() {
        assert_eq!(G17PioSlot::GpcAicSwInt as u8, 0x00);
        assert_eq!(
            G17PioSlot::PushTelemetryDashboardConfigSpace as u8,
            0x0b
        );
        assert_eq!(G17PioSlot::FenderRegsMcw as u8, 0x24);
        assert_eq!(G17S_PIO_DESCRIPTOR_SIZE, 0x28);
        assert_eq!(G17S_PIO_DESCRIPTOR_POINTER_OFFSET, 8);
        assert_eq!(
            g17s_pio_mapped_pointer_offset(G17PioSlot::GpcAicSwInt),
            0x008
        );
        assert_eq!(
            g17s_pio_mapped_pointer_offset(
                G17PioSlot::PushTelemetryDashboardConfigSpace
            ),
            0x1c0
        );
        assert_eq!(
            g17s_pio_mapped_pointer_offset(G17PioSlot::FenderRegsMcw),
            0x5a8
        );
    }

    #[test]
    fn g17s_pre_idle_firmware_ops_require_exact_order_and_width() {
        assert!(validate_g17s_gfx1_pre_idle_firmware_ops(
            &G17S_GFX1_PRE_IDLE_FIRMWARE_OPS
        ));

        let mut wrong_order = G17S_GFX1_PRE_IDLE_FIRMWARE_OPS;
        wrong_order.swap(0, 1);
        assert!(!validate_g17s_gfx1_pre_idle_firmware_ops(&wrong_order));

        let mut wrong_width = G17S_GFX1_PRE_IDLE_FIRMWARE_OPS;
        wrong_width[3] = G17SFirmwareOp::AtomicAdd32 {
            global_offset: 0x0000_11a0,
            addend: 0x0800_0000,
        };
        assert!(!validate_g17s_gfx1_pre_idle_firmware_ops(&wrong_width));

        let mut wrong_helper_object = G17S_GFX1_PRE_IDLE_FIRMWARE_OPS;
        wrong_helper_object[5] = G17SFirmwareOp::OpaqueHelperCall {
            helper_text_offset: 0x0003_b1d8,
            object_global_offset: 0x0000_0df0,
        };
        assert!(!validate_g17s_gfx1_pre_idle_firmware_ops(
            &wrong_helper_object
        ));
        assert!(!validate_g17s_gfx1_pre_idle_firmware_ops(&[]));
    }

    #[test]
    fn g17s_idle_poll_has_exact_map_predicate_and_reported_deadline() {
        assert_eq!(G17PioSlot::FenderRegsMcw as u8, 0x24);
        assert_eq!(G17_PIO_SLOT_24_RELATIVE_OFFSET, 0x00d4_0000);
        assert_eq!(G17_PIO_SLOT_24_SIZE, 0x4000);
        assert_eq!(G17S_GFX1_IDLE_STATUS_PHYS, 0x21_00d4_0000);
        assert_eq!(G17S_GFX1_IDLE_POLL_MASK, 1);
        assert_eq!(G17S_GFX1_IDLE_POLL_EXPECTED, 1);
        assert!(!g17s_gfx1_idle_poll_satisfied(0));
        assert!(g17s_gfx1_idle_poll_satisfied(1));
        assert!(!g17s_gfx1_idle_poll_satisfied(2));
        assert!(g17s_gfx1_idle_poll_satisfied(u32::MAX));
        assert_eq!(
            g17s_gfx1_report_timebase_delta_us(G17S_GFX1_IDLE_POLL_TIMEOUT_TICKS),
            G17S_GFX1_IDLE_POLL_REPORTED_MAX_US
        );
        assert_eq!(
            G17_PIO_SLOT_24_MAP_EVIDENCE_TARGETS,
            [
                G17PioMapEvidenceTarget::G17P,
                G17PioMapEvidenceTarget::G17G,
                G17PioMapEvidenceTarget::G17S,
            ]
        );
    }

    #[test]
    fn g17g_and_g17p_physical_pio_bindings_are_target_specific() {
        let g17p = g17_host_pio_physical_evidence(G17HostPioPhysicalEvidenceTarget::G17P);
        let g17g = g17_host_pio_physical_evidence(G17HostPioPhysicalEvidenceTarget::G17G);

        assert_eq!(g17p.sgx_phys_base, 0x2_7000_0000);
        assert_eq!(g17g.sgx_phys_base, 0x3_7000_0000);
        assert_eq!(g17p.host_io_page_size, 0x4000);
        assert_eq!(g17g.host_io_page_size, 0x4000);
        assert_eq!(
            g17p.meta_sw_interrupt,
            G17MetaSwInterruptEvidence {
                first_u64: 0x3_0101_4048,
                second_u64: 0x3_0101_4248,
                trailing_u32: 0x4000,
            }
        );
        assert_eq!(
            g17g.meta_sw_interrupt,
            G17MetaSwInterruptEvidence {
                first_u64: 0x3_8101_4048,
                second_u64: 0x3_8101_4248,
                trailing_u32: 0x4000,
            }
        );
        assert_eq!(
            G17_HOST_PIO_PHYSICAL_EVIDENCE_TARGETS,
            [
                G17HostPioPhysicalEvidenceTarget::G17P,
                G17HostPioPhysicalEvidenceTarget::G17G,
                G17HostPioPhysicalEvidenceTarget::G17S,
            ]
        );
        assert!(!G17_HOST_PIO_LINUX_MMIO_OWNERSHIP_PROVEN);
        assert!(!EXECUTABLE_GFX1_POWER_SEQUENCE_AVAILABLE);
    }

    #[test]
    fn g17g_and_g17p_pio_bindings_fail_closed_for_unbound_slot_0b() {
        let g17p_slot0 = Some(G17HostPioPhysicalBinding {
            phys_base: 0x3_0101_4000,
            size: 0x4000,
            source: G17HostPioBindingSource::MetaSwInterrupt,
        });
        let g17g_slot0 = Some(G17HostPioPhysicalBinding {
            phys_base: 0x3_8101_4000,
            size: 0x4000,
            source: G17HostPioBindingSource::MetaSwInterrupt,
        });
        let g17p_slot24 = Some(G17HostPioPhysicalBinding {
            phys_base: 0x2_70d4_0000,
            size: 0x4000,
            source: G17HostPioBindingSource::SgxRelativeOffsetTable,
        });
        let g17g_slot24 = Some(G17HostPioPhysicalBinding {
            phys_base: 0x3_70d4_0000,
            size: 0x4000,
            source: G17HostPioBindingSource::SgxRelativeOffsetTable,
        });

        assert!(validate_g17_host_pio_physical_binding(
            G17HostPioPhysicalEvidenceTarget::G17P,
            G17PioSlot::GpcAicSwInt,
            g17p_slot0,
        ));
        assert!(validate_g17_host_pio_physical_binding(
            G17HostPioPhysicalEvidenceTarget::G17G,
            G17PioSlot::GpcAicSwInt,
            g17g_slot0,
        ));
        assert!(validate_g17_host_pio_physical_binding(
            G17HostPioPhysicalEvidenceTarget::G17P,
            G17PioSlot::FenderRegsMcw,
            g17p_slot24,
        ));
        assert!(validate_g17_host_pio_physical_binding(
            G17HostPioPhysicalEvidenceTarget::G17G,
            G17PioSlot::FenderRegsMcw,
            g17g_slot24,
        ));

        for target in [
            G17HostPioPhysicalEvidenceTarget::G17P,
            G17HostPioPhysicalEvidenceTarget::G17G,
        ] {
            assert_eq!(
                g17_host_pio_physical_binding(
                    target,
                    G17PioSlot::PushTelemetryDashboardConfigSpace
                ),
                None
            );
            assert!(validate_g17_host_pio_physical_binding(
                target,
                G17PioSlot::PushTelemetryDashboardConfigSpace,
                None,
            ));
            assert!(!validate_g17_host_pio_physical_binding(
                target,
                G17PioSlot::PushTelemetryDashboardConfigSpace,
                g17p_slot0,
            ));
        }

        assert!(!validate_g17_host_pio_physical_binding(
            G17HostPioPhysicalEvidenceTarget::G17G,
            G17PioSlot::GpcAicSwInt,
            g17p_slot0,
        ));
        assert!(!validate_g17_host_pio_physical_binding(
            G17HostPioPhysicalEvidenceTarget::G17P,
            G17PioSlot::FenderRegsMcw,
            Some(G17HostPioPhysicalBinding {
                phys_base: 0x2_70d4_0000,
                size: 0x2000,
                source: G17HostPioBindingSource::SgxRelativeOffsetTable,
            }),
        ));
    }

    #[test]
    fn g17s_physical_pio_bindings_join_adt_table_and_target_override() {
        let evidence = g17_host_pio_physical_evidence(G17HostPioPhysicalEvidenceTarget::G17S);
        let target_configure = evidence.target_configure.unwrap();
        let slot0 = Some(G17HostPioPhysicalBinding {
            phys_base: 0x2_8041_4000,
            size: 0x4000,
            source: G17HostPioBindingSource::TargetConfigureConstant,
        });
        let slot0b = Some(G17HostPioPhysicalBinding {
            phys_base: 0x2_8425_8000,
            size: 0x4000,
            source: G17HostPioBindingSource::TargetConfigureConstant,
        });
        let slot24 = Some(G17HostPioPhysicalBinding {
            phys_base: 0x21_00d4_0000,
            size: 0x4000,
            source: G17HostPioBindingSource::SgxRelativeOffsetTable,
        });

        assert_eq!(evidence.sgx_phys_base, 0x21_0000_0000);
        assert_eq!(evidence.host_io_page_size, 0x4000);
        assert_eq!(
            evidence.meta_sw_interrupt,
            G17MetaSwInterruptEvidence {
                first_u64: 0x2_8041_4084,
                second_u64: 0x2_8041_4284,
                trailing_u32: 0x80,
            }
        );
        assert_eq!(target_configure.slot_0_phys_base, 0x2_8041_4000);
        assert_eq!(target_configure.slot_0b_phys_base, 0x2_8425_8000);
        assert_eq!(target_configure.slot_size, 0x4000);
        assert_eq!(
            evidence.meta_sw_interrupt.first_u64
                & !((evidence.host_io_page_size as u64) - 1),
            target_configure.slot_0_phys_base
        );
        assert_eq!(G17S_HOST_PIO_RELATIVE_TABLE_TYPES.len(), 19);
        assert!(!G17S_HOST_PIO_RELATIVE_TABLE_TYPES
            .iter()
            .any(|pio_type| *pio_type == G17PioSlot::PushTelemetryDashboardConfigSpace as u8));
        assert!(G17S_HOST_PIO_RELATIVE_TABLE_TYPES
            .iter()
            .any(|pio_type| *pio_type == G17PioSlot::FenderRegsMcw as u8));

        assert!(validate_g17_host_pio_physical_binding(
            G17HostPioPhysicalEvidenceTarget::G17S,
            G17PioSlot::GpcAicSwInt,
            slot0,
        ));
        assert!(validate_g17_host_pio_physical_binding(
            G17HostPioPhysicalEvidenceTarget::G17S,
            G17PioSlot::PushTelemetryDashboardConfigSpace,
            slot0b,
        ));
        assert!(validate_g17_host_pio_physical_binding(
            G17HostPioPhysicalEvidenceTarget::G17S,
            G17PioSlot::FenderRegsMcw,
            slot24,
        ));
        assert!(!validate_g17_host_pio_physical_binding(
            G17HostPioPhysicalEvidenceTarget::G17S,
            G17PioSlot::PushTelemetryDashboardConfigSpace,
            None,
        ));
        assert!(!validate_g17_host_pio_physical_binding(
            G17HostPioPhysicalEvidenceTarget::G17S,
            G17PioSlot::GpcAicSwInt,
            Some(G17HostPioPhysicalBinding {
                phys_base: 0x2_8041_4000,
                size: 0x4000,
                source: G17HostPioBindingSource::MetaSwInterrupt,
            }),
        ));
        assert_eq!(G17S_GFX1_IDLE_STATUS_PHYS, slot24.unwrap().phys_base);
        assert!(!G17_HOST_PIO_LINUX_MMIO_OWNERSHIP_PROVEN);
        assert!(!EXECUTABLE_GFX1_POWER_SEQUENCE_AVAILABLE);
        assert!(!EXECUTABLE_G17_RECOVERY_HANDSHAKE_AVAILABLE);
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
    fn g15_and_g16_variants_use_artifact_specific_mapped_state_offsets() {
        assert_eq!(G15_AND_G16G_RECOVERY_WORD_OFFSET, 0x5190);
        assert_eq!(G16X_RECOVERY_WORD_OFFSET, 0x4ae0);
        assert_eq!(
            G15_G16_RECOVERY_LAYOUT_EVIDENCE_TARGETS,
            [
                G15G16RecoveryLayoutEvidenceTarget::G15G,
                G15G16RecoveryLayoutEvidenceTarget::G15S,
                G15G16RecoveryLayoutEvidenceTarget::G15C,
                G15G16RecoveryLayoutEvidenceTarget::G15D,
                G15G16RecoveryLayoutEvidenceTarget::G16G,
                G15G16RecoveryLayoutEvidenceTarget::G16S,
                G15G16RecoveryLayoutEvidenceTarget::G16C,
            ]
        );

        let targets_and_offsets = [
            (G15G16RecoveryLayoutEvidenceTarget::G15G, 0x5190),
            (G15G16RecoveryLayoutEvidenceTarget::G15S, 0x5190),
            (G15G16RecoveryLayoutEvidenceTarget::G15C, 0x5190),
            (G15G16RecoveryLayoutEvidenceTarget::G15D, 0x5190),
            (G15G16RecoveryLayoutEvidenceTarget::G16G, 0x5190),
            (G15G16RecoveryLayoutEvidenceTarget::G16S, 0x4ae0),
            (G15G16RecoveryLayoutEvidenceTarget::G16C, 0x4ae0),
        ];
        for (target, expected_offset) in targets_and_offsets {
            assert_eq!(g15_g16_recovery_word_offset(target), expected_offset);
            for raw in 0..=3 {
                assert_eq!(
                    decode_g15_g16_recovery_word_at_offset(target, expected_offset, raw),
                    Ok(decode_recovery_word(raw).unwrap())
                );
            }
            let other_layout_offset = if expected_offset == 0x5190 {
                0x4ae0
            } else {
                0x5190
            };
            for wrong_offset in [
                0,
                other_layout_offset,
                expected_offset - 4,
                expected_offset + 4,
                u32::MAX,
            ] {
                assert_eq!(
                    decode_g15_g16_recovery_word_at_offset(target, wrong_offset, 1),
                    Err(RecoveryError::UnexpectedWordOffset)
                );
            }
        }

        for raw in [4, 0x22, 0x23, u32::MAX] {
            assert_eq!(
                decode_g15_g16_recovery_word_at_offset(
                    G15G16RecoveryLayoutEvidenceTarget::G15G,
                    G15_AND_G16G_RECOVERY_WORD_OFFSET,
                    raw,
                ),
                Err(RecoveryError::UnknownWordValue)
            );
        }
    }

    #[test]
    fn g17p_recovery_word_uses_the_cross_g17_mapped_state_offset() {
        assert_eq!(G17_RECOVERY_WORD_OFFSET, 0x4ae0);
        assert_eq!(
            G17_RECOVERY_LAYOUT_EVIDENCE_TARGETS,
            [
                G17RecoveryLayoutEvidenceTarget::G17P,
                G17RecoveryLayoutEvidenceTarget::G17G,
                G17RecoveryLayoutEvidenceTarget::G17S,
            ]
        );

        for target in G17_RECOVERY_LAYOUT_EVIDENCE_TARGETS {
            assert_eq!(g17_recovery_word_offset(target), 0x4ae0);
            for raw in 0..=3 {
                assert_eq!(
                    decode_g17_recovery_word_at_offset(target, 0x4ae0, raw),
                    Ok(decode_recovery_word(raw).unwrap())
                );
            }
            for wrong_offset in [0, 0x4adc, 0x4ae4, u32::MAX] {
                assert_eq!(
                    decode_g17_recovery_word_at_offset(target, wrong_offset, 1),
                    Err(RecoveryError::UnexpectedWordOffset)
                );
            }
        }

        for raw in [4, 0x22, 0x23, u32::MAX] {
            assert_eq!(
                decode_g17_recovery_word_at_offset(
                    G17RecoveryLayoutEvidenceTarget::G17P,
                    G17_RECOVERY_WORD_OFFSET,
                    raw,
                ),
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
    fn g17_pm_config_follows_the_proven_hal_boundary() {
        assert_eq!(
            g17_pm_config_for_evidence_target(G17PmConfigEvidenceTarget::G17P),
            &G17_HAL200_PM_CONFIG_OPAQUE
        );
        for target in [
            G17PmConfigEvidenceTarget::G17G,
            G17PmConfigEvidenceTarget::G17S,
        ] {
            assert_eq!(
                g17_pm_config_for_evidence_target(target),
                &G17_HAL300_PM_CONFIG_OPAQUE
            );
        }
        assert_ne!(G17_HAL200_PM_CONFIG_OPAQUE, G17_HAL300_PM_CONFIG_OPAQUE);
        assert_eq!(
            G17_PM_CONFIG_EVIDENCE_TARGETS,
            [
                G17PmConfigEvidenceTarget::G17P,
                G17PmConfigEvidenceTarget::G17G,
                G17PmConfigEvidenceTarget::G17S,
            ]
        );
    }

    #[test]
    fn t6050_perf_states1_properties_are_exact_table1_mirrors() {
        const FREQUENCIES: [u32; 14] = [
            0,
            338_000_000,
            486_000_000,
            636_000_000,
            796_000_000,
            888_000_000,
            988_000_000,
            1_084_000_000,
            1_182_000_000,
            1_278_000_000,
            1_374_000_000,
            1_470_000_000,
            1_578_000_000,
            1_620_000_000,
        ];
        const CORE_VOLTS_0: [u32; 14] = [
            125, 605, 620, 655, 705, 715, 745, 770, 800, 840, 880, 925, 1000, 1025,
        ];
        const CORE_VOLTS_1: [u32; 14] = [
            125, 605, 635, 655, 705, 715, 745, 770, 800, 840, 880, 925, 1000, 1040,
        ];
        const SRAM_VOLTS_0: [u32; 14] = [
            775, 775, 775, 775, 775, 775, 775, 775, 800, 840, 880, 925, 1000, 1025,
        ];
        const SRAM_VOLTS_1: [u32; 14] = [
            775, 775, 775, 775, 775, 775, 775, 775, 800, 840, 880, 925, 1000, 1040,
        ];
        const EMPTY: T6050PerfState = T6050PerfState {
            freq_hz: 0,
            volt_mv: 0,
        };

        let mut core = [EMPTY; 28];
        let mut sram = [EMPTY; 28];
        let mut core_mirror = [EMPTY; 14];
        let mut sram_mirror = [EMPTY; 14];
        for index in 0..14 {
            core[index] = T6050PerfState {
                freq_hz: FREQUENCIES[index],
                volt_mv: CORE_VOLTS_0[index],
            };
            core[index + 14] = T6050PerfState {
                freq_hz: FREQUENCIES[index],
                volt_mv: CORE_VOLTS_1[index],
            };
            sram[index] = T6050PerfState {
                freq_hz: FREQUENCIES[index],
                volt_mv: SRAM_VOLTS_0[index],
            };
            sram[index + 14] = T6050PerfState {
                freq_hz: FREQUENCIES[index],
                volt_mv: SRAM_VOLTS_1[index],
            };
            core_mirror[index] = core[index + 14];
            sram_mirror[index] = sram[index + 14];
        }

        let validate = |core: &[T6050PerfState],
                        sram: &[T6050PerfState],
                        core_mirror: &[T6050PerfState],
                        sram_mirror: &[T6050PerfState]| {
            validate_t6050_g17s_perf_state_mirrors(
                T6050_G17S_PERF_STATE_COUNT,
                T6050_G17S_PERF_STATE_TABLE_COUNT,
                core,
                sram,
                core_mirror,
                sram_mirror,
            )
        };

        assert_eq!(validate(&core, &sram, &core_mirror, &sram_mirror), Ok(()));
        assert_eq!(
            &core_mirror[..],
            &core[T6050_G17S_PERF_STATE_COUNT as usize..]
        );
        assert_ne!(
            &core_mirror[..],
            &core[..T6050_G17S_PERF_STATE_COUNT as usize]
        );

        core_mirror[2].volt_mv -= 1;
        assert_eq!(
            validate(&core, &sram, &core_mirror, &sram_mirror),
            Err(T6050PerfStateMirrorError::CoreMirrorMismatch)
        );
        core_mirror[2].volt_mv += 1;
        sram_mirror[13].volt_mv -= 1;
        assert_eq!(
            validate(&core, &sram, &core_mirror, &sram_mirror),
            Err(T6050PerfStateMirrorError::SramMirrorMismatch)
        );
        sram_mirror[13].volt_mv += 1;

        assert_eq!(
            validate_t6050_g17s_perf_state_mirrors(
                13,
                T6050_G17S_PERF_STATE_TABLE_COUNT,
                &core,
                &sram,
                &core_mirror,
                &sram_mirror,
            ),
            Err(T6050PerfStateMirrorError::UnexpectedPerfStateCount)
        );
        assert_eq!(
            validate_t6050_g17s_perf_state_mirrors(
                T6050_G17S_PERF_STATE_COUNT,
                3,
                &core,
                &sram,
                &core_mirror,
                &sram_mirror,
            ),
            Err(T6050PerfStateMirrorError::UnexpectedPerfStateTableCount)
        );
        assert_eq!(
            validate(&core[..27], &sram, &core_mirror, &sram_mirror),
            Err(T6050PerfStateMirrorError::UnexpectedMainCoreShape)
        );
        assert_eq!(
            validate(&core, &sram[..27], &core_mirror, &sram_mirror),
            Err(T6050PerfStateMirrorError::UnexpectedMainSramShape)
        );
        assert_eq!(
            validate(&core, &sram, &core_mirror[..13], &sram_mirror),
            Err(T6050PerfStateMirrorError::UnexpectedCoreMirrorShape)
        );
        assert_eq!(
            validate(&core, &sram, &core_mirror, &sram_mirror[..13]),
            Err(T6050PerfStateMirrorError::UnexpectedSramMirrorShape)
        );
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
                RecoveryEvidenceTarget::G15S,
                RecoveryEvidenceTarget::G15C,
                RecoveryEvidenceTarget::G15D,
                RecoveryEvidenceTarget::G16G,
                RecoveryEvidenceTarget::G16S,
                RecoveryEvidenceTarget::G16C,
                RecoveryEvidenceTarget::G17P,
                RecoveryEvidenceTarget::G17G,
                RecoveryEvidenceTarget::G17S,
            ]
        );
    }
}
