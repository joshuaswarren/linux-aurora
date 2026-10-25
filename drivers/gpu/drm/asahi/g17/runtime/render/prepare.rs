// SPDX-License-Identifier: GPL-2.0-only OR MIT

//! Off-lock render graph construction and command preparation. Every borrowed
//! installed pair returns to its device owner before its preparation lease ends.

use super::{Config, Reservation, Restore, Site, RENDER_CAPACITY_PENDING};
use crate::g17::{
    buffer::MetricsLease,
    preparation::Lease,
    job::Packet,
    kick,
    object::{Allocator, KernelObject, Pool},
    queue::render::{Pair, RenderAddresses},
};
use kernel::{prelude::*, sync::Arc};

struct BuildResources {
    uat: Arc<crate::mmu::Uat>,
    objects: Arc<Pool>,
    ids: Arc<crate::g17::freelist::RenderIds>,
    global: crate::mmu::Vm,
    metrics: Arc<KernelObject>,
    metrics_ids: Arc<crate::g17::buffer::MetricsIds>,
    metrics_bases: crate::g17::fw::buffer::MetricsAddresses,
    config: Config,
}
impl BuildResources {
    fn new(firmware: &crate::g17::Firmware) -> Result<Self> {
        let metrics = firmware._pm_metrics.as_ref().ok_or_else(|| crate::g17::unsupported(file!(), line!()))?.clone();
        let client = if firmware.uat.is_sptm() { metrics.gpu_va() } else {
            crate::hw::t8140::CONFIG.pm_metrics.as_ref().ok_or_else(|| crate::g17::unsupported(file!(), line!()))?.va
        };
        Ok(Self {
            uat: firmware.uat.clone(),
            objects: firmware.pool.clone(),
            ids: firmware.render_ids.clone(),
            global: firmware._render_global.clone(),
            metrics,
            metrics_ids: firmware.queues.metrics.clone(),
            metrics_bases: crate::g17::fw::buffer::MetricsAddresses {
                firmware: firmware.init.metrics_fw_va()?,
                client,
            },
            config: firmware.queues.render.config,
        })
    }
    fn pool_plan(&self, packet: &Packet) -> Result<Option<crate::g17::render_resources::Plan>> {
        let crate::g17::command::Validated::Render { plan, .. } = &packet.command
            else { return Err(EINVAL); };
        if self.uat.is_sptm() != plan.is_some() { return Err(EINVAL); }
        Ok(*plan)
    }
    fn metrics(&self) -> Result<MetricsLease> {
        MetricsLease::new(&self.metrics_ids, &self.metrics, self.metrics_bases)
    }
    /// Called during off-lock preparation, after userspace bindings exist.
    fn bind_metrics(&self, context: &crate::g17::context::Context, metrics: &mut MetricsLease) -> Result {
        if self.uat.is_sptm() {
            metrics.set_client_base(context.metrics_client(&self.metrics)?)?;
        }
        Ok(())
    }
}

enum Build {
    /// The last field holds spare rings of recycled QIDs (vmfix6) to adopt.
    Fresh(Reservation, u64, MetricsLease, Option<KBox<[Option<kick::Queue>; 2]>>),
    Parked {
        parked: crate::g17::queue::render::parked::ParkedPair,
        buffer: (u8, u64),
        pm_generation: u64,
        metrics: MetricsLease,
    },
    Rebind {
        pair: KBox<Pair>,
        buffer: Option<(u8, u64)>,
        pm_generation: Option<u64>,
        metrics: Option<MetricsLease>,
    },
}

/// What every build step needs besides its variant: the preparation lease, the allocator,
/// the device resources, the slot and the first packet.
#[derive(Clone, Copy)]
struct BuildContext<'a> {
    lease: &'a Lease,
    alloc: &'a Allocator<'a>,
    resources: &'a BuildResources,
    slot: u8,
    first: &'a Arc<Packet>,
    private_pool: Option<&'a Arc<crate::g17::freelist::RenderPool>>,
}

impl super::super::Backend {
    /// Reserve scarce pool capacity before transferring a physical render pair.
    /// Reclaim only a cached identity whose exact firmware release is consumed.
    pub(in crate::g17::runtime) fn claim_render_pool(&self, first: &Arc<Packet>)
        -> Result<(Arc<crate::g17::freelist::RenderPool>, Option<crate::g17::freelist::PoolPin>)>
    {
        let lease = self.shared.preparations.try_enter()?;
        let dev = self.shared.drm()?;
        let (resources, existing_pool) = {
            let state = self.shared.state.lock();
            let firmware = (*state).as_deref().ok_or(ENODEV)?;
            let resources = BuildResources::new(firmware)?;
            let geometry = resources.pool_plan(first)?.map(|plan| plan.geometry);
            let mut existing = None;
            for entry in firmware.queues.render.entries.iter().flatten() {
                let Some(pair) = entry.pair.as_ref() else { continue; };
                if pair.matches(self.owner, &self.context)? && pair.pool().fits(geometry)
                    && !pair.pool().vacant() {
                    existing = Some(pair.pool().clone());
                    break;
                }
            }
            (resources, existing)
        };
        let alloc = Allocator { dev: &dev, uat: &resources.uat };
        let pool = match existing_pool {
            Some(pool) => pool,
            None => self.context.render_pool(&alloc, &resources.ids, &resources.global, resources.pool_plan(first)?)?,
        };
        let pin = self.pin_render_pool(&pool, &resources, &lease)?;
        Ok((pool, pin))
    }

