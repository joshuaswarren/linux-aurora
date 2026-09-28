// SPDX-License-Identifier: GPL-2.0-only OR MIT
//! Owned initial G18 UMA pool resources. No firmware binding is published here.
//!
//! Flags 3 use a sparse directory: a page at each USC stride plus the final
//! page, with hardware addressing starting one KiB before the first page end.
//! Initial mappings reserve their trailing holes using the existing fixed
//! mapping guards. Populating those holes for runtime growth is not implemented.
//! The state must be fully encoded before a caller may bind or submit work.
//! A job's pool also needs its hardware UMA descriptor loaded into the table
//! slot before the tag15 that names the pool is published.

#![allow(dead_code)]

use super::MappedObject;
use crate::{driver::AsahiDevice, g18_uma, mmu};
use core::ops::Range;
use core::sync::atomic::{fence, Ordering};
use kernel::prelude::*;

// Host-selected addresses in the shared GPU view, disjoint from the descriptor
// and completion aliases. Actual extents are checked before allocation.
const DIRECTORY_BASE: u64 = 0x70_0200_0000;
const BLOCK_LIST_BASE: u64 = 0x70_0400_0000;
const BLOCK_DATA_BASE: u64 = 0x70_0500_0000;
const BLOCK_DATA_STRIDE: u64 = 0x20_0000;
const STATE_BYTES: usize = g18_uma::G18_POOL_STATE_BYTES;
/// Hardware UMA slot of this boot's single pool. Native
/// `AGXUSCPrivMemPool::prepareLocked` takes it from the accelerator's ID
/// manager (accelerator+0x13b98, 0x100 IDs, init 0xfffffe0008c458f4); the
/// first grant on a fresh accelerator is 0. The same ID is the tag15 +0x46
/// firmware pool slot (descriptor+0x7a8) and the low half of register 0x10241.
pub(crate) const G18_UMA_HARDWARE_SLOT: u32 = 0;
/// prepareLocked rejects IDs >= 0x100 (0xfffffe0008c8f334): 0x8000 bytes.
const UMA_TABLE_SLOTS: usize = 0x100;

#[derive(Debug, Copy, Clone)]
pub(crate) struct G18UmaPoolBinding {
    pub(crate) state: u64,
    pub(crate) generation: u64,
    pub(crate) slot: u32,
}

#[derive(Debug, Copy, Clone)]
pub(crate) struct G18UmaPoolView {
    pub(crate) directory_base: u64,
    pub(crate) reservation_bytes: u64,
    pub(crate) stride: u64,
    pub(crate) page_count: usize,
    pub(crate) gpu_base_offset: u64,
    pub(crate) state_high: u64,
    pub(crate) block_list_high: u64,
    pub(crate) block_list_low: u64,
}

pub(crate) struct G18UmaPoolMemory {
    // Drop the GPU aliases and their reservations before the CPU/FW mappings.
    // Every mapping independently retains its GEM backing and owning VM.
    // The job-context aliases go first: that VM's roots may still be live.
    process_aliases: KVec<mmu::KernelMapping>,
    aliases: KVec<mmu::KernelMapping>,
    scratch_aliases: KVec<mmu::KernelMapping>,
    scratch: KVec<MappedObject>,
    scratch_plan: Option<g18_uma::G18ComputeScratchPlan>,
    directory: KVec<MappedObject>,
    block_list: MappedObject,
    state: MappedObject,
    // Firmware dereferences state+0x88 during pool retirement. Keep the
    // independently allocated counter alive for the entire pool lifetime.
    preempt_counter: MappedObject,
    geometry: g18_uma::G18UmaGeometry,
    layout: g18_uma::G18UmaSparseDirectory,
    encoded: bool,
    descriptor_loaded: bool,
}

