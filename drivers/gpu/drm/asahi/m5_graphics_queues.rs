// SPDX-License-Identifier: GPL-2.0-only OR MIT
//! Paired first-submission queues for the retained M5 graphics experiment.

use super::{graphics_job::{M5GraphicsJob, PB_HARDWARE_ID, QUEUE_IDS}, graphics_usc, MappedObject};
use crate::{driver::AsahiDevice, g17_submission, m5_channel, m5_sksm, mmu};
use kernel::prelude::*;

const HIGH: u64 = 0xffff_fc20_c000_0000;
const ENTRY_HIGH: u64 = 0xffff_fc20_0039_0000;
const ENTRY_LOW: u64 = 0x70_005f_0000;
const ENTRY_BYTES: usize = 0x20000;
const CONTROL: usize = 0x18000;
const POINTERS: [usize; 2] = [0x4000, 0xc000];
const ITEMS: [usize; 2] = [0x8000, 0x10000];
const CONFIG: [usize; 2] = [0xa800, 0x12800];

pub(super) struct M5GraphicsQueues {
    aliases: KVec<mmu::KernelMapping>,
    entries: KVec<MappedObject>,
    graph: MappedObject,
}

impl M5GraphicsQueues {
    pub(super) fn new(dev: &AsahiDevice, uat: &mmu::Uat, job: &M5GraphicsJob) -> Result<Self> {
        let mut graph = MappedObject::new_wc_at(dev, uat.kernel_vm(), HIGH,
            0x20000, mmu::PROT_FW_PRIV_RW)?;
        let mut entries = KVec::new();
        let mut aliases = KVec::new();
        for stage in 0..2 {
            let id = QUEUE_IDS[stage];
            let high = ENTRY_HIGH + (stage * ENTRY_BYTES) as u64;
            let low = ENTRY_LOW + (stage * ENTRY_BYTES) as u64;
            let mut entry = MappedObject::new_wc_at(dev, uat.kernel_vm(), high,
                ENTRY_BYTES, mmu::PROT_FW_PRIV_RW)?;
            let alias = entry.map_alias_at(uat.kernel_lower_vm(), low, mmu::PROT_GPU_SHARED_RW)?;
            for offset in (0..ENTRY_BYTES).step_by(mmu::UAT_PGSZ) {
                if uat.kernel_vm().translate_iova(high + offset as u64)?
                    != uat.kernel_lower_vm().translate_iova(low + offset as u64)? {
                    return Err(EFAULT);
                }
            }
            let [descriptor, descriptor_low] = job.descriptor_addresses(stage)?;
            let dm = if stage == 0 { m5_sksm::M5DataMaster::Tiling }
                     else { m5_sksm::M5DataMaster::Fragment };
            let barrier = [m5_sksm::M5SksmDependency { queue_id: QUEUE_IDS[0], stamp: 1, kind: 1 }];
            let mut rce = [m5_sksm::M5SksmRce { address: 0, tag: 0 }; 4];
            if stage == 0 {
                rce[0] = m5_sksm::M5SksmRce { address: descriptor_low + 0x60, tag: 79 };
            } else {
                for (index, (offset, count)) in [(0xa0, 87), (0x7c0, 18),
                    (0xee0, 25), (0x1600, 12)].into_iter().enumerate() {
                    rce[index] = m5_sksm::M5SksmRce { address: descriptor_low + offset, tag: count };
                }
            }
            let prepared = m5_sksm::prepare_m5_sksm_entry(m5_sksm::M5SksmQueue {
                id, priority: 2, current_stamp: 1, submitted: 0, previous_stamp: None,
            }, m5_sksm::M5SksmOperands {
                barriers: if stage == 0 { &[] } else { &barrier }, first_predecessor: 0,
                implicit_dependency_type: 1, mcache: None, flag_bit1: false,
                flag_bit61: false, flag_bit63: false,
                payload: [descriptor, Self::queue_address(stage)?],
                qos_byte_32: 3, qos_class_40: if stage == 0 { 12 } else { 24 },
                qos_word_48: 0xffff, event_mask: [0; 4], rce_kind: 0, rce,
                auxiliary: 0x3f_ffff_ffff_ffff,
            }).map_err(|_| EINVAL)?;
            entry.with_bytes_mut(|out| {
                out[prepared.offset as usize..prepared.offset as usize + prepared.bytes.len()]
                    .copy_from_slice(&prepared.bytes);
                Ok(())
            })?;
            let config = m5_channel::encode_m5_channel_config_for(m5_channel::M5ChannelConfig {
                ring_low: low, ring_high: high, queue_id: id, priority: 2,
                install_queue: true, gpu_context: 3,
                event: Some(m5_channel::M5ChannelEvent { address: job.usc_control(),
                    timestamp: 1, index: graphics_usc::HARDWARE_ID }),
                channel_control: HIGH + CONTROL as u64, update_scheduler: true,
                scheduler_index: 0, channel_stamp_index: stage as u32,
                update_context: true, owner_context: 1, extended_command: false,
            }, stage as u32, 2, Some(0), if stage == 0 {
                Some(m5_channel::M5ParameterBufferEvent { address: job.parameter_state(),
                    timestamp: 1, hardware_buffer_id: PB_HARDWARE_ID, manager_id: 0 })
            } else { None }).map_err(|_| EINVAL)?;
            let kick = m5_sksm::encode_m5_sksm_add_kicks_for(dm, id, 2, 1, 1)
                .map_err(|_| EINVAL)?;
            graph.with_bytes_mut(|out| {
                let pointers = POINTERS[stage];
                let list = pointers + 0x100;
                let queue = id as usize * 0xc0;
                let reset = g17_submission::encode_m5_25g76_channel_state_reset(
                    g17_submission::M5ChannelStateResetValues {
                        uncached_state: HIGH + pointers as u64,
                        command_pointer_ring: HIGH + ITEMS[stage] as u64,
                        gpu_va_10: HIGH + list as u64, word_48: 0,
                        gpu_va_9c: HIGH + CONTROL as u64,
                    });
                out[queue..queue + 0xc0].copy_from_slice(&reset);
                for offset in [0x28, 0x2c, 0x40] { put32(out, queue + offset, 2); }
                out[queue + 0x36..queue + 0x38].fill(0xff);
                put32(out, pointers + 0x50, u32::MAX);
                put32(out, pointers + 0x60, 0x500);
                put64(out, list + 8, HIGH + list as u64);
                out[CONFIG[stage]..CONFIG[stage] + config.len()].copy_from_slice(&config);
                out[CONFIG[stage] + 0x100..CONFIG[stage] + 0x100 + kick.len()].copy_from_slice(&kick);
                for (index, address) in [descriptor, HIGH + CONFIG[stage] as u64,
                    HIGH + CONFIG[stage] as u64 + 0x100].into_iter().enumerate() {
                    put64(out, ITEMS[stage] + index * 8, address);
                }
                put32(out, pointers + 0x40, 3);
                Ok(())
            })?;
            aliases.push(alias, GFP_KERNEL)?;
            entries.push(entry, GFP_KERNEL)?;
        }
        graph.with_bytes_mut(|out| {
            out[CONTROL..CONTROL + 2].fill(0xff);
            out[CONTROL + 5] = 1;
            out[CONTROL + 0x26] = 2;
            out[CONTROL + 0x33] = 0xff;
            Ok(())
        })?;
        Ok(Self { aliases, entries, graph })
    }

    pub(super) fn queue_address(stage: usize) -> Result<u64> {
        Ok(HIGH + *QUEUE_IDS.get(stage).ok_or(EINVAL)? as u64 * 0xc0)
    }

    pub(super) fn counters(&mut self) -> Result<[[u32; 3]; 2]> {
        let mut result = [[0; 3]; 2];
        for stage in 0..2 {
            for (index, offset) in [0, 0x30, 0x40].into_iter().enumerate() {
                result[stage][index] = self.graph.read_u32(HIGH + (POINTERS[stage] + offset) as u64)?;
            }
        }
        Ok(result)
    }
}

fn put32(out: &mut [u8], offset: usize, value: u32) {
    out[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}
fn put64(out: &mut [u8], offset: usize, value: u64) {
    out[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}
