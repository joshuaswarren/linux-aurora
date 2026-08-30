// SPDX-License-Identifier: GPL-2.0-only OR MIT

#![cfg_attr(not(test), allow(dead_code))]

//! Pure G17P completion and firmware-report decoding.
//!
//! Channels 13 and 14 are 0x40-byte firmware-produced service rings with
//! split host/firmware cursors. Channel 13 retains the event record shapes
//! used by m1n1 for fault, flag, timeout, growth, and channel errors. Channel
//! 14 has a distinct five-record sequence captured repeatedly on J700; its
//! fields stay structural here. The SKSM decoder follows the A18 Pro firmware
//! bit extraction. Normal direct CL drains descriptor ordinal 0 on D93AP IRQ
//! bit 21 even though its queue configuration carries completion selector 2.
//! The ordinary EP0x20 type-1 event ring then signals the correlated stamp.
//! Queue
//! ownership follows the m1n1 hardware result: read reaching a published
//! prefix means accepted, done reaching it means firmware retired the queue
//! entries, and physical reuse requires done/read/write equality. Render
//! completion also checks the scheduler's job list and current-job timestamps.
//! J700 run `20260828-g17p-opening-job-record-live27` retired both queue
//! prefixes at 3/3/3 while the list still held `c0828100..c0828198` and all
//! eight execution timestamps stayed zero.

pub(crate) const REPORT_ENTRY_SIZE: usize = 0x40;
pub(crate) const REPORT_CHANNEL_EVENT: u8 = 13;
pub(crate) const REPORT_CHANNEL_TRACE: u8 = 14;
pub(crate) const G17P_COMPUTE_COMPLETION_SELECTOR: u8 = 2;
pub(crate) const KSM_COMPLETION_ENTRY_SIZE: usize = 0x40;
pub(crate) const D93AP_G17P_COMPLETION_IRQ: u32 = 1001;
pub(crate) const G17P_NORMAL_CL_COMPLETION_IRQ_BIT: u8 = 21;
pub(crate) const G17P_NORMAL_CL_COMPLETION_ORDINAL: u8 = 0;
pub(crate) const G17P_NORMAL_CL_COMPLETION_CAPACITY: u32 = 0x800;
pub(crate) const G17P_NORMAL_CL_COMPLETION_DESCRIPTOR_TABLE_OFFSET: u32 = 0x2614;
pub(crate) const G17P_NORMAL_CL_COMPLETION_DESCRIPTOR_STRIDE: u32 = 0x20;
pub(crate) const G17P_NORMAL_CL_COMPLETION_PROCESSED_OFFSET: u32 = 0x1c;
pub(crate) const G17P_KSM_COMPLETION_NOTIFY_BASE: u32 = 0x21168;
pub(crate) const G17P_KSM_COMPLETION_ACK_BASE: u32 = 0x21188;
pub(crate) const G17P_KSM_COMPLETION_MMIO_STRIDE: u32 = 8;
pub(crate) const G17P_FIRMWARE_EVENT_ENDPOINT: u8 = 0x20;
pub(crate) const G17P_FIRMWARE_EVENT_MESSAGE: u64 = 0x0042_0000_0000_0000;
pub(crate) const G17P_FIRMWARE_EVENT_HOST_TYPE: u8 = 2;
pub(crate) const G17P_FIRMWARE_EVENT_HOST_TYPE_MASK: u8 = 0x3f;
pub(crate) const G17P_FIRMWARE_EVENT_ENTRY_SIZE: usize = 0x48;
pub(crate) const G17P_FIRMWARE_EVENT_CAPACITY: u32 = 0x100;
pub(crate) const G17P_FIRMWARE_EVENT_CONSUMER_OFFSET: u32 = 0;
pub(crate) const G17P_FIRMWARE_EVENT_PRODUCER_OFFSET: u32 = 0x20;
pub(crate) const D93AP_G17P_SGX_SOURCE_INDEX: u8 = 4;
pub(crate) const D93AP_G17P_SGX_INTERRUPTS_VALID: u32 = 0xdf;

const REPORT_CURSOR_MASK: u32 = 0xff;
const KSM_TIMESTAMP_MASK: u64 = 0x00ff_ffff_ffff;
const KSM_QID_MASK: u8 = 0x7f;
const KSM_PARENT_MASK: u64 = 0x003f_ffff_ffff_ffff;

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) enum CompletionDecodeError {
    RecordSize,
    ChannelNumber,
    EventType,
    TraceShape,
    CompletionSelector,
    CompletionCommandType,
    CompletionCapacity,
    CompletionCursor,
    CursorRange,
    QueueCounterOrder,
    QueueCounterRegression,
    PublishedPrefixBeyondWrite,
    CompletionIrqSource,
    CompletionOrdinal,
    CompletionNotificationCount,
    CompletionQueueId,
    CompletionTimestamp,
    CompletionTimestampOrder,
    FirmwareEventType,
    FirmwareEventDiagnosticCount,
    FirmwareEventStampIndex,
    FirmwareEventStampMissing,
    CompletionOutstanding,
    CompletionRouteUnavailable,
    FirmwareEventEndpoint,
    FirmwareEventMessage,
    FirmwareEventCursor,
    FirmwareEventEmpty,
}

fn get_u16(raw: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes(raw[offset..offset + 2].try_into().unwrap())
}

fn get_u32(raw: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(raw[offset..offset + 4].try_into().unwrap())
}

fn get_i32(raw: &[u8], offset: usize) -> i32 {
    i32::from_le_bytes(raw[offset..offset + 4].try_into().unwrap())
}

fn get_u64(raw: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(raw[offset..offset + 8].try_into().unwrap())
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) struct SplitReportCursor {
    pub(crate) host: u8,
    pub(crate) firmware: u8,
}

impl SplitReportCursor {
    pub(crate) fn from_words(host: u32, firmware: u32) -> Result<Self, CompletionDecodeError> {
        if host & !REPORT_CURSOR_MASK != 0 || firmware & !REPORT_CURSOR_MASK != 0 {
            return Err(CompletionDecodeError::CursorRange);
        }
        Ok(Self {
            host: host as u8,
            firmware: firmware as u8,
        })
    }

    pub(crate) const fn pending(self) -> u8 {
        self.firmware.wrapping_sub(self.host)
    }

    pub(crate) const fn acknowledgement(self) -> u32 {
        self.firmware as u32
    }
}

/// The three words visible through a captured channel-table entry.
///
/// The two split cursor peers live at `state[0] + 0x20` and
/// `state[2] + 0x20`. `state[1]` is independent. Hardware captures expose the
/// produced record count in that middle word, so this helper names only that
/// observed snapshot property. Runtime acknowledgement uses
/// [`SplitReportCursor`] instead.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) struct CapturedReportCounters {
    pub(crate) state: [u32; 3],
}

