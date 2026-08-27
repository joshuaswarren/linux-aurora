// SPDX-License-Identifier: GPL-2.0-only OR MIT

#![cfg_attr(not(test), allow(dead_code))]

//! T8140/G17P manager construction boundary.
//!
//! This path needs only target identity, init-data geometry, and the retained
//! dual-role resource owner. It does not depend on the AGX2 `HwConfig` and it
//! cannot publish roots, start firmware, or submit work.

#[cfg(not(test))]
use kernel::prelude::*;

#[cfg(not(test))]
use crate::{
    agx_power_recovery, driver::AsahiDevice, g17_initdata, g17_lifecycle, g17_resources,
    g17_submission, hw::agx3, identity, mmu,
};
#[cfg(test)]
#[path = "g17_initdata.rs"]
mod g17_initdata;
#[cfg(test)]
#[path = "identity.rs"]
mod identity;

use g17_initdata::InstanceRole;
use identity::{FirmwareRoleTopology, GpuGen, GpuHalGeneration, GpuVariant, SubmissionTransport};

/// Grounded manager inputs for T8140. Power and tuning values are excluded.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) struct T8140ManagerConfig {
    pub(crate) chip_id: u32,
    pub(crate) base_clock_hz: u32,
    pub(crate) hw_family: u8,
    pub(crate) hw_variant: u8,
    pub(crate) num_dies: u32,
    pub(crate) uat_ias: u8,
    pub(crate) uat_oas: u32,
    pub(crate) uat_page_size: u16,
    pub(crate) uat_page_bits: u8,
    pub(crate) uat_levels: [(u8, u16); g17_initdata::UAT_LEVEL_COUNT],
    pub(crate) initdata_version: [u16; 4],
    pub(crate) root_sizes: [usize; 2],
    pub(crate) secondary_root_delta: u64,
    pub(crate) secondary_shared_region_delta: u64,
    pub(crate) shared_cluster_size: usize,
    pub(crate) work_channel_count: usize,
}

/// Exact facts needed to construct, but not execute, a T8140 manager.
pub(crate) const T8140_MANAGER_CONFIG: T8140ManagerConfig = T8140ManagerConfig {
    chip_id: 0x8140,
    base_clock_hz: 24_000_000,
    hw_family: 0x0a,
    hw_variant: 0,
    num_dies: 1,
    uat_ias: 42,
    uat_oas: 42,
    uat_page_size: g17_initdata::UAT_PAGE_SIZE,
    uat_page_bits: g17_initdata::UAT_PAGE_BITS,
    uat_levels: g17_initdata::UAT_LEVELS,
    initdata_version: g17_initdata::INITDATA_VERSION,
    root_sizes: [
        g17_initdata::ROOT_SIZE_PRIMARY,
        g17_initdata::ROOT_SIZE_SECONDARY,
    ],
    secondary_root_delta: g17_initdata::SECONDARY_ROOT_DELTA,
    secondary_shared_region_delta: g17_initdata::SECONDARY_SHARED_REGION_DELTA,
    shared_cluster_size: g17_initdata::NATIVE_SHARED_CLUSTER_SIZE,
    work_channel_count: g17_initdata::WORK_CHANNEL_COUNT,
};

/// Platform-derived SKSM scratch offsets within the mapped accelerator
/// register window.
///
/// Apple's host loads these three values from accelerator object fields
/// `+0x10730`, `+0x10734`, and `+0x10738`. Those are object offsets, not SGX
/// MMIO offsets. This type carries the loaded values and no physical base.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) struct G17SksmScratchGeometry {
    pub(crate) write0_base: u32,
    pub(crate) write1_base: u32,
    pub(crate) queue_stride: u32,
}

/// The two offsets used for the G17P compute queue.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) struct G17SksmComputeScratchOffsets {
    pub(crate) write0: u32,
    pub(crate) write1: u32,
}

