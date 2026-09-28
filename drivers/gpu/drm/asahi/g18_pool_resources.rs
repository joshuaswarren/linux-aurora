// SPDX-License-Identifier: GPL-2.0-only OR MIT
//! Owned initial G18 UMA pool resources. No firmware binding is published here.
//!
//! Flags 3 use a sparse directory: a page at each USC stride plus the final
//! page, with hardware addressing starting one KiB before the first page end.
//! Initial mappings reserve their trailing holes using the existing fixed
//! mapping guards. Populating those holes for runtime growth is not implemented.
//! The state must be fully encoded before a caller may bind or submit work.

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
const STATE_BYTES: usize = g18_uma::G18_POOL_STATE_BYTES;

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
    aliases: KVec<mmu::KernelMapping>,
    directory: KVec<MappedObject>,
    block_list: MappedObject,
    state: MappedObject,
    geometry: g18_uma::G18UmaGeometry,
    layout: g18_uma::G18UmaSparseDirectory,
    encoded: bool,
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
        dev_info!(dev.as_ref(),
            "G18 UMA sparse owner: reserved={:#x} backing_pages={} holes={} bias={:#x} aliases=verified guards=verified unpublished\n",
            layout.reservation_bytes, layout.initial_page_count, holes, layout.gpu_base_offset);
        Ok(Self { aliases, directory, block_list, state, geometry, layout, encoded: false })
    }

    /// Prepare this boot's single empty compute pool. The caller may publish
    /// the returned identity once; ownership must survive firmware access.
    /// No committed private blocks or runtime growth are provided yet.
    pub(crate) fn prepare_binding(&mut self) -> Result<G18UmaPoolBinding> {
        if self.encoded { return Err(EBUSY); }
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
            chunk_counters: [0, 0],
            block_list_gpu: BLOCK_LIST_BASE,
            populated_blocks: 0,
            block_units_64k: (self.geometry.block_bytes >> 16).try_into()?,
            // Native base configureDevice initializes this mode to zero.
            sma_mode: 0,
            preempt_counter_gpu: 0,
            preempt_counter: 0,
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
        Ok(G18UmaPoolBinding { state: self.state.iova(), generation: 0, slot: 0 })
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
