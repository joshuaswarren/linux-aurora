// SPDX-License-Identifier: GPL-2.0-only OR MIT

//! GPU MMIO register abstraction
//!
//! Since the vast majority of the interactions with the GPU are brokered through the firmware,
//! there is very little need to interact directly with GPU MMIO register. This module abstracts
//! the few operations that require that, mainly reading the MMU fault status, reading GPU ID
//! information, and starting the GPU firmware coprocessor.

use crate::{g17_submission, hw, identity};
use kernel::{
    c_str,
    device::Core,
    devres::Devres,
    io::{
        mem::IoMem, //
        Io,
    },
    new_mutex,
    platform,
    prelude::*,
    sync::{aref::ARef, Mutex}, //
};

/// Size of the ASC control MMIO region.
pub(crate) const ASC_CTL_SIZE: usize = 0x4000;

/// Size of the SGX MMIO region.
pub(crate) const SGX_SIZE: usize = 0x1000000;

const CPU_CONTROL: usize = 0x44;
const CPU_RUN: u32 = 0x1 << 4; // BIT(4)

// T8140 (A18 Pro / G17P) SGX registers, hardware-verified on the live J700
// bring-up. These offsets exceed the AGX2 `SGX_SIZE` window (the T8140 SGX
// block is 0x4000000), so their accessors map the full "sgx" resource
// unsized and use runtime-checked accesses.
//
// AXI transition workaround: after powering gfx-asc, gfx1-asc, and sgx, set
// bit 0 in both words. The observed post-write values on J700 are 0x00010001
// and 0x000413b1 respectively; the upper bits are hardware status and are
// only logged, not enforced.
const T8140_AXI_TRANSITION_0: usize = 0x1000104;
const T8140_AXI_TRANSITION_1: usize = 0x1000108;
/// Must be written 0 before ANY initdata is sent to either firmware
/// instance (T8140 startup sequence step "write zero to SGX+0xd06030").
const T8140_PRE_INITDATA_CLEAR: usize = 0xd06030;

const FAULT_INFO: usize = 0x17030;

const ID_VERSION: usize = 0xd04000;
const ID_UNK08: usize = 0xd04008;
const ID_COUNTS_1: usize = 0xd04010;
const ID_COUNTS_2: usize = 0xd04014;
const ID_UNK18: usize = 0xd04018;
const ID_CLUSTERS: usize = 0xd0401c;

const CORE_MASK_0: usize = 0xd01500;
const CORE_MASK_1: usize = 0xd01514;

// The "G14X" register family is shared by G14X (t602x) and every AGX3 part
// (G15/G16/G17). The ID block at 0xd04000 is unchanged, and the per-cluster
// core-enable masks are three u32 words at 0xe01500 (stride 4): AGXG17X
// `readChipInfo` builds the offset (`mov w23,#0x1500; movk w23,#0xe0,lsl#16`
// @ 0xfffffe0008b4b220) and reads [base], [base+4], [base+8]; the identical
// pattern appears in all eight AGX3 kexts of the pinned 26A5378n corpus
// (e.g. AGXG15G @ 0xfffffe0008af22f0).
const CORE_MASKS_G14X: usize = 0xe01500;
// MMU fault registers for the same family: AGXG17X fault-read helper
// @ 0xfffffe0008b58c04 reads FAULT_INFO=0xd8c0 and FAULT_ADDR=0xd8c8 and
// writes the acknowledge word to 0xd800 (AGXG15G equivalent
// @ 0xfffffe0008af1f64). The fault address register holds VA >> 6.
const FAULT_INFO_G14X: usize = 0xd8c0;
const FAULT_ADDR_G14X: usize = 0xd8c8;
/// Fault acknowledge/clear register (write-only). Recovered from the same
/// helpers as the fault registers above; the ack value encoding is not yet
/// modelled, so the driver does not write it.
#[allow(dead_code)]
const FAULT_ACK_G14X: usize = 0xd800;

/// Enum representing the unit that caused an MMU fault.
#[allow(non_camel_case_types)]
#[allow(clippy::upper_case_acronyms)]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum FaultUnit {
    /// Decompress / pixel fetch
    DCMP(u8),
    /// USC L1 Cache (device loads/stores)
    UL1C(u8),
    /// Compress / pixel store
    CMP(u8),
    GSL1(u8),
    IAP(u8),
    VCE(u8),
    /// Tiling Engine
    TE(u8),
    RAS(u8),
    /// Vertex Data Master
    VDM(u8),
    PPP(u8),
    /// ISP Parameter Fetch
    IPF(u8),
    IPF_CPF(u8),
    VF(u8),
    VF_CPF(u8),
    /// Depth/Stencil load/store
    ZLS(u8),

    /// Parameter Management
    dPM,
    /// Compute Data Master
    dCDM_KS(u8),
    dIPP,
    dIPP_CS,
    // Vertex Data Master
    dVDM_CSD,
    dVDM_SSD,
    dVDM_ILF,
    dVDM_ILD,
    dRDE(u8),
    FC,
    GSL2,

    /// Graphics L2 Cache Control?
    GL2CC_META(u8),
    GL2CC_MB,

    /// Parameter Management
    gPM_SP(u8),
    /// Vertex Data Master - CSD
    gVDM_CSD_SP(u8),
    gVDM_SSD_SP(u8),
    gVDM_ILF_SP(u8),
    gVDM_TFP_SP(u8),
    gVDM_MMB_SP(u8),
    /// Compute Data Master
    gCDM_CS_KS0_SP(u8),
    gCDM_CS_KS1_SP(u8),
    gCDM_CS_KS2_SP(u8),
    gCDM_KS0_SP(u8),
    gCDM_KS1_SP(u8),
    gCDM_KS2_SP(u8),
    gIPP_SP(u8),
    gIPP_CS_SP(u8),
    gRDE0_SP(u8),
    gRDE1_SP(u8),

    gCDM_CS,
    gCDM_ID,
    gCDM_CSR,
    gCDM_CSW,
    gCDM_CTXR,
    gCDM_CTXW,
    gIPP,
    gIPP_CS,
    gKSM_RCE,

    Unknown(u8),
}

