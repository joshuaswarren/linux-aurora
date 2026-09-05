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
/// Native QoS manager block published through hardware data.
pub(crate) const G17P_QOS_RESOURCE_OFFSET: usize = 0x27a4;
/// The QoS table pointer as the LIVE 25G83 build reads it.
///
/// KEXT_PROVEN (25G83 decompile): the live initdata object is 0x2708 bytes and
/// no instruction in that build references 0x27a4 or 0x27ac at all -- both of
/// our writes land past its end, in space the firmware never reads. The real
/// field is +0x26ac, written from `AGXQOSManager + 0x28`. The neighbouring
/// live fields are +0x2614 (the 4 x 0x20 KSM completion table, which we do
/// populate), +0x2694 (a 0x40-entry table fwva), +0x2604 (deferred-kick-mode
/// selector) and the flags at +0x2600/+0x2608/+0x2610.
///
/// We keep writing 0x27a4 as well: if that offset really is dead space the
/// extra store is inert, and leaving it in place keeps this a single-variable
/// change.
pub(crate) const G17P_QOS_RESOURCE_LIVE_OFFSET: usize = 0x26ac;
pub(crate) const G17P_QOS_RESOURCE_SIZE: usize = 0xe40;
/// Native global SKSM/QID configuration block in hardware data.
pub(crate) const G17P_SKSM_QID_CONFIG_2700_OFFSET: usize = 0x2700;
pub(crate) const G17P_SKSM_QID_CONFIG_2708_OFFSET: usize = 0x2708;
pub(crate) const G17P_SKSM_QID_RESOURCE_OFFSET: usize = 0x27ac;
/// Host->firmware transport selector, `initdata[0x26f8] = accel[0x4f8] & 1`
/// (`AGXFirmware::initFirmwareData` @0xfffffe0008a20940). 1 = the host
/// publishes channel commands; 0 = the host drives the MMIO kick bridge.
const HW_DATA_TRANSPORT_SELECTOR_OFFSET: usize = 0x26f8;
/// MEASURED, live 25G83 hardware-data block: native reads 0 here.
/// initFirmwareData clears the u32 at +0x26fe, which covers +0x2700..+0x2701,
/// and never sets it. Our 1 was a non-native write inside the live object --
/// the last non-perf difference in the whole 0x2708-byte structure.
pub(crate) const G17P_SKSM_QID_CONFIG_2700: u64 = 0;
pub(crate) const G17P_SKSM_QID_CONFIG_2708: u32 = 0x100;
/// Firmware log enable word inside main config.
pub(crate) const MAIN_FIRMWARE_LOG_ENABLE: usize = 0x250;

pub(crate) const G17P_SKSM_QID_RESOURCE_SIZE: usize = 0x2000;
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
/// Firmware-high base of the complete hardware-data/main-config/work-ring
/// cluster in every output-positive G17P capture.
pub(crate) const NATIVE_SHARED_CLUSTER_GPU_VA: u64 = 0xffff_fc20_c078_8000;

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
/// The argument that means "disable idle power off, and power the cores on
/// now". Any non-zero value only stores the enable word and returns.
pub(crate) const DEVICE_CONTROL_IDLE_POWER_OFF_ARG_POWER_ON: u32 = 0;
/// Non-zero inhibits the firmware's idle power-down. The scheduler idle-off
/// routine `0x7ca0` reads `[0xfffffc0000177838]` as one of a chain of guards
/// (b1 `0x8634`), each of which branches to `0x871c` = `mov w0,#1; ret` when
/// set — an early return that skips the bit-1 acquire at `0x8228`, i.e. skips
/// closing the GPU core power gate. `arg = 0` powers the cores on but leaves
/// this permissive, so the cores are gated off again shortly after; `arg != 0`
/// sets the inhibit but the handler's `cbnz w8, 0x5238` then skips the
/// power-on. Hence the pair: power on, then hold.
pub(crate) const DEVICE_CONTROL_IDLE_POWER_OFF_ARG_INHIBIT: u32 = 1;
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

