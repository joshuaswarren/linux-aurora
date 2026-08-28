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
//! separates the host's `Nop=17` name from the firmware's fatal handling of
//! that value.
//!
//! Grounding is first-hand from the on-disk M5 role macho `GFX.a010.macho`
//! (`g17s a0, SKSM Enabled`), all three pinned A18 Pro `g17p` firmware variants
//! (`a000`, `b000`, and `b100`), and the pinned G17P/G17G/G17X host kexts. The
//! native G17P channel-state ABI is admitted only for the A18 Pro identity;
//! the M5-family identities remain unsupported.
//! Firmware addresses are VAs (0xfffffc0000000000 + fileoff - 0x4000). See
//! `research/gpu/GPU_AGX3_G17_SUBMISSION_2026-08-25.md`.
//!
//! Two-signal discipline, mirroring `g17_rtkit`/`g17_boot`:
//! [`GROUNDED_SUBMISSION_PRODUCER_AVAILABLE`] states that the *codec* is real
//! and cited; [`EXECUTABLE_SUBMISSION_TRANSPORT_AVAILABLE`] states whether a
//! submission can be driven to completion offline. It is `false`: the SKSM
//! completion path is a hardware kick FIFO the firmware reads through indexed
//! MMIO scratch registers. The host publication form, runtime scratch geometry,
//! and resource-0 ordered writer are implemented. J700 has completed the
//! channel-doorbell round trip with exact compute output across a retained CL2
//! queue, and the measured first-partial path has executed graphics through
//! the classic channel transport. Linux still needs that contract wired into
//! its runtime and completion IRQ ingress. The native kext `addKicks` path is
//! tracked independently. See [`SUBMISSION_LIVE_GAPS`].

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
/// [`SUBMISSION_LIVE_GAPS`]. This keeps `SUBMISSION_TRANSPORT` out of
/// `g17_boot::pre_handoff_gate` admission until completion is implemented.
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

/// The three GPU submission pipes. The firmware treats `pipe_type == 3` as invalid
/// (`cmp w8, #0x3 ; b.eq 0x252f4` — no such pipe).
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
#[repr(u8)]
pub(crate) enum PipeType {
    Vertex = 0,
    Fragment = 1,
    Compute = 2,
}

/// Errors from the submission codec.
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
    /// A G17P CL entry stride cannot hold the complete 0x180-byte host body.
    KickEntryStrideTooSmall,
    /// Multiplying the slot by the runtime entry stride overflowed u32.
    KickEntryOffsetOverflow,
    /// The queue already has `num_stamps` entries awaiting completion.
    KickQueueSubmittedFull,
    /// A completion observed no submitted entries or exceeded their count.
    KickCompletionUnderflow,
    /// A provider-owned pre-B2 state object is smaller than one 16 KiB page.
    KickB2PrimaryStateBufferTooSmall,
    /// The retained queue already published its one-time pre-B2 state.
    KickB2PrimaryStateAlreadyPublished,
    /// A retained queue static object is smaller than its native extent.
    KickRetainedStorageBufferTooSmall,
    /// A CL entry has one implicit dependency and at most 31 explicit ones.
    KickDependencyCountOutOfRange,
    /// A CL dependency names a queue outside the seven-bit entry field.
    KickDependencyQueueIdOutOfRange,
    /// The MCache aperture index or count cannot be represented in its field.
    KickMcacheFieldOutOfRange,
    /// The MCache aperture address is unaligned or exceeds its 38-bit field.
    KickMcacheAddressUnencodable,
    /// The RCE kind is outside the two-bit entry field.
    KickRceKindOutOfRange,
    /// An RCE tag is outside its seven-bit entry field.
    KickRceTagOutOfRange,
    /// An RCE address is unaligned or exceeds its 38-bit entry field.
    KickRceAddressUnencodable,
    /// The middle QoS field is wider than the five bits copied by the host.
    KickQosClassOutOfRange,
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
    /// A first-render status pointer overflowed while deriving the FWCtl VAs.
    PartialOpeningAddressOverflow,
    /// A first-render scheduler or index mapping is not exactly one 16 KiB page.
    PartialOpeningPageSize,
    /// A first-render outer-channel counter has bits outside the 8-bit ring index.
    PartialOpeningChannelCounterOutOfRange,
    /// A first-render queue head does not fit the outer slot's 16-bit field.
    PartialOpeningQueueHeadOutOfRange,
    /// The host/firmware meaning of this in-range top-level tag is unresolved.
    CommandTagUnresolved,
    /// This value is not a grounded G17P top-level command tag.
    CommandTagInvalid,
    /// The host names this tag, but the matched firmware handles it fatally.
    CommandTagInvalidForFirmware,
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
/// masks and its invalid `pipe == 3` outcome.
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
//     G17 firmware consumer, so those ledger entries remain unsupported.
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
/// and consumer were both inspected. M5-family targets remain unsupported.
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

// ---------------------------------------------------------------------------
// 2d. Proven G17P first-partial graphics publication.
//
//     The measured J700 first-partial source contract executes graphics through
//     the native partial graph. It publishes a one-record scheduler page and
//     context-2 primary index, presents five primary status fields before
//     control 0x84, then exposes 3D before TA and sends one EP 0x21/0x83 kick
//     for transport pair 2 on channel 8. This is the classic channel path and
//     remains separate from the kext's SKSM queue-entry plus `addKicks` path.
// ---------------------------------------------------------------------------

pub(crate) const G17P_PARTIAL_OPENING_PAGE_SIZE: usize = 0x4000;
pub(crate) const G17P_PARTIAL_OPENING_SCHEDULER_GPU_VA: u64 = 0xffff_fc20_015e_0000;
pub(crate) const G17P_PARTIAL_OPENING_SCHEDULER_WORD: u64 = 0x0000_0020_0001_9000;
pub(crate) const G17P_PARTIAL_OPENING_PRIMARY_INDEX_GPU_VA: u64 = 0x0000_0010_0019_0000;
pub(crate) const G17P_PARTIAL_OPENING_FWCTL_OFFSET: u64 = 0x001c_0000;
pub(crate) const G17P_PARTIAL_OPENING_CONTROL_MESSAGE_SIZE: u64 = 0x40;
pub(crate) const G17P_PARTIAL_OPENING_WORK_DOORBELL_CHANNEL: u8 = 8;
pub(crate) const G17P_PARTIAL_OPENING_PAIRED_DOORBELL: u64 =
    MSG_TX_DOORBELL | G17P_PARTIAL_OPENING_WORK_DOORBELL_CHANNEL as u64;
pub(crate) const G17P_PARTIAL_OPENING_OUTER_SLOT_SIZE: usize = 0x18;
pub(crate) const G17P_PARTIAL_OPENING_OUTER_SLOT_COUNT: u32 = 0x100;
pub(crate) const G17P_PARTIAL_OPENING_TILING_CHANNEL_TABLE_INDEX: u8 = 6;
pub(crate) const G17P_PARTIAL_OPENING_FRAGMENT_CHANNEL_TABLE_INDEX: u8 = 7;

/// Root, queue, and scheduler identities at the proven first-work boundary.
/// Linux owns the backing pages; the physical addresses from the m1n1 run are
/// intentionally not part of this contract.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) struct G17PPartialOpeningContextContract {
    pub(crate) firmware_low_root_slot: u8,
    pub(crate) firmware_context_id: u8,
    pub(crate) render_root_slot: u8,
    pub(crate) render_context_id: u8,
    pub(crate) context0_empty_high_root_slot: u8,
    pub(crate) render_empty_high_root_slot: u8,
    pub(crate) transport_pair: u8,
    pub(crate) descriptor_pair: u8,
    pub(crate) graph_context_id: u8,
    pub(crate) optional_identity_context_id: u8,
    pub(crate) descriptor_context_id: u8,
    pub(crate) scheduler_class: u8,
    pub(crate) work_doorbell_channel: u8,
    pub(crate) firmware_low_root_present_at_first_work: bool,
    pub(crate) source_firmware_high_root_table_resident: bool,
}

pub(crate) const G17P_PARTIAL_OPENING_CONTEXT: G17PPartialOpeningContextContract =
    G17PPartialOpeningContextContract {
        firmware_low_root_slot: 1,
        firmware_context_id: 64,
        render_root_slot: 2,
        render_context_id: 1,
        context0_empty_high_root_slot: 0,
        render_empty_high_root_slot: 1,
        transport_pair: 2,
        descriptor_pair: 0,
        graph_context_id: 2,
        optional_identity_context_id: 3,
        descriptor_context_id: 2,
        scheduler_class: 2,
        work_doorbell_channel: G17P_PARTIAL_OPENING_WORK_DOORBELL_CHANNEL,
        firmware_low_root_present_at_first_work: false,
        source_firmware_high_root_table_resident: false,
    };

/// One exact 64-bit write into the primary status-B object before control 0x84.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) struct G17PPartialOpeningStatusWrite {
    pub(crate) offset: u32,
    pub(crate) value: u64,
}

/// State required to prepare one half of the paired outer-channel publish.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) struct G17PPartialOpeningOuterChannelState {
    pub(crate) consumers: [u32; 2],
    pub(crate) producer: u32,
    pub(crate) queue_gpu_va: u64,
    pub(crate) queue_write_index: u32,
}

/// One complete 0x18-byte outer slot followed by its producer publication.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) struct G17PPartialOpeningOuterSlotPlan {
    pub(crate) channel_table_index: u8,
    pub(crate) slot_index: u8,
    pub(crate) slot_byte_offset: u32,
    pub(crate) slot: [u8; G17P_PARTIAL_OPENING_OUTER_SLOT_SIZE],
    pub(crate) next_producer: u32,
}

/// The two producer writes and the single mailbox kick for first graphics.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) struct PreparedG17PPartialOpeningWorkPair {
    pub(crate) fragment: G17PPartialOpeningOuterSlotPlan,
    pub(crate) tiling: G17PPartialOpeningOuterSlotPlan,
    pub(crate) paired_doorbell: u64,
}

/// Side-effect order measured at the successful first-partial boundary.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) enum G17PPartialOpeningPublicationStep {
    PrimarySchedulerPage,
    PrimaryIndexPage,
    PreControlStatus,
    ControlDone0x84,
    FragmentState,
    FragmentProducer,
    LateTilingState,
    SystemBarrierBeforeTilingProducer,
    TilingProducer,
    SystemBarrierBeforePairedDoorbell,
    PairedWorkDoorbell0x83,
}

pub(crate) const G17P_PARTIAL_OPENING_PUBLICATION_ORDER: [G17PPartialOpeningPublicationStep; 11] = [
    G17PPartialOpeningPublicationStep::PrimarySchedulerPage,
    G17PPartialOpeningPublicationStep::PrimaryIndexPage,
    G17PPartialOpeningPublicationStep::PreControlStatus,
    G17PPartialOpeningPublicationStep::ControlDone0x84,
    G17PPartialOpeningPublicationStep::FragmentState,
    G17PPartialOpeningPublicationStep::FragmentProducer,
    G17PPartialOpeningPublicationStep::LateTilingState,
    G17PPartialOpeningPublicationStep::SystemBarrierBeforeTilingProducer,
    G17PPartialOpeningPublicationStep::TilingProducer,
    G17PPartialOpeningPublicationStep::SystemBarrierBeforePairedDoorbell,
    G17PPartialOpeningPublicationStep::PairedWorkDoorbell0x83,
];

/// Build the computed one-record scheduler page that decided graphics entry in
/// the successful post-control graft.
pub(crate) fn apply_g17p_partial_opening_scheduler_page(
    page: &mut [u8],
) -> Result<(), SubmissionError> {
    if page.len() != G17P_PARTIAL_OPENING_PAGE_SIZE {
        return Err(SubmissionError::PartialOpeningPageSize);
    }
    page.fill(0);
    put_u64_le(page, 0, G17P_PARTIAL_OPENING_SCHEDULER_WORD);
    Ok(())
}

