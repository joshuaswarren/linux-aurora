// SPDX-License-Identifier: GPL-2.0-only OR MIT
//! Recovery isolation (`g17p_recovery_isolation`): after a firmware
//! stamp-blamed recovery only the guilty client loses.
//!
//! Root-only sysfs, runtime-writable bitmask; also settable at load. Writing
//! 0 restores the accepted Y behaviour exactly (no completion is ever marked,
//! no admission wait is taken). Writing "reset" clears the counters.
//!
//! Hardware evidence (saved kernel logs, 2026-09-30/10-01): a stamp-blamed
//! recovery cancels in-flight work of EVERY client, and Y then poisons each
//! affected VM's sticky status, which Mesa turns into VK_ERROR_DEVICE_LOST:
//!   * mmuf2 01:10:21 -- blamed qid 10 (Hyprland, VM 15). Xwayland's compute
//!     qid 7 (VM 17) retired zero-duration -> `fail_while_owned(ENODATA)`
//!     -> VM 17 status -61 -> every map refused -> Xwayland died.
//!   * hypr-crash-0224 02:24:31 -- blamed qid 5 (another client). Hyprland's
//!     parked 3D (qid 14) never ran after the recovery; its 2 s timeout
//!     retired it unexecuted -> `complete(Err(EIO))` -> VM 43 status -5 ->
//!     Hyprland aborted.
//!
//! Recovery-info entry layout. Native decode (static RE of the J700 kext,
//! `AGXArmFirmware::processFirmwareInitiatedRecovery` in
//! neo-stability-20260930/native/kext25.dis @fffffe00088a8398): it walks all
//! 256 8-byte entries, skips those without bit0, takes qid = bits 4..10
//! (`ubfx w24, w22, #4, #7`), looks up the command by (qid, word1) and stores
//! a per-command error code: 5 when bit1 is clear, otherwise 3 (or 0xb when
//! the restart reason is 3). In public macOS logs 5 is
//! kIOGPUCommandBufferCallbackErrorInnocentVictim, 3 Hang, 0xb PageFault
//! (names from macOS log strings, NOT from this binary). So bit1 = GUILTY,
//! and native fails every listed non-guilty command with InnocentVictim --
//! per command buffer, no replay seen in this function. Linux captures agree:
//! bit1 is set exactly on the queue the host's blamed stamp names in 4/4
//! recoveries. bit2 is set on queues the firmware cancelled, bit3 on TA
//! queues (captures; native only forwards bit2 into its restart report).
//!
//!   bit0 INNOCENT: classify every in-flight completion at each recovery.
//!        A VM owns none of the guilty queues (blamed stamp + entries with
//!        bit1) -> its cancelled work fails with an error fence ONLY: no
//!        sticky VM status, no VM-wide fan-out. Its compute queue, once the
//!        existing firmware-retirement witness proves the cancelled kicks
//!        retired, returns to service instead of staying quarantined for the
//!        boot. Unresolvable blame (a guilty qid whose owner cannot be found)
//!        disables isolation for that recovery: today's behaviour.
//!   bit1 ADMISSION_WAIT: a VM whose status is clean but whose mappings are
//!        transiently blocked by retained (quarantined) work waits up to
//!        ADMISSION_WAIT_MS for the release instead of refusing VM_BIND, and
//!        a compute submit waits up to SUBMIT_WAIT_MS for the retirement
//!        witness of its own spared queue. Poisoned VMs still refuse at once.
//!   bit2 SETTLE_CANCELLED: spared renders the firmware listed as CANCELLED
//!        (bit2, not bit1) are settled at the end of the recovery through the
//!        same exact-once settle the blamed slot uses, instead of waiting for
//!        the 2 s scheduler watchdog. A group firmware has not provably
//!        retired keeps its pair quarantined (mappings retained) until the
//!        reclaim witness. Listed-only entries (bit2 clear) are left alone.
//!   bit3 GUILTY_GRACE: the guilty VM is also spared, at most once per
//!        GRACE_WINDOW_NS per VM; a second guilty recovery inside the window
//!        poisons it as today. Opt-in: this drops the guilty submission
//!        silently instead of reporting device loss.
//!   bit4 RESUME_DOORBELL (opt-in, A/B): when spared render work the firmware
//!        did not cancel is still pending after the recovery, send ONE paired
//!        render doorbell (the message every render submission ends with;
//!        it names no queue) so firmware rescans its rings. 02:24 evidence:
//!        Hyprland's parked 3D (entry flags 0x1, not cancelled) never ran in
//!        the 2 s after the recovery. Whether a doorbell activates it is the
//!        question this bit answers; it is not established.
//!
//! Replay of cancelled work is deliberately NOT done here (see the report):
//! the firmware state of a cancelled QID after recovery and the work-state
//! accounting of a cancelled kick are not yet established on hardware. The
//! `first-kicks-*` counters measure the former on every returned queue.
//!
//! Memory safety is unchanged in every mode: packets, VM-job guards and
//! quarantined pairs are retained and released only by the existing
//! witnesses (exact retirement, firmware drain/reclaim, processor stop).
use core::fmt::{self, Write};
use core::sync::atomic::{AtomicU32, AtomicU64, Ordering};
#[cfg(not(test))]
use kernel::{bindings, module_param::KernelParam};

