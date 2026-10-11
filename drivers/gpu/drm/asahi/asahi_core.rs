// SPDX-License-Identifier: GPL-2.0-only AND MIT
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
mod stats;
mod sysfs_exports;
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
    license: "GPL",
    params: {
        m5_platform_enable: u32 {
            default: 0,
            description: "Enable native M5 SLC allocation and static PMC voters at initialization",
        },
        m5_platform_max: u32 {
            default: 0,
            description: "Request M5 ADT maximum DCS and SOC floors once at GPU startup",
        },
        m5_cached_objects: u32 {
            default: 1,
            description: "Use retained native cached M5 command mappings; 0 disables for diagnostics",
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
        stats_export: u8 {
            default: 1,
            // permissions: 0,
            description: "Export AGX firmware stats to sysfs (1 = on, 0 = off)",
        },
    },
}