/// Reason for an MMU fault.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum FaultReason {
    Unmapped,
    AfFault,
    WriteOnly,
    ReadOnly,
    NoAccess,
    Unknown(u8),
}

/// Collection of information about an MMU fault.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) struct FaultInfo {
    pub(crate) address: u64,
    pub(crate) sideband: u8,
    pub(crate) vm_slot: u32,
    pub(crate) unit_code: u8,
    pub(crate) unit: FaultUnit,
    pub(crate) level: u8,
    pub(crate) unk_5: u8,
    pub(crate) read: bool,
    pub(crate) reason: FaultReason,
}

/// Device resources for this GPU instance.
pub(crate) struct Resources {
    dev: ARef<platform::Device>,
    sgx: Pin<KBox<Devres<IoMem<0>>>>,
    /// Serializes the shared fixed-slot SKSM FIFO across every queue.
    sksm_fifo: Pin<KBox<Mutex<()>>>,
}

/// Write-only publication view of resource 0 for the SKSM FIFO pair.
///
/// In particular, the word-0 port at `sgx+0x4090` must never be loaded. The
/// host has a separate diagnostic load from word 1 which is not exposed here.
struct G17SksmSgxPairWriter<'a>(&'a IoMem<0>);

impl g17_submission::G17SksmPairWriter for G17SksmSgxPairWriter<'_> {
    type Error = Error;

    fn write64(&self, offset: u32, value: u64) -> Result {
        // Regular writeq ordering is required: word 0 must reach the device
        // before word 1. Publication never reads either scratch location.
        self.0.try_write64(value, offset as usize)
    }
}

impl Resources {
    /// Map the required resources given our platform device.
    pub(crate) fn new(pdev: &platform::Device<Core>) -> Result<Resources> {
        // The pinned host maps accelerator device-memory index 0 and stores
        // it at accelerator+0x348 before every SKSM scratch access. That range
        // is the J700 GPU block exposed to Linux by the named `sgx` resource
        // (the second platform `reg` tuple, after `asc`).
        let sgx_req = pdev.io_request_by_name(c_str!("sgx")).ok_or(EINVAL)?;
        let sgx_iomem = KBox::pin_init(sgx_req.iomap(), GFP_KERNEL)?;
        let sksm_fifo = KBox::pin_init(new_mutex!((), "g17_sksm_fifo"), GFP_KERNEL)?;

        Ok(Resources {
            // SAFETY: This device does DMA via the UAT IOMMU.
            dev: pdev.into(),
            sgx: sgx_iomem,
            sksm_fifo,
        })
    }

    fn g17p_sksm_pair_fits(pair: g17_submission::G17SksmOrderedWritePair) -> bool {
        let fits_u64 = |offset: u32| {
            let offset = offset as usize;
            offset % core::mem::size_of::<u64>() == 0
                && offset
                    .checked_add(core::mem::size_of::<u64>())
                    .is_some_and(|end| end <= SGX_SIZE)
        };

        fits_u64(pair.word0_offset) && fits_u64(pair.word1_offset)
    }

    fn g17p_sksm_publish_locked(
        sgx: &IoMem<0>,
        pair: g17_submission::G17SksmOrderedWritePair,
    ) -> Result {
        g17_submission::publish_g17p_sksm_write_pair(&G17SksmSgxPairWriter(sgx), pair)
    }

    /// Publish word 0 then word 1 through J700 resource 0 (`sgx`).
    ///
    /// The pair is checked completely before either write. The fixed-slot
    /// lock prevents publications from two queues interleaving.
    pub(crate) fn g17p_sksm_publish_write_pair(
        &self,
        pair: g17_submission::G17SksmOrderedWritePair,
    ) -> Result {
        if !Self::g17p_sksm_pair_fits(pair) {
            return Err(EINVAL);
        }

        // Host writes to the SKSM tag/data port take an asynchronous SError on
        // J700: bisection showed that suppressing every one of them (the QID
        // configure pair AND addKicks, which uses the same port) removes the
        // fault entirely, while suppressing only one still panics. The whole
        // sgx 0x0..0x2ffff range is unreachable from the AP even though the
        // firmware reaches the same registers through its own mapping of
        // PA 0x480000000. Gated off by default until the correct host aperture
        // is proven or the KSM programming moves to firmware channel commands.
        if crate::g17_config::SKSM_MMIO == 0 || !self.g17p_sksm_port_powered() {
            dev_info!(
                self.dev.as_ref(),
                "G17P SKSM port MMIO suppressed (enabled={}, powered={}): {:#x}@{:#x} / {:#x}@{:#x}\n",
                crate::g17_config::SKSM_MMIO,
                self.g17p_sksm_port_powered(),
                pair.word0, pair.word0_offset, pair.word1, pair.word1_offset
            );
            self.stash_deferred_sksm_pair(pair);
            return Ok(());
        }
        let _guard = self.sksm_fifo.lock();
        let sgx = self.sgx.try_access().ok_or(ENODEV)?;
        Self::g17p_sksm_publish_locked(&sgx, pair)
    }

