// SPDX-License-Identifier: GPL-2.0-only

//! Immutable linked-program storage, shared by commands in one submission.

use super::fw::m5::{
    pool::PoolGeometry,
    pool_budget::{History, RenderBudget, Sizes},
    render_demand::{ComputeStorage, FragmentStorage, RenderDemand, TileStorage, VertexStorage},
};
use crate::util::Reader;
use kernel::{prelude::*, uapi};

// SAFETY: The UAPI record contains only fixed-width unsigned integers.
unsafe impl crate::util::AnyBitPattern for uapi::drm_asahi_render_program {}

pub(crate) struct Programs {
    entries: KVec<uapi::drm_asahi_render_program>,
}
impl Programs {
    pub(crate) fn parse(bytes: &[u8]) -> Result<Self> {
        let stride = size_of::<uapi::drm_asahi_render_program>();
        if bytes.is_empty() || bytes.len() % stride != 0 { return Err(EINVAL); }
        let mut entries = KVec::with_capacity(bytes.len() / stride, GFP_KERNEL)?;
        let mut reader = Reader::new(bytes);
        for _ in 0..bytes.len() / stride {
            let entry: uapi::drm_asahi_render_program = reader.read()?;
            Self::validate(&entry)?;
            entries.push(entry, GFP_KERNEL)?;
        }
        Ok(Self { entries })
    }

    pub(crate) fn from_entries(values: &[uapi::drm_asahi_render_program]) -> Result<Self> {
        if values.is_empty() { return Err(EINVAL); }
        let mut entries = KVec::with_capacity(values.len(), GFP_KERNEL)?;
        for entry in values {
            Self::validate(entry)?;
            entries.push(*entry, GFP_KERNEL)?;
        }
        Ok(Self { entries })
    }

    fn validate(entry: &uapi::drm_asahi_render_program) -> Result {
        if entry.stage > 1 || entry.flags != 0
            || (entry.stage == 0 && (entry.auxiliary_count != 0
                || entry.threadgroup_bytes != 0 || entry.imageblock_bytes != 0
                || entry.multisample_bytes != 0)) { return Err(EINVAL); }
        Ok(())
    }

    fn demands(&self, mut demand: RenderDemand) -> Result<(RenderDemand, [RenderDemand; 2])> {
        let mut stages = [demand; 2];
        stages[0].include_vertex(VertexStorage::default()).map_err(|_| EINVAL)?;
        let empty = FragmentStorage::default().parameters().map_err(|_| EINVAL)?;
        let no_tile = TileStorage::default().parameters(false).map_err(|_| EINVAL)?;
        stages[1].include_fragment(empty, no_tile).map_err(|_| EINVAL)?;
        demand.include_vertex(VertexStorage::default()).map_err(|_| EINVAL)?;
        demand.include_fragment(empty, no_tile).map_err(|_| EINVAL)?;
        for entry in &self.entries {
            if entry.stage == 0 {
                let storage = VertexStorage {
                    primary_bytes: entry.primary_bytes,
                    secondary_bytes: entry.secondary_bytes,
                    linked_bytes: entry.linked_bytes,
                    minimum_primary_bytes: entry.minimum_primary_bytes,
                };
                demand.include_vertex(storage).map_err(|_| EINVAL)?;
                stages[0].include_vertex(storage).map_err(|_| EINVAL)?;
            } else {
                let fragment = FragmentStorage {
                    primary_bytes: entry.primary_bytes,
                    secondary_bytes: entry.secondary_bytes,
                    linked_bytes: entry.linked_bytes,
                    minimum_primary_bytes: entry.minimum_primary_bytes,
                    auxiliary_count: entry.auxiliary_count,
                }.parameters().map_err(|_| EINVAL)?;
                let tile = TileStorage {
                    threadgroup_bytes: entry.threadgroup_bytes,
                    imageblock_bytes: entry.imageblock_bytes,
                    multisample_bytes: entry.multisample_bytes,
                }.parameters(false).map_err(|_| EINVAL)?;
                demand.include_fragment(fragment, tile).map_err(|_| EINVAL)?;
                stages[1].include_fragment(fragment, tile).map_err(|_| EINVAL)?;
            }
        }
        Ok((demand, stages))
    }
}

/// Frozen device topology and shared-queue policy. No per-command allocation.
#[derive(Clone, Copy)]
pub(crate) struct M5Policy {
    demand: RenderDemand,
    budgets: [RenderBudget; 2],
    compute_budgets: [RenderBudget; 2],
}

#[derive(Clone, Copy)]
pub(crate) struct Plan {
    pub(crate) sizes: Sizes,
    pub(crate) geometry: PoolGeometry,
    pub(crate) resource_flags: u64,
    pub(crate) pressure: u32,
    pub(crate) stages: Option<[StagePlan; 2]>,
}

