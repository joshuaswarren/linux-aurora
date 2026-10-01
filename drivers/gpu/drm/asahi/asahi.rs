// SPDX-License-Identifier: GPL-2.0-only OR MIT
#![recursion_limit = "2048"]

//! Driver for the Apple AGX GPUs found in Apple Silicon SoCs.

mod agx_power_recovery;
mod alloc;
mod apple_gpu_topology;
mod buffer;
mod channel;
#[cfg(CONFIG_DEV_COREDUMP)]
mod crashdump;
mod debug;
mod driver;
mod drm_gpu;
mod event;
mod file;
mod float;
mod fw;
mod g15_boot;
mod g15_initdata;
mod g17_adt_j700;
mod g17_asc_bringup;
mod g17_boot;
mod g17_completion;
mod g17_config;
mod g17_compute;
mod g17_drm;
mod g17_dvfs_parity;
mod g17_gpu_power;
mod g17_dvfs_policy;
mod g17_firmware;
mod g17_initdata;
mod g17_job;
mod g17_status;
mod g17_queue_limits;
mod g17_work_state;
mod g17_channel;
mod g17_lifecycle;
mod g17_preparation;
mod g17_live_boot;
mod g17_manager;
mod g17_perf;
mod g17_timing_buffer;
mod g17_e2e;
mod g17_host_path;
mod g17_recovery_path;
mod g17_mem_path;
mod g17_store;
mod g17_power_preflight;
mod g17_render;
mod g17_resources;
mod g17_rtkit;
mod g17_submission;
mod g17_trace_capture;
mod g17_uapi;
mod gem;
mod gpu;
mod hw;
mod identity;
mod initdata;
mod mem;
mod m5_channel;
mod m5_compute;
mod m5_compute_program;
mod m5_graphics_bundle;
mod m5_render;
mod m5_completion;
mod m5_sksm;
mod m5_vm;
mod m5_render_target;
mod microseq;
mod mmu;
mod object;
mod pgtable;
mod queue;
mod regs;
mod slotalloc;
mod uat;
mod util;
#[cfg(CONFIG_DRM_ASAHI_MAPLE_TREE)]
mod vm;
mod workqueue;

#[kernel::prelude::pin_data]
struct DriverModule {
    #[pin]
    _driver: kernel::driver::Registration<kernel::platform::Adapter<driver::AsahiDriver>>,
}