impl G17SksmScratchGeometry {
    /// Apply the exact host equation for compute queue index 9.
    pub(crate) const fn compute_offsets(
        self,
    ) -> core::result::Result<G17SksmComputeScratchOffsets, ConstructionError> {
        const COMPUTE_QUEUE_INDEX: u32 = 9;

        let queue_offset = match self.queue_stride.checked_mul(COMPUTE_QUEUE_INDEX) {
            Some(offset) => offset,
            None => return Err(ConstructionError::SksmScratchOffsetOverflow),
        };
        let write0 = match self.write0_base.checked_add(queue_offset) {
            Some(offset) => offset,
            None => return Err(ConstructionError::SksmScratchOffsetOverflow),
        };
        let write1 = match self.write1_base.checked_add(queue_offset) {
            Some(offset) => offset,
            None => return Err(ConstructionError::SksmScratchOffsetOverflow),
        };

        Ok(G17SksmComputeScratchOffsets { write0, write1 })
    }
}

/// Target configuration supplied by platform discovery.
///
/// There is no default T8140 scratch geometry. A caller must recover the
/// three values through the platform configuration path before construction.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) struct G17PPlatformConfig {
    pub(crate) sksm_scratch: G17SksmScratchGeometry,
}

/// SoC and register identity presented to the manager constructor.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) struct TargetObservation {
    pub(crate) chip_id: u32,
    pub(crate) declared_gen: GpuGen,
    pub(crate) declared_variant: GpuVariant,
    pub(crate) hw_family: u8,
    pub(crate) hw_variant: u8,
    pub(crate) num_dies: u32,
    pub(crate) uat_ias: u8,
    pub(crate) uat_oas: u32,
}

/// Ownership and placement facts copied from the retained resource owner.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) struct ResourceObservation {
    pub(crate) uat_ttb_base: u64,
    pub(crate) primary_uat_owner: u64,
    pub(crate) secondary_uat_owner: u64,
    pub(crate) primary_role: InstanceRole,
    pub(crate) secondary_role: InstanceRole,
    pub(crate) primary_root: u64,
    pub(crate) secondary_root: u64,
    pub(crate) shared_cluster_size: usize,
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) enum ConstructionError {
    WrongChip,
    WrongDeclaredIdentity,
    WrongHardwareIdentity,
    WrongUatGeometry,
    UndecodableIdentity,
    WrongDecodedIdentity,
    NullUatOwner,
    SplitUatOwner,
    WrongRoleTopology,
    WrongRootPlacement,
    WrongSharedClusterSize,
    SksmScratchOffsetOverflow,
}

/// Validate the exact T8140 identity without consulting an AGX2 `HwConfig`.
pub(crate) fn validate_target(
    target: TargetObservation,
) -> core::result::Result<(), ConstructionError> {
    let config = &T8140_MANAGER_CONFIG;

    if target.chip_id != config.chip_id {
        return Err(ConstructionError::WrongChip);
    }
    if target.declared_gen != GpuGen::G17 || target.declared_variant != GpuVariant::P {
        return Err(ConstructionError::WrongDeclaredIdentity);
    }
    if target.hw_family != config.hw_family
        || target.hw_variant != config.hw_variant
        || target.num_dies != config.num_dies
    {
        return Err(ConstructionError::WrongHardwareIdentity);
    }
    if target.uat_ias != config.uat_ias || target.uat_oas != config.uat_oas {
        return Err(ConstructionError::WrongUatGeometry);
    }

    let num_dies =
        u8::try_from(target.num_dies).map_err(|_| ConstructionError::UndecodableIdentity)?;
    let decoded = identity::decode_gpu_identity(target.hw_family, target.hw_variant, num_dies)
        .ok_or(ConstructionError::UndecodableIdentity)?;
    if decoded.gpu_gen != GpuGen::G17
        || decoded.gpu_variant != GpuVariant::P
        || decoded.usc_generation != 3
        || decoded.gpu_hal_generation != GpuHalGeneration::Hal200
        || decoded.firmware_roles != FirmwareRoleTopology::Dual
        || decoded.submission_transport != SubmissionTransport::ClassicAndSksm
        || decoded.uat_input_address_bits != config.uat_ias
    {
        return Err(ConstructionError::WrongDecodedIdentity);
    }

    Ok(())
}