    /// Configure one G17P SKSM queue and then mark its QID valid.
    ///
    /// The host publishes both 64-bit mask words without reading either one:
    /// high QIDs first, low QIDs second.
    pub(crate) fn g17p_sksm_configure_queue(
        &self,
        pair: g17_submission::G17SksmOrderedWritePair,
        enable_pair: g17_submission::G17SksmOrderedWritePair,
    ) -> Result {
        if !Self::g17p_sksm_pair_fits(pair)
            || !Self::g17p_sksm_pair_fits(enable_pair)
            || enable_pair.word0 | enable_pair.word1 == 0
        {
            return Err(EINVAL);
        }

        if crate::g17_config::SKSM_MMIO == 0 || !self.g17p_sksm_port_powered() {
            dev_info!(
                self.dev.as_ref(),
                "G17P QID configure suppressed (enabled={}, powered={}, tag={:#x}@{:#x} payload={:#x}@{:#x} maskHi={:#x} maskLo={:#x})\n",
                crate::g17_config::SKSM_MMIO,
                self.g17p_sksm_port_powered(),
                pair.word0, pair.word0_offset, pair.word1, pair.word1_offset,
                enable_pair.word0, enable_pair.word1
            );
            self.stash_deferred_sksm_registration(pair, enable_pair)?;
            return Ok(false);
        }
        let _guard = self.sksm_fifo.lock();
        let sgx = self.sgx.try_access().ok_or(ENODEV)?;
        Self::g17p_sksm_publish_locked(&sgx, pair)?;
        Self::g17p_sksm_publish_locked(&sgx, enable_pair)
    }

    fn sgx_read32<const OFF: usize>(&self) -> u32 {
        if let Some(sgx) = self.sgx.try_access() {
            sgx.relaxed().try_read32(OFF).unwrap_or(0)
        } else {
            0
        }
    }

    /* Not yet used
    fn sgx_write32<OFF: usize>(&self, val: u32) {
        if let Some(sgx) = self.sgx.try_access() {
            sgx.relaxed().write32(val, OFF)
        }
    }
    */

    fn sgx_read64<const OFF: usize>(&self) -> u64 {
        if let Some(sgx) = self.sgx.try_access() {
            sgx.relaxed().try_read64(OFF).unwrap_or(0)
        } else {
            0
        }
    }

    /* Not yet used
    fn sgx_write64<OFF: usize>(&self, val: u64) {
        if let Some(sgx) = self.sgx.try_access() {
            sgx.relaxed().write64(val, OFF)
        }
    }
    */

    /// Initialize the MMIO registers for the GPU.
    pub(crate) fn init_mmio(&self) -> Result {
        // Nothing to do for now...

        Ok(())
    }

    /// Start one ASC coprocessor CPU by its `reg-names` entry.
    fn start_cpu_by_name(pdev: &platform::Device<Core>, name: &CStr) -> Result {
        let asc_req = pdev.io_request_by_name(name).ok_or_else(|| {
            dev_err!(
                pdev.as_ref(),
                "ASC control region \"{}\" is not declared by this device\n",
                name
            );
            EINVAL
        })?;
        let asc_iomem = KBox::pin_init(asc_req.iomap_sized::<ASC_CTL_SIZE>(), GFP_KERNEL)?;
        let res = asc_iomem.access(pdev.as_ref())?.relaxed();

        let val = res.read32(CPU_CONTROL);
        res.write32(val | CPU_RUN, CPU_CONTROL);
        Ok(())
    }

    /// Start the (primary) ASC coprocessor CPU.
    pub(crate) fn start_cpu(pdev: &platform::Device<Core>) -> Result {
        Self::start_cpu_by_name(pdev, c_str!("asc"))
    }

    /// Apply the T8140 AXI transition workaround.
    ///
    /// Hardware-verified order: power `gfx-asc`, `gfx1-asc`, and `sgx`
    /// first, then set bit 0 of both AXI transition words. Observed
    /// post-write values on J700 are `0x00010001`/`0x000413b1`; a different
    /// readback is logged but not fatal (the upper bits are status).
    pub(crate) fn t8140_axi_transition_workaround(&self) -> Result {
        let sgx = self.sgx.try_access().ok_or(ENODEV)?;
        let res = sgx.relaxed();

        for (offset, observed) in [
            (T8140_AXI_TRANSITION_0, 0x0001_0001u32),
            (T8140_AXI_TRANSITION_1, 0x0004_13b1u32),
        ] {
            let val = res.try_read32(offset)?;
            res.try_write32(val | 1, offset)?;
            let after = res.try_read32(offset)?;
            if after != observed {
                dev_warn!(
                    self.dev.as_ref(),
                    "T8140 AXI transition word {:#x} reads {:#x} after set (J700 observed {:#x})\n",
                    offset,
                    after,
                    observed
                );
            }
        }
        Ok(())
    }

    /// T8140: write zero to the pre-initdata gate register.
    ///
    /// The grounded startup sequence requires this write after the AXI
    /// transition workaround and before ANY initdata is sent to either
    /// firmware instance.
    pub(crate) fn t8140_pre_initdata_clear(&self) -> Result {
        let sgx = self.sgx.try_access().ok_or(ENODEV)?;
        let res = sgx.relaxed();
        res.try_write32(0, T8140_PRE_INITDATA_CLEAR)?;
        Ok(())
    }

