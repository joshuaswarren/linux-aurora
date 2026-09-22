// SPDX-License-Identifier: GPL-2.0-only OR MIT

#![cfg_attr(not(test), allow(dead_code))]

//! Evidence-backed G17 three-level firmware init-data layout.
//!
//! The M5 firmware consumes one root per physical firmware role. Each
//! 0xc8-byte root points at a 0x4c0-byte runtime-pointer table, which in turn
//! points at a 0x2828-byte hardware/globals object plus a fixed set of
//! sub-allocations. This module encodes the full pointer graph the firmware
//! follows, using offsets and sizes grounded by matching host writes and
//! firmware reads.
//!
//! The whole graph was re-verified 2026-08-25 against the on-disk M5 firmware
//! `GFX.a010.macho`, init-data parser at VA 0xfffffc000002127c:
//!   - init-data tag `and 0xbf.. ; cmp 0x81..` @ 0x212b0-0x212bc;
//!   - address canonicalization `orr 0xfffff00000000000` @ 0x212c4;
//!   - magic `movz/movk` = 0x0c8de339072804c0 @ 0x212cc-0x212d8;
//!   - 0xc8-byte header copy into fw global 0x104890 @ 0x212e4-0x21328;
//!   - the sole parse-time value gate `ldr w9,[hdr+0x2c]; cbz -> abort`
//!     @ 0x2132c (host_mapped_allocs must be nonzero).
//! These are the *only* two acceptance checks the firmware makes while parsing
//! a root; every hw_globals field beyond them is read (and copied into fw
//! globals) but not validated, so a topology-correct, size-correct graph with
//! those two fields set is accepted by the parser.
//!
//! Byte slices are used instead of packed Rust structures because two proven
//! runtime-pointer fields are unaligned (`+0x254` and `+0x481`).

use core::result::Result;

/// The recovered fields are sufficient for offline validation and encoding.
pub(crate) const GROUNDED_LAYOUT_AVAILABLE: bool = true;
/// The init-data object graph the firmware's parser *accepts* — the
/// root/runtime_pointers/hw_globals pointer topology, the exact struct sizes,
/// the full sub-allocation graph, and the two parse-time acceptance gates
/// (magic + host_mapped) — is byte-grounded and encodable against the on-disk
/// M5 firmware (parser at 0xfffffc000002127c). This is what `INITDATA_GRAPH`
/// gates. It deliberately does *not* assert that every scalar the firmware
/// later reads out of hw_globals is grounded; see
/// [`ALL_HW_GLOBALS_VALUES_GROUNDED`].
pub(crate) const PARSE_ACCEPTED_GRAPH_AVAILABLE: bool = true;
/// Whether every hw_globals scalar the firmware reads (clock/voltage/power
/// tables, feature words) is grounded to a concrete source. It is not: those
/// values are ADT/power-manager runtime data, gated separately by
/// `g17_boot::missing::GFX1_POWER_SEQUENCE`, and a wrong value is a functional
/// (not a parse-acceptance) failure that only a live M5 boot can expose.
pub(crate) const ALL_HW_GLOBALS_VALUES_GROUNDED: bool = false;

pub(crate) const ROOT_SIZE: usize = 0x0c8;
pub(crate) const RUNTIME_POINTERS_SIZE: usize = 0x4c0;
pub(crate) const HW_GLOBALS_SIZE: usize = 0x2828;
pub(crate) const INITDATA_MAGIC: u64 = 0x0c8d_e339_0728_04c0;
pub(crate) const PERF_STATE_MAX: usize = 16;

const INITDATA_ADDR_MASK: u64 = 0x0000_0fff_ffff_ffff;
const INITDATA_ADDR_PREFIX: u64 = 0xffff_f000_0000_0000;

// --- Root layout ---------------------------------------------------------------

const ROOT_VERSION: usize = 0x00;
const ROOT_REGION_A: usize = 0x08;
/// Root `+0x10` is zero at handoff on both instances; semantics unknown.
const ROOT_ZERO_10: usize = 0x10;
const ROOT_MAIN_CONFIG: usize = 0x18;
const ROOT_REGION_C: usize = 0x20;
const ROOT_INSTANCE_KIND: usize = 0x28;
const ROOT_CONSTANT_2C: usize = 0x2c;
const ROOT_PAGE_SIZE: usize = 0x30;
const ROOT_PAGE_BITS: usize = 0x32;
const ROOT_LEVEL_COUNT: usize = 0x33;
const ROOT_LEVELS: usize = 0x34;
/// Root `+0xa8`: Status A pointer. (The M5-derived model misused this slot as
/// a role pointer.)
const ROOT_STATUS_A: usize = 0xa8;
/// Root `+0xb0`: Status B pointer. Real for the primary, zero for the
/// secondary. (The M5-derived model misused it as a cross-role pointer.)
const ROOT_STATUS_B: usize = 0xb0;
const ROOT_SECONDARY_EXTRA_0: usize = 0xb8;
const ROOT_SECONDARY_EXTRA_1: usize = 0xc0;

// --- UAT geometry (root +0x30..+0x94) -----------------------------------------

pub(crate) const UAT_PAGE_SIZE: u16 = 0x4000;
pub(crate) const UAT_PAGE_BITS: u8 = 14;
pub(crate) const UAT_LEVEL_COUNT: usize = 3;
/// `(index_shift, entry_count)` for the three levels below the root,
/// confirmed against a live table walk.
pub(crate) const UAT_LEVELS: [(u8, u16); UAT_LEVEL_COUNT] = [(36, 64), (25, 2048), (14, 2048)];
pub(crate) const UAT_LEVEL_DESC_SIZE: usize = 0x20;
/// Level descriptor `+0x10`: physical mask, identical in all three.
pub(crate) const UAT_LEVEL_PHYS_MASK: u64 = 0x0000_03ff_ffff_c000;

// --- Main configuration layout --------------------------------------------------

const MAIN_HW_DATA: usize = 0x00;
/// The same repeated-region address appears at `+0x08` and `+0x10`; it is the
/// bundle base plus [`MAIN_REPEATED_REGION_BUNDLE_OFFSET`], not the bundle
/// base itself. It reads zero at handoff (firmware writes into it), so the
/// host only has to provide it mapped.
const MAIN_REPEATED_0: usize = 0x08;
const MAIN_REPEATED_1: usize = 0x10;
pub(crate) const MAIN_REPEATED_REGION_BUNDLE_OFFSET: usize = 0xc500;
const MAIN_CHANNEL_TABLE: usize = 0x20;
/// Main `+0x254`: five unaligned u64 views into the hardware-data bundle.
const MAIN_BUNDLE_VIEWS: usize = 0x254;
/// Main `+0x2d0`: six qwords of region views.
const MAIN_REGION_VIEWS: usize = 0x2d0;
/// Main `+0x300`: u32 4, secondary only.
const MAIN_SECONDARY_KIND: usize = 0x300;
const MAIN_SECONDARY_KIND_VALUE: u32 = 4;
/// Main `+0x3e0`: u32 0xff, primary only.
const MAIN_PRIMARY_MASK: usize = 0x3e0;
const MAIN_PRIMARY_MASK_VALUE: u32 = 0xff;
const MAIN_PRIMARY_SCALAR_32C: usize = 0x32c;
const MAIN_PRIMARY_SCALAR_344: usize = 0x344;
/// Main `+0x471`: unaligned u64, secondary only, pointing *into* bundle view 2
/// at [`SECONDARY_EXTRA_VIEW_OFFSET`] (not a fresh allocation — the M5-derived
/// model had this at `+0x481` backed by an invented 0x12688-byte buffer; both
/// were wrong).
const MAIN_SECONDARY_EXTRA_PTR: usize = 0x471;
pub(crate) const SECONDARY_EXTRA_VIEW_INDEX: usize = 2;
pub(crate) const SECONDARY_EXTRA_VIEW_OFFSET: usize = 0xe40;
/// Main `+0x4c0` is both a scalar and a place: the leading u32 of the
/// device-control ring, which is embedded in the main configuration object at
/// exactly this offset on both instances. Writing the opening opcode here *is*
/// staging the opening device-control record (records are 0x40 bytes; fields
/// past the opcode are zero). The control channel's producer counter must then
/// be set to the role-specific value returned by
/// [`control_opening_producer`] before the descriptor is handed over.
const MAIN_CONTROL_RING: usize = 0x4c0;
pub(crate) const CONTROL_OPENING_OPCODE_PRIMARY: u32 = 0x16;
pub(crate) const CONTROL_OPENING_OPCODE_SECONDARY: u32 = 0x2a;
pub(crate) const CONTROL_RECORD_SIZE: usize = 0x40;
pub(crate) const CONTROL_OPENING_PRIMARY_RECORDS: usize = 1;
pub(crate) const CONTROL_OPENING_SECONDARY_RECORDS: usize = 1;
pub(crate) const CONTROL_OPENING_PRIMARY_PRODUCER: u32 = 1;
pub(crate) const CONTROL_OPENING_SECONDARY_PRODUCER: u32 = 1;
pub(crate) const CONTROL_OPENING_PRIMARY_REGISTER_OPCODE: u32 = 0x20;
pub(crate) const CONTROL_OPENING_SECONDARY_CONTINUE_OPCODE: u32 = 0x22;
/// Hardware-buffer release emitted after the opening UMA grow request retires.
pub(crate) const CONTROL_BOOTSTRAP_UMA_RELEASE_OPCODE: u32 = 0x2e;
/// Device-control ring capacity. FW_PROVEN (b1
/// `AGFAcceleratorDrainDeviceControlRing` @ `0xfffffc0000003a74`): the drain
/// walks `ring + index * 0x40` and advances its consumer with
/// `(consumer + 1) & 0xff`, so the ring is exactly 256 records. The placement
/// agrees byte for byte: the primary ring starts at
/// [`NATIVE_PRIMARY_MAIN_BUNDLE_OFFSET`] `+ 0x4c0` = bundle `+0x1ea80` and the
/// secondary main configuration begins at bundle `+0x22a80`, exactly
/// `0x100 * 0x40` bytes later.
pub(crate) const CONTROL_RING_ENTRIES: usize = 0x100;
pub(crate) const CONTROL_RING_INDEX_MASK: u32 = CONTROL_RING_ENTRIES as u32 - 1;
/// Device control `IdlePowerOff(arg)`. FW_PROVEN: the opcode jump table at
/// `0xfffffc0000005380` (indexed by `opcode - 4`) sends `0x0a` to
/// `0xfffffc0000003fd8`, which stores the record's `arg` into the idle-power-off
/// enable word `[0xfffffc0000177838]` and, **only when that arg is zero**, does
/// an unconditional `bl 0xfffffc0000021960`. That routine is the one thing in
/// the image that releases KSM pause reason bit 1 (mask `0x2`) in steady state:
/// it latches `[0xfffffc00001771f8] |= 2` and tail-calls
/// `gpu_core_power(0, 1, 0)` @ `0xfffffc000000e004`, whose power-on arm clears
/// the reason bit at `0xe488` and emits KTrace `0x46 (0, 0x2, mask)` at
/// `0xe4cc`. It is gated by nothing -- unlike the EP-0x21 work doorbell, which
/// handler `0x261a4` drops silently while `[0x1771f8] & 0x18` (RTKit
/// SLEEP/NAP) -- and it is idempotent, because `0x21960` returns immediately
/// when the powered latch is already set.
pub(crate) const DEVICE_CONTROL_OPCODE_IDLE_POWER_OFF: u32 = 0x0a;
/// Disable ordinary idle power-off and request that the GPU cores wake now.
///
/// FW_PROVEN (25F80 B100, SHA256 83af9ae0...): opcode 0x0a stores this value
/// in `[0xfffffc0000177838]` and calls the wake path only for zero. The
/// scheduler's 0x20000000 idle event skips its descent while that policy word
/// is zero. A cold wake may reload the word from Region-C +0x30, so callers
/// which need a scoped hold must retire a second zero after the core is up.
pub(crate) const DEVICE_CONTROL_IDLE_POWER_OFF_ARG_DISABLE_IDLE_AND_WAKE: u32 = 0;
/// Restore the firmware's ordinary idle power-off policy. A non-zero value
/// permits the scheduler idle event to descend; it is not a power hold.
pub(crate) const DEVICE_CONTROL_IDLE_POWER_OFF_ARG_ENABLE_IDLE: u32 = 1;
/// Separate retained compute resources from the render pool. Native
/// populateInitialFirmwareState (8aa258c) publishes resource-derived +0x14
/// page-list and +0x30 run-list addresses; neither is firmware-fixed.
pub(crate) const COMPUTE_FLIST_RELOCATION: u64 = 0x0200_0000;
pub(crate) const COMPUTE_FLIST_PAGE_LIST_ADDRESS: u64 = 0x70_0000_0000 + COMPUTE_FLIST_RELOCATION;
pub(crate) const COMPUTE_FLIST_RUN_TABLE_ADDRESS: u64 = CONTROL_OPERAND_TABLE_ADDRESS + COMPUTE_FLIST_RELOCATION;
pub(crate) const COMPUTE_FLIST_BUFFER_BASE: u64 = CONTROL_OPERAND_BUFFER_BASE + COMPUTE_FLIST_RELOCATION;

pub(crate) const COMPUTE_RUNTIME_CONTROL_RECORDS: usize = 65;
pub(crate) const COMPUTE_RUNTIME_CONTROL_FINAL_COUNTER: u32 =
    CONTROL_OPENING_PRIMARY_PRODUCER + COMPUTE_RUNTIME_CONTROL_RECORDS as u32;
pub(crate) const COMPUTE_RUNTIME_CLASS1_RECORD_INDEX: usize = 54;
pub(crate) const COMPUTE_RUNTIME_CLASS2_RECORD_INDEX: usize = 57;
pub(crate) const COMPUTE_RUNTIME_SUPPORT_ADDRESS: u64 = 0xffff_fc20_c086_8000;
pub(crate) const COMPUTE_RUNTIME_STATE_ADDRESS: u64 = 0xffff_fc20_0163_0000;
pub(crate) const COMPUTE_RUNTIME_CLASS1_TABLE_ADDRESS: u64 = 0x70_01ad_8000 + COMPUTE_FLIST_RELOCATION;
pub(crate) const COMPUTE_RUNTIME_PAGE_LIST_ADDRESS: u64 = 0x70_018d_0000 + COMPUTE_FLIST_RELOCATION;
pub(crate) const COMPUTE_RUNTIME_ZERO_BUFFER_0_ADDRESS: u64 = 0x70_0000_0000 + COMPUTE_FLIST_RELOCATION;
pub(crate) const COMPUTE_RUNTIME_ZERO_BUFFER_1_ADDRESS: u64 = 0x70_0010_8000 + COMPUTE_FLIST_RELOCATION;
pub(crate) const COMPUTE_RUNTIME_SUPPORT_SIZE: usize = 0x4000;
pub(crate) const COMPUTE_RUNTIME_CLASS1_TABLE_SIZE: usize = 0x4000;
pub(crate) const COMPUTE_RUNTIME_PAGE_LIST_SIZE: usize = 0xa800;
pub(crate) const COMPUTE_DISPATCH_RECORD_SIZE: usize = 0x20;

/// Output-positive first-CL readiness suffix. These six records follow the
/// clean three-record QoS opening. Native publishes and retires each slot
/// before exposing the next.
pub(crate) const COMPUTE_READINESS_RECORDS: usize = 6;
/// Output-positive final compute graph maps and names exactly operand entries
/// 0..20. The retained opening still owns 28 backing objects.
pub(crate) const COMPUTE_READINESS_OPERAND_ENTRY_COUNT: usize = 21;
pub(crate) const COMPUTE_READINESS_FINAL_COUNTER: u32 =
    CONTROL_OPENING_PRIMARY_PRODUCER + COMPUTE_READINESS_RECORDS as u32;
pub(crate) const COMPUTE_READINESS_CLASS1_SUPPORT_ADDRESS: u64 = 0xffff_fc20_c087_8000;
pub(crate) const COMPUTE_READINESS_CLASS1_STATE_ADDRESS: u64 = 0xffff_fc20_0164_8000;
pub(crate) const COMPUTE_READINESS_CLASS3_SUPPORT_ADDRESS: u64 = 0xffff_fc20_c085_0000;
pub(crate) const COMPUTE_READINESS_CLASS3_STATE_ADDRESS: u64 = 0xffff_fc20_0165_0000;
pub(crate) const COMPUTE_READINESS_PAGE_SIZE: usize = 0x4000;
pub(crate) const COMPUTE_READINESS_ACTIVATION_RECORD_INDEX: usize = 2;
pub(crate) const COMPUTE_READINESS_ACTIVATION_RECORD_OFFSET: usize =
    COMPUTE_READINESS_ACTIVATION_RECORD_INDEX * CONTROL_RECORD_SIZE;

/// One native first-CL device-control publication boundary.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) struct ComputeReadinessRecordBoundary {
    pub(crate) index: u8,
    pub(crate) producer_before: u32,
    pub(crate) producer_after: u32,
    pub(crate) opcode: u32,
    pub(crate) sequence: u32,
    pub(crate) context: u32,
}

