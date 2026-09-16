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
pub(crate) const COMPUTE_ITEM_RING: usize = 0x4000;
pub(crate) const COMPUTE_OPTIONAL: usize = 0x5000;
pub(crate) const COMPUTE_EVENT: usize = 0x5400;
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
    SamplerCountOutOfRange,
    SamplerPair,
    SamplerAlignment,
    ScsRequestOutOfRange,
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

/// The three register values that carry a compute kick's ray-traversal (SCS)
/// scratch request.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) struct ScsRequestRegisters {
    /// 0x101d1: pages in the low half, MiB in the high half.
    pub(crate) size_word: u64,
    /// 0xd479: the request's "extras" count.
    pub(crate) extras: u64,
    /// 0x107a1: window and enable, or the disabled constant.
    pub(crate) window_word: u64,
}

/// The MiB half of the SCS request, derived from the page count exactly as
/// AGXMetalG17P derives it in `ComputeContext::endComputePass`
/// (0x216ac1f5c..0x216ac1f6c: `pages << 2`, `+ 0x3ff`, `>> 10`).
fn scs_request_mib(pages: u32) -> u32 {
    ((pages as u64 + 0xff) >> 8) as u32
}

/// Encode an SCS scratch request the way the kext's register builder does
/// (generateCLRegisterListG16Common): the page and MiB counts share 0x101d1,
/// the extras count is its own register, and 0x107a1 either carries the window
/// with the enable bit or reads back the disabled constant 0x00ff_0000.
///
/// `mib` of zero means "derive it the way Metal does", which is what a faithful
/// request wants; pass a non-zero value only to sweep the two halves of 0x101d1
/// independently.  A request of zero pages reproduces the register program this
/// driver has always emitted, so the default path is unchanged.
pub(crate) fn scs_request_registers(
    pages: u32,
    mib: u32,
    extras: u32,
    window: u32,
) -> Result<ScsRequestRegisters, ComputeBuildError> {
    if pages == 0 {
        return Ok(ScsRequestRegisters {
            size_word: 0,
            extras: 0,
            window_word: 0xff_0000,
        });
    }
    // Keep the page and MiB halves distinct, and the window below bit 24's
    // enable flag. Reject unrepresentable diagnostic inputs before publishing
    // any register program instead of overlapping fields or truncating them.
    if pages > 0xffff || mib > 0xffff || window > 0xff {
        return Err(ComputeBuildError::ScsRequestOutOfRange);
    }
    let mib = if mib == 0 { scs_request_mib(pages) } else { mib };
    Ok(ScsRequestRegisters {
        size_word: (pages as u64) | ((mib as u64) << 16),
        extras: extras as u64,
        window_word: ((window as u64) << 16) | 0x0100_0000,
    })
}

