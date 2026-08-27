// SPDX-License-Identifier: GPL-2.0-only OR MIT

#![cfg_attr(not(test), allow(dead_code))]

//! Byte-confirmed subset of the G17 GPU submission-transport producer ABI (the
//! "SKSM producer").
//!
//! This module is a pure codec for the driver->firmware submission path — the
//! wire form that hands a compiled command buffer to the firmware for
//! execution. It allocates no memory, starts no ASC, sends no mailbox message,
//! and touches no MMIO. It models the EP 0x21 work-submission doorbell, the
//! channel/work-queue ring producer indices, the SKSM kick word/record, and the
//! host queue-timestamp plus `addKicks` MMIO producer. It also models the exact
//! host-side reset images for `_AGFIChannelState` and
//! `AGXUncachedFWChannelMem`, the A18 Pro SKSM hardware-bridge programming
//! equation, and its four completion-queue descriptors. The provenance ledger
//! explicitly separates byte-grounded G17 fields from inherited producer
//! records whose G17 consumer offsets still need first-hand confirmation. The
//! native G17 channel-command dispatcher is also modeled for the exact A18
//! Pro/G17P identity: it reads a little-endian u32 tag at command offset zero,
//! keeps unresolved and nested SKU meanings out of the top-level enum, and
//! separates the host's `Nop=17` name from the firmware's fatal rejection of
//! that value.
//!
//! Grounding is first-hand from the on-disk M5 role macho `GFX.a010.macho`
//! (`g17s a0, SKSM Enabled`), all three pinned A18 Pro `g17p` firmware variants
//! (`a000`, `b000`, and `b100`), and the pinned G17P/G17G/G17X host kexts. The
//! native G17P channel-state ABI is admitted only for the A18 Pro identity;
//! the M5-family identities stay fail-closed.
//! Firmware addresses are VAs (0xfffffc0000000000 + fileoff - 0x4000). See
//! `research/gpu/GPU_AGX3_G17_SUBMISSION_2026-08-25.md`.
//!
//! Two-signal discipline, mirroring `g17_rtkit`/`g17_boot`:
//! [`GROUNDED_SUBMISSION_PRODUCER_AVAILABLE`] states that the *codec* is real
//! and cited; [`EXECUTABLE_SUBMISSION_TRANSPORT_AVAILABLE`] states whether a
//! submission can be driven to completion offline. It is `false`: the SKSM
//! completion path is a hardware kick FIFO the firmware reads through indexed
//! MMIO scratch registers. The host publication form, runtime scratch geometry,
//! and mapped-window offsets are grounded. A Linux publication, its firmware
//! consumption, the round trip, and the completion IRQ remain unproved. See
//! [`SUBMISSION_LIVE_GAPS`].

#[cfg(not(test))]
use crate::g17_manager::G17SksmScratchGeometry;
#[cfg(test)]
#[path = "g17_manager.rs"]
mod g17_manager;
#[cfg(test)]
use g17_manager::G17SksmScratchGeometry;

/// The submission producer subset is byte-grounded against the M5 firmware.
/// See [`SUBMISSION_ABI_LEDGER`] for the inherited, still-unconfirmed pieces.
pub(crate) const GROUNDED_SUBMISSION_PRODUCER_AVAILABLE: bool = true;

/// Whether a submission can be driven to completion. `false`: see
/// [`SUBMISSION_LIVE_GAPS`]. This is what keeps `SUBMISSION_TRANSPORT`
/// fail-closed in `g17_boot::pre_handoff_gate`.
pub(crate) const EXECUTABLE_SUBMISSION_TRANSPORT_AVAILABLE: bool =
    SUBMISSION_LIVE_GAPS.all_proven();

// ---------------------------------------------------------------------------
// 1. EP 0x21 TX work-submission doorbell — byte-identical to AGX2.
//    Grounded from the firmware doorbell decoder at GFX 0xfffffc00000251cc /
//    the TX case 0xfffffc0000025278 (see the research note §1).
// ---------------------------------------------------------------------------

/// GPU doorbell endpoint (gfx_ints). Matches `gpu.rs` `EP_DOORBELL`.
pub(crate) const EP_DOORBELL: u8 = 0x21;

/// TX (channel-kick) doorbell tag, `0x83 << 48`. Matches `gpu.rs`
/// `MSG_TX_DOORBELL`.
const MSG_TX_DOORBELL: u64 = 0x83 << 48;

/// The firmware reads a 6-bit doorbell type from bits [53:48]
/// (`ubfx x8, x1, #48, #6` @ 0x251dc).
const DOORBELL_TYPE_SHIFT: u32 = 48;
const DOORBELL_TYPE_BITS: u32 = 6;
const DOORBELL_TYPE_MASK: u64 = (1 << DOORBELL_TYPE_BITS) - 1;

/// `0x83 & 0x3f = 3`: the channel-kick (TX) doorbell case (`b.eq 0x25278`).
pub(crate) const DOORBELL_TYPE_TX: u8 = 3;
/// `0x84 & 0x3f = 4`: the firmware-control doorbell case (`b.eq 0x25268`).
pub(crate) const DOORBELL_TYPE_FWCTL: u8 = 4;
/// `0x85 & 0x3f = 5`: the halt doorbell case.
pub(crate) const DOORBELL_TYPE_HALT: u8 = 5;

/// TX pipe field is `msg & 0x3` (`and w8, w1, #0x3` @ 0x25278).
const DOORBELL_PIPE_MASK: u64 = 0x3;
/// TX index field is bits [4:2] (`tst x1, #0x1c` @ 0x2527c; `index << 2`).
const DOORBELL_INDEX_SHIFT: u32 = 2;
const DOORBELL_INDEX_MASK: u64 = 0x7;

/// Well-known TX-doorbell payloads used for firmware wakeups, matching
/// `gpu.rs` `DOORBELL_KICKFW` / `DOORBELL_DEVCTRL`.
pub(crate) const DOORBELL_KICKFW: u8 = 0x10;
pub(crate) const DOORBELL_DEVCTRL: u8 = 0x11;

/// The three GPU submission pipes. The firmware rejects `pipe_type == 3`
/// (`cmp w8, #0x3 ; b.eq 0x252f4` — no such pipe).
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
#[repr(u8)]
pub(crate) enum PipeType {
    Vertex = 0,
    Fragment = 1,
    Compute = 2,
}

/// Fail-closed refusals from the submission codec.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) enum SubmissionError {
    /// A doorbell index outside the 3-bit [4:2] field.
    DoorbellIndexOutOfRange,
    /// A doorbell whose type field is not the TX (channel-kick) type.
    NotATxDoorbell,
    /// A doorbell that decodes to the reserved pipe value 3.
    ReservedPipe,
    /// A kick word with bits set outside the modeled 47-bit form.
    KickWordUnmodeledBits,
    /// A queue id outside the 7-bit [46:40] field / 128-slot table.
    QueueIdOutOfRange,
    /// A SKSM timestamp outside the 40-bit [39:0] field.
    KickTimestampOutOfRange,
    /// The runtime queue geometry must contain between 1 and 256 stamps.
    KickStampCountOutOfRange,
    /// The timestamp's low-byte slot does not exist in this queue geometry.
    KickStampIndexOutOfRange,
    /// A zero-byte entry stride would alias every queue entry.
    KickEntryStrideZero,
    /// Multiplying the slot by the runtime entry stride overflowed u32.
    KickEntryOffsetOverflow,
    /// The runtime MMIO aperture selector or base offset overflowed u32.
    KickMmioOffsetOverflow,
    /// The A18 Pro SKSM queue data-master field is limited to TA/3D/CL.
    KickQueueDataMasterOutOfRange,
    /// The A18 Pro SKSM completion-queue selector is outside the host range.
    KickQueueCompletionSelectorOutOfRange,
    /// The A18 Pro SKSM entry address cannot be represented by the host mask.
    KickQueueEntryAddressUnencodable,
    /// The A18 Pro completion descriptor ordinal is outside its four slots.
    CompletionQueueOrdinalOutOfRange,
    /// A present A18 Pro completion queue cannot have zero entries.
    CompletionQueueCapacityZero,
    /// A ring with zero entries cannot advance a producer index.
    RingSizeZero,
    /// A producer index must identify an entry inside its ring.
    RingIndexOutOfRange,
    /// Advancing the producer would make it collide with the consumer.
    RingFull,
    /// G17 exposes four priority groups in its runtime channel table.
    ChannelPriorityOutOfRange,
    /// This native channel layout is not grounded for the selected identity.
    ChannelAbiUnconfirmed,
    /// The top-level channel-command tag ABI is not grounded for this target.
    CommandAbiUnconfirmed,
    /// A top-level channel command must expose its complete four-byte tag.
    CommandHeaderTooShort,
    /// The host/firmware meaning of this in-range top-level tag is unresolved.
    CommandTagUnresolved,
    /// This value is not a grounded G17P top-level command tag.
    CommandTagInvalid,
    /// The host names this tag, but the matched firmware rejects it fatally.
    CommandTagRejectedByFirmware,
}

/// Encode a work-submission doorbell exactly as `gpu.rs::run_job` does:
/// `MSG_TX_DOORBELL | pipe_type | (index << 2)`.
///
/// `index` is the queue priority (0..=7 fits the firmware's [4:2] field).
pub(crate) const fn encode_tx_doorbell(pipe: PipeType, index: u8) -> Result<u64, SubmissionError> {
    if index as u64 > DOORBELL_INDEX_MASK {
        return Err(SubmissionError::DoorbellIndexOutOfRange);
    }
    Ok(MSG_TX_DOORBELL | (pipe as u64) | ((index as u64) << DOORBELL_INDEX_SHIFT))
}

/// Extract the firmware's 6-bit doorbell type from bits [53:48].
pub(crate) const fn doorbell_type(message: u64) -> u8 {
    ((message >> DOORBELL_TYPE_SHIFT) & DOORBELL_TYPE_MASK) as u8
}

/// Decode a TX doorbell to `(pipe_type, index)`, applying the firmware's exact
/// masks and its `pipe == 3` rejection.
pub(crate) const fn decode_tx_doorbell(message: u64) -> Result<(PipeType, u8), SubmissionError> {
    if doorbell_type(message) != DOORBELL_TYPE_TX {
        return Err(SubmissionError::NotATxDoorbell);
    }
    let pipe = match message & DOORBELL_PIPE_MASK {
        0 => PipeType::Vertex,
        1 => PipeType::Fragment,
        2 => PipeType::Compute,
        _ => return Err(SubmissionError::ReservedPipe),
    };
    let index = ((message >> DOORBELL_INDEX_SHIFT) & DOORBELL_INDEX_MASK) as u8;
    Ok((pipe, index))
}

/// The firmware's pending-pipe bit for a submitted pipe: `1 << pipe_type`
/// (`mov w9,#1 ; lsl w8,w9,w8` @ 0x2529c; atomic-OR'd into the global at
/// 0xfffffc00001049e8).
pub(crate) const fn pending_pipe_bit(pipe: PipeType) -> u32 {
    1u32 << (pipe as u32)
}

// ---------------------------------------------------------------------------
// 2. Legacy AGX2 channel state. These constants are retained only to make the
//    inherited producer explicit; they are not a native-G17 admission.
// ---------------------------------------------------------------------------

/// AGX2 FW-owned consumer index.
pub(crate) const AGX2_CHANNEL_STATE_READ_PTR_OFFSET: u32 = 0x00;
/// AGX2 driver-owned producer index.
pub(crate) const AGX2_CHANNEL_STATE_WRITE_PTR_OFFSET: u32 = 0x20;
/// Total AGX2 `ChannelState` size.
pub(crate) const AGX2_CHANNEL_STATE_SIZE: u32 = 0x40;

// ---------------------------------------------------------------------------
// 2a. G17 runtime channel descriptors -- native AGX3 layout.
//
//     Grounded first-hand from the G17S firmware recovery scanner at GFX
//     0xfffffc000000ff8c-0xfffffc00000103dc. runtime_pointers+0x20 holds four
//     priority groups of three descriptors (vertex, fragment, compute). Each
//     descriptor is 0x20 bytes: the firmware dereferences cursor pointers at
//     +0x00/+0x08/+0x10 and obtains the command-entry array from +0x18. It
//     advances through that array in 0x18-byte records. The final descriptor
//     ends at runtime_pointers+0x1a0, immediately before the independent cursor
//     pair at +0x1a0/+0x1a8.
// ---------------------------------------------------------------------------

/// First G17 channel descriptor in `runtime_pointers`.
pub(crate) const G17_CHANNEL_DESCRIPTOR_TABLE_OFFSET: u32 = 0x20;
/// Vertex, fragment, and compute descriptors per priority group.
pub(crate) const G17_CHANNEL_DESCRIPTORS_PER_PRIORITY: u32 = 3;
/// Priority groups scanned by the G17 firmware.
pub(crate) const G17_CHANNEL_PRIORITY_COUNT: u8 = 4;
/// Total descriptor count: four priorities times three submission pipes.
pub(crate) const G17_CHANNEL_DESCRIPTOR_COUNT: u32 = 12;
/// Native G17 descriptor stride. AGX2 `ChannelRing` is only 0x10 bytes.
pub(crate) const G17_CHANNEL_DESCRIPTOR_STRIDE: u32 = 0x20;
/// G17 command-entry stride used by all three pipe rings.
pub(crate) const G17_CHANNEL_ENTRY_STRIDE: u32 = 0x18;

/// Pointer fields inside one native G17 channel descriptor.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) struct G17ChannelDescriptorLayout {
    /// Pointer to the cursor advanced after an entry has been dispatched.
    pub(crate) dispatch_cursor: u32,
    /// Pointer to the cursor advanced after the entry range is made coherent.
    pub(crate) coherency_cursor: u32,
    /// Pointer to the producer cursor sampled before scanning the ring.
    pub(crate) producer_cursor: u32,
    /// GPU pointer to the first 0x18-byte command entry.
    pub(crate) entries: u32,
}