    /// Get the GPU identification info from registers.
    ///
    /// See [`hw::GpuIdConfig`] for the result.
    pub(crate) fn get_gpu_id(&self) -> Result<hw::GpuIdConfig> {
        let id_version = self.sgx_read32::<ID_VERSION>();
        let id_unk08 = self.sgx_read32::<ID_UNK08>();
        let id_counts_1 = self.sgx_read32::<ID_COUNTS_1>();
        let id_counts_2 = self.sgx_read32::<ID_COUNTS_2>();
        let id_unk18 = self.sgx_read32::<ID_UNK18>();
        let id_clusters = self.sgx_read32::<ID_CLUSTERS>();

        dev_info!(
            self.dev.as_ref(),
            "GPU ID registers: {:#x} {:#x} {:#x} {:#x} {:#x} {:#x}\n",
            id_version,
            id_unk08,
            id_counts_1,
            id_counts_2,
            id_unk18,
            id_clusters
        );

        let family = ((id_version >> 24) & 0xff) as u8;
        let variant = ((id_version >> 16) & 0xff) as u8;
        let num_dies = (id_counts_1 >> 16) & 0xf;
        let identity =
            identity::decode_gpu_identity(family, variant, num_dies as u8).ok_or_else(|| {
                dev_err!(
                    self.dev.as_ref(),
                    "Unknown GPU identity (family {:#x}, variant {:#x}, dies {})\n",
                    family,
                    variant,
                    num_dies
                );
                ENODEV
            })?;

        let mut core_mask_regs = KVec::new();

        let num_clusters = match family {
            4 | 5 => {
                // G13 | G14G
                core_mask_regs.push(self.sgx_read32::<CORE_MASK_0>(), GFP_KERNEL)?;
                core_mask_regs.push(self.sgx_read32::<CORE_MASK_1>(), GFP_KERNEL)?;
                (id_clusters >> 12) & 0xff
            }
            6 | 0x7 | 0x8 | 0xa | 0xb => {
                // G14X and every AGX3 family share the G14X register bank
                // (see the CORE_MASKS_G14X provenance comment above): three
                // u32 core-enable mask words at 0xe01500, and the cluster
                // count is the clusters-per-die x dies product from the
                // unchanged ID block.
                core_mask_regs.push(self.sgx_read32::<CORE_MASKS_G14X>(), GFP_KERNEL)?;
                core_mask_regs.push(self.sgx_read32::<{ CORE_MASKS_G14X + 4 }>(), GFP_KERNEL)?;
                core_mask_regs.push(self.sgx_read32::<{ CORE_MASKS_G14X + 8 }>(), GFP_KERNEL)?;
                // Clusters per die * num dies
                ((id_counts_1 >> 8) & 0xff) * ((id_counts_1 >> 16) & 0xf)
            }
            0xc => {
                // G18 (PI_400) keeps the G14X ID block, but 26A428
                // `AGX·PI_400·G·A0·Accelerator::readChipInfo` reads a single
                // core-enable word at 0xe01500 (@ 0xfffffe0008c3d6b4..0x6c4).
                // T8152 itself never takes this path; see get_gpu_id_t8152.
                core_mask_regs.push(self.sgx_read32::<CORE_MASKS_G14X>(), GFP_KERNEL)?;
                ((id_counts_1 >> 8) & 0xff) * ((id_counts_1 >> 16) & 0xf)
            }
            a => {
                dev_err!(self.dev.as_ref(), "Unsupported GPU family {:#x}\n", a);
                return Err(ENODEV);
            }
        };

        let mut core_masks_packed = KVec::new();
        core_masks_packed.extend_from_slice(&core_mask_regs, GFP_KERNEL)?;

        dev_info!(self.dev.as_ref(), "Core masks: {:#x?}\n", core_masks_packed);

        let num_cores = id_counts_1 & 0xff;

        if num_cores > 32 {
            dev_err!(
                self.dev.as_ref(),
                "Too many cores per cluster ({} > 32)\n",
                num_cores
            );
            return Err(ENODEV);
        }

        if num_cores * num_clusters > (core_mask_regs.len() * 32) as u32 {
            dev_err!(
                self.dev.as_ref(),
                "Too many total cores ({} x {} > {})\n",
                num_clusters,
                num_cores,
                core_mask_regs.len() * 32
            );
            return Err(ENODEV);
        }

        let mut core_masks = KVec::new();
        let mut total_active_cores: u32 = 0;

        let max_core_mask = ((1u64 << num_cores) - 1) as u32;
        for _ in 0..num_clusters {
            let mask = core_mask_regs[0] & max_core_mask;
            core_masks.push(mask, GFP_KERNEL)?;
            for i in 0..core_mask_regs.len() {
                core_mask_regs[i] >>= num_cores;
                if i < (core_mask_regs.len() - 1) {
                    core_mask_regs[i] |= core_mask_regs[i + 1] << (32 - num_cores);
                }
            }
            total_active_cores += mask.count_ones();
        }

        if core_mask_regs.iter().any(|a| *a != 0) {
            dev_err!(
                self.dev.as_ref(),
                "Leftover core mask: {:#x?}\n",
                core_mask_regs
            );
            return Err(EIO);
        }

        let (gpu_rev, gpu_rev_id) = match (id_version >> 8) & 0xff {
            0x00 => (hw::GpuRevision::A0, hw::GpuRevisionID::A0),
            0x01 => (hw::GpuRevision::A1, hw::GpuRevisionID::A1),
            0x10 => (hw::GpuRevision::B0, hw::GpuRevisionID::B0),
            0x11 => (hw::GpuRevision::B1, hw::GpuRevisionID::B1),
            0x20 => (hw::GpuRevision::C0, hw::GpuRevisionID::C0),
            0x21 => (hw::GpuRevision::C1, hw::GpuRevisionID::C1),
            a => {
                dev_err!(self.dev.as_ref(), "Unknown GPU revision {}\n", a);
                return Err(ENODEV);
            }
        };

        Ok(hw::GpuIdConfig {
            gpu_gen: identity.gpu_gen,
            gpu_variant: identity.gpu_variant,
            usc_generation: identity.usc_generation,
            gpu_hal_generation: identity.gpu_hal_generation,
            gpu_rev,
            gpu_rev_id,
            num_dies,
            num_clusters,
            num_cores,
            num_frags: num_cores, // Used to be id_counts_1[15:8] but does not work for G14X
            num_gps: (id_counts_2 >> 16) & 0xff,
            total_active_cores,
            core_masks,
            core_masks_packed,
            gpc_perf_state_map,
            gpc_perf_state_map_low,
            gpc_perf_state_control,
        })
    }