/// Build the complete 40-write compute register program.
pub(crate) fn build_compute_registers(
    p: ComputeRegisterParameters,
) -> Result<[ComputeRegister; COMPUTE_REGISTER_COUNT], ComputeBuildError> {
    let context_word = ((p.context_id as u64) << 8) | p.work_ordinal as u64;
    let scs = scs_request_registers(
        *crate::module_parameters::g17p_compute_scs_pages.value(),
        *crate::module_parameters::g17p_compute_scs_mib.value(),
        *crate::module_parameters::g17p_compute_scs_extras.value(),
        *crate::module_parameters::g17p_compute_scs_window.value(),
    )?;

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
        ComputeRegister::new(0x101d1, scs.size_word),
        ComputeRegister::new(0x0d479, scs.extras),
        ComputeRegister::new(0x1a0e9, 8),
        ComputeRegister::new(0x107a1, scs.window_word),
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
    put_u32(raw, 0x100 + 0x60, 0x500);
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
    hardware_buffer_id: u8,
    install_queue: bool,
    context_id: Option<u16>,
) -> Result<(), ComputeBuildError> {
    // Record +0x32 is the hardware context this queue's work is declared to run
    // in. It has always been the literal 2, from when every compute client ran
    // in the fixed pair `T8140_COMPUTE_CTXS = [2, 3]`.
    //
    // Those two contexts are a SINGLE GLOBAL alias pair:
    // `install_t8140_compute_context_alias` publishes the current client's VM
    // root into both and records one `t8140_alias_owner`. With one compute
    // client that is correct. With a second, the alias is re-pointed and the
    // loser's kicks -- still declaring 2 -- no longer name their own root,
    // while their real root lives in the app context the execution context
    // allocated (5..62). The kick already declares that real context at
    // `sgx+0x10428`; only this record still says 2.
    //
    // Probe-gated and default-off: 0 sends the historical constant, byte
    // identical to the previous driver; 1 sends the client's real context when
    // one is known. See `g17p_compute_config_context`.
    let declared_context = match context_id {
        Some(context) if *crate::module_parameters::g17p_compute_config_context.value() != 0 => {
            context
        }
        _ => 2u16,
    };
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
        (0x32, declared_context),
        (0x3e, flushid_slot),
        (0x46, flushid_index),
        (0x52, 1),
        // AGXCLChannelSKSM::submitBuffer copies AGXQOSQueue +0x10 here.
        // Native retained CL queues receive distinct HardwareBufferIDs in
        // allocation order (1, 2, 3 in the captured QID 4/5/7 sequence).
        // This field is therefore queue identity, not the literal 1 from the
        // first output-positive queue.
        (0x56, u16::from(hardware_buffer_id)),
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
    context_low: u64,
    context_high: u64,
    shared_control: u64,
    channel_control: u64,
    queue_id: u8,
    hardware_buffer_id: u8,
    graph: &mut [u8],
    queue_context: &mut [u8],
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
        hardware_buffer_id,
        install_queue,
        context_id,
    )?;

    let event_raw = &mut graph[COMPUTE_EVENT..COMPUTE_EVENT + COMPUTE_EVENT_SIZE];
    event_raw.fill(0);
    put_u32(event_raw, 0x00, 0x0e);
    put_u32(event_raw, 0x04, 0x0001_0004);
    put_u32(event_raw, 0x08, 0x0102);
    put_u32(event_raw, 0x10, 0x0200);

    queue_context[..COMPUTE_QUEUE_CONTEXT_SIZE].fill(0);
    let context = &mut queue_context
        [COMPUTE_QUEUE_CONTEXT_ITEM..COMPUTE_QUEUE_CONTEXT_ITEM + 0x200];
    put_u64(context, 0x00, 0x1000_1000_0000_0004);
    put_u64(context, 0x10, descriptor);
    put_u64(context, 0x18, base);
    put_u64(context, 0x20, 0xffff_0801_0000_0001);
    put_u64(context, 0x28, 4u64 << 40);
    put_u64(context, 0x130, 0);
    put_u64(context, 0x138, 4);
    put_u64(context, 0x150, 0x0001_1003_8001_a002);
    put_u64(context, 0x158, 0x0000_2003_8001_a03b);
    put_u64(context, 0x178, 0x003f_ffff_ffff_ffff);

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
    fn an_empty_scs_request_reproduces_the_historical_registers() {
        // Zero pages is the kext's "this pass asks for no ray-traversal
        // scratch" branch, and is what this driver emitted before the request
        // became settable.  register_program_matches_the_hardware_tested_m1n1_builder
        // above pins the same thing through the whole program's hash.
        let scs = scs_request_registers(0, 0, 0, 0).unwrap();
        assert_eq!(scs.size_word, 0);
        assert_eq!(scs.extras, 0);
        assert_eq!(scs.window_word, 0xff_0000);
        // The window is ignored while no pages are requested.
        assert_eq!(scs_request_registers(0, 7, 3, 9), scs_request_registers(0, 7, 3, 0));
    }

    #[test]
    fn a_nonempty_scs_request_packs_pages_mib_extras_and_window() {
        // 89 pages is the four-section floor (0x58000 bytes) plus the guard
        // page AGXMetalG17P adds; one MiB covers it ((89 * 4 + 0x3ff) >> 10).
        let scs = scs_request_registers(89, 1, 0, 0).unwrap();
        assert_eq!(scs.size_word, 0x0001_0059);
        assert_eq!(scs.extras, 0);
        assert_eq!(scs.window_word, 0x0100_0000);

        let wide = scs_request_registers(815, 4, 2, 0x20).unwrap();
        assert_eq!(wide.size_word, 0x0004_032f);
        assert_eq!(wide.extras, 2);
        assert_eq!(wide.window_word, 0x0120_0000);
    }

    #[test]
    fn an_unset_mib_is_derived_from_the_page_count_like_metal_does() {
        // AGXMetalG17P computes MiB as (pages * 4 + 0x3ff) >> 10 and never
        // leaves it zero next to a non-zero page count, so the driver must not
        // either: an operator who sets only the page count still gets the
        // register value a real ray-tracing kick carries.
        // 0x59 pages = the four fixed sections plus the guard page.
        assert_eq!(scs_request_registers(0x59, 0, 0, 0).unwrap().size_word, 0x0001_0059);
        // 0x32f pages = the intersector<triangle_data> layout
        // (traversal depth 0x70, driver payload 0x48, 96 cliques).
        assert_eq!(scs_request_registers(0x32f, 0, 0, 0).unwrap().size_word, 0x0004_032f);
        // 0x3c8 pages = the intersection_query<> layout (driver payload 0x114).
        assert_eq!(scs_request_registers(0x3c8, 0, 0, 0).unwrap().size_word, 0x0004_03c8);
        // 0x6a7 pages = intersection_query<instancing,triangle_data> on a
        // top-level structure, the Vulkan rayQueryEXT shape
        // (traversal depth 0xe8, driver payload 0x128).
        assert_eq!(scs_request_registers(0x6a7, 0, 0, 0).unwrap().size_word, 0x0007_06a7);
        // An explicit value still wins, so the halves stay separable.
        assert_eq!(scs_request_registers(0x59, 9, 0, 0).unwrap().size_word, 0x0009_0059);
        // Zero pages keeps the historical register exactly.
        assert_eq!(scs_request_registers(0, 0, 0, 0).unwrap().size_word, 0);
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
            0x70_0040_0000,
            0xffff_fc20_c300_0000,
            0xffff_fc20_c400_0000,
            0xffff_fc20_c500_0000,
            4,
            1,
            &mut graph,
            &mut context,
        )
        .unwrap();
        assert_eq!(
            addresses,
            ComputeChannelGroupAddresses {
                queue: base,
                optional: base + 0x5000,
                event: base + 0x5400,
                write_index: 3,
            }
        );
        // The original add3 fixture used FlushID index 1. The retained-queue
        // builder switched that index to 0 in fcc1b7dfc8847. Check that field
        // explicitly, then normalize only it to retain the original whole-
        // graph golden instead of silently replacing the historical hash.
        assert_eq!(&graph[COMPUTE_OPTIONAL + 0x46..COMPUTE_OPTIONAL + 0x48], &[0, 0]);
        put_u16(&mut graph, COMPUTE_OPTIONAL + 0x46, 1);
        assert_eq!(fnv1a64(&graph), 0xc8d6_b367_6415_8da7);
        assert_eq!(fnv1a64(&context), 0x5ba5_b355_8c90_0783);
    }
}