pub(crate) const COMPUTE_READINESS_BOUNDARIES:
    [ComputeReadinessRecordBoundary; COMPUTE_READINESS_RECORDS] = [
    ComputeReadinessRecordBoundary {
        index: 0,
        producer_before: CONTROL_OPENING_PRIMARY_PRODUCER,
        producer_after: CONTROL_OPENING_PRIMARY_PRODUCER + 1,
        opcode: 0x20,
        sequence: 35,
        context: 1,
    },
    ComputeReadinessRecordBoundary {
        index: 1,
        producer_before: CONTROL_OPENING_PRIMARY_PRODUCER + 1,
        producer_after: CONTROL_OPENING_PRIMARY_PRODUCER + 2,
        opcode: 0x2e,
        sequence: 35,
        context: 1,
    },
    ComputeReadinessRecordBoundary {
        index: 2,
        producer_before: CONTROL_OPENING_PRIMARY_PRODUCER + 2,
        producer_after: CONTROL_OPENING_PRIMARY_PRODUCER + 3,
        opcode: 0x2e,
        sequence: 36,
        context: 1,
    },
    ComputeReadinessRecordBoundary {
        index: 3,
        producer_before: CONTROL_OPENING_PRIMARY_PRODUCER + 3,
        producer_after: CONTROL_OPENING_PRIMARY_PRODUCER + 4,
        opcode: 0x20,
        sequence: 37,
        context: 2,
    },
    ComputeReadinessRecordBoundary {
        index: 4,
        producer_before: CONTROL_OPENING_PRIMARY_PRODUCER + 4,
        producer_after: CONTROL_OPENING_PRIMARY_PRODUCER + 5,
        opcode: 0x2e,
        sequence: 37,
        context: 2,
    },
    ComputeReadinessRecordBoundary {
        index: 5,
        producer_before: CONTROL_OPENING_PRIMARY_PRODUCER + 5,
        producer_after: CONTROL_OPENING_PRIMARY_PRODUCER + 6,
        opcode: 0x2e,
        sequence: 38,
        context: 2,
    },
];

pub(crate) const fn compute_readiness_boundary(
    index: usize,
) -> Option<ComputeReadinessRecordBoundary> {
    if index < COMPUTE_READINESS_RECORDS {
        Some(COMPUTE_READINESS_BOUNDARIES[index])
    } else {
        None
    }
}

/// Host progress for the native per-record publication handshake.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) struct ComputeReadinessProgress {
    retired_records: u8,
    pending_target: Option<u32>,
}

impl ComputeReadinessProgress {
    pub(crate) const fn new() -> Self {
        Self {
            retired_records: 0,
            pending_target: None,
        }
    }

    pub(crate) const fn retired_records(self) -> usize {
        self.retired_records as usize
    }

    pub(crate) const fn pending_target(self) -> Option<u32> {
        self.pending_target
    }

    pub(crate) const fn next_boundary(self) -> Option<ComputeReadinessRecordBoundary> {
        if self.pending_target.is_some() {
            None
        } else {
            compute_readiness_boundary(self.retired_records as usize)
        }
    }

    pub(crate) const fn begin_publication(
        self,
        boundary: ComputeReadinessRecordBoundary,
    ) -> Option<Self> {
        if self.pending_target.is_some() || boundary.index != self.retired_records {
            return None;
        }
        Some(Self {
            retired_records: self.retired_records,
            pending_target: Some(boundary.producer_after),
        })
    }

    pub(crate) const fn retire(self, target: u32) -> Option<Self> {
        match self.pending_target {
            Some(pending) if pending == target => Some(Self {
                retired_records: self.retired_records + 1,
                pending_target: None,
            }),
            _ => None,
        }
    }

