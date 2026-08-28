// SPDX-License-Identifier: GPL-2.0-only OR MIT

#![cfg_attr(not(test), allow(dead_code))]

//! Pure modern-Asahi UAPI parsing and G17P command translation.
//!
//! The byte layouts match `drm_asahi_cmd_header`, `drm_asahi_cmd_render`,
//! `drm_asahi_cmd_compute`, and `drm_asahi_attachment`. Translation validates
//! the queue's 4 GiB USC window and every userspace range before constructing
//! G17P render parameters or compute-entry operands. This module allocates no
//! object, publishes no queue state, and performs no firmware or MMIO action.

#[cfg(not(test))]
use crate::{g17_render, g17_submission};
#[cfg(test)]
#[path = "g17_render.rs"]
mod g17_render;
#[cfg(test)]
#[allow(dead_code)]
#[path = "g17_submission.rs"]
mod g17_submission;

use g17_render::{
    G17pRenderParameters, RenderBuildError, RenderDescriptorMetadata, RenderDescriptorObjects,
};
use g17_submission::{
    G17PClBarrierDependency, G17PClKickEntryOperands, G17PClMcacheAperture, G17PClRceBinding,
};

pub(crate) const UAPI_COMMAND_HEADER_SIZE: usize = 8;
pub(crate) const UAPI_RENDER_SIZE: usize = 240;
pub(crate) const UAPI_COMPUTE_SIZE: usize = 64;
pub(crate) const UAPI_ATTACHMENT_SIZE: usize = 24;
pub(crate) const UAPI_MAX_ATTACHMENTS: usize = 16;
pub(crate) const UAPI_MAX_HARDWARE_COMMANDS: u32 = 64;
pub(crate) const UAPI_BARRIER_NONE: u16 = 0xffff;
pub(crate) const USC_WINDOW_SIZE: u64 = 1 << 32;

const COMMAND_RENDER: u16 = 0;
const COMMAND_COMPUTE: u16 = 1;
const SET_VERTEX_ATTACHMENTS: u16 = 2;
const SET_FRAGMENT_ATTACHMENTS: u16 = 3;
const SET_COMPUTE_ATTACHMENTS: u16 = 4;

const RENDER_VERTEX_SCRATCH: u32 = 1 << 0;
const RENDER_PROCESS_EMPTY_TILES: u32 = 1 << 1;
const RENDER_NO_VERTEX_CLUSTERING: u32 = 1 << 2;
const RENDER_DBIAS_IS_INT: u32 = 1 << 18;
const RENDER_FLAGS: u32 = RENDER_VERTEX_SCRATCH
    | RENDER_PROCESS_EMPTY_TILES
    | RENDER_NO_VERTEX_CLUSTERING
    | RENDER_DBIAS_IS_INT;

const SAMPLER_SIZE: u64 = 8;
const MAX_SAMPLERS: u32 = 1024;
const PAGE_SIZE: u64 = 0x4000;
const BG_RESOURCE_PREFIX: u64 = 0x0007_8000_0000_0000;

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) enum UapiTranslateError {
    HeaderTruncated,
    PayloadTruncated,
    PayloadTail,
    CommandType,
    AttachmentPayloadSize,
    AttachmentCount,
    AttachmentFields,
    AttachmentBarrier,
    HardwareCommandCount,
    BarrierFuture,
    QueueUscAlignment,
    QueueUscRange,
    AddressRange,
    AddressAlignment,
    Flags,
    Dimensions,
    Layers,
    UtileDimensions,
    Samples,
    TilebufferSize,
    SamplerCount,
    SamplerPair,
    HelperProgram,
    ProgramOffset,
    ZlsFields,
    StencilClearBits,
    RenderBuild(RenderBuildError),
}

impl From<RenderBuildError> for UapiTranslateError {
    fn from(value: RenderBuildError) -> Self {
        Self::RenderBuild(value)
    }
}

fn get_u16(raw: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes(raw[offset..offset + 2].try_into().unwrap())
}

fn get_u32(raw: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(raw[offset..offset + 4].try_into().unwrap())
}

fn get_u64(raw: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(raw[offset..offset + 8].try_into().unwrap())
}

fn padded_payload<const N: usize>(raw: &[u8]) -> Result<[u8; N], UapiTranslateError> {
    if raw.len() > N && raw[N..].iter().any(|byte| *byte != 0) {
        return Err(UapiTranslateError::PayloadTail);
    }
    let mut out = [0u8; N];
    let count = core::cmp::min(raw.len(), N);
    out[..count].copy_from_slice(&raw[..count]);
    Ok(out)
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) struct UapiCommandHeader {
    pub(crate) command_type: u16,
    pub(crate) size: u16,
    pub(crate) render_barrier: u16,
    pub(crate) compute_barrier: u16,
}

impl UapiCommandHeader {
    fn parse(raw: &[u8]) -> Result<Self, UapiTranslateError> {
        if raw.len() < UAPI_COMMAND_HEADER_SIZE {
            return Err(UapiTranslateError::HeaderTruncated);
        }
        Ok(Self {
            command_type: get_u16(raw, 0),
            size: get_u16(raw, 2),
            render_barrier: get_u16(raw, 4),
            compute_barrier: get_u16(raw, 6),
        })
    }
}

#[derive(Debug, Copy, Clone, Default, PartialEq, Eq)]
pub(crate) struct UapiAttachment {
    pub(crate) address: u64,
    pub(crate) size: u64,
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) struct UapiAttachmentList {
    pub(crate) entries: [UapiAttachment; UAPI_MAX_ATTACHMENTS],
    pub(crate) count: u8,
}

