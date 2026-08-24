// SPDX-License-Identifier: GPL-2.0-only OR MIT

//! GPU identity decoding shared by register discovery and host-side tests.
//!
//! Apple's hardware family byte is not the GPU generation. In particular,
//! family 0xa contains both G16 and G17 parts, so all identity decisions must
//! include the hardware variant byte (and, for the older C/D split, die count).

/// GPU generation enumeration. Note: Part of the UABI.
#[derive(Debug, PartialEq, Eq, Copy, Clone)]
#[repr(u32)]
pub(crate) enum GpuGen {
    G13 = 13,
    G14 = 14,
    G15 = 15,
    G16 = 16,
    G17 = 17,
}

/// GPU variant enumeration. Note: Part of the UABI.
#[derive(Debug, PartialEq, Eq, Copy, Clone)]
#[repr(u32)]
pub(crate) enum GpuVariant {
    P = 'P' as u32,
    G = 'G' as u32,
    S = 'S' as u32,
    C = 'C' as u32,
    D = 'D' as u32,
    A = 'A' as u32,
}

/// Apple's GPU HAL generation.
///
/// G13 through G15 predate the numbered HAL namespace. This is separate from
/// both the design generation and the USC generation: G17P/G17A use HAL200,
/// while the other G17 parts use HAL300.
#[derive(Debug, PartialEq, Eq, Copy, Clone)]
#[repr(u32)]
pub(crate) enum GpuHalGeneration {
    Legacy = 0,
    Hal200 = 200,
    Hal300 = 300,
}

/// Firmware-role topology presented by Apple's RTBuddy driver.
#[derive(Debug, PartialEq, Eq, Copy, Clone)]
pub(crate) enum FirmwareRoleTopology {
    /// One GFX firmware role.
    Single,
    /// Separate GFX and GFX1 firmware roles.
    Dual,
}

/// Host-to-firmware submission transport present in Apple's driver.
#[derive(Debug, PartialEq, Eq, Copy, Clone)]
pub(crate) enum SubmissionTransport {
    /// Classic device-control ring and SKU channels only.
    Classic,
    /// Both classic and 128-queue SKSM stacks are linked.
    ClassicAndSksm,
    /// 128-queue SKSM transport only.
    Sksm,
}

/// Exact host-reported bit layout used to insert a queue ID in an SKSM kick.
#[allow(dead_code)]
#[derive(Debug, PartialEq, Eq, Copy, Clone)]
pub(crate) struct SksmKickQueueIdConfig {
    pub(crate) shift: u8,
    pub(crate) mask: u8,
}

/// Firmware role selected by Apple's host kick sender.
#[allow(dead_code)]
#[derive(Debug, PartialEq, Eq, Copy, Clone)]
pub(crate) enum SubmissionFirmwareRole {
    Gfx,
    Gfx1,
}

/// Exact role and wire word produced for one host firmware kick.
#[allow(dead_code)]
#[derive(Debug, PartialEq, Eq, Copy, Clone)]
pub(crate) struct FirmwareKickRoute {
    pub(crate) role: SubmissionFirmwareRole,
    pub(crate) message: u64,
}

/// Exact RTBuddy endpoint binding used by Apple's host firmware-kick sender.
#[allow(dead_code)]
#[derive(Debug, PartialEq, Eq, Copy, Clone)]
pub(crate) struct FirmwareKickEndpointConfig {
    pub(crate) sender_vtable_offset: u16,
    pub(crate) gated_sender_vtable_offset: u16,
    pub(crate) endpoint_service_id_offset: u16,
    pub(crate) message_endpoint_id: u8,
    pub(crate) message_endpoint_object_offset: u16,
    pub(crate) async_note_endpoint_id: u8,
    pub(crate) async_note_endpoint_object_offset: u16,
    pub(crate) endpoint_send_vtable_offset: u16,
    pub(crate) sender_arg2: u8,
    pub(crate) endpoint_send_arg2: u8,
    pub(crate) endpoint_send_arg3: u8,
}

/// Exact host-side ordering around one data-master ring publication.
#[allow(dead_code)]
#[derive(Debug, PartialEq, Eq, Copy, Clone)]
pub(crate) enum DataMasterRingPublicationStep {
    /// Select the slot at the current write index after checking for full.
    SelectNextEntry,
    /// The opaque, target-specific command entry has been fully encoded.
    RecordEncoded,
    /// Execute the host's inner-shareable data memory barrier.
    DmbIsh,
    /// Read the current shared write index again.
    ReadWriteIndex,
    /// Increment the write index and truncate it to eight bits.
    IncrementAndWrap8,
    /// Store the new shared write index before notifying firmware.
    StoreWriteIndex,
}

/// Exact host-side storage used for the three data-master ring indices.
#[allow(dead_code)]
#[derive(Debug, PartialEq, Eq, Copy, Clone)]
pub(crate) enum DataMasterRingIndexBacking {
    /// One slice of an `AGXFirmware::allocateSharedData` batch.
    FirmwareSharedData {
        resource_owner_object_offset: u16,
        cpu_object_offset: u8,
        firmware_object_offset: u8,
        read_index_offset: u8,
        cfi_index_offset: u8,
        write_index_offset: u8,
        slice_size: u8,
        caching_options: u8,
        allocate_shared_data_arg: bool,
        batch_ring_count: u8,
        release_zeroes_owner_slot: bool,
    },
    /// Three cells allocated from an `AGXFenderScratchRAM` object.
    FenderScratchRam {
        scratch_object_offset: u8,
        read_cpu_pointer_offset: u8,
        cfi_cpu_pointer_offset: u8,
        write_cpu_pointer_offset: u8,
        read_firmware_pointer_offset: u8,
        cfi_firmware_pointer_offset: u8,
        write_firmware_pointer_offset: u8,
        entries_write_allocator_offset: u8,
        read_cfi_allocator_offset: u8,
        index_allocation_size: u8,
        entries_write_mapping_options: u32,
        read_cfi_mapping_options: u32,
        cleanup_frees_allocations_proven: bool,
    },
}

/// Exact construction path for the 256 data-master ring entries.
#[allow(dead_code)]
#[derive(Debug, PartialEq, Eq, Copy, Clone)]
pub(crate) enum DataMasterRingEntryBacking {
    /// One slice of an `AGXFirmware::allocateSharedData` batch.
    FirmwareSharedData {
        resource_owner_object_offset: u16,
        slice_size: u16,
        caching_options: u8,
        allocate_shared_data_arg: bool,
        batch_ring_count: u8,
        release_zeroes_owner_slot: bool,
    },
    /// One allocation from the writable `AGXFenderScratchRAM` pool.
    FenderScratchRam {
        allocator_offset: u8,
        allocation_size: u16,
        mapping_options: u32,
        cleanup_frees_allocation_proven: bool,
    },
}

/// Host mutation policy independently visible in the ring vtable methods.
#[allow(dead_code)]
#[derive(Debug, PartialEq, Eq, Copy, Clone)]
pub(crate) enum DataMasterRingHostIndexMutation {
    /// Read, CFI, and write setters all store directly into shared data.
    DirectSharedStores,
    /// Read/CFI setters trap; the write setter stores while holding the
    /// Fender ScratchRAM lock.
    LockedScratchWriteOnly,
}

/// Exact host-visible geometry and publication contract of a data-master ring.
#[allow(dead_code)]
#[derive(Debug, PartialEq, Eq, Copy, Clone)]
pub(crate) struct DataMasterRingConfig {
    pub(crate) index_backing: DataMasterRingIndexBacking,
    pub(crate) entry_backing: DataMasterRingEntryBacking,
    pub(crate) entries_cpu_object_offset: u8,
    pub(crate) entries_firmware_object_offset: u8,
    pub(crate) host_index_mutation: DataMasterRingHostIndexMutation,
    pub(crate) get_read_index_vtable_offset: u8,
    pub(crate) get_write_index_vtable_offset: u8,
    pub(crate) set_write_index_vtable_offset: u8,
    pub(crate) entry_stride: u8,
    pub(crate) index_count: u16,
    pub(crate) usable_entry_count: u16,
    pub(crate) cold_init_ring_count: u8,
    pub(crate) shared_data_cacheability_names_proven: bool,
    pub(crate) firmware_consumer_ownership_proven: bool,
    pub(crate) publication_steps: [DataMasterRingPublicationStep; 6],
}

/// Pure result of applying the host's full/wrap calculation to one ring state.
#[allow(dead_code)]
#[derive(Debug, PartialEq, Eq, Copy, Clone)]
pub(crate) struct DataMasterRingNextEntry {
    /// Byte offset from the backing pointer loaded at ring object +0x18.
    pub(crate) entry_offset_from_entries_base: u16,
    pub(crate) published_write_index: u8,
}

/// Data-master ordinal passed to Apple's command-entry encoder.
#[allow(dead_code)]
#[derive(Debug, PartialEq, Eq, Copy, Clone)]
pub(crate) enum DataMasterClass {
    Ta,
    ThreeD,
    Cl,
}

/// Host index handoff performed before publishing firmware ring pointers.
#[allow(dead_code)]
#[derive(Debug, PartialEq, Eq, Copy, Clone)]
pub(crate) enum DataMasterFirmwareIndexHandoff {
    /// For each priority, copy each ring's write index to read, then copy each
    /// ring's write index to CFI.
    MirrorReadAndCfiToWrite,
    /// Scratch-backed read/CFI setters trap, and `initFirmwareData` performs
    /// no host stores through them before publishing their firmware pointers.
    NoHostReadOrCfiStore,
}

/// Exact host-side publication layout for firmware data-master ring pointers.
#[allow(dead_code)]
#[derive(Debug, PartialEq, Eq, Copy, Clone)]
pub(crate) struct DataMasterFirmwareTableConfig {
    pub(crate) descriptor_table_cpu_pointer_object_offset: u16,
    pub(crate) ta_ring_object_base_offset: u16,
    pub(crate) three_d_ring_object_base_offset: u16,
    pub(crate) cl_ring_object_base_offset: u16,
    pub(crate) ring_object_stride: u8,
    pub(crate) priority_count: u8,
    pub(crate) data_master_count: u8,
    pub(crate) descriptor_record_stride: u8,
    pub(crate) read_index_firmware_pointer_offset: u8,
    pub(crate) cfi_index_firmware_pointer_offset: u8,
    pub(crate) write_index_firmware_pointer_offset: u8,
    pub(crate) entries_firmware_pointer_offset: u8,
    pub(crate) index_handoff: DataMasterFirmwareIndexHandoff,
    pub(crate) firmware_read_index_acknowledgment_writer_proven: bool,
    pub(crate) dual_stack_runtime_selector_proven: bool,
}

/// Pure class/priority selection within Apple's classic submit methods and
/// corresponding firmware descriptor table.
#[allow(dead_code)]
#[derive(Debug, PartialEq, Eq, Copy, Clone)]
pub(crate) struct DataMasterFirmwareRingRoute {
    pub(crate) encoder_data_master_ordinal: u8,
    pub(crate) ring_object_offset: u16,
    pub(crate) descriptor_record_offset: u16,
    pub(crate) read_index_firmware_pointer_offset: u16,
    pub(crate) cfi_index_firmware_pointer_offset: u16,
    pub(crate) write_index_firmware_pointer_offset: u16,
    pub(crate) entries_firmware_pointer_offset: u16,
}