    /// Read and decode the T8152 (G18G) GPU identity.
    ///
    /// Requires the [`T8152SgxPowered`] proof: the SGX PMGR2 PSD service was
    /// read and reported active immediately before, so these
    /// reads cannot hit a gated block. Only the registers 26A428
    /// `AGX·PI_400·G·A0·Accelerator::readChipInfo` reads are touched: the ID
    /// block at 0xd04000..0xd0401c and the single core-enable word at
    /// 0xe01500. No register is written.
    ///
    /// The raw words are always logged. The decode never rejects: an unknown
    /// or different family/variant byte is reported and the part is
    /// identified by its chip id and DT compatible (`soc`), and implausible
    /// counts fall back to the native registry topology (2 x 6 cores).
    pub(crate) fn get_gpu_id_t8152(
        &self,
        powered: &T8152SgxPowered,
        soc: &'static hw::agx3::SocConfig,
    ) -> Result<hw::GpuIdConfig> {
        let registry = hw::agx3::T8152_REGISTRY;
        let expected =
            identity::decode_gpu_identity(soc.hw_family, soc.hw_variant, soc.num_dies as u8)
                .ok_or(EINVAL)?;
        if soc.chip_id != 0x8152 || expected.gpu_gen != hw::GpuGen::G18 {
            return Err(EINVAL);
        }

        let (id_version, id_unk08, id_counts_1, id_counts_2, id_unk18, id_clusters, core_mask) = {
            let sgx = self.sgx.try_access().ok_or(ENODEV)?;
            // Ordered (non-relaxed) reads, after the ordered PSD read.
            (
                sgx.try_read32(ID_VERSION)?,
                sgx.try_read32(ID_UNK08)?,
                sgx.try_read32(ID_COUNTS_1)?,
                sgx.try_read32(ID_COUNTS_2)?,
                sgx.try_read32(ID_UNK18)?,
                sgx.try_read32(ID_CLUSTERS)?,
                sgx.try_read32(CORE_MASKS_G14X)?,
            )
        };

        dev_info!(
            self.dev.as_ref(),
            "T8152: PSD sgx state={:#x} ack={:#x}; raw GPU ID {:#x} {:#x} {:#x} {:#x} {:#x} {:#x}; core mask {:#x}\n",
            powered.sample().state,
            powered.sample().ack,
            id_version,
            id_unk08,
            id_counts_1,
            id_counts_2,
            id_unk18,
            id_clusters,
            core_mask
        );

        let family = ((id_version >> 24) & 0xff) as u8;
        let variant = ((id_version >> 16) & 0xff) as u8;
        let reg_dies = (id_counts_1 >> 16) & 0xf;
        match identity::decode_gpu_identity(family, variant, reg_dies as u8) {
            Some(decoded) if decoded == expected => dev_info!(
                self.dev.as_ref(),
                "T8152: ID family {:#x} variant {:#x} decodes to {:?}{:?} as expected\n",
                family,
                variant,
                decoded.gpu_gen,
                decoded.gpu_variant
            ),
            Some(decoded) => dev_warn!(
                self.dev.as_ref(),
                "T8152: ID family {:#x} variant {:#x} decodes to {:?}{:?}; using the chip-id/compatible identity {:?}{:?}\n",
                family,
                variant,
                decoded.gpu_gen,
                decoded.gpu_variant,
                expected.gpu_gen,
                expected.gpu_variant
            ),
            None => dev_warn!(
                self.dev.as_ref(),
                "T8152: unknown ID family {:#x} variant {:#x} dies {}; using the chip-id/compatible identity {:?}{:?}\n",
                family,
                variant,
                reg_dies,
                expected.gpu_gen,
                expected.gpu_variant
            ),
        }

        // Same count layout as the PI_400 decode (@ 0xfffffe0008c3d53c..0x554):
        // dies [19:16], clusters per die [15:8], cores per cluster [7:0].
        let reg_cores = id_counts_1 & 0xff;
        let reg_clusters = ((id_counts_1 >> 8) & 0xff) * reg_dies;
        let registry_cores = registry.num_cores / registry.num_mgpus;
        let (num_dies, num_clusters, num_cores) = if (1..=32).contains(&reg_cores)
            && reg_clusters != 0
            && reg_cores * reg_clusters <= 32
        {
            (reg_dies, reg_clusters, reg_cores)
        } else {
            dev_warn!(
                self.dev.as_ref(),
                "T8152: implausible ID counts ({} dies, {} clusters, {} cores); using registry topology {} x {}\n",
                reg_dies,
                reg_clusters,
                reg_cores,
                registry.num_mgpus,
                registry_cores
            );
            (soc.num_dies, registry.num_mgpus, registry_cores)
        };

        let mut core_masks = KVec::new();
        let mut remaining = core_mask as u64;
        let cluster_mask = (1u64 << num_cores) - 1;
        let mut total_active_cores = 0;
        for _ in 0..num_clusters {
            let mask = (remaining & cluster_mask) as u32;
            core_masks.push(mask, GFP_KERNEL)?;
            total_active_cores += mask.count_ones();
            remaining >>= num_cores;
        }
        if remaining != 0 {
            dev_warn!(
                self.dev.as_ref(),
                "T8152: core mask {:#x} has bits beyond {} x {} cores\n",
                core_mask,
                num_clusters,
                num_cores
            );
        }
        if total_active_cores != registry.num_cores
            || core_masks.as_slice() != registry.core_mask_list.as_slice()
        {
            dev_warn!(
                self.dev.as_ref(),
                "T8152: core masks {:#x?} ({} active) differ from the native registry {:#x?} ({})\n",
                core_masks,
                total_active_cores,
                registry.core_mask_list,
                registry.num_cores
            );
        }

        let mut core_masks_packed = KVec::new();
        core_masks_packed.push(core_mask, GFP_KERNEL)?;

        let (gpu_rev, gpu_rev_id) = match (id_version >> 8) & 0xff {
            0x00 => (hw::GpuRevision::A0, hw::GpuRevisionID::A0),
            0x01 => (hw::GpuRevision::A1, hw::GpuRevisionID::A1),
            0x10 => (hw::GpuRevision::B0, hw::GpuRevisionID::B0),
            0x11 => (hw::GpuRevision::B1, hw::GpuRevisionID::B1),
            0x20 => (hw::GpuRevision::C0, hw::GpuRevisionID::C0),
            0x21 => (hw::GpuRevision::C1, hw::GpuRevisionID::C1),
            a => {
                dev_warn!(
                    self.dev.as_ref(),
                    "T8152: unknown GPU revision byte {:#x}; recorded as an A0 placeholder\n",
                    a
                );
                (hw::GpuRevision::A0, hw::GpuRevisionID::A0)
            }
        };

        dev_info!(
            self.dev.as_ref(),
            "T8152: {:?}{:?} rev {:?}, {} dies, {} clusters x {} cores, {} active, masks {:#x?}\n",
            expected.gpu_gen,
            expected.gpu_variant,
            gpu_rev,
            num_dies,
            num_clusters,
            num_cores,
            total_active_cores,
            core_masks
        );

        Ok(hw::GpuIdConfig {
            gpu_gen: expected.gpu_gen,
            gpu_variant: expected.gpu_variant,
            usc_generation: expected.usc_generation,
            gpu_hal_generation: expected.gpu_hal_generation,
            gpu_rev,
            gpu_rev_id,
            num_dies,
            num_clusters,
            num_cores,
            num_frags: num_cores,
            num_gps: (id_counts_2 >> 16) & 0xff,
            total_active_cores,
            core_masks,
            core_masks_packed,
            gpc_perf_state_map: 0,
            gpc_perf_state_map_low: 0,
            gpc_perf_state_control: 0,
        })
    }

