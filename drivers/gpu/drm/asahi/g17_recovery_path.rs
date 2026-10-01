// SPDX-License-Identifier: GPL-2.0-only OR MIT
//! v93r post-recovery controls and control-ring diagnostics (`g17p_recovery_path`).
//!
//! Root-only sysfs, runtime-writable bitmask, also settable at load. Default
//! 0 is exactly v93f.
//!   bit0 RECLAIM_DRAINED: after a firmware stamp-blamed recovery the render
//!        whose group could not be proven retired, and every render queued
//!        behind it on the same physical pair, are failed with their guards
//!        retained (`fail_while_owned`) and the pair is quarantined until both
//!        firmware processors stop. With this bit, once the recovery is
//!        closed (host and firmware handshake state 0) and the firmware has
//!        demonstrably drained that pair -- both engines' post-maintenance
//!        completion stamps cover the pair's last published ordinal and the
//!        pair FList's completed-kick counter equals its submitted counter --
//!        the event worker retires the pair's trackers through the normal
//!        paired-completion transaction (or, for a group the recovery
//!        skipped, the existing firmware-retired classifier), releases the
//!        retained VM-job and lease guards, and returns the pair. The client's
//!        VM stays poisoned; only the physical pair, its FList/HWPB state and
//!        the closed queue's teardown are recovered.
//!   bit1 CONTROL_TRACE: remember opcode/argument/publish time of every
//!        primary device-control record and the time of the last EP 0x21
//!        announcement; a control re-announce then names the oldest pending
//!        record and whether it had been announced after its publication.
//!   bit2 GROW_REKICK (v96): firmware event 6 (TVB grow) pauses the pool's
//!        allocator until the opcode-8 reply. A request whose owning pair is
//!        borrowed (`preparing`: warm-up, restage, grow, completion
//!        observation) is parked, and the grow worker exits. Without this bit
//!        only the next firmware event re-queues the worker; if the stalled
//!        render was the only producer of events, nothing does. With this
//!        bit, every end of a borrow (and every clearing of control-ring
//!        backpressure) re-queues the worker while a request is parked.
//! Grow ingress, deferral, re-kick and recovery-time grow state are always
//! logged (rate-limited) and counted, independent of the mask.
use core::fmt::{self, Write};
use core::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use kernel::{bindings, module_param::KernelParam};

pub(crate) const RECLAIM_DRAINED: u32 = 1 << 0;
pub(crate) const CONTROL_TRACE: u32 = 1 << 1;
pub(crate) const GROW_REKICK: u32 = 1 << 2;
const ALL_BITS: u32 = (1 << 3) - 1;

static MASK: AtomicU32 = AtomicU32::new(0);

#[inline(always)]
pub(crate) fn enabled(bit: u32) -> bool { MASK.load(Ordering::Relaxed) & bit != 0 }

/// Why a reclaim attempt for one quarantined pair stopped (counters and log).
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) enum ReclaimBlock {
    /// A firmware recovery is open or the handshake state is not 0.
    Recovery,
    /// The pair is being prepared by a publisher.
    Preparing,
    /// A completion stamp does not yet cover the last published ordinal.
    Stamps,
    /// The pair FList still has submitted kicks not completed.
    Flist,
    /// A tracked group is not yet provably retired.
    Tracker,
    /// A step returned an error; the pair stays quarantined.
    Error,
}

