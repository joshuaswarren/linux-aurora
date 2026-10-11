// SPDX-License-Identifier: GPL-2.0-only

//! Command planning for SPTM-owned GPU address spaces.
//!
//! A plan validates the entire request before producing commands. The caller
//! must reserve backing ownership and bounded command storage before submission.
//! A failed command may have mutated mappings; a completed prefix is not a
//! rollback receipt. Physical pages must remain pinned until confirmed unmap.

use core::ops::Range;

pub(super) const PAGE: u64 = 0x4000;
const L3_SPAN: u64 = 1 << 25;
const L2_SPAN: u64 = 1 << 36;
const MAX_PAGES: u64 = 64;
const PRIVATE_END: u64 = 1 << 42;
/// Canonical queue backing has its own sparse arena. It must not overlap
/// the client-private arena or retain the other profile's sub-2-GiB limit.
pub(super) const CANONICAL_RANGE: Range<u64> = 0x80_0000_0000..0x90_0000_0000;
pub(super) const GLOBAL_RANGE: Range<u64> = 0xffff_fc20_0000_0000..0xffff_fc30_0000_0000;

/// The shared firmware space is never created, bound or destroyed by a VM.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Space {
    Global,
    Private(u8),
}

impl Space {
    fn handle(self) -> Result<u64, InvalidRequest> {
        match self {
            Self::Global => Ok(0xff),
            Self::Private(0xff) => Err(InvalidRequest),
            Self::Private(handle) => Ok(u64::from(handle)),
        }
    }

    fn contains(self, range: &Range<u64>) -> bool {
        match self {
            Self::Global => GLOBAL_RANGE.start <= range.start && range.end <= GLOBAL_RANGE.end,
            Self::Private(_) => range.end <= PRIVATE_END,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct InvalidRequest;

/// Wire operation. Submission receipts overwrite `result`.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Operation {
    pub(super) opcode: u32,
    pub(super) flags: u32,
    pub(super) args: [u64; 6],
    pub(super) result: u64,
}

const _: () = assert!(core::mem::size_of::<Operation>() == 64);

impl Operation {
    pub(super) fn new(opcode: u32, args: [u64; 5]) -> Self {
        Self { opcode, flags: 0, args: [args[0], args[1], args[2], args[3], args[4], 0], result: 0 }
    }
}

/// Protection values accepted by the SPTM mapping interface.
#[derive(Clone, Copy, Debug)]
pub(super) enum Protection {
    FirmwarePrivateReadWrite,
    FirmwareReadWrite,
    FirmwareReadOnly,
    SharedReadWrite,
    FirmwareWriteGpuRead,
    GpuReadWrite,
    GpuReadOnly,
    FirmwareMmioReadWrite,
    FirmwareMmioReadOnly,
}

impl Protection {
    fn options(self) -> u64 {
        match self {
            Self::FirmwarePrivateReadWrite => 0x003,
            Self::FirmwareReadWrite => 0x00b,
            Self::FirmwareReadOnly => 0x009,
            Self::SharedReadWrite => 0x30b,
            Self::FirmwareWriteGpuRead => 0x103,
            Self::GpuReadWrite => 0x308,
            Self::GpuReadOnly => 0x108,
            Self::FirmwareMmioReadWrite => 0x007,
            Self::FirmwareMmioReadOnly => 0x005,
        }
    }
}

/// Backing pages without expanding an alias of one physical page into an array.
/// Repeated mappings retain the same native one-page MAP operations as a slice.
#[derive(Clone, Copy)]
pub(super) enum MapPages<'a> {
    Slice(&'a [u64]),
    Contiguous { physical: u64, count: u64 },
    Repeated { physical: u64, count: u64 },
}

impl MapPages<'_> {
    fn len(self) -> u64 {
        match self {
            Self::Slice(pages) => pages.len() as u64,
            Self::Contiguous { count, .. } | Self::Repeated { count, .. } => count,
        }
    }

    fn validate(self) -> Result<(), InvalidRequest> {
        let valid = |pa: u64| pa & (PAGE - 1) == 0 && pa.checked_add(PAGE).is_some();
        match self {
            Self::Slice(pages) if pages.iter().all(|pa| valid(*pa)) => Ok(()),
            Self::Contiguous { physical, count }
                if valid(physical) && count.checked_mul(PAGE)
                    .and_then(|size| physical.checked_add(size)).is_some() => Ok(()),
            Self::Repeated { physical, .. } if valid(physical) => Ok(()),
            _ => Err(InvalidRequest),
        }
    }

    fn physical_at(self, page: u64) -> u64 {
        match self {
            Self::Slice(pages) => pages[page as usize],
            Self::Contiguous { physical, .. } => physical + page * PAGE,
            Self::Repeated { physical, .. } => physical,
        }
    }

    fn contiguous_count(self, start: u64, limit: u64) -> u64 {
        match self {
            // The provider ABI maps physically consecutive pages. A repeated
            // physical address must still be published in separate operations.
            Self::Repeated { .. } => 1,
            Self::Contiguous { .. } => limit,
            Self::Slice(pages) => {
                let start = start as usize;
                let mut count = 1;
                while count < limit as usize
                    && pages[start + count] == pages[start + count - 1] + PAGE {
                    count += 1;
                }
                count as u64
            }
        }
    }
}

/// A contiguous virtual range backed by individual, possibly repeated pages.
#[derive(Clone)]
pub(super) struct MapPlan<'a> {
    handle: u64,
    va: u64,
    end: u64,
    l2: u64,
    l3: u64,
    pages: MapPages<'a>,
    next_page: u64,
    options: u64,
    opcode: u32,
}

