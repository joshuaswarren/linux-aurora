// SPDX-License-Identifier: GPL-2.0-only OR MIT
//! Owned TA/fragment resource graph for the bounded native-program test.
//! Construction leaves roots and queues unpublished. Submission wiring must
//! retain this owner through completion, faults and firmware shutdown.

use super::{graphics_memory::M5GraphicsMemory, graphics_usc, MappedObject};
use crate::{driver::AsahiDevice, g17_render, m5_render, mmu};
use kernel::prelude::*;

const HIGH: u64 = 0xffff_fc20_c100_0000;
const LOW: u64 = 0x70_0030_0000;
const BYTES: usize = 0x40000;
const TA: usize = 0;
const FRAGMENT: usize = 0x4000;
const SCHEDULER: usize = 0x8000;
const SUBMITTED: usize = 0x8100;
const PM: usize = 0xc000;
const PM_SHARED: usize = 0x10000;
const PM_COUNTER: usize = 0x14000;
const PM_BLOCKS: usize = 0x18000;
const USC: usize = 0x1c000;
const USC_INNER: usize = 0x20000;
const USC_STATISTICS: usize = 0x24000;
const SCENES: usize = 0x30000;
const SCENE_ZERO: usize = SCENES + 80 * 0x80;
const TIMESTAMPS: usize = 0x38000;
const PAGE_LIST_HIGH: u64 = HIGH + 0x40000;
const METRICS_HIGH: u64 = HIGH + 0x60000;
const PAGE_LIST: u64 = 0x10_0023_8000;
const METRICS: u64 = 0x10_0008_0000;
const USER_BASE: u64 = 0x10_0000_0000;
const BLOCKS: [u64; 3] = [0x10_0008_8000, 0x10_0011_0000, 0x10_0019_8000];
const BLOCK_BYTES: usize = 0x80000;
const PAGE_LIST_BYTES: usize = 0x1c000;
const PAGE_COUNT: u32 = 48;
pub(super) const PB_HARDWARE_ID: u32 = 0;

pub(super) struct M5GraphicsJob {
    // The first owner's context retracts its roots before page release.
    program: M5GraphicsMemory,
    aliases: KVec<mmu::KernelMapping>,
    graph_alias: mmu::KernelMapping,
    graph: MappedObject,
}