impl G18UmaPoolMemory {
    pub(crate) fn new(
        dev: &AsahiDevice,
        uat: &mmu::Uat,
        high_range: Range<u64>,
        uscs: u32,
    ) -> Result<Self> {
        if !uat.is_t8152_geometry() || mmu::UAT_PGSZ != 0x4000 {
            return Err(ENODEV);
        }
        // A 1 MiB block is one native accessor's admitted size. This is the
        // initial host policy, not a claim to reproduce every native override.
        let geometry = g18_uma::g18_uma_geometry(0x8000_0000, 0x10_0000,
                                                uscs, false, true, 14).ok_or(EINVAL)?;
        let layout = g18_uma::g18_uma_sparse_directory(&geometry, uscs, 14).ok_or(EINVAL)?;
        if DIRECTORY_BASE.checked_add(layout.reservation_bytes).ok_or(EOVERFLOW)? > BLOCK_LIST_BASE {
            return Err(EINVAL);
        }
        let fixed = |size| MappedObject::new_wc_in_range(dev, uat.kernel_vm(),
            high_range.clone(), size, mmu::UAT_PGSZ as u64, mmu::PROT_FW_SHARED_RW);
        let mut directory = KVec::new();
        let mut aliases = KVec::new();
        for index in 0..layout.initial_page_count {
            let offset = layout.page_offset(index).ok_or(EINVAL)?;
            let end = if index + 1 == layout.initial_page_count {
                layout.reservation_bytes
            } else {
                layout.page_offset(index + 1).ok_or(EINVAL)?
            };
            let guards: usize = ((end - offset - layout.page_bytes) / layout.page_bytes).try_into()?;
            let mut page = fixed(mmu::UAT_PGSZ)?;
            aliases.push(page.map_alias_at_with_guards(uat.kernel_lower_vm(),
                DIRECTORY_BASE + offset, mmu::PROT_GPU_SHARED_RW, guards)?, GFP_KERNEL)?;
            if uat.kernel_vm().translate_iova(page.iova())?
                != uat.kernel_lower_vm().translate_iova(DIRECTORY_BASE + offset)? {
                return Err(EFAULT);
            }
            directory.push(page, GFP_KERNEL)?;
        }
        // Validate every PTE in the aperture, including the intentionally
        // absent entries. Do this before a firmware owner can see any address.
        let mut holes = 0usize;
        for offset in (0..layout.reservation_bytes).step_by(mmu::UAT_PGSZ) {
            let mapped = uat.kernel_lower_vm().translate_iova(DIRECTORY_BASE + offset).is_ok();
            let expected = offset % geometry.directory_bytes_per_usc == 0;
            if mapped != expected { return Err(EFAULT); }
            if !mapped { holes += 1; }
        }
        // A missing PTE is not enough: the allocator must also reject a new
        // owner inside both the first and last gap. On unexpected success,
        // dropping the temporary alias unwinds it before returning an error.
        if geometry.directory_bytes_per_usc > layout.page_bytes {
            for offset in [layout.page_bytes, layout.reservation_bytes - 2 * layout.page_bytes] {
                match directory[0].map_alias_at(uat.kernel_lower_vm(),
                                               DIRECTORY_BASE + offset, mmu::PROT_GPU_SHARED_RW) {
                    Err(error) if error == EBUSY || error == ENOSPC => {},
                    Err(error) => return Err(error),
                    Ok(alias) => { drop(alias); return Err(EFAULT); },
                }
            }
        }
        let block_bytes = usize::try_from(u64::from(geometry.block_count) * 4)?;
        let block_bytes = block_bytes.checked_add(mmu::UAT_PGSZ - 1).ok_or(EOVERFLOW)?
            & !(mmu::UAT_PGSZ - 1);
        let mut block_list = fixed(block_bytes)?;
        aliases.push(block_list.map_alias_at(uat.kernel_lower_vm(), BLOCK_LIST_BASE,
                                            mmu::PROT_GPU_SHARED_RW)?, GFP_KERNEL)?;
        for offset in (0..block_bytes).step_by(mmu::UAT_PGSZ) {
            if uat.kernel_vm().translate_iova(block_list.iova() + offset as u64)?
                != uat.kernel_lower_vm().translate_iova(BLOCK_LIST_BASE + offset as u64)? {
                return Err(EFAULT);
            }
        }
        let mut state = fixed(STATE_BYTES)?;
        state.with_bytes_mut(|bytes| {
            // Native initial hardware slot is unassigned. All other fields
            // remain unpublished until the native pool-state encoder runs.
            bytes[8..12].copy_from_slice(&u32::MAX.to_le_bytes());
            Ok(())
        })?;
        // Native FList::init allocates eight bytes at 8c8d28c..8c8d298;
        // serialize at 8c8c19c/1b0 publishes its GPU address/current value.
        // A null address crashes b000 at 1e5c4 (stage28); zero is a valid
        // initial value of a backed counter, never a valid counter address.
        let mut preempt_counter = fixed(8)?;
        preempt_counter.with_bytes_mut(|bytes| {
            if bytes.len() != 8 || bytes.iter().any(|&value| value != 0) {
                return Err(EINVAL);
            }
            Ok(())
        })?;
        uat.kernel_vm().translate_iova(preempt_counter.iova())?;
        dev_info!(dev.as_ref(),
            "G18 UMA retirement counter: owned FW address={:#x} initial=0; unpublished\n",
            preempt_counter.iova());
        dev_info!(dev.as_ref(),
            "G18 UMA sparse owner: reserved={:#x} backing_pages={} holes={} bias={:#x} aliases=verified guards=verified unpublished\n",
            layout.reservation_bytes, layout.initial_page_count, holes, layout.gpu_base_offset);
        Ok(Self {
            process_aliases: KVec::new(),
            aliases, scratch_aliases: KVec::new(), scratch: KVec::new(), scratch_plan: None,
            directory, block_list, state, preempt_counter, geometry, layout, encoded: false,
            descriptor_loaded: false,
        })
    }

