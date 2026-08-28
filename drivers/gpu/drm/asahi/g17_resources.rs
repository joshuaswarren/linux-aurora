// SPDX-License-Identifier: GPL-2.0-only OR MIT

//! T8140 shared UAT and per-role firmware resources.
//!
//! GFX and GFX1 use one 42-bit UAT. Their roots share the same TTBAT owner,
//! while their main, state, and status addresses remain role-specific. The
//! layout here follows the source-built m1n1 G17P graph. It does not start an
//! ASC or publish an init-data doorbell.

#[cfg(not(test))]
use kernel::prelude::*;

#[cfg(not(test))]
use crate::g17_initdata;
#[cfg(not(test))]
use crate::{driver::AsahiDevice, g17_compute, gem, mmu};
#[cfg(test)]
#[path = "g17_initdata.rs"]
mod g17_initdata;
#[cfg(test)]
#[path = "g17_compute.rs"]
mod g17_compute;

#[cfg(not(test))]
use core::ops::Range;

use g17_initdata::{InstanceAddresses, InstanceRole, SharedAddresses};

/// The T8140 absent-handoff UAT constructor is available to production code.
pub(crate) const EXECUTABLE_T8140_SHARED_UAT_AVAILABLE: bool = true;
/// Production code can allocate and map the source-backed per-role set.
pub(crate) const EXECUTABLE_T8140_ROLE_RESOURCES_AVAILABLE: bool = true;

#[cfg(not(test))]
const ROOT_PAIR_ALLOC_SIZE: usize = 0xc000;
const PRIMARY_STATUS_A_STATE_GRID_OFFSET: u64 = 0xee40;
const SECONDARY_EXTRA_0_BEFORE_PRIMARY_STATUS_A: u64 = 0x6c0;
const SECONDARY_HWDATA_STATE_OFFSET: u64 = 0x3c0;
const FWCTL_SIZE: usize = 0x4000;
pub(crate) const G17P_CL_ENTRY_STORAGE_SIZE: usize = 0x20000;
pub(crate) const G17P_CL_SHARED_SUPPORT_SIZE: usize = 0x4000;
pub(crate) const G17P_CL_B2_OBJECT_SIZE: usize = 0x4000;
pub(crate) const G17P_CL_CHANNEL_CONTROL_SIZE: usize = 0x4000;
pub(crate) const G17P_CL_SUPPORT_STATE_SIZE: usize = 0x4000;
pub(crate) const G17P_CL_OPERAND_TABLE_SIZE: usize = 0x4000;
pub(crate) const G17P_CL_LOW_VA_START: u64 = 0x70_0000_0000;
pub(crate) const G17P_CL_LOW_VA_END: u64 = 0x71_0000_0000;
pub(crate) const G17P_COMPUTE_DESCRIPTOR_SLOT_SIZE: usize = 0x4000;
pub(crate) const G17P_COMPUTE_DESCRIPTOR_SLOT_COUNT: usize = 256;
pub(crate) const G17P_COMPUTE_DESCRIPTOR_STORAGE_SIZE: usize =
    G17P_COMPUTE_DESCRIPTOR_SLOT_SIZE * G17P_COMPUTE_DESCRIPTOR_SLOT_COUNT;
pub(crate) const G17P_COMPUTE_PREEMPT_SIZE: usize = 0x4000;
pub(crate) const G17P_COMPUTE_OPERAND_STATE_SIZE: usize = 0x14000;
pub(crate) const G17P_COMPUTE_SUPPORT_SIZE: usize = 0xc000;
pub(crate) const G17P_COMPUTE_QUEUE_GRAPH_SIZE: usize = g17_compute::COMPUTE_QUEUE_GRAPH_SIZE;
#[cfg(not(test))]
const STATUS_A_ALLOC_SIZE: usize = 0xb4000;
#[cfg(not(test))]
const PRIMARY_STATE_ALLOC_SIZE: usize = 0xc4000;
#[cfg(not(test))]
const SECONDARY_STATE_ALLOC_SIZE: usize = 0x4000;

/// One role's view of the common UAT owner and its private objects.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) struct RoleResourceBinding {
    pub(crate) role: InstanceRole,
    /// Physical base of the one TTBAT used by both roles.
    pub(crate) uat_owner: u64,
    pub(crate) instance: InstanceAddresses,
    pub(crate) state_grid: u64,
}

/// Addresses passed from the resource owner to the G17 init-data builder.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) struct G17ResourceHandoff {
    pub(crate) shared: SharedAddresses,
    pub(crate) primary: RoleResourceBinding,
    pub(crate) secondary: RoleResourceBinding,
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) enum ResourceError {
    NullUatOwner,
    SplitUatOwner,
    WrongRole,
    SharedLayout,
    RoleLayout,
    InitdataLayout,
}

