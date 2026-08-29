// SPDX-License-Identifier: GPL-2.0-only OR MIT

#![cfg_attr(not(test), allow(dead_code))]

//! Pure T8140/G17P render-command construction.
//!
//! The byte layout comes from m1n1
//! `916548424c00c72176326e3811badedce5714a99`. The source profile carries the
//! hardware-tested TA and fragment register recipes, the compiler-derived EOT
//! resource binding `0x8`, full-width load binding, and tagged BG/EOT program
//! pointers. Descriptor construction also writes the duplicated structural
//! pipeline fields. Queue construction returns ordered memory writes only;
//! allocation, cache maintenance, barriers, doorbells, MMIO, and completion
//! stay with the retained runtime.

const TA_REGISTER_COUNT: usize = 73;
const FRAGMENT_REGISTER_COUNT: usize = 89;
const PARTIAL_STORE_REGISTER_COUNT: usize = 16;
const PARTIAL_RESUME_REGISTER_COUNT: usize = 23;
const PARTIAL_LOAD_REGISTER_COUNT: usize = 10;
const REGISTER_BYTES: usize = 12;

pub(crate) const TA_DESCRIPTOR_SIZE: usize = 0x9c0;
pub(crate) const FRAGMENT_DESCRIPTOR_SIZE: usize = 0x2240;
pub(crate) const G17P_EOT_RESOURCE_SPEC: u64 = 0x8;
pub(crate) const G17P_LOAD_RESOURCE_SPEC: u64 = 0x0007_8000_0000_0040;
pub(crate) const G17P_SOURCE_BG_PIPELINE: u64 = 0x0000_0100_0199_0244;
pub(crate) const G17P_SOURCE_EOT_PIPELINE: u64 = 0x0000_0100_0199_0644;

const TILE_WIDTH: u64 = 32;
const TILE_HEIGHT: u64 = 32;
const MACRO_TILES_X: u64 = 4;
const MACRO_TILES_Y: u64 = 4;
const REGION_ENTRY_SIZE: u64 = 5;
const EVENT_SELECTOR: u32 = 0x0e;
const EVENT_SUBTYPE_BASE: u32 = 0x0001_0000;
const EVENT_RECORD_SIZE: usize = 0x40;
const RING_SLOT_SIZE: usize = 0x18;
const QUEUE_WRITE_OFFSET: u64 = 0x40;

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) enum RenderBuildError {
    ZeroDimensions,
    LayerCountOutOfRange,
    UtileDimensions,
    AddressBeforeContext,
    BufferTooSmall,
    ArithmeticOverflow,
    QueueGridOutOfRange,
    RingSlotOutOfRange,
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) struct RegisterWrite {
    pub(crate) number: u32,
    pub(crate) value: u64,
}

impl RegisterWrite {
    const fn new(number: u32, value: u64) -> Self {
        Self { number, value }
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) struct G17pRenderParameters {
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) context_base: u64,
    pub(crate) tilemap: u64,
    pub(crate) heapmeta: u64,
    pub(crate) tpc: u64,
    pub(crate) deflake_1: u64,
    pub(crate) deflake_2: u64,
    pub(crate) deflake_3: u64,
    pub(crate) encoder: u64,
    pub(crate) ta_status: u64,
    pub(crate) fragment_status: u64,
    pub(crate) store_pipeline_bind: u64,
    pub(crate) store_pipeline: u64,
    pub(crate) load_pipeline_bind: u64,
    pub(crate) load_pipeline: u64,
    pub(crate) partial_store_pipeline_bind: u64,
    pub(crate) partial_store_pipeline: u64,
    pub(crate) partial_load_pipeline_bind: u64,
    pub(crate) partial_load_pipeline: u64,
    pub(crate) scissor_array: u64,
    pub(crate) depth_bias_array: u64,
    pub(crate) aux_fb: u64,
    pub(crate) occlusion_query_base: u64,
    pub(crate) depth_buffer: u64,
    pub(crate) stencil_buffer: u64,
    pub(crate) depth_aux_buffer: u64,
    pub(crate) stencil_aux_buffer: u64,
    pub(crate) depth_stride: u64,
    pub(crate) stencil_stride: u64,
    pub(crate) depth_aux_stride: u64,
    pub(crate) stencil_aux_stride: u64,
    pub(crate) depth_clear_value_bits: u32,
    pub(crate) stencil_clear_value: u32,
    pub(crate) depth_flags: u64,
    pub(crate) depth_dimensions: u64,
    pub(crate) merge_upper_x_bits: u32,
    pub(crate) merge_upper_y_bits: u32,
    pub(crate) layers: u16,
    pub(crate) utile_width: u8,
    pub(crate) utile_height: u8,
    pub(crate) utile_config: u64,
    pub(crate) multisample_control: u64,
    pub(crate) ppp_control: u64,
    pub(crate) tib_blocks: u64,
    pub(crate) tile_config: u64,
    pub(crate) aux_fb_flags: u64,
    pub(crate) aux_fb_page_count: u64,
    pub(crate) cycle: u64,
    pub(crate) record_index: u64,
    pub(crate) lifecycle: u64,
    pub(crate) work_stamp: u64,
    pub(crate) sampler_array: u64,
    pub(crate) sampler_count: u32,
    pub(crate) process_empty_tiles: bool,
    pub(crate) ta_timestamp_start: u64,
    pub(crate) ta_timestamp_end: u64,
    pub(crate) ta_user_timestamp_start: u64,
    pub(crate) ta_user_timestamp_end: u64,
    pub(crate) fragment_timestamp_start: u64,
    pub(crate) fragment_timestamp_end: u64,
    pub(crate) fragment_user_timestamp_start: u64,
    pub(crate) fragment_user_timestamp_end: u64,
}

impl G17pRenderParameters {
    pub(crate) const fn source_rsrc8() -> Self {
        Self {
            width: 64,
            height: 64,
            context_base: 0x0000_0010_0000_0000,
            tilemap: 0x0000_0010_001b_0000,
            heapmeta: 0x0000_0010_001b_5000,
            tpc: 0x0000_0010_0024_0000,
            deflake_1: 0x0000_0010_0006_82a0,
            deflake_2: 0x0000_0010_0006_8020,
            deflake_3: 0x0000_0010_0006_8000,
            encoder: 0x0000_0010_0001_8000,
            ta_status: 0x0000_0010_0007_8000,
            fragment_status: 0x0000_0010_001a_8000,
            store_pipeline_bind: G17P_EOT_RESOURCE_SPEC,
            store_pipeline: G17P_SOURCE_EOT_PIPELINE,
            load_pipeline_bind: G17P_LOAD_RESOURCE_SPEC,
            load_pipeline: G17P_SOURCE_BG_PIPELINE,
            partial_store_pipeline_bind: 0,
            partial_store_pipeline: 0,
            partial_load_pipeline_bind: 0,
            partial_load_pipeline: 0,
            scissor_array: 0x0000_0100_019a_0000,
            depth_bias_array: 0x0000_0100_01af_8000,
            aux_fb: 0x0000_0100_01aa_8000,
            occlusion_query_base: 0,
            depth_buffer: 0,
            stencil_buffer: 0,
            depth_aux_buffer: 0,
            stencil_aux_buffer: 0,
            depth_stride: 0,
            stencil_stride: 0,
            depth_aux_stride: 0,
            stencil_aux_stride: 0,
            depth_clear_value_bits: 0x3f80_0000,
            stencil_clear_value: 0,
            depth_flags: 0,
            depth_dimensions: 0,
            merge_upper_x_bits: 0x3cdd_b3d9,
            merge_upper_y_bits: 0x3cdd_b3d9,
            layers: 1,
            utile_width: 32,
            utile_height: 32,
            utile_config: 0xa000,
            multisample_control: 0x88,
            ppp_control: 0x202,
            tib_blocks: 8,
            tile_config: 0x10280,
            aux_fb_flags: 0xc001,
            aux_fb_page_count: 0x10_0000,
            cycle: 0x17_8020,
            record_index: 0x8_0005,
            lifecycle: 0,
            work_stamp: 0x100,
            sampler_array: 0,
            sampler_count: 0,
            process_empty_tiles: true,
            ta_timestamp_start: 0xffff_fc20_0002_4c68,
            ta_timestamp_end: 0,
            ta_user_timestamp_start: 0,
            ta_user_timestamp_end: 0,
            fragment_timestamp_start: 0xffff_fc20_0002_4c68,
            fragment_timestamp_end: 0xffff_fc20_0002_4c70,
            fragment_user_timestamp_start: 0,
            fragment_user_timestamp_end: 0,
        }
    }
}