pub(crate) const G17_CHANNEL_DESCRIPTOR_LAYOUT: G17ChannelDescriptorLayout =
    G17ChannelDescriptorLayout {
        dispatch_cursor: 0x00,
        coherency_cursor: 0x08,
        producer_cursor: 0x10,
        entries: 0x18,
    };

/// Return the runtime-pointer byte offset of a native G17 pipe descriptor.
///
/// The table is priority-major and pipe-minor. Refuse priority values outside
/// the four groups that the firmware scans instead of aliasing the independent
/// cursor pair at `runtime_pointers+0x1a0`.
pub(crate) const fn g17_channel_descriptor_offset(
    priority: u8,
    pipe: PipeType,
) -> Result<u32, SubmissionError> {
    if priority >= G17_CHANNEL_PRIORITY_COUNT {
        return Err(SubmissionError::ChannelPriorityOutOfRange);
    }

    let index = priority as u32 * G17_CHANNEL_DESCRIPTORS_PER_PRIORITY + pipe as u32;
    Ok(G17_CHANNEL_DESCRIPTOR_TABLE_OFFSET + index * G17_CHANNEL_DESCRIPTOR_STRIDE)
}

// ---------------------------------------------------------------------------
// 2b. G17 host channel-reset images -- native AGX3 layout.
//
//     Grounded first-hand from byte-identical `AGXChannel::resetChannelState`
//     implementations in the pinned G17P host at 0xfffffe00089f2a60, G17G at
//     0xfffffe0008b225ac, and G17X at 0xfffffe0008ae6a6c. The host clears all
//     0x80 bytes of `_AGFIChannelState`, then writes the fields below. Its
//     separate `AGXUncachedFWChannelMem` reset touches only seven u32 fields
//     through +0x63; bytes between them are deliberately preserved here.
//
//     These are independent host-reset records. They do not prove that the
//     inherited AGX2 ChannelState/RingState names in sections 2/3 describe the
//     G17 firmware consumer, so those ledger entries remain fail-closed.
// ---------------------------------------------------------------------------

/// Full byte extent cleared by the G17 host for `_AGFIChannelState`.
pub(crate) const G17_HOST_CHANNEL_STATE_RESET_SIZE: usize = 0x80;
/// Last byte touched by the G17 host's uncached-channel reset stores.
pub(crate) const G17_UNCACHED_CHANNEL_RESET_WRITE_EXTENT: usize = 0x64;
/// All three in-scope host families use the identical reset layout.
pub(crate) const G17_HOST_CHANNEL_RESET_TARGETS_CONFIRMED: bool = true;

/// Offsets written after the host clears `_AGFIChannelState`.
///
/// Names are intentionally structural until the firmware consumer supplies
/// first-hand semantics for every field.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) struct G17HostChannelStateResetLayout {
    pub(crate) gpu_va_00: usize,
    pub(crate) gpu_va_08: usize,
    pub(crate) gpu_va_10: usize,
    pub(crate) zero_18: usize,
    pub(crate) zero_1c: usize,
    pub(crate) zero_20: usize,
    pub(crate) sentinel_24: usize,
    pub(crate) fixed_4_28: usize,
    pub(crate) sentinel_40: usize,
    pub(crate) word_44: usize,
    pub(crate) zero_58: usize,
    pub(crate) gpu_va_70: usize,
}

pub(crate) const G17_HOST_CHANNEL_STATE_RESET_LAYOUT: G17HostChannelStateResetLayout =
    G17HostChannelStateResetLayout {
        gpu_va_00: 0x00,
        gpu_va_08: 0x08,
        gpu_va_10: 0x10,
        zero_18: 0x18,
        zero_1c: 0x1c,
        zero_20: 0x20,
        sentinel_24: 0x24,
        fixed_4_28: 0x28,
        sentinel_40: 0x40,
        word_44: 0x44,
        zero_58: 0x58,
        gpu_va_70: 0x70,
    };

/// Runtime values copied by the host into the structural G17 reset image.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) struct G17HostChannelStateResetValues {
    pub(crate) gpu_va_00: u64,
    pub(crate) gpu_va_08: u64,
    pub(crate) gpu_va_10: u64,
    pub(crate) word_44: u32,
    pub(crate) gpu_va_70: u64,
}

/// Fields touched in `AGXUncachedFWChannelMem` during a G17 channel reset.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) struct G17UncachedChannelResetLayout {
    pub(crate) zero_00: usize,
    pub(crate) zero_10: usize,
    pub(crate) zero_20: usize,
    pub(crate) zero_30: usize,
    pub(crate) zero_40: usize,
    pub(crate) sentinel_50: usize,
    pub(crate) ring_entry_count_60: usize,
}

pub(crate) const G17_UNCACHED_CHANNEL_RESET_LAYOUT: G17UncachedChannelResetLayout =
    G17UncachedChannelResetLayout {
        zero_00: 0x00,
        zero_10: 0x10,
        zero_20: 0x20,
        zero_30: 0x30,
        zero_40: 0x40,
        sentinel_50: 0x50,
        ring_entry_count_60: 0x60,
    };

fn put_u32_le(raw: &mut [u8], offset: usize, value: u32) {
    raw[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

fn put_u64_le(raw: &mut [u8], offset: usize, value: u64) {
    raw[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}

/// Build the exact 0x80-byte `_AGFIChannelState` reset image.
pub(crate) fn encode_g17_host_channel_state_reset(
    values: G17HostChannelStateResetValues,
) -> [u8; G17_HOST_CHANNEL_STATE_RESET_SIZE] {
    let mut raw = [0u8; G17_HOST_CHANNEL_STATE_RESET_SIZE];
    let layout = G17_HOST_CHANNEL_STATE_RESET_LAYOUT;

    put_u64_le(&mut raw, layout.gpu_va_00, values.gpu_va_00);
    put_u64_le(&mut raw, layout.gpu_va_08, values.gpu_va_08);
    put_u64_le(&mut raw, layout.gpu_va_10, values.gpu_va_10);
    put_u32_le(&mut raw, layout.zero_18, 0);
    put_u32_le(&mut raw, layout.zero_1c, 0);
    put_u32_le(&mut raw, layout.zero_20, 0);
    put_u32_le(&mut raw, layout.sentinel_24, u32::MAX);
    put_u32_le(&mut raw, layout.fixed_4_28, 4);
    put_u32_le(&mut raw, layout.sentinel_40, u32::MAX);
    put_u32_le(&mut raw, layout.word_44, values.word_44);
    put_u32_le(&mut raw, layout.zero_58, 0);
    put_u64_le(&mut raw, layout.gpu_va_70, values.gpu_va_70);
    raw
}

/// Apply only the seven uncached reset stores made by the G17 host.
///
/// The host does not clear the intervening bytes in `resetChannelState`; a
/// caller-provided image therefore stays intact outside the proven stores.
pub(crate) fn apply_g17_uncached_channel_reset(
    raw: &mut [u8; G17_UNCACHED_CHANNEL_RESET_WRITE_EXTENT],
    ring_entry_count: u32,
) {
    let layout = G17_UNCACHED_CHANNEL_RESET_LAYOUT;

    put_u32_le(raw, layout.zero_00, 0);
    put_u32_le(raw, layout.zero_10, 0);
    put_u32_le(raw, layout.zero_20, 0);
    put_u32_le(raw, layout.zero_30, 0);
    put_u32_le(raw, layout.zero_40, 0);
    put_u32_le(raw, layout.sentinel_50, u32::MAX);
    put_u32_le(raw, layout.ring_entry_count_60, ring_entry_count);
}

// ---------------------------------------------------------------------------
// 2c. A18 Pro / G17P native channel state.
//
//     This is deliberately identity-scoped. The producer is the pinned G17P
//     host `AGXChannel::writeChannelCommandPointer` at
//     0xfffffe00089f5a88-0xfffffe00089f5b58. The matching firmware consumers
//     are `armfw_g17p.a000` 0xfffffc0000012388-0xfffffc00000124f4 and the
//     resume path 0xfffffc0000012514-0xfffffc0000012690. The matching b000
//     and b100 channel blocks are byte-identical to each other and implement
//     the same accesses at 0xfffffc00000122f4-0xfffffc0000012460 and
//     0xfffffc00000124a4-0xfffffc0000012664.
//
//     `_AGFIChannelState` is the cached indirection record: +0x00 points to
//     the uncached state, +0x08 points to the u64 command-pointer ring, and
//     +0x1c holds the cached coherency cursor. `AGXUncachedFWChannelMem` holds
//     u32 cursors at +0x00/+0x30/+0x40/+0x50 and a u32 ring entry count at
//     +0x60. This is not AGX2's read@0/write@0x20 `ChannelState`.
// ---------------------------------------------------------------------------

/// Silicon/host identity used to admit native G17 submission ABIs.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) enum G17ChannelAbiTarget {
    /// A18 Pro: G17P host and `g17p` firmware.
    A18ProG17P,
    /// M5: G17G host. No native channel consumer is admitted by this pass.
    M5G17G,
    /// M5 Pro/Max: G17X host with `g17s` firmware. Kept separate from G17P.
    M5ProMaxG17X,
}

/// Cached `_AGFIChannelState` fields consumed by the A18 Pro firmware.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) struct G17PCachedChannelStateLayout {
    pub(crate) uncached_state_gpu_va: u32,
    pub(crate) command_pointer_ring_gpu_va: u32,
    pub(crate) coherency_cursor: u32,
    pub(crate) record_size: u32,
}

/// Uncached channel fields shared by the A18 Pro host and firmware.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) struct G17PUncachedChannelStateLayout {
    /// Firmware dispatch/consumer cursor.
    pub(crate) consumer: u32,
    /// Cursor published after the command-pointer range is made coherent.
    pub(crate) coherency_shadow: u32,
    /// Host producer cursor, published after a `dmb ish`.
    pub(crate) producer: u32,
    /// Recovery target selected instead of the accelerator-entry snapshot.
    pub(crate) recovery_target: u32,
    /// Runtime ring depth in 8-byte command-pointer entries.
    pub(crate) ring_entry_count: u32,
    pub(crate) cursor_width: u32,
    pub(crate) write_extent: u32,
}

/// Complete admitted A18 Pro channel layout.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) struct G17PChannelAbiLayout {
    pub(crate) cached: G17PCachedChannelStateLayout,
    pub(crate) uncached: G17PUncachedChannelStateLayout,
    pub(crate) command_pointer_entry_bytes: u32,
    pub(crate) accelerator_target_width: u32,
}

pub(crate) const G17P_CHANNEL_ABI: G17PChannelAbiLayout = G17PChannelAbiLayout {
    cached: G17PCachedChannelStateLayout {
        uncached_state_gpu_va: 0x00,
        command_pointer_ring_gpu_va: 0x08,
        coherency_cursor: 0x1c,
        record_size: 0x80,
    },
    uncached: G17PUncachedChannelStateLayout {
        consumer: 0x00,
        coherency_shadow: 0x30,
        producer: 0x40,
        recovery_target: 0x50,
        ring_entry_count: 0x60,
        cursor_width: 4,
        write_extent: 0x64,
    },
    command_pointer_entry_bytes: 8,
    accelerator_target_width: 2,
};

/// The host executes `dmb ish` after storing the u64 command pointer and
/// before publishing the next u32 producer cursor.
pub(crate) const G17P_CHANNEL_PRODUCER_REQUIRES_DMB_ISH: bool = true;

/// Return the native channel ABI only for the exact identity whose producer
/// and consumer were both inspected. M5-family targets remain fail-closed.
pub(crate) const fn g17_channel_abi(
    target: G17ChannelAbiTarget,
) -> Result<G17PChannelAbiLayout, SubmissionError> {
    match target {
        G17ChannelAbiTarget::A18ProG17P => Ok(G17P_CHANNEL_ABI),
        G17ChannelAbiTarget::M5G17G | G17ChannelAbiTarget::M5ProMaxG17X => {
            Err(SubmissionError::ChannelAbiUnconfirmed)
        }
    }
}

/// A host write plan for one G17P command-pointer ring publication.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) struct G17PChannelWritePlan {
    /// Byte offset of the u64 slot written before the barrier.
    pub(crate) entry_byte_offset: u64,
    /// u32 producer value published after the barrier.
    pub(crate) next_producer: u32,
}

/// Validate and calculate the write performed by the G17P host producer.
///
/// This pure helper does not execute the required barrier; callers must store
/// the u64 command GPU VA, execute `dmb ish`, then store `next_producer` to
/// uncached offset +0x40.
pub(crate) const fn prepare_g17p_channel_write(
    target: G17ChannelAbiTarget,
    producer: u32,
    consumer: u32,
    ring_entry_count: u32,
) -> Result<G17PChannelWritePlan, SubmissionError> {
    match g17_channel_abi(target) {
        Ok(_) => {}
        Err(error) => return Err(error),
    }

    if consumer >= ring_entry_count {
        return Err(if ring_entry_count == 0 {
            SubmissionError::RingSizeZero
        } else {
            SubmissionError::RingIndexOutOfRange
        });
    }

    let next_producer = match ring_next(producer, ring_entry_count) {
        Ok(next) => next,
        Err(error) => return Err(error),
    };
    if next_producer == consumer {
        return Err(SubmissionError::RingFull);
    }

    Ok(G17PChannelWritePlan {
        entry_byte_offset: producer as u64 * G17P_CHANNEL_ABI.command_pointer_entry_bytes as u64,
        next_producer,
    })
}

/// Layout-only forms used to make accidental field drift a compile-time
/// error. The intervening words remain unnamed because their semantics are
/// outside the admitted producer/consumer subset.
#[allow(dead_code)]
#[repr(C)]
struct G17PCachedChannelStateLayoutAssert {
    uncached_state_gpu_va: u64,
    command_pointer_ring_gpu_va: u64,
    _unknown_10: u64,
    _unknown_18: u32,
    coherency_cursor: u32,
    _unknown_20: [u8; 0x60],
}