pub(crate) const INNOCENT: u32 = 1 << 0;
pub(crate) const ADMISSION_WAIT: u32 = 1 << 1;
pub(crate) const SETTLE_CANCELLED: u32 = 1 << 2;
pub(crate) const GUILTY_GRACE: u32 = 1 << 3;
pub(crate) const RESUME_DOORBELL: u32 = 1 << 4;
const ALL_BITS: u32 = INNOCENT | ADMISSION_WAIT | SETTLE_CANCELLED | GUILTY_GRACE | RESUME_DOORBELL;
/// Correctness/containment fixes with no hot-path cost are on; the guilty
/// grace changes the guilty client's semantics and stays opt-in.
pub(crate) const DEFAULT_MASK: u32 = INNOCENT | ADMISSION_WAIT | SETTLE_CANCELLED;

pub(crate) const ADMISSION_WAIT_MS: u32 = 1000;
pub(crate) const SUBMIT_WAIT_MS: u32 = 200;
pub(crate) const GRACE_WINDOW_NS: u64 = 60_000_000_000;

static MASK: AtomicU32 = AtomicU32::new(DEFAULT_MASK);

#[inline(always)]
pub(crate) fn enabled(bit: u32) -> bool { MASK.load(Ordering::Relaxed) & bit != 0 }

/// Per-completion classification, written only while servicing a recovery.
pub(crate) const CLASS_NONE: u8 = 0;
/// Cancelled by another client's recovery (or graced): fail the fence only.
pub(crate) const CLASS_SPARED: u8 = 1;
/// Owned by the guilty VM: today's behaviour.
pub(crate) const CLASS_GUILTY: u8 = 2;

pub(crate) const ENTRY_VALID: u8 = 1 << 0;
pub(crate) const ENTRY_GUILTY: u8 = 1 << 1;
pub(crate) const ENTRY_CANCELLED: u8 = 1 << 2;

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) struct RecoveryEntry {
    pub(crate) qid: u8,
    pub(crate) flags: u8,
    pub(crate) stamp: u32,
}

/// Decode one recovery-info table entry (see the module comment).
pub(crate) const fn decode_entry(word0: u32, word1: u32) -> Option<RecoveryEntry> {
    if word0 & 1 == 0 { return None; }
    // Native: ubfx #4, #7.
    let qid = (word0 >> 4) & 0x7f;
    Some(RecoveryEntry { qid: qid as u8, flags: (word0 & 0xf) as u8, stamp: word1 >> 8 })
}

/// QID masks of a decoded table plus the blamed stamp:
/// (guilty, listed, cancelled). The blamed stamp's QID counts as guilty.
pub(crate) fn qid_masks(entries: &[RecoveryEntry], blamed: Option<u8>) -> (u128, u128, u128) {
    let mut guilty = 0u128;
    let mut listed = 0u128;
    let mut cancelled = 0u128;
    for entry in entries {
        let bit = 1u128 << (entry.qid & 0x7f);
        listed |= bit;
        if entry.flags & ENTRY_GUILTY != 0 { guilty |= bit; }
        if entry.flags & ENTRY_CANCELLED != 0 { cancelled |= bit; }
    }
    if let Some(qid) = blamed.filter(|qid| *qid < 128) { guilty |= 1u128 << qid; }
    (guilty, listed, cancelled)
}