    pub(crate) const fn complete(self) -> bool {
        self.retired_records as usize == COMPUTE_READINESS_RECORDS
            && self.pending_target.is_none()
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) enum ComputeReadinessPublicationStep {
    ControlRecord(u8),
    InitializeContext2Activation,
}

pub(crate) const COMPUTE_READINESS_PUBLICATION_ORDER: [ComputeReadinessPublicationStep; 7] = [
    ComputeReadinessPublicationStep::ControlRecord(0),
    ComputeReadinessPublicationStep::ControlRecord(1),
    ComputeReadinessPublicationStep::ControlRecord(2),
    ComputeReadinessPublicationStep::InitializeContext2Activation,
    ComputeReadinessPublicationStep::ControlRecord(3),
    ComputeReadinessPublicationStep::ControlRecord(4),
    ComputeReadinessPublicationStep::ControlRecord(5),
];

pub(crate) const CONTROL_SHARED_ADDRESS: u64 = 0xffff_fc20_c083_0000;
pub(crate) const CONTROL_SHARED_INNER_ADDRESS: u64 = 0xffff_fc20_0160_8000;
pub(crate) const CONTROL_SHARED_OBJECT_SIZE: usize = 0x4000;
pub(crate) const CONTROL_SHARED_CURSOR_OFFSET: usize = 0x48;
pub(crate) const CONTROL_SHARED_CURSOR_BEFORE: u32 = 0x88;
pub(crate) const CONTROL_SHARED_INNER_PRESENTED: u64 = 2;
/// Separate compact class-2 object consumed by the first-partial opening.
/// Descriptor tails and later compute control continue to use
/// [`CONTROL_SHARED_ADDRESS`].
pub(crate) const PARTIAL_OPENING_CONTROL_SHARED_ADDRESS: u64 = 0xffff_fc20_c082_8000;
pub(crate) const PARTIAL_OPENING_CONTROL_SHARED_INNER_ADDRESS: u64 =
    0xffff_fc20_0160_0000;
/// Hardware-proven compact compute cursor. Twenty-one consecutive run qwords
/// occupy slots 0..20 and the next eight-qword boundary is 0x18.
pub(crate) const COMPUTE_PARTIAL_OPENING_CONTROL_SHARED_CURSOR_BEFORE: u32 = 0x18;
/// Initial compact FList cursor in qwords.
///
/// `AGXUSCPrivMemFList::prepareBufferResources` encodes one 1-MiB run for
/// each of the 17 initial pool blocks, rounding to an eight-qword slot after
/// each block. The first grow starts at qword 17 * 8 = 0x88 (byte +0x440),
/// exactly as the complete 25G83 first-render opcode-0x20 record publishes.
pub(crate) const PARTIAL_OPENING_CONTROL_SHARED_CURSOR_BEFORE: u32 = 0x88;
pub(crate) const PARTIAL_OPENING_CONTROL_SHARED_INNER_PRESENTED: u64 = 0;
/// Linux owns one USC private-memory FList. HardwareBufferIDManager permits
/// slot zero (native's generic allocator pops 0 first); macOS observed ID 1
/// only because another live FList already owned slot zero in that session.
pub(crate) const PARTIAL_OPENING_FREELIST_HARDWARE_BUFFER_ID: u32 = 0;
pub(crate) const CONTROL_OPERAND_TABLE_ADDRESS: u64 = 0x70_0020_8000;
pub(crate) const CONTROL_OPERAND_TABLE_SIZE: usize = 0x4000;
pub(crate) const CONTROL_OPERAND_SLOT_OFFSET: u64 = 0x440;
pub(crate) const PARTIAL_OPENING_CONTROL_OPERAND_SLOT_OFFSET: u64 = 0x640;
pub(crate) const PARTIAL_OPENING_CONTROL_REGISTER_COUNT: u32 = 0x18;
pub(crate) const PARTIAL_OPENING_CONTROL_OPERAND_ENTRY_COUNT: usize = 28;
pub(crate) const CONTROL_OPERAND_BUFFER_BASE: u64 = 0x70_0022_0000;
pub(crate) const CONTROL_OPERAND_BUFFER_STRIDE: u64 = 0x10_8000;
pub(crate) const CONTROL_OPERAND_BUFFER_SIZE: u64 = 0x10_0000;
pub(crate) const CONTROL_OPERAND_ENTRY_COUNT: usize = 22;
pub(crate) const CONTROL_OPERAND_ENTRY_STRIDE: usize = 0x40;
pub(crate) const CONTROL_OPERAND_ENTRY_FLAG: u64 = 1 << 60;
pub(crate) const COMPUTE_RUNTIME_BUFFER_ADDRESSES: [u64; 21] = [
    0x70_0022_0000 + COMPUTE_FLIST_RELOCATION,
    0x70_0032_8000 + COMPUTE_FLIST_RELOCATION,
    0x70_0043_0000 + COMPUTE_FLIST_RELOCATION,
    0x70_0053_8000 + COMPUTE_FLIST_RELOCATION,
    0x70_0064_0000 + COMPUTE_FLIST_RELOCATION,
    0x70_0074_8000 + COMPUTE_FLIST_RELOCATION,
    0x70_0085_0000 + COMPUTE_FLIST_RELOCATION,
    0x70_0095_8000 + COMPUTE_FLIST_RELOCATION,
    0x70_00a6_0000 + COMPUTE_FLIST_RELOCATION,
    0x70_00b6_8000 + COMPUTE_FLIST_RELOCATION,
    0x70_00c7_0000 + COMPUTE_FLIST_RELOCATION,
    0x70_00d7_8000 + COMPUTE_FLIST_RELOCATION,
    0x70_00e8_0000 + COMPUTE_FLIST_RELOCATION,
    0x70_00f8_8000 + COMPUTE_FLIST_RELOCATION,
    0x70_0109_0000 + COMPUTE_FLIST_RELOCATION,
    0x70_0119_8000 + COMPUTE_FLIST_RELOCATION,
    0x70_012a_0000 + COMPUTE_FLIST_RELOCATION,
    0x70_013a_8000 + COMPUTE_FLIST_RELOCATION,
    COMPUTE_RUNTIME_ZERO_BUFFER_0_ADDRESS,
    COMPUTE_RUNTIME_ZERO_BUFFER_1_ADDRESS,
    0x70_014b_0000 + COMPUTE_FLIST_RELOCATION,
];

pub(crate) const fn control_opening_producer(role: InstanceRole) -> u32 {
    match role {
        InstanceRole::Primary => CONTROL_OPENING_PRIMARY_PRODUCER,
        InstanceRole::Secondary => CONTROL_OPENING_SECONDARY_PRODUCER,
    }
}

pub(crate) const fn control_opening_size(role: InstanceRole) -> usize {
    match role {
        InstanceRole::Primary => CONTROL_OPENING_PRIMARY_RECORDS * CONTROL_RECORD_SIZE,
        InstanceRole::Secondary => CONTROL_OPENING_SECONDARY_RECORDS * CONTROL_RECORD_SIZE,
    }
}

fn encode_control_registration(
    out: &mut [u8],
    slot: usize,
    control_class: u32,
    sequence: u32,
    object: u64,
    operand: u64,
    slot_offset: u64,
    count: u32,
    context_word: u32,
) {
    let base = slot * CONTROL_RECORD_SIZE;
    put_u32(out, base, CONTROL_OPENING_PRIMARY_REGISTER_OPCODE);
    put_u32(out, base + 0x04, control_class);
    put_u32(out, base + 0x08, 0x3f);
    put_u32(out, base + 0x0c, sequence);
    put_u64(out, base + 0x14, object);
    put_u64(out, base + 0x1c, operand);
    put_u64(out, base + 0x24, operand + slot_offset);
    put_u32(out, base + 0x2c, count);
    put_u32(out, base + 0x30, context_word);
    put_u32(out, base + 0x34, 1);
}

fn encode_control_tick(out: &mut [u8], slot: usize, sequence: u32) {
    let base = slot * CONTROL_RECORD_SIZE;
    put_u32(out, base, CONTROL_BOOTSTRAP_UMA_RELEASE_OPCODE);
    put_u32(out, base + 0x04, sequence);
}

/// Encode the exact release record produced when an Async FList grow request
/// drops its last HardwareBuffer ID reference. The captured opening uses
/// sequence zero and ID zero; accepting them as operands keeps the encoder
/// tied to the retained host ownership rather than to a raw type-13 tuple.
pub(crate) fn encode_bootstrap_uma_release_record(
    sequence: u64,
    hardware_buffer_id: u32,
    out: &mut [u8],
) -> Result<(), InitdataError> {
    if out.len() != CONTROL_RECORD_SIZE {
        return Err(InitdataError::BufferSize);
    }
    out.fill(0);
    put_u32(out, 0, CONTROL_BOOTSTRAP_UMA_RELEASE_OPCODE);
    put_u64(out, 0x04, sequence);
    put_u32(out, 0x0c, hardware_buffer_id);
    Ok(())
}

fn encode_control_tick_with_context(
    out: &mut [u8],
    slot: usize,
    sequence: u32,
    context_word: u32,
) {
    encode_control_tick(out, slot, sequence);
    let base = slot * CONTROL_RECORD_SIZE;
    put_u32(out, base + 0x0c, context_word);
}

/// Encode one of the six exact records immediately preceding the first CL.
pub(crate) fn encode_compute_readiness_record(
    index: usize,
    out: &mut [u8],
) -> Result<(), InitdataError> {
    if out.len() != CONTROL_RECORD_SIZE || index >= COMPUTE_READINESS_RECORDS {
        return Err(InitdataError::BufferSize);
    }
    out.fill(0);
    match index {
        0 => encode_control_registration(
            out,
            0,
            1,
            35,
            COMPUTE_READINESS_CLASS1_SUPPORT_ADDRESS,
            COMPUTE_FLIST_RUN_TABLE_ADDRESS,
            0x440,
            0x20,
            1,
        ),
        1 => encode_control_tick_with_context(out, 0, 35, 1),
        2 => encode_control_tick_with_context(out, 0, 36, 1),
        3 => encode_control_registration(
            out,
            0,
            3,
            37,
            COMPUTE_READINESS_CLASS3_SUPPORT_ADDRESS,
            COMPUTE_FLIST_RUN_TABLE_ADDRESS,
            0x5c0,
            0x28,
            2,
        ),
        4 => encode_control_tick_with_context(out, 0, 37, 2),
        5 => encode_control_tick_with_context(out, 0, 38, 2),
        _ => return Err(InitdataError::BufferSize),
    }
    Ok(())
}

fn encode_compute_readiness_support(
    out: &mut [u8],
    class: u32,
    active: u32,
    resource_class: u64,
    cursor: u32,
    state: u64,
    final_kind: u32,
) -> Result<(), InitdataError> {
    if out.len() != COMPUTE_READINESS_PAGE_SIZE {
        return Err(InitdataError::BufferSize);
    }
    out.fill(0);
    put_u32(out, 0x00, class);
    put_u32(out, 0x08, active);
    put_u32(out, 0x10, class);
    put_u64(out, 0x14, COMPUTE_FLIST_PAGE_LIST_ADDRESS);
    put_u32(out, 0x1c, 0x4_0000);
    put_u64(out, 0x20, resource_class << 40);
    put_u64(out, 0x28, resource_class << 40);
    put_u64(out, 0x30, COMPUTE_FLIST_RUN_TABLE_ADDRESS);
    put_u32(out, 0x40, 4);
    put_u32(out, 0x48, cursor);
    put_u64(out, 0x4c, state);
    put_u32(out, 0x54, 0);
    put_u32(out, 0x5c, 0);
    put_u32(out, 0x60, final_kind);
    Ok(())
}

pub(crate) fn encode_compute_readiness_class1_support(
    out: &mut [u8],
) -> Result<(), InitdataError> {
    encode_compute_readiness_support(
        out,
        1,
        1,
        0x11,
        0x88,
        COMPUTE_READINESS_CLASS1_STATE_ADDRESS,
        2,
    )
}

pub(crate) fn encode_compute_readiness_class3_support(
    out: &mut [u8],
) -> Result<(), InitdataError> {
    encode_compute_readiness_support(
        out,
        3,
        2,
        0x17,
        0xb8,
        COMPUTE_READINESS_CLASS3_STATE_ADDRESS,
        3,
    )
}

pub(crate) fn encode_compute_readiness_state(
    out: &mut [u8],
) -> Result<(), InitdataError> {
    if out.len() != COMPUTE_READINESS_PAGE_SIZE {
        return Err(InitdataError::BufferSize);
    }
    out.fill(0);
    put_u32(out, 0, 1);
    Ok(())
}

pub(crate) fn encode_compute_readiness_activation_record(
    out: &mut [u8],
) -> Result<(), InitdataError> {
    if out.len() != CONTROL_RECORD_SIZE {
        return Err(InitdataError::BufferSize);
    }
    out.fill(0);
    put_u64(out, 0x00, 0x0000_0100_0000_ffff);
    put_u64(out, 0x20, 0x0002_0000_0000_0000);
    put_u64(out, 0x30, 0x0000_0000_ff00_0000);
    Ok(())
}

pub(crate) fn encode_compute_readiness_operand_table(
    out: &mut [u8],
) -> Result<(), InitdataError> {
    if out.len() != CONTROL_OPERAND_TABLE_SIZE {
        return Err(InitdataError::BufferSize);
    }
    out.fill(0);
    for index in 0..COMPUTE_READINESS_OPERAND_ENTRY_COUNT {
        let address = COMPUTE_FLIST_BUFFER_BASE
            .checked_add(index as u64 * CONTROL_OPERAND_BUFFER_STRIDE)
            .ok_or(InitdataError::AddressOutOfRange)?;
        put_u64(
            out,
            index * CONTROL_OPERAND_ENTRY_STRIDE,
            address | CONTROL_OPERAND_ENTRY_FLAG,
        );
    }
    Ok(())
}

/// Encode the complete control ring prefix consumed during the compute opening.
pub(crate) fn encode_control_opening(
    role: InstanceRole,
    out: &mut [u8],
) -> Result<(), InitdataError> {
    if out.len() != control_opening_size(role) {
        return Err(InitdataError::BufferSize);
    }
    out.fill(0);

    match role {
        InstanceRole::Primary => {
            for slot in 0..CONTROL_OPENING_PRIMARY_RECORDS {
                put_u32(
                    out,
                    slot * CONTROL_RECORD_SIZE,
                    CONTROL_OPENING_OPCODE_PRIMARY,
                );
            }
        }
        InstanceRole::Secondary => {
            let initial_records = core::cmp::min(3, CONTROL_OPENING_SECONDARY_RECORDS);
            for slot in 0..initial_records {
                put_u32(
                    out,
                    slot * CONTROL_RECORD_SIZE,
                    CONTROL_OPENING_OPCODE_SECONDARY,
                );
            }
            for slot in initial_records..CONTROL_OPENING_SECONDARY_RECORDS {
                put_u32(
                    out,
                    slot * CONTROL_RECORD_SIZE,
                    CONTROL_OPENING_SECONDARY_CONTINUE_OPCODE,
                );
            }
        }
    }
    Ok(())
}

/// Encode one bare device-control record: `u32 opcode @ +0x00`,
/// `u32 arg @ +0x04`, and 0x38 zero bytes.
///
/// FW_PROVEN (b1 `0x3a74` drain): a record is 0x40 bytes at
/// `ring + index * 0x40`; the drain loads the opcode with `ldr w?, [x24]` and
/// the argument with `ldr w8, [x24, #4]`. Every opcode that carries no further
/// operands leaves the remaining 0x38 bytes untouched, exactly as the opening
/// records this module already stages do.
pub(crate) fn encode_device_control_record(
    opcode: u32,
    arg: u32,
    out: &mut [u8],
) -> Result<(), InitdataError> {
    if out.len() != CONTROL_RECORD_SIZE {
        return Err(InitdataError::BufferSize);
    }
    out.fill(0);
    put_u32(out, 0x00, opcode);
    put_u32(out, 0x04, arg);
    Ok(())
}

/// Encode one record in the output-positive 65-record runtime control tail.
/// Record zero follows the clean three-record QoS opening.
pub(crate) fn encode_compute_runtime_control_record(
    index: usize,
    out: &mut [u8],
) -> Result<(), InitdataError> {
    if out.len() != CONTROL_RECORD_SIZE || index >= COMPUTE_RUNTIME_CONTROL_RECORDS {
        return Err(InitdataError::BufferSize);
    }
    out.fill(0);

    match index {
        0..=53 => encode_control_tick(out, 0, index as u32),
        COMPUTE_RUNTIME_CLASS1_RECORD_INDEX => encode_control_registration(
            out,
            0,
            1,
            54,
            COMPUTE_RUNTIME_SUPPORT_ADDRESS,
            COMPUTE_RUNTIME_CLASS1_TABLE_ADDRESS,
            0x480,
            0x18,
            0,
        ),
        55..=56 => encode_control_tick(out, 0, index as u32 - 1),
        COMPUTE_RUNTIME_CLASS2_RECORD_INDEX => encode_control_registration(
            out,
            0,
            2,
            56,
            COMPUTE_RUNTIME_SUPPORT_ADDRESS,
            COMPUTE_FLIST_RUN_TABLE_ADDRESS,
            0x5c0,
            0x28,
            0,
        ),
        58..=64 => encode_control_tick(out, 0, index as u32 - 2),
        _ => return Err(InitdataError::BufferSize),
    }
    Ok(())
}

fn encode_compute_runtime_support(
    out: &mut [u8],
    control_class: u32,
    low_buffer: u64,
    operand_table: u64,
    resource_class: u64,
    cursor: u32,
    final_kind: u32,
) -> Result<(), InitdataError> {
    if out.len() != COMPUTE_RUNTIME_SUPPORT_SIZE {
        return Err(InitdataError::BufferSize);
    }
    out.fill(0);
    put_u32(out, 0x00, control_class);
    put_u32(out, 0x10, control_class);
    put_u64(out, 0x14, low_buffer);
    put_u32(out, 0x1c, 0x4_0000);
    let resource = resource_class << 40;
    put_u64(out, 0x20, resource);
    put_u64(out, 0x28, resource);
    put_u64(out, 0x30, operand_table);
    put_u32(out, 0x40, 4);
    put_u32(out, 0x48, cursor);
    put_u64(out, 0x4c, COMPUTE_RUNTIME_STATE_ADDRESS);
    put_u32(out, 0x60, final_kind);
    Ok(())
}

pub(crate) fn encode_compute_runtime_class1_support(
    out: &mut [u8],
) -> Result<(), InitdataError> {
    encode_compute_runtime_support(
        out,
        1,
        COMPUTE_RUNTIME_PAGE_LIST_ADDRESS,
        COMPUTE_RUNTIME_CLASS1_TABLE_ADDRESS,
        0x12,
        0x90,
        2,
    )
}

pub(crate) fn encode_compute_runtime_class2_support(
    out: &mut [u8],
) -> Result<(), InitdataError> {
    encode_compute_runtime_support(out, 2, COMPUTE_FLIST_PAGE_LIST_ADDRESS, COMPUTE_FLIST_RUN_TABLE_ADDRESS, 0x17, 0xb8, 3)
}

pub(crate) fn encode_compute_runtime_class1_table(
    out: &mut [u8],
) -> Result<(), InitdataError> {
    if out.len() != COMPUTE_RUNTIME_CLASS1_TABLE_SIZE {
        return Err(InitdataError::BufferSize);
    }
    out.fill(0);
    for (index, address) in COMPUTE_RUNTIME_BUFFER_ADDRESSES.into_iter().enumerate() {
        put_u64(
            out,
            index * CONTROL_OPERAND_ENTRY_STRIDE,
            address | CONTROL_OPERAND_ENTRY_FLAG,
        );
    }
    Ok(())
}

pub(crate) fn encode_compute_runtime_page_inventory(
    out: &mut [u8],
) -> Result<(), InitdataError> {
    if out.len() != COMPUTE_RUNTIME_PAGE_LIST_SIZE {
        return Err(InitdataError::BufferSize);
    }
    out.fill(0);
    let mut index = 0usize;
    for base in COMPUTE_RUNTIME_BUFFER_ADDRESSES {
        for offset in (0..CONTROL_OPERAND_BUFFER_SIZE).step_by(0x1000) {
            put_u64(out, index * 8, base + offset);
            index += 1;
        }
    }
    if index * 8 != COMPUTE_RUNTIME_PAGE_LIST_SIZE {
        return Err(InitdataError::BufferSize);
    }
    Ok(())
}

/// Populate the 21-block pool advertised by the compute UMA descriptor.
/// Like render's initial page list, this contains 4-KiB addresses, not compact
/// run descriptors. Publishing nonzero UMA counters over a zero-filled list
/// makes the allocator return address zero on its first consumption.
pub(crate) fn encode_compute_flist_page_list(out: &mut [u8]) -> Result<(), InitdataError> {
    let pages_per_buffer = (CONTROL_OPERAND_BUFFER_SIZE / 0x1000) as usize;
    let count = COMPUTE_READINESS_OPERAND_ENTRY_COUNT * pages_per_buffer;
    if out.len() < count * 8 {
        return Err(InitdataError::BufferSize);
    }
    out.fill(0);
    for buffer in 0..COMPUTE_READINESS_OPERAND_ENTRY_COUNT {
        let base = COMPUTE_FLIST_BUFFER_BASE + buffer as u64 * CONTROL_OPERAND_BUFFER_STRIDE;
        for page in 0..pages_per_buffer {
            put_u64(out, (buffer * pages_per_buffer + page) * 8, base + page as u64 * 0x1000);
        }
    }
    Ok(())
}

pub(crate) fn encode_compute_dispatch_record(
    out: &mut [u8],
) -> Result<(), InitdataError> {
    if out.len() != COMPUTE_DISPATCH_RECORD_SIZE {
        return Err(InitdataError::BufferSize);
    }
    out.fill(0);
    // halLoadUMADesc packs the page-list VA in units of 128 bytes below
    // unit_pages << 41. The native literal names render's unrelocated pool;
    // slot 1 must instead name the independently backed compute page list.
    put_u64(out, 0x00, (0x4_0000u64 << 41) | (COMPUTE_FLIST_PAGE_LIST_ADDRESS >> 7));
    put_u64(out, 0x08, 0x1500u64 << 33);
    put_u64(out, 0x10, 0x1500);
    Ok(())
}

/// Encode the generic shared object named by descriptor tails and later
/// compute-control records.
pub(crate) fn encode_control_shared(out: &mut [u8]) -> Result<(), InitdataError> {
    if out.len() != CONTROL_SHARED_OBJECT_SIZE {
        return Err(InitdataError::BufferSize);
    }
    out.fill(0);
    for (offset, value) in [
        (0x00, 0x0000_0000_0000_0001),
        (0x10, 0x0000_0000_0000_0001),
        (0x18, 0x0004_0000_0000_0070),
        (0x20, 0x0000_1100_0000_0000),
        (0x28, 0x0000_1100_0000_0000),
        (0x30, CONTROL_OPERAND_TABLE_ADDRESS),
        (0x40, 0x0000_0000_0000_0004),
        (0x60, 0x0000_0000_0000_0002),
    ] {
        put_u64(out, offset, value);
    }
    put_u32(out, CONTROL_SHARED_CURSOR_OFFSET, CONTROL_SHARED_CURSOR_BEFORE);
    put_u64(out, 0x4c, CONTROL_SHARED_INNER_ADDRESS);
    Ok(())
}

/// Encode the distinct first-partial support object before opcode 0x20.
pub(crate) fn encode_partial_opening_control_shared(
    out: &mut [u8],
) -> Result<(), InitdataError> {
    if out.len() != CONTROL_SHARED_OBJECT_SIZE {
        return Err(InitdataError::BufferSize);
    }
    out.fill(0);
    // AGXUSCPrivMemFList::populateInitialFirmwareState seeds this hardware
    // buffer id with -1, but AGXUSCPrivMemPool::prepareLocked replaces it with
    // the allocated id before publishing the first tag-15 record. Linux owns
    // the cold-opening slot zero statically, so encode the post-allocation
    // state that the firmware actually receives.
    put_u32(
        out,
        0x08,
        PARTIAL_OPENING_FREELIST_HARDWARE_BUFFER_ID,
    );
    for (offset, value) in [
        (0x00, 0x0000_0000_0000_0001),
        (0x10, 0x0000_0000_0000_0002),
        (0x18, 0x0004_0000_0000_0070),
        // The complete first-render trace has 17 initial one-MiB blocks:
        // 17 MiB / 4 KiB = 0x1100 pages. Opcode 0x20 appends five blocks.
        (0x20, 0x0000_1100_0000_0000),
        (0x28, 0x0000_1100_0000_0000),
        (0x30, CONTROL_OPERAND_TABLE_ADDRESS),
        (0x40, 0x0000_0000_0000_0004),
        (0x60, 0x0000_0000_0000_0003),
    ] {
        put_u64(out, offset, value);
    }
    put_u32(
        out,
        CONTROL_SHARED_CURSOR_OFFSET,
        PARTIAL_OPENING_CONTROL_SHARED_CURSOR_BEFORE,
    );
    put_u64(
        out,
        0x4c,
        PARTIAL_OPENING_CONTROL_SHARED_INNER_ADDRESS,
    );
    Ok(())
}

/// Encode the live render FList state at the native c0830000 owner address.
///
/// The page-pool fields are the same prepared 17-block plus first-grow state
/// used by the first-partial encoder, but +0 is a pointer to the distinct
/// c01608000 inner object. Valid cold opcode-0x20, both render tag-15 records,
/// and descriptor +0x21ce all name this one object; c0828000 remains the
/// separate compact compute/first-partial state.
pub(crate) fn encode_render_control_shared(out: &mut [u8]) -> Result<(), InitdataError> {
    encode_partial_opening_control_shared(out)?;
    put_u64(out, 0x00, CONTROL_SHARED_INNER_ADDRESS);
    put_u64(out, 0x4c, CONTROL_SHARED_INNER_ADDRESS);
    Ok(())
}

/// Encode the hardware-proven compute form of the compact class-2 object.
///
/// This is deliberately separate from the render UMA form above. Compute's
/// successful opening uses 21 compact consecutive run descriptors, a cursor of
/// 0x18 and the original unallocated HardwareBufferID sentinel. Substituting
/// render's 22-block plus asynchronous-grow state makes compute retire without
/// executing.
pub(crate) fn encode_compute_partial_opening_control_shared(
    out: &mut [u8],
) -> Result<(), InitdataError> {
    if out.len() != CONTROL_SHARED_OBJECT_SIZE {
        return Err(InitdataError::BufferSize);
    }
    out.fill(0);
    put_u32(out, 0x08, u32::MAX);
    for (offset, value) in [
        (0x00, 0x0000_0000_0000_0001),
        (0x10, 0x0000_0000_0000_0002),
        (0x18, 0x0004_0000_0000_0070),
        (0x20, 0x0000_1900_0000_0000),
        (0x28, 0x0000_1900_0000_0000),
        (0x30, COMPUTE_FLIST_RUN_TABLE_ADDRESS),
        (0x40, 0x0000_0000_0000_0004),
        (0x60, 0x0000_0000_0000_0003),
    ] {
        put_u64(out, offset, value);
    }
    put_u64(out, 0x14, COMPUTE_FLIST_PAGE_LIST_ADDRESS);
    put_u32(
        out,
        CONTROL_SHARED_CURSOR_OFFSET,
        COMPUTE_PARTIAL_OPENING_CONTROL_SHARED_CURSOR_BEFORE,
    );
    put_u64(
        out,
        0x4c,
        PARTIAL_OPENING_CONTROL_SHARED_INNER_ADDRESS,
    );
    Ok(())
}

/// Keep the first-partial operand page blank through its opening and first
/// control-done notification. The host publishes the 28-entry form before
/// making either first-work producer visible.
pub(crate) fn encode_partial_opening_operand_table_pre_control(
    out: &mut [u8],
) -> Result<(), InitdataError> {
    if out.len() != CONTROL_OPERAND_TABLE_SIZE {
        return Err(InitdataError::BufferSize);
    }
    out.fill(0);
    Ok(())
}

pub(crate) fn encode_partial_opening_operand_table_post_control(
    out: &mut [u8],
) -> Result<(), InitdataError> {
    if out.len() != CONTROL_OPERAND_TABLE_SIZE {
        return Err(InitdataError::BufferSize);
    }
    out.fill(0);
    for index in 0..PARTIAL_OPENING_CONTROL_OPERAND_ENTRY_COUNT {
        let address = CONTROL_OPERAND_BUFFER_BASE
            .checked_add(index as u64 * CONTROL_OPERAND_BUFFER_STRIDE)
            .ok_or(InitdataError::AddressOutOfRange)?;
        put_u64(
            out,
            index * CONTROL_OPERAND_ENTRY_STRIDE,
            address | CONTROL_OPERAND_ENTRY_FLAG,
        );
    }
    Ok(())
}

/// Encode the 22 flagged buffer pointers used by the later compute tail.
pub(crate) fn encode_control_operand_table(out: &mut [u8]) -> Result<(), InitdataError> {
    if out.len() != CONTROL_OPERAND_TABLE_SIZE {
        return Err(InitdataError::BufferSize);
    }
    out.fill(0);
    for index in 0..CONTROL_OPERAND_ENTRY_COUNT {
        let address = COMPUTE_FLIST_BUFFER_BASE
            .checked_add(index as u64 * CONTROL_OPERAND_BUFFER_STRIDE)
            .ok_or(InitdataError::AddressOutOfRange)?;
        put_u64(
            out,
            index * CONTROL_OPERAND_ENTRY_STRIDE,
            address | CONTROL_OPERAND_ENTRY_FLAG,
        );
    }
    Ok(())
}

/// Bundle offsets of the five views named at main `+0x254`. They are views of
/// the common bundle, not independent objects: view 0 overlaps the tail of
/// the hardware-data object, and views 3 and 4 overlap each other by all but
/// 0x80 bytes.
pub(crate) const BUNDLE_VIEW_OFFSETS: [usize; 5] = [0x2740, 0x3380, 0x4400, 0xbc80, 0xbd00];
/// Logical extents of the five views.
pub(crate) const BUNDLE_VIEW_EXTENTS: [usize; 5] = [0x18c0, 0x0c80, 0x3c00, 0x3380, 0x3300];

/// The context-0 mappings of the PB descriptor and UMA page-pool descriptor
/// tables published at main `+0x2d8` and `+0x2e8`, respectively. The matching
/// firmware-high mappings are published at `+0x2e0` and `+0x2f0`.
pub(crate) const REGION_VIEW_LOW_ADDRS: [u64; 2] = [0x70_0183_8000, 0x70_0184_0000];

// --- Channel table ---------------------------------------------------------------

pub(crate) const CHANNEL_TABLE_ENTRIES: usize = 17;
pub(crate) const CHANNEL_ENTRY_SIZE: usize = 0x20;
/// Channel entry layout: three *separate* 32-bit state-counter addresses at
/// `+0x00`/`+0x08`/`+0x10` (the counters themselves sit at 0x10 spacing in
/// their state block — do not collapse them into one state object), then the
/// ring base at `+0x18`.
pub(crate) const CHANNEL_ENTRY_STATE_COUNT: usize = 3;
pub(crate) const CHANNEL_STATE_SPACING: usize = 0x10;
pub(crate) const CHANNEL_STATE_STRIDE: usize = 0x40;
/// For a work channel, state 0 is the consumer and state 2 the producer.
pub(crate) const CHANNEL_STATE_CONSUMER: usize = 0;
pub(crate) const CHANNEL_STATE_PRODUCER: usize = 2;
/// Entries 0..11 are ordered TA_0, 3D_0, CL_0, TA_1, ... CL_3.
pub(crate) const WORK_CHANNEL_COUNT: usize = 12;
/// Entry 12 is device control. It continues the compact control-state grid
/// ([`WORK_STATE_GRID_OFFSETS`] has 13 entries, one more than there are work
/// channels), and its ring is embedded at main `+0x4c0`. The M5-derived model
/// refused this index, claiming a "cursor pair at +0x1a0/+0x1a8"; that was a
/// misread — those offsets are exactly this entry's state-counter addresses 0
/// and 1.
pub(crate) const CONTROL_CHANNEL_INDEX: usize = 12;
/// Entries 13 and 14 are report channels with split state objects: their
/// state addresses alias off status A rather than the compact grid, and the
/// firmware peer of each host-owned counter is at `+0x20` within its object.
pub(crate) const REPORT_CHANNEL_INDICES: [usize; 2] = [13, 14];
/// Entry 15 is partial: only its first state address, no ring. Entry 16 is
/// empty. Filling them like the rest declares channels that do not exist.
pub(crate) const PARTIAL_CHANNEL_INDEX: usize = 15;

/// Offsets of the channel state blocks (channels 0..=12) inside the compact
/// state grid. Native state is *not* in channel-table order: within each
/// workload group the work queues sit at descending offsets, and the control
/// channel's block follows at `+0x300`. Preserving these address identities is
/// part of the descriptor ABI.
pub(crate) const WORK_STATE_GRID_OFFSETS: [usize; 13] = [
    0x0c0, 0x1c0, 0x2c0, // TA_0, 3D_0, CL_0
    0x080, 0x180, 0x280, // TA_1, 3D_1, CL_1
    0x040, 0x140, 0x240, // TA_2, 3D_2, CL_2
    0x000, 0x100, 0x200, // TA_3, 3D_3, CL_3
    0x300, // device control
];
/// The primary root's status-B pointer is the word immediately after that
/// compact grid: `status_b == state_grid + 0x340` in the native layout.
pub(crate) const PRIMARY_STATUS_B_STATE_GRID_OFFSET: usize = 0x340;
/// The primary status-B object runs from there to status A: `0xeb00` bytes.
pub(crate) const PRIMARY_STATUS_B_OBJECT_SIZE: usize = 0xeb00;

/// State-counter addresses of report channels 13/14, as offsets from status A.
pub(crate) const REPORT_STATE_STATUS_A_OFFSETS: [[usize; 3]; 2] =
    [[0x00040, 0x002c0, 0x00080], [0x00240, 0xa6ac0, 0x00280]];
/// Ring bases of report channels 13/14, as offsets from status A.
pub(crate) const REPORT_RING_STATUS_A_OFFSETS: [usize; 2] = [0x04ac0, 0xafac0];
/// Entry 15's sole state address, as an offset from status A.
pub(crate) const PARTIAL_STATE_STATUS_A_OFFSET: usize = 0x2d2c0;

/// Native bundle offsets of the twelve work rings (0x1800 each), in
/// channel-table order. These extend past [`HW_DATA_BUNDLE_MIN_ALLOC`]; a host
/// using this placement sizes the cluster [`NATIVE_SHARED_CLUSTER_SIZE`].
pub(crate) const WORK_RING_BUNDLE_OFFSETS: [usize; 12] = [
    0x10dc0, 0x16dc0, 0x1cdc0, 0x0f5c0, 0x155c0, 0x1b5c0, 0x0ddc0, 0x13dc0, 0x19dc0, 0x0c5c0,
    0x125c0, 0x185c0,
];
/// Native bundle offsets of the two main configuration objects.
pub(crate) const NATIVE_PRIMARY_MAIN_BUNDLE_OFFSET: usize = 0x1e5c0;
pub(crate) const NATIVE_SECONDARY_MAIN_BUNDLE_OFFSET: usize = 0x22a80;

// --- Hardware-data object ---------------------------------------------------------

/// Register-mapping array: 53 slots of 0x28 bytes at `+0x640`. Slots may also
/// carry only the flag at `+0x20` (declared but unmapped). Register windows
/// come from the ADT; do not replace ADT-defined sizes or addresses.
/// The five qwords `AGXFirmware::initFirmwareData` writes at the head of the
/// hardware-data object: `{range-3 base, range-3 size, GART window base, GART
/// window base, reserved top-of-range-5 page}`.
pub(crate) const HW_FIRMWARE_RANGE_HEAD: &[(usize, u64)] = &[
    (0x00, 0x0000_006f_0000_0000),
    (0x08, 0x0000_0000_ffc0_0000),
    (0x10, 0x0000_0010_0000_0000),
    (0x18, 0x0000_0010_0000_0000),
    (0x20, 0x0000_02ff_ffff_8000),
];

pub(crate) const HW_REGISTER_MAP_OFFSET: usize = 0x640;
pub(crate) const HW_REGISTER_MAP_ENTRY_SIZE: usize = 0x28;
pub(crate) const HW_REGISTER_MAP_SLOTS: usize = 53;
/// FW device-VA aperture the register windows live in.
pub(crate) const HW_REGISTER_APERTURE: Range<u64> = 0xffff_fc21_8000_0000..0xffff_fc21_9000_0000;

/// `+0xe90`: u32 chip identifier.
pub(crate) const HW_CHIP_ID_OFFSET: usize = 0xe90;
pub(crate) const HW_CHIP_ID_T8140: u32 = 0x8140;

// The acceptance block at +0x258e. Bisecting by zeroing showed every *other*
// unexplained hardware-data byte can be zero, so this is the part that has to
// be right for the descriptor to be accepted. It decodes as the work-channel
// count, two {0,1,1,1} flag groups, one 0xffffffff "unassigned" word per work
// channel, and three trailing ones.
pub(crate) const HW_ACCEPT_CHANNEL_COUNT: usize = 0x258e; // u16 = 12
pub(crate) const HW_ACCEPT_FLAG_GROUPS: [usize; 2] = [0x2590, 0x25a0];
pub(crate) const HW_ACCEPT_FLAG_PATTERN: [u32; 4] = [0, 1, 1, 1];
pub(crate) const HW_ACCEPT_UNASSIGNED_TABLE: usize = 0x25b4; // 12 x u32 0xffffffff
pub(crate) const HW_ACCEPT_TRAILING_ONES: [usize; 3] = [0x25f4, 0x2600, 0x2608];
/// `+0x2630` is firmware state, not host configuration (the one byte that
/// differed between a pre-start and a running capture). It must start zero.
pub(crate) const HW_FIRMWARE_STATE_OFFSET: usize = 0x2630;

/// Four inline G17P KSM completion descriptors at hardware-data `+0x2614`.
/// Only ordinals 0 and 2 are present. Each names low/high mappings of one
/// retained `0x800 * 0x40 == 0x20000` backing; ordinals 1 and 3 stay zero.
pub(crate) const G17P_KSM_COMPLETION_TABLE_OFFSET: usize = 0x2614;
pub(crate) const G17P_KSM_COMPLETION_DESCRIPTOR_STRIDE: usize = 0x20;
pub(crate) const G17P_KSM_COMPLETION_PRESENT_ORDINALS: [u32; 2] = [0, 2];
pub(crate) const G17P_KSM_COMPLETION_CAPACITY: u32 = 0x800;
pub(crate) const G17P_KSM_COMPLETION_ENTRY_SIZE: u32 = 0x40;
pub(crate) const G17P_KSM_COMPLETION_BACKING_SIZE: usize =
    G17P_KSM_COMPLETION_CAPACITY as usize * G17P_KSM_COMPLETION_ENTRY_SIZE as usize;
pub(crate) const G17P_KSM_COMPLETION_LOW_VAS: [u64; 2] =
    [0x70_03f0_0000, 0x70_03f2_8000];
pub(crate) const G17P_KSM_COMPLETION_HIGH_BUNDLE_OFFSETS: [usize; 2] = [0x48000, 0x70000];
const G17P_KSM_COMPLETION_TABLE_PREFIX_OFFSET: usize = 0x2610;
const G17P_KSM_COMPLETION_TABLE_PREFIX: u32 = 0x100;

/// Two GPU mappings of one present completion-queue backing.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) struct G17PKsmCompletionAliases {
    pub(crate) low: u64,
    pub(crate) high: u64,
}