#[derive(Debug, Copy, Clone)]
struct Geometry {
    tiles_x: u64,
    tiles_y: u64,
    size1: u64,
    size2: u64,
    size3: u64,
    x_blocks: u64,
    y_blocks: u64,
    screen: u64,
    pixels: u64,
    macro_size: u64,
}

const fn align(value: u64, alignment: u64) -> u64 {
    (value + alignment - 1) & !(alignment - 1)
}

fn geometry(parameters: &G17pRenderParameters) -> Result<Geometry, RenderBuildError> {
    if parameters.width == 0 || parameters.height == 0 {
        return Err(RenderBuildError::ZeroDimensions);
    }
    if parameters.layers == 0 || parameters.layers > 2048 {
        return Err(RenderBuildError::LayerCountOutOfRange);
    }

    let width = parameters.width as u64;
    let height = parameters.height as u64;
    let tiles_x = (width + TILE_WIDTH - 1) / TILE_WIDTH;
    let tiles_y = (height + TILE_HEIGHT - 1) / TILE_HEIGHT;
    let (utile_width, utile_height) = (parameters.utile_width, parameters.utile_height);
    if !matches!((utile_width, utile_height), (32, 32) | (32, 16) | (16, 16)) {
        return Err(RenderBuildError::UtileDimensions);
    }
    let utiles_per_tile_x = TILE_WIDTH / utile_width as u64;
    let utiles_per_tile_y = TILE_HEIGHT / utile_height as u64;
    let utiles_per_tile = utiles_per_tile_x * utiles_per_tile_y;
    let mtile_x1 = align((tiles_x + MACRO_TILES_X - 1) / MACRO_TILES_X, 4);
    let mtile_y1 = align((tiles_y + MACRO_TILES_Y - 1) / MACRO_TILES_Y, 4);
    let mtile_x2 = 2 * mtile_x1;
    let mtile_x3 = 3 * mtile_x1;
    let mtile_y2 = 2 * mtile_y1;
    let mtile_y3 = 3 * mtile_y1;
    let mtile_stride = mtile_x1 * mtile_y1;
    let size1 = (REGION_ENTRY_SIZE * mtile_stride * utiles_per_tile + 3) / 4;

    Ok(Geometry {
        tiles_x,
        tiles_y,
        size1,
        size2: mtile_stride,
        size3: 2 * mtile_stride * utiles_per_tile,
        x_blocks: mtile_x3 | (mtile_x2 << 9) | (mtile_x1 << 18),
        y_blocks: mtile_y3 | (mtile_y2 << 9) | (mtile_y1 << 18),
        screen: ((tiles_y - 1) << 12) | (tiles_x - 1),
        pixels: (width - 1) | ((height - 1) << 16),
        macro_size: mtile_y1 * utiles_per_tile_y | ((mtile_x1 * utiles_per_tile_x) << 16),
    })
}

fn ta_offset(parameters: &G17pRenderParameters, address: u64) -> Result<u64, RenderBuildError> {
    address
        .checked_sub(parameters.context_base)
        .ok_or(RenderBuildError::AddressBeforeContext)
}

pub(crate) fn build_ta_registers(
    p: &G17pRenderParameters,
) -> Result<[RegisterWrite; TA_REGISTER_COUNT], RenderBuildError> {
    let g = geometry(p)?;
    let tilemap = ta_offset(p, p.tilemap)?;
    let heapmeta = ta_offset(p, p.heapmeta)?;
    let tpc = ta_offset(p, p.tpc)?;
    let deflake_1 = ta_offset(p, p.deflake_1)?;
    let deflake_2 = ta_offset(p, p.deflake_2)?;
    let deflake_3 = ta_offset(p, p.deflake_3)?;
    let encoder = ta_offset(p, p.encoder)?;
    let layer_mode = if p.layers > 1 {
        0xe000 | (p.layers as u64 - 1)
    } else {
        0x8000
    };

    Ok([
        RegisterWrite::new(0x01748, 1),
        RegisterWrite::new(0x10141, 0x200),
        RegisterWrite::new(0x1c039, tilemap),
        RegisterWrite::new(0x1c9c8, tilemap),
        RegisterWrite::new(0x1c0a1, tpc),
        RegisterWrite::new(0x1c031, heapmeta | 0x8000_0000_0000_0000),
        RegisterWrite::new(0x1c9c0, heapmeta | 0x8000_0000_0000_0000),
        RegisterWrite::new(0x1c051, 0x003a_0012_006b_0003),
        RegisterWrite::new(0x1c061, 1),
        RegisterWrite::new(0x10149, p.utile_config),
        RegisterWrite::new(0x10139, p.multisample_control),
        RegisterWrite::new(0x10111, deflake_1),
        RegisterWrite::new(0x1c9b0, deflake_1),
        RegisterWrite::new(0x10119, deflake_2),
        RegisterWrite::new(0x1c9b8, deflake_2),
        RegisterWrite::new(0x1c958, 1),
        RegisterWrite::new(0x1c950, deflake_3 | 0x0004_0000_0000_0000),
        RegisterWrite::new(0x1c930, 0),
        RegisterWrite::new(0x1c880, encoder),
        RegisterWrite::new(0x1c079, heapmeta),
        RegisterWrite::new(0x1c9d8, heapmeta),
        RegisterWrite::new(0x10151, 0),
        RegisterWrite::new(0x1c199, 0),
        RegisterWrite::new(0x1c1a1, 0),
        RegisterWrite::new(0x1c1a9, 0),
        RegisterWrite::new(0x1c1b1, 0),
        RegisterWrite::new(0x1c1b9, 0),
        RegisterWrite::new(0x1c8f8, 0x8860),
        RegisterWrite::new(0x1c0b1, g.size1),
        RegisterWrite::new(0x1c850, g.size1),
        RegisterWrite::new(0x10131, p.multisample_control),
        RegisterWrite::new(0x10121, p.ppp_control),
        RegisterWrite::new(0x10129, g.pixels),
        RegisterWrite::new(0x101b9, g.screen),
        RegisterWrite::new(0x1c069, g.x_blocks),
        RegisterWrite::new(0x1c071, g.y_blocks),
        RegisterWrite::new(0x1c081, g.size2),
        RegisterWrite::new(0x1c0a9, g.size3),
        RegisterWrite::new(0x10171, 0x100),
        RegisterWrite::new(0x10169, layer_mode),
        RegisterWrite::new(0x0a309, 0),
        RegisterWrite::new(0x1c8e0, u64::MAX),
        RegisterWrite::new(0x1c8e8, u64::MAX),
        RegisterWrite::new(0x1c898, 0),
        RegisterWrite::new(0x101e1, 0x1c),
        RegisterWrite::new(0x1c9e8, 0),
        RegisterWrite::new(0x1a099, 0),
        RegisterWrite::new(0x1a0a1, 0),
        RegisterWrite::new(0x1a069, 0),
        RegisterWrite::new(0x1a071, 0),
        RegisterWrite::new(0x1a0c9, 0),
        RegisterWrite::new(0x1a0d1, 0),
        RegisterWrite::new(0x101c9, 0),
        RegisterWrite::new(0x0d471, 0),
        RegisterWrite::new(0x1a0f1, 8),
        RegisterWrite::new(0x10799, 0xff_0000),
        RegisterWrite::new(0x1c830, 0),
        RegisterWrite::new(0x1ca30, p.cycle),
        RegisterWrite::new(0x16c39, p.cycle),
        RegisterWrite::new(0x1c910, p.record_index),
        RegisterWrite::new(0x0a5a1, 0x0000_00fe_0040_0020),
        RegisterWrite::new(0x0d419, 0x0000_0002_0000_0001),
        RegisterWrite::new(0x1ca10, p.lifecycle),
        RegisterWrite::new(0x014a1, p.lifecycle),
        RegisterWrite::new(0x0a349, p.lifecycle),
        RegisterWrite::new(0x10209, p.work_stamp),
        RegisterWrite::new(0x1c9f0, p.work_stamp),
        RegisterWrite::new(0x14320, p.work_stamp),
        RegisterWrite::new(0x14308, 0),
        RegisterWrite::new(0x14318, p.ta_status | 1),
        RegisterWrite::new(0x01740, 1),
        RegisterWrite::new(0x1c880, deflake_1),
        RegisterWrite::new(0x1c898, 1),
    ])
}

