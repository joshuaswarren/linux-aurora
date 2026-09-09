#!/usr/bin/env python3
"""Validate N1 DRAM trace framing and print metadata only.

Never prints record arguments, packet data or private payload bytes. Source 6
matches UMAC log call sites and source 4 matches the TX firmware call sites.
Other source IDs are left numeric until their emitter mapping is verified.
"""
import argparse
from collections import Counter
from dataclasses import dataclass
import json
from pathlib import Path
import struct


SOURCES = {1, 2, 3, 4, 5, 6, 7, 8, 0x12, 0x22, 0x7f}

# Exact call sites whose arguments have been checked against the matching
# firmware. No general-purpose argument dumper: other records may hold keys.
QUEUE_LOG_SITES = {
    (6, 0x11a509): ('ctm_select', 4, ('flow', 'link', 'kind', 'subkind')),
    (6, 0x118ff7): ('ctm_submit', 4, ('queue_flags', 'tag_flags', 'length_head', 'indices')),
    (6, 0x119003): ('ctm_lifetime', 3, ('relative', 'now', 'expiry')),
    (6, 0x11a2c5): ('ctm_complete', 4, ('queue_sequence', 'tag_status', 'count')),
    (6, 0x11a42f): ('ctm_retire', 4, ('status_tag', 'producer', 'consumer', 'retired')),
    (6, 0x524f83): ('supp_dispatch', 3, ('index', 'kind')),
    (6, 0x524efd): ('supp_action', 1, ('action',)),
    (6, 0x52721b): ('supp_abort', 1, ('link',)),
    (6, 0x529141): ('supp_result', 2, ('result', 'phase')),
    (6, 0x5275f9): ('supp_complete', 2, ('context', 'result')),
    (6, 0x529011): ('supp_timer', 2, ('link', 'duration')),
    (4, 0x11429b): ('tx_complete', 4, ('queue_info', 'tag_status', 'sequence_count', 'flags')),
    # tx_exchange_handle_winmove_common_and_ring_fp_messages, call at
    # 0x1140c4: ring/queue/status, sequence range, counts, host TR plus tag.
    (4, 0x1140c9): ('tx_host_completion', 4, ('queue_status', 'sequence_range', 'counts', 'host_ring_tag')),
    (4, 0x1144cd): ('tx_retire_status', 4, ('ring_ids', 'link_tid_status', 'count', 'draining')),
    (4, 0x10488d): ('tx_schedulable', 4, ('queue', 'mask', 'clear', 'old_blocked')),
    (4, 0x112b5d): ('tx_selection_wait', 4, ('radio', 'access', 'queue', 'state')),
    (4, 0x111ff7): ('tx_selection_state', 4, ('radio', 'access', 'old_state', 'state')),
    (4, 0x112eeb): ('tx_selection_list', 4, ('selector', 'queues_low', 'queues_high', 'access_map')),
    (4, 0x108977): ('tx_link_action', 4, ('link', 'actions', 'add', 'old_actions')),
    (4, 0x1084b7): ('tx_link_state', 2, ('request', 'old_flags')),
    # link_mgr_update_link 0x107dc0, call at 0x10829c: byte-sized link ID
    # and three encryption booleans. No MAC addresses or key data.
    (4, 0x1082a1): ('tx_link_encryption', 1, ('link_flags',)),
    # rings_mgr_open_tr 0x10b014, call at 0x10b1dc: internal/wire ring IDs,
    # AC/index, state byte/encryption boolean, multicast and EAPOL booleans.
    # The destination MAC used earlier in this function is not logged here.
    (4, 0x10b1e1): ('tx_ring_open', 4,
                    ('ring_ids', 'ac_index', 'state_encryption', 'multicast_eapol')),
    # link_mgr_find_linkid_for_tr_params 0x109f44, call at 0x10a134:
    # internal ring, packed interface/type/link/role, selection flags, MLD.
    # This successful-selection log contains no destination MAC bytes.
    (4, 0x10a139): ('tx_ring_link_selection', 4,
                    ('ring', 'interface_type_link_role', 'selection', 'mld')),
    # LINK_MANAGER_encryption_set 0x10f39c, call at 0x10f3ce:
    # link ID, previous two-bit mode, requested mode. No key material.
    (3, 0x10f3d3): ('radio_encryption_mode', 3, ('link', 'previous', 'mode')),
    (3, 0x12dab9): ('radio_flush', 3, ('queue', 'previous_mode', 'mode')),
    (3, 0x12d015): ('radio_flush_result', 3, ('queue_write', 'window_end', 'mode_status')),
    (3, 0x12d615): ('radio_queue_state', 3, ('queue', 'state', 'event')),
    (3, 0x12da05): ('radio_queue_disable', 4, ('queue', 'reason', 'disable', 'mask')),
    (3, 0x119a7b): ('radio_selection_busy', 4, ('access', 'index', 'queue', 'busy')),
    (3, 0x117b5d): ('radio_txop_state', 4, ('access', 'outstanding', 'hardware', 'progress')),
}


