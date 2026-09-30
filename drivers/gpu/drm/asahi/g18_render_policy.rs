// SPDX-License-Identifier: GPL-2.0-only OR MIT
//! Bounded H1/application policy shared by the kernel and host.
//! Target kinds 1/2 use native geometry; mode 0 retains the stage33d policy.
use crate::{g17_render::G17pRenderParameters, g18_address_map as va,
    g18_render::G18RenderControls, g18_render_command::*, g18_render_assembly};

pub(crate) const PB_BLOCKS: u32 = 8; // plan: overprovision >=8; no PB growth.
pub(crate) const PB_CAPACITY: u32 = 1650;
pub(crate) const UMA_BLOCKS: u32 = 24; // 24 MiB > sum of both M5 budgets.
pub(crate) const USC_REQUIRED: [u64; 2] = [0x7d4000, 0x934000]; // M5 DRM hardware path.
// G18 native SMAV1 first-use unit pair, proven by the compute fixture. The
// graphics byte budgets above remain M5 policy; no claim of derived sizing.
pub(crate) const RESOURCE_PAIR: [u64; 2] = [16, 6];
pub(crate) const UMA_CHUNKS: [u32; 2] = [0,
    ((USC_REQUIRED[0] + USC_REQUIRED[1] + 12 * 1024 - 1) / (12 * 1024)) as u32];

// Native SMAV1 minimum-result serialization for the captured witness. Each
// stage needs 3 MiB, below the unchanged H1 backing/accounting. This probes the
// two resource words only; it does not claim native ideal allocation policy.
pub(crate) const NATIVE_MINIMUM_PAIRS: [[u64; 2]; 2] = [[4, 8], [9, 11]];
#[cfg(not(CONFIG_RUST))]
pub(crate) static HOST_SMA_MINIMUM: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

// Fixed first-draw witness policy, from the original M6 SMAV1 methods. This
// experiment is admitted only for stage9/variant0/profile4 before MMIO.
pub(crate) const NATIVE_IDEAL_PAIRS: [[u64; 2]; 2] = [[24, 8], [42, 11]];
pub(crate) const IDEAL_GROWTH_BYTES: u64 = 21 * va::PAGE;
pub(crate) const IDEAL_GROWTH_OFFSET: u64 = 0x158000 - IDEAL_GROWTH_BYTES;
#[cfg(not(CONFIG_RUST))]
pub(crate) static HOST_SMA_IDEAL: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

pub(crate) fn ideal_enabled() -> bool {
    #[cfg(CONFIG_RUST)] { *crate::module_parameters::g18_sma_ideal.value() == 1 }
    #[cfg(not(CONFIG_RUST))] { HOST_SMA_IDEAL.load(core::sync::atomic::Ordering::Relaxed) }
}
pub(crate) fn ta_barrier_flag() -> u32 {
    #[cfg(CONFIG_RUST)] { *crate::module_parameters::g18_native_barrier.value() }
    #[cfg(not(CONFIG_RUST))] { 0 }
}
pub(crate) fn uma_blocks() -> u32 { if ideal_enabled() { 40 } else { UMA_BLOCKS } }
pub(crate) fn uma_chunks() -> [u32; 2] { if ideal_enabled() { [0, 708 + 2305] } else { UMA_CHUNKS } }
pub(crate) fn block_base() -> u64 { if ideal_enabled() { va::GRAPHICS_IDEAL_BLOCKS } else { va::POOL_BLOCKS } }
pub(crate) fn usc_required() -> [u64; 2] { if ideal_enabled() { [9 * 0x100000, 28 * 0x100000] } else { USC_REQUIRED } }

pub(crate) fn resource_pairs() -> [[u64; 2]; 2] {
    #[cfg(CONFIG_RUST)]
    let minimum = *crate::module_parameters::g18_sma_minimum.value() == 1;
    #[cfg(not(CONFIG_RUST))]
    let minimum = HOST_SMA_MINIMUM.load(core::sync::atomic::Ordering::Relaxed);
    if ideal_enabled() { NATIVE_IDEAL_PAIRS } else if minimum { NATIVE_MINIMUM_PAIRS } else { [RESOURCE_PAIR; 2] }
}

