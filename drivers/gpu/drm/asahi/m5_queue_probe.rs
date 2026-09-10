// SPDX-License-Identifier: GPL-2.0-only OR MIT
//! A retained, tag-15-only M5 queue registration experiment. No GPU kicks.

use super::{MappedObject, KERNEL_BASE};
use crate::{driver::AsahiDevice, g17_submission, m5_channel, mmu};
use core::sync::atomic::{fence, Ordering};
use kernel::{c_str, io, prelude::*};

const GRAPH_BASE: u64 = KERNEL_BASE + 0xc000_0000;
const ENTRY_HIGH: u64 = KERNEL_BASE + 0x39_0000;
const ENTRY_LOW: u64 = 0x70_005f_0000;
const QUEUE_ID: u8 = 11;
const QUEUE_OFFSET: usize = QUEUE_ID as usize * 0xc0;
const POINTERS: usize = 0x7000;
const JOB_LIST: usize = 0x7100;
const CONTROL: usize = 0x7200;
const ITEMS: usize = 0x8000;
const CONFIG: usize = 0xa800;
// GFX RTKit-3255.160.4 has TEXT at +0 and DATA at +0x4c000.
// Its tag-15 handler 0x122a4..0x12434 installs qid*0x28 at +0x127430.
const FIRMWARE_SLOT: usize = 0x127430 - 0x4c000 + QUEUE_ID as usize * 0x28;

struct FirmwareDataView {
    map: io::mem::Mem,
    cache_line: usize,
}
// SAFETY: This owns a fixed mapping of reserved RAM, read only through
// bounded volatile accesses. It neither transfers ownership nor writes DATA.
unsafe impl Send for FirmwareDataView {}
// SAFETY: Concurrent read-only volatile observations do not mutate host state.
unsafe impl Sync for FirmwareDataView {}

pub(super) struct M5QueueProbe {
    entry_alias: mmu::KernelMapping,
    entry: MappedObject,
    graph: MappedObject,
    firmware: FirmwareDataView,
}

impl M5QueueProbe {
    pub(super) fn new(dev: &AsahiDevice, uat: &mmu::Uat) -> Result<Self> {
        if !uat.is_t8142_geometry() {
            return Err(ENODEV);
        }
        let mut entry = MappedObject::new_wc_at(dev, uat.kernel_vm(), ENTRY_HIGH,
            0x20000, mmu::PROT_FW_PRIV_RW)?;
        let entry_alias = entry.map_alias_at(uat.kernel_lower_vm(), ENTRY_LOW,
            mmu::PROT_GPU_SHARED_RW)?;
        for offset in (0..0x20000).step_by(mmu::UAT_PGSZ) {
            if uat.kernel_vm().translate_iova(ENTRY_HIGH + offset as u64)?
                != uat.kernel_lower_vm().translate_iova(ENTRY_LOW + offset as u64)? {
                return Err(EFAULT);
            }
        }
        let mut graph = MappedObject::new_wc_at(dev, uat.kernel_vm(), GRAPH_BASE,
            0xc000, mmu::PROT_FW_PRIV_RW)?;
        let config = m5_channel::encode_m5_channel_config(m5_channel::M5ChannelConfig {
            ring_low: ENTRY_LOW, ring_high: ENTRY_HIGH, queue_id: QUEUE_ID, priority: 2,
            install_queue: true, gpu_context: 3, event: None,
            channel_control: GRAPH_BASE + CONTROL as u64,
            // Only the firmware queue-install branch is exercised. No user
            // context, scheduler owner, event machine or shader is activated.
            update_scheduler: false, scheduler_index: 0, channel_stamp_index: 0,
            update_context: false, owner_context: 0, extended_command: false,
        }).map_err(|_| EINVAL)?;
        graph.with_bytes_mut(|out| {
            let reset = g17_submission::encode_m5_25g76_channel_state_reset(
                g17_submission::M5ChannelStateResetValues {
                    uncached_state: GRAPH_BASE + POINTERS as u64,
                    command_pointer_ring: GRAPH_BASE + ITEMS as u64,
                    gpu_va_10: GRAPH_BASE + JOB_LIST as u64,
                    word_48: 0,
                    gpu_va_9c: GRAPH_BASE + CONTROL as u64,
                });
            out[QUEUE_OFFSET..QUEUE_OFFSET + 0xc0].copy_from_slice(&reset);
            // Measured prepared CL_2 channel fields, independent of SKSM ID.
            for (offset, value) in [(0x28, 2u32), (0x2c, 2), (0x40, 2)] {
                out[QUEUE_OFFSET + offset..QUEUE_OFFSET + offset + 4]
                    .copy_from_slice(&value.to_le_bytes());
            }
            out[QUEUE_OFFSET + 0x36..QUEUE_OFFSET + 0x38].fill(0xff);
            out[POINTERS + 0x50..POINTERS + 0x54].fill(0xff);
            out[POINTERS + 0x60..POINTERS + 0x64].copy_from_slice(&0x500u32.to_le_bytes());
            out[JOB_LIST + 8..JOB_LIST + 16]
                .copy_from_slice(&(GRAPH_BASE + JOB_LIST as u64).to_le_bytes());
            // Native M5 scheduler-control initial view. It is retained but
            // not attached by the tag-15-only probe (update_scheduler=false).
            out[CONTROL..CONTROL + 2].fill(0xff);
            out[CONTROL + 5] = 1;
            out[CONTROL + 0x26] = 2;
            out[CONTROL + 0x33] = 0xff;
            out[CONFIG..CONFIG + config.len()].copy_from_slice(&config);
            out[ITEMS..ITEMS + 8].copy_from_slice(&(GRAPH_BASE + CONFIG as u64).to_le_bytes());
            out[POINTERS + 0x40..POINTERS + 0x44].copy_from_slice(&1u32.to_le_bytes());
            Ok(())
        })?;
        let resource = dev.as_ref().of_node().ok_or(EINVAL)?
            .reserved_mem_region_to_resource_byname(c_str!("gfx-data"))?;
        if resource.size() != 0x134000 || resource.start() < 0x100_0000_0000
            || resource.start() & 0x3fff != 0
            || resource.start().checked_add(resource.size()).ok_or(EOVERFLOW)? > 0x104_0000_0000
            || FIRMWARE_SLOT + 0x28 > resource.size() as usize {
            return Err(EINVAL);
        }
        let ctr: u64;
        // SAFETY: CTR_EL0 is an architectural read-only cache geometry register.
        unsafe { core::arch::asm!("mrs {ctr}, ctr_el0", ctr = out(reg) ctr,
            options(nomem, nostack, preserves_flags)) };
        let cache_line = 4usize << ((ctr >> 16) & 0xf);
        if cache_line > 0x4000 {
            return Err(EINVAL);
        }
        // SAFETY: The exact firmware profile and reserved DATA extent are
        // checked above and by the caller. WB matches the System RAM linear
        // mapping; creating a WC alias of that RAM would be invalid. The host
        // never writes this firmware-owned extent. Each observation invalidates
        // the covering cache lines before reading the 40-byte queue slot.
        let map = unsafe { io::mem::Mem::try_new(resource, io::mem::MemFlag::WB.into()) }?;
        fence(Ordering::SeqCst);
        Ok(Self { entry_alias, entry, graph, firmware: FirmwareDataView { map, cache_line } })
    }