impl CapturedReportCounters {
    pub(crate) const fn visible_record_count(self) -> u32 {
        let first = if self.state[0] > self.state[1] {
            self.state[0]
        } else {
            self.state[1]
        };
        if first > self.state[2] {
            first
        } else {
            self.state[2]
        }
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) struct Channel13FaultRecord {
    pub(crate) payload: [u8; 0x34],
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) struct Channel13FlagRecord {
    pub(crate) firing: [u64; 2],
    pub(crate) value_14: u16,
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) struct Channel13TimeoutRecord {
    pub(crate) counter: u64,
    pub(crate) stamp_index: i32,
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) struct Channel13GrowthRecord {
    pub(crate) vm_id: u32,
    pub(crate) buffer_manager_id: u32,
    pub(crate) counter: u32,
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) enum Channel13EventRecord {
    Fault(Channel13FaultRecord),
    Flag(Channel13FlagRecord),
    Timeout(Channel13TimeoutRecord),
    GrowTilingBuffer(Channel13GrowthRecord),
    ChannelError,
}

pub(crate) fn decode_channel13_event(
    channel: u8,
    raw: &[u8],
) -> Result<Channel13EventRecord, CompletionDecodeError> {
    if channel != REPORT_CHANNEL_EVENT {
        return Err(CompletionDecodeError::ChannelNumber);
    }
    if raw.len() != REPORT_ENTRY_SIZE {
        return Err(CompletionDecodeError::RecordSize);
    }
    match get_u32(raw, 0) {
        0 => {
            let mut payload = [0u8; 0x34];
            payload.copy_from_slice(&raw[4..0x38]);
            Ok(Channel13EventRecord::Fault(Channel13FaultRecord {
                payload,
            }))
        }
        1 => Ok(Channel13EventRecord::Flag(Channel13FlagRecord {
            firing: [get_u64(raw, 4), get_u64(raw, 12)],
            value_14: get_u16(raw, 0x14),
        })),
        4 => Ok(Channel13EventRecord::Timeout(Channel13TimeoutRecord {
            counter: get_u64(raw, 4),
            stamp_index: get_i32(raw, 12),
        })),
        7 => Ok(Channel13EventRecord::GrowTilingBuffer(
            Channel13GrowthRecord {
                vm_id: get_u32(raw, 4),
                buffer_manager_id: get_u32(raw, 8),
                counter: get_u32(raw, 12),
            },
        )),
        8 => Ok(Channel13EventRecord::ChannelError),
        _ => Err(CompletionDecodeError::EventType),
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) enum Channel14TraceRecord {
    Marker11At00 {
        tick: u64,
    },
    Marker19At08 {
        tick: u64,
        state_28: u32,
        value_34: u32,
    },
    Marker1cAt10 {
        tick: u64,
        state_1c: u32,
    },
    Marker19At18 {
        tick: u64,
        prior_tick: u64,
        state_2c: u32,
        state_38: u32,
    },
    Marker1cAt20 {
        value_04: u64,
        tick: u64,
        state_2c: u32,
    },
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) enum FirmwareReportRecord {
    Event(Channel13EventRecord),
    Trace(Channel14TraceRecord),
}

pub(crate) fn decode_firmware_report(
    channel: u8,
    raw: &[u8],
) -> Result<FirmwareReportRecord, CompletionDecodeError> {
    match channel {
        REPORT_CHANNEL_EVENT => Ok(FirmwareReportRecord::Event(decode_channel13_event(
            channel, raw,
        )?)),
        REPORT_CHANNEL_TRACE => Ok(FirmwareReportRecord::Trace(decode_channel14_trace(
            channel, raw,
        )?)),
        _ => Err(CompletionDecodeError::ChannelNumber),
    }
}

pub(crate) fn decode_channel14_trace(
    channel: u8,
    raw: &[u8],
) -> Result<Channel14TraceRecord, CompletionDecodeError> {
    if channel != REPORT_CHANNEL_TRACE {
        return Err(CompletionDecodeError::ChannelNumber);
    }
    if raw.len() != REPORT_ENTRY_SIZE {
        return Err(CompletionDecodeError::RecordSize);
    }

    if get_u32(raw, 0x00) == 0x11 {
        return Ok(Channel14TraceRecord::Marker11At00 {
            tick: get_u64(raw, 0x04),
        });
    }
    if get_u32(raw, 0x08) == 0x19 {
        return Ok(Channel14TraceRecord::Marker19At08 {
            tick: get_u64(raw, 0x0c),
            state_28: get_u32(raw, 0x28),
            value_34: get_u32(raw, 0x34),
        });
    }
    if get_u32(raw, 0x10) == 0x1c {
        return Ok(Channel14TraceRecord::Marker1cAt10 {
            tick: get_u64(raw, 0x14),
            state_1c: get_u32(raw, 0x1c),
        });
    }
    if get_u32(raw, 0x18) == 0x19 {
        return Ok(Channel14TraceRecord::Marker19At18 {
            tick: get_u64(raw, 0x1c),
            prior_tick: get_u64(raw, 0x24),
            state_2c: get_u32(raw, 0x2c),
            state_38: get_u32(raw, 0x38),
        });
    }
    if get_u32(raw, 0x20) == 0x1c {
        return Ok(Channel14TraceRecord::Marker1cAt20 {
            value_04: get_u64(raw, 0x04),
            tick: get_u64(raw, 0x24),
            state_2c: get_u32(raw, 0x2c),
        });
    }
    Err(CompletionDecodeError::TraceShape)
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) enum KsmCompletionCommandType {
    Type1,
    Type2,
    Type3,
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) struct G17pKsmCompletionEntry {
    pub(crate) completion_selector: u8,
    pub(crate) command_type: KsmCompletionCommandType,
    pub(crate) queue_id: u8,
    pub(crate) timestamp: u64,
    pub(crate) word_08: u64,
    pub(crate) payload: [u64; 2],
    pub(crate) parents: [u64; 2],
    pub(crate) tagged_object: u64,
    pub(crate) object: u64,
    pub(crate) metadata: u64,
}

pub(crate) fn decode_ksm_completion_entry(
    completion_selector: u8,
    raw: &[u8],
) -> Result<G17pKsmCompletionEntry, CompletionDecodeError> {
    if completion_selector > 4 {
        return Err(CompletionDecodeError::CompletionSelector);
    }
    if raw.len() != KSM_COMPLETION_ENTRY_SIZE {
        return Err(CompletionDecodeError::RecordSize);
    }

    let control = get_u64(raw, 0x00);
    let command_type = match (control >> 4) & 0x3 {
        1 => KsmCompletionCommandType::Type1,
        2 => KsmCompletionCommandType::Type2,
        3 => KsmCompletionCommandType::Type3,
        _ => return Err(CompletionDecodeError::CompletionCommandType),
    };
    let timestamp = ((control >> 8) & 0xffff_ffff_00) | ((control >> 48) & 0xff);
    let tagged_object = get_u64(raw, 0x30);
    Ok(G17pKsmCompletionEntry {
        completion_selector,
        command_type,
        queue_id: ((control >> 9) as u8) & KSM_QID_MASK,
        timestamp: timestamp & KSM_TIMESTAMP_MASK,
        word_08: get_u64(raw, 0x08),
        payload: [get_u64(raw, 0x10), get_u64(raw, 0x18)],
        parents: [
            get_u64(raw, 0x20) & KSM_PARENT_MASK,
            get_u64(raw, 0x28) & KSM_PARENT_MASK,
        ],
        tagged_object,
        object: tagged_object & !1,
        metadata: get_u64(raw, 0x38),
    })
}

pub(crate) fn decode_compute_completion_entry(
    raw: &[u8],
) -> Result<G17pKsmCompletionEntry, CompletionDecodeError> {
    decode_ksm_completion_entry(G17P_COMPUTE_COMPLETION_SELECTOR, raw)
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) struct KsmCompletionCursor {
    pub(crate) capacity: u32,
    pub(crate) processed: u32,
}

impl KsmCompletionCursor {
    pub(crate) fn entry_offset(self) -> Result<u64, CompletionDecodeError> {
        if self.capacity == 0 {
            return Err(CompletionDecodeError::CompletionCapacity);
        }
        if self.processed >= self.capacity {
            return Err(CompletionDecodeError::CompletionCursor);
        }
        Ok(self.processed as u64 * KSM_COMPLETION_ENTRY_SIZE as u64)
    }

    pub(crate) fn advance(self) -> Result<Self, CompletionDecodeError> {
        self.entry_offset()?;
        Ok(Self {
            capacity: self.capacity,
            processed: (self.processed + 1) % self.capacity,
        })
    }
}

/// Owner of one normal-CL completion transport operation.
///
/// Source bit 21 and the `+0x21168/+0x21188` notification pair are consumed
/// by GFX firmware. They are not a Linux interrupt-status/acknowledgement
/// pair. Linux first participates when the firmware publishes an ordinary
/// event-ring record and reports it through AIC source 4 or the equivalent
/// RTKit notification.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) enum G17PCompletionOwner {
    GfxFirmware,
    LinuxPlatform,
}

/// Exact firmware-owned descriptor/MMIO drain for normal CL.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) struct G17PFirmwareCompletionDrain {
    pub(crate) source_bit: u8,
    pub(crate) descriptor_table_offset: u32,
    pub(crate) descriptor_stride: u32,
    pub(crate) descriptor_ordinal: u8,
    pub(crate) processed_offset: u32,
    pub(crate) notification_offset: u32,
    pub(crate) acknowledgement_offset: u32,
    pub(crate) owner: G17PCompletionOwner,
}

