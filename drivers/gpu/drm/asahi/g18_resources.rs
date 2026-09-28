// SPDX-License-Identifier: GPL-2.0-only OR MIT

#![allow(dead_code)]

//! Retained RAM for the J873 / T8152 / G18G / 26A428 initialization graph
//! (plan M1, steps 1.1-1.7).
//!
//! Construction only. Nothing here starts a processor, publishes on a
//! mailbox, reads or writes MMIO, requests PMGR/PSD power, touches the ASC
//! wrappers or the post-office bank, or submits work: every write lands in
//! host-allocated GEM memory or in UAT page tables. Register windows are
//! mapped for the firmware (page-table writes only); their contents are never
//! accessed from the host.
//!
//! The G18 live-boot session owns this object. It must build the UAT with
//! `mmu::Uat::new_t8152` (plan step 0.6) before calling
//! [`G18FirmwareMemory::new_with_uat`], and must retain both the UAT and this
//! owner until BOTH firmware processors are stopped, including on failure.
//!
//! Derived from `m5_resources.rs` with the M2/M3 compute and graphics owners
//! removed (they depend on the unported HAL400 submission ABI). The firmware
//! VA plan is the J813 one: the addresses are host-chosen and both shared
//! allocations keep their 25G76 page totals (0x28000 / 0x178000) in 26A428.
//! Every object inside the allocations follows the 26A428 kext layout from
//! `g17_initdata` (`G18G_26A428_*`). Values that are not proven for 26A428
//! are listed in `g17_initdata::G18G_26A428_UNPROVEN`.
//!
//! Integration (outside this file): include it from `g17_resources.rs` next
//! to the M5 module, so it can use the private `MappedObject`:
//!
//! ```ignore
//! #[cfg(not(test))]
//! #[path = "g18_resources.rs"]
//! pub(crate) mod g18;
//! ```

use super::MappedObject;
use super::g18_pool::{G18UmaPoolBinding, G18UmaPoolMemory};
use crate::{driver::AsahiDevice, g17_initdata as init, mmu};
use core::sync::atomic::{fence, Ordering};
use kernel::prelude::*;
use kernel::sync::Arc;

const PROFILE: init::RootProfile = init::RootProfile::J873G18G26A428;

const KERNEL_BASE: u64 = 0xffff_fc20_0000_0000;
/// Shared allocation #2 (the private cluster, 0x178000 bytes).
const PRIVATE_BASE: u64 = KERNEL_BASE + 0x2_0000;
const REGION_C_BASE: u64 = KERNEL_BASE + 0x1a_0000;
/// The secondary root sits [`init::SECONDARY_ROOT_DELTA`] above the primary.
const ROOTS_BASE: u64 = KERNEL_BASE + 0x1a_8000;
const REGION_A_BASE: u64 = KERNEL_BASE + 0x1b_8000;
/// ArmFirmware fwctl allocation: 0x20-byte state, then the ring at +0x40.
const FWCTL_BASE: u64 = KERNEL_BASE + 0x1c_0000;
const FWCTL_SIZE: usize = 0x4000;
/// Shared allocation #1 (hw_data, policy blocks, rings, main configs).
const SHARED_BASE: u64 = KERNEL_BASE + 0xc078_8000;
/// Main `+0x2d0` blank sentinel; unmapped at handoff, as on J813.
const SENTINEL_BASE: u64 = KERNEL_BASE + 0xc07e_0000;
/// PB descriptor and UMA pool tables, aliased at [`init::REGION_VIEW_LOW_ADDRS`].
const PBDESC_BASE: u64 = KERNEL_BASE + 0x15e_0000;
const UMA_BASE: u64 = KERNEL_BASE + 0x15e_8000;
const PBDESC_TABLE_SIZE: usize = 0x4000;
/// 26A428 createHardwareMappings at 0xfffffe0008c41978 requests 0x8000
/// bytes: 256 native 0x80-byte UMA descriptors, rather than the M5 page.
const UMA_TABLE_SIZE: usize = 0x8000;
/// The four KSM completion queues (low context-0 alias / firmware-high).
const COMPLETION_LOW_BASE: u64 = 0x70_0184_8000;
const COMPLETION_HIGH_BASE: u64 = KERNEL_BASE + 0xc07e_8000;
const COMPLETION_STRIDE: u64 = 0x2_8000;
/// Allocator-selected objects (QoS block, AON PTD) stay between the PB/UMA
/// tables and the fixed high graph.
const DYNAMIC_START: u64 = KERNEL_BASE + 0x168_0000;
const DYNAMIC_END: u64 = KERNEL_BASE + 0x1000_0000;