/// Array-returning form retained for host tests. Kernel mappings should use
/// [`apply_g17p_partial_opening_scheduler_page`] to avoid a 16 KiB temporary.
pub(crate) fn build_g17p_partial_opening_scheduler_page() -> [u8; G17P_PARTIAL_OPENING_PAGE_SIZE] {
    let mut page = [0u8; G17P_PARTIAL_OPENING_PAGE_SIZE];
    let _ = apply_g17p_partial_opening_scheduler_page(&mut page);
    page
}

/// Build the eight-group context-2 primary index used by the proven opening.
pub(crate) fn apply_g17p_partial_opening_primary_index_page(
    page: &mut [u8],
) -> Result<(), SubmissionError> {
    if page.len() != G17P_PARTIAL_OPENING_PAGE_SIZE {
        return Err(SubmissionError::PartialOpeningPageSize);
    }
    page.fill(0);
    let mut index = 0usize;
    for (start, groups) in [(0x11u32, 6u32), (0x3cu32, 2u32)] {
        for group in 0..groups {
            let base = start + group * 5;
            for member in 0..4u32 {
                put_u32_le(page, index * 4, base + member);
                index += 1;
            }
        }
    }
    Ok(())
}

/// Array-returning form retained for host tests. Kernel mappings should use
/// [`apply_g17p_partial_opening_primary_index_page`] directly.
pub(crate) fn build_g17p_partial_opening_primary_index_page() -> [u8; G17P_PARTIAL_OPENING_PAGE_SIZE]
{
    let mut page = [0u8; G17P_PARTIAL_OPENING_PAGE_SIZE];
    let _ = apply_g17p_partial_opening_primary_index_page(&mut page);
    page
}

/// Derive the five stable status-B writes presented before control 0x84.
pub(crate) const fn prepare_g17p_partial_opening_pre_0x84_status(
    kernel_va_base: u64,
) -> Result<[G17PPartialOpeningStatusWrite; 5], SubmissionError> {
    let fwctl_va = match kernel_va_base.checked_add(G17P_PARTIAL_OPENING_FWCTL_OFFSET) {
        Some(value) => value,
        None => return Err(SubmissionError::PartialOpeningAddressOverflow),
    };
    let fwctl_next = match fwctl_va.checked_add(G17P_PARTIAL_OPENING_CONTROL_MESSAGE_SIZE) {
        Some(value) => value,
        None => return Err(SubmissionError::PartialOpeningAddressOverflow),
    };
    Ok([
        G17PPartialOpeningStatusWrite {
            offset: 0x4018,
            value: 0x0005_0601_0000_0000,
        },
        G17PPartialOpeningStatusWrite {
            offset: 0x4020,
            value: 61,
        },
        G17PPartialOpeningStatusWrite {
            offset: 0x40b0,
            value: 5,
        },
        G17PPartialOpeningStatusWrite {
            offset: 0x48e0,
            value: fwctl_va,
        },
        G17PPartialOpeningStatusWrite {
            offset: 0x48e8,
            value: fwctl_next,
        },
    ])
}

fn prepare_g17p_partial_opening_outer_slot(
    state: G17PPartialOpeningOuterChannelState,
    channel_table_index: u8,
    queue_grid_index: u8,
    kind: u32,
) -> Result<G17PPartialOpeningOuterSlotPlan, SubmissionError> {
    if state.producer >= G17P_PARTIAL_OPENING_OUTER_SLOT_COUNT
        || state
            .consumers
            .iter()
            .any(|consumer| *consumer >= G17P_PARTIAL_OPENING_OUTER_SLOT_COUNT)
    {
        return Err(SubmissionError::PartialOpeningChannelCounterOutOfRange);
    }
    if state.queue_write_index > u16::MAX as u32 {
        return Err(SubmissionError::PartialOpeningQueueHeadOutOfRange);
    }

    let next_producer = (state.producer + 1) & 0xff;
    if state.consumers.contains(&next_producer) {
        return Err(SubmissionError::RingFull);
    }

    let mut slot = [0u8; G17P_PARTIAL_OPENING_OUTER_SLOT_SIZE];
    put_u64_le(&mut slot, 0x08, state.queue_gpu_va);
    put_u32_le(&mut slot, 0x10, kind);
    put_u32_le(
        &mut slot,
        0x14,
        state.queue_write_index | ((queue_grid_index as u32) << 16) | (1 << 24),
    );

    Ok(G17PPartialOpeningOuterSlotPlan {
        channel_table_index,
        slot_index: state.producer as u8,
        slot_byte_offset: state.producer * G17P_PARTIAL_OPENING_OUTER_SLOT_SIZE as u32,
        slot,
        next_producer,
    })
}

/// Couple the fragment-first/tiling-second outer slots to their one channel-8
/// 0x83 notification. These are the initdata work rings at table indices 7 and
/// 6, not the kext's separate 8-byte command-pointer rings. The caller performs
/// the barriers in [`G17P_PARTIAL_OPENING_PUBLICATION_ORDER`].
pub(crate) fn prepare_g17p_partial_opening_work_pair(
    fragment: G17PPartialOpeningOuterChannelState,
    tiling: G17PPartialOpeningOuterChannelState,
) -> Result<PreparedG17PPartialOpeningWorkPair, SubmissionError> {
    let fragment = match prepare_g17p_partial_opening_outer_slot(
        fragment,
        G17P_PARTIAL_OPENING_FRAGMENT_CHANNEL_TABLE_INDEX,
        1,
        1,
    ) {
        Ok(value) => value,
        Err(error) => return Err(error),
    };
    let tiling = match prepare_g17p_partial_opening_outer_slot(
        tiling,
        G17P_PARTIAL_OPENING_TILING_CHANNEL_TABLE_INDEX,
        0,
        0,
    ) {
        Ok(value) => value,
        Err(error) => return Err(error),
    };
    Ok(PreparedG17PPartialOpeningWorkPair {
        fragment,
        tiling,
        paired_doorbell: G17P_PARTIAL_OPENING_PAIRED_DOORBELL,
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
/// Return an error for a zero-sized ring or a producer outside the ring. A
/// valid `index` is always below `u32::MAX`, so `index + 1` cannot overflow
/// after these checks.
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
//    the queue and can reach the firmware's out-of-order fatal path.
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
/// Invalid geometry returns an error before an entry address or doorbell is
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
// 5a. A18 Pro / G17P CL kick-entry body and host queue-state transition.
//
//     `AGXKSMKickQueueG16::encodeCLKickEntry` at
//     0xfffffe0008a56848-0xfffffe0008a56a64 gathers the CL descriptor inputs.
//     Its shared encoder at 0xfffffe0008a56a68-0xfffffe0008a56eb4 writes the
//     complete 0x180-byte body described below. The recovered G17S encoder at
//     0xfffffe0008b51f74-0xfffffe0008b521c4 independently confirms the slot,
//     timestamp, barrier, and counter tail; scratch tags remain target-specific.
//
//     The body is the first 0x180 bytes of each 0x200-spaced queue-context
//     record captured by m1n1. The host zeroes the runtime stride before these
//     stores, so every byte not named here stays zero.
// ---------------------------------------------------------------------------

pub(crate) const G17P_CL_KICK_ENTRY_SIZE: usize = 0x180;
pub(crate) const G17P_CL_DEPENDENCIES_MAX: usize = 32;
pub(crate) const G17P_CL_EXPLICIT_DEPENDENCIES_MAX: usize = G17P_CL_DEPENDENCIES_MAX - 1;
const G17P_CL_PACKED_ADDRESS_MASK: u64 = (1u64 << 38) - 1;

/// Host-object fields used while selecting, publishing, and retiring one CL
/// kick entry. These are CPU-object offsets, not firmware queue-record offsets.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) struct G17PClKickQueueStateLayout {
    pub(crate) queue_id: usize,
    pub(crate) num_stamps: usize,
    pub(crate) entry_stride: usize,
    pub(crate) completion_selector: usize,
    pub(crate) submitted: usize,
    pub(crate) current_timestamp: usize,
    pub(crate) entry_cpu_base: usize,
    pub(crate) entry_gpu_base: usize,
    pub(crate) base_timestamp: usize,
    pub(crate) previous_valid: usize,
    pub(crate) previous_timestamp: usize,
}

pub(crate) const G17P_CL_KICK_QUEUE_STATE_LAYOUT: G17PClKickQueueStateLayout =
    G17PClKickQueueStateLayout {
        queue_id: 0x0c,
        num_stamps: 0x10,
        entry_stride: 0x14,
        completion_selector: 0x1c,
        submitted: 0x44,
        current_timestamp: 0x48,
        entry_cpu_base: 0x78,
        entry_gpu_base: 0x80,
        base_timestamp: 0x90,
        previous_valid: 0x98,
        previous_timestamp: 0xa0,
    };

/// Fixed byte positions inside the 0x180-byte G17P CL entry body.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) struct G17PClKickEntryLayout {
    pub(crate) header: usize,
    pub(crate) mcache: usize,
    pub(crate) payload: [usize; 2],
    pub(crate) dependency_control: usize,
    pub(crate) dependencies: usize,
    pub(crate) event_mask: [usize; 4],
    pub(crate) rce_kind: usize,
    pub(crate) rce_bindings: [usize; 4],
    pub(crate) zero_170: usize,
    pub(crate) auxiliary: usize,
}

pub(crate) const G17P_CL_KICK_ENTRY_LAYOUT: G17PClKickEntryLayout = G17PClKickEntryLayout {
    header: 0x00,
    mcache: 0x08,
    payload: [0x10, 0x18],
    dependency_control: 0x20,
    dependencies: 0x28,
    event_mask: [0x128, 0x130, 0x138, 0x140],
    rce_kind: 0x148,
    rce_bindings: [0x150, 0x158, 0x160, 0x168],
    zero_170: 0x170,
    auxiliary: 0x178,
};

/// One explicit barrier dependency after its descriptor-selected shared stamp
/// has been loaded. `event_word` is barrier-record word zero; record word one
/// selects the shared-stamp source and has no independent wire field.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) struct G17PClBarrierDependency {
    pub(crate) event_word: u32,
    pub(crate) shared_stamp: u64,
    pub(crate) queue_id: u8,
}

/// Optional MCache aperture descriptor copied into entry +0x08.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) struct G17PClMcacheAperture {
    pub(crate) address: u64,
    pub(crate) index: u8,
    pub(crate) count: u8,
}

/// One of the four address/tag bindings written at +0x150..+0x168.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) struct G17PClRceBinding {
    pub(crate) address: u64,
    pub(crate) tag: u8,
}

/// Physical-queue QoS fields packed above the 32-bit dependency mask at entry
/// +0x20. These remain with the registered SKSM queue when later descriptors
/// publish new per-command event masks.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) struct G17PClQosConfig {
    pub(crate) queue_byte_32: u8,
    pub(crate) class_40: u8,
    pub(crate) word_48: u16,
}

/// Physical queue configuration and mutable producer state read before one CL
/// entry. The QoS fields belong to this retained queue, while the event mask in
/// [`G17PClKickEntryOperands`] belongs to the command being submitted.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) struct G17PClKickProducerState {
    pub(crate) queue_id: u8,
    pub(crate) num_stamps: u16,
    pub(crate) entry_stride: u32,
    pub(crate) completion_selector: u8,
    pub(crate) qos: G17PClQosConfig,
    pub(crate) submitted: u32,
    pub(crate) current_timestamp: u64,
    pub(crate) base_timestamp: u64,
    pub(crate) previous_valid: bool,
    pub(crate) previous_timestamp: u64,
}

/// Descriptor and call-site operands consumed by `encodeCLKickEntry`.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) struct G17PClKickEntryOperands<'a> {
    /// Descriptor +0x4c bit 0; true sets entry-header bit 1.
    pub(crate) descriptor_flag_4c: bool,
    /// Descriptor +0x5c8 bit 0; true sets entry-header bit 61.
    pub(crate) descriptor_flag_5c8: bool,
    /// Result of `convertEventMachineTSToHWTS` for a queue with no predecessor.
    pub(crate) converted_command_timestamp: u64,
    pub(crate) barriers: &'a [G17PClBarrierDependency],
    pub(crate) mcache: Option<G17PClMcacheAperture>,
    /// The two qwords reached through `AGXKSMKickQueueEntryPayload`.
    pub(crate) payload: [u64; 2],
    /// Per-command mask copied independently to +0x128..+0x140.
    pub(crate) event_mask: [u64; 4],
    pub(crate) rce_kind: u8,
    pub(crate) rce_bindings: [G17PClRceBinding; 4],
    /// Descriptor +0x178 when nonzero, otherwise the descriptor-indexed host
    /// table value selected by +0x61c.
    pub(crate) auxiliary: u64,
}

