// SPDX-License-Identifier: GPL-2.0-only
//! M5 qualification through the shared submission path.

use kernel::prelude::*;
use super::fw::m5::{render as cmd, render_registers as regs};
use crate::g17::{buffer::target, command::Validated, fw::render::{RenderArgs, RenderPass}};

pub(crate) fn enabled() -> bool {
    *crate::module_parameters::m5_render_submit_test.value() == 1
}

// Bound diagnostic output without adding per-pair storage to the hot owner.
static DUMP_CONTEXTS: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);
pub(crate) fn trace_context(id: u16) -> bool {
    if !enabled() || id >= 64 { return false; }
    let bit = 1u64 << id;
    DUMP_CONTEXTS.fetch_or(bit, core::sync::atomic::Ordering::Relaxed) & bit == 0
}

pub(crate) fn validate(pass: &RenderPass) -> Result {
    // HAL300 carries the sample stride in eight bits. The 64-byte limit
    // belonged to the first qualified profile, not the M5 imageblock.
    let single_sample = pass.samples_log2 == 0
        && matches!((pass.utile_width, pass.utile_height),
                    (32, 32) | (32, 16) | (16, 16))
        && pass.sample_size % 8 == 0
        && u32::from(pass.sample_size) * u32::from(pass.utile_width)
            * u32::from(pass.utile_height) <= 65536
        && (pass.sample_size != 0 || pass.layers == 1);
    // Native ZLS addresses/actions and page strides are sample-independent.
    // Retain the checked plain layout; compression is not admitted here,
    // except in the layered form native emits (native-window item 1,
    // render-layers-2-depth-msaa4-store / -stencil-store 3D commands: ZLS
    // control 0x80044 / 0xc0154, every enabled Z/S load or store compressed,
    // a compression address per compressed plane, depth stride word 0x1 and
    // stencil stride 0x7c000 as supplied by userspace).
    let plain_zls = pass.zls_control == 0
        || (pass.zls_control & 0x154 == 0
            && pass.depth.compression_va == 0 && pass.stencil.compression_va == 0);
    let multisample = (plain_zls || layered_compressed_zls(pass))
        && matches!(pass.samples_log2, 1..=3)
        && matches!((pass.utile_width, pass.utile_height),
                    (32, 32) | (32, 16) | (16, 16))
        && pass.sample_size >= 8 && pass.sample_size % 8 == 0
        && (u32::from(pass.utile_width) * u32::from(pass.utile_height)
            << pass.samples_log2) <= 2048
        && (u32::from(pass.sample_size) * u32::from(pass.utile_width)
            * u32::from(pass.utile_height) << pass.samples_log2) <= 65536;
    if !enabled() || !(single_sample || multisample)
        || (pass.zls_control != 0 && pass.depth.va == 0 && pass.stencil.va == 0) {
        return Err(ENOTSUPP);
    }
    Ok(())
}

