// SPDX-License-Identifier: GPL-2.0-only

//! SPTM address spaces and their confirmed mapping shadow.
//!
//! Each run shares GPUVM's counted SG lease. Reserving the shadow before
//! publication makes uncertain operations retain both the VA and its backing,
//! even when the GPUVA transaction cannot install its normal owner. Successful
//! unmap releases those references before GPUVM attempts idle page reclaim.

use super::{sptm::{ContextState, MapPages, Protection, PAGE}, sptm_transport::{AddressSpace, Transport}, MappingBacking};
use crate::{g17::status::VmStatus, pgtable::{prot, Prot}};
use core::ops::Range;
use kernel::{addr::PhysicalAddr, prelude::*, rbtree::{RBTree, RBTreeNode, RBTreeNodeReservation}, sync::Arc};

#[derive(Clone)]
struct Run {
    end: u64,
    physical: u64,
    prot: Prot,
    repeat: bool,
    _backing: Option<MappingBacking>,
}

impl Run {
    fn physical_at(&self, start: u64, address: u64) -> u64 {
        self.physical + if self.repeat { 0 } else { address - start }
    }
}

fn protection(value: Prot) -> Result<Protection> {
    Ok(if value == prot::PROT_FW_PRIV_RW { Protection::FirmwarePrivateReadWrite }
        else if value == prot::PROT_GPU_SHARED_RW { Protection::GpuReadWrite }
        else if value == prot::PROT_GPU_SHARED_RO { Protection::GpuReadOnly }
        else if value == prot::PROT_GPU_FW_SHARED_RW { Protection::SharedReadWrite }
        else if value == prot::PROT_GPU_RO_FW_PRIV_RW { Protection::FirmwareWriteGpuRead }
        else if value == prot::PROT_FW_SHARED_RW { Protection::FirmwareReadWrite }
        else if value == prot::PROT_FW_SHARED_RO { Protection::FirmwareReadOnly }
        else if value == prot::PROT_FW_MMIO_RW { Protection::FirmwareMmioReadWrite }
        else if value == prot::PROT_FW_MMIO_RO { Protection::FirmwareMmioReadOnly }
        else { return Err(ENOTSUPP); })
}

pub(super) struct PageTable {
    space: AddressSpace,
    runs: RBTree<u64, Run>,
    status: Arc<VmStatus>,
}

impl PageTable {
    pub(super) fn new(transport: &Arc<Transport>, status: Arc<VmStatus>) -> Result<Self> {
        Ok(Self { space: AddressSpace::private(transport)?, runs: RBTree::new(), status })
    }

    pub(super) fn new_global(transport: &Arc<Transport>, status: Arc<VmStatus>) -> Self {
        Self { space: AddressSpace::global(transport), runs: RBTree::new(), status }
    }

    pub(super) fn ensure_certain(&self) -> Result {
        match self.space.state() {
            ContextState::Unbound | ContextState::Bound(_) => Ok(()),
            _ => Err(EIO),
        }
    }

    fn report_uncertain(&self) {
        if matches!(self.space.state(), ContextState::Uncertain(_)) {
            self.status.record(EIO);
        }
    }

    pub(super) fn validate_range(&self, range: Range<u64>) -> Result {
        self.ensure_certain()?;
        if range.start >= range.end || (range.start | range.end) & (PAGE - 1) != 0
            || range.start < self.space.range().start || range.end > self.space.range().end { return Err(EINVAL); }
        Ok(())
    }

    fn run_at(&self, address: u64) -> Option<(u64, &Run)> {
        let start = match self.runs.cursor_lower_bound(&address) {
            Some(cursor) if *cursor.current().0 == address => Some(address),
            Some(cursor) => cursor.peek_prev().map(|(key, _)| *key),
            None => self.runs.cursor_back().map(|cursor| *cursor.current().0),
        }?;
        let run = self.runs.get(&start)?;
        (address < run.end).then_some((start, run))
    }

    pub(super) fn prepare_map(&self, range: Range<u64>) -> Result {
        self.validate_range(range.clone())?;
        if self.run_at(range.start).is_some()
            || self.runs.cursor_lower_bound(&range.start)
                .is_some_and(|cursor| *cursor.current().0 < range.end) { return Err(EBUSY); }
        Ok(())
    }