/// Pure result of one entry encode and the host state values published after
/// the entry barrier. No shared memory or MMIO is touched by this function.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PreparedG17PClKickEntry {
    /// Bytes the live publisher must clear before copying `entry`.
    pub(crate) zero_length: u32,
    pub(crate) entry: [u8; G17P_CL_KICK_ENTRY_SIZE],
    pub(crate) entry_index: u8,
    pub(crate) entry_offset: u32,
    pub(crate) current_timestamp: u64,
    pub(crate) next_timestamp: u64,
    pub(crate) submitted_before: u32,
    pub(crate) submitted_after: u32,
    pub(crate) previous_timestamp_after: u64,
    /// Producer state to use for the next entry on this physical queue.
    pub(crate) producer_after: G17PClKickProducerState,
}

/// Ordering at the side-effect boundary of the host encoder.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) enum G17PClKickPublicationStep {
    ZeroEntryStride,
    EntryBody,
    InnerShareableBarrier,
    NextTimestamp,
    SubmittedAtomicAdd,
    PreviousValid,
    PreviousTimestamp,
}

pub(crate) const G17P_CL_KICK_PUBLICATION_ORDER: [G17PClKickPublicationStep; 7] = [
    G17PClKickPublicationStep::ZeroEntryStride,
    G17PClKickPublicationStep::EntryBody,
    G17PClKickPublicationStep::InnerShareableBarrier,
    G17PClKickPublicationStep::NextTimestamp,
    G17PClKickPublicationStep::SubmittedAtomicAdd,
    G17PClKickPublicationStep::PreviousValid,
    G17PClKickPublicationStep::PreviousTimestamp,
];

fn encode_g17p_cl_dependency(timestamp: u64, queue_id: u8) -> Result<u64, SubmissionError> {
    if timestamp & !KICK_TS_MASK != 0 {
        return Err(SubmissionError::KickTimestampOutOfRange);
    }
    if queue_id > KICK_QID_MASK as u8 {
        return Err(SubmissionError::KickDependencyQueueIdOutOfRange);
    }
    Ok(timestamp | ((queue_id as u64) << KICK_QID_SHIFT))
}

fn encode_g17p_cl_barrier_dependency(
    queue: G17PClKickProducerState,
    barrier: G17PClBarrierDependency,
) -> Result<u64, SubmissionError> {
    if barrier.queue_id > KICK_QID_MASK as u8 {
        return Err(SubmissionError::KickDependencyQueueIdOutOfRange);
    }

    let stamp_count = queue.num_stamps as u64;
    let event_timestamp = (((barrier.event_word as u64) >> 8)
        | barrier.shared_stamp.wrapping_shl(24))
    .wrapping_sub(1);
    let event_stamp = event_timestamp / stamp_count;
    let event_index = event_timestamp % stamp_count;
    let index_sum = (queue.base_timestamp & KICK_STAMP_INDEX_MASK) + event_index;
    let index_carry = index_sum / stamp_count;
    let stamp = ((queue.base_timestamp >> KICK_STAMP_SHIFT) as u32)
        .wrapping_add(event_stamp as u32)
        .wrapping_add(index_carry as u32);
    let index = (index_sum % stamp_count) as u8;

    encode_g17p_cl_dependency(
        encode_kick_timestamp(KickTimestamp {
            stamp,
            stamp_index: index,
        }),
        barrier.queue_id,
    )
}

fn encode_g17p_cl_mcache(aperture: G17PClMcacheAperture) -> Result<u64, SubmissionError> {
    if aperture.index > 0x3f || aperture.count == 0 || aperture.count > 0x40 {
        return Err(SubmissionError::KickMcacheFieldOutOfRange);
    }
    if aperture.address & 0x1f != 0 || aperture.address >> 5 > G17P_CL_PACKED_ADDRESS_MASK {
        return Err(SubmissionError::KickMcacheAddressUnencodable);
    }

    Ok((aperture.index as u64)
        | ((aperture.address >> 5) << 6)
        | (((aperture.count as u64 - 1) & 0x3f) << 54))
}

fn encode_g17p_cl_rce(binding: G17PClRceBinding) -> Result<u64, SubmissionError> {
    if binding.tag > KICK_QID_MASK as u8 {
        return Err(SubmissionError::KickRceTagOutOfRange);
    }
    if binding.address & 0x1f != 0 || binding.address >> 5 > G17P_CL_PACKED_ADDRESS_MASK {
        return Err(SubmissionError::KickRceAddressUnencodable);
    }

    Ok((binding.address >> 5) | ((binding.tag as u64) << 43))
}

/// Encode the complete G17P CL entry body and calculate the exact host state
/// transition that follows the recovered `dmb ish`.
pub(crate) fn prepare_g17p_cl_kick_entry(
    queue: G17PClKickProducerState,
    operands: G17PClKickEntryOperands<'_>,
) -> Result<PreparedG17PClKickEntry, SubmissionError> {
    if queue.entry_stride == 0 {
        return Err(SubmissionError::KickEntryStrideZero);
    }
    if queue.entry_stride < G17P_CL_KICK_ENTRY_SIZE as u32 {
        return Err(SubmissionError::KickEntryStrideTooSmall);
    }
    if queue.completion_selector > 3 {
        return Err(SubmissionError::KickQueueCompletionSelectorOutOfRange);
    }
    if queue.base_timestamp & !KICK_TS_MASK != 0 {
        return Err(SubmissionError::KickTimestampOutOfRange);
    }
    if operands.barriers.len() > G17P_CL_EXPLICIT_DEPENDENCIES_MAX {
        return Err(SubmissionError::KickDependencyCountOutOfRange);
    }
    if queue.qos.class_40 > 0x1f {
        return Err(SubmissionError::KickQosClassOutOfRange);
    }
    if operands.rce_kind > 3 {
        return Err(SubmissionError::KickRceKindOutOfRange);
    }

    let prepared = prepare_kick_submission(
        queue.current_timestamp,
        queue.queue_id,
        queue.num_stamps,
        queue.entry_stride,
    )?;
    if queue.submitted >= queue.num_stamps as u32 {
        return Err(SubmissionError::KickQueueSubmittedFull);
    }

    let mut entry = [0u8; G17P_CL_KICK_ENTRY_SIZE];
    let layout = G17P_CL_KICK_ENTRY_LAYOUT;

    let mut header = queue.current_timestamp << 2;
    header |= (queue.queue_id as u64) << 42;
    header |= (queue.completion_selector as u64) << 59;
    if operands.descriptor_flag_4c {
        header |= 1 << 1;
    }
    if let Some(aperture) = operands.mcache {
        header |= 1 << 58;
        put_u64_le(&mut entry, layout.mcache, encode_g17p_cl_mcache(aperture)?);
    }
    if operands.descriptor_flag_5c8 {
        header |= 1 << 61;
    }
    put_u64_le(&mut entry, layout.header, header);

    for (offset, value) in layout.payload.into_iter().zip(operands.payload) {
        put_u64_le(&mut entry, offset, value);
    }

    for (index, barrier) in operands.barriers.iter().copied().enumerate() {
        put_u64_le(
            &mut entry,
            layout.dependencies + index * 8,
            encode_g17p_cl_barrier_dependency(queue, barrier)?,
        );
    }

    let implicit_timestamp = if queue.previous_valid {
        queue.previous_timestamp
    } else {
        operands.converted_command_timestamp.wrapping_sub(1)
    };
    let implicit_index = operands.barriers.len();
    put_u64_le(
        &mut entry,
        layout.dependencies + implicit_index * 8,
        encode_g17p_cl_dependency(implicit_timestamp, queue.queue_id)?,
    );

    let dependency_count = implicit_index + 1;
    let dependency_mask = if dependency_count == G17P_CL_DEPENDENCIES_MAX {
        u32::MAX
    } else {
        (1u32 << dependency_count) - 1
    };
    let dependency_control = dependency_mask as u64
        | ((queue.qos.queue_byte_32 as u64) << 32)
        | ((queue.qos.class_40 as u64) << 40)
        | ((queue.qos.word_48 as u64) << 48);
    put_u64_le(&mut entry, layout.dependency_control, dependency_control);

    for (offset, value) in layout.event_mask.into_iter().zip(operands.event_mask) {
        put_u64_le(&mut entry, offset, value);
    }
    entry[layout.rce_kind] = operands.rce_kind;
    for (offset, binding) in layout.rce_bindings.into_iter().zip(operands.rce_bindings) {
        put_u64_le(&mut entry, offset, encode_g17p_cl_rce(binding)?);
    }
    put_u64_le(&mut entry, layout.auxiliary, operands.auxiliary);

    let mut producer_after = queue;
    producer_after.current_timestamp = prepared.next_timestamp;
    producer_after.submitted = queue.submitted.wrapping_add(1);
    producer_after.previous_valid = true;
    producer_after.previous_timestamp = prepared.timestamp;

    Ok(PreparedG17PClKickEntry {
        zero_length: queue.entry_stride,
        entry,
        entry_index: prepared.entry_index,
        entry_offset: prepared.entry_offset,
        current_timestamp: prepared.timestamp,
        next_timestamp: prepared.next_timestamp,
        submitted_before: queue.submitted,
        submitted_after: producer_after.submitted,
        previous_timestamp_after: prepared.timestamp,
        producer_after,
    })
}

/// Apply the completed-kick count used by `AGXCLCommandDescriptor::complete`.
/// The live callback performs an acquire-release atomic subtraction at queue
/// +0x44. This pure form keeps every other producer field unchanged.
pub(crate) fn complete_g17p_cl_kicks(
    mut producer: G17PClKickProducerState,
    completed_count: u32,
) -> Result<G17PClKickProducerState, SubmissionError> {
    if completed_count == 0 {
        return Ok(producer);
    }
    if producer.submitted == 0 || completed_count > producer.submitted {
        return Err(SubmissionError::KickCompletionUnderflow);
    }

    producer.submitted -= completed_count;
    Ok(producer)
}

/// One range inside provider-owned queue storage.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) struct G17PClStorageRange {
    pub(crate) offset: usize,
    pub(crate) size: usize,
}

/// The scheduler record at +0x200 aliases the registered queue's 16 KiB
/// shared-support page. A second full-page support write would erase it.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) struct G17PClRetainedStorageLayout {
    pub(crate) shared_support: G17PClStorageRange,
    pub(crate) scheduler: G17PClStorageRange,
}

pub(crate) const G17P_CL_RETAINED_STORAGE_LAYOUT: G17PClRetainedStorageLayout =
    G17PClRetainedStorageLayout {
        shared_support: G17PClStorageRange {
            offset: 0,
            size: 0x4000,
        },
        scheduler: G17PClStorageRange {
            offset: 0x200,
            size: 0x100,
        },
    };

/// Required provider-owned storage for the measured pre-B2 state writes.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) struct G17PClB2PrimaryStateLayout {
    pub(crate) object_size: usize,
    pub(crate) computed_write_extent: usize,
    pub(crate) region_1_write_extent: usize,
    pub(crate) region_2_write_extent: usize,
}

pub(crate) const G17P_CL_B2_PRIMARY_STATE_LAYOUT: G17PClB2PrimaryStateLayout =
    G17PClB2PrimaryStateLayout {
        object_size: 0x4000,
        computed_write_extent: 0xe30,
        region_1_write_extent: 0x20,
        region_2_write_extent: 0x18,
    };

pub(crate) const fn g17p_cl_ranges_overlap(
    left: G17PClStorageRange,
    right: G17PClStorageRange,
) -> bool {
    let left_end = match left.offset.checked_add(left.size) {
        Some(end) => end,
        None => return false,
    };
    let right_end = match right.offset.checked_add(right.size) {
        Some(end) => end,
        None => return false,
    };
    left.offset < right_end && right.offset < left_end
}

