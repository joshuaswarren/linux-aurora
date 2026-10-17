// SPDX-License-Identifier: GPL-2.0-only AND MIT

//! Context-local work-state identities. The low six bits address a node;
//! the complete sequence distinguishes its owner until exact retirement.

use core::sync::atomic::{AtomicU32, Ordering};

pub(super) const FREE: u32 = u32::MAX;
pub(super) const STATE_COUNT: usize = 63;
pub(super) const NODE_STRIDE: usize = 0x100;

pub(crate) struct WorkStates {
    pub(crate) available: kernel::sync::Arc<crate::g17::availability::Availability>,
    next: AtomicU32,
    live: [AtomicU32; 64],
}
impl WorkStates {
    pub(crate) fn new() -> kernel::error::Result<Self> {
        Ok(Self {
            available: crate::g17::availability::Availability::new()?,
            next: AtomicU32::new(0),
            live: [const { AtomicU32::new(FREE) }; 64],
        })
    }
    pub(super) fn allocate(&self) -> Option<u32> {
        for _ in 0..64 {
            let word = self.next.fetch_add(1, Ordering::Relaxed);
            let index = (word & 63) as usize;
            if index != STATE_COUNT
                && self.live[index]
                    .compare_exchange(FREE, word, Ordering::AcqRel, Ordering::Acquire)
                    .is_ok()
            {
                return Some(word);
            }
        }
        None
    }
    pub(super) fn release(&self, word: u32) -> bool {
        let index = (word & 63) as usize;
        let released = index != STATE_COUNT
            && self.live[index]
                .compare_exchange(word, FREE, Ordering::AcqRel, Ordering::Acquire)
                .is_ok();
        if released { self.available.notify(); }
        released
    }
}

/// Two stable logical ordinals (render channels), independent of physical
/// pair numbering. A context's render work can move between physical pairs
/// (sibling pairs, lending, relocation, QID recycling), so a channel is rebound
/// to a new pair once every publication this context made through it has
/// retired. Native contexts keep one channel whose pass bytes count up for
/// the whole process (v70: TA +0x8be = 0 and +0x7b0 = 1, 2, 3, ... in all 157
/// captured TA commands), so a rebound channel continues its pass sequence.
pub(super) struct RenderSlots {
    pairs: [Option<u8>; 2],
    origins: [Option<u64>; 2],
    /// Pass of the channel's last publication on its previous pair.
    bases: [u8; 2],
    /// QIDs and visibility target (ordinal + 1) of the last publication made
    /// through each channel.
    last: [Option<([u8; 2], u64)>; 2],
    /// Recency of each channel's last publication.
    used: [u64; 2],
    epoch: u64,
}
impl RenderSlots {
    pub(crate) const fn new() -> Self {
        Self {
            pairs: [None; 2],
            origins: [None; 2],
            bases: [0; 2],
            last: [None; 2],
            used: [0; 2],
            epoch: 0,
        }
    }
    pub(super) fn get(&mut self, pair: u8) -> Option<u32> {
        if let Some(index) = self.pairs.iter().position(|value| *value == Some(pair)) {
            return Some(index as u32);
        }
        let index = self.pairs.iter().position(Option::is_none)?;
        self.pairs[index] = Some(pair);
        Some(index as u32)
    }
    /// The channel of `pair`, rebinding the least recently used channel whose
    /// publications have all retired (`retired(qids, ordinal)`) when both are
    /// bound to other pairs. `None`: no channel can move yet.
    pub(super) fn bind(
        &mut self,
        pair: u8,
        retired: impl Fn([u8; 2], u64) -> bool,
    ) -> Option<(u32, Option<u8>)> {
        if let Some(index) = self.get(pair) {
            return Some((index, None));
        }
        let index = (0..2)
            .filter(|index| self.last[*index].is_none_or(|(qids, ordinal)| retired(qids, ordinal)))
            .min_by_key(|index| self.used[*index])?;
        let previous = self.pairs[index];
        // `last` holds the ordinal after the last publication (its visibility
        // target), so that publication's pass is base + (next - origin).
        if let (Some(origin), Some((_, next))) = (self.origins[index], self.last[index]) {
            self.bases[index] = self.bases[index].wrapping_add(next.wrapping_sub(origin) as u8);
        }
        self.pairs[index] = Some(pair);
        self.origins[index] = None;
        self.last[index] = None;
        Some((index as u32, previous))
    }
    pub(super) fn payload(&mut self, pair: u8, ordinal: u64) -> Option<u8> {
        let index = self.pairs.iter().position(|value| *value == Some(pair))?;
        let origin = *self.origins[index].get_or_insert(ordinal);
        Some(self.bases[index].wrapping_add(ordinal.checked_sub(origin)?.wrapping_add(1) as u8))
    }
    /// Record a publication that reached firmware through `pair`'s channel.
    pub(super) fn published(&mut self, pair: u8, qids: [u8; 2], ordinal: u64) -> bool {
        let Some(index) = self.pairs.iter().position(|value| *value == Some(pair)) else {
            return false;
        };
        self.epoch = self.epoch.wrapping_add(1);
        self.used[index] = self.epoch;
        self.last[index] = Some((qids, ordinal));
        true
    }
}