// Performance-table geometry (values deliberately not encoded; see
// [`PERF_TABLE_VALUES_ENCODED`]). Two groups with identical internal layout,
// one per 11-entry ladder: frequency ladder, then a core-voltage column 0x40
// on, then a memory-voltage column 0x440 on, stored as 0x40-stride per-state
// blocks. Group 0 repeats each per-state value across 16 words; group 1
// stores it once.
pub(crate) const HW_PERF_GROUP_BASES: [usize; 2] = [0xfc8, 0x1cdc];
pub(crate) const HW_PERF_GROUP_REPEATS: [usize; 2] = [16, 1];
pub(crate) const HW_PERF_LADDER_ENTRIES: usize = 11;
pub(crate) const HW_PERF_VOLTAGE_DELTA: usize = 0x40;
pub(crate) const HW_PERF_MEMORY_VOLTAGE_DELTA: usize = 0x440;
pub(crate) const HW_PERF_STATE_BLOCK_STRIDE: usize = 0x40;
pub(crate) const HW_PERF_FREQ_LADDER_B: usize = 0x1808;
pub(crate) const HW_PERF_SCALE_LADDER_B: usize = 0x1848;
pub(crate) const HW_PERF_RELATIVE_LADDER_A: usize = 0x18c8;
pub(crate) const HW_PERF_RELATIVE_LADDER_B: usize = 0x1908;
pub(crate) const HW_PERF_INDEX_MAP_A: usize = 0x19c8;
pub(crate) const HW_PERF_INDEX_MAP_B: usize = 0x1a08;
const HW_PERF_FREQ_A: [u32; HW_PERF_LADDER_ENTRIES] =
    [0, 338, 492, 618, 796, 928, 1056, 1170, 1278, 1338, 1470];
const HW_PERF_FREQ_B: [u32; HW_PERF_LADDER_ENTRIES] =
    [0, 338, 492, 618, 796, 928, 952, 1053, 1152, 1204, 1326];
const HW_PERF_SCALE_B: [u32; HW_PERF_LADDER_ENTRIES] = [1_065_520_988; 11];
/// Relative-performance ladder A. Native's live block reads
/// 0f 16 1d 2a 38 4a 56 at +0x18cc (states 1..7) = 15 22 29 42 56 74 86,
/// against our 16 23 30 43 57 74 85 -- off by one nearly everywhere. States
/// 8..10 are not covered by that read and keep our values.
const HW_PERF_RELATIVE_A_VALUES: [u32; HW_PERF_LADDER_ENTRIES] =
    [0, 15, 22, 29, 42, 56, 74, 86, 90, 92, 100];
const HW_PERF_RELATIVE_B_VALUES: [u32; HW_PERF_LADDER_ENTRIES] =
    [0, 0, 13, 24, 40, 52, 63, 73, 83, 88, 100];
const HW_PERF_INDEX_A_VALUES: [u32; HW_PERF_LADDER_ENTRIES] =
    [0, 1, 2, 3, 4, 5, 7, 9, 11, 13, 15];
const HW_PERF_INDEX_B_VALUES: [u32; HW_PERF_LADDER_ENTRIES] =
    [0, 1, 2, 3, 4, 5, 6, 8, 10, 12, 14];
/// Per-perf-state GPU core voltage, in mV.
///
/// MEASURED against native's live hardware-data block (25G83, hwdata.d reading
/// *(AGXArmFirmware+0x2b8)): native carries
///     605 630 650 695 745 800 845 885 905 975
/// for states 1..10, and this driver derived
///     595 615 635 685 730 780 815 865 885 955
/// -- systematically LOW at every state, by 10 to 30 mV. Both machines run the
/// same ADT, so this is our derivation and not chip binning.
///
/// This is not cosmetic. The register this tree calls "GPC core power",
/// sgx+0xe01000 & 0xf, is G16::AGXFenderCommon::getGPCPerformanceState -- the
/// GPU PERFORMANCE state, where 0 means off. The firmware selects a state from
/// exactly these tables and raises the GPC to it, so a table it judges invalid
/// or below its floor is a direct mechanism for "firmware runs, tables loaded,
/// GPC stays 0, nothing ever executes".
const HW_PERF_CORE_VOLTAGE: [u32; HW_PERF_LADDER_ENTRIES] =
    [125, 605, 630, 650, 695, 745, 800, 845, 885, 905, 975];
/// Per-perf-state SRAM/memory-rail voltage, in mV.
///
/// Same defect as the core table: our tail tracked our own (wrong) core values.
/// Native's live block carries 765 x6 then 800 845 885 905 975, which is the
/// ADT perf-states-sram column mapped through HW_PERF_INDEX_A. The correct
/// derivation is memory[i] = ADT perf-states-sram[INDEX_A[i]].volt, not "the
/// memory tail tracks core".
const HW_PERF_MEMORY_VOLTAGE: [u32; HW_PERF_LADDER_ENTRIES] =
    [765, 765, 765, 765, 765, 765, 800, 845, 885, 905, 975];

// --- Region C ---------------------------------------------------------------------

/// Byte immediately preceding the unaligned host power-assert counter.
pub(crate) const REGION_C_REQUIRED_OFFSET: usize = 0xe50;
pub(crate) const REGION_C_REQUIRED_VALUE: u32 = 0;
/// Unaligned host-owned GPU-core power-assert counter.
///
/// FW_PROVEN (b1 `0x3aa8`/`0x3b20`): the firmware reads this **unaligned** u32
/// and compares it with its own tally `status_b[0xe438]`, which it increments
/// once per drained record whose opcode flag byte (`__const 0x45430 + opcode`)
/// is non-zero. When the GPU is unpowered and the two disagree, `0x3b2c` calls
/// `0x21960` and powers the cores on before draining. Equal values mean "no
/// unaccounted power-asserting work", so a value pinned forever at 1 — the old
/// static initializer — can never trigger that arm after the first flagged
/// record.
///
/// Was `0xe55`, which is off by four: that offset is read nowhere in b1.
/// The flag byte is `0` (i.e. no power assert) for opcodes `0x00`, `0x0e`,
/// `0x15`, `0x16`, `0x1b`, `0x22`, `0x2a`, `0x2e` and `0x3a..=0x3f`; note that
/// includes `0x2e`, contradicting the older comment here that named it as a
/// core-power claimant.
pub(crate) const REGION_C_GPU_CORE_PWR_ASSERT_COUNTER_OFFSET: usize = 0xe51;
/// Native cold value before the host publishes its first flag-1 control.
pub(crate) const REGION_C_GPU_CORE_PWR_ASSERT_COUNT: u32 = 0;

/// Whether a device-control opcode advances the host GPU-core power-assert
/// counter. The 64-entry table is exhaustive in B1; values outside it are not
/// valid device-control opcodes.
pub(crate) const fn device_control_asserts_gpu_power(opcode: u32) -> Option<bool> {
    if opcode > 0x3f {
        return None;
    }
    Some(!matches!(
        opcode,
        0x00 | 0x0e | 0x15 | 0x16 | 0x1b | 0x22 | 0x2a | 0x2e | 0x3a..=0x3f
    ))
}

// --- Status blocks -----------------------------------------------------------------

/// Status block `+0x04` holds u32 1 pre-init. `+0x10`/`+0x14` are *post-ack
/// firmware state* and must be zero at handoff.
pub(crate) const STATUS_BLOCK_ONE_OFFSET: usize = 0x04;
pub(crate) const STATUS_BLOCK_POST_ACK_OFFSETS: [usize; 2] = [0x10, 0x14];
pub(crate) const PRIMARY_STATUS_B_FWCTL_STATE: usize = 0x48e0;
pub(crate) const PRIMARY_STATUS_B_FWCTL_RING: usize = 0x48e8;
pub(crate) const PRIMARY_STATUS_B_CONFIG_HEADER: usize = 0xe434;
pub(crate) const PRIMARY_STATUS_B_CONFIG_OFFSET: usize = 0xe440;

// --- Instance separation -----------------------------------------------------------

/// The secondary root sits exactly this far above the primary root; the
/// secondary's initialisation message carries the primary's address plus this
/// delta in the same field, so the pair is not placed freely.
pub(crate) const SECONDARY_ROOT_DELTA: u64 = 0x8000;
/// Each instance maps the ADT shared region from its own half: the secondary
/// maps it from base plus this delta, and walks a top table at the same
/// offset into its half. Mappings installed only in the primary's tables
/// leave the secondary unable to translate anything it is handed.
pub(crate) const SECONDARY_SHARED_REGION_DELTA: u64 = 0x40000;

/// Physical firmware instance encoded at root `+0x28`.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
#[repr(u32)]
pub(crate) enum FirmwareRole {
    Gfx = 0,
    Gfx1 = 1,
}

