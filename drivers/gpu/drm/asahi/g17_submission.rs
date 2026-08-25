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
//! `AGXUncachedFWChannelMem`. The provenance ledger explicitly separates
//! byte-grounded G17 fields from inherited producer records whose G17 consumer
//! offsets still need first-hand confirmation. The native G17 channel-command
//! dispatcher is also modeled: it reads the tag at command offset zero, uses
//! an AGX3-specific accepted set through tag 18, and assigns tag 6 to the
//! SKSM-forbidden PM-rebuild command rather than AGX2's init-buffer command.
//!
//! Grounding is first-hand from the on-disk M5 role macho `GFX.a010.macho`
//! (`g17s a0, SKSM Enabled`) and the pinned G17P/G17G/G17X host-kext
//! `KSMKickQueue::addKicks` implementations. Firmware addresses are VAs
//! (0xfffffc0000000000 + fileoff - 0x4000). See
//! `research/gpu/GPU_AGX3_G17_SUBMISSION_2026-08-25.md`.
//!
//! Two-signal discipline, mirroring `g17_rtkit`/`g17_boot`:
//! [`GROUNDED_SUBMISSION_PRODUCER_AVAILABLE`] states that the *codec* is real
//! and cited; [`EXECUTABLE_SUBMISSION_TRANSPORT_AVAILABLE`] states whether a
//! submission can be driven to completion offline. It is `false`: the SKSM
//! completion path is a hardware kick FIFO the firmware reads through indexed
//! MMIO scratch registers. The host publication form is grounded, but its live
//! aperture values, silicon bridge to the firmware scratch consumer, round
//! trip, and completion IRQ can only be confirmed on-device. See
//! [`SUBMISSION_LIVE_GAPS`].

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
    /// A ring with zero entries cannot advance a producer index.
    RingSizeZero,
    /// A producer index must identify an entry inside its ring.
    RingIndexOutOfRange,
    /// G17 exposes four priority groups in its runtime channel table.
    ChannelPriorityOutOfRange,
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
// 2. Channel ring state — the driver->FW message ring carrying RunWorkQueueMsg.
//    AGX2 `channel.rs::raw::ChannelState`: read_ptr@0x00, write_ptr@0x20.
// ---------------------------------------------------------------------------

/// FW-owned consumer index the firmware publishes back to the driver.
pub(crate) const CHANNEL_STATE_READ_PTR_OFFSET: u32 = 0x00;
/// Driver-owned producer index the firmware reads to find new messages.
pub(crate) const CHANNEL_STATE_WRITE_PTR_OFFSET: u32 = 0x20;
/// Total `ChannelState` size.
pub(crate) const CHANNEL_STATE_SIZE: u32 = 0x40;

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
    pub(crate) byte_extent_60: usize,
}

pub(crate) const G17_UNCACHED_CHANNEL_RESET_LAYOUT: G17UncachedChannelResetLayout =
    G17UncachedChannelResetLayout {
        zero_00: 0x00,
        zero_10: 0x10,
        zero_20: 0x20,
        zero_30: 0x30,
        zero_40: 0x40,
        sentinel_50: 0x50,
        byte_extent_60: 0x60,
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
    byte_extent: u32,
) {
    let layout = G17_UNCACHED_CHANNEL_RESET_LAYOUT;

    put_u32_le(raw, layout.zero_00, 0);
    put_u32_le(raw, layout.zero_10, 0);
    put_u32_le(raw, layout.zero_20, 0);
    put_u32_le(raw, layout.zero_30, 0);
    put_u32_le(raw, layout.zero_40, 0);
    put_u32_le(raw, layout.sentinel_50, u32::MAX);
    put_u32_le(raw, layout.byte_extent_60, byte_extent);
}

// ---------------------------------------------------------------------------
// 3. Work-queue command ring (the actual SKSM producer ring).
//    AGX2 `fw/workqueue.rs::raw::RingState`, 0x10-strided fields.
// ---------------------------------------------------------------------------

/// FW-published "done" index; the producer guards `doneptr != next_wptr`.
pub(crate) const RING_STATE_GPU_DONEPTR_OFFSET: u32 = 0x00;
/// FW consumer index as it pops commands.
pub(crate) const RING_STATE_GPU_RPTR_OFFSET: u32 = 0x30;
/// Driver producer index; bumped after writing a command GPU-VA to the ring.
pub(crate) const RING_STATE_CPU_WPTR_OFFSET: u32 = 0x40;
/// Ring depth in entries.
pub(crate) const RING_STATE_RB_SIZE_OFFSET: u32 = 0x50;

/// Each ring entry is a u64 GPU virtual address of a command struct.
pub(crate) const RING_ENTRY_BYTES: u32 = 8;

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
//    runtime-selected aperture offsets.  The runtime bank selector is
//    multiplied by nine for every target.  This is a pure description of the
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

