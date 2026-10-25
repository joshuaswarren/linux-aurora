// SPDX-License-Identifier: GPL-2.0-only OR MIT
//! M5 Pro GPU firmware and address-space configuration.
use crate::g17::{Config, FirmwareProfile, FixedBuffer, Identity};
use crate::hw::GpuVariant;
pub(crate) const CONFIG: Config = Config {
    chip_id: 0x6050,
    identity: Identity { family: 0xb, variant: 3, num_dies: 1,
        gpu_variant: GpuVariant::S, usc_generation: 3, hal_generation: 300 },
    uat_ias: 42, uat_oas: 42, firmware: FirmwareProfile::M5Sptm,
    // The coprocessors request a crash log buffer at these addresses (primary first) when they start and hand
    // its contents back when they crash; outside the SPTM canonical range of dynamic objects.
    crash_buffers: Some([
        FixedBuffer { va: 0x100_025b_4000, size: 0x8000 },
        FixedBuffer { va: 0x100_0267_0000, size: 0x8000 },
    ]),
    pm_metrics: None,
};
