// SPDX-License-Identifier: GPL-2.0-only OR MIT
//! Experimental M5 DRM adapter using the retained M5 firmware session.
//! First-job admission is deliberately separate from general Mesa support.

use core::ops::Range;
use kernel::{bindings, c_str, macros::vtable, prelude::*, static_lock_class,
    dma_fence::{Fence, FenceContexts, FenceObject, FenceOps, RawDmaFence, UserFence},
    sync::{Arc, Mutex}, uapi, xarray};
use crate::{alloc, drm_gpu::{DrmGpu, DrmGpuParams},
    file, g17_live_boot::m5::M5Runtime, g17_resources::m5::M5FirmwareMemory,
    g17_uapi, gem, gpu, hw, m5_vm, mmu, queue};

pub(crate) type SharedM5Runtime = Arc<Mutex<M5Runtime>>;
const USER_END: u64 = (1 << 42) - 2 * mmu::UAT_PGSZ as u64;

/// Bytes of CDM control stream the witnessed M5 compute profile covers. The
/// captured stream (m5_compute_program::STREAM) is twelve words: one dispatch
/// and the terminator at +44. Nothing longer has native evidence, and a
/// longer one is what a geometry-shader prepass submits.
const M5_COMPUTE_STREAM_BYTES: u64 = 48;
static QUEUE_NAME: &CStr = c_str!("m5_queue");
static QUEUE_CLASS_KEY: Pin<&'static kernel::sync::LockClassKey> = static_lock_class!();

pub(crate) struct M5DrmGpu {
    // A strong reference prevents open DRM files from outliving the session.
    // The runtime already retains its device/module until a qualified halt;
    // this experimental ownership cycle is intentionally released by reboot.
    runtime: SharedM5Runtime,
    observed: hw::GpuIdConfig,
    ids: gpu::SequenceIDs,
}

impl M5DrmGpu {
    pub(crate) fn new(runtime: SharedM5Runtime, observed: hw::GpuIdConfig) -> Self {
        Self { runtime, observed, ids: Default::default() }
    }
}

impl DrmGpu for M5DrmGpu {
    fn init(&self) -> Result {
        if self.is_crashed() { Err(ENODEV) } else { Ok(()) }
    }
    fn ids(&self) -> &gpu::SequenceIDs { &self.ids }
    fn is_crashed(&self) -> bool { self.runtime.lock().is_crashed() }
    fn supports_vm_status(&self) -> bool { true }
    fn submission_error(&self) -> i32 { if self.is_crashed() { EIO.to_errno() } else { 0 } }
    fn update_globals(&self) {}
    fn params(&self) -> Result<DrmGpuParams> {
        let mut masks = [0; uapi::DRM_ASAHI_MAX_CLUSTERS as usize];
        for (i, mask) in self.observed.core_masks.iter().enumerate() {
            *masks.get_mut(i).ok_or(EIO)? = *mask;
        }
        Ok(DrmGpuParams {
            gpu_generation: self.observed.gpu_gen as u32,
            gpu_variant: self.observed.gpu_variant as u32,
            gpu_revision: self.observed.gpu_rev as u32,
            chip_id: 0x8142, num_dies: self.observed.num_dies,
            num_clusters_total: self.observed.num_clusters,
            num_cores_per_cluster: self.observed.num_cores, core_masks: masks,
            // Dynamic M5 clock policy is not implemented by this adapter.
            max_frequency_khz: 0, usc_generation: self.observed.usc_generation,
            gpu_hal_generation: self.observed.gpu_hal_generation as u32,
            max_commands_per_submission: 1,
        })
    }
    fn user_range(&self) -> Result<Range<u64>> { Ok(mmu::UAT_PGSZ as u64..USER_END) }
    fn unknown_page(&self) -> Result<u64> { Ok(USER_END) }
    fn base_clock_hz(&self) -> u32 { 24_000_000 }
    fn new_vm(&self, kernel_range: Range<u64>) -> Result<mmu::Vm> {
        self.runtime.lock().new_user_vm(self.ids.vm.next(), kernel_range)
    }
    fn new_queue(&self, vm: mmu::Vm,
        _ualloc: Arc<Mutex<alloc::DefaultAllocator>>,
        _ualloc_priv: Arc<Mutex<alloc::DefaultAllocator>>,
        priority: u32, usc_exec_base: u64) -> Result<KBox<dyn queue::Queue>> {
        if priority > 3 { return Err(EINVAL); }
        let window = g17_uapi::QueueUscWindow {
            base: usc_exec_base, user_start: m5_vm::DRIVER_BASE, user_end: USER_END,
        };
        window.validate().map_err(|_| EINVAL)?;
        let queue = M5Queue {
            runtime: self.runtime.clone(), vm, window,
            fences: FenceContexts::new(1, QUEUE_NAME, QUEUE_CLASS_KEY)?,
        };
        Ok(KBox::new(queue, GFP_KERNEL)?)
    }
    fn map_timestamp_buffer(&self, _bo: gem::ObjectRef, _range: Range<usize>)
        -> Result<mmu::KernelMapping> { Err(ENOTSUPP) }
}