const _: () = {
    assert!(PBDESC_BASE + PBDESC_TABLE_SIZE as u64 <= UMA_BASE);
    assert!(UMA_BASE + UMA_TABLE_SIZE as u64 <= DYNAMIC_START);
    assert!(init::REGION_VIEW_LOW_ADDRS[0] + PBDESC_TABLE_SIZE as u64
        <= init::REGION_VIEW_LOW_ADDRS[1]);
    assert!(init::REGION_VIEW_LOW_ADDRS[1] + UMA_TABLE_SIZE as u64 <= COMPLETION_LOW_BASE);
};

/// Report rings keep their 25G76 shape (FW_PROVEN: event producer b000
/// 0x7f14, 256 x 0x48; trace producer b000 0x19984, 512 x 0x48; host owns the
/// cursor at +0, firmware the producer at +0x20).
const REPORT_RECORD_SIZE: usize = 0x48;
const REPORT_PRODUCER_OFFSET: u64 = 0x20;
const REPORT_CAPACITY: [u32; 2] = [256, 512];
const REPORT_DRAIN_BUDGET: usize = 128;
const TRACE_PRINT_BUDGET: [u32; 2] = [512, 16];
/// Event type the J813 firmware used for recovery records (INFERRED unchanged).
const EVENT_TYPE_RECOVERY: u32 = 4;

/// Owning mappings are declared before the UAT so they drop first. Aliases
/// retain the SAME GEM backing; no firmware pointer names a copied snapshot.
pub(crate) struct G18FirmwareMemory {
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
    uma_pool: G18UmaPoolMemory,
    completions: KVec<MappedObject>,
    aliases: KVec<mmu::KernelMapping>,
    registers: KVec<mmu::KernelMapping>,
    report_counts: [[u32; 2]; 2],
    trace_printed: [u32; 2],
    uat: Arc<mmu::Uat>,
}

/// Read-back of the new 26A428 status cross-links (plan step 1.3 validation):
/// status B +0xe714 header, +0xe71c word and +0xe720 pointer; status config
/// +0x6c4 word and +0x6c8 pointer.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) struct G18StatusLinks {
    pub(crate) status_b_header: u32,
    pub(crate) status_b_word: u32,
    pub(crate) status_b_pointer: u64,
    pub(crate) status_config_word: u32,
    pub(crate) status_config_pointer: u64,
}

