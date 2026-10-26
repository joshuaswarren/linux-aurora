// SPDX-License-Identifier: GPL-2.0-only OR MIT
#![recursion_limit = "2048"]

//! Driver for the Apple AGX GPUs found in Apple Silicon SoCs.

mod alloc;
mod buffer;
mod channel;
mod cleanup;
#[cfg(CONFIG_DEV_COREDUMP)]
mod crashdump;
mod debug;
mod driver;
mod event;
mod file;
mod float;
mod fw;
mod gem;
mod g17;
mod gpu;
mod hw;
mod initdata;
mod mem;
mod microseq;
mod mmu;
mod object;
mod pgtable;
mod queue;
mod regs;
mod slotalloc;
mod util;
#[cfg(CONFIG_DRM_ASAHI_MAPLE_TREE)]
mod vm;
mod workqueue;

use kernel::prelude::*;

#[pin_data]
struct DriverModule {
    // Field drop order unregisters/joins driver producers before queue teardown.
    #[pin]
    _driver: kernel::driver::Registration<kernel::platform::Adapter<driver::AsahiDriver>>,
    _cleanup: cleanup::QueueOwner,
}

impl kernel::InPlaceModule for DriverModule {
    fn init(module: &'static kernel::ThisModule) -> impl PinInit<Self, Error> {
        try_pin_init!(Self {
            // Initialize the queue first, before registration can invoke probe.
            _cleanup: cleanup::QueueOwner::new()?,
            _driver <- kernel::driver::Registration::new(
                <Self as kernel::ModuleMetadata>::NAME,
                module,
            ),
        })
    }
}

