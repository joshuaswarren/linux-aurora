// SPDX-License-Identifier: GPL-2.0-only
//! M5 Pro GPU firmware and address-space configuration.
use crate::g17::{Config, FirmwareProfile, Identity};
use crate::hw::GpuVariant;
pub(crate) const CONFIG: Config = Config {
    chip_id: 0x6050,
    identity: Identity { family: 0xb, variant: 3, num_dies: 1,
        gpu_variant: GpuVariant::S, usc_generation: 3, hal_generation: 300 },
    uat_ias: 42, uat_oas: 42, firmware: FirmwareProfile::M5Sptm,
    crash_buffers: None, pm_metrics: None,
};