    /// Retain the exact addition fixture's initial backing before publication.
    /// A failed allocation/alias check leaves the prior empty pool untouched.
    /// No firmware command, process root or hardware pool binding is published.
    pub(crate) fn prepare_compute_backing(
        &mut self, dev: &AsahiDevice, uat: &mmu::Uat, high_range: Range<u64>,
        plan: g18_uma::G18ComputeScratchPlan,
    ) -> Result<()> {
        if self.encoded || self.scratch_plan.is_some() { return Err(EBUSY); }
        if !uat.is_t8152_geometry() || mmu::UAT_PGSZ != 0x4000
            || plan.block_bytes != self.geometry.block_bytes
            || plan.block_bytes != 0x10_0000 || plan.block_count != 5
            || plan.register_pair != [16, 6] || plan.chunk_counters != [0, 350]
            || self.geometry.pool_bytes != 0x8000_0000
            || self.geometry.directory_bytes != self.geometry.directory_bytes_per_usc * 12
            || g18_uma::g18_initial_directory_fits(plan.block_count, plan.block_bytes, 12, 3, 14)
                != Some(true) {
            return Err(EINVAL);
        }
        let mut backing = KVec::new();
        let mut aliases = KVec::new();
        let mut entries = [0u32; 5];
        for (index, entry) in entries.iter_mut().enumerate() {
            let gpu = BLOCK_DATA_BASE + index as u64 * BLOCK_DATA_STRIDE;
            *entry = g18_uma::g18_uma_block_entry(gpu, 0).ok_or(EINVAL)?;
            let mut block = MappedObject::new_wc_in_range(
                dev, uat.kernel_vm(), high_range.clone(), plan.block_bytes as usize,
                mmu::UAT_PGSZ as u64, mmu::PROT_FW_SHARED_RW,
            )?;
            // GPU-address alignment, not physical contiguity: the native list
            // stores GPU virtual addresses and the UAT retains each GEM page.
            let alias = block.map_alias_at_with_guards(
                uat.kernel_lower_vm(), gpu, mmu::PROT_GPU_SHARED_RW, 1,
            )?;
            for offset in (0..plan.block_bytes).step_by(mmu::UAT_PGSZ) {
                if uat.kernel_vm().translate_iova(block.iova() + offset)?
                    != uat.kernel_lower_vm().translate_iova(gpu + offset)? {
                    return Err(EFAULT);
                }
            }
            if uat.kernel_lower_vm().translate_iova(gpu + plan.block_bytes).is_ok() {
                return Err(EFAULT);
            }
            aliases.push(alias, GFP_KERNEL)?;
            backing.push(block, GFP_KERNEL)?;
        }
        // All allocations and translations are validated before the first
        // publication data changes. Nothing fallible follows this write.
        self.block_list.with_bytes_mut(|bytes| {
            if bytes.len() < entries.len() * 4 || bytes.iter().any(|&v| v != 0) {
                return Err(EBUSY);
            }
            for (index, entry) in entries.iter().enumerate() {
                bytes[index * 4..index * 4 + 4].copy_from_slice(&entry.to_le_bytes());
            }
            Ok(())
        })?;
        self.scratch_aliases = aliases;
        self.scratch = backing;
        self.scratch_plan = Some(plan);
        fence(Ordering::SeqCst);
        dev_info!(dev.as_ref(),
            "G18 compute scratch: 5 MiB retained, 5 GPU block entries, 320 translations verified; pool unpublished\n");
        Ok(())
    }

