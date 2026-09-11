// SPDX-License-Identifier: GPL-2.0-only OR MIT
//! Retained USC backing in an M5 application's reserved driver VA region.
//!
//! Firmware records are encoded from the owned mappings, never fixed test
//! addresses. Guard pages belong to each mapping's allocator reservation.
//! No root or firmware table is published here.

use super::MappedObject;
use crate::{driver::AsahiDevice, mmu};
use kernel::prelude::*;

pub(super) const HARDWARE_ID: u32 = 1;
const DIRECTORY_ENTRIES: usize = 0x80000;
const BLOCK_BYTES: usize = 0x20_0000;
const BLOCK_COUNT: usize = 19;
const UNIT_BYTES: usize = 0x1000;
const UNIT_COUNT: usize = BLOCK_COUNT * BLOCK_BYTES / UNIT_BYTES;
pub(super) const CAPACITY_BYTES: u64 = (BLOCK_COUNT * BLOCK_BYTES) as u64;

pub(super) struct M5GraphicsUsc {
    directory: MappedObject,
    freelist: MappedObject,
    blocks: KVec<MappedObject>,
}

impl M5GraphicsUsc {
    pub(super) fn new(dev: &AsahiDevice, vm: &mmu::Vm) -> Result<Self> {
        Self::new_in_range(dev, vm, vm.kernel_range())
    }

    pub(super) fn new_application(dev: &AsahiDevice, vm: &mmu::Vm) -> Result<Self> {
        let range = crate::m5_vm::auxiliary_range(&vm.kernel_range()).ok_or(EINVAL)?;
        Self::new_in_range(dev, vm, range)
    }

    fn new_in_range(
        dev: &AsahiDevice, vm: &mmu::Vm, range: core::ops::Range<u64>,
    ) -> Result<Self> {
        let allocate = |size, guard_pages| MappedObject::new_wc_in_range_with_guards(
            dev, vm, range.clone(), size, 0x8000, mmu::PROT_GPU_SHARED_RW, guard_pages,
        );
        let mut directory = allocate(DIRECTORY_ENTRIES * 8, 2)?;
        let mut freelist = allocate(mmu::UAT_PGSZ, 1)?;
        let mut blocks = KVec::new();
        for _ in 0..BLOCK_COUNT {
            blocks.push(allocate(BLOCK_BYTES, 2)?, GFP_KERNEL)?;
        }
        directory.with_bytes_mut(|out| {
            for unit in 0..UNIT_COUNT {
                let address = Self::unit_address(&blocks, unit).ok_or(EINVAL)?;
                out[unit * 8..unit * 8 + 8].copy_from_slice(&address.to_le_bytes());
            }
            Ok(())
        })?;
        freelist.with_bytes_mut(|out| {
            // Same initial free-block format as the qualified M5 compute
            // pool. Every allocation here is free; no native growth history
            // or in-use allocator contents are imported.
            for block in 0..BLOCK_COUNT {
                let address = blocks[block].iova();
                out[block * 0x40..block * 0x40 + 8]
                    .copy_from_slice(&(address | (1u64 << 61)).to_le_bytes());
            }
            Ok(())
        })?;
        let mut result = Self { directory, freelist, blocks };
        result.validate(vm)?;
        dev_info!(dev.as_ref(),
            "M5_GRAPHICS_USC reserved={:#x}..{:#x} directory={:#x} freelist={:#x} blocks={} capacity={:#x}; guard pages reserved\n",
            range.start, range.end, result.directory.iova(), result.freelist.iova(),
            BLOCK_COUNT, CAPACITY_BYTES);
        Ok(result)
    }

    fn unit_address(blocks: &[MappedObject], unit: usize) -> Option<u64> {
        if unit >= UNIT_COUNT { return None; }
        let units_per_block = BLOCK_BYTES / UNIT_BYTES;
        Some(blocks.get(unit / units_per_block)?.iova()
            + (unit % units_per_block * UNIT_BYTES) as u64)
    }

    pub(super) fn descriptor(&self) -> [u64; 4] {
        // M5 halLoadUMADesc: 4-KiB population, not 16-KiB CPU page count.
        [self.directory.iova() >> 7, (UNIT_COUNT as u64) << 32,
            (DIRECTORY_ENTRIES as u64) << 32 | UNIT_COUNT as u64, 0]
    }

    /// Initialize a fresh owner using the qualified M5 initial-state recipe.
    /// Native graphics snapshots include later growth generations; copying
    /// their seeds/disabled flag would disagree with these all-free blocks.
    pub(super) fn encode_initial_owner(&self, inner: u64, out: &mut [u8]) -> Result {
        if out.len() < 0x70 || inner < 0xffff_fc00_0000_0000 || inner & 7 != 0 {
            return Err(EINVAL);
        }
        out[..0x70].fill(0);
        for (offset, value) in [(0, 1u32), (8, HARDWARE_ID), (0x10, 2),
            (0x1c, DIRECTORY_ENTRIES as u32), (0x24, UNIT_COUNT as u32),
            (0x2c, UNIT_COUNT as u32), (0x40, 4), (0x48, (BLOCK_COUNT * 8) as u32),
            (0x5c, 1), (0x60, 2)] {
            out[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
        }
        for (offset, address) in [(0x14, self.directory.iova()),
            (0x30, self.freelist.iova()), (0x4c, inner)] {
            out[offset..offset + 8].copy_from_slice(&address.to_le_bytes());
        }
        Ok(())
    }

    /// Verify every CPU page named by the populated 4-KiB directory and the
    /// free-block heads. The allocator keeps the underlying mappings alive.
    pub(super) fn validate(&mut self, vm: &mmu::Vm) -> Result {
        if self.blocks.len() != BLOCK_COUNT { return Err(EFAULT); }
        let range = vm.kernel_range();
        let validate_guards = |object: &MappedObject, size: usize, guards: usize| -> Result {
            let end = object.iova().checked_add((size + guards * mmu::UAT_PGSZ) as u64)
                .ok_or(EOVERFLOW)?;
            if object.iova() < range.start || end > range.end
                || object.iova() & 0x7fff != 0 { return Err(EFAULT); }
            // Check after every allocation has completed: a later neighbor
            // must not have consumed either reserved 16-KiB guard page.
            for guard in 0..guards {
                if vm.translate_iova(object.iova() + (size + guard * mmu::UAT_PGSZ) as u64).is_ok() {
                    return Err(EFAULT);
                }
            }
            Ok(())
        };
        validate_guards(&self.directory, DIRECTORY_ENTRIES * 8, 2)?;
        validate_guards(&self.freelist, mmu::UAT_PGSZ, 1)?;
        for block in &self.blocks { validate_guards(block, BLOCK_BYTES, 2)?; }
        for block in 0..BLOCK_COUNT {
            let base = self.blocks[block].iova();
            if self.freelist.read_u64(self.freelist.iova() + (block * 0x40) as u64)?
                    != (base | (1u64 << 61)) {
                return Err(EFAULT);
            }
            for offset in (0..BLOCK_BYTES).step_by(mmu::UAT_PGSZ) {
                vm.translate_iova(base + offset as u64)?;
            }
        }
        for unit in 0..UNIT_COUNT {
            if self.directory.read_u64(self.directory.iova() + (unit * 8) as u64)?
                != Self::unit_address(&self.blocks, unit).ok_or(EFAULT)? {
                return Err(EFAULT);
            }
        }
        if self.directory.read_u64(self.directory.iova() + (UNIT_COUNT * 8) as u64)? != 0 {
            return Err(EFAULT);
        }
        Ok(())
    }
}