/// Refusals made by the target-pinned SKSM submission preflight.
#[allow(dead_code)]
#[derive(Debug, PartialEq, Eq, Copy, Clone)]
pub(crate) enum SubmissionConfigError {
    /// No independent pinned artifact proves SKSM configuration for the target.
    UnsupportedTarget,
    /// The supplied configuration differs from the target's exact host values.
    UnexpectedSksmKickQueueIdConfig,
    /// The kick index does not fit the three-bit host field.
    KickIndexOutOfRange,
    /// The opaque firmware message type does not fit the six-bit host field.
    FirmwareMessageTypeOutOfRange,
    /// The supplied endpoint binding differs from the target's exact host values.
    UnexpectedFirmwareKickEndpointConfig,
    /// The supplied data-master ring contract differs from exact host values.
    UnexpectedDataMasterRingConfig,
    /// The supplied firmware ring-table contract differs from exact host values.
    UnexpectedDataMasterFirmwareTableConfig,
    /// The priority is outside the four published data-master ring groups.
    DataMasterPriorityOutOfRange,
    /// The shared read index is outside the host's eight-bit ring index space.
    DataMasterReadIndexOutOfRange,
    /// The shared write index is outside the host's eight-bit ring index space.
    DataMasterWriteIndexOutOfRange,
    /// Advancing the write index would collide with the shared read index.
    DataMasterRingFull,
}

/// A pure submission configuration validator is available for the exact
/// G16X/G17 host targets below.
#[allow(dead_code)]
pub(crate) const GROUNDED_SKSM_KICK_QUEUE_ID_CONFIG_AVAILABLE: bool = true;
/// A pure endpoint-binding validator is available for every target supported
/// by the firmware-kick word encoder.
#[allow(dead_code)]
pub(crate) const GROUNDED_FIRMWARE_KICK_ENDPOINT_CONFIG_AVAILABLE: bool = true;
/// A pure data-master ring geometry and publication validator is available for
/// every target supported by the firmware-kick word encoder.
#[allow(dead_code)]
pub(crate) const GROUNDED_DATA_MASTER_RING_CONFIG_AVAILABLE: bool = true;
/// A pure firmware ring-table publication and classic class/priority selector
/// is available for the same independently pinned G15-G17 host corpus.
#[allow(dead_code)]
pub(crate) const GROUNDED_DATA_MASTER_FIRMWARE_TABLE_CONFIG_AVAILABLE: bool = true;
/// The symbolic meaning of the firmware shared-data caching-option numbers,
/// firmware consumer ownership, and transport selection on dual-stack targets
/// are not proved, so command submission must remain disconnected from these
/// helpers.
#[allow(dead_code)]
pub(crate) const EXECUTABLE_G15_G17_SUBMISSION_AVAILABLE: bool = false;

const SKSM_KICK_QUEUE_ID_CONFIG: SksmKickQueueIdConfig = SksmKickQueueIdConfig {
    shift: 40,
    mask: 0x7f,
};
const FIRMWARE_KICK_TAG: u64 = 0x0080_0000_0000_0000;
const FIRMWARE_KICK_INDEX_MASK: u8 = 0x07;
const FIRMWARE_KICK_INDEX_SHIFT: u32 = 2;
const FIRMWARE_MESSAGE_TYPE_MASK: u8 = 0x3f;
const FIRMWARE_MESSAGE_TYPE_SHIFT: u32 = 48;
const G17_GFX1_FIRMWARE_MESSAGE_TYPE: u8 = 0x07;
const FIRMWARE_KICK_ENDPOINT_CONFIG: FirmwareKickEndpointConfig = FirmwareKickEndpointConfig {
    sender_vtable_offset: 0x8a8,
    gated_sender_vtable_offset: 0x8b0,
    endpoint_service_id_offset: 0x88,
    message_endpoint_id: 0x20,
    message_endpoint_object_offset: 0x128,
    async_note_endpoint_id: 0x21,
    async_note_endpoint_object_offset: 0x130,
    endpoint_send_vtable_offset: 0x1e8,
    sender_arg2: 0,
    endpoint_send_arg2: 0,
    endpoint_send_arg3: 1,
};
const DATA_MASTER_RING_PUBLICATION_STEPS: [DataMasterRingPublicationStep; 6] = [
    DataMasterRingPublicationStep::SelectNextEntry,
    DataMasterRingPublicationStep::RecordEncoded,
    DataMasterRingPublicationStep::DmbIsh,
    DataMasterRingPublicationStep::ReadWriteIndex,
    DataMasterRingPublicationStep::IncrementAndWrap8,
    DataMasterRingPublicationStep::StoreWriteIndex,
];

const fn make_data_master_firmware_table_config(
    descriptor_table_cpu_pointer_object_offset: u16,
    ta_ring_object_base_offset: u16,
    three_d_ring_object_base_offset: u16,
    cl_ring_object_base_offset: u16,
    ring_object_stride: u8,
    index_handoff: DataMasterFirmwareIndexHandoff,
) -> DataMasterFirmwareTableConfig {
    DataMasterFirmwareTableConfig {
        descriptor_table_cpu_pointer_object_offset,
        ta_ring_object_base_offset,
        three_d_ring_object_base_offset,
        cl_ring_object_base_offset,
        ring_object_stride,
        priority_count: 4,
        data_master_count: 3,
        descriptor_record_stride: 0x20,
        read_index_firmware_pointer_offset: 0x00,
        cfi_index_firmware_pointer_offset: 0x08,
        write_index_firmware_pointer_offset: 0x10,
        entries_firmware_pointer_offset: 0x18,
        index_handoff,
        firmware_read_index_acknowledgment_writer_proven: false,
        dual_stack_runtime_selector_proven: false,
    }
}

// Host corpus SHA-256:
// mac15g: 7e207cc2a4c669623cd7242b74e166c63ba9e4aa625307db6567fae342a897b3
// mac15s: d0a5c876ce14c8ee2ec893cbf280ef57d214add2a0da74aaa9066f86795eed39
// mac15j (G15C/D): 146ce4df57416c3f03115727f04c4448078100d8c87b1f3893ff0cf2ae2acb37
// mac16g: 8bc6795041ba43f9a3b95c4623f17605c0969980eec08bfb782d25a1557c5a7a
// mac16j (G16S/C): ba728ba6e68539fd21774788bbbe3e0035bf45c8e627944fa4f4b22648af3f20
// mac17p: f804073df6367dceb1902bbc42fecb85d88def1a7d8a9b75a1c7b7efd0231507
// mac17g: 41432db82f74cc38aa51dbdac6def58004d8649a01f8b863a1858e723a9d6822
// mac17j (G17S/C): ccbe120a3a197038216426b507b5409873397b947308239b4ecc75d76759e87f
const fn data_master_ring_shared_indices(
    resource_owner_object_offset: u16,
) -> DataMasterRingIndexBacking {
    DataMasterRingIndexBacking::FirmwareSharedData {
        resource_owner_object_offset,
        cpu_object_offset: 0x08,
        firmware_object_offset: 0x10,
        read_index_offset: 0x00,
        cfi_index_offset: 0x10,
        write_index_offset: 0x20,
        slice_size: 0x30,
        caching_options: 0,
        allocate_shared_data_arg: false,
        batch_ring_count: 12,
        release_zeroes_owner_slot: true,
    }
}

const fn data_master_ring_shared_entries(
    resource_owner_object_offset: u16,
) -> DataMasterRingEntryBacking {
    DataMasterRingEntryBacking::FirmwareSharedData {
        resource_owner_object_offset,
        slice_size: 0x1800,
        caching_options: 1,
        allocate_shared_data_arg: false,
        batch_ring_count: 12,
        release_zeroes_owner_slot: true,
    }
}

const DATA_MASTER_RING_SCRATCH_INDICES_CLEANUP: DataMasterRingIndexBacking =
    DataMasterRingIndexBacking::FenderScratchRam {
        scratch_object_offset: 0x28,
        read_cpu_pointer_offset: 0x30,
        cfi_cpu_pointer_offset: 0x38,
        write_cpu_pointer_offset: 0x40,
        read_firmware_pointer_offset: 0x48,
        cfi_firmware_pointer_offset: 0x50,
        write_firmware_pointer_offset: 0x58,
        entries_write_allocator_offset: 0x98,
        read_cfi_allocator_offset: 0xa0,
        index_allocation_size: 0x04,
        entries_write_mapping_options: 0x0400_0101,
        read_cfi_mapping_options: 0x0400_1101,
        cleanup_frees_allocations_proven: true,
    };
const DATA_MASTER_RING_SCRATCH_INDICES_NO_PROVEN_CLEANUP: DataMasterRingIndexBacking =
    DataMasterRingIndexBacking::FenderScratchRam {
        scratch_object_offset: 0x28,
        read_cpu_pointer_offset: 0x30,
        cfi_cpu_pointer_offset: 0x38,
        write_cpu_pointer_offset: 0x40,
        read_firmware_pointer_offset: 0x48,
        cfi_firmware_pointer_offset: 0x50,
        write_firmware_pointer_offset: 0x58,
        entries_write_allocator_offset: 0x98,
        read_cfi_allocator_offset: 0xa0,
        index_allocation_size: 0x04,
        entries_write_mapping_options: 0x0400_0101,
        read_cfi_mapping_options: 0x0400_1101,
        cleanup_frees_allocations_proven: false,
    };
const DATA_MASTER_RING_SCRATCH_ENTRIES: DataMasterRingEntryBacking =
    DataMasterRingEntryBacking::FenderScratchRam {
        allocator_offset: 0x98,
        allocation_size: 0x1800,
        mapping_options: 0x0400_0101,
        cleanup_frees_allocation_proven: true,
    };

const fn data_master_ring_shared_config(
    entry_resource_owner_object_offset: u16,
    index_resource_owner_object_offset: u16,
    cold_init_ring_count: u8,
) -> DataMasterRingConfig {
    DataMasterRingConfig {
        index_backing: data_master_ring_shared_indices(index_resource_owner_object_offset),
        entry_backing: data_master_ring_shared_entries(entry_resource_owner_object_offset),
        entries_cpu_object_offset: 0x18,
        entries_firmware_object_offset: 0x20,
        host_index_mutation: DataMasterRingHostIndexMutation::DirectSharedStores,
        get_read_index_vtable_offset: 0x30,
        get_write_index_vtable_offset: 0x38,
        set_write_index_vtable_offset: 0x10,
        entry_stride: 0x18,
        index_count: 0x100,
        usable_entry_count: 0xff,
        cold_init_ring_count,
        shared_data_cacheability_names_proven: false,
        firmware_consumer_ownership_proven: false,
        publication_steps: DATA_MASTER_RING_PUBLICATION_STEPS,
    }
}

const DATA_MASTER_RING_CONFIG_G15G: DataMasterRingConfig =
    data_master_ring_shared_config(0x310, 0x548, 12);
const DATA_MASTER_RING_CONFIG_G16X: DataMasterRingConfig =
    data_master_ring_shared_config(0x350, 0x588, 12);
const DATA_MASTER_RING_CONFIG_G17P_G17X: DataMasterRingConfig =
    data_master_ring_shared_config(0x350, 0x588, 24);
const DATA_MASTER_RING_CONFIG_G17G: DataMasterRingConfig =
    data_master_ring_shared_config(0x358, 0x590, 24);