/// Class of one VM for one recovery. Returns (class, grace consumed).
pub(crate) const fn classify(guilty: bool, grace: bool, now_ns: u64, last_grace_ns: u64) -> (u8, bool) {
    if !guilty { return (CLASS_SPARED, false); }
    if grace && (last_grace_ns == 0 || now_ns.saturating_sub(last_grace_ns) >= GRACE_WINDOW_NS) {
        return (CLASS_SPARED, true);
    }
    (CLASS_GUILTY, false)
}

// ---- recovery-info snapshot ------------------------------------------------

pub(crate) const MAX_ENTRIES: usize = 64;
static ENTRY_WORDS: [AtomicU64; MAX_ENTRIES] = [const { AtomicU64::new(0) }; MAX_ENTRIES];
static ENTRY_COUNT: AtomicU32 = AtomicU32::new(0);

/// Forget the previous recovery's table before reading the new one.
pub(crate) fn clear_entries() { ENTRY_COUNT.store(0, Ordering::Release); }

/// Store the raw (word0, word1) of every VALID entry, in table order.
pub(crate) fn store_entries(raw: &[(u32, u32)]) {
    let count = raw.len().min(MAX_ENTRIES);
    for (slot, &(word0, word1)) in ENTRY_WORDS.iter().zip(raw.iter()) {
        slot.store(u64::from(word0) | u64::from(word1) << 32, Ordering::Relaxed);
    }
    ENTRY_COUNT.store(count as u32, Ordering::Release);
}

/// Decoded entries of the last stored table.
pub(crate) fn entries(out: &mut [RecoveryEntry; MAX_ENTRIES]) -> usize {
    let count = (ENTRY_COUNT.load(Ordering::Acquire) as usize).min(MAX_ENTRIES);
    let mut used = 0;
    for slot in ENTRY_WORDS.iter().take(count) {
        let raw = slot.load(Ordering::Relaxed);
        if let Some(entry) = decode_entry(raw as u32, (raw >> 32) as u32) {
            out[used] = entry;
            used += 1;
        }
    }
    used
}

// ---- counters ----------------------------------------------------------------

pub(crate) static RECOVERIES: AtomicU64 = AtomicU64::new(0);
pub(crate) static UNRESOLVED: AtomicU64 = AtomicU64::new(0);
pub(crate) static SPARED_RENDERS: AtomicU64 = AtomicU64::new(0);
pub(crate) static SPARED_COMPUTE: AtomicU64 = AtomicU64::new(0);
pub(crate) static GUILTY_MARKED: AtomicU64 = AtomicU64::new(0);
pub(crate) static GRACED: AtomicU64 = AtomicU64::new(0);
pub(crate) static POISON_SKIPPED: AtomicU64 = AtomicU64::new(0);
pub(crate) static FANOUT_SKIPPED: AtomicU64 = AtomicU64::new(0);
pub(crate) static SETTLED_CANCELLED: AtomicU64 = AtomicU64::new(0);
pub(crate) static QUEUES_RETURNED: AtomicU64 = AtomicU64::new(0);
pub(crate) static QUEUE_RETURN_FAILED: AtomicU64 = AtomicU64::new(0);
pub(crate) static FIRST_KICKS_OK: AtomicU64 = AtomicU64::new(0);
pub(crate) static FIRST_KICKS_FAILED: AtomicU64 = AtomicU64::new(0);
pub(crate) static ADMISSION_WAITS: AtomicU64 = AtomicU64::new(0);
pub(crate) static ADMISSION_TIMEOUTS: AtomicU64 = AtomicU64::new(0);
pub(crate) static SUBMIT_WAITS: AtomicU64 = AtomicU64::new(0);
pub(crate) static SUBMIT_TIMEOUTS: AtomicU64 = AtomicU64::new(0);
pub(crate) static RESUME_DOORBELLS: AtomicU64 = AtomicU64::new(0);

#[inline(always)]
pub(crate) fn count(counter: &AtomicU64) { counter.fetch_add(1, Ordering::Relaxed); }

