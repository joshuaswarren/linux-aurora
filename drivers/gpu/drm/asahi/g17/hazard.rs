// SPDX-License-Identifier: GPL-2.0-only OR MIT

//! Attachment overlap ordering across physical render pairs and shared images.
//! Tile stores can overlap even when the application's pixel regions do not.
//! Keep independent attachments concurrent; order only conflicting live spans.

use super::job::Order;

const MAX_SPANS: usize = 24;
pub(crate) const WAIT_MS: u32 = 1000;

#[derive(Clone, Copy)]
struct Span {
    key: u64,
    start: u64,
    end: u64,
}

/// Up to sixteen attachment ranges and eight shared-object ranges.
pub(crate) struct Footprint {
    vm: u64,
    spans: [Span; MAX_SPANS],
    count: usize,
    shared: bool,
    /// The render writes whole attachments: its tile stores reach no pixel
    /// outside its own render area.
    whole: bool,
}

impl Footprint {
    pub(crate) fn new(vm: u64, whole: bool) -> Self {
        Self {
            vm,
            whole,
            spans: [Span {
                key: 0,
                start: 0,
                end: 0,
            }; MAX_SPANS],
            count: 0,
            shared: false,
        }
    }

    fn push(&mut self, key: u64, start: u64, end: u64) {
        if start < end && self.count < MAX_SPANS {
            self.spans[self.count] = Span { key, start, end };
            self.count += 1;
        }
    }

    pub(crate) fn push_vm(&mut self, start: u64, end: u64) {
        self.push(self.vm << 1, start, end);
    }

    /// An aligned GEM object address is an identity, never dereferenced here.
    pub(crate) fn push_object(&mut self, object: usize, start: u64, end: u64) {
        let before = self.count;
        self.push(object as u64 | 1, start, end);
        self.shared |= self.count != before;
    }

    fn overlaps(&self, other: &Self) -> bool {
        if self.vm != other.vm && !(self.shared && other.shared) {
            return false;
        }
        // Tile-granular stores of two whole-attachment renders write only
        // pixels both renders own; unordered, they race as the application
        // asked.
        if self.whole && other.whole {
            return false;
        }
        self.spans[..self.count].iter().any(|a| {
            other.spans[..other.count]
                .iter()
                .any(|b| a.key == b.key && a.start < b.end && b.start < a.end)
        })
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Plan {
    Any,
    Pair(u8),
    /// Publish on the pair, after the owner's renders through the sequence:
    /// the conflicting renders on the owner's other pair complete their
    /// fragment work first through a firmware dependency (Packet::hazard_through).
    After(u8, u64),
    Wait,
}

pub(crate) struct Live<'a> {
    pub(crate) owner: u64,
    pub(crate) sequence: u64,
    pub(crate) slot: Option<u8>,
    pub(crate) ticket: u64,
    pub(crate) footprint: &'a Footprint,
    pub(crate) quarantined: bool,
}

/// A queue prefix already orders covered predecessors. Pending arrivals order
/// different owners fairly without imposing order on unrelated attachments.
pub(crate) fn plan<'a>(
    owner: u64,
    prefix: Option<u64>,
    ticket: Option<u64>,
    footprint: &Footprint,
    live: impl IntoIterator<Item = Live<'a>>,
) -> Plan {
    // The newest unordered conflict on each of at most two of the owner's
    // pairs: the pair FIFO orders a render after everything on its own pair.
    let mut conflicts: [Option<(u8, u64)>; 2] = [None; 2];
    for entry in live {
        if entry.quarantined || !footprint.overlaps(entry.footprint) {
            continue;
        }
        let Some(slot) = entry.slot else {
            if entry.owner != owner && ticket.is_none_or(|mine| entry.ticket < mine) {
                return Plan::Wait;
            }
            continue;
        };
        if entry.owner != owner {
            return Plan::Wait;
        }
        if Order::contains(prefix, entry.sequence) {
            continue;
        }
        match conflicts.iter_mut().find(|c| c.is_none_or(|(s, _)| s == slot)) {
            Some(c) => *c = Some((slot, c.map_or(entry.sequence, |(_, q)| q.max(entry.sequence)))),
            None => return Plan::Wait,
        }
    }
    match conflicts {
        [None, _] => Plan::Any,
        [Some((slot, _)), None] => Plan::Pair(slot),
        // Join the pair of the newer conflict and order the fragment work
        // after the older one, as an application barrier on it would. Tile
        // loads and stores happen only in fragment work.
        [Some(a), Some(b)] => {
            let (join, other) = if a.1 >= b.1 { (a, b) } else { (b, a) };
            Plan::After(join.0, other.1)
        }
    }
}