const DATA_MASTER_RING_CONFIG_SCRATCH_ENTRIES: DataMasterRingConfig = DataMasterRingConfig {
    index_backing: DATA_MASTER_RING_SCRATCH_INDICES_CLEANUP,
    entry_backing: DATA_MASTER_RING_SCRATCH_ENTRIES,
    entries_cpu_object_offset: 0x18,
    entries_firmware_object_offset: 0x20,
    host_index_mutation: DataMasterRingHostIndexMutation::LockedScratchWriteOnly,
    get_read_index_vtable_offset: 0x30,
    get_write_index_vtable_offset: 0x38,
    set_write_index_vtable_offset: 0x10,
    entry_stride: 0x18,
    index_count: 0x100,
    usable_entry_count: 0xff,
    cold_init_ring_count: 12,
    shared_data_cacheability_names_proven: false,
    firmware_consumer_ownership_proven: false,
    publication_steps: DATA_MASTER_RING_PUBLICATION_STEPS,
};
const DATA_MASTER_RING_CONFIG_SCRATCH_INDICES: DataMasterRingConfig = DataMasterRingConfig {
    index_backing: DATA_MASTER_RING_SCRATCH_INDICES_NO_PROVEN_CLEANUP,
    entry_backing: data_master_ring_shared_entries(0x318),
    host_index_mutation: DataMasterRingHostIndexMutation::LockedScratchWriteOnly,
    ..DATA_MASTER_RING_CONFIG_G16X
};

const DATA_MASTER_FIRMWARE_TABLE_CONFIG_G15G: DataMasterFirmwareTableConfig =
    make_data_master_firmware_table_config(
        0x800,
        0x368,
        0x408,
        0x4a8,
        0x28,
        DataMasterFirmwareIndexHandoff::MirrorReadAndCfiToWrite,
    );
const DATA_MASTER_FIRMWARE_TABLE_CONFIG_G15S: DataMasterFirmwareTableConfig =
    make_data_master_firmware_table_config(
        0xbc0,
        0x368,
        0x548,
        0x728,
        0x78,
        DataMasterFirmwareIndexHandoff::NoHostReadOrCfiStore,
    );
const DATA_MASTER_FIRMWARE_TABLE_CONFIG_G15C_D: DataMasterFirmwareTableConfig =
    make_data_master_firmware_table_config(
        0xbc0,
        0x368,
        0x548,
        0x728,
        0x78,
        DataMasterFirmwareIndexHandoff::NoHostReadOrCfiStore,
    );
const DATA_MASTER_FIRMWARE_TABLE_CONFIG_G16G: DataMasterFirmwareTableConfig =
    make_data_master_firmware_table_config(
        0xbc8,
        0x370,
        0x550,
        0x730,
        0x78,
        DataMasterFirmwareIndexHandoff::NoHostReadOrCfiStore,
    );
const DATA_MASTER_FIRMWARE_TABLE_CONFIG_G16X: DataMasterFirmwareTableConfig =
    make_data_master_firmware_table_config(
        0x840,
        0x3a8,
        0x448,
        0x4e8,
        0x28,
        DataMasterFirmwareIndexHandoff::MirrorReadAndCfiToWrite,
    );
const DATA_MASTER_FIRMWARE_TABLE_CONFIG_G17P: DataMasterFirmwareTableConfig =
    make_data_master_firmware_table_config(
        0xa80,
        0x3a8,
        0x448,
        0x4e8,
        0x28,
        DataMasterFirmwareIndexHandoff::MirrorReadAndCfiToWrite,
    );
const DATA_MASTER_FIRMWARE_TABLE_CONFIG_G17G: DataMasterFirmwareTableConfig =
    make_data_master_firmware_table_config(
        0xa88,
        0x3b0,
        0x450,
        0x4f0,
        0x28,
        DataMasterFirmwareIndexHandoff::MirrorReadAndCfiToWrite,
    );
const DATA_MASTER_FIRMWARE_TABLE_CONFIG_G17X: DataMasterFirmwareTableConfig =
    make_data_master_firmware_table_config(
        0xa80,
        0x3a8,
        0x448,
        0x4e8,
        0x28,
        DataMasterFirmwareIndexHandoff::MirrorReadAndCfiToWrite,
    );

/// Identity and userspace-visible architecture axes decoded from GPU ID MMIO.
#[derive(Debug, PartialEq, Eq, Copy, Clone)]
pub(crate) struct GpuIdentity {
    pub(crate) gpu_gen: GpuGen,
    pub(crate) gpu_variant: GpuVariant,
    pub(crate) usc_generation: u32,
    pub(crate) gpu_hal_generation: GpuHalGeneration,
    pub(crate) firmware_roles: FirmwareRoleTopology,
    pub(crate) submission_transport: SubmissionTransport,
    /// Input-address bits per UAT root.
    pub(crate) uat_input_address_bits: u8,
}

/// Decode the hardware family/variant/die tuple reported at 0xd04000/0xd04010.
///
/// This deliberately does not imply that firmware, initdata, command
/// submission, or a core-mask register layout is implemented for the returned
/// identity.
pub(crate) fn decode_gpu_identity(family: u8, variant: u8, num_dies: u8) -> Option<GpuIdentity> {
    use GpuGen::*;
    use GpuHalGeneration::*;
    use GpuVariant::*;

    let (gpu_gen, gpu_variant, usc_generation, gpu_hal_generation) = match (family, variant) {
        (0x4, 0) => (G13, P, 2, Legacy),
        (0x4, 2) => (G13, G, 2, Legacy),
        (0x4, 3) => (G13, S, 2, Legacy),
        // G13 has no D core identity; C covers both Max and Ultra.
        (0x4, 4) => (G13, C, 2, Legacy),

        (0x5, 0) => (G14, P, 2, Legacy),
        (0x5, 2) => (G14, G, 2, Legacy),
        (0x6, 3) => (G14, S, 2, Legacy),
        (0x6, 4) => match num_dies {
            1 => (G14, C, 2, Legacy),
            2 => (G14, D, 2, Legacy),
            _ => return None,
        },

        // G15P is A16, not an M3 GPU. Apple's compiler routes
        // applegpu_g15p through the AGX2 backend even though the design
        // generation is 15. Keep USC generation independent from gpu_gen.
        (0x7, 0) => (G15, P, 2, Legacy),
        (0x7, 2) => (G15, G, 3, Legacy),
        (0x7, 3) => (G15, S, 3, Legacy),
        (0x7, 4) => match num_dies {
            1 => (G15, C, 3, Legacy),
            2 => (G15, D, 3, Legacy),
            _ => return None,
        },

        (0x8, 0) => (G16, P, 3, Hal200),

        // Family 0xa crosses the design-generation boundary.
        (0xa, 0) => (G17, P, 3, Hal200),
        (0xa, 2) => (G16, G, 3, Hal200),
        (0xa, 3) => (G16, S, 3, Hal200),
        (0xa, 4) => (G16, C, 3, Hal200),
        (0xa, 5) => (G17, A, 3, Hal200),

        (0xb, 2) => (G17, G, 3, Hal300),
        (0xb, 3) => (G17, S, 3, Hal300),
        (0xb, 4) => (G17, C, 3, Hal300),
        _ => return None,
    };

    let firmware_roles = if gpu_gen == G17 {
        FirmwareRoleTopology::Dual
    } else {
        FirmwareRoleTopology::Single
    };

    let submission_transport = match (family, variant) {
        // G16X and G17P/G17A carry both stacks. The runtime selector is not
        // yet known, so callers must not silently choose one.
        (0xa, 0 | 3 | 4 | 5) => SubmissionTransport::ClassicAndSksm,
        // HAL300 removes the classic SKU channel stack.
        (0xb, 2 | 3 | 4) => SubmissionTransport::Sksm,
        _ => SubmissionTransport::Classic,
    };

    let uat_input_address_bits = if matches!(gpu_gen, G13 | G14) { 39 } else { 42 };

    Some(GpuIdentity {
        gpu_gen,
        gpu_variant,
        usc_generation,
        gpu_hal_generation,
        firmware_roles,
        submission_transport,
        uat_input_address_bits,
    })
}

/// Return the independently recovered SKSM queue-ID layout for one target.
///
/// The values are byte-identical in the pinned G16X (G16S/C), G17P, G17G and
/// G17X (G17S/C) host drivers. G17A is deliberately absent: sharing a host
/// kext with G17P is not independent target evidence. Classic-only targets do
/// not have an SKSM layout, while the presence of a layout on G16X/G17P does
/// not resolve their still-unknown classic-versus-SKSM runtime selector.
#[allow(dead_code)]
pub(crate) const fn sksm_kick_queue_id_config(
    gpu_gen: GpuGen,
    gpu_variant: GpuVariant,
) -> Option<SksmKickQueueIdConfig> {
    use GpuGen::*;
    use GpuVariant::*;

    match (gpu_gen, gpu_variant) {
        (G16, S | C) | (G17, P | G | S | C) => Some(SKSM_KICK_QUEUE_ID_CONFIG),
        _ => None,
    }
}

/// Validate externally recovered SKSM queue-ID properties for one target.
#[allow(dead_code)]
pub(crate) const fn validate_sksm_kick_queue_id_config(
    gpu_gen: GpuGen,
    gpu_variant: GpuVariant,
    shift: u8,
    mask: u8,
) -> Result<SksmKickQueueIdConfig, SubmissionConfigError> {
    let Some(expected) = sksm_kick_queue_id_config(gpu_gen, gpu_variant) else {
        return Err(SubmissionConfigError::UnsupportedTarget);
    };

    if shift != expected.shift || mask != expected.mask {
        Err(SubmissionConfigError::UnexpectedSksmKickQueueIdConfig)
    } else {
        Ok(expected)
    }
}

/// Encode the exact cross-generation host firmware-kick word and role route.
///
/// Pinned G15/G16 hosts always send through their single `GFX` role. Every
/// pinned G17 host instead selects `GFX1` only for opaque message type seven;
/// all other six-bit message types use `GFX`. The destination endpoint and
/// the complete queue publication/notification lifecycle remain deliberately
/// outside this model.
#[allow(dead_code)]
pub(crate) const fn encode_firmware_kick_route(
    gpu_gen: GpuGen,
    gpu_variant: GpuVariant,
    kick_index: u8,
    message_type: u8,
) -> Result<FirmwareKickRoute, SubmissionConfigError> {
    use GpuGen::*;
    use GpuVariant::*;

    let target_is_proven = matches!(
        (gpu_gen, gpu_variant),
        (G15, G | S | C | D) | (G16, G | S | C) | (G17, P | G | S | C)
    );
    if !target_is_proven {
        return Err(SubmissionConfigError::UnsupportedTarget);
    }
    if kick_index > FIRMWARE_KICK_INDEX_MASK {
        return Err(SubmissionConfigError::KickIndexOutOfRange);
    }
    if message_type > FIRMWARE_MESSAGE_TYPE_MASK {
        return Err(SubmissionConfigError::FirmwareMessageTypeOutOfRange);
    }

    let role = match (gpu_gen, message_type) {
        (G17, G17_GFX1_FIRMWARE_MESSAGE_TYPE) => SubmissionFirmwareRole::Gfx1,
        _ => SubmissionFirmwareRole::Gfx,
    };
    let message = FIRMWARE_KICK_TAG
        | ((message_type as u64) << FIRMWARE_MESSAGE_TYPE_SHIFT)
        | ((kick_index as u64) << FIRMWARE_KICK_INDEX_SHIFT);

    Ok(FirmwareKickRoute { role, message })
}

/// Return the exact RTBuddy endpoint route used by one host firmware kick.
///
/// The host's kick call is vtable slot 0x8a8, which is the asynchronous-note
/// sender rather than the ordinary message sender at slot 0x898. The gated
/// sender consequently dereferences the endpoint-0x21 object at offset 0x130;
/// endpoint 0x20 is bound separately at offset 0x128. These values and the
/// opaque endpoint-send arguments are byte-identical in the pinned G15G/X,
/// G16G/X, G17P, G17G and G17X RTBuddy drivers.
///
/// This does not prove queue allocation, shared-memory ownership, publication,
/// cache maintenance, acknowledgment, or teardown ordering.
#[allow(dead_code)]
pub(crate) const fn firmware_kick_endpoint_config(
    gpu_gen: GpuGen,
    gpu_variant: GpuVariant,
) -> Option<FirmwareKickEndpointConfig> {
    use GpuGen::*;
    use GpuVariant::*;

    match (gpu_gen, gpu_variant) {
        (G15, G | S | C | D) | (G16, G | S | C) | (G17, P | G | S | C) => {
            Some(FIRMWARE_KICK_ENDPOINT_CONFIG)
        }
        _ => None,
    }
}