fn checked_range(space: Space, va: u64, pages: u64) -> Result<Range<u64>, InvalidRequest> {
    let size = pages.checked_mul(PAGE).ok_or(InvalidRequest)?;
    let end = va.checked_add(size).ok_or(InvalidRequest)?;
    let range = va..end;
    if pages == 0 || va & (PAGE - 1) != 0 || !space.contains(&range) {
        return Err(InvalidRequest);
    }
    Ok(range)
}

fn segment_pages(va: u64, remaining: u64) -> u64 {
    remaining.min(MAX_PAGES).min((L3_SPAN - (va & (L3_SPAN - 1))) / PAGE)
}

impl<'a> MapPlan<'a> {
    pub(super) fn new(space: Space, va: u64, pages: MapPages<'a>, protection: Protection)
        -> Result<Self, InvalidRequest>
    {
        let handle = space.handle()?;
        let range = checked_range(space, va, pages.len())?;
        let io = matches!(protection, Protection::FirmwareMmioReadWrite | Protection::FirmwareMmioReadOnly);
        if io && space != Space::Global { return Err(InvalidRequest); }
        pages.validate()?;
        Ok(Self {
            handle, va, end: range.end,
            // The shared space already owns its L2; only add missing L3s.
            l2: if space == Space::Global { range.end } else { va & !(L2_SPAN - 1) },
            l3: va & !(L3_SPAN - 1), pages, next_page: 0, options: protection.options(),
            opcode: if io { 9 } else { 3 },
        })
    }
}

impl Iterator for MapPlan<'_> {
    type Item = Operation;

    fn next(&mut self) -> Option<Operation> {
        if self.l2 < self.end {
            let va = self.l2;
            self.l2 += L2_SPAN;
            return Some(Operation::new(2, [self.handle, va, 1, 0, 0]));
        }
        if self.l3 < self.end {
            let va = self.l3;
            self.l3 += L3_SPAN;
            return Some(Operation::new(2, [self.handle, va, 2, 0, 0]));
        }
        if self.next_page == self.pages.len() {
            return None;
        }
        let start = self.next_page;
        let va = self.va + start * PAGE;
        let count = self.pages.contiguous_count(start, segment_pages(va, self.pages.len() - start));
        self.next_page += count;
        Some(Operation::new(self.opcode, [self.handle, va, self.pages.physical_at(start), count, self.options]))
    }
}

#[derive(Clone)]
pub(super) struct UnmapPlan {
    handle: u64,
    remaining: Range<u64>,
}