pub(crate) const fn g17p_cl_range_contains(
    outer: G17PClStorageRange,
    inner: G17PClStorageRange,
) -> bool {
    let outer_end = match outer.offset.checked_add(outer.size) {
        Some(end) => end,
        None => return false,
    };
    let inner_end = match inner.offset.checked_add(inner.size) {
        Some(end) => end,
        None => return false,
    };
    outer.offset <= inner.offset && inner_end <= outer_end
}

/// A whole-page support initializer is valid only before queue registration.
pub(crate) const fn g17p_cl_full_support_initialization_allowed(registered: bool) -> bool {
    !registered
}

/// Initialize the queue-owned pages exactly once, before SKSM registration.
pub(crate) fn apply_g17p_cl_retained_static_state(
    shared_support: &mut [u8],
    support_state: &mut [u8],
    scheduler_state: &mut [u8],
    channel_control: &mut [u8],
    operand_table_gpu_va: u64,
    support_state_gpu_va: u64,
    scheduler_state_gpu_va: u64,
) -> Result<(), SubmissionError> {
    if shared_support.len() < 0x4000
        || support_state.len() < 0x4000
        || scheduler_state.len() < 0x4000
        || channel_control.len() < 0x40
    {
        return Err(SubmissionError::KickRetainedStorageBufferTooSmall);
    }

    shared_support.fill(0);
    support_state.fill(0);
    scheduler_state.fill(0);
    channel_control.fill(0);

    put_u64_le(shared_support, 0x00, 2);
    put_u64_le(shared_support, 0x08, 1);
    put_u64_le(shared_support, 0x10, 2);
    put_u64_le(shared_support, 0x18, 0x0004_0000_0000_0070);
    put_u64_le(shared_support, 0x20, 0x0000_1500_0000_0000);
    put_u64_le(shared_support, 0x28, 0x0000_1500_0000_0000);
    put_u64_le(shared_support, 0x30, operand_table_gpu_va);
    put_u64_le(shared_support, 0x40, 4);
    put_u32_le(shared_support, 0x48, 0xa8);
    put_u64_le(shared_support, 0x4c, support_state_gpu_va);
    put_u32_le(shared_support, 0x54, 0);
    put_u32_le(shared_support, 0x5c, 1);
    put_u32_le(shared_support, 0x60, 2);

    let scheduler = G17P_CL_RETAINED_STORAGE_LAYOUT.scheduler.offset;
    put_u64_le(shared_support, scheduler, scheduler_state_gpu_va + 8);
    put_u32_le(shared_support, scheduler + 0x08, 1);
    put_u32_le(shared_support, scheduler + 0x0c, 0);
    put_u32_le(shared_support, scheduler + 0x10, 0x50);
    put_u32_le(support_state, 0, 1);
    put_u32_le(scheduler_state, 8, 1);

    put_u64_le(channel_control, 0x00, 0x0000_0100_0000_ffff);
    put_u64_le(channel_control, 0x20, 0x0002_0000_0000_0000);
    put_u64_le(channel_control, 0x30, 0x0000_0000_ff00_0000);
    Ok(())
}

/// Apply the exact host-owned pre-B2 values to three provider-owned pages.
/// Bytes outside the listed fields remain unchanged. `channel_control_gpu_va`
/// comes from the retained Linux mapping, not the captured macOS address.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) enum G17PClB2ActivationState {
    Pending,
    Published,
}

pub(crate) fn apply_g17p_cl_b2_primary_state(
    activation: G17PClB2ActivationState,
    computed: &mut [u8],
    region_1: &mut [u8],
    region_2: &mut [u8],
    channel_control_gpu_va: u64,
) -> Result<G17PClB2ActivationState, SubmissionError> {
    if activation == G17PClB2ActivationState::Published {
        return Err(SubmissionError::KickB2PrimaryStateAlreadyPublished);
    }
    let layout = G17P_CL_B2_PRIMARY_STATE_LAYOUT;
    if computed.len() < layout.object_size
        || region_1.len() < layout.object_size
        || region_2.len() < layout.object_size
    {
        return Err(SubmissionError::KickB2PrimaryStateBufferTooSmall);
    }

    computed[0x000] = 0xff;
    computed[0x008] = 0xff;
    computed[0x010..0x012].copy_from_slice(&0x0f01u16.to_le_bytes());
    computed[0x018..0x01a].copy_from_slice(&0x0f01u16.to_le_bytes());
    put_u32_le(computed, 0x408, 2);
    put_u32_le(computed, 0x40c, 2);
    put_u32_le(computed, 0x604, 4);
    put_u64_le(computed, 0x808, channel_control_gpu_va);
    put_u32_le(computed, 0xc04, 0x1000);
    put_u64_le(computed, 0xe00, 7);
    put_u64_le(computed, 0xe08, 0x0000_0001_0e22_618a);
    put_u64_le(computed, 0xe20, 4);
    put_u64_le(computed, 0xe28, 4);

    put_u64_le(region_1, 0x10, 0x0000_0080_0007_7000);
    put_u32_le(region_1, 0x18, 0x17);
    put_u32_le(region_1, 0x1c, 0x17);

    put_u64_le(region_2, 0x00, 0x0800_0000_e000_0000);
    put_u64_le(region_2, 0x08, 0x0000_3d40_0000_3400);
    put_u64_le(region_2, 0x10, 0x0000_0000_0000_1d00);
    Ok(G17PClB2ActivationState::Published)
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) enum G17PClRetainedPublicationStep {
    InitializeSharedSupport,
    WriteSchedulerRecord,
    RegisterPhysicalQueue,
    PublishCommandOwnedState,
    CleanCommandOwnedState,
    WriteB2PrimaryState,
    CleanB2PrimaryState,
    SystemBarrierBeforeInnerPayload,
    PublishInnerQueuePayload,
    SystemBarrierBeforeInnerProducer,
    PublishInnerQueueProducer,
    SystemBarrierAfterInnerProducer,
}

/// One-time order that makes the scheduler alias valid before registration.
pub(crate) const G17P_CL_RETAINED_REGISTRATION_ORDER: [G17PClRetainedPublicationStep; 3] = [
    G17PClRetainedPublicationStep::InitializeSharedSupport,
    G17PClRetainedPublicationStep::WriteSchedulerRecord,
    G17PClRetainedPublicationStep::RegisterPhysicalQueue,
];

/// One-time activation order proven by retained command B2.
pub(crate) const G17P_CL_RETAINED_ACTIVATION_ORDER: [G17PClRetainedPublicationStep; 9] = [
    G17PClRetainedPublicationStep::PublishCommandOwnedState,
    G17PClRetainedPublicationStep::CleanCommandOwnedState,
    G17PClRetainedPublicationStep::WriteB2PrimaryState,
    G17PClRetainedPublicationStep::CleanB2PrimaryState,
    G17PClRetainedPublicationStep::SystemBarrierBeforeInnerPayload,
    G17PClRetainedPublicationStep::PublishInnerQueuePayload,
    G17PClRetainedPublicationStep::SystemBarrierBeforeInnerProducer,
    G17PClRetainedPublicationStep::PublishInnerQueueProducer,
    G17PClRetainedPublicationStep::SystemBarrierAfterInnerProducer,
];

/// Steady-state order proven by command B3 on the same physical queue.
pub(crate) const G17P_CL_RETAINED_SUBMISSION_ORDER: [G17PClRetainedPublicationStep; 6] = [
    G17PClRetainedPublicationStep::PublishCommandOwnedState,
    G17PClRetainedPublicationStep::CleanCommandOwnedState,
    G17PClRetainedPublicationStep::PublishInnerQueuePayload,
    G17PClRetainedPublicationStep::SystemBarrierBeforeInnerProducer,
    G17PClRetainedPublicationStep::PublishInnerQueueProducer,
    G17PClRetainedPublicationStep::SystemBarrierAfterInnerProducer,
];

pub(crate) const fn g17p_cl_retained_submission_order(
    activation: G17PClB2ActivationState,
) -> &'static [G17PClRetainedPublicationStep] {
    match activation {
        G17PClB2ActivationState::Pending => &G17P_CL_RETAINED_ACTIVATION_ORDER,
        G17PClB2ActivationState::Published => &G17P_CL_RETAINED_SUBMISSION_ORDER,
    }
}

// ---------------------------------------------------------------------------
// 6. Host addKicks MMIO publication (AGX3-specific).
//
//    The host does not write the firmware-side `timestamp | (qid << 40)` word
//    directly.  The pinned G17P/G17G/G17X implementations publish a
//    target-specific 64-bit queue tag followed by the zero-extended add count
//    as a second 64-bit store to two
//    runtime-selected scratch offsets. The host adds nine times the runtime
//    stride to both bases for every target. Publication never reads either
//    location and preserves the exact word-0 then word-1 order. The separate
//    host diagnostic path may read word 1; word 0 must never be read.
//    G17P `addKicks` performs `str x20` then `str x19` at
//    0xfffffe0008a57b5c/0xfffffe0008a57b60; the earlier `mov w19,w1`
//    zero-extends the add count.
// ---------------------------------------------------------------------------

/// One ordered transaction for the two SKSM scratch FIFO ports.
///
/// `word0` must be stored first and `word1` second. There is deliberately no
/// read operation in this publication interface.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) struct G17SksmOrderedWritePair {
    pub(crate) word0_offset: u32,
    pub(crate) word0: u64,
    pub(crate) word1_offset: u32,
    pub(crate) word1: u64,
}

/// Minimal backend for an ordered pair of 64-bit FIFO stores.
pub(crate) trait G17SksmPairWriter {
    type Error;

    fn write64(&self, offset: u32, value: u64) -> core::result::Result<(), Self::Error>;
}

/// Publish one pair in the only order accepted by the pinned host sequence.
pub(crate) fn publish_g17p_sksm_write_pair<P: G17SksmPairWriter>(
    port: &P,
    pair: G17SksmOrderedWritePair,
) -> core::result::Result<(), P::Error> {
    port.write64(pair.word0_offset, pair.word0)?;
    port.write64(pair.word1_offset, pair.word1)
}

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
    /// Second store: zero-extended number of newly published kicks.
    pub(crate) count_offset: u32,
    pub(crate) add_count: u32,
}

impl PreparedSksmAddKicksMmio {
    /// Convert the decoded host values to the exact two 64-bit FIFO stores.
    pub(crate) const fn write_pair(self) -> G17SksmOrderedWritePair {
        G17SksmOrderedWritePair {
            word0_offset: self.tag_offset,
            word0: self.tag,
            word1_offset: self.count_offset,
            word1: self.add_count as u64,
        }
    }
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
//    `addKicks` (§6). Linux resource 0 (`sgx`) owns this window. Its two
//    scratch locations receive ordered stores; queue enable is a separate
//    read/modify/write register after the pair. The production publication
//    surface intentionally exposes no scratch read operation.
//
//    `AGXAcceleratorG16::populateKSMCompletionQueue` at
//    0xfffffe0008a53034-0xfffffe0008a532b8 emits four 0x20-byte descriptors.
//    Each present queue has 0x40-byte entries and a fixed ordinal 0..3. The
//    matched firmware consumes those queues. D93AP IRQ 1001 and RTBuddy AKF
//    message type 2 converge on source 4 and drain the event rings that carry
//    completion stamps. The completion path remains pending until an actual
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
/// QID-disable mask base written before changing a registered queue's routing.
pub(crate) const G17P_SKSM_QUEUE_DISABLE_MASK_BASE: u32 = 0x21078;
/// Queue-entry address bits preserved by the host payload encoder.
pub(crate) const G17P_SKSM_QUEUE_ENTRY_ADDRESS_MASK: u64 = 0x0000_07ff_ffff_ffe0;
/// Fixed portion of the first G17P queue-configuration store.
const G17P_SKSM_QUEUE_CONFIG_TAG: u64 = 0x0000_0400_0002_1008;
const G17P_SKSM_QUEUE_COMPLETION_SELECTOR_MAX: u32 = 4;
/// `_AGFIDataMasterType` selected by the G17P TA channel.
pub(crate) const G17P_TA_DATA_MASTER: u32 = 0;
/// `_AGFIDataMasterType` selected by the G17P 3D channel.
pub(crate) const G17P_3D_DATA_MASTER: u32 = 1;
/// `_AGFIDataMasterType` selected by `AGXCommandQueue::processComputeSetup`.
pub(crate) const G17P_COMPUTE_DATA_MASTER: u32 = 2;

/// Runtime routing state that the compute-channel setup owns for one queue.
///
/// The pinned host selects the queue by the CL channel QID, prepares the
/// queue's mapping resource, obtains a completion selector from the channel,
/// and stores `selector | (2 << 32)` at queue `+0x1c`. Keeping the selector
/// and queue address together prevents a caller from supplying a non-compute
/// data-master value to the production compute path.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) struct G17PComputeQueueRouting {
    pub(crate) queue_id: u8,
    pub(crate) completion_selector: u32,
    pub(crate) entry_gpu_va: u64,
}