/// Validate the exact ownership boundary before either root can be published.
pub(crate) fn validate_handoff(
    handoff: &G17ResourceHandoff,
) -> core::result::Result<(), ResourceError> {
    let primary = &handoff.primary;
    let secondary = &handoff.secondary;

    if primary.uat_owner == 0 || secondary.uat_owner == 0 {
        return Err(ResourceError::NullUatOwner);
    }
    if primary.uat_owner != secondary.uat_owner {
        return Err(ResourceError::SplitUatOwner);
    }
    if primary.role != InstanceRole::Primary || secondary.role != InstanceRole::Secondary {
        return Err(ResourceError::WrongRole);
    }

    let primary_main = handoff
        .shared
        .hw_data_bundle
        .checked_add(g17_initdata::NATIVE_PRIMARY_MAIN_BUNDLE_OFFSET as u64)
        .ok_or(ResourceError::SharedLayout)?;
    let secondary_main = handoff
        .shared
        .hw_data_bundle
        .checked_add(g17_initdata::NATIVE_SECONDARY_MAIN_BUNDLE_OFFSET as u64)
        .ok_or(ResourceError::SharedLayout)?;
    if handoff.shared.hw_data_bundle_alloc != g17_initdata::NATIVE_SHARED_CLUSTER_SIZE
        || primary.instance.main_config != primary_main
        || secondary.instance.main_config != secondary_main
    {
        return Err(ResourceError::SharedLayout);
    }

    if primary.instance.status_b
        != primary
            .state_grid
            .checked_add(g17_initdata::PRIMARY_STATUS_B_STATE_GRID_OFFSET as u64)
            .ok_or(ResourceError::RoleLayout)?
        || primary.instance.status_a
            != primary
                .state_grid
                .checked_add(PRIMARY_STATUS_A_STATE_GRID_OFFSET)
                .ok_or(ResourceError::RoleLayout)?
        || secondary.instance.status_b != 0
        || primary.state_grid == secondary.state_grid
        || primary.instance.status_a == secondary.instance.status_a
    {
        return Err(ResourceError::RoleLayout);
    }

    let secondary_extras = [
        primary
            .instance
            .status_a
            .checked_sub(SECONDARY_EXTRA_0_BEFORE_PRIMARY_STATUS_A)
            .ok_or(ResourceError::RoleLayout)?,
        secondary
            .state_grid
            .checked_add(g17_initdata::PRIMARY_STATUS_B_STATE_GRID_OFFSET as u64)
            .ok_or(ResourceError::RoleLayout)?,
    ];
    if primary.instance.secondary_extras != [0, 0]
        || secondary.instance.secondary_extras != secondary_extras
    {
        return Err(ResourceError::RoleLayout);
    }

    g17_initdata::validate_pair(&primary.instance, &secondary.instance)
        .map_err(|_| ResourceError::InitdataLayout)?;

    let mut primary_root = [0u8; g17_initdata::ROOT_SIZE_PRIMARY];
    g17_initdata::encode_root(
        InstanceRole::Primary,
        &handoff.shared,
        &primary.instance,
        &mut primary_root,
    )
    .map_err(|_| ResourceError::InitdataLayout)?;
    let mut secondary_root = [0u8; g17_initdata::ROOT_SIZE_SECONDARY];
    g17_initdata::encode_root(
        InstanceRole::Secondary,
        &handoff.shared,
        &secondary.instance,
        &mut secondary_root,
    )
    .map_err(|_| ResourceError::InitdataLayout)?;

    Ok(())
}

#[cfg(not(test))]
struct MappedObject {
    mapping: mmu::KernelMapping,
    object: gem::ObjectRef,
    logical_size: usize,
}

#[cfg(not(test))]
impl MappedObject {
    fn new_in_range(
        dev: &AsahiDevice,
        vm: &mmu::Vm,
        range: Range<u64>,
        size: usize,
        alignment: u64,
        prot: mmu::Prot,
    ) -> Result<Self> {
        let mut object = gem::new_kernel_object(dev, size)?;
        object.vmap()?.memset(0);
        let mapping = object.map_into_range(vm, range, alignment, prot, true)?;
        Ok(Self {
            mapping,
            object,
            logical_size: size,
        })
    }

    fn new(
        dev: &AsahiDevice,
        uat: &mmu::Uat,
        size: usize,
        alignment: u64,
        prot: mmu::Prot,
    ) -> Result<Self> {
        Self::new_in_range(
            dev,
            uat.kernel_vm(),
            uat.kernel_va_range()?,
            size,
            alignment,
            prot,
        )
    }

    fn new_at(
        dev: &AsahiDevice,
        vm: &mmu::Vm,
        address: u64,
        size: usize,
        prot: mmu::Prot,
    ) -> Result<Self> {
        let mut object = gem::new_kernel_object(dev, size)?;
        object.vmap()?.memset(0);
        let mapping = object.map_at(vm, address, prot, false)?;
        Ok(Self {
            mapping,
            object,
            logical_size: size,
        })
    }

    fn iova(&self) -> u64 {
        self.mapping.iova()
    }

    fn with_bytes_mut<R>(&mut self, f: impl FnOnce(&mut [u8]) -> Result<R>) -> Result<R> {
        let vmap = self.object.vmap()?;
        let bytes = unsafe {
            // SAFETY: the VMap covers this object's complete, page-rounded
            // allocation and logical_size never exceeds the object size.
            core::slice::from_raw_parts_mut(vmap.as_mut_ptr(), self.logical_size)
        };
        f(bytes)
    }

    fn map_alias_at(
        &mut self,
        vm: &mmu::Vm,
        address: u64,
        prot: mmu::Prot,
    ) -> Result<mmu::KernelMapping> {
        self.object.map_at(vm, address, prot, false)
    }

    fn map_alias_in_range(
        &mut self,
        vm: &mmu::Vm,
        range: Range<u64>,
        alignment: u64,
        prot: mmu::Prot,
    ) -> Result<mmu::KernelMapping> {
        self.object
            .map_into_range(vm, range, alignment, prot, false)
    }

    fn write_roots(
        &mut self,
        shared: &SharedAddresses,
        primary: &InstanceAddresses,
        secondary: &InstanceAddresses,
    ) -> Result {
        let vmap = self.object.vmap()?;
        let bytes = unsafe {
            // SAFETY: the VMap covers this object's complete, page-rounded
            // allocation and logical_size never exceeds the object size.
            core::slice::from_raw_parts_mut(vmap.as_mut_ptr(), self.logical_size)
        };
        g17_initdata::encode_root(
            InstanceRole::Primary,
            shared,
            primary,
            &mut bytes[..g17_initdata::ROOT_SIZE_PRIMARY],
        )
        .map_err(|_| EINVAL)?;
        let secondary_offset = g17_initdata::SECONDARY_ROOT_DELTA as usize;
        g17_initdata::encode_root(
            InstanceRole::Secondary,
            shared,
            secondary,
            &mut bytes[secondary_offset..secondary_offset + g17_initdata::ROOT_SIZE_SECONDARY],
        )
        .map_err(|_| EINVAL)
    }

