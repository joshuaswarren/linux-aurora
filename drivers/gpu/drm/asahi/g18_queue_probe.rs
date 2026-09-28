// SPDX-License-Identifier: GPL-2.0-only OR MIT
//! J873/26A428 tag15-only queue registration. No SKSM kicks or GPU jobs.
//!
//! Native kernelcache: AGXChannel::resetChannelState 0xfffffe0008bdd178;
//! AGXCLChannelSKSM::submitBuffer 0xfffffe0008be3ddc..0x8be3fcc.
//! GFX b000: dispatcher 0x1267c, tag15 target 0x12824, table 0xb90b8.
//! M6 adds 12 bytes at config +0x6e; never use M5's 0x86-byte packet here.

use super::MappedObject;
use crate::{driver::AsahiDevice, mmu};
use core::sync::atomic::{fence, Ordering};
use kernel::{c_str, io, prelude::*};

const KERNEL_BASE: u64 = 0xffff_fc20_0000_0000;
const GRAPH_BASE: u64 = KERNEL_BASE + 0xc000_0000;
const ENTRY_HIGH: u64 = KERNEL_BASE + 0x39_0000;
const ENTRY_LOW: u64 = 0x70_005f_0000;
const ENTRY_SIZE: usize = 0x20000; // b000 0x1257c/0x125a0: 256 * 0x200
pub(crate) const QUEUE_ID: u8 = 11;
const POINTERS: usize = 0x100;
const JOB_LIST: usize = 0x200;
const CONTROL: usize = 0x300;
const ITEMS: usize = 0x800;
const CAPACITY: u32 = 0x100;
const CONFIG: usize = 0x1800;
const DATA_SIZE: usize = 0xbc000;
const SLOT: usize = 0xb90b8 - 0x50000 + QUEUE_ID as usize * 0x28;
const SCHEDULER: usize = 0xb7468 - 0x50000 + QUEUE_ID as usize * 8;

/// Reserved GFX DATA, never written by the host. No native snapshot pointers
/// are used: the current DT reservation is the only physical address source.
struct FirmwareDataView {
    map: io::mem::Mem,
    cache_line: usize,
}
// SAFETY: The fixed reserved RAM mapping is observed only through bounded
// volatile reads; no host write or ownership transfer is possible.
unsafe impl Send for FirmwareDataView {}
// SAFETY: Concurrent read-only observations cannot mutate host-owned state.
unsafe impl Sync for FirmwareDataView {}

impl FirmwareDataView {
    fn new(dev: &AsahiDevice) -> Result<Self> {
        let resource = dev.as_ref().of_node().ok_or(EINVAL)?
            .reserved_mem_region_to_resource_byname(c_str!("gfx-data"))?;
        if resource.size() != DATA_SIZE as u64 || resource.start() < 0x100_0000_0000
            || resource.start() & 0x3fff != 0
            || resource.start().checked_add(resource.size()).ok_or(EOVERFLOW)? > 0x104_0000_0000
        {
            return Err(EINVAL);
        }
        let ctr: u64;
        // SAFETY: Architectural read-only cache geometry register.
        unsafe { core::arch::asm!("mrs {ctr}, ctr_el0", ctr = out(reg) ctr,
            options(nomem, nostack, preserves_flags)) };
        let cache_line = 4usize << ((ctr >> 16) & 0xf);
        if cache_line > 0x4000 || (SLOT + 40 + cache_line - 1) & !(cache_line - 1) > DATA_SIZE {
            return Err(EINVAL);
        }
        // SAFETY: WB matches reserved System RAM; the host never dirties it.
        let map = unsafe { io::mem::Mem::try_new(resource, io::mem::MemFlag::WB.into()) }?;
        Ok(Self { map, cache_line })
    }