/// Pure description of the ordered host-side publications that make one A18
/// Pro SKSM queue visible to the hardware bridge.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) struct PreparedG17PSksmQueueConfig {
    /// First store: 64-bit QID tag.
    pub(crate) tag_offset: u32,
    pub(crate) tag: u64,
    /// Second store: 64-bit queue address/data-master/completion selector.
    pub(crate) payload_offset: u32,
    pub(crate) payload: u64,
    /// Final stores: high 64 QIDs first, then low 64 QIDs.
    pub(crate) enable_pair: G17SksmOrderedWritePair,
    /// Stores used before changing an already-enabled queue's routing.
    pub(crate) disable_pair: G17SksmOrderedWritePair,
}

impl PreparedG17PSksmQueueConfig {
    /// Convert the queue configuration to its two ordered write-only stores.
    pub(crate) const fn write_pair(self) -> G17SksmOrderedWritePair {
        G17SksmOrderedWritePair {
            word0_offset: self.tag_offset,
            word0: self.tag,
            word1_offset: self.payload_offset,
            word1: self.payload,
        }
    }
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
    let low_mask = if queue_id < 64 { 1u64 << queue_id } else { 0 };
    let high_mask = if queue_id >= 64 {
        1u64 << (queue_id - 64)
    } else {
        0
    };
    let enable_pair = G17SksmOrderedWritePair {
        word0_offset: G17P_SKSM_QUEUE_ENABLE_MASK_BASE + 8,
        word0: high_mask,
        word1_offset: G17P_SKSM_QUEUE_ENABLE_MASK_BASE,
        word1: low_mask,
    };
    let disable_pair = G17SksmOrderedWritePair {
        word0_offset: G17P_SKSM_QUEUE_DISABLE_MASK_BASE + 8,
        word0: high_mask,
        word1_offset: G17P_SKSM_QUEUE_DISABLE_MASK_BASE,
        word1: low_mask,
    };

    Ok(PreparedG17PSksmQueueConfig {
        tag_offset: offsets.write0,
        tag: G17P_SKSM_QUEUE_CONFIG_TAG | ((queue_id as u64) << 43),
        payload_offset: offsets.write1,
        payload,
        enable_pair,
        disable_pair,
    })
}

/// Prepare the queue-configuration transaction produced by the G17P compute
/// setup path.
///
/// `AGXCommandQueue::processComputeSetup` at
/// `0xfffffe0008a077a4..0xfffffe0008a078d4` selects the queue from the CL
/// channel QID, prepares its mapping resource, obtains the channel completion
/// selector, and writes the packed selector/data-master pair. The captured
/// native CL2 queue uses QID 4, selector 2, and entry VA `0x70004d8000`.
pub(crate) const fn prepare_g17p_compute_sksm_queue_config(
    routing: G17PComputeQueueRouting,
    geometry: G17SksmScratchGeometry,
) -> Result<PreparedG17PSksmQueueConfig, SubmissionError> {
    prepare_g17p_sksm_queue_config(
        G17ChannelAbiTarget::A18ProG17P,
        routing.queue_id,
        G17P_COMPUTE_DATA_MASTER,
        routing.completion_selector,
        routing.entry_gpu_va,
        geometry,
    )
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
//    from command offset zero, and treat tags above 18 as invalid. Their jump tables
//    process 0/1/2/3/4/5/7/9/12/14/15/16/18. Tag 6 reaches the dedicated
//    `PMRebuild must not be received in SKSM` fatal, while 8/10/11/13/17
//    reach the invalid-command fatal. The host nevertheless names 17 `Nop`;
//    that producer identity is preserved, but the admission helper marks it invalid
//    because the matched consumer cannot process it.
//
//    Tags 6/8/10/11 remain semantically unresolved instead of inheriting an
//    AGX2 or nested SKU-stream meaning. SKU-stream opcodes are a distinct
//    nested domain and intentionally have no representation in the top-level
//    enum. M5-family identities remain unsupported pending their own paired
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
/// SKU-stream opcode. `Nop` records the host ABI but is invalid for
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

/// Tags named by the pinned host's top-level ABI, including invalid `Nop`.
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
/// In particular, host-grounded `Nop = 17` is invalid here because the
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
        ) => Err(SubmissionError::CommandTagInvalidForFirmware),
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
    /// offsets were not read first-hand this pass. Kept unsupported.
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
/// generic channel/ring producer records remain inherited and unsupported.
/// A separate G17P-only channel-state entry records the matched A18 Pro host
/// producer and firmware consumer (§2c); it does not admit either M5 family.
/// The queue bridge/completion descriptors and command dispatch are grounded,
/// G17P-only AGX3 forks (§7-§8): the M5
/// identities remain unsupported; unresolved values do not inherit AGX2 or
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
    /// Linux has an ordered pair writer on resource 0, including fixed-slot
    /// geometry and serialization across queue owners.
    pub(crate) sksm_resource0_ordered_write_implemented: bool,
    /// EP 0x21 plus the retained CL2 channel queue has executed real work.
    pub(crate) channel_doorbell_round_trip_proven: bool,
    /// Linux allocates and publishes every object in the measured first-partial
    /// graphics contract through its runtime path.
    pub(crate) partial_graphics_runtime_implemented: bool,
    /// The measured first-partial channel path executed paired graphics work.
    pub(crate) partial_graphics_round_trip_proven: bool,
    /// A native SKSM queue entry followed by the host `addKicks` MMIO pair has
    /// executed real work. The shipping kext uses this path, but the classic
    /// first-partial graphics path is independently executable.
    pub(crate) sksm_add_kicks_round_trip_proven: bool,
    /// Apple's host builds platform interrupt event sources from runtime
    /// `interrupts`/`interrupts-valid` properties. D93AP physical IRQ 1001 and
    /// RTBuddy AKF type 2 both converge exactly on source 4 and drain the main
    /// firmware event rings, but an on-device SKSM completion round trip has
    /// not proven that this ingress is sufficient for Linux completion.
    pub(crate) completion_irq_route_proven: bool,
}

impl SubmissionLiveGaps {
    pub(crate) const fn classic_graphics_proven(&self) -> bool {
        self.channel_doorbell_round_trip_proven
            && self.partial_graphics_runtime_implemented
            && self.partial_graphics_round_trip_proven
            && self.completion_irq_route_proven
    }

    pub(crate) const fn native_sksm_proven(&self) -> bool {
        self.sksm_resource0_ordered_write_implemented
            && self.sksm_add_kicks_round_trip_proven
            && self.completion_irq_route_proven
    }

    /// True when either independently modeled transport can complete work.
    pub(crate) const fn all_proven(&self) -> bool {
        self.classic_graphics_proven() || self.native_sksm_proven()
    }
}

/// The host-side writer is implemented. The channel-doorbell execution result
/// comes from `20260827-g17p-compact-contract-queue-live12`: exact 64-float
/// add3 output, then two more submissions on the same CL2 queue at queue and
/// channel counters 6/6/6 and 9/9/9. That does not prove the separate native
/// SKSM `addKicks` FIFO used by the shipping graphics channels.
pub(crate) const SUBMISSION_LIVE_GAPS: SubmissionLiveGaps = SubmissionLiveGaps {
    sksm_resource0_ordered_write_implemented: true,
    channel_doorbell_round_trip_proven: true,
    partial_graphics_runtime_implemented: false,
    partial_graphics_round_trip_proven: true,
    sksm_add_kicks_round_trip_proven: false,
    completion_irq_route_proven: false,
};

#[cfg(test)]
mod tests {
    use super::*;

    fn get_u64_le(raw: &[u8], offset: usize) -> u64 {
        u64::from_le_bytes(raw[offset..offset + 8].try_into().unwrap())
    }

    fn get_u32_le(raw: &[u8], offset: usize) -> u32 {
        u32::from_le_bytes(raw[offset..offset + 4].try_into().unwrap())
    }

    fn rce_binding_from_packed(word: u64) -> G17PClRceBinding {
        G17PClRceBinding {
            address: (word & G17P_CL_PACKED_ADDRESS_MASK) << 5,
            tag: ((word >> 43) & KICK_QID_MASK as u64) as u8,
        }
    }

