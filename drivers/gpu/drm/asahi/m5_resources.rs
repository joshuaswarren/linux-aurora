// SPDX-License-Identifier: GPL-2.0-only OR MIT

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

#[path = "m5_queue_probe.rs"]
mod queue_probe;
use queue_probe::M5QueueProbe;
#[path = "m5_program_memory.rs"]
mod program_memory;
use program_memory::M5ProgramMemory;

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
    queue_probe: Option<M5QueueProbe>,
    program: Option<M5ProgramMemory>,
    report_counts: [[u32; 2]; 2],
    trace_printed: [u32; 2],
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
        probe_kind: u32,
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
        // These two tables have zero-backed mappings at handoff. Mode6
        // prepares its own UMA slot before any compute publication.
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
        let trace_mask = *crate::module_parameters::m5_fw_trace.value();
        if trace_mask & !0x203 != 0 { return Err(EINVAL); }
        // Exact M5 GFX firmware 0x1fadc retains root+0xa8 (status A),
        // then 0x3074..0x3088 copies status-A+0 into its trace flags.
        // Bits 0 and 1 gate power/submission records; 0x6260 reads the
        // NEXT byte and tests bit 1 (full-word bit 9) for recovery summary
        // 0x909. Publish through the host field before either descriptor.
        let native_trace = private.read_u32(primary.status_a)?;
        private.write_u32(primary.status_a, native_trace | trace_mask)?;
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
        let queue_probe = if probe_kind >= 3 {
            Some(M5QueueProbe::new(dev, &uat, probe_kind >= 4)?)
        } else {
            None
        };
        let program = if probe_kind >= 5 {
            Some(M5ProgramMemory::new(dev, &uat, probe_kind >= 6)?)
        } else {
            None
        };
        if probe_kind >= 6 {
            let descriptor = program.as_ref().ok_or(EINVAL)?.uma_descriptor()?;
            uma.with_bytes_mut(|out| {
                for (index, word) in descriptor.into_iter().enumerate() {
                    out[index * 8..index * 8 + 8].copy_from_slice(&word.to_le_bytes());
                }
                Ok(())
            })?;
        }
        fence(Ordering::SeqCst);
        Ok(Self { primary_root, secondary_root, shared, private, region_a, region_c, fwctl, aon, qos,
                  pbdesc, uma, completions, aliases, registers, queue_probe, program,
                  report_counts: [[0; 2]; 2], trace_printed: [0; 2], uat })
    }

    /// Drain M5 report channels using the native producer contracts.
    /// GFX 0x6ca8..0x6d18 emits 256 x 0x48 event records; 0x17e18
    /// emits 512 x 0x48 trace records. In each cursor object the host owns
    /// +0 and firmware owns +0x20. Copy before releasing each slot.
    pub(crate) fn drain_reports(&mut self, dev: &AsahiDevice) -> Result<bool> {
        let mut recovery = false;
        for (role, status_offset) in [0xee40u64, 0xc3100].into_iter().enumerate() {
            for (channel, (cursor_offset, ring_offset, capacity)) in
                [(0x40u64, 0x2c0u64, 256u32), (0x240, 0xa6ac0, 512)]
                    .into_iter().enumerate() {
                let cursor = PRIVATE_BASE + status_offset + cursor_offset;
                let ring = (status_offset + ring_offset) as usize;
                for _ in 0..128 {
                    let consumer = self.private.read_u32(cursor)?;
                    let producer = self.private.read_u32(cursor + 0x20)?;
                    if consumer >= capacity || producer >= capacity { return Err(EIO); }
                    if consumer == producer { break; }
                    fence(Ordering::Acquire);
                    let offset = ring + consumer as usize * 0x48;
                    let mut raw = [0u8; 0x48];
                    self.private.with_bytes_mut(|bytes| {
                        raw.copy_from_slice(bytes.get(offset..offset + 0x48).ok_or(EINVAL)?);
                        Ok(())
                    })?;
                    let word = |off: usize| u32::from_le_bytes(raw[off..off + 4].try_into().unwrap());
                    let qword = |off: usize| u64::from_le_bytes(raw[off..off + 8].try_into().unwrap());
                    let count = self.report_counts[role][channel];
                    if channel == 0 {
                        dev_info!(dev.as_ref(), "M5_EVENT role={} ordinal={} type={} payload={:x?}\n",
                            role, count, word(0), [qword(4), qword(12), qword(20), qword(28), qword(36)]);
                        recovery |= word(0) == 4;
                    } else {
                        let args = [qword(12), qword(20), qword(28), qword(36)];
                        // Exact M5 0x24eb4/0x26754 emit zero-argument
                        // processing-loop brackets. Count and consume them
                        // without spending the diagnostic print budget.
                        let loop_marker = word(0) == 5 && args == [0; 4]
                            && matches!(word(44) & 0x00ff_ffff, 0x105 | 0x106);
                        if !loop_marker && self.trace_printed[role] < if role == 0 { 512 } else { 16 } {
                            dev_info!(dev.as_ref(), "M5_TRACE role={} ordinal={} timestamp={} type={} id={:#x} args={:x?}\n",
                                role, count, qword(4), word(0), word(44), args);
                            self.trace_printed[role] += 1;
                        }
                    }
                    self.report_counts[role][channel] = count.saturating_add(1);
                    fence(Ordering::SeqCst);
                    self.private.write_u32(cursor, (consumer + 1) % capacity)?;
                }
            }
        }
        Ok(recovery)
    }

    pub(crate) fn validate_program_context(&mut self) -> Result<[u64; 2]> {
        self.program.as_mut().ok_or(EINVAL)?.validate_unsubmitted()
    }

    pub(crate) fn power_status(&self) -> Result<[u32; 3]> {
        Ok(self.queue_probe.as_ref().ok_or(EINVAL)?.power_status())
    }

    pub(crate) fn publish_idle_wake(&mut self) -> Result {
        if self.control_counters()? != [[1; 3]; 2] { return Err(EBUSY); }
        self.shared.with_bytes_mut(|out| {
            let record = &mut out[0x1eac0..0x1eb00];
            if record.iter().any(|x| *x != 0) { return Err(EBUSY); }
            record[..4].copy_from_slice(&0x0au32.to_le_bytes());
            Ok(())
        })?;
        fence(Ordering::SeqCst);
        let producer = init::WORK_STATE_GRID_OFFSETS[init::CONTROL_CHANNEL_INDEX]
            + 2 * init::CHANNEL_STATE_SPACING;
        self.private.write_u32(PRIVATE_BASE + producer as u64, 2)?;
        fence(Ordering::SeqCst);
        Ok(())
    }

    pub(crate) fn compute_pool_state(&mut self) -> Result<[u64; 5]> {
        self.program.as_mut().ok_or(EINVAL)?.pool_state()
    }

    pub(crate) fn compute_pool_ready(state: [u64; 5]) -> bool {
        M5ProgramMemory::pool_ready(state)
    }

    pub(crate) fn compute_result(&mut self) -> Result<crate::m5_compute_program::ComputeResult> {
        self.program.as_mut().ok_or(EINVAL)?.result()
    }

    pub(crate) fn compute_progress(&mut self) -> Result<[u32; 5]> {
        self.program.as_mut().ok_or(EINVAL)?.progress()
    }

    pub(crate) fn compute_observations(&mut self) -> Result<([u64; 4], [u64; 4], u32)> {
        let firmware = self.queue_probe.as_ref().ok_or(EINVAL)?.compute_observations();
        let uma = [self.uma.read_u64(UMA_BASE)?, self.uma.read_u64(UMA_BASE + 8)?,
                   self.uma.read_u64(UMA_BASE + 16)?, self.uma.read_u64(UMA_BASE + 24)?];
        let completion_cursor = self.shared.read_u32(SHARED_BASE + 0x2614 + 2 * 0x20 + 0x1c)?;
        Ok((firmware, uma, completion_cursor))
    }

    pub(crate) fn publish_compute_job(&mut self) -> Result {
        let (outer, inner, _) = self.queue_probe_status()?;
        if outer != [1; 3] || inner != [1; 3] { return Err(EBUSY); }
        let address = self.queue_probe.as_ref().ok_or(EINVAL)?.queue_address();
        self.queue_probe.as_mut().ok_or(EINVAL)?.prepare_compute_job()?;
        self.shared.with_bytes_mut(|out| {
            let record = &mut out[0x19dd8..0x19df0];
            if record.iter().any(|x| *x != 0) { return Err(EBUSY); }
            record[8..16].copy_from_slice(&address.to_le_bytes());
            record[16..20].copy_from_slice(&2u32.to_le_bytes());
            record[20..22].copy_from_slice(&4u16.to_le_bytes());
            record[22] = 11;
            record[23] = 1;
            Ok(())
        })?;
        fence(Ordering::SeqCst);
        self.private.write_u32(PRIVATE_BASE + 0x260, 2)?;
        fence(Ordering::SeqCst);
        Ok(())
    }

    pub(crate) fn root(&self, secondary: bool) -> u64 {
        if secondary { self.secondary_root.iova() } else { self.primary_root.iova() }
    }

    /// Read the actual shared control state, not a detached host copy. The
    /// offsets and three pointers per role are checked by the M5 capture
    /// verifier against both native main objects and the private arena.
    pub(crate) fn control_counters(&mut self) -> Result<[[u32; 3]; 2]> {
        self.private.with_bytes_mut(|bytes| {
            let mut counters = [[0; 3]; 2];
            for (role, grid) in [0usize, 0x1770c0].into_iter().enumerate() {
                let base = grid + init::WORK_STATE_GRID_OFFSETS[init::CONTROL_CHANNEL_INDEX];
                for (index, value) in counters[role].iter_mut().enumerate() {
                    let offset = base + index * init::CHANNEL_STATE_SPACING;
                    let word = bytes.get(offset..offset + 4).ok_or(EINVAL)?;
                    // SAFETY: arena and native counter offsets are u32 aligned;
                    // this bounded WC mapping is retained while firmware runs.
                    *value = unsafe { core::ptr::read_volatile(word.as_ptr().cast::<u32>()) };
                }
            }
            fence(Ordering::Acquire);
            Ok(counters)
        })
    }

    pub(crate) fn queue_probe_status(&mut self) -> Result<([u32; 3], [u32; 3], [u64; 5])> {
        let outer = [self.private.read_u32(PRIVATE_BASE + 0x240)?,
                     self.private.read_u32(PRIVATE_BASE + 0x250)?,
                     self.private.read_u32(PRIVATE_BASE + 0x260)?];
        let probe = self.queue_probe.as_mut().ok_or(EINVAL)?;
        Ok((outer, probe.inner_counters()?, probe.firmware_slot()))
    }

    /// Publish exactly one tag-15 configuration through the observed CL_2
    /// outer slot. All referenced memory was prepared before initdata ACKs.
    pub(crate) fn publish_queue_probe(&mut self) -> Result {
        let (outer, inner, slot) = self.queue_probe_status()?;
        if outer != [0; 3] || inner != [0, 0, 1] || slot != [0; 5] {
            return Err(EBUSY);
        }
        if let Some(status) = self.scheduler_probe_status()? {
            if status != [0, 0xffff, 0] { return Err(EBUSY); }
        }
        let address = self.queue_probe.as_ref().ok_or(EINVAL)?.queue_address();
        self.shared.with_bytes_mut(|out| {
            // Hardware capture: main row8 ring=shared+0x19dc0, 256*24B.
            let record = &mut out[0x19dc0..0x19dd8];
            if record.iter().any(|byte| *byte != 0) {
                return Err(EBUSY);
            }
            record[8..16].copy_from_slice(&address.to_le_bytes());
            record[16..20].copy_from_slice(&2u32.to_le_bytes());
            record[20..22].copy_from_slice(&1u16.to_le_bytes());
            record[22] = 11;
            record[23] = 1;
            Ok(())
        })?;
        // Record/backing writes precede the sole producer publication.
        fence(Ordering::SeqCst);
        self.private.write_u32(PRIVATE_BASE + 0x260, 1)?;
        fence(Ordering::SeqCst);
        Ok(())
    }

    pub(crate) fn queue_probe_installed(slot: [u64; 5]) -> bool {
        M5QueueProbe::is_installed(slot)
    }

    pub(crate) fn scheduler_probe_status(&mut self) -> Result<Option<[u64; 3]>> {
        self.queue_probe.as_mut().ok_or(EINVAL)?.scheduler_status()
    }

    pub(crate) fn scheduler_probe_bound(status: Option<[u64; 3]>) -> bool {
        M5QueueProbe::scheduler_bound(status)
    }
}
