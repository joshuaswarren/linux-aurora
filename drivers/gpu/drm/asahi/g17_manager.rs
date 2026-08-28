// SPDX-License-Identifier: GPL-2.0-only OR MIT

#![cfg_attr(not(test), allow(dead_code))]

//! T8140/G17P manager construction boundary.
//!
//! This path needs only target identity, init-data geometry, and the retained
//! dual-role resource owner. It does not depend on the AGX2 `HwConfig` and it
//! performs no publication during construction. Explicit queue methods own the
//! resource-0 SKSM configuration and kick-store sequence.

#[cfg(not(test))]
use kernel::prelude::*;

#[cfg(not(test))]
use core::sync::atomic::{fence, Ordering};

#[cfg(not(test))]
use crate::{
    driver::AsahiDevice, g17_initdata, g17_resources, g17_submission, hw::agx3, identity, mmu, regs,
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
    pub(crate) stride: u32,
}

/// The two offsets used by the fixed G17P scratch FIFO slot.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) struct G17SksmScratchOffsets {
    pub(crate) write0: u32,
    pub(crate) write1: u32,
}

impl G17SksmScratchGeometry {
    /// Apply the exact host equation for fixed FIFO slot 9.
    ///
    /// The slot is independent of the SKSM queue ID carried in each write.
    pub(crate) const fn fifo_offsets(
        self,
    ) -> core::result::Result<G17SksmScratchOffsets, ConstructionError> {
        const FIFO_SLOT: u32 = 9;

        let fifo_offset = match self.stride.checked_mul(FIFO_SLOT) {
            Some(offset) => offset,
            None => return Err(ConstructionError::SksmScratchOffsetOverflow),
        };
        let write0 = match self.write0_base.checked_add(fifo_offset) {
            Some(offset) => offset,
            None => return Err(ConstructionError::SksmScratchOffsetOverflow),
        };
        let write1 = match self.write1_base.checked_add(fifo_offset) {
            Some(offset) => offset,
            None => return Err(ConstructionError::SksmScratchOffsetOverflow),
        };

        Ok(G17SksmScratchOffsets { write0, write1 })
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

/// G17P values written by `AGXAcceleratorG16::configureDevice`.
///
/// At `0xfffffe0008a55674..0xfffffe0008a55684`, the host copies the constant
/// at `0xfffffe000714b348` (`00 40 00 00 08 40 00 00`) into accelerator
/// fields `+0x10730/+0x10734`, then writes `0x10` to `+0x10738`.
pub(crate) const T8140_G17P_PLATFORM_CONFIG: G17PPlatformConfig = G17PPlatformConfig {
    sksm_scratch: G17SksmScratchGeometry {
        write0_base: 0x4000,
        write1_base: 0x4008,
        stride: 0x10,
    },
};

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
    WrongSksmScratchGeometry,
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

/// Admit only the exact geometry produced by the pinned G17P host.
pub(crate) fn validate_platform(
    platform: G17PPlatformConfig,
) -> core::result::Result<(), ConstructionError> {
    if platform != T8140_G17P_PLATFORM_CONFIG {
        return Err(ConstructionError::WrongSksmScratchGeometry);
    }
    platform.sksm_scratch.fifo_offsets()?;
    Ok(())
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

/// A queue which has completed the host's configuration pair and QID-enable
/// publication. Private fields prevent constructing an unregistered queue.
#[cfg(not(test))]
pub(crate) struct G17PSksmQueue {
    queue_id: u8,
    scratch: G17SksmScratchGeometry,
    uat_owner: u64,
    storage: g17_resources::G17PClRuntimeStorage,
    producer: g17_submission::G17PClKickProducerState,
    b2_activation: g17_submission::G17PClB2ActivationState,
}

#[cfg(not(test))]
impl G17PManagerConstruction {
    pub(crate) fn new(
        dev: &AsahiDevice,
        soc: &'static agx3::SocConfig,
        platform: G17PPlatformConfig,
    ) -> Result<Self> {
        validate_target(target_observation(soc)).map_err(|_| EINVAL)?;
        validate_platform(platform).map_err(|_| EINVAL)?;

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

    /// Configure one SKSM queue through resource 0 and return its registered
    /// state only after the QID-valid bit is published.
    pub(crate) fn configure_compute_sksm_queue(
        &mut self,
        dev: &AsahiDevice,
        registers: &regs::Resources,
        queue_id: u8,
        completion_selector: u8,
    ) -> Result<G17PSksmQueue> {
        let mut storage = g17_resources::G17PClRuntimeStorage::new(dev, self.uat())?;
        let operand_table_gpu_va = storage.operand_table_gpu_va();
        let support_state_gpu_va = storage.support_state_gpu_va();
        let scheduler_state_gpu_va = storage.scheduler_state_gpu_va();
        storage.with_static_pages_mut(
            |shared_support, support_state, scheduler_state, channel_control| {
                g17_submission::apply_g17p_cl_retained_static_state(
                    shared_support,
                    support_state,
                    scheduler_state,
                    channel_control,
                    operand_table_gpu_va,
                    support_state_gpu_va,
                    scheduler_state_gpu_va,
                )
                .map_err(|_| EINVAL)
            },
        )?;
        let routing = g17_submission::G17PComputeQueueRouting {
            queue_id,
            completion_selector: completion_selector as u32,
            entry_gpu_va: storage.entry_gpu_va(),
        };
        let config =
            g17_submission::prepare_g17p_compute_sksm_queue_config(routing, self.sksm_scratch())
                .map_err(|_| EINVAL)?;

        registers.g17p_sksm_configure_queue(config.write_pair(), config.enable_pair)?;

        Ok(G17PSksmQueue {
            queue_id: routing.queue_id,
            scratch: self.sksm_scratch(),
            uat_owner: self.uat().ttb_base(),
            storage,
            producer: g17_submission::G17PClKickProducerState {
                queue_id,
                num_stamps: 256,
                entry_stride: 0x200,
                completion_selector,
                qos: g17_submission::G17PClQosConfig {
                    queue_byte_32: 1,
                    class_40: 8,
                    word_48: 0xffff,
                },
                submitted: 0,
                current_timestamp: 1,
                base_timestamp: 1,
                previous_valid: false,
                previous_timestamp: 0,
            },
            b2_activation: g17_submission::G17PClB2ActivationState::Pending,
        })
    }

    /// Publish one complete CL entry, then perform the queue's ordered SKSM
    /// notification. The first post-registration entry publishes the retained
    /// B2 state; later entries keep those provider-owned pages unchanged.
    pub(crate) fn submit_compute_entry(
        &mut self,
        registers: &regs::Resources,
        queue: &mut G17PSksmQueue,
        operands: g17_submission::G17PClKickEntryOperands<'_>,
    ) -> Result<u64> {
        if queue.scratch != self.sksm_scratch() || queue.uat_owner != self.uat().ttb_base() {
            return Err(EINVAL);
        }
        let prepared = g17_submission::prepare_g17p_cl_kick_entry(queue.producer, operands)
            .map_err(|_| EINVAL)?;
        queue.storage.write_entry(
            prepared.entry_offset as usize,
            prepared.zero_length as usize,
            &prepared.entry,
        )?;

        if queue.b2_activation == g17_submission::G17PClB2ActivationState::Pending {
            let channel_control_gpu_va = queue.storage.channel_control_gpu_va();
            queue.b2_activation =
                self.resources
                    .with_b2_pages_mut(|computed, region_1, region_2| {
                        g17_submission::apply_g17p_cl_b2_primary_state(
                            queue.b2_activation,
                            computed,
                            region_1,
                            region_2,
                            channel_control_gpu_va,
                        )
                        .map_err(|_| EINVAL)
                    })?;
        }

        fence(Ordering::SeqCst);
        queue.producer = prepared.producer_after;
        let timestamp = prepared.current_timestamp;
        self.add_sksm_kicks(registers, queue, 1)?;
        fence(Ordering::SeqCst);
        Ok(timestamp)
    }

    /// Completion ingress for the descriptor-provided completed-kick count.
    pub(crate) fn complete_compute_entries(
        &self,
        queue: &mut G17PSksmQueue,
        completed_count: u32,
    ) -> Result {
        if queue.scratch != self.sksm_scratch() || queue.uat_owner != self.uat().ttb_base() {
            return Err(EINVAL);
        }
        queue.producer = g17_submission::complete_g17p_cl_kicks(queue.producer, completed_count)
            .map_err(|_| EINVAL)?;
        Ok(())
    }

    /// Publish newly appended kicks for a queue returned by
    /// [`Self::configure_compute_sksm_queue`].
    pub(crate) fn add_sksm_kicks(
        &self,
        registers: &regs::Resources,
        queue: &mut G17PSksmQueue,
        add_count: u32,
    ) -> Result {
        if queue.scratch != self.sksm_scratch() || queue.uat_owner != self.uat().ttb_base() {
            return Err(EINVAL);
        }

        let pair = g17_submission::prepare_sksm_add_kicks_mmio(
            g17_submission::SksmAddKicksTarget::G17P,
            queue.queue_id,
            add_count,
            queue.scratch,
        )
        .map_err(|_| EINVAL)?
        .write_pair();
        registers.g17p_sksm_publish_write_pair(pair)
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
        let platform = T8140_G17P_PLATFORM_CONFIG;
        assert_eq!(platform.sksm_scratch.write0_base, 0x4000);
        assert_eq!(platform.sksm_scratch.write1_base, 0x4008);
        assert_eq!(platform.sksm_scratch.stride, 0x10);
        assert_eq!(
            platform.sksm_scratch.fifo_offsets(),
            Ok(G17SksmScratchOffsets {
                write0: 0x4090,
                write1: 0x4098,
            })
        );
        assert_eq!(validate_platform(platform), Ok(()));
    }

    #[test]
    fn non_g17p_scratch_geometry_fails_before_resource_allocation() {
        for geometry in [
            G17SksmScratchGeometry {
                write0_base: 0,
                ..T8140_G17P_PLATFORM_CONFIG.sksm_scratch
            },
            G17SksmScratchGeometry {
                write1_base: 0,
                ..T8140_G17P_PLATFORM_CONFIG.sksm_scratch
            },
            G17SksmScratchGeometry {
                stride: 0,
                ..T8140_G17P_PLATFORM_CONFIG.sksm_scratch
            },
        ] {
            assert_eq!(
                validate_platform(G17PPlatformConfig {
                    sksm_scratch: geometry
                }),
                Err(ConstructionError::WrongSksmScratchGeometry)
            );
        }
    }

    #[test]
    fn scratch_geometry_overflow_fails_before_resource_allocation() {
        for geometry in [
            G17SksmScratchGeometry {
                write0_base: 0,
                write1_base: 0,
                stride: u32::MAX,
            },
            G17SksmScratchGeometry {
                write0_base: u32::MAX,
                write1_base: 0,
                stride: 1,
            },
            G17SksmScratchGeometry {
                write0_base: 0,
                write1_base: u32::MAX,
                stride: 1,
            },
        ] {
            assert_eq!(
                geometry.fifo_offsets(),
                Err(ConstructionError::SksmScratchOffsetOverflow)
            );
        }
    }

    #[test]
    fn non_t8140_identity_and_uat_geometry_are_invalid() {
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
    fn split_null_or_wrong_role_uat_is_invalid() {
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
    fn root_or_shared_cluster_mismatch_is_invalid() {
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
}