    /// Alias the backed directory pages, block list and scratch blocks into the
    /// job's VM at their unchanged GPU addresses. The USC walks the descriptor's
    /// directory/block-list/block pointers in the job's context: stage27 took a
    /// GMMU fault (vm_slot 3, Unmapped read of the block list 0x7004000000)
    /// while these existed only in the kernel lower VM. Holes stay unmapped.
    /// Call after the backing exists and before the job's roots are published.
    pub(crate) fn alias_into_process(
        &mut self, dev: &AsahiDevice, uat: &mmu::Uat, vm: &mmu::Vm,
    ) -> Result<()> {
        let plan = self.scratch_plan.ok_or(EINVAL)?;
        if !self.process_aliases.is_empty() { return Err(EBUSY); }
        let block_list_bytes = (u64::from(self.geometry.block_count) * 4)
            .checked_add(mmu::UAT_PGSZ as u64 - 1).ok_or(EOVERFLOW)? & !(mmu::UAT_PGSZ as u64 - 1);
        let mut aliases = KVec::new();
        let mut ranges = KVec::new();
        for (index, page) in self.directory.iter_mut().enumerate() {
            let gpu = DIRECTORY_BASE + self.layout.page_offset(index as u32).ok_or(EINVAL)?;
            aliases.push(page.map_alias_at(vm, gpu, mmu::PROT_GPU_SHARED_RW)?, GFP_KERNEL)?;
            ranges.push((gpu, mmu::UAT_PGSZ as u64), GFP_KERNEL)?;
        }
        aliases.push(self.block_list.map_alias_at(vm, BLOCK_LIST_BASE, mmu::PROT_GPU_SHARED_RW)?,
                     GFP_KERNEL)?;
        ranges.push((BLOCK_LIST_BASE, block_list_bytes), GFP_KERNEL)?;
        for (index, block) in self.scratch.iter_mut().enumerate() {
            let gpu = BLOCK_DATA_BASE + index as u64 * BLOCK_DATA_STRIDE;
            aliases.push(block.map_alias_at(vm, gpu, mmu::PROT_GPU_SHARED_RW)?, GFP_KERNEL)?;
            ranges.push((gpu, plan.block_bytes), GFP_KERNEL)?;
        }
        // Every job-context page must reach the same memory as the kernel
        // lower VM view that the descriptor and block list were encoded from.
        let mut pages = 0usize;
        for &(gpu, bytes) in ranges.iter() {
            for offset in (0..bytes).step_by(mmu::UAT_PGSZ) {
                if vm.translate_iova(gpu + offset)?
                    != uat.kernel_lower_vm().translate_iova(gpu + offset)? {
                    return Err(EFAULT);
                }
                pages += 1;
            }
        }
        fence(Ordering::SeqCst);
        self.process_aliases = aliases;
        dev_info!(dev.as_ref(),
            "G18 UMA pool aliased into the job VM: {} mappings, {} pages verified (directory {:#x}, block list {:#x}, blocks {:#x}+{}x{:#x})\n",
            ranges.len(), pages, DIRECTORY_BASE, BLOCK_LIST_BASE, BLOCK_DATA_BASE,
            self.scratch.len(), BLOCK_DATA_STRIDE);
        Ok(())
    }

    /// Prepare this boot's single compute pool, empty unless backing was
    /// explicitly prepared first. Ownership must survive firmware access.
    /// Runtime pool growth is not implemented.
    pub(crate) fn prepare_binding(&mut self) -> Result<G18UmaPoolBinding> {
        if self.encoded { return Err(EBUSY); }
        let counter_address = self.preempt_counter.iova();
        if counter_address == 0 || counter_address & 7 != 0 { return Err(EINVAL); }
        let counter = self.preempt_counter.with_bytes_mut(|bytes| {
            if bytes.len() != 8 { return Err(EINVAL); }
            let value = u64::from_le_bytes(bytes.try_into().map_err(|_| EINVAL)?);
            if value != 0 { return Err(EBUSY); }
            Ok(value)
        })?;
        let encoded = g18_uma::encode_g18_pool_state(&g18_uma::G18UmaPoolState {
            // Host identities are local to this one fixed-VA pool and boot.
            // The statistics ID and FList generation have different meanings.
            statistics_id: 1,
            // Native halUMAGetGrowType returns 2; implicit engine init also
            // stores 2. This records the protocol, not an implemented grow path.
            grow_engine: 2,
            priority: 2,
            directory_gpu: DIRECTORY_BASE + self.layout.gpu_base_offset,
            directory_stride: self.geometry.directory_bytes_per_usc,
            chunk_counters: self.scratch_plan.map_or([0, 0], |plan| plan.chunk_counters),
            block_list_gpu: BLOCK_LIST_BASE,
            populated_blocks: self.scratch_plan.map_or(0, |plan| plan.block_count),
            block_units_64k: (self.geometry.block_bytes >> 16).try_into()?,
            // Native base configureDevice initializes this mode to zero.
            sma_mode: 0,
            preempt_counter_gpu: counter_address,
            preempt_counter: counter,
            compute: true,
            // Fresh Pool::init clears +0x1f0; prepareBufferResources writes 1.
            policy_value: 1,
        });
        self.state.with_bytes_mut(|bytes| {
            if bytes.iter().enumerate().any(|(i, &v)| v != if (8..12).contains(&i) { 0xff } else { 0 }) {
                return Err(EBUSY);
            }
            bytes.copy_from_slice(&encoded);
            Ok(())
        })?;
        fence(Ordering::SeqCst);
        self.encoded = true;
        Ok(G18UmaPoolBinding { state: self.state.iova(), generation: 0, slot: G18_UMA_HARDWARE_SLOT })
    }

