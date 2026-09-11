// SPDX-License-Identifier: GPL-2.0-only OR MIT
//! Retained USC backing for the bounded M5 native-program graphics job.
//!
//! The graphics layout differs from the proven compute layout: its first
//! block occupies the compute operand range. Each execution context owns
//! exactly one of these layouts. No root or firmware table is published here.

use super::MappedObject;
use crate::{driver::AsahiDevice, mmu};
use kernel::prelude::*;

pub(super) const HARDWARE_ID: u32 = 1;
pub(super) const DIRECTORY: u64 = 0x70_0000_0000;
const DIRECTORY_ENTRIES: usize = 0x80000;
const FREELIST: u64 = 0x70_0040_8000;
const BLOCK_BASE: u64 = 0x70_0042_0000;
const BLOCK_BYTES: usize = 0x20_0000;
const BLOCK_STRIDE: u64 = 0x20_8000;
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
        let mut directory = MappedObject::new_wc_at(dev, vm, DIRECTORY,
            DIRECTORY_ENTRIES * 8, mmu::PROT_GPU_SHARED_RW)?;
        let mut freelist = MappedObject::new_wc_at(dev, vm, FREELIST,
            mmu::UAT_PGSZ, mmu::PROT_GPU_SHARED_RW)?;
        let mut blocks = KVec::new();
        for index in 0..BLOCK_COUNT {
            let address = BLOCK_BASE + index as u64 * BLOCK_STRIDE;
            blocks.push(MappedObject::new_wc_at(dev, vm, address, BLOCK_BYTES,
                mmu::PROT_GPU_SHARED_RW)?, GFP_KERNEL)?;
            // The two 16-KiB guard pages must not acquire backing, including
            // the gap after the final block and after the directory.
            for delta in [0, mmu::UAT_PGSZ] {
                if vm.translate_iova(address + (BLOCK_BYTES + delta) as u64).is_ok() {
                    return Err(EFAULT);
                }
            }
        }
        for delta in [0, mmu::UAT_PGSZ] {
            if vm.translate_iova(DIRECTORY + (DIRECTORY_ENTRIES * 8 + delta) as u64).is_ok() {
                return Err(EFAULT);
            }
        }
        directory.with_bytes_mut(|out| {
            for unit in 0..UNIT_COUNT {
                let address = Self::unit_address(unit).ok_or(EINVAL)?;
                out[unit * 8..unit * 8 + 8].copy_from_slice(&address.to_le_bytes());
            }
            Ok(())
        })?;
        freelist.with_bytes_mut(|out| {
            // Same initial free-block format as the qualified M5 compute
            // pool. Every allocation here is free; no native growth history
            // or in-use allocator contents are imported.
            for block in 0..BLOCK_COUNT {
                let address = BLOCK_BASE + block as u64 * BLOCK_STRIDE;
                out[block * 0x40..block * 0x40 + 8]
                    .copy_from_slice(&(address | (1u64 << 61)).to_le_bytes());
            }
            Ok(())
        })?;
        let mut result = Self { directory, freelist, blocks };
        result.validate(vm)?;
        Ok(result)
    }

    fn unit_address(unit: usize) -> Option<u64> {
        if unit >= UNIT_COUNT { return None; }
        let units_per_block = BLOCK_BYTES / UNIT_BYTES;
        Some(BLOCK_BASE + (unit / units_per_block) as u64 * BLOCK_STRIDE
            + (unit % units_per_block * UNIT_BYTES) as u64)
    }

    pub(super) fn descriptor(&self) -> [u64; 4] {
        // M5 halLoadUMADesc: 4-KiB population, not 16-KiB CPU page count.
        [DIRECTORY >> 7, (UNIT_COUNT as u64) << 32,
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
        for (offset, address) in [(0x14, DIRECTORY), (0x30, FREELIST), (0x4c, inner)] {
            out[offset..offset + 8].copy_from_slice(&address.to_le_bytes());
        }
        Ok(())
    }

    /// Verify every CPU page named by the populated 4-KiB directory and the
    /// free-block heads. The allocator keeps the underlying mappings alive.
    pub(super) fn validate(&mut self, vm: &mmu::Vm) -> Result {
        if self.blocks.len() != BLOCK_COUNT { return Err(EFAULT); }
        for block in 0..BLOCK_COUNT {
            let base = BLOCK_BASE + block as u64 * BLOCK_STRIDE;
            if self.blocks[block].iova() != base
                || self.freelist.read_u64(FREELIST + (block * 0x40) as u64)?
                    != (base | (1u64 << 61)) {
                return Err(EFAULT);
            }
            for offset in (0..BLOCK_BYTES).step_by(mmu::UAT_PGSZ) {
                vm.translate_iova(base + offset as u64)?;
            }
        }
        for unit in 0..UNIT_COUNT {
            if self.directory.read_u64(DIRECTORY + (unit * 8) as u64)?
                != Self::unit_address(unit).ok_or(EFAULT)? {
                return Err(EFAULT);
            }
        }
        if self.directory.read_u64(DIRECTORY + (UNIT_COUNT * 8) as u64)? != 0 {
            return Err(EFAULT);
        }
        Ok(())
    }
}
