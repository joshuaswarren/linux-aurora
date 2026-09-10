// SPDX-License-Identifier: GPL-2.0-only OR MIT

// Built and checked now; reached only after the separate M5 UAT gate opens.
#![allow(dead_code)]

//! Retained RAM for the J813 G17G/25G76 initialization graph.
//!
//! Construction only: no processor start, mailbox publication, work submission
//! or DRM registration. The eventual boot session must retain this entire
//! owner until BOTH firmware processors are stopped, including on failure.
//! M5 UAT lifecycle admission remains separate from this memory layout.

use super::MappedObject;
use crate::{driver::AsahiDevice, g17_initdata as init, mmu};
use core::sync::atomic::{fence, Ordering};
use kernel::prelude::*;
use kernel::sync::Arc;

const KERNEL_BASE: u64 = 0xffff_fc20_0000_0000;
const PRIVATE_BASE: u64 = KERNEL_BASE + 0x2_0000;
const ROOTS_BASE: u64 = KERNEL_BASE + 0x1a_8000;
const SHARED_BASE: u64 = KERNEL_BASE + 0xc078_8000;
const FWCTL_BASE: u64 = KERNEL_BASE + 0x1c_0000;
const SENTINEL_BASE: u64 = KERNEL_BASE + 0xc07e_0000;
const PBDESC_BASE: u64 = KERNEL_BASE + 0x15e_0000;
const UMA_BASE: u64 = KERNEL_BASE + 0x15e_8000;

/// Owning mappings are dropped before the UAT. Aliases retain the SAME GEM
/// backing; no firmware pointer refers to a copied snapshot or temporary map.
pub(crate) struct M5FirmwareMemory {
    primary_root: MappedObject,
    secondary_root: MappedObject,
    shared: MappedObject,
    private: MappedObject,
    region_a: MappedObject,
    region_c: MappedObject,
    fwctl: MappedObject,
    aon: MappedObject,
    qos: MappedObject,
    pbdesc: MappedObject,
    uma: MappedObject,
    completions: KVec<MappedObject>,
    aliases: KVec<mmu::KernelMapping>,
    registers: KVec<mmu::KernelMapping>,
    uat: Arc<mmu::Uat>,
}