@dataclass(frozen=True)
class Record:
    slot: int
    offset: int
    source: int
    site: int
    timestamp_fragment: int
    words: int
    encoding: int
    compressed: bool


def decode_queue_record(data, record):
    """Decode only reviewed, non-secret queue metadata; never arbitrary logs."""
    specification = QUEUE_LOG_SITES.get((record.source, record.site))
    if specification is None:
        raise ValueError('This site is not approved for argument decoding')
    label, count, names = specification
    payload = data[record.offset + 8:record.offset + record.words * 4]
    if record.compressed:
        values, value, shift = [], 0, 0
        for byte in payload:
            if shift > 28 or (shift == 28 and byte & 0x70):
                raise ValueError('Queue metadata integer exceeds 32 bits')
            value |= (byte & 127) << shift
            if byte & 128:
                shift += 7
            else:
                values.append(value)
                value = shift = 0
                if len(values) == count:
                    break
        if len(values) != count:
            raise ValueError('Truncated queue metadata')
    else:
        if len(payload) < count * 4:
            raise ValueError('Truncated queue metadata')
        values = struct.unpack_from('<' + 'I' * count, payload)
    # Unnamed arguments are deliberately discarded, not exposed.
    return label, dict(zip(names, values))


def parse_capture(path):
    data = Path(path).read_bytes()
    if len(data) < 0x158:
        raise ValueError('Short control header')
    version, count, size, mask, start = struct.unpack_from('<HHIII', data)
    if (version != 0x201 or count < 8 or count & (count - 1) or
            size != 65536 or mask != count - 1 or start < 0x158 or
            start + count * size > len(data)):
        raise ValueError('Unsupported control header')
    consumer = struct.unpack_from('<I', data, 0x10)[0]
    producer = struct.unpack_from('<I', data, 0xb0)[0]
    records, invalid = [], []
    for slot in range(count):
        base = start + slot * size
        first, tag, _, last_word = struct.unpack_from('<IIII', data, base)
        if first & 255 != 0x7f or tag != 0x04000013:
            invalid.append({'slot': slot, 'error': 'missing block header'})
            continue
        end = (last_word + 1) * 4
        if end < 32 or end > size:
            invalid.append({'slot': slot, 'error': 'invalid block length'})
            continue
        position, block, error = 0, [], None
        while position < end:
            if position + 8 > end:
                error = 'partial record header'
                break
            clock, identifier = struct.unpack_from('<II', data, base + position)
            source = clock & 255
            words = 3 + (identifier >> 26)
            if source not in SOURCES or position + words * 4 > end:
                error = 'invalid source or record boundary'
                break
            block.append(Record(slot, base + position, source,
                                identifier & 0xffffff, (clock >> 8) & 0x7fffff,
                                words, (identifier >> 24) & 3, bool(clock >> 31)))
            position += words * 4
        if error:
            # Exclude the entire inconsistent block, including its valid prefix.
            invalid.append({'slot': slot, 'error': error, 'offset': position})
        else:
            records.extend(block)
    metadata = {'bytes': len(data), 'version': version, 'blocks': count,
                'consumer': consumer, 'producer': producer,
                'valid_blocks': count - len(invalid), 'invalid_blocks': invalid,
                'records': len(records),
                'source_counts': dict(sorted(Counter(r.source for r in records).items()))}
    return metadata, records


def integer(value):
    return int(value, 0)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('capture', type=Path)
    parser.add_argument('--source', type=integer)
    parser.add_argument('--min-site', type=integer, default=0)
    parser.add_argument('--max-site', type=integer, default=0xffffff)
    parser.add_argument('--sites', type=int, default=0,
                        help='Maximum site counts to print; no arguments are exposed')
    args = parser.parse_args()
    metadata, records = parse_capture(args.capture)
    print(json.dumps(metadata, sort_keys=True))
    if args.sites:
        counts = Counter((r.source, r.site) for r in records
                         if (args.source is None or r.source == args.source) and
                         args.min_site <= r.site <= args.max_site)
        for (source, site), count in counts.most_common(args.sites):
            print(f'source={source} site={site:#08x} count={count}')


if __name__ == '__main__':
    try:
        main()
    except Exception as error:
        # Exceptions must never echo binary data from a private artifact.
        raise SystemExit('Trace decoding failed: ' + type(error).__name__)
