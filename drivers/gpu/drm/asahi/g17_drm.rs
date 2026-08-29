// SPDX-License-Identifier: GPL-2.0-only OR MIT

//! T8140 DRM frontend and userspace render queue.

use core::ops::Range;

use kernel::{
    bindings, c_str,
    dma_fence::{Fence, FenceContexts, FenceObject, FenceOps, RawDmaFence, UserFence},
    macros::vtable,
    prelude::*,
    static_lock_class,
    sync::{Arc, Mutex},
    uapi, xarray,
};

use crate::{
    alloc,
    driver::{AsahiDevRef, AsahiDevice},
    drm_gpu::{DrmGpu, DrmGpuParams},
    file, g17_adt_j700, g17_live_boot, g17_render, g17_uapi, gem, gpu, hw, mmu, queue,
};

const T8140_USER_START: u64 = mmu::UAT_PGSZ as u64;
const T8140_USER_TOP: u64 = 1 << 42;
const T8140_UNKNOWN_PAGE: u64 = T8140_USER_TOP - 2 * mmu::UAT_PGSZ as u64;
const T8140_TIMESTAMP_ALIAS_RANGE: Range<u64> = 0x70_0000_0000..0x71_0000_0000;

pub(crate) type SharedG17PRuntime = Arc<Mutex<g17_live_boot::G17PLiveRuntime>>;

/// DRM-visible T8140 manager. Hardware lifetime stays in the retained runtime.
pub(crate) struct G17PDrmGpu {
    dev: AsahiDevRef,
    runtime: SharedG17PRuntime,
    soc: &'static hw::agx3::SocConfig,
    observed: hw::GpuIdConfig,
    ids: gpu::SequenceIDs,
}

impl G17PDrmGpu {
    pub(crate) fn new(
        dev: &AsahiDevice,
        runtime: SharedG17PRuntime,
        soc: &'static hw::agx3::SocConfig,
        observed: hw::GpuIdConfig,
    ) -> Self {
        Self {
            dev: dev.into(),
            runtime,
            soc,
            observed,
            ids: Default::default(),
        }
    }
}

impl DrmGpu for G17PDrmGpu {
    fn init(&self) -> Result {
        Ok(())
    }

    fn ids(&self) -> &gpu::SequenceIDs {
        &self.ids
    }

    fn is_crashed(&self) -> bool {
        self.runtime.lock().is_crashed()
    }

    fn update_globals(&self) {}

    fn params(&self) -> Result<DrmGpuParams> {
        let mut core_masks = [0u32; uapi::DRM_ASAHI_MAX_CLUSTERS as usize];
        for (index, mask) in self.observed.core_masks.iter().enumerate() {
            *core_masks.get_mut(index).ok_or(EIO)? = *mask;
        }
        Ok(DrmGpuParams {
            gpu_generation: self.observed.gpu_gen as u32,
            gpu_variant: self.observed.gpu_variant as u32,
            gpu_revision: self.observed.gpu_rev as u32,
            chip_id: self.soc.chip_id,
            num_dies: self.observed.num_dies,
            num_clusters_total: self.observed.num_clusters,
            num_cores_per_cluster: self.observed.num_cores,
            core_masks,
            max_frequency_khz: g17_adt_j700::J700_PERF_STATES
                [g17_adt_j700::J700_GPU_NUM_PERF_STATES as usize]
                .freq_hz
                / 1_000,
            usc_generation: self.observed.usc_generation,
            gpu_hal_generation: self.observed.gpu_hal_generation as u32,
            max_commands_per_submission: 1,
        })
    }

    fn user_range(&self) -> Result<Range<u64>> {
        Ok(T8140_USER_START..T8140_UNKNOWN_PAGE)
    }

    fn unknown_page(&self) -> Result<u64> {
        Ok(T8140_UNKNOWN_PAGE)
    }

    fn base_clock_hz(&self) -> u32 {
        24_000_000
    }

    fn new_vm(&self, kernel_range: Range<u64>) -> Result<mmu::Vm> {
        self.runtime
            .lock()
            .new_user_vm(self.ids.vm.next(), kernel_range)
    }