    fn write_main_configs(
        &mut self,
        shared: &SharedAddresses,
        primary: &RoleResourceBinding,
        secondary: &RoleResourceBinding,
        primary_views: &g17_initdata::PrimaryRegionViews,
    ) -> Result {
        let vmap = self.object.vmap()?;
        let bytes = unsafe {
            // SAFETY: the VMap covers this object's complete, page-rounded
            // allocation and logical_size never exceeds the object size.
            core::slice::from_raw_parts_mut(vmap.as_mut_ptr(), self.logical_size)
        };
        let primary_offset = g17_initdata::NATIVE_PRIMARY_MAIN_BUNDLE_OFFSET;
        let secondary_offset = g17_initdata::NATIVE_SECONDARY_MAIN_BUNDLE_OFFSET;
        let primary_channels = g17_initdata::derive_channel_table(
            InstanceRole::Primary,
            primary.state_grid,
            primary.instance.status_a,
            primary.instance.main_config,
            shared.hw_data_bundle,
        );
        let secondary_channels = g17_initdata::derive_channel_table(
            InstanceRole::Secondary,
            secondary.state_grid,
            secondary.instance.status_a,
            secondary.instance.main_config,
            shared.hw_data_bundle,
        );
        g17_initdata::encode_main_config(
            InstanceRole::Primary,
            shared,
            &primary.instance,
            &primary_channels,
            Some(primary_views),
            &mut bytes[primary_offset..primary_offset + g17_initdata::MAIN_CONFIG_SIZE],
        )
        .map_err(|_| EINVAL)?;
        g17_initdata::encode_main_config(
            InstanceRole::Secondary,
            shared,
            &secondary.instance,
            &secondary_channels,
            None,
            &mut bytes[secondary_offset..secondary_offset + g17_initdata::MAIN_CONFIG_SIZE],
        )
        .map_err(|_| EINVAL)
    }
}

#[cfg(not(test))]
fn write_status_block_at(object: &mut MappedObject, offset: usize) -> Result {
    let end = offset
        .checked_add(g17_initdata::STATUS_BLOCK_SIZE)
        .ok_or(EINVAL)?;
    if end > object.logical_size {
        return Err(EINVAL);
    }
    object.with_bytes_mut(|bytes| {
        g17_initdata::encode_status_block(&mut bytes[offset..end]).map_err(|_| EINVAL)
    })
}

#[cfg(not(test))]
fn map_t8140_register_windows(uat: &mmu::Uat) -> Result<KVec<mmu::KernelMapping>> {
    let mut mappings = KVec::new();
    let page_mask = mmu::UAT_PGMSK as u64;
    for window in g17_initdata::T8140_REGISTER_WINDOWS {
        let phys_page = window.phys & !page_mask;
        let device_page = window.device_va & !page_mask;
        if window.phys & page_mask != window.device_va & page_mask {
            return Err(EINVAL);
        }
        let end = window
            .device_va
            .checked_add(window.size as u64)
            .ok_or(EINVAL)?;
        let span = end
            .checked_sub(device_page)
            .and_then(|size| size.checked_add(page_mask))
            .map(|size| size & !page_mask)
            .ok_or(EINVAL)?;
        mappings.push(
            uat.kernel_vm().map_io(
                device_page,
                phys_page.try_into()?,
                span.try_into()?,
                mmu::PROT_FW_MMIO_RW,
            )?,
            GFP_KERNEL,
        )?;
    }
    Ok(mappings)
}

/// Queue-owned mappings retained from SKSM registration through completion.
#[cfg(not(test))]
pub(crate) struct G17PClRuntimeStorage {
    entries: MappedObject,
    shared_support: MappedObject,
    channel_control: MappedObject,
    support_state: MappedObject,
    scheduler_state: MappedObject,
    operand_table: MappedObject,
}

/// Queue-owned objects referenced by selector-3 compute descriptors.
///
/// Descriptor slots keep both views required by the command ABI: firmware
/// consumes the upper address carried in the CL entry, while descriptor-local
/// register-array pointers use the lower alias. All 256 slots remain mapped
/// until the physical queue is unregistered.
#[cfg(not(test))]
pub(crate) struct G17PComputeDescriptorStorage {
    descriptors: MappedObject,
    descriptors_high: mmu::KernelMapping,
    preempt: MappedObject,
    robustness: MappedObject,
    operand_state: MappedObject,
    support: MappedObject,
    queue_graph: MappedObject,
}