impl G18FirmwareMemory {
    /// Allocate, map and encode the complete G18G init-data graph while both
    /// firmware processors are stopped. All fallible work happens before the
    /// caller publishes anything. The caller keeps its own `Arc` of the UAT
    /// until both processors stop, including when this construction fails.
    #[inline(never)]
    pub(crate) fn new_with_uat(
        dev: &AsahiDevice,
        uat: Arc<mmu::Uat>,
        boot_timestamp: u64,
    ) -> Result<KBox<Self>> {
        if !uat.is_t8152_geometry() {
            return Err(ENODEV);
        }
        let mut registers = KVec::new();
        for window in init::G18G_26A428_REGISTER_WINDOWS {
            let (physical, virtual_address, span) = window.mapped_pages().map_err(|_| EINVAL)?;
            // Flag 2 windows are firmware read-write; flag 0 windows are
            // mapped read-only, the J813 rule (INFERRED for 26A428).
            let protection = match window.flag {
                0 => mmu::PROT_FW_MMIO_RO,
                2 => mmu::PROT_FW_MMIO_RW,
                _ => return Err(EINVAL),
            };
            // Page-table writes only. No register contents are accessed.
            registers.push(
                uat.kernel_vm().map_io(virtual_address, physical.try_into()?, span.try_into()?,
                                       protection)?,
                GFP_KERNEL,
            )?;
        }

        let fixed = |address, size, protection| {
            MappedObject::new_wc_at(dev, uat.kernel_vm(), address, size, protection)
        };
        // Install every fixed address before allocator-selected objects.
        let mut private = fixed(PRIVATE_BASE, init::G18G_26A428_PRIVATE_CLUSTER_SIZE,
                                mmu::PROT_FW_SHARED_RW)?;
        // Both 26A428 roots are 0xc8 bytes. Separate mappings keep the empty
        // page between them.
        let mut primary_root = fixed(ROOTS_BASE, init::G18G_26A428_ROOT_SIZE, mmu::PROT_FW_SHARED_RO)?;
        let mut secondary_root = fixed(ROOTS_BASE + init::SECONDARY_ROOT_DELTA,
                                       init::G18G_26A428_ROOT_SIZE, mmu::PROT_FW_SHARED_RO)?;
        let mut shared = fixed(SHARED_BASE, init::G18G_26A428_SHARED_CLUSTER_SIZE, mmu::PROT_FW_PRIV_RW)?;
        let region_a = fixed(REGION_A_BASE, init::REGION_A_SIZE, mmu::PROT_FW_SHARED_RO)?;
        let mut region_c = fixed(REGION_C_BASE, init::REGION_C_SIZE, mmu::PROT_FW_SHARED_RO)?;
        let fwctl = fixed(FWCTL_BASE, FWCTL_SIZE, mmu::PROT_FW_SHARED_RW)?;
        // Zero-backed at handoff; owned by later (M2/M3) publication.
        let mut pbdesc = fixed(PBDESC_BASE, PBDESC_TABLE_SIZE, mmu::PROT_FW_SHARED_RW)?;
        let mut uma = fixed(UMA_BASE, UMA_TABLE_SIZE, mmu::PROT_FW_SHARED_RW)?;
        let mut aliases = KVec::new();
        for (object, address, size) in [
            (&mut pbdesc, init::REGION_VIEW_LOW_ADDRS[0], PBDESC_TABLE_SIZE),
            (&mut uma, init::REGION_VIEW_LOW_ADDRS[1], UMA_TABLE_SIZE),
        ] {
            aliases.push(object.map_alias_at(uat.kernel_lower_vm(), address, mmu::PROT_GPU_SHARED_RW)?,
                         GFP_KERNEL)?;
            for offset in (0..size).step_by(mmu::UAT_PGSZ) {
                if uat.kernel_vm().translate_iova(object.iova() + offset as u64)?
                    != uat.kernel_lower_vm().translate_iova(address + offset as u64)?
                {
                    return Err(EFAULT);
                }
            }
        }
        dev_info!(dev.as_ref(), "G18 UMA descriptor table: bytes={:#x} slots=256 stride=0x80 alias_pages={} verified\n",
            UMA_TABLE_SIZE, UMA_TABLE_SIZE / mmu::UAT_PGSZ);

        let mut completions = KVec::new();
        let mut completion_bindings =
            [init::G17PKsmCompletionAliases { low: 0, high: 0 }; init::M5_25G76_COMPLETION_QUEUE_COUNT];
        for (ordinal, binding) in completion_bindings.iter_mut().enumerate() {
            // Host-chosen pairs, the J813 placement (INFERRED for 26A428).
            let low = COMPLETION_LOW_BASE + ordinal as u64 * COMPLETION_STRIDE;
            let high = COMPLETION_HIGH_BASE + ordinal as u64 * COMPLETION_STRIDE;
            let mut backing = MappedObject::new_wc_at(dev, uat.kernel_vm(), high,
                init::M5_25G76_COMPLETION_BACKING_SIZE, mmu::PROT_FW_PRIV_RW)?;
            aliases.push(backing.map_alias_at(uat.kernel_lower_vm(), low, mmu::PROT_GPU_SHARED_RW)?,
                         GFP_KERNEL)?;
            for offset in (0..init::M5_25G76_COMPLETION_BACKING_SIZE).step_by(mmu::UAT_PGSZ) {
                if uat.kernel_vm().translate_iova(high + offset as u64)?
                    != uat.kernel_lower_vm().translate_iova(low + offset as u64)?
                {
                    return Err(EFAULT);
                }
            }
            *binding = init::G17PKsmCompletionAliases { low, high };
            completions.push(backing, GFP_KERNEL)?;
        }
        let dynamic = DYNAMIC_START..DYNAMIC_END;
        let qos = MappedObject::new_wc_in_range(dev, uat.kernel_vm(), dynamic.clone(),
            init::G17P_QOS_RESOURCE_SIZE, mmu::UAT_PGSZ as u64, mmu::PROT_FW_SHARED_RW)?;
        let mut aon = MappedObject::new_wc_in_range(dev, uat.kernel_vm(), dynamic,
            init::G18G_26A428_AON_PTD_SIZE, mmu::UAT_PGSZ as u64, mmu::PROT_FW_PRIV_RW)?;
        let uma_pool = G18UmaPoolMemory::new(dev, &uat, DYNAMIC_START..DYNAMIC_END,
            crate::hw::agx3::T8152_REGISTRY.num_cores)?;

        aon.with_bytes_mut(|out| init::encode_g18g_26a428_aon_ptd(out).map_err(|_| EINVAL))?;
        region_c.with_bytes_mut(|out| {
            init::encode_region_c_for_profile(PROFILE, out).map_err(|_| EINVAL)
        })?;
        private.with_bytes_mut(|out| {
            init::encode_g18g_26a428_private_cluster(PRIVATE_BASE, FWCTL_BASE, out).map_err(|_| EINVAL)
        })?;

        let layout = init::G18G_26A428_PRIVATE_LAYOUT;
        let addresses = init::SharedAddresses {
            region_a: region_a.iova(),
            region_c: region_c.iova(),
            hw_data_bundle: SHARED_BASE,
            hw_data_bundle_alloc: init::G18G_26A428_SHARED_CLUSTER_SIZE,
        };
        let primary = init::InstanceAddresses {
            root: ROOTS_BASE,
            main_config: SHARED_BASE + init::G18G_26A428_MAIN_BUNDLE_OFFSETS[0] as u64,
            status_a: PRIVATE_BASE + init::G18G_26A428_PRIVATE_STATUS_A[0] as u64,
            status_b: PRIVATE_BASE + layout.status_b as u64,
            secondary_extras: [0, 0],
        };
        let secondary = init::InstanceAddresses {
            root: ROOTS_BASE + init::SECONDARY_ROOT_DELTA,
            main_config: SHARED_BASE + init::G18G_26A428_MAIN_BUNDLE_OFFSETS[1] as u64,
            status_a: PRIVATE_BASE + init::G18G_26A428_PRIVATE_STATUS_A[1] as u64,
            status_b: 0,
            // Secondary root +0xb8 = status config, +0xc0 = the 0x60 object
            // (KEXT_PROVEN, g18g_key.c:3748-3812).
            secondary_extras: [
                PRIVATE_BASE + layout.status_config as u64,
                PRIVATE_BASE + layout.secondary_extra_1 as u64,
            ],
        };
        init::validate_pair(&primary, &secondary).map_err(|_| EINVAL)?;
        let views = init::PrimaryRegionViews {
            sentinel_high: SENTINEL_BASE,
            alias_high: [PBDESC_BASE, UMA_BASE],
        };
        // The J813 status-A trace mask (m5_fw_trace) is deliberately not
        // applied: the 26A428 trace-class bits have not been re-derived.
        shared.with_bytes_mut(|out| {
            init::encode_g18g_26a428_hw_data(completion_bindings, qos.iova(),
                &mut out[..init::G18G_26A428_HW_DATA_SIZE]).map_err(|_| EINVAL)?;
            init::encode_g18g_26a428_bundle_policy(init::M5PolicyBindings {
                state: PRIVATE_BASE + layout.policy_state as u64,
                aon_ptd: aon.iova(),
                boot_timestamp,
            }, out).map_err(|_| EINVAL)?;
            for (role, instance, views) in [
                (init::InstanceRole::Primary, &primary, Some(&views)),
                (init::InstanceRole::Secondary, &secondary, None),
            ] {
                let grid = PRIVATE_BASE + init::G18G_26A428_PRIVATE_STATE_GRIDS[role as usize] as u64;
                let channels = init::derive_channel_table_for_profile(PROFILE, role, grid,
                    instance.status_a, instance.main_config, SHARED_BASE);
                let offset = (instance.main_config - SHARED_BASE) as usize;
                init::encode_main_config_for_profile(PROFILE, role, &addresses, instance, &channels,
                    views, &mut out[offset..offset + init::MAIN_CONFIG_SIZE]).map_err(|_| EINVAL)?;
            }
            Ok(())
        })?;
        for (root, role, instance) in [
            (&mut primary_root, init::InstanceRole::Primary, &primary),
            (&mut secondary_root, init::InstanceRole::Secondary, &secondary),
        ] {
            root.with_bytes_mut(|out| {
                init::encode_root_for_profile(PROFILE, role, &addresses, instance, out)
                    .map_err(|_| EINVAL)?;
                // The two firmware gates (version word, root+0x2c) must pass
                // before anything is published.
                init::check_firmware_root_gates_for_profile(PROFILE, out).map_err(|_| EINVAL)
            })?;
        }
        // Unlike PB/UMA, the blank sentinel is unmapped at native handoff.
        if uat.kernel_vm().translate_iova(SENTINEL_BASE).is_ok() {
            return Err(EINVAL);
        }
        // Writes to all CPU WC mappings precede any future handoff. Only the
        // two opening control records are staged; work rings stay empty.
        fence(Ordering::SeqCst);
        // Keep the retained graph off the kernel stack.
        KBox::init(try_init!(Self {
            primary_root, secondary_root, shared, private, region_a, region_c, fwctl, aon, qos,
            pbdesc, uma, uma_pool, completions, aliases, registers,
            report_counts: [[0; 2]; 2], trace_printed: [0; 2], uat,
        }), GFP_KERNEL)
    }

