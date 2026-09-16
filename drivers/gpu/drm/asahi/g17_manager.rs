// SPDX-License-Identifier: GPL-2.0-only OR MIT

#![cfg_attr(not(test), allow(dead_code))]

//! T8140/G17P manager construction boundary.
//!
//! This path needs only target identity, init-data geometry, and the retained
//! dual-role resource owner. It does not depend on the AGX2 `HwConfig` and it
//! performs no publication during construction. Explicit queue methods own the
//! resource-0 SKSM configuration and kick-store sequence.

#[cfg(not(test))]
use kernel::prelude::*;
#[cfg(not(test))]
use kernel::time::{delay::fsleep, Delta};

#[cfg(not(test))]
use core::sync::atomic::{fence, Ordering};

#[cfg(not(test))]
use crate::{
    driver::AsahiDevice, g17_completion, g17_compute, g17_initdata, g17_lifecycle, g17_render,
    g17_resources, g17_submission, g17_uapi, hw::agx3, identity, mmu, regs,
};
#[cfg(test)]
#[path = "g17_initdata.rs"]
mod g17_initdata;
#[cfg(test)]
#[path = "identity.rs"]
mod identity;

use g17_initdata::InstanceRole;
use identity::{FirmwareRoleTopology, GpuGen, GpuHalGeneration, GpuVariant, SubmissionTransport};

static G17P_STATE_DUMPED: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);

/// One-shot guard for the hardware-data dump: it must fire on the first
/// render checkpoint whatever the label, since gating it on a specific label
/// simply never matched.
static G17P_HW_DATA_DUMPED: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);

// Kext RE: AsyncGrowEngine::updateSubmitInfo copies AGXUSCPrivMemFList +0xb0
// into opcode-0x20 +0x0c, and the 3D submit path copies that same field into
// tag-15 +0x3e. Valid cold native opcode-0x20 and qid0/qid1 tag-15 records
// both publish generation zero; retained generations advance from there.
const G17P_BOOTSTRAP_UMA_SEQUENCE: u64 =
    g17_submission::g17p_render_flist_generation(0) as u64;
const G17P_BOOTSTRAP_UMA_HARDWARE_BUFFER_ID: u32 =
    g17_initdata::PARTIAL_OPENING_FREELIST_HARDWARE_BUFFER_ID;
const G17P_BOOTSTRAP_UMA_STAMP: i32 = 1;
// B1's type-13 completion echoes the submitted USC FList end address and the
// record's fixed +0x2c count. Keep the retained owner matched to those wire
// values rather than the adjacent operand-buffer cookie used by the older
// opening graph.
const G17P_BOOTSTRAP_UMA_COOKIE: u64 = g17_submission::G17P_USC_FREELIST_LOW_END;
const G17P_BOOTSTRAP_UMA_COUNT: u32 =
    g17_submission::G17P_USC_FREELIST_GROW_DESCRIPTOR_BYTES;
// Direct 2026-09-01 entry-ring captures of ordinary Metal TA and 3D kicks
// both carry this exact value at KSM entry +0x178.  It is selected by the
// descriptor wrapper immediately before encodeKickEntry; it is not the host
// physical-counter sample used by retained compute submissions.
const G17P_NATIVE_RENDER_KICK_AUXILIARY: u64 = 0x003f_ffff_ffff_ffff;

// Honeykrisp splits its T8140 main allocation heap into read/write and
// read-only sparse-shadow halves at bit 40. A PDM fault in the upper half is
// ambiguous unless recovery also checks the corresponding lower allocation.
const G17P_HONEYKRISP_SPARSE_RO_BIT: u64 = 1 << 40;

const fn g17p_fault_sparse_ro_lower(address: u64) -> Option<u64> {
    if address & G17P_HONEYKRISP_SPARSE_RO_BIT == 0 {
        None
    } else {
        Some(address & !G17P_HONEYKRISP_SPARSE_RO_BIT)
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
enum G17PNativeRenderPublishPhase {
    FragmentVisible,
    TilingPrefix,
    TilingSksm,
    TilingKick,
    TilingOuter,
    PoolTransition,
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
struct G17PNativeRenderPublishOrder {
    phase: G17PNativeRenderPublishPhase,
}

impl G17PNativeRenderPublishOrder {
    const fn after_fragment_doorbell() -> Self {
        Self {
            phase: G17PNativeRenderPublishPhase::FragmentVisible,
        }
    }

    fn advance(
        &mut self,
        next: G17PNativeRenderPublishPhase,
    ) -> core::result::Result<(), ()> {
        let expected = match self.phase {
            G17PNativeRenderPublishPhase::FragmentVisible => {
                G17PNativeRenderPublishPhase::TilingPrefix
            }
            G17PNativeRenderPublishPhase::TilingPrefix => {
                G17PNativeRenderPublishPhase::TilingSksm
            }
            G17PNativeRenderPublishPhase::TilingSksm => {
                G17PNativeRenderPublishPhase::TilingKick
            }
            G17PNativeRenderPublishPhase::TilingKick => {
                G17PNativeRenderPublishPhase::TilingOuter
            }
            G17PNativeRenderPublishPhase::TilingOuter => {
                G17PNativeRenderPublishPhase::PoolTransition
            }
            G17PNativeRenderPublishPhase::PoolTransition => return Err(()),
        };
        if next != expected {
            return Err(());
        }
        self.phase = next;
        Ok(())
    }
}

fn g17p_native_fragment_only_visible(
    previous_tiling_inner: u32,
    tiling_inner: u32,
    fragment_inner: u32,
    published_prefix: u32,
    fragment_outer: u32,
    fragment_outer_next: u32,
    tiling_outer: u32,
    tiling_outer_slot: u8,
) -> bool {
    tiling_inner == previous_tiling_inner
        && fragment_inner == published_prefix
        && fragment_outer == fragment_outer_next
        && tiling_outer == u32::from(tiling_outer_slot)
}

fn g17p_native_tiling_fully_visible(
    tiling_inner: u32,
    published_prefix: u32,
    tiling_outer: u32,
    tiling_outer_next: u32,
) -> bool {
    tiling_inner == published_prefix && tiling_outer == tiling_outer_next
}

fn g17p_native_fragment_first_mode(mode: u32) -> bool {
    mode == 2 || mode == 3
}

fn g17p_render_deferred_outer_mode(
    native_doorbell_mode: u32,
    split_launch: bool,
) -> g17_resources::G17PDeferredOuterPublication {
    if split_launch {
        // The TA group is complete, but the USC FList control notification
        // can wake it before the intended initial doorbell. Retain both here;
        // split launch publishes only TA after the FList transaction retires.
        g17_resources::G17PDeferredOuterPublication::Both
    } else if g17p_native_fragment_first_mode(native_doorbell_mode) {
        g17_resources::G17PDeferredOuterPublication::Tiling
    } else if native_doorbell_mode == 0 {
        g17_resources::G17PDeferredOuterPublication::Both
    } else {
        g17_resources::G17PDeferredOuterPublication::None
    }
}

fn g17p_native_fragment_preack_mode(mode: u32) -> bool {
    mode == 3
}

fn g17p_native_fragment_outer_consumed(counters: [u32; 3], expected: u32) -> bool {
    counters[0] == expected && counters[2] == expected
}

/// Native TA hardware-buffer ID namespace (`AGXAccelerator + 0x13380`).
///
/// `AGXHardwareBufferIDManager::init` receives 0x7f. Its initial free stack
/// contains 126..0 and is popped from the end, so allocations are exactly
/// 0, 1, ... 126. The bitmap fallback runs only after that stack is empty.
/// The kext's owner array, reference counts, bitmap, and free stack are CPU
/// bookkeeping, but the allocated number has five coupled firmware-visible
/// consumers: TA register 0x1c830, command +0x18, ConfigUpdate +0x7e,
/// PBState +0x0c, and the PBDesc ring index.
const G17P_TA_HARDWARE_BUFFER_ID_COUNT: usize = 0x7f;

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
enum G17PHardwareBufferIdError {
    Exhausted,
    InvalidId,
    DoubleFree,
    NotReserved,
}

/// `g17p_render_slot_globals` bit: derive the outer ring entry's "channel
/// started" byte from the selected native queue pair's retained channel state.
pub(crate) const G17P_RENDER_SLOT_GLOBAL_CHANNEL_STARTED: u32 = 1;
/// `g17p_render_slot_globals` bit: tag-15 `+0x3e` publishes the device-global
/// FList lifecycle sequence rather than the slot's ordinal.
pub(crate) const G17P_RENDER_SLOT_GLOBAL_FLIST_GENERATION: u32 = 2;

fn g17p_render_slot_globals() -> u32 {
    *module_parameters::g17p_render_slot_globals.value()
}

fn g17p_render_pair_first_outer_submit(started: &[bool; 2], slot: u8) -> Option<bool> {
    started.get(usize::from(slot)).map(|started| !*started)
}

const G17P_COMMAND_BUFFER_STATE_COUNT: usize = 64;
const G17P_COMMAND_BUFFER_EXCLUDED_STATE: usize = 63;

/// Host mirror of the native AGXShared command-buffer-state allocator.
///
/// Native keeps a full u32 allocation sequence at Shared +0x180 and a
/// 64-entry event array at +0x190. Candidate state is `sequence & 63`; state
/// 63 is skipped, and a state whose prior event has not completed is scanned
/// past under the Shared lock. The selected 36-object command-buffer ring is
/// a separate object lifetime: its selected object merely records the full
/// sequence word. Linux serializes this manager already, so exact live tags
/// are sufficient to model event availability without another lock.
#[derive(Debug, Clone)]
struct G17PCommandBufferStateAllocator {
    next: u32,
    live: [Option<u32>; G17P_COMMAND_BUFFER_STATE_COUNT],
}

impl G17PCommandBufferStateAllocator {
    const fn new() -> Self {
        Self {
            next: 0,
            live: [None; G17P_COMMAND_BUFFER_STATE_COUNT],
        }
    }

    fn allocate_one(&mut self) -> Option<u32> {
        let mut candidate = self.next;
        for _ in 0..G17P_COMMAND_BUFFER_STATE_COUNT {
            let state = (candidate as usize) & (G17P_COMMAND_BUFFER_STATE_COUNT - 1);
            if state != G17P_COMMAND_BUFFER_EXCLUDED_STATE && self.live[state].is_none() {
                self.live[state] = Some(candidate);
                self.next = candidate.wrapping_add(1);
                return Some(candidate);
            }
            candidate = candidate.wrapping_add(1);
            self.next = candidate;
        }
        None
    }

    fn release_one(&mut self, word: u32) -> bool {
        let state = (word as usize) & (G17P_COMMAND_BUFFER_STATE_COUNT - 1);
        if state == G17P_COMMAND_BUFFER_EXCLUDED_STATE || self.live[state] != Some(word) {
            return false;
        }
        self.live[state] = None;
        true
    }

}

#[derive(Debug, Clone)]
struct G17PHardwareBufferIdAllocator {
    free: [u64; 2],
    stack: [u8; G17P_TA_HARDWARE_BUFFER_ID_COUNT],
    stack_len: usize,
    next_parameter_buffer_token: u64,
    /// IDs owned by a live retained render storage. A storage's
    /// parameter-management graph and PBDesc slot are encoded against one ID
    /// for the storage's whole lifetime (`prepare_retained_scene` refuses any
    /// other), so between that storage's submissions the ID is free but must
    /// never be handed to a different owner: it leaves this set only when the
    /// storage is dropped.
    reserved: [u64; 2],
}

impl G17PHardwareBufferIdAllocator {
    fn new() -> Self {
        let mut stack = [0u8; G17P_TA_HARDWARE_BUFFER_ID_COUNT];
        for (index, slot) in stack.iter_mut().enumerate() {
            *slot = (G17P_TA_HARDWARE_BUFFER_ID_COUNT - 1 - index) as u8;
        }
        Self {
            // 127 valid bits: IDs 0..126.
            free: [u64::MAX, u64::MAX >> 1],
            stack,
            stack_len: G17P_TA_HARDWARE_BUFFER_ID_COUNT,
            next_parameter_buffer_token: 0,
            reserved: [0; 2],
        }
    }

    fn is_free(&self, id: u32) -> bool {
        let index = id as usize;
        index < G17P_TA_HARDWARE_BUFFER_ID_COUNT
            && self.free[index / 64] & (1u64 << (index % 64)) != 0
    }

    fn is_reserved(&self, id: u32) -> bool {
        let index = id as usize;
        index < G17P_TA_HARDWARE_BUFFER_ID_COUNT
            && self.reserved[index / 64] & (1u64 << (index % 64)) != 0
    }

    fn push_stack(&mut self, id: u32) {
        if self.stack[..self.stack_len].contains(&(id as u8)) {
            return;
        }
        if self.stack_len < self.stack.len() {
            self.stack[self.stack_len] = id as u8;
            self.stack_len += 1;
        }
    }

    fn remove_from_stack(&mut self, id: u32) {
        if let Some(at) = self.stack[..self.stack_len].iter().position(|entry| *entry == id as u8) {
            self.stack.copy_within(at + 1..self.stack_len, at);
            self.stack_len -= 1;
        }
    }

    /// Hand out an ID no live storage owns, in native order.
    fn allocate(&mut self) -> core::result::Result<u32, G17PHardwareBufferIdError> {
        let mut popped = None;
        while self.stack_len != 0 {
            self.stack_len -= 1;
            let candidate = self.stack[self.stack_len] as u32;
            // A reserved ID is removed from the stack when reserved; skipping
            // one here is defence in depth, never the normal path.
            if !self.is_reserved(candidate) {
                popped = Some(candidate);
                break;
            }
        }
        let id = if let Some(id) = popped {
            id
        } else {
            let available = [
                self.free[0] & !self.reserved[0],
                self.free[1] & !self.reserved[1],
            ];
            if available[0] != 0 {
                available[0].trailing_zeros()
            } else if available[1] != 0 {
                64 + available[1].trailing_zeros()
            } else {
                return Err(G17PHardwareBufferIdError::Exhausted);
            }
        };
        if !self.is_free(id) {
            return Err(G17PHardwareBufferIdError::DoubleFree);
        }
        let index = id as usize;
        self.free[index / 64] &= !(1u64 << (index % 64));
        Ok(id)
    }

    /// Allocate a retained compute owner from the same HardwareBufferID
    /// namespace as render. ID zero belongs to the bootstrap/render owner;
    /// native's captured retained CL queues then receive 1, 2 and 3 in
    /// creation order. A compute owner keeps its ID until the firmware session
    /// is stopped because its tag-15 record and QoS scheduler-state slot both
    /// retain that identity.
    fn allocate_compute(&mut self) -> core::result::Result<u32, G17PHardwareBufferIdError> {
        for id in 1..G17P_TA_HARDWARE_BUFFER_ID_COUNT as u32 {
            if self.is_free(id) && !self.is_reserved(id) {
                let index = id as usize;
                self.free[index / 64] &= !(1u64 << (index % 64));
                self.remove_from_stack(id);
                return Ok(id);
            }
        }
        Err(G17PHardwareBufferIdError::Exhausted)
    }

    /// Mark `id` as owned by a live retained storage. Usually called while the
    /// storage's first submission still holds the ID; tolerated at any time.
    fn reserve(&mut self, id: u32) -> core::result::Result<(), G17PHardwareBufferIdError> {
        let index = id as usize;
        if index >= G17P_TA_HARDWARE_BUFFER_ID_COUNT {
            return Err(G17PHardwareBufferIdError::InvalidId);
        }
        self.reserved[index / 64] |= 1u64 << (index % 64);
        self.remove_from_stack(id);
        Ok(())
    }

    /// End a storage's ownership of `id`. If no submission holds it, it goes
    /// back on the free list for the next owner. A no-op for an unreserved ID.
    fn unreserve(&mut self, id: u32) {
        if !self.is_reserved(id) {
            return;
        }
        let index = id as usize;
        self.reserved[index / 64] &= !(1u64 << (index % 64));
        if self.is_free(id) {
            self.push_stack(id);
        }
    }

    /// Take the ID a retained storage already owns for one more submission.
    fn acquire_reserved(&mut self, id: u32) -> core::result::Result<u32, G17PHardwareBufferIdError> {
        if !self.is_reserved(id) {
            return Err(G17PHardwareBufferIdError::NotReserved);
        }
        if !self.is_free(id) {
            return Err(G17PHardwareBufferIdError::DoubleFree);
        }
        let index = id as usize;
        self.free[index / 64] &= !(1u64 << (index % 64));
        Ok(id)
    }

    fn next_token(&mut self) -> u64 {
        let token = if self.next_parameter_buffer_token == u64::MAX {
            0
        } else {
            self.next_parameter_buffer_token
        };
        self.next_parameter_buffer_token = token.wrapping_add(1);
        token
    }

    /// `acquire_reserved` plus a fresh PB owner lease token, exactly as
    /// `allocate_with_token` pairs them for a fresh ID.
    fn acquire_reserved_with_token(
        &mut self,
        id: u32,
    ) -> core::result::Result<(u32, u64), G17PHardwareBufferIdError> {
        let id = self.acquire_reserved(id)?;
        Ok((id, self.next_token()))
    }

    /// Acquire a new PB owner lease, not a render-ordinal-derived generation.
    /// Native IDManager::alloc calls HWPB::prepareBufferResources(true) only
    /// on the reference-count 0 -> 1 transition (8a66f24..8a66f6c). That
    /// prepare uses a shared u64 counter, skipping -1 (8a741f0..8a7421c).
    /// Our serialized allocator has no already-referenced-ID acquisition;
    /// every successful allocation is that transition. Keep the counter
    /// across releases and processor-stop recovery, independently of Scene,
    /// queue ordinals and the separate FList generation namespace.
    fn allocate_with_token(
        &mut self,
    ) -> core::result::Result<(u32, u64), G17PHardwareBufferIdError> {
        let id = self.allocate()?;
        Ok((id, self.next_token()))
    }

    /// Return a submission's hold on `id`. A reserved ID stays with its
    /// storage -- off the free list -- until that storage unreserves it.
    fn release(&mut self, id: u32) -> core::result::Result<(), G17PHardwareBufferIdError> {
        let index = id as usize;
        if index >= G17P_TA_HARDWARE_BUFFER_ID_COUNT {
            return Err(G17PHardwareBufferIdError::InvalidId);
        }
        if self.is_free(id) {
            return Err(G17PHardwareBufferIdError::DoubleFree);
        }
        self.free[index / 64] |= 1u64 << (index % 64);
        if !self.is_reserved(id) {
            self.push_stack(id);
        }
        Ok(())
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) struct G17PBootstrapUmaCompletion {
    pub(crate) stamp: i32,
    pub(crate) hardware_buffer_id: u32,
    pub(crate) shared_control: u64,
    pub(crate) operand_table: u64,
    pub(crate) cookie: u64,
    pub(crate) count: u32,
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) struct G17PBootstrapUmaRelease {
    pub(crate) sequence: u64,
    pub(crate) hardware_buffer_id: u32,
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
enum G17PBootstrapUmaRequestState {
    Submitted,
    Ready,
    Published,
    Sent,
    Completed,
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
enum G17PBootstrapUmaLifecycleError {
    WrongPhase,
    WrongCompletion,
    WrongOwner,
    WrongReferenceCount,
    WrongPendingCount,
}

/// One command-pair lease on a retained FList. Pool bytes outlive this lease.
/// Native updateSubmitInfo increments pending command updates independently
/// of optional async growth; final reference retirement emits one 0x2e using
/// the command's captured generation (KEXT 8aa1804/8aa5340).
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
struct G17PBootstrapUmaLifecycle {
    sequence: u64,
    hardware_buffer_id: u32,
    hardware_buffer_owned: bool,
    hardware_buffer_references: u32,
    pending_release_count: u32,
    release_producer_after: Option<u32>,
    request: Option<G17PBootstrapUmaCompletion>,
    state: G17PBootstrapUmaRequestState,
}

impl G17PBootstrapUmaLifecycle {
    const fn new() -> Self {
        Self {
            sequence: G17P_BOOTSTRAP_UMA_SEQUENCE,
            hardware_buffer_id: G17P_BOOTSTRAP_UMA_HARDWARE_BUFFER_ID,
            hardware_buffer_owned: true,
            // prepareBufferResources, Async updateSubmitInfo, TA and 3D each
            // hold one reference. Publication retires prepare; type 13 retires
            // only the async-grow request. Metal keeps the two command refs
            // until normal paired render completion, so type 13 is not the
            // point at which opcode 0x2e may be emitted.
            hardware_buffer_references: 4,
            pending_release_count: 2,
            release_producer_after: None,
            request: Some(G17PBootstrapUmaCompletion {
                stamp: G17P_BOOTSTRAP_UMA_STAMP,
                hardware_buffer_id: G17P_BOOTSTRAP_UMA_HARDWARE_BUFFER_ID,
                shared_control: g17_initdata::CONTROL_SHARED_ADDRESS,
                operand_table: g17_initdata::CONTROL_OPERAND_TABLE_ADDRESS,
                cookie: G17P_BOOTSTRAP_UMA_COOKIE,
                count: G17P_BOOTSTRAP_UMA_COUNT,
            }),
            state: G17PBootstrapUmaRequestState::Submitted,
        }
    }

    /// The current fixed pool has one cold grow request. Reacquiring the
    /// unchanged pool creates command references, not another grow request.
    /// B1 12278..12298 calls 1b230 from warm tag-15 even with install=0;
    /// this rebinds the owner/generation invalidated by the preceding 0x2e.
    fn acquire(
        previous: Option<&Self>,
        sequence: u64,
    ) -> core::result::Result<Self, G17PBootstrapUmaLifecycleError> {
        let Some(previous) = previous else {
            return if sequence == G17P_BOOTSTRAP_UMA_SEQUENCE {
                Ok(Self::new())
            } else {
                Err(G17PBootstrapUmaLifecycleError::WrongOwner)
            };
        };
        if previous.hardware_buffer_id != G17P_BOOTSTRAP_UMA_HARDWARE_BUFFER_ID
            || previous.sequence.checked_add(1) != Some(sequence)
        {
            return Err(G17PBootstrapUmaLifecycleError::WrongOwner);
        }
        let overlapping = previous.state == G17PBootstrapUmaRequestState::Ready
            && previous.hardware_buffer_owned
            && previous.hardware_buffer_references >= 2
            && previous.pending_release_count >= 2;
        let reacquire = previous.state == G17PBootstrapUmaRequestState::Completed
            && !previous.hardware_buffer_owned
            && previous.hardware_buffer_references == 0
            && previous.pending_release_count == 0;
        if !overlapping && !reacquire {
            return Err(G17PBootstrapUmaLifecycleError::WrongPhase);
        }
        Ok(Self {
            sequence,
            hardware_buffer_id: previous.hardware_buffer_id,
            hardware_buffer_owned: true,
            hardware_buffer_references: previous
                .hardware_buffer_references
                .checked_add(3)
                .ok_or(G17PBootstrapUmaLifecycleError::WrongReferenceCount)?,
            pending_release_count: previous
                .pending_release_count
                .checked_add(2)
                .ok_or(G17PBootstrapUmaLifecycleError::WrongPendingCount)?,
            release_producer_after: None,
            request: None,
            state: G17PBootstrapUmaRequestState::Submitted,
        })
    }

    fn retire_prepare_reference(
        &mut self,
    ) -> core::result::Result<(), G17PBootstrapUmaLifecycleError> {
        if self.state != G17PBootstrapUmaRequestState::Submitted
            || !self.hardware_buffer_owned
            || self.hardware_buffer_references < if self.request.is_some() { 4 } else { 3 }
        {
            return Err(G17PBootstrapUmaLifecycleError::WrongPhase);
        }
        self.hardware_buffer_references -= 1;
        if self.request.is_none() {
            self.state = G17PBootstrapUmaRequestState::Ready;
        }
        Ok(())
    }

    fn match_completion(
        &mut self,
        completion: G17PBootstrapUmaCompletion,
    ) -> core::result::Result<(), G17PBootstrapUmaLifecycleError> {
        if self.state != G17PBootstrapUmaRequestState::Submitted {
            return Err(G17PBootstrapUmaLifecycleError::WrongPhase);
        }
        if Some(completion) != self.request {
            return Err(G17PBootstrapUmaLifecycleError::WrongCompletion);
        }
        if !self.hardware_buffer_owned
            || completion.hardware_buffer_id != self.hardware_buffer_id
        {
            return Err(G17PBootstrapUmaLifecycleError::WrongOwner);
        }
        if self.hardware_buffer_references != 3 {
            return Err(G17PBootstrapUmaLifecycleError::WrongReferenceCount);
        }
        if self.pending_release_count == 0 {
            return Err(G17PBootstrapUmaLifecycleError::WrongPendingCount);
        }

        self.hardware_buffer_references = 2;
        self.state = G17PBootstrapUmaRequestState::Ready;
        Ok(())
    }

    fn retire_render_references(
        &mut self,
    ) -> core::result::Result<Option<G17PBootstrapUmaRelease>, G17PBootstrapUmaLifecycleError> {
        if self.state != G17PBootstrapUmaRequestState::Ready
            || !self.hardware_buffer_owned
            || self.hardware_buffer_references < 2
        {
            return Err(G17PBootstrapUmaLifecycleError::WrongPhase);
        }
        if self.pending_release_count < 2 {
            return Err(G17PBootstrapUmaLifecycleError::WrongPendingCount);
        }
        self.hardware_buffer_references -= 2;
        self.pending_release_count -= 2;
        if self.hardware_buffer_references != 0 {
            return Ok(None);
        }
        self.hardware_buffer_owned = false;
        Ok(Some(G17PBootstrapUmaRelease {
            sequence: self.sequence,
            hardware_buffer_id: self.hardware_buffer_id,
        }))
    }

    fn pending_release(&self) -> Option<G17PBootstrapUmaRelease> {
        (self.state == G17PBootstrapUmaRequestState::Ready
            && self.hardware_buffer_references == 0)
        .then_some(
            G17PBootstrapUmaRelease {
                sequence: self.sequence,
                hardware_buffer_id: self.hardware_buffer_id,
            },
        )
    }

    fn mark_published(
        &mut self,
        release: G17PBootstrapUmaRelease,
    ) -> core::result::Result<(), G17PBootstrapUmaLifecycleError> {
        if self.pending_release() != Some(release) {
            return Err(G17PBootstrapUmaLifecycleError::WrongPhase);
        }
        self.state = G17PBootstrapUmaRequestState::Published;
        Ok(())
    }

    fn mark_sent(
        &mut self,
        producer_after: u32,
    ) -> core::result::Result<(), G17PBootstrapUmaLifecycleError> {
        if self.state != G17PBootstrapUmaRequestState::Published
            || self.release_producer_after.is_some()
        {
            return Err(G17PBootstrapUmaLifecycleError::WrongPhase);
        }
        self.release_producer_after = Some(producer_after);
        self.state = G17PBootstrapUmaRequestState::Sent;
        Ok(())
    }

    fn release_sent(&self) -> bool {
        self.state == G17PBootstrapUmaRequestState::Sent
            && self.release_producer_after.is_some()
    }

    fn mark_completed(
        &mut self,
        producer_after: u32,
    ) -> core::result::Result<(), G17PBootstrapUmaLifecycleError> {
        if !self.release_sent() || self.release_producer_after != Some(producer_after) {
            return Err(G17PBootstrapUmaLifecycleError::WrongPhase);
        }
        self.release_producer_after = None;
        self.state = G17PBootstrapUmaRequestState::Completed;
        Ok(())
    }

    fn observe_release_consumer(
        &mut self,
        consumer: u32,
    ) -> core::result::Result<bool, G17PBootstrapUmaLifecycleError> {
        let target = self
            .release_producer_after
            .ok_or(G17PBootstrapUmaLifecycleError::WrongPhase)?;
        if !self.release_sent() {
            return Err(G17PBootstrapUmaLifecycleError::WrongPhase);
        }
        if consumer != target {
            return Ok(false);
        }
        self.mark_completed(target)?;
        Ok(true)
    }
}

// Pinned J700 T8140 GFX A000 scheduler root used by the exact EP type-4
// thunk. The extracted image has SHA-256
// 3e2821bb40d3abffe30488a06ede5f523cfa47544d6a0ad7654a08be6b5dc30f.
// Its __DATA vmaddr/size are 0xfffffc000004c000/0x134000, exactly matching
// the J700 gfx-data carveout size. These addresses are diagnostic only.
const G17P_GFX_LINK_DATA_BASE: u64 = 0xffff_fc00_0004_c000;
const G17P_GFX_LINK_DATA_END: u64 = 0xffff_fc00_0018_0000;
const G17P_GFX_PHYSICAL_DATA_BASE: u64 = 0x0000_0100_01d7_c000;

/// Translate a GFX firmware LINK address into the physical address the host
/// can map. Same arithmetic the render status-page probes use.
pub(crate) fn g17p_gfx_link_to_physical(link: u64) -> u64 {
    G17P_GFX_PHYSICAL_DATA_BASE + (link - G17P_GFX_LINK_DATA_BASE)
}
/// Firmware per-QID dependency records: `0x1261f0 + qid*0x18`, valid byte at
/// `+0`, completed stamp at `+8`. Firmware DATA segment, so host-readable.
const G17P_DEPENDENCY_RECORD_LINK_BASE: u64 = 0xffff_fc00_0012_61f0;
/// GDirector per-HardwareBufferID shadow, `{u8 priority, u8 share}` per entry.
/// `FUN_1451c` writes `sgx+0xd058 = prio | (share << 8)` and mirrors it here,
/// where `share = ((weight << 5) / sum_weights) - 1` clamped at 0 and `0xff`
/// means never programmed. A share of 0 gives the queue no scheduling weight.
const G17P_GDIRECTOR_SHADOW_LINK_BASE: u64 = 0xffff_fc00_0012_f560;
/// Submit-time firmware DRAM the tag-15 handler maintains. These are the only
/// structures the KSM front-end plausibly scans that have never been diffed
/// between a queue that launches and one that does not.
/// Per-DM active-queue bitmap, `0x128a30 + dm*0x10` (+`0x08` is its sibling).
/// The scheduler pass ANDs against exactly these before launching.
const G17P_DM_ACTIVE_BITMAP_LINK_BASE: u64 = 0xffff_fc00_0012_8a30;
/// Per-queue state, `0x127628 + qid*0x28` (written at fw `0x119b4`).
const G17P_QUEUE_STATE_LINK_BASE: u64 = 0xffff_fc00_0012_7628;
/// KSM park record, `0x58010 + qid*0x28`: byte A at +0x418, B at +0x419,
/// C at +0x41a, deadline at +0x400. FW_PROVEN: the scheduler pass compares
/// A and B at 0x13100 and PARKS the queue when they are equal -- KTrace 0x42,
/// runnable bit cleared, then a status-4 NOP retire. A is written only by the
/// admit block at 0x12e38 (which also zeroes B and C); B only by the retire
/// path at 0xcf0c. Both start at zero, so a queue whose admit never ran is
/// parked by construction. Nothing has ever read these bytes from the host.
const G17P_KSM_PARK_RECORD_LINK_BASE: u64 = 0xffff_fc00_0005_8010;
/// Ready mask (fw `0x11b5c`).
const G17P_READY_MASK_LINK_BASE: u64 = 0xffff_fc00_0017_6ec0;
/// `0x175aa0 + qid*0x28 + 0x40` (fw `0x11b3c`).
const G17P_QUEUE_FLAG_LINK_BASE: u64 = 0xffff_fc00_0017_5aa0;
const G17P_DEPENDENCY_RECORD_STRIDE: u64 = 0x18;
const G17P_GFX1_LINK_DATA_BASE: u64 = 0xffff_fc00_0004_c000;
const G17P_GFX1_LINK_DATA_END: u64 = 0xffff_fc00_0017_8000;
const G17P_GFX1_PHYSICAL_DATA_BASE: u64 = 0x0000_0100_01eb_0000;
const G17P_SCHEDULER_ROOT_LINK_ADDRESS: u64 = 0xffff_fc00_0017_7190;
const G17P_SCHEDULER_ROOT_PHYSICAL_ADDRESS: u64 = 0x0000_0100_01ea_7190;
const G17P_SCHEDULER_ROOT_SNAPSHOT_SIZE: usize = 0x80;
const G17P_SCHEDULER_ROOT_PENDING_OFFSET: usize = 0x00;
const G17P_SCHEDULER_ROOT_DEFERRED_OFFSET: usize = 0x50;
const G17P_SCHEDULER_ROOT_STATE_OFFSET: usize = 0x68;
const G17P_RUNTIME_MAIN_GLOBAL_LINK_ADDRESS: u64 = 0xffff_fc00_0017_7030;
const G17P_RUNTIME_MAIN_GLOBAL_PHYSICAL_ADDRESS: u64 = 0x0000_0100_01ea_7030;
const G17P_TYPE4_EP_HANDLER_LINK_ADDRESS: u64 = 0xffff_fc00_0002_2e24;
const G17P_TYPE4_THUNK_LINK_ADDRESS: u64 = 0xffff_fc00_0002_635c;
const G17P_TYPE4_DRAIN_LINK_ADDRESS: u64 = 0xffff_fc00_0000_3a74;
const G17P_TYPE4_PENDING_MASK: u64 = 0x0002_0000;
const G17P_SCHEDULER_DEFERRED_REQUEUE_LINK_ADDRESS: u64 = 0xffff_fc00_0002_5ea4;
const G17P_TYPE4_MAIN_CONTROL_POINTER_OFFSETS: [usize; 3] = [0x1a0, 0x1a8, 0x1b0];
const G17P_MBI_GLOBAL_LINK_ADDRESS: u64 = 0xffff_fc00_0006_e840;
const G17P_MBI_STATIC_ARRAY_LINK_ADDRESS: u64 = 0xffff_fc00_0006_e800;
const G17P_MBI_GROUP_COUNT: u32 = 8;
const G17P_MBI_TRACE_CAPACITY: u32 = 128;
const G17P_MBI_TRACE_RECORD_SIZE: usize = 0x18;
// Primary A000 0x19184..0x191a0 initializes the adjacent endpoint handles;
// EP21 type 9 increments the lease at 0x22de4..0x22df8.
const G17P_PRIMARY_PEER_LEASE_LINK_ADDRESS: u64 = 0xffff_fc00_0017_7798;
const G17P_PRIMARY_PEER_LEASE_PHYSICAL_ADDRESS: u64 = 0x0000_0100_01ea_7798;
// Exact J700 GFX1 A000 (SHA-256 0caf8165...17fd63) initializes handles at
// 0x1bde0..0x1bdfc, increments this lease at 0x26738..0x26748, and decrements
// it only on ACTIVE->SLEEP at 0x25e70..0x25e7c. ACTIVE->NAP bypasses the write.
const G17P_GFX1_PEER_LEASE_LINK_ADDRESS: u64 = 0xffff_fc00_0016_d1c8;
const G17P_GFX1_PEER_LEASE_PHYSICAL_ADDRESS: u64 = 0x0000_0100_01fd_11c8;
const G17P_RTKIT_PRIVATE_ADDRESS_MASK: u64 = (1u64 << 42) - 1;

#[cfg(not(test))]
const _: () = {
    assert!(
        G17P_SCHEDULER_ROOT_PHYSICAL_ADDRESS
            == g17_adt_j700::J700_GFX_DATA.base
                + (G17P_SCHEDULER_ROOT_LINK_ADDRESS - G17P_GFX_LINK_DATA_BASE)
    );
    assert!(
        G17P_SCHEDULER_ROOT_PHYSICAL_ADDRESS + G17P_SCHEDULER_ROOT_SNAPSHOT_SIZE as u64
            <= g17_adt_j700::J700_GFX_DATA.base + g17_adt_j700::J700_GFX_DATA.size
    );
    assert!(G17P_SCHEDULER_ROOT_PHYSICAL_ADDRESS & 7 == 0);
    assert!(G17P_SCHEDULER_ROOT_SNAPSHOT_SIZE & 7 == 0);
    assert!(G17P_SCHEDULER_ROOT_STATE_OFFSET + 1 <= G17P_SCHEDULER_ROOT_SNAPSHOT_SIZE);
    assert!(G17P_GFX_LINK_DATA_END - G17P_GFX_LINK_DATA_BASE == 0x13_4000);
    assert!(G17P_GFX1_LINK_DATA_END - G17P_GFX1_LINK_DATA_BASE == 0x12_c000);
    assert!(G17P_GFX_PHYSICAL_DATA_BASE == g17_adt_j700::J700_GFX_DATA.base);
    assert!(G17P_GFX1_PHYSICAL_DATA_BASE == g17_adt_j700::J700_GFX1_DATA.base);
    assert!(
        G17P_RUNTIME_MAIN_GLOBAL_PHYSICAL_ADDRESS
            == g17_adt_j700::J700_GFX_DATA.base
                + (G17P_RUNTIME_MAIN_GLOBAL_LINK_ADDRESS - G17P_GFX_LINK_DATA_BASE)
    );
    assert!(
        G17P_RUNTIME_MAIN_GLOBAL_PHYSICAL_ADDRESS + core::mem::size_of::<u64>() as u64
            <= g17_adt_j700::J700_GFX_DATA.base + g17_adt_j700::J700_GFX_DATA.size
    );
    assert!(
        G17P_PRIMARY_PEER_LEASE_PHYSICAL_ADDRESS
            == G17P_GFX_PHYSICAL_DATA_BASE
                + (G17P_PRIMARY_PEER_LEASE_LINK_ADDRESS - G17P_GFX_LINK_DATA_BASE)
    );
    assert!(
        G17P_GFX1_PEER_LEASE_PHYSICAL_ADDRESS
            == G17P_GFX1_PHYSICAL_DATA_BASE
                + (G17P_GFX1_PEER_LEASE_LINK_ADDRESS - G17P_GFX1_LINK_DATA_BASE)
    );
};

/// Grounded manager inputs for T8140. Power and tuning values are excluded.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) struct T8140ManagerConfig {
    pub(crate) chip_id: u32,
    pub(crate) base_clock_hz: u32,
    pub(crate) hw_family: u8,
    pub(crate) hw_variant: u8,
    pub(crate) num_dies: u32,
    pub(crate) uat_ias: u8,
    pub(crate) uat_oas: u32,
    pub(crate) uat_page_size: u16,
    pub(crate) uat_page_bits: u8,
    pub(crate) uat_levels: [(u8, u16); g17_initdata::UAT_LEVEL_COUNT],
    pub(crate) initdata_version: [u16; 4],
    pub(crate) root_sizes: [usize; 2],
    pub(crate) secondary_root_delta: u64,
    pub(crate) secondary_shared_region_delta: u64,
    pub(crate) shared_cluster_size: usize,
    pub(crate) work_channel_count: usize,
}

/// Exact facts needed to construct, but not execute, a T8140 manager.
pub(crate) const T8140_MANAGER_CONFIG: T8140ManagerConfig = T8140ManagerConfig {
    chip_id: 0x8140,
    base_clock_hz: 24_000_000,
    hw_family: 0x0a,
    hw_variant: 0,
    num_dies: 1,
    uat_ias: 42,
    uat_oas: 42,
    uat_page_size: g17_initdata::UAT_PAGE_SIZE,
    uat_page_bits: g17_initdata::UAT_PAGE_BITS,
    uat_levels: g17_initdata::UAT_LEVELS,
    initdata_version: g17_initdata::INITDATA_VERSION,
    root_sizes: [
        g17_initdata::ROOT_SIZE_PRIMARY,
        g17_initdata::ROOT_SIZE_SECONDARY,
    ],
    secondary_root_delta: g17_initdata::SECONDARY_ROOT_DELTA,
    secondary_shared_region_delta: g17_initdata::SECONDARY_SHARED_REGION_DELTA,
    shared_cluster_size: g17_initdata::NATIVE_SHARED_CLUSTER_SIZE,
    work_channel_count: g17_initdata::WORK_CHANNEL_COUNT,
};

/// Platform-derived SKSM scratch offsets within the mapped accelerator
/// register window.
///
/// Apple's host loads these three values from accelerator object fields
/// `+0x10730`, `+0x10734`, and `+0x10738`. Those are object offsets, not SGX
/// MMIO offsets. This type carries the loaded values and no physical base.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) struct G17SksmScratchGeometry {
    pub(crate) write0_base: u32,
    pub(crate) write1_base: u32,
    pub(crate) stride: u32,
}

/// The two offsets used by the fixed G17P scratch FIFO slot.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) struct G17SksmScratchOffsets {
    pub(crate) write0: u32,
    pub(crate) write1: u32,
}

impl G17SksmScratchGeometry {
    /// Apply the exact host equation for fixed FIFO slot 9.
    ///
    /// The slot is independent of the SKSM queue ID carried in each write.
    pub(crate) const fn fifo_offsets(
        self,
    ) -> core::result::Result<G17SksmScratchOffsets, ConstructionError> {
        const FIFO_SLOT: u32 = 9;

        let fifo_offset = match self.stride.checked_mul(FIFO_SLOT) {
            Some(offset) => offset,
            None => return Err(ConstructionError::SksmScratchOffsetOverflow),
        };
        let write0 = match self.write0_base.checked_add(fifo_offset) {
            Some(offset) => offset,
            None => return Err(ConstructionError::SksmScratchOffsetOverflow),
        };
        let write1 = match self.write1_base.checked_add(fifo_offset) {
            Some(offset) => offset,
            None => return Err(ConstructionError::SksmScratchOffsetOverflow),
        };

        Ok(G17SksmScratchOffsets { write0, write1 })
    }
}

/// Target configuration supplied by platform discovery.
///
/// There is no default T8140 scratch geometry. A caller must recover the
/// three values through the platform configuration path before construction.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) struct G17PPlatformConfig {
    pub(crate) sksm_scratch: G17SksmScratchGeometry,
}

/// G17P values written by `AGXAcceleratorG16::configureDevice`.
///
/// At `0xfffffe0008a55674..0xfffffe0008a55684`, the host copies the constant
/// at `0xfffffe000714b348` (`00 40 00 00 08 40 00 00`) into accelerator
/// fields `+0x10730/+0x10734`, then writes `0x10` to `+0x10738`.
pub(crate) const T8140_G17P_PLATFORM_CONFIG: G17PPlatformConfig = G17PPlatformConfig {
    sksm_scratch: G17SksmScratchGeometry {
        write0_base: 0x4000,
        write1_base: 0x4008,
        stride: 0x10,
    },
};

/// SoC and register identity presented to the manager constructor.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) struct TargetObservation {
    pub(crate) chip_id: u32,
    pub(crate) declared_gen: GpuGen,
    pub(crate) declared_variant: GpuVariant,
    pub(crate) hw_family: u8,
    pub(crate) hw_variant: u8,
    pub(crate) num_dies: u32,
    pub(crate) uat_ias: u8,
    pub(crate) uat_oas: u32,
}

/// Ownership and placement facts copied from the retained resource owner.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) struct ResourceObservation {
    pub(crate) uat_ttb_base: u64,
    pub(crate) primary_uat_owner: u64,
    pub(crate) secondary_uat_owner: u64,
    pub(crate) primary_role: InstanceRole,
    pub(crate) secondary_role: InstanceRole,
    pub(crate) primary_root: u64,
    pub(crate) secondary_root: u64,
    pub(crate) shared_cluster_size: usize,
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) enum ConstructionError {
    WrongChip,
    WrongDeclaredIdentity,
    WrongHardwareIdentity,
    WrongUatGeometry,
    UndecodableIdentity,
    WrongDecodedIdentity,
    NullUatOwner,
    SplitUatOwner,
    WrongRoleTopology,
    WrongRootPlacement,
    WrongSharedClusterSize,
    WrongSksmScratchGeometry,
    SksmScratchOffsetOverflow,
}

