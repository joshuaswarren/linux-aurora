// SPDX-License-Identifier: GPL-2.0-only OR MIT

//! Top-level GPU driver implementation.

use kernel::{
    c_str,
    device::Core,
    dma::{
        Device,
        DmaMask, //
    },
    drm,
    drm::ioctl,
    of,
    platform,
    prelude::*,
    sync::{
        aref::ARef,
        Arc, //
    }, //
};

use crate::{
    debug,
    file,
    g15_boot,
    g17_boot,
    g17_firmware,
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
    pub(crate) gpu: Arc<dyn gpu::GpuManager>,
    pub(crate) pdev: ARef<platform::Device>,
    pub(crate) resources: regs::Resources,
}

unsafe impl Send for AsahiData {}
unsafe impl Sync for AsahiData {}

pub(crate) struct AsahiDriver {
    #[expect(unused)]
    drm: ARef<drm::Device<Self>>,
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
        (
            of::DeviceId::new(c_str!("apple,agx-t8142")),
            ProbeConfig::Agx3Diagnostic(&hw::agx3::T8142)
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
        hw::GpuGen::G17 => {
            refuse_g17_probe(pdev, expected.gpu_variant, loaded_g17_firmware.as_ref())
        }
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
    gpu_variant: identity::GpuVariant,
    firmware: Option<&g17_firmware::G17FirmwareIdentityAdmission>,
) -> Error {
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

        let gpu = match (cfg.gpu_gen, cfg.gpu_variant, compat.as_slice()) {
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

        let data = try_pin_init!(AsahiData {
            gpu,
            pdev: pdev.into(),
            resources: res,
        });

        let ptr: *const AsahiData = &raw const **drm;
        unsafe {
            data.__pinned_init(ptr as *mut AsahiData)?;
        }

        (*drm).gpu.init()?;

        drm::driver::Registration::new_foreign_owned(&drm, pdev.as_ref(), 0)?;

        Ok(Self { drm })
    }
}
