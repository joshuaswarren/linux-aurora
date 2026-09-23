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
use kernel::sync::Arc;
#[cfg(not(test))]
use kernel::time::{delay::fsleep, Delta};
#[cfg(all(not(test), CONFIG_DEV_COREDUMP))]
use crate::g17_trace_capture::{self as trace, TraceArchive};
#[cfg(all(not(test), CONFIG_DEV_COREDUMP))]
use kernel::time::{ClockSource, Monotonic};

#[cfg(not(test))]
use core::sync::atomic::{fence, AtomicBool, AtomicU32, Ordering};

/// Diagnostic classification only; these bits are not a firmware ABI.
pub(crate) mod render_transition {
    pub(crate) const REGISTRATION: u32 = 1;
    pub(crate) const CONTEXT_HANDOFF: u32 = 2;
    pub(crate) const NEW_PB_GRAPH: u32 = 4;
    pub(crate) const FLIST_INIT: u32 = 8;
    pub(crate) const TVB_GROWTH: u32 = 16;
    pub(crate) const DESYNC: u32 = 32;
    pub(crate) const UNREACHABLE: u32 = 64;
    pub(crate) const SESSION_RECYCLE: u32 = 128;
    pub(crate) const UNKNOWN: u32 = 256;

    /// Select exactly one known mutation. Compound transitions stay
    /// observation-only, including a desync which also rebuilds PB storage.
    pub(crate) const fn selected(reasons: u32, class: u32) -> bool {
        class != 0 && class <= DESYNC && class.is_power_of_two() && reasons == class
    }
}

fn g17p_mixed_resource_sample(mode: u32, ordinal: u32, fault: bool) -> bool {
    match mode {
        1 => fault,
        2 => fault || ordinal <= 1 || ordinal % 16 == 0,
        _ => false,
    }
}

#[cfg(not(test))]
pub(crate) const G17P_SUBMIT_PHASE_IDLE: u32 = 0;
#[cfg(not(test))]
pub(crate) const G17P_SUBMIT_PHASE_B2: u32 = 1;
#[cfg(not(test))]
pub(crate) const G17P_SUBMIT_PHASE_TAG15: u32 = 2;
#[cfg(not(test))]
pub(crate) const G17P_SUBMIT_PHASE_QID: u32 = 3;
#[cfg(not(test))]
pub(crate) const G17P_SUBMIT_PHASE_SKSM_ENTRY: u32 = 4;
#[cfg(not(test))]
pub(crate) const G17P_SUBMIT_PHASE_TAG16: u32 = 5;
#[cfg(not(test))]
pub(crate) const G17P_SUBMIT_PHASE_ADD_KICKS: u32 = 6;
#[cfg(not(test))]
pub(crate) const G17P_SUBMIT_PHASE_CL2: u32 = 7;

#[cfg(not(test))]
use crate::{
    driver::AsahiDevice, g17_adt_j700, g17_completion, g17_compute, g17_initdata, g17_lifecycle,
    g17_render, g17_resources, g17_submission, g17_uapi, hw::agx3, identity, mmu,
    module_parameters, pgtable, regs,
};
#[cfg(test)]
use super as g17_submission;
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
/// `g17p_render_slot_globals` bit: tag-15 `+0x3e` publishes the selected
/// render owner's FList lifecycle sequence rather than its submit ordinal.
pub(crate) const G17P_RENDER_SLOT_GLOBAL_FLIST_GENERATION: u32 = 2;

fn g17p_render_slot_globals() -> u32 {
    crate::g17_config::RENDER_SLOT_GLOBALS
}

fn g17p_render_pair_first_outer_submit(started: &[bool], slot: u8) -> Option<bool> {
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
    refs: [u8; G17P_TA_HARDWARE_BUFFER_ID_COUNT],
    tokens: [u64; G17P_TA_HARDWARE_BUFFER_ID_COUNT],
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
            refs: [0; G17P_TA_HARDWARE_BUFFER_ID_COUNT],
            tokens: [0; G17P_TA_HARDWARE_BUFFER_ID_COUNT],
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
        self.refs[index] = 1;
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
                self.refs[index] = 1;
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
        let index = id as usize;
        if self.refs[index] == u8::MAX {
            return Err(G17PHardwareBufferIdError::Exhausted);
        }
        if self.is_free(id) {
            self.free[index / 64] &= !(1u64 << (index % 64));
        } else if self.refs[index] == 0 {
            return Err(G17PHardwareBufferIdError::DoubleFree);
        }
        self.refs[index] += 1;
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
        let was_free = self.is_free(id);
        let id = self.acquire_reserved(id)?;
        let index = id as usize;
        if was_free {
            self.tokens[index] = self.next_token();
        }
        Ok((id, self.tokens[index]))
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
        let token = self.next_token();
        self.tokens[id as usize] = token;
        Ok((id, token))
    }

    /// Return a submission's hold on `id`. A reserved ID stays with its
    /// storage -- off the free list -- until that storage unreserves it.
    fn release(&mut self, id: u32) -> core::result::Result<(), G17PHardwareBufferIdError> {
        let index = id as usize;
        if index >= G17P_TA_HARDWARE_BUFFER_ID_COUNT {
            return Err(G17PHardwareBufferIdError::InvalidId);
        }
        if self.is_free(id) || self.refs[index] == 0 {
            return Err(G17PHardwareBufferIdError::DoubleFree);
        }
        self.refs[index] -= 1;
        if self.refs[index] != 0 {
            return Ok(());
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
pub(crate) enum G17PTvbGrowDisposition {
    IgnoredStale,
    Published,
    Deferred,
}

pub(crate) enum G17PPendingPrimaryControlFlush {
    Empty,
    Blocked,
    Published(g17_resources::G17PDeviceControlPublication),
}

struct G17PPendingPrimaryControl {
    record: [u8; g17_initdata::CONTROL_RECORD_SIZE],
    recovery_generation: u64,
    worker_attempts: u16,
}

const G17P_PRIMARY_CONTROL_RETRY_LIMIT: u16 = 1000;

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

/// A control is consumed when its producer-after cursor is no longer inside
/// the ring's current `(consumer, producer]` pending interval. This remains
/// exact when firmware consumes several records between host observations and
/// across the masked 8-bit wrap; equality alone would strand earlier owners.
fn g17p_control_ring_target_consumed(
    consumer: u32,
    producer: u32,
    target: u32,
) -> core::result::Result<bool, G17PBootstrapUmaLifecycleError> {
    let mask = g17_initdata::CONTROL_RING_INDEX_MASK;
    if consumer > mask || producer > mask || target > mask {
        return Err(G17PBootstrapUmaLifecycleError::WrongCompletion);
    }
    let pending = producer.wrapping_sub(consumer) & mask;
    let target_ahead = target.wrapping_sub(consumer) & mask;
    Ok(target_ahead == 0 || target_ahead > pending)
}

/// One command-pair lease on a retained FList. Pool bytes outlive this lease.
/// Native updateSubmitInfo increments pending command updates independently
/// of optional async growth; final reference retirement emits one 0x2e using
/// the command's captured generation (KEXT 8aa1804/8aa5340).
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
struct G17PBootstrapUmaLifecycle {
    sequence: u64,
    hardware_buffer_id: u32,
    shared_control: u64,
    operand_table: u64,
    cookie: u64,
    hardware_buffer_owned: bool,
    hardware_buffer_references: u32,
    pending_release_count: u32,
    release_producer_after: Option<u32>,
    request: Option<G17PBootstrapUmaCompletion>,
    state: G17PBootstrapUmaRequestState,
}

impl G17PBootstrapUmaLifecycle {
    fn matches_owner(
        &self,
        hardware_buffer_id: u32,
        shared_control: u64,
        operand_table: u64,
        cookie: u64,
    ) -> bool {
        self.hardware_buffer_id == hardware_buffer_id
            && self.shared_control == shared_control
            && self.operand_table == operand_table
            && self.cookie == cookie
    }

    const fn new() -> Self {
        Self {
            sequence: G17P_BOOTSTRAP_UMA_SEQUENCE,
            hardware_buffer_id: G17P_BOOTSTRAP_UMA_HARDWARE_BUFFER_ID,
            shared_control: g17_initdata::CONTROL_SHARED_ADDRESS,
            operand_table: g17_initdata::CONTROL_OPERAND_TABLE_ADDRESS,
            cookie: G17P_BOOTSTRAP_UMA_COOKIE,
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

    /// The current fixed pool has one cold grow request. Acquiring the
    /// unchanged pool creates command references, not another grow request.
    /// Native AGXHardwareBufferIDManager advances the FList cookie only on a
    /// new 0 -> 1 owner reference. Overlapping references to the same retained
    /// owner must keep its cookie stable; otherwise B1 treats the new cookie as
    /// a replacement registration and removes the still-live QID membership.
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
            sequence: if overlapping {
                previous.sequence
            } else {
                sequence
            },
            hardware_buffer_id: previous.hardware_buffer_id,
            shared_control: previous.shared_control,
            operand_table: previous.operand_table,
            cookie: previous.cookie,
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

    fn acquire_owner(
        previous: Option<&Self>,
        sequence: u64,
        hardware_buffer_id: u32,
        shared_control: u64,
        operand_table: u64,
        cookie: u64,
        needs_grow: bool,
    ) -> core::result::Result<Self, G17PBootstrapUmaLifecycleError> {
        let new_request = || G17PBootstrapUmaCompletion {
            stamp: G17P_BOOTSTRAP_UMA_STAMP,
            hardware_buffer_id,
            shared_control,
            operand_table,
            cookie,
            count: G17P_BOOTSTRAP_UMA_COUNT,
        };
        let Some(previous) = previous else {
            if sequence != G17P_BOOTSTRAP_UMA_SEQUENCE { return Err(G17PBootstrapUmaLifecycleError::WrongOwner); }
            if !needs_grow { return Err(G17PBootstrapUmaLifecycleError::WrongOwner); }
            return Ok(Self {
                sequence,
                hardware_buffer_id,
                shared_control,
                operand_table,
                cookie,
                hardware_buffer_owned: true,
                hardware_buffer_references: 4,
                pending_release_count: 2,
                release_producer_after: None,
                request: Some(new_request()),
                state: G17PBootstrapUmaRequestState::Submitted,
            });
        };
        if previous.hardware_buffer_id != hardware_buffer_id
            || previous.sequence.checked_add(1) != Some(sequence)
        {
            return Err(G17PBootstrapUmaLifecycleError::WrongOwner);
        }
        let same_owner = previous.shared_control == shared_control
            && previous.operand_table == operand_table
            && previous.cookie == cookie;
        let overlapping = same_owner
            && previous.state == G17PBootstrapUmaRequestState::Ready
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
        let cold_owner = reacquire && !same_owner;
        let request_grow = cold_owner && needs_grow;
        Ok(Self {
            sequence: if overlapping { previous.sequence } else { sequence },
            hardware_buffer_id,
            shared_control,
            operand_table,
            cookie,
            hardware_buffer_owned: true,
            hardware_buffer_references: if request_grow { 4 } else {
                previous.hardware_buffer_references.checked_add(3)
                    .ok_or(G17PBootstrapUmaLifecycleError::WrongReferenceCount)?
            },
            pending_release_count: previous.pending_release_count.checked_add(2)
                .ok_or(G17PBootstrapUmaLifecycleError::WrongPendingCount)?,
            release_producer_after: None,
            request: if request_grow { Some(new_request()) } else { None },
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
        producer: u32,
    ) -> core::result::Result<bool, G17PBootstrapUmaLifecycleError> {
        let target = self
            .release_producer_after
            .ok_or(G17PBootstrapUmaLifecycleError::WrongPhase)?;
        if !self.release_sent() {
            return Err(G17PBootstrapUmaLifecycleError::WrongPhase);
        }
        if !g17p_control_ring_target_consumed(consumer, producer, target)? {
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
/// B1 scheduler-global masks surrounding the park records. The scheduler pass
/// at `0x12d00` reads the requested pair at `+0x3c8`, stores its sampled
/// hardware-status pair at `+0x3d8`, and keeps blocked/deferred state at
/// `+0x3e8`; `+0x3f8` is the following pair used by that same pass. These are
/// firmware DATA. Never probe the corresponding live MMIO status registers
/// from the host.
const G17P_KSM_SCHEDULER_MASKS_LINK: u64 = 0xffff_fc00_0005_83c8;
/// Per-QID runnable mask updated by the park path at `0x131cc..0x131e4`.
const G17P_KSM_RUNNABLE_MASK_LINK: u64 = 0xffff_fc00_0005_9938;
/// B1 IRQ-to-scheduler handoff words. The hardware IRQ sampler, atomic pending
/// transfer and dispatcher use these in order. Capturing all five after a
/// fault distinguishes an absent IRQ from a pending RTOS wake or dispatch.
const G17P_KSM_IRQ_HANDOFF_LINKS: [u64; 5] = [
    0xffff_fc00_0017_7100,
    0xffff_fc00_0017_7188,
    0xffff_fc00_0017_7190,
    0xffff_fc00_0017_7200,
    0xffff_fc00_0017_7288,
];
/// FList worker busy flag and the four worker-ring state blocks identified by
/// the B1 worker. Fault-only snapshots retain the raw qwords; their individual
/// producer/consumer fields are not inferred here.
const G17P_FLIST_WORKER_BUSY_LINK: u64 = 0xffff_fc00_0017_9598;
const G17P_FLIST_WORKER_RING_LINKS: [u64; 4] = [
    0xffff_fc00_0017_a5e0,
    0xffff_fc00_0017_b5f0,
    0xffff_fc00_0017_c600,
    0xffff_fc00_0017_d610,
];
/// Ready mask (fw `0x11b5c`).
const G17P_READY_MASK_LINK_BASE: u64 = 0xffff_fc00_0017_6ec0;
/// `0x175aa0 + qid*0x28 + 0x40` (fw `0x11b3c`).
const G17P_QUEUE_FLAG_LINK_BASE: u64 = 0xffff_fc00_0017_5aa0;
/// B1's private FList registration rows. Row `id` starts at base + id*0xe8;
/// registration stores the exact FList pointer at +0, cookie at +8 and
/// context at +0x10. These are firmware DATA, never live TEXT.
const G17P_FLIST_ROW_LINK_BASE: u64 = 0xffff_fc00_0005_b778;
const G17P_FLIST_ROW_STRIDE: u64 = 0xe8;
/// Two-qword active-pool bitmap used by B1's FList worker.
const G17P_FLIST_ACTIVE_BITMAP_LINK: u64 = G17P_FLIST_ROW_LINK_BASE + 0x1_0440;
/// Tag-15's per-QID owner pointer table (`base + qid*8`).
const G17P_QID_OWNER_LINK_BASE: u64 = 0xffff_fc00_0012_59d8;
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
    pub(crate) submission_backend: identity::SubmissionBackendSelection,
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
    submission_backend: identity::T8140_G17P_SUBMISSION_BACKEND,
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
    pub(crate) sksm_queue_geometry: g17_submission::G17SksmQueueGeometry,
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
    sksm_queue_geometry: g17_submission::T8140_G17P_HAL200_SKSM_QUEUE_GEOMETRY,
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
    WrongSubmissionBackend,
    NullUatOwner,
    SplitUatOwner,
    WrongRoleTopology,
    WrongRootPlacement,
    WrongSharedClusterSize,
    WrongSksmScratchGeometry,
    WrongSksmQueueGeometry,
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
    let submission_backend = identity::require_executable_submission_backend(decoded)
        .map_err(|_| ConstructionError::WrongSubmissionBackend)?;
    if submission_backend != config.submission_backend {
        return Err(ConstructionError::WrongSubmissionBackend);
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
    if platform.sksm_scratch != T8140_G17P_PLATFORM_CONFIG.sksm_scratch {
        return Err(ConstructionError::WrongSksmScratchGeometry);
    }
    if platform.sksm_queue_geometry
        != T8140_G17P_PLATFORM_CONFIG.sksm_queue_geometry
    {
        return Err(ConstructionError::WrongSksmQueueGeometry);
    }
    platform.sksm_scratch.fifo_offsets()?;
    platform
        .sksm_queue_geometry
        .validate()
        .map_err(|_| ConstructionError::WrongSksmQueueGeometry)?;
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
    resources: KBox<g17_resources::G17ResourceOwner>,
    /// One bounded first-render archive. Allocated before initdata publication;
    /// exported by ownership transfer only after a terminal observation.
    #[cfg(CONFIG_DEV_COREDUMP)]
    trace_capture: Option<KBox<G17PTraceDump>>,
    platform: G17PPlatformConfig,
    num_clusters: u32,
    /// Cached SGX 0xe01480 bits 19:16 from the pre-ASC GPU-ID probe.
    gpc_perf_state_map: u32,
    /// Cached SGX 0xe01480 bits 3:0 from the same probe.
    gpc_perf_state_map_low: u32,
    /// Cached SGX 0xe0141c bit 0 from the same probe.
    gpc_perf_state_control: u32,
    render_storage: Option<g17_resources::G17PUserRenderStorage>,
    render_tracker: Option<G17PPartialOpeningRenderTracker>,
    render_queued_tracker: Option<G17PPartialOpeningRenderTracker>,
    render_bind: Option<G17PRenderRootOwner>,
    /// Root replaced by an in-place same-VM context update. The replacement
    /// descriptor is not a release witness until the pair carrying that update
    /// completes and its EventControl record is unlinked.
    render_context_retirement: Option<G17PRenderContextRetirement>,
    /// Number of successfully completed context-update groups on each physical
    /// render pair. Retired graphs name the next epoch as their release fence.
    render_context_epochs: [u64; 64],
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
    render_submission_ordinal: u64,
    /// Per-command host timestamp shared by the TA and 3D KSM entries. Native
    /// prepares it once in the combined render descriptor, before either
    /// stage wrapper encodes its entry.
    render_auxiliary_timestamp: u64,
    /// ID currently referenced by the submitted TA command. Native releases
    /// this from `AGXTACommandDescriptor::complete`; Linux's paired completion
    /// is the first safe semantic completion point currently exposed here.
    render_hardware_buffer_id: Option<u32>,
    /// Set immediately after the outer command pair becomes firmware-visible.
    /// A pre-publication construction failure can return its ID immediately;
    /// a post-publication failure must retain it until the CPUs stop.
    render_hardware_buffer_published: bool,
    render_hardware_buffer_ids: G17PHardwareBufferIdAllocator,
    /// AGXShared/session-wide command-buffer state sequence and the exact
    /// TA/3D pair held by each bounded live render owner. These identities do
    /// not move with the active-slot scratch fields: a parked slot may still
    /// be executing and therefore keeps both event states unavailable.
    render_command_buffer_states: G17PCommandBufferStateAllocator,
    render_command_buffer_state_pairs: [Option<u32>; 64],
    /// Whether each native TA/3D channel pair has published in this firmware
    /// session. `encodeAcceleratorRingCommand` reads this state from the
    /// selected AGXChannel, so pair 1 starts independently of pair 0.
    render_outer_channels_started: [bool; 64],
    /// Dedicated bootstrap owner pinned by B2 +0x808 for this session. Its
    /// firmware-private consumer remains unresolved; logical queue switching
    /// must never retarget that global field.
    compute_channel_control: Option<g17_resources::G17PClChannelControl>,
    /// Native free performs a firmware scheduler-context detach. Until that
    /// acknowledgement is implemented, retain referenced pages independently
    /// of UAT roots and physical-QID rebind completion, through processor stop.
    /// A created context that has never entered a render/CL binding owns its
    /// page alone and can release it normally when the queue closes.
    logical_channel_controls: KVec<Arc<g17_resources::G17PClChannelControl>>,
    logical_qos_ids: Option<Arc<kernel::sync::Mutex<u128>>>,
    /// The computed B2 page belongs to CL_2, not to an individual QID. Its
    /// counters and channel-control pointer are installed exactly once per
    /// firmware session.
    compute_b2_activation: g17_submission::G17PClB2ActivationState,
    /// The SKSM half of the render path: one KSM kick queue per render data
    /// master. Present only while `g17p_render_sksm` is armed.
    render_sksm: Option<G17PRenderSksmQueues>,
    /// Native's per-workqueue `AGXUSCPrivMemFList::updateSubmitInfo` owner.
    /// A type-13 tuple is actionable only while this retained owner exists.
    bootstrap_uma: Option<G17PBootstrapUmaLifecycle>,
    /// Captured before restaging; becomes the active lease at FList publication.
    prepared_render_flist: Option<G17PBootstrapUmaLifecycle>,
    /// Next FList lifecycle sequence for the selected render ownership domain.
    /// This field moves with the retained physical-slot graph.
    next_render_flist_sequence: u64,
    /// Next HWPB-manager generation in this firmware session. A replacement
    /// PM graph must never reuse its predecessor's Scene-owner generation.
    next_render_pm_manager_generation: u64,
    /// `render_pool_id()` of the storage the device-global parameter-buffer
    /// loader currently points at, or 0 for none.
    ///
    /// The PB page-list page and the PBDesc table live at fixed native
    /// addresses and are per-DEVICE, but each retained storage owns its own
    /// parameter-management graph. Only one of them can be installed at a time,
    /// so record which, and re-point the loader when a different owner
    /// submits.
    render_parameter_buffer_owner: u64,
    /// The manager's render methods operate on one selected state to keep the
    /// byte-critical publication code single-copy.  The other bounded slot is
    /// parked here while its independent queue pair executes.
    parked_render_slots: KVec<Option<KBox<G17PRenderSlotState>>>,
    /// Empty heap state used when moving the selected owner off the hot path.
    render_slot_scratch: Option<KBox<G17PRenderSlotState>>,
    render_channel_leases: [Option<[crate::g17_channel::Lease; 2]>; 64],
    active_render_slot: u8,
    accepted_firmware_recovery_generation: u64,
    /// Coalesced firmware recovery: host generation, record count, and the
    /// exact EventMachine stamp/QID blamed by type 4 when one was supplied.
    /// Keep the blame with the deferred recovery work so the worker can settle
    /// the affected output fence after the firmware's final clear.
    pending_primary_recovery: Option<(u64, u32, Option<u8>)>,
    /// A mandatory TVB reply whose event-ring record is already consumed.
    /// Stop both event-ring consumers until this exact encoded reply has a
    /// device-control credit; replaying the request would grow storage twice.
    pending_primary_control: Option<G17PPendingPrimaryControl>,
    /// Worker retry budget for an unpublished FList release retained in the
    /// bootstrap lifecycle while the device-control ring is full.
    pending_flist_release_worker_attempts: u16,
    /// Tag-14 AddKicks held back until the firmware recovery/power-up closes.
    pending_add_kicks: Option<[u8; g17_submission::G17P_KSM_ADD_KICKS_COMMAND_SIZE]>,
    /// Descriptor address published by the most recent compute submission.
    /// The KSM completion record echoes it at +0x30, so it identifies which
    /// submission a populated record belongs to.
    last_published_descriptor: u64,
    /// One namespace for firmware stamp/QID leases. Existing render graph
    /// reservations are entered here too; they are not a hardware partition.
    /// Published channels remain retained until verified detach or CPU stop.
    channel_ids: crate::g17_channel::ChannelTable,
    /// Host channel identities never alias a previous graph within a session.
    next_channel_identity: u64,
    /// End timestamp of the last completion already handed to a waiter. The
    /// KSM record is a single overwritten slot, so without this the record
    /// left behind by submission N is indistinguishable from submission N+1's
    /// and every later wait would be satisfied instantly by stale data.
    last_completion_end: u64,
    /// Preallocated per-QID index for one shared completion-ring observation.
    /// Kept in the heap-resident manager so event service neither allocates
    /// under the runtime mutex nor consumes several KiB of kernel stack.
    compute_completion_batch: g17_resources::G17PComputeCompletionBatch,
    /// Set once a submission has zeroed the queue's completion stamp, so a
    /// retire knows the word is meaningful. Cleared only by rebuilding the
    /// manager.
    compute_stamp_armed: bool,
    /// How many retires have actually had to wait for the completion stamp.
    /// Counted rather than logged per submission, because a per-submission
    /// console write costs about a millisecond on a dockchannel console and
    /// would pad the very timing this gate exists to measure.
    compute_stamp_waits: AtomicU32,
    /// How many retires the completion stamp never arrived for. Hardware says
    /// this is a real minority case (19 of 300), not a modelling error, so the
    /// warning is rate-limited rather than removed: a per-submission console
    /// write costs about a millisecond and would itself distort the timing.
    compute_stamp_misses: AtomicU32,
}

#[cfg(not(test))]
#[derive(Default)]
struct G17PRenderSlotState {
    #[cfg(CONFIG_DEV_COREDUMP)]
    trace_capture: Option<KBox<G17PTraceDump>>,
    // Rust drops fields in declaration order. Retract both roots before any
    // storage/SKSM backing reachable through them can be released.
    bind: Option<G17PRenderRootOwner>,
    context_retirement: Option<G17PRenderContextRetirement>,
    context_aliases: Option<mmu::T8140NativeContextAliases>,
    retired_handoff_graph: Option<G17PRetiredRenderGraph>,
    storage: Option<g17_resources::G17PUserRenderStorage>,
    sksm: Option<G17PRenderSksmQueues>,
    tracker: Option<G17PPartialOpeningRenderTracker>,
    queued_tracker: Option<G17PPartialOpeningRenderTracker>,
    submission_ordinal: u64,
    auxiliary_timestamp: u64,
    hardware_buffer_id: Option<u32>,
    hardware_buffer_published: bool,
    bootstrap_uma: Option<G17PBootstrapUmaLifecycle>,
    prepared_render_flist: Option<G17PBootstrapUmaLifecycle>,
    next_render_flist_sequence: u64,
}

#[cfg(not(test))]
impl G17PRenderSlotState {
    fn new() -> Result<KBox<Self>> {
        KBox::init(try_init!(Self {
            #[cfg(CONFIG_DEV_COREDUMP)]
            trace_capture: None,
            bind: None,
            context_retirement: None,
            context_aliases: None,
            retired_handoff_graph: None,
            storage: None,
            sksm: None,
            tracker: None,
            queued_tracker: None,
            submission_ordinal: 0,
            auxiliary_timestamp: 0,
            hardware_buffer_id: None,
            hardware_buffer_published: false,
            bootstrap_uma: None,
            prepared_render_flist: None,
            next_render_flist_sequence: G17P_BOOTSTRAP_UMA_SEQUENCE,
        }), GFP_KERNEL)
    }
}

#[cfg(not(test))]
fn g17p_empty_render_slots() -> Result<KVec<Option<KBox<G17PRenderSlotState>>>> {
    let mut slots = KVec::with_capacity(g17_resources::G17PRenderQueuePair::SLOT_COUNT as usize, GFP_KERNEL)?;
    for _ in 0..g17_resources::G17PRenderQueuePair::SLOT_COUNT {
        slots.push(None, GFP_KERNEL)?;
    }
    Ok(slots)
}

#[cfg(not(test))]
struct G17PRetiredRenderGraph {
    // Rust drops fields in declaration order. Retract roots and their aliases
    // before releasing any storage reachable through them.
    bind: G17PRenderRootOwner,
    context_aliases: Option<mmu::T8140NativeContextAliases>,
    storage: g17_resources::G17PUserRenderStorage,
    /// A later group on this physical pair must publish and complete a context
    /// update before this graph's context, mappings, and HWPB ID can be reused.
    retire_after_context_epoch: u64,
}

#[cfg(not(test))]
struct G17PRenderContextRetirement {
    /// One Arc here keeps the replaced TTBAT root installed. The storage's old
    /// command-context Arc may be replaced immediately because this owner
    /// remains until the superseding group completes.
    bind: G17PRenderRootOwner,
    _context_aliases: Option<mmu::T8140NativeContextAliases>,
    retire_after_context_epoch: u64,
}

#[cfg(not(test))]
enum G17PRenderRootOwner {
    Bootstrap(mmu::VmBind),
    Job {
        context: Arc<crate::g17_resources::G17PLogicalQueueContext>,
    },
}

#[cfg(not(test))]
impl G17PRenderRootOwner {
    fn matches(&self, vm: &mmu::Vm) -> bool {
        match self {
            Self::Bootstrap(bind) => bind.matches(vm),
            Self::Job { context } => context.matches(vm),
        }
    }

    fn root(&self) -> u64 {
        match self {
            Self::Bootstrap(bind) => bind.root(),
            Self::Job { context } => context.root(),
        }
    }

    fn vm_id(&self) -> u64 {
        match self {
            Self::Bootstrap(bind) => bind.vm_id(),
            Self::Job { context } => context.vm_id(),
        }
    }

    fn vm(&self) -> &mmu::Vm {
        match self {
            Self::Bootstrap(bind) => bind.vm(),
            Self::Job { context } => context.vm(),
        }
    }

    fn bootstrap(&self) -> Option<&mmu::VmBind> {
        match self { Self::Bootstrap(bind) => Some(bind), Self::Job { .. } => None }
    }

    /// TTBAT application-GART context which owns this graph's VM root.
    ///
    /// Context 1 separately retains the global FList/operand closure. Command
    /// descriptors and MCache ranges name this queue-owned context.
    fn root_slot(&self) -> Result<u8> {
        let slot = match self {
            Self::Bootstrap(bind) => bind.slot(),
            Self::Job { context } => context.context_id(),
        };
        u8::try_from(slot).map_err(|_| EIO)
    }
}

#[cfg(not(test))]
impl G17PRenderSlotState {
    fn scheduler_active(&self) -> bool {
        self.storage.is_some()
            || self.tracker.is_some()
            || self.queued_tracker.is_some()
            || self.bind.is_some()
            || self.context_retirement.is_some()
            || self.retired_handoff_graph.is_some()
            || self.submission_ordinal != 0
    }
}

/// A preallocated evidence buffer, not a live mapping exposed to readers.
/// `used` remains zero until the archive is sealed; the only export path takes
/// this owner out of the manager so later submits cannot overwrite evidence.
#[cfg(all(not(test), CONFIG_DEV_COREDUMP))]
pub(crate) struct G17PTraceDump {
    arena: KVVec<u8>,
    used: usize,
    phase_sequence: u64,
    prepared: bool,
    job_stamp: u64,
}

#[cfg(all(not(test), CONFIG_DEV_COREDUMP))]
impl G17PTraceDump {
    fn new(resources: &mut g17_resources::G17ResourceOwner) -> Result<KBox<Self>> {
        let mut arena = KVVec::from_elem(0, trace::DEFAULT_CAPACITY, GFP_KERNEL)?;
        let mut archive = TraceArchive::new(&mut arena).map_err(|_| EINVAL)?;
        if *module_parameters::g17p_binary_capture.value() & 2 != 0 {
            // HAZARD: mapped firmware TEXT reads can still panic hardware.
            // Mapping/attribute checks are not a safety proof. Leave this
            // explicit diagnostic off; prefer existing offline captures.
            // Inspect only the mapped code window preceding the established
            // GFX DATA link base. The read-only walker rejects unmapped pages
            // and non-memory attributes; never guess its physical address.
            // This boot-time diagnostic may allocate/map, never retirement.
            let mut meta = trace::Meta::new(
                trace::Kind::FirmwareCodeWindow, trace::Phase::BootConstructed, 1,
            );
            meta.context = 0;
            meta.root = trace::ROOT_CONTEXT_HIGH;
            meta.dva = 0xffff_fc00_0000_0000;
            meta.expected_len = 0x4_c000;
            let reader = resources.uat().firmware_context_table_reader();
            let root = reader.context0_upper_root();
            archive.capture(meta, &mut || Monotonic::ktime_get() as u64, |output| {
                let root = root.ok_or(trace::Omission::Unavailable)?;
                let bytes = root.copy_firmware_range(0, output.len())
                    .map_err(|_| trace::Omission::ReadError)?;
                output.copy_from_slice(&bytes);
                Ok(())
            }).map_err(|_| EINVAL)?;
        }
        if resources.capture_owned_trace(
            &mut archive, trace::Phase::BootConstructed, 1,
            &mut || Monotonic::ktime_get() as u64,
        ).is_err() {
            archive.omit(
                trace::Meta::new(trace::Kind::CaptureBoundary, trace::Phase::BootConstructed, 1),
                trace::Omission::ReadError,
            ).map_err(|_| EINVAL)?;
        }
        Ok(KBox::new(Self {
            arena, used: 0, phase_sequence: 1, prepared: false, job_stamp: 0,
        }, GFP_KERNEL)?)
    }

    pub(crate) fn len(&self) -> usize { self.used }
}

#[cfg(all(not(test), CONFIG_DEV_COREDUMP))]
impl kernel::devcoredump::DevCoreDump for G17PTraceDump {
    fn read(&self, output: &mut [u8], offset: usize) -> Result<usize> {
        // No caller is allowed to expose an in-progress archive.
        if self.used == 0 { return Err(EINVAL); }
        trace::read_sealed_prefix(&self.arena[..self.used], output, offset).map_err(|_| EINVAL)
    }
}

/// One record a steady-state submission publishes into the item ring.
enum G17PRepeatRecord<'a> {
    Pointer(u64),
    AddKicks(&'a [u8; g17_submission::G17P_KSM_ADD_KICKS_COMMAND_SIZE]),
    EntrySignal(&'a [u8; g17_submission::G17P_COMPUTE_OPTIONAL_EVENT_SIZE]),
}

/// Records one steady-state submission appends to the queue's item ring:
/// the tag-3 command pointer, tag 15, tag 14, tag 16.
const G17P_COMPUTE_REPEAT_RING_RECORDS: u32 = 4;
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
enum G17PSksmQueueHardwareState {
    PreparedMemory,
    HardwareRegistered,
}

/// One render KSM kick queue -- the SKSM half of a render data master.
///
/// The two render queues are QID 0 (TA, data master 0) and QID 1 (3D, data
/// master 1), a consecutive TA-first pair exactly as the twelve captured macOS
/// render passes use. Each one's entry ring is the fixed 0x20000 bootstrap
/// context peer whose low VA the render tag-15 ConfigUpdate already publishes
/// at +0x08.
#[cfg(not(test))]
struct G17PRenderSksmQueue {
    queue_id: u8,
    /// True for the TA/tiling queue, false for the 3D/fragment queue. Selects
    /// which bootstrap context peer backs the entry ring.
    tiling: bool,
    entry_low: u64,
    entry_high: u64,
    config: g17_submission::PreparedG17PSksmQueueConfig,
    producer: g17_submission::G17PClKickProducerState,
    /// Set only after the tag-15 install and complete outer group are visible
    /// to firmware. It never represents a host MMIO write.
    registered: bool,
}

/// The render pair, TA first. Held next to `render_storage` and released with
/// it, since the entry rings live in resources the storage object aliases.
#[cfg(not(test))]
struct G17PRenderSksmQueues {
    scratch: G17SksmScratchGeometry,
    uat_owner: u64,
    tiling: G17PRenderSksmQueue,
    fragment: G17PRenderSksmQueue,
    /// Present only between retired client graphs. KSMFE retains these ring
    /// addresses for the lifetime of the QID pair, so a handoff may replace
    /// their client-low aliases but never their backing/global aliases.
    backing: Option<g17_resources::G17PRenderSksmBacking>,
}

/// How many times the device-global parameter-buffer loader has been handed
/// from one client's retained storage to another's. Rate-limits its own log.
static G17P_PARAMETER_BUFFER_RETARGETS: AtomicU32 = AtomicU32::new(0);

/// Session-wide counters for the four decisive state-machine events.
///
/// Printed on EVERY `G17PMARK` line. The kernel log ring wraps easily on a
/// verbose boot -- especially with `g17p_fw_trace=1` -- and the two rarest
/// markers (`tag15-install`, fired once at the very first submit of a boot)
/// are exactly the ones that get evicted first. Carrying the counts on every
/// marker line means any single surviving line reconstructs the whole state
/// machine: how many QIDs were installed, how many zero-duration retires were
/// caught, how many sessions were rebuilt, how many handoffs retained.
/// One `context/qos` control sample per boot, taken from a render that
/// completed, so the wedge sample has something to be compared against.
static RENDER_CONTEXT_WITNESS_LOGGED: AtomicBool = AtomicBool::new(false);
/// Reason label of the one healthy control sample per boot; that sample is
/// logged at info level, a wedge sample at error level.
const HEALTHY_RENDER_WITNESS: &str = "healthy-paired-completion";
pub(crate) static G17P_MARK_TAG15_INSTALLS: AtomicU32 = AtomicU32::new(0);
pub(crate) static G17P_MARK_ZERO_DURATION: AtomicU32 = AtomicU32::new(0);
pub(crate) static G17P_MARK_GATE_REBUILDS: AtomicU32 = AtomicU32::new(0);
pub(crate) static G17P_MARK_RETAINED_HANDOFFS: AtomicU32 = AtomicU32::new(0);

/// Compact `i/z/r/h` census appended to every marker line.
pub(crate) fn g17p_mark_counts() -> (u32, u32, u32, u32) {
    (
        G17P_MARK_TAG15_INSTALLS.load(Ordering::Relaxed),
        G17P_MARK_ZERO_DURATION.load(Ordering::Relaxed),
        G17P_MARK_GATE_REBUILDS.load(Ordering::Relaxed),
        G17P_MARK_RETAINED_HANDOFFS.load(Ordering::Relaxed),
    )
}

/// First compute QID of a boot. The captured native CL2 queue uses QID 4, and
/// every proven run of this driver has used it.
pub(crate) const G17P_COMPUTE_QUEUE_ID_BASE: u8 = 4;
/// Last QID the allocator may hand out. `KICK_QID_MASK` is 127 and the
/// firmware's KSM restore set has 128 slots. IDs 0..3 are the two bounded
/// render pairs, 4 is the first compute queue, and additional compute graphs
/// consume 5..127 exactly once. Exhaustion returns ENOSPC rather than
/// reprogramming a queue still installed in the firmware.
pub(crate) const G17P_COMPUTE_QUEUE_ID_LAST: u8 = 127;
const _: () = assert!(G17P_COMPUTE_QUEUE_ID_BASE > 1);
const _: () = assert!(G17P_COMPUTE_QUEUE_ID_LAST <= 127);

/// The native bootstrap graph owns QIDs 0/1. Every logical engine channel
/// subsequently leases its ID from the same pool, without engine partitions.
fn g17p_reserve_bootstrap_channels(table: &mut crate::g17_channel::ChannelTable) -> bool {
    use crate::g17_channel::{ChannelId, ChannelTable, Engine};
    for qid in 0..2 {
        let engine = if qid & 1 == 0 { Engine::Tiling } else { Engine::Fragment };
        let Some(lease) = table.acquire(ChannelId(u64::from(qid) + 1), engine, Some(qid)) else {
            return false;
        };
        // Retain even an unsubmitted bootstrap reservation until processor stop.
        if !table.publish(lease) {
            return false;
        }
    }
    true
}

fn g17p_initial_channel_table() -> crate::g17_channel::ChannelTable {
    let mut table = crate::g17_channel::ChannelTable::new();
    // The empty table has 128 slots and these two identities are unique.
    assert!(g17p_reserve_bootstrap_channels(&mut table));
    table
}

/// A retained compute queue whose backing graph is prepared during startup.
/// Native processComputeSetup marks the queue dirty; the first direct
/// submitBuffer configures QID 4 after tag 15, once startup is complete.
#[cfg(not(test))]
pub(crate) struct G17PSksmQueue {
    queue_id: u8,
    channel_lease: crate::g17_channel::Lease,
    /// Bootstrap QoS identity, used only before a logical owner is attached.
    hardware_buffer_id: u8,
    scratch: G17SksmScratchGeometry,
    uat_owner: u64,
    configure_pair: g17_submission::G17SksmOrderedWritePair,
    enable_pair: g17_submission::G17SksmOrderedWritePair,
    disable_pair: g17_submission::G17SksmOrderedWritePair,
    hardware_state: G17PSksmQueueHardwareState,
    storage: g17_resources::G17PClRuntimeStorage,
    /// Current logical owner's scheduler page; bootstrap queues initially
    /// reference the dedicated session page. Neither belongs to this QID.
    channel_control_gpu_va: u64,
    logical_channel_control: Option<Arc<g17_resources::G17PClChannelControl>>,
    logical_qos: Option<Arc<g17_resources::G17PLogicalQosLease>>,
    compute: g17_resources::G17PComputeDescriptorStorage,
    producer: g17_submission::G17PClKickProducerState,
    direct_lifecycle: g17_submission::G17PDirectClLifecycleState,
    /// Updated only after the complete ConfigUpdate group is published.
    /// Preparing/reverting a logical-owner handoff never changes this cache.
    published_config: Option<g17_compute::ComputeConfigIdentity>,
    /// Native tracks the first outer publication on the selected AGXChannel.
    /// This state belongs to the logical command queue, not the session-wide
    /// CL_2 transport ring.
    outer_channel_started: bool,
    /// Native configureHardware compares the accelerator's retained QID stamp
    /// against EventMachine::getLastSubmittedStamp before touching MMIO.
    configure_guard_accelerator_stamp: u32,
    configure_guard_event_stamp: u32,
    submission_ordinal: u32,
    independent_context: bool,
    completion_descriptor: u64,
    completion_last_end: u64,
    completion_stamp_armed: bool,
    /// Diagnostic-only copy taken before the command pointer is published.
    /// Default-off so production submission has no extra reads or storage
    /// traffic. The paired FList array covers `+0x08` and `+0x48`.
    publication_resource_snapshot: Option<(
        g17_resources::G17PComputeSubmissionSnapshot,
        [u32; 28],
    )>,
}

/// Immutable identity of one firmware-visible compute publication.
///
/// The retained graph may change logical owners only after every firmware
/// cursor owned by this exact publication has reached its terminal value.
/// Keeping this separate from the queue's mutable producer state prevents a
/// later owner from accidentally satisfying an older completion check.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) struct G17PComputeRetirementTicket {
    queue_id: u8,
    descriptor: u64,
    submitted_target: u32,
    item_producer: u32,
    item_count: u32,
}

impl G17PSksmQueue {
    pub(crate) fn bind_logical_channel_control(
        &mut self, context: &g17_resources::G17PLogicalQueueContext,
    ) -> Result {
        // Apple selects the channel-control page from the command queue's
        // reclaimable resources. Keep that logical ownership even though the
        // queues share the physical CL_2 transport.
        let address = context.channel_control_gpu_va();
        self.compute.retarget_channel_control(address, context.owner_pid)?;
        self.channel_control_gpu_va = address;
        self.logical_channel_control = Some(context.channel_control.clone());
        self.logical_qos = Some(context.qos.clone());
        self.producer.qos.queue_byte_32 = context.qos.id();
        Ok(())
    }

    pub(crate) fn qos_hardware_buffer_id(&self) -> u8 {
        self.logical_qos.as_ref().map_or(self.hardware_buffer_id, |qos| qos.id())
    }

    fn flist_generation(&self) -> u32 {
        self.logical_channel_control.as_ref().map_or(0x3f, |owner| owner.flist_generation())
    }

    /// The hardware compute QID this graph is published as.
    ///
    /// Needed outside this module so a client handoff can name the QID whose
    /// firmware-side progress record it has to restore.
    pub(crate) fn queue_id(&self) -> u8 {
        self.queue_id
    }
    pub(crate) fn requires_first_install(&self) -> bool {
        self.hardware_state == G17PSksmQueueHardwareState::PreparedMemory
            && self.submission_ordinal == 0
    }

    /// `(submission ordinal, next kick stamp, item-ring producer)`.
    ///
    /// Sampled when the graph is handed from one DRM client to the next, so a
    /// log line proves the retained queue really did carry its stamp sequence
    /// and ring cursor across the handoff instead of restarting them. Every
    /// field is a plain read of an object the driver allocated -- no sgx MMIO,
    /// so it is safe with the GPU cores gated.
    pub(crate) fn handoff_snapshot(&mut self) -> (u32, u64, u32) {
        let ring = self.compute.compute_ring_producer().unwrap_or(u32::MAX);
        (self.submission_ordinal, self.producer.current_timestamp, ring)
    }
}

/// Command-owned fields used to build one compute descriptor and CL entry.
/// The queue supplies descriptor storage, sequencing, preemption storage, and
/// the retained support graph.
#[cfg(not(test))]
pub(crate) struct G17PComputeSubmissionContext<'a> {
    pub(crate) context_id: u32,
    pub(crate) dispatch_identity: u64,
    pub(crate) timestamps: g17_uapi::ComputeTimestampAddresses,
    pub(crate) descriptor_flag_4c: bool,
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
    _flist: mmu::KernelMapping,
    _sksm_entries: mmu::KernelMapping,
    _vm_bind: Option<mmu::VmBind>,
    _execution_context: Option<Arc<crate::g17_resources::G17PLogicalQueueContext>>,
}

#[cfg(not(test))]
impl G17PComputeUserBinding {
    pub(crate) fn retarget_logical_context(
        &mut self, context: Arc<g17_resources::G17PLogicalQueueContext>,
    ) -> Result {
        if !self.matches(context.vm()) || !context.is_current() { return Err(EFAULT); }
        self._execution_context = Some(context);
        Ok(())
    }

    /// Whether this retained binding already covers `vm`.
    ///
    /// A repeat submission reuses the binding rather than mapping the same
    /// fixed user VAs a second time, which would fail; a different VM has to
    /// go through a fresh bind.
    pub(crate) fn matches(&self, vm: &mmu::Vm) -> bool {
        self._vm_bind.as_ref().is_some_and(|bind| bind.matches(vm))
            || self._execution_context.as_ref().is_some_and(|context| context.matches(vm))
    }

    /// The UAT binding this client's submissions run against.
    ///
    /// Needed so every kick -- not just the one that created this binding --
    /// can re-prove that hardware contexts 2/3 still name this VM's root.
    /// The hardware context this client's compute work actually runs in.
    ///
    /// The kick already declares it at `sgx+0x10428` as
    /// `(context_id << 8) | work_ordinal`; this exposes the same value so the
    /// tag-15 ConfigUpdate can declare it too instead of the constant 2.
    pub(crate) fn declared_context_id(&self) -> Option<u16> {
        self._execution_context
            .as_ref()
            .and_then(|context| u16::try_from(context.context_id()).ok())
    }

    pub(crate) fn vm_bind(&self) -> &mmu::VmBind {
        self._vm_bind.as_ref().expect("legacy binding required")
    }
}

/// An outgoing client's compute binding, kept alive after its submission was
/// abandoned, with only the UAT slot given back.
///
/// A submission that was published and never seen to retire may still be held
/// by the firmware, and that kick names this client's page tables and the GEM
/// objects mapped into them. Dropping the binding outright would unmap and then
/// free exactly that memory. Every `mmu::KernelMapping` here owns an
/// `Arc<VmInner>`, so retaining them retains the page tables, the gpuvm and its
/// GEM references -- nothing the firmware could still dereference is released.
///
/// The `mmu::VmBind` is deliberately NOT retained. Slot release is refcounted
/// on the `VmBind` alone (`active_users` reaching zero clears `binding.binding`),
/// independently of the `VmInner` lifetime, so dropping it hands UAT slot 1
/// straight back to the next client while the memory stays parked.
///
/// Released only once both firmware processors have stopped, in
/// `release_runtime_objects` -- the same discipline `retired_queues` uses.
#[cfg(not(test))]
pub(crate) struct G17PQuarantinedComputeBinding {
    _mappings: g17_resources::G17PComputeUserMappings,
    _flist: mmu::KernelMapping,
    _sksm_entries: mmu::KernelMapping,
    _execution_context: Option<Arc<crate::g17_resources::G17PLogicalQueueContext>>,
}

#[cfg(not(test))]
impl G17PComputeUserBinding {
    /// Give the UAT slot back and park everything else.
    pub(crate) fn quarantine(self) -> G17PQuarantinedComputeBinding {
        let Self {
            mappings,
            _flist,
            _sksm_entries,
            _vm_bind,
            _execution_context,
        } = self;
        // Explicit and load-bearing: this is what frees UAT slot 1.
        drop(_vm_bind);
        G17PQuarantinedComputeBinding {
            _mappings: mappings,
            _flist,
            _sksm_entries,
            _execution_context,
        }
    }
}

#[cfg(not(test))]
pub(crate) struct G17PComputeTimeoutSnapshot {
    pub(crate) graph: g17_resources::G17PComputeGraphSnapshot,
    pub(crate) channel: g17_resources::G17PComputeChannelSnapshot,
    pub(crate) channel_scan:
        [g17_resources::G17PChannelScanSnapshot; g17_resources::G17P_DATA_MASTER_CHANNEL_COUNT],
    pub(crate) channel_control_prefix: [u8; 0x38],
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
    ordinal: u64,
    hardware_buffer_id: u32,
    command_buffer_state_word: u32,
    // Only translated renders publish the paired QoS rows. Keep retirement
    // with this command generation so a later cleanup failure cannot make a
    // retry subtract the same QoS references twice.
    qos_pending: bool,
    pool_a_record_index: u8,
    pool_a_submitted: u32,
    context_update: bool,
    tiling_previous: g17_completion::QueueIndices,
    fragment_previous: g17_completion::QueueIndices,
    tiling_prefix: u32,
    fragment_prefix: u32,
}

#[cfg(not(test))]

/// DIAGNOSTIC CONTROL, compute path only. When 0, the compute submit publishes
/// everything it normally does -- tag 3/15/14/16, the classic item-ring CL_2
/// command, the doorbell, the producer advance -- but never writes the SKSM
/// kick entry body. It answers one question about the CONTROL, not the failing
/// path: is compute actually scheduled through the SKSM entry ring, or through
/// the classic item ring with its SKSM entry vestigial?
///
/// This matters because "compute completes with `g17p_sksm_mmio=0`" was used to
/// dismiss the suppressed SKSM queue-enable as a render blocker. That argument
/// only carries if compute's completion depends on SKSM scheduling. If compute
/// still passes 64/64 with this at 0, it does not, and the enable becomes a live
/// candidate again -- render has no classic path to fall back on.
pub(crate) fn g17p_compute_sksm_entry_enabled() -> bool {
    crate::g17_config::COMPUTE_SKSM_ENTRY != 0
}


/// Result of one render completion poll.
#[derive(Debug)]
pub(crate) enum G17PRenderPollOutcome {
    /// Both halves retired; the render is done.
    Complete(g17_completion::PairedRenderCompletion),
    /// A completed job list was quiesced; re-poll immediately, do not sleep.
    Retry,
    /// Nothing yet; sleep before polling again.
    Pending,
}

impl G17PManagerConstruction {
    fn next_render_context_epoch(&self, slot: u8) -> Result<u64> {
        self.render_context_epochs
            .get(usize::from(slot))
            .ok_or(EINVAL)?
            .checked_add(1)
            .ok_or(EOVERFLOW)
    }

    fn swap_active_render_slot_state(&mut self, state: &mut G17PRenderSlotState) {
        // Swap fields in place: materializing the whole storage value puts
        // multiple render graphs on the small kernel stack.
        #[cfg(CONFIG_DEV_COREDUMP)]
        {
            // Before the first render, the boot archive belongs to the
            // session, not the unused bootstrap slot. Attach it to the first
            // real graph. Once prepared, it must travel only with that graph.
            let unattached = self.trace_capture.as_ref()
                .is_some_and(|capture| !capture.prepared);
            if !unattached || state.trace_capture.is_some() {
                core::mem::swap(&mut self.trace_capture, &mut state.trace_capture);
            }
        }
        core::mem::swap(&mut self.render_storage, &mut state.storage);
        core::mem::swap(&mut self.render_tracker, &mut state.tracker);
        core::mem::swap(&mut self.render_queued_tracker, &mut state.queued_tracker);
        core::mem::swap(&mut self.render_bind, &mut state.bind);
        core::mem::swap(&mut self.render_context_retirement, &mut state.context_retirement);
        core::mem::swap(&mut self.render_context_aliases, &mut state.context_aliases);
        core::mem::swap(&mut self.retired_render_handoff_graph, &mut state.retired_handoff_graph);
        core::mem::swap(&mut self.render_submission_ordinal, &mut state.submission_ordinal);
        core::mem::swap(&mut self.render_auxiliary_timestamp, &mut state.auxiliary_timestamp);
        core::mem::swap(&mut self.render_hardware_buffer_id, &mut state.hardware_buffer_id);
        core::mem::swap(&mut self.render_hardware_buffer_published, &mut state.hardware_buffer_published);
        core::mem::swap(&mut self.render_sksm, &mut state.sksm);
        core::mem::swap(&mut self.bootstrap_uma, &mut state.bootstrap_uma);
        core::mem::swap(&mut self.prepared_render_flist, &mut state.prepared_render_flist);
        core::mem::swap(&mut self.next_render_flist_sequence, &mut state.next_render_flist_sequence);
    }

    fn take_active_render_slot_state(&mut self) -> KBox<G17PRenderSlotState> {
        // Every caller restores a state synchronously before returning. The
        // runtime mutex excludes another take while this scratch box is held.
        let mut state = self.render_slot_scratch.take().expect("render slot scratch is owned");
        self.swap_active_render_slot_state(&mut state);
        state
    }

    fn install_active_render_slot_state(&mut self, mut state: KBox<G17PRenderSlotState>) {
        self.swap_active_render_slot_state(&mut state);
        self.render_slot_scratch = Some(state);
    }

    /// Select a retained logical render owner without altering any
    /// firmware-visible bytes.  All state reachable by a submitted TA/3D pair
    /// moves together; the global outer rings and HardwareBufferID allocator
    /// remain shared and are still published under the runtime mutex.
    pub(crate) fn active_render_slot(&self) -> u8 {
        self.active_render_slot
    }

    pub(crate) fn select_render_slot(&mut self, slot: u8) -> Result {
        if slot >= g17_resources::G17PRenderQueuePair::SLOT_COUNT {
            return Err(EINVAL);
        }
        if self.active_render_slot == slot {
            return Ok(());
        }
        let current_slot = self.active_render_slot as usize;
        if self.parked_render_slots[current_slot].is_some() {
            return Err(EIO);
        }
        // Allocate only when this logical owner is first selected, and finish
        // the fallible work before moving either live state. Repeated switches
        // exchange pointers and fields without allocating or freeing graphs.
        if self.parked_render_slots[slot as usize].is_none() {
            self.parked_render_slots[slot as usize] = Some(G17PRenderSlotState::new()?);
        }
        let incoming = self.parked_render_slots[slot as usize].take().ok_or(EIO)?;
        let current = self.take_active_render_slot_state();
        self.parked_render_slots[current_slot] = Some(current);
        self.install_active_render_slot_state(incoming);
        self.active_render_slot = slot;
        Ok(())
    }

    /// Lazily create two stable logical engine channels. The registry's slot
    /// belongs to one logical owner until processor stop; it is not a victim
    /// chosen for reassignment when another client becomes runnable.
    pub(crate) fn render_queue_pair(&mut self, slot: u8) -> Result<g17_resources::G17PRenderQueuePair> {
        use crate::g17_channel::{ChannelId, Engine};
        if slot == 0 { return g17_resources::G17PRenderQueuePair::for_slot(0).ok_or(EINVAL); }
        let index = usize::from(slot);
        if index >= self.render_channel_leases.len() { return Err(EINVAL); }
        if self.render_channel_leases[index].is_none() {
            let identity = self.next_channel_identity;
            let next = identity.checked_add(2).ok_or(EOVERFLOW)?;
            let ta = self.channel_ids.acquire(ChannelId(identity), Engine::Tiling, None).ok_or(ENOSPC)?;
            self.next_channel_identity = next;
            let fragment = match self.channel_ids.acquire(ChannelId(identity + 1), Engine::Fragment, None) {
                Some(lease) => lease,
                None => {
                    if !self.channel_ids.cancel_unpublished(ta) { return Err(EIO); }
                    return Err(ENOSPC);
                }
            };
            self.render_channel_leases[index] = Some([ta, fragment]);
        }
        let leases = self.render_channel_leases[index].ok_or(EIO)?;
        if !leases.iter().all(|lease| self.channel_ids.matches(*lease)) { return Err(EIO); }
        g17_resources::G17PRenderQueuePair::with_queue_ids(
            slot, u16::from(leases[0].qid), u16::from(leases[1].qid),
        ).ok_or(EINVAL)
    }

    /// Route recovery by the leased EventMachine ID, never by slot arithmetic.
    pub(crate) fn render_slot_for_qid(&self, qid: u8) -> Option<u8> {
        if qid < 2 { return Some(0); }
        self.render_channel_leases.iter().enumerate().find_map(|(slot, leases)| {
            leases.as_ref().filter(|leases| leases.iter().any(|lease|
                lease.qid == qid && self.channel_ids.matches(*lease)
            )).map(|_| slot as u8)
        })
    }

    fn allocate_active_render_command_buffer_state(&mut self) -> Result<u32> {
        let slot = self.active_render_slot as usize;
        if self.render_command_buffer_state_pairs[slot].is_some() {
            return Err(EBUSY);
        }
        let word = self
            .render_command_buffer_states
            .allocate_one()
            .ok_or(EBUSY)?;
        self.render_command_buffer_state_pairs[slot] = Some(word);
        Ok(word)
    }

    fn release_active_render_command_buffer_states(&mut self) -> Result {
        let slot = self.active_render_slot as usize;
        let word = self.render_command_buffer_state_pairs[slot]
            .take()
            .ok_or(EIO)?;
        if !self.render_command_buffer_states.release_one(word) {
            self.render_command_buffer_state_pairs[slot] = Some(word);
            return Err(EIO);
        }
        Ok(())
    }

    fn take_active_render_command_buffer_state(&mut self) -> Result<u32> {
        self.render_command_buffer_state_pairs[self.active_render_slot as usize]
            .take()
            .ok_or(EIO)
    }

    fn release_render_command_buffer_state(&mut self, word: u32) -> Result {
        if !self.render_command_buffer_states.release_one(word) {
            return Err(EIO);
        }
        Ok(())
    }

    fn parked_render_scheduler_active(&self) -> bool {
        self.parked_render_slots
            .iter()
            .flatten()
            .any(|state| state.scheduler_active())
    }

    pub(crate) fn new(
        dev: &AsahiDevice,
        soc: &'static agx3::SocConfig,
        platform: G17PPlatformConfig,
        num_clusters: u32,
        gpc_perf_state_map: u32,
        gpc_perf_state_map_low: u32,
        gpc_perf_state_control: u32,
    ) -> Result<KBox<Self>> {
        let uat = KBox::new(mmu::Uat::new_t8140(dev, true)?, GFP_KERNEL)?;
        Self::new_with_uat(
            dev,
            soc,
            platform,
            num_clusters,
            gpc_perf_state_map,
            gpc_perf_state_map_low,
            gpc_perf_state_control,
            uat,
        )
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
        gpc_perf_state_map: u32,
        gpc_perf_state_map_low: u32,
        gpc_perf_state_control: u32,
        uat: KBox<mmu::Uat>,
    ) -> Result<KBox<Self>> {
        if num_clusters == 0
            || gpc_perf_state_map > 0xf
            || gpc_perf_state_map_low > 0xf
            || gpc_perf_state_control > 1
        {
            return Err(EINVAL);
        }
        validate_target(target_observation(soc)).map_err(|_| EINVAL)?;
        validate_platform(platform).map_err(|_| EINVAL)?;

        let mut resources = g17_resources::G17ResourceOwner::new_with_uat(dev, uat)?;
        g17_resources::validate_handoff(resources.handoff()).map_err(|_| EINVAL)?;
        validate_resources(resource_observation(resources.handoff(), resources.uat()))
            .map_err(|_| EINVAL)?;
        // One live storage can own each native HardwareBufferID. Reserve the
        // entire bounded quarantine up front so a handoff never has to choose
        // between dropping a firmware-visible graph and allocating memory.
        let retired_render_handoff_graphs =
            KVec::with_capacity(G17P_TA_HARDWARE_BUFFER_ID_COUNT, GFP_KERNEL)?;
        let retired_parameter_managers =
            KVec::with_capacity(G17P_TA_HARDWARE_BUFFER_ID_COUNT, GFP_KERNEL)?;
        #[cfg(CONFIG_DEV_COREDUMP)]
        let trace_capture = if *module_parameters::g17p_binary_capture.value() != 0 {
            match G17PTraceDump::new(&mut resources) {
                Ok(capture) => Some(capture),
                Err(error) => {
                    // Evidence collection must not turn memory pressure into a
                    // GPU execution failure. This absence is explicit in the log.
                    dev_warn!(dev.as_ref(), "G17P binary capture unavailable: {:?}\n", error);
                    None
                }
            }
        } else {
            None
        };

        // Keep the complete manager value off the probe task's stack.  The
        // two-slot owner contains three render-state images (active plus two
        // parked slots); constructing `Self` by value made rustc reserve
        // 0x3a40 bytes in this function, on top of boot_initial_session's
        // 0x890-byte caller frame.  That exceeds arm64's 16 KiB kernel stack
        // before RTKit/initdata can return from module probe.  An in-place
        // initializer writes every field directly into the final allocation.
        let mut manager = KBox::init(
            try_init!(Self {
                resources,
                #[cfg(CONFIG_DEV_COREDUMP)]
                trace_capture,
                platform,
                num_clusters,
                gpc_perf_state_map,
                gpc_perf_state_map_low,
                gpc_perf_state_control,
                render_storage: None,
                render_tracker: None,
                render_queued_tracker: None,
                render_bind: None,
                render_context_retirement: None,
                render_context_epochs: [0; 64],
                render_context_aliases: None,
                retired_render_handoff_graph: None,
                retired_render_handoff_graphs,
                retired_parameter_managers,
                released_render_contexts: 0,
                render_submission_ordinal: 0,
                render_auxiliary_timestamp: 0,
                render_hardware_buffer_id: None,
                render_hardware_buffer_published: false,
                render_hardware_buffer_ids: G17PHardwareBufferIdAllocator::new(),
                render_command_buffer_states: G17PCommandBufferStateAllocator::new(),
                render_command_buffer_state_pairs: [None; 64],
                render_outer_channels_started: [false; 64],
                compute_channel_control: None,
                logical_channel_controls: KVec::new(),
                logical_qos_ids: None,
                compute_b2_activation: g17_submission::G17PClB2ActivationState::Pending,
                render_sksm: None,
                bootstrap_uma: None,
                prepared_render_flist: None,
                next_render_flist_sequence: G17P_BOOTSTRAP_UMA_SEQUENCE,
                next_render_pm_manager_generation:
                    g17_resources::G17P_PM_INITIAL_HWPB_MANAGER_GENERATION,
                render_parameter_buffer_owner: 0,
                parked_render_slots: g17p_empty_render_slots()?,
                render_slot_scratch: Some(G17PRenderSlotState::new()?),
                render_channel_leases: [None; 64],
                active_render_slot: 0,
                accepted_firmware_recovery_generation: 0,
                pending_primary_recovery: None,
                pending_primary_control: None,
                pending_flist_release_worker_attempts: 0,
                pending_add_kicks: None,
                last_published_descriptor: 0,
                channel_ids: crate::g17_channel::ChannelTable::new(),
                next_channel_identity: 3,
                last_completion_end: 0,
                compute_completion_batch: g17_resources::G17PComputeCompletionBatch::new(),
                compute_stamp_armed: false,
                compute_stamp_waits: AtomicU32::new(0),
                compute_stamp_misses: AtomicU32::new(0),
            }),
            GFP_KERNEL,
        )?;
        if !g17p_reserve_bootstrap_channels(&mut manager.channel_ids) {
            return Err(EINVAL);
        }
        Ok(manager)
    }

    /// Large prepared-source observation before FList and outer publication.
    /// Diagnostic modes which expose an outer early receive an explicit
    /// omission instead of a misleading or timing-invasive "before" snapshot.
    pub(crate) fn capture_prepared_render_trace(&mut self, outers_deferred: bool) -> Result {
        #[cfg(CONFIG_DEV_COREDUMP)]
        {
            let Some(capture) = self.trace_capture.as_mut() else { return Ok(()); };
            if capture.prepared { return Ok(()); }
            capture.prepared = true;
            capture.job_stamp = self.render_sksm.as_ref()
                .map_or(0, |queues| queues.fragment.producer.last_add_kicks_timestamp);
            self.capture_render_trace_observation(trace::Phase::PreparedBeforeFList, outers_deferred)?;
        }
        Ok(())
    }

    #[cfg(CONFIG_DEV_COREDUMP)]
    fn capture_render_trace_observation(&mut self, phase: trace::Phase, safe: bool) -> Result {
        let Some(capture) = self.trace_capture.as_mut() else { return Ok(()); };
        capture.phase_sequence += 1;
        let sequence = capture.phase_sequence;
        let stamp = capture.job_stamp;
        let mut archive = TraceArchive::resume(&mut capture.arena).map_err(|_| EINVAL)?;
        let mut boundary = trace::Meta::new(trace::Kind::CaptureBoundary, phase, sequence);
        boundary.job_stamp = stamp;
        if !safe {
            archive.omit(boundary, trace::Omission::NotImplemented).map_err(|_| EINVAL)?;
            return Ok(());
        }
        let mut clock = || Monotonic::ktime_get() as u64;
        if self.resources.capture_owned_trace(&mut archive, phase, sequence, &mut clock).is_err() {
            archive.omit(boundary, trace::Omission::ReadError).map_err(|_| EINVAL)?;
        }
        match self.render_storage.as_mut() {
            Some(storage) => {
                if storage.capture_owned_trace(&mut archive, phase, sequence, stamp, &mut clock).is_err() {
                    boundary.instance = 1;
                    archive.omit(boundary, trace::Omission::ReadError).map_err(|_| EINVAL)?;
                }
            }
            None => {
                boundary.instance = 1;
                archive.omit(boundary, trace::Omission::Unavailable).map_err(|_| EINVAL)?;
            }
        }
        Ok(())
    }

    /// Only call after paired completion (before FList release), or after the
    /// state-1 recovery handshake has been validated (before ACK). A timeout
    /// alone is not permission to label a running GPU's memory "halted".
    #[cfg(CONFIG_DEV_COREDUMP)]
    pub(crate) fn finish_render_trace(
        &mut self, phase: trace::Phase,
    ) -> Result<Option<KBox<G17PTraceDump>>> {
        if !matches!(phase, trace::Phase::AfterPairRetirement | trace::Phase::RecoveryBeforeAck) {
            return Err(EINVAL);
        }
        if !self.trace_capture.as_ref().is_some_and(|capture| capture.prepared) {
            return Ok(None);
        }
        self.capture_render_trace_observation(phase, true)?;
        let mut capture = self.trace_capture.take().ok_or(EINVAL)?;
        let mut archive = TraceArchive::resume(&mut capture.arena).map_err(|_| EINVAL)?;
        archive.seal();
        let used = archive.bytes().len();
        drop(archive);
        capture.used = used;
        Ok(Some(capture))
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

    pub(crate) fn control_pointer_snapshot(
        &mut self,
    ) -> Result<g17_resources::G17PControlPointerSnapshot> {
        self.resources.control_pointer_snapshot()
    }

    pub(crate) fn status_a_snapshot(&mut self) -> Result<g17_resources::G17PStatusASnapshot> {
        self.resources.status_a_snapshot()
    }

    pub(crate) fn firmware_recovery_handshake_snapshot(
        &mut self,
    ) -> Result<g17_resources::G17PFirmwareRecoveryHandshakeSnapshot> {
        self.resources.firmware_recovery_handshake_snapshot()
    }

    pub(crate) fn firmware_fault_report_snapshot(
        &mut self,
    ) -> Result<g17_resources::G17PFirmwareFaultReportSnapshot> {
        self.resources.firmware_fault_report_snapshot()
    }

    pub(crate) fn firmware_dm1_slot_snapshot(
        &mut self,
        slot: usize,
    ) -> Result<g17_resources::G17PFirmwareDm1SlotSnapshot> {
        self.resources.firmware_dm1_slot_snapshot(slot)
    }

    pub(crate) fn drain_firmware_event(
        &mut self, role: g17_initdata::InstanceRole,
    ) -> Result<Option<g17_resources::G17PPrimaryFirmwareEventRecord>> {
        self.resources.drain_firmware_event(role)
    }

    pub(crate) fn drain_primary_firmware_event(
        &mut self,
    ) -> Result<Option<g17_resources::G17PPrimaryFirmwareEventRecord>> {
        self.resources.drain_primary_firmware_event()
    }

    pub(crate) fn queue_primary_recovery(
        &mut self,
        generation: u64,
        records: u32,
        blamed_stamp: Option<u8>,
    ) -> Result {
        if records == 0 || self.pending_primary_recovery.is_some() {
            return Err(EBUSY);
        }
        if blamed_stamp.is_some_and(|stamp| stamp >= 128) {
            return Err(EINVAL);
        }
        self.pending_primary_recovery = Some((generation, records, blamed_stamp));
        Ok(())
    }

    /// Queue a firmware restart request, or fold it into the one already
    /// pending.
    ///
    /// The firmware raises ONE restart request, and both RTKit roles can
    /// observe it: `service_primary_firmware_events` drains the gfx ring and
    /// then, in the same pass and unconditionally, the gfx1 ring. A type-4 on
    /// the second ring therefore meets a request the first ring already
    /// queued, and the bare `queue_primary_recovery` answers that with EBUSY.
    /// Every caller propagates it with `?` into a chain whose only error
    /// handler sets `crashed`, so a duplicate view of a single recovery -- the
    /// normal dual-role case, not a fault -- permanently deafened the session.
    ///
    /// Coalescing is what the surrounding code already does with duplicate
    /// type-4 records inside ONE ring (`recovery_records` counts them and the
    /// accepted generation advances once), so doing it across the two rings is
    /// the same rule, not a new one. Returns whether a new request was queued.
    pub(crate) fn queue_or_coalesce_primary_recovery(
        &mut self,
        generation: u64,
        records: u32,
        blamed_stamp: Option<u8>,
    ) -> Result<bool> {
        if let Some((pending_generation, pending_records, pending_stamp)) =
            self.pending_primary_recovery
        {
            // A pending request for a DIFFERENT generation is a real disagreement
            // about how many restarts the firmware asked for, and must not be
            // silently merged.
            if pending_generation != generation {
                dev_err!(
                    self.resources.dev().as_ref(),
                    "G17P recovery: type-4 generation {} arrived while generation {} is still pending\n",
                    generation,
                    pending_generation
                );
                return Err(EBUSY);
            }
            self.pending_primary_recovery = Some((
                generation,
                pending_records.saturating_add(records),
                pending_stamp.or(blamed_stamp),
            ));
            return Ok(false);
        }
        self.queue_primary_recovery(generation, records, blamed_stamp)?;
        Ok(true)
    }

    /// The host generation the next type-4 record must name. Records naming an
    /// older generation are duplicates of a restart already serviced.
    pub(crate) fn accepted_firmware_recovery_generation(&self) -> u64 {
        self.accepted_firmware_recovery_generation
    }

    pub(crate) fn pending_primary_recovery(&self) -> Option<(u64, u32, Option<u8>)> {
        self.pending_primary_recovery
    }

    pub(crate) fn complete_pending_primary_recovery(
        &mut self,
        generation: u64,
        records: u32,
        blamed_stamp: Option<u8>,
    ) -> Result {
        if self.pending_primary_recovery != Some((generation, records, blamed_stamp)) {
            return Err(EINVAL);
        }
        self.pending_primary_recovery = None;
        Ok(())
    }

    pub(crate) fn primary_firmware_recovery_cause_snapshot(
        &mut self,
    ) -> Result<g17_resources::G17PFirmwareRecoveryCauseSnapshot> {
        self.resources.primary_firmware_recovery_cause_snapshot()
    }

    pub(crate) fn arm_firmware_trace_classes(&mut self, mask: u32) -> Result {
        self.resources.arm_firmware_trace_classes(mask)
    }

    pub(crate) fn check_report_ring_pointers(
        &mut self,
        role: InstanceRole,
    ) -> Result<(u64, u64)> {
        self.resources.check_report_ring_pointers(role)
    }

    pub(crate) fn drain_report_ring(
        &mut self,
        role: InstanceRole,
        advance: bool,
        print_budget: u32,
    ) -> Result<g17_resources::G17PReportDrainStats> {
        self.resources.drain_report_ring(role, advance, print_budget)
    }

    pub(crate) fn check_ktrace_ring_pointers(
        &mut self,
        role: InstanceRole,
    ) -> Result<(u64, u64)> {
        self.resources.check_ktrace_ring_pointers(role)
    }

    pub(crate) fn drain_ktrace_ring(
        &mut self,
        role: InstanceRole,
        verbose: bool,
        print_budget: u32,
    ) -> Result<g17_resources::G17PKtraceDrainStats> {
        self.resources
            .drain_ktrace_ring(role, verbose, print_budget)
    }

    /// Publish the held-back tag-14 AddKicks. Call only once the firmware
    /// recovery has closed, otherwise the unhalt's 128-queue restore erases it.
    pub(crate) fn publish_pending_add_kicks(
        &mut self,
        queue: &mut G17PSksmQueue,
        expected_producer: u32,
    ) -> Result<bool> {
        let Some(bytes) = self.pending_add_kicks.take() else {
            return Ok(false);
        };
        let after = queue
            .compute
            .publish_add_kicks_command(expected_producer, &bytes)?;
        dev_info!(
            self.resources.dev().as_ref(),
            "G17P compute: deferred tag-14 AddKicks published at producer {} -> {}\n",
            expected_producer,
            after
        );
        Ok(true)
    }

    pub(crate) fn has_pending_add_kicks(&self) -> bool {
        self.pending_add_kicks.is_some()
    }

    pub(crate) fn dump_firmware_log(&mut self, max_records: usize) -> Result<usize> {
        self.resources.dump_firmware_log(max_records)
    }

    pub(crate) fn dump_completion_stamps(&mut self, label: &'static str) -> Result {
        self.resources.dump_completion_stamps(label)
    }

    pub(crate) fn dump_completion_records(&mut self, label: &'static str) -> Result {
        self.resources.dump_completion_records(label)
    }

    /// Publish one item-ring record, reporting the ring state if it is refused.
    ///
    /// The four publishes used to `?` straight out, so an exhausted or
    /// unreclaimed ring surfaced as a bare `EBUSY` from four frames down with
    /// nothing to say which record failed or what the cursors were. At the ring
    /// wrap that is precisely the information needed.
    fn publish_repeat_record(
        &mut self,
        queue: &mut G17PSksmQueue,
        which: &'static str,
        producer: u32,
        record: G17PRepeatRecord<'_>,
    ) -> Result<u32> {
        let result = match record {
            G17PRepeatRecord::Pointer(gpu_va) => {
                queue.compute.publish_command_pointer_at(producer, gpu_va)
            }
            G17PRepeatRecord::AddKicks(bytes) => {
                queue.compute.publish_add_kicks_command(producer, bytes)
            }
            G17PRepeatRecord::EntrySignal(bytes) => {
                queue.compute.publish_initial_entry_signal_at(producer, bytes)
            }
        };
        match result {
            Ok(next) => Ok(next),
            Err(error) => {
                dev_err!(
                    self.resources.dev().as_ref(),
                    "G17P compute: publishing {} at ring producer {} failed ({:?}); the target slot was not free\n",
                    which,
                    producer,
                    error
                );
                self.log_compute_ring_state(queue, "publish-failed");
                Err(error)
            }
        }
    }

    /// Per-submission retire trace, on the SUCCESS path.
    ///
    /// The failure mode has moved: a repeat now retires cleanly, with no ioctl
    /// error and no timeout, while the GPU writes nothing. A timeout-only hook
    /// can never observe that, so this samples the same evidence on every
    /// successful retire.
    ///
    /// The decisive field is the completion's own duration, `end - start`.
    /// Those are GPU timestamps written by the firmware, so:
    ///
    ///  * a plausible, varying duration means the GPU really executed and the
    ///    result is invisible to the reader -- a coherency or cache-maintenance
    ///    problem, not a scheduling one; whereas
    ///  * a zero or near-constant duration means the firmware retired a record
    ///    for work the GPU never ran, and the bug is upstream of completion.
    ///
    /// The support words are the cross-check: they are the GPU's own progress
    /// words for the submission, so if they stop advancing while completions
    /// keep arriving, the second reading is confirmed independently.
    ///
    /// DRAM only.
    pub(crate) fn log_compute_retire(
        &mut self,
        queue: &mut G17PSksmQueue,
        completion: [u64; 2],
        ordinal: u32,
    ) {
        let support = queue.compute.read_compute_support_words();
        let duration = completion[1].wrapping_sub(completion[0]);
        dev_info!(
            self.resources.dev().as_ref(),
            "G17P retire[{}]: descriptor {:#x} start {:#x} end {:#x} duration {:#x} support(dispatch-a,dispatch-b,status-a,status-b)={:#x?}\n",
            ordinal,
            self.last_published_descriptor,
            completion[0],
            completion[1],
            duration,
            support
        );
        if *module_parameters::g17p_completion_trace.value() > 1 {
            let _ = self.dump_compute_completion_records("retire");
        }
    }

    /// Decode the completion ring against the descriptor the in-flight
    /// submission is waiting for.
    pub(crate) fn dump_compute_completion_records(&mut self, label: &'static str) -> Result {
        let expected = self.last_published_descriptor;
        self.resources
            .dump_compute_completion_records(label, expected)
    }

    pub(crate) fn read_compute_completion(&mut self) -> Result<Option<[u64; 2]>> {
        let expected = self.last_published_descriptor;
        let last_end = self.last_completion_end;
        self.resources.read_compute_completion(expected, last_end)
    }

    /// Mark a completion as consumed so the next wait cannot be satisfied by
    /// the record it left behind.
    pub(crate) fn commit_compute_completion(&mut self, completion: [u64; 2]) {
        self.last_completion_end = completion[1];
    }

    pub(crate) fn dump_primary_recovery_cause_block(&mut self) -> Result {
        self.resources.dump_primary_recovery_cause_block()
    }

    pub(crate) fn primary_firmware_recovery_info_snapshot(
        &mut self,
    ) -> Result<g17_resources::G17PFirmwareRecoveryInfoSnapshot> {
        self.resources.primary_firmware_recovery_info_snapshot()
    }

    /// Accept one firmware-initiated recovery before Linux has constructed a
    /// queue, fence, or user submission. Native macOS compares the packet to
    /// its host-side accepted generation and increments that generation before
    /// it publishes recovery state 2.
    pub(crate) fn validate_empty_primary_firmware_recovery(
        &self,
        packet_generation: u64,
    ) -> Result {
        if packet_generation != self.accepted_firmware_recovery_generation {
            dev_err!(
                self.resources.dev().as_ref(),
                "G17P recovery: packet generation {} does not match host accepted generation {}\n",
                packet_generation,
                self.accepted_firmware_recovery_generation
            );
            return Err(EIO);
        }
        Ok(())
    }

    /// Is there live render scheduler state?
    ///
    /// This is the exact predicate
    /// `validate_empty_primary_firmware_recovery_scheduler` refuses on. It is
    /// exposed separately because that refusal is unsatisfiable for a
    /// render-blamed recovery: the firmware raises the restart request DURING
    /// the render wait, so `render_storage` and `render_tracker` are
    /// necessarily populated at exactly the moment we need to answer.
    pub(crate) fn render_scheduler_active(&self) -> bool {
        self.render_storage.is_some()
            || self.render_tracker.is_some()
            || self.render_queued_tracker.is_some()
            || self.render_bind.is_some()
            || self.render_context_retirement.is_some()
            || self.retired_render_handoff_graph.is_some()
            || !self.retired_render_handoff_graphs.is_empty()
            || self.render_submission_ordinal != 0
            || self.parked_render_scheduler_active()
    }

    pub(crate) fn validate_empty_primary_firmware_recovery_scheduler(&self) -> Result {
        if self.render_storage.is_some()
            || self.render_tracker.is_some()
            || self.render_queued_tracker.is_some()
            || self.render_bind.is_some()
            || self.render_context_retirement.is_some()
            || self.retired_render_handoff_graph.is_some()
            || !self.retired_render_handoff_graphs.is_empty()
            || self.render_submission_ordinal != 0
            || self.parked_render_scheduler_active()
        {
            dev_err!(
                self.resources.dev().as_ref(),
                "G17P recovery: empty-scheduler validation failed storage={} tracker={} bind={} ordinal={}\n",
                self.render_storage.is_some(),
                self.render_tracker.is_some(),
                self.render_bind.is_some(),
                self.render_submission_ordinal
            );
            return Err(EBUSY);
        }
        Ok(())
    }

    pub(crate) fn accept_empty_primary_firmware_recovery(
        &mut self,
        packet_generation: u64,
    ) -> Result<u64> {
        self.validate_empty_primary_firmware_recovery(packet_generation)?;
        self.accepted_firmware_recovery_generation =
            self.accepted_firmware_recovery_generation.wrapping_add(1);
        Ok(self.accepted_firmware_recovery_generation)
    }

    pub(crate) fn acknowledge_primary_firmware_recovery(
        &mut self,
        expected: u32,
        next: u32,
    ) -> Result {
        self.resources
            .acknowledge_primary_firmware_recovery(expected, next)
    }

    pub(crate) fn reset_empty_sksm_last_submitted_hw_timestamps(&mut self) -> Result {
        self.resources
            .reset_empty_sksm_last_submitted_hw_timestamps()
    }

    /// Hexdump one kernel-built record, for the add3-vs-Honeykrisp diff.
    ///
    /// Everything audited so far is an object the GPU READS. These are the
    /// records the DRIVER WRITES from the UAPI submit arguments, and add3 and
    /// Honeykrisp reach them through different callers with different
    /// arguments -- the last surface where the two paths can differ.
    fn log_record(&self, label: &str, bytes: &[u8]) {
        if *module_parameters::g17p_fault_report.value() & 1 == 0 {
            return;
        }
        for (index, chunk) in bytes.chunks(16).enumerate() {
            dev_info!(
                self.resources.dev().as_ref(),
                "G17P rec {} +{:#05x} {:02x?}\n",
                label,
                index * 16,
                chunk,
            );
        }
    }

    pub(crate) fn g17p_last_submitted_hw_timestamp(&mut self, qid: usize) -> Result<(u32, u64)> {
        self.resources.g17p_last_submitted_hw_timestamp(qid)
    }

    /// Verify the untouched native pre-user root phase. The first real user
    /// bind owns the transition from the retained context-0 roots to context
    /// 1; no driver-owned startup render VM is needed to manufacture it.
    pub(crate) fn validate_native_pre_user_roots(&self) -> Result {
        if !self
            .resources
            .uat()
            .t8140_partial_opening_kernel_contexts_ready()
        {
            dev_err!(
                self.resources.dev().as_ref(),
                "G17P roots: native pre-user context 0/1 table is invalid\n"
            );
            return Err(EIO);
        }
        dev_info!(
            self.resources.dev().as_ref(),
            "G17P roots: native pre-user context 0/1 table retained; user bind owns first split\n"
        );
        Ok(())
    }

    pub(crate) fn activate_render_global_context(&self) -> Result {
        self.resources
            .uat()
            .activate_t8140_render_global_context()
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
        parameters.gpc_perf_state_map = self.gpc_perf_state_map;
        parameters.gpc_perf_state_map_low = self.gpc_perf_state_map_low;
        parameters.gpc_perf_state_control = self.gpc_perf_state_control;
        parameters.encoder = g17_resources::G17P_BOOTSTRAP_VDM_VA;
        parameters.process_empty_tiles = false;
        parameters.tile_config &= !0x1_0000;
        let timestamp = g17_uapi::UapiTimestamp {
            handle: 0,
            offset: 0,
        };
        let command = g17_uapi::TranslatedRenderCommand {
            render_barrier: g17_uapi::UAPI_BARRIER_NONE,
            compute_barrier: g17_uapi::UAPI_BARRIER_NONE,
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
            || self.render_queued_tracker.is_some()
            || self.render_storage.is_some()
            || self.render_bind.is_some()
            || self.render_submission_ordinal != 0
        {
            return Err(EBUSY);
        }
        let mut no_retained_sksm_backing = None;
        let pm_generation = self.prepare_render_pm_manager_generation()?;
        self.render_storage = Some(
            self.resources
                .new_user_render_storage(
                    dev,
                    &vm,
                    g17_resources::G17PRenderQueuePair::for_slot(0).ok_or(EINVAL)?,
                    None,
                    1,
                    None,
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
        if let Some(failed) = storage.first_unreachable(self.resources.uat(), &vm) {
            storage.log_reachability(dev, self.resources.uat(), &vm);
            dev_err!(
                dev.as_ref(),
                "G17P bootstrap render: storage unreachable, first failing predicate is {}\n",
                failed
            );
            return Err(EFAULT);
        }
        let binding = self.resources.uat().bind(&vm)?;
        if binding.slot() != 1 {
            return Err(EFAULT);
        }
        self.resources
            .uat()
            .install_t8140_partial_opening_shared_render_roots(&binding)?;
        self.render_bind = Some(G17PRenderRootOwner::Bootstrap(binding));
        self.commit_render_pm_manager_generation(pm_generation)?;
        Ok(vm)
    }

    /// Apply the measured post-opening context split after primary
    /// control-done and before either first-work producer becomes visible.
    pub(crate) fn split_compute_bootstrap_render_contexts(&self) -> Result {
        let binding = self.render_bind.as_ref().and_then(G17PRenderRootOwner::bootstrap).ok_or(ENODEV)?;
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
            || self.render_queued_tracker.is_some()
            || self.render_bind.is_none()
            || self.render_submission_ordinal != 0
        {
            return Err(EBUSY);
        }
        if !self.resources.uat().t8140_first_work_render_context_ready(
            self.render_bind.as_ref().and_then(G17PRenderRootOwner::bootstrap).ok_or(ENODEV)?,
        ) {
            return Err(EFAULT);
        }
        let (fragment_queue, tiling_queue, previous, late_tiling) = {
            let storage = self.render_storage.as_mut().ok_or(ENODEV)?;
            // The successful first-partial path keeps both inner groups blank
            // until primary control-done. Restore 3D now; TA remains held by
            // the late publication callback below.
            storage.stage_fragment(0)?;
            let previous = storage.snapshot(self.render_submission_ordinal)?;
            let late_tiling = storage.prepare_late_tiling_publication(0)?;
            let (fragment_queue, tiling_queue) = storage.queue_gpu_vas();
            (fragment_queue, tiling_queue, previous, late_tiling)
        };
        let prepared = self.resources.publish_partial_opening_pair(
            true,
            g17_resources::G17PRenderQueuePair::for_slot(0).ok_or(EINVAL)?,
            g17_resources::G17PDeferredOuterPublication::None,
            fragment_queue,
            3,
            tiling_queue,
            3,
            true,
            || {},
            move || late_tiling.publish(),
        )?;
        self.render_tracker = Some(G17PPartialOpeningRenderTracker {
            ordinal: 0,
            hardware_buffer_id: 0,
            command_buffer_state_word: 0,
            qos_pending: false,
            pool_a_record_index: self.render_storage.as_ref().ok_or(ENODEV)?
                .current_pool_a_ticket().0,
            pool_a_submitted: self.render_storage.as_ref().ok_or(ENODEV)?
                .current_pool_a_ticket().1,
            context_update: true,
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
            let state = self.render_storage.as_mut().ok_or(ENODEV)?
                .snapshot(self.render_submission_ordinal)?;
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
                self.unreserve_render_storage_hardware_buffer_id();
                self.render_storage = None;
                self.render_sksm = None;
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
        if self.primary_device_control_backpressured() {
            return Err(EAGAIN);
        }
        self.resources.publish_compute_runtime_control_record(index)
    }

    /// Append one live device-control record to the primary channel-12 ring and
    /// advance its host-owned producer. The caller owns the EP-0x21 mailbox and
    /// must send `encode_primary_device_control()` afterwards.
    pub(crate) fn publish_primary_device_control_record(
        &mut self,
        opcode: u32,
        arg: u32,
    ) -> Result<g17_resources::G17PDeviceControlPublication> {
        if self.primary_device_control_backpressured() {
            return Err(EAGAIN);
        }
        self.resources
            .publish_primary_device_control_record(opcode, arg)
    }

    pub(crate) fn primary_device_control_backpressured(&self) -> bool {
        self.pending_primary_control.is_some()
            || self.pending_render_usc_freelist_release().is_some()
    }

    pub(crate) fn primary_tvb_reply_pending(&self) -> bool {
        self.pending_primary_control.is_some()
    }

    pub(crate) fn flush_pending_primary_device_control(
        &mut self,
    ) -> Result<G17PPendingPrimaryControlFlush> {
        let Some(pending) = self.pending_primary_control.as_ref() else {
            return Ok(G17PPendingPrimaryControlFlush::Empty);
        };
        if pending.recovery_generation != self.accepted_firmware_recovery_generation {
            return Err(EIO);
        }
        let record = pending.record;
        match self.resources.publish_primary_device_control_raw(&record) {
            Ok(publication) => {
                self.pending_primary_control = None;
                Ok(G17PPendingPrimaryControlFlush::Published(publication))
            }
            Err(error) if error == EAGAIN => Ok(G17PPendingPrimaryControlFlush::Blocked),
            Err(error) => Err(error),
        }
    }

    /// Count one event-worker pass, which is followed by a one-millisecond
    /// sleep outside the runtime mutex. Inline submit-side probes do not call
    /// this and therefore cannot consume the retry budget in a tight loop.
    pub(crate) fn note_primary_device_control_backpressure(&mut self) -> Result {
        let pending = self.pending_primary_control.as_mut().ok_or(EINVAL)?;
        pending.worker_attempts = pending.worker_attempts.checked_add(1).ok_or(ETIMEDOUT)?;
        if pending.worker_attempts > G17P_PRIMARY_CONTROL_RETRY_LIMIT {
            return Err(ETIMEDOUT);
        }
        Ok(())
    }

    pub(crate) fn pending_render_usc_freelist_release(
        &self,
    ) -> Option<G17PBootstrapUmaRelease> {
        self.bootstrap_uma
            .as_ref()
            .and_then(G17PBootstrapUmaLifecycle::pending_release)
            .or_else(|| {
                self.parked_render_slots.iter().find_map(|state| {
                    state.as_ref()?.bootstrap_uma
                        .as_ref()
                        .and_then(G17PBootstrapUmaLifecycle::pending_release)
                })
            })
    }

    pub(crate) fn note_flist_release_backpressure(&mut self) -> Result {
        if self.pending_render_usc_freelist_release().is_none() {
            return Err(EINVAL);
        }
        self.pending_flist_release_worker_attempts = self
            .pending_flist_release_worker_attempts
            .checked_add(1)
            .ok_or(ETIMEDOUT)?;
        if self.pending_flist_release_worker_attempts > G17P_PRIMARY_CONTROL_RETRY_LIMIT {
            return Err(ETIMEDOUT);
        }
        Ok(())
    }

    /// Grow the retained owner named by firmware, then publish its opcode-8
    /// reply. Both parked and selected pairs can be executing. Keep selection
    /// unchanged so event service cannot redirect the caller's render state.
    /// The caller owns the primary DeviceControl doorbell.
    pub(crate) fn service_render_tvb_grow(
        &mut self,
        request: g17_completion::G17PFirmwareTvbGrowEvent,
    ) -> Result<G17PTvbGrowDisposition> {
        if self.primary_device_control_backpressured() {
            return Err(EBUSY);
        }
        let generation = self.accepted_firmware_recovery_generation;
        if request.halt_count != generation {
            dev_warn!(self.resources.dev().as_ref(),
                "G17P TVB grow: ignoring stale request slot={} generation={} current={}\n",
                request.buffer_slot, request.halt_count, generation);
            return Ok(G17PTvbGrowDisposition::IgnoredStale);
        }
        let grow_owner = |resources: &mut g17_resources::G17ResourceOwner,
                          storage: Option<&mut g17_resources::G17PUserRenderStorage>,
                          bind: Option<&G17PRenderRootOwner>,
                          published: bool| -> Result<bool> {
            let storage = storage.ok_or(ENOENT)?;
            let bind = bind.ok_or(ENOENT)?;
            let id = storage.retained_hardware_buffer_id().ok_or(ENOENT)?;
            if !published || !request.matches_owner(generation, id, bind.root_slot()? as u32) {
                return Err(EINVAL);
            }
            resources.grow_render_tvb_synchronously(storage, bind.vm())
        };
        let selected_published = self.render_tracker.as_ref()
            .is_some_and(|tracker| tracker.hardware_buffer_id == request.buffer_slot)
            || self.render_queued_tracker.as_ref()
                .is_some_and(|tracker| tracker.hardware_buffer_id == request.buffer_slot)
            || (self.render_hardware_buffer_published
                && self.render_hardware_buffer_id == Some(request.buffer_slot));
        let result = if selected_published {
            grow_owner(&mut self.resources, self.render_storage.as_mut(),
                self.render_bind.as_ref(), true)
        } else if let Some(owner) = self.parked_render_slots.iter_mut().flatten()
            .find(|owner| {
                owner.tracker.as_ref()
                    .is_some_and(|tracker| tracker.hardware_buffer_id == request.buffer_slot)
                    || owner.queued_tracker.as_ref()
                        .is_some_and(|tracker| tracker.hardware_buffer_id == request.buffer_slot)
                    || (owner.hardware_buffer_published
                        && owner.hardware_buffer_id == Some(request.buffer_slot))
            })
        {
            let owner = &mut **owner;
            grow_owner(&mut self.resources, owner.storage.as_mut(), owner.bind.as_ref(),
                true)
        } else {
            Err(ENOENT)
        };
        // Failure is still a reply: claiming success or ignoring a current
        // request would leave firmware with a fictitious inventory or no wake.
        let grew = match result {
            Ok(grew) => grew,
            Err(error) => {
                dev_warn!(self.resources.dev().as_ref(),
                    "G17P TVB grow: slot={} context={} allocation failed ({:?})\n",
                    request.buffer_slot, request.vm_slot, error);
                false
            }
        };
        let record = request.reply(grew);
        match self.resources.publish_primary_device_control_raw(&record) {
            Ok(_) => {
                dev_info!(self.resources.dev().as_ref(),
                    "G17P TVB grow: slot={} context={} counter={} subpipe={} generation={} grew={} ack=8\n",
                    request.buffer_slot, request.vm_slot, request.counter, request.subpipe,
                    request.halt_count, grew);
                Ok(G17PTvbGrowDisposition::Published)
            }
            Err(error) if error == EAGAIN => {
                if self.pending_primary_control.is_some() {
                    return Err(EBUSY);
                }
                self.pending_primary_control = Some(G17PPendingPrimaryControl {
                    record,
                    recovery_generation: generation,
                    worker_attempts: 0,
                });
                Ok(G17PTvbGrowDisposition::Deferred)
            }
            Err(error) => Err(error),
        }
    }

    /// Activate this prepared pair's FList lease. Only the cold lease has a
    /// pending async grow to publish; a warm pair becomes Ready without 0x20.
    pub(crate) fn publish_render_usc_freelist_request(
        &mut self,
    ) -> Result<Option<g17_resources::G17PDeviceControlPublication>> {
        if self.render_storage.is_none() {
            return Err(EINVAL);
        }
        let mut lifecycle = self.prepared_render_flist.ok_or(EIO)?;
        lifecycle.retire_prepare_reference().map_err(|_| EIO)?;
        if lifecycle.request.is_none() {
            self.next_render_flist_sequence = lifecycle.sequence.checked_add(1).ok_or(EOVERFLOW)?;
            self.bootstrap_uma = Some(lifecycle);
            self.prepared_render_flist = None;
            return Ok(None);
        }
        if self.primary_device_control_backpressured() {
            return Err(EAGAIN);
        }
        let mut record = [0u8; g17_initdata::CONTROL_RECORD_SIZE];
        g17_submission::encode_g17p_usc_freelist_control(
            lifecycle.shared_control,
            lifecycle.operand_table,
            lifecycle.cookie,
            g17_submission::G17PUscFreelistSequence {
                word_04: G17P_BOOTSTRAP_UMA_STAMP as u32,
                word_0c: u32::try_from(lifecycle.sequence).map_err(|_| EOVERFLOW)?,
                word_30: lifecycle.hardware_buffer_id,
            },
            &mut record,
        )
        .map_err(|_| EINVAL)?;

        let publication = self
            .resources
            .publish_primary_device_control_raw(&record)?;
        self.bootstrap_uma = Some(lifecycle);
        self.prepared_render_flist = None;
        self.next_render_flist_sequence = lifecycle.sequence.checked_add(1).ok_or(EOVERFLOW)?;
        Ok(Some(publication))
    }

    /// Match a firmware type-13 completion to the retained FList request.
    /// This retires the async-grow reference only; TA and 3D still own it.
    pub(crate) fn match_render_usc_freelist_completion(
        &mut self,
        completion: G17PBootstrapUmaCompletion,
    ) -> Result {
        let target_slot = completion.hardware_buffer_id
            .checked_sub(129)
            .and_then(|slot| u8::try_from(slot).ok())
            .filter(|slot| *slot < g17_resources::G17PRenderQueuePair::SLOT_COUNT)
            .ok_or(EINVAL)?;
        let previous_slot = self.active_render_slot;
        if target_slot != previous_slot {
            self.select_render_slot(target_slot)?;
        }
        let matched = self.bootstrap_uma
            .as_mut()
            .ok_or(EIO)
            .and_then(|owner| owner.match_completion(completion).map_err(|_| EIO));
        let marked = if matched.is_ok() {
            self.render_storage.as_ref().ok_or(EIO).map(|storage| {
                storage.mark_render_flist_grown();
            })
        } else {
            Ok(())
        };
        let restored = if target_slot != previous_slot {
            self.select_render_slot(previous_slot)
        } else {
            Ok(())
        };
        matched.and(marked).and(restored)
    }

    /// Whether the asynchronous opcode-0x20 grow has retired its own
    /// reference. TA/3D references intentionally remain live until paired
    /// render completion.
    pub(crate) fn render_usc_freelist_grow_matched(&self) -> bool {
        self.bootstrap_uma
            .as_ref()
            .is_some_and(|lifecycle| lifecycle.state == G17PBootstrapUmaRequestState::Ready)
    }

    /// Retire the TA and 3D FList references after paired render completion.
    /// Only this final native reference drop authorizes opcode 0x2e.
    pub(crate) fn retire_render_usc_freelist_at_completion(
        &mut self,
    ) -> Result<Option<G17PBootstrapUmaRelease>> {
        let Some(lifecycle) = self.bootstrap_uma.as_mut() else {
            return Ok(None);
        };
        let owned = lifecycle.hardware_buffer_owned;
        let references = lifecycle.hardware_buffer_references;
        let pending = lifecycle.pending_release_count;
        match lifecycle.retire_render_references() {
            Ok(release) => Ok(release),
            Err(error) => {
                // This used to collapse every lifecycle state into a bare EIO,
                // which is what the render now fails with AFTER its paired
                // completion fires -- i.e. after the GPU has done the whole
                // render and both stages have stamped real timestamps. Name
                // the phase so the failure is diagnosable from one run.
                dev_warn!(
                    self.resources.dev().as_ref(),
                    "G17P render: FList retire refused ({:?}) owned={} references={} pending={}\n",
                    error,
                    owned,
                    references,
                    pending,
                );
                Err(EIO)
            }
        }
    }

    /// Publish the release generated by this pair's final reference drop.
    /// The caller still owns the EP-0x21 announcement.
    pub(crate) fn publish_render_usc_freelist_release(
        &mut self,
        release: G17PBootstrapUmaRelease,
    ) -> Result<g17_resources::G17PDeviceControlPublication> {
        let target_slot = release.hardware_buffer_id
            .checked_sub(129)
            .and_then(|slot| u8::try_from(slot).ok())
            .filter(|slot| *slot < g17_resources::G17PRenderQueuePair::SLOT_COUNT)
            .ok_or(EINVAL)?;
        self.select_render_slot(target_slot)?;
        if self.pending_primary_control.is_some() {
            return Err(EAGAIN);
        }
        let pending = self
            .bootstrap_uma
            .as_ref()
            .and_then(G17PBootstrapUmaLifecycle::pending_release);
        if pending != Some(release) {
            dev_warn!(
                self.resources.dev().as_ref(),
                "G17P render: FList release mismatch pending={:?} wanted={:?}\n",
                pending,
                release,
            );
            return Err(EIO);
        }
        let mut record = [0u8; g17_initdata::CONTROL_RECORD_SIZE];
        g17_initdata::encode_bootstrap_uma_release_record(
            release.sequence,
            release.hardware_buffer_id,
            &mut record,
        )
        .map_err(|_| EINVAL)?;
        let publication = self
            .resources
            .publish_primary_device_control_raw(&record)?;
        self.bootstrap_uma
            .as_mut()
            .ok_or(EIO)?
            .mark_published(release)
            .map_err(|_| EIO)?;
        self.pending_flist_release_worker_attempts = 0;
        Ok(publication)
    }

    pub(crate) fn mark_render_usc_freelist_release_sent(
        &mut self,
        producer_after: u32,
    ) -> Result {
        self.bootstrap_uma
            .as_mut()
            .ok_or(EIO)?
            .mark_sent(producer_after)
            .map_err(|_| EIO)
    }

    /// Settle every sent owner against one coherent control-ring snapshot.
    /// This is allocation-free, sleep-free and independent of slot selection.
    pub(crate) fn settle_consumed_render_usc_freelist_releases(&mut self) -> Result<u32> {
        let counters = self.resources.control_counters()?;
        let consumer = counters.primary[g17_initdata::CHANNEL_STATE_CONSUMER];
        let producer = counters.primary[g17_initdata::CHANNEL_STATE_PRODUCER];
        let mut settled = 0u32;
        let mut settle = |lifecycle: &mut Option<G17PBootstrapUmaLifecycle>| -> Result {
            let Some(lifecycle) = lifecycle.as_mut().filter(|owner| owner.release_sent()) else {
                return Ok(());
            };
            if lifecycle
                .observe_release_consumer(consumer, producer)
                .map_err(|_| EIO)?
            {
                settled = settled.checked_add(1).ok_or(EOVERFLOW)?;
            }
            Ok(())
        };
        settle(&mut self.bootstrap_uma)?;
        for state in &mut self.parked_render_slots {
            if let Some(state) = state.as_mut() {
                settle(&mut state.bootstrap_uma)?;
            }
        }
        Ok(settled)
    }

    /// Only the selected physical owner gates reuse of its slot. A delayed
    /// release from another slot must not serialize independent render work.
    pub(crate) fn render_slot_flist_release_sent(&self, slot: u8) -> Result<bool> {
        if slot >= g17_resources::G17PRenderQueuePair::SLOT_COUNT {
            return Err(EINVAL);
        }
        let lifecycle = if slot == self.active_render_slot {
            self.bootstrap_uma.as_ref()
        } else {
            self.parked_render_slots[usize::from(slot)]
                .as_ref()
                .and_then(|state| state.bootstrap_uma.as_ref())
        };
        Ok(lifecycle.is_some_and(G17PBootstrapUmaLifecycle::release_sent))
    }

    pub(crate) fn prepare_compute_runtime_class2(&mut self) -> Result {
        self.resources.prepare_compute_runtime_class2()
    }

    pub(crate) fn render_flist_owned(&self) -> bool {
        self.render_storage.is_some()
    }

    pub(crate) fn stage_compute_flist_state(&mut self) -> Result {
        self.resources.stage_compute_flist_state()
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
            Some(binding) => binding.bootstrap().is_some_and(|binding| self
                .resources
                .uat()
                .t8140_partial_opening_shared_render_context_ready(binding)),
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
        let prepared = self.resources.publish_partial_opening_pair(
            false,
            g17_resources::G17PRenderQueuePair::for_slot(0).ok_or(EINVAL)?,
            g17_resources::G17PDeferredOuterPublication::None,
            submission.fragment_queue_gpu_va,
            submission.fragment_queue_write_index,
            submission.tiling_queue_gpu_va,
            submission.tiling_queue_write_index,
            true,
            || {},
            publish_late_tiling_state,
        )?;
        if prepared.paired_doorbell != g17_submission::G17P_PARTIAL_OPENING_PAIRED_DOORBELL
            || prepared.paired_doorbell != 0x0083_0000_0000_0000
        {
            return Err(EINVAL);
        }
        let tracker = G17PPartialOpeningRenderTracker {
            ordinal: 0,
            hardware_buffer_id: 0,
            command_buffer_state_word: 0,
            qos_pending: false,
            pool_a_record_index: 0,
            pool_a_submitted: 0,
            context_update: false,
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
            g17_resources::g17p_dynamic_kernel_va_range(
                self.resources.uat().kernel_va_range()?).ok_or(ERANGE)?,
            mmu::UAT_PGSZ as u64,
            mmu::PROT_FW_SHARED_RW,
            false,
        )
    }

    /// Release one stopped firmware session without replacing its UAT owner.
    pub(crate) fn release_runtime_objects_after_processor_stop(&mut self) {
        for parked in &mut self.parked_render_slots {
            let Some(mut state) = parked.take() else { continue };
            if let Some(id) = state.hardware_buffer_id.take() {
                if let Err(error) = self.render_hardware_buffer_ids.release(id) {
                    dev_warn!(
                        self.resources.dev().as_ref(),
                        "G17P render: failed to release parked TA HardwareBufferID {} after processor stop ({:?})\n",
                        id,
                        error,
                    );
                }
            }
            if let Some(id) = state
                .storage
                .as_ref()
                .and_then(g17_resources::G17PUserRenderStorage::retained_hardware_buffer_id)
            {
                self.render_hardware_buffer_ids.unreserve(id);
            }
            Self::release_retired_handoff_graph(
                &mut self.render_hardware_buffer_ids,
                &mut self.retired_parameter_managers,
                state.retired_handoff_graph.take(),
            );
            // Both firmware CPUs are stopped; dropping the complete state now
            // retracts its command root before any reachable mapping backing.
            drop(state);
        }
        if let Some(id) = self.render_hardware_buffer_id.take() {
            match self.render_hardware_buffer_ids.release(id) {
                Ok(()) => dev_info!(
                    self.resources.dev().as_ref(),
                    "G17P render: released TA HardwareBufferID {} after processor stop\n",
                    id,
                ),
                Err(error) => dev_warn!(
                    self.resources.dev().as_ref(),
                    "G17P render: failed to release TA HardwareBufferID {} after processor stop ({:?})\n",
                    id,
                    error,
                ),
            }
        }
        self.render_hardware_buffer_published = false;
        self.render_outer_channels_started = [false; 64];
        self.compute_channel_control = None;
        self.release_logical_channel_controls_after_processor_stop();
        self.logical_qos_ids = None;
        self.compute_b2_activation = g17_submission::G17PClB2ActivationState::Pending;
        self.render_context_retirement = None;
        self.render_context_epochs = [0; 64];
        self.render_bind = None;
        self.render_tracker = None;
        self.render_queued_tracker = None;
        self.unreserve_render_storage_hardware_buffer_id();
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
        self.render_command_buffer_state_pairs = [None; 64];
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
        self.channel_ids = g17p_initial_channel_table();
        self.render_channel_leases = [None; 64];
        // Keep host identities monotonic across restarts. An old packet's
        // cached lease must not validate against a replacement QID owner.
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
            || self.render_queued_tracker.is_some()
            || self.render_storage.is_some()
            || self.render_context_aliases.is_some()
            || self.render_context_retirement.is_some()
            || self.retired_render_handoff_graph.is_some()
            || !self.retired_render_handoff_graphs.is_empty()
            || self.parked_render_scheduler_active()
        {
            return Err(EBUSY);
        }
        // Both firmware CPUs are stopped, so no old-session control record can
        // still require delivery. Clear deferred host work before the fallible
        // resource rewrite; a failed rewrite must not leave it eligible for a
        // later retry against partially reset mappings.
        self.pending_primary_control = None;
        self.pending_flist_release_worker_attempts = 0;
        self.resources.reset_session_after_processor_stop()?;
        self.accepted_firmware_recovery_generation = 0;
        self.pending_primary_recovery = None;
        Ok(())
    }

    fn retained_graph_matches_vm(
        graph: &G17PRetiredRenderGraph,
        vm: &mmu::Vm,
    ) -> bool {
        // Fixed RT/TPC mappings belong to the VM and physical pair, not the
        // logical queue's descriptor context. A cached graph may therefore
        // need a context retarget before it can execute the incoming queue.
        graph.bind.matches(vm)
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
                        Self::retained_graph_matches_vm(graph, vm)
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
    /// receives an independent application-GART context. Reusing a sibling
    /// queue's VM-owned graph requires retargeting that encoded context first.
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

    /// Failure-only DRAM census. A firmware-branch failure supplies the exact
    /// snapshot that rejected handoff; a host-branch failure takes one diagnostic
    /// snapshot. Never poll, select a queue, access registers, or change owners.
    fn report_render_handoff_busy(
        &mut self,
        vm: &mmu::Vm,
        descriptor_context_id: u16,
        reason: &'static str,
        snapshot: Option<Result<g17_resources::G17PUserRenderQueueState>>,
    ) {
        let retained_bind = self.render_bind.as_ref()
            .map(|binding| (binding.vm_id(), binding.root()));
        let retained_storage = self.render_storage.as_ref().map(|storage| (
            storage.queue_pair().slot(),
            storage.descriptor_context_id(),
            storage.render_pool_id(),
            storage.context_update_pending(),
        ));
        let matches = (
            self.render_bind.as_ref().is_some_and(|binding| binding.matches(vm)),
            self.render_storage.as_ref().is_some_and(|storage| {
                storage.descriptor_context_id() == descriptor_context_id
            }),
            self.render_storage.as_ref().is_some_and(|storage| {
                vm.render_pool_mappings_match(storage.queue_pair().slot(), storage.render_pool_id())
            }),
            self.render_context_aliases.as_ref().map_or(true, |aliases| aliases.matches(vm)),
        );
        let host_mask = u32::from(self.render_tracker.is_some())
            | (u32::from(self.render_queued_tracker.is_some()) << 1)
            | (u32::from(self.render_hardware_buffer_id.is_some()) << 2)
            | (u32::from(self.render_hardware_buffer_published) << 3)
            | (u32::from(self.prepared_render_flist.is_some()) << 4)
            | (u32::from(self.render_submission_ordinal == 0) << 5);
        let host = (
            self.render_submission_ordinal,
            self.render_tracker.as_ref().map(|tracker| tracker.ordinal),
            self.render_queued_tracker.as_ref().map(|tracker| tracker.ordinal),
            self.render_hardware_buffer_id,
            self.render_hardware_buffer_published,
            self.prepared_render_flist.is_some(),
        );
        let snapshot = match snapshot {
            Some(snapshot) => snapshot,
            None => self.render_storage.as_mut().ok_or(ENODEV)
                .and_then(|storage| storage.snapshot(self.render_submission_ordinal)),
        };
        dev_warn!(
            self.resources.dev().as_ref(),
            "G17P render handoff busy: reason={} selected-slot={} incoming(vm,context)=({},{}) retained(vm,root)={:x?} storage(slot,context,pool,context-pending)={:x?} matches(vm,context,pool,aliases)={:?} host-mask={:#x} host(ordinal,tracker,queued,hbid,published,prepared-flist)={:?} dram={:x?}\n",
            reason, self.active_render_slot, vm.id(), descriptor_context_id,
            retained_bind, retained_storage, matches, host_mask, host, snapshot,
        );
    }

    /// Actual paired completion and FList release authorize root ownership
    /// changes. T8140's handoff page does not publish a live idle witness.
    fn require_retired_render_owner(&mut self) -> Result {
        self.require_retired_render_owner_inner(None)
    }

    fn require_retired_render_owner_inner(
        &mut self,
        handoff: Option<(&mmu::Vm, u16)>,
    ) -> Result {
        // Handoff.current_slot is a T8140 stand-in, not a firmware idle
        // witness. Use the actual paired retirement and FList acknowledgements.
        if self.render_tracker.is_some() || self.render_queued_tracker.is_some()
            || self.render_hardware_buffer_id.is_some()
            || self.render_hardware_buffer_published || self.prepared_render_flist.is_some()
            || self.render_submission_ordinal == 0
        {
            if let Some((vm, context)) = handoff {
                self.report_render_handoff_busy(vm, context, "host-state", None);
            }
            return Err(EBUSY);
        }
        // This slot's USC FList is retained with this graph. Other slots may
        // retain their independent pools after this slot's TA/3D pair is
        // complete; those references do not keep this job's graph or VM alive.
        let snapshot = match self.render_storage.as_mut().ok_or(ENODEV)?
            .snapshot(self.render_submission_ordinal)
        {
            Ok(snapshot) => snapshot,
            Err(error) => {
                if error == EBUSY {
                    if let Some((vm, context)) = handoff {
                        self.report_render_handoff_busy(vm, context, "snapshot-read", Some(Err(error)));
                    }
                }
                return Err(error);
            }
        };
        if !snapshot.job_list_empty || !snapshot.tiling.idle() || !snapshot.fragment.idle() {
            if let Some((vm, context)) = handoff {
                self.report_render_handoff_busy(vm, context, "firmware-state", Some(Ok(snapshot)));
            }
            return Err(EBUSY);
        }
        // Do not require the shared outer channels to be empty here.
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
        execution_context: Arc<crate::g17_resources::G17PLogicalQueueContext>,
    ) -> Result {
        let cached_vm = cached.bind.vm_id();
        let mut stage = "retarget-command-context";
        let prepared = (|| -> Result {
            // The old root remains owned by cached.bind through the entire
            // transaction. Retargeting first additionally retains the new
            // root if a later alias allocation fails.
            cached.storage.retarget_command_context(vm, execution_context.clone())?;
            cached.bind.vm().clear_render_pool_mappings(cached.storage.queue_pair().slot());
            drop(cached.context_aliases.take());
            stage = "queue-backing";
            let backing = self.render_sksm.as_mut()
                .and_then(|queues| queues.backing.as_mut()).ok_or(EIO)?;
            stage = "client-vm-aliases";
            self.resources.rebind_cached_render_vm(&mut cached.storage, vm, backing)?;
            stage = "reactivate-rings";
            cached.storage.prepare_cached_reactivation(self.render_submission_ordinal)?;
            stage = "claim-queue-backing";
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
                "G17P graph rebind failed from vm={} to vm={} stage={} ({:?}); retaining complete graph\n",
                cached_vm, vm.id(), stage, error,
            );
            return Err(error);
        }
        drop(cached.bind);
        self.render_bind = Some(G17PRenderRootOwner::Job { context: execution_context });
        self.render_context_aliases = None;
        self.render_storage = Some(cached.storage);
        if crate::util::g17p_verbose() {
            dev_info!(
                self.resources.dev().as_ref(),
                "G17P render VM handoff: rebound complete cached graph from vm={} to vm={} on pair {} at ordinal {}\n",
                cached_vm, vm.id(), self.active_render_slot, self.render_submission_ordinal,
            );
        }
        Ok(())
    }

    fn retarget_retained_render_context(
        &mut self,
        vm: &mmu::Vm,
        execution_context: Arc<crate::g17_resources::G17PLogicalQueueContext>,
    ) -> Result<bool> {
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
            return Ok(true);
        }
        Ok(false)
    }

    pub(crate) fn handoff_retained_render_vm(
        &mut self,
        vm: &mmu::Vm,
        execution_context: Arc<crate::g17_resources::G17PLogicalQueueContext>,
    ) -> Result {
        if !execution_context.matches(vm) || !execution_context.is_current() {
            return Err(EFAULT);
        }
        self.retain_logical_channel_control(&execution_context.channel_control)
            .map_err(|error| Self::render_stage_fail("handoff-retain-channel-control", error))?;
        let descriptor_context_id =
            u16::try_from(execution_context.context_id()).map_err(|_| EOVERFLOW)?;
        if self.retarget_retained_render_context(vm, execution_context.clone())
            .map_err(|error| Self::render_stage_fail("handoff-retarget-current", error))?
        {
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
                    !Self::retained_graph_matches_vm(graph, vm)
                });
            if cached_immediate {
                self.retired_render_handoff_graph
                    .as_mut()
                    .ok_or(EIO)?
                    .storage
                    .prepare_cached_reactivation(self.render_submission_ordinal)
                    .map_err(|error| Self::render_stage_fail("handoff-prepare-immediate-cache", error))?;
            } else {
                self.retired_render_handoff_graphs
                    .get_mut(cached_index.ok_or(EIO)?)
                    .ok_or(EIO)?
                    .storage
                    .prepare_cached_reactivation(self.render_submission_ordinal)
                    .map_err(|error| Self::render_stage_fail("handoff-prepare-indexed-cache", error))?;
                self.preserve_immediate_retired_render_graph()
                    .map_err(|error| Self::render_stage_fail("handoff-preserve-immediate-cache", error))?;
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
                return self.reactivate_recycled_render_graph(cached, vm, execution_context)
                    .map_err(|error| Self::render_stage_fail("handoff-reactivate-empty-slot", error));
            }
            cached.storage.claim_sksm_backing(
                &mut self.render_sksm.as_mut().ok_or(EIO)?.backing,
            )?;
            let cached_vm = cached.bind.vm_id();
            self.render_bind = Some(cached.bind);
            self.render_context_aliases = cached.context_aliases;
            self.render_storage = Some(cached.storage);
            self.retarget_retained_render_context(vm, execution_context)
                .map_err(|error| Self::render_stage_fail("handoff-retarget-restored-cache", error))?;
            self.prune_unreferenced_retired_render_graphs()
                .map_err(|error| Self::render_stage_fail("handoff-prune-restored-cache", error))?;
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
        self.require_retired_render_owner_inner(Some((vm, descriptor_context_id)))
            .map_err(|error| Self::render_stage_fail("handoff-require-retired-owner", error))?;
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
                !Self::retained_graph_matches_vm(graph, vm)
            });
        if cached_immediate {
            let cached = self.retired_render_handoff_graph.as_mut().ok_or(EIO)?;
            if !cached.storage.sksm_backing_detached() {
                return Err(EIO);
            }
            cached
                .storage
                .prepare_cached_reactivation(self.render_submission_ordinal)
                .map_err(|error| Self::render_stage_fail("handoff-prepare-immediate-replacement", error))?;
        } else if let Some(index) = cached_index {
            let cached = self.retired_render_handoff_graphs.get_mut(index).ok_or(EIO)?;
            if !cached.storage.sksm_backing_detached() {
                return Err(EIO);
            }
            cached
                .storage
                .prepare_cached_reactivation(self.render_submission_ordinal)
                .map_err(|error| Self::render_stage_fail("handoff-prepare-indexed-replacement", error))?;
            self.preserve_immediate_retired_render_graph()
                .map_err(|error| Self::render_stage_fail("handoff-preserve-before-indexed", error))?;
        } else {
            self.preserve_immediate_retired_render_graph()
                .map_err(|error| Self::render_stage_fail("handoff-preserve-before-fresh", error))?;
        }
        self.park_render_sksm_backing_for_graph_rebuild()
            .map_err(|error| Self::render_stage_fail("handoff-park-sksm-backing", error))?;

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
        let retire_after_context_epoch = self
            .next_render_context_epoch(self.active_render_slot)
            .map_err(|error| Self::render_stage_fail("handoff-next-context-epoch", error))?;
        let retired = G17PRetiredRenderGraph {
            bind: self.render_bind.take().ok_or(EIO)?,
            context_aliases: self.render_context_aliases.take(),
            storage: self.render_storage.take().ok_or(ENODEV)?,
            retire_after_context_epoch,
        };
        self.retired_render_handoff_graph = Some(retired);
        if let Some(mut cached) = cached.take() {
            if cached_rebind {
                return self.reactivate_recycled_render_graph(cached, vm, execution_context)
                    .map_err(|error| Self::render_stage_fail("handoff-reactivate-replacement", error));
            }
            cached.storage.claim_sksm_backing(
                &mut self.render_sksm.as_mut().ok_or(EIO)?.backing,
            )?;
            let cached_vm = cached.bind.vm_id();
            self.render_bind = Some(cached.bind);
            self.render_context_aliases = cached.context_aliases;
            self.render_storage = Some(cached.storage);
            self.retarget_retained_render_context(vm, execution_context)
                .map_err(|error| Self::render_stage_fail("handoff-retarget-replacement", error))?;
            self.prune_unreferenced_retired_render_graphs()
                .map_err(|error| Self::render_stage_fail("handoff-prune-replacement", error))?;
            if crate::util::g17p_verbose() {
                dev_info!(self.resources.dev().as_ref(),
                    "G17P render VM handoff: reactivated retained graph vm={} at ordinal {} without clearing its render-pool mappings\n",
                    cached_vm, self.render_submission_ordinal);
            }
            return Ok(());
        }
        self.prune_unreferenced_retired_render_graphs()
            .map_err(|error| Self::render_stage_fail("handoff-prune-fresh", error))?;
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
            pr_info!(
                "G17P render release blocked: context={} retired={} tracker={} hwid={} published={} bind={}\n",
                context,
                retired_witness,
                state.tracker.is_some(),
                state.hardware_buffer_id.is_some(),
                state.hardware_buffer_published,
                state.bind.is_some(),
            );
            return Err(EBUSY);
        }
        let snapshot = state.storage.as_mut().ok_or(EIO)?
            .snapshot(state.submission_ordinal)?;
        if !snapshot.job_list_empty || !snapshot.tiling.idle() || !snapshot.fragment.idle() {
            pr_info!(
                "G17P render release blocked: context={} ordinal={} job-empty={} ta-idle={} 3d-idle={}\n",
                context,
                state.submission_ordinal,
                snapshot.job_list_empty,
                snapshot.tiling.idle(),
                snapshot.fragment.idle(),
            );
            return Err(EBUSY);
        }
        if state.sksm.as_ref().ok_or(EIO)?.backing.is_some() {
            pr_info!(
                "G17P render release blocked: context={} ordinal={} reason=detached-sksm-backing-owned-by-slot\n",
                context,
                state.submission_ordinal,
            );
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
        completed_epochs: [u64; 64],
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
    fn complete_render_context_update_retirement(&mut self, context_update: bool) -> Result {
        if !context_update {
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
        if crate::g17_config::RENDER_SLOT_AFFINITY == 0 {
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

    /// Predict mutation classes without running handoff or writing GPU data.
    /// In particular, do NOT call retained_render_needs_recycle here: actual
    /// submission calls it AFTER handoff, which normally resolves other-owner.
    pub(crate) fn render_transition_reasons(
        &mut self,
        vm: &mmu::Vm,
        render_slot: u8,
        context: &crate::g17_resources::G17PLogicalQueueContext,
        command: &g17_uapi::TranslatedRenderCommand,
    ) -> Result<u32> {
        use render_transition as reason;
        self.select_render_slot(render_slot)?;
        let context_id = u16::try_from(context.context_id()).map_err(|_| EOVERFLOW)?;
        let mut reasons = 0;
        if !self.render_sksm.as_ref().is_some_and(|queues| {
            queues.tiling.registered && queues.fragment.registered
        }) {
            reasons |= reason::REGISTRATION;
        }
        if self.render_storage.is_none() {
            reasons |= reason::FLIST_INIT;
        }
        // Same-VM context retargeting precedes cache selection in handoff.
        let retarget_current = self.render_bind.as_ref().is_some_and(|binding| binding.matches(vm))
            && self.render_storage.as_ref().is_some_and(|storage| {
                storage.descriptor_context_id() != context_id
                    && storage.first_unreachable(self.resources.uat(), vm).is_none()
            });
        let other_owner = self.retained_render_has_other_owner(vm, context_id);
        let use_current = self.render_storage.is_some() && (retarget_current || !other_owner);
        let (cached_immediate, cached_index) = if use_current {
            (false, None)
        } else {
            self.select_cached_render_graph(vm, context_id)
        };
        if retarget_current || other_owner || cached_immediate || cached_index.is_some() {
            reasons |= reason::CONTEXT_HANDOFF;
        }
        // Inspect the graph that handoff will select, not the outgoing graph.
        // Cached reactivation rebases cursors before restaging, so its old
        // ordinal is not evidence of a desync recovery or PB allocation.
        let storage = if use_current {
            self.render_storage.as_mut()
        } else if cached_immediate {
            self.retired_render_handoff_graph.as_mut().map(|graph| &mut graph.storage)
        } else {
            cached_index.and_then(|index| self.retired_render_handoff_graphs.get_mut(index))
                .map(|graph| &mut graph.storage)
        };
        let Some(storage) = storage else {
            return Ok(reasons | reason::NEW_PB_GRAPH);
        };
        if storage.context_update_pending() {
            reasons |= reason::CONTEXT_HANDOFF;
        }
        if use_current {
            if storage.first_unreachable(self.resources.uat(), vm).is_some() {
                reasons |= reason::UNREACHABLE;
            }
            if storage.restage_admission_desynced(self.render_submission_ordinal)? {
                reasons |= reason::DESYNC;
                // A desync rebuild also allocates a new PB graph. Never call
                // this a single-class experiment until that phase is isolated.
                reasons |= reason::NEW_PB_GRAPH;
            }
        }
        if self.render_submission_ordinal == u64::MAX {
            reasons |= reason::SESSION_RECYCLE;
        }
        if g17_resources::g17p_render_tvb_mode() >= 2 {
            let required = g17_resources::required_render_tvb_blocks(
                &command.parameters, self.num_clusters,
            ).ok_or(EINVAL)?;
            if storage.tvb_capacity() < required {
                reasons |= reason::TVB_GROWTH;
            }
        }
        Ok(reasons)
    }

    pub(crate) fn retained_render_needs_recycle(
        &self,
        vm: &mmu::Vm,
        descriptor_context_id: u16,
    ) -> bool {
        let retained_graph = self.render_storage.is_some() || self.render_context_aliases.is_some();
        let same_owner = !self.retained_render_has_other_owner(vm, descriptor_context_id);
        let recycle = g17_lifecycle::retained_render_requires_recycle(
            retained_graph,
            same_owner,
            self.render_submission_ordinal,
        );
        if recycle {
            dev_warn!(self.resources.dev().as_ref(),
                "G17P render: session recycle requested ordinal={} retained={} same-owner={} descriptor-context={}\n",
                self.render_submission_ordinal, retained_graph, same_owner, descriptor_context_id);
        }
        recycle
    }

    /// Prepare the two render KSM kick queues without touching the hardware
    /// bridge: QID 0 (TA, data master 0) and QID 1 (3D, data master 1).
    ///
    /// PROVEN by the valid cold macOS doorbell-1 capture: the first render uses
    /// QID 0 for TA/DM0 and QID 1 for 3D/DM1, both on priority zero. The QIDs,
    /// data masters, outer slots, tag-14 records, and SKSM entries below retain
    /// that exact initial profile.
    /// Name the render publish step that failed. `submit_translated_render`
    /// reports only a propagated errno, so before this every EINVAL raised
    /// below `begin_translated_render` -- render storage setup, the two-QID
    /// SKSM staging, the outer publish -- was indistinguishable at the ioctl.
    fn render_stage_fail(stage: &str, error: kernel::error::Error) -> kernel::error::Error {
        pr_info!(
            "G17P render: STAGE FAIL stage={} errno={}\n",
            stage,
            error.to_errno()
        );
        error
    }

    fn new_render_sksm_queues(
        &self,
        pair: g17_resources::G17PRenderQueuePair,
        hardware_buffer_id: u8,
        entry_vas: [(u64, u64); 2],
    ) -> Result<G17PRenderSksmQueues> {
        const RENDER_COMPLETION_SELECTOR: u8 =
            g17_submission::G17P_NATIVE_RENDER_DOORBELL_PRIORITY;
        let geometry = self.sksm_queue_geometry();
        let scratch = self.sksm_scratch();
        let build = |queue_id: u8, data_master: u32, tiling: bool, entry_gpu_va: u64| -> Result<G17PRenderSksmQueue> {
            let routing = g17_submission::G17PComputeQueueRouting {
                queue_id,
                completion_selector: RENDER_COMPLETION_SELECTOR as u32,
                entry_gpu_va,
                geometry,
            };
            let config = g17_submission::prepare_g17p_render_sksm_queue_config(
                routing,
                data_master,
                scratch,
            )
            .map_err(|_| Self::render_stage_fail("sksm-queue-config", EINVAL))?;
            Ok(G17PRenderSksmQueue {
                queue_id,
                tiling,
                entry_low: entry_gpu_va,
                entry_high: if tiling { entry_vas[0].1 } else { entry_vas[1].1 },
                config,
                producer: geometry
                    .initial_producer_state(
                        queue_id,
                        RENDER_COMPLETION_SELECTOR,
                        g17_submission::G17PClQosConfig {
                            // `encodeKickEntry` obtains this byte from the
                            // queue's AGXQOSQueue HardwareBufferID. It must
                            // match both ConfigUpdate +0x56 and the qid*8
                            // willSubmit record. Native gives each retained
                            // TA/3D graph one ID (captured qid9/10 use 4 while
                            // qid15/16 use 5), which is the same retained ID
                            // that owns the graph's PBDesc slot.
                            queue_byte_32: match crate::g17_config::RENDER_KICK_QOS
                            {
                                1 if tiling => 1,
                                2 => 1,
                                _ => hardware_buffer_id,
                            },
                            // G17P's stage wrappers select row 1 of the native
                            // 9x3 QoS table: TA=0x0c, 3D=0x18, CL=0x08. The
                            // former render path copied CL's class into both
                            // graphics queues, leaving 3D on the wrong launch
                            // class even after TA completed successfully.
                            class_40: match crate::g17_config::RENDER_KICK_QOS
                            {
                                1 if tiling => 0x08,
                                2 => 0x08,
                                _ if tiling => 0x0c,
                                _ => 0x18,
                            },
                            // Direct entry-ring captures show 0xffff here for
                            // both ordinary Metal TA and 3D kicks. Earlier
                            // values 1/0 came from the wrong adjacent global,
                            // and made Linux's dependency-control qword differ
                            // before either RCE program could execute.
                            word_48: 0xffff,
                        },
                    )
                    .map_err(|_| Self::render_stage_fail("sksm-producer-state", EINVAL))?,
                registered: false,
            })
        };
        Ok(G17PRenderSksmQueues {
            scratch,
            uat_owner: self.uat().ttb_base(),
            tiling: build(
                pair.tiling as u8,
                g17_submission::G17P_TA_DATA_MASTER,
                true,
                entry_vas[0].0,
            )?,
            fragment: build(
                pair.fragment as u8,
                match crate::g17_config::RENDER_3D_DM {
                    0xff => g17_submission::G17P_3D_DATA_MASTER,
                    other => other,
                },
                false,
                entry_vas[1].0,
            )?,
            backing: None,
        })
    }

    /// Publish one render group's SKSM half and return the KSM producer stamp
    /// its tag-14 AddKicks must announce.
    ///
    /// Ordering mirrors `submit_translated_first_bind_channel` exactly: the
    /// caller has already staged the group's command record and tag-15
    /// ConfigUpdate pointer. This writes the kick entry body before the caller
    /// publishes tag-14; tag 15 itself asks firmware to install a fresh QID.
    fn log_render_context_route(&mut self, label: &'static str) -> Result {
        let (pair, descriptor, tiling, fragment, root_slot, ctxdesc) = {
            let storage = self.render_storage.as_mut().ok_or(ENODEV)?;
            let pair = storage.queue_pair();
            let descriptor = storage.descriptor_context_ids()?;
            let tiling = storage.tiling_config_update_header()?;
            let fragment = storage.fragment_config_update_header()?;
            let root_slot = storage.render_root_slot();
            let ctxdesc = storage.render_channel_control_prefix()?;
            (pair, descriptor, tiling, fragment, root_slot, ctxdesc)
        };
        // Slot zero is already qualified and would add four lines to every
        // boot. The unresolved route is the independently retained qid2/3
        // pair, so keep this diagnostic bounded to that first installation.
        if pair.slot() == 0 {
            return Ok(());
        }
        let qos_context = u8::try_from(fragment.field_56).map_err(|_| EINVAL)?;
        let descriptor_context = u8::try_from(fragment.context_id).map_err(|_| EINVAL)?;
        let contexts = [1, qos_context, descriptor_context, root_slot];
        let roots = self
            .resources
            .uat()
            .t8140_selected_context_roots(contexts)?;
        dev_info!(
            self.resources.dev().as_ref(),
            "G17P render route[{}]: qids={}/{} descriptor+0x0c={}/{} tag15+0x32={}/{} tag15+0x56={}/{} contexts(opening,qos,descriptor,bind)={:?} roots=[{:#x}/{:#x},{:#x}/{:#x},{:#x}/{:#x},{:#x}/{:#x}] fw-ctxdesc(slot={},ctx={},state={},bound={})\n",
            label,
            pair.tiling,
            pair.fragment,
            descriptor[0],
            descriptor[1],
            tiling.context_id,
            fragment.context_id,
            tiling.field_56,
            fragment.field_56,
            contexts,
            roots[0].0,
            roots[0].1,
            roots[1].0,
            roots[1].1,
            roots[2].0,
            roots[2].1,
            roots[3].0,
            roots[3].1,
            ctxdesc[0],
            ctxdesc[1],
            ctxdesc[0x26],
            ctxdesc[0x33],
        );
        Ok(())
    }

    /// Check that tag 15 describes the only legal firmware install transition.
    /// A live queue may never be installed twice, while a fresh production
    /// queue must carry the install bit. The explicit no-install module
    /// parameter remains a diagnostic and deliberately leaves the queue fresh.
    fn validate_render_firmware_install(&self, tiling: bool, install: u16) -> Result<bool> {
        let queues = self.render_sksm.as_ref().ok_or(ENODEV)?;
        let queue = if tiling {
            &queues.tiling
        } else {
            &queues.fragment
        };
        match (queue.registered, install != 0) {
            (false, true) => Ok(true),
            (true, false) => Ok(false),
            (true, true) => Err(EBUSY),
            (false, false) if crate::g17_config::RENDER_NO_INSTALL != 0 => {
                Ok(false)
            }
            (false, false) => Err(EINVAL),
        }
    }

    /// Prove that every independently authored queue-install surface names
    /// the same physical queue before its first outer producer is published.
    fn validate_render_queue_consistency(
        &mut self,
        dev: &AsahiDevice,
        tiling: bool,
        header: g17_resources::G17PRenderConfigUpdateHeader,
        qos_record: [u8; 8],
    ) -> Result {
        let (scratch, queue_id, entry_low, entry_high, config, producer) = {
            let queues = self.render_sksm.as_ref().ok_or(ENODEV)?;
            let queue = if tiling {
                &queues.tiling
            } else {
                &queues.fragment
            };
            (
                queues.scratch,
                queue.queue_id,
                queue.entry_low,
                queue.entry_high,
                queue.config,
                queue.producer,
            )
        };
        let (pair, descriptor_context_id, descriptor_contexts, render_root_slot, pb_binding,
            context_update) = {
            let storage = self.render_storage.as_mut().ok_or(ENODEV)?;
            (
                storage.queue_pair(),
                storage.descriptor_context_id(),
                storage.descriptor_context_ids()?,
                storage.render_root_slot(),
                storage.parameter_buffer_binding(),
                storage.context_update_pending(),
            )
        };
        let header_qid = u8::try_from(header.queue_id).map_err(|_| EINVAL)?;
        // Native sources call queue +0x1c both the channel priority and the
        // completion selector. It feeds tag-15 +0x1e, entry-header bits 59+
        // and the SKSM routing payload. It is not a completion-ring ordinal:
        // normal CL selector 2 is consumed from descriptor ordinal 0.
        let queue_selector = u8::try_from(header.priority).map_err(|_| EINVAL)?;
        let data_master = u8::try_from(header.data_master).map_err(|_| EINVAL)?;
        let expected = g17_submission::prepare_g17p_render_sksm_queue_config(
            g17_submission::G17PComputeQueueRouting {
                queue_id: header_qid,
                completion_selector: u32::from(queue_selector),
                entry_gpu_va: header.entry_low,
                geometry: producer.geometry,
            },
            u32::from(data_master),
            scratch,
        )
        .map_err(|_| EINVAL)?;
        let expected_data_master = if tiling {
            g17_submission::G17P_TA_DATA_MASTER
        } else {
            g17_submission::G17P_3D_DATA_MASTER
        };
        let class_equal = qos_record[1] == producer.qos.class_40;
        let descriptor_context = descriptor_contexts[usize::from(!tiling)];
        let context_equal = header.context_id == descriptor_context_id
            && u32::from(header.context_id) == descriptor_context;
        let pb_equal = !tiling
            || (header.parameter_buffer_object == pb_binding.0
                && header.parameter_buffer_token == pb_binding.1
                && header.parameter_buffer_id == pb_binding.2
                && header.parameter_buffer_bit == u32::from(pair.fragment));
        if header.selector != 0x0f
            || header_qid != queue_id
            || header_qid != producer.queue_id
            || header.entry_low != entry_low
            || header.entry_high != entry_high
            || header.graphics != 1
            || u32::from(data_master) != expected_data_master
            || queue_selector != producer.completion_selector
            || header.field_56 != u16::from(producer.qos.queue_byte_32)
            || qos_record[0] != producer.qos.queue_byte_32
            || !context_equal
            || header.context_update != u16::from(context_update)
            || !pb_equal
            || config != expected
        {
            dev_err!(
                dev.as_ref(),
                "G17P render install split: stage={} qid={}/{}/{} dm={}/{} selector={}/{} entry={:#x}/{:#x} expected={:#x}/{:#x} hwid={}/{}/{} qos={:02x?}\n",
                if tiling { "TA" } else { "3D" },
                header.queue_id,
                queue_id,
                producer.queue_id,
                header.data_master,
                expected_data_master,
                header.priority,
                producer.completion_selector,
                header.entry_low,
                header.entry_high,
                entry_low,
                entry_high,
                header.field_56,
                producer.qos.queue_byte_32,
                qos_record[0],
                qos_record,
            );
            dev_err!(
                dev.as_ref(),
                "G17P render binding split: stage={} context(tag/owner/descriptor)={}/{}/{} update={}/{} root-slot={} pb(object/token/id/partner)={:#x}/{:#x}/{}/{} expected={:#x}/{:#x}/{}/{}\n",
                if tiling { "TA" } else { "3D" },
                header.context_id,
                descriptor_context_id,
                descriptor_context,
                header.context_update,
                u16::from(context_update),
                render_root_slot,
                header.parameter_buffer_object,
                header.parameter_buffer_token,
                header.parameter_buffer_id,
                header.parameter_buffer_bit,
                pb_binding.0,
                pb_binding.1,
                pb_binding.2,
                pair.fragment,
            );
            return Err(EINVAL);
        }
        if header.install != 0 && pair.slot() != 0 {
            dev_info!(
                dev.as_ref(),
                "G17P render install consistent: stage={} qid={} dm={} selector={} entry={:#x}/{:#x} tag={:#x} payload={:#x} masks={:#x}/{:#x} context={} hwid={} pb={:#x}/id{}/bit{:#x} qos-class={} kick-class={} class-equal={}\n",
                if tiling { "TA" } else { "3D" },
                queue_id,
                data_master,
                queue_selector,
                entry_low,
                entry_high,
                config.tag,
                config.payload,
                config.enable_pair.word0,
                config.enable_pair.word1,
                header.context_id,
                header.field_56,
                header.parameter_buffer_object,
                header.parameter_buffer_id,
                header.parameter_buffer_bit,
                qos_record[1],
                producer.qos.class_40,
                class_equal,
            );
        }
        Ok(())
    }

    /// Commit firmware ownership only after the complete inner and outer
    /// channel publication succeeded. This is intentionally infallible: once
    /// the producer is visible to firmware, host state must describe it.
    fn commit_render_firmware_install(&mut self, tiling: bool, install: bool) {
        if !install {
            return;
        }
        let queues = self
            .render_sksm
            .as_mut()
            .expect("validated render firmware install has queue storage");
        let queue = if tiling {
            &mut queues.tiling
        } else {
            &mut queues.fragment
        };
        queue.registered = true;
    }

    /// Directly install both halves of a fresh non-bootstrap render pair after
    /// the live-boot owner has completed its retained-provider/GPC preflight.
    ///
    /// The cold qid0/1 pair is proven to install through its simultaneous
    /// firmware tag-15 publication. A secondary pair is different: qid2 TA
    /// executes, but its qid3 parent reaches DM1 without launching unless the
    /// two native KSMFE routes already exist. Native configures both queue
    /// objects before publishing either outer command. Never expose a fresh
    /// pair after only one configuration landed.
    fn configure_fresh_nonbootstrap_render_pair(
        &self,
        registers: &regs::Resources,
        fragment_install: bool,
        tiling_install: Option<bool>,
    ) -> Result {
        if !fragment_install && !tiling_install.unwrap_or(false) {
            return Ok(());
        }
        let queues = self.render_sksm.as_ref().ok_or(ENODEV)?;
        let pair = self.render_storage.as_ref().ok_or(ENODEV)?.queue_pair();
        if pair.slot() == 0 {
            return Ok(());
        }
        if !fragment_install || tiling_install != Some(true) {
            return Err(Self::render_stage_fail("secondary-pair-partial-install", EINVAL));
        }
        let issued = registers.g17p_sksm_configure_powered_render_pair(
            (
                queues.fragment.config.write_pair(),
                queues.fragment.config.enable_pair,
            ),
            (
                queues.tiling.config.write_pair(),
                queues.tiling.config.enable_pair,
            ),
        )?;
        if !issued {
            dev_err!(
                self.resources.dev().as_ref(),
                "G17P render: pair {}/{} direct configuration did not issue while powered; outer publication withheld\n",
                pair.tiling,
                pair.fragment,
            );
            return Err(EAGAIN);
        }
        dev_info!(
            self.resources.dev().as_ref(),
            "G17P render: pair {}/{} directly configured as one transaction before simultaneous outer publication\n",
            pair.tiling,
            pair.fragment,
        );
        Ok(())
    }

    fn publish_render_sksm_group(
        &mut self,
        dev: &AsahiDevice,
        tiling: bool,
        queue_record_gpu_va: u64,
        barriers: &[g17_submission::G17PClBarrierDependency],
    ) -> Result<u64> {
        let ordinal = self.render_submission_ordinal;
        let auxiliary_timestamp = self.render_auxiliary_timestamp;
        if auxiliary_timestamp == 0 {
            return Err(Self::render_stage_fail("sksm-auxiliary-timestamp", EINVAL));
        }
        let scratch = self.sksm_scratch();
        let uat_owner = self.uat().ttb_base();
        // Mode 2 publishes a bare entry -- no RCE bindings or event mask -- as
        // a retained diagnostic. The default is the byte-exact render shape
        // recovered from the valid cold macOS SKSM entry-ring capture.
        let sksm_mode = crate::g17_config::RENDER_SKSM;
        let minimal = sksm_mode == 2;
        // Mode 3 is the shape a real Metal render pass uses, read out of the
        // macOS witnesses (recs2048/recs4096/recsmax-gfx.txt):
        //
        //   AGFIKSMEventFlagMask ... 32 bytes, ALL ZERO on both queues
        //   AGXKSMKickQueueEntryRCEInfo:
        //     TA  kind 0, one RegisterArray at descriptor +0x60, tag 0x47
        //     3D  kind 0, four descriptor-embedded RegisterArrays at
        //         descriptor +0xa0/+0x7c0/+0xee0/+0x1600,
        //         tags 0x56/0x10/0x17/0x0a
        //
        // RCEInfo +0x04 contains the active array count (1/4); native takes the
        // kind from `(RCEInfo +0x00) & 3`, which is zero in every render dump.
        // TA and 3D are both descriptor-local. 3D's +0x7a0/+0xec0/+0x15e0/
        // +0x1d00 words are each phase's +0x700 self-pointer trailer, not
        // pointers to an external allocation. Mode 4 remains an alias for the
        // old diagnostic command line, but emits this corrected native shape.
        let native_shaped = sksm_mode == 3 || sksm_mode == 4;
        let storage = self.render_storage.as_ref().ok_or(ENODEV)?;
        let descriptor_high = storage.descriptor_high_va(tiling)?;
        let descriptor_low = storage.descriptor_low_va(tiling)?;
        let work_items = storage.work_item_gpu_vas(tiling)?;
        // Native's descriptor MCache mappings and KSM aperture are distinct.
        // The working Metal render populates firmware descriptor +0x6a0 but
        // leaves host AGX3DCommandDescriptor +0xaa8 count zero, so bit58 and
        // KSM entry +0x08 must remain clear. Keep the historical coupling only
        // as an explicit diagnostic.
        let mcache = if crate::g17_config::RENDER_KSM_MCACHE == 0 {
            None
        } else if tiling {
            self.render_storage.as_ref().ok_or(ENODEV)?.tiling_mcache()
        } else {
            self.render_storage.as_ref().ok_or(ENODEV)?.fragment_mcache()
        };
        let empty_binding = g17_submission::G17PClRceBinding { address: 0, tag: 0 };
        let command_state = if minimal {
            g17_submission::G17PComputeClCommandState {
                event_mask: [0; 4],
                rce_bindings: [empty_binding; 4],
            }
        } else if native_shaped {
            g17_submission::prepare_g17p_render_cl_command_state(
                descriptor_low,
                tiling,
            )
                .map_err(|_| Self::render_stage_fail("sksm-render-command-state", EINVAL))?
        } else {
            g17_submission::prepare_g17p_compute_cl_command_state(descriptor_low, ordinal, 0x22)
                .map_err(|_| Self::render_stage_fail("sksm-command-state", EINVAL))?
        };

        let storage = self.render_storage.as_mut().ok_or(ENODEV)?;
        let queues = self.render_sksm.as_mut().ok_or(ENODEV)?;
        if queues.scratch != scratch || queues.uat_owner != uat_owner {
            let scratch_differs = queues.scratch != scratch;
            let recorded_uat = queues.uat_owner;
            pr_info!(
                "G17P render: SKSM mismatch scratch_differs={} uat {:#x} vs {:#x}\n",
                scratch_differs,
                recorded_uat,
                uat_owner
            );
            return Err(Self::render_stage_fail("sksm-scratch-uat-mismatch", EINVAL));
        }
        let queue = if tiling {
            &mut queues.tiling
        } else {
            &mut queues.fragment
        };
        if queue.tiling != tiling {
            return Err(Self::render_stage_fail("sksm-queue-role-mismatch", EINVAL));
        }
        let prepared = g17_submission::prepare_g17p_cl_kick_entry(
            queue.producer,
            g17_submission::G17PClKickEntryOperands {
                // These are host-side predicates, not firmware-descriptor
                // bytes. Bit1 is 3D-descriptor-specific; bit61 is a shared
                // device-global mode whose captured/default value is clear.
                // Keep both independently bisectable until their semantic
                // sources are modeled instead of forced.
                descriptor_flag_4c: !tiling
                    && (crate::g17_config::RENDER_HEADER_FLAGS & 1 != 0),
                descriptor_flag_5c8:
                    crate::g17_config::RENDER_HEADER_FLAGS & 2 != 0,
                converted_command_timestamp: queue.producer.current_timestamp,
                barriers,
                mcache,
                payload: [descriptor_high, queue_record_gpu_va],
                event_mask: command_state.event_mask,
                rce_kind: 0,
                rce_bindings: command_state.rce_bindings,
                auxiliary: auxiliary_timestamp,
            },
        )
        .map_err(|_| Self::render_stage_fail("sksm-kick-entry", EINVAL))?;
        storage
            .write_sksm_entry(
                tiling,
                prepared.entry_offset as usize,
                prepared.zero_length as usize,
                &prepared.entry,
            )
            .map_err(|error| Self::render_stage_fail("sksm-write-entry", error))?;
        fence(Ordering::SeqCst);
        let stamp = prepared.current_timestamp;
        queue.producer = prepared.producer_after;
        queue.producer.last_add_kicks_timestamp = stamp;
        if crate::util::g17p_verbose() {
            dev_info!(
                dev.as_ref(),
                "G17P render SKSM: {} qid {} entry_offset {:#x} entry_low {:#x} descriptor=[{:#x},{:#x}] items=[{:#x},{:#x},{:#x}] stamp {:#x} barriers {} mcache-ranges {} submission {}\n",
                if tiling { "TA" } else { "3D" },
                queue.queue_id,
                prepared.entry_offset,
                storage.sksm_entry_low_va(tiling),
                descriptor_high,
                descriptor_low,
                work_items[0],
                work_items[1],
                work_items[2],
                stamp,
                barriers.len(),
                mcache.map_or(0, |value| value.count),
                ordinal
            );
        }
        Ok(stamp)
    }

    /// Stage both render groups with the SKSM half interleaved, in the
    /// publication order the captures show:
    ///
    /// ```text
    /// 3D record -> 3D tag-15 install -> 3D kick entry -> 3D tag-14
    /// TA record -> TA tag-15 install -> TA kick entry -> TA tag-14
    /// ```
    ///
    /// The 3D kick carries the one explicit barrier every captured pass has:
    /// `{count 1, event_word = ta_stamp << 8, queue_id = qta}`, naming the
    /// stamp the TA group is about to announce. When the UAPI VDM barrier is
    /// zero, the TA kick also carries the preceding fragment stamp. This is
    /// the native queue dependency that orders `3D[n] -> TA[n+1]` without a
    /// host completion wait; the per-engine predecessor fields alone provide
    /// only `TA[n] -> TA[n+1]` and `3D[n] -> 3D[n+1]`.
    /// TA group's records are handed back as a `G17PLateTilingPublication`
    /// because the outer publisher applies them between the two ring slots.
    fn stage_render_groups_with_sksm(
        &mut self,
        dev: &AsahiDevice,
        wait_previous_fragment: bool,
    ) -> Result<(
        u64,
        u64,
        g17_resources::G17PUserRenderQueueState,
        Option<g17_resources::G17PLateTilingPublication>,
    )> {
        let ordinal = self.render_submission_ordinal;
        let previous_fragment_barrier = {
            let queues = self.render_sksm.as_ref().ok_or(ENODEV)?;
            if wait_previous_fragment && queues.fragment.producer.submitted != 0 {
                Some(g17_submission::g17p_render_barrier_from_timestamp(
                    queues.fragment.producer.last_add_kicks_timestamp,
                    queues.fragment.queue_id,
                ).map_err(|_| Self::render_stage_fail(
                    "sksm-previous-fragment-barrier", EINVAL,
                ))?)
            } else {
                None
            }
        };
        let hardware_buffer_id = u8::try_from(
            self.render_storage
                .as_ref()
                .ok_or(ENODEV)?
                .qos_hardware_buffer_id()?,
        )
        .map_err(|_| EOVERFLOW)?;
        {
            // QoS belongs to the logical queue across all three engines;
            // physical PB/PM allocation and QID identity are separate.
            let queues = self.render_sksm.as_mut().ok_or(ENODEV)?;
            queues.tiling.producer.qos.queue_byte_32 = hardware_buffer_id;
            queues.fragment.producer.qos.queue_byte_32 = hardware_buffer_id;
        }
        let channel_payload = g17_submission::g17p_serialized_render_scratch_payload(ordinal);
        let fragment_command_timestamp = self
            .render_sksm
            .as_ref()
            .ok_or(ENODEV)?
            .fragment
            .producer
            .current_timestamp;
        let tiling_command_timestamp = self.render_sksm.as_ref().ok_or(ENODEV)?
            .tiling.producer.current_timestamp;
        let (fragment_queue, tiling_queue) = {
            let storage = self.render_storage.as_mut().ok_or(ENODEV)?;
            let scratch = storage
                .stage_fragment_completion_scratch(
                    tiling_command_timestamp, fragment_command_timestamp, channel_payload,
                )
                .inspect_err(|error| {
                    dev_err!(
                        dev.as_ref(),
                        "G17P render: fragment completion scratch stage failed ({:?})\n",
                        error
                    );
                })?;
            if crate::util::g17p_verbose() {
                dev_info!(
                    dev.as_ref(),
                    "G17P render: 3D completion scratch={:#018x} timestamp={:#x} payload={}\n",
                    scratch,
                    fragment_command_timestamp,
                    channel_payload,
                );
            }
            storage
                .dump_submit_ready_fragment_descriptor(dev, ordinal)
                .inspect_err(|error| {
                    dev_err!(
                        dev.as_ref(),
                        "G17P render: submit-ready fragment descriptor dump failed ({:?})\n",
                        error
                    );
                })?;
            storage.stage_fragment_prefix(ordinal).inspect_err(|error| {
                dev_err!(
                    dev.as_ref(),
                    "G17P render: fragment prefix stage failed ({:?})\n",
                    error
                );
            })?;
            storage.queue_gpu_vas()
        };
        let (ta_stamp, ta_queue_id) = {
            let queues = self.render_sksm.as_ref().ok_or(ENODEV)?;
            (
                queues.tiling.producer.current_timestamp,
                queues.tiling.queue_id,
            )
        };
        let split_launch =
            crate::g17_config::RENDER_SPLIT_LAUNCH != 0;
        let native_fragment_first = g17p_native_fragment_first_mode(
            crate::g17_config::RENDER_NATIVE_DOORBELLS,
        );
        if !split_launch {
            let barrier = [g17_submission::g17p_render_barrier_from_timestamp(
                ta_stamp, ta_queue_id,
            ).map_err(|_| Self::render_stage_fail("sksm-event-word", EINVAL))?];
            let event_word = barrier[0].event_word;
            // DIAGNOSTIC, not a fix. Metal reserves a barrier SLOT per render
            // pass; suppressing the dependency asks only whether that missing
            // lifecycle is what stalls the fragment group.
            let suppress_barrier =
                crate::g17_config::RENDER_NO_BARRIER != 0;
            let barrier_slice: &[g17_submission::G17PClBarrierDependency] =
                if suppress_barrier { &[] } else { &barrier };
            if suppress_barrier {
                pr_info!(
                    "G17P render: DIAGNOSTIC barrier dependency suppressed (event_word {:#x} qid {})\n",
                    event_word,
                    ta_queue_id
                );
            }
            let fragment_first_install = self
                .render_sksm
                .as_ref()
                .is_some_and(|queues| !queues.fragment.registered);
            if fragment_first_install {
                if let Err(error) = self.log_render_context_route("before-3d-qid-install") {
                    dev_warn!(dev.as_ref(), "G17P render route: pre-3D snapshot failed ({:?})\n", error);
                }
            }
            let fragment_stamp = self
                .publish_render_sksm_group(dev, false, fragment_queue, barrier_slice)
                .map_err(|error| Self::render_stage_fail("sksm-publish-fragment", error))?;
            if fragment_first_install {
                if let Err(error) = self.log_render_context_route("after-3d-qid-install") {
                    dev_warn!(dev.as_ref(), "G17P render route: post-3D snapshot failed ({:?})\n", error);
                }
            }
            self.render_storage
                .as_mut()
                .ok_or(ENODEV)?
                .stage_fragment_kick(ordinal, fragment_stamp)
                .inspect_err(|error| {
                    dev_err!(
                        dev.as_ref(),
                        "G17P render: fragment AddKicks stage failed ({:?})\n",
                        error
                    );
                })?;
        } else {
            dev_info!(
                dev.as_ref(),
                "G17P render split-launch: retaining 3D SKSM entry and AddKicks until TA completion\n"
            );
        }
        let previous = self
            .render_storage
            .as_mut()
            .ok_or(ENODEV)?
            .snapshot(ordinal)
            .inspect_err(|error| {
                dev_err!(
                    dev.as_ref(),
                    "G17P render: initial snapshot failed ({:?})\n",
                    error
                );
            })?;
        if native_fragment_first {
            // Modes 2/3 return with a COMPLETE 3D group but no TA item/SKSM
            // publication. The caller retains the TA outer producer, sends
            // the 3D doorbell, and invokes `publish_native_render_tiling_group`
            // to construct TA in its exact tag15 -> SKSM -> tag14 chronology.
            let queues = self.render_sksm.as_ref().ok_or(ENODEV)?;
            if queues.tiling.producer.current_timestamp != ta_stamp {
                return Err(Self::render_stage_fail(
                    "native-ta-sksm-advanced-before-3d-doorbell",
                    EINVAL,
                ));
            }
            dev_info!(
                dev.as_ref(),
                "G17P render native chronology: complete 3D visible; retaining complete TA group until after Fragment doorbell\n"
            );
            return Ok((fragment_queue, tiling_queue, previous, None));
        }
        let tiling_first_install = self
            .render_sksm
            .as_ref()
            .is_some_and(|queues| !queues.tiling.registered);
        if tiling_first_install {
            if let Err(error) = self.log_render_context_route("before-ta-qid-install") {
                dev_warn!(dev.as_ref(), "G17P render route: pre-TA snapshot failed ({:?})\n", error);
            }
        }
        let previous_fragment_barriers = previous_fragment_barrier.as_slice();
        let tiling_stamp = self
            .publish_render_sksm_group(dev, true, tiling_queue, previous_fragment_barriers)
            .map_err(|error| Self::render_stage_fail("sksm-publish-tiling", error))?;
        if tiling_first_install {
            if let Err(error) = self.log_render_context_route("after-ta-qid-install") {
                dev_warn!(dev.as_ref(), "G17P render route: post-TA snapshot failed ({:?})\n", error);
            }
        }
        // The barrier the 3D kick already carries names this exact value.
        if tiling_stamp != ta_stamp {
            pr_info!(
                "G17P render: SKSM tiling stamp {} != barrier stamp {}\n",
                tiling_stamp,
                ta_stamp
            );
            return Err(Self::render_stage_fail("sksm-tiling-stamp-mismatch", EINVAL));
        }
        let late_tiling = self
            .render_storage
            .as_mut()
            .ok_or(ENODEV)?
            .prepare_late_tiling_publication_with_stamp(ordinal, tiling_stamp)
            .inspect_err(|error| {
                dev_err!(dev.as_ref(), "G17P render: tiling stage failed ({:?})\n", error);
            })?;
        Ok((fragment_queue, tiling_queue, previous, Some(late_tiling)))
    }

    pub(crate) fn begin_translated_render(
        &mut self,
        dev: &AsahiDevice,
        vm: &mmu::Vm,
        queue_pair: g17_resources::G17PRenderQueuePair,
        execution_context: Arc<crate::g17_resources::G17PLogicalQueueContext>,
        command: &g17_uapi::TranslatedRenderCommand,
        registers: &regs::Resources,
        registration_power: &mut impl FnMut(&mut Self) -> Result,
    ) -> Result<g17_submission::PreparedG17PPartialOpeningWorkPair> {
        if self.render_queue_pair(queue_pair.slot())? != queue_pair { return Err(EIO); }
        if let Some(leases) = self.render_channel_leases[usize::from(queue_pair.slot())] {
            // Retain on any later ambiguous failure, including registration.
            // Returning a slot needs a detach witness or processor stop.
            for lease in leases {
                if !self.channel_ids.publish(lease) { return Err(EIO); }
            }
        }
        self.select_render_slot(queue_pair.slot())?;
        let descriptor_context_id = execution_context.context_id() as u16;
        if !execution_context.matches(vm) || !execution_context.is_current() {
            return Err(EFAULT);
        }
        self.retain_logical_channel_control(&execution_context.channel_control)?;
        if descriptor_context_id < 64 {
            self.released_render_contexts &= !(1u64 << descriptor_context_id);
        }
        if self.render_hardware_buffer_id.is_some()
            || self.render_hardware_buffer_published
            || self.prepared_render_flist.is_some()
            || self.render_queued_tracker.is_some()
        {
            return Err(EBUSY);
        }
        // Before anything is allocated for this submission, deal with a graph
        // whose ring producer has drifted ahead of the ordinal. Nothing this
        // function does later can recover from it, and leaving it retires the
        // client for good.
        if self.render_tracker.is_none() {
            self.recycle_desynced_render_storage()?;
        }
        if g17_resources::g17p_render_tvb_mode() >= 2 {
            let dimension_required = g17_resources::required_render_tvb_blocks(
                &command.parameters,
                self.num_clusters,
            ).ok_or(EINVAL)?;
            // Firmware's Stats object belongs to the retained PM graph. Read
            // and reset it only after the exact TA/3D pair has retired; a
            // recovery notification is not a retirement witness. The first
            // submission has fresh Stats(reset=1) and no prior sample.
            let feedback_required = if self.render_tracker.is_none()
                && self.render_storage.is_some()
                && self.render_submission_ordinal != 0
            {
                self.require_retired_render_owner()?;
                self.render_storage
                    .as_mut()
                    .ok_or(EIO)?
                    .take_tvb_auto_grow_target()?
            } else {
                None
            };
            let required = feedback_required
                .unwrap_or(0)
                .max(dimension_required);
            if self.render_storage.as_ref().is_some_and(|storage| storage.tvb_capacity()<required) {
                if self.render_tracker.is_some() {
                    return Err(EBUSY);
                }
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

    /// Finalize one successfully doorbelled render publication. The tracker
    /// already owns every per-command lease; clearing the staging fields here
    /// admits the next ordered group without weakening retirement.
    pub(crate) fn commit_render_publication(&mut self) -> Result {
        let ordinal = self.render_submission_ordinal;
        let tracker_matches = self.render_tracker.as_ref()
            .is_some_and(|tracker| tracker.ordinal == ordinal)
            || self.render_queued_tracker.as_ref()
                .is_some_and(|tracker| tracker.ordinal == ordinal);
        if !tracker_matches
            || !self.render_hardware_buffer_published
            || self.render_hardware_buffer_id.is_none()
            || self.render_command_buffer_state_pairs[self.active_render_slot as usize].is_some()
            || self.prepared_render_flist.is_some()
        {
            return Err(EIO);
        }
        self.render_submission_ordinal = ordinal.checked_add(1).ok_or(EOVERFLOW)?;
        self.render_hardware_buffer_id = None;
        self.render_hardware_buffer_published = false;
        self.render_auxiliary_timestamp = 0;
        Ok(())
    }

    fn begin_translated_render_with_hardware_buffer(
        &mut self,
        dev: &AsahiDevice,
        vm: &mmu::Vm,
        queue_pair: g17_resources::G17PRenderQueuePair,
        execution_context: Arc<crate::g17_resources::G17PLogicalQueueContext>,
        descriptor_context_id: u16,
        command: &g17_uapi::TranslatedRenderCommand,
        parameter_buffer_token: u64,
        command_buffer_state_words: [u32; 2],
        registers: &regs::Resources,
        registration_power: &mut impl FnMut(&mut Self) -> Result,
    ) -> Result<g17_submission::PreparedG17PPartialOpeningWorkPair> {
        let render_sksm = crate::g17_config::RENDER_SKSM;
        let native_doorbell_mode =
            crate::g17_config::RENDER_NATIVE_DOORBELLS;
        let split_launch = crate::g17_config::RENDER_SPLIT_LAUNCH != 0;
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
        if self.render_queued_tracker.is_some() {
            return Err(EBUSY);
        }
        // Check the previous release BEFORE touching any retained graph or
        // pool bytes. Capture the same generation encoded in both tag-15s;
        // paired completion increments the ordinal before FList retirement.
        self.prepared_render_flist = None;
        if crate::g17_config::RENDER_USC_FREELIST != 0
            && self.bootstrap_uma.as_ref().is_some_and(|owner| {
                !matches!(owner.state,
                    G17PBootstrapUmaRequestState::Ready
                    | G17PBootstrapUmaRequestState::Completed)
            })
        {
            return Err(EBUSY);
        }
        self.resources
            .stage_render_region_views()
            .map_err(|error| Self::render_stage_fail("stage-region-views", error))?;
        // The render storage owns the complete SecureGart low-view closure in
        // its application-GART root.  User work must never repoint the global
        // context-0 lower VM; that singleton was the fundamental cross-client
        // exclusion and made a second independently rooted render impossible.
        if self.render_context_aliases.is_some() {
            return Err(Self::render_stage_fail("legacy-context0-alias-live", EBUSY));
        }
        let retained = self.render_storage.is_some();
        // Context 1 is the stable firmware-service root. Each retained render
        // owner maps a distinct FList backing into it, while the queue's
        // independently allocated application-GART context owns command data
        // and is the MCache selector. Taking a second VmBind here both
        // overwrote that service root and imposed the artificial four-client
        // ceiling.
        let render_root_slot = if retained {
            self.render_bind
                .as_ref()
                .ok_or_else(|| Self::render_stage_fail("retained-render-root-missing", EBUSY))?
                .root_slot()
                .map_err(|error| Self::render_stage_fail("render-root-slot", error))?
        } else {
            u8::try_from(execution_context.context_id())
            .map_err(|_| Self::render_stage_fail("render-root-slot-width", EIO))?
        };
        // Which construction path a render takes has never been logged, and a
        // cold boot was measured taking the RESTAGE branch on what should be a
        // first render (93cc292: neither log line inside new_user_render_storage
        // appears cold, so the UMA page-pool descriptor is never staged, while
        // the tiler parameter-management graph that faults binds that pool).
        // Name the branch and its inputs so the two readings -- genuinely
        // retained, versus construction attempted and abandoned -- separate in
        // one run.
        if crate::util::g17p_verbose() {
            dev_info!(
                self.resources.dev().as_ref(),
                "G17P render storage path: retained={} ordinal={} bind={} tracker={} root-slot={}\n",
                retained,
                self.render_submission_ordinal,
                self.render_bind.is_some(),
                self.render_tracker.is_some(),
                render_root_slot,
            );
        }
        if retained
            && !self
                .render_bind
                .as_ref()
                .is_some_and(|binding| binding.matches(vm))
        {
            return Err(EBUSY);
        }
        if retained {
            // The retained descriptors already encode the bound context as
            // their MCache write-range selector. If those two ever disagreed,
            // this client's fragment write ranges would be resolved against
            // another client's page tables, so refuse instead of restaging.
            if self.render_storage.as_ref().ok_or(ENODEV)?.render_root_slot()
                != render_root_slot
            {
                return Err(Self::render_stage_fail("retained-root-slot-moved", EBUSY));
            }
            let parameter_page_list_vas = self
                .render_storage
                .as_ref()
                .ok_or(ENODEV)?
                .parameter_buffer_page_list_vas()?;
            self.render_storage
                .as_mut()
                .ok_or(ENODEV)?
                .restage_command(
                    dev,
                    vm,
                    descriptor_context_id,
                    parameter_page_list_vas,
                    command,
                    self.num_clusters,
                    command_buffer_state_words,
                    self.render_submission_ordinal,
                )
                .map_err(|error| Self::render_stage_fail("restage-command", error))?;
            // The retained PM owner validates its unchanged PBDesc identity.
            // Firmware owns the live page-list/PBDesc cursors; do not reset
            // them to cold values when preparing another Scene. The cold
            // new_user_render_storage path below still initializes them.
            //
            // Each owner has its own HardwareBufferID-indexed PBDesc. Check
            // that identity when switching owners without rewriting firmware's
            // live counters or flags with constructor defaults.
            self.retarget_render_parameter_buffer()
                .map_err(|error| Self::render_stage_fail("retarget-parameter-buffer", error))?;
        } else {
            if self.render_bind.is_some() {
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
                        descriptor_context_id,
                        parameter_page_list_vas,
                        command,
                        self.num_clusters,
                        command_buffer_state_words,
                        self.render_submission_ordinal,
                    )
                    .map_err(|error| Self::render_stage_fail("restage-new-vm-command", error))?;
            }
            // `new_user_render_storage` has just stamped the device-global PB
            // loader from this fresh graph, so record it as the owner without
            // repeating the write.
            self.render_parameter_buffer_owner =
                self.render_storage.as_ref().ok_or(ENODEV)?.render_pool_id();
        }
        // A fresh storage and its application-GART root are one reusable
        // graph. Install that ownership before the fallible direct secondary
        // pair registration. If power disappears before the transaction, the
        // outer pair is still unpublished, but the next submit can safely
        // reuse this complete graph instead of finding storage without its
        // root and returning EBUSY forever.
        if !retained {
            self.render_bind = Some(G17PRenderRootOwner::Job {
                context: execution_context.clone(),
            });
        }
        if crate::g17_config::RENDER_USC_FREELIST != 0 {
            let storage = self.render_storage.as_ref().ok_or(ENODEV)?;
            let owner_matches = self.bootstrap_uma.as_ref().is_some_and(|owner| {
                owner.matches_owner(
                    storage.render_flist_id(),
                    storage.render_flist_pointer(),
                    storage.render_flist_operand_table(),
                    storage.render_flist_cookie(),
                )
            });
            if !owner_matches {
                self.resources.stage_user_render_uma(storage)
                    .map_err(|error| Self::render_stage_fail("stage-render-uma-owner", error))?;
            }
            self.prepared_render_flist = Some(
                G17PBootstrapUmaLifecycle::acquire_owner(
                    self.bootstrap_uma.as_ref(),
                    self.next_render_flist_sequence,
                    storage.render_flist_id(),
                    storage.render_flist_pointer(),
                    storage.render_flist_operand_table(),
                    storage.render_flist_cookie(),
                    storage.render_flist_needs_grow(),
                )
                .map_err(|_| EBUSY)?,
            );
        }
        {
            let storage = self.render_storage.as_mut().ok_or(ENODEV)?;
            storage
                .set_parameter_buffer_lease_token(parameter_buffer_token)
                .map_err(|error| Self::render_stage_fail("set-pb-lease-token", error))?;
            if g17p_render_slot_globals() & G17P_RENDER_SLOT_GLOBAL_FLIST_GENERATION != 0 {
                // tag-15 +0x3e is the USC FList lifecycle generation. Publish
                // the sequence acquired for this retained owner, not its
                // submit ordinal. Independent slots carry independent
                // sequences with their FList state.
                storage.set_flist_generation(self.prepared_render_flist.map_or(
                    g17_submission::g17p_render_flist_generation(self.render_submission_ordinal),
                    |lifecycle| lifecycle.sequence,
                ));
            }
            if let Some(failed) = storage.first_unreachable(self.resources.uat(), vm) {
                storage.log_reachability(dev, self.resources.uat(), vm);
                dev_err!(
                    dev.as_ref(),
                    "G17P render: storage unreachable, first failing predicate is {}\n",
                    failed
                );
                return Err(EFAULT);
            }
        }
        if render_sksm != 0 && self.render_sksm.is_none() {
            let (pair, hardware_buffer_id, entry_vas) = {
                let storage = self.render_storage.as_ref().ok_or(ENODEV)?;
                (
                    storage.queue_pair(),
                    u8::try_from(storage.qos_hardware_buffer_id()?)
                        .map_err(|_| EOVERFLOW)?,
                    [
                        (
                            storage.sksm_entry_low_va(true),
                            storage.sksm_entry_high_va(true),
                        ),
                        (
                            storage.sksm_entry_low_va(false),
                            storage.sksm_entry_high_va(false),
                        ),
                    ],
                )
            };
            self.render_sksm = Some(
                self.new_render_sksm_queues(pair, hardware_buffer_id, entry_vas)
                    .map_err(|error| Self::render_stage_fail("new-render-sksm-queues", error))?,
            );
        }
        let (fragment_queue, tiling_queue, previous, late_tiling) = if render_sksm != 0 {
            self.stage_render_groups_with_sksm(
                dev,
                command.render_barrier != g17_uapi::UAPI_BARRIER_NONE,
            )
                .map_err(|error| Self::render_stage_fail("stage-render-groups-sksm", error))?
        } else {
            let storage = self.render_storage.as_mut().ok_or(ENODEV)?;
            storage
                .stage_fragment(self.render_submission_ordinal)
                .inspect_err(|error| {
                    dev_err!(dev.as_ref(), "G17P render: fragment stage failed ({:?})\n", error);
                })?;
            let previous = storage.snapshot(self.render_submission_ordinal).inspect_err(|error| {
                dev_err!(dev.as_ref(), "G17P render: initial snapshot failed ({:?})\n", error);
            })?;
            let late_tiling = storage
                .prepare_late_tiling_publication(self.render_submission_ordinal)
                .inspect_err(|error| {
                    dev_err!(dev.as_ref(), "G17P render: tiling stage failed ({:?})\n", error);
                })?;
            let (fragment_queue, tiling_queue) = storage.queue_gpu_vas();
            (fragment_queue, tiling_queue, previous, Some(late_tiling))
        };
        // What render's tag-15 ACTUALLY carries, both queues, every submit.
        // `+0x1a` is the queue-install flag: firmware RE says the per-queue slot
        // `0x127628 + qid*0x28` is written -- and the `0x40 tag15-programmed`
        // KTrace receipt emitted -- only when it is non-zero. Render's value has
        // been taken on trust from the spec; print it so it is a measurement.
        let fragment_header = self
            .render_storage
            .as_mut()
            .ok_or(ENODEV)?
            .fragment_config_update_header()
            .map_err(|error| Self::render_stage_fail("decode-fragment-tag15", error))?;
        let tiling_header = late_tiling
            .as_ref()
            .and_then(|publication| publication.config_update_header());
        let (fragment_install, tiling_install) = if render_sksm != 0 {
            let fragment = self
                .validate_render_firmware_install(false, fragment_header.install)
                .map_err(|error| {
                    Self::render_stage_fail("validate-fragment-tag15-install", error)
                })?;
            let tiling = tiling_header
                .map(|header| {
                    self.validate_render_firmware_install(true, header.install).map_err(|error| {
                        Self::render_stage_fail("validate-tiling-tag15-install", error)
                    })
                })
                .transpose()?;
            (fragment, tiling)
        } else {
            (false, None)
        };
        if crate::util::g17p_verbose() {
            dev_info!(
                dev.as_ref(),
                "G17P render tag15: 3D selector {:#x} qid {} install {} dm {} gfx {} ctx {} flist={:#x}/id{}/generation{:#x} scheduler-state {:#x} +0x56 {} entry {:#x}/{:#x} pb={:#x}/{:#x}/id{}/bit{}\n",
                fragment_header.selector,
                fragment_header.queue_id,
                fragment_header.install,
                fragment_header.data_master,
                fragment_header.graphics,
                fragment_header.context_id,
                fragment_header.flist_pointer,
                fragment_header.flist_id,
                fragment_header.flist_generation,
                fragment_header.scheduler_state,
                fragment_header.field_56,
                fragment_header.entry_low,
                fragment_header.entry_high,
                fragment_header.parameter_buffer_object,
                fragment_header.parameter_buffer_token,
                fragment_header.parameter_buffer_id,
                fragment_header.parameter_buffer_bit,
            );
        }
        if let Some(header) = tiling_header {
            if crate::util::g17p_verbose() {
                dev_info!(
                    dev.as_ref(),
                    "G17P render tag15: TA selector {:#x} qid {} install {} dm {} gfx {} ctx {} flist={:#x}/id{}/generation{:#x} scheduler-state {:#x} +0x56 {} entry {:#x}/{:#x} pb={:#x}/{:#x}/id{}/bit{}\n",
                    header.selector,
                    header.queue_id,
                    header.install,
                    header.data_master,
                    header.graphics,
                    header.context_id,
                    header.flist_pointer,
                    header.flist_id,
                    header.flist_generation,
                    header.scheduler_state,
                    header.field_56,
                    header.entry_low,
                    header.entry_high,
                    header.parameter_buffer_object,
                    header.parameter_buffer_token,
                    header.parameter_buffer_id,
                    header.parameter_buffer_bit,
                );
            }
        }

        // Native calls AGXQOSQueue::willSubmit before making the tag-15 item
        // visible. Populate the same QID-indexed firmware resource first. In
        // native fragment-first modes TA does not exist yet; its own record is
        // published by `publish_native_render_tiling_group` immediately
        // before that routine exposes the delayed TA tag-15 prefix.
        let mut qos_publications = [None; 2];
        let qos_result = (|| {
            let fragment_qos = self
                .resources
                .publish_render_qos_queue_record(fragment_header)
                .map_err(|error| Self::render_stage_fail("publish-fragment-qos", error))?;
            qos_publications[0] = Some(fragment_qos);
            if render_sksm != 0 {
                self.validate_render_queue_consistency(dev, false, fragment_header, fragment_qos.record)
                    .map_err(|error| {
                        Self::render_stage_fail("validate-fragment-qid-consistency", error)
                    })?;
            }
            if crate::util::g17p_verbose() {
                dev_info!(
                    dev.as_ref(),
                    "G17P render QoS: 3D qid {} record {:?}\n",
                    fragment_header.queue_id,
                    fragment_qos.record,
                );
            }
            if let Some(header) = tiling_header {
                let tiling_qos = self
                    .resources
                    .publish_render_qos_queue_record(header)
                    .map_err(|error| Self::render_stage_fail("publish-tiling-qos", error))?;
                qos_publications[1] = Some(tiling_qos);
                if render_sksm != 0 {
                    self.validate_render_queue_consistency(dev, true, header, tiling_qos.record)
                        .map_err(|error| {
                            Self::render_stage_fail("validate-tiling-qid-consistency", error)
                        })?;
                }
                if crate::util::g17p_verbose() {
                    dev_info!(
                        dev.as_ref(),
                        "G17P render QoS: TA qid {} record {:?}\n",
                        header.queue_id,
                        tiling_qos.record,
                    );
                }
            }
            // Construction and validation above are CPU-only and can take several
            // milliseconds on a dockchannel console.  Acquiring the powered KSMFE
            // aperture before that work left a timing race: firmware could enter
            // idle after the final power witness but before these four writes.
            // Worse, an EAGAIN here returned after the command/SKSM producers had
            // already been staged, so the userspace retry encountered a half-built
            // ordinal-zero graph and poisoned this physical pair for every later
            // client.  Native ensurePoweredHardware(true) brackets the actual
            // configureHardware transaction.  Do the same: reacquire immediately
            // at the transaction boundary and retry only the still-unpublished
            // four-write registration, without restaging the command.
            if queue_pair.slot() != 0 && (fragment_install || tiling_install.unwrap_or(false)) {
                let mut configured = false;
                for _ in 0..3 {
                    registration_power(self)
                        .map_err(|error| Self::render_stage_fail(
                            "power-secondary-render-pair", error,
                        ))?;
                    match self.configure_fresh_nonbootstrap_render_pair(
                        registers,
                        fragment_install,
                        tiling_install,
                    ) {
                        Ok(()) => {
                            configured = true;
                            break;
                        }
                        Err(error) if error == EAGAIN => continue,
                        Err(error) => return Err(Self::render_stage_fail(
                            "configure-secondary-render-pair", error,
                        )),
                    }
                }
                if !configured {
                    return Err(Self::render_stage_fail(
                        "configure-secondary-render-pair", EAGAIN,
                    ));
                }
            } else {
                self.configure_fresh_nonbootstrap_render_pair(
                    registers,
                    fragment_install,
                    tiling_install,
                )
                .map_err(|error| Self::render_stage_fail(
                    "configure-secondary-render-pair", error,
                ))?;
            }
            let pm_submission = self.render_storage.as_mut().ok_or(ENODEV)?
                .prepare_pm_submitted_operation()
                .map_err(|error| Self::render_stage_fail("prepare-pm-submitted-operation", error))?;
            let pm_submitted_count = pm_submission.as_ref().map(|operation| operation.next());
            // The TA/3D descriptors execute in their independently allocated
            // application context, and this VM's own TTBAT context was bound at
            // the top of this function -- both before anything here is visible to
            // firmware. Publication itself no longer touches the context table.
            //
            // The outer ring entry's "channel started" byte (`!chan[0x3c]` in
            // native's encodeAcceleratorRingCommand) belongs to the selected
            // AGXChannel. A fresh QID2/3 pair therefore publishes `1` even after
            // QID0/1 has run, while repeats on either pair publish `0`.
            let first_outer_submit =
                if g17p_render_slot_globals() & G17P_RENDER_SLOT_GLOBAL_CHANNEL_STARTED != 0 {
                    g17p_render_pair_first_outer_submit(
                        &self.render_outer_channels_started,
                        queue_pair.slot(),
                    )
                    .ok_or(EINVAL)?
                } else {
                    self.render_submission_ordinal == 0
                };
            let prepared = self
                .resources
                .publish_partial_opening_pair(
                    true,
                    queue_pair,
                    g17p_render_deferred_outer_mode(native_doorbell_mode, split_launch),
                    fragment_queue,
                    published_prefix,
                    tiling_queue,
                    published_prefix,
                    first_outer_submit,
                    move || {
                        if let Some(operation) = pm_submission {
                            operation.commit();
                        }
                    },
                    move || {
                        if let Some(publication) = late_tiling {
                            publication.publish();
                        }
                    },
                )
                .map_err(|error| Self::render_stage_fail("publish-partial-opening-pair", error))?;
            Ok((prepared, pm_submitted_count))
        })();
        let (prepared, pm_submitted_count) = match qos_result {
            Ok(published) => published,
            Err(error) => {
                // publish_partial_opening_pair acquires all mappings/checks
                // before either producer store and cannot fail afterwards.
                for ticket in qos_publications.into_iter().rev().flatten() {
                    self.resources.cancel_qos_publication(ticket)?;
                }
                return Err(error);
            }
        };
        self.commit_render_firmware_install(false, fragment_install);
        if let Some(install) = tiling_install {
            self.commit_render_firmware_install(true, install);
        }
        if crate::util::g17p_verbose() {
            dev_info!(dev.as_ref(), "G17P render PM: submitted-operations committed {:?}\n", pm_submitted_count);
        }
        // The ID is now firmware/GPU-owned. Any later validation failure must
        // retain it until semantic completion or a stopped-CPU teardown.
        self.render_hardware_buffer_published = true;
        self.render_outer_channels_started[usize::from(queue_pair.slot())] = true;
        if crate::util::g17p_verbose() {
            pr_info!(
                "G17P render: outer publish OK, retained={} root-slot={}\n",
                retained,
                render_root_slot,
            );
        }
        let roots = self.resources.uat().t8140_compute_context_roots();
        if crate::util::g17p_verbose() {
            dev_info!(
                dev.as_ref(),
                "G17P render context roots pre-kick: ctx0=[{:#x},{:#x}] ctx1=[{:#x},{:#x}] ctx2=[{:#x},{:#x}] ctx3=[{:#x},{:#x}]\n",
                roots[0].0,
                roots[0].1,
                roots[1].0,
                roots[1].1,
                roots[2].0,
                roots[2].1,
                roots[3].0,
                roots[3].1,
            );
        }
        // Prove the client regime reaches native's low view 0 before anything
        // is armed. gUPM's first fragment traffic writes 0x1e00..0x1f80
        // through this VM's own slot, and an unmapped write there is exactly
        // the fault this view exists to remove; a wrong or missing leaf must
        // fail here, on the host, rather than as another opaque dIPP miss.
        for offset in [0x1e00u64, 0x1f80u64] {
            let (canonical, client) = vm
                .t8140_client_null_view_probe(offset)
                .map_err(|error| {
                    dev_err!(
                        dev.as_ref(),
                        "G17P render client null view: +{:#x} does not translate ({:?})\n",
                        offset,
                        error,
                    );
                    Self::render_stage_fail("client-null-view-unmapped", error)
                })?;
            if canonical != client {
                dev_err!(
                    dev.as_ref(),
                    "G17P render client null view: +{:#x} client PA {:#x} != canonical PA {:#x}\n",
                    offset,
                    client,
                    canonical,
                );
                return Err(Self::render_stage_fail("client-null-view-mismatch", EFAULT));
            }
            if crate::util::g17p_verbose() {
                dev_info!(
                    dev.as_ref(),
                    "G17P render client null view: +{:#x} -> PA {:#x}, identical to canonical {:#x}+{:#x}\n",
                    offset,
                    client,
                    0x0010_0000_0000u64,
                    offset,
                );
            }
        }
        self.render_storage
            .as_mut()
            .ok_or(ENODEV)?
            .audit_fragment_rce_mapping(dev, vm)
            .map_err(|error| Self::render_stage_fail("fragment-rce-vm-audit", error))?;
        if g17p_native_fragment_first_mode(native_doorbell_mode) {
            fence(Ordering::Acquire);
            let current = self.render_storage.as_mut().ok_or(ENODEV)?
                .snapshot(self.render_submission_ordinal)?;
            let outer = self.resources.partial_opening_outer_counters()?;
            let fragment_sksm_visible = self
                .render_sksm
                .as_ref()
                .is_some_and(|queues| queues.fragment.producer.previous_valid);
            if !fragment_sksm_visible
                || !g17p_native_fragment_only_visible(
                    previous.tiling.write,
                    current.tiling.write,
                    current.fragment.write,
                    published_prefix,
                    outer[0][2],
                    prepared.fragment.next_producer,
                    outer[1][2],
                    prepared.tiling.slot_index,
                )
            {
                return Err(Self::render_stage_fail(
                    "native-fragment-only-visibility",
                    EINVAL,
                ));
            }
        }
        // A physical snapshot is a valid anchor only for the first in-flight
        // group.  While another group is active, TA and 3D can expose mixed
        // cursors from that predecessor (for example 3/6/9).  Anchoring the
        // queued tracker there makes its own modular completion window reject
        // the stale cursor forever.  Its immutable predecessor is instead the
        // active group's exact published target, including across ring wrap.
        let (tiling_previous, fragment_previous) =
            if let Some(active) = self.render_tracker.as_ref() {
                let active_target =
                    g17_resources::g17p_render_completion_window(active.ordinal)?.target();
                if active.tiling_prefix != active_target
                    || active.fragment_prefix != active_target
                {
                    return Err(Self::render_stage_fail(
                        "queued-render-active-target-mismatch",
                        EIO,
                    ));
                }
                let retired = g17_completion::QueueIndices {
                    done: active_target,
                    read: active_target,
                    write: active_target,
                };
                (retired, retired)
            } else {
                (previous.tiling, previous.fragment)
            };
        let hardware_buffer_id = self.render_hardware_buffer_id.ok_or(EIO)?;
        let command_buffer_state_word = self.take_active_render_command_buffer_state()?;
        let storage = self.render_storage.as_ref().ok_or(ENODEV)?;
        let pool_a_ticket = storage.current_pool_a_ticket();
        let context_update = storage.context_update_pending();
        let tracker = G17PPartialOpeningRenderTracker {
            ordinal: self.render_submission_ordinal,
            hardware_buffer_id,
            command_buffer_state_word,
            qos_pending: true,
            pool_a_record_index: pool_a_ticket.0,
            pool_a_submitted: pool_a_ticket.1,
            context_update,
            tiling_previous,
            fragment_previous,
            tiling_prefix: published_prefix,
            fragment_prefix: published_prefix,
        };
        if self.render_tracker.is_none() {
            self.render_tracker = Some(tracker);
        } else if self.render_queued_tracker.is_none() {
            self.render_queued_tracker = Some(tracker);
        } else {
            return Err(EIO);
        }
        if context_update {
            self.render_storage.as_mut().ok_or(ENODEV)?.complete_context_update();
        }
        Ok(prepared)
    }

    /// Expose the complete default-profile render pair after the USC FList
    /// update/grow has retired and before the one paired work doorbell.
    pub(crate) fn publish_default_render_pair_after_freelist(
        &mut self,
        prepared: &g17_submission::PreparedG17PPartialOpeningWorkPair,
    ) -> Result {
        if crate::g17_config::RENDER_NATIVE_DOORBELLS != 0
            || crate::g17_config::RENDER_SPLIT_LAUNCH != 0
            || self.render_tracker.is_none()
            || self.render_storage.is_none()
            || self.render_bind.is_none()
        {
            return Err(EINVAL);
        }
        self.resources.publish_deferred_partial_opening_pair(prepared)
    }

    /// Split launch's initial phase: expose only the complete TA outer slot
    /// after the same first-submit FList transaction has retired. The fragment
    /// prefix stays hidden until TA completion publishes its SKSM/AddKicks and
    /// matching deferred outer slot as one later transaction.
    pub(crate) fn publish_split_render_tiling_after_freelist(
        &mut self,
        plan: &g17_submission::G17PPartialOpeningOuterSlotPlan,
    ) -> Result {
        if crate::g17_config::RENDER_NATIVE_DOORBELLS != 0
            || crate::g17_config::RENDER_SPLIT_LAUNCH == 0
            || self.render_tracker.is_none()
            || self.render_storage.is_none()
            || self.render_bind.is_none()
        {
            return Err(EINVAL);
        }
        self.resources.publish_deferred_partial_opening_tiling(plan)
    }

    /// DRAM-only status-page sample; safe at any point, timeout included.
    /// Compute's status page, read exactly as render's is. Same VA.
    pub(crate) fn log_compute_status_pages(&mut self, label: &str) {
        self.resources.log_compute_status_pages(label);
    }

    /// Read both render SKSM entry regions back. Host memory we wrote the kick
    /// entries into, so a change here is direct evidence the firmware consumed
    /// them -- unlike 0x47, which only reports whether the scheduler was idling.
    /// Control for `log_render_sksm_entries`: the same readback on the compute
    /// queue, whose submissions provably complete.
    pub(crate) fn log_compute_sksm_entries(
        &mut self,
        queue: &mut G17PSksmQueue,
        label: &str,
    ) {
        let dev = self.resources.dev();
        let stride = queue.producer.geometry.entry_stride() as usize;
        queue.storage.log_entry(label, &dev, 0);
        queue.storage.log_entry(label, &dev, stride);
    }

    /// DIAGNOSTIC. Release the fragment's parent link (see
    /// `g17p_release_fragment_parent`).
    pub(crate) fn release_fragment_parent(&mut self, index: usize) -> Result<(u32, u32)> {
        self.resources.g17p_release_fragment_parent(index)
    }

    pub(crate) fn log_render_sksm_entries(&mut self, label: &str) {
        // DIAGNOSTIC ONLY, and it must stay that way. This runs from the
        // render submit path at "pre-doorbell" -- i.e. immediately before the
        // doorbell is rung -- and hexdumps roughly 19 KB per call once the two
        // descriptor regions grew to 0x2400. Leaving that in the default path
        // put a printk storm between building the kick and ringing it, and the
        // render came back VK_ERROR_DEVICE_LOST on BOTH the cold and the warm
        // path. Gate it exactly like log_render_status_pages just below.
        if crate::g17_config::DEP_RECORDS == 0 {
            return;
        }
        let strides = match self.render_sksm.as_ref() {
            Some(queues) => [
                queues.tiling.producer.geometry.entry_stride() as usize,
                queues.fragment.producer.geometry.entry_stride() as usize,
            ],
            None => [0usize, 0usize],
        };
        let dev = self.resources.dev();
        if let Some(storage) = self.render_storage.as_mut() {
            storage.log_render_queue_record(&dev, true, label);
            storage.log_render_queue_record(&dev, false, label);
        }
        let dumpdev = self.resources.dev();
        if let Some(storage) = self.render_storage.as_mut() {
            storage.dump_graph_region(&dumpdev, "opt-TA", label, 0x3000, 0xc0);
            storage.dump_graph_region(&dumpdev, "opt-3D", label, 0x30c0, 0xc0);
            storage.dump_graph_region(&dumpdev, "evt-TA", label, 0x3200, 0x40);
            storage.dump_graph_region(&dumpdev, "evt-3D", label, 0x3600, 0x40);
            storage.dump_descriptor_region(&dumpdev, "desc-TA", label, 0x0000, 0x2400);
            // Full fragment descriptor, not the first 0x200. Native's 3D command
            // record is captured to 0x2400 (recs3.txt), so dumping the same
            // extent makes a byte diff possible -- which is what "the fragment
            // executes but writes no pixels" now needs.
            storage.dump_descriptor_region(&dumpdev, "desc-3D", label, 0x4000, 0x2400);
            // b1 FUN_23004 selects the RENDER job state machine (FUN_15bcc,
            // which ends at the DM launch strobe FUN_15a48 -> 0x1c998 = 1)
            // when the command it builds the job from has +0x866 == 3. The
            // fragment is never launched, so read that byte on both stages.
            storage.dump_descriptor_region(&dumpdev, "kind-TA", label, 0x0840, 0x40);
            storage.dump_descriptor_region(&dumpdev, "kind-3D", label, 0x4840, 0x40);
            // b1 FUN_162f8, the 3D activation, is called as
            // FUN_162f8(sku+0x79c, sku+0x7a8) from the TILING job's state 7:
            // +0x79c names the fragment's QUEUE ID and +0x7a8 the fragment
            // kick entry's index. It uses exactly those two to find the entry
            // it patches, so if either is wrong in the tiling SKU the fragment
            // is never released. Dump that window of the tiling record.
            storage.dump_descriptor_region(&dumpdev, "link-TA", label, 0x0780, 0x40);
        }
        if let Some(storage) = self.render_storage.as_mut() {
            storage.dump_sksm_entry(&dumpdev, true, label);
            storage.dump_sksm_entry(&dumpdev, false, label);
            storage.log_sksm_entry(&dumpdev, true, label, 0);
            storage.log_sksm_entry(&dumpdev, true, label, strides[0]);
            storage.log_sksm_entry(&dumpdev, true, label, strides[0] + 0x120);
            storage.log_sksm_entry(&dumpdev, false, label, 0);
            storage.log_sksm_entry(&dumpdev, false, label, strides[1]);
            storage.log_sksm_entry(&dumpdev, false, label, strides[1] + 0x120);
        }
    }

    pub(crate) fn log_render_status_pages(&mut self, label: &str) {
        if !crate::util::g17p_verbose() {
            return;
        }
        if crate::g17_config::DEP_RECORDS != 0 {
            let dev = self.resources.dev();
            {
                let phys = G17P_GFX_PHYSICAL_DATA_BASE
                    + (G17P_GDIRECTOR_SHADOW_LINK_BASE - G17P_GFX_LINK_DATA_BASE);
                match pgtable::LiveFirmwareU64Probe::new(phys)
                    .and_then(|p| p.observe_mask(u64::MAX, 1))
                    .map(|o| o.first)
                {
                    Ok(v) => dev_info!(
                        dev.as_ref(),
                        "G17P gdirector-shadow[{}] link={:#x} raw={:#018x} hwbuf0={{prio {:#04x},share {:#04x}}} hwbuf1={{{:#04x},{:#04x}}} hwbuf2={{{:#04x},{:#04x}}} hwbuf3={{{:#04x},{:#04x}}}\n",
                        label,
                        G17P_GDIRECTOR_SHADOW_LINK_BASE,
                        v,
                        v & 0xff, (v >> 8) & 0xff,
                        (v >> 16) & 0xff, (v >> 24) & 0xff,
                        (v >> 32) & 0xff, (v >> 40) & 0xff,
                        (v >> 48) & 0xff, (v >> 56) & 0xff,
                    ),
                    Err(e) => dev_warn!(
                        dev.as_ref(),
                        "G17P gdirector-shadow[{}] unreadable ({:?})\n",
                        label,
                        e
                    ),
                }
            }
            {
                let rd = |link: u64| -> Option<u64> {
                    let phys = G17P_GFX_PHYSICAL_DATA_BASE
                        + (link - G17P_GFX_LINK_DATA_BASE);
                    pgtable::LiveFirmwareU64Probe::new(phys)
                        .and_then(|p| p.observe_mask(u64::MAX, 1))
                        .map(|o| o.first)
                        .ok()
                };
                for dm in 0u64..3 {
                    let a = rd(G17P_DM_ACTIVE_BITMAP_LINK_BASE + dm * 0x10);
                    let b = rd(G17P_DM_ACTIVE_BITMAP_LINK_BASE + dm * 0x10 + 8);
                    dev_info!(
                        dev.as_ref(),
                        "G17P dm-active[{}] dm={} lo={:#018x} hi={:#018x}\n",
                        label,
                        dm,
                        a.unwrap_or(u64::MAX),
                        b.unwrap_or(u64::MAX),
                    );
                }
                dev_info!(
                    dev.as_ref(),
                    "G17P ready-mask[{}] = {:#018x}\n",
                    label,
                    rd(G17P_READY_MASK_LINK_BASE).unwrap_or(u64::MAX),
                );
                for qid in [
                    4u64,
                    u64::from(g17_resources::g17p_render_queue_id(true)),
                    u64::from(g17_resources::g17p_render_queue_id(false)),
                ] {
                    let park = G17P_KSM_PARK_RECORD_LINK_BASE + qid * 0x28;
                    let counters = rd(park + 0x418);
                    let deadline = rd(park + 0x400);
                    // Same record through the SECOND ASC's data window. The
                    // scheduler pass may not run on the ASC whose base the
                    // other probes here use, and a per-queue deadline that
                    // reads identically for every qid says this one does not.
                    let rd1 = |link: u64| -> Option<u64> {
                        let phys = G17P_GFX1_PHYSICAL_DATA_BASE
                            + (link - G17P_GFX1_LINK_DATA_BASE);
                        pgtable::LiveFirmwareU64Probe::new(phys)
                            .and_then(|p| p.observe_mask(u64::MAX, 1))
                            .map(|o| o.first)
                            .ok()
                    };
                    let counters1 = rd1(park + 0x418);
                    let deadline1 = rd1(park + 0x400);
                    let raw1 = counters1.unwrap_or(u64::MAX);
                    let dl1 = deadline1.unwrap_or(u64::MAX);
                    dev_info!(
                        dev.as_ref(),
                        "G17P park-gfx1[{}] qid={} raw={:#018x} deadline={:#018x}\n",
                        label,
                        qid,
                        raw1,
                        dl1,
                    );
                    let raw = counters.unwrap_or(u64::MAX);
                    let byte_a = raw & 0xff;
                    let byte_b = (raw >> 8) & 0xff;
                    let byte_c = (raw >> 16) & 0xff;
                    let parked = byte_a == byte_b;
                    let dl = deadline.unwrap_or(u64::MAX);
                    dev_info!(
                        dev.as_ref(),
                        "G17P park[{}] qid={} A={} B={} C={} A==B(park)={} raw={:#018x} deadline={:#018x}\n",
                        label,
                        qid,
                        byte_a,
                        byte_b,
                        byte_c,
                        parked,
                        raw,
                        dl,
                    );
                }
                for qid in [
                    4u64,
                    u64::from(g17_resources::g17p_render_queue_id(true)),
                    u64::from(g17_resources::g17p_render_queue_id(false)),
                ] {
                    let st = rd(G17P_QUEUE_STATE_LINK_BASE + qid * 0x28);
                    let st2 = rd(G17P_QUEUE_STATE_LINK_BASE + qid * 0x28 + 8);
                    let fl = rd(G17P_QUEUE_FLAG_LINK_BASE + qid * 0x28 + 0x40);
                    dev_info!(
                        dev.as_ref(),
                        "G17P qstate[{}] qid={} +0={:#018x} +8={:#018x} flag+0x40={:#018x}\n",
                        label,
                        qid,
                        st.unwrap_or(u64::MAX),
                        st2.unwrap_or(u64::MAX),
                        fl.unwrap_or(u64::MAX),
                    );
                }
            }
            for qid in [
                4u8,
                g17_resources::g17p_render_queue_id(true) as u8,
                g17_resources::g17p_render_queue_id(false) as u8,
            ] {
                let link = G17P_DEPENDENCY_RECORD_LINK_BASE
                    + u64::from(qid) * G17P_DEPENDENCY_RECORD_STRIDE;
                let phys = G17P_GFX_PHYSICAL_DATA_BASE
                    + (link - G17P_GFX_LINK_DATA_BASE);
                let head = pgtable::LiveFirmwareU64Probe::new(phys)
                    .and_then(|p| p.observe_mask(u64::MAX, 1))
                    .map(|o| o.first);
                let stamp = pgtable::LiveFirmwareU64Probe::new(phys + 8)
                    .and_then(|p| p.observe_mask(u64::MAX, 1))
                    .map(|o| o.first);
                match (head, stamp) {
                    (Ok(head), Ok(stamp)) => dev_info!(
                        dev.as_ref(),
                        "G17P dep-record[{}] qid={} link={:#x} phys={:#x} head={:#018x} valid={} completed={:#018x}\n",
                        label,
                        qid,
                        link,
                        phys,
                        head,
                        head & 1,
                        stamp,
                    ),
                    _ => dev_warn!(
                        dev.as_ref(),
                        "G17P dep-record[{}] qid={} unreadable at phys={:#x}\n",
                        label,
                        qid,
                        phys,
                    ),
                }
            }
        }

        {
            let dev = self.resources.dev();
            {
                let mut pa = [0u8; 0x200];
                if self
                    .render_storage
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
        let dev = self.resources.dev();
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
        context: &crate::g17_resources::G17PLogicalQueueContext,
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
        let dev = self.resources.dev();
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
        let render_queues = self.render_sksm.as_ref().map(|queues| {
            [
                (
                    queues.tiling.queue_id as usize,
                    queues.tiling.producer.previous_timestamp,
                    queues.tiling.producer.geometry.entry_stride() as usize,
                ),
                (
                    queues.fragment.queue_id as usize,
                    queues.fragment.producer.previous_timestamp,
                    queues.fragment.producer.geometry.entry_stride() as usize,
                ),
            ]
        });
        if let Some([(tiling_qid, _, _), (fragment_qid, _, _)]) = render_queues {
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
            // Fault-only scheduler DATA.  The monitor/recovery notification is
            // already present when this runs, so these bounded reads cannot
            // perturb the causal execution window.  In particular, distinguish
            // a dependency that names an incomplete stamp from a queue that was
            // parked despite its dependency record reaching the requested value.
            if *module_parameters::g17p_fault_report.value() & 4 != 0 {
                let read = |link: u64| -> Result<u64> {
                    pgtable::LiveFirmwareU64Probe::new(g17p_gfx_link_to_physical(link))
                        .and_then(|probe| probe.observe_mask(u64::MAX, 1))
                        .map(|sample| sample.first)
                };
                for qid in [tiling_qid as u64, fragment_qid as u64] {
                    let dependency = G17P_DEPENDENCY_RECORD_LINK_BASE
                        + qid * G17P_DEPENDENCY_RECORD_STRIDE;
                    let park = G17P_KSM_PARK_RECORD_LINK_BASE + qid * 0x28;
                    dev_err!(
                        self.resources.dev().as_ref(),
                        "G17P render fault-only scheduler[{}]: qid={} dep(valid,completed,next)={:x?} park(deadline,counters)={:x?} owner={:x?} ready={:x?} qstate={:x?} qflag={:x?}\n",
                        label,
                        qid,
                        [read(dependency), read(dependency + 8), read(dependency + 16)],
                        [read(park + 0x400), read(park + 0x418)],
                        read(G17P_QID_OWNER_LINK_BASE + qid * 8),
                        read(G17P_READY_MASK_LINK_BASE),
                        [
                            read(G17P_QUEUE_STATE_LINK_BASE + qid * 0x28),
                            read(G17P_QUEUE_STATE_LINK_BASE + qid * 0x28 + 8),
                        ],
                        read(G17P_QUEUE_FLAG_LINK_BASE + qid * 0x28 + 0x40),
                    );
                }
            }
        }
        {
            let dev = self.resources.dev();
            if let Some(storage) = self.render_storage.as_mut() {
                match (
                    storage.tiling_config_update_header(),
                    storage.fragment_config_update_header(),
                ) {
                    (Ok(tiling), Ok(fragment)) => dev_err!(
                        dev.as_ref(),
                        "G17P render fault identity[{}]: ordinal={} TA(qid={},ctx={},flist={:#x}/id{}/generation{:#x},qos={},pb={:#x}/token{:#x}/id{}) 3D(qid={},ctx={},flist={:#x}/id{}/generation{:#x},qos={})\n",
                        label,
                        self.render_submission_ordinal,
                        tiling.queue_id,
                        tiling.context_id,
                        tiling.flist_pointer,
                        tiling.flist_id,
                        tiling.flist_generation,
                        tiling.field_56,
                        tiling.parameter_buffer_object,
                        tiling.parameter_buffer_token,
                        tiling.parameter_buffer_id,
                        fragment.queue_id,
                        fragment.context_id,
                        fragment.flist_pointer,
                        fragment.flist_id,
                        fragment.flist_generation,
                        fragment.field_56,
                    ),
                    (tiling, fragment) => dev_warn!(
                        dev.as_ref(),
                        "G17P render fault identity[{}]: tag15 decode failed TA={:?} 3D={:?}\n",
                        label,
                        tiling.err(),
                        fragment.err(),
                    ),
                }
                match storage.snapshot(self.render_submission_ordinal) {
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
                if *module_parameters::g17p_fault_report.value() & 4 != 0 {
                    if let Some([(_, ta_stamp, ta_stride), (_, fragment_stamp, fragment_stride)]) =
                        render_queues
                    {
                        let ta_offset = usize::try_from(
                            ta_stamp & g17_submission::KICK_STAMP_INDEX_MASK,
                        )
                        .unwrap_or(0)
                        .saturating_mul(ta_stride);
                        let fragment_offset = usize::try_from(
                            fragment_stamp & g17_submission::KICK_STAMP_INDEX_MASK,
                        )
                        .unwrap_or(0)
                        .saturating_mul(fragment_stride);
                        storage.log_sksm_entry(&dev, true, label, ta_offset);
                        storage.log_sksm_entry(
                            &dev,
                            false,
                            label,
                            fragment_offset,
                        );
                    }
                }
            }
        }
        self.log_render_status_pages(label);
        self.log_render_sksm_entries(label);
        if g17p_native_fragment_first_mode(
            crate::g17_config::RENDER_NATIVE_DOORBELLS,
        ) {
            self.log_render_outer_channels(label);
        }
        if let Err(error) = self.dump_completion_records(label) {
            let dev = self.resources.dev();
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
            dev_info!(
                self.resources.dev().as_ref(),
                "G17P render outer[{}]: {} table={} cursors(actual,coherency,producer)={:?} current-slot={} record={:#x} kind={} queue-byte={}\n",
                label,
                name,
                snapshot.table_index,
                snapshot.cursors,
                snapshot.current_slot_index,
                snapshot.current_slot_queue,
                snapshot.current_slot_kind,
                snapshot.current_slot_queue_id,
            );
        }
    }

    /// In native pre-ack mode, keep TA wholly unpublished until the primary
    /// firmware has synchronously drained the already-published Fragment outer
    /// group. The outer producer is host-owned and must remain at the expected
    /// value; only the firmware-owned actual consumer gates success. The middle
    /// coherency cursor is diagnostic because firmware can legally update it
    /// independently of the completed synchronous drain.
    pub(crate) fn wait_native_render_fragment_outer_consumed(
        &mut self,
        expected: u32,
    ) -> Result {
        const POLLS: usize = 100;
        const POLL_US: i64 = 100;

        if !g17p_native_fragment_preack_mode(
            crate::g17_config::RENDER_NATIVE_DOORBELLS,
        ) {
            return Err(EINVAL);
        }

        let mut last = [0; 3];
        for poll in 0..=POLLS {
            fence(Ordering::Acquire);
            last = self.resources.partial_opening_outer_counters()?[0];
            if last[2] != expected {
                dev_err!(
                    self.resources.dev().as_ref(),
                    "G17P render native pre-ack: Fragment producer drifted, expected {} cursors(actual,coherency,producer)={:?}\n",
                    expected,
                    last
                );
                return Err(EINVAL);
            }
            if g17p_native_fragment_outer_consumed(last, expected) {
                dev_info!(
                    self.resources.dev().as_ref(),
                    "G17P render native pre-ack: Fragment outer consumed after {} us, cursors(actual,coherency,producer)={:?}\n",
                    poll * POLL_US as usize,
                    last
                );
                return Ok(());
            }
            if poll != POLLS {
                fsleep(Delta::from_micros(POLL_US));
            }
        }

        dev_err!(
            self.resources.dev().as_ref(),
            "G17P render native pre-ack: Fragment outer was not consumed within {} us, expected {} cursors(actual,coherency,producer)={:?}; TA remains unpublished\n",
            POLLS * POLL_US as usize,
            expected,
            last
        );
        Err(ETIMEDOUT)
    }

    /// Observe the TA-owned execution timestamps for the opt-in split render
    /// launch. This is DRAM-only and does not advance either completion
    /// tracker: the ordinary paired wait still owns final retirement after the
    /// deferred fragment producer is published.
    pub(crate) fn split_render_tiling_completion(&mut self) -> Result<Option<[u64; 2]>> {
        if crate::g17_config::RENDER_SPLIT_LAUNCH == 0
            || self.render_tracker.is_none()
        {
            return Err(EINVAL);
        }
        fence(Ordering::Acquire);
        let state = self.render_storage.as_mut().ok_or(ENODEV)?
            .snapshot(self.render_submission_ordinal)?;
        if state.tiling_timestamps.iter().all(|value| *value != 0) {
            Ok(Some(state.tiling_timestamps))
        } else {
            Ok(None)
        }
    }

    /// Read both engine timestamp pairs without imposing the split-launch
    /// policy gate. This is used only by the post-doorbell KSM diagnostic.
    pub(crate) fn render_engine_timestamps(&mut self) -> Result<[[u64; 2]; 2]> {
        if self.render_tracker.is_none() {
            return Err(EINVAL);
        }
        fence(Ordering::Acquire);
        let state = self.render_storage.as_mut().ok_or(ENODEV)?
            .snapshot(self.render_submission_ordinal)?;
        Ok([state.tiling_timestamps, state.fragment_timestamps])
    }

    /// Publish the complete fragment group retained by a split launch after
    /// [`Self::split_render_tiling_completion`] has proved TA execution.
    /// Ordering is strict: SKSM entry, tag-14/AddKicks and inner producer, then
    /// outer slot and producer. The second doorbell is sent by the caller only
    /// after this returns.
    pub(crate) fn publish_split_render_fragment_group(
        &mut self,
        dev: &AsahiDevice,
        _registers: &regs::Resources,
        plan: &g17_submission::G17PPartialOpeningOuterSlotPlan,
    ) -> Result {
        if crate::g17_config::RENDER_SPLIT_LAUNCH == 0
            || self.render_tracker.is_none()
            || self.render_storage.is_none()
            || self.render_bind.is_none()
        {
            return Err(EINVAL);
        }
        let (ta_stamp, ta_queue_id) = {
            let queues = self.render_sksm.as_ref().ok_or(ENODEV)?;
            if !queues.tiling.producer.previous_valid {
                return Err(EINVAL);
            }
            (
                queues.tiling.producer.previous_timestamp,
                queues.tiling.queue_id,
            )
        };
        let barrier = [g17_submission::g17p_render_barrier_from_timestamp(
            ta_stamp, ta_queue_id,
        ).map_err(|_| EINVAL)?];
        let event_word = barrier[0].event_word;
        let suppress_barrier =
            crate::g17_config::RENDER_NO_BARRIER != 0;
        let barrier_slice: &[g17_submission::G17PClBarrierDependency] =
            if suppress_barrier { &[] } else { &barrier };
        if suppress_barrier {
            dev_info!(
                dev.as_ref(),
                "G17P render split-launch: DIAGNOSTIC barrier dependency suppressed (event_word {:#x} qid {})\n",
                event_word,
                ta_queue_id
            );
        }
        let fragment_queue = self
            .render_storage
            .as_ref()
            .ok_or(ENODEV)?
            .queue_gpu_vas()
            .0;
        let fragment_header = self
            .render_storage
            .as_mut()
            .ok_or(ENODEV)?
            .fragment_config_update_header()
            .map_err(|error| Self::render_stage_fail("split-decode-fragment-tag15", error))?;
        let fragment_install = self
            .validate_render_firmware_install(false, fragment_header.install)
            .map_err(|error| Self::render_stage_fail("split-validate-fragment-install", error))?;
        let fragment_stamp = self
            .publish_render_sksm_group(dev, false, fragment_queue, barrier_slice)
            .map_err(|error| Self::render_stage_fail("split-sksm-publish-fragment", error))?;
        self.render_storage
            .as_mut()
            .ok_or(ENODEV)?
            .stage_fragment_kick(self.render_submission_ordinal, fragment_stamp)
            .map_err(|error| Self::render_stage_fail("split-fragment-addkicks", error))?;
        fence(Ordering::SeqCst);
        let state = self.render_storage.as_mut().ok_or(ENODEV)?
            .snapshot(self.render_submission_ordinal)?;
        if state.fragment.write
            != g17_resources::g17p_render_published_prefix(self.render_submission_ordinal)?
        {
            return Err(Self::render_stage_fail("split-fragment-inner-prefix", EINVAL));
        }
        self.resources
            .publish_deferred_partial_opening_fragment(plan)
            .map_err(|error| Self::render_stage_fail("split-fragment-outer", error))?;
        self.commit_render_firmware_install(false, fragment_install);
        Ok(())
    }

    /// Complete the TA group retained by native render chronology modes 2/3.
    /// The caller has already sent the Fragment/priority-2 doorbell. Every
    /// fallible byte construction/VMap acquisition happens before the first TA
    /// inner producer is touched, then publication is strictly:
    ///
    /// tag-15 prefix -> SKSM entry -> tag-14/tag-16 inner producer ->
    /// outer producer -> late Pool-A/shared-inner transition.
    ///
    /// The caller may send the Vertex/priority-2 doorbell only after success.
    pub(crate) fn publish_native_render_tiling_group(
        &mut self,
        dev: &AsahiDevice,
        _registers: &regs::Resources,
        plan: &g17_submission::G17PPartialOpeningOuterSlotPlan,
    ) -> Result {
        if !g17p_native_fragment_first_mode(
            crate::g17_config::RENDER_NATIVE_DOORBELLS,
        )
            || crate::g17_config::RENDER_SPLIT_LAUNCH != 0
            || self.render_tracker.is_none()
            || self.render_storage.is_none()
            || self.render_bind.is_none()
            || self.render_sksm.is_none()
        {
            return Err(EINVAL);
        }
        let ordinal = self.render_submission_ordinal;
        let (tiling_queue, expected_stamp) = {
            let storage = self.render_storage.as_ref().ok_or(ENODEV)?;
            let queues = self.render_sksm.as_ref().ok_or(ENODEV)?;
            (
                storage.queue_gpu_vas().1,
                queues.tiling.producer.current_timestamp,
            )
        };

        // Complete every fallible preparation before publishing TA prefix 2.
        let late_tiling = self
            .render_storage
            .as_mut()
            .ok_or(ENODEV)?
            .prepare_late_tiling_publication_with_stamp(
                ordinal,
                expected_stamp,
            )
            .map_err(|error| Self::render_stage_fail("native-prepare-tiling", error))?;
        let mut order = G17PNativeRenderPublishOrder::after_fragment_doorbell();

        let tiling_header = late_tiling
            .config_update_header()
            .ok_or_else(|| Self::render_stage_fail("native-decode-tiling-tag15", EINVAL))?;
        let tiling_install = self
            .validate_render_firmware_install(true, tiling_header.install)
            .map_err(|error| Self::render_stage_fail("native-validate-tiling-install", error))?;
        dev_info!(
            dev.as_ref(),
            "G17P render native TA tag15: selector={:#x} entry=[{:#x},{:#x}] qid={} install={} dm={} graphics={} context={} flist=[{:#x},id={},generation={:#x}] scheduler={:#x} field56={} pb=[{:#x},{:#x},id={},bit={}]\n",
            tiling_header.selector,
            tiling_header.entry_low,
            tiling_header.entry_high,
            tiling_header.queue_id,
            tiling_header.install,
            tiling_header.data_master,
            tiling_header.graphics,
            tiling_header.context_id,
            tiling_header.flist_pointer,
            tiling_header.flist_id,
            tiling_header.flist_generation,
            tiling_header.scheduler_state,
            tiling_header.field_56,
            tiling_header.parameter_buffer_object,
            tiling_header.parameter_buffer_token,
            tiling_header.parameter_buffer_id,
            tiling_header.parameter_buffer_bit,
        );
        let tiling_qos = self
            .resources
            .publish_render_qos_queue_record(tiling_header)
            .map_err(|error| Self::render_stage_fail("native-publish-tiling-qos", error))?;
        self.validate_render_queue_consistency(dev, true, tiling_header, tiling_qos.record)
            .map_err(|error| {
                Self::render_stage_fail("native-validate-tiling-qid-consistency", error)
            }).or_else(|error| {
                self.resources.cancel_qos_publication(tiling_qos)?;
                Err(error)
            })?;
        if crate::util::g17p_verbose() {
            dev_info!(
                dev.as_ref(),
                "G17P render QoS: delayed native TA qid {} record {:?}\n",
                tiling_header.queue_id,
                tiling_qos.record,
            );
        }

        late_tiling.publish_native_prefix();
        order
            .advance(G17PNativeRenderPublishPhase::TilingPrefix)
            .map_err(|_| Self::render_stage_fail("native-order-prefix", EINVAL))?;
        fence(Ordering::SeqCst);

        let tiling_stamp = self
            .publish_render_sksm_group(dev, true, tiling_queue, &[])
            .map_err(|error| Self::render_stage_fail("native-sksm-publish-tiling", error))?;
        if tiling_stamp != expected_stamp {
            return Err(Self::render_stage_fail("native-tiling-stamp-mismatch", EINVAL));
        }
        order
            .advance(G17PNativeRenderPublishPhase::TilingSksm)
            .map_err(|_| Self::render_stage_fail("native-order-sksm", EINVAL))?;

        late_tiling.publish_native_kick();
        order
            .advance(G17PNativeRenderPublishPhase::TilingKick)
            .map_err(|_| Self::render_stage_fail("native-order-kick", EINVAL))?;
        fence(Ordering::SeqCst);
        let state = self.render_storage.as_mut().ok_or(ENODEV)?
            .snapshot(self.render_submission_ordinal)?;
        let published_prefix = g17_resources::g17p_render_published_prefix(ordinal)?;
        if state.tiling.write != published_prefix {
            return Err(Self::render_stage_fail("native-tiling-inner-prefix", EINVAL));
        }

        self.resources
            .publish_deferred_partial_opening_tiling(plan)
            .map_err(|error| Self::render_stage_fail("native-tiling-outer", error))?;
        order
            .advance(G17PNativeRenderPublishPhase::TilingOuter)
            .map_err(|_| Self::render_stage_fail("native-order-outer", EINVAL))?;
        fence(Ordering::Acquire);
        let outer = self.resources.partial_opening_outer_counters()?;
        if !g17p_native_tiling_fully_visible(
            state.tiling.write,
            published_prefix,
            outer[1][2],
            plan.next_producer,
        ) {
            return Err(Self::render_stage_fail("native-complete-ta-visibility", EINVAL));
        }

        late_tiling.publish_native_pool_transition();
        order
            .advance(G17PNativeRenderPublishPhase::PoolTransition)
            .map_err(|_| Self::render_stage_fail("native-order-pool", EINVAL))?;
        fence(Ordering::SeqCst);
        self.commit_render_firmware_install(true, tiling_install);
        Ok(())
    }

    /// One iteration of the render completion poll.
    ///
    /// Split out of `wait_translated_render_completion` so the runtime can
    /// interleave `service_firmware_events_inline` between polls. The compute
    /// submit does exactly that, and its poll loop carries the reason:
    /// "The submit path holds the runtime mutex for the whole wait, which is
    /// why this drains here rather than from an RTKit handler or a workqueue."
    /// Render's wait had no such interleave, so a firmware restart request
    /// raised during the wait stayed unread until the wait gave up.
    pub(crate) fn poll_translated_render_completion(
        &mut self,
    ) -> Result<G17PRenderPollOutcome> {
        {
            fence(Ordering::Acquire);
            let tracker = self.render_tracker.as_ref().ok_or(ENODEV)?;
            let ordinal = tracker.ordinal;
            let mut state = self.render_storage.as_mut().ok_or(ENODEV)?
                .snapshot(ordinal)?;
            // B1 unlinks EventControl only after its completed count reaches
            // its own submitted-count slot (168d0, KTrace 0x10b/0x10d).
            // Queue retirement and timestamps do not authorize host writes
            // to this firmware-owned list. Keep polling until it is empty.
            let tracker = self.render_tracker.as_ref().ok_or(ENODEV)?;
            let window = g17_resources::g17p_render_completion_window(ordinal)?;
            if window.target() != tracker.tiling_prefix || window.target() != tracker.fragment_prefix {
                dev_err!(
                    self.resources.dev().as_ref(),
                    "G17P render completion: ordinal {} window target {} disagrees with tracker TA/3D {}/{}\n",
                    ordinal,
                    window.target(),
                    tracker.tiling_prefix,
                    tracker.fragment_prefix,
                );
                return Ok(G17PRenderPollOutcome::Retry);
            }
            let tiling_previous = tracker.tiling_previous;
            let fragment_previous = tracker.fragment_previous;
            let execution = |state: &g17_resources::G17PUserRenderQueueState| {
                g17_completion::RenderExecutionState {
                    job_list_empty: state.job_list_empty,
                    tiling_timestamps: state.tiling_timestamps,
                    fragment_timestamps: state.fragment_timestamps,
                }
            };
            let observe = |state: &g17_resources::G17PUserRenderQueueState| {
                g17_completion::observe_paired_render_window_depth(
                    window,
                    g17_resources::G17P_RENDER_MAX_IN_FLIGHT as u32,
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
                        .snapshot(ordinal)?;
                    match observe(&retry) {
                        Ok(completion) => {
                            state = retry;
                            completion
                        }
                        Err(second_error) => {
                            dev_err!(
                                self.resources.dev().as_ref(),
                                "G17P render completion: ordinal {} target {} rejected two snapshots ({:?}/{:?}); previous TA {}/{}/{} 3D {}/{}/{} first TA {}/{}/{} 3D {}/{}/{} retry TA {}/{}/{} 3D {}/{}/{}\n",
                                ordinal,
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
                let event_record = tracker.pool_a_record_index;
                let event_submitted = tracker.pool_a_submitted;
                let hardware_buffer_id = tracker.hardware_buffer_id;
                let command_buffer_state_word = tracker.command_buffer_state_word;
                let context_update = tracker.context_update;
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
                    .render_event_control_state(event_record, event_submitted)?;
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
                // has one in-flight pair. The event value wraps as on M1/M2;
                // the previous completed value differs even across zero.
                // Require this exact generation, never a stale nonzero stamp.
                let pair = self.render_storage.as_ref().ok_or(ENODEV)?.queue_pair();
                let expected_stamp = ordinal
                    .checked_add(1)
                    .map(|value| (value as u32).wrapping_shl(8))
                    .ok_or(EOVERFLOW)?;
                let fragment_stamp = self.resources.render_fragment_completion_stamp(pair)?;
                let stamp_delta = fragment_stamp.wrapping_sub(expected_stamp);
                if stamp_delta & 0xff != 0 || stamp_delta >= 0x8000_0000 {
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
                        .ok_or(EINVAL)?;
                    let (tiling_qid, fragment_qid) = self.render_sksm.as_ref()
                        .map_or((fallback_pair.tiling, fallback_pair.fragment), |queues| (
                            u16::from(queues.tiling.queue_id),
                            u16::from(queues.fragment.queue_id),
                        ));
                    if crate::util::g17p_verbose() {
                        dev_info!(
                            self.resources.dev().as_ref(),
                            "G17P render: GPU-INTERVAL ordinal={} vm={} root={:#x} ta-qid={} fragment-qid={} start={} end={}\n",
                            ordinal,
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
                if context_update {
                    self.complete_render_context_update_retirement(true)?;
                }
                // Native TA/3D complete subtract completed kicks from queue
                // +44. Prepare both results before mutating either queue;
                // timestamps and previous-stamp history must never reset.
                let retired_sksm = self.render_sksm.as_ref().map(|queues| {
                    let ta = g17_submission::complete_g17p_cl_kicks(queues.tiling.producer, 1)?;
                    let fragment = g17_submission::complete_g17p_cl_kicks(queues.fragment.producer, 1)?;
                    Ok::<_, g17_submission::SubmissionError>((ta, fragment))
                }).transpose().map_err(|_| EIO)?;
                self.retire_tracked_render_qos()?;
                self.render_hardware_buffer_ids
                    .release(hardware_buffer_id)
                    .map_err(|_| EIO)?;
                self.release_render_command_buffer_state(command_buffer_state_word)?;
                self.render_storage.as_mut().ok_or(ENODEV)?
                    .retire_command_generation(ordinal)?;
                if let (Some(queues), Some((ta, fragment))) = (self.render_sksm.as_mut(), retired_sksm) {
                    queues.tiling.producer = ta;
                    queues.fragment.producer = fragment;
                }
                self.render_tracker = self.render_queued_tracker.take();
                if crate::util::g17p_verbose() {
                    dev_info!(
                        self.resources.dev().as_ref(),
                        "G17P render: released TA HardwareBufferID {} at paired completion\n",
                        hardware_buffer_id,
                    );
                }
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
                    self.log_render_context_witness(HEALTHY_RENDER_WITNESS);
                }
                return Ok(G17PRenderPollOutcome::Complete(completion));
            }
            tracker.tiling_previous = state.tiling;
            tracker.fragment_previous = state.fragment;
        }
        Ok(G17PRenderPollOutcome::Pending)
    }

    pub(crate) fn render_completion_masks(&self) -> Result<[u64; 2]> {
        self.render_storage
            .as_ref()
            .ok_or(ENODEV)?
            .queue_pair()
            .completion_masks()
            .ok_or(EINVAL)
    }

    /// Nominal render wait bound, expressed the way it was originally written:
    /// 2000 polls of one millisecond each.
    ///
    /// The waits are now deadline-driven (`g17p_render_wait_ms`, which
    /// defaults to exactly this many milliseconds) because the poll interval
    /// varies. The constant stays so the two cannot drift apart and so the
    /// timeout report still names a poll budget.
    pub(crate) const RENDER_COMPLETION_POLLS: usize = 2000;

    pub(crate) fn wait_translated_render_completion(
        &mut self,
    ) -> Result<g17_completion::PairedRenderCompletion> {
        let mut schedule = crate::g17_perf::PollSchedule::new(
            *module_parameters::g17p_render_poll_us.value(),
        );
        let deadline_ns =
            i64::from(*module_parameters::g17p_render_wait_ms.value()).saturating_mul(1_000_000);
        let started = kernel::time::Instant::<kernel::time::Monotonic>::now();
        loop {
            match self.poll_translated_render_completion()? {
                G17PRenderPollOutcome::Complete(completion) => return Ok(completion),
                // Re-poll immediately, do not sleep -- but still inside the
                // deadline. Without the old fixed poll count, an outcome that
                // never advances would spin this loop forever.
                G17PRenderPollOutcome::Retry => {
                    if started.elapsed().as_nanos() >= deadline_ns {
                        break;
                    }
                }
                G17PRenderPollOutcome::Pending => {
                    if started.elapsed().as_nanos() >= deadline_ns {
                        break;
                    }
                    fsleep(Delta::from_micros(i64::from(schedule.next_delay_us())));
                }
            }
        }
        self.report_render_wait_timeout("inline-wait-expired");
        Err(ETIMEDOUT)
    }

    /// Call only after this tracker's paired firmware retirement is proved.
    /// complete_qos_queue_records validates both exact QID/HBID/DM owners before
    /// changing either row. A successful call belongs to this tracker once,
    /// even if a later HWPB/command-generation cleanup step fails and retries.
    fn retire_tracked_render_qos(&mut self) -> Result {
        if !self.render_tracker.as_ref().ok_or(ENODEV)?.qos_pending {
            return Ok(());
        }
        let storage = self.render_storage.as_ref().ok_or(ENODEV)?;
        let pair = storage.queue_pair();
        let qos_id = u8::try_from(storage.qos_hardware_buffer_id()?).map_err(|_| EINVAL)?;
        self.resources.complete_qos_queue_records(&[
            (pair.tiling as u8, qos_id, 0),
            (pair.fragment as u8, qos_id, 1),
        ])?;
        // The runtime lock keeps this exact tracker selected through the
        // transaction; only the caller may promote its queued successor.
        self.render_tracker.as_mut().ok_or(ENODEV)?.qos_pending = false;
        Ok(())
    }

    /// After a render wait has expired: if the firmware has provably retired
    /// this group without executing it, perform the host bookkeeping a paired
    /// completion performs and report `true`; otherwise touch nothing and
    /// report `false`.
    ///
    /// "Provably retired" is the firmware's own word on it: both queue cursors
    /// at this group's published target, the job unlinked from the firmware's
    /// completion list, and no pass timestamp. The tiler stamps the pass start
    /// before any fragment work, so a retired, unlinked group with a zero start
    /// is a group the GPU never ran -- the shape `g17p_zero_duration` names for
    /// compute. The firmware owns nothing of such a group any more, so
    /// quarantining the slot for the rest of the session and pinning its
    /// HardwareBufferID bought nothing; the failing client is still told its
    /// work did not run.
    ///
    /// A group the firmware still holds -- cursors short of the target, or the
    /// job still linked -- is left exactly as the timeout found it.
    ///
    /// `accept_executed` drops ONLY the `observed()` term -- see
    /// `g17p_render_recovery_retire`. Both ownership proofs are retained in
    /// every mode: `completion.retired` (cursors at the published target) and
    /// `job_list_empty` (unlinked from the firmware completion list). Those two
    /// are what make the bookkeeping below safe to perform; `observed()` only
    /// ever described whether the GPU did any work, which does not bear on
    /// whether the firmware still owns the group.
    pub(crate) fn retire_abandoned_render_if_firmware_retired(
        &mut self,
        accept_executed: bool,
    ) -> Result<bool> {
        if crate::g17_config::RENDER_NOP_RETIRE == 0 {
            return Ok(false);
        }
        fence(Ordering::Acquire);
        let tracker = self.render_tracker.as_ref().ok_or(ENODEV)?;
        let ordinal = tracker.ordinal;
        let hardware_buffer_id = tracker.hardware_buffer_id;
        let command_buffer_state_word = tracker.command_buffer_state_word;
        let state = self.render_storage.as_mut().ok_or(ENODEV)?
            .snapshot(ordinal)?;
        let window =
            g17_resources::g17p_render_completion_window(ordinal)?;
        if window.target() != tracker.tiling_prefix || window.target() != tracker.fragment_prefix {
            return Err(EINVAL);
        }
        let execution = g17_completion::RenderExecutionState {
            job_list_empty: state.job_list_empty,
            tiling_timestamps: state.tiling_timestamps,
            fragment_timestamps: state.fragment_timestamps,
        };
        let completion = g17_completion::observe_paired_render_window_depth(
            window,
            g17_resources::G17P_RENDER_MAX_IN_FLIGHT as u32,
            tracker.tiling_previous,
            state.tiling,
            tracker.fragment_previous,
            state.fragment,
            execution,
        )
        .map_err(|_| EINVAL)?;
        if !completion.retired
            || !state.job_list_empty
            || (execution.observed() && !accept_executed)
        {
            return Ok(false);
        }
        // Twin of the paired-completion transaction in
        // `poll_translated_render_completion`; keep the two in step.
        let retired_sksm = self.render_sksm.as_ref().map(|queues| {
            let ta = g17_submission::complete_g17p_cl_kicks(queues.tiling.producer, 1)?;
            let fragment = g17_submission::complete_g17p_cl_kicks(queues.fragment.producer, 1)?;
            Ok::<_, g17_submission::SubmissionError>((ta, fragment))
        }).transpose().map_err(|_| EIO)?;
        self.retire_tracked_render_qos()?;
        self.render_hardware_buffer_ids
            .release(hardware_buffer_id)
            .map_err(|_| EIO)?;
        self.release_render_command_buffer_state(command_buffer_state_word)?;
        self.render_storage.as_mut().ok_or(ENODEV)?
            .retire_command_generation(ordinal)?;
        if let (Some(queues), Some((ta, fragment))) = (self.render_sksm.as_mut(), retired_sksm) {
            queues.tiling.producer = ta;
            queues.fragment.producer = fragment;
        }
        self.render_tracker = self.render_queued_tracker.take();
        let pair = self
            .render_storage
            .as_ref()
            .map(g17_resources::G17PUserRenderStorage::queue_pair);
        dev_warn!(
            self.resources.dev().as_ref(),
            "G17P render: firmware retired ordinal {} on slot {} (qids {:?}) [executed={}] -- tiling-ts [{:#x},{:#x}] fragment-ts [{:#x},{:#x}]; failing the submission and keeping the slot\n",
            ordinal,
            self.active_render_slot,
            pair.map(|pair| (pair.tiling, pair.fragment)),
            execution.observed(),
            state.tiling_timestamps[0],
            state.tiling_timestamps[1],
            state.fragment_timestamps[0],
            state.fragment_timestamps[1],
        );
        Ok(true)
    }

    /// Explain a render wait timeout. Split out with the poll body so both the
    /// plain and the event-servicing loops report identically.
    pub(crate) fn report_render_wait_timeout(&mut self, reason: &'static str) {
        let polls = Self::RENDER_COMPLETION_POLLS;
        // The graph remains pinned because firmware may still own it. Runtime
        // teardown stops both provider CPUs before manager resources drop.
        //
        // Say WHY the wait failed. Without this a render timeout is anonymous,
        // and the three stages it can stall in are indistinguishable:
        //
        //   read  < target       the firmware has not consumed this group
        //   done  < target       consumed but not retired
        //   timestamps still 0   retired while the GPU wrote nothing -- the
        //                        exact shape the compute tag-16 bug had
        //
        // Every field is a plain read of an object the driver itself allocated:
        // DRAM only, no sgx MMIO, which would take an asynchronous SError with
        // the GPU cores gated -- and by the time a submit has timed out they
        // are.
        let ordinal = self.render_tracker.as_ref()
            .map_or(self.render_submission_ordinal, |tracker| tracker.ordinal);
        let final_state = self
            .render_storage
            .as_mut()
            .and_then(|storage| storage.snapshot(ordinal).ok());
        let target =
            g17_resources::g17p_render_published_prefix(ordinal).ok();
        if let Some(state) = final_state {
            dev_err!(
                self.resources.dev().as_ref(),
                "G17P render: settle-reason {} (wait budget {} polls); ordinal {} target {:?} tiling done/read/write {}/{}/{} fragment {}/{}/{} job-list-empty {} job-list [first={:#x} last={:#x} head-va={:#x}] tiling-ts [{:#x},{:#x}] fragment-ts [{:#x},{:#x}]\n",
                reason,
                polls,
                ordinal,
                target,
                state.tiling.done,
                state.tiling.read,
                state.tiling.write,
                state.fragment.done,
                state.fragment.read,
                state.fragment.write,
                state.job_list_empty,
                state.job_list[0],
                state.job_list[1],
                state.job_list_head_va,
                state.tiling_timestamps[0],
                state.tiling_timestamps[1],
                state.fragment_timestamps[0],
                state.fragment_timestamps[1],
            );
        }
        self.log_render_context_witness(reason);
    }

    /// The firmware-side view of this render's context binding, labelled.
    ///
    /// Split out of `report_render_wait_timeout` so it can also be taken once
    /// from a render that COMPLETED. Every field the `context/qos` line prints
    /// has a driver-written cold-opening default, so a wedge sample on its own
    /// cannot distinguish "the firmware cleared this" from "the firmware never
    /// wrote it" -- only a healthy sample can.
    pub(crate) fn log_render_context_witness(&mut self, reason: &'static str) {
        // The healthy control is evidence, not an error: keep it out of
        // `dmesg --level=err` so a clean boot has no GPU error lines.
        let healthy = reason == HEALTHY_RENDER_WITNESS;
        macro_rules! witness {
            ($($arg:tt)*) => {
                if healthy {
                    dev_info!($($arg)*)
                } else {
                    dev_err!($($arg)*)
                }
            };
        }
        // The counter pair that decides whether the job gets unlinked at all.
        //
        // `poll_translated_render_completion` states the rule: "B1 unlinks
        // EventControl only after its completed count reaches its own
        // submitted-count slot". `unlinked()` is `completed == submitted &&
        // membership == 0`, and an unlinked EventControl is what leaves the
        // graph's job list empty -- so this pair is upstream of
        // `job_list_empty`, which is in turn the only term that decides
        // `firmware-retired`, which decides whether one client loses a frame or
        // the whole session is marked crashed.
        //
        // It was sampled nowhere on the settle path and only under
        // `g17p_verbose` on the Pending path, so the one number that explains
        // the split was never in a wedge capture.
        let reusable = self
            .render_storage
            .as_mut()
            .ok_or(ENODEV)
            .and_then(|storage| storage.pool_a_reusable_count());
        let event_ticket = self.render_tracker.as_ref()
            .map(|tracker| (tracker.pool_a_record_index, tracker.pool_a_submitted));
        match event_ticket.ok_or(ENODEV).and_then(|(record, submitted)| {
            self.render_storage.as_mut().ok_or(ENODEV)?.render_event_control_state(
                record,
                submitted,
            )
        })
        {
            Ok(event_control) => witness!(
                self.resources.dev().as_ref(),
                "G17P render event-control [{}]: record={} submitted={} slot-word={} completed={} lag={} membership={:#x} unlinked={} pool-a-reusable={:?}/35\n",
                reason,
                event_control.record_index,
                event_control.submitted,
                event_control.slot_word,
                event_control.completed,
                event_control.submitted.wrapping_sub(event_control.completed),
                event_control.membership,
                event_control.unlinked(),
                reusable,
            ),
            Err(error) => dev_err!(
                self.resources.dev().as_ref(),
                "G17P render event-control [{}]: read failed ({:?})\n",
                reason,
                error,
            ),
        }
        // What the firmware keeps in the per-queue stamp lanes both pairs'
        // records point at. A second pair whose lanes still read as the first
        // pair's is the measured retire-without-executing shape.
        if let Ok(lanes) = self.resources.render_stamp_lanes() {
            witness!(
                self.resources.dev().as_ref(),
                "G17P render: stamp lanes [{}] A(qid0..3)={:x?} B(qid0..3)={:x?}\n",
                reason,
                lanes[0],
                lanes[1],
            );
        }
        match self
            .render_storage
            .as_mut()
            .ok_or(ENODEV)
            .and_then(|storage| storage.fragment_word_20f0())
        {
            // Demoted to verbose: the healthy control proved it reads 0x0
            // identically on a paired completion and on a recovery-blamed
            // wedge, so it discriminates nothing and was pure noise on the
            // settle path.
            Ok(_) if !crate::util::g17p_verbose() => {}
            Ok(value) => dev_err!(
                self.resources.dev().as_ref(),
                "G17P render descriptor diagnostic [{}]: +0x20f0={:#010x} (DEAD WITNESS -- reads 0x0 identically on a healthy paired completion and on a recovery-blamed wedge, so it discriminates nothing; kept only so nobody re-derives it, do not reason from it)\n",
                reason,
                value,
            ),
            Err(error) => dev_err!(
                self.resources.dev().as_ref(),
                "G17P render descriptor diagnostic: +0x20f0 read failed ({:?})\n",
                error,
            ),
        }
        // ConfigUpdate +0x56 is the AGXQOSQueue HardwareBufferID, not a
        // submission counter. The firmware writes that byte to ctxdesc[0x33]
        // and maintains private ctx<->slot maps before programming MMIO
        // 0x103a0. The private maps are not AP-readable; this shared descriptor
        // is the exact host-safe witness for the same transaction.
        let context_qos = self.render_storage.as_mut().and_then(|storage| {
            let header = storage.fragment_config_update_header().ok()?;
            let tiling_header = storage.tiling_config_update_header().ok()?;
            let prefix = storage.render_channel_control_prefix().ok()?;
            Some((header, tiling_header, prefix))
        });
        if let Some((header, tiling_header, prefix)) = context_qos {
            let slot = header.field_56 as u8;
            let fragment_qos = self
                .resources
                .qos_queue_record(header.queue_id as u8)
                .ok();
            let tiling_qos = self
                .resources
                .qos_queue_record(tiling_header.queue_id as u8)
                .ok();
            let bitmap_lo = u64::from_le_bytes([
                prefix[0x12], prefix[0x13], prefix[0x14], prefix[0x15],
                prefix[0x16], prefix[0x17], prefix[0x18], prefix[0x19],
            ]);
            let bitmap_hi = u64::from_le_bytes([
                prefix[0x1a], prefix[0x1b], prefix[0x1c], prefix[0x1d],
                prefix[0x1e], prefix[0x1f], prefix[0x20], prefix[0x21],
            ]);
            witness!(
                self.resources.dev().as_ref(),
                "G17P render context/qos [{}]: requested-slot={} ctxdesc(slot={},ctx={},prio={},class={},state={},bound-slot={}) qid-bitmap={:#018x}/{:#018x} qos-records TA(qid {})={:?} 3D(qid {})={:?}\n",
                reason,
                slot,
                prefix[0],
                prefix[1],
                prefix[3],
                prefix[4],
                prefix[0x26],
                prefix[0x33],
                bitmap_lo,
                bitmap_hi,
                tiling_header.queue_id,
                tiling_qos,
                header.queue_id,
                fragment_qos,
            );
        }
    }

    pub(crate) fn sksm_scratch(&self) -> G17SksmScratchGeometry {
        self.platform.sksm_scratch
    }

    pub(crate) fn sksm_queue_geometry(&self) -> g17_submission::G17SksmQueueGeometry {
        self.platform.sksm_queue_geometry
    }

    /// Return the hardware QID of a queue owned by this manager.
    pub(crate) fn compute_queue_id(&self, queue: &G17PSksmQueue) -> Result<u8> {
        if queue.scratch != self.sksm_scratch() || queue.uat_owner != self.uat().ttb_base() {
            return Err(EINVAL);
        }
        Ok(queue.queue_id)
    }

    /// Prove that a prepared compute queue is still host-only and has never
    /// been registered, published, kicked, or assigned a command timestamp.
    /// Submission ordinal of the retained queue, for trace labelling.
    pub(crate) fn compute_queue_submission_ordinal(&self, queue: &G17PSksmQueue) -> u32 {
        queue.submission_ordinal
    }

    /// Whether the retained queue is past its first bind and can take a
    /// steady-state submission.
    pub(crate) fn compute_queue_is_installed(&self, queue: &G17PSksmQueue) -> bool {
        queue.hardware_state == G17PSksmQueueHardwareState::HardwareRegistered
            && queue.submission_ordinal != 0
            && queue.scratch == self.sksm_scratch()
            && queue.uat_owner == self.uat().ttb_base()
    }

    pub(crate) fn validate_prepublication_compute_queue(
        &self,
        queue: &G17PSksmQueue,
    ) -> Result {
        if queue.scratch != self.sksm_scratch()
            || queue.uat_owner != self.uat().ttb_base()
            || queue.hardware_state != G17PSksmQueueHardwareState::PreparedMemory
            || queue.submission_ordinal != 0
            || queue.producer.submitted != 0
            || queue.producer.current_timestamp != queue.producer.geometry.timestamp_seed()
            || queue.producer.last_add_kicks_timestamp
                != queue.producer.geometry.timestamp_seed()
            || queue.producer.base_timestamp != queue.producer.geometry.timestamp_seed()
            || queue.producer.previous_valid
            || queue.producer.previous_timestamp != 0
            || queue.producer.queue_id != queue.queue_id
            || !queue.direct_lifecycle.queue_config_dirty
            || queue.direct_lifecycle.cached_channel_identity != 0
            || queue.direct_lifecycle.next_correlation_token != 1
        {
            return Err(EBUSY);
        }
        Ok(())
    }

    pub(crate) fn bind_compute_user_vm(
        &mut self,
        queue: &mut G17PSksmQueue,
        vm: &mmu::Vm,
    ) -> Result<G17PComputeUserBinding> {
        if queue.scratch != self.sksm_scratch()
            || queue.uat_owner != self.uat().ttb_base()
        {
            return Err(EINVAL);
        }
        self.resources
            .cache_compute_vm_mappings(vm)
            .inspect_err(|error| {
            pr_info!("G17P compute VM: FList aliases failed {:?}\n", error);
        })?;
        let sksm_entries = queue.storage.map_user_entry_alias(vm).inspect_err(|error| {
            pr_info!("G17P compute VM: canonical SKSM entry alias {:#x} failed {:?}\n",
                queue.storage.entry_gpu_va(), error);
        })?;
        let flist = queue.storage.map_user_flist_alias(vm)?;
        let mappings = queue.compute.map_user_execution_state(vm).inspect_err(|error| {
            pr_info!("G17P compute VM: execution aliases failed {:?}\n", error);
        })?;
        // Bracketed to localise the ordering hang: after a failed render the
        // next compute goes silent for tens of seconds here. slotalloc's
        // get_inner is documented "Blocks if no slots are free", and T8140
        // limits the allocator to ONE user slot, so a retained binding from a
        // previous client blocks every later one.
        dev_info!(
            self.resources.dev().as_ref(),
            "G17P compute: acquiring UAT bind for vm\n"
        );
        let vm_bind = self.resources.uat().bind(vm)?;
        dev_info!(
            self.resources.dev().as_ref(),
            "G17P compute: UAT bind acquired, slot {}\n",
            vm_bind.slot()
        );
        // Slot 1 is where the VM is *bound*: the first-partial opening pins
        // the render/descriptor/optional-identity context there and the
        // root-phase machine reads `ttbs[0]`/`ttbs[1]` back verbatim. It is
        // not the context the kick names -- the compute submission declares
        // `context_id = 2`, which needs its own translation root.
        if vm_bind.slot() != 1 || !mappings.all_reachable_from(vm) {
            return Err(EFAULT);
        }
        // Publish that root before anything can be kicked. Without it GPU
        // context 2 has no lower and no upper root at all, and the first
        // firmware-half fetch the front end makes takes a Page Invalid read
        // fault with vm_slot = 2 (unit 0xd9, gUPM).
        self.resources
            .uat()
            .install_t8140_compute_context_alias(&vm_bind)?;
        if crate::g17_config::CTX2_ROOT != 0
            && !self
                .resources
                .uat()
                .t8140_compute_context_alias_ready(&vm_bind)
        {
            let roots = self.resources.uat().t8140_compute_context_roots();
            dev_err!(
                self.resources.dev().as_ref(),
                "G17P compute: context alias did not take (root={:#x}): ctx1 ttb0={:#x}, ctx2 ttb0={:#x} ttb1={:#x}, ctx3 ttb0={:#x} ttb1={:#x}\n",
                vm_bind.root(),
                roots[1].0,
                roots[2].0,
                roots[2].1,
                roots[3].0,
                roots[3].1,
            );
            return Err(EFAULT);
        }
        let (descriptor_high, descriptor_low) = queue.compute.descriptor_gpu_vas(0);
        let encoded_low = mappings.descriptor_gpu_va(0);
        if encoded_low != descriptor_low {
            return Err(EFAULT);
        }
        let high_pa = self.uat().kernel_vm().translate_iova(descriptor_high)?;
        let context0_pa = self
            .uat()
            .kernel_lower_vm()
            .translate_iova(descriptor_low)?;
        let context1_pa = vm.translate_iova(encoded_low)?;
        if high_pa != context0_pa || high_pa != context1_pa {
            dev_err!(
                self.resources.dev().as_ref(),
                "G17P descriptor aliases disagree high={:#x}->{:#x} context0-low={:#x}->{:#x} context1-low={:#x}->{:#x}\n",
                descriptor_high,
                high_pa,
                descriptor_low,
                context0_pa,
                encoded_low,
                context1_pa
            );
            return Err(EFAULT);
        }
        let entry_aliases = queue.storage.entry_aliases()?;
        if sksm_entries.iova() != entry_aliases.low()
            || sksm_entries.size() != entry_aliases.extent()
        {
            return Err(EFAULT);
        }
        let entry_high_pa = self
            .uat()
            .kernel_vm()
            .translate_iova(entry_aliases.high())?;
        let entry_context0_pa = self
            .uat()
            .kernel_lower_vm()
            .translate_iova(entry_aliases.low())?;
        let entry_context1_pa = vm.translate_iova(sksm_entries.iova())?;
        if entry_high_pa != entry_context0_pa || entry_high_pa != entry_context1_pa {
            dev_err!(
                self.resources.dev().as_ref(),
                "G17P SKSM entry aliases disagree high={:#x}->{:#x} context0-low={:#x}->{:#x} context1-low={:#x}->{:#x}\n",
                entry_aliases.high(),
                entry_high_pa,
                entry_aliases.low(),
                entry_context0_pa,
                sksm_entries.iova(),
                entry_context1_pa
            );
            return Err(EFAULT);
        }
        dev_info!(
            self.resources.dev().as_ref(),
            "G17P SKSM entry aliases: high={:#x} context0-low={:#x} context1-low={:#x} backing-pa={:#x} size={:#x}\n",
            entry_aliases.high(),
            entry_aliases.low(),
            sksm_entries.iova(),
            entry_high_pa,
            entry_aliases.extent()
        );
        dev_info!(
            self.resources.dev().as_ref(),
            "G17P descriptor aliases: high={:#x} context0-low={:#x} encoded/context1-low={:#x} backing-pa={:#x}\n",
            descriptor_high,
            descriptor_low,
            encoded_low,
            high_pa
        );
        Ok(G17PComputeUserBinding {
            mappings,
            _flist: flist,
            _sksm_entries: sksm_entries,
            _vm_bind: Some(vm_bind),
            _execution_context: None,
        })
    }

    pub(crate) fn bind_independent_compute_user_vm(
        &mut self, queue: &mut G17PSksmQueue, vm: &mmu::Vm,
        context: Arc<crate::g17_resources::G17PLogicalQueueContext>,
    ) -> Result<G17PComputeUserBinding> {
        if !context.matches(vm) || !context.is_current() { return Err(EFAULT); }
        // Independent compute roots execute the same firmware context-local
        // low-view accesses as render roots. Render binding already installs
        // this native SecureGart closure; omitting it for compute leaves
        // writes such as VA 0x1080 unmapped when CL_2 switches between QIDs.
        // The helper is idempotent for a VM shared by multiple queues and
        // retains the mapping for the VM lifetime.
        let aliases = self.resources.uat().install_t8140_job_context_aliases(vm)?;
        vm.append_driver_mappings(aliases)?;
        self.resources.cache_compute_vm_mappings(vm)?;
        let sksm_entries = queue.storage.map_user_entry_alias(vm)?;
        let flist = queue.storage.map_user_flist_alias(vm)?;
        let mappings = queue.compute.map_user_execution_state(vm)?;
        let (high, low) = queue.compute.descriptor_gpu_vas(0);
        if !mappings.all_reachable_from(vm) || mappings.descriptor_gpu_va(0) != low
            || vm.translate_iova(low)? != self.uat().kernel_vm().translate_iova(high)?
        { return Err(EFAULT); }
        queue.independent_context = true;
        Ok(G17PComputeUserBinding {
            mappings,
            _flist: flist,
            _sksm_entries: sksm_entries, _vm_bind: None,
            _execution_context: Some(context.clone()),
        })
    }

    /// Start one shared-ring transaction. All target queues are staged before
    /// the ring is read, so a coalesced event incurs one DRAM walk regardless
    /// of how many clients completed in the batch.
    pub(crate) fn begin_independent_compute_completion_batch(&mut self) {
        self.compute_completion_batch.clear();
    }

    pub(crate) fn stage_independent_compute_completion(
        &mut self,
        queue: &G17PSksmQueue,
    ) -> Result<bool> {
        if !queue.independent_context || queue.completion_descriptor == 0 {
            return Ok(false);
        }
        self.compute_completion_batch.request(
            queue.queue_id,
            queue.completion_descriptor,
            queue.completion_last_end,
        )?;
        Ok(true)
    }

    pub(crate) fn scan_independent_compute_completion_batch(&mut self) -> Result {
        self.resources
            .scan_compute_completion_batch(&mut self.compute_completion_batch)
    }

    /// Snapshot the terminal values published for the active compute command.
    /// This runs after the outer channel publication and before its RTKit
    /// notifications are sent, while the runtime mutex excludes completion and
    /// recovery handling.
    pub(crate) fn capture_independent_compute_retirement_ticket(
        &mut self,
        queue: &mut G17PSksmQueue,
    ) -> Result<G17PComputeRetirementTicket> {
        if !queue.independent_context
            || queue.completion_descriptor == 0
            || queue.producer.submitted != 1
        {
            return Err(EIO);
        }
        let [target, _completed] = queue.compute.read_compute_submitted_counters()?;
        let ring = queue.compute.read_compute_ring_state()?;
        if target != queue.submission_ordinal || ring.count == 0 || ring.producer >= ring.count {
            return Err(EIO);
        }
        Ok(G17PComputeRetirementTicket {
            queue_id: queue.queue_id,
            descriptor: queue.completion_descriptor,
            submitted_target: target,
            item_producer: ring.producer,
            item_count: ring.count,
        })
    }

    /// Consume this queue's record from the single acquired batch snapshot.
    /// Completion identity and visibility remain queue-local: the snapshot is
    /// only an index, and retirement still validates the exact descriptor,
    /// monotonic end timestamp, and final queue stamp before releasing owners.
    pub(crate) fn poll_staged_independent_compute_completion(
        &mut self,
        queue: &mut G17PSksmQueue,
        ticket: &G17PComputeRetirementTicket,
    ) -> Result<Option<[u64; 2]>> {
        if !queue.independent_context || queue.completion_descriptor == 0 {
            return Ok(None);
        }
        if ticket.queue_id != queue.queue_id
            || ticket.descriptor != queue.completion_descriptor
            || ticket.item_count == 0
        {
            return Err(EIO);
        }
        // Two different failures leave this function as a bare EIO, and the
        // caller turns either one into `session-crashed
        // reason=async-compute-completion-unclassifiable` -- terminal, because
        // the rebuild it arms ends in rtkit-boot ETIME and ENODEV. They want
        // opposite responses and the log could not tell them apart. Name them.
        let candidate = match self.compute_completion_batch.take(
            queue.queue_id,
            queue.completion_descriptor,
            queue.completion_last_end,
        ) {
            Ok(Some(candidate)) => candidate,
            Ok(None) => return Ok(None),
            Err(error) => {
                // The batch slot is indexed by queue_id and was staged by
                // `request()` with this queue's descriptor and last-consumed
                // end. A mismatch means the slot's identity moved underneath
                // us -- which a graph rebound to a new owner without reinstall
                // can do, since the QID is retained across the handoff.
                dev_err!(self.resources.dev().as_ref(),
                    "G17P async compute: UNCLASSIFIABLE qid={} cause=batch-slot-identity descriptor={:#x} last-end={} ({:?})\n",
                    queue.queue_id, queue.completion_descriptor,
                    queue.completion_last_end, error);
                return Err(error);
            }
        };
        let timestamps = candidate.timestamps;
        if timestamps[0] == timestamps[1] {
            // Retired without executing. This is the DOCUMENTED post-recovery
            // outcome -- `g17p_zero_duration` says so in as many words: "every
            // submission after a fault-blamed firmware recovery retires with
            // duration 0" -- and that parameter's default 1 is "fail the
            // submission ... but do NOT rebuild the firmware session". This
            // path never consulted it and killed the device instead.
            G17P_MARK_ZERO_DURATION.fetch_add(1, Ordering::Relaxed);
            self.log_compute_usage_slot(queue, "zero-duration");
            dev_err!(self.resources.dev().as_ref(),
                "G17P async compute: zero-duration qid={} descriptor={:#x} timestamps=[{},{}] -- retired without executing, the expected shape after a firmware recovery\n",
                queue.queue_id, queue.completion_descriptor,
                timestamps[0], timestamps[1]);
            // ENODATA, not EIO, when the parameter is enabled: its default of 1
            // means "fail the submission but do NOT rebuild the firmware
            // session", and returning EIO here made the caller escalate to
            // `note_session_recovery_pending`, which is exactly the rebuild the
            // parameter says not to do. The caller treats ENODATA as
            // submission-scoped. Setting the parameter to 0 restores the old
            // device-wide behaviour byte for byte.
            if crate::g17_config::ZERO_DURATION != 0 {
                return Err(ENODATA);
            }
            return Err(EIO);
        }
        if queue.completion_stamp_armed && queue.compute.read_compute_stamp()? == 0 {
            return Err(EAGAIN);
        }
        let [target, completed] = queue.compute.read_compute_submitted_counters()?;
        let ring = queue.compute.read_compute_ring_state()?;
        if ring.count != ticket.item_count {
            return Err(EIO);
        }
        let consumer = ring.consumer % ring.count;
        let producer = ring.producer % ring.count;
        if target != ticket.submitted_target
            || completed != ticket.submitted_target
            || producer != ticket.item_producer
            || consumer != ticket.item_producer
            || queue.producer.submitted != 1
        {
            return Err(EAGAIN);
        }
        if let Some(pair) = candidate.interval {
            if crate::util::g17p_verbose() {
                dev_info!(self.resources.dev().as_ref(),
                    "G17P async compute: GPU-INTERVAL qid={} descriptor={:#x} start={} end={}\n",
                    queue.queue_id, queue.completion_descriptor, pair[0], pair[1]);
            }
        }
        if crate::g17_config::COMPUTE_RESOURCE_SNAPSHOT != 0
            && queue.producer.previous_valid
        {
            let last = g17_submission::prepare_kick_submission(
                queue.producer.previous_timestamp,
                queue.queue_id,
                queue.producer.geometry.stamp_count(),
                queue.producer.geometry.entry_stride(),
            ).map_err(|_| EINVAL)?;
            let entry = queue.storage.snapshot_compute_entry(last.entry_offset as usize)?;
            dev_info!(self.resources.dev().as_ref(),
                "G17P B1 entry retirement: qid={} stamp={:#x} offset={:#x} header={:#x} word08={:#x} rce-kind-word={:#x} rce0={:#x} rce1={:#x} rce2={:#x} rce3={:#x}\n",
                queue.queue_id, queue.producer.previous_timestamp,
                last.entry_offset, entry[0], entry[1], entry[41], entry[42],
                entry[43], entry[44], entry[45]);
        }
        self.log_compute_usage_slot(queue, "retired");
        self.resources.complete_qos_queue_records(&[
            (queue.queue_id, queue.qos_hardware_buffer_id(), 2),
        ])?;
        let retired = g17_submission::complete_g17p_cl_kicks(queue.producer, 1)
            .map_err(|_| EINVAL)?;
        if retired.submitted != 0 {
            return Err(EIO);
        }
        queue.producer = retired;
        queue.completion_last_end = timestamps[1];
        queue.completion_descriptor = 0;
        queue.completion_stamp_armed = false;
        Ok(Some(timestamps))
    }

    /// Allocate a stable graph identity and lease an empty slot in the common
    /// table. Constructor failure returns the slot, but never reuses identity.
    fn allocate_compute_queue_id(&mut self) -> Result<crate::g17_channel::Lease> {
        use crate::g17_channel::{ChannelId, Engine};
        let identity = self.next_channel_identity;
        let next = identity.checked_add(1).ok_or(EOVERFLOW)?;
        let lease = self.channel_ids.acquire(ChannelId(identity), Engine::Compute, Some(G17P_COMPUTE_QUEUE_ID_BASE))
            .ok_or(ENOSPC)?;
        self.next_channel_identity = next;
        Ok(lease)
    }

    /// Allocate and encode the native compute queue without touching the SKSM
    /// hardware bridge. The dirty first direct submitBuffer asks firmware to
    /// install the QID through tag 15 once startup is complete.
    pub(crate) fn prepare_logical_queue_context(
        &mut self, dev: &AsahiDevice, vm: &mmu::Vm,
    ) -> Result<Arc<g17_resources::G17PLogicalQueueContext>> {
        // QUEUE_CREATE runs in the owning process, before asynchronous dispatch.
        let owner_pid = u32::try_from(kernel::current!().group_leader().pid())
            .map_err(|_| EOVERFLOW)?;
        if self.logical_qos_ids.is_none() {
            // Keep bootstrap QoS IDs 0/1 out of the logical namespace.
            // Native AGXQOSManager::init supplies 128 slots to its allocator;
            // unlike the PB namespace, QoS table index 127 is usable.
            self.logical_qos_ids = Some(Arc::pin_init(kernel::new_mutex!(
                3u128, "G17 logical QoS IDs"
            ), GFP_KERNEL)?);
        }
        let qos = g17_resources::G17PLogicalQosLease::new(
            self.logical_qos_ids.as_ref().ok_or(EIO)?.clone(),
        )?;
        let execution = self.uat().allocate_t8140_compute_execution_context(vm)?;
        let control = Arc::new(g17_resources::G17PClChannelControl::new(dev, self.uat())?, GFP_KERNEL)?;
        let context = Arc::new(g17_resources::G17PLogicalQueueContext::new(
            execution, control, qos, owner_pid,
        ), GFP_KERNEL)?;
        Ok(context)
    }

    /// Retain once, before a render graph or CL binding can name this page.
    /// All callers hold the runtime mutex, so the marker describes this exact
    /// Arc's membership without scanning the session list on repeated binds.
    pub(crate) fn retain_logical_channel_control(
        &mut self, control: &Arc<g17_resources::G17PClChannelControl>,
    ) -> Result {
        if control.is_session_retained() { return Ok(()); }
        self.logical_channel_controls.reserve(1, GFP_KERNEL)?;
        self.logical_channel_controls.push(control.clone(), GFP_KERNEL)?;
        control.set_session_retained(true);
        Ok(())
    }

    fn release_logical_channel_controls_after_processor_stop(&mut self) {
        for control in &self.logical_channel_controls {
            control.set_session_retained(false);
        }
        self.logical_channel_controls.clear();
    }

    /// Diagnostic isolation of the execution ID from cross-engine firmware
    /// caches. Scheduler/QoS/process ownership stays with the logical queue.
    /// This consumes another root-table slot; it is not a production fix.
    pub(crate) fn prepare_compute_execution_context(
        &mut self,
        context: &Arc<g17_resources::G17PLogicalQueueContext>,
        split: bool,
    ) -> Result<Arc<g17_resources::G17PLogicalQueueContext>> {
        if !split { return Ok(context.clone()); }
        if !context.is_current() { return Err(EFAULT); }
        let execution = self.uat().allocate_t8140_compute_execution_context(context.vm())?;
        if !execution.matches(context.vm()) || execution.context_id() == context.context_id() {
            return Err(EIO);
        }
        let compute = Arc::new(g17_resources::G17PLogicalQueueContext::new(
            execution, context.channel_control.clone(), context.qos.clone(), context.owner_pid,
        ), GFP_KERNEL)?;
        dev_info!(
            self.resources.dev().as_ref(),
            "G17P split-execution-ID probe: pid={} vm={} root={:#x} render-context={} compute-context={} scheduler={:#x} qos={} -- diagnostic only; extra root slot, no execution gating\n",
            context.owner_pid, context.vm_id(), context.root(), context.context_id(),
            compute.context_id(), compute.channel_control_gpu_va(), compute.qos.id(),
        );
        Ok(compute)
    }

    /// A prepared graph may be dropped on close only while its exact lease
    /// remains unpublished. Hardware-state alone is insufficient: publication
    /// can fail after the lease was sealed but before that state advances.
    pub(crate) fn cancel_unpublished_compute_queue(&mut self, queue: &G17PSksmQueue) -> bool {
        queue.requires_first_install()
            && self.channel_ids.cancel_unpublished(queue.channel_lease)
    }

    pub(crate) fn prepare_g17p_compute_queue(
        &mut self,
        dev: &AsahiDevice,
    ) -> Result<G17PSksmQueue> {
        let channel_lease = self.allocate_compute_queue_id()?;
        let queue_id = channel_lease.qid;
        let hardware_buffer_id = 1;
        let result = (|| {
        const COMPLETION_SELECTOR: u8 = 2;

        let geometry = self.sksm_queue_geometry();
        let flist = Arc::new(
            g17_resources::G17PComputeFlist::new(dev, self.uat(), queue_id)?, GFP_KERNEL,
        )?;
        self.resources.initialize_compute_pool_uma(&flist)?;
        let storage = g17_resources::G17PClRuntimeStorage::new(
            dev,
            self.uat(),
            geometry,
            flist,
        )?;
        if self.compute_channel_control.is_none() {
            self.compute_channel_control =
                Some(g17_resources::G17PClChannelControl::new(dev, self.uat())?);
        }
        let channel_control_gpu_va = self
            .compute_channel_control
            .as_ref()
            .ok_or(EIO)?
            .gpu_va();
        let compute = g17_resources::G17PComputeDescriptorStorage::new(
            dev,
            self.uat(),
            channel_control_gpu_va,
        )?;
        let routing = g17_submission::G17PComputeQueueRouting {
            queue_id,
            completion_selector: COMPLETION_SELECTOR as u32,
            entry_gpu_va: storage.entry_gpu_va(),
            geometry,
        };
        let config =
            g17_submission::prepare_g17p_compute_sksm_queue_config(routing, self.sksm_scratch())
                .map_err(|_| EINVAL)?;

        Ok(G17PSksmQueue {
            queue_id: routing.queue_id,
            channel_lease,
            hardware_buffer_id,
            scratch: self.sksm_scratch(),
            uat_owner: self.uat().ttb_base(),
            configure_pair: config.write_pair(),
            enable_pair: config.enable_pair,
            disable_pair: config.disable_pair,
            hardware_state: G17PSksmQueueHardwareState::PreparedMemory,
            storage,
            channel_control_gpu_va,
            logical_channel_control: None,
            logical_qos: None,
            compute,
            producer: geometry
                .initial_producer_state(
                    queue_id,
                    COMPLETION_SELECTOR,
                    g17_submission::G17PClQosConfig {
                        queue_byte_32: 1,
                        class_40: 8,
                        word_48: 0xffff,
                    },
                )
                .map_err(|_| EINVAL)?,
            direct_lifecycle: g17_submission::G17PDirectClLifecycleState {
                queue_config_dirty: true,
                cached_channel_identity: 0,
                next_correlation_token: 1,
            },
            configure_guard_accelerator_stamp: 0,
            configure_guard_event_stamp: 0,
            published_config: None,
            outer_channel_started: false,
            submission_ordinal: 0,
            independent_context: false,
            completion_descriptor: 0,
            completion_last_end: 0,
            completion_stamp_armed: false,
            publication_resource_snapshot: None,
        })
        })();
        // No firmware publication occurs in this constructor. A failed host
        // allocation must not consume the finite QID namespace permanently.
        if result.is_err() {
            if !self.channel_ids.cancel_unpublished(channel_lease) { return Err(EIO); }
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
        if !self.channel_ids.matches(queue.channel_lease) { return Err(EIO); }
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
        if queue.hardware_state == G17PSksmQueueHardwareState::PreparedMemory {
            return Ok(());
        }

        if registers.g17p_cancel_deferred_sksm_registration(queue.configure_pair) {
            queue.hardware_state = G17PSksmQueueHardwareState::PreparedMemory;
            return Ok(());
        }
        registers.g17p_sksm_publish_write_pair(queue.disable_pair)?;
        queue.hardware_state = G17PSksmQueueHardwareState::PreparedMemory;
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
        if queue.hardware_state != G17PSksmQueueHardwareState::HardwareRegistered
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
        if g17p_compute_sksm_entry_enabled() {
            queue.storage.write_entry(
                prepared.entry_offset as usize,
                prepared.zero_length as usize,
                &prepared.entry,
            )?;
        }

        self.publish_compute_b2_once(queue)?;

        fence(Ordering::SeqCst);
        queue.producer = prepared.producer_after;
        let timestamp = prepared.current_timestamp;
        queue.storage.reserve_flist_submission()?;
        self.add_sksm_kicks(registers, queue, 1)?;
        queue.producer.last_add_kicks_timestamp = timestamp;
        fence(Ordering::SeqCst);
        Ok(timestamp)
    }

    fn publish_compute_b2_once(&mut self, _queue: &mut G17PSksmQueue) -> Result {
        self.resources.stage_compute_region_views()?;
        if self.compute_b2_activation == g17_submission::G17PClB2ActivationState::Pending {
            let bootstrap_control = self.compute_channel_control.as_ref().ok_or(EIO)?.gpu_va();
            let activation = self.compute_b2_activation;
            let published = self.resources.with_b2_computed_mut(|computed| {
                g17_submission::apply_g17p_cl_b2_computed_state(
                    activation,
                    computed,
                    bootstrap_control,
                )
                .map_err(|_| EINVAL)
            })?;
            self.compute_b2_activation = published;
        }
        Ok(())
    }

    /// The binary-verified first-bind chronology: tag 3 -> 15 -> QID
    /// configure -> SKSM entry -> 14 -> 16 -> CL_2, ending with the queue
    /// registered on the SKSM bridge. It is shape-independent -- the command
    /// only supplies the CDM range, USC window, sampler state and timestamps
    /// that `build_compute_descriptor` consumes -- so any well-formed compute
    /// command may drive it, not just the add3 probe.
    fn submit_translated_first_bind_channel(
        &mut self,
        _registers: &regs::Resources,
        queue: &mut G17PSksmQueue,
        submission_phase: &AtomicU32,
        binding: &G17PComputeUserBinding,
        command: &g17_uapi::TranslatedComputeCommand,
        context: G17PComputeSubmissionContext<'_>,
        add3_buffers: Option<[u64; 3]>,
    ) -> Result<g17_submission::PreparedG17PDirectClNotifications> {
        if queue.submission_ordinal != 0 {
            return Err(EINVAL);
        }
        // This release gate is deliberately first: any future contract
        // regression must fail before descriptor, ring, SKSM, MMIO, or mailbox
        // state changes.
        g17_submission::require_g17p_first_bind_direct_hardware_release(
            g17_submission::G17P_FIRST_BIND_COMMAND_PLAN,
        )
        .map_err(|_| ENOTSUPP)?;

        let descriptor_slot = 0;
        let (descriptor_high, _) = queue.compute.descriptor_gpu_vas(descriptor_slot);
        let descriptor_low = binding.mappings.descriptor_gpu_va(descriptor_slot);
        // The +0x14a0 operand table belongs to the add3 *shader*, not to the
        // firmware. Register 0x1a510 names the retained preempt object and the
        // hand-written USC binary loads input A, input B and output out of it;
        // the constant 0x14a0 is not an offset anywhere in the b1 firmware
        // image, so no firmware path reads the structure. A Honeykrisp
        // dispatch carries its bindings in the command stream instead (root
        // descriptor pointer plus its Ioto uniform ranges), so it neither
        // needs nor reads the table. Write it only for the flagged probe. The
        // object itself is allocated, mapped and named by 0x1a510 either way,
        // and 0x1a4d0..0x1a4e8 keep pointing at preempt+0x1480..+0x1498, whose
        // contents the proven add3 run also leaves entirely zero.
        match add3_buffers {
            Some(buffers) => queue.compute.write_g17p_add3_resource_table(buffers)?,
            None => dev_info!(
                self.resources.dev().as_ref(),
                "G17P compute: generic first-bind, no add3 operand table; 0x1a510 still names the retained preempt object\n"
            ),
        }

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
            usage_record: queue.compute.compute_usage_record_gpu_va(),
        };
        queue.compute.clear_compute_usage_slot()?;
        let resume_checkpoints = queue.compute.with_descriptor_mut(descriptor_slot, |raw| {
            g17_uapi::build_compute_descriptor(
                command,
                g17_uapi::ComputeInternalState {
                    preempt_base: user_layout.preempt,
                    dispatch_identity: context.dispatch_identity,
                    context_id: context.context_id,
                    work_ordinal: 0,
                    robustness: user_layout.robustness,
                    operand_state_base: user_layout.operand_state,
                    usc_flist_id: u32::from(queue.storage.flist_id()),
                },
                objects,
                g17_compute::ComputeDescriptorMetadata {
                    submit_sequence: 0,
                    context_id: context.context_id,
                    grid_index: u32::from(queue.queue_id),
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
        queue
            .compute
            .with_descriptor_mut(descriptor_slot, |raw| {
                self.log_record("descriptor", &raw[..raw.len().min(320)]);
                Ok(())
            })?;

        let primary_checkpoint = u8::try_from(resume_checkpoints[0]).map_err(|_| EINVAL)?;
        let command_state = g17_submission::prepare_g17p_compute_cl_command_state(
            descriptor_low,
            u64::from(queue.submission_ordinal),
            primary_checkpoint,
        )
        .map_err(|_| EINVAL)?;
        let prepared_entry = g17_submission::prepare_g17p_cl_kick_entry(
            queue.producer,
            g17_submission::G17PClKickEntryOperands {
                descriptor_flag_4c: false,
                descriptor_flag_5c8: command.scs_enabled(),
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
                queue.channel_control_gpu_va,
                queue.queue_id,
                queue.qos_hardware_buffer_id(),
                install_queue,
                binding.declared_context_id(),
                queue.flist_generation(),
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
        if *module_parameters::g17p_fault_report.value() & 1 != 0 {
            dev_info!(
                self.resources.dev().as_ref(),
                "G17P rec tag15: install_queue={} descriptor_high={:#x} shared_control={:#x} channel_control={:#x} group(queue={:#x} optional={:#x} event={:#x} write_index={})\n",
                install_queue,
                descriptor_high,
                shared_control,
                queue.channel_control_gpu_va,
                group.queue,
                group.optional,
                group.event,
                group.write_index,
            );
        }
        self.arm_compute_completion(queue, descriptor_high)?;
        self.publish_compute_b2_once(queue)?;
        submission_phase.store(G17P_SUBMIT_PHASE_B2, Ordering::Release);
        let qos_publication = self.resources.publish_compute_qos_queue_record(
            queue.queue_id,
            queue.qos_hardware_buffer_id(),
            queue.channel_control_gpu_va,
        )?;
        let flist_target = queue.storage.reserve_flist_submission().or_else(|error| {
            self.resources.cancel_qos_publication(qos_publication)?;
            Err(error)
        })?;
        if let Err(error) = queue.compute.publish_initial_config_update_pointer(group) {
            queue.storage.cancel_unpublished_flist_submission(flist_target)?;
            self.resources.cancel_qos_publication(qos_publication)?;
            return Err(error);
        }
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
        if crate::g17_config::COMPUTE_RESOURCE_SNAPSHOT != 0 {
            let entry = queue.storage.snapshot_compute_entry(
                prepared_entry.entry_offset as usize,
            )?;
            dev_info!(self.resources.dev().as_ref(),
                "G17P B1 entry publication: qid={} stamp={:#x} offset={:#x} header={:#x} word08={:#x} rce-kind-word={:#x} rce0={:#x} rce1={:#x} rce2={:#x} rce3={:#x}\n",
                queue.queue_id, prepared_entry.current_timestamp,
                prepared_entry.entry_offset, entry[0], entry[1], entry[41],
                entry[42], entry[43], entry[44], entry[45]);
        }
        queue.producer = prepared_entry.producer_after;
        submission_phase.store(G17P_SUBMIT_PHASE_SKSM_ENTRY, Ordering::Release);
        let descriptor_generation =
            g17_submission::advance_g17p_compute_descriptor_generation(0, 0)
                .map_err(|_| EINVAL)?;
        queue.direct_lifecycle = lifecycle.next;
        // Native's publication order is tag 15 -> 14 -> 16; ours has been
        // 15 -> 16 -> 14. Selectable so both can be tested from one build.
        let native_order = crate::g17_config::NATIVE_ORDER != 0;
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

        let skip = crate::g17_config::SKIP;
        if native_order {
            // Native's firmware-transport order is tag 3 -> 15 -> 14 -> 16.
            // Tag 16's handler ends with a broadcast MMIO[0x210e0]/[0x210d8]
            // = -1 "go" poke, so it must follow AddKicks, not precede it.
            // publish_initial_config_update_pointer leaves the ring producer
            // at 2. group.write_index is staged as 3 for the default order,
            // where tag 16 advances 2 -> 3 first; in native order tag 14 takes
            // slot 2 itself.
            let defer_kicks = crate::g17_config::DEFER_ADD_KICKS != 0;
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
        // Check exact graph ownership before firmware can observe this route.
        // Retain on any ambiguous publication failure; do not return its QID.
        if !self.channel_ids.publish(queue.channel_lease) { return Err(EIO); }
        let prepared = self
            .resources
            .publish_compute_channel(
                group.queue,
                group.write_index,
                queue.queue_id,
                queue.outer_channel_started,
            )?;
        queue.outer_channel_started = true;
        submission_phase.store(G17P_SUBMIT_PHASE_CL2, Ordering::Release);
        // The complete install group is now visible to firmware. From this
        // point a retry must use the retained route; publishing another
        // install for the same QID could reprogram a live queue.
        queue.hardware_state = G17PSksmQueueHardwareState::HardwareRegistered;
        queue.published_config = Some(g17_compute::ComputeConfigIdentity {
            channel_control: queue.channel_control_gpu_va,
            qos_id: queue.qos_hardware_buffer_id(),
            context_id: binding.declared_context_id(),
        });
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
        if !self.channel_ids.matches(queue.channel_lease) { return Err(EIO); }
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
        // Descriptor ordinals split into two fields that behave differently.
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
        // Advancing work_ordinal broke the first repeat because it changed the
        // context word. Sequence advancement also fails current hardware smoke,
        // so preserve the established steady-state descriptor values while the
        // actual concurrent scheduler-stall cause is investigated separately.
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
            usage_record: queue.compute.compute_usage_record_gpu_va(),
        };
        queue.compute.clear_compute_usage_slot()?;
        let resume_checkpoints = queue.compute.with_descriptor_mut(descriptor_slot, |raw| {
            g17_uapi::build_compute_descriptor(
                command,
                g17_uapi::ComputeInternalState {
                    preempt_base: user_layout.preempt,
                    dispatch_identity: context.dispatch_identity,
                    context_id: context.context_id,
                    work_ordinal,
                    robustness: user_layout.robustness,
                    operand_state_base: user_layout.operand_state,
                    usc_flist_id: u32::from(queue.storage.flist_id()),
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
        let primary_checkpoint = u8::try_from(resume_checkpoints[0]).map_err(|_| EINVAL)?;
        let command_state = g17_submission::prepare_g17p_compute_cl_command_state(
            descriptor_low,
            0,
            primary_checkpoint,
        )
        .map_err(|_| EINVAL)?;
        let prepared_entry = g17_submission::prepare_g17p_cl_kick_entry(
            queue.producer,
            g17_submission::G17PClKickEntryOperands {
                descriptor_flag_4c: context.descriptor_flag_4c,
                descriptor_flag_5c8: command.scs_enabled(),
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
        match crate::g17_config::REPEAT_CONTEXT {
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
        let rewind = crate::g17_config::REPEAT_RING == 0;
        let config_identity = g17_compute::ComputeConfigIdentity {
            channel_control: queue.channel_control_gpu_va,
            qos_id: queue.qos_hardware_buffer_id(),
            context_id: binding.declared_context_id(),
        };
        let updates = config_identity.updates_from(queue.published_config);
        let group = queue.compute.stage_repeat_config_update(
            sksm_entry_aliases,
            shared_control,
            queue.channel_control_gpu_va,
            queue.queue_id,
            queue.qos_hardware_buffer_id(),
            rewind,
            updates,
            binding.declared_context_id(),
            queue.flist_generation(),
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
        if crate::g17_config::REPEAT_ACTIVATION != 0
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
        let flist_target = queue.storage.reserve_flist_submission()?;
        let after_command = self.publish_repeat_record(
            queue,
            "the tag-3 command pointer",
            producer_before,
            G17PRepeatRecord::Pointer(descriptor_high),
        ).or_else(|error| {
            queue.storage.cancel_unpublished_flist_submission(flist_target)?;
            Err(error)
        })?;
        submission_phase.store(G17P_SUBMIT_PHASE_B2, Ordering::Release);
        let qos_publication = self.resources.publish_compute_qos_queue_record(
            queue.queue_id,
            queue.qos_hardware_buffer_id(),
            queue.channel_control_gpu_va,
        )?;
        let after_config = self.publish_repeat_record(
            queue,
            "the tag-15 ConfigUpdate pointer",
            after_command,
            G17PRepeatRecord::Pointer(group.optional),
        ).or_else(|error| {
            self.resources.cancel_qos_publication(qos_publication)?;
            Err(error)
        })?;
        submission_phase.store(G17P_SUBMIT_PHASE_TAG15, Ordering::Release);

        if g17p_compute_sksm_entry_enabled() {
            queue.storage.write_entry(
                prepared_entry.entry_offset as usize,
                prepared_entry.zero_length as usize,
                &prepared_entry.entry,
            )?;
        }
        fence(Ordering::SeqCst);
        if crate::g17_config::COMPUTE_RESOURCE_SNAPSHOT != 0 {
            let entry = queue.storage.snapshot_compute_entry(
                prepared_entry.entry_offset as usize,
            )?;
            dev_info!(self.resources.dev().as_ref(),
                "G17P B1 entry publication: qid={} stamp={:#x} offset={:#x} header={:#x} word08={:#x} rce-kind-word={:#x} rce0={:#x} rce1={:#x} rce2={:#x} rce3={:#x}\n",
                queue.queue_id, prepared_entry.current_timestamp,
                prepared_entry.entry_offset, entry[0], entry[1], entry[41],
                entry[42], entry[43], entry[44], entry[45]);
        }
        queue.producer = prepared_entry.producer_after;
        submission_phase.store(G17P_SUBMIT_PHASE_SKSM_ENTRY, Ordering::Release);

        let descriptor_generation =
            g17_submission::advance_g17p_compute_descriptor_generation(0, 0)
                .map_err(|_| EINVAL)?;
        let add_kicks = g17_submission::prepare_g17p_ksm_add_kicks_command(
            g17_submission::G17PKsmAddKicksCommand {
                queue_id: queue.queue_id,
                add_count: descriptor_generation,
                queue_priority: queue.producer.completion_selector,
                entry_stamp: prepared_entry.current_timestamp,
            },
        )
        .map_err(|_| EINVAL)?;
        let after_kicks = self.publish_repeat_record(
            queue,
            "the tag-14 AddKicks record",
            after_config,
            G17PRepeatRecord::AddKicks(&add_kicks.bytes),
        )?;
        queue.producer.last_add_kicks_timestamp = prepared_entry.current_timestamp;
        fence(Ordering::SeqCst);
        submission_phase.store(G17P_SUBMIT_PHASE_ADD_KICKS, Ordering::Release);
        let write_index = self.publish_repeat_record(
            queue,
            "the tag-16 EntrySignal record",
            after_kicks,
            G17PRepeatRecord::EntrySignal(&entry_signal),
        )?;
        submission_phase.store(G17P_SUBMIT_PHASE_TAG16, Ordering::Release);

        // Depth 320 used to fail BEFORE reaching this point: the host-side
        // publisher compared the ring transition's `(producer + 1) % count`
        // against a locally computed `producer + 1`, so the fourth record of
        // submission 320 -- the one that lands on the ring's last slot -- was
        // refused with EBUSY. That is fixed in `publish_command_pointer_at` and
        // its two siblings, so a wrapped write index now actually reaches the
        // outer channel. Whether the firmware reads a producer of 0 as
        // "wrapped" or as "empty" is a separate, still-open question, and
        // `g17p_ring_wrap=1` reports the head as `count` instead so one boot
        // decides it. With `g17p_ring_limit=32` the wrap arrives on submission
        // 8, so that boot costs 20 iterations rather than 400.
        let reported_write_index = if write_index == 0
            && crate::g17_config::RING_WRAP != 0
        {
            let entries = queue.compute.compute_ring_entries()?;
            dev_info!(
                self.resources.dev().as_ref(),
                "G17P compute: item-ring producer wrapped to 0; reporting write index {} to the outer channel\n",
                entries
            );
            entries
        } else {
            write_index
        };
        let prepared = self
            .resources
            .publish_compute_channel(group.queue, reported_write_index, queue.queue_id, true)?;
        submission_phase.store(G17P_SUBMIT_PHASE_CL2, Ordering::Release);
        let _ = prepared;
        queue.direct_lifecycle = lifecycle.next;
        queue.submission_ordinal = next_ordinal;
        queue.published_config = Some(config_identity);
        // Optional settle. The 319-deep build padded every submission with
        // several console writes here; this makes that padding explicit and
        // measurable instead of an accident of log verbosity.
        let settle = crate::g17_config::REPEAT_SETTLE_US;
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
    /// Descriptor and CL-entry slots share the configured stamp lifetime. The
    /// descriptor is written first, then its upper address and the retained
    /// queue record are placed in the CL payload before the ordered SKSM kick.
    pub(crate) fn submit_translated_compute(
        &mut self,
        registers: &regs::Resources,
        queue: &mut G17PSksmQueue,
        submission_phase: &AtomicU32,
        binding: &G17PComputeUserBinding,
        command: &g17_uapi::TranslatedComputeCommand,
        context: G17PComputeSubmissionContext<'_>,
    ) -> Result<Option<g17_submission::PreparedG17PDirectClNotifications>> {
        self.log_mixed_resource_census(queue, false);
        if queue.scratch != self.sksm_scratch()
            || queue.uat_owner != self.uat().ttb_base()
        {
            return Err(EINVAL);
        }
        // Prove the kick's translation regime before anything is published.
        //
        // The kick declares hardware context 2, and every later workload
        // context 3 (`T8140_COMPUTE_CTXS`). If those entries do not name this
        // binding's VM the GPU has no root to walk at all: the work is
        // accepted and silently never runs -- no fault, no blamed queue, a
        // zero-duration retire, and a destination buffer that comes back
        // untouched. That is indistinguishable from "the shader did nothing",
        // which is exactly how it stayed hidden.
        //
        // `install_t8140_compute_context_alias` runs only on the bind that
        // creates a client's retained binding. Every repeat submission on that
        // binding, and any submission after another VM was created or
        // destroyed in between, previously reached the kick with nobody having
        // re-checked those two entries.
        if let Some(execution) = binding._execution_context.as_ref() {
            if context.context_id != execution.context_id() || !execution.is_current() {
                return Err(EFAULT);
            }
            if crate::util::g17p_verbose() {
                dev_info!(self.resources.dev().as_ref(),
                    "G17P independent compute root: qid={} context={} vm={} root={:#x}\n",
                    queue.queue_id, execution.context_id(), execution.vm_id(), execution.root());
            }
        } else {
        self.resources
            .uat()
            .ensure_t8140_compute_context_alias(binding.vm_bind())?;
        {
            let roots = self.resources.uat().t8140_compute_context_roots();
            dev_info!(
                self.resources.dev().as_ref(),
                "G17P vm-audit(pre-kick): vm {} root={:#x} ctx1 ttb0={:#x} ctx2 ttb0={:#x} ttb1={:#x} ctx3 ttb0={:#x} ttb1={:#x}\n",
                binding.vm_bind().vm_id(),
                binding.vm_bind().root(),
                roots[1].0,
                roots[2].0,
                roots[2].1,
                roots[3].0,
                roots[3].1,
            );
        }
        }
        // Which chronology a command needs is a property of the *queue*, not
        // of the command's shape. A queue that has never been configured on
        // the SKSM bridge needs the first-bind sequence above; one that has
        // needs the post-registration sequence below. `g17p_add3_buffers` only
        // says whether the probe wants its private operand table written into
        // the preempt object, so it must not be what decides the route -- that
        // coupling is exactly why a Honeykrisp dispatch, which carries its
        // bindings in the command stream and therefore sets no flag and
        // declares no attachments, fell through to the ENOTSUPP gate below
        // while the add3 probe reached the firmware.
        let first_bind = queue.hardware_state == G17PSksmQueueHardwareState::PreparedMemory
            && queue.submission_ordinal == 0;
        // Which of the three chronologies this submit takes. A submit that
        // produces no record dump at all took a path with no logging, so name
        // the route before branching -- otherwise "no output" is ambiguous
        // between "different path" and "never got here".
        if *module_parameters::g17p_fault_report.value() & 1 != 0 {
            let attachments = command.attachments.as_slice();
            let route = if first_bind {
                "first-bind"
            } else if queue.hardware_state
                != G17PSksmQueueHardwareState::HardwareRegistered
            {
                "REFUSED-not-registered"
            } else if queue.submission_ordinal != 0 {
                "repeat"
            } else {
                "steady-state"
            };
            dev_info!(
                self.resources.dev().as_ref(),
                "G17P rec route={} hardware_state={:?} submission_ordinal={} first_bind={} add3_proof={}\n",
                route,
                queue.hardware_state,
                queue.submission_ordinal,
                first_bind,
                command.g17p_add3_buffers.is_some(),
            );
            dev_info!(
                self.resources.dev().as_ref(),
                "G17P rec uapi: usc_exec_base={:#x} cs=[{:#x},{:#x}) term={:#x} sampler={:#x}/{} attachments={}\n",
                command.usc_exec_base,
                command.control_stream_base,
                command.control_stream_end,
                command.control_stream_terminator,
                command.sampler_heap,
                command.sampler_count,
                attachments.len(),
            );
            for (index, entry) in attachments.iter().enumerate() {
                dev_info!(
                    self.resources.dev().as_ref(),
                    "G17P rec uapi: attachment[{}] address={:#x} size={:#x}\n",
                    index,
                    entry.address,
                    entry.size,
                );
            }
        }
        // Which chronology to run is decided by the queue, never by the
        // command's flags. The add3 probe used to force the first-bind route
        // unconditionally, which meant its second submission either hit the
        // `submission_ordinal != 0` gate or re-ran the first-bind builder on a
        // live queue and reset the item-ring producer under the firmware.
        if first_bind {
            return self
                .submit_translated_first_bind_channel(
                    registers,
                    queue,
                    submission_phase,
                    binding,
                    command,
                    context,
                    command.g17p_add3_buffers,
                )
                .map(Some);
        }

        // Post-registration path. It assumes the tag/configure chronology has
        // already run, so a still-prepared queue is refused here exactly as
        // before the queue has been registered.
        if queue.hardware_state != G17PSksmQueueHardwareState::HardwareRegistered {
            return Err(ENOTSUPP);
        }

        if queue.submission_ordinal != 0 {
            return self
                .submit_translated_repeat_channel(
                    registers,
                    queue,
                    submission_phase,
                    binding,
                    command,
                    context,
                    command.g17p_add3_buffers,
                )
                .map(Some);
        }

        let timestamp = g17_submission::decode_kick_timestamp(queue.producer.current_timestamp)
            .map_err(|_| EINVAL)?;
        let (descriptor_high, _) = queue.compute.descriptor_gpu_vas(timestamp.stamp_index);
        let descriptor_low = binding
            .mappings
            .descriptor_gpu_va(timestamp.stamp_index);
        let user_layout = binding.mappings.layout();
        let checkpoint_internal = g17_uapi::ComputeInternalState {
            preempt_base: user_layout.preempt,
            dispatch_identity: context.dispatch_identity,
            context_id: context.context_id,
            work_ordinal: queue.submission_ordinal,
            robustness: user_layout.robustness,
            operand_state_base: user_layout.operand_state,
            usc_flist_id: u32::from(queue.storage.flist_id()),
        };
        let resume_checkpoints =
            g17_uapi::compute_resume_checkpoints(command, checkpoint_internal)
                .map_err(|_| EINVAL)?;
        let primary_checkpoint = u8::try_from(resume_checkpoints[0]).map_err(|_| EINVAL)?;
        let command_state = g17_submission::prepare_g17p_compute_cl_command_state(
            descriptor_low,
            u64::from(queue.submission_ordinal),
            primary_checkpoint,
        )
        .map_err(|_| EINVAL)?;
        let payload = [descriptor_high, queue.compute.queue_record_gpu_va()];
        let operands = command.entry_operands(g17_uapi::ComputeEntryContext {
            descriptor_flag_4c: context.descriptor_flag_4c,
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
        // The fall-through path. It publishes a kick entry WITHOUT ever having
        // run the first-bind chronology (tag 3 -> 15 -> QID configure -> SKSM
        // entry -> 14 -> 16 -> CL_2) for this binding, so name it loudly.
        self.log_record("fallthrough-cl-kick-entry", &prepared.entry);

        let ordinal = queue.submission_ordinal;
        let next_ordinal = ordinal.checked_add(1).ok_or(EOVERFLOW)?;
        let queue_submission = ordinal.checked_add(1).ok_or(EINVAL)?;
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
            usage_record: queue.compute.compute_usage_record_gpu_va(),
        };
        queue.compute.clear_compute_usage_slot()?;
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
                        usc_flist_id: u32::from(queue.storage.flist_id()),
                    },
                    objects,
                    g17_compute::ComputeDescriptorMetadata {
                        submit_sequence: ordinal as u64,
                        context_id: context.context_id,
                        grid_index: u32::from(queue.queue_id),
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
        // The alternate direct-entry route can expose an SKSM entry and pool
        // reservation before add_sksm_kicks returns. Treat every subsequent
        // error as partial publication, never as an unpublished retry.
        submission_phase.store(G17P_SUBMIT_PHASE_B2, Ordering::Release);
        let timestamp = self.publish_compute_entry(registers, queue, prepared)?;
        queue.submission_ordinal = next_ordinal;
        let _ = timestamp;
        Ok(None)
    }

    /// Report the record register 0x14070 names.
    ///
    /// Pure observation of driver-owned DRAM. `clear_compute_usage_slot` zeroed
    /// it before publication, so a record that is still all zeroes here means
    /// the GPU wrote nothing through the shared 0x14070/+0xfcb record. Words 0
    /// and 1 are the two the kext's `AGXUSCPrivMemPool::updateStatistics`
    /// consumes (mac17p `0xfffffe0008aa6bf8` and `0xfffffe0008aa6c40`).
    fn log_compute_usage_slot(&mut self, queue: &mut G17PSksmQueue, phase: &str) {
        if *crate::module_parameters::g17p_compute_usage_slot.value() == 0 {
            return;
        }
        let address = queue.compute.compute_usage_slot_gpu_va();
        match queue.compute.read_compute_usage_slot() {
            Ok(words) => dev_info!(
                self.resources.dev().as_ref(),
                "G17P compute usage[{}]: qid={} slot={:#x} word0={:#x} word1={:#x} rest={:#x?}\n",
                phase,
                queue.queue_id,
                address,
                words[0],
                words[1],
                &words[2..]
            ),
            Err(error) => dev_err!(
                self.resources.dev().as_ref(),
                "G17P compute usage[{}]: qid={} slot={:#x} unreadable ({:?})\n",
                phase,
                queue.queue_id,
                address,
                error
            ),
        }
    }

    /// Log the queue-graph ring cursors. DRAM only -- safe on a timeout path,
    /// unlike anything that reads the sgx register file once the cores have
    /// powered back down.
    pub(crate) fn log_compute_ring_state(
        &mut self,
        queue: &mut G17PSksmQueue,
        label: &'static str,
    ) {
        // Never gate the failure samples: they are the whole point.
        if !Self::compute_trace_enabled(queue.submission_ordinal)
            && label != "timeout"
            && label != "publish-failed"
        {
            return;
        }
        let state = match queue.compute.read_compute_ring_state() {
            Ok(state) => state,
            Err(error) => {
                dev_err!(
                    self.resources.dev().as_ref(),
                    "G17P ring[{}]: unreadable ({:?})\n",
                    label,
                    error
                );
                return;
            }
        };
        dev_info!(
            self.resources.dev().as_ref(),
            "G17P ring[{}]: queue {:#x} consumer {} producer {} mirror {:#x} count {:#x} submitted {} ordinal {} reclaim-cursor {} reclaimed-total {} items {:x?}\n",
            label,
            state.queue,
            state.consumer,
            state.producer,
            state.mirror,
            state.count,
            queue.producer.submitted,
            queue.submission_ordinal,
            state.reclaimed,
            state.reclaimed_total,
            state.items
        );
    }

    /// Read retained and in-flight ownership without selecting another slot.
    /// The timestamp arrays are pass-wide views, NOT separate TA/3D intervals.
    fn log_mixed_render_owner(
        checkpoint: &str,
        compute_qid: u8,
        slot: u8,
        host: (u64, bool, Option<u32>, bool, Option<u64>),
        storage: Option<&mut g17_resources::G17PUserRenderStorage>,
    ) {
        let retained = storage.map(|storage| {
            (
                storage.descriptor_context_id(),
                storage.retained_hardware_buffer_id(),
                storage.render_pool_id(),
                storage.parameter_manager_generation(),
                storage.tvb_capacity(),
                storage.context_update_pending(),
                storage.snapshot(host.0),
            )
        });
        pr_info!("G17P mixed-resource render[{}]: compute-qid={} slot={} host(ordinal,tracker,active-hbid,published,vm)={:?} retained(context,hbid,pool,pm-generation,tvb-blocks,context-pending,queues-job-list-pass-stamps)={:x?}\n",
            checkpoint, compute_qid, slot, host, retained);
    }

    /// Diagnostic DRAM census; no register reads, queue selection, retirement,
    /// scheduling changes or error propagation. Firmware may update memory
    /// between reads; report raw witnesses rather than claiming active engines.
    fn log_mixed_resource_census(&mut self, queue: &mut G17PSksmQueue, fault: bool) {
        if !g17p_mixed_resource_sample(
            *module_parameters::g17p_mixed_resource_probe.value(), queue.submission_ordinal, fault,
        ) { return; }
        let started = kernel::time::Instant::<kernel::time::Monotonic>::now();
        let checkpoint = if fault { "compute-fault" } else { "compute-before-publish" };
        pr_info!("G17P mixed-resource owners[{}]: qid={} compute-ordinal={} qos-hbid={} selected-render={} hb-free={:x?} hb-reserved={:x?} cached-graphs={} retained-pm={} flist={:?}\n",
            checkpoint, queue.queue_id, queue.submission_ordinal, queue.qos_hardware_buffer_id(),
            self.active_render_slot, self.render_hardware_buffer_ids.free,
            self.render_hardware_buffer_ids.reserved,
            self.retired_render_handoff_graphs.len()
                + usize::from(self.retired_render_handoff_graph.is_some())
                + self.parked_render_slots.iter().flatten()
                    .filter(|slot| slot.retired_handoff_graph.is_some()).count(),
            self.retired_parameter_managers.len(), self.bootstrap_uma);
        Self::log_mixed_render_owner(
            checkpoint, queue.queue_id, self.active_render_slot,
            (self.render_submission_ordinal, self.render_tracker.is_some(),
                self.render_hardware_buffer_id, self.render_hardware_buffer_published,
                self.render_bind.as_ref().map(|binding| binding.vm_id())),
            self.render_storage.as_mut(),
        );
        for (index, slot) in self.parked_render_slots.iter_mut().enumerate() {
            if index == usize::from(self.active_render_slot) { continue; }
            if let Some(slot) = slot {
                Self::log_mixed_render_owner(
                    checkpoint, queue.queue_id, index as u8,
                    (slot.submission_ordinal, slot.tracker.is_some(), slot.hardware_buffer_id,
                        slot.hardware_buffer_published, slot.bind.as_ref().map(|binding| binding.vm_id())),
                    slot.storage.as_mut(),
                );
            } else {
                Self::log_mixed_render_owner(
                    checkpoint, queue.queue_id, index as u8, (0, false, None, false, None), None,
                );
            }
        }
        let uma = [0u8, queue.storage.flist_id() as u8]
            .map(|slot| (slot, self.resources.snapshot_compute_uma(slot)));
        let flist = queue.storage.snapshot_compute_flist();
        let qos = self.resources.snapshot_compute_qos_owner(queue.queue_id, queue.qos_hardware_buffer_id());
        pr_info!("G17P mixed-resource pools[{}]: qid={} uma-slots={:x?} compute-flist={:x?} compute-qos={:x?}\n",
            checkpoint, queue.queue_id, uma, flist, qos);
        self.resources.log_render_uma_growth(checkpoint);
        pr_info!("G17P mixed-resource sample[{}]: qid={} census-ns={} (sequential DRAM and logging, not an atomic execution snapshot)\n",
            checkpoint, queue.queue_id, started.elapsed().as_nanos().max(0));
    }

    /// Bounded, read-only timeout evidence for the retained asynchronous owner.
    /// The shared outer slot is the most recent publication, not necessarily
    /// this queue's slot. Samples are sequential DRAM observations, not an
    /// atomic hardware snapshot; no firmware pause or MMIO is attempted.
    pub(crate) fn log_async_compute_timeout(&mut self, queue: &mut G17PSksmQueue) {
        self.log_mixed_resource_census(queue, true);
        let pool_id = queue.storage.flist_id() as u8;
        let uma = self.resources.snapshot_compute_uma(pool_id);
        let uma_slots = [0u8, 1, 2, 3]
            .map(|slot| (slot, self.resources.snapshot_compute_uma(slot)));
        let flist = queue.storage.snapshot_compute_flist();
        let qos_owner = self.resources.snapshot_compute_qos_owner(
            queue.queue_id,
            queue.qos_hardware_buffer_id(),
        );
        if *module_parameters::g17p_fault_report.value() & 4 != 0 {
            let read = |link: u64| -> Result<u64> {
                let phys = g17p_gfx_link_to_physical(link);
                pgtable::LiveFirmwareU64Probe::new(phys)
                    .and_then(|probe| probe.observe_mask(u64::MAX, 1))
                    .map(|sample| sample.first)
            };
            let pool_id = u64::from(queue.storage.flist_id());
            let qid = u64::from(queue.queue_id);
            let row_base = G17P_FLIST_ROW_LINK_BASE + pool_id * G17P_FLIST_ROW_STRIDE;
            let row: [Result<u64>; 29] =
                core::array::from_fn(|index| read(row_base + index as u64 * 8));
            let active = [
                read(G17P_FLIST_ACTIVE_BITMAP_LINK),
                read(G17P_FLIST_ACTIVE_BITMAP_LINK + 8),
            ];
            let dm2 = [
                read(G17P_DM_ACTIVE_BITMAP_LINK_BASE + 2 * 0x10),
                read(G17P_DM_ACTIVE_BITMAP_LINK_BASE + 2 * 0x10 + 8),
            ];
            let park = G17P_KSM_PARK_RECORD_LINK_BASE + qid * 0x28;
            let scheduler = [
                read(G17P_QID_OWNER_LINK_BASE + qid * 8),
                read(G17P_READY_MASK_LINK_BASE),
                read(park + 0x418),
                read(park + 0x400),
                read(G17P_QUEUE_STATE_LINK_BASE + qid * 0x28),
                read(G17P_QUEUE_STATE_LINK_BASE + qid * 0x28 + 8),
                read(G17P_QUEUE_FLAG_LINK_BASE + qid * 0x28 + 0x40),
            ];
            let scheduler_masks: [Result<u64>; 8] = core::array::from_fn(|index| {
                read(G17P_KSM_SCHEDULER_MASKS_LINK + index as u64 * 8)
            });
            let runnable = [
                read(G17P_KSM_RUNNABLE_MASK_LINK),
                read(G17P_KSM_RUNNABLE_MASK_LINK + 8),
            ];
            let park_record: [Result<u64>; 5] =
                core::array::from_fn(|word| read(park + 0x3f8 + word as u64 * 8));
            let irq_handoff: [Result<u64>; 5] =
                core::array::from_fn(|word| read(G17P_KSM_IRQ_HANDOFF_LINKS[word]));
            let worker_busy = read(G17P_FLIST_WORKER_BUSY_LINK);
            let worker_rings: [[Result<u64>; 2]; 4] =
                core::array::from_fn(|ring| {
                    core::array::from_fn(|word| {
                        read(G17P_FLIST_WORKER_RING_LINKS[ring] + word as u64 * 8)
                    })
                });
            dev_err!(self.resources.dev().as_ref(),
                "G17P fault-only B1 scheduler: qid={} pool={} qid-owner/ready/park-counters/park-deadline/qstate0/qstate8/qflag={:x?} dm2-active={:x?} pool-active={:x?} masks(requested,sampled,deferred,next)={:x?} runnable={:x?} park-record={:x?} irq-handoff={:x?} flist-worker-busy={:x?} worker-rings={:x?}\n",
                queue.queue_id, pool_id, scheduler, dm2, active,
                scheduler_masks, runnable, park_record, irq_handoff,
                worker_busy, worker_rings);
            dev_err!(self.resources.dev().as_ref(),
                "G17P fault-only B1 FList row: qid={} pool={} link={:#x} qwords={:x?}\n",
                queue.queue_id, pool_id, row_base, row);
        }
        let usage = queue.compute.read_compute_usage_slot();
        let descriptor_usage = queue.compute.read_compute_descriptor_usage_slot();
        dev_err!(self.resources.dev().as_ref(),
            "G17P async-timeout spill: qid={} qos-hbid={} uma-slot={} uma={:x?} uma-slots={:x?} flist-index={} flist-pool={} flist-va={:#x} flist={:x?} reg-14070-usage={:x?} descriptor-fcb-usage={:x?}\n",
            queue.queue_id, queue.qos_hardware_buffer_id(), pool_id, uma, uma_slots, pool_id, pool_id,
            queue.storage.shared_support_gpu_va(), flist,
            usage, descriptor_usage);
        let ring = queue.compute.read_compute_ring_state();
        let outer = self.resources.snapshot_compute_channel();
        let submission = queue.compute.snapshot_compute_submission(queue.completion_descriptor);
        if crate::g17_config::COMPUTE_RESOURCE_SNAPSHOT != 0 {
            match queue.publication_resource_snapshot.as_ref() {
                Some((sample, sample_flist)) => dev_err!(self.resources.dev().as_ref(),
                    "G17P async-timeout B1 publication: qid={} descriptor={:#x} context={} grid={} state={:#x} count={:#x} usage={:#x} index={:#x} register={:x?} flist-pointer={:#x} flist+08={:#x} flist+48={:#x}\n",
                    queue.queue_id, sample.descriptor, sample.context, sample.grid,
                    sample.resource_state, sample.primary_register_count,
                    sample.resource_usage, sample.resource_index,
                    sample.resource_register, sample.flist_pointer,
                    sample_flist[2], sample_flist[18]),
                None => dev_err!(self.resources.dev().as_ref(),
                    "G17P async-timeout B1 publication: qid={} unavailable\n",
                    queue.queue_id),
            }
            match submission.as_ref() {
                Ok(sample) => dev_err!(self.resources.dev().as_ref(),
                    "G17P async-timeout B1 halt: qid={} descriptor={:#x} context={} grid={} state={:#x} count={:#x} usage={:#x} index={:#x} register={:x?} flist-pointer={:#x}\n",
                    queue.queue_id, sample.descriptor, sample.context, sample.grid,
                    sample.resource_state, sample.primary_register_count,
                    sample.resource_usage, sample.resource_index,
                    sample.resource_register, sample.flist_pointer),
                Err(error) => dev_err!(self.resources.dev().as_ref(),
                    "G17P async-timeout B1 halt: qid={} descriptor unavailable ({:?})\n",
                    queue.queue_id, error),
            }
            match flist.as_ref() {
                Ok(sample_flist) => dev_err!(self.resources.dev().as_ref(),
                    "G17P async-timeout B1 halt FList: qid={} flist+08={:#x} flist+48={:#x}\n",
                    queue.queue_id, sample_flist[2], sample_flist[18]),
                Err(error) => dev_err!(self.resources.dev().as_ref(),
                    "G17P async-timeout B1 halt FList: qid={} unavailable ({:?})\n",
                    queue.queue_id, error),
            }
        }
        self.log_compute_usage_slot(queue, "timeout");
        let roots = (|| -> Result<(u64, u64)> {
            let decoded = submission.as_ref().map_err(|error| *error)?;
            let context = u8::try_from(decoded.context).map_err(|_| EINVAL)?;
            Ok(self.uat().t8140_selected_context_roots([context; 4])?[0])
        })();
        let control = queue.logical_channel_control.as_deref()
            .or(self.compute_channel_control.as_ref())
            .ok_or(EINVAL)
            .and_then(g17_resources::G17PClChannelControl::snapshot_prefix);
        let b2_channel_control = self.resources.compute_b2_channel_control();
        let expected_channel_control = self.compute_channel_control.as_ref().map(|c| c.gpu_va());
        let qos_rows = [queue.queue_id, 4u8, 5, 6, 7]
            .map(|qid| (qid, self.resources.qos_queue_record(qid)));
        let entry = (|| -> Result<_> {
            if !queue.producer.previous_valid { return Err(EINVAL); }
            let last = g17_submission::prepare_kick_submission(
                queue.producer.previous_timestamp, queue.queue_id,
                queue.producer.geometry.stamp_count(), queue.producer.geometry.entry_stride(),
            ).map_err(|_| EINVAL)?;
            Ok((last.entry_offset,
                queue.storage.snapshot_compute_entry(last.entry_offset as usize)?))
        })();
        // The fork this whole class of wedge turns on, sampled at the moment
        // it fails rather than inferred afterwards. Three outcomes, three
        // different bugs, and until now the log could not tell them apart:
        //
        //   completed < ordinal   the firmware never retired the work -- a
        //                         scheduling/dispatch failure, and the
        //                         publication-time TSQ check should already
        //                         have said so;
        //   completed == ordinal, stamp == 0
        //                         retired, but the completion stamp was never
        //                         made visible -- the poller has been returning
        //                         EAGAIN, which is swallowed silently;
        //   completed == ordinal, stamp != 0
        //                         retired and stamped, so the loss is in the
        //                         completion RECORD: the poller has been
        //                         returning Ok(None) because no record folded.
        let counters = queue.compute.read_compute_submitted_counters();
        let stamp_word = queue.compute.read_compute_stamp();
        // The firmware's own dependency record for THIS queue.
        //
        // Every kick declares an implicit in-order self-dependency on its own
        // previous stamp -- the encoder writes {own qid, converted_command
        // _timestamp - 1} -- and the firmware's rule is: satisfied iff the
        // record is valid (byte 0) and dep_stamp <= completed_stamp. So a queue
        // reading `completed = target - 1` is exactly the arithmetic of one kick
        // parked on a self-dependency that never resolved, which is also the
        // render event-control class's signature (submitted=386 completed=385).
        // Nothing has ever sampled this record at an async-compute timeout.
        //
        // Deliberately NOT gated on `g17p_dep_records`: that parameter is
        // overloaded and also enables `log_render_sksm_entries`, a ~19 KB
        // hexdump per call at pre-doorbell that has itself caused
        // VK_ERROR_DEVICE_LOST. This is three words on a path that only runs
        // when a submission has already timed out.
        let dependency = {
            let phys = G17P_GFX_PHYSICAL_DATA_BASE
                + (G17P_DEPENDENCY_RECORD_LINK_BASE - G17P_GFX_LINK_DATA_BASE)
                + u64::from(queue.queue_id) * G17P_DEPENDENCY_RECORD_STRIDE;
            let valid = pgtable::LiveFirmwareU64Probe::new(phys)
                .and_then(|p| p.observe_mask(u64::MAX, 1))
                .map(|o| o.first);
            let completed = pgtable::LiveFirmwareU64Probe::new(phys + 8)
                .and_then(|p| p.observe_mask(u64::MAX, 1))
                .map(|o| o.first);
            (valid, completed)
        };
        dev_err!(self.resources.dev().as_ref(),
            "G17P async-timeout dependency: qid={} record={:#x} valid-word={:x?} completed-stamp={:x?} -- a kick is satisfied iff valid and dep_stamp <= completed_stamp\n",
            queue.queue_id,
            G17P_DEPENDENCY_RECORD_LINK_BASE
                + u64::from(queue.queue_id) * G17P_DEPENDENCY_RECORD_STRIDE,
            dependency.0,
            dependency.1);
        // `consumed-end-floor` is `G17PComputeCompletionBatchSlot::
        // last_consumed_end`, the value `fold_record` compares every candidate
        // against with `pair_b_end <= floor -> reject`. That rejection is
        // PERMANENT for the record, and nothing has ever printed the floor, so
        // a record the decoded dump reports as `matched-rec Some(n)` could be
        // one the batch reader had already refused on this comparison and the
        // two readers' disagreement was invisible. Printed next to the record
        // dump's per-record `b-end` so the two can be compared directly.
        dev_err!(self.resources.dev().as_ref(),
            "G17P async-timeout retirement: qid={} ordinal={} counters(target,completed)={:x?} stamp-word={:x?} stamp-armed={} awaited-descriptor={:#x} consumed-end-floor={:#x} independent-context={}\n",
            queue.queue_id, queue.submission_ordinal, counters, stamp_word,
            queue.completion_stamp_armed, queue.completion_descriptor,
            queue.completion_last_end, queue.independent_context);
        dev_err!(self.resources.dev().as_ref(),
            "G17P async-timeout owner: qid={} hbid={} descriptor={:#x} ordinal={} producer={:?} direct={:?} hardware={:?} qos-owner={:x?}\n",
            queue.queue_id, queue.qos_hardware_buffer_id(), queue.completion_descriptor,
            queue.submission_ordinal, queue.producer, queue.direct_lifecycle,
            queue.hardware_state, qos_owner);
        dev_err!(self.resources.dev().as_ref(),
            "G17P async-timeout ring: {:x?}\n", ring);
        dev_err!(self.resources.dev().as_ref(),
            "G17P async-timeout outer-latest-not-owner: {:x?}\n", outer);
        dev_err!(self.resources.dev().as_ref(),
            "G17P async-timeout submission: {:x?} descriptor-context-ttbat={:x?}\n", submission, roots);
        dev_err!(self.resources.dev().as_ref(),
            "G17P async-timeout channel-control: {:x?}\n", control);
        dev_err!(self.resources.dev().as_ref(),
            "G17P async-timeout B2 bootstrap-control(actual,expected): ({:x?},{:x?}) QoS rows(qid,result)={:x?}\n",
            b2_channel_control, expected_channel_control, qos_rows);
        dev_err!(self.resources.dev().as_ref(),
            "G17P async-timeout last-host-entry(offset,words): {:x?}\n", entry);
    }

    /// Whether the noisy per-submission traces should be emitted.
    ///
    /// A 400-deep run emits thousands of lines and overflows the kernel ring
    /// buffer, which destroys exactly the evidence at the end of the run that
    /// matters -- and makes any count obtained by grepping dmesg a measure of
    /// how much survived rather than of what happened. Trace the opening
    /// submissions, then go quiet; errors and end-of-run summaries are never
    /// gated.
    fn compute_trace_enabled(ordinal: u32) -> bool {
        let depth = *module_parameters::g17p_trace_depth.value();
        depth != 0 && ordinal <= depth
    }

    /// Log the OUTER CL_2 work-channel cursors and its most recent slot.
    ///
    /// The inner item ring and the outer channel are two different rings and
    /// either can stall independently: the item ring says whether the firmware
    /// walked the records we appended, this says whether it ever looked at the
    /// slot that points at them. Also DRAM only -- `primary_state` and
    /// `shared_cluster` are driver-allocated objects, not MMIO.
    pub(crate) fn log_compute_outer_channel(&mut self, label: &'static str) {
        let snapshot = match self.resources.snapshot_compute_channel() {
            Ok(snapshot) => snapshot,
            Err(error) => {
                dev_err!(
                    self.resources.dev().as_ref(),
                    "G17P outer[{}]: unreadable ({:?})\n",
                    label,
                    error
                );
                return;
            }
        };
        dev_info!(
            self.resources.dev().as_ref(),
            "G17P outer[{}]: table {} ring {:#x} cursors(actual,mirror,producer)={:?} slot {} queue {:#x} kind {} flags {:#x}\n",
            label,
            snapshot.table_index,
            snapshot.ring,
            snapshot.cursors,
            snapshot.slot_index,
            snapshot.slot_queue,
            snapshot.slot_kind,
            snapshot.slot_flags
        );
    }

    /// Zero the KSM completion record and remember which descriptor the
    /// submission about to be published owns.
    ///
    /// Must run after the descriptor is encoded and before ANY firmware-
    /// visible publication, so that from the firmware's first opportunity to
    /// write the record onwards, "populated" means "written for this
    /// submission". Without it the record left by a previous submission
    /// satisfies the next wait immediately -- which is how a run whose GPU
    /// cores never powered (`gpc-state = 0x0`) still reported a woken doorbell
    /// and retired.
    fn arm_compute_completion(
        &mut self,
        queue: &mut G17PSksmQueue,
        descriptor: u64,
    ) -> Result {
        if queue.independent_context {
            let count = queue.submission_ordinal.checked_add(1).ok_or(EOVERFLOW)?;
            let [previous, completed] = queue.compute.publish_compute_submitted_count(count)?;
            if queue.submission_ordinal < 4 {
                // Four rather than two: the steady-state relationship between
                // `completed` and the ordinal is what the violation test below
                // rests on, and it is not observable from the first two
                // publications alone.
                dev_info!(self.resources.dev().as_ref(),
                    "G17P compute TSQ: qid={} ordinal={} target={} previous={} completed={} before-publication\n",
                    queue.queue_id, queue.submission_ordinal, count, previous, completed);
            }
            // `MAX_IN_FLIGHT_PER_QUEUE` is 1, so publishing target
            // `ordinal + 1` means the firmware must already have retired every
            // earlier publication -- its completed counter should read exactly
            // `ordinal`. When it lags, the previous submission was accepted and
            // never executed, and stacking this one on top guarantees the
            // client waits out a full scheduler timeout and then quarantines
            // the queue. That wedge is otherwise only visible two seconds
            // later, as an ETIMEDOUT with no fault anywhere.
            //
            // Measured against `ordinal`, NOT against `previous`: `previous` is
            // the target word's prior contents, and first-bind staging seeds it
            // to 1, so on a fresh queue it reads target=1 previous=1
            // completed=0 -- which the first version of this test called a
            // violation. Three fired on a green idle boot that never wedged.
            // The firmware cannot have *stopped* retiring before it has retired
            // anything, so ordinal 0 is excluded by construction rather than by
            // a special case.
            //
            // Logged rather than refused: a refusal is the right end state but
            // it has to be proven not to fire on a healthy boot first, and this
            // line is what proves it.
            // Two independent gates, both of which the fresh-queue case fails.
            //
            // `submission_ordinal > 0` is definitional: the field is written
            // only at construction (0), at first bind, and by the steady
            // advance -- never reset -- so `target == 1` means this queue has
            // published nothing, and a queue that has published nothing cannot
            // have stopped retiring. That alone explains all three fires on the
            // green boot, including the late one: compute QIDs are handed out
            // ascending and never reused, so 5, 6 and 7 arriving at t=19, t=20
            // and t=224 are three new clients, not one queue stalling.
            //
            // `completion_last_end != 0` is the stronger witness and is what
            // the check actually wants: that field is written only by a
            // successful `poll_staged_independent_compute_completion`, so it is
            // proof the firmware has retired at least once on THIS queue. The
            // condition being tested is that it has stopped, which is not a
            // statement anyone can make about a queue that never started.
            if queue.submission_ordinal > 0
                && queue.completion_last_end != 0
                && completed != queue.submission_ordinal
            {
                dev_err!(self.resources.dev().as_ref(),
                    "G17P compute TSQ: qid={} publishing target={} at ordinal={} while firmware completed={} (lag={}, previous={}) -- an earlier submission never retired; this publication will not complete\n",
                    queue.queue_id,
                    count,
                    queue.submission_ordinal,
                    completed,
                    queue.submission_ordinal.saturating_sub(completed),
                    previous);
            }
        }
        let previous = if queue.independent_context { [0, 0] }
            else { self.resources.clear_compute_completion()? };
        queue.completion_descriptor = descriptor;
        self.last_published_descriptor = descriptor;
        queue.publication_resource_snapshot =
            if crate::g17_config::COMPUTE_RESOURCE_SNAPSHOT != 0 {
                Some((
                    queue.compute.snapshot_compute_submission(descriptor)?,
                    queue.storage.snapshot_compute_flist()?,
                ))
            } else {
                None
            };
        // Arm the completion stamp as well. The KSM completion record is
        // produced by the KSM hardware block and consumed by the firmware at
        // the TOP of the completion interrupt; the firmware's GPU
        // cache-maintenance command and the stamp store both happen after
        // that. So the record proves scheduling, and only the stamp proves
        // visibility -- see `await_compute_stamp`.
        if crate::g17_config::COMPLETION_STAMP != 0 {
            let discarded = queue.compute.arm_compute_stamp()?;
            self.compute_stamp_armed = true;
            queue.completion_stamp_armed = true;
            if discarded != 0 && Self::compute_trace_enabled(queue.submission_ordinal) {
                dev_info!(
                    self.resources.dev().as_ref(),
                    "G17P completion stamp: discarded {:#x} before publishing {:#x}\n",
                    discarded,
                    descriptor
                );
            }
        }
        // Do NOT reset `last_completion_end` here. It is the floor that makes
        // "strictly newer than the completion already consumed" mean anything,
        // and zeroing it once per submission made that test vacuous -- which
        // is why tightening the scan from `end == last_consumed_end` to
        // `end <= last_consumed_end` measured as noise: with the floor at 0 the
        // two predicates are identical. GPU timestamps are monotonic across a
        // manager's lifetime, so the previous submission's end is a valid
        // floor for this one; the field is initialised to 0 when the manager is
        // constructed, and a recovery rebuilds the manager, so a teardown
        // resets it without any per-submission help.
        if previous[0] != 0 {
            dev_info!(
                self.resources.dev().as_ref(),
                "G17P completion: cleared stale record descriptor {:#x} end {:#x} before publishing {:#x}\n",
                previous[0],
                previous[1],
                descriptor
            );
        }
        Ok(())
    }

    /// Completion ingress for the descriptor-provided completed-kick count.
    /// Sample everything host-visible that describes whether the retained
    /// compute queue and the firmware still agree about QID 4.
    ///
    /// Taken either side of a serviced firmware recovery, so the divergence a
    /// fault introduces can be SEEN rather than inferred. A boot with no fault
    /// hands the graph to any number of sequential clients; one GMMU fault in
    /// between and every later kick is accepted, retired and never executed --
    /// so whatever moves across this boundary is the mechanism.
    ///
    /// HOST DRAM ONLY, deliberately. Every field is a read of an object this
    /// driver allocated or of the firmware handoff region. Nothing here touches
    /// the sgx register file: a recovery is serviced with the GPU cores gated,
    /// and an sgx read in that state takes an asynchronous SError and panics
    /// the machine. That is also why the firmware's KSM queue slot at
    /// `0x127628 + qid*0x28` is NOT sampled here -- it lives in the ASC's own
    /// address space, reachable only through sgx MMIO or the firmware's own
    /// mapping, so there is no safe host read of it on this path.
    pub(crate) fn log_compute_recovery_alignment(
        &mut self,
        queue: &mut G17PSksmQueue,
        label: &'static str,
    ) {
        let fw_stamp = self.g17p_last_submitted_hw_timestamp(queue.queue_id as usize);
        let ring = queue.compute.read_compute_ring_state().ok();
        let free = queue.compute.compute_ring_free_slots().ok();
        let producer = queue.producer;
        dev_info!(
            self.resources.dev().as_ref(),
            "G17P recovery-align[{}]: qid={} fw-last-submitted={:?} host(ordinal={} stamp={:#x} prev-valid={} prev-stamp={:#x} base={:#x} submitted={} addkicks={:#x}) ring(consumer={:?} producer={:?} free={:?})\n",
            label,
            queue.queue_id,
            fw_stamp,
            queue.submission_ordinal,
            producer.current_timestamp,
            producer.previous_valid,
            producer.previous_timestamp,
            producer.base_timestamp,
            producer.submitted,
            producer.last_add_kicks_timestamp,
            ring.as_ref().map(|state| state.consumer),
            ring.as_ref().map(|state| state.producer),
            free,
        );
        // The KSM pause-reason mask, read HOST-SAFELY.
        //
        // The mask itself lives at `sgx+0x2_11c8` and its refcount at the
        // firmware's `0x128a68`; both are inside the sgx 0x0..0x2ffff window,
        // which is unreachable from the AP and takes an asynchronous SError.
        // With the cores demonstrably gated after a fault-blamed recovery that
        // hazard is the NORMAL state on this path, so neither is read here.
        // The firmware publishes the same value itself: channel-0 KTrace code
        // 0x46 carries the pause reason, and the KTrace ring is host DRAM.
        // Needs `asahi.g17p_fw_trace=1` -- with tracing off the ring is empty
        // and `consumed` reads 0, which is why that is reported too.
        let ktrace = self.resources.drain_ktrace_ring(
            InstanceRole::Primary,
            *module_parameters::g17p_fw_ktrace_verbose.value() != 0,
            *module_parameters::g17p_fw_ktrace_budget.value(),
        );
        dev_info!(
            self.resources.dev().as_ref(),
            "G17P recovery-align[{}]: ktrace(consumed={:?} pause-mask={:?} saw-addkicks={:?} lost={:?}) -- pause-mask needs g17p_fw_trace=1\n",
            label,
            ktrace.as_ref().map(|stats| stats.consumed),
            ktrace.as_ref().map(|stats| stats.last_pause_mask),
            ktrace.as_ref().map(|stats| stats.saw_add_kicks),
            ktrace.as_ref().map(|stats| stats.lost_records),
        );
        let status_a = self.resources.status_a_snapshot();
        let channel = self.resources.snapshot_compute_channel();
        dev_info!(
            self.resources.dev().as_ref(),
            "G17P recovery-align[{}]: statusA(scan-active={:?} active={:?} scheduler-constructed={:?}) outer(cursors={:?} slot-queue={:?})\n",
            label,
            status_a.as_ref().map(|state| state.scan_active),
            status_a.as_ref().map(|state| state.active),
            status_a.as_ref().map(|state| state.scheduler_constructed),
            channel.as_ref().map(|state| state.cursors),
            channel.as_ref().map(|state| state.slot_queue),
        );
    }

    /// Realign the retained compute queue with the firmware after a recovery.
    ///
    /// A recovery is the one event that can move the firmware's idea of this
    /// QID without the host publishing anything, so continuity is exactly the
    /// assumption that must not be made across it. See `g17p_recovery_resync`
    /// for the individual arms; returns the mask actually applied.
    pub(crate) fn resynchronise_compute_queue_after_recovery(
        &mut self,
        queue: &mut G17PSksmQueue,
    ) -> Result<u32> {
        let mode = crate::g17_config::RECOVERY_RESYNC;
        if mode == 0 {
            return Ok(0);
        }
        let mut applied = 0u32;
        if mode & 1 != 0 && queue.producer.previous_valid {
            // The next kick now encodes the no-predecessor implicit dependency
            // -- dependency on (qid, stamp 0) -- which is byte-for-byte what a
            // proven first bind sends.
            queue.producer.previous_valid = false;
            queue.producer.previous_timestamp = 0;
            applied |= 1;
        }
        if mode & 2 != 0 {
            if let Ok((valid, stamp)) =
                self.g17p_last_submitted_hw_timestamp(queue.queue_id as usize)
            {
                // Forward only. Rewinding the stamp is what makes a kick look
                // already retired, which is the failure this whole line of work
                // started from.
                if valid != 0 && stamp >= queue.producer.current_timestamp {
                    let aligned = stamp.wrapping_add(1);
                    if g17_submission::decode_kick_timestamp(aligned).is_ok() {
                        queue.producer.current_timestamp = aligned;
                        queue.producer.base_timestamp = aligned;
                        applied |= 2;
                    }
                }
            }
        }
        if mode & 4 != 0 && queue.direct_lifecycle.cached_channel_identity != 0 {
            // Only the notification predicate: `publish_config_update` keys off
            // `queue_config_dirty`, which stays false, so this cannot re-assert
            // the tag-15 queue-install flag.
            queue.direct_lifecycle.cached_channel_identity = 0;
            applied |= 4;
        }
        dev_info!(
            self.resources.dev().as_ref(),
            "G17P recovery-resync: qid={} mode={:#x} applied={:#x} -> stamp={:#x} prev-valid={} prev-stamp={:#x} channel-identity={:#x}\n",
            queue.queue_id,
            mode,
            applied,
            queue.producer.current_timestamp,
            queue.producer.previous_valid,
            queue.producer.previous_timestamp,
            queue.direct_lifecycle.cached_channel_identity,
        );
        Ok(applied)
    }

    /// Make a retained queue submittable again after a submission was
    /// published and never seen to retire.
    ///
    /// The queue's forward-only state -- the SKSM stamp sequence, the
    /// submission ordinal and the item-ring producer -- is deliberately left
    /// exactly where the abandoned kick left it. Rewinding any of them is what
    /// makes the firmware and the host disagree, and the stamp namespace is
    /// per-QID and monotonic for the life of the hardware queue.
    ///
    /// What IS repaired is the host-side accounting the abandoned kick leaked:
    ///
    ///  * `producer.submitted` is written off. `prepare_g17p_cl_kick_entry`
    ///    refuses once it reaches `stamp_count` (0x100), so one abandoned kick
    ///    per failing client would otherwise brick the queue after 256 of them
    ///    -- and a CTS run submits failing work by design.
    ///  * The armed completion stamp is disarmed, so the next retire does not
    ///    spend `g17p_completion_stamp_us` waiting for a stamp that belongs to
    ///    a kick nobody is waiting for any more.
    ///  * The KSM completion record and its expected-descriptor filter are
    ///    cleared, so a late write for the abandoned kick cannot satisfy the
    ///    next client's wait. `last_completion_end` is deliberately NOT reset:
    ///    it is the monotonic floor that makes that guarantee work.
    ///
    /// Reports the item-ring cursors, because whether the firmware CONSUMED
    /// the abandoned records is the one thing no capture has settled: a
    /// consumer that has passed the producer means the records are retired and
    /// the ring recycles normally; a consumer stuck behind them means the ring
    /// will fill and `compute_ring_free_slots` will eventually refuse.
    /// DRAM only -- safe with the GPU cores gated, which is the state a timed
    /// out submission leaves them in.
    pub(crate) fn resynchronise_compute_queue_after_abandon(
        &mut self,
        queue: &mut G17PSksmQueue,
    ) -> Result<u32> {
        let outstanding = queue.producer.submitted;
        if outstanding != 0 {
            queue.producer =
                g17_submission::complete_g17p_cl_kicks(queue.producer, outstanding)
                    .map_err(|_| EINVAL)?;
        }
        self.compute_stamp_armed = false;
        let stale = self.resources.clear_compute_completion()?;
        self.last_published_descriptor = 0;
        let ring = queue.compute.read_compute_ring_state().ok();
        let free = queue.compute.compute_ring_free_slots().ok();
        dev_info!(
            self.resources.dev().as_ref(),
            "G17P compute: abandoned kick written off on qid {}; outstanding {} -> 0, ordinal={} stamp={:#x} ring consumer={:?} producer={:?} free={:?} stale-record desc={:#x} end={:#x}\n",
            queue.queue_id,
            outstanding,
            queue.submission_ordinal,
            queue.producer.current_timestamp,
            ring.as_ref().map(|state| state.consumer),
            ring.as_ref().map(|state| state.producer),
            free,
            stale[0],
            stale[1],
        );
        Ok(outstanding)
    }

    pub(crate) fn complete_compute_entries(
        &self,
        queue: &mut G17PSksmQueue,
        completed_count: u32,
    ) -> Result {
        if queue.hardware_state != G17PSksmQueueHardwareState::HardwareRegistered
            || queue.scratch != self.sksm_scratch()
            || queue.uat_owner != self.uat().ttb_base()
        {
            return Err(EINVAL);
        }
        self.await_compute_stamp(queue);
        queue.producer = g17_submission::complete_g17p_cl_kicks(queue.producer, completed_count)
            .map_err(|_| EINVAL)?;
        Ok(())
    }

    /// Hold a retire until the firmware has published this submission's
    /// completion stamp, so that "complete" implies "the dispatch's writes are
    /// visible".
    ///
    /// Firmware RE (`b1`) settles the ordering. The KSM completion record is
    /// written by the KSM hardware block, and the firmware's completion
    /// interrupt handler CONSUMES it first (`0xd550` drains the ring, `0xab64`
    /// decodes one record). Only afterwards, in
    /// `AGFAcceleratorProcessTimeStampQueue` (`0x168d0`), does it issue the GPU
    /// cache-maintenance command to `sgx+0x1_8030`, poll `sgx+0x1_8038` bit 1
    /// until it clears, scan the fault units, and finally store the stamp
    /// value from descriptor `+0x0F50` through the pointer at descriptor
    /// `+0x0F40`. That pointer is this queue's support-page dispatch-A word.
    ///
    /// So the record's arrival is upstream of the flush and says nothing about
    /// the dispatch's stores; the stamp store is downstream of it and says
    /// exactly that. Waiting for the record and not the stamp is why a
    /// submission retired cleanly and the output buffer then took a further
    /// 0-200us to show correct values -- and why adding console verbosity or a
    /// settle "fixed" depth: both simply spent that time somewhere else.
    ///
    /// This is a data gate, not a settle: when the stamp is already present --
    /// which it will be whenever the driver was not the first thing to look --
    /// nothing is waited for at all. If the stamp never arrives the retire
    /// proceeds anyway, so the worst case is exactly the previous behaviour.
    fn await_compute_stamp(&self, queue: &mut G17PSksmQueue) {
        const POLL_US: u32 = 5;
        if crate::g17_config::COMPLETION_STAMP == 0 || !self.compute_stamp_armed {
            return;
        }
        let budget_us = *module_parameters::g17p_completion_stamp_us.value();
        let mut waited_us = 0u32;
        loop {
            match queue.compute.read_compute_stamp() {
                Ok(0) => {}
                Ok(stamp) => {
                    if waited_us != 0 {
                        // Log the first wait unconditionally -- a quiet run
                        // must still say whether this gate does any work --
                        // and later ones only while tracing is on.
                        let first = self.compute_stamp_waits.fetch_add(1, Ordering::Relaxed) == 0;
                        if first || Self::compute_trace_enabled(queue.submission_ordinal) {
                            dev_info!(
                                self.resources.dev().as_ref(),
                                "G17P completion stamp: {:#x} arrived {} us after the KSM record (waits so far {})\n",
                                stamp,
                                waited_us,
                                self.compute_stamp_waits.load(Ordering::Relaxed)
                            );
                        }
                    }
                    return;
                }
                Err(error) => {
                    dev_warn!(
                        self.resources.dev().as_ref(),
                        "G17P completion stamp unreadable ({:?}); retiring on the KSM record alone\n",
                        error
                    );
                    return;
                }
            }
            if waited_us >= budget_us {
                // Rate-limited: hardware shows a real minority of submissions
                // never publish a stamp (19 of 300), and one console write per
                // occurrence costs about a millisecond, which would distort the
                // very timing this gate measures. Report the first, then every
                // 64th, always with the running total.
                let misses = self.compute_stamp_misses.fetch_add(1, Ordering::Relaxed) + 1;
                if misses == 1 || misses % 64 == 0 {
                    dev_warn!(
                        self.resources.dev().as_ref(),
                        "G17P completion stamp never published within {} us ({} so far); retiring on the KSM record alone\n",
                        budget_us,
                        misses
                    );
                }
                return;
            }
            fsleep(Delta::from_micros(POLL_US as i64));
            waited_us = waited_us.saturating_add(POLL_US);
        }
    }

    pub(crate) fn snapshot_compute_timeout(
        &mut self,
        queue: &mut G17PSksmQueue,
    ) -> Result<G17PComputeTimeoutSnapshot> {
        Ok(G17PComputeTimeoutSnapshot {
            graph: queue.compute.snapshot_initial_channel_group()?,
            channel: self.resources.snapshot_compute_channel()?,
            channel_scan: self.resources.snapshot_all_primary_work_channels()?,
            channel_control_prefix: queue.logical_channel_control.as_deref()
                .or(self.compute_channel_control.as_ref())
                .ok_or(EINVAL)?
                .snapshot_prefix()?,
            control: self.resources.control_counters()?,
        })
    }

    /// Publish newly appended kicks for a hardware-registered queue.
    pub(crate) fn add_sksm_kicks(
        &self,
        registers: &regs::Resources,
        queue: &mut G17PSksmQueue,
        add_count: u32,
    ) -> Result {
        if queue.hardware_state != G17PSksmQueueHardwareState::HardwareRegistered
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

    #[test]
    fn ta_hardware_buffer_ids_match_native_order_exhaustion_and_reuse() {
        let mut allocator = G17PHardwareBufferIdAllocator::new();
        for expected in 0..G17P_TA_HARDWARE_BUFFER_ID_COUNT as u32 {
            assert_eq!(allocator.allocate(), Ok(expected));
        }
        assert_eq!(
            allocator.allocate(),
            Err(G17PHardwareBufferIdError::Exhausted)
        );

        allocator.release(42).unwrap();
        assert_eq!(
            allocator.release(42),
            Err(G17PHardwareBufferIdError::DoubleFree)
        );
        assert_eq!(allocator.allocate(), Ok(42));
        assert_eq!(
            allocator.release(G17P_TA_HARDWARE_BUFFER_ID_COUNT as u32),
            Err(G17PHardwareBufferIdError::InvalidId)
        );
    }

    #[test]
    fn command_buffer_state_allocator_is_shared_monotonic_and_skips_63() {
        let mut allocator = G17PCommandBufferStateAllocator::new();
        for expected in 0u32..63 {
            let word = allocator.allocate_one().unwrap();
            assert_eq!(word, expected);
            assert!(allocator.release_one(word));
        }
        assert_eq!(allocator.allocate_one(), Some(64));
        assert!(allocator.release_one(64));

        allocator.next = 128;
        let replacement = allocator.allocate_one().unwrap();
        assert_eq!(replacement, 128);
        // An ABA/stale completion cannot release the wrapped replacement.
        assert!(!allocator.release_one(0));
        assert!(allocator.release_one(replacement));

        let mut full = G17PCommandBufferStateAllocator::new();
        for _ in 0..63 {
            assert!(full.allocate_one().is_some());
        }
        assert_eq!(full.allocate_one(), None);
    }

    #[test]
    fn command_buffer_state_allocator_preserves_full_u32_across_wrap() {
        let mut allocator = G17PCommandBufferStateAllocator::new();
        allocator.next = u32::MAX - 1;
        let word = allocator.allocate_one().unwrap();
        assert_eq!(word, u32::MAX - 1);
        assert_eq!(word & 63, 62);
        assert!(allocator.release_one(word));
    }

    #[test]
    fn native_render_visibility_and_ta_order_are_strict() {
        assert!(g17p_native_fragment_only_visible(
            0, 0, 4, 4, 1, 1, 0, 0
        ));
        assert!(!g17p_native_fragment_only_visible(
            0, 4, 4, 4, 1, 1, 0, 0
        ));
        assert!(!g17p_native_fragment_only_visible(
            0, 0, 4, 4, 1, 1, 1, 0
        ));
        assert!(g17p_native_tiling_fully_visible(4, 4, 1, 1));
        assert!(!g17p_native_tiling_fully_visible(2, 4, 1, 1));

        let mut order = G17PNativeRenderPublishOrder::after_fragment_doorbell();
        assert_eq!(
            order.advance(G17PNativeRenderPublishPhase::TilingSksm),
            Err(())
        );
        for phase in [
            G17PNativeRenderPublishPhase::TilingPrefix,
            G17PNativeRenderPublishPhase::TilingSksm,
            G17PNativeRenderPublishPhase::TilingKick,
            G17PNativeRenderPublishPhase::TilingOuter,
            G17PNativeRenderPublishPhase::PoolTransition,
        ] {
            assert_eq!(order.advance(phase), Ok(()));
        }
        assert_eq!(
            order.advance(G17PNativeRenderPublishPhase::PoolTransition),
            Err(())
        );
    }

    #[test]
    fn native_render_mode_three_waits_for_exact_fragment_outer_consumption() {
        assert!(!g17p_native_fragment_first_mode(0));
        assert!(!g17p_native_fragment_first_mode(1));
        assert!(g17p_native_fragment_first_mode(2));
        assert!(g17p_native_fragment_first_mode(3));
        assert!(!g17p_native_fragment_first_mode(4));

        assert!(!g17p_native_fragment_preack_mode(2));
        assert!(g17p_native_fragment_preack_mode(3));

        assert!(!g17p_native_fragment_outer_consumed([0, 0, 1], 1));
        assert!(!g17p_native_fragment_outer_consumed([0, 1, 1], 1));
        assert!(g17p_native_fragment_outer_consumed([1, 0, 1], 1));
        assert!(g17p_native_fragment_outer_consumed([1, 1, 1], 1));
        assert!(!g17p_native_fragment_outer_consumed([1, 1, 2], 1));
    }

    #[test]
    fn render_outer_deferral_mode_matrix_is_explicit() {
        use g17_resources::G17PDeferredOuterPublication::{Both, None, Tiling};

        assert_eq!(g17p_render_deferred_outer_mode(0, false), Both);
        assert_eq!(g17p_render_deferred_outer_mode(1, false), None);
        assert_eq!(g17p_render_deferred_outer_mode(2, false), Tiling);
        assert_eq!(g17p_render_deferred_outer_mode(3, false), Tiling);
        for mode in 0..=3 {
            assert_eq!(g17p_render_deferred_outer_mode(mode, true), Both);
        }
    }

    #[test]
    fn bootstrap_async_flist_releases_only_after_type13_and_paired_render() {
        let mut lifecycle = G17PBootstrapUmaLifecycle::new();
        assert_eq!(
            lifecycle.match_completion(lifecycle.request.unwrap()),
            Err(G17PBootstrapUmaLifecycleError::WrongReferenceCount)
        );
        lifecycle.retire_prepare_reference().unwrap();
        lifecycle.match_completion(lifecycle.request.unwrap()).unwrap();
        assert_eq!(lifecycle.hardware_buffer_references, 2);
        assert_eq!(lifecycle.pending_release_count, 2);
        assert_eq!(lifecycle.pending_release(), None);
        let release = lifecycle.retire_render_references().unwrap().unwrap();
        assert_eq!(
            release,
            G17PBootstrapUmaRelease {
                sequence: 0,
                hardware_buffer_id: 0,
            }
        );
        assert_eq!(lifecycle.hardware_buffer_references, 0);
        assert_eq!(lifecycle.pending_release_count, 0);
        assert_eq!(lifecycle.pending_release(), Some(release));
        lifecycle.mark_published(release).unwrap();
        lifecycle.mark_sent(29).unwrap();
        assert_eq!(lifecycle.pending_release(), None);
        assert!(lifecycle.release_sent());
        assert_eq!(lifecycle.release_producer_after, Some(29));
        assert!(lifecycle.mark_completed(28).is_err());
        lifecycle.mark_completed(29).unwrap();
        assert!(!lifecycle.release_sent());
        assert_eq!(lifecycle.release_producer_after, None);
    }

    #[test]
    fn bootstrap_async_flist_release_backpressure_tracks_exact_producer() {
        fn sent_lifecycle(target: u32) -> G17PBootstrapUmaLifecycle {
            let mut lifecycle = G17PBootstrapUmaLifecycle::new();
            lifecycle.retire_prepare_reference().unwrap();
            lifecycle
                .match_completion(lifecycle.request.unwrap())
                .unwrap();
            let release = lifecycle.retire_render_references().unwrap().unwrap();
            lifecycle.mark_published(release).unwrap();
            lifecycle.mark_sent(target).unwrap();
            lifecycle
        }

        let mut delayed = sent_lifecycle(29);
        assert_eq!(delayed.observe_release_consumer(28, 29), Ok(false));
        assert!(delayed.release_sent());
        assert_eq!(delayed.observe_release_consumer(29, 29), Ok(true));
        assert_eq!(delayed.state, G17PBootstrapUmaRequestState::Completed);

        let mut timed_out = sent_lifecycle(41);
        for _ in 0..20 {
            assert_eq!(timed_out.observe_release_consumer(40, 41), Ok(false));
        }
        assert!(timed_out.release_sent());
        assert_eq!(timed_out.release_producer_after, Some(41));
        assert_eq!(timed_out.observe_release_consumer(41, 41), Ok(true));

        // Consumer may pass an earlier release when later owners or TVB
        // replies publish before the host observes the shared ring again.
        let mut passed = sent_lifecycle(41);
        assert_eq!(passed.observe_release_consumer(43, 45), Ok(true));

        // The same pending-interval test is exact across 255 -> 0 wrap.
        let mut wrapped = sent_lifecycle(0);
        assert_eq!(wrapped.observe_release_consumer(255, 1), Ok(false));
        assert_eq!(wrapped.observe_release_consumer(1, 2), Ok(true));
        assert_eq!(g17p_control_ring_target_consumed(255, 1, 1), Ok(false));
        assert_eq!(g17p_control_ring_target_consumed(0, 1, 1), Ok(false));
        assert_eq!(g17p_control_ring_target_consumed(1, 1, 1), Ok(true));
    }

    #[test]
    fn bootstrap_release_stays_ready_until_backpressure_clears() {
        let mut lifecycle = G17PBootstrapUmaLifecycle::new();
        lifecycle.retire_prepare_reference().unwrap();
        lifecycle
            .match_completion(lifecycle.request.unwrap())
            .unwrap();
        let release = lifecycle.retire_render_references().unwrap().unwrap();
        for _ in 0..1000 {
            assert_eq!(lifecycle.pending_release(), Some(release));
            assert_eq!(lifecycle.state, G17PBootstrapUmaRequestState::Ready);
        }
        lifecycle.mark_published(release).unwrap();
        assert_eq!(lifecycle.pending_release(), None);
        assert_eq!(
            lifecycle.mark_published(release),
            Err(G17PBootstrapUmaLifecycleError::WrongPhase)
        );
    }

    #[test]
    fn bootstrap_async_flist_rejects_raw_or_duplicate_type13_records() {
        let mut lifecycle = G17PBootstrapUmaLifecycle::new();
        lifecycle.retire_prepare_reference().unwrap();
        let exact = lifecycle.request.unwrap();
        for wrong in [
            G17PBootstrapUmaCompletion {
                cookie: exact.cookie + 0x40,
                ..exact
            },
            G17PBootstrapUmaCompletion {
                count: exact.count - 1,
                ..exact
            },
            G17PBootstrapUmaCompletion {
                hardware_buffer_id: 1,
                ..exact
            },
        ] {
            assert_eq!(
                lifecycle.match_completion(wrong),
                Err(G17PBootstrapUmaLifecycleError::WrongCompletion)
            );
        }
        lifecycle.match_completion(exact).unwrap();
        assert_eq!(
            lifecycle.match_completion(exact),
            Err(G17PBootstrapUmaLifecycleError::WrongPhase)
        );
    }

    #[test]
    fn shared_flist_holds_both_render_pairs_until_the_last_retirement() {
        let mut first = G17PBootstrapUmaLifecycle::new();
        first.retire_prepare_reference().unwrap();
        first.match_completion(first.request.unwrap()).unwrap();
        assert_eq!((first.hardware_buffer_references, first.pending_release_count), (2, 2));

        let mut both = G17PBootstrapUmaLifecycle::acquire(Some(&first), 1).unwrap();
        // The second command pair is another reference to the same native
        // hardware-buffer owner, not a new registration incarnation.
        assert_eq!(both.sequence, first.sequence);
        assert_eq!((both.hardware_buffer_references, both.pending_release_count), (5, 4));
        both.retire_prepare_reference().unwrap();
        assert_eq!((both.hardware_buffer_references, both.pending_release_count), (4, 4));
        assert_eq!(both.retire_render_references().unwrap(), None);
        assert_eq!((both.hardware_buffer_references, both.pending_release_count), (2, 2));
        let release = both.retire_render_references().unwrap().unwrap();
        assert_eq!(release.sequence, 0);
        assert_eq!((both.hardware_buffer_references, both.pending_release_count), (0, 0));
    }

    #[test]
    fn retained_flist_leases_grow_once_and_release_every_captured_generation() {
        let mut previous = None;
        let mut grows = 0;
        let mut releases = 0;
        for ordinal in 0..3 {
            let sequence = u64::from(g17_submission::g17p_render_flist_generation(ordinal));
            let mut lease = G17PBootstrapUmaLifecycle::acquire(previous.as_ref(), sequence).unwrap();
            assert_eq!(lease.sequence, sequence);
            assert_eq!(lease.pending_release_count, 2);
            assert_eq!(lease.hardware_buffer_references, if ordinal == 0 { 4 } else { 3 });
            assert_eq!(lease.retire_render_references(), Err(G17PBootstrapUmaLifecycleError::WrongPhase));
            lease.retire_prepare_reference().unwrap();
            if let Some(request) = lease.request {
                grows += 1;
                assert_eq!(lease.retire_render_references(), Err(G17PBootstrapUmaLifecycleError::WrongPhase));
                lease.match_completion(request).unwrap();
            }
            assert_eq!(lease.state, G17PBootstrapUmaRequestState::Ready);
            assert!(lease.retire_prepare_reference().is_err());
            let release = lease.retire_render_references().unwrap().unwrap();
            releases += 1;
            assert_eq!(release.sequence, sequence);
            assert_eq!(release.hardware_buffer_id, G17P_BOOTSTRAP_UMA_HARDWARE_BUFFER_ID);
            assert!(lease.retire_render_references().is_err());
            assert!(G17PBootstrapUmaLifecycle::acquire(Some(&lease), sequence + 1).is_err());
            // Submission ordinals are wide; channel ring producers remain u32.
            let producer_after = u32::try_from(ordinal + 20).unwrap();
            assert!(lease.mark_completed(producer_after).is_err());
            assert!(lease.mark_published(G17PBootstrapUmaRelease { sequence: sequence + 1, ..release }).is_err());
            lease.mark_published(release).unwrap();
            assert!(G17PBootstrapUmaLifecycle::acquire(Some(&lease), sequence + 1).is_err());
            lease.mark_sent(producer_after).unwrap();
            assert!(G17PBootstrapUmaLifecycle::acquire(Some(&lease), sequence + 1).is_err());
            assert!(lease.mark_completed(producer_after + 1).is_err());
            lease.mark_completed(producer_after).unwrap();
            assert!(lease.mark_completed(producer_after).is_err());
            assert!(lease.mark_published(release).is_err());
            assert!(G17PBootstrapUmaLifecycle::acquire(Some(&lease), sequence).is_err());
            assert!(G17PBootstrapUmaLifecycle::acquire(Some(&lease), sequence + 2).is_err());
            previous = Some(lease);
        }
        assert_eq!((grows, releases), (1, 3));
        assert!(G17PBootstrapUmaLifecycle::acquire(None, 1).is_err());
        let mut wrong = previous.unwrap();
        wrong.hardware_buffer_id += 1;
        assert!(G17PBootstrapUmaLifecycle::acquire(Some(&wrong), 3).is_err());
        let mut warm = G17PBootstrapUmaLifecycle::acquire(previous.as_ref(), 3).unwrap();
        let stale = G17PBootstrapUmaLifecycle::new().request.unwrap();
        assert_eq!(warm.match_completion(stale), Err(G17PBootstrapUmaLifecycleError::WrongCompletion));
        warm.retire_prepare_reference().unwrap();
        assert_eq!(warm.match_completion(stale), Err(G17PBootstrapUmaLifecycleError::WrongPhase));
    }

    #[test]
    fn private_render_flist_owner_grows_once_then_reacquires_warm() {
        let owner = (129, 0xffff_fc20_1100_0000, 0x70_0410_0000, 0x70_0410_0440);
        let mut cold = G17PBootstrapUmaLifecycle::acquire_owner(
            None, 0, owner.0, owner.1, owner.2, owner.3, true,
        ).unwrap();
        let request = cold.request.expect("cold owner must grow");
        assert_eq!((cold.hardware_buffer_references, cold.pending_release_count), (4, 2));
        cold.retire_prepare_reference().unwrap();
        cold.match_completion(request).unwrap();
        let release = cold.retire_render_references().unwrap().unwrap();
        cold.mark_published(release).unwrap();
        cold.mark_sent(17).unwrap();
        cold.mark_completed(17).unwrap();

        let mut warm = G17PBootstrapUmaLifecycle::acquire_owner(
            Some(&cold), 1, owner.0, owner.1, owner.2, owner.3, false,
        ).unwrap();
        assert_eq!(warm.sequence, 1);
        assert_eq!(warm.request, None);
        assert_eq!((warm.hardware_buffer_references, warm.pending_release_count), (3, 2));
        warm.retire_prepare_reference().unwrap();
        assert_eq!(warm.state, G17PBootstrapUmaRequestState::Ready);
        assert!(warm.retire_render_references().unwrap().is_some());
    }

    #[test]
    fn private_render_flist_replacement_waits_for_exact_old_owner_retirement() {
        let mut old = G17PBootstrapUmaLifecycle::acquire_owner(
            None, 0, 130, 0xffff_fc20_1200_0000, 0x70_0420_0000,
            0x70_0420_0440, true,
        ).unwrap();
        old.retire_prepare_reference().unwrap();
        old.match_completion(old.request.unwrap()).unwrap();
        assert_eq!(
            G17PBootstrapUmaLifecycle::acquire_owner(
                Some(&old), 1, 130, 0xffff_fc20_1300_0000,
                0x70_0430_0000, 0x70_0430_0440, true,
            ),
            Err(G17PBootstrapUmaLifecycleError::WrongPhase),
        );
        let release = old.retire_render_references().unwrap().unwrap();
        old.mark_published(release).unwrap();
        old.mark_sent(31).unwrap();
        old.mark_completed(31).unwrap();

        let replacement = G17PBootstrapUmaLifecycle::acquire_owner(
            Some(&old), 1, 130, 0xffff_fc20_1300_0000,
            0x70_0430_0000, 0x70_0430_0440, true,
        ).unwrap();
        assert_eq!(replacement.sequence, 1);
        assert_eq!(replacement.hardware_buffer_references, 4);
        assert_eq!(replacement.request.unwrap().shared_control, 0xffff_fc20_1300_0000);
    }

    #[test]
    fn private_render_flist_same_owner_overlap_keeps_identity_until_last_pair() {
        let owner = (131, 0xffff_fc20_1400_0000, 0x70_0440_0000, 0x70_0440_0440);
        let mut first = G17PBootstrapUmaLifecycle::acquire_owner(
            None, 0, owner.0, owner.1, owner.2, owner.3, true,
        ).unwrap();
        first.retire_prepare_reference().unwrap();
        first.match_completion(first.request.unwrap()).unwrap();
        let mut both = G17PBootstrapUmaLifecycle::acquire_owner(
            Some(&first), 1, owner.0, owner.1, owner.2, owner.3, false,
        ).unwrap();
        assert_eq!(both.sequence, first.sequence);
        assert_eq!(both.request, None);
        assert_eq!((both.hardware_buffer_references, both.pending_release_count), (5, 4));
        both.retire_prepare_reference().unwrap();
        assert_eq!(both.retire_render_references().unwrap(), None);
        let release = both.retire_render_references().unwrap().unwrap();
        assert_eq!((release.sequence, release.hardware_buffer_id), (0, owner.0));
    }

    fn decode_hex<const N: usize>(source: &[u8]) -> [u8; N] {
        let source = source.strip_suffix(b"\n").unwrap_or(source);
        assert_eq!(source.len(), N * 2);
        let nibble = |value: u8| match value {
            b'0'..=b'9' => value - b'0',
            b'a'..=b'f' => value - b'a' + 10,
            _ => panic!("invalid hex fixture"),
        };
        let mut out = [0u8; N];
        let mut index = 0;
        while index < N {
            out[index] = nibble(source[index * 2]) << 4 | nibble(source[index * 2 + 1]);
            index += 1;
        }
        out
    }

    fn instruction(bytes: &[u8], index: usize) -> u32 {
        u32::from_le_bytes(bytes[index * 4..index * 4 + 4].try_into().unwrap())
    }

    fn sign_extend(value: u64, bits: u32) -> i64 {
        ((value << (64 - bits)) as i64) >> (64 - bits)
    }

    fn adrp_target(pc: u64, value: u32) -> u64 {
        assert_eq!(value & 0x9f00_0000, 0x9000_0000);
        let immediate = (((value as u64 >> 5) & 0x7ffff) << 2)
            | ((value as u64 >> 29) & 0x3);
        (pc & !0xfff).wrapping_add((sign_extend(immediate, 21) << 12) as u64)
    }

    fn branch_target(pc: u64, value: u32) -> u64 {
        assert_eq!(value & 0x7c00_0000, 0x1400_0000);
        pc.wrapping_add((sign_extend((value & 0x03ff_ffff) as u64, 26) << 2) as u64)
    }

    fn conditional_branch_target(pc: u64, value: u32) -> u64 {
        assert_eq!(value & 0xff00_0010, 0x5400_0000);
        pc.wrapping_add((sign_extend(((value >> 5) & 0x7ffff) as u64, 19) << 2) as u64)
    }

    fn load_store_unsigned_offset(value: u32) -> usize {
        let scale = value >> 30;
        (((value >> 10) & 0xfff) as usize) << scale
    }

    fn add_immediate(value: u32) -> usize {
        assert_eq!(value & 0x7f00_0000, 0x1100_0000);
        (((value >> 10) & 0xfff) as usize) << (((value >> 22) & 1) * 12)
    }

    fn wide_immediate(value: u32) -> u64 {
        let shift = ((value >> 21) & 0x3) * 16;
        ((value as u64 >> 5) & 0xffff) << shift
    }

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
        assert_eq!(
            T8140_MANAGER_CONFIG.submission_backend,
            identity::T8140_G17P_SUBMISSION_BACKEND
        );
        assert_eq!(T8140_MANAGER_CONFIG.work_channel_count, 12);
    }

    #[test]
    fn type4_diagnostic_fields_match_the_pinned_a000_consumers() {
        const GFX_DATA_BASE: u64 = 0x0000_0100_01d7_c000;
        const GFX_DATA_SIZE: u64 = 0x13_4000;
        assert_eq!(
            GFX_DATA_BASE + (G17P_SCHEDULER_ROOT_LINK_ADDRESS - G17P_GFX_LINK_DATA_BASE),
            G17P_SCHEDULER_ROOT_PHYSICAL_ADDRESS,
        );
        assert_eq!(G17P_GFX_LINK_DATA_BASE, 0xffff_fc00_0004_c000);
        assert_eq!(G17P_SCHEDULER_ROOT_PHYSICAL_ADDRESS, 0x0000_0100_01ea_7190);
        assert_eq!(
            G17P_SCHEDULER_ROOT_PHYSICAL_ADDRESS + G17P_SCHEDULER_ROOT_STATE_OFFSET as u64,
            0x0000_0100_01ea_71f8,
        );
        assert_eq!(
            GFX_DATA_BASE + (G17P_RUNTIME_MAIN_GLOBAL_LINK_ADDRESS - G17P_GFX_LINK_DATA_BASE),
            G17P_RUNTIME_MAIN_GLOBAL_PHYSICAL_ADDRESS,
        );
        assert_eq!(G17P_RUNTIME_MAIN_GLOBAL_PHYSICAL_ADDRESS, 0x0000_0100_01ea_7030);
        assert_eq!(
            GFX_DATA_BASE + (G17P_MBI_GLOBAL_LINK_ADDRESS - G17P_GFX_LINK_DATA_BASE),
            0x0000_0100_01d9_e840,
        );
        assert_eq!(G17P_MBI_STATIC_ARRAY_LINK_ADDRESS, 0xffff_fc00_0006_e800);
        assert_eq!(G17P_MBI_GROUP_COUNT, 8);
        assert_eq!(G17P_MBI_TRACE_CAPACITY, 128);
        assert_eq!(G17P_MBI_TRACE_RECORD_SIZE, 0x18);
        assert_eq!(
            GFX_DATA_BASE
                + (G17P_PRIMARY_PEER_LEASE_LINK_ADDRESS - G17P_GFX_LINK_DATA_BASE),
            G17P_PRIMARY_PEER_LEASE_PHYSICAL_ADDRESS,
        );
        assert_eq!(G17P_PRIMARY_PEER_LEASE_PHYSICAL_ADDRESS, 0x0000_0100_01ea_7798);
        assert_eq!(
            G17P_GFX1_PHYSICAL_DATA_BASE
                + (G17P_GFX1_PEER_LEASE_LINK_ADDRESS - G17P_GFX1_LINK_DATA_BASE),
            G17P_GFX1_PEER_LEASE_PHYSICAL_ADDRESS,
        );
        assert_eq!(G17P_GFX1_PEER_LEASE_PHYSICAL_ADDRESS, 0x0000_0100_01fd_11c8);
        assert!(
            G17P_SCHEDULER_ROOT_PHYSICAL_ADDRESS + G17P_SCHEDULER_ROOT_SNAPSHOT_SIZE as u64
                <= GFX_DATA_BASE + GFX_DATA_SIZE
        );
        assert_eq!(G17P_TYPE4_EP_HANDLER_LINK_ADDRESS, 0xffff_fc00_0002_2e24);
        assert_eq!(G17P_TYPE4_THUNK_LINK_ADDRESS, 0xffff_fc00_0002_635c);
        assert_eq!(G17P_TYPE4_DRAIN_LINK_ADDRESS, 0xffff_fc00_0000_3a74);
        assert_eq!(G17P_TYPE4_PENDING_MASK, 1 << 17);
        assert_eq!(
            G17P_SCHEDULER_DEFERRED_REQUEUE_LINK_ADDRESS,
            0xffff_fc00_0002_5ea4
        );
        assert_eq!(
            G17P_SCHEDULER_ROOT_LINK_ADDRESS + G17P_SCHEDULER_ROOT_PENDING_OFFSET as u64,
            0xffff_fc00_0017_7190
        );
        assert_eq!(
            G17P_SCHEDULER_ROOT_LINK_ADDRESS + G17P_SCHEDULER_ROOT_DEFERRED_OFFSET as u64,
            0xffff_fc00_0017_71e0
        );
        assert_eq!(
            G17P_SCHEDULER_ROOT_LINK_ADDRESS + G17P_SCHEDULER_ROOT_STATE_OFFSET as u64,
            0xffff_fc00_0017_71f8
        );
        let control = g17_initdata::channel_table_entry_range(
            g17_initdata::CONTROL_CHANNEL_INDEX,
        )
        .unwrap();
        assert_eq!(control.start, G17P_TYPE4_MAIN_CONTROL_POINTER_OFFSETS[0]);
        assert_eq!(
            G17P_TYPE4_MAIN_CONTROL_POINTER_OFFSETS,
            [control.start, control.start + 8, control.start + 0x10]
        );
    }

    #[test]
    fn pinned_j700_a000_instruction_fixtures_prove_the_type4_pointer_chain() {
        const BOOT_MAIN: &[u8] =
            include_bytes!("fixtures/g17p_j700_a000/boot-main-config-global.hex");
        const SELECTOR: &[u8] = include_bytes!("fixtures/g17p_j700_a000/type4-selector.hex");
        const PUBLISH: &[u8] = include_bytes!("fixtures/g17p_j700_a000/type4-publish.hex");
        const COMMON: &[u8] =
            include_bytes!("fixtures/g17p_j700_a000/type4-common-publish.hex");
        const TAKE: &[u8] =
            include_bytes!("fixtures/g17p_j700_a000/type4-pending-take.hex");
        const DEFERRED: &[u8] =
            include_bytes!("fixtures/g17p_j700_a000/scheduler-deferred.hex");
        const THUNK: &[u8] = include_bytes!("fixtures/g17p_j700_a000/type4-thunk.hex");
        const DISPATCH: &[u8] =
            include_bytes!("fixtures/g17p_j700_a000/type4-dispatch-record.hex");
        const CURSORS: &[u8] =
            include_bytes!("fixtures/g17p_j700_a000/control-drain-cursors.hex");

        let boot = decode_hex::<0x30>(BOOT_MAIN);
        assert_eq!(
            adrp_target(0xffff_fc00_0001_e9dc, instruction(&boot, 8)),
            0xffff_fc00_0017_7000
        );
        assert_eq!(load_store_unsigned_offset(instruction(&boot, 9)), 0x30);

        let selector = decode_hex::<0x1c>(SELECTOR);
        let extract = instruction(&selector, 0);
        assert_eq!((extract >> 5) & 0x1f, 1);
        assert_eq!(extract & 0x1f, 8);
        assert_eq!((extract >> 16) & 0x3f, 48);
        assert_eq!(
            ((extract >> 10) & 0x3f) - ((extract >> 16) & 0x3f) + 1,
            6
        );
        let compare_type4 = instruction(&selector, 5);
        assert_eq!((compare_type4 >> 5) & 0x1f, 8);
        assert_eq!((compare_type4 >> 10) & 0xfff, 4);
        assert_eq!(
            conditional_branch_target(0xffff_fc00_0002_2db0, instruction(&selector, 6)),
            G17P_TYPE4_EP_HANDLER_LINK_ADDRESS
        );

        let publish = decode_hex::<0x10>(PUBLISH);
        assert_eq!(
            adrp_target(G17P_TYPE4_EP_HANDLER_LINK_ADDRESS, instruction(&publish, 0)),
            0xffff_fc00_0017_7000
        );
        assert_eq!(add_immediate(instruction(&publish, 1)), 0x190);
        assert_eq!(wide_immediate(instruction(&publish, 2)), G17P_TYPE4_PENDING_MASK);
        assert_eq!(
            branch_target(
                G17P_TYPE4_EP_HANDLER_LINK_ADDRESS + 0xc,
                instruction(&publish, 3)
            ),
            0xffff_fc00_0002_2ebc
        );

        let common = decode_hex::<0xc>(COMMON);
        assert_eq!(instruction(&common, 0), 0xf8e8_3008);
        let take = decode_hex::<0x20>(TAKE);
        assert_eq!(instruction(&take, 4), 0xf83c_1275);

        let deferred = decode_hex::<0x14>(DEFERRED);
        assert_eq!(
            adrp_target(
                G17P_SCHEDULER_DEFERRED_REQUEUE_LINK_ADDRESS,
                instruction(&deferred, 0)
            ),
            0xffff_fc00_0017_7000
        );
        assert_eq!(load_store_unsigned_offset(instruction(&deferred, 1)), 0x1e0);
        assert_eq!(load_store_unsigned_offset(instruction(&deferred, 3)), 0x1e0);
        assert_eq!(
            0xffff_fc00_0017_7000 + 0x1e0,
            G17P_SCHEDULER_ROOT_LINK_ADDRESS + G17P_SCHEDULER_ROOT_DEFERRED_OFFSET as u64
        );

        let thunk = decode_hex::<0x28>(THUNK);
        assert_eq!(
            adrp_target(G17P_TYPE4_THUNK_LINK_ADDRESS, instruction(&thunk, 0)),
            0xffff_fc00_0017_7000
        );
        assert_eq!(load_store_unsigned_offset(instruction(&thunk, 1)), 0x1f8);
        assert_eq!(instruction(&thunk, 2), 0x721d_051f);
        assert_eq!(
            conditional_branch_target(
                G17P_TYPE4_THUNK_LINK_ADDRESS + 0xc,
                instruction(&thunk, 3)
            ),
            G17P_TYPE4_THUNK_LINK_ADDRESS + 0x24
        );
        assert_eq!(
            adrp_target(G17P_TYPE4_THUNK_LINK_ADDRESS + 0x10, instruction(&thunk, 4)),
            0xffff_fc00_0017_7000
        );
        assert_eq!(load_store_unsigned_offset(instruction(&thunk, 5)), 0x30);
        assert_eq!(load_store_unsigned_offset(instruction(&thunk, 6)), 0x1b0);
        assert_eq!(load_store_unsigned_offset(instruction(&thunk, 7)), 0);
        assert_eq!(
            branch_target(G17P_TYPE4_THUNK_LINK_ADDRESS + 0x20, instruction(&thunk, 8)),
            G17P_TYPE4_DRAIN_LINK_ADDRESS
        );

        let dispatch = decode_hex::<0x18>(DISPATCH);
        assert_eq!(
            u64::from_le_bytes(dispatch[0..8].try_into().unwrap()),
            G17P_TYPE4_PENDING_MASK
        );
        let signed_target = u64::from_le_bytes(dispatch[8..16].try_into().unwrap());
        assert_eq!(signed_target >> 48, 0x8030);
        assert_eq!(
            signed_target & 0xffff_ffff,
            G17P_TYPE4_THUNK_LINK_ADDRESS & 0xffff_ffff
        );
        assert_eq!(u64::from_le_bytes(dispatch[16..24].try_into().unwrap()), 0);

        let cursors = decode_hex::<0x18>(CURSORS);
        assert_eq!(load_store_unsigned_offset(instruction(&cursors, 1)), 0x1a8);
        assert_eq!(load_store_unsigned_offset(instruction(&cursors, 3)), 0x1a0);
        assert_eq!(
            G17P_TYPE4_MAIN_CONTROL_POINTER_OFFSETS,
            [0x1a0, 0x1a8, 0x1b0]
        );
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
            platform.sksm_queue_geometry,
            g17_submission::g17_sksm_queue_geometry(
                g17_submission::G17ChannelAbiTarget::A18ProG17P,
                g17_submission::G17ClSubmissionBackend::G17PHal200Hybrid,
            )
            .unwrap()
        );
        assert_eq!(platform.sksm_queue_geometry.validate(), Ok(()));
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
                    sksm_scratch: geometry,
                    ..T8140_G17P_PLATFORM_CONFIG
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
