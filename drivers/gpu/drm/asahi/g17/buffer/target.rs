// SPDX-License-Identifier: GPL-2.0-only AND MIT

//! Rotating tile-map/heap metadata and the retained tail-pointer cache.
//! Larger targets replace only the objects that need more capacity. Every old
//! backing keeps the exact commands that still reference it until retirement.

use super::compact_alias;
use crate::g17::{
    fw::render::RenderPass,
    object::{CpuMap, KernelObject},
};
use crate::{
    driver::AsahiDevice,
    hw::t8140::{queues, resources as cfg},
    mmu,
};
use kernel::prelude::*;

const DEPTH: usize = queues::RENDER_DEPTH as usize;

fn aligned(value: usize, alignment: usize) -> Result<usize> {
    value
        .checked_add(alignment - 1)
        .map(|v| v & !(alignment - 1))
        .ok_or(EOVERFLOW)
}

/// Per-command geometry, independent of the capacity already allocated.
#[derive(Copy, Clone, Debug)]
pub(crate) struct Layout {
    pub(crate) tilemap_size: usize,
    pub(crate) layer_offset: usize,
    pub(crate) heap_offset: usize,
    pub(crate) used: usize,
    pub(crate) tpc_size: usize,
    pub(crate) tvb_blocks: usize,
    m5: Option<(crate::g17::fw::m5::target::TargetSlices, [u64; 3])>,
}
impl Layout {
    pub(crate) fn new(pass: &RenderPass, clusters: u32) -> Result<Self> {
        let width = pass.width as usize;
        let height = pass.height as usize;
        let layers = pass.layers as usize;
        let (uw, uh) = (pass.utile_width as usize, pass.utile_height as usize);
        if width == 0
            || height == 0
            || layers == 0
            || clusters == 0
            || uw == 0
            || uh == 0
            || 32 % uw != 0
            || 32 % uh != 0
        {
            return Err(EINVAL);
        }
        let utiles = (32 / uw) * (32 / uh);
        let tiles_x = width.div_ceil(32);
        let tiles_y = height.div_ceil(32);
        let macro_x = aligned(tiles_x.div_ceil(4), 4)?;
        let macro_y = aligned(tiles_y.div_ceil(4), 4)?;
        let macro_tiles = macro_x.checked_mul(macro_y).ok_or(EOVERFLOW)?;
        let tilemap_size = aligned(
            5usize
                .checked_mul(macro_tiles)
                .and_then(|v| v.checked_mul(utiles))
                .ok_or(EOVERFLOW)?,
            4,
        )?
        .checked_mul(16)
        .and_then(|v| v.checked_mul(layers))
        .ok_or(EOVERFLOW)?;
        let tpc_size = 8usize
            .checked_mul(utiles)
            .and_then(|v| v.checked_mul(macro_tiles))
            .map(|v| v / 4 * 4)
            .and_then(|v| v.checked_mul(16))
            .and_then(|v| v.checked_mul(layers))
            .and_then(|v| v.checked_mul(clusters as usize))
            .ok_or(EOVERFLOW)?;
        let layer_offset = aligned(tilemap_size, 0x1000)?;
        let heap_offset = layer_offset
            .checked_add(if layers > 1 { 0x100 } else { 0 })
            .ok_or(EOVERFLOW)?;
        let used = heap_offset.checked_add(0x200).ok_or(EOVERFLOW)?;
        let tiles = tiles_x.checked_mul(tiles_y).ok_or(EOVERFLOW)?;
        let mut tvb_blocks = aligned(tiles.div_ceil(128), 8)?.max(8);
        if clusters > 1 {
            tvb_blocks = tvb_blocks.max(
                7usize
                    .checked_add(2usize.checked_mul(layers).ok_or(EOVERFLOW)?)
                    .ok_or(EOVERFLOW)?,
            );
        }
        tvb_blocks = tvb_blocks.max(cfg::TVB_INITIAL_PAGES.len());
        Ok(Self {
            tilemap_size,
            layer_offset,
            heap_offset,
            used,
            tpc_size,
            tvb_blocks,
            m5: None,
        })
    }
    pub(crate) fn with_m5_slices(mut self, pass: &RenderPass, groups: u32,
        segments: u32, parameter_bytes: usize) -> Result<Self> {
        let slices = crate::g17::fw::m5::target::TargetSlices::new(
            self.tilemap_size, self.tpc_size, pass.layers, groups, segments, parameter_bytes)
            .ok_or(EINVAL)?;
        let control = crate::g17::fw::m5::target::cluster_control(groups, u64::from(segments))
            .ok_or(EINVAL)?;
        self.m5 = Some((slices, control));
        Ok(self)
    }
    pub(crate) fn is_m5(&self) -> bool { self.m5.is_some() }
    pub(crate) fn tpc_capacity(&self) -> Result<usize> {
        match self.m5 {
            Some((slices, _)) => aligned(slices.bytes, mmu::UAT_PGSZ),
            None => Ok(aligned(self.tpc_size, mmu::UAT_PGSZ)?.max(cfg::TPC_MIN)),
        }
    }
    pub(crate) fn record_stride(&self) -> Result<usize> {
        let bytes = aligned(self.used, mmu::UAT_PGSZ)?;
        Ok(if self.m5.is_some() { bytes } else { bytes.max(cfg::TARGET_RECORD_MIN) })
    }
    fn check_capacity(&self, tpc: usize, stride: usize) -> Result {
        if self.m5.is_none() && (tpc > cfg::TPC_MAX || stride > cfg::TARGET_RECORD_MAX) {
            return Err(ERANGE);
        }
        stride.checked_mul(DEPTH).ok_or(EOVERFLOW)?;
        Ok(())
    }
}