    /// Recycle a released cache entry before assigning another physical pool.
    /// Logical contexts keep metadata, not an unbounded private-memory cache.
    /// The detached backing is always dropped after the device lock.
    fn pin_render_pool(&self, pool: &Arc<crate::g17::freelist::RenderPool>,
        resources: &BuildResources, lease: &Lease)
        -> Result<Option<crate::g17::freelist::PoolPin>>
    {
        let mut reclaimed_pages = None;
        loop {
            if !lease.is_current() { return Err(EAGAIN); }
            match pool.pin_id() {
                Ok(pin) => {
                    if let Some(reclaimed) = reclaimed_pages.take() {
                        pool.cache_reclaimed(reclaimed);
                    }
                    return Ok(pin);
                }
                Err(error) if error == ENOSPC && resources.uat.is_sptm() => {
                    // Repeat the release observation once after a failed claim;
                    // a concurrent pin may have consumed the reclaimed slot.
                    let reclaimed = {
                        let mut state = self.shared.state.lock();
                        let firmware = (*state).as_deref_mut().ok_or(ENODEV)?;
                        if !lease.is_current() { return Err(EAGAIN); }
                        if firmware.recovery.pending()
                            || crate::g17::recovery::Memory::recovery_state(&firmware.init)? != 0 {
                            return Err(EAGAIN);
                        }
                        firmware.queues.render.pools.observe(&firmware.init)?;
                        resources.ids.reclaim_one(|id|
                            firmware.queues.render.pools.released(u32::from(id), 0))
                    };
                    if reclaimed.is_some() { reclaimed_pages = reclaimed; continue; }
                    self.shared.queue_events();
                    return Err(self.capacity(Site::CapacityRenderPool, error));
                }
                Err(error) => return Err(self.capacity(Site::CapacityRenderPool, error)),
            }
        }
    }

    /// Caller owns physical admission and holds this pool's ID pin. Do not
    /// evict a warm backing for a logical contender still awaiting admission.
    fn prepare_pool_backing(&self, pool: &Arc<crate::g17::freelist::RenderPool>,
        resources: &BuildResources, lease: &Lease, client: &crate::mmu::Vm) -> Result
    {
        if resources.uat.is_sptm() && pool.needs_storage() {
            let reclaimed = {
                let mut state = self.shared.state.lock();
                let firmware = (*state).as_deref_mut().ok_or(ENODEV)?;
                if !lease.is_current() { return Err(EAGAIN); }
                if firmware.recovery.pending()
                    || crate::g17::recovery::Memory::recovery_state(&firmware.init)? != 0 {
                    return Err(EAGAIN);
                }
                firmware.queues.render.pools.observe(&firmware.init)?;
                resources.ids.reclaim_backing(|id|
                    firmware.queues.render.pools.released(u32::from(id), 0))
            };
            if let Some(reclaimed) = reclaimed { pool.cache_reclaimed(reclaimed); }
        }
        let dev = self.shared.drm()?;
        let alloc = Allocator { dev: &dev, uat: &resources.uat };
        pool.ensure_backing(&alloc, &resources.global, client)
    }

    /// Restoring a borrowed graph wakes other publishers. A preparation
    /// attempt must not mistake its own restoration for returned capacity;
    /// preserve any independently observed progress when advancing its sample.
    fn notify_render_attempt(&self, observed: Option<&mut u64>) {
        if let Some(observed) = observed {
            crate::g17::progress::note_change();
            self.shared.changed.notify_all();
            self.shared.physical_available.notify_own(observed);
        } else {
            self.shared.notify_changed();
        }
    }