    pub(super) fn queue_address(&self) -> u64 {
        GRAPH_BASE + QUEUE_OFFSET as u64
    }

    pub(super) fn inner_counters(&mut self) -> Result<[u32; 3]> {
        Ok([self.graph.read_u32(GRAPH_BASE + POINTERS as u64)?,
            self.graph.read_u32(GRAPH_BASE + POINTERS as u64 + 0x30)?,
            self.graph.read_u32(GRAPH_BASE + POINTERS as u64 + 0x40)?])
    }

    pub(super) fn firmware_slot(&self) -> [u64; 5] {
        let line = self.firmware.cache_line;
        let start = FIRMWARE_SLOT & !(line - 1);
        let end = (FIRMWARE_SLOT + 0x28 + line - 1) & !(line - 1);
        for offset in (start..end).step_by(line) {
            // SAFETY: Both covering lines are wholly inside the aligned,
            // retained firmware-owned DATA extent. No host code writes that
            // extent, so invalidation cannot discard dirty host data. Never
            // clean stale CPU cache lines back over firmware updates.
            unsafe { core::arch::asm!("dc ivac, {addr}",
                addr = in(reg) self.firmware.map.ptr().add(offset),
                options(nostack, preserves_flags)) };
        }
        // SAFETY: Wait for the preceding invalidations before volatile reads.
        unsafe { core::arch::asm!("dsb sy", options(nostack, preserves_flags)) };
        let mut out = [0; 5];
        for (index, value) in out.iter_mut().enumerate() {
            // SAFETY: The constructor bounds-checks the entire aligned slot
            // inside this retained reserved-RAM mapping. Firmware is its writer.
            *value = unsafe { core::ptr::read_volatile(
                self.firmware.map.ptr().add(FIRMWARE_SLOT + index * 8).cast::<u64>()) };
        }
        fence(Ordering::Acquire);
        out
    }

    pub(super) fn is_installed(slot: [u64; 5]) -> bool {
        slot[0] & 1 != 0 && slot[1] == ENTRY_HIGH && slot[2] == ENTRY_LOW
            && slot[3] == 4u64 << 32 && slot[4] as u32 == 2
    }
}