/// Validate externally recovered RTBuddy endpoint-binding properties.
#[allow(dead_code)]
pub(crate) const fn validate_firmware_kick_endpoint_config(
    gpu_gen: GpuGen,
    gpu_variant: GpuVariant,
    supplied: FirmwareKickEndpointConfig,
) -> Result<FirmwareKickEndpointConfig, SubmissionConfigError> {
    let Some(expected) = firmware_kick_endpoint_config(gpu_gen, gpu_variant) else {
        return Err(SubmissionConfigError::UnsupportedTarget);
    };

    if supplied.sender_vtable_offset != expected.sender_vtable_offset
        || supplied.gated_sender_vtable_offset != expected.gated_sender_vtable_offset
        || supplied.endpoint_service_id_offset != expected.endpoint_service_id_offset
        || supplied.message_endpoint_id != expected.message_endpoint_id
        || supplied.message_endpoint_object_offset != expected.message_endpoint_object_offset
        || supplied.async_note_endpoint_id != expected.async_note_endpoint_id
        || supplied.async_note_endpoint_object_offset != expected.async_note_endpoint_object_offset
        || supplied.endpoint_send_vtable_offset != expected.endpoint_send_vtable_offset
        || supplied.sender_arg2 != expected.sender_arg2
        || supplied.endpoint_send_arg2 != expected.endpoint_send_arg2
        || supplied.endpoint_send_arg3 != expected.endpoint_send_arg3
    {
        Err(SubmissionConfigError::UnexpectedFirmwareKickEndpointConfig)
    } else {
        Ok(expected)
    }
}

/// Return the exact host-side data-master ring contract for one target.
///
/// The pinned G15G/X, G16G/X, G17P, G17G and G17X hosts all use 256 indices,
/// reserve one slot to distinguish full from empty, and address 24-byte entries
/// with the current write index. Their backing differs materially: G15S/C/D
/// allocate indices and entries from Fender ScratchRAM, G16G puts only the
/// indices there, and the other targets use firmware shared-data batches. The
/// host initializes 12 rings on single-role G15/G16 and 24 on dual-role G17.
/// Publication is ordered as recorded here, including the `dmb ish` before the
/// write-index update.
///
/// ScratchRAM CPU mappings are exactly inhibit-cache; read/CFI are additionally
/// read-only. Generic entries and indices are distinct 12-ring batches with raw
/// caching-option values 1 and 0 respectively. Each batch is owned and released
/// through one target-specific `AGXFirmware` slot. The stripped option names and
/// firmware-side consumer ownership remain unproved, and no host object offset
/// is executable.
#[allow(dead_code)]
pub(crate) const fn data_master_ring_config(
    gpu_gen: GpuGen,
    gpu_variant: GpuVariant,
) -> Option<DataMasterRingConfig> {
    use GpuGen::*;
    use GpuVariant::*;

    match (gpu_gen, gpu_variant) {
        (G15, G) => Some(DATA_MASTER_RING_CONFIG_G15G),
        (G15, S | C | D) => Some(DATA_MASTER_RING_CONFIG_SCRATCH_ENTRIES),
        (G16, G) => Some(DATA_MASTER_RING_CONFIG_SCRATCH_INDICES),
        (G16, S | C) => Some(DATA_MASTER_RING_CONFIG_G16X),
        (G17, P | S | C) => Some(DATA_MASTER_RING_CONFIG_G17P_G17X),
        (G17, G) => Some(DATA_MASTER_RING_CONFIG_G17G),
        _ => None,
    }
}

/// Return the exact ring-pointer table published by `initFirmwareData`.
///
/// Each pinned host publishes four priorities in priority-major order. Within
/// each priority, TA, 3D and CL occupy three consecutive 0x20-byte records.
/// Each record carries firmware pointers for read, CFI, write and entries at
/// offsets 0, 8, 0x10 and 0x18. The corresponding submit methods independently
/// select a ring object as the class base plus `priority * ring_object_stride`
/// and pass ordinals 0, 1 and 2 to their command encoders.
///
/// This describes the classic submit-method ring path only. The firmware code
/// that advances the read index and the runtime selector between classic and
/// SKSM stacks have not been pinned, so both claims remain explicitly false.
#[allow(dead_code)]
pub(crate) const fn data_master_firmware_table_config(
    gpu_gen: GpuGen,
    gpu_variant: GpuVariant,
) -> Option<DataMasterFirmwareTableConfig> {
    use GpuGen::*;
    use GpuVariant::*;

    match (gpu_gen, gpu_variant) {
        (G15, G) => Some(DATA_MASTER_FIRMWARE_TABLE_CONFIG_G15G),
        (G15, S) => Some(DATA_MASTER_FIRMWARE_TABLE_CONFIG_G15S),
        (G15, C | D) => Some(DATA_MASTER_FIRMWARE_TABLE_CONFIG_G15C_D),
        (G16, G) => Some(DATA_MASTER_FIRMWARE_TABLE_CONFIG_G16G),
        (G16, S | C) => Some(DATA_MASTER_FIRMWARE_TABLE_CONFIG_G16X),
        (G17, P) => Some(DATA_MASTER_FIRMWARE_TABLE_CONFIG_G17P),
        (G17, G) => Some(DATA_MASTER_FIRMWARE_TABLE_CONFIG_G17G),
        (G17, S | C) => Some(DATA_MASTER_FIRMWARE_TABLE_CONFIG_G17X),
        _ => None,
    }
}

const fn data_master_firmware_index_handoff_matches(
    supplied: DataMasterFirmwareIndexHandoff,
    expected: DataMasterFirmwareIndexHandoff,
) -> bool {
    matches!(
        (supplied, expected),
        (
            DataMasterFirmwareIndexHandoff::MirrorReadAndCfiToWrite,
            DataMasterFirmwareIndexHandoff::MirrorReadAndCfiToWrite,
        ) | (
            DataMasterFirmwareIndexHandoff::NoHostReadOrCfiStore,
            DataMasterFirmwareIndexHandoff::NoHostReadOrCfiStore,
        )
    )
}

/// Validate an externally recovered firmware ring-pointer table contract.
#[allow(dead_code)]
pub(crate) const fn validate_data_master_firmware_table_config(
    gpu_gen: GpuGen,
    gpu_variant: GpuVariant,
    supplied: DataMasterFirmwareTableConfig,
) -> Result<DataMasterFirmwareTableConfig, SubmissionConfigError> {
    let Some(expected) = data_master_firmware_table_config(gpu_gen, gpu_variant) else {
        return Err(SubmissionConfigError::UnsupportedTarget);
    };

    if supplied.descriptor_table_cpu_pointer_object_offset
        != expected.descriptor_table_cpu_pointer_object_offset
        || supplied.ta_ring_object_base_offset != expected.ta_ring_object_base_offset
        || supplied.three_d_ring_object_base_offset != expected.three_d_ring_object_base_offset
        || supplied.cl_ring_object_base_offset != expected.cl_ring_object_base_offset
        || supplied.ring_object_stride != expected.ring_object_stride
        || supplied.priority_count != expected.priority_count
        || supplied.data_master_count != expected.data_master_count
        || supplied.descriptor_record_stride != expected.descriptor_record_stride
        || supplied.read_index_firmware_pointer_offset
            != expected.read_index_firmware_pointer_offset
        || supplied.cfi_index_firmware_pointer_offset != expected.cfi_index_firmware_pointer_offset
        || supplied.write_index_firmware_pointer_offset
            != expected.write_index_firmware_pointer_offset
        || supplied.entries_firmware_pointer_offset != expected.entries_firmware_pointer_offset
        || !data_master_firmware_index_handoff_matches(
            supplied.index_handoff,
            expected.index_handoff,
        )
        || supplied.firmware_read_index_acknowledgment_writer_proven
            != expected.firmware_read_index_acknowledgment_writer_proven
        || supplied.dual_stack_runtime_selector_proven
            != expected.dual_stack_runtime_selector_proven
    {
        Err(SubmissionConfigError::UnexpectedDataMasterFirmwareTableConfig)
    } else {
        Ok(expected)
    }
}

/// Select the exact ring object and firmware descriptor record for one class
/// and priority without reading or modifying any shared state.
#[allow(dead_code)]
pub(crate) const fn data_master_firmware_ring_route(
    gpu_gen: GpuGen,
    gpu_variant: GpuVariant,
    data_master: DataMasterClass,
    priority: u8,
) -> Result<DataMasterFirmwareRingRoute, SubmissionConfigError> {
    let Some(config) = data_master_firmware_table_config(gpu_gen, gpu_variant) else {
        return Err(SubmissionConfigError::UnsupportedTarget);
    };
    if priority >= config.priority_count {
        return Err(SubmissionConfigError::DataMasterPriorityOutOfRange);
    }

    let (encoder_data_master_ordinal, ring_object_base_offset) = match data_master {
        DataMasterClass::Ta => (0u8, config.ta_ring_object_base_offset),
        DataMasterClass::ThreeD => (1, config.three_d_ring_object_base_offset),
        DataMasterClass::Cl => (2, config.cl_ring_object_base_offset),
    };
    let ring_object_offset =
        ring_object_base_offset + priority as u16 * config.ring_object_stride as u16;
    let descriptor_record_offset = (priority as u16 * config.data_master_count as u16
        + encoder_data_master_ordinal as u16)
        * config.descriptor_record_stride as u16;

    Ok(DataMasterFirmwareRingRoute {
        encoder_data_master_ordinal,
        ring_object_offset,
        descriptor_record_offset,
        read_index_firmware_pointer_offset: descriptor_record_offset
            + config.read_index_firmware_pointer_offset as u16,
        cfi_index_firmware_pointer_offset: descriptor_record_offset
            + config.cfi_index_firmware_pointer_offset as u16,
        write_index_firmware_pointer_offset: descriptor_record_offset
            + config.write_index_firmware_pointer_offset as u16,
        entries_firmware_pointer_offset: descriptor_record_offset
            + config.entries_firmware_pointer_offset as u16,
    })
}

