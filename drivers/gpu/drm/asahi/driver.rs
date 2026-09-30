// SPDX-License-Identifier: GPL-2.0-only OR MIT

//! Top-level GPU driver implementation.

use kernel::{
    bindings, c_str,
    device::Core,
    dma::{
        Device,
        DmaMask, //
    },
    drm,
    drm::ioctl,
    new_mutex,
    of,
    platform,
    prelude::*,
    sync::{
        aref::ARef,
        Arc, //
    },
    time::{delay::fsleep, Delta},
    workqueue::{self, HasWork, impl_has_work, new_work, Work, WorkItem}, //
};

use crate::{
    agx_power_recovery,
    debug,
    drm_gpu,
    file,
    g15_boot,
    g17_boot,
    g17_firmware,
    g17_power_preflight,
    g18_live_boot,
    gem::AsahiObject,
    gpu,
    hw,
    identity,
    regs, //
};

use kernel::macros::vtable;

/// Holds a reference to the top-level `GpuManager` object.
#[pin_data]
pub(crate) struct AsahiData {
    #[pin]
    pub(crate) gpu: Arc<dyn drm_gpu::DrmGpu>,
    pub(crate) pdev: ARef<platform::Device>,
    pub(crate) resources: Option<regs::Resources>,
}

unsafe impl Send for AsahiData {}
unsafe impl Sync for AsahiData {}

impl_has_work! {
    impl HasWork<AsahiDevice, 1> for AsahiData { self.g17_event_work }
    impl HasWork<AsahiDevice, 2> for AsahiData { self.g17_recovery_work }
}

impl WorkItem<1> for AsahiData {
    type Pointer = ARef<AsahiDevice>;

    fn run(dev: ARef<AsahiDevice>) {
        let Some(runtime) = dev.g17_runtime else {
            return;
        };
        if let Err(error) = runtime.service_primary_firmware_events() {
            dev_err!(
                dev.as_ref(),
                "G17P event worker: serialized drain/recovery failed ({:?})\n",
                error
            );
            return;
        }
        if runtime.primary_device_control_backpressured() {
            // Full-ring backpressure is exceptional. Release the runtime
            // mutex before yielding so submissions and firmware progress are
            // never serialized behind this retry.
            fsleep(Delta::from_millis(1));
        }
        if runtime.primary_recovery_pending() {
            queue_g17p_recovery_worker(dev);
        } else {
            requeue_g17p_firmware_event_worker_if_pending(dev);
        }
    }
}

impl WorkItem<2> for AsahiData {
    type Pointer = ARef<AsahiDevice>;

    fn run(dev: ARef<AsahiDevice>) {
        let Some(runtime) = dev.g17_runtime else {
            return;
        };
        if let Err(error) = runtime.service_primary_recovery() {
            dev_err!(
                dev.as_ref(),
                "G17P recovery worker: serialized recovery failed ({:?})\n",
                error
            );
            return;
        }
        if runtime.primary_firmware_event_pending() {
            requeue_g17p_firmware_event_worker_if_pending(dev);
        } else if runtime.primary_recovery_pending() {
            queue_g17p_recovery_worker(dev);
        }
    }
}

pub(crate) fn queue_g17p_firmware_event_worker(dev: ARef<AsahiDevice>) {
    let _ = workqueue::system_highpri().enqueue::<ARef<AsahiDevice>, 1>(dev);
}

fn requeue_g17p_firmware_event_worker_if_pending(dev: ARef<AsahiDevice>) {
    let Some(runtime) = dev.g17_runtime else {
        return;
    };
    let runtime = runtime.mutex().lock();
    if runtime.firmware_event_worker_armed() && runtime.primary_firmware_event_pending() {
        // Hold the same mutex used by disarm before queueing. Unbind therefore
        // either observes this work in cancel_work_sync, or disarms first and
        // makes this final self-requeue impossible.
        let _ = workqueue::system_highpri().enqueue::<ARef<AsahiDevice>, 1>(dev);
    }
}

pub(crate) fn queue_g17p_recovery_worker(dev: ARef<AsahiDevice>) {
    let _ = workqueue::system_highpri().enqueue::<ARef<AsahiDevice>, 2>(dev);
}

struct G17PDriverUnbind<'a> {
    runtime: &'a g17_drm::SharedG17PRuntime,
    drm: ARef<AsahiDevice>,
}

impl g17_lifecycle::DriverUnbindQuiesce for G17PDriverUnbind<'_> {
    fn disarm_ingress(&self) {
        self.runtime.lock().disarm_firmware_event_worker();
    }

    fn cancel_event_work(&self) {
        cancel_g17p_work::<1>(&self.drm);
    }

    fn cancel_recovery_work(&self) {
        cancel_g17p_work::<2>(&self.drm);
    }
}

fn cancel_g17p_work<const ID: u64>(dev: &AsahiDevice)
where
    AsahiDevice: HasWork<AsahiDevice, ID>,
{
    let dev_ptr = (dev as *const AsahiDevice).cast_mut();
    // SAFETY: `dev` remains strongly referenced by the unbind owner for this
    // whole call. The HasWork implementation returns its embedded work item,
    // and cancel_work_sync waits until no callback owns that item.
    let work = unsafe {
        <AsahiDevice as HasWork<AsahiDevice, ID>>::raw_get_work(dev_ptr)
    };
    // SAFETY: `work` is the initialized embedded work item selected by ID.
    let work = unsafe { Work::raw_get(work) };
    // SAFETY: no callback can be newly queued after ingress was disarmed, and
    // the strong `drm` reference keeps the containing object alive.
    unsafe { bindings::cancel_work_sync(work) };
}

#[allow(dead_code)]
enum AsahiRuntime {
    Legacy(ARef<drm::Device<AsahiDriver>>),
    G17P(g17_drm::SharedG17PRuntime),
    M5(g17_live_boot::m5::M5Runtime),
    M5Drm(crate::m5_drm::SharedM5Runtime),
    /// T8152/G18G runtime returned by the G18 live-boot entry point.
    G18(Arc<dyn G18Runtime>),
}

/// Owner object returned by the G18 live-boot module.
///
/// The live-boot module implements this for its session type. The driver
/// keeps it for the device's lifetime and calls [`G18Runtime::shutdown`] on
/// unbind, before devres releases the SGX mapping and the mailbox links.
pub(crate) trait G18Runtime: Send + Sync {
    /// Quiesce the firmware, both ASC processors and every host mapping.
    fn shutdown(&self);
}

