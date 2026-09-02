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
use crate::{
    driver::{AsahiDevRef, AsahiDevice},
    g17_completion, g17_compute, g17_render, g17_submission, g17_uapi, gem, mem, mmu,
};
#[cfg(test)]
#[path = "g17_completion.rs"]
mod g17_completion;
#[cfg(test)]
#[path = "g17_compute.rs"]
mod g17_compute;
#[cfg(test)]
#[path = "g17_initdata.rs"]
mod g17_initdata;
#[cfg(test)]
#[path = "g17_render.rs"]
mod g17_render;
#[cfg(test)]
#[path = "g17_submission.rs"]
mod g17_submission;

#[cfg(not(test))]
use core::{
    ops::Range,
    sync::atomic::{AtomicU32, Ordering},
};

use g17_initdata::{InstanceAddresses, InstanceRole, SharedAddresses};

/// Linux counterpart of IOGPUFamily's `___globalObjectIDCounter`.
///
/// The native render path uses this one counter for userspace GID-group
/// reservations and for the kernel command IDs allocated after a reservation.
#[cfg(not(test))]
static G17P_RENDER_GID_COUNTER: AtomicU32 = AtomicU32::new(1);

#[cfg(not(test))]
fn g17p_render_gid_add(delta: u32) -> Result<(u32, u32)> {
    let mut old = G17P_RENDER_GID_COUNTER.load(Ordering::Acquire);
    loop {
        let new = old.checked_add(delta).ok_or(EOVERFLOW)?;
        match G17P_RENDER_GID_COUNTER.compare_exchange_weak(
            old,
            new,
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => return Ok((old, new)),
            Err(observed) => old = observed,
        }
    }
}

#[cfg(not(test))]
fn g17p_reserve_render_gid_group() -> Result<u32> {
    let (predecessor, _) = g17p_render_gid_add(g17_render::G17P_RENDER_GID_GROUP_STRIDE)?;
    if predecessor == 0 {
        Err(EOVERFLOW)
    } else {
        Ok(predecessor)
    }
}

#[cfg(not(test))]
fn g17p_allocate_render_lifecycle_pair(predecessor: u32) -> Result<(u64, u64)> {
    // This kernel's OSAddAtomic64 is an arm64 `ldadd`: it returns the value
    // from before the increment. Allocate both IDs in one transaction so the
    // returned old value is 3D's current ID and the following value is TA's.
    let (fragment_current, _) = g17p_render_gid_add(2)?;
    g17_render::g17p_render_lifecycle_pair(predecessor, fragment_current).ok_or(EOVERFLOW)
}

/// The T8140 absent-handoff UAT constructor is available to production code.
pub(crate) const EXECUTABLE_T8140_SHARED_UAT_AVAILABLE: bool = true;
/// Production code can allocate and map the source-backed per-role set.
pub(crate) const EXECUTABLE_T8140_ROLE_RESOURCES_AVAILABLE: bool = true;

#[cfg(not(test))]
const ROOT_PAIR_ALLOC_SIZE: usize = 0xc000;
const PRIMARY_STATUS_A_STATE_GRID_OFFSET: u64 = 0xee40;
const PRIMARY_STATUS_A_WORK_SCAN_OFFSET: u64 = 0x0c;
const PRIMARY_STATUS_A_RUNTIME_OFFSET: u64 = 0x10;
const PRIMARY_STATUS_A_POWER_CALLBACK_COUNT_OFFSET: u64 = 0x1c;
const PRIMARY_STATUS_A_WORK_SCAN_STATE_GRID_OFFSET: u64 =
    PRIMARY_STATUS_A_STATE_GRID_OFFSET + PRIMARY_STATUS_A_WORK_SCAN_OFFSET;
const PRIMARY_STATUS_A_RUNTIME_STATE_GRID_OFFSET: u64 =
    PRIMARY_STATUS_A_STATE_GRID_OFFSET + PRIMARY_STATUS_A_RUNTIME_OFFSET;
// Exact J700 A000 firmware-recovery handshake inside primary Status-B. The
// worker clears the EP21 callback before entering this protocol, increments
// +0x48f0, drives +0x4900 through 1 -> 2 and 3 -> 0, then reinstalls it.
const PRIMARY_STATUS_B_FIRMWARE_RECOVERY_EPOCH_OFFSET: u64 = 0x48f0;
const PRIMARY_STATUS_B_FIRMWARE_RECOVERY_STATE_OFFSET: u64 = 0x4900;
const PRIMARY_STATUS_B_RECOVERY_INFO_OFFSET: u64 = 0x40b8;
const PRIMARY_STATUS_B_RECOVERY_INFO_ENTRY_COUNT: usize = 0x100;
const PRIMARY_STATUS_B_RECOVERY_INFO_ENTRY_STRIDE: u64 = 8;
const PRIMARY_STATUS_B_RECOVERY_INFO_EMITTED_COUNT_OFFSET: u64 = 0x48b8;
const PRIMARY_STATUS_B_RECOVERY_ZERO_0_OFFSET: u64 = 0x48bc;
const PRIMARY_STATUS_B_RECOVERY_SOURCE_COUNT_OFFSET: u64 = 0x48c0;
const PRIMARY_STATUS_B_RECOVERY_ZERO_1_OFFSET: u64 = 0x48c4;
const PRIMARY_STATUS_B_RECOVERY_COMMAND_WORD_OFFSET: u64 = 0x48c8;
const PRIMARY_STATUS_B_RECOVERY_RAW_48CC_OFFSET: u64 = 0x48cc;
const PRIMARY_STATUS_B_RECOVERY_RAW_48D0_OFFSET: u64 = 0x48d0;
const PRIMARY_STATUS_B_RECOVERY_FAULT_STATUS_OFFSET: u64 = 0x48d8;
const PRIMARY_STATUS_B_RECOVERY_OBJECT_ACTIVE_OFFSET: u64 = 0x4910;
const PRIMARY_FIRMWARE_EVENT_CHANNEL_INDEX: usize = 13;
const SECONDARY_EXTRA_0_BEFORE_PRIMARY_STATUS_A: u64 = 0x6c0;
const SECONDARY_HWDATA_STATE_OFFSET: u64 = 0x3c0;
const FWCTL_SIZE: usize = 0x4000;
/// Size of the KSM completion record in completion ordinal 0.
///
/// Layout established on hardware: four GPU timestamps at +0x10/+0x18/+0x20/
/// +0x28, the submitted descriptor at +0x30 and the CL queue at +0x38. The last
/// field ends at 0x40, so that is the record.
const G17P_COMPUTE_COMPLETION_RECORD_SIZE: usize = 0x40;
/// Bits of a completion-record timestamp that are actually time.
///
/// Firmware RE (`b1` `0xab64`, the KSM completion handler): every timestamp it
/// loads out of a record is immediately masked with `0x3fff_ffff_ffff_ffff`
/// before it is used --
///
/// ```text
/// ldp x8, x9, [x19, #32]            ; +0x20, +0x28
/// and x22, x8, #0x3fffffffffffff
/// and x27, x9, #0x3fffffffffffff
/// ```
///
/// -- so the top ten bits are tag bits, not time. Reading them unmasked is why
/// a record could satisfy "end != 0" while carrying no timestamp at all: tag
/// bits alone make the raw qword non-zero.
const G17P_COMPUTE_COMPLETION_TIMESTAMP_MASK: u64 = 0x003f_ffff_ffff_ffff;
/// Bit 0 of `+0x30` is a flag, not part of the descriptor address.
///
/// Same handler, immediately after loading the pair:
///
/// ```text
/// ldp x22, x20, [x0, #48]           ; +0x30 -> x22, +0x38 -> x20
/// and x26, x22, #0xfffffffffffffffe ; the firmware masks bit 0 off
/// ```
const G17P_COMPUTE_COMPLETION_DESCRIPTOR_MASK: u64 = !1u64;
/// Offset of the compute completion stamp inside the queue support object.
///
/// This is the word the selector-3 descriptor names at `+0x0F40`, i.e. what
/// the driver already calls `dispatch_a`. Firmware RE (`b1`
/// `AGFAcceleratorProcessTimeStampQueue` at `0x168d0`): after the completion
/// record has been consumed, the firmware issues its GPU cache-maintenance
/// command to `sgx+0x1_8030`, polls `sgx+0x1_8038` bit 1 until it clears,
/// scans the fault units, and only then does
///
/// ```text
/// ldp x26, x21, [x0]        ; x26 = *(descriptor + 0x0F40)
/// ldp w22, w23, [x0, #16]   ; w22 =  descriptor + 0x0F50 (the stamp value)
/// str w22, [x26]            ; the host-visible completion stamp
/// ```
///
/// So the stamp store is the first host-visible event that is ordered AFTER
/// the flush, which is precisely the property a completion needs.
const G17P_COMPUTE_STAMP_SUPPORT_OFFSET: u64 = 0x300;

pub(crate) const G17P_CL_ENTRY_STORAGE_SIZE: usize =
    g17_submission::T8140_G17P_HAL200_SKSM_QUEUE_GEOMETRY.backing_size();
pub(crate) const G17P_CL_SHARED_SUPPORT_SIZE: usize = 0x4000;
pub(crate) const G17P_CL_B2_OBJECT_SIZE: usize = 0x4000;
pub(crate) const G17P_CL_CHANNEL_CONTROL_SIZE: usize = 0x4000;
pub(crate) const G17P_CL_SUPPORT_STATE_SIZE: usize = 0x4000;
pub(crate) const G17P_CL_OPERAND_TABLE_SIZE: usize = 0x4000;
const G17P_PB_DESCRIPTOR_TABLE_SIZE: usize = 0x4000;
const G17P_UMA_PAGE_POOL_DESCRIPTOR_TABLE_SIZE: usize = 0x4000;
const G17P_LAST_SUBMITTED_HW_TIMESTAMP_COUNT: usize = 128;
const G17P_LAST_SUBMITTED_HW_TIMESTAMP_STRIDE: usize = 0x10;
const G17P_LAST_SUBMITTED_HW_TIMESTAMP_TABLE_SIZE: usize =
    G17P_LAST_SUBMITTED_HW_TIMESTAMP_COUNT * G17P_LAST_SUBMITTED_HW_TIMESTAMP_STRIDE;
/// `AGXQOSManager` constructs its `QOSQueueIDAllocator` with capacity 0x80;
/// firmware's QID/HWBufferID tables have the same 128-entry geometry.
const G17P_QOS_QUEUE_COUNT: usize = 0x80;
/// QoS weight selected by `AGXQOSManager::registerQueue` for an ordinary
/// priority-2 command queue.
///
/// The G17P kext indexes the five-byte table `{0x14, 0x0f, 0x04, 0x01, 0}`
/// with zero for every command-queue priority other than 0/1, then stores the
/// selected byte at `AGXQOSQueue + 0x46`. Both TA and 3D `willSubmit` calls
/// copy that byte to their QID-indexed firmware-visible queue records.
const G17P_DEFAULT_RENDER_QOS_CLASS: u8 = 0x14;
/// Native's normalized per-HardwareBufferID share for one registered queue:
/// `qos_class * group_weight * 2^13 / (queue_count * total_weight)`.
/// Linux currently serializes one render command queue, so both weights and
/// the queue count cancel and the exact value is `0x14 << 13`.
const G17P_SINGLE_RENDER_QOS_SHARE: u32 = (G17P_DEFAULT_RENDER_QOS_CLASS as u32) << 13;
/// `AGXArmFirmware::initFirmwareSharedData` publishes this allocation through
/// primary main-config `+0x274`, the fifth pointer in the packed bundle-view
/// array.  The native CPU mapping is `AGXFirmware + 0x2f0`, its firmware GPU
/// pointer is `AGXFirmware + 0x338`, and the allocation is exactly 0x800 bytes.
const G17P_LAST_SUBMITTED_HW_TIMESTAMP_BUNDLE_VIEW_INDEX: usize = 4;
const G17P_LAST_SUBMITTED_HW_TIMESTAMP_BUNDLE_OFFSET: usize =
    g17_initdata::BUNDLE_VIEW_OFFSETS[G17P_LAST_SUBMITTED_HW_TIMESTAMP_BUNDLE_VIEW_INDEX];
pub(crate) const G17P_CL_LOW_VA_START: u64 = 0x70_0000_0000;
pub(crate) const G17P_CL_LOW_VA_END: u64 = 0x71_0000_0000;
/// Dynamic queue, descriptor, and render state live above every native fixed
/// low resource. The lower 32 MiB remain available for temporal bootstrap,
/// readiness, B2 aliases, and the fixed completion-queue aliases.
pub(crate) const G17P_CL_DYNAMIC_LOW_VA_START: u64 = 0x70_0200_0000;
/// Architectural base for compact user/GART pointers consumed by the TA.
const G17P_RENDER_USER_BASE: u64 = 0x10_0000_0000;
const G17P_RENDER_USER_END: u64 = 0x11_0000_0000;
/// Driver-reserved client-root arena for compact aliases of render-state
/// subranges. Two alternating 512 KiB slots allow a new retained generation
/// to be built completely before the old mappings are released.  The larger
/// slots also keep the native RT-memory and TPC views separate and aligned.
const G17P_RENDER_USER_ALIAS_START: u64 = 0x10_0200_0000;
const G17P_RENDER_USER_ALIAS_END: u64 = 0x10_0210_0000;
const G17P_RENDER_USER_ALIAS_SLOT_SIZE: u64 = 0x8_0000;
/// AXFB comes from the queue's ordinary user allocator on macOS, rather than
/// the compact Buffer/Scene allocator used for tilemap, heap metadata and TPC.
/// Preserve that separate upper user-GART address class with two retained
/// 32-KiB slots.
const G17P_RENDER_AUX_ALIAS_START: u64 = 0x100_0200_0000;
const G17P_RENDER_AUX_ALIAS_SLOT_SIZE: u64 = G17P_RENDER_STATE_AUX_FB_SIZE as u64;
const G17P_RENDER_AUX_ALIAS_END: u64 =
    G17P_RENDER_AUX_ALIAS_START + 2 * G17P_RENDER_AUX_ALIAS_SLOT_SIZE;
/// Kernel-owned compact-GART aperture for TVB backing. Keep it below the
/// high-half aliases Honeykrisp derives from descending main-heap command BOs;
/// the first such alias is currently near 0x10ffed0000. The aperture holds two
/// maximum-size TVBs so retained-state replacement can allocate the new object
/// before releasing the old one.
const G17P_RENDER_USER_TVB_START: u64 = G17P_RENDER_USER_ALIAS_END;
const G17P_RENDER_USER_TVB_END: u64 = 0x10_8000_0000;

#[cfg(not(test))]
fn g17p_cl_dynamic_low_va_range() -> Range<u64> {
    G17P_CL_DYNAMIC_LOW_VA_START..G17P_CL_LOW_VA_END
}

fn clear_g17p_last_submitted_hw_timestamps(bytes: &mut [u8]) -> bool {
    if bytes.len() < G17P_LAST_SUBMITTED_HW_TIMESTAMP_TABLE_SIZE {
        return false;
    }
    for qid in 0..G17P_LAST_SUBMITTED_HW_TIMESTAMP_COUNT {
        let entry = qid * G17P_LAST_SUBMITTED_HW_TIMESTAMP_STRIDE;
        bytes[entry..entry + 4].fill(0);
        bytes[entry + 8..entry + 0x10].fill(0);
    }
    true
}

fn g17p_last_submitted_hw_timestamp_table_mut(bytes: &mut [u8]) -> Option<&mut [u8]> {
    let end = G17P_LAST_SUBMITTED_HW_TIMESTAMP_BUNDLE_OFFSET
        .checked_add(G17P_LAST_SUBMITTED_HW_TIMESTAMP_TABLE_SIZE)?;
    bytes.get_mut(G17P_LAST_SUBMITTED_HW_TIMESTAMP_BUNDLE_OFFSET..end)
}

pub(crate) const G17P_COMPUTE_DESCRIPTOR_SLOT_SIZE: usize = 0x4000;
pub(crate) const G17P_COMPUTE_DESCRIPTOR_SLOT_COUNT: usize =
    g17_submission::T8140_G17P_HAL200_SKSM_QUEUE_GEOMETRY.stamp_count() as usize;
pub(crate) const G17P_COMPUTE_DESCRIPTOR_STORAGE_SIZE: usize =
    G17P_COMPUTE_DESCRIPTOR_SLOT_SIZE * G17P_COMPUTE_DESCRIPTOR_SLOT_COUNT;
pub(crate) const G17P_COMPUTE_PREEMPT_SIZE: usize = 0x4000;
const G17P_COMPUTE_ADD3_RESOURCE_TABLE: usize = 0x14a0;
pub(crate) const G17P_COMPUTE_OPERAND_STATE_SIZE: usize = 0x14000;
pub(crate) const G17P_COMPUTE_SUPPORT_SIZE: usize = 0xc000;
pub(crate) const G17P_COMPUTE_QUEUE_GRAPH_SIZE: usize = g17_compute::COMPUTE_QUEUE_GRAPH_SIZE;
pub(crate) const G17P_RENDER_DESCRIPTOR_STORAGE_SIZE: usize = 0x8000;
/// Number of retained ordinals representable by the optional-record ABI.
pub(crate) const G17P_RETAINED_RENDER_ORDINALS: u32 = 0x100;
pub(crate) const G17P_RENDER_QUEUE_GRAPH_SIZE: usize = 0xc000;
pub(crate) const G17P_RENDER_SUPPORT_SIZE: usize = 0x40000;
const G17P_PAGE_SIZE: usize = 0x4000;
const G17P_RENDER_SECONDARY_INDEX: usize = 0x00000;
const G17P_RENDER_POOL_A_SLOTS: usize = 0x04000;
const G17P_RENDER_POOL_B_SLOTS: usize = 0x08000;
const G17P_RENDER_SHARED_SLOTS: usize = 0x0c000;
const G17P_RENDER_FLAG: usize = 0x10000;
const G17P_RENDER_POOL_A: usize = 0x14000;
const G17P_RENDER_POOL_B: usize = 0x18000;
const G17P_RENDER_PACKED_SHARED: usize = 0x1c000;
const G17P_RENDER_ZERO_SHARED: usize = 0x20000;
const G17P_RENDER_SHARED_CONTROL: usize = 0x24000;
const G17P_RENDER_SHARED_CONTROL_INNER: usize = 0x28000;
// TA and 3D channels on one Metal command queue share the single
// AGXCommandQueueReclaimableState context descriptor. Both tag-15 +0x4a
// fields and both _AGFIChannelState +0x70 fields name this same DVA.
const G17P_RENDER_CHANNEL_CONTROL: usize = 0x2c000;

const G17P_CONTROL_DIRECTORY_VA: u64 = 0x70_0000_0000;
// The positive first-partial render root owns 128 16-KiB leaves here. The
// first failed Linux group proved that a 64-KiB placeholder is insufficient:
// GFX writes the fifth leaf at 0x7000010000 during channel-8 startup.
const G17P_CONTROL_DIRECTORY_SIZE: usize = 0x20_0000;
// The native per-workqueue FList owns this complete main backing. It remains
// zero before QID configuration; its compact run list is a separate resource.
const G17P_CONTROL_OPERAND_PAGE_LIST_SIZE: usize = G17P_CONTROL_DIRECTORY_SIZE;
// Firmware also writes the native high private-state arena by fixed offset,
// independently of the status pointers carried in initdata. The first missing
// leaf observed on GFX1 was base +0x17c000, inside this exact source-built run.
const G17P_NATIVE_PRIVATE_CLUSTER_OFFSET: u64 = 0x0002_0000;
const G17P_NATIVE_PRIVATE_CLUSTER_SIZE: usize = 0x0017_8000;
#[cfg(test)]
const G17P_NATIVE_SECONDARY_STATUS_A_OFFSET: u64 = 0x000e_3100;
/// Exact executable extent used by the positive first-partial render root.
pub(crate) const G17P_BOOTSTRAP_VDM_VA: u64 = 0x10_0001_8000;
const G17P_BOOTSTRAP_VDM_SIZE: usize = 0x8000;
const G17P_BOOTSTRAP_CONTEXT_BASE: u64 = 0x10_0000_0000;
const G17P_BOOTSTRAP_CONTEXT_SIZE: usize = 0x1_0000;
const G17P_VDM_STREAM_TERMINATE: u32 = 6 << 29;
const G17P_CONTROL_OPERAND_TABLE_VA: u64 = g17_initdata::CONTROL_OPERAND_TABLE_ADDRESS;
// The shared-support pointer names a 64-KiB native backing even though the
// fixed registration entries occupy only its first 16-KiB page.
const G17P_CONTROL_OPERAND_TABLE_BACKING_SIZE: usize = 0x1_0000;
const G17P_CONTROL_OPERAND_RUN_LENGTH_PAGES: u64 = 0x100;
const G17P_CONTROL_OPERAND_RUN_ADDRESS_MASK: u64 = 0x0000_ffff_ffff_f000;
const G17P_CONTROL_OPERAND_BUFFER_BASE: u64 = g17_initdata::CONTROL_OPERAND_BUFFER_BASE;
const G17P_CONTROL_OPERAND_BUFFER_COUNT: usize = 28;
/// Native allocates 22 one-MiB blocks before publishing the UMA state, then
/// appends four blocks in the first asynchronous grow advertised by opcode
/// 0x20. Each block consumes eight qword slots even though only slot zero is
/// populated.
const G17P_CONTROL_OPERAND_INITIAL_BLOCK_COUNT: usize =
    g17_initdata::CONTROL_OPERAND_ENTRY_COUNT;
const G17P_CONTROL_OPERAND_FIRST_GROW_BLOCK_COUNT: usize = 4;
const G17P_CONTROL_OPERAND_ACTIVE_BLOCK_COUNT: usize =
    G17P_CONTROL_OPERAND_INITIAL_BLOCK_COUNT + G17P_CONTROL_OPERAND_FIRST_GROW_BLOCK_COUNT;
const G17P_CONTROL_OPERAND_RUN_SLOT_SIZE: usize = 8 * core::mem::size_of::<u64>();
const G17P_CONTROL_OPERAND_BUFFER_SIZE: usize =
    g17_initdata::CONTROL_OPERAND_BUFFER_SIZE as usize;
const G17P_CONTROL_OPERAND_BUFFER_STRIDE: usize =
    g17_initdata::CONTROL_OPERAND_BUFFER_STRIDE as usize;
pub(crate) const G17P_DATA_MASTER_CHANNEL_COUNT: usize = 12;
const G17P_COMPUTE_READINESS_CLASS3_PRIMARY_INDEX_OFFSET: usize =
    (g17_initdata::COMPUTE_READINESS_CLASS3_SUPPORT_ADDRESS
        - g17_submission::G17P_PARTIAL_OPENING_PRIMARY_INDEX_FIRMWARE_GPU_VA)
        as usize;
const G17P_BOOTSTRAP_TA_DESCRIPTOR_LOW_VA: u64 = 0x70_0000_0000;
const G17P_BOOTSTRAP_3D_DESCRIPTOR_LOW_VA: u64 = 0x70_0009_8000;
const G17P_BOOTSTRAP_DESCRIPTOR_ZERO_A_VA: u64 = 0xffff_fc20_001c_8000;
const G17P_BOOTSTRAP_DESCRIPTOR_ZERO_B_VA: u64 = 0xffff_fc20_c07c_0000;
/// Relocated 2026-08-31. The original `0xffff_fc20_0161_0000` is page 4 of the
/// window at `0xffff_fc20_0160_0000`, and that page is already mapped by the
/// time render storage is built -- a 64-page occupancy scan read
/// `0x0000_0000_0014_01dd`, i.e. pages {0,2,3,4,6,7,8,18,20} taken. Mapping a
/// single page at a fixed VA that is occupied fails with ENOSPC, which killed
/// render storage setup before any SKSM work could matter. Page 32 sits in the
/// long free run above page 20. This VA is ours to choose: the driver both maps
/// it and publishes it to the firmware (tiling descriptor +0x0945), so the two
/// stay consistent.
const G17P_BOOTSTRAP_TA_STATUS_VA: u64 = 0xffff_fc20_0168_0000;
const G17P_BOOTSTRAP_3D_STATUS_VA: u64 = 0xffff_fc20_0163_0000;
const G17P_BOOTSTRAP_CONTEXT_PEER_SIZE: usize = 0x20_000;
/// QIDs 0 and 1 are not ordinary application queues on G17P. Metal's type-1
/// 3D descriptor retains a distinct QID-0 RCE coordinate even while its
/// actual TA/3D queues use allocator-issued IDs. Keep the application's three
/// physical queues together at the first proven ordinary QID: compute is 4,
/// TA is 5, and 3D is 6.
pub(crate) const G17P_RENDER_TA_QUEUE_ID: u16 = 5;
pub(crate) const G17P_RENDER_3D_QUEUE_ID: u16 = 6;
const G17P_RENDER_QUEUE_LOW_BASE: u64 = 0x70_0043_8000;
const G17P_RENDER_QUEUE_HIGH_BASE: u64 = 0xffff_fc20_001d_8000;
const G17P_RENDER_QUEUE_VA_STRIDE: u64 = 0x28_000;
const G17P_BOOTSTRAP_TA_CONTEXT_LOW_VA: u64 =
    G17P_RENDER_QUEUE_LOW_BASE + G17P_RENDER_TA_QUEUE_ID as u64 * G17P_RENDER_QUEUE_VA_STRIDE;
const G17P_BOOTSTRAP_TA_CONTEXT_HIGH_VA: u64 =
    G17P_RENDER_QUEUE_HIGH_BASE + G17P_RENDER_TA_QUEUE_ID as u64 * G17P_RENDER_QUEUE_VA_STRIDE;
const G17P_BOOTSTRAP_3D_CONTEXT_LOW_VA: u64 =
    G17P_RENDER_QUEUE_LOW_BASE + G17P_RENDER_3D_QUEUE_ID as u64 * G17P_RENDER_QUEUE_VA_STRIDE;
const G17P_BOOTSTRAP_3D_CONTEXT_HIGH_VA: u64 =
    G17P_RENDER_QUEUE_HIGH_BASE + G17P_RENDER_3D_QUEUE_ID as u64 * G17P_RENDER_QUEUE_VA_STRIDE;

fn g17p_control_operand_buffer_va(index: usize) -> Option<u64> {
    if index >= G17P_CONTROL_OPERAND_BUFFER_COUNT {
        return None;
    }
    G17P_CONTROL_OPERAND_BUFFER_BASE
        .checked_add(index as u64 * G17P_CONTROL_OPERAND_BUFFER_STRIDE as u64)
}

fn encode_g17p_flist_runs(out: &mut [u8], block_count: usize) -> bool {
    if out.len() != G17P_CONTROL_OPERAND_TABLE_BACKING_SIZE {
        return false;
    }

    out.fill(0);
    for buffer_index in 0..block_count {
        let Some(buffer_base) = g17p_control_operand_buffer_va(buffer_index) else {
            return false;
        };
        // Kext: `(run_count + cursor + 7) & ~7` after every block. A 1-MiB
        // block is one 0x100-page run, so valid descriptors have 0x40 stride.
        let offset = buffer_index * G17P_CONTROL_OPERAND_RUN_SLOT_SIZE;
        let Some(slot) = out.get_mut(offset..offset + core::mem::size_of::<u64>()) else {
            return false;
        };
        let run = (buffer_base & G17P_CONTROL_OPERAND_RUN_ADDRESS_MASK)
            | (G17P_CONTROL_OPERAND_RUN_LENGTH_PAGES << 52);
        slot.copy_from_slice(&run.to_le_bytes());
    }
    true
}

fn encode_g17p_compute_flist_runs(out: &mut [u8]) -> bool {
    if out.len() != G17P_CONTROL_OPERAND_TABLE_BACKING_SIZE {
        return false;
    }
    out.fill(0);
    for buffer_index in 0..g17_initdata::COMPUTE_READINESS_OPERAND_ENTRY_COUNT {
        let Some(buffer_base) = g17p_control_operand_buffer_va(buffer_index) else {
            return false;
        };
        let offset = buffer_index * core::mem::size_of::<u64>();
        let Some(slot) = out.get_mut(offset..offset + core::mem::size_of::<u64>()) else {
            return false;
        };
        let run = (buffer_base & G17P_CONTROL_OPERAND_RUN_ADDRESS_MASK)
            | (G17P_CONTROL_OPERAND_RUN_LENGTH_PAGES << 52);
        slot.copy_from_slice(&run.to_le_bytes());
    }
    true
}

fn encode_g17p_pre_qid_flist_runs(out: &mut [u8]) -> bool {
    encode_g17p_flist_runs(out, G17P_CONTROL_OPERAND_ACTIVE_BLOCK_COUNT)
}

/// Encode the page-address array consumed by the initial UMA descriptor.
/// Native prepares this before `halLoadUMADesc`: 22 initial one-MiB blocks,
/// expanded into 256 4-KiB page addresses each. The four blocks described by
/// the opcode-0x20 grow are deliberately absent; firmware expands their run
/// descriptors into this list when it consumes the grow request.
fn encode_g17p_initial_flist_page_list(out: &mut [u8]) -> bool {
    if out.len() != G17P_CONTROL_OPERAND_PAGE_LIST_SIZE {
        return false;
    }

    out.fill(0);
    let mut entry = 0usize;
    for buffer_index in 0..G17P_CONTROL_OPERAND_INITIAL_BLOCK_COUNT {
        let Some(buffer_base) = g17p_control_operand_buffer_va(buffer_index) else {
            return false;
        };
        for page_index in 0..G17P_CONTROL_OPERAND_RUN_LENGTH_PAGES as usize {
            let Some(address) = buffer_base.checked_add((page_index * 0x1000) as u64) else {
                return false;
            };
            let offset = entry * core::mem::size_of::<u64>();
            let Some(slot) = out.get_mut(offset..offset + core::mem::size_of::<u64>()) else {
                return false;
            };
            slot.copy_from_slice(&address.to_le_bytes());
            entry += 1;
        }
    }
    true
}

fn g17p_control_operand_table_prefix(bytes: &mut [u8]) -> Option<&mut [u8]> {
    bytes.get_mut(..g17_initdata::CONTROL_OPERAND_TABLE_SIZE)
}

fn all_ranges_covered(
    ranges: impl IntoIterator<Item = (u64, u64)>,
    mut covers: impl FnMut(u64, u64) -> bool,
) -> bool {
    ranges
        .into_iter()
        .all(|(address, size)| covers(address, size))
}

#[cfg(not(test))]
fn map_compute_readiness_mappings(
    dev: &AsahiDevice,
    table: &mut RenderBackingObject,
    buffers: &mut KVec<RenderBackingObject>,
    vm: &mmu::Vm,
) -> Result<G17PComputeReadinessMappings> {
    if buffers.len() != G17P_CONTROL_OPERAND_BUFFER_COUNT {
        return Err(EINVAL);
    }
    let table = table
        .map_at(
            vm,
            G17P_CONTROL_OPERAND_TABLE_VA,
            mmu::PROT_GPU_FW_SHARED_RW,
        )
        .inspect_err(|error| {
            dev_err!(
                dev.as_ref(),
                "G17P compute readiness: operand table map at {:#x}:{:#x} failed ({:?})\n",
                G17P_CONTROL_OPERAND_TABLE_VA,
                g17_initdata::CONTROL_OPERAND_TABLE_SIZE,
                error,
            );
        })?;
    let mut mappings = KVec::with_capacity(
        g17_initdata::COMPUTE_READINESS_OPERAND_ENTRY_COUNT,
        GFP_KERNEL,
    )?;
    for (index, buffer) in buffers
        .iter_mut()
        .take(g17_initdata::COMPUTE_READINESS_OPERAND_ENTRY_COUNT)
        .enumerate()
    {
        let address = g17p_control_operand_buffer_va(index).ok_or(EINVAL)?;
        let mapping = buffer
            .map_at(vm, address, mmu::PROT_GPU_FW_SHARED_RW)
            .inspect_err(|error| {
                dev_err!(
                    dev.as_ref(),
                    "G17P compute readiness: operand buffer {} map at {:#x}:{:#x} failed ({:?})\n",
                    index,
                    address,
                    G17P_CONTROL_OPERAND_BUFFER_SIZE,
                    error,
                );
            })?;
        mappings.push(mapping, GFP_KERNEL)?;
    }
    let result = G17PComputeReadinessMappings {
        table,
        buffers: mappings,
    };
    if !result.all_reachable_from(vm) {
        return Err(EFAULT);
    }
    Ok(result)
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) struct G17PComputeUserVaLayout {
    pub(crate) descriptors: u64,
    pub(crate) preempt: u64,
    pub(crate) robustness: u64,
    pub(crate) operand_state: u64,
    pub(crate) queue_context: u64,
}

impl G17PComputeUserVaLayout {
    pub(crate) fn all_referenced_ranges_reachable(
        self,
        mut covers: impl FnMut(u64, u64) -> bool,
    ) -> bool {
        [
            (
                self.descriptors,
                G17P_COMPUTE_DESCRIPTOR_STORAGE_SIZE as u64,
            ),
            (self.preempt, G17P_COMPUTE_PREEMPT_SIZE as u64),
            (self.robustness, G17P_PAGE_SIZE as u64),
            (
                self.operand_state,
                G17P_COMPUTE_OPERAND_STATE_SIZE as u64,
            ),
            (
                self.queue_context,
                g17_compute::COMPUTE_QUEUE_CONTEXT_EXTENT as u64,
            ),
        ]
        .into_iter()
        .all(|(address, size)| covers(address, size))
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
struct G17PRenderStorageVaLayout {
    descriptors: u64,
    graph: u64,
    support: u64,
    render_state: u64,
    render_state_size: u64,
    timestamps: u64,
}

impl G17PRenderStorageVaLayout {
    fn all_referenced_ranges_reachable(
        self,
        mut firmware_covers: impl FnMut(u64, u64) -> bool,
        mut client_covers: impl FnMut(u64, u64) -> bool,
    ) -> bool {
        [
            (self.descriptors, G17P_RENDER_DESCRIPTOR_STORAGE_SIZE as u64),
            (self.graph, G17P_RENDER_QUEUE_GRAPH_SIZE as u64),
            (self.support, G17P_RENDER_SUPPORT_SIZE as u64),
            (self.timestamps, G17P_PAGE_SIZE as u64),
        ]
        .into_iter()
        .all(|(address, size)| firmware_covers(address, size))
            && [(self.render_state, self.render_state_size)]
            .into_iter()
            .all(|(address, size)| client_covers(address, size))
    }
}

const G17P_RENDER_STATE_DEFLAKE: usize = 0x04000;
const G17P_RENDER_STATE_TA_STATUS: usize = 0x08000;
const G17P_RENDER_STATE_FRAGMENT_STATUS: usize = 0x0c000;
const G17P_RENDER_STATE_DYNAMIC: usize = 0x10000;
const G17P_RENDER_STATE_AUX_FB_SIZE: usize = 0x8000;
const G17P_RENDER_AUX_FB_PAGE_COUNT: u64 = 0x10_0000;
const G17P_RENDER_DBIAS_IS_INT: u32 = 1 << 18;
const G17P_RENDER_QUEUE_RECORD_SIZE: usize = 0xc0;
const G17P_RENDER_POINTER_BLOCK_SIZE: usize = 0x80;
const G17P_USER_TILING_POINTERS: usize = 0x1000;
const G17P_USER_TILING_OPTIONAL: usize = 0x3000;
const G17P_USER_TILING_EVENT: usize = 0x3200;
/// Tag-16 (`selector 0x10`) entry-signal records, one per render queue.
///
/// Free graph space: TILING_EVENT ends at 0x3240 and FRAGMENT_EVENT at 0x3640,
/// with nothing else allocated before TILING_RING at 0x4000. Each record is
/// `G17P_COMPUTE_OPTIONAL_EVENT_SIZE` (0x12) bytes.
const G17P_USER_TILING_ENTRY_SIGNAL: usize = 0x3800;
const G17P_USER_FRAGMENT_ENTRY_SIGNAL: usize = 0x3840;

const G17P_USER_TILING_RING: usize = 0x4000;

fn page_align(value: usize) -> Option<usize> {
    value
        .checked_add(G17P_PAGE_SIZE - 1)
        .map(|value| value & !(G17P_PAGE_SIZE - 1))
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
struct G17PRenderStateLayout {
    heapmeta: usize,
    heapmeta_size: usize,
    tpc: usize,
    tpc_size: usize,
    aux_fb: usize,
    tilemap: usize,
    tilemap_size: usize,
    /// Tiled Vertex Buffer -- the heap the tiling pass bins geometry into.
    ///
    /// The G17P render path has never had one: `buffer.rs` / `fw/buffer.rs`
    /// model exactly this ("a heap of 128K blocks split into 32K pages") and
    /// are used only by `queue/render.rs`, the legacy path. Every g17_* file
    /// has zero references to it. This region is the storage half.
    tvb: usize,
    tvb_size: usize,
    tvb_blocks: usize,
    /// Per-scene scratch consumed by native TA/3D register generation.
    pm_scene_scratch: usize,
    /// Dedicated 32 KiB discard page published through the HWPB state.
    pm_discard: usize,
    total_size: usize,
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
struct G17PParameterManagementLayout {
    hwpb_state: usize,
    page_list: usize,
    block_page_bases: usize,
    shared_control: usize,
    counter: usize,
    page_metrics: usize,
    scene_states: usize,
    total_size: usize,
}

fn g17p_pm_page_align(value: usize) -> Option<usize> {
    value
        .checked_add(G17P_PM_PAGE_SIZE - 1)
        .map(|value| value & !(G17P_PM_PAGE_SIZE - 1))
}

fn g17p_parameter_management_layout() -> Option<G17PParameterManagementLayout> {
    let hwpb_state = 0;
    let page_list = g17p_pm_page_align(G17P_PM_HWPB_STATE_SIZE)?;
    let block_page_bases =
        page_list.checked_add(g17p_pm_page_align(G17P_PM_PAGE_LIST_SIZE)?)?;
    let shared_control = block_page_bases
        .checked_add(g17p_pm_page_align(G17P_PM_BLOCK_PAGE_BASE_TABLE_SIZE)?)?;
    let counter =
        shared_control.checked_add(g17p_pm_page_align(G17P_PM_SHARED_CONTROL_SIZE)?)?;
    let page_metrics = counter.checked_add(g17p_pm_page_align(G17P_PM_COUNTER_SIZE)?)?;
    let scene_states =
        page_metrics.checked_add(g17p_pm_page_align(G17P_PM_PAGE_METRICS_SIZE)?)?;
    let total_size =
        scene_states.checked_add(g17p_pm_page_align(G17P_PM_SCENE_ALLOCATION_SIZE)?)?;
    Some(G17PParameterManagementLayout {
        hwpb_state,
        page_list,
        block_page_bases,
        shared_control,
        counter,
        page_metrics,
        scene_states,
        total_size,
    })
}

fn align_four(value: usize) -> Option<usize> {
    value.checked_add(3).map(|value| value & !3)
}

fn g17p_render_state_layout(
    parameters: &g17_render::G17pRenderParameters,
    num_clusters: u32,
) -> Option<G17PRenderStateLayout> {
    if parameters.width == 0
        || parameters.height == 0
        || parameters.layers == 0
        || num_clusters == 0
    {
        return None;
    }
    let utile_width = parameters.utile_width as usize;
    let utile_height = parameters.utile_height as usize;
    if utile_width == 0 || utile_height == 0 || 32 % utile_width != 0 || 32 % utile_height != 0 {
        return None;
    }
    let utiles_per_tile = (32 / utile_width).checked_mul(32 / utile_height)?;
    let tiles_x = (parameters.width as usize).checked_add(31)? / 32;
    let tiles_y = (parameters.height as usize).checked_add(31)? / 32;
    let tiles_per_mtile_x = align_four(tiles_x.checked_add(3)? / 4)?;
    let tiles_per_mtile_y = align_four(tiles_y.checked_add(3)? / 4)?;
    let tiles_per_mtile = tiles_per_mtile_x.checked_mul(tiles_per_mtile_y)?;
    let layers = parameters.layers as usize;
    let tilemap_size = align_four(
        5usize
            .checked_mul(tiles_per_mtile)?
            .checked_mul(utiles_per_tile)?,
    )?
    .checked_div(4)?
    .checked_mul(4)?
    .checked_mul(16)?
    .checked_mul(layers)?;
    let tpc_size = 8usize
        .checked_mul(utiles_per_tile)?
        .checked_mul(tiles_per_mtile)?
        .checked_div(4)?
        .checked_mul(4)?
        .checked_mul(16)?
        .checked_mul(layers)?
        .checked_mul(num_clusters as usize)?;
    let heapmeta_size = 0x200usize.checked_add(if layers > 1 { 0x100 } else { 0 })?;

    // TVB sizing, using the same formula the legacy path feeds to
    // `Buffer::ensure_blocks` (queue/render.rs:132): blocks scale with the tile
    // count, floored at 8, and multi-cluster parts need at least 7 + 2*layers.
    let mut tvb_blocks = {
        let tiles = tiles_x.checked_mul(tiles_y)?;
        let blocks = tiles.checked_add(127)? / 128;
        let blocks = blocks.checked_add(7)? & !7;
        if blocks == 0 {
            8
        } else {
            blocks
        }
    };
    if num_clusters > 1 {
        let floor = 7usize.checked_add(2usize.checked_mul(layers)?)?;
        if tvb_blocks < floor {
            tvb_blocks = floor;
        }
    }
    let tvb_size = tvb_blocks
        .checked_sub(1)?
        .checked_mul(G17P_RENDER_TVB_BLOCK_STRIDE)?
        .checked_add(G17P_RENDER_TVB_BLOCK_SIZE)?;

    // Native's one RT-memory allocation places the tilemap first and the
    // heap metadata at the next 4-KiB subrange. For the launchgate triangle
    // that is exactly TMAP+0x0000 and HMTA+0x1000 inside one 16-KiB UAT page.
    // The previous inherited layout reversed them and separated them by
    // 0x10000, even though the 3D fixed-function path consumes their native
    // relationship directly.
    let tilemap = G17P_RENDER_STATE_DYNAMIC;
    let tilemap_span = tilemap_size.checked_add(0xfff)? & !0xfff;
    let heapmeta = tilemap.checked_add(tilemap_span)?;
    let tpc = page_align(heapmeta.checked_add(heapmeta_size)?)?;
    let aux_fb = tpc.checked_add(page_align(tpc_size)?)?;
    let after_tilemap = tilemap.checked_add(page_align(tilemap_size)?)?;
    let after_aux_fb = aux_fb.checked_add(G17P_RENDER_STATE_AUX_FB_SIZE)?;
    let (pm_scene_scratch, pm_discard, tvb, total_size) = if g17p_render_tvb_enabled() {
        let scene_scratch = g17p_pm_page_align(after_aux_fb.max(after_tilemap))?;
        let discard = scene_scratch.checked_add(G17P_PM_SCENE_SCRATCH_SIZE)?;
        let tvb = discard.checked_add(G17P_PM_DISCARD_SIZE)?;
        // TVB backing lives in a separate compact-GART allocation. Keep the
        // offset as the end of the render-state object, but do not duplicate
        // the potentially hundreds-of-megabytes TVB inside that object.
        (scene_scratch, discard, tvb, tvb)
    } else {
        (after_aux_fb, after_aux_fb, after_aux_fb, after_aux_fb)
    };
    Some(G17PRenderStateLayout {
        tvb,
        tvb_size,
        tvb_blocks,
        pm_scene_scratch,
        pm_discard,
        heapmeta,
        heapmeta_size,
        tpc,
        tpc_size,
        aux_fb,
        tilemap,
        tilemap_size,
        total_size,
    })
}

fn positive_half_f32_bits(value: u32) -> Option<u32> {
    if value == 0 {
        return Some(0);
    }
    let highest = 31u32.checked_sub(value.leading_zeros())?;
    let significand = value.checked_shl(23u32.checked_sub(highest)?)?;
    Some(((highest + 126) << 23) | (significand & 0x7f_ffff))
}

fn g17p_aux_fb_flags(_command_flags: u32) -> u64 {
    // The working G17P Metal command writes 0xc001 both to register 0x15021
    // and to the duplicated descriptor field at +0x1f40, including the
    // depth/stencil-inactive triangle case.  Deriving bit 0 from the legacy
    // DRM_ASAHI_RENDER_DBIAS_IS_INT flag changed that native value to 0xc000
    // whenever userspace had no active depth-bias requirement.  Keep the
    // generation-specific native default until the G17P producer of this
    // whole field (rather than the older UAPI name) is fully recovered.
    0xc001
}

fn encode_g17p_render_runtime_state(
    raw: &mut [u8],
    render_layout: &G17PRenderStateLayout,
    width: u32,
    height: u32,
    status_flag: u32,
) -> Result {
    let aux_end = render_layout
        .aux_fb
        .checked_add(G17P_RENDER_STATE_AUX_FB_SIZE)
        .ok_or(EOVERFLOW)?;
    // G17P native allocates AXFB with `array_empty_tagged(0x8000, "AXFB")`.
    // It is an all-zero auxiliary framebuffer, not an inherited G13/G14
    // metadata record. Keep this explicit so retained backing reuse cannot
    // resurrect stale dIPP-followed words.
    raw.get_mut(render_layout.aux_fb..aux_end)
        .ok_or(ERANGE)?
        .fill(0);
    apply_g17p_viewport(
        width,
        height,
        raw.get_mut(G17P_RENDER_STATE_DEFLAKE..G17P_RENDER_STATE_DEFLAKE + mmu::UAT_PGSZ)
            .ok_or(ERANGE)?,
    )
    .ok_or(EINVAL)?;
    if status_flag != 0 {
        put_u32(raw, G17P_RENDER_STATE_FRAGMENT_STATUS, 1);
        if status_flag > 1 {
            put_u32(raw, G17P_RENDER_STATE_TA_STATUS, 1);
        }
    }
    Ok(())
}

fn apply_g17p_viewport(width: u32, height: u32, page: &mut [u8]) -> Option<()> {
    if page.len() != G17P_PAGE_SIZE || width == 0 || height == 0 {
        return None;
    }
    let tiles_x = width.checked_add(31)? / 32;
    let tiles_y = height.checked_add(31)? / 32;
    put_u32(page, 0x900, 0x0000_0c00);
    put_u32(page, 0x904, 0x8000_0000 | (tiles_x - 1));
    put_u32(page, 0x908, tiles_y - 1);
    let half_width = positive_half_f32_bits(width)?;
    let half_height = positive_half_f32_bits(height)?;
    put_u32(page, 0x910, half_width);
    put_u32(page, 0x914, half_width);
    put_u32(page, 0x918, half_height);
    put_u32(page, 0x91c, half_height | 0x8000_0000);
    put_u32(page, 0x924, 0x3f80_0000);
    Some(())
}

fn put_u32(raw: &mut [u8], offset: usize, value: u32) {
    raw[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

#[cfg(test)]
fn put_u16(raw: &mut [u8], offset: usize, value: u16) {
    raw[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
}

fn put_u64(raw: &mut [u8], offset: usize, value: u64) {
    raw[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}

/// Apply one native AGXChannel command-pointer transition to the retained
/// compute graph. The command bytes are prepared separately; this performs
/// only pointer store, inner-shareable ordering, and producer publication.
fn apply_g17p_initial_command_pointer(
    graph: &mut [u8],
    expected_producer: u32,
    command_gpu_va: u64,
) -> Option<u32> {
    let state = g17_compute::COMPUTE_QUEUE_POINTERS;
    let producer_offset = state.checked_add(0x40)?;
    let count_offset = state.checked_add(0x60)?;
    let consumer = u32::from_le_bytes(graph.get(state..state + 4)?.try_into().ok()?);
    let producer = u32::from_le_bytes(
        graph
            .get(producer_offset..producer_offset + 4)?
            .try_into()
            .ok()?,
    );
    let count = u32::from_le_bytes(
        graph
            .get(count_offset..count_offset + 4)?
            .try_into()
            .ok()?,
    );
    // Compare against the size actually programmed into the graph, not a
    // literal. Hard-coding `0x500` here is why `g17p_ring_limit` failed on the
    // FIRST submission rather than at a wrap: every publish was refused the
    // moment the ring was programmed to any other size.
    if producer != expected_producer || count != g17_compute::compute_item_ring_entries() {
        return None;
    }
    let next = producer.checked_add(1)? % count;
    if next == consumer {
        return None;
    }
    let slot = g17_compute::COMPUTE_ITEM_RING
        .checked_add(producer as usize * core::mem::size_of::<u64>())?;
    let ring_end = g17_compute::COMPUTE_ITEM_RING.checked_add(g17_compute::COMPUTE_ITEM_RING_SIZE)?;
    if slot.checked_add(8)? > ring_end
        || u64::from_le_bytes(graph.get(slot..slot + 8)?.try_into().ok()?) != 0
    {
        return None;
    }
    put_u64(graph, slot, command_gpu_va);
    core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
    put_u32(graph, producer_offset, next);
    Some(next)
}

/// Fill the nonzero generic-pair tail pointers used by the full 4/16
/// opening. Descriptor-local pointer fields are installed separately by
/// `apply_g17p_retained_descriptor_fields`; the 3D RCE mirrors are then
/// replaced with their app-GART program addresses.
fn apply_g17p_generic_descriptor_tail(
    tiling: &mut [u8],
    fragment: &mut [u8],
) -> Option<()> {
    tiling.get(0x08a6..0x08b6)?;
    tiling.get(0x0945..0x094d)?;
    fragment.get(0x2140..0x2150)?;
    fragment.get(0x21df..0x21e7)?;

    put_u64(tiling, 0x08a6, G17P_BOOTSTRAP_DESCRIPTOR_ZERO_A_VA);
    put_u64(tiling, 0x08ae, G17P_BOOTSTRAP_DESCRIPTOR_ZERO_B_VA);
    put_u64(tiling, 0x0945, G17P_BOOTSTRAP_TA_STATUS_VA);
    // Native 3D points at +0x28 in both shared tail records.  The old +4
    // came from an inherited generic-pair guess and is contradicted by every
    // complete G17P fragment descriptor capture.
    put_u64(fragment, 0x2140, G17P_BOOTSTRAP_DESCRIPTOR_ZERO_A_VA + 0x28);
    put_u64(fragment, 0x2148, G17P_BOOTSTRAP_DESCRIPTOR_ZERO_B_VA + 0x28);
    put_u64(fragment, 0x21df, G17P_BOOTSTRAP_3D_STATUS_VA);
    Some(())
}

fn g17p_cold_opening_optional_addresses(
    support: u64,
    tiling_shared_object: u64,
    ta_hardware_buffer_id: u32,
    owner_pid: u32,
    tiling: bool,
) -> g17_submission::G17PColdOpeningOptionalAddresses {
    let (context_scratch, firmware_scratch) = if tiling {
        (G17P_BOOTSTRAP_TA_CONTEXT_LOW_VA, G17P_BOOTSTRAP_TA_CONTEXT_HIGH_VA)
    } else {
        (G17P_BOOTSTRAP_3D_CONTEXT_LOW_VA, G17P_BOOTSTRAP_3D_CONTEXT_HIGH_VA)
    };
    g17_submission::G17PColdOpeningOptionalAddresses {
        context_scratch,
        firmware_scratch,
        // Tag-15 +0x36 is the AGXUSCPrivMemFList firmware-state map. The fixed
        // cold-opening object has the exact native post-prepare layout; its
        // +0x08 hardware-buffer id is transitioned from -1 to slot zero by the
        // initdata encoder before this pointer becomes visible.
        usc_priv_mem_flist: g17_initdata::PARTIAL_OPENING_CONTROL_SHARED_ADDRESS,
        usc_freelist_hardware_buffer_id:
            g17_initdata::PARTIAL_OPENING_FREELIST_HARDWARE_BUFFER_ID,
        channel_control: support + G17P_RENDER_CHANNEL_CONTROL as u64,
        tiling_shared_object,
        owner_pid,
        ta_hardware_buffer_id,
    }
}

fn apply_g17p_add3_resource_table(raw: &mut [u8], buffers: [u64; 3]) -> Option<()> {
    let end = G17P_COMPUTE_ADD3_RESOURCE_TABLE
        .checked_add(buffers.len() * core::mem::size_of::<u64>())?;
    let table = raw.get_mut(G17P_COMPUTE_ADD3_RESOURCE_TABLE..end)?;
    for (slot, address) in buffers.into_iter().enumerate() {
        put_u64(table, slot * core::mem::size_of::<u64>(), address);
    }
    Some(())
}

fn g17p_render_can_reset_job_list(
    tiling: g17_completion::QueueIndices,
    fragment: g17_completion::QueueIndices,
    tiling_timestamps: [u64; 2],
    fragment_timestamps: [u64; 2],
) -> bool {
    tiling.idle()
        && fragment.idle()
        && tiling_timestamps.iter().all(|value| *value != 0)
        && fragment_timestamps.iter().all(|value| *value != 0)
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) enum G17PDeferredOuterPublication {
    None,
    Fragment,
    Tiling,
}

fn g17p_deferred_outer_plan_ready(
    plan: &g17_submission::G17PPartialOpeningOuterSlotPlan,
    channel_table_index: u8,
    current_producer: u32,
) -> bool {
    plan.channel_table_index == channel_table_index
        && current_producer == u32::from(plan.slot_index)
        && plan.next_producer == (current_producer + 1) & 0xff
}

#[cfg(test)]
fn build_g17p_user_queue_record(
    raw: &mut [u8],
    offset: usize,
    pointers: u64,
    item_ring: u64,
    job_list: u64,
    context: u64,
) {
    let body = &mut raw[offset..offset + G17P_RENDER_QUEUE_RECORD_SIZE];
    body.fill(0);
    put_u64(body, 0x00, pointers);
    put_u64(body, 0x08, item_ring);
    put_u64(body, 0x10, job_list);
    put_u32(body, 0x24, u32::MAX);
    put_u32(body, 0x28, 2);
    put_u32(body, 0x2c, 2);
    body[0x36..0x38].fill(0xff);
    put_u32(body, 0x38, 0);
    put_u32(body, 0x3c, 2);
    put_u32(body, 0x40, u32::MAX);
    put_u32(body, 0x44, 0x15);
    put_u64(body, 0x70, context);
}

#[cfg(test)]
fn build_g17p_user_pointer_block(raw: &mut [u8], offset: usize) {
    let body = &mut raw[offset..offset + G17P_RENDER_POINTER_BLOCK_SIZE];
    body.fill(0);
    put_u32(body, 0x50, u32::MAX);
    put_u32(body, 0x60, 0x500);
}

#[cfg(test)]
fn build_g17p_user_optional(
    raw: &mut [u8],
    offset: usize,
    tiling: bool,
    support: u64,
    shared: u64,
) {
    let body = &mut raw[offset..offset + 0xc0];
    body.fill(0);
    put_u32(body, 0x00, 0x0f);
    put_u64(body, 0x08, support);
    put_u64(body, 0x10, support + 0x400);
    put_u64(body, 0x36, support + 0x800);
    put_u64(body, 0x4a, support + 0xc00);
    put_u16(body, 0x1a, 1);
    put_u16(body, 0x1e, 2);
    put_u16(body, 0x26, 1);
    put_u16(body, 0x2a, 0);
    put_u16(body, 0x2e, 0);
    put_u16(body, 0x32, 1);
    put_u16(body, 0x3e, 0);
    put_u16(body, 0x46, 0);
    put_u16(body, 0x52, 1);
    put_u16(body, 0x56, 0);
    put_u16(body, 0x5a, 0x15);
    put_u16(body, 0x5e, 2);
    put_u16(body, 0x62, 1);
    put_u16(body, 0x66, 1);
    if tiling {
        put_u64(body, 0x6e, shared);
        put_u16(body, 0x76, 0);
        put_u16(body, 0x7e, 0);
        put_u16(body, 0x82, 1);
    } else {
        put_u16(body, 0x18, 1);
        put_u16(body, 0x22, 1);
        body[0x76..0x86].fill(0xff);
    }
}

#[cfg(test)]
fn g17p_cold_opening_metadata(
    kind: g17_render::RenderDescriptorKind,
) -> g17_render::RenderDescriptorMetadata {
    g17_render::RenderDescriptorMetadata {
        context_id: 1,
        submission_ordinal: 0,
        ta_hardware_buffer_id: 0,
        submit_sequence: match kind {
            g17_render::RenderDescriptorKind::Tiling => 1,
            g17_render::RenderDescriptorKind::Fragment => 0,
        },
    }
}
// Native Status A starts at private-cluster +0xe3100 and stays backed until
// the cluster end at +0x198000: 0xb4f00 bytes, or 0xb8000 page-rounded when
// represented as a separate Linux object. GFX1 writes through +0xb4004.
const STATUS_A_ALLOC_SIZE: usize = 0xb8000;
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
pub(crate) struct G17PControlCounters {
    pub(crate) primary: [u32; g17_initdata::CHANNEL_ENTRY_STATE_COUNT],
    pub(crate) secondary: [u32; g17_initdata::CHANNEL_ENTRY_STATE_COUNT],
}

/// Raw primary main-config channel-12 pointer chain and the values reached by
/// those pointers. Pinned J700 GFX A000 type-4 thunk `0x2635c` uses
/// `main+0x1b0` as its producer source; drain `0x3a74` uses all three.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) struct G17PControlPointerSnapshot {
    pub(crate) main_config: u64,
    pub(crate) raw_state_pointers: [u64; g17_initdata::CHANNEL_ENTRY_STATE_COUNT],
    pub(crate) expected_state_pointers: [u64; g17_initdata::CHANNEL_ENTRY_STATE_COUNT],
    pub(crate) state_values: [u32; g17_initdata::CHANNEL_ENTRY_STATE_COUNT],
}

/// Cache-safe Status-A state written through the primary WC/firmware-uncached
/// state grid. Exact J700 A000 writes the full work-scan flag at Status-A
/// `+0x0c`, then exposes `active` at `+0x10` and `scheduler_constructed` in
/// the adjacent high word.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) struct G17PStatusASnapshot {
    pub(crate) scan_address: u64,
    pub(crate) scan_active: u32,
    pub(crate) address: u64,
    pub(crate) raw: u64,
    pub(crate) active: u32,
    pub(crate) scheduler_constructed: u32,
}

/// Cache-safe primary Status-B firmware-recovery handshake state.
///
/// Exact J700 GFX A000 writes state 1 and waits for 2, then writes 3 and waits
/// for 0. The epoch increments between the first transition and its wait.
/// Status-A `+0x1c` increments only after a completed power callback, so it
/// distinguishes a retained lifecycle from an intervening transition.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) struct G17PFirmwareRecoveryHandshakeSnapshot {
    pub(crate) status_b: u64,
    pub(crate) epoch_address: u64,
    pub(crate) epoch: u64,
    pub(crate) state_address: u64,
    pub(crate) state: u32,
    pub(crate) power_callback_count_address: u64,
    pub(crate) power_callback_count: u32,
}

/// Offset of the pointer the firmware follows to its fault report. It is
/// bundle view 3, so the host already owns the target memory. The firmware
/// caches `main_config` at its own global `0xfffffc0000177030` (copied from
/// initdata root `+0x18`) and reaches the report as
/// `add x24, mc, #0x254 ; ldr x22, [x24, #24]` (b1 `0x5474-0x5480`).
const MAIN_CONFIG_FAULT_REPORT_POINTER: u64 = 0x26c;
const FAULT_REPORT_VIEW_INDEX: usize = 3;
/// Recovery state, `{0 running, 1 halted, 2 recovery in progress}`. The only
/// value the firmware ever tests is 2 (b1: 52 of 53 read sites are `cmp #2`).
const FAULT_REPORT_RECOVERY_STATE: u64 = 0x00;
/// Set when *the host* asked for the recovery; the firmware then wipes the
/// blame fields before the handshake, so an empty report is correct here.
const FAULT_REPORT_HOST_REQUESTED: u64 = 0x04;
/// The blame fields are packed `{u32 present, u32 value}` pairs. The consumer
/// at b1 `0xe8b0` proves the naming: it compares `report+0x38` against
/// `0xfffffc0000127628 + qid*0x28 + 0x20` (the per-queue data-master index)
/// and `report+0x28` against the queue id.
const FAULT_REPORT_BLAMED_QID_PRESENT: u64 = 0x24;
const FAULT_REPORT_BLAMED_QID: u64 = 0x28;
/// KSM slot index. Written only on the kick-timeout path (b1 `0x26e80`); the
/// fault-scan path explicitly zeroes it (`0x54a0`), so 0 here is normal.
const FAULT_REPORT_BLAMED_SLOT_PRESENT: u64 = 0x2c;
const FAULT_REPORT_BLAMED_SLOT: u64 = 0x30;
/// That queue's data master (2 == compute on G17P).
const FAULT_REPORT_DATA_MASTER_PRESENT: u64 = 0x34;
const FAULT_REPORT_DATA_MASTER: u64 = 0x38;
/// Number of fault sources the firmware detected this pass.
const FAULT_REPORT_SOURCE_COUNT: u64 = 0x3c;
/// Fault reason, `{2, 3, 4}`; 3 is the GMMU page fault (b1 `0x5f1c`).
const FAULT_REPORT_REASON: u64 = 0x4c;

/// The firmware's own account of a recovery: which queue it blamed, that
/// queue's data master, the reason, and the recovery state it left behind.
///
/// Every field is a 32-bit word; reading them as u64 would splice in the
/// neighbouring presence tag or counter. The record is complete before the
/// firmware writes 1 into `status_b[0x4900]`, stays valid across the host's
/// `1 -> 2` ack, and `state` is reset to 0 immediately after that ack.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) struct G17PFirmwareFaultReportSnapshot {
    pub(crate) report: u64,
    pub(crate) state: u32,
    pub(crate) host_requested: u32,
    pub(crate) qid_present: u32,
    pub(crate) qid: u32,
    pub(crate) slot_present: u32,
    pub(crate) slot: u32,
    pub(crate) data_master_present: u32,
    pub(crate) data_master: u32,
    pub(crate) source_count: u32,
    pub(crate) reason: u32,
}

/// One record consumed from the primary role's native 0x48-byte firmware
/// event ring. The indices are retained so the host publication can be
/// checked against the same snapshot that selected the entry.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) struct G17PPrimaryFirmwareEventRecord {
    pub(crate) raw: [u8; g17_completion::G17P_FIRMWARE_EVENT_ENTRY_SIZE],
    pub(crate) consumer_before: u32,
    pub(crate) producer_snapshot: u32,
    pub(crate) consumer_published: u32,
}

/// Outcome of one pass over an instance's firmware *event* ring
/// (channel-table entry 13, `status_a + 0x2c0`).
#[derive(Debug, Copy, Clone, Default, PartialEq, Eq)]
pub(crate) struct G17PReportDrainStats {
    /// Firmware-written write pointer, `status_a + 0x60`.
    pub(crate) producer: u32,
    /// Host-written read pointer as found, `status_a + 0x40`.
    pub(crate) consumer_before: u32,
    /// Firmware-*log* ring-0 write pointer, `status_a + 0xa0`. Carried only so
    /// the log line shows it next to the event producer: `status_a + 0x80` is
    /// the log control block `dump_firmware_log` walks, not the event state,
    /// and it is non-zero on every boot for unrelated reasons.
    pub(crate) fwlog_write: u32,
    /// Records read out of the ring this pass.
    pub(crate) consumed: u32,
    /// Records written to the log.
    pub(crate) printed: u32,
    /// Slots whose event type the decoder rejected.
    pub(crate) skipped: u32,
    /// Count of type-7 `GrowTilingBuffer` requests seen.
    pub(crate) grow_requests: u32,
    /// Count of type-4 `kAGFIFirmwareEventTypeGPURestart` records seen: the
    /// firmware halted and is asking the host to run the restart handshake.
    pub(crate) restart_requests: u32,
    /// Set when a cursor was outside `G17P_EVENT_CAPACITY`.
    pub(crate) cursor_out_of_range: bool,
}

/// Outcome of one pass over an instance's KTrace ring.
#[derive(Debug, Copy, Clone, Default, PartialEq, Eq)]
pub(crate) struct G17PKtraceDrainStats {
    /// Records copied out of the ring and acknowledged.
    pub(crate) consumed: u32,
    /// Records actually written to the log.
    pub(crate) printed: u32,
    /// Slots whose message type was not 5 (stale/never-written), skipped.
    pub(crate) skipped: u32,
    /// Total records the firmware dropped (sum of the 0x11c marker's arg0).
    pub(crate) lost_records: u64,
    /// Number of overflow windows (count of 0x11c markers).
    pub(crate) lost_windows: u32,
    /// Set once a channel-0 code 0x41 (tag-14 AddKicks) has been seen.
    pub(crate) saw_add_kicks: bool,
    /// Last non-zero pause-reason mask observed in a channel-0 code 0x46.
    pub(crate) last_pause_mask: u64,
    pub(crate) producer: u32,
    pub(crate) consumer_before: u32,
}

/// Coherent raw cause fields published by A000 before it notifies the host of
/// a type-4 firmware recovery event.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) struct G17PFirmwareRecoveryCauseSnapshot {
    pub(crate) zero_0: u32,
    pub(crate) detected_source_count: u32,
    pub(crate) zero_1: u32,
    pub(crate) command_word: u32,
    pub(crate) raw_48cc: u32,
    pub(crate) raw_48d0: u64,
    pub(crate) fault_status: u64,
    pub(crate) host_recovery: u32,
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) struct G17PFirmwareRecoveryInfoSnapshot {
    pub(crate) emitted_entry_count: u32,
    pub(crate) valid_entry_count: u32,
    pub(crate) first_valid_index: Option<u32>,
    pub(crate) first_valid_flags: u32,
}

impl G17PFirmwareRecoveryHandshakeSnapshot {
    const fn decode(
        status_b: u64,
        epoch_address: u64,
        epoch: u64,
        state_address: u64,
        state: u32,
        power_callback_count_address: u64,
        power_callback_count: u32,
    ) -> Self {
        Self {
            status_b,
            epoch_address,
            epoch,
            state_address,
            state,
            power_callback_count_address,
            power_callback_count,
        }
    }

    pub(crate) const fn waiting_for_first_response(self) -> bool {
        self.state == 1
    }

    pub(crate) const fn waiting_for_final_clear(self) -> bool {
        self.state == 3
    }

    /// States 1, 2, and 3 all occur after the worker removes EP21 and before
    /// it reinstalls the callback. State zero alone cannot prove either side
    /// of that interval.
    pub(crate) const fn callback_known_unregistered(self) -> bool {
        self.state >= 1 && self.state <= 3
    }
}

impl G17PStatusASnapshot {
    const fn decode(scan_address: u64, scan_active: u32, address: u64, raw: u64) -> Self {
        Self {
            scan_address,
            scan_active,
            address,
            raw,
            active: raw as u32,
            scheduler_constructed: (raw >> 32) as u32,
        }
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) struct G17PControlOpeningEffect {
    pub(crate) shared_cursor: u32,
    pub(crate) inner_state: u32,
    pub(crate) operand_slot: u64,
}

/// What one live device-control publication moved.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) struct G17PDeviceControlPublication {
    pub(crate) opcode: u32,
    pub(crate) arg: u32,
    /// Ring slot the record landed in, i.e. `producer_before & 0xff`.
    pub(crate) slot: u32,
    pub(crate) record_address: u64,
    pub(crate) consumer_before: u32,
    pub(crate) producer_before: u32,
    pub(crate) producer_after: u32,
}

impl G17PControlCounters {
    pub(crate) fn opening_staged(self) -> bool {
        self.primary == [0, 0, g17_initdata::CONTROL_OPENING_PRIMARY_PRODUCER]
            && self.secondary
                == [0, 0, g17_initdata::CONTROL_OPENING_SECONDARY_PRODUCER]
    }

    pub(crate) fn opening_retired(self) -> bool {
        self.primary == [g17_initdata::CONTROL_OPENING_PRIMARY_PRODUCER; 3]
            && self.secondary == [g17_initdata::CONTROL_OPENING_SECONDARY_PRODUCER; 3]
    }

    pub(crate) fn compute_runtime_ready(self) -> bool {
        self.primary == [g17_initdata::COMPUTE_RUNTIME_CONTROL_FINAL_COUNTER; 3]
            && self.secondary == [g17_initdata::CONTROL_OPENING_SECONDARY_PRODUCER; 3]
    }

    pub(crate) fn compute_readiness_record_staged(self, target: u32) -> bool {
        let Some(previous) = target.checked_sub(1) else {
            return false;
        };
        self.primary == [previous, previous, target]
            && self.secondary
                == [g17_initdata::CONTROL_OPENING_SECONDARY_PRODUCER; 3]
    }

    pub(crate) fn compute_readiness_record_retired(self, target: u32) -> bool {
        self.primary == [target; 3]
            && self.secondary
                == [g17_initdata::CONTROL_OPENING_SECONDARY_PRODUCER; 3]
    }

    pub(crate) fn compute_readiness_retired(self) -> bool {
        self.primary == [g17_initdata::COMPUTE_READINESS_FINAL_COUNTER; 3]
            && self.secondary
                == [g17_initdata::CONTROL_OPENING_SECONDARY_PRODUCER; 3]
    }
}

/// Exact first-graphics objects retained by the T8140 resource owner.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) struct G17PPartialOpeningResourceBinding {
    pub(crate) kernel_va_base: u64,
    pub(crate) scheduler_page: u64,
    pub(crate) primary_index_page: u64,
    pub(crate) fwctl: u64,
    pub(crate) tiling_channel: g17_initdata::ChannelEntry,
    pub(crate) fragment_channel: g17_initdata::ChannelEntry,
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

/// Kernel-owned storage whose GPU mapping belongs only to a render VM.
///
/// The context-0 low root has unrelated B2 aliases inside the render operand
/// arena, so these objects must not acquire a context-0 mapping merely to keep
/// their backing alive.
#[cfg(not(test))]
struct RenderBackingObject {
    object: gem::ObjectRef,
    logical_size: usize,
}

#[cfg(not(test))]
impl RenderBackingObject {
    fn new(dev: &AsahiDevice, size: usize) -> Result<Self> {
        let mut object = gem::new_kernel_object_wc(dev, size)?;
        object.vmap()?.memset(0);
        Ok(Self {
            object,
            logical_size: size,
        })
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

    fn map_at(
        &mut self,
        vm: &mmu::Vm,
        address: u64,
        prot: mmu::Prot,
    ) -> Result<mmu::KernelMapping> {
        self.object.map_at(vm, address, prot, false)
    }
}

#[cfg(not(test))]
fn map_fixed_object(
    dev: &AsahiDevice,
    vm: &mmu::Vm,
    name: &str,
    address: u64,
    size: usize,
    prot: mmu::Prot,
) -> Result<MappedObject> {
    match MappedObject::new_at(dev, vm, address, size, prot) {
        Ok(mapping) => Ok(mapping),
        Err(error) => {
            dev_err!(
                dev.as_ref(),
                "G17P resources: {} map at {:#x} size {:#x} failed ({:?})\n",
                name,
                address,
                size,
                error
            );
            Err(error)
        }
    }
}

#[cfg(not(test))]
fn mapped_page_mask(vm: &mmu::Vm, address: u64, size: usize) -> u64 {
    let pages = size.div_ceil(mmu::UAT_PGSZ).min(64);
    let mut occupied = 0u64;
    for index in 0..pages {
        let page = address + index as u64 * mmu::UAT_PGSZ as u64;
        if vm.covers_range(page, mmu::UAT_PGSZ as u64, false, false) {
            occupied |= 1 << index;
        }
    }
    occupied
}

#[cfg(not(test))]
fn map_fixed_wc_object(
    dev: &AsahiDevice,
    vm: &mmu::Vm,
    name: &str,
    address: u64,
    size: usize,
    prot: mmu::Prot,
) -> Result<MappedObject> {
    match MappedObject::new_wc_at(dev, vm, address, size, prot) {
        Ok(mapping) => Ok(mapping),
        Err(error) => {
            let occupied = mapped_page_mask(vm, address, size);
            dev_err!(
                dev.as_ref(),
                "G17P resources: {} WC map at {:#x} size {:#x} failed ({:?}), occupied pages={:#x}\n",
                name,
                address,
                size,
                error,
                occupied
            );
            Err(error)
        }
    }
}

#[cfg(not(test))]
impl MappedObject {
    fn new_wc_in_range(
        dev: &AsahiDevice,
        vm: &mmu::Vm,
        range: Range<u64>,
        size: usize,
        alignment: u64,
        prot: mmu::Prot,
    ) -> Result<Self> {
        let mut object = gem::new_kernel_object_wc(dev, size)?;
        object.vmap()?.memset(0);
        let mapping = object.map_into_range(vm, range, alignment, prot, true)?;
        Ok(Self {
            mapping,
            object,
            logical_size: size,
        })
    }

    fn new_wc(
        dev: &AsahiDevice,
        uat: &mmu::Uat,
        size: usize,
        alignment: u64,
        prot: mmu::Prot,
    ) -> Result<Self> {
        Self::new_wc_in_range(
            dev,
            uat.kernel_vm(),
            uat.kernel_va_range()?,
            size,
            alignment,
            prot,
        )
    }

    fn new_wc_at(
        dev: &AsahiDevice,
        vm: &mmu::Vm,
        address: u64,
        size: usize,
        prot: mmu::Prot,
    ) -> Result<Self> {
        let mut object = gem::new_kernel_object_wc(dev, size)?;
        object.vmap()?.memset(0);
        let mapping = object.map_at(vm, address, prot, false)?;
        Ok(Self {
            mapping,
            object,
            logical_size: size,
        })
    }

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

    fn read_u32(&mut self, address: u64) -> Result<u32> {
        let offset = address.checked_sub(self.iova()).ok_or(EINVAL)? as usize;
        let end = offset.checked_add(4).ok_or(EINVAL)?;
        if end > self.logical_size {
            return Err(EINVAL);
        }
        self.with_bytes_mut(|bytes| {
            Ok(u32::from_le_bytes(
                bytes[offset..end].try_into().map_err(|_| EINVAL)?,
            ))
        })
    }

    fn read_u64(&mut self, address: u64) -> Result<u64> {
        let offset = address.checked_sub(self.iova()).ok_or(EINVAL)? as usize;
        let end = offset.checked_add(8).ok_or(EINVAL)?;
        if end > self.logical_size {
            return Err(EINVAL);
        }
        self.with_bytes_mut(|bytes| {
            Ok(u64::from_le_bytes(
                bytes[offset..end].try_into().map_err(|_| EINVAL)?,
            ))
        })
    }

    fn write_u32(&mut self, address: u64, value: u32) -> Result {
        let offset = address.checked_sub(self.iova()).ok_or(EINVAL)? as usize;
        let end = offset.checked_add(4).ok_or(EINVAL)?;
        if end > self.logical_size {
            return Err(EINVAL);
        }
        self.with_bytes_mut(|bytes| {
            bytes[offset..end].copy_from_slice(&value.to_le_bytes());
            Ok(())
        })
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
        .map_err(|_| EINVAL)?;

        let primary_ring = primary_offset + 0x4c0;
        let primary_ring_end = primary_ring
            .checked_add(g17_initdata::control_opening_size(InstanceRole::Primary))
            .ok_or(EINVAL)?;
        g17_initdata::encode_control_opening(
            InstanceRole::Primary,
            bytes
                .get_mut(primary_ring..primary_ring_end)
                .ok_or(EINVAL)?,
        )
        .map_err(|_| EINVAL)?;

        let secondary_ring = secondary_offset + 0x4c0;
        let secondary_ring_end = secondary_ring
            .checked_add(g17_initdata::control_opening_size(
                InstanceRole::Secondary,
            ))
            .ok_or(EINVAL)?;
        g17_initdata::encode_control_opening(
            InstanceRole::Secondary,
            bytes
                .get_mut(secondary_ring..secondary_ring_end)
                .ok_or(EINVAL)?,
        )
        .map_err(|_| EINVAL)
    }
}

#[cfg(not(test))]
fn map_fresh_zero_object_at(
    dev: &AsahiDevice,
    vm: &mmu::Vm,
    address: u64,
    size: usize,
) -> Result<mmu::KernelMapping> {
    // This page is part of the firmware-owned render operand graph. Keep the
    // CPU alias WC so initial zeroes and later firmware stores share one
    // coherent view.
    let mut object = gem::new_kernel_object_wc(dev, size)?;
    object.vmap()?.memset(0);
    object.map_at(vm, address, mmu::PROT_GPU_FW_SHARED_RW, false)
}

#[cfg(not(test))]
fn map_bootstrap_vdm_at(dev: &AsahiDevice, vm: &mmu::Vm) -> Result<mmu::KernelMapping> {
    let mut object = gem::new_kernel_object_wc(dev, G17P_BOOTSTRAP_VDM_SIZE)?;
    let mut vmap = object.vmap()?;
    vmap.memset(0);
    let bytes = unsafe {
        // SAFETY: the VMap covers the complete bootstrap VDM allocation.
        core::slice::from_raw_parts_mut(vmap.as_mut_ptr(), G17P_BOOTSTRAP_VDM_SIZE)
    };
    bytes[..core::mem::size_of::<u32>()]
        .copy_from_slice(&G17P_VDM_STREAM_TERMINATE.to_le_bytes());
    core::mem::drop(vmap);
    object.map_at(
        vm,
        G17P_BOOTSTRAP_VDM_VA,
        mmu::PROT_GPU_FW_SHARED_RW,
        false,
    )
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
fn read_control_counters(object: &mut MappedObject, state_grid: u64) -> Result<[u32; 3]> {
    let base = state_grid
        .checked_add(g17_initdata::WORK_STATE_GRID_OFFSETS
            [g17_initdata::CONTROL_CHANNEL_INDEX] as u64)
        .ok_or(EINVAL)?;
    let mut counters = [0u32; 3];
    for (index, counter) in counters.iter_mut().enumerate() {
        let address = base
            .checked_add(index as u64 * g17_initdata::CHANNEL_STATE_SPACING as u64)
            .ok_or(EINVAL)?;
        *counter = object.read_u32(address)?;
    }
    Ok(counters)
}

#[cfg(not(test))]
fn write_control_counters(object: &mut MappedObject, counters: [u32; 3]) -> Result {
    object.with_bytes_mut(|bytes| {
        let control = g17_initdata::WORK_STATE_GRID_OFFSETS
            [g17_initdata::CONTROL_CHANNEL_INDEX];
        for (index, counter) in counters.into_iter().enumerate() {
            let offset = control
                .checked_add(index * g17_initdata::CHANNEL_STATE_SPACING)
                .ok_or(EINVAL)?;
            let end = offset.checked_add(4).ok_or(EINVAL)?;
            bytes
                .get_mut(offset..end)
                .ok_or(EINVAL)?
                .copy_from_slice(&counter.to_le_bytes());
        }
        Ok(())
    })
}

#[cfg(not(test))]
fn write_control_counter(object: &mut MappedObject, index: usize, counter: u32) -> Result {
    if index >= g17_initdata::CHANNEL_ENTRY_STATE_COUNT {
        return Err(EINVAL);
    }
    object.with_bytes_mut(|bytes| {
        let offset = g17_initdata::WORK_STATE_GRID_OFFSETS
            [g17_initdata::CONTROL_CHANNEL_INDEX]
            .checked_add(index * g17_initdata::CHANNEL_STATE_SPACING)
            .ok_or(EINVAL)?;
        let end = offset.checked_add(4).ok_or(EINVAL)?;
        bytes
            .get_mut(offset..end)
            .ok_or(EINVAL)?
            .copy_from_slice(&counter.to_le_bytes());
        Ok(())
    })
}

fn register_window_pages(window: &g17_initdata::RegisterWindow) -> Option<(u64, u64, u64)> {
    let page_mask = G17P_PAGE_SIZE as u64 - 1;
    let phys_page = window.phys & !page_mask;
    let device_page = window.device_va & !page_mask;
    if window.phys & page_mask != window.device_va & page_mask {
        return None;
    }
    let end = window.device_va.checked_add(window.size as u64)?;
    let span = end
        .checked_sub(device_page)?
        .checked_add(page_mask)?
        & !page_mask;
    Some((phys_page, device_page, span))
}

#[cfg(not(test))]
fn map_t8140_register_windows(
    dev: &AsahiDevice,
    uat: &mmu::Uat,
) -> Result<KVec<mmu::KernelMapping>> {
    let mut mappings = KVec::new();
    for window in g17_initdata::T8140_REGISTER_WINDOWS {
        let (phys_page, device_page, span) = register_window_pages(&window).ok_or(EINVAL)?;
        let mapping = match uat.kernel_vm().map_io(
            device_page,
            phys_page.try_into()?,
            span.try_into()?,
            mmu::PROT_FW_MMIO_RW,
        ) {
            Ok(mapping) => mapping,
            Err(error) => {
                dev_err!(
                    dev.as_ref(),
                    "G17P resources: register window slot {} map failed ({:#x}:{:#x} -> {:#x}, {:?})\n",
                    window.slot,
                    phys_page,
                    span,
                    device_page,
                    error
                );
                return Err(error);
            }
        };
        if let Err(error) = mappings.push(mapping, GFP_KERNEL) {
            dev_err!(
                dev.as_ref(),
                "G17P resources: retaining register window slot {} failed ({:?})\n",
                window.slot,
                error
            );
            return Err(error.into());
        }
    }
    dev_info!(
        dev.as_ref(),
        "G17P resources: all {} register windows mapped\n",
        g17_initdata::T8140_REGISTER_WINDOWS.len()
    );
    Ok(mappings)
}

/// Queue-owned mappings retained from SKSM registration through completion.
#[cfg(not(test))]
pub(crate) struct G17PClRuntimeStorage {
    entries: MappedObject,
    /// Upper-root alias of the exact GEM subrange owned by `entries`.
    /// `KernelMapping` retains its own GEM reference for the full alias life.
    entries_high: mmu::KernelMapping,
    geometry: g17_submission::G17SksmQueueGeometry,
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
    queue_context_low: MappedObject,
    queue_context_high: MappedObject,
    /// Host-only cursor: item-ring slots below this (mod the ring count) have
    /// been zeroed since the firmware consumed them. Recycling is what makes
    /// submission depth unbounded -- see `recycle_consumed_ring_slots`.
    ring_reclaimed: u32,
    /// Slots reclaimed since boot. Counted rather than inferred from log lines,
    /// because a 400-deep run overflows the kernel ring buffer and makes any
    /// count taken by grepping dmesg an artefact of how much survived.
    ring_reclaimed_total: u64,
}

/// User-context aliases retained while one SKSM compute entry can still
/// execute. Firmware keeps the high descriptor and queue mappings; the GPU
/// consumes these aliases together with the userspace CDM and USC mappings.
#[cfg(not(test))]
pub(crate) struct G17PComputeUserMappings {
    descriptors: mmu::KernelMapping,
    preempt: mmu::KernelMapping,
    robustness: mmu::KernelMapping,
    operand_state: mmu::KernelMapping,
    queue_context: mmu::KernelMapping,
}

#[cfg(not(test))]
struct G17PComputeReadinessMappings {
    table: mmu::KernelMapping,
    buffers: KVec<mmu::KernelMapping>,
}

/// Queue-graph ring cursors and the first item-ring slots.
///
/// Every field is a plain read of DRAM the driver itself allocated. NOTHING
/// here touches sgx MMIO -- that distinction matters, because reading the
/// fault bank on a timeout path SErrors once the GPU cores have powered back
/// down, which is a hard panic rather than a diagnostic.
#[cfg(not(test))]
#[derive(Debug, Copy, Clone)]
pub(crate) struct G17PComputeRingState {
    pub(crate) queue: u64,
    /// Queue graph `+0x100`. `apply_g17p_initial_command_pointer` treats this
    /// as the firmware's consumer cursor; whether the firmware actually writes
    /// it is exactly what this diagnostic exists to settle.
    pub(crate) consumer: u32,
    /// Queue graph `+0x140`, the host's item-ring producer.
    pub(crate) producer: u32,
    /// Queue graph `+0x150`, initialised to `u32::MAX` by the graph builder.
    pub(crate) mirror: u32,
    /// Queue graph `+0x160`, the ring entry count (`0x500`).
    pub(crate) count: u32,
    pub(crate) items: [u64; 8],
    /// Host reclaim cursor, and slots reclaimed since boot.
    pub(crate) reclaimed: u32,
    pub(crate) reclaimed_total: u64,
}

#[cfg(not(test))]
#[cfg(not(test))]
#[derive(Debug, Copy, Clone)]
pub(crate) struct G17PComputeGraphSnapshot {
    pub(crate) queue: u64,
    pub(crate) pointers: u64,
    pub(crate) item_ring: u64,
    pub(crate) context_high: u64,
    pub(crate) cursors: [u32; 3],
    pub(crate) gpu_read_cursors: [u32; 3],
    pub(crate) uuid: u32,
    pub(crate) queue_context: u64,
    pub(crate) items: [u64; 3],
    pub(crate) event: [u32; 5],
    pub(crate) status: [u64; 2],
    pub(crate) descriptor: u64,
    pub(crate) descriptor_selector: u32,
    pub(crate) descriptor_context: u32,
    pub(crate) descriptor_grid: u32,
    pub(crate) descriptor_timestamps: [u64; 2],
    pub(crate) optional_contexts: [u64; 2],
    pub(crate) optional_grid: u16,
    pub(crate) optional_uuid: u16,
    pub(crate) optional_shared: u64,
    pub(crate) optional_channel: u64,
    pub(crate) context_record: [u64; 3],
}

#[cfg(not(test))]
#[derive(Debug, Copy, Clone)]
pub(crate) struct G17PComputeChannelSnapshot {
    pub(crate) table_index: u8,
    pub(crate) ring: u64,
    pub(crate) state_addresses: [u64; 3],
    pub(crate) cursors: [u32; 3],
    pub(crate) slot_index: u8,
    pub(crate) slot_queue: u64,
    pub(crate) slot_kind: u32,
    pub(crate) slot_flags: u32,
    pub(crate) runtime_descriptor_pointers: [u64; 2],
}

#[cfg(not(test))]
#[derive(Debug, Copy, Clone)]
pub(crate) struct G17PChannelScanSnapshot {
    pub(crate) table_index: u8,
    pub(crate) ring: u64,
    pub(crate) state_addresses: [u64; 3],
    pub(crate) cursors: [u32; 3],
    pub(crate) current_slot_index: u8,
    pub(crate) current_slot_queue: u64,
    pub(crate) current_slot_kind: u32,
    pub(crate) current_slot_queue_id: u8,
}

#[cfg(not(test))]
impl G17PComputeUserMappings {
    pub(crate) fn layout(&self) -> G17PComputeUserVaLayout {
        G17PComputeUserVaLayout {
            descriptors: self.descriptors.iova(),
            preempt: self.preempt.iova(),
            robustness: self.robustness.iova(),
            operand_state: self.operand_state.iova(),
            queue_context: self.queue_context.iova(),
        }
    }

    pub(crate) fn descriptor_gpu_va(&self, slot: u8) -> u64 {
        self.descriptors.iova()
            + slot as u64 * G17P_COMPUTE_DESCRIPTOR_SLOT_SIZE as u64
    }

    pub(crate) fn all_reachable_from(&self, vm: &mmu::Vm) -> bool {
        self.layout().all_referenced_ranges_reachable(|address, size| {
            vm.covers_range(address, size, true, true)
        })
    }
}

#[cfg(not(test))]
impl G17PComputeReadinessMappings {
    fn all_reachable_from(&self, vm: &mmu::Vm) -> bool {
        self.buffers.len() == g17_initdata::COMPUTE_READINESS_OPERAND_ENTRY_COUNT
            && self.table.iova() == G17P_CONTROL_OPERAND_TABLE_VA
            && vm.covers_range(
                self.table.iova(),
                self.table.size() as u64,
                true,
                true,
            )
            && self.buffers.iter().enumerate().all(|(index, mapping)| {
                g17p_control_operand_buffer_va(index).is_some_and(|address| {
                    mapping.iova() == address
                        && mapping.size() == G17P_CONTROL_OPERAND_BUFFER_SIZE
                        && vm.covers_range(address, mapping.size() as u64, true, true)
                })
            })
    }
}

/// One Linux-owned paired render graph retained through synchronous
/// completion. The queue records, pointer blocks, item-address rings, shared
/// job list, descriptors, optional records, events, and timestamp words all
/// live in these mappings.
#[cfg(not(test))]
struct G17PRenderUserAliases {
    deflake: mmu::KernelMapping,
    scene_scratch: Option<mmu::KernelMapping>,
    discard: Option<mmu::KernelMapping>,
    /// One compact Buffer/Scene view containing TMAP followed by HMTA.
    rt_memory: mmu::KernelMapping,
    /// Separate compact, firmware-writable Tail Pointer Cache view.
    tpc: mmu::KernelMapping,
    /// Queue-owned AXFB in the upper ordinary user-GART address class.
    aux_fb: mmu::KernelMapping,
}

#[cfg(not(test))]
impl G17PRenderUserAliases {
    const DEFLAKE_OFFSET: u64 = 0;
    const SCENE_SCRATCH_OFFSET: u64 = 0x8000;
    const DISCARD_OFFSET: u64 = 0x1_0000;
    const RT_MEMORY_OFFSET: u64 = 0x1_8000;
    const TPC_OFFSET: u64 = 0x5_0000;

    fn new(
        render_state: &mut MappedObject,
        vm: &mmu::Vm,
        render_layout: &G17PRenderStateLayout,
        submission_ordinal: u32,
        with_parameter_management: bool,
    ) -> Result<Self> {
        let slot_base = G17P_RENDER_USER_ALIAS_START
            + (submission_ordinal as u64 & 1) * G17P_RENDER_USER_ALIAS_SLOT_SIZE;
        let deflake_va = slot_base + Self::DEFLAKE_OFFSET;
        let deflake = render_state.object.map_range_into_range(
            vm,
            G17P_RENDER_STATE_DEFLAKE..G17P_RENDER_STATE_DEFLAKE + mmu::UAT_PGSZ,
            deflake_va..deflake_va + mmu::UAT_PGSZ as u64,
            mmu::UAT_PGSZ as u64,
            mmu::PROT_GPU_FW_SHARED_RW,
            false,
        )?;

        let (scene_scratch, discard) = if with_parameter_management {
            let scene_va = slot_base + Self::SCENE_SCRATCH_OFFSET;
            let scene_scratch = render_state.object.map_range_into_range(
                vm,
                render_layout.pm_scene_scratch
                    ..render_layout.pm_scene_scratch + G17P_PM_SCENE_SCRATCH_SIZE,
                scene_va..scene_va + G17P_PM_SCENE_SCRATCH_SIZE as u64,
                G17P_PM_PAGE_SIZE as u64,
                mmu::PROT_GPU_FW_SHARED_RW,
                false,
            )?;
            let discard_va = slot_base + Self::DISCARD_OFFSET;
            let discard = render_state.object.map_range_into_range(
                vm,
                render_layout.pm_discard..render_layout.pm_discard + G17P_PM_DISCARD_SIZE,
                discard_va..discard_va + G17P_PM_DISCARD_SIZE as u64,
                G17P_PM_PAGE_SIZE as u64,
                mmu::PROT_GPU_FW_SHARED_RW,
                false,
            )?;
            (Some(scene_scratch), Some(discard))
        } else {
            (None, None)
        };

        let rt_source_start = render_layout.tilemap;
        let rt_source_end = page_align(
            render_layout
                .heapmeta
                .checked_add(render_layout.heapmeta_size)
                .ok_or(EOVERFLOW)?,
        )
        .ok_or(EOVERFLOW)?;
        let rt_size = rt_source_end
            .checked_sub(rt_source_start)
            .ok_or(EINVAL)?;
        if Self::RT_MEMORY_OFFSET
            .checked_add(rt_size as u64)
            .ok_or(EOVERFLOW)?
            > Self::TPC_OFFSET
        {
            return Err(ERANGE);
        }
        let rt_va = slot_base + Self::RT_MEMORY_OFFSET;
        let rt_memory = render_state.object.map_range_into_range(
            vm,
            rt_source_start..rt_source_end,
            rt_va..rt_va + rt_size as u64,
            mmu::UAT_PGSZ as u64,
            mmu::PROT_GPU_FW_SHARED_RW,
            false,
        )?;

        let tpc_size = page_align(render_layout.tpc_size).ok_or(EOVERFLOW)?;
        if Self::TPC_OFFSET
            .checked_add(tpc_size as u64)
            .ok_or(EOVERFLOW)?
            > G17P_RENDER_USER_ALIAS_SLOT_SIZE
        {
            return Err(ERANGE);
        }
        let tpc_source_end = render_layout
            .tpc
            .checked_add(tpc_size)
            .ok_or(EOVERFLOW)?;
        let tpc_va = slot_base + Self::TPC_OFFSET;
        let tpc = render_state.object.map_range_into_range(
            vm,
            render_layout.tpc..tpc_source_end,
            tpc_va..tpc_va + tpc_size as u64,
            mmu::UAT_PGSZ as u64,
            mmu::PROT_GPU_FW_SHARED_RW,
            false,
        )?;

        let aux_va = G17P_RENDER_AUX_ALIAS_START
            + (submission_ordinal as u64 & 1) * G17P_RENDER_AUX_ALIAS_SLOT_SIZE;
        let aux_fb = render_state.object.map_range_into_range(
            vm,
            render_layout.aux_fb
                ..render_layout.aux_fb + G17P_RENDER_STATE_AUX_FB_SIZE,
            aux_va..aux_va + G17P_RENDER_STATE_AUX_FB_SIZE as u64,
            mmu::UAT_PGSZ as u64,
            mmu::PROT_GPU_FW_SHARED_RW,
            false,
        )?;

        Ok(Self {
            deflake,
            scene_scratch,
            discard,
            rt_memory,
            tpc,
            aux_fb,
        })
    }

    fn deflake_va(&self) -> u64 {
        self.deflake.iova()
    }

    fn scene_scratch_va(&self) -> Option<u64> {
        self.scene_scratch.as_ref().map(mmu::KernelMapping::iova)
    }

    fn discard_va(&self) -> Option<u64> {
        self.discard.as_ref().map(mmu::KernelMapping::iova)
    }

    fn tilemap_va(&self) -> u64 {
        self.rt_memory.iova()
    }

    fn heapmeta_va(&self, render_layout: &G17PRenderStateLayout) -> Result<u64> {
        self.rt_memory
            .iova()
            .checked_add(
                render_layout
                    .heapmeta
                    .checked_sub(render_layout.tilemap)
                    .ok_or(EINVAL)? as u64,
            )
            .ok_or(EOVERFLOW)
    }

    fn tpc_va(&self) -> u64 {
        self.tpc.iova()
    }

    fn aux_fb_va(&self) -> u64 {
        self.aux_fb.iova()
    }

    fn all_reachable_from(&self, vm: &mmu::Vm) -> bool {
        vm.covers_range(
            self.deflake.iova(),
            self.deflake.size() as u64,
            true,
            true,
        ) && self.scene_scratch.as_ref().map_or(true, |mapping| {
            vm.covers_range(mapping.iova(), mapping.size() as u64, true, true)
        }) && self.discard.as_ref().map_or(true, |mapping| {
            vm.covers_range(mapping.iova(), mapping.size() as u64, true, true)
        }) && vm.covers_range(
            self.rt_memory.iova(),
            self.rt_memory.size() as u64,
            true,
            true,
        ) && vm.covers_range(self.tpc.iova(), self.tpc.size() as u64, true, true)
            && vm.covers_range(
                self.aux_fb.iova(),
                self.aux_fb.size() as u64,
                true,
                true,
            )
    }
}

#[cfg(not(test))]
pub(crate) struct G17PUserRenderStorage {
    /// Command GART shared by the final TA and 3D descriptors. Declare the
    /// lease first so teardown retracts and invalidates its TTBAT root before
    /// any of the mappings reachable through that root are destroyed. Native's
    /// second AGXContextIDManager allocation is a refcounted lookup of this
    /// same GART, not a second hardware context.
    command_context: mmu::T8140AppContextLease,
    /// Render-only tail of the USC private-memory aperture.  Ordinary client
    /// VMs start with the 21-buffer compute closure; a render submission adds
    /// blocks 21..25 for the native 22-block initial pool plus four-block
    /// first grow.  Keeping these aliases with the retained render graph makes
    /// the workload switch automatic without placing the overlapping range in
    /// every compute VM.
    render_operand_aliases: KVec<mmu::KernelMapping>,
    descriptors: MappedObject,
    /// Entries-only diagnostic mirror of the descriptor-embedded 3D RCE
    /// programs. Native KSM never binds this incomplete mirror.
    fragment_rce: MappedObject,
    descriptor_ta_high: mmu::KernelMapping,
    descriptor_3d_high: mmu::KernelMapping,
    descriptor_ta_low: mmu::KernelMapping,
    descriptor_3d_low: mmu::KernelMapping,
    /// Render-VM alias of the 3D descriptor backing page. Native allocates
    /// this low address dynamically and uses it for both the descriptor's RCE
    /// self trailers and the four KSM RCEInfo bindings.
    descriptor_3d_render: mmu::KernelMapping,
    ta_context_low: mmu::KernelMapping,
    fragment_context_low: mmu::KernelMapping,
    graph: MappedObject,
    support: MappedObject,
    render_state: MappedObject,
    render_layout: G17PRenderStateLayout,
    render_tvb: Option<MappedObject>,
    /// Context-1 view at the address the TA parameter-buffer hardware forms
    /// from the kext-exact page ID.  G17P adds the compact GART base to
    /// `(page_id << 15)`, so a native page ID derived from 0x100... names the
    /// same backing through a 0x200... hardware alias.
    render_tvb_hardware_alias: Option<mmu::KernelMapping>,
    render_user_aliases: G17PRenderUserAliases,
    parameter_management: Option<G17PParameterManagement>,
    render_ta_status: Option<mmu::KernelMapping>,
    render_3d_status: Option<mmu::KernelMapping>,
    timestamps: MappedObject,
    user_timestamp_aliases: KVec<mmu::KernelMapping>,
    fragment_mcache: Option<g17_submission::G17PClMcacheAperture>,
    /// HardwareBufferID owned by the currently staged TA command.  Retained
    /// storage can outlive a command, so restaging replaces this together with
    /// the descriptors before the next tag-15 record is built.
    ta_hardware_buffer_id: u32,
    /// Process PID captured when the retained channel pair is created.
    owner_pid: u32,
    /// Userspace-side predecessor returned by the native-style global-ID group
    /// reservation. Every restaged command pair keeps this predecessor while
    /// receiving fresh, consecutive 3D/TA current IDs.
    lifecycle_predecessor: u32,
}

#[cfg(not(test))]
#[derive(Debug, Copy, Clone)]
pub(crate) struct G17PUserRenderQueueState {
    pub(crate) tiling: g17_completion::QueueIndices,
    pub(crate) fragment: g17_completion::QueueIndices,
    pub(crate) job_list_empty: bool,
    pub(crate) tiling_timestamps: [u64; 2],
    pub(crate) fragment_timestamps: [u64; 2],
}

/// The install-relevant header of one render tag-15 ConfigUpdate, read back
/// from the bytes the driver actually published.
///
/// The compute firmware RE says the per-queue slot `0x127628 + qid*0x28` is
/// written -- and the `0x40` KTrace receipt emitted -- only when `+0x1a` is
/// non-zero. Everything about render's install has so far been taken on trust
/// from the spec; this makes it a measurement.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) struct G17PRenderConfigUpdateHeader {
    pub(crate) selector: u32,
    pub(crate) entry_low: u64,
    pub(crate) entry_high: u64,
    pub(crate) queue_id: u16,
    pub(crate) install: u16,
    pub(crate) data_master: u16,
    pub(crate) graphics: u16,
    pub(crate) context_id: u16,
    pub(crate) flush_generation: u16,
    pub(crate) flushid_index: u16,
    pub(crate) scheduler_state: u64,
    pub(crate) field_56: u16,
    pub(crate) parameter_buffer_object: u64,
    pub(crate) parameter_buffer_token: u64,
    pub(crate) parameter_buffer_id: u32,
    pub(crate) parameter_buffer_bit: u32,
}

fn decode_g17p_render_config_update(raw: &[u8]) -> Option<G17PRenderConfigUpdateHeader> {
    // One length check up front; every offset below is inside the record.
    if raw.len() < g17_submission::G17P_COLD_OPENING_OPTIONAL_SIZE {
        return None;
    }
    let u16_at = |at: usize| -> u16 {
        u16::from_le_bytes([raw[at], raw[at + 1]])
    };
    let u32_at = |at: usize| -> u32 {
        u32::from_le_bytes([raw[at], raw[at + 1], raw[at + 2], raw[at + 3]])
    };
    let u64_at = |at: usize| -> u64 {
        let mut bytes = [0u8; 8];
        bytes.copy_from_slice(&raw[at..at + 8]);
        u64::from_le_bytes(bytes)
    };
    Some(G17PRenderConfigUpdateHeader {
        selector: u32_at(0x00),
        entry_low: u64_at(0x08),
        entry_high: u64_at(0x10),
        queue_id: u16_at(0x18),
        install: u16_at(0x1a),
        data_master: u16_at(0x22),
        graphics: u16_at(0x26),
        context_id: u16_at(0x32),
        flush_generation: u16_at(0x3e),
        flushid_index: u16_at(0x46),
        scheduler_state: u64_at(0x4a),
        field_56: u16_at(0x56),
        parameter_buffer_object: u64_at(0x6e),
        parameter_buffer_token: u64_at(0x76),
        parameter_buffer_id: u32_at(0x7e),
        parameter_buffer_bit: u32_at(0x82),
    })
}

struct G17PLateTilingBytes {
    item_group: [u8; 32],
    /// Ring items actually published: 3 without tag-16, 4 with it.
    published_items: u32,
    entry_signal: [u8; g17_submission::G17P_COMPUTE_OPTIONAL_EVENT_SIZE],
    optional: [u8; g17_submission::G17P_COLD_OPENING_OPTIONAL_SIZE],
    event: [u8; g17_submission::G17P_COLD_OPENING_EVENT_SIZE],
    pool_a_slot_offset: usize,
    shared_inner_value: u32,
}

impl G17PLateTilingBytes {
    fn apply(&self, graph: &mut [u8], support: &mut [u8]) {
        let items = self.published_items as usize * 8;
        graph[G17P_USER_TILING_RING..G17P_USER_TILING_RING + items]
            .copy_from_slice(&self.item_group[..items]);
        if self.published_items > 3 {
            graph[G17P_USER_TILING_ENTRY_SIGNAL
                ..G17P_USER_TILING_ENTRY_SIGNAL + self.entry_signal.len()]
                .copy_from_slice(&self.entry_signal);
        }
        graph[G17P_USER_TILING_OPTIONAL..G17P_USER_TILING_OPTIONAL + self.optional.len()]
            .copy_from_slice(&self.optional);
        graph[G17P_USER_TILING_EVENT..G17P_USER_TILING_EVENT + self.event.len()]
            .copy_from_slice(&self.event);
        put_u32(graph, G17P_USER_TILING_POINTERS + 0x40, self.published_items);
        put_u32(support, self.pool_a_slot_offset, 2);
        put_u32(
            support,
            G17P_RENDER_SHARED_CONTROL_INNER,
            self.shared_inner_value,
        );
    }

    /// Native's TA submission is split around its SKSM publication. Publish
    /// only the descriptor/tag-15 prefix and its item producer here; the
    /// outer producer is still retained, so firmware cannot dispatch a
    /// half-group before the caller completes the transaction.
    fn apply_native_prefix(&self, graph: &mut [u8]) {
        graph[G17P_USER_TILING_RING..G17P_USER_TILING_RING + 2 * 8]
            .copy_from_slice(&self.item_group[..2 * 8]);
        graph[G17P_USER_TILING_OPTIONAL..G17P_USER_TILING_OPTIONAL + self.optional.len()]
            .copy_from_slice(&self.optional);
        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
        put_u32(graph, G17P_USER_TILING_POINTERS + 0x40, 2);
    }

    /// Publish tag-14 (and optional tag-16) after the TA SKSM entry exists.
    /// The Pool-A transition is intentionally separate: the exact native
    /// chronology advances it only after the TA outer producer is visible.
    fn apply_native_kick(&self, graph: &mut [u8]) {
        let items = self.published_items as usize * 8;
        graph[G17P_USER_TILING_RING + 2 * 8..G17P_USER_TILING_RING + items]
            .copy_from_slice(&self.item_group[2 * 8..items]);
        if self.published_items > 3 {
            graph[G17P_USER_TILING_ENTRY_SIGNAL
                ..G17P_USER_TILING_ENTRY_SIGNAL + self.entry_signal.len()]
                .copy_from_slice(&self.entry_signal);
        }
        graph[G17P_USER_TILING_EVENT..G17P_USER_TILING_EVENT + self.event.len()]
            .copy_from_slice(&self.event);
        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
        put_u32(
            graph,
            G17P_USER_TILING_POINTERS + 0x40,
            self.published_items,
        );
    }

    fn apply_native_pool_transition(&self, support: &mut [u8]) {
        put_u32(support, self.pool_a_slot_offset, 2);
        put_u32(
            support,
            G17P_RENDER_SHARED_CONTROL_INNER,
            self.shared_inner_value,
        );
    }
}

fn build_g17p_late_tiling_bytes(
    descriptor: u64,
    graph: u64,
    optional_addresses: g17_submission::G17PColdOpeningOptionalAddresses,
    ta_tag15_context_id: u16,
    submission_ordinal: u32,
    entry_stamp: u32,
    tag16: bool,
    native_pool_a_first_record: bool,
) -> core::result::Result<G17PLateTilingBytes, g17_submission::SubmissionError> {
    let record_a = g17_submission::g17p_render_pool_a_record_index(
        submission_ordinal,
        native_pool_a_first_record,
    );
    let mut bytes = G17PLateTilingBytes {
        item_group: [0; 32],
        published_items: if tag16 { 4 } else { 3 },
        entry_signal: [0; g17_submission::G17P_COMPUTE_OPTIONAL_EVENT_SIZE],
        optional: [0; g17_submission::G17P_COLD_OPENING_OPTIONAL_SIZE],
        event: [0; g17_submission::G17P_COLD_OPENING_EVENT_SIZE],
        pool_a_slot_offset: G17P_RENDER_POOL_A_SLOTS + 4 + record_a * 4,
        shared_inner_value: submission_ordinal
            .checked_add(1)
            .and_then(|value| value.checked_mul(2))
            .ok_or(g17_submission::SubmissionError::PartialOpeningQueueHeadOutOfRange)?,
    };
    let optional = graph
        .checked_add(G17P_USER_TILING_OPTIONAL as u64)
        .ok_or(g17_submission::SubmissionError::PartialOpeningAddressOverflow)?;
    let event = graph
        .checked_add(G17P_USER_TILING_EVENT as u64)
        .ok_or(g17_submission::SubmissionError::PartialOpeningAddressOverflow)?;
    for (index, address) in [descriptor, optional, event].iter().enumerate() {
        put_u64(&mut bytes.item_group, index * 8, *address);
    }
    if tag16 {
        let entry_signal = graph
            .checked_add(G17P_USER_TILING_ENTRY_SIGNAL as u64)
            .ok_or(g17_submission::SubmissionError::PartialOpeningAddressOverflow)?;
        put_u64(&mut bytes.item_group, 3 * 8, entry_signal);
        g17_submission::encode_g17p_compute_optional_event(
            g17_submission::G17PComputeOptionalEvent {
                queue_id: g17p_render_queue_id(true),
                // See the fragment sites: this field is the previous stamp.
                old_timestamp: u64::from(entry_stamp).saturating_sub(1),
            },
            &mut bytes.entry_signal,
        )?;
    }
    g17_submission::apply_g17p_retained_optional(
        g17_submission::G17PColdOpeningStage::Tiling,
        optional_addresses,
        submission_ordinal,
        g17p_render_generation_bias_enabled(),
        g17p_render_queue_id(true),
        g17p_render_install(true),
        ta_tag15_context_id,
        &mut bytes.optional,
    )?;
    g17_submission::apply_g17p_retained_event_with_stamp(
        g17_submission::G17PColdOpeningStage::Tiling,
        entry_stamp,
        g17p_render_queue_id(true),
        &mut bytes.event,
    )?;
    Ok(bytes)
}

/// All fallible mapping and byte construction finishes before this object is
/// created. `publish` performs only the six measured late TA memory copies.
#[cfg(not(test))]
pub(crate) struct G17PLateTilingPublication {
    graph: *mut u8,
    support: *mut u8,
    bytes: G17PLateTilingBytes,
}

#[cfg(not(test))]
impl G17PLateTilingPublication {
    /// The TA queue's tag-15 ConfigUpdate header, read out of the bytes this
    /// publication is about to apply. The TA record is not in the graph yet at
    /// staging time -- the outer publisher copies it in between the two ring
    /// slots -- so it has to be read from here.
    pub(crate) fn config_update_header(&self) -> Option<G17PRenderConfigUpdateHeader> {
        decode_g17p_render_config_update(&self.bytes.optional)
    }

    pub(crate) fn publish(self) {
        let graph = unsafe {
            // SAFETY: the retained graph object owns this previously acquired
            // VMap for the complete synchronous publication transaction.
            core::slice::from_raw_parts_mut(self.graph, G17P_RENDER_QUEUE_GRAPH_SIZE)
        };
        let support = unsafe {
            // SAFETY: the retained support object owns this previously
            // acquired VMap for the same transaction.
            core::slice::from_raw_parts_mut(self.support, G17P_RENDER_SUPPORT_SIZE)
        };
        self.bytes.apply(graph, support);
    }

    pub(crate) fn publish_native_prefix(&self) {
        let graph = unsafe {
            // SAFETY: the retained graph object owns this previously acquired
            // VMap for the complete synchronous publication transaction.
            core::slice::from_raw_parts_mut(self.graph, G17P_RENDER_QUEUE_GRAPH_SIZE)
        };
        self.bytes.apply_native_prefix(graph);
    }

    pub(crate) fn publish_native_kick(&self) {
        let graph = unsafe {
            // SAFETY: same retained VMap as `publish_native_prefix`; the
            // synchronous caller keeps the storage alive between phases.
            core::slice::from_raw_parts_mut(self.graph, G17P_RENDER_QUEUE_GRAPH_SIZE)
        };
        self.bytes.apply_native_kick(graph);
    }

    pub(crate) fn publish_native_pool_transition(self) {
        let support = unsafe {
            // SAFETY: the retained support object owns this previously
            // acquired VMap for the complete synchronous transaction.
            core::slice::from_raw_parts_mut(self.support, G17P_RENDER_SUPPORT_SIZE)
        };
        self.bytes.apply_native_pool_transition(support);
        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
    }
}

#[cfg(not(test))]
impl G17PComputeDescriptorStorage {
    pub(crate) fn new(
        dev: &AsahiDevice,
        uat: &mmu::Uat,
        channel_control_gpu_va: u64,
    ) -> Result<Self> {
        // These are the descriptor, queue, and support ranges cleaned by the
        // hardware-proven m1n1 producer before it publishes CL2. Linux writes
        // them through a persistent CPU alias, so use WC aliases rather than
        // leaving dirty write-back lines behind the subsequent dmb ish.
        let mut descriptors = MappedObject::new_wc_in_range(
            dev,
            uat.kernel_lower_vm(),
            g17p_cl_dynamic_low_va_range(),
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
        let preempt = MappedObject::new_wc(
            dev,
            uat,
            G17P_COMPUTE_PREEMPT_SIZE,
            mmu::UAT_PGSZ as u64,
            mmu::PROT_GPU_FW_SHARED_RW,
        )?;
        let robustness = MappedObject::new_wc(
            dev,
            uat,
            mmu::UAT_PGSZ,
            mmu::UAT_PGSZ as u64,
            mmu::PROT_GPU_FW_SHARED_RW,
        )?;
        let operand_state = MappedObject::new_wc_in_range(
            dev,
            uat.kernel_lower_vm(),
            g17p_cl_dynamic_low_va_range(),
            G17P_COMPUTE_OPERAND_STATE_SIZE,
            mmu::UAT_PGSZ as u64,
            mmu::PROT_GPU_FW_SHARED_RW,
        )?;
        let mut support = MappedObject::new_wc(
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
            raw[mmu::UAT_PGSZ + 4..mmu::UAT_PGSZ + 8].copy_from_slice(&1u32.to_le_bytes());
            Ok(())
        })?;
        let mut queue_graph = MappedObject::new_wc(
            dev,
            uat,
            G17P_COMPUTE_QUEUE_GRAPH_SIZE,
            mmu::UAT_PGSZ as u64,
            mmu::PROT_GPU_FW_SHARED_RW,
        )?;
        let queue_graph_gpu_va = queue_graph.iova();
        queue_graph.with_bytes_mut(|raw| {
            g17_compute::build_compute_queue_graph(queue_graph_gpu_va, channel_control_gpu_va, raw)
                .map(|_| ())
                .map_err(|_| EINVAL)
        })?;
        let queue_context_low = MappedObject::new_wc_in_range(
            dev,
            uat.kernel_lower_vm(),
            g17p_cl_dynamic_low_va_range(),
            g17_compute::COMPUTE_QUEUE_CONTEXT_EXTENT,
            mmu::UAT_PGSZ as u64,
            mmu::PROT_GPU_FW_SHARED_RW,
        )?;
        let queue_context_high = MappedObject::new_wc(
            dev,
            uat,
            g17_compute::COMPUTE_QUEUE_CONTEXT_EXTENT,
            mmu::UAT_PGSZ as u64,
            mmu::PROT_GPU_FW_SHARED_RW,
        )?;

        Ok(Self {
            descriptors,
            descriptors_high,
            preempt,
            robustness,
            operand_state,
            support,
            queue_graph,
            queue_context_low,
            queue_context_high,
            ring_reclaimed: 0,
            ring_reclaimed_total: 0,
        })
    }

    pub(crate) fn descriptor_gpu_vas(&self, slot: u8) -> (u64, u64) {
        let offset = slot as u64 * G17P_COMPUTE_DESCRIPTOR_SLOT_SIZE as u64;
        (
            self.descriptors_high.iova() + offset,
            self.descriptors.iova() + offset,
        )
    }

    /// Install the three-qword Tier-2 table consumed by the hardware-proven
    /// direct add3 preamble. Register 0x1a510 names this retained object; the
    /// shader reads input A, input B, and output at +0x14a0.
    /// Read back the preempt object's argument window at +0x1480..+0x14c0.
    ///
    /// Registers 0x1a4d0/0x1a4d8/0x1a4e0/0x1a4e8 name preempt+0x1480/+0x1488/
    /// +0x1490/+0x1498 on EVERY compute submit, and the add3 proof flag
    /// additionally writes three operand pointers at +0x14a0. So this window
    /// is populated for an add3 submit and untouched for a Honeykrisp one --
    /// the only GPU-visible state the proof flag changes.
    pub(crate) fn g17p_preempt_argument_window(&mut self) -> Result<[u8; 0x40]> {
        self.preempt.with_bytes_mut(|raw| {
            let start = G17P_COMPUTE_ADD3_RESOURCE_TABLE - 0x20;
            let window = raw.get(start..start + 0x40).ok_or(ERANGE)?;
            let mut out = [0u8; 0x40];
            out.copy_from_slice(window);
            Ok(out)
        })
    }

    pub(crate) fn write_g17p_add3_resource_table(&mut self, buffers: [u64; 3]) -> Result {
        self.preempt
            .with_bytes_mut(|raw| apply_g17p_add3_resource_table(raw, buffers).ok_or(ERANGE))
    }

    pub(crate) fn map_user_execution_state(
        &mut self,
        vm: &mmu::Vm,
    ) -> Result<G17PComputeUserMappings> {
        // Native uses one stable descriptor-low identity. Map the same GEM at
        // the context-0 low VA in the client VM instead of allocating a second
        // low address and encoding that unrelated alias in the descriptor.
        let descriptor_low = self.descriptors.iova();
        let descriptors =
            self.descriptors
                .map_alias_at(vm, descriptor_low, mmu::PROT_GPU_FW_SHARED_RW)?;
        let preempt = self.preempt.map_alias_in_range(
            vm,
            g17p_cl_dynamic_low_va_range(),
            mmu::UAT_PGSZ as u64,
            mmu::PROT_GPU_FW_SHARED_RW,
        )?;
        let robustness = self.robustness.map_alias_in_range(
            vm,
            g17p_cl_dynamic_low_va_range(),
            mmu::UAT_PGSZ as u64,
            mmu::PROT_GPU_FW_SHARED_RW,
        )?;
        let operand_state = self.operand_state.map_alias_in_range(
            vm,
            g17p_cl_dynamic_low_va_range(),
            mmu::UAT_PGSZ as u64,
            mmu::PROT_GPU_FW_SHARED_RW,
        )?;
        let queue_context = self.queue_context_low.map_alias_in_range(
            vm,
            g17p_cl_dynamic_low_va_range(),
            mmu::UAT_PGSZ as u64,
            mmu::PROT_GPU_FW_SHARED_RW,
        )?;
        let mappings = G17PComputeUserMappings {
            descriptors,
            preempt,
            robustness,
            operand_state,
            queue_context,
        };
        if !mappings.all_reachable_from(vm) {
            return Err(EFAULT);
        }
        Ok(mappings)
    }

    pub(crate) fn scheduler_record_gpu_va(&self) -> u64 {
        self.support.iova() + 0x100
    }

    pub(crate) fn dispatch_a_gpu_va(&self) -> u64 {
        self.support.iova() + G17P_COMPUTE_STAMP_SUPPORT_OFFSET
    }

    /// Zero the completion stamp before a submission is published.
    ///
    /// The compute descriptor's stamp pointer (`+0x0F40`) is exactly
    /// `dispatch_a_gpu_va()`, and the firmware stores the u32 it reads from
    /// descriptor `+0x0F50` through it once the completion's GPU cache
    /// maintenance has been issued AND polled to completion. Zeroing the word
    /// first is what turns "non-zero" into "written for this submission" --
    /// the same discipline `clear_compute_completion` applies to the KSM
    /// record, and necessary here for the same reason: retained submissions
    /// program the same descriptor stamp value, so the value alone cannot
    /// distinguish submissions.
    ///
    /// The firmware only ever STORES to this word (`str w22, [x26]`), so
    /// clearing it cannot disturb anything in flight.
    ///
    /// Returns what it discarded.
    pub(crate) fn arm_compute_stamp(&mut self) -> Result<u32> {
        let base = self.support.iova() + G17P_COMPUTE_STAMP_SUPPORT_OFFSET;
        let previous = self.support.read_u32(base)?;
        self.support.write_u32(base, 0)?;
        self.support.write_u32(base + 4, 0)?;
        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
        Ok(previous)
    }

    /// Read the completion stamp the firmware writes at the end of a retire.
    pub(crate) fn read_compute_stamp(&mut self) -> Result<u32> {
        core::sync::atomic::fence(core::sync::atomic::Ordering::Acquire);
        let base = self.support.iova() + G17P_COMPUTE_STAMP_SUPPORT_OFFSET;
        self.support.read_u32(base)
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

    pub(crate) fn stage_initial_channel_group_without_fallback_event(
        &mut self,
        descriptor: u64,
        sksm_entries: g17_compute::ComputeSksmEntryAliases,
        shared_control: u64,
        channel_control: u64,
        queue_id: u8,
        install_queue: bool,
    ) -> Result<g17_compute::ComputeChannelGroupAddresses> {
        let queue = self.queue_graph.iova();
        let graph_vmap = self.queue_graph.object.vmap()?;
        let context_vmap = self.queue_context_high.object.vmap()?;
        let graph = unsafe {
            // SAFETY: each retained VMap covers the complete object.
            core::slice::from_raw_parts_mut(graph_vmap.as_mut_ptr(), self.queue_graph.logical_size)
        };
        let queue_context = unsafe {
            // SAFETY: each retained VMap covers the complete object.
            core::slice::from_raw_parts_mut(
                context_vmap.as_mut_ptr(),
                self.queue_context_high.logical_size,
            )
        };
        let mut group = g17_compute::build_initial_compute_channel_group(
            queue,
            descriptor,
            sksm_entries,
            shared_control,
            channel_control,
            queue_id,
            graph,
            queue_context,
            install_queue,
        )
        .map_err(|_| EINVAL)?;

        // The source builder provides all command bytes. Direct first bind
        // begins with only the main descriptor reachable at slot zero; tag 15
        // and tag 16 are published through distinct ordered transitions around
        // the SKSM entry. Tag 14 remains omitted in direct mode.
        graph[g17_compute::COMPUTE_EVENT
            ..g17_compute::COMPUTE_EVENT + g17_compute::COMPUTE_EVENT_SIZE]
            .fill(0);
        put_u64(graph, g17_compute::COMPUTE_ITEM_RING + 0x08, 0);
        put_u64(graph, g17_compute::COMPUTE_ITEM_RING + 0x10, 0);
        put_u32(graph, g17_compute::COMPUTE_QUEUE_POINTERS + 0x40, 1);
        group.write_index = 3;
        Ok(group)
    }

    /// Current item-ring producer of the retained compute queue graph.
    ///
    /// A repeat submission must append at this cursor. The first-bind builder
    /// resets it to 1, which is exactly why re-running that builder for a
    /// second submission made the firmware's consumer and our producer
    /// disagree and nothing was ever picked up.
    pub(crate) fn compute_ring_producer(&mut self) -> Result<u32> {
        core::sync::atomic::fence(core::sync::atomic::Ordering::Acquire);
        self.queue_graph.with_bytes_mut(|graph| {
            let offset = g17_compute::COMPUTE_QUEUE_POINTERS + 0x40;
            Ok(u32::from_le_bytes(
                graph
                    .get(offset..offset + 4)
                    .ok_or(ERANGE)?
                    .try_into()
                    .map_err(|_| ERANGE)?,
            ))
        })
    }

    /// Read the item-ring cursors and the first eight slots.
    ///
    /// The decisive number is `consumer`. If it advances to 4 after a
    /// submission the firmware has demonstrably executed, the firmware tracks a
    /// consumer cursor and appending at the producer is the right model. If it
    /// stays 0 across a working submission, that word is not a firmware-owned
    /// consumer and the host is expected to rewind and reuse slots 0..3
    /// instead -- see `g17p_repeat_ring`.
    pub(crate) fn read_compute_ring_state(&mut self) -> Result<G17PComputeRingState> {
        core::sync::atomic::fence(core::sync::atomic::Ordering::Acquire);
        let queue = self.queue_graph.iova();
        let reclaimed = self.ring_reclaimed;
        let reclaimed_total = self.ring_reclaimed_total;
        self.queue_graph.with_bytes_mut(|graph| {
            let read_u32 = |offset: usize| -> Result<u32> {
                Ok(u32::from_le_bytes(
                    graph
                        .get(offset..offset + 4)
                        .ok_or(ERANGE)?
                        .try_into()
                        .map_err(|_| ERANGE)?,
                ))
            };
            let read_u64 = |offset: usize| -> Result<u64> {
                Ok(u64::from_le_bytes(
                    graph
                        .get(offset..offset + 8)
                        .ok_or(ERANGE)?
                        .try_into()
                        .map_err(|_| ERANGE)?,
                ))
            };
            let pointers = g17_compute::COMPUTE_QUEUE_POINTERS;
            let ring = g17_compute::COMPUTE_ITEM_RING;
            let mut items = [0u64; 8];
            for (index, slot) in items.iter_mut().enumerate() {
                *slot = read_u64(ring + index * 8)?;
            }
            Ok(G17PComputeRingState {
                queue,
                consumer: read_u32(pointers)?,
                producer: read_u32(pointers + 0x40)?,
                mirror: read_u32(pointers + 0x50)?,
                count: read_u32(pointers + 0x60)?,
                items,
                reclaimed,
                reclaimed_total,
            })
        })
    }

    /// Zero every item-ring slot the firmware has already consumed.
    ///
    /// `apply_g17p_initial_command_pointer` refuses to write a slot that is not
    /// already zero. That guard is worth keeping -- it is what would catch us
    /// overwriting a record still in flight -- but nothing was ever clearing
    /// consumed slots, so the ring was single-use: 0x500 entries at four
    /// records per submission is 320 submissions and then a hard stop. That is
    /// enough for a probe and nowhere near enough for a CTS run.
    ///
    /// Recycling is only sound because the firmware's ownership of the consumer
    /// cursor is now PROVEN on hardware: across two successive submissions the
    /// cursor read 4 and then 8, exactly tracking what we published. Slots
    /// strictly behind it are therefore known-retired and safe to clear.
    ///
    /// Returns the number of slots reclaimed.
    pub(crate) fn recycle_consumed_ring_slots(&mut self) -> Result<u32> {
        core::sync::atomic::fence(core::sync::atomic::Ordering::Acquire);
        let mut reclaimed = self.ring_reclaimed;
        let count = self.queue_graph.with_bytes_mut(|graph| {
            let pointers = g17_compute::COMPUTE_QUEUE_POINTERS;
            let read_u32 = |graph: &[u8], offset: usize| -> Result<u32> {
                Ok(u32::from_le_bytes(
                    graph
                        .get(offset..offset + 4)
                        .ok_or(ERANGE)?
                        .try_into()
                        .map_err(|_| ERANGE)?,
                ))
            };
            let entries = read_u32(graph, pointers + 0x60)?;
            if entries == 0 {
                return Err(ERANGE);
            }
            let raw_consumer = read_u32(graph, pointers)?;
            // Normalising a cursor that reads `>= entries` is OPT-IN. It was
            // meant to survive a consumer reported as exactly `entries` at the
            // wrap, but if the firmware can report an out-of-range consumer for
            // any OTHER reason, folding it mod `entries` makes reclaim zero
            // slots the firmware has not consumed -- letting a later repeat
            // overwrite live entries. Refusing, as the 319-deep build did, is
            // the conservative behaviour and is the default.
            let consumer = if *crate::module_parameters::g17p_ring_normalize.value() != 0 {
                raw_consumer % entries
            } else {
                if raw_consumer >= entries {
                    return Err(ERANGE);
                }
                raw_consumer
            };
            if reclaimed >= entries {
                return Err(ERANGE);
            }
            let ring = g17_compute::COMPUTE_ITEM_RING;
            let mut cleared = 0u32;
            while reclaimed != consumer {
                let slot = ring
                    .checked_add(reclaimed as usize * core::mem::size_of::<u64>())
                    .ok_or(ERANGE)?;
                graph
                    .get_mut(slot..slot + core::mem::size_of::<u64>())
                    .ok_or(ERANGE)?
                    .fill(0);
                reclaimed = reclaimed.wrapping_add(1) % entries;
                cleared = cleared.saturating_add(1);
                // The ring cannot need more than a full lap of clearing; refuse
                // to spin if the cursors are inconsistent.
                if cleared > entries {
                    return Err(ERANGE);
                }
            }
            Ok(cleared)
        })?;
        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
        self.ring_reclaimed = reclaimed;
        self.ring_reclaimed_total = self.ring_reclaimed_total.saturating_add(count as u64);
        Ok(count)
    }

    /// Republish the tag-15 ConfigUpdate record for an ALREADY-installed queue.
    ///
    /// Only the optional-record window is rewritten: the queue header, the
    /// item ring, the ring producer, the tag-16 window and the queue context
    /// all keep the state the firmware is already using. `install_queue` is
    /// false, which is the one field macOS varies between a first bind and a
    /// repeat.
    pub(crate) fn stage_repeat_config_update(
        &mut self,
        sksm_entries: g17_compute::ComputeSksmEntryAliases,
        shared_control: u64,
        channel_control: u64,
        queue_id: u8,
        rewind: bool,
    ) -> Result<g17_compute::ComputeChannelGroupAddresses> {
        let queue = self.queue_graph.iova();
        let optional = queue
            .checked_add(g17_compute::COMPUTE_OPTIONAL as u64)
            .ok_or(EINVAL)?;
        let event = queue
            .checked_add(g17_compute::COMPUTE_EVENT as u64)
            .ok_or(EINVAL)?;
        self.queue_graph.with_bytes_mut(|graph| {
            g17_compute::encode_compute_config_update(
                graph,
                sksm_entries,
                shared_control,
                channel_control,
                queue_id,
                // A repeat is by definition not a first bind, so the install
                // flag is clear -- which is also what every captured macOS
                // steady-state submission sends.
                false,
            )
            .map_err(|_| EINVAL)
        })?;
        if rewind {
            // Reuse ring slots 0..3 instead of appending: zero them and put the
            // producer back to 0, so the four records land exactly where the
            // first bind put them. This is the alternative to the append model
            // and it is what the layout implies IF the firmware does not own
            // the `+0x100` consumer word. Nothing else in the graph is touched
            // -- the header, UUID and queue context stay as the first bind left
            // them, and no QID configure is re-issued.
            let records = g17_compute::COMPUTE_REPEAT_RING_RECORD_COUNT;
            self.queue_graph.with_bytes_mut(|graph| {
                let ring = g17_compute::COMPUTE_ITEM_RING;
                graph
                    .get_mut(ring..ring + records * core::mem::size_of::<u64>())
                    .ok_or(ERANGE)?
                    .fill(0);
                put_u32(graph, g17_compute::COMPUTE_QUEUE_POINTERS + 0x40, 0);
                Ok(())
            })?;
            core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
        }
        let write_index = self.compute_ring_producer()?;
        Ok(g17_compute::ComputeChannelGroupAddresses {
            queue,
            optional,
            event,
            write_index,
        })
    }

    /// Read the support-page words the descriptor names: dispatch A/B at
    /// `+0x300`/`+0x308` and status A/B at `+0x310`/`+0x318`.
    ///
    /// These are the GPU/firmware's own progress words for the submission, and
    /// they are DRAM the driver allocated, so they are safe to sample at any
    /// time -- unlike anything in the sgx register file. If they stop advancing
    /// while completion records keep arriving, the firmware is retiring work the
    /// GPU never executed.
    pub(crate) fn read_compute_support_words(&mut self) -> Result<[u64; 4]> {
        core::sync::atomic::fence(core::sync::atomic::Ordering::Acquire);
        let base = self.support.iova();
        Ok([
            self.support.read_u64(base + 0x300)?,
            self.support.read_u64(base + 0x308)?,
            self.support.read_u64(base + 0x310)?,
            self.support.read_u64(base + 0x318)?,
        ])
    }

    /// Point the retained queue-context item at the descriptor a repeat is
    /// publishing.
    ///
    /// The first-bind builder writes this item once, with `+0x10` naming the
    /// descriptor it published. A repeat publishes a DIFFERENT descriptor slot,
    /// and nothing was updating this, so the item kept naming submission 1's
    /// descriptor. Whatever the firmware uses the item for, "consistent with
    /// the work actually published" is the only defensible state, and it is the
    /// state the working first bind is in.
    ///
    /// Only the descriptor qword is rewritten. The rest of the item -- the
    /// `0x1000_1000_0000_0004` header, the queue base, `0xffff_0801_0000_0001`,
    /// the data-master word and the tail constants -- is left exactly as the
    /// first bind established it.
    pub(crate) fn refresh_repeat_queue_context(&mut self, descriptor: u64) -> Result {
        let item = g17_compute::COMPUTE_QUEUE_CONTEXT_ITEM_OFFSET;
        self.queue_context_high.with_bytes_mut(|context| {
            let slot = item.checked_add(0x10).ok_or(ERANGE)?;
            if slot.checked_add(8).ok_or(ERANGE)? > context.len() {
                return Err(ERANGE);
            }
            put_u64(context, slot, descriptor);
            Ok(())
        })?;
        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
        Ok(())
    }

    /// Write a COMPLETE queue-context item for this submission's stamp index.
    ///
    /// The alternative to `refresh_repeat_queue_context`, which rewrites item 1
    /// -- the first bind's -- in place. If the array is indexed by stamp then
    /// items 2.. have never been written at all and every repeat has been
    /// running against a zero item while overwriting the first bind's; if it is
    /// a single current-work record then rewriting item 1 is right and this is
    /// wrong. Six consecutive passes were achieved with item 1 being rewritten,
    /// so that is the default and this is the experiment.
    pub(crate) fn write_repeat_queue_context_item(
        &mut self,
        stamp_index: u8,
        descriptor: u64,
        queue_id: u8,
    ) -> Result {
        let queue_base = self.queue_graph.iova();
        self.queue_context_high.with_bytes_mut(|context| {
            g17_compute::encode_compute_queue_context_item(
                context,
                stamp_index as usize,
                descriptor,
                queue_base,
                queue_id,
            )
            .map_err(|_| ERANGE)
        })?;
        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
        Ok(())
    }

    /// Entries in the queue's item ring, as the graph reports them.
    pub(crate) fn compute_ring_entries(&mut self) -> Result<u32> {
        Ok(self.read_compute_ring_state()?.count)
    }

    /// Item-ring slots free for the host to publish into, i.e. the distance
    /// from the producer to the firmware's consumer, less the one slot a ring
    /// must always keep empty to stay distinguishable from full.
    pub(crate) fn compute_ring_free_slots(&mut self) -> Result<u32> {
        let state = self.read_compute_ring_state()?;
        if state.count == 0 {
            return Err(ERANGE);
        }
        let state = if *crate::module_parameters::g17p_ring_normalize.value() != 0 {
            G17PComputeRingState {
                producer: state.producer % state.count,
                consumer: state.consumer % state.count,
                ..state
            }
        } else {
            if state.producer >= state.count || state.consumer >= state.count {
                return Err(ERANGE);
            }
            state
        };
        Ok(state
            .consumer
            .wrapping_sub(state.producer)
            .wrapping_add(state.count)
            .wrapping_sub(1)
            % state.count)
    }

    /// Publish one already-resident record pointer at an explicit ring
    /// producer. Used by the repeat path for the tag-3 command record, whose
    /// address changes with the descriptor slot.
    pub(crate) fn publish_command_pointer_at(
        &mut self,
        expected_producer: u32,
        command_gpu_va: u64,
    ) -> Result<u32> {
        // Take the producer the ring transition itself computed. It is
        // `(producer + 1) % count`; comparing it against a locally computed
        // `expected_producer + 1` was THE 320-submission wall. The item ring is
        // `0x500` entries and a submission publishes four records, so
        // submission 320's last record lands at producer `0x4ff`, the
        // transition correctly returns 0, the caller expected `0x500`, and the
        // publish was refused with EBUSY on a ring that was entirely free.
        // `apply_g17p_initial_command_pointer` already validates everything the
        // comparison was standing in for: that the graph's producer really is
        // `expected_producer`, that the programmed count matches, that the next
        // slot is not the consumer, and that the target slot is zero.
        self.queue_graph.with_bytes_mut(|graph| {
            apply_g17p_initial_command_pointer(graph, expected_producer, command_gpu_va)
                .ok_or(EBUSY)
        })
    }

    /// Publish the prepared tag-15 ConfigUpdate pointer at channel slot one.
    pub(crate) fn publish_initial_config_update_pointer(
        &mut self,
        group: g17_compute::ComputeChannelGroupAddresses,
    ) -> Result {
        self.queue_graph.with_bytes_mut(|graph| {
            apply_g17p_initial_command_pointer(graph, 1, group.optional)
                .ok_or(EBUSY)
                .map(|_| ())
        })
    }

    /// Write and publish the first-bind tag-16 EntrySignal at channel slot two.
    pub(crate) fn publish_initial_entry_signal(
        &mut self,
        entry_signal: &[u8; g17_submission::G17P_COMPUTE_OPTIONAL_EVENT_SIZE],
    ) -> Result {
        self.publish_initial_entry_signal_at(2, entry_signal).map(|_| ())
    }

    /// As above, but at an explicit ring producer. Native's transport order is
    /// tag 3 -> 15 -> 14 -> 16, so tag 16 does not always land at producer 2:
    /// its handler ends with a broadcast `MMIO[0x210e0]/[0x210d8] = -1` "go"
    /// poke, which native issues strictly after AddKicks.
    pub(crate) fn publish_initial_entry_signal_at(
        &mut self,
        expected_producer: u32,
        entry_signal: &[u8; g17_submission::G17P_COMPUTE_OPTIONAL_EVENT_SIZE],
    ) -> Result<u32> {
        let entry_signal_gpu_va =
            self.queue_graph.iova() + g17_compute::COMPUTE_EVENT as u64;
        self.queue_graph.with_bytes_mut(|graph| {
            graph[g17_compute::COMPUTE_EVENT
                ..g17_compute::COMPUTE_EVENT + entry_signal.len()]
                .copy_from_slice(entry_signal);
            // Wrap-safe: see `publish_command_pointer_at`. This is the record
            // that actually hit the wall, because the tag-16 EntrySignal is the
            // fourth and last of a submission's four records and therefore the
            // one that lands on the ring's final slot.
            apply_g17p_initial_command_pointer(graph, expected_producer, entry_signal_gpu_va)
                .ok_or(EBUSY)
        })
    }

    /// Write and publish one tag-14 `KsmKickQueueAddKicks` channel command.
    ///
    /// This is the firmware transport for the host `addKicks` MMIO pair. J700
    /// cannot reach the SKSM tag/data port at all, but the matched A000 image
    /// dispatches command type 14 to `0xfffffc0000011710`, which writes the QID
    /// to `sgx+0x103a8` and the record's count to `sgx+0x21018` from the
    /// firmware side. The record is placed in the retained queue graph exactly
    /// as tags 15 and 16 are, and published through the same command-pointer
    /// ring transition that `AGXChannel::writeChannelCommandPointer` performs.
    ///
    /// The whole reserved window is zeroed first: the A000 handler
    /// cache-invalidates `G17P_KSM_ADD_KICKS_COMMAND_FETCH_SIZE` bytes, which is
    /// wider than the host-written extent.
    ///
    /// Returns the ring producer after the publication.
    pub(crate) fn publish_add_kicks_command(
        &mut self,
        expected_producer: u32,
        add_kicks: &[u8; g17_submission::G17P_KSM_ADD_KICKS_COMMAND_SIZE],
    ) -> Result<u32> {
        let add_kicks_gpu_va = self.queue_graph.iova() + g17_compute::COMPUTE_ADD_KICKS as u64;
        self.queue_graph.with_bytes_mut(|graph| {
            let window = graph
                .get_mut(
                    g17_compute::COMPUTE_ADD_KICKS
                        ..g17_compute::COMPUTE_ADD_KICKS + g17_compute::COMPUTE_ADD_KICKS_SIZE,
                )
                .ok_or(ERANGE)?;
            window.fill(0);
            window[..add_kicks.len()].copy_from_slice(add_kicks);
            // Wrap-safe: see `publish_command_pointer_at`.
            apply_g17p_initial_command_pointer(graph, expected_producer, add_kicks_gpu_va)
                .ok_or(EBUSY)
        })
    }

    pub(crate) fn snapshot_initial_channel_group(&mut self) -> Result<G17PComputeGraphSnapshot> {
        let read_u16 = |raw: &[u8], offset: usize| -> Result<u16> {
            Ok(u16::from_le_bytes(
                raw.get(offset..offset + 2)
                    .ok_or(ERANGE)?
                    .try_into()
                    .map_err(|_| ERANGE)?,
            ))
        };
        let read_u32 = |raw: &[u8], offset: usize| -> Result<u32> {
            Ok(u32::from_le_bytes(
                raw.get(offset..offset + 4)
                    .ok_or(ERANGE)?
                    .try_into()
                    .map_err(|_| ERANGE)?,
            ))
        };
        let read_u64 = |raw: &[u8], offset: usize| -> Result<u64> {
            Ok(u64::from_le_bytes(
                raw.get(offset..offset + 8)
                    .ok_or(ERANGE)?
                    .try_into()
                    .map_err(|_| ERANGE)?,
            ))
        };

        core::sync::atomic::fence(core::sync::atomic::Ordering::Acquire);
        let queue_gpu_va = self.queue_graph.iova();
        let context_high_gpu_va = self.queue_context_high.iova();
        let graph_vmap = self.queue_graph.object.vmap()?;
        let graph = unsafe {
            // SAFETY: the retained VMap covers the complete queue graph.
            core::slice::from_raw_parts(
                graph_vmap.as_mut_ptr(),
                self.queue_graph.logical_size,
            )
        };
        let descriptor_vmap = self.descriptors.object.vmap()?;
        let descriptor = unsafe {
            // SAFETY: the retained VMap covers the complete descriptor store.
            core::slice::from_raw_parts(
                descriptor_vmap.as_mut_ptr(),
                self.descriptors.logical_size,
            )
        };
        let support_vmap = self.support.object.vmap()?;
        let support = unsafe {
            // SAFETY: the retained VMap covers the complete support object.
            core::slice::from_raw_parts(support_vmap.as_mut_ptr(), self.support.logical_size)
        };
        let context_vmap = self.queue_context_high.object.vmap()?;
        let context = unsafe {
            // SAFETY: the retained VMap covers the complete high context.
            core::slice::from_raw_parts(
                context_vmap.as_mut_ptr(),
                self.queue_context_high.logical_size,
            )
        };

        let pointers = g17_compute::COMPUTE_QUEUE_POINTERS;
        let items = g17_compute::COMPUTE_ITEM_RING;
        let optional = g17_compute::COMPUTE_OPTIONAL;
        let event = g17_compute::COMPUTE_EVENT;
        let snapshot = G17PComputeGraphSnapshot {
            queue: queue_gpu_va,
            pointers: queue_gpu_va + pointers as u64,
            item_ring: queue_gpu_va + items as u64,
            context_high: context_high_gpu_va,
            cursors: [
                read_u32(graph, pointers)?,
                read_u32(graph, pointers + 0x30)?,
                read_u32(graph, pointers + 0x40)?,
            ],
            gpu_read_cursors: [
                read_u32(graph, 0x18)?,
                read_u32(graph, 0x1c)?,
                read_u32(graph, 0x20)?,
            ],
            uuid: read_u32(graph, 0x48)?,
            queue_context: read_u64(graph, 0x9c)?,
            items: [
                read_u64(graph, items)?,
                read_u64(graph, items + 8)?,
                read_u64(graph, items + 16)?,
            ],
            event: [
                read_u32(graph, event)?,
                read_u32(graph, event + 4)?,
                read_u32(graph, event + 8)?,
                read_u32(graph, event + 0x0c)?,
                read_u32(graph, event + 0x10)?,
            ],
            status: [read_u64(support, 0x310)?, read_u64(support, 0x318)?],
            descriptor: self.descriptors_high.iova(),
            descriptor_selector: read_u32(descriptor, 0)?,
            descriptor_context: read_u32(descriptor, 0x0c)?,
            descriptor_grid: read_u32(descriptor, 0x0f54)?,
            descriptor_timestamps: [
                read_u64(descriptor, 0x0f8c)?,
                read_u64(descriptor, 0x0f94)?,
            ],
            optional_contexts: [
                read_u64(graph, optional + 0x08)?,
                read_u64(graph, optional + 0x10)?,
            ],
            optional_grid: read_u16(graph, optional + 0x18)?,
            optional_uuid: read_u16(graph, optional + 0x5a)?,
            optional_shared: read_u64(graph, optional + 0x36)?,
            optional_channel: read_u64(graph, optional + 0x4a)?,
            context_record: [
                read_u64(context, 0x200)?,
                read_u64(context, 0x210)?,
                read_u64(context, 0x218)?,
            ],
        };
        core::sync::atomic::fence(core::sync::atomic::Ordering::Acquire);
        Ok(snapshot)
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
/// Name which step of the render-storage build refused a submission.
///
/// `G17PUserRenderStorage::new` has two dozen `EINVAL` sites -- layout
/// derivation plus every descriptor field write -- and they were
/// indistinguishable once propagated to the ioctl. The site number is stable
/// for a given build and maps to a source line in the table kept alongside
/// this function's callers.
fn render_build_fail_named(site: u32, error: g17_render::RenderBuildError) -> kernel::error::Error {
    pr_info!(
        "G17P render storage: BUILD FAIL site={} reason={}\n",
        site,
        error.name()
    );
    EINVAL
}

/// Kernel-owned physical render queue identity. The native allocator chooses
/// IDs per workqueue; the architectural requirement is internal coherence,
/// not copying one captured allocator result. IDs 5/6 keep the application
/// queues clear of the descriptor's distinct QID-0 RCE coordinate and of the
/// retained compute queue at QID 4.
pub(crate) const fn g17p_render_queue_id(tiling: bool) -> u16 {
    if tiling {
        G17P_RENDER_TA_QUEUE_ID
    } else {
        G17P_RENDER_3D_QUEUE_ID
    }
}

/// First-bind flag for a newly allocated render queue. Kext RE sources this
/// from descriptor lifecycle state, not stage identity. Captured native 3D
/// queues can carry zero because the queue already exists; Linux QIDs 0/1 are
/// fresh here and must be installed once. `apply_g17p_retained_optional`
/// clears the flag automatically after submission zero.
pub(crate) fn g17p_render_install(_tiling: bool) -> u16 {
    1
}

/// Convert the validated UAPI fragment write ranges into the exact packed
/// table produced by native `mcacheApertureBufferSetup`.
#[cfg(not(test))]
fn apply_g17p_fragment_mcache(
    attachments: &g17_uapi::UapiAttachmentList,
    fragment_internal_address: u64,
    fragment_internal_size: u64,
    descriptor_low: u64,
    fragment: &mut [u8],
) -> Result<Option<g17_submission::G17PClMcacheAperture>> {
    let hwsid = *crate::module_parameters::g17p_render_mcache_hwsid.value();
    if hwsid == 0 {
        return Ok(None);
    }
    let hwsid = u8::try_from(hwsid).map_err(|_| EINVAL)?;
    let attachments = attachments.as_slice();
    if attachments.is_empty() {
        return Ok(None);
    }
    let mode = *crate::module_parameters::g17p_render_mcache_mode.value();
    if mode > 2
        || (mode == 1 && attachments.len() != 1)
        || (mode == 2 && attachments.len() != 2)
    {
        return Err(EINVAL);
    }
    // The kext takes this from host AGX3DCommandDescriptor +0x154 and encodes
    // it verbatim.  It is not firmware descriptor +0x0c: native witnesses
    // have descriptor context 1 but MCache selector 2 or 3.  Keep the bound
    // root as the compatibility default while the host GART-ID allocator is
    // absent, and allow the native candidate to be selected independently.
    let selector = match *crate::module_parameters::g17p_render_mcache_selector.value() {
        64 => g17_submission::G17P_PARTIAL_OPENING_CONTEXT.render_root_slot,
        value => u8::try_from(value)
            .ok()
            .filter(|value| *value <= 0x3f)
            .ok_or(EINVAL)?,
    };
    let internal_count = if mode == 1 { 1 } else { 0 };
    let count = attachments
        .len()
        .checked_add(internal_count)
        .ok_or(EOVERFLOW)?;
    let table_size = count
        .checked_mul(g17_render::G17P_MCACHE_MAPPING_SIZE)
        .ok_or(EOVERFLOW)?;
    let table_end = g17_render::G17P_FRAGMENT_MCACHE_OFFSET
        .checked_add(table_size)
        .ok_or(EOVERFLOW)?;
    if table_end > fragment.len() {
        return Err(EINVAL);
    }

    let mut encode_range =
        |index: usize, address: u64, size: u64, range_shift: u16| -> Result<()> {
            if size == 0 {
                return Err(EINVAL);
            }
            let start = address & !0x7f;
            let end = address
                .checked_add(size)
                .and_then(|value| value.checked_add(0x7f))
                .map(|value| value & !0x7f)
                .ok_or(EOVERFLOW)?;
            let size_units = u32::try_from((end - start) >> 7).map_err(|_| EOVERFLOW)?;
            let mapping = g17_render::encode_g17p_mcache_mapping(
                g17_render::G17PMcacheMapping {
                    address: start,
                    size_units,
                    hwsid,
                    selector,
                    range_shift,
                },
            )
            .map_err(|_| EINVAL)?;
            let offset = g17_render::G17P_FRAGMENT_MCACHE_OFFSET
                + index * g17_render::G17P_MCACHE_MAPPING_SIZE;
            fragment[offset..offset + mapping.len()].copy_from_slice(&mapping);
            Ok(())
        };

    if mode == 1 {
        // Native always puts a distinct class-2 internal/global write range
        // before its class-1 attachment.  Its stable 0xeeee0000 capture is not
        // mapped by Linux and must not be copied.  The fragment aux-FB arena is
        // the closest safe diagnostic analogue: driver-owned, writable by the
        // GPU, retained with this command, and already mapped in the same
        // client root as the attachment.
        encode_range(0, fragment_internal_address, fragment_internal_size, 2)?;
        pr_info!(
            "G17P render MCache: native-pair hwsid={} selector={} internal={:#x}+{:#x} attachment={:#x}+{:#x} count=2\n",
            hwsid,
            selector,
            fragment_internal_address,
            fragment_internal_size,
            attachments[0].address,
            attachments[0].size,
        );
    }
    for (index, attachment) in attachments.iter().enumerate() {
        // Mode 2 receives the two complete Linux writable resource spans in
        // the same order as Metal's command write-range records. The native
        // 2/1 values are computed sharing exponents, not resource classes.
        let range_shift = if mode == 2 && index == 0 { 2 } else { 1 };
        encode_range(
            internal_count + index,
            attachment.address,
            attachment.size,
            range_shift,
        )?;
    }
    if mode == 2 {
        pr_info!(
            "G17P render MCache: writable-spans hwsid={} selector={} range0={:#x}+{:#x} shift=2 range1={:#x}+{:#x} shift=1 count=2\n",
            hwsid,
            selector,
            attachments[0].address,
            attachments[0].size,
            attachments[1].address,
            attachments[1].size,
        );
    }

    Ok(Some(g17_submission::G17PClMcacheAperture {
        address: descriptor_low
            + g17_render::G17P_FRAGMENT_MCACHE_OFFSET as u64,
        index: 0,
        count: u8::try_from(count).map_err(|_| EOVERFLOW)?,
    }))
}

/// Native G17P PM geometry, joined from `pmblocks-gfx.txt`, `pbdesc2.txt`, and
/// the exact 25G83 AGXG17P kext.
///
/// Each backing is exactly 0x20000 bytes (four 32 KiB pages). The observed
/// 0x28000 address delta is the allocator stride/guard, not usable block size.
const G17P_RENDER_TVB_BLOCK_SIZE: usize = 0x20000;
const G17P_RENDER_TVB_BLOCK_STRIDE: usize = 0x28000;
const G17P_PM_PAGE_SIZE: usize = 0x8000;
const G17P_PM_PAGES_PER_BLOCK: usize = 4;
const G17P_PM_BLOCK_COUNT: usize = 8;
const G17P_PM_MAX_BLOCKS: usize = 0xd1a;
const G17P_PM_MAX_BLOCKS_NOMEMLESS: usize = 0x45e;

const G17P_PM_HWPB_STATE_SIZE: usize = 0xc0;
const G17P_PM_PAGE_LIST_SIZE: usize =
    G17P_PM_MAX_BLOCKS * G17P_PM_PAGES_PER_BLOCK * core::mem::size_of::<u32>();
const G17P_PM_BLOCK_PAGE_BASE_TABLE_SIZE: usize =
    G17P_PM_MAX_BLOCKS * core::mem::size_of::<u64>();
const G17P_PM_SHARED_CONTROL_SIZE: usize = 0x80;
const G17P_PM_COUNTER_SIZE: usize = 4;

const G17P_PM_SCENE_COUNT: usize = 0x50;
const G17P_PM_SCENE_ENTRY_SIZE: usize = 0x80;
const G17P_PM_SCENE_TRAILER_SIZE: usize = 0x40;
const G17P_PM_SCENE_ALLOCATION_SIZE: usize =
    G17P_PM_SCENE_COUNT * G17P_PM_SCENE_ENTRY_SIZE + G17P_PM_SCENE_TRAILER_SIZE;
const G17P_PM_SELECTED_SCENE: usize = 1;
const G17P_PM_PAGE_METRICS_SIZE: usize = G17P_PM_SCENE_COUNT * 4;
/// `DevicePMConfig` gives 36 reusable scene scratch slots.  The G17P kext
/// derives a 0x30-byte stride from the same PM geometry and publishes
/// `base + (scene_index % 36) * 0x30` through Scene +0x28.
const G17P_PM_SCENE_SCRATCH_SLOT_COUNT: usize = 0x24;
const G17P_PM_SCENE_SCRATCH_STRIDE: usize = 0x30;
const G17P_PM_SCENE_SCRATCH_SIZE: usize = 0x8000;
const G17P_PM_DISCARD_SIZE: usize = 0x8000;

/// Firmware event type 7: the firmware asking the host to grow a buffer.
const G17P_EVENT_GROW_REQUEST_TYPE: u32 = 7;

/// Allocate the tiler's parameter buffer in the render state.
/// Send the single-capture witnessed values for TA descriptor bytes `+0x93f`
/// and `+0x94d` (`0x87`/`0x03`) instead of this driver's `0x91`/`0x01`.
///
/// Diagnostic only. See `g17p_render_native_bytes` for why one sighting is not
/// grounds to change the constants outright.
pub(crate) fn g17p_render_native_pm_bytes() -> bool {
    *crate::module_parameters::g17p_render_native_bytes.value() != 0
}

/// Encode the firmware-visible PM graph. Host C++ `AGXParameterBufferBlock`
/// objects are intentionally absent: the kext proves that firmware receives
/// page IDs, a block-page-base table, shared control, Scene entries, and an
/// HWPB state instead.
fn encode_g17p_parameter_management(
    raw: &mut [u8],
    high_va: u64,
    page_list_vas: [u64; 2],
    metrics_vas: [u64; 2],
    tvb_va: u64,
    scene_scratch_va: u64,
    discard_va: u64,
    ta_hardware_buffer_id: u32,
    render_layout: &G17PRenderStateLayout,
    pm_layout: &G17PParameterManagementLayout,
) -> Result {
    if raw.len() < pm_layout.total_size
        || render_layout.tvb_blocks < G17P_PM_BLOCK_COUNT
        || render_layout.tvb_blocks > G17P_PM_MAX_BLOCKS
    {
        return Err(EINVAL);
    }

    raw.fill(0);
    let tvb_end = tvb_va
        .checked_add(render_layout.tvb_size as u64)
        .ok_or(EINVAL)?;
    if tvb_end > G17P_RENDER_USER_END {
        return Err(EINVAL);
    }
    for index in 0..render_layout.tvb_blocks {
        // AGXParameterManagementVirtual::prepareParameterBufferBlock stores
        // `(gpu_va >> 15) & 0x01ff_ffff` verbatim.  Unlike Scene +0x28 and the
        // discard pointer below, a PB page ID retains the 0x1000000000 GART
        // segment as bit 21.  Subtracting G17P_RENDER_USER_BASE here made the
        // page list name the same offsets in the wrong address class.
        let backing = tvb_va
            .checked_add(
                (index
                    .checked_mul(G17P_RENDER_TVB_BLOCK_STRIDE)
                    .ok_or(EINVAL)?) as u64,
            )
            .ok_or(EINVAL)?;
        let first_page = ((backing >> 15) & 0x01ff_ffff) as u32;
        put_u32(raw, pm_layout.block_page_bases + index * 8, first_page);
        for page in 0..G17P_PM_PAGES_PER_BLOCK {
            let slot = index * G17P_PM_PAGES_PER_BLOCK + page;
            put_u32(raw, pm_layout.page_list + slot * 4, first_page + page as u32);
        }
    }

    let [metrics_high, metrics_low] = metrics_vas;
    // Scene +0x28 is a compact user/GART pointer. Fresh native traces show
    // 0x1000178000 published as 0x178000, independently of the 0x7000000000
    // resource domain used by tilemap/TPC/heapmeta.
    let scene_scratch = scene_scratch_va
        .checked_sub(G17P_RENDER_USER_BASE)
        .ok_or(EINVAL)?;
    if scene_scratch > u32::MAX as u64 {
        return Err(EINVAL);
    }
    let stats = high_va
        .checked_add((pm_layout.shared_control + 0x40) as u64)
        .ok_or(EINVAL)?;
    for index in 0..G17P_PM_SCENE_COUNT {
        let entry = pm_layout.scene_states + index * G17P_PM_SCENE_ENTRY_SIZE;
        put_u64(raw, entry, metrics_high + (index * 4) as u64);
        put_u64(raw, entry + 0x08, metrics_low + (index * 4) as u64);
        put_u64(
            raw,
            entry + 0x28,
            scene_scratch
                + ((index % G17P_PM_SCENE_SCRATCH_SLOT_COUNT)
                    * G17P_PM_SCENE_SCRATCH_STRIDE) as u64,
        );
        put_u64(raw, entry + 0x40, stats);
    }
    // Native has no host store to selected Scene +0x4c, so leave the fresh
    // shared allocation's zero there. TA owns the adjacent handoff/accounting
    // words.

    // BlockControl { total, committed, firmware_cursor }, then Stats in the
    // second 0x40-byte half. Native reset leaves the Stats words zero and sets
    // reset=1 at Stats+0x20.
    put_u32(
        raw,
        pm_layout.shared_control,
        render_layout.tvb_blocks as u32,
    );
    put_u32(
        raw,
        pm_layout.shared_control + 0x04,
        render_layout.tvb_blocks as u32,
    );
    put_u32(raw, pm_layout.shared_control + 0x08, 0);
    put_u32(raw, pm_layout.shared_control + 0x60, 1);

    let page_count = render_layout
        .tvb_blocks
        .checked_mul(G17P_PM_PAGES_PER_BLOCK)
        .ok_or(EINVAL)? as u32;
    let discard = discard_va
        .checked_sub(G17P_RENDER_USER_BASE)
        .ok_or(EINVAL)?;
    if discard > u32::MAX as u64 {
        return Err(EINVAL);
    }
    let state = pm_layout.hwpb_state;
    // AGXParameterManagement::loadPBDesc copies the command's allocated
    // HardwareBufferID to `_AGFIParameterBufferState +0x0c` immediately before
    // invoking halLoadPBDesc. This is one of five sinks of the same ID carried
    // by TA command +0x18, register 0x1c830, ConfigUpdate +0x7e, and the PBDesc
    // ring index returned by `pb_descriptor()` below.
    put_u32(raw, state + 0x0c, ta_hardware_buffer_id);
    // Native's PB page list is one retained backing with a firmware-high view
    // and an app-GART view.  The latter is the captured 0x1000190000 mapping;
    // it is not a context-0 0x700... alias of this per-submit PM aggregate.
    put_u64(raw, state + 0x20, page_list_vas[0]);
    put_u64(raw, state + 0x28, page_list_vas[1]);
    put_u32(raw, state + 0x30, page_align(G17P_PM_PAGE_LIST_SIZE).ok_or(EINVAL)? as u32);
    put_u32(raw, state + 0x34, page_count);
    put_u32(raw, state + 0x38, G17P_PM_MAX_BLOCKS as u32);
    put_u32(raw, state + 0x3c, render_layout.tvb_blocks as u32);
    put_u32(raw, state + 0x40, 0);
    put_u64(raw, state + 0x44, high_va + pm_layout.block_page_bases as u64);
    put_u64(raw, state + 0x4c, high_va + pm_layout.shared_control as u64);
    put_u32(raw, state + 0x54, page_count.checked_sub(1).ok_or(EINVAL)?);
    put_u32(raw, state + 0x58, G17P_RENDER_TVB_BLOCK_SIZE as u32);
    put_u64(raw, state + 0x64, high_va + pm_layout.counter as u64);
    put_u32(
        raw,
        state + 0x7c,
        (G17P_PM_MAX_BLOCKS * core::mem::size_of::<u32>()) as u32,
    );
    put_u32(
        raw,
        state + 0x80,
        (G17P_PM_MAX_BLOCKS_NOMEMLESS * core::mem::size_of::<u32>()) as u32,
    );
    put_u64(raw, state + 0x84, discard);
    Ok(())
}

#[cfg(not(test))]
struct G17PParameterManagement {
    backing: MappedObject,
    layout: G17PParameterManagementLayout,
    page_count: u32,
    page_list_vas: [u64; 2],
    metrics_vas: [u64; 2],
    hardware_buffer_id: u32,
}

#[cfg(not(test))]
impl G17PParameterManagement {
    fn new(
        dev: &AsahiDevice,
        uat: &mmu::Uat,
        page_list_vas: [u64; 2],
        metrics_vas: [u64; 2],
        tvb_va: u64,
        scene_scratch_va: u64,
        discard_va: u64,
        ta_hardware_buffer_id: u32,
        render_layout: &G17PRenderStateLayout,
    ) -> Result<Self> {
        let layout = g17p_parameter_management_layout().ok_or(EINVAL)?;
        // Native uses `[firmware-high, primary-low]`, but hardware proved that
        // the firmware CPU's primary GART is not Linux's UAT TTBR0: runtime,
        // dual-context, exact-VA, and pre-ASC mappings all faulted at B1
        // `0xbba0`. B1 performs only an ordinary writable u32 load/store here,
        // so use the same uncached firmware-high alias for Scene +0 and +8.
        let metrics_vas = [metrics_vas[0], metrics_vas[0]];
        let mut backing = MappedObject::new_wc(
            dev,
            uat,
            layout.total_size,
            G17P_PM_PAGE_SIZE as u64,
            mmu::PROT_GPU_FW_SHARED_RW,
        )?;
        let high_va = backing.iova();
        backing.with_bytes_mut(|raw| {
            encode_g17p_parameter_management(
                raw,
                high_va,
                page_list_vas,
                metrics_vas,
                tvb_va,
                scene_scratch_va,
                discard_va,
                ta_hardware_buffer_id,
                render_layout,
                &layout,
            )
        })?;
        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
        Ok(Self {
            backing,
            layout,
            page_count: (render_layout.tvb_blocks * G17P_PM_PAGES_PER_BLOCK) as u32,
            page_list_vas,
            metrics_vas,
            hardware_buffer_id: ta_hardware_buffer_id,
        })
    }

    fn selected_scene_va(&self) -> u64 {
        self.backing.iova()
            + self.layout.scene_states as u64
            + (G17P_PM_SELECTED_SCENE * G17P_PM_SCENE_ENTRY_SIZE) as u64
    }

    fn trailer_va(&self) -> u64 {
        self.backing.iova()
            + self.layout.scene_states as u64
            + (G17P_PM_SCENE_COUNT * G17P_PM_SCENE_ENTRY_SIZE) as u64
    }

    fn hwpb_state_va(&self) -> u64 {
        self.backing.iova() + self.layout.hwpb_state as u64
    }

    fn page_list_low_va(&self) -> u64 {
        self.page_list_vas[1]
    }

    fn page_metrics_cpu_va(&self) -> u64 {
        self.metrics_vas[1]
    }

    fn pb_descriptor(&self) -> Result<(u32, u64, u32)> {
        // halLoadPBDesc consumes PBState +0x28 verbatim. Native passes the
        // full tagged 0x1000190000 address, not 0x190000. The descriptor's
        // segment nibble is biased by +0x1000000000 when hardware rebuilds
        // the address: passing our old stripped 0x2478000 therefore produced
        // the measured fault at 0x8002478000. Passing the full
        // 0x7002478000 selects segment 0x60, which hardware rebuilds as the
        // intended 0x7002478000 mapping.
        Ok((
            self.hardware_buffer_id,
            self.page_list_low_va(),
            self.page_count,
        ))
    }

    fn copy_page_list_to(&mut self, target: &mut MappedObject) -> Result {
        if target.logical_size < G17P_PM_PAGE_LIST_SIZE {
            return Err(ERANGE);
        }
        let start = self.layout.page_list;
        let end = start
            .checked_add(G17P_PM_PAGE_LIST_SIZE)
            .ok_or(EOVERFLOW)?;
        self.backing.with_bytes_mut(|source| {
            let page_list = source.get(start..end).ok_or(ERANGE)?;
            target.with_bytes_mut(|destination| {
                destination
                    .get_mut(..G17P_PM_PAGE_LIST_SIZE)
                    .ok_or(ERANGE)?
                    .copy_from_slice(page_list);
                Ok(())
            })
        })?;
        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
        Ok(())
    }
}

#[cfg(not(test))]
fn map_g17p_render_tvb_hardware_alias(
    dev: &AsahiDevice,
    vm: &mmu::Vm,
    tvb: &mut MappedObject,
) -> Result<mmu::KernelMapping> {
    let logical_va = tvb.iova();
    let logical_end = logical_va
        .checked_add(tvb.logical_size as u64)
        .ok_or(EOVERFLOW)?;
    if logical_va < G17P_RENDER_USER_TVB_START
        || logical_end > G17P_RENDER_USER_TVB_END
        || tvb.logical_size == 0
    {
        return Err(EINVAL);
    }

    // AGXParameterManagementVirtual::prepareParameterBufferBlock publishes
    // `(getGPUAddress() >> 15) & 0x01ff_ffff`.  Live G17P fault requestors
    // prove that VCE/dPM then add the 0x1000000000 compact-GART base before
    // their first PB write: ID 0x200420 faults at 0x2002100000, not at the
    // logical 0x1002100000 mapping from which the kext derived that ID.
    let hardware_va = G17P_RENDER_USER_BASE
        .checked_add(logical_va)
        .ok_or(EOVERFLOW)?;
    let alias = tvb.map_alias_at(vm, hardware_va, mmu::PROT_GPU_FW_SHARED_RW)?;
    if alias.iova() != hardware_va
        || alias.size() < tvb.logical_size
        || !vm.covers_range(hardware_va, tvb.logical_size as u64, true, true)
    {
        return Err(EFAULT);
    }

    let mut offset = 0usize;
    let mut first_pa = 0u64;
    let mut last_pa = 0u64;
    while offset < tvb.logical_size {
        let offset_u64 = offset as u64;
        let logical_pa = vm.translate_iova(logical_va + offset_u64)?;
        let hardware_pa = vm.translate_iova(hardware_va + offset_u64)?;
        if logical_pa != hardware_pa {
            dev_err!(
                dev.as_ref(),
                "G17P render TVB: logical/hardware aliases disagree at +{:#x}: {:#x}->{:#x}, {:#x}->{:#x}\n",
                offset,
                logical_va + offset_u64,
                logical_pa,
                hardware_va + offset_u64,
                hardware_pa,
            );
            return Err(EFAULT);
        }
        if offset == 0 {
            first_pa = logical_pa;
        }
        last_pa = logical_pa;
        offset = offset.checked_add(mmu::UAT_PGSZ).ok_or(EOVERFLOW)?;
    }
    dev_info!(
        dev.as_ref(),
        "G17P render TVB: client logical={:#x} hardware={:#x} size={:#x} backing-pa=[{:#x},{:#x}]\n",
        logical_va,
        hardware_va,
        tvb.logical_size,
        first_pa,
        last_pa,
    );
    Ok(alias)
}

/// 0: off. 1: reserve the storage only. 2: reserve, populate the block
/// management graph, and bind the selected Scene entry/trailer into both
/// command records.
pub(crate) fn g17p_render_tvb_mode() -> u32 {
    *crate::module_parameters::g17p_render_tvb.value()
}

pub(crate) fn g17p_render_tvb_enabled() -> bool {
    *crate::module_parameters::g17p_render_tvb.value() != 0
}

fn g17p_render_cycle_pointer(scene_scratch_va: u64) -> Result<u64> {
    scene_scratch_va
        .checked_sub(G17P_RENDER_USER_BASE)
        .and_then(|offset| offset.checked_add(0x20))
        .ok_or(EINVAL)
}

pub(crate) fn g17p_render_generation_bias_enabled() -> bool {
    *crate::module_parameters::g17p_render_generation_bias.value() != 0
}

pub(crate) fn g17p_render_tag16_enabled() -> bool {
    *crate::module_parameters::g17p_render_tag16.value() != 0
}

pub(crate) fn g17p_render_native_pool_a_first_record_enabled() -> bool {
    *crate::module_parameters::g17p_render_native_pool_a_first_record.value() != 0
}

pub(crate) fn g17p_render_native_fragment_hwpb_mode() -> u32 {
    *crate::module_parameters::g17p_render_native_fragment_hwpb.value()
}

pub(crate) fn g17p_render_native_fragment_hwpb_enabled() -> bool {
    g17p_render_native_fragment_hwpb_mode() != 0
}

/// Ring items one render queue publishes per submission.
pub(crate) fn g17p_render_published_prefix() -> u32 {
    if g17p_render_tag16_enabled() {
        4
    } else {
        3
    }
}

fn render_build_fail(site: u32) -> kernel::error::Error {
    pr_info!("G17P render storage: BUILD FAIL site={}\n", site);
    EINVAL
}

impl G17PUserRenderStorage {
    /// Same, for the descriptor object the kick entry names at +0x10.
    pub(crate) fn dump_descriptor_region(
        &mut self,
        dev: &AsahiDevice,
        who: &str,
        label: &str,
        offset: usize,
        length: usize,
    ) {
        let Ok(vmap) = self.descriptors.object.vmap() else {
            return;
        };
        let bytes = unsafe {
            // SAFETY: the VMap covers the page-rounded descriptor object; the
            // slice below is bounds-checked against logical_size.
            core::slice::from_raw_parts(vmap.as_ptr(), self.descriptors.logical_size)
        };
        if offset + length > bytes.len() {
            return;
        }
        for row in 0..(length / 16) {
            let at = offset + row * 16;
            dev_info!(
                dev.as_ref(),
                "G17PDUMP {} {} {:#05x} {:02x?}\n",
                who,
                label,
                row * 16,
                &bytes[at..at + 16],
            );
        }
    }

    /// Machine-parseable dump of any graph region, for byte-diffing against
    /// the native capture. Lines are `G17PDUMP <who> <label> <off> <bytes>`.
    pub(crate) fn dump_graph_region(
        &mut self,
        dev: &AsahiDevice,
        who: &str,
        label: &str,
        offset: usize,
        length: usize,
    ) {
        let Ok(vmap) = self.graph.object.vmap() else {
            return;
        };
        let bytes = unsafe {
            // SAFETY: the VMap covers the page-rounded graph object; the slice
            // below is bounds-checked against logical_size.
            core::slice::from_raw_parts(vmap.as_ptr(), self.graph.logical_size)
        };
        if offset + length > bytes.len() {
            return;
        }
        for row in 0..(length / 16) {
            let at = offset + row * 16;
            dev_info!(
                dev.as_ref(),
                "G17PDUMP {} {} {:#05x} {:02x?}\n",
                who,
                label,
                row * 16,
                &bytes[at..at + 16],
            );
        }
    }

    /// Dump one render queue record, to diff against the native capture.
    ///
    /// macOS 2026-09-02 (`fullrecords-gfx.txt`) gives the same object for a
    /// real Metal render pass, so a byte diff of this against native's is the
    /// direct answer to "what is missing from the record the KSM reads".
    pub(crate) fn log_render_queue_record(&mut self, dev: &AsahiDevice, tiling: bool, label: &str) {
        let offset = if tiling {
            Self::TILING_QUEUE
        } else {
            Self::FRAGMENT_QUEUE
        };
        let mut words = [0u32; G17P_RENDER_QUEUE_RECORD_SIZE / 4];
        let mut ok = false;
        if let Ok(vmap) = self.graph.object.vmap() {
            let bytes = unsafe {
                // SAFETY: the VMap covers the page-rounded graph object and
                // every index below is bounds-checked against it.
                core::slice::from_raw_parts(vmap.as_ptr(), self.graph.logical_size)
            };
            for (index, slot) in words.iter_mut().enumerate() {
                let at = offset + index * 4;
                if at + 4 <= bytes.len() {
                    *slot =
                        u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]]);
                    ok = true;
                }
            }
        }
        if !ok {
            return;
        }
        for row in 0..(G17P_RENDER_QUEUE_RECORD_SIZE / 32) {
            let base = row * 8;
            dev_info!(
                dev.as_ref(),
                "G17P render queue-record[{}] {} +{:#05x}: {:#010x} {:#010x} {:#010x} {:#010x} {:#010x} {:#010x} {:#010x} {:#010x}\n",
                label,
                if tiling { "TA" } else { "3D" },
                row * 32,
                words[base],
                words[base + 1],
                words[base + 2],
                words[base + 3],
                words[base + 4],
                words[base + 5],
                words[base + 6],
                words[base + 7],
            );
        }
    }

    // The per-queue record is addressed by QUEUE ID, not by stage index.
    // MEASURED ON macOS 2026-09-02: a native render's data-master ring slots
    // name records at 0xfffffc20c0000540 for its TA (queue 7) and
    // 0xfffffc20c0000600 for its 3D (queue 8) -- exactly base + qid * 0xc0
    // in both cases, and the same addresses appear again in each kick entry's
    // payload[1].  This driver placed them at index 0 and 1 instead, so every
    // pointer it handed the firmware named a record 5 and 5 slots short of
    // where a queue-id lookup would find one.  Keep our own base, index it the
    // way native does.
    const TILING_QUEUE: usize = G17P_RENDER_TA_QUEUE_ID as usize * G17P_RENDER_QUEUE_RECORD_SIZE;
    const FRAGMENT_QUEUE: usize =
        G17P_RENDER_3D_QUEUE_ID as usize * G17P_RENDER_QUEUE_RECORD_SIZE;
    const TILING_POINTERS: usize = G17P_USER_TILING_POINTERS;
    const FRAGMENT_POINTERS: usize = 0x1080;
    const JOB_LIST: usize = 0x2000;
    const FRAGMENT_OPTIONAL: usize = 0x30c0;
    const TILING_EVENT: usize = G17P_USER_TILING_EVENT;
    const FRAGMENT_EVENT: usize = 0x3600;
    const TILING_RING: usize = G17P_USER_TILING_RING;
    const FRAGMENT_RING: usize = 0x8000;
    const FRAGMENT_ENTRY_SIGNAL: usize = G17P_USER_FRAGMENT_ENTRY_SIGNAL;

    pub(crate) fn parameter_buffer_descriptor(&self) -> Result<Option<(u32, u64, u32)>> {
        self.parameter_management
            .as_ref()
            .map(|pm| pm.pb_descriptor())
            .transpose()
    }

    pub(crate) fn fragment_mcache(&self) -> Option<g17_submission::G17PClMcacheAperture> {
        self.fragment_mcache
    }

    pub(crate) fn descriptor_low_va(&self, tiling: bool) -> u64 {
        if tiling {
            G17P_BOOTSTRAP_TA_DESCRIPTOR_LOW_VA
        } else {
            self.descriptor_3d_render.iova()
        }
    }

    pub(crate) fn fragment_rce_base(&self) -> u64 {
        self.fragment_rce.iova()
    }

    fn new(
        dev: &AsahiDevice,
        uat: &mmu::Uat,
        parameter_page_list_vas: [u64; 2],
        parameter_metrics_vas: [u64; 2],
        ta_context_peer: &mut MappedObject,
        fragment_context_peer: &mut MappedObject,
        vm: &mmu::Vm,
        command: &g17_uapi::TranslatedRenderCommand,
        num_clusters: u32,
        owner_pid: u32,
        user_timestamps: [u64; 4],
        user_timestamp_aliases: KVec<mmu::KernelMapping>,
        command_context: mmu::T8140AppContextLease,
    ) -> Result<Self> {
        macro_rules! bootstrap_stage {
            ($name:literal, $expression:expr) => {
                match $expression {
                    Ok(value) => value,
                    Err(error) => {
                        dev_err!(
                            dev.as_ref(),
                            "G17P render storage: {} failed ({:?})\n",
                            $name,
                            error
                        );
                        return Err(error);
                    }
                }
            };
        }

        let command_context_id = command_context.context_id();

        // The render context peers ARE the two render queues' SKSM entry
        // regions: their low VAs are what the render tag-15 ConfigUpdate
        // publishes at +0x08. An SKSM entry ring is not a pure firmware-read
        // surface -- AGXKSMKickQueue::init maps the client view GPU
        // read-WRITE, and compute's equivalent region carries
        // PROT_GPU_SHARED_RW for exactly that reason. Flip both peers to RW
        // when the render SKSM half is armed; keep the historical read-only
        // parity when it is not, so one build tests both paths.
        let render_peer_prot = if *crate::module_parameters::g17p_render_sksm.value() != 0 {
            mmu::PROT_GPU_SHARED_RW
        } else {
            mmu::PROT_GPU_SHARED_RO
        };
        let ta_context_low = match ta_context_peer.map_alias_at(
            uat.kernel_lower_vm(),
            G17P_BOOTSTRAP_TA_CONTEXT_LOW_VA,
            render_peer_prot,
        ) {
            Ok(mapping) => mapping,
            Err(error) => {
                dev_err!(
                    dev.as_ref(),
                    "G17P render storage: TA context low alias at {:#x} failed ({:?}), occupied pages={:#x}\n",
                    G17P_BOOTSTRAP_TA_CONTEXT_LOW_VA,
                    error,
                    mapped_page_mask(
                        uat.kernel_lower_vm(),
                        G17P_BOOTSTRAP_TA_CONTEXT_LOW_VA,
                        G17P_BOOTSTRAP_CONTEXT_PEER_SIZE,
                    )
                );
                return Err(error);
            }
        };
        let fragment_context_low = match fragment_context_peer.map_alias_at(
            uat.kernel_lower_vm(),
            G17P_BOOTSTRAP_3D_CONTEXT_LOW_VA,
            render_peer_prot,
        ) {
            Ok(mapping) => mapping,
            Err(error) => {
                dev_err!(
                    dev.as_ref(),
                    "G17P render storage: 3D context low alias at {:#x} failed ({:?}), occupied pages={:#x}\n",
                    G17P_BOOTSTRAP_3D_CONTEXT_LOW_VA,
                    error,
                    mapped_page_mask(
                        uat.kernel_lower_vm(),
                        G17P_BOOTSTRAP_3D_CONTEXT_LOW_VA,
                        G17P_BOOTSTRAP_CONTEXT_PEER_SIZE,
                    )
                );
                return Err(error);
            }
        };

        // Queue items name the firmware-high descriptors, while descriptor
        // self-pointers resolve through these one-page context-0 aliases.
        // The output-positive first-partial path names the descriptors at
        // exactly the native arena bases (TA 0xfffffc20c0018000, 3D
        // 0xfffffc20c00b0000), so alias the two pages there and keep the
        // context locators at their native, delta-zero values.
        let mut descriptors = bootstrap_stage!("descriptor allocation", MappedObject::new_wc(
            dev,
            uat,
            G17P_RENDER_DESCRIPTOR_STORAGE_SIZE,
            mmu::UAT_PGSZ as u64,
            mmu::PROT_GPU_FW_SHARED_RW,
        ));
        let descriptor_ta_high = bootstrap_stage!(
            "TA descriptor high alias",
            descriptors.object.map_range_into_range(
                uat.kernel_vm(),
                0..mmu::UAT_PGSZ,
                g17_submission::G17P_COLD_OPENING_TA_DESCRIPTOR_BASE
                    ..g17_submission::G17P_COLD_OPENING_TA_DESCRIPTOR_BASE
                        + mmu::UAT_PGSZ as u64,
                mmu::UAT_PGSZ as u64,
                mmu::PROT_GPU_FW_SHARED_RW,
                false,
            )
        );
        let descriptor_3d_high = bootstrap_stage!(
            "3D descriptor high alias",
            descriptors.object.map_range_into_range(
                uat.kernel_vm(),
                mmu::UAT_PGSZ..2 * mmu::UAT_PGSZ,
                g17_submission::G17P_COLD_OPENING_3D_DESCRIPTOR_BASE
                    ..g17_submission::G17P_COLD_OPENING_3D_DESCRIPTOR_BASE
                        + mmu::UAT_PGSZ as u64,
                mmu::UAT_PGSZ as u64,
                mmu::PROT_GPU_FW_SHARED_RW,
                false,
            )
        );
        dev_info!(
            dev.as_ref(),
            "G17P render storage: descriptor pages at native bases {:#x}/{:#x}\n",
            descriptor_ta_high.iova(),
            descriptor_3d_high.iova()
        );
        let descriptor_ta_low = bootstrap_stage!(
            "TA descriptor low alias",
            descriptors.object.map_range_into_range(
                uat.kernel_lower_vm(),
                0..mmu::UAT_PGSZ,
                G17P_BOOTSTRAP_TA_DESCRIPTOR_LOW_VA
                    ..G17P_BOOTSTRAP_TA_DESCRIPTOR_LOW_VA + mmu::UAT_PGSZ as u64,
                mmu::UAT_PGSZ as u64,
                mmu::PROT_GPU_SHARED_RO,
                false,
            )
        );
        let descriptor_3d_low = bootstrap_stage!(
            "3D descriptor low alias",
            descriptors.object.map_range_into_range(
                uat.kernel_lower_vm(),
                mmu::UAT_PGSZ..2 * mmu::UAT_PGSZ,
                G17P_BOOTSTRAP_3D_DESCRIPTOR_LOW_VA
                    ..G17P_BOOTSTRAP_3D_DESCRIPTOR_LOW_VA + mmu::UAT_PGSZ as u64,
                mmu::UAT_PGSZ as u64,
                mmu::PROT_GPU_SHARED_RO,
                false,
            )
        );
        let mut graph = bootstrap_stage!("queue allocation", MappedObject::new_wc(
            dev,
            uat,
            G17P_RENDER_QUEUE_GRAPH_SIZE,
            mmu::UAT_PGSZ as u64,
            mmu::PROT_GPU_FW_SHARED_RW,
        ));
        let mut support = bootstrap_stage!("support allocation", MappedObject::new_wc(
            dev,
            uat,
            G17P_RENDER_SUPPORT_SIZE,
            mmu::UAT_PGSZ as u64,
            mmu::PROT_GPU_FW_SHARED_RW,
        ));
        let render_layout = bootstrap_stage!(
            "state layout",
            g17p_render_state_layout(&command.parameters, num_clusters).ok_or_else(|| render_build_fail(1))
        );
        let mut render_state = bootstrap_stage!("state allocation", MappedObject::new_wc_in_range(
            dev,
            vm,
            g17p_cl_dynamic_low_va_range(),
            render_layout.total_size,
            if g17p_render_tvb_enabled() {
                G17P_PM_PAGE_SIZE as u64
            } else {
                mmu::UAT_PGSZ as u64
            },
            mmu::PROT_GPU_FW_SHARED_RW,
        ));
        let mut render_tvb = if g17p_render_tvb_enabled() {
            Some(bootstrap_stage!("TVB GART allocation", MappedObject::new_wc_in_range(
                dev,
                vm,
                G17P_RENDER_USER_TVB_START..G17P_RENDER_USER_TVB_END,
                render_layout.tvb_size,
                G17P_PM_PAGE_SIZE as u64,
                mmu::PROT_GPU_FW_SHARED_RW,
            )))
        } else {
            None
        };
        if let Some(tvb) = render_tvb.as_mut() {
            bootstrap_stage!("TVB initialization", tvb.with_bytes_mut(|raw| {
                raw.fill(0);
                Ok(())
            }));
        }
        let render_tvb_hardware_alias = if let Some(tvb) = render_tvb.as_mut() {
            Some(bootstrap_stage!(
                "TVB hardware alias",
                map_g17p_render_tvb_hardware_alias(dev, vm, tvb)
            ))
        } else {
            None
        };
        dev_info!(
            dev.as_ref(),
            "G17P render aliases: arena {:#x}..{:#x} occupancy={:#018x}\n",
            G17P_RENDER_USER_ALIAS_START,
            G17P_RENDER_USER_ALIAS_END,
            mapped_page_mask(
                vm,
                G17P_RENDER_USER_ALIAS_START,
                (G17P_RENDER_USER_ALIAS_END - G17P_RENDER_USER_ALIAS_START) as usize,
            ),
        );
        let render_user_aliases = bootstrap_stage!(
            "compact render-state aliases",
            G17PRenderUserAliases::new(
                &mut render_state,
                vm,
                &render_layout,
                0,
                g17p_render_tvb_mode() >= 2,
            )
        );
        // Both render bootstrap status pages land in the same high window the
        // compute/control state uses -- G17P_BOOTSTRAP_3D_STATUS_VA is bit-for-bit
        // COMPUTE_RUNTIME_STATE_ADDRESS (0xfffffc20_0163_0000). Report which pages
        // are already taken so an ENOSPC here names the collision instead of just
        // failing.
        // 64-page occupancy window over the region both status pages live in, so
        // a free relocation target can be chosen from evidence rather than guessed.
        dev_info!(
            dev.as_ref(),
            "G17P render storage: window {:#x} +64pg occupancy={:#018x} (pgsz={:#x})\n",
            0xffff_fc20_0160_0000u64,
            mapped_page_mask(uat.kernel_vm(), 0xffff_fc20_0160_0000, mmu::UAT_PGSZ * 64),
            mmu::UAT_PGSZ,
        );
        dev_info!(
            dev.as_ref(),
            "G17P render storage: status page occupancy TA {:#x}=>{:#x} 3D {:#x}=>{:#x}\n",
            G17P_BOOTSTRAP_TA_STATUS_VA,
            mapped_page_mask(uat.kernel_vm(), G17P_BOOTSTRAP_TA_STATUS_VA, mmu::UAT_PGSZ),
            G17P_BOOTSTRAP_3D_STATUS_VA,
            mapped_page_mask(uat.kernel_vm(), G17P_BOOTSTRAP_3D_STATUS_VA, mmu::UAT_PGSZ),
        );
        let render_ta_status = bootstrap_stage!(
            "TA status high alias",
            render_state.object.map_range_into_range(
                uat.kernel_vm(),
                G17P_RENDER_STATE_TA_STATUS..G17P_RENDER_STATE_TA_STATUS + mmu::UAT_PGSZ,
                G17P_BOOTSTRAP_TA_STATUS_VA
                    ..G17P_BOOTSTRAP_TA_STATUS_VA + mmu::UAT_PGSZ as u64,
                mmu::UAT_PGSZ as u64,
                mmu::PROT_GPU_FW_SHARED_RW,
                false,
            )
        );
        let render_3d_status = bootstrap_stage!(
            "3D status high alias",
            render_state.object.map_range_into_range(
                uat.kernel_vm(),
                G17P_RENDER_STATE_FRAGMENT_STATUS
                    ..G17P_RENDER_STATE_FRAGMENT_STATUS + mmu::UAT_PGSZ,
                G17P_BOOTSTRAP_3D_STATUS_VA
                    ..G17P_BOOTSTRAP_3D_STATUS_VA + mmu::UAT_PGSZ as u64,
                mmu::UAT_PGSZ as u64,
                mmu::PROT_GPU_FW_SHARED_RW,
                false,
            )
        );
        // The descriptor's private +0x8fe/+0x906 and +0x2198/+0x21a0
        // timestamp pointers are dereferenced directly by the firmware CPU
        // after TA/3D completion. Native uses firmware-high addresses here;
        // the adjacent +0x90e/+0x916 user timestamp pointers are the separate
        // client-VM destinations. A client-low private timestamp page lets TA
        // finish, then crashes firmware at B1 +0xc190 when it publishes the
        // result through a root that cannot see the client mapping.
        let timestamps = bootstrap_stage!("firmware timestamp allocation", MappedObject::new_wc(
            dev,
            uat,
            mmu::UAT_PGSZ,
            mmu::UAT_PGSZ as u64,
            mmu::PROT_GPU_FW_SHARED_RW,
        ));
        let support_va = support.iova();
        let render_state_va = render_state.iova();
        let timestamp_va = timestamps.iova();
        let mut translated = *command;
        let bootstrap_vdm = translated.parameters.encoder == G17P_BOOTSTRAP_VDM_VA;
        translated.parameters.context_base = G17P_RENDER_USER_BASE;
        translated.parameters.usc_flist_hardware_buffer_id =
            g17_initdata::PARTIAL_OPENING_FREELIST_HARDWARE_BUFFER_ID;
        let parameter_management = if g17p_render_tvb_mode() >= 2 {
            let scene_scratch_va = render_user_aliases
                .scene_scratch_va()
                .ok_or_else(|| render_build_fail(2))?;
            let discard_va = render_user_aliases
                .discard_va()
                .ok_or_else(|| render_build_fail(2))?;
            let tvb_va = render_tvb
                .as_ref()
                .map(MappedObject::iova)
                .ok_or_else(|| render_build_fail(2))?;
            Some(bootstrap_stage!(
                "parameter-management allocation",
                G17PParameterManagement::new(
                    dev,
                    uat,
                    parameter_page_list_vas,
                    parameter_metrics_vas,
                    tvb_va,
                    scene_scratch_va,
                    discard_va,
                    translated.parameters.ta_hardware_buffer_id,
                    &render_layout,
                )
            ))
        } else {
            None
        };
        if let Some(pm) = parameter_management.as_ref() {
            dev_info!(
                dev.as_ref(),
                "G17P render PM: scene={:#x} trailer={:#x} hwpb={:#x} page-list-low={:#x} metrics-cpu={:#x}\n",
                pm.selected_scene_va(),
                pm.trailer_va(),
                pm.hwpb_state_va(),
                pm.page_list_low_va(),
                pm.page_metrics_cpu_va(),
            );
        }
        translated.parameters.absolute_pointers =
            *crate::module_parameters::g17p_render_absolute_pointers.value() != 0;
        translated.parameters.deflake_3 = render_user_aliases.deflake_va();
        translated.parameters.deflake_2 = translated.parameters.deflake_3 + 0x20;
        translated.parameters.deflake_1 = translated.parameters.deflake_3 + 0x2a0;
        if let Some(scene_scratch_va) = render_user_aliases.scene_scratch_va() {
            translated.parameters.cycle = g17p_render_cycle_pointer(scene_scratch_va)?;
        }
        translated.parameters.ta_status = render_state_va + G17P_RENDER_STATE_TA_STATUS as u64;
        translated.parameters.fragment_status =
            render_state_va + G17P_RENDER_STATE_FRAGMENT_STATUS as u64;
        translated.parameters.heapmeta = render_user_aliases.heapmeta_va(&render_layout)?;
        translated.parameters.tpc = render_user_aliases.tpc_va();
        translated.parameters.aux_fb = render_user_aliases.aux_fb_va();
        translated.parameters.aux_fb_page_count = G17P_RENDER_AUX_FB_PAGE_COUNT;
        translated.parameters.aux_fb_flags = g17p_aux_fb_flags(translated.flags);
        translated.parameters.tilemap = render_user_aliases.tilemap_va();
        if let Some(pm) = parameter_management.as_ref() {
            translated.parameters.parameter_buffer = pm.selected_scene_va();
        }
        dev_info!(
            dev.as_ref(),
            "G17P render address domains: resource-base={:#x} state={:#x} deflake={:#x} scene={:#x} discard={:#x} tilemap={:#x} heapmeta={:#x} tpc={:#x} axfb={:#x} tvb={:#x}\n",
            translated.parameters.context_base,
            render_state_va,
            render_user_aliases.deflake_va(),
            render_user_aliases.scene_scratch_va().unwrap_or(0),
            render_user_aliases.discard_va().unwrap_or(0),
            translated.parameters.tilemap,
            translated.parameters.heapmeta,
            translated.parameters.tpc,
            translated.parameters.aux_fb,
            render_tvb.as_ref().map_or(0, MappedObject::iova),
        );
        translated.parameters.ta_timestamp_start = timestamp_va;
        translated.parameters.ta_timestamp_end = timestamp_va + 8;
        translated.parameters.ta_user_timestamp_start = user_timestamps[0];
        translated.parameters.ta_user_timestamp_end = user_timestamps[1];
        translated.parameters.fragment_timestamp_start = timestamp_va + 16;
        translated.parameters.fragment_timestamp_end = timestamp_va + 24;
        translated.parameters.fragment_user_timestamp_start = user_timestamps[2];
        translated.parameters.fragment_user_timestamp_end = user_timestamps[3];
        let lifecycle_predecessor = g17p_reserve_render_gid_group()?;
        let (fragment_lifecycle, tiling_lifecycle) =
            g17p_allocate_render_lifecycle_pair(lifecycle_predecessor)?;
        translated.parameters.fragment_lifecycle = fragment_lifecycle;
        translated.parameters.lifecycle = tiling_lifecycle;
        dev_info!(
            dev.as_ref(),
            "G17P render lifecycle: predecessor={:#x} 3D current={:#x} TA current={:#x} ordinal=0\n",
            lifecycle_predecessor,
            fragment_lifecycle as u32,
            tiling_lifecycle as u32,
        );
        if *crate::module_parameters::g17p_render_eot_bind_zero.value() != 0 {
            translated.parameters.store_pipeline_bind = 0;
            translated.parameters.partial_store_pipeline_bind = 0;
            dev_info!(
                dev.as_ref(),
                "G17P render EOT bind: forcing native-captured main/partial value 0\n",
            );
        }

        for (address, size, need_read, need_write) in [
            (
                translated.parameters.context_base,
                mmu::UAT_PGSZ as u64,
                true,
                true,
            ),
            (
                translated.parameters.encoder,
                core::mem::size_of::<u32>() as u64,
                true,
                false,
            ),
            (
                translated.parameters.tilemap,
                render_layout.tilemap_size as u64,
                true,
                true,
            ),
            (
                translated.parameters.heapmeta,
                render_layout.heapmeta_size as u64,
                true,
                true,
            ),
            (
                translated.parameters.tpc,
                render_layout.tpc_size as u64,
                true,
                true,
            ),
            (
                translated.parameters.deflake_3,
                mmu::UAT_PGSZ as u64,
                true,
                false,
            ),
            (
                translated.parameters.ta_status,
                mmu::UAT_PGSZ as u64,
                true,
                true,
            ),
            (
                translated.parameters.fragment_status,
                mmu::UAT_PGSZ as u64,
                true,
                true,
            ),
            (
                translated.parameters.aux_fb,
                G17P_RENDER_STATE_AUX_FB_SIZE as u64,
                true,
                true,
            ),
        ] {
            if !vm.covers_range(address, size, need_read, need_write) {
                return Err(EFAULT);
            }
        }
        if render_tvb.as_ref().is_some_and(|tvb| {
            !vm.covers_range(tvb.iova(), tvb.logical_size as u64, true, true)
        }) {
            return Err(EFAULT);
        }
        if !uat
            .kernel_vm()
            .covers_range(timestamp_va, 32, true, true)
        {
            return Err(EFAULT);
        }

        let status_flag = *crate::module_parameters::g17p_render_status_flag.value();
        bootstrap_stage!("state encoding", render_state.with_bytes_mut(|raw| {
            encode_g17p_render_runtime_state(
                raw,
                &render_layout,
                translated.parameters.width,
                translated.parameters.height,
                status_flag,
            )
            .map_err(|_| render_build_fail(3))
        }));

        let graph_va = graph.iova();
        // These pages ARE the SKSM entry rings: the queue-context template
        // lands at offset 0x200, which is ring slot 1 -- exactly the slot
        // stamp 1 will use. When the SKSM path is armed the real encoder
        // writes that slot later, but the 3D doorbell rings first and its
        // kick declares a barrier on {qid 5, stamp 1}; the KSM resolves that
        // by reading queue 5's slot, and a synthetic entry sitting there
        // self-identifies as QID 0 (word0 bits 42:48 = 0, own-dependency
        // {stamp 0, qid 0}), which is what the firmware then loaded, ran and
        // blamed. Native never stages anything into a live ring slot: the ring
        // is zero until encodeKickEntry fills it. Leave it zero here too.
        let sksm_owns_entry_ring = *crate::module_parameters::g17p_render_sksm.value() != 0
            && *crate::module_parameters::g17p_render_slot_template.value() == 0;
        bootstrap_stage!("TA queue context", ta_context_peer.with_bytes_mut(|raw| {
            raw.fill(0);
            if sksm_owns_entry_ring {
                return Ok(());
            }
            g17_submission::apply_g17p_cold_opening_queue_context(
                g17_submission::G17PColdOpeningStage::Tiling,
                g17_submission::G17P_COLD_OPENING_TA_DESCRIPTOR_BASE,
                graph_va + Self::TILING_QUEUE as u64,
                &mut raw[..mmu::UAT_PGSZ],
            )
            .map_err(|_| render_build_fail(4))
        }));
        bootstrap_stage!("3D queue context", fragment_context_peer.with_bytes_mut(|raw| {
            raw.fill(0);
            if sksm_owns_entry_ring {
                return Ok(());
            }
            g17_submission::apply_g17p_cold_opening_queue_context(
                g17_submission::G17PColdOpeningStage::Fragment,
                g17_submission::G17P_COLD_OPENING_3D_DESCRIPTOR_BASE,
                graph_va + Self::FRAGMENT_QUEUE as u64,
                &mut raw[..mmu::UAT_PGSZ],
            )
            .map_err(|_| render_build_fail(5))
        }));
        let graph_addresses = g17_submission::G17PColdOpeningGraphAddresses {
            primary_index: g17_submission::G17P_PARTIAL_OPENING_PRIMARY_INDEX_FIRMWARE_GPU_VA,
            secondary_index: support_va + G17P_RENDER_SECONDARY_INDEX as u64,
            pool_a_slots: support_va + G17P_RENDER_POOL_A_SLOTS as u64,
            pool_b_slots: support_va + G17P_RENDER_POOL_B_SLOTS as u64,
            shared_slots: support_va + G17P_RENDER_SHARED_SLOTS as u64,
            flag: support_va + G17P_RENDER_FLAG as u64,
        };
        let pool_a_record_index = g17_submission::g17p_render_pool_a_record_index(
            0,
            g17p_render_native_pool_a_first_record_enabled(),
        );
        let shared_object = if g17p_render_native_fragment_hwpb_enabled() {
            parameter_management.as_ref().map_or(
                support_va + G17P_RENDER_PACKED_SHARED as u64,
                G17PParameterManagement::hwpb_state_va,
            )
        } else {
            support_va + G17P_RENDER_PACKED_SHARED as u64
        };
        let objects = g17_render::RenderDescriptorObjects {
            record_a: support_va + G17P_RENDER_POOL_A as u64,
            shared: shared_object,
            record_b: parameter_management
                .as_ref()
                .map_or(support_va + G17P_RENDER_POOL_B as u64, |pm| {
                    pm.selected_scene_va()
                }),
            zero: parameter_management
                .as_ref()
                .map_or(support_va + G17P_RENDER_ZERO_SHARED as u64, |pm| {
                    pm.trailer_va()
                }),
        }
        .with_pool_a_record_index(pool_a_record_index)
        .map_err(|_| render_build_fail(6))?;
        let tiling_objects = if g17p_render_native_fragment_hwpb_mode() == 3 {
            g17_render::RenderDescriptorObjects {
                shared: support_va + G17P_RENDER_PACKED_SHARED as u64,
                ..objects
            }
        } else {
            objects
        };
        let render_channel_control = support_va + G17P_RENDER_CHANNEL_CONTROL as u64;
        bootstrap_stage!("support encoding", support.with_bytes_mut(|raw| {
            g17_submission::apply_g17p_cold_opening_leaf_page(
                g17_submission::G17PColdOpeningLeaf::SecondaryIndex,
                &mut raw[G17P_RENDER_SECONDARY_INDEX..G17P_RENDER_SECONDARY_INDEX + mmu::UAT_PGSZ],
            )
            .map_err(|_| render_build_fail(6))?;
            g17_submission::apply_g17p_cold_opening_leaf_page(
                g17_submission::G17PColdOpeningLeaf::PoolASlots,
                &mut raw[G17P_RENDER_POOL_A_SLOTS..G17P_RENDER_POOL_A_SLOTS + mmu::UAT_PGSZ],
            )
            .map_err(|_| render_build_fail(7))?;
            // Native exposes value one to early 3D and changes it to two in
            // the late TA publication callback.
            g17_submission::apply_g17p_cold_opening_pool_a_start_slot(
                pool_a_record_index,
                &mut raw[G17P_RENDER_POOL_A_SLOTS..G17P_RENDER_POOL_A_SLOTS + mmu::UAT_PGSZ],
            )
            .map_err(|_| render_build_fail(7))?;
            g17_submission::apply_g17p_cold_opening_leaf_page(
                g17_submission::G17PColdOpeningLeaf::PoolBSlots,
                &mut raw[G17P_RENDER_POOL_B_SLOTS..G17P_RENDER_POOL_B_SLOTS + mmu::UAT_PGSZ],
            )
            .map_err(|_| render_build_fail(8))?;
            g17_submission::apply_g17p_cold_opening_leaf_page(
                g17_submission::G17PColdOpeningLeaf::SharedSlots,
                &mut raw[G17P_RENDER_SHARED_SLOTS..G17P_RENDER_SHARED_SLOTS + mmu::UAT_PGSZ],
            )
            .map_err(|_| render_build_fail(9))?;
            g17_submission::apply_g17p_cold_opening_leaf_page(
                g17_submission::G17PColdOpeningLeaf::Flag,
                &mut raw[G17P_RENDER_FLAG..G17P_RENDER_FLAG + mmu::UAT_PGSZ],
            )
            .map_err(|_| render_build_fail(10))?;
            g17_submission::apply_g17p_cold_opening_record_pool_a_at(
                graph_addresses,
                pool_a_record_index,
                &mut raw[G17P_RENDER_POOL_A
                    ..G17P_RENDER_POOL_A + g17_submission::G17P_COLD_OPENING_RECORD_POOL_A_SIZE],
            )
            .map_err(|_| render_build_fail(11))?;
            g17_submission::apply_g17p_cold_opening_record_pool_b(
                graph_addresses,
                &mut raw[G17P_RENDER_POOL_B
                    ..G17P_RENDER_POOL_B + g17_submission::G17P_COLD_OPENING_RECORD_POOL_B_SIZE],
            )
            .map_err(|_| render_build_fail(12))?;
            g17_submission::apply_g17p_cold_opening_shared_object(
                graph_addresses,
                &mut raw[G17P_RENDER_PACKED_SHARED
                    ..G17P_RENDER_PACKED_SHARED
                        + g17_submission::G17P_COLD_OPENING_SHARED_OBJECT_SIZE],
            )
            .map_err(|_| render_build_fail(13))?;
            g17_submission::apply_g17p_cold_opening_shared_control(
                G17P_CONTROL_OPERAND_TABLE_VA,
                support_va + G17P_RENDER_SHARED_CONTROL_INNER as u64,
                &mut raw[G17P_RENDER_SHARED_CONTROL
                    ..G17P_RENDER_SHARED_CONTROL
                        + g17_submission::G17P_COLD_OPENING_SHARED_CONTROL_SIZE],
            )
            .map_err(|_| render_build_fail(14))?;
            put_u32(raw, G17P_RENDER_SHARED_CONTROL_INNER, 1);
            g17_submission::apply_g17p_cold_opening_channel_control(
                &mut raw[G17P_RENDER_CHANNEL_CONTROL
                    ..G17P_RENDER_CHANNEL_CONTROL
                        + g17_submission::G17P_COLD_OPENING_CHANNEL_CONTROL_SIZE],
            )
            .map_err(|_| render_build_fail(15))?;
            Ok(())
        }));
        pr_info!(
            "G17P render ta-inputs: context_base={:#x} encoder(vdm)={:#x} tilemap={:#x} heapmeta={:#x} tpc={:#x}\n",
            translated.parameters.context_base,
            translated.parameters.encoder,
            translated.parameters.tilemap,
            translated.parameters.heapmeta,
            translated.parameters.tpc,
        );
        // Enumerate what the tiling VDM will actually dereference. The MMU
        // fault bank names no usable address on this part, so the only way to
        // tell a bad pointer from a bad descriptor is to read the control
        // stream the GPU is about to fetch and check every plausible pointer
        // in it against this VM's own tables.
        {
            let encoder = translated.parameters.encoder;
            let mut stream = [0u8; 0x100];
            match vm.read_bytes(encoder, &mut stream) {
                Ok(()) => {
                    for chunk in 0..(stream.len() / 32) {
                        let base = chunk * 32;
                        pr_info!(
                            "G17P render vdm-stream +{:#05x}: {:02x?}\n",
                            base,
                            &stream[base..base + 32],
                        );
                    }
                    for word in 0..(stream.len() / 8) {
                        let mut bytes = [0u8; 8];
                        bytes.copy_from_slice(&stream[word * 8..word * 8 + 8]);
                        let value = u64::from_le_bytes(bytes);
                        if value == 0 {
                            continue;
                        }
                        let mapped = vm.covers_range(value & !0x3fff, 0x4000, true, false);
                        let rebased = value.checked_add(0x10_0000_0000).unwrap_or(0);
                        let rebased_mapped =
                            rebased != 0 && vm.covers_range(rebased & !0x3fff, 0x4000, true, false);
                        if mapped || rebased_mapped {
                            pr_info!(
                                "G17P render vdm-pointer +{:#05x}: {:#x} mapped={} rebased({:#x})={}\n",
                                word * 8,
                                value,
                                mapped,
                                rebased,
                                rebased_mapped,
                            );
                        }
                    }
                }
                Err(error) => pr_info!(
                    "G17P render vdm-stream: read at {:#x} failed ({:?})\n",
                    encoder,
                    error,
                ),
            }
        }
        pr_info!(
            "G17P render ta-inputs: deflake={:#x}/{:#x}/{:#x} {}x{} layers={} utile={}x{} bootstrap_vdm={}\n",
            translated.parameters.deflake_1,
            translated.parameters.deflake_2,
            translated.parameters.deflake_3,
            translated.parameters.width,
            translated.parameters.height,
            translated.parameters.layers,
            translated.parameters.utile_width,
            translated.parameters.utile_height,
            bootstrap_vdm,
        );
        // Allocate after render state/TVB so the entries-only diagnostic RCE
        // mirror does not perturb their established low VAs. Native KSM binds
        // the complete descriptor-local arrays instead.
        let mut fragment_rce = bootstrap_stage!(
            "3D external RCE program allocation",
            MappedObject::new_wc_in_range(
                dev,
                vm,
                g17p_cl_dynamic_low_va_range(),
                g17_render::G17P_FRAGMENT_RCE_STORAGE_SIZE,
                mmu::UAT_PGSZ as u64,
                mmu::PROT_GPU_SHARED_RO,
            )
        );
        let fragment_rce_va = fragment_rce.iova();
        let descriptor_3d_render = bootstrap_stage!(
            "3D descriptor render-VM alias",
            descriptors.object.map_range_into_range(
                vm,
                mmu::UAT_PGSZ..2 * mmu::UAT_PGSZ,
                g17p_cl_dynamic_low_va_range(),
                mmu::UAT_PGSZ as u64,
                mmu::PROT_GPU_SHARED_RO,
                false,
            )
        );
        let descriptor_3d_render_va = descriptor_3d_render.iova();
        dev_info!(
            dev.as_ref(),
            "G17P render RCE: descriptor backing aliased into render VM at {:#x}; diagnostic entry mirrors={:#x}/{:#x}/{:#x}/{:#x} stride={:#x}\n",
            descriptor_3d_render_va,
            fragment_rce_va,
            fragment_rce_va + g17_render::G17P_FRAGMENT_RCE_PROGRAM_STRIDE,
            fragment_rce_va + 2 * g17_render::G17P_FRAGMENT_RCE_PROGRAM_STRIDE,
            fragment_rce_va + 3 * g17_render::G17P_FRAGMENT_RCE_PROGRAM_STRIDE,
            g17_render::G17P_FRAGMENT_RCE_PROGRAM_STRIDE,
        );
        let fragment_mcache = bootstrap_stage!("descriptor encoding", descriptors.with_bytes_mut(|raw| {
            let (tiling, fragment) = raw.split_at_mut(0x4000);
            g17_render::build_cold_opening_ta_descriptor(
                &translated.parameters,
                tiling_objects,
                command_context_id,
                translated.parameters.ta_hardware_buffer_id,
                tiling,
            )
                .map_err(|error| render_build_fail_named(16, error))?;
            g17_render::apply_g17p_retained_descriptor_fields(
                g17_render::RenderDescriptorKind::Tiling,
                G17P_BOOTSTRAP_TA_DESCRIPTOR_LOW_VA,
                g17_initdata::CONTROL_SHARED_ADDRESS,
                g17p_render_queue_id(true) as u8,
                g17p_render_queue_id(false) as u8,
                0,
                true,
                g17p_render_native_pm_bytes(),
                tiling,
            )
            .map_err(|error| render_build_fail_named(17, error))?;
            g17_render::build_cold_opening_fragment_descriptor(
                &translated.parameters,
                objects,
                command_context_id,
                fragment,
            )
            .map_err(|error| render_build_fail_named(18, error))?;
            g17_render::apply_g17p_retained_descriptor_fields(
                g17_render::RenderDescriptorKind::Fragment,
                descriptor_3d_render_va,
                g17_initdata::CONTROL_SHARED_ADDRESS,
                g17p_render_queue_id(false) as u8,
                g17p_render_queue_id(false) as u8,
                0,
                true,
                g17p_render_native_pm_bytes(),
                fragment,
            )
            .map_err(|error| render_build_fail_named(19, error))?;
            fragment_rce.with_bytes_mut(|rce| {
                g17_render::stage_g17p_fragment_rce_programs(
                    fragment,
                    fragment_rce_va,
                    rce,
                )
                .map_err(|error| render_build_fail_named(19, error))
            })?;
            if *crate::module_parameters::g17p_render_dump_descriptor.value() != 0 {
                fragment_rce.with_bytes_mut(|rce| {
                    let dump_size = (4 * g17_render::G17P_FRAGMENT_RCE_PROGRAM_STRIDE)
                        as usize;
                    for (index, chunk) in rce[..dump_size].chunks(16).enumerate() {
                        dev_info!(
                            dev.as_ref(),
                            "G17P 3d-rce bootstrap +{:#05x} {:02x?}\n",
                            index * 16,
                            chunk,
                        );
                    }
                    Ok(())
                })?;
            }
            apply_g17p_generic_descriptor_tail(tiling, fragment)
                .ok_or_else(|| render_build_fail(20))?;
            // AGX3DChannelSKSM::submitBuffer fills +0x2160 from the physical
            // SKSM queue timestamp immediately before publishing this command.
            // Keep it zero until `stage_fragment_completion_scratch` can use
            // the real retained producer state.
            put_u64(fragment, 0x2160, 0);
            let fragment_mcache = apply_g17p_fragment_mcache(
                &translated.fragment_attachments,
                translated.parameters.aux_fb,
                G17P_RENDER_STATE_AUX_FB_SIZE as u64,
                descriptor_3d_render_va,
                fragment,
            )?;
            if *crate::module_parameters::g17p_render_dump_descriptor.value() != 0 {
                dev_info!(
                    dev.as_ref(),
                    "G17P ta-desc bootstrap context={} ordinal=0 low_va={:#x} bytes={:#x}\n",
                    command_context_id,
                    G17P_BOOTSTRAP_TA_DESCRIPTOR_LOW_VA,
                    g17_render::TA_DESCRIPTOR_SIZE,
                );
                for (index, chunk) in tiling[..g17_render::TA_DESCRIPTOR_SIZE]
                    .chunks(64)
                    .enumerate()
                {
                    dev_info!(
                        dev.as_ref(),
                        "G17P ta-desc bootstrap +{:#05x} {:02x?}\n",
                        index * 64,
                        chunk,
                    );
                }
                dev_info!(
                    dev.as_ref(),
                    "G17P 3d-desc bootstrap context={} ordinal=0 low_va={:#x} bytes={:#x}\n",
                    command_context_id,
                    descriptor_3d_render_va,
                    g17_render::FRAGMENT_DESCRIPTOR_SIZE,
                );
                for (index, chunk) in fragment[..g17_render::FRAGMENT_DESCRIPTOR_SIZE]
                    .chunks(16)
                    .enumerate()
                {
                    dev_info!(
                        dev.as_ref(),
                        "G17P 3d-desc bootstrap +{:#05x} {:02x?}\n",
                        index * 16,
                        chunk,
                    );
                }
            }
            Ok(fragment_mcache)
        }));
        dev_info!(
            dev.as_ref(),
            "G17P render storage: TA self={:#x} context={:#x}/{:#x} tails={:#x}/{:#x}/{:#x}\n",
            G17P_BOOTSTRAP_TA_DESCRIPTOR_LOW_VA,
            G17P_BOOTSTRAP_TA_CONTEXT_LOW_VA,
            G17P_BOOTSTRAP_TA_CONTEXT_HIGH_VA,
            G17P_BOOTSTRAP_DESCRIPTOR_ZERO_A_VA,
            G17P_BOOTSTRAP_DESCRIPTOR_ZERO_B_VA,
            G17P_BOOTSTRAP_TA_STATUS_VA
        );
        dev_info!(
            dev.as_ref(),
            "G17P render storage: 3D self={:#x} context={:#x}/{:#x} tails={:#x}/{:#x}/{:#x}\n",
            descriptor_3d_render_va,
            G17P_BOOTSTRAP_3D_CONTEXT_LOW_VA,
            G17P_BOOTSTRAP_3D_CONTEXT_HIGH_VA,
            G17P_BOOTSTRAP_DESCRIPTOR_ZERO_A_VA + 4,
            G17P_BOOTSTRAP_DESCRIPTOR_ZERO_B_VA + 4,
            G17P_BOOTSTRAP_3D_STATUS_VA
        );

        bootstrap_stage!("queue encoding", graph.with_bytes_mut(|raw| {
            let job_list = graph_va + Self::JOB_LIST as u64;
            g17_submission::apply_g17p_cold_opening_queue_record(
                g17_submission::G17PColdOpeningQueueAddresses {
                    pointers: graph_va + Self::TILING_POINTERS as u64,
                    item_ring: graph_va + Self::TILING_RING as u64,
                    job_list,
                    channel_control: render_channel_control,
                    owner_pid,
                },
                &mut raw[Self::TILING_QUEUE..Self::TILING_QUEUE + G17P_RENDER_QUEUE_RECORD_SIZE],
            )
            .map_err(|_| render_build_fail(21))?;
            g17_submission::apply_g17p_cold_opening_queue_record(
                g17_submission::G17PColdOpeningQueueAddresses {
                    pointers: graph_va + Self::FRAGMENT_POINTERS as u64,
                    item_ring: graph_va + Self::FRAGMENT_RING as u64,
                    job_list,
                    channel_control: render_channel_control,
                    owner_pid,
                },
                &mut raw
                    [Self::FRAGMENT_QUEUE..Self::FRAGMENT_QUEUE + G17P_RENDER_QUEUE_RECORD_SIZE],
            )
            .map_err(|_| render_build_fail(22))?;
            g17_submission::apply_g17p_cold_opening_pointer_block(
                &mut raw
                    [Self::TILING_POINTERS..Self::TILING_POINTERS + G17P_RENDER_POINTER_BLOCK_SIZE],
            )
            .map_err(|_| render_build_fail(23))?;
            g17_submission::apply_g17p_cold_opening_pointer_block(
                &mut raw[Self::FRAGMENT_POINTERS
                    ..Self::FRAGMENT_POINTERS + G17P_RENDER_POINTER_BLOCK_SIZE],
            )
            .map_err(|_| render_build_fail(24))?;
            put_u64(raw, Self::JOB_LIST + 0x08, job_list);
            Ok(())
        }));

        Ok(Self {
            render_operand_aliases: KVec::new(),
            descriptors,
            fragment_rce,
            descriptor_ta_high,
            descriptor_3d_high,
            descriptor_ta_low,
            descriptor_3d_low,
            descriptor_3d_render,
            ta_context_low,
            fragment_context_low,
            graph,
            support,
            render_state,
            render_layout,
            render_tvb,
            render_tvb_hardware_alias,
            render_user_aliases,
            parameter_management,
            render_ta_status: Some(render_ta_status),
            render_3d_status: Some(render_3d_status),
            timestamps,
            user_timestamp_aliases,
            fragment_mcache,
            ta_hardware_buffer_id: translated.parameters.ta_hardware_buffer_id,
            owner_pid,
            lifecycle_predecessor,
            command_context,
        })
    }

    /// Refresh the canonical queue group's command-owned objects after the
    /// previous group has reached semantic completion.
    pub(crate) fn restage_command(
        &mut self,
        dev: &AsahiDevice,
        uat: &mmu::Uat,
        vm: &mmu::Vm,
        parameter_page_list_vas: [u64; 2],
        command: &g17_uapi::TranslatedRenderCommand,
        num_clusters: u32,
        user_timestamps: [u64; 4],
        user_timestamp_aliases: KVec<mmu::KernelMapping>,
        submission_ordinal: u32,
    ) -> Result {
        if submission_ordinal == 0 || submission_ordinal >= G17P_RETAINED_RENDER_ORDINALS {
            return Err(EINVAL);
        }
        let mut next_descriptors = MappedObject::new_wc(
            dev,
            uat,
            G17P_RENDER_DESCRIPTOR_STORAGE_SIZE,
            mmu::UAT_PGSZ as u64,
            mmu::PROT_GPU_FW_SHARED_RW,
        )?;
        let render_layout =
            g17p_render_state_layout(&command.parameters, num_clusters).ok_or(EINVAL)?;
        let mut next_render_state = MappedObject::new_wc_in_range(
            dev,
            vm,
            g17p_cl_dynamic_low_va_range(),
            render_layout.total_size,
            if g17p_render_tvb_enabled() {
                G17P_PM_PAGE_SIZE as u64
            } else {
                mmu::UAT_PGSZ as u64
            },
            mmu::PROT_GPU_FW_SHARED_RW,
        )?;
        let mut next_render_tvb = if g17p_render_tvb_enabled() {
            Some(MappedObject::new_wc_in_range(
                dev,
                vm,
                G17P_RENDER_USER_TVB_START..G17P_RENDER_USER_TVB_END,
                render_layout.tvb_size,
                G17P_PM_PAGE_SIZE as u64,
                mmu::PROT_GPU_FW_SHARED_RW,
            )?)
        } else {
            None
        };
        if let Some(tvb) = next_render_tvb.as_mut() {
            tvb.with_bytes_mut(|raw| {
                raw.fill(0);
                Ok(())
            })?;
        }
        let next_render_tvb_hardware_alias = if let Some(tvb) = next_render_tvb.as_mut() {
            Some(map_g17p_render_tvb_hardware_alias(dev, vm, tvb)?)
        } else {
            None
        };
        let next_render_user_aliases = G17PRenderUserAliases::new(
            &mut next_render_state,
            vm,
            &render_layout,
            submission_ordinal,
            g17p_render_tvb_mode() >= 2,
        )?;
        let next_timestamps = MappedObject::new_wc(
            dev,
            uat,
            mmu::UAT_PGSZ,
            mmu::UAT_PGSZ as u64,
            mmu::PROT_GPU_FW_SHARED_RW,
        )?;
        let render_state_va = next_render_state.iova();
        let timestamp_va = next_timestamps.iova();
        let mut translated = *command;
        translated.parameters.context_base = G17P_RENDER_USER_BASE;
        translated.parameters.usc_flist_hardware_buffer_id =
            g17_initdata::PARTIAL_OPENING_FREELIST_HARDWARE_BUFFER_ID;
        let next_parameter_management = if g17p_render_tvb_mode() >= 2 {
            let metrics_vas = self
                .parameter_management
                .as_ref()
                .ok_or(EIO)?
                .metrics_vas;
            Some(G17PParameterManagement::new(
                dev,
                uat,
                parameter_page_list_vas,
                metrics_vas,
                next_render_tvb.as_ref().map(MappedObject::iova).ok_or(EIO)?,
                next_render_user_aliases.scene_scratch_va().ok_or(EINVAL)?,
                next_render_user_aliases.discard_va().ok_or(EINVAL)?,
                translated.parameters.ta_hardware_buffer_id,
                &render_layout,
            )?)
        } else {
            None
        };
        translated.parameters.deflake_3 = next_render_user_aliases.deflake_va();
        translated.parameters.deflake_2 = translated.parameters.deflake_3 + 0x20;
        translated.parameters.deflake_1 = translated.parameters.deflake_3 + 0x2a0;
        if let Some(scene_scratch_va) = next_render_user_aliases.scene_scratch_va() {
            translated.parameters.cycle = g17p_render_cycle_pointer(scene_scratch_va)?;
        }
        translated.parameters.ta_status = render_state_va + G17P_RENDER_STATE_TA_STATUS as u64;
        translated.parameters.fragment_status =
            render_state_va + G17P_RENDER_STATE_FRAGMENT_STATUS as u64;
        translated.parameters.heapmeta = next_render_user_aliases.heapmeta_va(&render_layout)?;
        translated.parameters.tpc = next_render_user_aliases.tpc_va();
        translated.parameters.aux_fb = next_render_user_aliases.aux_fb_va();
        translated.parameters.aux_fb_page_count = G17P_RENDER_AUX_FB_PAGE_COUNT;
        translated.parameters.aux_fb_flags = g17p_aux_fb_flags(translated.flags);
        translated.parameters.tilemap = next_render_user_aliases.tilemap_va();
        if let Some(pm) = next_parameter_management.as_ref() {
            translated.parameters.parameter_buffer = pm.selected_scene_va();
        }
        translated.parameters.ta_timestamp_start = timestamp_va;
        translated.parameters.ta_timestamp_end = timestamp_va + 8;
        translated.parameters.ta_user_timestamp_start = user_timestamps[0];
        translated.parameters.ta_user_timestamp_end = user_timestamps[1];
        translated.parameters.fragment_timestamp_start = timestamp_va + 16;
        translated.parameters.fragment_timestamp_end = timestamp_va + 24;
        translated.parameters.fragment_user_timestamp_start = user_timestamps[2];
        translated.parameters.fragment_user_timestamp_end = user_timestamps[3];
        let (fragment_lifecycle, tiling_lifecycle) =
            g17p_allocate_render_lifecycle_pair(self.lifecycle_predecessor)?;
        translated.parameters.fragment_lifecycle = fragment_lifecycle;
        translated.parameters.lifecycle = tiling_lifecycle;
        dev_info!(
            dev.as_ref(),
            "G17P render lifecycle: predecessor={:#x} 3D current={:#x} TA current={:#x} ordinal={}\n",
            self.lifecycle_predecessor,
            fragment_lifecycle as u32,
            tiling_lifecycle as u32,
            submission_ordinal,
        );
        if *crate::module_parameters::g17p_render_eot_bind_zero.value() != 0 {
            translated.parameters.store_pipeline_bind = 0;
            translated.parameters.partial_store_pipeline_bind = 0;
            dev_info!(
                dev.as_ref(),
                "G17P render EOT bind: forcing native-captured main/partial value 0\n",
            );
        }

        if !vm.covers_range(
            render_state_va,
            render_layout.total_size as u64,
            true,
            true,
        ) || !uat
            .kernel_vm()
            .covers_range(timestamp_va, 32, true, true)
        {
            return Err(EFAULT);
        }
        if !user_timestamp_aliases
            .iter()
            .all(|mapping| vm.covers_range(mapping.iova(), mapping.size() as u64, false, true))
        {
            return Err(EFAULT);
        }
        if !next_render_user_aliases.all_reachable_from(vm) {
            return Err(EFAULT);
        }
        if next_render_tvb.as_ref().is_some_and(|tvb| {
            !vm.covers_range(tvb.iova(), tvb.logical_size as u64, true, true)
        }) {
            return Err(EFAULT);
        }
        let status_flag = *crate::module_parameters::g17p_render_status_flag.value();
        next_render_state.with_bytes_mut(|raw| {
            encode_g17p_render_runtime_state(
                raw,
                &render_layout,
                translated.parameters.width,
                translated.parameters.height,
                status_flag,
            )
        })?;

        let record_a_index = (submission_ordinal as usize * 2) % 35;
        let record_b_index = submission_ordinal as usize % 79;
        let support_va = self.support.iova();
        let shared_object = if g17p_render_native_fragment_hwpb_enabled() {
            next_parameter_management.as_ref().map_or(
                support_va + G17P_RENDER_PACKED_SHARED as u64,
                G17PParameterManagement::hwpb_state_va,
            )
        } else {
            support_va + G17P_RENDER_PACKED_SHARED as u64
        };
        let objects = g17_render::RenderDescriptorObjects {
            record_a: support_va + G17P_RENDER_POOL_A as u64 + (record_a_index * 0x100) as u64,
            shared: shared_object,
            record_b: next_parameter_management.as_ref().map_or(
                support_va + G17P_RENDER_POOL_B as u64 + (record_b_index * 0x80) as u64,
                |pm| pm.selected_scene_va(),
            ),
            zero: next_parameter_management
                .as_ref()
                .map_or(support_va + G17P_RENDER_ZERO_SHARED as u64, |pm| {
                    pm.trailer_va()
                }),
        };
        let tiling_objects = if g17p_render_native_fragment_hwpb_mode() == 3 {
            g17_render::RenderDescriptorObjects {
                shared: support_va + G17P_RENDER_PACKED_SHARED as u64,
                ..objects
            }
        } else {
            objects
        };
        let fragment_rce_va = self.fragment_rce.iova();
        let descriptor_3d_render_va = self.descriptor_3d_render.iova();
        let fragment_rce = &mut self.fragment_rce;
        let fragment_mcache = next_descriptors.with_bytes_mut(|raw| {
            let (tiling, fragment) = raw.split_at_mut(0x4000);
            g17_render::build_ta_descriptor(
                &translated.parameters,
                tiling_objects,
                g17_render::RenderDescriptorMetadata {
                    context_id: self.command_context.context_id(),
                    submission_ordinal,
                    ta_hardware_buffer_id: translated.parameters.ta_hardware_buffer_id,
                    submit_sequence: submission_ordinal as u64 * 2 + 1,
                },
                tiling,
            )
            .map_err(|_| EINVAL)?;
            g17_render::apply_g17p_retained_descriptor_fields(
                g17_render::RenderDescriptorKind::Tiling,
                G17P_BOOTSTRAP_TA_DESCRIPTOR_LOW_VA,
                g17_initdata::CONTROL_SHARED_ADDRESS,
                g17p_render_queue_id(true) as u8,
                g17p_render_queue_id(false) as u8,
                submission_ordinal,
                false,
                g17p_render_native_pm_bytes(),
                tiling,
            )
            .map_err(|_| EINVAL)?;
            g17_render::build_fragment_descriptor(
                &translated.parameters,
                objects,
                g17_render::RenderDescriptorMetadata {
                    context_id: self.command_context.context_id(),
                    submission_ordinal,
                    ta_hardware_buffer_id: 0,
                    submit_sequence: submission_ordinal as u64 * 2,
                },
                fragment,
            )
            .map_err(|_| EINVAL)?;
            g17_render::apply_g17p_retained_descriptor_fields(
                g17_render::RenderDescriptorKind::Fragment,
                descriptor_3d_render_va,
                g17_initdata::CONTROL_SHARED_ADDRESS,
                g17p_render_queue_id(false) as u8,
                g17p_render_queue_id(false) as u8,
                submission_ordinal,
                false,
                g17p_render_native_pm_bytes(),
                fragment,
            )
            .map_err(|_| EINVAL)?;
            fragment_rce.with_bytes_mut(|rce| {
                g17_render::stage_g17p_fragment_rce_programs(
                    fragment,
                    fragment_rce_va,
                    rce,
                )
                .map_err(|_| EINVAL)
            })?;
            if *crate::module_parameters::g17p_render_dump_descriptor.value() != 0 {
                fragment_rce.with_bytes_mut(|rce| {
                    let dump_size = (4 * g17_render::G17P_FRAGMENT_RCE_PROGRAM_STRIDE)
                        as usize;
                    for (index, chunk) in rce[..dump_size].chunks(16).enumerate() {
                        dev_info!(
                            dev.as_ref(),
                            "G17P 3d-rce restage +{:#05x} {:02x?}\n",
                            index * 16,
                            chunk,
                        );
                    }
                    Ok(())
                })?;
            }
            apply_g17p_generic_descriptor_tail(tiling, fragment).ok_or(EINVAL)?;
            // The physical queue timestamp is staged after this fresh image
            // replaces the retained one and before its command pointer is
            // published. Building the descriptor must not approximate it from
            // the userspace submission ordinal.
            put_u64(fragment, 0x2160, 0);
            apply_g17p_fragment_mcache(
                &translated.fragment_attachments,
                translated.parameters.aux_fb,
                G17P_RENDER_STATE_AUX_FB_SIZE as u64,
                descriptor_3d_render_va,
                fragment,
            )
        })?;

        let source = next_descriptors.object.vmap()?;
        let destination = self.descriptors.object.vmap()?;
        let support = self.support.object.vmap()?;
        let source = unsafe {
            core::slice::from_raw_parts(source.as_ptr(), G17P_RENDER_DESCRIPTOR_STORAGE_SIZE)
        };
        let destination = unsafe {
            core::slice::from_raw_parts_mut(
                destination.as_mut_ptr(),
                G17P_RENDER_DESCRIPTOR_STORAGE_SIZE,
            )
        };
        let support = unsafe {
            core::slice::from_raw_parts_mut(support.as_mut_ptr(), G17P_RENDER_SUPPORT_SIZE)
        };
        destination.copy_from_slice(source);
        let record_a = G17P_RENDER_POOL_A + record_a_index * 0x100;
        put_u32(
            support,
            record_a + 0x08,
            submission_ordinal + submission_ordinal / 2,
        );
        put_u32(support, record_a + 0x10, 0x50);
        put_u32(
            support,
            G17P_RENDER_POOL_A_SLOTS + 4 + record_a_index * 4,
            1,
        );
        put_u32(
            support,
            G17P_RENDER_SHARED_CONTROL_INNER,
            submission_ordinal * 2 + 1,
        );
        put_u32(support, G17P_RENDER_FLAG, submission_ordinal + 1);
        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);

        // The fixed status addresses are phase-stable while their backing is
        // replaced after an idle retained submission. Release the old aliases
        // before installing the new render-state pages at the same addresses.
        self.render_ta_status = None;
        self.render_3d_status = None;
        let next_ta_status = next_render_state.object.map_range_into_range(
            uat.kernel_vm(),
            G17P_RENDER_STATE_TA_STATUS..G17P_RENDER_STATE_TA_STATUS + mmu::UAT_PGSZ,
            G17P_BOOTSTRAP_TA_STATUS_VA
                ..G17P_BOOTSTRAP_TA_STATUS_VA + mmu::UAT_PGSZ as u64,
            mmu::UAT_PGSZ as u64,
            mmu::PROT_GPU_FW_SHARED_RW,
            false,
        )?;
        let next_3d_status = next_render_state.object.map_range_into_range(
            uat.kernel_vm(),
            G17P_RENDER_STATE_FRAGMENT_STATUS
                ..G17P_RENDER_STATE_FRAGMENT_STATUS + mmu::UAT_PGSZ,
            G17P_BOOTSTRAP_3D_STATUS_VA
                ..G17P_BOOTSTRAP_3D_STATUS_VA + mmu::UAT_PGSZ as u64,
            mmu::UAT_PGSZ as u64,
            mmu::PROT_GPU_FW_SHARED_RW,
            false,
        )?;
        self.render_ta_status = Some(next_ta_status);
        self.render_3d_status = Some(next_3d_status);

        let old_render_state = core::mem::replace(&mut self.render_state, next_render_state);
        self.render_layout = render_layout;
        let old_render_tvb_hardware_alias = core::mem::replace(
            &mut self.render_tvb_hardware_alias,
            next_render_tvb_hardware_alias,
        );
        let old_render_tvb = core::mem::replace(&mut self.render_tvb, next_render_tvb);
        let old_render_user_aliases =
            core::mem::replace(&mut self.render_user_aliases, next_render_user_aliases);
        let old_parameter_management =
            core::mem::replace(&mut self.parameter_management, next_parameter_management);
        let old_timestamps = core::mem::replace(&mut self.timestamps, next_timestamps);
        let old_aliases =
            core::mem::replace(&mut self.user_timestamp_aliases, user_timestamp_aliases);
        self.fragment_mcache = fragment_mcache;
        self.ta_hardware_buffer_id = translated.parameters.ta_hardware_buffer_id;
        drop(old_aliases);
        drop(old_timestamps);
        drop(old_parameter_management);
        drop(old_render_user_aliases);
        drop(old_render_tvb_hardware_alias);
        drop(old_render_tvb);
        drop(old_render_state);
        Ok(())
    }

    /// Report which reachability condition fails, instead of a bare bool.
    ///
    /// `is_reachable_from` ANDs together a dozen checks and the caller turns a
    /// false into EFAULT, which tells us nothing. Render currently fails here,
    /// so print each clause.
    /// Dump the two per-queue status pages the firmware would write.
    ///
    /// Plain DRAM reads of an object the driver allocated -- no sgx MMIO -- so
    /// unlike the scheduler-gate probe this is safe at every point in a
    /// submission, including after a timeout when the GPU cores have powered
    /// back down. Going from all-zero to non-zero across the kick proves the
    /// firmware touched them.
    pub(crate) fn log_status_pages(&mut self, dev: &AsahiDevice, label: &str) {
        let mut ta = [0u32; 8];
        let mut fragment = [0u32; 8];
        let read = self.render_state.with_bytes_mut(|raw| {
            for (index, slot) in ta.iter_mut().enumerate() {
                let at = G17P_RENDER_STATE_TA_STATUS + index * 4;
                *slot = u32::from_le_bytes([raw[at], raw[at + 1], raw[at + 2], raw[at + 3]]);
            }
            for (index, slot) in fragment.iter_mut().enumerate() {
                let at = G17P_RENDER_STATE_FRAGMENT_STATUS + index * 4;
                *slot = u32::from_le_bytes([raw[at], raw[at + 1], raw[at + 2], raw[at + 3]]);
            }
            Ok(())
        });
        match read {
            Ok(()) => {
                dev_info!(
                    dev.as_ref(),
                    "G17P render status[{}]: TA  {:#010x} {:#010x} {:#010x} {:#010x} {:#010x} {:#010x} {:#010x} {:#010x}\n",
                    label, ta[0], ta[1], ta[2], ta[3], ta[4], ta[5], ta[6], ta[7],
                );
                dev_info!(
                    dev.as_ref(),
                    "G17P render status[{}]: 3D  {:#010x} {:#010x} {:#010x} {:#010x} {:#010x} {:#010x} {:#010x} {:#010x}\n",
                    label, fragment[0], fragment[1], fragment[2], fragment[3],
                    fragment[4], fragment[5], fragment[6], fragment[7],
                );
            }
            Err(error) => dev_err!(
                dev.as_ref(),
                "G17P render status[{}]: read failed ({:?})\n",
                label,
                error
            ),
        }
        self.log_render_target_state(dev, label);
    }

    /// Dump the fixed-function render targets at every existing render
    /// checkpoint.  These are the exact objects TA produces and 3D/dIPP
    /// consumes, so a one-boot failure can distinguish an untouched RT graph
    /// from a fragment-side decode failure without another instrumentation
    /// rebuild.
    fn log_render_target_state(&mut self, dev: &AsahiDevice, label: &str) {
        let layout = self.render_layout;
        let mut tilemap = [0u64; 8];
        let mut heapmeta = [0u64; 8];
        let mut tpc = [0u64; 8];
        let mut axfb_head = [0u64; 8];
        let mut axfb_480 = [0u64; 8];
        let mut nonzero = [0usize; 4];
        let read = self.render_state.with_bytes_mut(|raw| {
            let copy_head = |offset: usize, out: &mut [u64; 8]| -> Result {
                for (index, value) in out.iter_mut().enumerate() {
                    let at = offset.checked_add(index * 8).ok_or(EOVERFLOW)?;
                    *value = u64::from_le_bytes(
                        raw.get(at..at + 8)
                            .ok_or(ERANGE)?
                            .try_into()
                            .map_err(|_| EINVAL)?,
                    );
                }
                Ok(())
            };
            copy_head(layout.tilemap, &mut tilemap)?;
            copy_head(layout.heapmeta, &mut heapmeta)?;
            copy_head(layout.tpc, &mut tpc)?;
            copy_head(layout.aux_fb, &mut axfb_head)?;
            copy_head(layout.aux_fb + 0x480, &mut axfb_480)?;
            for (index, (offset, size)) in [
                (layout.tilemap, layout.tilemap_size),
                (layout.heapmeta, layout.heapmeta_size),
                (layout.tpc, layout.tpc_size),
                (layout.aux_fb, G17P_RENDER_STATE_AUX_FB_SIZE),
            ]
            .into_iter()
            .enumerate()
            {
                nonzero[index] = raw
                    .get(offset..offset.checked_add(size).ok_or(EOVERFLOW)?)
                    .ok_or(ERANGE)?
                    .iter()
                    .filter(|byte| **byte != 0)
                    .count();
            }
            Ok(())
        });
        // The tilemap/heapmeta/TPC heads above live in the driver-owned render
        // state. The TILING VERTEX BUFFER is a separate object and was never
        // scanned, so "the tiler wrote nothing" could not be distinguished
        // from "the tiler wrote where we do not look" -- which matters,
        // because KTrace 0x43 reports status 3 (executed) for the TA and 4
        // (NOP-retired) only for the fragment, i.e. the tiler really does run.
        let tvb_nonzero = self.render_tvb.as_mut().map(|tvb| {
            let size = tvb.logical_size;
            tvb.with_bytes_mut(|raw| {
                Ok(raw.iter().filter(|byte| **byte != 0).count())
            })
            .map(|count| (count, size))
            .unwrap_or((usize::MAX, size))
        });
        if let Some((count, size)) = tvb_nonzero {
            // Where the tiler wrote matters as much as how much: 32 bytes in a
            // 1.2MB buffer is either a bare header or a single minimal tile
            // list, and the two say opposite things about whether geometry was
            // processed. Report the first non-zero offset and the bytes there.
            let mut first = usize::MAX;
            let mut window = [0u8; 64];
            if let Some(tvb) = self.render_tvb.as_mut() {
                let _ = tvb.with_bytes_mut(|raw| {
                    if let Some(at) = raw.iter().position(|byte| *byte != 0) {
                        first = at;
                        let start = at & !0xf;
                        for (index, slot) in window.iter_mut().enumerate() {
                            *slot = raw.get(start + index).copied().unwrap_or(0);
                        }
                    }
                    Ok(())
                });
            }
            if first == usize::MAX {
                dev_info!(
                    dev.as_ref(),
                    "G17P render TVB[{}]: nonzero={} of {} bytes (all zero)\n",
                    label,
                    count,
                    size,
                );
            } else {
                dev_info!(
                    dev.as_ref(),
                    "G17P render TVB[{}]: nonzero={} of {} bytes, first at {:#x}: {:02x?}\n",
                    label,
                    count,
                    size,
                    first,
                    &window[..],
                );
            }
        }
        match read {
            Ok(()) => {
                dev_info!(
                    dev.as_ref(),
                    "G17P render RT[{}]: aliases tilemap={:#x} heapmeta={:#x} tpc={:#x} axfb={:#x} nonzero(T/H/P/A)={:?}\n",
                    label,
                    self.render_user_aliases.tilemap_va(),
                    self.render_user_aliases.heapmeta_va(&layout).unwrap_or(0),
                    self.render_user_aliases.tpc_va(),
                    self.render_user_aliases.aux_fb_va(),
                    nonzero,
                );
                dev_info!(
                    dev.as_ref(),
                    "G17P render RT[{}]: tilemap={:x?} heapmeta={:x?} tpc={:x?}\n",
                    label,
                    tilemap,
                    heapmeta,
                    tpc,
                );
                dev_info!(
                    dev.as_ref(),
                    "G17P render RT[{}]: axfb+0={:x?} axfb+480={:x?}\n",
                    label,
                    axfb_head,
                    axfb_480,
                );
            }
            Err(error) => dev_warn!(
                dev.as_ref(),
                "G17P render RT[{}]: snapshot failed ({:?})\n",
                label,
                error,
            ),
        }
    }

    /// Read back the parameter-management state at the same launch checkpoints
    /// as the queue status pages.
    ///
    /// Native Metal's `_AGFIParameterBufferState` is not a static descriptor:
    /// TA writes its first five words while binning and the 3D admission path
    /// consumes the resulting HWPB/Scene state.  Input-descriptor parity alone
    /// therefore cannot distinguish a bad PM graph from a later KSM gate.  All
    /// objects sampled here are driver-owned shared DRAM; no SGX MMIO is read,
    /// so this remains safe after the cores have powered down for recovery.
    pub(crate) fn log_parameter_management(&mut self, dev: &AsahiDevice, label: &str) {
        let Some(pm) = self.parameter_management.as_mut() else {
            return;
        };
        let mut hwpb_head = [0u32; 8];
        let mut hwpb_config = [0u64; 12];
        let mut block_control = [0u32; 8];
        let mut stats = [0u32; 16];
        let mut selected_scene = [0u64; 16];
        let mut scene_trailer = [0u64; 8];
        let mut page_list = [0u32; 8];
        let mut block_bases = [0u64; 4];
        let mut counter = 0u32;
        let read = pm.backing.with_bytes_mut(|raw| {
            let u32_at = |at: usize| -> Result<u32> {
                Ok(u32::from_le_bytes(
                    raw.get(at..at + 4)
                        .ok_or(EINVAL)?
                        .try_into()
                        .map_err(|_| EINVAL)?,
                ))
            };
            let u64_at = |at: usize| -> Result<u64> {
                Ok(u64::from_le_bytes(
                    raw.get(at..at + 8)
                        .ok_or(EINVAL)?
                        .try_into()
                        .map_err(|_| EINVAL)?,
                ))
            };
            for (index, value) in hwpb_head.iter_mut().enumerate() {
                *value = u32_at(pm.layout.hwpb_state + index * 4)?;
            }
            // Pointers and scalar configuration from PBState +0x20..+0x8b.
            for (index, offset) in [
                0x20usize, 0x28, 0x30, 0x38, 0x40, 0x44,
                0x4c, 0x54, 0x5c, 0x64, 0x7c, 0x84,
            ]
            .into_iter()
            .enumerate()
            {
                hwpb_config[index] = u64_at(pm.layout.hwpb_state + offset)?;
            }
            for (index, value) in block_control.iter_mut().enumerate() {
                *value = u32_at(pm.layout.shared_control + index * 4)?;
            }
            for (index, value) in stats.iter_mut().enumerate() {
                *value = u32_at(pm.layout.shared_control + 0x40 + index * 4)?;
            }
            let selected = pm.layout.scene_states
                + G17P_PM_SELECTED_SCENE * G17P_PM_SCENE_ENTRY_SIZE;
            for (index, value) in selected_scene.iter_mut().enumerate() {
                *value = u64_at(selected + index * 8)?;
            }
            let trailer = pm.layout.scene_states
                + G17P_PM_SCENE_COUNT * G17P_PM_SCENE_ENTRY_SIZE;
            for (index, value) in scene_trailer.iter_mut().enumerate() {
                *value = u64_at(trailer + index * 8)?;
            }
            for (index, value) in page_list.iter_mut().enumerate() {
                *value = u32_at(pm.layout.page_list + index * 4)?;
            }
            for (index, value) in block_bases.iter_mut().enumerate() {
                *value = u64_at(pm.layout.block_page_bases + index * 8)?;
            }
            counter = u32_at(pm.layout.counter)?;
            Ok(())
        });
        match read {
            Ok(()) => {
                dev_info!(
                    dev.as_ref(),
                    "G17P render PM[{}]: HWPB head={:x?} config(+20,+28,+30,+38,+40,+44,+4c,+54,+5c,+64,+7c,+84)={:x?}\n",
                    label,
                    hwpb_head,
                    hwpb_config,
                );
                dev_info!(
                    dev.as_ref(),
                    "G17P render PM[{}]: block-control={:x?} counter={:#x} scene-qwords={:x?}\n",
                    label,
                    block_control,
                    counter,
                    selected_scene,
                );
                dev_info!(
                    dev.as_ref(),
                    "G17P render PM[{}]: stats={:x?} scene-trailer={:x?}\n",
                    label,
                    stats,
                    scene_trailer,
                );
                dev_info!(
                    dev.as_ref(),
                    "G17P render PM[{}]: page-list[0..8]={:x?} block-bases[0..4]={:x?}\n",
                    label,
                    page_list,
                    block_bases,
                );
            }
            Err(error) => dev_warn!(
                dev.as_ref(),
                "G17P render PM[{}]: snapshot failed ({:?})\n",
                label,
                error,
            ),
        }
    }

    pub(crate) fn log_reachability(&self, dev: &AsahiDevice, uat: &mmu::Uat, vm: &mmu::Vm) {
        let layout = G17PRenderStorageVaLayout {
            descriptors: self.descriptors.iova(),
            graph: self.graph.iova(),
            support: self.support.iova(),
            render_state: self.render_state.iova(),
            render_state_size: self.render_state.logical_size as u64,
            timestamps: self.timestamps.iova(),
        };
        let ranges_ok = layout.all_referenced_ranges_reachable(
            |address, size| uat.kernel_vm().covers_range(address, size, true, true),
            |address, size| vm.covers_range(address, size, true, true),
        ) && self.render_user_aliases.all_reachable_from(vm)
            && self.render_tvb.as_ref().map_or(true, |tvb| {
                vm.covers_range(tvb.iova(), tvb.logical_size as u64, true, true)
            })
            && match (&self.render_tvb, &self.render_tvb_hardware_alias) {
                (None, None) => true,
                (Some(tvb), Some(alias)) => {
                    alias.iova() == G17P_RENDER_USER_BASE + tvb.iova()
                        && alias.size() >= tvb.logical_size
                        && vm.covers_range(
                            alias.iova(),
                            tvb.logical_size as u64,
                            true,
                            true,
                        )
                }
                _ => false,
            }
            && vm.covers_range(
                self.fragment_rce.iova(),
                self.fragment_rce.logical_size as u64,
                true,
                false,
            )
            && vm.covers_range(
                self.descriptor_3d_render.iova(),
                self.descriptor_3d_render.size() as u64,
                true,
                false,
            );
        dev_info!(
            dev.as_ref(),
            "G17P render reach: ranges={} ta_high(iova={:#x} want={:#x} size={:#x} covered={}) 3d_high(iova={:#x} want={:#x} covered={})\n",
            ranges_ok,
            self.descriptor_ta_high.iova(),
            g17_submission::G17P_COLD_OPENING_TA_DESCRIPTOR_BASE,
            self.descriptor_ta_high.size(),
            uat.kernel_vm().covers_range(self.descriptor_ta_high.iova(),
                self.descriptor_ta_high.size() as u64, true, true),
            self.descriptor_3d_high.iova(),
            g17_submission::G17P_COLD_OPENING_3D_DESCRIPTOR_BASE,
            uat.kernel_vm().covers_range(self.descriptor_3d_high.iova(),
                self.descriptor_3d_high.size() as u64, true, true),
        );
        dev_info!(
            dev.as_ref(),
            "G17P render reach: ta_low(iova={:#x} want={:#x} covered={}) descriptors={:#x} graph={:#x} support={:#x} state={:#x}+{:#x} ts={:#x}\n",
            self.descriptor_ta_low.iova(),
            G17P_BOOTSTRAP_TA_DESCRIPTOR_LOW_VA,
            uat.kernel_lower_vm().covers_range(self.descriptor_ta_low.iova(),
                self.descriptor_ta_low.size() as u64, true, true),
            layout.descriptors,
            layout.graph,
            layout.support,
            layout.render_state,
            layout.render_state_size,
            layout.timestamps,
        );
        dev_info!(
            dev.as_ref(),
            "G17P render reach: compact deflake={:#x} scene={:#x} discard={:#x} tilemap={:#x} tpc={:#x} axfb={:#x} tvb={:#x} tvb-hw={:#x} rce={:#x} descriptor3d={:#x} covered={}\n",
            self.render_user_aliases.deflake_va(),
            self.render_user_aliases.scene_scratch_va().unwrap_or(0),
            self.render_user_aliases.discard_va().unwrap_or(0),
            self.render_user_aliases.tilemap_va(),
            self.render_user_aliases.tpc_va(),
            self.render_user_aliases.aux_fb_va(),
            self.render_tvb.as_ref().map_or(0, MappedObject::iova),
            self.render_tvb_hardware_alias
                .as_ref()
                .map_or(0, mmu::KernelMapping::iova),
            self.fragment_rce.iova(),
            self.descriptor_3d_render.iova(),
            self.render_user_aliases.all_reachable_from(vm)
                && self.render_tvb.as_ref().map_or(true, |tvb| {
                    vm.covers_range(tvb.iova(), tvb.logical_size as u64, true, true)
                })
                && match (&self.render_tvb, &self.render_tvb_hardware_alias) {
                    (None, None) => true,
                    (Some(tvb), Some(alias)) => {
                        alias.iova() == G17P_RENDER_USER_BASE + tvb.iova()
                            && alias.size() >= tvb.logical_size
                            && vm.covers_range(
                                alias.iova(),
                                tvb.logical_size as u64,
                                true,
                                true,
                            )
                    }
                    _ => false,
                }
                && vm.covers_range(
                    self.fragment_rce.iova(),
                    self.fragment_rce.logical_size as u64,
                    true,
                    false,
                )
                && vm.covers_range(
                    self.descriptor_3d_render.iova(),
                    self.descriptor_3d_render.size() as u64,
                    true,
                    false,
                ),
        );
    }

    /// Name the first mapping predicate a render submission fails, or `None`
    /// when every one of them holds.
    ///
    /// This used to be a single anonymous `&&` chain behind
    /// `is_reachable_from`, so any failure surfaced as a bare `EFAULT` from
    /// `begin_translated_render` with nothing said about which of ~20
    /// predicates was false. Every arm now carries a name.
    pub(crate) fn first_unreachable(
        &self,
        uat: &mmu::Uat,
        vm: &mmu::Vm,
    ) -> Option<&'static str> {
        let layout_ok = G17PRenderStorageVaLayout {
            descriptors: self.descriptors.iova(),
            graph: self.graph.iova(),
            support: self.support.iova(),
            render_state: self.render_state.iova(),
            render_state_size: self.render_state.logical_size as u64,
            timestamps: self.timestamps.iova(),
        }
        .all_referenced_ranges_reachable(
            |address, size| uat.kernel_vm().covers_range(address, size, true, true),
            |address, size| vm.covers_range(address, size, true, true),
        );

        let checks: [(&'static str, bool); 26] = [
            ("storage-va-layout", layout_ok),
            (
                "compact-render-state-aliases",
                self.render_user_aliases.all_reachable_from(vm),
            ),
            (
                "render-tvb-gart",
                self.render_tvb.as_ref().map_or(true, |tvb| {
                    vm.covers_range(tvb.iova(), tvb.logical_size as u64, true, true)
                }),
            ),
            (
                "render-tvb-hardware-alias",
                match (&self.render_tvb, &self.render_tvb_hardware_alias) {
                    (None, None) => true,
                    (Some(tvb), Some(alias)) => {
                        alias.iova() == G17P_RENDER_USER_BASE + tvb.iova()
                            && alias.size() >= tvb.logical_size
                            && vm.covers_range(
                                alias.iova(),
                                tvb.logical_size as u64,
                                true,
                                true,
                            )
                    }
                    _ => false,
                },
            ),
            (
                "fragment-rce-app-gart",
                vm.covers_range(
                    self.fragment_rce.iova(),
                    self.fragment_rce.logical_size as u64,
                    true,
                    false,
                ),
            ),
            (
                "descriptor-ta-high-identity",
                self.descriptor_ta_high.iova()
                    == g17_submission::G17P_COLD_OPENING_TA_DESCRIPTOR_BASE
                    && self.descriptor_ta_high.size() == mmu::UAT_PGSZ,
            ),
            (
                "descriptor-ta-high-mapped",
                uat.kernel_vm().covers_range(
                    self.descriptor_ta_high.iova(),
                    self.descriptor_ta_high.size() as u64,
                    true,
                    true,
                ),
            ),
            (
                "descriptor-3d-high-identity",
                self.descriptor_3d_high.iova()
                    == g17_submission::G17P_COLD_OPENING_3D_DESCRIPTOR_BASE
                    && self.descriptor_3d_high.size() == mmu::UAT_PGSZ,
            ),
            (
                "descriptor-3d-high-mapped",
                uat.kernel_vm().covers_range(
                    self.descriptor_3d_high.iova(),
                    self.descriptor_3d_high.size() as u64,
                    true,
                    true,
                ),
            ),
            (
                "descriptor-ta-low-identity",
                self.descriptor_ta_low.iova() == G17P_BOOTSTRAP_TA_DESCRIPTOR_LOW_VA
                    && self.descriptor_ta_low.size() == mmu::UAT_PGSZ,
            ),
            (
                "descriptor-ta-low-mapped",
                uat.kernel_lower_vm().covers_range(
                    self.descriptor_ta_low.iova(),
                    self.descriptor_ta_low.size() as u64,
                    true,
                    false,
                ),
            ),
            (
                "descriptor-3d-low-identity",
                self.descriptor_3d_low.iova() == G17P_BOOTSTRAP_3D_DESCRIPTOR_LOW_VA
                    && self.descriptor_3d_low.size() == mmu::UAT_PGSZ,
            ),
            (
                "descriptor-3d-low-mapped",
                uat.kernel_lower_vm().covers_range(
                    self.descriptor_3d_low.iova(),
                    self.descriptor_3d_low.size() as u64,
                    true,
                    false,
                ),
            ),
            (
                "descriptor-3d-render-size",
                self.descriptor_3d_render.size() == mmu::UAT_PGSZ,
            ),
            (
                "descriptor-3d-render-mapped",
                vm.covers_range(
                    self.descriptor_3d_render.iova(),
                    self.descriptor_3d_render.size() as u64,
                    true,
                    false,
                ),
            ),
            (
                "ta-context-high-mapped",
                uat.kernel_vm().covers_range(
                    G17P_BOOTSTRAP_TA_CONTEXT_HIGH_VA,
                    G17P_BOOTSTRAP_CONTEXT_PEER_SIZE as u64,
                    true,
                    true,
                ),
            ),
            (
                "ta-context-low",
                self.ta_context_low.iova() == G17P_BOOTSTRAP_TA_CONTEXT_LOW_VA
                    && self.ta_context_low.size() == G17P_BOOTSTRAP_CONTEXT_PEER_SIZE
                    && uat.kernel_lower_vm().covers_range(
                        self.ta_context_low.iova(),
                        self.ta_context_low.size() as u64,
                        true,
                        false,
                    ),
            ),
            (
                "3d-context-high-mapped",
                uat.kernel_vm().covers_range(
                    G17P_BOOTSTRAP_3D_CONTEXT_HIGH_VA,
                    G17P_BOOTSTRAP_CONTEXT_PEER_SIZE as u64,
                    true,
                    true,
                ),
            ),
            (
                "3d-context-low",
                self.fragment_context_low.iova() == G17P_BOOTSTRAP_3D_CONTEXT_LOW_VA
                    && self.fragment_context_low.size() == G17P_BOOTSTRAP_CONTEXT_PEER_SIZE
                    && uat.kernel_lower_vm().covers_range(
                        self.fragment_context_low.iova(),
                        self.fragment_context_low.size() as u64,
                        true,
                        false,
                    ),
            ),
            (
                "ta-status-page",
                self.render_ta_status.as_ref().is_some_and(|mapping| {
                    mapping.iova() == G17P_BOOTSTRAP_TA_STATUS_VA
                        && mapping.size() == mmu::UAT_PGSZ
                        && uat.kernel_vm().covers_range(
                            G17P_BOOTSTRAP_TA_STATUS_VA,
                            mmu::UAT_PGSZ as u64,
                            true,
                            true,
                        )
                }),
            ),
            (
                "3d-status-page",
                self.render_3d_status.as_ref().is_some_and(|mapping| {
                    mapping.iova() == G17P_BOOTSTRAP_3D_STATUS_VA
                        && mapping.size() == mmu::UAT_PGSZ
                        && uat.kernel_vm().covers_range(
                            G17P_BOOTSTRAP_3D_STATUS_VA,
                            mmu::UAT_PGSZ as u64,
                            true,
                            true,
                        )
                }),
            ),
            (
                "control-directory",
                vm.covers_range(
                    G17P_CONTROL_DIRECTORY_VA,
                    G17P_CONTROL_DIRECTORY_SIZE as u64,
                    true,
                    true,
                ),
            ),
            (
                "control-operand-table",
                vm.covers_range(
                    G17P_CONTROL_OPERAND_TABLE_VA,
                    g17_initdata::CONTROL_OPERAND_TABLE_SIZE as u64,
                    true,
                    true,
                ),
            ),
            // The cold render FList publishes 22 initial blocks and four first-
            // grow blocks. Their captured low addresses deliberately overlap
            // context-0 aliases, but this check is against the client low root;
            // native uses the same split-root topology.
            (
                "control-operand-buffers",
                (0..G17P_CONTROL_OPERAND_ACTIVE_BLOCK_COUNT).all(|index| {
                    g17p_control_operand_buffer_va(index).is_some_and(|address| {
                        vm.covers_range(
                            address,
                            G17P_CONTROL_OPERAND_BUFFER_SIZE as u64,
                            true,
                            true,
                        )
                    })
                }),
            ),
            (
                "primary-index-page",
                vm.covers_range(
                    g17_submission::G17P_PARTIAL_OPENING_PRIMARY_INDEX_GPU_VA,
                    mmu::UAT_PGSZ as u64,
                    true,
                    true,
                ),
            ),
            (
                "pm-page-metrics-client-alias",
                vm.covers_range(
                    mmu::T8140_PARAMETER_METRICS_LOW_VA,
                    mmu::T8140_PARAMETER_METRICS_SIZE as u64,
                    true,
                    true,
                ),
            ),
        ];

        checks
            .into_iter()
            .find_map(|(name, ok)| if ok { None } else { Some(name) })
            .or_else(|| {
                if self
                    .user_timestamp_aliases
                    .iter()
                    .all(|mapping| vm.covers_range(mapping.iova(), mapping.size() as u64, false, true))
                {
                    None
                } else {
                    Some("user-timestamp-aliases")
                }
            })
    }


    fn optional_addresses(&self, tiling: bool) -> g17_submission::G17PColdOpeningOptionalAddresses {
        let support = self.support.iova();
        let shared_object = if tiling && g17p_render_native_fragment_hwpb_mode() == 2 {
            // A single-variable diagnostic for the firmware's HWPB bind: both
            // final descriptors keep the native PM HWPB pointer and every ID
            // sink stays coherent, but ConfigUpdate declines to register it.
            0
        } else if tiling && g17p_render_native_fragment_hwpb_mode() == 3 {
            support + G17P_RENDER_PACKED_SHARED as u64
        } else if g17p_render_native_fragment_hwpb_enabled() {
            self.parameter_management.as_ref().map_or(
                support + G17P_RENDER_PACKED_SHARED as u64,
                G17PParameterManagement::hwpb_state_va,
            )
        } else {
            support + G17P_RENDER_PACKED_SHARED as u64
        };
        g17p_cold_opening_optional_addresses(
            support,
            shared_object,
            self.ta_hardware_buffer_id,
            self.owner_pid,
            tiling,
        )
    }

    pub(crate) fn stage_fragment(&mut self, submission_ordinal: u32) -> Result {
        let graph_va = self.graph.iova();
        let items = [
            g17_submission::G17P_COLD_OPENING_3D_DESCRIPTOR_BASE,
            graph_va + Self::FRAGMENT_OPTIONAL as u64,
            graph_va + Self::FRAGMENT_EVENT as u64,
        ];
        let optional_addresses = self.optional_addresses(false);
        let tag16 = g17p_render_tag16_enabled();
        // The stamp this path's tag-14 announces is `ordinal + 1`
        // (`apply_g17p_retained_event`), which is the 1 the firmware reported
        // in its 0x41 receipt. Tag-16 must name that same stamp.
        let entry_stamp = submission_ordinal.checked_add(1).ok_or(EINVAL)?;
        let entry_signal_va = graph_va + Self::FRAGMENT_ENTRY_SIGNAL as u64;
        self.graph.with_bytes_mut(|raw| {
            for (index, address) in items.iter().enumerate() {
                put_u64(raw, Self::FRAGMENT_RING + index * 8, *address);
            }
            g17_submission::apply_g17p_retained_optional(
                g17_submission::G17PColdOpeningStage::Fragment,
                optional_addresses,
                submission_ordinal,
                g17p_render_generation_bias_enabled(),
                g17p_render_queue_id(false),
                g17p_render_install(false),
                self.command_context.context_id() as u16,
                &mut raw[Self::FRAGMENT_OPTIONAL
                    ..Self::FRAGMENT_OPTIONAL + g17_submission::G17P_COLD_OPENING_OPTIONAL_SIZE],
            )
            .map_err(|_| EINVAL)?;
            g17_submission::apply_g17p_retained_event(
                g17_submission::G17PColdOpeningStage::Fragment,
                submission_ordinal,
                g17p_render_queue_id(false),
                &mut raw[Self::FRAGMENT_EVENT
                    ..Self::FRAGMENT_EVENT + g17_submission::G17P_COLD_OPENING_EVENT_SIZE],
            )
            .map_err(|_| EINVAL)?;
            if tag16 {
                g17_submission::encode_g17p_compute_optional_event(
                    g17_submission::G17PComputeOptionalEvent {
                        queue_id: g17p_render_queue_id(false),
                        // `old_timestamp` means the PREVIOUS stamp, not this
                        // submission's own. Compute's depth wall was exactly
                        // this field naming the stamp being queued, which let
                        // the firmware retire the entry before the dispatch
                        // ran; see the tag-16 fix that took compute to 100/100.
                        old_timestamp: u64::from(entry_stamp).saturating_sub(1),
                    },
                    &mut raw[Self::FRAGMENT_ENTRY_SIGNAL
                        ..Self::FRAGMENT_ENTRY_SIGNAL
                            + g17_submission::G17P_COMPUTE_OPTIONAL_EVENT_SIZE],
                )
                .map_err(|_| EINVAL)?;
                put_u64(raw, Self::FRAGMENT_RING + 3 * 8, entry_signal_va);
            }
            core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
            put_u32(
                raw,
                Self::FRAGMENT_POINTERS + 0x40,
                if tag16 { 4 } else { 3 },
            );
            Ok(())
        })
    }

    /// Bind the emitted 3D command to the exact timestamp the physical SKSM
    /// fragment queue is about to consume.
    ///
    /// Native computes this immediately before command publication. Keeping
    /// the write beside the prefix transition prevents descriptor restaging or
    /// retained-ring wrap from fabricating the timestamp independently.
    pub(crate) fn stage_fragment_completion_scratch(
        &mut self,
        current_timestamp: u64,
    ) -> Result<u64> {
        let value = g17_submission::encode_g17p_3d_completion_scratch(
            current_timestamp,
            g17_submission::G17P_3D_COMPLETION_SCRATCH_PAYLOAD,
        )
        .map_err(|_| EINVAL)?;
        self.descriptors.with_bytes_mut(|raw| {
            let fragment = raw.get_mut(0x4000..0x8000).ok_or(EINVAL)?;
            put_u64(fragment, 0x2160, value);
            core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
            Ok(())
        })?;
        Ok(value)
    }

    /// Dump the fragment descriptor after every submission-time mutation has
    /// been applied.  The construction-time `3d-desc bootstrap` dump is
    /// deliberately earlier than `stage_fragment_completion_scratch`, so it
    /// must not be used for byte-for-byte comparison with a native descriptor
    /// captured at publication.  Keep this second, explicitly labelled dump
    /// beside the last descriptor write so trace comparisons cannot silently
    /// mix the two lifecycle points again.
    pub(crate) fn dump_submit_ready_fragment_descriptor(
        &mut self,
        dev: &AsahiDevice,
        submission_ordinal: u32,
    ) -> Result {
        if *crate::module_parameters::g17p_render_dump_descriptor.value() == 0 {
            return Ok(());
        }

        let descriptor_3d_render_va = self.descriptor_3d_render.iova();
        self.descriptors.with_bytes_mut(|raw| {
            let fragment = raw.get(0x4000..0x8000).ok_or(EINVAL)?;
            dev_info!(
                dev.as_ref(),
                "G17P 3d-desc submit-ready context={} ordinal={} low_va={:#x} bytes={:#x}\n",
                self.command_context.context_id(),
                submission_ordinal,
                descriptor_3d_render_va,
                g17_render::FRAGMENT_DESCRIPTOR_SIZE,
            );
            for (index, chunk) in fragment[..g17_render::FRAGMENT_DESCRIPTOR_SIZE]
                .chunks(16)
                .enumerate()
            {
                dev_info!(
                    dev.as_ref(),
                    "G17P 3d-desc submit-ready +{:#05x} {:02x?}\n",
                    index * 16,
                    chunk,
                );
            }
            Ok(())
        })
    }

    /// Prove that every RCE binding resolves through the submitted app VM to
    /// the descriptor bytes the CPU just built.  A syntactically correct
    /// binding is insufficient if the selected hardware context names a
    /// different page-table root; that failure reaches KSM pipe assignment
    /// and then looks exactly like an RCE engine which never performs its
    /// first register write.
    pub(crate) fn audit_fragment_rce_mapping(
        &mut self,
        dev: &AsahiDevice,
        vm: &mmu::Vm,
    ) -> Result {
        if *crate::module_parameters::g17p_render_dump_descriptor.value() == 0 {
            return Ok(());
        }

        let base = self.descriptor_3d_render.iova();
        let mut roots = [(0u64, 0u64); 4];
        vm.context_roots(&mut roots)?;
        dev_info!(
            dev.as_ref(),
            "G17P 3d-rce VM audit: base={:#x} vm-root={:#x} ctx0=[{:#x},{:#x}] ctx1=[{:#x},{:#x}] ctx2=[{:#x},{:#x}] ctx3=[{:#x},{:#x}]\n",
            base,
            vm.page_table_root(),
            roots[0].0,
            roots[0].1,
            roots[1].0,
            roots[1].1,
            roots[2].0,
            roots[2].1,
            roots[3].0,
            roots[3].1,
        );

        for (tag, offset) in [(56u8, 0xa0usize), (10, 0x7c0), (17, 0xee0), (10, 0x1600)] {
            let address = base.checked_add(offset as u64).ok_or(EOVERFLOW)?;
            let physical = vm.translate_iova(address)?;
            let mut gpu_bytes = [0u8; 16];
            vm.read_bytes(address, &mut gpu_bytes)?;
            let mut cpu_bytes = [0u8; 16];
            self.descriptors.with_bytes_mut(|raw| {
                cpu_bytes.copy_from_slice(
                    raw.get(0x4000 + offset..0x4000 + offset + 16)
                        .ok_or(ERANGE)?,
                );
                Ok(())
            })?;
            if gpu_bytes != cpu_bytes {
                dev_err!(
                    dev.as_ref(),
                    "G17P 3d-rce VM audit: tag={} address={:#x} physical={:#x} GPU bytes {:02x?} != descriptor bytes {:02x?}\n",
                    tag,
                    address,
                    physical,
                    gpu_bytes,
                    cpu_bytes,
                );
                return Err(EIO);
            }
            dev_info!(
                dev.as_ref(),
                "G17P 3d-rce VM audit: tag={} address={:#x} physical={:#x} bytes={:02x?}\n",
                tag,
                address,
                physical,
                gpu_bytes,
            );
        }
        Ok(())
    }

    /// B1's 3D activation routine stores 1 here before it inspects any of the
    /// later launch-control fields or resumes the PDM scheduler.  Unlike an
    /// SGX register snapshot this is ordinary pinned descriptor DRAM, so it is
    /// safe to read after recovery and separates pre-activation KSM stalls
    /// from failures inside the launch path.
    pub(crate) fn fragment_activation_word(&mut self) -> Result<u32> {
        self.descriptors.with_bytes_mut(|raw| {
            let offset = 0x4000usize.checked_add(0x20f0).ok_or(EOVERFLOW)?;
            Ok(u32::from_le_bytes(
                raw.get(offset..offset + 4)
                    .ok_or(ERANGE)?
                    .try_into()
                    .map_err(|_| ERANGE)?,
            ))
        })
    }

    /// Read the two superficially similar but semantically distinct coordinate
    /// sets at the TA-to-3D activation boundary.
    ///
    /// B1 registers the 0x9a0-byte TA record on the bit-4 new-work interrupt;
    /// its +0x79c/+0x7a8 names the physical 3D KSM QID/stamp. FUN_14cd4 waits
    /// for stamp-1 and FUN_162f8 later indexes that exact queue entry. The
    /// type-1 3D record's +0x79c/+0x7a0/+0x7a8 instead forms an SKU register
    /// stream header: reserved zero, stream pointer, and {0x42c bytes, 0x59
    /// records}. It is not a QID-0/slot-89 dependency.
    pub(crate) fn render_activation_coordinates(&mut self) -> Result<(u32, u64, u32, u64, u64)> {
        self.descriptors.with_bytes_mut(|raw| {
            let tiling = raw.get(0x0000..0x4000).ok_or(ERANGE)?;
            let fragment = raw.get(0x4000..0x8000).ok_or(ERANGE)?;
            let ta_qid = u32::from_le_bytes(
                tiling
                    .get(0x079c..0x07a0)
                    .ok_or(ERANGE)?
                    .try_into()
                    .map_err(|_| ERANGE)?,
            );
            let ta_stamp = u64::from_le_bytes(
                tiling
                    .get(0x07a8..0x07b0)
                    .ok_or(ERANGE)?
                    .try_into()
                    .map_err(|_| ERANGE)?,
            );
            let sku_reserved = u32::from_le_bytes(
                fragment
                    .get(0x079c..0x07a0)
                    .ok_or(ERANGE)?
                    .try_into()
                    .map_err(|_| ERANGE)?,
            );
            let sku_pointer = u64::from_le_bytes(
                fragment
                    .get(0x07a0..0x07a8)
                    .ok_or(ERANGE)?
                    .try_into()
                    .map_err(|_| ERANGE)?,
            );
            let sku_header = u64::from_le_bytes(
                fragment
                    .get(0x07a8..0x07b0)
                    .ok_or(ERANGE)?
                    .try_into()
                    .map_err(|_| ERANGE)?,
            );
            Ok((ta_qid, ta_stamp, sku_reserved, sku_pointer, sku_header))
        })
    }

    /// Host-visible inputs to B1 `FUN_14cd4`'s state-1 admission decision.
    /// These bytes are sampled from the final TA command, not inferred from
    /// the builder, so a firmware mutation between publication and recovery is
    /// visible. The resource-table and job-substate operands are firmware
    /// private; KTrace supplies the corresponding path witnesses.
    pub(crate) fn render_activation_gate_words(&mut self) -> Result<[u32; 5]> {
        self.descriptors.with_bytes_mut(|raw| {
            let tiling = raw.get(0x0000..0x4000).ok_or(ERANGE)?;
            let u32_at = |offset: usize| -> Result<u32> {
                Ok(u32::from_le_bytes(
                    tiling
                        .get(offset..offset + 4)
                        .ok_or(ERANGE)?
                        .try_into()
                        .map_err(|_| ERANGE)?,
                ))
            };
            Ok([
                u32_at(0x07a4)?,
                u32_at(0x0866)?,
                u32_at(0x0892)?,
                u32_at(0x08ba)?,
                u32::from(*tiling.get(0x08cb).ok_or(ERANGE)?),
            ])
        })
    }

    /// The first half of a 3D group: item 0 (the fragment command record) and
    /// item 1 (the tag-15 ConfigUpdate), with the queue's item producer left at
    /// 2. Native's `AGX3DChannelSKSM::submitBuffer` publishes exactly these two
    /// pointers, then configures the QID and writes the KSM kick entry, and
    /// only then publishes the AddKicks that takes item 2 -- so the split is
    /// what lets the SKSM half sit where the captures put it.
    pub(crate) fn stage_fragment_prefix(&mut self, submission_ordinal: u32) -> Result {
        let graph_va = self.graph.iova();
        let items = [
            g17_submission::G17P_COLD_OPENING_3D_DESCRIPTOR_BASE,
            graph_va + Self::FRAGMENT_OPTIONAL as u64,
        ];
        let optional_addresses = self.optional_addresses(false);
        self.graph.with_bytes_mut(|raw| {
            for (index, address) in items.iter().enumerate() {
                put_u64(raw, Self::FRAGMENT_RING + index * 8, *address);
            }
            g17_submission::apply_g17p_retained_optional(
                g17_submission::G17PColdOpeningStage::Fragment,
                optional_addresses,
                submission_ordinal,
                g17p_render_generation_bias_enabled(),
                g17p_render_queue_id(false),
                g17p_render_install(false),
                self.command_context.context_id() as u16,
                &mut raw[Self::FRAGMENT_OPTIONAL
                    ..Self::FRAGMENT_OPTIONAL + g17_submission::G17P_COLD_OPENING_OPTIONAL_SIZE],
            )
            .map_err(|_| EINVAL)?;
            put_u32(raw, Self::FRAGMENT_POINTERS + 0x40, 2);
            Ok(())
        })
    }

    /// The second half of a 3D group: item 2 (the tag-14 AddKicks announcing
    /// `entry_stamp`) and the item producer transition 2 -> 3.
    pub(crate) fn stage_fragment_kick(&mut self, entry_stamp: u32) -> Result {
        let graph_va = self.graph.iova();
        let event_va = graph_va + Self::FRAGMENT_EVENT as u64;
        let entry_signal_va = graph_va + Self::FRAGMENT_ENTRY_SIGNAL as u64;
        let tag16 = g17p_render_tag16_enabled();
        self.graph.with_bytes_mut(|raw| {
            put_u64(raw, Self::FRAGMENT_RING + 2 * 8, event_va);
            g17_submission::apply_g17p_retained_event_with_stamp(
                g17_submission::G17PColdOpeningStage::Fragment,
                entry_stamp,
                g17p_render_queue_id(false),
                &mut raw[Self::FRAGMENT_EVENT
                    ..Self::FRAGMENT_EVENT + g17_submission::G17P_COLD_OPENING_EVENT_SIZE],
            )
            .map_err(|_| EINVAL)?;
            if tag16 {
                // Tag-16, the entry-signal record (`selector 0x10`). Compute
                // publishes this on first bind and render never did; the
                // firmware then has the kick entry in the ring with nothing
                // telling it the producer advanced, which is exactly the
                // published-but-never-started shape the tiler shows.
                g17_submission::encode_g17p_compute_optional_event(
                    g17_submission::G17PComputeOptionalEvent {
                        queue_id: g17p_render_queue_id(false),
                        // `old_timestamp` means the PREVIOUS stamp, not this
                        // submission's own. Compute's depth wall was exactly
                        // this field naming the stamp being queued, which let
                        // the firmware retire the entry before the dispatch
                        // ran; see the tag-16 fix that took compute to 100/100.
                        old_timestamp: u64::from(entry_stamp).saturating_sub(1),
                    },
                    &mut raw[Self::FRAGMENT_ENTRY_SIGNAL
                        ..Self::FRAGMENT_ENTRY_SIGNAL
                            + g17_submission::G17P_COMPUTE_OPTIONAL_EVENT_SIZE],
                )
                .map_err(|_| EINVAL)?;
                put_u64(raw, Self::FRAGMENT_RING + 3 * 8, entry_signal_va);
            }
            core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
            put_u32(
                raw,
                Self::FRAGMENT_POINTERS + 0x40,
                if tag16 { 4 } else { 3 },
            );
            Ok(())
        })
    }

    pub(crate) fn prepare_late_tiling_publication(
        &mut self,
        submission_ordinal: u32,
    ) -> Result<G17PLateTilingPublication> {
        let stamp = submission_ordinal.checked_add(1).ok_or(EINVAL)?;
        self.prepare_late_tiling_publication_with_stamp(submission_ordinal, stamp)
    }

    /// Same as [`Self::prepare_late_tiling_publication`], except that the
    /// tag-14 AddKicks stamp comes from the TA queue's KSM producer instead of
    /// being derived from the submission ordinal. The render SKSM half must use
    /// this form so the stamp the 3D group's barrier waits on and the stamp the
    /// TA group's AddKicks announces are the same value by construction.
    pub(crate) fn prepare_late_tiling_publication_with_stamp(
        &mut self,
        submission_ordinal: u32,
        entry_stamp: u32,
    ) -> Result<G17PLateTilingPublication> {
        let graph_va = self.graph.iova();
        let bytes = build_g17p_late_tiling_bytes(
            g17_submission::G17P_COLD_OPENING_TA_DESCRIPTOR_BASE,
            graph_va,
            self.optional_addresses(true),
            self.command_context.context_id() as u16,
            submission_ordinal,
            entry_stamp,
            g17p_render_tag16_enabled(),
            g17p_render_native_pool_a_first_record_enabled(),
        )
        .map_err(|_| EINVAL)?;

        if Self::TILING_EVENT + bytes.event.len() > self.graph.logical_size
            || G17P_RENDER_SHARED_CONTROL_INNER + 4 > self.support.logical_size
        {
            return Err(EINVAL);
        }
        let graph = self.graph.object.vmap()?.as_mut_ptr();
        let support = self.support.object.vmap()?.as_mut_ptr();
        Ok(G17PLateTilingPublication {
            graph,
            support,
            bytes,
        })
    }

    /// Read back the 3D queue's tag-15 ConfigUpdate header from the graph, as
    /// actually published. Measured, not assumed.
    pub(crate) fn fragment_config_update_header(
        &mut self,
    ) -> Result<G17PRenderConfigUpdateHeader> {
        let mut header = None;
        self.graph.with_bytes_mut(|raw| {
            header = decode_g17p_render_config_update(
                raw.get(Self::FRAGMENT_OPTIONAL
                    ..Self::FRAGMENT_OPTIONAL
                        + g17_submission::G17P_COLD_OPENING_OPTIONAL_SIZE)
                    .ok_or(EINVAL)?,
            );
            Ok(())
        })?;
        header.ok_or(EINVAL)
    }

    pub(crate) fn tiling_config_update_header(
        &mut self,
    ) -> Result<G17PRenderConfigUpdateHeader> {
        let mut header = None;
        self.graph.with_bytes_mut(|raw| {
            header = decode_g17p_render_config_update(
                raw.get(G17P_USER_TILING_OPTIONAL
                    ..G17P_USER_TILING_OPTIONAL
                        + g17_submission::G17P_COLD_OPENING_OPTIONAL_SIZE)
                    .ok_or(EINVAL)?,
            );
            Ok(())
        })?;
        header.ok_or(EINVAL)
    }

    /// Read the firmware-writable context descriptor named by ConfigUpdate
    /// +0x4a.  The tag-15 handler changes byte 0x33 from 0xff to the requested
    /// QoS hardware-buffer slot (+0x56), so this is a host-safe proof that the
    /// context/slot bind actually completed before a later render stall.
    pub(crate) fn render_channel_control_prefix(&mut self) -> Result<[u8; 0x38]> {
        core::sync::atomic::fence(core::sync::atomic::Ordering::Acquire);
        let vmap = self.support.object.vmap()?;
        let bytes = unsafe {
            // SAFETY: the VMap covers the complete support object and the
            // fixed channel-control range is checked below.
            core::slice::from_raw_parts(vmap.as_ptr(), self.support.logical_size)
        };
        let range = bytes
            .get(
                G17P_RENDER_CHANNEL_CONTROL
                    ..G17P_RENDER_CHANNEL_CONTROL
                        + g17_submission::G17P_COLD_OPENING_CHANNEL_CONTROL_SIZE,
            )
            .ok_or(EINVAL)?;
        let mut prefix = [0u8; 0x38];
        prefix.copy_from_slice(&range[..0x38]);
        Ok(prefix)
    }

    pub(crate) fn queue_gpu_vas(&self) -> (u64, u64) {
        (
            self.graph.iova() + Self::FRAGMENT_QUEUE as u64,
            self.graph.iova() + Self::TILING_QUEUE as u64,
        )
    }

    pub(crate) fn snapshot(&mut self) -> Result<G17PUserRenderQueueState> {
        let mut state = G17PUserRenderQueueState {
            tiling: g17_completion::QueueIndices {
                done: 0,
                read: 0,
                write: 0,
            },
            fragment: g17_completion::QueueIndices {
                done: 0,
                read: 0,
                write: 0,
            },
            job_list_empty: false,
            tiling_timestamps: [0; 2],
            fragment_timestamps: [0; 2],
        };
        let graph_va = self.graph.iova();
        self.graph.with_bytes_mut(|raw| {
            let read = |offset: usize| -> Result<u32> {
                Ok(u32::from_le_bytes(
                    raw[offset..offset + 4].try_into().map_err(|_| EINVAL)?,
                ))
            };
            state.tiling = g17_completion::QueueIndices {
                done: read(Self::TILING_POINTERS)?,
                read: read(Self::TILING_POINTERS + 0x30)?,
                write: read(Self::TILING_POINTERS + 0x40)?,
            };
            state.fragment = g17_completion::QueueIndices {
                done: read(Self::FRAGMENT_POINTERS)?,
                read: read(Self::FRAGMENT_POINTERS + 0x30)?,
                write: read(Self::FRAGMENT_POINTERS + 0x40)?,
            };
            let first = u64::from_le_bytes(
                raw[Self::JOB_LIST..Self::JOB_LIST + 8]
                    .try_into()
                    .map_err(|_| EINVAL)?,
            );
            let last = u64::from_le_bytes(
                raw[Self::JOB_LIST + 8..Self::JOB_LIST + 16]
                    .try_into()
                    .map_err(|_| EINVAL)?,
            );
            state.job_list_empty = first == 0 && last == graph_va + Self::JOB_LIST as u64;
            Ok(())
        })?;
        self.timestamps.with_bytes_mut(|raw| {
            let tiling_start = u64::from_le_bytes(raw[0..8].try_into().map_err(|_| EINVAL)?);
            let tiling_end = u64::from_le_bytes(raw[8..16].try_into().map_err(|_| EINVAL)?);
            let fragment_start = u64::from_le_bytes(raw[16..24].try_into().map_err(|_| EINVAL)?);
            let fragment_end = u64::from_le_bytes(raw[24..32].try_into().map_err(|_| EINVAL)?);
            state.tiling_timestamps = [tiling_start, tiling_end];
            state.fragment_timestamps = [fragment_start, fragment_end];
            Ok(())
        })?;
        Ok(state)
    }

    /// Restore the shared intrusive-list head after both queues are idle and
    /// both GPU stages have written their owned timestamps.
    pub(crate) fn quiesce_completed_job_list(&mut self, state: G17PUserRenderQueueState) -> Result {
        if !g17p_render_can_reset_job_list(
            state.tiling,
            state.fragment,
            state.tiling_timestamps,
            state.fragment_timestamps,
        ) {
            return Err(EBUSY);
        }
        let job_list = self.graph.iova() + Self::JOB_LIST as u64;
        self.graph.with_bytes_mut(|raw| {
            put_u64(raw, Self::JOB_LIST, 0);
            put_u64(raw, Self::JOB_LIST + 0x08, job_list);
            core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
            Ok(())
        })
    }
}

#[cfg(not(test))]
impl G17PClRuntimeStorage {
    pub(crate) fn new(
        dev: &AsahiDevice,
        uat: &mmu::Uat,
        geometry: g17_submission::G17SksmQueueGeometry,
    ) -> Result<Self> {
        geometry.validate().map_err(|_| EINVAL)?;
        let backing_size = geometry.backing_size();
        let mapping_alignment = geometry.mapping_alignment(mmu::UAT_PGSZ as u64);
        // AGXKSMKickQueue publishes this array through an uncached GPU
        // read-write low mapping. KEXT_PROVEN: AGXKSMKickQueue::init at
        // 0xfffffe0008a69898 creates the mapping whose address becomes
        // queue+0x80 -- the base encoded into the +0x4098 config payload --
        // with GPU mapping options 0x0000000600000085, while the paired view
        // that becomes queue+0x88 uses 0x0000000800000084 at 0xfffffe0008a69784.
        // The two differ only in option bit 0, which is set exactly on the view
        // published to KSMFE. The same 0x...87 / 0x...84 split appears between
        // the two mappings of AGXAccelerator::allocSingleKSMCompletionQueue.
        // A read-only low view is therefore not native, and KSMFE is not a
        // pure consumer of this ring.
        let entries = MappedObject::new_wc_in_range(
            dev,
            uat.kernel_lower_vm(),
            g17p_cl_dynamic_low_va_range(),
            backing_size,
            mapping_alignment,
            mmu::PROT_GPU_SHARED_RW,
        )?;
        // Native queue +0x80/+0x88 name the lower and upper mappings of one
        // 0x20000 SKSM entry allocation. Alias the existing KernelMapping's
        // exact GEM subrange so size and backing identity cannot diverge. The
        // firmware-high view intentionally retains its separate RW access
        // class; native queue +0x80 and +0x88 are not protection-identical.
        let entries_high = entries.mapping.map_alias_into_range(
            uat.kernel_vm(),
            uat.kernel_va_range()?,
            mmu::PROT_FW_SHARED_RW,
        )?;
        if entries.mapping.size() != backing_size || entries_high.size() != backing_size
        {
            return Err(EINVAL);
        }
        let shared_support = MappedObject::new_wc(
            dev,
            uat,
            G17P_CL_SHARED_SUPPORT_SIZE,
            mmu::UAT_PGSZ as u64,
            mmu::PROT_GPU_FW_SHARED_RW,
        )?;
        let channel_control = MappedObject::new_wc(
            dev,
            uat,
            G17P_CL_CHANNEL_CONTROL_SIZE,
            mmu::UAT_PGSZ as u64,
            mmu::PROT_GPU_FW_SHARED_RW,
        )?;
        let support_state = MappedObject::new_wc(
            dev,
            uat,
            G17P_CL_SUPPORT_STATE_SIZE,
            mmu::UAT_PGSZ as u64,
            mmu::PROT_GPU_FW_SHARED_RW,
        )?;
        let scheduler_state = MappedObject::new_wc(
            dev,
            uat,
            G17P_CL_SUPPORT_STATE_SIZE,
            mmu::UAT_PGSZ as u64,
            mmu::PROT_GPU_FW_SHARED_RW,
        )?;
        let operand_table = MappedObject::new_wc(
            dev,
            uat,
            G17P_CL_OPERAND_TABLE_SIZE,
            mmu::UAT_PGSZ as u64,
            mmu::PROT_GPU_FW_SHARED_RW,
        )?;

        Ok(Self {
            entries,
            entries_high,
            geometry,
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

    /// Retain the SKSM entry backing at its canonical low VA in the active
    /// client VM. ConfigureHardware publishes this address directly to KSMFE,
    /// so a separately allocated client alias cannot satisfy the queue ABI.
    pub(crate) fn map_user_entry_alias(
        &mut self,
        vm: &mmu::Vm,
    ) -> Result<mmu::KernelMapping> {
        let address = self.entries.iova();
        self.entries
            .map_alias_at(vm, address, mmu::PROT_GPU_SHARED_RW)
    }

    /// Return the two GPU aliases of the same retained SKSM entry backing.
    pub(crate) fn entry_aliases(&self) -> Result<g17_compute::ComputeSksmEntryAliases> {
        g17_compute::ComputeSksmEntryAliases::new(
            self.entries.iova(),
            self.entries_high.iova(),
            self.geometry.backing_size(),
            self.geometry.allocation_alignment(),
        )
        .map_err(|_| EINVAL)
    }

    /// Make both queue-entry aliases visible to the already-running G17P UAT
    /// before the write-only SKSM configuration transaction.
    pub(crate) fn prepare_live_mappings(&self, uat: &mmu::Uat) -> Result {
        let low = &self.entries.mapping;
        let high = &self.entries_high;
        if low.size() == 0
            || low.size() != high.size()
            || !uat.kernel_lower_vm().covers_range(
                low.iova(),
                low.size() as u64,
                true,
                true,
            )
            || !uat.kernel_vm().covers_range(
                high.iova(),
                high.size() as u64,
                false,
                false,
            )
        {
            return Err(EFAULT);
        }

        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
        // ASID 2 is a live translation regime once the compute kick's context
        // has a root of its own (`g17p_ctx2_root`, see
        // `mmu::Uat::install_t8140_compute_context_alias`); it shares these
        // very page tables, so it needs the same maintenance.
        let ctx_mask = *crate::module_parameters::g17p_ctx2_root.value();
        let asids: &[u8] = match ctx_mask & 3 {
            0 => &[0u8, 1u8],
            1 => &[0u8, 1u8, 2u8],
            2 => &[0u8, 1u8, 3u8],
            _ => &[0u8, 1u8, 2u8, 3u8],
        };
        for asid in asids {
            mem::tlbi_range(*asid, low.iova() as usize, low.size());
            mem::tlbi_range(*asid, high.iova() as usize, high.size());
        }
        mem::sync();
        Ok(())
    }

    pub(crate) fn shared_support_gpu_va(&self) -> u64 {
        self.shared_support.iova()
    }

    pub(crate) fn channel_control_gpu_va(&self) -> u64 {
        self.channel_control.iova()
    }

    /// Initialize the distinct native context-2 activation record. Record 0
    /// remains the relocated CL queue/optional role; record 2 starts fresh.
    pub(crate) fn initialize_context2_activation_record(&mut self) -> Result<u64> {
        let offset = g17_initdata::COMPUTE_READINESS_ACTIVATION_RECORD_OFFSET;
        let activation_gpu_va = self
            .channel_control
            .iova()
            .checked_add(offset as u64)
            .ok_or(EINVAL)?;
        let end = offset
            .checked_add(g17_initdata::CONTROL_RECORD_SIZE)
            .ok_or(EINVAL)?;
        let vmap = self.channel_control.object.vmap()?;
        let bytes = unsafe {
            // SAFETY: the VMap covers the complete channel-control object.
            core::slice::from_raw_parts_mut(vmap.as_mut_ptr(), self.channel_control.logical_size)
        };
        let qword = |at: usize| -> Result<u64> {
            Ok(u64::from_le_bytes(
                bytes
                    .get(at..at + 8)
                    .ok_or(EINVAL)?
                    .try_into()
                    .map_err(|_| EINVAL)?,
            ))
        };
        if end > bytes.len()
            || qword(0x00)? != 0x0000_0100_0000_ffff
            || qword(0x20)? != 0x0002_0000_0000_0000
            || qword(0x30)? != 0x0000_0000_ff00_0000
            || !bytes[offset..end].iter().all(|byte| *byte == 0)
        {
            return Err(EINVAL);
        }
        g17_initdata::encode_compute_readiness_activation_record(&mut bytes[offset..end])
            .map_err(|_| EINVAL)?;
        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
        Ok(activation_gpu_va)
    }

    pub(crate) fn snapshot_channel_control_prefix(&mut self) -> Result<[u8; 0x38]> {
        core::sync::atomic::fence(core::sync::atomic::Ordering::Acquire);
        let vmap = self.channel_control.object.vmap()?;
        let bytes = unsafe {
            // SAFETY: the VMap covers the complete channel-control object.
            core::slice::from_raw_parts(vmap.as_mut_ptr(), self.channel_control.logical_size)
        };
        let mut prefix = [0u8; 0x38];
        prefix.copy_from_slice(bytes.get(..0x38).ok_or(EINVAL)?);
        Ok(prefix)
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
            core::slice::from_raw_parts_mut(support_state.as_mut_ptr(), G17P_CL_SUPPORT_STATE_SIZE)
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

    /// Read back the head of a compute SKSM entry slot.
    ///
    /// The CONTROL for the render readback. Render's entry region is
    /// bit-identical before the doorbell and after the timeout, and that was
    /// read as "never consumed" -- but nothing had ever sampled the same region
    /// on the path that provably completes. If compute's entries are also
    /// unchanged across a successful submit, then "unchanged" says nothing
    /// about consumption and the render reading is void.
    pub(crate) fn log_entry(&mut self, label: &str, dev: &AsahiDevice, offset: usize) {
        let mut words = [0u32; 12];
        let mut ok = false;
        if let Ok(vmap) = self.entries.object.vmap() {
            let bytes = unsafe {
                // SAFETY: the VMap covers the page-rounded object; every index
                // below is bounds-checked against the mapped extent.
                core::slice::from_raw_parts(vmap.as_ptr(), self.entries.logical_size)
            };
            for (index, slot) in words.iter_mut().enumerate() {
                let at = offset + index * 4;
                if at + 4 <= bytes.len() {
                    *slot =
                        u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]]);
                    ok = true;
                }
            }
        }
        if ok {
            dev_info!(
                dev.as_ref(),
                "G17P sksm entry[{}] CS +{:#x}: {:#010x} {:#010x} {:#010x} {:#010x} {:#010x} {:#010x} {:#010x} {:#010x} {:#010x} {:#010x} {:#010x} {:#010x}\n",
                label, offset,
                words[0], words[1], words[2], words[3], words[4], words[5],
                words[6], words[7], words[8], words[9], words[10], words[11],
            );
        } else {
            dev_err!(dev.as_ref(), "G17P sksm entry[{}] CS read failed\n", label);
        }
    }

    pub(crate) fn write_entry(&mut self, offset: usize, zero_length: usize, body: &[u8]) -> Result {
        let end = offset.checked_add(zero_length).ok_or(EINVAL)?;
        let stride = self.geometry.entry_stride() as usize;
        if zero_length != stride
            || offset % stride != 0
            || end > self.geometry.backing_size()
            || end > self.entries.logical_size
            || body.len() > zero_length
        {
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
    native_private_cluster: MappedObject,
    partial_computed: MappedObject,
    partial_scheduler: MappedObject,
    partial_primary_index: MappedObject,
    roots: MappedObject,
    shared_cluster: MappedObject,
    region_a: MappedObject,
    region_c: MappedObject,
    primary_state: MappedObject,
    secondary_state: MappedObject,
    secondary_status_a: MappedObject,
    primary_b2_sentinel: MappedObject,
    primary_b2_alias_1: MappedObject,
    primary_b2_alias_0_low: mmu::KernelMapping,
    primary_b2_alias_1_low: mmu::KernelMapping,
    completion_ordinal_0: MappedObject,
    completion_ordinal_0_low: mmu::KernelMapping,
    completion_ordinal_2: MappedObject,
    completion_ordinal_2_low: mmu::KernelMapping,
    qos_resource: MappedObject,
    sksm_qid_resource: MappedObject,
    pb_descriptor_table: MappedObject,
    pb_descriptor_table_low: mmu::KernelMapping,
    uma_page_pool_descriptor_table: MappedObject,
    uma_page_pool_descriptor_table_low: mmu::KernelMapping,
    parameter_metrics: MappedObject,
    parameter_metrics_low_va: u64,
    fwctl: MappedObject,
    control_shared: MappedObject,
    control_shared_inner: MappedObject,
    partial_opening_control_shared: MappedObject,
    partial_opening_control_shared_inner: MappedObject,
    /// Device-global primary-GART view of the retained PB page list.  The
    /// fragment PB loader reports faults against VM slot 0, so a per-process
    /// alias at the same numeric VA is not sufficient.
    partial_primary_index_low: mmu::KernelMapping,
    bootstrap_descriptor_zero_a: MappedObject,
    bootstrap_descriptor_zero_b: MappedObject,
    bootstrap_ta_context_peer: MappedObject,
    bootstrap_3d_context_peer: MappedObject,
    control_operand_page_lists: RenderBackingObject,
    control_operand_table: RenderBackingObject,
    control_operand_buffers: KVec<RenderBackingObject>,
    compute_runtime_support: MappedObject,
    compute_runtime_state: RenderBackingObject,
    compute_runtime_state_mapping: Option<mmu::KernelMapping>,
    compute_runtime_zero_buffer_0: RenderBackingObject,
    compute_runtime_zero_buffer_1: RenderBackingObject,
    compute_runtime_zero_buffer_0_mapping: Option<mmu::KernelMapping>,
    compute_runtime_zero_buffer_1_mapping: Option<mmu::KernelMapping>,
    compute_runtime_class1_table: MappedObject,
    compute_runtime_table_alias: Option<mmu::KernelMapping>,
    compute_runtime_buffer_aliases: KVec<mmu::KernelMapping>,
    compute_readiness_class1_support: MappedObject,
    compute_readiness_class1_state: MappedObject,
    compute_readiness_class3_state: MappedObject,
    compute_readiness_context1: Option<G17PComputeReadinessMappings>,
    compute_readiness_class3_support_active: bool,
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
    dev: AsahiDevRef,
    handoff: G17ResourceHandoff,
    resources: G17MappedResources,
    address_space: G17UatOwner,
    kernel_va_base: u64,
}

#[cfg(not(test))]
#[allow(dead_code)]
impl G17ResourceOwner {
    /// Read the real shared page-metrics object referenced by the PB
    /// descriptor. The per-submit render backing has an unused placeholder
    /// with the same shape, but firmware never consumes that copy.
    pub(crate) fn selected_parameter_metric(&mut self) -> Result<u32> {
        self.resources.parameter_metrics.with_bytes_mut(|bytes| {
            let at = G17P_PM_SELECTED_SCENE * 4;
            let raw = bytes.get(at..at + 4).ok_or(ERANGE)?;
            Ok(u32::from_le_bytes(raw.try_into().map_err(|_| ERANGE)?))
        })
    }

    /// Compute's counterpart to `log_status_pages`.
    ///
    /// `compute_runtime_state` is the object mapped at
    /// `COMPUTE_RUNTIME_STATE_ADDRESS` (0xfffffc20_0163_0000) -- which is
    /// bit-for-bit `G17P_BOOTSTRAP_3D_STATUS_VA`, the render 3D status page.
    /// Same GPU virtual address, same page, read the same way, so a compute
    /// sample and a render sample are directly comparable rather than
    /// analogous. Plain DRAM, no sgx MMIO, safe at any point.
    pub(crate) fn log_compute_status_pages(&mut self, label: &str) {
        let mut words = [0u32; 8];
        let read = self.resources.compute_runtime_state.with_bytes_mut(|raw| {
            for (index, slot) in words.iter_mut().enumerate() {
                let at = index * 4;
                if at + 4 <= raw.len() {
                    *slot = u32::from_le_bytes([raw[at], raw[at + 1], raw[at + 2], raw[at + 3]]);
                }
            }
            Ok(())
        });
        // `with_bytes_mut` has returned, so the mutable borrow of
        // `self.resources` is over and `dev()` can take its immutable one.
        let dev = self.dev();
        match read {
            Ok(()) => dev_info!(
                dev.as_ref(),
                "G17P compute status[{}]: CS  {:#010x} {:#010x} {:#010x} {:#010x} {:#010x} {:#010x} {:#010x} {:#010x}\n",
                label, words[0], words[1], words[2], words[3],
                words[4], words[5], words[6], words[7],
            ),
            Err(error) => dev_err!(
                dev.as_ref(),
                "G17P compute status[{}]: read failed ({:?})\n",
                label,
                error
            ),
        }
    }

    /// Map both native SKSM completion rings into the active client root at
    /// the exact low VAs used by their firmware descriptors. QID4 selects
    /// ordinal 2, while native context setup retains ordinal 0 alongside it.
    pub(crate) fn map_user_completion_aliases(
        &mut self,
        vm: &mmu::Vm,
    ) -> Result<(mmu::KernelMapping, mmu::KernelMapping)> {
        let ordinal_0 = self.resources.completion_ordinal_0.map_alias_at(
            vm,
            g17_initdata::G17P_KSM_COMPLETION_LOW_VAS[0],
            mmu::PROT_GPU_SHARED_RW,
        )?;
        let ordinal_2 = self.resources.completion_ordinal_2.map_alias_at(
            vm,
            g17_initdata::G17P_KSM_COMPLETION_LOW_VAS[1],
            mmu::PROT_GPU_SHARED_RW,
        )?;

        let aliases = [
            (
                self.resources.completion_ordinal_0.iova(),
                self.resources.completion_ordinal_0_low.iova(),
                ordinal_0.iova(),
            ),
            (
                self.resources.completion_ordinal_2.iova(),
                self.resources.completion_ordinal_2_low.iova(),
                ordinal_2.iova(),
            ),
        ];
        let mut backing = [0u64; 2];
        for (index, (high, context0_low, context1_low)) in aliases.into_iter().enumerate() {
            let high_pa = self.address_space.uat.kernel_vm().translate_iova(high)?;
            let context0_pa = self
                .address_space
                .uat
                .kernel_lower_vm()
                .translate_iova(context0_low)?;
            let context1_pa = vm.translate_iova(context1_low)?;
            if high_pa != context0_pa || high_pa != context1_pa {
                dev_err!(
                    self.dev.as_ref(),
                    "G17P completion ordinal {} aliases disagree high={:#x}->{:#x} context0-low={:#x}->{:#x} context1-low={:#x}->{:#x}\n",
                    index * 2,
                    high,
                    high_pa,
                    context0_low,
                    context0_pa,
                    context1_low,
                    context1_pa
                );
                return Err(EFAULT);
            }
            backing[index] = high_pa;
        }
        dev_info!(
            self.dev.as_ref(),
            "G17P completion client aliases: ord0 low={:#x} PA={:#x}; ord2 low={:#x} PA={:#x}\n",
            ordinal_0.iova(),
            backing[0],
            ordinal_2.iova(),
            backing[1]
        );
        Ok((ordinal_0, ordinal_2))
    }

    pub(crate) fn dev(&self) -> &AsahiDevice {
        &self.dev
    }

    pub(crate) fn new(dev: &AsahiDevice) -> Result<Self> {
        let uat = KBox::new(mmu::Uat::new_t8140(dev, true)?, GFP_KERNEL)?;
        Self::new_with_uat(dev, uat)
    }

    /// Kept out of line: dozens of mapping locals live across this body, and
    /// inlining it into the boot closure overflowed the task stack on
    /// hardware.
    #[inline(never)]
    pub(crate) fn new_with_uat(dev: &AsahiDevice, uat: KBox<mmu::Uat>) -> Result<Self> {
        let address_space = G17UatOwner { uat };
        dev_info!(dev.as_ref(), "G17P resources: building firmware layout\n");

        // Reserve the firmware MMIO aperture before allocator-selected
        // objects can occupy one of its fixed device-VA ranges.
        let register_windows = map_t8140_register_windows(dev, &address_space.uat)?;
        let kernel_va_base = address_space.uat.kernel_va_range()?.start;
        let native_private_cluster_va = kernel_va_base
            .checked_add(G17P_NATIVE_PRIVATE_CLUSTER_OFFSET)
            .ok_or(EINVAL)?;
        // Reserve this fixed arena before any allocator-selected high object.
        // It is a firmware-written uncached run in the measured native root.
        let native_private_cluster = map_fixed_wc_object(
            dev,
            address_space.uat.kernel_vm(),
            "native private cluster",
            native_private_cluster_va,
            G17P_NATIVE_PRIVATE_CLUSTER_SIZE,
            mmu::PROT_FW_SHARED_RW,
        )?;
        dev_info!(
            dev.as_ref(),
            "G17P resources: native private cluster mapped at {:#x}:{:#x}\n",
            native_private_cluster_va,
            G17P_NATIVE_PRIVATE_CLUSTER_SIZE
        );
        let fwctl_gpu_va = kernel_va_base
            .checked_add(g17_submission::G17P_PARTIAL_OPENING_FWCTL_OFFSET)
            .ok_or(EINVAL)?;
        let partial_computed = MappedObject::new_wc_at(
            dev,
            address_space.uat.kernel_vm(),
            g17_submission::G17P_PARTIAL_OPENING_COMPUTED_GPU_VA,
            g17_submission::G17P_PARTIAL_OPENING_PAGE_SIZE,
            mmu::PROT_FW_PRIV_RW,
        )?;
        let mut partial_scheduler = MappedObject::new_wc_at(
            dev,
            address_space.uat.kernel_vm(),
            g17_submission::G17P_PARTIAL_OPENING_SCHEDULER_GPU_VA,
            g17_submission::G17P_PARTIAL_OPENING_PAGE_SIZE,
            mmu::PROT_FW_SHARED_RW,
        )?;
        partial_scheduler.with_bytes_mut(|bytes| {
            g17_submission::apply_g17p_partial_opening_scheduler_page(bytes).map_err(|_| EINVAL)
        })?;
        let primary_b2_alias_1_gpu_va =
            g17_submission::G17P_PARTIAL_OPENING_SCHEDULER_GPU_VA
                .checked_add(0x8000)
                .ok_or(EINVAL)?;
        dev_info!(
            dev.as_ref(),
            "G17P resources: mapping B2 alias at {:#x}\n",
            primary_b2_alias_1_gpu_va
        );
        let mut primary_b2_alias_1 = match MappedObject::new_wc_at(
            dev,
            address_space.uat.kernel_vm(),
            primary_b2_alias_1_gpu_va,
            G17P_CL_B2_OBJECT_SIZE,
            mmu::PROT_FW_SHARED_RW,
        ) {
            Ok(mapping) => mapping,
            Err(error) => {
                dev_err!(
                    dev.as_ref(),
                    "G17P resources: B2 alias map at {:#x} failed ({:?})\n",
                    primary_b2_alias_1_gpu_va,
                    error
                );
                return Err(error);
            }
        };
        primary_b2_alias_1.with_bytes_mut(|bytes| {
            g17_initdata::encode_compute_dispatch_record(
                &mut bytes[0x20..0x20 + g17_initdata::COMPUTE_DISPATCH_RECORD_SIZE],
            )
            .map_err(|_| EINVAL)
        })?;
        // The compute firmware consumes these two objects through the fixed
        // context-0 low views published in primary main config. Keep the
        // low/high pairs on the same backing; copying their bytes into an
        // unrelated descriptor table is not equivalent once firmware writes
        // scheduler state through one view and reads it through the other.
        let primary_b2_alias_0_low = partial_scheduler.map_alias_at(
            address_space.uat.kernel_lower_vm(),
            g17_initdata::REGION_VIEW_LOW_ADDRS[0],
            mmu::PROT_GPU_SHARED_RW,
        )?;
        let primary_b2_alias_1_low = primary_b2_alias_1.map_alias_at(
            address_space.uat.kernel_lower_vm(),
            g17_initdata::REGION_VIEW_LOW_ADDRS[1],
            mmu::PROT_GPU_SHARED_RW,
        )?;
        dev_info!(dev.as_ref(), "G17P resources: compute B2 low/high aliases mapped\n");
        dev_info!(
            dev.as_ref(),
            "G17P resources: mapping primary-index owner at {:#x}, render alias {:#x}\n",
            g17_submission::G17P_PARTIAL_OPENING_PRIMARY_INDEX_FIRMWARE_GPU_VA,
            g17_submission::G17P_PARTIAL_OPENING_PRIMARY_INDEX_GPU_VA
        );
        let mut partial_primary_index = match MappedObject::new_at(
            dev,
            address_space.uat.kernel_vm(),
            g17_submission::G17P_PARTIAL_OPENING_PRIMARY_INDEX_FIRMWARE_GPU_VA,
            g17_submission::G17P_PARTIAL_OPENING_PRIMARY_INDEX_SIZE,
            mmu::PROT_GPU_FW_SHARED_RW,
        ) {
            Ok(mapping) => mapping,
            Err(error) => {
                dev_err!(
                    dev.as_ref(),
                    "G17P resources: primary-index owner map failed ({:?})\n",
                    error
                );
                return Err(error);
            }
        };
        partial_primary_index.with_bytes_mut(|bytes| {
            g17_submission::apply_g17p_partial_opening_primary_index_page(
                &mut bytes[..g17_submission::G17P_PARTIAL_OPENING_PAGE_SIZE],
            )
            .map_err(|_| EINVAL)
        })?;
        // `AGXRenderTargetMemory` and the process command mappings use the
        // client GART, but `halLoadPBDesc` publishes PBState +0x28 to a
        // device-global PB loader.  Hardware identifies faults from that
        // loader as VM slot 0.  Native's 0x1000190000 mapping is therefore a
        // primary-GART mapping as well as a process-visible one; keeping it
        // only in `install_user_vm_aliases()` leaves context 0 unmapped.
        let partial_primary_index_low = partial_primary_index
            .map_alias_at(
                address_space.uat.kernel_lower_vm(),
                g17_submission::G17P_PARTIAL_OPENING_PRIMARY_INDEX_GPU_VA,
                mmu::PROT_GPU_FW_SHARED_RW,
            )
            .map_err(|error| {
                dev_err!(
                    dev.as_ref(),
                    "G17P resources: primary-index context-0 alias at {:#x} failed ({:?})\n",
                    g17_submission::G17P_PARTIAL_OPENING_PRIMARY_INDEX_GPU_VA,
                    error
                );
                error
            })?;
        let partial_primary_index_high_pa = address_space
            .uat
            .kernel_vm()
            .translate_iova(partial_primary_index.iova())?;
        let partial_primary_index_low_pa = address_space
            .uat
            .kernel_lower_vm()
            .translate_iova(partial_primary_index_low.iova())?;
        if partial_primary_index_high_pa != partial_primary_index_low_pa {
            dev_err!(
                dev.as_ref(),
                "G17P resources: primary-index aliases disagree high={:#x}->{:#x} context0={:#x}->{:#x}\n",
                partial_primary_index.iova(),
                partial_primary_index_high_pa,
                partial_primary_index_low.iova(),
                partial_primary_index_low_pa
            );
            return Err(EFAULT);
        }
        dev_info!(
            dev.as_ref(),
            "G17P resources: primary-index aliases high/context0=[{:#x},{:#x}] size={:#x}\n",
            partial_primary_index.iova(),
            partial_primary_index_low.iova(),
            partial_primary_index_low.size()
        );
        let fwctl = map_fixed_object(
            dev,
            address_space.uat.kernel_vm(),
            "fwctl",
            fwctl_gpu_va,
            FWCTL_SIZE,
            mmu::PROT_FW_SHARED_RW,
        )?;
        let mut control_shared = map_fixed_wc_object(
            dev,
            address_space.uat.kernel_vm(),
            "control-shared",
            g17_initdata::CONTROL_SHARED_ADDRESS,
            g17_initdata::CONTROL_SHARED_OBJECT_SIZE,
            mmu::PROT_GPU_FW_PRIV_RW,
        )?;
        control_shared.with_bytes_mut(|bytes| {
            g17_initdata::encode_control_shared(bytes).map_err(|_| EINVAL)
        })?;
        let mut control_shared_inner = map_fixed_wc_object(
            dev,
            address_space.uat.kernel_vm(),
            "control-shared-inner",
            g17_initdata::CONTROL_SHARED_INNER_ADDRESS,
            mmu::UAT_PGSZ,
            mmu::PROT_GPU_FW_SHARED_RW,
        )?;
        control_shared_inner.with_bytes_mut(|bytes| {
            bytes.fill(0);
            bytes[..8].copy_from_slice(&g17_initdata::CONTROL_SHARED_INNER_PRESENTED.to_le_bytes());
            Ok(())
        })?;
        let mut partial_opening_control_shared = map_fixed_wc_object(
            dev,
            address_space.uat.kernel_vm(),
            "partial-opening-control-shared",
            g17_initdata::PARTIAL_OPENING_CONTROL_SHARED_ADDRESS,
            g17_initdata::CONTROL_SHARED_OBJECT_SIZE,
            mmu::PROT_GPU_FW_PRIV_RW,
        )?;
        partial_opening_control_shared.with_bytes_mut(|bytes| {
            g17_initdata::encode_partial_opening_control_shared(bytes).map_err(|_| EINVAL)
        })?;
        let mut partial_opening_control_shared_inner = map_fixed_wc_object(
            dev,
            address_space.uat.kernel_vm(),
            "partial-opening-control-shared-inner",
            g17_initdata::PARTIAL_OPENING_CONTROL_SHARED_INNER_ADDRESS,
            mmu::UAT_PGSZ,
            mmu::PROT_GPU_FW_SHARED_RW,
        )?;
        partial_opening_control_shared_inner.with_bytes_mut(|bytes| {
            bytes.fill(0);
            bytes[..8].copy_from_slice(
                &g17_initdata::PARTIAL_OPENING_CONTROL_SHARED_INNER_PRESENTED.to_le_bytes(),
            );
            Ok(())
        })?;
        let bootstrap_descriptor_zero_a = map_fixed_wc_object(
            dev,
            address_space.uat.kernel_vm(),
            "bootstrap descriptor zero A",
            G17P_BOOTSTRAP_DESCRIPTOR_ZERO_A_VA,
            mmu::UAT_PGSZ,
            mmu::PROT_GPU_FW_SHARED_RW,
        )?;
        let bootstrap_descriptor_zero_b = map_fixed_wc_object(
            dev,
            address_space.uat.kernel_vm(),
            "bootstrap descriptor zero B",
            G17P_BOOTSTRAP_DESCRIPTOR_ZERO_B_VA,
            mmu::UAT_PGSZ,
            mmu::PROT_GPU_FW_PRIV_RW,
        )?;
        let bootstrap_ta_context_peer = map_fixed_wc_object(
            dev,
            address_space.uat.kernel_vm(),
            "bootstrap TA context peer",
            G17P_BOOTSTRAP_TA_CONTEXT_HIGH_VA,
            G17P_BOOTSTRAP_CONTEXT_PEER_SIZE,
            mmu::PROT_GPU_FW_SHARED_RW,
        )?;
        let bootstrap_3d_context_peer = map_fixed_wc_object(
            dev,
            address_space.uat.kernel_vm(),
            "bootstrap 3D context peer",
            G17P_BOOTSTRAP_3D_CONTEXT_HIGH_VA,
            G17P_BOOTSTRAP_CONTEXT_PEER_SIZE,
            mmu::PROT_GPU_FW_SHARED_RW,
        )?;
        dev_info!(
            dev.as_ref(),
            "G17P resources: bootstrap context peers reserved before dynamic graph\n"
        );
        let mut control_operand_page_lists =
            match RenderBackingObject::new(dev, G17P_CONTROL_OPERAND_PAGE_LIST_SIZE) {
                Ok(object) => object,
                Err(error) => {
                    dev_err!(
                        dev.as_ref(),
                        "G17P resources: operand page-list backing for compute VA {:#x} failed ({:?})\n",
                        G17P_CONTROL_DIRECTORY_VA,
                        error
                    );
                    return Err(error);
                }
            };
        if let Err(error) = control_operand_page_lists.with_bytes_mut(|bytes| {
            if !encode_g17p_initial_flist_page_list(bytes) {
                return Err(EINVAL);
            }
            Ok(())
        }) {
            dev_err!(
                dev.as_ref(),
                "G17P resources: operand page-list initialization failed ({:?})\n",
                error
            );
            return Err(error);
        }
        let mut control_operand_table = match RenderBackingObject::new(
            dev,
            G17P_CONTROL_OPERAND_TABLE_BACKING_SIZE,
        ) {
            Ok(object) => object,
            Err(error) => {
                dev_err!(
                    dev.as_ref(),
                    "G17P resources: operand-table backing for render VA {:#x} failed ({:?})\n",
                    G17P_CONTROL_OPERAND_TABLE_VA,
                    error
                );
                return Err(error);
            }
        };
        if let Err(error) = control_operand_table.with_bytes_mut(|bytes| {
            bytes.fill(0);
            g17_initdata::encode_partial_opening_operand_table_pre_control(
                &mut bytes[..g17_initdata::CONTROL_OPERAND_TABLE_SIZE],
            )
            .map_err(|_| EINVAL)
        }) {
            dev_err!(
                dev.as_ref(),
                "G17P resources: operand-table initialization failed ({:?})\n",
                error
            );
            return Err(error);
        }
        let mut control_operand_buffers =
            KVec::with_capacity(G17P_CONTROL_OPERAND_BUFFER_COUNT, GFP_KERNEL)?;
        for index in 0..G17P_CONTROL_OPERAND_BUFFER_COUNT {
            let address = g17p_control_operand_buffer_va(index).ok_or(EINVAL)?;
            let object = match RenderBackingObject::new(dev, G17P_CONTROL_OPERAND_BUFFER_SIZE) {
                Ok(object) => object,
                Err(error) => {
                    dev_err!(
                        dev.as_ref(),
                        "G17P resources: operand buffer {} backing for render VA {:#x} failed ({:?})\n",
                        index,
                        address,
                        error
                    );
                    return Err(error);
                }
            };
            control_operand_buffers.push(object, GFP_KERNEL)?;
        }
        dev_info!(
            dev.as_ref(),
            "G17P resources: prepared compute FList page lists at {:#x}+{:#x}, operand table at {:#x}+{:#x}, {} buffers at {:#x}\n",
            G17P_CONTROL_DIRECTORY_VA,
            G17P_CONTROL_OPERAND_PAGE_LIST_SIZE,
            G17P_CONTROL_OPERAND_TABLE_VA,
            G17P_CONTROL_OPERAND_TABLE_BACKING_SIZE,
            G17P_CONTROL_OPERAND_BUFFER_COUNT,
            G17P_CONTROL_OPERAND_BUFFER_BASE
        );
        let mut compute_runtime_support = map_fixed_wc_object(
            dev,
            address_space.uat.kernel_vm(),
            "compute runtime support",
            g17_initdata::COMPUTE_RUNTIME_SUPPORT_ADDRESS,
            g17_initdata::COMPUTE_RUNTIME_SUPPORT_SIZE,
            mmu::PROT_GPU_FW_SHARED_RW,
        )?;
        let mut compute_runtime_state = RenderBackingObject::new(dev, mmu::UAT_PGSZ)?;
        let compute_runtime_state_mapping = None;
        let compute_runtime_zero_buffer_0 =
            RenderBackingObject::new(dev, G17P_CONTROL_OPERAND_BUFFER_SIZE)?;
        let compute_runtime_zero_buffer_1 =
            RenderBackingObject::new(dev, G17P_CONTROL_OPERAND_BUFFER_SIZE)?;
        let compute_runtime_zero_buffer_0_mapping = None;
        let compute_runtime_zero_buffer_1_mapping = None;
        let mut compute_runtime_class1_table = map_fixed_wc_object(
            dev,
            address_space.uat.kernel_lower_vm(),
            "compute runtime class-1 table",
            g17_initdata::COMPUTE_RUNTIME_CLASS1_TABLE_ADDRESS,
            g17_initdata::COMPUTE_RUNTIME_CLASS1_TABLE_SIZE,
            mmu::PROT_GPU_FW_SHARED_RW,
        )?;
        let compute_runtime_table_alias = None;
        let compute_runtime_buffer_aliases = KVec::with_capacity(20, GFP_KERNEL)?;
        // Native final-26.6 reuses pair-1 secondary-index, flag, and
        // fragment-status objects at these three addresses. The retained
        // Linux opening graph has no fixed owners for those pair-1 objects,
        // so these are their sole mappings here. In contrast, class-3 support
        // is inside the retained pair-0 primary-index owner and must not be
        // allocated a second time.
        let mut compute_readiness_class1_support = map_fixed_wc_object(
            dev,
            address_space.uat.kernel_vm(),
            "compute readiness class-1 support",
            g17_initdata::COMPUTE_READINESS_CLASS1_SUPPORT_ADDRESS,
            g17_initdata::COMPUTE_READINESS_PAGE_SIZE,
            mmu::PROT_GPU_FW_SHARED_RW,
        )?;
        let mut compute_readiness_class1_state = map_fixed_wc_object(
            dev,
            address_space.uat.kernel_vm(),
            "compute readiness context-1 state",
            g17_initdata::COMPUTE_READINESS_CLASS1_STATE_ADDRESS,
            g17_initdata::COMPUTE_READINESS_PAGE_SIZE,
            mmu::PROT_GPU_FW_SHARED_RW,
        )?;
        let mut compute_readiness_class3_state = map_fixed_wc_object(
            dev,
            address_space.uat.kernel_vm(),
            "compute readiness context-2 state",
            g17_initdata::COMPUTE_READINESS_CLASS3_STATE_ADDRESS,
            g17_initdata::COMPUTE_READINESS_PAGE_SIZE,
            mmu::PROT_GPU_FW_SHARED_RW,
        )?;
        // Bootstrap owns overlapping low aliases first. Readiness replaces
        // them only after render retirement restores the kernel context.
        let compute_readiness_context1 = None;
        let compute_readiness_class3_support_active = false;
        compute_runtime_support.with_bytes_mut(|bytes| {
            g17_initdata::encode_compute_runtime_class1_support(bytes).map_err(|_| EINVAL)
        })?;
        compute_runtime_state.with_bytes_mut(|bytes| {
            bytes.fill(0);
            bytes[..4].copy_from_slice(&1u32.to_le_bytes());
            Ok(())
        })?;
        compute_runtime_class1_table.with_bytes_mut(|bytes| {
            g17_initdata::encode_compute_runtime_class1_table(bytes).map_err(|_| EINVAL)
        })?;
        compute_readiness_class1_support.with_bytes_mut(|bytes| {
            g17_initdata::encode_compute_readiness_class1_support(bytes).map_err(|_| EINVAL)
        })?;
        compute_readiness_class1_state.with_bytes_mut(|bytes| {
            g17_initdata::encode_compute_readiness_state(bytes).map_err(|_| EINVAL)
        })?;
        compute_readiness_class3_state.with_bytes_mut(|bytes| {
            g17_initdata::encode_compute_readiness_state(bytes).map_err(|_| EINVAL)
        })?;
        let page_list = control_operand_buffers
            .iter_mut()
            .enumerate()
            .find(|(index, _)| *index == 22)
            .map(|(_, buffer)| buffer)
            .ok_or(EINVAL)?;
        page_list.with_bytes_mut(|bytes| {
            bytes.fill(0);
            g17_initdata::encode_compute_runtime_page_inventory(
                &mut bytes[..g17_initdata::COMPUTE_RUNTIME_PAGE_LIST_SIZE],
            )
            .map_err(|_| EINVAL)
        })?;
        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
        dev_info!(
            dev.as_ref(),
            "G17P resources: live compute-control closure mapped in context 1\n"
        );

        // The classic CL producer appends its outer slot in this bundle and
        // advances the paired channel producer in primary_state. Both are
        // live host-to-firmware publication surfaces, not boot-only blobs. The
        // output-positive producer requires the complete cluster at its native
        // base, including CL_2 at base + 0x19dc0.
        let mut shared_cluster = map_fixed_wc_object(
            dev,
            address_space.uat.kernel_vm(),
            "native shared cluster",
            g17_initdata::NATIVE_SHARED_CLUSTER_GPU_VA,
            g17_initdata::NATIVE_SHARED_CLUSTER_SIZE,
            mmu::PROT_FW_PRIV_RW,
        )?;
        // The native bundle has three out-of-line pages at fixed offsets from
        // its base. Claim them before placing the remaining dynamic objects,
        // otherwise a large state object can occupy one of these addresses.
        let primary_b2_sentinel = MappedObject::new_wc_at(
            dev,
            address_space.uat.kernel_vm(),
            shared_cluster.iova() + 0x40000,
            G17P_CL_B2_OBJECT_SIZE,
            mmu::PROT_GPU_FW_SHARED_RW,
        )?;
        let mut completion_ordinal_0 = map_fixed_object(
            dev,
            address_space.uat.kernel_vm(),
            "KSM completion ordinal 0 high",
            shared_cluster.iova()
                + g17_initdata::G17P_KSM_COMPLETION_HIGH_BUNDLE_OFFSETS[0] as u64,
            g17_initdata::G17P_KSM_COMPLETION_BACKING_SIZE,
            mmu::PROT_FW_PRIV_RW,
        )?;
        let completion_ordinal_0_low = completion_ordinal_0
            .map_alias_at(
                address_space.uat.kernel_lower_vm(),
                g17_initdata::G17P_KSM_COMPLETION_LOW_VAS[0],
                mmu::PROT_GPU_SHARED_RW,
            )
            .map_err(|error| {
                dev_err!(
                    dev.as_ref(),
                    "G17P resources: KSM completion ordinal 0 low alias failed ({:?})\n",
                    error
                );
                error
            })?;
        let mut completion_ordinal_2 = map_fixed_object(
            dev,
            address_space.uat.kernel_vm(),
            "KSM completion ordinal 2 high",
            shared_cluster.iova()
                + g17_initdata::G17P_KSM_COMPLETION_HIGH_BUNDLE_OFFSETS[1] as u64,
            g17_initdata::G17P_KSM_COMPLETION_BACKING_SIZE,
            mmu::PROT_FW_PRIV_RW,
        )?;
        let completion_ordinal_2_low = completion_ordinal_2
            .map_alias_at(
                address_space.uat.kernel_lower_vm(),
                g17_initdata::G17P_KSM_COMPLETION_LOW_VAS[1],
                mmu::PROT_GPU_SHARED_RW,
            )
            .map_err(|error| {
                dev_err!(
                    dev.as_ref(),
                    "G17P resources: KSM completion ordinal 2 low alias failed ({:?})\n",
                    error
                );
                error
            })?;
        dev_info!(
            dev.as_ref(),
            "G17P resources: KSM completion aliases ready ord0=[{:#x},{:#x}] ord2=[{:#x},{:#x}] size={:#x}\n",
            completion_ordinal_0_low.iova(),
            completion_ordinal_0.iova(),
            completion_ordinal_2_low.iova(),
            completion_ordinal_2.iova(),
            g17_initdata::G17P_KSM_COMPLETION_BACKING_SIZE
        );
        // Native createKSMKickQueues retains this separate zeroed global QID
        // resource and publishes its firmware-high VA through HW data.
        let mut sksm_qid_resource = MappedObject::new_wc(
            dev,
            &address_space.uat,
            g17_initdata::G17P_SKSM_QID_RESOURCE_SIZE,
            mmu::UAT_PGSZ as u64,
            mmu::PROT_FW_SHARED_RW,
        )?;
        sksm_qid_resource.with_bytes_mut(|bytes| {
            bytes.fill(0);
            Ok(())
        })?;
        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
        dev_info!(
            dev.as_ref(),
            "G17P resources: global SKSM QID state high={:#x} size={:#x}\n",
            sksm_qid_resource.iova(),
            g17_initdata::G17P_SKSM_QID_RESOURCE_SIZE
        );
        // createAGXQOSManager runs after createKSMKickQueues. Keep that
        // allocation order so the established QID resource placement is not
        // displaced by this smaller block.
        let mut qos_resource = MappedObject::new_wc(
            dev,
            &address_space.uat,
            g17_initdata::G17P_QOS_RESOURCE_SIZE,
            mmu::UAT_PGSZ as u64,
            mmu::PROT_FW_SHARED_RW,
        )?;
        qos_resource.with_bytes_mut(|bytes| {
            bytes.fill(0);
            Ok(())
        })?;
        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
        dev_info!(
            dev.as_ref(),
            "G17P resources: QoS state high={:#x} size={:#x}\n",
            qos_resource.iova(),
            g17_initdata::G17P_QOS_RESOURCE_SIZE
        );
        // G16 createHardwareMappings allocates these two global descriptor
        // tables after the QID and QoS resources. Their firmware-high and
        // GPU-task-low views name the same zeroed backing, and all four
        // mappings are prepared before either firmware receives initdata.
        let mut pb_descriptor_table = MappedObject::new_wc(
            dev,
            &address_space.uat,
            G17P_PB_DESCRIPTOR_TABLE_SIZE,
            mmu::UAT_PGSZ as u64,
            mmu::PROT_FW_SHARED_RW,
        )?;
        let pb_descriptor_table_low = pb_descriptor_table.map_alias_in_range(
            address_space.uat.kernel_lower_vm(),
            g17p_cl_dynamic_low_va_range(),
            mmu::UAT_PGSZ as u64,
            mmu::PROT_GPU_SHARED_RW,
        )?;
        let mut uma_page_pool_descriptor_table = MappedObject::new_wc(
            dev,
            &address_space.uat,
            G17P_UMA_PAGE_POOL_DESCRIPTOR_TABLE_SIZE,
            mmu::UAT_PGSZ as u64,
            mmu::PROT_FW_SHARED_RW,
        )?;
        let uma_page_pool_descriptor_table_low =
            uma_page_pool_descriptor_table.map_alias_in_range(
                address_space.uat.kernel_lower_vm(),
                g17p_cl_dynamic_low_va_range(),
                mmu::UAT_PGSZ as u64,
                mmu::PROT_GPU_SHARED_RW,
            )?;
        pb_descriptor_table.with_bytes_mut(|bytes| {
            bytes.fill(0);
            Ok(())
        })?;
        uma_page_pool_descriptor_table.with_bytes_mut(|bytes| {
            bytes.fill(0);
            Ok(())
        })?;
        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
        dev_info!(
            dev.as_ref(),
            "G17P resources: PB descriptors low/high=[{:#x},{:#x}], UMA page-pool descriptors low/high=[{:#x},{:#x}]\n",
            pb_descriptor_table_low.iova(),
            pb_descriptor_table.iova(),
            uma_page_pool_descriptor_table_low.iova(),
            uma_page_pool_descriptor_table.iova()
        );
        // The primary mapping was installed by `Uat::new_t8140` before the
        // RTKit processors started. Firmware has now published its upper-root
        // subtree, so add the high alias of that same retained backing.
        let (mut parameter_metrics_object, parameter_metrics_low_va) =
            address_space.uat.t8140_parameter_metrics()?;
        let parameter_metrics_high = parameter_metrics_object.map_into_range(
            address_space.uat.kernel_vm(),
            address_space.uat.kernel_va_range()?,
            mmu::T8140_PARAMETER_METRICS_SIZE as u64,
            mmu::PROT_GPU_FW_SHARED_RW,
            false,
        )?;
        let mut parameter_metrics = MappedObject {
            mapping: parameter_metrics_high,
            object: parameter_metrics_object,
            logical_size: mmu::T8140_PARAMETER_METRICS_SIZE,
        };
        parameter_metrics.with_bytes_mut(|bytes| {
            bytes.fill(0);
            Ok(())
        })?;
        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
        dev_info!(
            dev.as_ref(),
            "G17P resources: PM page metrics low/high=[{:#x},{:#x}] size={:#x}\n",
            parameter_metrics_low_va,
            parameter_metrics.iova(),
            mmu::T8140_PARAMETER_METRICS_SIZE
        );
        // Claim the complete fixed bundle topology before placing dynamic
        // initdata objects, so none can take a captured ring or out-of-line
        // hardware-data address.
        let mut roots = match MappedObject::new(
            dev,
            &address_space.uat,
            ROOT_PAIR_ALLOC_SIZE,
            g17_initdata::SECONDARY_ROOT_DELTA,
            mmu::PROT_FW_SHARED_RO,
        ) {
            Ok(object) => object,
            Err(error) => {
                dev_err!(dev.as_ref(), "G17P resources: roots allocation failed ({:?})\n", error);
                return Err(error);
            }
        };
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
        let mut primary_state = MappedObject::new_wc(
            dev,
            &address_space.uat,
            PRIMARY_STATE_ALLOC_SIZE,
            mmu::UAT_PGSZ as u64,
            mmu::PROT_FW_SHARED_RW,
        )?;
        let mut secondary_state = MappedObject::new_wc(
            dev,
            &address_space.uat,
            SECONDARY_STATE_ALLOC_SIZE,
            mmu::UAT_PGSZ as u64,
            mmu::PROT_FW_SHARED_RW,
        )?;
        let mut secondary_status_a = MappedObject::new_wc(
            dev,
            &address_space.uat,
            STATUS_A_ALLOC_SIZE,
            mmu::UAT_PGSZ as u64,
            mmu::PROT_FW_SHARED_RW,
        )?;
        let primary_region_views = g17_initdata::PrimaryRegionViews {
            sentinel_high: primary_b2_sentinel.iova(),
            alias_high: [partial_scheduler.iova(), primary_b2_alias_1.iova()],
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
        let fixed_high_ranges = [
            ("native private cluster", &native_private_cluster),
            ("partial computed", &partial_computed),
            ("partial scheduler", &partial_scheduler),
            ("B2 alias", &primary_b2_alias_1),
            ("primary index", &partial_primary_index),
            ("fwctl", &fwctl),
            ("control shared", &control_shared),
            ("control shared inner", &control_shared_inner),
            (
                "partial-opening control shared",
                &partial_opening_control_shared,
            ),
            (
                "partial-opening control shared inner",
                &partial_opening_control_shared_inner,
            ),
            ("bootstrap descriptor zero A", &bootstrap_descriptor_zero_a),
            ("bootstrap descriptor zero B", &bootstrap_descriptor_zero_b),
            ("bootstrap TA context peer", &bootstrap_ta_context_peer),
            ("bootstrap 3D context peer", &bootstrap_3d_context_peer),
            ("compute runtime support", &compute_runtime_support),
            (
                "compute readiness class-1 support",
                &compute_readiness_class1_support,
            ),
            (
                "compute readiness context-1 state",
                &compute_readiness_class1_state,
            ),
            (
                "compute readiness context-2 state",
                &compute_readiness_class3_state,
            ),
            ("roots", &roots),
            ("shared cluster", &shared_cluster),
            ("B2 sentinel", &primary_b2_sentinel),
            ("KSM completion ordinal 0 high", &completion_ordinal_0),
            ("KSM completion ordinal 2 high", &completion_ordinal_2),
            ("global QoS state", &qos_resource),
            ("global SKSM QID state", &sksm_qid_resource),
            ("PB descriptor table high", &pb_descriptor_table),
            (
                "UMA page-pool descriptor table high",
                &uma_page_pool_descriptor_table,
            ),
            ("PM page metrics high", &parameter_metrics),
            ("region A", &region_a),
            ("region C", &region_c),
            ("primary state", &primary_state),
            ("secondary state", &secondary_state),
            ("secondary status A", &secondary_status_a),
        ];
        for (name, object) in fixed_high_ranges {
            if !address_space.uat.kernel_vm().covers_range(
                object.iova(),
                object.logical_size as u64,
                false,
                false,
            ) {
                dev_err!(
                    dev.as_ref(),
                    "G17P resources: {} high mapping at {:#x} size {:#x} is incomplete\n",
                    name,
                    object.iova(),
                    object.logical_size
                );
                return Err(EIO);
            }
        }
        for (name, mapping, expected_size) in [
            (
                "KSM completion ordinal 0 low",
                &completion_ordinal_0_low,
                g17_initdata::G17P_KSM_COMPLETION_BACKING_SIZE,
            ),
            (
                "KSM completion ordinal 2 low",
                &completion_ordinal_2_low,
                g17_initdata::G17P_KSM_COMPLETION_BACKING_SIZE,
            ),
            (
                "PB descriptor table low",
                &pb_descriptor_table_low,
                G17P_PB_DESCRIPTOR_TABLE_SIZE,
            ),
            (
                "UMA page-pool descriptor table low",
                &uma_page_pool_descriptor_table_low,
                G17P_UMA_PAGE_POOL_DESCRIPTOR_TABLE_SIZE,
            ),
        ] {
            if mapping.size() != expected_size
                || !address_space.uat.kernel_lower_vm().covers_range(
                    mapping.iova(),
                    mapping.size() as u64,
                    true,
                    true,
                )
            {
                dev_err!(
                    dev.as_ref(),
                    "G17P resources: {} low mapping is incomplete\n",
                    name
                );
                return Err(EIO);
            }
        }
        if primary_b2_alias_0_low.iova() != g17_initdata::REGION_VIEW_LOW_ADDRS[0]
            || primary_b2_alias_1_low.iova() != g17_initdata::REGION_VIEW_LOW_ADDRS[1]
        {
            dev_err!(
                dev.as_ref(),
                "G17P resources: compute B2 low aliases do not match initdata\n"
            );
            return Err(EIO);
        }
        if parameter_metrics_low_va != mmu::T8140_PARAMETER_METRICS_LOW_VA
            || !address_space.uat.kernel_vm().covers_range(
                parameter_metrics.iova(),
                mmu::T8140_PARAMETER_METRICS_SIZE as u64,
                true,
                true,
            )
            || !address_space.uat.kernel_lower_vm().covers_range(
                parameter_metrics_low_va,
                mmu::T8140_PARAMETER_METRICS_SIZE as u64,
                true,
                true,
            )
        {
            dev_err!(dev.as_ref(), "G17P resources: preboot PM page metrics aliases are incomplete\n");
            return Err(EIO);
        }
        if !address_space.uat.kernel_lower_vm().covers_range(
            compute_runtime_class1_table.iova(),
            compute_runtime_class1_table.logical_size as u64,
            false,
            false,
        ) {
            dev_err!(
                dev.as_ref(),
                "G17P resources: compute runtime class-1 table mapping is incomplete\n"
            );
            return Err(EIO);
        }
        if !all_ranges_covered(
            register_windows
                .iter()
                .map(|mapping| (mapping.iova(), mapping.size() as u64)),
            |address, size| {
                address_space
                    .uat
                    .kernel_vm()
                    .covers_range(address, size, false, false)
            },
        ) {
            dev_err!(dev.as_ref(), "G17P resources: register aperture mapping is incomplete\n");
            return Err(EIO);
        }
        dev_info!(dev.as_ref(), "G17P resources: all kernel-high mappings cover\n");
        validate_handoff(&handoff).map_err(|_| EINVAL)?;
        let completion_aliases = [
            g17_initdata::G17PKsmCompletionAliases {
                low: completion_ordinal_0_low.iova(),
                high: completion_ordinal_0.iova(),
            },
            g17_initdata::G17PKsmCompletionAliases {
                low: completion_ordinal_2_low.iova(),
                high: completion_ordinal_2.iova(),
            },
        ];
        shared_cluster.with_bytes_mut(|bytes| {
            g17_initdata::encode_hw_data(
                &g17_initdata::T8140_REGISTER_WINDOWS,
                &g17_initdata::T8140_REGISTER_FLAG_ONLY_SLOTS,
                completion_aliases,
                qos_resource.iova(),
                sksm_qid_resource.iova(),
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
        // Publish both opening producers after the rings and descriptor graph.
        // Firmware owns the two consumer cursors until CONTROL_START.
        write_control_counters(
            &mut primary_state,
            [0, 0, g17_initdata::CONTROL_OPENING_PRIMARY_PRODUCER],
        )?;
        write_control_counters(
            &mut secondary_state,
            [0, 0, g17_initdata::CONTROL_OPENING_SECONDARY_PRODUCER],
        )?;
        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
        dev_info!(dev.as_ref(), "G17P resources: firmware layout ready\n");

        Ok(Self {
            dev: dev.into(),
            handoff,
            resources: G17MappedResources {
                native_private_cluster,
                partial_computed,
                partial_scheduler,
                partial_primary_index,
                roots,
                shared_cluster,
                region_a,
                region_c,
                primary_state,
                secondary_state,
                secondary_status_a,
                primary_b2_sentinel,
                primary_b2_alias_1,
                primary_b2_alias_0_low,
                primary_b2_alias_1_low,
                completion_ordinal_0,
                completion_ordinal_0_low,
                completion_ordinal_2,
                completion_ordinal_2_low,
                qos_resource,
                sksm_qid_resource,
                pb_descriptor_table,
                pb_descriptor_table_low,
                uma_page_pool_descriptor_table,
                uma_page_pool_descriptor_table_low,
                parameter_metrics,
                parameter_metrics_low_va,
                fwctl,
                control_shared,
                control_shared_inner,
                partial_opening_control_shared,
                partial_opening_control_shared_inner,
                partial_primary_index_low,
                bootstrap_descriptor_zero_a,
                bootstrap_descriptor_zero_b,
                bootstrap_ta_context_peer,
                bootstrap_3d_context_peer,
                control_operand_page_lists,
                control_operand_table,
                control_operand_buffers,
                compute_runtime_support,
                compute_runtime_state,
                compute_runtime_state_mapping,
                compute_runtime_zero_buffer_0,
                compute_runtime_zero_buffer_1,
                compute_runtime_zero_buffer_0_mapping,
                compute_runtime_zero_buffer_1_mapping,
                compute_runtime_class1_table,
                compute_runtime_table_alias,
                compute_runtime_buffer_aliases,
                compute_readiness_class1_support,
                compute_readiness_class1_state,
                compute_readiness_class3_state,
                compute_readiness_context1,
                compute_readiness_class3_support_active,
                register_windows,
            },
            address_space,
            kernel_va_base,
        })
    }

    pub(crate) fn handoff(&self) -> &G17ResourceHandoff {
        &self.handoff
    }

    pub(crate) fn uat(&self) -> &mmu::Uat {
        &self.address_space.uat
    }

    pub(crate) fn mark_firmware_cache_flush_ready(&self) {
        self.address_space.uat.mark_firmware_cache_flush_ready();
    }

    /// Select the two global low/high region-view pairs used by the next
    /// serialized workload. Compute and render need different same-backing
    /// objects at these four main-config fields; the queue submission mutex
    /// keeps the selected mode resident until that workload has retired.
    fn stage_primary_region_views(
        &mut self,
        label: &str,
        low: [u64; 2],
        high: [u64; 2],
    ) -> Result {
        let shared_base = self.resources.shared_cluster.iova();
        let main_offset: usize = self
            .handoff
            .primary
            .instance
            .main_config
            .checked_sub(shared_base)
            .ok_or(ERANGE)?
            .try_into()
            .map_err(|_| ERANGE)?;
        let values = [low[0], high[0], low[1], high[1]];
        let mut changed = false;
        self.resources.shared_cluster.with_bytes_mut(|bytes| {
            for (index, value) in values.into_iter().enumerate() {
                let offset = main_offset
                    .checked_add(0x2d8 + index * 8)
                    .ok_or(EOVERFLOW)?;
                let slot = bytes.get_mut(offset..offset + 8).ok_or(ERANGE)?;
                let previous = u64::from_le_bytes(slot.try_into().map_err(|_| ERANGE)?);
                if previous != value {
                    slot.copy_from_slice(&value.to_le_bytes());
                    changed = true;
                }
            }
            Ok(())
        })?;
        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
        if changed {
            dev_info!(
                self.dev().as_ref(),
                "G17P resources: primary region views switched to {} [{:#x},{:#x}] [{:#x},{:#x}]\n",
                label,
                low[0],
                high[0],
                low[1],
                high[1],
            );
        }
        Ok(())
    }

    pub(crate) fn stage_compute_region_views(&mut self) -> Result {
        let low = [
            self.resources.primary_b2_alias_0_low.iova(),
            self.resources.primary_b2_alias_1_low.iova(),
        ];
        let high = [
            self.resources.partial_scheduler.iova(),
            self.resources.primary_b2_alias_1.iova(),
        ];
        self.stage_primary_region_views("compute B2", low, high)
    }

    pub(crate) fn stage_render_region_views(&mut self) -> Result {
        let low = [
            self.resources.pb_descriptor_table_low.iova(),
            self.resources.uma_page_pool_descriptor_table_low.iova(),
        ];
        let high = [
            self.resources.pb_descriptor_table.iova(),
            self.resources.uma_page_pool_descriptor_table.iova(),
        ];
        self.stage_primary_region_views("render PB/UMA", low, high)
    }

    /// Read one AGXQOSManager firmware-visible 8-byte queue record. Native
    /// `AGXQOSQueue::willSubmit` writes `{hardware_buffer_id, qos_class}` into
    /// bytes 0/1 at `queue_id * 8`; ConfigUpdate +0x56 carries that same
    /// hardware-buffer id. This resource is ordinary shared DRAM, so it is
    /// safe to sample after a timeout.
    pub(crate) fn qos_queue_record(&mut self, queue_id: u8) -> Result<[u8; 8]> {
        let offset = usize::from(queue_id).checked_mul(8).ok_or(EINVAL)?;
        core::sync::atomic::fence(core::sync::atomic::Ordering::Acquire);
        let vmap = self.resources.qos_resource.object.vmap()?;
        let bytes = unsafe {
            // SAFETY: the VMap covers the complete QoS resource and the slot
            // range is bounds-checked below.
            core::slice::from_raw_parts(vmap.as_ptr(), self.resources.qos_resource.logical_size)
        };
        let mut record = [0u8; 8];
        record.copy_from_slice(bytes.get(offset..offset + 8).ok_or(EINVAL)?);
        Ok(record)
    }

    /// Publish the firmware-visible part of native
    /// `AGXQOSQueue::willSubmit` for one render data master.
    ///
    /// The first 0x400 bytes of the 0xe40-byte QoS object are 128 records
    /// indexed by queue ID, not by HardwareBufferID. Native writes the
    /// ConfigUpdate `+0x56` HardwareBufferID to byte 0 and the queue's QoS
    /// class to byte 1 before it makes tag-15 visible. On the first submit it
    /// also publishes two HardwareBufferID-indexed properties: `+0x800` is the
    /// exact same `_AGFISchedulerState` firmware VA carried by ConfigUpdate
    /// `+0x4a`, while `+0xc00` is the queue's normalized QoS share. The
    /// remaining six queue-record bytes belong to other runtime state and must
    /// not be reset on a retained submission.
    pub(crate) fn publish_render_qos_queue_record(
        &mut self,
        header: G17PRenderConfigUpdateHeader,
    ) -> Result<[u8; 8]> {
        if header.queue_id >= G17P_QOS_QUEUE_COUNT as u16
            || header.field_56 >= G17P_QOS_QUEUE_COUNT as u16
        {
            return Err(EINVAL);
        }
        if header.scheduler_state == 0 {
            return Err(EINVAL);
        }
        let offset = usize::from(header.queue_id)
            .checked_mul(8)
            .ok_or(EINVAL)?;
        let hardware_buffer_id = usize::from(header.field_56);
        let scheduler_state_offset = 0x800usize
            .checked_add(hardware_buffer_id.checked_mul(8).ok_or(EINVAL)?)
            .ok_or(EINVAL)?;
        let share_offset = 0xc00usize
            .checked_add(hardware_buffer_id.checked_mul(4).ok_or(EINVAL)?)
            .ok_or(EINVAL)?;
        let mut published = [0u8; 8];
        self.resources.qos_resource.with_bytes_mut(|bytes| {
            let record = bytes.get_mut(offset..offset + 8).ok_or(EINVAL)?;
            record[0] = header.field_56 as u8;
            record[1] = G17P_DEFAULT_RENDER_QOS_CLASS;
            published.copy_from_slice(record);
            bytes
                .get_mut(scheduler_state_offset..scheduler_state_offset + 8)
                .ok_or(EINVAL)?
                .copy_from_slice(&header.scheduler_state.to_le_bytes());
            bytes
                .get_mut(share_offset..share_offset + 4)
                .ok_or(EINVAL)?
                .copy_from_slice(&G17P_SINGLE_RENDER_QOS_SHARE.to_le_bytes());
            Ok(())
        })?;
        // Native ends `willSubmit` with `dmb ish`. Order these shared WC
        // stores before any inner/outer producer or doorbell can expose the
        // corresponding tag-15 record to firmware.
        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
        Ok(published)
    }

    /// Dump the QOS resource regions `AGXQOSQueue::willSubmit` touches.
    ///
    /// The kext writes rows at `qid*8`, counters at `+0x400+qid*4`, `+0x600`,
    /// a u64 firmware VA per index at `+0x800`, `+0xc00`, and a further block
    /// at `+0xe00` that is indexed PER DATA MASTER. This driver writes only
    /// the `qid*8` row, `+0x800` and `+0xc00`, so the per-data-master block
    /// has never been written by us at all -- and data master 1 is the one
    /// that never dispatches, while 0 and 2 both do.
    /// DIAGNOSTIC. Dump the whole hardware-data block this driver builds.
    ///
    /// HW_DATA_OPAQUE_RUNS -- the 269 hardcoded bytes -- is verified byte-exact
    /// against native. The REST of the 0x3db4 block is computed here (register
    /// windows, completion aliases, QoS and SKSM resource pointers, geometry),
    /// and has never been compared. Native's equivalent block was captured
    /// whole from AGXArmFirmwareASC + 0x2b8, so a full diff is possible; only
    /// the address-shaped fields are expected to differ.
    pub(crate) fn dump_hw_data(&mut self, label: &str) {
        let mut window = [0u8; 0x40];
        let mut offset = 0usize;
        while offset < g17_initdata::HW_DATA_SIZE {
            let take = core::cmp::min(0x40, g17_initdata::HW_DATA_SIZE - offset);
            let ok = self
                .resources
                .shared_cluster
                .with_bytes_mut(|bytes| {
                    window[..take].copy_from_slice(bytes.get(offset..offset + take).ok_or(ERANGE)?);
                    Ok(())
                })
                .is_ok();
            if ok {
                let dev = self.dev().clone();
                let mut row = 0usize;
                while row < take {
                    let end = core::cmp::min(row + 16, take);
                    dev_info!(
                        dev.as_ref(),
                        "G17PHWDATA {} {:#07x} {:02x?}\n",
                        label,
                        offset + row,
                        &window[row..end],
                    );
                    row += 16;
                }
            }
            offset += take;
        }
    }

    pub(crate) fn dump_render_qos_regions(&mut self, label: &str) {
        const REGIONS: [(&str, usize, usize); 6] = [
            ("rows", 0x000, 0x40),
            ("cnt400", 0x400, 0x40),
            ("blk600", 0x600, 0x40),
            ("sched800", 0x800, 0x40),
            ("sharec00", 0xc00, 0x40),
            ("perdme00", 0xe00, 0x80),
        ];
        let mut windows = [[0u8; 0x80]; 6];
        let mut ok = [false; 6];
        let _ = self.resources.qos_resource.with_bytes_mut(|bytes| {
            for (index, (_, base, len)) in REGIONS.iter().enumerate() {
                let end = match base.checked_add(*len) {
                    Some(end) => end,
                    None => continue,
                };
                if let Some(slice) = bytes.get(*base..end) {
                    windows[index][..*len].copy_from_slice(slice);
                    ok[index] = true;
                }
            }
            Ok(())
        });
        let dev = self.dev().clone();
        for (index, (name, base, len)) in REGIONS.iter().enumerate() {
            if !ok[index] {
                continue;
            }
            for row in 0..(len / 16) {
                let at = row * 16;
                dev_info!(
                    dev.as_ref(),
                    "G17P QOS[{}] {} +{:#05x}: {:02x?}\n",
                    label,
                    name,
                    base + at,
                    &windows[index][at..at + 16],
                );
            }
        }
    }

    pub(crate) fn control_counters(&mut self) -> Result<G17PControlCounters> {
        core::sync::atomic::fence(core::sync::atomic::Ordering::Acquire);
        let primary = read_control_counters(
            &mut self.resources.primary_state,
            self.handoff.primary.state_grid,
        )?;
        let secondary = read_control_counters(
            &mut self.resources.secondary_state,
            self.handoff.secondary.state_grid,
        )?;
        Ok(G17PControlCounters { primary, secondary })
    }

    /// Read the exact primary channel-12 pointers serialized at main
    /// `+0x1a0/+0x1a8/+0x1b0`, then follow them through the retained WC state
    /// object. This is diagnostic only and performs no firmware write.
    pub(crate) fn control_pointer_snapshot(&mut self) -> Result<G17PControlPointerSnapshot> {
        let entry = g17_initdata::channel_table_entry_range(
            g17_initdata::CONTROL_CHANNEL_INDEX,
        )
        .ok_or(EINVAL)?;
        let main_config = self.handoff.primary.instance.main_config;
        let expected = g17_initdata::derive_channel_table(
            InstanceRole::Primary,
            self.handoff.primary.state_grid,
            self.handoff.primary.instance.status_a,
            main_config,
            self.handoff.shared.hw_data_bundle,
        )[g17_initdata::CONTROL_CHANNEL_INDEX];
        let mut raw_state_pointers = [0u64; g17_initdata::CHANNEL_ENTRY_STATE_COUNT];
        let mut state_values = [0u32; g17_initdata::CHANNEL_ENTRY_STATE_COUNT];
        for index in 0..g17_initdata::CHANNEL_ENTRY_STATE_COUNT {
            let address = main_config
                .checked_add(entry.start as u64)
                .and_then(|address| address.checked_add(index as u64 * 8))
                .ok_or(EINVAL)?;
            raw_state_pointers[index] = self.resources.shared_cluster.read_u64(address)?;
            state_values[index] = self.resources.primary_state.read_u32(raw_state_pointers[index])?;
        }
        Ok(G17PControlPointerSnapshot {
            main_config,
            raw_state_pointers,
            expected_state_pointers: expected.states,
            state_values,
        })
    }

    /// Read primary Status-A `+0x0c/+0x10/+0x14` through the existing WC
    /// object. Firmware maps this host-owned state grid uncached, so this
    /// snapshot does not rely on dirty private firmware cachelines.
    pub(crate) fn status_a_snapshot(&mut self) -> Result<G17PStatusASnapshot> {
        let scan_address = self
            .handoff
            .primary
            .state_grid
            .checked_add(PRIMARY_STATUS_A_WORK_SCAN_STATE_GRID_OFFSET)
            .ok_or(EINVAL)?;
        let address = self
            .handoff
            .primary
            .state_grid
            .checked_add(PRIMARY_STATUS_A_RUNTIME_STATE_GRID_OFFSET)
            .ok_or(EINVAL)?;
        let status_a = self.handoff.primary.instance.status_a;
        if scan_address
            != status_a
                .checked_add(PRIMARY_STATUS_A_WORK_SCAN_OFFSET)
                .ok_or(EINVAL)?
            || address
                != status_a
                    .checked_add(PRIMARY_STATUS_A_RUNTIME_OFFSET)
                    .ok_or(EINVAL)?
        {
            return Err(EINVAL);
        }
        core::sync::atomic::fence(core::sync::atomic::Ordering::Acquire);
        let scan_active = self.resources.primary_state.read_u32(scan_address)?;
        let raw = self.resources.primary_state.read_u64(address)?;
        Ok(G17PStatusASnapshot::decode(
            scan_address,
            scan_active,
            address,
            raw,
        ))
    }

    /// Read the exact primary Status-B firmware-recovery state through the
    /// existing WC object. Firmware maps this host-owned state grid uncached,
    /// so no private firmware cacheline is sampled.
    /// Read the firmware's fault report.
    ///
    /// The firmware reaches it through the pointer the host writes at
    /// `main_config + 0x26c`, which is bundle view 3, i.e.
    /// `hw_data_bundle + BUNDLE_VIEW_OFFSETS[3]`. The report names the blamed
    /// kick queue, that queue's data master, the reason, and the recovery
    /// state. The driver has never read it, so a firmware-initiated recovery
    /// has always been anonymous on our side.
    ///
    /// Two firmware paths fill it: the queue-progress fault scan in
    /// `fw_gpu_recovery` (b1 `0x5450`, which zeroes the presence tags at
    /// `0x5494-0x54a4` before scanning) and the KSM kick-timeout blame path in
    /// the recovery worker (b1 `0x26c94`, `0x26e40-0x26e9c`). Only the second
    /// fills the KSM-slot field.
    pub(crate) fn firmware_fault_report_snapshot(
        &mut self,
    ) -> Result<G17PFirmwareFaultReportSnapshot> {
        let main_config = self.handoff.primary.instance.main_config;
        let pointer_address = main_config
            .checked_add(MAIN_CONFIG_FAULT_REPORT_POINTER)
            .ok_or(EINVAL)?;
        let expected = self
            .handoff
            .shared
            .hw_data_bundle
            .checked_add(g17_initdata::BUNDLE_VIEW_OFFSETS[FAULT_REPORT_VIEW_INDEX] as u64)
            .ok_or(EINVAL)?;
        core::sync::atomic::fence(core::sync::atomic::Ordering::Acquire);
        let report = self.resources.shared_cluster.read_u64(pointer_address)?;
        if report != expected {
            return Err(EINVAL);
        }
        let mut read = |offset: u64| -> Result<u32> {
            self.resources
                .shared_cluster
                .read_u32(report.checked_add(offset).ok_or(EINVAL)?)
        };
        Ok(G17PFirmwareFaultReportSnapshot {
            report,
            state: read(FAULT_REPORT_RECOVERY_STATE)?,
            host_requested: read(FAULT_REPORT_HOST_REQUESTED)?,
            qid_present: read(FAULT_REPORT_BLAMED_QID_PRESENT)?,
            qid: read(FAULT_REPORT_BLAMED_QID)?,
            slot_present: read(FAULT_REPORT_BLAMED_SLOT_PRESENT)?,
            slot: read(FAULT_REPORT_BLAMED_SLOT)?,
            data_master_present: read(FAULT_REPORT_DATA_MASTER_PRESENT)?,
            data_master: read(FAULT_REPORT_DATA_MASTER)?,
            source_count: read(FAULT_REPORT_SOURCE_COUNT)?,
            reason: read(FAULT_REPORT_REASON)?,
        })
    }

    pub(crate) fn firmware_recovery_handshake_snapshot(
        &mut self,
    ) -> Result<G17PFirmwareRecoveryHandshakeSnapshot> {
        let status_b = self.handoff.primary.instance.status_b;
        let expected_status_b = self
            .handoff
            .primary
            .state_grid
            .checked_add(g17_initdata::PRIMARY_STATUS_B_STATE_GRID_OFFSET as u64)
            .ok_or(EINVAL)?;
        if status_b != expected_status_b {
            return Err(EINVAL);
        }
        let epoch_address = status_b
            .checked_add(PRIMARY_STATUS_B_FIRMWARE_RECOVERY_EPOCH_OFFSET)
            .ok_or(EINVAL)?;
        let state_address = status_b
            .checked_add(PRIMARY_STATUS_B_FIRMWARE_RECOVERY_STATE_OFFSET)
            .ok_or(EINVAL)?;
        let power_callback_count_address = self
            .handoff
            .primary
            .instance
            .status_a
            .checked_add(PRIMARY_STATUS_A_POWER_CALLBACK_COUNT_OFFSET)
            .ok_or(EINVAL)?;
        core::sync::atomic::fence(core::sync::atomic::Ordering::Acquire);
        let epoch = self.resources.primary_state.read_u64(epoch_address)?;
        let state = self.resources.primary_state.read_u32(state_address)?;
        let power_callback_count = self
            .resources
            .primary_state
            .read_u32(power_callback_count_address)?;
        Ok(G17PFirmwareRecoveryHandshakeSnapshot::decode(
            status_b,
            epoch_address,
            epoch,
            state_address,
            state,
            power_callback_count_address,
            power_callback_count,
        ))
    }

    /// Consume one primary firmware-event record using the exact native ring
    /// pointer pair serialized at main-config channel 13. J700 A000 treats
    /// word 0 as the shared cursor object and word 1 as the 0x48-byte ring
    /// base; producer is cursor `+0x20`. The consumer is published only after
    /// the complete record has been copied.
    pub(crate) fn drain_primary_firmware_event(
        &mut self,
    ) -> Result<Option<G17PPrimaryFirmwareEventRecord>> {
        let entry = g17_initdata::channel_table_entry_range(
            PRIMARY_FIRMWARE_EVENT_CHANNEL_INDEX,
        )
        .ok_or(EINVAL)?;
        let main_config = self.handoff.primary.instance.main_config;
        let expected = g17_initdata::derive_channel_table(
            InstanceRole::Primary,
            self.handoff.primary.state_grid,
            self.handoff.primary.instance.status_a,
            main_config,
            self.handoff.shared.hw_data_bundle,
        )[PRIMARY_FIRMWARE_EVENT_CHANNEL_INDEX];
        let cursor_address = self.resources.shared_cluster.read_u64(
            main_config
                .checked_add(entry.start as u64)
                .ok_or(EINVAL)?,
        )?;
        let ring_address = self.resources.shared_cluster.read_u64(
            main_config
                .checked_add(entry.start as u64)
                .and_then(|address| address.checked_add(8))
                .ok_or(EINVAL)?,
        )?;
        if cursor_address != expected.states[0] || ring_address != expected.states[1] {
            dev_err!(
                self.dev().as_ref(),
                "G17P firmware-event ring: pointer mismatch cursor={:#x}/{:#x} ring={:#x}/{:#x}\n",
                cursor_address,
                expected.states[0],
                ring_address,
                expected.states[1]
            );
            return Err(EINVAL);
        }

        core::sync::atomic::fence(core::sync::atomic::Ordering::Acquire);
        let consumer = self.resources.primary_state.read_u32(cursor_address)?;
        let producer = self.resources.primary_state.read_u32(
            cursor_address
                .checked_add(g17_completion::G17P_FIRMWARE_EVENT_PRODUCER_OFFSET as u64)
                .ok_or(EINVAL)?,
        )?;
        let read = match g17_completion::prepare_g17p_firmware_event_read(
            g17_completion::G17PFirmwareEventCursor {
                consumer,
                producer,
            },
        ) {
            Ok(read) => read,
            Err(g17_completion::CompletionDecodeError::FirmwareEventEmpty) => return Ok(None),
            Err(error) => {
                dev_err!(
                    self.dev().as_ref(),
                    "G17P firmware-event ring: cursor consumer={} producer={} rejected ({:?})\n",
                    consumer,
                    producer,
                    error
                );
                return Err(EIO);
            }
        };
        let record_address = ring_address
            .checked_add(read.entry_offset as u64)
            .ok_or(EINVAL)?;
        let record_offset = record_address
            .checked_sub(self.resources.primary_state.iova())
            .ok_or(EINVAL)? as usize;
        let record_end = record_offset
            .checked_add(g17_completion::G17P_FIRMWARE_EVENT_ENTRY_SIZE)
            .ok_or(EINVAL)?;
        let mut raw = [0u8; g17_completion::G17P_FIRMWARE_EVENT_ENTRY_SIZE];
        self.resources.primary_state.with_bytes_mut(|bytes| {
            raw.copy_from_slice(bytes.get(record_offset..record_end).ok_or(EINVAL)?);
            Ok(())
        })?;
        let event_type = u32::from_le_bytes(raw[0..4].try_into().map_err(|_| EIO)?);
        let validation = match event_type {
            // Type 1 has had a decoder in g17_completion since the completion
            // work, but nothing ever dispatched to it, so every stamp-signal
            // record was logged as unmodelled and dropped. It is the
            // firmware's own statement of which stamps it signalled: compute
            // emits mask 0x10 (stamp 4, its QID) on the submission that
            // passes, and a failing render emits 0x22 (stamps 1 and 5).
            g17_completion::G17P_FIRMWARE_EVENT_STAMP_TYPE => {
                match g17_completion::decode_g17p_firmware_stamp_event(&raw) {
                    Ok(event) => {
                        dev_info!(
                            self.dev().as_ref(),
                            "G17P firmware-event ring: stamp signal cursor={} producer={} masks=[{:#018x},{:#018x}] diagnostics={}\n",
                            consumer,
                            producer,
                            event.masks[0],
                            event.masks[1],
                            event.diagnostic_pair_count,
                        );
                        Ok(())
                    }
                    Err(error) => Err(error),
                }
            }
            g17_completion::G17P_FIRMWARE_EVENT_RECOVERY_TYPE => {
                g17_completion::decode_g17p_firmware_recovery_event(&raw).map(|_| ())
            }
            g17_completion::G17P_FIRMWARE_EVENT_UMA_GROW_TYPE => {
                g17_completion::decode_g17p_firmware_uma_grow_event(&raw).map(|_| ())
            }
            _ => Err(g17_completion::CompletionDecodeError::FirmwareEventType),
        };
        if let Err(error) = validation {
            let raw_qword = |offset: usize| {
                u64::from_le_bytes(raw[offset..offset + 8].try_into().unwrap_or([0; 8]))
            };
            dev_err!(
                self.dev().as_ref(),
                "G17P firmware-event ring: record type={} cursor={} producer={} raw=[{:#x},{:#x},{:#x},{:#x},{:#x},{:#x},{:#x},{:#x},{:#x}] unmodelled ({:?}); consuming\n",
                event_type,
                consumer,
                producer,
                raw_qword(0x00),
                raw_qword(0x08),
                raw_qword(0x10),
                raw_qword(0x18),
                raw_qword(0x20),
                raw_qword(0x28),
                raw_qword(0x30),
                raw_qword(0x38),
                raw_qword(0x40),
                error
            );
            // Do NOT stall the ring on a record we do not model. Holding the
            // consumer back made one unmodelled entry block every later record
            // forever: once the firmware actually executed a kick it published
            // a type-1 record (payload tagged "RTKSTACK") ahead of the
            // completion, and the strict type-4/type-13 allowlist rejected it
            // with the consumer unchanged, so the drain returned EIO on every
            // poll and the submission could never retire. Log it and fall
            // through to the consumer publish below; the batch dispatcher
            // counts it as unhandled.
        }

        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
        self.resources
            .primary_state
            .write_u32(cursor_address, read.published_consumer)?;
        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
        Ok(Some(G17PPrimaryFirmwareEventRecord {
            raw,
            consumer_before: consumer,
            producer_snapshot: producer,
            consumer_published: read.published_consumer,
        }))
    }

    pub(crate) fn primary_firmware_recovery_cause_snapshot(
        &mut self,
    ) -> Result<G17PFirmwareRecoveryCauseSnapshot> {
        let status_b = self.handoff.primary.instance.status_b;
        let mut read_u32 = |offset: u64| {
            self.resources
                .primary_state
                .read_u32(status_b.checked_add(offset).ok_or(EINVAL)?)
        };
        let zero_0 = read_u32(PRIMARY_STATUS_B_RECOVERY_ZERO_0_OFFSET)?;
        let detected_source_count =
            read_u32(PRIMARY_STATUS_B_RECOVERY_SOURCE_COUNT_OFFSET)?;
        let zero_1 = read_u32(PRIMARY_STATUS_B_RECOVERY_ZERO_1_OFFSET)?;
        let command_word = read_u32(PRIMARY_STATUS_B_RECOVERY_COMMAND_WORD_OFFSET)?;
        let raw_48cc = read_u32(PRIMARY_STATUS_B_RECOVERY_RAW_48CC_OFFSET)?;
        let host_recovery = read_u32(PRIMARY_STATUS_B_RECOVERY_OBJECT_ACTIVE_OFFSET)?;
        let raw_48d0 = self.resources.primary_state.read_u64(
            status_b
                .checked_add(PRIMARY_STATUS_B_RECOVERY_RAW_48D0_OFFSET)
                .ok_or(EINVAL)?,
        )?;
        let fault_status = self.resources.primary_state.read_u64(
            status_b
                .checked_add(PRIMARY_STATUS_B_RECOVERY_FAULT_STATUS_OFFSET)
                .ok_or(EINVAL)?,
        )?;
        Ok(G17PFirmwareRecoveryCauseSnapshot {
            zero_0,
            detected_source_count,
            zero_1,
            command_word,
            raw_48cc,
            raw_48d0,
            fault_status,
            host_recovery,
        })
    }

    /// Scan the exact 256-entry Status-B recovery-info table after firmware
    /// publishes state 3. Native macOS checks every entry's VALID bit instead
    /// of trusting the emitted-entry count at the end of the table.
    pub(crate) fn primary_firmware_recovery_info_snapshot(
        &mut self,
    ) -> Result<G17PFirmwareRecoveryInfoSnapshot> {
        let status_b = self.handoff.primary.instance.status_b;
        core::sync::atomic::fence(core::sync::atomic::Ordering::Acquire);
        let mut valid_entry_count = 0u32;
        let mut first_valid_index = None;
        let mut first_valid_flags = 0u32;
        for index in 0..PRIMARY_STATUS_B_RECOVERY_INFO_ENTRY_COUNT {
            let flags = self.resources.primary_state.read_u32(
                status_b
                    .checked_add(PRIMARY_STATUS_B_RECOVERY_INFO_OFFSET)
                    .and_then(|address| {
                        address.checked_add(
                            (index as u64)
                                .checked_mul(PRIMARY_STATUS_B_RECOVERY_INFO_ENTRY_STRIDE)?,
                        )
                    })
                    .ok_or(EINVAL)?,
            )?;
            if flags & 1 != 0 {
                valid_entry_count = valid_entry_count.checked_add(1).ok_or(EOVERFLOW)?;
                if first_valid_index.is_none() {
                    first_valid_index = Some(index as u32);
                    first_valid_flags = flags;
                }
            }
        }
        let emitted_entry_count = self.resources.primary_state.read_u32(
            status_b
                .checked_add(PRIMARY_STATUS_B_RECOVERY_INFO_EMITTED_COUNT_OFFSET)
                .ok_or(EINVAL)?,
        )?;
        Ok(G17PFirmwareRecoveryInfoSnapshot {
            emitted_entry_count,
            valid_entry_count,
            first_valid_index,
            first_valid_flags,
        })
    }

    /// Dump the raw recovery cause block that follows the recovery-info table
    /// (`status_b + 0x48b8` onward, where the emitted count lives) plus the
    /// first few table entries. The driver only decodes a handful of named
    /// fields from here; on J700 every looping recovery reports the same
    /// `+0x48cc == 3` with everything else zero, so the surrounding words are
    /// what is needed to identify the request.
    /// Drain the firmware log ring. Nine per-thread sub-rings share one text
    /// area; each control block is `status_a + 0x80 + i*0x30`, with the read
    /// pointer at `+0x00` and the write pointer at `+0x20`. Text records are
    /// `0xd8` bytes: `{u32 type, u32 seq, u64 timestamp, char[0xc8]}`, already
    /// formatted ASCII.
    /// Arm the firmware trace-class mask at `status_a + 0x00`. The firmware
    /// samples it while parsing initdata, so this must run before the ASCs are
    /// started. It gates the verbose log classes and the KTrace ring.
    pub(crate) fn arm_firmware_trace_classes(&mut self, mask: u32) -> Result {
        // Each ASC parses its own initdata, latches its own `status_a`
        // pointer, and runs its own guarded trace sites, so the mask has to
        // land in both objects. Both existing call sites already run after the
        // resource re-encode that zero-fills `secondary_status_a` and rewrites
        // the two status blocks, and both run before the initdata doorbell,
        // which is when the firmware samples this word.
        let primary = self.handoff.primary.instance.status_a;
        let secondary = self.handoff.secondary.instance.status_a;
        let before_primary = self.resources.primary_state.read_u32(primary)?;
        self.resources.primary_state.write_u32(primary, mask)?;
        let after_primary = self.resources.primary_state.read_u32(primary)?;
        let before_secondary = self.resources.secondary_status_a.read_u32(secondary)?;
        self.resources
            .secondary_status_a
            .write_u32(secondary, mask)?;
        let after_secondary = self.resources.secondary_status_a.read_u32(secondary)?;
        dev_info!(
            self.dev().as_ref(),
            "G17P firmware trace classes: primary {:#x} -> {:#x} secondary {:#x} -> {:#x} (requested {:#x})\n",
            before_primary,
            after_primary,
            before_secondary,
            after_secondary,
            mask
        );
        Ok(())
    }

    /// Read the KSM completion record for the in-flight compute submission.
    ///
    /// Layout of completion ordinal 0, from the firmware's own consumer
    /// (`b1` `0xd550` reads the ring; `0xab64` decodes one record). The
    /// producer is the KSM hardware block, not the firmware -- the firmware
    /// only `clean_invalidate`s the entries and reads them, so there is no
    /// firmware store order to appeal to and every field must be validated on
    /// its own terms:
    ///
    /// ```text
    /// +0x00   packed control word (kind, class, data master, queue id, stamp)
    /// +0x08   u32; only bits [28:5] are tested, and they select which
    ///         completion tail the firmware runs -- unmodelled here
    /// +0x10, +0x18   timestamp pair A, each masked to 54 bits
    /// +0x20, +0x28   timestamp pair B, each masked to 54 bits; this is the
    ///                pair fed to the execution-time attributor and rebased
    ///                into the descriptor's user-timestamp objects
    /// +0x30   the submitted descriptor address, with a FLAG in bit 0
    /// +0x38   the CL queue address
    /// ```
    ///
    /// So it is two ordered (start, end) pairs, not "four increasing
    /// timestamps", and the top ten bits of each are tag bits rather than
    /// time. Returns pair B, which is what a completion means and what the
    /// compute UAPI reports.
    /// Zero KSM completion ordinal 0 immediately before a submission is
    /// published.
    ///
    /// Ordinal 0 is a single slot the firmware overwrites, so without this a
    /// record left by an EARLIER submission is indistinguishable from a fresh
    /// one and satisfies the next wait instantly. That is not hypothetical: a
    /// submission observed with `gpc-state = 0x0` -- GPU cores never powered,
    /// so the work provably never ran -- still "woke" the doorbell and
    /// retired, because the predicate only asked whether the record was
    /// populated. After this, populated means written since this publish.
    ///
    /// Returns the record's previous `+0x30` (descriptor back-reference, with
    /// the bit-0 flag masked off) and `+0x28` (pair-B end timestamp, masked to
    /// 54 bits) so a caller can log what it is discarding.
    pub(crate) fn clear_compute_completion(&mut self) -> Result<[u64; 2]> {
        let object = &mut self.resources.completion_ordinal_0;
        let base = object.iova();
        if object.logical_size < G17P_COMPUTE_COMPLETION_RECORD_SIZE {
            return Err(ERANGE);
        }
        let previous = [
            object.read_u64(base + 0x30)? & G17P_COMPUTE_COMPLETION_DESCRIPTOR_MASK,
            object.read_u64(base + 0x28)? & G17P_COMPUTE_COMPLETION_TIMESTAMP_MASK,
        ];
        object.with_bytes_mut(|raw| {
            raw.get_mut(..G17P_COMPUTE_COMPLETION_RECORD_SIZE)
                .ok_or(ERANGE)?
                .fill(0);
            Ok(())
        })?;
        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
        Ok(previous)
    }

    /// Read KSM completion ordinal 0, rejecting a record that is not the one
    /// the caller is waiting for.
    ///
    /// Ordinal 0 is a single slot the firmware overwrites, so once more than
    /// one submission runs per boot a populated record is NOT automatically
    /// ours. Two independent discriminators are applied:
    ///
    ///  * `+0x30` echoes the submitted descriptor address (established on
    ///    hardware: it matched `0xfffffc20003a4000`). Every submission uses a
    ///    different descriptor slot, so this identifies the owner directly.
    ///    It is advisory -- a mismatch is logged, not fatal -- because the
    ///    field's meaning rests on one observation.
    ///  * `+0x28`, one of four monotonically increasing GPU timestamps, must
    ///    differ from the end timestamp of the completion already consumed.
    ///    This one is layout-independent and is what actually gates the
    ///    return, so a stale record can never satisfy the next wait.
    /// Decode every completion-ring slot: index, descriptor, queue, start/end.
    ///
    /// The raw word dump proved the buffer is a ring; this makes the ring's
    /// contents legible in one run so a depth failure can be diagnosed without
    /// hand-decoding hex.
    /// Scan both KSM completion ordinals.
    ///
    /// Ordinal 0 is compute's. RENDER's completion selector is 2
    /// (`RENDER_COMPLETION_SELECTOR` in `new_render_sksm_queues`), and until now
    /// nothing read ordinal 2 at all -- so a render kick that DID complete
    /// would have written a record no code path ever looked at, and would have
    /// been indistinguishable from one that never ran.
    pub(crate) fn dump_completion_records(&mut self, label: &'static str) -> Result {
        self.dump_completion_ordinal(label, 0)?;
        self.dump_completion_ordinal(label, 2)
    }

    fn dump_completion_ordinal(&mut self, label: &'static str, ordinal: u8) -> Result {
        let stride = G17P_COMPUTE_COMPLETION_RECORD_SIZE as u64;
        let mut rows: [(u64, u64, u64, u64); 24] = [(0, 0, 0, 0); 24];
        let mut n = 0usize;
        let (base, slots) = {
            let object = if ordinal == 0 {
                &mut self.resources.completion_ordinal_0
            } else {
                &mut self.resources.completion_ordinal_2
            };
            let base = object.iova();
            let slots = (object.logical_size as u64 / stride).min(24);
            for slot in 0..slots {
                let rec = base + slot * stride;
                let descriptor =
                    object.read_u64(rec + 0x30)? & G17P_COMPUTE_COMPLETION_DESCRIPTOR_MASK;
                let queue = object.read_u64(rec + 0x38)?;
                // Pair B, masked -- the window a completion actually reports.
                let start =
                    object.read_u64(rec + 0x20)? & G17P_COMPUTE_COMPLETION_TIMESTAMP_MASK;
                let end =
                    object.read_u64(rec + 0x28)? & G17P_COMPUTE_COMPLETION_TIMESTAMP_MASK;
                if descriptor != 0 || queue != 0 || end != 0 {
                    rows[n] = (slot, descriptor, start, end);
                    n += 1;
                }
            }
            (base, slots)
        };
        let dev = self.dev().clone();
        dev_info!(
            dev.as_ref(),
            "G17P completion ring[{}] ordinal={} base={:#x} slots={} populated={}\n",
            label,
            ordinal,
            base,
            slots,
            n
        );
        for row in rows.iter().take(n) {
            dev_info!(
                dev.as_ref(),
                "G17P completion ring[{}] ordinal={} slot {} desc={:#x} start={:#x} end={:#x}\n",
                label,
                ordinal,
                row.0,
                row.1,
                row.2,
                row.3
            );
        }
        Ok(())
    }

    pub(crate) fn read_compute_completion(
        &mut self,
        expected_descriptor: u64,
        last_consumed_end: u64,
    ) -> Result<Option<[u64; 2]>> {
        // The completion buffer is a RING of 0x40-strided records, not a single
        // record. Proven on hardware: after a repeat submission timed out, the
        // timeout dump showed a second descriptor/queue back-reference pair at
        // +0x70/+0x78 -- exactly 0x40 past the first at +0x30/+0x38. Reading only
        // record 0 is why repeats "published but never signalled": the firmware
        // had reported the completion, into the next slot, and we never looked.
        let object = &mut self.resources.completion_ordinal_0;
        let stride = G17P_COMPUTE_COMPLETION_RECORD_SIZE;
        // Take the slot count from the geometry that was published to the
        // firmware, not from a guess. Initdata programs this ring as
        // `G17P_KSM_COMPLETION_CAPACITY` records of
        // `G17P_KSM_COMPLETION_ENTRY_SIZE` bytes and the object is sized to
        // exactly that, so the two models agree by construction.
        //
        // The previous `.min(64)` was a latent SECOND wall: nothing keeps the
        // KSM's write index inside the first 64 slots, and a completion landing
        // beyond them would simply never be found -- an unbounded run would
        // have hit it as a hang. It is NOT the 320 wall; that one is the ITEM
        // ring (`0x500` entries at four records per submission = 320
        // submissions), which is a different ring in a different object.
        let slots = core::cmp::min(
            object.logical_size / stride,
            g17_initdata::G17P_KSM_COMPLETION_CAPACITY as usize,
        );
        if slots == 0 {
            return Ok(None);
        }
        // One borrow for the whole ring. The producer is the KSM hardware
        // block, which can update a record between two host loads, so reading
        // a record's back-reference and its timestamps as separate loads can
        // mix a fresh back-reference with a not-yet-written timestamp. Each
        // record below is decoded from one contiguous slice; the firmware does
        // the same thing -- `clean_invalidate` the entries, then decode.
        object.with_bytes_mut(|bytes| {
        let mut best: Option<[u64; 2]> = None;
        for slot in 0..slots {
            let offset = slot * stride;
            let record = bytes.get(offset..offset + stride).ok_or(ERANGE)?;
            let field = |at: usize| -> u64 {
                u64::from_le_bytes(record[at..at + 8].try_into().unwrap_or([0; 8]))
            };
            // +0x30 carries a flag in bit 0. The firmware masks it off before
            // it dereferences the pointer (`and x26, x22, #~1`), so an exact
            // comparison against the published descriptor address can miss --
            // or, worse, a record whose only populated field is that flag
            // looks like a live back-reference. Mask first, then compare.
            let descriptor = field(0x30) & G17P_COMPUTE_COMPLETION_DESCRIPTOR_MASK;
            if descriptor == 0 || field(0x38) == 0 {
                continue;
            }
            if expected_descriptor != 0 && descriptor != expected_descriptor {
                continue;
            }
            // The record does NOT carry "four increasing timestamps": it
            // carries TWO (start, end) pairs, A at +0x10/+0x18 and B at
            // +0x20/+0x28, and the firmware masks every one of them to 54 bits
            // before use -- the top ten bits are tag bits, not time. Reading
            // them raw is a real defect: a record that has been claimed but
            // not yet stamped has `raw != 0` while the timestamp itself is
            // still zero, so the old "end != 0" test could accept a slot the
            // KSM had only reserved.
            //
            // The FIELDS are deliberately the same two the proven reader used,
            // so this is a correction and not a new predicate; only the
            // masking is new. Gating on pair B's start as well would be more
            // faithful to the firmware's own use of the record, but nothing
            // establishes that a compute completion stamps it, and an unproven
            // field in the predicate can only add ways to hang. The decoded
            // dump prints all of them so that can be settled from a run.
            let start = field(0x10) & G17P_COMPUTE_COMPLETION_TIMESTAMP_MASK;
            let end = field(0x28) & G17P_COMPUTE_COMPLETION_TIMESTAMP_MASK;
            if start == 0 || end == 0 {
                continue;
            }
            // Timestamps are monotonic, so ANY record from a previous
            // submission has end <= last_consumed_end. Excluding only the
            // single most-recently-consumed end (== ) left every OLDER slot
            // eligible: on a fast repeat, before the firmware had written this
            // submission's record, a record from two-or-more submissions ago
            // was accepted as ours. The caller then read a buffer the GPU had
            // not written yet and saw zeroes. Proven on hardware: repeat depth
            // tracked how much delay the submit path happened to get -- 8 with
            // light logging, 28 with a 2ms settle, 100 only when console
            // tracing padded every submission by milliseconds.
            if end <= last_consumed_end {
                continue;
            }
            // Prefer the newest record, so a stale slot can never win over the
            // one this submission just produced.
            if best.is_none_or(|b| end > b[1]) {
                best = Some([start, end]);
            }
        }
        Ok(best)
        })
    }

    /// Decode and log the KSM completion ring.
    ///
    /// The raw word dump is what found the ring in the first place, but it
    /// truncates at 24 non-zero words -- about three records -- and prints
    /// offsets rather than meaning. This prints one line per POPULATED record:
    /// slot index, the descriptor it reports, the queue, and the start/end
    /// timestamps, alongside the descriptor the waiting submission expects. A
    /// timeout is then a one-run diagnosis: either the expected descriptor is
    /// absent (the firmware reported nothing for this submission) or it is
    /// present (the firmware reported it and the reader's predicate rejected
    /// it).
    ///
    /// Scans the whole object, not the first `0x400` bytes. DRAM only.
    pub(crate) fn dump_compute_completion_records(
        &mut self,
        label: &'static str,
        expected_descriptor: u64,
    ) -> Result {
        const LOG_LIMIT: usize = 24;
        let stride = G17P_COMPUTE_COMPLETION_RECORD_SIZE as u64;
        let mut populated = 0usize;
        let mut logged = 0usize;
        let mut newest: Option<(u64, u64)> = None;
        let mut matched: Option<u64> = None;
        // slot, descriptor, queue, pair-A start, pair-B start, pair-B end,
        // and the +0x08 routing word.
        let mut lines: [(u64, u64, u64, u64, u64, u64, u32); LOG_LIMIT] =
            [(0, 0, 0, 0, 0, 0, 0); LOG_LIMIT];
        // Scope the object borrow so the logging below can reach `self.dev()`.
        let (base, logical_size, slots) = {
            let object = &mut self.resources.completion_ordinal_0;
            let base = object.iova();
            let logical_size = object.logical_size;
            let slots = logical_size as u64 / stride;
            for slot in 0..slots {
                let rec = base + slot * stride;
                let descriptor =
                    object.read_u64(rec + 0x30)? & G17P_COMPUTE_COMPLETION_DESCRIPTOR_MASK;
                let queue = object.read_u64(rec + 0x38)?;
                if descriptor == 0 && queue == 0 {
                    continue;
                }
                // Every timestamp masked exactly as the firmware masks it, so
                // a record that is claimed but not yet stamped shows as zero
                // here instead of as a large tag-bit value.
                let a_start =
                    object.read_u64(rec + 0x10)? & G17P_COMPUTE_COMPLETION_TIMESTAMP_MASK;
                let start =
                    object.read_u64(rec + 0x20)? & G17P_COMPUTE_COMPLETION_TIMESTAMP_MASK;
                let end =
                    object.read_u64(rec + 0x28)? & G17P_COMPUTE_COMPLETION_TIMESTAMP_MASK;
                let routing = object.read_u32(rec + 0x08)?;
                populated += 1;
                if newest.is_none_or(|(_, best_end)| end > best_end) {
                    newest = Some((slot, end));
                }
                if expected_descriptor != 0 && descriptor == expected_descriptor {
                    matched = Some(slot);
                }
                if logged < LOG_LIMIT {
                    lines[logged] = (slot, descriptor, queue, a_start, start, end, routing);
                    logged += 1;
                }
            }
            (base, logical_size, slots)
        };
        let dev = self.dev().clone();
        for entry in lines.iter().take(logged) {
            dev_info!(
                dev.as_ref(),
                "G17P completion[{}]: rec {} descriptor {:#x} queue {:#x} a-start {:#x} b-start {:#x} b-end {:#x} route {:#x}\n",
                label,
                entry.0,
                entry.1,
                entry.2,
                entry.3,
                entry.4,
                entry.5,
                entry.6
            );
        }
        dev_info!(
            dev.as_ref(),
            "G17P completion[{}]: base {:#x} size {:#x} stride {:#x} slots {} populated {} logged {} expected-descriptor {:#x} matched-rec {:?} newest-rec {:?}\n",
            label,
            base,
            logical_size,
            stride,
            slots,
            populated,
            logged,
            expected_descriptor,
            matched,
            newest.map(|(slot, _)| slot)
        );
        Ok(())
    }

    /// Dump the KSM completion-stamp buffers.
    ///
    /// On M1/M2 the firmware reports work completion by writing an event
    /// "stamp" into memory (upstream `EventValue`, which advances by 0x100 per
    /// completion) rather than by publishing a record in the firmware event
    /// ring -- that ring carries telemetry. macOS confirms the same split on
    /// G17P: its event ring only produced types 0x11-0x1c during a compute
    /// dispatch while the completion ran on a separate thread. Our firmware
    /// KTrace shows `0x0e stamp-update value=0x100 index=4`, so the stamp lands
    /// somewhere in these buffers; report every non-zero word so the layout can
    /// be established from hardware rather than guessed.
    pub(crate) fn dump_completion_stamps(&mut self, label: &'static str) -> Result {
        let mut found: [(usize, usize, u32); 24] = [(0, 0, 0); 24];
        let mut n = 0usize;
        let mut counts = [0usize; 2];
        let mut bases = [0u64; 2];
        let mut limits = [0usize; 2];
        for (slot, which) in [0usize, 2usize].into_iter().enumerate() {
            let object = if which == 0 {
                &mut self.resources.completion_ordinal_0
            } else {
                &mut self.resources.completion_ordinal_2
            };
            let base = object.iova();
            let limit = core::cmp::min(object.logical_size, 0x400);
            bases[slot] = base;
            limits[slot] = limit;
            let mut offset = 0usize;
            while offset < limit {
                let value = object.read_u32(base + offset as u64)?;
                if value != 0 {
                    if n < found.len() {
                        found[n] = (which, offset, value);
                        n += 1;
                    }
                    counts[slot] += 1;
                }
                offset += 4;
            }
        }
        let dev = self.dev().clone();
        for entry in found.iter().take(n) {
            dev_info!(
                dev.as_ref(),
                "G17P completion stamps[{}] ord{} +{:#x} = {:#x}\n",
                label,
                entry.0,
                entry.1,
                entry.2
            );
        }
        for (slot, which) in [0usize, 2usize].into_iter().enumerate() {
            dev_info!(
                dev.as_ref(),
                "G17P completion stamps[{}] ord{} base={:#x} scanned={:#x} non-zero={}\n",
                label,
                which,
                bases[slot],
                limits[slot],
                counts[slot]
            );
        }
        Ok(())
    }

    pub(crate) fn dump_firmware_log(&mut self, max_records: usize) -> Result<usize> {
        const CONTROL_BASE: u64 = 0x80;
        const CONTROL_STRIDE: u64 = 0x30;
        const RING_COUNT: u64 = 9;
        const TEXT_BASE: u64 = 0x2d2c0;
        const RECORD_STRIDE: u64 = 0xd8;
        const RECORD_TEXT: u64 = 0x10;
        const RECORDS_PER_RING: u64 = 0x900 / RING_COUNT;

        let status_a = self.handoff.primary.instance.status_a;
        core::sync::atomic::fence(core::sync::atomic::Ordering::Acquire);
        let mut shown = 0usize;
        for ring in 0..RING_COUNT {
            let control = status_a
                .checked_add(CONTROL_BASE + ring * CONTROL_STRIDE)
                .ok_or(EINVAL)?;
            let read_ptr = self.resources.primary_state.read_u32(control)?;
            let write_ptr = self
                .resources
                .primary_state
                .read_u32(control.checked_add(0x20).ok_or(EINVAL)?)?;
            if write_ptr == 0 && read_ptr == 0 {
                continue;
            }
            dev_info!(
                self.dev().as_ref(),
                "G17P fwlog ring {}: read={} write={}\n",
                ring,
                read_ptr,
                write_ptr
            );
            for index in 0..write_ptr.min(RECORDS_PER_RING as u32) {
                if shown >= max_records {
                    return Ok(shown);
                }
                let record = status_a
                    .checked_add(TEXT_BASE)
                    .and_then(|a| {
                        a.checked_add((ring * RECORDS_PER_RING + index as u64) * RECORD_STRIDE)
                    })
                    .ok_or(EINVAL)?;
                let mut text = [0u8; 0x60];
                for (i, byte) in text.iter_mut().enumerate() {
                    let word = self.resources.primary_state.read_u32(
                        record
                            .checked_add(RECORD_TEXT + (i as u64 & !3))
                            .ok_or(EINVAL)?,
                    )?;
                    *byte = (word >> ((i as u32 & 3) * 8)) as u8;
                }
                let end = text.iter().position(|b| *b == 0).unwrap_or(text.len());
                let printable = &text[..end];
                if printable.is_empty() {
                    continue;
                }
                dev_info!(
                    self.dev().as_ref(),
                    "G17P fwlog[{}.{}]: {}\n",
                    ring,
                    index,
                    core::str::from_utf8(printable).unwrap_or("<non-utf8>")
                );
                shown += 1;
            }
        }
        Ok(shown)
    }

    /// The mapped object that backs one role's `status_a` block. Primary
    /// `status_a` lives inside `primary_state` at +0xee40; the secondary owns
    /// a dedicated object whose base *is* its `status_a`.
    fn ktrace_object(&mut self, role: InstanceRole) -> &mut MappedObject {
        match role {
            InstanceRole::Primary => &mut self.resources.primary_state,
            InstanceRole::Secondary => &mut self.resources.secondary_status_a,
        }
    }

    /// Cross-check the published channel-13 event pointer pair against the
    /// channel-table derivation, exactly as the channel-14 check does.
    pub(crate) fn check_report_ring_pointers(&mut self, role: InstanceRole) -> Result<(u64, u64)> {
        let entry = g17_initdata::channel_table_entry_range(
            g17_completion::G17P_EVENT_CHANNEL_TABLE_INDEX,
        )
        .ok_or(EINVAL)?;
        let (main_config, status_a, state_grid) = match role {
            InstanceRole::Primary => (
                self.handoff.primary.instance.main_config,
                self.handoff.primary.instance.status_a,
                self.handoff.primary.state_grid,
            ),
            InstanceRole::Secondary => (
                self.handoff.secondary.instance.main_config,
                self.handoff.secondary.instance.status_a,
                self.handoff.secondary.state_grid,
            ),
        };
        let expected = g17_initdata::derive_channel_table(
            role,
            state_grid,
            status_a,
            main_config,
            self.handoff.shared.hw_data_bundle,
        )[g17_completion::G17P_EVENT_CHANNEL_TABLE_INDEX];
        let base = main_config.checked_add(entry.start as u64).ok_or(EINVAL)?;
        let published_state = self.resources.shared_cluster.read_u64(base)?;
        let published_ring = self
            .resources
            .shared_cluster
            .read_u64(base.checked_add(8).ok_or(EINVAL)?)?;
        if published_state != expected.states[0] || published_ring != expected.states[1] {
            dev_err!(
                self.dev().as_ref(),
                "G17P report: channel-13 pointer mismatch state={:#x}/{:#x} ring={:#x}/{:#x}\n",
                published_state,
                expected.states[0],
                published_ring,
                expected.states[1]
            );
            return Err(EINVAL);
        }
        Ok((published_state, published_ring))
    }

    /// Read one instance's firmware event ring (channel-table entry 13).
    ///
    /// This ring carries `Fault` / `Flag` / `Timeout` / `GrowTilingBuffer` /
    /// `ChannelError`, and this driver has never read it: `decode_firmware_report`
    /// and `decode_channel13_event` had no live caller at all, so a firmware
    /// request for the host to grow the tiling parameter buffer would have sat
    /// unread in DRAM while the tiler waited for a reply that never came.
    ///
    /// `advance` publishes the consumer cursor. It defaults off at the call
    /// sites: publishing is a write to firmware-shared state on a ring whose
    /// consumer has never moved on this driver, and the question this answers
    /// -- is anything *there* -- does not require consuming it. Nothing here
    /// acts on a record; servicing a growth request is a separate change.
    pub(crate) fn drain_report_ring(
        &mut self,
        role: InstanceRole,
        advance: bool,
        print_budget: u32,
    ) -> Result<G17PReportDrainStats> {
        let status_a = match role {
            InstanceRole::Primary => self.handoff.primary.instance.status_a,
            InstanceRole::Secondary => self.handoff.secondary.instance.status_a,
        };
        let role_name = match role {
            InstanceRole::Primary => "primary",
            InstanceRole::Secondary => "secondary",
        };
        let state = status_a
            .checked_add(g17_completion::G17P_EVENT_STATE_STATUS_A_OFFSET)
            .ok_or(EINVAL)?;
        let ring = status_a
            .checked_add(g17_completion::G17P_EVENT_RING_STATUS_A_OFFSET)
            .ok_or(EINVAL)?;
        let consumer_address = state
            .checked_add(g17_completion::G17P_EVENT_CONSUMER_OFFSET)
            .ok_or(EINVAL)?;
        let producer_address = state
            .checked_add(g17_completion::G17P_EVENT_PRODUCER_OFFSET)
            .ok_or(EINVAL)?;
        let fwlog_write_address = status_a
            .checked_add(g17_completion::G17P_FWLOG_CONTROL_STATUS_A_OFFSET)
            .and_then(|address| address.checked_add(0x20))
            .ok_or(EINVAL)?;

        core::sync::atomic::fence(core::sync::atomic::Ordering::Acquire);
        let mut consumer = self.ktrace_object(role).read_u32(consumer_address)?;
        let producer = self.ktrace_object(role).read_u32(producer_address)?;
        let fwlog_write = self.ktrace_object(role).read_u32(fwlog_write_address)?;
        let mut stats = G17PReportDrainStats {
            producer,
            consumer_before: consumer,
            fwlog_write,
            ..G17PReportDrainStats::default()
        };
        if producer >= g17_completion::G17P_EVENT_CAPACITY
            || consumer >= g17_completion::G17P_EVENT_CAPACITY
        {
            stats.cursor_out_of_range = true;
            dev_warn!(
                self.dev().as_ref(),
                "G17P report[{}]: cursor out of range consumer={} producer={}\n",
                role_name,
                consumer,
                producer
            );
            return Ok(stats);
        }

        // The consumer is already level with the producer by the time any
        // render checkpoint runs -- the event worker services this ring during
        // the submit -- so a cursor-driven walk reports pending=0 and reads
        // nothing, and the records that actually described the render are
        // never seen. They are still in DRAM: dump the slots ending at the
        // producer regardless of the cursor, so a stalled render can be read
        // after the fact.
        {
            let capacity = g17_completion::G17P_EVENT_CAPACITY;
            let size = g17_completion::G17P_EVENT_ENTRY_SIZE;
            let history = if capacity < 8 { capacity } else { 8 };
            for back in (1..=history).rev() {
                let slot = (producer + capacity - back) & (capacity - 1);
                let record_address = ring
                    .checked_add((slot as usize * size) as u64)
                    .ok_or(EINVAL)?;
                let record_offset = record_address
                    .checked_sub(self.ktrace_object(role).iova())
                    .ok_or(EINVAL)? as usize;
                let record_end = record_offset.checked_add(size).ok_or(EINVAL)?;
                let mut raw = [0u8; g17_completion::G17P_EVENT_ENTRY_SIZE];
                if self
                    .ktrace_object(role)
                    .with_bytes_mut(|bytes| {
                        raw.copy_from_slice(bytes.get(record_offset..record_end).ok_or(EINVAL)?);
                        Ok(())
                    })
                    .is_err()
                {
                    continue;
                }
                if raw.iter().all(|byte| *byte == 0) {
                    continue;
                }
                dev_info!(
                    self.dev().as_ref(),
                    "G17P report[{}] history slot {} type={:#04x}: {:02x?}\n",
                    role_name,
                    slot,
                    raw[0],
                    &raw[..],
                );
            }
        }

        let mut budget = g17_completion::G17P_EVENT_CAPACITY;
        while consumer != producer && budget > 0 {
            budget -= 1;
            let entry_offset = consumer as usize * g17_completion::G17P_EVENT_ENTRY_SIZE;
            let published = (consumer + 1) & (g17_completion::G17P_EVENT_CAPACITY - 1);
            let record_address = ring.checked_add(entry_offset as u64).ok_or(EINVAL)?;
            let record_offset = record_address
                .checked_sub(self.ktrace_object(role).iova())
                .ok_or(EINVAL)? as usize;
            let record_end = record_offset
                .checked_add(g17_completion::G17P_EVENT_ENTRY_SIZE)
                .ok_or(EINVAL)?;
            let mut raw = [0u8; g17_completion::G17P_EVENT_ENTRY_SIZE];
            self.ktrace_object(role).with_bytes_mut(|bytes| {
                raw.copy_from_slice(bytes.get(record_offset..record_end).ok_or(EINVAL)?);
                Ok(())
            })?;

            if advance {
                core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
                self.ktrace_object(role)
                    .write_u32(consumer_address, published)?;
                core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
            }
            let slot = consumer;
            consumer = published;
            stats.consumed = stats.consumed.saturating_add(1);

            // Decode with the G17P-native firmware-event model, NOT the AGX2
            // `Channel13EventRecord` shape: this ring is the one
            // `read_primary_firmware_event` already services, so type 4 is
            // `kAGFIFirmwareEventTypeGPURestart` (generation at +0x04, stamp
            // index at +0x0c, -1 meaning "no stamp to attribute"), not a bare
            // timeout. Always dump the whole 0x48-byte record: the earlier
            // 0x40-byte model silently discarded the tail.
            let event_type = u32::from_le_bytes([raw[0], raw[1], raw[2], raw[3]]);
            if event_type == g17_completion::G17P_FIRMWARE_EVENT_RECOVERY_TYPE {
                stats.restart_requests = stats.restart_requests.saturating_add(1);
            }
            // Type 7 is the host-serviced grow request (the kext's type-7 arm
            // bounds-checks a vm index < 5 and a buffer index < 3 before
            // signalling). Counted so `grow-requests` keeps a live writer.
            if event_type == G17P_EVENT_GROW_REQUEST_TYPE {
                stats.grow_requests = stats.grow_requests.saturating_add(1);
            }
            if print_budget == 0 || stats.printed < print_budget {
                stats.printed = stats.printed.saturating_add(1);
                let qword = |offset: usize| {
                    let mut bytes = [0u8; 8];
                    bytes.copy_from_slice(&raw[offset..offset + 8]);
                    u64::from_le_bytes(bytes)
                };
                match g17_completion::decode_g17p_firmware_recovery_event(&raw) {
                    Ok(record) => dev_info!(
                        self.dev().as_ref(),
                        "G17P report[{}.{}]: type={} ({}) generation={:#x} stamp-index={}\n",
                        role_name,
                        slot,
                        event_type,
                        g17_completion::g17p_firmware_event_name(event_type),
                        record.generation,
                        record.stamp_index
                    ),
                    Err(_) => {
                        stats.skipped = stats.skipped.saturating_add(1);
                    }
                }
                dev_info!(
                    self.dev().as_ref(),
                    "G17P report[{}.{}]: type={} ({}) raw=[{:#x},{:#x},{:#x},{:#x},{:#x},{:#x},{:#x},{:#x},{:#x}]\n",
                    role_name,
                    slot,
                    event_type,
                    g17_completion::g17p_firmware_event_name(event_type),
                    qword(0x00),
                    qword(0x08),
                    qword(0x10),
                    qword(0x18),
                    qword(0x20),
                    qword(0x28),
                    qword(0x30),
                    qword(0x38),
                    qword(0x40)
                );
            }
        }
        Ok(stats)
    }

    /// Cross-check the published channel-14 KTrace pointer pair against the
    /// channel-table derivation. Cheap; meant to run once per boot so a
    /// mis-encode does not masquerade as "the firmware never traced".
    pub(crate) fn check_ktrace_ring_pointers(&mut self, role: InstanceRole) -> Result<(u64, u64)> {
        let entry = g17_initdata::channel_table_entry_range(
            g17_completion::G17P_KTRACE_CHANNEL_TABLE_INDEX,
        )
        .ok_or(EINVAL)?;
        let (main_config, status_a, state_grid) = match role {
            InstanceRole::Primary => (
                self.handoff.primary.instance.main_config,
                self.handoff.primary.instance.status_a,
                self.handoff.primary.state_grid,
            ),
            InstanceRole::Secondary => (
                self.handoff.secondary.instance.main_config,
                self.handoff.secondary.instance.status_a,
                self.handoff.secondary.state_grid,
            ),
        };
        let expected = g17_initdata::derive_channel_table(
            role,
            state_grid,
            status_a,
            main_config,
            self.handoff.shared.hw_data_bundle,
        )[g17_completion::G17P_KTRACE_CHANNEL_TABLE_INDEX];
        let base = main_config.checked_add(entry.start as u64).ok_or(EINVAL)?;
        let published_state = self.resources.shared_cluster.read_u64(base)?;
        let published_ring = self
            .resources
            .shared_cluster
            .read_u64(base.checked_add(8).ok_or(EINVAL)?)?;
        if published_state != expected.states[0] || published_ring != expected.states[1] {
            dev_err!(
                self.dev().as_ref(),
                "G17P ktrace: channel-14 pointer mismatch state={:#x}/{:#x} ring={:#x}/{:#x}\n",
                published_state,
                expected.states[0],
                published_ring,
                expected.states[1]
            );
            return Err(EINVAL);
        }
        Ok((published_state, published_ring))
    }

    /// Drain one instance's KTrace ring at `status_a + 0xa6ac0`.
    ///
    /// Every available record is consumed and the read pointer is published
    /// per record, exactly as `AGXFirmwareRingValidator::fetchNextEntry` does.
    /// That is not optional: when the ring fills the emitter busy-waits up to
    /// 0x11940 timebase ticks once and then latches a lost counter, and its
    /// EP-0x20 nudge is re-raised only while the consumer keeps moving. A
    /// drain that does not publish its read pointer is worse than no drain.
    ///
    /// Printing is filtered to the submission-path codes (plus the ungated
    /// records-lost marker) unless `verbose`, and capped at `print_budget`
    /// lines per call (0: unlimited). Records over budget are still consumed.
    pub(crate) fn drain_ktrace_ring(
        &mut self,
        role: InstanceRole,
        verbose: bool,
        print_budget: u32,
    ) -> Result<G17PKtraceDrainStats> {
        let status_a = match role {
            InstanceRole::Primary => self.handoff.primary.instance.status_a,
            InstanceRole::Secondary => self.handoff.secondary.instance.status_a,
        };
        let role_name = match role {
            InstanceRole::Primary => "primary",
            InstanceRole::Secondary => "secondary",
        };
        let state = status_a
            .checked_add(g17_completion::G17P_KTRACE_STATE_STATUS_A_OFFSET)
            .ok_or(EINVAL)?;
        let ring = status_a
            .checked_add(g17_completion::G17P_KTRACE_RING_STATUS_A_OFFSET)
            .ok_or(EINVAL)?;
        let consumer_address = state
            .checked_add(g17_completion::G17P_KTRACE_CONSUMER_OFFSET)
            .ok_or(EINVAL)?;
        let producer_address = state
            .checked_add(g17_completion::G17P_KTRACE_PRODUCER_OFFSET)
            .ok_or(EINVAL)?;

        core::sync::atomic::fence(core::sync::atomic::Ordering::Acquire);
        let mut consumer = self.ktrace_object(role).read_u32(consumer_address)?;
        let producer = self.ktrace_object(role).read_u32(producer_address)?;
        let mut stats = G17PKtraceDrainStats {
            producer,
            consumer_before: consumer,
            ..G17PKtraceDrainStats::default()
        };
        if producer >= g17_completion::G17P_KTRACE_CAPACITY
            || consumer >= g17_completion::G17P_KTRACE_CAPACITY
        {
            dev_err!(
                self.dev().as_ref(),
                "G17P ktrace[{}]: cursor out of range consumer={} producer={}\n",
                role_name,
                consumer,
                producer
            );
            return Err(EIO);
        }

        // Bound the walk by the ring capacity, never by the producer alone: a
        // corrupt pointer must not spin the submit path.
        let mut budget = g17_completion::G17P_KTRACE_CAPACITY;
        while consumer != producer && budget > 0 {
            budget -= 1;
            let (entry_offset, published) =
                match g17_completion::prepare_g17p_ktrace_read(consumer, producer) {
                    Ok(read) => read,
                    Err(g17_completion::CompletionDecodeError::FirmwareEventEmpty) => break,
                    Err(error) => {
                        dev_err!(
                            self.dev().as_ref(),
                            "G17P ktrace[{}]: cursor consumer={} producer={} rejected ({:?})\n",
                            role_name,
                            consumer,
                            producer,
                            error
                        );
                        return Err(EIO);
                    }
                };
            let record_address = ring.checked_add(entry_offset as u64).ok_or(EINVAL)?;
            let record_offset = record_address
                .checked_sub(self.ktrace_object(role).iova())
                .ok_or(EINVAL)? as usize;
            let record_end = record_offset
                .checked_add(g17_completion::G17P_KTRACE_ENTRY_SIZE)
                .ok_or(EINVAL)?;
            let mut raw = [0u8; g17_completion::G17P_KTRACE_ENTRY_SIZE];
            self.ktrace_object(role).with_bytes_mut(|bytes| {
                raw.copy_from_slice(bytes.get(record_offset..record_end).ok_or(EINVAL)?);
                Ok(())
            })?;

            // Publish before decoding or printing. dev_info! is slow and the
            // firmware watches this word to decide whether to nudge or stall.
            core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
            self.ktrace_object(role)
                .write_u32(consumer_address, published)?;
            core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
            consumer = published;
            stats.consumed = stats.consumed.saturating_add(1);

            let record = match g17_completion::decode_g17p_ktrace(&raw) {
                Ok(record) => record,
                Err(_) => {
                    stats.skipped = stats.skipped.saturating_add(1);
                    continue;
                }
            };
            let key = record.key();
            let lost_marker = key == g17_completion::G17P_KTRACE_LOST_KEY;
            if lost_marker {
                stats.lost_windows = stats.lost_windows.saturating_add(1);
                stats.lost_records = stats.lost_records.saturating_add(record.args[0]);
            }
            if key == g17_completion::G17P_KTRACE_ADD_KICKS_KEY {
                stats.saw_add_kicks = true;
            }
            if key == g17_completion::G17P_KTRACE_PAUSE_REASON_KEY {
                stats.last_pause_mask = record.args[2];
            }
            let wanted = verbose || g17_completion::g17p_ktrace_key_is_interesting(key);
            let within_budget =
                lost_marker || print_budget == 0 || stats.printed < print_budget;
            if wanted && within_budget {
                let name = g17_completion::g17p_ktrace_key_name(key);
                dev_info!(
                    self.dev().as_ref(),
                    "G17P ktrace[{}] t={:#x} ch={} code={:#04x} thr={} flag={} a=[{:#x},{:#x},{:#x},{:#x}] {}\n",
                    role_name,
                    record.timestamp,
                    record.channel,
                    record.code,
                    record.thread,
                    record.flag,
                    record.args[0],
                    record.args[1],
                    record.args[2],
                    record.args[3],
                    name
                );
                stats.printed = stats.printed.saturating_add(1);
            }
        }
        Ok(stats)
    }

    pub(crate) fn dump_primary_recovery_cause_block(&mut self) -> Result {
        let status_b = self.handoff.primary.instance.status_b;
        core::sync::atomic::fence(core::sync::atomic::Ordering::Acquire);
        for base in [0x4890u64, 0x48b0, 0x48d0, 0x48f0] {
            let mut words = [0u32; 8];
            for (i, word) in words.iter_mut().enumerate() {
                *word = self.resources.primary_state.read_u32(
                    status_b
                        .checked_add(base)
                        .and_then(|a| a.checked_add((i as u64).checked_mul(4)?))
                        .ok_or(EINVAL)?,
                )?;
            }
            dev_info!(
                self.dev().as_ref(),
                "G17P recovery raw +{:#06x}: {:#010x} {:#010x} {:#010x} {:#010x} {:#010x} {:#010x} {:#010x} {:#010x}\n",
                base, words[0], words[1], words[2], words[3],
                words[4], words[5], words[6], words[7]
            );
        }
        let mut entries = [0u32; 8];
        for (i, word) in entries.iter_mut().enumerate() {
            *word = self.resources.primary_state.read_u32(
                status_b
                    .checked_add(PRIMARY_STATUS_B_RECOVERY_INFO_OFFSET)
                    .and_then(|a| a.checked_add((i as u64).checked_mul(4)?))
                    .ok_or(EINVAL)?,
            )?;
        }
        dev_info!(
            self.dev().as_ref(),
            "G17P recovery info entries[0..2]: {:#010x} {:#010x} {:#010x} {:#010x} {:#010x} {:#010x} {:#010x} {:#010x}\n",
            entries[0], entries[1], entries[2], entries[3],
            entries[4], entries[5], entries[6], entries[7]
        );
        // The block carries two 64-bit firmware VAs at +0x48e0 and +0x48e8,
        // exactly 0x40 apart. Follow them: this is the most concrete lead on
        // what the firmware is actually asking for.
        for slot in [0x48e0u64, 0x48e8] {
            let target = self.resources.primary_state.read_u64(
                status_b.checked_add(slot).ok_or(EINVAL)?,
            )?;
            if target == 0 {
                continue;
            }
            let mut words = [0u32; 16];
            let mut ok = true;
            for (i, word) in words.iter_mut().enumerate() {
                match target
                    .checked_add((i as u64).checked_mul(4).ok_or(EINVAL)?)
                    .ok_or(EINVAL)
                    .and_then(|a| self.resources.primary_state.read_u32(a))
                {
                    Ok(value) => *word = value,
                    Err(_) => {
                        ok = false;
                        break;
                    }
                }
            }
            if !ok {
                dev_info!(
                    self.dev().as_ref(),
                    "G17P recovery ptr +{:#06x} = {:#x}: unreadable\n",
                    slot,
                    target
                );
                continue;
            }
            dev_info!(
                self.dev().as_ref(),
                "G17P recovery ptr +{:#06x} = {:#x}: {:#010x} {:#010x} {:#010x} {:#010x} {:#010x} {:#010x} {:#010x} {:#010x}\n",
                slot, target,
                words[0], words[1], words[2], words[3],
                words[4], words[5], words[6], words[7]
            );
            dev_info!(
                self.dev().as_ref(),
                "G17P recovery ptr +{:#06x} cont: {:#010x} {:#010x} {:#010x} {:#010x} {:#010x} {:#010x} {:#010x} {:#010x}\n",
                slot,
                words[8], words[9], words[10], words[11],
                words[12], words[13], words[14], words[15]
            );
        }
        Ok(())
    }

    /// Apply one native ArmFirmware recovery acknowledgement. Shipping macOS
    /// checks the current phase, executes `dmb ish`, writes the next phase,
    /// then executes a second `dmb ish`.
    pub(crate) fn acknowledge_primary_firmware_recovery(
        &mut self,
        expected: u32,
        next: u32,
    ) -> Result {
        if !matches!((expected, next), (1, 2) | (3, 0)) {
            return Err(EINVAL);
        }
        let state_address = self
            .handoff
            .primary
            .instance
            .status_b
            .checked_add(PRIMARY_STATUS_B_FIRMWARE_RECOVERY_STATE_OFFSET)
            .ok_or(EINVAL)?;
        core::sync::atomic::fence(core::sync::atomic::Ordering::Acquire);
        if self.resources.primary_state.read_u32(state_address)? != expected {
            return Err(EBUSY);
        }
        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
        self.resources
            .primary_state
            .write_u32(state_address, next)?;
        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
        Ok(())
    }

    pub(crate) fn control_opening_effect(&mut self) -> Result<G17PControlOpeningEffect> {
        core::sync::atomic::fence(core::sync::atomic::Ordering::Acquire);
        let shared_cursor_address = self
            .resources
            .partial_opening_control_shared
            .iova()
            .checked_add(g17_initdata::CONTROL_SHARED_CURSOR_OFFSET as u64)
            .ok_or(EINVAL)?;
        let inner_state_address = self.resources.partial_opening_control_shared_inner.iova();
        let shared_cursor = self
            .resources
            .partial_opening_control_shared
            .read_u32(shared_cursor_address)?;
        let inner_state = self
            .resources
            .partial_opening_control_shared_inner
            .read_u32(inner_state_address)?;
        let operand_slot = self.resources.control_operand_table.with_bytes_mut(|bytes| {
            let start = g17_initdata::PARTIAL_OPENING_CONTROL_OPERAND_SLOT_OFFSET as usize;
            let end = start.checked_add(8).ok_or(EINVAL)?;
            Ok(u64::from_le_bytes(
                bytes
                    .get(start..end)
                    .ok_or(EINVAL)?
                    .try_into()
                    .map_err(|_| EINVAL)?,
            ))
        })?;
        Ok(G17PControlOpeningEffect {
            shared_cursor,
            inner_state,
            operand_slot,
        })
    }

    fn validate_compute_readiness_objects(&self) -> Result {
        let primary_index_start = self.resources.partial_primary_index.iova();
        let primary_index_end = primary_index_start
            .checked_add(self.resources.partial_primary_index.logical_size as u64)
            .ok_or(EINVAL)?;
        let class3_end = g17_initdata::COMPUTE_READINESS_CLASS3_SUPPORT_ADDRESS
            .checked_add(g17_initdata::COMPUTE_READINESS_PAGE_SIZE as u64)
            .ok_or(EINVAL)?;
        if self.resources.compute_readiness_class1_support.iova()
            != g17_initdata::COMPUTE_READINESS_CLASS1_SUPPORT_ADDRESS
            || self.resources.compute_readiness_class1_state.iova()
                != g17_initdata::COMPUTE_READINESS_CLASS1_STATE_ADDRESS
            || self.resources.compute_readiness_class3_state.iova()
                != g17_initdata::COMPUTE_READINESS_CLASS3_STATE_ADDRESS
            || primary_index_start
                != g17_submission::G17P_PARTIAL_OPENING_PRIMARY_INDEX_FIRMWARE_GPU_VA
            || g17_initdata::COMPUTE_READINESS_CLASS3_SUPPORT_ADDRESS < primary_index_start
            || class3_end > primary_index_end
            || G17P_COMPUTE_READINESS_CLASS3_PRIMARY_INDEX_OFFSET
                % g17_initdata::COMPUTE_READINESS_PAGE_SIZE
                != 0
        {
            return Err(EINVAL);
        }
        Ok(())
    }

    /// Map the exact operand closure into context 1 before staging the suffix.
    /// The production caller is reachable only after
    /// `wait_compute_bootstrap_render_retirement()` has retired both inner
    /// queues and outer slots, dropped the retained render graph, and restored
    /// the kernel context. Preserve the primary-index contents until then.
    pub(crate) fn prepare_compute_readiness_context1(&mut self) -> Result {
        if !self.control_counters()?.opening_retired() {
            return Err(EINVAL);
        }
        self.validate_compute_readiness_objects()?;
        self.resources.control_operand_table.with_bytes_mut(|bytes| {
            g17_initdata::encode_compute_readiness_operand_table(
                g17p_control_operand_table_prefix(bytes).ok_or(EINVAL)?,
            )
            .map_err(|_| EINVAL)
        })?;
        let mut mapped_now = false;
        if self.resources.compute_readiness_context1.is_none() {
            let vm = self.address_space.uat.kernel_lower_vm();
            self.resources.compute_readiness_context1 = Some(map_compute_readiness_mappings(
                &self.dev,
                &mut self.resources.control_operand_table,
                &mut self.resources.control_operand_buffers,
                vm,
            )?);
            mapped_now = true;
        }
        let mappings = self
            .resources
            .compute_readiness_context1
            .as_ref()
            .ok_or(EINVAL)?;
        if !mappings.all_reachable_from(self.address_space.uat.kernel_lower_vm()) {
            return Err(EFAULT);
        }
        if mapped_now {
            dev_info!(
                self.dev.as_ref(),
                "G17P resources: readiness operand table plus entries 0..20 mapped after bootstrap retirement\n"
            );
        }
        if !self.resources.compute_readiness_class3_support_active {
            let start = G17P_COMPUTE_READINESS_CLASS3_PRIMARY_INDEX_OFFSET;
            let end = start
                .checked_add(g17_initdata::COMPUTE_READINESS_PAGE_SIZE)
                .ok_or(EINVAL)?;
            self.resources
                .partial_primary_index
                .with_bytes_mut(|bytes| {
                    g17_initdata::encode_compute_readiness_class3_support(
                        bytes.get_mut(start..end).ok_or(EINVAL)?,
                    )
                    .map_err(|_| EINVAL)
                })?;
            core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
            self.resources.compute_readiness_class3_support_active = true;
            dev_info!(
                self.dev.as_ref(),
                "G17P resources: retired primary-index page {:#x}+{:#x} now owns class-3 readiness support\n",
                self.resources.partial_primary_index.iova(),
                start,
            );
        }
        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
        Ok(())
    }

    /// Map the same output-positive operand closure into the first CL context.
    pub(crate) fn validate_compute_readiness_context2(&mut self, vm: &mmu::Vm) -> Result {
        if !self.control_counters()?.opening_retired() {
            return Err(EINVAL);
        }
        self.validate_compute_readiness_objects()?;
        if !self.resources.compute_readiness_class3_support_active {
            return Err(EINVAL);
        }
        self.resources.control_operand_table.with_bytes_mut(|bytes| {
            g17_initdata::encode_compute_readiness_operand_table(
                g17p_control_operand_table_prefix(bytes).ok_or(EINVAL)?,
            )
            .map_err(|_| EINVAL)
        })?;
        if !vm.covers_range(
            G17P_CONTROL_OPERAND_TABLE_VA,
            g17_initdata::CONTROL_OPERAND_TABLE_SIZE as u64,
            true,
            true,
        ) || !all_ranges_covered(
            (0..g17_initdata::COMPUTE_READINESS_OPERAND_ENTRY_COUNT).map(|index| {
                (
                    g17p_control_operand_buffer_va(index).unwrap_or(0),
                    G17P_CONTROL_OPERAND_BUFFER_SIZE as u64,
                )
            }),
            |address, size| vm.covers_range(address, size, true, true),
        ) {
            return Err(EFAULT);
        }
        Ok(())
    }

    /// Restore the source-built class-1 closure before publishing live
    /// compute-control records.
    pub(crate) fn prepare_compute_runtime_class1(&mut self) -> Result {
        if !self.control_counters()?.opening_retired() {
            return Err(EINVAL);
        }
        // The mature compute tail uses the 22-entry table. It replaces the
        // first-partial 28-entry render table only after that group retires.
        self.resources.control_operand_table.with_bytes_mut(|bytes| {
            g17_initdata::encode_control_operand_table(
                g17p_control_operand_table_prefix(bytes).ok_or(EINVAL)?,
            )
            .map_err(|_| EINVAL)
        })?;
        let mapping_state = self.resources.compute_runtime_state_mapping.is_some();
        let mapping_zero_0 = self
            .resources
            .compute_runtime_zero_buffer_0_mapping
            .is_some();
        let mapping_zero_1 = self
            .resources
            .compute_runtime_zero_buffer_1_mapping
            .is_some();
        let mapping_table = self.resources.compute_runtime_table_alias.is_some();
        let mapping_buffers = self.resources.compute_runtime_buffer_aliases.len();
        match (
            mapping_state,
            mapping_zero_0,
            mapping_zero_1,
            mapping_table,
            mapping_buffers,
        ) {
            (false, false, false, false, 0) => {
                let state = self.resources.compute_runtime_state.map_at(
                    self.address_space.uat.kernel_vm(),
                    g17_initdata::COMPUTE_RUNTIME_STATE_ADDRESS,
                    mmu::PROT_GPU_FW_SHARED_RW,
                )?;
                let zero_0 = self.resources.compute_runtime_zero_buffer_0.map_at(
                    self.address_space.uat.kernel_lower_vm(),
                    g17_initdata::COMPUTE_RUNTIME_ZERO_BUFFER_0_ADDRESS,
                    mmu::PROT_GPU_FW_SHARED_RW,
                )?;
                let zero_1 = self.resources.compute_runtime_zero_buffer_1.map_at(
                    self.address_space.uat.kernel_lower_vm(),
                    g17_initdata::COMPUTE_RUNTIME_ZERO_BUFFER_1_ADDRESS,
                    mmu::PROT_GPU_FW_SHARED_RW,
                )?;
                let table = self.resources.control_operand_table.map_at(
                    self.address_space.uat.kernel_lower_vm(),
                    g17_initdata::CONTROL_OPERAND_TABLE_ADDRESS,
                    mmu::PROT_GPU_FW_SHARED_RW,
                )?;
                let mut buffers = KVec::with_capacity(20, GFP_KERNEL)?;
                for (index, buffer) in self
                    .resources
                    .control_operand_buffers
                    .iter_mut()
                    .enumerate()
                {
                    if index > 18 && index != 22 {
                        continue;
                    }
                    let address = g17p_control_operand_buffer_va(index).ok_or(EINVAL)?;
                    buffers.push(
                        buffer.map_at(
                            self.address_space.uat.kernel_lower_vm(),
                            address,
                            mmu::PROT_GPU_FW_SHARED_RW,
                        )?,
                        GFP_KERNEL,
                    )?;
                }
                self.resources.compute_runtime_state_mapping = Some(state);
                self.resources.compute_runtime_zero_buffer_0_mapping = Some(zero_0);
                self.resources.compute_runtime_zero_buffer_1_mapping = Some(zero_1);
                self.resources.compute_runtime_table_alias = Some(table);
                self.resources.compute_runtime_buffer_aliases = buffers;
            }
            (true, true, true, true, 20) => {}
            _ => return Err(EIO),
        }
        if !self.address_space.uat.kernel_vm().covers_range(
            g17_initdata::COMPUTE_RUNTIME_STATE_ADDRESS,
            mmu::UAT_PGSZ as u64,
            true,
            true,
        ) || !self.address_space.uat.kernel_lower_vm().covers_range(
            g17_initdata::COMPUTE_RUNTIME_ZERO_BUFFER_0_ADDRESS,
            G17P_CONTROL_OPERAND_BUFFER_SIZE as u64,
            true,
            true,
        ) || !self.address_space.uat.kernel_lower_vm().covers_range(
            g17_initdata::COMPUTE_RUNTIME_ZERO_BUFFER_1_ADDRESS,
            G17P_CONTROL_OPERAND_BUFFER_SIZE as u64,
            true,
            true,
        ) || !self.address_space.uat.kernel_lower_vm().covers_range(
            g17_initdata::CONTROL_OPERAND_TABLE_ADDRESS,
            g17_initdata::CONTROL_OPERAND_TABLE_SIZE as u64,
            true,
            true,
        ) || !all_ranges_covered(
            self.resources
                .compute_runtime_buffer_aliases
                .iter()
                .map(|mapping| (mapping.iova(), mapping.size() as u64)),
            |address, size| {
                self.address_space
                    .uat
                    .kernel_lower_vm()
                    .covers_range(address, size, true, true)
            },
        ) {
            return Err(EIO);
        }
        self.resources.compute_runtime_support.with_bytes_mut(|bytes| {
            g17_initdata::encode_compute_runtime_class1_support(bytes).map_err(|_| EINVAL)
        })?;
        self.resources.compute_runtime_state.with_bytes_mut(|bytes| {
            bytes.fill(0);
            bytes[..4].copy_from_slice(&1u32.to_le_bytes());
            Ok(())
        })?;
        self.resources
            .compute_runtime_zero_buffer_0
            .with_bytes_mut(|bytes| {
                bytes.fill(0);
                Ok(())
            })?;
        self.resources
            .compute_runtime_zero_buffer_1
            .with_bytes_mut(|bytes| {
                bytes.fill(0);
                Ok(())
            })?;
        self.resources
            .compute_runtime_class1_table
            .with_bytes_mut(|bytes| {
                g17_initdata::encode_compute_runtime_class1_table(bytes).map_err(|_| EINVAL)
            })?;
        for (index, buffer) in self
            .resources
            .control_operand_buffers
            .iter_mut()
            .enumerate()
        {
            if index <= 18 {
                buffer.with_bytes_mut(|bytes| {
                    bytes.fill(0);
                    Ok(())
                })?;
            }
        }
        let page_list = self
            .resources
            .control_operand_buffers
            .iter_mut()
            .enumerate()
            .find(|(index, _)| *index == 22)
            .map(|(_, buffer)| buffer)
            .ok_or(EINVAL)?;
        page_list.with_bytes_mut(|bytes| {
            bytes.fill(0);
            g17_initdata::encode_compute_runtime_page_inventory(
                &mut bytes[..g17_initdata::COMPUTE_RUNTIME_PAGE_LIST_SIZE],
            )
            .map_err(|_| EINVAL)
        })?;
        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
        Ok(())
    }

    /// Store one first-CL readiness record and publish only its producer.
    /// The caller owns the matching control-done message and retirement wait.
    pub(crate) fn publish_compute_readiness_record(&mut self, index: usize) -> Result<u32> {
        if !self.resources.compute_readiness_class3_support_active {
            return Err(EINVAL);
        }
        let boundary = g17_initdata::compute_readiness_boundary(index).ok_or(EINVAL)?;
        let before = self.control_counters()?;
        if before.primary != [boundary.producer_before; 3]
            || before.secondary
                != [g17_initdata::CONTROL_OPENING_SECONDARY_PRODUCER; 3]
        {
            return Err(EINVAL);
        }

        let mut record = [0u8; g17_initdata::CONTROL_RECORD_SIZE];
        g17_initdata::encode_compute_readiness_record(index, &mut record)
            .map_err(|_| EINVAL)?;
        let channels = g17_initdata::derive_channel_table(
            InstanceRole::Primary,
            self.handoff.primary.state_grid,
            self.handoff.primary.instance.status_a,
            self.handoff.primary.instance.main_config,
            self.handoff.shared.hw_data_bundle,
        );
        let ring = channels[g17_initdata::CONTROL_CHANNEL_INDEX].ring;
        let record_address = ring
            .checked_add(
                boundary.producer_before as u64 * g17_initdata::CONTROL_RECORD_SIZE as u64,
            )
            .ok_or(EINVAL)?;
        let offset: usize = record_address
            .checked_sub(self.resources.shared_cluster.iova())
            .ok_or(EINVAL)?
            .try_into()?;
        let end = offset.checked_add(record.len()).ok_or(EINVAL)?;
        self.resources.shared_cluster.with_bytes_mut(|bytes| {
            let destination = bytes.get_mut(offset..end).ok_or(EINVAL)?;
            if !destination.iter().all(|byte| *byte == 0) {
                return Err(EINVAL);
            }
            destination.copy_from_slice(&record);
            Ok(())
        })?;
        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
        write_control_counter(
            &mut self.resources.primary_state,
            g17_initdata::CHANNEL_STATE_PRODUCER,
            boundary.producer_after,
        )?;
        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
        Ok(boundary.producer_after)
    }

    /// Append one bare device-control record to the primary channel-12 ring at
    /// whatever the producer happens to be, and advance the producer. Mailbox
    /// ownership stays with the live-boot layer: the caller must follow this
    /// with `encode_primary_device_control()` on EP 0x21, which is the message
    /// that raises event bit 17 and runs the drain.
    ///
    /// Unlike the staged opening/readiness publishers this takes no boundary
    /// constants: it is a runtime wire that may be sent any number of times
    /// after the opening has retired, so it reads the live cursor instead of
    /// asserting one.
    ///
    /// The cursor mapping, verified against `derive_channel_table`: channel
    /// table entry [`CONTROL_CHANNEL_INDEX`] (= 12) has
    /// `ring = main_config + MAIN_CONTROL_RING (0x4c0)` and three state words
    /// taken from `grid_states(12)`, i.e.
    /// `state_grid + WORK_STATE_GRID_OFFSETS[12] + n * CHANNEL_STATE_SPACING`.
    /// State word [`CHANNEL_STATE_PRODUCER`] (index 2) is the host-owned
    /// producer, and it lives in the state-grid object, **not** at
    /// `main_config + 0x1b0` — those main-configuration slots hold the
    /// *pointers* to these three words. `write_control_counter` writes the
    /// state-grid word; the pointers are never touched.
    pub(crate) fn publish_primary_device_control_record(
        &mut self,
        opcode: u32,
        arg: u32,
    ) -> Result<G17PDeviceControlPublication> {
        let mut record = [0u8; g17_initdata::CONTROL_RECORD_SIZE];
        g17_initdata::encode_device_control_record(opcode, arg, &mut record)
            .map_err(|_| EINVAL)?;
        self.publish_primary_device_control_raw(&record)
    }

    /// Append one complete native-width record to the primary device-control
    /// ring. Complex controls such as USC FList updateSubmitInfo (0x20) and
    /// its HardwareBufferID release (0x2e) use fields beyond the two-word
    /// form accepted by `encode_device_control_record`.
    pub(crate) fn publish_primary_device_control_raw(
        &mut self,
        record: &[u8; g17_initdata::CONTROL_RECORD_SIZE],
    ) -> Result<G17PDeviceControlPublication> {
        let opcode = u32::from_le_bytes([record[0], record[1], record[2], record[3]]);
        let arg = u32::from_le_bytes([record[4], record[5], record[6], record[7]]);
        if opcode == 0 {
            return Err(EINVAL);
        }
        let before = self.control_counters()?;
        let consumer_before = before.primary[g17_initdata::CHANNEL_STATE_CONSUMER];
        let producer_before = before.primary[g17_initdata::CHANNEL_STATE_PRODUCER];
        let slot = producer_before & g17_initdata::CONTROL_RING_INDEX_MASK;
        let channels = g17_initdata::derive_channel_table(
            InstanceRole::Primary,
            self.handoff.primary.state_grid,
            self.handoff.primary.instance.status_a,
            self.handoff.primary.instance.main_config,
            self.handoff.shared.hw_data_bundle,
        );
        let ring = channels[g17_initdata::CONTROL_CHANNEL_INDEX].ring;
        let record_address = ring
            .checked_add(slot as u64 * g17_initdata::CONTROL_RECORD_SIZE as u64)
            .ok_or(EINVAL)?;
        let offset: usize = record_address
            .checked_sub(self.resources.shared_cluster.iova())
            .ok_or(EINVAL)?
            .try_into()?;
        let end = offset
            .checked_add(g17_initdata::CONTROL_RECORD_SIZE)
            .ok_or(EINVAL)?;
        self.resources.shared_cluster.with_bytes_mut(|bytes| {
            bytes
                .get_mut(offset..end)
                .ok_or(EINVAL)?
                .copy_from_slice(record);
            Ok(())
        })?;
        // The record must be visible before the producer that publishes it.
        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
        let producer_after =
            producer_before.wrapping_add(1) & g17_initdata::CONTROL_RING_INDEX_MASK;
        write_control_counter(
            &mut self.resources.primary_state,
            g17_initdata::CHANNEL_STATE_PRODUCER,
            producer_after,
        )?;
        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
        Ok(G17PDeviceControlPublication {
            opcode,
            arg,
            slot,
            record_address,
            consumer_before,
            producer_before,
            producer_after,
        })
    }

    /// Store one live primary control record and publish only its producer.
    /// Mailbox ownership stays with the live-boot layer.
    pub(crate) fn publish_compute_runtime_control_record(
        &mut self,
        index: usize,
    ) -> Result<u32> {
        let before = self.control_counters()?;
        let index_u32 = u32::try_from(index).map_err(|_| EINVAL)?;
        let producer = g17_initdata::CONTROL_OPENING_PRIMARY_PRODUCER
            .checked_add(index_u32)
            .ok_or(EINVAL)?;
        if before.primary != [producer; 3]
            || before.secondary
                != [g17_initdata::CONTROL_OPENING_SECONDARY_PRODUCER; 3]
        {
            return Err(EINVAL);
        }
        let mut record = [0u8; g17_initdata::CONTROL_RECORD_SIZE];
        g17_initdata::encode_compute_runtime_control_record(index, &mut record)
            .map_err(|_| EINVAL)?;
        let channels = g17_initdata::derive_channel_table(
            InstanceRole::Primary,
            self.handoff.primary.state_grid,
            self.handoff.primary.instance.status_a,
            self.handoff.primary.instance.main_config,
            self.handoff.shared.hw_data_bundle,
        );
        let ring = channels[g17_initdata::CONTROL_CHANNEL_INDEX].ring;
        let record_address = ring
            .checked_add(producer as u64 * g17_initdata::CONTROL_RECORD_SIZE as u64)
            .ok_or(EINVAL)?;
        let offset: usize = record_address
            .checked_sub(self.resources.shared_cluster.iova())
            .ok_or(EINVAL)?
            .try_into()?;
        let end = offset.checked_add(record.len()).ok_or(EINVAL)?;
        self.resources.shared_cluster.with_bytes_mut(|bytes| {
            bytes
                .get_mut(offset..end)
                .ok_or(EINVAL)?
                .copy_from_slice(&record);
            Ok(())
        })?;
        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
        let target = producer.checked_add(1).ok_or(EINVAL)?;
        write_control_counter(
            &mut self.resources.primary_state,
            g17_initdata::CHANNEL_STATE_PRODUCER,
            target,
        )?;
        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
        Ok(target)
    }

    /// Rewrite the shared support object for the measured sequence-56
    /// class-2 registration.
    pub(crate) fn prepare_compute_runtime_class2(&mut self) -> Result {
        let expected = g17_initdata::CONTROL_OPENING_PRIMARY_PRODUCER
            + g17_initdata::COMPUTE_RUNTIME_CLASS2_RECORD_INDEX as u32;
        let counters = self.control_counters()?;
        if counters.primary != [expected; 3]
            || counters.secondary
                != [g17_initdata::CONTROL_OPENING_SECONDARY_PRODUCER; 3]
        {
            return Err(EINVAL);
        }
        self.resources.control_operand_table.with_bytes_mut(|bytes| {
            bytes.fill(0);
            Ok(())
        })?;
        self.resources.compute_runtime_support.with_bytes_mut(|bytes| {
            g17_initdata::encode_compute_runtime_class2_support(bytes).map_err(|_| EINVAL)
        })?;
        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
        Ok(())
    }

    pub(crate) fn rewrite_compute_dispatch_record(&mut self) -> Result {
        if !self.control_counters()?.compute_runtime_ready() {
            return Err(EINVAL);
        }
        self.resources.primary_b2_alias_1.with_bytes_mut(|bytes| {
            g17_initdata::encode_compute_dispatch_record(
                &mut bytes[0x20..0x20 + g17_initdata::COMPUTE_DISPATCH_RECORD_SIZE],
            )
            .map_err(|_| EINVAL)
        })?;
        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
        Ok(())
    }

    /// Reproduce `restartWorkQueue`'s empty-scheduler QID publication before
    /// firmware pre-recovery resume. Native walks all 128 queues in order and
    /// writes `{u32 valid, pad, u64 timestamp}`; every fresh queue contributes
    /// valid=0/timestamp=0, followed by one inner-shareable barrier.
    pub(crate) fn reset_empty_sksm_last_submitted_hw_timestamps(&mut self) -> Result {
        self.resources.shared_cluster.with_bytes_mut(|bytes| {
            let table = g17p_last_submitted_hw_timestamp_table_mut(bytes).ok_or(ERANGE)?;
            if !clear_g17p_last_submitted_hw_timestamps(table) {
                return Err(ERANGE);
            }
            Ok(())
        })?;
        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
        Ok(())
    }

    /// Read one QID's `{u32 valid, pad, u64 timestamp}` entry out of the same
    /// 128-entry table.
    ///
    /// Firmware reads the table from primary main-config `+0x274` while it
    /// rebuilds queue progress during restart. It is not the unrelated second
    /// high half of the main-config `+0x2d0` region-view alias pairs.
    pub(crate) fn g17p_last_submitted_hw_timestamp(&mut self, qid: usize) -> Result<(u32, u64)> {
        if qid >= G17P_LAST_SUBMITTED_HW_TIMESTAMP_COUNT {
            return Err(EINVAL);
        }
        self.resources.shared_cluster.with_bytes_mut(|bytes| {
            let bytes = g17p_last_submitted_hw_timestamp_table_mut(bytes).ok_or(ERANGE)?;
            let entry = qid * G17P_LAST_SUBMITTED_HW_TIMESTAMP_STRIDE;
            let end = entry
                .checked_add(G17P_LAST_SUBMITTED_HW_TIMESTAMP_STRIDE)
                .ok_or(ERANGE)?;
            let slot = bytes.get(entry..end).ok_or(ERANGE)?;
            let mut valid_bytes = [0u8; 4];
            valid_bytes.copy_from_slice(&slot[0..4]);
            let mut stamp_bytes = [0u8; 8];
            stamp_bytes.copy_from_slice(&slot[8..0x10]);
            Ok((
                u32::from_le_bytes(valid_bytes),
                u64::from_le_bytes(stamp_bytes),
            ))
        })
    }

    /// Rebuild firmware-owned session state after both processors stop.
    ///
    /// Every mapping and the UAT owner stay in place so existing DRM VMs keep
    /// their page-table identity and fixed driver aliases.
    pub(crate) fn reset_session_after_processor_stop(&mut self) -> Result {
        // The generic render status/descriptor aliases reuse these fixed VAs
        // on the next bootstrap. Both processors are already stopped here.
        self.resources.compute_runtime_state_mapping = None;
        self.resources.compute_runtime_zero_buffer_0_mapping = None;
        self.resources.compute_runtime_zero_buffer_1_mapping = None;
        self.resources.compute_runtime_table_alias = None;
        self.resources.compute_runtime_buffer_aliases.clear();
        self.resources.compute_readiness_context1 = None;
        self.resources.compute_readiness_class3_support_active = false;
        self.address_space
            .uat
            .restore_t8140_bootstrap_roots_after_processor_stop()?;

        self.resources.partial_scheduler.with_bytes_mut(|bytes| {
            g17_submission::apply_g17p_partial_opening_scheduler_page(bytes).map_err(|_| EINVAL)
        })?;
        self.resources
            .native_private_cluster
            .with_bytes_mut(|bytes| {
                bytes.fill(0);
                Ok(())
            })?;
        self.resources.partial_computed.with_bytes_mut(|bytes| {
            bytes.fill(0);
            Ok(())
        })?;
        self.resources
            .partial_primary_index
            .with_bytes_mut(|bytes| {
                bytes.fill(0);
                g17_submission::apply_g17p_partial_opening_primary_index_page(
                    &mut bytes[..g17_submission::G17P_PARTIAL_OPENING_PAGE_SIZE],
                )
                .map_err(|_| EINVAL)
            })?;
        self.resources.pb_descriptor_table.with_bytes_mut(|bytes| {
            bytes.fill(0);
            Ok(())
        })?;
        self.resources
            .uma_page_pool_descriptor_table
            .with_bytes_mut(|bytes| {
                bytes.fill(0);
                Ok(())
            })?;
        self.resources.parameter_metrics.with_bytes_mut(|bytes| {
            bytes.fill(0);
            Ok(())
        })?;
        self.resources.roots.with_bytes_mut(|bytes| {
            bytes.fill(0);
            Ok(())
        })?;
        self.resources.shared_cluster.with_bytes_mut(|bytes| {
            bytes.fill(0);
            Ok(())
        })?;
        self.resources.region_a.with_bytes_mut(|bytes| {
            bytes.fill(0);
            Ok(())
        })?;
        self.resources.region_c.with_bytes_mut(|bytes| {
            bytes.fill(0);
            g17_initdata::encode_region_c(bytes).map_err(|_| EINVAL)
        })?;
        self.resources.primary_state.with_bytes_mut(|bytes| {
            bytes.fill(0);
            Ok(())
        })?;
        self.resources.secondary_state.with_bytes_mut(|bytes| {
            bytes.fill(0);
            Ok(())
        })?;
        self.resources.secondary_status_a.with_bytes_mut(|bytes| {
            bytes.fill(0);
            Ok(())
        })?;
        self.resources.primary_b2_sentinel.with_bytes_mut(|bytes| {
            bytes.fill(0);
            Ok(())
        })?;
        self.resources.primary_b2_alias_1.with_bytes_mut(|bytes| {
            bytes.fill(0);
            g17_initdata::encode_compute_dispatch_record(
                &mut bytes[0x20..0x20 + g17_initdata::COMPUTE_DISPATCH_RECORD_SIZE],
            )
            .map_err(|_| EINVAL)
        })?;
        self.resources.completion_ordinal_0.with_bytes_mut(|bytes| {
            bytes.fill(0);
            Ok(())
        })?;
        self.resources.completion_ordinal_2.with_bytes_mut(|bytes| {
            bytes.fill(0);
            Ok(())
        })?;
        self.resources.fwctl.with_bytes_mut(|bytes| {
            bytes.fill(0);
            Ok(())
        })?;
        self.resources.control_shared.with_bytes_mut(|bytes| {
            g17_initdata::encode_control_shared(bytes).map_err(|_| EINVAL)
        })?;
        self.resources
            .control_shared_inner
            .with_bytes_mut(|bytes| {
                bytes.fill(0);
                bytes[..8]
                    .copy_from_slice(&g17_initdata::CONTROL_SHARED_INNER_PRESENTED.to_le_bytes());
                Ok(())
            })?;
        self.resources
            .partial_opening_control_shared
            .with_bytes_mut(|bytes| {
                g17_initdata::encode_partial_opening_control_shared(bytes).map_err(|_| EINVAL)
            })?;
        self.resources
            .partial_opening_control_shared_inner
            .with_bytes_mut(|bytes| {
                bytes.fill(0);
                bytes[..8].copy_from_slice(
                    &g17_initdata::PARTIAL_OPENING_CONTROL_SHARED_INNER_PRESENTED.to_le_bytes(),
                );
                Ok(())
            })?;
        self.resources
            .bootstrap_descriptor_zero_a
            .with_bytes_mut(|bytes| {
                bytes.fill(0);
                Ok(())
            })?;
        self.resources
            .bootstrap_descriptor_zero_b
            .with_bytes_mut(|bytes| {
                bytes.fill(0);
                Ok(())
            })?;
        self.resources
            .bootstrap_ta_context_peer
            .with_bytes_mut(|bytes| {
                bytes.fill(0);
                Ok(())
            })?;
        self.resources
            .bootstrap_3d_context_peer
            .with_bytes_mut(|bytes| {
                bytes.fill(0);
                Ok(())
            })?;
        self.resources
            .control_operand_page_lists
            .with_bytes_mut(|bytes| {
                bytes.fill(0);
                Ok(())
            })?;
        self.resources
            .control_operand_table
            .with_bytes_mut(|bytes| {
                bytes.fill(0);
                g17_initdata::encode_partial_opening_operand_table_pre_control(
                    &mut bytes[..g17_initdata::CONTROL_OPERAND_TABLE_SIZE],
                )
                .map_err(|_| EINVAL)
            })?;
        for buffer in self.resources.control_operand_buffers.iter_mut() {
            buffer.with_bytes_mut(|bytes| {
                bytes.fill(0);
                Ok(())
            })?;
        }
        self.resources.compute_runtime_support.with_bytes_mut(|bytes| {
            g17_initdata::encode_compute_runtime_class1_support(bytes).map_err(|_| EINVAL)
        })?;
        self.resources.compute_runtime_state.with_bytes_mut(|bytes| {
            bytes.fill(0);
            bytes[..4].copy_from_slice(&1u32.to_le_bytes());
            Ok(())
        })?;
        self.resources
            .compute_runtime_zero_buffer_0
            .with_bytes_mut(|bytes| {
                bytes.fill(0);
                Ok(())
            })?;
        self.resources
            .compute_runtime_zero_buffer_1
            .with_bytes_mut(|bytes| {
                bytes.fill(0);
                Ok(())
            })?;
        self.resources
            .compute_runtime_class1_table
            .with_bytes_mut(|bytes| {
                g17_initdata::encode_compute_runtime_class1_table(bytes).map_err(|_| EINVAL)
            })?;
        self.resources
            .compute_readiness_class1_support
            .with_bytes_mut(|bytes| {
                g17_initdata::encode_compute_readiness_class1_support(bytes)
                    .map_err(|_| EINVAL)
            })?;
        self.resources
            .compute_readiness_class1_state
            .with_bytes_mut(|bytes| {
                g17_initdata::encode_compute_readiness_state(bytes).map_err(|_| EINVAL)
            })?;
        self.resources
            .compute_readiness_class3_state
            .with_bytes_mut(|bytes| {
                g17_initdata::encode_compute_readiness_state(bytes).map_err(|_| EINVAL)
            })?;
        let page_list = self
            .resources
            .control_operand_buffers
            .iter_mut()
            .enumerate()
            .find(|(index, _)| *index == 22)
            .map(|(_, buffer)| buffer)
            .ok_or(EINVAL)?;
        page_list.with_bytes_mut(|bytes| {
            g17_initdata::encode_compute_runtime_page_inventory(
                &mut bytes[..g17_initdata::COMPUTE_RUNTIME_PAGE_LIST_SIZE],
            )
            .map_err(|_| EINVAL)
        })?;

        let shared = self.handoff.shared;
        let primary = self.handoff.primary;
        let secondary = self.handoff.secondary;
        let completion_aliases = [
            g17_initdata::G17PKsmCompletionAliases {
                low: self.resources.completion_ordinal_0_low.iova(),
                high: self.resources.completion_ordinal_0.iova(),
            },
            g17_initdata::G17PKsmCompletionAliases {
                low: self.resources.completion_ordinal_2_low.iova(),
                high: self.resources.completion_ordinal_2.iova(),
            },
        ];
        self.resources.shared_cluster.with_bytes_mut(|bytes| {
            g17_initdata::encode_hw_data(
                &g17_initdata::T8140_REGISTER_WINDOWS,
                &g17_initdata::T8140_REGISTER_FLAG_ONLY_SLOTS,
                completion_aliases,
                self.resources.qos_resource.iova(),
                self.resources.sksm_qid_resource.iova(),
                &mut bytes[..g17_initdata::HW_DATA_SIZE],
            )
            .map_err(|_| EINVAL)
        })?;
        write_status_block_at(
            &mut self.resources.primary_state,
            PRIMARY_STATUS_A_STATE_GRID_OFFSET as usize,
        )?;
        write_status_block_at(&mut self.resources.secondary_status_a, 0)?;
        let fwctl = self.resources.fwctl.iova();
        self.resources.primary_state.with_bytes_mut(|bytes| {
            let status_b = g17_initdata::PRIMARY_STATUS_B_STATE_GRID_OFFSET;
            let end = status_b + g17_initdata::PRIMARY_STATUS_B_OBJECT_SIZE;
            g17_initdata::encode_primary_status_b(
                fwctl,
                fwctl + g17_initdata::CONTROL_RECORD_SIZE as u64,
                &mut bytes[status_b..end],
            )
            .map_err(|_| EINVAL)
        })?;
        self.resources.secondary_state.with_bytes_mut(|bytes| {
            let extra = g17_initdata::PRIMARY_STATUS_B_STATE_GRID_OFFSET;
            g17_initdata::encode_secondary_root_extra_1(
                &mut bytes[extra..extra + g17_initdata::STATUS_BLOCK_SIZE],
            )
            .map_err(|_| EINVAL)
        })?;
        self.resources.shared_cluster.with_bytes_mut(|bytes| {
            g17_initdata::encode_bundle_static(
                secondary.state_grid + SECONDARY_HWDATA_STATE_OFFSET,
                bytes,
            )
            .map_err(|_| EINVAL)
        })?;
        let primary_views = g17_initdata::PrimaryRegionViews {
            sentinel_high: self.resources.primary_b2_sentinel.iova(),
            alias_high: [
                self.resources.partial_scheduler.iova(),
                self.resources.primary_b2_alias_1.iova(),
            ],
        };
        self.resources.shared_cluster.write_main_configs(
            &shared,
            &primary,
            &secondary,
            &primary_views,
        )?;
        self.resources
            .roots
            .write_roots(&shared, &primary.instance, &secondary.instance)?;
        write_control_counters(
            &mut self.resources.primary_state,
            [0, 0, g17_initdata::CONTROL_OPENING_PRIMARY_PRODUCER],
        )?;
        write_control_counters(
            &mut self.resources.secondary_state,
            [0, 0, g17_initdata::CONTROL_OPENING_SECONDARY_PRODUCER],
        )?;
        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
        Ok(())
    }

    pub(crate) fn stage_render_parameter_buffer_descriptor(
        &mut self,
        descriptor: Option<(u32, u64, u32)>,
    ) -> Result {
        let Some((index, page_list_address, page_count)) = descriptor else {
            return Ok(());
        };
        let offset = g17_submission::g17p_pbdesc_offset(index);
        let mut published = [0u32; 4];
        self.resources.pb_descriptor_table.with_bytes_mut(|bytes| {
            let end = offset.checked_add(g17_submission::G17P_PBDESC_ENTRY_SIZE).ok_or(EINVAL)?;
            if end > bytes.len() {
                return Err(EINVAL);
            }
            let mut previous = [0u32; 4];
            for (word, value) in previous.iter_mut().enumerate() {
                let at = offset + word * 4;
                *value = u32::from_le_bytes(bytes[at..at + 4].try_into().map_err(|_| EINVAL)?);
            }
            let encoded = g17_submission::encode_g17p_pbdesc(
                previous,
                page_list_address,
                page_count,
                g17_submission::G17PPbDescRingState::default(),
            );
            published = encoded;
            for (word, value) in encoded.iter().enumerate() {
                let at = offset + word * 4;
                bytes[at..at + 4].copy_from_slice(&value.to_le_bytes());
            }
            Ok(())
        })?;
        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
        dev_info!(
            self.dev().as_ref(),
            "G17P render PM: PBDesc index={} page-list-address={:#x} pages={} raw={:08x?} reconstructed={:#x} published-table=[{:#x},{:#x}]\n",
            index,
            page_list_address,
            page_count,
            published,
            g17_submission::g17p_pbdesc_reconstructed_address(page_list_address),
            self.resources.pb_descriptor_table_low.iova(),
            self.resources.pb_descriptor_table.iova(),
        );
        Ok(())
    }

    /// Snapshot the retained PBDesc entry and the two primary-main-config
    /// pointers through which B1 discovers that table.  All reads are from
    /// host-owned DRAM and remain safe at recovery/timeout checkpoints.
    pub(crate) fn log_render_parameter_buffer_descriptor(
        &mut self,
        label: &str,
        index: u32,
    ) {
        let offset = g17_submission::g17p_pbdesc_offset(index);
        let mut entry = [0u32; 4];
        let entry_read = self.resources.pb_descriptor_table.with_bytes_mut(|bytes| {
            for (word, value) in entry.iter_mut().enumerate() {
                let at = offset.checked_add(word * 4).ok_or(EOVERFLOW)?;
                *value = u32::from_le_bytes(
                    bytes
                        .get(at..at + 4)
                        .ok_or(ERANGE)?
                        .try_into()
                        .map_err(|_| ERANGE)?,
                );
            }
            Ok(())
        });

        let shared_base = self.resources.shared_cluster.iova();
        let main_offset: Result<usize> = self
            .handoff
            .primary
            .instance
            .main_config
            .checked_sub(shared_base)
            .ok_or(ERANGE)
            .and_then(|value| value.try_into().map_err(|_| ERANGE));
        let mut main_views = [0u64; 2];
        let main_read = main_offset.and_then(|main_offset| {
            self.resources.shared_cluster.with_bytes_mut(|bytes| {
                for (slot, relative) in [0x2d8usize, 0x2e0].into_iter().enumerate() {
                    let at = main_offset.checked_add(relative).ok_or(EOVERFLOW)?;
                    main_views[slot] = u64::from_le_bytes(
                        bytes
                            .get(at..at + 8)
                            .ok_or(ERANGE)?
                            .try_into()
                            .map_err(|_| ERANGE)?,
                    );
                }
                Ok(())
            })
        });
        core::sync::atomic::fence(core::sync::atomic::Ordering::Acquire);

        match (entry_read, main_read) {
            (Ok(()), Ok(())) => dev_info!(
                self.dev().as_ref(),
                "G17P render PBDesc[{}]: index={} entry={:08x?} main-config(+2d8,+2e0)=[{:#x},{:#x}] expected-table=[{:#x},{:#x}]\n",
                label,
                index,
                entry,
                main_views[0],
                main_views[1],
                self.resources.pb_descriptor_table_low.iova(),
                self.resources.pb_descriptor_table.iova(),
            ),
            (entry_result, main_result) => dev_warn!(
                self.dev().as_ref(),
                "G17P render PBDesc[{}]: snapshot failed entry={:?} main-config={:?}\n",
                label,
                entry_result.err(),
                main_result.err(),
            ),
        }
    }

    pub(crate) fn parameter_buffer_page_list_vas(&self) -> [u64; 2] {
        [
            self.resources.partial_primary_index.iova(),
            self.resources.partial_primary_index_low.iova(),
        ]
    }

    pub(crate) fn stage_render_parameter_buffer_page_list(
        &mut self,
        storage: &mut G17PUserRenderStorage,
    ) -> Result {
        let pm = storage.parameter_management.as_mut().ok_or(EINVAL)?;
        pm.copy_page_list_to(&mut self.resources.partial_primary_index)?;
        let mut first = [0u32; 8];
        self.resources.partial_primary_index.with_bytes_mut(|bytes| {
            for (index, value) in first.iter_mut().enumerate() {
                let offset = index * 4;
                *value = u32::from_le_bytes(
                    bytes
                        .get(offset..offset + 4)
                        .ok_or(ERANGE)?
                        .try_into()
                        .map_err(|_| EINVAL)?,
                );
            }
            Ok(())
        })?;
        dev_info!(
            self.dev().as_ref(),
            "G17P render PM: staged native PB page-list aliases=[{:#x},{:#x}] first={:x?}\n",
            self.resources.partial_primary_index.iova(),
            g17_submission::G17P_PARTIAL_OPENING_PRIMARY_INDEX_GPU_VA,
            first,
        );
        Ok(())
    }

    /// Publish the Linux-owned USC private-memory FList into the global UMA
    /// descriptor table exactly as `AGXAcceleratorG16::halLoadUMADesc` does.
    pub(crate) fn stage_render_uma_page_pool_descriptor(&mut self) -> Result {
        let hardware_buffer_id =
            g17_initdata::PARTIAL_OPENING_FREELIST_HARDWARE_BUFFER_ID;
        let offset = g17_submission::g17p_umadesc_offset(hardware_buffer_id).ok_or(EINVAL)?;
        let mut state = g17_submission::G17PUmaPagePoolState::default();
        self.resources
            .partial_opening_control_shared
            .with_bytes_mut(|bytes| {
                let read_u32 = |at: usize| -> Result<u32> {
                    Ok(u32::from_le_bytes(
                        bytes.get(at..at + 4).ok_or(EINVAL)?.try_into().map_err(|_| EINVAL)?,
                    ))
                };
                let read_u64 = |at: usize| -> Result<u64> {
                    Ok(u64::from_le_bytes(
                        bytes.get(at..at + 8).ok_or(EINVAL)?.try_into().map_err(|_| EINVAL)?,
                    ))
                };
                state = g17_submission::G17PUmaPagePoolState {
                    page_list: read_u64(0x14)?,
                    unit_pages: read_u32(0x1c)?,
                    counter_a: read_u32(0x20)?,
                    counter_b: read_u32(0x24)?,
                    empty: read_u32(0x28)?,
                    total_pages: read_u32(0x2c)?,
                };
                Ok(())
            })?;
        let mut encoded = [0u64; 4];
        self.resources
            .uma_page_pool_descriptor_table
            .with_bytes_mut(|bytes| {
                let end = offset
                    .checked_add(g17_submission::G17P_UMA_DESC_ENTRY_SIZE)
                    .ok_or(EINVAL)?;
                if end > bytes.len() {
                    return Err(EINVAL);
                }
                let mut previous = [0u64; 4];
                for (word, value) in previous.iter_mut().enumerate() {
                    let at = offset + word * 8;
                    *value = u64::from_le_bytes(
                        bytes[at..at + 8].try_into().map_err(|_| EINVAL)?,
                    );
                }
                encoded = g17_submission::encode_g17p_umadesc(previous, state);
                for (word, value) in encoded.iter().enumerate() {
                    let at = offset + word * 8;
                    bytes[at..at + 8].copy_from_slice(&value.to_le_bytes());
                }
                Ok(())
            })?;
        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
        dev_info!(
            self.dev().as_ref(),
            "G17P render UMA: ID={} state=[{:#x},{:#x},{:#x},{:#x},{:#x},{:#x}] descriptor=[{:#x},{:#x},{:#x},{:#x}] table=[{:#x},{:#x}]\n",
            hardware_buffer_id,
            state.page_list,
            state.unit_pages,
            state.counter_a,
            state.counter_b,
            state.empty,
            state.total_pages,
            encoded[0],
            encoded[1],
            encoded[2],
            encoded[3],
            self.resources.uma_page_pool_descriptor_table_low.iova(),
            self.resources.uma_page_pool_descriptor_table.iova(),
        );
        Ok(())
    }

    /// Diagnose the firmware mutation expected after a successful opcode-0x20
    /// grow. Native appends four 1-MiB blocks after the 0x1600 initial pages,
    /// so page-list entry 0x1600 must become 0x70018d0000 and advance by 4 KiB.
    pub(crate) fn log_render_uma_growth(&mut self, checkpoint: &str) {
        let mut state = [0u32; 7];
        let state_result = self
            .resources
            .partial_opening_control_shared
            .with_bytes_mut(|bytes| {
                for (index, offset) in [0x20usize, 0x24, 0x28, 0x2c, 0x40, 0x44, 0x48]
                    .into_iter()
                    .enumerate()
                {
                    state[index] = u32::from_le_bytes(
                        bytes.get(offset..offset + 4).ok_or(EINVAL)?
                            .try_into()
                            .map_err(|_| EINVAL)?,
                    );
                }
                Ok(())
            });
        let mut appended = [0u64; 4];
        let page_result = self.resources.control_operand_page_lists.with_bytes_mut(|bytes| {
            let start = 0x1600usize * 8;
            for (index, value) in appended.iter_mut().enumerate() {
                let offset = start + index * 8;
                *value = u64::from_le_bytes(
                    bytes.get(offset..offset + 8).ok_or(EINVAL)?
                        .try_into()
                        .map_err(|_| EINVAL)?,
                );
            }
            Ok(())
        });
        match (state_result, page_result) {
            (Ok(()), Ok(())) => dev_info!(
                self.dev().as_ref(),
                "G17P render UMA growth[{}]: state(+20,+24,+28,+2c,+40,+44,+48)={:x?} page-list[0x1600..]={:x?}\n",
                checkpoint,
                state,
                appended,
            ),
            (state_error, page_error) => dev_warn!(
                self.dev().as_ref(),
                "G17P render UMA growth[{}]: snapshot failed state={:?} pages={:?}\n",
                checkpoint,
                state_error,
                page_error,
            ),
        }
    }

    pub(crate) fn new_user_render_storage(
        &mut self,
        dev: &AsahiDevice,
        vm: &mmu::Vm,
        command: &g17_uapi::TranslatedRenderCommand,
        num_clusters: u32,
        user_timestamps: [u64; 4],
        user_timestamp_aliases: KVec<mmu::KernelMapping>,
    ) -> Result<G17PUserRenderStorage> {
        let parameter_metrics_vas = [
            self.resources.parameter_metrics.iova(),
            self.resources.parameter_metrics_low_va,
        ];
        let command = *command;
        let owner_pid = u32::try_from(kernel::current!().group_leader().pid())
            .map_err(|_| EOVERFLOW)?;
        dev_info!(
            dev.as_ref(),
            "G17P render channel owner: process pid={}\n",
            owner_pid
        );
        let render_operand_aliases = self.stage_user_render_flist(vm)?;
        self.stage_render_uma_page_pool_descriptor()?;
        let parameter_page_list_vas = self.parameter_buffer_page_list_vas();
        let render_bind = self.address_space.uat.bind(vm)?;
        let command_context = self
            .address_space
            .uat
            .allocate_t8140_app_context(&render_bind)?;
        dev_info!(
            dev.as_ref(),
            "G17P render app-GART allocation: shared TA/3D command context {} (vm {})\n",
            command_context.context_id(),
            render_bind.vm_id(),
        );
        let mut storage = G17PUserRenderStorage::new(
            dev,
            &self.address_space.uat,
            parameter_page_list_vas,
            parameter_metrics_vas,
            &mut self.resources.bootstrap_ta_context_peer,
            &mut self.resources.bootstrap_3d_context_peer,
            vm,
            &command,
            num_clusters,
            owner_pid,
            user_timestamps,
            user_timestamp_aliases,
            command_context,
        )?;
        storage.render_operand_aliases = render_operand_aliases;
        self.stage_render_parameter_buffer_page_list(&mut storage)?;
        self.stage_render_parameter_buffer_descriptor(storage.parameter_buffer_descriptor()?)?;
        Ok(storage)
    }

    /// Upgrade an ordinary compute-capable client VM to the complete native
    /// render FList aperture when its first TA/3D submission arrives.
    ///
    /// Blocks 21..25 cannot live in the device-global context-0 root: block 21
    /// overlaps the fixed KSM completion aliases.  Native resolves the render
    /// pool through the client translation context, so map the tail only in
    /// that client VM and retain it with the render graph.  The whole aperture
    /// is reserved from userspace at VM creation, making this late mapping
    /// deterministic and race-free.
    fn stage_user_render_flist(
        &mut self,
        vm: &mmu::Vm,
    ) -> Result<KVec<mmu::KernelMapping>> {
        if !all_ranges_covered(
            (0..g17_initdata::COMPUTE_READINESS_OPERAND_ENTRY_COUNT).map(|index| {
                (
                    g17p_control_operand_buffer_va(index).unwrap_or(0),
                    G17P_CONTROL_OPERAND_BUFFER_SIZE as u64,
                )
            }),
            |address, size| vm.covers_range(address, size, true, true),
        ) {
            return Err(EFAULT);
        }

        let tail_count = G17P_CONTROL_OPERAND_ACTIVE_BLOCK_COUNT
            .checked_sub(g17_initdata::COMPUTE_READINESS_OPERAND_ENTRY_COUNT)
            .ok_or(EINVAL)?;
        let mut aliases = KVec::with_capacity(tail_count, GFP_KERNEL)?;
        for index in g17_initdata::COMPUTE_READINESS_OPERAND_ENTRY_COUNT
            ..G17P_CONTROL_OPERAND_ACTIVE_BLOCK_COUNT
        {
            let address = g17p_control_operand_buffer_va(index).ok_or(EINVAL)?;
            // The driver-owned bootstrap render VM starts with all 28 blocks.
            // Ordinary Vulkan VMs start with 21 and enter this arm for the
            // five render-only aliases.
            if vm.covers_range(
                address,
                G17P_CONTROL_OPERAND_BUFFER_SIZE as u64,
                true,
                true,
            ) {
                continue;
            }
            aliases.push(
                self.resources.control_operand_buffers[index].map_at(
                    vm,
                    address,
                    mmu::PROT_GPU_SHARED_RW,
                )?,
                GFP_KERNEL,
            )?;
        }
        self.stage_render_flist_state()?;

        if !all_ranges_covered(
            (0..G17P_CONTROL_OPERAND_ACTIVE_BLOCK_COUNT).map(|index| {
                (
                    g17p_control_operand_buffer_va(index).unwrap_or(0),
                    G17P_CONTROL_OPERAND_BUFFER_SIZE as u64,
                )
            }),
            |address, size| vm.covers_range(address, size, true, true),
        ) {
            return Err(EFAULT);
        }
        dev_info!(
            self.dev().as_ref(),
            "G17P resources: client VM upgraded to render FList ({} initial blocks + {} first-grow blocks)\n",
            G17P_CONTROL_OPERAND_INITIAL_BLOCK_COUNT,
            G17P_CONTROL_OPERAND_FIRST_GROW_BLOCK_COUNT,
        );
        Ok(aliases)
    }

    /// Restore the shared render shape on every render submission.  Compute
    /// intentionally reuses these backings with a compact 21-run table and an
    /// empty page list, so staging only when a retained render graph is first
    /// allocated is not sufficient for compute -> render transitions.
    pub(crate) fn stage_render_flist_state(&mut self) -> Result {
        self.resources
            .control_operand_page_lists
            .with_bytes_mut(|bytes| {
                if encode_g17p_initial_flist_page_list(bytes) {
                    Ok(())
                } else {
                    Err(EINVAL)
                }
            })?;
        self.resources.control_operand_table.with_bytes_mut(|bytes| {
            if encode_g17p_pre_qid_flist_runs(bytes) {
                Ok(())
            } else {
                Err(EINVAL)
            }
        })?;
        self.resources
            .partial_opening_control_shared
            .with_bytes_mut(|bytes| {
                g17_initdata::encode_partial_opening_control_shared(bytes)
                    .map_err(|_| EINVAL)
            })?;
        self.resources
            .partial_opening_control_shared_inner
            .with_bytes_mut(|bytes| {
                bytes.fill(0);
                bytes[..8].copy_from_slice(
                    &g17_initdata::PARTIAL_OPENING_CONTROL_SHARED_INNER_PRESENTED
                        .to_le_bytes(),
                );
                Ok(())
            })?;
        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
        Ok(())
    }

    /// Install every hardware-fixed low alias before the VM is exposed to
    /// userspace and retain it for that VM's complete lifetime.
    /// The low (client-half) GPU address of one render queue's SKSM entry
    /// region.
    ///
    /// These are the per-QID VAs the render tag-15 ConfigUpdate publishes at
    /// `+0x08`, backed by `bootstrap_{ta,3d}_context_peer`. Metal's live queue
    /// objects prove both aliases follow `base + qid * 0x28000`; Linux maps
    /// only the two kernel-owned application queues into those slots.
    pub(crate) const fn render_sksm_entry_low_va(tiling: bool) -> u64 {
        if tiling {
            G17P_BOOTSTRAP_TA_CONTEXT_LOW_VA
        } else {
            G17P_BOOTSTRAP_3D_CONTEXT_LOW_VA
        }
    }

    /// The low (client-half) GPU address of one render command descriptor.
    /// Compute derives its CL entry's two RCE bindings from the same alias.
    pub(crate) const fn render_descriptor_low_va(tiling: bool) -> u64 {
        if tiling {
            G17P_BOOTSTRAP_TA_DESCRIPTOR_LOW_VA
        } else {
            G17P_BOOTSTRAP_3D_DESCRIPTOR_LOW_VA
        }
    }

    /// The firmware-half alias of the same region (render tag-15 `+0x10`).
    pub(crate) const fn render_sksm_entry_high_va(tiling: bool) -> u64 {
        if tiling {
            G17P_BOOTSTRAP_TA_CONTEXT_HIGH_VA
        } else {
            G17P_BOOTSTRAP_3D_CONTEXT_HIGH_VA
        }
    }

    /// Publish one KSM kick entry into a render queue's SKSM entry region.
    ///
    /// Byte-for-byte the same transaction as
    /// `G17PClRuntimeStorage::write_entry` on the compute side: clear the
    /// whole stride, then copy the encoded body over its head.
    /// Read back the head of a render queue's SKSM entry region.
    ///
    /// This is HOST memory we wrote the kick entry into, so anything the
    /// firmware changes here is direct evidence it touched the entry -- as
    /// opposed to `0x47`, which only reports whether the scheduler was idling.
    /// DRAM only; safe at any point including a timeout.
    /// Machine-parseable dump of a whole SKSM kick entry, for byte-diffing
    /// against the native capture. Lines are `G17PDUMP <who> <off> <bytes>`.
    pub(crate) fn dump_render_sksm_entry(&mut self, tiling: bool, label: &str) {
        let object = if tiling {
            &mut self.resources.bootstrap_ta_context_peer
        } else {
            &mut self.resources.bootstrap_3d_context_peer
        };
        let mut body = [0u8; 0x200];
        let mut ok = false;
        if let Ok(vmap) = object.object.vmap() {
            let bytes = unsafe {
                // SAFETY: the VMap covers the page-rounded object and every
                // index below is bounds-checked against logical_size.
                core::slice::from_raw_parts(vmap.as_ptr(), object.logical_size)
            };
            if bytes.len() >= 0x400 {
                body.copy_from_slice(&bytes[0x200..0x400]);
                ok = true;
            }
        }
        if !ok {
            return;
        }
        let dev = self.dev().clone();
        for row in 0..(0x200 / 16) {
            let at = row * 16;
            dev_info!(
                dev.as_ref(),
                "G17PDUMP entry-{} {} {:#05x} {:02x?}\n",
                if tiling { "TA" } else { "3D" },
                label,
                at,
                &body[at..at + 16],
            );
        }
    }

    /// DIAGNOSTIC. Clear the firmware-owned parent link in the fragment's
    /// kick entry, the way b1 `FUN_162f8` does from the tiling job's state 7.
    ///
    /// `FUN_162f8(sku+0x79c, sku+0x7a8)` indexes the entry as
    /// `ring_base(qid) + (index & 0xff) * 0x200` and, among other things,
    /// does `entry[0x20] &= ~1` -- clearing dependency slot 0, which is the
    /// link the firmware itself owns and releases when tiling completes. Our
    /// tiling SKU carries the right values there (+0x79c = 6, +0x7a8 = 1), but
    /// the state machine that would call it never runs, so the entry stays
    /// parented and no strobe can start it.
    ///
    /// Returns the dependency word before and after.
    pub(crate) fn g17p_release_fragment_parent(&mut self, index: usize) -> Result<(u32, u32)> {
        let object = &mut self.resources.bootstrap_3d_context_peer;
        let at = index
            .checked_mul(0x200)
            .and_then(|base| base.checked_add(0x20))
            .ok_or(EINVAL)?;
        let mut before = 0u32;
        let mut after = 0u32;
        object.with_bytes_mut(|bytes| {
            let word = bytes.get_mut(at..at + 4).ok_or(ERANGE)?;
            before = u32::from_le_bytes([word[0], word[1], word[2], word[3]]);
            after = before & !1;
            word.copy_from_slice(&after.to_le_bytes());
            Ok(())
        })?;
        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
        Ok((before, after))
    }

    pub(crate) fn log_render_sksm_entry(&mut self, tiling: bool, label: &str, offset: usize) {
        let object = if tiling {
            &mut self.resources.bootstrap_ta_context_peer
        } else {
            &mut self.resources.bootstrap_3d_context_peer
        };
        let mut words = [0u32; 24];
        let mut ok = false;
        if let Ok(vmap) = object.object.vmap() {
            let bytes = unsafe {
                // SAFETY: the VMap covers the page-rounded object; logical_size
                // is the mapped extent, and every index below is bounds-checked.
                core::slice::from_raw_parts(vmap.as_ptr(), object.logical_size)
            };
            for (index, slot) in words.iter_mut().enumerate() {
                let at = offset + index * 4;
                if at + 4 <= bytes.len() {
                    *slot =
                        u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]]);
                    ok = true;
                }
            }
        }
        let dev = self.dev().clone();
        if ok {
            dev_info!(
                dev.as_ref(),
                "G17P sksm entry[{}] {} +{:#x}: {:#010x} {:#010x} {:#010x} {:#010x} {:#010x} {:#010x} {:#010x} {:#010x} {:#010x} {:#010x} {:#010x} {:#010x}\n",
                label,
                if tiling { "TA" } else { "3D" },
                offset,
                words[0], words[1], words[2], words[3], words[4], words[5],
                words[6], words[7], words[8], words[9], words[10], words[11],
            );
            dev_info!(
                dev.as_ref(),
                "G17P sksm entry[{}] {} +{:#x}: {:#010x} {:#010x} {:#010x} {:#010x} {:#010x} {:#010x} {:#010x} {:#010x} {:#010x} {:#010x} {:#010x} {:#010x}\n",
                label,
                if tiling { "TA" } else { "3D" },
                offset + 0x30,
                words[12], words[13], words[14], words[15], words[16], words[17],
                words[18], words[19], words[20], words[21], words[22], words[23],
            );
        } else {
            dev_err!(
                dev.as_ref(),
                "G17P sksm entry[{}] {} read failed\n",
                label,
                if tiling { "TA" } else { "3D" }
            );
        }
    }

    pub(crate) fn write_render_sksm_entry(
        &mut self,
        tiling: bool,
        offset: usize,
        zero_length: usize,
        body: &[u8],
    ) -> Result {
        let object = if tiling {
            &mut self.resources.bootstrap_ta_context_peer
        } else {
            &mut self.resources.bootstrap_3d_context_peer
        };
        let end = offset.checked_add(zero_length).ok_or(EINVAL)?;
        if zero_length == 0
            || offset % zero_length != 0
            || end > G17P_BOOTSTRAP_CONTEXT_PEER_SIZE
            || end > object.logical_size
            || body.len() > zero_length
        {
            return Err(EINVAL);
        }
        let vmap = object.object.vmap()?;
        let bytes = unsafe {
            // SAFETY: the VMap covers the complete page-rounded object and
            // logical_size is the requested mapped extent.
            core::slice::from_raw_parts_mut(vmap.as_mut_ptr(), object.logical_size)
        };
        bytes[offset..end].fill(0);
        bytes[offset..offset + body.len()].copy_from_slice(body);
        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
        Ok(())
    }

    pub(crate) fn install_user_vm_aliases(
        &mut self,
        vm: &mmu::Vm,
        bootstrap_vdm: bool,
    ) -> Result {
        // The VDM bootstrap root owns the complete captured aperture. An
        // ordinary client root must stop at operand block 20: block 21 spans
        // 0x70017c8000..0x70018c8000 and therefore overlaps both native SKSM
        // completion aliases at 0x7001848000/0x7001870000. The render pool's
        // 22 initial plus four grown blocks need a render-specific translation
        // context; installing them in every client root makes the first
        // compute bind fail with ENOSPC before any hardware publication.
        let operand_buffer_count = if bootstrap_vdm {
            G17P_CONTROL_OPERAND_BUFFER_COUNT
        } else {
            g17_initdata::COMPUTE_READINESS_OPERAND_ENTRY_COUNT
        };
        let mut mappings = KVec::with_capacity(
            operand_buffer_count + if bootstrap_vdm { 6 } else { 4 },
            GFP_KERNEL,
        )?;
        let mut reserved_ranges = KVec::new();
        // Native's SecureGart owns the first low view of a process's fixed
        // resources (VA 0..0x10000); the driver materializes it in this root
        // while the VM owns a render graph. Reserve it for the VM's whole
        // lifetime so a user bind can never take the window between graphs.
        reserved_ranges.push(
            mmu::T8140_CLIENT_NULL_VIEW_START
                ..mmu::T8140_CLIENT_NULL_VIEW_START + mmu::T8140_CLIENT_NULL_VIEW_SIZE as u64,
            GFP_KERNEL,
        )?;
        reserved_ranges.push(G17P_CL_LOW_VA_START..G17P_CL_LOW_VA_END, GFP_KERNEL)?;
        // Keep only the low-half render aperture driver-owned. Honeykrisp's VDM
        // command BO aliases descend from the top of this 4 GiB window, so
        // reserving the entire tail would reject valid userspace aliases such
        // as 0x10ffed0000 before a render could even be submitted.
        reserved_ranges.push(G17P_RENDER_USER_ALIAS_START..G17P_RENDER_USER_TVB_END, GFP_KERNEL)?;
        reserved_ranges.push(G17P_RENDER_AUX_ALIAS_START..G17P_RENDER_AUX_ALIAS_END, GFP_KERNEL)?;
        reserved_ranges.push(
            self.resources.parameter_metrics_low_va
                ..self.resources.parameter_metrics_low_va
                    + mmu::T8140_PARAMETER_METRICS_SIZE as u64,
            GFP_KERNEL,
        )?;
        // Both the bootstrap and userspace render roots must see the retained
        // initial UMA page list. Mapping a throwaway zero object here made the
        // halLoad descriptor name valid memory containing no page addresses.
        mappings.push(
            match self.resources.control_operand_page_lists.map_at(
                vm,
                G17P_CONTROL_DIRECTORY_VA,
                mmu::PROT_GPU_SHARED_RW,
            ) {
                Ok(mapping) => mapping,
                Err(error) => {
                    dev_err!(
                        self.dev.as_ref(),
                        "G17P VM: retained UMA page-list alias at {:#x}+{:#x} failed ({:?})\n",
                        G17P_CONTROL_DIRECTORY_VA,
                        G17P_CONTROL_DIRECTORY_SIZE,
                        error
                    );
                    return Err(error);
                }
            },
            GFP_KERNEL,
        )?;
        if bootstrap_vdm {
            mappings.push(
                map_fresh_zero_object_at(
                    &self.dev,
                    vm,
                    G17P_BOOTSTRAP_CONTEXT_BASE,
                    G17P_BOOTSTRAP_CONTEXT_SIZE,
                )?,
                GFP_KERNEL,
            )?;
            mappings.push(
                map_bootstrap_vdm_at(&self.dev, vm)?,
                GFP_KERNEL,
            )?;
        }
        if !bootstrap_vdm {
            self.resources
                .partial_opening_control_shared
                .with_bytes_mut(|bytes| {
                    g17_initdata::encode_compute_partial_opening_control_shared(bytes)
                        .map_err(|_| EINVAL)
                })?;
            self.resources
                .partial_opening_control_shared_inner
                .with_bytes_mut(|bytes| {
                    bytes.fill(0);
                    Ok(())
                })?;
            self.resources
                .control_operand_page_lists
                .with_bytes_mut(|bytes| {
                    // The proven compute opening starts with no populated UMA
                    // page list. Render publishes its prepared pool separately.
                    bytes.fill(0);
                    Ok(())
                })?;
            self.resources.control_operand_table.with_bytes_mut(|bytes| {
                if !encode_g17p_compute_flist_runs(bytes) {
                    return Err(EINVAL);
                }
                Ok(())
            })?;
            core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
        }
        let table_mapping = match self.resources.control_operand_table.map_at(
            vm,
            G17P_CONTROL_OPERAND_TABLE_VA,
            mmu::PROT_GPU_SHARED_RW,
        ) {
            Ok(mapping) => mapping,
            Err(error) => {
                dev_err!(
                    self.dev.as_ref(),
                    "G17P resources: render operand-table alias at {:#x} failed ({:?})\n",
                    G17P_CONTROL_OPERAND_TABLE_VA,
                    error
                );
                return Err(error);
            }
        };
        mappings.push(table_mapping, GFP_KERNEL)?;
        if !bootstrap_vdm {
            dev_info!(
                self.dev.as_ref(),
                "G17P VM: prepared FList visible before QID config main={:#x}+{:#x} runs={:#x}+{:#x} descriptor={:#x} aux={:#x} padded-count={:#x}\n",
                G17P_CONTROL_DIRECTORY_VA,
                G17P_CONTROL_OPERAND_PAGE_LIST_SIZE,
                G17P_CONTROL_OPERAND_TABLE_VA,
                G17P_CONTROL_OPERAND_TABLE_BACKING_SIZE,
                g17_initdata::PARTIAL_OPENING_CONTROL_SHARED_ADDRESS,
                g17_initdata::PARTIAL_OPENING_CONTROL_SHARED_INNER_ADDRESS,
                g17_initdata::COMPUTE_PARTIAL_OPENING_CONTROL_SHARED_CURSOR_BEFORE
            );
        }
        for (index, buffer) in self
            .resources
            .control_operand_buffers
            .iter_mut()
            .take(operand_buffer_count)
            .enumerate()
        {
            let address = g17p_control_operand_buffer_va(index).ok_or(EINVAL)?;
            let mapping = match buffer.map_at(vm, address, mmu::PROT_GPU_SHARED_RW) {
                Ok(mapping) => mapping,
                Err(error) => {
                    dev_err!(
                        self.dev.as_ref(),
                        "G17P resources: render operand buffer {} alias at {:#x} failed ({:?})\n",
                        index,
                        address,
                        error
                    );
                    return Err(error);
                }
            };
            mappings.push(mapping, GFP_KERNEL)?;
        }
        dev_info!(
            self.dev.as_ref(),
            "G17P VM: {} operand buffers mapped GPU-shared; entry18={:#x}\n",
            operand_buffer_count,
            g17p_control_operand_buffer_va(18).ok_or(EINVAL)?
                | g17_initdata::CONTROL_OPERAND_ENTRY_FLAG,
        );
        mappings.push(
            match self.resources.partial_primary_index.map_alias_at(
                vm,
                g17_submission::G17P_PARTIAL_OPENING_PRIMARY_INDEX_GPU_VA,
                mmu::PROT_GPU_FW_SHARED_RW,
            ) {
                Ok(mapping) => mapping,
                Err(error) => {
                    dev_err!(
                        self.dev.as_ref(),
                        "G17P VM: primary-index alias failed ({:?})\n",
                        error
                    );
                    return Err(error);
                }
            },
            GFP_KERNEL,
        )?;
        mappings.push(
            match self.resources.parameter_metrics.map_alias_at(
                vm,
                self.resources.parameter_metrics_low_va,
                mmu::PROT_GPU_FW_SHARED_RW,
            ) {
                Ok(mapping) => mapping,
                Err(error) => {
                    dev_err!(
                        self.dev.as_ref(),
                        "G17P VM: PM page-metrics alias at {:#x}+{:#x} failed ({:?})\n",
                        self.resources.parameter_metrics_low_va,
                        mmu::T8140_PARAMETER_METRICS_SIZE,
                        error
                    );
                    return Err(error);
                }
            },
            GFP_KERNEL,
        )?;
        vm.install_driver_mappings(mappings, reserved_ranges)
            .inspect_err(|error| {
                dev_err!(
                    self.dev.as_ref(),
                    "G17P VM: fixed-alias retention failed ({:?})\n",
                    error
                );
            })
    }

    pub(crate) fn partial_opening_binding(&self) -> G17PPartialOpeningResourceBinding {
        let channels = g17_initdata::derive_channel_table(
            InstanceRole::Primary,
            self.handoff.primary.state_grid,
            self.handoff.primary.instance.status_a,
            self.handoff.primary.instance.main_config,
            self.handoff.shared.hw_data_bundle,
        );
        G17PPartialOpeningResourceBinding {
            kernel_va_base: self.kernel_va_base,
            scheduler_page: self.resources.partial_scheduler.iova(),
            primary_index_page: g17_submission::G17P_PARTIAL_OPENING_PRIMARY_INDEX_GPU_VA,
            fwctl: self.resources.fwctl.iova(),
            tiling_channel: channels
                [g17_submission::G17P_PARTIAL_OPENING_TILING_CHANNEL_TABLE_INDEX as usize],
            fragment_channel: channels
                [g17_submission::G17P_PARTIAL_OPENING_FRAGMENT_CHANNEL_TABLE_INDEX as usize],
        }
    }

    /// Publish one prepared queue on the primary CL_2 work channel. The queue
    /// item ring and write pointer are already visible when this is called;
    /// the outer slot is therefore stored before the channel producer.
    pub(crate) fn publish_compute_channel(
        &mut self,
        queue_gpu_va: u64,
        queue_write_index: u32,
        queue_id: u8,
    ) -> Result<g17_submission::PreparedG17PComputeChannelWork> {
        let channels = g17_initdata::derive_channel_table(
            InstanceRole::Primary,
            self.handoff.primary.state_grid,
            self.handoff.primary.instance.status_a,
            self.handoff.primary.instance.main_config,
            self.handoff.shared.hw_data_bundle,
        );
        let channel = channels[g17_submission::G17P_COMPUTE_CHANNEL_TABLE_INDEX as usize];
        let state = g17_submission::G17PPartialOpeningOuterChannelState {
            consumers: [
                self.resources.primary_state.read_u32(channel.states[0])?,
                self.resources.primary_state.read_u32(channel.states[1])?,
            ],
            producer: self.resources.primary_state.read_u32(
                channel.states[g17_initdata::CHANNEL_STATE_PRODUCER],
            )?,
            queue_gpu_va,
            queue_write_index,
        };
        let prepared = g17_submission::prepare_g17p_compute_channel_work(state, queue_id)
            .map_err(|_| EINVAL)?;
        let slot_gpu_va = channel
            .ring
            .checked_add(prepared.outer.slot_byte_offset as u64)
            .ok_or(EINVAL)?;
        let slot_offset: usize = slot_gpu_va
            .checked_sub(self.resources.shared_cluster.iova())
            .ok_or(EINVAL)?
            .try_into()?;
        let producer_gpu_va = channel.states[g17_initdata::CHANNEL_STATE_PRODUCER];
        let producer_offset: usize = producer_gpu_va
            .checked_sub(self.resources.primary_state.iova())
            .ok_or(EINVAL)?
            .try_into()?;
        if slot_offset
            .checked_add(prepared.outer.slot.len())
            .ok_or(EINVAL)?
            > self.resources.shared_cluster.logical_size
            || producer_offset.checked_add(4).ok_or(EINVAL)?
                > self.resources.primary_state.logical_size
        {
            return Err(EINVAL);
        }

        let ring_vmap = self.resources.shared_cluster.object.vmap()?;
        let state_vmap = self.resources.primary_state.object.vmap()?;
        let ring = unsafe {
            // SAFETY: the retained VMap covers the complete shared cluster.
            core::slice::from_raw_parts_mut(
                ring_vmap.as_mut_ptr(),
                self.resources.shared_cluster.logical_size,
            )
        };
        let state = unsafe {
            // SAFETY: the retained VMap covers the complete primary state.
            core::slice::from_raw_parts_mut(
                state_vmap.as_mut_ptr(),
                self.resources.primary_state.logical_size,
            )
        };
        ring[slot_offset..slot_offset + prepared.outer.slot.len()]
            .copy_from_slice(&prepared.outer.slot);
        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
        state[producer_offset..producer_offset + 4]
            .copy_from_slice(&prepared.outer.next_producer.to_le_bytes());
        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
        Ok(prepared)
    }

    pub(crate) fn snapshot_compute_channel(&mut self) -> Result<G17PComputeChannelSnapshot> {
        let channels = g17_initdata::derive_channel_table(
            InstanceRole::Primary,
            self.handoff.primary.state_grid,
            self.handoff.primary.instance.status_a,
            self.handoff.primary.instance.main_config,
            self.handoff.shared.hw_data_bundle,
        );
        let table_index = g17_submission::G17P_COMPUTE_CHANNEL_TABLE_INDEX;
        let channel = channels[table_index as usize];
        core::sync::atomic::fence(core::sync::atomic::Ordering::Acquire);
        let cursors = [
            self.resources.primary_state.read_u32(channel.states[0])?,
            self.resources.primary_state.read_u32(channel.states[1])?,
            self.resources.primary_state.read_u32(channel.states[2])?,
        ];
        let slot_index = cursors[2].wrapping_sub(1) as u8;
        let slot_gpu_va = channel
            .ring
            .checked_add(slot_index as u64 * 0x18)
            .ok_or(EINVAL)?;
        let shared_cluster_iova = self.resources.shared_cluster.iova();
        let shared_cluster_size = self.resources.shared_cluster.logical_size;
        let slot_offset: usize = slot_gpu_va
            .checked_sub(shared_cluster_iova)
            .ok_or(EINVAL)?
            .try_into()?;
        if slot_offset.checked_add(0x18).ok_or(EINVAL)? > shared_cluster_size {
            return Err(ERANGE);
        }
        let main_config_offset: usize = self
            .handoff
            .primary
            .instance
            .main_config
            .checked_sub(shared_cluster_iova)
            .ok_or(EINVAL)?
            .try_into()?;
        let ring_vmap = self.resources.shared_cluster.object.vmap()?;
        let ring = unsafe {
            // SAFETY: the checked slot lies inside the retained shared VMap.
            core::slice::from_raw_parts(ring_vmap.as_mut_ptr(), shared_cluster_size)
        };
        let read_u32 = |offset: usize| -> Result<u32> {
            Ok(u32::from_le_bytes(
                ring[offset..offset + 4]
                    .try_into()
                    .map_err(|_| ERANGE)?,
            ))
        };
        let read_u64 = |offset: usize| -> Result<u64> {
            Ok(u64::from_le_bytes(
                ring[offset..offset + 8]
                    .try_into()
                    .map_err(|_| ERANGE)?,
            ))
        };
        let snapshot = G17PComputeChannelSnapshot {
            table_index,
            ring: channel.ring,
            state_addresses: channel.states,
            cursors,
            slot_index,
            slot_queue: read_u64(slot_offset + 0x08)?,
            slot_kind: read_u32(slot_offset + 0x10)?,
            slot_flags: read_u32(slot_offset + 0x14)?,
            runtime_descriptor_pointers: [
                read_u64(main_config_offset + 0x120)?,
                read_u64(main_config_offset + 0x130)?,
            ],
        };
        core::sync::atomic::fence(core::sync::atomic::Ordering::Acquire);
        Ok(snapshot)
    }

    pub(crate) fn snapshot_all_primary_work_channels(
        &mut self,
    ) -> Result<[G17PChannelScanSnapshot; G17P_DATA_MASTER_CHANNEL_COUNT]> {
        const EMPTY: G17PChannelScanSnapshot = G17PChannelScanSnapshot {
            table_index: 0,
            ring: 0,
            state_addresses: [0; 3],
            cursors: [0; 3],
            current_slot_index: 0,
            current_slot_queue: 0,
            current_slot_kind: 0,
            current_slot_queue_id: 0,
        };
        let channels = g17_initdata::derive_channel_table(
            InstanceRole::Primary,
            self.handoff.primary.state_grid,
            self.handoff.primary.instance.status_a,
            self.handoff.primary.instance.main_config,
            self.handoff.shared.hw_data_bundle,
        );
        let shared_cluster_iova = self.resources.shared_cluster.iova();
        let shared_cluster_size = self.resources.shared_cluster.logical_size;
        let ring_vmap = self.resources.shared_cluster.object.vmap()?;
        let ring = unsafe {
            // SAFETY: every derived primary work ring is inside this retained
            // native shared-cluster mapping and each slot is bounds checked.
            core::slice::from_raw_parts(ring_vmap.as_mut_ptr(), shared_cluster_size)
        };
        let mut snapshots = [EMPTY; G17P_DATA_MASTER_CHANNEL_COUNT];
        core::sync::atomic::fence(core::sync::atomic::Ordering::Acquire);
        for (index, channel) in channels
            .into_iter()
            .take(G17P_DATA_MASTER_CHANNEL_COUNT)
            .enumerate()
        {
            let cursors = [
                self.resources.primary_state.read_u32(channel.states[0])?,
                self.resources.primary_state.read_u32(channel.states[1])?,
                self.resources.primary_state.read_u32(channel.states[2])?,
            ];
            let current_slot_index = cursors[0] as u8;
            let slot_gpu_va = channel
                .ring
                .checked_add(current_slot_index as u64 * 0x18)
                .ok_or(EINVAL)?;
            let offset: usize = slot_gpu_va
                .checked_sub(shared_cluster_iova)
                .ok_or(EINVAL)?
                .try_into()?;
            let end = offset.checked_add(0x18).ok_or(EINVAL)?;
            let slot = ring.get(offset..end).ok_or(ERANGE)?;
            snapshots[index] = G17PChannelScanSnapshot {
                table_index: index as u8,
                ring: channel.ring,
                state_addresses: channel.states,
                cursors,
                current_slot_index,
                current_slot_queue: u64::from_le_bytes(
                    slot[0x08..0x10].try_into().map_err(|_| ERANGE)?,
                ),
                current_slot_kind: u32::from_le_bytes(
                    slot[0x10..0x14].try_into().map_err(|_| ERANGE)?,
                ),
                current_slot_queue_id: slot[0x16],
            };
        }
        core::sync::atomic::fence(core::sync::atomic::Ordering::Acquire);
        Ok(snapshots)
    }

    /// Present the measured five status-B fields at the runtime boundary just
    /// before the primary control-done message.
    pub(crate) fn apply_partial_opening_pre_control_status(&mut self) -> Result {
        let writes =
            g17_submission::prepare_g17p_partial_opening_pre_0x84_status(self.kernel_va_base)
                .map_err(|_| EINVAL)?;
        let status_b = g17_initdata::PRIMARY_STATUS_B_STATE_GRID_OFFSET;
        self.resources.primary_state.with_bytes_mut(|bytes| {
            for write in writes {
                let offset = status_b.checked_add(write.offset as usize).ok_or(EINVAL)?;
                let end = offset.checked_add(8).ok_or(EINVAL)?;
                if end > bytes.len() {
                    return Err(EINVAL);
                }
                bytes[offset..end].copy_from_slice(&write.value.to_le_bytes());
            }
            core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
            Ok(())
        })
    }

    /// Publish the 28-entry render operand table after primary control-done
    /// and before either first-work producer becomes visible.
    pub(crate) fn publish_partial_opening_operand_table(&mut self) -> Result {
        if !self.control_counters()?.opening_retired() {
            return Err(EINVAL);
        }
        self.resources.control_operand_table.with_bytes_mut(|bytes| {
            g17_initdata::encode_partial_opening_operand_table_post_control(
                g17p_control_operand_table_prefix(bytes).ok_or(EINVAL)?,
            )
            .map_err(|_| EINVAL)?;
            core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
            Ok(())
        })
    }

    /// The source bootstrap fence owns the group only after both inner queues
    /// and both outer channel consumers cover the published prefix.
    pub(crate) fn partial_opening_outer_counters(&mut self) -> Result<[[u32; 3]; 2]> {
        let binding = self.partial_opening_binding();
        let mut counters = [[0; 3]; 2];
        for (output, channel) in counters
            .iter_mut()
            .zip([binding.fragment_channel, binding.tiling_channel])
        {
            *output = [
                self.resources.primary_state.read_u32(channel.states[0])?,
                self.resources.primary_state.read_u32(channel.states[1])?,
                self.resources.primary_state.read_u32(channel.states[2])?,
            ];
        }
        Ok(counters)
    }

    pub(crate) fn partial_opening_outer_retired(&mut self) -> Result<bool> {
        Ok(self
            .partial_opening_outer_counters()?
            .into_iter()
            .all(|counters| counters[2] == 1 && counters[..2] == [counters[2]; 2]))
    }

    /// Publish the first fragment/tiling outer-ring pair in the measured
    /// order. Queue records and their item rings must already contain the
    /// three-entry groups named by the supplied queue heads.
    pub(crate) fn publish_partial_opening_pair<F>(
        &mut self,
        render_vm: Option<&mmu::Vm>,
        context_already_bound: bool,
        // This layer may retain exactly one OUTER producer. The manager owns
        // the matching whole-group SKSM/item publication and later completes
        // the selected channel without ever rewinding a live producer.
        deferred_outer: G17PDeferredOuterPublication,
        fragment_queue_gpu_va: u64,
        fragment_queue_write_index: u32,
        tiling_queue_gpu_va: u64,
        tiling_queue_write_index: u32,
        publish_late_tiling_state: F,
    ) -> Result<(
        g17_submission::PreparedG17PPartialOpeningWorkPair,
        Option<mmu::VmBind>,
    )>
    where
        F: FnOnce(),
    {
        if fragment_queue_gpu_va == 0
            || tiling_queue_gpu_va == 0
            || (render_vm.is_some() && context_already_bound)
        {
            return Err(EINVAL);
        }
        let binding = self.partial_opening_binding();
        let fragment = g17_submission::G17PPartialOpeningOuterChannelState {
            consumers: [
                self.resources
                    .primary_state
                    .read_u32(binding.fragment_channel.states[0])?,
                self.resources
                    .primary_state
                    .read_u32(binding.fragment_channel.states[1])?,
            ],
            producer: self
                .resources
                .primary_state
                .read_u32(binding.fragment_channel.states[2])?,
            queue_gpu_va: fragment_queue_gpu_va,
            queue_write_index: fragment_queue_write_index,
        };
        let tiling = g17_submission::G17PPartialOpeningOuterChannelState {
            consumers: [
                self.resources
                    .primary_state
                    .read_u32(binding.tiling_channel.states[0])?,
                self.resources
                    .primary_state
                    .read_u32(binding.tiling_channel.states[1])?,
            ],
            producer: self
                .resources
                .primary_state
                .read_u32(binding.tiling_channel.states[2])?,
            queue_gpu_va: tiling_queue_gpu_va,
            queue_write_index: tiling_queue_write_index,
        };
        // The outer slot's queue-id byte MUST agree with the QID published in
        // tag-15/tag-14/SKSM, or the firmware is told to dispatch to a queue
        // that was never configured.
        let prepared = g17_submission::prepare_g17p_partial_opening_work_pair(
            fragment,
            tiling,
            g17p_render_queue_id(false) as u8,
            g17p_render_queue_id(true) as u8,
        )
            .map_err(|_| EINVAL)?;
        if prepared.fragment.channel_table_index
            != g17_submission::G17P_PARTIAL_OPENING_FRAGMENT_CHANNEL_TABLE_INDEX
            || prepared.tiling.channel_table_index
                != g17_submission::G17P_PARTIAL_OPENING_TILING_CHANNEL_TABLE_INDEX
        {
            return Err(EINVAL);
        }

        let fragment_slot = binding
            .fragment_channel
            .ring
            .checked_add(prepared.fragment.slot_byte_offset as u64)
            .ok_or(EINVAL)?;
        let tiling_slot = binding
            .tiling_channel
            .ring
            .checked_add(prepared.tiling.slot_byte_offset as u64)
            .ok_or(EINVAL)?;
        let fragment_producer =
            binding.fragment_channel.states[g17_initdata::CHANNEL_STATE_PRODUCER];
        let tiling_producer = binding.tiling_channel.states[g17_initdata::CHANNEL_STATE_PRODUCER];
        let mapped_offset = |object: &MappedObject, address: u64, size: usize| -> Result<usize> {
            let offset: usize = address
                .checked_sub(object.iova())
                .ok_or(EINVAL)?
                .try_into()?;
            let end = offset.checked_add(size).ok_or(EINVAL)?;
            if end > object.logical_size {
                return Err(EINVAL);
            }
            Ok(offset)
        };
        let fragment_slot_offset = mapped_offset(
            &self.resources.shared_cluster,
            fragment_slot,
            prepared.fragment.slot.len(),
        )?;
        let tiling_slot_offset = mapped_offset(
            &self.resources.shared_cluster,
            tiling_slot,
            prepared.tiling.slot.len(),
        )?;
        let fragment_producer_offset = mapped_offset(
            &self.resources.primary_state,
            fragment_producer,
            core::mem::size_of::<u32>(),
        )?;
        let tiling_producer_offset = mapped_offset(
            &self.resources.primary_state,
            tiling_producer,
            core::mem::size_of::<u32>(),
        )?;

        // Acquire every fallible CPU mapping and validate every destination
        // before making the first producer visible.
        let shared_vmap = self.resources.shared_cluster.object.vmap()?;
        let state_vmap = self.resources.primary_state.object.vmap()?;
        let shared = unsafe {
            // SAFETY: the VMap covers this object's complete allocation.
            core::slice::from_raw_parts_mut(
                shared_vmap.as_mut_ptr(),
                self.resources.shared_cluster.logical_size,
            )
        };
        let state = unsafe {
            // SAFETY: the VMap covers this object's complete allocation.
            core::slice::from_raw_parts_mut(
                state_vmap.as_mut_ptr(),
                self.resources.primary_state.logical_size,
            )
        };

        shared[fragment_slot_offset..fragment_slot_offset + prepared.fragment.slot.len()]
            .copy_from_slice(&prepared.fragment.slot);
        let render_bind = if let Some(vm) = render_vm {
            let bind = self.address_space.uat.bind(vm)?;
            // G17PUserRenderStorage owns two native-style dynamic app-GART
            // leases already rooted against this binding. Do not overwrite a
            // fixed context here: descriptor and tag-15 IDs come from those
            // leases, exactly as AGXContextIDManager allocates them.
            let mcache_hwsid = *crate::module_parameters::g17p_render_mcache_hwsid.value();
            let mcache_selector =
                *crate::module_parameters::g17p_render_mcache_selector.value() as usize;
            if mcache_hwsid != 0 && matches!(mcache_selector, 2 | 3) {
                self.address_space
                    .uat
                    .install_t8140_render_mcache_context_alias(&bind, mcache_selector)?;
            }
            Some(bind)
        } else if context_already_bound {
            None
        } else {
            self.address_space
                .uat
                .install_t8140_partial_opening_empty_high_roots()?;
            None
        };
        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
        if deferred_outer != G17PDeferredOuterPublication::Fragment {
            state[fragment_producer_offset..fragment_producer_offset + 4]
                .copy_from_slice(&prepared.fragment.next_producer.to_le_bytes());
        }

        publish_late_tiling_state();
        shared[tiling_slot_offset..tiling_slot_offset + prepared.tiling.slot.len()]
            .copy_from_slice(&prepared.tiling.slot);
        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
        if deferred_outer != G17PDeferredOuterPublication::Tiling {
            state[tiling_producer_offset..tiling_producer_offset + 4]
                .copy_from_slice(&prepared.tiling.next_producer.to_le_bytes());
        }
        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);

        Ok((prepared, render_bind))
    }

    /// Make a fragment outer slot retained by a split render publication
    /// visible after TA execution has completed.
    ///
    /// The first phase already validated and wrote the complete slot body, but
    /// deliberately left this channel's producer unchanged. Rewrite the body
    /// before advancing the producer so the second phase remains a complete
    /// ordinary slot publication even if diagnostic reads happened between
    /// the two doorbells. Never roll a producer backwards.
    pub(crate) fn publish_deferred_partial_opening_fragment(
        &mut self,
        plan: &g17_submission::G17PPartialOpeningOuterSlotPlan,
    ) -> Result {
        let channel = self.partial_opening_binding().fragment_channel;
        self.publish_deferred_partial_opening_outer(
            plan,
            channel,
            g17_submission::G17P_PARTIAL_OPENING_FRAGMENT_CHANNEL_TABLE_INDEX,
        )
    }

    /// Publish a retained TA outer slot only after its complete inner/SKSM
    /// group has been staged. This is the mirror of the split-launch fragment
    /// helper and obeys the same no-rewind producer rule.
    pub(crate) fn publish_deferred_partial_opening_tiling(
        &mut self,
        plan: &g17_submission::G17PPartialOpeningOuterSlotPlan,
    ) -> Result {
        let channel = self.partial_opening_binding().tiling_channel;
        self.publish_deferred_partial_opening_outer(
            plan,
            channel,
            g17_submission::G17P_PARTIAL_OPENING_TILING_CHANNEL_TABLE_INDEX,
        )
    }

    fn publish_deferred_partial_opening_outer(
        &mut self,
        plan: &g17_submission::G17PPartialOpeningOuterSlotPlan,
        channel: g17_initdata::ChannelEntry,
        channel_table_index: u8,
    ) -> Result {
        let current_producer = self.resources.primary_state.read_u32(
            channel.states[g17_initdata::CHANNEL_STATE_PRODUCER],
        )?;
        if !g17p_deferred_outer_plan_ready(plan, channel_table_index, current_producer) {
            return Err(EBUSY);
        }

        let slot_gpu_va = channel
            .ring
            .checked_add(plan.slot_byte_offset as u64)
            .ok_or(EINVAL)?;
        let slot_offset: usize = slot_gpu_va
            .checked_sub(self.resources.shared_cluster.iova())
            .ok_or(EINVAL)?
            .try_into()?;
        let slot_end = slot_offset.checked_add(plan.slot.len()).ok_or(EINVAL)?;
        let producer_gpu_va = channel.states[g17_initdata::CHANNEL_STATE_PRODUCER];
        let producer_offset: usize = producer_gpu_va
            .checked_sub(self.resources.primary_state.iova())
            .ok_or(EINVAL)?
            .try_into()?;
        let producer_end = producer_offset.checked_add(4).ok_or(EINVAL)?;
        if slot_end > self.resources.shared_cluster.logical_size
            || producer_end > self.resources.primary_state.logical_size
        {
            return Err(ERANGE);
        }

        let shared_vmap = self.resources.shared_cluster.object.vmap()?;
        let state_vmap = self.resources.primary_state.object.vmap()?;
        let shared = unsafe {
            // SAFETY: the checked slot lies inside the complete shared VMap.
            core::slice::from_raw_parts_mut(
                shared_vmap.as_mut_ptr(),
                self.resources.shared_cluster.logical_size,
            )
        };
        let state = unsafe {
            // SAFETY: the checked producer lies inside the complete state VMap.
            core::slice::from_raw_parts_mut(
                state_vmap.as_mut_ptr(),
                self.resources.primary_state.logical_size,
            )
        };
        shared[slot_offset..slot_end].copy_from_slice(&plan.slot);
        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
        state[producer_offset..producer_end].copy_from_slice(&plan.next_producer.to_le_bytes());
        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
        Ok(())
    }

    pub(crate) fn with_b2_pages_mut<R>(
        &mut self,
        f: impl FnOnce(&mut [u8], &mut [u8], &mut [u8]) -> Result<R>,
    ) -> Result<R> {
        let computed = self.resources.partial_computed.object.vmap()?;
        let region_1 = self.resources.partial_scheduler.object.vmap()?;
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

    #[test]
    fn compute_flist_stops_before_native_completion_aliases() {
        let mut raw = vec![0xa5; G17P_CONTROL_OPERAND_TABLE_BACKING_SIZE];
        assert!(encode_g17p_compute_flist_runs(&mut raw));
        let count = g17_initdata::COMPUTE_READINESS_OPERAND_ENTRY_COUNT;
        assert_eq!(count, 21);
        for buffer_index in 0..count {
            let base = g17p_control_operand_buffer_va(buffer_index).unwrap();
            let offset = buffer_index * core::mem::size_of::<u64>();
            assert_eq!(
                u64::from_le_bytes(raw[offset..offset + 8].try_into().unwrap()),
                (base & G17P_CONTROL_OPERAND_RUN_ADDRESS_MASK)
                    | (G17P_CONTROL_OPERAND_RUN_LENGTH_PAGES << 52)
            );
        }
        assert!(raw[count * core::mem::size_of::<u64>()..]
            .iter()
            .all(|byte| *byte == 0));

        let last_end = g17p_control_operand_buffer_va(count - 1).unwrap()
            + G17P_CONTROL_OPERAND_BUFFER_SIZE as u64;
        let first_completion = g17_initdata::G17P_KSM_COMPLETION_LOW_VAS[0];
        assert!(last_end <= first_completion);
        let opening_only = g17p_control_operand_buffer_va(count).unwrap();
        assert!(opening_only < first_completion);
        assert!(first_completion < opening_only + G17P_CONTROL_OPERAND_BUFFER_SIZE as u64);
    }

    #[test]
    fn render_flist_run_list_matches_native_initial_and_first_grow_slots() {
        let mut raw = vec![0xa5; G17P_CONTROL_OPERAND_TABLE_BACKING_SIZE];
        assert!(encode_g17p_pre_qid_flist_runs(&mut raw));
        for buffer_index in 0..G17P_CONTROL_OPERAND_ACTIVE_BLOCK_COUNT {
            let base = g17p_control_operand_buffer_va(buffer_index).unwrap();
            let offset = buffer_index * G17P_CONTROL_OPERAND_RUN_SLOT_SIZE;
            assert_eq!(
                u64::from_le_bytes(raw[offset..offset + 8].try_into().unwrap()),
                (base & G17P_CONTROL_OPERAND_RUN_ADDRESS_MASK)
                    | (G17P_CONTROL_OPERAND_RUN_LENGTH_PAGES << 52)
            );
            assert!(raw[offset + 8..offset + G17P_CONTROL_OPERAND_RUN_SLOT_SIZE]
                .iter()
                .all(|byte| *byte == 0));
        }
        let used = G17P_CONTROL_OPERAND_ACTIVE_BLOCK_COUNT
            * G17P_CONTROL_OPERAND_RUN_SLOT_SIZE;
        assert_eq!(G17P_CONTROL_OPERAND_INITIAL_BLOCK_COUNT * 0x40, 0x580);
        assert_eq!(G17P_CONTROL_OPERAND_FIRST_GROW_BLOCK_COUNT * 8, 0x20);
        assert!(raw[used..].iter().all(|byte| *byte == 0));
        assert!(!encode_g17p_pre_qid_flist_runs(
            &mut raw[..G17P_CONTROL_OPERAND_TABLE_BACKING_SIZE - 1]
        ));
    }

    #[test]
    fn render_flist_initial_page_list_expands_the_22_native_blocks() {
        let mut raw = vec![0xa5; G17P_CONTROL_OPERAND_PAGE_LIST_SIZE];
        assert!(encode_g17p_initial_flist_page_list(&mut raw));
        let entries = G17P_CONTROL_OPERAND_INITIAL_BLOCK_COUNT
            * G17P_CONTROL_OPERAND_RUN_LENGTH_PAGES as usize;
        assert_eq!(entries, 0x1600);
        for block in 0..G17P_CONTROL_OPERAND_INITIAL_BLOCK_COUNT {
            let base = g17p_control_operand_buffer_va(block).unwrap();
            for page in 0..G17P_CONTROL_OPERAND_RUN_LENGTH_PAGES as usize {
                let entry = block * G17P_CONTROL_OPERAND_RUN_LENGTH_PAGES as usize + page;
                let offset = entry * 8;
                assert_eq!(
                    u64::from_le_bytes(raw[offset..offset + 8].try_into().unwrap()),
                    base + (page * 0x1000) as u64,
                );
            }
        }
        assert!(raw[entries * 8..].iter().all(|byte| *byte == 0));
        assert!(!encode_g17p_initial_flist_page_list(
            &mut raw[..G17P_CONTROL_OPERAND_PAGE_LIST_SIZE - 1]
        ));
    }

    #[test]
    fn last_submitted_timestamp_table_is_exactly_bundle_view_four() {
        assert_eq!(G17P_LAST_SUBMITTED_HW_TIMESTAMP_BUNDLE_VIEW_INDEX, 4);
        assert_eq!(G17P_LAST_SUBMITTED_HW_TIMESTAMP_BUNDLE_OFFSET, 0xbd00);
        assert_eq!(G17P_LAST_SUBMITTED_HW_TIMESTAMP_TABLE_SIZE, 0x800);
        assert_eq!(
            G17P_LAST_SUBMITTED_HW_TIMESTAMP_BUNDLE_OFFSET
                + G17P_LAST_SUBMITTED_HW_TIMESTAMP_TABLE_SIZE,
            0xc500
        );
        assert!(
            G17P_LAST_SUBMITTED_HW_TIMESTAMP_BUNDLE_OFFSET
                + G17P_LAST_SUBMITTED_HW_TIMESTAMP_TABLE_SIZE
                <= g17_initdata::NATIVE_SHARED_CLUSTER_SIZE
        );

        let mut short = vec![
            0xa5;
            G17P_LAST_SUBMITTED_HW_TIMESTAMP_BUNDLE_OFFSET
                + G17P_LAST_SUBMITTED_HW_TIMESTAMP_TABLE_SIZE
                - 1
        ];
        assert!(g17p_last_submitted_hw_timestamp_table_mut(&mut short).is_none());
    }

    #[test]
    fn empty_recovery_updates_only_bundle_view_four_timestamp_records() {
        let mut raw = vec![0xa5; g17_initdata::NATIVE_SHARED_CLUSTER_SIZE];
        let table = g17p_last_submitted_hw_timestamp_table_mut(&mut raw).unwrap();
        assert!(clear_g17p_last_submitted_hw_timestamps(table));
        for qid in 0..G17P_LAST_SUBMITTED_HW_TIMESTAMP_COUNT {
            let entry = qid * G17P_LAST_SUBMITTED_HW_TIMESTAMP_STRIDE;
            let entry = G17P_LAST_SUBMITTED_HW_TIMESTAMP_BUNDLE_OFFSET + entry;
            assert_eq!(&raw[entry..entry + 4], &[0; 4]);
            assert_eq!(&raw[entry + 4..entry + 8], &[0xa5; 4]);
            assert_eq!(&raw[entry + 8..entry + 0x10], &[0; 8]);
        }
        assert!(raw[..G17P_LAST_SUBMITTED_HW_TIMESTAMP_BUNDLE_OFFSET]
            .iter()
            .all(|byte| *byte == 0xa5));
        assert!(raw[G17P_LAST_SUBMITTED_HW_TIMESTAMP_BUNDLE_OFFSET
            + G17P_LAST_SUBMITTED_HW_TIMESTAMP_TABLE_SIZE..]
            .iter()
            .all(|byte| *byte == 0xa5));

        let mut short = vec![0xa5; G17P_LAST_SUBMITTED_HW_TIMESTAMP_TABLE_SIZE - 1];
        assert!(!clear_g17p_last_submitted_hw_timestamps(&mut short));
        assert!(short.iter().all(|byte| *byte == 0xa5));
    }

    const KERN: u64 = 0xffff_fc20_0000_0000;

    fn get_u32(raw: &[u8], offset: usize) -> u32 {
        u32::from_le_bytes(raw[offset..offset + 4].try_into().unwrap())
    }

    fn get_u64(raw: &[u8], offset: usize) -> u64 {
        u64::from_le_bytes(raw[offset..offset + 8].try_into().unwrap())
    }

    #[test]
    fn g17p_parameter_management_matches_the_native_object_graph() {
        let pm = g17p_parameter_management_layout().unwrap();
        assert_eq!(G17P_PM_HWPB_STATE_SIZE, 0xc0);
        assert_eq!(pm.hwpb_state, 0);
        assert_eq!(pm.page_list, 0x8000);
        assert_eq!(pm.block_page_bases, 0x18000);
        assert_eq!(pm.shared_control, 0x20000);
        assert_eq!(pm.counter, 0x28000);
        assert_eq!(pm.page_metrics, 0x30000);
        assert_eq!(pm.scene_states, 0x38000);
        assert_eq!(pm.total_size, 0x40000);
        assert_eq!(G17P_PM_PAGE_LIST_SIZE, 0xd1a0);
        assert_eq!(G17P_PM_BLOCK_PAGE_BASE_TABLE_SIZE, 0x68d0);
        assert_eq!(G17P_PM_SCENE_ALLOCATION_SIZE, 0x2840);
        let tvb_va = 0x10_0008_8000;
        let scene_scratch_va = 0x10_0017_8000;
        let discard_va = 0x10_0018_0000;
        assert_eq!(g17p_render_cycle_pointer(scene_scratch_va), Ok(0x178020));
        assert_eq!(
            g17p_render_cycle_pointer(0x10_0200_8000),
            Ok(0x2008020)
        );
        let render = G17PRenderStateLayout {
            heapmeta: 0,
            heapmeta_size: 0,
            tpc: 0,
            tpc_size: 0,
            aux_fb: 0,
            tilemap: 0,
            tilemap_size: 0,
            tvb: 0x88_000,
            tvb_size: 7 * G17P_RENDER_TVB_BLOCK_STRIDE + G17P_RENDER_TVB_BLOCK_SIZE,
            tvb_blocks: 8,
            pm_scene_scratch: 0x178_000,
            pm_discard: 0x180_000,
            total_size: 0x2c0_000,
        };
        let high = 0xffff_fc20_c091_0000;
        let page_list_vas = [0xffff_fc20_c097_8000, 0x10_0019_0000];
        let metrics_vas = [
            0xffff_fc20_016d_0000,
            mmu::T8140_PARAMETER_METRICS_LOW_VA,
        ];
        let mut raw = vec![0xa5; pm.total_size];
        assert_eq!(
            encode_g17p_parameter_management(
                &mut raw,
                high,
                page_list_vas,
                metrics_vas,
                tvb_va,
                scene_scratch_va,
                discard_va,
                5,
                &render,
                &pm,
            ),
            Ok(())
        );

        let state = pm.hwpb_state;
        assert_eq!(get_u32(&raw, state + 0x0c), 5);
        assert_eq!(get_u64(&raw, state + 0x20), page_list_vas[0]);
        assert_eq!(get_u64(&raw, state + 0x28), page_list_vas[1]);
        assert_eq!(get_u32(&raw, state + 0x30), 0x10000);
        assert_eq!(get_u32(&raw, state + 0x34), 0x20);
        assert_eq!(get_u32(&raw, state + 0x38), 0xd1a);
        assert_eq!(get_u32(&raw, state + 0x3c), 8);
        assert_eq!(get_u32(&raw, state + 0x40), 0);
        assert_eq!(
            get_u64(&raw, state + 0x44),
            high + pm.block_page_bases as u64
        );
        assert_eq!(
            get_u64(&raw, state + 0x4c),
            high + pm.shared_control as u64
        );
        assert_eq!(get_u32(&raw, state + 0x54), 0x1f);
        assert_eq!(get_u32(&raw, state + 0x58), 0x20000);
        assert_eq!(get_u64(&raw, state + 0x64), high + pm.counter as u64);
        assert_eq!(get_u32(&raw, state + 0x7c), 0x3468);
        assert_eq!(get_u32(&raw, state + 0x80), 0x1178);
        assert_eq!(get_u64(&raw, state + 0x84), 0x180000);

        for block in 0..8 {
            // The GART segment survives the native `(gpu_va >> 15)` page-ID
            // encoding as bit 21.
            let first = 0x20_0011 + block as u32 * 5;
            assert_eq!(get_u32(&raw, pm.block_page_bases + block * 8), first);
            assert_eq!(get_u32(&raw, pm.block_page_bases + block * 8 + 4), 0);
            for page in 0..4 {
                assert_eq!(
                    get_u32(&raw, pm.page_list + (block * 4 + page) * 4),
                    first + page as u32
                );
            }
        }
        assert_eq!(get_u32(&raw, pm.shared_control), 8);
        assert_eq!(get_u32(&raw, pm.shared_control + 4), 8);
        assert_eq!(get_u32(&raw, pm.shared_control + 8), 0);
        assert_eq!(get_u32(&raw, pm.shared_control + 0x60), 1);

        let selected = pm.scene_states + G17P_PM_SCENE_ENTRY_SIZE;
        assert_eq!(get_u64(&raw, selected), metrics_vas[0] + 4);
        assert_eq!(get_u64(&raw, selected + 8), metrics_vas[1] + 4);
        assert_eq!(get_u64(&raw, selected + 0x28), 0x178030);
        assert_eq!(
            get_u64(
                &raw,
                pm.scene_states + G17P_PM_SCENE_SCRATCH_SLOT_COUNT
                    * G17P_PM_SCENE_ENTRY_SIZE
                    + 0x28,
            ),
            0x178000
        );
        assert_eq!(
            get_u64(&raw, selected + 0x40),
            high + pm.shared_control as u64 + 0x40
        );
        assert_eq!(get_u64(&raw, selected + 0x4c), 0);
        let trailer = pm.scene_states + G17P_PM_SCENE_COUNT * G17P_PM_SCENE_ENTRY_SIZE;
        assert!(raw[trailer..trailer + G17P_PM_SCENE_TRAILER_SIZE]
            .iter()
            .all(|byte| *byte == 0));
    }

    #[test]
    fn g17p_cl_runtime_storage_has_native_ring_geometry() {
        let geometry = g17_submission::T8140_G17P_HAL200_SKSM_QUEUE_GEOMETRY;
        assert_eq!(geometry.validate(), Ok(()));
        assert_eq!(geometry.allocation_alignment(), 0x100);
        assert_eq!(geometry.mapping_alignment(G17P_PAGE_SIZE as u64), 0x4000);
        assert_eq!(geometry.entry_stride(), 0x200);
        assert_eq!(geometry.stamp_count(), 256);
        assert_eq!(geometry.timestamp_seed(), 1);
        assert_eq!(geometry.backing_size(), 0x20000);
        assert_eq!(G17P_CL_ENTRY_STORAGE_SIZE, geometry.backing_size());
        assert_eq!(G17P_CL_SHARED_SUPPORT_SIZE, 0x4000);
        assert_eq!(G17P_CL_B2_OBJECT_SIZE, 0x4000);
        assert_eq!(G17P_CL_CHANNEL_CONTROL_SIZE, 0x4000);
        assert_eq!(G17P_CL_LOW_VA_START, 0x70_0000_0000);
        assert_eq!(G17P_CL_LOW_VA_END, 0x71_0000_0000);
        assert_eq!(G17P_RENDER_USER_ALIAS_START, 0x10_0200_0000);
        assert_eq!(G17P_RENDER_USER_ALIAS_END, 0x10_0210_0000);
        assert_eq!(G17P_RENDER_USER_TVB_START, G17P_RENDER_USER_ALIAS_END);
        assert_eq!(G17P_RENDER_USER_TVB_END, 0x10_8000_0000);
        assert!(G17P_RENDER_USER_ALIAS_START >= G17P_RENDER_USER_BASE);
        assert!(G17P_RENDER_USER_TVB_END <= G17P_RENDER_USER_END);
        assert!(
            G17P_RENDER_USER_ALIAS_START + 2 * G17P_RENDER_USER_ALIAS_SLOT_SIZE
                <= G17P_RENDER_USER_ALIAS_END
        );
        let maximum_tvb_size = (G17P_PM_MAX_BLOCKS - 1) * G17P_RENDER_TVB_BLOCK_STRIDE
            + G17P_RENDER_TVB_BLOCK_SIZE;
        assert!(
            2 * maximum_tvb_size as u64
                <= G17P_RENDER_USER_TVB_END - G17P_RENDER_USER_TVB_START
        );
        assert_eq!(
            g17_initdata::COMPUTE_READINESS_CLASS1_SUPPORT_ADDRESS,
            0xffff_fc20_c087_8000
        );
        assert_eq!(
            g17_initdata::COMPUTE_READINESS_CLASS1_STATE_ADDRESS,
            0xffff_fc20_0164_8000
        );
        assert_eq!(
            g17_initdata::COMPUTE_READINESS_CLASS3_SUPPORT_ADDRESS,
            0xffff_fc20_c085_0000
        );
        assert_eq!(
            g17_initdata::COMPUTE_READINESS_CLASS3_STATE_ADDRESS,
            0xffff_fc20_0165_0000
        );
        assert_eq!(G17P_COMPUTE_DESCRIPTOR_SLOT_SIZE, 0x4000);
        assert_eq!(G17P_COMPUTE_DESCRIPTOR_SLOT_COUNT, 256);
        assert_eq!(G17P_COMPUTE_DESCRIPTOR_STORAGE_SIZE, 0x400000);
        assert_eq!(G17P_COMPUTE_PREEMPT_SIZE, 0x4000);
        assert_eq!(G17P_COMPUTE_OPERAND_STATE_SIZE, 0x14000);
        assert_eq!(G17P_COMPUTE_SUPPORT_SIZE, 0xc000);
        assert_eq!(G17P_COMPUTE_QUEUE_GRAPH_SIZE, 0x8000);
        assert_eq!(G17P_RETAINED_RENDER_ORDINALS, 0x100);
        assert_eq!(g17_initdata::T8140_REGISTER_WINDOWS.len(), 18);
        assert_eq!(g17_initdata::T8140_REGISTER_WINDOWS[0].slot, 0);
        assert_eq!(
            g17_initdata::T8140_REGISTER_WINDOWS[0].phys,
            0x0030_1014_000
        );
        assert_eq!(g17_initdata::T8140_REGISTER_WINDOWS[17].slot, 41);
        assert_eq!(g17_initdata::T8140_REGISTER_FLAG_ONLY_SLOTS.len(), 17);
    }

    #[test]
    fn g17p_normal_cl_completion_storage_matches_inline_descriptor_abi() {
        assert_eq!(g17_submission::G17P_NORMAL_CL_COMPLETION_ORDINAL, 0);
        assert_eq!(g17_submission::G17P_KSM_COMPLETION_PRESENT_ORDINALS, [0, 2]);
        assert_eq!(g17_initdata::G17P_KSM_COMPLETION_PRESENT_ORDINALS, [0, 2]);
        assert_eq!(g17_initdata::G17P_KSM_COMPLETION_BACKING_SIZE, 0x20000);
        assert_eq!(
            g17_initdata::G17P_KSM_COMPLETION_BACKING_SIZE,
            g17_submission::G17P_KSM_COMPLETION_BACKING_SIZE
        );
        let aliases = [
            g17_initdata::G17PKsmCompletionAliases {
                low: g17_initdata::G17P_KSM_COMPLETION_LOW_VAS[0],
                high: g17_initdata::NATIVE_SHARED_CLUSTER_GPU_VA
                    + g17_initdata::G17P_KSM_COMPLETION_HIGH_BUNDLE_OFFSETS[0] as u64,
            },
            g17_initdata::G17PKsmCompletionAliases {
                low: g17_initdata::G17P_KSM_COMPLETION_LOW_VAS[1],
                high: g17_initdata::NATIVE_SHARED_CLUSTER_GPU_VA
                    + g17_initdata::G17P_KSM_COMPLETION_HIGH_BUNDLE_OFFSETS[1] as u64,
            },
        ];
        let extent = g17_initdata::G17P_KSM_COMPLETION_BACKING_SIZE as u64;
        assert!(aliases[0].low + extent <= aliases[1].low);
        assert!(aliases[0].high + extent <= aliases[1].high);

        let mut hw = vec![0xa5; g17_initdata::HW_DATA_SIZE];
        g17_initdata::encode_hw_data(
            &[],
            &[],
            aliases,
            g17_initdata::NATIVE_SHARED_CLUSTER_GPU_VA
                + g17_initdata::NATIVE_SHARED_CLUSTER_SIZE as u64
                + 0x8000,
            g17_initdata::NATIVE_SHARED_CLUSTER_GPU_VA
                + g17_initdata::NATIVE_SHARED_CLUSTER_SIZE as u64
                + 0x4000,
            &mut hw,
        )
        .unwrap();
        for ordinal in [1usize, 3] {
            let base = g17_initdata::G17P_KSM_COMPLETION_TABLE_OFFSET
                + ordinal * g17_initdata::G17P_KSM_COMPLETION_DESCRIPTOR_STRIDE;
            assert_eq!(&hw[base..base + 0x20], &[0; 0x20]);
        }
        for (pair, ordinal) in aliases
            .iter()
            .zip(g17_submission::G17P_KSM_COMPLETION_PRESENT_ORDINALS)
        {
            let expected = g17_submission::encode_g17p_ksm_completion_descriptor(
                g17_submission::G17ChannelAbiTarget::A18ProG17P,
                ordinal,
                pair.low,
                pair.high,
                g17_submission::G17P_KSM_COMPLETION_CAPACITY,
            )
            .unwrap();
            let base = g17_initdata::G17P_KSM_COMPLETION_TABLE_OFFSET
                + ordinal as usize * g17_initdata::G17P_KSM_COMPLETION_DESCRIPTOR_STRIDE;
            assert_eq!(&hw[base..base + expected.len()], &expected);
        }
    }

    #[test]
    fn class3_readiness_reuses_only_the_retired_primary_index_page() {
        let owner = g17_submission::G17P_PARTIAL_OPENING_PRIMARY_INDEX_FIRMWARE_GPU_VA;
        let owner_size = g17_submission::G17P_PARTIAL_OPENING_PRIMARY_INDEX_SIZE;
        let class3 = g17_initdata::COMPUTE_READINESS_CLASS3_SUPPORT_ADDRESS;
        assert_eq!(G17P_COMPUTE_READINESS_CLASS3_PRIMARY_INDEX_OFFSET, 0x8000);
        assert_eq!(class3, owner + 0x8000);
        assert!(G17P_COMPUTE_READINESS_CLASS3_PRIMARY_INDEX_OFFSET
            + g17_initdata::COMPUTE_READINESS_PAGE_SIZE
            <= owner_size);

        let mut storage = vec![0u8; owner_size];
        g17_submission::apply_g17p_partial_opening_primary_index_page(
            &mut storage[..g17_submission::G17P_PARTIAL_OPENING_PAGE_SIZE],
        )
        .unwrap();
        let opening_page = storage[..g17_submission::G17P_PARTIAL_OPENING_PAGE_SIZE].to_vec();
        let start = G17P_COMPUTE_READINESS_CLASS3_PRIMARY_INDEX_OFFSET;
        let end = start + g17_initdata::COMPUTE_READINESS_PAGE_SIZE;
        g17_initdata::encode_compute_readiness_class3_support(&mut storage[start..end]).unwrap();
        let mut expected = vec![0u8; g17_initdata::COMPUTE_READINESS_PAGE_SIZE];
        g17_initdata::encode_compute_readiness_class3_support(&mut expected).unwrap();
        assert_eq!(&storage[start..end], expected);
        assert_eq!(
            &storage[..g17_submission::G17P_PARTIAL_OPENING_PAGE_SIZE],
            opening_page,
        );

        let owner_end = owner + owner_size as u64;
        for (address, size) in [
            (
                g17_initdata::COMPUTE_READINESS_CLASS1_SUPPORT_ADDRESS,
                g17_initdata::COMPUTE_READINESS_PAGE_SIZE as u64,
            ),
            (
                g17_initdata::COMPUTE_READINESS_CLASS1_STATE_ADDRESS,
                g17_initdata::COMPUTE_READINESS_PAGE_SIZE as u64,
            ),
            (
                g17_initdata::COMPUTE_READINESS_CLASS3_STATE_ADDRESS,
                g17_initdata::COMPUTE_READINESS_PAGE_SIZE as u64,
            ),
        ] {
            assert!(address >= owner_end || address + size <= owner);
        }
    }

    #[test]
    fn dynamic_cl_range_excludes_the_complete_fixed_low_arena() {
        let dynamic = G17P_CL_DYNAMIC_LOW_VA_START..G17P_CL_LOW_VA_END;
        assert_eq!(dynamic.start, 0x70_0200_0000);
        assert!(G17P_CL_LOW_VA_START < dynamic.start && dynamic.start < dynamic.end);
        let table = G17P_CONTROL_OPERAND_TABLE_VA
            ..G17P_CONTROL_OPERAND_TABLE_VA
                + g17_initdata::CONTROL_OPERAND_TABLE_SIZE as u64;
        assert!(table.end <= dynamic.start);

        for index in 0..G17P_CONTROL_OPERAND_BUFFER_COUNT {
            let start = g17p_control_operand_buffer_va(index).unwrap();
            let end = start + G17P_CONTROL_OPERAND_BUFFER_SIZE as u64;
            assert!(end <= dynamic.start);
        }
        let fixed_end = g17p_control_operand_buffer_va(G17P_CONTROL_OPERAND_BUFFER_COUNT - 1)
            .unwrap()
            + G17P_CONTROL_OPERAND_BUFFER_SIZE as u64;
        assert_eq!(fixed_end, 0x70_01ef_8000);
        assert_eq!(dynamic.start - fixed_end, 0x10_8000);

        for (start, size) in [
            (G17P_CONTROL_DIRECTORY_VA, G17P_CONTROL_DIRECTORY_SIZE),
            (G17P_BOOTSTRAP_TA_DESCRIPTOR_LOW_VA, G17P_PAGE_SIZE),
            (G17P_BOOTSTRAP_3D_DESCRIPTOR_LOW_VA, G17P_PAGE_SIZE),
            (G17P_BOOTSTRAP_TA_CONTEXT_LOW_VA, G17P_BOOTSTRAP_CONTEXT_PEER_SIZE),
            (G17P_BOOTSTRAP_3D_CONTEXT_LOW_VA, G17P_BOOTSTRAP_CONTEXT_PEER_SIZE),
            (
                g17_initdata::COMPUTE_RUNTIME_CLASS1_TABLE_ADDRESS,
                g17_initdata::COMPUTE_RUNTIME_CLASS1_TABLE_SIZE,
            ),
            (
                g17_initdata::COMPUTE_RUNTIME_PAGE_LIST_ADDRESS,
                g17_initdata::COMPUTE_RUNTIME_PAGE_LIST_SIZE,
            ),
        ] {
            assert!(start >= G17P_CL_LOW_VA_START);
            assert!(start + size as u64 <= dynamic.start);
        }
        assert!(g17_initdata::REGION_VIEW_LOW_ADDRS
            .into_iter()
            .all(|address| address < dynamic.start));
    }

    #[test]
    fn compute_readiness_activation_record_is_distinct_from_the_cl_record() {
        let mut shared = [0u8; G17P_CL_SHARED_SUPPORT_SIZE];
        let mut support = [0u8; G17P_CL_SUPPORT_STATE_SIZE];
        let mut scheduler = [0u8; G17P_CL_SUPPORT_STATE_SIZE];
        let mut channel = [0u8; G17P_CL_CHANNEL_CONTROL_SIZE];
        g17_submission::apply_g17p_cl_retained_static_state(
            &mut shared,
            &mut support,
            &mut scheduler,
            &mut channel,
            0x70_0020_8000,
            0xffff_fc20_0164_8000,
            0xffff_fc20_0165_0000,
        )
        .unwrap();
        let cl_before = channel[..g17_initdata::CONTROL_RECORD_SIZE].to_vec();
        let activation = g17_initdata::COMPUTE_READINESS_ACTIVATION_RECORD_OFFSET;
        assert!(channel[activation..activation + g17_initdata::CONTROL_RECORD_SIZE]
            .iter()
            .all(|byte| *byte == 0));
        g17_initdata::encode_compute_readiness_activation_record(
            &mut channel[activation..activation + g17_initdata::CONTROL_RECORD_SIZE],
        )
        .unwrap();
        assert_eq!(&channel[..g17_initdata::CONTROL_RECORD_SIZE], cl_before);
        assert!(channel[g17_initdata::CONTROL_RECORD_SIZE..activation]
            .iter()
            .all(|byte| *byte == 0));
        assert_eq!(get_u64(&channel, activation), 0x0000_0100_0000_ffff);
        assert_eq!(get_u64(&channel, activation + 0x20), 0x0002_0000_0000_0000);
        assert_eq!(get_u64(&channel, activation + 0x30), 0x0000_0000_ff00_0000);
    }

    /// The item ring must WRAP, and the publisher must take the wrapped
    /// producer the transition computed.
    ///
    /// This is the 320-submission wall in one assertion. The ring is `0x500`
    /// entries and a submission publishes four records, so submission 320's
    /// last record -- the tag-16 EntrySignal -- lands on the final slot. The
    /// transition correctly returns 0; the publisher used to compare that
    /// against a locally computed `expected_producer + 1` and refuse with
    /// EBUSY on a ring that was almost entirely free.
    #[test]
    fn item_ring_command_pointer_wraps_to_zero_on_the_last_slot() {
        const RECORD: u64 = 0xffff_fc20_c035_8000;
        let count = g17_compute::compute_item_ring_entries();
        let pointers = g17_compute::COMPUTE_QUEUE_POINTERS;
        let mut graph = vec![0u8; g17_compute::COMPUTE_QUEUE_GRAPH_SIZE];
        put_u32(&mut graph, pointers + 0x60, count);
        // Consumer far behind the producer, so the ring is nowhere near full
        // and the only thing under test is the wrap itself.
        put_u32(&mut graph, pointers, count / 2);
        put_u32(&mut graph, pointers + 0x40, count - 1);

        assert_eq!(
            apply_g17p_initial_command_pointer(&mut graph, count - 1, RECORD),
            Some(0),
            "the last slot's transition must report producer 0, not count"
        );
        assert_eq!(
            get_u32(&graph, pointers + 0x40),
            0,
            "the graph's producer must wrap to 0"
        );
        assert_eq!(
            get_u64(
                &graph,
                g17_compute::COMPUTE_ITEM_RING + (count as usize - 1) * 8
            ),
            RECORD
        );

        // And the lap after the wrap publishes at slot 0 as usual.
        assert_eq!(
            apply_g17p_initial_command_pointer(&mut graph, 0, RECORD),
            Some(1)
        );
    }

    #[test]
    fn g17p_first_bind_tag15_reuses_the_admitted_optional_builder() {
        const TAG15: &[u8; 0x86] =
            include_bytes!("fixtures/g17p_first_submit/tag15-config-update-first.bin");
        const RELOCATION_MASK: &[u8; 0x86] = include_bytes!(
            "fixtures/g17p_first_submit/tag15-config-update-relocation-mask.bin"
        );
        const BEFORE_TAG15: &[u8; 0x80] =
            include_bytes!("fixtures/g17p_first_submit/channel-state-before-tag15.bin");
        const AFTER_TAG15: &[u8; 0x80] =
            include_bytes!("fixtures/g17p_first_submit/channel-state-after-tag15.bin");
        const AFTER_TAG16: &[u8; 0x80] =
            include_bytes!("fixtures/g17p_first_submit/channel-state-after-tag16.bin");
        const QUEUE: u64 = 0xffff_fc20_c000_0300;
        const DESCRIPTOR: u64 = 0xffff_fc20_c035_8000;
        const ENTRY_LOW: u64 = 0x70_004d_8000;
        const ENTRY_HIGH: u64 = 0xffff_fc20_0027_8000;
        const SHARED_SUPPORT: u64 = 0xffff_fc20_c087_0000;
        const CHANNEL_CONTROL: u64 = 0xffff_fc20_c07b_8040;
        const GEOMETRY: g17_submission::G17SksmQueueGeometry =
            g17_submission::T8140_G17P_HAL200_SKSM_QUEUE_GEOMETRY;

        let mut graph = [0u8; g17_compute::COMPUTE_QUEUE_GRAPH_SIZE];
        g17_compute::build_compute_queue_graph(QUEUE, CHANNEL_CONTROL, &mut graph).unwrap();
        let mut context = vec![0u8; g17_compute::COMPUTE_QUEUE_CONTEXT_EXTENT];
        let sksm_entries = g17_compute::ComputeSksmEntryAliases::new(
            ENTRY_LOW,
            ENTRY_HIGH,
            GEOMETRY.backing_size(),
            GEOMETRY.allocation_alignment(),
        )
        .unwrap();
        g17_compute::build_initial_compute_channel_group(
            QUEUE,
            DESCRIPTOR,
            sksm_entries,
            SHARED_SUPPORT,
            CHANNEL_CONTROL,
            4,
            &mut graph,
            &mut context,
            true,
        )
        .unwrap();
        let actual = &graph
            [g17_compute::COMPUTE_OPTIONAL..g17_compute::COMPUTE_OPTIONAL + TAG15.len()];
        assert_eq!(actual, *TAG15);
        assert_eq!(get_u64(actual, 0x08), sksm_entries.low());
        assert_eq!(get_u64(actual, 0x10), sksm_entries.high());
        assert_eq!(sksm_entries.extent(), GEOMETRY.backing_size());
        assert_eq!(
            sksm_entries.range_at(0x200, 0x200).unwrap(),
            [ENTRY_LOW + 0x200, ENTRY_HIGH + 0x200]
        );
        assert!(RELOCATION_MASK.iter().any(|byte| *byte != 0));
        for index in 0..TAG15.len() {
            assert_eq!(
                actual[index] & !RELOCATION_MASK[index],
                TAG15[index] & !RELOCATION_MASK[index]
            );
        }

        // Moving the two mappings of the same backing changes only the two
        // address qwords selected by the admitted relocation mask.
        let relocated = g17_compute::ComputeSksmEntryAliases::new(
            ENTRY_LOW + 0x4000,
            ENTRY_HIGH + 0x8000,
            GEOMETRY.backing_size(),
            GEOMETRY.allocation_alignment(),
        )
        .unwrap();
        let mut relocated_graph = [0u8; g17_compute::COMPUTE_QUEUE_GRAPH_SIZE];
        g17_compute::build_compute_queue_graph(
            QUEUE,
            CHANNEL_CONTROL,
            &mut relocated_graph,
        )
        .unwrap();
        let mut relocated_context = vec![0u8; g17_compute::COMPUTE_QUEUE_CONTEXT_EXTENT];
        g17_compute::build_initial_compute_channel_group(
            QUEUE,
            DESCRIPTOR,
            relocated,
            SHARED_SUPPORT,
            CHANNEL_CONTROL,
            4,
            &mut relocated_graph,
            &mut relocated_context,
            true,
        )
        .unwrap();
        let relocated_actual = &relocated_graph
            [g17_compute::COMPUTE_OPTIONAL..g17_compute::COMPUTE_OPTIONAL + TAG15.len()];
        assert_eq!(get_u64(relocated_actual, 0x08), relocated.low());
        assert_eq!(get_u64(relocated_actual, 0x10), relocated.high());
        assert_eq!(relocated_context, context);
        for index in 0..TAG15.len() {
            assert_eq!(
                relocated_actual[index] & !RELOCATION_MASK[index],
                actual[index] & !RELOCATION_MASK[index]
            );
        }

        graph[g17_compute::COMPUTE_EVENT
            ..g17_compute::COMPUTE_EVENT + g17_compute::COMPUTE_EVENT_SIZE]
            .fill(0);
        put_u64(&mut graph, g17_compute::COMPUTE_ITEM_RING + 0x08, 0);
        put_u64(&mut graph, g17_compute::COMPUTE_ITEM_RING + 0x10, 0);
        put_u32(&mut graph, g17_compute::COMPUTE_QUEUE_POINTERS + 0x40, 1);
        assert_eq!(
            &graph[g17_compute::COMPUTE_QUEUE_POINTERS
                ..g17_compute::COMPUTE_QUEUE_POINTERS + 0x80],
            *BEFORE_TAG15
        );

        assert_eq!(
            apply_g17p_initial_command_pointer(
                &mut graph,
                1,
                QUEUE + g17_compute::COMPUTE_OPTIONAL as u64,
            ),
            Some(2)
        );
        assert_eq!(
            &graph[g17_compute::COMPUTE_QUEUE_POINTERS
                ..g17_compute::COMPUTE_QUEUE_POINTERS + 0x80],
            *AFTER_TAG15
        );

        let mut entry_signal = [0u8; g17_submission::G17P_COMPUTE_OPTIONAL_EVENT_SIZE];
        g17_submission::encode_g17p_compute_optional_event(
            g17_submission::G17PComputeOptionalEvent {
                queue_id: 4,
                old_timestamp: 1,
            },
            &mut entry_signal,
        )
        .unwrap();
        graph[g17_compute::COMPUTE_EVENT
            ..g17_compute::COMPUTE_EVENT + entry_signal.len()]
            .copy_from_slice(&entry_signal);
        assert_eq!(
            apply_g17p_initial_command_pointer(
                &mut graph,
                2,
                QUEUE + g17_compute::COMPUTE_EVENT as u64,
            ),
            Some(3)
        );
        assert_eq!(
            &graph[g17_compute::COMPUTE_QUEUE_POINTERS
                ..g17_compute::COMPUTE_QUEUE_POINTERS + 0x80],
            *AFTER_TAG16
        );
        assert_eq!(
            get_u64(&graph, g17_compute::COMPUTE_ITEM_RING + 0x08),
            QUEUE + g17_compute::COMPUTE_OPTIONAL as u64
        );
        assert_eq!(
            get_u64(&graph, g17_compute::COMPUTE_ITEM_RING + 0x10),
            QUEUE + g17_compute::COMPUTE_EVENT as u64
        );
        assert!(
            g17_compute::COMPUTE_ITEM_RING + g17_compute::COMPUTE_ITEM_RING_SIZE
                <= g17_compute::COMPUTE_OPTIONAL
        );
    }

    #[test]
    fn t8140_register_windows_expand_to_distinct_uat_pages() {
        let mapped: Vec<_> = g17_initdata::T8140_REGISTER_WINDOWS
            .iter()
            .map(|window| register_window_pages(window).unwrap())
            .collect();

        for (index, (_, device_page, span)) in mapped.iter().copied().enumerate() {
            assert_eq!(device_page & (G17P_PAGE_SIZE as u64 - 1), 0);
            assert_eq!(span & (G17P_PAGE_SIZE as u64 - 1), 0);
            assert!(g17_initdata::HW_REGISTER_APERTURE.start <= device_page);
            assert!(
                device_page + span <= g17_initdata::HW_REGISTER_APERTURE.end,
                "register window {index} leaves its aperture"
            );
            for (_, other_page, other_span) in mapped.iter().copied().skip(index + 1) {
                assert!(
                    device_page + span <= other_page || other_page + other_span <= device_page,
                    "register window {index} overlaps another expanded mapping"
                );
            }
        }

        assert_eq!(
            mapped[17],
            (0x0048_0e1c_000, 0xffff_fc21_800e_8000, 0x8000)
        );
        let computed = g17_submission::G17P_PARTIAL_OPENING_COMPUTED_GPU_VA;
        let scheduler = g17_submission::G17P_PARTIAL_OPENING_SCHEDULER_GPU_VA;
        let b2_alias = scheduler + 0x8000;
        assert!(computed + G17P_CL_B2_OBJECT_SIZE as u64 <= scheduler);
        assert!(scheduler + G17P_CL_B2_OBJECT_SIZE as u64 <= b2_alias);
        assert!(b2_alias + G17P_CL_B2_OBJECT_SIZE as u64 <= mapped[0].1);
    }

    #[test]
    fn primary_index_owner_and_render_alias_match_the_captured_roots() {
        let owner = g17_submission::G17P_PARTIAL_OPENING_PRIMARY_INDEX_FIRMWARE_GPU_VA;
        let alias = g17_submission::G17P_PARTIAL_OPENING_PRIMARY_INDEX_GPU_VA;
        let size = g17_submission::G17P_PARTIAL_OPENING_PRIMARY_INDEX_SIZE as u64;

        assert_eq!(owner, 0xffff_fc20_c084_8000);
        assert_eq!(alias, 0x10_0019_0000);
        assert_eq!(size, 4 * G17P_PAGE_SIZE as u64);
        assert!(alias + size < G17P_CL_LOW_VA_START);
        assert!(
            g17_initdata::CONTROL_SHARED_ADDRESS
                + g17_initdata::CONTROL_SHARED_OBJECT_SIZE as u64
                <= owner
        );
        assert!(owner + size < g17_initdata::HW_REGISTER_APERTURE.start);

        let mut shared = [0u8; g17_submission::G17P_COLD_OPENING_SHARED_OBJECT_SIZE];
        g17_submission::apply_g17p_cold_opening_shared_object(
            g17_submission::G17PColdOpeningGraphAddresses {
                primary_index: owner,
                secondary_index: 0x70_0100_0000,
                pool_a_slots: 0x70_0100_4000,
                pool_b_slots: 0x70_0100_8000,
                shared_slots: 0x70_0100_c000,
                flag: 0x70_0101_0000,
            },
            &mut shared,
        )
        .unwrap();
        assert_eq!(get_u64(&shared, 0x20), owner);
    }

    #[test]
    fn fixed_high_coverage_requires_every_range() {
        let ranges = [
            (0xffff_fc20_015e_0000, 0x4000),
            (0xffff_fc20_c084_8000, 0x10000),
            (0xffff_fc21_800e_8000, 0x8000),
        ];
        assert!(all_ranges_covered(ranges, |_, _| true));
        assert!(!all_ranges_covered(ranges, |address, _| {
            address != 0xffff_fc20_c084_8000
        }));
    }

    #[test]
    fn compute_user_layout_requires_every_gpu_execution_range() {
        let layout = G17PComputeUserVaLayout {
            descriptors: 0x70_0040_0000,
            preempt: 0x70_0080_0000,
            robustness: 0x70_0080_4000,
            operand_state: 0x70_0081_0000,
            queue_context: 0x70_0084_0000,
        };
        let expected = [
            (layout.descriptors, G17P_COMPUTE_DESCRIPTOR_STORAGE_SIZE as u64),
            (layout.preempt, G17P_COMPUTE_PREEMPT_SIZE as u64),
            (layout.robustness, G17P_PAGE_SIZE as u64),
            (
                layout.operand_state,
                G17P_COMPUTE_OPERAND_STATE_SIZE as u64,
            ),
            (
                layout.queue_context,
                g17_compute::COMPUTE_QUEUE_CONTEXT_EXTENT as u64,
            ),
        ];
        let mut seen = [false; 5];
        assert!(layout.all_referenced_ranges_reachable(|address, size| {
            let index = expected
                .iter()
                .position(|range| *range == (address, size))
                .unwrap();
            seen[index] = true;
            true
        }));
        assert_eq!(seen, [true; 5]);

        for missing in 0..expected.len() {
            assert!(!layout.all_referenced_ranges_reachable(|address, size| {
                expected
                    .iter()
                    .position(|range| *range == (address, size))
                    != Some(missing)
            }));
        }
    }

    #[test]
    fn scheduler_low_alias_keeps_its_head_across_b2_activation() {
        let mut scheduler = g17_submission::build_g17p_partial_opening_scheduler_page();
        let mut computed = [0u8; G17P_CL_B2_OBJECT_SIZE];
        let mut region_2 = [0u8; G17P_CL_B2_OBJECT_SIZE];
        assert_eq!(
            g17_submission::apply_g17p_cl_b2_primary_state(
                g17_submission::G17PClB2ActivationState::Pending,
                &mut computed,
                &mut scheduler,
                &mut region_2,
                0xffff_fc20_1234_0000,
            ),
            Ok(g17_submission::G17PClB2ActivationState::Published)
        );
        assert_eq!(
            u64::from_le_bytes(scheduler[0..8].try_into().unwrap()),
            g17_submission::G17P_PARTIAL_OPENING_SCHEDULER_WORD
        );
        assert_eq!(
            u64::from_le_bytes(scheduler[0x10..0x18].try_into().unwrap()),
            0x0000_0080_0007_7000
        );
        assert_eq!(
            u64::from_le_bytes(scheduler[0x18..0x20].try_into().unwrap()),
            0x0000_0017_0000_0017
        );
    }

    #[test]
    fn user_render_queue_records_match_the_scheduled_class_two_profile() {
        fn expected(pointers: u64, ring: u64, job_list: u64, context: u64) -> [u8; 0xc0] {
            let mut record = [0u8; 0xc0];
            record[0x00..0x08].copy_from_slice(&pointers.to_le_bytes());
            record[0x08..0x10].copy_from_slice(&ring.to_le_bytes());
            record[0x10..0x18].copy_from_slice(&job_list.to_le_bytes());
            record[0x24..0x28].copy_from_slice(&u32::MAX.to_le_bytes());
            record[0x28..0x2c].copy_from_slice(&2u32.to_le_bytes());
            record[0x2c..0x30].copy_from_slice(&2u32.to_le_bytes());
            record[0x36..0x38].fill(0xff);
            record[0x38..0x3c].copy_from_slice(&0u32.to_le_bytes());
            record[0x3c..0x40].copy_from_slice(&2u32.to_le_bytes());
            record[0x40..0x44].copy_from_slice(&u32::MAX.to_le_bytes());
            record[0x44..0x48].copy_from_slice(&0x15u32.to_le_bytes());
            record[0x70..0x78].copy_from_slice(&context.to_le_bytes());
            record
        }

        let job_list = 0x70_0000_2000;
        let context = 0x70_0001_0c00;
        let mut graph = [0xa5; 2 * G17P_RENDER_QUEUE_RECORD_SIZE];
        build_g17p_user_queue_record(
            &mut graph,
            0,
            0x70_0000_1000,
            0x70_0000_4000,
            job_list,
            context,
        );
        build_g17p_user_queue_record(
            &mut graph,
            G17P_RENDER_QUEUE_RECORD_SIZE,
            0x70_0000_1080,
            0x70_0000_8000,
            job_list,
            context,
        );
        assert_eq!(
            &graph[..G17P_RENDER_QUEUE_RECORD_SIZE],
            &expected(0x70_0000_1000, 0x70_0000_4000, job_list, context,)
        );
        assert_eq!(
            &graph[G17P_RENDER_QUEUE_RECORD_SIZE..],
            &expected(0x70_0000_1080, 0x70_0000_8000, job_list, context,)
        );
    }

    #[test]
    fn user_render_pointer_blocks_carry_the_source_ring_capacity() {
        let mut pointers = [0xa5; 2 * G17P_RENDER_POINTER_BLOCK_SIZE];
        build_g17p_user_pointer_block(&mut pointers, 0);
        build_g17p_user_pointer_block(&mut pointers, G17P_RENDER_POINTER_BLOCK_SIZE);
        let mut expected = [0u8; G17P_RENDER_POINTER_BLOCK_SIZE];
        expected[0x50..0x54].copy_from_slice(&u32::MAX.to_le_bytes());
        expected[0x60..0x64].copy_from_slice(&0x500u32.to_le_bytes());
        assert_eq!(&pointers[..G17P_RENDER_POINTER_BLOCK_SIZE], &expected);
        assert_eq!(&pointers[G17P_RENDER_POINTER_BLOCK_SIZE..], &expected);
    }

    #[test]
    fn user_render_optional_records_match_the_cold_opening_oracle() {
        fn decode(hex: &str) -> [u8; 0xc0] {
            fn nibble(byte: u8) -> u8 {
                match byte {
                    b'0'..=b'9' => byte - b'0',
                    b'a'..=b'f' => byte - b'a' + 10,
                    _ => panic!("invalid fixture"),
                }
            }
            let bytes = hex.as_bytes();
            assert_eq!(bytes.len(), 0x180);
            let mut out = [0u8; 0xc0];
            for (index, value) in out.iter_mut().enumerate() {
                *value = (nibble(bytes[2 * index]) << 4) | nibble(bytes[2 * index + 1]);
            }
            out
        }

        let tiling_oracle = decode("0f00000000000000000012007000000000041200700000000000010000000200000000000000010000000000000000000000010000000008120070000000000000000000000000000000000c1200700000000100000000000000150000000200000001000000010000000000000000001300700000000000000000000000000000000100000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000");
        let fragment_oracle = decode("0f00000000000000000012007000000000041200700000000100010000000200000001000000010000000000000000000000010000000008120070000000000000000000000000000000000c120070000000010000000000000015000000020000000100000001000000000000000000000000000000ffffffffffffffffffffffffffffffff00000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000");
        let mut tiling = [0xa5; 0xc0];
        let mut fragment = [0xa5; 0xc0];
        build_g17p_user_optional(&mut tiling, 0, true, 0x70_0012_0000, 0x70_0013_0000);
        build_g17p_user_optional(&mut fragment, 0, false, 0x70_0012_0000, 0x70_0013_0000);
        assert_eq!(tiling, tiling_oracle);
        assert_eq!(fragment, fragment_oracle);
    }

    #[test]
    fn user_render_descriptors_use_the_cold_opening_namespace() {
        fn u32_at(raw: &[u8], offset: usize) -> u32 {
            u32::from_le_bytes(raw[offset..offset + 4].try_into().unwrap())
        }
        fn u64_at(raw: &[u8], offset: usize) -> u64 {
            u64::from_le_bytes(raw[offset..offset + 8].try_into().unwrap())
        }
        let parameters = g17_render::G17pRenderParameters::source_rsrc8();
        let objects = g17_render::RenderDescriptorObjects {
            record_a: 0x70_0014_0000,
            shared: 0x70_0015_0000,
            record_b: 0x70_0016_0000,
            zero: 0x70_0017_0000,
        };
        let mut tiling = [0u8; g17_render::TA_DESCRIPTOR_SIZE];
        let mut fragment = [0u8; g17_render::FRAGMENT_DESCRIPTOR_SIZE];
        assert_eq!(
            g17_render::build_ta_descriptor(
                &parameters,
                objects,
                g17p_cold_opening_metadata(g17_render::RenderDescriptorKind::Tiling),
                &mut tiling,
            ),
            Ok(())
        );
        assert_eq!(
            g17_render::build_fragment_descriptor(
                &parameters,
                objects,
                g17p_cold_opening_metadata(g17_render::RenderDescriptorKind::Fragment),
                &mut fragment,
            ),
            Ok(())
        );

        assert_eq!(u64_at(&tiling, 0x04), 1);
        assert_eq!(u32_at(&tiling, 0x0c), 1);
        assert_eq!(u32_at(&tiling, 0x18), 0);
        assert_eq!(u64_at(&tiling, 0x28), objects.record_b);
        assert_eq!(u64_at(&tiling, 0x30), objects.zero);
        assert_eq!(u32_at(&tiling, 0x48), 0);
        assert_eq!(u32_at(&tiling, 0x304), 0);
        for offset in [0x370, 0x37c, 0x388] {
            assert_eq!(u32_at(&tiling, offset), 0x100);
        }

        assert_eq!(u64_at(&fragment, 0x04), 0);
        assert_eq!(u32_at(&fragment, 0x0c), 1);
        assert_eq!(u64_at(&fragment, 0x30), objects.record_b);
        assert_eq!(u64_at(&fragment, 0x38), objects.zero);
        assert_eq!(u32_at(&fragment, 0x458), 0);
        assert_eq!(u32_at(&fragment, 0x470), 0x100);
        assert_eq!(u32_at(&fragment, 0x47c), 0x100);
    }

    #[test]
    fn generic_opening_descriptor_tail_uses_retained_aliases() {
        let mut tiling = [0u8; g17_render::TA_DESCRIPTOR_SIZE];
        let mut fragment = [0u8; g17_render::FRAGMENT_DESCRIPTOR_SIZE];
        let overlapping_context_base = 0x10_0000_0000u64.to_le_bytes();
        fragment[0x1f4e..0x1f56].copy_from_slice(&overlapping_context_base);

        assert_eq!(
            apply_g17p_generic_descriptor_tail(&mut tiling, &mut fragment),
            Some(())
        );
        assert_eq!(get_u64(&tiling, 0x08a6), G17P_BOOTSTRAP_DESCRIPTOR_ZERO_A_VA);
        assert_eq!(get_u64(&tiling, 0x08ae), G17P_BOOTSTRAP_DESCRIPTOR_ZERO_B_VA);
        assert_eq!(get_u64(&tiling, 0x0945), G17P_BOOTSTRAP_TA_STATUS_VA);
        assert_eq!(
            get_u64(&fragment, 0x2140),
            G17P_BOOTSTRAP_DESCRIPTOR_ZERO_A_VA + 0x28
        );
        assert_eq!(
            get_u64(&fragment, 0x2148),
            G17P_BOOTSTRAP_DESCRIPTOR_ZERO_B_VA + 0x28
        );
        assert_eq!(get_u64(&fragment, 0x21df), G17P_BOOTSTRAP_3D_STATUS_VA);
        assert_eq!(
            &fragment[0x1f4e..0x1f56],
            &overlapping_context_base,
            "the native overlapping context-base view is already encoded elsewhere"
        );

        assert_eq!(
            G17P_BOOTSTRAP_TA_DESCRIPTOR_LOW_VA,
            g17_initdata::COMPUTE_RUNTIME_ZERO_BUFFER_0_ADDRESS
        );
        assert!(
            G17P_BOOTSTRAP_3D_DESCRIPTOR_LOW_VA
                >= g17_initdata::COMPUTE_RUNTIME_ZERO_BUFFER_0_ADDRESS
        );
        assert!(
            G17P_BOOTSTRAP_3D_DESCRIPTOR_LOW_VA + G17P_PAGE_SIZE as u64
                <= g17_initdata::COMPUTE_RUNTIME_ZERO_BUFFER_0_ADDRESS
                    + G17P_CONTROL_OPERAND_BUFFER_SIZE as u64
        );
        assert_eq!(
            G17P_BOOTSTRAP_3D_STATUS_VA,
            g17_initdata::COMPUTE_RUNTIME_STATE_ADDRESS
        );
    }

    #[test]
    fn user_render_state_sizes_follow_the_legacy_scene_geometry() {
        let parameters = g17_render::G17pRenderParameters::source_rsrc8();
        let layout = g17p_render_state_layout(&parameters, 2).unwrap();
        assert_eq!(layout.tilemap, 0x10000);
        assert_eq!(layout.tilemap_size, 0x500);
        assert_eq!(layout.heapmeta, 0x11000);
        assert_eq!(layout.heapmeta_size, 0x200);
        assert_eq!(layout.tpc, 0x14000);
        assert_eq!(layout.tpc_size, 0x1000);
        assert_eq!(layout.aux_fb, 0x18000);
        assert_eq!(layout.total_size, 0x20000);
        assert_eq!(G17P_RENDER_STATE_AUX_FB_SIZE, 0x8000);
        assert_eq!(G17P_RENDER_AUX_FB_PAGE_COUNT, 0x10_0000);
        assert_eq!(g17p_aux_fb_flags(0), 0xc001);
        assert_eq!(g17p_aux_fb_flags(G17P_RENDER_DBIAS_IS_INT), 0xc001);

        let mut runtime = vec![0xa5; layout.total_size];
        assert_eq!(
            encode_g17p_render_runtime_state(&mut runtime, &layout, 64, 64, 0),
            Ok(())
        );
        assert!(runtime[layout.aux_fb..layout.aux_fb + G17P_RENDER_STATE_AUX_FB_SIZE]
            .iter()
            .all(|byte| *byte == 0));

        let mut viewport = [0u8; G17P_PAGE_SIZE];
        assert_eq!(apply_g17p_viewport(64, 64, &mut viewport), Some(()));
        assert_eq!(
            u32::from_le_bytes(viewport[0x900..0x904].try_into().unwrap()),
            0xc00
        );
        assert_eq!(
            u32::from_le_bytes(viewport[0x904..0x908].try_into().unwrap()),
            0x8000_0001
        );
        assert_eq!(
            u32::from_le_bytes(viewport[0x908..0x90c].try_into().unwrap()),
            1
        );
        assert_eq!(
            u32::from_le_bytes(viewport[0x910..0x914].try_into().unwrap()),
            0x4200_0000
        );
        assert_eq!(
            u32::from_le_bytes(viewport[0x91c..0x920].try_into().unwrap()),
            0xc200_0000
        );
    }

    #[test]
    fn user_render_late_tiling_changes_only_the_six_held_ranges() {
        let graph_va = 0x70_0100_0000;
        let descriptor = 0x70_0101_0000;
        let addresses = g17_submission::G17PColdOpeningOptionalAddresses {
            context_scratch: 0x70_0102_0000,
            firmware_scratch: 0x70_0102_0000,
            usc_priv_mem_flist: 0x70_0102_4000,
            usc_freelist_hardware_buffer_id: 0,
            channel_control: 0x70_0102_8000,
            tiling_shared_object: 0x70_0102_c000,
            owner_pid: 0x15,
            ta_hardware_buffer_id: 0,
        };
        let mut graph = vec![0u8; G17P_RENDER_QUEUE_GRAPH_SIZE];
        let mut support = vec![0u8; G17P_RENDER_SUPPORT_SIZE];
        put_u32(&mut support, G17P_RENDER_POOL_A_SLOTS + 4, 1);
        put_u32(&mut support, G17P_RENDER_SHARED_CONTROL_INNER, 1);

        // An address-construction error occurs before any publication and
        // cannot change the early 3D-visible bytes.
        assert!(
            build_g17p_late_tiling_bytes(descriptor, u64::MAX, addresses, 6, 0, 1, false, false)
                .is_err()
        );
        assert!(graph[G17P_USER_TILING_RING..G17P_USER_TILING_RING + 24]
            .iter()
            .all(|byte| *byte == 0));
        assert_eq!(get_u32(&support, G17P_RENDER_POOL_A_SLOTS + 4), 1);
        assert_eq!(get_u32(&support, G17P_RENDER_SHARED_CONTROL_INNER), 1);

        let late =
            build_g17p_late_tiling_bytes(descriptor, graph_va, addresses, 6, 0, 1, false, false)
                .unwrap();
        late.apply(&mut graph, &mut support);
        assert_eq!(get_u64(&graph, G17P_USER_TILING_RING), descriptor);
        assert_eq!(
            get_u64(&graph, G17P_USER_TILING_RING + 8),
            graph_va + G17P_USER_TILING_OPTIONAL as u64
        );
        assert_eq!(
            get_u64(&graph, G17P_USER_TILING_RING + 16),
            graph_va + G17P_USER_TILING_EVENT as u64
        );
        assert_eq!(get_u32(&graph, G17P_USER_TILING_POINTERS + 0x40), 3);
        assert_eq!(get_u32(&graph, G17P_USER_TILING_OPTIONAL + 0x32), 6);
        assert_eq!(get_u32(&graph, G17P_USER_TILING_EVENT + 0x08), 0x102);
        assert_eq!(get_u32(&support, G17P_RENDER_POOL_A_SLOTS + 4), 2);
        assert_eq!(get_u32(&support, G17P_RENDER_SHARED_CONTROL_INNER), 2);

        let retained =
            build_g17p_late_tiling_bytes(descriptor, graph_va, addresses, 6, 1, 2, false, false)
                .unwrap();
        retained.apply(&mut graph, &mut support);
        assert_eq!(get_u32(&graph, G17P_USER_TILING_EVENT + 0x08), 0x202);
        assert_eq!(get_u32(&support, G17P_RENDER_POOL_A_SLOTS + 4 + 2 * 4), 2);
        assert_eq!(get_u32(&support, G17P_RENDER_SHARED_CONTROL_INNER), 4);

        let mut native_support = vec![0u8; G17P_RENDER_SUPPORT_SIZE];
        put_u32(&mut native_support, G17P_RENDER_POOL_A_SLOTS + 8, 1);
        let native_first =
            build_g17p_late_tiling_bytes(descriptor, graph_va, addresses, 6, 0, 1, false, true)
                .unwrap();
        native_first.apply(&mut graph, &mut native_support);
        assert_eq!(get_u32(&native_support, G17P_RENDER_POOL_A_SLOTS + 4), 0);
        assert_eq!(get_u32(&native_support, G17P_RENDER_POOL_A_SLOTS + 8), 2);
        let native_retained =
            build_g17p_late_tiling_bytes(descriptor, graph_va, addresses, 6, 1, 2, false, true)
                .unwrap();
        assert_eq!(native_retained.pool_a_slot_offset, retained.pool_a_slot_offset);
    }

    #[test]
    fn native_tiling_phases_hold_inner_and_pool_until_their_exact_turn() {
        let graph_va = 0x70_0100_0000;
        let descriptor = 0x70_0101_0000;
        let addresses = g17_submission::G17PColdOpeningOptionalAddresses {
            context_scratch: 0x70_0102_0000,
            firmware_scratch: 0x70_0102_0000,
            usc_priv_mem_flist: 0x70_0102_4000,
            usc_freelist_hardware_buffer_id: 0,
            channel_control: 0x70_0102_8000,
            tiling_shared_object: 0x70_0102_c000,
            owner_pid: 0x15,
            ta_hardware_buffer_id: 0,
        };
        let late =
            build_g17p_late_tiling_bytes(descriptor, graph_va, addresses, 6, 0, 1, true, false)
                .unwrap();
        let mut graph = vec![0u8; G17P_RENDER_QUEUE_GRAPH_SIZE];
        let mut support = vec![0u8; G17P_RENDER_SUPPORT_SIZE];
        put_u32(&mut support, G17P_RENDER_POOL_A_SLOTS + 4, 1);
        put_u32(&mut support, G17P_RENDER_SHARED_CONTROL_INNER, 1);

        // Pre-TA callback: no TA inner producer or group is visible.
        assert_eq!(get_u32(&graph, G17P_USER_TILING_POINTERS + 0x40), 0);
        assert_eq!(get_u64(&graph, G17P_USER_TILING_RING), 0);

        // Tag-15 prefix becomes visible before SKSM, but tag-14/tag-16 and the
        // late Pool-A transition remain hidden.
        late.apply_native_prefix(&mut graph);
        assert_eq!(get_u32(&graph, G17P_USER_TILING_POINTERS + 0x40), 2);
        assert_eq!(get_u64(&graph, G17P_USER_TILING_RING), descriptor);
        assert_eq!(get_u64(&graph, G17P_USER_TILING_RING + 16), 0);
        assert_eq!(get_u32(&support, G17P_RENDER_POOL_A_SLOTS + 4), 1);

        // The runtime publishes SKSM between these two pure byte phases.
        late.apply_native_kick(&mut graph);
        assert_eq!(get_u32(&graph, G17P_USER_TILING_POINTERS + 0x40), 4);
        assert_eq!(
            get_u64(&graph, G17P_USER_TILING_RING + 16),
            graph_va + G17P_USER_TILING_EVENT as u64
        );
        assert_eq!(get_u32(&graph, G17P_USER_TILING_EVENT + 0x08), 0x102);
        assert_eq!(get_u32(&support, G17P_RENDER_POOL_A_SLOTS + 4), 1);
        assert_eq!(get_u32(&support, G17P_RENDER_SHARED_CONTROL_INNER), 1);

        // The runtime advances the outer producer before this final phase.
        late.apply_native_pool_transition(&mut support);
        assert_eq!(get_u32(&support, G17P_RENDER_POOL_A_SLOTS + 4), 2);
        assert_eq!(get_u32(&support, G17P_RENDER_SHARED_CONTROL_INNER), 2);
    }

    #[test]
    fn retained_job_list_reset_requires_idle_queues_and_all_owned_timestamps() {
        let idle = g17_completion::QueueIndices {
            done: 3,
            read: 3,
            write: 3,
        };
        let active = g17_completion::QueueIndices {
            done: 2,
            read: 3,
            write: 3,
        };
        assert!(g17p_render_can_reset_job_list(idle, idle, [1, 2], [3, 4]));
        assert!(!g17p_render_can_reset_job_list(
            active,
            idle,
            [1, 2],
            [3, 4]
        ));
        assert!(!g17p_render_can_reset_job_list(idle, idle, [1, 0], [3, 4]));
    }

    #[test]
    fn deferred_fragment_publication_rejects_wrong_or_moved_outer_plan() {
        let mut plan = g17_submission::G17PPartialOpeningOuterSlotPlan {
            channel_table_index:
                g17_submission::G17P_PARTIAL_OPENING_FRAGMENT_CHANNEL_TABLE_INDEX,
            slot_index: 7,
            slot_byte_offset: 7 * g17_submission::G17P_PARTIAL_OPENING_OUTER_SLOT_SIZE as u32,
            slot: [0; g17_submission::G17P_PARTIAL_OPENING_OUTER_SLOT_SIZE],
            next_producer: 8,
        };
        let fragment_channel =
            g17_submission::G17P_PARTIAL_OPENING_FRAGMENT_CHANNEL_TABLE_INDEX;
        assert!(g17p_deferred_outer_plan_ready(
            &plan,
            fragment_channel,
            7
        ));
        assert!(!g17p_deferred_outer_plan_ready(
            &plan,
            fragment_channel,
            8
        ));

        plan.channel_table_index =
            g17_submission::G17P_PARTIAL_OPENING_TILING_CHANNEL_TABLE_INDEX;
        assert!(!g17p_deferred_outer_plan_ready(
            &plan,
            fragment_channel,
            7
        ));
        plan.channel_table_index =
            g17_submission::G17P_PARTIAL_OPENING_FRAGMENT_CHANNEL_TABLE_INDEX;
        plan.next_producer = 9;
        assert!(!g17p_deferred_outer_plan_ready(
            &plan,
            fragment_channel,
            7
        ));

        plan.slot_index = u8::MAX;
        plan.next_producer = 0;
        assert!(g17p_deferred_outer_plan_ready(
            &plan,
            fragment_channel,
            0xff
        ));

        plan.channel_table_index =
            g17_submission::G17P_PARTIAL_OPENING_TILING_CHANNEL_TABLE_INDEX;
        plan.slot_index = 11;
        plan.next_producer = 12;
        assert!(g17p_deferred_outer_plan_ready(
            &plan,
            g17_submission::G17P_PARTIAL_OPENING_TILING_CHANNEL_TABLE_INDEX,
            11
        ));
        assert!(!g17p_deferred_outer_plan_ready(
            &plan,
            fragment_channel,
            11
        ));
    }

    #[test]
    fn user_render_support_names_fixed_context_peers_and_channel_record_zero() {
        assert_eq!(G17P_RENDER_SUPPORT_SIZE, 0x40000);
        let support_va = 0x70_0100_0000;
        let render_channel_control = support_va + G17P_RENDER_CHANNEL_CONTROL as u64;
        let shared_object = support_va + G17P_RENDER_PACKED_SHARED as u64;
        let mut support = vec![0u8; G17P_RENDER_SUPPORT_SIZE];
        let tiling_optional =
            g17p_cold_opening_optional_addresses(support_va, shared_object, 9, true);
        let fragment_optional =
            g17p_cold_opening_optional_addresses(support_va, shared_object, 9, false);
        assert_eq!(
            tiling_optional.usc_priv_mem_flist,
            g17_initdata::PARTIAL_OPENING_CONTROL_SHARED_ADDRESS
        );
        assert_eq!(
            fragment_optional.usc_priv_mem_flist,
            g17_initdata::PARTIAL_OPENING_CONTROL_SHARED_ADDRESS
        );
        assert_eq!(
            tiling_optional.context_scratch,
            G17P_BOOTSTRAP_TA_CONTEXT_LOW_VA
        );
        assert_eq!(
            tiling_optional.firmware_scratch,
            G17P_BOOTSTRAP_TA_CONTEXT_HIGH_VA
        );
        assert_eq!(
            fragment_optional.context_scratch,
            G17P_BOOTSTRAP_3D_CONTEXT_LOW_VA
        );
        assert_eq!(
            fragment_optional.firmware_scratch,
            G17P_BOOTSTRAP_3D_CONTEXT_HIGH_VA
        );
        assert_eq!(tiling_optional.tiling_shared_object, shared_object);
        assert_eq!(fragment_optional.tiling_shared_object, shared_object);
        assert_eq!(tiling_optional.ta_hardware_buffer_id, 9);
        assert_eq!(tiling_optional.channel_control, render_channel_control);
        assert_eq!(fragment_optional.channel_control, render_channel_control);
        assert_eq!(G17P_BOOTSTRAP_CONTEXT_PEER_SIZE, 8 * G17P_PAGE_SIZE);
        g17_submission::apply_g17p_cold_opening_channel_control(
            &mut support[G17P_RENDER_CHANNEL_CONTROL
                ..G17P_RENDER_CHANNEL_CONTROL
                    + g17_submission::G17P_COLD_OPENING_CHANNEL_CONTROL_SIZE],
        )
        .unwrap();
        let pointed = (render_channel_control - support_va) as usize;
        assert_eq!(get_u64(&support, pointed), 0x0000_0100_0000_ffff);
        assert_eq!(get_u64(&support, pointed + 0x20), 0x0002_0000_0000_0000);
        assert_eq!(get_u64(&support, pointed + 0x30), 0x0000_0000_ff00_0000);
    }

    #[test]
    fn render_operand_ranges_match_the_captured_sparse_layout() {
        let directory = G17P_CONTROL_DIRECTORY_VA
            ..G17P_CONTROL_DIRECTORY_VA + G17P_CONTROL_DIRECTORY_SIZE as u64;
        assert_eq!(G17P_CONTROL_DIRECTORY_SIZE, 0x20_0000);
        assert_eq!(directory.end, 0x70_0020_0000);
        let table = G17P_CONTROL_OPERAND_TABLE_VA
            ..G17P_CONTROL_OPERAND_TABLE_VA
                + g17_initdata::CONTROL_OPERAND_TABLE_SIZE as u64;
        for range in [&directory, &table] {
            assert!(G17P_CL_LOW_VA_START <= range.start);
            assert!(range.end <= G17P_CL_LOW_VA_END);
        }
        assert!(directory.end <= table.start);
        assert!(table.end <= G17P_CONTROL_OPERAND_BUFFER_BASE);

        let mut previous_end = None;
        for index in 0..G17P_CONTROL_OPERAND_BUFFER_COUNT {
            let start = g17p_control_operand_buffer_va(index).unwrap();
            let end = start + G17P_CONTROL_OPERAND_BUFFER_SIZE as u64;
            assert_eq!(
                start,
                G17P_CONTROL_OPERAND_BUFFER_BASE
                    + index as u64 * G17P_CONTROL_OPERAND_BUFFER_STRIDE as u64
            );
            assert!(G17P_CL_LOW_VA_START <= start);
            assert!(end <= G17P_CL_LOW_VA_END);
            if let Some(previous_end) = previous_end {
                assert_eq!(start - previous_end, 0x8000);
            }
            previous_end = Some(end);
        }
        assert_eq!(
            g17p_control_operand_buffer_va(G17P_CONTROL_OPERAND_BUFFER_COUNT),
            None
        );
        assert!(
            g17_initdata::CONTROL_OPERAND_ENTRY_COUNT <= G17P_CONTROL_OPERAND_BUFFER_COUNT
        );
        assert_eq!(g17_initdata::COMPUTE_READINESS_OPERAND_ENTRY_COUNT, 21);
        let readiness_last =
            g17p_control_operand_buffer_va(g17_initdata::COMPUTE_READINESS_OPERAND_ENTRY_COUNT - 1)
                .unwrap();
        let first_opening_only =
            g17p_control_operand_buffer_va(g17_initdata::COMPUTE_READINESS_OPERAND_ENTRY_COUNT)
                .unwrap();
        assert_eq!(readiness_last, 0x70_016c_0000);
        assert_eq!(first_opening_only, 0x70_017c_8000);
        assert!(
            readiness_last + G17P_CONTROL_OPERAND_BUFFER_SIZE as u64 <= first_opening_only
        );

        // These are B2 aliases only in context 0. They fall inside render
        // operand slot 21, so the layouts cannot share one low root.
        let slot_21 = g17p_control_operand_buffer_va(21).unwrap();
        assert_eq!(slot_21, first_opening_only);
        let slot_21_end = slot_21 + G17P_CONTROL_OPERAND_BUFFER_SIZE as u64;
        for b2 in g17_initdata::REGION_VIEW_LOW_ADDRS {
            assert!(slot_21 <= b2 && b2 < slot_21_end);
        }
        assert_eq!(G17P_BOOTSTRAP_CONTEXT_BASE, 0x10_0000_0000);
        assert_eq!(G17P_BOOTSTRAP_CONTEXT_SIZE, 0x1_0000);
        assert_eq!(G17P_BOOTSTRAP_VDM_VA, 0x10_0001_8000);
        assert_eq!(G17P_BOOTSTRAP_VDM_SIZE, 0x8000);
        assert!(
            G17P_BOOTSTRAP_CONTEXT_BASE + G17P_BOOTSTRAP_CONTEXT_SIZE as u64
                <= G17P_BOOTSTRAP_VDM_VA
        );
        assert!(G17P_BOOTSTRAP_VDM_VA + G17P_BOOTSTRAP_VDM_SIZE as u64 <= directory.start);
    }

    #[test]
    fn compute_runtime_closure_uses_source_ranges_and_phase_reuses_peers() {
        for index in 0..18 {
            assert_eq!(
                g17_initdata::COMPUTE_RUNTIME_BUFFER_ADDRESSES[index],
                g17p_control_operand_buffer_va(index).unwrap()
            );
        }
        assert_eq!(
            g17_initdata::COMPUTE_RUNTIME_BUFFER_ADDRESSES[18..],
            [
                g17_initdata::COMPUTE_RUNTIME_ZERO_BUFFER_0_ADDRESS,
                g17_initdata::COMPUTE_RUNTIME_ZERO_BUFFER_1_ADDRESS,
                g17p_control_operand_buffer_va(18).unwrap(),
            ]
        );
        assert_eq!(
            g17_initdata::COMPUTE_RUNTIME_PAGE_LIST_ADDRESS,
            g17p_control_operand_buffer_va(22).unwrap()
        );
        let buffer_23_end = g17p_control_operand_buffer_va(23).unwrap()
            + G17P_CONTROL_OPERAND_BUFFER_SIZE as u64;
        assert_eq!(
            g17_initdata::COMPUTE_RUNTIME_CLASS1_TABLE_ADDRESS,
            buffer_23_end
        );
        assert!(
            g17_initdata::COMPUTE_RUNTIME_CLASS1_TABLE_ADDRESS
                + g17_initdata::COMPUTE_RUNTIME_CLASS1_TABLE_SIZE as u64
                <= g17p_control_operand_buffer_va(24).unwrap()
        );

        // Operand buffer 2 deliberately reuses both generic bootstrap peer
        // ranges. Its context-0 alias must therefore be installed only after
        // the startup render storage and its two peer aliases are dropped.
        let buffer_2 = g17p_control_operand_buffer_va(2).unwrap();
        let buffer_2_end = buffer_2 + G17P_CONTROL_OPERAND_BUFFER_SIZE as u64;
        for peer in [
            G17P_BOOTSTRAP_TA_CONTEXT_LOW_VA,
            G17P_BOOTSTRAP_3D_CONTEXT_LOW_VA,
        ] {
            assert!(buffer_2 <= peer);
            assert!(peer + G17P_BOOTSTRAP_CONTEXT_PEER_SIZE as u64 <= buffer_2_end);
        }
    }

    #[test]
    fn add3_resource_table_occupies_the_hardware_proven_preempt_offset() {
        let buffers = [
            0x100_0003_0000,
            0x100_0003_8000,
            0x100_0004_0000,
        ];
        let mut raw = vec![0u8; G17P_COMPUTE_PREEMPT_SIZE];
        assert_eq!(apply_g17p_add3_resource_table(&mut raw, buffers), Some(()));
        for (slot, address) in buffers.into_iter().enumerate() {
            assert_eq!(
                get_u64(
                    &raw,
                    G17P_COMPUTE_ADD3_RESOURCE_TABLE + slot * core::mem::size_of::<u64>(),
                ),
                address
            );
        }
        assert_eq!(
            apply_g17p_add3_resource_table(
                &mut raw[..G17P_COMPUTE_ADD3_RESOURCE_TABLE + 23],
                buffers,
            ),
            None
        );
    }

    #[test]
    fn control_opening_requires_all_six_exact_counter_values() {
        let staged = G17PControlCounters {
            primary: [0, 0, g17_initdata::CONTROL_OPENING_PRIMARY_PRODUCER],
            secondary: [0, 0, 16],
        };
        assert!(staged.opening_staged());
        assert!(!staged.opening_retired());

        let retired = G17PControlCounters {
            primary: [g17_initdata::CONTROL_OPENING_PRIMARY_PRODUCER; 3],
            secondary: [16, 16, 16],
        };
        assert!(retired.opening_retired());
        assert!(!retired.opening_staged());

        for primary_index in 0..3 {
            let mut current = retired;
            current.primary[primary_index] -= 1;
            assert!(!current.opening_retired());
        }
        for secondary_index in 0..3 {
            let mut current = retired;
            current.secondary[secondary_index] -= 1;
            assert!(!current.opening_retired());
        }

        let runtime = G17PControlCounters {
            primary: [g17_initdata::COMPUTE_RUNTIME_CONTROL_FINAL_COUNTER; 3],
            secondary: [16, 16, 16],
        };
        assert!(runtime.compute_runtime_ready());
        assert!(!runtime.opening_staged());
        assert!(!runtime.opening_retired());

        let first_readiness_staged = G17PControlCounters {
            primary: [
                g17_initdata::CONTROL_OPENING_PRIMARY_PRODUCER,
                g17_initdata::CONTROL_OPENING_PRIMARY_PRODUCER,
                g17_initdata::COMPUTE_READINESS_FINAL_COUNTER,
            ],
            secondary: [16, 16, 16],
        };
        assert!(!first_readiness_staged.compute_readiness_record_staged(
            g17_initdata::CONTROL_OPENING_PRIMARY_PRODUCER + 1,
        ));
        assert!(!first_readiness_staged.compute_readiness_record_retired(
            g17_initdata::COMPUTE_READINESS_FINAL_COUNTER,
        ));
        for target in g17_initdata::CONTROL_OPENING_PRIMARY_PRODUCER + 1
            ..=g17_initdata::COMPUTE_READINESS_FINAL_COUNTER
        {
            let staged = G17PControlCounters {
                primary: [target - 1, target - 1, target],
                secondary: [16, 16, 16],
            };
            assert!(staged.compute_readiness_record_staged(target));
            assert!(!staged.compute_readiness_record_retired(target));

            let retired = G17PControlCounters {
                primary: [target; 3],
                secondary: [16, 16, 16],
            };
            assert!(retired.compute_readiness_record_retired(target));
            assert!(!retired.compute_readiness_record_staged(target));
        }
        let readiness_retired = G17PControlCounters {
            primary: [g17_initdata::COMPUTE_READINESS_FINAL_COUNTER; 3],
            secondary: [16, 16, 16],
        };
        assert!(readiness_retired.compute_readiness_retired());
        assert!(readiness_retired.compute_readiness_record_retired(
            g17_initdata::COMPUTE_READINESS_FINAL_COUNTER,
        ));
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
    fn native_private_cluster_covers_the_observed_gfx1_fault() {
        let cluster = KERN + G17P_NATIVE_PRIVATE_CLUSTER_OFFSET
            ..KERN
                + G17P_NATIVE_PRIVATE_CLUSTER_OFFSET
                + G17P_NATIVE_PRIVATE_CLUSTER_SIZE as u64;
        assert_eq!(cluster.start, 0xffff_fc20_0002_0000);
        assert_eq!(cluster.end, 0xffff_fc20_0019_8000);
        assert!(cluster.contains(&0xffff_fc20_0017_c004));
    }

    #[test]
    fn status_a_uses_the_page_rounded_native_extent() {
        let cluster_end = G17P_NATIVE_PRIVATE_CLUSTER_OFFSET
            + G17P_NATIVE_PRIVATE_CLUSTER_SIZE as u64;
        let source_extent = cluster_end - G17P_NATIVE_SECONDARY_STATUS_A_OFFSET;
        assert_eq!(source_extent, 0xb4f00);
        assert_eq!(STATUS_A_ALLOC_SIZE, 0xb8000);
        assert!(0xb4004 < STATUS_A_ALLOC_SIZE);
        assert!(STATUS_A_ALLOC_SIZE as u64 >= source_extent);
        assert_eq!(STATUS_A_ALLOC_SIZE % G17P_PAGE_SIZE, 0);
    }

    #[test]
    fn status_a_scan_and_runtime_words_are_exact() {
        assert_eq!(PRIMARY_STATUS_A_STATE_GRID_OFFSET, 0xee40);
        assert_eq!(PRIMARY_STATUS_A_WORK_SCAN_OFFSET, 0x0c);
        assert_eq!(PRIMARY_STATUS_A_RUNTIME_OFFSET, 0x10);
        assert_eq!(PRIMARY_STATUS_A_WORK_SCAN_STATE_GRID_OFFSET, 0xee4c);
        assert_eq!(PRIMARY_STATUS_A_RUNTIME_STATE_GRID_OFFSET, 0xee50);
        let scan_address = KERN + PRIMARY_STATUS_A_WORK_SCAN_STATE_GRID_OFFSET;
        let address = KERN + PRIMARY_STATUS_A_RUNTIME_STATE_GRID_OFFSET;
        let snapshot = G17PStatusASnapshot::decode(
            scan_address,
            0x55aa_55aa,
            address,
            0xaabb_ccdd_1122_3344,
        );
        assert_eq!(snapshot.scan_address, scan_address);
        assert_eq!(snapshot.scan_active, 0x55aa_55aa);
        assert_eq!(snapshot.address, address);
        assert_eq!(snapshot.raw, 0xaabb_ccdd_1122_3344);
        assert_eq!(snapshot.active, 0x1122_3344);
        assert_eq!(snapshot.scheduler_constructed, 0xaabb_ccdd);
    }

    #[test]
    fn firmware_recovery_handshake_uses_exact_coherent_status_offsets() {
        assert_eq!(PRIMARY_STATUS_A_POWER_CALLBACK_COUNT_OFFSET, 0x1c);
        assert_eq!(PRIMARY_STATUS_B_FIRMWARE_RECOVERY_EPOCH_OFFSET, 0x48f0);
        assert_eq!(PRIMARY_STATUS_B_FIRMWARE_RECOVERY_STATE_OFFSET, 0x4900);

        let status_a = KERN + 0xee40;
        let status_b = KERN + 0x340;
        let snapshot = G17PFirmwareRecoveryHandshakeSnapshot::decode(
            status_b,
            status_b + 0x48f0,
            7,
            status_b + 0x4900,
            3,
            status_a + 0x1c,
            2,
        );
        assert_eq!(snapshot.status_b, status_b);
        assert_eq!(snapshot.epoch_address, status_b + 0x48f0);
        assert_eq!(snapshot.epoch, 7);
        assert_eq!(snapshot.state_address, status_b + 0x4900);
        assert_eq!(snapshot.state, 3);
        assert_eq!(snapshot.power_callback_count_address, status_a + 0x1c);
        assert_eq!(snapshot.power_callback_count, 2);
        assert!(!snapshot.waiting_for_first_response());
        assert!(snapshot.waiting_for_final_clear());
        assert!(snapshot.callback_known_unregistered());
        let between_waits = G17PFirmwareRecoveryHandshakeSnapshot::decode(
            status_b,
            status_b + 0x48f0,
            7,
            status_b + 0x4900,
            2,
            status_a + 0x1c,
            2,
        );
        assert!(between_waits.callback_known_unregistered());
    }

    #[test]
    fn render_firmware_items_and_client_state_use_distinct_vms() {
        let firmware = KERN..KERN + 0x20_0000;
        let client = 0x70_0000_0000..0x71_0000_0000;
        let layout = G17PRenderStorageVaLayout {
            descriptors: KERN + 0x10000,
            graph: KERN + 0x20000,
            support: KERN + 0x30000,
            render_state: 0x70_0020_0000,
            render_state_size: 0x20000,
            timestamps: KERN + 0x40000,
        };
        let covers = |range: &core::ops::Range<u64>, address: u64, size: u64| {
            address >= range.start
                && address
                    .checked_add(size)
                    .is_some_and(|end| end <= range.end)
        };
        assert!(layout.all_referenced_ranges_reachable(
            |address, size| covers(&firmware, address, size),
            |address, size| covers(&client, address, size),
        ));

        let low_descriptor = G17PRenderStorageVaLayout {
            descriptors: 0x70_0020_0000,
            ..layout
        };
        assert!(!low_descriptor.all_referenced_ranges_reachable(
            |address, size| covers(&firmware, address, size),
            |address, size| covers(&client, address, size),
        ));

        let low_timestamps = G17PRenderStorageVaLayout {
            timestamps: 0x70_0030_0000,
            ..layout
        };
        assert!(!low_timestamps.all_referenced_ranges_reachable(
            |address, size| covers(&firmware, address, size),
            |address, size| covers(&client, address, size),
        ));
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
