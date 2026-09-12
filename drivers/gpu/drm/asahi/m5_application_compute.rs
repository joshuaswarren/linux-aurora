// SPDX-License-Identifier: GPL-2.0-only OR MIT
//! One application compute dispatch sharing the retained graphics VM and USC.
//!
//! Queue 12 is separate from TA/fragment queues 10/11. All publication and
//! completion gates are explicit; constructing this owner does not submit it.

use super::{graphics_job::M5GraphicsJob, graphics_usc,
    queue_probe::FirmwareDataView, MappedObject};
use crate::{driver::AsahiDevice, g17_submission, g17_uapi,
    m5_channel, m5_compute, m5_sksm, mmu};
use kernel::prelude::*;

pub(super) const QUEUE_ID: u8 = 12;
const GRAPH: u64 = 0xffff_fc20_c008_0000;
const POINTERS: usize = 0x4000;
const ITEMS: usize = 0x8000;
const CONFIG: usize = 0xa000;
const CONTROL: usize = 0xc000;
const LIST: u64 = GRAPH + 0x10000;
const ENTRY: u64 = 0xffff_fc20_003d_0000;
const ENTRY_LOW: u64 = 0x70_0063_0000;
const DESC: u64 = 0xffff_fc20_c040_0000;
const DESC_LOW: u64 = 0x70_0038_0000;
const SCHEDULER: u64 = DESC + 0x4000;
const ZERO: u64 = DESC + 0x8000;
const SIGNALS: u64 = DESC + 0x10000;

pub(super) struct M5ApplicationCompute {
    aliases: KVec<mmu::KernelMapping>,
    descriptor: MappedObject,
    graph: MappedObject,
    entry: MappedObject,
    list: MappedObject,
    signals: MappedObject,
    storage: MappedObject,
    firmware: FirmwareDataView,
}