impl UapiAttachmentList {
    pub(crate) const EMPTY: Self = Self {
        entries: [UapiAttachment {
            address: 0,
            size: 0,
        }; UAPI_MAX_ATTACHMENTS],
        count: 0,
    };

    pub(crate) fn as_slice(&self) -> &[UapiAttachment] {
        &self.entries[..self.count as usize]
    }

    fn parse(raw: &[u8]) -> Result<Self, UapiTranslateError> {
        if raw.len() % UAPI_ATTACHMENT_SIZE != 0 {
            return Err(UapiTranslateError::AttachmentPayloadSize);
        }
        let count = raw.len() / UAPI_ATTACHMENT_SIZE;
        if count > UAPI_MAX_ATTACHMENTS {
            return Err(UapiTranslateError::AttachmentCount);
        }
        let mut out = Self::EMPTY;
        for index in 0..count {
            let offset = index * UAPI_ATTACHMENT_SIZE;
            if get_u32(raw, offset + 0x10) != 0 || get_u32(raw, offset + 0x14) != 0 {
                return Err(UapiTranslateError::AttachmentFields);
            }
            out.entries[index] = UapiAttachment {
                address: get_u64(raw, offset),
                size: get_u64(raw, offset + 8),
            };
        }
        out.count = count as u8;
        Ok(out)
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) struct UapiHelperProgram {
    pub(crate) binary: u32,
    pub(crate) config: u32,
    pub(crate) data: u64,
}

fn helper(raw: &[u8], offset: usize) -> UapiHelperProgram {
    UapiHelperProgram {
        binary: get_u32(raw, offset),
        config: get_u32(raw, offset + 4),
        data: get_u64(raw, offset + 8),
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) struct UapiProgram {
    pub(crate) usc: u32,
    pub(crate) resource_spec: u32,
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) struct UapiTimestamp {
    pub(crate) handle: u32,
    pub(crate) offset: u32,
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) struct UapiTimestamps {
    pub(crate) start: UapiTimestamp,
    pub(crate) end: UapiTimestamp,
}

fn timestamps(raw: &[u8], offset: usize) -> UapiTimestamps {
    UapiTimestamps {
        start: UapiTimestamp {
            handle: get_u32(raw, offset),
            offset: get_u32(raw, offset + 4),
        },
        end: UapiTimestamp {
            handle: get_u32(raw, offset + 8),
            offset: get_u32(raw, offset + 12),
        },
    }
}

fn program(raw: &[u8], offset: usize) -> UapiProgram {
    UapiProgram {
        usc: get_u32(raw, offset),
        resource_spec: get_u32(raw, offset + 4),
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) struct UapiZlsBuffer {
    pub(crate) base: u64,
    pub(crate) compression_base: u64,
    pub(crate) stride: u32,
    pub(crate) compression_stride: u32,
}

fn zls(raw: &[u8], offset: usize) -> UapiZlsBuffer {
    UapiZlsBuffer {
        base: get_u64(raw, offset),
        compression_base: get_u64(raw, offset + 8),
        stride: get_u32(raw, offset + 0x10),
        compression_stride: get_u32(raw, offset + 0x14),
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) struct UapiRenderCommand {
    pub(crate) flags: u32,
    pub(crate) depth_dimensions: u32,
    pub(crate) vdm_base: u64,
    pub(crate) vertex_helper: UapiHelperProgram,
    pub(crate) fragment_helper: UapiHelperProgram,
    pub(crate) scissor_base: u64,
    pub(crate) depth_bias_base: u64,
    pub(crate) occlusion_query_base: u64,
    pub(crate) depth: UapiZlsBuffer,
    pub(crate) stencil: UapiZlsBuffer,
    pub(crate) zls_control: u64,
    pub(crate) multisample_control: u64,
    pub(crate) sampler_heap: u64,
    pub(crate) ppp_control: u32,
    pub(crate) width: u16,
    pub(crate) height: u16,
    pub(crate) layers: u16,
    pub(crate) sampler_count: u16,
    pub(crate) utile_width: u8,
    pub(crate) utile_height: u8,
    pub(crate) samples: u8,
    pub(crate) sample_size: u8,
    pub(crate) merge_upper_x: u32,
    pub(crate) merge_upper_y: u32,
    pub(crate) background: UapiProgram,
    pub(crate) end_of_tile: UapiProgram,
    pub(crate) partial_background: UapiProgram,
    pub(crate) partial_end_of_tile: UapiProgram,
    pub(crate) depth_clear: u32,
    pub(crate) stencil_clear: u32,
    pub(crate) vertex_timestamps: UapiTimestamps,
    pub(crate) fragment_timestamps: UapiTimestamps,
}

impl UapiRenderCommand {
    fn parse(raw: &[u8]) -> Result<Self, UapiTranslateError> {
        let raw = padded_payload::<UAPI_RENDER_SIZE>(raw)?;
        Ok(Self {
            flags: get_u32(&raw, 0x00),
            depth_dimensions: get_u32(&raw, 0x04),
            vdm_base: get_u64(&raw, 0x08),
            vertex_helper: helper(&raw, 0x10),
            fragment_helper: helper(&raw, 0x20),
            scissor_base: get_u64(&raw, 0x30),
            depth_bias_base: get_u64(&raw, 0x38),
            occlusion_query_base: get_u64(&raw, 0x40),
            depth: zls(&raw, 0x48),
            stencil: zls(&raw, 0x60),
            zls_control: get_u64(&raw, 0x78),
            multisample_control: get_u64(&raw, 0x80),
            sampler_heap: get_u64(&raw, 0x88),
            ppp_control: get_u32(&raw, 0x90),
            width: get_u16(&raw, 0x94),
            height: get_u16(&raw, 0x96),
            layers: get_u16(&raw, 0x98),
            sampler_count: get_u16(&raw, 0x9a),
            utile_width: raw[0x9c],
            utile_height: raw[0x9d],
            samples: raw[0x9e],
            sample_size: raw[0x9f],
            merge_upper_x: get_u32(&raw, 0xa0),
            merge_upper_y: get_u32(&raw, 0xa4),
            background: program(&raw, 0xa8),
            end_of_tile: program(&raw, 0xb0),
            partial_background: program(&raw, 0xb8),
            partial_end_of_tile: program(&raw, 0xc0),
            depth_clear: get_u32(&raw, 0xc8),
            stencil_clear: get_u32(&raw, 0xcc),
            vertex_timestamps: timestamps(&raw, 0xd0),
            fragment_timestamps: timestamps(&raw, 0xe0),
        })
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) struct UapiComputeCommand {
    pub(crate) flags: u32,
    pub(crate) sampler_count: u32,
    pub(crate) control_stream_base: u64,
    pub(crate) control_stream_end: u64,
    pub(crate) sampler_heap: u64,
    pub(crate) helper: UapiHelperProgram,
    pub(crate) timestamps: UapiTimestamps,
}

impl UapiComputeCommand {
    fn parse(raw: &[u8]) -> Result<Self, UapiTranslateError> {
        let raw = padded_payload::<UAPI_COMPUTE_SIZE>(raw)?;
        Ok(Self {
            flags: get_u32(&raw, 0),
            sampler_count: get_u32(&raw, 4),
            control_stream_base: get_u64(&raw, 8),
            control_stream_end: get_u64(&raw, 0x10),
            sampler_heap: get_u64(&raw, 0x18),
            helper: helper(&raw, 0x20),
            timestamps: timestamps(&raw, 0x30),
        })
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) enum ParsedHardwareCommand {
    Render {
        index: u32,
        header: UapiCommandHeader,
        payload: UapiRenderCommand,
        vertex_attachments: UapiAttachmentList,
        fragment_attachments: UapiAttachmentList,
    },
    Compute {
        index: u32,
        header: UapiCommandHeader,
        payload: UapiComputeCommand,
        attachments: UapiAttachmentList,
    },
}

pub(crate) struct UapiCommandParser<'a> {
    raw: &'a [u8],
    offset: usize,
    command_index: u32,
    hardware_count: u32,
    render_count: u16,
    compute_count: u16,
    vertex_attachments: UapiAttachmentList,
    fragment_attachments: UapiAttachmentList,
    compute_attachments: UapiAttachmentList,
}

impl<'a> UapiCommandParser<'a> {
    pub(crate) const fn new(raw: &'a [u8]) -> Self {
        Self {
            raw,
            offset: 0,
            command_index: 0,
            hardware_count: 0,
            render_count: 0,
            compute_count: 0,
            vertex_attachments: UapiAttachmentList::EMPTY,
            fragment_attachments: UapiAttachmentList::EMPTY,
            compute_attachments: UapiAttachmentList::EMPTY,
        }
    }

    pub(crate) fn finish(&self) -> Result<u32, UapiTranslateError> {
        if self.offset != self.raw.len() {
            return Err(UapiTranslateError::PayloadTruncated);
        }
        if self.hardware_count == 0 || self.hardware_count > UAPI_MAX_HARDWARE_COMMANDS {
            return Err(UapiTranslateError::HardwareCommandCount);
        }
        Ok(self.hardware_count)
    }

    pub(crate) fn next_hardware(
        &mut self,
    ) -> Result<Option<ParsedHardwareCommand>, UapiTranslateError> {
        loop {
            if self.offset == self.raw.len() {
                return Ok(None);
            }
            if self.raw.len() - self.offset < UAPI_COMMAND_HEADER_SIZE {
                return Err(UapiTranslateError::HeaderTruncated);
            }
            let header = UapiCommandHeader::parse(&self.raw[self.offset..])?;
            self.offset += UAPI_COMMAND_HEADER_SIZE;
            let end = self
                .offset
                .checked_add(header.size as usize)
                .ok_or(UapiTranslateError::PayloadTruncated)?;
            if end > self.raw.len() {
                return Err(UapiTranslateError::PayloadTruncated);
            }
            let payload = &self.raw[self.offset..end];
            self.offset = end;
            self.command_index += 1;

            match header.command_type {
                COMMAND_RENDER | COMMAND_COMPUTE => {
                    if header.render_barrier != UAPI_BARRIER_NONE
                        && header.render_barrier > self.render_count
                    {
                        return Err(UapiTranslateError::BarrierFuture);
                    }
                    if header.compute_barrier != UAPI_BARRIER_NONE
                        && header.compute_barrier > self.compute_count
                    {
                        return Err(UapiTranslateError::BarrierFuture);
                    }
                    self.hardware_count += 1;
                    if self.hardware_count > UAPI_MAX_HARDWARE_COMMANDS {
                        return Err(UapiTranslateError::HardwareCommandCount);
                    }
                    if header.command_type == COMMAND_RENDER {
                        self.render_count += 1;
                        return Ok(Some(ParsedHardwareCommand::Render {
                            index: self.command_index,
                            header,
                            payload: UapiRenderCommand::parse(payload)?,
                            vertex_attachments: self.vertex_attachments,
                            fragment_attachments: self.fragment_attachments,
                        }));
                    }
                    self.compute_count += 1;
                    return Ok(Some(ParsedHardwareCommand::Compute {
                        index: self.command_index,
                        header,
                        payload: UapiComputeCommand::parse(payload)?,
                        attachments: self.compute_attachments,
                    }));
                }
                SET_VERTEX_ATTACHMENTS | SET_FRAGMENT_ATTACHMENTS | SET_COMPUTE_ATTACHMENTS => {
                    if header.render_barrier != UAPI_BARRIER_NONE
                        || header.compute_barrier != UAPI_BARRIER_NONE
                    {
                        return Err(UapiTranslateError::AttachmentBarrier);
                    }
                    let attachments = UapiAttachmentList::parse(payload)?;
                    match header.command_type {
                        SET_VERTEX_ATTACHMENTS => self.vertex_attachments = attachments,
                        SET_FRAGMENT_ATTACHMENTS => self.fragment_attachments = attachments,
                        SET_COMPUTE_ATTACHMENTS => self.compute_attachments = attachments,
                        _ => unreachable!(),
                    }
                }
                _ => return Err(UapiTranslateError::CommandType),
            }
        }
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) enum GpuAccess {
    Read,
    Write,
    ReadWrite,
}

pub(crate) trait GpuAddressSpace {
    fn covers(&self, address: u64, size: u64, access: GpuAccess) -> bool;
}

fn require_range<A: GpuAddressSpace>(
    address_space: &A,
    address: u64,
    size: u64,
    access: GpuAccess,
) -> Result<(), UapiTranslateError> {
    let end = address
        .checked_add(size)
        .ok_or(UapiTranslateError::AddressRange)?;
    if size == 0 || end <= address || !address_space.covers(address, size, access) {
        return Err(UapiTranslateError::AddressRange);
    }
    Ok(())
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) struct QueueUscWindow {
    pub(crate) base: u64,
    pub(crate) user_start: u64,
    pub(crate) user_end: u64,
}

impl QueueUscWindow {
    pub(crate) fn validate(self) -> Result<(), UapiTranslateError> {
        if self.base & (USC_WINDOW_SIZE - 1) != 0 {
            return Err(UapiTranslateError::QueueUscAlignment);
        }
        let end = self
            .base
            .checked_add(USC_WINDOW_SIZE)
            .ok_or(UapiTranslateError::QueueUscRange)?;
        if self.base < self.user_start || end > self.user_end || end <= self.base {
            return Err(UapiTranslateError::QueueUscRange);
        }
        Ok(())
    }

    fn full_program<A: GpuAddressSpace>(
        self,
        address_space: &A,
        tagged_offset: u32,
    ) -> Result<u64, UapiTranslateError> {
        self.validate()?;
        if tagged_offset == 0 {
            return Err(UapiTranslateError::ProgramOffset);
        }
        let allocation = self
            .base
            .checked_add((tagged_offset & !7) as u64)
            .ok_or(UapiTranslateError::ProgramOffset)?;
        require_range(address_space, allocation, 4, GpuAccess::Read)?;
        self.base
            .checked_add(tagged_offset as u64)
            .ok_or(UapiTranslateError::ProgramOffset)
    }
}

fn validate_sampler<A: GpuAddressSpace>(
    address_space: &A,
    heap: u64,
    count: u32,
) -> Result<(), UapiTranslateError> {
    if count > MAX_SAMPLERS {
        return Err(UapiTranslateError::SamplerCount);
    }
    if (heap == 0) != (count == 0) {
        return Err(UapiTranslateError::SamplerPair);
    }
    if count != 0 {
        if heap & (SAMPLER_SIZE - 1) != 0 {
            return Err(UapiTranslateError::AddressAlignment);
        }
        require_range(
            address_space,
            heap,
            count as u64 * SAMPLER_SIZE,
            GpuAccess::Read,
        )?;
    }
    Ok(())
}

fn validate_attachments<A: GpuAddressSpace>(
    address_space: &A,
    attachments: &UapiAttachmentList,
) -> Result<(), UapiTranslateError> {
    for attachment in attachments.as_slice() {
        require_range(
            address_space,
            attachment.address,
            attachment.size,
            GpuAccess::Write,
        )?;
    }
    Ok(())
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) struct RenderInternalState {
    pub(crate) parameters: G17pRenderParameters,
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) struct TranslatedRenderCommand {
    pub(crate) flags: u32,
    pub(crate) vdm_base: u64,
    pub(crate) parameters: G17pRenderParameters,
    pub(crate) vertex_attachments: UapiAttachmentList,
    pub(crate) fragment_attachments: UapiAttachmentList,
    pub(crate) vertex_timestamps: UapiTimestamps,
    pub(crate) fragment_timestamps: UapiTimestamps,
}

fn validate_zls<A: GpuAddressSpace>(
    address_space: &A,
    value: UapiZlsBuffer,
    layers: u16,
) -> Result<(), UapiTranslateError> {
    if value.base == 0
        && (value.compression_base != 0 || value.stride != 0 || value.compression_stride != 0)
    {
        return Err(UapiTranslateError::ZlsFields);
    }
    if value.compression_base == 0 && value.compression_stride != 0 {
        return Err(UapiTranslateError::ZlsFields);
    }
    if layers > 1 && value.base != 0 && value.stride == 0 {
        return Err(UapiTranslateError::ZlsFields);
    }
    if value.stride != 0 && value.stride & 0x3fff != 1 {
        return Err(UapiTranslateError::ZlsFields);
    }
    if value.compression_stride & 0x3fff != 0 {
        return Err(UapiTranslateError::ZlsFields);
    }
    if value.base != 0 {
        let layer_stride = if value.stride == 0 {
            0
        } else {
            ((value.stride as u64 >> 14) + 1) * PAGE_SIZE
        };
        let size = layer_stride
            .checked_mul(layers.saturating_sub(1) as u64)
            .and_then(|value| value.checked_add(1))
            .ok_or(UapiTranslateError::AddressRange)?;
        require_range(address_space, value.base, size, GpuAccess::ReadWrite)?;
    }
    if value.compression_base != 0 {
        let layer_stride = ((value.compression_stride as u64 >> 14) + 1) * 0x80;
        let size = layer_stride
            .checked_mul(layers.saturating_sub(1) as u64)
            .and_then(|value| value.checked_add(1))
            .ok_or(UapiTranslateError::AddressRange)?;
        require_range(
            address_space,
            value.compression_base,
            size,
            GpuAccess::ReadWrite,
        )?;
    }
    Ok(())
}

pub(crate) fn translate_render_command<A: GpuAddressSpace>(
    command: UapiRenderCommand,
    vertex_attachments: UapiAttachmentList,
    fragment_attachments: UapiAttachmentList,
    queue: QueueUscWindow,
    address_space: &A,
    internal: RenderInternalState,
) -> Result<TranslatedRenderCommand, UapiTranslateError> {
    if command.flags & !RENDER_FLAGS != 0
        || command.flags & (RENDER_VERTEX_SCRATCH | RENDER_NO_VERTEX_CLUSTERING) != 0
    {
        return Err(UapiTranslateError::Flags);
    }
    if !(1..=16_384).contains(&command.width) || !(1..=16_384).contains(&command.height) {
        return Err(UapiTranslateError::Dimensions);
    }
    if !(1..=2048).contains(&command.layers) {
        return Err(UapiTranslateError::Layers);
    }
    if !matches!(
        (command.utile_width, command.utile_height),
        (32, 32) | (32, 16) | (16, 16)
    ) {
        return Err(UapiTranslateError::UtileDimensions);
    }
    let samples_log2 = match command.samples {
        1 => 0,
        2 => 1,
        4 => 2,
        _ => return Err(UapiTranslateError::Samples),
    };
    let utile_bytes = command.sample_size as u64
        * command.utile_width as u64
        * command.utile_height as u64
        * command.samples as u64;
    if utile_bytes > 32_768 {
        return Err(UapiTranslateError::TilebufferSize);
    }
    let blocks_per_utile = utile_bytes.div_ceil(2048);

    if command.vdm_base == 0 || command.vdm_base & 3 != 0 {
        return Err(UapiTranslateError::AddressAlignment);
    }
    require_range(address_space, command.vdm_base, 4, GpuAccess::Read)?;

    if command.scissor_base == 0 || command.scissor_base & 7 != 0 {
        return Err(UapiTranslateError::AddressAlignment);
    }
    require_range(address_space, command.scissor_base, 8, GpuAccess::Read)?;
    if command.depth_bias_base != 0 {
        if command.depth_bias_base & 7 != 0 {
            return Err(UapiTranslateError::AddressAlignment);
        }
        require_range(address_space, command.depth_bias_base, 8, GpuAccess::Read)?;
    }
    if command.occlusion_query_base != 0 {
        if command.occlusion_query_base & 7 != 0 {
            return Err(UapiTranslateError::AddressAlignment);
        }
        require_range(
            address_space,
            command.occlusion_query_base,
            8,
            GpuAccess::Write,
        )?;
    }
    validate_zls(address_space, command.depth, command.layers)?;
    validate_zls(address_space, command.stencil, command.layers)?;
    validate_sampler(
        address_space,
        command.sampler_heap,
        command.sampler_count as u32,
    )?;
    if command.vertex_helper
        != (UapiHelperProgram {
            binary: 0,
            config: 0,
            data: 0,
        })
        || command.fragment_helper
            != (UapiHelperProgram {
                binary: 0,
                config: 0,
                data: 0,
            })
    {
        return Err(UapiTranslateError::HelperProgram);
    }
    if command.stencil_clear & !0xff != 0 {
        return Err(UapiTranslateError::StencilClearBits);
    }

    let background = queue.full_program(address_space, command.background.usc)?;
    let end_of_tile = queue.full_program(address_space, command.end_of_tile.usc)?;
    let partial_background = queue.full_program(address_space, command.partial_background.usc)?;
    let partial_end_of_tile = queue.full_program(address_space, command.partial_end_of_tile.usc)?;
    validate_attachments(address_space, &vertex_attachments)?;
    validate_attachments(address_space, &fragment_attachments)?;

    let utile_config = ((command.utile_width as u64 / 16) << 12)
        | ((command.utile_height as u64 / 16) << 14)
        | samples_log2;
    let mut tile_config = 0x280;
    if command.layers > 1 {
        tile_config |= 1;
    }
    if command.flags & RENDER_PROCESS_EMPTY_TILES != 0 {
        tile_config |= 0x1_0000;
    }

    let mut parameters = internal.parameters;
    parameters.width = command.width as u32;
    parameters.height = command.height as u32;
    parameters.encoder = command.vdm_base;
    parameters.layers = command.layers;
    parameters.utile_width = command.utile_width;
    parameters.utile_height = command.utile_height;
    parameters.utile_config = utile_config;
    parameters.multisample_control = command.multisample_control;
    parameters.ppp_control = command.ppp_control as u64;
    parameters.tib_blocks = blocks_per_utile;
    parameters.tile_config = tile_config;
    parameters.depth_dimensions = command.depth_dimensions as u64;
    parameters.depth_buffer = command.depth.base;
    parameters.depth_aux_buffer = command.depth.compression_base;
    parameters.depth_stride = command.depth.stride as u64;
    parameters.depth_aux_stride = command.depth.compression_stride as u64;
    parameters.stencil_buffer = command.stencil.base;
    parameters.stencil_aux_buffer = command.stencil.compression_base;
    parameters.stencil_stride = command.stencil.stride as u64;
    parameters.stencil_aux_stride = command.stencil.compression_stride as u64;
    parameters.depth_flags = command.zls_control;
    parameters.occlusion_query_base = command.occlusion_query_base;
    parameters.scissor_array = command.scissor_base;
    parameters.depth_bias_array = command.depth_bias_base;
    parameters.sampler_array = command.sampler_heap;
    parameters.sampler_count = command.sampler_count as u32;
    parameters.process_empty_tiles = command.flags & RENDER_PROCESS_EMPTY_TILES != 0;
    parameters.merge_upper_x_bits = command.merge_upper_x;
    parameters.merge_upper_y_bits = command.merge_upper_y;
    parameters.depth_clear_value_bits = command.depth_clear;
    parameters.stencil_clear_value = command.stencil_clear;
    parameters.load_pipeline_bind = BG_RESOURCE_PREFIX | command.background.resource_spec as u64;
    parameters.load_pipeline = background;
    parameters.store_pipeline_bind = command.end_of_tile.resource_spec as u64;
    parameters.store_pipeline = end_of_tile;
    parameters.partial_load_pipeline_bind = command.partial_background.resource_spec as u64;
    parameters.partial_load_pipeline = partial_background;
    parameters.partial_store_pipeline_bind = command.partial_end_of_tile.resource_spec as u64;
    parameters.partial_store_pipeline = partial_end_of_tile;

    Ok(TranslatedRenderCommand {
        flags: command.flags,
        vdm_base: command.vdm_base,
        parameters,
        vertex_attachments,
        fragment_attachments,
        vertex_timestamps: command.vertex_timestamps,
        fragment_timestamps: command.fragment_timestamps,
    })
}

pub(crate) fn build_render_descriptors(
    command: &TranslatedRenderCommand,
    objects: RenderDescriptorObjects,
    tiling_metadata: RenderDescriptorMetadata,
    fragment_metadata: RenderDescriptorMetadata,
    tiling: &mut [u8],
    fragment: &mut [u8],
) -> Result<(), UapiTranslateError> {
    g17_render::build_ta_descriptor(&command.parameters, objects, tiling_metadata, tiling)?;
    g17_render::build_fragment_descriptor(
        &command.parameters,
        objects,
        fragment_metadata,
        fragment,
    )?;
    Ok(())
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) struct TranslatedComputeCommand {
    pub(crate) control_stream_base: u64,
    pub(crate) control_stream_end: u64,
    pub(crate) control_stream_terminator: u64,
    pub(crate) sampler_heap: u64,
    pub(crate) sampler_count: u32,
    pub(crate) attachments: UapiAttachmentList,
    pub(crate) timestamps: UapiTimestamps,
}

pub(crate) fn translate_compute_command<A: GpuAddressSpace>(
    command: UapiComputeCommand,
    attachments: UapiAttachmentList,
    queue: QueueUscWindow,
    address_space: &A,
) -> Result<TranslatedComputeCommand, UapiTranslateError> {
    queue.validate()?;
    if command.flags != 0 {
        return Err(UapiTranslateError::Flags);
    }
    if command.control_stream_base & 3 != 0
        || command.control_stream_end & 3 != 0
        || command.control_stream_end <= command.control_stream_base
    {
        return Err(UapiTranslateError::AddressAlignment);
    }
    require_range(
        address_space,
        command.control_stream_base,
        command.control_stream_end - command.control_stream_base,
        GpuAccess::Read,
    )?;
    validate_sampler(address_space, command.sampler_heap, command.sampler_count)?;
    if command.helper
        != (UapiHelperProgram {
            binary: 0,
            config: 0,
            data: 0,
        })
    {
        return Err(UapiTranslateError::HelperProgram);
    }
    validate_attachments(address_space, &attachments)?;
    Ok(TranslatedComputeCommand {
        control_stream_base: command.control_stream_base,
        control_stream_end: command.control_stream_end,
        control_stream_terminator: command.control_stream_end - 4,
        sampler_heap: command.sampler_heap,
        sampler_count: command.sampler_count,
        attachments,
        timestamps: command.timestamps,
    })
}

#[derive(Debug, Copy, Clone)]
pub(crate) struct ComputeEntryContext<'a> {
    pub(crate) descriptor_flag_4c: bool,
    pub(crate) descriptor_flag_5c8: bool,
    pub(crate) converted_command_timestamp: u64,
    pub(crate) barriers: &'a [G17PClBarrierDependency],
    pub(crate) mcache: Option<G17PClMcacheAperture>,
    pub(crate) payload: [u64; 2],
    pub(crate) event_mask: [u64; 4],
    pub(crate) rce_kind: u8,
    pub(crate) rce_bindings: [G17PClRceBinding; 4],
    pub(crate) auxiliary: u64,
}

impl TranslatedComputeCommand {
    pub(crate) const fn entry_operands<'a>(
        &self,
        context: ComputeEntryContext<'a>,
    ) -> G17PClKickEntryOperands<'a> {
        G17PClKickEntryOperands {
            descriptor_flag_4c: context.descriptor_flag_4c,
            descriptor_flag_5c8: context.descriptor_flag_5c8,
            converted_command_timestamp: context.converted_command_timestamp,
            barriers: context.barriers,
            mcache: context.mcache,
            payload: context.payload,
            event_mask: context.event_mask,
            rce_kind: context.rce_kind,
            rce_bindings: context.rce_bindings,
            auxiliary: context.auxiliary,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const fn nibble(value: u8) -> u8 {
        match value {
            b'0'..=b'9' => value - b'0',
            b'a'..=b'f' => value - b'a' + 10,
            _ => 0xff,
        }
    }

    const fn hex<const N: usize, const M: usize>(source: &[u8; M]) -> [u8; N] {
        let mut out = [0u8; N];
        let mut index = 0;
        while index < N {
            out[index] = (nibble(source[index * 2]) << 4) | nibble(source[index * 2 + 1]);
            index += 1;
        }
        out
    }

    const RENDER_FIXTURE: [u8; 312] = hex::<312, 624>(b"02001800ffffffff00000a00000000000001000000000000000000000000000003001800ffffffff00000b0000000000000200000000000000000000000000000000f000ffffffff020000000000000000000100000000000000000000000000000000000000000000000000000000000000000000000000000002000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000088000000000000000000000000000000020200004000300001000000202001040000000000000000040001004000000004000200080000000400010000000000040002000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000");
    const COMPUTE_FIXTURE: [u8; 104] = hex::<104, 208>(b"04001800ffffffff00000300000000000001000000000000000000000000000001004000ffffffff00000000000000000000010000000000400001000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000");

    fn fnv1a64(raw: &[u8]) -> u64 {
        let mut value = 0xcbf2_9ce4_8422_2325u64;
        for byte in raw {
            value ^= *byte as u64;
            value = value.wrapping_mul(0x0000_0100_0000_01b3);
        }
        value
    }

    struct TestAddressSpace;

    impl GpuAddressSpace for TestAddressSpace {
        fn covers(&self, address: u64, size: u64, _access: GpuAccess) -> bool {
            let end = match address.checked_add(size) {
                Some(value) => value,
                None => return false,
            };
            (0x1_0000..=0x20_0000).contains(&address) && end <= 0x20_0000
                || (0x11_0000_0000..0x12_0000_0000).contains(&address) && end <= 0x12_0000_0000
        }
    }

    fn window() -> QueueUscWindow {
        QueueUscWindow {
            base: 0x11_0000_0000,
            user_start: 0x4000,
            user_end: (1 << 39) - 0x8000,
        }
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
    fn render_fixture_translates_to_full_g17p_programs_and_descriptors() {
        assert_eq!(fnv1a64(&RENDER_FIXTURE), 0xf056_ea1d_ef06_32c5);
        let mut parser = UapiCommandParser::new(&RENDER_FIXTURE);
        let command = parser.next_hardware().unwrap().unwrap();
        let (payload, vertex, fragment) = match command {
            ParsedHardwareCommand::Render {
                index,
                payload,
                vertex_attachments,
                fragment_attachments,
                ..
            } => {
                assert_eq!(index, 3);
                (payload, vertex_attachments, fragment_attachments)
            }
            _ => panic!("wrong command kind"),
        };
        assert!(parser.next_hardware().unwrap().is_none());
        assert_eq!(parser.finish(), Ok(1));

        let mut internal = G17pRenderParameters::source_rsrc8();
        internal.context_base = 0;
        let translated = translate_render_command(
            payload,
            vertex,
            fragment,
            window(),
            &TestAddressSpace,
            RenderInternalState {
                parameters: internal,
            },
        )
        .unwrap();
        assert_eq!(translated.parameters.load_pipeline, 0x11_0001_0004);
        assert_eq!(translated.parameters.store_pipeline, 0x11_0002_0004);
        assert_eq!(
            translated.parameters.load_pipeline_bind,
            0x0007_8000_0000_0040
        );
        assert_eq!(translated.parameters.store_pipeline_bind, 0x8);
        assert_eq!(translated.parameters.partial_load_pipeline, 0x11_0001_0004);
        assert_eq!(translated.parameters.partial_store_pipeline, 0x11_0002_0004);
        assert_eq!(translated.parameters.utile_config, 0xa000);
        assert_eq!(translated.parameters.tib_blocks, 2);
        assert_eq!(translated.parameters.tile_config, 0x1_0280);
        assert_eq!(translated.parameters.encoder, 0x1_0000);
        assert_eq!(
            translated.vertex_attachments.as_slice(),
            &[UapiAttachment {
                address: 0xa_0000,
                size: 0x100,
            }]
        );
        assert_eq!(
            translated.fragment_attachments.as_slice(),
            &[UapiAttachment {
                address: 0xb_0000,
                size: 0x200,
            }]
        );

        let mut tiling = [0u8; g17_render::TA_DESCRIPTOR_SIZE];
        let mut fragment_raw = [0u8; g17_render::FRAGMENT_DESCRIPTOR_SIZE];
        build_render_descriptors(
            &translated,
            objects(),
            RenderDescriptorMetadata::source(g17_render::RenderDescriptorKind::Tiling),
            RenderDescriptorMetadata::source(g17_render::RenderDescriptorKind::Fragment),
            &mut tiling,
            &mut fragment_raw,
        )
        .unwrap();
        assert_eq!(fnv1a64(&tiling), 0x8581_5830_2d5b_e227);
        assert_eq!(fnv1a64(&fragment_raw), 0x6688_e9f8_d061_d776);
    }

    #[test]
    fn compute_fixture_translates_to_existing_cl_operands() {
        assert_eq!(fnv1a64(&COMPUTE_FIXTURE), 0xeb42_2636_cc6a_c274);
        let mut parser = UapiCommandParser::new(&COMPUTE_FIXTURE);
        let (payload, attachments) = match parser.next_hardware().unwrap().unwrap() {
            ParsedHardwareCommand::Compute {
                index,
                payload,
                attachments,
                ..
            } => {
                assert_eq!(index, 2);
                (payload, attachments)
            }
            _ => panic!("wrong command kind"),
        };
        assert!(parser.next_hardware().unwrap().is_none());
        assert_eq!(parser.finish(), Ok(1));
        let translated =
            translate_compute_command(payload, attachments, window(), &TestAddressSpace).unwrap();
        assert_eq!(translated.control_stream_base, 0x1_0000);
        assert_eq!(translated.control_stream_end, 0x1_0040);
        assert_eq!(translated.control_stream_terminator, 0x1_003c);
        assert_eq!(
            translated.attachments.as_slice(),
            &[UapiAttachment {
                address: 0x3_0000,
                size: 0x100,
            }]
        );

        let empty = G17PClRceBinding { address: 0, tag: 0 };
        let operands = translated.entry_operands(ComputeEntryContext {
            descriptor_flag_4c: false,
            descriptor_flag_5c8: false,
            converted_command_timestamp: 1,
            barriers: &[],
            mcache: None,
            payload: [0xffff_fc20_c001_a000, 0xffff_fc20_c000_0480],
            event_mask: [1, 2, 3, 4],
            rce_kind: 0,
            rce_bindings: [empty; 4],
            auxiliary: 0x003f_ffff_ffff_ffff,
        });
        assert_eq!(
            operands.payload,
            [0xffff_fc20_c001_a000, 0xffff_fc20_c000_0480]
        );
        assert_eq!(operands.event_mask, [1, 2, 3, 4]);
        assert_eq!(operands.auxiliary, 0x003f_ffff_ffff_ffff);
        let entry = g17_submission::prepare_g17p_cl_kick_entry(
            g17_submission::G17PClKickProducerState {
                queue_id: 4,
                num_stamps: 4,
                entry_stride: 0x200,
                completion_selector: 2,
                qos: g17_submission::G17PClQosConfig {
                    queue_byte_32: 0,
                    class_40: 0,
                    word_48: 0,
                },
                submitted: 0,
                current_timestamp: 0,
                base_timestamp: 0,
                previous_valid: false,
                previous_timestamp: 0,
            },
            operands,
        )
        .unwrap();
        assert_eq!(fnv1a64(&entry.entry), 0xbcc4_342d_108e_f9d0);
    }

    #[test]
    fn parser_zero_extends_old_payloads_and_checks_new_tail_bytes() {
        let mut command = [0u8; UAPI_COMMAND_HEADER_SIZE + 8];
        command[0..2].copy_from_slice(&COMMAND_COMPUTE.to_le_bytes());
        command[2..4].copy_from_slice(&8u16.to_le_bytes());
        command[4..6].copy_from_slice(&UAPI_BARRIER_NONE.to_le_bytes());
        command[6..8].copy_from_slice(&UAPI_BARRIER_NONE.to_le_bytes());
        let mut parser = UapiCommandParser::new(&command);
        match parser.next_hardware().unwrap().unwrap() {
            ParsedHardwareCommand::Compute { payload, .. } => {
                assert_eq!(
                    payload,
                    UapiComputeCommand {
                        flags: 0,
                        sampler_count: 0,
                        control_stream_base: 0,
                        control_stream_end: 0,
                        sampler_heap: 0,
                        helper: UapiHelperProgram {
                            binary: 0,
                            config: 0,
                            data: 0,
                        },
                        timestamps: UapiTimestamps {
                            start: UapiTimestamp {
                                handle: 0,
                                offset: 0,
                            },
                            end: UapiTimestamp {
                                handle: 0,
                                offset: 0,
                            },
                        },
                    }
                );
            }
            _ => panic!("wrong command kind"),
        }

        let mut extended = [0u8; UAPI_COMPUTE_SIZE + 1];
        extended[UAPI_COMPUTE_SIZE] = 1;
        assert_eq!(
            UapiComputeCommand::parse(&extended),
            Err(UapiTranslateError::PayloadTail)
        );
    }

    #[test]
    fn translation_checks_sampler_program_and_attachment_ranges() {
        let mut parser = UapiCommandParser::new(&RENDER_FIXTURE);
        let (mut payload, vertex, fragment) = match parser.next_hardware().unwrap().unwrap() {
            ParsedHardwareCommand::Render {
                payload,
                vertex_attachments,
                fragment_attachments,
                ..
            } => (payload, vertex_attachments, fragment_attachments),
            _ => panic!("wrong command kind"),
        };
        payload.sampler_count = 1;
        assert_eq!(
            translate_render_command(
                payload,
                vertex,
                fragment,
                window(),
                &TestAddressSpace,
                RenderInternalState {
                    parameters: G17pRenderParameters::source_rsrc8(),
                },
            ),
            Err(UapiTranslateError::SamplerPair)
        );
        payload.sampler_count = 0;
        payload.end_of_tile.usc = 0;
        assert_eq!(
            translate_render_command(
                payload,
                vertex,
                fragment,
                window(),
                &TestAddressSpace,
                RenderInternalState {
                    parameters: G17pRenderParameters::source_rsrc8(),
                },
            ),
            Err(UapiTranslateError::ProgramOffset)
        );
    }
}