    /// Snapshot the latched G17P MMU fault words without acknowledging them.
    pub(crate) fn g17p_fault_registers(&self) -> (u64, u64) {
        (
            self.sgx_read64::<FAULT_INFO_G14X>(),
            self.sgx_read64::<FAULT_ADDR_G14X>(),
        )
    }

    /// Read the four side-effect-free SGX words that distinguish an armed
    /// Fender wake path from a disabled one at the first-CL boundary.
    pub(crate) fn g17p_scheduler_gate_registers(&self) -> Result<G17PSchedulerGateRegisters> {
        let sgx = self.sgx.try_access().ok_or(ENODEV)?;
        let registers = sgx.relaxed();
        Ok(G17PSchedulerGateRegisters {
            fender_dynamic_gating: registers.try_read32(T8140_FENDER_DYNAMIC_GATING)?,
            host_irq_summary: registers.try_read32(T8140_HOST_IRQ_SUMMARY)? & 0xf,
            host_irq_enable: registers.try_read32(T8140_HOST_IRQ_ENABLE)?,
            host_irq_status: registers.try_read32(T8140_HOST_IRQ_STATUS)?,
        })
    }

    /// Snapshot the latched G17P MMU fault words without acknowledging them.
    pub(crate) fn g17p_fault_registers(&self) -> (u64, u64) {
        (
            self.sgx_read64::<FAULT_INFO_G14X>(),
            self.sgx_read64::<FAULT_ADDR_G14X>(),
        )
    }