    fn build_render_pair(&self, slot: u8, first: &Arc<Packet>, optional: bool,
        nonblocking: bool, mut observed: Option<&mut u64>, pair_loan: &mut bool) -> Result {
        let lease = if nonblocking { self.shared.preparations.try_enter()? } else { self.shared.preparations.enter(false)? };
        let dev = self.shared.drm()?;
        let pool_claim = if matches!(self.shared.firmware_profile, crate::g17::FirmwareProfile::M5Sptm) {
            Some(self.claim_render_pool(first)?)
        } else { None };
        let (resources, build) = loop {
            let mut state = self.shared.state.lock();
            let firmware = (*state).as_deref_mut().ok_or(ENODEV)?;
            if !lease.is_current() {
                return Err(EAGAIN);
            }
            // vmfix6: with no free QID, recycle a parked pair of a released
            // owner (native-like) before falling back to lending.
            let mut recycled = 0;
            let ready = loop {
                match firmware.queues.ensure_render_ids(slot, self.owner) {
                    Ok(()) => break true,
                    Err(error) if error == ENOSPC && recycled < 4
                        && firmware.recycle_parked_render(Some(slot))? => recycled += 1,
                    Err(error) if error == ENOSPC => {
                        firmware.queues.admission.mark_unbuildable(slot);
                        if optional {
                            break false;
                        }
                        return Err(error);
                    }
                    Err(error) => return Err(error),
                }
            };
            if !ready {
                return Ok(());
            }
            firmware.queues.lend_live_pair(slot, self.owner)?;
            if firmware.render_retained_for(slot, &self.context) {
                // vmfix6d: reinstall this VM's retained graph first, in its own
                // frame (off this deep build path), then build as usual.
                drop(state);
                self.restore_retained(slot, &lease, observed.as_deref_mut())?;
                continue;
            }
            let resources = BuildResources::new(firmware)?;
            let build =
                if let Some(entry) = firmware.queues.render.entries[usize::from(slot)].as_ref() {
                    if entry.pending_error.is_some() || entry.pending_timeout {
                        self.shared.queue_events();
                        return Err(self.busy(Site::BuildPending));
                    }
                    if entry.constructing || entry.preparing.is_some() || entry.growing {
                        *pair_loan = entry.pair.is_none() && entry.preparing.is_some();
                        return Err(self.busy(Site::BuildBusy));
                    }
                    if entry.parked.is_some() {
                        let metrics = resources.metrics().map_err(|e| self.capacity(Site::CapacityMetrics, e))?;
                        let pm_generation = firmware.queues.render.reserve_pm_generation()?;
                        let buffer = firmware.queues.buffers.allocate_render().map_err(|e| self.capacity(Site::CapacityBuffer, e))?;
                        let entry = firmware.queues.render.entry_mut(slot)?;
                        entry.preparing = Some(first.clone());
                        entry.pending_owner = Some(self.owner);
                        entry.borrowed_active = false;
                        Build::Parked {
                            parked: entry.parked.take().ok_or(EIO)?,
                            buffer,
                            pm_generation,
                            metrics,
                        }
                    } else if entry.pair.is_none() {
                        let metrics = resources.metrics().map_err(|e| self.capacity(Site::CapacityMetrics, e))?;
                        let pm_generation = firmware.queues.render.reserve_pm_generation()?;
                        {
                            let reservation = firmware
                                .queues
                                .reserve_render(slot, self.owner)
                                .map_err(|error| self.tag(Site::BuildReserve, self.capacity(Site::CapacityReserve, error)))?;
                            let channels = firmware.queues.take_render_channels(reservation.ids)?;
                            Build::Fresh(reservation, pm_generation, metrics, channels)
                        }
                    } else {
                        let pair = entry.pair.as_ref().ok_or(EBUSY)?;
                        if pair.matches(self.owner, &self.context)? {
                            let plan = resources.pool_plan(first)?;
                            if pair.pool().fits(plan.map(|p| p.geometry)) {
                                if !pair.pool().vacant() { return Ok(()); }
                                let pool = pair.pool().clone();
                                drop(state);
                                return self.prepare_pool_backing(&pool, &resources, &lease, self.context.vm());
                            }
                        }
                        // Every rebind, including same-context pool growth,
                        // needs a retired-binding slot. Refuse before borrowing
                        // the pair so restoration cannot wake our own capacity
                        // wait without any resource becoming available.
                        if firmware.queues.render.retired.len() == firmware.queues.render.retired.capacity() {
                            return Err(self.capacity(Site::CapacityRetired, ENOSPC));
                        }
                        if !pair.bindable(self.owner, &self.context)? {
                            return Err(self.busy(Site::BuildBindable));
                        }
                        // vmfix6f: a different logical context (even of the same
                        // VM) gets its own graph; only a pool-geometry rebind of
                        // the same context keeps the graph and its queue records.
                        let foreign = if crate::g17::queue::render::rebind_legacy(
                            crate::g17::queue::render::LEGACY_IN_PLACE_RETARGET)
                        {
                            !Arc::ptr_eq(pair.context().status(), self.context.status())
                        } else {
                            !Arc::ptr_eq(pair.context(), &self.context)
                        };
                        if foreign {
                            firmware
                                .queues
                                .render
                                .save_detaching_pool(&firmware.init, slot)?;
                        }
                        let metrics = if foreign {
                            Some(resources.metrics().map_err(|e| self.capacity(Site::CapacityMetrics, e))?)
                        } else {
                            None
                        };
                        let pm_generation = if foreign {
                            Some(firmware.queues.render.reserve_pm_generation()?)
                        } else {
                            None
                        };
                        let buffer = if foreign {
                            Some(firmware.queues.buffers.allocate_render().map_err(|e| self.capacity(Site::CapacityBuffer, e))?)
                        } else {
                            None
                        };
                        let entry = firmware.queues.render.entry_mut(slot)?;
                        entry.preparing = Some(first.clone());
                        entry.pending_owner = Some(self.owner);
                        entry.borrowed_active = entry
                            .pair
                            .as_ref()
                            .is_some_and(|pair| pair.published_in_flight());
                        entry.borrowed_dependencies =
                            entry.pair.as_ref().map(|pair| pair.dependency_snapshot());
                        Build::Rebind {
                            pair: entry.pair.take().ok_or(EIO)?,
                            buffer,
                            pm_generation,
                            metrics,
                        }
                    }
                } else {
                    let metrics = resources.metrics().map_err(|e| self.capacity(Site::CapacityMetrics, e))?;
                    let pm_generation = firmware.queues.render.reserve_pm_generation()?;
                    {
                        let reservation = firmware
                            .queues
                            .reserve_render(slot, self.owner)
                            .map_err(|error| self.tag(Site::BuildReserve, self.capacity(Site::CapacityReserve, error)))?;
                        let channels = firmware.queues.take_render_channels(reservation.ids)?;
                        Build::Fresh(reservation, pm_generation, metrics, channels)
                    }
                };
            break (resources, build);
        };
        let alloc = Allocator {
            dev: &dev,
            uat: &resources.uat,
        };
        let fresh_graph = match &build {
            Build::Fresh(..) | Build::Parked { .. } => true,
            Build::Rebind { buffer, .. } => buffer.is_some(),
        };
        let ctx = BuildContext {
            lease: &lease,
            alloc: &alloc,
            resources: &resources,
            slot,
            first,
            private_pool: pool_claim.as_ref().map(|(pool, _)| pool),
        };
        match build {
            Build::Fresh(reservation, pm_generation, metrics, channels) => self
                .install_fresh_pair(&ctx, reservation, pm_generation, metrics, channels)
                .map_err(|error| self.tag(Site::FreshInstall, error))?,
            Build::Parked {
                parked,
                buffer,
                pm_generation,
                metrics,
            } => self
                .install_parked_pair(&ctx, parked, buffer, pm_generation, metrics, observed.as_deref_mut())
                .map_err(|error| self.tag(Site::ParkedInstall, error))?,
            Build::Rebind {
                pair,
                buffer,
                pm_generation,
                metrics,
            } => self.rebind_pair(&ctx, pair, buffer, pm_generation, metrics, observed.as_deref_mut())?,
        }
        self.finish_render_build(&lease, slot, fresh_graph)
            .map_err(|error| self.tag(Site::BuildFinish, error))
    }