pub(crate) fn build_fragment_registers(
    p: &G17pRenderParameters,
) -> Result<[RegisterWrite; FRAGMENT_REGISTER_COUNT], RenderBuildError> {
    let g = geometry(p)?;
    let tile_state = 0x3717f
        | ((g.tiles_x - 1) << 44)
        | ((g.tiles_y - 1) << 53)
        | 0x20_0000_0000
        | (if p.layers > 1 { 0x1_0000_0000 } else { 0 })
        | ((p.utile_config & 0xf000) << 28);

    Ok([
        RegisterWrite::new(0x01739, 1),
        RegisterWrite::new(0x10009, p.utile_config),
        RegisterWrite::new(0x15379, p.store_pipeline_bind),
        RegisterWrite::new(0x15381, p.store_pipeline),
        RegisterWrite::new(0x15369, p.load_pipeline_bind),
        RegisterWrite::new(0x15371, p.load_pipeline),
        RegisterWrite::new(0x15131, p.merge_upper_x_bits as u64),
        RegisterWrite::new(0x15139, p.merge_upper_y_bits as u64),
        RegisterWrite::new(0x100a1, 0),
        RegisterWrite::new(0x15069, 0),
        RegisterWrite::new(0x15071, 0),
        RegisterWrite::new(0x16058, 0),
        RegisterWrite::new(0x10019, p.multisample_control),
        RegisterWrite::new(0x100b1, g.macro_size),
        RegisterWrite::new(0x16030, g.macro_size),
        RegisterWrite::new(0x100d9, g.screen),
        RegisterWrite::new(0x0a301, 0),
        RegisterWrite::new(0x10791, 0xff_0200),
        RegisterWrite::new(0x16098, p.heapmeta),
        RegisterWrite::new(0x15109, p.scissor_array),
        RegisterWrite::new(0x15101, p.depth_bias_array),
        RegisterWrite::new(0x15021, p.aux_fb_flags),
        RegisterWrite::new(0x15211, ((p.height as u64) << 32) | p.width as u64),
        RegisterWrite::new(0x15049, p.aux_fb_page_count),
        RegisterWrite::new(0x10051, p.tib_blocks),
        RegisterWrite::new(0x15321, p.depth_dimensions),
        RegisterWrite::new(0x15301, p.depth_clear_value_bits as u64),
        RegisterWrite::new(0x15309, (p.stencil_clear_value | 0x300) as u64),
        RegisterWrite::new(0x15311, p.occlusion_query_base),
        RegisterWrite::new(0x15319, p.depth_flags),
        RegisterWrite::new(0x15349, 0x0404_0404),
        RegisterWrite::new(0x15351, 0),
        RegisterWrite::new(0x15329, p.depth_buffer),
        RegisterWrite::new(0x15331, p.depth_buffer),
        RegisterWrite::new(0x15339, p.stencil_buffer),
        RegisterWrite::new(0x15341, p.stencil_buffer),
        RegisterWrite::new(0x15231, 0),
        RegisterWrite::new(0x15221, 0),
        RegisterWrite::new(0x15239, 0),
        RegisterWrite::new(0x15229, 0),
        RegisterWrite::new(0x15401, p.depth_stride),
        RegisterWrite::new(0x15421, p.depth_stride),
        RegisterWrite::new(0x15409, p.stencil_stride),
        RegisterWrite::new(0x15429, p.stencil_stride),
        RegisterWrite::new(0x153c1, p.depth_aux_buffer),
        RegisterWrite::new(0x15411, p.depth_aux_stride),
        RegisterWrite::new(0x153c9, p.depth_aux_buffer),
        RegisterWrite::new(0x15431, p.depth_aux_stride),
        RegisterWrite::new(0x153d1, p.stencil_aux_buffer),
        RegisterWrite::new(0x15419, p.stencil_aux_stride),
        RegisterWrite::new(0x153d9, p.stencil_aux_buffer),
        RegisterWrite::new(0x15439, p.stencil_aux_stride),
        RegisterWrite::new(0x16429, p.tilemap),
        RegisterWrite::new(0x16060, p.heapmeta),
        RegisterWrite::new(0x16431, (4 * g.size1) << 24),
        RegisterWrite::new(0x10039, p.tile_config),
        RegisterWrite::new(0x16020, 0),
        RegisterWrite::new(0x16451, 0),
        RegisterWrite::new(0x15359, 0),
        RegisterWrite::new(0x100b8, 0x8860),
        RegisterWrite::new(0x16461, p.aux_fb),
        RegisterWrite::new(0x16090, p.aux_fb),
        RegisterWrite::new(0x101e9, 0x1c),
        RegisterWrite::new(0x160a8, 0),
        RegisterWrite::new(0x16068, tile_state),
        RegisterWrite::new(0x1a0a9, 0),
        RegisterWrite::new(0x1a0b1, 0),
        RegisterWrite::new(0x1a079, 0),
        RegisterWrite::new(0x1a081, 0),
        RegisterWrite::new(0x1a0d9, 0),
        RegisterWrite::new(0x1a0e1, 0),
        RegisterWrite::new(0x101c1, 0),
        RegisterWrite::new(0x0d469, 0),
        RegisterWrite::new(0x1a0f9, 8),
        RegisterWrite::new(0x0a5a9, 0x0000_0146_0040_0020),
        RegisterWrite::new(0x0d429, 0x0000_0002_0000_0001),
        RegisterWrite::new(0x160e0, p.lifecycle),
        RegisterWrite::new(0x01499, p.lifecycle),
        RegisterWrite::new(0x0a341, p.lifecycle),
        RegisterWrite::new(0x1c838, 0),
        RegisterWrite::new(0x1ca28, p.cycle),
        RegisterWrite::new(0x10211, p.work_stamp),
        RegisterWrite::new(0x10420, p.work_stamp),
        RegisterWrite::new(0x14048, 0),
        RegisterWrite::new(0x14080, p.fragment_status | 1),
        RegisterWrite::new(0x01731, 1),
        RegisterWrite::new(0x16020, 1),
        RegisterWrite::new(0x16020, 0),
        RegisterWrite::new(0x16068, 0x4_0000),
    ])
}

