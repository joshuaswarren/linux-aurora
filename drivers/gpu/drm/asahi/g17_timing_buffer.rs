// SPDX-License-Identifier: GPL-2.0-only OR MIT
//! One-shot diagnostic buffer. No allocation, printk, waiting or overwrite.
//! Atomic words avoid unsafe shared payload access; ready publishes each slot.
use core::sync::atomic::{AtomicU32, AtomicU64, AtomicUsize, Ordering};
use core::fmt::{self, Write};
use kernel::{bindings, module_param::KernelParam};

pub(crate) const WORDS: usize = 24;
const CAPACITY: usize = 16384;
struct Slot { ready: AtomicU32, words: [AtomicU64; WORDS] }
static SLOTS: [Slot; CAPACITY] = [const { Slot {
    ready: AtomicU32::new(0), words: [const { AtomicU64::new(0) }; WORDS],
} }; CAPACITY];
// 0 unarmed, 1 capturing, 2 permanently frozen until module unload.
static STATE: AtomicU32 = AtomicU32::new(0);
// Freeze and reservation share one atomic: stop cannot miss a late reservation.
const FROZEN: usize = 1usize << (usize::BITS - 1);
static NEXT: AtomicUsize = AtomicUsize::new(FROZEN);
static FROZEN_BOUND: AtomicUsize = AtomicUsize::new(0);
static CURSOR: AtomicUsize = AtomicUsize::new(0);
static DROPPED: AtomicU64 = AtomicU64::new(0);

pub(crate) fn enabled() -> bool { STATE.load(Ordering::Relaxed) == 1 }
pub(crate) fn now() -> u64 {
    <kernel::time::Monotonic as kernel::time::ClockSource>::ktime_get() as u64
}
pub(crate) fn record(words: [u64; WORDS]) {
    if !enabled() { return; }
    let next = NEXT.load(Ordering::Relaxed);
    if next & FROZEN != 0 { return; }
    if next >= CAPACITY { DROPPED.fetch_add(1, Ordering::Relaxed); return; }
    // A freeze racing this reservation either includes this ticket in its
    // finite drain bound, or this ticket observes FROZEN and writes nothing.
    let index = NEXT.fetch_add(1, Ordering::Relaxed);
    if index & FROZEN != 0 { return; }
    if index >= CAPACITY { DROPPED.fetch_add(1, Ordering::Relaxed); return; }
    let slot = &SLOTS[index];
    for (dst, src) in slot.words.iter().zip(words) { dst.store(src, Ordering::Relaxed); }
    slot.ready.store(1, Ordering::Release);
}

fn reserved() -> usize {
    if STATE.load(Ordering::Acquire) == 2 { FROZEN_BOUND.load(Ordering::Relaxed) }
    else { (NEXT.load(Ordering::Relaxed) & !FROZEN).min(CAPACITY) }
}

// Limit each sysfs response to 4095 bytes, below PAGE_SIZE even on 4K kernels.
struct Output<'a> { bytes: &'a mut [u8], used: usize }
impl Write for Output<'_> {
    fn write_str(&mut self, value: &str) -> fmt::Result {
        if value.len() > self.bytes.len() - self.used { return Err(fmt::Error); }
        self.bytes[self.used..self.used + value.len()].copy_from_slice(value.as_bytes());
        self.used += value.len(); Ok(())
    }
}
unsafe extern "C" fn get(buf: *mut core::ffi::c_char, _: *const bindings::kernel_param) -> core::ffi::c_int {
    // SAFETY: sysfs module parameter get supplies a PAGE_SIZE output buffer.
    let bytes = unsafe { core::slice::from_raw_parts_mut(buf.cast::<u8>(), 4095) };
    let mut out = Output { bytes, used: 0 };
    let mut cursor = CURSOR.load(Ordering::Relaxed);
    // The kernel parameter mutex serializes readers/setters, never writers.
    // Four records fit even when every u64 takes 20 decimal digits.
    for _ in 0..4 {
        if cursor >= reserved() { break; }
        let slot = &SLOTS[cursor];
        if slot.ready.load(Ordering::Acquire) == 0 { break; }
        let _ = write!(out, "record {}", cursor);
        for word in &slot.words { let _ = write!(out, " {}", word.load(Ordering::Relaxed)); }
        let _ = writeln!(out);
        cursor += 1;
    }
    CURSOR.store(cursor, Ordering::Relaxed);
    let reserved = reserved();
    let _ = writeln!(out, "status state={} cursor={} reserved={} dropped={} capacity={}",
        STATE.load(Ordering::Relaxed), cursor, reserved, DROPPED.load(Ordering::Relaxed), CAPACITY);
    out.used as core::ffi::c_int
}
unsafe extern "C" fn set(value: *const core::ffi::c_char, _: *const bindings::kernel_param) -> core::ffi::c_int {
    if value.is_null() { return -22; }
    // Bounded parser avoids strlen and rejects commands longer than 7 bytes.
    let mut command = [0u8; 8];
    let mut len = 0;
    while len < command.len() {
        // SAFETY: parameter core supplies a NUL-terminated value; never read
        // beyond the first NUL, and at most eight bytes are examined.
        let byte = unsafe { *value.add(len) } as u8;
        if byte == 0 { break; }
        command[len] = byte;
        len += 1;
    }
    if len == command.len() { return -22; }
    let arg = &command[..len];
    match arg {
        b"1" | b"1\n" => {
            if *crate::module_parameters::g17p_completion_trace.value() != 0
                || *crate::module_parameters::g17p_trace_depth.value() != 0 { return -22; }
            if STATE.load(Ordering::Relaxed) != 0 { return -16; }
            NEXT.store(0, Ordering::Relaxed);
            STATE.store(1, Ordering::Relaxed);
        }
        b"0" | b"0\n" => {
            if STATE.load(Ordering::Relaxed) != 2 {
                FROZEN_BOUND.store((NEXT.fetch_or(FROZEN, Ordering::Relaxed) & !FROZEN).min(CAPACITY), Ordering::Relaxed);
                STATE.store(2, Ordering::Release);
            }
        }
        // Rewind readout only. Slots remain immutable, including while capturing.
        b"rewind" | b"rewind\n" => { CURSOR.store(0, Ordering::Relaxed); }
        _ => return -22,
    }
    0
}
static OPS: bindings::kernel_param_ops = bindings::kernel_param_ops {
    flags: 0, set: Some(set), get: Some(get), free: None,
};
#[link_section = "__param"]
#[used]
static PARAM: KernelParam = KernelParam::new(bindings::kernel_param {
    name: kernel::str::as_char_ptr_in_const_context(if cfg!(MODULE) {
        c"g17p_timing_buffer"
    } else { c"asahi.g17p_timing_buffer" }),
    #[cfg(MODULE)]
    mod_: unsafe { core::ptr::from_ref(&bindings::__this_module).cast_mut() },
    #[cfg(not(MODULE))]
    mod_: core::ptr::null_mut(),
    ops: core::ptr::from_ref(&OPS), perm: 0o600, level: -1, flags: 0,
    __bindgen_anon_1: bindings::kernel_param__bindgen_ty_1 { arg: core::ptr::null_mut() },
});