/// Everything the T8152 probe gate hands to the G18 live-boot entry point.
///
/// Built only by [`prepare_t8152_device`], and only when `g18_initdata_test`
/// is non-zero. Holding one means:
/// - the three GPU PSD services (sgx 3, gfx-asc 81, gfx1-asc 82) were read
///   once, read-only, and their samples are in [`T8152Probe::psd`];
/// - the SGX window is mapped in `registers`. Its ID block was read only if
///   the sgx PSD sample proved the block powered: `observed` is `Some`
///   exactly then;
/// - for the firmware stages 2..4 the loader's G18G firmware receipt matched
///   and the 42-bit DMA mask is set. Stages 1 and 90 require neither.
///
/// No firmware, ASC CPU, PSD request, DMA or UAT state has been touched.
#[allow(dead_code)]
pub(crate) struct T8152Probe {
    /// The matched SoC entry (`hw::agx3::T8152`).
    pub(crate) soc: &'static hw::agx3::SocConfig,
    /// Static identity decoded from the SoC entry (G18G, HAL400, SKSM).
    pub(crate) identity: identity::GpuIdentity,
    /// The `g18_initdata_test` stage, always non-zero here.
    pub(crate) stage: u32,
    /// Whether /chosen `aurora,g18g-gpu-firmware-profile` matched.
    pub(crate) firmware_profile_admitted: bool,
    /// Read-only samples of PSD services 3, 81 and 82, in that order.
    pub(crate) psd: [regs::T8152PsdSample; 3],
    /// SGX mapping. Every further SGX read must again follow a powered
    /// `regs::t8152_prove_sgx_powered()` result.
    pub(crate) registers: regs::Resources,
    /// Hardware identity, only when the SGX PSD proved the block powered.
    pub(crate) observed: Option<hw::GpuIdConfig>,
    /// Unregistered DRM device whose data slot is still uninitialized, as
    /// on M5. The live-boot module initializes it before any registration.
    pub(crate) drm: ARef<AsahiDevice>,
}

/// Loader receipt the G18G firmware stages require. The loader writes it
/// only after hashing both loaded TEXT images (b000 GFX 0x50000, b010 GFX1
/// 0x54000) against the 26A428 profile (plan step 0.2).
const G18G_FIRMWARE_PROFILE: &[u8] = b"G18G-RTKit-3514.0.15.release\0";

pub(crate) struct AsahiDriver {
    runtime: AsahiRuntime,
}