fn partial_store_registers(
    p: &G17pRenderParameters,
) -> [RegisterWrite; PARTIAL_STORE_REGISTER_COUNT] {
    [
        RegisterWrite::new(0x15379, p.partial_store_pipeline_bind),
        RegisterWrite::new(0x15381, p.partial_store_pipeline),
        RegisterWrite::new(0x10039, p.tile_config),
        RegisterWrite::new(0x15359, 0x20),
        RegisterWrite::new(0x15331, p.depth_buffer),
        RegisterWrite::new(0x153c9, p.depth_aux_buffer),
        RegisterWrite::new(0x15341, p.stencil_buffer),
        RegisterWrite::new(0x153d9, p.stencil_aux_buffer),
        RegisterWrite::new(0x15421, p.depth_stride),
        RegisterWrite::new(0x15431, p.depth_aux_stride),
        RegisterWrite::new(0x15429, p.stencil_stride),
        RegisterWrite::new(0x15439, p.stencil_aux_stride),
        RegisterWrite::new(0x15221, 0),
        RegisterWrite::new(0x15229, 0),
        RegisterWrite::new(0x15319, p.depth_flags),
        RegisterWrite::new(0x15349, 0x0404_0404),
    ]
}

fn partial_resume_registers(
    p: &G17pRenderParameters,
) -> [RegisterWrite; PARTIAL_RESUME_REGISTER_COUNT] {
    [
        RegisterWrite::new(0x15379, p.partial_store_pipeline_bind),
        RegisterWrite::new(0x15381, p.partial_store_pipeline),
        RegisterWrite::new(0x15369, p.partial_load_pipeline_bind),
        RegisterWrite::new(0x15371, p.partial_load_pipeline),
        RegisterWrite::new(0x10039, p.tile_config & 0xffff),
        RegisterWrite::new(0x15359, 0x20),
        RegisterWrite::new(0x15331, p.depth_buffer),
        RegisterWrite::new(0x153c9, p.depth_aux_buffer),
        RegisterWrite::new(0x15341, p.stencil_buffer),
        RegisterWrite::new(0x153d9, p.stencil_aux_buffer),
        RegisterWrite::new(0x15421, p.depth_stride),
        RegisterWrite::new(0x15431, p.depth_aux_stride),
        RegisterWrite::new(0x15429, p.stencil_stride),
        RegisterWrite::new(0x15439, p.stencil_aux_stride),
        RegisterWrite::new(0x15221, 0),
        RegisterWrite::new(0x15229, 0),
        RegisterWrite::new(0x15309, (p.stencil_clear_value | 0x300) as u64),
        RegisterWrite::new(0x15329, p.depth_buffer),
        RegisterWrite::new(0x153c1, p.depth_aux_buffer),
        RegisterWrite::new(0x15339, p.stencil_buffer),
        RegisterWrite::new(0x153d1, p.stencil_aux_buffer),
        RegisterWrite::new(0x15319, p.depth_flags),
        RegisterWrite::new(0x15349, 0x0404_0404),
    ]
}

fn partial_load_registers(
    p: &G17pRenderParameters,
) -> [RegisterWrite; PARTIAL_LOAD_REGISTER_COUNT] {
    [
        RegisterWrite::new(0x15369, p.partial_load_pipeline_bind),
        RegisterWrite::new(0x15371, p.partial_load_pipeline),
        RegisterWrite::new(0x10039, p.tile_config & 0xffff),
        RegisterWrite::new(0x15309, (p.stencil_clear_value | 0x300) as u64),
        RegisterWrite::new(0x15329, p.depth_buffer),
        RegisterWrite::new(0x153c1, p.depth_aux_buffer),
        RegisterWrite::new(0x15339, p.stencil_buffer),
        RegisterWrite::new(0x153d1, p.stencil_aux_buffer),
        RegisterWrite::new(0x15319, p.depth_flags),
        RegisterWrite::new(0x15349, 0x0404_0404),
    ]
}

fn put_u8(raw: &mut [u8], offset: usize, value: u8) {
    raw[offset] = value;
}