/// Runtime aperture geometry loaded by Apple's host immediately before the
/// two publication stores.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) struct SksmAddKicksMmioGeometry {
    pub(crate) tag_offset_base: u32,
    pub(crate) count_offset_base: u32,
    pub(crate) aperture_index: u32,
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
const SKSM_ADD_KICKS_APERTURE_SCALE: u32 = 9;

/// Prepare the host's two ordered MMIO stores without touching the aperture.
pub(crate) const fn prepare_sksm_add_kicks_mmio(
    target: SksmAddKicksTarget,
    queue_id: u8,
    add_count: u32,
    geometry: SksmAddKicksMmioGeometry,
) -> Result<PreparedSksmAddKicksMmio, SubmissionError> {
    if queue_id > KICK_QID_MASK as u8 {
        return Err(SubmissionError::QueueIdOutOfRange);
    }

    let bank_offset = match geometry
        .aperture_index
        .checked_mul(SKSM_ADD_KICKS_APERTURE_SCALE)
    {
        Some(offset) => offset,
        None => return Err(SubmissionError::KickMmioOffsetOverflow),
    };
    let tag_offset = match geometry.tag_offset_base.checked_add(bank_offset) {
        Some(offset) => offset,
        None => return Err(SubmissionError::KickMmioOffsetOverflow),
    };
    let count_offset = match geometry.count_offset_base.checked_add(bank_offset) {
        Some(offset) => offset,
        None => return Err(SubmissionError::KickMmioOffsetOverflow),
    };

    Ok(PreparedSksmAddKicksMmio {
        tag_offset,
        tag: sksm_add_kicks_tag(target, queue_id),
        count_offset,
        add_count,
    })
}

// ---------------------------------------------------------------------------
// 7. Native G17 channel-command dispatch.
//
//    Grounded first-hand from `agfaProcessChannelCommand` at GFX
//    0xfffffc00000122c0-0xfffffc0000012364 and its 19-entry jump table at
//    0xfffffc0000013654. The firmware validates at least four bytes, loads the
//    u32 tag from command offset zero, rejects tags above 18, and dispatches
//    the exact set below. Tags 8/10/11/13/17 share the invalid-command fatal
//    path at 0x13610. Tag 6 reaches the dedicated
//    `kAGFIChannelCommandTypePMRebuild must not be received in SKSM` fatal at
//    0x135f4. Therefore AGX2's `InitBuffer = 6` must not be reused on G17.
//
//    The optional pre-dispatch filter at 0x12318-0x12338 uses literal mask
//    0x1d03f for tags <= 16. Its accepted set is exposed separately so a
//    future live producer can preserve that gate instead of conflating it
//    with the complete jump-table dispatch.
// ---------------------------------------------------------------------------

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
#[repr(u32)]
pub(crate) enum G17KnownCommandType {
    RunVertex = 0,
    RunFragment = 1,
    RunBlitter = 2,
    RunCompute = 3,
    Barrier = 4,
    PmRebuild = 6,
}

/// Highest tag admitted by the native G17 jump-table bounds check.
pub(crate) const G17_CHANNEL_COMMAND_TYPE_MAX: u32 = 18;

/// Tags whose jump-table entries reach a normal processing case.
pub(crate) const G17_CHANNEL_COMMAND_PROCESS_MASK: u32 = 0x0005_d2bf;
/// Tag 6 has a dedicated PM-rebuild-in-SKSM fatal path.
pub(crate) const G17_CHANNEL_COMMAND_PM_REBUILD_MASK: u32 = 0x0000_0040;
/// In-range tags whose jump-table entries reach the invalid-command fatal.
pub(crate) const G17_CHANNEL_COMMAND_INVALID_MASK: u32 = 0x0002_2d00;
/// Literal mask used by the optional pre-dispatch filter for tags <= 16.
pub(crate) const G17_CHANNEL_COMMAND_PREFILTER_MASK: u32 = 0x0001_d03f;

/// Firmware disposition for one raw command tag.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) enum G17ChannelCommandDisposition {
    Process,
    PmRebuildForbidden,
    Invalid,
}

/// Classify a raw tag exactly like the native G17 jump table.
pub(crate) const fn classify_g17_channel_command(raw: u32) -> G17ChannelCommandDisposition {
    if raw > G17_CHANNEL_COMMAND_TYPE_MAX {
        return G17ChannelCommandDisposition::Invalid;
    }

    let bit = 1u32 << raw;
    if bit & G17_CHANNEL_COMMAND_PROCESS_MASK != 0 {
        G17ChannelCommandDisposition::Process
    } else if bit & G17_CHANNEL_COMMAND_PM_REBUILD_MASK != 0 {
        G17ChannelCommandDisposition::PmRebuildForbidden
    } else {
        G17ChannelCommandDisposition::Invalid
    }
}