    /// vmfix6d eager restore of this VM's retained graph on `slot` (see
    /// Firmware::restore_render_binding). Pair storage for a parked slot is
    /// allocated off-lock only when needed; an emptied parked owner (with its
    /// last scheduler page) is dropped after the device mutex is released.
    #[inline(never)]
    fn restore_retained(&self, slot: u8, lease: &Lease, observed: Option<&mut u64>) -> Result {
        let (resources, pool, context) = {
            let state = self.shared.state.lock();
            let firmware = (*state).as_deref().ok_or(ENODEV)?;
            let Some(index) = firmware.retained_index(slot, &self.context) else { return Ok(()); };
            let binding = &firmware.queues.render.retired[index];
            (BuildResources::new(firmware)?, binding.pool().clone(), binding.context().clone())
        };
        let mut pool_pin = self.pin_render_pool(&pool, &resources, lease)?;
        self.prepare_pool_backing(&pool, &resources, lease, context.vm())?;
        let mut storage: Option<KBox<core::mem::MaybeUninit<Pair>>> = None;
        loop {
            let emptied = {
                let mut state = self.shared.state.lock();
                let firmware = (*state).as_deref_mut().ok_or(ENODEV)?;
                if !lease.is_current() {
                    return Err(EAGAIN);
                }
                match firmware.restore_render_binding(slot, &self.context, &mut storage,
                    &pool, &mut pool_pin) {
                    Ok(Restore::None) => return Ok(()),
                    Ok(Restore::Done(parked)) => parked,
                    Ok(Restore::NeedStorage) => {
                        drop(state);
                        storage = Some(KBox::<Pair>::new_uninit(GFP_KERNEL)?);
                        continue;
                    }
                    Err(error) => return Err(self.tag(Site::Restore, error)),
                }
            };
            drop(emptied);
            self.notify_render_attempt(observed);
            return Ok(());
        }
    }

