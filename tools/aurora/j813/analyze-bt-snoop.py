#!/usr/bin/env python3
"""Summarize a private Linux-monitor btsnoop without exposing peer identities.

Only aggregate counters, opcodes, lengths and status values are emitted.
Addresses, link keys and vendor seed bytes stay private in the original capture.
"""
import argparse
from collections import Counter
import json
from pathlib import Path
import struct


def summarize(path):
    data = path.read_bytes()
    if data[:8] != b'btsnoop\0' or len(data) < 16:
        raise ValueError('not a btsnoop file')
    if struct.unpack_from('>II', data, 8) != (1, 2001):
        raise ValueError('expected Linux monitor btsnoop version 1, datalink 2001')
    offset = 16
    commands = []
    completions = Counter()
    connection_results = Counter()
    authentication_results = Counter()
    pairing_results = Counter()
    encryption_results = Counter()
    encryption_key_sizes = Counter()
    link_key_types = Counter()
    disconnection_reasons = Counter()
    link_key_notifications = 0
    packets = Counter()
    advertising_reports = 0
    expected_address = None
    matching_reads = 0
    mismatching_reads = 0
    init_done_valid = []
    seed_lengths = []
    while offset < len(data):
        if len(data) - offset < 24:
            raise ValueError('truncated btsnoop record header')
        original, length, flags, _, _ = struct.unpack_from('>IIIIQ', data, offset)
        offset += 24
        if original != length or length > len(data) - offset:
            raise ValueError('truncated btsnoop record body')
        packet = data[offset:offset + length]
        offset += length
        kind = flags & 0xffff
        packets[kind] += 1
        if kind == 2:
            if length < 3 or packet[2] + 3 != length:
                raise ValueError('invalid HCI command length')
            opcode = struct.unpack_from('<H', packet)[0]
            commands.append((opcode, packet[2]))
            if opcode == 0xfc06 and packet[2] == 6:
                expected_address = packet[3:][::-1]
            elif opcode == 0xff0b:
                init_done_valid.append(packet[3:] == b'\xff' * 16)
            elif opcode == 0xfe44:
                seed_lengths.append(packet[2])
        elif kind == 3:
            if length < 2 or packet[1] + 2 != length:
                raise ValueError('invalid HCI event length')
            if packet[0] == 0x0e and length >= 6:
                opcode = struct.unpack_from('<H', packet, 3)[0]
                completions[(opcode, packet[5])] += 1
                if opcode == 0x1408 and length == 9 and packet[5] == 0:
                    encryption_key_sizes[packet[8]] += 1
                if opcode == 0x1009 and packet[5] == 0 and expected_address:
                    if length == 12 and packet[6:] == expected_address:
                        matching_reads += 1
                    else:
                        mismatching_reads += 1
            elif packet[0] == 0x0f and length == 6:
                opcode = struct.unpack_from('<H', packet, 4)[0]
                completions[(opcode, packet[2])] += 1
            elif packet[0] == 0x3e and length >= 4 and packet[2] in (2, 0x0d):
                advertising_reports += packet[3]
            elif packet[0] == 0x03 and length == 13:
                connection_results[(packet[2], packet[11])] += 1
            elif packet[0] == 0x06 and length == 5:
                authentication_results[packet[2]] += 1
            elif packet[0] == 0x36 and length == 9:
                pairing_results[packet[2]] += 1
            elif packet[0] == 0x08 and length == 6:
                encryption_results[(packet[2], packet[5])] += 1
            elif packet[0] == 0x05 and length == 6:
                disconnection_reasons[packet[5]] += 1
            elif packet[0] == 0x18 and length == 25:
                link_key_notifications += 1
                link_key_types[packet[24]] += 1
        elif kind in (4, 5):
            if length < 4 or struct.unpack_from('<H', packet, 2)[0] + 4 != length:
                raise ValueError('invalid HCI ACL length')
    return {
        'records': sum(packets.values()),
        'hci_commands': len(commands),
        'hci_events': packets[3],
        'first_commands': [{'opcode': f'0x{op:04x}', 'parameters': n}
                           for op, n in commands[:4]],
        'native_startup_before_reset': [op for op, _ in commands[:4]] ==
                                      [0xfc06, 0xff0b, 0xfe44, 0x0c03],
        'init_done_masks_valid': init_done_valid,
        'seed_parameter_lengths': seed_lengths,
        'native_address_matching_reads': matching_reads,
        'native_address_mismatching_reads': mismatching_reads,
        'advertising_reports': advertising_reports,
        'acl_tx_packets': packets[4],
        'acl_rx_packets': packets[5],
        'connection_results': [{'status': status, 'link_type': link_type, 'count': count}
                               for (status, link_type), count in sorted(connection_results.items())],
        'authentication_results': [{'status': status, 'count': count}
                                   for status, count in sorted(authentication_results.items())],
        'pairing_results': [{'status': status, 'count': count}
                           for status, count in sorted(pairing_results.items())],
        'encryption_results': [{'status': status, 'enabled': enabled, 'count': count}
                               for (status, enabled), count in sorted(encryption_results.items())],
        'encryption_key_sizes': [{'bytes': size, 'count': count}
                                 for size, count in sorted(encryption_key_sizes.items())],
        'link_key_types': [{'type': key_type, 'count': count}
                           for key_type, count in sorted(link_key_types.items())],
        'disconnection_reasons': [{'reason': reason, 'count': count}
                                 for reason, count in sorted(disconnection_reasons.items())],
        'link_key_notifications': link_key_notifications,
        'command_failures': [{'opcode': f'0x{op:04x}', 'status': status, 'count': count}
                             for (op, status), count in sorted(completions.items())
                             if status],
    }


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('capture', type=Path)
    args = parser.parse_args()
    try:
        print(json.dumps(summarize(args.capture), indent=2))
    except ValueError as error:
        parser.exit(1, f'Capture rejected: {error}\n')