// Keep registration in this owned file. This is the same KernelParam ABI as
// kernel::module!, with load-time-only access (perm=0), so a live job cannot
// change layout under its mappings. Default 1 matches the parsed colour/gears
// force-kind-1 flag. Invalid selectors fail target_plan before publication.
#[cfg(CONFIG_RUST)]
mod parameter {
    use kernel::{bindings, module_param::{KernelParam, ModuleParamAccess, PARAM_OPS_U32}};
    pub(super) static VALUE: ModuleParamAccess<u32> = ModuleParamAccess::new(1);
    #[link_section = "__param"]
    #[used(compiler)]
    static PARAM: KernelParam = KernelParam::new(bindings::kernel_param {
        name: kernel::str::as_char_ptr_in_const_context(if cfg!(MODULE) {
            c"g18_render_target_kind"
        } else { c"asahi.g18_render_target_kind" }),
        // SAFETY: module lifetime and parameter serialization are managed by
        // the kernel, identically to the kernel::module! registration.
        #[cfg(MODULE)]
        mod_: unsafe { core::ptr::from_ref(&bindings::__this_module).cast_mut() },
        #[cfg(not(MODULE))]
        mod_: core::ptr::null_mut(),
        ops: core::ptr::from_ref(&PARAM_OPS_U32), perm: 0, level: -1, flags: 0,
        __bindgen_anon_1: bindings::kernel_param__bindgen_ty_1 { arg: VALUE.as_void_ptr() },
    });
    #[link_section = ".modinfo"]
    #[used(compiler)]
    static TYPE: [u8; b"parmtype=g18_render_target_kind:uint\0".len()] = *b"parmtype=g18_render_target_kind:uint\0";
    #[link_section = ".modinfo"]
    #[used(compiler)]
    static DESCRIPTION: [u8; b"parm=g18_render_target_kind:0 legacy, 1 native single target (default), 2 native clustered target\0".len()] =
        *b"parm=g18_render_target_kind:0 legacy, 1 native single target (default), 2 native clustered target\0";
}

#[cfg(not(CONFIG_RUST))]
pub(crate) static HOST_TARGET_KIND: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(1);

pub(crate) fn target_kind() -> u32 {
    #[cfg(CONFIG_RUST)] { *parameter::VALUE.value() }
    #[cfg(not(CONFIG_RUST))] { HOST_TARGET_KIND.load(core::sync::atomic::Ordering::Relaxed) }
}

/// All non-capture fields are either assigned here or disabled explicitly by
/// the default zero optional-feature policy. No value is taken from post-run
/// firmware memory or a captured kernel pointer.
pub(crate) fn h1_inputs(command: &[u8], context: u32,
    p: &mut G17pRenderParameters, c: &mut G18RenderControls,
) -> G18RenderCommandInputs {
    let (i, _) = crate::g18_render_passthrough::capture_fields(command);
    owned_inputs(i, context, p, c)
}