    fn new_queue(
        &self,
        vm: mmu::Vm,
        _ualloc: Arc<Mutex<alloc::DefaultAllocator>>,
        _ualloc_priv: Arc<Mutex<alloc::DefaultAllocator>>,
        priority: u32,
        usc_exec_base: u64,
    ) -> Result<KBox<dyn queue::Queue>> {
        Ok(KBox::new(
            G17PQueue::new(
                &self.dev,
                self.runtime.clone(),
                vm,
                self.ids.queue.next(),
                priority,
                usc_exec_base,
            )?,
            GFP_KERNEL,
        )?)
    }

    fn map_timestamp_buffer(
        &self,
        bo: gem::ObjectRef,
        range: Range<usize>,
    ) -> Result<mmu::KernelMapping> {
        self.runtime.lock().map_timestamp_buffer(bo, range)
    }
}

struct UserAddressSpace<'a> {
    vm: &'a mmu::Vm,
    range: Range<u64>,
}

impl g17_uapi::GpuAddressSpace for UserAddressSpace<'_> {
    fn covers(&self, address: u64, size: u64, access: g17_uapi::GpuAccess) -> bool {
        let Some(end) = address.checked_add(size) else {
            return false;
        };
        let (need_read, need_write) = match access {
            g17_uapi::GpuAccess::Read => (true, false),
            g17_uapi::GpuAccess::Write => (false, true),
            g17_uapi::GpuAccess::ReadWrite => (true, true),
        };
        size != 0
            && self.range.start <= address
            && end <= self.range.end
            && self.vm.covers_range(address, size, need_read, need_write)
    }
}

#[derive(Default)]
struct G17PSubmissionFence;

#[vtable]
impl FenceOps for G17PSubmissionFence {
    fn get_driver_name<'a>(self: &'a FenceObject<Self>) -> &'a CStr {
        c_str!("asahi")
    }

    fn get_timeline_name<'a>(self: &'a FenceObject<Self>) -> &'a CStr {
        c_str!("g17p-submit")
    }
}

static QUEUE_NAME: &CStr = c_str!("g17p_queue");
static QUEUE_CLASS_KEY: core::pin::Pin<&'static kernel::sync::LockClassKey> = static_lock_class!();

/// One userspace queue bound to the retained G17P runtime.
struct G17PQueue {
    dev: AsahiDevRef,
    runtime: SharedG17PRuntime,
    vm: mmu::Vm,
    user_range: Range<u64>,
    usc_exec_base: u64,
    fence_contexts: FenceContexts,
    id: u64,
}

enum G17PPendingCommand {
    Render(g17_uapi::TranslatedRenderCommand),
    Compute(g17_uapi::TranslatedComputeCommand),
}

struct TimestampDestination {
    mapping: Arc<mmu::KernelMapping>,
    offset: usize,
}

impl G17PQueue {
    fn new(
        dev: &AsahiDevice,
        runtime: SharedG17PRuntime,
        vm: mmu::Vm,
        id: u64,
        priority: u32,
        usc_exec_base: u64,
    ) -> Result<Self> {
        if priority > 3 {
            return Err(EINVAL);
        }
        let user_range = T8140_USER_START..T8140_UNKNOWN_PAGE;
        g17_uapi::QueueUscWindow {
            base: usc_exec_base,
            user_start: user_range.start,
            user_end: user_range.end,
        }
        .validate()
        .map_err(|_| EINVAL)?;
        Ok(Self {
            dev: dev.into(),
            runtime,
            vm,
            user_range,
            usc_exec_base,
            fence_contexts: FenceContexts::new(1, QUEUE_NAME, QUEUE_CLASS_KEY)?,
            id,
        })
    }