#[derive(Clone, Copy)]
pub(crate) struct StagePlan {
    pub(crate) resource_flags: u64,
    pub(crate) ptc_max: u32,
}

impl M5Policy {
    pub(crate) fn new(id: &crate::regs::G17Id) -> Result<Self> {
        // Configured G17S context storage per enabled core. This is separate
        // from the final per-kick PTC cap selected during command preparation.
        const CONTEXT_FLOOR: u32 = 96;
        let nominal = id.num_cores.checked_mul(id.num_clusters).ok_or(EOVERFLOW)?;
        Ok(Self {
            demand: RenderDemand::new(&id.core_masks, nominal, CONTEXT_FLOOR)
                .map_err(|_| EINVAL)?,
            compute_budgets: [
                RenderBudget::new(&id.core_masks, CONTEXT_FLOOR, 3).map_err(|_| EINVAL)?,
                RenderBudget::new(&id.core_masks, CONTEXT_FLOOR, 2).map_err(|_| EINVAL)?,
            ],
            budgets: [
                RenderBudget::new(&id.core_masks, CONTEXT_FLOOR, 3).map_err(|_| EINVAL)?,
                RenderBudget::new(&id.core_masks, CONTEXT_FLOOR, 4).map_err(|_| EINVAL)?,
            ],
        })
    }

    pub(crate) fn plan_compute(&self, programs: &ComputePrograms, priority: u8) -> Result<Plan> {
        // Native UMA pool class 1 is realtime; class 2 covers all other
        // public priorities. Its resident-kick count also depends on engine.
        let (budget, active_kicks) = match priority {
            0 => (&self.compute_budgets[0], 3),
            1..=3 => (&self.compute_budgets[1], 2),
            _ => return Err(EINVAL),
        };
        let mut demand = self.demand;
        for entry in &programs.entries {
            demand.include_compute(ComputeStorage {
                primary_bytes: entry.primary_bytes,
                secondary_bytes: entry.secondary_bytes,
                linked_bytes: entry.linked_bytes,
                minimum_primary_bytes: entry.minimum_primary_bytes,
                threadgroup_bytes: entry.threadgroup_bytes,
                threads: entry.threads,
            }, 96, 114688).map_err(|_| EINVAL)?;
        }
        let sizes = budget.sizes(demand.requirements().map_err(|_| EINVAL)?, History::default())
            .map_err(|_| EINVAL)?;
        Ok(Plan {
            geometry: sizes.geometry().map_err(|_| EINVAL)?, sizes,
            resource_flags: demand.resource_flags().map_err(|_| EINVAL)?,
            pressure: (demand.pressure() / active_kicks).clamp(96, 512),
            stages: None,
        })
    }

    pub(crate) fn plan(&self, programs: &Programs, priority: u8) -> Result<Plan> {
        let (budget, active_kicks) = match priority { 0 => (&self.budgets[0], 3), 1..=3 => (&self.budgets[1], 4),
            _ => return Err(EINVAL) };
        let (demand, stage_demands) = programs.demands(self.demand)?;
        let stage = |d: RenderDemand| -> Result<StagePlan> { Ok(StagePlan {
            resource_flags: d.resource_flags().map_err(|_| EINVAL)?,
            ptc_max: (d.pressure() / active_kicks).clamp(96, 512),
        }) };
        let sizes = budget.sizes(demand.requirements().map_err(|_| EINVAL)?, History::default())
            .map_err(|_| EINVAL)?;
        Ok(Plan {
            geometry: sizes.geometry().map_err(|_| EINVAL)?, sizes,
            resource_flags: demand.resource_flags().map_err(|_| EINVAL)?,
            pressure: demand.pressure(),
            stages: Some([stage(stage_demands[0])?, stage(stage_demands[1])?]),
        })
    }
}

// SAFETY: The record contains only fixed-width unsigned integers.
unsafe impl crate::util::AnyBitPattern for uapi::drm_asahi_compute_program {}

pub(crate) struct ComputePrograms {
    entries: KVec<uapi::drm_asahi_compute_program>,
}
impl ComputePrograms {
    pub(crate) fn parse(bytes: &[u8]) -> Result<Self> {
        let stride = size_of::<uapi::drm_asahi_compute_program>();
        if bytes.is_empty() || bytes.len() % stride != 0 { return Err(EINVAL); }
        let mut entries = KVec::with_capacity(bytes.len() / stride, GFP_KERNEL)?;
        let mut reader = Reader::new(bytes);
        for _ in 0..bytes.len() / stride {
            let entry: uapi::drm_asahi_compute_program = reader.read()?;
            if entry.flags != 0 || entry.threads == 0 { return Err(EINVAL); }
            entries.push(entry, GFP_KERNEL)?;
        }
        Ok(Self { entries })
    }
}