struct Object {
    address: u64,
    _mapping: mmu::KernelMapping,
    backing: KernelObject,
}
impl Object {
    fn new(dev: &AsahiDevice, vm: &mmu::Vm, size: usize, m5: bool) -> Result<Self> {
        let backing = KernelObject::backing(dev, size, CpuMap::WriteCombined)?;
        let prot = mmu::PROT_GPU_FW_SHARED_RW;
        let mapping = if m5 {
            backing.map_range(vm, 0..size, super::BlockGeometry::M5_COMPACT,
                cfg::TVB_PAGE, prot, mmu::UAT_PGSZ)?
        } else { compact_alias(&backing, vm, 0..size, cfg::TVB_PAGE, prot, mmu::UAT_PGSZ)? };
        Ok(Self {
            address: mapping.iova(),
            _mapping: mapping,
            backing,
        })
    }
}
struct Retired {
    _object: Object,
    owners: [Option<u64>; DEPTH],
}

#[derive(Copy, Clone, Debug)]
pub(crate) struct Addresses {
    pub(crate) tilemap: u64,
    pub(crate) layer: u64,
    pub(crate) heap: u64,
}

pub(crate) struct Target {
    records: Object,
    tpc: Object,
    stride: usize,
    slots: [Option<u64>; DEPTH],
    retired: KVec<Retired>,
    m5: bool,
}
impl Target {
    /// Keep this allocation frame separate from the rest of the render graph.
    #[inline(never)]
    pub(crate) fn new(dev: &AsahiDevice, vm: &mmu::Vm, layout: &Layout) -> Result<KBox<Self>> {
        let tpc = layout.tpc_capacity()?;
        let stride = layout.record_stride()?;
        layout.check_capacity(tpc, stride)?;
        let m5 = layout.m5.is_some();
        let records = Object::new(dev, vm, stride * DEPTH, m5)?;
        let tpc = Object::new(dev, vm, tpc, m5)?;
        KBox::new(
            Self {
                records,
                tpc,
                stride,
                slots: [None; DEPTH],
                retired: KVec::new(),
                m5,
            },
            GFP_KERNEL,
        )
        .map_err(Into::into)
    }
    pub(crate) fn tpc_va(&self) -> u64 {
        self.tpc.address
    }

    pub(crate) fn m5_extra(&self, layout: &Layout)
        -> Result<crate::g17::fw::m5::render_registers::TaExtra> {
        if !self.m5 { return Err(EINVAL); }
        let (slices, control) = layout.m5.ok_or(EINVAL)?;
        if slices.bytes > self.tpc.backing.size() { return Err(ERANGE); }
        let base = self.tpc.address.checked_sub(super::BlockGeometry::M5_COMPACT.start)
            .ok_or(EINVAL)?;
        let mut pointers = [0; 8];
        for index in 0..8 {
            if slices.sizes[index] != 0 {
                pointers[index] = base.checked_add(slices.offsets[index] as u64).ok_or(EOVERFLOW)?;
            }
        }
        Ok(crate::g17::fw::m5::render_registers::TaExtra {
            d11c8: pointers[1], d11d0: pointers[2], d11e8: pointers[4], d11f0: pointers[5],
            d11f8: pointers[6], d1208: pointers[7], d1210: control[0], d1218: control[1],
            d1220: control[2], d1590: 0, d1591: 0,
        })
    }