#[cfg(not(test))]
impl G17PComputeDescriptorStorage {
    pub(crate) fn new(
        dev: &AsahiDevice,
        uat: &mmu::Uat,
        channel_control_gpu_va: u64,
    ) -> Result<Self> {
        let mut descriptors = MappedObject::new_in_range(
            dev,
            uat.kernel_lower_vm(),
            G17P_CL_LOW_VA_START..G17P_CL_LOW_VA_END,
            G17P_COMPUTE_DESCRIPTOR_STORAGE_SIZE,
            mmu::UAT_PGSZ as u64,
            mmu::PROT_GPU_FW_SHARED_RW,
        )?;
        let descriptors_high = descriptors.map_alias_in_range(
            uat.kernel_vm(),
            uat.kernel_va_range()?,
            mmu::UAT_PGSZ as u64,
            mmu::PROT_GPU_FW_SHARED_RW,
        )?;
        let preempt = MappedObject::new(
            dev,
            uat,
            G17P_COMPUTE_PREEMPT_SIZE,
            mmu::UAT_PGSZ as u64,
            mmu::PROT_GPU_FW_SHARED_RW,
        )?;
        let robustness = MappedObject::new(
            dev,
            uat,
            mmu::UAT_PGSZ,
            mmu::UAT_PGSZ as u64,
            mmu::PROT_GPU_FW_SHARED_RW,
        )?;
        let operand_state = MappedObject::new_in_range(
            dev,
            uat.kernel_lower_vm(),
            G17P_CL_LOW_VA_START..G17P_CL_LOW_VA_END,
            G17P_COMPUTE_OPERAND_STATE_SIZE,
            mmu::UAT_PGSZ as u64,
            mmu::PROT_GPU_FW_SHARED_RW,
        )?;
        let mut support = MappedObject::new(
            dev,
            uat,
            G17P_COMPUTE_SUPPORT_SIZE,
            mmu::UAT_PGSZ as u64,
            mmu::PROT_GPU_FW_SHARED_RW,
        )?;
        let support_gpu_va = support.iova();
        support.with_bytes_mut(|raw| {
            let scheduler_state = support_gpu_va + mmu::UAT_PGSZ as u64;
            raw[0x00..0x08].copy_from_slice(&scheduler_state.to_le_bytes());
            raw[0x100..0x108].copy_from_slice(&(scheduler_state + 4).to_le_bytes());
            raw[0x110..0x114].copy_from_slice(&0x50u32.to_le_bytes());
            raw[mmu::UAT_PGSZ + 4..mmu::UAT_PGSZ + 8]
                .copy_from_slice(&1u32.to_le_bytes());
            Ok(())
        })?;
        let mut queue_graph = MappedObject::new(
            dev,
            uat,
            G17P_COMPUTE_QUEUE_GRAPH_SIZE,
            mmu::UAT_PGSZ as u64,
            mmu::PROT_GPU_FW_SHARED_RW,
        )?;
        let queue_graph_gpu_va = queue_graph.iova();
        queue_graph.with_bytes_mut(|raw| {
            g17_compute::build_compute_queue_graph(
                queue_graph_gpu_va,
                channel_control_gpu_va,
                raw,
            )
            .map(|_| ())
            .map_err(|_| EINVAL)
        })?;

        Ok(Self {
            descriptors,
            descriptors_high,
            preempt,
            robustness,
            operand_state,
            support,
            queue_graph,
        })
    }

    pub(crate) fn descriptor_gpu_vas(&self, slot: u8) -> (u64, u64) {
        let offset = slot as u64 * G17P_COMPUTE_DESCRIPTOR_SLOT_SIZE as u64;
        (
            self.descriptors_high.iova() + offset,
            self.descriptors.iova() + offset,
        )
    }

    pub(crate) fn preempt_gpu_va(&self) -> u64 {
        self.preempt.iova()
    }

    pub(crate) fn robustness_gpu_va(&self) -> u64 {
        self.robustness.iova()
    }

    pub(crate) fn operand_state_gpu_va(&self) -> u64 {
        self.operand_state.iova()
    }

    pub(crate) fn scheduler_record_gpu_va(&self) -> u64 {
        self.support.iova() + 0x100
    }

    pub(crate) fn dispatch_a_gpu_va(&self) -> u64 {
        self.support.iova() + 0x300
    }

    pub(crate) fn dispatch_b_gpu_va(&self) -> u64 {
        self.support.iova() + 0x308
    }

    pub(crate) fn status_a_gpu_va(&self) -> u64 {
        self.support.iova() + 0x310
    }

    pub(crate) fn status_b_gpu_va(&self) -> u64 {
        self.support.iova() + 0x318
    }

    pub(crate) fn zero_page_gpu_va(&self) -> u64 {
        self.support.iova() + 2 * mmu::UAT_PGSZ as u64
    }

    pub(crate) fn queue_record_gpu_va(&self) -> u64 {
        self.queue_graph.iova()
    }

    pub(crate) fn with_descriptor_mut<R>(
        &mut self,
        slot: u8,
        f: impl FnOnce(&mut [u8]) -> Result<R>,
    ) -> Result<R> {
        let offset = slot as usize * G17P_COMPUTE_DESCRIPTOR_SLOT_SIZE;
        let end = offset
            .checked_add(G17P_COMPUTE_DESCRIPTOR_SLOT_SIZE)
            .ok_or(EINVAL)?;
        self.descriptors
            .with_bytes_mut(|raw| f(&mut raw[offset..end]))
    }

}

#[cfg(not(test))]
impl G17PClRuntimeStorage {
    pub(crate) fn new(dev: &AsahiDevice, uat: &mmu::Uat) -> Result<Self> {
        let entries = MappedObject::new_in_range(
            dev,
            uat.kernel_lower_vm(),
            G17P_CL_LOW_VA_START..G17P_CL_LOW_VA_END,
            G17P_CL_ENTRY_STORAGE_SIZE,
            mmu::UAT_PGSZ as u64,
            mmu::PROT_GPU_FW_SHARED_RW,
        )?;
        let shared_support = MappedObject::new(
            dev,
            uat,
            G17P_CL_SHARED_SUPPORT_SIZE,
            mmu::UAT_PGSZ as u64,
            mmu::PROT_GPU_FW_SHARED_RW,
        )?;
        let channel_control = MappedObject::new(
            dev,
            uat,
            G17P_CL_CHANNEL_CONTROL_SIZE,
            mmu::UAT_PGSZ as u64,
            mmu::PROT_GPU_FW_SHARED_RW,
        )?;
        let support_state = MappedObject::new(
            dev,
            uat,
            G17P_CL_SUPPORT_STATE_SIZE,
            mmu::UAT_PGSZ as u64,
            mmu::PROT_GPU_FW_SHARED_RW,
        )?;
        let scheduler_state = MappedObject::new(
            dev,
            uat,
            G17P_CL_SUPPORT_STATE_SIZE,
            mmu::UAT_PGSZ as u64,
            mmu::PROT_GPU_FW_SHARED_RW,
        )?;
        let operand_table = MappedObject::new(
            dev,
            uat,
            G17P_CL_OPERAND_TABLE_SIZE,
            mmu::UAT_PGSZ as u64,
            mmu::PROT_GPU_FW_SHARED_RW,
        )?;

        Ok(Self {
            entries,
            shared_support,
            channel_control,
            support_state,
            scheduler_state,
            operand_table,
        })
    }

