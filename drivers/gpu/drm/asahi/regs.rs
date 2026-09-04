// SPDX-License-Identifier: GPL-2.0-only OR MIT

//! GPU MMIO register abstraction
//!
//! Since the vast majority of the interactions with the GPU are brokered through the firmware,
//! there is very little need to interact directly with GPU MMIO register. This module abstracts
//! the few operations that require that, mainly reading the MMU fault status, reading GPU ID
//! information, and starting the GPU firmware coprocessor.

use crate::{g17_submission, hw, identity, module_parameters};
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
/// G17P Fender dynamic-gating control. The pinned host clears bits 1 and 2
/// when it arms the firmware power callback; bit 2 set proves the callback is
/// disabled at the scheduler boundary.
const T8140_FENDER_DYNAMIC_GATING: usize = 0xd06030;
/// Host interrupt summary and enable/status words sampled by the native
/// scheduler-active boundary audit.
/// Offset of the native `accelerator+0x348` GPU register aperture within the
/// single `sgx` resource Linux maps (PA `0x480000000`, 64 MiB).
///
/// Every register offset recovered from the mac17p kext (`0x4000`/`0x4008`
/// tag/data port bases, `0x21068`/`0x21070` valid-mask registers, `0x21008`
/// queue config) is relative to that aperture, so it is rebased through this
/// one constant.
///
/// UNRESOLVED. `0` means "the aperture is the start of the sgx window", which
/// is the literal reading of the kext but makes `sgx+0x4090` take an
/// asynchronous SError on J700 because the whole `0x0..0x2ffff` range is
/// undecoded from the AP. Two alternatives were measured and BOTH are decoded
/// but inert -- writes are accepted and every readback stays `0x0`, so nothing
/// is actually programmed:
///   * `0xd00000` (second ADT sgx window, PA `0x480D00000`)
///   * `0x2600000` (the `asc` window, PA `0x482600000`)
/// Keep this at `0` until the true aperture is proven; a silent inert write is
/// strictly worse than a loud fault.
pub(crate) const T8140_GPU_REG_APERTURE_BASE: usize = 0;
/// GPC performance state. macOS reads this as `getGPCPerformanceState`
/// (`isGPUCorePowerEnabled` is `& 0xf` non-zero); it is a power register, not
/// an interrupt summary, despite the historical name of its reader here.
const T8140_HOST_IRQ_SUMMARY: usize = 0xe01000;
/// GPC host-interrupt enable (`enableGPCHostInterrupts` in macOS).
const T8140_HOST_IRQ_ENABLE: usize = 0xe01010;
const T8140_HOST_IRQ_STATUS: usize = 0xe01014;
/// GPC performance-state map used by the fragment register program.
///
/// `AGXAcceleratorG16::parsePerfStateMapRegs` maps the block at 0xe01400,
/// reads the word at +0x80, and retains bits 19:16. The 3D channel copies that
/// retained nibble into its command and `generate3DRegisterListG16Common`
/// emits it as register 0x1c838. Capture values therefore vary with hardware
/// state (3 and 7 have both been observed) and must not be inferred from render
/// geometry or pinned to one trace.
const T8140_GPC_PERF_STATE_MAP: usize = 0xe01480;
/// Earlier word in the same native performance-map snapshot. The 3D submit
/// path retains bit 0 and writes it to descriptor tail +0x2104.
const T8140_GPC_PERF_STATE_CONTROL: usize = 0xe0141c;

// G17P scheduler/USC engine-state diagnostics copied from the B1 firmware's
// own fault and explicit USC-state dump paths. The service and debug-status
// words are banked: B1 writes the USC pipe index to the corresponding selector
// before reading all three data-master views.
const T8140_KSM_ACTIVE_QUEUE_MASK: usize = 0xc120;
const T8140_KSM_ACTIVE_QUEUE_STATE: usize = 0xc128;
const T8140_KSM_FAULT_MASK_0: usize = 0xc0b0;
const T8140_KSM_FAULT_MASK_1: usize = 0xc0c8;
const T8140_KSM_FAULT_MASK_2: usize = 0xc0d8;
const T8140_KSM_DM_MASK_0: usize = 0xc160;
const T8140_KSM_DM_MASK_1: usize = 0xc168;
const T8140_KSM_DM_MASK_2: usize = 0xc170;
// Host reads of the KSM's 0x2_xxxx register namespace use the native
// bit-42 read tag. All of these words are 64-bit in B1: the firmware uses
// `ldr/str xN` for the pause and stop paths, and tests bits in the status and
// work-channel producer words as 64-bit values.
const T8140_KSM_QUEUE_STOPPED_MASK_LO: u64 = 0x21058;
const T8140_KSM_QUEUE_STOPPED_MASK_HI: u64 = 0x21060;
const T8140_KSM_QUEUE_STOP_REQUEST_LO: u64 = 0x210a8;
const T8140_KSM_QUEUE_STOP_REQUEST_HI: u64 = 0x210b0;
const T8140_KSM_RESOURCE_CLEAR_LO: u64 = 0x210c8;
const T8140_KSM_RESOURCE_CLEAR_HI: u64 = 0x210d0;
const T8140_KSM_RESOURCE_GO_LO: u64 = 0x210d8;
const T8140_KSM_RESOURCE_GO_HI: u64 = 0x210e0;
const T8140_KSM_RESOURCE_CALLBACK_STATE: u64 = 0x211b0;
const T8140_KSM_WORK_CHANNEL_0: u64 = 0x21168;
const T8140_KSM_WORK_CHANNEL_1: u64 = 0x21170;
const T8140_KSM_WORK_CHANNEL_2: u64 = 0x21178;
const T8140_KSM_WORK_CHANNEL_3: u64 = 0x21180;
const T8140_KSM_SCHEDULING_PAUSE_REQUEST: u64 = 0x211c8;
const T8140_KSM_SCHEDULING_PAUSE_STATUS: u64 = 0x211d0;
const T8140_KSM_SCHEDULER_STATUS: u64 = 0x21208;
const T8140_USC_SERV_SELECTOR: usize = 0xa010;
const T8140_USC_VDM_SERV: usize = 0xa088;
const T8140_USC_PDM_SERV: usize = 0xa068;
const T8140_USC_CDM_SERV: usize = 0xa0a8;
const T8140_USC_DEBUG_SELECTOR: usize = 0xa000;
const T8140_USC_VDM_DEBUG_STATUS: usize = 0xa090;
const T8140_USC_PDM_DEBUG_STATUS: usize = 0xa070;
const T8140_USC_CDM_DEBUG_STATUS: usize = 0xa0b0;

const fn t8140_gpc_perf_state_map(raw: u32) -> u32 {
    (raw >> 16) & 0xf
}

const fn t8140_gpc_perf_state_map_low(raw: u32) -> u32 {
    raw & 0xf
}

const fn t8140_gpc_perf_state_control(raw: u32) -> u32 {
    raw & 1
}

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
/// 64-bit requestor selector for the G14X+ fault-info/address window.
/// Acknowledge is a separate store of 1 to 0xd8e8.
const FAULT_REQUESTOR_G14X: usize = 0xd800;

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
    /// Suppressed SKSM port writes, kept so they can be replayed once the
    /// firmware has raised the GPU core power state. The port is only
    /// reachable from the AP while the cores are powered, and the driver has
    /// to publish the queue configuration before that happens.
    sksm_deferred: Pin<KBox<Mutex<G17PDeferredSksmWrites>>>,
}

pub(crate) struct G17PDeferredSksmWrites {
    pairs: [Option<g17_submission::G17SksmOrderedWritePair>; 8],
    count: usize,
    replayed: bool,
}