fn put_u16(raw: &mut [u8], offset: usize, value: u16) {
    raw[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
}

fn put_u32(raw: &mut [u8], offset: usize, value: u32) {
    raw[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

fn put_u64(raw: &mut [u8], offset: usize, value: u64) {
    raw[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}

fn encode_registers(raw: &mut [u8], offset: usize, registers: &[RegisterWrite]) {
    for (index, register) in registers.iter().enumerate() {
        let entry = offset + index * REGISTER_BYTES;
        put_u32(raw, entry, register.number);
        put_u64(raw, entry + 4, register.value);
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) enum RenderDescriptorKind {
    Tiling,
    Fragment,
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) struct RenderDescriptorObjects {
    pub(crate) record_a: u64,
    pub(crate) shared: u64,
    pub(crate) record_b: u64,
    pub(crate) zero: u64,
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) struct RenderDescriptorMetadata {
    pub(crate) context_id: u32,
    pub(crate) submission_ordinal: u32,
    pub(crate) queue_pair: u32,
    pub(crate) submit_sequence: u64,
}

impl RenderDescriptorMetadata {
    pub(crate) const fn source(kind: RenderDescriptorKind) -> Self {
        Self {
            context_id: 1,
            submission_ordinal: 0,
            queue_pair: 0,
            submit_sequence: match kind {
                RenderDescriptorKind::Tiling => 1,
                RenderDescriptorKind::Fragment => 0,
            },
        }
    }
}

fn descriptor_work_ordinal(ordinal: u32) -> u32 {
    ordinal + ordinal / 2
}

fn write_common_descriptor(
    kind: RenderDescriptorKind,
    objects: RenderDescriptorObjects,
    metadata: RenderDescriptorMetadata,
    registers: &[RegisterWrite],
    raw: &mut [u8],
) {
    let (selector, pointers, register_offset, pointer_gap) = match kind {
        RenderDescriptorKind::Tiling => (0u32, 0x10usize, 0x60usize, 8usize),
        RenderDescriptorKind::Fragment => (1u32, 0x20usize, 0xa0usize, 0usize),
    };
    put_u32(raw, 0, selector);
    put_u64(raw, 4, metadata.submit_sequence);
    put_u32(raw, 0x0c, metadata.context_id);

    let addresses = [
        objects.record_a,
        objects.shared,
        objects.record_b,
        objects.zero,
    ];
    let mut offset = pointers;
    for (index, address) in addresses.iter().enumerate() {
        put_u64(raw, offset, *address);
        offset += 8;
        if index == 0 {
            offset += pointer_gap;
        }
    }
    encode_registers(raw, register_offset, registers);

    let work = descriptor_work_ordinal(metadata.submission_ordinal);
    let stamp = metadata.context_id.max(1) * 0x100 + work;
    match kind {
        RenderDescriptorKind::Tiling => {
            put_u32(raw, 0x18, metadata.queue_pair);
            put_u32(raw, 0x304, metadata.queue_pair);
            put_u32(raw, 0x48, work);
            for field in [0x370, 0x37c, 0x388] {
                put_u32(raw, field, stamp);
            }
        }
        RenderDescriptorKind::Fragment => {
            put_u32(raw, 0x458, metadata.queue_pair);
            put_u32(raw, 0x470, stamp);
            put_u32(raw, 0x47c, stamp);
        }
    }
}

pub(crate) fn build_ta_descriptor(
    parameters: &G17pRenderParameters,
    objects: RenderDescriptorObjects,
    metadata: RenderDescriptorMetadata,
    raw: &mut [u8],
) -> Result<(), RenderBuildError> {
    if raw.len() < TA_DESCRIPTOR_SIZE {
        return Err(RenderBuildError::BufferTooSmall);
    }
    raw[..TA_DESCRIPTOR_SIZE].fill(0);
    let registers = build_ta_registers(parameters)?;
    write_common_descriptor(
        RenderDescriptorKind::Tiling,
        objects,
        metadata,
        &registers,
        raw,
    );

    let deflake = ta_offset(parameters, parameters.deflake_1)?;
    let sampler_max = if parameters.sampler_count == 0 {
        0
    } else {
        parameters.sampler_count + 1
    };
    put_u32(raw, 0x0768, 0x036c_0049);
    put_u64(raw, 0x0780, parameters.tpc);
    put_u8(raw, 0x0789, 0x78);
    put_u32(raw, 0x07d6, deflake as u32);
    put_u32(raw, 0x0876, u32::MAX);
    put_u64(raw, 0x087a, parameters.sampler_array);
    put_u32(raw, 0x0882, parameters.sampler_count);
    put_u32(raw, 0x0886, sampler_max);
    put_u8(raw, 0x0932, 0x44);
    put_u8(raw, 0x093c, 1);
    put_u8(raw, 0x094d, 1);
    put_u64(raw, 0x08fe, parameters.ta_timestamp_start);
    put_u64(raw, 0x0906, parameters.ta_timestamp_end);
    put_u64(raw, 0x090e, parameters.ta_user_timestamp_start);
    put_u64(raw, 0x0916, parameters.ta_user_timestamp_end);
    Ok(())
}

/// Build the TA descriptor used by the proven native cold opening.
///
/// The generic descriptor remains the older 9165484 profile. The first-work
/// path changes these exact fields immediately before queue publication.
pub(crate) fn build_cold_opening_ta_descriptor(
    parameters: &G17pRenderParameters,
    objects: RenderDescriptorObjects,
    raw: &mut [u8],
) -> Result<(), RenderBuildError> {
    build_ta_descriptor(
        parameters,
        objects,
        RenderDescriptorMetadata::source(RenderDescriptorKind::Tiling),
        raw,
    )?;
    put_u16(raw, 0x38, 0x47);
    put_u16(raw, 0x3a, 0x49);
    put_u16(raw, 0x3c, 0x49);
    put_u8(raw, 0x789, 0x08);
    put_u8(raw, 0x93e, 0xd0);
    put_u8(raw, 0x93f, 0x91);
    Ok(())
}

fn put_embedded_registers(
    raw: &mut [u8],
    header_offset: usize,
    program_offset: usize,
    registers: &[RegisterWrite],
) {
    let bytes = registers.len() * REGISTER_BYTES;
    put_u32(
        raw,
        header_offset,
        ((bytes as u32) << 16) | registers.len() as u32,
    );
    encode_registers(raw, program_offset, registers);
}

pub(crate) fn build_fragment_descriptor(
    parameters: &G17pRenderParameters,
    objects: RenderDescriptorObjects,
    metadata: RenderDescriptorMetadata,
    raw: &mut [u8],
) -> Result<(), RenderBuildError> {
    if raw.len() < FRAGMENT_DESCRIPTOR_SIZE {
        return Err(RenderBuildError::BufferTooSmall);
    }
    raw[..FRAGMENT_DESCRIPTOR_SIZE].fill(0);
    let geometry = geometry(parameters)?;
    let registers = build_fragment_registers(parameters)?;
    write_common_descriptor(
        RenderDescriptorKind::Fragment,
        objects,
        metadata,
        &registers,
        raw,
    );

    put_u64(raw, 0x40, parameters.tilemap);
    put_u64(raw, 0x48, parameters.multisample_control);
    put_u32(raw, 0x54, geometry.macro_size as u32);
    put_u32(raw, 0x68, parameters.merge_upper_x_bits);
    put_u32(raw, 0x6c, parameters.merge_upper_y_bits);
    put_u64(raw, 0x78, geometry.tiles_x * geometry.tiles_y);

    put_u32(
        raw,
        0x07a8,
        (((FRAGMENT_REGISTER_COUNT * REGISTER_BYTES) as u32) << 16)
            | FRAGMENT_REGISTER_COUNT as u32,
    );
    put_embedded_registers(raw, 0x0ec8, 0x07c0, &partial_store_registers(parameters));
    put_embedded_registers(raw, 0x15e8, 0x0ee0, &partial_resume_registers(parameters));
    put_embedded_registers(raw, 0x1d08, 0x1600, &partial_load_registers(parameters));

    put_u64(raw, 0x1d20, parameters.depth_bias_array);
    put_u64(raw, 0x1d30, parameters.scissor_array);
    put_u64(raw, 0x1d40, parameters.occlusion_query_base);
    put_u64(raw, 0x1e78, parameters.load_pipeline_bind);
    put_u64(raw, 0x1e80, parameters.load_pipeline);
    put_u64(raw, 0x1ea8, parameters.partial_load_pipeline_bind);
    put_u64(raw, 0x1eb0, parameters.partial_load_pipeline);
    put_u32(raw, 0x1ec0, 0x0404_0404);
    put_u64(raw, 0x1f38, parameters.tib_blocks);
    put_u64(raw, 0x1f40, parameters.aux_fb_flags);
    put_u32(raw, 0x1f48, parameters.width);
    put_u32(raw, 0x1f4c, parameters.height);
    put_u64(raw, 0x1f50, parameters.aux_fb_page_count);
    put_u64(raw, 0x1f58, parameters.tile_config);
    put_u32(raw, 0x1f78, parameters.store_pipeline_bind as u32);
    put_u64(raw, 0x1f7c, parameters.store_pipeline);
    put_u32(raw, 0x1f98, parameters.partial_store_pipeline_bind as u32);
    put_u64(raw, 0x1f9c, parameters.partial_store_pipeline);
    put_u32(raw, 0x1fa8, parameters.depth_clear_value_bits);
    put_u64(raw, 0x1fac, (parameters.tib_blocks << 33) | 0x300);
    put_u8(raw, 0x2100, (!parameters.process_empty_tiles) as u8);
    put_u32(raw, 0x2110, u32::MAX);
    put_u64(raw, 0x2114, parameters.sampler_array);
    put_u32(raw, 0x211c, parameters.sampler_count);
    put_u32(
        raw,
        0x2120,
        if parameters.sampler_count == 0 {
            0
        } else {
            parameters.sampler_count + 1
        },
    );
    put_u8(raw, 0x2124, parameters.process_empty_tiles as u8);
    put_u8(raw, 0x2128, 1);
    put_u64(raw, 0x2198, parameters.fragment_timestamp_start);
    put_u64(raw, 0x21a0, parameters.fragment_timestamp_end);
    put_u64(raw, 0x21a8, parameters.fragment_user_timestamp_start);
    put_u64(raw, 0x21b0, parameters.fragment_user_timestamp_end);
    put_u8(raw, 0x21cc, 0x53);
    put_u8(raw, 0x21d6, 1);
    put_u8(raw, 0x21e7, 1);
    put_u8(raw, 0x2208, 1);
    put_u8(raw, 0x220c, 1);
    put_u8(raw, 0x222d, 1);
    Ok(())
}

/// Build the fragment descriptor used by the proven native cold opening.
pub(crate) fn build_cold_opening_fragment_descriptor(
    parameters: &G17pRenderParameters,
    objects: RenderDescriptorObjects,
    raw: &mut [u8],
) -> Result<(), RenderBuildError> {
    build_fragment_descriptor(
        parameters,
        objects,
        RenderDescriptorMetadata::source(RenderDescriptorKind::Fragment),
        raw,
    )?;
    put_u32(raw, 0x50, 1);
    put_u16(raw, 0x80, 0x56);
    put_u16(raw, 0x82, 0x57);
    put_u16(raw, 0x84, 0x57);
    put_u16(raw, 0x88, 0x59);
    put_u32(raw, 0x90, 1);
    put_u32(raw, 0x215c, 0);
    put_u8(raw, 0x21d8, 0x10);
    put_u8(raw, 0x21d9, 0xa2);
    put_u8(raw, 0x222d, 0);
    Ok(())
}

/// Apply the queue-local fields that change as the canonical three-item group
/// is refreshed in place. `descriptor_gpu_va` is the low VM address firmware
/// uses for the descriptor's self pointers.
pub(crate) fn apply_g17p_retained_descriptor_fields(
    kind: RenderDescriptorKind,
    descriptor_gpu_va: u64,
    runtime_control: u64,
    submission_ordinal: u32,
    cold_opening: bool,
    raw: &mut [u8],
) -> Result<(), RenderBuildError> {
    let item_number = submission_ordinal
        .checked_add(1)
        .ok_or(RenderBuildError::ArithmeticOverflow)?;
    if item_number > 0xff {
        return Err(RenderBuildError::ArithmeticOverflow);
    }
    match kind {
        RenderDescriptorKind::Tiling => {
            if raw.len() < TA_DESCRIPTOR_SIZE {
                return Err(RenderBuildError::BufferTooSmall);
            }
            put_u64(
                raw,
                0x0760,
                descriptor_gpu_va
                    .checked_add(0x60)
                    .ok_or(RenderBuildError::ArithmeticOverflow)?,
            );
            put_u32(raw, 0x079c, 1);
            put_u32(raw, 0x07a0, item_number << 8);
            put_u32(raw, 0x07a8, item_number);
            put_u32(raw, 0x07b0, item_number | (item_number << 8));
            put_u32(raw, 0x08b4, (item_number << 24) | 0xffff);
            put_u32(raw, 0x08c4, item_number << 16);
            put_u32(raw, 0x08c8, item_number << 24);
            put_u32(raw, 0x08d4, submission_ordinal << 16);
            put_u64(raw, 0x0934, runtime_control);
            if cold_opening {
                put_u8(raw, 0x0789, 0x08);
                put_u8(raw, 0x093e, 0xd0);
                put_u8(raw, 0x093f, 0x91);
            }
        }
        RenderDescriptorKind::Fragment => {
            if raw.len() < FRAGMENT_DESCRIPTOR_SIZE {
                return Err(RenderBuildError::BufferTooSmall);
            }
            for (at, target) in [
                (0x07a0, 0x00a0),
                (0x0ec0, 0x07c0),
                (0x15e0, 0x0ee0),
                (0x1d00, 0x1600),
            ] {
                put_u64(
                    raw,
                    at,
                    descriptor_gpu_va
                        .checked_add(target)
                        .ok_or(RenderBuildError::ArithmeticOverflow)?,
                );
            }
            put_u32(raw, 0x2150, item_number << 8);
            put_u32(raw, 0x2154, 1);
            put_u32(raw, 0x215c, if cold_opening { 0 } else { item_number });
            put_u32(raw, 0x2160, item_number);
            put_u32(raw, 0x2164, item_number << 8);
            put_u32(raw, 0x2170, submission_ordinal);
            put_u64(raw, 0x21ce, runtime_control);
            if cold_opening {
                put_u8(raw, 0x21d8, 0x10);
                put_u8(raw, 0x21d9, 0xa2);
                put_u8(raw, 0x222d, 0);
            }
        }
    }
    Ok(())
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) struct AddressedBytes<const N: usize> {
    pub(crate) address: u64,
    pub(crate) bytes: [u8; N],
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) struct QueuePublicationInput {
    pub(crate) queue: u64,
    pub(crate) pointers: u64,
    pub(crate) item_ring: u64,
    pub(crate) channel_ring: u64,
    pub(crate) channel_producer: u64,
    pub(crate) items: [u64; 3],
    pub(crate) grid_index: u16,
    pub(crate) slot_index: u16,
    pub(crate) current_write: u32,
    pub(crate) group_number: u32,
    pub(crate) first_submit: bool,
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) struct QueuePublication {
    pub(crate) item_entries: [AddressedBytes<8>; 3],
    pub(crate) event: AddressedBytes<EVENT_RECORD_SIZE>,
    pub(crate) queue_write: AddressedBytes<4>,
    pub(crate) ring_slot: AddressedBytes<RING_SLOT_SIZE>,
    pub(crate) producer: AddressedBytes<4>,
    pub(crate) write_after: u32,
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) struct PairedQueuePublication {
    pub(crate) tiling: QueuePublication,
    pub(crate) fragment: QueuePublication,
}

pub(crate) fn build_queue_publication(
    kind: RenderDescriptorKind,
    input: QueuePublicationInput,
) -> Result<QueuePublication, RenderBuildError> {
    let write_after = input
        .current_write
        .checked_add(3)
        .ok_or(RenderBuildError::ArithmeticOverflow)?;
    if input.grid_index > 0xff {
        return Err(RenderBuildError::QueueGridOutOfRange);
    }
    if input.slot_index > 0xff {
        return Err(RenderBuildError::RingSlotOutOfRange);
    }
    if input.group_number > (u32::MAX >> 8) {
        return Err(RenderBuildError::ArithmeticOverflow);
    }

    let item_offset = (input.current_write as u64)
        .checked_mul(8)
        .ok_or(RenderBuildError::ArithmeticOverflow)?;
    let first_item = input
        .item_ring
        .checked_add(item_offset)
        .ok_or(RenderBuildError::ArithmeticOverflow)?;
    let item_entries = [
        AddressedBytes {
            address: first_item,
            bytes: input.items[0].to_le_bytes(),
        },
        AddressedBytes {
            address: first_item
                .checked_add(8)
                .ok_or(RenderBuildError::ArithmeticOverflow)?,
            bytes: input.items[1].to_le_bytes(),
        },
        AddressedBytes {
            address: first_item
                .checked_add(16)
                .ok_or(RenderBuildError::ArithmeticOverflow)?,
            bytes: input.items[2].to_le_bytes(),
        },
    ];

    let mut event = [0u8; EVENT_RECORD_SIZE];
    let counter = input.group_number << 8;
    put_u32(&mut event, 0x00, EVENT_SELECTOR);
    put_u32(
        &mut event,
        0x04,
        EVENT_SUBTYPE_BASE | input.grid_index as u32,
    );
    put_u32(&mut event, 0x08, counter);
    put_u32(
        &mut event,
        0x10,
        match kind {
            RenderDescriptorKind::Tiling => 0,
            RenderDescriptorKind::Fragment => 0x100,
        },
    );

    let mut ring_slot = [0u8; RING_SLOT_SIZE];
    put_u64(&mut ring_slot, 0x08, input.queue);
    put_u32(
        &mut ring_slot,
        0x10,
        match kind {
            RenderDescriptorKind::Tiling => 0,
            RenderDescriptorKind::Fragment => 1,
        },
    );
    let flags = (write_after & 0xffff)
        | ((input.grid_index as u32) << 16)
        | if input.first_submit { 1 << 24 } else { 0 };
    put_u32(&mut ring_slot, 0x14, flags);

    let queue_write_address = input
        .pointers
        .checked_add(QUEUE_WRITE_OFFSET)
        .ok_or(RenderBuildError::ArithmeticOverflow)?;
    let slot_offset = (input.slot_index as u64)
        .checked_mul(RING_SLOT_SIZE as u64)
        .ok_or(RenderBuildError::ArithmeticOverflow)?;
    let ring_slot_address = input
        .channel_ring
        .checked_add(slot_offset)
        .ok_or(RenderBuildError::ArithmeticOverflow)?;
    let producer = (input.slot_index as u32 + 1) & 0xff;
    Ok(QueuePublication {
        item_entries,
        event: AddressedBytes {
            address: input.items[2],
            bytes: event,
        },
        queue_write: AddressedBytes {
            address: queue_write_address,
            bytes: write_after.to_le_bytes(),
        },
        ring_slot: AddressedBytes {
            address: ring_slot_address,
            bytes: ring_slot,
        },
        producer: AddressedBytes {
            address: input.channel_producer,
            bytes: producer.to_le_bytes(),
        },
        write_after,
    })
}

pub(crate) fn build_paired_queue_publication(
    tiling: QueuePublicationInput,
    fragment: QueuePublicationInput,
) -> Result<PairedQueuePublication, RenderBuildError> {
    Ok(PairedQueuePublication {
        tiling: build_queue_publication(RenderDescriptorKind::Tiling, tiling)?,
        fragment: build_queue_publication(RenderDescriptorKind::Fragment, fragment)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fnv1a64(parts: &[&[u8]]) -> u64 {
        let mut value = 0xcbf2_9ce4_8422_2325u64;
        for part in parts {
            for byte in *part {
                value ^= *byte as u64;
                value = value.wrapping_mul(0x0000_0100_0000_01b3);
            }
        }
        value
    }

    fn encoded_register_hash(registers: &[RegisterWrite]) -> u64 {
        let mut raw = [0u8; FRAGMENT_REGISTER_COUNT * REGISTER_BYTES];
        encode_registers(&mut raw, 0, registers);
        fnv1a64(&[&raw[..registers.len() * REGISTER_BYTES]])
    }

    fn get_u32(raw: &[u8], offset: usize) -> u32 {
        u32::from_le_bytes(raw[offset..offset + 4].try_into().unwrap())
    }

    fn get_u64(raw: &[u8], offset: usize) -> u64 {
        u64::from_le_bytes(raw[offset..offset + 8].try_into().unwrap())
    }

    fn objects() -> RenderDescriptorObjects {
        RenderDescriptorObjects {
            record_a: 0xffff_fc20_0150_0000,
            shared: 0xffff_fc20_0160_0000,
            record_b: 0xffff_fc20_0158_0000,
            zero: 0xffff_fc20_0168_0000,
        }
    }

    #[test]
    fn source_profile_pins_rsrc8_and_tagged_programs() {
        let parameters = G17pRenderParameters::source_rsrc8();
        assert_eq!(parameters.store_pipeline_bind, 0x8);
        assert_eq!(parameters.load_pipeline_bind, 0x0007_8000_0000_0040);
        assert_eq!(parameters.store_pipeline, 0x0000_0100_0199_0644);
        assert_eq!(parameters.load_pipeline, 0x0000_0100_0199_0244);
    }

    #[test]
    fn register_programs_match_m1n1_9165484() {
        let parameters = G17pRenderParameters::source_rsrc8();
        let tiling = build_ta_registers(&parameters).unwrap();
        let fragment = build_fragment_registers(&parameters).unwrap();
        assert_eq!(tiling.len(), 73);
        assert_eq!(fragment.len(), 89);
        assert_eq!(encoded_register_hash(&tiling), 0x29dc_2725_a3c2_90c9);
        assert_eq!(encoded_register_hash(&fragment), 0xdb21_2bfe_1bbf_85c8);
        assert_eq!(fragment[2], RegisterWrite::new(0x15379, 0x8));
        assert_eq!(
            fragment[3],
            RegisterWrite::new(0x15381, G17P_SOURCE_EOT_PIPELINE)
        );
        assert_eq!(
            fragment[4],
            RegisterWrite::new(0x15369, G17P_LOAD_RESOURCE_SPEC)
        );
        assert_eq!(
            fragment[5],
            RegisterWrite::new(0x15371, G17P_SOURCE_BG_PIPELINE)
        );
    }

    #[test]
    fn descriptors_match_m1n1_9165484_and_duplicate_pipeline_fields() {
        let parameters = G17pRenderParameters::source_rsrc8();
        let mut tiling = [0u8; TA_DESCRIPTOR_SIZE];
        let mut fragment = [0u8; FRAGMENT_DESCRIPTOR_SIZE];
        build_ta_descriptor(
            &parameters,
            objects(),
            RenderDescriptorMetadata::source(RenderDescriptorKind::Tiling),
            &mut tiling,
        )
        .unwrap();
        build_fragment_descriptor(
            &parameters,
            objects(),
            RenderDescriptorMetadata::source(RenderDescriptorKind::Fragment),
            &mut fragment,
        )
        .unwrap();

        assert_eq!(fnv1a64(&[&tiling]), 0x8902_c437_9d74_1cc7);
        assert_eq!(fnv1a64(&[&fragment]), 0xdd6e_bcc7_77e7_834d);
        assert_eq!(get_u32(&fragment, 0xb8), 0x15379);
        assert_eq!(get_u64(&fragment, 0xbc), 0x8);
        assert_eq!(get_u64(&fragment, 0xc8), G17P_SOURCE_EOT_PIPELINE);
        assert_eq!(get_u64(&fragment, 0x1e78), G17P_LOAD_RESOURCE_SPEC);
        assert_eq!(get_u64(&fragment, 0x1e80), G17P_SOURCE_BG_PIPELINE);
        assert_eq!(get_u32(&fragment, 0x1f78), 0x8);
        assert_eq!(get_u64(&fragment, 0x1f7c), G17P_SOURCE_EOT_PIPELINE);
    }

    #[test]
    fn cold_opening_descriptors_apply_only_the_native_first_work_overrides() {
        let parameters = G17pRenderParameters::source_rsrc8();
        let mut tiling = [0u8; TA_DESCRIPTOR_SIZE];
        let mut fragment = [0u8; FRAGMENT_DESCRIPTOR_SIZE];
        build_cold_opening_ta_descriptor(&parameters, objects(), &mut tiling).unwrap();
        build_cold_opening_fragment_descriptor(&parameters, objects(), &mut fragment).unwrap();

        assert_eq!(&tiling[0x38..0x3e], &[0x47, 0, 0x49, 0, 0x49, 0]);
        assert_eq!(tiling[0x789], 0x08);
        assert_eq!(&tiling[0x93e..0x940], &[0xd0, 0x91]);

        assert_eq!(get_u32(&fragment, 0x50), 1);
        assert_eq!(&fragment[0x80..0x86], &[0x56, 0, 0x57, 0, 0x57, 0]);
        assert_eq!(&fragment[0x88..0x8a], &[0x59, 0]);
        assert_eq!(get_u32(&fragment, 0x90), 1);
        assert_eq!(get_u32(&fragment, 0x215c), 0);
        assert_eq!(&fragment[0x21d8..0x21da], &[0x10, 0xa2]);
        assert_eq!(fragment[0x222d], 0);

        assert_eq!(fnv1a64(&[&tiling]), 0xb81a_1c11_1ede_fe5f);
        assert_eq!(fnv1a64(&[&fragment]), 0xf846_54e9_74ac_6ad3);
    }

    #[test]
    fn retained_descriptor_refresh_keeps_canonical_self_pointers_and_advances_item_two() {
        let parameters = G17pRenderParameters::source_rsrc8();
        let mut tiling = [0u8; TA_DESCRIPTOR_SIZE];
        let mut fragment = [0u8; FRAGMENT_DESCRIPTOR_SIZE];
        let tiling_va = 0x70_0001_8000;
        let fragment_va = 0x70_0009_8000;
        build_ta_descriptor(
            &parameters,
            objects(),
            RenderDescriptorMetadata {
                context_id: 1,
                submission_ordinal: 1,
                queue_pair: 0,
                submit_sequence: 3,
            },
            &mut tiling,
        )
        .unwrap();
        build_fragment_descriptor(
            &parameters,
            objects(),
            RenderDescriptorMetadata {
                context_id: 1,
                submission_ordinal: 1,
                queue_pair: 0,
                submit_sequence: 2,
            },
            &mut fragment,
        )
        .unwrap();
        apply_g17p_retained_descriptor_fields(
            RenderDescriptorKind::Tiling,
            tiling_va,
            0xffff_fc20_c082_8000,
            1,
            false,
            &mut tiling,
        )
        .unwrap();
        apply_g17p_retained_descriptor_fields(
            RenderDescriptorKind::Fragment,
            fragment_va,
            0xffff_fc20_c082_8000,
            1,
            false,
            &mut fragment,
        )
        .unwrap();

        assert_eq!(get_u64(&tiling, 0x760), tiling_va + 0x60);
        assert_eq!(get_u32(&tiling, 0x7a0), 0x200);
        assert_eq!(get_u32(&tiling, 0x7a8), 2);
        assert_eq!(get_u32(&tiling, 0x8d4), 0x1_0000);
        assert_eq!(get_u64(&tiling, 0x934), 0xffff_fc20_c082_8000);
        assert_eq!(get_u64(&fragment, 0x7a0), fragment_va + 0xa0);
        assert_eq!(get_u64(&fragment, 0xec0), fragment_va + 0x7c0);
        assert_eq!(get_u64(&fragment, 0x15e0), fragment_va + 0xee0);
        assert_eq!(get_u64(&fragment, 0x1d00), fragment_va + 0x1600);
        assert_eq!(get_u32(&fragment, 0x215c), 2);
        assert_eq!(get_u32(&fragment, 0x2170), 1);
        assert_eq!(get_u64(&fragment, 0x21ce), 0xffff_fc20_c082_8000);
    }

    fn publication_input(kind: RenderDescriptorKind) -> QueuePublicationInput {
        let fragment = matches!(kind, RenderDescriptorKind::Fragment);
        let grid = fragment as u16;
        QueuePublicationInput {
            queue: 0xffff_fc20_c000_8000 + grid as u64 * 0xc0,
            pointers: 0xffff_fc20_0001_0000 + grid as u64 * 0x80,
            item_ring: 0xffff_fc20_c000_c000 + grid as u64 * 0x2870,
            channel_ring: 0xffff_fc20_c010_0000 + grid as u64 * 0x4000,
            channel_producer: 0xffff_fc20_0170_0008 + grid as u64 * 0x100,
            items: [
                0xffff_fc20_c020_0000 + grid as u64 * 0x1_0000,
                0xffff_fc20_c020_4000 + grid as u64 * 0x1_0000,
                0xffff_fc20_c020_8000 + grid as u64 * 0x1_0000,
            ],
            grid_index: grid,
            slot_index: 0,
            current_write: 0,
            group_number: 1,
            first_submit: true,
        }
    }

    fn publication_hash(publication: &QueuePublication) -> u64 {
        let mut value = 0xcbf2_9ce4_8422_2325u64;
        let mut add = |address: u64, bytes: &[u8]| {
            for byte in address.to_le_bytes() {
                value ^= byte as u64;
                value = value.wrapping_mul(0x0000_0100_0000_01b3);
            }
            for byte in (bytes.len() as u32).to_le_bytes() {
                value ^= byte as u64;
                value = value.wrapping_mul(0x0000_0100_0000_01b3);
            }
            for byte in bytes {
                value ^= *byte as u64;
                value = value.wrapping_mul(0x0000_0100_0000_01b3);
            }
        };
        for entry in &publication.item_entries {
            add(entry.address, &entry.bytes);
        }
        add(publication.event.address, &publication.event.bytes);
        add(
            publication.queue_write.address,
            &publication.queue_write.bytes,
        );
        add(publication.ring_slot.address, &publication.ring_slot.bytes);
        add(publication.producer.address, &publication.producer.bytes);
        value
    }

    #[test]
    fn paired_publication_matches_m1n1_9165484() {
        let publication = build_paired_queue_publication(
            publication_input(RenderDescriptorKind::Tiling),
            publication_input(RenderDescriptorKind::Fragment),
        )
        .unwrap();
        assert_eq!(publication.tiling.write_after, 3);
        assert_eq!(publication.fragment.write_after, 3);
        assert_eq!(publication_hash(&publication.tiling), 0x8d2e_1298_cc1d_39c7);
        assert_eq!(
            publication_hash(&publication.fragment),
            0x75e1_3fd1_c48e_51ae
        );
        assert_eq!(
            publication.tiling.ring_slot.bytes[0x14..0x18],
            0x0100_0003u32.to_le_bytes()
        );
        assert_eq!(
            publication.fragment.ring_slot.bytes[0x14..0x18],
            0x0101_0003u32.to_le_bytes()
        );
        assert_eq!(publication.tiling.producer.bytes, 1u32.to_le_bytes());
        assert_eq!(publication.fragment.producer.bytes, 1u32.to_le_bytes());
    }
}
