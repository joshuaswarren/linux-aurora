// SPDX-License-Identifier: GPL-2.0-only OR MIT

#![cfg_attr(not(test), allow(dead_code))]

//! Evidence-backed T8140/G17P firmware init-data layout.
//!
//! HISTORY / PROVENANCE. The previous revision of this module was derived from
//! the **M5 / G17S** on-disk firmware `GFX.a010.macho` (parser at VA
//! 0xfffffc000002127c) — a different chip and a different firmware build from
//! the target, **A18 Pro / T8140 / G17P**. G17S and G17P share a generation
//! name but not an ABI: every structure size in that model was wrong, the five
//! `+0x254` pointers were modeled as independent allocations instead of views
//! into one bundle, and the M5 magic 0x0c8d_e339_0728_04c0 is rejected by the
//! T8140 firmware with "Host and FW Interface is mismatched". This revision is
//! re-derived from:
//!   - live T8140 hardware runs (Mac17,5 / J700 / T8140), which booted both
//!     firmware instances with a source-constructed descriptor;
//!   - our own J700 firmware image: the version-word gate is the MOVZ/MOVK
//!     chain at `__TEXT+0x1e8a8` (`mov x10,#0x4c0; movk #0x396,lsl 16;
//!     movk #0xa322,lsl 32; movk #0xc8a,lsl 48`), and the same routine
//!     confirms the endpoint-0x20 doorbell contract
//!     (`(msg & 0xbf<<48) == 0x81<<48`, address rebuilt as
//!     `msg | 0xfffff0_0000_0000_0000`);
//!   - the executable serializers in the m1n1 tree
//!     (`proxyclient/m1n1/agx/g17p.py`, `g17p_initdata.py`) and
//!     `docs/t8140-g17p-firmware-abi-spec.md`, both validated by byte-exact
//!     rebuilds of pre-handoff captures.
//!
//! The object graph: each firmware instance is handed one root (`0xb8` bytes
//! for the primary, `0xc8` for the secondary). The root carries the version
//! tuple, the full UAT geometry (page size, level count, and three 0x20-byte
//! level descriptors), and points at a per-instance main configuration
//! (`0x600`), the shared region A (16 KiB), the shared region C (`0x1000`),
//! and per-instance status blocks. The main configuration points at the shared
//! hardware-data object (`0x3db4`, at the base of a contiguous bundle
//! allocation of at least `0x10000`) and carries a 17-entry channel table.
//! The five bare pointers at main `+0x254` are views into that one bundle;
//! allocating them independently breaks the overlap and offset relations
//! firmware relies on.
//!
//! Byte slices are used instead of packed Rust structures because several
//! proven fields are unaligned (main `+0x254`, main `+0x471`, and the region
//! record address at `+0x0c`).

use core::ops::Range;
use core::result::Result;

/// The recovered fields are sufficient for offline validation and encoding.
pub(crate) const GROUNDED_LAYOUT_AVAILABLE: bool = true;
/// The init-data object graph the T8140 firmware *accepts and boots with* —
/// per-role root sizes, the version tuple, the UAT geometry block, the main
/// configuration, the bundle-view relations, the 17-entry channel table, the
/// hardware-data acceptance block, region C, and the status blocks — is
/// grounded by live T8140 boots of a source-constructed descriptor. This is
/// what `INITDATA_GRAPH` gates.
pub(crate) const PARSE_ACCEPTED_GRAPH_AVAILABLE: bool = true;
/// Whether every hardware-data scalar the firmware reads is grounded to a
/// concrete *derivation* (rather than a recorded value). It is not: the
/// performance ladders/voltage columns and the recorded opaque platform
/// constants ([`HW_DATA_OPAQUE_RUNS`], [`REGION_C_RUNS`]) still lack an ADT
/// derivation, and this module does not encode the performance tables at all
/// (see [`PERF_TABLE_VALUES_ENCODED`]).
pub(crate) const ALL_HW_GLOBALS_VALUES_GROUNDED: bool = false;
/// The complete hardware-validated T8140 performance tables are encoded.
/// Their eventual ADT derivation remains future work, so they are retained as
/// target-scoped measured values rather than inferred from an AGX2 config.
pub(crate) const PERF_TABLE_VALUES_ENCODED: bool = true;
/// The sparse *content* of the five bundle views and of the bundle's third
/// page (`MAIN_ADDR_OBJECTS` / `HWDATA_BUNDLE_STATIC_RUNS` in `g17p.py`,
/// several KiB of recorded runs) is not yet carried by this module; only the
/// view topology is. A live boot needs those runs staged into the bundle.
pub(crate) const BUNDLE_VIEW_CONTENT_CARRIED: bool = true;
/// The primary status-B pointer names a large (`0xeb00`) status/configuration
/// object, not merely the 0x80-byte block modeled here: it holds FW-control
/// state/ring pointers at `+0x48e0`/`+0x48e8`, a lifecycle header at
/// `+0xe434`, and sparse configuration from `+0xe440`
/// (`NATIVE_PRIMARY_STATUS_B_CONFIG_RUNS` in `g17p.py`). Not yet carried.
pub(crate) const PRIMARY_STATUS_B_FULL_OBJECT_CARRIED: bool = true;

// --- Object sizes ------------------------------------------------------------

/// Root size for the primary (`gfx-asc`) instance.
pub(crate) const ROOT_SIZE_PRIMARY: usize = 0x0b8;
/// Root size for the secondary (`gfx1-asc`) instance; the extra 0x10 bytes are
/// the two secondary-only pointers at `+0xb8`/`+0xc0`.
pub(crate) const ROOT_SIZE_SECONDARY: usize = 0x0c8;
pub(crate) const MAIN_CONFIG_SIZE: usize = 0x600;
pub(crate) const HW_DATA_SIZE: usize = 0x3db4;
pub(crate) const REGION_A_SIZE: usize = 0x4000;
pub(crate) const REGION_C_SIZE: usize = 0x1000;
pub(crate) const STATUS_BLOCK_SIZE: usize = 0x80;

/// The hardware-data bundle: one contiguous allocation with the hardware-data
/// object at its base. Its principal content spans `0xc000`, but the repeated
/// writable region at `+0xc500` (named by main `+0x08`/`+0x10`) lies past
/// that, so the allocation must reach at least `0x10000`.
pub(crate) const HW_DATA_BUNDLE_SIZE: usize = 0xc000;
pub(crate) const HW_DATA_BUNDLE_MIN_ALLOC: usize = 0x10000;
/// The native placement additionally hosts the twelve work rings and both main
/// configuration objects inside the same cluster, which then spans this much
/// (see [`WORK_RING_BUNDLE_OFFSETS`], [`NATIVE_PRIMARY_MAIN_BUNDLE_OFFSET`]).
pub(crate) const NATIVE_SHARED_CLUSTER_SIZE: usize = 0x28000;

// --- Version tuple ------------------------------------------------------------

/// Root `+0x00`: four u16 version words, `{0x04c0, 0x0396, 0xa322, 0x0c8a}`.
/// Grounded twice: read out of every live T8140 root, and independently
/// re-derived from the MOVZ/MOVK chain at `__TEXT+0x1e8a8` of our own J700
/// firmware image. A mismatch is rejected with "Host and FW Interface is
/// mismatched" (the M5/G17S image wants 0x0c8d_e339_0728_04c0 instead — that
/// value must never be sent to T8140 firmware).
pub(crate) const INITDATA_VERSION: [u16; 4] = [0x04c0, 0x0396, 0xa322, 0x0c8a];
/// The same tuple as the little-endian u64 the firmware compares in one go.
pub(crate) const INITDATA_VERSION_WORD: u64 = 0x0c8a_a322_0396_04c0;

// --- Address representability --------------------------------------------------

// The endpoint-0x20 doorbell carries the root address in a 44-bit field; the
// J700 firmware rebuilds it as `msg | 0xfffff0_0000_0000_0000`. Every pointer
// placed in the graph must therefore be either bare or carry exactly that
// kernel-half prefix, or it silently truncates.
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
/// be set to [`CONTROL_OPENING_PRODUCER`] before the descriptor is handed
/// over.
const MAIN_CONTROL_RING: usize = 0x4c0;
pub(crate) const CONTROL_OPENING_OPCODE_PRIMARY: u32 = 0x16;
pub(crate) const CONTROL_OPENING_OPCODE_SECONDARY: u32 = 0x2a;
pub(crate) const CONTROL_OPENING_PRODUCER: u32 = 1;
pub(crate) const CONTROL_RECORD_SIZE: usize = 0x40;
pub(crate) const CONTROL_OPENING_PRIMARY_RECORDS: usize = 4;
pub(crate) const CONTROL_OPENING_SECONDARY_RECORDS: usize = 16;
pub(crate) const CONTROL_OPENING_PRIMARY_PRODUCER: u32 = 4;
pub(crate) const CONTROL_OPENING_SECONDARY_PRODUCER: u32 = 16;
pub(crate) const CONTROL_OPENING_PRIMARY_REGISTER_OPCODE: u32 = 0x20;
pub(crate) const CONTROL_OPENING_SECONDARY_CONTINUE_OPCODE: u32 = 0x22;

pub(crate) const CONTROL_SHARED_ADDRESS: u64 = 0xffff_fc20_c083_0000;
pub(crate) const CONTROL_SHARED_INNER_ADDRESS: u64 = 0xffff_fc20_0160_8000;
pub(crate) const CONTROL_SHARED_OBJECT_SIZE: usize = 0x4000;
pub(crate) const CONTROL_SHARED_CURSOR_OFFSET: usize = 0x48;
pub(crate) const CONTROL_SHARED_CURSOR_BEFORE: u32 = 0x88;
pub(crate) const CONTROL_OPERAND_TABLE_ADDRESS: u64 = 0x70_0020_8000;
pub(crate) const CONTROL_OPERAND_TABLE_SIZE: usize = 0x4000;
pub(crate) const CONTROL_OPERAND_SLOT_OFFSET: u64 = 0x440;
pub(crate) const CONTROL_OPERAND_BUFFER_BASE: u64 = 0x70_0022_0000;
pub(crate) const CONTROL_OPERAND_BUFFER_STRIDE: u64 = 0x10_8000;
pub(crate) const CONTROL_OPERAND_BUFFER_SIZE: u64 = 0x10_0000;
pub(crate) const CONTROL_OPERAND_ENTRY_COUNT: usize = 22;
pub(crate) const CONTROL_OPERAND_ENTRY_STRIDE: usize = 0x40;
pub(crate) const CONTROL_OPERAND_ENTRY_FLAG: u64 = 1 << 60;

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

