// SPDX-License-Identifier: GPL-2.0-only OR MIT

//! Default-off, allocation-free slow-drain observations. No synchronization decisions.
//!
//! CLOCK_MONOTONIC timestamps include CPU descheduling. Phase totals overlap:
//! object contains exec/unmap/BO-put; close/prepared contain eager cleanup.
//! Logs are bounded independently to 128 slow drain and 128 slow wait records.
//! Counters continue after the cap; logged snapshots become lower bounds.

use core::sync::atomic::{AtomicU64, Ordering};
use kernel::{prelude::*, time::{ClockSource, Monotonic}};

const SLOW_NS: u64 = 1_000_000;
const LOG_LIMIT: u64 = 128;
static SEQUENCE: AtomicU64 = AtomicU64::new(0);
static DRAIN_SLOW: AtomicU64 = AtomicU64::new(0);
static DRAIN_DROPPED: AtomicU64 = AtomicU64::new(0);
static WAIT_SLOW: AtomicU64 = AtomicU64::new(0);
static WAIT_DROPPED: AtomicU64 = AtomicU64::new(0);

fn now() -> u64 { Monotonic::ktime_get() as u64 }

#[derive(Clone, Copy)]
pub(super) struct Snapshot {
    pub(super) flags: u8,
    pub(super) active: usize,
    pub(super) commits: usize,
    pub(super) status: i32,
    pub(super) reported: i32,
    pub(super) close: usize,
    pub(super) unmaps: usize,
    pub(super) objects: usize,
}

#[derive(Clone, Copy)]
pub(super) enum Phase { Close, Prepared, Object, Exec, Unmap, BoPut, OwnerPut, Eager, Tail }

pub(super) struct DrainTrace {
    vm: u64,
    pub(super) seq: u64,
    origin: u8,
    initial: Snapshot,
    start: u64,
    closed_end: u64,
    notified: u64,
    loops: u64,
    accepted: [u64; 3],
    phases: [u64; 9],
    final_flags: u8,
    error: i32,
}

impl DrainTrace {
    pub(super) fn enabled() -> bool { *crate::module_parameters::m5_vm_drain_trace.value() != 0 }

    // Caller checks enabled before constructing the snapshot or touching counters/clocks.
    pub(super) fn begin(vm: u64, origin: u8, initial: Snapshot) -> Self {
        let start = now();
        Self { vm, seq: SEQUENCE.fetch_add(1, Ordering::Relaxed).wrapping_add(1), origin,
            initial, start, closed_end: 0, notified: 0, loops: 0, accepted: [0; 3],
            phases: [0; 9], final_flags: 0, error: 0 }
    }