    fn wait_input_fences(syncs: &[file::SyncItem]) -> Result {
        for sync in syncs {
            let Some(fence) = sync.fence.as_ref() else {
                continue;
            };
            // SAFETY: `fence.raw()` remains valid for the duration of this call.
            let ret = unsafe {
                bindings::dma_fence_wait_timeout(
                    fence.raw(),
                    true,
                    kernel::task::MAX_SCHEDULE_TIMEOUT,
                )
            };
            if ret < 0 {
                return Err(Error::from_errno(ret as i32));
            }
        }
        Ok(())
    }

    fn timestamp_address(
        objects: core::pin::Pin<&xarray::XArray<KBox<file::Object>>>,
        vm: &mmu::Vm,
        timestamp: g17_uapi::UapiTimestamp,
        aliases: &mut KVec<mmu::KernelMapping>,
    ) -> Result<u64> {
        if timestamp.handle == 0 {
            return Ok(0);
        }
        let guard = objects.lock();
        let object = guard
            .get(timestamp.handle.try_into()?)
            .ok_or(ENOENT)?
            .clone();
        core::mem::drop(guard);
        match object {
            file::Object::TimestampBuffer(mapping) => {
                let end = timestamp.offset.checked_add(8).ok_or(EINVAL)? as usize;
                if end > mapping.size() {
                    return Err(ERANGE);
                }
                let alias = mapping.map_alias_into_range(
                    vm,
                    T8140_TIMESTAMP_ALIAS_RANGE.clone(),
                    mmu::PROT_GPU_FW_SHARED_RW,
                )?;
                let address = alias
                    .iova()
                    .checked_add(timestamp.offset as u64)
                    .ok_or(EINVAL)?;
                aliases.push(alias, GFP_KERNEL)?;
                Ok(address)
            }
        }
    }

    fn timestamp_destination(
        objects: core::pin::Pin<&xarray::XArray<KBox<file::Object>>>,
        timestamp: g17_uapi::UapiTimestamp,
    ) -> Result<Option<TimestampDestination>> {
        if timestamp.handle == 0 {
            return Ok(None);
        }
        let guard = objects.lock();
        let object = guard
            .get(timestamp.handle.try_into()?)
            .ok_or(ENOENT)?
            .clone();
        core::mem::drop(guard);
        match object {
            file::Object::TimestampBuffer(mapping) => {
                let offset: usize = timestamp.offset.try_into()?;
                let end = offset.checked_add(8).ok_or(EINVAL)?;
                if end > mapping.size() {
                    return Err(ERANGE);
                }
                Ok(Some(TimestampDestination { mapping, offset }))
            }
        }
    }

    fn copy_timestamp(destination: Option<TimestampDestination>, value: u64) -> Result {
        let Some(destination) = destination else {
            return Ok(());
        };
        destination.mapping.with_cpu_bytes(|raw| {
            let end = destination.offset.checked_add(8).ok_or(EINVAL)?;
            let bytes = raw.get_mut(destination.offset..end).ok_or(ERANGE)?;
            bytes.copy_from_slice(&value.to_le_bytes());
            core::sync::atomic::fence(core::sync::atomic::Ordering::Release);
            Ok(())
        })
    }

    fn owned_compute_timestamps(&self) -> Result<mmu::KernelMapping> {
        // Firmware writes both completion words. A WB CPU alias can keep the
        // polling core on stale zeroes even after the GPU has completed.
        let mut object = gem::new_kernel_object_wc(&self.dev, mmu::UAT_PGSZ)?;
        object.vmap()?.memset(0);
        object.map_into_range(
            &self.vm,
            T8140_TIMESTAMP_ALIAS_RANGE.clone(),
            mmu::UAT_PGSZ as u64,
            mmu::PROT_GPU_FW_SHARED_RW,
            false,
        )
    }

    fn install_output_fences(mut syncs: impl Iterator<Item = file::SyncItem>, fence: &Fence) {
        for mut sync in syncs.by_ref() {
            if let Some(chain) = sync.chain_fence.take() {
                sync.syncobj.add_point(chain, fence, sync.timeline_value);
            } else {
                sync.syncobj.replace_fence(Some(fence));
            }
        }
    }
}