struct M5SubmissionFence;
#[vtable]
impl FenceOps for M5SubmissionFence {
    fn get_driver_name<'a>(self: &'a FenceObject<Self>) -> &'a CStr { c_str!("asahi") }
    fn get_timeline_name<'a>(self: &'a FenceObject<Self>) -> &'a CStr { c_str!("m5-submit") }
}

/// Keep the original sync vector: creating a second output list would add an
/// allocation failure after userspace has already supplied its wait targets.
struct OutputPublication {
    fence: UserFence<M5SubmissionFence>,
    syncs: KVec<file::SyncItem>,
    input_count: usize,
    result: Result,
}

impl Drop for OutputPublication {
    fn drop(&mut self) {
        if let Err(error) = self.result { self.fence.set_error(error); }
        let fence = Fence::from_fence(&self.fence);
        for mut sync in self.syncs.drain(self.input_count..) {
            if let Some(chain) = sync.chain_fence.take() {
                sync.syncobj.add_point(chain, &fence, sync.timeline_value);
            } else { sync.syncobj.replace_fence(Some(&fence)); }
        }
        self.fence.signal();
    }
}

struct UserAddressSpace<'a> { vm: &'a mmu::Vm }
impl g17_uapi::GpuAddressSpace for UserAddressSpace<'_> {
    fn covers(&self, address: u64, size: u64, access: g17_uapi::GpuAccess) -> bool {
        let (read, write) = match access {
            g17_uapi::GpuAccess::Read => (true, false),
            g17_uapi::GpuAccess::Write => (false, true),
            g17_uapi::GpuAccess::ReadWrite => (true, true),
        };
        m5_vm::user_range_contains(&self.vm.kernel_range(), address, size, USER_END)
            && self.vm.covers_range(address, size, read, write)
    }
}

struct M5Queue {
    runtime: SharedM5Runtime,
    vm: mmu::Vm,
    window: g17_uapi::QueueUscWindow,
    fences: FenceContexts,
}

