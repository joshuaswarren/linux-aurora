// SPDX-License-Identifier: GPL-2.0-only OR MIT

//! G18 application path: one decoded Mesa render command -> G18 job inputs.
//!
//! This is the DRM counterpart of `g18_render::g18_capture_parameters`.
//! Where the capture helper inverts Apple's native gears/colour descriptors,
//! this module starts from the UAPI `drm_asahi_cmd_render` that Mesa's G18G
//! (M5-path) Gallium driver submits and produces:
//!
//! - `G17pRenderParameters` with every user field translated by the shared
//!   `g17_uapi::translate_render_command` (the M5-proven parser and range
//!   checks) and the G18 adaptations below;
//! - `G18RenderControls` from `g18_render::g18_default_controls`;
//! - every job-owned field left at zero (see [`job_owned_fields_clear`]) for
//!   the job (`G18RenderJobSink`) to allocate and fill.
//!
//! G18 adaptations, each against a named source:
//! - VDM stream: the UAPI carries the full GART-alias VA; `g18_render` takes
//!   the native compact view (`G18-RENDER-REGS.md` "Kernel-owned inputs"),
//!   exactly Mesa's `agx_m5_vdm_compact` (`agx_m5_va.h:34-41`).
//! - aux_fb_flags 0x30c000: indexed multi-draw, the gears-g1 value
//!   (`g18_render.rs:490-494`), where M5 used 0x10c000 (`m5_drm.rs:251`).
//! - Main-pass tile flags 0xc00000 (native +8d2/+8d3): present in all three
//!   G18 captures (`g18_render.rs:489`, tile_config 0xc10280), dropped by the
//!   partial helper (`g18_render.rs:396`). The UAPI cannot express them.
//! - Single-layer ZLS array strides are zeroed, as on M5 (`m5_drm.rs:238-241`).
//! - An inert stencil plane (no load/store, bits 14/18 clear) is stripped:
//!   `g18_render` has no stencil witness and refuses any stencil field
//!   (`g18_render.rs:148-152`). `isp_bgobjvals` 0x300 is the constant the
//!   register builder re-asserts (`g17_render.rs:800`), so it is normalised
//!   to zero; a real stencil clear value is refused.
//!
//! Pure: no allocation, MMIO, firmware or VM access. The job boundary at the
//! end of the file is the only kernel-typed item (`hv/tools/
//! check-g18-drm-application.py` compiles it against a stub `kernel` crate).

use crate::{g17_render::G17pRenderParameters, g17_uapi, g18_render, m5_vm};

/// Largest admitted framebuffer side. The job's target plan
/// (`g18_render_assembly::target_plan`, 2x M5 sizing at two clusters) grows
/// with both sides; 2048x2048 (16x16 four-tile macrotiles) exactly fills its
/// 0x20000-byte TPC slot and 2049x2049 overflows it. Thinner targets past
/// 2048 would fit but are not admitted.
pub(crate) const G18_MAX_RENDER_DIMENSION: u32 = 2048;
/// Indexed multi-draw auxiliary framebuffer flags (gears-g1 capture).
pub(crate) const G18_AUX_FB_FLAGS: u64 = 0x30c000;
/// Native main-pass tile flags (+8d2/+8d3), witnessed in colour-a/b and gears-g1.
pub(crate) const G18_MAIN_TILE_FLAGS: u64 = 0xc0_0000;
/// One-sample PPP multisample control; the only value `g18_render` admits.
pub(crate) const G18_MULTISAMPLE_CONTROL: u64 = 0x88;

