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
use kernel::time::{delay::fsleep, Delta};

#[cfg(not(test))]
use core::sync::atomic::{fence, Ordering};

#[cfg(not(test))]
use crate::{
    driver::AsahiDevice, g17_completion, g17_compute, g17_initdata, g17_lifecycle, g17_render,
    g17_resources, g17_submission, g17_uapi, hw::agx3, identity, mmu, regs,
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
    num_clusters: u32,
    render_storage: Option<g17_resources::G17PUserRenderStorage>,
    render_tracker: Option<G17PPartialOpeningRenderTracker>,
    render_bind: Option<mmu::VmBind>,
    render_submission_ordinal: u32,
    runtime_tick_staged: bool,
}

/// A queue which has completed the host's configuration pair and QID-enable
/// publication. Private fields prevent constructing an unregistered queue.
#[cfg(not(test))]
pub(crate) struct G17PSksmQueue {
    queue_id: u8,
    scratch: G17SksmScratchGeometry,
    uat_owner: u64,
    disable_pair: g17_submission::G17SksmOrderedWritePair,
    registered: bool,
    storage: g17_resources::G17PClRuntimeStorage,
    compute: g17_resources::G17PComputeDescriptorStorage,
    producer: g17_submission::G17PClKickProducerState,
    submission_ordinal: u32,
    b2_activation: g17_submission::G17PClB2ActivationState,
}

/// Command-owned fields used to build one compute descriptor and CL entry.
/// The queue supplies descriptor storage, sequencing, preemption storage, and
/// the retained support graph.
#[cfg(not(test))]
pub(crate) struct G17PComputeSubmissionContext<'a> {
    pub(crate) context_id: u32,
    pub(crate) dispatch_identity: u64,
    pub(crate) execution_gate: u64,
    pub(crate) timestamps: g17_uapi::ComputeTimestampAddresses,
    pub(crate) descriptor_flag_4c: bool,
    pub(crate) descriptor_flag_5c8: bool,
    pub(crate) converted_command_timestamp: u64,
    pub(crate) barriers: &'a [g17_submission::G17PClBarrierDependency],
    pub(crate) mcache: Option<g17_submission::G17PClMcacheAperture>,
    pub(crate) rce_kind: u8,
    pub(crate) auxiliary: u64,
}

/// User-VM aliases and its active TTBAT binding. The live runtime retains this
/// object until the entry completes or both processors stop during recovery.
#[cfg(not(test))]
pub(crate) struct G17PComputeUserBinding {
    mappings: g17_resources::G17PComputeUserMappings,
    _vm_bind: mmu::VmBind,
}

#[cfg(not(test))]
pub(crate) struct G17PComputeTimeoutSnapshot {
    pub(crate) graph: g17_resources::G17PComputeGraphSnapshot,
    pub(crate) channel: g17_resources::G17PComputeChannelSnapshot,
    pub(crate) control: g17_resources::G17PControlCounters,
}

/// The two complete queue groups exposed by the exact first graphics publish.
#[cfg(not(test))]
pub(crate) struct G17PPartialOpeningRenderSubmission {
    pub(crate) fragment_queue_gpu_va: u64,
    pub(crate) fragment_queue_write_index: u32,
    pub(crate) tiling_queue_gpu_va: u64,
    pub(crate) tiling_queue_write_index: u32,
}

/// Completion cursors retained across observations of one graphics pair.
#[cfg(not(test))]
pub(crate) struct G17PPartialOpeningRenderTracker {
    tiling_previous: g17_completion::QueueIndices,
    fragment_previous: g17_completion::QueueIndices,
    tiling_prefix: u32,
    fragment_prefix: u32,
}

#[cfg(not(test))]
impl G17PManagerConstruction {
    pub(crate) fn new(
        dev: &AsahiDevice,
        soc: &'static agx3::SocConfig,
        platform: G17PPlatformConfig,
        num_clusters: u32,
    ) -> Result<KBox<Self>> {
        let uat = KBox::new(mmu::Uat::new_t8140(dev, true)?, GFP_KERNEL)?;
        Self::new_with_uat(dev, soc, platform, num_clusters, uat)
    }

    /// Kept out of line and heap-returned: the construction locals plus the
    /// assembled manager otherwise land several times in the probe-path stack
    /// frames, which overflowed the 32 KiB task stack on hardware.
    #[inline(never)]
    pub(crate) fn new_with_uat(
        dev: &AsahiDevice,
        soc: &'static agx3::SocConfig,
        platform: G17PPlatformConfig,
        num_clusters: u32,
        uat: KBox<mmu::Uat>,
    ) -> Result<KBox<Self>> {
        if num_clusters == 0 {
            return Err(EINVAL);
        }
        validate_target(target_observation(soc)).map_err(|_| EINVAL)?;
        validate_platform(platform).map_err(|_| EINVAL)?;

        let resources = g17_resources::G17ResourceOwner::new_with_uat(dev, uat)?;
        g17_resources::validate_handoff(resources.handoff()).map_err(|_| EINVAL)?;
        validate_resources(resource_observation(resources.handoff(), resources.uat()))
            .map_err(|_| EINVAL)?;

        Ok(KBox::new(
            Self {
                resources,
                platform,
                num_clusters,
                render_storage: None,
                render_tracker: None,
                render_bind: None,
                render_submission_ordinal: 0,
                runtime_tick_staged: false,
            },
            GFP_KERNEL,
        )?)
    }

