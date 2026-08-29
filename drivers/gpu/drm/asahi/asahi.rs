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
mod g17_asc_bringup;
mod g17_boot;
mod g17_firmware;
mod g17_initdata;
mod g17_power_preflight;
mod g17_rtkit;
mod g17_submission;
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

kernel::module_platform_driver! {
    type: driver::AsahiDriver,
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
            // permissions: 0o644,
            description: "Debug flags",
        },
        fault_control: u32 {
            default: 0xb,
            // permissions: 0,
            description: "Fault control (0x0: hard faults, 0xb: macOS default)",
        },
        initial_tvb_size: usize {
            default: 0x8,
            // permissions: 0o644,
            description: "Initial TVB size in blocks",
        },
        robust_isolation: u32 {
            default: 0,
            // permissions: 0o644,
            description: "Fully isolate GPU contexts (limits performance)",
        },
        g17p_test_stage: u32 {
            default: 1,
            // permissions: 0,
            description: "G17P bring-up stage (0: control, 1: QID4, 2: compute UAPI)",
        },
    },
}