    fn words<const N: usize>(&self, offset: usize) -> [u64; N] {
        // The only callers are the fixed slot and scheduler extents. Both
        // are below the constructor-checked slot end inside reserved DATA.
        let start = offset & !(self.cache_line - 1);
        let end = (offset + N * 8 + self.cache_line - 1) & !(self.cache_line - 1);
        for offset in (start..end).step_by(self.cache_line) {
            // SAFETY: Constructor bounds the whole covering cache lines.
            // Invalidate only: cleaning stale data would corrupt firmware.
            unsafe { core::arch::asm!("dc ivac, {addr}",
                addr = in(reg) self.map.ptr().add(offset), options(nostack, preserves_flags)) };
        }
        // SAFETY: Complete cache invalidation before observing firmware stores.
        unsafe { core::arch::asm!("dsb sy", options(nostack, preserves_flags)) };
        let mut out = [0; N];
        for (index, word) in out.iter_mut().enumerate() {
            // SAFETY: Aligned reads wholly inside the constructor-checked slot.
            *word = unsafe { core::ptr::read_volatile(
                self.map.ptr().add(offset + index * 8).cast::<u64>()) };
        }
        fence(Ordering::Acquire);
        out
    }
}

pub(crate) struct G18QueueProbe {
    _entry_alias: mmu::KernelMapping,
    _entry: MappedObject,
    graph: MappedObject,
    firmware: FirmwareDataView,
}