/// Validate the exact T8140 identity without consulting an AGX2 `HwConfig`.
pub(crate) fn validate_target(
    target: TargetObservation,
) -> core::result::Result<(), ConstructionError> {
    let config = &T8140_MANAGER_CONFIG;

    if target.chip_id != config.chip_id {
        return Err(ConstructionError::WrongChip);
    }
    if target.declared_gen != GpuGen::G17 || target.declared_variant != GpuVariant::P {
        return Err(ConstructionError::WrongDeclaredIdentity);
    }
    if target.hw_family != config.hw_family
        || target.hw_variant != config.hw_variant
        || target.num_dies != config.num_dies
    {
        return Err(ConstructionError::WrongHardwareIdentity);
    }
    if target.uat_ias != config.uat_ias || target.uat_oas != config.uat_oas {
        return Err(ConstructionError::WrongUatGeometry);
    }

    let num_dies =
        u8::try_from(target.num_dies).map_err(|_| ConstructionError::UndecodableIdentity)?;
    let decoded = identity::decode_gpu_identity(target.hw_family, target.hw_variant, num_dies)
        .ok_or(ConstructionError::UndecodableIdentity)?;
    if decoded.gpu_gen != GpuGen::G17
        || decoded.gpu_variant != GpuVariant::P
        || decoded.usc_generation != 3
        || decoded.gpu_hal_generation != GpuHalGeneration::Hal200
        || decoded.firmware_roles != FirmwareRoleTopology::Dual
        || decoded.submission_transport != SubmissionTransport::ClassicAndSksm
        || decoded.uat_input_address_bits != config.uat_ias
    {
        return Err(ConstructionError::WrongDecodedIdentity);
    }

    Ok(())
}

/// Validate that both roles are retained under one UAT with native placement.
pub(crate) fn validate_resources(
    resources: ResourceObservation,
) -> core::result::Result<(), ConstructionError> {
    if resources.uat_ttb_base == 0
        || resources.primary_uat_owner == 0
        || resources.secondary_uat_owner == 0
    {
        return Err(ConstructionError::NullUatOwner);
    }
    if resources.uat_ttb_base != resources.primary_uat_owner
        || resources.uat_ttb_base != resources.secondary_uat_owner
    {
        return Err(ConstructionError::SplitUatOwner);
    }
    if resources.primary_role != InstanceRole::Primary
        || resources.secondary_role != InstanceRole::Secondary
    {
        return Err(ConstructionError::WrongRoleTopology);
    }
    let secondary_root = resources
        .primary_root
        .checked_add(T8140_MANAGER_CONFIG.secondary_root_delta)
        .ok_or(ConstructionError::WrongRootPlacement)?;
    if resources.secondary_root != secondary_root {
        return Err(ConstructionError::WrongRootPlacement);
    }
    if resources.shared_cluster_size != T8140_MANAGER_CONFIG.shared_cluster_size {
        return Err(ConstructionError::WrongSharedClusterSize);
    }

    Ok(())
}

/// Admit only the exact geometry produced by the pinned G17P host.
pub(crate) fn validate_platform(
    platform: G17PPlatformConfig,
) -> core::result::Result<(), ConstructionError> {
    if platform != T8140_G17P_PLATFORM_CONFIG {
        return Err(ConstructionError::WrongSksmScratchGeometry);
    }
    platform.sksm_scratch.fifo_offsets()?;
    Ok(())
}

#[cfg(not(test))]
fn target_observation(soc: &'static agx3::SocConfig) -> TargetObservation {
    TargetObservation {
        chip_id: soc.chip_id,
        declared_gen: soc.gpu_gen,
        declared_variant: soc.gpu_variant,
        hw_family: soc.hw_family,
        hw_variant: soc.hw_variant,
        num_dies: soc.num_dies,
        uat_ias: soc.uat_ias,
        uat_oas: soc.uat_oas,
    }
}

#[cfg(not(test))]
fn resource_observation(
    handoff: &g17_resources::G17ResourceHandoff,
    uat: &mmu::Uat,
) -> ResourceObservation {
    ResourceObservation {
        uat_ttb_base: uat.ttb_base(),
        primary_uat_owner: handoff.primary.uat_owner,
        secondary_uat_owner: handoff.secondary.uat_owner,
        primary_role: handoff.primary.role,
        secondary_role: handoff.secondary.role,
        primary_root: handoff.primary.instance.root,
        secondary_root: handoff.secondary.instance.root,
        shared_cluster_size: handoff.shared.hw_data_bundle_alloc,
    }
}

