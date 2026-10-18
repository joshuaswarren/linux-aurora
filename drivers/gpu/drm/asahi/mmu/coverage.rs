// SPDX-License-Identifier: GPL-2.0-only OR MIT

//! Submission coverage proofs that never walk the page table on a hit.
//!
//! A command's GPU address ranges must be mapped with the access it needs.
//! A logical queue keeps the runs it proved, merged into intervals.
//! Runs only appear in unmapped space; a mapping can only lose coverage by an
//! unmap (every removal path, residency eviction included, goes through the
//! page table's `unmap_pages`) or by the table becoming uncertain. Both
//! record the affected range in the VM's [`RemovalLog`] before the page
//! tables change, so a proof drops exactly the intervals a removal touched.
//! A hit is a few comparisons against the queue's own intervals.

use super::Vm;
use core::sync::atomic::{AtomicU64, Ordering};
use kernel::{new_spinlock, prelude::*, sync::{Arc, SpinLock}};

/// Removals remembered for proofs that are behind; an older proof drops all.
const LOG_ENTRIES: usize = 64;
/// Intervals one queue keeps; a full cache restarts.
const MAX_INTERVALS: usize = 512;

struct Ring {
    /// (sequence, start, end) of the last removals.
    entries: [(u64, u64, u64); LOG_ENTRIES],
    next: usize,
    /// Every removal with a sequence at or above this is still in `entries`.
    complete_from: u64,
}

/// Ranges removed from one VM's page table, in order. Written under the
/// GPUVM exec lock before the removal changes any page table entry.
#[pin_data]
pub(crate) struct RemovalLog {
    sequence: AtomicU64,
    #[pin]
    ring: SpinLock<Ring>,
}

impl RemovalLog {
    pub(crate) fn new() -> Result<Arc<Self>> {
        Arc::pin_init(pin_init!(Self {
            sequence: AtomicU64::new(0),
            ring <- new_spinlock!(Ring { entries: [(0, 0, 0); LOG_ENTRIES], next: 0, complete_from: 1 },
                "asahi coverage removals"),
        }), GFP_KERNEL)
    }

    pub(crate) fn sequence(&self) -> Option<u64> {
        let value = self.sequence.load(Ordering::Acquire);
        (value != u64::MAX).then_some(value)
    }

    /// `range` loses coverage (call before its page table entries change).
    pub(crate) fn record(&self, start: u64, end: u64) {
        let mut ring = self.ring.lock();
        let sequence = self.sequence.load(Ordering::Relaxed).checked_add(1).unwrap_or(u64::MAX);
        let slot = ring.next % LOG_ENTRIES;
        let replaced = ring.entries[slot].0;
        if replaced != 0 {
            ring.complete_from = replaced.saturating_add(1);
        }
        ring.entries[slot] = (sequence, start, end);
        ring.next = ring.next.wrapping_add(1);
        self.sequence.store(sequence, Ordering::Release);
    }

    /// Everything loses coverage (the table became uncertain).
    pub(crate) fn record_all(&self) {
        self.record(0, u64::MAX);
    }

    /// Calls `removed` for every removal after `since` up to `until`.
    /// False if some of them are no longer remembered.
    fn removed_between(&self, since: u64, until: u64, mut removed: impl FnMut(u64, u64)) -> bool {
        let ring = self.ring.lock();
        if since.saturating_add(1) < ring.complete_from {
            return false;
        }
        for &(sequence, start, end) in ring.entries.iter() {
            if sequence > since && sequence <= until {
                removed(start, end);
            }
        }
        true
    }
}

/// One logical queue's proven intervals: disjoint, sorted, merged runs with
/// the GPU access every page of them allows, valid as of `sequence`.
pub(crate) struct CoverageCache {
    sequence: u64,
    intervals: KVec<(u64, u64, u8)>,
    pub(crate) stats: CoverageStats,
}