/// Encode the complete control ring prefix consumed during the 25G83 opening.
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
            for slot in 0..3 {
                put_u32(
                    out,
                    slot * CONTROL_RECORD_SIZE,
                    CONTROL_OPENING_OPCODE_PRIMARY,
                );
            }
            let base = 3 * CONTROL_RECORD_SIZE;
            put_u32(out, base, CONTROL_OPENING_PRIMARY_REGISTER_OPCODE);
            put_u32(out, base + 0x04, 1);
            put_u32(out, base + 0x08, 0x3f);
            put_u64(out, base + 0x14, CONTROL_SHARED_ADDRESS);
            put_u64(out, base + 0x1c, CONTROL_OPERAND_TABLE_ADDRESS);
            put_u64(
                out,
                base + 0x24,
                CONTROL_OPERAND_TABLE_ADDRESS + CONTROL_OPERAND_SLOT_OFFSET,
            );
            put_u32(out, base + 0x2c, 0x28);
            put_u32(out, base + 0x34, 1);
        }
        InstanceRole::Secondary => {
            for slot in 0..3 {
                put_u32(
                    out,
                    slot * CONTROL_RECORD_SIZE,
                    CONTROL_OPENING_OPCODE_SECONDARY,
                );
            }
            for slot in 3..CONTROL_OPENING_SECONDARY_RECORDS {
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

/// Encode the shared object named by the primary opcode-0x20 record.
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
    put_u32(
        out,
        CONTROL_SHARED_CURSOR_OFFSET,
        CONTROL_SHARED_CURSOR_BEFORE,
    );
    put_u64(out, 0x4c, CONTROL_SHARED_INNER_ADDRESS);
    Ok(())
}

/// Encode the 22 flagged buffer pointers present before the first control start.
pub(crate) fn encode_control_operand_table(out: &mut [u8]) -> Result<(), InitdataError> {
    if out.len() != CONTROL_OPERAND_TABLE_SIZE {
        return Err(InitdataError::BufferSize);
    }
    out.fill(0);
    for index in 0..CONTROL_OPERAND_ENTRY_COUNT {
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

/// Bundle offsets of the five views named at main `+0x254`. They are views of
/// the common bundle, not independent objects: view 0 overlaps the tail of
/// the hardware-data object, and views 3 and 4 overlap each other by all but
/// 0x80 bytes.
pub(crate) const BUNDLE_VIEW_OFFSETS: [usize; 5] = [0x2740, 0x3380, 0x4400, 0xbc80, 0xbd00];
/// Logical extents of the five views.
pub(crate) const BUNDLE_VIEW_EXTENTS: [usize; 5] = [0x18c0, 0x0c80, 0x3c00, 0x3380, 0x3300];

/// The two low/context-0 addresses in the region views at main `+0x2d0`.
/// Captured as `(value, 0x70)` u32 pairs, which are really the low and high
/// halves of full context-0 virtual addresses; the secondary carries the same
/// two values with no high aliases at all, so they are not properties of any
/// region the host allocates.
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

/// Two 0x40-byte hardware region records at `+0x2610`, each naming a 16 KiB
/// region: `(lead, value, trail)` per record; kind `0x70` at `+0x08`, the
/// unaligned region FW DVA at `+0x0c`, `0x800` at `+0x14`, `0x40` at `+0x18`.
pub(crate) const HW_REGION_RECORDS_OFFSET: usize = 0x2610;
pub(crate) const HW_REGION_RECORD_STRIDE: usize = 0x40;
pub(crate) const HW_REGION_RECORDS: [(u32, u32, u32); 2] =
    [(0x100, 0x0184_8000, 0), (0, 0x0187_0000, 2)];
const HW_REGION_RECORD_KIND: u32 = 0x70;
const HW_REGION_RECORD_SIZE_A: u32 = 0x800;
const HW_REGION_RECORD_SIZE_B: u32 = 0x40;
/// Native bundle offsets the two region records point at.
pub(crate) const NATIVE_HW_REGION_BUNDLE_OFFSETS: [usize; 2] = [0x48000, 0x70000];

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
const HW_PERF_RELATIVE_A_VALUES: [u32; HW_PERF_LADDER_ENTRIES] =
    [0, 16, 23, 30, 43, 57, 74, 85, 90, 92, 100];
const HW_PERF_RELATIVE_B_VALUES: [u32; HW_PERF_LADDER_ENTRIES] =
    [0, 0, 13, 24, 40, 52, 63, 73, 83, 88, 100];
const HW_PERF_INDEX_A_VALUES: [u32; HW_PERF_LADDER_ENTRIES] =
    [0, 1, 2, 3, 4, 5, 7, 9, 11, 13, 15];
const HW_PERF_INDEX_B_VALUES: [u32; HW_PERF_LADDER_ENTRIES] =
    [0, 1, 2, 3, 4, 5, 6, 8, 10, 12, 14];
const HW_PERF_CORE_VOLTAGE: [u32; HW_PERF_LADDER_ENTRIES] =
    [125, 595, 615, 635, 685, 730, 780, 815, 865, 885, 955];
const HW_PERF_MEMORY_VOLTAGE: [u32; HW_PERF_LADDER_ENTRIES] =
    [765, 765, 765, 765, 765, 765, 780, 815, 865, 885, 955];

// --- Region C ---------------------------------------------------------------------

/// Region C `+0xe50` must hold this u32 (bytes `00 01 00 00`) before the
/// first compact class-1 registration. Leaving it zero makes the *primary*
/// firmware take an asynchronous SError while consuming device-control opcode
/// `0x20`.
pub(crate) const REGION_C_REQUIRED_OFFSET: usize = 0xe50;
pub(crate) const REGION_C_REQUIRED_VALUE: u32 = 0x0000_0100;

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
pub(crate) enum InstanceRole {
    /// `gfx-asc`: scheduler, device control, and all work channels.
    Primary = 0,
    /// `gfx1-asc`: control-only peer. Started *before* the primary.
    Secondary = 1,
}

pub(crate) const fn root_size(role: InstanceRole) -> usize {
    match role {
        InstanceRole::Primary => ROOT_SIZE_PRIMARY,
        InstanceRole::Secondary => ROOT_SIZE_SECONDARY,
    }
}

/// GPU virtual addresses of the objects both instances share.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) struct SharedAddresses {
    /// Shared 16 KiB region A (root `+0x08`).
    pub(crate) region_a: u64,
    /// Shared region C (root `+0x20`), 0x1000 bytes.
    pub(crate) region_c: u64,
    /// Base of the contiguous hardware-data bundle. The hardware-data object
    /// sits at the base; the five main-config views, the repeated region, and
    /// the secondary's `+0x471` pointer are all derived offsets into it.
    pub(crate) hw_data_bundle: u64,
    /// Size of that one allocation; must be at least
    /// [`HW_DATA_BUNDLE_MIN_ALLOC`].
    pub(crate) hw_data_bundle_alloc: usize,
}

/// GPU virtual addresses of one instance's private objects.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) struct InstanceAddresses {
    /// This instance's root; its address rides the endpoint-0x20 doorbell.
    pub(crate) root: u64,
    pub(crate) main_config: u64,
    pub(crate) status_a: u64,
    /// Required for the primary; must be zero for the secondary (whose root
    /// still has the field, written zero).
    pub(crate) status_b: u64,
    /// Root `+0xb8`/`+0xc0`. Required for the secondary; the primary root is
    /// only 0xb8 bytes and cannot even hold them, so they must be zero there.
    pub(crate) secondary_extras: [u64; 2],
}

/// One channel-table entry: three state-counter addresses, then the ring base.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) struct ChannelEntry {
    pub(crate) states: [u64; 3],
    pub(crate) ring: u64,
}

impl ChannelEntry {
    pub(crate) const EMPTY: ChannelEntry = ChannelEntry {
        states: [0; 3],
        ring: 0,
    };
}

/// The primary's region views at main `+0x2d0`: a high-only blank sentinel,
/// then the high halves of two low/high alias pairs (the low halves are the
/// fixed context-0 addresses in [`REGION_VIEW_LOW_ADDRS`]). Each pair must map
/// one physical page from both address spaces — an invariant of the UAT
/// mapping layer that cannot be checked from the addresses alone.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) struct PrimaryRegionViews {
    pub(crate) sentinel_high: u64,
    pub(crate) alias_high: [u64; 2],
}

/// One register-window declaration for the hardware-data mapping array.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) struct RegisterWindow {
    pub(crate) slot: usize,
    pub(crate) phys: u64,
    pub(crate) device_va: u64,
    /// Byte count; written at both `+0x10` and `+0x14`. Not always a granule
    /// multiple.
    pub(crate) size: u32,
    /// Low 24 bits of the physical address for accelerator-internal windows,
    /// else zero.
    pub(crate) unk_18: u64,
    pub(crate) flag: u32,
}

/// T8140 register windows mapped into the firmware MMIO aperture.
pub(crate) const T8140_REGISTER_WINDOWS: [RegisterWindow; 18] = [
    RegisterWindow {
        slot: 0,
        phys: 0x0030_1014_000,
        device_va: 0xffff_fc21_8000_0000,
        size: 0x004000,
        unk_18: 0,
        flag: 2,
    },
    RegisterWindow {
        slot: 3,
        phys: 0x0022_0104_000,
        device_va: 0xffff_fc21_8000_8000,
        size: 0x018000,
        unk_18: 0,
        flag: 2,
    },
    RegisterWindow {
        slot: 9,
        phys: 0x0030_03d0_000,
        device_va: 0xffff_fc21_8002_8000,
        size: 0x001000,
        unk_18: 0,
        flag: 2,
    },
    RegisterWindow {
        slot: 10,
        phys: 0x0030_03c0_000,
        device_va: 0xffff_fc21_8003_0000,
        size: 0x002000,
        unk_18: 0,
        flag: 0,
    },
    RegisterWindow {
        slot: 12,
        phys: 0x0040_165c_000,
        device_va: 0xffff_fc21_8003_8000,
        size: 0x004000,
        unk_18: 0,
        flag: 2,
    },
    RegisterWindow {
        slot: 14,
        phys: 0x0030_0280_000,
        device_va: 0xffff_fc21_8004_0000,
        size: 0x008000,
        unk_18: 0,
        flag: 0,
    },
    RegisterWindow {
        slot: 17,
        phys: 0x0048_0000_000,
        device_va: 0xffff_fc21_8005_0000,
        size: 0x021400,
        unk_18: 0,
        flag: 2,
    },
    RegisterWindow {
        slot: 22,
        phys: 0x0048_1000_000,
        device_va: 0xffff_fc21_8007_8000,
        size: 0x008000,
        unk_18: 0,
        flag: 2,
    },
    RegisterWindow {
        slot: 26,
        phys: 0x0048_0d04_000,
        device_va: 0xffff_fc21_8008_8000,
        size: 0x008000,
        unk_18: 0xd04000,
        flag: 2,
    },
    RegisterWindow {
        slot: 27,
        phys: 0x0048_0d0d_000,
        device_va: 0xffff_fc21_8009_9000,
        size: 0x001000,
        unk_18: 0xd0d000,
        flag: 2,
    },
    RegisterWindow {
        slot: 28,
        phys: 0x0048_0d58_000,
        device_va: 0xffff_fc21_800a_0000,
        size: 0x008000,
        unk_18: 0xd58000,
        flag: 2,
    },
    RegisterWindow {
        slot: 29,
        phys: 0x0048_0d10_000,
        device_va: 0xffff_fc21_800b_0000,
        size: 0x004000,
        unk_18: 0xd10000,
        flag: 2,
    },
    RegisterWindow {
        slot: 31,
        phys: 0x0048_0d40_000,
        device_va: 0xffff_fc21_800b_8000,
        size: 0x004000,
        unk_18: 0xd40000,
        flag: 2,
    },
    RegisterWindow {
        slot: 32,
        phys: 0x0048_0d60_000,
        device_va: 0xffff_fc21_800c_0000,
        size: 0x004000,
        unk_18: 0xd60000,
        flag: 2,
    },
    RegisterWindow {
        slot: 35,
        phys: 0x0048_0e00_000,
        device_va: 0xffff_fc21_800c_8000,
        size: 0x004000,
        unk_18: 0xe00000,
        flag: 2,
    },
    RegisterWindow {
        slot: 39,
        phys: 0x0048_0e08_000,
        device_va: 0xffff_fc21_800d_0000,
        size: 0x008000,
        unk_18: 0,
        flag: 2,
    },
    RegisterWindow {
        slot: 40,
        phys: 0x0048_0e1c_000,
        device_va: 0xffff_fc21_800e_0000,
        size: 0x004000,
        unk_18: 0xe1c000,
        flag: 2,
    },
    RegisterWindow {
        slot: 41,
        phys: 0x0048_0e1f_800,
        device_va: 0xffff_fc21_800e_b800,
        size: 0x004000,
        unk_18: 0,
        flag: 2,
    },
];

pub(crate) const T8140_REGISTER_FLAG_ONLY_SLOTS: [(usize, u32); 17] = [
    (2, 2),
    (5, 2),
    (6, 2),
    (7, 2),
    (8, 2),
    (30, 2),
    (33, 2),
    (34, 2),
    (37, 2),
    (38, 2),
    (42, 2),
    (43, 2),
    (46, 2),
    (47, 2),
    (48, 2),
    (49, 2),
    (52, 2),
];