    pub(super) fn stamp(trace: &Option<Self>) -> u64 {
        if trace.is_some() { now() } else { 0 }
    }
    pub(super) fn ref_stamp(trace: &Option<&mut Self>) -> u64 {
        if trace.is_some() { now() } else { 0 }
    }
    fn record(&mut self, phase: Phase, start: u64) {
        let slot = &mut self.phases[phase as usize];
        *slot = slot.saturating_add(now().saturating_sub(start));
    }
    pub(super) fn add(trace: &mut Option<Self>, phase: Phase, start: u64) {
        if let Some(trace) = trace { trace.record(phase, start); }
    }
    pub(super) fn ref_add(trace: &mut Option<&mut Self>, phase: Phase, start: u64) {
        if let Some(trace) = trace { trace.record(phase, start); }
    }
    pub(super) fn batch(trace: &mut Option<Self>, close: usize, unmaps: usize, objects: usize) {
        if let Some(trace) = trace {
            trace.loops = trace.loops.saturating_add(1);
            for (slot, n) in trace.accepted.iter_mut().zip([close, unmaps, objects]) {
                *slot = slot.saturating_add(n as u64);
            }
        }
    }
    pub(super) fn closed(trace: &mut Option<Self>, blocked: bool, closed: bool, failed: bool, error: i32) {
        if let Some(trace) = trace {
            trace.closed_end = now();
            trace.final_flags = blocked as u8 | ((closed as u8) << 1) | ((failed as u8) << 2);
            trace.error = error;
        }
    }
    pub(super) fn notified(trace: &mut Option<Self>) {
        if let Some(trace) = trace { trace.notified = now(); }
    }
    pub(super) fn finish(trace: Option<Self>) {
        let Some(t) = trace else { return; };
        let end = now();
        let closed = t.closed_end.saturating_sub(t.start);
        let tail = t.phases[Phase::Tail as usize];
        // Include slow post-notify destruction even when admission closure was short.
        if closed < SLOW_NS && tail < SLOW_NS { return; }
        let seen = DRAIN_SLOW.fetch_add(1, Ordering::Relaxed).wrapping_add(1);
        if seen > LOG_LIMIT {
            DRAIN_DROPPED.fetch_add(1, Ordering::Relaxed);
            return;
        }
        let new_close = t.accepted[0].saturating_sub(t.initial.close as u64);
        let new_unmaps = t.accepted[1].saturating_sub(t.initial.unmaps as u64);
        let new_objects = t.accepted[2].saturating_sub(t.initial.objects as u64);
        pr_info!("asahi-vmdrain vm={} seq={} origin={} start={} closed_end={} notified={} end={} flags={} active={} commits={} status={} reported={} close={} unmaps={} objects={} loops={} newclose={} newunmaps={} newobjects={} final={} error={}\n",
            t.vm, t.seq, t.origin, t.start, t.closed_end, t.notified, end, t.initial.flags,
            t.initial.active, t.initial.commits, t.initial.status, t.initial.reported,
            t.initial.close, t.initial.unmaps, t.initial.objects, t.loops,
            new_close, new_unmaps, new_objects, t.final_flags, t.error);
        pr_info!("asahi-vmdrain-phase vm={} seq={} closed_ns={} close_ns={} prepared_ns={} object_ns={} exec_ns={} unmap_ns={} boput_ns={} ownerput_ns={} eager_ns={} tail_ns={} slow_seen={} dropped={} cap={} capped={}\n",
            t.vm, t.seq, closed, t.phases[0], t.phases[1], t.phases[2], t.phases[3],
            t.phases[4], t.phases[5], t.phases[6], t.phases[7], t.phases[8], seen,
            DRAIN_DROPPED.load(Ordering::Relaxed), LOG_LIMIT, (seen == LOG_LIMIT) as u8);
    }
}

/// Declared before the lifetime guard so summary logging follows its unlock.
/// Times only actual untimed draining waits, including mutex reacquisition.
pub(super) struct WaitTrace {
    enabled: bool,
    vm: u64,
    pid: i32,
    tid: i32,
    first_seq: u64,
    last_seq: u64,
    start: u64,
    end: u64,
    total: u64,
    count: u64,
    signaled: bool,
}
impl WaitTrace {
    pub(super) fn new(vm: u64) -> Self {
        let enabled = DrainTrace::enabled();
        let (pid, tid) = if enabled {
            let task = kernel::current!();
            (task.group_leader().pid(), task.pid())
        } else { (0, 0) };
        Self { enabled, vm, pid, tid, first_seq: 0, last_seq: 0,
            start: 0, end: 0, total: 0, count: 0, signaled: false }
    }
    pub(super) fn before_wait(&mut self, seq: u64) -> u64 {
        if !self.enabled { return 0; }
        let start = now();
        if self.count == 0 { self.start = start; self.first_seq = seq; }
        self.last_seq = seq;
        start
    }
    pub(super) fn after_wait(&mut self, start: u64, seq: u64, signaled: bool) {
        if !self.enabled { return; }
        self.end = now();
        self.total = self.total.saturating_add(self.end.saturating_sub(start));
        self.count = self.count.saturating_add(1);
        self.last_seq = seq;
        self.signaled |= signaled;
    }
}
impl Drop for WaitTrace {
    fn drop(&mut self) {
        if !self.enabled || self.total < SLOW_NS { return; }
        let seen = WAIT_SLOW.fetch_add(1, Ordering::Relaxed).wrapping_add(1);
        if seen > LOG_LIMIT { WAIT_DROPPED.fetch_add(1, Ordering::Relaxed); return; }
        pr_info!("asahi-vmwait vm={} pid={} tid={} first_seq={} last_seq={} start={} end={} wait_ns={} count={} signaled={} origin=unknown slow_seen={} dropped={} cap={} capped={}\n",
            self.vm, self.pid, self.tid, self.first_seq, self.last_seq, self.start, self.end, self.total,
            self.count, self.signaled as u8, seen, WAIT_DROPPED.load(Ordering::Relaxed),
            LOG_LIMIT, (seen == LOG_LIMIT) as u8);
    }
}