impl ReclaimBlock {
    const fn index(self) -> usize {
        match self {
            Self::Recovery => 0, Self::Preparing => 1, Self::Stamps => 2,
            Self::Flist => 3, Self::Tracker => 4, Self::Error => 5,
        }
    }
    const NAMES: [&'static str; 6] = ["recovery", "preparing", "stamps", "flist", "tracker", "error"];
}

/// Attempts, reclaimed pairs, retired trackers, released completions.
pub(crate) static RECLAIM_ATTEMPTS: AtomicU64 = AtomicU64::new(0);
pub(crate) static RECLAIM_PAIRS: AtomicU64 = AtomicU64::new(0);
pub(crate) static RECLAIM_TRACKERS: AtomicU64 = AtomicU64::new(0);
pub(crate) static RECLAIM_RELEASED: AtomicU64 = AtomicU64::new(0);
static RECLAIM_BLOCKED: [AtomicU64; 6] = [const { AtomicU64::new(0) }; 6];

/// v96 TVB grow accounting: firmware requests received, requests left parked
/// because their pair was borrowed when the worker looked, borrow ends that
/// found a parked request, and worker re-queues issued for them (bit2).
pub(crate) static GROW_INGRESS: AtomicU64 = AtomicU64::new(0);
pub(crate) static GROW_DEFERRED: AtomicU64 = AtomicU64::new(0);
pub(crate) static GROW_PARKED_AT_RETURN: AtomicU64 = AtomicU64::new(0);
pub(crate) static GROW_REKICKS: AtomicU64 = AtomicU64::new(0);
pub(crate) static GROW_REPLIED: AtomicU64 = AtomicU64::new(0);

/// Handshake steps 5-6: the last opcode-8 grow reply published to the
/// primary control ring (bit63 valid | producer_after << 32 | buffer_slot)
/// and its CNTPCT, until the firmware consumer is observed past it.
pub(crate) static GROW_REPLY_WATCH: AtomicU64 = AtomicU64::new(0);
pub(crate) static GROW_REPLY_AT: AtomicU64 = AtomicU64::new(0);
pub(crate) static GROW_REPLY_CONSUMED: AtomicU64 = AtomicU64::new(0);

pub(crate) fn watch_grow_reply(producer_after: u32, buffer_slot: u32) {
    GROW_REPLY_AT.store(crate::g17_timing_buffer::arch_counter(), Ordering::Relaxed);
    GROW_REPLY_WATCH.store(1 << 63 | u64::from(producer_after) << 32 | u64::from(buffer_slot),
        Ordering::Release);
}

/// A bounded log budget: the first `first` events, then one in `every`.
pub(crate) fn log_budget(counter: u64, first: u64, every: u64) -> bool {
    counter <= first || counter % every == 0
}

pub(crate) fn note_blocked(reason: ReclaimBlock) {
    RECLAIM_BLOCKED[reason.index()].fetch_add(1, Ordering::Relaxed);
}

// ---- control-ring shadow (bit1) -----------------------------------------

const CONTROL_SLOTS: usize = crate::g17_initdata::CONTROL_RING_ENTRIES;
/// opcode | arg << 32, and CNTPCT at publication, per ring slot.
static CONTROL_RECORD: [AtomicU64; CONTROL_SLOTS] = [const { AtomicU64::new(0) }; CONTROL_SLOTS];
static CONTROL_PUBLISHED_AT: [AtomicU64; CONTROL_SLOTS] = [const { AtomicU64::new(0) }; CONTROL_SLOTS];
static LAST_ANNOUNCE: AtomicU64 = AtomicU64::new(0);
/// Re-announces whose oldest pending record was (not) announced after it
/// was published.
static REANNOUNCE_ANNOUNCED: AtomicU64 = AtomicU64::new(0);
static REANNOUNCE_UNANNOUNCED: AtomicU64 = AtomicU64::new(0);
/// Re-announces by the pending record's opcode (0..0x3f, larger folded to 0x3f).
static REANNOUNCE_BY_OPCODE: [AtomicU32; 64] = [const { AtomicU32::new(0) }; 64];

/// Called for every primary device-control publication (ring slot index).
#[inline(always)]
pub(crate) fn note_control_publish(slot: u32, opcode: u32, arg: u32) {
    if !enabled(CONTROL_TRACE) { return; }
    let index = slot as usize % CONTROL_SLOTS;
    CONTROL_RECORD[index].store(u64::from(opcode) | u64::from(arg) << 32, Ordering::Relaxed);
    CONTROL_PUBLISHED_AT[index].store(crate::g17_timing_buffer::arch_counter(), Ordering::Relaxed);
}

/// Called for every EP 0x21 device-control announcement actually sent.
#[inline(always)]
pub(crate) fn note_control_announce() {
    if !enabled(CONTROL_TRACE) { return; }
    LAST_ANNOUNCE.store(crate::g17_timing_buffer::arch_counter(), Ordering::Relaxed);
}

/// The oldest pending record at `consumer`, if traced: (opcode, arg, age
/// in counter ticks, announced after publication).
pub(crate) fn describe_pending(consumer: u32) -> Option<(u32, u32, u64, bool)> {
    if !enabled(CONTROL_TRACE) { return None; }
    let index = consumer as usize % CONTROL_SLOTS;
    let published = CONTROL_PUBLISHED_AT[index].load(Ordering::Relaxed);
    if published == 0 { return None; }
    let record = CONTROL_RECORD[index].load(Ordering::Relaxed);
    let now = crate::g17_timing_buffer::arch_counter();
    let announced = LAST_ANNOUNCE.load(Ordering::Relaxed) >= published;
    let opcode = record as u32;
    let counter = if announced { &REANNOUNCE_ANNOUNCED } else { &REANNOUNCE_UNANNOUNCED };
    counter.fetch_add(1, Ordering::Relaxed);
    REANNOUNCE_BY_OPCODE[(opcode as usize).min(63)].fetch_add(1, Ordering::Relaxed);
    Some((opcode, (record >> 32) as u32, now.saturating_sub(published), announced))
}

// ---- parameter ------------------------------------------------------------

struct Output<'a> { bytes: &'a mut [u8], used: usize }
impl Write for Output<'_> {
    fn write_str(&mut self, value: &str) -> fmt::Result {
        if value.len() > self.bytes.len() - self.used { return Err(fmt::Error); }
        self.bytes[self.used..self.used + value.len()].copy_from_slice(value.as_bytes());
        self.used += value.len(); Ok(())
    }
}