// drm_asahi_render_flags (include/uapi/drm/asahi_drm.h:859-895).
const RENDER_PROCESS_EMPTY_TILES: u32 = 1 << 1;
const RENDER_RSRC_SPEC_HI: u32 = 1 << 4;
const G18_RENDER_FLAGS: u32 = RENDER_PROCESS_EMPTY_TILES | RENDER_RSRC_SPEC_HI;
// ZLS control: stencil load (14) / store (18), stencil compression (4, 8),
// depth load (15) / store (19), depth format (25-26: 0 = Z32F, 2 = Z16).
const ZLS_STENCIL_TRANSFER: u64 = (1 << 14) | (1 << 18);
const ZLS_STENCIL_COMPRESSION: u64 = (1 << 4) | (1 << 8);
const ZLS_DEPTH_TRANSFER: u64 = (1 << 15) | (1 << 19);
const ZLS_DEPTH_FORMAT: u64 = 3 << 25;
/// Upper ISP_BGOBJVALS bits every Mesa backend seeds (`agx_pipe.c:1333`).
const ISP_BGOBJVALS_BASE: u32 = 0x300;
/// Width of the compact VDM view (`agx_m5_va.h:37`, `g17_render.rs:88`).
const VDM_COMPACT_WINDOW: u64 = 1 << 39;

/// Why a render is outside what the G18 job can do yet. `Invalid` is a
/// malformed or unmapped command (EINVAL); every other variant is a valid
/// command outside the admitted envelope (ENOTSUPP).
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) enum G18ApplicationRefusal {
    Invalid(g17_uapi::UapiTranslateError),
    Flags(u32),
    /// Vertex or fragment scratch helper program (spilling shaders).
    HelperProgram,
    Dimensions { width: u16, height: u16 },
    Layers(u16),
    Samples(u8),
    Utile(u8, u8),
    MultisampleControl(u64),
    Samplers { heap: u64, count: u16 },
    OcclusionQuery,
    StencilTransfer(u64),
    StencilClear(u32),
    Depth(g17_uapi::UapiTranslateError),
    DepthFormat(u64),
    VdmCompact(u64),
}

impl G18ApplicationRefusal {
    /// True when the command itself is malformed rather than out of envelope.
    pub(crate) fn is_invalid(&self) -> bool {
        matches!(self, Self::Invalid(_))
    }
}

/// Render-target geometry the job must plan and allocate for this command.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) struct G18TargetRequest {
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) layers: u16,
    pub(crate) utile: (u8, u8),
    pub(crate) samples: u8,
}

/// Everything the job receives for one admitted render.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) struct G18ApplicationRender {
    /// User fields translated; every job-owned field is zero.
    pub(crate) parameters: G17pRenderParameters,
    /// `g18_default_controls(&parameters)`; job-owned control fields are zero.
    pub(crate) controls: g18_render::G18RenderControls,
    pub(crate) target: G18TargetRequest,
    /// VDM stream as submitted (full VA in the submitting VM). The compact
    /// form is `parameters.encoder`; the job needs both views (risk #1).
    pub(crate) vdm_base: u64,
    /// Attachment ranges Mesa declared (render targets and PBE images).
    pub(crate) vertex_attachments: g17_uapi::UapiAttachmentList,
    pub(crate) fragment_attachments: g17_uapi::UapiAttachmentList,
    /// An inert stencil plane was attached and removed (see module doc).
    pub(crate) stencil_stripped: bool,
    /// UAPI render flags: only PROCESS_EMPTY_TILES and RSRC_SPEC_HI remain.
    pub(crate) flags: u32,
}

/// User pointers the G18 3D command carries as native descriptor copies
/// (render-cmd `G183dCommandFields`), because the G18 fragment program drops
/// their registers 0x15101/0x15109 (`m5_render.rs:306`). The slots are the
/// ones G17P fills from the same parameters (`g17_render.rs:1381-1383`); the
/// gears-g1 native command carries its depth-bias and scissor arrays there.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) struct G18DescriptorPointers {
    /// d04e0 -> 3D +0x1d20: ISP depth-bias array.
    pub(crate) d04e0: u64,
    /// d04e8 -> 3D +0x1d30: ISP scissor array.
    pub(crate) d04e8: u64,
    /// d04f0 -> 3D +0x1d40: occlusion query base (refused, so zero).
    pub(crate) d04f0: u64,
}

