// SPDX-License-Identifier: GPL-2.0-only OR MIT

#![cfg_attr(not(test), allow(dead_code))]

//! T8140/G17P compute descriptor construction.
//!
//! The descriptor is the one-page selector-3 object executed by the CL path.
//! Every field here comes from the hardware-tested m1n1 compute builder. The
//! caller supplies queue-owned object addresses and command-owned UAPI values;
//! this module only validates and encodes them.

pub(crate) const COMPUTE_DESCRIPTOR_SIZE: usize = 0x4000;
pub(crate) const COMPUTE_REGISTER_COUNT: usize = 40;
pub(crate) const COMPUTE_REGISTER_CAPACITY: usize = 128;
pub(crate) const COMPUTE_REGISTER_SIZE: usize = 0x0c;
pub(crate) const COMPUTE_QUEUE_GRAPH_SIZE: usize = 0x8000;
pub(crate) const COMPUTE_QUEUE_CONTEXT_SIZE: usize = 0x4000;
pub(crate) const COMPUTE_QUEUE_CONTEXT_EXTENT: usize = 8 * COMPUTE_QUEUE_CONTEXT_SIZE;
pub(crate) const COMPUTE_OPTIONAL_SIZE: usize = 0xc0;
pub(crate) const COMPUTE_EVENT_SIZE: usize = 0x400;
pub(crate) const COMPUTE_CHANNEL_WRITE_INDEX: u32 = 3;
pub(crate) const COMPUTE_QUEUE_POINTERS: usize = 0x100;
const COMPUTE_QUEUE_CONTEXT_ITEM: usize = 0x200;
/// Same offset, exported so a repeat can refresh the item the first bind wrote.
pub(crate) const COMPUTE_QUEUE_CONTEXT_ITEM_OFFSET: usize = COMPUTE_QUEUE_CONTEXT_ITEM;
pub(crate) const COMPUTE_ITEM_RING: usize = 0x4000;
pub(crate) const COMPUTE_ITEM_RING_SIZE: usize = 0x500 * 8;
pub(crate) const COMPUTE_OPTIONAL: usize = 0x6800;
pub(crate) const COMPUTE_EVENT: usize = 0x6c00;
/// Storage for the tag-14 `KsmKickQueueAddKicks` channel command.
///
/// Native allocates this record from a firmware-owned pool
/// (`AGXFirmware::requestChannelCommandKSMKickQueueAddKicks`,
/// mac17p `0xfffffe0008a2d664`). Linux keeps one retained slot per queue graph,
/// placed after the tag-16 window exactly as tags 15 and 16 are placed after the
/// command-pointer ring. A000 `0xfffffc0000011710` cache-invalidates `0x18`
/// bytes of the record before reading it, so the reserved window is zeroed as a
/// whole even though the host writes only `0x15` bytes.
/// Records one steady-state submission publishes into the item ring:
/// the tag-3 command pointer, tag 15, tag 14, tag 16.
pub(crate) const COMPUTE_REPEAT_RING_RECORD_COUNT: usize = 4;
pub(crate) const COMPUTE_ADD_KICKS: usize = 0x7000;
pub(crate) const COMPUTE_ADD_KICKS_SIZE: usize = 0x40;
const _: () = assert!(COMPUTE_ITEM_RING + COMPUTE_ITEM_RING_SIZE <= COMPUTE_OPTIONAL);
const _: () = assert!(COMPUTE_OPTIONAL + COMPUTE_OPTIONAL_SIZE <= COMPUTE_EVENT);
const _: () = assert!(COMPUTE_EVENT + COMPUTE_EVENT_SIZE <= COMPUTE_ADD_KICKS);
const _: () = assert!(COMPUTE_ADD_KICKS + COMPUTE_ADD_KICKS_SIZE <= COMPUTE_QUEUE_GRAPH_SIZE);
const _: () = assert!(COMPUTE_REGISTER_COUNT <= COMPUTE_REGISTER_CAPACITY);

const COMPUTE_SELECTOR: u32 = 3;
const COMPUTE_REGISTER_START: usize = 0x40;
const COMPUTE_SECONDARY_REGISTER_START: usize = 0x760;
const COMPUTE_PRIMARY_COUNT: usize = 0x748;
const COMPUTE_ENCODER_PARAMS: usize = 0xf14;
const COMPUTE_SAMPLER_ARRAY: usize = COMPUTE_ENCODER_PARAMS + 0x18;
const COMPUTE_SAMPLER_COUNT: usize = COMPUTE_ENCODER_PARAMS + 0x20;
const COMPUTE_SAMPLER_MAX: usize = COMPUTE_ENCODER_PARAMS + 0x24;

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) enum ComputeBuildError {
    BufferTooSmall,
    AddressOverflow,
    CdmTerminatorBeforeStream,
    SubmissionIndexZero,
    QueueSubmissionZero,
    InvalidSksmEntryAliases,
    SamplerCountOutOfRange,
    SamplerPair,
    SamplerAlignment,
}