    /// Inlined into build_render_pair(): as a separate frame this step deepens the scheduler
    /// run_job stack path, which is close to its budget.
    #[inline(always)]
    /// Off-lock construction of a fresh pair, then its device-locked install.
    fn install_fresh_pair(
        &self,
        ctx: &BuildContext<'_>,
        reservation: Reservation,
        pm_generation: u64,
        mut metrics: MetricsLease,
        mut channels: Option<KBox<[Option<kick::Queue>; 2]>>,
    ) -> Result {
        let BuildContext {
            lease,
            alloc,
            resources,
            slot,
            first,
            private_pool,
        } = *ctx;
        let built = (|| -> Result<KBox<Pair>> {
            resources.bind_metrics(&self.context, &mut metrics)?;
            let free_list = match private_pool {
                Some(pool) => pool.clone(),
                None => self.context.render_pool(&alloc, &resources.ids, &resources.global, resources.pool_plan(first)?)
                    .map_err(|error| self.capacity(Site::CapacityRenderPool, error))?,
            };
            self.prepare_pool_backing(&free_list, resources, lease, self.context.vm())?;
            Pair::new(
                &alloc,
                &resources.objects,
                reservation.ids,
                slot,
                self.owner,
                self.context.clone(),
                free_list,
                metrics,
                reservation.buffer,
                pm_generation,
                first,
                resources.config.clusters,
                resources.config.descriptor_flags,
                &mut channels,
            )
        })();
        let (mut built, error) = match built {
            Ok(pair) => (Some(pair), None),
            Err(error) => (None, Some(error)),
        };
        let result = (|| {
            let mut state = self.shared.state.lock();
            let firmware = (*state).as_deref_mut().ok_or(ENODEV)?;
            if !lease.is_current() {
                return Err(EAGAIN);
            }
            if let Some(error) = error {
                return Err(error);
            }
            if !self.context.is_current() || self.context.status().get() != 0 {
                return Err(EFAULT);
            }
            if firmware.queues.render.reserved(reservation)?.closed {
                return Err(ECANCELED);
            }
            built
                .as_mut()
                .ok_or(EIO)?
                .finish_install(&alloc, firmware.init.pb_descriptor_table()?)?;
            firmware.queues.install_render(reservation, &mut built)?;
            firmware.queues.render_channels_installed(reservation.ids);
            Ok(())
        })();
        // A pair that adopted spare rings and then failed returns them instead
        // of dropping them (an installed QID must keep its ring).
        if result.is_err() {
            if let Some(pair) = built.take() {
                if let Some(rings) = Pair::into_unused_kicks(pair) {
                    match channels.as_mut() {
                        Some(spares) => {
                            let mut rings = rings;
                            for lane in 0..2 {
                                if spares[lane].is_none() {
                                    spares[lane] = rings[lane].take();
                                }
                            }
                        }
                        None => channels = Some(rings),
                    }
                }
            }
        }
        // Fresh backing disappears before its temporary PB reference returns.
        drop(built);
        if result.is_err() {
            let mut state = self.shared.state.lock();
            let firmware = (*state).as_deref_mut().ok_or(ENODEV)?;
            // Unused spare rings go back to their QIDs; an adopted ring that
            // went down with the failed pair poisons its QID (never re-ringed).
            firmware.queues.settle_render_channels(slot, reservation.ids, channels.take());
            firmware.queues.cancel_render_reservation(reservation)?;
        } else if let Some(left) = channels.take() {
            // Defensive: success adopts both lanes, so nothing should be left.
            let mut state = self.shared.state.lock();
            if let Some(firmware) = (*state).as_deref_mut() {
                firmware.queues.settle_render_channels(slot, reservation.ids, Some(left));
            }
        }
        result
    }