impl G18ApplicationRender {
    /// The job must copy these into its 3D command fields; the register
    /// programs built from `parameters` do not contain them.
    pub(crate) fn descriptor_pointers(&self) -> G18DescriptorPointers {
        G18DescriptorPointers {
            d04e0: self.parameters.depth_bias_array,
            d04e8: self.parameters.scissor_array,
            d04f0: self.parameters.occlusion_query_base,
        }
    }
}

/// Whether every job-owned field is still zero. The translation leaves them
/// zero; the job must assign each before building registers:
/// - parameters: context_base, tilemap, layermeta, heapmeta, tpc,
///   deflake_1/2/3, ta_status, fragment_status, aux_fb, cycle, record_index,
///   lifecycle, fragment_lifecycle, ta_hardware_buffer_id,
///   usc_flist_hardware_buffer_id, parameter_buffer and the four timestamps
///   (which `g18_render` requires to stay zero);
/// - controls: resource_word, work_key and usc_statistics of both stages,
///   and the optional TA cluster fields (zero means absent).
///
/// Sources: `G18-RENDER-REGS.md` "Kernel-owned inputs", the H1 job
/// (`g18_graphics_job.rs:61-74`, `g18_render_assembly.rs:34-47`) and its
/// policy (`g18_render_policy.rs:25-28`).
pub(crate) fn job_owned_fields_clear(
    p: &G17pRenderParameters,
    c: &g18_render::G18RenderControls,
) -> bool {
    let parameters = [p.context_base, p.tilemap, p.layermeta, p.heapmeta, p.tpc,
        p.deflake_1, p.deflake_2, p.deflake_3, p.ta_status, p.fragment_status,
        p.aux_fb, p.cycle, p.record_index, p.lifecycle, p.fragment_lifecycle,
        p.parameter_buffer, p.ta_timestamp_start, p.ta_timestamp_end,
        p.fragment_timestamp_start, p.fragment_timestamp_end];
    let controls = [c.ta.resource_word, c.fragment.resource_word, c.ta.work_key,
        c.fragment.work_key, c.ta.usc_statistics, c.fragment.usc_statistics,
        c.ta_cluster_tilemap, c.ta_cluster_layermeta];
    parameters.iter().chain(controls.iter()).all(|&v| v == 0)
        && p.ta_hardware_buffer_id == 0 && p.usc_flist_hardware_buffer_id == 0
        && c.ta_cluster_meta == [0; 4] && c.ta_cluster_control == [0; 3]
}

/// Internal state handed to `translate_render_command`: the M5 reference
/// profile's role (`m5_drm.rs:234`) with every job-owned field zeroed and
/// G18 policy applied. `translate_render_command` overwrites all user fields.
pub(crate) fn g18_application_base() -> G17pRenderParameters {
    let mut p = G17pRenderParameters::source_rsrc8();
    p.context_base = 0;
    p.tilemap = 0;
    p.layermeta = 0;
    p.heapmeta = 0;
    p.tpc = 0;
    p.deflake_1 = 0;
    p.deflake_2 = 0;
    p.deflake_3 = 0;
    p.ta_status = 0;
    p.fragment_status = 0;
    p.aux_fb = 0;
    p.cycle = 0;
    p.record_index = 0;
    p.lifecycle = 0;
    p.fragment_lifecycle = 0;
    p.ta_hardware_buffer_id = 0;
    p.usc_flist_hardware_buffer_id = 0;
    p.parameter_buffer = 0;
    p.gpc_perf_state_map = 0;
    p.gpc_perf_state_map_low = 0;
    p.gpc_perf_state_control = 0;
    p.ta_timestamp_start = 0;
    p.ta_timestamp_end = 0;
    p.fragment_timestamp_start = 0;
    p.fragment_timestamp_end = 0;
    // G18 policy. aux_fb_page_count keeps the value every M5/G18 capture
    // carries (0x100000, `g17_render.rs:421`).
    p.aux_fb_flags = G18_AUX_FB_FLAGS;
    p.samples = 1;
    p.absolute_pointers = false;
    p.bg_resource_prefix = 0;
    p.native_pm_bytes = false;
    p.native_ta_registers = 0;
    p
}