#[allow(dead_code)]
#[repr(C)]
struct G17PUncachedChannelStateLayoutAssert {
    consumer: u32,
    _unknown_04: [u8; 0x0c],
    _unknown_10: u32,
    _unknown_14: [u8; 0x0c],
    _unknown_20: u32,
    _unknown_24: [u8; 0x0c],
    coherency_shadow: u32,
    _unknown_34: [u8; 0x0c],
    producer: u32,
    _unknown_44: [u8; 0x0c],
    recovery_target: u32,
    _unknown_54: [u8; 0x0c],
    ring_entry_count: u32,
}

const _: () = {
    assert!(core::mem::size_of::<G17PCachedChannelStateLayoutAssert>() == 0x80);
    assert!(
        core::mem::offset_of!(G17PCachedChannelStateLayoutAssert, uncached_state_gpu_va) == 0x00
    );
    assert!(
        core::mem::offset_of!(
            G17PCachedChannelStateLayoutAssert,
            command_pointer_ring_gpu_va
        ) == 0x08
    );
    assert!(core::mem::offset_of!(G17PCachedChannelStateLayoutAssert, coherency_cursor) == 0x1c);

    assert!(core::mem::size_of::<G17PUncachedChannelStateLayoutAssert>() == 0x64);
    assert!(core::mem::offset_of!(G17PUncachedChannelStateLayoutAssert, consumer) == 0x00);
    assert!(core::mem::offset_of!(G17PUncachedChannelStateLayoutAssert, coherency_shadow) == 0x30);
    assert!(core::mem::offset_of!(G17PUncachedChannelStateLayoutAssert, producer) == 0x40);
    assert!(core::mem::offset_of!(G17PUncachedChannelStateLayoutAssert, recovery_target) == 0x50);
    assert!(core::mem::offset_of!(G17PUncachedChannelStateLayoutAssert, ring_entry_count) == 0x60);
};

// ---------------------------------------------------------------------------
// 3. Legacy AGX2 work-queue RingState. The A18 Pro ARM firmware directly
//    confirms only done@0; the SKSM hardware consumer keeps the remaining
//    rptr/wptr/size offsets unconfirmed, so this record is not admitted as a
//    native G17P layout.
// ---------------------------------------------------------------------------

pub(crate) const AGX2_RING_STATE_GPU_DONEPTR_OFFSET: u32 = 0x00;
pub(crate) const AGX2_RING_STATE_GPU_RPTR_OFFSET: u32 = 0x30;
pub(crate) const AGX2_RING_STATE_CPU_WPTR_OFFSET: u32 = 0x40;
pub(crate) const AGX2_RING_STATE_RB_SIZE_OFFSET: u32 = 0x50;

/// AGX2 work-queue entries are u64 command GPU virtual addresses.
pub(crate) const AGX2_RING_ENTRY_BYTES: u32 = 8;

/// The shipping producer's maximum outstanding event slots
/// (`workqueue.rs::MAX_JOB_SLOTS`). This is not the command-ring depth: the
/// current Linux ring has `WQ_SIZE == 0x500`. Its numerical equality with the
/// 7-bit QID maximum is only a coincidence and carries no ABI meaning.
pub(crate) const HOST_MAX_OUTSTANDING_JOB_SLOTS: u32 = 127;

/// Advance a producer/consumer index within a ring of `size` entries.
///
/// Fail closed instead of panicking on a zero-sized ring or accepting a stale
/// producer index. A valid `index` is always below `u32::MAX`, so `index + 1`
/// cannot overflow after these checks.
pub(crate) const fn ring_next(index: u32, size: u32) -> Result<u32, SubmissionError> {
    if size == 0 {
        return Err(SubmissionError::RingSizeZero);
    }
    if index >= size {
        return Err(SubmissionError::RingIndexOutOfRange);
    }
    Ok((index + 1) % size)
}

// ---------------------------------------------------------------------------
// 4. SKSM kick word + per-QID kick-record table (AGX3-specific).
//    Grounded from GFX 0x10c88-0x10d2c. The canonical kick-word *encoder* lives
//    in `g17_rtkit::KickWord`; these constants are the same firmware bytes,
//    re-cited so this module stands alone. `g17_boot` cross-checks them against
//    `g17_rtkit` to prove no drift.
// ---------------------------------------------------------------------------

/// TS occupies bits [39:0] (`and x0,x5,#0xffffffffff` @ 0x10c8c). 40-bit, per
/// the firmware print `"TS=0x%010llx"`.
pub(crate) const KICK_TS_BITS: u32 = 40;
pub(crate) const KICK_TS_MASK: u64 = (1u64 << KICK_TS_BITS) - 1;
/// QID occupies bits [46:40] (`ubfx x1,x5,#40,#7` @ 0x10c90).
pub(crate) const KICK_QID_SHIFT: u32 = 40;
pub(crate) const KICK_QID_BITS: u32 = 7;
pub(crate) const KICK_QID_MASK: u8 = (1u8 << KICK_QID_BITS) - 1;
/// Bit 46 also selects one of two 64-bit valid-queue mask words
/// (`ubfx x6,x5,#46,#1` @ 0x10c9c).
pub(crate) const KICK_SELECTOR_BIT: u32 = 46;
/// Bits [63:47] are unmodeled and must be zero.
pub(crate) const KICK_RESERVED_MASK: u64 = !((1u64 << (KICK_SELECTOR_BIT + 1)) - 1);

/// Per-QID kick-record table base offset from the valid-mask base
/// (`add x2,x22,#0x10` @ 0x10c94).
pub(crate) const KICK_RECORD_TABLE_BASE: u32 = 0x10;
/// Per-QID record stride (`add x2,x2,x1,lsl#5` @ 0x10c98).
pub(crate) const KICK_RECORD_STRIDE: u32 = 0x20;
/// 128 queue slots: 7-bit QID, two 64-bit valid-mask words selected by bit 46.
pub(crate) const KICK_RECORD_COUNT: u32 = 128;

/// Fields of one 0x20-byte per-QID kick record (written across 0x10cb0-0x10d2c).
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) struct KickRecordLayout {
    /// `+0x00` u64: first/min TS seen for this QID.
    pub(crate) ts_first: u32,
    /// `+0x08` u64: last/max TS seen for this QID.
    pub(crate) ts_last: u32,
    /// `+0x10` u64: pending kick-slot bitmask.
    pub(crate) slot_mask: u32,
    /// `+0x18` u32: valid/first flag.
    pub(crate) valid_flag: u32,
    /// `+0x1c` u32: winning kick-slot index.
    pub(crate) slot_index: u32,
}

/// The exact offsets, so a change to the firmware layout fails a test.
pub(crate) const KICK_RECORD_LAYOUT: KickRecordLayout = KickRecordLayout {
    ts_first: 0x00,
    ts_last: 0x08,
    slot_mask: 0x10,
    valid_flag: 0x18,
    slot_index: 0x1c,
};

/// Byte offset of one firmware kick record: `0x10 + QID * 0x20`. Mirrors
/// `g17_rtkit::kick_queue_record_offset`.
pub(crate) const fn kick_record_offset(queue_id: u8) -> Result<u32, SubmissionError> {
    if queue_id > KICK_QID_MASK {
        Err(SubmissionError::QueueIdOutOfRange)
    } else {
        Ok(KICK_RECORD_TABLE_BASE + queue_id as u32 * KICK_RECORD_STRIDE)
    }
}

/// The firmware-visible fields of a decoded SKSM kick word.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) struct SksmKickWord {
    pub(crate) timestamp: u64,
    pub(crate) queue_id: u8,
    pub(crate) mask_selector: u8,
}

/// Encode the exact raw SKSM kick fields, leaving bits [63:47] clear.
pub(crate) const fn encode_kick_word(timestamp: u64, queue_id: u8) -> Result<u64, SubmissionError> {
    if timestamp & !KICK_TS_MASK != 0 {
        return Err(SubmissionError::KickTimestampOutOfRange);
    }
    if queue_id > KICK_QID_MASK {
        return Err(SubmissionError::QueueIdOutOfRange);
    }
    Ok(timestamp | ((queue_id as u64) << KICK_QID_SHIFT))
}

/// Decode a kick word with the firmware's exact bitfields, refusing the
/// unmodeled upper bits. Kept parallel to `g17_rtkit::KickWord::decode_strict`.
pub(crate) const fn decode_kick_word(word: u64) -> Result<SksmKickWord, SubmissionError> {
    if word & KICK_RESERVED_MASK != 0 {
        return Err(SubmissionError::KickWordUnmodeledBits);
    }
    Ok(SksmKickWord {
        timestamp: word & KICK_TS_MASK,
        queue_id: ((word >> KICK_QID_SHIFT) as u8) & KICK_QID_MASK,
        mask_selector: ((word >> KICK_SELECTOR_BIT) & 1) as u8,
    })
}

// ---------------------------------------------------------------------------
// 5. Host SKSM kick-queue producer transaction (AGX3-specific).
//
//    Grounded from G17S KSMKickQueue::encodeKickEntry at host text
//    0xfffffe0008b51f74-0xfffffe0008b521c4. The current timestamp at queue
//    +0x48 selects `entry_base + low8(timestamp) * entry_stride`; the current
//    40-bit timestamp is returned for the doorbell, then the low-byte slot is
//    advanced modulo runtime `num_stamps`. Wrapping the slot to zero increments
//    the 32-bit stamp in bits [39:8]. This coupling is load-bearing: choosing a
//    different entry slot from the one encoded in the doorbell desynchronizes
//    the queue and can reach the firmware's out-of-order rejection path.
// ---------------------------------------------------------------------------

/// Stamp sequence in bits [39:8] of a host SKSM timestamp.
pub(crate) const KICK_STAMP_SHIFT: u32 = 8;
pub(crate) const KICK_STAMP_MASK: u64 = u32::MAX as u64;
/// Entry/stamp slot in bits [7:0] of a host SKSM timestamp.
pub(crate) const KICK_STAMP_INDEX_MASK: u64 = 0xff;
/// The low-byte slot representation permits at most 256 runtime stamps.
pub(crate) const KICK_NUM_STAMPS_MAX: u16 = 256;

/// Decoded host-side timestamp state before a kick is published.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) struct KickTimestamp {
    pub(crate) stamp: u32,
    pub(crate) stamp_index: u8,
}

/// Pure result of preparing one host SKSM kick-queue producer transaction.
///
/// The caller must still encode the complete target-specific entry, execute
/// the recovered `dmb ish`, update the submitted counter, and perform the live
/// target-specific `addKicks` MMIO publication. This structure only makes the
/// entry selection, queue identity, and next producer timestamp indivisible.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) struct PreparedKickSubmission {
    /// Slot selected from the current timestamp's low byte.
    pub(crate) entry_index: u8,
    /// Byte offset from the runtime queue entry base.
    pub(crate) entry_offset: u32,
    /// Current 40-bit timestamp consumed by this entry.
    pub(crate) timestamp: u64,
    /// Queue whose target-specific `addKicks` tag must be published later.
    pub(crate) queue_id: u8,
    /// Timestamp stored at queue +0x48 for the following submission.
    pub(crate) next_timestamp: u64,
}

/// Decode the exact host timestamp split: stamp[31:0] followed by slot[7:0].
pub(crate) const fn decode_kick_timestamp(
    timestamp: u64,
) -> Result<KickTimestamp, SubmissionError> {
    if timestamp & !KICK_TS_MASK != 0 {
        return Err(SubmissionError::KickTimestampOutOfRange);
    }

    Ok(KickTimestamp {
        stamp: ((timestamp >> KICK_STAMP_SHIFT) & KICK_STAMP_MASK) as u32,
        stamp_index: (timestamp & KICK_STAMP_INDEX_MASK) as u8,
    })
}

/// Encode the host timestamp layout without a queue ID.
pub(crate) const fn encode_kick_timestamp(timestamp: KickTimestamp) -> u64 {
    ((timestamp.stamp as u64) << KICK_STAMP_SHIFT) | timestamp.stamp_index as u64
}

/// Prepare the exact entry-slot/doorbell/next-timestamp tuple for one kick.
///
/// `num_stamps` and `entry_stride` are intentionally runtime inputs: Apple's
/// host loads both from the per-queue geometry rather than hard-coding them.
/// Invalid geometry is rejected before an entry address or doorbell is
/// produced. No shared memory, counter, barrier, MMIO, or endpoint state is
/// mutated here.
pub(crate) const fn prepare_kick_submission(
    current_timestamp: u64,
    queue_id: u8,
    num_stamps: u16,
    entry_stride: u32,
) -> Result<PreparedKickSubmission, SubmissionError> {
    let current = match decode_kick_timestamp(current_timestamp) {
        Ok(timestamp) => timestamp,
        Err(err) => return Err(err),
    };

    if num_stamps == 0 || num_stamps > KICK_NUM_STAMPS_MAX {
        return Err(SubmissionError::KickStampCountOutOfRange);
    }
    if current.stamp_index as u16 >= num_stamps {
        return Err(SubmissionError::KickStampIndexOutOfRange);
    }
    if entry_stride == 0 {
        return Err(SubmissionError::KickEntryStrideZero);
    }

    let index = current.stamp_index as u32;
    if index != 0 && entry_stride > u32::MAX / index {
        return Err(SubmissionError::KickEntryOffsetOverflow);
    }
    let entry_offset = index * entry_stride;

    if queue_id > KICK_QID_MASK as u8 {
        return Err(SubmissionError::QueueIdOutOfRange);
    }

    let next_index = ((current.stamp_index as u16 + 1) % num_stamps) as u8;
    let next_stamp = if next_index == 0 {
        current.stamp.wrapping_add(1)
    } else {
        current.stamp
    };
    let next_timestamp = encode_kick_timestamp(KickTimestamp {
        stamp: next_stamp,
        stamp_index: next_index,
    });

    Ok(PreparedKickSubmission {
        entry_index: current.stamp_index,
        entry_offset,
        timestamp: current_timestamp,
        queue_id,
        next_timestamp,
    })
}

