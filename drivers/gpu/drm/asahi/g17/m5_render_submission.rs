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
    // Retain the checked plain layout; compression is not admitted here.
    let multisample = (pass.layers == 1 || pass.zls_control == 0)
        && matches!(pass.samples_log2, 1..=3)
        && matches!((pass.utile_width, pass.utile_height),
                    (32, 32) | (32, 16) | (16, 16))
        && pass.sample_size >= 8 && pass.sample_size % 8 == 0
        && (u32::from(pass.utile_width) * u32::from(pass.utile_height)
            << pass.samples_log2) <= 2048
        && (u32::from(pass.sample_size) * u32::from(pass.utile_width)
            * u32::from(pass.utile_height) << pass.samples_log2) <= 65536
        && (pass.zls_control == 0
            || (pass.zls_control & 0x154 == 0
                && pass.depth.compression_va == 0 && pass.stencil.compression_va == 0));
    if !enabled() || !(single_sample || multisample)
        || (pass.zls_control != 0 && pass.depth.va == 0 && pass.stencil.va == 0) {
        return Err(ENOTSUPP);
    }
    Ok(())
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
        resource_flags, resource_control: 0x200000001, ptc_max: 96, work_key: 0,
    };
    Ok(regs::M5RenderControls {
        fragment_event_control: regs::fragment_event_control(true, 1),
        ta: stage(0x4054b), fragment: stage(0x104054b),
        ta_tile_control: 0x10c08860, ta_dispatch_control: 0x700,
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
