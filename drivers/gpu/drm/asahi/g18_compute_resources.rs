// SPDX-License-Identifier: GPL-2.0-only OR MIT
//! Retained G18 channel working memory, one command, and completion storage.
//!
//! This owner never publishes a ring entry. All mappings remain retained by
//! G18FirmwareMemory until both firmware processors are stopped. Shader and
//! argument buffers, native device policy, and process context installation
//! remain separate obligations of the eventual submission path.

#![allow(dead_code)]

use super::{g18_scheduler::G18SchedulerBinding, MappedObject};
use crate::{driver::AsahiDevice, g18_compute::*, g18_compute_memory::*, mmu};
use core::ops::Range;
use core::sync::atomic::{fence, Ordering};
use kernel::prelude::*;

// Host-selected range, after the existing UMA directory/block list, before
// the fixed graph. The VM allocator checks overlap with every existing owner.
const LOW_RANGE: Range<u64> = 0x70_0800_0000..0x70_1000_0000;
const SUPPORT_BYTES: usize = 0x100;
const STAMP_OFFSETS: [u64; 2] = [0, 0x40];
const TIME_OFFSETS: [u64; 2] = [0x80, 0xc0];

#[derive(Debug, Copy, Clone)]
pub(crate) struct G18PreparedComputeCommand {
    pub(crate) firmware_address: u64,
    pub(crate) gpu_address: u64,
    pub(crate) expected_stamp: u32,
    pub(crate) scheduler_record: u64,
    pub(crate) registers: G18ComputeRegisterSummary,
}

#[derive(Debug, Copy, Clone)]
pub(crate) struct G18ComputeCompletionSnapshot {
    pub(crate) stamps: [u32; 2],
    pub(crate) timestamps: [u64; 2],
}

pub(crate) struct G18ComputeMemory {
    // Alias owns the same GEM object and is released before the lower mapping.
    command_high: mmu::KernelMapping,
    command: MappedObject,
    channel: MappedObject,
    support: MappedObject,
    layout: G18ComputeChannelLayout,
    pointers: G18ComputeChannelPointers,
    prepared: bool,
}

impl G18ComputeMemory {
    pub(crate) fn new(
        dev: &AsahiDevice,
        uat: &mmu::Uat,
        high_range: Range<u64>,
        clusters: u32,
        width: u32,
    ) -> Result<Self> {
        if !uat.is_t8152_geometry() || mmu::UAT_PGSZ != 0x4000 {
            return Err(ENODEV);
        }
        let layout = G18ComputeChannelLayout::new(clusters, width).ok_or(EINVAL)?;
        let size: usize = layout.allocation_bytes.try_into()?;
        let channel = MappedObject::new_wc_in_range(
            dev,
            uat.kernel_lower_vm(),
            LOW_RANGE,
            size,
            mmu::UAT_PGSZ as u64,
            mmu::PROT_GPU_FW_SHARED_RW,
        )?;
        let pointers = layout.bind(channel.iova()).ok_or(ERANGE)?;
        for offset in (0..size).step_by(mmu::UAT_PGSZ) {
            uat.kernel_lower_vm()
                .translate_iova(channel.iova() + offset as u64)?;
        }
        let mut command = MappedObject::new_wc_in_range(
            dev,
            uat.kernel_lower_vm(),
            LOW_RANGE,
            G18_COMPUTE_COMMAND_BYTES,
            mmu::UAT_PGSZ as u64,
            mmu::PROT_GPU_FW_SHARED_RW,
        )?;
        let command_high = command.map_alias_in_range(
            uat.kernel_vm(),
            high_range.clone(),
            mmu::UAT_PGSZ as u64,
            mmu::PROT_FW_SHARED_RW,
        )?;
        for offset in (0..G18_COMPUTE_COMMAND_BYTES).step_by(mmu::UAT_PGSZ) {
            if uat
                .kernel_lower_vm()
                .translate_iova(command.iova() + offset as u64)?
                != uat
                    .kernel_vm()
                    .translate_iova(command_high.iova() + offset as u64)?
            {
                return Err(EFAULT);
            }
        }
        let support = MappedObject::new_wc_in_range(
            dev,
            uat.kernel_vm(),
            high_range,
            SUPPORT_BYTES,
            mmu::UAT_PGSZ as u64,
            mmu::PROT_FW_SHARED_RW,
        )?;
        uat.kernel_vm().translate_iova(support.iova())?;
        fence(Ordering::SeqCst);
        dev_info!(dev.as_ref(),
            "G18 compute memory: channel={:#x}+{:#x} operands={:#x}/{:#x} control={:#x} tail={:#x} command={:#x}/{:#x} support={:#x}, retained unpublished\n",
            channel.iova(), layout.allocation_bytes, pointers.operands[0], pointers.operands[1],
            pointers.control, pointers.tail, command.iova(), command_high.iova(), support.iova());
        Ok(Self {
            command_high,
            command,
            channel,
            support,
            layout,
            pointers,
            prepared: false,
        })
    }