    pub(crate) fn uma_pool(&self) -> &G18UmaPoolMemory { &self.uma_pool }

    pub(crate) fn prepare_uma_pool_binding(&mut self) -> Result<G18UmaPoolBinding> {
        self.uma_pool.prepare_binding()
    }

    /// FW VA of one role's root, for the EP 0x20 0x81 doorbell.
    pub(crate) fn root(&self, secondary: bool) -> u64 {
        if secondary { self.secondary_root.iova() } else { self.primary_root.iova() }
    }

    pub(crate) fn compute_outer_counters(&mut self) -> Result<[u32; 3]> {
        let base = PRIVATE_BASE + (init::G18G_26A428_PRIVATE_STATE_GRIDS[0]
            + init::WORK_STATE_GRID_OFFSETS[8]) as u64;
        Ok([self.private.read_u32(base)?, self.private.read_u32(base + 0x10)?,
            self.private.read_u32(base + 0x20)?])
    }

    /// G18 native 24-byte CL_2 record, limited to the registration and
    /// scheduler-bind records. Each new producer is published exactly once.
    pub(crate) fn publish_queue_probe(&mut self, address: u64, queue_id: u8, producer: u32) -> Result {
        if address < KERNEL_BASE || address & 0x3f != 0 || queue_id > 127
            || !(1..=3).contains(&producer) {
            return Err(EINVAL);
        }
        if self.compute_outer_counters()? != [producer - 1; 3]
            || self.control_counters()? != [[2; 3], [1; 3]] {
            return Err(EBUSY);
        }
        self.shared.with_bytes_mut(|out| {
            let start = init::G18G_26A428_WORK_RING_BUNDLE_OFFSETS[8] + (producer as usize - 1) * 24;
            let record = out.get_mut(start..start + 24).ok_or(EINVAL)?;
            if record.iter().any(|byte| *byte != 0) { return Err(EBUSY); }
            record[8..16].copy_from_slice(&address.to_le_bytes());
            record[16..20].copy_from_slice(&2u32.to_le_bytes());
            record[20..22].copy_from_slice(&(producer as u16).to_le_bytes());
            record[22] = queue_id;
            record[23] = 1;
            Ok(())
        })?;
        fence(Ordering::SeqCst);
        let producer_address = PRIVATE_BASE + (init::G18G_26A428_PRIVATE_STATE_GRIDS[0]
            + init::WORK_STATE_GRID_OFFSETS[8] + 0x20) as u64;
        self.private.write_u32(producer_address, producer)?;
        fence(Ordering::SeqCst);
        Ok(())
    }