    pub(crate) fn entry_gpu_va(&self) -> u64 {
        self.entries.iova()
    }

    pub(crate) fn shared_support_gpu_va(&self) -> u64 {
        self.shared_support.iova()
    }

    pub(crate) fn channel_control_gpu_va(&self) -> u64 {
        self.channel_control.iova()
    }

    pub(crate) fn support_state_gpu_va(&self) -> u64 {
        self.support_state.iova()
    }

    pub(crate) fn scheduler_state_gpu_va(&self) -> u64 {
        self.scheduler_state.iova()
    }

    pub(crate) fn operand_table_gpu_va(&self) -> u64 {
        self.operand_table.iova()
    }

    pub(crate) fn with_static_pages_mut<R>(
        &mut self,
        f: impl FnOnce(&mut [u8], &mut [u8], &mut [u8], &mut [u8]) -> Result<R>,
    ) -> Result<R> {
        let shared_support = self.shared_support.object.vmap()?;
        let support_state = self.support_state.object.vmap()?;
        let scheduler_state = self.scheduler_state.object.vmap()?;
        let channel_control = self.channel_control.object.vmap()?;
        let shared_support = unsafe {
            // SAFETY: every VMap covers its complete 16 KiB object.
            core::slice::from_raw_parts_mut(
                shared_support.as_mut_ptr(),
                G17P_CL_SHARED_SUPPORT_SIZE,
            )
        };
        let support_state = unsafe {
            // SAFETY: every VMap covers its complete 16 KiB object.
            core::slice::from_raw_parts_mut(
                support_state.as_mut_ptr(),
                G17P_CL_SUPPORT_STATE_SIZE,
            )
        };
        let scheduler_state = unsafe {
            // SAFETY: every VMap covers its complete 16 KiB object.
            core::slice::from_raw_parts_mut(
                scheduler_state.as_mut_ptr(),
                G17P_CL_SUPPORT_STATE_SIZE,
            )
        };
        let channel_control = unsafe {
            // SAFETY: every VMap covers its complete 16 KiB object.
            core::slice::from_raw_parts_mut(
                channel_control.as_mut_ptr(),
                G17P_CL_CHANNEL_CONTROL_SIZE,
            )
        };
        f(
            shared_support,
            support_state,
            scheduler_state,
            channel_control,
        )
    }

    pub(crate) fn write_entry(&mut self, offset: usize, zero_length: usize, body: &[u8]) -> Result {
        let end = offset.checked_add(zero_length).ok_or(EINVAL)?;
        if end > self.entries.logical_size || body.len() > zero_length {
            return Err(EINVAL);
        }
        let vmap = self.entries.object.vmap()?;
        let bytes = unsafe {
            // SAFETY: the VMap covers the complete page-rounded object and
            // logical_size is the requested mapped extent.
            core::slice::from_raw_parts_mut(vmap.as_mut_ptr(), self.entries.logical_size)
        };
        bytes[offset..end].fill(0);
        bytes[offset..offset + body.len()].copy_from_slice(body);
        Ok(())
    }
}

#[cfg(not(test))]
#[allow(dead_code)]
struct G17MappedResources {
    roots: MappedObject,
    shared_cluster: MappedObject,
    region_a: MappedObject,
    region_c: MappedObject,
    primary_state: MappedObject,
    secondary_state: MappedObject,
    secondary_status_a: MappedObject,
    primary_b2_sentinel: MappedObject,
    primary_b2_alias_0: MappedObject,
    primary_b2_alias_1: MappedObject,
    primary_b2_alias_0_low: mmu::KernelMapping,
    primary_b2_alias_1_low: mmu::KernelMapping,
    hw_region_0: MappedObject,
    hw_region_1: MappedObject,
    fwctl: MappedObject,
    register_windows: KVec<mmu::KernelMapping>,
}

#[cfg(not(test))]
struct G17UatOwner {
    uat: KBox<mmu::Uat>,
}

/// One production owner for the T8140 UAT and every GFX/GFX1 mapping.
///
/// Role records contain addresses only. They cannot allocate a second UAT or
/// map private objects behind the owner's back.
#[cfg(not(test))]
#[allow(dead_code)]
pub(crate) struct G17ResourceOwner {
    handoff: G17ResourceHandoff,
    resources: G17MappedResources,
    address_space: G17UatOwner,
}