    /// Encode one unpublished command using this owner's addresses. The lower
    /// mappings currently belong to context 0, so a process context cannot be
    /// selected until its mappings have an explicit retained owner as well.
    /// Caller must supply owned shader/argument bindings and native policy.
    pub(crate) fn prepare(
        &mut self,
        scheduler: G18SchedulerBinding,
        inputs: &G18ComputeCommandInputs,
        registers: &G18ComputeRegisterInputs,
    ) -> Result<G18PreparedComputeCommand> {
        if self.prepared {
            return Err(EBUSY);
        }
        if registers.clusters != self.layout.clusters
            || registers.cluster_width != self.layout.width
            || registers.context_id != 0
            || inputs.channel_ordinal >= 4
            || inputs.completion.stamp == 0
            || scheduler.record == 0
        {
            return Err(EINVAL);
        }
        let inputs = G18ComputeCommandInputs {
            d198: scheduler.record,
            stamp_addresses: STAMP_OFFSETS.map(|offset| self.support.iova() + offset),
            d1b0: self.support.iova() + TIME_OFFSETS[0],
            d1b8: self.support.iova() + TIME_OFFSETS[1],
            ..*inputs
        };
        let registers = G18ComputeRegisterInputs {
            command_gpu: self.command.iova(),
            operands: self.pointers.operands,
            support_control: self.pointers.control,
            ..*registers
        };
        let summary = self.command.with_bytes_mut(|out| {
            let out = out.try_into().map_err(|_| EINVAL)?;
            encode_g18_compute_command(out, &inputs, &registers).map_err(|_| EINVAL)
        })?;
        fence(Ordering::SeqCst);
        self.prepared = true;
        Ok(G18PreparedComputeCommand {
            firmware_address: self.command_high.iova(),
            gpu_address: self.command.iova(),
            expected_stamp: inputs.completion.stamp,
            scheduler_record: scheduler.record,
            registers: summary,
        })
    }

    /// These independent WC reads are diagnostic, not an atomic completion
    /// fence. Shader execution still requires verified output data and a clean
    /// firmware/fault observation, even if both stamp words change.
    pub(crate) fn completion_snapshot(&mut self) -> Result<G18ComputeCompletionSnapshot> {
        if !self.prepared {
            return Err(EINVAL);
        }
        fence(Ordering::SeqCst);
        let base = self.support.iova();
        Ok(G18ComputeCompletionSnapshot {
            stamps: [
                self.support.read_u32(base + STAMP_OFFSETS[0])?,
                self.support.read_u32(base + STAMP_OFFSETS[1])?,
            ],
            timestamps: [
                self.support.read_u64(base + TIME_OFFSETS[0])?,
                self.support.read_u64(base + TIME_OFFSETS[1])?,
            ],
        })
    }
}