    /// Get the fault information from the MMU status register, if one occurred.
    pub(crate) fn get_fault_info(&self, cfg: &'static hw::HwConfig) -> Option<FaultInfo> {
        // The G14X fault-register bank and fault-word decode carry unchanged
        // to every AGX3 part (see the FAULT_INFO_G14X provenance comment).
        // `gpu_core` is a firmware-ABI value that only exists for the AGX2
        // targets, so AGX3 membership is keyed off the design generation.
        let g14x = cfg.gpu_core as u32 >= hw::GpuCore::G14S as u32
            || cfg.gpu_gen as u32 >= hw::GpuGen::G15 as u32;

        let fault_info = if g14x {
            self.sgx_read64::<FAULT_INFO_G14X>()
        } else {
            self.sgx_read64::<FAULT_INFO>()
        };

        if fault_info & 1 == 0 {
            return None;
        }

        let fault_addr = if g14x {
            self.sgx_read64::<FAULT_ADDR_G14X>()
        } else {
            fault_info >> 30
        };

        let unit_code = ((fault_info >> 9) & 0xff) as u8;
        let unit = match unit_code {
            0x00..=0x9f => match unit_code & 0xf {
                0x0 => FaultUnit::DCMP(unit_code >> 4),
                0x1 => FaultUnit::UL1C(unit_code >> 4),
                0x2 => FaultUnit::CMP(unit_code >> 4),
                0x3 => FaultUnit::GSL1(unit_code >> 4),
                0x4 => FaultUnit::IAP(unit_code >> 4),
                0x5 => FaultUnit::VCE(unit_code >> 4),
                0x6 => FaultUnit::TE(unit_code >> 4),
                0x7 => FaultUnit::RAS(unit_code >> 4),
                0x8 => FaultUnit::VDM(unit_code >> 4),
                0x9 => FaultUnit::PPP(unit_code >> 4),
                0xa => FaultUnit::IPF(unit_code >> 4),
                0xb => FaultUnit::IPF_CPF(unit_code >> 4),
                0xc => FaultUnit::VF(unit_code >> 4),
                0xd => FaultUnit::VF_CPF(unit_code >> 4),
                0xe => FaultUnit::ZLS(unit_code >> 4),
                _ => FaultUnit::Unknown(unit_code),
            },
            0xa1 => FaultUnit::dPM,
            0xa2 => FaultUnit::dCDM_KS(0),
            0xa3 => FaultUnit::dCDM_KS(1),
            0xa4 => FaultUnit::dCDM_KS(2),
            0xa5 => FaultUnit::dIPP,
            0xa6 => FaultUnit::dIPP_CS,
            0xa7 => FaultUnit::dVDM_CSD,
            0xa8 => FaultUnit::dVDM_SSD,
            0xa9 => FaultUnit::dVDM_ILF,
            0xaa => FaultUnit::dVDM_ILD,
            0xab => FaultUnit::dRDE(0),
            0xac => FaultUnit::dRDE(1),
            0xad => FaultUnit::FC,
            0xae => FaultUnit::GSL2,
            0xb0..=0xb7 => FaultUnit::GL2CC_META(unit_code & 0xf),
            0xb8 => FaultUnit::GL2CC_MB,
            0xd0..=0xdf if g14x => match unit_code & 0xf {
                0x0 => FaultUnit::gCDM_CS,
                0x1 => FaultUnit::gCDM_ID,
                0x2 => FaultUnit::gCDM_CSR,
                0x3 => FaultUnit::gCDM_CSW,
                0x4 => FaultUnit::gCDM_CTXR,
                0x5 => FaultUnit::gCDM_CTXW,
                0x6 => FaultUnit::gIPP,
                0x7 => FaultUnit::gIPP_CS,
                0x8 => FaultUnit::gKSM_RCE,
                _ => FaultUnit::Unknown(unit_code),
            },
            0xe0..=0xff if g14x => match unit_code & 0xf {
                0x0 => FaultUnit::gPM_SP((unit_code >> 4) & 1),
                0x1 => FaultUnit::gVDM_CSD_SP((unit_code >> 4) & 1),
                0x2 => FaultUnit::gVDM_SSD_SP((unit_code >> 4) & 1),
                0x3 => FaultUnit::gVDM_ILF_SP((unit_code >> 4) & 1),
                0x4 => FaultUnit::gVDM_TFP_SP((unit_code >> 4) & 1),
                0x5 => FaultUnit::gVDM_MMB_SP((unit_code >> 4) & 1),
                0x6 => FaultUnit::gRDE0_SP((unit_code >> 4) & 1),
                _ => FaultUnit::Unknown(unit_code),
            },
            0xe0..=0xff if !g14x => match unit_code & 0xf {
                0x0 => FaultUnit::gPM_SP((unit_code >> 4) & 1),
                0x1 => FaultUnit::gVDM_CSD_SP((unit_code >> 4) & 1),
                0x2 => FaultUnit::gVDM_SSD_SP((unit_code >> 4) & 1),
                0x3 => FaultUnit::gVDM_ILF_SP((unit_code >> 4) & 1),
                0x4 => FaultUnit::gVDM_TFP_SP((unit_code >> 4) & 1),
                0x5 => FaultUnit::gVDM_MMB_SP((unit_code >> 4) & 1),
                0x6 => FaultUnit::gCDM_CS_KS0_SP((unit_code >> 4) & 1),
                0x7 => FaultUnit::gCDM_CS_KS1_SP((unit_code >> 4) & 1),
                0x8 => FaultUnit::gCDM_CS_KS2_SP((unit_code >> 4) & 1),
                0x9 => FaultUnit::gCDM_KS0_SP((unit_code >> 4) & 1),
                0xa => FaultUnit::gCDM_KS1_SP((unit_code >> 4) & 1),
                0xb => FaultUnit::gCDM_KS2_SP((unit_code >> 4) & 1),
                0xc => FaultUnit::gIPP_SP((unit_code >> 4) & 1),
                0xd => FaultUnit::gIPP_CS_SP((unit_code >> 4) & 1),
                0xe => FaultUnit::gRDE0_SP((unit_code >> 4) & 1),
                0xf => FaultUnit::gRDE1_SP((unit_code >> 4) & 1),
                _ => FaultUnit::Unknown(unit_code),
            },
            _ => FaultUnit::Unknown(unit_code),
        };

        let reason = match (fault_info >> 1) & 0x7 {
            0 => FaultReason::Unmapped,
            1 => FaultReason::AfFault,
            2 => FaultReason::WriteOnly,
            3 => FaultReason::ReadOnly,
            4 => FaultReason::NoAccess,
            a => FaultReason::Unknown(a as u8),
        };

        Some(FaultInfo {
            address: fault_addr << 6,
            sideband: ((fault_info >> 23) & 0x7f) as u8,
            vm_slot: ((fault_info >> 17) & 0x3f) as u32,
            unit_code,
            unit,
            level: ((fault_info >> 7) & 3) as u8,
            unk_5: ((fault_info >> 5) & 3) as u8,
            read: (fault_info & (1 << 4)) != 0,
            reason,
        })
    }
}

// -------------------------------------------------------------------------
// T8152 (J873, G18G) PMGR2 PSD state reads.
//
// J873 GPU power is PMGR2 PSD, not PMGR1 device power-states: the ADT sgx,
// gfx-asc and gfx1-asc power-gates/clock-gates/service-gates are implicit
// PSD services 3, 81 and 82, whose 0x100-byte service blocks sit at
// 0x303a10300, 0x303a15100 and 0x303a15200 (PSD aperture 0x303000000 +
// 0xa10000 + (service << 8); hv/tools/gpu_power.py contract()). In each
// block the state is at +0x80, the request at +0x90 and the acknowledgment
// at +0xbc.
//
// This module only READS state and ack. It maps the three blocks from the GPU
// node's "psd-sgx", "psd-gfx-asc" and "psd-gfx1-asc" resources without
// claiming them. It never writes the request register: PSD power requests
// are plan step 0.4 and need explicit user approval.
// -------------------------------------------------------------------------