    /// Inlined into build_render_pair(): as a separate frame this step deepens the scheduler
    /// run_job stack path, which is close to its budget.
    #[inline(always)]
    /// Off-lock preparation of a parked pair for its new owner, then its device-locked
    /// commit; the pair returns to its parked state on failure.
    fn install_parked_pair(
        &self,
        ctx: &BuildContext<'_>,
        mut parked: crate::g17::queue::render::parked::ParkedPair,
        buffer: (u8, u64),
        pm_generation: u64,
        mut metrics: MetricsLease,
        observed: Option<&mut u64>,
    ) -> Result {
        let BuildContext {
            lease,
            alloc,
            resources,
            slot,
            first,
            private_pool,
        } = *ctx;
        let built = (|| -> Result<_> {
            resources.bind_metrics(&self.context, &mut metrics)?;
            let free_list = match private_pool {
                Some(pool) => pool.clone(),
                None => self.context.render_pool(&alloc, &resources.ids, &resources.global, resources.pool_plan(first)?)
                    .map_err(|error| self.capacity(Site::CapacityRenderPool, error))?,
            };
            self.prepare_pool_backing(&free_list, resources, lease, self.context.vm())?;
            let prepared = parked.prepare(
                &alloc,
                &resources.objects,
                self.owner,
                self.context.clone(),
                free_list,
                metrics,
                buffer,
                pm_generation,
                first,
            )?;
            let storage = KBox::<Pair>::new_uninit(GFP_KERNEL)?;
            Ok((prepared, storage))
        })();
        let (mut prepared, mut storage, error) = match built {
            Ok((prepared, storage)) => (Some(prepared), Some(storage), None),
            Err(error) => (None, None, Some(error)),
        };
        let result = {
            let mut state = self.shared.state.lock();
            let firmware = (*state).as_deref_mut().ok_or(ENODEV)?;
            (|| {
                if !lease.is_current() {
                    return Err(EAGAIN);
                }
                if let Some(error) = error {
                    return Err(error);
                }
                if !self.context.is_current() || self.context.status().get() != 0 {
                    return Err(EFAULT);
                }
                let entry = firmware.queues.render.entry(slot)?;
                if entry.pending_owner != Some(self.owner)
                    || entry.pending_owner_closed
                    || entry.pair.is_some()
                    || entry.parked.is_some()
                {
                    return Err(ECANCELED);
                }
                let storage = storage.take().ok_or(EIO)?;
                firmware.queues.buffers.can_release(buffer.0)?;
                prepared.as_mut().ok_or(EIO)?.finish_install(
                    &parked,
                    &alloc,
                    firmware.init.pb_descriptor_table()?,
                )?;
                let pair = parked.commit(&mut prepared, storage)?;
                firmware.queues.buffers.reserve_owner(buffer.0)?;
                firmware.queues.buffers.release(buffer.0)?;
                let entry = firmware.queues.render.entry_mut(slot)?;
                entry.reservation.owner = self.owner;
                entry.reservation.buffer = buffer;
                entry.context = Some(self.context.clone());
                entry.pool = Some(pair.pool().clone());
                entry.pair = Some(pair);
                entry.closed = false;
                entry.published = false;
                Ok(())
            })()
        };
        // Rejected candidate mappings disappear before the PB identity
        // returns, while the original installed kick owner stays live.
        drop(prepared);
        drop(storage);
        let grow_ready = {
            let mut state = self.shared.state.lock();
            let firmware = (*state).as_deref_mut().ok_or(ENODEV)?;
            if result.is_err() {
                firmware.queues.render.entry_mut(slot)?.parked = Some(parked);
                firmware.queues.buffers.release(buffer.0)?;
            } else {
                drop(state);
                drop(parked);
                state = self.shared.state.lock();
            }
            let firmware = (*state).as_deref_mut().ok_or(ENODEV)?;
            let entry = firmware.queues.render.entry_mut(slot)?;
            entry.preparing = None;
            entry.pending_owner = None;
            entry.pending_owner_closed = false;
            firmware.render_grow_ready()
        };
        self.notify_render_attempt(observed);
        if grow_ready {
            self.shared.queue_grow();
        }
        result
    }

