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
    g17_completion, g17_compute, g17_render, g17_submission, g17_uapi, gem, mmu,
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
use core::ops::Range;

use g17_initdata::{InstanceAddresses, InstanceRole, SharedAddresses};

/// Nonzero object IDs shared by pool owners and render commands.
/// Keep IDs unique across retained graphs, clients and firmware resets.
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
fn g17p_allocate_object_id() -> Result<u32> {
    // Metal reserves 0x01000000 IDs for userspace allocation. Linux has no
    // userspace GID allocator: a pool or lifecycle predecessor needs just one
    // ID. Reserving the whole unused range here exhausts the 32-bit namespace
    // after about 255 render graphs and permanently refuses new clients.
    let (id, _) = g17p_render_gid_add(1)?;
    if id == 0 {
        Err(EOVERFLOW)
    } else {
        Ok(id)
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
const G17P_RENDER_CHANNEL_CONTROL: usize = 0x2c000;

const G17P_CONTROL_DIRECTORY_VA: u64 = 0x70_0000_0000;
// The positive first-partial render root owns 128 16-KiB leaves here. The
// first failed Linux group proved that a 64-KiB placeholder is insufficient:
// GFX writes the fifth leaf at 0x7000010000 during channel-8 startup.
const G17P_CONTROL_DIRECTORY_SIZE: usize = 0x20_0000;
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
const G17P_CONTROL_OPERAND_BUFFER_BASE: u64 = g17_initdata::CONTROL_OPERAND_BUFFER_BASE;
const G17P_CONTROL_OPERAND_BUFFER_COUNT: usize = 28;
const G17P_CONTROL_OPERAND_BUFFER_SIZE: usize =
    g17_initdata::CONTROL_OPERAND_BUFFER_SIZE as usize;
const G17P_CONTROL_OPERAND_BUFFER_STRIDE: usize =
    g17_initdata::CONTROL_OPERAND_BUFFER_STRIDE as usize;
const G17P_BOOTSTRAP_TA_DESCRIPTOR_LOW_VA: u64 = 0x70_0000_0000;
const G17P_BOOTSTRAP_3D_DESCRIPTOR_LOW_VA: u64 = 0x70_0009_8000;
const G17P_BOOTSTRAP_DESCRIPTOR_ZERO_A_VA: u64 = 0xffff_fc20_001c_8000;
const G17P_BOOTSTRAP_DESCRIPTOR_ZERO_B_VA: u64 = 0xffff_fc20_c07c_0000;
const G17P_BOOTSTRAP_TA_STATUS_VA: u64 = 0xffff_fc20_0161_0000;
const G17P_BOOTSTRAP_3D_STATUS_VA: u64 = 0xffff_fc20_0163_0000;
const G17P_BOOTSTRAP_CONTEXT_PEER_SIZE: usize = 0x20_000;
const G17P_BOOTSTRAP_TA_CONTEXT_LOW_VA: u64 = 0x70_0043_8000;
const G17P_BOOTSTRAP_TA_CONTEXT_HIGH_VA: u64 = 0xffff_fc20_001d_8000;
const G17P_BOOTSTRAP_3D_CONTEXT_LOW_VA: u64 = 0x70_0046_0000;
const G17P_BOOTSTRAP_3D_CONTEXT_HIGH_VA: u64 = 0xffff_fc20_0020_0000;

fn g17p_control_operand_buffer_va(index: usize) -> Option<u64> {
    if index >= G17P_CONTROL_OPERAND_BUFFER_COUNT {
        return None;
    }
    G17P_CONTROL_OPERAND_BUFFER_BASE
        .checked_add(index as u64 * G17P_CONTROL_OPERAND_BUFFER_STRIDE as u64)
}

fn all_ranges_covered(
    ranges: impl IntoIterator<Item = (u64, u64)>,
    mut covers: impl FnMut(u64, u64) -> bool,
) -> bool {
    ranges
        .into_iter()
        .all(|(address, size)| covers(address, size))
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
        ]
        .into_iter()
        .all(|(address, size)| firmware_covers(address, size))
            && [
                (self.render_state, self.render_state_size),
                (self.timestamps, G17P_PAGE_SIZE as u64),
            ]
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
const G17P_USER_TILING_RING: usize = 0x4000;

fn page_align(value: usize) -> Option<usize> {
    value
        .checked_add(G17P_PAGE_SIZE - 1)
        .map(|value| value & !(G17P_PAGE_SIZE - 1))
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
struct G17PRenderStateLayout {
    layermeta: usize,
    layermeta_size: usize,
    heapmeta: usize,
    heapmeta_size: usize,
    tpc: usize,
    tpc_size: usize,
    aux_fb: usize,
    tilemap: usize,
    tilemap_size: usize,
    total_size: usize,
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
    // AGXRenderTargetMemoryG16::halComputeGeometry: a layered target carries
    // 0x100 bytes of per-layer tiler metadata beside the 0x200-byte heap
    // metadata, which every target carries.
    let layermeta_size = if layers > 1 { 0x100usize } else { 0 };
    let heapmeta_size = 0x200usize;

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
    // G17P does not derive the initial parameter-buffer pool solely from this
    // command's tile count. halGetDefaultParameterBufferSizeMin supplies a
    // chip-global 32-block floor, and the valid cold HWPB publishes exactly
    // 32 blocks / 128 pages even before its first render. Linux's inherited
    // eight-block floor left the TA with one quarter of the native pool.
    if tvb_blocks < G17P_PM_BLOCK_COUNT {
        tvb_blocks = G17P_PM_BLOCK_COUNT;
    }
    let tvb_size = tvb_blocks
        .checked_sub(1)?
        .checked_mul(G17P_RENDER_TVB_BLOCK_STRIDE)?
        .checked_add(G17P_RENDER_TVB_BLOCK_SIZE)?;

    // Native's one RT-memory allocation places the tilemap first, then the
    // layer metadata at the next 4-KiB subrange and the heap metadata directly
    // behind it (AGXRenderTargetMemory::updateRenderTarget, +0x2a8/+0x2b0/
    // +0x2b8). For the launchgate triangle that is exactly TMAP+0x0000 and
    // HMTA+0x1000 inside one 16-KiB UAT page, because a single layer has no
    // layer metadata. The previous inherited layout reversed them and
    // separated them by 0x10000, even though the 3D fixed-function path
    // consumes their native relationship directly.
    let tilemap = G17P_RENDER_STATE_DYNAMIC;
    let tilemap_span = tilemap_size.checked_add(0xfff)? & !0xfff;
    let layermeta = tilemap.checked_add(tilemap_span)?;
    let heapmeta = layermeta.checked_add(layermeta_size)?;
    let tpc = page_align(heapmeta.checked_add(heapmeta_size)?)?;
    // TMAP/HMTA rotate in `G17PRenderTargetMemory::rt_memory` and TPC has its
    // own stable object. They no longer occupy `render_state`; only their
    // sizes and relative RT-record offsets remain in this layout. AXFB is the
    // final region actually backed by render_state and therefore has one
    // fixed offset/capacity for the graph's whole lifetime.
    let aux_fb = G17P_RENDER_STATE_AUX_FB;
    let after_tilemap = tilemap.checked_add(page_align(tilemap_size)?)?;
    let after_aux_fb = aux_fb.checked_add(G17P_RENDER_STATE_AUX_FB_SIZE)?;
    let (pm_scene_scratch, pm_discard, tvb, total_size) = if g17p_render_tvb_enabled() {
        let scene_scratch = g17p_pm_page_align(after_aux_fb.max(after_tilemap))?;
        let discard = scene_scratch.checked_add(G17P_PM_SCENE_SCRATCH_SIZE)?;
        let tvb = discard.checked_add(G17P_PM_DISCARD_SIZE)?;
        // TVB backing lives in a separate compact-GART allocation. Keep the
        // offset as the end of the render-state object, but do not duplicate
        // the potentially hundreds-of-megabytes TVB inside that object.
        (scene_scratch, discard, tvb, after_aux_fb)
    } else {
        (after_aux_fb, after_aux_fb, after_aux_fb, after_aux_fb)
    };
    Some(G17PRenderStateLayout {
        tvb,
        tvb_size,
        tvb_blocks,
        pm_scene_scratch,
        pm_discard,
        layermeta,
        layermeta_size,
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

fn g17p_aux_fb_flags(command_flags: u32) -> u64 {
    0xc000 | u64::from(command_flags & G17P_RENDER_DBIAS_IS_INT != 0)
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

/// Fill the nonzero generic-pair tail pointers used by the full 4/16
/// opening. The two descriptor self-pointers are installed separately by
/// `apply_g17p_retained_descriptor_fields` because they live in context 0.
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
    put_u64(fragment, 0x2140, G17P_BOOTSTRAP_DESCRIPTOR_ZERO_A_VA + 4);
    put_u64(fragment, 0x2148, G17P_BOOTSTRAP_DESCRIPTOR_ZERO_B_VA + 4);
    put_u64(fragment, 0x21df, G17P_BOOTSTRAP_3D_STATUS_VA);
    Some(())
}

fn g17p_cold_opening_optional_addresses(
    support: u64,
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
        shared_control: g17_initdata::PARTIAL_OPENING_CONTROL_SHARED_ADDRESS,
        channel_control: support + G17P_RENDER_CHANNEL_CONTROL as u64,
        tiling_shared_object: support + G17P_RENDER_PACKED_SHARED as u64,
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
    put_u32(body, 0x40, 2);
    put_u32(body, 0x44, u32::MAX);
    put_u32(body, 0x48, 0x15);
    put_u64(body, 0x94, 0);
    put_u64(body, 0x9c, context);
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
        queue_pair: 0,
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

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) struct G17PControlOpeningEffect {
    pub(crate) shared_cursor: u32,
    pub(crate) inner_state: u32,
    pub(crate) operand_slot: u64,
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
struct G17PRenderTargetMemory {
    rt_memory: MappedObject,
    tpc: MappedObject,
    rt_slot_ordinals: [Option<u32>; G17P_RENDER_RT_EVENT_RECORD_COUNT],
}

#[cfg(not(test))]
impl G17PRenderTargetMemory {
    fn prepare_rt_slot(
        &mut self,
        submission_ordinal: u32,
        layout: &G17PRenderStateLayout,
    ) -> Result<G17PRenderTargetAddresses> {
        let slot = submission_ordinal as usize % G17P_RENDER_RT_EVENT_RECORD_COUNT;
        if self.rt_slot_ordinals[slot].is_some_and(|previous| {
            submission_ordinal
                .checked_sub(previous)
                .is_none_or(|distance| {
                    distance != 0 && distance < G17P_RENDER_RT_EVENT_RECORD_COUNT as u32
                })
        }) {
            return Err(EBUSY);
        }
        let used = layout
            .heapmeta
            .checked_add(layout.heapmeta_size)
            .and_then(|end| end.checked_sub(layout.tilemap))
            .ok_or(EINVAL)?;
        if used > G17P_RENDER_RT_REGION_STRIDE {
            return Err(ERANGE);
        }
        let offset = g17p_render_rt_record_offset(submission_ordinal);
        self.rt_memory.with_bytes_mut(|raw| {
            raw.get_mut(offset..offset + G17P_RENDER_RT_REGION_STRIDE)
                .ok_or(ERANGE)?
                .fill(0);
            Ok(())
        })?;
        self.rt_slot_ordinals[slot] = Some(submission_ordinal);
        Ok(G17PRenderTargetAddresses {
            tilemap: self.tilemap_va(submission_ordinal)?,
            layermeta: self.rt_region_va(submission_ordinal, layout, layout.layermeta)?,
            heapmeta: self.heapmeta_va(submission_ordinal, layout)?,
        })
    }

    fn tpc_va(&self) -> u64 {
        self.tpc.iova()
    }

    fn tilemap_va(&self, submission_ordinal: u32) -> Result<u64> {
        self.rt_memory
            .iova()
            .checked_add(g17p_render_rt_record_offset(submission_ordinal) as u64)
            .ok_or(EOVERFLOW)
    }

    fn rt_region_va(
        &self,
        submission_ordinal: u32,
        layout: &G17PRenderStateLayout,
        region: usize,
    ) -> Result<u64> {
        self.tilemap_va(submission_ordinal)?
            .checked_add(region.checked_sub(layout.tilemap).ok_or(EINVAL)? as u64)
            .ok_or(EOVERFLOW)
    }

    fn heapmeta_va(
        &self,
        submission_ordinal: u32,
        layout: &G17PRenderStateLayout,
    ) -> Result<u64> {
        self.rt_region_va(submission_ordinal, layout, layout.heapmeta)
    }
}

/// The addresses one render-target-memory record publishes to TA and 3D.
#[cfg(not(test))]
#[derive(Debug, Copy, Clone)]
struct G17PRenderTargetAddresses {
    tilemap: u64,
    layermeta: u64,
    heapmeta: u64,
}

/// Keep the comparatively large owning mappings out of
/// `G17PUserRenderStorage::new`'s already deep construction frame. Descriptor
/// dual-root collision retries can synchronously drop a temporary mapping and
/// enter the page-table walker; retaining another full `MappedObject` local in
/// that call chain exhausted the 32-KiB kernel stack on the second client.
#[cfg(not(test))]
#[inline(never)]
fn new_g17p_render_target_memory(
    dev: &AsahiDevice,
    vm: &mmu::Vm,
    queue_pair: G17PRenderQueuePair,
) -> Result<KBox<G17PRenderTargetMemory>> {
    let rt_memory = MappedObject::new_wc_at(
        dev,
        vm,
        g17p_render_rt_memory_va(queue_pair).ok_or(EINVAL)?,
        G17P_RENDER_RT_MEMORY_PAIR_SIZE,
        mmu::PROT_GPU_FW_SHARED_RW,
    )?;
    let tpc = MappedObject::new_wc_at(
            dev,
            vm,
            g17p_render_tpc_va(queue_pair).ok_or(EINVAL)?,
            G17P_RENDER_TPC_CAPACITY,
            mmu::PROT_GPU_FW_SHARED_RW,
        )?;
    Ok(KBox::new(
        G17PRenderTargetMemory {
            rt_memory,
            tpc,
            rt_slot_ordinals: [None; G17P_RENDER_RT_EVENT_RECORD_COUNT],
        },
        GFP_KERNEL,
    )?)
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

/// One Linux-owned paired render graph retained through synchronous
/// completion. The queue records, pointer blocks, item-address rings, shared
/// job list, descriptors, optional records, events, and timestamp words all
/// live in these mappings.
#[cfg(not(test))]
pub(crate) struct G17PUserRenderStorage {
    descriptors: MappedObject,
    descriptor_ta_high: mmu::KernelMapping,
    descriptor_3d_high: mmu::KernelMapping,
    descriptor_ta_low: mmu::KernelMapping,
    descriptor_3d_low: mmu::KernelMapping,
    ta_context_low: mmu::KernelMapping,
    fragment_context_low: mmu::KernelMapping,
    graph: MappedObject,
    support: MappedObject,
    render_state: MappedObject,
    render_ta_status: Option<mmu::KernelMapping>,
    render_3d_status: Option<mmu::KernelMapping>,
    timestamps: MappedObject,
    fragment_mcache: Option<g17_submission::G17PClMcacheAperture>,
    tiling_mcache: Option<g17_submission::G17PClMcacheAperture>,
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
    /// Retained staging image for the descriptor pair built by
    /// `restage_command`.
    ///
    /// This buffer is pure scratch. Its GPU address is never written into a
    /// descriptor, never published to firmware and never read by the RCE: the
    /// function builds the TA and 3D records in it and then copies both into
    /// the retained `descriptors` object, which is the storage the hardware
    /// actually addresses. Allocating, mapping, zeroing and unmapping 32 KiB
    /// of write-combining memory for that on every single submission is a page
    /// table update, a TLB maintenance operation and a VA-allocator round trip
    /// per frame, for a buffer whose contents are fully rewritten each time.
    /// Retaining it and zeroing it in place is byte-for-byte the same input to
    /// the descriptor builders -- `MappedObject::new_wc_in_range` zeroes a
    /// fresh allocation with exactly the same `VMap::memset(0)`.
    staging_descriptors: Option<MappedObject>,
}

#[cfg(not(test))]
enum G17PRenderCommandContext {
    Bootstrap(mmu::T8140AppContextLease),
    Job(Arc<mmu::T8140ComputeExecutionContext>),
}

#[cfg(not(test))]
impl G17PRenderCommandContext {
    fn context_id(&self) -> u32 {
        match self {
            Self::Bootstrap(context) => context.context_id(),
            Self::Job(context) => context.context_id(),
        }
    }
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

struct G17PLateTilingBytes {
    item_group: [u8; 24],
    optional: [u8; g17_submission::G17P_COLD_OPENING_OPTIONAL_SIZE],
    event: [u8; g17_submission::G17P_COLD_OPENING_EVENT_SIZE],
    pool_a_slot_offset: usize,
    shared_inner_value: u32,
}

impl G17PLateTilingBytes {
    fn apply(&self, graph: &mut [u8], support: &mut [u8]) {
        graph[G17P_USER_TILING_RING..G17P_USER_TILING_RING + self.item_group.len()]
            .copy_from_slice(&self.item_group);
        graph[G17P_USER_TILING_OPTIONAL..G17P_USER_TILING_OPTIONAL + self.optional.len()]
            .copy_from_slice(&self.optional);
        graph[G17P_USER_TILING_EVENT..G17P_USER_TILING_EVENT + self.event.len()]
            .copy_from_slice(&self.event);
        put_u32(graph, G17P_USER_TILING_POINTERS + 0x40, 3);
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
    submission_ordinal: u32,
) -> core::result::Result<G17PLateTilingBytes, g17_submission::SubmissionError> {
    let record_a = (submission_ordinal as usize * 2) % 35;
    let mut bytes = G17PLateTilingBytes {
        item_group: [0; 24],
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
    g17_submission::apply_g17p_retained_optional(
        g17_submission::G17PColdOpeningStage::Tiling,
        optional_addresses,
        submission_ordinal,
        &mut bytes.optional,
    )?;
    g17_submission::apply_g17p_retained_event(
        g17_submission::G17PColdOpeningStage::Tiling,
        submission_ordinal,
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
            G17P_CL_LOW_VA_START..G17P_CL_LOW_VA_END,
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
            G17P_CL_LOW_VA_START..G17P_CL_LOW_VA_END,
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
    pub(crate) fn write_g17p_add3_resource_table(&mut self, buffers: [u64; 3]) -> Result {
        self.preempt
            .with_bytes_mut(|raw| apply_g17p_add3_resource_table(raw, buffers).ok_or(ERANGE))
    }

    pub(crate) fn map_user_execution_state(
        &mut self,
        vm: &mmu::Vm,
    ) -> Result<G17PComputeUserMappings> {
        let descriptors = self.descriptors.map_alias_in_range(
            vm,
            G17P_CL_LOW_VA_START..G17P_CL_LOW_VA_END,
            mmu::UAT_PGSZ as u64,
            mmu::PROT_GPU_FW_SHARED_RW,
        )?;
        let preempt = self.preempt.map_alias_in_range(
            vm,
            G17P_CL_LOW_VA_START..G17P_CL_LOW_VA_END,
            mmu::UAT_PGSZ as u64,
            mmu::PROT_GPU_FW_SHARED_RW,
        )?;
        let robustness = self.robustness.map_alias_in_range(
            vm,
            G17P_CL_LOW_VA_START..G17P_CL_LOW_VA_END,
            mmu::UAT_PGSZ as u64,
            mmu::PROT_GPU_FW_SHARED_RW,
        )?;
        let operand_state = self.operand_state.map_alias_in_range(
            vm,
            G17P_CL_LOW_VA_START..G17P_CL_LOW_VA_END,
            mmu::UAT_PGSZ as u64,
            mmu::PROT_GPU_FW_SHARED_RW,
        )?;
        let queue_context = self.queue_context_low.map_alias_in_range(
            vm,
            G17P_CL_LOW_VA_START..G17P_CL_LOW_VA_END,
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

    pub(crate) fn stage_initial_channel_group(
        &mut self,
        descriptor: u64,
        context_low: u64,
        shared_control: u64,
        channel_control: u64,
    ) -> Result<g17_compute::ComputeChannelGroupAddresses> {
        let queue = self.queue_graph.iova();
        let context_high = self.queue_context_high.iova();
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
        g17_compute::build_initial_compute_channel_group(
            queue,
            descriptor,
            context_low,
            context_high,
            shared_control,
            channel_control,
            graph,
            queue_context,
        )
        .map_err(|_| EINVAL)
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
impl G17PUserRenderStorage {
    fn sksm_backing_mut(&mut self) -> Result<&mut G17PRenderSksmBacking> {
        self.sksm_backing.as_mut().ok_or(EIO)
    }

    /// Detach only the queue-address-stable part of a retired graph.  The
    /// per-VM low mappings stay with this storage and disappear before the
    /// incoming VM maps the same addresses.
    pub(crate) fn take_sksm_backing(&mut self) -> Result<G17PRenderSksmBacking> {
        self.sksm_backing.take().ok_or(EIO)
    }

    pub(crate) fn sksm_backing_detached(&self) -> bool { self.sksm_backing.is_none() }

    pub(crate) fn claim_sksm_backing(
        &mut self,
        backing: &mut Option<G17PRenderSksmBacking>,
    ) -> Result {
        if self.sksm_backing.is_some()
            || !backing
                .as_ref()
                .is_some_and(|backing| backing.matches_queue_pair(self.queue_pair))
        {
            return Err(EIO);
        }
        self.sksm_backing = backing.take();
        Ok(())
    }

    fn descriptor_context_valid(context_id: u16) -> bool {
        // A descriptor context is a queue execution context. The bind band and
        // the render app context are owned by other allocators, so a
        // descriptor naming one of them would be a cross-allocator collision,
        // not a valid submission.
        context_id
            == g17_submission::G17P_PARTIAL_OPENING_CONTEXT.descriptor_context_id as u16
            || (u32::from(context_id) >= crate::g17_queue_limits::COMPUTE_CONTEXT_FIRST
                && u32::from(context_id) < crate::g17_queue_limits::BIND_CONTEXT_FIRST)
    }

    pub(crate) fn write_sksm_entry(
        &mut self,
        tiling: bool,
        offset: usize,
        zero_length: usize,
        body: &[u8],
    ) -> Result {
        let backing = self.sksm_backing_mut()?;
        let object = if tiling {
            &mut backing.ta_context
        } else {
            &mut backing.fragment_context
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
        object.with_bytes_mut(|bytes| {
            bytes[offset..end].fill(0);
            bytes[offset..offset + body.len()].copy_from_slice(body);
            Ok(())
        })?;
        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
        Ok(())
    }

    pub(crate) fn sksm_entry_low_va(&self, tiling: bool) -> u64 {
        if tiling {
            self.ta_context_low.iova()
        } else {
            self.fragment_context_low.iova()
        }
    }

    pub(crate) fn sksm_entry_high_va(&self, tiling: bool) -> u64 {
        let Some(backing) = self.sksm_backing.as_ref() else { return 0 };
        if tiling { backing.ta_context.iova() } else { backing.fragment_context.iova() }
    }

    pub(crate) fn dump_sksm_entry(&mut self, dev: &AsahiDevice, tiling: bool, label: &str) {
        let Ok(backing) = self.sksm_backing_mut() else { return };
        let object = if tiling { &mut backing.ta_context } else { &mut backing.fragment_context };
        let mut body = [0u8; 0x200];
        let Ok(vmap) = object.object.vmap() else { return };
        let bytes = unsafe {
            // SAFETY: VMap spans the page-rounded object and the range below
            // is checked against the logical allocation.
            core::slice::from_raw_parts(vmap.as_ptr(), object.logical_size)
        };
        let Some(entry) = bytes.get(0x200..0x400) else { return };
        body.copy_from_slice(entry);
        for (row, chunk) in body.chunks(16).enumerate() {
            dev_info!(
                dev.as_ref(),
                "G17PDUMP entry-{} {} {:#05x} {:02x?}\n",
                if tiling { "TA" } else { "3D" },
                label,
                row * 16,
                chunk,
            );
        }
    }

    pub(crate) fn log_sksm_entry(
        &mut self,
        dev: &AsahiDevice,
        tiling: bool,
        label: &str,
        offset: usize,
    ) {
        let Ok(backing) = self.sksm_backing_mut() else { return };
        let object = if tiling { &mut backing.ta_context } else { &mut backing.fragment_context };
        let Ok(vmap) = object.object.vmap() else { return };
        let bytes = unsafe {
            // SAFETY: VMap spans the object and each word is bounds checked.
            core::slice::from_raw_parts(vmap.as_ptr(), object.logical_size)
        };
        let mut words = [0u32; 24];
        for (index, word) in words.iter_mut().enumerate() {
            let at = offset + index * 4;
            let Some(raw) = bytes.get(at..at + 4) else { return };
            *word = u32::from_le_bytes(raw.try_into().unwrap());
        }
        for row in 0..2 {
            let base = row * 12;
            dev_info!(
                dev.as_ref(),
                "G17P sksm entry[{}] {} +{:#x}: {:#010x} {:#010x} {:#010x} {:#010x} {:#010x} {:#010x} {:#010x} {:#010x} {:#010x} {:#010x} {:#010x} {:#010x}\n",
                label,
                if tiling { "TA" } else { "3D" },
                offset + row * 0x30,
                words[base], words[base + 1], words[base + 2], words[base + 3],
                words[base + 4], words[base + 5], words[base + 6], words[base + 7],
                words[base + 8], words[base + 9], words[base + 10], words[base + 11],
            );
        }
    }

    /// First-stage raw capture of the retained host-authored render graph.
    /// Descriptor pages include the actual embedded RCE arrays. The separate
    /// fragment_rce diagnostic mirror is deliberately not substituted for them.
    pub(crate) fn capture_owned_trace(
        &mut self, archive: &mut TraceArchive<'_>, phase: TracePhase,
        phase_sequence: u64, job_stamp: u64, clock: &mut impl FnMut() -> u64,
    ) -> Result {
        // The command-GART lease is independent of the queue execution
        // context carried by this TA/3D pair. Both roots retain this VM.
        let context = u32::from(self.descriptor_context_id);
        let mut meta = TraceMeta::new(TraceKind::RenderDescriptors, phase, phase_sequence);
        meta.role = 0;
        meta.context = 0;
        meta.job_stamp = job_stamp;
        for (offset, dva, qid) in [
            (0, self.descriptors.iova(), self.queue_pair.tiling),
            (
                G17P_RENDER_3D_DESCRIPTOR_OFFSET,
                self.descriptors.iova() + G17P_RENDER_3D_DESCRIPTOR_OFFSET as u64,
                self.queue_pair.fragment,
            ),
        ] {
            let mut item = meta;
            item.dva = dva;
            item.qid = u32::from(qid);
            self.descriptors.capture_trace_range(archive, item, offset, mmu::UAT_PGSZ, clock)?;
        }
        // Preserve the job-root alias identities too: RCE self pointers are
        // low application-GART addresses, not descriptor high-root addresses.
        for (offset, dva, qid, alias_context, instance) in [
            (0, self.descriptor_ta_low.iova(), self.queue_pair.tiling, context, 1),
            (mmu::UAT_PGSZ, self.descriptor_3d_low.iova(), self.queue_pair.fragment, context, 1),
            (mmu::UAT_PGSZ, self.descriptor_3d_render.iova(), self.queue_pair.fragment, context, 2),
        ] {
            let mut item = meta;
            item.dva = dva;
            item.qid = u32::from(qid);
            item.context = alias_context;
            item.root = trace::ROOT_CONTEXT_LOW;
            item.instance = instance;
            self.descriptors.capture_trace_range(archive, item, offset, mmu::UAT_PGSZ, clock)?;
        }
        for (kind, object, object_context) in [
            (TraceKind::RenderQueueGraph, &mut self.graph, 0),
            (TraceKind::RenderSupport, &mut self.support, 0),
            (TraceKind::RenderState, &mut self.render_state, context),
            (TraceKind::RenderTimestamps, &mut self.timestamps, 0),
            (TraceKind::FragmentStatus, &mut self.fragment_status, 0),
        ] {
            let mut item = meta;
            item.kind = kind;
            item.context = object_context;
            object.capture_trace(archive, item, clock)?;
        }
        // Follow the exact retained alias owners/constructor source offsets.
        // These compact addresses, not the generic backing allocation's DVA,
        // are what the actual RCE arrays publish for command-owned DEFLAKE/AXFB.
        for (instance, offset, mapping) in [
            (1, G17P_RENDER_STATE_DEFLAKE, &self.render_user_aliases.deflake),
            (4, self.render_layout.aux_fb, &self.render_user_aliases.aux_fb),
        ] {
            let mut item = meta;
            item.kind = TraceKind::RenderState;
            item.context = context;
            item.root = trace::ROOT_CONTEXT_LOW;
            item.instance = instance;
            item.dva = mapping.iova();
            self.render_state.capture_trace_range(archive, item, offset, mapping.size(), clock)?;
        }
        let rt_offset = g17p_render_rt_record_offset(self.descriptor_ordinal);
        for (instance, offset, size, dva) in [
            (
                2,
                rt_offset,
                self.render_layout.tilemap_size,
                self.render_target_memory.tilemap_va(self.descriptor_ordinal)?,
            ),
            (
                7,
                rt_offset
                    + self.render_layout.layermeta.checked_sub(self.render_layout.tilemap)
                        .ok_or(EINVAL)?,
                self.render_layout.layermeta_size,
                self.render_target_memory.rt_region_va(
                    self.descriptor_ordinal,
                    &self.render_layout,
                    self.render_layout.layermeta,
                )?,
            ),
            (
                6,
                rt_offset
                    + self.render_layout.heapmeta.checked_sub(self.render_layout.tilemap)
                        .ok_or(EINVAL)?,
                self.render_layout.heapmeta_size,
                self.render_target_memory
                    .heapmeta_va(self.descriptor_ordinal, &self.render_layout)?,
            ),
        ] {
            if size == 0 {
                continue;
            }
            let mut item = meta;
            item.kind = TraceKind::RenderState;
            item.context = context;
            item.root = trace::ROOT_CONTEXT_LOW;
            item.instance = instance;
            item.dva = dva;
            self.render_target_memory.rt_memory.capture_trace_range(
                archive, item, offset, size, clock,
            )?;
        }
        let mut tpc_item = meta;
        tpc_item.kind = TraceKind::RenderState;
        tpc_item.context = context;
        tpc_item.root = trace::ROOT_CONTEXT_LOW;
        tpc_item.instance = 3;
        tpc_item.dva = self.render_target_memory.tpc_va();
        self.render_target_memory.tpc.capture_trace_range(
            archive,
            tpc_item,
            0,
            self.render_layout.tpc_size,
            clock,
        )?;
        let mut item = meta;
        item.kind = TraceKind::RenderState;
        item.context = context;
        item.root = trace::ROOT_CONTEXT_LOW;
        item.instance = 5;
        item.dva = self.render_state.iova() + G17P_RENDER_STATE_TA_STATUS as u64;
        self.render_state.capture_trace_range(
            archive,
            item,
            G17P_RENDER_STATE_TA_STATUS,
            mmu::UAT_PGSZ,
            clock,
        )?;
        let mut status = meta;
        status.kind = TraceKind::FragmentStatus;
        status.context = context;
        status.root = trace::ROOT_CONTEXT_LOW;
        status.instance = 1;
        status.dva = self.fragment_status_low.iova();
        self.fragment_status.capture_trace_range(
            archive, status, 0, self.fragment_status_low.size(), clock)?;
        for (kind, object, object_context) in [
            (TraceKind::SceneScratch, self.scene_scratch.as_mut(), context),
            (TraceKind::Discard, self.discard.as_mut(), context),
            // PM descriptors/PoolA/Scene/HWPB are firmware-high objects.
            (TraceKind::ParameterManagement, self.parameter_management.as_mut().map(|pm| &mut pm.backing), 0),
        ] {
            let mut item = meta;
            item.kind = kind;
            item.context = object_context;
            if let Some(object) = object {
                object.capture_trace(archive, item, clock)?;
            } else {
                archive.omit(item, TraceOmission::Unavailable).map_err(|_| EINVAL)?;
            }
        }
        // These buffers require separate ownership-aware traversal; a missing
        // payload must not turn into a fabricated all-zero native comparison.
        for kind in [TraceKind::ClientObjects, TraceKind::TvbPayload] {
            let mut item = meta;
            item.kind = kind;
            item.context = context;
            item.root = trace::ROOT_CONTEXT_LOW;
            if kind == TraceKind::TvbPayload {
                if let Some(tvb) = &self.render_tvb {
                    item.dva = tvb.iova();
                    item.expected_len = (tvb.block_count * G17P_RENDER_TVB_BLOCK_SIZE) as u64;
                }
            }
            archive.omit(item, TraceOmission::NotImplemented).map_err(|_| EINVAL)?;
        }
        Ok(())
    }

    /// DIAGNOSTIC. Read the render `channel_control` object the tag-15 record
    /// names at `+0x4a`.
    ///
    /// Firmware RE of b1: the tag-15 tail binds this object into the scheduler
    /// state, filling in fields the host leaves as `0xff` -- `[1]` the firmware
    /// index (allocated at `0x12408`, published to `0x124ba8[idx]` at
    /// `0x12460`), `[0]` the pid slot (`FUN_98f0` `0x99b0`), `[2] = 4`,
    /// `[3] = priority`, `[4]` the class from `table_0x47467`, `+0x12` the
    /// 128-bit qid mask, and `[0x33]` the HardwareBufferID. The firmware
    /// cache-cleans the object back to host memory at `0x12488`, so a host read
    /// after the first submit shows whether the bind actually ran. Fields still
    /// reading `0xff` mean it did not.
    /// DIAGNOSTIC. Pool-A record 0. The classic TA command handler dereferences
    /// TA descriptor `+0x10` (the Pool-A record), invalidates 0xc8 bytes there,
    /// and `FUN_16f7c` then does `ldr w8,[rec,#148]; cbnz w8, <bare return>` --
    /// a non-zero `+0x94` makes the firmware DROP the TA work silently, with no
    /// job inserted, no trace and no fault, which is exactly our symptom.
    pub(crate) fn read_pool_a_record(&mut self, out: &mut [u8; 0x200]) -> Result {
        self.support.with_bytes_mut(|bytes| {
            out.copy_from_slice(
                bytes
                    .get(G17P_RENDER_POOL_A..G17P_RENDER_POOL_A + 0x200)
                    .ok_or(ERANGE)?,
            );
            Ok(())
        })
    }

    pub(crate) fn read_channel_control(&mut self, out: &mut [u8; 0x40]) -> Result {
        self.support.with_bytes_mut(|bytes| {
            out.copy_from_slice(
                bytes
                    .get(
                        G17P_RENDER_CHANNEL_CONTROL
                            ..G17P_RENDER_CHANNEL_CONTROL + 0x40,
                    )
                    .ok_or(ERANGE)?,
            );
            Ok(())
        })
    }

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
        let offset = self.queue_record_offset(tiling);
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
    const TILING_POINTERS: usize = G17P_USER_TILING_POINTERS;
    const FRAGMENT_POINTERS: usize = 0x1080;
    const JOB_LIST: usize = 0x2000;
    const FRAGMENT_OPTIONAL: usize = 0x30c0;
    const TILING_EVENT: usize = G17P_USER_TILING_EVENT;
    const FRAGMENT_EVENT: usize = 0x3600;
    const TILING_RING: usize = G17P_USER_TILING_RING;
    const FRAGMENT_RING: usize = 0x8000;

    fn new(
        dev: &AsahiDevice,
        uat: &mmu::Uat,
        ta_context_peer: &mut MappedObject,
        fragment_context_peer: &mut MappedObject,
        vm: &mmu::Vm,
        command: &g17_uapi::TranslatedRenderCommand,
        num_clusters: u32,
        owner_pid: u32,
        command_context: G17PRenderCommandContext,
        descriptor_context_id: u16,
        render_root_slot: u8,
        command_buffer_state_words: [u32; 2],
        submission_ordinal: u32,
        parameter_manager_generation: Option<u64>,
        retained_sksm_backing: &mut Option<G17PRenderSksmBacking>,
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

        let ta_context_low = match ta_context_peer.map_alias_at(
            uat.kernel_lower_vm(),
            G17P_BOOTSTRAP_TA_CONTEXT_LOW_VA,
            mmu::PROT_GPU_SHARED_RO,
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
            mmu::PROT_GPU_SHARED_RO,
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
        if page_align(render_layout.tpc_size).ok_or(EOVERFLOW)?
            > G17P_RENDER_TPC_CAPACITY
        {
            return Err(ERANGE);
        }
        let mut render_target_memory = bootstrap_stage!(
            "persistent render-target-memory allocation",
            new_g17p_render_target_memory(dev, vm, queue_pair)
        );
        let render_target_addresses = bootstrap_stage!(
            "render-target-memory record",
            render_target_memory.prepare_rt_slot(submission_ordinal, &render_layout)
        );
        let mut render_state = bootstrap_stage!("state allocation", MappedObject::new_wc_in_range(
            dev,
            vm,
            G17P_CL_LOW_VA_START..G17P_CL_LOW_VA_END,
            render_layout.total_size,
            mmu::UAT_PGSZ as u64,
            mmu::PROT_GPU_FW_SHARED_RW,
        ));
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
        let timestamps = bootstrap_stage!("timestamp allocation", MappedObject::new_wc_in_range(
            dev,
            vm,
            G17P_CL_LOW_VA_START..G17P_CL_LOW_VA_END,
            mmu::UAT_PGSZ,
            mmu::UAT_PGSZ as u64,
            mmu::PROT_GPU_FW_SHARED_RW,
        ));
        let support_va = support.iova();
        let render_state_va = render_state.iova();
        let timestamp_va = timestamps.iova();
        let mut translated = *command;
        let bootstrap_vdm = translated.parameters.encoder == G17P_BOOTSTRAP_VDM_VA;
        if bootstrap_vdm && translated.parameters.context_base != G17P_BOOTSTRAP_CONTEXT_BASE {
            return Err(EINVAL);
        }
        translated.parameters.context_base = if bootstrap_vdm {
            G17P_BOOTSTRAP_CONTEXT_BASE
        } else {
            render_state_va
        };
        translated.parameters.deflake_3 = render_state_va + G17P_RENDER_STATE_DEFLAKE as u64;
        translated.parameters.deflake_2 = translated.parameters.deflake_3 + 0x20;
        translated.parameters.deflake_1 = translated.parameters.deflake_3 + 0x2a0;
        translated.parameters.ta_status = render_state_va + G17P_RENDER_STATE_TA_STATUS as u64;
        translated.parameters.fragment_status = fragment_status_low.iova();
        translated.parameters.layermeta = render_target_addresses.layermeta;
        translated.parameters.heapmeta = render_target_addresses.heapmeta;
        translated.parameters.tpc = render_target_memory.tpc_va();
        translated.parameters.aux_fb = render_user_aliases.aux_fb_va();
        translated.parameters.aux_fb_page_count = G17P_RENDER_AUX_FB_PAGE_COUNT;
        translated.parameters.aux_fb_flags = g17p_aux_fb_flags(translated.flags);
        translated.parameters.tilemap = render_target_addresses.tilemap;
        if let Some(pm) = parameter_management.as_ref() {
            translated.parameters.parameter_buffer = pm.selected_scene_va();
            let (cycle, record_index) = pm.scene_registers(
                scene_scratch.as_ref().map(MappedObject::iova).ok_or(EINVAL)?,
            )?;
            translated.parameters.cycle = cycle;
            translated.parameters.record_index = record_index;
        }
        dev_info!(
            dev.as_ref(),
            "G17P render address domains: resource-base={:#x} state={:#x} deflake={:#x} fragment-status={:#x} scene={:#x} discard={:#x} tilemap={:#x} layermeta={:#x} heapmeta={:#x} tpc={:#x} axfb={:#x} tvb={:#x}\n",
            translated.parameters.context_base,
            render_state_va,
            render_user_aliases.deflake_va(),
            fragment_status_low.iova(),
            scene_scratch.as_ref().map_or(0, MappedObject::iova),
            discard.as_ref().map_or(0, MappedObject::iova),
            translated.parameters.tilemap,
            translated.parameters.layermeta,
            translated.parameters.heapmeta,
            translated.parameters.tpc,
            translated.parameters.aux_fb,
            render_tvb.as_ref().map_or(0, G17PRenderTvb::iova),
        );
        // NATIVE LAYOUT, macOS 26.6.2 2026-09-02 (recs3.txt, three passes,
        // byte-stable): a render pass owns ONE (start, end) pair, not one per
        // stage. The TA descriptor carries only a START pointer and leaves its
        // end field ZERO (+0x08fe = 0xfffffc2000024c68, +0x0906 = 0); the 3D
        // points its START at the SAME address and its END at start+8. The
        // tiler stamps the pass start, the fragment stamps the pass end.
        //
        // This driver published four distinct qwords and required all four to
        // be non-zero before calling a render complete, which the hardware can
        // never satisfy because nothing writes a TA end. That is why a render
        // whose completion ring showed BOTH stages finished with real
        // start/end timestamps was still abandoned after 2000 polls.
        translated.parameters.ta_timestamp_start = timestamp_va;
        translated.parameters.ta_timestamp_end = 0;
        translated.parameters.fragment_timestamp_start = timestamp_va;
        translated.parameters.fragment_timestamp_end = timestamp_va + 8;
        let lifecycle_predecessor = g17p_allocate_object_id()?;
        let (fragment_lifecycle, tiling_lifecycle) =
            g17p_allocate_render_lifecycle_pair(lifecycle_predecessor)?;
        translated.parameters.fragment_lifecycle = fragment_lifecycle;
        translated.parameters.lifecycle = tiling_lifecycle;
        if crate::util::g17p_verbose() {
            dev_info!(
                dev.as_ref(),
                "G17P render lifecycle: predecessor={:#x} 3D current={:#x} TA current={:#x} ordinal=0\n",
                lifecycle_predecessor,
                fragment_lifecycle as u32,
                tiling_lifecycle as u32,
            );
        }
        if *crate::module_parameters::g17p_render_eot_bind_zero.value() != 0 {
            translated.parameters.store_pipeline_bind = 0;
            translated.parameters.partial_store_pipeline_bind = 0;
            if crate::util::g17p_verbose() {
                dev_info!(
                    dev.as_ref(),
                    "G17P render EOT bind: forcing native-captured main/partial value 0\n",
                );
            }
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
                translated.parameters.layermeta,
                render_layout
                    .layermeta_size
                    .checked_add(render_layout.heapmeta_size)
                    .ok_or(EOVERFLOW)? as u64,
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
            (timestamp_va, 32, true, true),
        ] {
            if !vm.covers_range(address, size, need_read, need_write) {
                return Err(EFAULT);
            }
        }

        bootstrap_stage!("state encoding", render_state.with_bytes_mut(|raw| {
            apply_g17p_viewport(
                translated.parameters.width,
                translated.parameters.height,
                &mut raw[G17P_RENDER_STATE_DEFLAKE..G17P_RENDER_STATE_DEFLAKE + mmu::UAT_PGSZ],
            )
            .ok_or(EINVAL)?;
            put_u32(raw, render_layout.aux_fb + 0x480, 0x6000_0000);
            put_u32(raw, render_layout.aux_fb + 0x484, 0x35b);
            Ok(())
        }));

        let graph_va = graph.iova();
        bootstrap_stage!("TA queue context", ta_context_peer.with_bytes_mut(|raw| {
            raw.fill(0);
            g17_submission::apply_g17p_cold_opening_queue_context(
                g17_submission::G17PColdOpeningStage::Tiling,
                g17_submission::G17P_COLD_OPENING_TA_DESCRIPTOR_BASE,
                graph_va + Self::TILING_QUEUE as u64,
                &mut raw[..mmu::UAT_PGSZ],
            )
            .map_err(|_| EINVAL)
        }));
        bootstrap_stage!("3D queue context", fragment_context_peer.with_bytes_mut(|raw| {
            raw.fill(0);
            g17_submission::apply_g17p_cold_opening_queue_context(
                g17_submission::G17PColdOpeningStage::Fragment,
                g17_submission::G17P_COLD_OPENING_3D_DESCRIPTOR_BASE,
                graph_va + Self::FRAGMENT_QUEUE as u64,
                &mut raw[..mmu::UAT_PGSZ],
            )
            .map_err(|_| EINVAL)
        }));
        let graph_addresses = g17_submission::G17PColdOpeningGraphAddresses {
            primary_index: g17_submission::G17P_PARTIAL_OPENING_PRIMARY_INDEX_FIRMWARE_GPU_VA,
            secondary_index: support_va + G17P_RENDER_SECONDARY_INDEX as u64,
            pool_a_slots: support_va + G17P_RENDER_POOL_A_SLOTS as u64,
            pool_b_slots: support_va + G17P_RENDER_POOL_B_SLOTS as u64,
            shared_slots: support_va + G17P_RENDER_SHARED_SLOTS as u64,
            flag: support_va + G17P_RENDER_FLAG as u64,
        };
        let objects = g17_render::RenderDescriptorObjects {
            record_a: support_va + G17P_RENDER_POOL_A as u64,
            shared: support_va + G17P_RENDER_PACKED_SHARED as u64,
            record_b: support_va + G17P_RENDER_POOL_B as u64,
            zero: support_va + G17P_RENDER_ZERO_SHARED as u64,
        };
        let channel_control = support_va + G17P_RENDER_CHANNEL_CONTROL as u64;
        bootstrap_stage!("support encoding", support.with_bytes_mut(|raw| {
            g17_submission::apply_g17p_cold_opening_leaf_page(
                g17_submission::G17PColdOpeningLeaf::SecondaryIndex,
                &mut raw[G17P_RENDER_SECONDARY_INDEX..G17P_RENDER_SECONDARY_INDEX + mmu::UAT_PGSZ],
            )
            .map_err(|_| EINVAL)?;
            g17_submission::apply_g17p_cold_opening_leaf_page(
                g17_submission::G17PColdOpeningLeaf::PoolASlots,
                &mut raw[G17P_RENDER_POOL_A_SLOTS..G17P_RENDER_POOL_A_SLOTS + mmu::UAT_PGSZ],
            )
            .map_err(|_| EINVAL)?;
            // Native exposes value one to early 3D and changes it to two in
            // the late TA publication callback.
            put_u32(raw, G17P_RENDER_POOL_A_SLOTS + 4, 1);
            g17_submission::apply_g17p_cold_opening_leaf_page(
                g17_submission::G17PColdOpeningLeaf::PoolBSlots,
                &mut raw[G17P_RENDER_POOL_B_SLOTS..G17P_RENDER_POOL_B_SLOTS + mmu::UAT_PGSZ],
            )
            .map_err(|_| EINVAL)?;
            g17_submission::apply_g17p_cold_opening_leaf_page(
                g17_submission::G17PColdOpeningLeaf::SharedSlots,
                &mut raw[G17P_RENDER_SHARED_SLOTS..G17P_RENDER_SHARED_SLOTS + mmu::UAT_PGSZ],
            )
            .map_err(|_| EINVAL)?;
            g17_submission::apply_g17p_cold_opening_leaf_page(
                g17_submission::G17PColdOpeningLeaf::Flag,
                &mut raw[G17P_RENDER_FLAG..G17P_RENDER_FLAG + mmu::UAT_PGSZ],
            )
            .map_err(|_| EINVAL)?;
            g17_submission::apply_g17p_cold_opening_record_pool_a(
                graph_addresses,
                &mut raw[G17P_RENDER_POOL_A
                    ..G17P_RENDER_POOL_A + g17_submission::G17P_COLD_OPENING_RECORD_POOL_A_SIZE],
            )
            .map_err(|_| render_build_fail(11))?;
            g17_render::set_render_event_control_generation(
                &mut raw[G17P_RENDER_POOL_A + pool_a_record_index * 0x100..],
                command_buffer_state_words[0],
            )
            .map_err(|_| render_build_fail(11))?;
            g17_submission::apply_g17p_cold_opening_record_pool_b(
                graph_addresses,
                &mut raw[G17P_RENDER_POOL_B
                    ..G17P_RENDER_POOL_B + g17_submission::G17P_COLD_OPENING_RECORD_POOL_B_SIZE],
            )
            .map_err(|_| EINVAL)?;
            g17_submission::apply_g17p_cold_opening_shared_object(
                graph_addresses,
                &mut raw[G17P_RENDER_PACKED_SHARED
                    ..G17P_RENDER_PACKED_SHARED
                        + g17_submission::G17P_COLD_OPENING_SHARED_OBJECT_SIZE],
            )
            .map_err(|_| EINVAL)?;
            g17_submission::apply_g17p_cold_opening_shared_control(
                G17P_CONTROL_OPERAND_TABLE_VA,
                support_va + G17P_RENDER_SHARED_CONTROL_INNER as u64,
                &mut raw[G17P_RENDER_SHARED_CONTROL
                    ..G17P_RENDER_SHARED_CONTROL
                        + g17_submission::G17P_COLD_OPENING_SHARED_CONTROL_SIZE],
            )
            .map_err(|_| EINVAL)?;
            put_u32(raw, G17P_RENDER_SHARED_CONTROL_INNER, 1);
            g17_submission::apply_g17p_cold_opening_channel_control(
                &mut raw[G17P_RENDER_CHANNEL_CONTROL
                    ..G17P_RENDER_CHANNEL_CONTROL
                        + g17_submission::G17P_COLD_OPENING_CHANNEL_CONTROL_SIZE],
            )
            .map_err(|_| EINVAL)?;
            Ok(())
        }));
        pr_info!(
            "G17P render ta-inputs: context_base={:#x} encoder(vdm)={:#x} tilemap={:#x} layermeta={:#x} heapmeta={:#x} tpc={:#x}\n",
            translated.parameters.context_base,
            translated.parameters.encoder,
            translated.parameters.tilemap,
            translated.parameters.layermeta,
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
                        if crate::util::g17p_verbose() {
                            pr_info!(
                                "G17P render vdm-stream +{:#05x}: {:02x?}\n",
                                base,
                                &stream[base..base + 32],
                            );
                        }
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
                g17p_cl_client_low_va_range(),
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
                G17P_RENDER_3D_DESCRIPTOR_OFFSET
                    ..G17P_RENDER_3D_DESCRIPTOR_OFFSET + mmu::UAT_PGSZ,
                g17p_cl_client_low_va_range(),
                mmu::UAT_PGSZ as u64,
                mmu::PROT_GPU_SHARED_RO,
                false,
            )
        );
        let descriptor_3d_render_va = descriptor_3d_render.iova();
        let descriptor_3d_rce_va = descriptor_3d_low.iova();
        dev_info!(
            dev.as_ref(),
            "G17P render RCE: job descriptor={:#x}, retained diagnostic client alias={:#x}; diagnostic entry mirrors={:#x}/{:#x}/{:#x}/{:#x} stride={:#x}\n",
            descriptor_3d_rce_va,
            descriptor_3d_render_va,
            fragment_rce_va,
            fragment_rce_va + g17_render::G17P_FRAGMENT_RCE_PROGRAM_STRIDE,
            fragment_rce_va + 2 * g17_render::G17P_FRAGMENT_RCE_PROGRAM_STRIDE,
            fragment_rce_va + 3 * g17_render::G17P_FRAGMENT_RCE_PROGRAM_STRIDE,
            g17_render::G17P_FRAGMENT_RCE_PROGRAM_STRIDE,
        );
        let (fragment_mcache, tiling_mcache) = bootstrap_stage!("descriptor encoding", descriptors.with_bytes_mut(|raw| {
            let (ta_array, fragment_array) = raw.split_at_mut(G17P_RENDER_3D_DESCRIPTOR_OFFSET);
            let tiling = ta_array.get_mut(..g17_render::TA_DESCRIPTOR_SIZE).ok_or(ERANGE)?;
            let fragment = fragment_array
                .get_mut(..g17_render::FRAGMENT_DESCRIPTOR_SIZE).ok_or(ERANGE)?;
            g17_render::build_cold_opening_ta_descriptor(
                &translated.parameters,
                tiling_objects,
                command_context_id,
                command_buffer_state_words[0],
                translated.parameters.ta_hardware_buffer_id,
                tiling,
            )
                .map_err(|error| render_build_fail_named(16, error))?;
            g17_render::apply_g17p_retained_descriptor_fields(
                g17_render::RenderDescriptorKind::Tiling,
                G17P_BOOTSTRAP_TA_DESCRIPTOR_LOW_VA,
                g17_initdata::CONTROL_SHARED_ADDRESS,
                0,
                true,
                tiling,
            )
            .map_err(|_| EINVAL)?;
            g17_render::build_cold_opening_fragment_descriptor(
                &translated.parameters,
                objects,
                fragment,
            )
            .map_err(|_| EINVAL)?;
            g17_render::apply_g17p_retained_descriptor_fields(
                g17_render::RenderDescriptorKind::Fragment,
                G17P_BOOTSTRAP_3D_DESCRIPTOR_LOW_VA,
                g17_initdata::CONTROL_SHARED_ADDRESS,
                0,
                true,
                fragment,
            )
            .map_err(|_| EINVAL)?;
            apply_g17p_generic_descriptor_tail(tiling, fragment).ok_or(EINVAL)
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
            G17P_BOOTSTRAP_3D_DESCRIPTOR_LOW_VA,
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
                    channel_control,
                },
                &mut raw[Self::TILING_QUEUE..Self::TILING_QUEUE + G17P_RENDER_QUEUE_RECORD_SIZE],
            )
            .map_err(|_| EINVAL)?;
            g17_submission::apply_g17p_cold_opening_queue_record(
                g17_submission::G17PColdOpeningQueueAddresses {
                    pointers: graph_va + Self::FRAGMENT_POINTERS as u64,
                    item_ring: graph_va + Self::FRAGMENT_RING as u64,
                    job_list,
                    channel_control,
                },
                &mut raw
                    [Self::FRAGMENT_QUEUE..Self::FRAGMENT_QUEUE + G17P_RENDER_QUEUE_RECORD_SIZE],
            )
            .map_err(|_| EINVAL)?;
            g17_submission::apply_g17p_cold_opening_pointer_block(
                &mut raw
                    [Self::TILING_POINTERS..Self::TILING_POINTERS + G17P_RENDER_POINTER_BLOCK_SIZE],
            )
            .map_err(|_| EINVAL)?;
            g17_submission::apply_g17p_cold_opening_pointer_block(
                &mut raw[Self::FRAGMENT_POINTERS
                    ..Self::FRAGMENT_POINTERS + G17P_RENDER_POINTER_BLOCK_SIZE],
            )
            .map_err(|_| EINVAL)?;
            put_u64(raw, Self::JOB_LIST + 0x08, job_list);
            Ok(())
        }));

        Ok(Self {
            descriptors,
            descriptor_ta_high,
            descriptor_3d_high,
            descriptor_ta_low,
            descriptor_3d_low,
            ta_context_low,
            fragment_context_low,
            graph,
            support,
            render_state,
            render_ta_status: Some(render_ta_status),
            render_3d_status: Some(render_3d_status),
            timestamps,
            fragment_mcache,
            tiling_mcache,
            ta_hardware_buffer_id: translated.parameters.ta_hardware_buffer_id,
            owner_pid,
            lifecycle_predecessor,
            _command_context: Some(command_context),
            descriptor_context_id,
            context_update_pending: true,
            render_root_slot,
            staging_descriptors: None,
        })
    }

    /// TTBAT context this render's VM is bound to.
    pub(crate) fn render_root_slot(&self) -> u8 {
        self.render_root_slot
    }

    pub(crate) fn descriptor_context_id(&self) -> u16 {
        self.descriptor_context_id
    }

    /// Transfer an idle retained graph to another logical queue in the same
    /// VM. The graph's fixed client mappings and parameter-management storage
    /// belong to the VM, not to one queue context; rebuilding them at the same
    /// addresses would collide with the still-retained firmware owner. The
    /// command context, descriptor context, and MCache root selector are the
    /// queue-owned pieces and are restaged before the next publication.
    pub(crate) fn retarget_command_context(
        &mut self,
        vm: &mmu::Vm,
        context: Arc<mmu::T8140ComputeExecutionContext>,
    ) -> Result {
        if !context.matches(vm) || !context.is_current() {
            return Err(EFAULT);
        }
        let context_id = u16::try_from(context.context_id()).map_err(|_| EOVERFLOW)?;
        if !Self::descriptor_context_valid(context_id) {
            return Err(EINVAL);
        }
        let root_slot = u8::try_from(context.context_id()).map_err(|_| EOVERFLOW)?;
        self._command_context = Some(G17PRenderCommandContext::Job(context));
        self.descriptor_context_id = context_id;
        self.render_root_slot = root_slot;
        self.context_update_pending = true;
        Ok(())
    }

    pub(crate) fn complete_context_update(&mut self) {
        self.context_update_pending = false;
    }

    pub(crate) fn context_update_pending(&self) -> bool {
        self.context_update_pending
    }

    /// Expected TA parameter-buffer binding copied from the retained owner
    /// into tag-15. These values are independent of the physical QID and must
    /// survive a graph rebuild as one tuple.
    pub(crate) fn parameter_buffer_binding(&self) -> (u64, u64, u32) {
        let addresses = self.optional_addresses(true);
        (
            addresses.tiling_shared_object,
            addresses.parameter_buffer_token,
            addresses.ta_hardware_buffer_id,
        )
    }

    pub(crate) fn parameter_buffer_page_list_vas(&self) -> Result<[u64; 2]> {
        self.parameter_management
            .as_ref()
            .map(G17PParameterManagement::page_list_vas)
            .ok_or(EINVAL)
    }

    /// Read the exact EventControl record owned by the current TA command.
    /// The matching firmware type-1 edge orders these host-DRAM fields; no
    /// MMIO or periodic polling is needed to decide whether it is reusable.
    /// How many of the 35 Pool-A EventControl records are currently reusable.
    ///
    /// The burn question in one number. A record is reusable only while
    /// `completed == submitted && membership == 0`; a render whose pair
    /// half-completes leaves it short by one forever, and `prepare_reuse` then
    /// refuses it for the rest of the boot.
    ///
    /// That refusal is invisible in the log as things stand. Selection SKIPS an
    /// unusable record silently -- `(0..35).map(...).find(...)` -- the fallback
    /// line is gated on `G17P_POOL_A_FALLBACK_LOGGED == 0` so it fires once per
    /// boot and never again, the `became unusable` EBUSY needs a record to pass
    /// selection and then fail at restage (a race, and rightly rare), and the
    /// 35-way audit only runs when EVERY record is gone. So a boot can burn
    /// records steadily and print nothing. Counting them on the witness path --
    /// which only runs on a wedge, plus once on a healthy completion -- makes
    /// the trend observable without adding a line per frame.
    pub(crate) fn pool_a_reusable_count(&mut self) -> Result<u32> {
        let support_va = self.support.iova();
        let view = self.support.object.vmap()?;
        let support_ro = unsafe {
            // SAFETY: the VMap covers the complete support object; the reuse
            // predicate bounds-checks every record it reads.
            core::slice::from_raw_parts(view.as_ptr(), G17P_RENDER_SUPPORT_SIZE)
        };
        Ok((0..35usize)
            .filter(|&index| {
                G17PEventControlKick::prepare_reuse(support_ro, support_va, index).is_some()
            })
            .count() as u32)
    }

    pub(crate) fn render_event_control_state(&mut self) -> Result<G17PRenderEventControlState> {
        let index = usize::from(self.pool_a_record_index);
        let record = G17P_RENDER_POOL_A
            .checked_add(index.checked_mul(0x100).ok_or(EOVERFLOW)?)
            .ok_or(EOVERFLOW)?;
        let slot_offset = G17P_RENDER_POOL_A_SLOTS
            .checked_add(index.checked_mul(4).ok_or(EOVERFLOW)?)
            .ok_or(EOVERFLOW)?;
        let mut completed = 0;
        let mut membership = 0;
        let mut slot_word = 0;
        self.support.with_bytes_mut(|support| {
            slot_word = u32::from_le_bytes(
                support
                    .get(slot_offset..slot_offset + 4)
                    .ok_or(ERANGE)?
                    .try_into()
                    .map_err(|_| EINVAL)?,
            );
            completed = u32::from_le_bytes(
                support
                    .get(record + 0x0c..record + 0x10)
                    .ok_or(ERANGE)?
                    .try_into()
                    .map_err(|_| EINVAL)?,
            );
            membership = u32::from_le_bytes(
                support
                    .get(record + 0x94..record + 0x98)
                    .ok_or(ERANGE)?
                    .try_into()
                    .map_err(|_| EINVAL)?,
            );
            Ok(())
        })?;
        Ok(G17PRenderEventControlState {
            record_index: self.pool_a_record_index,
            submitted: self.pool_a_submitted,
            slot_word,
            completed,
            membership,
        })
    }

    /// Read the context IDs from the TA and 3D descriptors actually staged
    /// for the current ordinal. This is deliberately a byte readback rather
    /// than the host-side value passed to the builder: the former is what the
    /// firmware consumes.
    pub(crate) fn descriptor_context_ids(&mut self) -> Result<[u32; 2]> {
        let ta = g17p_render_ta_descriptor_offset(self.descriptor_ordinal);
        let fragment = g17p_render_3d_descriptor_offset(self.descriptor_ordinal);
        let mut contexts = [0; 2];
        self.descriptors.with_bytes_mut(|raw| {
            for (context, base) in contexts.iter_mut().zip([ta, fragment]) {
                let at = base.checked_add(0x0c).ok_or(EOVERFLOW)?;
                *context = u32::from_le_bytes(
                    raw.get(at..at + 4)
                        .ok_or(ERANGE)?
                        .try_into()
                        .map_err(|_| ERANGE)?,
                );
            }
            Ok(())
        })?;
        core::sync::atomic::fence(Ordering::Acquire);
        Ok(contexts)
    }

    pub(crate) fn tvb_capacity(&self) -> usize {
        self.render_tvb.as_ref().map_or(0, |tvb| tvb.block_count)
    }

    pub(crate) fn render_pool_id(&self) -> u64 {
        // This checked, monotonic object ID is unique to this
        // retained graph, even if its GPU addresses are reused after reset.
        u64::from(self.lifecycle_predecessor)
    }

    pub(crate) fn take_retired_parameter_management(
        &mut self,
    ) -> Option<G17PRetiredParameterManagement> {
        self.parameter_management.take().map(|manager| {
            // The client page-list presentation is rooted through the
            // retiring VmBind and is not what the firmware crash named.
            // Destructure the manager so that only its firmware-high backing
            // survives; all scalar metadata and the low alias are released.
            let G17PParameterManagement {
                _page_list_low,
                backing,
                ..
            } = manager;
            drop(_page_list_low);
            G17PRetiredParameterManagement {
                _backing: backing,
                _support: None,
            }
        })
    }

    /// Split the firmware-retained portion out of a graph whose client roots
    /// have a positive context-update retirement witness. GFX PC 0x24144
    /// faulted inside `support` after that witness, while the earlier PC 0x95f0
    /// fault named the PM backing. Both therefore outlive the VM-facing graph.
    pub(crate) fn into_retired_parameter_management(
        self,
    ) -> Option<G17PRetiredParameterManagement> {
        let G17PUserRenderStorage {
            support,
            parameter_management,
            ..
        } = self;
        parameter_management.map(|manager| {
            let G17PParameterManagement {
                _page_list_low,
                backing,
                ..
            } = manager;
            drop(_page_list_low);
            G17PRetiredParameterManagement {
                _backing: backing,
                _support: Some(support),
            }
        })
    }

    fn take_persistent_client_mappings(&mut self) -> Result<KVec<mmu::KernelMapping>> {
        let mut aliases = core::mem::take(&mut self.render_operand_aliases);
        for object in [self.scene_scratch.as_mut(), self.discard.as_mut(),
                       Some(&mut self.fragment_rce)] {
            if let Some(object) = object {
                aliases.push(object.mapping.take().ok_or(EIO)?, GFP_KERNEL)?;
            }
        }
        if let Some(tvb) = self.render_tvb.as_mut() {
            for mapping in core::mem::take(&mut tvb.block_mappings) {
                aliases.push(mapping, GFP_KERNEL)?;
            }
        }
        Ok(aliases)
    }

    fn take_sksm_vm_aliases(&mut self) -> KVec<mmu::KernelMapping> {
        core::mem::take(&mut self.sksm_vm_aliases)
    }

    fn map_persistent_client_aliases(&mut self, vm: &mmu::Vm) -> Result<KVec<mmu::KernelMapping>> {
        let mut aliases = KVec::new();
        if let Some(tvb) = self.render_tvb.as_mut() {
            aliases = tvb.map_cached_blocks(vm)?;
        }
        for object in [self.scene_scratch.as_mut(), self.discard.as_mut()] {
            if let Some(object) = object {
                aliases.push(object.map_alias_at(vm, object.iova(), mmu::PROT_GPU_SHARED_RW)?, GFP_KERNEL)?;
            }
        }
        aliases.push(self.fragment_rce.map_alias_at(
            vm, self.fragment_rce.iova(), mmu::PROT_GPU_SHARED_RO,
        )?, GFP_KERNEL)?;
        aliases.push(self.descriptors.object.map_range_into_range(
            vm,
            G17P_RENDER_3D_DESCRIPTOR_OFFSET
                ..G17P_RENDER_3D_DESCRIPTOR_OFFSET + mmu::UAT_PGSZ,
            self.descriptor_3d_render.iova()
                ..self.descriptor_3d_render.iova() + self.descriptor_3d_render.size() as u64,
            mmu::UAT_PGSZ as u64, mmu::PROT_GPU_SHARED_RO, false,
        )?, GFP_KERNEL)?;
        aliases.push(self.fragment_status.map_alias_at(
            vm, self.fragment_status_low.iova(), mmu::PROT_GPU_SHARED_RW,
        )?, GFP_KERNEL)?;
        Ok(aliases)
    }

    /// Called only after semantic paired completion and FList release.
    pub(crate) fn release_client_context(&mut self) {
        self._command_context = None;
    }

    /// A cross-VM handoff builds a fresh queue graph while the physical QID
    /// producer remains monotonic. Move the empty graph's three cursors to the
    /// retained producer before staging the next group, so firmware never
    /// walks the intentionally empty slots preceding that group.
    pub(crate) fn rebase_empty_queue_cursors(
        &mut self,
        submission_ordinal: u32,
    ) -> Result {
        if submission_ordinal == 0 {
            return Err(EINVAL);
        }
        let plan = G17PRenderItemPlan::new(
            submission_ordinal,
            g17p_render_tag16_enabled(),
        )
        .ok_or(EINVAL)?;
        core::sync::atomic::fence(Ordering::Acquire);
        let previous = self.snapshot()?;
        if !previous.job_list_empty
            || !previous.tiling.idle()
            || !previous.fragment.idle()
            || previous.tiling.write != 0
            || previous.fragment.write != 0
        {
            pr_err!(
                "G17P render rebase: fresh graph is not empty; owner-pid={} qids={}/{} ordinal={} job-list-empty={} tiling done/read/write={}/{}/{} fragment done/read/write={}/{}/{} (required: job-list empty, both queues done==read==write==0)\n",
                self.owner_pid,
                self.queue_pair.tiling,
                self.queue_pair.fragment,
                submission_ordinal,
                previous.job_list_empty,
                previous.tiling.done,
                previous.tiling.read,
                previous.tiling.write,
                previous.fragment.done,
                previous.fragment.read,
                previous.fragment.write,
            );
            return Err(EBUSY);
        }
        self.graph.with_bytes_mut(|raw| {
            for pointers in [Self::TILING_POINTERS, Self::FRAGMENT_POINTERS] {
                put_u32(raw, pointers, plan.base);
                put_u32(raw, pointers + 0x30, plan.base);
                put_u32(raw, pointers + 0x40, plan.base);
            }
            Ok(())
        })?;
        core::sync::atomic::fence(Ordering::SeqCst);
        Ok(())
    }

    /// Reactivate a completed graph cached across a VM handoff. Its private
    /// ring may end at an older physical-QID prefix, but no job or queue item
    /// remains live. Advance all three cursors together to the current shared
    /// producer and require a fresh context update when this graph is selected
    /// again; no mapping or descriptor backing is rebuilt.
    pub(crate) fn prepare_cached_reactivation(
        &mut self,
        submission_ordinal: u32,
    ) -> Result {
        let plan = G17PRenderItemPlan::new(
            submission_ordinal,
            g17p_render_tag16_enabled(),
        )
        .ok_or(EINVAL)?;
        core::sync::atomic::fence(Ordering::Acquire);
        let previous = self.snapshot()?;
        if !previous.job_list_empty
            || !previous.tiling.idle()
            || !previous.fragment.idle()
        {
            return Err(EBUSY);
        }
        self.graph.with_bytes_mut(|raw| {
            for pointers in [Self::TILING_POINTERS, Self::FRAGMENT_POINTERS] {
                put_u32(raw, pointers, plan.base);
                put_u32(raw, pointers + 0x30, plan.base);
                put_u32(raw, pointers + 0x40, plan.base);
            }
            Ok(())
        })?;
        self.context_update_pending = true;
        core::sync::atomic::fence(Ordering::SeqCst);
        Ok(())
    }

    /// True when a restage at `submission_ordinal` would be refused for a
    /// HOST bookkeeping mismatch alone, with the firmware provably idle.
    ///
    /// `restage_command`'s guard tests two independent things at once: whether
    /// the firmware is still running the previous group (job list, queue
    /// idleness) and whether the host's ordinal still agrees with the ring
    /// producer this graph has published. The second can drift on its own. A
    /// submission stages its items into the ring -- advancing `write` -- before
    /// the outer publication and the tag-14/tag-15 that make them visible; if
    /// any step in between fails, the ordinal never advances while `write` has
    /// moved on by one group. From then on every restage on this graph is
    /// refused for a mismatch that has nothing to do with the GPU, so the
    /// client is retired permanently by a single lost frame.
    ///
    /// Reported separately so the caller can rebuild the graph -- the same
    /// thing a cross-VM handoff does, and equally safe here because the
    /// condition includes an empty job list and two idle queues. It is never
    /// true while the firmware still owns the previous group.
    pub(crate) fn restage_admission_desynced(
        &mut self,
        submission_ordinal: u32,
    ) -> Result<bool> {
        if submission_ordinal == 0 || submission_ordinal >= G17P_RETAINED_RENDER_ORDINALS {
            return Ok(false);
        }
        let Some(plan) =
            G17PRenderItemPlan::new(submission_ordinal, g17p_render_tag16_enabled())
        else {
            return Ok(false);
        };
        core::sync::atomic::fence(Ordering::Acquire);
        let previous = self.snapshot()?;
        Ok(previous.job_list_empty
            && previous.tiling.idle()
            && previous.fragment.idle()
            && (previous.tiling.write != plan.base || previous.fragment.write != plan.base))
    }

    /// Describe the drift for the log that reports it.
    /// Tuple: (expected base, tiling write, fragment write).
    pub(crate) fn restage_admission_cursors(
        &mut self,
        submission_ordinal: u32,
    ) -> Result<(u32, u32, u32)> {
        let plan = G17PRenderItemPlan::new(submission_ordinal, g17p_render_tag16_enabled())
            .ok_or(EINVAL)?;
        let previous = self.snapshot()?;
        Ok((plan.base, previous.tiling.write, previous.fragment.write))
    }

    /// Refresh the canonical queue group's command-owned objects after the
    /// previous group has reached semantic completion.
    pub(crate) fn restage_command(
        &mut self,
        dev: &AsahiDevice,
        uat: &mmu::Uat,
        vm: &mmu::Vm,
        command: &g17_uapi::TranslatedRenderCommand,
        num_clusters: u32,
        command_buffer_state_words: [u32; 2],
        submission_ordinal: u32,
    ) -> Result {
        if submission_ordinal == 0 || submission_ordinal >= G17P_RETAINED_RENDER_ORDINALS {
            return Err(EINVAL);
        }
        if !Self::descriptor_context_valid(descriptor_context_id) {
            return Err(EINVAL);
        }
        // The retained graph keeps its VM's binding for its whole lifetime,
        // so the bound root cannot move between submissions on it.
        let render_root_slot = self.render_root_slot;
        let plan = G17PRenderItemPlan::new(submission_ordinal, g17p_render_tag16_enabled())
            .ok_or(EINVAL)?;
        core::sync::atomic::fence(Ordering::Acquire);
        let previous = self.snapshot()?;
        if !previous.job_list_empty || !previous.tiling.idle() || !previous.fragment.idle()
            || previous.tiling.write != plan.base || previous.fragment.write != plan.base
        {
            // This is the one place a render is refused because the PREVIOUS
            // group on this retained storage has not finished. It used to
            // return a bare EBUSY, which reaches Mesa as VK_ERROR_DEVICE_LOST
            // and destroys the client's context -- so the one boot that
            // reproduces it has to carry every input the guard tested, plus
            // which owner's storage this is. Name the owner (its pid and queue
            // pair), its ordinal, and each field with its actual value against
            // what was required.
            dev_err!(
                dev.as_ref(),
                "G17P render restage: previous group still owns this storage; owner-pid={} qids={}/{} ordinal={} base={} job-list-empty={} tiling done/read/write={}/{}/{} fragment done/read/write={}/{}/{} (required: job-list empty, both queues done==read==write, both writes == {})\n",
                self.owner_pid,
                self.queue_pair.tiling,
                self.queue_pair.fragment,
                submission_ordinal,
                plan.base,
                previous.job_list_empty,
                previous.tiling.done,
                previous.tiling.read,
                previous.tiling.write,
                previous.fragment.done,
                previous.fragment.read,
                previous.fragment.write,
                plan.base,
            );
            return Err(EBUSY);
        }
        // The staging image is scratch whose address never leaves this
        // function (see `staging_descriptors`). Take the retained one when
        // there is one and zero it exactly as a fresh allocation is zeroed;
        // otherwise allocate. It is handed back at the end of a successful
        // restage, so an early error simply drops it and the next submission
        // allocates again.
        let mut next_descriptors = match self.staging_descriptors.take() {
            Some(mut retained) if g17p_retain_staging() => {
                restage_step!(
                    "staging reset",
                    retained.object.vmap().map(|mut view| view.memset(0))
                );
                retained
            }
            _ => restage_step!("descriptor allocation", MappedObject::new_wc(
                dev,
                uat,
                G17P_RENDER_DESCRIPTOR_IMAGE_SIZE,
                mmu::UAT_PGSZ as u64,
                mmu::PROT_GPU_FW_SHARED_RW,
            )),
        };
        let render_layout =
            g17p_render_state_layout(&command.parameters, num_clusters).ok_or(EINVAL)?;
        if page_align(render_layout.tpc_size).ok_or(EOVERFLOW)?
            > self.render_target_memory.tpc.logical_size
        {
            return Err(ERANGE);
        }
        let render_target_addresses = restage_step!(
            "render-target-memory record",
            self.render_target_memory
                .prepare_rt_slot(submission_ordinal, &render_layout)
        );
        // Native retains command descriptor memory maps with the work queue;
        // DEFLAKE/TA-status/AXFB are graph-owned, not submission-parity
        // allocations. TMAP/HMTA rotate in render_target_memory and TPC is
        // separately persistent, so the surviving command-state layout is
        // capacity-stable and can be refreshed in place for every restage.
        if render_layout.total_size > self.render_state.logical_size
            || render_layout.aux_fb != self.render_layout.aux_fb
            || !self.render_user_aliases.all_reachable_from(vm)
        {
            dev_err!(
                dev.as_ref(),
                "G17P render restage: cached render-state layout is incompatible; old-size={:#x} new-size={:#x} old-aux={:#x} new-aux={:#x} aliases-reachable={} ordinal={}\n",
                self.render_state.logical_size,
                render_layout.total_size,
                self.render_layout.aux_fb,
                render_layout.aux_fb,
                self.render_user_aliases.all_reachable_from(vm),
                submission_ordinal,
            );
            return Err(ERANGE);
        }
        if g17p_render_tvb_enabled() {
            let tvb = self.render_tvb.as_mut().ok_or(EIO)?;
            if tvb.logical_size < render_layout.tvb_size
                || render_layout.tvb_blocks > tvb.block_count
                || !tvb.all_reachable_from(vm)
            {
                return Err(ERANGE);
            }
            // Native retains this pool and lets firmware recycle its pages.
            // Clearing its contents would destroy retained PM-owned state.
        }
        let next_timestamps = restage_step!("timestamp allocation", MappedObject::new_wc(
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
            G17P_CL_LOW_VA_START..G17P_CL_LOW_VA_END,
            render_layout.total_size,
            mmu::UAT_PGSZ as u64,
            mmu::PROT_GPU_FW_SHARED_RW,
        )?;
        let next_timestamps = MappedObject::new_wc_in_range(
            dev,
            vm,
            G17P_CL_LOW_VA_START..G17P_CL_LOW_VA_END,
            mmu::UAT_PGSZ,
            mmu::UAT_PGSZ as u64,
            mmu::PROT_GPU_FW_SHARED_RW,
        )?;
        let render_state_va = next_render_state.iova();
        let timestamp_va = next_timestamps.iova();
        let mut translated = *command;
        translated.parameters.context_base = render_state_va;
        translated.parameters.deflake_3 = render_state_va + G17P_RENDER_STATE_DEFLAKE as u64;
        translated.parameters.deflake_2 = translated.parameters.deflake_3 + 0x20;
        translated.parameters.deflake_1 = translated.parameters.deflake_3 + 0x2a0;
        translated.parameters.ta_status = render_state_va + G17P_RENDER_STATE_TA_STATUS as u64;
        translated.parameters.fragment_status = self.fragment_status_low.iova();
        translated.parameters.layermeta = render_target_addresses.layermeta;
        translated.parameters.heapmeta = render_target_addresses.heapmeta;
        translated.parameters.tpc = self.render_target_memory.tpc_va();
        translated.parameters.aux_fb = aux_fb_va;
        translated.parameters.aux_fb_page_count = G17P_RENDER_AUX_FB_PAGE_COUNT;
        translated.parameters.aux_fb_flags = g17p_aux_fb_flags(translated.flags);
        translated.parameters.tilemap = render_target_addresses.tilemap;
        if let Some(pm) = self.parameter_management.as_ref() {
            translated.parameters.parameter_buffer = pm.selected_scene_va();
            let (cycle, record_index) = pm.scene_registers(
                self.scene_scratch.as_ref().map(MappedObject::iova).ok_or(EINVAL)?,
            )?;
            translated.parameters.cycle = cycle;
            translated.parameters.record_index = record_index;
            if crate::util::g17p_verbose() {
                dev_info!(
                    dev.as_ref(),
                    "G17P retained PM: owner={:#x} scene-index={} scene={:#x} cycle={:#x} metric-register={:#x} manager-generation={} (pool and PBDesc preserved)\n",
                    pm.hwpb_state_va(), pm.selected_scene, pm.selected_scene_va(),
                    cycle, record_index, pm.manager_generation,
                );
            }
        }
        // NATIVE LAYOUT, macOS 26.6.2 2026-09-02 (recs3.txt, three passes,
        // byte-stable): a render pass owns ONE (start, end) pair, not one per
        // stage. The TA descriptor carries only a START pointer and leaves its
        // end field ZERO (+0x08fe = 0xfffffc2000024c68, +0x0906 = 0); the 3D
        // points its START at the SAME address and its END at start+8. The
        // tiler stamps the pass start, the fragment stamps the pass end.
        //
        // This driver published four distinct qwords and required all four to
        // be non-zero before calling a render complete, which the hardware can
        // never satisfy because nothing writes a TA end. That is why a render
        // whose completion ring showed BOTH stages finished with real
        // start/end timestamps was still abandoned after 2000 polls.
        translated.parameters.ta_timestamp_start = timestamp_va;
        translated.parameters.ta_timestamp_end = 0;
        translated.parameters.fragment_timestamp_start = timestamp_va;
        translated.parameters.fragment_timestamp_end = timestamp_va + 8;
        let (fragment_lifecycle, tiling_lifecycle) =
            g17p_allocate_render_lifecycle_pair(self.lifecycle_predecessor)?;
        translated.parameters.fragment_lifecycle = fragment_lifecycle;
        translated.parameters.lifecycle = tiling_lifecycle;
        if crate::util::g17p_verbose() {
            dev_info!(
                dev.as_ref(),
                "G17P render lifecycle: predecessor={:#x} 3D current={:#x} TA current={:#x} ordinal={}\n",
                self.lifecycle_predecessor,
                fragment_lifecycle as u32,
                tiling_lifecycle as u32,
                submission_ordinal,
            );
        }
        if *crate::module_parameters::g17p_render_eot_bind_zero.value() != 0 {
            translated.parameters.store_pipeline_bind = 0;
            translated.parameters.partial_store_pipeline_bind = 0;
            if crate::util::g17p_verbose() {
                dev_info!(
                    dev.as_ref(),
                    "G17P render EOT bind: forcing native-captured main/partial value 0\n",
                );
            }
        }
        if !user_timestamp_aliases
            .iter()
            .all(|mapping| vm.covers_range(mapping.iova(), mapping.size() as u64, false, true))
        {
            return Err(EFAULT);
        }
        if !self.render_user_aliases.all_reachable_from(vm) {
            return Err(EFAULT);
        }
        let status_flag = *crate::module_parameters::g17p_render_status_flag.value();
        self.render_state.with_bytes_mut(|raw| {
            encode_g17p_render_runtime_state(
                raw,
                &render_layout,
                translated.parameters.width,
                translated.parameters.height,
                &mut raw[G17P_RENDER_STATE_DEFLAKE..G17P_RENDER_STATE_DEFLAKE + mmu::UAT_PGSZ],
            )
            .ok_or(EINVAL)?;
            put_u32(raw, render_layout.aux_fb + 0x480, 0x6000_0000);
            put_u32(raw, render_layout.aux_fb + 0x484, 0x35b);
            Ok(())
        })?;

        let record_a_index = (submission_ordinal as usize * 2) % 35;
        let record_b_index = submission_ordinal as usize % 79;
        let support_va = self.support.iova();
        let objects = g17_render::RenderDescriptorObjects {
            record_a: support_va + G17P_RENDER_POOL_A as u64 + (record_a_index * 0x100) as u64,
            shared: support_va + G17P_RENDER_PACKED_SHARED as u64,
            record_b: support_va + G17P_RENDER_POOL_B as u64 + (record_b_index * 0x80) as u64,
            zero: support_va + G17P_RENDER_ZERO_SHARED as u64,
        };
        next_descriptors.with_bytes_mut(|raw| {
            let (tiling, fragment) = raw.split_at_mut(0x4000);
            g17_render::build_ta_descriptor(
                &translated.parameters,
                objects,
                g17_render::RenderDescriptorMetadata {
                    context_id: 1,
                    submission_ordinal,
                    queue_pair: 0,
                    submit_sequence: submission_ordinal as u64 * 2 + 1,
                },
                tiling,
            )
            .map_err(|_| EINVAL)?;
            g17_render::apply_g17p_retained_descriptor_fields(
                g17_render::RenderDescriptorKind::Tiling,
                G17P_BOOTSTRAP_TA_DESCRIPTOR_LOW_VA,
                g17_initdata::CONTROL_SHARED_ADDRESS,
                submission_ordinal,
                false,
                tiling,
            )
            .map_err(|_| EINVAL)?;
            g17_render::build_fragment_descriptor(
                &translated.parameters,
                objects,
                g17_render::RenderDescriptorMetadata {
                    context_id: 1,
                    submission_ordinal,
                    queue_pair: 0,
                    submit_sequence: submission_ordinal as u64 * 2,
                },
                fragment,
            )
            .map_err(|_| EINVAL)?;
            g17_render::apply_g17p_retained_descriptor_fields(
                g17_render::RenderDescriptorKind::Fragment,
                G17P_BOOTSTRAP_3D_DESCRIPTOR_LOW_VA,
                g17_initdata::CONTROL_SHARED_ADDRESS,
                submission_ordinal,
                false,
                fragment,
            )
            .map_err(|_| EINVAL)?;
            apply_g17p_generic_descriptor_tail(tiling, fragment).ok_or(EINVAL)
        })?;

        // Confine the staging map to this block. The `VMapRef` borrows
        // `next_descriptors` mutably for as long as it lives, and the retained
        // staging image is handed back to `self` after the copy.
        {
            let source = next_descriptors.object.vmap()?;
            let destination = self.descriptors.object.vmap()?;
            let support = self.support.object.vmap()?;
            let source = unsafe {
                core::slice::from_raw_parts(source.as_ptr(), G17P_RENDER_DESCRIPTOR_IMAGE_SIZE)
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
            let pool_a_kick = match G17PEventControlKick::prepare_reuse(
                support,
                support_va,
                record_a_index,
            ) {
                Some(kick) => kick,
                None => {
                    // The record was reusable when it was selected above and the
                    // manager lock is held throughout, so this can only mean
                    // firmware relinked it underneath us. Report the exact fields.
                    if let Some((slot_va, want_slot_va, completed, want_completed, membership)) =
                        G17PEventControlKick::reuse_fields(support, support_va, record_a_index)
                    {
                        dev_err!(
                            dev.as_ref(),
                            "G17P render restage: selected pool-A record {} became unusable ordinal={} slot_va={:#x} expected={:#x} completed={} expected={} membership={:#x}\n",
                            record_a_index,
                            submission_ordinal,
                            slot_va,
                            want_slot_va,
                            completed,
                            want_completed,
                            membership,
                        );
                    }
                    return Err(EBUSY);
                }
            };
            let ta_offset = g17p_render_ta_descriptor_offset(submission_ordinal);
            let fragment_offset = g17p_render_3d_descriptor_offset(submission_ordinal);
            destination
                .get_mut(ta_offset..ta_offset + g17_render::TA_DESCRIPTOR_SIZE)
                .ok_or(ERANGE)?
                .copy_from_slice(&source[..g17_render::TA_DESCRIPTOR_SIZE]);
            destination
                .get_mut(fragment_offset..fragment_offset + g17_render::FRAGMENT_DESCRIPTOR_SIZE)
                .ok_or(ERANGE)?
                .copy_from_slice(
                    &source[0x4000..0x4000 + g17_render::FRAGMENT_DESCRIPTOR_SIZE],
                );
            let record_a = G17P_RENDER_POOL_A + record_a_index * 0x100;
            g17_render::set_render_event_control_generation(
                &mut support[record_a..],
                command_buffer_state_words[0],
            )
            .map_err(|_| EINVAL)?;
            put_u32(support, record_a + 0x10, 0x50);
            pool_a_kick.apply(support);
            put_u32(
                support,
                G17P_RENDER_SHARED_CONTROL_INNER,
                submission_ordinal * 2 + 1,
            );
            put_u32(support, G17P_RENDER_FLAG, submission_ordinal + 1);
            core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
        }

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
        let old_timestamps = core::mem::replace(&mut self.timestamps, next_timestamps);
        self.fragment_mcache = fragment_mcache;
        self.pool_a_record_index = u8::try_from(record_a_index)?;
        self.ta_hardware_buffer_id = translated.parameters.ta_hardware_buffer_id;
        self.descriptor_ordinal = submission_ordinal;
        self.flist_generation = g17_submission::g17p_render_flist_generation(submission_ordinal);
        self.descriptor_context_id = descriptor_context_id;
        drop(old_timestamps);
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
        if !crate::util::g17p_verbose() {
            return;
        }
        let mut ta = [0u32; 8];
        let mut fragment = [0u32; 8];
        let ta_read = self.render_state.with_bytes_mut(|raw| {
            for (index, slot) in ta.iter_mut().enumerate() {
                let at = G17P_RENDER_STATE_TA_STATUS + index * 4;
                *slot = u32::from_le_bytes([raw[at], raw[at + 1], raw[at + 2], raw[at + 3]]);
            }
            Ok(())
        });
        let fragment_read = self.fragment_status.with_bytes_mut(|raw| {
            for (index, slot) in fragment.iter_mut().enumerate() {
                let at = index * 4;
                *slot = u32::from_le_bytes([raw[at], raw[at + 1], raw[at + 2], raw[at + 3]]);
            }
            Ok(())
        });
        let status_read_failed = ta_read.is_err() || fragment_read.is_err();
        match (ta_read, fragment_read) {
            (Ok(()), Ok(())) => {
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
            (ta_error, fragment_error) => dev_err!(
                dev.as_ref(),
                "G17P render status[{}]: read failed TA={:?} 3D={:?}\n",
                label,
                ta_error,
                fragment_error,
            ),
        }
        if g17p_render_target_scan_due(
            label,
            *crate::module_parameters::g17p_dep_records.value() != 0,
            status_read_failed,
        ) {
            self.log_render_target_state(dev, label);
        }
    }

    /// Dump the fixed-function render targets for diagnostic checkpoints.
    /// These are the exact objects TA produces and 3D/dIPP
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
        let state_read = self.render_state.with_bytes_mut(|raw| {
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
            copy_head(layout.aux_fb, &mut axfb_head)?;
            copy_head(layout.aux_fb + 0x480, &mut axfb_480)?;
            nonzero[3] = raw
                .get(
                    layout.aux_fb
                        ..layout
                            .aux_fb
                            .checked_add(G17P_RENDER_STATE_AUX_FB_SIZE)
                            .ok_or(EOVERFLOW)?,
                )
                .ok_or(ERANGE)?
                .iter()
                .filter(|byte| **byte != 0)
                .count();
            Ok(())
        });
        let rt_offset = g17p_render_rt_record_offset(self.descriptor_ordinal);
        let rt_read = self.render_target_memory.rt_memory.with_bytes_mut(|raw| {
            let tilemap_start = rt_offset;
            let heapmeta_start = rt_offset
                .checked_add(layout.heapmeta.checked_sub(layout.tilemap).ok_or(EINVAL)?)
                .ok_or(EOVERFLOW)?;
            let mut read_region =
                |start: usize, size: usize, out: &mut [u64; 8], count_index: usize| -> Result {
                for (index, value) in out.iter_mut().enumerate() {
                    let at = start.checked_add(index * 8).ok_or(EOVERFLOW)?;
                    *value = u64::from_le_bytes(
                        raw.get(at..at + 8)
                            .ok_or(ERANGE)?
                            .try_into()
                            .map_err(|_| EINVAL)?,
                    );
                }
                nonzero[count_index] = raw
                    .get(start..start.checked_add(size).ok_or(EOVERFLOW)?)
                    .ok_or(ERANGE)?
                    .iter()
                    .filter(|byte| **byte != 0)
                    .count();
                Ok(())
            };
            read_region(tilemap_start, layout.tilemap_size, &mut tilemap, 0)?;
            read_region(heapmeta_start, layout.heapmeta_size, &mut heapmeta, 1)?;
            Ok(())
        });
        let tpc_read = self.render_target_memory.tpc.with_bytes_mut(|raw| {
            for (index, value) in tpc.iter_mut().enumerate() {
                let at = index.checked_mul(8).ok_or(EOVERFLOW)?;
                *value = u64::from_le_bytes(
                    raw.get(at..at + 8)
                        .ok_or(ERANGE)?
                        .try_into()
                        .map_err(|_| EINVAL)?,
                );
            }
            nonzero[2] = raw
                .get(..layout.tpc_size)
                .ok_or(ERANGE)?
                .iter()
                .filter(|byte| **byte != 0)
                .count();
            Ok(())
        });
        let read = state_read.and(rt_read).and(tpc_read);
        // TMAP/HMTA rotate through the persistent render-target-memory ring;
        // TPC lives in its separate stable mapping. The TILING VERTEX BUFFER
        // is a separate object and was never
        // scanned, so "the tiler wrote nothing" could not be distinguished
        // from "the tiler wrote where we do not look" -- which matters,
        // because KTrace 0x43 reports status 3 (executed) for the TA and 4
        // (NOP-retired) only for the fragment, i.e. the tiler really does run.
        let mut first = usize::MAX;
        let mut window = [0u8; 64];
        let tvb_nonzero = self.render_tvb.as_mut().map(|tvb| {
            let block_count = tvb.block_count;
            let size = block_count * G17P_RENDER_TVB_BLOCK_SIZE;
            (|| -> Result<usize> {
                let mut count=0usize;
                for block in 0..block_count {
                    count+=tvb.with_block_bytes(block,|bytes| {
                        if first==usize::MAX {
                            if let Some(at)=bytes.iter().position(|byte| *byte!=0) {
                                first=block*G17P_RENDER_TVB_BLOCK_SIZE+at;
                                let start=at & !0xf;
                                for (index,slot) in window.iter_mut().enumerate() {
                                    *slot=bytes.get(start+index).copied().unwrap_or(0);
                                }
                            }
                        }
                        Ok(bytes.iter().filter(|byte| **byte!=0).count())
                    })?;
                }
                Ok(count)
            })()
            .map(|count| (count, size))
            .unwrap_or((usize::MAX, size))
        });
        if let Some((count, size)) = tvb_nonzero {
            // Where the tiler wrote matters as much as how much: 32 bytes in a
            // 1.2MB buffer is either a bare header or a single minimal tile
            // list, and the two say opposite things about whether geometry was
            // processed. Report the first non-zero offset and the bytes there.
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
                    self.render_target_memory
                        .tilemap_va(self.descriptor_ordinal).unwrap_or(0),
                    self.render_target_memory
                        .heapmeta_va(self.descriptor_ordinal, &layout).unwrap_or(0),
                    self.render_target_memory.tpc_va(),
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
                + pm.selected_scene * G17P_PM_SCENE_ENTRY_SIZE;
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
            && self.scene_scratch.as_ref().map_or(true, |scene| {
                scene.iova() == g17p_render_scene_scratch_va(self.queue_pair).unwrap_or(0)
                    && scene.logical_size == G17P_RENDER_SCENE_SCRATCH_MAPPING_SIZE
                    && vm.covers_range(
                        scene.iova(),
                        scene.logical_size as u64,
                        true,
                        true,
                    )
            })
            && self.discard.as_ref().map_or(true, |discard| {
                discard.iova() == g17p_render_discard_va(self.queue_pair).unwrap_or(0)
                    && discard.logical_size == G17P_PM_DISCARD_SIZE
                    && vm.covers_range(
                        discard.iova(),
                        discard.logical_size as u64,
                        true,
                        true,
                    )
            })
            && uat.kernel_vm().covers_range(
                self.fragment_status.iova(),
                self.fragment_status.logical_size as u64,
                false,
                false,
            )
            && vm.covers_range(
                self.fragment_status_low.iova(),
                self.fragment_status_low.size() as u64,
                true,
                true,
            )
            && self.render_tvb.as_ref().map_or(true, |tvb| {
                tvb.all_reachable_from(vm)
            })
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
            "G17P render reach: ranges={} ta_high(iova={:#x} size={:#x} covered={}) 3d_high(iova={:#x} covered={})\n",
            ranges_ok,
            self.descriptors.iova(),
            G17P_RENDER_TA_DESCRIPTOR_ARRAY_SIZE,
            uat.kernel_vm().covers_range(self.descriptors.iova(),
                G17P_RENDER_TA_DESCRIPTOR_ARRAY_SIZE as u64, true, true),
            self.descriptors.iova() + G17P_RENDER_3D_DESCRIPTOR_OFFSET as u64,
            uat.kernel_vm().covers_range(
                self.descriptors.iova() + G17P_RENDER_3D_DESCRIPTOR_OFFSET as u64,
                G17P_RENDER_3D_DESCRIPTOR_ARRAY_SIZE as u64, true, true),
        );
        dev_info!(
            dev.as_ref(),
            "G17P render reach: ta_low(iova={:#x} arena={:#x} covered={}) descriptors={:#x} graph={:#x} support={:#x} state={:#x}+{:#x} ts={:#x}\n",
            self.descriptor_ta_low.iova(),
            G17P_CL_CLIENT_LOW_VA_START,
            vm.covers_range(self.descriptor_ta_low.iova(),
                self.descriptor_ta_low.size() as u64, true, false),
            layout.descriptors,
            layout.graph,
            layout.support,
            layout.render_state,
            layout.render_state_size,
            layout.timestamps,
        );
        dev_info!(
            dev.as_ref(),
            "G17P render reach: compact deflake={:#x} 3d-status={:#x}/{:#x} scene={:#x} discard={:#x} tilemap={:#x} tpc={:#x} axfb={:#x} tvb-first={:#x} tvb-last={:#x} rce={:#x} descriptor3d={:#x} covered={}\n",
            self.render_user_aliases.deflake_va(),
            self.fragment_status_low.iova(),
            self.fragment_status.iova(),
            self.scene_scratch.as_ref().map_or(0, MappedObject::iova),
            self.discard.as_ref().map_or(0, MappedObject::iova),
            self.render_target_memory
                .tilemap_va(self.descriptor_ordinal).unwrap_or(0),
            self.render_target_memory.tpc_va(),
            self.render_user_aliases.aux_fb_va(),
            self.render_tvb.as_ref().map_or(0, G17PRenderTvb::iova),
            self.render_tvb.as_ref().map_or(0, |_| {
                g17p_render_tvb_block_va(self.queue_pair, G17P_PM_BLOCK_COUNT - 1).unwrap_or(0)
            }),
            self.fragment_rce.iova(),
            self.descriptor_3d_render.iova(),
            self.render_user_aliases.all_reachable_from(vm)
                && (self.render_target_memory.tpc_va()
                    == g17p_render_tpc_va(self.queue_pair).unwrap_or(0))
                && (self.render_target_memory.tpc.logical_size >= self.render_layout.tpc_size)
                && vm.covers_range(
                    self.render_target_memory.tpc_va(),
                    self.render_layout.tpc_size as u64,
                    true,
                    true,
                )
                && (self.render_target_memory.rt_memory.iova()
                    == g17p_render_rt_memory_va(self.queue_pair).unwrap_or(0))
                && vm.covers_range(
                    self.render_target_memory
                        .tilemap_va(self.descriptor_ordinal).unwrap_or(0),
                    self.render_layout.tilemap_size as u64,
                    true,
                    true,
                )
                && vm.covers_range(
                    self.render_target_memory
                        .rt_region_va(
                            self.descriptor_ordinal,
                            &self.render_layout,
                            self.render_layout.layermeta,
                        )
                        .unwrap_or(0),
                    (self.render_layout.layermeta_size + self.render_layout.heapmeta_size) as u64,
                    true,
                    true,
                )
                && self.scene_scratch.as_ref().map_or(true, |scene| {
                    scene.iova() == g17p_render_scene_scratch_va(self.queue_pair).unwrap_or(0)
                        && scene.logical_size == G17P_RENDER_SCENE_SCRATCH_MAPPING_SIZE
                        && vm.covers_range(
                            scene.iova(),
                            scene.logical_size as u64,
                            true,
                            true,
                        )
                })
                && self.discard.as_ref().map_or(true, |discard| {
                    discard.iova() == g17p_render_discard_va(self.queue_pair).unwrap_or(0)
                        && discard.logical_size == G17P_PM_DISCARD_SIZE
                        && vm.covers_range(
                            discard.iova(),
                            discard.logical_size as u64,
                            true,
                            true,
                        )
                })
                && uat.kernel_vm().covers_range(
                    self.fragment_status.iova(),
                    self.fragment_status.logical_size as u64,
                    false,
                    false,
                )
                && vm.covers_range(
                    self.fragment_status_low.iova(),
                    self.fragment_status_low.size() as u64,
                    true,
                    true,
                )
                && self.render_tvb.as_ref().map_or(true, |tvb| {
                    tvb.all_reachable_from(vm)
                })
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
        let sksm_backing = self.sksm_backing.as_ref();
        let layout_ok = G17PRenderStorageVaLayout {
            descriptors: self.descriptors.iova(),
            graph: self.graph.iova(),
            support: self.support.iova(),
            render_state: self.render_state.iova(),
            render_state_size: self.render_state.logical_size as u64,
            timestamps: self.timestamps.iova(),
        }
        .all_referenced_ranges_reachable(
            |address, size| {
                uat.kernel_vm()
                    .covers_range(address, size, true, true)
            },
            |address, size| vm.covers_range(address, size, true, true),
        )
            && self.descriptor_ta_high.iova()
                == g17_submission::G17P_COLD_OPENING_TA_DESCRIPTOR_BASE
            && self.descriptor_ta_high.size() == mmu::UAT_PGSZ
            && uat.kernel_vm().covers_range(
                self.descriptor_ta_high.iova(),
                self.descriptor_ta_high.size() as u64,
                true,
                true,
            )
            && self.descriptor_3d_high.iova()
                == g17_submission::G17P_COLD_OPENING_3D_DESCRIPTOR_BASE
            && self.descriptor_3d_high.size() == mmu::UAT_PGSZ
            && uat.kernel_vm().covers_range(
                self.descriptor_3d_high.iova(),
                self.descriptor_3d_high.size() as u64,
                true,
                true,
            )
            && self.descriptor_ta_low.iova() == G17P_BOOTSTRAP_TA_DESCRIPTOR_LOW_VA
            && self.descriptor_ta_low.size() == mmu::UAT_PGSZ
            && uat.kernel_lower_vm().covers_range(
                self.descriptor_ta_low.iova(),
                self.descriptor_ta_low.size() as u64,
                true,
                false,
            )
            && self.descriptor_3d_low.iova() == G17P_BOOTSTRAP_3D_DESCRIPTOR_LOW_VA
            && self.descriptor_3d_low.size() == mmu::UAT_PGSZ
            && uat.kernel_lower_vm().covers_range(
                self.descriptor_3d_low.iova(),
                self.descriptor_3d_low.size() as u64,
                true,
                false,
            )
            && uat.kernel_vm().covers_range(
                G17P_BOOTSTRAP_TA_CONTEXT_HIGH_VA,
                G17P_BOOTSTRAP_CONTEXT_PEER_SIZE as u64,
                true,
                true,
            )
            && self.ta_context_low.iova() == G17P_BOOTSTRAP_TA_CONTEXT_LOW_VA
            && self.ta_context_low.size() == G17P_BOOTSTRAP_CONTEXT_PEER_SIZE
            && uat.kernel_lower_vm().covers_range(
                self.ta_context_low.iova(),
                self.ta_context_low.size() as u64,
                true,
                false,
            )
            && uat.kernel_vm().covers_range(
                G17P_BOOTSTRAP_3D_CONTEXT_HIGH_VA,
                G17P_BOOTSTRAP_CONTEXT_PEER_SIZE as u64,
                true,
                true,
            )
            && self.fragment_context_low.iova() == G17P_BOOTSTRAP_3D_CONTEXT_LOW_VA
            && self.fragment_context_low.size() == G17P_BOOTSTRAP_CONTEXT_PEER_SIZE
            && uat.kernel_lower_vm().covers_range(
                self.fragment_context_low.iova(),
                self.fragment_context_low.size() as u64,
                true,
                false,
            )
            && self.render_ta_status.as_ref().is_some_and(|mapping| {
                mapping.iova() == G17P_BOOTSTRAP_TA_STATUS_VA
                    && mapping.size() == mmu::UAT_PGSZ
                    && uat.kernel_vm().covers_range(
                        G17P_BOOTSTRAP_TA_STATUS_VA,
                        mmu::UAT_PGSZ as u64,
                        true,
                        true,
                    )
            })
            && self.render_3d_status.as_ref().is_some_and(|mapping| {
                mapping.iova() == G17P_BOOTSTRAP_3D_STATUS_VA
                    && mapping.size() == mmu::UAT_PGSZ
                    && uat.kernel_vm().covers_range(
                        G17P_BOOTSTRAP_3D_STATUS_VA,
                        mmu::UAT_PGSZ as u64,
                        true,
                        true,
                    ),
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
            // The cold render FList publishes 17 initial blocks and five first-
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
                "pm-page-list",
                self.parameter_management.as_ref().map_or(
                    g17p_render_tvb_mode() < 2,
                    |pm| {
                        pm.page_list_low_va()
                            == g17p_render_page_list_va(self.queue_pair).unwrap_or(0)
                            && vm.covers_range(
                                pm.page_list_low_va(),
                                page_align(G17P_PM_PAGE_LIST_SIZE).unwrap_or(0) as u64,
                                true,
                                true,
                            )
                    },
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
    }

    fn optional_addresses(&self, tiling: bool) -> g17_submission::G17PColdOpeningOptionalAddresses {
        g17p_cold_opening_optional_addresses(self.support.iova(), tiling)
    }

    pub(crate) fn stage_fragment(&mut self, submission_ordinal: u32) -> Result {
        let graph_va = self.graph.iova();
        let items = [
            g17_submission::G17P_COLD_OPENING_3D_DESCRIPTOR_BASE,
            graph_va + Self::FRAGMENT_OPTIONAL as u64,
            graph_va + Self::FRAGMENT_EVENT as u64,
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
                &mut raw[Self::FRAGMENT_OPTIONAL
                    ..Self::FRAGMENT_OPTIONAL + g17_submission::G17P_COLD_OPENING_OPTIONAL_SIZE],
            )
            .map_err(|_| EINVAL)?;
            g17_submission::apply_g17p_retained_event(
                g17_submission::G17PColdOpeningStage::Fragment,
                submission_ordinal,
                &mut raw[Self::FRAGMENT_EVENT
                    ..Self::FRAGMENT_EVENT + g17_submission::G17P_COLD_OPENING_EVENT_SIZE],
            )
            .map_err(|_| EINVAL)?;
            put_u32(raw, Self::FRAGMENT_POINTERS + 0x40, 3);
            Ok(())
        })
    }

    pub(crate) fn prepare_late_tiling_publication(
        &mut self,
        submission_ordinal: u32,
    ) -> Result<G17PLateTilingPublication> {
        let graph_va = self.graph.iova();
        let bytes = build_g17p_late_tiling_bytes(
            g17_submission::G17P_COLD_OPENING_TA_DESCRIPTOR_BASE,
            graph_va,
            self.optional_addresses(true),
            submission_ordinal,
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
    pub(crate) fn new(dev: &AsahiDevice, uat: &mmu::Uat) -> Result<Self> {
        // AGXKSMKickQueue publishes this array through an uncached channel
        // allocation, followed by dmb ish and the addKicks MMIO pair. Keep the
        // CPU alias WC so that the freshly encoded entry is at the point of
        // coherency when the hardware consumes the doorbell.
        let entries = MappedObject::new_wc_in_range(
            dev,
            uat.kernel_lower_vm(),
            G17P_CL_LOW_VA_START..G17P_CL_LOW_VA_END,
            G17P_CL_ENTRY_STORAGE_SIZE,
            mmu::UAT_PGSZ as u64,
            mmu::PROT_GPU_FW_SHARED_RW,
        )?;
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
    hw_region_0: MappedObject,
    hw_region_1: MappedObject,
    fwctl: MappedObject,
    control_shared: MappedObject,
    control_shared_inner: MappedObject,
    partial_opening_control_shared: MappedObject,
    partial_opening_control_shared_inner: MappedObject,
    bootstrap_descriptor_zero_a: MappedObject,
    bootstrap_descriptor_zero_b: MappedObject,
    bootstrap_ta_context_peer: MappedObject,
    bootstrap_3d_context_peer: MappedObject,
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
            mmu::PROT_GPU_FW_SHARED_RW,
        )?;
        let mut partial_scheduler = MappedObject::new_wc_at(
            dev,
            address_space.uat.kernel_vm(),
            g17_submission::G17P_PARTIAL_OPENING_SCHEDULER_GPU_VA,
            g17_submission::G17P_PARTIAL_OPENING_PAGE_SIZE,
            mmu::PROT_GPU_FW_SHARED_RW,
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
            mmu::PROT_GPU_FW_SHARED_RW,
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
        let primary_b2_alias_0_low = partial_scheduler.map_alias_at(
            address_space.uat.kernel_lower_vm(),
            g17_initdata::REGION_VIEW_LOW_ADDRS[0],
            mmu::PROT_GPU_FW_SHARED_RW,
        )?;
        let primary_b2_alias_1_low = primary_b2_alias_1.map_alias_at(
            address_space.uat.kernel_lower_vm(),
            g17_initdata::REGION_VIEW_LOW_ADDRS[1],
            mmu::PROT_GPU_FW_SHARED_RW,
        )?;
        dev_info!(dev.as_ref(), "G17P resources: B2 aliases mapped\n");
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
        let mut control_operand_table = match RenderBackingObject::new(
            dev,
            g17_initdata::CONTROL_OPERAND_TABLE_SIZE,
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
            g17_initdata::encode_partial_opening_operand_table_pre_control(bytes)
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
            "G17P resources: render operand closure ready at {:#x}, {} buffers at {:#x}\n",
            G17P_CONTROL_OPERAND_TABLE_VA,
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
        // The classic CL producer appends its outer slot in this bundle and
        // advances the paired channel producer in primary_state. Both are
        // live host-to-firmware publication surfaces, not boot-only blobs.
        let mut shared_cluster = match MappedObject::new_wc(
            dev,
            &address_space.uat,
            g17_initdata::NATIVE_SHARED_CLUSTER_SIZE,
            mmu::UAT_PGSZ as u64,
            mmu::PROT_FW_PRIV_RW,
        ) {
            Ok(object) => object,
            Err(error) => {
                dev_err!(
                    dev.as_ref(),
                    "G17P resources: shared-cluster allocation failed ({:?})\n",
                    error
                );
                return Err(error);
            }
        };
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
        let hw_region_0 = map_fixed_object(
            dev,
            address_space.uat.kernel_vm(),
            "hardware region 0",
            shared_cluster.iova() + g17_initdata::NATIVE_HW_REGION_BUNDLE_OFFSETS[0] as u64,
            mmu::UAT_PGSZ,
            mmu::PROT_FW_SHARED_RW,
        )?;
        let hw_region_1 = map_fixed_object(
            dev,
            address_space.uat.kernel_vm(),
            "hardware region 1",
            shared_cluster.iova() + g17_initdata::NATIVE_HW_REGION_BUNDLE_OFFSETS[1] as u64,
            mmu::UAT_PGSZ,
            mmu::PROT_FW_SHARED_RW,
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
            ("roots", &roots),
            ("shared cluster", &shared_cluster),
            ("B2 sentinel", &primary_b2_sentinel),
            ("hardware region 0", &hw_region_0),
            ("hardware region 1", &hw_region_1),
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
                hw_region_0,
                hw_region_1,
                fwctl,
                control_shared,
                control_shared_inner,
                partial_opening_control_shared,
                partial_opening_control_shared_inner,
                bootstrap_descriptor_zero_a,
                bootstrap_descriptor_zero_b,
                bootstrap_ta_context_peer,
                bootstrap_3d_context_peer,
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

    /// Restore the source-built class-1 closure before publishing live
    /// compute-control records.
    pub(crate) fn prepare_compute_runtime_class1(&mut self) -> Result {
        if !self.control_counters()?.opening_retired() {
            return Err(EINVAL);
        }
        // The mature compute tail uses the 22-entry table. It replaces the
        // first-partial 28-entry render table only after that group retires.
        self.resources.control_operand_table.with_bytes_mut(|bytes| {
            g17_initdata::encode_control_operand_table(bytes).map_err(|_| EINVAL)
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
        self.resources.hw_region_0.with_bytes_mut(|bytes| {
            bytes.fill(0);
            Ok(())
        })?;
        self.resources.hw_region_1.with_bytes_mut(|bytes| {
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
            .control_operand_table
            .with_bytes_mut(|bytes| {
                g17_initdata::encode_partial_opening_operand_table_pre_control(bytes)
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
        let region_record_addrs = [
            self.resources.hw_region_0.iova(),
            self.resources.hw_region_1.iova(),
        ];
        self.resources.shared_cluster.with_bytes_mut(|bytes| {
            g17_initdata::encode_hw_data(
                &g17_initdata::T8140_REGISTER_WINDOWS,
                &g17_initdata::T8140_REGISTER_FLAG_ONLY_SLOTS,
                region_record_addrs,
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
    ) -> Result<Option<(u32, [u32; 4])>> {
        let Some((index, page_list_address, page_count)) = descriptor else {
            return Ok(None);
        };
        let offset = g17_submission::g17p_pbdesc_offset(index);
        let mut previous = [0u32; 4];
        let mut published = [0u32; 4];
        self.resources.pb_descriptor_table.with_bytes_mut(|bytes| {
            let end = offset.checked_add(g17_submission::G17P_PBDESC_ENTRY_SIZE).ok_or(EINVAL)?;
            if end > bytes.len() {
                return Err(EINVAL);
            }
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
        if crate::util::g17p_verbose() {
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
        }
        Ok(Some((index, previous)))
    }

    fn restore_render_parameter_buffer_descriptor(
        &mut self,
        staged: Option<(u32, [u32; 4])>,
    ) -> Result {
        let Some((index, previous)) = staged else {
            return Ok(());
        };
        let offset = g17_submission::g17p_pbdesc_offset(index);
        self.resources.pb_descriptor_table.with_bytes_mut(|bytes| {
            let end = offset
                .checked_add(g17_submission::G17P_PBDESC_ENTRY_SIZE)
                .ok_or(EINVAL)?;
            if end > bytes.len() {
                return Err(EINVAL);
            }
            for (word, value) in previous.into_iter().enumerate() {
                let at = offset + word * 4;
                bytes[at..at + 4].copy_from_slice(&value.to_le_bytes());
            }
            Ok(())
        })?;
        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
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
        if !crate::util::g17p_verbose() {
            return;
        }
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

    /// Read the selected physical 3D queue's post-maintenance completion stamp.
    ///
    /// B1's timestamp queue unlinks EventControl at 0x16b48, performs GPU
    /// cache maintenance, then writes the first pointer from fragment stamp
    /// info (+0x2140) at 0x16ea0. Page B is firmware-private bookkeeping;
    /// page A is the host-visible completion witness, as on M1/M2.
    pub(crate) fn render_fragment_completion_stamp(
        &mut self,
        pair: G17PRenderQueuePair,
    ) -> Result<u32> {
        let offset = usize::from(pair.fragment) * core::mem::size_of::<u32>();
        self.resources.bootstrap_descriptor_zero_a.with_bytes_mut(|bytes| {
            let word = bytes.get(offset..offset + 4).ok_or(ERANGE)?;
            // SAFETY: this WC-mapped allocation is page aligned, each QID's
            // lane is u32 aligned, and the checked slice covers that lane.
            // Firmware updates it with one aligned u32 store. The acquire
            // load orders subsequent CPU reads after the completion witness.
            let stamp = unsafe {
                &*word.as_ptr().cast::<core::sync::atomic::AtomicU32>()
            };
            Ok(u32::from_le(stamp.load(core::sync::atomic::Ordering::Acquire)))
        })
    }

    /// Sample the first four per-queue stamp lanes of the shared "descriptor
    /// zero" pages -- A (GPU and firmware) and B (firmware-private) -- that the
    /// TA/3D records point at by their own QID. DRAM only, safe after a
    /// timeout; QIDs 0/1 belong to render pair 0 and 2/3 to pair 1.
    pub(crate) fn render_stamp_lanes(&mut self) -> Result<[[u32; 4]; 2]> {
        let mut lanes = [[0u32; 4]; 2];
        for (page, object) in [
            &mut self.resources.bootstrap_descriptor_zero_a,
            &mut self.resources.bootstrap_descriptor_zero_b,
        ]
        .into_iter()
        .enumerate()
        {
            object.with_bytes_mut(|bytes| {
                for (qid, lane) in lanes[page].iter_mut().enumerate() {
                    *lane = u32::from_le_bytes(
                        bytes
                            .get(qid * 4..qid * 4 + 4)
                            .ok_or(ERANGE)?
                            .try_into()
                            .map_err(|_| EINVAL)?,
                    );
                }
                Ok(())
            })?;
        }
        Ok(lanes)
    }

    /// Validate a retained storage's indexed PBDesc entry on owner switches.
    /// The table has one entry per HardwareBufferID, not a single current
    /// descriptor to retarget. Firmware owns the live allocation cursors and
    /// flags: running the cold initializer here destroys halStorePBDesc state.
    pub(crate) fn stage_render_parameter_buffer_view(
        &mut self,
        storage: &mut G17PUserRenderStorage,
    ) -> Result {
        self.stage_render_parameter_buffer_page_list(storage)?;
        let Some((index, page_list_address, page_count)) =
            storage.parameter_buffer_descriptor()? else { return Ok(()); };
        let offset = g17_submission::g17p_pbdesc_offset(index);
        self.resources.pb_descriptor_table.with_bytes_mut(|bytes| {
            let end = offset.checked_add(g17_submission::G17P_PBDESC_ENTRY_SIZE)
                .ok_or(EINVAL)?;
            if end > bytes.len() { return Err(EINVAL); }
            let mut previous = [0u32; 4];
            for (word, value) in previous.iter_mut().enumerate() {
                let at = offset + word * 4;
                *value = u32::from_le_bytes(bytes[at..at + 4].try_into().map_err(|_| EINVAL)?);
            }
            let expected = g17_submission::encode_g17p_pbdesc(
                previous, page_list_address, page_count,
                g17_submission::decode_g17p_pbdesc(previous),
            );
            // A different address/count means the retained identity was lost.
            // Do not repair it while firmware may retain that other owner.
            if previous != expected { return Err(EIO); }
            Ok(())
        })
    }

    pub(crate) fn stage_render_parameter_buffer_page_list(
        &mut self,
        storage: &mut G17PUserRenderStorage,
    ) -> Result {
        let pm = storage.parameter_management.as_mut().ok_or(EINVAL)?;
        let page_list_offset = pm.layout.page_list;
        let page_list_vas = pm.page_list_vas;
        let mut first = [0u32; 8];
        pm.backing.with_bytes_mut(|bytes| {
            for (index, value) in first.iter_mut().enumerate() {
                let offset = page_list_offset + index * 4;
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
        if crate::util::g17p_verbose() {
            dev_info!(
                self.dev().as_ref(),
                "G17P render PM: staged native PB page-list aliases=[{:#x},{:#x}] first={:x?}\n",
                page_list_vas[0],
                page_list_vas[1],
                first,
            );
        }
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
            .control_shared
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
    /// grow. Native appends five 1-MiB blocks after the 0x1100 initial pages.
    pub(crate) fn log_render_uma_growth(&mut self, checkpoint: &str) {
        let mut state = [0u32; 7];
        let state_result = self
            .resources
            .control_shared
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
        let mut appended = [0u64; 5];
        let page_result = self.resources.control_operand_page_lists.with_bytes_mut(|bytes| {
            let start = 0x1100usize * 8;
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
                "G17P render UMA growth[{}]: state(+20,+24,+28,+2c,+40,+44,+48)={:x?} page-list[0x1100..]={:x?}\n",
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
        command_buffer_state_words: [u32; 2],
        submission_ordinal: u32,
        parameter_manager_generation: Option<u64>,
        retained_sksm_backing: &mut Option<G17PRenderSksmBacking>,
    ) -> Result<G17PUserRenderStorage> {
        G17PUserRenderStorage::new(
            dev,
            &self.address_space.uat,
            &mut self.resources.bootstrap_ta_context_peer,
            &mut self.resources.bootstrap_3d_context_peer,
            vm,
            command,
            num_clusters,
            owner_pid,
            command_context,
            descriptor_context_id,
            render_root_slot,
            command_buffer_state_words,
            submission_ordinal,
            parameter_manager_generation,
            retained_sksm_backing,
        )?;
        let mut staged_descriptor = None;
        let finish = (|| -> Result {
            let aliases = storage.take_persistent_client_mappings()?;
            vm.install_render_pool_mappings(queue_pair.slot(), storage.render_pool_id(),
                storage.render_tvb.as_ref().map_or(0, |tvb| tvb.block_count), aliases)?;
            // The VM may already have executed compute or an earlier render
            // generation through its independently allocated descriptor
            // context. Generic mapping teardown flushes only the bind context;
            // publish this replacement to every TTBAT context carrying the
            // same root before any descriptor can name it.
            self.address_space.uat.flush_t8140_vm_contexts(vm)?;
            self.stage_render_parameter_buffer_page_list(&mut storage)?;
            staged_descriptor = self.stage_render_parameter_buffer_descriptor(
                storage.parameter_buffer_descriptor()?,
            )?;
            // Queue-context aliases belong to the installed physical QID and
            // this VM, not to the command-generation pool cleared on every
            // graph handoff. Commit them only after every other fallible stage
            // so a constructor failure cannot strand an alias whose high
            // backing was never retained.
            vm.append_driver_mappings(storage.take_sksm_vm_aliases())?;
            Ok(())
        })();
        if let Err(error) = finish {
            // A constructor is not firmware-visible yet. Undo both global and
            // VM-owned mutations in reverse publication order so a retry sees
            // neither a PBDesc pointing at freed PM memory nor an occupied
            // render-pool generation. The append above allocates before it
            // consumes its mappings, so an append failure left no VM aliases.
            //
            // ENOENT here can now only mean the VM's root left the TTBAT table
            // between the precondition above and this point, which is the same
            // permanent condition and not something a retry can improve. Record
            // it so this VM's later submissions fail instead of hanging.
            if error == ENOENT {
                vm.status().record(error.to_errno());
            }
            let restore = self.restore_render_parameter_buffer_descriptor(staged_descriptor);
            vm.clear_render_pool_mappings(queue_pair.slot());
            restore?;
            if had_retained_sksm_backing {
                *retained_sksm_backing = Some(storage.take_sksm_backing()?);
            }
            return Err(error);
        }
        Ok(storage)
    }

    /// Prepare the incoming VM without changing the active execution root.
    /// Mapping failure drops only its new aliases; prior clients remain live.
    /// Grow only at the manager's paired-retirement boundary. Every backing,
    /// mapping and CPU view is prepared before publishing the enlarged native
    /// PM inventory; a failed allocation leaves the previous capacity intact.
    pub(crate) fn grow_render_tvb(
        &mut self, storage: &mut G17PUserRenderStorage, vm: &mmu::Vm, blocks: usize,
    ) -> Result {
        self.grow_render_tvb_inner(storage, vm, blocks, false)
    }

    /// Firmware event 6 suspends allocation until the new block inventory is
    /// committed. PBDesc, PBState and the live page list remain firmware-owned.
    pub(crate) fn grow_render_tvb_synchronously(
        &mut self, storage: &mut G17PUserRenderStorage, vm: &mmu::Vm,
    ) -> Result<bool> {
        let before = storage.tvb_capacity();
        let target = g17p_tvb_grow_target(before, G17P_PM_MAX_OWNED_BLOCKS);
        if target <= before { return Ok(false); }
        self.grow_render_tvb_inner(storage, vm, target, true)?;
        Ok(storage.tvb_capacity() > before)
    }

    fn grow_render_tvb_inner(
        &mut self, storage: &mut G17PUserRenderStorage, vm: &mmu::Vm, blocks: usize,
        synchronous: bool,
    ) -> Result {
        let pool_id=storage.render_pool_id();
        let render_slot=storage.queue_pair.slot();
        let tvb=storage.render_tvb.as_mut().ok_or(EINVAL)?;
        let previous_blocks=tvb.block_count;
        if blocks <= previous_blocks { return Ok(()); }
        if vm.render_pool_tvb_blocks(render_slot, pool_id) != Some(previous_blocks) || !tvb.all_reachable_from(vm) {
            return Err(EFAULT);
        }
        let growth=tvb.prepare_growth(self.dev(),vm,blocks)?.ok_or(EINVAL)?;
        let pm=storage.parameter_management.as_mut().ok_or(EIO)?;
        let page_count=blocks.checked_mul(G17P_PM_PAGES_PER_BLOCK)
            .and_then(|count| u32::try_from(count).ok()).ok_or(EOVERFLOW)?;
        let logical_size=(blocks-1).checked_mul(G17P_RENDER_TVB_BLOCK_STRIDE)
            .and_then(|size| size.checked_add(G17P_RENDER_TVB_BLOCK_SIZE)).ok_or(EOVERFLOW)?;
        let added_blocks=blocks.checked_sub(previous_blocks).ok_or(EINVAL)?;
        let pm_layout=pm.layout;
        let control=pm.backing.with_bytes_mut(|pm_raw| {
            if pm_raw.len()<pm_layout.total_size { return Err(ERANGE); }
            g17p_pm_control_cursors(pm_raw,pm_layout.shared_control).ok_or(ERANGE)
        })?;
        let ring=g17p_pm_growth_plan(control.read,control.committed,control.total,added_blocks)
            .ok_or(EIO)?;
        let available_pages=ring.available_blocks().checked_mul(G17P_PM_PAGES_PER_BLOCK)
            .ok_or(EOVERFLOW)?;
        let mut added_page_bases=KVec::with_capacity(added_blocks,GFP_KERNEL)?;
        for added in 0..added_blocks {
            let slot=ring.added_slot(added).ok_or(EIO)?;
            let block=previous_blocks.checked_add(added).ok_or(EOVERFLOW)?;
            let first=g17p_render_tvb_page_id(storage.queue_pair,block).ok_or(EINVAL)?;
            first.checked_add((G17P_PM_PAGES_PER_BLOCK-1) as u32).ok_or(EOVERFLOW)?;
            added_page_bases.push((slot,first),GFP_KERNEL)?;
        }
        let mut rebuilt_page_list=KVec::with_capacity(
            if synchronous { 0 } else { available_pages },GFP_KERNEL)?;
        if !synchronous {
            pm.backing.with_bytes_mut(|pm_raw| {
                if g17p_pm_control_cursors(pm_raw,pm_layout.shared_control)!=Some(control) {
                    return Err(EAGAIN);
                }
                for ordinal in 0..ring.available_blocks() {
                    let slot=ring.available_slot(ordinal).ok_or(EIO)?;
                    let first=if ordinal<ring.retained_blocks {
                        let at=pm_layout.block_page_bases+slot*8;
                        u32::from_le_bytes(pm_raw.get(at..at+4).ok_or(ERANGE)?
                            .try_into().map_err(|_| ERANGE)?)
                    } else {
                        let added=ordinal-ring.retained_blocks;
                        added_page_bases[added].1
                    };
                    first.checked_add((G17P_PM_PAGES_PER_BLOCK-1) as u32).ok_or(EOVERFLOW)?;
                    for page in 0..G17P_PM_PAGES_PER_BLOCK {
                        rebuilt_page_list.push(first+page as u32,GFP_KERNEL)?;
                    }
                }
                Ok(())
            })?;
        }
        let descriptor_offset=g17_submission::g17p_pbdesc_offset(pm.hardware_buffer_id);
        let page_list_address=pm.page_list_low_va();
        let descriptor=&mut self.resources.pb_descriptor_table;
        pm.backing.with_bytes_mut(|pm_raw| {
            let commit = |desc_raw: Option<&mut [u8]>| {
                    if pm_raw.len()<pm_layout.total_size
                        || tvb.extensions.len()==tvb.extensions.capacity()
                    { return Err(ERANGE); }
                    if g17p_pm_control_cursors(pm_raw,pm_layout.shared_control)!=Some(control) {
                        return Err(EAGAIN);
                    }
                    // Only a retired scene may rebuild PBDesc. A synchronous
                    // request leaves its current firmware-owned words alone.
                    let descriptor_update = if let Some(desc_raw) = desc_raw {
                        let descriptor_end=descriptor_offset.checked_add(g17_submission::G17P_PBDESC_ENTRY_SIZE).ok_or(EOVERFLOW)?;
                        if descriptor_end>desc_raw.len() { return Err(ERANGE); }
                        let mut previous=[0u32;4];
                        for (word,value) in previous.iter_mut().enumerate() {
                            let at=descriptor_offset+word*4;
                            *value=u32::from_le_bytes(desc_raw[at..at+4].try_into().map_err(|_| EINVAL)?);
                        }
                        let encoded=g17_submission::encode_g17p_pbdesc(previous,page_list_address,page_count,
                            g17_submission::G17PPbDescRingState::default());
                        Some((desc_raw, encoded))
                    } else {
                        None
                    };
                    // This is the last fallible step. New aliases and the
                    // chosen publication are ready before any visible write.
                    vm.append_render_pool_mappings(render_slot,pool_id,previous_blocks,blocks,growth.mappings)?;
                    for (slot,first) in added_page_bases.into_iter() {
                        put_u32(pm_raw,pm_layout.block_page_bases+slot*8,first);
                        put_u32(pm_raw,pm_layout.block_page_bases+slot*8+4,0);
                    }
                    if let Some((desc_raw, encoded)) = descriptor_update {
                        for (entry,value) in rebuilt_page_list.into_iter().enumerate() {
                            put_u32(pm_raw,pm_layout.page_list+entry*4,value);
                        }
                        // buildPageList (25G83 0xfffffe0008a71908) walks the block
                        // ring from shared +0x08 to the staged target, wrapping at
                        // 0xd1a, then publishes those exact cursors at PBState
                        // +0x40/+0x3c. It does not clear the unused page-list tail.
                        let state=pm_layout.hwpb_state;
                        put_u32(pm_raw,state+0x34,page_count);
                        put_u32(pm_raw,state+0x3c,ring.target_cursor as u32);
                        put_u32(pm_raw,state+0x40,ring.read_cursor as u32);
                        put_u32(pm_raw,state+0x54,page_count-1);
                        for (word,value) in encoded.into_iter().enumerate() {
                            put_u32(desc_raw,descriptor_offset+word*4,value);
                        }
                    }
                    let length=tvb.extensions.len();
                    tvb.extensions.spare_capacity_mut()[0].write(growth.extension);
                    // SAFETY: prepare_growth reserved one extension entry;
                    // this commit initialized that exact spare slot once.
                    unsafe { tvb.extensions.set_len(length+1); }
                    tvb.block_count=blocks;
                    tvb.logical_size=logical_size;
                    // Publish the completed block entries after their backing
                    // and aliases have been retained. Firmware alone owns +8.
                    put_u32(pm_raw,pm_layout.shared_control+G17P_PM_SHARED_TOTAL_CURSOR,
                        ring.target_cursor as u32);
                    core::sync::atomic::fence(Ordering::SeqCst);
                    put_u32(pm_raw,pm_layout.shared_control+G17P_PM_SHARED_COMMITTED_CURSOR,
                        ring.target_cursor as u32);
                    Ok(())
            };
            if synchronous {
                // growImmediately does not load or map PBDesc. A missing
                // descriptor CPU view must not prevent a live TVB allocation.
                commit(None)
            } else {
                descriptor.with_bytes_mut(|desc_raw| commit(Some(desc_raw)))
            }
        })?;
        pm.page_count=page_count;
        core::sync::atomic::fence(Ordering::SeqCst);
        dev_info!(self.dev().as_ref(),"G17P render TVB grew: blocks {} -> {} pages={} retained-backing=true\n",
            previous_blocks,blocks,page_count);
        Ok(())
    }

    pub(crate) fn cache_render_vm_mappings(
        &mut self, storage: &mut G17PUserRenderStorage, vm: &mmu::Vm,
    ) -> Result {
        let render_slot = storage.queue_pair.slot();
        if let Some(mapped_blocks)=vm.render_pool_tvb_blocks(render_slot, storage.render_pool_id()) {
            let blocks=storage.tvb_capacity();
            if mapped_blocks==blocks { return Ok(()); }
            let tvb=storage.render_tvb.as_mut().ok_or(EIO)?;
            let aliases=tvb.map_cached_blocks_from(vm,mapped_blocks)?;
            return vm.append_render_pool_mappings(render_slot,storage.render_pool_id(),mapped_blocks,blocks,aliases);
        }
        vm.clear_render_pool_mappings(render_slot);
        let aliases = storage.map_persistent_client_aliases(vm)?;
        vm.append_driver_mappings(self.stage_user_render_flist(vm)?)?;
        vm.install_render_pool_mappings(render_slot, storage.render_pool_id(),
            storage.render_tvb.as_ref().map_or(0, |tvb| tvb.block_count), aliases)
    }

    /// Upgrade an ordinary compute-capable client VM to the complete native
    /// render FList aperture when its first TA/3D submission arrives.
    ///
    /// Block 21 cannot live in the device-global context-0 root: it
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
            // Ordinary Vulkan VMs start with 21 and enter this arm for the one
            // render-only alias in the native 17+5 active set.
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

    /// Initialize once per retained pool, not once per command lease. Native
    /// prepareBufferResources (8aa1bb8) gates populateInitialFirmwareState on
    /// the FList's initialized bit. Replaying the cold 17-block list here would
    /// discard the five-block grow and firmware-updated cursors on every pair.
    pub(crate) fn stage_render_flist_state(&mut self) -> Result {
        if self.flist_backing_owner == G17PFListBackingOwner::Render {
            return Ok(());
        }
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
            .control_shared
            .with_bytes_mut(|bytes| {
                g17_initdata::encode_render_control_shared(bytes).map_err(|_| EINVAL)
            })?;
        self.resources
            .control_shared_inner
            .with_bytes_mut(|bytes| {
                bytes.fill(0);
                bytes[..8].copy_from_slice(&g17_initdata::CONTROL_SHARED_INNER_PRESENTED.to_le_bytes());
                Ok(())
            })?;
        // The UMA descriptor is live pool state, not per-client command
        // state. Native pairs halLoadUMADesc with halStoreUMADesc so firmware
        // updates to its counters survive later clients. Publish it only at
        // the same cold-pool boundary as the FList objects above; replaying it
        // from new_user_render_storage overwrote those live counters whenever
        // another client first rendered.
        self.stage_render_uma_page_pool_descriptor()?;
        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
        self.flist_backing_owner = G17PFListBackingOwner::Render;
        Ok(())
    }

    pub(crate) fn render_flist_owned(&self) -> bool {
        self.flist_backing_owner == G17PFListBackingOwner::Render
    }

    /// Prepare the independent compute FList once. Native obtains both low
    /// addresses from the resource objects (populateInitialFirmwareState
    /// +0x14/+0x30); no render pool state or mapping is rewritten here.
    pub(crate) fn stage_compute_flist_state(&mut self) -> Result {
        if self.resources.compute_operand_table.is_none() {
            let pages = RenderBackingObject::new(&self.dev, G17P_CONTROL_OPERAND_PAGE_LIST_SIZE)?;
            let table = RenderBackingObject::new(&self.dev, G17P_CONTROL_OPERAND_TABLE_BACKING_SIZE)?;
            let mut buffers = KVec::with_capacity(23, GFP_KERNEL)?;
            for _ in 0..23 {
                buffers.push(RenderBackingObject::new(&self.dev,
                    G17P_CONTROL_OPERAND_BUFFER_SIZE)?, GFP_KERNEL)?;
            }
            let pool_id = u64::from(g17p_allocate_object_id()?);
            self.resources.compute_operand_page_lists = Some(pages);
            self.resources.compute_operand_table = Some(table);
            self.resources.compute_operand_buffers = buffers;
            self.resources.compute_flist_pool_id = pool_id;
        }
        if self.resources.compute_flist_initialized {
            return Ok(());
        }
        self.resources.partial_opening_control_shared.with_bytes_mut(|bytes| {
            g17_initdata::encode_compute_partial_opening_control_shared(bytes)
                .map_err(|_| EINVAL)
        })?;
        self.resources.partial_opening_control_shared_inner.with_bytes_mut(|bytes| {
            bytes.fill(0);
            Ok(())
        })?;
        self.resources.compute_operand_page_lists.as_mut().ok_or(EIO)?
            .with_bytes_mut(|bytes| {
                g17_initdata::encode_compute_flist_page_list(bytes).map_err(|_| EINVAL)
            })?;
        self.resources.compute_operand_table.as_mut().ok_or(EIO)?
            .with_bytes_mut(|bytes| {
                if encode_g17p_compute_flist_runs(bytes) { Ok(()) } else { Err(EINVAL) }
            })?;
        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
        self.resources.compute_flist_initialized = true;
        Ok(())
    }

    /// Cache only driver-owned compute aliases on first use by this VM. The
    /// disjoint aperture allows render aliases (including TVB) to stay mapped.
    pub(crate) fn cache_compute_vm_mappings(&mut self, vm: &mmu::Vm) -> Result {
        self.stage_compute_flist_state()?;
        let pool_id = self.resources.compute_flist_pool_id;
        if vm.compute_pool_mappings_match(pool_id) {
            return Ok(());
        }
        vm.clear_compute_pool_mappings();
        let mut mappings = KVec::with_capacity(26, GFP_KERNEL)?;
        mappings.push(self.resources.compute_operand_page_lists.as_mut().ok_or(EIO)?
            .map_at(vm, g17_initdata::COMPUTE_FLIST_PAGE_LIST_ADDRESS,
                mmu::PROT_GPU_SHARED_RW)?, GFP_KERNEL)?;
        mappings.push(self.resources.compute_operand_table.as_mut().ok_or(EIO)?
            .map_at(vm, g17_initdata::COMPUTE_FLIST_RUN_TABLE_ADDRESS,
                mmu::PROT_GPU_SHARED_RW)?, GFP_KERNEL)?;
        for (index, buffer) in self.resources.compute_operand_buffers.iter_mut().take(22).enumerate() {
            mappings.push(buffer.map_at(vm, g17p_compute_operand_buffer_va(index).ok_or(EINVAL)?,
                mmu::PROT_GPU_SHARED_RW)?, GFP_KERNEL)?;
        }
        // Completion rings belong to the VM aperture, not to one queue.
        // Multiple independent QIDs in the same VM must share these aliases.
        let (completion0, completion2) = self.map_user_completion_aliases(vm)?;
        mappings.push(completion0, GFP_KERNEL)?;
        mappings.push(completion2, GFP_KERNEL)?;
        vm.install_compute_pool_mappings(pool_id, mappings)
    }

    /// Install every hardware-fixed low alias before the VM is exposed to
    /// userspace and retain it for that VM's complete lifetime.
    pub(crate) fn install_user_vm_aliases(
        &mut self,
        vm: &mmu::Vm,
        bootstrap_vdm: bool,
    ) -> Result {
        let mut mappings = KVec::with_capacity(
            G17P_CONTROL_OPERAND_BUFFER_COUNT + if bootstrap_vdm { 5 } else { 3 },
            GFP_KERNEL,
        )?;
        let mut reserved_ranges = KVec::new();
        reserved_ranges.push(G17P_CL_LOW_VA_START..G17P_CL_LOW_VA_END, GFP_KERNEL)?;
        mappings.push(
            match map_fresh_zero_object_at(
                &self.dev,
                vm,
                G17P_CONTROL_DIRECTORY_VA,
                G17P_CONTROL_DIRECTORY_SIZE,
            ) {
                Ok(mapping) => mapping,
                Err(error) => {
                    dev_err!(
                        self.dev.as_ref(),
                        "G17P VM: control-directory alias failed ({:?})\n",
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
        for (index, buffer) in self
            .resources
            .control_operand_buffers
            .iter_mut()
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
            "G17P VM: operand graph mapped GPU-shared; entry18={:#x}\n",
            g17p_control_operand_buffer_va(18).ok_or(EINVAL)?
                | g17_initdata::CONTROL_OPERAND_ENTRY_FLAG
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
        channel_started: bool,
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
        let prepared = g17_submission::prepare_g17p_compute_channel_work(
            state,
            queue_id,
            channel_started,
        )
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
        let slot_offset: usize = slot_gpu_va
            .checked_sub(self.resources.shared_cluster.iova())
            .ok_or(EINVAL)?
            .try_into()?;
        if slot_offset.checked_add(0x18).ok_or(EINVAL)?
            > self.resources.shared_cluster.logical_size
        {
            return Err(ERANGE);
        }
        let ring_vmap = self.resources.shared_cluster.object.vmap()?;
        let ring = unsafe {
            // SAFETY: the checked slot lies inside the retained shared VMap.
            core::slice::from_raw_parts(
                ring_vmap.as_mut_ptr(),
                self.resources.shared_cluster.logical_size,
            )
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
        };
        core::sync::atomic::fence(core::sync::atomic::Ordering::Acquire);
        Ok(snapshot)
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
            g17_initdata::encode_partial_opening_operand_table_post_control(bytes)
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
        let prepared = g17_submission::prepare_g17p_partial_opening_work_pair(fragment, tiling)
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
            Some(self.address_space.uat.bind(vm)?)
        } else if context_already_bound {
            None
        } else {
            self.address_space
                .uat
                .install_t8140_partial_opening_empty_high_roots()?;
            None
        };
        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
        state[fragment_producer_offset..fragment_producer_offset + 4]
            .copy_from_slice(&prepared.fragment.next_producer.to_le_bytes());

        publish_late_tiling_state();
        shared[tiling_slot_offset..tiling_slot_offset + prepared.tiling.slot.len()]
            .copy_from_slice(&prepared.tiling.slot);
        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
        state[tiling_producer_offset..tiling_producer_offset + 4]
            .copy_from_slice(&prepared.tiling.next_producer.to_le_bytes());
        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);

        Ok((prepared, render_bind))
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

    const KERN: u64 = 0xffff_fc20_0000_0000;

    fn get_u32(raw: &[u8], offset: usize) -> u32 {
        u32::from_le_bytes(raw[offset..offset + 4].try_into().unwrap())
    }

    fn get_u64(raw: &[u8], offset: usize) -> u64 {
        u64::from_le_bytes(raw[offset..offset + 8].try_into().unwrap())
    }

    #[test]
    fn retained_descriptors_advance_through_native_stride_arrays() {
        assert_eq!(G17P_RENDER_DESCRIPTOR_SLOT_COUNT, 32);
        assert_eq!(G17P_RENDER_TA_DESCRIPTOR_ARRAY_SIZE, 0x14000);
        assert_eq!(G17P_RENDER_3D_DESCRIPTOR_ARRAY_SIZE, 0x48000);
        assert_eq!(G17P_RENDER_DESCRIPTOR_STORAGE_SIZE, 0x5c000);
        assert_eq!(g17p_render_ta_descriptor_offset(0), 0);
        assert_eq!(g17p_render_ta_descriptor_offset(1), 0x9c0);
        assert_eq!(g17p_render_3d_descriptor_offset(0), 0x14000);
        assert_eq!(g17p_render_3d_descriptor_offset(1), 0x16240);
        assert_eq!(
            g17p_render_ta_descriptor_high_va(1),
            Some(g17_submission::G17P_COLD_OPENING_TA_DESCRIPTOR_BASE + 0x9c0),
        );
        assert_eq!(
            g17p_render_3d_descriptor_low_va(1),
            Some(G17P_BOOTSTRAP_3D_DESCRIPTOR_LOW_VA + 0x2240),
        );
        assert_eq!(G17P_RENDER_QUEUE_GRAPH_SIZE, 0x14000);
        assert_eq!(g17p_render_optional_offset(true, 1), 0xc180);
        assert_eq!(g17p_render_event_offset(true, 1), 0xf080);
        assert_eq!(g17p_render_optional_offset(false, 1), 0x10180);
        assert_eq!(g17p_render_event_offset(false, 1), 0x13080);
        assert_eq!(g17p_render_descriptor_slot(32), 0);
    }

    #[test]
    fn g17p_parameter_management_matches_the_native_object_graph() {
        let pm = g17p_parameter_management_layout().unwrap();
        assert_eq!(G17P_PM_HWPB_STATE_SIZE, 0xc0);
        assert_eq!(pm.hwpb_state, 0);
        assert_eq!(pm.page_list, 0x8000);
        assert_eq!(pm.block_page_bases, 0x40000);
        assert_eq!(pm.shared_control, 0x20000);
        assert_eq!(pm.counter, 0x28000);
        assert_eq!(pm.page_metrics, 0x30000);
        assert_eq!(pm.scene_states, 0x38000);
        assert_eq!(pm.total_size, 0x48000);
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
            layermeta: 0,
            layermeta_size: 0,
            heapmeta: 0,
            heapmeta_size: 0,
            tpc: 0,
            tpc_size: 0,
            aux_fb: 0,
            tilemap: 0,
            tilemap_size: 0,
            tvb: 0x88_000,
            tvb_size: 31 * G17P_RENDER_TVB_BLOCK_STRIDE + G17P_RENDER_TVB_BLOCK_SIZE,
            tvb_blocks: 32,
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
        let firmware_metrics_vas = [metrics_vas[1], metrics_vas[0]];
        let mut raw = vec![0xa5; pm.total_size];
        assert_eq!(
            encode_g17p_parameter_management(
                &mut raw,
                high,
                G17PRenderQueuePair::for_slot(0).unwrap(),
                page_list_vas,
                firmware_metrics_vas,
                tvb_va,
                scene_scratch_va,
                discard_va,
                5,
                7,
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
        assert_eq!(get_u32(&raw, state + 0x34), 0x80);
        assert_eq!(get_u32(&raw, state + 0x38), 0xd1a);
        assert_eq!(get_u32(&raw, state + 0x3c), 32);
        assert_eq!(get_u32(&raw, state + 0x40), 0);
        assert_eq!(
            get_u64(&raw, state + 0x44),
            high + pm.block_page_bases as u64
        );
        assert_eq!(
            get_u64(&raw, state + 0x4c),
            high + pm.shared_control as u64
        );
        assert_eq!(get_u32(&raw, state + 0x54), 0x7f);
        assert_eq!(get_u32(&raw, state + 0x58), 0x20000);
        assert_eq!(get_u64(&raw, state + 0x64), high + pm.counter as u64);
        assert_eq!(get_u32(&raw, state + 0x7c), 0x3468);
        assert_eq!(get_u32(&raw, state + 0x80), 0x1178);
        assert_eq!(get_u64(&raw, state + 0x84), 0x180000);

        for (block, first) in G17P_NATIVE_TVB_BLOCK_PAGE_IDS.into_iter().enumerate() {
            assert_eq!(get_u32(&raw, pm.block_page_bases + block * 8), first);
            assert_eq!(get_u32(&raw, pm.block_page_bases + block * 8 + 4), 0);
            for page in 0..4 {
                assert_eq!(
                    get_u32(&raw, pm.page_list + (block * 4 + page) * 4),
                    first + page as u32
                );
            }
        }
        for slot in G17P_PM_BLOCK_COUNT * G17P_PM_PAGES_PER_BLOCK..160 {
            assert_eq!(get_u32(&raw, pm.page_list + slot * 4), 0);
        }
        assert_eq!(render.tvb_size, 0x4f_8000);
        assert_eq!(G17P_PM_BLOCK_COUNT * G17P_RENDER_TVB_BLOCK_SIZE, 0x40_0000);
        assert_eq!(get_u32(&raw, pm.shared_control), 32);
        assert_eq!(get_u32(&raw, pm.shared_control + 4), 32);
        assert_eq!(get_u32(&raw, pm.shared_control + 8), 0);
        assert_eq!(get_u32(&raw, pm.shared_control + 0x60), 1);

        let selected = pm.scene_states + G17P_PM_SCENE_ENTRY_SIZE;
        assert_eq!(get_u64(&raw, selected), metrics_vas[1] + 4);
        assert_eq!(get_u64(&raw, selected + 8), metrics_vas[0] + 4);
        assert_eq!(get_u64(&raw, selected + 0x28), 0x178020);
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
        assert_eq!(G17P_PM_INITIAL_HWPB_MANAGER_GENERATION, 1);
        assert_eq!(get_u64(&raw, selected + 0x4c), 7);
        assert_eq!(get_u64(&raw, pm.scene_states + 0x4c), 0);
        assert_eq!(
            get_u64(&raw, pm.scene_states + 2 * G17P_PM_SCENE_ENTRY_SIZE + 0x4c),
            0
        );
        let trailer = pm.scene_states + G17P_PM_SCENE_COUNT * G17P_PM_SCENE_ENTRY_SIZE;
        assert!(raw[trailer..trailer + G17P_PM_SCENE_TRAILER_SIZE]
            .iter()
            .all(|byte| *byte == 0));

        // Scene ownership starts over when a replacement PM manager is built,
        // even though the retained queue's producer ordinal does not. A fresh
        // manager at either observed handoff ordinal therefore begins at
        // Scene1. It reaches Scene0 only after 79 completed submissions of its
        // own; the independent 36-slot scratch ring still wraps modulo 36 in
        // g17p_render_scene_registers().
        assert_eq!(g17p_render_scene_index(195, 195), Some(1));
        assert_eq!(g17p_render_scene_index(195, 196), Some(2));
        assert_eq!(g17p_render_scene_index(195, 274), Some(0));
        assert_eq!(g17p_render_scene_index(195, 275), Some(1));
        assert_eq!(g17p_render_scene_index(2022, 2022), Some(1));
        assert_eq!(g17p_render_scene_index(2022, 2079), Some(58));
        assert_eq!(g17p_render_scene_index(2022, 2101), Some(0));
        assert_eq!(g17p_render_scene_index(2022, 2021), None);
        assert_eq!(g17p_pm_manager_generation_plan(1), Ok((1, 2)));
        assert_eq!(g17p_pm_manager_generation_plan(7), Ok((7, 8)));
        assert_eq!(g17p_pm_manager_generation_plan(0), Err(EINVAL));
        assert_eq!(g17p_pm_manager_generation_plan(u64::MAX), Err(EOVERFLOW));
        // Firmware may overwrite PBState scratch between submissions. Native
        // loadPBDesc restores the HardwareBufferID on every command, not only
        // when constructing the retained manager.
        put_u32(&mut raw, pm.hwpb_state + 0x0c, 0x38_0000);
        assert_eq!(
            prepare_g17p_retained_scene(&mut raw, &pm, 0, 8, 5),
            Ok(())
        );
        assert_eq!(get_u32(&raw, pm.hwpb_state + 0x0c), 5);
        assert_eq!(get_u64(&raw, pm.scene_states + 0x4c), 8);
        assert_eq!(get_u64(&raw, pm.scene_states + 8), metrics_vas[0]);
    }

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
        assert_eq!(dynamic.start, 0x70_0400_0000);
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
        assert_eq!(dynamic.start - fixed_end, 0x210_8000);

        for (start, size) in [
            (G17P_CONTROL_DIRECTORY_VA, G17P_CONTROL_DIRECTORY_SIZE),
            (
                G17P_BOOTSTRAP_TA_DESCRIPTOR_LOW_VA,
                G17P_RENDER_TA_DESCRIPTOR_ARRAY_SIZE,
            ),
            (
                G17P_BOOTSTRAP_3D_DESCRIPTOR_LOW_VA,
                G17P_RENDER_3D_DESCRIPTOR_ARRAY_SIZE,
            ),
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
            g17_initdata::COMPUTE_FLIST_PAGE_LIST_ADDRESS,
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
            None,
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
            None,
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
            record[0x40..0x44].copy_from_slice(&2u32.to_le_bytes());
            record[0x44..0x48].copy_from_slice(&u32::MAX.to_le_bytes());
            record[0x48..0x4c].copy_from_slice(&0x15u32.to_le_bytes());
            record[0x94..0x9c].copy_from_slice(&0u64.to_le_bytes());
            record[0x9c..0xa4].copy_from_slice(&context.to_le_bytes());
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
        assert_eq!(u32_at(&tiling, 0x48), 0);
        assert_eq!(u32_at(&tiling, 0x304), 0);
        for offset in [0x370, 0x37c, 0x388] {
            assert_eq!(u32_at(&tiling, offset), 0x100);
        }

        assert_eq!(u64_at(&fragment, 0x04), 0);
        assert_eq!(u32_at(&fragment, 0x0c), 1);
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
            G17P_BOOTSTRAP_DESCRIPTOR_ZERO_A_VA + 4
        );
        assert_eq!(
            get_u64(&fragment, 0x2148),
            G17P_BOOTSTRAP_DESCRIPTOR_ZERO_B_VA + 4
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
        assert_eq!(layout.layermeta, 0x11000);
        assert_eq!(layout.layermeta_size, 0);
        assert_eq!(layout.heapmeta, 0x11000);
        assert_eq!(layout.heapmeta_size, 0x200);
        assert_eq!(layout.tpc, 0x14000);
        assert_eq!(layout.tpc_size, 0x1000);
        assert_eq!(layout.aux_fb, 0x18000);
        assert_eq!(layout.tilemap, 0x20000);
        assert_eq!(layout.tilemap_size, 0x500);
        assert_eq!(layout.total_size, 0x24000);
        assert_eq!(G17P_RENDER_STATE_AUX_FB_SIZE, 0x8000);
        assert_eq!(G17P_RENDER_AUX_FB_PAGE_COUNT, 0x10_0000);
        assert_eq!(g17p_aux_fb_flags(0), 0xc000);
        assert_eq!(
            g17p_aux_fb_flags(G17P_RENDER_DBIAS_IS_INT),
            0x4c000
        );

        let mut layered_parameters = parameters;
        layered_parameters.layers = 2;
        let layered = g17p_render_state_layout(&layered_parameters, 2).unwrap();
        assert_eq!(layered.tilemap_size, 0xa00);
        assert_eq!(layered.layermeta, 0x11000);
        assert_eq!(layered.layermeta_size, 0x100);
        assert_eq!(layered.heapmeta, 0x11100);
        assert_eq!(layered.heapmeta_size, 0x200);

        let mut large_parameters = parameters;
        large_parameters.width = 4096;
        large_parameters.height = 4096;
        let large = g17p_render_state_layout(&large_parameters, 2).unwrap();
        assert!(large.tpc_size > layout.tpc_size);
        assert!(large.tilemap_size > layout.tilemap_size);
        assert_eq!(large.aux_fb, layout.aux_fb);
        assert_eq!(large.total_size, layout.total_size);

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
            shared_control: 0x70_0102_4000,
            channel_control: 0x70_0102_8000,
            tiling_shared_object: 0x70_0102_c000,
        };
        let mut graph = vec![0u8; G17P_RENDER_QUEUE_GRAPH_SIZE];
        let mut support = vec![0u8; G17P_RENDER_SUPPORT_SIZE];
        put_u32(&mut support, G17P_RENDER_POOL_A_SLOTS + 4, 1);
        put_u32(&mut support, G17P_RENDER_SHARED_CONTROL_INNER, 1);

        // An address-construction error occurs before any publication and
        // cannot change the early 3D-visible bytes.
        assert!(build_g17p_late_tiling_bytes(descriptor, u64::MAX, addresses, 0).is_err());
        assert!(graph[G17P_USER_TILING_RING..G17P_USER_TILING_RING + 24]
            .iter()
            .all(|byte| *byte == 0));
        assert_eq!(get_u32(&support, G17P_RENDER_POOL_A_SLOTS + 4), 1);
        assert_eq!(get_u32(&support, G17P_RENDER_SHARED_CONTROL_INNER), 1);

        let late = build_g17p_late_tiling_bytes(descriptor, graph_va, addresses, 0).unwrap();
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
        assert_eq!(get_u32(&graph, G17P_USER_TILING_EVENT + 0x08), 0x102);
        assert_eq!(get_u32(&support, G17P_RENDER_POOL_A_SLOTS + 4), 2);
        assert_eq!(get_u32(&support, G17P_RENDER_SHARED_CONTROL_INNER), 2);

        let retained = build_g17p_late_tiling_bytes(descriptor, graph_va, addresses, 1).unwrap();
        retained.apply(&mut graph, &mut support);
        assert_eq!(get_u32(&graph, G17P_USER_TILING_EVENT + 0x08), 0x202);
        assert_eq!(get_u32(&support, G17P_RENDER_POOL_A_SLOTS + 4 + 2 * 4), 2);
        assert_eq!(get_u32(&support, G17P_RENDER_SHARED_CONTROL_INNER), 4);
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
    fn user_render_support_names_fixed_context_peers_and_channel_record_zero() {
        assert_eq!(G17P_RENDER_SUPPORT_SIZE, 0x40000);
        let support_va = 0x70_0100_0000;
        let channel_control = support_va + G17P_RENDER_CHANNEL_CONTROL as u64;
        let mut support = vec![0u8; G17P_RENDER_SUPPORT_SIZE];
        let tiling_optional = g17p_cold_opening_optional_addresses(support_va, true);
        let fragment_optional = g17p_cold_opening_optional_addresses(support_va, false);
        assert_eq!(
            tiling_optional.shared_control,
            g17_initdata::PARTIAL_OPENING_CONTROL_SHARED_ADDRESS
        );
        assert_eq!(
            fragment_optional.shared_control,
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
        assert_eq!(G17P_BOOTSTRAP_CONTEXT_PEER_SIZE, 8 * G17P_PAGE_SIZE);
        g17_submission::apply_g17p_cold_opening_channel_control(
            &mut support[G17P_RENDER_CHANNEL_CONTROL
                ..G17P_RENDER_CHANNEL_CONTROL
                    + g17_submission::G17P_COLD_OPENING_CHANNEL_CONTROL_SIZE],
        )
        .unwrap();
        let pointed = (channel_control - support_va) as usize;
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

        // These are B2 aliases only in context 0. They fall inside render
        // operand slot 21, so the layouts cannot share one low root.
        let slot_21 = g17p_control_operand_buffer_va(21).unwrap();
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
            primary: [0, 0, 4],
            secondary: [0, 0, 16],
        };
        assert!(staged.opening_staged());
        assert!(!staged.opening_retired());

        let retired = G17PControlCounters {
            primary: [4, 4, 4],
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
            primary: [69, 69, 69],
            secondary: [16, 16, 16],
        };
        assert!(runtime.compute_runtime_ready());
        assert!(!runtime.opening_staged());
        assert!(!runtime.opening_retired());
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
    fn render_firmware_items_and_client_resources_use_distinct_vms() {
        let firmware = KERN..KERN + 0x20_0000;
        let client = 0x70_0000_0000..0x71_0000_0000;
        let layout = G17PRenderStorageVaLayout {
            descriptors: KERN + 0x10000,
            graph: KERN + 0x20000,
            support: KERN + 0x30000,
            render_state: 0x70_0020_0000,
            render_state_size: 0x20000,
            timestamps: 0x70_0030_0000,
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