impl M5FirmwareMemory {
    /// All fallible preparation happens before publication. The caller must
    /// pass a prepared M5 UAT while both processors await initdata. The
    /// caller retains its own Arc until both CPUs stop, including when this
    /// unpublished graph fails construction. There is no Neo tuning fallback.
    #[inline(never)]
    pub(crate) fn new_with_uat(
        dev: &AsahiDevice,
        uat: Arc<mmu::Uat>,
        boot_timestamp: u64,
    ) -> Result<Self> {
        if !uat.is_t8142_geometry() {
            return Err(ENODEV);
        }
        let mut registers = KVec::new();
        for window in init::M5_25G76_REGISTER_WINDOWS {
            let (physical, virtual_address, span) = window.mapped_pages().map_err(|_| EINVAL)?;
            let protection = match window.flag {
                0 => mmu::PROT_FW_MMIO_RO,
                2 => mmu::PROT_FW_MMIO_RW,
                _ => return Err(EINVAL),
            };
            // Page-table writes only. No register contents are accessed.
            registers.push(uat.kernel_vm().map_io(virtual_address, physical.try_into()?,
                span.try_into()?, protection)?, GFP_KERNEL)?;
        }
        let fixed = |address, size, protection| MappedObject::new_wc_at(
            dev, uat.kernel_vm(), address, size, protection);
        // Install every fixed address before allocator-selected objects.
        let mut private = fixed(PRIVATE_BASE, init::M5_25G76_PRIVATE_CLUSTER_SIZE, mmu::PROT_FW_SHARED_RW)?;
        // Separate mappings retain the empty page between the two roots.
        let mut primary_root = fixed(ROOTS_BASE, init::ROOT_SIZE_PRIMARY, mmu::PROT_FW_SHARED_RO)?;
        let mut secondary_root = fixed(ROOTS_BASE + init::SECONDARY_ROOT_DELTA,
            init::ROOT_SIZE_SECONDARY, mmu::PROT_FW_SHARED_RO)?;
        let mut shared = fixed(SHARED_BASE, init::NATIVE_SHARED_CLUSTER_SIZE, mmu::PROT_FW_PRIV_RW)?;
        let region_a = fixed(KERNEL_BASE + 0x1b_8000, init::REGION_A_SIZE, mmu::PROT_FW_SHARED_RO)?;
        let mut region_c = fixed(KERNEL_BASE + 0x1a_0000, init::REGION_C_SIZE, mmu::PROT_FW_SHARED_RO)?;
        let fwctl = fixed(FWCTL_BASE, 0x4000, mmu::PROT_FW_SHARED_RW)?;
        // These two tables already have zero-backed mappings at handoff.
        // Their populated runtime descriptors belong to a later stage.
        let mut pbdesc = fixed(PBDESC_BASE, 0x4000, mmu::PROT_FW_SHARED_RW)?;
        let mut uma = fixed(UMA_BASE, 0x4000, mmu::PROT_FW_SHARED_RW)?;
        let mut aliases = KVec::new();
        for (object, address) in [(&mut pbdesc, init::REGION_VIEW_LOW_ADDRS[0]),
                                  (&mut uma, init::REGION_VIEW_LOW_ADDRS[1])] {
            aliases.push(object.map_alias_at(uat.kernel_lower_vm(), address,
                mmu::PROT_GPU_SHARED_RW)?, GFP_KERNEL)?;
            if uat.kernel_vm().translate_iova(object.iova())?
                != uat.kernel_lower_vm().translate_iova(address)? {
                return Err(EFAULT);
            }
        }

        let mut completions = KVec::new();
        let mut completion_bindings = [init::G17PKsmCompletionAliases { low: 0, high: 0 }; 4];
        for (ordinal, binding) in completion_bindings.iter_mut().enumerate() {
            // The fixed alias pairs are measured on M5, including ordinals
            // 1 and 3 which are absent from the older Neo initialization.
            let low = 0x70_0184_8000 + ordinal as u64 * 0x2_8000;
            let high = KERNEL_BASE + 0xc07e_8000 + ordinal as u64 * 0x2_8000;
            let mut backing = MappedObject::new_wc_at(dev, uat.kernel_vm(), high,
                init::M5_25G76_COMPLETION_BACKING_SIZE, mmu::PROT_FW_PRIV_RW)?;
            aliases.push(backing.map_alias_at(uat.kernel_lower_vm(), low,
                mmu::PROT_GPU_SHARED_RW)?, GFP_KERNEL)?;
            for offset in (0..init::M5_25G76_COMPLETION_BACKING_SIZE).step_by(mmu::UAT_PGSZ) {
                if uat.kernel_vm().translate_iova(high + offset as u64)?
                    != uat.kernel_lower_vm().translate_iova(low + offset as u64)? {
                    return Err(EFAULT);
                }
            }
            *binding = init::G17PKsmCompletionAliases { low, high };
            completions.push(backing, GFP_KERNEL)?;
        }
        // Keep allocator-selected objects below the high fixed graph, and
        // above the PB/UMA tables. This cannot occupy the deferred blank
        // sentinel address or its neighboring guard page.
        let dynamic = KERNEL_BASE + 0x168_0000..KERNEL_BASE + 0x1000_0000;
        let qos = MappedObject::new_wc_in_range(dev, uat.kernel_vm(), dynamic.clone(),
            0xe40, mmu::UAT_PGSZ as u64,
            mmu::PROT_FW_SHARED_RW)?;
        let mut aon = MappedObject::new_wc_in_range(dev, uat.kernel_vm(), dynamic,
            init::M5_25G76_AON_PTD_SIZE,
            mmu::UAT_PGSZ as u64, mmu::PROT_FW_PRIV_RW)?;
        aon.with_bytes_mut(|out| init::encode_m5_25g76_aon_ptd(out).map_err(|_| EINVAL))?;
        region_c.with_bytes_mut(|out| init::encode_region_c_for_profile(
            init::RootProfile::J813G17G25G76, out).map_err(|_| EINVAL))?;
        private.with_bytes_mut(|out| init::encode_m5_25g76_private_cluster(
            FWCTL_BASE, out).map_err(|_| EINVAL))?;

        let addresses = init::SharedAddresses {
            region_a: region_a.iova(), region_c: region_c.iova(),
            hw_data_bundle: SHARED_BASE, hw_data_bundle_alloc: init::NATIVE_SHARED_CLUSTER_SIZE,
        };
        let primary = init::InstanceAddresses {
            root: ROOTS_BASE, main_config: SHARED_BASE + init::NATIVE_PRIMARY_MAIN_BUNDLE_OFFSET as u64,
            status_a: PRIVATE_BASE + 0xee40, status_b: PRIVATE_BASE + 0x340,
            secondary_extras: [0, 0],
        };
        let secondary = init::InstanceAddresses {
            root: ROOTS_BASE + init::SECONDARY_ROOT_DELTA,
            main_config: SHARED_BASE + init::NATIVE_SECONDARY_MAIN_BUNDLE_OFFSET as u64,
            status_a: PRIVATE_BASE + 0xc3100, status_b: 0,
            secondary_extras: [primary.status_b + init::PRIMARY_STATUS_B_CONFIG_OFFSET as u64,
                               PRIVATE_BASE + 0x177400],
        };
        let views = init::PrimaryRegionViews {
            sentinel_high: SENTINEL_BASE, alias_high: [PBDESC_BASE, UMA_BASE],
        };
        shared.with_bytes_mut(|out| {
            init::encode_m5_25g76_hw_data(completion_bindings, qos.iova(),
                &mut out[..init::M5_25G76_HW_DATA_SIZE]).map_err(|_| EINVAL)?;
            init::encode_m5_25g76_bundle_policy(init::M5PolicyBindings {
                state: PRIVATE_BASE + 0x177480, aon_ptd: aon.iova(), boot_timestamp,
            }, out).map_err(|_| EINVAL)?;
            for (role, instance, grid, views) in [
                (init::InstanceRole::Primary, &primary, PRIVATE_BASE, Some(&views)),
                (init::InstanceRole::Secondary, &secondary, PRIVATE_BASE + 0x1770c0, None),
            ] {
                let channels = init::derive_channel_table(role, grid, instance.status_a,
                    instance.main_config, SHARED_BASE);
                let offset = (instance.main_config - SHARED_BASE) as usize;
                init::encode_main_config_for_profile(init::RootProfile::J813G17G25G76,
                    role, &addresses, instance, &channels, views,
                    &mut out[offset..offset + init::MAIN_CONFIG_SIZE]).map_err(|_| EINVAL)?;
            }
            Ok(())
        })?;
        primary_root.with_bytes_mut(|out| {
            init::encode_root_for_profile(init::RootProfile::J813G17G25G76,
                init::InstanceRole::Primary, &addresses, &primary,
                out).map_err(|_| EINVAL)
        })?;
        secondary_root.with_bytes_mut(|out| {
            init::encode_root_for_profile(init::RootProfile::J813G17G25G76,
                init::InstanceRole::Secondary, &addresses, &secondary,
                out).map_err(|_| EINVAL)
        })?;
        // Unlike PB/UMA, this future blank sentinel is unmapped at native
        // handoff. It becomes zero-backed later in macOS initialization.
        if uat.kernel_vm().translate_iova(SENTINEL_BASE).is_ok() {
            return Err(EINVAL);
        }
        // Writes to all CPU WC mappings precede any future handoff. Only
        // the two measured control openings are present; work rings and
        // later compute/render descriptors remain unsubmitted.
        fence(Ordering::SeqCst);
        Ok(Self { primary_root, secondary_root, shared, private, region_a, region_c, fwctl, aon, qos,
                  pbdesc, uma, completions, aliases, registers, uat })
    }

    pub(crate) fn root(&self, secondary: bool) -> u64 {
        if secondary { self.secondary_root.iova() } else { self.primary_root.iova() }
    }
}