    /// Inlined into build_render_pair(): as a separate frame this step deepens the scheduler
    /// run_job stack path, which is close to its budget.
    #[inline(always)]
    /// Off-lock preparation of an installed pair's rebinding, then its device-locked
    /// owner change; the original pair stays installed on failure.
    fn rebind_pair(
        &self,
        ctx: &BuildContext<'_>,
        mut pair: KBox<Pair>,
        buffer: Option<(u8, u64)>,
        pm_generation: Option<u64>,
        mut metrics: Option<MetricsLease>,
        observed: Option<&mut u64>,
    ) -> Result {
        let BuildContext {
            lease,
            alloc,
            resources,
            slot,
            first,
            private_pool,
        } = *ctx;
        let prepared = (|| {
            if let Some(metrics) = metrics.as_mut() {
                resources.bind_metrics(&self.context, metrics)?;
            }
            let free_list = match private_pool {
                Some(pool) => pool.clone(),
                None => self.context.render_pool(&alloc, &resources.ids, &resources.global, resources.pool_plan(first)?)
                    .map_err(|error| self.capacity(Site::CapacityRenderPool, error))?,
            };
            self.prepare_pool_backing(&free_list, resources, lease, self.context.vm())?;
            pair.prepare_bind(
                &alloc,
                &resources.objects,
                self.owner,
                self.context.clone(),
                free_list,
                metrics,
                buffer,
                pm_generation,
                first,
            )
        })();
        let (mut prepared, error) = match prepared {
            Ok(binding) => (Some(binding), None),
            Err(error) => (None, Some(error)),
        };
        let (result, grow_ready) = {
            let mut state = self.shared.state.lock();
            let firmware = (*state).as_deref_mut().ok_or(ENODEV)?;
            let result = (|| {
                if !lease.is_current() {
                    return Err(EAGAIN);
                }
                if let Some(error) = error {
                    return Err(self.tag(Site::RebindPrepare, error));
                }
                if !self.context.is_current() || self.context.status().get() != 0 {
                    return Err(EFAULT);
                }
                let entry = firmware.queues.render.entry(slot)?;
                if entry.pending_owner != Some(self.owner) || entry.pending_owner_closed {
                    return Err(ECANCELED);
                }
                if firmware.queues.render.retired.len()
                    == firmware.queues.render.retired.capacity()
                {
                    // vmfix6d D: a full retired-binding list is a capacity
                    // wait (the event worker drains it), not a client error.
                    return Err(self.capacity(Site::CapacityRetired, enospc_site!("render/prepare.rs:489")));
                }
                if !pair.matches(self.owner, &self.context)? {
                    // A pair lent from a live owner keeps that owner's
                    // scheduler state until it closes (Deferred, armed at close).
                    let owner_closed = entry.closed || !entry.lent_live;
                    firmware.queues.teardown.displace(
                        pair.context(), owner_closed, super::super::now_ns(),
                    )?;
                }
                pair.bind_owner(
                    prepared.as_mut().ok_or(EIO)?,
                    &alloc,
                    firmware.init.pb_descriptor_table()?,
                )
                .map_err(|error| self.tag(Site::RebindBind, error))?;
                if let Some((id, _)) = buffer {
                    firmware.queues.buffers.reserve_owner(id)?;
                    firmware.queues.buffers.release(id)?;
                }
                let entry = firmware.queues.render.entry_mut(slot)?;
                entry.reservation.owner = self.owner;
                if let Some(buffer) = buffer {
                    entry.reservation.buffer = buffer;
                }
                entry.context = Some(self.context.clone());
                entry.pool = Some(pair.pool().clone());
                entry.closed = false;
                entry.lent_live = false;
                Ok(())
            })();
            // The original installed pair remains owned on every error.
            let entry = firmware.queues.render.entry_mut(slot)?;
            entry.preparing = None;
            entry.pending_owner = None;
            entry.pending_owner_closed = false;
            if entry.closed { pair.release_owner(); }
            entry.pair = Some(pair);
            entry.borrowed_dependencies = None;
            (result, firmware.render_grow_ready())
        };
        drop(prepared);
        if result.is_err() {
            if let Some((id, _)) = buffer {
                let mut state = self.shared.state.lock();
                (*state)
                    .as_deref_mut()
                    .ok_or(ENODEV)?
                    .queues
                    .buffers
                    .release(id)?;
            }
        }
        self.notify_render_attempt(observed);
        if grow_ready {
            self.shared.queue_grow();
        }
        result
    }

    /// Under the device mutex: the graph is complete and retained; rebase a fresh graph's
    /// cursors and warm its pool.
    fn finish_render_build(&self, lease: &Lease, slot: u8, fresh_graph: bool) -> Result {
        let mut state = self.shared.state.lock();
        let firmware = (*state).as_deref_mut().ok_or(ENODEV)?;
        if !lease.is_current() {
            return Err(EAGAIN);
        }
        if firmware.queues.render.entry(slot)?.closed {
            return Err(ECANCELED);
        }
        if !self.context.is_current() || self.context.status().get() != 0 {
            return Err(EFAULT);
        }
        // From this point every error leaves the complete graph retained for
        // retry. Physical queue identities and installed mappings stay owned.
        if fresh_graph {
            firmware
                .queues
                .render
                .entry_mut(slot)?
                .pair
                .as_mut()
                .ok_or(EIO)?
                .rebase_fresh_graph()?;
        }
        let pool = firmware
            .queues
            .render
            .entry(slot)?
            .pool
            .clone()
            .ok_or(EIO)?;
        firmware.queues.render.pools.warm(&firmware.init, &pool)
    }

    pub(super) fn ensure_render(&self, packet: &Arc<Packet>, slot: u8, nonblocking: bool,
        mut observed: Option<&mut u64>, pair_loan: &mut bool) -> Result {
        // This stack-owned result belongs only to the selected pair's attempt.
        // Diagnostic busy_site may be written by another backend caller.
        *pair_loan = false;
        self.build_render_pair(slot, packet, false, nonblocking, observed.as_deref_mut(), pair_loan)?;
        let sibling = {
            let state = self.shared.state.lock();
            (*state)
                .as_deref()
                .ok_or(ENODEV)?
                .queues
                .admission
                .reserve_warmable_sibling(self.owner, slot)
        };
        if let Some(sibling) = sibling {
            // An optional sibling may be borrowed or waiting for recovery.
            // The selected pair is revalidated by prepare_render before publication.
            // Its refusal must not change the selected pair's wait reason.
            let mut sibling_loan = false;
            match self.build_render_pair(sibling.slot(), packet, true, nonblocking, observed.as_deref_mut(), &mut sibling_loan) {
                Err(error)
                    if error == EBUSY
                        || error == EAGAIN
                        || error == super::LEND_PENDING
                        || error == RENDER_CAPACITY_PENDING => {}
                result => result?,
            }
        }
        Ok(())
    }