/// Low and high GPU aliases of one retained SKSM entry backing.
///
/// Native tag-15 ConfigUpdate reads these from the same KSM queue object at
/// `+0x80/+0x88`. Keeping them typed prevents the classic CL2 queue-context
/// allocation from being substituted for either address.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) struct ComputeSksmEntryAliases {
    low: u64,
    high: u64,
    extent: usize,
}

impl ComputeSksmEntryAliases {
    pub(crate) const fn new(
        low: u64,
        high: u64,
        extent: usize,
        alignment: u64,
    ) -> Result<Self, ComputeBuildError> {
        if low == 0
            || high == 0
            || low == high
            || extent == 0
            || alignment == 0
            || alignment & (alignment - 1) != 0
            || low & (alignment - 1) != 0
            || high & (alignment - 1) != 0
            || low.checked_add(extent as u64).is_none()
            || high.checked_add(extent as u64).is_none()
        {
            return Err(ComputeBuildError::InvalidSksmEntryAliases);
        }
        Ok(Self { low, high, extent })
    }

    pub(crate) const fn low(self) -> u64 {
        self.low
    }

    pub(crate) const fn high(self) -> u64 {
        self.high
    }

    pub(crate) const fn extent(self) -> usize {
        self.extent
    }

    /// Return corresponding addresses within both aliases of the one backing.
    pub(crate) const fn range_at(
        self,
        offset: usize,
        size: usize,
    ) -> Result<[u64; 2], ComputeBuildError> {
        let end = match offset.checked_add(size) {
            Some(end) => end,
            None => return Err(ComputeBuildError::InvalidSksmEntryAliases),
        };
        if end > self.extent {
            return Err(ComputeBuildError::InvalidSksmEntryAliases);
        }
        Ok([self.low + offset as u64, self.high + offset as u64])
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) struct ComputeRegister {
    pub(crate) number: u32,
    pub(crate) value: u64,
}

impl ComputeRegister {
    const fn new(number: u32, value: u64) -> Self {
        Self { number, value }
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) struct ComputeRegisterParameters {
    pub(crate) preempt_base: u64,
    pub(crate) cdm_base: u64,
    pub(crate) usc_exec_base: u64,
    pub(crate) helper_binary: u64,
    pub(crate) helper_data: u64,
    pub(crate) helper_config: u64,
    pub(crate) dispatch_identity: u64,
    pub(crate) context_id: u32,
    pub(crate) work_ordinal: u32,
    pub(crate) robustness: u64,
    pub(crate) operand_state_base: u64,
    pub(crate) execution_gate: u64,
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) struct ComputeDescriptorObjects {
    pub(crate) scheduler_record: u64,
    pub(crate) descriptor_low_alias: u64,
    pub(crate) dispatch_a: u64,
    pub(crate) dispatch_b: u64,
    pub(crate) status_a: u64,
    pub(crate) status_b: u64,
    pub(crate) shared_control: u64,
    pub(crate) zero_page: u64,
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) struct ComputeDescriptorMetadata {
    pub(crate) submit_sequence: u64,
    pub(crate) context_id: u32,
    pub(crate) grid_index: u32,
    pub(crate) work_ordinal: u32,
    pub(crate) queue_submission: u32,
    pub(crate) queue_ordinal: u32,
    pub(crate) submission_index: u32,
    pub(crate) support_control: u32,
    pub(crate) support_flags: u32,
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) struct ComputeDescriptorCommand {
    pub(crate) cdm_terminator: u64,
    pub(crate) sampler_array: u64,
    pub(crate) sampler_count: u32,
    pub(crate) user_timestamp_start: u64,
    pub(crate) user_timestamp_end: u64,
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) struct ComputeQueueGraphAddresses {
    pub(crate) queue_record: u64,
    pub(crate) pointers: u64,
    pub(crate) job_list: u64,
    pub(crate) item_ring: u64,
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) struct ComputeChannelGroupAddresses {
    pub(crate) queue: u64,
    pub(crate) optional: u64,
    pub(crate) event: u64,
    pub(crate) write_index: u32,
}

fn add(address: u64, offset: u64) -> Result<u64, ComputeBuildError> {
    address
        .checked_add(offset)
        .ok_or(ComputeBuildError::AddressOverflow)
}

/// Build the complete 40-write compute register program.
pub(crate) fn build_compute_registers(
    p: ComputeRegisterParameters,
) -> Result<[ComputeRegister; COMPUTE_REGISTER_COUNT], ComputeBuildError> {
    let context_word = ((p.context_id as u64) << 8) | p.work_ordinal as u64;

    Ok([
        ComputeRegister::new(0x1a510, p.preempt_base),
        ComputeRegister::new(0x1a420, p.cdm_base),
        ComputeRegister::new(0x1a4d0, add(p.preempt_base, 0x1480)?),
        ComputeRegister::new(0x1a4d8, add(p.preempt_base, 0x1488)?),
        ComputeRegister::new(0x1a4e0, add(p.preempt_base, 0x1490)?),
        ComputeRegister::new(0x1a4e8, add(p.preempt_base, 0x1498)?),
        ComputeRegister::new(0x10071, p.usc_exec_base),
        ComputeRegister::new(0x11841, p.helper_binary),
        ComputeRegister::new(0x11849, p.helper_data),
        ComputeRegister::new(0x11f81, p.helper_config),
        ComputeRegister::new(0x1a440, 0x0000_0001_5402_4201),
        ComputeRegister::new(0x1a458, 0x0000_0000_10c0_8860),
        ComputeRegister::new(0x101d9, 0x1c),
        ComputeRegister::new(0x1a089, 0),
        ComputeRegister::new(0x1a091, 0),
        ComputeRegister::new(0x1a059, 0),
        ComputeRegister::new(0x1a061, 0),
        ComputeRegister::new(0x1a0b9, 0),
        ComputeRegister::new(0x1a0c1, 0),
        ComputeRegister::new(0x101d1, 0),
        ComputeRegister::new(0x0d479, 0),
        ComputeRegister::new(0x1a0e9, 8),
        ComputeRegister::new(0x107a1, 0xff_0000),
        ComputeRegister::new(0x0a599, 0x0000_0132_0040_0020),
        ComputeRegister::new(0x0d411, 0x0000_0002_0000_0001),
        ComputeRegister::new(0x1a540, p.dispatch_identity),
        ComputeRegister::new(0x014a9, p.dispatch_identity),
        ComputeRegister::new(0x0a351, p.dispatch_identity),
        ComputeRegister::new(0x10201, context_word),
        ComputeRegister::new(0x10428, context_word),
        ComputeRegister::new(0x14028, p.execution_gate),
        ComputeRegister::new(0x14070, p.robustness | 1),
        ComputeRegister::new(0x10229, add(p.operand_state_base, 0x12800)?),
        ComputeRegister::new(0x140a8, add(p.operand_state_base, 0x13000)?),
        ComputeRegister::new(0x10099, add(p.operand_state_base, 0x9405)?),
        ComputeRegister::new(0x10091, add(p.operand_state_base, 0x12400)?),
        ComputeRegister::new(0x0a5c1, add(p.operand_state_base, 0x0005)?),
        ComputeRegister::new(0x0a5c9, add(p.operand_state_base, 0x9000)?),
        ComputeRegister::new(0x1a440, 0x0000_0001_5402_4209),
        ComputeRegister::new(0x0a599, 0x0000_0060_0040_0020),
    ])
}

fn put_u8(raw: &mut [u8], offset: usize, value: u8) {
    raw[offset] = value;
}

fn put_u16(raw: &mut [u8], offset: usize, value: u16) {
    raw[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
}

fn put_u32(raw: &mut [u8], offset: usize, value: u32) {
    raw[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

fn put_u64(raw: &mut [u8], offset: usize, value: u64) {
    raw[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}

/// Build the retained queue record named by every compute CL entry.
pub(crate) fn build_compute_queue_graph(
    base: u64,
    channel_control: u64,
    raw: &mut [u8],
) -> Result<ComputeQueueGraphAddresses, ComputeBuildError> {
    if raw.len() < COMPUTE_QUEUE_GRAPH_SIZE {
        return Err(ComputeBuildError::BufferTooSmall);
    }
    let addresses = ComputeQueueGraphAddresses {
        queue_record: base,
        pointers: add(base, 0x100)?,
        job_list: add(base, 0x200)?,
        item_ring: add(base, 0x4000)?,
    };
    let raw = &mut raw[..COMPUTE_QUEUE_GRAPH_SIZE];
    raw.fill(0);
    put_u64(raw, 0x00, addresses.pointers);
    put_u64(raw, 0x08, addresses.item_ring);
    put_u64(raw, 0x10, addresses.job_list);
    put_u32(raw, 0x24, u32::MAX);
    put_u32(raw, 0x28, 2);
    put_u32(raw, 0x2c, 2);
    raw[0x36..0x38].fill(0xff);
    put_u32(raw, 0x40, 2);
    put_u32(raw, 0x44, u32::MAX);
    put_u32(raw, 0x48, 0x1aa);
    put_u64(raw, 0x9c, channel_control);
    put_u32(raw, 0x100 + 0x50, u32::MAX);
    put_u32(raw, 0x100 + 0x60, compute_item_ring_entries());
    put_u64(raw, 0x200 + 0x08, addresses.job_list);
    Ok(addresses)
}

/// Usable entries in the queue's item ring, as programmed into the graph's
/// pointer block at `+0x160` and read back by both the host publisher and the
/// firmware.
///
/// Normally the full `0x500`. `g17p_ring_limit` shrinks it so the ring WRAP can
/// be reached in a handful of submissions instead of 320: at four records per
/// submission, a 32-entry ring wraps on the eighth. Wrap behaviour is identical
/// at any size, so this turns a 400-iteration run -- which also overflows the
/// kernel log and destroys its own evidence -- into a 20-iteration one.
pub(crate) fn compute_item_ring_entries() -> u32 {
    const FULL: u32 = 0x500;
    let requested = *crate::module_parameters::g17p_ring_limit.value();
    if requested == 0 {
        return FULL;
    }
    // Must stay a multiple of the per-submission record count so a batch never
    // straddles the wrap differently than it would at full size, and must leave
    // room for at least two submissions plus the always-empty slot.
    let clamped = requested.clamp(
        2 * COMPUTE_REPEAT_RING_RECORD_COUNT as u32 + 4,
        FULL,
    );
    clamped - (clamped % COMPUTE_REPEAT_RING_RECORD_COUNT as u32)
}

/// Encode the tag-15 `KSMKickQueueConfigUpdate` record inside a retained
/// queue graph.
///
/// Split out of the first-bind builder because macOS republishes this record
/// on EVERY submission -- both live captures (QID 0x11 and QID 0x16) are of
/// already-installed queues and both still carry a tag-15 ConfigUpdate. The
/// only field that differs between a first bind and a repeat is the
/// queue-install flag at `+0x1a`. Nothing outside the optional-record window
/// is touched, so a repeat submission can call this without resetting the
/// item-ring producer or rewriting the queue header the way the first-bind
/// builder does.
pub(crate) fn encode_compute_config_update(
    graph: &mut [u8],
    sksm_entries: ComputeSksmEntryAliases,
    shared_control: u64,
    channel_control: u64,
    queue_id: u8,
    install_queue: bool,
) -> Result<(), ComputeBuildError> {
    let flushid_fix = *crate::module_parameters::g17p_flushid_fix.value() != 0;
    let (flushid_object, flushid_slot, flushid_index) = if flushid_fix {
        (0u64, 0xffffu16, 0xffffu16)
    } else {
        (shared_control, 0x3fu16, 0u16)
    };
    let optional_raw = graph
        .get_mut(COMPUTE_OPTIONAL..COMPUTE_OPTIONAL + COMPUTE_OPTIONAL_SIZE)
        .ok_or(ComputeBuildError::BufferTooSmall)?;
    optional_raw.fill(0);
    put_u32(optional_raw, 0x00, 0x0f);
    put_u64(optional_raw, 0x08, sksm_entries.low());
    put_u64(optional_raw, 0x10, sksm_entries.high());
    for (offset, value) in [
        (0x18, u16::from(queue_id)),
        // +0x1a is the QUEUE-INSTALL flag, and it must be non-zero on a first
        // bind. Firmware RE (2026-08-31): the only writer of the per-queue slot
        // `0x127628 + qid*0x28` is at 0x119b8, and that store -- together with
        // slot[+0x00]=1, the per-data-master active bitmap, the sgx+0x21008
        // queue-base write and the 0x40 KTrace receipt -- is reachable only when
        // this field is non-zero. Sending 0 left slot[+0x08] NULL, and the tag-16
        // handler then faulted dereferencing it (firmware exception pc=0x1190c
        // far=0x340 esr=0x96000004, a level-0 translation fault), which is why
        // pause bit 7 taken at 0x118c0 was never cleared at 0x11948.
        //
        // The two captured macOS records show 0 here because both were taken on
        // already-installed queues, not on a first bind. Absence of the 0x40
        // KTrace code is the receipt that the install branch was not taken.
        // Queue-install flag: 1 only on a queue's FIRST bind. Native clears it
        // afterwards -- leaving it set forces a disable/reprogram/enable cycle
        // on the KSMFE queue for every submission, which is why a second submit
        // published fine and then never completed.
        (0x1a, if install_queue { 1 } else { 0 }),
        (0x1e, 2),
        (0x22, 2),
        (0x32, 2),
        (0x3e, flushid_slot),
        (0x46, flushid_index),
        (0x52, 1),
        (0x56, 1),
        (0x5a, 0x170),
        (0x5e, 2),
        (0x62, 1),
        (0x66, 1),
    ] {
        put_u16(optional_raw, offset, value);
    }
    put_u64(optional_raw, 0x36, flushid_object);
    put_u64(optional_raw, 0x4a, channel_control);
    optional_raw[0x76..0x86].fill(0xff);
    Ok(())
}

/// Encode one queue-context item.
///
/// The array is a sibling of the SKSM entry array, not a single record:
/// `COMPUTE_QUEUE_CONTEXT_EXTENT` is `8 * 0x4000 = 0x20000`, the item stride is
/// `0x200`, so it holds `0x100` items -- exactly the queue geometry's
/// `stamp_count`, and exactly the size and stride of the entry backing
/// (`0x100 * 0x200 = 0x20000`). The first bind writes item 1 and consumes stamp
/// index 1, which is consistent with the item being indexed by stamp.
///
/// That reading is INFERRED from the geometry, not proven, which is why the
/// repeat path selects between "rewrite item 1" and "write item[stamp]" by
/// parameter instead of assuming.
pub(crate) fn encode_compute_queue_context_item(
    queue_context: &mut [u8],
    item_index: usize,
    descriptor: u64,
    queue_base: u64,
    queue_id: u8,
) -> Result<(), ComputeBuildError> {
    let offset = item_index
        .checked_mul(COMPUTE_QUEUE_CONTEXT_ITEM)
        .ok_or(ComputeBuildError::AddressOverflow)?;
    let end = offset
        .checked_add(COMPUTE_QUEUE_CONTEXT_ITEM)
        .ok_or(ComputeBuildError::AddressOverflow)?;
    if end > queue_context.len() {
        return Err(ComputeBuildError::BufferTooSmall);
    }
    let context = &mut queue_context[offset..end];
    context.fill(0);
    put_u64(context, 0x00, 0x1000_1000_0000_0004);
    put_u64(context, 0x10, descriptor);
    put_u64(context, 0x18, queue_base);
    put_u64(context, 0x20, 0xffff_0801_0000_0001);
    put_u64(context, 0x28, u64::from(queue_id) << 40);
    put_u64(context, 0x130, 0);
    put_u64(context, 0x138, 4);
    put_u64(context, 0x150, 0x0001_1003_8001_a002);
    put_u64(context, 0x158, 0x0000_2003_8001_a03b);
    put_u64(context, 0x178, 0x003f_ffff_ffff_ffff);
    Ok(())
}

/// Stage the first hardware-proven direct-compute group in a retained queue.
///
/// This is the CL_2 channel graph used by the output-positive m1n1 add3 test:
/// selector-3 descriptor, selector-0x0f optional record, selector-0x0e event,
/// followed by queue write index 3. The caller publishes the outer channel
/// slot and its producer only after this function returns.
pub(crate) fn build_initial_compute_channel_group(
    base: u64,
    descriptor: u64,
    sksm_entries: ComputeSksmEntryAliases,
    shared_control: u64,
    channel_control: u64,
    queue_id: u8,
    graph: &mut [u8],
    queue_context: &mut [u8],
    install_queue: bool,
) -> Result<ComputeChannelGroupAddresses, ComputeBuildError> {
    if graph.len() < COMPUTE_QUEUE_GRAPH_SIZE
        || queue_context.len() < COMPUTE_QUEUE_CONTEXT_SIZE
    {
        return Err(ComputeBuildError::BufferTooSmall);
    }

    let optional = add(base, COMPUTE_OPTIONAL as u64)?;
    let event = add(base, COMPUTE_EVENT as u64)?;

    // The initial channel queue has UUID 0x170. The retained SKSM fixture
    // uses 0x1aa, so set this only when the proven channel path is staged.
    put_u32(graph, 0x48, 0x170);
    for (index, address) in [descriptor, optional, event].into_iter().enumerate() {
        put_u64(graph, COMPUTE_ITEM_RING + index * 8, address);
    }
    put_u32(
        graph,
        COMPUTE_QUEUE_POINTERS + 0x40,
        COMPUTE_CHANNEL_WRITE_INDEX,
    );

    // Tag-15 FlushID triple. Ours (shared_control / 0x3f / 1) is inferred, and
    // because +0x36 is non-zero the firmware runs its FlushID bind path, which
    // indexes `0x5b778 + rec[0x46]*0xe8` unbounded. The kext writes 0 / -1 / -1
    // for a non-first bind, which makes the firmware skip the block, and
    // PROVENANCE.md records that T8140 leaves the AGXSKSMFlushID bit clear.
    // The non-fix tuple's third element lands at +0x46. Native writes 0 there:
    // both compute-queue tag-15 records captured live on macOS 26.6.2 (QID 0x11
    // and QID 0x16) have +0x46 = 0, so it is a constant. The +0x3e slot is NOT a
    // constant -- native carries a live per-queue counter there (0x719 / 0x770,
    // copied from AGXKSMKickQueueG16 +0x28), so 0x3f stays until that counter's
    // provenance is known. Do not substitute the kext's 0/-1/-1 non-first-bind
    // constants; those have hard-killed the machine twice.
    encode_compute_config_update(
        graph,
        sksm_entries,
        shared_control,
        channel_control,
        queue_id,
        install_queue,
    )?;

    let event_raw = &mut graph[COMPUTE_EVENT..COMPUTE_EVENT + COMPUTE_EVENT_SIZE];
    event_raw.fill(0);
    put_u32(event_raw, 0x00, 0x0e);
    put_u32(event_raw, 0x04, (1u32 << 16) | u32::from(queue_id));
    put_u32(event_raw, 0x08, 0x0102);
    put_u32(event_raw, 0x10, 0x0200);

    queue_context[..COMPUTE_QUEUE_CONTEXT_SIZE].fill(0);
    // Item index 1, which is also the stamp index the first bind consumes
    // (the geometry seeds `current_timestamp` at 1). See
    // `encode_compute_queue_context_item` for why that coincidence matters.
    encode_compute_queue_context_item(queue_context, 1, descriptor, base, queue_id)?;

    Ok(ComputeChannelGroupAddresses {
        queue: base,
        optional,
        event,
        write_index: COMPUTE_CHANNEL_WRITE_INDEX,
    })
}

fn encode_registers(raw: &mut [u8], offset: usize, registers: &[ComputeRegister]) {
    for (index, register) in registers.iter().enumerate() {
        let entry = offset + index * COMPUTE_REGISTER_SIZE;
        put_u32(raw, entry, register.number);
        put_u64(raw, entry + 4, register.value);
    }
}

fn register_value(registers: &[ComputeRegister], number: u32, occurrence: usize) -> u64 {
    registers
        .iter()
        .filter(|register| register.number == number)
        .nth(occurrence)
        .expect("fixed G17P register program lost a required register")
        .value
}

/// Encode a selector-3 descriptor without dropping sampler, timestamp, or CDM
/// command state.
pub(crate) fn build_compute_descriptor(
    registers: &[ComputeRegister; COMPUTE_REGISTER_COUNT],
    objects: ComputeDescriptorObjects,
    metadata: ComputeDescriptorMetadata,
    command: ComputeDescriptorCommand,
    raw: &mut [u8],
) -> Result<(), ComputeBuildError> {
    if raw.len() < COMPUTE_DESCRIPTOR_SIZE {
        return Err(ComputeBuildError::BufferTooSmall);
    }
    if command.cdm_terminator < register_value(registers, 0x1a420, 0) {
        return Err(ComputeBuildError::CdmTerminatorBeforeStream);
    }
    if metadata.submission_index == 0 {
        return Err(ComputeBuildError::SubmissionIndexZero);
    }
    if metadata.queue_submission == 0 {
        return Err(ComputeBuildError::QueueSubmissionZero);
    }
    if command.sampler_count == u32::MAX {
        return Err(ComputeBuildError::SamplerCountOutOfRange);
    }
    if (command.sampler_array == 0) != (command.sampler_count == 0) {
        return Err(ComputeBuildError::SamplerPair);
    }
    if command.sampler_array & 7 != 0 {
        return Err(ComputeBuildError::SamplerAlignment);
    }

    let raw = &mut raw[..COMPUTE_DESCRIPTOR_SIZE];
    raw.fill(0);
    put_u32(raw, 0x00, COMPUTE_SELECTOR);
    put_u64(raw, 0x04, metadata.submit_sequence);
    put_u32(raw, 0x0c, metadata.context_id);
    put_u64(raw, 0x10, objects.scheduler_record);
    for (index, value) in [0x22u16, 0x23, 0x23, 0x24].into_iter().enumerate() {
        put_u16(raw, 0x18 + index * 2, value);
    }

    encode_registers(raw, COMPUTE_REGISTER_START, registers);
    put_u64(
        raw,
        0x740,
        add(objects.descriptor_low_alias, COMPUTE_REGISTER_START as u64)?,
    );
    put_u32(
        raw,
        COMPUTE_PRIMARY_COUNT,
        ((COMPUTE_REGISTER_COUNT * COMPUTE_REGISTER_SIZE) as u32) << 16
            | COMPUTE_REGISTER_COUNT as u32,
    );

    let secondary = [
        ComputeRegister::new(0x10099, register_value(registers, 0x0a5c1, 0)),
        ComputeRegister::new(0x10091, register_value(registers, 0x0a5c9, 0)),
        ComputeRegister::new(0x0a5c1, register_value(registers, 0x10099, 0)),
        ComputeRegister::new(0x0a5c9, register_value(registers, 0x10091, 0)),
    ];
    encode_registers(raw, COMPUTE_SECONDARY_REGISTER_START, &secondary);
    put_u64(
        raw,
        0x0e60,
        add(
            objects.descriptor_low_alias,
            COMPUTE_SECONDARY_REGISTER_START as u64,
        )?,
    );
    put_u32(raw, 0x0e68, 0x0030_0004);
    put_u32(raw, 0x0f28, u32::MAX);

    put_u64(raw, 0x0ed8, register_value(registers, 0x1a510, 0));
    put_u64(raw, 0x0ee0, command.cdm_terminator);
    put_u64(raw, 0x0f08, register_value(registers, 0x1a440, 0));
    put_u32(
        raw,
        0x0f20,
        (register_value(registers, 0x1a540, 0) >> 32) as u32,
    );
    put_u64(raw, COMPUTE_SAMPLER_ARRAY, command.sampler_array);
    put_u32(raw, COMPUTE_SAMPLER_COUNT, command.sampler_count);
    put_u32(
        raw,
        COMPUTE_SAMPLER_MAX,
        if command.sampler_count == 0 {
            0
        } else {
            command.sampler_count + 1
        },
    );
    put_u64(raw, 0x0f40, objects.dispatch_a);
    put_u64(raw, 0x0f48, objects.dispatch_b);
    put_u32(raw, 0x0f50, metadata.queue_submission << 8);
    put_u32(raw, 0x0f54, metadata.grid_index);
    put_u32(raw, 0x0f58, metadata.queue_ordinal);
    put_u32(raw, 0x0f60, metadata.submission_index);
    put_u64(
        raw,
        0x0f68,
        register_value(registers, 0x1a540, 0) & 0xffff_ffff,
    );
    put_u32(raw, 0x0f70, metadata.work_ordinal);
    put_u64(raw, 0x0f7c, objects.status_a);
    put_u64(raw, 0x0f84, objects.status_b);
    put_u64(raw, 0x0f8c, command.user_timestamp_start);
    put_u64(raw, 0x0f94, command.user_timestamp_end);

    put_u16(raw, 0x0fb0, 0x001a);
    put_u64(raw, 0x0fb2, objects.shared_control);
    put_u32(raw, 0x0fba, metadata.support_control);
    put_u32(raw, 0x0fbe, metadata.support_flags);
    put_u8(raw, 0x0fc5, 0x9f);
    put_u32(raw, 0x0fc8, (metadata.work_ordinal & 3) << 30);
    put_u64(raw, 0x0fcb, objects.zero_page);
    put_u8(raw, 0x0fd3, 1);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fnv1a64(raw: &[u8]) -> u64 {
        let mut value = 0xcbf2_9ce4_8422_2325u64;
        for byte in raw {
            value ^= *byte as u64;
            value = value.wrapping_mul(0x0000_0100_0000_01b3);
        }
        value
    }

    fn register_parameters() -> ComputeRegisterParameters {
        ComputeRegisterParameters {
            preempt_base: 0x100_0404_0000,
            cdm_base: 0x100_0410_0000,
            usc_exec_base: 0x1_0000_0000,
            helper_binary: 0,
            helper_data: 0,
            helper_config: 0,
            dispatch_identity: 0x1234_5678_0000_0001,
            context_id: 3,
            work_ordinal: 7,
            robustness: 0x100_0430_0000,
            operand_state_base: 0x70_0022_8000,
            execution_gate: 2,
        }
    }

    #[test]
    fn register_program_matches_the_hardware_tested_m1n1_builder() {
        let registers = build_compute_registers(register_parameters()).unwrap();
        let mut raw = [0u8; COMPUTE_REGISTER_COUNT * COMPUTE_REGISTER_SIZE];
        encode_registers(&mut raw, 0, &registers);
        assert_eq!(fnv1a64(&raw), 0xc2e9_a727_fcf7_d740);
        assert_eq!(registers[0], ComputeRegister::new(0x1a510, 0x100_0404_0000));
        assert_eq!(registers[39], ComputeRegister::new(0x0a599, 0x6000_4000_20));
    }

    #[test]
    fn descriptor_preserves_cdm_sampler_timestamp_and_queue_state() {
        let registers = build_compute_registers(register_parameters()).unwrap();
        let mut raw = [0u8; COMPUTE_DESCRIPTOR_SIZE];
        build_compute_descriptor(
            &registers,
            ComputeDescriptorObjects {
                scheduler_record: 0xffff_fc20_c099_8100,
                descriptor_low_alias: 0x70_0034_0000,
                dispatch_a: 0xffff_fc20_001c_8028,
                dispatch_b: 0xffff_fc20_c07c_0028,
                status_a: 0xffff_fc20_0002_4c68,
                status_b: 0xffff_fc20_0002_4c70,
                shared_control: 0xffff_fc20_c095_0000,
                zero_page: 0xffff_fc20_0171_0000,
            },
            ComputeDescriptorMetadata {
                submit_sequence: 5,
                context_id: 3,
                grid_index: 2,
                work_ordinal: 7,
                queue_submission: 8,
                queue_ordinal: 7,
                submission_index: 9,
                support_control: 0xe0a0_0001,
                support_flags: 0,
            },
            ComputeDescriptorCommand {
                cdm_terminator: 0x100_0410_002c,
                sampler_array: 0x100_0800_0000,
                sampler_count: 4,
                user_timestamp_start: 0x200_0000_4000,
                user_timestamp_end: 0x200_0000_4008,
            },
            &mut raw,
        )
        .unwrap();

        assert_eq!(fnv1a64(&raw), 0x9a59_8076_3ca3_6a2a);
        assert_eq!(u64::from_le_bytes(raw[0x0ee0..0x0ee8].try_into().unwrap()), 0x100_0410_002c);
        assert_eq!(u64::from_le_bytes(raw[COMPUTE_SAMPLER_ARRAY..COMPUTE_SAMPLER_ARRAY + 8].try_into().unwrap()), 0x100_0800_0000);
        assert_eq!(u32::from_le_bytes(raw[COMPUTE_SAMPLER_COUNT..COMPUTE_SAMPLER_COUNT + 4].try_into().unwrap()), 4);
        assert_eq!(u64::from_le_bytes(raw[0x0f8c..0x0f94].try_into().unwrap()), 0x200_0000_4000);
        assert_eq!(u32::from_le_bytes(raw[0x0f50..0x0f54].try_into().unwrap()), 8 << 8);
    }

    #[test]
    fn invalid_command_relations_are_caught_before_encoding() {
        let registers = build_compute_registers(register_parameters()).unwrap();
        let objects = ComputeDescriptorObjects {
            scheduler_record: 1,
            descriptor_low_alias: 0x70_0000_0000,
            dispatch_a: 2,
            dispatch_b: 3,
            status_a: 4,
            status_b: 5,
            shared_control: 6,
            zero_page: 7,
        };
        let metadata = ComputeDescriptorMetadata {
            submit_sequence: 1,
            context_id: 3,
            grid_index: 2,
            work_ordinal: 0,
            queue_submission: 1,
            queue_ordinal: 0,
            submission_index: 1,
            support_control: 0xe0a0_0001,
            support_flags: 0,
        };
        let mut raw = [0u8; COMPUTE_DESCRIPTOR_SIZE];
        assert_eq!(
            build_compute_descriptor(
                &registers,
                objects,
                metadata,
                ComputeDescriptorCommand {
                    cdm_terminator: register_parameters().cdm_base - 4,
                    sampler_array: 0,
                    sampler_count: 0,
                    user_timestamp_start: 0,
                    user_timestamp_end: 0,
                },
                &mut raw,
            ),
            Err(ComputeBuildError::CdmTerminatorBeforeStream)
        );
        assert_eq!(
            build_compute_descriptor(
                &registers,
                objects,
                metadata,
                ComputeDescriptorCommand {
                    cdm_terminator: register_parameters().cdm_base,
                    sampler_array: 0,
                    sampler_count: 1,
                    user_timestamp_start: 0,
                    user_timestamp_end: 0,
                },
                &mut raw,
            ),
            Err(ComputeBuildError::SamplerPair)
        );
    }

    #[test]
    fn queue_graph_matches_the_hardware_tested_m1n1_builder() {
        let mut raw = [0u8; COMPUTE_QUEUE_GRAPH_SIZE];
        let addresses = build_compute_queue_graph(
            0xffff_fc20_c100_0000,
            0xffff_fc20_c200_0000,
            &mut raw,
        )
        .unwrap();
        assert_eq!(
            addresses,
            ComputeQueueGraphAddresses {
                queue_record: 0xffff_fc20_c100_0000,
                pointers: 0xffff_fc20_c100_0100,
                job_list: 0xffff_fc20_c100_0200,
                item_ring: 0xffff_fc20_c100_4000,
            }
        );
        assert_eq!(u32::from_le_bytes(raw[0x48..0x4c].try_into().unwrap()), 0x1aa);
        assert_eq!(fnv1a64(&raw), 0x3490_ef31_add0_ac88);
    }

    #[test]
    fn initial_channel_group_matches_the_output_positive_add3_builder() {
        let base = 0xffff_fc20_c100_0000;
        let descriptor = 0xffff_fc20_c200_0000;
        let mut graph = [0u8; COMPUTE_QUEUE_GRAPH_SIZE];
        build_compute_queue_graph(base, 0xffff_fc20_c500_0000, &mut graph).unwrap();
        let mut context = vec![0u8; COMPUTE_QUEUE_CONTEXT_EXTENT];
        let addresses = build_initial_compute_channel_group(
            base,
            descriptor,
            ComputeSksmEntryAliases::new(
                0x70_0040_0000,
                0xffff_fc20_c300_0000,
                0x8000,
                0x100,
            )
            .unwrap(),
            0xffff_fc20_c400_0000,
            0xffff_fc20_c500_0000,
            4,
            &mut graph,
            &mut context,
            true,
        )
        .unwrap();
        assert_eq!(
            addresses,
            ComputeChannelGroupAddresses {
                queue: base,
                optional: base + 0x6800,
                event: base + 0x6c00,
                write_index: 3,
            }
        );
        assert_eq!(fnv1a64(&graph), 0xc8d6_b367_6415_8da7);
        assert_eq!(fnv1a64(&context), 0x5ba5_b355_8c90_0783);
    }

    #[test]
    fn sksm_entry_aliases_preserve_one_backing_offset() {
        const EXTENT: usize = 0x8000;
        let aliases = ComputeSksmEntryAliases::new(
            0x70_004d_8000,
            0xffff_fc20_0027_8000,
            EXTENT,
            0x100,
        )
        .unwrap();
        assert_eq!(
            aliases.range_at(0x200, 0x200),
            Ok([0x70_004d_8200, 0xffff_fc20_0027_8200])
        );
        assert_eq!(
            aliases.range_at(EXTENT - 0x200, 0x200),
            Ok([0x70_004d_fe00, 0xffff_fc20_0027_fe00])
        );
        assert_eq!(
            aliases.range_at(EXTENT, 1),
            Err(ComputeBuildError::InvalidSksmEntryAliases)
        );
        assert_eq!(
            ComputeSksmEntryAliases::new(0x70_004d_8000, 0x70_004d_8000, EXTENT, 0x100),
            Err(ComputeBuildError::InvalidSksmEntryAliases)
        );
        assert_eq!(
            ComputeSksmEntryAliases::new(
                0x70_004d_8001,
                0xffff_fc20_0027_8000,
                EXTENT,
                0x100,
            ),
            Err(ComputeBuildError::InvalidSksmEntryAliases)
        );
    }
}