/// GPU virtual addresses consumed by the grounded subset for one role.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) struct RoleAddresses {
    /// Address of this role's root. It is validated for the 44-bit doorbell.
    pub(crate) root: u64,
    pub(crate) runtime_pointers: u64,
    pub(crate) hw_globals: u64,
    /// Shared firmware allocation. Zero is allowed because only GFX1 reads it.
    pub(crate) firmware_shared_allocation: u64,
    pub(crate) shared: u64,
    pub(crate) role: u64,
    pub(crate) cross_role: u64,
    /// Proven only for GFX1 and required there; must be zero for GFX.
    pub(crate) cross_role_2: u64,
    /// Role-shared firmware sub-allocations, aligned to
    /// [`RT_SHARED_SUBALLOC_OFFSETS`]. Each must be a real (non-zero) GPU VA
    /// because the firmware unconditionally dereferences these pointers.
    pub(crate) shared_runtime_allocations: [u64; RT_SHARED_SUBALLOC_OFFSETS.len()],
    /// The GFX1-only runtime buffer at `runtime_pointers + 0x481`. Required
    /// (non-zero) for `Gfx1`; must be zero for `Gfx`.
    pub(crate) gfx1_runtime_buffer: u64,
}

/// Fail-closed validation errors for the grounded layout subset.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) enum InitdataError {
    BufferSize,
    AddressOutOfRange,
    NullRequiredAddress,
    RoleSpecificAddress,
    PairSharedPointerMismatch,
    PairAliasedRoot,
    PairAliasedRuntimePointers,
    PerfStateCount,
    InterfaceMismatch,
    HostMappedAllocationsDisabled,
}

/// Firmware allocations whose sizes and runtime-pointer destinations are
/// recovered exactly. Their contents are not implemented by this module.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) struct FirmwareAllocation {
    pub(crate) runtime_pointer_offset: usize,
    pub(crate) size: usize,
    pub(crate) role_only: Option<FirmwareRole>,
}

pub(crate) const FIRMWARE_ALLOCATIONS: [FirmwareAllocation; 8] = [
    FirmwareAllocation {
        runtime_pointer_offset: 0x000,
        size: HW_GLOBALS_SIZE,
        role_only: None,
    },
    FirmwareAllocation {
        runtime_pointer_offset: 0x254,
        size: 0x0c18,
        role_only: None,
    },
    FirmwareAllocation {
        runtime_pointer_offset: 0x25c,
        size: 0x1048,
        role_only: None,
    },
    FirmwareAllocation {
        runtime_pointer_offset: 0x264,
        size: 0x0e10,
        role_only: None,
    },
    FirmwareAllocation {
        runtime_pointer_offset: 0x481,
        size: 0x12688,
        role_only: Some(FirmwareRole::Gfx1),
    },
    FirmwareAllocation {
        runtime_pointer_offset: 0x26c,
        size: 0x0068,
        role_only: None,
    },
    FirmwareAllocation {
        runtime_pointer_offset: 0x274,
        size: 0x0800,
        role_only: None,
    },
    FirmwareAllocation {
        runtime_pointer_offset: 0x010,
        size: 0x0088,
        role_only: None,
    },
];

fn address_representable(address: u64) -> bool {
    let high = address & !INITDATA_ADDR_MASK;
    high == 0 || high == INITDATA_ADDR_PREFIX
}

fn required_address(address: u64) -> Result<(), InitdataError> {
    if address == 0 {
        Err(InitdataError::NullRequiredAddress)
    } else if !address_representable(address) {
        Err(InitdataError::AddressOutOfRange)
    } else {
        Ok(())
    }
}

fn optional_address(address: u64) -> Result<(), InitdataError> {
    if address == 0 || address_representable(address) {
        Ok(())
    } else {
        Err(InitdataError::AddressOutOfRange)
    }
}