/// Validate that both roles are retained under one UAT with native placement.
pub(crate) fn validate_resources(
    resources: ResourceObservation,
) -> core::result::Result<(), ConstructionError> {
    if resources.uat_ttb_base == 0
        || resources.primary_uat_owner == 0
        || resources.secondary_uat_owner == 0
    {
        return Err(ConstructionError::NullUatOwner);
    }
    if resources.uat_ttb_base != resources.primary_uat_owner
        || resources.uat_ttb_base != resources.secondary_uat_owner
    {
        return Err(ConstructionError::SplitUatOwner);
    }
    if resources.primary_role != InstanceRole::Primary
        || resources.secondary_role != InstanceRole::Secondary
    {
        return Err(ConstructionError::WrongRoleTopology);
    }
    let secondary_root = resources
        .primary_root
        .checked_add(T8140_MANAGER_CONFIG.secondary_root_delta)
        .ok_or(ConstructionError::WrongRootPlacement)?;
    if resources.secondary_root != secondary_root {
        return Err(ConstructionError::WrongRootPlacement);
    }
    if resources.shared_cluster_size != T8140_MANAGER_CONFIG.shared_cluster_size {
        return Err(ConstructionError::WrongSharedClusterSize);
    }

    Ok(())
}

/// Live-execution facts which remain separate from manager construction.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) struct ExecutionAvailability {
    pub(crate) initdata_layout: bool,
    pub(crate) initdata_graph: bool,
    pub(crate) all_hw_globals: bool,
    pub(crate) perf_tables: bool,
    pub(crate) bundle_content: bool,
    pub(crate) primary_status_b: bool,
    pub(crate) power_sequence: bool,
    pub(crate) lifecycle_executor: bool,
    pub(crate) submission_transport: bool,
}

pub(crate) mod execution_gap {
    pub(crate) const INITDATA_LAYOUT: u16 = 1 << 0;
    pub(crate) const INITDATA_GRAPH: u16 = 1 << 1;
    pub(crate) const HW_GLOBALS: u16 = 1 << 2;
    pub(crate) const PERF_TABLES: u16 = 1 << 3;
    pub(crate) const BUNDLE_CONTENT: u16 = 1 << 4;
    pub(crate) const PRIMARY_STATUS_B: u16 = 1 << 5;
    pub(crate) const POWER_SEQUENCE: u16 = 1 << 6;
    pub(crate) const LIFECYCLE_EXECUTOR: u16 = 1 << 7;
    pub(crate) const SUBMISSION_TRANSPORT: u16 = 1 << 8;
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) struct ExecutionGaps(u16);

impl ExecutionGaps {
    pub(crate) const fn bits(self) -> u16 {
        self.0
    }

    pub(crate) const fn contains(self, gap: u16) -> bool {
        self.0 & gap != 0
    }
}

/// Return every unavailable execution group. An empty result is the only pass.
pub(crate) const fn execution_gaps(available: ExecutionAvailability) -> ExecutionGaps {
    let mut gaps = 0;
    if !available.initdata_layout {
        gaps |= execution_gap::INITDATA_LAYOUT;
    }
    if !available.initdata_graph {
        gaps |= execution_gap::INITDATA_GRAPH;
    }
    if !available.all_hw_globals {
        gaps |= execution_gap::HW_GLOBALS;
    }
    if !available.perf_tables {
        gaps |= execution_gap::PERF_TABLES;
    }
    if !available.bundle_content {
        gaps |= execution_gap::BUNDLE_CONTENT;
    }
    if !available.primary_status_b {
        gaps |= execution_gap::PRIMARY_STATUS_B;
    }
    if !available.power_sequence {
        gaps |= execution_gap::POWER_SEQUENCE;
    }
    if !available.lifecycle_executor {
        gaps |= execution_gap::LIFECYCLE_EXECUTOR;
    }
    if !available.submission_transport {
        gaps |= execution_gap::SUBMISSION_TRANSPORT;
    }

    ExecutionGaps(gaps)
}