    /// Only the span consumed by this render is cleared. A small render does
    /// not clear a previous large target's complete retained record capacity.
    pub(crate) fn prepare(&mut self, ordinal: u64, layout: &Layout) -> Result<Addresses> {
        let slot = ordinal as usize % DEPTH;
        if self.m5 != layout.m5.is_some() { return Err(EINVAL); }
        if self.slots[slot].is_some_and(|owner| owner != ordinal) {
            return Err(EBUSY);
        }
        if layout.used > self.stride {
            return Err(ERANGE);
        }
        let offset = slot * self.stride;
        let span = aligned(layout.used, mmu::UAT_PGSZ)?.min(self.stride);
        self.records
            .backing
            .bytes_mut()
            .get_mut(offset..offset + span)
            .ok_or(ERANGE)?
            .fill(0);
        self.slots[slot] = Some(ordinal);
        let tilemap = self
            .records
            .address
            .checked_add(offset as u64)
            .ok_or(EOVERFLOW)?;
        Ok(Addresses {
            tilemap,
            layer: tilemap
                .checked_add(layout.layer_offset as u64)
                .ok_or(EOVERFLOW)?,
            heap: tilemap
                .checked_add(layout.heap_offset as u64)
                .ok_or(EOVERFLOW)?,
        })
    }

    /// Growth never shrinks a live allocation. A successfully replaced TPC
    /// stays enlarged even if allocating the following record object fails.
    pub(crate) fn ensure_capacity(
        &mut self,
        dev: &AsahiDevice,
        vm: &mmu::Vm,
        layout: &Layout,
        owners: &[Option<u64>; DEPTH],
    ) -> Result<bool> {
        let tpc = layout.tpc_capacity()?;
        let stride = layout.record_stride()?;
        if self.m5 != layout.m5.is_some() { return Err(EINVAL); }
        layout.check_capacity(tpc, stride)?;
        let mut grew = false;
        if tpc > self.tpc.backing.size() {
            Self::replace(&mut self.retired, &mut self.tpc, dev, vm, tpc, owners, self.m5)?;
            grew = true;
        }
        if stride > self.stride {
            Self::replace(
                &mut self.retired,
                &mut self.records,
                dev,
                vm,
                stride * DEPTH,
                owners,
                self.m5,
            )?;
            self.stride = stride;
            self.slots = [None; DEPTH];
            grew = true;
        }
        Ok(grew)
    }
    fn replace(
        retired: &mut KVec<Retired>,
        old: &mut Object,
        dev: &AsahiDevice,
        vm: &mmu::Vm,
        size: usize,
        owners: &[Option<u64>; DEPTH],
        m5: bool,
    ) -> Result {
        let retain = owners.iter().any(Option::is_some);
        if retain {
            retired.reserve(1, GFP_KERNEL)?;
        }
        let new = Object::new(dev, vm, size, m5)?;
        let previous = core::mem::replace(old, new);
        if retain {
            super::append_reserved(
                retired,
                Retired {
                    _object: previous,
                    owners: *owners,
                },
            );
        }
        Ok(())
    }
    pub(crate) fn retire(&mut self, ordinal: u64) -> Result {
        let slot = &mut self.slots[ordinal as usize % DEPTH];
        if *slot == Some(ordinal) {
            *slot = None;
        }
        let mut index = 0;
        while index < self.retired.len() {
            for owner in &mut self.retired[index].owners {
                if *owner == Some(ordinal) {
                    *owner = None;
                }
            }
            if self.retired[index].owners.iter().all(Option::is_none) {
                drop(self.retired.remove(index)?);
            } else {
                index += 1;
            }
        }
        Ok(())
    }
    /// Cancels only an unpublished record; older referenced backing remains
    /// owned until normal paired retirement or the firmware-stop boundary.
    pub(crate) fn cancel(&mut self, ordinal: u64) {
        let slot = &mut self.slots[ordinal as usize % DEPTH];
        if *slot == Some(ordinal) {
            *slot = None;
        }
    }
}