// ---------------------------------------------------------------------------
// 6. Host addKicks MMIO publication (AGX3-specific).
//
//    The host does not write the firmware-side `timestamp | (qid << 40)` word
//    directly.  The pinned G17P/G17G/G17X implementations publish a
//    target-specific 64-bit queue tag followed by the 32-bit add count to two
//    runtime-selected scratch offsets. The host adds nine times the runtime
//    stride to both bases for every target. This is a pure description of the
//    two ordered stores; it performs no MMIO.
// ---------------------------------------------------------------------------

/// Host producer family with an independently recovered `addKicks` tag.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) enum SksmAddKicksTarget {
    /// A18 Pro / HAL200 (`AGXKSMKickQueueG16`).
    G17P,
    /// M5 / HAL300 G target.
    G17G,
    /// M5 Pro/Max HAL300 X-family target (G17S/G17C host implementation).
    G17X,
}

/// Exact ordered values and offsets for one host `addKicks` publication.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) struct PreparedSksmAddKicksMmio {
    /// First store: 64-bit target tag to `tag_offset`.
    pub(crate) tag_offset: u32,
    pub(crate) tag: u64,
    /// Second store: 32-bit number of newly published kicks.
    pub(crate) count_offset: u32,
    pub(crate) add_count: u32,
}

const fn sksm_add_kicks_tag(target: SksmAddKicksTarget, queue_id: u8) -> u64 {
    match target {
        // mac17p 0x8a57b28-0x8a57b34: base bit 42, qid at bit 43.
        SksmAddKicksTarget::G17P => G17P_ADD_KICKS_TAG | ((queue_id as u64) << 43),
        // mac17g 0x8b91978-0x8b91984: base bit 43, qid at bit 44.
        SksmAddKicksTarget::G17G => G17G_ADD_KICKS_TAG | ((queue_id as u64) << 44),
        // mac17j 0x8b53144-0x8b53154: G17G form plus host-family bit 53.
        SksmAddKicksTarget::G17X => G17X_ADD_KICKS_TAG | ((queue_id as u64) << 44),
    }
}

const G17P_ADD_KICKS_TAG: u64 = 0x0000_0400_0002_1018;
const G17G_ADD_KICKS_TAG: u64 = 0x0000_0800_0002_1018;
const G17X_ADD_KICKS_TAG: u64 = 0x0020_0800_0002_1018;
/// Prepare the host's two ordered MMIO stores without touching the aperture.
pub(crate) const fn prepare_sksm_add_kicks_mmio(
    target: SksmAddKicksTarget,
    queue_id: u8,
    add_count: u32,
    geometry: G17SksmScratchGeometry,
) -> Result<PreparedSksmAddKicksMmio, SubmissionError> {
    if queue_id > KICK_QID_MASK as u8 {
        return Err(SubmissionError::QueueIdOutOfRange);
    }

    let offsets = match geometry.fifo_offsets() {
        Ok(offsets) => offsets,
        Err(_) => return Err(SubmissionError::KickMmioOffsetOverflow),
    };

    Ok(PreparedSksmAddKicksMmio {
        tag_offset: offsets.write0,
        tag: sksm_add_kicks_tag(target, queue_id),
        count_offset: offsets.write1,
        add_count,
    })
}

// ---------------------------------------------------------------------------
// 7. A18 Pro / G17P SKSM hardware bridge and completion descriptors.
//
//    Grounded from the pinned G17P host. `AGXKSMKickQueueG16::configureHardware`
//    at 0xfffffe0008a56580-0xfffffe0008a5682c reads the runtime MMIO geometry
//    from accelerator-object fields +0x10730/+0x10734/+0x10738, writes a
//    64-bit queue tag and 64-bit queue payload, then publishes the QID bit at
//    MMIO +0x21068. The object fields contain base0=0x4000, base1=0x4008 and
//    stride=0x10; they are not MMIO locations. The same geometry is used by
//    `addKicks` (§6). This closes the host-side programming equation, but not
//    a Linux live publication or firmware consumption, so this module still
//    performs no MMIO and the executable gate remains closed.
//
//    `AGXAcceleratorG16::populateKSMCompletionQueue` at
//    0xfffffe0008a53034-0xfffffe0008a532b8 emits four 0x20-byte descriptors.
//    Each present queue has 0x40-byte entries and a fixed ordinal 0..3. The
//    matched firmware consumes those queues. D93AP IRQ 1001 and RTBuddy AKF
//    message type 2 converge on source 4 and drain the event rings that carry
//    completion stamps. The completion gap remains live-gated until an actual
//    SKSM kick proves that complete round trip on D93AP hardware.
// ---------------------------------------------------------------------------

/// Accelerator-object fields holding the three runtime geometry values.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) struct G17PSksmRuntimeGeometryObjectLayout {
    pub(crate) write0_base_field: u32,
    pub(crate) write1_base_field: u32,
    pub(crate) stride_field: u32,
}

pub(crate) const G17P_SKSM_RUNTIME_GEOMETRY_OBJECT_LAYOUT: G17PSksmRuntimeGeometryObjectLayout =
    G17PSksmRuntimeGeometryObjectLayout {
        write0_base_field: 0x10730,
        write1_base_field: 0x10734,
        stride_field: 0x10738,
    };

/// QID-valid mask base written after the two queue-configuration stores.
pub(crate) const G17P_SKSM_QUEUE_ENABLE_MASK_BASE: u32 = 0x21068;
/// Queue-entry address bits preserved by the host payload encoder.
pub(crate) const G17P_SKSM_QUEUE_ENTRY_ADDRESS_MASK: u64 = 0x0000_07ff_ffff_ffe0;
/// Fixed portion of the first G17P queue-configuration store.
const G17P_SKSM_QUEUE_CONFIG_TAG: u64 = 0x0000_0400_0002_1008;
const G17P_SKSM_QUEUE_COMPLETION_SELECTOR_MAX: u32 = 4;

/// Pure description of the three ordered host-side publications that make one
/// A18 Pro SKSM queue visible to the hardware bridge.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) struct PreparedG17PSksmQueueConfig {
    /// First store: 64-bit QID tag.
    pub(crate) tag_offset: u32,
    pub(crate) tag: u64,
    /// Second store: 64-bit queue address/data-master/completion selector.
    pub(crate) payload_offset: u32,
    pub(crate) payload: u64,
    /// Final read/modify/write: enable this QID in one of two mask words.
    pub(crate) enable_mask_offset: u32,
    pub(crate) enable_mask: u64,
}

/// Prepare the exact G17P host queue-configuration stores without touching
/// MMIO. Only the paired A18 Pro identity is admitted.
pub(crate) const fn prepare_g17p_sksm_queue_config(
    target: G17ChannelAbiTarget,
    queue_id: u8,
    data_master: u32,
    completion_selector: u32,
    entry_gpu_va: u64,
    geometry: G17SksmScratchGeometry,
) -> Result<PreparedG17PSksmQueueConfig, SubmissionError> {
    match target {
        G17ChannelAbiTarget::A18ProG17P => {}
        G17ChannelAbiTarget::M5G17G | G17ChannelAbiTarget::M5ProMaxG17X => {
            return Err(SubmissionError::ChannelAbiUnconfirmed)
        }
    }
    if queue_id > KICK_QID_MASK {
        return Err(SubmissionError::QueueIdOutOfRange);
    }
    if data_master >= 3 {
        return Err(SubmissionError::KickQueueDataMasterOutOfRange);
    }
    if completion_selector > G17P_SKSM_QUEUE_COMPLETION_SELECTOR_MAX {
        return Err(SubmissionError::KickQueueCompletionSelectorOutOfRange);
    }
    if entry_gpu_va & !G17P_SKSM_QUEUE_ENTRY_ADDRESS_MASK != 0 {
        return Err(SubmissionError::KickQueueEntryAddressUnencodable);
    }

    let offsets = match geometry.fifo_offsets() {
        Ok(offsets) => offsets,
        Err(_) => return Err(SubmissionError::KickMmioOffsetOverflow),
    };

    // Host sequence: 3 * (4 - selector) + 0x3fe, shifted into bit 54.
    let completion_field = (3 * (4 - completion_selector) + 0x3fe) as u64;
    let payload =
        (((data_master as u64) + 1) << 60) | entry_gpu_va | completion_field.wrapping_shl(54);
    let mask_word = (queue_id as u32 >> 6) * 8;

    Ok(PreparedG17PSksmQueueConfig {
        tag_offset: offsets.write0,
        tag: G17P_SKSM_QUEUE_CONFIG_TAG | ((queue_id as u64) << 43),
        payload_offset: offsets.write1,
        payload,
        enable_mask_offset: G17P_SKSM_QUEUE_ENABLE_MASK_BASE + mask_word,
        enable_mask: 1u64 << (queue_id & 63),
    })
}

pub(crate) const G17P_KSM_COMPLETION_QUEUE_COUNT: u8 = 4;
pub(crate) const G17P_KSM_COMPLETION_DESCRIPTOR_SIZE: usize = 0x20;
pub(crate) const G17P_KSM_COMPLETION_ENTRY_SIZE: u32 = 0x40;

/// Exact field offsets in one host-produced G17P completion descriptor.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) struct G17PKsmCompletionDescriptorLayout {
    pub(crate) gpu_va: usize,
    pub(crate) cpu_va: usize,
    pub(crate) capacity: usize,
    pub(crate) entry_size: usize,
    pub(crate) ordinal: usize,
    pub(crate) processed: usize,
}

pub(crate) const G17P_KSM_COMPLETION_DESCRIPTOR_LAYOUT: G17PKsmCompletionDescriptorLayout =
    G17PKsmCompletionDescriptorLayout {
        gpu_va: 0x00,
        cpu_va: 0x08,
        capacity: 0x10,
        entry_size: 0x14,
        ordinal: 0x18,
        processed: 0x1c,
    };

/// Encode one present completion queue exactly as the G17P host populates it.
pub(crate) fn encode_g17p_ksm_completion_descriptor(
    target: G17ChannelAbiTarget,
    ordinal: u8,
    gpu_va: u64,
    cpu_va: u64,
    capacity: u32,
) -> Result<[u8; G17P_KSM_COMPLETION_DESCRIPTOR_SIZE], SubmissionError> {
    if target != G17ChannelAbiTarget::A18ProG17P {
        return Err(SubmissionError::ChannelAbiUnconfirmed);
    }
    if ordinal >= G17P_KSM_COMPLETION_QUEUE_COUNT {
        return Err(SubmissionError::CompletionQueueOrdinalOutOfRange);
    }
    if capacity == 0 {
        return Err(SubmissionError::CompletionQueueCapacityZero);
    }

    let mut raw = [0u8; G17P_KSM_COMPLETION_DESCRIPTOR_SIZE];
    let layout = G17P_KSM_COMPLETION_DESCRIPTOR_LAYOUT;
    put_u64_le(&mut raw, layout.gpu_va, gpu_va);
    put_u64_le(&mut raw, layout.cpu_va, cpu_va);
    put_u32_le(&mut raw, layout.capacity, capacity);
    put_u32_le(&mut raw, layout.entry_size, G17P_KSM_COMPLETION_ENTRY_SIZE);
    put_u32_le(&mut raw, layout.ordinal, ordinal as u32);
    put_u32_le(&mut raw, layout.processed, 0);
    Ok(raw)
}

/// The host-side completion descriptor and SKSM bridge equations are grounded;
/// this does not assert that Linux knows the physical IRQ source or has
/// observed an interrupt round trip.
pub(crate) const G17P_COMPLETION_HOST_ABI_GROUNDED: bool = true;

// ---------------------------------------------------------------------------
// 8. A18 Pro / G17P top-level channel-command dispatch.
//
//    This ABI is deliberately identity-scoped, just like §2c. The pinned G17P
//    host grounds the top-level names below through its command-request and
//    submit paths. The matching a000 firmware dispatcher is
//    `agfaProcessChannelCommand` at 0xfffffc0000010fa8 with the 19-entry table
//    at 0xfffffc000001233c. The b000/b100 dispatcher starts at
//    0xfffffc0000010f14 and uses the byte-identical table at
//    0xfffffc00000122a8.
//
//    All three firmware variants validate at least four bytes, load a u32 tag
//    from command offset zero, and reject tags above 18. Their jump tables
//    process 0/1/2/3/4/5/7/9/12/14/15/16/18. Tag 6 reaches the dedicated
//    `PMRebuild must not be received in SKSM` fatal, while 8/10/11/13/17
//    reach the invalid-command fatal. The host nevertheless names 17 `Nop`;
//    that producer identity is preserved, but the admission helper rejects it
//    because the matched consumer cannot process it.
//
//    Tags 6/8/10/11 remain semantically unresolved instead of inheriting an
//    AGX2 or nested SKU-stream meaning. SKU-stream opcodes are a distinct
//    nested domain and intentionally have no representation in the top-level
//    enum. M5-family identities remain fail-closed pending their own paired
//    producer/consumer pass.
//
//    The optional pre-dispatch filter uses literal mask 0x1d03f for tags <=
//    16. It is exposed separately so a future live producer can preserve that
//    gate instead of conflating it with complete jump-table dispatch.
// ---------------------------------------------------------------------------

/// Offset and width of the little-endian top-level command tag.
pub(crate) const G17P_TOP_LEVEL_COMMAND_TAG_OFFSET: u32 = 0;
pub(crate) const G17P_TOP_LEVEL_COMMAND_TAG_WIDTH: u32 = 4;

/// Host-grounded G17P top-level command types.
///
/// This type intentionally excludes unresolved tags and every nested
/// SKU-stream opcode. `Nop` records the host ABI but is rejected by
/// [`admit_g17p_top_level_command`] because all matched firmware variants send
/// tag 17 to the invalid-command fatal path.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
#[repr(u32)]
pub(crate) enum G17PTopLevelCommandType {
    Ta = 0,
    ThreeD = 1,
    FastBlit = 2,
    Cl = 3,
    Barrier = 4,
    PmGrow = 5,
    RemoteNode = 7,
    SharedEventSignal = 9,
    AddFinalFrgGtpcKick = 12,
    KsmKickQueueAddKicks = 14,
    ConfigUpdate = 15,
    EntrySignal = 16,
    Nop = 17,
    LateEvalEventSignal = 18,
}