const fn data_master_ring_index_backing_matches(
    supplied: DataMasterRingIndexBacking,
    expected: DataMasterRingIndexBacking,
) -> bool {
    use DataMasterRingIndexBacking::*;

    match (supplied, expected) {
        (
            FirmwareSharedData {
                resource_owner_object_offset: supplied_owner,
                cpu_object_offset: supplied_cpu,
                firmware_object_offset: supplied_firmware,
                read_index_offset: supplied_read,
                cfi_index_offset: supplied_cfi,
                write_index_offset: supplied_write,
                slice_size: supplied_size,
                caching_options: supplied_caching,
                allocate_shared_data_arg: supplied_arg,
                batch_ring_count: supplied_count,
                release_zeroes_owner_slot: supplied_release,
            },
            FirmwareSharedData {
                resource_owner_object_offset: expected_owner,
                cpu_object_offset: expected_cpu,
                firmware_object_offset: expected_firmware,
                read_index_offset: expected_read,
                cfi_index_offset: expected_cfi,
                write_index_offset: expected_write,
                slice_size: expected_size,
                caching_options: expected_caching,
                allocate_shared_data_arg: expected_arg,
                batch_ring_count: expected_count,
                release_zeroes_owner_slot: expected_release,
            },
        ) => {
            supplied_owner == expected_owner
                && supplied_cpu == expected_cpu
                && supplied_firmware == expected_firmware
                && supplied_read == expected_read
                && supplied_cfi == expected_cfi
                && supplied_write == expected_write
                && supplied_size == expected_size
                && supplied_caching == expected_caching
                && supplied_arg == expected_arg
                && supplied_count == expected_count
                && supplied_release == expected_release
        }
        (
            FenderScratchRam {
                scratch_object_offset: supplied_scratch,
                read_cpu_pointer_offset: supplied_read_cpu,
                cfi_cpu_pointer_offset: supplied_cfi_cpu,
                write_cpu_pointer_offset: supplied_write_cpu,
                read_firmware_pointer_offset: supplied_read_firmware,
                cfi_firmware_pointer_offset: supplied_cfi_firmware,
                write_firmware_pointer_offset: supplied_write_firmware,
                entries_write_allocator_offset: supplied_write_allocator,
                read_cfi_allocator_offset: supplied_read_cfi_allocator,
                index_allocation_size: supplied_size,
                entries_write_mapping_options: supplied_write_options,
                read_cfi_mapping_options: supplied_read_cfi_options,
                cleanup_frees_allocations_proven: supplied_cleanup,
            },
            FenderScratchRam {
                scratch_object_offset: expected_scratch,
                read_cpu_pointer_offset: expected_read_cpu,
                cfi_cpu_pointer_offset: expected_cfi_cpu,
                write_cpu_pointer_offset: expected_write_cpu,
                read_firmware_pointer_offset: expected_read_firmware,
                cfi_firmware_pointer_offset: expected_cfi_firmware,
                write_firmware_pointer_offset: expected_write_firmware,
                entries_write_allocator_offset: expected_write_allocator,
                read_cfi_allocator_offset: expected_read_cfi_allocator,
                index_allocation_size: expected_size,
                entries_write_mapping_options: expected_write_options,
                read_cfi_mapping_options: expected_read_cfi_options,
                cleanup_frees_allocations_proven: expected_cleanup,
            },
        ) => {
            supplied_scratch == expected_scratch
                && supplied_read_cpu == expected_read_cpu
                && supplied_cfi_cpu == expected_cfi_cpu
                && supplied_write_cpu == expected_write_cpu
                && supplied_read_firmware == expected_read_firmware
                && supplied_cfi_firmware == expected_cfi_firmware
                && supplied_write_firmware == expected_write_firmware
                && supplied_write_allocator == expected_write_allocator
                && supplied_read_cfi_allocator == expected_read_cfi_allocator
                && supplied_size == expected_size
                && supplied_write_options == expected_write_options
                && supplied_read_cfi_options == expected_read_cfi_options
                && supplied_cleanup == expected_cleanup
        }
        _ => false,
    }
}

const fn data_master_ring_entry_backing_matches(
    supplied: DataMasterRingEntryBacking,
    expected: DataMasterRingEntryBacking,
) -> bool {
    use DataMasterRingEntryBacking::*;

    match (supplied, expected) {
        (
            FirmwareSharedData {
                resource_owner_object_offset: supplied_owner,
                slice_size: supplied_size,
                caching_options: supplied_caching,
                allocate_shared_data_arg: supplied_arg,
                batch_ring_count: supplied_count,
                release_zeroes_owner_slot: supplied_release,
            },
            FirmwareSharedData {
                resource_owner_object_offset: expected_owner,
                slice_size: expected_size,
                caching_options: expected_caching,
                allocate_shared_data_arg: expected_arg,
                batch_ring_count: expected_count,
                release_zeroes_owner_slot: expected_release,
            },
        ) => {
            supplied_owner == expected_owner
                && supplied_size == expected_size
                && supplied_caching == expected_caching
                && supplied_arg == expected_arg
                && supplied_count == expected_count
                && supplied_release == expected_release
        }
        (
            FenderScratchRam {
                allocator_offset: supplied_allocator,
                allocation_size: supplied_size,
                mapping_options: supplied_options,
                cleanup_frees_allocation_proven: supplied_cleanup,
            },
            FenderScratchRam {
                allocator_offset: expected_allocator,
                allocation_size: expected_size,
                mapping_options: expected_options,
                cleanup_frees_allocation_proven: expected_cleanup,
            },
        ) => {
            supplied_allocator == expected_allocator
                && supplied_size == expected_size
                && supplied_options == expected_options
                && supplied_cleanup == expected_cleanup
        }
        _ => false,
    }
}

/// Validate an externally recovered host-side data-master ring contract.
#[allow(dead_code)]
pub(crate) const fn validate_data_master_ring_config(
    gpu_gen: GpuGen,
    gpu_variant: GpuVariant,
    supplied: DataMasterRingConfig,
) -> Result<DataMasterRingConfig, SubmissionConfigError> {
    let Some(expected) = data_master_ring_config(gpu_gen, gpu_variant) else {
        return Err(SubmissionConfigError::UnsupportedTarget);
    };

    if !data_master_ring_index_backing_matches(supplied.index_backing, expected.index_backing)
        || !data_master_ring_entry_backing_matches(supplied.entry_backing, expected.entry_backing)
        || supplied.entries_cpu_object_offset != expected.entries_cpu_object_offset
        || supplied.entries_firmware_object_offset != expected.entries_firmware_object_offset
        || !matches!(
            (supplied.host_index_mutation, expected.host_index_mutation),
            (
                DataMasterRingHostIndexMutation::DirectSharedStores,
                DataMasterRingHostIndexMutation::DirectSharedStores,
            ) | (
                DataMasterRingHostIndexMutation::LockedScratchWriteOnly,
                DataMasterRingHostIndexMutation::LockedScratchWriteOnly,
            )
        )
        || supplied.get_read_index_vtable_offset != expected.get_read_index_vtable_offset
        || supplied.get_write_index_vtable_offset != expected.get_write_index_vtable_offset
        || supplied.set_write_index_vtable_offset != expected.set_write_index_vtable_offset
        || supplied.entry_stride != expected.entry_stride
        || supplied.index_count != expected.index_count
        || supplied.usable_entry_count != expected.usable_entry_count
        || supplied.cold_init_ring_count != expected.cold_init_ring_count
        || supplied.shared_data_cacheability_names_proven
            != expected.shared_data_cacheability_names_proven
        || supplied.firmware_consumer_ownership_proven
            != expected.firmware_consumer_ownership_proven
        || !matches!(
            supplied.publication_steps,
            [
                DataMasterRingPublicationStep::SelectNextEntry,
                DataMasterRingPublicationStep::RecordEncoded,
                DataMasterRingPublicationStep::DmbIsh,
                DataMasterRingPublicationStep::ReadWriteIndex,
                DataMasterRingPublicationStep::IncrementAndWrap8,
                DataMasterRingPublicationStep::StoreWriteIndex,
            ]
        )
    {
        Err(SubmissionConfigError::UnexpectedDataMasterRingConfig)
    } else {
        Ok(expected)
    }
}

