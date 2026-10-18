// SPDX-License-Identifier: GPL-2.0-only OR MIT

//! GPUVM mapping dispatch. Only directly owned tables expose a physical root.

use super::{MappingBacking, Prot, UatPageTable};
use core::ops::Range;
use kernel::{addr::PhysicalAddr, prelude::*};

/// Diagnostic size of one batched mapping.
#[derive(Clone, Copy, Default)]
pub(crate) struct BatchStats {
    pub(crate) chunks: u64,
    pub(crate) pages: u64,
    pub(crate) operations: u64,
}

pub(super) enum PageTable {
    Direct(UatPageTable),
    #[cfg(CONFIG_ARM64)]
    Sptm(super::sptm_pgtable::PageTable),
}

impl PageTable {
    pub(super) fn removal_log(&self) -> Option<kernel::sync::Arc<super::coverage::RemovalLog>> {
        match self {
            Self::Direct(_) => None,
            #[cfg(CONFIG_ARM64)]
            Self::Sptm(table) => Some(table.removal_log()),
        }
    }

    pub(super) fn visit_runs(&self, range: Range<u64>, visit: impl FnMut(u64, u64, u8) -> Result) -> Result<bool> {
        match self {
            Self::Direct(_) => { let _ = (range, visit); Err(ENOTSUPP) }
            #[cfg(CONFIG_ARM64)]
            Self::Sptm(table) => table.visit_runs(range, visit),
        }
    }
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

    /// Map VA-contiguous chunks of one mapping. SPTM tables submit them as one
    /// batched plan; direct tables keep their per-chunk path.
    pub(super) fn map_chunks(&mut self, chunks: &[(Range<u64>, PhysicalAddr, Prot)],
        backing: &MappingBacking) -> Result<Option<BatchStats>>
    {
        match self {
            Self::Direct(table) => {
                for (range, physical, prot) in chunks {
                    table.map_pages(range.clone(), *physical, *prot, false)?;
                }
                Ok(None)
            }
            #[cfg(CONFIG_ARM64)]
            Self::Sptm(table) => table.map_chunks(chunks, backing).map(Some),
        }
    }

    #[cfg(CONFIG_ARM64)]
    pub(super) fn map_tiled(&mut self, range: Range<u64>,
        tile: &kernel::sync::Arc<super::sptm_pgtable::SparseTile>, prot: Prot) -> Result
    {
        match self {
            Self::Direct(_) => Err(ENOTSUPP),
            Self::Sptm(table) => table.map_tiled(range, tile, prot),
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