impl G17PDeferredSksmWrites {
    pub(crate) const fn new() -> Self {
        Self {
            pairs: [None; 8],
            count: 0,
            replayed: false,
        }
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) struct G17PSchedulerGateRegisters {
    pub(crate) fender_dynamic_gating: u32,
    pub(crate) host_irq_summary: u32,
    pub(crate) host_irq_enable: u32,
    pub(crate) host_irq_status: u32,
}

/// Read-only view of the GMMU interrupt source while the GPU cores are live.
///
/// Unlike [`g17p_log_fault_requestors_pre_ack`], taking this snapshot never
/// writes the requestor selector.  That makes it safe to sample in the dense
/// post-doorbell loop while firmware may be servicing the level-triggered
/// bit-35 interrupt itself.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) struct G17PFaultIrqSnapshot {
    pub(crate) gpc_state: u32,
    pub(crate) fault_info: u64,
    pub(crate) fault_addr_word: u64,
    pub(crate) requestor_gate: u64,
    pub(crate) sub_status: u64,
    pub(crate) irq_status: u64,
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) struct G17PHaltedGmmuSnapshot {
    pub(crate) gpc_state: u32,
    pub(crate) info_before: u64,
    pub(crate) address_word: Option<u64>,
    pub(crate) info_after: u64,
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) struct G17PHaltedSlot0Snapshot {
    pub(crate) gpc_state: u32,
    pub(crate) state_before: u64,
    pub(crate) key: u64,
    pub(crate) state_after: u64,
    pub(crate) progress: [u64; 8],
    pub(crate) progress_count: usize,
    pub(crate) progress_errno: i32,
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) struct G17PRenderKsmAdmissionSnapshot {
    pub(crate) gpc_state: u32,
    pub(crate) pdm_qid0_state: u64,
    pub(crate) pdm_qid0_progress: u64,
    pub(crate) ta_qid: u8,
    pub(crate) ta_state: u64,
    pub(crate) ta_progress: u64,
    pub(crate) fragment_qid: u8,
    pub(crate) fragment_state: u64,
    pub(crate) fragment_progress: u64,
    /// Per-QID {outstanding/timestamp, submitted, completed} from the same
    /// KSMFE registers used by Apple's queue-state dumper.
    /// TA, 3D, and the COMPUTE queue (qid 4). The third row is the control:
    /// compute is the queue that provably dispatches, so its counter domain
    /// is what the render rows must be compared against.
    pub(crate) qid_counters: [[u64; 3]; 3],
    /// Authoritative queue-enabled status. These are not the write-only
    /// set-valid strobes at 0x21068/0x21070.
    pub(crate) queue_enabled: [u64; 2],
    /// The valid mask tag-15 tests before writing the enable strobe.
    pub(crate) queue_valid_mask: [u64; 2],
    /// Per-QID descriptor register 0x21008 for the TA and 3D queues. This is
    /// what `configureHardware` writes and what the firmware's tag-15 install
    /// is expected to program: entry-ring base plus the hardware-set valid
    /// bit. Zero here means the queue was never installed, whatever the
    /// enable mask says.
    /// TA, 3D, and the COMPUTE queue (qid 4). Bit 63 is the hardware's
    /// "queue active" flag: `FUN_20d20` refuses to advance the completed
    /// stamp `0x21028[qid]` unless it is set, so the queue that provably
    /// dispatches is the control for whether the hardware ever sets it.
    pub(crate) queue_descriptor: [u64; 3],
    /// KSM slots 0..3 read by SLOT index, not by queue id. The 0xc020/0xc078
    /// bank is indexed by the scheduler slot a queue is loaded into; the queue
    /// id is recovered from the progress word's bits 46:40, and the kick
    /// timestamp from its low 40 bits. Reading these by queue id -- as the
    /// probe originally did -- samples empty slots and hides the fact that the
    /// work is loaded and waiting.
    pub(crate) slot_state: [u64; 4],
    pub(crate) slot_progress: [u64; 4],
    /// Per-data-master control (0xc140) and stopped (0xc148) words. DM0 owns
    /// bits 1:0 of the first and bit 0 of the second; a DM left stopped loads
    /// slots and never runs them, which is exactly the render symptom.
    /// The KSM's live per-queue ENABLED mask (0x21058/0x21060), the readback
    /// side of the enable pair the QID install writes to 0x21068/0x21070.
    /// b1 `FUN_12a84` walks the enabled queues by comparing its shadow
    /// (DAT_583d8/583e0) against these two words, so a queue whose bit is
    /// clear here is one the scheduler will never dispatch.
    pub(crate) queue_enabled_mask: [u64; 2],
    pub(crate) dm_control: u64,
    pub(crate) dm_stopped: u64,
    /// The per-queue masks tag-15 consults before it issues the resume
    /// strobes at 0x21098/0x210a0. If a render queue's bit is set here the
    /// resume never happens, which would leave the slot loaded and unstarted.
    pub(crate) queue_resume_mask: [u64; 2],
    /// Completed-timestamp register 0x21028 for queues 0..7. The render's 3D
    /// barrier waits on the TA queue's value, which never advances; sampling
    /// every queue shows whether the COMPUTE queue's accounting works, i.e.
    /// whether the mechanism exists at all and render simply misses it.
    pub(crate) completed_by_qid: [u64; 8],
    /// Pipe-0 TA registers read the way the firmware's own hang detector reads
    /// them (tag = pipe << 43 | reg, no namespace bit): the VDM control-stream
    /// pointer and the work stamp. Non-zero means the SKU stream was fetched
    /// and the TA is waiting for a launch; zero means the slot was admitted
    /// but nothing was ever read.
    ///
    /// Read for EVERY pipe, not just pipe 0. The KSM admits the fragment to a
    /// slot of its own, so a pipe-0-only read cannot distinguish "the fragment
    /// pipe was never armed" from "the fragment pipe was armed and stalled" --
    /// and pipe 0 still holds the TILING stream long after the TA retires,
    /// which made the single reading look armed when it was simply stale.
    pub(crate) pipe_vdm_stream: [u64; 4],
    pub(crate) pipe_work_stamp: [u64; 4],
    pub(crate) current_command: u64,
    pub(crate) current_context: u64,
    pub(crate) active_queue_mask: u64,
    pub(crate) active_queue_state: u64,
    pub(crate) fault_masks: [u64; 3],
    pub(crate) dm_masks: [u64; 3],
    pub(crate) scheduling_pause_request: u64,
    pub(crate) scheduling_pause_status: u64,
    pub(crate) queue_stop_request: [u64; 2],
    pub(crate) queue_stopped: [u64; 2],
    /// Resource-manager broadcasts used by B1's callback path. `clear` is
    /// written by 0x12a84 while releasing a requested mask; `go` is written by
    /// 0x15608 after installing the job callback. 0x211b0 becomes 4 while the
    /// callback list remains pending.
    pub(crate) resource_clear: [u64; 2],
    pub(crate) resource_go: [u64; 2],
    pub(crate) resource_callback_state: u64,
    /// PDM launch/interrupt chain used by B1 to advance a fragment job from
    /// scheduler state 2 to state 3. `launch_mode == 0x100` is the idle/reset
    /// value; FUN_15a48 overwrites it with 0 or 1 before `launch_strobe`.
    /// Firmware then requires bit 3 in `launch_irq_status` and the matching
    /// bit in `launch_slot_pending`.
    pub(crate) launch_slot: u32,
    pub(crate) launch_mode: u64,
    pub(crate) launch_strobe: u32,
    pub(crate) launch_irq_enable: u64,
    pub(crate) launch_irq_status: u64,
    pub(crate) launch_slot_pending: u64,
    pub(crate) launch_slot_ack: u64,
    /// Bit-4 new-work interrupt source. FUN_216f0 reads the QID bitmap at
    /// 0x10a60 and acknowledges that same bitmap at 0x10a68 before FUN_23004
    /// allocates the firmware job and emits KTrace code 2.
    pub(crate) new_work_pending: u64,
    pub(crate) new_work_ack: u64,
    pub(crate) scheduler_status: u64,
    pub(crate) work_channel_status: [u64; 4],
    pub(crate) usc: Option<G17PUscEngineSnapshot>,
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) struct G17PUscEngineSnapshot {
    pub(crate) pipe: u32,
    pub(crate) vdm_serv: u64,
    pub(crate) pdm_serv: u64,
    pub(crate) cdm_serv: u64,
    pub(crate) vdm_debug_status: u64,
    pub(crate) pdm_debug_status: u64,
    pub(crate) cdm_debug_status: u64,
    /// Pipe-banked fragment RCE target registers sampled with the same tag
    /// construction as B1's assigned-engine diagnostics. The first four keep
    /// the original execution/strobe probes; the final four are persistent RT
    /// bases (AXFB twice, heapmeta, tilemap) whose readback distinguishes a
    /// malformed RCE fetch from dIPP starting with a zero per-cluster base.
    pub(crate) fragment_rce_targets: [u64; 8],
}

impl G17PSchedulerGateRegisters {
    pub(crate) const fn fender_wake_armed(self) -> bool {
        self.fender_dynamic_gating & 0x6 == 0
    }
}

