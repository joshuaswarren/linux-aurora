// SPDX-License-Identifier: GPL-2.0-only OR MIT
//! Firmware queue-slot accounting (`g17p_queue_slots`, root-only, read-only
//! sysfs). Counter-only: relaxed atomics, no printk.
//!
//! Every render pair and compute queue leases QIDs from one 128-entry table
//! (`g17_channel`). A published QID is kept until processor stop, and a
//! quarantined or exit-killed compute graph is never handed out again, so a
//! boot can run out of QIDs while memory is fine. Queue creation then fails
//! with ENOSPC, and so does a new render pair (`render-warm-pairs`). The
//! `qids` fields are exact (kept by the table itself); the `compute`,
//! `teardowns` and `render_quarantined` fields are a snapshot taken on every
//! teardown turn and on every queue creation.
use core::fmt::Write;
use core::sync::atomic::{AtomicU32, Ordering};
use kernel::{bindings, module_param::KernelParam};

use crate::g17_channel::{self, QID_COUNT};

static COMPUTE_GRAPHS: AtomicU32 = AtomicU32::new(0);
static COMPUTE_OWNED: AtomicU32 = AtomicU32::new(0);
static COMPUTE_RELEASED: AtomicU32 = AtomicU32::new(0);
static COMPUTE_QUARANTINED: AtomicU32 = AtomicU32::new(0);
static COMPUTE_EXIT_KILLED: AtomicU32 = AtomicU32::new(0);
static RETIRED_GRAPHS: AtomicU32 = AtomicU32::new(0);
static TEARDOWNS: AtomicU32 = AtomicU32::new(0);
static RENDER_QUARANTINED: AtomicU32 = AtomicU32::new(0);

/// Snapshot of the runtime's queue-slot holders.
pub(crate) struct Snapshot {
    pub(crate) compute_graphs: usize,
    pub(crate) compute_owned: usize,
    pub(crate) compute_released: usize,
    pub(crate) compute_quarantined: usize,
    pub(crate) compute_exit_killed: usize,
    pub(crate) retired_graphs: usize,
    pub(crate) teardowns: usize,
    pub(crate) render_quarantined: usize,
}

pub(crate) fn note(snapshot: Snapshot) {
    let set = |gauge: &AtomicU32, value: usize| {
        gauge.store(u32::try_from(value).unwrap_or(u32::MAX), Ordering::Relaxed)
    };
    set(&COMPUTE_GRAPHS, snapshot.compute_graphs);
    set(&COMPUTE_OWNED, snapshot.compute_owned);
    set(&COMPUTE_RELEASED, snapshot.compute_released);
    set(&COMPUTE_QUARANTINED, snapshot.compute_quarantined);
    set(&COMPUTE_EXIT_KILLED, snapshot.compute_exit_killed);
    set(&RETIRED_GRAPHS, snapshot.retired_graphs);
    set(&TEARDOWNS, snapshot.teardowns);
    set(&RENDER_QUARANTINED, snapshot.render_quarantined);
}

struct Output<'a> { bytes: &'a mut [u8], used: usize }
impl Write for Output<'_> {
    fn write_str(&mut self, value: &str) -> core::fmt::Result {
        if value.len() > self.bytes.len() - self.used { return Err(core::fmt::Error); }
        self.bytes[self.used..self.used + value.len()].copy_from_slice(value.as_bytes());
        self.used += value.len();
        Ok(())
    }
}

unsafe extern "C" fn slots_get(buf: *mut core::ffi::c_char, _: *const bindings::kernel_param) -> core::ffi::c_int {
    if buf.is_null() { return -22; }
    // SAFETY: sysfs parameter output is PAGE_SIZE, at least 4096 bytes.
    let bytes = unsafe { core::slice::from_raw_parts_mut(buf.cast::<u8>(), 1024) };
    let mut out = Output { bytes, used: 0 };
    let r = |c: &AtomicU32| c.load(Ordering::Relaxed);
    let leased = g17_channel::leased_total();
    let published: u32 = g17_channel::PUBLISHED.iter().map(r).sum();
    let _ = writeln!(out,
        "qids leased={} free={} published={} tiling={} fragment={} compute={} high_water={} exhausted={} \
         compute graphs={} owned={} released={} quarantined={} exit_killed={} retired_graphs={} \
         teardowns={} render_quarantined={}",
        leased, (QID_COUNT as u32).saturating_sub(leased), published,
        r(&g17_channel::LEASED[0]), r(&g17_channel::LEASED[1]), r(&g17_channel::LEASED[2]),
        r(&g17_channel::HIGH_WATER), g17_channel::EXHAUSTED.load(Ordering::Relaxed),
        r(&COMPUTE_GRAPHS), r(&COMPUTE_OWNED), r(&COMPUTE_RELEASED), r(&COMPUTE_QUARANTINED),
        r(&COMPUTE_EXIT_KILLED), r(&RETIRED_GRAPHS), r(&TEARDOWNS), r(&RENDER_QUARANTINED));
    out.used as core::ffi::c_int
}

unsafe extern "C" fn slots_set(_: *const core::ffi::c_char, _: *const bindings::kernel_param) -> core::ffi::c_int {
    -1 // EPERM: read-only counters
}

static SLOTS_OPS: bindings::kernel_param_ops = bindings::kernel_param_ops {
    flags: 0, set: Some(slots_set), get: Some(slots_get), free: None,
};
#[link_section = "__param"]
#[used]
static SLOTS_PARAM: KernelParam = KernelParam::new(bindings::kernel_param {
    name: kernel::str::as_char_ptr_in_const_context(if cfg!(MODULE) {
        c"g17p_queue_slots"
    } else { c"asahi.g17p_queue_slots" }),
    #[cfg(MODULE)]
    mod_: unsafe { core::ptr::from_ref(&bindings::__this_module).cast_mut() },
    #[cfg(not(MODULE))]
    mod_: core::ptr::null_mut(),
    ops: core::ptr::from_ref(&SLOTS_OPS), perm: 0o400, level: -1, flags: 0,
    __bindgen_anon_1: bindings::kernel_param__bindgen_ty_1 { arg: core::ptr::null_mut() },
});