/// Accepts "reset" or a decimal/0x-hex mask of known bits.
fn parse_mask(text: &str) -> Option<u32> {
    let parsed = match text.strip_prefix("0x") {
        Some(hex) => u32::from_str_radix(hex, 16).ok(),
        None => text.parse::<u32>().ok(),
    }?;
    (parsed & !ALL_BITS == 0).then_some(parsed)
}

unsafe extern "C" fn mask_set(value: *const core::ffi::c_char, _: *const bindings::kernel_param) -> core::ffi::c_int {
    if value.is_null() { return -22; }
    // SAFETY: the parameter core supplies a NUL-terminated string.
    let Ok(text) = unsafe { core::ffi::CStr::from_ptr(value) }.to_str() else { return -22; };
    let text = text.trim();
    if text == "reset" {
        for counter in [&RECLAIM_ATTEMPTS, &RECLAIM_PAIRS, &RECLAIM_TRACKERS, &RECLAIM_RELEASED,
                        &REANNOUNCE_ANNOUNCED, &REANNOUNCE_UNANNOUNCED, &GROW_INGRESS,
                        &GROW_DEFERRED, &GROW_PARKED_AT_RETURN, &GROW_REKICKS, &GROW_REPLIED,
                        &GROW_REPLY_CONSUMED] {
            counter.store(0, Ordering::Relaxed);
        }
        for counter in RECLAIM_BLOCKED.iter() { counter.store(0, Ordering::Relaxed); }
        for counter in REANNOUNCE_BY_OPCODE.iter() { counter.store(0, Ordering::Relaxed); }
        return 0;
    }
    let Some(mask) = parse_mask(text) else { return -22; };
    MASK.store(mask, Ordering::Release);
    0
}

unsafe extern "C" fn mask_get(buf: *mut core::ffi::c_char, _: *const bindings::kernel_param) -> core::ffi::c_int {
    if buf.is_null() { return -22; }
    // SAFETY: sysfs parameter output is PAGE_SIZE, at least 4096 bytes.
    let bytes = unsafe { core::slice::from_raw_parts_mut(buf.cast::<u8>(), 4000) };
    let mut out = Output { bytes, used: 0 };
    let _ = write!(out, "{:#x} reclaim attempts={} pairs={} trackers={} released={} blocked:",
        MASK.load(Ordering::Relaxed), RECLAIM_ATTEMPTS.load(Ordering::Relaxed),
        RECLAIM_PAIRS.load(Ordering::Relaxed), RECLAIM_TRACKERS.load(Ordering::Relaxed),
        RECLAIM_RELEASED.load(Ordering::Relaxed));
    for (name, counter) in ReclaimBlock::NAMES.iter().zip(RECLAIM_BLOCKED.iter()) {
        let _ = write!(out, " {}={}", name, counter.load(Ordering::Relaxed));
    }
    let _ = write!(out, " tvb-grow ingress={} replied={} reply-consumed={} deferred={} parked-at-return={} rekicks={}",
        GROW_INGRESS.load(Ordering::Relaxed), GROW_REPLIED.load(Ordering::Relaxed),
        GROW_REPLY_CONSUMED.load(Ordering::Relaxed),
        GROW_DEFERRED.load(Ordering::Relaxed), GROW_PARKED_AT_RETURN.load(Ordering::Relaxed),
        GROW_REKICKS.load(Ordering::Relaxed));
    let _ = write!(out, " reannounce announced={} unannounced={} by-opcode:",
        REANNOUNCE_ANNOUNCED.load(Ordering::Relaxed), REANNOUNCE_UNANNOUNCED.load(Ordering::Relaxed));
    for (opcode, counter) in REANNOUNCE_BY_OPCODE.iter().enumerate() {
        let count = counter.load(Ordering::Relaxed);
        if count != 0 { let _ = write!(out, " {:#x}={}", opcode, count); }
    }
    let _ = writeln!(out);
    out.used as core::ffi::c_int
}

static MASK_OPS: bindings::kernel_param_ops = bindings::kernel_param_ops {
    flags: 0, set: Some(mask_set), get: Some(mask_get), free: None,
};
#[link_section = "__param"]
#[used]
static MASK_PARAM: KernelParam = KernelParam::new(bindings::kernel_param {
    name: kernel::str::as_char_ptr_in_const_context(if cfg!(MODULE) {
        c"g17p_recovery_path"
    } else { c"asahi.g17p_recovery_path" }),
    #[cfg(MODULE)]
    mod_: unsafe { core::ptr::from_ref(&bindings::__this_module).cast_mut() },
    #[cfg(not(MODULE))]
    mod_: core::ptr::null_mut(),
    ops: core::ptr::from_ref(&MASK_OPS), perm: 0o600, level: -1, flags: 0,
    __bindgen_anon_1: bindings::kernel_param__bindgen_ty_1 { arg: core::ptr::null_mut() },
});