impl M5GraphicsJob {
    pub(super) fn new(dev: &AsahiDevice, uat: &mmu::Uat) -> Result<Self> {
        let mut program = M5GraphicsMemory::load(dev, uat)?;
        let mut graph = MappedObject::new_wc_at(dev, uat.kernel_vm(), HIGH,
            BYTES, mmu::PROT_FW_PRIV_RW)?;
        let graph_alias = graph.map_alias_at(uat.kernel_lower_vm(), LOW,
            mmu::PROT_GPU_SHARED_RW)?;
        for offset in (0..BYTES).step_by(mmu::UAT_PGSZ) {
            if uat.kernel_vm().translate_iova(HIGH + offset as u64)?
                != uat.kernel_lower_vm().translate_iova(LOW + offset as u64)? {
                return Err(EFAULT);
            }
        }
        // Discard captured allocator/output contents. The shader, pipeline
        // and input data remain in separately owned program pages.
        for address in BLOCKS {
            program.clear_pages(address, BLOCK_BYTES)?;
        }
        for (address, bytes) in [(METRICS, 0x4000), (PAGE_LIST, PAGE_LIST_BYTES),
            (USER_BASE + 0x220000, 0x4000), (USER_BASE + 0x228000, 0x8000)] {
            program.clear_pages(address, bytes)?;
        }
        for (block, base) in BLOCKS.into_iter().enumerate() {
            let first = ((base - USER_BASE) >> 15) as u32;
            for page in 0..16 {
                program.write_word(PAGE_LIST + ((block * 16 + page) * 4) as u64,
                    first + page as u32)?;
            }
        }
        let mut aliases = program.alias_kernel_pages(uat, PAGE_LIST,
            PAGE_LIST_HIGH, PAGE_LIST_BYTES)?;
        for alias in program.alias_kernel_pages(uat, METRICS, METRICS_HIGH, 0x4000)? {
            aliases.push(alias, GFP_KERNEL)?;
        }
        graph.with_bytes_mut(|out| {
            put64(out, SCHEDULER, HIGH + SUBMITTED as u64);
            put32(out, SCHEDULER + 0x10, 0x50);
            // Fresh native TA snapshot observes two pending stage records.
            put32(out, SUBMITTED, 2);
            for (block, base) in BLOCKS.into_iter().enumerate() {
                put32(out, PM_BLOCKS + block * 8, ((base - USER_BASE) >> 15) as u32);
            }
            put32(out, PM_SHARED, 3);
            put32(out, PM_SHARED + 4, 3);
            put32(out, PM_SHARED + 0x60, 1);
            put32(out, PM_COUNTER, 1);
            for (offset, value) in [(0xc, PB_HARDWARE_ID), (0x30, PAGE_LIST_BYTES as u32),
                (0x34, PAGE_COUNT), (0x38, 1650), (0x3c, 3), (0x54, PAGE_COUNT - 1),
                (0x58, BLOCK_BYTES as u32), (0x7c, 1650 * 16), (0x80, 550 * 16)] {
                put32(out, PM + offset, value);
            }
            for (offset, address) in [(0x20, PAGE_LIST_HIGH), (0x28, PAGE_LIST),
                (0x44, HIGH + PM_BLOCKS as u64), (0x4c, HIGH + PM_SHARED as u64),
                (0x64, HIGH + PM_COUNTER as u64), (0x84, 0x228000)] {
                put64(out, PM + offset, address);
            }
            // This sole job owns scene 1. M5 metrics use 64-byte spacing,
            // unlike Neo dwords. Other reserved scenes remain unassigned;
            // do not guess a scratch-ring wrap rule from a single draw.
            let selected_scene = SCENES + 0x80;
            put64(out, selected_scene, METRICS + 0x40);
            put64(out, selected_scene + 8, METRICS_HIGH + 0x40);
            put64(out, selected_scene + 0x28, 0x220040);
            put64(out, selected_scene + 0x40, HIGH + PM_SHARED as u64 + 0x40);
            put64(out, SCENES + 0x80 + 0x4c, 1);
            put32(out, USC_INNER, 1);
            program.usc().encode_initial_owner(HIGH + USC_INNER as u64,
                &mut out[USC..USC + 0x70])?;

            let (p, controls) = triangle_parameters();
            let objects = g17_render::RenderDescriptorObjects {
                record_a: HIGH + SCHEDULER as u64,
                shared: HIGH + PM as u64,
                record_b: HIGH + SCENES as u64 + 0x80,
                zero: HIGH + SCENE_ZERO as u64,
            };
            let metadata = g17_render::RenderDescriptorMetadata {
                context_id: 3, submission_ordinal: 0, command_buffer_state_word: 0,
                ta_hardware_buffer_id: PB_HARDWARE_ID, submit_sequence: 1,
            };
            m5_render::build_ta_descriptor_base(&p, &controls, objects, metadata,
                LOW + TA as u64, &mut out[TA..TA + g17_render::TA_DESCRIPTOR_SIZE])
                .map_err(|_| EINVAL)?;
            m5_render::build_fragment_descriptor_base(&p, &controls, objects,
                g17_render::RenderDescriptorMetadata { submit_sequence: 0, ..metadata },
                LOW + FRAGMENT as u64,
                &mut out[FRAGMENT..FRAGMENT + g17_render::FRAGMENT_DESCRIPTOR_SIZE])
                .map_err(|_| EINVAL)?;
            let (ta, fragment) = out.split_at_mut(FRAGMENT);
            let ta_usc = m5_render::M5RenderUscState {
                control: HIGH + USC as u64, required_bytes: 0x7d4000,
                allocated_bytes: graphics_usc::CAPACITY_BYTES,
                statistics: HIGH + USC_STATISTICS as u64,
            };
            m5_render::apply_usc_submission_state(ta, fragment, ta_usc,
                m5_render::M5RenderUscState {
                    required_bytes: 0x934000, statistics: ta_usc.statistics + 0x40, ..ta_usc
                }).map_err(|_| EINVAL)?;
            Ok(())
        })?;
        Ok(Self { program, aliases, graph_alias, graph })
    }

    pub(super) fn parameter_descriptor(&self) -> Result<[u32; 4]> {
        m5_render::initial_parameter_buffer_descriptor(PAGE_LIST, PAGE_COUNT).map_err(|_| EINVAL)
    }