pub(crate) fn encode_compute_dispatch_record(
    out: &mut [u8],
) -> Result<(), InitdataError> {
    if out.len() != COMPUTE_DISPATCH_RECORD_SIZE {
        return Err(InitdataError::BufferSize);
    }
    out.fill(0);
    for (index, value) in [
        0xe000_0000u32,
        0x0800_0000,
        0x0000_0000,
        0x0000_2a00,
        0x0000_1500,
    ]
    .into_iter()
    .enumerate()
    {
        put_u32(out, index * 4, value);
    }
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
pub(crate) enum InstanceRole {
    /// `gfx-asc`: scheduler, device control, and all work channels.
    Primary = 0,
    /// `gfx1-asc`: control-only peer. Linux starts it after primary RTKit is ready.
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

/// The primary's global region views at main `+0x2d0`: a high-only blank
/// sentinel, then the high halves of two low/high alias pairs (the low halves
/// are the fixed context-0 addresses in
/// [`REGION_VIEW_LOW_ADDRS`]). Each pair must map one physical page from both
/// address spaces — an invariant of the UAT mapping layer that cannot be
/// checked from the addresses alone.
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
    // Firmware log enable. The emitter at A000 0xfffffc0000021fa0 reads this
    // word and returns immediately when it is zero; every buffer the facility
    // needs is already allocated and pointed at by the addresses below. The
    // records are plain ASCII. Shipping macOS provisions the whole facility
    // and then throws the messages away (drainFirmwareLogRing is `bti c; ret`),
    // which is why the captured value is zero.
    if *crate::module_parameters::g17p_fw_log.value() != 0 {
        put_u32(out, MAIN_FIRMWARE_LOG_ENABLE, 1);
    }
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
            // High-only sentinel followed by two low/high region-view pairs.
            put_u64(out, MAIN_REGION_VIEWS, views.sentinel_high);
            put_u64(out, MAIN_REGION_VIEWS + 0x08, REGION_VIEW_LOW_ADDRS[0]);
            put_u64(out, MAIN_REGION_VIEWS + 0x10, views.alias_high[0]);
            put_u64(out, MAIN_REGION_VIEWS + 0x18, REGION_VIEW_LOW_ADDRS[1]);
            put_u64(out, MAIN_REGION_VIEWS + 0x20, views.alias_high[1]);
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
/// `completion_aliases` are the low/high GPU mappings of the two retained
/// completion-queue backings named by descriptor ordinals 0 and 2.
pub(crate) fn encode_hw_data(
    windows: &[RegisterWindow],
    flag_only_slots: &[(usize, u32)],
    completion_aliases: [G17PKsmCompletionAliases; 2],
    qos_resource_high: u64,
    sksm_qid_resource_high: u64,
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
    for aliases in completion_aliases {
        required_address(aliases.low)?;
        required_address(aliases.high)?;
    }
    required_address(qos_resource_high)?;
    required_address(sksm_qid_resource_high)?;

    out.fill(0);
    // Native's `AGXFirmware::initFirmwareData` opens this object with five
    // qwords that Linux has always left zero: the range-3 base and size, the
    // architectural GART window base twice (Asahi's HwDataB calls the pair
    // "USC start"), and the reserved page at the top of range 5. They are the
    // only place the host tells the firmware side about 0x10_0000_0000, which
    // is the base hardware folds into every compact TA/VDM pointer, so a zero
    // here leaves USC-class units resolving bare offsets.
    for (offset, value) in HW_FIRMWARE_RANGE_HEAD {
        put_u64(out, *offset, *value);
    }
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
        // NB +0x2600 is also DAT_178648, a firmware GATE: FUN_e004's per-DM
        // KSM completion-block arm runs only when (a == 0 || DAT_178648 == 0).
        // Writing 0 here to let that arm run was TESTED and the driver failed
        // probe with -22. I first wrote that our own initdata validation
        // rejected it; that was an invention -- nothing in the runtime path
        // validates this field, and the cause of the -22 was never
        // established. What settles it instead is the live 25G83 capture:
        // native's hardware-data block reads 1 at +0x2600 and 1 at +0x2608,
        // the same as ours, so this is not a divergence.
        put_u32(out, offset, 1);
    }

    put_u32(
        out,
        G17P_KSM_COMPLETION_TABLE_PREFIX_OFFSET,
        G17P_KSM_COMPLETION_TABLE_PREFIX,
    );
    for (aliases, ordinal) in completion_aliases
        .iter()
        .zip(G17P_KSM_COMPLETION_PRESENT_ORDINALS)
    {
        let base = G17P_KSM_COMPLETION_TABLE_OFFSET
            + ordinal as usize * G17P_KSM_COMPLETION_DESCRIPTOR_STRIDE;
        put_u64(out, base, aliases.low);
        put_u64(out, base + 0x08, aliases.high);
        put_u32(out, base + 0x10, G17P_KSM_COMPLETION_CAPACITY);
        put_u32(out, base + 0x14, G17P_KSM_COMPLETION_ENTRY_SIZE);
        put_u32(out, base + 0x18, ordinal);
        // +0x1c is the firmware-owned processed cursor and starts at zero.
    }

    put_u64(out, G17P_QOS_RESOURCE_LIVE_OFFSET, qos_resource_high);
    put_u64(
        out,
        G17P_SKSM_QID_CONFIG_2700_OFFSET,
        G17P_SKSM_QID_CONFIG_2700,
    );
    // The valid live G17P HWData object leaves +0x2708, +0x27a4 and +0x27ac
    // zero. The kext writes those only in a staging/version tail and B1 has no
    // consumer; the live QoS pointer is the independent +0x26ac field above.

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
    // regionC+0x9cc is the KSM progress-watchdog threshold for data master 2
    // (compute); the firmware copies it to 0x1778d0 and its handler starts
    // `cbz w20 -> ret`, so 0 disables the watchdog. We ship 0x32 (50) here and
    // leave the other data master's slot at 0 -- i.e. we arm the watchdog on
    // exactly the queue we submit to. When work cannot progress for any other
    // reason this turns a stall into a self-sustaining recovery loop.
    if *crate::module_parameters::g17p_progress_watchdog.value() == 0 {
        put_u32(out, 0x9cc, 0);
    }
    // Native initializes the idle-descent selector to zero. Keep the old
    // alternate predicate only as an explicit diagnostic override.
    if *crate::module_parameters::g17p_idle_selector.value() != 0 {
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
/// not; derived from the hardware-validated `g17p_initdata.py`
/// `HWDATA_CONSTANTS`). 29 sparse runs, 262 bytes total. `+0x2630` is
/// deliberately absent: it is firmware state, not host configuration.
///
/// Native fixture pointer bytes are not platform constants. The original
/// +0x26ad run included seven bytes of the captured QoS allocation address;
/// replaying it after encode_hw_data's live pointer store silently replaced
/// the owned allocation with that unrelated address. Keep only its adjacent
/// state words here and encode the complete pointer from its retained owner.
///
/// +0x0f08 is zero in native too. That field is the firmware's ISR mask
/// selector -- FUN_1e858 copies it to DAT_1785f4, and FUN_215cc picks the
/// interrupt-enable word from it as
///     DAT_1785f4 == 2 ? 0x0e00620038 : 0x0a00620038
/// storing it to DAT_179328, which the ISR ANDs against MMIO[0x10a10]. Writing
/// 2 here was tested and does exactly that (0x10a08 measured 0x0e00620038,
/// bit 34 enabled) and changes nothing else, so leaving it unset is right and
/// this part is not the variant-2 configuration.
///
/// A caution for anyone diffing this table again: it has TWENTY-NINE runs,
/// several spanning many lines. A naive `\(0x..,\s*&\[(.*?)\]\)` regex
/// silently matches only 19 of them and mis-associates offsets, which
/// manufactures differences that are not there and, if used to rewrite the
/// table, corrupts it into 17 runs / 292 bytes and makes probe fail with -22.
/// Parse by matching brackets, and check the 29/262 totals before trusting a
/// comparison.
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
    // NOTE, do not "fix" this by adding 0x0f08 without evidence. That u32 is
    // the firmware's ISR mask selector: FUN_1e858 copies hw-data +0xf08 to
    // DAT_1785f4 (store at b1 0x1e9e8), and FUN_215cc picks the interrupt
    // enable word from it with
    //     csel: DAT_1785f4 == 2 ? 0x0e00620038 : 0x0a00620038
    // storing the result to DAT_179328, which the ISR at 0x216f0 ANDs against
    // MMIO[0x10a10] to decide which interrupts to service. The field is absent
    // here, so it reads zero and 0x10a08 measures 0x0a00620038 -- the variant
    // without bit 34.
    //
    // TESTED 2026-09-02: writing 2 here does exactly what the disassembly
    // says -- 0x10a08 measured 0x0e00620038, bit 34 enabled -- and changes
    // nothing else. The render still stalls with bit 4 clear, no KTrace 2,
    // fragment status 4. Reverted because there is no oracle saying this part
    // is variant 2 and it alters firmware interrupt behaviour on a guess.
    // Both variants already enable bits 3, 4 and 5, so neither withholds
    // new-work.
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
    // +0x26ac..+0x26b3 is the live QoS pointer, never captured allocation bytes.
    (0x26b4, &[0x01, 0x00, 0x00, 0x00, 0x01]),
    (0x26e8, &[0x01]),
    (0x3d98, &[0xff, 0xff, 0xff, 0xff]),
    (0x3db0, &[0xff, 0xff, 0xff, 0xff]),
];

/// Recorded T8140 region-C platform tuning constants (values known, meanings
/// not; generated from the hardware-validated `g17p_initdata.py`
/// `REGION_C_CONSTANTS`). 12 sparse runs, 180 bytes total. The final run
/// initializes the byte preceding the host power-assert counter and the
/// counter itself to their native cold value of zero.
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
            0x02, 0x00, 0x00, 0x00, 0x28, 0x00, 0x00, 0x00, 0x05, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x28, 0x00, 0x00, 0x00, 0x32, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00,
        ],
    ),
    (0x0e1c, &[0x00, 0x01, 0x00, 0x00]),
    (0x0e38, &[0x00, 0x03, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00]),
    (0x0e50, &[0x00, 0x00, 0x00, 0x00]),
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
    (0x12c, &[0x16, 0x26]),
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


    fn native_compute_bytes(bytes: &[u8]) -> Vec<u8> {
        let mut native = bytes.to_vec();
        for offset in 0..bytes.len().saturating_sub(7) {
            let value = get_u64(bytes, offset);
            let address = value & !CONTROL_OPERAND_ENTRY_FLAG;
            if (0x70_0200_0000..0x70_03ef_8000).contains(&address) {
                put_u64(&mut native, offset, value - COMPUTE_FLIST_RELOCATION);
            }
        }
        native
    }
    #[test]
    fn compute_aperture_ranges_do_not_overlap_each_other_or_render() {
        let mut ranges = vec![
            (COMPUTE_FLIST_PAGE_LIST_ADDRESS, 0x20_0000),
            (COMPUTE_FLIST_RUN_TABLE_ADDRESS, 0x1_0000),
            (COMPUTE_RUNTIME_PAGE_LIST_ADDRESS, 0xc000),
            (COMPUTE_RUNTIME_CLASS1_TABLE_ADDRESS, 0x4000),
        ];
        for i in 0..CONTROL_OPERAND_ENTRY_COUNT {
            ranges.push((COMPUTE_FLIST_BUFFER_BASE + i as u64 * CONTROL_OPERAND_BUFFER_STRIDE,
                         CONTROL_OPERAND_BUFFER_SIZE));
        }
        for base in G17P_KSM_COMPLETION_LOW_VAS {
            ranges.push((base, G17P_KSM_COMPLETION_BACKING_SIZE as u64));
        }
        // Native mature kernel-low ZERO0/ZERO1 intentionally alias that
        // mode's main backing, whereas ordinary client mappings retain its
        // separate page-list. They are not additional overlapping mappings.
        assert_eq!(COMPUTE_RUNTIME_ZERO_BUFFER_0_ADDRESS, COMPUTE_FLIST_PAGE_LIST_ADDRESS);
        assert_eq!(COMPUTE_RUNTIME_ZERO_BUFFER_1_ADDRESS, COMPUTE_FLIST_PAGE_LIST_ADDRESS + 0x10_8000);
        for (i, &(base, size)) in ranges.iter().enumerate() {
            assert_eq!(base & 0x3fff, 0);
            assert!(base >= 0x70_0200_0000 && base + size <= 0x70_0400_0000);
            for &(other, other_size) in ranges.iter().skip(i + 1) {
                assert!(base + size <= other || other + other_size <= base,
                        "overlap: {base:#x}+{size:#x} with {other:#x}+{other_size:#x}");
            }
        }
        assert_eq!(CONTROL_OPERAND_TABLE_ADDRESS, 0x70_0020_8000);
        assert_eq!(CONTROL_OPERAND_BUFFER_BASE, 0x70_0022_0000);
    }

    #[test]
    fn compute_support_uses_full_unaligned_page_pointer() {
        for encode in [encode_compute_readiness_class1_support,
                       encode_compute_readiness_class3_support,
                       encode_compute_runtime_class2_support,
                       encode_compute_partial_opening_control_shared] {
            let mut bytes = vec![0xa5; 0x4000];
            encode(&mut bytes).unwrap();
            assert_eq!(get_u64(&bytes, 0x14), 0x70_0200_0000);
            assert_eq!(get_u32(&bytes, 0x1c), 0x4_0000);
            assert_eq!(get_u64(&bytes, 0x30), 0x70_0220_8000);
        }
    }

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

        // Four inline descriptors begin at +0x2614; only ordinals 0/2 exist.
        assert_eq!(get_u32(&hw, G17P_KSM_COMPLETION_TABLE_PREFIX_OFFSET), 0x100);
        for ordinal in [1usize, 3] {
            let base = G17P_KSM_COMPLETION_TABLE_OFFSET
                + ordinal * G17P_KSM_COMPLETION_DESCRIPTOR_STRIDE;
            assert_eq!(&hw[base..base + 0x20], &[0; 0x20]);
        }
        for (aliases, ordinal) in aliases.iter().zip(G17P_KSM_COMPLETION_PRESENT_ORDINALS) {
            let base = G17P_KSM_COMPLETION_TABLE_OFFSET
                + ordinal as usize * G17P_KSM_COMPLETION_DESCRIPTOR_STRIDE;
            assert_eq!(get_u64(&hw, base), aliases.low);
            assert_eq!(get_u64(&hw, base + 0x08), aliases.high);
            assert_eq!(get_u32(&hw, base + 0x10), 0x800);
            assert_eq!(get_u32(&hw, base + 0x14), 0x40);
            assert_eq!(get_u32(&hw, base + 0x18), ordinal);
            assert_eq!(get_u32(&hw, base + 0x1c), 0);
        }
        assert_eq!(get_u64(&hw, G17P_SKSM_QID_CONFIG_2700_OFFSET), 0);
        assert_eq!(get_u64(&hw, G17P_QOS_RESOURCE_LIVE_OFFSET), qos_resource);
        assert_eq!(get_u32(&hw, G17P_SKSM_QID_CONFIG_2708_OFFSET), 0);
        assert_eq!(get_u64(&hw, G17P_QOS_RESOURCE_OFFSET), 0);
        assert_eq!(get_u64(&hw, G17P_SKSM_QID_RESOURCE_OFFSET), 0);

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
        let aliases = completion_aliases();
        let mut hw = vec![0; HW_DATA_SIZE];

        // A device VA outside the fixed register aperture fails closed.
        let mut window = register_windows();
        window[0].device_va = 0x1000;
        assert_eq!(
            encode_hw_data(
                &window,
                &[],
                aliases,
                qos_resource_high(),
                sksm_qid_resource_high(),
                &mut hw,
            ),
            Err(InitdataError::RegisterWindow)
        );

        // Two declarations for the same slot fail closed.
        assert_eq!(
            encode_hw_data(
                &register_windows(),
                &[(0usize, 2u32)],
                aliases,
                qos_resource_high(),
                sksm_qid_resource_high(),
                &mut hw,
            ),
            Err(InitdataError::RegisterWindow)
        );
    }

    #[test]
    fn region_c_size_and_required_word() {
        assert_eq!(REGION_C_SIZE, 0x1000);
        let mut region_c = vec![0xa5; REGION_C_SIZE];
        encode_region_c(&mut region_c).unwrap();
        assert_eq!(region_c.len(), 0x1000);
        assert_eq!(REGION_C_REQUIRED_VALUE, 0);
        assert_eq!(get_u32(&region_c, 0xe50), REGION_C_REQUIRED_VALUE);
        assert_eq!(&region_c[0xe50..0xe55], &[0; 5]);
        // Those same four bytes, framed the way b1 actually reads them: the
        // byte at +0xe50 is zero and the GPU-core power-assert counter is the
        // UNALIGNED u32 at +0xe51. Nothing in the image reads +0xe50 or +0xe55.
        assert_eq!(region_c[REGION_C_REQUIRED_OFFSET], 0);
        assert_eq!(
            get_u32(&region_c, REGION_C_GPU_CORE_PWR_ASSERT_COUNTER_OFFSET),
            REGION_C_GPU_CORE_PWR_ASSERT_COUNT
        );
        assert_eq!(REGION_C_GPU_CORE_PWR_ASSERT_COUNTER_OFFSET, 0xe51);
        assert_eq!(get_u32(&region_c, 0x9b8), 2);
        assert_eq!(get_u32(&region_c, 0x9bc), 40);
        assert_eq!(region_c[0xe21], 0);
        // A recorded run near the front landed; the tail past the last run is 0.
        assert_eq!(get_u32(&region_c, 0x24), 0x0000_0bb8);
        assert_eq!(get_u32(&region_c, 0x7c), 1);
        assert_eq!(get_u32(&region_c, 0x80), 1);
        assert_eq!(get_u32(&region_c, 0x9c), 0x3000_001d);
        assert_eq!(get_u32(&region_c, 0xa0), 0x3100_0002);
        assert_eq!(region_c[REGION_C_SIZE - 1], 0);
    }

    #[test]
    fn device_control_power_assert_flags_match_b1() {
        const CLEAR: [u32; 15] = [
            0x00, 0x0e, 0x15, 0x16, 0x1b, 0x22, 0x2a, 0x2e, 0x3a, 0x3b, 0x3c, 0x3d,
            0x3e, 0x3f, 0x40,
        ];
        for opcode in 0..=0x3f {
            assert_eq!(
                device_control_asserts_gpu_power(opcode),
                Some(!CLEAR[..CLEAR.len() - 1].contains(&opcode)),
                "opcode {opcode:#x}",
            );
        }
        assert_eq!(device_control_asserts_gpu_power(CLEAR[CLEAR.len() - 1]), None);

        // Native host-tally lifecycle for two updateSubmitInfo records, one
        // HardwareBufferID release, and one IdlePowerOff record.
        let mut tally = REGION_C_GPU_CORE_PWR_ASSERT_COUNT;
        for (opcode, expected) in [(0x20, 1), (0x20, 2), (0x2e, 2), (0x0a, 3)] {
            if device_control_asserts_gpu_power(opcode).unwrap() {
                tally = tally.wrapping_add(1);
            }
            assert_eq!(tally, expected);
        }
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
        // HWData excludes the seven captured QoS pointer bytes; the adjacent
        // state words and the remaining platform runs are preserved.
        let hw: usize = HW_DATA_OPAQUE_RUNS.iter().map(|(_, data)| data.len()).sum();
        assert_eq!(HW_DATA_OPAQUE_RUNS.len(), 29);
        assert_eq!(hw, 262);
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
        // Device-control framing and the ownerless-UMA-free opening prefixes.
        assert_eq!(CONTROL_RECORD_SIZE, 0x40);
        assert_eq!(CONTROL_OPENING_PRIMARY_PRODUCER, 1);
        assert_eq!(COMPUTE_RUNTIME_CONTROL_RECORDS, 65);
        assert_eq!(COMPUTE_RUNTIME_CONTROL_FINAL_COUNTER, 66);
        assert_eq!(CONTROL_OPENING_SECONDARY_PRODUCER, 1);
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