#[cfg(not(test))]
const fn current_execution_availability() -> ExecutionAvailability {
    ExecutionAvailability {
        initdata_layout: g17_initdata::GROUNDED_LAYOUT_AVAILABLE,
        initdata_graph: g17_initdata::PARSE_ACCEPTED_GRAPH_AVAILABLE,
        all_hw_globals: g17_initdata::ALL_HW_GLOBALS_VALUES_GROUNDED,
        perf_tables: g17_initdata::PERF_TABLE_VALUES_ENCODED,
        bundle_content: g17_initdata::BUNDLE_VIEW_CONTENT_CARRIED,
        primary_status_b: g17_initdata::PRIMARY_STATUS_B_FULL_OBJECT_CARRIED,
        power_sequence: agx_power_recovery::EXECUTABLE_GFX1_POWER_SEQUENCE_AVAILABLE,
        lifecycle_executor: g17_lifecycle::TRANSACTIONAL_CPU_EXECUTOR_AVAILABLE,
        submission_transport: g17_submission::EXECUTABLE_SUBMISSION_TRANSPORT_AVAILABLE,
    }
}

#[cfg(not(test))]
fn target_observation(soc: &'static agx3::SocConfig) -> TargetObservation {
    TargetObservation {
        chip_id: soc.chip_id,
        declared_gen: soc.gpu_gen,
        declared_variant: soc.gpu_variant,
        hw_family: soc.hw_family,
        hw_variant: soc.hw_variant,
        num_dies: soc.num_dies,
        uat_ias: soc.uat_ias,
        uat_oas: soc.uat_oas,
    }
}

#[cfg(not(test))]
fn resource_observation(
    handoff: &g17_resources::G17ResourceHandoff,
    uat: &mmu::Uat,
) -> ResourceObservation {
    ResourceObservation {
        uat_ttb_base: uat.ttb_base(),
        primary_uat_owner: handoff.primary.uat_owner,
        secondary_uat_owner: handoff.secondary.uat_owner,
        primary_role: handoff.primary.role,
        secondary_role: handoff.secondary.role,
        primary_root: handoff.primary.instance.root,
        secondary_root: handoff.secondary.instance.root,
        shared_cluster_size: handoff.shared.hw_data_bundle_alloc,
    }
}

/// Retained construction state for both T8140 firmware roles.
///
/// Creating this object allocates and validates resources. It performs no
/// firmware publication, ASC start, MMIO write, or submission.
#[cfg(not(test))]
pub(crate) struct G17PManagerConstruction {
    resources: g17_resources::G17ResourceOwner,
    platform: G17PPlatformConfig,
}

#[cfg(not(test))]
impl G17PManagerConstruction {
    pub(crate) fn new(
        dev: &AsahiDevice,
        soc: &'static agx3::SocConfig,
        platform: G17PPlatformConfig,
    ) -> Result<Self> {
        validate_target(target_observation(soc)).map_err(|_| EINVAL)?;
        platform
            .sksm_scratch
            .compute_offsets()
            .map_err(|_| EINVAL)?;

        let resources = g17_resources::G17ResourceOwner::new(dev)?;
        g17_resources::validate_handoff(resources.handoff()).map_err(|_| EINVAL)?;
        validate_resources(resource_observation(resources.handoff(), resources.uat()))
            .map_err(|_| EINVAL)?;

        Ok(Self {
            resources,
            platform,
        })
    }