    /// The live device-control counters (consumer, CFI, producer) of both
    /// roles, read from the shared state the firmware consumes.
    pub(crate) fn control_counters(&mut self) -> Result<[[u32; 3]; 2]> {
        self.private.with_bytes_mut(|bytes| {
            let mut counters = [[0; 3]; 2];
            for (role, grid) in init::G18G_26A428_PRIVATE_STATE_GRIDS.into_iter().enumerate() {
                let base = grid + init::WORK_STATE_GRID_OFFSETS[init::CONTROL_CHANNEL_INDEX];
                for (index, value) in counters[role].iter_mut().enumerate() {
                    let offset = base + index * init::CHANNEL_STATE_SPACING;
                    let word = bytes.get(offset..offset + 4).ok_or(EINVAL)?;
                    // SAFETY: the grid offsets are u32 aligned inside this
                    // bounded WC mapping, which is retained while firmware runs.
                    *value = unsafe { core::ptr::read_volatile(word.as_ptr().cast::<u32>()) };
                }
            }
            fence(Ordering::Acquire);
            Ok(counters)
        })
    }

    /// Stage device-control record 1 of role 0, IdlePowerOff(0) (opcode 0x0a,
    /// FW_PROVEN unchanged: b000 0x4ef0), and publish it. Only valid after
    /// both opening records retired, i.e. every counter reads 1.
    pub(crate) fn publish_idle_wake(&mut self) -> Result {
        if self.control_counters()? != [[1; 3]; 2] {
            return Err(EBUSY);
        }
        let record = init::G18G_26A428_CONTROL_RING_BUNDLE_OFFSETS[0] + init::CONTROL_RECORD_SIZE;
        self.shared.with_bytes_mut(|out| {
            let record = out.get_mut(record..record + init::CONTROL_RECORD_SIZE).ok_or(EINVAL)?;
            if record.iter().any(|byte| *byte != 0) {
                return Err(EBUSY);
            }
            init::encode_device_control_record(
                init::G18G_26A428_DEVICE_CONTROL_OPCODE_IDLE_POWER_OFF,
                init::DEVICE_CONTROL_IDLE_POWER_OFF_ARG_DISABLE_IDLE_AND_WAKE,
                record,
            )
            .map_err(|_| EINVAL)
        })?;
        fence(Ordering::SeqCst);
        let producer = init::G18G_26A428_PRIVATE_STATE_GRIDS[0]
            + init::WORK_STATE_GRID_OFFSETS[init::CONTROL_CHANNEL_INDEX]
            + init::CHANNEL_STATE_PRODUCER * init::CHANNEL_STATE_SPACING;
        self.private.write_u32(PRIVATE_BASE + producer as u64, init::CONTROL_OPENING_PRIMARY_PRODUCER + 1)?;
        fence(Ordering::SeqCst);
        Ok(())
    }