/// A matched device either has a complete legacy hardware configuration or is
/// an exact AGX3 identification target whose firmware boundary remains closed.
pub(crate) enum ProbeConfig {
    Supported(&'static hw::HwConfig),
    /// AGX3 (G15/G16/G17) target: chip identification and the read-only
    /// topology decode run at probe time, then the probe fails closed before
    /// DMA setup, ASC start, or any MMIO write.
    Agx3Diagnostic(&'static hw::agx3::SocConfig),
}

/// A matched device either has a complete legacy hardware configuration or is
/// an exact AGX3 identification target whose firmware boundary remains closed.
pub(crate) enum ProbeConfig {
    Supported(&'static hw::HwConfig),
    /// AGX3 (G15/G16/G17) target: chip identification and the read-only
    /// topology decode run at probe time, then the probe fails closed before
    /// DMA setup, ASC start, or any MMIO write.
    Agx3Diagnostic(&'static hw::agx3::SocConfig),
}

unsafe impl Send for AsahiDriver {}
unsafe impl Sync for AsahiDriver {}

/// Convenience type alias for the DRM device type for this driver.
pub(crate) type AsahiDevice = drm::device::Device<AsahiDriver>;
pub(crate) type AsahiDevRef = ARef<AsahiDevice>;

/// DRM Driver metadata
const INFO: drm::driver::DriverInfo = drm::driver::DriverInfo {
    major: 0,
    minor: 1,
    patchlevel: 0,
    name: c_str!("asahi"),
    desc: c_str!("Apple AGX Graphics"),
};

/// DRM Driver implementation for `AsahiDriver`.
#[vtable]
impl drm::driver::Driver for AsahiDriver {
    /// Our `DeviceData` type, reference-counted
    type Data = AsahiData;
    /// Our `File` type.
    type File = file::File;
    /// Our `Object` type.
    type Object = drm::gem::shmem::Object<AsahiObject>;

    const INFO: drm::driver::DriverInfo = INFO;
    const FEATURES: u32 = drm::driver::FEAT_GEM
        | drm::driver::FEAT_RENDER
        | drm::driver::FEAT_SYNCOBJ
        | drm::driver::FEAT_SYNCOBJ_TIMELINE
        | drm::driver::FEAT_GEM_GPUVA;

    kernel::declare_drm_ioctls! {
        (ASAHI_GET_PARAMS,      drm_asahi_get_params,
                          ioctl::RENDER_ALLOW, crate::file::File::get_params),
        (ASAHI_GET_TIME,        drm_asahi_get_time,
            ioctl::AUTH | ioctl::RENDER_ALLOW, crate::file::File::get_time),
        (ASAHI_VM_CREATE,       drm_asahi_vm_create,
            ioctl::AUTH | ioctl::RENDER_ALLOW, crate::file::File::vm_create),
        (ASAHI_VM_DESTROY,      drm_asahi_vm_destroy,
            ioctl::AUTH | ioctl::RENDER_ALLOW, crate::file::File::vm_destroy),
        (ASAHI_VM_BIND,         drm_asahi_vm_bind,
            ioctl::AUTH | ioctl::RENDER_ALLOW, crate::file::File::vm_bind),
        (ASAHI_GEM_CREATE,      drm_asahi_gem_create,
            ioctl::AUTH | ioctl::RENDER_ALLOW, crate::file::File::gem_create),
        (ASAHI_GEM_MMAP_OFFSET, drm_asahi_gem_mmap_offset,
            ioctl::AUTH | ioctl::RENDER_ALLOW, crate::file::File::gem_mmap_offset),
        (ASAHI_GEM_BIND_OBJECT, drm_asahi_gem_bind_object,
            ioctl::AUTH | ioctl::RENDER_ALLOW, crate::file::File::gem_bind_object),
        (ASAHI_QUEUE_CREATE,    drm_asahi_queue_create,
            ioctl::AUTH | ioctl::RENDER_ALLOW, crate::file::File::queue_create),
        (ASAHI_QUEUE_DESTROY,   drm_asahi_queue_destroy,
            ioctl::AUTH | ioctl::RENDER_ALLOW, crate::file::File::queue_destroy),
        (ASAHI_SUBMIT,          drm_asahi_submit,
            ioctl::AUTH | ioctl::RENDER_ALLOW, crate::file::File::submit),
    }
}

// OF Device ID table.s
kernel::of_device_table!(
    OF_TABLE,
    MODULE_OF_TABLE,
    <AsahiDriver as platform::Driver>::IdInfo,
    [
        (
            of::DeviceId::new(c_str!("apple,agx-t8103")),
            ProbeConfig::Supported(&hw::t8103::HWCONFIG)
        ),
        (
            of::DeviceId::new(c_str!("apple,agx-t8112")),
            ProbeConfig::Supported(&hw::t8112::HWCONFIG)
        ),
        (
            of::DeviceId::new(c_str!("apple,agx-t6000")),
            ProbeConfig::Supported(&hw::t600x::HWCONFIG_T6000)
        ),
        (
            of::DeviceId::new(c_str!("apple,agx-t6001")),
            ProbeConfig::Supported(&hw::t600x::HWCONFIG_T6001)
        ),
        (
            of::DeviceId::new(c_str!("apple,agx-t6002")),
            ProbeConfig::Supported(&hw::t600x::HWCONFIG_T6002)
        ),
        (
            of::DeviceId::new(c_str!("apple,agx-t6020")),
            ProbeConfig::Supported(&hw::t602x::HWCONFIG_T6020)
        ),
        (
            of::DeviceId::new(c_str!("apple,agx-t6021")),
            ProbeConfig::Supported(&hw::t602x::HWCONFIG_T6021)
        ),
        (
            of::DeviceId::new(c_str!("apple,agx-t6022")),
            ProbeConfig::Supported(&hw::t602x::HWCONFIG_T6022)
        ),
        // AGX3 (G15/G16/G17) identification targets. These intentionally
        // select no HwConfig: unproved power/MMIO/firmware-tuning fields
        // must never be represented by copied or zero-filled legacy values,
        // and the probe fails closed before any firmware handoff.
        (
            of::DeviceId::new(c_str!("apple,agx-t8122")),
            ProbeConfig::Agx3Diagnostic(&hw::agx3::T8122)
        ),
        (
            of::DeviceId::new(c_str!("apple,agx-t6030")),
            ProbeConfig::Agx3Diagnostic(&hw::agx3::T6030)
        ),
        (
            of::DeviceId::new(c_str!("apple,agx-t6031")),
            ProbeConfig::Agx3Diagnostic(&hw::agx3::T6031)
        ),
        (
            of::DeviceId::new(c_str!("apple,agx-t6032")),
            ProbeConfig::Agx3Diagnostic(&hw::agx3::T6032)
        ),
        (
            of::DeviceId::new(c_str!("apple,agx-t8132")),
            ProbeConfig::Agx3Diagnostic(&hw::agx3::T8132)
        ),
        (
            of::DeviceId::new(c_str!("apple,agx-t6040")),
            ProbeConfig::Agx3Diagnostic(&hw::agx3::T6040)
        ),
        (
            of::DeviceId::new(c_str!("apple,agx-t6041")),
            ProbeConfig::Agx3Diagnostic(&hw::agx3::T6041)
        ),
        // A18 Pro / D93AP. Reserve both the Linux-style handoff compatible and
        // the raw ADT-compatible form as diagnostic-only until the complete
        // G17P firmware lifecycle is proven on the physical J700 target.
        (
            of::DeviceId::new(c_str!("apple,agx-t8140")),
            ProbeConfig::Agx3Diagnostic(&hw::agx3::T8140)
        ),
        (
            of::DeviceId::new(c_str!("gpu,t8140")),
            ProbeConfig::Agx3Diagnostic(&hw::agx3::T8140)
        ),
        (
            of::DeviceId::new(c_str!("apple,agx-t8142")),
            ProbeConfig::Agx3Diagnostic(&hw::agx3::T8142)
        ),
        // M6 / J873. Refuses with ENODEV unless g18_initdata_test is set.
        (
            of::DeviceId::new(c_str!("apple,agx-t8152")),
            ProbeConfig::Agx3Diagnostic(&hw::agx3::T8152)
        ),
        (
            of::DeviceId::new(c_str!("apple,agx-t6050")),
            ProbeConfig::Agx3Diagnostic(&hw::agx3::T6050)
        ),
        (
            of::DeviceId::new(c_str!("apple,agx-t6051")),
            ProbeConfig::Agx3Diagnostic(&hw::agx3::T6051)
        ),
        // The literal compatible captured from the J714s/T6050 ADT.
        (
            of::DeviceId::new(c_str!("gpu,t6050")),
            ProbeConfig::Agx3Diagnostic(&hw::agx3::T6050)
        ),
    ]
);

/// Start the persistent runtime for an exact T8140/G17P match.
fn probe_t8140_runtime(
    pdev: &platform::Device<Core>,
    soc: &'static hw::agx3::SocConfig,
) -> Result<g17_drm::SharedG17PRuntime> {
    let expected = identity::decode_gpu_identity(soc.hw_family, soc.hw_variant, soc.num_dies as u8)
        .ok_or(EINVAL)?;
    if soc.chip_id != 0x8140
        || expected.gpu_gen != hw::GpuGen::G17
        || expected.gpu_variant != identity::GpuVariant::P
        || expected.uat_input_address_bits != soc.uat_ias
    {
        return Err(EINVAL);
    }

    // Topology is read and admitted before mappings, DMA setup, or provider
    // start. The token is consumed by the retained runtime constructor.
    let topology = admit_t8140_live_topology(pdev, &expected)?;
    let registers = regs::Resources::new(pdev)?;
    let observed = registers.get_gpu_id()?;
    if observed.gpu_gen != soc.gpu_gen
        || observed.gpu_variant != soc.gpu_variant
        || observed.num_dies != soc.num_dies
    {
        dev_err!(
            pdev.as_ref(),
            "G17P: hardware identity {:?}{:?}/{} dies does not match exact T8140 {:?}{:?}/{} dies\n",
            observed.gpu_gen,
            observed.gpu_variant,
            observed.num_dies,
            soc.gpu_gen,
            soc.gpu_variant,
            soc.num_dies
        );
        return Err(ENODEV);
    }

    unsafe { pdev.dma_set_mask_and_coherent(DmaMask::try_new(soc.uat_oas)?)? };

    // The retained manager allocates its GEM objects from this DRM device.
    // Registration follows runtime and adapter initialization below.
    let uninit = unsafe {
        pin_init::pin_init_from_closure::<AsahiData, kernel::error::Error>(|_slot| Ok(()))
    };
    let drm: ARef<AsahiDevice> = drm::device::Device::new(pdev.as_ref(), uninit)?;

    let runtime = g17_live_boot::G17PLiveRuntime::start(
        pdev,
        drm.clone(),
        registers,
        soc,
        &expected,
        &observed,
        topology,
    )?;
    let runtime = Arc::pin_init(new_mutex!(runtime), GFP_KERNEL)?;
    let runtime_ref = g17_drm::G17PRuntimeRef::new(&runtime);
    let gpu: Arc<dyn drm_gpu::DrmGpu> = Arc::new(
        g17_drm::G17PDrmGpu::new(runtime_ref, soc, observed)?,
        GFP_KERNEL,
    )?;
    let data = try_pin_init!(AsahiData {
        gpu,
        pdev: pdev.into(),
        resources: None,
        g17_runtime: Some(runtime_ref),
        g17_event_work <- new_work!("AsahiData::g17_event_work"),
        g17_recovery_work <- new_work!("AsahiData::g17_recovery_work"),
    });
    let ptr: *const AsahiData = &raw const **drm;
    // SAFETY: `drm` was created with an uninitialized data slot above; this
    // is its only initialization before registration exposes file operations.
    unsafe {
        data.__pinned_init(ptr as *mut AsahiData)?;
    }
    runtime.lock().arm_primary_firmware_event_worker()?;
    runtime.lock().mark_firmware_cache_flush_ready()?;
    (*drm).gpu.init()?;
    drm::driver::Registration::new_foreign_owned(&drm, pdev.as_ref(), 0)?;
    Ok(runtime)
}

fn prepare_t8142_device(pdev: &platform::Device<Core>)
    -> Result<(ARef<AsahiDevice>, regs::Resources, hw::GpuIdConfig)> {
    // The RAM loader supplies this receipt only after checking both loaded
    // TEXT images against the exact M5 profile, including all instructions.
    let chosen = of::chosen().ok_or(ENODEV)?;
    let profile: KVec<u8> = chosen.get_property(c_str!("aurora,m5-gpu-firmware-profile"))?;
    if profile.as_slice() != b"M5-RTKit-3255.160.4.release\0" {
        return Err(ENODEV);
    }
    let registers = regs::Resources::new(pdev)?;
    let observed = registers.get_gpu_id()?;
    if observed.gpu_gen != hw::GpuGen::G17 || observed.gpu_variant != hw::GpuVariant::G
        || observed.num_dies != 1 || observed.total_active_cores != 10
    {
        return Err(ENODEV);
    }
    unsafe { pdev.dma_set_mask_and_coherent(DmaMask::try_new(42)?)? };
    // This device is deliberately unregistered. The DRM release path does
    // not drop its uninitialized data slot; no callback reads that slot.
    let uninit = unsafe {
        pin_init::pin_init_from_closure::<AsahiData, kernel::error::Error>(|_slot| Ok(()))
    };
    let drm: ARef<AsahiDevice> = drm::device::Device::new(pdev.as_ref(), uninit)?;
    Ok((drm, registers, observed))
}

/// T8152 probe gate. With `g18_initdata_test=0` (the default) this is a
/// complete no-op: it logs and refuses with ENODEV before any MMIO, DMA
/// mask, reserved-memory, mailbox or processor access, just as M5 refuses by
/// default. Unlike the M5 default it also skips the read-only SGX identity
/// probe, because J873 cold-boots with the GPU PSD services idle, where an
/// SGX read can raise an SError.
fn probe_t8152(
    pdev: &platform::Device<Core>,
    soc: &'static hw::agx3::SocConfig,
) -> Result<Arc<dyn G18Runtime>> {
    let stage = *crate::module_parameters::g18_initdata_test.value();
    if stage == 0 {
        dev_info!(
            pdev.as_ref(),
            "G18G: T8152 matched; g18_initdata_test=0 refuses before any MMIO, DMA, mailbox or processor access\n"
        );
        return Err(ENODEV);
    }
    if !g18_live_boot::stage_is_known(stage) {
        dev_err!(
            pdev.as_ref(),
            "G18G: g18_initdata_test={} is not a known stage (1-9, 90-93); refused before any MMIO\n",
            stage
        );
        return Err(EINVAL);
    }
    let variant = *crate::module_parameters::g18_compute_variant.value();
    if variant > 2 || (variant != 0 && stage != g18_live_boot::STAGE_COMPUTE_ADD) {
        dev_err!(
            pdev.as_ref(),
            "G18G: g18_compute_variant={} needs 0, or 1/2 with g18_initdata_test=8; refused before any MMIO\n",
            variant
        );
        return Err(EINVAL);
    }
    let render_variant = *crate::module_parameters::g18_render_variant.value();
    let render_profile = *crate::module_parameters::g18_render_profile.value();
    let sma_minimum = *crate::module_parameters::g18_sma_minimum.value();
    let sma_ideal = *crate::module_parameters::g18_sma_ideal.value();
    let native_barrier = *crate::module_parameters::g18_native_barrier.value();
    if native_barrier > 1 || (native_barrier != 0 && (sma_ideal != 1 || sma_minimum != 0 ||
        stage != crate::g18_render_stage::STAGE_RENDER || render_variant != 0 || render_profile != 4)) {
        dev_err!(pdev.as_ref(), "G18G: native barrier probe requires ideal SMA stage9 variant0 profile4; refused before MMIO\n");
        return Err(EINVAL);
    }
    if sma_ideal > 1 || (sma_ideal != 0 && (sma_minimum != 0 ||
        stage != crate::g18_render_stage::STAGE_RENDER || render_variant != 0 || render_profile != 4)) {
        dev_err!(pdev.as_ref(), "G18G: SMA ideal probe requires stage9 variant0 profile4 and minimum=0; refused before MMIO\n");
        return Err(EINVAL);
    }
    // Explicit fixed-minimum comparison for the retained first application
    // draw, as well as the original H1 witness. This is not dynamic sizing.
    let minimum_probe = render_variant == 0 &&
        ((stage == crate::g18_render_stage::STAGE_RENDER && render_profile == 4) ||
         (stage == g18_live_boot::STAGE_DRM && render_profile == 1));
    let stall_trace = *crate::module_parameters::g18_graphics_stall_trace.value();
    if stall_trace > 1 || (stall_trace != 0 && !minimum_probe) {
        dev_err!(pdev.as_ref(), "G18G: graphics stall tracing requires stage9/profile4 or stage10/profile1, variant0; refused before MMIO\n");
        return Err(EINVAL);
    }
    let high_vdm = *crate::module_parameters::g18_vdm_high_alias.value();
    if high_vdm > 1 || (high_vdm != 0 &&
        (stage != crate::g18_render_stage::STAGE_RENDER || render_variant != 0 ||
         render_profile != 4 || sma_minimum != 1 || sma_ideal != 0 || stall_trace != 1)) {
        dev_err!(pdev.as_ref(), "G18G: high VDM alias requires stage9/profile4/variant0, minimum SMA and stall tracing; refused before MMIO\n");
        return Err(EINVAL);
    }
    if sma_minimum > 1 || (sma_minimum != 0 && !minimum_probe) {
        dev_err!(pdev.as_ref(), "G18G: SMA minimum probe requires stage9/profile4 or stage10/profile1, variant0; refused before MMIO\n");
        return Err(EINVAL);
    }
    let usc_statistics = *crate::module_parameters::g18_usc_statistics.value();
    if usc_statistics > 1 || (usc_statistics != 0 &&
        (stage != crate::g18_render_stage::STAGE_RENDER || render_variant != 0)) {
        dev_err!(pdev.as_ref(), "G18G: USC statistics require the stage9 colour fixture; refused before MMIO\n");
        return Err(EINVAL);
    }
    if stage == crate::g18_render_stage::STAGE_RENDER {
        crate::g18_render_stage::admit(pdev.as_ref(), render_variant, render_profile)?;
    } else if render_variant != 0 || render_profile != 1 {
        return Err(EINVAL);
    }
    if stage == g18_live_boot::STAGE_DRM {
        if *crate::module_parameters::g18_admit_compute.value() != 0 {return Err(ENOTSUPP);}
        return probe_t8152_drm(pdev, soc, stage);
    }
    let probe = prepare_t8152_device(pdev, soc, stage)?;
    g18_live_boot_start(pdev, probe)
}

/// Register the G18 DRM render node over a started G18 session.
fn probe_t8152_drm(
    pdev: &platform::Device<Core>,
    soc: &'static hw::agx3::SocConfig,
    stage: u32,
) -> Result<Arc<dyn G18Runtime>> {
    let mut probe = prepare_t8152_device(pdev, soc, stage)?;
    let drm = probe.drm.clone();
    let observed = probe.observed.take();
    let (owner, runtime) = g18_live_boot::start_drm(pdev, probe)?;
    let config = crate::g18_drm::G18DrmConfig {
        admit_compute: *crate::module_parameters::g18_admit_compute.value() != 0,
    };
    let gpu: Arc<dyn drm_gpu::DrmGpu> = Arc::new(
        crate::g18_drm::G18DrmGpu::new(runtime, observed.as_ref(), config)?,
        GFP_KERNEL,
    )?;
    let data = try_pin_init!(AsahiData {
        gpu, pdev: pdev.into(), resources: None, g17_runtime: None,
        g17_event_work <- new_work!("AsahiData::g17_event_work"),
        g17_recovery_work <- new_work!("AsahiData::g17_recovery_work"),
    });
    let ptr: *const AsahiData = &raw const **drm;
    // SAFETY: prepare_t8152_device leaves this data slot uninitialized. This
    // is its sole initialization, before registration exposes file operations.
    unsafe { data.__pinned_init(ptr as *mut AsahiData)?; }
    (*drm).gpu.init()?;
    drm::driver::Registration::new_foreign_owned(&drm, pdev.as_ref(), 0)?;
    dev_info!(pdev.as_ref(), "G18_DRM registered render node (asahi, apple,agx-t8152)\n");
    Ok(owner)
}

fn prepare_t8152_device(
    pdev: &platform::Device<Core>,
    soc: &'static hw::agx3::SocConfig,
    stage: u32,
) -> Result<T8152Probe> {
    let identity =
        identity::decode_gpu_identity(soc.hw_family, soc.hw_variant, soc.num_dies as u8)
            .ok_or(EINVAL)?;
    if soc.chip_id != 0x8152
        || identity.gpu_gen != hw::GpuGen::G18
        || identity.gpu_variant != identity::GpuVariant::G
        || identity.uat_input_address_bits != soc.uat_ias
    {
        return Err(EINVAL);
    }
    dev_info!(
        pdev.as_ref(),
        "G18G: T8152 stage {}: {:?}{:?} USC gen {} HAL {:?} {:?} roles {:?} submission, {}-bit UAT roots\n",
        stage,
        identity.gpu_gen,
        identity.gpu_variant,
        identity.usc_generation,
        identity.gpu_hal_generation,
        identity.firmware_roles,
        identity.submission_transport,
        soc.uat_ias
    );

    let firmware_profile_admitted = match of::chosen() {
        Some(chosen) => {
            let profile: Result<KVec<u8>> =
                chosen.get_property(c_str!("aurora,g18g-gpu-firmware-profile"));
            matches!(profile, Ok(profile) if profile.as_slice() == G18G_FIRMWARE_PROFILE)
        }
        None => false,
    };
    dev_info!(
        pdev.as_ref(),
        "G18G: firmware receipt {}\n",
        (if firmware_profile_admitted { "admitted" } else { "absent or different" })
    );
    if g18_live_boot::stage_runs_firmware(stage) && !firmware_profile_admitted {
        dev_err!(
            pdev.as_ref(),
            "G18G: stage {} needs /chosen aurora,g18g-gpu-firmware-profile = G18G-RTKit-3514.0.15.release\n",
            stage
        );
        return Err(ENODEV);
    }

    // PSD state first, read-only. Nothing below reads SGX unless the sgx
    // service proves the block active and acknowledged.
    let mut psd = [regs::T8152PsdSample {
        service: regs::T8152PsdService::Sgx,
        state: 0,
        ack: 0,
    }; 3];
    for (slot, service) in psd.iter_mut().zip(regs::T8152PsdService::ALL) {
        *slot = regs::t8152_read_psd(pdev, service)?;
        dev_info!(
            pdev.as_ref(),
            "G18G: PSD service {} ({:#x}) state={:#x} power state {} ack={:#x} -> {}\n",
            service.service_id(),
            service.physical_base(),
            slot.state,
            slot.power_state(),
            slot.ack,
            (if slot.powered() { "powered" } else { "not powered" })
        );
    }

    let registers = regs::Resources::new(pdev)?;
    let observed = match regs::t8152_prove_sgx_powered(pdev) {
        Ok(powered) => Some(registers.get_gpu_id_t8152(&powered, soc)?),
        Err(error) if error == regs::t8152_not_powered() => {
            dev_info!(
                pdev.as_ref(),
                "G18G: SGX identity not read (sgx PSD idle); identified by chip id {:#x} and compatible\n",
                soc.chip_id
            );
            None
        }
        Err(error) => return Err(error),
    };

    if g18_live_boot::stage_runs_firmware(stage) {
        // J873 DRAM ends at 0x106_0000_0000, inside a 42-bit output space.
        unsafe { pdev.dma_set_mask_and_coherent(DmaMask::try_new(soc.uat_oas)?)? };
    }

    // This device is deliberately unregistered. The DRM release path does
    // not drop its uninitialized data slot; no callback reads that slot.
    let uninit = unsafe {
        pin_init::pin_init_from_closure::<AsahiData, kernel::error::Error>(|_slot| Ok(()))
    };
    let drm: ARef<AsahiDevice> = drm::device::Device::new(pdev.as_ref(), uninit)?;
    Ok(T8152Probe {
        soc,
        identity,
        stage,
        firmware_profile_admitted,
        psd,
        registers,
        observed,
        drm,
    })
}

/// G18 live-boot entry point: the staged bring-up in `g18_live_boot.rs`.
///
/// `probe.stage` is `g18_initdata_test`, already checked by
/// [`g18_live_boot::stage_is_known`]:
/// - 1: read-only probe (PSD, SGX ID, wrapper CPU state, post-office
///   status), then ENODEV;
/// - 2, 3, 4, 5, 6 (REQUIRE USER APPROVAL, plus the mailbox provider's own
///   `apple_mailbox.ascwrap_v8_cpu_control=1`): ASC CPU start and RTKit,
///   init-data ACKs, openings + idle wake + heartbeat. Stages 2 and 3 end in
///   ENODEV with every firmware owner retained; stages 4/5/6 return the owner.
///   Stage 5 registers one empty queue; stage 6 binds its scheduler. No GPU jobs;
/// - 90 (REQUIRES USER APPROVAL): PMGR2 PSD power-on only, then ENODEV.
///
/// On success the driver keeps the owner and calls
/// [`G18Runtime::shutdown`] on unbind.
fn g18_live_boot_start(
    pdev: &platform::Device<Core>,
    probe: T8152Probe,
) -> Result<Arc<dyn G18Runtime>> {
    g18_live_boot::start(pdev, probe)
}

fn probe_t8142_initdata(pdev: &platform::Device<Core>) -> Result<g17_live_boot::m5::M5Runtime> {
    let (drm, registers, _) = prepare_t8142_device(pdev)?;
    if *crate::module_parameters::m5_initdata_test.value() == 13 {
        g17_live_boot::m5::M5Runtime::start_application(pdev, drm, registers)
    } else {
        g17_live_boot::m5::M5Runtime::start(pdev, drm, registers)
    }
}

fn probe_t8142_runtime(pdev: &platform::Device<Core>) -> Result<crate::m5_drm::SharedM5Runtime> {
    let (drm, registers, observed) = prepare_t8142_device(pdev)?;
    let runtime = g17_live_boot::m5::M5Runtime::start_application(pdev, drm.clone(), registers)?;
    let runtime = Arc::pin_init(new_mutex!(runtime), GFP_KERNEL)?;
    let gpu: Arc<dyn drm_gpu::DrmGpu> = Arc::new(
        crate::m5_drm::M5DrmGpu::new(runtime.clone(), observed), GFP_KERNEL)?;
    let data = try_pin_init!(AsahiData {
        gpu, pdev: pdev.into(), resources: None, g17_runtime: None,
        g17_event_work <- new_work!("AsahiData::g17_event_work"),
        g17_recovery_work <- new_work!("AsahiData::g17_recovery_work"),
    });
    let ptr: *const AsahiData = &raw const **drm;
    // SAFETY: prepare_t8142_device leaves this data slot uninitialized. This
    // is its sole initialization, before registration exposes file operations.
    unsafe { data.__pinned_init(ptr as *mut AsahiData)?; }
    (*drm).gpu.init()?;
    drm::driver::Registration::new_foreign_owned(&drm, pdev.as_ref(), 0)?;
    dev_info!(pdev.as_ref(),
        "M5_DRM registered experimental first-job render adapter; general Mesa rendering unqualified\n");
    Ok(runtime)
}

/// Identify an AGX3 target as far as read-only evidence allows, then refuse.
///
/// This logs the statically decoded identity, performs the read-only SGX
/// identification (ID block + core-enable masks; no DMA mask, no ASC start,
/// no MMIO write), cross-checks the two, and then fails closed at the
/// generation-specific firmware boundary.
fn refuse_agx3_probe(pdev: &platform::Device<Core>, soc: &'static hw::agx3::SocConfig) -> Error {
    let Some(expected) =
        identity::decode_gpu_identity(soc.hw_family, soc.hw_variant, soc.num_dies as u8)
    else {
        dev_err!(
            pdev.as_ref(),
            "AGX3: static identity for chip {:#x} does not decode\n",
            soc.chip_id
        );
        return EINVAL;
    };

    if expected.uat_input_address_bits != soc.uat_ias {
        dev_err!(
            pdev.as_ref(),
            "AGX3: static UAT width for chip {:#x} disagrees with the identity decode\n",
            soc.chip_id
        );
        return EINVAL;
    }

    dev_info!(
        pdev.as_ref(),
        "AGX3: matched {:?}{:?} (chip {:#x}, USC gen {}, HAL {:?}, {:?} firmware roles, {:?} submission, {}-bit UAT roots, {}-bit OAS)\n",
        expected.gpu_gen,
        expected.gpu_variant,
        soc.chip_id,
        expected.usc_generation,
        expected.gpu_hal_generation,
        expected.firmware_roles,
        expected.submission_transport,
        soc.uat_ias,
        soc.uat_oas
    );

    // The G17S loaded-firmware handoff is reserved memory, not GPU MMIO. It
    // must be mapped and hashed before touching the SGX register aperture so
    // firmware identity can never be inferred from a powered or partially
    // initialized GPU. Other G17 variants never consume these G17S carveouts.
    // This token grants identity only.
    let loaded_g17_firmware = if expected.gpu_gen == hw::GpuGen::G17
        && expected.gpu_variant == identity::GpuVariant::S
    {
        match g17_firmware::admit_g17_loaded_pair_from_handoff(expected.gpu_variant as u32 as u8) {
            Ok(admission) => {
                dev_info!(
                    pdev.as_ref(),
                    "G17: exact versioned GFX/GFX1 loaded-segment identity admitted before SGX MMIO\n"
                );
                Some(admission)
            }
            Err(e) => {
                dev_err!(
                    pdev.as_ref(),
                    "G17S: loaded-segment firmware identity rejected; refusing before SGX MMIO: {:?}\n",
                    e
                );
                return ENODEV;
            }
        }
    } else {
        None
    };

    match regs::Resources::new(pdev) {
        Ok(res) => match res.get_gpu_id() {
            Ok(id) => {
                dev_info!(
                    pdev.as_ref(),
                    "AGX3: hardware reports {:?}{:?} rev {:?}, {} dies, {} clusters, {} cores/cluster, {} active cores\n",
                    id.gpu_gen,
                    id.gpu_variant,
                    id.gpu_rev,
                    id.num_dies,
                    id.num_clusters,
                    id.num_cores,
                    id.total_active_cores
                );
                if id.gpu_gen != soc.gpu_gen || id.gpu_variant != soc.gpu_variant {
                    dev_warn!(
                        pdev.as_ref(),
                        "AGX3: hardware identity {:?}{:?} does not match the matched compatible ({:?}{:?})\n",
                        id.gpu_gen,
                        id.gpu_variant,
                        soc.gpu_gen,
                        soc.gpu_variant
                    );
                }
            }
            Err(e) => dev_warn!(pdev.as_ref(), "AGX3: GPU ID decode failed: {:?}\n", e),
        },
        Err(e) => dev_warn!(
            pdev.as_ref(),
            "AGX3: SGX MMIO region unavailable: {:?}\n",
            e
        ),
    }

    match expected.gpu_gen {
        hw::GpuGen::G15 => refuse_g15_probe(pdev),
        hw::GpuGen::G17 => refuse_g17_probe(pdev, &expected, loaded_g17_firmware.as_ref()),
        gen => {
            dev_err!(
                pdev.as_ref(),
                "AGX3: no firmware handoff for {:?}: the HwConfig firmware-tuning blocks, init-data layout, and Handoff ABI are not grounded for this generation; refusing before DMA setup or ASC start\n",
                gen
            );
            ENODEV
        }
    }
}

/// Report the grounded-vs-UNKNOWN state of the single-role G15 (M3) boot, then
/// fail closed. G15 is the easiest AGX3 target — one `GFX` role, classic
/// device-control-ring submission — but the firmware image is not on disk and
/// the init-data/RTKit graph is not boot-complete offline.
fn refuse_g15_probe(pdev: &platform::Device<Core>) -> Error {
    let Err(missing) = g15_boot::pre_handoff_gate(hw::GpuGen::G15 as u32) else {
        dev_err!(
            pdev.as_ref(),
            "G15 diagnostic target remained selected after its preflight opened\n"
        );
        return EINVAL;
    };

    dev_info!(
        pdev.as_ref(),
        "G15 identity decoded (single-role GFX, classic submission); grounded init-data root and shared RTKit codec are diagnostic-only\n"
    );
    dev_err!(
        pdev.as_ref(),
        "G15 preflight missing evidence {:#x}; refusing before DMA setup, MMIO mapping, UAT ownership, or GFX ASC start\n",
        missing.bits()
    );
    if missing.contains(g15_boot::missing::FIRMWARE_IMAGE) {
        dev_err!(
            pdev.as_ref(),
            "G15 preflight: the G15 GFX RTKit firmware image and its nested identity are not available offline\n"
        );
    }
    if missing.contains(g15_boot::missing::UAT_HANDOFF) {
        dev_err!(
            pdev.as_ref(),
            "G15 preflight: the dual-TTBR 42-bit UAT/Handoff firmware-shared state is not validated\n"
        );
    }
    if missing.contains(g15_boot::missing::INITDATA_GRAPH) {
        dev_err!(
            pdev.as_ref(),
            "G15 preflight: init-data root and runtime-pointer graph are grounded, but hw_globals and struct sizes are not boot-complete\n"
        );
    }
    if missing.contains(g15_boot::missing::RTKIT_TRANSPORT) {
        dev_err!(
            pdev.as_ref(),
            "G15 preflight: the GPU RTKit codec is grounded but HELLO/EPMAP transmit is unproved\n"
        );
    }
    if missing.contains(g15_boot::missing::INTERFACE_VERSION) {
        dev_err!(
            pdev.as_ref(),
            "G15 preflight: the EP0 RTKit interface version for this firmware/OS build is unknown\n"
        );
    }
    if missing.contains(g15_boot::missing::SUBMISSION_TRANSPORT) {
        dev_err!(
            pdev.as_ref(),
            "G15 preflight: classic register-list submission is not implemented\n"
        );
    }

    ENODEV
}

fn refuse_g17_probe(
    pdev: &platform::Device<Core>,
    identity: &identity::GpuIdentity,
    firmware: Option<&g17_firmware::G17FirmwareIdentityAdmission>,
) -> Error {
    // G17P remains fail-closed until the Linux platform decoder can supply an
    // exact observed D93AP/T8140 topology. Never substitute the expected
    // constant here: that would turn topology admission back into dead data.
    let power_preflight = match g17_power_preflight::G17PowerPreflight::new(identity, None) {
        Ok(preflight) => preflight,
        Err(e) => {
            dev_err!(
                pdev.as_ref(),
                "G17: no exact target-bound cold-boot/power preflight: {:?}; refusing\n",
                e
            );
            return ENODEV;
        }
    };
    let Err(power_missing) = power_preflight.execution_gate() else {
        dev_err!(
            pdev.as_ref(),
            "G17 diagnostic target remained selected after its power execution gate opened\n"
        );
        return EINVAL;
    };
    dev_info!(
        pdev.as_ref(),
        "G17 {:?} cold-boot/power model selected; live execution evidence still missing {:#x}\n",
        power_preflight.target(),
        power_missing.bits()
    );

    let gpu_variant = identity.gpu_variant;
    let gate = if gpu_variant == identity::GpuVariant::S {
        g17_boot::pre_handoff_gate_with_identity(
            hw::GpuGen::G17 as u32,
            g17_boot::G17FirmwareVariant::G17S,
            firmware,
        )
    } else {
        g17_boot::pre_handoff_gate(hw::GpuGen::G17 as u32)
    };
    let Err(missing) = gate else {
        dev_err!(
            pdev.as_ref(),
            "G17 diagnostic target remained selected after its preflight opened\n"
        );
        return EINVAL;
    };

    dev_info!(
        pdev.as_ref(),
        "G17 identity decoded; grounded init-data and RTKit codecs are diagnostic-only\n"
    );
    if firmware.is_some() {
        let Err(default_missing) = g17_boot::pre_handoff_gate(hw::GpuGen::G17 as u32) else {
            dev_err!(
                pdev.as_ref(),
                "G17: default pre-handoff unexpectedly opened\n"
            );
            return EINVAL;
        };
        if missing.bits() != default_missing.bits() & !g17_boot::missing::FIRMWARE_IDENTITY {
            dev_err!(
                pdev.as_ref(),
                "G17: loaded-firmware admission changed a non-identity gate; refusing\n"
            );
            return EINVAL;
        }
        dev_info!(
            pdev.as_ref(),
            "G17: loaded GFX/GFX1 evidence cleared only the firmware-identity bit; all execution gates remain closed\n"
        );
    }
    dev_err!(
        pdev.as_ref(),
        "G17 preflight missing evidence {:#x}; refusing before DMA setup, any MMIO write, UAT ownership, or GFX/GFX1 ASC start\n",
        missing.bits()
    );
    if missing.contains(g17_boot::missing::FIRMWARE_IDENTITY) {
        dev_err!(
            pdev.as_ref(),
            "G17 preflight: no exact versioned m1n1 loaded-segment GFX/GFX1 identity was admitted\n"
        );
    }
    if missing.contains(g17_boot::missing::ROLE_RESOURCES) {
        dev_err!(
            pdev.as_ref(),
            "G17 preflight: distinct GFX/GFX1 resource plumbing is not wired\n"
        );
    }
    if missing.contains(g17_boot::missing::TOPOLOGY_REGISTERS) {
        // Cannot fire while regs.rs wires the proven G14X-family bank for
        // AGX3; kept for the case where that evidence is ever withdrawn.
        dev_err!(
            pdev.as_ref(),
            "G17 preflight: G17 core-mask register bank is not proven\n"
        );
    }
    if missing.contains(g17_boot::missing::UAT_HANDOFF) {
        dev_err!(
            pdev.as_ref(),
            "G17 preflight: SPTM/UAT ownership handoff is not implemented\n"
        );
    }
    if missing.contains(g17_boot::missing::INITDATA_GRAPH) {
        dev_err!(
            pdev.as_ref(),
            "G17 preflight: init-data graph has grounded fields but is not boot-complete\n"
        );
    }
    if missing.contains(g17_boot::missing::RTKIT_TRANSPORT) {
        dev_err!(
            pdev.as_ref(),
            "G17 preflight: RTKit codec is grounded but HELLO/EPMAP transmit is unproved\n"
        );
    }
    if missing.contains(g17_boot::missing::GFX1_POWER_SEQUENCE) {
        dev_err!(
            pdev.as_ref(),
            "{}{}",
            "G17 preflight: G17S PIO type identities, transition order, intervening state/sync order, and one 250us-reported poll are grounded, ",
            "but type 0/0xb physical bases, opaque helper ownership, and G17G/G17P firmware parity are incomplete\n"
        );
    }
    if missing.contains(g17_boot::missing::RECOVERY_HANDSHAKE) {
        dev_err!(
            pdev.as_ref(),
            "{}{}",
            "G17 preflight: recovery word states 0..3, host writes 1->2/3->0, and firmware writes 1 at begin/0 at completion are grounded, ",
            "but host-visible DRAM causality and the cross-role 0x22/0x23 handshake are incomplete\n"
        );
    }
    if missing.contains(g17_boot::missing::SUBMISSION_TRANSPORT) {
        dev_err!(
            pdev.as_ref(),
            "{}{}",
            "G17 preflight: submission producer subset is grounded (EP 0x21 doorbell byte-identical to AGX2, SKSM kick word and 0x20-strided kick-record table forked and cited), ",
            "but the SKSM hardware kick-FIFO MMIO scratch, the doorbell round-trip, and the completion IRQ can only be confirmed on a live M5\n"
        );
    }

    ENODEV
}

/// Platform Driver implementation for `AsahiDriver`.
impl platform::Driver for AsahiDriver {
    type IdInfo = ProbeConfig;
    const OF_ID_TABLE: Option<of::IdTable<Self::IdInfo>> = Some(&OF_TABLE);

    /// Device probe function.
    fn probe(
        pdev: &platform::Device<Core>,
        info: Option<&Self::IdInfo>,
    ) -> impl PinInit<Self, Error> {
        debug::update_debug_flags();

        dev_info!(pdev.as_ref(), "Probing...\n");

        let cfg = match info.ok_or(ENODEV)? {
            ProbeConfig::Supported(cfg) => *cfg,
            ProbeConfig::Agx3Diagnostic(soc) if soc.chip_id == 0x8140 => {
                let runtime = probe_t8140_runtime(pdev, soc)?;
                dev_info!(
                    pdev.as_ref(),
                    "G17P: persistent dual-ASC runtime bound and DRM render node registered\n"
                );
                return Ok(Self {
                    runtime: AsahiRuntime::G17P(runtime),
                });
            }
            // T8152/G18G is routed before the generic AGX3 refusal: that
            // path reads SGX ID registers, which J873 must not do unless the
            // sgx PSD service proves the block powered.
            ProbeConfig::Agx3Diagnostic(soc) if soc.chip_id == 0x8152 => {
                return Ok(Self { runtime: AsahiRuntime::G18(probe_t8152(pdev, soc)?) });
            }
            ProbeConfig::Agx3Diagnostic(soc) if soc.chip_id == 0x8142
                && *crate::module_parameters::m5_initdata_test.value() == 14 => {
                return Ok(Self { runtime: AsahiRuntime::M5Drm(probe_t8142_runtime(pdev)?) });
            }
            ProbeConfig::Agx3Diagnostic(soc) if soc.chip_id == 0x8142
                && matches!(*crate::module_parameters::m5_initdata_test.value(), 1..=13) => {
                return Ok(Self { runtime: AsahiRuntime::M5(probe_t8142_initdata(pdev)?) });
            }
            ProbeConfig::Agx3Diagnostic(soc) => return Err(refuse_agx3_probe(pdev, soc)),
        };

        // G17 requires two physical ASC roles with distinct transports and
        // init-data roots, but one shared GPU/UAT owner. Refuse before any
        // MMIO setup or ASC start until the target-specific GFX1 resources,
        // power mapping, recovery wire protocol, and submission handoff are
        // all implemented from evidence.
        if let Err(missing) = g17_boot::pre_handoff_gate(cfg.gpu_gen as u32) {
            dev_err!(
                pdev.as_ref(),
                "G17 dual-role pre-handoff is incomplete (missing evidence mask {:#x}); refusing to start GFX/GFX1\n",
                missing.bits()
            );
            return Err(ENODEV);
        }

        unsafe { pdev.dma_set_mask_and_coherent(DmaMask::try_new(cfg.uat_oas)?)? };

        let res = regs::Resources::new(pdev)?;

        // Initialize misc MMIO
        res.init_mmio()?;

        // Start the coprocessor CPU, so UAT can initialize the handoff
        regs::Resources::start_cpu(pdev)?;

        let fwnode = pdev.as_ref().fwnode().ok_or(EIO)?;
        let compat: KVec<u32> = fwnode
            .property_read_array_vec(c_str!("apple,firmware-compat"), 3)?
            .required_by(pdev.as_ref())?;

        // TODO: This is very temporary
        // SAFETY: This should be safe as data is not touched by the driver
        // untill it gets fully initialised.
        // Additionally drm::device::Device::release() will not drop data and
        // leaks instead.
        let uninit = unsafe {
            pin_init::pin_init_from_closure::<AsahiData, kernel::error::Error>(|_slot| Ok(()))
        };
        let drm: ARef<AsahiDevice> = drm::device::Device::new(pdev.as_ref(), uninit)?;

        let legacy_gpu = match (cfg.gpu_gen, cfg.gpu_variant, compat.as_slice()) {
            (hw::GpuGen::G13, _, &[12, 3, 0]) => {
                gpu::GpuManagerG13V12_3::new(&drm.clone(), &res, cfg)? as Arc<dyn gpu::GpuManager>
            }
            (hw::GpuGen::G14, hw::GpuVariant::G, &[12, 4, 0]) => {
                gpu::GpuManagerG14V12_4::new(&drm.clone(), &res, cfg)? as Arc<dyn gpu::GpuManager>
            }
            (hw::GpuGen::G13, _, &[13, 5, 0]) => {
                gpu::GpuManagerG13V13_5::new(&drm.clone(), &res, cfg)? as Arc<dyn gpu::GpuManager>
            }
            (hw::GpuGen::G14, hw::GpuVariant::G, &[13, 5, 0]) => {
                gpu::GpuManagerG14V13_5::new(&drm.clone(), &res, cfg)? as Arc<dyn gpu::GpuManager>
            }
            (hw::GpuGen::G14, _, &[13, 5, 0]) => {
                gpu::GpuManagerG14XV13_5::new(&drm.clone(), &res, cfg)? as Arc<dyn gpu::GpuManager>
            }
            _ => {
                dev_info!(
                    pdev.as_ref(),
                    "Unsupported GPU/firmware combination ({:?}, {:?}, {:?})\n",
                    cfg.gpu_gen,
                    cfg.gpu_variant,
                    compat
                );
                return Err(ENODEV);
            }
        };
        let gpu: Arc<dyn drm_gpu::DrmGpu> =
            Arc::new(drm_gpu::LegacyDrmGpu::new(legacy_gpu), GFP_KERNEL)?;

        let data = try_pin_init!(AsahiData {
            gpu,
            pdev: pdev.into(),
            resources: Some(res),
        });

        let ptr: *const AsahiData = &raw const **drm;
        unsafe {
            data.__pinned_init(ptr as *mut AsahiData)?;
        }

        (*drm).gpu.init()?;

        drm::driver::Registration::new_foreign_owned(&drm, pdev.as_ref(), 0)?;

        Ok(Self {
            runtime: AsahiRuntime::Legacy(drm),
        })
    }

    fn unbind(_pdev: &platform::Device<Core>, this: Pin<&Self>) {
        if let AsahiRuntime::G18(runtime) = &this.runtime {
            runtime.shutdown();
            return;
        }
        if let AsahiRuntime::M5Drm(runtime) = &this.runtime {
            let _ = runtime.lock().shutdown();
            return;
        }
        let AsahiRuntime::G17P(runtime) = &this.runtime else {
            return;
        };
        let drm = match runtime.lock().drm_ref() {
            Ok(drm) => drm,
            Err(_) => return,
        };
        g17_lifecycle::quiesce_driver_unbind(&G17PDriverUnbind { runtime, drm });
    }
}