pub(crate) const G17P_FIRMWARE_COMPLETION_DRAIN: G17PFirmwareCompletionDrain =
    G17PFirmwareCompletionDrain {
        source_bit: G17P_NORMAL_CL_COMPLETION_IRQ_BIT,
        descriptor_table_offset: G17P_NORMAL_CL_COMPLETION_DESCRIPTOR_TABLE_OFFSET,
        descriptor_stride: G17P_NORMAL_CL_COMPLETION_DESCRIPTOR_STRIDE,
        descriptor_ordinal: G17P_NORMAL_CL_COMPLETION_ORDINAL,
        processed_offset: G17P_NORMAL_CL_COMPLETION_PROCESSED_OFFSET,
        notification_offset: G17P_KSM_COMPLETION_NOTIFY_BASE,
        acknowledgement_offset: G17P_KSM_COMPLETION_ACK_BASE,
        owner: G17PCompletionOwner::GfxFirmware,
    };

/// Exact D93AP host ingress facts and the current Linux ownership state.
///
/// The pinned ADT selects raw SGX source 4 (valid in mask `0xdf`) and AIC
/// hardware interrupt 1001. The checked-in T8140 Linux GPU node exposes no
/// SGX interrupt resource, so 1001 must not be passed directly to Linux's IRQ
/// registration API: that API expects a DT-translated Linux virtual IRQ. The
/// retained RTKit client does receive endpoint 0x20 and can demultiplex the
/// exact type-2 notification, but no role-local event-ring pointer graph is
/// mapped yet.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) struct G17PNormalClCompletionRoute {
    pub(crate) physical_aic_irq: u32,
    pub(crate) sgx_source_index: u8,
    pub(crate) sgx_interrupts_valid: u32,
    pub(crate) firmware_event_endpoint: u8,
    pub(crate) firmware_event_message: u64,
    pub(crate) firmware_completion_owner: G17PCompletionOwner,
    pub(crate) linux_gpu_dt_irq_resource: bool,
    pub(crate) physical_irq_registered: bool,
    pub(crate) rtkit_notification_demultiplexed: bool,
    pub(crate) firmware_event_ring_source_mapped: bool,
    pub(crate) firmware_event_shared_indices_mapped: bool,
    pub(crate) firmware_event_entries_mapped: bool,
    pub(crate) firmware_event_consumer_writer_available: bool,
}

pub(crate) const G17P_NORMAL_CL_COMPLETION_ROUTE: G17PNormalClCompletionRoute =
    G17PNormalClCompletionRoute {
        physical_aic_irq: D93AP_G17P_COMPLETION_IRQ,
        sgx_source_index: D93AP_G17P_SGX_SOURCE_INDEX,
        sgx_interrupts_valid: D93AP_G17P_SGX_INTERRUPTS_VALID,
        firmware_event_endpoint: G17P_FIRMWARE_EVENT_ENDPOINT,
        firmware_event_message: G17P_FIRMWARE_EVENT_MESSAGE,
        firmware_completion_owner: G17PCompletionOwner::GfxFirmware,
        linux_gpu_dt_irq_resource: false,
        physical_irq_registered: false,
        rtkit_notification_demultiplexed: true,
        firmware_event_ring_source_mapped: false,
        firmware_event_shared_indices_mapped: false,
        firmware_event_entries_mapped: false,
        firmware_event_consumer_writer_available: false,
    };

pub(crate) const fn require_g17p_normal_cl_completion_route(
    route: G17PNormalClCompletionRoute,
) -> Result<(), CompletionDecodeError> {
    let physical_ingress = route.linux_gpu_dt_irq_resource && route.physical_irq_registered;
    if route.physical_aic_irq == D93AP_G17P_COMPLETION_IRQ
        && route.sgx_source_index == D93AP_G17P_SGX_SOURCE_INDEX
        && route.sgx_interrupts_valid == D93AP_G17P_SGX_INTERRUPTS_VALID
        && route.firmware_event_endpoint == G17P_FIRMWARE_EVENT_ENDPOINT
        && route.firmware_event_message == G17P_FIRMWARE_EVENT_MESSAGE
        && matches!(
            route.firmware_completion_owner,
            G17PCompletionOwner::GfxFirmware
        )
        && (physical_ingress || route.rtkit_notification_demultiplexed)
        && route.firmware_event_ring_source_mapped
        && route.firmware_event_shared_indices_mapped
        && route.firmware_event_entries_mapped
        && route.firmware_event_consumer_writer_available
    {
        Ok(())
    } else {
        Err(CompletionDecodeError::CompletionRouteUnavailable)
    }
}