    /// The four firmware-owned KSM completion cursors (hw_data +0x2734 +
    /// ordinal * 0x20 + 0x1c; FW_PROVEN b000 0x1cbc0..0x1cc48).
    pub(crate) fn completion_cursors(&mut self) -> Result<[u32; init::M5_25G76_COMPLETION_QUEUE_COUNT]> {
        let mut cursors = [0; init::M5_25G76_COMPLETION_QUEUE_COUNT];
        for (ordinal, cursor) in cursors.iter_mut().enumerate() {
            *cursor = self.shared.read_u32(
                SHARED_BASE
                    + (init::G18G_26A428_KSM_COMPLETION_TABLE_OFFSET
                        + ordinal * init::G17P_KSM_COMPLETION_DESCRIPTOR_STRIDE
                        + init::G18G_26A428_KSM_COMPLETION_CURSOR) as u64,
            )?;
        }
        Ok(cursors)
    }

    /// Read back the new status cross-links from the host mapping (plan step
    /// 1.3 validation after the 0x81 ACKs).
    pub(crate) fn status_links(&mut self) -> Result<G18StatusLinks> {
        let layout = init::G18G_26A428_PRIVATE_LAYOUT;
        let status_b = PRIVATE_BASE + layout.status_b as u64;
        let config = PRIVATE_BASE + layout.status_config as u64;
        Ok(G18StatusLinks {
            status_b_header: self.private.read_u32(status_b + init::G18G_26A428_STATUS_B_CONFIG_HEADER as u64)?,
            status_b_word: self.private.read_u32(status_b + init::G18G_26A428_STATUS_B_LINK_WORD as u64)?,
            status_b_pointer: self.private.read_u64(status_b + init::G18G_26A428_STATUS_B_LINK_PTR as u64)?,
            status_config_word: self.private.read_u32(config + init::G18G_26A428_STATUS_CONFIG_LINK_WORD as u64)?,
            status_config_pointer: self.private.read_u64(config + init::G18G_26A428_STATUS_CONFIG_LINK_PTR as u64)?,
        })
    }