static COUNTERS: [(&str, &AtomicU64); 18] = [
    ("recoveries", &RECOVERIES), ("unresolved", &UNRESOLVED),
    ("spared-renders", &SPARED_RENDERS), ("spared-compute", &SPARED_COMPUTE),
    ("guilty-marked", &GUILTY_MARKED), ("graced", &GRACED),
    ("poison-skipped", &POISON_SKIPPED), ("fanout-skipped", &FANOUT_SKIPPED),
    ("settled-cancelled", &SETTLED_CANCELLED), ("queues-returned", &QUEUES_RETURNED),
    ("queue-return-failed", &QUEUE_RETURN_FAILED), ("first-kicks-ok", &FIRST_KICKS_OK),
    ("first-kicks-failed", &FIRST_KICKS_FAILED), ("admission-waits", &ADMISSION_WAITS),
    ("admission-timeouts", &ADMISSION_TIMEOUTS), ("submit-waits", &SUBMIT_WAITS),
    ("submit-timeouts", &SUBMIT_TIMEOUTS), ("resume-doorbells", &RESUME_DOORBELLS),
];

// ---- parameter -----------------------------------------------------------------

struct Output<'a> { bytes: &'a mut [u8], used: usize }
impl Write for Output<'_> {
    fn write_str(&mut self, value: &str) -> fmt::Result {
        if value.len() > self.bytes.len() - self.used { return Err(fmt::Error); }
        self.bytes[self.used..self.used + value.len()].copy_from_slice(value.as_bytes());
        self.used += value.len(); Ok(())
    }
}

pub(crate) fn parse_mask(text: &str) -> Option<u32> {
    let parsed = match text.strip_prefix("0x") {
        Some(hex) => u32::from_str_radix(hex, 16).ok(),
        None => text.parse::<u32>().ok(),
    }?;
    (parsed & !ALL_BITS == 0).then_some(parsed)
}

#[cfg(not(test))]
unsafe extern "C" fn mask_set(value: *const core::ffi::c_char, _: *const bindings::kernel_param) -> core::ffi::c_int {
    if value.is_null() { return -22; }
    // SAFETY: the parameter core supplies a NUL-terminated string.
    let Ok(text) = unsafe { core::ffi::CStr::from_ptr(value) }.to_str() else { return -22; };
    let text = text.trim();
    if text == "reset" {
        for (_, counter) in COUNTERS.iter() { counter.store(0, Ordering::Relaxed); }
        return 0;
    }
    let Some(mask) = parse_mask(text) else { return -22; };
    MASK.store(mask, Ordering::Release);
    0
}

#[cfg(not(test))]
unsafe extern "C" fn mask_get(buf: *mut core::ffi::c_char, _: *const bindings::kernel_param) -> core::ffi::c_int {
    if buf.is_null() { return -22; }
    // SAFETY: sysfs parameter output is PAGE_SIZE, at least 4096 bytes.
    let bytes = unsafe { core::slice::from_raw_parts_mut(buf.cast::<u8>(), 4000) };
    let mut out = Output { bytes, used: 0 };
    let _ = write!(out, "{:#x}", MASK.load(Ordering::Relaxed));
    for (name, counter) in COUNTERS.iter() {
        let _ = write!(out, " {}={}", name, counter.load(Ordering::Relaxed));
    }
    let _ = writeln!(out);
    out.used as core::ffi::c_int
}

#[cfg(not(test))]
static MASK_OPS: bindings::kernel_param_ops = bindings::kernel_param_ops {
    flags: 0, set: Some(mask_set), get: Some(mask_get), free: None,
};
#[cfg(not(test))]
#[link_section = "__param"]
#[used]
static MASK_PARAM: KernelParam = KernelParam::new(bindings::kernel_param {
    name: kernel::str::as_char_ptr_in_const_context(if cfg!(MODULE) {
        c"g17p_recovery_isolation"
    } else { c"asahi.g17p_recovery_isolation" }),
    #[cfg(MODULE)]
    mod_: unsafe { core::ptr::from_ref(&bindings::__this_module).cast_mut() },
    #[cfg(not(MODULE))]
    mod_: core::ptr::null_mut(),
    ops: core::ptr::from_ref(&MASK_OPS), perm: 0o600, level: -1, flags: 0,
    __bindgen_anon_1: bindings::kernel_param__bindgen_ty_1 { arg: core::ptr::null_mut() },
});