impl UnmapPlan {
    pub(super) fn new(space: Space, va: u64, pages: u64) -> Result<Self, InvalidRequest> {
        Ok(Self { handle: space.handle()?, remaining: checked_range(space, va, pages)? })
    }
}

impl Iterator for UnmapPlan {
    type Item = Operation;

    fn next(&mut self) -> Option<Operation> {
        if self.remaining.is_empty() { return None; }
        let va = self.remaining.start;
        let count = segment_pages(va, (self.remaining.end - va) / PAGE);
        self.remaining.start += count * PAGE;
        Some(Operation::new(4, [self.handle, va, count, 0, 0]))
    }
}

/// Classify a failed submission, including the first unconfirmed operation.
/// An impossible receipt cannot establish that backing was never published.
pub(super) fn may_have_mapped(ops: &[Operation], completed: usize) -> bool {
    completed > ops.len() || ops.iter().take(completed.saturating_add(1)).any(|op| matches!(op.opcode, 3 | 9))
}

/// Binding ownership is retained even when an operation's outcome is unknown.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ContextState {
    Unbound,
    Bound(u8),
    Uncertain(Option<u8>),
    Destroyed,
}

/// Provider object identity; this is deliberately not a physical root address.
pub(super) struct PrivateState {
    handle: u8,
    state: ContextState,
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum TransitionError<E> {
    Invalid,
    Provider(E),
}

impl PrivateState {
    pub(super) fn new(handle: u64) -> Result<Self, InvalidRequest> {
        if handle >= 0xff { return Err(InvalidRequest); }
        Ok(Self { handle: handle as u8, state: ContextState::Unbound })
    }

    pub(super) fn state(&self) -> ContextState { self.state }

    pub(super) fn space(&self) -> Result<Space, InvalidRequest> {
        match self.state {
            ContextState::Unbound | ContextState::Bound(_) => Ok(Space::Private(self.handle)),
            _ => Err(InvalidRequest),
        }
    }

    /// Mark possible mapping publication or failed cleanup as unusable.
    pub(super) fn poison(&mut self) {
        self.state = match self.state {
            ContextState::Bound(id) => ContextState::Uncertain(Some(id)),
            ContextState::Unbound => ContextState::Uncertain(None),
            other => other,
        };
    }

    pub(super) fn bind<E>(&mut self, context: u8,
        submit: impl FnOnce(&mut Operation) -> Result<(), E>) -> Result<(), TransitionError<E>>
    {
        if context >= 64 || self.state != ContextState::Unbound {
            return Err(TransitionError::Invalid);
        }
        // Claim before publication. A failing call does not authorize reuse.
        self.state = ContextState::Uncertain(Some(context));
        submit(&mut Operation::new(5, [self.handle as u64, context as u64, 0, 0, 0]))
            .map_err(TransitionError::Provider)?;
        self.state = ContextState::Bound(context);
        Ok(())
    }

    /// The caller must first retire all work using this execution binding.
    pub(super) fn unbind<E>(&mut self,
        submit: impl FnOnce(&mut Operation) -> Result<(), E>) -> Result<(), TransitionError<E>>
    {
        let ContextState::Bound(id) = self.state else { return Err(TransitionError::Invalid); };
        self.state = ContextState::Uncertain(Some(id));
        submit(&mut Operation::new(6, [self.handle as u64, 0, 0, 0, 0]))
            .map_err(TransitionError::Provider)?;
        self.state = ContextState::Unbound;
        Ok(())
    }

    /// The caller must confirm unmap of all backing before returning the object.
    /// Failed destruction retains its identity; it never makes it reusable.
    pub(super) fn destroy<E>(&mut self,
        submit: impl FnOnce(&mut Operation) -> Result<(), E>) -> Result<(), TransitionError<E>>
    {
        if self.state != ContextState::Unbound { return Err(TransitionError::Invalid); }
        self.state = ContextState::Uncertain(None);
        submit(&mut Operation::new(7, [self.handle as u64, 0, 0, 0, 0]))
            .map_err(TransitionError::Provider)?;
        self.state = ContextState::Destroyed;
        Ok(())
    }
}