    pub(crate) fn config(&self) -> &'static T8140ManagerConfig {
        &T8140_MANAGER_CONFIG
    }

    pub(crate) fn handoff(&self) -> &g17_resources::G17ResourceHandoff {
        self.resources.handoff()
    }

    pub(crate) fn uat(&self) -> &mmu::Uat {
        self.resources.uat()
    }

    pub(crate) fn sksm_scratch(&self) -> G17SksmScratchGeometry {
        self.platform.sksm_scratch
    }

    pub(crate) fn execution_gaps(&self) -> ExecutionGaps {
        execution_gaps(current_execution_availability())
    }

    pub(crate) fn require_executable(&self) -> Result {
        if self.execution_gaps().bits() != 0 {
            return Err(ENOTSUPP);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ROOT: u64 = 0xffff_fc20_0100_0000;
    const UAT: u64 = 0x9000_0000;

    fn target() -> TargetObservation {
        TargetObservation {
            chip_id: 0x8140,
            declared_gen: GpuGen::G17,
            declared_variant: GpuVariant::P,
            hw_family: 0x0a,
            hw_variant: 0,
            num_dies: 1,
            uat_ias: 42,
            uat_oas: 42,
        }
    }

    fn resources() -> ResourceObservation {
        ResourceObservation {
            uat_ttb_base: UAT,
            primary_uat_owner: UAT,
            secondary_uat_owner: UAT,
            primary_role: InstanceRole::Primary,
            secondary_role: InstanceRole::Secondary,
            primary_root: ROOT,
            secondary_root: ROOT + g17_initdata::SECONDARY_ROOT_DELTA,
            shared_cluster_size: g17_initdata::NATIVE_SHARED_CLUSTER_SIZE,
        }
    }

    #[test]
    fn manager_config_is_exact_t8140_geometry() {
        assert_eq!(T8140_MANAGER_CONFIG.chip_id, 0x8140);
        assert_eq!(T8140_MANAGER_CONFIG.base_clock_hz, 24_000_000);
        assert_eq!(T8140_MANAGER_CONFIG.uat_ias, 42);
        assert_eq!(T8140_MANAGER_CONFIG.uat_oas, 42);
        assert_eq!(T8140_MANAGER_CONFIG.uat_page_size, 0x4000);
        assert_eq!(T8140_MANAGER_CONFIG.uat_page_bits, 14);
        assert_eq!(
            T8140_MANAGER_CONFIG.uat_levels,
            [(36, 64), (25, 2048), (14, 2048)]
        );
        assert_eq!(
            T8140_MANAGER_CONFIG.initdata_version,
            [0x04c0, 0x0396, 0xa322, 0x0c8a]
        );
        assert_eq!(T8140_MANAGER_CONFIG.root_sizes, [0xb8, 0xc8]);
        assert_eq!(T8140_MANAGER_CONFIG.secondary_root_delta, 0x8000);
        assert_eq!(T8140_MANAGER_CONFIG.secondary_shared_region_delta, 0x40000);
        assert_eq!(T8140_MANAGER_CONFIG.shared_cluster_size, 0x28000);
        assert_eq!(T8140_MANAGER_CONFIG.work_channel_count, 12);
    }

    #[test]
    fn exact_target_and_shared_dual_role_resources_pass() {
        assert_eq!(validate_target(target()), Ok(()));
        assert_eq!(validate_resources(resources()), Ok(()));
    }

    #[test]
    fn scratch_geometry_uses_values_not_accelerator_object_offsets() {
        let platform = G17PPlatformConfig {
            sksm_scratch: G17SksmScratchGeometry {
                write0_base: 0,
                write1_base: 4,
                queue_stride: 1,
            },
        };
        assert_eq!(
            platform.sksm_scratch.compute_offsets(),
            Ok(G17SksmComputeScratchOffsets {
                write0: 9,
                write1: 13,
            })
        );
    }

    #[test]
    fn scratch_geometry_overflow_fails_before_resource_allocation() {
        for geometry in [
            G17SksmScratchGeometry {
                write0_base: 0,
                write1_base: 0,
                queue_stride: u32::MAX,
            },
            G17SksmScratchGeometry {
                write0_base: u32::MAX,
                write1_base: 0,
                queue_stride: 1,
            },
            G17SksmScratchGeometry {
                write0_base: 0,
                write1_base: u32::MAX,
                queue_stride: 1,
            },
        ] {
            assert_eq!(
                geometry.compute_offsets(),
                Err(ConstructionError::SksmScratchOffsetOverflow)
            );
        }
    }

    #[test]
    fn non_t8140_identity_and_uat_geometry_fail_closed() {
        let mut observed = target();
        observed.chip_id = 0x8142;
        assert_eq!(validate_target(observed), Err(ConstructionError::WrongChip));

        let mut observed = target();
        observed.declared_variant = GpuVariant::G;
        assert_eq!(
            validate_target(observed),
            Err(ConstructionError::WrongDeclaredIdentity)
        );

        let mut observed = target();
        observed.hw_family = 0x0b;
        assert_eq!(
            validate_target(observed),
            Err(ConstructionError::WrongHardwareIdentity)
        );

        let mut observed = target();
        observed.num_dies = 2;
        assert_eq!(
            validate_target(observed),
            Err(ConstructionError::WrongHardwareIdentity)
        );

        let mut observed = target();
        observed.uat_ias = 39;
        assert_eq!(
            validate_target(observed),
            Err(ConstructionError::WrongUatGeometry)
        );
    }

    #[test]
    fn split_null_or_wrong_role_uat_fails_closed() {
        let mut observed = resources();
        observed.primary_uat_owner = 0;
        assert_eq!(
            validate_resources(observed),
            Err(ConstructionError::NullUatOwner)
        );

        let mut observed = resources();
        observed.secondary_uat_owner += 0x4000;
        assert_eq!(
            validate_resources(observed),
            Err(ConstructionError::SplitUatOwner)
        );

        let mut observed = resources();
        observed.primary_role = InstanceRole::Secondary;
        assert_eq!(
            validate_resources(observed),
            Err(ConstructionError::WrongRoleTopology)
        );
    }

    #[test]
    fn root_or_shared_cluster_mismatch_fails_closed() {
        let mut observed = resources();
        observed.secondary_root += 0x4000;
        assert_eq!(
            validate_resources(observed),
            Err(ConstructionError::WrongRootPlacement)
        );

        let mut observed = resources();
        observed.shared_cluster_size -= 0x4000;
        assert_eq!(
            validate_resources(observed),
            Err(ConstructionError::WrongSharedClusterSize)
        );
    }

    #[test]
    fn execution_remains_closed_on_power_tuning_and_submission() {
        let gaps = execution_gaps(ExecutionAvailability {
            initdata_layout: true,
            initdata_graph: true,
            all_hw_globals: false,
            perf_tables: false,
            bundle_content: false,
            primary_status_b: false,
            power_sequence: false,
            lifecycle_executor: false,
            submission_transport: false,
        });

        assert!(!gaps.contains(execution_gap::INITDATA_LAYOUT));
        assert!(!gaps.contains(execution_gap::INITDATA_GRAPH));
        assert!(gaps.contains(execution_gap::HW_GLOBALS));
        assert!(gaps.contains(execution_gap::PERF_TABLES));
        assert!(gaps.contains(execution_gap::BUNDLE_CONTENT));
        assert!(gaps.contains(execution_gap::PRIMARY_STATUS_B));
        assert!(gaps.contains(execution_gap::POWER_SEQUENCE));
        assert!(gaps.contains(execution_gap::LIFECYCLE_EXECUTOR));
        assert!(gaps.contains(execution_gap::SUBMISSION_TRANSPORT));
    }

    #[test]
    fn execution_gate_opens_only_when_every_group_is_available() {
        let all = ExecutionAvailability {
            initdata_layout: true,
            initdata_graph: true,
            all_hw_globals: true,
            perf_tables: true,
            bundle_content: true,
            primary_status_b: true,
            power_sequence: true,
            lifecycle_executor: true,
            submission_transport: true,
        };
        assert_eq!(execution_gaps(all).bits(), 0);

        let mut missing = all;
        missing.submission_transport = false;
        assert_eq!(
            execution_gaps(missing).bits(),
            execution_gap::SUBMISSION_TRANSPORT
        );
    }
}