/// Apply the exact host full/wrap and entry-offset calculation to ring state.
///
/// This is a pure diagnostic. It neither reserves an entry nor publishes an
/// index, and therefore carries no shared-memory ownership claim.
#[allow(dead_code)]
pub(crate) const fn data_master_ring_next_entry(
    gpu_gen: GpuGen,
    gpu_variant: GpuVariant,
    read_index: u16,
    write_index: u16,
) -> Result<DataMasterRingNextEntry, SubmissionConfigError> {
    let Some(config) = data_master_ring_config(gpu_gen, gpu_variant) else {
        return Err(SubmissionConfigError::UnsupportedTarget);
    };
    if read_index >= config.index_count {
        return Err(SubmissionConfigError::DataMasterReadIndexOutOfRange);
    }
    if write_index >= config.index_count {
        return Err(SubmissionConfigError::DataMasterWriteIndexOutOfRange);
    }

    let published_write_index = write_index.wrapping_add(1) as u8;
    if published_write_index == read_index as u8 {
        return Err(SubmissionConfigError::DataMasterRingFull);
    }

    Ok(DataMasterRingNextEntry {
        entry_offset_from_entries_base: write_index * config.entry_stride as u16,
        published_write_index,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(family: u8, variant: u8, dies: u8) -> GpuIdentity {
        decode_gpu_identity(family, variant, dies).expect("identity should decode")
    }

    #[test]
    fn variant_zero_is_p_and_variant_one_is_rejected() {
        assert_eq!(id(0x4, 0, 1).gpu_variant, GpuVariant::P);
        assert_eq!(id(0x7, 0, 1).gpu_variant, GpuVariant::P);
        assert_eq!(id(0xa, 0, 1).gpu_variant, GpuVariant::P);
        assert!(decode_gpu_identity(0x4, 1, 1).is_none());
        assert!(decode_gpu_identity(0x7, 1, 1).is_none());
    }

    #[test]
    fn family_a_crosses_the_generation_boundary() {
        assert_eq!(id(0xa, 0, 1).gpu_gen, GpuGen::G17);
        assert_eq!(id(0xa, 0, 1).gpu_variant, GpuVariant::P);
        assert_eq!(id(0xa, 2, 1).gpu_gen, GpuGen::G16);
        assert_eq!(id(0xa, 4, 1).gpu_variant, GpuVariant::C);
        assert_eq!(id(0xa, 5, 1).gpu_variant, GpuVariant::A);
    }

    #[test]
    fn c_d_split_uses_die_count_only_where_apple_does() {
        assert_eq!(id(0x6, 4, 1).gpu_variant, GpuVariant::C);
        assert_eq!(id(0x6, 4, 2).gpu_variant, GpuVariant::D);
        assert_eq!(id(0x7, 4, 1).gpu_variant, GpuVariant::C);
        assert_eq!(id(0x7, 4, 2).gpu_variant, GpuVariant::D);
        assert_eq!(id(0xa, 4, 2).gpu_variant, GpuVariant::C);
        assert_eq!(id(0xb, 4, 2).gpu_variant, GpuVariant::C);
    }

    #[test]
    fn architecture_axes_are_independent() {
        let g14 = id(0x6, 3, 1);
        assert_eq!(g14.usc_generation, 2);
        assert_eq!(g14.gpu_hal_generation, GpuHalGeneration::Legacy);

        let g15p = id(0x7, 0, 1);
        assert_eq!(g15p.gpu_gen, GpuGen::G15);
        assert_eq!(g15p.usc_generation, 2);

        let g15 = id(0x7, 2, 1);
        assert_eq!(g15.usc_generation, 3);
        assert_eq!(g15.gpu_hal_generation, GpuHalGeneration::Legacy);

        let g17p = id(0xa, 0, 1);
        assert_eq!(g17p.gpu_gen, GpuGen::G17);
        assert_eq!(g17p.gpu_hal_generation, GpuHalGeneration::Hal200);

        let g17g = id(0xb, 2, 1);
        assert_eq!(g17g.gpu_gen, GpuGen::G17);
        assert_eq!(g17g.gpu_hal_generation, GpuHalGeneration::Hal300);
    }

    #[test]
    fn g15_g16_g17_role_and_submission_routing_is_exact() {
        for (variant, dies) in [(0, 1), (2, 1), (3, 1), (4, 1), (4, 2)] {
            let g15 = id(0x7, variant, dies);
            assert_eq!(g15.gpu_gen, GpuGen::G15);
            assert_eq!(g15.usc_generation, if variant == 0 { 2 } else { 3 });
            assert_eq!(g15.gpu_hal_generation, GpuHalGeneration::Legacy);
            assert_eq!(g15.firmware_roles, FirmwareRoleTopology::Single);
            assert_eq!(g15.submission_transport, SubmissionTransport::Classic);
            assert_eq!(g15.uat_input_address_bits, 42);
        }

        for (family, variant, transport) in [
            (0x8, 0, SubmissionTransport::Classic),
            (0xa, 2, SubmissionTransport::Classic),
            (0xa, 3, SubmissionTransport::ClassicAndSksm),
            (0xa, 4, SubmissionTransport::ClassicAndSksm),
        ] {
            let g16 = id(family, variant, 1);
            assert_eq!(g16.gpu_gen, GpuGen::G16);
            assert_eq!(g16.gpu_hal_generation, GpuHalGeneration::Hal200);
            assert_eq!(g16.firmware_roles, FirmwareRoleTopology::Single);
            assert_eq!(g16.submission_transport, transport);
            assert_eq!(g16.uat_input_address_bits, 42);
        }

        for variant in [0, 5] {
            let g17_hal200 = id(0xa, variant, 1);
            assert_eq!(g17_hal200.gpu_gen, GpuGen::G17);
            assert_eq!(g17_hal200.gpu_hal_generation, GpuHalGeneration::Hal200);
            assert_eq!(g17_hal200.firmware_roles, FirmwareRoleTopology::Dual);
            assert_eq!(
                g17_hal200.submission_transport,
                SubmissionTransport::ClassicAndSksm
            );
        }

        for variant in [2, 3, 4] {
            let g17_hal300 = id(0xb, variant, 1);
            assert_eq!(g17_hal300.gpu_gen, GpuGen::G17);
            assert_eq!(g17_hal300.gpu_hal_generation, GpuHalGeneration::Hal300);
            assert_eq!(g17_hal300.firmware_roles, FirmwareRoleTopology::Dual);
            assert_eq!(g17_hal300.submission_transport, SubmissionTransport::Sksm);
            assert_eq!(g17_hal300.uat_input_address_bits, 42);
        }
    }

    #[test]
    fn sksm_kick_queue_id_config_is_exact_and_target_pinned() {
        for (gpu_gen, gpu_variant) in [
            (GpuGen::G16, GpuVariant::S),
            (GpuGen::G16, GpuVariant::C),
            (GpuGen::G17, GpuVariant::P),
            (GpuGen::G17, GpuVariant::G),
            (GpuGen::G17, GpuVariant::S),
            (GpuGen::G17, GpuVariant::C),
        ] {
            assert_eq!(
                validate_sksm_kick_queue_id_config(gpu_gen, gpu_variant, 40, 0x7f),
                Ok(SKSM_KICK_QUEUE_ID_CONFIG)
            );
            assert_eq!(
                validate_sksm_kick_queue_id_config(gpu_gen, gpu_variant, 39, 0x7f),
                Err(SubmissionConfigError::UnexpectedSksmKickQueueIdConfig)
            );
            assert_eq!(
                validate_sksm_kick_queue_id_config(gpu_gen, gpu_variant, 40, 0xff),
                Err(SubmissionConfigError::UnexpectedSksmKickQueueIdConfig)
            );
        }

        for (gpu_gen, gpu_variant) in [
            (GpuGen::G15, GpuVariant::G),
            (GpuGen::G15, GpuVariant::S),
            (GpuGen::G15, GpuVariant::C),
            (GpuGen::G15, GpuVariant::D),
            (GpuGen::G16, GpuVariant::P),
            (GpuGen::G16, GpuVariant::G),
            (GpuGen::G17, GpuVariant::A),
        ] {
            assert_eq!(
                validate_sksm_kick_queue_id_config(gpu_gen, gpu_variant, 40, 0x7f),
                Err(SubmissionConfigError::UnsupportedTarget)
            );
        }

        assert!(GROUNDED_SKSM_KICK_QUEUE_ID_CONFIG_AVAILABLE);
        assert!(!EXECUTABLE_G15_G17_SUBMISSION_AVAILABLE);
    }

    #[test]
    fn firmware_kick_word_and_g17_role_route_are_exact() {
        for (gpu_gen, gpu_variant) in [
            (GpuGen::G15, GpuVariant::G),
            (GpuGen::G15, GpuVariant::S),
            (GpuGen::G15, GpuVariant::C),
            (GpuGen::G15, GpuVariant::D),
            (GpuGen::G16, GpuVariant::G),
            (GpuGen::G16, GpuVariant::S),
            (GpuGen::G16, GpuVariant::C),
            (GpuGen::G17, GpuVariant::P),
            (GpuGen::G17, GpuVariant::G),
            (GpuGen::G17, GpuVariant::S),
            (GpuGen::G17, GpuVariant::C),
        ] {
            assert_eq!(
                encode_firmware_kick_route(gpu_gen, gpu_variant, 4, 3),
                Ok(FirmwareKickRoute {
                    role: SubmissionFirmwareRole::Gfx,
                    message: 0x0083_0000_0000_0010,
                })
            );
        }

        for gpu_variant in [GpuVariant::P, GpuVariant::G, GpuVariant::S, GpuVariant::C] {
            assert_eq!(
                encode_firmware_kick_route(GpuGen::G17, gpu_variant, 7, 7),
                Ok(FirmwareKickRoute {
                    role: SubmissionFirmwareRole::Gfx1,
                    message: 0x0087_0000_0000_001c,
                })
            );
        }
        for (gpu_gen, gpu_variant) in [(GpuGen::G15, GpuVariant::G), (GpuGen::G16, GpuVariant::G)] {
            assert_eq!(
                encode_firmware_kick_route(gpu_gen, gpu_variant, 7, 7),
                Ok(FirmwareKickRoute {
                    role: SubmissionFirmwareRole::Gfx,
                    message: 0x0087_0000_0000_001c,
                })
            );
        }

        assert_eq!(
            encode_firmware_kick_route(GpuGen::G17, GpuVariant::G, 8, 3),
            Err(SubmissionConfigError::KickIndexOutOfRange)
        );
        assert_eq!(
            encode_firmware_kick_route(GpuGen::G17, GpuVariant::G, 0, 0x40),
            Err(SubmissionConfigError::FirmwareMessageTypeOutOfRange)
        );
        for (gpu_gen, gpu_variant) in [
            (GpuGen::G15, GpuVariant::P),
            (GpuGen::G16, GpuVariant::P),
            (GpuGen::G17, GpuVariant::A),
        ] {
            assert_eq!(
                encode_firmware_kick_route(gpu_gen, gpu_variant, 0, 3),
                Err(SubmissionConfigError::UnsupportedTarget)
            );
        }
    }

    #[test]
    fn firmware_kick_endpoint_binding_is_exact_and_target_pinned() {
        for (gpu_gen, gpu_variant) in [
            (GpuGen::G15, GpuVariant::G),
            (GpuGen::G15, GpuVariant::S),
            (GpuGen::G15, GpuVariant::C),
            (GpuGen::G15, GpuVariant::D),
            (GpuGen::G16, GpuVariant::G),
            (GpuGen::G16, GpuVariant::S),
            (GpuGen::G16, GpuVariant::C),
            (GpuGen::G17, GpuVariant::P),
            (GpuGen::G17, GpuVariant::G),
            (GpuGen::G17, GpuVariant::S),
            (GpuGen::G17, GpuVariant::C),
        ] {
            assert_eq!(
                validate_firmware_kick_endpoint_config(
                    gpu_gen,
                    gpu_variant,
                    FIRMWARE_KICK_ENDPOINT_CONFIG,
                ),
                Ok(FIRMWARE_KICK_ENDPOINT_CONFIG)
            );

            for unexpected in [
                FirmwareKickEndpointConfig {
                    sender_vtable_offset: 0x898,
                    ..FIRMWARE_KICK_ENDPOINT_CONFIG
                },
                FirmwareKickEndpointConfig {
                    gated_sender_vtable_offset: 0x8a0,
                    ..FIRMWARE_KICK_ENDPOINT_CONFIG
                },
                FirmwareKickEndpointConfig {
                    endpoint_service_id_offset: 0x80,
                    ..FIRMWARE_KICK_ENDPOINT_CONFIG
                },
                FirmwareKickEndpointConfig {
                    message_endpoint_id: 0x21,
                    ..FIRMWARE_KICK_ENDPOINT_CONFIG
                },
                FirmwareKickEndpointConfig {
                    message_endpoint_object_offset: 0x130,
                    ..FIRMWARE_KICK_ENDPOINT_CONFIG
                },
                FirmwareKickEndpointConfig {
                    async_note_endpoint_id: 0x20,
                    ..FIRMWARE_KICK_ENDPOINT_CONFIG
                },
                FirmwareKickEndpointConfig {
                    async_note_endpoint_object_offset: 0x128,
                    ..FIRMWARE_KICK_ENDPOINT_CONFIG
                },
                FirmwareKickEndpointConfig {
                    endpoint_send_vtable_offset: 0x1e0,
                    ..FIRMWARE_KICK_ENDPOINT_CONFIG
                },
                FirmwareKickEndpointConfig {
                    sender_arg2: 1,
                    ..FIRMWARE_KICK_ENDPOINT_CONFIG
                },
                FirmwareKickEndpointConfig {
                    endpoint_send_arg2: 1,
                    ..FIRMWARE_KICK_ENDPOINT_CONFIG
                },
                FirmwareKickEndpointConfig {
                    endpoint_send_arg3: 0,
                    ..FIRMWARE_KICK_ENDPOINT_CONFIG
                },
            ] {
                assert_eq!(
                    validate_firmware_kick_endpoint_config(gpu_gen, gpu_variant, unexpected),
                    Err(SubmissionConfigError::UnexpectedFirmwareKickEndpointConfig)
                );
            }
        }

        for (gpu_gen, gpu_variant) in [
            (GpuGen::G15, GpuVariant::P),
            (GpuGen::G16, GpuVariant::P),
            (GpuGen::G17, GpuVariant::A),
        ] {
            assert_eq!(
                validate_firmware_kick_endpoint_config(
                    gpu_gen,
                    gpu_variant,
                    FIRMWARE_KICK_ENDPOINT_CONFIG,
                ),
                Err(SubmissionConfigError::UnsupportedTarget)
            );
        }

        assert!(GROUNDED_FIRMWARE_KICK_ENDPOINT_CONFIG_AVAILABLE);
        assert!(!EXECUTABLE_G15_G17_SUBMISSION_AVAILABLE);
    }

    #[test]
    fn data_master_ring_geometry_and_publication_are_exact() {
        for (gpu_gen, gpu_variant, expected) in [
            (GpuGen::G15, GpuVariant::G, DATA_MASTER_RING_CONFIG_G15G),
            (
                GpuGen::G15,
                GpuVariant::S,
                DATA_MASTER_RING_CONFIG_SCRATCH_ENTRIES,
            ),
            (
                GpuGen::G15,
                GpuVariant::C,
                DATA_MASTER_RING_CONFIG_SCRATCH_ENTRIES,
            ),
            (
                GpuGen::G15,
                GpuVariant::D,
                DATA_MASTER_RING_CONFIG_SCRATCH_ENTRIES,
            ),
            (
                GpuGen::G16,
                GpuVariant::G,
                DATA_MASTER_RING_CONFIG_SCRATCH_INDICES,
            ),
            (GpuGen::G16, GpuVariant::S, DATA_MASTER_RING_CONFIG_G16X),
            (GpuGen::G16, GpuVariant::C, DATA_MASTER_RING_CONFIG_G16X),
            (
                GpuGen::G17,
                GpuVariant::P,
                DATA_MASTER_RING_CONFIG_G17P_G17X,
            ),
            (GpuGen::G17, GpuVariant::G, DATA_MASTER_RING_CONFIG_G17G),
            (
                GpuGen::G17,
                GpuVariant::S,
                DATA_MASTER_RING_CONFIG_G17P_G17X,
            ),
            (
                GpuGen::G17,
                GpuVariant::C,
                DATA_MASTER_RING_CONFIG_G17P_G17X,
            ),
        ] {
            let config = data_master_ring_config(gpu_gen, gpu_variant)
                .expect("target should have a pinned data-master ring contract");
            assert_eq!(config, expected);
            assert_eq!(config.entries_cpu_object_offset, 0x18);
            assert_eq!(config.entries_firmware_object_offset, 0x20);
            assert_eq!(config.get_read_index_vtable_offset, 0x30);
            assert_eq!(config.get_write_index_vtable_offset, 0x38);
            assert_eq!(config.set_write_index_vtable_offset, 0x10);
            assert_eq!(config.entry_stride, 0x18);
            assert_eq!(config.index_count, 0x100);
            assert_eq!(config.usable_entry_count, 0xff);
            assert!(!config.shared_data_cacheability_names_proven);
            assert!(!config.firmware_consumer_ownership_proven);
            assert_eq!(config.publication_steps, DATA_MASTER_RING_PUBLICATION_STEPS);
            assert_eq!(
                validate_data_master_ring_config(gpu_gen, gpu_variant, config),
                Ok(config)
            );

            for recovered_layout in [
                DATA_MASTER_RING_CONFIG_G15G,
                DATA_MASTER_RING_CONFIG_SCRATCH_ENTRIES,
                DATA_MASTER_RING_CONFIG_SCRATCH_INDICES,
                DATA_MASTER_RING_CONFIG_G16X,
                DATA_MASTER_RING_CONFIG_G17P_G17X,
                DATA_MASTER_RING_CONFIG_G17G,
            ] {
                let validation =
                    validate_data_master_ring_config(gpu_gen, gpu_variant, recovered_layout);
                if recovered_layout == config {
                    assert_eq!(validation, Ok(config));
                } else {
                    assert_eq!(
                        validation,
                        Err(SubmissionConfigError::UnexpectedDataMasterRingConfig)
                    );
                }
            }

            let alternate_index_backing = if matches!(
                config.index_backing,
                DataMasterRingIndexBacking::FirmwareSharedData { .. }
            ) {
                DATA_MASTER_RING_SCRATCH_INDICES_CLEANUP
            } else {
                data_master_ring_shared_indices(0x548)
            };
            let alternate_entry_backing = if matches!(
                config.entry_backing,
                DataMasterRingEntryBacking::FirmwareSharedData { .. }
            ) {
                DATA_MASTER_RING_SCRATCH_ENTRIES
            } else {
                data_master_ring_shared_entries(0x310)
            };
            let alternate_host_index_mutation = match config.host_index_mutation {
                DataMasterRingHostIndexMutation::DirectSharedStores => {
                    DataMasterRingHostIndexMutation::LockedScratchWriteOnly
                }
                DataMasterRingHostIndexMutation::LockedScratchWriteOnly => {
                    DataMasterRingHostIndexMutation::DirectSharedStores
                }
            };
            for unexpected in [
                DataMasterRingConfig {
                    index_backing: alternate_index_backing,
                    ..config
                },
                DataMasterRingConfig {
                    entry_backing: alternate_entry_backing,
                    ..config
                },
                DataMasterRingConfig {
                    entries_cpu_object_offset: 0x20,
                    ..config
                },
                DataMasterRingConfig {
                    entries_firmware_object_offset: 0x28,
                    ..config
                },
                DataMasterRingConfig {
                    host_index_mutation: alternate_host_index_mutation,
                    ..config
                },
                DataMasterRingConfig {
                    shared_data_cacheability_names_proven: true,
                    ..config
                },
                DataMasterRingConfig {
                    firmware_consumer_ownership_proven: true,
                    ..config
                },
                DataMasterRingConfig {
                    get_read_index_vtable_offset: 0x28,
                    ..config
                },
                DataMasterRingConfig {
                    get_write_index_vtable_offset: 0x30,
                    ..config
                },
                DataMasterRingConfig {
                    set_write_index_vtable_offset: 0x18,
                    ..config
                },
                DataMasterRingConfig {
                    entry_stride: 0x20,
                    ..config
                },
                DataMasterRingConfig {
                    index_count: 0xff,
                    ..config
                },
                DataMasterRingConfig {
                    usable_entry_count: 0x100,
                    ..config
                },
                DataMasterRingConfig {
                    cold_init_ring_count: config.cold_init_ring_count + 1,
                    ..config
                },
                DataMasterRingConfig {
                    publication_steps: [
                        DataMasterRingPublicationStep::SelectNextEntry,
                        DataMasterRingPublicationStep::RecordEncoded,
                        DataMasterRingPublicationStep::ReadWriteIndex,
                        DataMasterRingPublicationStep::DmbIsh,
                        DataMasterRingPublicationStep::IncrementAndWrap8,
                        DataMasterRingPublicationStep::StoreWriteIndex,
                    ],
                    ..config
                },
            ] {
                assert_eq!(
                    validate_data_master_ring_config(gpu_gen, gpu_variant, unexpected),
                    Err(SubmissionConfigError::UnexpectedDataMasterRingConfig)
                );
            }

            assert_eq!(
                data_master_ring_next_entry(gpu_gen, gpu_variant, 0, 0),
                Ok(DataMasterRingNextEntry {
                    entry_offset_from_entries_base: 0,
                    published_write_index: 1,
                })
            );
            assert_eq!(
                data_master_ring_next_entry(gpu_gen, gpu_variant, 1, 0xff),
                Ok(DataMasterRingNextEntry {
                    entry_offset_from_entries_base: 0x17e8,
                    published_write_index: 0,
                })
            );
            assert_eq!(
                data_master_ring_next_entry(gpu_gen, gpu_variant, 0, 0xff),
                Err(SubmissionConfigError::DataMasterRingFull)
            );
            assert_eq!(
                data_master_ring_next_entry(gpu_gen, gpu_variant, 0x0b, 0x0a),
                Err(SubmissionConfigError::DataMasterRingFull)
            );
            assert_eq!(
                data_master_ring_next_entry(gpu_gen, gpu_variant, 0x100, 0),
                Err(SubmissionConfigError::DataMasterReadIndexOutOfRange)
            );
            assert_eq!(
                data_master_ring_next_entry(gpu_gen, gpu_variant, 0, 0x100),
                Err(SubmissionConfigError::DataMasterWriteIndexOutOfRange)
            );
        }

        for (gpu_gen, gpu_variant) in [
            (GpuGen::G15, GpuVariant::P),
            (GpuGen::G16, GpuVariant::P),
            (GpuGen::G17, GpuVariant::A),
        ] {
            assert_eq!(data_master_ring_config(gpu_gen, gpu_variant), None);
            assert_eq!(
                validate_data_master_ring_config(
                    gpu_gen,
                    gpu_variant,
                    DATA_MASTER_RING_CONFIG_G15G,
                ),
                Err(SubmissionConfigError::UnsupportedTarget)
            );
            assert_eq!(
                data_master_ring_next_entry(gpu_gen, gpu_variant, 0, 0),
                Err(SubmissionConfigError::UnsupportedTarget)
            );
        }

        assert!(GROUNDED_DATA_MASTER_RING_CONFIG_AVAILABLE);
        assert!(!EXECUTABLE_G15_G17_SUBMISSION_AVAILABLE);
    }

    #[test]
    fn data_master_firmware_table_and_class_priority_route_are_exact() {
        for (gpu_gen, gpu_variant, expected) in [
            (
                GpuGen::G15,
                GpuVariant::G,
                DATA_MASTER_FIRMWARE_TABLE_CONFIG_G15G,
            ),
            (
                GpuGen::G15,
                GpuVariant::S,
                DATA_MASTER_FIRMWARE_TABLE_CONFIG_G15S,
            ),
            (
                GpuGen::G15,
                GpuVariant::C,
                DATA_MASTER_FIRMWARE_TABLE_CONFIG_G15C_D,
            ),
            (
                GpuGen::G15,
                GpuVariant::D,
                DATA_MASTER_FIRMWARE_TABLE_CONFIG_G15C_D,
            ),
            (
                GpuGen::G16,
                GpuVariant::G,
                DATA_MASTER_FIRMWARE_TABLE_CONFIG_G16G,
            ),
            (
                GpuGen::G16,
                GpuVariant::S,
                DATA_MASTER_FIRMWARE_TABLE_CONFIG_G16X,
            ),
            (
                GpuGen::G16,
                GpuVariant::C,
                DATA_MASTER_FIRMWARE_TABLE_CONFIG_G16X,
            ),
            (
                GpuGen::G17,
                GpuVariant::P,
                DATA_MASTER_FIRMWARE_TABLE_CONFIG_G17P,
            ),
            (
                GpuGen::G17,
                GpuVariant::G,
                DATA_MASTER_FIRMWARE_TABLE_CONFIG_G17G,
            ),
            (
                GpuGen::G17,
                GpuVariant::S,
                DATA_MASTER_FIRMWARE_TABLE_CONFIG_G17X,
            ),
            (
                GpuGen::G17,
                GpuVariant::C,
                DATA_MASTER_FIRMWARE_TABLE_CONFIG_G17X,
            ),
        ] {
            let config = data_master_firmware_table_config(gpu_gen, gpu_variant)
                .expect("target should have a pinned firmware ring-table contract");
            assert_eq!(config, expected);
            assert_eq!(config.priority_count, 4);
            assert_eq!(config.data_master_count, 3);
            assert_eq!(config.descriptor_record_stride, 0x20);
            assert_eq!(config.read_index_firmware_pointer_offset, 0x00);
            assert_eq!(config.cfi_index_firmware_pointer_offset, 0x08);
            assert_eq!(config.write_index_firmware_pointer_offset, 0x10);
            assert_eq!(config.entries_firmware_pointer_offset, 0x18);
            assert!(!config.firmware_read_index_acknowledgment_writer_proven);
            assert!(!config.dual_stack_runtime_selector_proven);
            assert_eq!(
                validate_data_master_firmware_table_config(gpu_gen, gpu_variant, config),
                Ok(config)
            );

            let alternate_handoff = match config.index_handoff {
                DataMasterFirmwareIndexHandoff::MirrorReadAndCfiToWrite => {
                    DataMasterFirmwareIndexHandoff::NoHostReadOrCfiStore
                }
                DataMasterFirmwareIndexHandoff::NoHostReadOrCfiStore => {
                    DataMasterFirmwareIndexHandoff::MirrorReadAndCfiToWrite
                }
            };
            for unexpected in [
                DataMasterFirmwareTableConfig {
                    descriptor_table_cpu_pointer_object_offset: config
                        .descriptor_table_cpu_pointer_object_offset
                        + 8,
                    ..config
                },
                DataMasterFirmwareTableConfig {
                    ta_ring_object_base_offset: config.ta_ring_object_base_offset + 8,
                    ..config
                },
                DataMasterFirmwareTableConfig {
                    three_d_ring_object_base_offset: config.three_d_ring_object_base_offset + 8,
                    ..config
                },
                DataMasterFirmwareTableConfig {
                    cl_ring_object_base_offset: config.cl_ring_object_base_offset + 8,
                    ..config
                },
                DataMasterFirmwareTableConfig {
                    ring_object_stride: config.ring_object_stride + 8,
                    ..config
                },
                DataMasterFirmwareTableConfig {
                    priority_count: 5,
                    ..config
                },
                DataMasterFirmwareTableConfig {
                    data_master_count: 4,
                    ..config
                },
                DataMasterFirmwareTableConfig {
                    descriptor_record_stride: 0x28,
                    ..config
                },
                DataMasterFirmwareTableConfig {
                    read_index_firmware_pointer_offset: 0x08,
                    ..config
                },
                DataMasterFirmwareTableConfig {
                    cfi_index_firmware_pointer_offset: 0x10,
                    ..config
                },
                DataMasterFirmwareTableConfig {
                    write_index_firmware_pointer_offset: 0x18,
                    ..config
                },
                DataMasterFirmwareTableConfig {
                    entries_firmware_pointer_offset: 0x20,
                    ..config
                },
                DataMasterFirmwareTableConfig {
                    index_handoff: alternate_handoff,
                    ..config
                },
                DataMasterFirmwareTableConfig {
                    firmware_read_index_acknowledgment_writer_proven: true,
                    ..config
                },
                DataMasterFirmwareTableConfig {
                    dual_stack_runtime_selector_proven: true,
                    ..config
                },
            ] {
                assert_eq!(
                    validate_data_master_firmware_table_config(gpu_gen, gpu_variant, unexpected,),
                    Err(SubmissionConfigError::UnexpectedDataMasterFirmwareTableConfig)
                );
            }

            for (data_master, ordinal, base_offset) in [
                (DataMasterClass::Ta, 0, config.ta_ring_object_base_offset),
                (
                    DataMasterClass::ThreeD,
                    1,
                    config.three_d_ring_object_base_offset,
                ),
                (DataMasterClass::Cl, 2, config.cl_ring_object_base_offset),
            ] {
                for priority in 0..config.priority_count {
                    let record_offset = (priority as u16 * 3 + ordinal as u16) * 0x20;
                    assert_eq!(
                        data_master_firmware_ring_route(
                            gpu_gen,
                            gpu_variant,
                            data_master,
                            priority,
                        ),
                        Ok(DataMasterFirmwareRingRoute {
                            encoder_data_master_ordinal: ordinal,
                            ring_object_offset: base_offset
                                + priority as u16 * config.ring_object_stride as u16,
                            descriptor_record_offset: record_offset,
                            read_index_firmware_pointer_offset: record_offset,
                            cfi_index_firmware_pointer_offset: record_offset + 0x08,
                            write_index_firmware_pointer_offset: record_offset + 0x10,
                            entries_firmware_pointer_offset: record_offset + 0x18,
                        })
                    );
                }
                assert_eq!(
                    data_master_firmware_ring_route(
                        gpu_gen,
                        gpu_variant,
                        data_master,
                        config.priority_count,
                    ),
                    Err(SubmissionConfigError::DataMasterPriorityOutOfRange)
                );
            }
        }

        for (gpu_gen, gpu_variant) in [
            (GpuGen::G15, GpuVariant::P),
            (GpuGen::G16, GpuVariant::P),
            (GpuGen::G17, GpuVariant::A),
        ] {
            assert_eq!(
                data_master_firmware_table_config(gpu_gen, gpu_variant),
                None
            );
            assert_eq!(
                validate_data_master_firmware_table_config(
                    gpu_gen,
                    gpu_variant,
                    DATA_MASTER_FIRMWARE_TABLE_CONFIG_G15G,
                ),
                Err(SubmissionConfigError::UnsupportedTarget)
            );
            assert_eq!(
                data_master_firmware_ring_route(gpu_gen, gpu_variant, DataMasterClass::Ta, 0,),
                Err(SubmissionConfigError::UnsupportedTarget)
            );
        }

        assert!(GROUNDED_DATA_MASTER_FIRMWARE_TABLE_CONFIG_AVAILABLE);
        assert!(!EXECUTABLE_G15_G17_SUBMISSION_AVAILABLE);
    }

    #[test]
    fn data_master_ring_backing_and_host_ownership_are_target_pinned() {
        assert_eq!(
            DATA_MASTER_RING_SCRATCH_INDICES_CLEANUP,
            DataMasterRingIndexBacking::FenderScratchRam {
                scratch_object_offset: 0x28,
                read_cpu_pointer_offset: 0x30,
                cfi_cpu_pointer_offset: 0x38,
                write_cpu_pointer_offset: 0x40,
                read_firmware_pointer_offset: 0x48,
                cfi_firmware_pointer_offset: 0x50,
                write_firmware_pointer_offset: 0x58,
                entries_write_allocator_offset: 0x98,
                read_cfi_allocator_offset: 0xa0,
                index_allocation_size: 0x04,
                entries_write_mapping_options: 0x0400_0101,
                read_cfi_mapping_options: 0x0400_1101,
                cleanup_frees_allocations_proven: true,
            }
        );
        assert_eq!(
            DATA_MASTER_RING_SCRATCH_INDICES_NO_PROVEN_CLEANUP,
            DataMasterRingIndexBacking::FenderScratchRam {
                scratch_object_offset: 0x28,
                read_cpu_pointer_offset: 0x30,
                cfi_cpu_pointer_offset: 0x38,
                write_cpu_pointer_offset: 0x40,
                read_firmware_pointer_offset: 0x48,
                cfi_firmware_pointer_offset: 0x50,
                write_firmware_pointer_offset: 0x58,
                entries_write_allocator_offset: 0x98,
                read_cfi_allocator_offset: 0xa0,
                index_allocation_size: 0x04,
                entries_write_mapping_options: 0x0400_0101,
                read_cfi_mapping_options: 0x0400_1101,
                cleanup_frees_allocations_proven: false,
            }
        );
        assert_eq!(
            DATA_MASTER_RING_SCRATCH_ENTRIES,
            DataMasterRingEntryBacking::FenderScratchRam {
                allocator_offset: 0x98,
                allocation_size: 0x1800,
                mapping_options: 0x0400_0101,
                cleanup_frees_allocation_proven: true,
            }
        );

        fn assert_shared_indices(backing: DataMasterRingIndexBacking, owner_offset: u16) {
            assert_eq!(backing, data_master_ring_shared_indices(owner_offset));
            assert_eq!(
                backing,
                DataMasterRingIndexBacking::FirmwareSharedData {
                    resource_owner_object_offset: owner_offset,
                    cpu_object_offset: 0x08,
                    firmware_object_offset: 0x10,
                    read_index_offset: 0x00,
                    cfi_index_offset: 0x10,
                    write_index_offset: 0x20,
                    slice_size: 0x30,
                    caching_options: 0,
                    allocate_shared_data_arg: false,
                    batch_ring_count: 12,
                    release_zeroes_owner_slot: true,
                }
            );
        }

        fn assert_shared_entries(backing: DataMasterRingEntryBacking, owner_offset: u16) {
            assert_eq!(backing, data_master_ring_shared_entries(owner_offset));
            assert_eq!(
                backing,
                DataMasterRingEntryBacking::FirmwareSharedData {
                    resource_owner_object_offset: owner_offset,
                    slice_size: 0x1800,
                    caching_options: 1,
                    allocate_shared_data_arg: false,
                    batch_ring_count: 12,
                    release_zeroes_owner_slot: true,
                }
            );
        }

        let g15g = data_master_ring_config(GpuGen::G15, GpuVariant::G).unwrap();
        assert_shared_entries(g15g.entry_backing, 0x310);
        assert_shared_indices(g15g.index_backing, 0x548);
        assert_eq!(
            g15g.host_index_mutation,
            DataMasterRingHostIndexMutation::DirectSharedStores
        );

        for (gpu_gen, gpu_variant) in [
            (GpuGen::G15, GpuVariant::S),
            (GpuGen::G15, GpuVariant::C),
            (GpuGen::G15, GpuVariant::D),
        ] {
            let config = data_master_ring_config(gpu_gen, gpu_variant).unwrap();
            assert_eq!(
                config.index_backing,
                DATA_MASTER_RING_SCRATCH_INDICES_CLEANUP
            );
            assert_eq!(config.entry_backing, DATA_MASTER_RING_SCRATCH_ENTRIES);
            assert_eq!(
                config.host_index_mutation,
                DataMasterRingHostIndexMutation::LockedScratchWriteOnly
            );
        }

        let g16g = data_master_ring_config(GpuGen::G16, GpuVariant::G).unwrap();
        assert_eq!(
            g16g.index_backing,
            DATA_MASTER_RING_SCRATCH_INDICES_NO_PROVEN_CLEANUP
        );
        assert_shared_entries(g16g.entry_backing, 0x318);
        assert_eq!(
            g16g.host_index_mutation,
            DataMasterRingHostIndexMutation::LockedScratchWriteOnly
        );

        for (gpu_gen, gpu_variant, entry_owner, index_owner) in [
            (GpuGen::G16, GpuVariant::S),
            (GpuGen::G16, GpuVariant::C),
            (GpuGen::G17, GpuVariant::P),
            (GpuGen::G17, GpuVariant::G),
            (GpuGen::G17, GpuVariant::S),
            (GpuGen::G17, GpuVariant::C),
        ]
        .map(|(gpu_gen, gpu_variant)| {
            let owners = match (gpu_gen, gpu_variant) {
                (GpuGen::G17, GpuVariant::G) => (0x358, 0x590),
                _ => (0x350, 0x588),
            };
            (gpu_gen, gpu_variant, owners.0, owners.1)
        }) {
            let config = data_master_ring_config(gpu_gen, gpu_variant).unwrap();
            assert_shared_entries(config.entry_backing, entry_owner);
            assert_shared_indices(config.index_backing, index_owner);
            assert_eq!(
                config.host_index_mutation,
                DataMasterRingHostIndexMutation::DirectSharedStores
            );
            assert!(!config.shared_data_cacheability_names_proven);
            assert!(!config.firmware_consumer_ownership_proven);
        }

        assert!(!EXECUTABLE_G15_G17_SUBMISSION_AVAILABLE);
    }

    #[test]
    fn a18_pro_g17p_acceptance_matrix() {
        let g17p = id(0xa, 0, 1);
        assert_eq!(
            (g17p.gpu_gen, g17p.gpu_variant),
            (GpuGen::G17, GpuVariant::P)
        );
        assert_eq!(g17p.gpu_hal_generation, GpuHalGeneration::Hal200);
        assert_eq!(g17p.firmware_roles, FirmwareRoleTopology::Dual);
        assert_eq!(
            g17p.submission_transport,
            SubmissionTransport::ClassicAndSksm
        );
        assert_eq!(g17p.uat_input_address_bits, 42);
    }

    #[test]
    fn m5_g17g_acceptance_matrix() {
        let g17g = id(0xb, 2, 1);
        assert_eq!(
            (g17g.gpu_gen, g17g.gpu_variant),
            (GpuGen::G17, GpuVariant::G)
        );
        assert_eq!(g17g.gpu_hal_generation, GpuHalGeneration::Hal300);
        assert_eq!(g17g.firmware_roles, FirmwareRoleTopology::Dual);
        assert_eq!(g17g.submission_transport, SubmissionTransport::Sksm);
        assert_eq!(g17g.uat_input_address_bits, 42);
    }

    #[test]
    fn m5_pro_g17s_acceptance_matrix() {
        let g17s = id(0xb, 3, 1);
        assert_eq!(
            (g17s.gpu_gen, g17s.gpu_variant),
            (GpuGen::G17, GpuVariant::S)
        );
        assert_eq!(g17s.gpu_hal_generation, GpuHalGeneration::Hal300);
        assert_eq!(g17s.firmware_roles, FirmwareRoleTopology::Dual);
        assert_eq!(g17s.submission_transport, SubmissionTransport::Sksm);
        assert_eq!(g17s.uat_input_address_bits, 42);
    }

    #[test]
    fn unknown_families_and_variants_are_rejected() {
        assert!(decode_gpu_identity(0x9, 2, 1).is_none());
        assert!(decode_gpu_identity(0xb, 0, 1).is_none());
        assert!(decode_gpu_identity(0x7, 4, 0).is_none());
        assert!(decode_gpu_identity(0x7, 4, 3).is_none());
    }
}