/// Mesa's compact VDM view (`agx_m5_vdm_compact`).
pub(crate) fn g18_vdm_compact(vdm_base: u64) -> Option<u64> {
    let offset = vdm_base.checked_sub(m5_vm::DRIVER_BASE)?;
    if vdm_base & 3 != 0 || offset >= VDM_COMPACT_WINDOW {
        return None;
    }
    Some(offset)
}

/// The envelope checks that need only the raw command. Order matters for
/// diagnostics only; every check is independent.
fn admit_envelope(command: &g17_uapi::UapiRenderCommand) -> Result<(), G18ApplicationRefusal> {
    use G18ApplicationRefusal as R;
    if command.flags & !G18_RENDER_FLAGS != 0 {
        return Err(R::Flags(command.flags));
    }
    let no_helper = g17_uapi::UapiHelperProgram { binary: 0, config: 0, data: 0 };
    if command.vertex_helper != no_helper || command.fragment_helper != no_helper {
        return Err(R::HelperProgram);
    }
    if !(1..=G18_MAX_RENDER_DIMENSION).contains(&u32::from(command.width))
        || !(1..=G18_MAX_RENDER_DIMENSION).contains(&u32::from(command.height))
    {
        return Err(R::Dimensions { width: command.width, height: command.height });
    }
    if command.layers != 1 {
        return Err(R::Layers(command.layers));
    }
    if command.samples != 1 {
        return Err(R::Samples(command.samples));
    }
    if (command.utile_width, command.utile_height) != (32, 32) {
        return Err(R::Utile(command.utile_width, command.utile_height));
    }
    if command.multisample_control != G18_MULTISAMPLE_CONTROL {
        return Err(R::MultisampleControl(command.multisample_control));
    }
    if command.sampler_heap != 0 || command.sampler_count != 0 {
        return Err(R::Samplers { heap: command.sampler_heap, count: command.sampler_count });
    }
    if command.occlusion_query_base != 0 {
        return Err(R::OcclusionQuery);
    }
    if command.zls_control & ZLS_STENCIL_TRANSFER != 0 {
        return Err(R::StencilTransfer(command.zls_control));
    }
    if command.stencil_clear & !ISP_BGOBJVALS_BASE != 0 {
        return Err(R::StencilClear(command.stencil_clear));
    }
    Ok(())
}