fn put_u32(bytes: &mut [u8], offset: usize, value: u32) {
    bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

fn put_u64(bytes: &mut [u8], offset: usize, value: u64) {
    bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}

fn get_u32(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
}

fn get_u64(bytes: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(bytes[offset..offset + 8].try_into().unwrap())
}

/// Validate the cross-role invariants established before either ASC is kicked.
pub(crate) fn validate_pair(addresses: &[RoleAddresses; 2]) -> Result<(), InitdataError> {
    if addresses[0].shared != addresses[1].shared {
        return Err(InitdataError::PairSharedPointerMismatch);
    }
    if addresses[0].root == addresses[1].root {
        return Err(InitdataError::PairAliasedRoot);
    }
    if addresses[0].runtime_pointers == addresses[1].runtime_pointers {
        return Err(InitdataError::PairAliasedRuntimePointers);
    }
    Ok(())
}

/// Encode the grounded subset for one G17 firmware role.
///
/// Every output is zeroed first. This makes unknown bytes deterministic and,
/// critically, prevents stale host memory from becoming invented firmware ABI.
pub(crate) fn encode_grounded_subset(
    role: FirmwareRole,
    addresses: &RoleAddresses,
    perf_state_clock_mhz: &[u32],
    root: &mut [u8],
    runtime_pointers: &mut [u8],
    hw_globals: &mut [u8],
) -> Result<(), InitdataError> {
    if root.len() != ROOT_SIZE
        || runtime_pointers.len() != RUNTIME_POINTERS_SIZE
        || hw_globals.len() != HW_GLOBALS_SIZE
    {
        return Err(InitdataError::BufferSize);
    }
    if perf_state_clock_mhz.is_empty() || perf_state_clock_mhz.len() > PERF_STATE_MAX {
        return Err(InitdataError::PerfStateCount);
    }

    for address in [
        addresses.root,
        addresses.runtime_pointers,
        addresses.hw_globals,
        addresses.shared,
        addresses.role,
        addresses.cross_role,
    ] {
        required_address(address)?;
    }
    optional_address(addresses.firmware_shared_allocation)?;

    // The firmware unconditionally follows every role-shared sub-allocation
    // pointer, so a null there is a fail-closed error, not an optional field.
    for address in addresses.shared_runtime_allocations {
        required_address(address)?;
    }

    match role {
        FirmwareRole::Gfx if addresses.cross_role_2 != 0 || addresses.gfx1_runtime_buffer != 0 => {
            return Err(InitdataError::RoleSpecificAddress);
        }
        FirmwareRole::Gfx1 => {
            required_address(addresses.cross_role_2)?;
            required_address(addresses.gfx1_runtime_buffer)?;
        }
        FirmwareRole::Gfx => {}
    }

    root.fill(0);
    runtime_pointers.fill(0);
    hw_globals.fill(0);

    put_u64(root, 0x00, INITDATA_MAGIC);
    put_u64(
        root,
        ROOT_FW_SHARED_ALLOC,
        addresses.firmware_shared_allocation,
    );
    put_u64(root, ROOT_RUNTIME_POINTERS, addresses.runtime_pointers);
    put_u64(root, ROOT_SHARED, addresses.shared);
    put_u32(root, ROOT_ROLE, role as u32);
    put_u32(root, ROOT_HOST_MAPPED_ALLOCS, 1);
    put_u64(root, ROOT_ROLE_POINTER, addresses.role);
    match role {
        FirmwareRole::Gfx => put_u64(root, ROOT_ROLE0_CROSS, addresses.cross_role),
        FirmwareRole::Gfx1 => {
            put_u64(root, ROOT_ROLE1_CROSS, addresses.cross_role);
            put_u64(root, ROOT_ROLE1_CROSS_2, addresses.cross_role_2);
        }
    }

    put_u64(runtime_pointers, RT_HW_GLOBALS, addresses.hw_globals);
    put_u64(runtime_pointers, RT_ROLE_POINTER, addresses.role);
    for (offset, address) in RT_SHARED_SUBALLOC_OFFSETS
        .iter()
        .copied()
        .zip(addresses.shared_runtime_allocations)
    {
        put_u64(runtime_pointers, offset, address);
    }
    if role == FirmwareRole::Gfx1 {
        // Unaligned store at +0x481, matching the host builder.
        put_u64(runtime_pointers, RT_GFX1_RUNTIME_BUFFER, addresses.gfx1_runtime_buffer);
    }

    put_u32(
        hw_globals,
        HW_PERF_STATE0_CLOCK_MHZ,
        perf_state_clock_mhz[0],
    );
    for (index, clock) in perf_state_clock_mhz.iter().copied().enumerate() {
        put_u32(hw_globals, HW_PERF_STATE_CLOCK_MHZ + index * 4, clock);
    }
    put_u32(
        hw_globals,
        HW_PERF_STATE_COUNT_MINUS_ONE,
        perf_state_clock_mhz.len() as u32 - 1,
    );

    // Recorded platform constants. Descriptor *acceptance* survives without
    // them, but the device-control phase that follows does not. +0x2630 is
    // deliberately absent from the runs: it is firmware state and must start
    // zero (the fill above guarantees it).
    write_runs(out, HW_DATA_OPAQUE_RUNS);
    // The host->firmware transport selector.
    //
    // `AGXFirmware::initFirmwareData` @0xfffffe0008a20940 exports
    // `initdata[0x26f8] = accel[0x4f8] & 1`. That same bit decides, in
    // `AGX{TA,3D,CL}ChannelSKSM::submitBuffer`, whether the host publishes the
    // command record and the tag-14 AddKicks as CHANNEL COMMANDS through
    // `writeChannelCommandPointer` (bit set, `accel[0x10769] & 1 == 0`) or
    // instead pushes onto the host kick-trace ring and drives
    // `configureHardware`/`addKicks` over MMIO (bit clear).
    //
    // A live SKSM capture on this target shows tag-15 and tag-14 published as
    // channel commands for both render queues, so native runs with the bit
    // SET and this byte at 1. This driver uses the same channel-command
    // transport but left the byte at 0 -- it is outside every
    // `HW_DATA_OPAQUE_RUNS` extent and nothing else wrote it -- so the
    // firmware was told the host drives the MMIO bridge while the host in
    // fact speaks channel commands. Declare the transport we actually use.
    // MEASURED 2026-09-02, and it contradicts the reasoning above. Native's
    // complete hardware-data block (hwdata-gfx.txt, captured from
    // accel+0x5b0 -> +0x2b8 at submit3DChannel ENTRY, i.e. long after init)
    // carries ZERO at 0x26f8. The "native runs with the bit SET" conclusion was
    // inferred from seeing channel commands on the wire, not read off the
    // block, and the block disagrees. Publish what native publishes.
    if let Some(byte) = out.get_mut(HW_DATA_TRANSPORT_SELECTOR_OFFSET) {
        *byte = 0;
    }
    // The measured performance tables were encoded above. Their values are
    // target-scoped observations rather than a complete ADT derivation; see
    // [`PERF_TABLE_VALUES_ENCODED`].
    Ok(())
}

/// Encode the shared 0x1000-byte region C. Both roots point at the same
/// object; giving the secondary a private copy is a difference from what a
/// working host does.
pub(crate) fn encode_region_c(out: &mut [u8]) -> Result<(), InitdataError> {
    if out.len() != REGION_C_SIZE {
        return Err(InitdataError::BufferSize);
    }
    out.fill(0);
    write_runs(out, REGION_C_RUNS);
    // regionC+0x9cc is the KSM progress-watchdog threshold for data master 2
    // (compute); the firmware copies it to 0x1778d0 and its handler starts
    // `cbz w20 -> ret`, so 0 disables the watchdog. We ship 0x32 (50) here and
    // leave the other data master's slot at 0 -- i.e. we arm the watchdog on
    // exactly the queue we submit to. When work cannot progress for any other
    // reason this turns a stall into a self-sustaining recovery loop.
    if crate::g17_config::PROGRESS_WATCHDOG == 0 {
        put_u32(out, 0x9cc, 0);
    }
    // Native initializes the idle-descent selector to zero. Keep the old
    // alternate predicate only as an explicit diagnostic override.
    if crate::g17_config::IDLE_SELECTOR != 0 {
        out[0xe21] = 1;
    }
    for (offset, value) in [
        (0x7c, 1),
        (0x80, 1),
        (0x9c, 0x3000_001d),
        (0xa0, 0x3100_0002),
    ] {
        put_u32(out, offset, value);
    }
    Ok(())
}

/// Overlay the target-scoped static third-page bytes and its one live state
/// pointer without disturbing hardware-data or overlapping view content.
pub(crate) fn encode_bundle_static(
    state_ptr: u64,
    out: &mut [u8],
) -> Result<(), InitdataError> {
    if out.len() < HW_DATA_BUNDLE_MIN_ALLOC {
        return Err(InitdataError::BufferSize);
    }
    required_address(state_ptr)?;
    write_runs(out, BUNDLE_STATIC_RUNS);
    for (index, runs) in BUNDLE_VIEW_RUNS.iter().enumerate() {
        let base = BUNDLE_VIEW_OFFSETS[index];
        let valid = BUNDLE_VIEW_EXTENTS[index];
        for (offset, data) in *runs {
            if offset + data.len() > valid {
                continue;
            }
            let start = base + offset;
            out[start..start + data.len()].copy_from_slice(data);
        }
    }
    for (view, offset, count, trailing_ff) in BUNDLE_VIEW_U16_REPEATS {
        let base = BUNDLE_VIEW_OFFSETS[*view] + offset;
        let end = base + *count * 2 + usize::from(*trailing_ff);
        if end > BUNDLE_VIEW_OFFSETS[*view] + BUNDLE_VIEW_EXTENTS[*view] {
            return Err(InitdataError::BundleAllocationTooSmall);
        }
        for index in 0..*count {
            out[base + index * 2..base + index * 2 + 2].copy_from_slice(&[0xff, 0x7f]);
        }
        if *trailing_ff {
            out[base + *count * 2] = 0xff;
        }
    }
    put_u64(out, 0x8ed8, state_ptr);
    Ok(())
}

/// Encode one 0x80-byte status block in its pre-init state: u32 1 at `+0x04`,
/// everything else zero. `+0x10`/`+0x14` are post-acknowledgement firmware
/// state and must be zero at handoff.
pub(crate) fn encode_status_block(out: &mut [u8]) -> Result<(), InitdataError> {
    if out.len() != STATUS_BLOCK_SIZE {
        return Err(InitdataError::BufferSize);
    }
    out.fill(0);
    put_u32(out, STATUS_BLOCK_ONE_OFFSET, 1);
    Ok(())
}

/// Encode the complete pre-init primary status/config object.
pub(crate) fn encode_primary_status_b(
    fwctl_state: u64,
    fwctl_ring: u64,
    out: &mut [u8],
) -> Result<(), InitdataError> {
    if out.len() != PRIMARY_STATUS_B_OBJECT_SIZE {
        return Err(InitdataError::BufferSize);
    }
    required_address(fwctl_state)?;
    required_address(fwctl_ring)?;
    out.fill(0);
    encode_status_block(&mut out[..STATUS_BLOCK_SIZE])?;
    put_u64(out, PRIMARY_STATUS_B_FWCTL_STATE, fwctl_state);
    put_u64(out, PRIMARY_STATUS_B_FWCTL_RING, fwctl_ring);
    put_u32(out, PRIMARY_STATUS_B_CONFIG_HEADER, 1);
    for (offset, data) in PRIMARY_STATUS_B_CONFIG_RUNS {
        let start = PRIMARY_STATUS_B_CONFIG_OFFSET + offset;
        out[start..start + data.len()].copy_from_slice(data);
    }
    Ok(())
}

/// Encode the secondary root's independent `+0xc0` target.
pub(crate) fn encode_secondary_root_extra_1(out: &mut [u8]) -> Result<(), InitdataError> {
    if out.len() != STATUS_BLOCK_SIZE {
        return Err(InitdataError::BufferSize);
    }
    out.fill(0);
    write_runs(out, SECONDARY_ROOT_EXTRA_1_RUNS);
    Ok(())
}

/// Validate the cross-instance invariants established before either ASC is
/// kicked. The two roots are not placed freely: the secondary's sits exactly
/// [`SECONDARY_ROOT_DELTA`] above the primary's.
pub(crate) fn validate_pair(
    primary: &InstanceAddresses,
    secondary: &InstanceAddresses,
) -> Result<(), InitdataError> {
    if secondary.root != primary.root + SECONDARY_ROOT_DELTA {
        return Err(InitdataError::PairRootSeparation);
    }
    if primary.main_config == secondary.main_config || primary.status_a == secondary.status_a {
        return Err(InitdataError::PairAliasedObject);
    }
    Ok(())
}

/// Validate the per-instance halves of the ADT shared region: the secondary
/// maps it from the primary's base plus [`SECONDARY_SHARED_REGION_DELTA`].
pub(crate) fn validate_shared_region_bases(
    primary_base: u64,
    secondary_base: u64,
) -> Result<(), InitdataError> {
    if secondary_base != primary_base + SECONDARY_SHARED_REGION_DELTA {
        return Err(InitdataError::SharedRegionSeparation);
    }
    Ok(())
}

/// Reproduce the firmware's root acceptance checks.
///
/// The version-word gate is re-derived from our own J700 image
/// (`__TEXT+0x1e8a8`); a mismatch is answered with "Host and FW Interface is
/// mismatched" on the crashlog endpoint. The `+0x2c` nonzero gate was proven
/// on the M5 parser and the field is constant 1 in every T8140 capture; its
/// T8140 parse-gate status has not been independently re-derived, but a host
/// must write 1 there regardless. Passing this does not mean the rest of the
/// descriptor is complete.
pub(crate) fn check_firmware_root_gates(root: &[u8]) -> Result<(), InitdataError> {
    if root.len() != ROOT_SIZE {
        return Err(InitdataError::BufferSize);
    }
    if get_u64(root, 0x00) != INITDATA_MAGIC {
        return Err(InitdataError::InterfaceMismatch);
    }
    if get_u32(root, ROOT_HOST_MAPPED_ALLOCS) == 0 {
        return Err(InitdataError::HostMappedAllocationsDisabled);
    }
    Ok(())
}

/// Return the byte range of a proved channel descriptor.
///
/// Index 12 is refused because it would overlap the cursor at `+0x1a0`.
pub(crate) fn channel_descriptor_range(index: usize) -> Option<core::ops::Range<usize>> {
    if index >= RT_CHANNEL_DESCRIPTOR_COUNT {
        return None;
    }
    let start = RT_CHANNEL_DESCRIPTORS + index * RT_CHANNEL_DESCRIPTOR_STRIDE;
    Some(start..start + RT_CHANNEL_DESCRIPTOR_STRIDE)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn addresses(base: u64) -> RoleAddresses {
        RoleAddresses {
            root: base,
            runtime_pointers: base + 0x4000,
            hw_globals: base + 0x8000,
            firmware_shared_allocation: 0x5000_0000,
            shared: 0x6000_0000,
            role: base + 0xc000,
            cross_role: base + 0x1_0000,
            cross_role_2: 0,
            shared_runtime_allocations: [
                base + 0x2_0000,
                base + 0x2_1000,
                base + 0x2_2000,
                base + 0x2_3000,
                base + 0x2_4000,
                base + 0x2_5000,
            ],
            gfx1_runtime_buffer: 0,
        }
    }

    fn encode(role: FirmwareRole, addresses: &RoleAddresses) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
        let mut root = vec![0xa5; ROOT_SIZE];
        let mut runtime = vec![0xa5; RUNTIME_POINTERS_SIZE];
        let mut hw = vec![0xa5; HW_GLOBALS_SIZE];
        encode_grounded_subset(
            role,
            state_grid(role),
            inst.status_a,
            inst.main_config,
            shared().hw_data_bundle,
        )
    }

    fn region_views() -> PrimaryRegionViews {
        PrimaryRegionViews {
            sentinel_high: KERN + 0xc07c_8000,
            alias_high: [KERN + 0x15e_0000, KERN + 0x15e_8000],
        }
    }

    fn encode_root_bytes(role: InstanceRole) -> Vec<u8> {
        let inst = match role {
            InstanceRole::Primary => primary(),
            InstanceRole::Secondary => secondary(),
        };
        let mut out = vec![0xa5; root_size(role)];
        encode_root(role, &shared(), &inst, &mut out).unwrap();
        out
    }

    fn encode_main_bytes(role: InstanceRole) -> Vec<u8> {
        let inst = match role {
            InstanceRole::Primary => primary(),
            InstanceRole::Secondary => secondary(),
        };
        let views = region_views();
        let views = match role {
            InstanceRole::Primary => Some(&views),
            InstanceRole::Secondary => None,
        };
        let mut out = vec![0xa5; MAIN_CONFIG_SIZE];
        encode_main_config(role, &shared(), &inst, &channels(role), views, &mut out).unwrap();
        out
    }

    #[test]
    fn descriptor_table_aliases_are_published_in_both_main_configs() {
        let views = region_views();
        let primary = encode_main_bytes(InstanceRole::Primary);
        assert_eq!(get_u64(&primary, 0x2d8), REGION_VIEW_LOW_ADDRS[0]);
        assert_eq!(get_u64(&primary, 0x2e0), views.alias_high[0]);
        assert_eq!(get_u64(&primary, 0x2e8), REGION_VIEW_LOW_ADDRS[1]);
        assert_eq!(get_u64(&primary, 0x2f0), views.alias_high[1]);

        let secondary = encode_main_bytes(InstanceRole::Secondary);
        assert_eq!(get_u64(&secondary, 0x2d8), REGION_VIEW_LOW_ADDRS[0]);
        assert_eq!(get_u64(&secondary, 0x2e0), 0);
        assert_eq!(get_u64(&secondary, 0x2e8), REGION_VIEW_LOW_ADDRS[1]);
        assert_eq!(get_u64(&secondary, 0x2f0), 0);
    }

    #[test]
    fn per_role_root_sizes_version_tuple_and_handoff_zeroes() {
        assert_eq!(ROOT_SIZE_PRIMARY, 0xb8);
        assert_eq!(ROOT_SIZE_SECONDARY, 0xc8);
        assert_eq!(root_size(InstanceRole::Primary), 0xb8);
        assert_eq!(root_size(InstanceRole::Secondary), 0xc8);
        // The exact version tuple, both as words and as the u64 the firmware
        // compares. The M5 value 0x0c8d_e339_0728_04c0 must never reappear.
        assert_eq!(INITDATA_VERSION, [0x04c0, 0x0396, 0xa322, 0x0c8a]);
        assert_eq!(INITDATA_VERSION_WORD, 0x0c8a_a322_0396_04c0);

        let root = encode_root_bytes(InstanceRole::Primary);
        assert_eq!(root.len(), 0xb8);
        assert_eq!(get_u64(&root, 0x00), INITDATA_VERSION_WORD);
        assert_eq!(get_u64(&root, 0x08), shared().region_a);
        // +0x10 is zero at handoff even though the buffer started dirty.
        assert_eq!(get_u64(&root, 0x10), 0);
        assert_eq!(get_u64(&root, 0x18), primary().main_config);
        assert_eq!(get_u64(&root, 0x20), shared().region_c);
        assert_eq!(get_u32(&root, 0x28), 0);
        assert_eq!(get_u32(&root, 0x2c), 1);
        assert_eq!(get_u64(&root, 0xa8), primary().status_a);
        assert_eq!(get_u64(&root, 0xb0), primary().status_b);
        assert!(check_firmware_root_gates(&root).is_ok());

        let root = encode_root_bytes(InstanceRole::Secondary);
        assert_eq!(root.len(), 0xc8);
        assert_eq!(get_u32(&root, 0x28), 1);
        assert_eq!(get_u64(&root, 0xa8), secondary().status_a);
        // Status B is real only on the primary.
        assert_eq!(get_u64(&root, 0xb0), 0);
        assert_eq!(get_u64(&root, 0xb8), secondary().secondary_extras[0]);
        assert_eq!(get_u64(&root, 0xc0), secondary().secondary_extras[1]);
        assert!(check_firmware_root_gates(&root).is_ok());
    }

    #[test]
    fn uat_geometry_block_and_level_descriptors() {
        let root = encode_root_bytes(InstanceRole::Primary);
        // Page size 0x4000 (u16), page bits 14, level count 3.
        assert_eq!(
            u16::from_le_bytes(root[0x30..0x32].try_into().unwrap()),
            0x4000
        );
        assert_eq!(root[0x32], 14);
        assert_eq!(root[0x33], 3);
        // Three 0x20-byte descriptors: shifts 36/25/14, entries 64/2048/2048.
        let expected = [(36u8, 64u16), (25, 2048), (14, 2048)];
        for (index, (shift, entries)) in expected.iter().enumerate() {
            let base = 0x34 + index * 0x20;
            assert_eq!(root[base], 8);
            assert_eq!(root[base + 1], 14);
            assert_eq!(root[base + 2], 14);
            assert_eq!(root[base + 3], *shift);
            assert_eq!(
                u16::from_le_bytes(root[base + 4..base + 6].try_into().unwrap()),
                *entries
            );
            assert_eq!(
                u16::from_le_bytes(root[base + 6..base + 8].try_into().unwrap()),
                0x4000
            );
            assert_eq!(get_u64(&root, base + 0x08), 1);
            assert_eq!(get_u64(&root, base + 0x10), 0x0000_03ff_ffff_c000);
            assert_eq!(get_u64(&root, base + 0x18), (*entries as u64 - 1) << *shift);
        }
        // Spot-check the derived index masks against their absolute values.
        assert_eq!(get_u64(&root, 0x34 + 0x18), 0x3f_0000_0000_0 >> 4 << 4); // 63 << 36
        assert_eq!(get_u64(&root, 0x34 + 0x18), 63u64 << 36);
        assert_eq!(get_u64(&root, 0x54 + 0x18), 2047u64 << 25);
        assert_eq!(get_u64(&root, 0x74 + 0x18), 2047u64 << 14);
        // The descriptor block ends at 0x94; the gap to status A stays zero.
        assert!(root[0x94..0xa8].iter().all(|byte| *byte == 0));
    }

    #[test]
    fn main_config_bundle_views_are_views_into_one_allocation() {
        assert_eq!(MAIN_CONFIG_SIZE, 0x600);
        let main = encode_main_bytes(InstanceRole::Primary);
        assert_eq!(main.len(), 0x600);
        let bundle = shared().hw_data_bundle;
        assert_eq!(get_u64(&main, 0x00), bundle);
        // The repeated writable region is bundle + 0xc500, at +0x08 and
        // +0x10 — past the 0xc000 principal bundle, which is why the
        // allocation must reach 0x10000.
        assert_eq!(get_u64(&main, 0x08), bundle + 0xc500);
        assert_eq!(get_u64(&main, 0x10), bundle + 0xc500);
        assert!(HW_DATA_BUNDLE_MIN_ALLOC >= 0xc500);
        // Five unaligned qwords at +0x254: views into the ONE bundle
        // allocation at the fixed offsets, not independent objects.
        let offsets = [0x2740u64, 0x3380, 0x4400, 0xbc80, 0xbd00];
        let extents = [0x18c0usize, 0x0c80, 0x3c00, 0x3380, 0x3300];
        for (index, (offset, extent)) in offsets.iter().zip(extents).enumerate() {
            let view = get_u64(&main, 0x254 + index * 8);
            assert_eq!(view, bundle + offset);
            // Each view lies wholly inside the single allocation.
            assert!((*offset as usize) + extent <= shared().hw_data_bundle_alloc);
        }
        assert_eq!(BUNDLE_VIEW_OFFSETS.map(|o| o as u64), offsets);
        assert_eq!(BUNDLE_VIEW_EXTENTS, extents);
        // Views 3 and 4 overlap by all but 0x80 bytes, and view 0 overlaps
        // the tail of the hardware-data object — the relations independent
        // allocations cannot reproduce.
        assert_eq!(0xbc80 + 0x3380, 0xbd00 + 0x3300);
        assert!(BUNDLE_VIEW_OFFSETS[0] < HW_DATA_SIZE);
    }

    #[test]
    fn bundle_allocation_below_minimum_fails_closed() {
        let mut small = shared();
        small.hw_data_bundle_alloc = 0xc000;
        let mut out = vec![0; ROOT_SIZE_PRIMARY];
        assert_eq!(
            encode_root(InstanceRole::Primary, &small, &primary(), &mut out),
            Err(InitdataError::BundleAllocationTooSmall)
        );
    }

    #[test]
    fn channel_table_has_seventeen_entries_with_the_control_grid_at_12() {
        assert_eq!(CHANNEL_TABLE_ENTRIES, 17);
        assert_eq!(channel_table_entry_range(0), Some(0x20..0x40));
        assert_eq!(channel_table_entry_range(11), Some(0x180..0x1a0));
        // Entry 12 exists. The M5-derived model refused it, misreading its
        // state-counter addresses at +0x1a0/+0x1a8 as an unrelated cursor.
        assert_eq!(channel_table_entry_range(12), Some(0x1a0..0x1c0));
        assert_eq!(channel_table_entry_range(16), Some(0x220..0x240));
        assert_eq!(channel_table_entry_range(17), None);

        let main = encode_main_bytes(InstanceRole::Primary);
        let grid = state_grid(InstanceRole::Primary);
        let bundle = shared().hw_data_bundle;
        // Work entry 0 (TA_0): three separate state-counter addresses at 0x10
        // spacing; consumer = state 0, producer = state 2. Its ring is a work
        // ring in the bundle.
        assert_eq!(get_u64(&main, 0x20), grid + 0x0c0); // consumer
        assert_eq!(get_u64(&main, 0x28), grid + 0x0d0);
        assert_eq!(get_u64(&main, 0x30), grid + 0x0e0); // producer
        assert_eq!(
            get_u64(&main, 0x38),
            bundle + WORK_RING_BUNDLE_OFFSETS[0] as u64
        );

        // Entry 12 (device control): three counters in the compact grid, and
        // the ring embedded at main + 0x4c0, where the opening opcode is staged.
        let e12 = channel_table_entry_range(12).unwrap().start;
        assert_eq!(
            get_u64(&main, e12 + CHANNEL_STATE_CONSUMER * 8),
            grid + 0x300
        );
        assert_eq!(
            get_u64(&main, e12 + CHANNEL_STATE_PRODUCER * 8),
            grid + 0x320
        );
        assert_eq!(get_u64(&main, e12 + 0x18), primary().main_config + 0x4c0);
        assert_eq!(get_u32(&main, 0x4c0), CONTROL_OPENING_OPCODE_PRIMARY);

        // Entry 15 is partial: a first state address, no producer, no ring.
        let e15 = channel_table_entry_range(15).unwrap().start;
        assert_ne!(get_u64(&main, e15), 0);
        assert_eq!(get_u64(&main, e15 + 0x08), 0);
        assert_eq!(get_u64(&main, e15 + 0x10), 0);
        assert_eq!(get_u64(&main, e15 + 0x18), 0);
        // Entry 16 is entirely empty.
        let e16 = channel_table_entry_range(16).unwrap().start;
        assert!(main[e16..e16 + CHANNEL_ENTRY_SIZE]
            .iter()
            .all(|byte| *byte == 0));
        // The primary carries the +0x3e0 mask; +0x300 (secondary kind) stays 0.
        assert_eq!(get_u32(&main, 0x3e0), MAIN_PRIMARY_MASK_VALUE);
        assert_eq!(get_u32(&main, 0x32c), 1);
        assert_eq!(get_u32(&main, 0x344), 0xabcd_abcd);
        assert_eq!(get_u32(&main, 0x300), 0);
    }

    #[test]
    fn secondary_leaves_work_channels_empty_but_keeps_control() {
        let main = encode_main_bytes(InstanceRole::Secondary);
        // Entries 0..11 are empty on the control-only peer.
        for index in 0..WORK_CHANNEL_COUNT {
            let range = channel_table_entry_range(index).unwrap();
            assert!(main[range].iter().all(|byte| *byte == 0));
        }
        // Entry 12 control ring is still main + 0x4c0, with the secondary opcode.
        let e12 = channel_table_entry_range(12).unwrap().start;
        assert_eq!(get_u64(&main, e12 + 0x18), secondary().main_config + 0x4c0);
        assert_eq!(get_u32(&main, 0x4c0), CONTROL_OPENING_OPCODE_SECONDARY);
        // Secondary-only fields: +0x300 = 4, and the unaligned +0x471 pointer
        // into primary view 2. The primary's +0x3e0 mask is absent here.
        assert_eq!(get_u32(&main, 0x300), MAIN_SECONDARY_KIND_VALUE);
        let extra = shared().hw_data_bundle
            + BUNDLE_VIEW_OFFSETS[SECONDARY_EXTRA_VIEW_INDEX] as u64
            + SECONDARY_EXTRA_VIEW_OFFSET as u64;
        assert_eq!(get_u64(&main, 0x471), extra);
        assert_eq!(get_u32(&main, 0x3e0), 0);
    }

    #[test]
    fn primary_control_opening_is_the_single_native_record() {
        let mut opening = vec![0; control_opening_size(InstanceRole::Primary)];
        encode_control_opening(InstanceRole::Primary, &mut opening).unwrap();

        assert_eq!(control_opening_producer(InstanceRole::Primary), 1);
        for slot in 0..1 {
            let base = slot * CONTROL_RECORD_SIZE;
            assert_eq!(get_u32(&opening, base), CONTROL_OPENING_OPCODE_PRIMARY);
            assert!(opening[base + 4..base + CONTROL_RECORD_SIZE]
                .iter()
                .all(|byte| *byte == 0));
        }
        assert_eq!(opening.len(), CONTROL_RECORD_SIZE);
    }

    #[test]
    fn live_compute_control_tail_matches_the_output_positive_sequence() {
        fn fnv1a64(bytes: &[u8]) -> u64 {
            bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
                (hash ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01b3)
            })
        }

        let mut tail = vec![0u8; COMPUTE_RUNTIME_CONTROL_RECORDS * CONTROL_RECORD_SIZE];
        for index in 0..COMPUTE_RUNTIME_CONTROL_RECORDS {
            let start = index * CONTROL_RECORD_SIZE;
            encode_compute_runtime_control_record(
                index,
                &mut tail[start..start + CONTROL_RECORD_SIZE],
            )
            .unwrap();
        }
        assert_eq!(tail.len(), 65 * 0x40);
        assert_eq!(fnv1a64(&native_compute_bytes(&tail)), 0x544f_a4b7_bc23_e65a);
        assert_eq!(COMPUTE_RUNTIME_CONTROL_FINAL_COUNTER, 66);

        for (index, sequence) in [(0, 0), (53, 53), (55, 54), (56, 55), (58, 56), (64, 62)] {
            let base = index * CONTROL_RECORD_SIZE;
            assert_eq!(get_u32(&tail, base), 0x2e);
            assert_eq!(get_u32(&tail, base + 0x04), sequence);
            assert_eq!(get_u32(&tail, base + 0x0c), 0);
        }
        for (index, class, sequence, operand, slot_offset, count) in [
            (
                COMPUTE_RUNTIME_CLASS1_RECORD_INDEX,
                1,
                54,
                COMPUTE_RUNTIME_CLASS1_TABLE_ADDRESS,
                0x480,
                0x18,
            ),
            (
                COMPUTE_RUNTIME_CLASS2_RECORD_INDEX,
                2,
                56,
                COMPUTE_FLIST_RUN_TABLE_ADDRESS,
                0x5c0,
                0x28,
            ),
        ] {
            let base = index * CONTROL_RECORD_SIZE;
            assert_eq!(get_u32(&tail, base), 0x20);
            assert_eq!(get_u32(&tail, base + 0x04), class);
            assert_eq!(get_u32(&tail, base + 0x08), 0x3f);
            assert_eq!(get_u32(&tail, base + 0x0c), sequence);
            assert_eq!(get_u64(&tail, base + 0x14), COMPUTE_RUNTIME_SUPPORT_ADDRESS);
            assert_eq!(get_u64(&tail, base + 0x1c), operand);
            assert_eq!(get_u64(&tail, base + 0x24), operand + slot_offset);
            assert_eq!(get_u32(&tail, base + 0x2c), count);
            assert_eq!(get_u32(&tail, base + 0x30), 0);
            assert_eq!(get_u32(&tail, base + 0x34), 1);
        }
    }

    #[test]
    fn bootstrap_uma_release_record_is_exact() {
        let mut record = [0xa5; CONTROL_RECORD_SIZE];
        encode_bootstrap_uma_release_record(0, 0, &mut record).unwrap();
        assert_eq!(get_u32(&record, 0), CONTROL_BOOTSTRAP_UMA_RELEASE_OPCODE);
        assert!(record[4..].iter().all(|byte| *byte == 0));

        encode_bootstrap_uma_release_record(0x1122_3344_5566_7788, 0xaabb_ccdd, &mut record)
            .unwrap();
        assert_eq!(get_u64(&record, 0x04), 0x1122_3344_5566_7788);
        assert_eq!(get_u32(&record, 0x0c), 0xaabb_ccdd);
        assert!(record[0x10..].iter().all(|byte| *byte == 0));
    }

    #[test]
    fn first_cl_readiness_suffix_has_exact_sequences_contexts_and_bytes() {
        assert_eq!(
            COMPUTE_READINESS_PUBLICATION_ORDER,
            [
                ComputeReadinessPublicationStep::ControlRecord(0),
                ComputeReadinessPublicationStep::ControlRecord(1),
                ComputeReadinessPublicationStep::ControlRecord(2),
                ComputeReadinessPublicationStep::InitializeContext2Activation,
                ComputeReadinessPublicationStep::ControlRecord(3),
                ComputeReadinessPublicationStep::ControlRecord(4),
                ComputeReadinessPublicationStep::ControlRecord(5),
            ]
        );
        let mut records = [[0u8; CONTROL_RECORD_SIZE]; COMPUTE_READINESS_RECORDS];
        for (index, record) in records.iter_mut().enumerate() {
            encode_compute_readiness_record(index, record).unwrap();
        }

        for (index, boundary) in COMPUTE_READINESS_BOUNDARIES.iter().enumerate() {
            assert_eq!(boundary.index as usize, index);
            assert_eq!(
                boundary.producer_before,
                CONTROL_OPENING_PRIMARY_PRODUCER + index as u32
            );
            assert_eq!(
                boundary.producer_after,
                CONTROL_OPENING_PRIMARY_PRODUCER + 1 + index as u32
            );
            assert_eq!(get_u32(&records[index], 0x00), boundary.opcode);
            let (sequence_offset, context_offset) = if boundary.opcode == 0x20 {
                (0x0c, 0x30)
            } else {
                (0x04, 0x0c)
            };
            assert_eq!(get_u32(&records[index], sequence_offset), boundary.sequence);
            assert_eq!(get_u32(&records[index], context_offset), boundary.context);
            assert_eq!(compute_readiness_boundary(index), Some(*boundary));
        }
        assert_eq!(compute_readiness_boundary(COMPUTE_READINESS_RECORDS), None);

        let mut progress = ComputeReadinessProgress::new();
        assert_eq!(progress.retire(5), None);
        for boundary in COMPUTE_READINESS_BOUNDARIES {
            assert_eq!(progress.next_boundary(), Some(boundary));
            let pending = progress.begin_publication(boundary).unwrap();
            assert_eq!(pending.pending_target(), Some(boundary.producer_after));
            assert_eq!(pending.next_boundary(), None);
            assert_eq!(pending.begin_publication(boundary), None);
            assert_eq!(pending.retire(boundary.producer_before), None);
            progress = pending.retire(boundary.producer_after).unwrap();
            assert_eq!(progress.retired_records(), boundary.index as usize + 1);
        }
        assert!(progress.complete());
        assert_eq!(progress.next_boundary(), None);

        for (index, class, sequence, support, slot, count, context) in [
            (
                0,
                1,
                35,
                COMPUTE_READINESS_CLASS1_SUPPORT_ADDRESS,
                0x440,
                0x20,
                1,
            ),
            (
                3,
                3,
                37,
                COMPUTE_READINESS_CLASS3_SUPPORT_ADDRESS,
                0x5c0,
                0x28,
                2,
            ),
        ] {
            let record = &records[index];
            assert_eq!(get_u32(record, 0x00), 0x20);
            assert_eq!(get_u32(record, 0x04), class);
            assert_eq!(get_u32(record, 0x08), 0x3f);
            assert_eq!(get_u32(record, 0x0c), sequence);
            assert_eq!(get_u64(record, 0x14), support);
            assert_eq!(get_u64(record, 0x1c), COMPUTE_FLIST_RUN_TABLE_ADDRESS);
            assert_eq!(get_u64(record, 0x24), COMPUTE_FLIST_RUN_TABLE_ADDRESS + slot);
            assert_eq!(get_u32(record, 0x2c), count);
            assert_eq!(get_u32(record, 0x30), context);
            assert_eq!(get_u32(record, 0x34), 1);
        }

        for (index, sequence, context) in [(1, 35, 1), (2, 36, 1), (4, 37, 2), (5, 38, 2)] {
            let record = &records[index];
            assert_eq!(get_u32(record, 0x00), 0x2e);
            assert_eq!(get_u32(record, 0x04), sequence);
            assert_eq!(get_u32(record, 0x0c), context);
            assert!(record[0x10..].iter().all(|byte| *byte == 0));
        }
        assert_eq!(
            COMPUTE_READINESS_FINAL_COUNTER,
            CONTROL_OPENING_PRIMARY_PRODUCER + COMPUTE_READINESS_RECORDS as u32
        );
        assert!(encode_compute_readiness_record(COMPUTE_READINESS_RECORDS, &mut records[0])
            .is_err());
    }

    #[test]
    fn first_cl_readiness_support_state_and_activation_pages_are_exact() {
        fn expected_support(
            class: u32,
            active: u32,
            resource_class: u64,
            cursor: u32,
            state: u64,
            final_kind: u32,
        ) -> [u8; COMPUTE_READINESS_PAGE_SIZE] {
            let mut expected = [0u8; COMPUTE_READINESS_PAGE_SIZE];
            put_u32(&mut expected, 0x00, class);
            put_u32(&mut expected, 0x08, active);
            put_u32(&mut expected, 0x10, class);
            put_u64(&mut expected, 0x14, COMPUTE_FLIST_PAGE_LIST_ADDRESS);
            put_u32(&mut expected, 0x1c, 0x4_0000);
            put_u64(&mut expected, 0x20, resource_class << 40);
            put_u64(&mut expected, 0x28, resource_class << 40);
            put_u64(&mut expected, 0x30, COMPUTE_FLIST_RUN_TABLE_ADDRESS);
            put_u32(&mut expected, 0x40, 4);
            put_u32(&mut expected, 0x48, cursor);
            put_u64(&mut expected, 0x4c, state);
            put_u32(&mut expected, 0x60, final_kind);
            expected
        }

        let mut class1 = [0u8; COMPUTE_READINESS_PAGE_SIZE];
        let mut class3 = [0u8; COMPUTE_READINESS_PAGE_SIZE];
        let mut state = [0xffu8; COMPUTE_READINESS_PAGE_SIZE];
        encode_compute_readiness_class1_support(&mut class1).unwrap();
        encode_compute_readiness_class3_support(&mut class3).unwrap();
        encode_compute_readiness_state(&mut state).unwrap();
        assert_eq!(
            class1,
            expected_support(
                1,
                1,
                0x11,
                0x88,
                COMPUTE_READINESS_CLASS1_STATE_ADDRESS,
                2,
            )
        );
        assert_eq!(
            class3,
            expected_support(
                3,
                2,
                0x17,
                0xb8,
                COMPUTE_READINESS_CLASS3_STATE_ADDRESS,
                3,
            )
        );
        assert_eq!(get_u32(&state, 0), 1);
        assert!(state[4..].iter().all(|byte| *byte == 0));

        let mut activation = [0xffu8; CONTROL_RECORD_SIZE];
        encode_compute_readiness_activation_record(&mut activation).unwrap();
        let mut expected = [0u8; CONTROL_RECORD_SIZE];
        put_u64(&mut expected, 0x00, 0x0000_0100_0000_ffff);
        put_u64(&mut expected, 0x20, 0x0002_0000_0000_0000);
        put_u64(&mut expected, 0x30, 0x0000_0000_ff00_0000);
        assert_eq!(activation, expected);
        assert_eq!(COMPUTE_READINESS_ACTIVATION_RECORD_OFFSET, 0x80);

        let mut operand_table = [0xffu8; CONTROL_OPERAND_TABLE_SIZE];
        encode_compute_readiness_operand_table(&mut operand_table).unwrap();
        for index in 0..COMPUTE_READINESS_OPERAND_ENTRY_COUNT {
            assert_eq!(
                get_u64(&operand_table, index * CONTROL_OPERAND_ENTRY_STRIDE),
                (COMPUTE_FLIST_BUFFER_BASE
                    + index as u64 * CONTROL_OPERAND_BUFFER_STRIDE)
                    | CONTROL_OPERAND_ENTRY_FLAG,
            );
        }
        assert!(operand_table
            [COMPUTE_READINESS_OPERAND_ENTRY_COUNT * CONTROL_OPERAND_ENTRY_STRIDE..]
            .iter()
            .all(|byte| *byte == 0));
    }

    #[test]
    fn live_compute_control_closure_matches_source_builders() {
        fn fnv1a64(bytes: &[u8]) -> u64 {
            bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
                (hash ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01b3)
            })
        }

        let mut class1 = vec![0; COMPUTE_RUNTIME_SUPPORT_SIZE];
        encode_compute_runtime_class1_support(&mut class1).unwrap();
        assert_eq!(fnv1a64(&native_compute_bytes(&class1)), 0xb40b_a63a_16c1_937d);
        assert_eq!(get_u32(&class1, 0x00), 1);
        assert_eq!(get_u64(&class1, 0x14), COMPUTE_RUNTIME_PAGE_LIST_ADDRESS);
        assert_eq!(get_u64(&class1, 0x30), COMPUTE_RUNTIME_CLASS1_TABLE_ADDRESS);
        assert_eq!(get_u64(&class1, 0x4c), COMPUTE_RUNTIME_STATE_ADDRESS);

        let mut class2 = vec![0; COMPUTE_RUNTIME_SUPPORT_SIZE];
        encode_compute_runtime_class2_support(&mut class2).unwrap();
        assert_eq!(fnv1a64(&native_compute_bytes(&class2)), 0xdd12_cbe2_2d7c_3d50);
        assert_eq!(get_u32(&class2, 0x00), 2);
        assert_eq!(get_u64(&class2, 0x30), COMPUTE_FLIST_RUN_TABLE_ADDRESS);

        let mut table = vec![0; COMPUTE_RUNTIME_CLASS1_TABLE_SIZE];
        encode_compute_runtime_class1_table(&mut table).unwrap();
        assert_eq!(fnv1a64(&native_compute_bytes(&table)), 0xc820_66ed_9e50_6c41);
        for (index, address) in COMPUTE_RUNTIME_BUFFER_ADDRESSES.into_iter().enumerate() {
            assert_eq!(get_u64(&table, index * 0x40), address | (1 << 60));
        }

        let mut inventory = vec![0; COMPUTE_RUNTIME_PAGE_LIST_SIZE];
        encode_compute_runtime_page_inventory(&mut inventory).unwrap();
        assert_eq!(fnv1a64(&native_compute_bytes(&inventory)), 0x9f78_e915_163a_cea5);
        assert_eq!(get_u64(&inventory, 0), COMPUTE_RUNTIME_BUFFER_ADDRESSES[0]);
        assert_eq!(
            get_u64(&inventory, inventory.len() - 8),
            COMPUTE_RUNTIME_BUFFER_ADDRESSES[20] + 0xff000
        );

        let mut dispatch = [0u8; COMPUTE_DISPATCH_RECORD_SIZE];
        encode_compute_dispatch_record(&mut dispatch).unwrap();
        let packed = get_u64(&dispatch, 0);
        assert_eq!((packed & ((1 << 41) - 1)) << 7, COMPUTE_FLIST_PAGE_LIST_ADDRESS);
        // Compare the capture only after undoing the packed VA relocation.
        put_u64(&mut dispatch, 0, packed - (COMPUTE_FLIST_RELOCATION >> 7));
        assert_eq!(fnv1a64(&dispatch), 0x12a0_cf17_a88d_a788);
    }

    #[test]
    fn secondary_control_opening_is_the_single_native_record() {
        let mut opening = vec![0; control_opening_size(InstanceRole::Secondary)];
        encode_control_opening(InstanceRole::Secondary, &mut opening).unwrap();

        assert_eq!(control_opening_producer(InstanceRole::Secondary), 1);
        for slot in 0..CONTROL_OPENING_SECONDARY_RECORDS {
            let base = slot * CONTROL_RECORD_SIZE;
            let expected = if slot < core::cmp::min(3, CONTROL_OPENING_SECONDARY_RECORDS) {
                CONTROL_OPENING_OPCODE_SECONDARY
            } else {
                CONTROL_OPENING_SECONDARY_CONTINUE_OPCODE
            };
            assert_eq!(get_u32(&opening, base), expected);
            assert!(opening[base + 4..base + CONTROL_RECORD_SIZE]
                .iter()
                .all(|byte| *byte == 0));
        }
    }

    #[test]
    fn generic_control_support_keeps_descriptor_and_runtime_identity() {
        let mut shared = vec![0; CONTROL_SHARED_OBJECT_SIZE];
        encode_control_shared(&mut shared).unwrap();

        assert_eq!(get_u64(&shared, 0x00), 1);
        assert_eq!(get_u64(&shared, 0x10), 1);
        assert_eq!(get_u64(&shared, 0x18), 0x0004_0000_0000_0070);
        assert_eq!(get_u64(&shared, 0x20), 0x0000_1100_0000_0000);
        assert_eq!(get_u64(&shared, 0x28), 0x0000_1100_0000_0000);
        assert_eq!(get_u64(&shared, 0x30), CONTROL_OPERAND_TABLE_ADDRESS);
        assert_eq!(
            get_u32(&shared, CONTROL_SHARED_CURSOR_OFFSET),
            CONTROL_SHARED_CURSOR_BEFORE
        );
        assert_eq!(get_u64(&shared, 0x4c), CONTROL_SHARED_INNER_ADDRESS);
        assert_eq!(get_u64(&shared, 0x60), 2);
        assert_eq!(CONTROL_SHARED_INNER_PRESENTED, 2);
    }

    #[test]
    fn compute_partial_support_preserves_hardware_proven_compact_state() {
        let mut shared = vec![0; CONTROL_SHARED_OBJECT_SIZE];
        encode_compute_partial_opening_control_shared(&mut shared).unwrap();

        assert_eq!(get_u64(&shared, 0x00), 1);
        assert_eq!(get_u32(&shared, 0x08), u32::MAX);
        assert_eq!(get_u32(&shared, 0x10), 2);
        assert_eq!(get_u64(&shared, 0x20), 0x0000_1900_0000_0000);
        assert_eq!(get_u64(&shared, 0x28), 0x0000_1900_0000_0000);
        assert_eq!(get_u64(&shared, 0x30), COMPUTE_FLIST_RUN_TABLE_ADDRESS);
        assert_eq!(
            get_u32(&shared, CONTROL_SHARED_CURSOR_OFFSET),
            COMPUTE_PARTIAL_OPENING_CONTROL_SHARED_CURSOR_BEFORE
        );
        assert_eq!(COMPUTE_PARTIAL_OPENING_CONTROL_SHARED_CURSOR_BEFORE, 0x18);
        assert_eq!(
            get_u64(&shared, 0x4c),
            PARTIAL_OPENING_CONTROL_SHARED_INNER_ADDRESS
        );
        assert_eq!(get_u64(&shared, 0x60), 3);
    }

    #[test]
    fn first_partial_support_has_distinct_address_cursor_and_inner_page() {
        let mut shared = vec![0; CONTROL_SHARED_OBJECT_SIZE];
        encode_partial_opening_control_shared(&mut shared).unwrap();

        assert_eq!(PARTIAL_OPENING_CONTROL_SHARED_ADDRESS, 0xffff_fc20_c082_8000);
        assert_eq!(get_u64(&shared, 0x00), 1);
        assert_eq!(get_u32(&shared, 0x08), 0);
        assert_eq!(get_u32(&shared, 0x0c), 0);
        assert_eq!(get_u64(&shared, 0x10), 2);
        assert_eq!(get_u64(&shared, 0x20), 0x0000_1100_0000_0000);
        assert_eq!(get_u64(&shared, 0x28), 0x0000_1100_0000_0000);
        assert_eq!(get_u64(&shared, 0x30), CONTROL_OPERAND_TABLE_ADDRESS);
        assert_eq!(
            get_u32(&shared, CONTROL_SHARED_CURSOR_OFFSET),
            PARTIAL_OPENING_CONTROL_SHARED_CURSOR_BEFORE
        );
        assert_eq!(
            get_u64(&shared, 0x4c),
            PARTIAL_OPENING_CONTROL_SHARED_INNER_ADDRESS
        );
        assert_eq!(get_u64(&shared, 0x60), 3);
        assert_eq!(PARTIAL_OPENING_CONTROL_SHARED_INNER_PRESENTED, 0);

        encode_render_control_shared(&mut shared).unwrap();
        assert_eq!(get_u64(&shared, 0x00), CONTROL_SHARED_INNER_ADDRESS);
        assert_eq!(get_u32(&shared, 0x08), 0);
        assert_eq!(get_u64(&shared, 0x10), 2);
        assert_eq!(get_u64(&shared, 0x14), 0x70_0000_0000);
        assert_eq!(get_u64(&shared, 0x30), CONTROL_OPERAND_TABLE_ADDRESS);
        assert_eq!(get_u64(&shared, 0x4c), CONTROL_SHARED_INNER_ADDRESS);
        assert_eq!(get_u64(&shared, 0x60), 3);
    }

    #[test]
    fn first_partial_operand_table_is_published_between_control_and_work() {
        let mut table = vec![0xff; CONTROL_OPERAND_TABLE_SIZE];
        encode_partial_opening_operand_table_pre_control(&mut table).unwrap();
        assert!(table.iter().all(|byte| *byte == 0));

        encode_partial_opening_operand_table_post_control(&mut table).unwrap();
        for index in 0..PARTIAL_OPENING_CONTROL_OPERAND_ENTRY_COUNT {
            assert_eq!(
                get_u64(&table, index * CONTROL_OPERAND_ENTRY_STRIDE),
                CONTROL_OPERAND_ENTRY_FLAG
                    | (CONTROL_OPERAND_BUFFER_BASE
                        + index as u64 * CONTROL_OPERAND_BUFFER_STRIDE)
            );
        }
        assert!(table[PARTIAL_OPENING_CONTROL_OPERAND_ENTRY_COUNT
            * CONTROL_OPERAND_ENTRY_STRIDE..]
            .iter()
            .all(|byte| *byte == 0));

        encode_control_operand_table(&mut table).unwrap();

        for index in 0..CONTROL_OPERAND_ENTRY_COUNT {
            assert_eq!(
                get_u64(&table, index * CONTROL_OPERAND_ENTRY_STRIDE),
                CONTROL_OPERAND_ENTRY_FLAG
                    | (COMPUTE_FLIST_BUFFER_BASE
                        + index as u64 * CONTROL_OPERAND_BUFFER_STRIDE)
            );
        }
        assert!(table[CONTROL_OPERAND_ENTRY_COUNT * CONTROL_OPERAND_ENTRY_STRIDE..]
            .iter()
            .all(|byte| *byte == 0));
    }

    #[test]
    fn a_null_producer_on_a_populated_channel_fails_closed() {
        // A populated work channel with a null producer (state 2) makes the
        // firmware fault on address zero; encode must refuse it.
        let mut table = channels(InstanceRole::Primary);
        table[0].states[CHANNEL_STATE_PRODUCER] = 0;
        let views = region_views();
        let mut out = vec![0; MAIN_CONFIG_SIZE];
        assert_eq!(
            encode_main_config(
                InstanceRole::Primary,
                &shared(),
                &primary(),
                &table,
                Some(&views),
                &mut out,
            ),
            Err(InitdataError::ChannelTableShape)
        );
    }

    #[test]
    fn role_specific_root_fields_fail_closed() {
        // A primary must not carry the secondary-only extras (its root is only
        // 0xb8 bytes and cannot even hold them).
        let mut inst = primary();
        inst.secondary_extras = [KERN + 0x1000, 0];
        let mut out = vec![0; ROOT_SIZE_PRIMARY];
        assert_eq!(
            encode_root(InstanceRole::Primary, &shared(), &inst, &mut out),
            Err(InitdataError::RoleSpecificAddress)
        );
        // A secondary must not carry a status-B pointer.
        let mut inst = secondary();
        inst.status_b = KERN + 0x1000;
        let mut out = vec![0; ROOT_SIZE_SECONDARY];
        assert_eq!(
            encode_root(InstanceRole::Secondary, &shared(), &inst, &mut out),
            Err(InitdataError::RoleSpecificAddress)
        );
    }

    #[test]
    fn wrong_root_buffer_size_fails_closed() {
        // The primary root is 0xb8, not 0xc8; a secondary-sized buffer is wrong.
        let mut out = vec![0; ROOT_SIZE_SECONDARY];
        assert_eq!(
            encode_root(InstanceRole::Primary, &shared(), &primary(), &mut out),
            Err(InitdataError::BufferSize)
        );
    }

    fn register_windows() -> Vec<RegisterWindow> {
        vec![
            RegisterWindow {
                slot: 0,
                phys: 0x2_0000_0000,
                device_va: HW_REGISTER_APERTURE.start,
                size: 0x4000,
                unk_18: 0,
                flag: 2,
            },
            RegisterWindow {
                slot: 5,
                phys: 0x2_1000_0000,
                device_va: HW_REGISTER_APERTURE.start + 0x1_0000,
                size: 0x8000,
                unk_18: 0,
                flag: 2,
            },
        ]
    }

    fn completion_aliases() -> [G17PKsmCompletionAliases; 2] {
        [
            G17PKsmCompletionAliases {
                low: G17P_KSM_COMPLETION_LOW_VAS[0],
                high: shared().hw_data_bundle
                    + G17P_KSM_COMPLETION_HIGH_BUNDLE_OFFSETS[0] as u64,
            },
            G17PKsmCompletionAliases {
                low: G17P_KSM_COMPLETION_LOW_VAS[1],
                high: shared().hw_data_bundle
                    + G17P_KSM_COMPLETION_HIGH_BUNDLE_OFFSETS[1] as u64,
            },
        ]
    }

    fn sksm_qid_resource_high() -> u64 {
        shared().hw_data_bundle + NATIVE_SHARED_CLUSTER_SIZE as u64 + 0x4000
    }

    fn qos_resource_high() -> u64 {
        shared().hw_data_bundle + NATIVE_SHARED_CLUSTER_SIZE as u64 + 0x8000
    }

    #[test]
    fn hw_data_live_pointers_survive_opaque_constants() {
        for qos_resource in [
            0xffff_fc20_001f_8000,
            0xffff_fc20_013a_c000,
            0xffff_fc20_cabc_4000,
        ] {
            let aliases = [
                G17PKsmCompletionAliases {
                    low: 0x70_0240_0000,
                    high: 0xffff_fc20_c240_0000,
                },
                G17PKsmCompletionAliases {
                    low: 0x70_0280_0000,
                    high: 0xffff_fc20_c280_0000,
                },
            ];
            let windows: Vec<_> = (0..HW_REGISTER_MAP_SLOTS)
                .map(|slot| RegisterWindow {
                    slot,
                    phys: 0x4_0000_0000 + (slot as u64) * 0x4000,
                    device_va: HW_REGISTER_APERTURE.start + (slot as u64) * 0x4000,
                    size: 0x4000,
                    unk_18: 0,
                    flag: 2,
                })
                .collect();
            let mut hw = vec![0xa5; HW_DATA_SIZE];
            encode_hw_data(&windows, &[], aliases, qos_resource,
                sksm_qid_resource_high(), &mut hw).unwrap();
            assert_eq!(get_u64(&hw, G17P_QOS_RESOURCE_LIVE_OFFSET), qos_resource);
            assert_eq!(get_u32(&hw, 0x26b4), 1);
            assert_eq!(get_u32(&hw, 0x26b8), 1);
            for (aliases, ordinal) in aliases.iter().zip(G17P_KSM_COMPLETION_PRESENT_ORDINALS) {
                let base = G17P_KSM_COMPLETION_TABLE_OFFSET
                    + ordinal as usize * G17P_KSM_COMPLETION_DESCRIPTOR_STRIDE;
                assert_eq!(get_u64(&hw, base), aliases.low);
                assert_eq!(get_u64(&hw, base + 8), aliases.high);
            }
            for window in windows {
                let base = HW_REGISTER_MAP_OFFSET + window.slot * HW_REGISTER_MAP_ENTRY_SIZE;
                assert_eq!(get_u64(&hw, base), window.phys);
                assert_eq!(get_u64(&hw, base + 8), window.device_va);
            }
        }
    }

    #[test]
    fn hw_data_opaque_runs_do_not_overlap_live_fields() {
        let live = [
            HW_REGISTER_MAP_OFFSET
                ..HW_REGISTER_MAP_OFFSET + HW_REGISTER_MAP_SLOTS * HW_REGISTER_MAP_ENTRY_SIZE,
            G17P_KSM_COMPLETION_TABLE_OFFSET
                ..G17P_KSM_COMPLETION_TABLE_OFFSET + 4 * G17P_KSM_COMPLETION_DESCRIPTOR_STRIDE,
            G17P_QOS_RESOURCE_LIVE_OFFSET..G17P_QOS_RESOURCE_LIVE_OFFSET + 8,
        ];
        for (offset, data) in HW_DATA_OPAQUE_RUNS {
            for range in &live {
                assert!(*offset + data.len() <= range.start || *offset >= range.end,
                    "opaque range {offset:#x}..{:#x} overlaps live {range:#x?}",
                    offset + data.len());
            }
        }
    }

    #[test]
    fn hw_data_size_acceptance_completion_table_and_firmware_state() {
        assert_eq!(HW_DATA_SIZE, 0x3db4);
        let windows = register_windows();
        let flag_only = [(3usize, 2u32), (7, 2)];
        let aliases = completion_aliases();
        let mut hw = vec![0xa5; HW_DATA_SIZE];
        let qos_resource = qos_resource_high();
        let sksm_qid_resource = sksm_qid_resource_high();
        encode_hw_data(
            &windows,
            &flag_only,
            aliases,
            qos_resource,
            sksm_qid_resource,
            &mut hw,
        )
        .unwrap();
        (root, runtime, hw)
    }

    #[test]
    fn allocation_graph_and_descriptor_boundary_are_exact() {
        assert_eq!(FIRMWARE_ALLOCATIONS[0].size, 0x2828);
        assert_eq!(FIRMWARE_ALLOCATIONS[4].runtime_pointer_offset, 0x481);
        assert_eq!(FIRMWARE_ALLOCATIONS[4].size, 0x12688);
        assert_eq!(FIRMWARE_ALLOCATIONS[4].role_only, Some(FirmwareRole::Gfx1));
        assert_eq!(channel_descriptor_range(0), Some(0x20..0x40));
        assert_eq!(channel_descriptor_range(11), Some(0x180..0x1a0));
        assert_eq!(channel_descriptor_range(12), None);
    }

    #[test]
    fn gfx_subset_encodes_the_proven_pointer_chain_and_perf_table() {
        let address = addresses(0x1000_0000);
        let (root, runtime, hw) = encode(FirmwareRole::Gfx, &address);

        assert_eq!(root.len(), 0xc8);
        assert_eq!(runtime.len(), 0x4c0);
        assert_eq!(hw.len(), 0x2828);
        assert_eq!(get_u64(&root, 0), INITDATA_MAGIC);
        assert_eq!(get_u64(&root, 0x18), address.runtime_pointers);
        assert_eq!(get_u32(&root, 0x28), 0);
        assert_eq!(get_u32(&root, 0x2c), 1);
        assert_eq!(get_u64(&root, 0xb0), address.cross_role);
        assert_eq!(get_u64(&root, 0xb8), 0);
        assert_eq!(get_u64(&runtime, 0), address.hw_globals);
        assert_eq!(get_u64(&runtime, 0x200), address.role);
        // The full role-shared sub-allocation graph is populated.
        for (offset, va) in RT_SHARED_SUBALLOC_OFFSETS
            .iter()
            .copied()
            .zip(address.shared_runtime_allocations)
        {
            assert_eq!(get_u64(&runtime, offset), va);
        }
        // GFX must not touch the GFX1-only buffer slot.
        assert_eq!(get_u64(&runtime, 0x481), 0);
        assert_eq!(get_u32(&hw, 0x10b8), 400);
        assert_eq!(get_u32(&hw, 0x10bc + 12), 700);
        assert_eq!(get_u32(&hw, 0x1b3c), 3);
        assert!(root[ROOT_GPU_CONFIG].iter().all(|byte| *byte == 0));
        assert_eq!(runtime[0x250], 0);
        assert_eq!(hw[0x2700], 0);
        assert!(check_firmware_root_gates(&root).is_ok());
    }

    #[test]
    fn gfx1_uses_only_its_proven_cross_role_fields() {
        let mut address = addresses(0x2000_0000);
        address.cross_role_2 = 0x7000_0000;
        address.gfx1_runtime_buffer = 0x7100_0000;
        let (root, runtime, _) = encode(FirmwareRole::Gfx1, &address);

        assert_eq!(get_u32(&root, 0x28), 1);
        assert_eq!(get_u64(&root, 0xb0), 0);
        assert_eq!(get_u64(&root, 0xb8), address.cross_role);
        assert_eq!(get_u64(&root, 0xc0), address.cross_role_2);
        // The unaligned GFX1-only runtime buffer pointer is written at +0x481.
        assert_eq!(get_u64(&runtime, 0x481), address.gfx1_runtime_buffer);
    }

    #[test]
    fn gfx1_requires_its_runtime_buffer_and_gfx_forbids_it() {
        let mut root = vec![0; ROOT_SIZE];
        let mut runtime = vec![0; RUNTIME_POINTERS_SIZE];
        let mut hw = vec![0; HW_GLOBALS_SIZE];

        // GFX1 without the +0x481 buffer fails closed.
        let mut address = addresses(0x3000_0000);
        address.cross_role_2 = 0x7000_0000;
        assert_eq!(
            encode_grounded_subset(
                FirmwareRole::Gfx1,
                &address,
                &[400],
                &mut root,
                &mut runtime,
                &mut hw,
            ),
            Err(InitdataError::NullRequiredAddress)
        );

        // GFX carrying a GFX1-only buffer fails closed.
        let mut address = addresses(0x3000_0000);
        address.gfx1_runtime_buffer = 0x7100_0000;
        assert_eq!(
            encode_grounded_subset(
                FirmwareRole::Gfx,
                &address,
                &[400],
                &mut root,
                &mut runtime,
                &mut hw,
            ),
            Err(InitdataError::RoleSpecificAddress)
        );
    }

    #[test]
    fn a_null_shared_sub_allocation_fails_closed() {
        let mut root = vec![0; ROOT_SIZE];
        let mut runtime = vec![0; RUNTIME_POINTERS_SIZE];
        let mut hw = vec![0; HW_GLOBALS_SIZE];
        let mut address = addresses(0x4000_0000);
        address.shared_runtime_allocations[2] = 0;
        assert_eq!(
            encode_grounded_subset(
                FirmwareRole::Gfx,
                &address,
                &[400],
                &mut root,
                &mut runtime,
                &mut hw,
            ),
            Err(InitdataError::NullRequiredAddress)
        );
    }

    #[test]
    fn invalid_addresses_and_role_specific_fields_fail_closed() {
        let mut address = addresses(0x1000_0000);
        let mut root = vec![0; ROOT_SIZE];
        let mut runtime = vec![0; RUNTIME_POINTERS_SIZE];
        let mut hw = vec![0; HW_GLOBALS_SIZE];

        address.root = 1 << 44;
        assert_eq!(
            encode_grounded_subset(
                FirmwareRole::Gfx,
                &address,
                &[400],
                &mut root,
                &mut runtime,
                &mut hw,
            ),
            Err(InitdataError::AddressOutOfRange)
        );

        address = addresses(0x1000_0000);
        address.cross_role_2 = 0x7000_0000;
        assert_eq!(
            encode_grounded_subset(
                FirmwareRole::Gfx,
                &address,
                &[400],
                &mut root,
                &mut runtime,
                &mut hw,
            ),
            Err(InitdataError::RoleSpecificAddress)
        );
    }

    #[test]
    fn pair_requires_one_shared_object_and_distinct_role_roots() {
        let first = addresses(0x1000_0000);
        let mut second = addresses(0x2000_0000);
        second.cross_role_2 = 0x7000_0000;
        assert!(validate_pair(&[first, second]).is_ok());

        second.shared += 0x4000;
        assert_eq!(
            validate_pair(&[first, second]),
            Err(InitdataError::PairSharedPointerMismatch)
        );
        second.shared = first.shared;
        second.root = first.root;
        assert_eq!(
            validate_pair(&[first, second]),
            Err(InitdataError::PairAliasedRoot)
        );
    }

    #[test]
    fn root_gate_errors_are_distinct() {
        let address = addresses(0x1000_0000);
        let (mut root, _, _) = encode(FirmwareRole::Gfx, &address);

        root[0] ^= 1;
        assert_eq!(
            check_firmware_root_gates(&root),
            Err(InitdataError::InterfaceMismatch)
        );
        put_u64(&mut root, 0, INITDATA_MAGIC);
        put_u32(&mut root, ROOT_HOST_MAPPED_ALLOCS, 0);
        assert_eq!(
            check_firmware_root_gates(&root),
            Err(InitdataError::HostMappedAllocationsDisabled)
        );
    }
}