    /// Drain both roles' event (channel 13) and trace (channel 14) report
    /// rings. Records are copied before their slot is released; each drain is
    /// bounded. Returns whether an event record of the J813 recovery type was
    /// seen. This is the milestone-1 heartbeat (plan step 1.8, stage 4).
    pub(crate) fn drain_reports(&mut self, dev: &AsahiDevice) -> Result<bool> {
        let mut recovery = false;
        for (role, status_a) in init::G18G_26A428_PRIVATE_STATUS_A.into_iter().enumerate() {
            for channel in 0..2 {
                let [cursor_offset, ring_offset, _] = init::REPORT_STATE_STATUS_A_OFFSETS[channel];
                let capacity = REPORT_CAPACITY[channel];
                let cursor = PRIVATE_BASE + (status_a + cursor_offset) as u64;
                let ring = status_a + ring_offset;
                for _ in 0..REPORT_DRAIN_BUDGET {
                    let consumer = self.private.read_u32(cursor)?;
                    let producer = self.private.read_u32(cursor + REPORT_PRODUCER_OFFSET)?;
                    if consumer >= capacity || producer >= capacity {
                        return Err(EIO);
                    }
                    if consumer == producer {
                        break;
                    }
                    fence(Ordering::Acquire);
                    let offset = ring + consumer as usize * REPORT_RECORD_SIZE;
                    let mut raw = [0u8; REPORT_RECORD_SIZE];
                    self.private.with_bytes_mut(|bytes| {
                        raw.copy_from_slice(bytes.get(offset..offset + REPORT_RECORD_SIZE).ok_or(EINVAL)?);
                        Ok(())
                    })?;
                    let word = |off: usize| u32::from_le_bytes([raw[off], raw[off + 1], raw[off + 2], raw[off + 3]]);
                    let qword = |off: usize| u64::from(word(off)) | (u64::from(word(off + 4)) << 32);
                    let count = self.report_counts[role][channel];
                    if channel == 0 {
                        dev_info!(dev.as_ref(), "G18_EVENT role={} ordinal={} type={} payload={:x?}\n",
                            role, count, word(0), [qword(4), qword(12), qword(20), qword(28), qword(36)]);
                        recovery |= word(0) == EVENT_TYPE_RECOVERY;
                    } else if self.trace_printed[role] < TRACE_PRINT_BUDGET[role] {
                        dev_info!(dev.as_ref(),
                            "G18_TRACE role={} ordinal={} timestamp={} type={} id={:#x} args={:x?}\n",
                            role, count, qword(4), word(0), word(0x2c),
                            [qword(12), qword(20), qword(28), qword(36)]);
                        self.trace_printed[role] += 1;
                    }
                    self.report_counts[role][channel] = count.saturating_add(1);
                    fence(Ordering::SeqCst);
                    self.private.write_u32(cursor, (consumer + 1) % capacity)?;
                }
            }
        }
        Ok(recovery)
    }

    /// Report-record totals per role and channel since construction.
    pub(crate) fn report_counts(&self) -> [[u32; 2]; 2] {
        self.report_counts
    }

    /// One-line construction summary for the bring-up log.
    pub(crate) fn log_summary(&self, dev: &AsahiDevice) {
        dev_info!(dev.as_ref(),
            "G18G initdata: roots {:#x}/{:#x} version {:#x}; shared {:#x}+{:#x}; private {:#x}+{:#x}; {} register windows; {} unproven fields\n",
            self.root(false), self.root(true), init::G18G_26A428_INITDATA_VERSION_WORD,
            SHARED_BASE, init::G18G_26A428_SHARED_CLUSTER_SIZE,
            PRIVATE_BASE, init::G18G_26A428_PRIVATE_CLUSTER_SIZE,
            self.registers.len(), init::G18G_26A428_UNPROVEN.len());
    }
}