/// Apply the firmware's optional `tag <= 16 && bit(tag) & 0x1d03f` filter.
pub(crate) const fn g17_channel_command_prefilter_allows(raw: u32) -> bool {
    raw <= 16 && ((1u32 << raw) & G17_CHANNEL_COMMAND_PREFILTER_MASK) != 0
}

// ---------------------------------------------------------------------------
// 8. Provenance ledger + live gaps.
// ---------------------------------------------------------------------------

/// How each submission-ABI component was grounded, so reuse-vs-fork is explicit.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) enum AbiProvenance {
    /// Byte-identical to the shipping AGX2 producer; reused unchanged.
    IdenticalToAgx2,
    /// AGX3-specific, grounded first-hand from the M5 firmware this pass.
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
    pub(crate) ring_state: AbiProvenance,
    pub(crate) channel_descriptor: AbiProvenance,
    pub(crate) host_channel_state_reset: AbiProvenance,
    pub(crate) host_uncached_channel_reset: AbiProvenance,
    pub(crate) command_types: AbiProvenance,
    pub(crate) kick_word: AbiProvenance,
    pub(crate) kick_queue_producer: AbiProvenance,
    pub(crate) host_add_kicks_mmio: AbiProvenance,
    pub(crate) kick_record: AbiProvenance,
}

