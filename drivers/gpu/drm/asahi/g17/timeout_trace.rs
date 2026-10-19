// SPDX-License-Identifier: GPL-2.0-only OR MIT
//! Opt-in one-shot compute timeout snapshot. No firmware writes or nested locks.
use core::sync::atomic::{AtomicU32, Ordering};
use kernel::prelude::*;

pub(crate) const ACTIVE: usize = 16;
// Claim states: 0 available, 1 copying, 2 one valid snapshot consumed.
static CLAIM: AtomicU32 = AtomicU32::new(0);
static ATTEMPTS: AtomicU32 = AtomicU32::new(0);
static SKIPS: AtomicU32 = AtomicU32::new(0);
pub(crate) const NO_FIRMWARE: u32 = 1;
pub(crate) const SCAN_FAILED: u32 = 2;
pub(crate) const NO_QUEUE: u32 = 3;
pub(crate) const NOT_OWNED: u32 = 4;
pub(crate) const ALLOCATION_FAILED: u32 = 5;
pub(crate) const CLAIM_UNAVAILABLE: u32 = 6;
pub(crate) const OVERFLOW: u32 = 7;
pub(crate) const DEFERRED_ALLOCATION_FAILED: u32 = 8;

/// Sequential raw reads of one exact retained victim's work storage. Status:
/// 0 not attempted, 1 copied, 2 busy, 3 unbound, 4 bad slot, 5 identity,
/// 6 ticket/node mismatch, 7 range, 8 read error. Valid counts qualify zeros.
/// Mismatch bits: 1 raw counter pointer, 2 mapped counter versus host count.
pub(crate) struct WorkSnapshot {
    pub(crate) node: [u32; 50],
    pub(crate) head: [u32; 6],
    /// Ticket node, mapped node, mapped head, mapped submitted counter.
    pub(crate) addresses: [u64; 4],
    /// Lease word, stored word (MAX if absent), slot, host submitted count.
    pub(crate) identity: [u32; 4],
    pub(crate) counter: u32,
    pub(crate) valid: [u32; 3],
    pub(crate) status: u32,
    pub(crate) error: i32,
    pub(crate) mismatch: u32,
}
const _: () = assert!(core::mem::size_of::<WorkSnapshot>() <= 400);