/// Layered multisample depth/stencil in native's form: each enabled load or
/// store of a plane is compressed (ZLS bits: Z load 15 / compress 2, S load 14
/// / compress 4, Z store 19 / compress 6, S store 18 / compress 8; the depth
/// format bits 25:26 are allowed), a plane
/// with any compression has a compression address, and no other ZLS bits.
fn layered_compressed_zls(pass: &RenderPass) -> bool {
    const Z_LOAD: u64 = 1 << 15;
    const S_LOAD: u64 = 1 << 14;
    const Z_STORE: u64 = 1 << 19;
    const S_STORE: u64 = 1 << 18;
    const Z_LOAD_COMPRESS: u64 = 1 << 2;
    const S_LOAD_COMPRESS: u64 = 1 << 4;
    const Z_STORE_COMPRESS: u64 = 1 << 6;
    const S_STORE_COMPRESS: u64 = 1 << 8;
    /// Depth format (bits 25:26) is a property of the plane, not of layering.
    const Z_FORMAT: u64 = 3 << 25;
    const KNOWN: u64 = Z_LOAD | S_LOAD | Z_STORE | S_STORE | Z_LOAD_COMPRESS | S_LOAD_COMPRESS
        | Z_STORE_COMPRESS | S_STORE_COMPRESS | Z_FORMAT;
    let control = pass.zls_control;
    let z_compressed = control & (Z_LOAD_COMPRESS | Z_STORE_COMPRESS) != 0;
    let s_compressed = control & (S_LOAD_COMPRESS | S_STORE_COMPRESS) != 0;
    pass.layers > 1
        && control != 0
        && control & !KNOWN == 0
        && (control & Z_LOAD == 0 || control & Z_LOAD_COMPRESS != 0)
        && (control & S_LOAD == 0 || control & S_LOAD_COMPRESS != 0)
        && (control & Z_STORE == 0 || control & Z_STORE_COMPRESS != 0)
        && (control & S_STORE == 0 || control & S_STORE_COMPRESS != 0)
        && (!z_compressed || (pass.depth.va != 0 && pass.depth.compression_va != 0))
        && (!s_compressed || (pass.stencil.va != 0 && pass.stencil.compression_va != 0))
        && (z_compressed || pass.depth.compression_va == 0)
        && (s_compressed || pass.stencil.compression_va == 0)
}

pub(crate) fn parameter_geometry(blocks: usize) -> Result<super::buffer::BlockGeometry> {
    let mut info = core::mem::MaybeUninit::<kernel::bindings::sysinfo>::zeroed();
    // SAFETY: The complete sysinfo is initialized and writable.
    unsafe { kernel::bindings::si_meminfo(info.as_mut_ptr()); }
    // SAFETY: The object was initialized before si_meminfo filled it.
    let info = unsafe { info.assume_init() };
    let ram = (info.totalram as u64).checked_mul(u64::from(info.mem_unit)).ok_or(EOVERFLOW)?;
    let initial = blocks.checked_mul(0x80000).ok_or(EOVERFLOW)?;
    super::buffer::BlockGeometry::m5(ram, initial as u64)
}

pub(crate) fn layout(command: &Validated, clusters: u32) -> Result<target::Layout> {
    let Validated::Render { pass, plan, .. } = command else { return Err(EINVAL); };
    let layout = target::Layout::new(pass, clusters)?;
    if plan.is_none() { return Ok(layout); }
    validate(pass)?;
    // Match the native target policy when no single-cluster override is requested.
    // Both target storage and the segment controls use the physical cluster count.
    let geometry = parameter_geometry(layout.tvb_blocks)?;
    // Native auxiliary tables cover the complete parameter inventory, including
    // future growth, rather than just the initially resident physical blocks.
    let bytes = geometry.capacity.checked_mul(geometry.block_bytes).ok_or(EOVERFLOW)?;
    let groups = if pass.few_primitives { 1 } else { clusters };
    layout.with_m5_slices(pass, groups, 64, bytes)
}

pub(crate) fn controls(pass: &RenderPass) -> Result<regs::M5RenderControls> {
    validate(pass)?;
    let stage = |resource_flags| regs::M5RenderStageControls {
        scs: super::render_resources::StageScratch::NONE,
        resource_flags, resource_control: 0x200000001, ptc_max: 96, work_key: 0,
    };
    Ok(regs::M5RenderControls {
        fragment_event_control: regs::fragment_event_control(true, 1),
        ta: stage(0x4054b), fragment: stage(0x104054b),
        // Native code chooses this word from descriptor+12a4.
        // Mode 2 is a private command-policy discriminator, not a default.
        ta_tile_control: if *crate::module_parameters::m5_ta_policy_test.value() == 2 {
            0x14c08a60
        } else { 0x10c08860 }, ta_dispatch_control: 0x700,
        fragment_raster_control: 0x200000ff0200, fragment_tile_flags: 0x200003717f,
        store_pipeline_2_bind: pass.eot_rsrc_spec << 32,
        store_pipeline_2: pass.usc_base + u64::from(pass.eot_offset & !7),
        partial_store_pipeline_2_bind: pass.partial_eot_rsrc_spec << 32,
        partial_store_pipeline_2: pass.usc_base + u64::from(pass.partial_eot_offset & !7),
        partial_tile_flags: 0x200000,
        aux_fb_flags: 0x10c000 | u64::from(pass.few_primitives)
            | (u64::from(pass.depth_bias_is_int) << 18),
        aux_fb_control: 0x100000,
    })
}