const _: () = {
    const T8140_SGX_RESOURCE_SIZE: usize = 0x400_0000;
    assert!(T8140_FENDER_DYNAMIC_GATING + 4 <= T8140_SGX_RESOURCE_SIZE);
    assert!(T8140_HOST_IRQ_SUMMARY + 4 <= T8140_SGX_RESOURCE_SIZE);
    assert!(T8140_HOST_IRQ_ENABLE + 4 <= T8140_SGX_RESOURCE_SIZE);
    assert!(T8140_HOST_IRQ_STATUS + 4 <= T8140_SGX_RESOURCE_SIZE);
    assert!(T8140_GPC_PERF_STATE_MAP + 4 <= T8140_SGX_RESOURCE_SIZE);
    assert!(T8140_GPC_PERF_STATE_CONTROL + 4 <= T8140_SGX_RESOURCE_SIZE);
    assert!(t8140_gpc_perf_state_map(0xffff_ffff) == 0xf);
    assert!(t8140_gpc_perf_state_map(0x0003_0000) == 3);
    assert!(t8140_gpc_perf_state_map(0x0007_0000) == 7);
    assert!(t8140_gpc_perf_state_map_low(0xffff_fff3) == 3);
    assert!(t8140_gpc_perf_state_control(0xffff_fff2) == 0);
    assert!(t8140_gpc_perf_state_control(0xffff_fff3) == 1);
    assert!(
        G17PSchedulerGateRegisters {
            fender_dynamic_gating: 0,
            host_irq_summary: 0,
            host_irq_enable: 0,
            host_irq_status: 0,
        }
        .fender_wake_armed()
    );
    assert!(
        !G17PSchedulerGateRegisters {
            fender_dynamic_gating: 4,
            host_irq_summary: 0,
            host_irq_enable: 0,
            host_irq_status: 0,
        }
        .fender_wake_armed()
    );
};

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
        // Offsets are aperture-relative (kext `accelerator+0x348`), so they are
        // rebased onto the second `sgx` window before use.
        self.0
            .try_write64(value, T8140_GPU_REG_APERTURE_BASE + offset as usize)
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
        let sksm_deferred = KBox::pin_init(
            new_mutex!(G17PDeferredSksmWrites::new(), "g17_sksm_deferred"),
            GFP_KERNEL,
        )?;

        Ok(Resources {
            // SAFETY: This device does DMA via the UAT IOMMU.
            dev: pdev.into(),
            sgx: sgx_iomem,
            sksm_fifo,
            sksm_deferred,
        })
    }

    fn g17p_sksm_pair_fits(pair: g17_submission::G17SksmOrderedWritePair) -> bool {
        let fits_u64 = |offset: u32| {
            let offset = offset as usize;
            offset % core::mem::size_of::<u64>() == 0
                && offset
                    .checked_add(T8140_GPU_REG_APERTURE_BASE)
                    .and_then(|base| base.checked_add(core::mem::size_of::<u64>()))
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

    /// DIAGNOSTIC: seed a queue's completed-timestamp register to zero.
    ///
    /// A fresh render queue's 0x21028 reads a different uninitialised value
    /// every boot, and the TA's kick entry carries an implicit parent of
    /// {own qid, TS 0}. If the KSM will not make a queue eligible until its
    /// completed timestamp actually reaches that parent, a garbage register
    /// blocks the launch forever -- which is exactly the observed symptom: the
    /// slot loads, the pipe arms, and nothing ever starts. Native never does
    /// this (the firmware seeds the register when it creates the job), so this
    /// exists only to identify the gate.
    pub(crate) fn g17p_seed_queue_completed_timestamp(&self, qid: u8, value: u64) -> Result {
        if !self.g17p_sksm_port_powered() {
            return Err(EAGAIN);
        }
        let _guard = self.sksm_fifo.lock();
        let sgx = self.sgx.try_access().ok_or(ENODEV)?;
        let registers = sgx.relaxed();
        let tag = 0x21028u64 | (u64::from(qid) << 43) | (1u64 << 42);
        registers.try_write64(tag, 0x4090)?;
        registers.try_write64(value, 0x4098)?;
        Ok(())
    }

    /// DIAGNOSTIC. Perform, from the host, the one-time KSM arm that b1's
    /// scheduler pass `FUN_25644` does for itself.
    ///
    /// That pass writes `sgx+0x21140 = 1 << 63` exactly once, gated on the
    /// firmware globals `DAT_124b8d == 1 && DAT_177d18 == 1`, and clears the
    /// first one afterwards (b1 0x25b00/0x25d48). Measured on hardware: on a
    /// cold boot a render publishes tag-15 and tag-14 for both stages and then
    /// NOTHING drains the work channel for ~21ms, after which firmware thread
    /// 4 raises pause reason 0x1000 and both queues are declared stopped with
    /// pending kicks (KTrace 0x42 for qid 5 AND qid 6) and NOP-retired. With
    /// one completed compute first, the same publication is drained 18us later
    /// (KTrace 0x47) and the TA executes. Three renders from cold never
    /// bootstrap it, so the trigger is a completed classic job, not merely a
    /// prior submission.
    ///
    /// Native never writes this: the whole Metal submit path performs ZERO
    /// MMIO (measured over every process, reads and writes). So this is not a
    /// candidate fix, only an instrument to decide whether that arm is the
    /// state a completed compute establishes.
    ///
    /// REFUTED ON HARDWARE 2026-09-02. The write lands (the port reports
    /// success and the GPC gate is open), and the cold render fails exactly as
    /// before: no drain, pause 0x1000 from thread 4, KTrace 0x42 for qid 5 and
    /// qid 6, both NOP-retired, tiling timestamps still zero. The one-time
    /// 0x21140 arm is therefore NOT what a completed compute establishes.
    /// Kept, default off, so the negative result is not re-derived.
    pub(crate) fn g17p_ksm_cold_arm(&self) -> Result {
        if !self.g17p_sksm_port_powered() {
            return Err(EAGAIN);
        }
        let _guard = self.sksm_fifo.lock();
        let sgx = self.sgx.try_access().ok_or(ENODEV)?;
        let registers = sgx.relaxed();
        let tag = 0x21140u64 | (1u64 << 42);
        registers.try_write64(tag, 0x4090)?;
        registers.try_write64(1u64 << 63, 0x4098)?;
        Ok(())
    }

    /// Sweep the pipe register window for BOTH pipes and log every offset
    /// whose two readings differ or are non-zero.
    ///
    /// The fragment is retired by hardware with no data-master fetch, and the
    /// two pipe registers we already read (0x1_c880 control stream, 0x1_c9f0
    /// work stamp) say pipe 0 is armed with the tiling stream while pipe 1
    /// stays zero. Rather than guess which further register arms a pipe, read
    /// the whole window both ways and let the asymmetry name itself.
    ///
    /// Reads only, through the same tag/data port the existing snapshot uses,
    /// and gated on the port being drivable.
    pub(crate) fn g17p_sweep_pipe_registers(&self, label: &str) -> Result {
        if !self.g17p_sksm_port_powered() {
            return Err(EAGAIN);
        }
        let _guard = self.sksm_fifo.lock();
        let sgx = self.sgx.try_access().ok_or(ENODEV)?;
        let mut differing = 0usize;
        let mut offset = 0x1_c800u64;
        while offset < 0x1_cc00 {
            let pipe0 = Self::g17p_sksm_port_read_locked(&sgx, offset, 0, 0);
            let pipe1 = Self::g17p_sksm_port_read_locked(&sgx, offset, 1, 0);
            if let (Ok(a), Ok(b)) = (pipe0, pipe1) {
                if a != b || a != 0 {
                    let mark = if a != b { " DIFF" } else { "" };
                    dev_info!(
                        self.dev.as_ref(),
                        "G17P PIPESWEEP[{}] {:#07x}: pipe0={:#018x} pipe1={:#018x}{}\n",
                        label,
                        offset,
                        a,
                        b,
                        mark,
                    );
                    differing += 1;
                }
            }
            offset += 8;
        }
        dev_info!(
            self.dev.as_ref(),
            "G17P PIPESWEEP[{}]: {} interesting offsets in 0x1c800..0x1cc00\n",
            label,
            differing,
        );
        Ok(())
    }

    /// DIAGNOSTIC. Seed the KSM work-channel word the compute establishes.
    ///
    /// Diffing a cold render-only boot against a compute-then-render boot at
    /// the same poll shows exactly one difference in the scheduler bank:
    ///
    ///   render-only cold   work-channels=[0,0,0,0]  KSM slot 0 holds the
    ///                      TILER (qid 5) at state 0x0100000d phase 6, stuck
    ///   compute first      work-channels=[1,0,0,0]  KSM slot 0 holds the
    ///                      FRAGMENT (qid 6) in that same stuck state, and
    ///                      the tiler ran instead
    ///
    /// So whichever render queue is admitted to the KSM sticks at phase 6, and
    /// what a completed compute changes is this word going 0 -> 1, after which
    /// the tiler takes the classic path and executes. This asks whether the
    /// word itself is the gate.
    pub(crate) fn g17p_seed_work_channel(&self, value: u64) -> Result {
        if !self.g17p_sksm_port_powered() {
            return Err(EAGAIN);
        }
        let _guard = self.sksm_fifo.lock();
        let sgx = self.sgx.try_access().ok_or(ENODEV)?;
        let registers = sgx.relaxed();
        let tag = T8140_KSM_WORK_CHANNEL_0 | (1u64 << 42);
        registers.try_write64(tag, 0x4090)?;
        registers.try_write64(value, 0x4098)?;
        Ok(())
    }

    /// DIAGNOSTIC. Perform, from the host, the 3D launch strobe that b1's
    /// tiling job would issue from its state 4.
    ///
    /// FW-PROVEN: `0x1c9a0 = 2` is the ONLY 3D launch in the image and has a
    /// single writer, `FUN_14cd4` state 4, reached only from a tiling job's
    /// state machine, after `0x10398` selects that job's slot. Measured here:
    /// the new-work and tiling-done interrupts are enabled (0x10a08 =
    /// 0xa00620038) and never raised, `0x10a60` stays 0, no job is ever built
    /// (KTrace 2 absent), so state 4 is unreachable and the fragment sits in
    /// its KSM slot. The hardware has meanwhile latched the fragment at
    /// 0xc0f0/0xc0f8 with dm-masks[1] set, so the work is presented.
    ///
    /// Native performs ZERO MMIO, so this can never ship. It answers one
    /// question: if the strobe is issued, does the fragment execute?
    pub(crate) fn g17p_strobe_3d_launch(&self, selector: u32) -> Result<(u32, u64, u32)> {
        let sgx = self.sgx.try_access().ok_or(ENODEV)?;
        let registers = sgx.relaxed();
        registers.try_write32(selector, 0x1_0398)?;
        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
        registers.try_write32(2, 0x1_c9a0)?;
        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
        Ok((
            registers.try_read32(0x1_0398)?,
            registers.try_read64(0x1_0408)?,
            registers.try_read32(0x1_c9a0)?,
        ))
    }

    /// DIAGNOSTIC. Set descriptor bit 62 for one queue, as b1 `FUN_162f8`
    /// does (`0x21008 |= 1 << 62`) after releasing the fragment's kick entry.
    /// Measured: our descriptors read 0x11.. and 0x21.. with bit 62 clear on
    /// both queues, and tag-16 -- which the firmware acknowledges with KTrace
    /// 0x57 for both render queues -- does not set it either.
    pub(crate) fn g17p_activate_queue_descriptor(&self, qid: u8) -> Result<(u64, u64)> {
        if !self.g17p_sksm_port_powered() {
            return Err(EAGAIN);
        }
        let _guard = self.sksm_fifo.lock();
        let sgx = self.sgx.try_access().ok_or(ENODEV)?;
        let before = Self::g17p_sksm_port_read_locked(&sgx, 0x21008, u64::from(qid), 1u64 << 42)?;
        let registers = sgx.relaxed();
        let tag = 0x21008u64 | (u64::from(qid) << 43) | (1u64 << 42);
        registers.try_write64(tag, 0x4090)?;
        registers.try_write64(before | (1u64 << 62), 0x4098)?;
        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
        let after = Self::g17p_sksm_port_read_locked(&sgx, 0x21008, u64::from(qid), 1u64 << 42)?;
        Ok((before, after))
    }

    /// DIAGNOSTIC. Set descriptor bit 63 for one queue before the doorbell.
    ///
    /// b1's admit block at 0x12dc0 windows the per-queue register file with
    /// `sgx[0x103a8] = qid` and then tests `tbnz sgx[0x21008], #63`. Set, the
    /// admitted byte becomes `0x21020 - 0x21028 + 1`; clear, it is zero, which
    /// leaves A equal to B at `0x58010 + qid*0x28 + 0x418/0x419` and the
    /// scheduler parks the queue and NOP-retires it with status 4 -- the exact
    /// cold-render symptom. Nothing in the firmware image writes bit 63 (only
    /// bit 62, at 0xd1d4 and 0x164e4), so it is hardware-set, and a cold boot
    /// is precisely the case where the hardware has never set it.
    ///
    /// This asks whether that one bit is the cold gate. Native performs zero
    /// MMIO on the submit path, so a positive result IDENTIFIES the gate and
    /// does not license keeping the write.
    pub(crate) fn g17p_arm_queue_admit(&self, qid: u8) -> Result<(u64, u64)> {
        if !self.g17p_sksm_port_powered() {
            return Err(EAGAIN);
        }
        let _guard = self.sksm_fifo.lock();
        let sgx = self.sgx.try_access().ok_or(ENODEV)?;
        let before = Self::g17p_sksm_port_read_locked(&sgx, 0x21008, u64::from(qid), 1u64 << 42)?;
        let registers = sgx.relaxed();
        let tag = 0x21008u64 | (u64::from(qid) << 43) | (1u64 << 42);
        registers.try_write64(tag, 0x4090)?;
        registers.try_write64(before | (1u64 << 63), 0x4098)?;
        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
        let after = Self::g17p_sksm_port_read_locked(&sgx, 0x21008, u64::from(qid), 1u64 << 42)?;
        Ok((before, after))
    }

    /// DIAGNOSTIC. Sample KSM slot 0's state register in a tight burst and
    /// return the distinct values seen, in order.
    ///
    /// The render poll loop samples before the doorbell and again about a
    /// millisecond later, by which time the tiler has already run and retired.
    /// Everything known about a slot's life therefore comes from two frames:
    /// empty, then the fragment stuck at 0x0100000d. A burst catches the
    /// transitions -- what a slot looks like while a queue is actually
    /// dispatched and progressing -- which is the "good" reference this has
    /// never had.
    pub(crate) fn g17p_burst_sample_slot0(&self, out: &mut [u64; 16]) -> Result<usize> {
        if !self.g17p_sksm_port_powered() {
            return Err(EAGAIN);
        }
        let _guard = self.sksm_fifo.lock();
        let sgx = self.sgx.try_access().ok_or(ENODEV)?;
        let mut distinct = 0usize;
        let mut last = u64::MAX;
        for _ in 0..256 {
            let state = Self::g17p_sksm_port_read_locked(&sgx, 0xc020, 0, 1u64 << 42)?;
            if state != last {
                if distinct < out.len() {
                    out[distinct] = state;
                }
                distinct += 1;
                last = state;
            }
        }
        Ok(distinct)
    }

    /// DIAGNOSTIC. Burst-sample the GPU interrupt status 0x1_0a10 together
    /// with the new-work and completion slot masks, returning the distinct
    /// (status, new-work, completion) triples seen.
    ///
    /// Every sample so far has been a poll a millisecond apart, and has shown
    /// bit 3 and bit 4 clear with 0x10a60 empty. Both are enabled in 0x10a08
    /// (0xa00620038). If they are raised and serviced between polls this would
    /// catch them; if they never appear in a tight burst either, the firmware
    /// is not receiving GPU hardware interrupts at all -- which would explain
    /// why no job is ever created for ANY workload, compute included.
    pub(crate) fn g17p_burst_sample_irq(&self, out: &mut [(u64, u64, u64); 12]) -> Result<usize> {
        // MUST be gated: an sgx read taken with the graphics cores gated off
        // raises an asynchronous SError and kills the machine. The render
        // window happens to run with the cores up, so this was survivable
        // there; calling the same helper from the compute path without the
        // gate killed the target outright.
        if !self.g17p_sksm_port_powered() {
            return Err(EAGAIN);
        }
        let sgx = self.sgx.try_access().ok_or(ENODEV)?;
        let registers = sgx.relaxed();
        let mut distinct = 0usize;
        let mut last = (u64::MAX, u64::MAX, u64::MAX);
        for _ in 0..512 {
            // 0x10a10 is the raw pending status, which the ISR clears almost
            // immediately, so a host sample nearly always reads it as 0.
            // 0x10a18 is where the ISR WRITES the bits it just serviced
            // (b1 0x21760: str x9, [x8, #0x10a18], x9 = status & DAT_179328),
            // so it retains the last serviced set and is the register that
            // actually answers "has bit 4 ever been handled".
            let now = (
                registers.try_read64(0x1_0a10)?,
                registers.try_read64(0x1_0a18)?,
                registers.try_read64(0x1_0a60)?,
            );
            if now != last {
                if distinct < out.len() {
                    out[distinct] = now;
                }
                distinct += 1;
                last = now;
            }
        }
        Ok(distinct)
    }

    /// Whether the SKSM tag/data port can be driven from the host right now.
    ///
    /// The asynchronous SErrors that made every host SKSM write suppressible
    /// were taken with the graphics cores gated off. The read path already
    /// writes the same port -- `g17p_sksm_port_read_locked` stores a tag at
    /// 0x4090 before loading 0x4098 -- and does so without incident whenever
    /// the always-on GPC state is non-zero, which is direct evidence that the
    /// aperture is decoded and the fault was a power-gate artifact. Apply the
    /// same precondition to writes instead of refusing them outright.
    fn g17p_sksm_port_powered(&self) -> bool {
        self.sgx.try_access().is_some_and(|sgx| {
            sgx.relaxed()
                .try_read32(T8140_HOST_IRQ_SUMMARY)
                .is_ok_and(|state| state & 0xf != 0)
        })
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
        if *module_parameters::g17p_sksm_mmio.value() == 0 || !self.g17p_sksm_port_powered() {
            dev_info!(
                self.dev.as_ref(),
                "G17P SKSM port MMIO suppressed (enabled={}, powered={}): {:#x}@{:#x} / {:#x}@{:#x}\n",
                *module_parameters::g17p_sksm_mmio.value(),
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

        if *module_parameters::g17p_sksm_mmio.value() == 0 || !self.g17p_sksm_port_powered() {
            dev_info!(
                self.dev.as_ref(),
                "G17P QID configure suppressed (enabled={}, powered={}, tag={:#x}@{:#x} payload={:#x}@{:#x} maskHi={:#x} maskLo={:#x})\n",
                *module_parameters::g17p_sksm_mmio.value(),
                self.g17p_sksm_port_powered(),
                pair.word0, pair.word0_offset, pair.word1, pair.word1_offset,
                enable_pair.word0, enable_pair.word1
            );
            self.stash_deferred_sksm_pair(pair);
            self.stash_deferred_sksm_pair(enable_pair);
            return Ok(());
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
    /// Read `FAULT_INFO` in the sgx GPU-core register file. Unreachable from
    /// the AP while the cores are unpowered (asynchronous SError); this exists
    /// to retest it once the firmware has raised the GPC performance state.
    fn stash_deferred_sksm_pair(&self, pair: g17_submission::G17SksmOrderedWritePair) {
        let mut deferred = self.sksm_deferred.lock();
        let count = deferred.count;
        if count < deferred.pairs.len() {
            deferred.pairs[count] = Some(pair);
            deferred.count = count + 1;
            // A newly deferred pair has never been issued, so the one-shot
            // latch must reopen. It was written for a single compute replay;
            // left latched, the FIRST render's suppressed fragment QID install
            // would be the last one ever replayed and every later submission
            // would silently drop its own.
            deferred.replayed = false;
        }
    }

    /// Replay every suppressed SKSM port write, once. The port lives in the
    /// sgx GPU-core register file, which the AP can only reach while the cores
    /// are powered -- and the firmware only raises the GPC performance state
    /// in response to a submission, after the queue configuration has already
    /// had to be published. So the writes are recorded when they are skipped
    /// and issued here, from the submit poll loop, once power is up.
    pub(crate) fn g17p_replay_deferred_sksm(&self) -> Result<usize> {
        let (pairs, count) = {
            let mut deferred = self.sksm_deferred.lock();
            if deferred.replayed {
                return Ok(0);
            }
            deferred.replayed = true;
            (deferred.pairs, deferred.count)
        };
        if count == 0 {
            return Ok(0);
        }
        let sgx = self.sgx.try_access().ok_or(ENODEV)?;
        let _guard = self.sksm_fifo.lock();
        let mut issued = 0usize;
        for slot in pairs.iter().take(count) {
            let Some(pair) = slot else {
                continue;
            };
            dev_info!(
                self.dev.as_ref(),
                "G17P SKSM replay (powered): {:#x}@{:#x} / {:#x}@{:#x}\n",
                pair.word0, pair.word0_offset, pair.word1, pair.word1_offset
            );
            Self::g17p_sksm_publish_locked(&sgx, *pair)?;
            issued += 1;
        }
        Ok(issued)
    }

    pub(crate) fn g17p_probe_fault_info(&self) -> Result<u32> {
        let sgx = self.sgx.try_access().ok_or(ENODEV)?;
        sgx.relaxed().try_read32(FAULT_INFO)
    }

    /// Read one KSM register back through the SKSM tag/data port while the
    /// GPU cores are powered. Native's own hang detector uses this same port
    /// to read per-QID progress, so it is a supported read path -- unlike a
    /// direct load of `0x21008`, which returns 0 because the register file is
    /// only addressable through the port.
    pub(crate) fn g17p_sksm_port_read(&self, reg: u64, qid: u64) -> Result<u64> {
        let sgx = self.sgx.try_access().ok_or(ENODEV)?;
        let _guard = self.sksm_fifo.lock();
        Self::g17p_sksm_port_read_locked(&sgx, reg, qid, 1u64 << 42)
    }

    fn g17p_sksm_port_read_locked(
        sgx: &IoMem<0>,
        reg: u64,
        qid: u64,
        tag_flags: u64,
    ) -> Result<u64> {
        let tag = reg | (qid << 43) | tag_flags;
        // The tag store and data load form one ordered transaction. Apple's
        // host uses a device-memory STR/LDR pair; Linux's ordered writeq/readq
        // provides the corresponding MMIO ordering. Relaxed accesses returned
        // opaque values for 0x21028 and hid the TA -> 3D dependency frontier.
        sgx.try_write64(tag, 0x4090)?;
        sgx.try_read64(0x4098)
    }

    /// Caller must own the runtime mutex and have just verified the firmware
    /// is stopped at the pre-ACK recovery handshake with DM1 slot0 blamed.
    /// This is not an admission probe and must never run before that halt.
    pub(crate) fn g17p_halted_slot0_snapshot(&self) -> Result<Option<G17PHaltedSlot0Snapshot>> {
        let sgx = self.sgx.try_access().ok_or(ENODEV)?;
        let _guard = self.sksm_fifo.lock();
        // The GPC summary is in the always-on range. A halted firmware need
        // not imply powered graphics cores; skip all low-SGX access if off.
        let gpc_state = sgx.try_read32(T8140_HOST_IRQ_SUMMARY)? & 0xf;
        if gpc_state == 0 {
            return Ok(None);
        }
        // B1 FUN_6a20 uses the bit42 namespace for c020, but not c078.
        // Index 0 is a hardware SLOT, never the fragment's physical QID.
        // The host port at4090 is distinct from B1's seven thread ports
        // (4080,40a0,40b0,40c0,40d0,40e0,40f0; table45c78).
        let state_before = Self::g17p_sksm_port_read_locked(&sgx, 0xc020, 0, 1u64 << 42)?;
        let key = Self::g17p_sksm_port_read_locked(&sgx, 0xc078, 0, 0)?;
        let mut progress = [0u64; 8];
        let mut progress_count = 0;
        let mut progress_errno = 0;
        if let Some(selector) = crate::g17_completion::g17p_dm1_monitor_selector_index(state_before) {
            // These are the eight reads B1 FUN_6a20 performs for DM1, through
            // the selected progress port. Do not write10400/10408 or touch
            // any bank absent from the native sample table.
            for (index, (register, flags)) in crate::g17_completion::G17P_DM1_MONITOR_READS
                .iter().enumerate()
            {
                match Self::g17p_sksm_port_read_locked(&sgx, *register, selector, *flags) {
                    Ok(value) => {
                        progress[index] = value;
                        progress_count += 1;
                    }
                    Err(error) => {
                        progress_errno = error.to_errno();
                        break;
                    }
                }
            }
        }
        let state_after = Self::g17p_sksm_port_read_locked(&sgx, 0xc020, 0, 1u64 << 42)?;
        Ok(Some(G17PHaltedSlot0Snapshot {
            gpc_state,
            state_before,
            key,
            state_after,
            progress,
            progress_count,
            progress_errno,
        }))
    }

    /// Take the hardware snapshot the G17P firmware uses when diagnosing a
    /// selected command which has not reached its start timestamp. Include the
    /// kernel-owned TA/3D pair and the launch registers which move a TA command
    /// through B1's bit-4 new-work and bit-3 completion callbacks.
    ///
    /// The low SGX register file is inaccessible while the GPU cores are off,
    /// so the GPC-state read is a hard precondition rather than another field
    /// sampled after the dangerous accesses. `None` means the caller may retry
    /// briefly while it is still in the immediate post-doorbell window.
    /// Read one 0x100-byte sgx register window for the pre-firmware
    /// hardware-state audit. Caller must only invoke this where the GPU cores
    /// are known powered: an sgx read with the cores gated raises an
    /// asynchronous SError and panics the machine.
    pub(crate) fn g17p_dump_sgx_window(&self, base: usize, out: &mut [u32; 64]) -> Result {
        let sgx = self.sgx.try_access().ok_or(ENODEV)?;
        let registers = sgx.relaxed();
        for (index, word) in out.iter_mut().enumerate() {
            *word = registers.try_read32(base + index * 4)?;
        }
        Ok(())
    }

    pub(crate) fn g17p_render_ksm_admission_snapshot(
        &self,
        ta_qid: u8,
        fragment_qid: u8,
    ) -> Result<Option<G17PRenderKsmAdmissionSnapshot>> {
        let sgx = self.sgx.try_access().ok_or(ENODEV)?;
        let registers = sgx.relaxed();
        let gpc_state = registers.try_read32(T8140_HOST_IRQ_SUMMARY)? & 0xf;
        if gpc_state == 0 {
            return Ok(None);
        }

        let _guard = self.sksm_fifo.lock();
        // These tags are copied from the firmware's own readers. State uses
        // the bit-42 KSM namespace; the hang-detector progress read does not.
        // QID 0 remains useful only as a reserved-queue control. The type-1
        // descriptor's +0x79c/+0x7a8 is an SKU register-stream header, not a
        // QID-0/slot-89 dependency; the real dependency is in the TA record.
        let pdm_qid0_state =
            Self::g17p_sksm_port_read_locked(&sgx, 0xc020, 0, 1u64 << 42)?;
        let pdm_qid0_progress = Self::g17p_sksm_port_read_locked(&sgx, 0xc078, 0, 0)?;
        let ta_state = Self::g17p_sksm_port_read_locked(
            &sgx,
            0xc020,
            u64::from(ta_qid),
            1u64 << 42,
        )?;
        let ta_progress =
            Self::g17p_sksm_port_read_locked(&sgx, 0xc078, u64::from(ta_qid), 0)?;
        let fragment_state = Self::g17p_sksm_port_read_locked(
            &sgx,
            0xc020,
            u64::from(fragment_qid),
            1u64 << 42,
        )?;
        let fragment_progress =
            Self::g17p_sksm_port_read_locked(&sgx, 0xc078, u64::from(fragment_qid), 0)?;
        let qid_counters = [
            [
                Self::g17p_sksm_port_read_locked(
                    &sgx,
                    0x21010,
                    u64::from(ta_qid),
                    1u64 << 42,
                )?,
                Self::g17p_sksm_port_read_locked(
                    &sgx,
                    0x21020,
                    u64::from(ta_qid),
                    1u64 << 42,
                )?,
                Self::g17p_sksm_port_read_locked(
                    &sgx,
                    0x21028,
                    u64::from(ta_qid),
                    1u64 << 42,
                )?,
            ],
            [
                Self::g17p_sksm_port_read_locked(
                    &sgx,
                    0x21010,
                    u64::from(fragment_qid),
                    1u64 << 42,
                )?,
                Self::g17p_sksm_port_read_locked(
                    &sgx,
                    0x21020,
                    u64::from(fragment_qid),
                    1u64 << 42,
                )?,
                Self::g17p_sksm_port_read_locked(
                    &sgx,
                    0x21028,
                    u64::from(fragment_qid),
                    1u64 << 42,
                )?,
            ],
            [
                Self::g17p_sksm_port_read_locked(&sgx, 0x21010, 4, 1u64 << 42)?,
                Self::g17p_sksm_port_read_locked(&sgx, 0x21020, 4, 1u64 << 42)?,
                Self::g17p_sksm_port_read_locked(&sgx, 0x21028, 4, 1u64 << 42)?,
            ],
        ];
        let queue_enabled = [
            Self::g17p_sksm_port_read_locked(&sgx, 0x21038, 0, 1u64 << 42)?,
            Self::g17p_sksm_port_read_locked(&sgx, 0x21040, 0, 1u64 << 42)?,
        ];
        let mut slot_state = [0u64; 4];
        let mut slot_progress = [0u64; 4];
        for slot in 0..4u64 {
            slot_state[slot as usize] =
                Self::g17p_sksm_port_read_locked(&sgx, 0xc020, slot, 1u64 << 42)?;
            slot_progress[slot as usize] =
                Self::g17p_sksm_port_read_locked(&sgx, 0xc078, slot, 0)?;
        }
        let mut completed_by_qid = [0u64; 8];
        for qid in 0..8u64 {
            completed_by_qid[qid as usize] =
                Self::g17p_sksm_port_read_locked(&sgx, 0x21028, qid, 1u64 << 42)?;
        }
        // The valid mask the tag-15 handler TESTS at fw 0x11b60 before deciding
        // whether to write the enable strobe pair 0x21070/0x21068. If a queue
        // already reads valid here the firmware SKIPS the enable entirely, so
        // this is distinct from the authoritative enabled status at
        // 0x21058/0x21060 that this snapshot already carries.
        let queue_valid_mask = [
            Self::g17p_sksm_port_read_locked(&sgx, 0x21038, 0, 1u64 << 42)?,
            Self::g17p_sksm_port_read_locked(&sgx, 0x21040, 0, 1u64 << 42)?,
        ];
        let queue_resume_mask = [
            Self::g17p_sksm_port_read_locked(&sgx, 0x21048, 0, 1u64 << 42)?,
            Self::g17p_sksm_port_read_locked(&sgx, 0x21050, 0, 1u64 << 42)?,
        ];
        let queue_enabled_mask = [
            Self::g17p_sksm_port_read_locked(&sgx, 0x21058, 0, 1u64 << 42)?,
            Self::g17p_sksm_port_read_locked(&sgx, 0x21060, 0, 1u64 << 42)?,
        ];
        let dm_control = registers.try_read64(0xc140)?;
        let dm_stopped = registers.try_read64(0xc148)?;
        let pipe_vdm_stream = [
            Self::g17p_sksm_port_read_locked(&sgx, 0x1_c880, 0, 0)?,
            Self::g17p_sksm_port_read_locked(&sgx, 0x1_c880, 1, 0)?,
            Self::g17p_sksm_port_read_locked(&sgx, 0x1_c880, 2, 0)?,
            Self::g17p_sksm_port_read_locked(&sgx, 0x1_c880, 3, 0)?,
        ];
        let pipe_work_stamp = [
            Self::g17p_sksm_port_read_locked(&sgx, 0x1_c9f0, 0, 0)?,
            Self::g17p_sksm_port_read_locked(&sgx, 0x1_c9f0, 1, 0)?,
            Self::g17p_sksm_port_read_locked(&sgx, 0x1_c9f0, 2, 0)?,
            Self::g17p_sksm_port_read_locked(&sgx, 0x1_c9f0, 3, 0)?,
        ];
        let queue_descriptor = [
            Self::g17p_sksm_port_read_locked(&sgx, 0x21008, u64::from(ta_qid), 1u64 << 42)?,
            Self::g17p_sksm_port_read_locked(
                &sgx,
                0x21008,
                u64::from(fragment_qid),
                1u64 << 42,
            )?,
                    Self::g17p_sksm_port_read_locked(&sgx, 0x21008, 4, 1u64 << 42)?,
        ];
        let current_command = registers.try_read64(0xc0f0)?;
        let current_context = registers.try_read64(0xc0f8)?;
        let active_queue_mask = registers.try_read64(T8140_KSM_ACTIVE_QUEUE_MASK)?;
        let active_queue_state = registers.try_read64(T8140_KSM_ACTIVE_QUEUE_STATE)?;
        let fault_masks = [
            registers.try_read64(T8140_KSM_FAULT_MASK_0)?,
            registers.try_read64(T8140_KSM_FAULT_MASK_1)?,
            registers.try_read64(T8140_KSM_FAULT_MASK_2)?,
        ];
        let dm_masks = [
            registers.try_read64(T8140_KSM_DM_MASK_0)?,
            registers.try_read64(T8140_KSM_DM_MASK_1)?,
            registers.try_read64(T8140_KSM_DM_MASK_2)?,
        ];
        // B1 accesses these as global 64-bit KSM registers. Use QID zero in
        // the host tag because the index field is immaterial for globals; the
        // bit-42 read tag is the same one already validated by 0x211d0.
        // 0x210a8/0x210b0 are stop-request command registers, so their
        // readback is diagnostic only. 0x21058/0x21060 are the authoritative
        // stopped masks which B1 polls after issuing those requests.
        let scheduling_pause_request = Self::g17p_sksm_port_read_locked(
            &sgx,
            T8140_KSM_SCHEDULING_PAUSE_REQUEST,
            0,
            1u64 << 42,
        )?;
        let scheduling_pause_status = Self::g17p_sksm_port_read_locked(
            &sgx,
            T8140_KSM_SCHEDULING_PAUSE_STATUS,
            0,
            1u64 << 42,
        )?;
        let queue_stop_request = [
            Self::g17p_sksm_port_read_locked(
                &sgx,
                T8140_KSM_QUEUE_STOP_REQUEST_LO,
                0,
                1u64 << 42,
            )?,
            Self::g17p_sksm_port_read_locked(
                &sgx,
                T8140_KSM_QUEUE_STOP_REQUEST_HI,
                0,
                1u64 << 42,
            )?,
        ];
        let queue_stopped = [
            Self::g17p_sksm_port_read_locked(
                &sgx,
                T8140_KSM_QUEUE_STOPPED_MASK_LO,
                0,
                1u64 << 42,
            )?,
            Self::g17p_sksm_port_read_locked(
                &sgx,
                T8140_KSM_QUEUE_STOPPED_MASK_HI,
                0,
                1u64 << 42,
            )?,
        ];
        let resource_clear = [
            Self::g17p_sksm_port_read_locked(
                &sgx,
                T8140_KSM_RESOURCE_CLEAR_LO,
                0,
                1u64 << 42,
            )?,
            Self::g17p_sksm_port_read_locked(
                &sgx,
                T8140_KSM_RESOURCE_CLEAR_HI,
                0,
                1u64 << 42,
            )?,
        ];
        let resource_go = [
            Self::g17p_sksm_port_read_locked(
                &sgx,
                T8140_KSM_RESOURCE_GO_LO,
                0,
                1u64 << 42,
            )?,
            Self::g17p_sksm_port_read_locked(
                &sgx,
                T8140_KSM_RESOURCE_GO_HI,
                0,
                1u64 << 42,
            )?,
        ];
        let resource_callback_state = Self::g17p_sksm_port_read_locked(
            &sgx,
            T8140_KSM_RESOURCE_CALLBACK_STATE,
            0,
            1u64 << 42,
        )?;
        // FUN_15a48 publishes these immediately before marking the firmware
        // job state 2. FUN_216f0 then consumes IRQ status bit 3 and the
        // per-slot pending bitmap to make the state-3 callback runnable. A
        // 0x100 mode readback proves the launch write was not reached.
        // These are read-only observations; in particular, do not write the
        // 0x10a58 acknowledge register from the live diagnostic path.
        let launch_slot = registers.try_read32(0x1_0398)?;
        let launch_mode = registers.try_read64(0x1_0408)?;
        let launch_strobe = registers.try_read32(0x1_c998)?;
        let launch_irq_enable = registers.try_read64(0x1_0a08)?;
        let launch_irq_status = registers.try_read64(0x1_0a10)?;
        let launch_slot_pending = registers.try_read64(0x1_0a50)?;
        let launch_slot_ack = registers.try_read64(0x1_0a58)?;
        let new_work_pending = registers.try_read64(0x1_0a60)?;
        let new_work_ack = registers.try_read64(0x1_0a68)?;
        let scheduler_status = Self::g17p_sksm_port_read_locked(
            &sgx,
            T8140_KSM_SCHEDULER_STATUS,
            0,
            1u64 << 42,
        )?;
        // B1's host-channel drain scanner treats bits 31:16 of these four
        // consecutive words as producer counts. Its hang classifier also
        // tests them when the c128 active class is empty.
        let work_channel_status = [
            Self::g17p_sksm_port_read_locked(
                &sgx,
                T8140_KSM_WORK_CHANNEL_0,
                0,
                1u64 << 42,
            )?,
            Self::g17p_sksm_port_read_locked(
                &sgx,
                T8140_KSM_WORK_CHANNEL_1,
                0,
                1u64 << 42,
            )?,
            Self::g17p_sksm_port_read_locked(
                &sgx,
                T8140_KSM_WORK_CHANNEL_2,
                0,
                1u64 << 42,
            )?,
            Self::g17p_sksm_port_read_locked(
                &sgx,
                T8140_KSM_WORK_CHANNEL_3,
                0,
                1u64 << 42,
            )?,
        ];

        // Bits 25:24 of an admitted slot state encode pipe + 1. B1 uses this
        // exact conversion before its banked USC reads. A zero value means no
        // pipe has been assigned yet, so do not perturb either selector.
        //
        // Index the state bank by SLOT, not by QID. `0xc020` is the same
        // register `slot_state` reads with a slot index above, and measurement
        // settles which meaning is right: with the fragment stuck in KSM slot
        // 0, the read at index 0 returns 0x0100000d with progress naming qid 6,
        // while the reads at index 5 and index 6 -- the TA and 3D qids -- both
        // return zero. So a QID-indexed read of this bank reports "no pipe" for
        // work that demonstrably has one, which is why this snapshot was always
        // skipped for the one queue it exists to describe.
        //
        // Find the slot whose progress word names the fragment's qid. The qid
        // lives at bits 46:40 of the progress word, the same field the slot
        // dump decodes.
        // Prefer the fragment's slot. Fall back to any slot that has a pipe
        // assigned, so the COLD render is measurable too: with no compute in
        // the boot the tiler is the queue that occupies a slot and sticks,
        // and gating solely on the fragment's qid would skip the one case
        // PRIORITY 1 is about.
        let fragment_slot_state = slot_progress
            .iter()
            .position(|progress| ((progress >> 40) & 0x7f) == u64::from(fragment_qid))
            .map(|slot| slot_state[slot])
            .filter(|state| (state >> 24) & 0x3 != 0)
            .or_else(|| {
                slot_state
                    .iter()
                    .copied()
                    .find(|state| state & 1 != 0 && (state >> 24) & 0x3 != 0)
            })
            .unwrap_or(0);
        let usc = match ((fragment_slot_state >> 24) & 0x3).checked_sub(1) {
            Some(pipe) => {
                let pipe = pipe as u32;
                // B1's explicit USC dump writes these selectors but does not
                // restore them; leave the same firmware-defined final state.
                registers.try_write32(pipe, T8140_USC_SERV_SELECTOR)?;
                let vdm_serv = registers.try_read64(T8140_USC_VDM_SERV)?;
                let pdm_serv = registers.try_read64(T8140_USC_PDM_SERV)?;
                let cdm_serv = registers.try_read64(T8140_USC_CDM_SERV)?;
                registers.try_write32(pipe, T8140_USC_DEBUG_SELECTOR)?;
                let vdm_debug_status = registers.try_read64(T8140_USC_VDM_DEBUG_STATUS)?;
                let pdm_debug_status = registers.try_read64(T8140_USC_PDM_DEBUG_STATUS)?;
                let cdm_debug_status = registers.try_read64(T8140_USC_CDM_DEBUG_STATUS)?;
                let fragment_rce_targets = [
                    Self::g17p_sksm_port_read_locked(&sgx, 0x01738, pipe as u64, 0)?,
                    Self::g17p_sksm_port_read_locked(&sgx, 0x15020, pipe as u64, 0)?,
                    Self::g17p_sksm_port_read_locked(&sgx, 0x14080, pipe as u64, 0)?,
                    Self::g17p_sksm_port_read_locked(&sgx, 0x16068, pipe as u64, 0)?,
                    Self::g17p_sksm_port_read_locked(&sgx, 0x16460, pipe as u64, 0)?,
                    Self::g17p_sksm_port_read_locked(&sgx, 0x16090, pipe as u64, 0)?,
                    Self::g17p_sksm_port_read_locked(&sgx, 0x16098, pipe as u64, 0)?,
                    Self::g17p_sksm_port_read_locked(&sgx, 0x16428, pipe as u64, 0)?,
                ];
                Some(G17PUscEngineSnapshot {
                    pipe,
                    vdm_serv,
                    pdm_serv,
                    cdm_serv,
                    vdm_debug_status,
                    pdm_debug_status,
                    cdm_debug_status,
                    fragment_rce_targets,
                })
            }
            None => None,
        };

        Ok(Some(G17PRenderKsmAdmissionSnapshot {
            gpc_state,
            pdm_qid0_state,
            pdm_qid0_progress,
            ta_qid,
            ta_state,
            ta_progress,
            fragment_qid,
            fragment_state,
            fragment_progress,
            qid_counters,
            queue_enabled,
            queue_descriptor,
            slot_state,
            slot_progress,
            queue_enabled_mask,
            dm_control,
            dm_stopped,
            queue_valid_mask,
            queue_resume_mask,
            completed_by_qid,
            pipe_vdm_stream,
            pipe_work_stamp,
            current_command,
            current_context,
            active_queue_mask,
            active_queue_state,
            fault_masks,
            dm_masks,
            scheduling_pause_request,
            scheduling_pause_status,
            queue_stop_request,
            queue_stopped,
            resource_clear,
            resource_go,
            resource_callback_state,
            launch_slot,
            launch_mode,
            launch_strobe,
            launch_irq_enable,
            launch_irq_status,
            launch_slot_pending,
            launch_slot_ack,
            new_work_pending,
            new_work_ack,
            scheduler_status,
            work_channel_status,
            usc,
        }))
    }

    /// Sample the three KSM words that change as a submission progresses:
    /// the queue descriptor (bit 63 = hardware "queue active"), the
    /// scheduling-pause word, and the AddKicks count.
    pub(crate) fn g17p_sample_ksm(&self, qid: u64) -> Result<(u64, u64, u64)> {
        Ok((
            self.g17p_sksm_port_read(0x21008, qid)?,
            self.g17p_sksm_port_read(0x211d0, qid)?,
            self.g17p_sksm_port_read(0x21018, qid)?,
        ))
    }

    /// Read back what the firmware's tag-15 handler actually programmed for a
    /// queue: the descriptor (bit 63 = hardware "queue active") and the
    /// 128-bit queue-enabled mask.
    pub(crate) fn g17p_dump_queue_descriptor(&self, qid: u64) -> Result {
        let descriptor = self.g17p_sksm_port_read(0x21008, qid)?;
        let mask_lo = self.g17p_sksm_port_read(0x21038, 0)?;
        let mask_hi = self.g17p_sksm_port_read(0x21040, 0)?;
        let ring_base = (descriptor >> 5) & ((1u64 << 38) - 1);
        dev_info!(
            self.dev.as_ref(),
            "G17P QID {} descriptor={:#018x} active={} ring_base_bits42_5={:#x} mask={:#x}:{:#x}\n",
            qid,
            descriptor,
            descriptor >> 63,
            ring_base,
            mask_hi,
            mask_lo
        );
        // AddKicks, native's hang-detector progress metric, and the two words
        // tag 16's handler writes as -1 at the end of its "go" poke: if they
        // are not -1 that handler never reached its tail.
        for reg in [0x21010u64, 0x21020, 0x21028, 0x211c8, 0x211d0, 0x211b0] {
            match self.g17p_sksm_port_read(reg, qid) {
                Ok(value) => dev_info!(
                    self.dev.as_ref(),
                    "G17P QID {} port[{:#07x}] = {:#018x} (kicks={} ts={:#x})\n",
                    qid,
                    reg,
                    value,
                    (value >> 49) & 0x1ff,
                    value & ((1u64 << 40) - 1)
                ),
                Err(error) => dev_warn!(
                    self.dev.as_ref(),
                    "G17P QID {} port[{:#07x}] read failed ({:?})\n",
                    qid,
                    reg,
                    error
                ),
            }
        }
        // Establish whether these are per-QID or global: a register that is
        // global will read identically whatever qid the tag encodes.
        for reg in [0x211d0u64, 0x103a8, 0x21018, 0x21008] {
            let mut values = [0u64; 3];
            let mut ok = true;
            for (slot, probe_qid) in [0u64, 1, qid].iter().enumerate() {
                match self.g17p_sksm_port_read(reg, *probe_qid) {
                    Ok(value) => values[slot] = value,
                    Err(_) => ok = false,
                }
            }
            if ok {
                dev_info!(
                    self.dev.as_ref(),
                    "G17P port[{:#07x}] qid0={:#x} qid1={:#x} qid{}={:#x}\n",
                    reg, values[0], values[1], qid, values[2]
                );
            }
        }
        Ok(())
    }

    /// Read the KSM / SKSM register file while the GPU cores are powered.
    /// Historically every one of these SErrored, which is why the driver
    /// suppresses the SKSM port entirely; retest now that the precondition
    /// (GPC performance state non-zero) actually holds.
    pub(crate) fn g17p_probe_ksm_block(&self) -> Result {
        let sgx = self.sgx.try_access().ok_or(ENODEV)?;
        let res = sgx.relaxed();
        let (fault_info, fault_addr) = self.g17p_fault_registers();
        dev_info!(
            self.dev.as_ref(),
            "G17P MMU fault: latched={} info={:#018x} addr={:#018x} unit_code={:#04x} level={} read={} sideband={:#x} vm_slot={}\n",
            fault_info & 1,
            fault_info,
            fault_addr,
            (fault_info >> 9) & 0xff,
            (fault_info >> 24) & 0x3,
            (fault_info >> 23) & 1,
            (fault_info >> 1) & 0xff,
            (fault_info >> 17) & 0x3f
        );

        // 0xd8c0/0xd8c8 are the G14X+/AGX3 MMU fault-status pair upstream reads
        // (0x17030 is the G13/G14G offset our FAULT_INFO still carries). If the
        // firmware halted us for a GMMU fault these should name the address.
        for offset in [
            0xd8a0usize, 0xd8a8, 0xd8b0, 0xd8b8, 0xd8c0, 0xd8c8, 0xd8d0, 0xd8d8,
            0xd8e0, 0xd8e8, 0xd8f0, 0xd8f8,
        ] {
            let value = res.try_read64(offset)?;
            if value != 0 {
                dev_info!(
                    self.dev.as_ref(),
                    "G17P fault bank {:#07x} = {:#018x}\n",
                    offset,
                    value
                );
            }
        }
        Ok(())
    }

    /// Acknowledge a latched GMMU fault.
    ///
    /// The firmware unmasks GPU interrupt bit 35 inside its power-on routine
    /// without clearing the GMMU latch first, so a fault inherited from a
    /// previous OS is re-raised the moment the GPU powers up: recovery, KSM
    /// halt, re-unmask, repeat. The only fault-clearing write in the image is
    /// "select the requestor in `0xd800`, then store 1 to `0xd8e8`", taken from
    /// the firmware's own context-teardown path.
    pub(crate) fn g17p_ack_stale_fault(&self) -> Result {
        let sgx = self.sgx.try_access().ok_or(ENODEV)?;
        let res = sgx.relaxed();
        // Per-context fault mask. `agfaProcessChannelCommand` selects the
        // summary view (0xd800 = 0x100) and clears the context's bit in
        // 0xd8f0. vm_slot 2 is the context our submission runs in. This bank
        // is in the 0xd0xxxx control range, so unlike 0x1_0a18 it is reachable
        // before the GPU cores are powered -- which is the only point at which
        // we can get ahead of the firmware's power-on unmask.
        res.try_write64(0x100, 0xd800)?;
        let mask_before = res.try_read64(0xd8f0)?;
        let gate_summary = res.try_read64(0xd918)?;
        res.try_write64(mask_before & !(1u64 << 2), 0xd8f0)?;
        let mask_after = res.try_read64(0xd8f0)?;
        dev_info!(
            self.dev.as_ref(),
            "G17P fault ctx mask: summary-gate={:#x} 0xd8f0 {:#x} -> {:#x}\n",
            gate_summary, mask_before, mask_after
        );

        // The firmware invalidates the GMMU before acknowledging: command at
        // 0x1_8030, busy in 0x1_8038 bit 1.
        let inv_before = res.try_read64(0x1_8038)?;
        res.try_write64(0x007F_7377, 0x1_8030)?;
        let mut spins = 0u32;
        let mut busy = 0u64;
        while spins < 10_000 {
            busy = res.try_read64(0x1_8038)?;
            if busy & 2 == 0 {
                break;
            }
            spins += 1;
        }
        dev_info!(
            self.dev.as_ref(),
            "G17P gmmu invalidate: 0x18038 {:#x} -> {:#x} spins={}\n",
            inv_before, busy, spins
        );

        // Every access to this bank is 64-bit in both the firmware and the
        // kext (size=3 on every store). A 32-bit store to the 0xd800 selector
        // is dropped, which silently leaves the select unmoved.
        for requestor in 0u64..4 {
            res.try_write64(requestor, 0xd800)?;
            let before = res.try_read64(0xd8c0)?;
            let gate = res.try_read64(0xd918)?;
            res.try_write64(1, 0xd8e8)?;
            let after = res.try_read64(0xd8c0)?;
            dev_info!(
                self.dev.as_ref(),
                "G17P fault ack req {}: gate={:#x} {:#x} -> {:#x}\n",
                requestor, gate, before, after
            );
        }
        res.try_write64(0, 0xd800)?;
        // Bit 35 is level-driven and its sub-condition is never written back
        // (every other enabled bit reads a sub-status and writes it straight
        // back one register later; bit 35 reads 0x1_0a00 and writes nothing).
        // With the fault record exposed as a reset-default phantom, 0x1_0a00 is
        // the only register that names what actually holds bit 35 up.
        // NB: do not write 0x1_0a08 -- that is the enable register.
        let sub_status = res.try_read64(0x1_0a00)?;
        let irq_enable = res.try_read64(0x1_0a08)?;
        dev_info!(
            self.dev.as_ref(),
            "G17P irq bit35 sub-status 0x10a00 = {:#018x} enable 0x10a08 = {:#018x}\n",
            sub_status,
            irq_enable
        );
        let irq_status = res.try_read64(0x1_0a10)?;
        // 0x1_0a18 has exactly one writer in the firmware -- the interrupt
        // handler -- while 0x1_0a10 has one reader: the classic status/clear
        // pair. Try acknowledging bit 35 (GMMU page fault) there.
        res.try_write64(1u64 << 35, 0x1_0a18)?;
        let after_clear = res.try_read64(0x1_0a10)?;
        let fault_after = res.try_read64(0xd8c0)?;
        dev_info!(
            self.dev.as_ref(),
            "G17P GPU irq 0x10a10 {:#x} (bit35={}) -> after 0x10a18 ack {:#x} (bit35={}); fault {:#x}\n",
            irq_status,
            (irq_status >> 35) & 1,
            after_clear,
            (after_clear >> 35) & 1,
            fault_after
        );
        let (info, addr) = self.g17p_fault_registers();
        dev_info!(
            self.dev.as_ref(),
            "G17P fault ack: after clear latched={} info={:#x} addr={:#x}\n",
            info & 1,
            info,
            addr
        );
        Ok(())
    }

    /// Snapshot and log the MMU fault bank. Safe with the GPU cores
    /// unpowered: the fault registers live at `0xd8xx`, inside the `0xd0xxxx`
    /// control range that is reachable from the AP at all times -- unlike the
    /// `0x0..0x2ffff` core register file.
    pub(crate) fn g17p_log_fault_bank(&self, when: &'static str) -> Result {
        let (info, addr) = self.g17p_fault_registers();
        dev_info!(
            self.dev.as_ref(),
            "G17P fault bank @{}: latched={} info={:#018x} addr={:#018x} reason={} read={} level={} unit={:#04x} vm_slot={}\n",
            when,
            info & 1,
            info,
            addr,
            (info >> 1) & 7,
            (info >> 4) & 1,
            (info >> 7) & 3,
            (info >> 9) & 0xff,
            (info >> 17) & 0x3f
        );
        Ok(())
    }

    /// Sample the currently selected GMMU requestor and its authoritative GPU
    /// IRQ source without changing or acknowledging any hardware state.
    ///
    /// The low core register block is unsafe to read after power-off on J700,
    /// so report `None` when the always-on GPC state says the cores are gated.
    pub(crate) fn g17p_render_fault_irq_snapshot(
        &self,
    ) -> Result<Option<G17PFaultIrqSnapshot>> {
        let sgx = self.sgx.try_access().ok_or(ENODEV)?;
        let registers = sgx.relaxed();
        let gpc_state = registers.try_read32(T8140_HOST_IRQ_SUMMARY)? & 0xf;
        if gpc_state == 0 {
            return Ok(None);
        }
        Ok(Some(G17PFaultIrqSnapshot {
            gpc_state,
            fault_info: registers.try_read64(FAULT_INFO_G14X)?,
            fault_addr_word: registers.try_read64(FAULT_ADDR_G14X)?,
            requestor_gate: registers.try_read64(0xd918)?,
            sub_status: registers.try_read64(0x1_0a00)?,
            irq_status: registers.try_read64(0x1_0a10)?,
        }))
    }

    /// Read only B1's retained first-fault requestor. The caller must validate
    /// the current reason-3 report and state-1 pre-ACK epoch. FUN_5450 selects
    /// d800=0..3 until d8c0.valid, then leaves that requestor selected while
    /// waiting for the host. Do not change d800 or acknowledge at d8e8 here.
    pub(crate) fn g17p_halted_gmmu_snapshot(
        &self,
    ) -> Result<Option<G17PHaltedGmmuSnapshot>> {
        let sgx = self.sgx.try_access().ok_or(ENODEV)?;
        let registers = &*sgx;
        let gpc_state = registers.try_read32(T8140_HOST_IRQ_SUMMARY)? & 0xf;
        if gpc_state == 0 {
            return Ok(None);
        }
        let info_before = registers.try_read64(FAULT_INFO_G14X)?;
        let address_word = if info_before & 1 != 0 {
            Some(registers.try_read64(FAULT_ADDR_G14X)?)
        } else {
            None
        };
        let info_after = registers.try_read64(FAULT_INFO_G14X)?;
        Ok(Some(G17PHaltedGmmuSnapshot {
            gpc_state, info_before, address_word, info_after,
        }))
    }

    /// Snapshot every GMMU fault requestor while firmware is halted in the
    /// type-4 recovery state, without acknowledging or clearing any latch.
    ///
    /// This mirrors `AGXAcceleratorG16::getFirstMMUFaultStatus`: select each
    /// requestor with a 64-bit store at 0xd800, read 0xd8c0, and read 0xd8c8
    /// only when the valid bit is set.  The selector is restored to zero and
    /// 0xd8e8 (the destructive acknowledge register) is never touched.
    pub(crate) fn g17p_log_fault_requestors_pre_ack(&self, when: &'static str) -> Result {
        let sgx = self.sgx.try_access().ok_or(ENODEV)?;
        let registers = sgx.relaxed();
        let gpc_state = registers.try_read32(T8140_HOST_IRQ_SUMMARY)? & 0xf;
        if gpc_state == 0 {
            dev_warn!(
                self.dev.as_ref(),
                "G17P fault requestors @{}: skipped because gpc-state=0\n",
                when
            );
            return Ok(());
        }

        let mut sweep_error = None;
        for requestor in 0u64..4 {
            if let Err(error) = registers.try_write64(requestor, FAULT_REQUESTOR_G14X) {
                sweep_error = Some(error);
                break;
            }
            let info = match registers.try_read64(FAULT_INFO_G14X) {
                Ok(value) => value,
                Err(error) => {
                    sweep_error = Some(error);
                    break;
                }
            };
            let addr_word = if info & 1 != 0 {
                match registers.try_read64(FAULT_ADDR_G14X) {
                    Ok(value) => value,
                    Err(error) => {
                        sweep_error = Some(error);
                        break;
                    }
                }
            } else {
                0
            };
            let address = (addr_word & ((1u64 << 42) - 1)) << 6;
            dev_info!(
                self.dev.as_ref(),
                "G17P fault requestor @{} req={} gpc-state={:#x} latched={} info={:#018x} addr-word={:#018x} va={:#x} reason={} read={} level={} unit={:#04x} vm-slot={} sideband={:#x}\n",
                when,
                requestor,
                gpc_state,
                info & 1,
                info,
                addr_word,
                address,
                (info >> 1) & 7,
                (info >> 4) & 1,
                (info >> 7) & 3,
                (info >> 9) & 0xff,
                (info >> 17) & 0x3f,
                (info >> 23) & 0x7f,
            );
        }

        let restore = registers.try_write64(0, FAULT_REQUESTOR_G14X);
        if let Some(error) = sweep_error {
            return Err(error);
        }
        restore?;
        Ok(())
    }

    /// Validate the GFX power provider's AXI2AF handshake. It must complete
    /// before PMGR AUTO_ENABLE, including power-ons preceding DRM probe.
    /// Keep this legacy call site read-only: repairing the words here is late.
    pub(crate) fn validate_t8140_axi_transition(&self) -> Result {
        let sgx = self.sgx.try_access().ok_or(ENODEV)?;
        let res = sgx.relaxed();
        for (offset, observed) in [
            (T8140_AXI_TRANSITION_0, 0x0001_0001u32),
            (T8140_AXI_TRANSITION_1, 0x0004_13b1u32),
        ] {
            let val = res.try_read32(offset)?;
            dev_info!(
                self.dev.as_ref(),
                "T8140 AXI validation: offset={:#x} value={:#010x} provider_bit0={}\n",
                offset,
                val,
                val & 1,
            );
            if val & 1 == 0 {
                dev_err!(
                    self.dev.as_ref(),
                    "T8140 AXI transition word {:#x} lacks the power-provider handshake\n",
                    offset,
                );
                return Err(EIO);
            }
            if val != observed {
                dev_warn!(
                    self.dev.as_ref(),
                    "T8140 AXI transition word {:#x} reads {:#x} (J700 observed {:#x})\n",
                    offset,
                    val,
                    observed
                );
            }
        }
        Ok(())
    }

    /// T8140: enable dynamic Fender gating before initdata.
    ///
    /// Native preserves every unrelated bit and clears only bits 1 and 2
    /// (`value & 0xfffffff9`) after AXI2AF and before either role's initdata.
    pub(crate) fn t8140_pre_initdata_clear(&self) -> Result {
        let sgx = self.sgx.try_access().ok_or(ENODEV)?;
        let res = sgx.relaxed();
        let before = res.try_read32(T8140_PRE_INITDATA_CLEAR)?;
        res.try_write32(before & !0x6, T8140_PRE_INITDATA_CLEAR)?;
        let after = res.try_read32(T8140_PRE_INITDATA_CLEAR)?;
        dev_info!(
            self.dev.as_ref(),
            "T8140 Fender dynamic gating: {:#x} {:#x} -> {:#x}\n",
            T8140_PRE_INITDATA_CLEAR,
            before,
            after,
        );
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

        // This is intentionally sampled only in the existing GPU-ID probe
        // window. Those ID/core-mask reads already require the SGX control
        // aperture to be safely accessible; reading this register later from
        // render submission would race GPU core power-gating and can raise an
        // asynchronous SError on G17P. Cache the decoded nibble in GpuIdConfig
        // and carry it through ordinary host memory from here on.
        let (gpc_perf_state_map, gpc_perf_state_map_low, gpc_perf_state_control) = if identity
            .gpu_gen
            == hw::GpuGen::G17
            && identity.gpu_variant == identity::GpuVariant::P
        {
            let sgx = self.sgx.try_access().ok_or(ENODEV)?;
            let registers = sgx.relaxed();
            let control_raw = registers.try_read32(T8140_GPC_PERF_STATE_CONTROL)?;
            let map_raw = registers.try_read32(T8140_GPC_PERF_STATE_MAP)?;
            let map = t8140_gpc_perf_state_map(map_raw);
            let map_low = t8140_gpc_perf_state_map_low(map_raw);
            let control = t8140_gpc_perf_state_control(control_raw);
            dev_info!(
                self.dev.as_ref(),
                "G17P GPC performance-state map: control_raw={:#010x} map_raw={:#010x} control={} low={:#x} high={:#x}\n",
                control_raw,
                map_raw,
                control,
                map_low,
                map,
            );
            (map, map_low, control)
        } else {
            (0, 0, 0)
        };

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

    /// Get the fault information from the MMU status register, if one occurred.
    pub(crate) fn get_fault_info(&self, cfg: &'static hw::HwConfig) -> Option<FaultInfo> {
        // The G14X fault-register bank and fault-word decode carry unchanged
        // to every AGX3 part (see the FAULT_INFO_G14X provenance comment).
        // `gpu_core` is a firmware-ABI value that only exists for the AGX2
        // targets (None on AGX3), so AGX3 membership is keyed off the design
        // generation.
        let g14x = cfg
            .gpu_core
            .map(|core| core as u32 >= hw::GpuCore::G14S as u32)
            .unwrap_or(false)
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