fn put32(out: &mut [u8], offset: usize, value: u32) {
    out[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

fn put64(out: &mut [u8], offset: usize, value: u64) {
    out[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}

impl G18QueueProbe {
    pub(crate) fn new(dev: &AsahiDevice, uat: &mmu::Uat, bind_scheduler: bool) -> Result<Self> {
        if !uat.is_t8152_geometry() { return Err(ENODEV); }
        let mut entry = MappedObject::new_wc_at(dev, uat.kernel_vm(), ENTRY_HIGH,
            ENTRY_SIZE, mmu::PROT_FW_PRIV_RW)?;
        let entry_alias = entry.map_alias_at(uat.kernel_lower_vm(), ENTRY_LOW,
            mmu::PROT_GPU_SHARED_RW)?;
        for offset in (0..ENTRY_SIZE).step_by(mmu::UAT_PGSZ) {
            if uat.kernel_vm().translate_iova(ENTRY_HIGH + offset as u64)?
                != uat.kernel_lower_vm().translate_iova(ENTRY_LOW + offset as u64)? {
                return Err(EFAULT);
            }
        }
        let mut graph = MappedObject::new_wc_at(dev, uat.kernel_vm(), GRAPH_BASE,
            0x4000, mmu::PROT_FW_PRIV_RW)?;
        graph.with_bytes_mut(|out| {
            // M6 channel state: resetChannelState zeros 0x80 bytes. The
            // command processor only needs the pointers and ring capacity
            // for this tag15-only experiment; retain native reset sentinels.
            put64(out, 0, GRAPH_BASE + POINTERS as u64);
            put64(out, 8, GRAPH_BASE + ITEMS as u64);
            put64(out, 0x10, GRAPH_BASE + JOB_LIST as u64);
            put32(out, 0x24, u32::MAX);
            put32(out, 0x28, 4);
            put32(out, 0x40, u32::MAX);
            put32(out, 0x44, 0);
            put64(out, 0x70, GRAPH_BASE + CONTROL as u64);
            put32(out, POINTERS + 0x50, u32::MAX);
            put32(out, POINTERS + 0x60, CAPACITY);
            put64(out, JOB_LIST + 8, GRAPH_BASE + JOB_LIST as u64);
            if bind_scheduler {
                // 26A428 AGXCommandQueueReclaimableState::init at
                // 0xfffffe0008bf9014. Both indices start unallocated.
                out[CONTROL..CONTROL + 2].fill(0xff);
                out[CONTROL + 5] = 1;
                out[CONTROL + 0x33] = 0xff;
                // b000 0xb234 sets2 on this fresh process-slot path.
                out[CONTROL + 0x26] = 2;
            }

            // b000 0x12824 requests 0x98 bytes. Unwritten trailing padding
            // is zero. UMA/PB pool bindings and the new +0x6e/+0x72 grow
            // pair are disabled. UMA +0x3e is a FList generation identity,
            // not an event pointer; +0x46 selects its firmware pool slot.
            let config = &mut out[CONFIG..CONFIG + 0x98];
            put32(config, 0, 15);
            put64(config, 8, ENTRY_LOW);
            put64(config, 0x10, ENTRY_HIGH);
            config[0x18..0x1a].copy_from_slice(&u16::from(QUEUE_ID).to_le_bytes());
            for (offset, value) in [(0x1a, 1), (0x1e, 2), (0x22, 2), (0x32, 3), (0x5e, 2)] {
                put32(config, offset, value);
            }
            put64(config, 0x3e, u64::MAX);
            put32(config, 0x46, u32::MAX);
            put64(config, 0x4a, GRAPH_BASE + CONTROL as u64);
            config[0x82..0x92].fill(0xff);
            put64(out, ITEMS, GRAPH_BASE + CONFIG as u64);
            put32(out, POINTERS + 0x40, 1);
            Ok(())
        })?;
        fence(Ordering::SeqCst);
        Ok(Self { _entry_alias: entry_alias, _entry: entry, graph,
            firmware: FirmwareDataView::new(dev)? })
    }

    pub(crate) fn queue_address(&self) -> u64 { GRAPH_BASE }

    pub(crate) fn counters(&mut self) -> Result<[u32; 3]> {
        Ok([self.graph.read_u32(GRAPH_BASE + POINTERS as u64)?,
            self.graph.read_u32(GRAPH_BASE + POINTERS as u64 + 0x30)?,
            self.graph.read_u32(GRAPH_BASE + POINTERS as u64 + 0x40)?])
    }

    pub(crate) fn slot(&self) -> [u64; 5] { self.firmware.words(SLOT) }

    pub(crate) fn scheduler_status(&mut self) -> Result<[u64; 4]> {
        Ok([self.firmware.words::<1>(SCHEDULER)[0],
            self.graph.read_u64(GRAPH_BASE + CONTROL as u64)?,
            self.graph.read_u64(GRAPH_BASE + CONTROL as u64 + 0x12)?,
            self.graph.read_u64(GRAPH_BASE + CONTROL as u64 + 0x1a)?])
    }

    pub(crate) fn prepare_scheduler_bind(&mut self) -> Result {
        if self.counters()? != [1; 3] || !Self::installed(self.slot())
            || self.scheduler_status()? != [0, 0x1000000ffff, 0, 0] {
            return Err(EBUSY);
        }
        self.graph.with_bytes_mut(|out| {
            if out[CONFIG + 0x100..CONFIG + 0x198].iter().any(|x| *x != 0) {
                return Err(EBUSY);
            }
            out.copy_within(CONFIG..CONFIG + 0x98, CONFIG + 0x100);
            let config = &mut out[CONFIG + 0x100..CONFIG + 0x198];
            put32(config, 0x1a, 0); // already installed
            put32(config, 0x52, 1); // bind scheduler
            put64(out, ITEMS + 8, GRAPH_BASE + CONFIG as u64 + 0x100);
            Ok(())
        })?;
        fence(Ordering::SeqCst);
        self.graph.write_u32(GRAPH_BASE + POINTERS as u64 + 0x40, 2)?;
        fence(Ordering::SeqCst);
        Ok(())
    }

    pub(crate) fn scheduler_bound([pointer, header, low, high]: [u64; 4]) -> bool {
        pointer == GRAPH_BASE + CONTROL as u64 && header & 0xff != 0xff
            && (header >> 8) & 0xff != 0xff
            && (header >> 16) & 0xffff_ffff == 0x0104_0204
            && low == 1u64 << QUEUE_ID && high == 0
    }

    pub(crate) fn installed(slot: [u64; 5]) -> bool {
        slot[0] & 1 != 0 && slot[1] == ENTRY_HIGH && slot[2] == ENTRY_LOW
            && slot[3] == 4u64 << 32 && slot[4] as u32 == 2
    }
}