    /// Native `prepareLocked` 0xfffffe0008c8f308..0x8c8f390 on a pool's first
    /// use: store the hardware slot at state+0x08 (only when it differs), then
    /// `halLoadUMADesc` packs state[..0x7c] with the four SMA words into
    /// table[slot]. Plain host stores; native has no barrier or cache
    /// maintenance here. The submit's tag15 names the pool only afterwards, so
    /// call this before the binding is published. Refuses an occupied slot, a
    /// foreign binding or a second load.
    pub(crate) fn load_hardware_descriptor(
        &mut self,
        dev: &AsahiDevice,
        table: &mut MappedObject,
        binding: G18UmaPoolBinding,
        device_config: [u64; 4],
    ) -> Result<[u64; g18_uma::G18_UMA_DESCRIPTOR_BYTES / 8]> {
        const DESCRIPTOR: usize = g18_uma::G18_UMA_DESCRIPTOR_BYTES;
        if !self.encoded || self.descriptor_loaded { return Err(EBUSY); }
        if binding.state != self.state.iova() || binding.slot != G18_UMA_HARDWARE_SLOT
            || binding.generation != 0 || device_config == [0; 4] {
            return Err(EINVAL);
        }
        let slot = binding.slot as usize;
        if slot >= UMA_TABLE_SLOTS { return Err(EINVAL); }
        let mut packed = [0u8; g18_uma::G18_UMA_STATE_BYTES];
        self.state.with_bytes_mut(|bytes| {
            // The encoded state must still carry the native unassigned slot.
            if bytes.len() < STATE_BYTES || bytes[8..12] != u32::MAX.to_le_bytes() {
                return Err(EBUSY);
            }
            packed.copy_from_slice(&bytes[..g18_uma::G18_UMA_STATE_BYTES]);
            Ok(())
        })?;
        packed[8..12].copy_from_slice(&binding.slot.to_le_bytes());
        let words = g18_uma::pack_g18_uma_descriptor(&packed, device_config);
        let offset = slot * DESCRIPTOR;
        table.with_bytes_mut(|out| {
            if out.len() != UMA_TABLE_SLOTS * DESCRIPTOR
                || out[offset..offset + DESCRIPTOR].iter().any(|&v| v != 0) {
                return Err(EBUSY);
            }
            Ok(())
        })?;
        // All checks passed. A later failure still aborts before tag15.
        // Native order: state+0x08 first, then the sixteen descriptor words.
        self.state.with_bytes_mut(|bytes| {
            bytes[8..12].copy_from_slice(&binding.slot.to_le_bytes());
            Ok(())
        })?;
        table.with_bytes_mut(|out| {
            for (index, word) in words.iter().enumerate() {
                let at = offset + index * 8;
                out[at..at + 8].copy_from_slice(&word.to_le_bytes());
            }
            Ok(())
        })?;
        fence(Ordering::SeqCst);
        self.descriptor_loaded = true;
        dev_info!(dev.as_ref(),
            "G18 UMA descriptor loaded: slot={} entry={:#x} state={:#x} state+0x08={} sma={:x?} words={:x?}; tag15 unpublished\n",
            binding.slot, table.iova() + offset as u64, binding.state, binding.slot,
            device_config, words);
        Ok(words)
    }

    pub(crate) fn directory_page_high(&self, index: usize) -> Option<u64> {
        self.directory.get(index).map(MappedObject::iova)
    }

    pub(crate) fn view(&self) -> G18UmaPoolView {
        G18UmaPoolView {
            directory_base: DIRECTORY_BASE, reservation_bytes: self.layout.reservation_bytes,
            stride: self.geometry.directory_bytes_per_usc, page_count: self.directory.len(),
            gpu_base_offset: self.layout.gpu_base_offset, state_high: self.state.iova(),
            block_list_high: self.block_list.iova(), block_list_low: BLOCK_LIST_BASE,
        }
    }
}