/// Fail-closed validation errors for the grounded layout.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) enum InitdataError {
    BufferSize,
    AddressOutOfRange,
    NullRequiredAddress,
    RoleSpecificAddress,
    BundleAllocationTooSmall,
    ChannelTableShape,
    RegisterWindow,
    PairRootSeparation,
    PairAliasedObject,
    SharedRegionSeparation,
    InterfaceMismatch,
    HostMappedAllocationsDisabled,
}

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

fn put_u16(bytes: &mut [u8], offset: usize, value: u16) {
    bytes[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
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

fn write_runs(bytes: &mut [u8], runs: &[(usize, &[u8])]) {
    for (offset, data) in runs {
        bytes[*offset..*offset + data.len()].copy_from_slice(data);
    }
}

fn validate_shared(shared: &SharedAddresses) -> Result<(), InitdataError> {
    required_address(shared.region_a)?;
    required_address(shared.region_c)?;
    required_address(shared.hw_data_bundle)?;
    if shared.hw_data_bundle_alloc < HW_DATA_BUNDLE_MIN_ALLOC {
        return Err(InitdataError::BundleAllocationTooSmall);
    }
    Ok(())
}

fn validate_instance(role: InstanceRole, inst: &InstanceAddresses) -> Result<(), InitdataError> {
    required_address(inst.root)?;
    required_address(inst.main_config)?;
    required_address(inst.status_a)?;
    match role {
        InstanceRole::Primary => {
            // The primary root has no +0xb8/+0xc0 fields at all; carrying
            // secondary extras there is a role-confusion bug at the caller.
            if inst.secondary_extras != [0, 0] {
                return Err(InitdataError::RoleSpecificAddress);
            }
            required_address(inst.status_b)?;
        }
        InstanceRole::Secondary => {
            // Every hardware-booting host hands the secondary a zero status-B;
            // a nonzero value there is ungrounded, so fail closed on it.
            if inst.status_b != 0 {
                return Err(InitdataError::RoleSpecificAddress);
            }
            for extra in inst.secondary_extras {
                required_address(extra)?;
            }
        }
    }
    Ok(())
}

/// Encode one 0x20-byte UAT level descriptor. Every field is derivable from
/// the level's index shift and entry count; the index mask
/// `(entries - 1) << shift` reproduces all three captured masks exactly.
fn encode_uat_level_descriptor(bytes: &mut [u8], offset: usize, shift: u8, entries: u16) {
    bytes[offset] = 8;
    bytes[offset + 1] = UAT_PAGE_BITS;
    bytes[offset + 2] = UAT_PAGE_BITS;
    bytes[offset + 3] = shift;
    put_u16(bytes, offset + 0x04, entries);
    put_u16(bytes, offset + 0x06, UAT_PAGE_SIZE);
    put_u64(bytes, offset + 0x08, 1);
    put_u64(bytes, offset + 0x10, UAT_LEVEL_PHYS_MASK);
    put_u64(bytes, offset + 0x18, (entries as u64 - 1) << shift);
}

/// Encode one instance's root.
///
/// The output is zeroed first, which makes unknown bytes deterministic and,
/// critically, prevents stale host memory from becoming invented firmware
/// ABI. Root `+0x10` stays zero: it is zero at handoff on both instances and
/// its semantics are unknown.
pub(crate) fn encode_root(
    role: InstanceRole,
    shared: &SharedAddresses,
    inst: &InstanceAddresses,
    out: &mut [u8],
) -> Result<(), InitdataError> {
    if out.len() != root_size(role) {
        return Err(InitdataError::BufferSize);
    }
    validate_shared(shared)?;
    validate_instance(role, inst)?;

    out.fill(0);
    for (index, word) in INITDATA_VERSION.iter().enumerate() {
        put_u16(out, ROOT_VERSION + index * 2, *word);
    }
    put_u64(out, ROOT_REGION_A, shared.region_a);
    // ROOT_ZERO_10 deliberately left zero.
    put_u64(out, ROOT_MAIN_CONFIG, inst.main_config);
    put_u64(out, ROOT_REGION_C, shared.region_c);
    put_u32(out, ROOT_INSTANCE_KIND, role as u32);
    put_u32(out, ROOT_CONSTANT_2C, 1);
    put_u16(out, ROOT_PAGE_SIZE, UAT_PAGE_SIZE);
    out[ROOT_PAGE_BITS] = UAT_PAGE_BITS;
    out[ROOT_LEVEL_COUNT] = UAT_LEVEL_COUNT as u8;
    for (index, (shift, entries)) in UAT_LEVELS.iter().enumerate() {
        encode_uat_level_descriptor(
            out,
            ROOT_LEVELS + index * UAT_LEVEL_DESC_SIZE,
            *shift,
            *entries,
        );
    }
    put_u64(out, ROOT_STATUS_A, inst.status_a);
    put_u64(out, ROOT_STATUS_B, inst.status_b);
    if role == InstanceRole::Secondary {
        put_u64(out, ROOT_SECONDARY_EXTRA_0, inst.secondary_extras[0]);
        put_u64(out, ROOT_SECONDARY_EXTRA_1, inst.secondary_extras[1]);
    }
    Ok(())
}

/// Byte range of one channel-table entry inside the main configuration.
pub(crate) fn channel_table_entry_range(index: usize) -> Option<Range<usize>> {
    if index >= CHANNEL_TABLE_ENTRIES {
        return None;
    }
    let start = MAIN_CHANNEL_TABLE + index * CHANNEL_ENTRY_SIZE;
    Some(start..start + CHANNEL_ENTRY_SIZE)
}

/// Derive the native channel table for one instance.
///
/// `state_grid` is this instance's compact channel-state block (0x40-stride,
/// permuted per [`WORK_STATE_GRID_OFFSETS`]); `status_a` anchors the trailing
/// report/partial channels; the ring for entry 12 is `main_config + 0x4c0`;
/// work rings for the primary sit in the bundle at
/// [`WORK_RING_BUNDLE_OFFSETS`] (which requires the
/// [`NATIVE_SHARED_CLUSTER_SIZE`] cluster). The secondary leaves entries
/// 0..11 empty: it is a control-only peer with no work channels.
pub(crate) fn derive_channel_table(
    role: InstanceRole,
    state_grid: u64,
    status_a: u64,
    main_config: u64,
    bundle: u64,
) -> [ChannelEntry; CHANNEL_TABLE_ENTRIES] {
    let mut table = [ChannelEntry::EMPTY; CHANNEL_TABLE_ENTRIES];
    let grid_states = |index: usize| {
        let base = state_grid + WORK_STATE_GRID_OFFSETS[index] as u64;
        [
            base,
            base + CHANNEL_STATE_SPACING as u64,
            base + 2 * CHANNEL_STATE_SPACING as u64,
        ]
    };
    if role == InstanceRole::Primary {
        for index in 0..WORK_CHANNEL_COUNT {
            table[index] = ChannelEntry {
                states: grid_states(index),
                ring: bundle + WORK_RING_BUNDLE_OFFSETS[index] as u64,
            };
        }
    }
    table[CONTROL_CHANNEL_INDEX] = ChannelEntry {
        states: grid_states(CONTROL_CHANNEL_INDEX),
        ring: main_config + MAIN_CONTROL_RING as u64,
    };
    for (slot, index) in REPORT_CHANNEL_INDICES.iter().enumerate() {
        let mut states = [0u64; 3];
        for (word, offset) in states.iter_mut().zip(REPORT_STATE_STATUS_A_OFFSETS[slot]) {
            *word = status_a + offset as u64;
        }
        table[*index] = ChannelEntry {
            states,
            ring: status_a + REPORT_RING_STATUS_A_OFFSETS[slot] as u64,
        };
    }
    table[PARTIAL_CHANNEL_INDEX] = ChannelEntry {
        states: [status_a + PARTIAL_STATE_STATUS_A_OFFSET as u64, 0, 0],
        ring: 0,
    };
    table
}

fn validate_populated_entry(entry: &ChannelEntry) -> Result<(), InitdataError> {
    // A populated channel needs all three state-counter addresses — a null
    // producer (state 2) on a populated entry makes firmware fault on address
    // zero — and a real ring.
    for state in entry.states {
        if state == 0 {
            return Err(InitdataError::ChannelTableShape);
        }
        if !address_representable(state) {
            return Err(InitdataError::AddressOutOfRange);
        }
    }
    required_address(entry.ring)
}

fn validate_channel_table(
    role: InstanceRole,
    main_config: u64,
    channels: &[ChannelEntry; CHANNEL_TABLE_ENTRIES],
) -> Result<(), InitdataError> {
    for (index, entry) in channels.iter().enumerate() {
        match index {
            0..=11 => match role {
                InstanceRole::Primary => validate_populated_entry(entry)?,
                // The secondary has no work channels at all.
                InstanceRole::Secondary => {
                    if *entry != ChannelEntry::EMPTY {
                        return Err(InitdataError::ChannelTableShape);
                    }
                }
            },
            12 => {
                validate_populated_entry(entry)?;
                // Device control is embedded in the main object, at exactly
                // main + 0x4c0 on both instances.
                if entry.ring != main_config + MAIN_CONTROL_RING as u64 {
                    return Err(InitdataError::ChannelTableShape);
                }
            }
            13 | 14 => validate_populated_entry(entry)?,
            15 => {
                // Partial: a first state address and nothing else.
                required_address(entry.states[0])?;
                if entry.states[1] != 0 || entry.states[2] != 0 || entry.ring != 0 {
                    return Err(InitdataError::ChannelTableShape);
                }
            }
            _ => {
                if *entry != ChannelEntry::EMPTY {
                    return Err(InitdataError::ChannelTableShape);
                }
            }
        }
    }
    Ok(())
}

/// Encode one instance's 0x600-byte main configuration object.
///
/// `region_views` must be `Some` for the primary and `None` for the
/// secondary: the secondary's region views carry the two low context-0
/// values with no addresses at all.
pub(crate) fn encode_main_config(
    role: InstanceRole,
    shared: &SharedAddresses,
    inst: &InstanceAddresses,
    channels: &[ChannelEntry; CHANNEL_TABLE_ENTRIES],
    region_views: Option<&PrimaryRegionViews>,
    out: &mut [u8],
) -> Result<(), InitdataError> {
    if out.len() != MAIN_CONFIG_SIZE {
        return Err(InitdataError::BufferSize);
    }
    validate_shared(shared)?;
    validate_instance(role, inst)?;
    validate_channel_table(role, inst.main_config, channels)?;
    match (role, region_views) {
        (InstanceRole::Primary, Some(views)) => {
            required_address(views.sentinel_high)?;
            for high in views.alias_high {
                required_address(high)?;
            }
        }
        (InstanceRole::Secondary, None) => {}
        _ => return Err(InitdataError::RoleSpecificAddress),
    }
    // Every view must fit inside the one bundle allocation. (The last two end
    // exactly at 0xf000, inside the 0x10000 minimum.)
    for (offset, extent) in BUNDLE_VIEW_OFFSETS.iter().zip(BUNDLE_VIEW_EXTENTS) {
        if offset + extent > shared.hw_data_bundle_alloc {
            return Err(InitdataError::BundleAllocationTooSmall);
        }
    }

    out.fill(0);
    let bundle = shared.hw_data_bundle;
    put_u64(out, MAIN_HW_DATA, bundle);
    let repeated = bundle + MAIN_REPEATED_REGION_BUNDLE_OFFSET as u64;
    put_u64(out, MAIN_REPEATED_0, repeated);
    put_u64(out, MAIN_REPEATED_1, repeated);

    for (index, entry) in channels.iter().enumerate() {
        let base = MAIN_CHANNEL_TABLE + index * CHANNEL_ENTRY_SIZE;
        for (word, state) in entry.states.iter().enumerate() {
            put_u64(out, base + word * 8, *state);
        }
        put_u64(out, base + CHANNEL_ENTRY_STATE_COUNT * 8, entry.ring);
    }

    match (role, region_views) {
        (InstanceRole::Primary, Some(views)) => {
            // The five bare pointers are views into the one bundle
            // allocation, at fixed offsets. Unaligned stores at +0x254.
            for (index, offset) in BUNDLE_VIEW_OFFSETS.iter().enumerate() {
                put_u64(out, MAIN_BUNDLE_VIEWS + index * 8, bundle + *offset as u64);
            }
            // Six region-view qwords: high-only sentinel, two low/high alias
            // pairs, null terminator.
            put_u64(out, MAIN_REGION_VIEWS, views.sentinel_high);
            put_u64(out, MAIN_REGION_VIEWS + 0x08, REGION_VIEW_LOW_ADDRS[0]);
            put_u64(out, MAIN_REGION_VIEWS + 0x10, views.alias_high[0]);
            put_u64(out, MAIN_REGION_VIEWS + 0x18, REGION_VIEW_LOW_ADDRS[1]);
            put_u64(out, MAIN_REGION_VIEWS + 0x20, views.alias_high[1]);
            // +0x2f0 stays zero.
            put_u32(out, MAIN_PRIMARY_SCALAR_32C, 1);
            put_u32(out, MAIN_PRIMARY_SCALAR_344, 0xabcd_abcd);
            put_u32(out, MAIN_PRIMARY_MASK, MAIN_PRIMARY_MASK_VALUE);
            put_u32(out, MAIN_CONTROL_RING, CONTROL_OPENING_OPCODE_PRIMARY);
        }
        (InstanceRole::Secondary, _) => {
            // The secondary keeps the two low context-0 values but carries no
            // view or region addresses; the values are therefore not
            // properties of any region the host allocates.
            put_u64(out, MAIN_REGION_VIEWS + 0x08, REGION_VIEW_LOW_ADDRS[0]);
            put_u64(out, MAIN_REGION_VIEWS + 0x18, REGION_VIEW_LOW_ADDRS[1]);
            put_u32(out, MAIN_SECONDARY_KIND, MAIN_SECONDARY_KIND_VALUE);
            // One additional unaligned pointer *into* primary view 2, landing
            // five bytes before that view's first populated run.
            let extra = bundle
                + BUNDLE_VIEW_OFFSETS[SECONDARY_EXTRA_VIEW_INDEX] as u64
                + SECONDARY_EXTRA_VIEW_OFFSET as u64;
            put_u64(out, MAIN_SECONDARY_EXTRA_PTR, extra);
            put_u32(out, MAIN_CONTROL_RING, CONTROL_OPENING_OPCODE_SECONDARY);
        }
        _ => unreachable!(),
    }
    Ok(())
}

/// Encode the shared 0x3db4-byte hardware-data object (the base of the
/// bundle). Both instances reference this one object; handing the secondary a
/// private copy makes it fault.
///
/// `region_record_addrs` are the FW DVAs of the two 16 KiB regions named by
/// the records at `+0x2610` (natively at bundle offsets
/// [`NATIVE_HW_REGION_BUNDLE_OFFSETS`]).
pub(crate) fn encode_hw_data(
    windows: &[RegisterWindow],
    flag_only_slots: &[(usize, u32)],
    region_record_addrs: [u64; 2],
    out: &mut [u8],
) -> Result<(), InitdataError> {
    if out.len() != HW_DATA_SIZE {
        return Err(InitdataError::BufferSize);
    }
    let mut used = [false; HW_REGISTER_MAP_SLOTS];
    let mut claim = |slot: usize| -> Result<(), InitdataError> {
        if slot >= HW_REGISTER_MAP_SLOTS || used[slot] {
            return Err(InitdataError::RegisterWindow);
        }
        used[slot] = true;
        Ok(())
    };
    for window in windows {
        claim(window.slot)?;
        if window.phys == 0 || window.size == 0 {
            return Err(InitdataError::RegisterWindow);
        }
        // Firmware reaches its registers only through this table; the device
        // VAs live in one fixed aperture.
        if !HW_REGISTER_APERTURE.contains(&window.device_va) {
            return Err(InitdataError::RegisterWindow);
        }
    }
    for (slot, _flag) in flag_only_slots {
        claim(*slot)?;
    }
    for address in region_record_addrs {
        required_address(address)?;
    }

    out.fill(0);
    for window in windows {
        let base = HW_REGISTER_MAP_OFFSET + window.slot * HW_REGISTER_MAP_ENTRY_SIZE;
        put_u64(out, base, window.phys);
        put_u64(out, base + 0x08, window.device_va);
        put_u32(out, base + 0x10, window.size);
        put_u32(out, base + 0x14, window.size);
        put_u64(out, base + 0x18, window.unk_18);
        put_u32(out, base + 0x20, window.flag);
    }
    for (slot, flag) in flag_only_slots {
        let base = HW_REGISTER_MAP_OFFSET + slot * HW_REGISTER_MAP_ENTRY_SIZE;
        put_u32(out, base + 0x20, *flag);
    }

    put_u32(out, HW_CHIP_ID_OFFSET, HW_CHIP_ID_T8140);

    for index in 0..HW_PERF_LADDER_ENTRIES {
        put_u32(out, HW_PERF_GROUP_BASES[0] + index * 4, HW_PERF_FREQ_A[index]);
        put_u32(out, HW_PERF_FREQ_LADDER_B + index * 4, HW_PERF_FREQ_B[index]);
        put_u32(out, HW_PERF_SCALE_LADDER_B + index * 4, HW_PERF_SCALE_B[index]);
        put_u32(
            out,
            HW_PERF_RELATIVE_LADDER_A + index * 4,
            HW_PERF_RELATIVE_A_VALUES[index],
        );
        put_u32(
            out,
            HW_PERF_RELATIVE_LADDER_B + index * 4,
            HW_PERF_RELATIVE_B_VALUES[index],
        );
        put_u32(out, HW_PERF_INDEX_MAP_A + index * 4, HW_PERF_INDEX_A_VALUES[index]);
        put_u32(out, HW_PERF_INDEX_MAP_B + index * 4, HW_PERF_INDEX_B_VALUES[index]);
        put_u32(out, HW_PERF_GROUP_BASES[1] + index * 4, HW_PERF_FREQ_B[index]);

        for repeat in 0..HW_PERF_GROUP_REPEATS[0] {
            put_u32(
                out,
                HW_PERF_GROUP_BASES[0]
                    + HW_PERF_VOLTAGE_DELTA
                    + index * HW_PERF_STATE_BLOCK_STRIDE
                    + repeat * 4,
                HW_PERF_CORE_VOLTAGE[index],
            );
            put_u32(
                out,
                HW_PERF_GROUP_BASES[0]
                    + HW_PERF_MEMORY_VOLTAGE_DELTA
                    + index * HW_PERF_STATE_BLOCK_STRIDE
                    + repeat * 4,
                HW_PERF_MEMORY_VOLTAGE[index],
            );
        }
        put_u32(
            out,
            HW_PERF_GROUP_BASES[1]
                + HW_PERF_VOLTAGE_DELTA
                + index * HW_PERF_STATE_BLOCK_STRIDE,
            HW_PERF_CORE_VOLTAGE[index],
        );
        put_u32(
            out,
            HW_PERF_GROUP_BASES[1]
                + HW_PERF_MEMORY_VOLTAGE_DELTA
                + index * HW_PERF_STATE_BLOCK_STRIDE,
            HW_PERF_MEMORY_VOLTAGE[index],
        );
    }

    // The acceptance block: derived from the work-channel count rather than
    // copied, so a constructed descriptor needs nothing from a capture here.
    put_u16(out, HW_ACCEPT_CHANNEL_COUNT, WORK_CHANNEL_COUNT as u16);
    for group in HW_ACCEPT_FLAG_GROUPS {
        for (index, value) in HW_ACCEPT_FLAG_PATTERN.iter().enumerate() {
            put_u32(out, group + index * 4, *value);
        }
    }
    for channel in 0..WORK_CHANNEL_COUNT {
        put_u32(out, HW_ACCEPT_UNASSIGNED_TABLE + channel * 4, 0xffff_ffff);
    }
    for offset in HW_ACCEPT_TRAILING_ONES {
        put_u32(out, offset, 1);
    }

    for (index, ((lead, value, trail), address)) in HW_REGION_RECORDS
        .iter()
        .zip(region_record_addrs)
        .enumerate()
    {
        let base = HW_REGION_RECORDS_OFFSET + index * HW_REGION_RECORD_STRIDE;
        put_u32(out, base, *lead);
        put_u32(out, base + 0x04, *value);
        put_u32(out, base + 0x08, HW_REGION_RECORD_KIND);
        // Unaligned region FW DVA.
        put_u64(out, base + 0x0c, address);
        put_u32(out, base + 0x14, HW_REGION_RECORD_SIZE_A);
        put_u32(out, base + 0x18, HW_REGION_RECORD_SIZE_B);
        put_u32(out, base + 0x1c, *trail);
    }

    // Recorded platform constants. Descriptor *acceptance* survives without
    // them, but the device-control phase that follows does not. +0x2630 is
    // deliberately absent from the runs: it is firmware state and must start
    // zero (the fill above guarantees it).
    write_runs(out, HW_DATA_OPAQUE_RUNS);
    // Performance tables intentionally left zero; see
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
    if root.len() != ROOT_SIZE_PRIMARY && root.len() != ROOT_SIZE_SECONDARY {
        return Err(InitdataError::BufferSize);
    }
    if get_u64(root, ROOT_VERSION) != INITDATA_VERSION_WORD {
        return Err(InitdataError::InterfaceMismatch);
    }
    if get_u32(root, ROOT_CONSTANT_2C) == 0 {
        return Err(InitdataError::HostMappedAllocationsDisabled);
    }
    Ok(())
}

/// Recorded T8140 hardware-data platform constants (values known, meanings
/// not; generated from the hardware-validated `g17p_initdata.py`
/// `HWDATA_CONSTANTS`). 29 sparse runs, 269 bytes total. `+0x2630` is
/// deliberately absent: it is firmware state, not host configuration.
pub(crate) const HW_DATA_OPAQUE_RUNS: &[(usize, &[u8])] = &[
    (0x0004, &[0x6f, 0x00, 0x00, 0x00, 0x00, 0x00, 0xc0, 0xff]),
    (
        0x0014,
        &[
            0x10, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x10, 0x00, 0x00, 0x00, 0x00, 0x80,
            0xff, 0xff, 0xff, 0x02, 0x00, 0x00, 0x00, 0x00, 0x40, 0x81, 0x21, 0xfc, 0xff, 0xff,
        ],
    ),
    (0x00e0, &[0x08, 0x20]),
    (0x00ea, &[0x08, 0x20]),
    (
        0x00f4,
        &[
            0x08, 0x20, 0x00, 0x00, 0xcb, 0x24, 0x00, 0x00, 0xfa, 0x2c, 0x8a, 0xc9, 0xcb, 0x24,
            0xf6, 0xf4, 0x17, 0xe9, 0x77, 0x18, 0xcb, 0x24, 0xd9, 0x38, 0x00, 0x00, 0xab, 0xbd,
        ],
    ),
    (
        0x02d9,
        &[
            0x20, 0xf8, 0xff, 0xdb, 0x2c, 0x2d, 0xd3, 0x00, 0x20, 0x00, 0xf5, 0x26, 0xe9, 0xda,
            0x21, 0x00, 0x20, 0xb6, 0x38, 0x08, 0x00, 0x42, 0xc7,
        ],
    ),
    (0x03e0, &[0xe0, 0x7f]),
    (0x03ea, &[0xe0, 0x7f]),
    (0x03f4, &[0xe0, 0x7f, 0x00, 0x00, 0x00, 0x80]),
    (0x0403, &[0x80]),
    (0x040d, &[0x80]),
    (
        0x05d8,
        &[
            0x45, 0x26, 0x23, 0x4b, 0x98, 0x0e, 0x00, 0x00, 0x5f, 0xea, 0xa2, 0xd5, 0xff, 0x3f,
            0x00, 0x40, 0x00, 0x40, 0x5e, 0xca, 0xa2, 0xf5, 0x00, 0x40,
        ],
    ),
    (
        0x0e94,
        &[
            0x01, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00,
            0x00, 0x00, 0x01,
        ],
    ),
    (
        0x0eb8,
        &[
            0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xc0, 0x5d, 0x00, 0x00,
            0x01, 0x00, 0x00, 0x00, 0x10, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00,
            0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01,
        ],
    ),
    (0x0f04, &[0x1f]),
    (0x0f24, &[0x04]),
    (0x0f34, &[0x01, 0x00, 0x00, 0x00, 0x01]),
    (0x0f4c, &[0x31]),
    (0x0f6c, &[0x01]),
    (0x0f88, &[0x06, 0x00, 0x00, 0x00, 0x01]),
    (0x0fac, &[0x01]),
    (
        0x0fb8,
        &[
            0x1e, 0x00, 0x00, 0x00, 0x04, 0x00, 0x00, 0x00, 0x06, 0x00, 0x00, 0x00, 0x0a,
        ],
    ),
    (0x1cd8, &[0x0a]),
    (
        0x2544,
        &[
            0x04, 0x00, 0x00, 0x00, 0x06, 0x00, 0x00, 0x00, 0x03, 0x00, 0x00, 0x00, 0x07, 0x00,
            0x00, 0x00, 0x07,
        ],
    ),
    (
        0x2568,
        &[0x05, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01],
    ),
    (
        0x26ad,
        &[
            0x80, 0x5d, 0x01, 0x20, 0xfc, 0xff, 0xff, 0x01, 0x00, 0x00, 0x00, 0x01,
        ],
    ),
    (0x26e8, &[0x01]),
    (0x3d98, &[0xff, 0xff, 0xff, 0xff]),
    (0x3db0, &[0xff, 0xff, 0xff, 0xff]),
];

/// Recorded T8140 region-C platform tuning constants (values known, meanings
/// not; generated from the hardware-validated `g17p_initdata.py`
/// `REGION_C_CONSTANTS`). 12 sparse runs, 180 bytes total, including the
/// required word at [`REGION_C_REQUIRED_OFFSET`].
pub(crate) const REGION_C_RUNS: &[(usize, &[u8])] = &[
    (0x0024, &[0xb8, 0x0b, 0x00, 0x00]),
    (
        0x0030,
        &[
            0x01, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x78, 0x00,
            0x00, 0x00,
        ],
    ),
    (
        0x0054,
        &[
            0xff, 0xff, 0x28, 0x00, 0xff, 0xff, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00,
            0x01, 0x00, 0x00, 0x00, 0x03, 0x00, 0x00, 0x00, 0x01, 0x00,
        ],
    ),
    (0x0078, &[0x01, 0x00, 0x00, 0x00]),
    (
        0x0084,
        &[
            0x4e, 0x25, 0x00, 0x00, 0xe8, 0x03, 0x00, 0x00, 0xe8, 0x03, 0x00, 0x00,
        ],
    ),
    (0x0098, &[0xe8, 0x03, 0x00, 0x00]),
    (
        0x07c4,
        &[
            0xd0, 0x07, 0x00, 0x00, 0x00, 0x00, 0x80, 0x3f, 0xcd, 0xcc, 0x4c, 0x3f, 0xcd, 0xcc,
            0x4c, 0x3e, 0x66, 0x66, 0x66, 0x3f, 0xcd, 0xcc, 0xcc, 0x3d, 0x00, 0x00, 0x80, 0x3e,
            0x9a, 0x99, 0x19, 0x3f, 0x66, 0x66, 0x66, 0x3f, 0x06, 0x00, 0x00, 0x00, 0x01, 0x00,
            0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x64, 0x00, 0x00, 0x00, 0x64, 0x00, 0x00, 0x00,
        ],
    ),
    (
        0x0998,
        &[
            0x28, 0x00, 0x00, 0x00, 0x0a, 0x00, 0x00, 0x00, 0xfa, 0x00, 0x00, 0x00, 0x01, 0x00,
            0x00, 0x00,
        ],
    ),
    (
        0x09b8,
        &[
            0xc0, 0x27, 0x09, 0x00, 0xc0, 0x27, 0x09, 0x00, 0x05, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x28, 0x00, 0x00, 0x00, 0x32, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00,
        ],
    ),
    (0x0e1c, &[0x00, 0x01, 0x00, 0x00]),
    (0x0e38, &[0x00, 0x03, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00]),
    // Required before the first compact class-1 registration; leaving it
    // clear SErrors the primary on device-control opcode 0x20.
    (0x0e50, &[0x00, 0x01, 0x00, 0x00]),
];

pub(crate) const BUNDLE_VIEW_RUNS: [&[(usize, &[u8])]; 5] = [
    &[
        (0x1658, &[0xff, 0xff, 0xff, 0xff]),
        (0x1670, &[0xff, 0xff, 0xff, 0xff]),
    ],
    &[
        (0xa18, &[0xff, 0xff, 0xff, 0xff]),
        (0xa30, &[0xff, 0xff, 0xff, 0xff]),
    ],
    &[
        (0xe45, &[0xdc, 0x05, 0x00, 0x00, 0xdc, 0x05, 0x00, 0x00, 0x00, 0x00, 0x00, 0x04, 0x00, 0x00, 0x00, 0x00, 0x00, 0x80, 0x3f]),
        (0xe6c, &[0x01, 0x00, 0x00, 0x00, 0x01]),
        (0xe80, &[0x64, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0xe8, 0x03, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x64]),
        (0xea0, &[0x04, 0x00, 0x00, 0x00, 0x00, 0x00, 0x80, 0x3f, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01]),
        (0xec0, &[0x64, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0xe8, 0x03, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x64]),
        (0xf00, &[0x5c, 0x8f, 0x82, 0x3f, 0x5c, 0x8f, 0x82, 0x3f, 0x5c, 0x8f, 0x82, 0x3f, 0x5c, 0x8f, 0x82, 0x3f, 0x5c, 0x8f, 0x82, 0x3f, 0x5c, 0x8f, 0x82, 0x3f, 0x5c, 0x8f, 0x82, 0x3f, 0x5c, 0x8f, 0x82, 0x3f, 0x5c, 0x8f, 0x82, 0x3f, 0x5c, 0x8f, 0x82, 0x3f, 0x5c, 0x8f, 0x82, 0x3f]),
        (0x17f8, &[0x71, 0x02]),
        (0x1804, &[0x9f, 0x2e, 0x7f, 0x3f, 0x00, 0x00, 0x00, 0x00, 0x54, 0x61, 0x51, 0x3b, 0x00, 0x00, 0x00, 0x00, 0x86, 0x95, 0xa5, 0x3c]),
        (0x1821, &[0x38, 0x15, 0x46, 0xdb, 0x0f, 0xa9, 0x40, 0x00, 0x00, 0x00, 0x00, 0x44, 0x73, 0xd6, 0xbd, 0x28, 0x00, 0x00, 0x00, 0xe8, 0x03, 0x00, 0x00, 0xe8, 0x03]),
        (0x1844, &[0x4e, 0x25]),
        (0x1878, &[0xe8, 0x03]),
        (0x18b0, &[0x04]),
        (0x18c6, &[0x80, 0x3f, 0x00, 0x00, 0x00, 0x00, 0xcd, 0xcc, 0xcc, 0x40]),
        (0x18da, &[0x80, 0x47, 0x00, 0x00, 0x00, 0x3f, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x28, 0x00, 0x00, 0x00, 0xe8, 0x03]),
        (0x18fc, &[0x4e, 0x25]),
        (0x1908, &[0x10, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xdc, 0x05]),
        (0x1968, &[0x5c, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x64, 0x00, 0x00, 0x00, 0x22, 0x00, 0x00, 0x00, 0x06, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x06, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xcd, 0xcc, 0x4c, 0x3f, 0xb6, 0xf3, 0x7d, 0x3f, 0xcd, 0xcc, 0x4c, 0x3e, 0x6f, 0x12, 0x03, 0x3c, 0xd8, 0xd4, 0x69, 0x3f, 0xd8, 0xd4, 0x69, 0x3f, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xbe, 0x42, 0xc3, 0xf5, 0x64, 0x40, 0xc3, 0xf5, 0x64, 0x40, 0x36, 0x94, 0x17, 0x41, 0x64, 0x00, 0x00, 0x00, 0xe8, 0x03, 0x00, 0x00, 0x64]),
        (0x19cc, &[0x5c]),
        (0x1a00, &[0x64]),
        (0x1a20, &[0x01, 0x04, 0x00, 0x00, 0x00, 0x64, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x40, 0x1f, 0x00, 0x00, 0xc8, 0x00, 0x00, 0x00, 0xa0, 0x0f, 0x00, 0x00, 0xc8, 0x00, 0x00, 0x00, 0xd0, 0x07, 0x00, 0x00, 0xc8, 0x00, 0x00, 0x00, 0xe8, 0x03, 0x00, 0x00, 0xc8, 0x00, 0x00, 0x00, 0x01]),
        (0x1aa4, &[0x01]),
        (0x1ad0, &[0x01]),
        (0x30da, &[0x80, 0x47, 0x00, 0x00, 0x20, 0x42, 0x00, 0x00, 0x7a, 0x44, 0xbe, 0x05]),
        (0x30f8, &[0x28]),
        (0x3102, &[0xc8, 0x42, 0xe8, 0x03, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xcd, 0xcc, 0x4c, 0x3f, 0xcd, 0xcc, 0x4c, 0x3e]),
        (0x3150, &[0x2a, 0x08, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x40, 0x1f, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x04]),
        (0x3176, &[0x80, 0x3f, 0x00, 0x00, 0x00, 0x00, 0x19, 0x04, 0x16, 0x41]),
        (0x318a, &[0x80, 0x47, 0x52, 0xb8, 0xa2, 0x41, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x28, 0x00, 0x00, 0x00, 0xe8, 0x03, 0x00, 0x00, 0xe8, 0x03]),
        (0x31ac, &[0xf8, 0x2a]),
        (0x31e0, &[0xe8, 0x03]),
        (0x33fc, &[0x06, 0x00, 0x00, 0x42]),
        (0x3612, &[0x52, 0xb8, 0x7e, 0x3f, 0x00, 0x00, 0x00, 0x00, 0x0a, 0xd7, 0xa3, 0x3b, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xc8, 0x43]),
        (0x3630, &[0x80, 0x47, 0x00, 0x00, 0xc8, 0x42, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xc8, 0xba, 0x84, 0x03, 0x00, 0x00, 0xe8, 0x03, 0x00, 0x00, 0xe8, 0x03, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x80, 0x3b, 0x45, 0xac, 0x26]),
        (0x3686, &[0xe8, 0x03]),
        (0x36a2, &[0xc8]),
        (0x3758, &[0xe8, 0x03]),
        (0x376c, &[0x01]),
        (0x37a0, &[0x01, 0x01, 0x00, 0x00, 0x00, 0x04]),
        (0x37bd, &[0x3f, 0x02, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01]),
        (0x37dd, &[0x40, 0x02, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01]),
        (0x37fd, &[0x41, 0x02]),
        (0x3848, &[0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x4e, 0x25, 0x00, 0x00, 0x4e, 0x25, 0x00, 0x00, 0x4e, 0x25]),
        (0x386a, &[0x80, 0x3f, 0x04, 0x00, 0x00, 0x00, 0x10, 0x00, 0x00, 0x00, 0x00, 0xdc, 0x05]),
        (0x3944, &[0x3c]),
        (0x3950, &[0xef, 0xee, 0x6e, 0x3f, 0x00, 0x00, 0x00, 0x00, 0x89, 0x88, 0x88, 0x3d, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x3f]),
        (0x396e, &[0x80, 0x47, 0x00, 0x00, 0x88, 0x40, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x28, 0x00, 0x00, 0x00, 0xe8, 0x03, 0x00, 0x00, 0xe8, 0x03, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x38, 0x15, 0x46, 0x4e, 0x25]),
        (0x399c, &[0xf0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xe4, 0x57]),
        (0x39c4, &[0xe8, 0x03]),
        (0x3a04, &[0x01]),
        (0x3ad4, &[0x32, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x39, 0x8e, 0x63, 0x3f, 0xab, 0xaa, 0x2a, 0x3f, 0x39, 0x8e, 0xe3, 0x3d, 0xab, 0xaa, 0xaa, 0x3e, 0xcd, 0xcc, 0x4c, 0xbf, 0xcd, 0xcc, 0x4c, 0xbf, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x80, 0x47, 0x00, 0x00, 0xa0, 0xc0, 0x00, 0x00, 0xa0, 0xc0, 0x00, 0x00, 0x00, 0x00, 0x64, 0x00, 0x00, 0x00, 0xe8, 0x03, 0x00, 0x00, 0x64]),
        (0x3b1e, &[0x7a, 0x46, 0x78, 0x05]),
        (0x3b2c, &[0x90, 0x00, 0x00, 0x00, 0x30, 0x00, 0x00, 0x00, 0x00, 0xbc, 0x34, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x94, 0x11]),
        (0x3b4a, &[0x80, 0x47]),
        (0x3b70, &[0x80, 0x3e, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00, 0xc4, 0x09, 0x00, 0x00, 0x0d, 0x02, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00, 0x04, 0x00, 0x00, 0x00, 0x32]),
        (0x3bba, &[0x80, 0x47]),
        (0x3bc8, &[0x28, 0x00, 0x00, 0x00, 0xe8, 0x03]),
    ],
    &[
        (0x15c5, &[0xdc, 0x05, 0x00, 0x00, 0xdc, 0x05, 0x00, 0x00, 0x00, 0x00, 0x00, 0x04, 0x00, 0x00, 0x00, 0x00, 0x00, 0x80, 0x3f]),
        (0x15ec, &[0x01, 0x00, 0x00, 0x00, 0x01]),
        (0x1600, &[0x64, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0xe8, 0x03, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x64]),
        (0x1620, &[0x04, 0x00, 0x00, 0x00, 0x00, 0x00, 0x80, 0x3f, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01]),
        (0x1640, &[0x64, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0xe8, 0x03, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x64]),
        (0x1680, &[0x5c, 0x8f, 0x82, 0x3f, 0x5c, 0x8f, 0x82, 0x3f, 0x5c, 0x8f, 0x82, 0x3f, 0x5c, 0x8f, 0x82, 0x3f, 0x5c, 0x8f, 0x82, 0x3f, 0x5c, 0x8f, 0x82, 0x3f, 0x5c, 0x8f, 0x82, 0x3f, 0x5c, 0x8f, 0x82, 0x3f, 0x5c, 0x8f, 0x82, 0x3f, 0x5c, 0x8f, 0x82, 0x3f, 0x5c, 0x8f, 0x82, 0x3f]),
        (0x1f78, &[0x71, 0x02]),
        (0x1f84, &[0x9f, 0x2e, 0x7f, 0x3f, 0x00, 0x00, 0x00, 0x00, 0x54, 0x61, 0x51, 0x3b, 0x00, 0x00, 0x00, 0x00, 0x86, 0x95, 0xa5, 0x3c]),
        (0x1fa1, &[0x38, 0x15, 0x46, 0xdb, 0x0f, 0xa9, 0x40, 0x00, 0x00, 0x00, 0x00, 0x44, 0x73, 0xd6, 0xbd, 0x28, 0x00, 0x00, 0x00, 0xe8, 0x03, 0x00, 0x00, 0xe8, 0x03]),
        (0x1fc4, &[0x4e, 0x25]),
        (0x1ff8, &[0xe8, 0x03]),
        (0x2030, &[0x04]),
        (0x2046, &[0x80, 0x3f, 0x00, 0x00, 0x00, 0x00, 0xcd, 0xcc, 0xcc, 0x40]),
        (0x205a, &[0x80, 0x47, 0x00, 0x00, 0x00, 0x3f, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x28, 0x00, 0x00, 0x00, 0xe8, 0x03]),
        (0x207c, &[0x4e, 0x25]),
        (0x2088, &[0x10, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xdc, 0x05]),
        (0x20e8, &[0x5c, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x64, 0x00, 0x00, 0x00, 0x22, 0x00, 0x00, 0x00, 0x06, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x06, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xcd, 0xcc, 0x4c, 0x3f, 0xb6, 0xf3, 0x7d, 0x3f, 0xcd, 0xcc, 0x4c, 0x3e, 0x6f, 0x12, 0x03, 0x3c, 0xd8, 0xd4, 0x69, 0x3f, 0xd8, 0xd4, 0x69, 0x3f, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xbe, 0x42, 0xc3, 0xf5, 0x64, 0x40, 0xc3, 0xf5, 0x64, 0x40, 0x36, 0x94, 0x17, 0x41, 0x64, 0x00, 0x00, 0x00, 0xe8, 0x03, 0x00, 0x00, 0x64]),
        (0x214c, &[0x5c]),
        (0x2180, &[0x64]),
        (0x21a0, &[0x01, 0x04, 0x00, 0x00, 0x00, 0x64, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x40, 0x1f, 0x00, 0x00, 0xc8, 0x00, 0x00, 0x00, 0xa0, 0x0f, 0x00, 0x00, 0xc8, 0x00, 0x00, 0x00, 0xd0, 0x07, 0x00, 0x00, 0xc8, 0x00, 0x00, 0x00, 0xe8, 0x03, 0x00, 0x00, 0xc8, 0x00, 0x00, 0x00, 0x01]),
        (0x2224, &[0x01]),
        (0x2250, &[0x01]),
    ],
    &[
        (0x1545, &[0xdc, 0x05, 0x00, 0x00, 0xdc, 0x05, 0x00, 0x00, 0x00, 0x00, 0x00, 0x04, 0x00, 0x00, 0x00, 0x00, 0x00, 0x80, 0x3f]),
        (0x156c, &[0x01, 0x00, 0x00, 0x00, 0x01]),
        (0x1580, &[0x64, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0xe8, 0x03, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x64]),
        (0x15a0, &[0x04, 0x00, 0x00, 0x00, 0x00, 0x00, 0x80, 0x3f, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01]),
        (0x15c0, &[0x64, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0xe8, 0x03, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x64]),
        (0x1600, &[0x5c, 0x8f, 0x82, 0x3f, 0x5c, 0x8f, 0x82, 0x3f, 0x5c, 0x8f, 0x82, 0x3f, 0x5c, 0x8f, 0x82, 0x3f, 0x5c, 0x8f, 0x82, 0x3f, 0x5c, 0x8f, 0x82, 0x3f, 0x5c, 0x8f, 0x82, 0x3f, 0x5c, 0x8f, 0x82, 0x3f, 0x5c, 0x8f, 0x82, 0x3f, 0x5c, 0x8f, 0x82, 0x3f, 0x5c, 0x8f, 0x82, 0x3f]),
        (0x1ef8, &[0x71, 0x02]),
        (0x1f04, &[0x9f, 0x2e, 0x7f, 0x3f, 0x00, 0x00, 0x00, 0x00, 0x54, 0x61, 0x51, 0x3b, 0x00, 0x00, 0x00, 0x00, 0x86, 0x95, 0xa5, 0x3c]),
        (0x1f21, &[0x38, 0x15, 0x46, 0xdb, 0x0f, 0xa9, 0x40, 0x00, 0x00, 0x00, 0x00, 0x44, 0x73, 0xd6, 0xbd, 0x28, 0x00, 0x00, 0x00, 0xe8, 0x03, 0x00, 0x00, 0xe8, 0x03]),
        (0x1f44, &[0x4e, 0x25]),
        (0x1f78, &[0xe8, 0x03]),
        (0x1fb0, &[0x04]),
        (0x1fc6, &[0x80, 0x3f, 0x00, 0x00, 0x00, 0x00, 0xcd, 0xcc, 0xcc, 0x40]),
        (0x1fda, &[0x80, 0x47, 0x00, 0x00, 0x00, 0x3f, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x28, 0x00, 0x00, 0x00, 0xe8, 0x03]),
        (0x1ffc, &[0x4e, 0x25]),
        (0x2008, &[0x10, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xdc, 0x05]),
        (0x2068, &[0x5c, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x64, 0x00, 0x00, 0x00, 0x22, 0x00, 0x00, 0x00, 0x06, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x06, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xcd, 0xcc, 0x4c, 0x3f, 0xb6, 0xf3, 0x7d, 0x3f, 0xcd, 0xcc, 0x4c, 0x3e, 0x6f, 0x12, 0x03, 0x3c, 0xd8, 0xd4, 0x69, 0x3f, 0xd8, 0xd4, 0x69, 0x3f, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xbe, 0x42, 0xc3, 0xf5, 0x64, 0x40, 0xc3, 0xf5, 0x64, 0x40, 0x36, 0x94, 0x17, 0x41, 0x64, 0x00, 0x00, 0x00, 0xe8, 0x03, 0x00, 0x00, 0x64]),
        (0x20cc, &[0x5c]),
        (0x2100, &[0x64]),
        (0x2120, &[0x01, 0x04, 0x00, 0x00, 0x00, 0x64, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x40, 0x1f, 0x00, 0x00, 0xc8, 0x00, 0x00, 0x00, 0xa0, 0x0f, 0x00, 0x00, 0xc8, 0x00, 0x00, 0x00, 0xd0, 0x07, 0x00, 0x00, 0xc8, 0x00, 0x00, 0x00, 0xe8, 0x03, 0x00, 0x00, 0xc8, 0x00, 0x00, 0x00, 0x01]),
        (0x21a4, &[0x01]),
        (0x21d0, &[0x01]),
    ],
];
pub(crate) const BUNDLE_VIEW_U16_REPEATS: &[(usize, usize, usize, bool)] = &[
    (2, 0x2ad7, 512, false),
    (3, 0x3257, 148, true),
    (4, 0x31d7, 148, true),
];

pub(crate) const BUNDLE_STATIC_RUNS: &[(usize, &[u8])] = &[
    (0x80ac, &[0x06]),
    (0x80b8, &[0x01]),
    (0x8177, &[0xac, 0x0d]),
    (0x817b, &[0xe8, 0x03]),
    (0x817f, &[0xb8, 0x0b]),
    (0x8183, &[0xe8, 0x03]),
    (0x8187, &[0xe8, 0x03]),
    (0x818b, &[0x64]),
    (0x818f, &[0x20]),
    (0x81b4, &[0x04]),
    (0x81c2, &[0x78, 0x3f]),
    (0x81cb, &[0x3d]),
    (0x81d2, &[0xa0, 0x40]),
    (0x81de, &[0x80, 0x47]),
    (0x81e2, &[0x70, 0x41]),
    (0x81ec, &[0x84, 0x03]),
    (0x81f0, &[0xe8, 0x03]),
    (0x81f4, &[0xe8, 0x03]),
    (0x8200, &[0xe8, 0x03]),
    (0x8234, &[0xe8, 0x03]),
    (0x8274, &[0x64]),
    (0x8290, &[0x2a, 0x08]),
    (0x8298, &[0x7d]),
    (0x829c, &[0x01]),
    (0x8ee0, &[0xae, 0x18]),
    (0x8ee8, &[0x01]),
    (0xaeec, &[0xcd, 0xcc, 0xc8, 0x41, 0xcd, 0xcc, 0xc8, 0x41]),
    (0xb5e4, &[0x01]),
    (0xb5e8, &[0xf4, 0x01]),
    (0xb5ec, &[0x06]),
    (0xb5f0, &[0xd4, 0x30]),
    (0xb5f4, &[0xd4, 0x30]),
    (0xb5f8, &[0xd4, 0x30]),
    (0xb5fc, &[0xd4, 0x30]),
    (0xb600, &[0xd4, 0x30]),
    (0xb604, &[0xd4, 0x30]),
    (0xb608, &[0xa0, 0x0f]),
    (0xb61c, &[0xd4, 0x30]),
    (0xb620, &[0xd4, 0x30]),
    (0xb624, &[0xd4, 0x30]),
    (0xb628, &[0xd4, 0x30]),
    (0xb62c, &[0xd4, 0x30]),
    (0xb630, &[0x01]),
    (0xb7a8, &[0x01]),
    (0xb7b4, &[0xcd, 0xcc, 0x4c, 0x42]),
    (0xb7f5, &[0x80, 0x06, 0x44]),
    (0xb834, &[0x9a, 0x99, 0xc9, 0x41]),
    (0xb8e0, &[0x6a, 0x18]),
];

pub(crate) const PRIMARY_STATUS_B_CONFIG_RUNS: &[(usize, &[u8])] = &[
    (0x010, &[0x0f]),
    (0x017, &[0x3f]),
    (0x01a, &[0x88, 0x40, 0x28]),
    (0x020, &[0x01]),
    (0x0c8, &[0xf8, 0x2a]),
    (0x0cc, &[0x40, 0x1f]),
    (0x0d8, &[0x52, 0xb8, 0xa2, 0x41, 0x19, 0x04, 0x16, 0x41]),
    (0x0f0, &[0xac, 0x26]),
    (0x0f4, &[0xc8]),
    (0x10a, &[0xc8, 0x42]),
    (0x10e, &[0xc8, 0x43, 0xc8]),
    (0x128, &[0x01]),
    (0x12c, &[0x4e, 0x25]),
    (0x133, &[0x3f, 0xcd, 0xcc, 0xcc, 0x40]),
    (0x14c, &[0x01]),
    (0x1c0, &[0x01]),
    (0x1c4, &[0x01]),
    (0x1c8, &[0x04]),
    (0x1cc, &[0x01]),
    (0x1d0, &[0x01]),
    (0x1d4, &[0x01]),
    (0x1d8, &[0x01]),
    (0x4e4, &[0x01]),
    (0x4e8, &[0xf4, 0x01]),
    (0x4ec, &[0x06]),
    (0x4f0, &[0xd4, 0x30]),
    (0x4f4, &[0xd4, 0x30]),
    (0x4f8, &[0xd4, 0x30]),
    (0x4fc, &[0xd4, 0x30]),
    (0x500, &[0xd4, 0x30]),
    (0x504, &[0xd4, 0x30]),
    (0x508, &[0xa0, 0x0f]),
    (0x51c, &[0xd4, 0x30]),
    (0x520, &[0xd4, 0x30]),
    (0x524, &[0xd4, 0x30]),
    (0x528, &[0xd4, 0x30]),
    (0x52c, &[0xd4, 0x30]),
    (0x530, &[0x01]),
    (0x58c, &[0x06]),
    (0x590, &[0xac, 0x0d]),
    (0x594, &[0xe8, 0x03]),
    (0x598, &[0xb8, 0x0b]),
    (0x59c, &[0x64]),
    (0x5cc, &[0x01]),
    (0x5d0, &[0x04]),
    (0x5dc, &[0x40, 0x1f]),
    (0x5e0, &[0xc8]),
    (0x5e4, &[0xa0, 0x0f]),
    (0x5e8, &[0xc8]),
    (0x5ec, &[0xd0, 0x07]),
    (0x5f0, &[0xc8]),
    (0x5f4, &[0xe8, 0x03]),
    (0x5f8, &[0xc8]),
    (0x606, &[0x70, 0x41]),
    (0x60a, &[0xa0, 0x40, 0x20]),
    (0x610, &[0xe8, 0x03]),
    (0x620, &[0x84, 0x03]),
];

pub(crate) const SECONDARY_ROOT_EXTRA_1_RUNS: &[(usize, &[u8])] = &[
    (0x14, &[0x01]),
    (0x2c, &[0x01]),
    (0x30, &[0x01]),
    (0x40, &[0x01]),
    (0x4c, &[0x01]),
    (0x50, &[0x6a, 0x18]),
];

#[cfg(test)]
mod tests {
    use super::*;

    const KERN: u64 = 0xffff_fc20_0000_0000;

    fn shared() -> SharedAddresses {
        SharedAddresses {
            region_a: KERN + 0x1b_8000,
            region_c: KERN + 0x1a_0000,
            hw_data_bundle: KERN + 0xc078_8000,
            hw_data_bundle_alloc: NATIVE_SHARED_CLUSTER_SIZE,
        }
    }

    fn primary() -> InstanceAddresses {
        InstanceAddresses {
            root: KERN + 0x1a_8000,
            main_config: KERN + 0xc078_8000 + NATIVE_PRIMARY_MAIN_BUNDLE_OFFSET as u64,
            status_a: KERN + 0x2_ee40,
            status_b: KERN + 0x2_0000 + PRIMARY_STATUS_B_STATE_GRID_OFFSET as u64,
            secondary_extras: [0, 0],
        }
    }

    fn secondary() -> InstanceAddresses {
        InstanceAddresses {
            root: KERN + 0x1a_8000 + SECONDARY_ROOT_DELTA,
            main_config: KERN + 0xc078_8000 + NATIVE_SECONDARY_MAIN_BUNDLE_OFFSET as u64,
            status_a: KERN + 0xe_3100,
            status_b: 0,
            secondary_extras: [KERN + 0x2_e780, KERN + 0x19_7400],
        }
    }

    fn state_grid(role: InstanceRole) -> u64 {
        match role {
            InstanceRole::Primary => KERN + 0x2_0000,
            InstanceRole::Secondary => KERN + 0x19_70c0,
        }
    }

    fn channels(role: InstanceRole) -> [ChannelEntry; CHANNEL_TABLE_ENTRIES] {
        let inst = match role {
            InstanceRole::Primary => primary(),
            InstanceRole::Secondary => secondary(),
        };
        derive_channel_table(
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
    fn primary_control_opening_is_three_init_records_then_registration() {
        let mut opening = vec![0; control_opening_size(InstanceRole::Primary)];
        encode_control_opening(InstanceRole::Primary, &mut opening).unwrap();

        assert_eq!(control_opening_producer(InstanceRole::Primary), 4);
        for slot in 0..3 {
            let base = slot * CONTROL_RECORD_SIZE;
            assert_eq!(get_u32(&opening, base), CONTROL_OPENING_OPCODE_PRIMARY);
            assert!(opening[base + 4..base + CONTROL_RECORD_SIZE]
                .iter()
                .all(|byte| *byte == 0));
        }
        let base = 3 * CONTROL_RECORD_SIZE;
        assert_eq!(
            get_u32(&opening, base),
            CONTROL_OPENING_PRIMARY_REGISTER_OPCODE
        );
        assert_eq!(get_u32(&opening, base + 0x04), 1);
        assert_eq!(get_u32(&opening, base + 0x08), 0x3f);
        assert_eq!(get_u64(&opening, base + 0x14), CONTROL_SHARED_ADDRESS);
        assert_eq!(
            get_u64(&opening, base + 0x1c),
            CONTROL_OPERAND_TABLE_ADDRESS
        );
        assert_eq!(
            get_u64(&opening, base + 0x24),
            CONTROL_OPERAND_TABLE_ADDRESS + CONTROL_OPERAND_SLOT_OFFSET
        );
        assert_eq!(get_u32(&opening, base + 0x2c), 0x28);
        assert_eq!(get_u32(&opening, base + 0x34), 1);
    }

    #[test]
    fn secondary_control_opening_is_three_init_then_thirteen_continue_records() {
        let mut opening = vec![0; control_opening_size(InstanceRole::Secondary)];
        encode_control_opening(InstanceRole::Secondary, &mut opening).unwrap();

        assert_eq!(control_opening_producer(InstanceRole::Secondary), 16);
        for slot in 0..CONTROL_OPENING_SECONDARY_RECORDS {
            let base = slot * CONTROL_RECORD_SIZE;
            let expected = if slot < 3 {
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
    fn shared_control_object_names_the_fixed_operand_and_inner_pages() {
        let mut shared = vec![0; CONTROL_SHARED_OBJECT_SIZE];
        encode_control_shared(&mut shared).unwrap();

        assert_eq!(get_u64(&shared, 0x00), 1);
        assert_eq!(get_u64(&shared, 0x18), 0x0004_0000_0000_0070);
        assert_eq!(get_u64(&shared, 0x30), CONTROL_OPERAND_TABLE_ADDRESS);
        assert_eq!(
            get_u32(&shared, CONTROL_SHARED_CURSOR_OFFSET),
            CONTROL_SHARED_CURSOR_BEFORE
        );
        assert_eq!(get_u64(&shared, 0x4c), CONTROL_SHARED_INNER_ADDRESS);
        assert_eq!(get_u64(&shared, 0x60), 2);
    }

    #[test]
    fn operand_table_has_exactly_twenty_two_flagged_entries() {
        let mut table = vec![0xff; CONTROL_OPERAND_TABLE_SIZE];
        encode_control_operand_table(&mut table).unwrap();

        for index in 0..CONTROL_OPERAND_ENTRY_COUNT {
            assert_eq!(
                get_u64(&table, index * CONTROL_OPERAND_ENTRY_STRIDE),
                CONTROL_OPERAND_ENTRY_FLAG
                    | (CONTROL_OPERAND_BUFFER_BASE
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

    #[test]
    fn hw_data_size_acceptance_block_regions_and_firmware_state() {
        assert_eq!(HW_DATA_SIZE, 0x3db4);
        let windows = register_windows();
        let flag_only = [(3usize, 2u32), (7, 2)];
        let regions = [
            shared().hw_data_bundle + NATIVE_HW_REGION_BUNDLE_OFFSETS[0] as u64,
            shared().hw_data_bundle + NATIVE_HW_REGION_BUNDLE_OFFSETS[1] as u64,
        ];
        let mut hw = vec![0xa5; HW_DATA_SIZE];
        encode_hw_data(&windows, &flag_only, regions, &mut hw).unwrap();
        assert_eq!(hw.len(), 0x3db4);

        // Chip id at +0xe90.
        assert_eq!(get_u32(&hw, 0xe90), 0x8140);

        // Register-mapping array: 0x28 stride at +0x640.
        let base0 = HW_REGISTER_MAP_OFFSET;
        assert_eq!(get_u64(&hw, base0), 0x2_0000_0000);
        assert_eq!(get_u64(&hw, base0 + 0x08), HW_REGISTER_APERTURE.start);
        assert_eq!(get_u32(&hw, base0 + 0x10), 0x4000);
        assert_eq!(get_u32(&hw, base0 + 0x14), 0x4000);
        assert_eq!(get_u32(&hw, base0 + 0x20), 2);
        // A declared-but-unmapped slot carries only its flag.
        let base3 = HW_REGISTER_MAP_OFFSET + 3 * HW_REGISTER_MAP_ENTRY_SIZE;
        assert_eq!(get_u64(&hw, base3), 0);
        assert_eq!(get_u32(&hw, base3 + 0x20), 2);

        // The +0x258e acceptance block — the one block firmware requires.
        assert_eq!(
            u16::from_le_bytes(hw[0x258e..0x2590].try_into().unwrap()),
            12
        );
        for group in [0x2590usize, 0x25a0] {
            for (index, value) in [0u32, 1, 1, 1].iter().enumerate() {
                assert_eq!(get_u32(&hw, group + index * 4), *value);
            }
        }
        for channel in 0..12 {
            assert_eq!(get_u32(&hw, 0x25b4 + channel * 4), 0xffff_ffff);
        }
        for offset in [0x25f4usize, 0x2600, 0x2608] {
            assert_eq!(get_u32(&hw, offset), 1);
        }
        // +0x2630 is firmware state and must start zero even though the buffer
        // began dirty (0xa5).
        assert_eq!(hw[0x2630], 0);

        // Two 0x40-byte hardware region records at +0x2610.
        assert_eq!(get_u32(&hw, 0x2610), 0x100); // record 0 lead
        assert_eq!(get_u32(&hw, 0x2610 + 0x04), 0x0184_8000); // value
        assert_eq!(get_u32(&hw, 0x2610 + 0x08), 0x70); // kind
        assert_eq!(get_u64(&hw, 0x2610 + 0x0c), regions[0]); // unaligned DVA
        assert_eq!(get_u32(&hw, 0x2610 + 0x14), 0x800);
        assert_eq!(get_u32(&hw, 0x2610 + 0x18), 0x40);
        assert_eq!(get_u32(&hw, 0x2610 + 0x1c), 0); // record 0 trail
        assert_eq!(get_u32(&hw, 0x2650 + 0x04), 0x0187_0000); // record 1 value
        assert_eq!(get_u64(&hw, 0x2650 + 0x0c), regions[1]);
        assert_eq!(get_u32(&hw, 0x2650 + 0x1c), 2); // record 1 trail

        // The recorded opaque runs landed (spot check the two 0xffffffff tails).
        assert_eq!(get_u32(&hw, 0x3d98), 0xffff_ffff);
        assert_eq!(get_u32(&hw, 0x3db0), 0xffff_ffff);

        // Both measured ladders and their voltage columns are carried.
        assert!(PERF_TABLE_VALUES_ENCODED);
        assert_eq!(get_u32(&hw, HW_PERF_GROUP_BASES[0]), 0);
        assert_eq!(get_u32(&hw, HW_PERF_GROUP_BASES[0] + 10 * 4), 1470);
        assert_eq!(get_u32(&hw, HW_PERF_GROUP_BASES[1] + 10 * 4), 1326);
        assert_eq!(get_u32(&hw, HW_PERF_GROUP_BASES[0] + 0x40), 125);
        assert_eq!(get_u32(&hw, HW_PERF_GROUP_BASES[1] + 0x40), 125);
    }

    #[test]
    fn hw_data_rejects_out_of_aperture_and_duplicate_slots() {
        let regions = [
            shared().hw_data_bundle + NATIVE_HW_REGION_BUNDLE_OFFSETS[0] as u64,
            shared().hw_data_bundle + NATIVE_HW_REGION_BUNDLE_OFFSETS[1] as u64,
        ];
        let mut hw = vec![0; HW_DATA_SIZE];

        // A device VA outside the fixed register aperture fails closed.
        let mut window = register_windows();
        window[0].device_va = 0x1000;
        assert_eq!(
            encode_hw_data(&window, &[], regions, &mut hw),
            Err(InitdataError::RegisterWindow)
        );

        // Two declarations for the same slot fail closed.
        assert_eq!(
            encode_hw_data(&register_windows(), &[(0usize, 2u32)], regions, &mut hw),
            Err(InitdataError::RegisterWindow)
        );
    }

    #[test]
    fn region_c_size_and_required_word() {
        assert_eq!(REGION_C_SIZE, 0x1000);
        let mut region_c = vec![0xa5; REGION_C_SIZE];
        encode_region_c(&mut region_c).unwrap();
        assert_eq!(region_c.len(), 0x1000);
        // +0xe50 must hold 0x0000_0100 (bytes 00 01 00 00) or the primary takes
        // an asynchronous SError on device-control opcode 0x20.
        assert_eq!(REGION_C_REQUIRED_VALUE, 0x0000_0100);
        assert_eq!(get_u32(&region_c, 0xe50), REGION_C_REQUIRED_VALUE);
        assert_eq!(&region_c[0xe50..0xe54], &[0x00, 0x01, 0x00, 0x00]);
        // A recorded run near the front landed; the tail past the last run is 0.
        assert_eq!(get_u32(&region_c, 0x24), 0x0000_0bb8);
        assert_eq!(get_u32(&region_c, 0x7c), 1);
        assert_eq!(get_u32(&region_c, 0x80), 1);
        assert_eq!(get_u32(&region_c, 0x9c), 0x3000_001d);
        assert_eq!(get_u32(&region_c, 0xa0), 0x3100_0002);
        assert_eq!(region_c[REGION_C_SIZE - 1], 0);
    }

    #[test]
    fn status_block_pre_init_shape() {
        assert_eq!(STATUS_BLOCK_SIZE, 0x80);
        let mut status = vec![0xa5; STATUS_BLOCK_SIZE];
        encode_status_block(&mut status).unwrap();
        assert_eq!(get_u32(&status, 0x00), 0);
        assert_eq!(get_u32(&status, 0x04), 1);
        // +0x10 / +0x14 are post-ack firmware state and must be zero at handoff.
        assert_eq!(get_u32(&status, 0x10), 0);
        assert_eq!(get_u32(&status, 0x14), 0);
    }

    #[test]
    fn full_status_objects_and_bundle_views_are_carried() {
        let mut primary_status_b = vec![0xa5; PRIMARY_STATUS_B_OBJECT_SIZE];
        let fwctl = KERN + 0x5000_0000;
        encode_primary_status_b(fwctl, fwctl + CONTROL_RECORD_SIZE as u64, &mut primary_status_b)
            .unwrap();
        assert_eq!(get_u32(&primary_status_b, 0x04), 1);
        assert_eq!(get_u64(&primary_status_b, PRIMARY_STATUS_B_FWCTL_STATE), fwctl);
        assert_eq!(
            get_u64(&primary_status_b, PRIMARY_STATUS_B_FWCTL_RING),
            fwctl + CONTROL_RECORD_SIZE as u64
        );
        assert_eq!(get_u32(&primary_status_b, PRIMARY_STATUS_B_CONFIG_HEADER), 1);
        assert_eq!(primary_status_b[PRIMARY_STATUS_B_CONFIG_OFFSET + 0x10], 0x0f);

        let mut secondary_extra = [0xa5; STATUS_BLOCK_SIZE];
        encode_secondary_root_extra_1(&mut secondary_extra).unwrap();
        assert_eq!(secondary_extra[0x14], 1);
        assert_eq!(&secondary_extra[0x50..0x52], &[0x6a, 0x18]);

        let mut bundle = vec![0; NATIVE_SHARED_CLUSTER_SIZE];
        let state = KERN + 0x6000_0000;
        encode_bundle_static(state, &mut bundle).unwrap();
        assert_eq!(get_u64(&bundle, 0x8ed8), state);
        assert_eq!(bundle[0x80ac], 0x06);
        assert_eq!(bundle[BUNDLE_VIEW_OFFSETS[2] + 0xe45], 0xdc);
        assert_eq!(
            &bundle[BUNDLE_VIEW_OFFSETS[2] + 0x2ad7..BUNDLE_VIEW_OFFSETS[2] + 0x2adb],
            &[0xff, 0x7f, 0xff, 0x7f]
        );
    }

    #[test]
    fn instance_separation_invariants() {
        assert!(validate_pair(&primary(), &secondary()).is_ok());
        // The two roots are exactly 0x8000 apart.
        assert_eq!(SECONDARY_ROOT_DELTA, 0x8000);
        let mut bad = secondary();
        bad.root += 0x4000;
        assert_eq!(
            validate_pair(&primary(), &bad),
            Err(InitdataError::PairRootSeparation)
        );
        // The two instances' shared-region halves are 0x40000 apart.
        assert_eq!(SECONDARY_SHARED_REGION_DELTA, 0x40000);
        assert!(validate_shared_region_bases(KERN, KERN + 0x40000).is_ok());
        assert_eq!(
            validate_shared_region_bases(KERN, KERN + 0x8000),
            Err(InitdataError::SharedRegionSeparation)
        );
    }

    #[test]
    fn root_gate_rejects_the_m5_version_word() {
        let mut root = encode_root_bytes(InstanceRole::Primary);
        assert!(check_firmware_root_gates(&root).is_ok());
        // The M5/G17S word is answered with "Host and FW Interface is mismatched".
        put_u64(&mut root, 0x00, 0x0c8d_e339_0728_04c0);
        assert_eq!(
            check_firmware_root_gates(&root),
            Err(InitdataError::InterfaceMismatch)
        );
        put_u64(&mut root, 0x00, INITDATA_VERSION_WORD);
        put_u32(&mut root, 0x2c, 0);
        assert_eq!(
            check_firmware_root_gates(&root),
            Err(InitdataError::HostMappedAllocationsDisabled)
        );
    }

    #[test]
    fn evidence_flags_are_honest() {
        // The graph the firmware accepts and boots with is grounded.
        assert!(GROUNDED_LAYOUT_AVAILABLE);
        assert!(PARSE_ACCEPTED_GRAPH_AVAILABLE);
        // The measured performance values are carried, while ADT derivation,
        // The target-scoped bundle and status-B bytes are carried too.
        assert!(!ALL_HW_GLOBALS_VALUES_GROUNDED);
        assert!(PERF_TABLE_VALUES_ENCODED);
        assert!(BUNDLE_VIEW_CONTENT_CARRIED);
        assert!(PRIMARY_STATUS_B_FULL_OBJECT_CARRIED);
    }

    #[test]
    fn recorded_run_tables_match_the_serializer_totals() {
        // These byte totals match the hardware-validated g17p_initdata.py.
        let hw: usize = HW_DATA_OPAQUE_RUNS.iter().map(|(_, data)| data.len()).sum();
        assert_eq!(hw, 269);
        let region_c: usize = REGION_C_RUNS.iter().map(|(_, data)| data.len()).sum();
        assert_eq!(region_c, 180);
        // Every run fits its object.
        assert!(HW_DATA_OPAQUE_RUNS
            .iter()
            .all(|(off, data)| off + data.len() <= HW_DATA_SIZE));
        assert!(REGION_C_RUNS
            .iter()
            .all(|(off, data)| off + data.len() <= REGION_C_SIZE));
        // No opaque run touches firmware-state +0x2630.
        assert!(HW_DATA_OPAQUE_RUNS.iter().all(|(off, data)| {
            !(*off <= HW_FIRMWARE_STATE_OFFSET && HW_FIRMWARE_STATE_OFFSET < *off + data.len())
        }));
    }

    /// Pins the remaining documented ABI/geometry constants to their grounded
    /// values. They describe parts of the graph this offline module records but
    /// does not yet encode (the control-record framing and the full primary
    /// status-B object); asserting them keeps the
    /// recorded facts under test even before the encoders reach them.
    #[test]
    fn documented_constants_have_their_grounded_values() {
        assert_eq!(ROOT_ZERO_10, 0x10);
        assert_eq!(REGION_A_SIZE, 0x4000);
        assert_eq!(HW_DATA_BUNDLE_SIZE, 0xc000);
        assert_eq!(REGION_C_REQUIRED_OFFSET, 0xe50);
        assert_eq!(STATUS_BLOCK_POST_ACK_OFFSETS, [0x10, 0x14]);
        assert_eq!(PRIMARY_STATUS_B_OBJECT_SIZE, 0xeb00);
        // Device-control record framing (records are 0x40 bytes; the opening
        // producer is 1).
        assert_eq!(CONTROL_RECORD_SIZE, 0x40);
        assert_eq!(CONTROL_OPENING_PRODUCER, 1);
        // Channel state-block stride within the compact grid.
        assert_eq!(CHANNEL_STATE_STRIDE, 0x40);
        // Performance-table geometry and measured values.
        assert_eq!(HW_PERF_LADDER_ENTRIES, 11);
        assert_eq!(HW_PERF_GROUP_REPEATS, [16, 1]);
        assert_eq!(HW_PERF_VOLTAGE_DELTA, 0x40);
        assert_eq!(HW_PERF_MEMORY_VOLTAGE_DELTA, 0x440);
        assert_eq!(HW_PERF_STATE_BLOCK_STRIDE, 0x40);
        assert_eq!(HW_PERF_FREQ_LADDER_B, 0x1808);
        assert_eq!(HW_PERF_SCALE_LADDER_B, 0x1848);
        assert_eq!(HW_PERF_RELATIVE_LADDER_A, 0x18c8);
        assert_eq!(HW_PERF_RELATIVE_LADDER_B, 0x1908);
        assert_eq!(HW_PERF_INDEX_MAP_A, 0x19c8);
        assert_eq!(HW_PERF_INDEX_MAP_B, 0x1a08);
    }
}