impl queue::Queue for M5Queue {
    fn submit(&mut self, _submission_id: u64, syncs: KVec<file::SyncItem>,
        in_sync_count: usize, cmdbuf_raw: &[u8],
        _objects: Pin<&xarray::XArray<KBox<file::Object>>>) -> Result {
        if in_sync_count > syncs.len() { return Err(EINVAL); }
        let fence = self.fences.new_fence(0, M5SubmissionFence)?.into();
        let mut publication = OutputPublication {
            fence, syncs, input_count: in_sync_count, result: Err(ECANCELED),
        };
        let result = (|| {
            if self.vm.status().get() != 0 { return Err(EIO); }
            // Wait outside the runtime lock; another client may own the job
            // whose fence satisfies this submission's dependency.
            for sync in &publication.syncs[..in_sync_count] {
                if let Some(fence) = &sync.fence {
                    // SAFETY: both calls borrow the strongly held input fence.
                    let waited = unsafe { bindings::dma_fence_wait_timeout(
                        fence.raw(), true, kernel::task::MAX_SCHEDULE_TIMEOUT) };
                    if waited < 0 { return Err(Error::from_errno(waited as i32)); }
                    if waited == 0 { return Err(ETIMEDOUT); }
                    let status = unsafe { bindings::dma_fence_get_status(fence.raw()) };
                    if status < 0 { return Err(Error::from_errno(status)); }
                }
            }
            let _vm_job = self.vm.retain_t8140_job()?;
            let address_space = UserAddressSpace { vm: &self.vm };
            let mut parser = g17_uapi::UapiCommandParser::new(cmdbuf_raw);
            let command = parser.next_hardware().map_err(|_| EINVAL)?.ok_or(EINVAL)?;
            if parser.next_hardware().map_err(|_| EINVAL)?.is_some() {
                pr_err!("M5 submit admission rejected: more than one hardware command\n");
                return Err(ENOTSUPP);
            }
            parser.finish().map_err(|_| EINVAL)?;
            if let g17_uapi::ParsedHardwareCommand::Compute { payload, attachments, .. } = command {
                let timestamps = [payload.timestamps.start, payload.timestamps.end];
                if payload.flags != 0 || payload.sampler_heap != 0 || payload.sampler_count != 0
                    || timestamps.iter().any(|t| t.handle != 0 || t.offset != 0) {
                    pr_err!("M5 compute admission rejected flags={:#x} sampler_heap={:#x} samplers={} timestamps={}\n",
                        payload.flags, payload.sampler_heap, payload.sampler_count,
                        timestamps.iter().any(|t| t.handle != 0 || t.offset != 0));
                    return Err(ENOTSUPP);
                }
                // The witnessed M5 compute profile is one dispatch: the
                // captured control stream is twelve words, a single dispatch
                // followed by the terminator at +44, and the descriptor
                // carries exactly one dispatch identity, one submission index
                // and one pair of dispatch stamps. A longer stream -- every
                // geometry-shader and transform-feedback prepass emits four or
                // five dispatches -- is accepted by firmware, linked into the
                // job list and never retired, so the submitting ioctl times
                // out after forty seconds and quarantines the VM. Refuse it
                // here instead: the client drops the batch and the session
                // survives, exactly as it does for an out-of-profile render.
                let stream_bytes = payload.control_stream_end
                    .saturating_sub(payload.control_stream_base);
                if stream_bytes > M5_COMPUTE_STREAM_BYTES {
                    pr_err!("M5 compute admission rejected stream_bytes={} (profile admits one dispatch, {})\n",
                        stream_bytes, M5_COMPUTE_STREAM_BYTES);
                    return Err(ENOTSUPP);
                }
                let translated = g17_uapi::translate_compute_command(payload, attachments,
                    self.window, &address_space).map_err(|_| EINVAL)?;
                return self.runtime.lock().submit_application_compute(&self.vm, &translated);
            }
            let g17_uapi::ParsedHardwareCommand::Render {
                payload, vertex_attachments, fragment_attachments, ..
            } = command else {
                pr_err!("M5 submit admission rejected: neither a render nor a compute command\n");
                return Err(ENOTSUPP);
            };
            admit_reference_render(&payload).map_err(|error| {
                pr_err!("M5 render admission rejected size={}x{} layers={} utiles={}x{} samples={}\n",
                    payload.width, payload.height, payload.layers,
                    payload.utile_width, payload.utile_height, payload.samples);
                error
            })?;
            // An attached stencil plane is admitted only while it is inert:
            // its buffers are range-checked with the depth ones and its
            // registers are written from the same parameter block, but a
            // render that loads or stores it, or that runs the stencil unit,
            // is outside the witnessed profile.
            if payload.zls_control & 0x44000 != 0 {
                pr_err!("M5 render admission rejected stencil transfer control={:#x}\n",
                    payload.zls_control);
                return Err(ENOTSUPP);
            }
            g17_uapi::validate_m5_reference_depth(&payload, &address_space)
                .map_err(|error| {
                    pr_err!("M5 depth admission rejected {:?}: size={}x{} dimensions={:#x} control={:#x} stride={:#x} meta_stride={:#x} base={:#x} meta={:#x} clear={:#x}\n",
                        error, payload.width, payload.height, payload.depth_dimensions,
                        payload.zls_control, payload.depth.stride, payload.depth.compression_stride,
                        payload.depth.base, payload.depth.compression_base, payload.depth_clear);
                    ENOTSUPP
                })?;
            let (parameters, mut controls) = M5FirmwareMemory::reference_render_profile();
            let mut translated = g17_uapi::translate_render_command(payload,
                vertex_attachments, fragment_attachments, self.window, &address_space,
                g17_uapi::RenderInternalState { parameters }).map_err(|_| EINVAL)?;
            // Native single-layer M5 ZLS does not encode an array stride.
            // The full data/metadata ranges were validated above.
            translated.parameters.depth_stride = 0;
            translated.parameters.depth_aux_stride = 0;
            // Native 25G76 indexed draws use 15021=0x10c000. The reference
            // triangle's low bit causes fragment recovery with larger
            // indexed overdraw, even after the tiling stage completes.
            // Bit18 selects the narrow depth plane: the same controlled
            // render against a Depth16Unorm attachment publishes 0x14c001
            // where the 32-bit one publishes 0x10c001, and that bit is the
            // only difference in the whole register program beyond the ZLS
            // format field and the integer clear value.
            let z16 = payload.zls_control & (3 << 25) == (2 << 25);
            translated.parameters.aux_fb_flags =
                if z16 { 0x14c000 } else { 0x10c000 };
            // M5's second store-program pair refers to the same EOT program
            // in the admitted native profile, with caller-owned program VAs.
            controls.store_pipeline_2 = translated.parameters.store_pipeline;
            controls.store_pipeline_2_bind = translated.parameters.store_pipeline_bind;
            controls.partial_store_pipeline_2 = translated.parameters.partial_store_pipeline;
            controls.partial_store_pipeline_2_bind = translated.parameters.partial_store_pipeline_bind;
            self.runtime.lock().submit_application(&self.vm, translated.parameters,
                controls, 1, [0x7d4000, 0x934000])?;
            Ok(())
        })();
        // The runtime records and quarantines a failed published VM before
        // this output fence is installed/signalled by the publication guard.
        publication.result = result;
        result
    }
}

/// Until the M5 controls are derived for other render modes, admit only the
/// reference render-state envelope. This is an experimental first-job ABI
/// path, not an assertion that arbitrary Vulkan programs are supported.
fn admit_reference_render(command: &g17_uapi::UapiRenderCommand) -> Result {
    let timestamps = [command.vertex_timestamps.start, command.vertex_timestamps.end,
        command.fragment_timestamps.start, command.fragment_timestamps.end];
    if !(1..=g17_uapi::M5_MAX_RENDER_DIMENSION).contains(&u32::from(command.width))
        || !(1..=g17_uapi::M5_MAX_RENDER_DIMENSION).contains(&u32::from(command.height))
        || command.layers != 1
        || command.utile_width != 32 || command.utile_height != 32 || command.samples != 1
        || command.sampler_heap != 0 || command.sampler_count != 0
        || command.occlusion_query_base != 0
        || timestamps.iter().any(|timestamp| timestamp.handle != 0 || timestamp.offset != 0) {
        return Err(ENOTSUPP);
    }
    Ok(())
}