/// Retained construction state for both T8140 firmware roles.
///
/// Creating this object allocates and validates resources. It performs no
/// firmware publication, ASC start, MMIO write, or submission.
#[cfg(not(test))]
pub(crate) struct G17PManagerConstruction {
    resources: g17_resources::G17ResourceOwner,
    platform: G17PPlatformConfig,
    num_clusters: u32,
    render_storage: Option<g17_resources::G17PUserRenderStorage>,
    render_tracker: Option<G17PPartialOpeningRenderTracker>,
    render_bind: Option<G17PRenderRootOwner>,
    /// Root replaced by an in-place same-VM context update. The replacement
    /// descriptor is not a release witness until the pair carrying that update
    /// completes and its EventControl record is unlinked.
    render_context_retirement: Option<G17PRenderContextRetirement>,
    /// Number of successfully completed context-update groups on each physical
    /// render pair. Retired graphs name the next epoch as their release fence.
    render_context_epochs: [u64; 2],
    /// Native secure-GART view of the active render process resources in
    /// hardware context 0. It is installed only for the VM which owns the
    /// retained render graph. Replaced only after paired completion plus
    /// FList release, or after both firmware CPUs stop.
    render_context_aliases: Option<mmu::T8140NativeContextAliases>,
    /// The immediately preceding cross-VM graph. Firmware retirement makes
    /// its queue entries reusable, but native keeps workqueue objects alive
    /// across process switches and may still dereference their job-private
    /// graph after the host-visible EventControl unlink. Keep one complete
    /// generation mapped so the incoming graph cannot immediately reuse and
    /// overwrite the same VA or PBDesc ID.
    retired_render_handoff_graph: Option<G17PRetiredRenderGraph>,
    /// Older VM/context graphs retained past ordinary completion. A finite
    /// completion-ring overwrite is not a release fence; a dead owner's graph
    /// is reclaimed only after a later context-update group on the same
    /// physical pair reaches paired completion and EventControl unlink.
    retired_render_handoff_graphs: KVec<G17PRetiredRenderGraph>,
    /// Firmware background PB maintenance retains manager pointers beyond the
    /// command queue's paired completion and userspace queue destruction. Keep
    /// each firmware-high PM/support pair with its reserved HardwareBufferID.
    /// These quarantined objects are never reinitialized or offered to a new
    /// graph. Both processors stopping drains the remaining inventory.
    retired_parameter_managers: KVec<g17_resources::G17PRetiredParameterManagement>,
    /// Descriptor contexts whose owning DRM queue has been destroyed after
    /// its scheduler entity drained. The bit also prevents a graph which was
    /// concurrently moved into the retirement cache from escaping teardown.
    released_render_contexts: u64,
    render_submission_ordinal: u32,
    runtime_tick_staged: bool,
}

/// A queue which has completed the host's configuration pair and QID-enable
/// publication. Private fields prevent constructing an unregistered queue.
#[cfg(not(test))]
pub(crate) struct G17PSksmQueue {
    queue_id: u8,
    /// Queue-lifetime AGXQOSQueue/HardwareBufferID identity. Native copies it
    /// into ConfigUpdate +0x56 and indexes the QoS scheduler-state table by it.
    hardware_buffer_id: u8,
    scratch: G17SksmScratchGeometry,
    uat_owner: u64,
    disable_pair: g17_submission::G17SksmOrderedWritePair,
    registered: bool,
    storage: g17_resources::G17PClRuntimeStorage,
    compute: g17_resources::G17PComputeDescriptorStorage,
    producer: g17_submission::G17PClKickProducerState,
    submission_ordinal: u32,
    b2_activation: g17_submission::G17PClB2ActivationState,
}

/// Command-owned fields used to build one compute descriptor and CL entry.
/// The queue supplies descriptor storage, sequencing, preemption storage, and
/// the retained support graph.
#[cfg(not(test))]
pub(crate) struct G17PComputeSubmissionContext<'a> {
    pub(crate) context_id: u32,
    pub(crate) dispatch_identity: u64,
    pub(crate) execution_gate: u64,
    pub(crate) timestamps: g17_uapi::ComputeTimestampAddresses,
    pub(crate) descriptor_flag_4c: bool,
    pub(crate) descriptor_flag_5c8: bool,
    pub(crate) converted_command_timestamp: u64,
    pub(crate) barriers: &'a [g17_submission::G17PClBarrierDependency],
    pub(crate) mcache: Option<g17_submission::G17PClMcacheAperture>,
    pub(crate) rce_kind: u8,
    pub(crate) auxiliary: u64,
}

/// User-VM aliases and its active TTBAT binding. The live runtime retains this
/// object until the entry completes or both processors stop during recovery.
#[cfg(not(test))]
pub(crate) struct G17PComputeUserBinding {
    mappings: g17_resources::G17PComputeUserMappings,
    _vm_bind: mmu::VmBind,
}

#[cfg(not(test))]
pub(crate) struct G17PComputeTimeoutSnapshot {
    pub(crate) graph: g17_resources::G17PComputeGraphSnapshot,
    pub(crate) channel: g17_resources::G17PComputeChannelSnapshot,
    pub(crate) control: g17_resources::G17PControlCounters,
}

/// The two complete queue groups exposed by the exact first graphics publish.
#[cfg(not(test))]
pub(crate) struct G17PPartialOpeningRenderSubmission {
    pub(crate) fragment_queue_gpu_va: u64,
    pub(crate) fragment_queue_write_index: u32,
    pub(crate) tiling_queue_gpu_va: u64,
    pub(crate) tiling_queue_write_index: u32,
}

/// Completion cursors retained across observations of one graphics pair.
#[cfg(not(test))]
pub(crate) struct G17PPartialOpeningRenderTracker {
    tiling_previous: g17_completion::QueueIndices,
    fragment_previous: g17_completion::QueueIndices,
    tiling_prefix: u32,
    fragment_prefix: u32,
}

#[cfg(not(test))]
impl G17PManagerConstruction {
    pub(crate) fn new(
        dev: &AsahiDevice,
        soc: &'static agx3::SocConfig,
        platform: G17PPlatformConfig,
        num_clusters: u32,
    ) -> Result<KBox<Self>> {
        let uat = KBox::new(mmu::Uat::new_t8140(dev, true)?, GFP_KERNEL)?;
        Self::new_with_uat(dev, soc, platform, num_clusters, uat)
    }

    /// Kept out of line and heap-returned: the construction locals plus the
    /// assembled manager otherwise land several times in the probe-path stack
    /// frames, which overflowed the 32 KiB task stack on hardware.
    #[inline(never)]
    pub(crate) fn new_with_uat(
        dev: &AsahiDevice,
        soc: &'static agx3::SocConfig,
        platform: G17PPlatformConfig,
        num_clusters: u32,
        uat: KBox<mmu::Uat>,
    ) -> Result<KBox<Self>> {
        if num_clusters == 0 {
            return Err(EINVAL);
        }
        validate_target(target_observation(soc)).map_err(|_| EINVAL)?;
        validate_platform(platform).map_err(|_| EINVAL)?;

        let resources = g17_resources::G17ResourceOwner::new_with_uat(dev, uat)?;
        g17_resources::validate_handoff(resources.handoff()).map_err(|_| EINVAL)?;
        validate_resources(resource_observation(resources.handoff(), resources.uat()))
            .map_err(|_| EINVAL)?;

        Ok(KBox::new(
            Self {
                resources,
                platform,
                num_clusters,
                render_storage: None,
                render_tracker: None,
                render_bind: None,
                render_submission_ordinal: 0,
                runtime_tick_staged: false,
            },
            GFP_KERNEL,
        )?)
    }