#[derive(Default, Clone, Copy)]
pub(crate) struct CoverageStats {
    pub(crate) requests: u64,
    pub(crate) hits: u64,
    pub(crate) walks: u64,
    pub(crate) walk_ns: u64,
    pub(crate) removals_applied: u64,
    pub(crate) restarts: u64,
    pub(crate) checks: u64,
    pub(crate) mismatches: u64,
    /// Why a request set took the walk: a mapping change was in flight, the
    /// intervals did not hold some request, or the mapping changed during the lookup.
    pub(crate) miss_epoch: u64,
    pub(crate) miss_holds: u64,
    pub(crate) miss_sequence: u64,
    /// Page-table runs visited by walks (each one a lookup in the run tree).
    pub(crate) runs_visited: u64,
}

fn page_extent(address: u64, size: u64) -> Option<(u64, u64)> {
    let mask = crate::mmu::UAT_PGMSK as u64;
    let end = address.checked_add(size)?;
    let end = if end & mask == 0 { end } else { (end | mask).checked_add(1)? };
    (size != 0).then_some((address & !mask, end))
}

impl CoverageCache {
    pub(crate) fn new() -> Self {
        Self { sequence: 0, intervals: KVec::new(), stats: CoverageStats::default() }
    }

    /// Bring the intervals to `until`, dropping every one a removal touched.
    fn catch_up(&mut self, log: &RemovalLog, until: u64) {
        if self.sequence == until {
            return;
        }
        let intervals = &mut self.intervals;
        let complete = log.removed_between(self.sequence, until, |start, end| {
            intervals.retain(|&mut (s, e, _)| e <= start || s >= end);
        });
        if !complete {
            self.intervals.clear();
        }
        self.stats.removals_applied += until.saturating_sub(self.sequence);
        self.sequence = until;
    }

    /// Index of the interval containing `address`, if any.
    fn find(&self, address: u64) -> Option<usize> {
        let index = self.intervals.partition_point(|&(start, _, _)| start <= address);
        let index = index.checked_sub(1)?;
        (address < self.intervals[index].1).then_some(index)
    }

    /// The pages of `[start, end)` are all inside proven intervals allowing `access`.
    fn holds(&self, start: u64, end: u64, access: u8) -> bool {
        let Some(mut index) = self.find(start) else { return false; };
        loop {
            let (_, e, allowed) = self.intervals[index];
            if allowed & access != access {
                return false;
            }
            if e >= end {
                return true;
            }
            index += 1;
            if index >= self.intervals.len() || self.intervals[index].0 != e {
                return false;
            }
        }
    }

    /// Insert a proven run (slow path only), merging overlapping and touching
    /// intervals of the same access. Proven intervals at one sequence describe
    /// existing runs, so an overlap with a different access is stale: drop it.
    fn learn(&mut self, start: u64, end: u64, access: u8) -> Result {
        if start >= end {
            return Ok(());
        }
        if self.intervals.len() >= MAX_INTERVALS {
            self.stats.restarts += 1;
            self.intervals.clear();
        }
        let (mut s, mut e) = (start, end);
        let mut out = KVec::with_capacity(self.intervals.len() + 1, GFP_KERNEL)?;
        let mut placed = false;
        for &(is, ie, ia) in self.intervals.iter() {
            if ie < s || (ie == s && ia != access) {
                out.push((is, ie, ia), GFP_KERNEL)?;
            } else if is > e || (is == e && ia != access) {
                if !placed {
                    out.push((s, e, access), GFP_KERNEL)?;
                    placed = true;
                }
                out.push((is, ie, ia), GFP_KERNEL)?;
            } else if ia == access {
                s = s.min(is);
                e = e.max(ie);
            }
        }
        if !placed {
            out.push((s, e, access), GFP_KERNEL)?;
        }
        self.intervals = out;
        Ok(())
    }