    fn native_add3_compute_operands(
        descriptor: u64,
        queue: u64,
        converted_command_timestamp: u64,
        event_mask: [u64; 4],
        rce_words: [u64; 2],
    ) -> G17PClKickEntryOperands<'static> {
        let empty = G17PClRceBinding { address: 0, tag: 0 };
        G17PClKickEntryOperands {
            descriptor_flag_4c: false,
            descriptor_flag_5c8: false,
            converted_command_timestamp,
            barriers: &[],
            mcache: None,
            payload: [descriptor, queue],
            event_mask,
            rce_kind: 0,
            rce_bindings: [
                rce_binding_from_packed(rce_words[0]),
                rce_binding_from_packed(rce_words[1]),
                empty,
                empty,
            ],
            auxiliary: 0x003f_ffff_ffff_ffff,
        }
    }

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
    fn firmware_marks_pipe_three_invalid() {
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
    fn g17p_partial_opening_pages_and_context_match_the_successful_graph() {
        assert_eq!(G17P_PARTIAL_OPENING_PAGE_SIZE, 0x4000);
        assert_eq!(G17P_PARTIAL_OPENING_SCHEDULER_GPU_VA, 0xffff_fc20_015e_0000);
        assert_eq!(
            G17P_PARTIAL_OPENING_PRIMARY_INDEX_GPU_VA,
            0x0000_0010_0019_0000
        );

        let scheduler = build_g17p_partial_opening_scheduler_page();
        assert_eq!(get_u64_le(&scheduler, 0), 0x0000_0020_0001_9000);
        assert!(scheduler[8..].iter().all(|value| *value == 0));
        let mut scheduler_slice = [0xa5; G17P_PARTIAL_OPENING_PAGE_SIZE];
        assert_eq!(
            apply_g17p_partial_opening_scheduler_page(&mut scheduler_slice),
            Ok(())
        );
        assert_eq!(scheduler_slice, scheduler);

        let primary = build_g17p_partial_opening_primary_index_page();
        let expected = [
            0x11, 0x12, 0x13, 0x14, 0x16, 0x17, 0x18, 0x19, 0x1b, 0x1c, 0x1d, 0x1e, 0x20, 0x21,
            0x22, 0x23, 0x25, 0x26, 0x27, 0x28, 0x2a, 0x2b, 0x2c, 0x2d, 0x3c, 0x3d, 0x3e, 0x3f,
            0x41, 0x42, 0x43, 0x44,
        ];
        for (index, value) in expected.into_iter().enumerate() {
            assert_eq!(get_u32_le(&primary, index * 4), value);
        }
        assert!(primary[expected.len() * 4..]
            .iter()
            .all(|value| *value == 0));
        let mut primary_slice = [0xa5; G17P_PARTIAL_OPENING_PAGE_SIZE];
        assert_eq!(
            apply_g17p_partial_opening_primary_index_page(&mut primary_slice),
            Ok(())
        );
        assert_eq!(primary_slice, primary);
        assert_eq!(
            apply_g17p_partial_opening_scheduler_page(&mut []),
            Err(SubmissionError::PartialOpeningPageSize)
        );
        assert_eq!(
            apply_g17p_partial_opening_primary_index_page(&mut [0; 1]),
            Err(SubmissionError::PartialOpeningPageSize)
        );

        assert_eq!(
            G17P_PARTIAL_OPENING_CONTEXT,
            G17PPartialOpeningContextContract {
                firmware_low_root_slot: 1,
                firmware_context_id: 64,
                render_root_slot: 2,
                render_context_id: 1,
                context0_empty_high_root_slot: 0,
                render_empty_high_root_slot: 1,
                transport_pair: 2,
                descriptor_pair: 0,
                graph_context_id: 2,
                optional_identity_context_id: 3,
                descriptor_context_id: 2,
                scheduler_class: 2,
                work_doorbell_channel: 8,
                firmware_low_root_present_at_first_work: false,
                source_firmware_high_root_table_resident: false,
            }
        );
    }

    #[test]
    fn g17p_partial_opening_status_is_exact_and_checked() {
        const KERNEL_VA_BASE: u64 = 0xffff_fc20_0000_0000;
        assert_eq!(
            prepare_g17p_partial_opening_pre_0x84_status(KERNEL_VA_BASE),
            Ok([
                G17PPartialOpeningStatusWrite {
                    offset: 0x4018,
                    value: 0x0005_0601_0000_0000,
                },
                G17PPartialOpeningStatusWrite {
                    offset: 0x4020,
                    value: 61,
                },
                G17PPartialOpeningStatusWrite {
                    offset: 0x40b0,
                    value: 5,
                },
                G17PPartialOpeningStatusWrite {
                    offset: 0x48e0,
                    value: KERNEL_VA_BASE + 0x001c_0000,
                },
                G17PPartialOpeningStatusWrite {
                    offset: 0x48e8,
                    value: KERNEL_VA_BASE + 0x001c_0040,
                },
            ])
        );
        assert_eq!(
            prepare_g17p_partial_opening_pre_0x84_status(u64::MAX),
            Err(SubmissionError::PartialOpeningAddressOverflow)
        );
    }

    #[test]
    fn g17p_partial_opening_publishes_3d_then_ta_with_one_pair_kick() {
        assert_eq!(
            G17P_PARTIAL_OPENING_PUBLICATION_ORDER,
            [
                G17PPartialOpeningPublicationStep::PrimarySchedulerPage,
                G17PPartialOpeningPublicationStep::PrimaryIndexPage,
                G17PPartialOpeningPublicationStep::PreControlStatus,
                G17PPartialOpeningPublicationStep::ControlDone0x84,
                G17PPartialOpeningPublicationStep::FragmentState,
                G17PPartialOpeningPublicationStep::FragmentProducer,
                G17PPartialOpeningPublicationStep::LateTilingState,
                G17PPartialOpeningPublicationStep::SystemBarrierBeforeTilingProducer,
                G17PPartialOpeningPublicationStep::TilingProducer,
                G17PPartialOpeningPublicationStep::SystemBarrierBeforePairedDoorbell,
                G17PPartialOpeningPublicationStep::PairedWorkDoorbell0x83,
            ]
        );
        let prepared = prepare_g17p_partial_opening_work_pair(
            G17PPartialOpeningOuterChannelState {
                consumers: [0, 0],
                producer: 0,
                queue_gpu_va: 0xffff_fc20_c000_00c0,
                queue_write_index: 3,
            },
            G17PPartialOpeningOuterChannelState {
                consumers: [0, 0],
                producer: 0,
                queue_gpu_va: 0xffff_fc20_c000_0000,
                queue_write_index: 3,
            },
        )
        .unwrap();
        assert_eq!(
            prepared.fragment,
            G17PPartialOpeningOuterSlotPlan {
                channel_table_index: 7,
                slot_index: 0,
                slot_byte_offset: 0,
                slot: [
                    0, 0, 0, 0, 0, 0, 0, 0, 0xc0, 0x00, 0x00, 0xc0, 0x20, 0xfc, 0xff, 0xff, 1, 0,
                    0, 0, 3, 0, 1, 1,
                ],
                next_producer: 1,
            }
        );
        assert_eq!(
            prepared.tiling,
            G17PPartialOpeningOuterSlotPlan {
                channel_table_index: 6,
                slot_index: 0,
                slot_byte_offset: 0,
                slot: [
                    0, 0, 0, 0, 0, 0, 0, 0, 0x00, 0x00, 0x00, 0xc0, 0x20, 0xfc, 0xff, 0xff, 0, 0,
                    0, 0, 3, 0, 0, 1,
                ],
                next_producer: 1,
            }
        );
        assert_eq!(G17P_PARTIAL_OPENING_OUTER_SLOT_SIZE, 0x18);
        assert_eq!(G17P_PARTIAL_OPENING_OUTER_SLOT_COUNT, 0x100);
        assert_eq!(G17P_PARTIAL_OPENING_TILING_CHANNEL_TABLE_INDEX, 6);
        assert_eq!(G17P_PARTIAL_OPENING_FRAGMENT_CHANNEL_TABLE_INDEX, 7);
        assert_eq!(G17P_PARTIAL_OPENING_WORK_DOORBELL_CHANNEL, 8);
        assert_eq!(prepared.paired_doorbell, 0x0083_0000_0000_0008);
        assert_eq!(
            prepared.paired_doorbell,
            encode_tx_doorbell(PipeType::Vertex, 2).unwrap()
        );

        let full = G17PPartialOpeningOuterChannelState {
            consumers: [0, 7],
            producer: 6,
            queue_gpu_va: 0,
            queue_write_index: 3,
        };
        assert_eq!(
            prepare_g17p_partial_opening_work_pair(full, full),
            Err(SubmissionError::RingFull)
        );
        assert_eq!(
            prepare_g17p_partial_opening_work_pair(
                G17PPartialOpeningOuterChannelState {
                    producer: 0x100,
                    ..full
                },
                full,
            ),
            Err(SubmissionError::PartialOpeningChannelCounterOutOfRange)
        );
        assert_eq!(
            prepare_g17p_partial_opening_work_pair(
                G17PPartialOpeningOuterChannelState {
                    consumers: [0, 0],
                    producer: 0,
                    queue_write_index: 0x1_0000,
                    ..full
                },
                full,
            ),
            Err(SubmissionError::PartialOpeningQueueHeadOutOfRange)
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
    fn kick_queue_preparation_reports_unrepresentable_state_before_publication() {
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
    fn g17p_cl_kick_layout_and_publication_order_are_exact() {
        assert_eq!(G17P_CL_KICK_ENTRY_SIZE, 0x180);
        assert_eq!(G17P_CL_DEPENDENCIES_MAX, 32);
        assert_eq!(G17P_CL_EXPLICIT_DEPENDENCIES_MAX, 31);
        assert_eq!(
            G17P_CL_KICK_QUEUE_STATE_LAYOUT,
            G17PClKickQueueStateLayout {
                queue_id: 0x0c,
                num_stamps: 0x10,
                entry_stride: 0x14,
                completion_selector: 0x1c,
                submitted: 0x44,
                current_timestamp: 0x48,
                entry_cpu_base: 0x78,
                entry_gpu_base: 0x80,
                base_timestamp: 0x90,
                previous_valid: 0x98,
                previous_timestamp: 0xa0,
            }
        );
        assert_eq!(
            G17P_CL_KICK_ENTRY_LAYOUT,
            G17PClKickEntryLayout {
                header: 0x00,
                mcache: 0x08,
                payload: [0x10, 0x18],
                dependency_control: 0x20,
                dependencies: 0x28,
                event_mask: [0x128, 0x130, 0x138, 0x140],
                rce_kind: 0x148,
                rce_bindings: [0x150, 0x158, 0x160, 0x168],
                zero_170: 0x170,
                auxiliary: 0x178,
            }
        );
        assert_eq!(
            G17P_CL_KICK_PUBLICATION_ORDER,
            [
                G17PClKickPublicationStep::ZeroEntryStride,
                G17PClKickPublicationStep::EntryBody,
                G17PClKickPublicationStep::InnerShareableBarrier,
                G17PClKickPublicationStep::NextTimestamp,
                G17PClKickPublicationStep::SubmittedAtomicAdd,
                G17PClKickPublicationStep::PreviousValid,
                G17PClKickPublicationStep::PreviousTimestamp,
            ]
        );
    }

    #[test]
    fn g17p_cl_hardware_positive_retained_items_are_exact() {
        const QUEUE: u64 = 0xffff_fc20_c000_0300;
        const DESCRIPTOR: u64 = 0xffff_fc20_c035_8000;
        const DESCRIPTOR_STRIDE: u64 = 0x1040;
        let item0_queue = G17PClKickProducerState {
            queue_id: 4,
            num_stamps: 256,
            entry_stride: 0x200,
            completion_selector: 2,
            qos: G17PClQosConfig {
                queue_byte_32: 1,
                class_40: 8,
                word_48: 0xffff,
            },
            submitted: 0,
            current_timestamp: 1,
            base_timestamp: 1,
            previous_valid: false,
            previous_timestamp: 0,
        };
        let item0 = prepare_g17p_cl_kick_entry(
            item0_queue,
            native_add3_compute_operands(
                DESCRIPTOR,
                QUEUE,
                1,
                [0, 0, 4, 0],
                [0x0001_1003_8001_a002, 0x0000_2003_8001_a03b],
            ),
        )
        .unwrap();
        let mut expected0 = [0u8; G17P_CL_KICK_ENTRY_SIZE];
        for (offset, value) in [
            (0x00, 0x1000_1000_0000_0004),
            (0x10, DESCRIPTOR),
            (0x18, QUEUE),
            (0x20, 0xffff_0801_0000_0001),
            (0x28, 0x0000_0400_0000_0000),
            (0x138, 4),
            (0x150, 0x0001_1003_8001_a002),
            (0x158, 0x0000_2003_8001_a03b),
            (0x178, 0x003f_ffff_ffff_ffff),
        ] {
            put_u64_le(&mut expected0, offset, value);
        }
        assert_eq!(item0.entry, expected0);
        assert_eq!(item0.zero_length, 0x200);
        assert_eq!(item0.entry_index, 1);
        assert_eq!(item0.entry_offset, 0x200);
        assert_eq!(item0.current_timestamp, 1);
        assert_eq!(item0.next_timestamp, 2);
        assert_eq!(item0.submitted_before, 0);
        assert_eq!(item0.submitted_after, 1);
        assert_eq!(item0.previous_timestamp_after, 1);
        assert_eq!(item0.producer_after.current_timestamp, 2);
        assert_eq!(item0.producer_after.submitted, 1);
        assert!(item0.producer_after.previous_valid);
        assert_eq!(item0.producer_after.previous_timestamp, 1);
        assert_eq!(item0.producer_after.qos, item0_queue.qos);

        let item1 = prepare_g17p_cl_kick_entry(
            item0.producer_after,
            native_add3_compute_operands(
                DESCRIPTOR + DESCRIPTOR_STRIDE,
                QUEUE,
                0,
                [0, 2, 8, 0],
                [0x0001_1003_8001_a084, 0x0000_2003_8001_a0bd],
            ),
        )
        .unwrap();
        let mut expected1 = [0u8; G17P_CL_KICK_ENTRY_SIZE];
        for (offset, value) in [
            (0x00, 0x1000_1000_0000_0008),
            (0x10, DESCRIPTOR + DESCRIPTOR_STRIDE),
            (0x18, QUEUE),
            (0x20, 0xffff_0801_0000_0001),
            (0x28, 0x0000_0400_0000_0001),
            (0x130, 2),
            (0x138, 8),
            (0x150, 0x0001_1003_8001_a084),
            (0x158, 0x0000_2003_8001_a0bd),
            (0x178, 0x003f_ffff_ffff_ffff),
        ] {
            put_u64_le(&mut expected1, offset, value);
        }
        assert_eq!(item1.entry, expected1);
        assert_eq!(item1.zero_length, 0x200);
        assert_eq!(item1.entry_index, 2);
        assert_eq!(item1.entry_offset, 0x400);
        assert_eq!(item1.current_timestamp, 2);
        assert_eq!(item1.next_timestamp, 3);
        assert_eq!(item1.submitted_before, 1);
        assert_eq!(item1.submitted_after, 2);
        assert_eq!(item1.previous_timestamp_after, 2);
        assert_eq!(item1.producer_after.current_timestamp, 3);
        assert_eq!(item1.producer_after.submitted, 2);
        assert_eq!(item1.producer_after.previous_timestamp, 2);
        assert_eq!(item1.producer_after.qos, item0_queue.qos);

        // Command two keeps physical queue 0's QoS word while advancing its
        // command-owned event mask from 4 to 8.
        assert_eq!(get_u64_le(&item1.entry, 0x00), 0x1000_1000_0000_0008);
        assert_eq!(get_u64_le(&item1.entry, 0x28), 0x0000_0400_0000_0001);
        assert_eq!(
            get_u64_le(&item1.entry, 0x20),
            get_u64_le(&item0.entry, 0x20)
        );
        assert_eq!(get_u64_le(&item0.entry, 0x138), 4);
        assert_eq!(get_u64_le(&item1.entry, 0x138), 8);
        assert_eq!(
            get_u64_le(&item1.entry, 0x178),
            get_u64_le(&item0.entry, 0x178)
        );
    }

    #[test]
    fn g17p_cl_descriptor_flags_map_directly_to_header_bits() {
        let empty = G17PClRceBinding { address: 0, tag: 0 };
        let producer = G17PClKickProducerState {
            queue_id: 4,
            num_stamps: 16,
            entry_stride: 0x200,
            completion_selector: 2,
            qos: G17PClQosConfig {
                queue_byte_32: 1,
                class_40: 8,
                word_48: 0xffff,
            },
            submitted: 0,
            current_timestamp: 1,
            base_timestamp: 1,
            previous_valid: false,
            previous_timestamp: 0,
        };
        let flag_mask = (1u64 << 1) | (1u64 << 61);

        for (flag_4c, flag_5c8, expected) in [
            (false, false, 0),
            (true, false, 1u64 << 1),
            (false, true, 1u64 << 61),
            (true, true, flag_mask),
        ] {
            let prepared = prepare_g17p_cl_kick_entry(
                producer,
                G17PClKickEntryOperands {
                    descriptor_flag_4c: flag_4c,
                    descriptor_flag_5c8: flag_5c8,
                    converted_command_timestamp: 1,
                    barriers: &[],
                    mcache: None,
                    payload: [0; 2],
                    event_mask: [0; 4],
                    rce_kind: 0,
                    rce_bindings: [empty; 4],
                    auxiliary: 0,
                },
            )
            .unwrap();

            assert_eq!(get_u64_le(&prepared.entry, 0) & flag_mask, expected);
        }
    }

    #[test]
    fn g17p_cl_completion_updates_only_the_submitted_count() {
        let producer = G17PClKickProducerState {
            queue_id: 4,
            num_stamps: 256,
            entry_stride: 0x200,
            completion_selector: 2,
            qos: G17PClQosConfig {
                queue_byte_32: 1,
                class_40: 8,
                word_48: 0xffff,
            },
            submitted: 2,
            current_timestamp: 3,
            base_timestamp: 1,
            previous_valid: true,
            previous_timestamp: 2,
        };

        assert_eq!(complete_g17p_cl_kicks(producer, 0), Ok(producer));
        let after_one = complete_g17p_cl_kicks(producer, 1).unwrap();
        assert_eq!(
            after_one,
            G17PClKickProducerState {
                submitted: 1,
                ..producer
            }
        );
        let empty = complete_g17p_cl_kicks(producer, 2).unwrap();
        assert_eq!(empty.submitted, 0);
        assert_eq!(complete_g17p_cl_kicks(empty, 0), Ok(empty));
        assert_eq!(
            complete_g17p_cl_kicks(empty, 1),
            Err(SubmissionError::KickCompletionUnderflow)
        );
        assert_eq!(
            complete_g17p_cl_kicks(producer, 3),
            Err(SubmissionError::KickCompletionUnderflow)
        );
    }

    #[test]
    fn g17p_cl_retained_storage_overlap_and_order_are_exact() {
        let layout = G17P_CL_RETAINED_STORAGE_LAYOUT;
        assert_eq!(
            layout,
            G17PClRetainedStorageLayout {
                shared_support: G17PClStorageRange {
                    offset: 0,
                    size: 0x4000,
                },
                scheduler: G17PClStorageRange {
                    offset: 0x200,
                    size: 0x100,
                },
            }
        );
        assert_eq!(
            layout.scheduler.offset - layout.shared_support.offset,
            0x200
        );
        assert!(g17p_cl_range_contains(
            layout.shared_support,
            layout.scheduler
        ));
        assert!(g17p_cl_ranges_overlap(
            layout.shared_support,
            layout.scheduler
        ));
        assert!(g17p_cl_full_support_initialization_allowed(false));
        assert!(!g17p_cl_full_support_initialization_allowed(true));

        assert_eq!(
            G17P_CL_RETAINED_REGISTRATION_ORDER,
            [
                G17PClRetainedPublicationStep::InitializeSharedSupport,
                G17PClRetainedPublicationStep::WriteSchedulerRecord,
                G17PClRetainedPublicationStep::RegisterPhysicalQueue,
            ]
        );
        assert_eq!(
            G17P_CL_RETAINED_ACTIVATION_ORDER,
            [
                G17PClRetainedPublicationStep::PublishCommandOwnedState,
                G17PClRetainedPublicationStep::CleanCommandOwnedState,
                G17PClRetainedPublicationStep::WriteB2PrimaryState,
                G17PClRetainedPublicationStep::CleanB2PrimaryState,
                G17PClRetainedPublicationStep::SystemBarrierBeforeInnerPayload,
                G17PClRetainedPublicationStep::PublishInnerQueuePayload,
                G17PClRetainedPublicationStep::SystemBarrierBeforeInnerProducer,
                G17PClRetainedPublicationStep::PublishInnerQueueProducer,
                G17PClRetainedPublicationStep::SystemBarrierAfterInnerProducer,
            ]
        );
        assert_eq!(
            G17P_CL_RETAINED_SUBMISSION_ORDER,
            [
                G17PClRetainedPublicationStep::PublishCommandOwnedState,
                G17PClRetainedPublicationStep::CleanCommandOwnedState,
                G17PClRetainedPublicationStep::PublishInnerQueuePayload,
                G17PClRetainedPublicationStep::SystemBarrierBeforeInnerProducer,
                G17PClRetainedPublicationStep::PublishInnerQueueProducer,
                G17PClRetainedPublicationStep::SystemBarrierAfterInnerProducer,
            ]
        );
        assert_eq!(
            g17p_cl_retained_submission_order(G17PClB2ActivationState::Pending),
            G17P_CL_RETAINED_ACTIVATION_ORDER
        );
        assert_eq!(
            g17p_cl_retained_submission_order(G17PClB2ActivationState::Published),
            G17P_CL_RETAINED_SUBMISSION_ORDER
        );
    }

    #[test]
    fn g17p_cl_b2_primary_state_builder_is_byte_exact() {
        const FILL: u8 = 0xa5;
        const CHANNEL_CONTROL: u64 = 0xffff_fc20_1234_8000;
        let layout = G17P_CL_B2_PRIMARY_STATE_LAYOUT;
        assert_eq!(
            layout,
            G17PClB2PrimaryStateLayout {
                object_size: 0x4000,
                computed_write_extent: 0xe30,
                region_1_write_extent: 0x20,
                region_2_write_extent: 0x18,
            }
        );

        let mut computed = [FILL; 0x4000];
        let mut region_1 = [FILL; 0x4000];
        let mut region_2 = [FILL; 0x4000];
        let activation = apply_g17p_cl_b2_primary_state(
            G17PClB2ActivationState::Pending,
            &mut computed,
            &mut region_1,
            &mut region_2,
            CHANNEL_CONTROL,
        )
        .unwrap();
        assert_eq!(activation, G17PClB2ActivationState::Published);

        let mut expected_computed = [FILL; 0x4000];
        expected_computed[0x000] = 0xff;
        expected_computed[0x008] = 0xff;
        expected_computed[0x010..0x012].copy_from_slice(&0x0f01u16.to_le_bytes());
        expected_computed[0x018..0x01a].copy_from_slice(&0x0f01u16.to_le_bytes());
        for (offset, value) in [(0x408, 2), (0x40c, 2), (0x604, 4), (0xc04, 0x1000)] {
            put_u32_le(&mut expected_computed, offset, value);
        }
        for (offset, value) in [
            (0x808, CHANNEL_CONTROL),
            (0xe00, 7),
            (0xe08, 0x0000_0001_0e22_618a),
            (0xe20, 4),
            (0xe28, 4),
        ] {
            put_u64_le(&mut expected_computed, offset, value);
        }

        let mut expected_region_1 = [FILL; 0x4000];
        put_u64_le(&mut expected_region_1, 0x10, 0x0000_0080_0007_7000);
        put_u32_le(&mut expected_region_1, 0x18, 0x17);
        put_u32_le(&mut expected_region_1, 0x1c, 0x17);

        let mut expected_region_2 = [FILL; 0x4000];
        put_u64_le(&mut expected_region_2, 0x00, 0x0800_0000_e000_0000);
        put_u64_le(&mut expected_region_2, 0x08, 0x0000_3d40_0000_3400);
        put_u64_le(&mut expected_region_2, 0x10, 0x0000_0000_0000_1d00);

        assert_eq!(computed, expected_computed);
        assert_eq!(region_1, expected_region_1);
        assert_eq!(region_2, expected_region_2);
        assert_eq!(
            apply_g17p_cl_b2_primary_state(
                activation,
                &mut computed,
                &mut region_1,
                &mut region_2,
                CHANNEL_CONTROL,
            ),
            Err(SubmissionError::KickB2PrimaryStateAlreadyPublished)
        );
        assert_eq!(computed, expected_computed);
        assert_eq!(region_1, expected_region_1);
        assert_eq!(region_2, expected_region_2);

        let mut short = [0u8; 1];
        assert_eq!(
            apply_g17p_cl_b2_primary_state(
                G17PClB2ActivationState::Pending,
                &mut short,
                &mut region_1,
                &mut region_2,
                CHANNEL_CONTROL,
            ),
            Err(SubmissionError::KickB2PrimaryStateBufferTooSmall)
        );
    }

    #[test]
    fn g17p_cl_entry_encodes_every_optional_section() {
        let barriers = [G17PClBarrierDependency {
            event_word: 0x100,
            shared_stamp: 0,
            queue_id: 3,
        }];
        let rce_bindings = [
            G17PClRceBinding {
                address: 0x2000,
                tag: 1,
            },
            G17PClRceBinding {
                address: 0x4000,
                tag: 2,
            },
            G17PClRceBinding {
                address: 0x6000,
                tag: 3,
            },
            G17PClRceBinding {
                address: 0x8000,
                tag: 4,
            },
        ];
        let queue = G17PClKickProducerState {
            queue_id: 4,
            num_stamps: 16,
            entry_stride: 0x200,
            completion_selector: 2,
            qos: G17PClQosConfig {
                queue_byte_32: 0x12,
                class_40: 0x1b,
                word_48: 0x3456,
            },
            submitted: 2,
            current_timestamp: 2,
            base_timestamp: 1,
            previous_valid: true,
            previous_timestamp: 7,
        };
        let prepared = prepare_g17p_cl_kick_entry(
            queue,
            G17PClKickEntryOperands {
                descriptor_flag_4c: true,
                descriptor_flag_5c8: true,
                converted_command_timestamp: 0,
                barriers: &barriers,
                mcache: Some(G17PClMcacheAperture {
                    address: 0x12340,
                    index: 5,
                    count: 3,
                }),
                payload: [0x1111_2222_3333_4444, 0x5555_6666_7777_8888],
                event_mask: [0x10, 0x20, 0x30, 0x40],
                rce_kind: 2,
                rce_bindings,
                auxiliary: 0xa5a5_5a5a_1234_5678,
            },
        )
        .unwrap();
        let header =
            (2u64 << 2) | (4u64 << 42) | (1u64 << 58) | (2u64 << 59) | (1u64 << 1) | (1u64 << 61);
        assert_eq!(get_u64_le(&prepared.entry, 0x00), header);
        assert_eq!(
            get_u64_le(&prepared.entry, 0x08),
            5 | ((0x12340u64 >> 5) << 6) | (2u64 << 54)
        );
        assert_eq!(get_u64_le(&prepared.entry, 0x10), 0x1111_2222_3333_4444);
        assert_eq!(get_u64_le(&prepared.entry, 0x18), 0x5555_6666_7777_8888);
        assert_eq!(get_u64_le(&prepared.entry, 0x20), 0x3456_1b12_0000_0003);
        assert_eq!(get_u64_le(&prepared.entry, 0x28), (3u64 << 40) | 1);
        assert_eq!(get_u64_le(&prepared.entry, 0x30), (4u64 << 40) | 7);
        assert_eq!(
            [
                get_u64_le(&prepared.entry, 0x128),
                get_u64_le(&prepared.entry, 0x130),
                get_u64_le(&prepared.entry, 0x138),
                get_u64_le(&prepared.entry, 0x140),
            ],
            [0x10, 0x20, 0x30, 0x40]
        );
        assert_eq!(prepared.entry[0x148], 2);
        for (index, binding) in rce_bindings.into_iter().enumerate() {
            assert_eq!(
                get_u64_le(&prepared.entry, 0x150 + index * 8),
                (binding.address >> 5) | ((binding.tag as u64) << 43)
            );
        }
        assert_eq!(get_u64_le(&prepared.entry, 0x170), 0);
        assert_eq!(get_u64_le(&prepared.entry, 0x178), 0xa5a5_5a5a_1234_5678);
        assert_eq!(prepared.submitted_after, 3);
        assert_eq!(prepared.previous_timestamp_after, 2);
    }

    #[test]
    fn g17p_cl_entry_reports_invalid_geometry_and_fields() {
        let empty = G17PClRceBinding { address: 0, tag: 0 };
        let queue = G17PClKickProducerState {
            queue_id: 0,
            num_stamps: 16,
            entry_stride: 0x200,
            completion_selector: 0,
            qos: G17PClQosConfig {
                queue_byte_32: 0,
                class_40: 0,
                word_48: 0,
            },
            submitted: 0,
            current_timestamp: 1,
            base_timestamp: 1,
            previous_valid: false,
            previous_timestamp: 0,
        };
        let operands = G17PClKickEntryOperands {
            descriptor_flag_4c: true,
            descriptor_flag_5c8: true,
            converted_command_timestamp: 1,
            barriers: &[],
            mcache: None,
            payload: [0; 2],
            event_mask: [0; 4],
            rce_kind: 0,
            rce_bindings: [empty; 4],
            auxiliary: 0,
        };

        assert_eq!(
            prepare_g17p_cl_kick_entry(
                G17PClKickProducerState {
                    entry_stride: 0,
                    ..queue
                },
                operands,
            ),
            Err(SubmissionError::KickEntryStrideZero)
        );
        assert_eq!(
            prepare_g17p_cl_kick_entry(
                G17PClKickProducerState {
                    entry_stride: 0x17f,
                    ..queue
                },
                operands,
            ),
            Err(SubmissionError::KickEntryStrideTooSmall)
        );
        assert_eq!(
            prepare_g17p_cl_kick_entry(
                G17PClKickProducerState {
                    num_stamps: 0,
                    ..queue
                },
                operands,
            ),
            Err(SubmissionError::KickStampCountOutOfRange)
        );
        assert_eq!(
            prepare_g17p_cl_kick_entry(
                G17PClKickProducerState {
                    submitted: 16,
                    ..queue
                },
                operands,
            ),
            Err(SubmissionError::KickQueueSubmittedFull)
        );
        assert_eq!(
            prepare_g17p_cl_kick_entry(
                G17PClKickProducerState {
                    qos: G17PClQosConfig {
                        class_40: 0x20,
                        ..queue.qos
                    },
                    ..queue
                },
                operands,
            ),
            Err(SubmissionError::KickQosClassOutOfRange)
        );
        assert_eq!(
            prepare_g17p_cl_kick_entry(
                queue,
                G17PClKickEntryOperands {
                    mcache: Some(G17PClMcacheAperture {
                        address: 0x20,
                        index: 0x40,
                        count: 1,
                    }),
                    ..operands
                },
            ),
            Err(SubmissionError::KickMcacheFieldOutOfRange)
        );
        assert_eq!(
            prepare_g17p_cl_kick_entry(
                queue,
                G17PClKickEntryOperands {
                    rce_bindings: [G17PClRceBinding { address: 1, tag: 0 }, empty, empty, empty,],
                    ..operands
                },
            ),
            Err(SubmissionError::KickRceAddressUnencodable)
        );

        let too_many = [G17PClBarrierDependency {
            event_word: 0,
            shared_stamp: 0,
            queue_id: 0,
        }; G17P_CL_DEPENDENCIES_MAX];
        assert_eq!(
            prepare_g17p_cl_kick_entry(
                queue,
                G17PClKickEntryOperands {
                    barriers: &too_many,
                    ..operands
                },
            ),
            Err(SubmissionError::KickDependencyCountOutOfRange)
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
            assert_eq!(
                p.write_pair(),
                G17SksmOrderedWritePair {
                    word0_offset: 0x11b,
                    word0: 0x0000_0400_0002_1018 | ((queue_id as u64) << 43),
                    word1_offset: 0x11f,
                    word1: 5,
                }
            );

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
    fn pair_writer_publishes_two_64_bit_words_in_order() {
        use std::cell::RefCell;

        struct Recorder(RefCell<Vec<(u32, u64)>>);

        impl G17SksmPairWriter for Recorder {
            type Error = ();

            fn write64(&self, offset: u32, value: u64) -> core::result::Result<(), Self::Error> {
                self.0.borrow_mut().push((offset, value));
                Ok(())
            }
        }

        let pair = prepare_sksm_add_kicks_mmio(
            SksmAddKicksTarget::G17P,
            0x2a,
            0x89ab_cdef,
            g17_manager::T8140_G17P_PLATFORM_CONFIG.sksm_scratch,
        )
        .unwrap()
        .write_pair();
        let recorder = Recorder(RefCell::new(Vec::new()));

        assert_eq!(publish_g17p_sksm_write_pair(&recorder, pair), Ok(()));
        assert_eq!(
            recorder.0.into_inner(),
            vec![
                (0x4090, 0x0001_5400_0002_1018),
                (0x4098, 0x0000_0000_89ab_cdef),
            ]
        );
    }

    #[test]
    fn pair_writer_does_not_publish_word1_after_word0_error() {
        use std::cell::Cell;

        struct FirstWriteError(Cell<u32>);

        impl G17SksmPairWriter for FirstWriteError {
            type Error = u32;

            fn write64(&self, _offset: u32, _value: u64) -> core::result::Result<(), Self::Error> {
                let calls = self.0.get() + 1;
                self.0.set(calls);
                Err(calls)
            }
        }

        let writer = FirstWriteError(Cell::new(0));
        let pair = G17SksmOrderedWritePair {
            word0_offset: 0x4090,
            word0: 1,
            word1_offset: 0x4098,
            word1: 2,
        };
        assert_eq!(publish_g17p_sksm_write_pair(&writer, pair), Err(1));
        assert_eq!(writer.0.get(), 1);
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
        assert_eq!(
            config.write_pair(),
            G17SksmOrderedWritePair {
                word0_offset: 0x4090,
                word0: 0x0002_0400_0002_1008,
                word1_offset: 0x4098,
                word1: 0x2280_0000_0001_2340,
            }
        );
        assert_eq!(
            config.enable_pair,
            G17SksmOrderedWritePair {
                word0_offset: 0x21070,
                word0: 1,
                word1_offset: 0x21068,
                word1: 0,
            }
        );
        assert_eq!(
            config.disable_pair,
            G17SksmOrderedWritePair {
                word0_offset: 0x21080,
                word0: 1,
                word1_offset: 0x21078,
                word1: 0,
            }
        );
    }

    #[test]
    fn g17p_compute_queue_matches_the_captured_native_cl2_tuple() {
        assert_eq!(G17P_TA_DATA_MASTER, 0);
        assert_eq!(G17P_3D_DATA_MASTER, 1);
        assert_eq!(G17P_COMPUTE_DATA_MASTER, 2);

        let config = prepare_g17p_compute_sksm_queue_config(
            G17PComputeQueueRouting {
                queue_id: 4,
                completion_selector: 2,
                entry_gpu_va: 0x0000_0070_004d_8000,
            },
            g17_manager::T8140_G17P_PLATFORM_CONFIG.sksm_scratch,
        )
        .unwrap();

        assert_eq!(config.tag_offset, 0x4090);
        assert_eq!(config.tag, 0x0000_2400_0002_1008);
        assert_eq!(config.payload_offset, 0x4098);
        assert_eq!(config.payload, 0x3100_0070_004d_8000);
        assert_eq!(
            config.enable_pair,
            G17SksmOrderedWritePair {
                word0_offset: 0x21070,
                word0: 0,
                word1_offset: 0x21068,
                word1: 1 << 4,
            }
        );
        assert_eq!(
            config.disable_pair,
            G17SksmOrderedWritePair {
                word0_offset: 0x21080,
                word0: 0,
                word1_offset: 0x21078,
                word1: 1 << 4,
            }
        );
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
    fn g17p_completion_descriptors_report_invalid_inputs() {
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
    fn g17p_top_level_command_decode_reports_bad_tag_and_identity() {
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
            Err(SubmissionError::CommandTagInvalidForFirmware)
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
        // The channel/ring state records stay inherited and unsupported. The
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
    fn transport_tracks_classic_graphics_and_native_sksm_independently() {
        assert!(GROUNDED_SUBMISSION_PRODUCER_AVAILABLE);
        assert!(!EXECUTABLE_SUBMISSION_TRANSPORT_AVAILABLE);
        assert!(!SUBMISSION_LIVE_GAPS.all_proven());
        assert!(!SUBMISSION_LIVE_GAPS.classic_graphics_proven());
        assert!(!SUBMISSION_LIVE_GAPS.native_sksm_proven());
        assert!(SUBMISSION_LIVE_GAPS.sksm_resource0_ordered_write_implemented);
        assert!(SUBMISSION_LIVE_GAPS.channel_doorbell_round_trip_proven);
        assert!(!SUBMISSION_LIVE_GAPS.partial_graphics_runtime_implemented);
        assert!(SUBMISSION_LIVE_GAPS.partial_graphics_round_trip_proven);
        assert!(!SUBMISSION_LIVE_GAPS.sksm_add_kicks_round_trip_proven);
        assert!(!SUBMISSION_LIVE_GAPS.completion_irq_route_proven);

        let classic = SubmissionLiveGaps {
            sksm_resource0_ordered_write_implemented: true,
            channel_doorbell_round_trip_proven: true,
            partial_graphics_runtime_implemented: true,
            partial_graphics_round_trip_proven: true,
            sksm_add_kicks_round_trip_proven: false,
            completion_irq_route_proven: true,
        };
        assert!(classic.classic_graphics_proven());
        assert!(!classic.native_sksm_proven());
        assert!(classic.all_proven());
        assert!(!classic.sksm_add_kicks_round_trip_proven);

        let native = SubmissionLiveGaps {
            sksm_resource0_ordered_write_implemented: true,
            channel_doorbell_round_trip_proven: false,
            partial_graphics_runtime_implemented: false,
            partial_graphics_round_trip_proven: false,
            sksm_add_kicks_round_trip_proven: true,
            completion_irq_route_proven: true,
        };
        assert!(!native.classic_graphics_proven());
        assert!(native.native_sksm_proven());
        assert!(native.all_proven());
    }
}