    pub(crate) fn config(&self) -> &'static T8140ManagerConfig {
        &T8140_MANAGER_CONFIG
    }

    pub(crate) fn handoff(&self) -> &g17_resources::G17ResourceHandoff {
        self.resources.handoff()
    }

    pub(crate) fn control_counters(&mut self) -> Result<g17_resources::G17PControlCounters> {
        self.resources.control_counters()
    }

    pub(crate) fn control_opening_effect(
        &mut self,
    ) -> Result<g17_resources::G17PControlOpeningEffect> {
        self.resources.control_opening_effect()
    }

    /// Stage the same queue-pair-zero TA/3D bootstrap used by the
    /// output-positive source path. The VDM stream is a driver-owned terminate
    /// command, so this establishes scheduler lifecycle without user BOs.
    pub(crate) fn stage_compute_bootstrap_render(
        &mut self,
        dev: &AsahiDevice,
    ) -> Result<mmu::Vm> {
        let kernel_range = self.resources.uat().kernel_va_range().inspect_err(|error| {
            dev_err!(
                dev.as_ref(),
                "G17P bootstrap: kernel VA range failed ({:?})\n",
                error
            );
        })?;
        let vm = self
            .new_user_vm_inner(0x8140_b007, kernel_range, true)
            .inspect_err(|error| {
                dev_err!(
                    dev.as_ref(),
                    "G17P bootstrap: render VM setup failed ({:?})\n",
                    error
                );
            })?;
        let mut parameters = g17_render::G17pRenderParameters::source_rsrc8();
        parameters.encoder = g17_resources::G17P_BOOTSTRAP_VDM_VA;
        parameters.process_empty_tiles = false;
        parameters.tile_config &= !0x1_0000;
        let timestamp = g17_uapi::UapiTimestamp {
            handle: 0,
            offset: 0,
        };
        let command = g17_uapi::TranslatedRenderCommand {
            flags: 0,
            vdm_base: parameters.encoder,
            parameters,
            vertex_attachments: g17_uapi::UapiAttachmentList::EMPTY,
            fragment_attachments: g17_uapi::UapiAttachmentList::EMPTY,
            vertex_timestamps: g17_uapi::UapiTimestamps {
                start: timestamp,
                end: timestamp,
            },
            fragment_timestamps: g17_uapi::UapiTimestamps {
                start: timestamp,
                end: timestamp,
            },
        };
        if self.render_tracker.is_some()
            || self.render_storage.is_some()
            || self.render_bind.is_some()
            || self.render_submission_ordinal != 0
        {
            return Err(EBUSY);
        }
        self.render_storage = Some(
            self.resources
                .new_user_render_storage(
                    dev,
                    &vm,
                    &command,
                    self.num_clusters,
                    // The driver-owned opening command has its own Shared
                    // state owner. Its first TA/3D pair uses native tags 0/1;
                    // user command buffers allocate from the session owner
                    // below and are not advanced by this bootstrap object.
                    [0, 1],
                    0,
                    pm_generation.map(|plan| plan.0),
                    &mut no_retained_sksm_backing,
                )
                .inspect_err(|error| {
                    dev_err!(
                        dev.as_ref(),
                        "G17P bootstrap: render graph setup failed ({:?})\n",
                        error
                    );
                })?,
        );
        let storage = self.render_storage.as_mut().ok_or(ENODEV)?;
        if !storage.is_reachable_from(self.resources.uat(), &vm) {
            return Err(EFAULT);
        }
        let binding = self.resources.uat().bind(&vm)?;
        if binding.slot() != 1 {
            return Err(EFAULT);
        }
        self.resources
            .uat()
            .install_t8140_partial_opening_shared_render_roots(&binding)?;
        self.render_bind = Some(binding);
        Ok(vm)
    }

    /// Apply the measured post-opening context split after primary
    /// control-done and before either first-work producer becomes visible.
    pub(crate) fn split_compute_bootstrap_render_contexts(&self) -> Result {
        let binding = self.render_bind.as_ref().ok_or(ENODEV)?;
        self.resources
            .uat()
            .split_t8140_partial_opening_render_roots(binding)?;
        if !self
            .resources
            .uat()
            .t8140_first_work_render_context_ready(binding)
        {
            return Err(EFAULT);
        }
        Ok(())
    }

    /// Publish the staged bootstrap pair after primary control-done.
    pub(crate) fn publish_staged_compute_bootstrap_render(
        &mut self,
    ) -> Result<g17_submission::PreparedG17PPartialOpeningWorkPair> {
        if self.render_tracker.is_some()
            || self.render_bind.is_none()
            || self.render_submission_ordinal != 0
        {
            return Err(EBUSY);
        }
        if !self.resources.uat().t8140_first_work_render_context_ready(
            self.render_bind.as_ref().ok_or(ENODEV)?,
        ) {
            return Err(EFAULT);
        }
        let (fragment_queue, tiling_queue, previous, late_tiling) = {
            let storage = self.render_storage.as_mut().ok_or(ENODEV)?;
            // The successful first-partial path keeps both inner groups blank
            // until primary control-done. Restore 3D now; TA remains held by
            // the late publication callback below.
            storage.stage_fragment(0)?;
            let previous = storage.snapshot()?;
            let late_tiling = storage.prepare_late_tiling_publication(0)?;
            let (fragment_queue, tiling_queue) = storage.queue_gpu_vas();
            (fragment_queue, tiling_queue, previous, late_tiling)
        };
        let (prepared, render_bind) = self.resources.publish_partial_opening_pair(
            None,
            true,
            fragment_queue,
            3,
            tiling_queue,
            3,
            move || late_tiling.publish(),
        )?;
        if render_bind.is_some() {
            return Err(EINVAL);
        }
        self.render_tracker = Some(G17PPartialOpeningRenderTracker {
            tiling_previous: previous.tiling,
            fragment_previous: previous.fragment,
            tiling_prefix: 3,
            fragment_prefix: 3,
        });
        Ok(prepared)
    }

    /// Match the source bootstrap fence: both three-item inner queues and
    /// both outer channel consumers must retire before the graph can drop.
    pub(crate) fn wait_compute_bootstrap_render_retirement(&mut self) -> Result {
        const POLLS: usize = 500;
        let mut last = None;
        for _ in 0..POLLS {
            fence(Ordering::Acquire);
            let state = self.render_storage.as_mut().ok_or(ENODEV)?.snapshot()?;
            let outer = self.resources.partial_opening_outer_counters()?;
            // m1n1's fence owns the prefix once done/read cover the three
            // items and the outer consumers reach producer one. The queue may
            // already advertise a later write prefix (the positive run is
            // 3/3/6), so equality with write=3 is not an ownership rule.
            let inner_retired = state.tiling.done >= 3
                && state.tiling.read >= 3
                && state.tiling.write >= 3
                && state.fragment.done >= 3
                && state.fragment.read >= 3
                && state.fragment.write >= 3;
            let outer_retired = outer
                .into_iter()
                .all(|counters| counters[2] == 1 && counters[..2] == [counters[2]; 2]);
            if inner_retired && outer_retired {
                self.render_tracker = None;
                self.render_bind = None;
                self.render_storage = None;
                self.render_submission_ordinal = 0;
                self.resources
                    .uat()
                    .restore_t8140_kernel_context_after_bootstrap_render()?;
                return Ok(());
            }
            last = Some((state, outer));
            fsleep(Delta::from_millis(1));
        }
        if let Some((state, outer)) = last {
            pr_err!(
                "G17P bootstrap timeout: TA {:?}, 3D {:?}, outer 3D/TA {:?}\n",
                state.tiling,
                state.fragment,
                outer
            );
        }
        Err(ETIMEDOUT)
    }

    pub(crate) fn prepare_compute_runtime_class1(&mut self) -> Result {
        self.resources.prepare_compute_runtime_class1()
    }

    pub(crate) fn publish_compute_runtime_control_record(
        &mut self,
        index: usize,
    ) -> Result<u32> {
        self.resources.publish_compute_runtime_control_record(index)
    }

    pub(crate) fn prepare_compute_runtime_class2(&mut self) -> Result {
        self.resources.prepare_compute_runtime_class2()
    }

    pub(crate) fn rewrite_compute_dispatch_record(&mut self) -> Result {
        self.resources.rewrite_compute_dispatch_record()
    }

    pub(crate) fn uat(&self) -> &mmu::Uat {
        self.resources.uat()
    }

    pub(crate) fn mark_firmware_cache_flush_ready(&self) {
        self.resources.mark_firmware_cache_flush_ready();
    }

    fn validate_partial_opening_resources(&self) -> Result {
        let binding = self.resources.partial_opening_binding();
        let context = g17_submission::G17P_PARTIAL_OPENING_CONTEXT;
        let expected_fwctl = binding
            .kernel_va_base
            .checked_add(g17_submission::G17P_PARTIAL_OPENING_FWCTL_OFFSET)
            .ok_or(EINVAL)?;
        let addresses_ready = binding.scheduler_page
            == g17_submission::G17P_PARTIAL_OPENING_SCHEDULER_GPU_VA
            && binding.primary_index_page
                == g17_submission::G17P_PARTIAL_OPENING_PRIMARY_INDEX_GPU_VA
            && binding.fwctl == expected_fwctl;
        let context_ready = context.context0_root_slot == 0
            && context.context0_context_id == 0
            && context.render_root_slot == 1
            && context.render_context_id == 1
            && context.context0_empty_high_root_slot == 0
            && context.render_empty_high_root_slot == 1
            && context.transport_pair == 0
            && context.descriptor_pair == 0
            && context.shared_queue_pair_namespace == 0
            && context.optional_identity_context_id == 1
            && context.descriptor_context_id == 1
            && context.scheduler_class == 2
            && context.work_doorbell_channel == 0
            && context.context0_low_root_present_at_first_work
            && context.render_low_root_present_at_first_work
            && !context.source_firmware_high_root_table_resident;
        let roots_ready = match self.render_bind.as_ref() {
            Some(binding) => self
                .resources
                .uat()
                .t8140_partial_opening_shared_render_context_ready(binding),
            None => self
                .resources
                .uat()
                .t8140_partial_opening_kernel_contexts_ready(),
        };
        if !addresses_ready || !context_ready || !roots_ready {
            dev_err!(
                self.resources.dev().as_ref(),
                "G17P render: opening contract addresses={} context={} roots={} (scheduler {:#x}, index {:#x}, fwctl {:#x}/{:#x})\n",
                addresses_ready,
                context_ready,
                roots_ready,
                binding.scheduler_page,
                binding.primary_index_page,
                binding.fwctl,
                expected_fwctl
            );
            return Err(EINVAL);
        }
        Ok(())
    }

    /// Apply first-work status immediately before the caller sends primary
    /// control-done 0x84.
    pub(crate) fn prepare_partial_opening_control_done(&mut self) -> Result {
        self.validate_partial_opening_resources().inspect_err(|error| {
            dev_err!(
                self.resources.dev().as_ref(),
                "G17P render: opening validation failed ({:?})\n",
                error
            );
        })?;
        self.resources
            .apply_partial_opening_pre_control_status()
            .inspect_err(|error| {
                dev_err!(
                    self.resources.dev().as_ref(),
                    "G17P render: status-B update failed ({:?})\n",
                    error
                );
            })
    }

    /// Publish the first-partial operand table in the control-done to
    /// first-work interval.
    pub(crate) fn publish_partial_opening_operand_table(&mut self) -> Result {
        self.resources.publish_partial_opening_operand_table()
    }

    /// Publish the exact first 3D/TA outer-ring pair. The callback installs
    /// the late TA descriptor, item-ring, and queue-head state between the 3D
    /// producer and the TA outer slot. The retained live runtime sends the
    /// returned channel-0 doorbell exactly once.
    pub(crate) fn submit_partial_opening_render<F>(
        &mut self,
        submission: G17PPartialOpeningRenderSubmission,
        publish_late_tiling_state: F,
    ) -> Result<(
        G17PPartialOpeningRenderTracker,
        g17_submission::PreparedG17PPartialOpeningWorkPair,
    )>
    where
        F: FnOnce(),
    {
        self.validate_partial_opening_resources()?;
        if submission.fragment_queue_write_index != 3 || submission.tiling_queue_write_index != 3 {
            return Err(EINVAL);
        }
        let (prepared, render_bind) = self.resources.publish_partial_opening_pair(
            None,
            false,
            submission.fragment_queue_gpu_va,
            submission.fragment_queue_write_index,
            submission.tiling_queue_gpu_va,
            submission.tiling_queue_write_index,
            publish_late_tiling_state,
        )?;
        if render_bind.is_some() {
            return Err(EINVAL);
        }
        if prepared.paired_doorbell != g17_submission::G17P_PARTIAL_OPENING_PAIRED_DOORBELL
            || prepared.paired_doorbell != 0x0083_0000_0000_0000
        {
            return Err(EINVAL);
        }
        let tracker = G17PPartialOpeningRenderTracker {
            tiling_previous: g17_completion::QueueIndices {
                done: 0,
                read: 0,
                write: submission.tiling_queue_write_index,
            },
            fragment_previous: g17_completion::QueueIndices {
                done: 0,
                read: 0,
                write: submission.fragment_queue_write_index,
            },
            tiling_prefix: submission.tiling_queue_write_index,
            fragment_prefix: submission.fragment_queue_write_index,
        };
        Ok((tracker, prepared))
    }

    /// Classify paired render progress from queue indices plus the scheduler
    /// job list and both descriptor timestamp quartets.
    pub(crate) fn observe_partial_opening_render_completion(
        &self,
        tracker: &mut G17PPartialOpeningRenderTracker,
        tiling_current: g17_completion::QueueIndices,
        fragment_current: g17_completion::QueueIndices,
        execution: g17_completion::RenderExecutionState,
    ) -> Result<g17_completion::PairedRenderCompletion> {
        let completion = g17_completion::observe_paired_render_completion(
            tracker.tiling_previous,
            tiling_current,
            tracker.tiling_prefix,
            tracker.fragment_previous,
            fragment_current,
            tracker.fragment_prefix,
            execution,
        )
        .map_err(|_| EINVAL)?;
        tracker.tiling_previous = tiling_current;
        tracker.fragment_previous = fragment_current;
        Ok(completion)
    }

    pub(crate) fn new_user_vm(
        &mut self,
        id: u64,
        kernel_range: core::ops::Range<u64>,
    ) -> Result<mmu::Vm> {
        self.new_user_vm_inner(id, kernel_range, false)
    }

    fn new_user_vm_inner(
        &mut self,
        id: u64,
        kernel_range: core::ops::Range<u64>,
        bootstrap_vdm: bool,
    ) -> Result<mmu::Vm> {
        let vm = self
            .resources
            .uat()
            .new_vm(id, kernel_range)
            .inspect_err(|error| {
                dev_err!(
                    self.resources.dev().as_ref(),
                    "G17P VM: context allocation failed ({:?})\n",
                    error
                );
            })?;
        self.resources
            .install_user_vm_aliases(&vm, bootstrap_vdm)
            .inspect_err(|error| {
                dev_err!(
                    self.resources.dev().as_ref(),
                    "G17P VM: fixed aliases failed ({:?})\n",
                    error
                );
            })?;
        Ok(vm)
    }

    pub(crate) fn map_timestamp_buffer(
        &self,
        mut bo: crate::gem::ObjectRef,
        range: core::ops::Range<usize>,
    ) -> Result<mmu::KernelMapping> {
        bo.map_range_into_range(
            self.resources.uat().kernel_vm(),
            range,
            self.resources.uat().kernel_va_range()?,
            mmu::UAT_PGSZ as u64,
            mmu::PROT_FW_SHARED_RW,
            false,
        )
    }

    /// Release one stopped firmware session without replacing its UAT owner.
    pub(crate) fn release_runtime_objects_after_processor_stop(&mut self) {
        self.render_bind = None;
        self.render_tracker = None;
        self.render_storage = None;
        self.render_submission_ordinal = 0;
    }

    /// Restore cold initdata in the existing mappings after both CPUs stop.
    pub(crate) fn reset_session_after_processor_stop(&mut self) -> Result {
        if self.render_bind.is_some()
            || self.render_tracker.is_some()
            || self.render_storage.is_some()
        {
            return Err(EBUSY);
        }
        self.resources.reset_session_after_processor_stop()
    }

    pub(crate) fn retained_render_needs_recycle(&self, vm: &mmu::Vm) -> bool {
        let retained = self.render_storage.is_some();
        let same_vm = self
            .render_bind
            .as_ref()
            .is_some_and(|binding| binding.matches(vm));
        g17_lifecycle::retained_render_requires_recycle(
            retained,
            same_vm,
            self.render_submission_ordinal,
        )
    }

    pub(crate) fn begin_translated_render(
        &mut self,
        dev: &AsahiDevice,
        vm: &mmu::Vm,
        command: &g17_uapi::TranslatedRenderCommand,
        registers: &regs::Resources,
        registration_power: &mut impl FnMut(&mut Self) -> Result,
    ) -> Result<g17_submission::PreparedG17PPartialOpeningWorkPair> {
        self.select_render_slot(queue_pair.slot())?;
        let descriptor_context_id = execution_context.context_id() as u16;
        if !execution_context.matches(vm) || !execution_context.is_current() {
            return Err(EFAULT);
        }
        if descriptor_context_id < 64 {
            self.released_render_contexts &= !(1u64 << descriptor_context_id);
        }
        if self.render_tracker.is_some() || self.render_hardware_buffer_id.is_some() {
            return Err(EBUSY);
        }
        // Before anything is allocated for this submission, deal with a graph
        // whose ring producer has drifted ahead of the ordinal. Nothing this
        // function does later can recover from it, and leaving it retires the
        // client for good.
        self.recycle_desynced_render_storage()?;
        if g17_resources::g17p_render_tvb_mode() >= 2 {
            let required=g17_resources::required_render_tvb_blocks(&command.parameters,self.num_clusters).ok_or(EINVAL)?;
            if self.render_storage.as_ref().is_some_and(|storage| storage.tvb_capacity()<required) {
                self.require_retired_render_owner()?;
                self.resources.grow_render_tvb(self.render_storage.as_mut().ok_or(EIO)?,vm,required)?;
            }
        }
        self.render_auxiliary_timestamp = G17P_NATIVE_RENDER_KICK_AUXILIARY;
        // The TA HardwareBufferID names this storage's PBDesc slot and its
        // retained parameter-management graph was encoded against it, so it
        // belongs to the storage for as long as the storage lives: a retained
        // storage re-acquires its own reserved ID, and only a storage about to
        // be built takes a fresh one.
        //
        // Allocating per submission handed out whichever ID the shared LIFO had
        // on top, which is the same number only while a single client renders.
        // As soon as another slot held an ID -- in flight, or pinned by an
        // abandoned render -- the next restage here received a different ID
        // and prepare_retained_scene refused it with ENOTSUPP, which reached
        // the compositor as a lost device.
        let retained_hardware_buffer_id = self
            .render_storage
            .as_ref()
            .and_then(g17_resources::G17PUserRenderStorage::retained_hardware_buffer_id);
        let (hardware_buffer_id, parameter_buffer_token) = match retained_hardware_buffer_id {
            Some(id) => self
                .render_hardware_buffer_ids
                .acquire_reserved_with_token(id)
                .map_err(|_| EBUSY)?,
            None => self
                .render_hardware_buffer_ids
                .allocate_with_token()
                .map_err(|_| EBUSY)?,
        };
        self.render_hardware_buffer_id = Some(hardware_buffer_id);
        self.render_hardware_buffer_published = false;
        let command_buffer_state_word = match self.allocate_active_render_command_buffer_state() {
            Ok(word) => word,
            Err(error) => {
                self.render_hardware_buffer_id = None;
                self.render_hardware_buffer_ids
                    .release(hardware_buffer_id)
                    .map_err(|_| EIO)?;
                return Err(error);
            }
        };

        let mut command = *command;
        command.parameters.ta_hardware_buffer_id = hardware_buffer_id;
        if crate::util::g17p_verbose() {
            dev_info!(
                dev.as_ref(),
                "G17P render: allocated TA HardwareBufferID {} for ordinal {}\n",
                hardware_buffer_id,
                self.render_submission_ordinal,
            );
        }
        let result = self.begin_translated_render_with_hardware_buffer(
            dev,
            vm,
            queue_pair,
            execution_context,
            descriptor_context_id,
            &command,
            parameter_buffer_token,
            [command_buffer_state_word; 2],
            registers,
            registration_power,
        );
        if result.is_err() && !self.render_hardware_buffer_published {
            // acquire() stages a copy of the completed FList owner. Until
            // outer publication it holds no firmware reference and must be
            // cancelled with the HardwareBufferID reservation. Otherwise a
            // fallible PM allocation leaves every later VM handoff EBUSY.
            self.prepared_render_flist = None;
            self.render_auxiliary_timestamp = 0;
            self.release_active_render_command_buffer_states()?;
            let id = self.render_hardware_buffer_id.take().ok_or(EIO)?;
            self.render_hardware_buffer_ids.release(id).map_err(|_| EIO)?;
        }
        result
    }

    fn begin_translated_render_with_hardware_buffer(
        &mut self,
        dev: &AsahiDevice,
        vm: &mmu::Vm,
        queue_pair: g17_resources::G17PRenderQueuePair,
        execution_context: Arc<mmu::T8140ComputeExecutionContext>,
        descriptor_context_id: u16,
        command: &g17_uapi::TranslatedRenderCommand,
        parameter_buffer_token: u64,
        command_buffer_state_words: [u32; 2],
        registers: &regs::Resources,
        registration_power: &mut impl FnMut(&mut Self) -> Result,
    ) -> Result<g17_submission::PreparedG17PPartialOpeningWorkPair> {
        let render_sksm = *crate::module_parameters::g17p_render_sksm.value();
        let native_doorbell_mode =
            *crate::module_parameters::g17p_render_native_doorbells.value();
        let split_launch = *crate::module_parameters::g17p_render_split_launch.value() != 0;
        if native_doorbell_mode > 3
            || (native_doorbell_mode != 0 && split_launch)
            || (g17p_native_fragment_first_mode(native_doorbell_mode) && render_sksm == 0)
        {
            return Err(EINVAL);
        }
        // Both render paths publish tag-16 when it is armed -- the SKSM
        // staging path through `stage_fragment_kick`, and the plain path
        // through `stage_fragment`. The absolute prefix includes all earlier
        // groups in the retained ring; it is not merely this group's 3/4
        // items. The same target feeds both outer slots and the tracker.
        let published_prefix =
            g17_resources::g17p_render_published_prefix(self.render_submission_ordinal)?;
        if self.render_tracker.is_some() {
            return Err(EBUSY);
        }
        let retained = self.render_storage.is_some();
        if retained
            && !self
                .render_bind
                .as_ref()
                .is_some_and(|binding| binding.matches(vm))
        {
            return Err(EBUSY);
        }
        if retained {
            self.render_storage
                .as_mut()
                .ok_or(ENODEV)?
                .restage_command(
                    dev,
                    self.resources.uat(),
                    vm,
                    command,
                    self.num_clusters,
                    command_buffer_state_words,
                    self.render_submission_ordinal,
                )?;
        } else {
            if self.render_submission_ordinal != 0 || self.render_bind.is_some() {
                return Err(EIO);
            }
            let mut retained_sksm_backing = self
                .render_sksm
                .as_mut()
                .and_then(|queues| queues.backing.take());
            let pm_generation = self.prepare_render_pm_manager_generation()?;
            let storage = self.resources.new_user_render_storage(
                dev,
                vm,
                queue_pair,
                Some(execution_context.clone()),
                descriptor_context_id,
                Some(render_root_slot),
                command,
                self.num_clusters,
                command_buffer_state_words,
                self.render_submission_ordinal,
                pm_generation.map(|plan| plan.0),
                &mut retained_sksm_backing,
            );
            self.render_storage = Some(match storage {
                Ok(storage) if retained_sksm_backing.is_none() => storage,
                Ok(_) => return Err(Self::render_stage_fail(
                    "new-user-render-sksm-backing-unclaimed", EIO,
                )),
                Err(error) => {
                    if let Some(backing) = retained_sksm_backing.take() {
                        let queues = self.render_sksm.as_mut().ok_or(EIO)?;
                        if queues.backing.replace(backing).is_some() {
                            return Err(EIO);
                        }
                    }
                    return Err(Self::render_stage_fail("new-user-render-storage", error));
                }
            });
            self.commit_render_pm_manager_generation(pm_generation)
                .map_err(|error| Self::render_stage_fail("commit-pm-manager-generation", error))?;
            // The fresh graph was encoded against this ID; keep the ID off the
            // shared free list for as long as the storage lives.
            if let Some(id) = self
                .render_storage
                .as_ref()
                .and_then(g17_resources::G17PUserRenderStorage::retained_hardware_buffer_id)
            {
                self.render_hardware_buffer_ids
                    .reserve(id)
                    .map_err(|_| Self::render_stage_fail("reserve-hardware-buffer-id", EIO))?;
            }
            if self.render_submission_ordinal != 0 {
                let parameter_page_list_vas = self
                    .render_storage
                    .as_ref()
                    .ok_or(ENODEV)?
                    .parameter_buffer_page_list_vas()?;
                let storage = self.render_storage.as_mut().ok_or(ENODEV)?;
                storage
                    .rebase_empty_queue_cursors(self.render_submission_ordinal)
                    .map_err(|error| Self::render_stage_fail("rebase-new-vm-queue", error))?;
                storage.restage_command(
                        dev,
                        vm,
                        command,
                        self.num_clusters,
                        command_buffer_state_words,
                        self.render_submission_ordinal,
                    )
                    .inspect_err(|error| {
                        dev_err!(
                            dev.as_ref(),
                            "G17P render: retained storage setup failed ({:?})\n",
                            error
                        );
                    })?,
            );
        }
        let (fragment_queue, tiling_queue, previous, late_tiling) = {
            let storage = self.render_storage.as_mut().ok_or(ENODEV)?;
            if !storage.is_reachable_from(self.resources.uat(), vm) {
                return Err(EFAULT);
            }
            storage
                .stage_fragment(self.render_submission_ordinal)
                .inspect_err(|error| {
                    dev_err!(dev.as_ref(), "G17P render: fragment stage failed ({:?})\n", error);
                })?;
            let previous = storage.snapshot().inspect_err(|error| {
                dev_err!(dev.as_ref(), "G17P render: initial snapshot failed ({:?})\n", error);
            })?;
            let late_tiling = storage
                .prepare_late_tiling_publication(self.render_submission_ordinal)
                .inspect_err(|error| {
                    dev_err!(dev.as_ref(), "G17P render: tiling stage failed ({:?})\n", error);
                })?;
            let (fragment_queue, tiling_queue) = storage.queue_gpu_vas();
            (fragment_queue, tiling_queue, previous, late_tiling)
        };
        let (prepared, render_bind) = self
            .resources
            .publish_partial_opening_pair(
                if retained { None } else { Some(vm) },
                retained,
                fragment_queue,
                3,
                tiling_queue,
                3,
                move || late_tiling.publish(),
            )
            .inspect_err(|error| {
                dev_err!(dev.as_ref(), "G17P render: outer publish failed ({:?})\n", error);
            })?;
        if retained {
            if render_bind.is_some() {
                return Err(EINVAL);
            }
        } else {
            self.render_bind = Some(render_bind.ok_or(EINVAL)?);
        }
        self.render_tracker = Some(G17PPartialOpeningRenderTracker {
            tiling_previous: previous.tiling,
            fragment_previous: previous.fragment,
            tiling_prefix: 3,
            fragment_prefix: 3,
        });
        Ok(prepared)
    }

    pub(crate) fn wait_translated_render_completion(
        &mut self,
    ) -> Result<g17_completion::PairedRenderCompletion> {
        const POLLS: usize = 2000;
        for _ in 0..POLLS {
            fence(Ordering::Acquire);
            let state = self.render_storage.as_mut().ok_or(ENODEV)?.snapshot()?;
            if !state.job_list_empty
                && state.tiling.idle()
                && state.fragment.idle()
                && state.tiling_timestamps.iter().all(|value| *value != 0)
                && state.fragment_timestamps.iter().all(|value| *value != 0)
            {
                self.render_storage
                    .as_mut()
                    .map(|s| s.read_pool_a_record(&mut pa).is_ok())
                    .unwrap_or(false)
                {
                    // The TA descriptor's +0x10 selects WHICH Pool-A record the
                    // firmware gates on, and it is record 1 by default -- read
                    // both. `FUN_16f7c` sets +0x94 = 1 after linking the job, so
                    // a 1 proves the classic TA enqueue ran and the job is
                    // sitting in the list; a 0 proves it never ran at all.
                    let r0 = u32::from_le_bytes([pa[0x94], pa[0x95], pa[0x96], pa[0x97]]);
                    let r1 = u32::from_le_bytes([
                        pa[0x194], pa[0x195], pa[0x196], pa[0x197],
                    ]);
                    let v1 = if r1 != 0 {
                        "rec1 QUEUED (FUN_16f7c ran, job linked, nothing dequeues DM0)"
                    } else {
                        "rec1 NOT queued (FUN_16f7c never ran)"
                    };
                    dev_info!(
                        dev.as_ref(),
                        "G17P pool-a[{}] rec0(+0x00={:02x?} +0x10={:02x?} +0x94={:#010x}) rec1(+0x00={:02x?} +0x10={:02x?} +0x94={:#010x} link={:02x?}) {}\n",
                        label,
                        &pa[0x00..0x08],
                        &pa[0x10..0x14],
                        r0,
                        &pa[0x100..0x108],
                        &pa[0x110..0x114],
                        r1,
                        &pa[0x198..0x1a8],
                        v1,
                    );
                }
            }
            let mut cc = [0u8; 0x40];
            let ok = self
                .render_storage
                .as_mut()
                .map(|s| s.read_channel_control(&mut cc).is_ok())
                .unwrap_or(false);
            if ok {
                dev_info!(
                    dev.as_ref(),
                    "G17P render channel-control[{}]: pid={:#04x} idx={:#04x} f2={:#04x} prio={:#04x} class={:#04x} qidmask={:02x?} hwbuf={:#04x}\n",
                    label,
                    cc[0],
                    cc[1],
                    cc[2],
                    cc[3],
                    cc[4],
                    &cc[0x12..0x1a],
                    cc[0x33],
                );
            }
        }

        self.resources.dump_render_qos_regions(label);
        if *crate::module_parameters::g17p_dump_state.value() != 0
            && !G17P_STATE_DUMPED.swap(true, core::sync::atomic::Ordering::Relaxed)
        {
            let kib = *crate::module_parameters::g17p_dump_state.value() as usize;
            if kib == 1 {
                self.resources.checksum_primary_state(label);
            } else {
                self.resources.dump_primary_state(label, kib * 1024);
            }
        }
        if *crate::module_parameters::g17p_dump_hwdata.value() != 0
            && !G17P_HW_DATA_DUMPED.swap(true, core::sync::atomic::Ordering::Relaxed)
        {
            self.resources.dump_hw_data(label);
        }
        self.resources.log_render_parameter_buffer_descriptor(
            label,
            self.render_hardware_buffer_id.unwrap_or(0),
        );
        let selected_metric = self
            .render_storage
            .as_ref()
            .ok_or(ENODEV)
            .and_then(|storage| storage.selected_scene_index())
            .and_then(|scene| self.resources.selected_parameter_metric(scene));
        let dev = self.resources.dev().clone();
        match selected_metric {
            Ok(value) => dev_info!(
                dev.as_ref(),
                "G17P render PM[{}]: actual-selected-metric={:#x}\n",
                label,
                value,
            ),
            Err(error) => dev_warn!(
                dev.as_ref(),
                "G17P render PM[{}]: actual selected metric read failed ({:?})\n",
                label,
                error,
            ),
        }
        if let Some(storage) = self.render_storage.as_mut() {
            match storage.render_activation_coordinates() {
                Ok((ta_qid, ta_stamp, sku_reserved, sku_pointer, sku_header)) => dev_info!(
                    dev.as_ref(),
                    "G17P render activation[{}]: TA-link qid={} stamp={:#x} predecessor={:#x}; 3D-SKU reserved={:#x} stream={:#x} header={:#x}\n",
                    label,
                    ta_qid,
                    ta_stamp,
                    ta_stamp.wrapping_sub(1) & ((1u64 << 40) - 1),
                    sku_reserved,
                    sku_pointer,
                    sku_header,
                ),
                Err(error) => dev_warn!(
                    dev.as_ref(),
                    "G17P render activation[{}]: descriptor coordinates unreadable ({:?})\n",
                    label,
                    error,
                ),
            }
            match storage.render_activation_gate_words() {
                Ok(gate) => dev_info!(
                    dev.as_ref(),
                    "G17P render activation gate[{}]: TA(+7a4,+866,+892,+8ba,+8cb)={:x?}\n",
                    label,
                    gate,
                ),
                Err(error) => dev_warn!(
                    dev.as_ref(),
                    "G17P render activation gate[{}]: descriptor inputs unreadable ({:?})\n",
                    label,
                    error,
                ),
            }
            storage.log_status_pages(&dev, label);
            storage.log_parameter_management(&dev, label);
        }
    }

    /// Snapshot every host-DRAM render progress marker while firmware is
    /// halted at recovery state 1, before the host acknowledges the restart.
    /// The recovery path retires both queue graphs and writes a synthetic 3D
    /// completion record, so the ordinary timeout dump is too late to tell a
    /// launch failure from recovery cleanup.
    /// Locate a reported MMU fault address inside the render VM's own tables.
    ///
    /// The G14X fault-address register is documented as VA >> 6, but that
    /// reading and the raw one cannot be told apart from a fault inside the
    /// first page. The GMMU report's VM slot is the descriptor/application
    /// context, not the ordinary VM bind slot. Resolve the retained storage by
    /// that exact context before walking its owner. Then report whether the
    /// selected VM maps the page and every mapped run within +/-1 MiB of it. A
    /// candidate that abuts or falls just outside a live run is the real
    /// address; one that lands in empty space at every scale is not.
    fn render_bind_for_descriptor_context(
        &self,
        descriptor_context_id: u8,
    ) -> Option<(&G17PRenderRootOwner, &'static str)> {
        let matches = |storage: &g17_resources::G17PUserRenderStorage| {
            storage.descriptor_context_id() == u16::from(descriptor_context_id)
        };
        if self.render_storage.as_ref().is_some_and(matches) {
            if let Some(bind) = self.render_bind.as_ref() {
                return Some((bind, "active"));
            }
        }
        if let Some(retirement) = self.render_context_retirement.as_ref().filter(|retirement| {
            retirement.bind.root_slot().ok() == Some(descriptor_context_id)
        }) {
            return Some((&retirement.bind, "active-context-retirement"));
        }
        if let Some(graph) = self
            .retired_render_handoff_graph
            .as_ref()
            .filter(|graph| matches(&graph.storage))
        {
            return Some((&graph.bind, "active-quarantine"));
        }
        if let Some(graph) = self
            .retired_render_handoff_graphs
            .iter()
            .find(|graph| matches(&graph.storage))
        {
            return Some((&graph.bind, "completion-quarantine"));
        }
        for state in self.parked_render_slots.iter().flatten() {
            if state.storage.as_ref().is_some_and(matches) {
                if let Some(bind) = state.bind.as_ref() {
                    return Some((bind, "parked"));
                }
            }
            if let Some(graph) = state
                .retired_handoff_graph
                .as_ref()
                .filter(|graph| matches(&graph.storage))
            {
                return Some((&graph.bind, "parked-quarantine"));
            }
            if let Some(retirement) = state.context_retirement.as_ref().filter(|retirement| {
                retirement.bind.root_slot().ok() == Some(descriptor_context_id)
            }) {
                return Some((&retirement.bind, "parked-context-retirement"));
            }
        }
        None
    }

    pub(crate) fn log_fault_address_candidates(
        &self,
        label: &'static str,
        addr_word: u64,
        fault_vm_slot: u8,
    ) -> bool {
        let Some((bind, owner)) = self.render_bind_for_descriptor_context(fault_vm_slot) else {
            return false;
        };
        self.log_fault_address_candidates_for_vm(
            label,
            addr_word,
            fault_vm_slot,
            bind.vm(),
            bind.vm_id(),
            bind.root(),
            owner,
        );
        true
    }

    pub(crate) fn log_compute_fault_address_candidates(
        &self,
        label: &'static str,
        addr_word: u64,
        fault_vm_slot: u8,
        context: &mmu::T8140ComputeExecutionContext,
        owner: &'static str,
    ) {
        self.log_fault_address_candidates_for_vm(
            label,
            addr_word,
            fault_vm_slot,
            context.vm(),
            context.vm_id(),
            context.root(),
            owner,
        );
    }

    fn log_fault_address_candidates_for_vm(
        &self,
        label: &'static str,
        addr_word: u64,
        fault_vm_slot: u8,
        vm: &mmu::Vm,
        vm_id: u64,
        root: u64,
        owner: &'static str,
    ) {
        let dev = self.resources.dev().clone();
        dev_info!(
            dev.as_ref(),
            "G17P fault address[{}]: walking {} owner vm={} root={:#x} vm-slot={}\n",
            label,
            owner,
            vm_id,
            root,
            fault_vm_slot,
        );
        let candidates = [
            ("raw", addr_word),
            ("shift6", (addr_word & ((1u64 << 42) - 1)) << 6),
            ("shift6-masked", (addr_word << 6) & ((1u64 << 42) - 1)),
        ];
        for (name, candidate) in candidates {
            let page = candidate & !(mmu::UAT_PGSZ as u64 - 1);
            let mapped = vm.covers_range(page, mmu::UAT_PGSZ as u64, true, false);
            let translation = vm.translate_iova(candidate);
            dev_info!(
                dev.as_ref(),
                "G17P fault address[{}]: {} candidate {:#x} page {:#x} mapped={} translate={:?}\n",
                label,
                name,
                candidate,
                page,
                mapped,
                translation,
            );
            let window_start = page.saturating_sub(0x10_0000);
            let window_end = match page.checked_add(0x10_0000) {
                Some(end) => end,
                None => continue,
            };
            let mut ranges = [pgtable::MappedRange::default(); 16];
            match vm.mapped_ranges(window_start..window_end, &mut ranges) {
                Ok((count, truncated)) => {
                    dev_info!(
                        dev.as_ref(),
                        "G17P fault address[{}]: {} neighbourhood {:#x}..{:#x} has {} run(s){}\n",
                        label,
                        name,
                        window_start,
                        window_end,
                        count,
                        if truncated { " TRUNCATED" } else { "" },
                    );
                    for run in ranges.iter().take(count) {
                        dev_info!(
                            dev.as_ref(),
                            "G17P fault address[{}]:   {:#x}..{:#x} pte={:#x}\n",
                            label,
                            run.start,
                            run.end,
                            run.pte,
                        );
                    }
                }
                Err(error) => dev_info!(
                    dev.as_ref(),
                    "G17P fault address[{}]: {} neighbourhood walk failed ({:?})\n",
                    label,
                    name,
                    error,
                ),
            }
        }

        // One fault-only discriminator for Honeykrisp's sparse buffer layout:
        // a mapped lower peer with an unmapped upper address means the shadow
        // bind disappeared, while two unmapped peers point to a stale or
        // malformed address. Keep this to one line; it is not submit logging.
        let decoded = (addr_word << 6) & ((1u64 << 42) - 1);
        if let Some(candidate) = g17p_fault_sparse_ro_lower(decoded) {
            let page = candidate & !(mmu::UAT_PGSZ as u64 - 1);
            let mapped = vm.covers_range(page, mmu::UAT_PGSZ as u64, true, false);
            let translation = vm.translate_iova(candidate);
            dev_info!(
                dev.as_ref(),
                "G17P fault address[{}]: Honeykrisp sparse-RO lower peer {:#x} page {:#x} mapped={} translate={:?}\n",
                label,
                candidate,
                page,
                mapped,
                translation,
            );
        }

        // The whole client root, so any decode of the address register can be
        // tested against the real layout instead of one window at a time.
        let mut all = [pgtable::MappedRange::default(); 96];
        // The full 42-bit low half, from the client null view up to the
        // unknown page, so no decode of the address register is excluded by
        // the window rather than by the tables.
        let user_range = 0u64..(1u64 << 42);
        match vm.mapped_ranges(user_range.clone(), &mut all) {
            Ok((count, truncated)) => {
                dev_info!(
                    dev.as_ref(),
                    "G17P fault address[{}]: client root {:#x}..{:#x} holds {} run(s){}\n",
                    label,
                    user_range.start,
                    user_range.end,
                    count,
                    if truncated { " TRUNCATED" } else { "" },
                );
                for run in all.iter().take(count) {
                    dev_info!(
                        dev.as_ref(),
                        "G17P fault address[{}]: run {:#x}..{:#x} ({:#x}) pte={:#x}\n",
                        label,
                        run.start,
                        run.end,
                        run.end - run.start,
                        run.pte,
                    );
                }
            }
            Err(error) => dev_info!(
                dev.as_ref(),
                "G17P fault address[{}]: client root walk failed ({:?})\n",
                label,
                error,
            ),
        }
    }

    pub(crate) fn log_render_recovery_pre_ack(&mut self, label: &'static str) {
        let render_qids = self.render_sksm.as_ref().map(|queues| {
            [
                queues.tiling.queue_id as usize,
                queues.fragment.queue_id as usize,
            ]
        });
        if let Some([tiling_qid, fragment_qid]) = render_qids {
            let tiling_last = self.g17p_last_submitted_hw_timestamp(tiling_qid);
            let fragment_last = self.g17p_last_submitted_hw_timestamp(fragment_qid);
            dev_info!(
                self.resources.dev().as_ref(),
                "G17P render recovery pre-ack[{}]: last-submitted TA(qid {})={:?} 3D(qid {})={:?}\n",
                label,
                tiling_qid,
                tiling_last,
                fragment_qid,
                fragment_last,
            );
        }
        {
            let dev = self.resources.dev().clone();
            if let Some(storage) = self.render_storage.as_mut() {
                match storage.snapshot() {
                    Ok(state) => dev_info!(
                        dev.as_ref(),
                        "G17P render recovery pre-ack[{}]: tiling done/read/write {}/{}/{} fragment {}/{}/{} job-list-empty {} tiling-ts [{:#x},{:#x}] fragment-ts [{:#x},{:#x}]\n",
                        label,
                        state.tiling.done,
                        state.tiling.read,
                        state.tiling.write,
                        state.fragment.done,
                        state.fragment.read,
                        state.fragment.write,
                        state.job_list_empty,
                        state.tiling_timestamps[0],
                        state.tiling_timestamps[1],
                        state.fragment_timestamps[0],
                        state.fragment_timestamps[1],
                    ),
                    Err(error) => dev_warn!(
                        dev.as_ref(),
                        "G17P render recovery pre-ack[{}]: queue snapshot failed ({:?})\n",
                        label,
                        error
                    ),
                }
            }
        }
        self.log_render_status_pages(label);
        self.log_render_sksm_entries(label);
        if g17p_native_fragment_first_mode(
            *crate::module_parameters::g17p_render_native_doorbells.value(),
        ) {
            self.log_render_outer_channels(label);
        }
        if let Err(error) = self.dump_completion_records(label) {
            let dev = self.resources.dev().clone();
            dev_warn!(
                dev.as_ref(),
                "G17P render recovery pre-ack[{}]: completion dump failed ({:?})\n",
                label,
                error
            );
        }
    }

    /// DRAM-only TA/3D outer-channel state. The current slot record pointer and
    /// queue-id byte prove whether firmware consumed the Fragment slot after
    /// its dedicated doorbell without touching the SGX MMIO aperture.
    pub(crate) fn log_render_outer_channels(&mut self, label: &'static str) {
        let snapshots = match self.resources.snapshot_all_primary_work_channels() {
            Ok(snapshots) => snapshots,
            Err(error) => {
                dev_warn!(
                    self.resources.dev().as_ref(),
                    "G17P render outer[{}]: snapshot failed ({:?})\n",
                    label,
                    error
                );
                return;
            }
        };
        // Log EVERY work channel, not the first two. The userspace render's
        // tables are TA_2/3D_2 (indices 6/7) and compute publishes on CL_2 (8).
        // Native's distinct system bring-up queues occupy TA_0/3D_0.
        for snapshot in snapshots.into_iter() {
            let name = if snapshot.table_index
                == g17_submission::G17P_PARTIAL_OPENING_TILING_CHANNEL_TABLE_INDEX
            {
                "TA"
            } else if snapshot.table_index
                == g17_submission::G17P_PARTIAL_OPENING_FRAGMENT_CHANNEL_TABLE_INDEX
            {
                "3D"
            } else if snapshot.table_index
                == g17_submission::G17P_COMPUTE_CHANNEL_TABLE_INDEX
            {
                "CL"
            } else {
                "--"
            };
            if snapshot.cursors == [0, 0, 0] && snapshot.current_slot_queue == 0 {
                continue;
            }
            let tracker = self.render_tracker.as_mut().ok_or(ENODEV)?;
            let completion = g17_completion::observe_paired_render_completion(
                tracker.tiling_previous,
                state.tiling,
                tracker.tiling_prefix,
                tracker.fragment_previous,
                state.fragment,
                tracker.fragment_prefix,
                g17_completion::RenderExecutionState {
                    job_list_empty: state.job_list_empty,
                    tiling_timestamps: state.tiling_timestamps,
                    fragment_timestamps: state.fragment_timestamps,
                }
            };
            let observe = |state: &g17_resources::G17PUserRenderQueueState| {
                g17_completion::observe_paired_render_window(
                    window,
                    tiling_previous,
                    state.tiling,
                    fragment_previous,
                    state.fragment,
                    execution(state),
                )
            };
            let completion = match observe(&state) {
                Ok(completion) => completion,
                Err(first_error) => {
                    // The six producer/consumer words are firmware-owned and
                    // sampled separately.  An event edge can therefore catch
                    // one cache line before the paired line becomes visible.
                    // Retry one immediate DRAM snapshot; this is bounded edge
                    // handling, not a completion poll or a steady-state timer.
                    fence(Ordering::Acquire);
                    let retry = self
                        .render_storage
                        .as_mut()
                        .ok_or(ENODEV)?
                        .snapshot()?;
                    match observe(&retry) {
                        Ok(completion) => {
                            state = retry;
                            completion
                        }
                        Err(second_error) => {
                            dev_err!(
                                self.resources.dev().as_ref(),
                                "G17P render completion: ordinal {} target {} rejected two snapshots ({:?}/{:?}); previous TA {}/{}/{} 3D {}/{}/{} first TA {}/{}/{} 3D {}/{}/{} retry TA {}/{}/{} 3D {}/{}/{}\n",
                                self.render_submission_ordinal,
                                window.target(),
                                first_error,
                                second_error,
                                tiling_previous.done,
                                tiling_previous.read,
                                tiling_previous.write,
                                fragment_previous.done,
                                fragment_previous.read,
                                fragment_previous.write,
                                state.tiling.done,
                                state.tiling.read,
                                state.tiling.write,
                                state.fragment.done,
                                state.fragment.read,
                                state.fragment.write,
                                retry.tiling.done,
                                retry.tiling.read,
                                retry.tiling.write,
                                retry.fragment.done,
                                retry.fragment.read,
                                retry.fragment.write,
                            );
                            return Ok(G17PRenderPollOutcome::Retry);
                        }
                    }
                }
            };
            let tracker = self.render_tracker.as_mut().ok_or(ENODEV)?;
            if completion.complete {
                // Queue retirement can become visible before the TA command's
                // EventControl has reached its own submitted target and been
                // unlinked. Releasing the fence at that intermediate state
                // lets the next command consume another Pool-A record until
                // all 35 appear busy. The matching type-1 edge is the wake;
                // this is its single ordered host-DRAM validation.
                let event_control = self
                    .render_storage
                    .as_mut()
                    .ok_or(ENODEV)?
                    .render_event_control_state()?;
                if !event_control.unlinked() {
                    if crate::util::g17p_verbose() {
                        dev_info!(
                            self.resources.dev().as_ref(),
                            "G17P render completion: EventControl {} pending submitted={} completed={} membership={:#x}\n",
                            event_control.record_index,
                            event_control.submitted,
                            event_control.completed,
                            event_control.membership,
                        );
                    }
                    return Ok(G17PRenderPollOutcome::Pending);
                }
                // B1 clears EventControl membership BEFORE GPU cache
                // maintenance. Its fragment stamp A write follows that
                // maintenance (25G83 B1 0x16ea0); queue/list retirement alone
                // therefore cannot authorize a userspace-visible fence.
                // The physical slot retains its ordinal across VM switches,
                // has one in-flight pair, and refuses 24-bit stamp overflow.
                // Require this exact generation, never a stale nonzero stamp.
                let pair = self.render_storage.as_ref().ok_or(ENODEV)?.queue_pair();
                let expected_stamp = self.render_submission_ordinal
                    .checked_add(1)
                    .and_then(|value| value.checked_mul(0x100))
                    .ok_or(EOVERFLOW)?;
                let fragment_stamp = self.resources.render_fragment_completion_stamp(pair)?;
                if fragment_stamp != expected_stamp {
                    if crate::util::g17p_verbose() {
                        dev_info!(
                            self.resources.dev().as_ref(),
                            "G17P render completion: fragment stamp pending qid={} observed={:#x} expected={:#x}\n",
                            pair.fragment,
                            fragment_stamp,
                            expected_stamp,
                        );
                    }
                    // The post-stamp type-1 notification wakes this job.
                    // Keep its ownership and fence pending without polling.
                    return Ok(G17PRenderPollOutcome::Pending);
                }
                // Publish the exact GPU-written pass envelope while the completed job's
                // VM binding, ordinal and QIDs are still owned.  This is
                // observation only: it neither gates completion nor changes
                // queue state.  Invalid retirement-only pairs are omitted.
                if let Some(interval) =
                    g17_completion::render_gpu_interval(
                        g17_completion::RenderExecutionState {
                            job_list_empty: state.job_list_empty,
                            tiling_timestamps: state.tiling_timestamps,
                            fragment_timestamps: state.fragment_timestamps,
                        },
                    )
                {
                    let (vm_id, root) = self.render_bind.as_ref()
                        .map_or((0, 0), |binding| (binding.vm_id(), binding.root()));
                    let fallback_pair = self.render_storage
                        .as_ref()
                        .map(g17_resources::G17PUserRenderStorage::queue_pair)
                        .unwrap_or(g17_resources::G17PRenderQueuePair::for_slot(
                            self.active_render_slot,
                        ).ok_or(EINVAL)?);
                    let (tiling_qid, fragment_qid) = self.render_sksm.as_ref()
                        .map_or((fallback_pair.tiling, fallback_pair.fragment), |queues| (
                            u16::from(queues.tiling.queue_id),
                            u16::from(queues.fragment.queue_id),
                        ));
                    if crate::util::g17p_verbose() {
                        dev_info!(
                            self.resources.dev().as_ref(),
                            "G17P render: GPU-INTERVAL ordinal={} vm={} root={:#x} ta-qid={} fragment-qid={} start={} end={}\n",
                            self.render_submission_ordinal,
                            vm_id,
                            root,
                            tiling_qid,
                            fragment_qid,
                            interval[0],
                            interval[1],
                        );
                    }
                }
                // Validate and commit the context-retirement epoch before the
                // ordinary occupancy transaction starts mutating allocator
                // state. Every fallible retirement precondition is therefore
                // checked before an HWPB lease or queue producer is changed.
                self.complete_render_context_update_retirement()?;
                // Native TA/3D complete subtract completed kicks from queue
                // +44. Prepare both results before mutating either queue;
                // timestamps and previous-stamp history must never reset.
                let retired_sksm = self.render_sksm.as_ref().map(|queues| {
                    let ta = g17_submission::complete_g17p_cl_kicks(queues.tiling.producer, 1)?;
                    let fragment = g17_submission::complete_g17p_cl_kicks(queues.fragment.producer, 1)?;
                    Ok::<_, g17_submission::SubmissionError>((ta, fragment))
                }).transpose().map_err(|_| EIO)?;
                let next_ordinal = self.render_submission_ordinal.checked_add(1).ok_or(EOVERFLOW)?;
                let hardware_buffer_id = self.render_hardware_buffer_id.ok_or(EIO)?;
                self.render_hardware_buffer_ids
                    .release(hardware_buffer_id)
                    .map_err(|_| EIO)?;
                self.release_active_render_command_buffer_states()?;
                if let (Some(queues), Some((ta, fragment))) = (self.render_sksm.as_mut(), retired_sksm) {
                    queues.tiling.producer = ta;
                    queues.fragment.producer = fragment;
                }
                self.render_hardware_buffer_id = None;
                self.render_tracker = None;
                self.render_hardware_buffer_published = false;
                self.render_auxiliary_timestamp = 0;
                if crate::util::g17p_verbose() {
                    dev_info!(
                        self.resources.dev().as_ref(),
                        "G17P render: released TA HardwareBufferID {} at paired completion\n",
                        hardware_buffer_id,
                    );
                }
                self.render_submission_ordinal = next_ordinal;
                // One control sample per boot, from a render that DID complete.
                //
                // Every field of the `context/qos` line captured at a wedge --
                // slot=255 ctx=255 prio=0 class=0 state=2 bound-slot=255 and an
                // all-zero qid bitmap -- is byte-for-byte what
                // `apply_g17p_cold_opening_channel_control` writes at cold
                // opening (`0x0000_0100_0000_ffff` at +0x00, `0x0002_..` at
                // +0x20, `0x0000_0000_ff00_0000` at +0x30, `fill(0)` elsewhere).
                // So that line proves only that the record is UNTOUCHED, which
                // is a different claim from "the context was lost", and without
                // a healthy sample the two are indistinguishable. If this
                // control prints the same constants, the firmware never writes
                // this record and the diagnostic -- and the comment above it
                // claiming it is "the exact host-safe witness" -- is wrong.
                if !RENDER_CONTEXT_WITNESS_LOGGED.swap(true, Ordering::Relaxed) {
                    self.log_render_context_witness("healthy-paired-completion");
                }
                return Ok(G17PRenderPollOutcome::Complete(completion));
            }
            tracker.tiling_previous = state.tiling;
            tracker.fragment_previous = state.fragment;
            if completion.complete {
                self.render_tracker = None;
                self.render_submission_ordinal = self
                    .render_submission_ordinal
                    .checked_add(1)
                    .ok_or(EOVERFLOW)?;
                return Ok(completion);
            }
            fsleep(Delta::from_millis(1));
        }
        // The graph remains pinned because firmware may still own it. Runtime
        // teardown stops both provider CPUs before manager resources drop.
        Err(ETIMEDOUT)
    }

    pub(crate) fn sksm_scratch(&self) -> G17SksmScratchGeometry {
        self.platform.sksm_scratch
    }

    /// Return the hardware QID of a queue owned by this manager.
    pub(crate) fn compute_queue_id(&self, queue: &G17PSksmQueue) -> Result<u8> {
        if queue.scratch != self.sksm_scratch() || queue.uat_owner != self.uat().ttb_base() {
            return Err(EINVAL);
        }
        Ok(queue.queue_id)
    }

    pub(crate) fn bind_compute_user_vm(
        &mut self,
        queue: &mut G17PSksmQueue,
        vm: &mmu::Vm,
    ) -> Result<G17PComputeUserBinding> {
        if !queue.registered
            || queue.scratch != self.sksm_scratch()
            || queue.uat_owner != self.uat().ttb_base()
        {
            return Err(EINVAL);
        }
        let mappings = queue.compute.map_user_execution_state(vm)?;
        let vm_bind = self.resources.uat().bind(vm)?;
        if vm_bind.slot() != 1 || !mappings.all_reachable_from(vm) {
            return Err(EFAULT);
        }
        Ok(G17PComputeUserBinding {
            mappings,
            _vm_bind: vm_bind,
        })
    }

    /// Configure the hardware-proven G17P compute queue through resource 0 and
    /// return its retained state only after the QID-valid bit is published.
    pub(crate) fn configure_g17p_compute_queue(
        &mut self,
        dev: &AsahiDevice,
        registers: &regs::Resources,
    ) -> Result<G17PSksmQueue> {
        const QUEUE_ID: u8 = 4;
        const COMPLETION_SELECTOR: u8 = 2;

        let mut storage = g17_resources::G17PClRuntimeStorage::new(dev, self.uat())?;
        let compute = g17_resources::G17PComputeDescriptorStorage::new(
            dev,
            self.uat(),
            storage.channel_control_gpu_va(),
        )?;
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
                    g17_initdata::COMPUTE_FLIST_PAGE_LIST_ADDRESS,
                    operand_table_gpu_va,
                    support_state_gpu_va,
                    scheduler_state_gpu_va,
                )
                .map_err(|_| EINVAL)
            },
        )?;
        let routing = g17_submission::G17PComputeQueueRouting {
            queue_id: QUEUE_ID,
            completion_selector: COMPLETION_SELECTOR as u32,
            entry_gpu_va: storage.entry_gpu_va(),
        };
        let config =
            g17_submission::prepare_g17p_compute_sksm_queue_config(routing, self.sksm_scratch())
                .map_err(|_| EINVAL)?;

        // Every firmware-visible page and its DVA must be globally published
        // before the config payload makes the queue reachable from SKSM. The
        // MMIO helper then stores config word 0, config word 1, and finally the
        // two QID-valid mask words under one fixed-slot FIFO lock.
        fence(Ordering::SeqCst);
        registers.g17p_sksm_configure_queue(config.write_pair(), config.enable_pair)?;

        Ok(G17PSksmQueue {
            queue_id: routing.queue_id,
            scratch: self.sksm_scratch(),
            uat_owner: self.uat().ttb_base(),
            disable_pair: config.disable_pair,
            registered: true,
            storage,
            compute,
            producer: g17_submission::G17PClKickProducerState {
                queue_id: QUEUE_ID,
                num_stamps: 256,
                entry_stride: 0x200,
                completion_selector: COMPLETION_SELECTOR,
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
            submission_ordinal: 0,
            b2_activation: g17_submission::G17PClB2ActivationState::Pending,
        })
    }

    /// Remove a retained queue from the hardware bridge before releasing its
    /// backing storage. Repeated teardown is harmless.
    pub(crate) fn unregister_compute_sksm_queue(
        &self,
        registers: &regs::Resources,
        queue: &mut G17PSksmQueue,
    ) -> Result {
        if queue.scratch != self.sksm_scratch() || queue.uat_owner != self.uat().ttb_base() {
            return Err(EINVAL);
        }
        if !queue.registered {
            return Ok(());
        }

        registers.g17p_sksm_publish_write_pair(queue.disable_pair)?;
        queue.registered = false;
        Ok(())
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
        if !queue.registered
            || queue.scratch != self.sksm_scratch()
            || queue.uat_owner != self.uat().ttb_base()
        {
            return Err(EINVAL);
        }
        let prepared = g17_submission::prepare_g17p_cl_kick_entry(queue.producer, operands)
            .map_err(|_| EINVAL)?;
        self.publish_compute_entry(registers, queue, prepared)
    }

    fn publish_compute_entry(
        &mut self,
        registers: &regs::Resources,
        queue: &mut G17PSksmQueue,
        prepared: g17_submission::PreparedG17PClKickEntry,
    ) -> Result<u64> {
        if !self.runtime_tick_staged {
            // Stage the runtime 0x2e tick exactly the way the output-positive
            // paths do: written and producer-published with no doorbell,
            // pending for this work kick to consume. The parked co-processor
            // never answers a bare 0x84 announcement for it.
            let target = self.publish_compute_runtime_control_record(0)?;
            self.runtime_tick_staged = true;
            dev_info!(
                self.resources.dev().as_ref(),
                "G17P compute control: runtime tick 0 staged for this kick (target {})\n",
                target
            );
        }
        queue.storage.write_entry(
            prepared.entry_offset as usize,
            prepared.zero_length as usize,
            &prepared.entry,
        )?;

        self.publish_compute_b2_once(queue)?;

        fence(Ordering::SeqCst);
        queue.producer = prepared.producer_after;
        let timestamp = prepared.current_timestamp;
        self.add_sksm_kicks(registers, queue, 1)?;
        fence(Ordering::SeqCst);
        Ok(timestamp)
    }

    fn publish_compute_b2_once(&mut self, queue: &mut G17PSksmQueue) -> Result {
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
        Ok(())
    }

    fn submit_translated_add3_channel(
        &mut self,
        queue: &mut G17PSksmQueue,
        binding: &G17PComputeUserBinding,
        command: &g17_uapi::TranslatedComputeCommand,
        context: G17PComputeSubmissionContext<'_>,
        buffers: [u64; 3],
    ) -> Result<u64> {
        if queue.submission_ordinal != 0 {
            return Err(EINVAL);
        }

        let descriptor_slot = 0;
        let (descriptor_high, _) = queue.compute.descriptor_gpu_vas(descriptor_slot);
        let descriptor_low = binding.mappings.descriptor_gpu_va(descriptor_slot);
        queue.compute.write_g17p_add3_resource_table(buffers)?;

        let user_layout = binding.mappings.layout();
        let shared_control = queue.storage.shared_support_gpu_va();
        let objects = g17_compute::ComputeDescriptorObjects {
            scheduler_record: queue.compute.scheduler_record_gpu_va(),
            descriptor_low_alias: descriptor_low,
            dispatch_a: queue.compute.dispatch_a_gpu_va(),
            dispatch_b: queue.compute.dispatch_b_gpu_va(),
            status_a: queue.compute.status_a_gpu_va(),
            status_b: queue.compute.status_b_gpu_va(),
            shared_control,
            zero_page: queue.compute.zero_page_gpu_va(),
        };
        queue.compute.with_descriptor_mut(descriptor_slot, |raw| {
            g17_uapi::build_compute_descriptor(
                command,
                g17_uapi::ComputeInternalState {
                    preempt_base: user_layout.preempt,
                    dispatch_identity: context.dispatch_identity,
                    context_id: context.context_id,
                    work_ordinal: 0,
                    robustness: user_layout.robustness,
                    operand_state_base: user_layout.operand_state,
                    execution_gate: context.execution_gate,
                },
                objects,
                g17_compute::ComputeDescriptorMetadata {
                    submit_sequence: 0,
                    context_id: context.context_id,
                    grid_index: g17_submission::G17P_COMPUTE_CHANNEL_GRID_INDEX as u32,
                    work_ordinal: 0,
                    queue_submission: 1,
                    queue_ordinal: 0,
                    submission_index: 1,
                    support_control: 0xe0a0_0001,
                    support_flags: 0,
                },
                context.timestamps,
                raw,
            )
            .map_err(|_| EINVAL)
        })?;

        let command_state = g17_submission::prepare_g17p_compute_cl_command_state(
            descriptor_low,
            queue.submission_ordinal,
        )
        .map_err(|_| EINVAL)?;
        let prepared_entry = g17_submission::prepare_g17p_cl_kick_entry(
            queue.producer,
            g17_submission::G17PClKickEntryOperands {
                descriptor_flag_4c: false,
                descriptor_flag_5c8: false,
                converted_command_timestamp: 1,
                barriers: &[],
                mcache: None,
                payload: [descriptor_high, queue.compute.queue_record_gpu_va()],
                event_mask: command_state.event_mask,
                rce_kind: 0,
                rce_bindings: command_state.rce_bindings,
                auxiliary: context.auxiliary,
            },
        )
        .map_err(|_| EINVAL)?;
        if prepared_entry.entry_index != 1
            || prepared_entry.entry_offset != queue.producer.geometry.entry_stride()
            || prepared_entry.current_timestamp != queue.producer.geometry.timestamp_seed()
        {
            return Err(EINVAL);
        }
        let backend = g17_submission::T8140_G17P_CL_BACKEND;
        if !g17_submission::g17p_first_bind_tag15_required(true)
            || !g17_submission::g17p_first_bind_tag16_required(backend, true)
        {
            return Err(EINVAL);
        }
        let mut entry_signal =
            [0u8; g17_submission::G17P_COMPUTE_OPTIONAL_EVENT_SIZE];
        // Tag 16 names the last retired entry. A newly installed queue has no
        // predecessor, so the protocol value is zero.
        let first_bind_old_timestamp = prepared_entry.current_timestamp.saturating_sub(1);
        g17_submission::encode_g17p_compute_optional_event(
            g17_submission::G17PComputeOptionalEvent {
                queue_id: u16::from(queue.queue_id),
                old_timestamp: first_bind_old_timestamp,
            },
            &mut entry_signal,
        )
        .map_err(|_| EINVAL)?;
        if let Ok(window) = queue.compute.g17p_preempt_argument_window() {
            // +0x1480 is where 0x1a4d0..0x1a4e8 point; +0x14a0 (this dump's
            // +0x20) is the add3 operand table. Populated for the proof path,
            // untouched for a Honeykrisp dispatch.
            self.log_record("preempt+0x1480", &window);
        }
        self.log_record("cl-kick-entry", &prepared_entry.entry);
        self.log_record("tag16-entry-signal", &entry_signal);

        let sksm_entry_aliases = queue.storage.entry_aliases()?;
        // Tag 15 installs a queue exactly once. This path is reachable only for
        // the prepared first bind; retained submissions use the repeat encoder
        // with the install flag clear.
        let install_queue = matches!(
            queue.hardware_state,
            G17PSksmQueueHardwareState::PreparedMemory
        );
        // The single most diagnostic line for a client handoff. The tag-15
        // queue-install flag must be published exactly once for each fresh QID.
        // Reusing an installed QID with another context would ask firmware to
        // reprogram a live KSMFE queue and strand one of the two routes.
        dev_info!(
            self.resources.dev().as_ref(),
            "G17PMARK tag15-install i={} z={} r={} h={} qid={} flag={} (first-bind chronology; a second install on the SAME qid in one boot is the second-client bug)\n",
            {
                let n = G17P_MARK_TAG15_INSTALLS.fetch_add(1, Ordering::Relaxed) + 1;
                n
            },
            G17P_MARK_ZERO_DURATION.load(Ordering::Relaxed),
            G17P_MARK_GATE_REBUILDS.load(Ordering::Relaxed),
            G17P_MARK_RETAINED_HANDOFFS.load(Ordering::Relaxed),
            queue.queue_id,
            install_queue as u32,
        );
        let mut group = queue
            .compute
            .stage_initial_channel_group_without_fallback_event(
                descriptor_high,
                sksm_entry_aliases,
                shared_control,
                queue.storage.channel_control_gpu_va(),
                queue.queue_id,
                install_queue,
                binding.declared_context_id(),
            )?;
        // Purely precompute the host-only sequence token and notification
        // predicates. Retained state is not committed until after the SKSM
        // producer transition, matching the native correlation chronology.
        let lifecycle = g17_submission::prepare_g17p_direct_cl_lifecycle(
            queue.direct_lifecycle,
            group.queue,
            true,
            backend,
        )
        .map_err(|_| EINVAL)?;
        if !lifecycle.publish_config_update
            || !lifecycle.publish_entry_signal
            || lifecycle.correlation_token == 0
        {
            return Err(EINVAL);
        }
        if *module_parameters::g17p_fault_report.value() != 0 {
            dev_info!(
                self.resources.dev().as_ref(),
                "G17P rec tag15: install_queue={} descriptor_high={:#x} shared_control={:#x} channel_control={:#x} group(queue={:#x} optional={:#x} event={:#x} write_index={})\n",
                install_queue,
                descriptor_high,
                shared_control,
                queue.storage.channel_control_gpu_va(),
                group.queue,
                group.optional,
                group.event,
                group.write_index,
            );
        }
        self.arm_compute_completion(queue, descriptor_high)?;
        self.publish_compute_b2_once(queue)?;
        submission_phase.store(G17P_SUBMIT_PHASE_B2, Ordering::Release);
        queue
            .compute
            .publish_initial_config_update_pointer(group)?;
        submission_phase.store(G17P_SUBMIT_PHASE_TAG15, Ordering::Release);
        // Tag 15 carries the firmware-visible queue-install request. Prepare
        // every mapping before the remainder of that group is published; no
        // AP-side SKSM MMIO is valid on J700.
        self.prepare_compute_firmware_install(queue)?;
        dev_info!(
            self.resources.dev().as_ref(),
            "G17P submitBuffer: tag15 firmware install published; writing first SKSM entry\n"
        );
        if g17p_compute_sksm_entry_enabled() {
            queue.storage.write_entry(
                prepared_entry.entry_offset as usize,
                prepared_entry.zero_length as usize,
                &prepared_entry.entry,
            )?;
        }
        fence(Ordering::SeqCst);
        queue.producer = prepared_entry.producer_after;
        submission_phase.store(G17P_SUBMIT_PHASE_SKSM_ENTRY, Ordering::Release);
        let descriptor_generation =
            g17_submission::advance_g17p_compute_descriptor_generation(0, 0)
                .map_err(|_| EINVAL)?;
        queue.direct_lifecycle = lifecycle.next;
        // Native's publication order is tag 15 -> 14 -> 16; ours has been
        // 15 -> 16 -> 14. Selectable so both can be tested from one build.
        let native_order = *module_parameters::g17p_native_order.value() != 0;
        let add_kicks = g17_submission::prepare_g17p_ksm_add_kicks_command(
            g17_submission::G17PKsmAddKicksCommand {
                queue_id: queue.queue_id,
                add_count: descriptor_generation,
                queue_priority: queue.producer.completion_selector,
                entry_stamp: prepared_entry.current_timestamp,
            },
        )
        .map_err(|_| EINVAL)?;
        self.log_record("tag14-add-kicks", &add_kicks.bytes);

        let skip = *module_parameters::g17p_skip.value();
        if native_order {
            // Native's firmware-transport order is tag 3 -> 15 -> 14 -> 16.
            // Tag 16's handler ends with a broadcast MMIO[0x210e0]/[0x210d8]
            // = -1 "go" poke, so it must follow AddKicks, not precede it.
            // publish_initial_config_update_pointer leaves the ring producer
            // at 2. group.write_index is staged as 3 for the default order,
            // where tag 16 advances 2 -> 3 first; in native order tag 14 takes
            // slot 2 itself.
            let defer_kicks = *module_parameters::g17p_defer_add_kicks.value() != 0;
            let after_kicks = if skip & 1 != 0 {
                dev_info!(self.resources.dev().as_ref(), "G17P bisect: tag-14 AddKicks SKIPPED\n");
                2
            } else if defer_kicks {
                // Native defers the AddKicks poke (host flag accel[0x662]) and
                // replays it from an IOP power-state callback. It must not be
                // published while a recovery is open: the unhalt's 128-queue
                // restore rewrites 0x21010/0x21018/0x21020 for every queue from
                // the halt-time snapshot and erases it.
                self.pending_add_kicks = Some(add_kicks.bytes);
                dev_info!(
                    self.resources.dev().as_ref(),
                    "G17P compute: tag-14 AddKicks deferred until recovery closes\n"
                );
                2
            } else {
                queue
                    .compute
                    .publish_add_kicks_command(2, &add_kicks.bytes)?
            };
            queue.producer.last_add_kicks_timestamp = prepared_entry.current_timestamp;
            fence(Ordering::SeqCst);
            submission_phase.store(G17P_SUBMIT_PHASE_ADD_KICKS, Ordering::Release);
            group.write_index = if skip & 2 != 0 {
                dev_info!(self.resources.dev().as_ref(), "G17P bisect: tag-16 EntrySignal SKIPPED\n");
                after_kicks
            } else {
                queue
                    .compute
                    .publish_initial_entry_signal_at(after_kicks, &entry_signal)?
            };
            submission_phase.store(G17P_SUBMIT_PHASE_TAG16, Ordering::Release);
        } else {
            queue
                .compute
                .publish_initial_entry_signal(&entry_signal)?;
            submission_phase.store(G17P_SUBMIT_PHASE_TAG16, Ordering::Release);
            group.write_index = queue
                .compute
                .publish_add_kicks_command(group.write_index, &add_kicks.bytes)?;
            queue.producer.last_add_kicks_timestamp = prepared_entry.current_timestamp;
            fence(Ordering::SeqCst);
            submission_phase.store(G17P_SUBMIT_PHASE_ADD_KICKS, Ordering::Release);
        }
        let prepared = self
            .resources
            .publish_compute_channel(group.queue, group.write_index, queue.queue_id, false)?;
        submission_phase.store(G17P_SUBMIT_PHASE_CL2, Ordering::Release);
        // The complete install group is now visible to firmware. From this
        // point a retry must use the retained route; publishing another
        // install for the same QID could reprogram a live queue.
        queue.hardware_state = G17PSksmQueueHardwareState::HardwareRegistered;
        if lifecycle.notifications.activation != Some(prepared.doorbell) {
            return Err(EINVAL);
        }
        dev_info!(
            self.resources.dev().as_ref(),
            "G17P compute: native-direct-first-v2 qid {} entry_offset {:#x} entry_low {:#x} entry_high {:#x} timestamp {:#x} descriptor_generation {} tag15_producer 2 tag16_producer 3 tag14_producer {} addkicks-via-channel-command=true correlation_token {}\n",
            queue.queue_id,
            prepared_entry.entry_offset,
            sksm_entry_aliases.low(),
            sksm_entry_aliases.high(),
            prepared_entry.current_timestamp,
            descriptor_generation,
            group.write_index,
            lifecycle.correlation_token
        );
        queue.submission_ordinal = 1;
        self.log_compute_ring_state(queue, "first-post-cl2");
        self.log_compute_outer_channel("first-post-cl2");
        Ok(lifecycle.notifications)
    }

    /// The steady-state (second and later) submission on an already-installed
    /// queue.
    ///
    /// Everything the first bind does exactly once is skipped here: the QID
    /// configure transaction, the B2 activation, the queue-install flag in
    /// tag 15, and above all the first-bind graph builder, which rewrites the
    /// queue header and RESETS the item-ring producer to 1. Re-running that
    /// builder for a repeat is what made the firmware's consumer cursor and
    /// our producer disagree, so the record was published and never consumed.
    ///
    /// What DOES repeat, in native's transport order:
    ///
    ///   tag 3 (a new descriptor slot) -> tag 15 -> tag 14 -> tag 16 -> CL_2
    ///
    /// appended at the live ring producer rather than at slots 0..3. macOS
    /// republishes tag 15 on every submission -- both live captures are of
    /// already-installed queues and both carry one -- with `+0x1a` clear.
    fn submit_translated_repeat_channel(
        &mut self,
        registers: &regs::Resources,
        queue: &mut G17PSksmQueue,
        submission_phase: &AtomicU32,
        binding: &G17PComputeUserBinding,
        command: &g17_uapi::TranslatedComputeCommand,
        context: G17PComputeSubmissionContext<'_>,
        add3_buffers: Option<[u64; 3]>,
    ) -> Result<g17_submission::PreparedG17PDirectClNotifications> {
        if queue.hardware_state != G17PSksmQueueHardwareState::HardwareRegistered
            || queue.submission_ordinal == 0
        {
            return Err(EINVAL);
        }
        let _ = registers;
        // The add3 operand table lives in the retained preempt object and is
        // unchanged by a completed submission, but the probe may have staged
        // new buffers, so honour the flag exactly as the first bind does.
        if let Some(buffers) = add3_buffers {
            queue.compute.write_g17p_add3_resource_table(buffers)?;
        }

        let ordinal = queue.submission_ordinal;
        let next_ordinal = ordinal.checked_add(1).ok_or(EOVERFLOW)?;
        // Descriptor ordinals, split into the two fields that behave
        // differently. Hardware has ruled on the extremes: advancing everything
        // (mode 1) fails on the very first repeat, pinning everything (mode 0)
        // gets exactly two consecutive submissions. Mode 2 separates them.
        //
        // `sequence_ordinal` feeds submit_sequence / queue_submission /
        // queue_ordinal / submission_index -- pure per-submission counters, and
        // macOS's CL command record advances its +0x04 sequence once per
        // published record.
        //
        // `work_ordinal` is different in kind: it is folded into the register
        // program as `context_word = (context_id << 8) | work_ordinal`, which
        // m1n1's proven add3 left at 0x200. Mode 1 was sending 0x201, which is
        // the most likely reason it failed immediately.
        // The output-positive 100-submit run keeps both descriptor ordinals at
        // their first-bind values. The item-ring producer and entry timestamp
        // provide the forward progress for repeat commands.
        let sequence_ordinal: u32 = 0;
        let work_ordinal = 0;
        let queue_submission = sequence_ordinal.checked_add(1).ok_or(EINVAL)?;
        let stamp = g17_submission::decode_kick_timestamp(queue.producer.current_timestamp)
            .map_err(|_| EINVAL)?;
        let descriptor_slot = stamp.stamp_index;
        let (descriptor_high, _) = queue.compute.descriptor_gpu_vas(descriptor_slot);
        let descriptor_low = binding.mappings.descriptor_gpu_va(descriptor_slot);
        let user_layout = binding.mappings.layout();
        let shared_control = queue.storage.shared_support_gpu_va();
        let objects = g17_compute::ComputeDescriptorObjects {
            scheduler_record: queue.compute.scheduler_record_gpu_va(),
            descriptor_low_alias: descriptor_low,
            dispatch_a: queue.compute.dispatch_a_gpu_va(),
            dispatch_b: queue.compute.dispatch_b_gpu_va(),
            status_a: queue.compute.status_a_gpu_va(),
            status_b: queue.compute.status_b_gpu_va(),
            shared_control,
            zero_page: queue.compute.zero_page_gpu_va(),
        };
        queue.compute.with_descriptor_mut(descriptor_slot, |raw| {
            g17_uapi::build_compute_descriptor(
                command,
                g17_uapi::ComputeInternalState {
                    preempt_base: user_layout.preempt,
                    dispatch_identity: context.dispatch_identity,
                    context_id: context.context_id,
                    work_ordinal,
                    robustness: user_layout.robustness,
                    operand_state_base: user_layout.operand_state,
                    execution_gate: context.execution_gate,
                },
                objects,
                g17_compute::ComputeDescriptorMetadata {
                    submit_sequence: sequence_ordinal as u64,
                    context_id: context.context_id,
                    grid_index: u32::from(queue.queue_id),
                    work_ordinal,
                    queue_submission,
                    queue_ordinal: sequence_ordinal,
                    submission_index: queue_submission,
                    support_control: 0xe0a0_0001,
                    support_flags: 0,
                },
                context.timestamps,
                raw,
            )
            .map_err(|_| EINVAL)
        })?;

        // Ordinal 0 deliberately: the entry's event mask is a set of event
        // SLOT indices, not a counter. macOS's captured kick entries carry the
        // same constant mask value on every submission of a queue (0x22 for
        // CL), and the CL command record's own event quartet is likewise
        // identical across all twelve captured submissions. Only the RCE
        // bindings, which this call also derives, legitimately move -- they
        // follow the descriptor slot.
        let command_state = g17_submission::prepare_g17p_compute_cl_command_state(
            descriptor_low,
            0,
        )
        .map_err(|_| EINVAL)?;
        let prepared_entry = g17_submission::prepare_g17p_cl_kick_entry(
            queue.producer,
            g17_submission::G17PClKickEntryOperands {
                descriptor_flag_4c: context.descriptor_flag_4c,
                descriptor_flag_5c8: context.descriptor_flag_5c8,
                converted_command_timestamp: context.converted_command_timestamp,
                barriers: context.barriers,
                mcache: context.mcache,
                payload: [descriptor_high, queue.compute.queue_record_gpu_va()],
                event_mask: command_state.event_mask,
                rce_kind: context.rce_kind,
                rce_bindings: command_state.rce_bindings,
                auxiliary: context.auxiliary,
            },
        )
        .map_err(|_| EINVAL)?;
        self.log_record("repeat-cl-kick-entry", &prepared_entry.entry);
        if prepared_entry.entry_index != descriptor_slot {
            return Err(EINVAL);
        }
        let backend = g17_submission::T8140_G17P_CL_BACKEND;
        let mut entry_signal = [0u8; g17_submission::G17P_COMPUTE_OPTIONAL_EVENT_SIZE];
        // Tag 16 names the last retired entry, never the kick being published.
        let entry_old_timestamp = queue.producer.previous_timestamp;
        g17_submission::encode_g17p_compute_optional_event(
            g17_submission::G17PComputeOptionalEvent {
                queue_id: u16::from(queue.queue_id),
                old_timestamp: entry_old_timestamp,
            },
            &mut entry_signal,
        )
        .map_err(|_| EINVAL)?;
        let sksm_entry_aliases = queue.storage.entry_aliases()?;
        // Reclaim everything the firmware has finished with, so slots come back
        // round instead of the ring being single-use.
        let trace = Self::compute_trace_enabled(ordinal);
        match queue.compute.recycle_consumed_ring_slots() {
            Ok(_) if !trace => {}
            Ok(0) => {}
            Ok(reclaimed) => dev_info!(
                self.resources.dev().as_ref(),
                "G17P compute: reclaimed {} consumed item-ring slots\n",
                reclaimed
            ),
            Err(error) => dev_err!(
                self.resources.dev().as_ref(),
                "G17P compute: item-ring reclaim failed ({:?}); publishing without it\n",
                error
            ),
        }
        match *module_parameters::g17p_repeat_context.value() {
            0 => {}
            2 => queue
                .compute
                .write_repeat_queue_context_item(
                    descriptor_slot,
                    descriptor_high,
                    queue.queue_id,
                )?,
            _ => queue.compute.refresh_repeat_queue_context(descriptor_high)?,
        }
        let rewind = *module_parameters::g17p_repeat_ring.value() == 0;
        let group = queue.compute.stage_repeat_config_update(
            sksm_entry_aliases,
            shared_control,
            queue.storage.channel_control_gpu_va(),
        )?;
        self.publish_compute_b2_once(queue)?;
        fence(Ordering::SeqCst);
        let prepared = self
            .resources
            .publish_compute_channel(group.queue, reported_write_index, queue.queue_id, true)?;
        submission_phase.store(G17P_SUBMIT_PHASE_CL2, Ordering::Release);
        let _ = prepared;
        queue.direct_lifecycle = lifecycle.next;
        queue.submission_ordinal = next_ordinal;
        // Optional settle. The 319-deep build padded every submission with
        // several console writes here; this makes that padding explicit and
        // measurable instead of an accident of log verbosity.
        let settle = *module_parameters::g17p_repeat_settle_us.value();
        if settle != 0 {
            fsleep(Delta::from_micros(settle as i64));
        }
        self.log_compute_ring_state(queue, "repeat-post-cl2");
        if !trace {
            return Ok(lifecycle.notifications);
        }
        self.log_compute_outer_channel("repeat-post-cl2");
        dev_info!(
            self.resources.dev().as_ref(),
            "G17P compute: repeat submission {} qid {} descriptor-slot {} descriptor {:#x} stamp {:#x} ring {} -> {} activation {:?} kick {:?}\n",
            ordinal,
            queue.queue_id,
            descriptor_slot,
            descriptor_high,
            prepared_entry.current_timestamp,
            producer_before,
            write_index,
            lifecycle.notifications.activation,
            lifecycle.notifications.direct_kick
        );
        Ok(lifecycle.notifications)
    }

    /// Build and publish one complete userspace compute command.
    ///
    /// Descriptor and CL-entry slots share the same 256-entry lifetime. The
    /// descriptor is written first, then its upper address and the retained
    /// queue record are placed in the CL payload before the ordered SKSM kick.
    pub(crate) fn submit_translated_compute(
        &mut self,
        registers: &regs::Resources,
        queue: &mut G17PSksmQueue,
        binding: &G17PComputeUserBinding,
        command: &g17_uapi::TranslatedComputeCommand,
        context: G17PComputeSubmissionContext<'_>,
    ) -> Result<Option<u64>> {
        if !queue.registered
            || queue.scratch != self.sksm_scratch()
            || queue.uat_owner != self.uat().ttb_base()
        {
            return Err(EINVAL);
        }

        if let Some(buffers) = command.g17p_add3_buffers {
            return self
                .submit_translated_add3_channel(queue, binding, command, context, buffers)
                .map(Some);
        }

        let timestamp = g17_submission::decode_kick_timestamp(queue.producer.current_timestamp)
            .map_err(|_| EINVAL)?;
        let (descriptor_high, _) = queue.compute.descriptor_gpu_vas(timestamp.stamp_index);
        let descriptor_low = binding
            .mappings
            .descriptor_gpu_va(timestamp.stamp_index);
        let command_state = g17_submission::prepare_g17p_compute_cl_command_state(
            descriptor_low,
            queue.submission_ordinal,
        )
        .map_err(|_| EINVAL)?;
        let payload = [descriptor_high, queue.compute.queue_record_gpu_va()];
        let operands = command.entry_operands(g17_uapi::ComputeEntryContext {
            descriptor_flag_4c: context.descriptor_flag_4c,
            descriptor_flag_5c8: context.descriptor_flag_5c8,
            converted_command_timestamp: context.converted_command_timestamp,
            barriers: context.barriers,
            mcache: context.mcache,
            payload,
            event_mask: command_state.event_mask,
            rce_kind: context.rce_kind,
            rce_bindings: command_state.rce_bindings,
            auxiliary: context.auxiliary,
        });
        let prepared = g17_submission::prepare_g17p_cl_kick_entry(queue.producer, operands)
            .map_err(|_| EINVAL)?;
        if prepared.entry_index != timestamp.stamp_index {
            return Err(EINVAL);
        }

        let ordinal = queue.submission_ordinal;
        let next_ordinal = ordinal.checked_add(1).ok_or(EOVERFLOW)?;
        let queue_submission = ordinal.checked_add(1).ok_or(EINVAL)?;
        let user_layout = binding.mappings.layout();
        let preempt_base = user_layout.preempt;
        let robustness = user_layout.robustness;
        let operand_state_base = user_layout.operand_state;
        let shared_control = queue.storage.shared_support_gpu_va();
        let objects = g17_compute::ComputeDescriptorObjects {
            scheduler_record: queue.compute.scheduler_record_gpu_va(),
            descriptor_low_alias: descriptor_low,
            dispatch_a: queue.compute.dispatch_a_gpu_va(),
            dispatch_b: queue.compute.dispatch_b_gpu_va(),
            status_a: queue.compute.status_a_gpu_va(),
            status_b: queue.compute.status_b_gpu_va(),
            shared_control,
            zero_page: queue.compute.zero_page_gpu_va(),
        };
        queue
            .compute
            .with_descriptor_mut(prepared.entry_index, |raw| {
                g17_uapi::build_compute_descriptor(
                    command,
                    g17_uapi::ComputeInternalState {
                        preempt_base,
                        dispatch_identity: context.dispatch_identity,
                        context_id: context.context_id,
                        work_ordinal: ordinal,
                        robustness,
                        operand_state_base,
                        execution_gate: context.execution_gate,
                    },
                    objects,
                    g17_compute::ComputeDescriptorMetadata {
                        submit_sequence: ordinal as u64,
                        context_id: context.context_id,
                        grid_index: 6,
                        work_ordinal: ordinal,
                        queue_submission,
                        queue_ordinal: ordinal,
                        submission_index: queue_submission,
                        support_control: 0xe0a0_0001,
                        support_flags: 0,
                    },
                    context.timestamps,
                    raw,
                )
                .map_err(|_| EINVAL)
            })?;

        fence(Ordering::SeqCst);
        let timestamp = self.publish_compute_entry(registers, queue, prepared)?;
        queue.submission_ordinal = next_ordinal;
        let _ = timestamp;
        Ok(None)
    }

    /// Completion ingress for the descriptor-provided completed-kick count.
    pub(crate) fn complete_compute_entries(
        &self,
        queue: &mut G17PSksmQueue,
        completed_count: u32,
    ) -> Result {
        if !queue.registered
            || queue.scratch != self.sksm_scratch()
            || queue.uat_owner != self.uat().ttb_base()
        {
            return Err(EINVAL);
        }
        queue.producer = g17_submission::complete_g17p_cl_kicks(queue.producer, completed_count)
            .map_err(|_| EINVAL)?;
        Ok(())
    }

    pub(crate) fn snapshot_compute_timeout(
        &mut self,
        queue: &mut G17PSksmQueue,
    ) -> Result<G17PComputeTimeoutSnapshot> {
        Ok(G17PComputeTimeoutSnapshot {
            graph: queue.compute.snapshot_initial_channel_group()?,
            channel: self.resources.snapshot_compute_channel()?,
            control: self.resources.control_counters()?,
        })
    }

    /// Publish newly appended kicks for a queue returned by
    /// [`Self::configure_g17p_compute_queue`].
    pub(crate) fn add_sksm_kicks(
        &self,
        registers: &regs::Resources,
        queue: &mut G17PSksmQueue,
        add_count: u32,
    ) -> Result {
        if !queue.registered
            || queue.scratch != self.sksm_scratch()
            || queue.uat_owner != self.uat().ttb_base()
        {
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