module! {
    type: DriverModule,
    name: "asahi",
    description: "AGX GPU driver for Apple silicon SoCs",
    license: "Dual MIT/GPL",
    params: {
        m5_kick_notify: u32 {
            default: 0,
            description: "Diagnostic: 1 = M5 compute kicks notify the firmware only on queue activation",
        },
        m5_cross_queue_fw_wait: u32 {
            default: 2,
            description: "Same-VM compute waits on other queues: 2 = firmware dependencies (default), 0 = host waits",
        },
        m5_matrix_submit_test: u32 {
            default: 0,
            description: "Diagnostic native M5 matrix submission controls; default0 unchanged,1 enabled",
        },
        m5_platform_enable: u32 {
            default: 0,
            description: "Enable native M5 SLC allocation and static PMC voters at initialization",
        },
        m5_platform_max: u32 {
            default: 0,
            description: "Request M5 ADT maximum DCS and SOC floors once at GPU startup",
        },
        m5_platform_probe: u32 {
            default: 0,
            description: "Read M5 cache and PMC state once during GPU initialization",
        },
        m5_cached_objects: u32 {
            default: 1,
            description: "Use retained native cached M5 command mappings; 0 disables for diagnostics",
        },
        m5_vm_drain_trace: u32 {
            default: 0,
            description: "Diagnostic only: bounded slow VM cleanup/admission timing; default off",
        },
        m5_compute_timeout_trace: u32 {
            default: 0,
            description: "Diagnostic only: one exact compute timeout snapshot; default off",
        },
        m5_map_stats: u32 {
            default: 0,
            description: "Diagnostic only: log batched SPTM kernel mappings of at least 64 pages (chunks, pages, provider operations, ns); default off",
        },
        sptm_trace: u32 {
            default: 0,
            description: "Diagnostic SPTM batch counts and duration; disabled by default",
        },
        coverage_cache: u32 {
            default: 1,
            description: "Use revocation-tracked submission coverage proofs; 0 is a diagnostic full-walk control",
        },
        coverage_check: u32 {
            default: 0,
            description: "Diagnostic only: recheck one cached proof per N requests against the page table; 0 disables",
        },
        coverage_stats: u32 {
            default: 0,
            description: "Diagnostic only: log submission coverage hit, walk and mismatch counts; default off",
        },
        sptm_space_trace: u32 {
            default: 0,
            description: "Diagnostic: log SPTM private address-space create/destroy/retention with live counts; disabled by default",
        },
        m5_ta_policy_test: u32 {
            default: 0,
            description: "Diagnostic only: force native image-copy TA field0x89a; default off",
        },
        m5_mesh_ta_word_938: u32 {
            default: 1,
            description: "TA command word +0x938: 1 (default) = native class values (0xc0c0 vertex, 0x10ac0 mesh, 0x12b40 task->mesh); 0 = legacy zero (diagnostic only); other = raw value for every TA command",
        },
        m5_mesh_storage_chunks: u32 {
            default: 0x200,
            description: "M5 mesh rendering buffer bound to API task (object-stage) render kicks only, in 64 KiB chunks (native TA register 0x1ca48 control 0x200); 0 disables the binding and the task feature",
        },
        m5_thermal_governor: u32 {
            default: 0,
            description: "M5 thermal flags: bit 0 enables the host GPU power budget controller, bit 1 logs one-second sensor and limiter samples, bit 2 lets the controller lower the CPU frequency ceiling while the GPU is budget-bound, bit 3 runs the fans at their maximum while it governs; default 0 preserves the existing policy",
        },
        m5_thermal_target_mc: u32 {
            default: 90000,
            description: "M5 GPU power budget governor target for the SoC hotspot temperature computed by the SMC, in milli-degrees C (70000..104000)",
        },
        m5_render_lend_test: u32 {
            default: 0,
            description: "Diagnostic only (vmfix6b qualification): lend idle live owners' render pairs before fresh or released pairs, even when fresh pairs can be built; default off",
        },
        m5_regionc_native: u32 {
            default: 0,
            description: "Diagnostic only (long-VS investigation): set the M5 region C words that differ from native M5 macOS (94 identical captures, native-window 2026-10-12; all other words already match). Bit 0: 0x990 = 0x06000840, 0x994 = 2, 0x998 = 0x30d40 (ours 0). Bit 1: 0xa0 = 0x3200000d, 0xa4 = 0x31000002 (ours 0). Default off",
        },
        m5_render_rebind_legacy: u32 {
            default: 0,
            description: "Diagnostic only (vmfix6f attribution): restore one pre-6f render rebind behaviour. Bit 0: a same-context pool rebind sets the queue configuration's context/QoS update bits (native: clear on same-owner free-list/PB switches). Bit 1: it rewrites both queue records (read-modify-write including firmware-owned words) although their host words are unchanged. Bit 2: another logical context of the same VM retargets the graph's queue records in place instead of getting its own graph. Bit 3: after any rebind every publication repeats the update bits until the first one retires (native: only a new owner's first configuration). Default 0",
        },
        m5_render_collect_poison: u32 {
            default: 0,
            description: "Diagnostic only (vmfix6e qualification): keep eagerly collected render graphs out of reuse, filled with a poison pattern, their client pool mappings removed and their PB descriptor pointed at an unmapped address, so a late firmware reference faults; default off",
        },
        m5_depth_msaa_compression: u32 {
            default: 1,
            description: "Enable single-layer D32 compressed 2x/4x ZLS admission",
        },
        m5_render_submit_test: u32 {
            default: 0,
            description: "Enable bounded single-group M5 render submission qualification",
        },
        m5_drm_test: u32 {
            default: 0,
            description: "M5 DRM qualification: 1=memory/queues, 2=non-scratch compute submission",
        },
        m5_render_test: u32 {
            default: 0,
            description: "M5 render qualification: 1=queues, 2=draw input, 3=draw after pool growth",
        },
        m5_render_iterations: u32 {
            default: 1,
            description: "Number of exact draws in explicit M5 render qualification",
        },
        m5_compute_test: u32 {
            default: 0,
            description: "Run explicit M5 arithmetic qualification after firmware startup",
        },
        m5_firmware_test: u32 {
            default: 0,
            description: "Start M5 firmware through the shared G17 runtime; reboot to repeat",
        },
        m5_sptm_selftest: u32 {
            default: 0,
            description: "Test M5 SPTM VM/context lifetime without starting GPU firmware",
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
    },
}
