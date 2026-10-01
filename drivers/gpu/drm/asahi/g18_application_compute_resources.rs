// SPDX-License-Identifier: GPL-2.0-only OR MIT
//! Bounded application CDM owner, separate from the captured addition fixture.
use super::*;
use crate::{g17_uapi::TranslatedComputeCommand, g18_address_map as va, g18_compute::*};
use core::sync::atomic::Ordering;

// Application-compute-only knobs K1-K5 (hv/private/diag-compute-162) and
// g18_app_d62a (hv/private/case10-168). Unlike
// kernel::module! parameters (perm 0, set once at load), these are 0644 with
// the C uint ops, so /sys/module/asahi/parameters/g18_app_* may be changed
// between submissions. prepare_application_compute reads each exactly once
// per submission; nothing else reads them, so graphics and the stage8 fixture
// are unaffected. Registration otherwise follows g18_render_policy.rs.
macro_rules! app_knob {
    ($module:ident, $name:literal, $default:expr, $description:literal) => {
        mod $module {
            use kernel::{bindings, module_param::KernelParam};
            pub(super) static VALUE: core::sync::atomic::AtomicU32 =
                core::sync::atomic::AtomicU32::new($default);
            #[link_section = "__param"]
            #[used(compiler)]
            static PARAM: KernelParam = KernelParam::new(bindings::kernel_param {
                name: kernel::str::as_char_ptr_in_const_context(if cfg!(MODULE) {
                    kernel::c_str!($name)
                } else { kernel::c_str!(concat!("asahi.", $name)) }),
                // Module lifetime and parameter serialization are managed by the
                // kernel, identically to the kernel::module! registration. A raw
                // address avoids a reference to the mutable extern static.
                #[cfg(MODULE)]
                mod_: &raw mut bindings::__this_module,
                #[cfg(not(MODULE))]
                mod_: core::ptr::null_mut(),
                // SAFETY: exported vmlinux ops; they access `arg` as an unsigned
                // int under the parameter lock, which AtomicU32 represents exactly.
                ops: unsafe { core::ptr::from_ref(&bindings::param_ops_uint) },
                perm: 0o644, level: -1, flags: 0,
                __bindgen_anon_1: bindings::kernel_param__bindgen_ty_1 {
                    arg: core::ptr::from_ref(&VALUE).cast_mut().cast(),
                },
            });
            const TYPE: &[u8] = concat!("parmtype=", $name, ":uint\0").as_bytes();
            #[link_section = ".modinfo"]
            #[used(compiler)]
            static TYPE_INFO: [u8; TYPE.len()] = super::modinfo(TYPE);
            const TEXT: &[u8] = concat!("parm=", $name, ":", $description, "\0").as_bytes();
            #[link_section = ".modinfo"]
            #[used(compiler)]
            static TEXT_INFO: [u8; TEXT.len()] = super::modinfo(TEXT);
        }
    };
}

const fn modinfo<const N: usize>(bytes: &[u8]) -> [u8; N] {
    let mut out = [0; N];
    let mut i = 0;
    while i < N { out[i] = bytes[i]; i += 1; }
    out
}

app_knob!(knob_d514, "g18_app_d514", 0xffff_ffff,
    "Application compute command +0xf28 (native desc+0x514). Default 0xffffffff = native, fixture and M5; 0 = candidate160. Read at every compute submission");
// Defaults are the values that retire on hardware (run 162d: exact integer
// compute and atomics readbacks). The native 0xa731=1 alone stalls the
// dispatch silently, with no fault or watchdog.
app_knob!(knob_a731, "g18_app_a731", 0,
    "Application compute first 0xa731: 0 (default) = fixture/M5 (emits 0), proven on hardware; 1 = native integer compute (d420=1,d418=2 emits 1), which stalls the dispatch on Linux. Other values refuse the submission");
app_knob!(knob_sma_pair, "g18_app_sma_pair", 0x6_0010,
    "Application compute 0xa5e1 SMA pair as d550|d558<<16: 0x60010 (16,6) fixture (default, proven on hardware), 0xb001e (30,11) native integer compute, 0xc001e (30,12) candidate160. Must fit the pool plan (30,12); other values refuse the submission");
app_knob!(knob_sma_units, "g18_app_sma_units", 0,
    "Application compute 0x10241[63:16] SMA result (AGXUMADataRec+0x100). Default 0 (proven on hardware); 0xae = native integer compute. Independent of the pool binding generation");