impl queue::Queue for G17PQueue {
    fn submit(
        &mut self,
        submission_id: u64,
        mut syncs: KVec<file::SyncItem>,
        in_sync_count: usize,
        cmdbuf_raw: &[u8],
        objects: core::pin::Pin<&xarray::XArray<KBox<file::Object>>>,
    ) -> Result {
        if in_sync_count > syncs.len() {
            return Err(EINVAL);
        }
        let address_space = UserAddressSpace {
            vm: &self.vm,
            range: self.user_range.clone(),
        };
        let window = g17_uapi::QueueUscWindow {
            base: self.usc_exec_base,
            user_start: self.user_range.start,
            user_end: self.user_range.end,
        };
        let mut parser = g17_uapi::UapiCommandParser::new(cmdbuf_raw);
        let mut pending = None;
        while let Some(command) = parser.next_hardware().map_err(|_| EINVAL)? {
            if pending.is_some() {
                return Err(ENOTSUPP);
            }
            pending = Some(match command {
                g17_uapi::ParsedHardwareCommand::Render {
                    payload,
                    vertex_attachments,
                    fragment_attachments,
                    ..
                } => G17PPendingCommand::Render(
                    g17_uapi::translate_render_command(
                        payload,
                        vertex_attachments,
                        fragment_attachments,
                        window,
                        &address_space,
                        g17_uapi::RenderInternalState {
                            parameters: g17_render::G17pRenderParameters::source_rsrc8(),
                        },
                    )
                    .map_err(|_| EINVAL)?,
                ),
                g17_uapi::ParsedHardwareCommand::Compute {
                    payload,
                    attachments,
                    ..
                } => G17PPendingCommand::Compute(
                    g17_uapi::translate_compute_command(
                        payload,
                        attachments,
                        window,
                        &address_space,
                    )
                    .map_err(|_| EINVAL)?,
                ),
            });
        }
        parser.finish().map_err(|_| EINVAL)?;
        let pending = pending.ok_or(EINVAL)?;

        Self::wait_input_fences(&syncs[..in_sync_count])?;
        let output_fence: UserFence<G17PSubmissionFence> = self
            .fence_contexts
            .new_fence(0, G17PSubmissionFence)?
            .into();
        match pending {
            G17PPendingCommand::Render(translated) => {
                let mut timestamp_aliases = KVec::new();
                let timestamps = [
                    Self::timestamp_address(
                        objects,
                        &self.vm,
                        translated.vertex_timestamps.start,
                        &mut timestamp_aliases,
                    )?,
                    Self::timestamp_address(
                        objects,
                        &self.vm,
                        translated.vertex_timestamps.end,
                        &mut timestamp_aliases,
                    )?,
                    Self::timestamp_address(
                        objects,
                        &self.vm,
                        translated.fragment_timestamps.start,
                        &mut timestamp_aliases,
                    )?,
                    Self::timestamp_address(
                        objects,
                        &self.vm,
                        translated.fragment_timestamps.end,
                        &mut timestamp_aliases,
                    )?,
                ];
                self.runtime.lock().submit_translated_render(
                    &self.vm,
                    &translated,
                    timestamps,
                    timestamp_aliases,
                )?;
            }
            G17PPendingCommand::Compute(translated) => {
                let start = Self::timestamp_destination(objects, translated.timestamps.start)?;
                let end = Self::timestamp_destination(objects, translated.timestamps.end)?;
                let owned_timestamps = self.owned_compute_timestamps()?;
                let completion = self
                    .runtime
                    .lock()
                    .submit_translated_compute(&self.vm, &translated, owned_timestamps)?;
                Self::copy_timestamp(start, completion[0])?;
                Self::copy_timestamp(end, completion[1])?;
            }
        }

        output_fence.signal();
        let fence = Fence::from_fence(&output_fence);
        let outputs = syncs.drain(in_sync_count..);
        Self::install_output_fences(outputs, &fence);
        dev_dbg!(
            self.dev.as_ref(),
            "G17P queue {} completed synchronous submission {} (one hardware command)\n",
            self.id,
            submission_id
        );
        Ok(())
    }
}
