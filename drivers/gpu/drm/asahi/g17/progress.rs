// SPDX-License-Identifier: GPL-2.0-only OR MIT

//! Forward-progress witness for capacity waits (vmfix6d).
//!
//! A publication that waits for render capacity (a busy pair, a lent pair's
//! drain, a render channel, a buffer identity, an admission slot) is never
//! failed on a fixed poll count. It fails only after `STALL_NS` in which the
//! GPU retired nothing and the blocker state did not change: a real deadlock.
//! Every wait reports its blocker at 1 s, 10 s, 30 s and then every 60 s.

use core::sync::atomic::{AtomicU64, Ordering};

/// Exact completions witnessed device-wide (render and compute).
static RETIRED: AtomicU64 = AtomicU64::new(0);
/// Bumped before every notification of the device `changed` condition.
static CHANGES: AtomicU64 = AtomicU64::new(0);

pub(crate) fn note_retirement() {
    RETIRED.fetch_add(1, Ordering::Relaxed);
}
pub(crate) fn retired() -> u64 {
    RETIRED.load(Ordering::Relaxed)
}
pub(crate) fn note_change() {
    CHANGES.fetch_add(1, Ordering::Release);
}
pub(crate) fn changes() -> u64 {
    CHANGES.load(Ordering::Acquire)
}

const SECOND: u64 = 1_000_000_000;
/// No retirement and an unchanged blocker for this long fails the wait.
pub(crate) const STALL_NS: u64 = 30 * SECOND;

pub(crate) enum Step {
    Wait,
    /// Log the blocker (1 s, 10 s, 30 s, then every 60 s of waiting).
    Report,
    /// `STALL_NS` without retirement or blocker change: log and fail.
    Stalled,
}

/// One wait, possibly across several blocker kinds of the same publication.
#[derive(Clone, Copy)]
pub(crate) struct Watch {
    start: u64,
    progress: u64,
    retired: u64,
    sampled: u64,
    fingerprint: Option<u64>,
    next_report: u64,
    pub(crate) polls: u32,
}

impl Watch {
    pub(crate) fn new(now: u64) -> Self {
        Self {
            start: now,
            progress: now,
            retired: retired(),
            sampled: now,
            fingerprint: None,
            next_report: SECOND,
            polls: 0,
        }
    }
    /// A capacity stall was reported: keep waiting, and report again only
    /// after another full window without progress.
    pub(crate) fn rearm(&mut self, now: u64) {
        self.progress = now;
    }
    pub(crate) fn elapsed_ms(&self, now: u64) -> u64 {
        now.saturating_sub(self.start) / 1_000_000
    }
    pub(crate) fn idle_ms(&self, now: u64) -> u64 {
        now.saturating_sub(self.progress) / 1_000_000
    }
    /// `sample` hashes the blocker state; it is taken at most once a second.
    /// A capacity wait (`global`) also counts any exact retirement as
    /// progress; a pair-local wait counts only a change of its blocker, so
    /// unrelated clients' work cannot mask a local deadlock.
    pub(crate) fn poll(&mut self, now: u64, global: bool, sample: impl FnOnce() -> u64) -> Step {
        self.polls = self.polls.saturating_add(1);
        let retired = retired();
        if retired != self.retired {
            self.retired = retired;
            if global {
                self.progress = now;
            }
        }
        if now.saturating_sub(self.sampled) >= SECOND || self.fingerprint.is_none() {
            self.sampled = now;
            let fingerprint = sample();
            if self.fingerprint.is_some_and(|old| old != fingerprint) {
                self.progress = now;
            }
            self.fingerprint = Some(fingerprint);
        }
        if now.saturating_sub(self.progress) >= STALL_NS {
            return Step::Stalled;
        }
        let elapsed = now.saturating_sub(self.start);
        if elapsed >= self.next_report {
            self.next_report = match self.next_report {
                n if n < 10 * SECOND => 10 * SECOND,
                n if n < 30 * SECOND => 30 * SECOND,
                n => n + 60 * SECOND,
            };
            return Step::Report;
        }
        Step::Wait
    }
}

/// FNV-1a over the blocker words.
pub(crate) fn mix(hash: u64, word: u64) -> u64 {
    (hash ^ word).wrapping_mul(0x100_0000_01b3)
}
pub(crate) const SEED: u64 = 0xcbf2_9ce4_8422_2325;

/// First eight, then every power of two.
pub(crate) fn sampled(counter: &AtomicU64) -> Option<u64> {
    let n = counter.fetch_add(1, Ordering::Relaxed) + 1;
    (n <= 8 || n.is_power_of_two()).then_some(n)
}