impl M5ApplicationCompute {
    pub(super) fn new(dev: &AsahiDevice, uat: &mmu::Uat, job: &M5GraphicsJob,
        command: &g17_uapi::TranslatedComputeCommand) -> Result<Self> {
        // The adapter excludes helpers, timestamps, legacy sampler registers
        // and the Neo add3 proof ABI before allocating or publishing anything.
        if command.sampler_heap != 0 || command.sampler_count != 0
            || command.g17p_add3_buffers.is_some() { return Err(ENOTSUPP); }
        // Validate observation support and vacancy before publishing a queue.
        let firmware = FirmwareDataView::new(dev)?;
        if firmware.queue_registration(QUEUE_ID)? != ([0; 5], 0) { return Err(EBUSY); }
        // 128 KiB operands, a preemption page and a zero robustness page.
        // Their addresses come from the application's reserved driver arena.
        let storage = job.allocate_compute_storage(dev, 0x28000)?;
        let mut descriptor = MappedObject::new_wc_at(dev, uat.kernel_vm(), DESC,
            0x10000, mmu::PROT_FW_PRIV_RW)?;
        let mut entry = MappedObject::new_wc_at(dev, uat.kernel_vm(), ENTRY,
            0x20000, mmu::PROT_FW_PRIV_RW)?;
        let mut aliases = KVec::new();
        for (object, low, size) in [(&mut descriptor, DESC_LOW, 0x10000),
            (&mut entry, ENTRY_LOW, 0x20000)] {
            aliases.push(object.map_alias_at(uat.kernel_lower_vm(), low,
                mmu::PROT_GPU_SHARED_RW)?, GFP_KERNEL)?;
            for offset in (0..size).step_by(mmu::UAT_PGSZ) {
                if uat.kernel_vm().translate_iova(object.iova() + offset as u64)?
                    != uat.kernel_lower_vm().translate_iova(low + offset as u64)? {
                    return Err(EFAULT);
                }
            }
        }
        let mut signals = MappedObject::new_wc_at(dev, uat.kernel_vm(), SIGNALS,
            0x4000, mmu::PROT_FW_SHARED_RW)?;
        signals.write_u32(SIGNALS, 1)?;
        descriptor.with_bytes_mut(|out| {
            put64(out, 0x4000, SIGNALS);
            put32(out, 0x4010, 0x50);
            m5_compute::encode_m5_compute_descriptor(m5_compute::M5ComputeDescriptor {
                registers: m5_compute::M5ComputeParameters {
                    preempt_base: storage.iova() + 0x20000,
                    cdm_base: command.control_stream_base,
                    dispatch_identity: 0x0000_0001_0000_0001,
                    context_id: job.context_id(), work_ordinal: 0,
                    robustness: storage.iova() + 0x24000,
                    operand_state_base: storage.iova(),
                    private_pool_hw_id: u64::from(graphics_usc::HARDWARE_ID),
                    descriptor_control_530: 0x54b, descriptor_control_538: 0xf5,
                    descriptor_flag_60a: false, descriptor_flag_60b: false,
                },
                submit_sequence: 0, scheduler_record: SCHEDULER,
                descriptor_low_alias: DESC_LOW,
                cdm_terminator: command.control_stream_terminator,
                dispatch_stamps: [SIGNALS + 0x40, SIGNALS + 0x80],
                event_machine_stamp: 0x100, queue_id: u32::from(QUEUE_ID),
                queue_ordinal: 0, has_event: false, submission_index: 1,
                status: [SIGNALS + 0xc0, SIGNALS + 0xc8],
                shared_control: job.usc_control(), shared_control_enabled: true,
                event_metadata: [0; 16], zero_page: ZERO, owner_id: 1,
                descriptor_work_ordinal: 0,
            }, &mut out[..0x1000]).map_err(|_| EINVAL)
        })?;
        let mut graph = MappedObject::new_wc_at(dev, uat.kernel_vm(), GRAPH,
            0x10000, mmu::PROT_FW_PRIV_RW)?;
        let mut list = MappedObject::new_wc_at(dev, uat.kernel_vm(), LIST,
            0x4000, mmu::PROT_FW_SHARED_RW)?;
        list.with_bytes_mut(|out| { put64(out, 8, LIST); Ok(()) })?;
        let prepared = m5_sksm::prepare_m5_sksm_entry(m5_sksm::M5SksmQueue {
            id: QUEUE_ID, priority: 2, current_stamp: 1, submitted: 0, previous_stamp: None,
        }, m5_sksm::M5SksmOperands {
            barriers: &[], first_predecessor: 0, implicit_dependency_type: 1,
            mcache: None, flag_bit1: false, flag_bit61: false, flag_bit63: false,
            payload: [DESC, Self::queue_address()], qos_byte_32: 3, qos_class_40: 8,
            qos_word_48: 0xffff, event_mask: [0; 4], rce_kind: 0,
            rce: [m5_sksm::M5SksmRce { address: DESC_LOW + 0x40, tag: 37 },
                m5_sksm::M5SksmRce { address: DESC_LOW + 0x760, tag: 4 },
                m5_sksm::M5SksmRce { address: 0, tag: 0 },
                m5_sksm::M5SksmRce { address: 0, tag: 0 }],
            auxiliary: 0x3f_ffff_ffff_ffff,
        }).map_err(|_| EINVAL)?;
        entry.with_bytes_mut(|out| {
            let offset = prepared.offset as usize;
            out[offset..offset + prepared.bytes.len()].copy_from_slice(&prepared.bytes);
            Ok(())
        })?;
        let configuration = |install| m5_channel::encode_m5_channel_config(
            m5_channel::M5ChannelConfig {
                ring_low: ENTRY_LOW, ring_high: ENTRY, queue_id: QUEUE_ID, priority: 2,
                install_queue: install, gpu_context: job.context_id(),
                event: if install { None } else { Some(m5_channel::M5ChannelEvent {
                    address: job.usc_control(), timestamp: 1, index: graphics_usc::HARDWARE_ID,
                }) },
                channel_control: GRAPH + CONTROL as u64, update_scheduler: install,
                scheduler_index: 0, channel_stamp_index: 0,
                update_context: !install, owner_context: if install { 0 } else { 1 },
                extended_command: false,
            }).map_err(|_| EINVAL);
        let install = configuration(true)?;
        let config = configuration(false)?;
        let kick = m5_sksm::encode_m5_sksm_add_kicks(QUEUE_ID, 2, 1, 1)
            .map_err(|_| EINVAL)?;
        graph.with_bytes_mut(|out| {
            let reset = g17_submission::encode_m5_25g76_channel_state_reset(
                g17_submission::M5ChannelStateResetValues {
                    uncached_state: GRAPH + POINTERS as u64,
                    command_pointer_ring: GRAPH + ITEMS as u64, gpu_va_10: LIST,
                    word_48: 0, gpu_va_9c: GRAPH + CONTROL as u64,
                });
            let queue = usize::from(QUEUE_ID) * 0xc0;
            out[queue..queue + 0xc0].copy_from_slice(&reset);
            for offset in [0x28, 0x2c, 0x40] { put32(out, queue + offset, 2); }
            out[queue + 0x36..queue + 0x38].fill(0xff);
            put32(out, POINTERS + 0x50, u32::MAX);
            put32(out, POINTERS + 0x60, 0x500);
            out[CONTROL..CONTROL + 2].fill(0xff);
            out[CONTROL + 5] = 1;
            out[CONTROL + 0x26] = 2;
            out[CONTROL + 0x33] = 0xff;
            for (offset, bytes) in [(CONFIG, &install[..]), (CONFIG + 0x100, &config[..]),
                (CONFIG + 0x200, &kick[..])] {
                out[offset..offset + bytes.len()].copy_from_slice(bytes);
            }
            for (index, address) in [GRAPH + CONFIG as u64, DESC,
                GRAPH + CONFIG as u64 + 0x100, GRAPH + CONFIG as u64 + 0x200]
                .into_iter().enumerate() { put64(out, ITEMS + index * 8, address); }
            put32(out, POINTERS + 0x40, 1);
            Ok(())
        })?;
        Ok(Self { aliases, descriptor, graph, entry, list, signals, storage, firmware })
    }