    pub(super) fn prepare_render(
        &self,
        slot: u8,
        packet: &Arc<Packet>,
        deferred: &mut super::super::DeferredBatch,
        outer: Option<&mut crate::g17::initdata::outer_credit::OuterClaim>,
        pair_loan: &mut bool,
    ) -> Result {
        *pair_loan = false;
        let lease = self.shared.preparations.enter(false)?;
        let dev = self.shared.drm()?;
        let (uat, mut pair, addresses, growth_target) = {
            let mut state = self.shared.state.lock();
            let firmware = (*state).as_deref_mut().ok_or(ENODEV)?;
            if !lease.is_current() {
                return Err(EAGAIN);
            }
            let entry = firmware.queues.render.entry(slot)?;
            let qids = entry.pair
                .as_ref()
                .ok_or_else(|| {
                    *pair_loan = entry.preparing.is_some();
                    self.busy(Site::PrepareNoPair)
                })?
                .qids();
            let addresses = RenderAddresses {
                stamps: [
                    firmware.init.stamp_addresses(qids[0])?,
                    firmware.init.stamp_addresses(qids[1])?,
                ],
            };
            let pair = firmware
                .queues
                .render
                .entry_mut(slot)?
                .pair
                .as_mut()
                .ok_or(EIO)?;
            if !pair.matches(self.owner, &packet.context)? {
                return Err(EIO);
            }
            // The selected scheduler is retained through release acknowledgement
            // even if later private command allocation fails. Sibling warming
            // does not cross this publication boundary.
            packet.context.mark_published();
            let target = pair
                .host_growth_target(packet)
                .map_err(|error| self.tag(Site::PrepareGrowth, error))?;
            let buffer_id = pair.buffer_id();
            let growth_target = if let Some(target) = target {
                let table = firmware.init.pb_descriptor_table()?;
                let offset = usize::from(buffer_id) * 16;
                core::sync::atomic::fence(core::sync::atomic::Ordering::Acquire);
                let mut descriptor = [0; 4];
                for (index, word) in descriptor.iter_mut().enumerate() {
                    *word = table
                        .word(offset + index * 4)?
                        .load(core::sync::atomic::Ordering::Relaxed);
                }
                Some((target, descriptor))
            } else {
                None
            };
            let pair = firmware
                .queues
                .render
                .take_pair(slot, self.owner, packet)
                .map_err(|error| self.tag(Site::PrepareTake, error))?;
            (firmware.uat.clone(), pair, addresses, growth_target)
        };
        let alloc = Allocator {
            dev: &dev,
            uat: &uat,
        };
        let mut growth = None;
        let result = (|| {
            if let Some((target, descriptor)) = growth_target {
                let vm = pair.context().vm().clone();
                growth = pair.manager().prepare_growth(
                    &dev,
                    &vm,
                    target,
                    Some(descriptor),
                )?;
            }
            pair.prepare(&alloc, packet, addresses)
        })();
        let mut returned = Some(pair);
        let mut state = self.shared.state.lock();
        let firmware = (*state).as_deref_mut().ok_or(ENODEV)?;
        firmware.queues.render.return_pair(slot, &mut returned)?;
        if firmware.queues.render.entry(slot)?.closed {
            firmware
                .queues
                .render
                .entry_mut(slot)?
                .pair
                .as_mut()
                .ok_or(EIO)?
                .release_owner();
        }
        self.shared.notify_changed();
        let published = (|| {
            let entry = firmware.queues.render.entry(slot)?;
            if entry.pending_error.is_some() || entry.pending_timeout {
                // Final timeout observations can settle unrelated queues. The
                // event worker owns the full collector needed for that sweep.
                self.shared.queue_events();
                return Err(self.busy(Site::PreparePending));
            }
            if !lease.is_current()
                || firmware.recovery.pending()
                || crate::g17::recovery::Memory::recovery_state(&firmware.init)? != 0
            {
                return Err(EAGAIN);
            }
            // Private restaging can fail after preparing a larger inventory.
            // Finish its descriptor before discarding the command preparation.
            firmware.commit_render_growth(slot, &mut growth)?;
            result.map_err(|error| self.tag(Site::PreparePair, error))?;
            if packet.cancel_requested() || firmware.queues.render.entry(slot)?.closed {
                return Err(ECANCELED);
            }
            // Lend this packet's reserved credits only while the device
            // mutex excludes every other producer. Scope drop charges actual
            // producer movement and restores the remaining reservation.
            let _scope = outer.map(|claim| firmware.init.outer_scope(claim)).transpose()?;
            firmware
                .publish_render(self.owner, slot, packet, deferred)
                .map_err(|error| self.tag(Site::Publish, error))
        })();
        let result = (|| {
            if published.is_err() && !packet.is_published() {
                firmware
                    .queues
                    .render
                    .entry_mut(slot)?
                    .pair
                    .as_mut()
                    .ok_or(EIO)?
                    .discard_prepared(packet)?;
            }
            published
        })();
        let grow_ready = firmware.render_grow_ready();
        drop(state);
        if grow_ready {
            self.shared.queue_grow();
        }
        result
    }
}