    /// Whether every `(address, size, access)` request is mapped with its
    /// access. `None`: this VM has no provider page table; use the walk.
    /// A hit takes no lock and reads no page-table state.
    pub(crate) fn covers(&mut self, vm: &Vm, requests: &[(u64, u64, u8)]) -> Option<bool> {
        let log = vm.removals.as_ref()?;
        self.stats.requests += requests.len() as u64;
        // During a mapping change, wait for it under the exec lock below.
        if vm.mapping_validation_epoch().is_none() {
            self.stats.miss_epoch += 1;
        } else {
            let Some(sequence) = log.sequence() else {
                self.intervals.clear();
                return Some(vm.covers_ranges_batch(requests).0);
            };
            self.catch_up(log, sequence);
            let all = requests.iter().all(|&(address, size, access)| {
                page_extent(address, size).is_some_and(|(start, end)| self.holds(start, end, access))
            });
            if all && vm.mapping_validation_epoch().is_some() && log.sequence() == Some(sequence) {
                self.stats.hits += requests.len() as u64;
                let every = u64::from(*crate::module_parameters::coverage_check.value());
                if every != 0 && self.stats.hits % every < requests.len() as u64 {
                    return Some(self.check(vm, requests));
                }
                return Some(true);
            }
            if all {
                self.stats.miss_sequence += 1;
            } else {
                self.stats.miss_holds += 1;
            }
        }
        let start = crate::g17::now_ns();
        let covered = self.walk(vm, log, requests).unwrap_or(false);
        self.stats.walks += 1;
        self.stats.walk_ns += crate::g17::now_ns().saturating_sub(start);
        Some(covered)
    }

    /// Diagnostic: re-prove a cache hit by walking the page table (the walk wins).
    fn check(&mut self, vm: &Vm, requests: &[(u64, u64, u8)]) -> bool {
        self.stats.checks += 1;
        let Ok(mut inner) = vm.inner.exec_lock(None, false) else { return false; };
        for &(address, size, access) in requests {
            if !Vm::covers_range_locked(&mut inner, address, size, access & 1 != 0, access & 2 != 0) {
                self.stats.mismatches += 1;
                pr_err!("asahi: coverage cache said {:#x}+{:#x} access {} is mapped but the page table disagrees (mismatch {})\n",
                    address, size, access, self.stats.mismatches);
                self.intervals.clear();
                return false;
            }
        }
        true
    }

    /// Under one exec lock, prove the requests the intervals do not hold and
    /// learn every run they span (clipped to the VM's user range).
    fn walk(&mut self, vm: &Vm, log: &RemovalLog, requests: &[(u64, u64, u8)]) -> Result<bool> {
        let mut inner = vm.inner.exec_lock(None, false)?;
        // Removals hold this lock: the sequence is stable while we walk.
        let Some(sequence) = log.sequence() else {
            self.intervals.clear();
            return Ok(requests.iter().all(|&(address, size, access)|
                Vm::covers_range_locked(&mut inner, address, size, access & 1 != 0, access & 2 != 0)));
        };
        self.catch_up(log, sequence);
        let range = inner.va_range.clone();
        for &(address, size, access) in requests {
            let Some((start, end)) = page_extent(address, size) else { return Ok(false); };
            if start < range.start || end > range.end {
                return Ok(false);
            }
            if self.holds(start, end, access) {
                continue;
            }
            // Learn every run the request spans, merging touching runs of the
            // same access into one span first (as learn() would). fold351: an
            // eight-run cap left every longer range unlearned, so each
            // submission that named one walked again (Minecraft: 14090 walks
            // visiting 3.2M runs, 227 per walk; a second launch walked half of
            // its submissions). Learning is an optimization: a failed
            // allocation only leaves the span unlearned.
            let mut allowed = true;
            let mut visited = 0u64;
            let mut span: Option<(u64, u64, u8)> = None;
            let (low, high) = (range.start, range.end);
            let cache = &mut *self;
            let covered = inner.page_table.visit_runs(start..end, |s, e, a| {
                visited += 1;
                allowed &= a & access == access;
                span = match span {
                    Some((ss, se, sa)) if se == s && sa == a => Some((ss, e, a)),
                    Some((ss, se, sa)) => {
                        let _ = cache.learn(ss.max(low), se.min(high), sa);
                        Some((s, e, a))
                    }
                    None => Some((s, e, a)),
                };
                Ok(())
            })?;
            if let Some((ss, se, sa)) = span {
                let _ = self.learn(ss.max(low), se.min(high), sa);
            }
            self.stats.runs_visited += visited;
            if !covered || !allowed {
                return Ok(false);
            }
        }
        Ok(true)
    }
}