/// Translate one render command for the G18 job. `address_space` must answer
/// for the submitting VM; `window` is the queue's validated USC window.
#[inline(never)]
pub(crate) fn translate_g18_render<A: g17_uapi::GpuAddressSpace>(
    command: &g17_uapi::UapiRenderCommand,
    vertex_attachments: g17_uapi::UapiAttachmentList,
    fragment_attachments: g17_uapi::UapiAttachmentList,
    window: g17_uapi::QueueUscWindow,
    address_space: &A,
) -> Result<G18ApplicationRender, G18ApplicationRefusal> {
    use G18ApplicationRefusal as R;
    admit_envelope(command)?;
    // Z32F only: validate_m5_reference_depth also admits Z16, which has no
    // G18 witness (G18 captures are D32F, `g18_render.rs:505-511`).
    g17_uapi::validate_m5_reference_depth(command, address_space).map_err(R::Depth)?;
    if command.depth.base != 0 && command.zls_control & ZLS_DEPTH_FORMAT != 0 {
        return Err(R::DepthFormat(command.zls_control));
    }
    let translated = g17_uapi::translate_render_command(*command, vertex_attachments,
        fragment_attachments, window, address_space,
        g17_uapi::RenderInternalState { parameters: g18_application_base() })
        .map_err(R::Invalid)?;
    let mut p = translated.parameters;

    p.encoder = g18_vdm_compact(command.vdm_base).ok_or(R::VdmCompact(command.vdm_base))?;
    // Native single-layer ZLS does not encode an array stride; the full
    // data/metadata ranges were validated above (m5_drm.rs:238-241).
    p.depth_stride = 0;
    p.depth_aux_stride = 0;
    let stencil_stripped = p.stencil_buffer != 0;
    if stencil_stripped {
        p.stencil_buffer = 0;
        p.stencil_aux_buffer = 0;
        p.stencil_stride = 0;
        p.stencil_aux_stride = 0;
        p.depth_flags &= !ZLS_STENCIL_COMPRESSION;
        // Same derivation as translate_render_command, now without stencil.
        p.partial_depth_flags = p.depth_flags
            | if p.depth_buffer != 0 { ZLS_DEPTH_TRANSFER } else { 0 };
        if p.depth_buffer == 0 {
            // Neither plane remains: the M5 "no ZLS" envelope.
            p.depth_dimensions = 0;
        }
    }
    // ISP_BGOBJVALS: the builder writes value | 0x300 (g17_render.rs:800),
    // so the Mesa constant maps to zero; admit_envelope refused anything else.
    p.stencil_clear_value = 0;
    p.aux_fb_flags = G18_AUX_FB_FLAGS;
    p.tile_config |= G18_MAIN_TILE_FLAGS;

    let mut controls = g18_render::g18_default_controls(&p);
    // admit_envelope permits one sample only. The older UAPI translator's
    // tib_blocks is storage size, whereas G18 +770 / 10051 is sample control:
    // native run112 has zero at 1x and 0x40000 at 4x. Keep storage accounting
    // in p.tib_blocks for the independent +4d0 metadata.
    controls.fragment_sample_control = 0;
    Ok(G18ApplicationRender {
        parameters: p,
        controls,
        target: G18TargetRequest {
            width: p.width,
            height: p.height,
            layers: p.layers,
            utile: (p.utile_width, p.utile_height),
            samples: p.samples,
        },
        vdm_base: command.vdm_base,
        vertex_attachments: translated.vertex_attachments,
        fragment_attachments: translated.fragment_attachments,
        stencil_stripped,
        flags: command.flags,
    })
}

/// Stage timestamps a retired job can report, in ticks of the architectural
/// counter (`CNTPCT_EL0`, the `DRM_IOCTL_ASAHI_GET_TIME` source):
/// `[vertex start, vertex end, fragment start, fragment end]`.
#[derive(Debug, Copy, Clone, Default, PartialEq, Eq)]
pub(crate) struct G18RenderRetirement {
    pub(crate) stage_ticks: Option<[u64; 4]>,
}

/// The job boundary. The conductor implements this over the G18 render
/// runtime (`g18_graphics_job.rs` and its queues/PB/UMA owners).
///
/// Contract for `submit_render`:
/// - calls are serialized by the adapter (one at a time per device);
/// - the job fills every field [`job_owned_fields_clear`] lists, allocates targets
///   for `render.target`, maps them into `vm` (both compact views while risk
///   #1 is open), builds registers from `render.parameters/controls`, copies
///   `render.descriptor_pointers()` into its 3D command fields, encodes,
///   publishes and waits;
/// - `Ok` only after both TA and 3D retired with no fault or recovery;
/// - any error after publication must record it in `vm.status()`, retain
///   every owner and make `G18DrmRuntime::is_crashed` true (M5 contract,
///   `m5_live_boot.rs:639-656`).
#[cfg(not(test))]
pub(crate) trait G18RenderJobSink: Send + Sync {
    fn submit_render(&self, vm: &crate::mmu::Vm, render: &G18ApplicationRender)
        -> kernel::error::Result<G18RenderRetirement>;
}
