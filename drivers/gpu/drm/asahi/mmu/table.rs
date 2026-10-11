// SPDX-License-Identifier: GPL-2.0-only

//! GPUVM mapping dispatch. Only directly owned tables expose a physical root.

use super::{MappingBacking, Prot, UatPageTable};
use core::ops::Range;
use kernel::{addr::PhysicalAddr, prelude::*};

pub(super) enum PageTable {
    Direct(UatPageTable),
    #[cfg(CONFIG_ARM64)]
    Sptm(super::sptm_pgtable::PageTable),
}

impl PageTable {
    pub(super) fn direct_root(&self) -> Option<u64> {
        match self {
            Self::Direct(table) => Some(table.ttb()),
            #[cfg(CONFIG_ARM64)]
            Self::Sptm(_) => None,
        }
    }

    pub(super) fn map_pages(&mut self, range: Range<u64>, physical: PhysicalAddr,
        prot: Prot, repeat: bool, backing: Option<&MappingBacking>) -> Result
    {
        match self {
            Self::Direct(table) => table.map_pages(range, physical, prot, repeat),
            #[cfg(CONFIG_ARM64)]
            Self::Sptm(table) => table.map_pages(range, physical, prot, repeat, backing),
        }
    }

    pub(super) fn prepare_map(&mut self, range: Range<u64>) -> Result {
        match self {
            Self::Direct(table) => table.prepare_map(range),
            #[cfg(CONFIG_ARM64)]
            Self::Sptm(table) => table.prepare_map(range),
        }
    }

    pub(super) fn alloc_pages(&mut self, range: Range<u64>) -> Result {
        match self {
            Self::Direct(table) => table.alloc_pages(range),
            #[cfg(CONFIG_ARM64)]
            Self::Sptm(table) => table.validate_range(range),
        }
    }

    pub(super) fn unmap_pages(&mut self, range: Range<u64>) -> Result {
        match self {
            Self::Direct(table) => table.unmap_pages(range),
            #[cfg(CONFIG_ARM64)]
            Self::Sptm(table) => table.unmap_pages(range, false),
        }
    }

    pub(super) fn discard_partial_map(&mut self, range: Range<u64>) -> Result {
        match self {
            Self::Direct(table) => table.discard_partial_map(range),
            #[cfg(CONFIG_ARM64)]
            Self::Sptm(table) => table.unmap_pages(range, true),
        }
    }

    pub(super) fn covers_range(&mut self, range: Range<u64>, read: bool, write: bool) -> Result<bool> {
        match self {
            Self::Direct(table) => table.covers_range(range, read, write),
            #[cfg(CONFIG_ARM64)]
            Self::Sptm(table) => table.covers_range(range, read, write),
        }
    }

    pub(super) fn translate_iova(&mut self, address: u64) -> Result<u64> {
        match self {
            Self::Direct(table) => table.translate_iova(address),
            #[cfg(CONFIG_ARM64)]
            Self::Sptm(table) => table.translate_iova(address),
        }
    }

    pub(super) fn reprot_pages(&mut self, range: Range<u64>, prot: Prot) -> Result {
        match self {
            Self::Direct(table) => table.reprot_pages(range, prot),
            // No atomic permission-change service is available yet. In
            // particular, do not emulate it with an unmap of live firmware.
            #[cfg(CONFIG_ARM64)]
            Self::Sptm(_) => Err(ENOTSUPP),
        }
    }

    pub(super) fn leaf_entry(&mut self, address: u64) -> Result<u64> {
        match self {
            Self::Direct(table) => table.leaf_entry(address),
            // A shadow is not a hardware PTE readback.
            #[cfg(CONFIG_ARM64)]
            Self::Sptm(_) => Err(ENOTSUPP),
        }
    }

    #[cfg(CONFIG_DEV_COREDUMP)]
    pub(super) fn dump_pages(&mut self, range: Range<u64>) -> Result<KVVec<crate::pgtable::DumpedPage>> {
        match self {
            Self::Direct(table) => table.dump_pages(range),
            #[cfg(CONFIG_ARM64)]
            Self::Sptm(_) => Err(ENOTSUPP),
        }
    }
}