/// Plain integer storage: initialized in its heap allocation, never on the stack.
/// Packet words: pointer,id,sequence,wait0,wait1,wait-valid bits,publication,cancelled,
/// pin-id,pin-generation,pin-qos,pinned,input-count,firmware-count.
pub(crate) struct Snapshot {
    pub(crate) start: u64,
    pub(crate) end: u64,
    pub(crate) epoch: u64,
    pub(crate) recovery: u32,
    pub(crate) recovery_error: i32,
    pub(crate) owner: u64,
    pub(crate) pid: u32,
    pub(crate) context: u64,
    pub(crate) packet: [u64; 14],
    pub(crate) reservation: [u64; 6],
    pub(crate) queue: [u64; 16],
    pub(crate) cursors: [u32; 3],
    pub(crate) cursor_error: i32,
    pub(crate) stamp: u32,
    pub(crate) stamp_error: i32,
    pub(crate) pool_completed: u64,
    pub(crate) pool_error: i32,
    pub(crate) active_count: u32,
    pub(crate) victim_index: u32,
    pub(crate) items: [[u64; 18]; ACTIVE],
    pub(crate) kick_words: [u32; 128],
    pub(crate) kick_error: i32,
    pub(crate) descriptor_words: [u32; 64],
    pub(crate) descriptor_error: i32,
    pub(crate) raw_identity: [u64; 4],
    pub(crate) work: WorkSnapshot,
    pub(crate) peer_count: u32,
    pub(crate) peers: [[u64; 12]; 128],
    pub(crate) render_count: u32,
    pub(crate) renders: [[u64; 12]; 64],
    pub(crate) reason: u32,
    pub(crate) valid: u32,
}
pub(crate) struct Attempt {
    snapshot: Option<KBox<Snapshot>>,
    owned: bool,
    number: u32,
    reason: u32,
}
fn now() -> u64 {
    <kernel::time::Monotonic as kernel::time::ClockSource>::ktime_get() as u64
}
impl Attempt {
    /// Must be called before acquiring the device mutex. Failure never returns Err
    /// to the real timeout path. Only an opt-in timeout allocates storage.
    pub(crate) fn begin() -> Option<Self> {
        if *crate::module_parameters::m5_compute_timeout_trace.value() == 0 { return None; }
        let number = ATTEMPTS.fetch_add(1, Ordering::Relaxed).wrapping_add(1);
        if CLAIM.compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire).is_err() {
            return Some(Self { snapshot: None, owned: false, number, reason: CLAIM_UNAVAILABLE });
        }
        let snapshot = match KBox::<Snapshot>::new_uninit(GFP_KERNEL) {
            Ok(mut storage) => {
                // SAFETY: Snapshot consists entirely of integer arrays/scalars. All
                // zero bit patterns are valid. Initialize directly in allocated memory.
                unsafe { storage.as_mut_ptr().write_bytes(0, 1); Some(storage.assume_init()) }
            }
            Err(_) => None,
        };
        let reason = if snapshot.is_some() { NO_FIRMWARE } else { ALLOCATION_FAILED };
        Some(Self { snapshot, owned: true, number, reason })
    }
    pub(crate) fn storage(&mut self) -> Option<&mut Snapshot> { self.snapshot.as_deref_mut() }
    pub(crate) fn skip(&mut self, reason: u32) { self.reason = reason; }
    pub(crate) fn started(s: &mut Snapshot) { s.start = now(); }
    pub(crate) fn ended(s: &mut Snapshot) { s.end = now(); }
    /// Called after device unlock and VM failure fanout, before outer deferred callbacks.
    pub(crate) fn finish(self) {
        let valid = self.snapshot.as_ref().is_some_and(|s| s.valid == 1);
        if self.owned { CLAIM.store(if valid { 2 } else { 0 }, Ordering::Release); }
        if !valid {
            let count = SKIPS.fetch_add(1, Ordering::Relaxed).wrapping_add(1);
            if count <= 4 || count.is_power_of_two() {
                pr_info!("M5 compute timeout snapshot skipped attempt={} reason={} skips={}\n", self.number, self.snapshot.as_ref().map_or(self.reason, |s| if s.reason != 0 { s.reason } else { self.reason }), count);
            }
            return;
        }
        let s = self.snapshot.as_ref().unwrap();
        pr_info!("M5 compute timeout snapshot v1 attempt={} ns={}/{} owner={} pid={} context={} epoch={} recovery={}/{} active={} victim={} reason={}\n",
            self.number,s.start,s.end,s.owner,s.pid,s.context,s.epoch,s.recovery,s.recovery_error,s.active_count,s.victim_index,s.reason);
        pr_info!("M5 compute timeout packet={:x?} reservation={:x?} queue={:x?}\n",s.packet,s.reservation,s.queue);
        pr_info!("M5 compute timeout cursors={:x?}/{} stamp={:#x}/{} pool={}/{} rawidentity={:x?} rawerrors={}/{}\n",
            s.cursors,s.cursor_error,s.stamp,s.stamp_error,s.pool_completed,s.pool_error,s.raw_identity,s.kick_error,s.descriptor_error);
        pr_info!("M5 compute timeout work status={} error={} mismatch={:#x} valid={:?} addresses={:x?} identity={:x?} counter={:#x} head={:08x?}\n",
            s.work.status,s.work.error,s.work.mismatch,s.work.valid,s.work.addresses,s.work.identity,s.work.counter,s.work.head);
        pr_info!("M5 compute timeout work node 000 {:08x?}\n",&s.work.node[..25]);
        pr_info!("M5 compute timeout work node 064 {:08x?}\n",&s.work.node[25..]);
        for (i,row) in s.items.iter().take(s.active_count as usize).enumerate() {
            pr_info!("M5 compute timeout active {} {:x?}\n",i,row);
        }
        for (i,row) in s.peers.iter().take(s.peer_count as usize).enumerate() {
            pr_info!("M5 compute timeout peer {} {:x?}\n",i,row);
        }
        for (i,row) in s.renders.iter().take(s.render_count as usize).enumerate() {
            pr_info!("M5 compute timeout render {} {:x?}\n",i,row);
        }
        for (i,row) in s.kick_words.chunks(8).enumerate() {
            pr_info!("M5 compute timeout kick {:03x} {:08x?}\n",i*32,row);
        }
        for (i,row) in s.descriptor_words.chunks(8).enumerate() {
            pr_info!("M5 compute timeout descriptor {:03x} {:08x?}\n",i*32,row);
        }
    }
}