/// Recognize the exact firmware-event notification delivered to the retained
/// RTKit endpoint. Bit 6 is a transport flag; the host-visible message type is
/// the low six bits (`0x42 & 0x3f == 2`).
pub(crate) const fn decode_g17p_firmware_event_notification(
    endpoint: u8,
    message: u64,
) -> Result<(), CompletionDecodeError> {
    if endpoint != G17P_FIRMWARE_EVENT_ENDPOINT {
        return Err(CompletionDecodeError::FirmwareEventEndpoint);
    }
    if message != G17P_FIRMWARE_EVENT_MESSAGE
        || ((message >> 48) as u8 & G17P_FIRMWARE_EVENT_HOST_TYPE_MASK)
            != G17P_FIRMWARE_EVENT_HOST_TYPE
    {
        return Err(CompletionDecodeError::FirmwareEventMessage);
    }
    Ok(())
}

/// Shared indices for one ordinary firmware-event ring.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) struct G17PFirmwareEventCursor {
    pub(crate) consumer: u32,
    pub(crate) producer: u32,
}

/// Pure read/publish plan for one host-consumed event record.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) struct G17PFirmwareEventRead {
    pub(crate) entry_offset: u32,
    pub(crate) published_consumer: u32,
    pub(crate) has_more_from_snapshot: bool,
}

/// Required host ordering for one role-local firmware-event record.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) enum G17PFirmwareEventReadStep {
    SnapshotIndices,
    ValidateIndices,
    CopyEntry,
    DecodeType,
    AdvanceConsumer,
    DmbIsh,
    PublishConsumer,
    CorrelateStamp,
}

pub(crate) const G17P_FIRMWARE_EVENT_READ_ORDER: [G17PFirmwareEventReadStep; 8] = [
    G17PFirmwareEventReadStep::SnapshotIndices,
    G17PFirmwareEventReadStep::ValidateIndices,
    G17PFirmwareEventReadStep::CopyEntry,
    G17PFirmwareEventReadStep::DecodeType,
    G17PFirmwareEventReadStep::AdvanceConsumer,
    G17PFirmwareEventReadStep::DmbIsh,
    G17PFirmwareEventReadStep::PublishConsumer,
    G17PFirmwareEventReadStep::CorrelateStamp,
];

pub(crate) const fn prepare_g17p_firmware_event_read(
    cursor: G17PFirmwareEventCursor,
) -> Result<G17PFirmwareEventRead, CompletionDecodeError> {
    if cursor.consumer >= G17P_FIRMWARE_EVENT_CAPACITY
        || cursor.producer >= G17P_FIRMWARE_EVENT_CAPACITY
    {
        return Err(CompletionDecodeError::FirmwareEventCursor);
    }
    if cursor.consumer == cursor.producer {
        return Err(CompletionDecodeError::FirmwareEventEmpty);
    }
    let published_consumer = (cursor.consumer + 1) & (G17P_FIRMWARE_EVENT_CAPACITY - 1);
    Ok(G17PFirmwareEventRead {
        entry_offset: cursor.consumer * G17P_FIRMWARE_EVENT_ENTRY_SIZE as u32,
        published_consumer,
        has_more_from_snapshot: published_consumer != cursor.producer,
    })
}

/// One decoded notification for a present completion descriptor.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) struct G17PKsmCompletionNotification {
    pub(crate) ordinal: u8,
    pub(crate) notification_offset: u32,
    pub(crate) acknowledgement_offset: u32,
    pub(crate) entry_count: u32,
}

pub(crate) const fn decode_g17p_normal_cl_notification(
    irq_sources: u64,
    ordinal: u8,
    notification_word: u32,
) -> Result<G17PKsmCompletionNotification, CompletionDecodeError> {
    if irq_sources & (1u64 << G17P_NORMAL_CL_COMPLETION_IRQ_BIT) == 0 {
        return Err(CompletionDecodeError::CompletionIrqSource);
    }
    if ordinal != G17P_NORMAL_CL_COMPLETION_ORDINAL {
        return Err(CompletionDecodeError::CompletionOrdinal);
    }
    let entry_count = if notification_word < 0x1000 {
        0
    } else {
        notification_word >> 16
    };
    Ok(G17PKsmCompletionNotification {
        ordinal,
        notification_offset: G17P_KSM_COMPLETION_NOTIFY_BASE
            + ordinal as u32 * G17P_KSM_COMPLETION_MMIO_STRIDE,
        acknowledgement_offset: G17P_KSM_COMPLETION_ACK_BASE
            + ordinal as u32 * G17P_KSM_COMPLETION_MMIO_STRIDE,
        entry_count,
    })
}

/// Firmware event type 1, delivered through the ordinary EP0x20 event ring.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) struct G17PFirmwareStampEvent {
    pub(crate) masks: [u64; 2],
    pub(crate) diagnostic_pair_count: u16,
}

impl G17PFirmwareStampEvent {
    pub(crate) const fn signals(self, stamp_index: u8) -> bool {
        if stamp_index >= 128 {
            return false;
        }
        let word = stamp_index as usize / 64;
        let bit = stamp_index as usize % 64;
        self.masks[word] & (1u64 << bit) != 0
    }
}

pub(crate) fn decode_g17p_firmware_stamp_event(
    raw: &[u8],
) -> Result<G17PFirmwareStampEvent, CompletionDecodeError> {
    if raw.len() != G17P_FIRMWARE_EVENT_ENTRY_SIZE {
        return Err(CompletionDecodeError::RecordSize);
    }
    if get_u32(raw, 0x00) != 1 {
        return Err(CompletionDecodeError::FirmwareEventType);
    }
    let diagnostic_pair_count = get_u16(raw, 0x14);
    if diagnostic_pair_count >= 6 {
        return Err(CompletionDecodeError::FirmwareEventDiagnosticCount);
    }
    Ok(G17PFirmwareStampEvent {
        masks: [get_u64(raw, 0x04), get_u64(raw, 0x0c)],
        diagnostic_pair_count,
    })
}

/// Firmware's per-QID timestamp ordering state.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) struct G17PCompletionQidState {
    pub(crate) valid: bool,
    pub(crate) last_completed_timestamp: u64,
    pub(crate) dependency_timestamp: u64,
}

pub(crate) const fn advance_g17p_completion_qid_state(
    state: G17PCompletionQidState,
    timestamp: u64,
) -> Result<G17PCompletionQidState, CompletionDecodeError> {
    if timestamp & !KSM_TIMESTAMP_MASK != 0 {
        return Err(CompletionDecodeError::CompletionTimestamp);
    }
    if state.valid
        && timestamp.wrapping_sub(state.last_completed_timestamp) & KSM_TIMESTAMP_MASK != 1
    {
        return Err(CompletionDecodeError::CompletionTimestampOrder);
    }
    Ok(G17PCompletionQidState {
        valid: true,
        last_completed_timestamp: timestamp,
        dependency_timestamp: state.dependency_timestamp,
    })
}

/// Host-owned state required to validate one first normal-CL completion.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) struct G17PNormalClCompletionState {
    pub(crate) cursor: KsmCompletionCursor,
    pub(crate) qid_state: G17PCompletionQidState,
    pub(crate) queue_id: u8,
    pub(crate) expected_timestamp: u64,
    pub(crate) expected_stamp_index: u8,
    pub(crate) outstanding: u32,
    pub(crate) descriptor_generation: u32,
}