    pub(super) fn uma_descriptor(&self) -> [u64; 4] { self.program.usc().descriptor() }

    pub(super) fn roots(&self) -> [u64; 2] { self.program.roots() }
}

fn put32(out: &mut [u8], offset: usize, value: u32) {
    out[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}
fn put64(out: &mut [u8], offset: usize, value: u64) {
    out[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}

/// Addresses are the bounded native-program bundle's user ABI. Kernel
/// objects and hardware IDs above are freshly owned by this job.
fn triangle_parameters() -> (g17_render::G17pRenderParameters, m5_render::M5RenderControls) {
    let mut p = g17_render::G17pRenderParameters::source_rsrc8();
    p.aux_fb = 0x10000288000;
    p.aux_fb_flags = 0x10c001;
    p.aux_fb_page_count = 0x100000;
    p.context_base = USER_BASE;
    p.cycle = 0x220040;
    p.deflake_1 = 0x1000068340;
    p.deflake_2 = 0x1000068040;
    p.deflake_3 = 0x1000068000;
    p.depth_aux_buffer = 0;
    p.depth_aux_stride = 0;
    p.depth_buffer = 0;
    p.depth_clear_value_bits = 0x3f800000;
    p.depth_dimensions = 0;
    p.depth_flags = 0;
    p.depth_stride = 0;
    p.encoder = 0x1000018000;
    p.fragment_lifecycle = 0x4089f4c0508e74c;
    p.fragment_status = 0x1000258000;
    p.heapmeta = 0x1000261000;
    p.width = 256;
    p.height = 256;
    p.layers = 1;
    p.lifecycle = 0x4089f4c0508e74d;
    p.load_pipeline = 0x10000170200;
    p.load_pipeline_bind = 0xffff800000000040;
    p.merge_upper_x_bits = 0x3bddb3d9;
    p.merge_upper_y_bits = 0x3bddb3d9;
    p.multisample_control = 0x88;
    p.occlusion_query_base = 0;
    p.ppp_control = 0x202;
    p.record_index = 0x80041;
    p.stencil_aux_buffer = 0;
    p.stencil_aux_stride = 0;
    p.stencil_buffer = 0;
    p.stencil_clear_value = 0;
    p.stencil_stride = 0;
    p.store_pipeline = 0x10000170400;
    p.store_pipeline_bind = 0;
    p.ta_hardware_buffer_id = PB_HARDWARE_ID;
    p.ta_status = 0x1000078000;
    p.tib_blocks = 0;
    p.tile_config = 0x10280;
    p.tilemap = 0x1000260000;
    p.tpc = 0x1000288000;
    p.usc_flist_hardware_buffer_id = graphics_usc::HARDWARE_ID;
    p.utile_config = 0xa000;
    p.utile_height = 32;
    p.utile_width = 32;
    p.absolute_pointers = false;
    p.partial_store_pipeline_bind = 0;
    p.partial_store_pipeline = 0x10000170400;
    p.partial_load_pipeline_bind = 0xffff800000000040;
    p.partial_load_pipeline = 0x10000170000;
    p.depth_bias_array = 0x100002d8000;
    p.scissor_array = 0x10000180000;
    p.ta_timestamp_start = HIGH + TIMESTAMPS as u64;
    p.ta_timestamp_end = p.ta_timestamp_start + 8;
    p.fragment_timestamp_start = p.ta_timestamp_start;
    p.fragment_timestamp_end = p.ta_timestamp_end;
    let controls = m5_render::M5RenderControls {
        ta: m5_render::M5RenderStageControls {
            resource_flags: 0x4054b, resource_control: 0x200000001, ptc_max: 0xc6, work_key: 0x300,
        },
        fragment: m5_render::M5RenderStageControls {
            resource_flags: 0x104054b, resource_control: 0x200000001, ptc_max: 0xce, work_key: 0x300,
        },
        ta_tile_control: 0x10c08860, ta_dispatch_control: 0x700,
        fragment_raster_control: 0x200000ff0200, fragment_tile_flags: 0x200003717f,
        store_pipeline_2_bind: 0, store_pipeline_2: 0x10000170400,
        partial_store_pipeline_2_bind: 0, partial_store_pipeline_2: 0x10000170400,
        partial_tile_flags: 0x200000,
    };
    (p, controls)
}