pub(crate) fn inputs(args: &RenderArgs<'_>, usage: [u64; 2]) -> Result<cmd::M5RenderCommandInputs> {
    let r = args.resources;
    let mut fields = cmd::M5RenderCommandInputs::default();
    let token = super::fw::m5::render_tail::context_token(u32::from(r.context_id),
        u64::from(r.context_generation)).ok_or(EINVAL)?;
    fields.ta.d1298 = token;
    fields.fragment.d158 = token;
    fields.ta.resolved_tail_value = usage[0];
    fields.fragment.resolved_tail_value = Some(usage[1]);
    fields.scheduler_record = r.kick_record_va;
    // Native725 image-copy TA+0x89a copies descriptor+0x1177 bit0.
    // Its full policy is not decoded; keep this explicit diagnostic off by default.
    fields.ta.d1177 = u8::from(*crate::module_parameters::m5_ta_policy_test.value() == 1);
    fields.ta.sequence = args.ordinal.checked_mul(2).and_then(|n| n.checked_add(1)).ok_or(EOVERFLOW)?;
    fields.fragment.sequence = args.ordinal.checked_mul(2).ok_or(EOVERFLOW)?;
    fields.fragment.pass = 1;
    fields.ta.d08a4 = r.ta_ids.predecessor;
    fields.fragment.d08a4 = r.fragment_ids.predecessor;
    fields.fragment.dc68 = 1;
    fields.fragment.dc6c = 1;
    fields.fragment.d0518[22][1] = 0x04040404;
    // Pass policy is filled by apply_pass alongside its register copies.
    fields.fragment.d08c5_flags = [0; 4];
    fields.ta.d08b0 = r.timestamp_va;
    fields.ta.diagnostic_ta_timestamp_upper = r.timestamp_va + 8;
    fields.fragment.d08b0 = [r.timestamp_va + 16, r.timestamp_va + 24];
    let stamp = (args.ordinal as u32).wrapping_add(1).wrapping_shl(8);
    fields.ta.linked_3d = cmd::M5TaLinked3d {
        queue_index: u32::from(args.fragment.qid), stamp, ..Default::default()
    };
    let completion = |stage: &crate::g17::fw::render::RenderStage, key| cmd::M5StageCompletion {
        stamp_addresses: [stage.stamp_va, stage.aux_stamp_va], stamp,
        queue_index: u32::from(stage.qid), ordinal: args.ordinal as u32,
        key, ..Default::default()
    };
    fields.ta.completion = completion(&args.ta, u64::from(r.ta_ids.current));
    fields.fragment.completion = completion(&args.fragment, u64::from(r.fragment_ids.current));
    fields.ta.pb_slot = r.pb_slot;
    fields.ta.pb_state = r.pb_va;
    fields.fragment.pm_state = r.pb_va;
    fields.ta.pm_scene = r.scene_va;
    fields.fragment.pm_scene = r.scene_va;
    fields.ta.pm_b8 = r.scene_end_va;
    fields.fragment.pm_b8 = r.scene_end_va;
    fields.ta.uma_state = r.free_list_control_va;
    fields.fragment.uma_state = r.free_list_control_va;
    fields.ta.d1444 = 1;
    fields.fragment.dbc4 = 1;
    Ok(fields)
}