/// A fully correlated transaction. Only this type carries an outstanding
/// decrement to the manager boundary.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) struct PreparedG17PNormalClCompletion {
    pub(crate) entry: G17pKsmCompletionEntry,
    pub(crate) notification: G17PKsmCompletionNotification,
    pub(crate) firmware_event: G17PFirmwareStampEvent,
    pub(crate) cursor_after: KsmCompletionCursor,
    pub(crate) qid_state_after: G17PCompletionQidState,
    pub(crate) outstanding_after: u32,
    pub(crate) completed_generation: u32,
}

pub(crate) fn prepare_g17p_first_normal_cl_completion(
    state: G17PNormalClCompletionState,
    irq_sources: u64,
    notification_word: u32,
    completion_entry: &[u8],
    firmware_event: &[u8],
) -> Result<PreparedG17PNormalClCompletion, CompletionDecodeError> {
    let notification = decode_g17p_normal_cl_notification(
        irq_sources,
        G17P_NORMAL_CL_COMPLETION_ORDINAL,
        notification_word,
    )?;
    if notification.entry_count != 1 {
        return Err(CompletionDecodeError::CompletionNotificationCount);
    }
    if state.cursor.capacity != G17P_NORMAL_CL_COMPLETION_CAPACITY {
        return Err(CompletionDecodeError::CompletionCapacity);
    }
    state.cursor.entry_offset()?;
    let entry = decode_compute_completion_entry(completion_entry)?;
    if entry.queue_id != state.queue_id {
        return Err(CompletionDecodeError::CompletionQueueId);
    }
    if entry.timestamp != state.expected_timestamp {
        return Err(CompletionDecodeError::CompletionTimestamp);
    }
    let qid_state_after = advance_g17p_completion_qid_state(state.qid_state, entry.timestamp)?;
    if state.expected_stamp_index >= 128 {
        return Err(CompletionDecodeError::FirmwareEventStampIndex);
    }
    let firmware_event = decode_g17p_firmware_stamp_event(firmware_event)?;
    if !firmware_event.signals(state.expected_stamp_index) {
        return Err(CompletionDecodeError::FirmwareEventStampMissing);
    }
    if state.descriptor_generation == 0 || state.descriptor_generation > state.outstanding {
        return Err(CompletionDecodeError::CompletionOutstanding);
    }
    Ok(PreparedG17PNormalClCompletion {
        entry,
        notification,
        firmware_event,
        cursor_after: state.cursor.advance()?,
        qid_state_after,
        outstanding_after: state.outstanding - state.descriptor_generation,
        completed_generation: state.descriptor_generation,
    })
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) struct QueueIndices {
    pub(crate) done: u32,
    pub(crate) read: u32,
    pub(crate) write: u32,
}

impl QueueIndices {
    fn validate(self) -> Result<(), CompletionDecodeError> {
        if self.done > self.write || self.read > self.write {
            return Err(CompletionDecodeError::QueueCounterOrder);
        }
        Ok(())
    }

    pub(crate) const fn idle(self) -> bool {
        self.done == self.read && self.read == self.write
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) struct QueuePrefixState {
    pub(crate) indices: QueueIndices,
    pub(crate) published_prefix: u32,
    pub(crate) accepted: bool,
    pub(crate) complete: bool,
    pub(crate) idle: bool,
}

pub(crate) fn observe_queue_prefix(
    previous: QueueIndices,
    current: QueueIndices,
    published_prefix: u32,
) -> Result<QueuePrefixState, CompletionDecodeError> {
    previous.validate()?;
    current.validate()?;
    if current.done < previous.done
        || current.read < previous.read
        || current.write < previous.write
    {
        return Err(CompletionDecodeError::QueueCounterRegression);
    }
    if published_prefix > current.write {
        return Err(CompletionDecodeError::PublishedPrefixBeyondWrite);
    }
    Ok(QueuePrefixState {
        indices: current,
        published_prefix,
        accepted: current.read >= published_prefix,
        complete: current.done >= published_prefix,
        idle: current.idle(),
    })
}

pub(crate) fn observe_compute_completion(
    previous: QueueIndices,
    current: QueueIndices,
    published_prefix: u32,
) -> Result<QueuePrefixState, CompletionDecodeError> {
    observe_queue_prefix(previous, current, published_prefix)
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) struct ComputeCompletionReport {
    pub(crate) entry: G17pKsmCompletionEntry,
    pub(crate) queue: QueuePrefixState,
}

pub(crate) fn observe_compute_completion_report(
    raw: &[u8],
    previous: QueueIndices,
    current: QueueIndices,
    published_prefix: u32,
) -> Result<ComputeCompletionReport, CompletionDecodeError> {
    Ok(ComputeCompletionReport {
        entry: decode_compute_completion_entry(raw)?,
        queue: observe_compute_completion(previous, current, published_prefix)?,
    })
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) struct PairedRenderCompletion {
    pub(crate) tiling: QueuePrefixState,
    pub(crate) fragment: QueuePrefixState,
    pub(crate) execution: RenderExecutionState,
    pub(crate) accepted: bool,
    pub(crate) retired: bool,
    pub(crate) complete: bool,
    pub(crate) idle: bool,
}

/// Scheduler state for one paired TA/fragment job.
///
/// Firmware queue retirement alone does not show that either GPU stage ran.
/// A finished pair has left the intrusive job list and has both owned
/// start/end timestamp words populated for each half. Optional UAPI
/// timestamp destinations are separate outputs and are not completion gates.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) struct RenderExecutionState {
    pub(crate) job_list_empty: bool,
    pub(crate) tiling_timestamps: [u64; 2],
    pub(crate) fragment_timestamps: [u64; 2],
}

impl RenderExecutionState {
    pub(crate) fn observed(self) -> bool {
        self.job_list_empty
            && self.tiling_timestamps.iter().all(|value| *value != 0)
            && self.fragment_timestamps.iter().all(|value| *value != 0)
    }
}