/// Shared first-use policy; application callers supply their own descriptor
/// fields and then replace frame identity and PB addresses via owner accessors.
pub(crate) fn owned_inputs(mut i: G18RenderCommandInputs, context: u32,
    p: &mut G17pRenderParameters, c: &mut G18RenderControls) -> G18RenderCommandInputs {
    // Linux first-pass lifecycle and work identity, shared with command copies.
    p.lifecycle = 1u64 << 32; p.fragment_lifecycle = p.lifecycle;
    c.ta.work_key = u64::from(context) << 8; c.fragment.work_key = c.ta.work_key;
    let pairs = resource_pairs();
    c.ta.resource_word = pairs[0][0] | pairs[0][1] << 16;
    c.fragment.resource_word = pairs[1][0] | pairs[1][1] << 16;
    i.context_id = context; i.scheduler_record = va::GRAPHICS_SCHEDULER_START + 0x4000;
    i.fragment.sequence = 0; i.ta.sequence = 1; i.fragment.pass = 1;
    for stage in 0..2 {
        let qid = va::QUEUE_IDS[stage];
        let completion = G18StageCompletion {
            stamp_addresses: [va::GRAPHICS_SIGNALS + 0x4000 + u64::from(qid)*4,
                va::GRAPHICS_SIGNALS + 0x8000 + u64::from(qid)*4],
            stamp: 0x100, queue_index: u32::from(qid), ordinal: 0,
            has_wait: stage == 1, completion: (1u64 << 40) | 1,
            key: if stage == 0 {2} else {1},
        };
        if stage == 0 { i.ta.completion = completion; } else { i.fragment.completion = completion; }
    }
    i.ta.linked_3d = G18TaLinked3d { queue_index: 10, stamp: 0x100, flag: 0,
        completion: i.fragment.completion.completion };
    i.ta.pb_slot = 0; i.ta.pm_1d4 = 0; // M5 first-pass PM policy.
    i.ta.pb_state = va::GRAPHICS_PB_HIGH; i.fragment.pm_state = i.ta.pb_state;
    i.ta.pm_scene = va::PB_GRAPH + 0x1080; i.fragment.pm_scene = i.ta.pm_scene;
    i.ta.pm_b8 = va::PB_GRAPH + 0x3800; i.fragment.pm_b8 = i.ta.pm_b8;
    i.ta.uma_state = va::GRAPHICS_UMA_STATE; i.fragment.uma_state = i.ta.uma_state;
    // Native render-target-loader fields with the M5 geometry of a 256x256,
    // 32x32, single-layer target, sized with the register worker's 2x policy.
    let plan = g18_render_assembly::target_plan(p).expect("admitted H1 geometry");
    g18_render_assembly::apply_target_controls(p, c, &plan);
    i.ta.d11b8 = p.tpc;
    i.ta.d11d8 = plan.native.map_or(plan.layout.tpc_bytes, |n| n.slice_sizes[0]) as u64;
    i.ta.d10f8 = p.deflake_1;
    i.fragment.d08d8 = p.tilemap;
    i.fragment.d08f0 = p.multisample_control;
    i.fragment.d08f8 = u32::from(p.samples);
    i.fragment.d0900 = 1;
    i.fragment.d0908 = 0x40004;
    i.fragment.d0910 = [0, 0];
    i.fragment.d0920 = 0;
    i.fragment.d0930 = 64;
    if plan.native.is_some() {
        let tx = (p.width + 31) / 32;
        let ty = (p.height + 31) / 32;
        let mx = ((tx + 3) / 4 + 3) & !3;
        let my = ((ty + 3) / 4 + 3) & !3;
        i.fragment.d08f8 = 0; // target +0x20: native single-sample value
        i.fragment.d0900 = u32::from(p.samples);
        i.fragment.d0908 = (mx << 16) | my;
        i.fragment.d0910 = [0, u64::from(p.merge_upper_x_bits) | (u64::from(p.merge_upper_y_bits) << 32)];
        i.fragment.d0930 = tx * ty;
    }
    i.ta.d08a4 = 1; i.ta.d08a8 = 0; i.ta.d08ac = 0;
    i.fragment.d08a4 = 1; i.fragment.d08a8 = 0; i.fragment.d08ac = 0;
    // M5 TA activation bit; no optional policy/event/history payloads.
    i.ta.d11a4 = 1;
    i.ta.d08b0 = va::GRAPHICS_SIGNALS + 0xc000;
    i.fragment.d08b0 = [va::GRAPHICS_SIGNALS + 0xc010, va::GRAPHICS_SIGNALS + 0xc018];
    // Native UMA data-record tail: required/allocated byte accounting.
    let required = usc_required();
    i.ta.d1418 = [0, 0]; i.ta.d1428 = required[0]; i.ta.d1430 = 0;
    i.fragment.dad0 = [0, 0]; i.fragment.dae0 = required[1]; i.fragment.dae8 = 0;
    i
}