impl kernel::InPlaceModule for DriverModule {
    fn init(module: &'static kernel::ThisModule)
        -> impl ::pin_init::PinInit<Self, kernel::error::Error> {
        // SAFETY: called once before registration can probe any device.
        unsafe { g17_resources::g17p_fw_pool_init() };
        kernel::try_pin_init!(Self {
            _driver <- kernel::driver::Registration::new(
                <Self as kernel::ModuleMetadata>::NAME, module,
            ),
        })
    }
}

kernel::prelude::module! {
    type: DriverModule,
    name: "asahi",
    description: "AGX GPU driver for Apple silicon SoCs",
    license: "Dual MIT/GPL",
    params: {
        m5_fw_trace: u32 {
            default: 0,
            description: "M5 primary trace classes: power bit 0, submission bit 1, recovery classifier bit 9",
        },
        m5_initdata_test: u32 {
            default: 0,
            description: "M5 fresh RAM boot: 1=initdata ACKs, 2=openings, 3=queue, 4=scheduler, 5=context, 6=one compute, 7=setup without kick, 8=request stop after admission, 9=live status, 10=live firmware monitors, 11=compute triangle, 12=TA/fragment triangle; 1..12 request stop; 13=retained session, 14=experimental DRM first-job adapter",
        },
        g18_initdata_test: u32 {
            default: 0,
            description: "T8152/J873 G18G bring-up stage. 0 refuses before MMIO/DMA. 1=read-only powered-resource probe. 2=RTKit. 3=initdata ACKs. 4=openings, idle policy, 60 s observation. 5=empty queue. 6=queue plus scheduler binding. 7=owned empty UMA pool binding. 8=compute diagnostic. 9=H1 render replay. 10=firmware plus application DRM render node. Stages 5/6/7 then observe without GPU jobs. Firmware stages require prior power, loader receipt and apple_mailbox.ascwrap_v8_cpu_control=1; all owners remain retained until reboot. 90=SGX power-on; 92=GFX1 power-on; 91=GFX power-on; 93=idle requests. Power stages are never implied. Mutating stages require user-authorized bring-up. Other values refused",
        },
        g18_compute_variant: u32 {
            default: 0,
            description: "Stage 8 compute diagnostic. 0=captured compute-add. 1=empty stream: captured barrier and terminate replace the launch; all output must remain poisoned. 2=64x64 compute triangle using a separately verified private shader patch; no TA/3D or Mesa. Variants 1 and 2 require g18_initdata_test=8. Other values are refused",
        },
        g18_compute_owner_test: u32 {
            default: 0,
            description: "J873 allocation-only E2E before firmware publication: 1 verifies shared-compute backing, guarded directory growth, allocator rollback and process aliases. Never submits GPU work. Other nonzero values refused",
        },
        g18_render_variant: u32 {
            default: 0,
            description: "Stage 9 H1 diagnostic: 0=colour, 2=terminated VDM plus background clear. Other variants refused before MMIO. One asynchronous worker pass, captured/M5 H1 policy, all owners retained; see G18-RENDER-RUNTIME.md",
        },
        g18_render_profile: u32 {
            default: 1,
            description: "Stage 9 G18GFX01 v2 profile: 1=colour-a, 2=colour-b, 4=fragment-witness (colour only). Gears profile3 is offline only and refused by H1. Stage0 remains the default",
        },
        g18_admit_compute: u32 {
            default: 0,
            description: "G18 DRM adapter: 1 admits single-dispatch compute commands (K1). 0 refuses them",
        },
        g18_admit_msaa4: u32 {
            default: 0,
            description: "G18 DRM adapter (g18_initdata_test=10 only): 1 admits exactly the native 4x MSAA render (4 samples, 32x16 utiles, sample positions 0xeaa26e26, 8-byte samples, one layer, no depth/stencil) with its native 4x command fields; Mesa needs AGX_G18_MSAA4=1 to submit it. 0 refuses every multisampled render (default)",
        },
        g18_usc_statistics: u32 {
            default: 0,
            description: "Stage 9 colour fixture only: 1 enables owned TA/3D SMAPP statistics pages. Raw memory statistics, not shader invocation counts. Default 0",
        },
        g18_sma_minimum: u32 {
            default: 0,
            description: "Stage9/profile4 or stage10/profile1, variant0: 1 probes fixed native minimum TA/fragment resource words with unchanged owned backing. Default 0",
        },
        g18_graphics_stall_trace: u32 {
            default: 0,
            description: "Stage9/profile4 or stage10/profile1, variant0: 1 enables firmware category9 diagnostics and the native TA mode2 progress sampler. No timeout changes. Default 0",
        },
        g18_vdm_high_alias: u32 {
            default: 0,
            description: "Stage9/profile4/variant0 with minimum SMA and stall tracing: 1 fetches unchanged native VDM bytes through the Gallium-address alias. Diagnostic only. Default 0",
        },
        g18_ppp_high_alias: u32 {
            default: 0,
            description: "With the stage9 high-VDM control: 1 aliases unchanged native PPP pages into the Gallium range and relocates only their VDM pointers. Diagnostic only. Default 0",
        },
        g18_sma_ideal: u32 {
            default: 0,
            description: "Stage9 variant0 profile4 only: 1 probes native ideal graphics resource policy with 40MiB backing and precommitted directory growth. Default 0",
        },
        g18_native_barrier: u32 {
            default: 0,
            description: "Stage9 variant0 profile4 with ideal SMA only: 1 probes the native same-group TA-to-3D barrier kind3. Default 0",
        },
        m5_graphics_frames: u32 {
            default: 1,
            description: "Number of sequential mode12 renders on a fresh RAM boot (1..64); reset pixels between completed frames",
        },
        m5_graphics_context: u32 {
            default: 3,
            description: "Application root slot for the mode12 regression (3..63); ordinary application queues allocate their own slots",
        },
        debug_flags: u64 {
            default: 0,
            description: "Debug flags",
        },
        fault_control: u32 {
            default: 0xb,
            description: "Fault control (0x0: hard faults, 0xb: macOS default)",
        },
        initial_tvb_size: usize {
            default: 0x8,
            description: "Initial TVB size in blocks",
        },
        robust_isolation: u32 {
            default: 0,
            description: "Fully isolate GPU contexts (limits performance)",
        },
        g17p_simplefb_iova: u64 {
            default: 0,
            description: "Diagnostic simplefb GPU address override; 0 disables it",
        },
        g17p_test_stage: u32 {
            default: 2,
            description: "G17P bring-up stage (0: control, 1: QID4, 2: full driver path)",
        },
        g17p_max_recoveries: u32 {
            default: 0,
            description: "Maximum firmware recoveries per session; 0 is unlimited",
        },
        g17p_fw_log: u32 {
            default: 0,
            description: "Enable the GPU firmware log ring",
        },
        g17p_fw_trace: u32 {
            default: 0,
            description: "Firmware trace class mask; 0 disables capture",
        },
        g17p_native_ring_drain: u32 {
            default: 1,
            description: "Native drainFirmwareRings parity: every event-worker turn consumes both roles' trace (0x200) and statistics (0x100) rings entry by entry; 0 restores the diagnostic-gated consumers (A/B only)",
        },
        g17p_binary_capture: u32 {
            default: 0,
            description: "Export first-render binary capture through devcoredump; allocates 64 MiB; bit 1 also reads the boot firmware code window",
        },
        g17p_fw_ktrace_verbose: u32 {
            default: 0,
            description: "Print all captured KTrace records instead of the bounded selection",
        },
        g17p_fw_ktrace_budget: u32 {
            default: 32,
            description: "Maximum ordinary KTrace records printed per drain",
        },
        g17p_fw_trace_classes: u32 {
            default: 0,
            description: "v93r diagnostic: KTrace class-enable bits (bit n = class n, 0..15) OR-ed into the firmware class bytes at 0x17780c/0x17780d, checked at the first render and every 256th after (re-armed if cleared); 0 leaves them untouched. Class 0 submission/launch, 1 monitor sample, 9 monitor state/recovery scan. Needs g17p_fw_trace != 0 for the ring",
        },
        g17p_fastdie_target_cc: u32 {
            default: 0,
            description: "Diagnostic: GPU firmware fast-die target in 0.01 C, lower-only 8500..=11000; 0 keeps the native 11000 (110.00 C)",
        },
        // NOTE: every `permissions:` line in this block is commented out, so
        // the macro gives each parameter permissions 0. That means NONE of
        // these appear under /sys/module/asahi/parameters -- they can only be
        // set on the insmod command line. Looking for one in sysfs and not
        // finding it does not mean the build lacks it; check
        // `strings asahi.ko | grep parmtype=` instead. This cost a hardware
        // cycle once already.
        g17p_vm_probe_va: u64 {
            default: 0,
            description: "Diagnostic GPU virtual address to inspect; 0 disables the probe",
        },
        g17p_vdm_dump: u32 {
            default: 0,
            description: "Dump submitted VDM command bytes",
        },
        g17p_fault_report: u32 {
            default: 0,
            description: "Fault diagnostics: bit 0 includes submission dumps; bit 1 enables recovery-only register snapshots; bit 2 captures fault-only B1 scheduler/FList DRAM; bit 3 enables post-timeout power-held scoreboard diagnostic",
        },
        g17p_render_dump_descriptor: u32 {
            default: 0,
            description: "Dump render descriptor bytes",
        },
        g17p_mcache_dsid_probe: u32 {
            default: 0,
            description: "Diagnostic: allocate non-reused native-range render MCache IDs per logical owner; reboot to reset pool",
        },
        g17p_ta_previous_fragment_probe: u32 {
            default: 2,
            description: "Diagnostic: 1 ordinal TA flag; 2 coherent idle-gated TA/FRG scratch; 0 preserves current policy",
        },
        g17p_render_single_pair: u32 {
            default: 0,
            description: "Diagnostic: 1 keeps each render queue on one physical TA/3D pair (no sibling pair); 0 preserves the two-pair policy",
        },
        g17p_frg_hwdsid_clear: u32 {
            default: 0,
            description: "Diagnostic: 1 writes FRG descriptor +0x222d as 0 (native light captures); 0 preserves 1",
        },
        g17p_disable_idle_core_off: u32 {
            default: 0,
            description: "Diagnostic: 1 writes RegionC+0x30 (idle core power-off enable) as 0 at init; 0 keeps the encoded 1",
        },
        g17p_timer_cswitch_mask: u32 {
            default: 2,
            description: "RegionC+0x0 TimerCSwitch per-DM resume mask (bit0 TA, bit1 FRG, bit2 CL); 0 disables",
        },
        g17p_fender_delay: u32 {
            default: 0,
            description: "Diagnostic: RegionC+0x9bc idle-off (fender) delay override; 0 keeps the native 40",
        },
        g17p_cswitch_timer_multiplier: u32 {
            default: 16,
            description: "RegionC+0x6a CSwitchTimerMultiplier: 1 ms engine walks per TimerCSwitch resume; 0 keeps the encoded 1",
        },
        g17p_dump_state: u32 {
            default: 0,
            description: "Render state dump size in KiB; 0 disables it",
        },
        g17p_dump_hwdata: u32 {
            default: 0,
            description: "Dump firmware hardware-data objects",
        },
        g17p_perf_window: u32 {
            default: 0,
            description: "Submissions per throughput report; 0 disables accounting",
        },
        g17p_mixed_resource_probe: u32 {
            default: 0,
            description: "Read-only mixed-resource census: 0=off, 1=compute fault only, 2=also compute ordinals 0/1 and every 16; both render owners, FList/UMA and HBIDs; no execution gating",
        },
        g17p_split_execution_id_probe: u32 {
            default: 0,
            description: "Diagnostic only: separate compute context ID for the same VM/root, retaining logical scheduler/QoS/PID; costs one extra context slot per compute owner; no execution gating",
        },
        g17p_render_poll_us: u32 {
            default: 0,
            description: "Completion poll interval in microseconds; 0 selects adaptive backoff",
        },
        g17p_render_wait_ms: u32 {
            default: 2000,
            description: "Render completion timeout in milliseconds",
        },
        g17p_verbose: u32 {
            default: 0,
            description: "Enable per-submission diagnostic logs",
        },
        g17p_recovery_snapshot: u32 {
            default: 0,
            description: "Capture halted firmware recovery state before acknowledgement",
        },
        // 2026-09-01 follow-up: descriptor +0x789=0x10 was also hardware-null
        // (same fault-free TA timeout and recovery as the 0x08 baseline).
        g17p_trace_depth: u32 {
            default: 0,
            description: "Trace the first N compute submissions per queue; 0 disables success-path tracing",
        },
        g17p_compute_usage_slot: u32 {
            default: 0,
            description: "Enable per-kick compute scratch-usage accounting",
        },
        g17p_gem_huge: u32 {
            default: 0,
            description: "v94 boot-only: back GEM objects with a huge tmpfs mount (0 off, 1 within_size, 2 always)",
        },
        g17p_completion_stamp_us: u32 {
            default: 500,
            description: "Maximum completion-stamp visibility wait in microseconds",
        },
        g17p_completion_trace: u32 {
            default: 0,
            description: "Completion logging level: 0 off, 1 summary, 2 detailed",
        },
    },
}