app_knob!(knob_preempt_mode, "g18_app_preempt_mode", 1,
    "Application compute 0x1a510 object: 1 (default) = fresh zeroed 0xc000 bytes mapped only in the job VM at 0x1100000000 (driver arena); 0 = candidate160 16 KiB page at 0x7050000000. Other values refuse the submission");
app_knob!(knob_d62a, "g18_app_d62a", 1,
    "Application compute desc+0x62a (0x1a458 0x14c08a60 texture mode instead of 0x10c08860): 1 (default) = from the command's DRM_ASAHI_COMPUTE_USES_TEXTURE_STATE flag, as native; 0 = never; 2 = always. Other values refuse the submission");

/// Every SMA pair witnessed in a completed native or fixture compute command.
const SMA_PAIRS: [[u64; 2]; 3] = [[30, 11], [30, 12], [16, 6]];

/// One submission's knob values, read once and logged with its kick.
#[derive(Clone, Copy)]
pub(crate) struct ApplicationComputeKnobs {
    d514: u32,
    a731: u32,
    sma_pair: u32,
    sma_units: u32,
    preempt_mode: u32,
    d62a_mode: u32,
    preempt: u64,
    preempt_bytes: u64,
    pool_slot: u32,
    /// The command's USES_TEXTURE_STATE flag and the d62a it produced.
    texture_state: bool,
    d62a: u8,
}

impl ApplicationComputeKnobs {
    fn read() -> Self {
        Self {
            d514: knob_d514::VALUE.load(Ordering::Relaxed),
            a731: knob_a731::VALUE.load(Ordering::Relaxed),
            sma_pair: knob_sma_pair::VALUE.load(Ordering::Relaxed),
            sma_units: knob_sma_units::VALUE.load(Ordering::Relaxed),
            preempt_mode: knob_preempt_mode::VALUE.load(Ordering::Relaxed),
            d62a_mode: knob_d62a::VALUE.load(Ordering::Relaxed),
            preempt: 0, preempt_bytes: 0, pool_slot: 0,
            texture_state: false, d62a: 0,
        }
    }

    /// Native run112 sets desc+0x62a (0x1a458 = 0x14c08a60) on every compute
    /// that uses texture or image instructions and on none of the
    /// buffer-only ones; Mesa reports that per command.
    fn resolve_d62a(&mut self, texture_state: bool) {
        self.texture_state = texture_state;
        self.d62a = match self.d62a_mode {
            0 => 0,
            2 => 1,
            _ => u8::from(texture_state),
        };
    }

    fn pair(&self) -> [u64; 2] {
        [u64::from(self.sma_pair & 0xffff), u64::from(self.sma_pair >> 16)]
    }

    /// Refuse, before any owner changes, values that would make the command
    /// inconsistent. Nothing is published for a refused submission.
    fn validate(&self, dev: &AsahiDevice) -> Result {
        let refusal = if self.a731 > 1 {
            Some(("g18_app_a731", self.a731, "admitted 0 (fixture) or 1 (native)"))
        } else if !SMA_PAIRS.contains(&self.pair()) {
            Some(("g18_app_sma_pair", self.sma_pair,
                "admitted 0xb001e (30,11), 0xc001e (30,12) or 0x60010 (16,6)"))
        } else if self.preempt_mode > 1 {
            Some(("g18_app_preempt_mode", self.preempt_mode,
                "admitted 0 (0x7050000000 page) or 1 (driver arena)"))
        } else if self.d62a_mode > 2 {
            Some(("g18_app_d62a", self.d62a_mode,
                "admitted 0 (never), 1 (from the command flag) or 2 (always)"))
        } else { None };
        if let Some((knob, value, reason)) = refusal {
            dev_err!(dev.as_ref(),
                "G18_APP_COMPUTE_REFUSED knob={} value={:#x}: {}; command not sent\n",
                knob, value, reason);
            return Err(EINVAL);
        }
        Ok(())
    }
}

impl kernel::fmt::Display for ApplicationComputeKnobs {
    fn fmt(&self, f: &mut kernel::fmt::Formatter<'_>) -> kernel::fmt::Result {
        let [d550, d558] = self.pair();
        write!(f, "d514={:#x} a731={} sma_pair=({},{}) reg_a5e1={:#x} sma_units={:#x} reg_10241={:#x} preempt_mode={} preempt={:#x}+{:#x} texture_state={} d62a_mode={} d62a={}",
            self.d514, self.a731, d550, d558, d550 | d558 << 16, self.sma_units,
            (self.pool_slot as i32 as i64 as u64) | u64::from(self.sma_units) << 16,
            self.preempt_mode, self.preempt, self.preempt_bytes,
            u8::from(self.texture_state), self.d62a_mode, self.d62a)
    }
}