    pub(super) fn map_pages(&mut self, range: Range<u64>, physical: PhysicalAddr,
        prot: Prot, repeat: bool, backing: Option<&MappingBacking>) -> Result
    {
        self.prepare_map(range.clone())?;
        // Only firmware MMIO in the global window can lack a pinned RAM
        // owner. The provider independently validates its physical allowlist.
        let io = prot == prot::PROT_FW_MMIO_RW || prot == prot::PROT_FW_MMIO_RO;
        if io {
            if !matches!(self.space, AddressSpace::Global { .. }) || backing.is_some() || repeat {
                return Err(EINVAL);
            }
        } else if backing.is_none() {
            return Err(EINVAL);
        }
        let protection = protection(prot)?;
        let count = (range.end - range.start) / PAGE;
        if physical & (PAGE - 1) != 0 { return Err(EINVAL); }
        physical.checked_add(if repeat { PAGE } else { range.end - range.start }).ok_or(EOVERFLOW)?;
        let map_pages = if repeat { MapPages::Repeated { physical, count } }
            else { MapPages::Contiguous { physical, count } };
        let node = RBTreeNode::new(range.start, Run {
            end: range.end, physical, prot, repeat, _backing: backing.cloned(),
        }, GFP_KERNEL)?;
        match self.space.map(range.start, map_pages, protection) {
            Ok(()) => { self.runs.insert(node); Ok(()) }
            Err(failure) => {
                if failure.may_have_mapped { self.runs.insert(node); }
                self.report_uncertain();
                Err(failure.error)
            }
        }
    }

    /// Rollback/reclaim can skip holes; normal unmap must be wholly covered.
    /// All shadow splits and command storage are reserved before publication.
    pub(super) fn unmap_pages(&mut self, range: Range<u64>, allow_holes: bool) -> Result {
        self.validate_range(range.clone())?;
        let mut touched = KVec::new();
        let mut ranges = KVec::new();
        let mut at = range.start;
        while at < range.end {
            let Some((start, run)) = self.run_at(at) else {
                if !allow_holes { return Err(EFAULT); }
                at = self.runs.cursor_lower_bound(&at)
                    .map_or(range.end, |cursor| (*cursor.current().0).min(range.end));
                continue;
            };
            let end = run.end.min(range.end);
            ranges.push(at..end, GFP_KERNEL)?;
            touched.push((start, run.clone()), GFP_KERNEL)?;
            at = end;
        }
        if touched.is_empty() { return Ok(()); }
        let mut spare = KVec::with_capacity(2, GFP_KERNEL)?;
        // Only the first and last runs can have surviving fragments.
        for _ in 0..2 { spare.push(RBTreeNodeReservation::<u64, Run>::new(GFP_KERNEL)?, GFP_KERNEL)?; }
        if let Err(error) = self.space.unmap_ranges(&ranges) {
            self.report_uncertain();
            return Err(error);
        }
        for (start, run) in touched {
            self.runs.remove(&start);
            if start < range.start {
                let left = Run { end: range.start, ..run.clone() };
                self.runs.insert(spare.pop().expect("reserved left fragment").into_node(start, left));
            }
            if run.end > range.end {
                let right = Run { physical: run.physical_at(start, range.end), ..run };
                self.runs.insert(spare.pop().expect("reserved right fragment").into_node(range.end, right));
            }
        }
        Ok(())
    }

    pub(super) fn covers_range(&self, range: Range<u64>, read: bool, write: bool) -> Result<bool> {
        self.ensure_certain()?;
        if self.validate_range(range.clone()).is_err() { return Ok(false); }
        let mut at = range.start;
        while at < range.end {
            let Some((_, run)) = self.run_at(at) else { return Ok(false); };
            if !run.prot.allows_gpu(read, write) { return Ok(false); }
            at = run.end;
        }
        Ok(true)
    }

    pub(super) fn translate_iova(&self, address: u64) -> Result<u64> {
        self.ensure_certain()?;
        let (start, run) = self.run_at(address).ok_or(EFAULT)?;
        Ok(run.physical_at(start, address & !(PAGE - 1)) + (address & (PAGE - 1)))
    }

    pub(super) fn bind(&mut self, context: u8) -> Result {
        self.ensure_certain()?;
        let result = self.space.bind(context);
        self.report_uncertain();
        result
    }

    pub(super) fn context_mask(&self) -> Result<u64> {
        self.ensure_certain()?;
        Ok(match self.space.state() {
            ContextState::Bound(id) => 1u64 << id,
            _ => 0,
        })
    }

    pub(super) fn sync_context(&mut self) -> Result<u64> {
        let mask = self.context_mask()?;
        if mask == 0 { return Err(ENOENT); }
        let result = self.space.sync_context();
        self.report_uncertain();
        result.map(|()| mask)
    }

    /// The execution owner must prove every queue using the context retired.
    pub(super) fn unbind(&mut self) -> Result {
        let result = self.space.unbind();
        self.report_uncertain();
        result
    }
}

impl Drop for PageTable {
    fn drop(&mut self) {
        // Only the execution owner can withdraw a live context after all its
        // queues retire. Never infer retirement from the final host reference.
        if self.space.state() == ContextState::Unbound {
            loop {
                let next = self.runs.iter().next().map(|(start, run)| (*start, run.end));
                let Some((start, end)) = next else {
                    if self.space.destroy().is_err() {
                        pr_err!("SPTM: empty address space could not be released\n");
                    }
                    return;
                };
                if self.space.unmap(start, (end - start) / PAGE).is_err() {
                    break;
                }
                self.runs.remove(&start);
            }
        }
        self.status.record(EIO);
        core::mem::forget(core::mem::replace(&mut self.runs, RBTree::new()));
        pr_err!("SPTM: address space retained without confirmed retirement\n");
    }
}