/// The doorbell is byte-grounded (§1); the SKSM kick word/record and host
/// queue-timestamp producer are AGX3 forks grounded first-hand (§2-§5). The
/// channel/ring producer *records* are the AGX2 layouts the driver writes; the
/// G17 firmware consumer offsets were not read first-hand this pass. The G17
/// channel descriptor is a grounded AGX3 fork (§2a), while the state records
/// remain unconfirmed and fail-closed. The command dispatch is a grounded AGX3
/// fork (§7): in particular, inherited AGX2 `InitBuffer = 6` is rejected.
pub(crate) const SUBMISSION_ABI_LEDGER: SubmissionAbiLedger = SubmissionAbiLedger {
    tx_doorbell: AbiProvenance::IdenticalToAgx2,
    channel_state: AbiProvenance::Agx2InheritedUnconfirmed,
    ring_state: AbiProvenance::Agx2InheritedUnconfirmed,
    channel_descriptor: AbiProvenance::ForkedAgx3Grounded,
    host_channel_state_reset: AbiProvenance::ForkedAgx3Grounded,
    host_uncached_channel_reset: AbiProvenance::ForkedAgx3Grounded,
    command_types: AbiProvenance::ForkedAgx3Grounded,
    kick_word: AbiProvenance::ForkedAgx3Grounded,
    kick_queue_producer: AbiProvenance::ForkedAgx3Grounded,
    host_add_kicks_mmio: AbiProvenance::ForkedAgx3Grounded,
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
/// needs a live M5.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) struct SubmissionLiveGaps {
    /// Host `addKicks` publication and firmware scratch consumption are both
    /// grounded, but their silicon bridge, live aperture values, and ordering
    /// against the firmware consumer still need an on-device witness.
    pub(crate) sksm_hardware_kick_fifo_mmio_scratch_proven: bool,
    /// The kick -> execute -> completion IRQ handshake is a live event.
    pub(crate) doorbell_round_trip_proven: bool,
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
    fn channel_and_ring_state_offsets_match_agx2() {
        // channel.rs raw::ChannelState: read_ptr@0x00, write_ptr@0x20.
        assert_eq!(CHANNEL_STATE_READ_PTR_OFFSET, 0x00);
        assert_eq!(CHANNEL_STATE_WRITE_PTR_OFFSET, 0x20);
        assert_eq!(CHANNEL_STATE_SIZE, 0x40);
        // fw/workqueue.rs raw::RingState: doneptr@0x00, rptr@0x30, wptr@0x40,
        // size@0x50 (0x10-strided).
        assert_eq!(RING_STATE_GPU_DONEPTR_OFFSET, 0x00);
        assert_eq!(RING_STATE_GPU_RPTR_OFFSET, 0x30);
        assert_eq!(RING_STATE_CPU_WPTR_OFFSET, 0x40);
        assert_eq!(RING_STATE_RB_SIZE_OFFSET, 0x50);
        assert_eq!(RING_ENTRY_BYTES, 8);
        assert_eq!(HOST_MAX_OUTSTANDING_JOB_SLOTS, 127);
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
        let byte_extent = 0x800;
        apply_g17_uncached_channel_reset(&mut raw, byte_extent);
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
            &raw[layout.byte_extent_60..layout.byte_extent_60 + 4],
            &byte_extent.to_le_bytes()
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
        let geometry = SksmAddKicksMmioGeometry {
            tag_offset_base: 0x100,
            count_offset_base: 0x104,
            aperture_index: 3,
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
    fn host_add_kicks_mmio_offsets_fail_closed_on_overflow() {
        let valid = SksmAddKicksMmioGeometry {
            tag_offset_base: 0,
            count_offset_base: 0,
            aperture_index: u32::MAX / SKSM_ADD_KICKS_APERTURE_SCALE,
        };
        let publication =
            prepare_sksm_add_kicks_mmio(SksmAddKicksTarget::G17G, 0, 1, valid).unwrap();
        assert_eq!(
            publication.tag_offset,
            (u32::MAX / SKSM_ADD_KICKS_APERTURE_SCALE) * SKSM_ADD_KICKS_APERTURE_SCALE
        );

        assert_eq!(
            prepare_sksm_add_kicks_mmio(
                SksmAddKicksTarget::G17P,
                0x80,
                1,
                SksmAddKicksMmioGeometry {
                    tag_offset_base: 0,
                    count_offset_base: 4,
                    aperture_index: 0,
                },
            ),
            Err(SubmissionError::QueueIdOutOfRange)
        );
        for geometry in [
            SksmAddKicksMmioGeometry {
                tag_offset_base: 0,
                count_offset_base: 0,
                aperture_index: u32::MAX / SKSM_ADD_KICKS_APERTURE_SCALE + 1,
            },
            SksmAddKicksMmioGeometry {
                tag_offset_base: u32::MAX,
                count_offset_base: 0,
                aperture_index: 1,
            },
            SksmAddKicksMmioGeometry {
                tag_offset_base: 0,
                count_offset_base: u32::MAX,
                aperture_index: 1,
            },
        ] {
            assert_eq!(
                prepare_sksm_add_kicks_mmio(SksmAddKicksTarget::G17X, 0, 1, geometry),
                Err(SubmissionError::KickMmioOffsetOverflow)
            );
        }
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
    fn g17_command_dispatch_matches_the_firmware_jump_table() {
        assert_eq!(G17KnownCommandType::RunVertex as u32, 0);
        assert_eq!(G17KnownCommandType::RunFragment as u32, 1);
        assert_eq!(G17KnownCommandType::RunBlitter as u32, 2);
        assert_eq!(G17KnownCommandType::RunCompute as u32, 3);
        assert_eq!(G17KnownCommandType::Barrier as u32, 4);
        assert_eq!(G17KnownCommandType::PmRebuild as u32, 6);

        assert_eq!(G17_CHANNEL_COMMAND_TYPE_MAX, 18);
        assert_eq!(G17_CHANNEL_COMMAND_PROCESS_MASK, 0x0005_d2bf);
        assert_eq!(G17_CHANNEL_COMMAND_PM_REBUILD_MASK, 0x0000_0040);
        assert_eq!(G17_CHANNEL_COMMAND_INVALID_MASK, 0x0002_2d00);
        assert_eq!(
            G17_CHANNEL_COMMAND_PROCESS_MASK
                | G17_CHANNEL_COMMAND_PM_REBUILD_MASK
                | G17_CHANNEL_COMMAND_INVALID_MASK,
            (1 << (G17_CHANNEL_COMMAND_TYPE_MAX + 1)) - 1
        );

        for raw in 0..=G17_CHANNEL_COMMAND_TYPE_MAX {
            let expected = match raw {
                0 | 1 | 2 | 3 | 4 | 5 | 7 | 9 | 12 | 14 | 15 | 16 | 18 => {
                    G17ChannelCommandDisposition::Process
                }
                6 => G17ChannelCommandDisposition::PmRebuildForbidden,
                8 | 10 | 11 | 13 | 17 => G17ChannelCommandDisposition::Invalid,
                _ => unreachable!(),
            };
            assert_eq!(classify_g17_channel_command(raw), expected);
        }
        assert_eq!(
            classify_g17_channel_command(19),
            G17ChannelCommandDisposition::Invalid
        );
        assert_eq!(
            classify_g17_channel_command(u32::MAX),
            G17ChannelCommandDisposition::Invalid
        );
    }

    #[test]
    fn g17_command_prefilter_matches_literal_mask() {
        assert_eq!(G17_CHANNEL_COMMAND_PREFILTER_MASK, 0x0001_d03f);
        for raw in 0..=20 {
            let expected = matches!(raw, 0 | 1 | 2 | 3 | 4 | 5 | 12 | 14 | 15 | 16);
            assert_eq!(g17_channel_command_prefilter_allows(raw), expected);
        }
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
            SUBMISSION_ABI_LEDGER.command_types,
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