pub(super) struct ApplicationCompute {
    guard: Option<mmu::T8140VmJobGuard>,
    aliases: KVec<mmu::KernelMapping>,
    // g18_app_preempt_mode=1 object, mapped only in the submitting job VM.
    // Replaced only after the previous generation retired.
    driver_preempt: Option<MappedObject>,
    pool: G18UmaPoolMemory,
    binding: G18UmaPoolBinding,
    preempt: MappedObject,
    generation: u32,
    retired: bool,
}

impl G18FirmwareMemory {
    pub(crate) fn prepare_application_compute(&mut self, dev: &AsahiDevice,
        vm: &mmu::Vm, command: &TranslatedComputeCommand)
        -> Result<(G18PreparedComputeCommand, G18UmaPoolBinding, u32, ApplicationComputeKnobs)> {
        if command.sampler_heap != 0 || command.sampler_count != 0
            || command.g17p_add3_buffers.is_some()
            || command.control_stream_end.checked_sub(command.control_stream_base)
                .map_or(true, |n| n > 48) { return Err(ENOTSUPP); }
        let mut knobs = ApplicationComputeKnobs::read();
        knobs.validate(dev)?;
        knobs.resolve_d62a(command.g18_texture_state);
        if self.application_compute.is_none() {
            let high = va::DYNAMIC_HIGH_START..va::DYNAMIC_HIGH_END;
            let mut pool = G18UmaPoolMemory::new_application_compute(dev, &self.uat, high)?;
            let binding = pool.prepare_binding()?;
            pool.load_hardware_descriptor(dev, &mut self.uma, binding,
                j873_sma_device_words(2, 6)?)?;
            let preempt = MappedObject::new_wc_in_range(dev, self.uat.kernel_lower_vm(),
                0x70_5000_0000..0x70_5100_0000, 0x4000, 0x4000,
                mmu::PROT_GPU_FW_SHARED_RW)?;
            self.scheduler.prepare(0, 1)?;
            self.application_compute = Some(KBox::new(ApplicationCompute {
                guard: None, aliases: KVec::new(), driver_preempt: None, pool, binding,
                preempt, generation: 0, retired: true,
            }, GFP_KERNEL)?);
        }
        let job = self.application_compute.as_mut().ok_or(EIO)?;
        if !job.retired { return Err(EBUSY); }
        let generation = job.generation.checked_add(1).ok_or(EOVERFLOW)?;
        if generation > 0x00ff_ffff { return Err(EOVERFLOW); }
        let previous = self.scheduler.snapshot(0)?;
        if previous.submitted != job.generation || previous.completed != job.generation
            || previous.linked != 0 { return Err(EBUSY); }
        // The pool is sized once for its plan pair; a larger pair would name
        // backing that does not exist. The slot must not reach the SMA half.
        let plan = job.pool.scratch_plan().ok_or(EINVAL)?;
        let pair = knobs.pair();
        if pair[0] > plan.register_pair[0] || pair[1] > plan.register_pair[1] {
            dev_err!(dev.as_ref(),
                "G18_APP_COMPUTE_REFUSED knob=g18_app_sma_pair value={:#x}: exceeds pool plan {:?}; command not sent\n",
                knobs.sma_pair, plan.register_pair);
            return Err(EINVAL);
        }
        if job.binding.slot >= 0x1_0000 {
            dev_err!(dev.as_ref(),
                "G18_APP_COMPUTE_REFUSED knob=g18_app_sma_units value={:#x}: pool slot {} overlaps 0x10241[63:16]; command not sent\n",
                knobs.sma_units, job.binding.slot);
            return Err(EINVAL);
        }
        knobs.pool_slot = job.binding.slot;
        let driver = va::APPLICATION_COMPUTE_PREEMPT_DRIVER;
        let driver_bytes = va::APPLICATION_COMPUTE_PREEMPT_DRIVER_BYTES;
        let driver_range = driver..driver + driver_bytes + mmu::UAT_PGSZ as u64;
        let reserved = vm.kernel_range();
        if knobs.preempt_mode == 1
            && (driver_range.start < reserved.start || driver_range.end > reserved.end) {
            dev_err!(dev.as_ref(),
                "G18_APP_COMPUTE_REFUSED knob=g18_app_preempt_mode value=1: VM_CREATE driver range {:#x}..{:#x} does not contain {:#x}..{:#x}; command not sent\n",
                reserved.start, reserved.end, driver_range.start, driver_range.end);
            return Err(EINVAL);
        }
        job.guard = Some(vm.retain_t8140_job()?);
        job.retired = false;
        job.aliases.clear();
        job.driver_preempt = None;
        let context = self.compute.bind_process(dev, &self.uat, vm)?;
        job.aliases = job.pool.compute_application_aliases(dev, &self.uat, vm)?;
        let preempt = if knobs.preempt_mode == 0 {
            let preempt = job.preempt.iova();
            job.preempt.with_bytes_mut(|bytes| { bytes.fill(0); Ok(()) })?;
            let alias = job.preempt.map_alias_at(vm, preempt, mmu::PROT_GPU_SHARED_RW)?;
            job.aliases.push(alias, GFP_KERNEL)?;
            if vm.translate_iova(preempt)? != self.uat.kernel_lower_vm().translate_iova(preempt)? {
                return Err(EFAULT);
            }
            knobs.preempt_bytes = 0x4000;
            preempt
        } else {
            // Native keeps this object in the process VM only (0x100_003f_0000,
            // 48 KiB). The driver arena is refused to userspace binds, and the
            // range admits exactly one placement: the object plus one guard.
            let object = MappedObject::new_wc_in_range_with_guards(dev, vm, driver_range,
                driver_bytes as usize, mmu::UAT_PGSZ as u64, mmu::PROT_GPU_SHARED_RW, 1)?;
            let preempt = object.iova();
            job.driver_preempt = Some(object);
            if preempt != driver { return Err(EFAULT); }
            for offset in (0..driver_bytes).step_by(mmu::UAT_PGSZ) {
                vm.translate_iova(preempt + offset)?;
            }
            if vm.translate_iova(preempt + driver_bytes).is_ok() { return Err(EFAULT); }
            knobs.preempt_bytes = driver_bytes;
            preempt
        };
        knobs.preempt = preempt;
        let inputs = G18ComputeCommandInputs {
            sequence: u64::from(generation), d100: 1,
            completion: G18ComputeCompletionState {
                stamp: generation << 8, index: 11, entries: 256, generation: 0, base: 1,
            },
            d4e0: command.control_stream_terminator, d7b8: job.binding.state,
            ..Default::default()
        };
        // G18 preemption pointers are +0x1b80..+0x1b98 (native gallery),
        // not the M5 +0x1480 layout. These are fresh driver-owned zero pages.
        let registers = G18ComputeRegisterInputs {
            program: [preempt, command.control_stream_base, preempt + 0x1b80,
                preempt + 0x1b88, preempt + 0x1b90, preempt + 0x1b98],
            d550: pair[0], d558: pair[1], d774: 0x1c, d5c4: 8,
            d420: knobs.a731 as u8, d418: 2, d50c: 1, d514: knobs.d514,
            d62a: knobs.d62a,
            context_id: context, pool_slot: job.binding.slot,
            sma_units: u64::from(knobs.sma_units),
            a10aac: 1, a6d0: 1, clusters: 2, cluster_width: 6,
            ..Default::default()
        };
        let prepared = self.compute.prepare(self.scheduler.binding(0)?, &inputs, &registers)?;
        job.generation = generation;
        self.completion_masks = [0; 2];
        Ok((prepared, job.binding, generation, knobs))
    }

    pub(crate) fn retire_application_compute(&mut self, generation: u32) -> Result {
        let job = self.application_compute.as_mut().ok_or(EIO)?;
        if job.retired || job.generation != generation { return Err(EINVAL); }
        self.compute.retire_application(generation << 8)?;
        job.retired = true;
        job.guard = None;
        Ok(())
    }

    /// The last compute stamp kicked to QID 11 (0 before the first dispatch).
    pub(crate) fn application_compute_generation(&self) -> u32 {
        self.application_compute.as_ref().map_or(0, |job| job.generation)
    }

    /// As [`Self::retire_application_compute`] for a dispatch a recovery ended.
    pub(crate) fn retire_recovered_application_compute(&mut self, generation: u32) -> Result {
        let job = self.application_compute.as_mut().ok_or(EIO)?;
        if job.retired || job.generation != generation { return Err(EINVAL); }
        self.compute.retire_recovered()?;
        job.retired = true;
        job.guard = None;
        Ok(())
    }

    pub(crate) fn quarantine_application_compute(&mut self) {
        if let Some(job) = &self.application_compute {
            if let Some(guard) = &job.guard { guard.quarantine(); }
        }
    }
}