const T8152_PSD_STATE: usize = 0x80;
const T8152_PSD_ACK: usize = 0xbc;
const T8152_PSD_STATE_MASK: u32 = 0x7;

/// One of the three PSD services that power the T8152 GPU.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) enum T8152PsdService {
    /// `sgx`, PSD service 3.
    Sgx,
    /// `gfx-asc` (GFX role ASC), PSD service 81.
    GfxAsc,
    /// `gfx1-asc` (GFX1 role ASC), PSD service 82.
    Gfx1Asc,
}

impl T8152PsdService {
    /// Every GPU PSD service, in ADT order.
    pub(crate) const ALL: [Self; 3] = [Self::Sgx, Self::GfxAsc, Self::Gfx1Asc];

    /// Native 26A428 request/state, reproduced on J873 with PMP running.
    pub(crate) const fn active_state(self) -> u32 {
        match self {
            Self::Sgx => 0,
            Self::GfxAsc | Self::Gfx1Asc => 2,
        }
    }

    const fn resource_name(self) -> &'static CStr {
        match self {
            Self::Sgx => c_str!("psd-sgx"),
            Self::GfxAsc => c_str!("psd-gfx-asc"),
            Self::Gfx1Asc => c_str!("psd-gfx1-asc"),
        }
    }

    /// PMGR2 implicit service id.
    pub(crate) const fn service_id(self) -> u32 {
        match self {
            Self::Sgx => 3,
            Self::GfxAsc => 81,
            Self::Gfx1Asc => 82,
        }
    }

    /// CPU-physical base of the service block.
    pub(crate) const fn physical_base(self) -> u64 {
        0x3_0300_0000 + 0xa1_0000 + ((self.service_id() as u64) << 8)
    }
}

/// One read-only sample of a PSD service's state and ack words.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) struct T8152PsdSample {
    pub(crate) service: T8152PsdService,
    pub(crate) state: u32,
    pub(crate) ack: u32,
}

impl T8152PsdSample {
    /// The PSD power state field, state[2:0].
    pub(crate) const fn power_state(&self) -> u32 {
        self.state & T8152_PSD_STATE_MASK
    }

    /// Point-in-time active-state proof, not a transition-completion test.
    /// J873 hardware confirms STATE reads clear ACK. A request must poll
    /// ACK first and verify STATE once; later access gates use STATE alone.
    pub(crate) const fn powered(&self) -> bool {
        self.power_state() == self.service.active_state()
    }

    /// Short steady-state verdict for logs.
    pub(crate) const fn verdict(&self) -> &'static str {
        if self.powered() {
            "powered"
        } else {
            "not powered"
        }
    }
}

/// `-EHOSTDOWN`: the block's PSD service is not proven powered, so every
/// MMIO read of it was skipped. The mailbox provider returns the same errno.
pub(crate) fn t8152_not_powered() -> Error {
    Error::from_errno(-(kernel::bindings::EHOSTDOWN as i32))
}

/// Proof that the SGX PSD service read as powered.
///
/// Built only by [`t8152_prove_sgx_powered`] from a fresh powered sample.
/// [`Resources::get_gpu_id_t8152`] requires it, so the SGX ID block cannot
/// be read without a preceding PSD read. The proof is a point-in-time fact.
/// It does not keep the block powered, so use it right away.
pub(crate) struct T8152SgxPowered {
    sample: T8152PsdSample,
}

impl T8152SgxPowered {
    pub(crate) const fn sample(&self) -> T8152PsdSample {
        self.sample
    }
}

/// Read one GPU PSD service's state and ack words. Reads only, no writes.
///
/// The block is mapped for this call alone and never claimed, so a later
/// power owner can map the same service block.
pub(crate) fn t8152_read_psd(
    pdev: &platform::Device<Core>,
    service: T8152PsdService,
) -> Result<T8152PsdSample> {
    let resource = pdev
        .resource_by_name(service.resource_name())
        .ok_or(ENODEV)?;
    if resource.start() != service.physical_base() || resource.size() != 0x100 {
        dev_err!(
            pdev.as_ref(),
            "T8152: PSD service {} resource {:#x}+{:#x} does not match {:#x}+0x100\n",
            service.service_id(),
            resource.start(),
            resource.size(),
            service.physical_base()
        );
        return Err(EINVAL);
    }
    let request = pdev
        .io_request_by_name(service.resource_name())
        .ok_or(ENODEV)?;
    let iomem = KBox::pin_init(request.iomap(), GFP_KERNEL)?;
    let io = iomem.try_access().ok_or(ENODEV)?;
    // STATE consumes the transition ACK; preserve it in the snapshot first.
    let ack = io.try_read32(T8152_PSD_ACK)?;
    let state = io.try_read32(T8152_PSD_STATE)?;
    Ok(T8152PsdSample {
        service,
        state,
        ack,
    })
}

/// Read the SGX PSD service and return a power proof only if it is active
/// (state 0). Otherwise log the sample and return [`t8152_not_powered`], so
/// the caller skips every SGX read.
pub(crate) fn t8152_prove_sgx_powered(
    pdev: &platform::Device<Core>,
) -> Result<T8152SgxPowered> {
    let sample = t8152_read_psd(pdev, T8152PsdService::Sgx)?;
    if !sample.powered() {
        dev_info!(
            pdev.as_ref(),
            "T8152: sgx PSD service 3 state={:#x} (power state {}) ack={:#x} is not active ({}); SGX reads skipped\n",
            sample.state,
            sample.power_state(),
            sample.ack,
            sample.verdict()
        );
        return Err(t8152_not_powered());
    }
    Ok(T8152SgxPowered { sample })
}