#[cfg(not(test))]
#[allow(dead_code)]
impl G17ResourceOwner {
    pub(crate) fn new(dev: &AsahiDevice) -> Result<Self> {
        let uat = KBox::new(mmu::Uat::new_t8140(dev, true)?, GFP_KERNEL)?;
        let address_space = G17UatOwner { uat };

        let mut roots = MappedObject::new(
            dev,
            &address_space.uat,
            ROOT_PAIR_ALLOC_SIZE,
            g17_initdata::SECONDARY_ROOT_DELTA,
            mmu::PROT_FW_SHARED_RO,
        )?;
        let mut shared_cluster = MappedObject::new(
            dev,
            &address_space.uat,
            g17_initdata::NATIVE_SHARED_CLUSTER_SIZE,
            mmu::UAT_PGSZ as u64,
            mmu::PROT_FW_PRIV_RW,
        )?;
        let region_a = MappedObject::new(
            dev,
            &address_space.uat,
            g17_initdata::REGION_A_SIZE,
            mmu::UAT_PGSZ as u64,
            mmu::PROT_FW_SHARED_RO,
        )?;
        let mut region_c = MappedObject::new(
            dev,
            &address_space.uat,
            g17_initdata::REGION_C_SIZE,
            mmu::UAT_PGSZ as u64,
            mmu::PROT_FW_SHARED_RW,
        )?;
        let mut primary_state = MappedObject::new(
            dev,
            &address_space.uat,
            PRIMARY_STATE_ALLOC_SIZE,
            mmu::UAT_PGSZ as u64,
            mmu::PROT_FW_SHARED_RW,
        )?;
        let mut secondary_state = MappedObject::new(
            dev,
            &address_space.uat,
            SECONDARY_STATE_ALLOC_SIZE,
            mmu::UAT_PGSZ as u64,
            mmu::PROT_FW_SHARED_RW,
        )?;
        let mut secondary_status_a = MappedObject::new(
            dev,
            &address_space.uat,
            STATUS_A_ALLOC_SIZE,
            mmu::UAT_PGSZ as u64,
            mmu::PROT_FW_SHARED_RW,
        )?;
        let hw_region_0 = MappedObject::new_at(
            dev,
            address_space.uat.kernel_vm(),
            shared_cluster.iova() + g17_initdata::NATIVE_HW_REGION_BUNDLE_OFFSETS[0] as u64,
            mmu::UAT_PGSZ,
            mmu::PROT_FW_SHARED_RW,
        )?;
        let hw_region_1 = MappedObject::new_at(
            dev,
            address_space.uat.kernel_vm(),
            shared_cluster.iova() + g17_initdata::NATIVE_HW_REGION_BUNDLE_OFFSETS[1] as u64,
            mmu::UAT_PGSZ,
            mmu::PROT_FW_SHARED_RW,
        )?;
        let fwctl = MappedObject::new(
            dev,
            &address_space.uat,
            FWCTL_SIZE,
            mmu::UAT_PGSZ as u64,
            mmu::PROT_FW_SHARED_RW,
        )?;
        let register_windows = map_t8140_register_windows(&address_space.uat)?;
        let primary_b2_sentinel = MappedObject::new_at(
            dev,
            address_space.uat.kernel_vm(),
            shared_cluster.iova() + 0x40000,
            G17P_CL_B2_OBJECT_SIZE,
            mmu::PROT_GPU_FW_SHARED_RW,
        )?;
        let mut primary_b2_alias_0 = MappedObject::new(
            dev,
            &address_space.uat,
            G17P_CL_B2_OBJECT_SIZE,
            mmu::UAT_PGSZ as u64,
            mmu::PROT_GPU_FW_SHARED_RW,
        )?;
        let mut primary_b2_alias_1 = MappedObject::new(
            dev,
            &address_space.uat,
            G17P_CL_B2_OBJECT_SIZE,
            mmu::UAT_PGSZ as u64,
            mmu::PROT_GPU_FW_SHARED_RW,
        )?;
        primary_b2_alias_1.with_bytes_mut(|bytes| {
            for (index, value) in [
                0xe000_0000u32,
                0x0800_0000,
                0x0000_0000,
                0x0000_2a00,
                0x0000_1500,
            ]
            .into_iter()
            .enumerate()
            {
                bytes[0x20 + index * 4..0x24 + index * 4]
                    .copy_from_slice(&value.to_le_bytes());
            }
            Ok(())
        })?;
        let primary_b2_alias_0_low = primary_b2_alias_0.map_alias_at(
            address_space.uat.kernel_lower_vm(),
            g17_initdata::REGION_VIEW_LOW_ADDRS[0],
            mmu::PROT_GPU_FW_SHARED_RW,
        )?;
        let primary_b2_alias_1_low = primary_b2_alias_1.map_alias_at(
            address_space.uat.kernel_lower_vm(),
            g17_initdata::REGION_VIEW_LOW_ADDRS[1],
            mmu::PROT_GPU_FW_SHARED_RW,
        )?;
        let primary_region_views = g17_initdata::PrimaryRegionViews {
            sentinel_high: primary_b2_sentinel.iova(),
            alias_high: [primary_b2_alias_0.iova(), primary_b2_alias_1.iova()],
        };

        let shared = SharedAddresses {
            region_a: region_a.iova(),
            region_c: region_c.iova(),
            hw_data_bundle: shared_cluster.iova(),
            hw_data_bundle_alloc: g17_initdata::NATIVE_SHARED_CLUSTER_SIZE,
        };
        let primary_instance = InstanceAddresses {
            root: roots.iova(),
            main_config: shared_cluster.iova()
                + g17_initdata::NATIVE_PRIMARY_MAIN_BUNDLE_OFFSET as u64,
            status_a: primary_state.iova() + PRIMARY_STATUS_A_STATE_GRID_OFFSET,
            status_b: primary_state.iova()
                + g17_initdata::PRIMARY_STATUS_B_STATE_GRID_OFFSET as u64,
            secondary_extras: [0, 0],
        };
        let secondary_instance = InstanceAddresses {
            root: roots.iova() + g17_initdata::SECONDARY_ROOT_DELTA,
            main_config: shared_cluster.iova()
                + g17_initdata::NATIVE_SECONDARY_MAIN_BUNDLE_OFFSET as u64,
            status_a: secondary_status_a.iova(),
            status_b: 0,
            secondary_extras: [
                primary_instance.status_a - SECONDARY_EXTRA_0_BEFORE_PRIMARY_STATUS_A,
                secondary_state.iova() + g17_initdata::PRIMARY_STATUS_B_STATE_GRID_OFFSET as u64,
            ],
        };
        let uat_owner = address_space.uat.ttb_base();
        let handoff = G17ResourceHandoff {
            shared,
            primary: RoleResourceBinding {
                role: InstanceRole::Primary,
                uat_owner,
                instance: primary_instance,
                state_grid: primary_state.iova(),
            },
            secondary: RoleResourceBinding {
                role: InstanceRole::Secondary,
                uat_owner,
                instance: secondary_instance,
                state_grid: secondary_state.iova(),
            },
        };
        validate_handoff(&handoff).map_err(|_| EINVAL)?;
        let region_record_addrs = [hw_region_0.iova(), hw_region_1.iova()];
        shared_cluster.with_bytes_mut(|bytes| {
            g17_initdata::encode_hw_data(
                &g17_initdata::T8140_REGISTER_WINDOWS,
                &g17_initdata::T8140_REGISTER_FLAG_ONLY_SLOTS,
                region_record_addrs,
                &mut bytes[..g17_initdata::HW_DATA_SIZE],
            )
            .map_err(|_| EINVAL)
        })?;
        region_c
            .with_bytes_mut(|bytes| g17_initdata::encode_region_c(bytes).map_err(|_| EINVAL))?;
        write_status_block_at(
            &mut primary_state,
            PRIMARY_STATUS_A_STATE_GRID_OFFSET as usize,
        )?;
        write_status_block_at(&mut secondary_status_a, 0)?;
        primary_state.with_bytes_mut(|bytes| {
            bytes[0x320..0x324]
                .copy_from_slice(&g17_initdata::CONTROL_OPENING_PRODUCER.to_le_bytes());
            let status_b = g17_initdata::PRIMARY_STATUS_B_STATE_GRID_OFFSET;
            let end = status_b + g17_initdata::PRIMARY_STATUS_B_OBJECT_SIZE;
            g17_initdata::encode_primary_status_b(
                fwctl.iova(),
                fwctl.iova() + g17_initdata::CONTROL_RECORD_SIZE as u64,
                &mut bytes[status_b..end],
            )
            .map_err(|_| EINVAL)
        })?;
        secondary_state.with_bytes_mut(|bytes| {
            bytes[0x320..0x324]
                .copy_from_slice(&g17_initdata::CONTROL_OPENING_PRODUCER.to_le_bytes());
            let extra = g17_initdata::PRIMARY_STATUS_B_STATE_GRID_OFFSET;
            g17_initdata::encode_secondary_root_extra_1(
                &mut bytes[extra..extra + g17_initdata::STATUS_BLOCK_SIZE],
            )
            .map_err(|_| EINVAL)
        })?;
        shared_cluster.with_bytes_mut(|bytes| {
            g17_initdata::encode_bundle_static(
                secondary_state.iova() + SECONDARY_HWDATA_STATE_OFFSET,
                bytes,
            )
            .map_err(|_| EINVAL)
        })?;
        shared_cluster.write_main_configs(
            &shared,
            &handoff.primary,
            &handoff.secondary,
            &primary_region_views,
        )?;
        roots.write_roots(&shared, &primary_instance, &secondary_instance)?;

        Ok(Self {
            handoff,
            resources: G17MappedResources {
                roots,
                shared_cluster,
                region_a,
                region_c,
                primary_state,
                secondary_state,
                secondary_status_a,
                primary_b2_sentinel,
                primary_b2_alias_0,
                primary_b2_alias_1,
                primary_b2_alias_0_low,
                primary_b2_alias_1_low,
                hw_region_0,
                hw_region_1,
                fwctl,
                register_windows,
            },
            address_space,
        })
    }