/// Highest tag admitted by the G17P jump-table bounds check.
pub(crate) const G17P_CHANNEL_COMMAND_TYPE_MAX: u32 = 18;

/// Tags named by the pinned host's top-level ABI, including rejected `Nop`.
pub(crate) const G17P_TOP_LEVEL_COMMAND_KNOWN_MASK: u32 = 0x0007_d2bf;
/// In-range top-level tags whose host/firmware semantics remain unresolved.
pub(crate) const G17P_TOP_LEVEL_COMMAND_UNRESOLVED_MASK: u32 = 0x0000_0d40;
/// Tag 13 is in range but is neither a grounded host type nor unresolved.
pub(crate) const G17P_TOP_LEVEL_COMMAND_INVALID_MASK: u32 = 0x0000_2000;

/// Tags whose firmware jump-table entries reach a normal processing case.
pub(crate) const G17P_CHANNEL_COMMAND_PROCESS_MASK: u32 = 0x0005_d2bf;
/// Tag 6 has a dedicated PM-rebuild-in-SKSM fatal path.
pub(crate) const G17P_CHANNEL_COMMAND_PM_REBUILD_MASK: u32 = 0x0000_0040;
/// In-range tags whose jump-table entries reach the invalid-command fatal.
pub(crate) const G17P_CHANNEL_COMMAND_INVALID_MASK: u32 = 0x0002_2d00;
/// Literal mask used by the optional pre-dispatch filter for tags <= 16.
pub(crate) const G17P_CHANNEL_COMMAND_PREFILTER_MASK: u32 = 0x0001_d03f;

/// Firmware disposition for one raw command tag.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) enum G17PChannelCommandDisposition {
    Process,
    PmRebuildForbiddenFatal,
    InvalidCommandFatal,
}

/// Admit the command ABI only for the exact paired G17P identity.
const fn ensure_g17p_command_abi(target: G17ChannelAbiTarget) -> Result<(), SubmissionError> {
    match target {
        G17ChannelAbiTarget::A18ProG17P => Ok(()),
        G17ChannelAbiTarget::M5G17G | G17ChannelAbiTarget::M5ProMaxG17X => {
            Err(SubmissionError::CommandAbiUnconfirmed)
        }
    }
}

/// Read the complete little-endian u32 tag at top-level command offset zero.
const fn g17p_raw_top_level_command_tag(command: &[u8]) -> Result<u32, SubmissionError> {
    let offset = G17P_TOP_LEVEL_COMMAND_TAG_OFFSET as usize;
    let end = offset + G17P_TOP_LEVEL_COMMAND_TAG_WIDTH as usize;
    if command.len() < end {
        return Err(SubmissionError::CommandHeaderTooShort);
    }
    Ok(u32::from_le_bytes([
        command[offset],
        command[offset + 1],
        command[offset + 2],
        command[offset + 3],
    ]))
}

/// Decode a host-grounded top-level G17P command name.
///
/// This records producer identity only. Call
/// [`admit_g17p_top_level_command`] before treating a decoded tag as
/// consumable by the matched firmware.
pub(crate) const fn decode_g17p_top_level_command(
    target: G17ChannelAbiTarget,
    command: &[u8],
) -> Result<G17PTopLevelCommandType, SubmissionError> {
    match ensure_g17p_command_abi(target) {
        Ok(()) => {}
        Err(error) => return Err(error),
    }

    let raw = match g17p_raw_top_level_command_tag(command) {
        Ok(raw) => raw,
        Err(error) => return Err(error),
    };
    match raw {
        0 => Ok(G17PTopLevelCommandType::Ta),
        1 => Ok(G17PTopLevelCommandType::ThreeD),
        2 => Ok(G17PTopLevelCommandType::FastBlit),
        3 => Ok(G17PTopLevelCommandType::Cl),
        4 => Ok(G17PTopLevelCommandType::Barrier),
        5 => Ok(G17PTopLevelCommandType::PmGrow),
        7 => Ok(G17PTopLevelCommandType::RemoteNode),
        9 => Ok(G17PTopLevelCommandType::SharedEventSignal),
        12 => Ok(G17PTopLevelCommandType::AddFinalFrgGtpcKick),
        14 => Ok(G17PTopLevelCommandType::KsmKickQueueAddKicks),
        15 => Ok(G17PTopLevelCommandType::ConfigUpdate),
        16 => Ok(G17PTopLevelCommandType::EntrySignal),
        17 => Ok(G17PTopLevelCommandType::Nop),
        18 => Ok(G17PTopLevelCommandType::LateEvalEventSignal),
        6 | 8 | 10 | 11 => Err(SubmissionError::CommandTagUnresolved),
        _ => Err(SubmissionError::CommandTagInvalid),
    }
}

/// Classify a raw tag exactly like the matched G17P firmware jump table.
pub(crate) const fn classify_g17p_channel_command(
    target: G17ChannelAbiTarget,
    raw: u32,
) -> Result<G17PChannelCommandDisposition, SubmissionError> {
    match ensure_g17p_command_abi(target) {
        Ok(()) => {}
        Err(error) => return Err(error),
    }

    if raw > G17P_CHANNEL_COMMAND_TYPE_MAX {
        return Ok(G17PChannelCommandDisposition::InvalidCommandFatal);
    }

    let bit = 1u32 << raw;
    if bit & G17P_CHANNEL_COMMAND_PROCESS_MASK != 0 {
        Ok(G17PChannelCommandDisposition::Process)
    } else if bit & G17P_CHANNEL_COMMAND_PM_REBUILD_MASK != 0 {
        Ok(G17PChannelCommandDisposition::PmRebuildForbiddenFatal)
    } else {
        Ok(G17PChannelCommandDisposition::InvalidCommandFatal)
    }
}

/// Decode a top-level tag and require a normal matched-firmware dispatch case.
///
/// In particular, host-grounded `Nop = 17` is rejected here because the
/// firmware sends it to the invalid-command fatal path.
pub(crate) const fn admit_g17p_top_level_command(
    target: G17ChannelAbiTarget,
    command: &[u8],
) -> Result<G17PTopLevelCommandType, SubmissionError> {
    let command_type = match decode_g17p_top_level_command(target, command) {
        Ok(command_type) => command_type,
        Err(error) => return Err(error),
    };
    match classify_g17p_channel_command(target, command_type as u32) {
        Ok(G17PChannelCommandDisposition::Process) => Ok(command_type),
        Ok(
            G17PChannelCommandDisposition::PmRebuildForbiddenFatal
            | G17PChannelCommandDisposition::InvalidCommandFatal,
        ) => Err(SubmissionError::CommandTagRejectedByFirmware),
        Err(error) => Err(error),
    }
}

/// Apply the firmware's optional `tag <= 16 && bit(tag) & 0x1d03f` filter.
pub(crate) const fn g17p_channel_command_prefilter_allows(
    target: G17ChannelAbiTarget,
    raw: u32,
) -> Result<bool, SubmissionError> {
    match ensure_g17p_command_abi(target) {
        Ok(()) => {}
        Err(error) => return Err(error),
    }
    Ok(raw <= 16 && ((1u32 << raw) & G17P_CHANNEL_COMMAND_PREFILTER_MASK) != 0)
}

// ---------------------------------------------------------------------------
// 9. Provenance ledger + live gaps.
// ---------------------------------------------------------------------------

/// How each submission-ABI component was grounded, so reuse-vs-fork is explicit.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) enum AbiProvenance {
    /// Byte-identical to the shipping AGX2 producer; reused unchanged.
    IdenticalToAgx2,
    /// AGX3-specific, grounded first-hand from matched host/firmware artifacts.
    ForkedAgx3Grounded,
    /// AGX2 layout the driver produces; the G17 firmware's *own* consumer
    /// offsets were not read first-hand this pass. Kept fail-closed.
    Agx2InheritedUnconfirmed,
}

/// Per-component provenance for the submission transport.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) struct SubmissionAbiLedger {
    pub(crate) tx_doorbell: AbiProvenance,
    pub(crate) channel_state: AbiProvenance,
    pub(crate) g17p_channel_state: AbiProvenance,
    pub(crate) ring_state: AbiProvenance,
    pub(crate) channel_descriptor: AbiProvenance,
    pub(crate) host_channel_state_reset: AbiProvenance,
    pub(crate) host_uncached_channel_reset: AbiProvenance,
    pub(crate) g17p_command_types: AbiProvenance,
    pub(crate) kick_word: AbiProvenance,
    pub(crate) kick_queue_producer: AbiProvenance,
    pub(crate) host_add_kicks_mmio: AbiProvenance,
    pub(crate) g17p_queue_config: AbiProvenance,
    pub(crate) g17p_completion_descriptors: AbiProvenance,
    pub(crate) kick_record: AbiProvenance,
}

/// The doorbell is byte-grounded (§1); the SKSM kick word/record and host
/// queue-timestamp producer are AGX3 forks grounded first-hand (§2-§5). The
/// generic channel/ring producer records remain inherited and fail-closed.
/// A separate G17P-only channel-state entry records the matched A18 Pro host
/// producer and firmware consumer (§2c); it does not admit either M5 family.
/// The queue bridge/completion descriptors and command dispatch are grounded,
/// G17P-only AGX3 forks (§7-§8): the M5
/// identities remain fail-closed, unresolved values do not inherit AGX2 or
/// nested SKU meanings, and the host's `Nop = 17` is not firmware-admitted.
pub(crate) const SUBMISSION_ABI_LEDGER: SubmissionAbiLedger = SubmissionAbiLedger {
    tx_doorbell: AbiProvenance::IdenticalToAgx2,
    channel_state: AbiProvenance::Agx2InheritedUnconfirmed,
    g17p_channel_state: AbiProvenance::ForkedAgx3Grounded,
    ring_state: AbiProvenance::Agx2InheritedUnconfirmed,
    channel_descriptor: AbiProvenance::ForkedAgx3Grounded,
    host_channel_state_reset: AbiProvenance::ForkedAgx3Grounded,
    host_uncached_channel_reset: AbiProvenance::ForkedAgx3Grounded,
    g17p_command_types: AbiProvenance::ForkedAgx3Grounded,
    kick_word: AbiProvenance::ForkedAgx3Grounded,
    kick_queue_producer: AbiProvenance::ForkedAgx3Grounded,
    host_add_kicks_mmio: AbiProvenance::ForkedAgx3Grounded,
    g17p_queue_config: AbiProvenance::ForkedAgx3Grounded,
    g17p_completion_descriptors: AbiProvenance::ForkedAgx3Grounded,
    kick_record: AbiProvenance::ForkedAgx3Grounded,
};

/// The pinned G17P/G17G/G17X hosts all use the target-specific §6 MMIO
/// publication for `KSMKickQueue::addKicks`. EP 0x21 remains the separate
/// channel/work-ring doorbell; the two producers are complementary, not an
/// unresolved selector.
pub(crate) const HOST_SKSM_ADD_KICKS_PATH_SELECTED: bool = true;

/// Whether the AGX3 per-pipe channel *descriptor* stride is proven. The G17S
/// firmware directly walks twelve 0x20-byte entries at
/// `runtime_pointers+0x020..0x19f` (§2a).
pub(crate) const CHANNEL_DESCRIPTOR_STRIDE_CONFIRMED: bool = true;

/// The offline-unprovable pieces that keep the transport non-executable. Each
/// needs a live target device.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) struct SubmissionLiveGaps {
    /// The host equation and G17P geometry are grounded. Linux has not yet
    /// published through the mapped register window or witnessed firmware
    /// consumption of that publication.
    pub(crate) sksm_hardware_kick_fifo_mmio_scratch_proven: bool,
    /// The kick -> execute -> completion IRQ handshake is a live event.
    pub(crate) doorbell_round_trip_proven: bool,
    /// Apple's host builds platform interrupt event sources from runtime
    /// `interrupts`/`interrupts-valid` properties. D93AP physical IRQ 1001 and
    /// RTBuddy AKF type 2 both converge exactly on source 4 and drain the main
    /// firmware event rings, but an on-device SKSM completion round trip has
    /// not proven that this ingress is sufficient for Linux completion.
    pub(crate) completion_irq_route_proven: bool,
}

impl SubmissionLiveGaps {
    /// True only if every live gap is closed.
    pub(crate) const fn all_proven(&self) -> bool {
        self.sksm_hardware_kick_fifo_mmio_scratch_proven
            && self.doorbell_round_trip_proven
            && self.completion_irq_route_proven
    }
}