    pub(crate) fn config(&self) -> &'static T8140ManagerConfig {
        &T8140_MANAGER_CONFIG
    }

    pub(crate) fn handoff(&self) -> &g17_resources::G17ResourceHandoff {
        self.resources.handoff()
    }

    pub(crate) fn control_counters(&mut self) -> Result<g17_resources::G17PControlCounters> {
        self.resources.control_counters()
    }

    pub(crate) fn control_opening_effect(
        &mut self,
    ) -> Result<g17_resources::G17PControlOpeningEffect> {
        self.resources.control_opening_effect()
    }

    /// Stage the same queue-pair-zero TA/3D bootstrap used by the
    /// output-positive source path. The VDM stream is a driver-owned terminate
    /// command, so this establishes scheduler lifecycle without user BOs.
    pub(crate) fn stage_compute_bootstrap_render(
        &mut self,
        dev: &AsahiDevice,
    ) -> Result<mmu::Vm> {
        let kernel_range = self.resources.uat().kernel_va_range().inspect_err(|error| {
            dev_err!(
                dev.as_ref(),
                "G17P bootstrap: kernel VA range failed ({:?})\n",
                error
            );
        })?;
        let vm = self
            .new_user_vm_inner(0x8140_b007, kernel_range, true)
            .inspect_err(|error| {
                dev_err!(
                    dev.as_ref(),
                    "G17P bootstrap: render VM setup failed ({:?})\n",
                    error
                );
            })?;
        let mut parameters = g17_render::G17pRenderParameters::source_rsrc8();
        parameters.encoder = g17_resources::G17P_BOOTSTRAP_VDM_VA;
        parameters.process_empty_tiles = false;
        parameters.tile_config &= !0x1_0000;
        let timestamp = g17_uapi::UapiTimestamp {
            handle: 0,
            offset: 0,
        };
        let command = g17_uapi::TranslatedRenderCommand {
            flags: 0,
            vdm_base: parameters.encoder,
            parameters,
            vertex_attachments: g17_uapi::UapiAttachmentList::EMPTY,
            fragment_attachments: g17_uapi::UapiAttachmentList::EMPTY,
            vertex_timestamps: g17_uapi::UapiTimestamps {
                start: timestamp,
                end: timestamp,
            },
            fragment_timestamps: g17_uapi::UapiTimestamps {
                start: timestamp,
                end: timestamp,
            },
        };
        if self.render_tracker.is_some()
            || self.render_storage.is_some()
            || self.render_bind.is_some()
            || self.render_submission_ordinal != 0
        {
            return Err(EBUSY);
        }
        self.render_storage = Some(
            self.resources
                .new_user_render_storage(
                    dev,
                    &vm,
                    &command,
                    self.num_clusters,
                    // The driver-owned opening command has its own Shared
                    // state owner. Its first TA/3D pair uses native tags 0/1;
                    // user command buffers allocate from the session owner
                    // below and are not advanced by this bootstrap object.
                    [0, 1],
                    0,
                    pm_generation.map(|plan| plan.0),
                    &mut no_retained_sksm_backing,
                )
                .inspect_err(|error| {
                    dev_err!(
                        dev.as_ref(),
                        "G17P bootstrap: render graph setup failed ({:?})\n",
                        error
                    );
                })?,
        );
        let storage = self.render_storage.as_mut().ok_or(ENODEV)?;
        if !storage.is_reachable_from(self.resources.uat(), &vm) {
            return Err(EFAULT);
        }
        let binding = self.resources.uat().bind(&vm)?;
        if binding.slot() != 1 {
            return Err(EFAULT);
        }
        self.resources
            .uat()
            .install_t8140_partial_opening_shared_render_roots(&binding)?;
        self.render_bind = Some(binding);
        Ok(vm)
    }

    /// Apply the measured post-opening context split after primary
    /// control-done and before either first-work producer becomes visible.
    pub(crate) fn split_compute_bootstrap_render_contexts(&self) -> Result {
        let binding = self.render_bind.as_ref().ok_or(ENODEV)?;
        self.resources
            .uat()
            .split_t8140_partial_opening_render_roots(binding)?;
        if !self
            .resources
            .uat()
            .t8140_first_work_render_context_ready(binding)
        {
            return Err(EFAULT);
        }
        Ok(())
    }

    /// Publish the staged bootstrap pair after primary control-done.
    pub(crate) fn publish_staged_compute_bootstrap_render(
        &mut self,
    ) -> Result<g17_submission::PreparedG17PPartialOpeningWorkPair> {
        if self.render_tracker.is_some()
            || self.render_bind.is_none()
            || self.render_submission_ordinal != 0
        {
            return Err(EBUSY);
        }
        if !self.resources.uat().t8140_first_work_render_context_ready(
            self.render_bind.as_ref().ok_or(ENODEV)?,
        ) {
            return Err(EFAULT);
        }
        let (fragment_queue, tiling_queue, previous, late_tiling) = {
            let storage = self.render_storage.as_mut().ok_or(ENODEV)?;
            // The successful first-partial path keeps both inner groups blank
            // until primary control-done. Restore 3D now; TA remains held by
            // the late publication callback below.
            storage.stage_fragment(0)?;
            let previous = storage.snapshot()?;
            let late_tiling = storage.prepare_late_tiling_publication(0)?;
            let (fragment_queue, tiling_queue) = storage.queue_gpu_vas();
            (fragment_queue, tiling_queue, previous, late_tiling)
        };
        let (prepared, render_bind) = self.resources.publish_partial_opening_pair(
            None,
            true,
            fragment_queue,
            3,
            tiling_queue,
            3,
            move || late_tiling.publish(),
        )?;
        if render_bind.is_some() {
            return Err(EINVAL);
        }
        self.render_tracker = Some(G17PPartialOpeningRenderTracker {
            tiling_previous: previous.tiling,
            fragment_previous: previous.fragment,
            tiling_prefix: 3,
            fragment_prefix: 3,
        });
        Ok(prepared)
    }

    /// Match the source bootstrap fence: both three-item inner queues and
    /// both outer channel consumers must retire before the graph can drop.
    pub(crate) fn wait_compute_bootstrap_render_retirement(&mut self) -> Result {
        const POLLS: usize = 500;
        let mut last = None;
        for _ in 0..POLLS {
            fence(Ordering::Acquire);
            let state = self.render_storage.as_mut().ok_or(ENODEV)?.snapshot()?;
            let outer = self.resources.partial_opening_outer_counters()?;
            // m1n1's fence owns the prefix once done/read cover the three
            // items and the outer consumers reach producer one. The queue may
            // already advertise a later write prefix (the positive run is
            // 3/3/6), so equality with write=3 is not an ownership rule.
            let inner_retired = state.tiling.done >= 3
                && state.tiling.read >= 3
                && state.tiling.write >= 3
                && state.fragment.done >= 3
                && state.fragment.read >= 3
                && state.fragment.write >= 3;
            let outer_retired = outer
                .into_iter()
                .all(|counters| counters[2] == 1 && counters[..2] == [counters[2]; 2]);
            if inner_retired && outer_retired {
                self.render_tracker = None;
                self.render_bind = None;
                self.render_storage = None;
                self.render_submission_ordinal = 0;
                self.resources
                    .uat()
                    .restore_t8140_kernel_context_after_bootstrap_render()?;
                return Ok(());
            }
            last = Some((state, outer));
            fsleep(Delta::from_millis(1));
        }
        if let Some((state, outer)) = last {
            pr_err!(
                "G17P bootstrap timeout: TA {:?}, 3D {:?}, outer 3D/TA {:?}\n",
                state.tiling,
                state.fragment,
                outer
            );
        }
        Err(ETIMEDOUT)
    }

    pub(crate) fn prepare_compute_runtime_class1(&mut self) -> Result {
        self.resources.prepare_compute_runtime_class1()
    }

    pub(crate) fn publish_compute_runtime_control_record(
        &mut self,
        index: usize,
    ) -> Result<u32> {
        self.resources.publish_compute_runtime_control_record(index)
    }

    pub(crate) fn prepare_compute_runtime_class2(&mut self) -> Result {
        self.resources.prepare_compute_runtime_class2()
    }

    pub(crate) fn rewrite_compute_dispatch_record(&mut self) -> Result {
        self.resources.rewrite_compute_dispatch_record()
    }

    pub(crate) fn uat(&self) -> &mmu::Uat {
        self.resources.uat()
    }

    pub(crate) fn mark_firmware_cache_flush_ready(&self) {
        self.resources.mark_firmware_cache_flush_ready();
    }

    fn validate_partial_opening_resources(&self) -> Result {
        let binding = self.resources.partial_opening_binding();
        let context = g17_submission::G17P_PARTIAL_OPENING_CONTEXT;
        let expected_fwctl = binding
            .kernel_va_base
            .checked_add(g17_submission::G17P_PARTIAL_OPENING_FWCTL_OFFSET)
            .ok_or(EINVAL)?;
        let addresses_ready = binding.scheduler_page
            == g17_submission::G17P_PARTIAL_OPENING_SCHEDULER_GPU_VA
            && binding.primary_index_page
                == g17_submission::G17P_PARTIAL_OPENING_PRIMARY_INDEX_GPU_VA
            && binding.fwctl == expected_fwctl;
        let context_ready = context.context0_root_slot == 0
            && context.context0_context_id == 0
            && context.render_root_slot == 1
            && context.render_context_id == 1
            && context.context0_empty_high_root_slot == 0
            && context.render_empty_high_root_slot == 1
            && context.transport_pair == 0
            && context.descriptor_pair == 0
            && context.shared_queue_pair_namespace == 0
            && context.optional_identity_context_id == 1
            && context.descriptor_context_id == 1
            && context.scheduler_class == 2
            && context.work_doorbell_channel == 0
            && context.context0_low_root_present_at_first_work
            && context.render_low_root_present_at_first_work
            && !context.source_firmware_high_root_table_resident;
        let roots_ready = match self.render_bind.as_ref() {
            Some(binding) => self
                .resources
                .uat()
                .t8140_partial_opening_shared_render_context_ready(binding),
            None => self
                .resources
                .uat()
                .t8140_partial_opening_kernel_contexts_ready(),
        };
        if !addresses_ready || !context_ready || !roots_ready {
            dev_err!(
                self.resources.dev().as_ref(),
                "G17P render: opening contract addresses={} context={} roots={} (scheduler {:#x}, index {:#x}, fwctl {:#x}/{:#x})\n",
                addresses_ready,
                context_ready,
                roots_ready,
                binding.scheduler_page,
                binding.primary_index_page,
                binding.fwctl,
                expected_fwctl
            );
            return Err(EINVAL);
        }
        Ok(())
    }

    /// Apply first-work status immediately before the caller sends primary
    /// control-done 0x84.
    pub(crate) fn prepare_partial_opening_control_done(&mut self) -> Result {
        self.validate_partial_opening_resources().inspect_err(|error| {
            dev_err!(
                self.resources.dev().as_ref(),
                "G17P render: opening validation failed ({:?})\n",
                error
            );
        })?;
        self.resources
            .apply_partial_opening_pre_control_status()
            .inspect_err(|error| {
                dev_err!(
                    self.resources.dev().as_ref(),
                    "G17P render: status-B update failed ({:?})\n",
                    error
                );
            })
    }

    /// Publish the first-partial operand table in the control-done to
    /// first-work interval.
    pub(crate) fn publish_partial_opening_operand_table(&mut self) -> Result {
        self.resources.publish_partial_opening_operand_table()
    }

    /// Publish the exact first 3D/TA outer-ring pair. The callback installs
    /// the late TA descriptor, item-ring, and queue-head state between the 3D
    /// producer and the TA outer slot. The retained live runtime sends the
    /// returned channel-0 doorbell exactly once.
    pub(crate) fn submit_partial_opening_render<F>(
        &mut self,
        submission: G17PPartialOpeningRenderSubmission,
        publish_late_tiling_state: F,
    ) -> Result<(
        G17PPartialOpeningRenderTracker,
        g17_submission::PreparedG17PPartialOpeningWorkPair,
    )>
    where
        F: FnOnce(),
    {
        self.validate_partial_opening_resources()?;
        if submission.fragment_queue_write_index != 3 || submission.tiling_queue_write_index != 3 {
            return Err(EINVAL);
        }
        let (prepared, render_bind) = self.resources.publish_partial_opening_pair(
            None,
            false,
            submission.fragment_queue_gpu_va,
            submission.fragment_queue_write_index,
            submission.tiling_queue_gpu_va,
            submission.tiling_queue_write_index,
            publish_late_tiling_state,
        )?;
        if render_bind.is_some() {
            return Err(EINVAL);
        }
        if prepared.paired_doorbell != g17_submission::G17P_PARTIAL_OPENING_PAIRED_DOORBELL
            || prepared.paired_doorbell != 0x0083_0000_0000_0000
        {
            return Err(EINVAL);
        }
        let tracker = G17PPartialOpeningRenderTracker {
            tiling_previous: g17_completion::QueueIndices {
                done: 0,
                read: 0,
                write: submission.tiling_queue_write_index,
            },
            fragment_previous: g17_completion::QueueIndices {
                done: 0,
                read: 0,
                write: submission.fragment_queue_write_index,
            },
            tiling_prefix: submission.tiling_queue_write_index,
            fragment_prefix: submission.fragment_queue_write_index,
        };
        Ok((tracker, prepared))
    }

    /// Classify paired render progress from queue indices plus the scheduler
    /// job list and both descriptor timestamp quartets.
    pub(crate) fn observe_partial_opening_render_completion(
        &self,
        tracker: &mut G17PPartialOpeningRenderTracker,
        tiling_current: g17_completion::QueueIndices,
        fragment_current: g17_completion::QueueIndices,
        execution: g17_completion::RenderExecutionState,
    ) -> Result<g17_completion::PairedRenderCompletion> {
        let completion = g17_completion::observe_paired_render_completion(
            tracker.tiling_previous,
            tiling_current,
            tracker.tiling_prefix,
            tracker.fragment_previous,
            fragment_current,
            tracker.fragment_prefix,
            execution,
        )
        .map_err(|_| EINVAL)?;
        tracker.tiling_previous = tiling_current;
        tracker.fragment_previous = fragment_current;
        Ok(completion)
    }

    pub(crate) fn new_user_vm(
        &mut self,
        id: u64,
        kernel_range: core::ops::Range<u64>,
    ) -> Result<mmu::Vm> {
        self.new_user_vm_inner(id, kernel_range, false)
    }

    fn new_user_vm_inner(
        &mut self,
        id: u64,
        kernel_range: core::ops::Range<u64>,
        bootstrap_vdm: bool,
    ) -> Result<mmu::Vm> {
        let vm = self
            .resources
            .uat()
            .new_vm(id, kernel_range)
            .inspect_err(|error| {
                dev_err!(
                    self.resources.dev().as_ref(),
                    "G17P VM: context allocation failed ({:?})\n",
                    error
                );
            })?;
        self.resources
            .install_user_vm_aliases(&vm, bootstrap_vdm)
            .inspect_err(|error| {
                dev_err!(
                    self.resources.dev().as_ref(),
                    "G17P VM: fixed aliases failed ({:?})\n",
                    error
                );
            })?;
        Ok(vm)
    }

    pub(crate) fn map_timestamp_buffer(
        &self,
        mut bo: crate::gem::ObjectRef,
        range: core::ops::Range<usize>,
    ) -> Result<mmu::KernelMapping> {
        bo.map_range_into_range(
            self.resources.uat().kernel_vm(),
            range,
            self.resources.uat().kernel_va_range()?,
            mmu::UAT_PGSZ as u64,
            mmu::PROT_FW_SHARED_RW,
            false,
        )
    }

    /// Release one stopped firmware session without replacing its UAT owner.
    pub(crate) fn release_runtime_objects_after_processor_stop(&mut self) {
        self.render_bind = None;
        self.render_tracker = None;
        self.render_storage = None;
        // Both CPUs are stopped at this boundary. The alias owner's Drop
        // removes the context-0 leaves and invalidates ASIDs 0/1.
        self.render_context_aliases = None;
        Self::release_retired_handoff_graph(
            &mut self.render_hardware_buffer_ids,
            &mut self.retired_parameter_managers,
            self.retired_render_handoff_graph.take(),
        );
        while let Some(retired) = self.retired_render_handoff_graphs.pop() {
            Self::release_retired_handoff_graph(
                &mut self.render_hardware_buffer_ids,
                &mut self.retired_parameter_managers,
                Some(retired),
            );
        }
        for manager in &self.retired_parameter_managers {
            self.render_hardware_buffer_ids
                .unreserve(manager.hardware_buffer_id());
        }
        self.retired_parameter_managers.clear();
        self.released_render_contexts = 0;
        self.render_sksm = None;
        self.bootstrap_uma = None;
        self.prepared_render_flist = None;
        self.render_submission_ordinal = 0;
        self.render_auxiliary_timestamp = 0;
        self.next_render_flist_sequence = G17P_BOOTSTRAP_UMA_SEQUENCE;
        self.next_render_pm_manager_generation =
            g17_resources::G17P_PM_INITIAL_HWPB_MANAGER_GENERATION;
        self.render_parameter_buffer_owner = 0;
        self.render_command_buffer_states = G17PCommandBufferStateAllocator::new();
        self.render_command_buffer_state_pairs = [None, None];
        self.active_render_slot = 0;
        // Both processors are stopped and every retained compute/render graph
        // is gone, so queue-lifetime HardwareBufferIDs can be allocated from
        // the native cold-session order again (render/bootstrap 0, compute 1+).
        self.render_hardware_buffer_ids = G17PHardwareBufferIdAllocator::new();
        // Every compute graph and the boot graph have been dropped after both
        // processors stopped. Their QIDs are no longer installed in firmware,
        // so the replacement session owns a fresh namespace. Leaving this at
        // its previous high-water mark exhausted QID 127 after enough bounded
        // recoveries even though no hardware graph survived any of them.
        self.next_compute_queue_id = Some(G17P_COMPUTE_QUEUE_ID_BASE);
        // These values name the stopped session's descriptor and deferred
        // AddKicks record. Replaying either against the replacement boot graph
        // can acknowledge or kick the wrong QID even when that QID number was
        // legitimately reused above.
        self.pending_add_kicks = None;
        self.last_published_descriptor = 0;
        self.last_completion_end = 0;
        self.compute_stamp_armed = false;
    }

    /// Restore cold initdata in the existing mappings after both CPUs stop.
    pub(crate) fn reset_session_after_processor_stop(&mut self) -> Result {
        if self.render_bind.is_some()
            || self.render_tracker.is_some()
            || self.render_storage.is_some()
        {
            return Err(EBUSY);
        }
        self.resources.reset_session_after_processor_stop()
    }

    fn retained_graph_matches_owner(
        graph: &G17PRetiredRenderGraph,
        vm: &mmu::Vm,
        descriptor_context_id: u16,
    ) -> bool {
        graph.bind.matches(vm)
            && graph.storage.descriptor_context_id() == descriptor_context_id
            && vm.render_pool_mappings_match(
                graph.storage.queue_pair().slot(),
                graph.storage.render_pool_id(),
            )
    }

    /// Search the whole cache for the incoming owner before considering a
    /// released foreign owner. A foreign graph must not displace the incoming
    /// VM's existing render-pool mappings just because it precedes them in the
    /// cache. Context supersession and queue release remain required for reuse.
    fn select_cached_render_graph(
        &self,
        vm: &mmu::Vm,
        descriptor_context_id: u16,
    ) -> (bool, Option<usize>) {
        for reuse_foreign in [false, true] {
            let matches = |graph: &G17PRetiredRenderGraph| {
                graph.storage.queue_pair().slot() == self.active_render_slot
                    && if reuse_foreign {
                        !graph.bind.matches(vm)
                            && Self::retired_render_graph_reclaimable(
                                self.render_context_epochs,
                                self.released_render_contexts,
                                graph,
                            )
                            && graph.storage.client_rebind_addresses_available(vm)
                    } else {
                        Self::retained_graph_matches_owner(graph, vm, descriptor_context_id)
                    }
            };
            if self.retired_render_handoff_graph.as_ref().is_some_and(&matches) {
                return (true, None);
            }
            if let Some(index) = self.retired_render_handoff_graphs.iter().position(matches) {
                return (false, Some(index));
            }
        }
        if crate::util::g17p_verbose() {
            for graph in Iterator::chain(
                self.retired_render_handoff_graph.iter(),
                self.retired_render_handoff_graphs.iter(),
            )
                .filter(|graph| graph.storage.queue_pair().slot() == self.active_render_slot)
            {
                dev_info!(
                    self.resources.dev().as_ref(),
                    "G17P graph cache: incoming vm={} context={} cached vm={} context={} pool-match={} epoch={}/{} released={:#x}\n",
                    vm.id(), descriptor_context_id, graph.bind.vm_id(),
                    graph.storage.descriptor_context_id(),
                    vm.render_pool_mappings_match(
                        self.active_render_slot, graph.storage.render_pool_id(),
                    ),
                    self.render_context_epochs[usize::from(self.active_render_slot)],
                    graph.retire_after_context_epoch, self.released_render_contexts,
                );
            }
        }
        (false, None)
    }

    /// The retained graph and global aliases must agree on the incoming VM and
    /// descriptor context. A VM can own several logical queues, but each queue
    /// receives an independent application-GART context and cannot reuse a
    /// sibling queue's encoded render graph.
    pub(crate) fn retained_render_has_other_owner(
        &self,
        vm: &mmu::Vm,
        descriptor_context_id: u16,
    ) -> bool {
        let graph_retained = self.render_storage.is_some();
        let aliases_retained = self.render_context_aliases.is_some();
        let retained = graph_retained || aliases_retained;
        let graph_same_owner = !graph_retained
            || self.render_bind.as_ref().is_some_and(|binding| {
                binding.matches(vm)
                    && self.render_storage.as_ref().is_some_and(|storage| {
                        storage.descriptor_context_id() == descriptor_context_id
                            && vm.render_pool_mappings_match(
                                storage.queue_pair().slot(),
                                storage.render_pool_id(),
                            )
                    })
            });
        let aliases_same_vm = self
            .render_context_aliases
            .as_ref()
            .map_or(true, |aliases| aliases.matches(vm));
        let same_owner = graph_same_owner && aliases_same_vm;
        retained && !same_owner
    }

    /// Actual paired completion and FList release authorize root ownership
    /// changes. T8140's handoff page does not publish a live idle witness.
    fn require_retired_render_owner(&mut self) -> Result {
        // Handoff.current_slot is a T8140 stand-in, not a firmware idle
        // witness. Use the actual paired retirement and FList acknowledgements.
        if self.render_tracker.is_some() || self.render_hardware_buffer_id.is_some()
            || self.render_hardware_buffer_published || self.prepared_render_flist.is_some()
            || self.render_submission_ordinal == 0
        {
            return Err(EBUSY);
        }
        // The USC FList is a device-global refcounted pool. Another slot may
        // legitimately retain its references after this slot's TA/3D pair is
        // complete; those references do not keep this job's graph or VM alive.
        let snapshot = self.render_storage.as_mut().ok_or(ENODEV)?.snapshot()?;
        if !snapshot.job_list_empty || !snapshot.tiling.idle() || !snapshot.fragment.idle() {
            return Err(EBUSY);
        }
        // Do not require the device-global outer channels to be empty here.
        // This selected slot's paired completion already proved that its two
        // queue items were consumed and its job list is idle.  A later outer
        // item may legitimately belong to the other independently rooted
        // slot; using its live producer as an idle condition reintroduced the
        // full render/render lease at every cross-VM handoff.
        Ok(())
    }

    /// Retire one VM's job-private graph before another VM constructs its own
    /// graph on the already-installed physical QID pair.  Queue producer state
    /// and the monotonically increasing submission ordinal stay retained; no
    /// global context-0 mapping is repointed.
    fn park_render_sksm_backing_for_graph_rebuild(&mut self) -> Result {
        if self.render_sksm.is_none() {
            return Ok(());
        }
        let expected = {
            let queues = self.render_sksm.as_ref().ok_or(EIO)?;
            if queues.backing.is_some() {
                return Err(EIO);
            }
            [
                (queues.tiling.entry_low, queues.tiling.entry_high),
                (queues.fragment.entry_low, queues.fragment.entry_high),
            ]
        };
        let actual = {
            let storage = self.render_storage.as_ref().ok_or(EIO)?;
            [
                (storage.sksm_entry_low_va(true), storage.sksm_entry_high_va(true)),
                (storage.sksm_entry_low_va(false), storage.sksm_entry_high_va(false)),
            ]
        };
        if actual != expected {
            dev_err!(
                self.resources.dev().as_ref(),
                "G17P render graph rebuild: refusing to detach mismatched SKSM backing actual={:#x?} expected={:#x?}\n",
                actual,
                expected,
            );
            return Err(EFAULT);
        }
        let backing = self.render_storage.as_mut().ok_or(EIO)?.take_sksm_backing()?;
        self.render_sksm.as_mut().ok_or(EIO)?.backing = Some(backing);
        Ok(())
    }

    fn reactivate_recycled_render_graph(
        &mut self,
        mut cached: G17PRetiredRenderGraph,
        vm: &mmu::Vm,
        execution_context: Arc<mmu::T8140ComputeExecutionContext>,
    ) -> Result {
        let cached_vm = cached.bind.vm_id();
        let prepared = (|| -> Result {
            // The old root remains owned by cached.bind through the entire
            // transaction. Retargeting first additionally retains the new
            // root if a later alias allocation fails.
            cached.storage.retarget_command_context(vm, execution_context.clone())?;
            cached.bind.vm().clear_render_pool_mappings(cached.storage.queue_pair().slot());
            drop(cached.context_aliases.take());
            let backing = self.render_sksm.as_mut()
                .and_then(|queues| queues.backing.as_mut()).ok_or(EIO)?;
            self.resources.rebind_cached_render_vm(&mut cached.storage, vm, backing)?;
            cached.storage.prepare_cached_reactivation(self.render_submission_ordinal)?;
            cached.storage.claim_sksm_backing(
                &mut self.render_sksm.as_mut().ok_or(EIO)?.backing,
            )?;
            Ok(())
        })();
        if let Err(error) = prepared {
            // This graph has already been firmware-visible. A failed alias
            // transaction must not drop its descriptors, support or PM data.
            // Its old bind and new command context retain both roots.
            if let Err(full) = self.retired_render_handoff_graphs.push_within_capacity(cached) {
                core::mem::forget(full.0);
            }
            dev_err!(
                self.resources.dev().as_ref(),
                "G17P graph rebind failed from vm={} to vm={} ({:?}); retaining complete graph\n",
                cached_vm, vm.id(), error,
            );
            return Err(error);
        }
        drop(cached.bind);
        self.render_bind = Some(G17PRenderRootOwner::Job { context: execution_context });
        self.render_context_aliases = None;
        self.render_storage = Some(cached.storage);
        dev_info!(
            self.resources.dev().as_ref(),
            "G17P render VM handoff: rebound complete cached graph from vm={} to vm={} on pair {} at ordinal {}\n",
            cached_vm, vm.id(), self.active_render_slot, self.render_submission_ordinal,
        );
        Ok(())
    }

    pub(crate) fn handoff_retained_render_vm(
        &mut self,
        vm: &mmu::Vm,
        execution_context: Arc<mmu::T8140ComputeExecutionContext>,
    ) -> Result {
        let descriptor_context_id =
            u16::try_from(execution_context.context_id()).map_err(|_| EOVERFLOW)?;
        // Logical queues in one DRM VM receive independent descriptor
        // contexts, but the retained RT/TVB/PM graph and all of its fixed
        // client addresses belong to the VM. Once its previous pair is fully
        // retired, retarget only the queue-owned context fields and preserve
        // the graph. Rebuilding it in the same VM would collide with the
        // firmware-retained mappings that deliberately survive queue close.
        let same_vm_different_context = self.render_storage.as_ref().is_some_and(|storage| {
            storage.descriptor_context_id() != descriptor_context_id
                && self
                    .render_bind
                    .as_ref()
                    .is_some_and(|binding| binding.matches(vm))
                && storage.first_unreachable(self.resources.uat(), vm).is_none()
        });
        if same_vm_different_context {
            self.require_retired_render_owner()?;
            let context_update_pending = self
                .render_storage
                .as_ref()
                .ok_or(ENODEV)?
                .context_update_pending();
            // A failed pre-publication retarget may be replaced by another
            // queue without accumulating roots: the currently selected context
            // was never acknowledged by firmware, while the first replaced
            // (firmware-visible) root remains owned here. Once a context update
            // completed, however, no retirement owner may still be pending.
            if self.render_context_retirement.is_some() && !context_update_pending {
                return Err(EIO);
            }
            let retire_after_context_epoch =
                self.next_render_context_epoch(self.active_render_slot)?;
            self.render_storage
                .as_mut()
                .ok_or(ENODEV)?
                .retarget_command_context(vm, execution_context.clone())?;
            let previous_bind = self
                .render_bind
                .replace(G17PRenderRootOwner::Job {
                    context: execution_context,
                })
                .ok_or(EIO)?;
            let previous_context_aliases = self.render_context_aliases.take();
            if self.render_context_retirement.is_none() {
                self.render_context_retirement = Some(G17PRenderContextRetirement {
                    bind: previous_bind,
                    _context_aliases: previous_context_aliases,
                    retire_after_context_epoch,
                });
            }
            dev_info!(
                self.resources.dev().as_ref(),
                "G17P render owner handoff: retained VM {} graph on pair {} retargeted to descriptor context {}; prior root held through context epoch {}\n",
                vm.id(),
                self.active_render_slot,
                descriptor_context_id,
                retire_after_context_epoch,
            );
            return Ok(());
        }
        // A pre-publication failure can leave this slot with no selected graph
        // while its previous graphs remain safely cached. Search that cache
        // before treating the slot as genuinely empty: constructing over a
        // cached graph's fixed RT-memory/TPC addresses in the same VM is an
        // ownership collision and deterministically returns ENOSPC.
        if self.render_storage.is_none() {
            let pair_slot = self.active_render_slot;
            let (cached_immediate, cached_index) =
                self.select_cached_render_graph(vm, descriptor_context_id);
            if !cached_immediate && cached_index.is_none() {
                return Ok(());
            }
            let cached_rebind = self
                .retired_render_handoff_graph
                .as_ref()
                .filter(|_| cached_immediate)
                .or_else(|| {
                    cached_index.and_then(|index| self.retired_render_handoff_graphs.get(index))
                })
                .is_some_and(|graph| {
                    !Self::retained_graph_matches_owner(graph, vm, descriptor_context_id)
                });
            if cached_immediate {
                self.retired_render_handoff_graph
                    .as_mut()
                    .ok_or(EIO)?
                    .storage
                    .prepare_cached_reactivation(self.render_submission_ordinal)?;
            } else {
                self.retired_render_handoff_graphs
                    .get_mut(cached_index.ok_or(EIO)?)
                    .ok_or(EIO)?
                    .storage
                    .prepare_cached_reactivation(self.render_submission_ordinal)?;
                self.preserve_immediate_retired_render_graph()?;
            }
            let mut cached = if cached_immediate {
                self.retired_render_handoff_graph.take().ok_or(EIO)?
            } else {
                self.retired_render_handoff_graphs
                    .remove(cached_index.ok_or(EIO)?)
                    .map_err(|_| EIO)?
            };
            if !cached.storage.sksm_backing_detached() {
                return Err(EIO);
            }
            if cached_rebind {
                return self.reactivate_recycled_render_graph(cached, vm, execution_context);
            }
            cached.storage.claim_sksm_backing(
                &mut self.render_sksm.as_mut().ok_or(EIO)?.backing,
            )?;
            let cached_vm = cached.bind.vm_id();
            self.render_bind = Some(cached.bind);
            self.render_context_aliases = cached.context_aliases;
            self.render_storage = Some(cached.storage);
            self.prune_unreferenced_retired_render_graphs()?;
            dev_info!(
                self.resources.dev().as_ref(),
                "G17P render VM handoff: restored cached graph vm={} into empty slot {} at ordinal {}\n",
                cached_vm,
                pair_slot,
                self.render_submission_ordinal,
            );
            return Ok(());
        }
        if !self.retained_render_has_other_owner(vm, descriptor_context_id) {
            return Ok(());
        }
        self.require_retired_render_owner()?;
        let old_root = self.render_bind.as_ref().map_or(0, |binding| binding.root());
        let old_vm_id = self.render_bind.as_ref().map_or(0, |binding| binding.vm_id());
        if self.render_bind.is_none() {
            return Err(EIO);
        }

        // Prefer the incoming VM's own cached graph. If this is a fresh VM,
        // recycle an older graph only after a later context-update epoch has
        // positively unlinked it. The queue record, item ring, descriptors,
        // support graph and PM/TVB owner move together; only client-root
        // aliases and the command context are rebound.
        let (cached_immediate, cached_index) =
            self.select_cached_render_graph(vm, descriptor_context_id);
        let cached_rebind = self
            .retired_render_handoff_graph
            .as_ref()
            .filter(|_| cached_immediate)
            .or_else(|| cached_index.and_then(|index| self.retired_render_handoff_graphs.get(index)))
            .is_some_and(|graph| {
                !Self::retained_graph_matches_owner(graph, vm, descriptor_context_id)
            });
        if cached_immediate {
            let cached = self.retired_render_handoff_graph.as_mut().ok_or(EIO)?;
            if !cached.storage.sksm_backing_detached() {
                return Err(EIO);
            }
            cached
                .storage
                .prepare_cached_reactivation(self.render_submission_ordinal)?;
        } else if let Some(index) = cached_index {
            let cached = self.retired_render_handoff_graphs.get_mut(index).ok_or(EIO)?;
            if !cached.storage.sksm_backing_detached() {
                return Err(EIO);
            }
            cached
                .storage
                .prepare_cached_reactivation(self.render_submission_ordinal)?;
            self.preserve_immediate_retired_render_graph()?;
        } else {
            self.preserve_immediate_retired_render_graph()?;
        }
        self.park_render_sksm_backing_for_graph_rebuild()?;

        let mut cached = if cached_immediate {
            self.retired_render_handoff_graph.take()
        } else if cached_index.is_some() {
            Some(
                self.retired_render_handoff_graphs
                    .remove(cached_index.ok_or(EIO)?)
                    .map_err(|_| EIO)?,
            )
        } else {
            None
        };
        let retire_after_context_epoch =
            self.next_render_context_epoch(self.active_render_slot)?;
        let retired = G17PRetiredRenderGraph {
            bind: self.render_bind.take().ok_or(EIO)?,
            context_aliases: self.render_context_aliases.take(),
            storage: self.render_storage.take().ok_or(ENODEV)?,
            retire_after_context_epoch,
        };
        self.retired_render_handoff_graph = Some(retired);
        if let Some(mut cached) = cached.take() {
            if cached_rebind {
                return self.reactivate_recycled_render_graph(cached, vm, execution_context);
            }
            cached.storage.claim_sksm_backing(
                &mut self.render_sksm.as_mut().ok_or(EIO)?.backing,
            )?;
            let cached_vm = cached.bind.vm_id();
            self.render_bind = Some(cached.bind);
            self.render_context_aliases = cached.context_aliases;
            self.render_storage = Some(cached.storage);
            self.prune_unreferenced_retired_render_graphs()?;
            if crate::util::g17p_verbose() {
                dev_info!(self.resources.dev().as_ref(),
                    "G17P render VM handoff: reactivated retained graph vm={} at ordinal {} without clearing its render-pool mappings\n",
                    cached_vm, self.render_submission_ordinal);
            }
            return Ok(());
        }
        self.prune_unreferenced_retired_render_graphs()?;
        if crate::util::g17p_verbose() {
            dev_info!(self.resources.dev().as_ref(),
                "G17P render VM handoff: quarantined retired job-private graph vm={} root={:#x}; incoming VM builds a new root at ordinal {}, QID producer state retained\n",
                old_vm_id, old_root, self.render_submission_ordinal);
        }
        Ok(())
    }

    fn preserve_immediate_retired_render_graph(&mut self) -> Result {
        let Some(previous) = self.retired_render_handoff_graph.take() else {
            return Ok(());
        };
        if let Err(error) = self
            .retired_render_handoff_graphs
            .push_within_capacity(previous)
        {
            self.retired_render_handoff_graph = Some(error.0);
            return Err(ENOMEM);
        }
        Ok(())
    }

    fn release_render_slot_context_owner(
        state: &mut G17PRenderSlotState,
        context: u16,
        retired_witness: bool,
    ) -> Result {
        if !state
            .storage
            .as_ref()
            .is_some_and(|storage| storage.descriptor_context_id() == context)
        {
            return Ok(());
        }

        // G17PQueue::drop calls this only after destroying the render
        // scheduler entity. Therefore no packet carrying this context can
        // still be pending. Keep the complete idle graph on its physical pair:
        // its fixed RT/TVB/PM mappings are VM-owned and cannot be rebuilt at
        // the same addresses while the firmware-lifetime owner is retained.
        // The next queue in this VM retargets the context fields in place; a
        // different VM takes the ordinary graph-handoff path below.
        if !retired_witness
            || state.tracker.is_some()
            || state.hardware_buffer_id.is_some()
            || state.hardware_buffer_published
            || state.bind.is_none()
        {
            return Err(EBUSY);
        }
        let snapshot = state.storage.as_mut().ok_or(EIO)?.snapshot()?;
        if !snapshot.job_list_empty || !snapshot.tiling.idle() || !snapshot.fragment.idle() {
            return Err(EBUSY);
        }
        if state.sksm.as_ref().ok_or(EIO)?.backing.is_some() {
            return Err(EIO);
        }
        state.auxiliary_timestamp = 0;
        Ok(())
    }

    /// Mark every graph belonging to a DRM queue after its scheduler entity
    /// has drained. Completed active graphs remain installed on their bounded
    /// physical pairs and can be retargeted to the next queue in the same VM;
    /// an already-retired graph remains whole until a later context-update
    /// group on its physical pair reaches paired completion.
    pub(crate) fn release_render_context_owner(&mut self, context: u16) {
        if context >= 64 { return; }
        self.released_render_contexts |= 1u64 << context;

        // Move the selected slot through the same helper as parked slots so
        // there is only one ownership transaction to audit.
        let active_matches = self
            .render_storage
            .as_ref()
            .is_some_and(|storage| storage.descriptor_context_id() == context);
        let active_retired = !active_matches || self.require_retired_render_owner().is_ok();
        let mut active = self.take_active_render_slot_state();
        let active_release = Self::release_render_slot_context_owner(
            &mut active,
            context,
            active_retired,
        );
        self.install_active_render_slot_state(active);

        let mut release_error = active_release.err();
        for state in self.parked_render_slots.iter_mut().flatten() {
            if let Err(error) = Self::release_render_slot_context_owner(
                state,
                context,
                true,
            ) {
                release_error = Some(error);
            }
        }
        if let Some(error) = release_error {
            dev_err!(
                self.resources.dev().as_ref(),
                "G17P render: drained descriptor context {} still owns active scheduler state ({:?}); retaining its graph\n",
                context,
                error,
            );
        }
        self.reclaim_released_superseded_render_graphs();
    }

    fn retired_render_graph_reclaimable(
        completed_epochs: [u64; 2],
        released_contexts: u64,
        graph: &G17PRetiredRenderGraph,
    ) -> bool {
        let context = graph.storage.descriptor_context_id();
        let slot = usize::from(graph.storage.queue_pair().slot());
        context < 64
            && released_contexts & (1u64 << context) != 0
            && completed_epochs
                .get(slot)
                .is_some_and(|epoch| *epoch >= graph.retire_after_context_epoch)
    }

    /// Drop a graph only after a later context-update group on the same
    /// physical pair completed and unlinked its EventControl. Unlike ordinary
    /// command completion, that is a positive firmware witness that both queue
    /// records now name the successor context, so its VM roots and client
    /// aliases may be reclaimed.
    ///
    /// Firmware background PB maintenance has a longer lifetime than those
    /// queue records. The same high PM backing which release_retired_handoff_graph
    /// preserves must therefore be detached here before the client graph is
    /// dropped. Releasing those objects at the context epoch reproduced GFX
    /// read faults at PC 0x95f0 (PM) and PC 0x24144 (support) under concurrent
    /// full-resolution clients.
    fn release_superseded_render_graph(
        ids: &mut G17PHardwareBufferIdAllocator,
        retired_parameter_managers: &mut KVec<
            g17_resources::G17PRetiredParameterManagement,
        >,
        retired: Option<G17PRetiredRenderGraph>,
    ) {
        let Some(retired) = retired else { return };
        let G17PRetiredRenderGraph {
            bind,
            context_aliases,
            storage,
            ..
        } = retired;
        let hardware_buffer_id = storage.retained_hardware_buffer_id();
        let retained_firmware_state = Self::quarantine_retired_parameter_management(
            retired_parameter_managers,
            storage.into_retired_parameter_management(),
        );
        if !retained_firmware_state {
            if let Some(id) = hardware_buffer_id {
                ids.unreserve(id);
            }
        }
        drop(context_aliases);
        drop(bind);
    }

    fn reclaim_released_superseded_render_graphs(&mut self) {
        // Reclaimable graphs are the reusable physical queue slots. Dropping
        // their support/PM graph here recreates the firmware UAF; the next VM
        // rebinds the complete owner, and processor stop drains leftovers.
    }

    /// Commit one per-pair context-supersession epoch. This runs only after the
    /// normal executed paired-completion path has also observed EventControl
    /// unlink; an abandoned/unexecuted group deliberately does not advance it.
    fn complete_render_context_update_retirement(&mut self) -> Result {
        let context_update = self
            .render_storage
            .as_ref()
            .ok_or(ENODEV)?
            .context_update_pending();
        if !context_update {
            if self.render_context_retirement.is_some() {
                return Err(EIO);
            }
            return Ok(());
        }
        let slot = usize::from(self.active_render_slot);
        let next = self.next_render_context_epoch(self.active_render_slot)?;
        if self
            .render_context_retirement
            .as_ref()
            .is_some_and(|retirement| retirement.retire_after_context_epoch != next)
        {
            return Err(EIO);
        }
        self.render_storage
            .as_mut()
            .ok_or(ENODEV)?
            .complete_context_update();
        self.render_context_epochs[slot] = next;
        self.render_context_retirement = None;
        self.reclaim_released_superseded_render_graphs();
        Ok(())
    }

    fn prune_unreferenced_retired_render_graphs(&mut self) -> Result {
        // Deliberately no completion-ring pruning. That ring is a diagnostic
        // history, not the native work-queue/descriptor event which owns the
        // context. Reclamation is instead driven by the later context-update
        // epoch committed at paired completion and EventControl unlink.
        Ok(())
    }

    fn release_retired_handoff_graph(
        ids: &mut G17PHardwareBufferIdAllocator,
        retired_parameter_managers: &mut KVec<
            g17_resources::G17PRetiredParameterManagement,
        >,
        retired: Option<G17PRetiredRenderGraph>,
    ) {
        let Some(retired) = retired else { return };
        let hardware_buffer_id = retired.storage.retained_hardware_buffer_id();
        let retained_firmware_state = Self::quarantine_retired_parameter_management(
            retired_parameter_managers,
            retired.storage.into_retired_parameter_management(),
        );
        if !retained_firmware_state {
            if let Some(id) = hardware_buffer_id {
                ids.unreserve(id);
            }
        }
    }

    fn quarantine_retired_parameter_management(
        retired_parameter_managers: &mut KVec<
            g17_resources::G17PRetiredParameterManagement,
        >,
        manager: Option<g17_resources::G17PRetiredParameterManagement>,
    ) -> bool {
        if let Some(manager) = manager {
            if let Err(error) = retired_parameter_managers.push_within_capacity(manager) {
                // Never free a firmware-reachable backing merely because the
                // preallocated quarantine has reached its diagnostic bound.
                // The mapping is intentionally leaked until module teardown;
                // dropping it here reproduces the observed GFX PC 0x95f0 UAF.
                core::mem::forget(error.0);
                pr_err!(
                    "G17P render: retired PM quarantine exhausted; retaining firmware state permanently\n"
                );
            }
            true
        } else {
            false
        }
    }

    /// Rebuild a retained graph whose ring producer no longer matches the
    /// ordinal, instead of refusing every later submission on it.
    ///
    /// A submission stages its items into the retained ring before the outer
    /// publication and the tag-14/tag-15 that make them visible. A failure in
    /// between advances `write` without advancing the ordinal, and from then on
    /// `restage_command`'s admission guard refuses this client forever -- one
    /// lost frame becomes a permanently dead context, which for the compositor
    /// means a dead session.
    ///
    /// The repair is the cross-VM handoff's, because the situation is the same:
    /// retire the job-private graph and let the next submission build a fresh
    /// one, whose `rebase_empty_queue_cursors` puts its cursors where this
    /// ordinal expects them while the physical QID producer stays monotonic.
    /// It is gated on `require_retired_render_owner`, so it can never run while
    /// the firmware still owns the previous group -- in that case the guard
    /// still refuses, and now says so in full.
    fn recycle_desynced_render_storage(&mut self) -> Result {
        let ordinal = self.render_submission_ordinal;
        let desynced = match self.render_storage.as_mut() {
            Some(storage) => storage.restage_admission_desynced(ordinal)?,
            None => return Ok(()),
        };
        if !desynced {
            return Ok(());
        }
        // Exactly the evidence a cross-VM handoff requires before it may drop a
        // job-private graph: no tracker, no HardwareBufferID, no staged FList,
        // empty job list and two idle queues.
        if self.require_retired_render_owner().is_err() {
            return Ok(());
        }
        let cursors = self
            .render_storage
            .as_mut()
            .ok_or(ENODEV)?
            .restage_admission_cursors(ordinal)?;
        let root = self.render_bind.as_ref().map_or(0, |binding| binding.root());
        let vm_id = self.render_bind.as_ref().map_or(0, |binding| binding.vm_id());
        self.park_render_sksm_backing_for_graph_rebuild()?;
        let mut storage = self.render_storage.take().ok_or(ENODEV)?;
        storage.release_client_context();
        let hardware_buffer_id = storage.retained_hardware_buffer_id();
        let retained_firmware_state = Self::quarantine_retired_parameter_management(
            &mut self.retired_parameter_managers,
            storage.into_retired_parameter_management(),
        );
        self.render_bind = None;
        self.render_context_aliases = None;
        if !retained_firmware_state {
            if let Some(id) = hardware_buffer_id {
                self.render_hardware_buffer_ids.unreserve(id);
            }
        }
        dev_warn!(
            self.resources.dev().as_ref(),
            "G17P render: retained graph desynced at ordinal {} (expected base {}, tiling write {}, fragment write {}) while both queues are idle; rebuilding it for vm {} root {:#x} instead of refusing the client\n",
            ordinal,
            cursors.0,
            cursors.1,
            cursors.2,
            vm_id,
            root,
        );
        Ok(())
    }

    /// End the active slot's retained storage claim on its TA HardwareBufferID.
    /// Call only where that storage is about to be dropped.
    fn unreserve_render_storage_hardware_buffer_id(&mut self) {
        if let Some(id) = self
            .render_storage
            .as_ref()
            .and_then(g17_resources::G17PUserRenderStorage::retained_hardware_buffer_id)
        {
            self.render_hardware_buffer_ids.unreserve(id);
        }
    }

    fn prepare_render_pm_manager_generation(&self) -> Result<Option<(u64, u64)>> {
        if g17_resources::g17p_render_tvb_mode() < 2 {
            return Ok(None);
        }
        Ok(Some(g17_resources::g17p_pm_manager_generation_plan(
            self.next_render_pm_manager_generation,
        )?))
    }

    /// Commit a prepared generation only after the replacement graph has
    /// become retained manager state. Constructor failures leave the session
    /// counter untouched and can retry the same never-published generation.
    fn commit_render_pm_manager_generation(
        &mut self,
        plan: Option<(u64, u64)>,
    ) -> Result {
        let actual = self
            .render_storage
            .as_ref()
            .ok_or(ENODEV)?
            .parameter_manager_generation();
        match (plan, actual) {
            (None, None) => Ok(()),
            (Some((current, next)), Some(actual))
                if current == actual && current == self.next_render_pm_manager_generation => {
                self.next_render_pm_manager_generation = next;
                Ok(())
            }
            _ => Err(EIO),
        }
    }

    /// Validate the incoming retained storage's indexed PBDesc identity.
    /// Owner switches must preserve the allocation state firmware left there.
    fn retarget_render_parameter_buffer(&mut self) -> Result {
        // Without slot affinity every client alternation rebuilds the graph,
        // and `new_user_render_storage` stamps the loader on the way through.
        if *crate::module_parameters::g17p_render_slot_affinity.value() == 0 {
            return Ok(());
        }
        let owner = self.render_storage.as_ref().ok_or(ENODEV)?.render_pool_id();
        if owner == self.render_parameter_buffer_owner {
            return Ok(());
        }
        let storage = self.render_storage.as_mut().ok_or(ENODEV)?;
        self.resources.stage_render_parameter_buffer_view(storage)?;
        let previous = self.render_parameter_buffer_owner;
        self.render_parameter_buffer_owner = owner;
        // Rate-limited: one alternating pair of clients would otherwise print
        // this twice a frame, and a dockchannel console write costs about a
        // millisecond -- enough to distort the timing this records.
        if G17P_PARAMETER_BUFFER_RETARGETS.fetch_add(1, Ordering::Relaxed) < 8 {
            dev_info!(
                self.resources.dev().as_ref(),
                "G17P render PM: retained parameter-buffer owner validated from {:#x} to {:#x}\n",
                previous,
                owner,
            );
        }
        Ok(())
    }

    pub(crate) fn retained_render_needs_recycle(
        &self,
        vm: &mmu::Vm,
        descriptor_context_id: u16,
    ) -> bool {
        g17_lifecycle::retained_render_requires_recycle(
            retained,
            same_vm,
            self.render_submission_ordinal,
        )
    }

    pub(crate) fn begin_translated_render(
        &mut self,
        dev: &AsahiDevice,
        vm: &mmu::Vm,
        command: &g17_uapi::TranslatedRenderCommand,
        registers: &regs::Resources,
        registration_power: &mut impl FnMut(&mut Self) -> Result,
    ) -> Result<g17_submission::PreparedG17PPartialOpeningWorkPair> {
        self.select_render_slot(queue_pair.slot())?;
        let descriptor_context_id = execution_context.context_id() as u16;
        if !execution_context.matches(vm) || !execution_context.is_current() {
            return Err(EFAULT);
        }
        if descriptor_context_id < 64 {
            self.released_render_contexts &= !(1u64 << descriptor_context_id);
        }
        if self.render_tracker.is_some() || self.render_hardware_buffer_id.is_some() {
            return Err(EBUSY);
        }
        // Before anything is allocated for this submission, deal with a graph
        // whose ring producer has drifted ahead of the ordinal. Nothing this
        // function does later can recover from it, and leaving it retires the
        // client for good.
        self.recycle_desynced_render_storage()?;
        if g17_resources::g17p_render_tvb_mode() >= 2 {
            let required=g17_resources::required_render_tvb_blocks(&command.parameters,self.num_clusters).ok_or(EINVAL)?;
            if self.render_storage.as_ref().is_some_and(|storage| storage.tvb_capacity()<required) {
                self.require_retired_render_owner()?;
                self.resources.grow_render_tvb(self.render_storage.as_mut().ok_or(EIO)?,vm,required)?;
            }
        }
        self.render_auxiliary_timestamp = G17P_NATIVE_RENDER_KICK_AUXILIARY;
        // The TA HardwareBufferID names this storage's PBDesc slot and its
        // retained parameter-management graph was encoded against it, so it
        // belongs to the storage for as long as the storage lives: a retained
        // storage re-acquires its own reserved ID, and only a storage about to
        // be built takes a fresh one.
        //
        // Allocating per submission handed out whichever ID the shared LIFO had
        // on top, which is the same number only while a single client renders.
        // As soon as another slot held an ID -- in flight, or pinned by an
        // abandoned render -- the next restage here received a different ID
        // and prepare_retained_scene refused it with ENOTSUPP, which reached
        // the compositor as a lost device.
        let retained_hardware_buffer_id = self
            .render_storage
            .as_ref()
            .and_then(g17_resources::G17PUserRenderStorage::retained_hardware_buffer_id);
        let (hardware_buffer_id, parameter_buffer_token) = match retained_hardware_buffer_id {
            Some(id) => self
                .render_hardware_buffer_ids
                .acquire_reserved_with_token(id)
                .map_err(|_| EBUSY)?,
            None => self
                .render_hardware_buffer_ids
                .allocate_with_token()
                .map_err(|_| EBUSY)?,
        };
        self.render_hardware_buffer_id = Some(hardware_buffer_id);
        self.render_hardware_buffer_published = false;
        let command_buffer_state_word = match self.allocate_active_render_command_buffer_state() {
            Ok(word) => word,
            Err(error) => {
                self.render_hardware_buffer_id = None;
                self.render_hardware_buffer_ids
                    .release(hardware_buffer_id)
                    .map_err(|_| EIO)?;
                return Err(error);
            }
        };

        let mut command = *command;
        command.parameters.ta_hardware_buffer_id = hardware_buffer_id;
        if crate::util::g17p_verbose() {
            dev_info!(
                dev.as_ref(),
                "G17P render: allocated TA HardwareBufferID {} for ordinal {}\n",
                hardware_buffer_id,
                self.render_submission_ordinal,
            );
        }
        let result = self.begin_translated_render_with_hardware_buffer(
            dev,
            vm,
            queue_pair,
            execution_context,
            descriptor_context_id,
            &command,
            parameter_buffer_token,
            [command_buffer_state_word; 2],
            registers,
            registration_power,
        );
        if result.is_err() && !self.render_hardware_buffer_published {
            // acquire() stages a copy of the completed FList owner. Until
            // outer publication it holds no firmware reference and must be
            // cancelled with the HardwareBufferID reservation. Otherwise a
            // fallible PM allocation leaves every later VM handoff EBUSY.
            self.prepared_render_flist = None;
            self.render_auxiliary_timestamp = 0;
            self.release_active_render_command_buffer_states()?;
            let id = self.render_hardware_buffer_id.take().ok_or(EIO)?;
            self.render_hardware_buffer_ids.release(id).map_err(|_| EIO)?;
        }
        result
    }

    fn begin_translated_render_with_hardware_buffer(
        &mut self,
        dev: &AsahiDevice,
        vm: &mmu::Vm,
        queue_pair: g17_resources::G17PRenderQueuePair,
        execution_context: Arc<mmu::T8140ComputeExecutionContext>,
        descriptor_context_id: u16,
        command: &g17_uapi::TranslatedRenderCommand,
        parameter_buffer_token: u64,
        command_buffer_state_words: [u32; 2],
        registers: &regs::Resources,
        registration_power: &mut impl FnMut(&mut Self) -> Result,
    ) -> Result<g17_submission::PreparedG17PPartialOpeningWorkPair> {
        let render_sksm = *crate::module_parameters::g17p_render_sksm.value();
        let native_doorbell_mode =
            *crate::module_parameters::g17p_render_native_doorbells.value();
        let split_launch = *crate::module_parameters::g17p_render_split_launch.value() != 0;
        if native_doorbell_mode > 3
            || (native_doorbell_mode != 0 && split_launch)
            || (g17p_native_fragment_first_mode(native_doorbell_mode) && render_sksm == 0)
        {
            return Err(EINVAL);
        }
        // Both render paths publish tag-16 when it is armed -- the SKSM
        // staging path through `stage_fragment_kick`, and the plain path
        // through `stage_fragment`. The absolute prefix includes all earlier
        // groups in the retained ring; it is not merely this group's 3/4
        // items. The same target feeds both outer slots and the tracker.
        let published_prefix =
            g17_resources::g17p_render_published_prefix(self.render_submission_ordinal)?;
        if self.render_tracker.is_some() {
            return Err(EBUSY);
        }
        let retained = self.render_storage.is_some();
        if retained
            && !self
                .render_bind
                .as_ref()
                .is_some_and(|binding| binding.matches(vm))
        {
            return Err(EBUSY);
        }
        if retained {
            self.render_storage
                .as_mut()
                .ok_or(ENODEV)?
                .restage_command(
                    dev,
                    self.resources.uat(),
                    vm,
                    command,
                    self.num_clusters,
                    command_buffer_state_words,
                    self.render_submission_ordinal,
                )?;
        } else {
            if self.render_submission_ordinal != 0 || self.render_bind.is_some() {
                return Err(EIO);
            }
            let mut retained_sksm_backing = self
                .render_sksm
                .as_mut()
                .and_then(|queues| queues.backing.take());
            let pm_generation = self.prepare_render_pm_manager_generation()?;
            let storage = self.resources.new_user_render_storage(
                dev,
                vm,
                queue_pair,
                Some(execution_context.clone()),
                descriptor_context_id,
                Some(render_root_slot),
                command,
                self.num_clusters,
                command_buffer_state_words,
                self.render_submission_ordinal,
                pm_generation.map(|plan| plan.0),
                &mut retained_sksm_backing,
            );
            self.render_storage = Some(match storage {
                Ok(storage) if retained_sksm_backing.is_none() => storage,
                Ok(_) => return Err(Self::render_stage_fail(
                    "new-user-render-sksm-backing-unclaimed", EIO,
                )),
                Err(error) => {
                    if let Some(backing) = retained_sksm_backing.take() {
                        let queues = self.render_sksm.as_mut().ok_or(EIO)?;
                        if queues.backing.replace(backing).is_some() {
                            return Err(EIO);
                        }
                    }
                    return Err(Self::render_stage_fail("new-user-render-storage", error));
                }
            });
            self.commit_render_pm_manager_generation(pm_generation)
                .map_err(|error| Self::render_stage_fail("commit-pm-manager-generation", error))?;
            // The fresh graph was encoded against this ID; keep the ID off the
            // shared free list for as long as the storage lives.
            if let Some(id) = self
                .render_storage
                .as_ref()
                .and_then(g17_resources::G17PUserRenderStorage::retained_hardware_buffer_id)
            {
                self.render_hardware_buffer_ids
                    .reserve(id)
                    .map_err(|_| Self::render_stage_fail("reserve-hardware-buffer-id", EIO))?;
            }
            if self.render_submission_ordinal != 0 {
                let parameter_page_list_vas = self
                    .render_storage
                    .as_ref()
                    .ok_or(ENODEV)?
                    .parameter_buffer_page_list_vas()?;
                let storage = self.render_storage.as_mut().ok_or(ENODEV)?;
                storage
                    .rebase_empty_queue_cursors(self.render_submission_ordinal)
                    .map_err(|error| Self::render_stage_fail("rebase-new-vm-queue", error))?;
                storage.restage_command(
                        dev,
                        vm,
                        command,
                        self.num_clusters,
                        command_buffer_state_words,
                        self.render_submission_ordinal,
                    )
                    .inspect_err(|error| {
                        dev_err!(
                            dev.as_ref(),
                            "G17P render: retained storage setup failed ({:?})\n",
                            error
                        );
                    })?,
            );
        }
        let (fragment_queue, tiling_queue, previous, late_tiling) = {
            let storage = self.render_storage.as_mut().ok_or(ENODEV)?;
            if !storage.is_reachable_from(self.resources.uat(), vm) {
                return Err(EFAULT);
            }
            storage
                .stage_fragment(self.render_submission_ordinal)
                .inspect_err(|error| {
                    dev_err!(dev.as_ref(), "G17P render: fragment stage failed ({:?})\n", error);
                })?;
            let previous = storage.snapshot().inspect_err(|error| {
                dev_err!(dev.as_ref(), "G17P render: initial snapshot failed ({:?})\n", error);
            })?;
            let late_tiling = storage
                .prepare_late_tiling_publication(self.render_submission_ordinal)
                .inspect_err(|error| {
                    dev_err!(dev.as_ref(), "G17P render: tiling stage failed ({:?})\n", error);
                })?;
            let (fragment_queue, tiling_queue) = storage.queue_gpu_vas();
            (fragment_queue, tiling_queue, previous, late_tiling)
        };
        let (prepared, render_bind) = self
            .resources
            .publish_partial_opening_pair(
                if retained { None } else { Some(vm) },
                retained,
                fragment_queue,
                3,
                tiling_queue,
                3,
                move || late_tiling.publish(),
            )
            .inspect_err(|error| {
                dev_err!(dev.as_ref(), "G17P render: outer publish failed ({:?})\n", error);
            })?;
        if retained {
            if render_bind.is_some() {
                return Err(EINVAL);
            }
        } else {
            self.render_bind = Some(render_bind.ok_or(EINVAL)?);
        }
        self.render_tracker = Some(G17PPartialOpeningRenderTracker {
            tiling_previous: previous.tiling,
            fragment_previous: previous.fragment,
            tiling_prefix: 3,
            fragment_prefix: 3,
        });
        Ok(prepared)
    }

    pub(crate) fn wait_translated_render_completion(
        &mut self,
    ) -> Result<g17_completion::PairedRenderCompletion> {
        const POLLS: usize = 2000;
        for _ in 0..POLLS {
            fence(Ordering::Acquire);
            let state = self.render_storage.as_mut().ok_or(ENODEV)?.snapshot()?;
            if !state.job_list_empty
                && state.tiling.idle()
                && state.fragment.idle()
                && state.tiling_timestamps.iter().all(|value| *value != 0)
                && state.fragment_timestamps.iter().all(|value| *value != 0)
            {
                self.render_storage
                    .as_mut()
                    .map(|s| s.read_pool_a_record(&mut pa).is_ok())
                    .unwrap_or(false)
                {
                    // The TA descriptor's +0x10 selects WHICH Pool-A record the
                    // firmware gates on, and it is record 1 by default -- read
                    // both. `FUN_16f7c` sets +0x94 = 1 after linking the job, so
                    // a 1 proves the classic TA enqueue ran and the job is
                    // sitting in the list; a 0 proves it never ran at all.
                    let r0 = u32::from_le_bytes([pa[0x94], pa[0x95], pa[0x96], pa[0x97]]);
                    let r1 = u32::from_le_bytes([
                        pa[0x194], pa[0x195], pa[0x196], pa[0x197],
                    ]);
                    let v1 = if r1 != 0 {
                        "rec1 QUEUED (FUN_16f7c ran, job linked, nothing dequeues DM0)"
                    } else {
                        "rec1 NOT queued (FUN_16f7c never ran)"
                    };
                    dev_info!(
                        dev.as_ref(),
                        "G17P pool-a[{}] rec0(+0x00={:02x?} +0x10={:02x?} +0x94={:#010x}) rec1(+0x00={:02x?} +0x10={:02x?} +0x94={:#010x} link={:02x?}) {}\n",
                        label,
                        &pa[0x00..0x08],
                        &pa[0x10..0x14],
                        r0,
                        &pa[0x100..0x108],
                        &pa[0x110..0x114],
                        r1,
                        &pa[0x198..0x1a8],
                        v1,
                    );
                }
            }
            let mut cc = [0u8; 0x40];
            let ok = self
                .render_storage
                .as_mut()
                .map(|s| s.read_channel_control(&mut cc).is_ok())
                .unwrap_or(false);
            if ok {
                dev_info!(
                    dev.as_ref(),
                    "G17P render channel-control[{}]: pid={:#04x} idx={:#04x} f2={:#04x} prio={:#04x} class={:#04x} qidmask={:02x?} hwbuf={:#04x}\n",
                    label,
                    cc[0],
                    cc[1],
                    cc[2],
                    cc[3],
                    cc[4],
                    &cc[0x12..0x1a],
                    cc[0x33],
                );
            }
        }

        self.resources.dump_render_qos_regions(label);
        if *crate::module_parameters::g17p_dump_state.value() != 0
            && !G17P_STATE_DUMPED.swap(true, core::sync::atomic::Ordering::Relaxed)
        {
            let kib = *crate::module_parameters::g17p_dump_state.value() as usize;
            if kib == 1 {
                self.resources.checksum_primary_state(label);
            } else {
                self.resources.dump_primary_state(label, kib * 1024);
            }
        }
        if *crate::module_parameters::g17p_dump_hwdata.value() != 0
            && !G17P_HW_DATA_DUMPED.swap(true, core::sync::atomic::Ordering::Relaxed)
        {
            self.resources.dump_hw_data(label);
        }
        self.resources.log_render_parameter_buffer_descriptor(
            label,
            self.render_hardware_buffer_id.unwrap_or(0),
        );
        let selected_metric = self
            .render_storage
            .as_ref()
            .ok_or(ENODEV)
            .and_then(|storage| storage.selected_scene_index())
            .and_then(|scene| self.resources.selected_parameter_metric(scene));
        let dev = self.resources.dev().clone();
        match selected_metric {
            Ok(value) => dev_info!(
                dev.as_ref(),
                "G17P render PM[{}]: actual-selected-metric={:#x}\n",
                label,
                value,
            ),
            Err(error) => dev_warn!(
                dev.as_ref(),
                "G17P render PM[{}]: actual selected metric read failed ({:?})\n",
                label,
                error,
            ),
        }
        if let Some(storage) = self.render_storage.as_mut() {
            match storage.render_activation_coordinates() {
                Ok((ta_qid, ta_stamp, sku_reserved, sku_pointer, sku_header)) => dev_info!(
                    dev.as_ref(),
                    "G17P render activation[{}]: TA-link qid={} stamp={:#x} predecessor={:#x}; 3D-SKU reserved={:#x} stream={:#x} header={:#x}\n",
                    label,
                    ta_qid,
                    ta_stamp,
                    ta_stamp.wrapping_sub(1) & ((1u64 << 40) - 1),
                    sku_reserved,
                    sku_pointer,
                    sku_header,
                ),
                Err(error) => dev_warn!(
                    dev.as_ref(),
                    "G17P render activation[{}]: descriptor coordinates unreadable ({:?})\n",
                    label,
                    error,
                ),
            }
            match storage.render_activation_gate_words() {
                Ok(gate) => dev_info!(
                    dev.as_ref(),
                    "G17P render activation gate[{}]: TA(+7a4,+866,+892,+8ba,+8cb)={:x?}\n",
                    label,
                    gate,
                ),
                Err(error) => dev_warn!(
                    dev.as_ref(),
                    "G17P render activation gate[{}]: descriptor inputs unreadable ({:?})\n",
                    label,
                    error,
                ),
            }
            storage.log_status_pages(&dev, label);
            storage.log_parameter_management(&dev, label);
        }
    }

    /// Snapshot every host-DRAM render progress marker while firmware is
    /// halted at recovery state 1, before the host acknowledges the restart.
    /// The recovery path retires both queue graphs and writes a synthetic 3D
    /// completion record, so the ordinary timeout dump is too late to tell a
    /// launch failure from recovery cleanup.
    /// Locate a reported MMU fault address inside the render VM's own tables.
    ///
    /// The G14X fault-address register is documented as VA >> 6, but that
    /// reading and the raw one cannot be told apart from a fault inside the
    /// first page. The GMMU report's VM slot is the descriptor/application
    /// context, not the ordinary VM bind slot. Resolve the retained storage by
    /// that exact context before walking its owner. Then report whether the
    /// selected VM maps the page and every mapped run within +/-1 MiB of it. A
    /// candidate that abuts or falls just outside a live run is the real
    /// address; one that lands in empty space at every scale is not.
    fn render_bind_for_descriptor_context(
        &self,
        descriptor_context_id: u8,
    ) -> Option<(&G17PRenderRootOwner, &'static str)> {
        let matches = |storage: &g17_resources::G17PUserRenderStorage| {
            storage.descriptor_context_id() == u16::from(descriptor_context_id)
        };
        if self.render_storage.as_ref().is_some_and(matches) {
            if let Some(bind) = self.render_bind.as_ref() {
                return Some((bind, "active"));
            }
        }
        if let Some(retirement) = self.render_context_retirement.as_ref().filter(|retirement| {
            retirement.bind.root_slot().ok() == Some(descriptor_context_id)
        }) {
            return Some((&retirement.bind, "active-context-retirement"));
        }
        if let Some(graph) = self
            .retired_render_handoff_graph
            .as_ref()
            .filter(|graph| matches(&graph.storage))
        {
            return Some((&graph.bind, "active-quarantine"));
        }
        if let Some(graph) = self
            .retired_render_handoff_graphs
            .iter()
            .find(|graph| matches(&graph.storage))
        {
            return Some((&graph.bind, "completion-quarantine"));
        }
        for state in self.parked_render_slots.iter().flatten() {
            if state.storage.as_ref().is_some_and(matches) {
                if let Some(bind) = state.bind.as_ref() {
                    return Some((bind, "parked"));
                }
            }
            if let Some(graph) = state
                .retired_handoff_graph
                .as_ref()
                .filter(|graph| matches(&graph.storage))
            {
                return Some((&graph.bind, "parked-quarantine"));
            }
            if let Some(retirement) = state.context_retirement.as_ref().filter(|retirement| {
                retirement.bind.root_slot().ok() == Some(descriptor_context_id)
            }) {
                return Some((&retirement.bind, "parked-context-retirement"));
            }
        }
        None
    }

    pub(crate) fn log_fault_address_candidates(
        &self,
        label: &'static str,
        addr_word: u64,
        fault_vm_slot: u8,
    ) -> bool {
        let Some((bind, owner)) = self.render_bind_for_descriptor_context(fault_vm_slot) else {
            return false;
        };
        self.log_fault_address_candidates_for_vm(
            label,
            addr_word,
            fault_vm_slot,
            bind.vm(),
            bind.vm_id(),
            bind.root(),
            owner,
        );
        true
    }

    pub(crate) fn log_compute_fault_address_candidates(
        &self,
        label: &'static str,
        addr_word: u64,
        fault_vm_slot: u8,
        context: &mmu::T8140ComputeExecutionContext,
        owner: &'static str,
    ) {
        self.log_fault_address_candidates_for_vm(
            label,
            addr_word,
            fault_vm_slot,
            context.vm(),
            context.vm_id(),
            context.root(),
            owner,
        );
    }

    fn log_fault_address_candidates_for_vm(
        &self,
        label: &'static str,
        addr_word: u64,
        fault_vm_slot: u8,
        vm: &mmu::Vm,
        vm_id: u64,
        root: u64,
        owner: &'static str,
    ) {
        let dev = self.resources.dev().clone();
        dev_info!(
            dev.as_ref(),
            "G17P fault address[{}]: walking {} owner vm={} root={:#x} vm-slot={}\n",
            label,
            owner,
            vm_id,
            root,
            fault_vm_slot,
        );
        let candidates = [
            ("raw", addr_word),
            ("shift6", (addr_word & ((1u64 << 42) - 1)) << 6),
            ("shift6-masked", (addr_word << 6) & ((1u64 << 42) - 1)),
        ];
        for (name, candidate) in candidates {
            let page = candidate & !(mmu::UAT_PGSZ as u64 - 1);
            let mapped = vm.covers_range(page, mmu::UAT_PGSZ as u64, true, false);
            let translation = vm.translate_iova(candidate);
            dev_info!(
                dev.as_ref(),
                "G17P fault address[{}]: {} candidate {:#x} page {:#x} mapped={} translate={:?}\n",
                label,
                name,
                candidate,
                page,
                mapped,
                translation,
            );
            let window_start = page.saturating_sub(0x10_0000);
            let window_end = match page.checked_add(0x10_0000) {
                Some(end) => end,
                None => continue,
            };
            let mut ranges = [pgtable::MappedRange::default(); 16];
            match vm.mapped_ranges(window_start..window_end, &mut ranges) {
                Ok((count, truncated)) => {
                    dev_info!(
                        dev.as_ref(),
                        "G17P fault address[{}]: {} neighbourhood {:#x}..{:#x} has {} run(s){}\n",
                        label,
                        name,
                        window_start,
                        window_end,
                        count,
                        if truncated { " TRUNCATED" } else { "" },
                    );
                    for run in ranges.iter().take(count) {
                        dev_info!(
                            dev.as_ref(),
                            "G17P fault address[{}]:   {:#x}..{:#x} pte={:#x}\n",
                            label,
                            run.start,
                            run.end,
                            run.pte,
                        );
                    }
                }
                Err(error) => dev_info!(
                    dev.as_ref(),
                    "G17P fault address[{}]: {} neighbourhood walk failed ({:?})\n",
                    label,
                    name,
                    error,
                ),
            }
        }

        // One fault-only discriminator for Honeykrisp's sparse buffer layout:
        // a mapped lower peer with an unmapped upper address means the shadow
        // bind disappeared, while two unmapped peers point to a stale or
        // malformed address. Keep this to one line; it is not submit logging.
        let decoded = (addr_word << 6) & ((1u64 << 42) - 1);
        if let Some(candidate) = g17p_fault_sparse_ro_lower(decoded) {
            let page = candidate & !(mmu::UAT_PGSZ as u64 - 1);
            let mapped = vm.covers_range(page, mmu::UAT_PGSZ as u64, true, false);
            let translation = vm.translate_iova(candidate);
            dev_info!(
                dev.as_ref(),
                "G17P fault address[{}]: Honeykrisp sparse-RO lower peer {:#x} page {:#x} mapped={} translate={:?}\n",
                label,
                candidate,
                page,
                mapped,
                translation,
            );
        }

        // The whole client root, so any decode of the address register can be
        // tested against the real layout instead of one window at a time.
        let mut all = [pgtable::MappedRange::default(); 96];
        // The full 42-bit low half, from the client null view up to the
        // unknown page, so no decode of the address register is excluded by
        // the window rather than by the tables.
        let user_range = 0u64..(1u64 << 42);
        match vm.mapped_ranges(user_range.clone(), &mut all) {
            Ok((count, truncated)) => {
                dev_info!(
                    dev.as_ref(),
                    "G17P fault address[{}]: client root {:#x}..{:#x} holds {} run(s){}\n",
                    label,
                    user_range.start,
                    user_range.end,
                    count,
                    if truncated { " TRUNCATED" } else { "" },
                );
                for run in all.iter().take(count) {
                    dev_info!(
                        dev.as_ref(),
                        "G17P fault address[{}]: run {:#x}..{:#x} ({:#x}) pte={:#x}\n",
                        label,
                        run.start,
                        run.end,
                        run.end - run.start,
                        run.pte,
                    );
                }
            }
            Err(error) => dev_info!(
                dev.as_ref(),
                "G17P fault address[{}]: client root walk failed ({:?})\n",
                label,
                error,
            ),
        }
    }

    pub(crate) fn log_render_recovery_pre_ack(&mut self, label: &'static str) {
        let render_qids = self.render_sksm.as_ref().map(|queues| {
            [
                queues.tiling.queue_id as usize,
                queues.fragment.queue_id as usize,
            ]
        });
        if let Some([tiling_qid, fragment_qid]) = render_qids {
            let tiling_last = self.g17p_last_submitted_hw_timestamp(tiling_qid);
            let fragment_last = self.g17p_last_submitted_hw_timestamp(fragment_qid);
            dev_info!(
                self.resources.dev().as_ref(),
                "G17P render recovery pre-ack[{}]: last-submitted TA(qid {})={:?} 3D(qid {})={:?}\n",
                label,
                tiling_qid,
                tiling_last,
                fragment_qid,
                fragment_last,
            );
        }
        {
            let dev = self.resources.dev().clone();
            if let Some(storage) = self.render_storage.as_mut() {
                match storage.snapshot() {
                    Ok(state) => dev_info!(
                        dev.as_ref(),
                        "G17P render recovery pre-ack[{}]: tiling done/read/write {}/{}/{} fragment {}/{}/{} job-list-empty {} tiling-ts [{:#x},{:#x}] fragment-ts [{:#x},{:#x}]\n",
                        label,
                        state.tiling.done,
                        state.tiling.read,
                        state.tiling.write,
                        state.fragment.done,
                        state.fragment.read,
                        state.fragment.write,
                        state.job_list_empty,
                        state.tiling_timestamps[0],
                        state.tiling_timestamps[1],
                        state.fragment_timestamps[0],
                        state.fragment_timestamps[1],
                    ),
                    Err(error) => dev_warn!(
                        dev.as_ref(),
                        "G17P render recovery pre-ack[{}]: queue snapshot failed ({:?})\n",
                        label,
                        error
                    ),
                }
            }
        }
        self.log_render_status_pages(label);
        self.log_render_sksm_entries(label);
        if g17p_native_fragment_first_mode(
            *crate::module_parameters::g17p_render_native_doorbells.value(),
        ) {
            self.log_render_outer_channels(label);
        }
        if let Err(error) = self.dump_completion_records(label) {
            let dev = self.resources.dev().clone();
            dev_warn!(
                dev.as_ref(),
                "G17P render recovery pre-ack[{}]: completion dump failed ({:?})\n",
                label,
                error
            );
        }
    }

    /// DRAM-only TA/3D outer-channel state. The current slot record pointer and
    /// queue-id byte prove whether firmware consumed the Fragment slot after
    /// its dedicated doorbell without touching the SGX MMIO aperture.
    pub(crate) fn log_render_outer_channels(&mut self, label: &'static str) {
        let snapshots = match self.resources.snapshot_all_primary_work_channels() {
            Ok(snapshots) => snapshots,
            Err(error) => {
                dev_warn!(
                    self.resources.dev().as_ref(),
                    "G17P render outer[{}]: snapshot failed ({:?})\n",
                    label,
                    error
                );
                return;
            }
        };
        // Log EVERY work channel, not the first two. The userspace render's
        // tables are TA_2/3D_2 (indices 6/7) and compute publishes on CL_2 (8).
        // Native's distinct system bring-up queues occupy TA_0/3D_0.
        for snapshot in snapshots.into_iter() {
            let name = if snapshot.table_index
                == g17_submission::G17P_PARTIAL_OPENING_TILING_CHANNEL_TABLE_INDEX
            {
                "TA"
            } else if snapshot.table_index
                == g17_submission::G17P_PARTIAL_OPENING_FRAGMENT_CHANNEL_TABLE_INDEX
            {
                "3D"
            } else if snapshot.table_index
                == g17_submission::G17P_COMPUTE_CHANNEL_TABLE_INDEX
            {
                "CL"
            } else {
                "--"
            };
            if snapshot.cursors == [0, 0, 0] && snapshot.current_slot_queue == 0 {
                continue;
            }
            let tracker = self.render_tracker.as_mut().ok_or(ENODEV)?;
            let completion = g17_completion::observe_paired_render_completion(
                tracker.tiling_previous,
                state.tiling,
                tracker.tiling_prefix,
                tracker.fragment_previous,
                state.fragment,
                tracker.fragment_prefix,
                g17_completion::RenderExecutionState {
                    job_list_empty: state.job_list_empty,
                    tiling_timestamps: state.tiling_timestamps,
                    fragment_timestamps: state.fragment_timestamps,
                }
            };
            let observe = |state: &g17_resources::G17PUserRenderQueueState| {
                g17_completion::observe_paired_render_window(
                    window,
                    tiling_previous,
                    state.tiling,
                    fragment_previous,
                    state.fragment,
                    execution(state),
                )
            };
            let completion = match observe(&state) {
                Ok(completion) => completion,
                Err(first_error) => {
                    // The six producer/consumer words are firmware-owned and
                    // sampled separately.  An event edge can therefore catch
                    // one cache line before the paired line becomes visible.
                    // Retry one immediate DRAM snapshot; this is bounded edge
                    // handling, not a completion poll or a steady-state timer.
                    fence(Ordering::Acquire);
                    let retry = self
                        .render_storage
                        .as_mut()
                        .ok_or(ENODEV)?
                        .snapshot()?;
                    match observe(&retry) {
                        Ok(completion) => {
                            state = retry;
                            completion
                        }
                        Err(second_error) => {
                            dev_err!(
                                self.resources.dev().as_ref(),
                                "G17P render completion: ordinal {} target {} rejected two snapshots ({:?}/{:?}); previous TA {}/{}/{} 3D {}/{}/{} first TA {}/{}/{} 3D {}/{}/{} retry TA {}/{}/{} 3D {}/{}/{}\n",
                                self.render_submission_ordinal,
                                window.target(),
                                first_error,
                                second_error,
                                tiling_previous.done,
                                tiling_previous.read,
                                tiling_previous.write,
                                fragment_previous.done,
                                fragment_previous.read,
                                fragment_previous.write,
                                state.tiling.done,
                                state.tiling.read,
                                state.tiling.write,
                                state.fragment.done,
                                state.fragment.read,
                                state.fragment.write,
                                retry.tiling.done,
                                retry.tiling.read,
                                retry.tiling.write,
                                retry.fragment.done,
                                retry.fragment.read,
                                retry.fragment.write,
                            );
                            return Ok(G17PRenderPollOutcome::Retry);
                        }
                    }
                }
            };
            let tracker = self.render_tracker.as_mut().ok_or(ENODEV)?;
            if completion.complete {
                // Queue retirement can become visible before the TA command's
                // EventControl has reached its own submitted target and been
                // unlinked. Releasing the fence at that intermediate state
                // lets the next command consume another Pool-A record until
                // all 35 appear busy. The matching type-1 edge is the wake;
                // this is its single ordered host-DRAM validation.
                let event_control = self
                    .render_storage
                    .as_mut()
                    .ok_or(ENODEV)?
                    .render_event_control_state()?;
                if !event_control.unlinked() {
                    if crate::util::g17p_verbose() {
                        dev_info!(
                            self.resources.dev().as_ref(),
                            "G17P render completion: EventControl {} pending submitted={} completed={} membership={:#x}\n",
                            event_control.record_index,
                            event_control.submitted,
                            event_control.completed,
                            event_control.membership,
                        );
                    }
                    return Ok(G17PRenderPollOutcome::Pending);
                }
                // B1 clears EventControl membership BEFORE GPU cache
                // maintenance. Its fragment stamp A write follows that
                // maintenance (25G83 B1 0x16ea0); queue/list retirement alone
                // therefore cannot authorize a userspace-visible fence.
                // The physical slot retains its ordinal across VM switches,
                // has one in-flight pair, and refuses 24-bit stamp overflow.
                // Require this exact generation, never a stale nonzero stamp.
                let pair = self.render_storage.as_ref().ok_or(ENODEV)?.queue_pair();
                let expected_stamp = self.render_submission_ordinal
                    .checked_add(1)
                    .and_then(|value| value.checked_mul(0x100))
                    .ok_or(EOVERFLOW)?;
                let fragment_stamp = self.resources.render_fragment_completion_stamp(pair)?;
                if fragment_stamp != expected_stamp {
                    if crate::util::g17p_verbose() {
                        dev_info!(
                            self.resources.dev().as_ref(),
                            "G17P render completion: fragment stamp pending qid={} observed={:#x} expected={:#x}\n",
                            pair.fragment,
                            fragment_stamp,
                            expected_stamp,
                        );
                    }
                    // The post-stamp type-1 notification wakes this job.
                    // Keep its ownership and fence pending without polling.
                    return Ok(G17PRenderPollOutcome::Pending);
                }
                // Publish the exact GPU-written pass envelope while the completed job's
                // VM binding, ordinal and QIDs are still owned.  This is
                // observation only: it neither gates completion nor changes
                // queue state.  Invalid retirement-only pairs are omitted.
                if let Some(interval) =
                    g17_completion::render_gpu_interval(
                        g17_completion::RenderExecutionState {
                            job_list_empty: state.job_list_empty,
                            tiling_timestamps: state.tiling_timestamps,
                            fragment_timestamps: state.fragment_timestamps,
                        },
                    )
                {
                    let (vm_id, root) = self.render_bind.as_ref()
                        .map_or((0, 0), |binding| (binding.vm_id(), binding.root()));
                    let fallback_pair = self.render_storage
                        .as_ref()
                        .map(g17_resources::G17PUserRenderStorage::queue_pair)
                        .unwrap_or(g17_resources::G17PRenderQueuePair::for_slot(
                            self.active_render_slot,
                        ).ok_or(EINVAL)?);
                    let (tiling_qid, fragment_qid) = self.render_sksm.as_ref()
                        .map_or((fallback_pair.tiling, fallback_pair.fragment), |queues| (
                            u16::from(queues.tiling.queue_id),
                            u16::from(queues.fragment.queue_id),
                        ));
                    if crate::util::g17p_verbose() {
                        dev_info!(
                            self.resources.dev().as_ref(),
                            "G17P render: GPU-INTERVAL ordinal={} vm={} root={:#x} ta-qid={} fragment-qid={} start={} end={}\n",
                            self.render_submission_ordinal,
                            vm_id,
                            root,
                            tiling_qid,
                            fragment_qid,
                            interval[0],
                            interval[1],
                        );
                    }
                }
                // Validate and commit the context-retirement epoch before the
                // ordinary occupancy transaction starts mutating allocator
                // state. Every fallible retirement precondition is therefore
                // checked before an HWPB lease or queue producer is changed.
                self.complete_render_context_update_retirement()?;
                // Native TA/3D complete subtract completed kicks from queue
                // +44. Prepare both results before mutating either queue;
                // timestamps and previous-stamp history must never reset.
                let retired_sksm = self.render_sksm.as_ref().map(|queues| {
                    let ta = g17_submission::complete_g17p_cl_kicks(queues.tiling.producer, 1)?;
                    let fragment = g17_submission::complete_g17p_cl_kicks(queues.fragment.producer, 1)?;
                    Ok::<_, g17_submission::SubmissionError>((ta, fragment))
                }).transpose().map_err(|_| EIO)?;
                let next_ordinal = self.render_submission_ordinal.checked_add(1).ok_or(EOVERFLOW)?;
                let hardware_buffer_id = self.render_hardware_buffer_id.ok_or(EIO)?;
                self.render_hardware_buffer_ids
                    .release(hardware_buffer_id)
                    .map_err(|_| EIO)?;
                self.release_active_render_command_buffer_states()?;
                if let (Some(queues), Some((ta, fragment))) = (self.render_sksm.as_mut(), retired_sksm) {
                    queues.tiling.producer = ta;
                    queues.fragment.producer = fragment;
                }
                self.render_hardware_buffer_id = None;
                self.render_tracker = None;
                self.render_hardware_buffer_published = false;
                self.render_auxiliary_timestamp = 0;
                if crate::util::g17p_verbose() {
                    dev_info!(
                        self.resources.dev().as_ref(),
                        "G17P render: released TA HardwareBufferID {} at paired completion\n",
                        hardware_buffer_id,
                    );
                }
                self.render_submission_ordinal = next_ordinal;
                // One control sample per boot, from a render that DID complete.
                //
                // Every field of the `context/qos` line captured at a wedge --
                // slot=255 ctx=255 prio=0 class=0 state=2 bound-slot=255 and an
                // all-zero qid bitmap -- is byte-for-byte what
                // `apply_g17p_cold_opening_channel_control` writes at cold
                // opening (`0x0000_0100_0000_ffff` at +0x00, `0x0002_..` at
                // +0x20, `0x0000_0000_ff00_0000` at +0x30, `fill(0)` elsewhere).
                // So that line proves only that the record is UNTOUCHED, which
                // is a different claim from "the context was lost", and without
                // a healthy sample the two are indistinguishable. If this
                // control prints the same constants, the firmware never writes
                // this record and the diagnostic -- and the comment above it
                // claiming it is "the exact host-safe witness" -- is wrong.
                if !RENDER_CONTEXT_WITNESS_LOGGED.swap(true, Ordering::Relaxed) {
                    self.log_render_context_witness("healthy-paired-completion");
                }
                return Ok(G17PRenderPollOutcome::Complete(completion));
            }
            tracker.tiling_previous = state.tiling;
            tracker.fragment_previous = state.fragment;
            if completion.complete {
                self.render_tracker = None;
                self.render_submission_ordinal = self
                    .render_submission_ordinal
                    .checked_add(1)
                    .ok_or(EOVERFLOW)?;
                return Ok(completion);
            }
            fsleep(Delta::from_millis(1));
        }
        // The graph remains pinned because firmware may still own it. Runtime
        // teardown stops both provider CPUs before manager resources drop.
        Err(ETIMEDOUT)
    }

    pub(crate) fn sksm_scratch(&self) -> G17SksmScratchGeometry {
        self.platform.sksm_scratch
    }

    /// Return the hardware QID of a queue owned by this manager.
    pub(crate) fn compute_queue_id(&self, queue: &G17PSksmQueue) -> Result<u8> {
        if queue.scratch != self.sksm_scratch() || queue.uat_owner != self.uat().ttb_base() {
            return Err(EINVAL);
        }
        Ok(queue.queue_id)
    }

    pub(crate) fn bind_compute_user_vm(
        &mut self,
        queue: &mut G17PSksmQueue,
        vm: &mmu::Vm,
    ) -> Result<G17PComputeUserBinding> {
        if !queue.registered
            || queue.scratch != self.sksm_scratch()
            || queue.uat_owner != self.uat().ttb_base()
        {
            return Err(EINVAL);
        }
        let mappings = queue.compute.map_user_execution_state(vm)?;
        let vm_bind = self.resources.uat().bind(vm)?;
        if vm_bind.slot() != 1 || !mappings.all_reachable_from(vm) {
            return Err(EFAULT);
        }
        Ok(G17PComputeUserBinding {
            mappings,
            _vm_bind: vm_bind,
        })
    }

    /// Configure the hardware-proven G17P compute queue through resource 0 and
    /// return its retained state only after the QID-valid bit is published.
    pub(crate) fn configure_g17p_compute_queue(
        &mut self,
        dev: &AsahiDevice,
        registers: &regs::Resources,
    ) -> Result<G17PSksmQueue> {
        let queue_id = self.allocate_compute_queue_id()?;
        let hardware_buffer_id = match self.render_hardware_buffer_ids.allocate_compute() {
            Ok(id) => u8::try_from(id).map_err(|_| ERANGE)?,
            Err(_) => {
                self.next_compute_queue_id = Some(queue_id);
                return Err(ENOSPC);
            }
        };
        let result = (|| {
        const COMPLETION_SELECTOR: u8 = 2;

        let mut storage = g17_resources::G17PClRuntimeStorage::new(dev, self.uat())?;
        let compute = g17_resources::G17PComputeDescriptorStorage::new(
            dev,
            self.uat(),
            storage.channel_control_gpu_va(),
        )?;
        let operand_table_gpu_va = storage.operand_table_gpu_va();
        let support_state_gpu_va = storage.support_state_gpu_va();
        let scheduler_state_gpu_va = storage.scheduler_state_gpu_va();
        storage.with_static_pages_mut(
            |shared_support, support_state, scheduler_state, channel_control| {
                g17_submission::apply_g17p_cl_retained_static_state(
                    shared_support,
                    support_state,
                    scheduler_state,
                    channel_control,
                    g17_initdata::COMPUTE_FLIST_PAGE_LIST_ADDRESS,
                    operand_table_gpu_va,
                    support_state_gpu_va,
                    scheduler_state_gpu_va,
                )
                .map_err(|_| EINVAL)
            },
        )?;
        let routing = g17_submission::G17PComputeQueueRouting {
            queue_id: QUEUE_ID,
            completion_selector: COMPLETION_SELECTOR as u32,
            entry_gpu_va: storage.entry_gpu_va(),
        };
        let config =
            g17_submission::prepare_g17p_compute_sksm_queue_config(routing, self.sksm_scratch())
                .map_err(|_| EINVAL)?;

        // Every firmware-visible page and its DVA must be globally published
        // before the config payload makes the queue reachable from SKSM. The
        // MMIO helper then stores config word 0, config word 1, and finally the
        // two QID-valid mask words under one fixed-slot FIFO lock.
        fence(Ordering::SeqCst);
        registers.g17p_sksm_configure_queue(config.write_pair(), config.enable_pair)?;

        Ok(G17PSksmQueue {
            queue_id: routing.queue_id,
            hardware_buffer_id,
            scratch: self.sksm_scratch(),
            uat_owner: self.uat().ttb_base(),
            disable_pair: config.disable_pair,
            registered: true,
            storage,
            compute,
            producer: g17_submission::G17PClKickProducerState {
                queue_id: QUEUE_ID,
                num_stamps: 256,
                entry_stride: 0x200,
                completion_selector: COMPLETION_SELECTOR,
                qos: g17_submission::G17PClQosConfig {
                    queue_byte_32: 1,
                    class_40: 8,
                    word_48: 0xffff,
                },
                submitted: 0,
                current_timestamp: 1,
                base_timestamp: 1,
                previous_valid: false,
                previous_timestamp: 0,
            },
            submission_ordinal: 0,
            b2_activation: g17_submission::G17PClB2ActivationState::Pending,
        })
        })();
        // No firmware publication occurs in this constructor. A failed host
        // allocation must not consume the finite QID namespace permanently.
        if result.is_err() {
            self.next_compute_queue_id = Some(queue_id);
            self.render_hardware_buffer_ids
                .release(u32::from(hardware_buffer_id))
                .map_err(|_| EIO)?;
        }
        result
    }

    /// Prepare a queue for its firmware-mediated tag-15 installation.
    ///
    /// Native performs no host MMIO during steady-state rendering, and the
    /// working QID4 path also succeeds with the AP SKSM aperture suppressed.
    /// The queue therefore becomes installed only when its tag-15 group is
    /// published to the firmware channel below, never from an AP port write.
    fn prepare_compute_firmware_install(
        &self,
        queue: &mut G17PSksmQueue,
    ) -> Result {
        if queue.scratch != self.sksm_scratch() || queue.uat_owner != self.uat().ttb_base() {
            return Err(EINVAL);
        }
        if queue.hardware_state != G17PSksmQueueHardwareState::PreparedMemory {
            return Err(EBUSY);
        }
        if queue.configure_guard_accelerator_stamp != queue.configure_guard_event_stamp {
            dev_err!(
                self.resources.dev().as_ref(),
                "G17P QID {} configure guard mismatch accelerator={} event-machine={}\n",
                queue.queue_id,
                queue.configure_guard_accelerator_stamp,
                queue.configure_guard_event_stamp
            );
            return Err(EBUSY);
        }
        queue.storage.prepare_live_mappings(self.uat())?;
        dev_info!(
            self.resources.dev().as_ref(),
            "G17P submitBuffer/prepareMappings: low/high QID4 aliases published to live UAT\n"
        );

        fence(Ordering::SeqCst);
        Ok(())
    }

    /// Remove a retained queue from the hardware bridge before releasing its
    /// backing storage. Repeated teardown is harmless.
    pub(crate) fn unregister_compute_sksm_queue(
        &self,
        registers: &regs::Resources,
        queue: &mut G17PSksmQueue,
    ) -> Result {
        if queue.scratch != self.sksm_scratch() || queue.uat_owner != self.uat().ttb_base() {
            return Err(EINVAL);
        }
        if !queue.registered {
            return Ok(());
        }

        registers.g17p_sksm_publish_write_pair(queue.disable_pair)?;
        queue.registered = false;
        Ok(())
    }

    /// Publish one complete CL entry, then perform the queue's ordered SKSM
    /// notification. The first post-registration entry publishes the retained
    /// B2 state; later entries keep those provider-owned pages unchanged.
    pub(crate) fn submit_compute_entry(
        &mut self,
        registers: &regs::Resources,
        queue: &mut G17PSksmQueue,
        operands: g17_submission::G17PClKickEntryOperands<'_>,
    ) -> Result<u64> {
        if !queue.registered
            || queue.scratch != self.sksm_scratch()
            || queue.uat_owner != self.uat().ttb_base()
        {
            return Err(EINVAL);
        }
        let prepared = g17_submission::prepare_g17p_cl_kick_entry(queue.producer, operands)
            .map_err(|_| EINVAL)?;
        self.publish_compute_entry(registers, queue, prepared)
    }

    fn publish_compute_entry(
        &mut self,
        registers: &regs::Resources,
        queue: &mut G17PSksmQueue,
        prepared: g17_submission::PreparedG17PClKickEntry,
    ) -> Result<u64> {
        if !self.runtime_tick_staged {
            // Stage the runtime 0x2e tick exactly the way the output-positive
            // paths do: written and producer-published with no doorbell,
            // pending for this work kick to consume. The parked co-processor
            // never answers a bare 0x84 announcement for it.
            let target = self.publish_compute_runtime_control_record(0)?;
            self.runtime_tick_staged = true;
            dev_info!(
                self.resources.dev().as_ref(),
                "G17P compute control: runtime tick 0 staged for this kick (target {})\n",
                target
            );
        }
        queue.storage.write_entry(
            prepared.entry_offset as usize,
            prepared.zero_length as usize,
            &prepared.entry,
        )?;

        self.publish_compute_b2_once(queue)?;

        fence(Ordering::SeqCst);
        queue.producer = prepared.producer_after;
        let timestamp = prepared.current_timestamp;
        self.add_sksm_kicks(registers, queue, 1)?;
        fence(Ordering::SeqCst);
        Ok(timestamp)
    }

    fn publish_compute_b2_once(&mut self, queue: &mut G17PSksmQueue) -> Result {
        if queue.b2_activation == g17_submission::G17PClB2ActivationState::Pending {
            let channel_control_gpu_va = queue.storage.channel_control_gpu_va();
            queue.b2_activation =
                self.resources
                    .with_b2_pages_mut(|computed, region_1, region_2| {
                        g17_submission::apply_g17p_cl_b2_primary_state(
                            queue.b2_activation,
                            computed,
                            region_1,
                            region_2,
                            channel_control_gpu_va,
                        )
                        .map_err(|_| EINVAL)
                    })?;
        }
        Ok(())
    }

    fn submit_translated_add3_channel(
        &mut self,
        queue: &mut G17PSksmQueue,
        binding: &G17PComputeUserBinding,
        command: &g17_uapi::TranslatedComputeCommand,
        context: G17PComputeSubmissionContext<'_>,
        buffers: [u64; 3],
    ) -> Result<u64> {
        if queue.submission_ordinal != 0 {
            return Err(EINVAL);
        }

        let descriptor_slot = 0;
        let (descriptor_high, _) = queue.compute.descriptor_gpu_vas(descriptor_slot);
        let descriptor_low = binding.mappings.descriptor_gpu_va(descriptor_slot);
        queue.compute.write_g17p_add3_resource_table(buffers)?;

        let user_layout = binding.mappings.layout();
        let shared_control = queue.storage.shared_support_gpu_va();
        let objects = g17_compute::ComputeDescriptorObjects {
            scheduler_record: queue.compute.scheduler_record_gpu_va(),
            descriptor_low_alias: descriptor_low,
            dispatch_a: queue.compute.dispatch_a_gpu_va(),
            dispatch_b: queue.compute.dispatch_b_gpu_va(),
            status_a: queue.compute.status_a_gpu_va(),
            status_b: queue.compute.status_b_gpu_va(),
            shared_control,
            zero_page: queue.compute.zero_page_gpu_va(),
        };
        queue.compute.with_descriptor_mut(descriptor_slot, |raw| {
            g17_uapi::build_compute_descriptor(
                command,
                g17_uapi::ComputeInternalState {
                    preempt_base: user_layout.preempt,
                    dispatch_identity: context.dispatch_identity,
                    context_id: context.context_id,
                    work_ordinal: 0,
                    robustness: user_layout.robustness,
                    operand_state_base: user_layout.operand_state,
                    execution_gate: context.execution_gate,
                },
                objects,
                g17_compute::ComputeDescriptorMetadata {
                    submit_sequence: 0,
                    context_id: context.context_id,
                    grid_index: g17_submission::G17P_COMPUTE_CHANNEL_GRID_INDEX as u32,
                    work_ordinal: 0,
                    queue_submission: 1,
                    queue_ordinal: 0,
                    submission_index: 1,
                    support_control: 0xe0a0_0001,
                    support_flags: 0,
                },
                context.timestamps,
                raw,
            )
            .map_err(|_| EINVAL)
        })?;

        let command_state = g17_submission::prepare_g17p_compute_cl_command_state(
            descriptor_low,
            queue.submission_ordinal,
        )
        .map_err(|_| EINVAL)?;
        let prepared_entry = g17_submission::prepare_g17p_cl_kick_entry(
            queue.producer,
            g17_submission::G17PClKickEntryOperands {
                descriptor_flag_4c: false,
                descriptor_flag_5c8: false,
                converted_command_timestamp: 1,
                barriers: &[],
                mcache: None,
                payload: [descriptor_high, queue.compute.queue_record_gpu_va()],
                event_mask: command_state.event_mask,
                rce_kind: 0,
                rce_bindings: command_state.rce_bindings,
                auxiliary: context.auxiliary,
            },
        )
        .map_err(|_| EINVAL)?;
        if prepared_entry.entry_index != 1
            || prepared_entry.entry_offset != queue.producer.geometry.entry_stride()
            || prepared_entry.current_timestamp != queue.producer.geometry.timestamp_seed()
        {
            return Err(EINVAL);
        }
        let backend = g17_submission::T8140_G17P_CL_BACKEND;
        if !g17_submission::g17p_first_bind_tag15_required(true)
            || !g17_submission::g17p_first_bind_tag16_required(backend, true)
        {
            return Err(EINVAL);
        }
        let mut entry_signal =
            [0u8; g17_submission::G17P_COMPUTE_OPTIONAL_EVENT_SIZE];
        // Tag 16 names the last retired entry. A newly installed queue has no
        // predecessor, so the protocol value is zero.
        let first_bind_old_timestamp = prepared_entry.current_timestamp.saturating_sub(1);
        g17_submission::encode_g17p_compute_optional_event(
            g17_submission::G17PComputeOptionalEvent {
                queue_id: u16::from(queue.queue_id),
                old_timestamp: first_bind_old_timestamp,
            },
            &mut entry_signal,
        )
        .map_err(|_| EINVAL)?;
        if let Ok(window) = queue.compute.g17p_preempt_argument_window() {
            // +0x1480 is where 0x1a4d0..0x1a4e8 point; +0x14a0 (this dump's
            // +0x20) is the add3 operand table. Populated for the proof path,
            // untouched for a Honeykrisp dispatch.
            self.log_record("preempt+0x1480", &window);
        }
        self.log_record("cl-kick-entry", &prepared_entry.entry);
        self.log_record("tag16-entry-signal", &entry_signal);

        let sksm_entry_aliases = queue.storage.entry_aliases()?;
        // Tag 15 installs a queue exactly once. This path is reachable only for
        // the prepared first bind; retained submissions use the repeat encoder
        // with the install flag clear.
        let install_queue = matches!(
            queue.hardware_state,
            G17PSksmQueueHardwareState::PreparedMemory
        );
        // The single most diagnostic line for a client handoff. The tag-15
        // queue-install flag must be published exactly once for each fresh QID.
        // Reusing an installed QID with another context would ask firmware to
        // reprogram a live KSMFE queue and strand one of the two routes.
        dev_info!(
            self.resources.dev().as_ref(),
            "G17PMARK tag15-install i={} z={} r={} h={} qid={} flag={} (first-bind chronology; a second install on the SAME qid in one boot is the second-client bug)\n",
            {
                let n = G17P_MARK_TAG15_INSTALLS.fetch_add(1, Ordering::Relaxed) + 1;
                n
            },
            G17P_MARK_ZERO_DURATION.load(Ordering::Relaxed),
            G17P_MARK_GATE_REBUILDS.load(Ordering::Relaxed),
            G17P_MARK_RETAINED_HANDOFFS.load(Ordering::Relaxed),
            queue.queue_id,
            install_queue as u32,
        );
        let mut group = queue
            .compute
            .stage_initial_channel_group_without_fallback_event(
                descriptor_high,
                sksm_entry_aliases,
                shared_control,
                queue.storage.channel_control_gpu_va(),
                queue.queue_id,
                queue.hardware_buffer_id,
                install_queue,
                binding.declared_context_id(),
            )?;
        // Purely precompute the host-only sequence token and notification
        // predicates. Retained state is not committed until after the SKSM
        // producer transition, matching the native correlation chronology.
        let lifecycle = g17_submission::prepare_g17p_direct_cl_lifecycle(
            queue.direct_lifecycle,
            group.queue,
            true,
            backend,
        )
        .map_err(|_| EINVAL)?;
        if !lifecycle.publish_config_update
            || !lifecycle.publish_entry_signal
            || lifecycle.correlation_token == 0
        {
            return Err(EINVAL);
        }
        if *module_parameters::g17p_fault_report.value() != 0 {
            dev_info!(
                self.resources.dev().as_ref(),
                "G17P rec tag15: install_queue={} descriptor_high={:#x} shared_control={:#x} channel_control={:#x} group(queue={:#x} optional={:#x} event={:#x} write_index={})\n",
                install_queue,
                descriptor_high,
                shared_control,
                queue.storage.channel_control_gpu_va(),
                group.queue,
                group.optional,
                group.event,
                group.write_index,
            );
        }
        self.arm_compute_completion(queue, descriptor_high)?;
        self.publish_compute_b2_once(queue)?;
        submission_phase.store(G17P_SUBMIT_PHASE_B2, Ordering::Release);
        self.resources.publish_compute_qos_queue_record(
            queue.queue_id,
            queue.hardware_buffer_id,
            queue.storage.channel_control_gpu_va(),
        )?;
        queue
            .compute
            .publish_initial_config_update_pointer(group)?;
        submission_phase.store(G17P_SUBMIT_PHASE_TAG15, Ordering::Release);
        // Tag 15 carries the firmware-visible queue-install request. Prepare
        // every mapping before the remainder of that group is published; no
        // AP-side SKSM MMIO is valid on J700.
        self.prepare_compute_firmware_install(queue)?;
        dev_info!(
            self.resources.dev().as_ref(),
            "G17P submitBuffer: tag15 firmware install published; writing first SKSM entry\n"
        );
        if g17p_compute_sksm_entry_enabled() {
            queue.storage.write_entry(
                prepared_entry.entry_offset as usize,
                prepared_entry.zero_length as usize,
                &prepared_entry.entry,
            )?;
        }
        fence(Ordering::SeqCst);
        queue.producer = prepared_entry.producer_after;
        submission_phase.store(G17P_SUBMIT_PHASE_SKSM_ENTRY, Ordering::Release);
        let descriptor_generation =
            g17_submission::advance_g17p_compute_descriptor_generation(0, 0)
                .map_err(|_| EINVAL)?;
        queue.direct_lifecycle = lifecycle.next;
        // Native's publication order is tag 15 -> 14 -> 16; ours has been
        // 15 -> 16 -> 14. Selectable so both can be tested from one build.
        let native_order = *module_parameters::g17p_native_order.value() != 0;
        let add_kicks = g17_submission::prepare_g17p_ksm_add_kicks_command(
            g17_submission::G17PKsmAddKicksCommand {
                queue_id: queue.queue_id,
                add_count: descriptor_generation,
                queue_priority: queue.producer.completion_selector,
                entry_stamp: prepared_entry.current_timestamp,
            },
        )
        .map_err(|_| EINVAL)?;
        self.log_record("tag14-add-kicks", &add_kicks.bytes);

        let skip = *module_parameters::g17p_skip.value();
        if native_order {
            // Native's firmware-transport order is tag 3 -> 15 -> 14 -> 16.
            // Tag 16's handler ends with a broadcast MMIO[0x210e0]/[0x210d8]
            // = -1 "go" poke, so it must follow AddKicks, not precede it.
            // publish_initial_config_update_pointer leaves the ring producer
            // at 2. group.write_index is staged as 3 for the default order,
            // where tag 16 advances 2 -> 3 first; in native order tag 14 takes
            // slot 2 itself.
            let defer_kicks = *module_parameters::g17p_defer_add_kicks.value() != 0;
            let after_kicks = if skip & 1 != 0 {
                dev_info!(self.resources.dev().as_ref(), "G17P bisect: tag-14 AddKicks SKIPPED\n");
                2
            } else if defer_kicks {
                // Native defers the AddKicks poke (host flag accel[0x662]) and
                // replays it from an IOP power-state callback. It must not be
                // published while a recovery is open: the unhalt's 128-queue
                // restore rewrites 0x21010/0x21018/0x21020 for every queue from
                // the halt-time snapshot and erases it.
                self.pending_add_kicks = Some(add_kicks.bytes);
                dev_info!(
                    self.resources.dev().as_ref(),
                    "G17P compute: tag-14 AddKicks deferred until recovery closes\n"
                );
                2
            } else {
                queue
                    .compute
                    .publish_add_kicks_command(2, &add_kicks.bytes)?
            };
            queue.producer.last_add_kicks_timestamp = prepared_entry.current_timestamp;
            fence(Ordering::SeqCst);
            submission_phase.store(G17P_SUBMIT_PHASE_ADD_KICKS, Ordering::Release);
            group.write_index = if skip & 2 != 0 {
                dev_info!(self.resources.dev().as_ref(), "G17P bisect: tag-16 EntrySignal SKIPPED\n");
                after_kicks
            } else {
                queue
                    .compute
                    .publish_initial_entry_signal_at(after_kicks, &entry_signal)?
            };
            submission_phase.store(G17P_SUBMIT_PHASE_TAG16, Ordering::Release);
        } else {
            queue
                .compute
                .publish_initial_entry_signal(&entry_signal)?;
            submission_phase.store(G17P_SUBMIT_PHASE_TAG16, Ordering::Release);
            group.write_index = queue
                .compute
                .publish_add_kicks_command(group.write_index, &add_kicks.bytes)?;
            queue.producer.last_add_kicks_timestamp = prepared_entry.current_timestamp;
            fence(Ordering::SeqCst);
            submission_phase.store(G17P_SUBMIT_PHASE_ADD_KICKS, Ordering::Release);
        }
        let prepared = self
            .resources
            .publish_compute_channel(group.queue, group.write_index, queue.queue_id, false)?;
        submission_phase.store(G17P_SUBMIT_PHASE_CL2, Ordering::Release);
        // The complete install group is now visible to firmware. From this
        // point a retry must use the retained route; publishing another
        // install for the same QID could reprogram a live queue.
        queue.hardware_state = G17PSksmQueueHardwareState::HardwareRegistered;
        if lifecycle.notifications.activation != Some(prepared.doorbell) {
            return Err(EINVAL);
        }
        dev_info!(
            self.resources.dev().as_ref(),
            "G17P compute: native-direct-first-v2 qid {} entry_offset {:#x} entry_low {:#x} entry_high {:#x} timestamp {:#x} descriptor_generation {} tag15_producer 2 tag16_producer 3 tag14_producer {} addkicks-via-channel-command=true correlation_token {}\n",
            queue.queue_id,
            prepared_entry.entry_offset,
            sksm_entry_aliases.low(),
            sksm_entry_aliases.high(),
            prepared_entry.current_timestamp,
            descriptor_generation,
            group.write_index,
            lifecycle.correlation_token
        );
        queue.submission_ordinal = 1;
        self.log_compute_ring_state(queue, "first-post-cl2");
        self.log_compute_outer_channel("first-post-cl2");
        Ok(lifecycle.notifications)
    }

    /// The steady-state (second and later) submission on an already-installed
    /// queue.
    ///
    /// Everything the first bind does exactly once is skipped here: the QID
    /// configure transaction, the B2 activation, the queue-install flag in
    /// tag 15, and above all the first-bind graph builder, which rewrites the
    /// queue header and RESETS the item-ring producer to 1. Re-running that
    /// builder for a repeat is what made the firmware's consumer cursor and
    /// our producer disagree, so the record was published and never consumed.
    ///
    /// What DOES repeat, in native's transport order:
    ///
    ///   tag 3 (a new descriptor slot) -> tag 15 -> tag 14 -> tag 16 -> CL_2
    ///
    /// appended at the live ring producer rather than at slots 0..3. macOS
    /// republishes tag 15 on every submission -- both live captures are of
    /// already-installed queues and both carry one -- with `+0x1a` clear.
    fn submit_translated_repeat_channel(
        &mut self,
        registers: &regs::Resources,
        queue: &mut G17PSksmQueue,
        submission_phase: &AtomicU32,
        binding: &G17PComputeUserBinding,
        command: &g17_uapi::TranslatedComputeCommand,
        context: G17PComputeSubmissionContext<'_>,
        add3_buffers: Option<[u64; 3]>,
    ) -> Result<g17_submission::PreparedG17PDirectClNotifications> {
        if queue.hardware_state != G17PSksmQueueHardwareState::HardwareRegistered
            || queue.submission_ordinal == 0
        {
            return Err(EINVAL);
        }
        let _ = registers;
        // The add3 operand table lives in the retained preempt object and is
        // unchanged by a completed submission, but the probe may have staged
        // new buffers, so honour the flag exactly as the first bind does.
        if let Some(buffers) = add3_buffers {
            queue.compute.write_g17p_add3_resource_table(buffers)?;
        }

        let ordinal = queue.submission_ordinal;
        let next_ordinal = ordinal.checked_add(1).ok_or(EOVERFLOW)?;
        // Descriptor ordinals, split into the two fields that behave
        // differently. Hardware has ruled on the extremes: advancing everything
        // (mode 1) fails on the very first repeat, pinning everything (mode 0)
        // gets exactly two consecutive submissions. Mode 2 separates them.
        //
        // `sequence_ordinal` feeds submit_sequence / queue_submission /
        // queue_ordinal / submission_index -- pure per-submission counters, and
        // macOS's CL command record advances its +0x04 sequence once per
        // published record.
        //
        // `work_ordinal` is different in kind: it is folded into the register
        // program as `context_word = (context_id << 8) | work_ordinal`, which
        // m1n1's proven add3 left at 0x200. Mode 1 was sending 0x201, which is
        // the most likely reason it failed immediately.
        // The output-positive 100-submit run keeps both descriptor ordinals at
        // their first-bind values. The item-ring producer and entry timestamp
        // provide the forward progress for repeat commands.
        let sequence_ordinal: u32 = 0;
        let work_ordinal = 0;
        let queue_submission = sequence_ordinal.checked_add(1).ok_or(EINVAL)?;
        let stamp = g17_submission::decode_kick_timestamp(queue.producer.current_timestamp)
            .map_err(|_| EINVAL)?;
        let descriptor_slot = stamp.stamp_index;
        let (descriptor_high, _) = queue.compute.descriptor_gpu_vas(descriptor_slot);
        let descriptor_low = binding.mappings.descriptor_gpu_va(descriptor_slot);
        let user_layout = binding.mappings.layout();
        let shared_control = queue.storage.shared_support_gpu_va();
        let objects = g17_compute::ComputeDescriptorObjects {
            scheduler_record: queue.compute.scheduler_record_gpu_va(),
            descriptor_low_alias: descriptor_low,
            dispatch_a: queue.compute.dispatch_a_gpu_va(),
            dispatch_b: queue.compute.dispatch_b_gpu_va(),
            status_a: queue.compute.status_a_gpu_va(),
            status_b: queue.compute.status_b_gpu_va(),
            shared_control,
            zero_page: queue.compute.zero_page_gpu_va(),
        };
        queue.compute.with_descriptor_mut(descriptor_slot, |raw| {
            g17_uapi::build_compute_descriptor(
                command,
                g17_uapi::ComputeInternalState {
                    preempt_base: user_layout.preempt,
                    dispatch_identity: context.dispatch_identity,
                    context_id: context.context_id,
                    work_ordinal,
                    robustness: user_layout.robustness,
                    operand_state_base: user_layout.operand_state,
                    execution_gate: context.execution_gate,
                },
                objects,
                g17_compute::ComputeDescriptorMetadata {
                    submit_sequence: sequence_ordinal as u64,
                    context_id: context.context_id,
                    grid_index: u32::from(queue.queue_id),
                    work_ordinal,
                    queue_submission,
                    queue_ordinal: sequence_ordinal,
                    submission_index: queue_submission,
                    support_control: 0xe0a0_0001,
                    support_flags: 0,
                },
                context.timestamps,
                raw,
            )
            .map_err(|_| EINVAL)
        })?;

        // Ordinal 0 deliberately: the entry's event mask is a set of event
        // SLOT indices, not a counter. macOS's captured kick entries carry the
        // same constant mask value on every submission of a queue (0x22 for
        // CL), and the CL command record's own event quartet is likewise
        // identical across all twelve captured submissions. Only the RCE
        // bindings, which this call also derives, legitimately move -- they
        // follow the descriptor slot.
        let command_state = g17_submission::prepare_g17p_compute_cl_command_state(
            descriptor_low,
            0,
        )
        .map_err(|_| EINVAL)?;
        let prepared_entry = g17_submission::prepare_g17p_cl_kick_entry(
            queue.producer,
            g17_submission::G17PClKickEntryOperands {
                descriptor_flag_4c: context.descriptor_flag_4c,
                descriptor_flag_5c8: context.descriptor_flag_5c8,
                converted_command_timestamp: context.converted_command_timestamp,
                barriers: context.barriers,
                mcache: context.mcache,
                payload: [descriptor_high, queue.compute.queue_record_gpu_va()],
                event_mask: command_state.event_mask,
                rce_kind: context.rce_kind,
                rce_bindings: command_state.rce_bindings,
                auxiliary: context.auxiliary,
            },
        )
        .map_err(|_| EINVAL)?;
        self.log_record("repeat-cl-kick-entry", &prepared_entry.entry);
        if prepared_entry.entry_index != descriptor_slot {
            return Err(EINVAL);
        }
        let backend = g17_submission::T8140_G17P_CL_BACKEND;
        let mut entry_signal = [0u8; g17_submission::G17P_COMPUTE_OPTIONAL_EVENT_SIZE];
        // Tag 16 names the last retired entry, never the kick being published.
        let entry_old_timestamp = queue.producer.previous_timestamp;
        g17_submission::encode_g17p_compute_optional_event(
            g17_submission::G17PComputeOptionalEvent {
                queue_id: u16::from(queue.queue_id),
                old_timestamp: entry_old_timestamp,
            },
            &mut entry_signal,
        )
        .map_err(|_| EINVAL)?;
        let sksm_entry_aliases = queue.storage.entry_aliases()?;
        // Reclaim everything the firmware has finished with, so slots come back
        // round instead of the ring being single-use.
        let trace = Self::compute_trace_enabled(ordinal);
        match queue.compute.recycle_consumed_ring_slots() {
            Ok(_) if !trace => {}
            Ok(0) => {}
            Ok(reclaimed) => dev_info!(
                self.resources.dev().as_ref(),
                "G17P compute: reclaimed {} consumed item-ring slots\n",
                reclaimed
            ),
            Err(error) => dev_err!(
                self.resources.dev().as_ref(),
                "G17P compute: item-ring reclaim failed ({:?}); publishing without it\n",
                error
            ),
        }
        match *module_parameters::g17p_repeat_context.value() {
            0 => {}
            2 => queue
                .compute
                .write_repeat_queue_context_item(
                    descriptor_slot,
                    descriptor_high,
                    queue.queue_id,
                )?,
            _ => queue.compute.refresh_repeat_queue_context(descriptor_high)?,
        }
        let rewind = *module_parameters::g17p_repeat_ring.value() == 0;
        let group = queue.compute.stage_repeat_config_update(
            sksm_entry_aliases,
            shared_control,
            queue.storage.channel_control_gpu_va(),
            queue.queue_id,
            queue.hardware_buffer_id,
            rewind,
            binding.declared_context_id(),
        )?;
        // `queue_config_dirty` is already false here, so the lifecycle reports
        // no first-bind publications and -- because the channel identity is
        // unchanged -- no priority-2 activation either. The repeat notifies
        // with the direct EP 0x21 kick alone.
        let mut lifecycle = g17_submission::prepare_g17p_direct_cl_lifecycle(
            queue.direct_lifecycle,
            group.queue,
            true,
            backend,
        )
        .map_err(|_| EINVAL)?;
        if *module_parameters::g17p_repeat_activation.value() != 0
            && lifecycle.notifications.activation.is_none()
        {
            lifecycle.notifications.activation =
                Some(g17_submission::encode_g17p_cl_activation(2).map_err(|_| EINVAL)?);
        }

        // The ring is genuinely circular now, so depth is unbounded rather than
        // capped at 0x500/4 = 320 submissions. Free space is measured against
        // the firmware's consumer, which hardware has proven it owns.
        let free = queue.compute.compute_ring_free_slots()?;
        if free < G17P_COMPUTE_REPEAT_RING_RECORDS {
            dev_err!(
                self.resources.dev().as_ref(),
                "G17P compute: item ring full, {} slots free but {} needed; the firmware consumer has stopped advancing\n",
                free,
                G17P_COMPUTE_REPEAT_RING_RECORDS
            );
            return Err(ENOSPC);
        }
        let producer_before = group.write_index;
        self.arm_compute_completion(queue, descriptor_high)?;
        // Order matters and is native's: the command record's pointer first,
        // then ConfigUpdate, then AddKicks, then the EntrySignal whose handler
        // ends with the broadcast "go" poke.
        let after_command = self.publish_repeat_record(
            queue,
            "the tag-3 command pointer",
            producer_before,
            G17PRepeatRecord::Pointer(descriptor_high),
        )?;
        submission_phase.store(G17P_SUBMIT_PHASE_B2, Ordering::Release);
        self.resources.publish_compute_qos_queue_record(
            queue.queue_id,
            queue.hardware_buffer_id,
            queue.storage.channel_control_gpu_va(),
        )?;
        let after_config = self.publish_repeat_record(
            queue,
            "the tag-15 ConfigUpdate pointer",
            after_command,
            G17PRepeatRecord::Pointer(group.optional),
        )?;
        submission_phase.store(G17P_SUBMIT_PHASE_TAG15, Ordering::Release);

        if g17p_compute_sksm_entry_enabled() {
            queue.storage.write_entry(
                prepared_entry.entry_offset as usize,
                prepared_entry.zero_length as usize,
                &prepared_entry.entry,
            )?;
        }
        fence(Ordering::SeqCst);
        let prepared = self
            .resources
            .publish_compute_channel(group.queue, reported_write_index, queue.queue_id, true)?;
        submission_phase.store(G17P_SUBMIT_PHASE_CL2, Ordering::Release);
        let _ = prepared;
        queue.direct_lifecycle = lifecycle.next;
        queue.submission_ordinal = next_ordinal;
        // Optional settle. The 319-deep build padded every submission with
        // several console writes here; this makes that padding explicit and
        // measurable instead of an accident of log verbosity.
        let settle = *module_parameters::g17p_repeat_settle_us.value();
        if settle != 0 {
            fsleep(Delta::from_micros(settle as i64));
        }
        self.log_compute_ring_state(queue, "repeat-post-cl2");
        if !trace {
            return Ok(lifecycle.notifications);
        }
        self.log_compute_outer_channel("repeat-post-cl2");
        dev_info!(
            self.resources.dev().as_ref(),
            "G17P compute: repeat submission {} qid {} descriptor-slot {} descriptor {:#x} stamp {:#x} ring {} -> {} activation {:?} kick {:?}\n",
            ordinal,
            queue.queue_id,
            descriptor_slot,
            descriptor_high,
            prepared_entry.current_timestamp,
            producer_before,
            write_index,
            lifecycle.notifications.activation,
            lifecycle.notifications.direct_kick
        );
        Ok(lifecycle.notifications)
    }

    /// Build and publish one complete userspace compute command.
    ///
    /// Descriptor and CL-entry slots share the same 256-entry lifetime. The
    /// descriptor is written first, then its upper address and the retained
    /// queue record are placed in the CL payload before the ordered SKSM kick.
    pub(crate) fn submit_translated_compute(
        &mut self,
        registers: &regs::Resources,
        queue: &mut G17PSksmQueue,
        binding: &G17PComputeUserBinding,
        command: &g17_uapi::TranslatedComputeCommand,
        context: G17PComputeSubmissionContext<'_>,
    ) -> Result<Option<u64>> {
        if !queue.registered
            || queue.scratch != self.sksm_scratch()
            || queue.uat_owner != self.uat().ttb_base()
        {
            return Err(EINVAL);
        }

        if let Some(buffers) = command.g17p_add3_buffers {
            return self
                .submit_translated_add3_channel(queue, binding, command, context, buffers)
                .map(Some);
        }

        let timestamp = g17_submission::decode_kick_timestamp(queue.producer.current_timestamp)
            .map_err(|_| EINVAL)?;
        let (descriptor_high, _) = queue.compute.descriptor_gpu_vas(timestamp.stamp_index);
        let descriptor_low = binding
            .mappings
            .descriptor_gpu_va(timestamp.stamp_index);
        let command_state = g17_submission::prepare_g17p_compute_cl_command_state(
            descriptor_low,
            queue.submission_ordinal,
        )
        .map_err(|_| EINVAL)?;
        let payload = [descriptor_high, queue.compute.queue_record_gpu_va()];
        let operands = command.entry_operands(g17_uapi::ComputeEntryContext {
            descriptor_flag_4c: context.descriptor_flag_4c,
            descriptor_flag_5c8: context.descriptor_flag_5c8,
            converted_command_timestamp: context.converted_command_timestamp,
            barriers: context.barriers,
            mcache: context.mcache,
            payload,
            event_mask: command_state.event_mask,
            rce_kind: context.rce_kind,
            rce_bindings: command_state.rce_bindings,
            auxiliary: context.auxiliary,
        });
        let prepared = g17_submission::prepare_g17p_cl_kick_entry(queue.producer, operands)
            .map_err(|_| EINVAL)?;
        if prepared.entry_index != timestamp.stamp_index {
            return Err(EINVAL);
        }

        let ordinal = queue.submission_ordinal;
        let next_ordinal = ordinal.checked_add(1).ok_or(EOVERFLOW)?;
        let queue_submission = ordinal.checked_add(1).ok_or(EINVAL)?;
        let user_layout = binding.mappings.layout();
        let preempt_base = user_layout.preempt;
        let robustness = user_layout.robustness;
        let operand_state_base = user_layout.operand_state;
        let shared_control = queue.storage.shared_support_gpu_va();
        let objects = g17_compute::ComputeDescriptorObjects {
            scheduler_record: queue.compute.scheduler_record_gpu_va(),
            descriptor_low_alias: descriptor_low,
            dispatch_a: queue.compute.dispatch_a_gpu_va(),
            dispatch_b: queue.compute.dispatch_b_gpu_va(),
            status_a: queue.compute.status_a_gpu_va(),
            status_b: queue.compute.status_b_gpu_va(),
            shared_control,
            zero_page: queue.compute.zero_page_gpu_va(),
        };
        queue
            .compute
            .with_descriptor_mut(prepared.entry_index, |raw| {
                g17_uapi::build_compute_descriptor(
                    command,
                    g17_uapi::ComputeInternalState {
                        preempt_base,
                        dispatch_identity: context.dispatch_identity,
                        context_id: context.context_id,
                        work_ordinal: ordinal,
                        robustness,
                        operand_state_base,
                        execution_gate: context.execution_gate,
                    },
                    objects,
                    g17_compute::ComputeDescriptorMetadata {
                        submit_sequence: ordinal as u64,
                        context_id: context.context_id,
                        grid_index: 6,
                        work_ordinal: ordinal,
                        queue_submission,
                        queue_ordinal: ordinal,
                        submission_index: queue_submission,
                        support_control: 0xe0a0_0001,
                        support_flags: 0,
                    },
                    context.timestamps,
                    raw,
                )
                .map_err(|_| EINVAL)
            })?;

        fence(Ordering::SeqCst);
        let timestamp = self.publish_compute_entry(registers, queue, prepared)?;
        queue.submission_ordinal = next_ordinal;
        let _ = timestamp;
        Ok(None)
    }

    /// Completion ingress for the descriptor-provided completed-kick count.
    pub(crate) fn complete_compute_entries(
        &self,
        queue: &mut G17PSksmQueue,
        completed_count: u32,
    ) -> Result {
        if !queue.registered
            || queue.scratch != self.sksm_scratch()
            || queue.uat_owner != self.uat().ttb_base()
        {
            return Err(EINVAL);
        }
        queue.producer = g17_submission::complete_g17p_cl_kicks(queue.producer, completed_count)
            .map_err(|_| EINVAL)?;
        Ok(())
    }

    pub(crate) fn snapshot_compute_timeout(
        &mut self,
        queue: &mut G17PSksmQueue,
    ) -> Result<G17PComputeTimeoutSnapshot> {
        Ok(G17PComputeTimeoutSnapshot {
            graph: queue.compute.snapshot_initial_channel_group()?,
            channel: self.resources.snapshot_compute_channel()?,
            control: self.resources.control_counters()?,
        })
    }

    /// Publish newly appended kicks for a queue returned by
    /// [`Self::configure_g17p_compute_queue`].
    pub(crate) fn add_sksm_kicks(
        &self,
        registers: &regs::Resources,
        queue: &mut G17PSksmQueue,
        add_count: u32,
    ) -> Result {
        if !queue.registered
            || queue.scratch != self.sksm_scratch()
            || queue.uat_owner != self.uat().ttb_base()
        {
            return Err(EINVAL);
        }

        let pair = g17_submission::prepare_sksm_add_kicks_mmio(
            g17_submission::SksmAddKicksTarget::G17P,
            queue.queue_id,
            add_count,
            queue.scratch,
        )
        .map_err(|_| EINVAL)?
        .write_pair();
        registers.g17p_sksm_publish_write_pair(pair)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ROOT: u64 = 0xffff_fc20_0100_0000;
    const UAT: u64 = 0x9000_0000;

    fn target() -> TargetObservation {
        TargetObservation {
            chip_id: 0x8140,
            declared_gen: GpuGen::G17,
            declared_variant: GpuVariant::P,
            hw_family: 0x0a,
            hw_variant: 0,
            num_dies: 1,
            uat_ias: 42,
            uat_oas: 42,
        }
    }

    fn resources() -> ResourceObservation {
        ResourceObservation {
            uat_ttb_base: UAT,
            primary_uat_owner: UAT,
            secondary_uat_owner: UAT,
            primary_role: InstanceRole::Primary,
            secondary_role: InstanceRole::Secondary,
            primary_root: ROOT,
            secondary_root: ROOT + g17_initdata::SECONDARY_ROOT_DELTA,
            shared_cluster_size: g17_initdata::NATIVE_SHARED_CLUSTER_SIZE,
        }
    }

    #[test]
    fn manager_config_is_exact_t8140_geometry() {
        assert_eq!(T8140_MANAGER_CONFIG.chip_id, 0x8140);
        assert_eq!(T8140_MANAGER_CONFIG.base_clock_hz, 24_000_000);
        assert_eq!(T8140_MANAGER_CONFIG.uat_ias, 42);
        assert_eq!(T8140_MANAGER_CONFIG.uat_oas, 42);
        assert_eq!(T8140_MANAGER_CONFIG.uat_page_size, 0x4000);
        assert_eq!(T8140_MANAGER_CONFIG.uat_page_bits, 14);
        assert_eq!(
            T8140_MANAGER_CONFIG.uat_levels,
            [(36, 64), (25, 2048), (14, 2048)]
        );
        assert_eq!(
            T8140_MANAGER_CONFIG.initdata_version,
            [0x04c0, 0x0396, 0xa322, 0x0c8a]
        );
        assert_eq!(T8140_MANAGER_CONFIG.root_sizes, [0xb8, 0xc8]);
        assert_eq!(T8140_MANAGER_CONFIG.secondary_root_delta, 0x8000);
        assert_eq!(T8140_MANAGER_CONFIG.secondary_shared_region_delta, 0x40000);
        assert_eq!(T8140_MANAGER_CONFIG.shared_cluster_size, 0x28000);
        assert_eq!(T8140_MANAGER_CONFIG.work_channel_count, 12);
    }

    #[test]
    fn exact_target_and_shared_dual_role_resources_pass() {
        assert_eq!(validate_target(target()), Ok(()));
        assert_eq!(validate_resources(resources()), Ok(()));
    }

    #[test]
    fn scratch_geometry_uses_values_not_accelerator_object_offsets() {
        let platform = T8140_G17P_PLATFORM_CONFIG;
        assert_eq!(platform.sksm_scratch.write0_base, 0x4000);
        assert_eq!(platform.sksm_scratch.write1_base, 0x4008);
        assert_eq!(platform.sksm_scratch.stride, 0x10);
        assert_eq!(
            platform.sksm_scratch.fifo_offsets(),
            Ok(G17SksmScratchOffsets {
                write0: 0x4090,
                write1: 0x4098,
            })
        );
        assert_eq!(validate_platform(platform), Ok(()));
    }

    #[test]
    fn non_g17p_scratch_geometry_fails_before_resource_allocation() {
        for geometry in [
            G17SksmScratchGeometry {
                write0_base: 0,
                ..T8140_G17P_PLATFORM_CONFIG.sksm_scratch
            },
            G17SksmScratchGeometry {
                write1_base: 0,
                ..T8140_G17P_PLATFORM_CONFIG.sksm_scratch
            },
            G17SksmScratchGeometry {
                stride: 0,
                ..T8140_G17P_PLATFORM_CONFIG.sksm_scratch
            },
        ] {
            assert_eq!(
                validate_platform(G17PPlatformConfig {
                    sksm_scratch: geometry
                }),
                Err(ConstructionError::WrongSksmScratchGeometry)
            );
        }
    }

    #[test]
    fn scratch_geometry_overflow_fails_before_resource_allocation() {
        for geometry in [
            G17SksmScratchGeometry {
                write0_base: 0,
                write1_base: 0,
                stride: u32::MAX,
            },
            G17SksmScratchGeometry {
                write0_base: u32::MAX,
                write1_base: 0,
                stride: 1,
            },
            G17SksmScratchGeometry {
                write0_base: 0,
                write1_base: u32::MAX,
                stride: 1,
            },
        ] {
            assert_eq!(
                geometry.fifo_offsets(),
                Err(ConstructionError::SksmScratchOffsetOverflow)
            );
        }
    }

    #[test]
    fn non_t8140_identity_and_uat_geometry_are_invalid() {
        let mut observed = target();
        observed.chip_id = 0x8142;
        assert_eq!(validate_target(observed), Err(ConstructionError::WrongChip));

        let mut observed = target();
        observed.declared_variant = GpuVariant::G;
        assert_eq!(
            validate_target(observed),
            Err(ConstructionError::WrongDeclaredIdentity)
        );

        let mut observed = target();
        observed.hw_family = 0x0b;
        assert_eq!(
            validate_target(observed),
            Err(ConstructionError::WrongHardwareIdentity)
        );

        let mut observed = target();
        observed.num_dies = 2;
        assert_eq!(
            validate_target(observed),
            Err(ConstructionError::WrongHardwareIdentity)
        );

        let mut observed = target();
        observed.uat_ias = 39;
        assert_eq!(
            validate_target(observed),
            Err(ConstructionError::WrongUatGeometry)
        );
    }

    #[test]
    fn split_null_or_wrong_role_uat_is_invalid() {
        let mut observed = resources();
        observed.primary_uat_owner = 0;
        assert_eq!(
            validate_resources(observed),
            Err(ConstructionError::NullUatOwner)
        );

        let mut observed = resources();
        observed.secondary_uat_owner += 0x4000;
        assert_eq!(
            validate_resources(observed),
            Err(ConstructionError::SplitUatOwner)
        );

        let mut observed = resources();
        observed.primary_role = InstanceRole::Secondary;
        assert_eq!(
            validate_resources(observed),
            Err(ConstructionError::WrongRoleTopology)
        );
    }

    #[test]
    fn root_or_shared_cluster_mismatch_is_invalid() {
        let mut observed = resources();
        observed.secondary_root += 0x4000;
        assert_eq!(
            validate_resources(observed),
            Err(ConstructionError::WrongRootPlacement)
        );

        let mut observed = resources();
        observed.shared_cluster_size -= 0x4000;
        assert_eq!(
            validate_resources(observed),
            Err(ConstructionError::WrongSharedClusterSize)
        );
    }
}