    pub(crate) fn handoff(&self) -> &G17ResourceHandoff {
        &self.handoff
    }

    pub(crate) fn uat(&self) -> &mmu::Uat {
        &self.address_space.uat
    }

    pub(crate) fn with_b2_pages_mut<R>(
        &mut self,
        f: impl FnOnce(&mut [u8], &mut [u8], &mut [u8]) -> Result<R>,
    ) -> Result<R> {
        let computed = self.resources.primary_b2_sentinel.object.vmap()?;
        let region_1 = self.resources.primary_b2_alias_0.object.vmap()?;
        let region_2 = self.resources.primary_b2_alias_1.object.vmap()?;
        let computed = unsafe {
            // SAFETY: each VMap covers its complete 16 KiB object.
            core::slice::from_raw_parts_mut(computed.as_mut_ptr(), G17P_CL_B2_OBJECT_SIZE)
        };
        let region_1 = unsafe {
            // SAFETY: each VMap covers its complete 16 KiB object.
            core::slice::from_raw_parts_mut(region_1.as_mut_ptr(), G17P_CL_B2_OBJECT_SIZE)
        };
        let region_2 = unsafe {
            // SAFETY: each VMap covers its complete 16 KiB object.
            core::slice::from_raw_parts_mut(region_2.as_mut_ptr(), G17P_CL_B2_OBJECT_SIZE)
        };
        f(computed, region_1, region_2)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const KERN: u64 = 0xffff_fc20_0000_0000;

    #[test]
    fn g17p_cl_runtime_storage_has_native_ring_geometry() {
        assert_eq!(G17P_CL_ENTRY_STORAGE_SIZE, 256 * 0x200);
        assert_eq!(G17P_CL_SHARED_SUPPORT_SIZE, 0x4000);
        assert_eq!(G17P_CL_B2_OBJECT_SIZE, 0x4000);
        assert_eq!(G17P_CL_CHANNEL_CONTROL_SIZE, 0x4000);
        assert_eq!(G17P_CL_LOW_VA_START, 0x70_0000_0000);
        assert_eq!(G17P_CL_LOW_VA_END, 0x71_0000_0000);
        assert_eq!(G17P_COMPUTE_DESCRIPTOR_SLOT_SIZE, 0x4000);
        assert_eq!(G17P_COMPUTE_DESCRIPTOR_SLOT_COUNT, 256);
        assert_eq!(G17P_COMPUTE_DESCRIPTOR_STORAGE_SIZE, 0x400000);
        assert_eq!(G17P_COMPUTE_PREEMPT_SIZE, 0x4000);
        assert_eq!(G17P_COMPUTE_OPERAND_STATE_SIZE, 0x14000);
        assert_eq!(G17P_COMPUTE_SUPPORT_SIZE, 0xc000);
        assert_eq!(G17P_COMPUTE_QUEUE_GRAPH_SIZE, 0x8000);
        assert_eq!(g17_initdata::T8140_REGISTER_WINDOWS.len(), 18);
        assert_eq!(g17_initdata::T8140_REGISTER_WINDOWS[0].slot, 0);
        assert_eq!(
            g17_initdata::T8140_REGISTER_WINDOWS[0].phys,
            0x0030_1014_000
        );
        assert_eq!(g17_initdata::T8140_REGISTER_WINDOWS[17].slot, 41);
        assert_eq!(g17_initdata::T8140_REGISTER_FLAG_ONLY_SLOTS.len(), 17);
    }

    fn valid_handoff() -> G17ResourceHandoff {
        let bundle = KERN + 0x4000_0000;
        let primary_state = KERN + 0x1000_0000;
        let secondary_state = KERN + 0x2000_0000;
        let primary_status_a = primary_state + PRIMARY_STATUS_A_STATE_GRID_OFFSET;
        G17ResourceHandoff {
            shared: SharedAddresses {
                region_a: KERN + 0x0100_0000,
                region_c: KERN + 0x0100_4000,
                hw_data_bundle: bundle,
                hw_data_bundle_alloc: g17_initdata::NATIVE_SHARED_CLUSTER_SIZE,
            },
            primary: RoleResourceBinding {
                role: InstanceRole::Primary,
                uat_owner: 0x9000_0000,
                instance: InstanceAddresses {
                    root: KERN + 0x0200_0000,
                    main_config: bundle + g17_initdata::NATIVE_PRIMARY_MAIN_BUNDLE_OFFSET as u64,
                    status_a: primary_status_a,
                    status_b: primary_state
                        + g17_initdata::PRIMARY_STATUS_B_STATE_GRID_OFFSET as u64,
                    secondary_extras: [0, 0],
                },
                state_grid: primary_state,
            },
            secondary: RoleResourceBinding {
                role: InstanceRole::Secondary,
                uat_owner: 0x9000_0000,
                instance: InstanceAddresses {
                    root: KERN + 0x0200_0000 + g17_initdata::SECONDARY_ROOT_DELTA,
                    main_config: bundle + g17_initdata::NATIVE_SECONDARY_MAIN_BUNDLE_OFFSET as u64,
                    status_a: KERN + 0x3000_0000,
                    status_b: 0,
                    secondary_extras: [
                        primary_status_a - SECONDARY_EXTRA_0_BEFORE_PRIMARY_STATUS_A,
                        secondary_state + g17_initdata::PRIMARY_STATUS_B_STATE_GRID_OFFSET as u64,
                    ],
                },
                state_grid: secondary_state,
            },
        }
    }

    #[test]
    fn exact_t8140_handoff_is_executable_and_source_accepted() {
        assert!(EXECUTABLE_T8140_SHARED_UAT_AVAILABLE);
        assert!(EXECUTABLE_T8140_ROLE_RESOURCES_AVAILABLE);
        assert_eq!(validate_handoff(&valid_handoff()), Ok(()));
    }

    #[test]
    fn split_or_null_uat_owner_is_invalid() {
        let mut handoff = valid_handoff();
        handoff.secondary.uat_owner += 0x4000;
        assert_eq!(
            validate_handoff(&handoff),
            Err(ResourceError::SplitUatOwner)
        );

        let mut handoff = valid_handoff();
        handoff.primary.uat_owner = 0;
        assert_eq!(validate_handoff(&handoff), Err(ResourceError::NullUatOwner));
    }

    #[test]
    fn cross_role_or_aliased_resources_are_invalid() {
        let mut handoff = valid_handoff();
        handoff.primary.role = InstanceRole::Secondary;
        assert_eq!(validate_handoff(&handoff), Err(ResourceError::WrongRole));

        let mut handoff = valid_handoff();
        handoff.secondary.instance.main_config = handoff.primary.instance.main_config;
        assert_eq!(validate_handoff(&handoff), Err(ResourceError::SharedLayout));

        let mut handoff = valid_handoff();
        handoff.secondary.state_grid = handoff.primary.state_grid;
        assert_eq!(validate_handoff(&handoff), Err(ResourceError::RoleLayout));
    }

    #[test]
    fn role_specific_status_and_extra_relations_are_invalid() {
        let mut handoff = valid_handoff();
        handoff.secondary.instance.status_b = KERN + 0x4000;
        assert_eq!(validate_handoff(&handoff), Err(ResourceError::RoleLayout));

        let mut handoff = valid_handoff();
        handoff.secondary.instance.secondary_extras[1] += 0x4000;
        assert_eq!(validate_handoff(&handoff), Err(ResourceError::RoleLayout));

        let mut handoff = valid_handoff();
        handoff.primary.instance.secondary_extras[0] = KERN + 0x8000;
        assert_eq!(validate_handoff(&handoff), Err(ResourceError::RoleLayout));
    }
}