    pub(super) fn queue_address() -> u64 { GRAPH + QUEUE_ID as u64 * 0xc0 }

    pub(super) fn counters(&mut self) -> Result<[u32; 3]> {
        let mut out = [0; 3];
        for (index, offset) in [0, 0x30, 0x40].into_iter().enumerate() {
            out[index] = self.graph.read_u32(GRAPH + (POINTERS + offset) as u64)?;
        }
        Ok(out)
    }

    pub(super) fn installed(&mut self) -> Result<bool> {
        let (slot, scheduler) = self.firmware.queue_registration(QUEUE_ID)?;
        let header = self.graph.read_u32(GRAPH + CONTROL as u64)?;
        let bitmap = self.graph.read_u64(GRAPH + CONTROL as u64 + 0x12)?;
        Ok(slot[0] & 1 != 0 && slot[1] == ENTRY && slot[2] == ENTRY_LOW
            && slot[3] == 4u64 << 32 && slot[4] as u32 == 2
            && scheduler == GRAPH + CONTROL as u64
            && header & 0xff != 0xff && (header >> 8) & 0xff != 0xff
            && (header >> 16) & 0xff == 4 && bitmap == 1u64 << QUEUE_ID)
    }

    pub(super) fn publish_dispatch(&mut self) -> Result {
        if self.counters()? != [1; 3] || !self.installed()? { return Err(EBUSY); }
        self.graph.write_u32(GRAPH + POINTERS as u64 + 0x40, 4)
    }

    pub(super) fn completion(&mut self, signalled: u128)
        -> Result<crate::m5_completion::M5ComputeCompletion> {
        Ok(crate::m5_completion::M5ComputeCompletion {
            submitted: self.signals.read_u32(SIGNALS)?,
            completed: self.descriptor.read_u32(SCHEDULER + 0xc)?,
            linked: self.descriptor.read_u32(SCHEDULER + 0x94)?,
            list: [self.list.read_u64(LIST)?, self.list.read_u64(LIST + 8)?],
            stamp: self.signals.read_u32(SIGNALS + 0x40)?, signalled,
        })
    }

    pub(super) fn complete(&mut self, signalled: u128) -> Result<bool> {
        Ok(self.counters()? == [4; 3]
            && self.completion(signalled)?.complete_at(LIST, QUEUE_ID, 0x100))
    }
}

fn put32(out: &mut [u8], offset: usize, value: u32) {
    out[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}
fn put64(out: &mut [u8], offset: usize, value: u64) {
    out[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}