pub(crate) fn observe_paired_render_completion(
    tiling_previous: QueueIndices,
    tiling_current: QueueIndices,
    tiling_prefix: u32,
    fragment_previous: QueueIndices,
    fragment_current: QueueIndices,
    fragment_prefix: u32,
    execution: RenderExecutionState,
) -> Result<PairedRenderCompletion, CompletionDecodeError> {
    let tiling = observe_queue_prefix(tiling_previous, tiling_current, tiling_prefix)?;
    let fragment = observe_queue_prefix(fragment_previous, fragment_current, fragment_prefix)?;
    let retired = tiling.complete && fragment.complete;
    let complete = retired && execution.observed();
    Ok(PairedRenderCompletion {
        accepted: tiling.accepted && fragment.accepted,
        retired,
        complete,
        idle: tiling.idle && fragment.idle && execution.job_list_empty,
        tiling,
        fragment,
        execution,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // J700 hardware capture from
    // neo-bringup/logs/20260827-g17p-compact-contract-queue-live12/run.log.
    const CHANNEL14_CAPTURE: [[u8; REPORT_ENTRY_SIZE]; 5] = [
        hex64(b"110000008446b8df0000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000"),
        hex64(b"0000000000000000190000008446b8df000000000000000000000000000000000000000000000000010000000000000000000000640000000000000000000000"),
        hex64(b"000000000000000000000000000000001c0000008446b8df00000000010000000000000000000000000000000000000000000000000000000000000000000000"),
        hex64(b"00000000000000000000000000000000000000000000000019000000f9f5c0df000000008446b8df000000000100000000000000000000000100000000000000"),
        hex64(b"00000000640000000000000000000000000000000000000000000000000000001c000000f9f5c0df000000000100000000000000000000000000000000000000"),
    ];

    const fn nibble(value: u8) -> u8 {
        match value {
            b'0'..=b'9' => value - b'0',
            b'a'..=b'f' => value - b'a' + 10,
            _ => 0xff,
        }
    }

    const fn hex64(source: &[u8; 128]) -> [u8; 64] {
        let mut out = [0u8; 64];
        let mut index = 0;
        while index < 64 {
            out[index] = (nibble(source[index * 2]) << 4) | nibble(source[index * 2 + 1]);
            index += 1;
        }
        out
    }

    fn fnv1a64(records: &[[u8; REPORT_ENTRY_SIZE]]) -> u64 {
        let mut value = 0xcbf2_9ce4_8422_2325u64;
        for record in records {
            for byte in record {
                value ^= *byte as u64;
                value = value.wrapping_mul(0x0000_0100_0000_01b3);
            }
        }
        value
    }

    #[test]
    fn captured_report_counts_keep_channels_13_and_14_distinct() {
        assert_eq!(
            CapturedReportCounters { state: [0, 13, 0] }.visible_record_count(),
            13
        );
        assert_eq!(
            CapturedReportCounters { state: [0, 5, 0] }.visible_record_count(),
            5
        );
        let split = SplitReportCursor::from_words(0xfe, 1).unwrap();
        assert_eq!(split.pending(), 3);
        assert_eq!(split.acknowledgement(), 1);
    }

    #[test]
    fn channel14_capture_decodes_all_five_structural_shapes() {
        assert_eq!(fnv1a64(&CHANNEL14_CAPTURE), 0x9453_2fc9_2c3e_6089);
        assert!(matches!(
            decode_firmware_report(14, &CHANNEL14_CAPTURE[0]),
            Ok(FirmwareReportRecord::Trace(
                Channel14TraceRecord::Marker11At00 { .. }
            ))
        ));
        assert_eq!(
            decode_channel14_trace(14, &CHANNEL14_CAPTURE[0]),
            Ok(Channel14TraceRecord::Marker11At00 { tick: 0xdfb8_4684 })
        );
        assert_eq!(
            decode_channel14_trace(14, &CHANNEL14_CAPTURE[1]),
            Ok(Channel14TraceRecord::Marker19At08 {
                tick: 0xdfb8_4684,
                state_28: 1,
                value_34: 0x64,
            })
        );
        assert_eq!(
            decode_channel14_trace(14, &CHANNEL14_CAPTURE[2]),
            Ok(Channel14TraceRecord::Marker1cAt10 {
                tick: 0xdfb8_4684,
                state_1c: 1,
            })
        );
        assert_eq!(
            decode_channel14_trace(14, &CHANNEL14_CAPTURE[3]),
            Ok(Channel14TraceRecord::Marker19At18 {
                tick: 0xdfc0_f5f9,
                prior_tick: 0xdfb8_4684,
                state_2c: 1,
                state_38: 1,
            })
        );
        assert_eq!(
            decode_channel14_trace(14, &CHANNEL14_CAPTURE[4]),
            Ok(Channel14TraceRecord::Marker1cAt20 {
                value_04: 0x64,
                tick: 0xdfc0_f5f9,
                state_2c: 1,
            })
        );
    }

    #[test]
    fn channel13_fault_and_timeout_layouts_are_byte_exact() {
        let mut fault = [0u8; REPORT_ENTRY_SIZE];
        fault[4..12].copy_from_slice(&0x1122_3344_5566_7788u64.to_le_bytes());
        let decoded = decode_channel13_event(13, &fault).unwrap();
        match decoded {
            Channel13EventRecord::Fault(record) => {
                assert_eq!(
                    &record.payload[..8],
                    &0x1122_3344_5566_7788u64.to_le_bytes()
                );
            }
            _ => panic!("wrong event shape"),
        }

        let mut timeout = [0u8; REPORT_ENTRY_SIZE];
        timeout[0..4].copy_from_slice(&4u32.to_le_bytes());
        timeout[4..12].copy_from_slice(&0x0102_0304_0506_0708u64.to_le_bytes());
        timeout[12..16].copy_from_slice(&(-7i32).to_le_bytes());
        assert_eq!(
            decode_channel13_event(13, &timeout),
            Ok(Channel13EventRecord::Timeout(Channel13TimeoutRecord {
                counter: 0x0102_0304_0506_0708,
                stamp_index: -7,
            }))
        );
    }

    #[test]
    fn selector2_completion_entry_matches_firmware_bit_extraction() {
        let raw = hex64(b"6008785634129a0044443333222211118888777766665555ccccbbbbaaaa9999ab89674523010000bc9a785634020000018000c020fcffff000020c020fcffff");
        assert_eq!(
            decode_compute_completion_entry(&raw),
            Ok(G17pKsmCompletionEntry {
                completion_selector: 2,
                command_type: KsmCompletionCommandType::Type2,
                queue_id: 4,
                timestamp: 0x12_3456_789a,
                word_08: 0x1111_2222_3333_4444,
                payload: [0x5555_6666_7777_8888, 0x9999_aaaa_bbbb_cccc],
                parents: [0x123_4567_89ab, 0x234_5678_9abc],
                tagged_object: 0xffff_fc20_c000_8001,
                object: 0xffff_fc20_c000_8000,
                metadata: 0xffff_fc20_c020_0000,
            })
        );
        let opening = QueueIndices {
            done: 0,
            read: 0,
            write: 3,
        };
        let observed = observe_compute_completion_report(
            &raw,
            opening,
            QueueIndices {
                done: 3,
                read: 3,
                write: 3,
            },
            3,
        )
        .unwrap();
        assert_eq!(observed.entry.queue_id, 4);
        assert!(observed.queue.complete);
        assert_eq!(
            decode_ksm_completion_entry(4, &raw)
                .unwrap()
                .completion_selector,
            4
        );
        assert_eq!(
            decode_ksm_completion_entry(5, &raw),
            Err(CompletionDecodeError::CompletionSelector)
        );
    }

    #[test]
    fn completion_cursor_uses_0x40_byte_entries_and_wraps() {
        let cursor = KsmCompletionCursor {
            capacity: 4,
            processed: 3,
        };
        assert_eq!(cursor.entry_offset(), Ok(0xc0));
        assert_eq!(
            cursor.advance(),
            Ok(KsmCompletionCursor {
                capacity: 4,
                processed: 0,
            })
        );
        assert_eq!(
            KsmCompletionCursor {
                capacity: 0,
                processed: 0,
            }
            .advance(),
            Err(CompletionDecodeError::CompletionCapacity)
        );
    }

    #[test]
    fn normal_cl_completion_route_is_ordinal_zero_and_live_closed() {
        let route = G17P_NORMAL_CL_COMPLETION_ROUTE;
        assert_eq!(route.physical_aic_irq, 1001);
        assert_eq!(route.sgx_source_index, 4);
        assert_eq!(route.sgx_interrupts_valid, 0xdf);
        assert_eq!(route.firmware_event_endpoint, 0x20);
        assert_eq!(route.firmware_event_message, 0x0042_0000_0000_0000);
        assert_eq!(
            route.firmware_completion_owner,
            G17PCompletionOwner::GfxFirmware
        );
        assert!(!route.linux_gpu_dt_irq_resource);
        assert!(!route.physical_irq_registered);
        assert!(route.rtkit_notification_demultiplexed);
        assert!(!route.firmware_event_ring_source_mapped);
        assert!(!route.firmware_event_shared_indices_mapped);
        assert!(!route.firmware_event_entries_mapped);
        assert!(!route.firmware_event_consumer_writer_available);
        assert_eq!(G17P_FIRMWARE_EVENT_ENTRY_SIZE, 0x48);
        assert_eq!(G17P_FIRMWARE_EVENT_CAPACITY, 0x100);
        assert_eq!(G17P_COMPUTE_COMPLETION_SELECTOR, 2);
        assert_eq!(
            require_g17p_normal_cl_completion_route(route),
            Err(CompletionDecodeError::CompletionRouteUnavailable)
        );
        assert_eq!(
            decode_g17p_normal_cl_notification(1 << 21, 2, 1 << 16),
            Err(CompletionDecodeError::CompletionOrdinal)
        );
        assert_eq!(
            decode_g17p_normal_cl_notification(0, 0, 1 << 16),
            Err(CompletionDecodeError::CompletionIrqSource)
        );
        assert_eq!(
            decode_g17p_normal_cl_notification(1 << 21, 0, 0x800).unwrap(),
            G17PKsmCompletionNotification {
                ordinal: 0,
                notification_offset: 0x21168,
                acknowledgement_offset: 0x21188,
                entry_count: 0,
            }
        );
    }

    #[test]
    fn normal_cl_firmware_drain_has_firmware_owned_status_and_ack() {
        assert_eq!(
            G17P_FIRMWARE_COMPLETION_DRAIN,
            G17PFirmwareCompletionDrain {
                source_bit: 21,
                descriptor_table_offset: 0x2614,
                descriptor_stride: 0x20,
                descriptor_ordinal: 0,
                processed_offset: 0x1c,
                notification_offset: 0x21168,
                acknowledgement_offset: 0x21188,
                owner: G17PCompletionOwner::GfxFirmware,
            }
        );
        assert_eq!(
            G17P_FIRMWARE_COMPLETION_DRAIN.descriptor_table_offset
                + G17P_FIRMWARE_COMPLETION_DRAIN.descriptor_stride
                    * G17P_FIRMWARE_COMPLETION_DRAIN.descriptor_ordinal as u32
                + G17P_FIRMWARE_COMPLETION_DRAIN.processed_offset,
            0x2630
        );
    }

    #[test]
    fn exact_ep20_notification_is_demultiplexed_without_opening_live_route() {
        assert_eq!(
            decode_g17p_firmware_event_notification(0x20, 0x0042_0000_0000_0000),
            Ok(())
        );
        assert_eq!(
            decode_g17p_firmware_event_notification(0x21, 0x0042_0000_0000_0000),
            Err(CompletionDecodeError::FirmwareEventEndpoint)
        );
        assert_eq!(
            decode_g17p_firmware_event_notification(0x20, 0x0002_0000_0000_0000),
            Err(CompletionDecodeError::FirmwareEventMessage)
        );
        assert_eq!(
            (G17P_FIRMWARE_EVENT_MESSAGE >> 48) as u8 & G17P_FIRMWARE_EVENT_HOST_TYPE_MASK,
            G17P_FIRMWARE_EVENT_HOST_TYPE
        );
        assert_eq!(
            require_g17p_normal_cl_completion_route(G17P_NORMAL_CL_COMPLETION_ROUTE),
            Err(CompletionDecodeError::CompletionRouteUnavailable)
        );
    }

    #[test]
    fn firmware_event_read_wraps_and_never_publishes_an_empty_ring() {
        assert_eq!(
            prepare_g17p_firmware_event_read(G17PFirmwareEventCursor {
                consumer: 0,
                producer: 2,
            }),
            Ok(G17PFirmwareEventRead {
                entry_offset: 0,
                published_consumer: 1,
                has_more_from_snapshot: true,
            })
        );
        assert_eq!(
            prepare_g17p_firmware_event_read(G17PFirmwareEventCursor {
                consumer: 0xff,
                producer: 0,
            }),
            Ok(G17PFirmwareEventRead {
                entry_offset: 0xff * 0x48,
                published_consumer: 0,
                has_more_from_snapshot: false,
            })
        );
        assert_eq!(
            prepare_g17p_firmware_event_read(G17PFirmwareEventCursor {
                consumer: 7,
                producer: 7,
            }),
            Err(CompletionDecodeError::FirmwareEventEmpty)
        );
        assert_eq!(
            prepare_g17p_firmware_event_read(G17PFirmwareEventCursor {
                consumer: 0x100,
                producer: 0,
            }),
            Err(CompletionDecodeError::FirmwareEventCursor)
        );
        assert_eq!(G17P_FIRMWARE_EVENT_CONSUMER_OFFSET, 0);
        assert_eq!(G17P_FIRMWARE_EVENT_PRODUCER_OFFSET, 0x20);
        assert_eq!(
            G17P_FIRMWARE_EVENT_READ_ORDER,
            [
                G17PFirmwareEventReadStep::SnapshotIndices,
                G17PFirmwareEventReadStep::ValidateIndices,
                G17PFirmwareEventReadStep::CopyEntry,
                G17PFirmwareEventReadStep::DecodeType,
                G17PFirmwareEventReadStep::AdvanceConsumer,
                G17PFirmwareEventReadStep::DmbIsh,
                G17PFirmwareEventReadStep::PublishConsumer,
                G17PFirmwareEventReadStep::CorrelateStamp,
            ]
        );
    }

    #[test]
    fn live_route_requires_one_ingress_and_the_complete_event_ring_mapping() {
        let complete_rtkit = G17PNormalClCompletionRoute {
            firmware_event_ring_source_mapped: true,
            firmware_event_shared_indices_mapped: true,
            firmware_event_entries_mapped: true,
            firmware_event_consumer_writer_available: true,
            ..G17P_NORMAL_CL_COMPLETION_ROUTE
        };
        assert_eq!(
            require_g17p_normal_cl_completion_route(complete_rtkit),
            Ok(())
        );

        for unavailable in [
            G17PNormalClCompletionRoute {
                rtkit_notification_demultiplexed: false,
                ..complete_rtkit
            },
            G17PNormalClCompletionRoute {
                firmware_event_ring_source_mapped: false,
                ..complete_rtkit
            },
            G17PNormalClCompletionRoute {
                firmware_event_shared_indices_mapped: false,
                ..complete_rtkit
            },
            G17PNormalClCompletionRoute {
                firmware_event_entries_mapped: false,
                ..complete_rtkit
            },
            G17PNormalClCompletionRoute {
                firmware_event_consumer_writer_available: false,
                ..complete_rtkit
            },
            G17PNormalClCompletionRoute {
                firmware_completion_owner: G17PCompletionOwner::LinuxPlatform,
                ..complete_rtkit
            },
        ] {
            assert_eq!(
                require_g17p_normal_cl_completion_route(unavailable),
                Err(CompletionDecodeError::CompletionRouteUnavailable)
            );
        }
    }

    #[test]
    fn first_normal_cl_completion_requires_stamp_before_outstanding_subtraction() {
        let completion = hex64(b"6008785634129a0044443333222211118888777766665555ccccbbbbaaaa9999ab89674523010000bc9a785634020000018000c020fcffff000020c020fcffff");
        let mut event = [0u8; G17P_FIRMWARE_EVENT_ENTRY_SIZE];
        event[0..4].copy_from_slice(&1u32.to_le_bytes());
        event[4..12].copy_from_slice(&(1u64 << 7).to_le_bytes());
        let state = G17PNormalClCompletionState {
            cursor: KsmCompletionCursor {
                capacity: 0x800,
                processed: 0,
            },
            qid_state: G17PCompletionQidState {
                valid: false,
                last_completed_timestamp: 0,
                dependency_timestamp: 0,
            },
            queue_id: 4,
            expected_timestamp: 0x12_3456_789a,
            expected_stamp_index: 7,
            outstanding: 1,
            descriptor_generation: 1,
        };
        let prepared = prepare_g17p_first_normal_cl_completion(
            state,
            1 << 21,
            1 << 16,
            &completion,
            &event,
        )
        .unwrap();
        assert_eq!(prepared.entry.completion_selector, 2);
        assert_eq!(prepared.notification.ordinal, 0);
        assert_eq!(prepared.notification.entry_count, 1);
        assert_eq!(prepared.notification.acknowledgement_offset, 0x21188);
        assert_eq!(prepared.cursor_after.processed, 1);
        assert_eq!(prepared.qid_state_after.last_completed_timestamp, state.expected_timestamp);
        assert_eq!(prepared.completed_generation, 1);
        assert_eq!(prepared.outstanding_after, 0);

        event[4..12].fill(0);
        let mut outstanding_subtractions = 0;
        let gated: Result<(), CompletionDecodeError> = (|| {
            let prepared = prepare_g17p_first_normal_cl_completion(
                state,
                1 << 21,
                1 << 16,
                &completion,
                &event,
            )?;
            outstanding_subtractions += prepared.completed_generation;
            Ok(())
        })();
        assert_eq!(gated, Err(CompletionDecodeError::FirmwareEventStampMissing));
        assert_eq!(outstanding_subtractions, 0);
    }

    #[test]
    fn normal_cl_completion_rejects_bad_sequence_event_and_generation() {
        assert_eq!(
            advance_g17p_completion_qid_state(
                G17PCompletionQidState {
                    valid: true,
                    last_completed_timestamp: 7,
                    dependency_timestamp: 3,
                },
                9,
            ),
            Err(CompletionDecodeError::CompletionTimestampOrder)
        );
        assert_eq!(
            advance_g17p_completion_qid_state(
                G17PCompletionQidState {
                    valid: true,
                    last_completed_timestamp: KSM_TIMESTAMP_MASK,
                    dependency_timestamp: 3,
                },
                0,
            )
            .unwrap()
            .last_completed_timestamp,
            0
        );
        let mut event = [0u8; G17P_FIRMWARE_EVENT_ENTRY_SIZE];
        event[0..4].copy_from_slice(&1u32.to_le_bytes());
        event[0x14..0x16].copy_from_slice(&6u16.to_le_bytes());
        assert_eq!(
            decode_g17p_firmware_stamp_event(&event),
            Err(CompletionDecodeError::FirmwareEventDiagnosticCount)
        );
        event[0..4].copy_from_slice(&2u32.to_le_bytes());
        event[0x14..0x16].fill(0);
        assert_eq!(
            decode_g17p_firmware_stamp_event(&event),
            Err(CompletionDecodeError::FirmwareEventType)
        );
    }

    #[test]
    fn compute_queue_progress_uses_read_for_acceptance_and_done_for_completion() {
        let opening = QueueIndices {
            done: 0,
            read: 0,
            write: 3,
        };
        let accepted = observe_compute_completion(
            opening,
            QueueIndices {
                done: 0,
                read: 3,
                write: 3,
            },
            3,
        )
        .unwrap();
        assert!(accepted.accepted);
        assert!(!accepted.complete);
        assert!(!accepted.idle);

        let complete = observe_compute_completion(
            opening,
            QueueIndices {
                done: 3,
                read: 3,
                write: 3,
            },
            3,
        )
        .unwrap();
        assert!(complete.accepted);
        assert!(complete.complete);
        assert!(complete.idle);
    }

    #[test]
    fn paired_render_separates_queue_retirement_from_gpu_execution() {
        let opening = QueueIndices {
            done: 0,
            read: 0,
            write: 3,
        };
        let complete = QueueIndices {
            done: 3,
            read: 3,
            write: 3,
        };
        let fragment_pending = QueueIndices {
            done: 0,
            read: 3,
            write: 3,
        };
        let executed = RenderExecutionState {
            job_list_empty: true,
            tiling_timestamps: [0x10, 0x20],
            fragment_timestamps: [0x50, 0x60],
        };
        let pending = observe_paired_render_completion(
            opening,
            complete,
            3,
            opening,
            fragment_pending,
            3,
            executed,
        )
        .unwrap();
        assert!(pending.accepted);
        assert!(!pending.retired);
        assert!(!pending.complete);
        assert!(!pending.idle);

        let finished =
            observe_paired_render_completion(opening, complete, 3, opening, complete, 3, executed)
                .unwrap();
        assert!(finished.accepted);
        assert!(finished.retired);
        assert!(finished.complete);
        assert!(finished.idle);
    }

    #[test]
    fn live27_parked_job_is_retired_but_not_complete() {
        let opening = QueueIndices {
            done: 0,
            read: 0,
            write: 3,
        };
        let retired = QueueIndices {
            done: 3,
            read: 3,
            write: 3,
        };
        let parked = RenderExecutionState {
            job_list_empty: false,
            tiling_timestamps: [0; 2],
            fragment_timestamps: [0; 2],
        };
        let state =
            observe_paired_render_completion(opening, retired, 3, opening, retired, 3, parked)
                .unwrap();

        assert!(state.accepted);
        assert!(state.retired);
        assert!(!state.complete);
        assert!(!state.idle);
        assert!(!state.execution.observed());
    }

    #[test]
    fn queue_counter_regressions_and_prefix_overruns_are_reported() {
        let previous = QueueIndices {
            done: 3,
            read: 3,
            write: 6,
        };
        assert_eq!(
            observe_compute_completion(
                previous,
                QueueIndices {
                    done: 2,
                    read: 3,
                    write: 6,
                },
                6,
            ),
            Err(CompletionDecodeError::QueueCounterRegression)
        );
        assert_eq!(
            observe_compute_completion(previous, previous, 7),
            Err(CompletionDecodeError::PublishedPrefixBeyondWrite)
        );
    }
}