/// All live gaps are open (fail-closed).
pub(crate) const SUBMISSION_LIVE_GAPS: SubmissionLiveGaps = SubmissionLiveGaps {
    sksm_hardware_kick_fifo_mmio_scratch_proven: false,
    doorbell_round_trip_proven: false,
    completion_irq_route_proven: false,
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tx_doorbell_matches_the_shipping_agx2_wire_form() {
        // gpu.rs: MSG_TX_DOORBELL = 0x83 << 48; run_job kicks
        // MSG_TX_DOORBELL | pipe_type | (index << 2).
        const MSG_TX_DOORBELL: u64 = 0x83 << 48;
        assert_eq!(EP_DOORBELL, 0x21);
        for (pipe, raw) in [
            (PipeType::Vertex, 0u64),
            (PipeType::Fragment, 1),
            (PipeType::Compute, 2),
        ] {
            for index in 0u8..=7 {
                let msg = encode_tx_doorbell(pipe, index).unwrap();
                assert_eq!(msg, MSG_TX_DOORBELL | raw | ((index as u64) << 2));
                // Firmware type extraction: ubfx x8,x1,#48,#6 == 3.
                assert_eq!(doorbell_type(msg), DOORBELL_TYPE_TX);
                assert_eq!(decode_tx_doorbell(msg), Ok((pipe, index)));
            }
        }
        assert_eq!(
            encode_tx_doorbell(PipeType::Vertex, 8),
            Err(SubmissionError::DoorbellIndexOutOfRange)
        );
    }

    #[test]
    fn doorbell_types_come_from_the_firmware_dispatch() {
        // 0x83/0x84/0x85 & 0x3f -> 3/4/5, the firmware's cmp targets.
        assert_eq!(DOORBELL_TYPE_TX, (0x83u8 & 0x3f));
        assert_eq!(DOORBELL_TYPE_FWCTL, (0x84u8 & 0x3f));
        assert_eq!(DOORBELL_TYPE_HALT, (0x85u8 & 0x3f));
        // A FwCtl (0x84<<48) doorbell is not a TX doorbell.
        assert_eq!(
            decode_tx_doorbell(0x84u64 << 48),
            Err(SubmissionError::NotATxDoorbell)
        );
    }

    #[test]
    fn firmware_rejects_pipe_three() {
        // The firmware `cmp w8,#0x3 ; b.eq done` path: pipe 3 is not a pipe.
        let msg = (0x83u64 << 48) | 0x3;
        assert_eq!(doorbell_type(msg), DOORBELL_TYPE_TX);
        assert_eq!(decode_tx_doorbell(msg), Err(SubmissionError::ReservedPipe));
    }

    #[test]
    fn wakeup_doorbell_payloads_decode_like_the_firmware() {
        // DOORBELL_KICKFW=0x10 -> pipe 0, index 4; DOORBELL_DEVCTRL=0x11 ->
        // pipe 1, index 4 (msg & 3, (msg>>2)&7).
        let kickfw = (0x83u64 << 48) | DOORBELL_KICKFW as u64;
        assert_eq!(decode_tx_doorbell(kickfw), Ok((PipeType::Vertex, 4)));
        let devctrl = (0x83u64 << 48) | DOORBELL_DEVCTRL as u64;
        assert_eq!(decode_tx_doorbell(devctrl), Ok((PipeType::Fragment, 4)));
    }

    #[test]
    fn pending_pipe_bit_matches_1_shl_pipe() {
        assert_eq!(pending_pipe_bit(PipeType::Vertex), 1);
        assert_eq!(pending_pipe_bit(PipeType::Fragment), 2);
        assert_eq!(pending_pipe_bit(PipeType::Compute), 4);
    }

    #[test]
    fn inherited_channel_and_ring_state_offsets_are_labeled_agx2() {
        // channel.rs raw::ChannelState: read_ptr@0x00, write_ptr@0x20.
        assert_eq!(AGX2_CHANNEL_STATE_READ_PTR_OFFSET, 0x00);
        assert_eq!(AGX2_CHANNEL_STATE_WRITE_PTR_OFFSET, 0x20);
        assert_eq!(AGX2_CHANNEL_STATE_SIZE, 0x40);
        // fw/workqueue.rs raw::RingState: doneptr@0x00, rptr@0x30, wptr@0x40,
        // size@0x50 (0x10-strided).
        assert_eq!(AGX2_RING_STATE_GPU_DONEPTR_OFFSET, 0x00);
        assert_eq!(AGX2_RING_STATE_GPU_RPTR_OFFSET, 0x30);
        assert_eq!(AGX2_RING_STATE_CPU_WPTR_OFFSET, 0x40);
        assert_eq!(AGX2_RING_STATE_RB_SIZE_OFFSET, 0x50);
        assert_eq!(AGX2_RING_ENTRY_BYTES, 8);
        assert_eq!(HOST_MAX_OUTSTANDING_JOB_SLOTS, 127);
    }

    #[test]
    fn g17p_native_channel_layout_is_identity_scoped() {
        assert_eq!(
            g17_channel_abi(G17ChannelAbiTarget::A18ProG17P),
            Ok(G17P_CHANNEL_ABI)
        );
        assert_eq!(
            g17_channel_abi(G17ChannelAbiTarget::M5G17G),
            Err(SubmissionError::ChannelAbiUnconfirmed)
        );
        assert_eq!(
            g17_channel_abi(G17ChannelAbiTarget::M5ProMaxG17X),
            Err(SubmissionError::ChannelAbiUnconfirmed)
        );

        assert_eq!(
            G17P_CHANNEL_ABI.cached,
            G17PCachedChannelStateLayout {
                uncached_state_gpu_va: 0x00,
                command_pointer_ring_gpu_va: 0x08,
                coherency_cursor: 0x1c,
                record_size: 0x80,
            }
        );
        assert_eq!(
            G17P_CHANNEL_ABI.uncached,
            G17PUncachedChannelStateLayout {
                consumer: 0x00,
                coherency_shadow: 0x30,
                producer: 0x40,
                recovery_target: 0x50,
                ring_entry_count: 0x60,
                cursor_width: 4,
                write_extent: 0x64,
            }
        );
        assert_eq!(G17P_CHANNEL_ABI.command_pointer_entry_bytes, 8);
        assert_eq!(G17P_CHANNEL_ABI.accelerator_target_width, 2);
        assert!(G17P_CHANNEL_PRODUCER_REQUIRES_DMB_ISH);
    }

    #[test]
    fn g17p_channel_write_plan_matches_host_ordering_and_wrap() {
        assert_eq!(
            prepare_g17p_channel_write(G17ChannelAbiTarget::A18ProG17P, 2, 0, 4),
            Ok(G17PChannelWritePlan {
                entry_byte_offset: 0x10,
                next_producer: 3,
            })
        );
        assert_eq!(
            prepare_g17p_channel_write(G17ChannelAbiTarget::A18ProG17P, 3, 2, 4),
            Ok(G17PChannelWritePlan {
                entry_byte_offset: 0x18,
                next_producer: 0,
            })
        );
        assert_eq!(
            prepare_g17p_channel_write(G17ChannelAbiTarget::A18ProG17P, 1, 2, 4),
            Err(SubmissionError::RingFull)
        );
        assert_eq!(
            prepare_g17p_channel_write(G17ChannelAbiTarget::A18ProG17P, 0, 0, 0),
            Err(SubmissionError::RingSizeZero)
        );
        assert_eq!(
            prepare_g17p_channel_write(G17ChannelAbiTarget::M5ProMaxG17X, 0, 2, 4),
            Err(SubmissionError::ChannelAbiUnconfirmed)
        );
    }

    #[test]
    fn g17_channel_descriptor_geometry_matches_the_firmware_walk() {
        assert_eq!(G17_CHANNEL_DESCRIPTOR_TABLE_OFFSET, 0x20);
        assert_eq!(G17_CHANNEL_DESCRIPTORS_PER_PRIORITY, 3);
        assert_eq!(G17_CHANNEL_PRIORITY_COUNT, 4);
        assert_eq!(G17_CHANNEL_DESCRIPTOR_COUNT, 12);
        assert_eq!(G17_CHANNEL_DESCRIPTOR_STRIDE, 0x20);
        assert_eq!(G17_CHANNEL_ENTRY_STRIDE, 0x18);
        assert_eq!(
            G17_CHANNEL_DESCRIPTOR_LAYOUT,
            G17ChannelDescriptorLayout {
                dispatch_cursor: 0x00,
                coherency_cursor: 0x08,
                producer_cursor: 0x10,
                entries: 0x18,
            }
        );

        for priority in 0..G17_CHANNEL_PRIORITY_COUNT {
            for (pipe, pipe_index) in [
                (PipeType::Vertex, 0u32),
                (PipeType::Fragment, 1),
                (PipeType::Compute, 2),
            ] {
                let linear = priority as u32 * 3 + pipe_index;
                assert_eq!(
                    g17_channel_descriptor_offset(priority, pipe),
                    Ok(0x20 + linear * 0x20)
                );
            }
        }

        // Descriptor eleven occupies 0x180..0x19f. Priority four must not
        // alias the independent cursor pair beginning at +0x1a0.
        assert_eq!(
            g17_channel_descriptor_offset(3, PipeType::Compute),
            Ok(0x180)
        );
        assert_eq!(
            g17_channel_descriptor_offset(4, PipeType::Vertex),
            Err(SubmissionError::ChannelPriorityOutOfRange)
        );
    }

    #[test]
    fn g17_host_channel_state_reset_matches_all_three_pinned_hosts() {
        let values = G17HostChannelStateResetValues {
            gpu_va_00: 0x0011_2233_4455_6677,
            gpu_va_08: 0x8899_aabb_ccdd_eeff,
            gpu_va_10: 0x1020_3040_5060_7080,
            word_44: 0x7654_3210,
            gpu_va_70: 0xfedc_ba98_7654_3210,
        };
        let raw = encode_g17_host_channel_state_reset(values);
        let layout = G17_HOST_CHANNEL_STATE_RESET_LAYOUT;

        assert_eq!(raw.len(), 0x80);
        assert_eq!(
            &raw[layout.gpu_va_00..layout.gpu_va_00 + 8],
            &values.gpu_va_00.to_le_bytes()
        );
        assert_eq!(
            &raw[layout.gpu_va_08..layout.gpu_va_08 + 8],
            &values.gpu_va_08.to_le_bytes()
        );
        assert_eq!(
            &raw[layout.gpu_va_10..layout.gpu_va_10 + 8],
            &values.gpu_va_10.to_le_bytes()
        );
        assert_eq!(&raw[layout.zero_18..layout.zero_18 + 4], &[0; 4]);
        assert_eq!(&raw[layout.zero_1c..layout.zero_1c + 4], &[0; 4]);
        assert_eq!(&raw[layout.zero_20..layout.zero_20 + 4], &[0; 4]);
        assert_eq!(
            &raw[layout.sentinel_24..layout.sentinel_24 + 4],
            &u32::MAX.to_le_bytes()
        );
        assert_eq!(
            &raw[layout.fixed_4_28..layout.fixed_4_28 + 4],
            &4u32.to_le_bytes()
        );
        assert_eq!(
            &raw[layout.sentinel_40..layout.sentinel_40 + 4],
            &u32::MAX.to_le_bytes()
        );
        assert_eq!(
            &raw[layout.word_44..layout.word_44 + 4],
            &values.word_44.to_le_bytes()
        );
        assert_eq!(&raw[layout.zero_58..layout.zero_58 + 4], &[0; 4]);
        assert_eq!(
            &raw[layout.gpu_va_70..layout.gpu_va_70 + 8],
            &values.gpu_va_70.to_le_bytes()
        );

        // resetChannelState clears the entire record before its scalar stores.
        // Every byte outside the nonzero runtime/fixed ranges stays zero.
        for (index, byte) in raw.iter().enumerate() {
            let nonzero_field = (0x00..0x18).contains(&index)
                || (0x24..0x2c).contains(&index)
                || (0x40..0x48).contains(&index)
                || (0x70..0x78).contains(&index);
            if !nonzero_field {
                assert_eq!(*byte, 0, "unexpected reset byte at {index:#x}");
            }
        }
        assert!(G17_HOST_CHANNEL_RESET_TARGETS_CONFIRMED);
    }

    #[test]
    fn g17_uncached_channel_reset_preserves_unwritten_bytes() {
        let mut raw = [0xa5; G17_UNCACHED_CHANNEL_RESET_WRITE_EXTENT];
        let ring_entry_count = 0x800;
        apply_g17_uncached_channel_reset(&mut raw, ring_entry_count);
        let layout = G17_UNCACHED_CHANNEL_RESET_LAYOUT;

        for offset in [
            layout.zero_00,
            layout.zero_10,
            layout.zero_20,
            layout.zero_30,
            layout.zero_40,
        ] {
            assert_eq!(&raw[offset..offset + 4], &[0; 4]);
        }
        assert_eq!(
            &raw[layout.sentinel_50..layout.sentinel_50 + 4],
            &u32::MAX.to_le_bytes()
        );
        assert_eq!(
            &raw[layout.ring_entry_count_60..layout.ring_entry_count_60 + 4],
            &ring_entry_count.to_le_bytes()
        );

        for (index, byte) in raw.iter().enumerate() {
            let written = [0x00usize, 0x10, 0x20, 0x30, 0x40, 0x50, 0x60]
                .iter()
                .any(|offset| (*offset..*offset + 4).contains(&index));
            if !written {
                assert_eq!(*byte, 0xa5, "unproven byte changed at {index:#x}");
            }
        }
    }

    #[test]
    fn ring_index_wraps_at_size() {
        assert_eq!(ring_next(0, 4), Ok(1));
        assert_eq!(ring_next(3, 4), Ok(0));
        assert_eq!(ring_next(0x4ff, 0x500), Ok(0));
        assert_eq!(ring_next(0, 0), Err(SubmissionError::RingSizeZero));
        assert_eq!(ring_next(4, 4), Err(SubmissionError::RingIndexOutOfRange));
    }

    #[test]
    fn kick_word_fields_match_the_firmware_extraction() {
        // and x0,x5,#0xffffffffff ; ubfx x1,x5,#40,#7 ; ubfx x6,x5,#46,#1.
        assert_eq!(KICK_TS_BITS, 40);
        assert_eq!(KICK_TS_MASK, 0xff_ffff_ffff);
        assert_eq!(KICK_QID_SHIFT, 40);
        assert_eq!(KICK_QID_MASK, 0x7f);
        assert_eq!(KICK_SELECTOR_BIT, 46);

        // Compose the exact word consumed from the firmware scratch FIFO;
        // QID bit 6 is the mask selector.
        let ts = 0x89_abcd_ef42u64;
        let word = encode_kick_word(ts, 0x7f).unwrap();
        assert_eq!(
            decode_kick_word(word),
            Ok(SksmKickWord {
                timestamp: ts,
                queue_id: 0x7f,
                mask_selector: 1,
            })
        );
        // A word with bit 47 set is outside the modeled form.
        assert_eq!(
            decode_kick_word(1 << 47),
            Err(SubmissionError::KickWordUnmodeledBits)
        );
        assert_eq!(
            encode_kick_word(KICK_TS_MASK + 1, 0),
            Err(SubmissionError::KickTimestampOutOfRange)
        );
        assert_eq!(
            encode_kick_word(0, 0x80),
            Err(SubmissionError::QueueIdOutOfRange)
        );
    }

    #[test]
    fn kick_queue_preparation_couples_slot_queue_and_next_timestamp() {
        let timestamp = encode_kick_timestamp(KickTimestamp {
            stamp: 0x1234_5678,
            stamp_index: 7,
        });
        let prepared = prepare_kick_submission(timestamp, 0x2a, 16, 0x180).unwrap();

        assert_eq!(prepared.entry_index, 7);
        assert_eq!(prepared.entry_offset, 7 * 0x180);
        assert_eq!(prepared.timestamp, 0x12_3456_7807);
        assert_eq!(prepared.queue_id, 0x2a);
        assert_eq!(
            decode_kick_timestamp(prepared.next_timestamp),
            Ok(KickTimestamp {
                stamp: 0x1234_5678,
                stamp_index: 8,
            })
        );
    }

    #[test]
    fn kick_queue_timestamp_wrap_matches_the_host_divide_remainder_sequence() {
        // Slot 15 wraps modulo 16, so the host adds 0x100 to the timestamp:
        // the low byte becomes zero and the 32-bit stamp increments.
        let timestamp = encode_kick_timestamp(KickTimestamp {
            stamp: 0x1234_5678,
            stamp_index: 15,
        });
        let prepared = prepare_kick_submission(timestamp, 0x7f, 16, 0x180).unwrap();
        assert_eq!(prepared.entry_index, 15);
        assert_eq!(prepared.entry_offset, 15 * 0x180);
        assert_eq!(
            decode_kick_timestamp(prepared.next_timestamp),
            Ok(KickTimestamp {
                stamp: 0x1234_5679,
                stamp_index: 0,
            })
        );

        // A 256-slot queue is representable despite num_stamps being larger
        // than u8; slot 255 wraps and the 32-bit stamp wraps exactly like the
        // host's masked add.
        let timestamp = encode_kick_timestamp(KickTimestamp {
            stamp: u32::MAX,
            stamp_index: 0xff,
        });
        let prepared = prepare_kick_submission(timestamp, 0x40, 256, 0x180).unwrap();
        assert_eq!(prepared.entry_index, 0xff);
        assert_eq!(
            decode_kick_timestamp(prepared.next_timestamp),
            Ok(KickTimestamp {
                stamp: 0,
                stamp_index: 0,
            })
        );
    }

    #[test]
    fn kick_queue_preparation_matches_all_runtime_stamp_geometries() {
        for num_stamps in 1u16..=KICK_NUM_STAMPS_MAX {
            for index in 0u16..num_stamps {
                let current = KickTimestamp {
                    stamp: 0x89ab_cdef,
                    stamp_index: index as u8,
                };
                let timestamp = encode_kick_timestamp(current);
                let prepared = prepare_kick_submission(timestamp, 3, num_stamps, 0x180).unwrap();
                let expected_index = (index + 1) % num_stamps;
                let expected_stamp = if expected_index == 0 {
                    current.stamp.wrapping_add(1)
                } else {
                    current.stamp
                };

                assert_eq!(prepared.entry_index, index as u8);
                assert_eq!(prepared.entry_offset, index as u32 * 0x180);
                assert_eq!(prepared.timestamp, timestamp);
                assert_eq!(
                    decode_kick_timestamp(prepared.next_timestamp),
                    Ok(KickTimestamp {
                        stamp: expected_stamp,
                        stamp_index: expected_index as u8,
                    })
                );
            }
        }
    }

    #[test]
    fn kick_queue_preparation_rejects_unrepresentable_state_before_publication() {
        assert_eq!(
            prepare_kick_submission(1 << 40, 0, 1, 0x180),
            Err(SubmissionError::KickTimestampOutOfRange)
        );
        assert_eq!(
            prepare_kick_submission(0, 0, 0, 0x180),
            Err(SubmissionError::KickStampCountOutOfRange)
        );
        assert_eq!(
            prepare_kick_submission(0, 0, 257, 0x180),
            Err(SubmissionError::KickStampCountOutOfRange)
        );
        assert_eq!(
            prepare_kick_submission(4, 0, 4, 0x180),
            Err(SubmissionError::KickStampIndexOutOfRange)
        );
        assert_eq!(
            prepare_kick_submission(0, 0, 1, 0),
            Err(SubmissionError::KickEntryStrideZero)
        );
        assert_eq!(
            prepare_kick_submission(2, 0, 3, u32::MAX),
            Err(SubmissionError::KickEntryOffsetOverflow)
        );
        assert_eq!(
            prepare_kick_submission(0, 0x80, 1, 0x180),
            Err(SubmissionError::QueueIdOutOfRange)
        );
    }

    #[test]
    fn host_add_kicks_tags_match_all_three_pinned_producers() {
        let geometry = G17SksmScratchGeometry {
            write0_base: 0x100,
            write1_base: 0x104,
            stride: 3,
        };

        for queue_id in 0u8..=0x7f {
            let p = prepare_sksm_add_kicks_mmio(SksmAddKicksTarget::G17P, queue_id, 5, geometry)
                .unwrap();
            assert_eq!(p.tag, 0x0000_0400_0002_1018 | ((queue_id as u64) << 43));
            assert_eq!(p.tag_offset, 0x11b);
            assert_eq!(p.count_offset, 0x11f);
            assert_eq!(p.add_count, 5);

            let g = prepare_sksm_add_kicks_mmio(
                SksmAddKicksTarget::G17G,
                queue_id,
                0x89ab_cdef,
                geometry,
            )
            .unwrap();
            assert_eq!(g.tag, 0x0000_0800_0002_1018 | ((queue_id as u64) << 44));

            let x =
                prepare_sksm_add_kicks_mmio(SksmAddKicksTarget::G17X, queue_id, u32::MAX, geometry)
                    .unwrap();
            assert_eq!(x.tag, 0x0020_0800_0002_1018 | ((queue_id as u64) << 44));
            assert_eq!(x.add_count, u32::MAX);
        }
    }

    #[test]
    fn host_add_kicks_mmio_offsets_report_overflow() {
        let valid = G17SksmScratchGeometry {
            write0_base: 0,
            write1_base: 0,
            stride: u32::MAX / 9,
        };
        let publication =
            prepare_sksm_add_kicks_mmio(SksmAddKicksTarget::G17G, 0, 1, valid).unwrap();
        assert_eq!(publication.tag_offset, (u32::MAX / 9) * 9);

        assert_eq!(
            prepare_sksm_add_kicks_mmio(
                SksmAddKicksTarget::G17P,
                0x80,
                1,
                G17SksmScratchGeometry {
                    write0_base: 0,
                    write1_base: 4,
                    stride: 0,
                },
            ),
            Err(SubmissionError::QueueIdOutOfRange)
        );
        for geometry in [
            G17SksmScratchGeometry {
                write0_base: 0,
                write1_base: 0,
                stride: u32::MAX / 9 + 1,
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
                prepare_sksm_add_kicks_mmio(SksmAddKicksTarget::G17X, 0, 1, geometry),
                Err(SubmissionError::KickMmioOffsetOverflow)
            );
        }
    }

    #[test]
    fn g17p_sksm_queue_configuration_matches_the_host_store_sequence() {
        assert_eq!(
            G17P_SKSM_RUNTIME_GEOMETRY_OBJECT_LAYOUT,
            G17PSksmRuntimeGeometryObjectLayout {
                write0_base_field: 0x10730,
                write1_base_field: 0x10734,
                stride_field: 0x10738,
            }
        );
        assert_eq!(G17P_SKSM_QUEUE_ENABLE_MASK_BASE, 0x21068);
        assert_eq!(G17P_SKSM_QUEUE_ENTRY_ADDRESS_MASK, 0x0000_07ff_ffff_ffe0);

        let config = prepare_g17p_sksm_queue_config(
            G17ChannelAbiTarget::A18ProG17P,
            0x40,
            1,
            0,
            0x12340,
            g17_manager::T8140_G17P_PLATFORM_CONFIG.sksm_scratch,
        )
        .unwrap();

        // configureHardware: base + 9*stride, tag, payload, then QID mask.
        assert_eq!(config.tag_offset, 0x4090);
        assert_eq!(config.tag, 0x0002_0400_0002_1008);
        assert_eq!(config.payload_offset, 0x4098);
        // This hard-coded value catches both the 32-bit selector arithmetic
        // and the host's ADD of `(data_master + 1) << 60` before the OR.
        assert_eq!(config.payload, 0x2280_0000_0001_2340);
        assert_eq!(config.enable_mask_offset, 0x21070);
        assert_eq!(config.enable_mask, 1);
    }

    #[test]
    fn g17p_sksm_queue_configuration_is_identity_and_range_scoped() {
        let geometry = g17_manager::T8140_G17P_PLATFORM_CONFIG.sksm_scratch;

        for target in [
            G17ChannelAbiTarget::M5G17G,
            G17ChannelAbiTarget::M5ProMaxG17X,
        ] {
            assert_eq!(
                prepare_g17p_sksm_queue_config(target, 0, 0, 0, 0x20, geometry),
                Err(SubmissionError::ChannelAbiUnconfirmed)
            );
        }
        assert_eq!(
            prepare_g17p_sksm_queue_config(
                G17ChannelAbiTarget::A18ProG17P,
                0x80,
                0,
                0,
                0x20,
                geometry,
            ),
            Err(SubmissionError::QueueIdOutOfRange)
        );
        assert_eq!(
            prepare_g17p_sksm_queue_config(
                G17ChannelAbiTarget::A18ProG17P,
                0,
                3,
                0,
                0x20,
                geometry,
            ),
            Err(SubmissionError::KickQueueDataMasterOutOfRange)
        );
        assert_eq!(
            prepare_g17p_sksm_queue_config(
                G17ChannelAbiTarget::A18ProG17P,
                0,
                0,
                5,
                0x20,
                geometry,
            ),
            Err(SubmissionError::KickQueueCompletionSelectorOutOfRange)
        );
        for address in [0x21, 0x0000_0800_0000_0000] {
            assert_eq!(
                prepare_g17p_sksm_queue_config(
                    G17ChannelAbiTarget::A18ProG17P,
                    0,
                    0,
                    0,
                    address,
                    geometry,
                ),
                Err(SubmissionError::KickQueueEntryAddressUnencodable)
            );
        }
        assert_eq!(
            prepare_g17p_sksm_queue_config(
                G17ChannelAbiTarget::A18ProG17P,
                0,
                0,
                0,
                0x20,
                G17SksmScratchGeometry {
                    write0_base: u32::MAX,
                    write1_base: 0,
                    stride: 1,
                },
            ),
            Err(SubmissionError::KickMmioOffsetOverflow)
        );
    }

    #[test]
    fn g17p_completion_descriptors_match_all_four_host_slots() {
        assert_eq!(G17P_KSM_COMPLETION_QUEUE_COUNT, 4);
        assert_eq!(G17P_KSM_COMPLETION_DESCRIPTOR_SIZE, 0x20);
        assert_eq!(G17P_KSM_COMPLETION_ENTRY_SIZE, 0x40);
        assert_eq!(
            G17P_KSM_COMPLETION_DESCRIPTOR_LAYOUT,
            G17PKsmCompletionDescriptorLayout {
                gpu_va: 0x00,
                cpu_va: 0x08,
                capacity: 0x10,
                entry_size: 0x14,
                ordinal: 0x18,
                processed: 0x1c,
            }
        );

        for ordinal in 0..G17P_KSM_COMPLETION_QUEUE_COUNT {
            let gpu_va = 0x1234_0000 + ordinal as u64 * 0x1000;
            let cpu_va = 0xabcd_0000 + ordinal as u64 * 0x2000;
            let capacity = 0x80 + ordinal as u32;
            let raw = encode_g17p_ksm_completion_descriptor(
                G17ChannelAbiTarget::A18ProG17P,
                ordinal,
                gpu_va,
                cpu_va,
                capacity,
            )
            .unwrap();

            assert_eq!(&raw[0x00..0x08], &gpu_va.to_le_bytes());
            assert_eq!(&raw[0x08..0x10], &cpu_va.to_le_bytes());
            assert_eq!(&raw[0x10..0x14], &capacity.to_le_bytes());
            assert_eq!(&raw[0x14..0x18], &0x40u32.to_le_bytes());
            assert_eq!(&raw[0x18..0x1c], &(ordinal as u32).to_le_bytes());
            assert_eq!(&raw[0x1c..0x20], &[0; 4]);
        }
        assert!(G17P_COMPLETION_HOST_ABI_GROUNDED);
    }

    #[test]
    fn g17p_completion_descriptors_fail_closed() {
        assert_eq!(
            encode_g17p_ksm_completion_descriptor(
                G17ChannelAbiTarget::M5G17G,
                0,
                0x1000,
                0x2000,
                1,
            ),
            Err(SubmissionError::ChannelAbiUnconfirmed)
        );
        assert_eq!(
            encode_g17p_ksm_completion_descriptor(
                G17ChannelAbiTarget::A18ProG17P,
                4,
                0x1000,
                0x2000,
                1,
            ),
            Err(SubmissionError::CompletionQueueOrdinalOutOfRange)
        );
        assert_eq!(
            encode_g17p_ksm_completion_descriptor(
                G17ChannelAbiTarget::A18ProG17P,
                0,
                0x1000,
                0x2000,
                0,
            ),
            Err(SubmissionError::CompletionQueueCapacityZero)
        );
    }

    #[test]
    fn kick_record_table_uses_the_firmware_geometry() {
        // add x2,x22,#0x10 ; add x2,x2,QID,lsl#5 -> base 0x10, stride 0x20.
        assert_eq!(KICK_RECORD_TABLE_BASE, 0x10);
        assert_eq!(KICK_RECORD_STRIDE, 0x20);
        assert_eq!(KICK_RECORD_COUNT, 128);
        assert_eq!(kick_record_offset(0), Ok(0x10));
        assert_eq!(kick_record_offset(1), Ok(0x30));
        assert_eq!(kick_record_offset(0x7f), Ok(0x0ff0));
        assert_eq!(
            kick_record_offset(0x80),
            Err(SubmissionError::QueueIdOutOfRange)
        );
        // The whole 128-entry table fits in 0x10 + 128*0x20 = 0x1010 bytes.
        assert_eq!(
            KICK_RECORD_TABLE_BASE + KICK_RECORD_COUNT * KICK_RECORD_STRIDE,
            0x1010
        );
    }

    #[test]
    fn kick_record_internal_layout_is_exact() {
        // Written across 0x10cb0-0x10d2c.
        assert_eq!(
            KICK_RECORD_LAYOUT,
            KickRecordLayout {
                ts_first: 0x00,
                ts_last: 0x08,
                slot_mask: 0x10,
                valid_flag: 0x18,
                slot_index: 0x1c,
            }
        );
        // Every field lies inside one 0x20-byte record.
        for off in [
            KICK_RECORD_LAYOUT.ts_first,
            KICK_RECORD_LAYOUT.ts_last,
            KICK_RECORD_LAYOUT.slot_mask,
            KICK_RECORD_LAYOUT.valid_flag,
            KICK_RECORD_LAYOUT.slot_index,
        ] {
            assert!(off < KICK_RECORD_STRIDE);
        }
    }

    #[test]
    fn g17p_top_level_command_names_match_the_host_abi() {
        let known = [
            (G17PTopLevelCommandType::Ta, 0),
            (G17PTopLevelCommandType::ThreeD, 1),
            (G17PTopLevelCommandType::FastBlit, 2),
            (G17PTopLevelCommandType::Cl, 3),
            (G17PTopLevelCommandType::Barrier, 4),
            (G17PTopLevelCommandType::PmGrow, 5),
            (G17PTopLevelCommandType::RemoteNode, 7),
            (G17PTopLevelCommandType::SharedEventSignal, 9),
            (G17PTopLevelCommandType::AddFinalFrgGtpcKick, 12),
            (G17PTopLevelCommandType::KsmKickQueueAddKicks, 14),
            (G17PTopLevelCommandType::ConfigUpdate, 15),
            (G17PTopLevelCommandType::EntrySignal, 16),
            (G17PTopLevelCommandType::Nop, 17),
            (G17PTopLevelCommandType::LateEvalEventSignal, 18),
        ];
        for (command_type, raw) in known {
            assert_eq!(command_type as u32, raw);
        }

        assert_eq!(G17P_TOP_LEVEL_COMMAND_TAG_OFFSET, 0);
        assert_eq!(G17P_TOP_LEVEL_COMMAND_TAG_WIDTH, 4);
        assert_eq!(G17P_CHANNEL_COMMAND_TYPE_MAX, 18);
        assert_eq!(G17P_TOP_LEVEL_COMMAND_KNOWN_MASK, 0x0007_d2bf);
        assert_eq!(G17P_TOP_LEVEL_COMMAND_UNRESOLVED_MASK, 0x0000_0d40);
        assert_eq!(G17P_TOP_LEVEL_COMMAND_INVALID_MASK, 0x0000_2000);
        assert_eq!(
            G17P_TOP_LEVEL_COMMAND_KNOWN_MASK
                | G17P_TOP_LEVEL_COMMAND_UNRESOLVED_MASK
                | G17P_TOP_LEVEL_COMMAND_INVALID_MASK,
            (1 << (G17P_CHANNEL_COMMAND_TYPE_MAX + 1)) - 1
        );
    }

    #[test]
    fn g17p_top_level_command_decoder_reads_exact_little_endian_bytes() {
        let known = [
            G17PTopLevelCommandType::Ta,
            G17PTopLevelCommandType::ThreeD,
            G17PTopLevelCommandType::FastBlit,
            G17PTopLevelCommandType::Cl,
            G17PTopLevelCommandType::Barrier,
            G17PTopLevelCommandType::PmGrow,
            G17PTopLevelCommandType::RemoteNode,
            G17PTopLevelCommandType::SharedEventSignal,
            G17PTopLevelCommandType::AddFinalFrgGtpcKick,
            G17PTopLevelCommandType::KsmKickQueueAddKicks,
            G17PTopLevelCommandType::ConfigUpdate,
            G17PTopLevelCommandType::EntrySignal,
            G17PTopLevelCommandType::Nop,
            G17PTopLevelCommandType::LateEvalEventSignal,
        ];
        for command_type in known {
            let mut command = [0xa5; 9];
            command[..4].copy_from_slice(&(command_type as u32).to_le_bytes());
            assert_eq!(
                decode_g17p_top_level_command(G17ChannelAbiTarget::A18ProG17P, &command),
                Ok(command_type)
            );
        }

        for length in 0..4 {
            assert_eq!(
                decode_g17p_top_level_command(
                    G17ChannelAbiTarget::A18ProG17P,
                    &[0, 0, 0, 0][..length]
                ),
                Err(SubmissionError::CommandHeaderTooShort)
            );
        }

        // The dispatcher reads all four little-endian bytes at offset zero;
        // it neither truncates to byte zero nor scans for a nested opcode.
        assert_eq!(
            decode_g17p_top_level_command(
                G17ChannelAbiTarget::A18ProG17P,
                &[0, 0, 0, 1, 18, 0, 0, 0]
            ),
            Err(SubmissionError::CommandTagInvalid)
        );
    }

    #[test]
    fn g17p_top_level_command_decode_fails_closed_by_tag_and_identity() {
        for raw in [6u32, 8, 10, 11] {
            assert_eq!(
                decode_g17p_top_level_command(G17ChannelAbiTarget::A18ProG17P, &raw.to_le_bytes()),
                Err(SubmissionError::CommandTagUnresolved)
            );
        }
        for raw in [13u32, 19, u32::MAX] {
            assert_eq!(
                decode_g17p_top_level_command(G17ChannelAbiTarget::A18ProG17P, &raw.to_le_bytes()),
                Err(SubmissionError::CommandTagInvalid)
            );
        }
        for target in [
            G17ChannelAbiTarget::M5G17G,
            G17ChannelAbiTarget::M5ProMaxG17X,
        ] {
            assert_eq!(
                decode_g17p_top_level_command(target, &0u32.to_le_bytes()),
                Err(SubmissionError::CommandAbiUnconfirmed)
            );
            assert_eq!(
                classify_g17p_channel_command(target, 0),
                Err(SubmissionError::CommandAbiUnconfirmed)
            );
        }
    }

    #[test]
    fn g17p_command_dispatch_matches_the_firmware_jump_table() {
        assert_eq!(G17P_CHANNEL_COMMAND_PROCESS_MASK, 0x0005_d2bf);
        assert_eq!(G17P_CHANNEL_COMMAND_PM_REBUILD_MASK, 0x0000_0040);
        assert_eq!(G17P_CHANNEL_COMMAND_INVALID_MASK, 0x0002_2d00);
        assert_eq!(
            G17P_CHANNEL_COMMAND_PROCESS_MASK
                | G17P_CHANNEL_COMMAND_PM_REBUILD_MASK
                | G17P_CHANNEL_COMMAND_INVALID_MASK,
            (1 << (G17P_CHANNEL_COMMAND_TYPE_MAX + 1)) - 1
        );

        for raw in 0..=G17P_CHANNEL_COMMAND_TYPE_MAX {
            let expected = match raw {
                0 | 1 | 2 | 3 | 4 | 5 | 7 | 9 | 12 | 14 | 15 | 16 | 18 => {
                    G17PChannelCommandDisposition::Process
                }
                6 => G17PChannelCommandDisposition::PmRebuildForbiddenFatal,
                8 | 10 | 11 | 13 | 17 => G17PChannelCommandDisposition::InvalidCommandFatal,
                _ => unreachable!(),
            };
            assert_eq!(
                classify_g17p_channel_command(G17ChannelAbiTarget::A18ProG17P, raw),
                Ok(expected)
            );
        }
        for raw in [19, u32::MAX] {
            assert_eq!(
                classify_g17p_channel_command(G17ChannelAbiTarget::A18ProG17P, raw),
                Ok(G17PChannelCommandDisposition::InvalidCommandFatal)
            );
        }

        // Host ABI identity and matched-firmware admission intentionally split.
        let nop = (G17PTopLevelCommandType::Nop as u32).to_le_bytes();
        assert_eq!(
            decode_g17p_top_level_command(G17ChannelAbiTarget::A18ProG17P, &nop),
            Ok(G17PTopLevelCommandType::Nop)
        );
        assert_eq!(
            admit_g17p_top_level_command(G17ChannelAbiTarget::A18ProG17P, &nop),
            Err(SubmissionError::CommandTagRejectedByFirmware)
        );

        for command_type in [
            G17PTopLevelCommandType::Ta,
            G17PTopLevelCommandType::ThreeD,
            G17PTopLevelCommandType::FastBlit,
            G17PTopLevelCommandType::Cl,
            G17PTopLevelCommandType::Barrier,
            G17PTopLevelCommandType::PmGrow,
            G17PTopLevelCommandType::RemoteNode,
            G17PTopLevelCommandType::SharedEventSignal,
            G17PTopLevelCommandType::AddFinalFrgGtpcKick,
            G17PTopLevelCommandType::KsmKickQueueAddKicks,
            G17PTopLevelCommandType::ConfigUpdate,
            G17PTopLevelCommandType::EntrySignal,
            G17PTopLevelCommandType::LateEvalEventSignal,
        ] {
            assert_eq!(
                admit_g17p_top_level_command(
                    G17ChannelAbiTarget::A18ProG17P,
                    &(command_type as u32).to_le_bytes()
                ),
                Ok(command_type)
            );
        }
    }

    #[test]
    fn g17p_command_prefilter_matches_literal_mask() {
        assert_eq!(G17P_CHANNEL_COMMAND_PREFILTER_MASK, 0x0001_d03f);
        for raw in 0..=20 {
            let expected = matches!(raw, 0 | 1 | 2 | 3 | 4 | 5 | 12 | 14 | 15 | 16);
            assert_eq!(
                g17p_channel_command_prefilter_allows(G17ChannelAbiTarget::A18ProG17P, raw),
                Ok(expected)
            );
        }
        assert_eq!(
            g17p_channel_command_prefilter_allows(G17ChannelAbiTarget::M5G17G, 0),
            Err(SubmissionError::CommandAbiUnconfirmed)
        );
    }

    #[test]
    fn provenance_ledger_is_explicit_about_reuse_and_fork() {
        assert_eq!(
            SUBMISSION_ABI_LEDGER.tx_doorbell,
            AbiProvenance::IdenticalToAgx2
        );
        assert_eq!(
            SUBMISSION_ABI_LEDGER.kick_word,
            AbiProvenance::ForkedAgx3Grounded
        );
        assert_eq!(
            SUBMISSION_ABI_LEDGER.kick_queue_producer,
            AbiProvenance::ForkedAgx3Grounded
        );
        assert_eq!(
            SUBMISSION_ABI_LEDGER.host_add_kicks_mmio,
            AbiProvenance::ForkedAgx3Grounded
        );
        assert_eq!(
            SUBMISSION_ABI_LEDGER.g17p_queue_config,
            AbiProvenance::ForkedAgx3Grounded
        );
        assert_eq!(
            SUBMISSION_ABI_LEDGER.g17p_completion_descriptors,
            AbiProvenance::ForkedAgx3Grounded
        );
        assert!(HOST_SKSM_ADD_KICKS_PATH_SELECTED);
        assert_eq!(
            SUBMISSION_ABI_LEDGER.kick_record,
            AbiProvenance::ForkedAgx3Grounded
        );
        // The channel/ring state records stay inherited and fail-closed. The
        // widened native descriptor geometry is grounded first-hand.
        assert_eq!(
            SUBMISSION_ABI_LEDGER.channel_state,
            AbiProvenance::Agx2InheritedUnconfirmed
        );
        assert_eq!(
            SUBMISSION_ABI_LEDGER.g17p_channel_state,
            AbiProvenance::ForkedAgx3Grounded
        );
        assert_eq!(
            SUBMISSION_ABI_LEDGER.ring_state,
            AbiProvenance::Agx2InheritedUnconfirmed
        );
        assert_eq!(
            SUBMISSION_ABI_LEDGER.channel_descriptor,
            AbiProvenance::ForkedAgx3Grounded
        );
        assert_eq!(
            SUBMISSION_ABI_LEDGER.host_channel_state_reset,
            AbiProvenance::ForkedAgx3Grounded
        );
        assert_eq!(
            SUBMISSION_ABI_LEDGER.host_uncached_channel_reset,
            AbiProvenance::ForkedAgx3Grounded
        );
        assert_eq!(
            SUBMISSION_ABI_LEDGER.g17p_command_types,
            AbiProvenance::ForkedAgx3Grounded
        );
        assert!(CHANNEL_DESCRIPTOR_STRIDE_CONFIRMED);
    }

    #[test]
    fn transport_stays_fail_closed_with_named_live_gaps() {
        assert!(GROUNDED_SUBMISSION_PRODUCER_AVAILABLE);
        assert!(!EXECUTABLE_SUBMISSION_TRANSPORT_AVAILABLE);
        assert!(!SUBMISSION_LIVE_GAPS.all_proven());
        assert!(!SUBMISSION_LIVE_GAPS.sksm_hardware_kick_fifo_mmio_scratch_proven);
        assert!(!SUBMISSION_LIVE_GAPS.doorbell_round_trip_proven);
        assert!(!SUBMISSION_LIVE_GAPS.completion_irq_route_proven);

        // A hypothetical all-proven set would flip the executable signal — the
        // gate is wired to the evidence, not hard-coded.
        let proven = SubmissionLiveGaps {
            sksm_hardware_kick_fifo_mmio_scratch_proven: true,
            doorbell_round_trip_proven: true,
            completion_irq_route_proven: true,
        };
        assert!(proven.all_proven());
    }
}
