#!/usr/bin/env python3
"""Validate a private Ethernet DHCP pcap without printing network identities."""
import argparse
from collections import Counter
import json
from pathlib import Path
import struct


def checksum(data):
    if len(data) & 1:
        data += b'\0'
    total = sum(struct.unpack('!%dH' % (len(data) // 2), data))
    while total >> 16:
        total = (total & 0xffff) + (total >> 16)
    return (~total) & 0xffff


def inspect(path):
    if path.stat().st_mode & 0o077:
        raise ValueError('Capture must have private permissions')
    if path.stat().st_size > 64 * 1024 * 1024:
        raise ValueError('Capture exceeds bounded inspection size')
    data = path.read_bytes()
    endian = {b'\xd4\xc3\xb2\xa1': '<', b'\xa1\xb2\xc3\xd4': '>',
              b'\x4d\x3c\xb2\xa1': '<', b'\xa1\xb2\x3c\x4d': '>'}.get(data[:4])
    if not endian or len(data) < 24 or struct.unpack_from(endian + 'I', data, 20)[0] != 1:
        raise ValueError('Expected a classic Ethernet pcap')
    offset, records = 24, 0
    messages, transactions = Counter(), {}
    while offset < len(data):
        if offset + 16 > len(data):
            raise ValueError('Truncated record header')
        _, _, size, original = struct.unpack_from(endian + 'IIII', data, offset)
        offset += 16
        if size > original or offset + size > len(data):
            raise ValueError('Truncated or invalid record')
        frame = data[offset:offset + size]
        offset += size
        records += 1
        if len(frame) < 34 or frame[12:14] != b'\x08\x00':
            continue
        ip = frame[14:]
        ihl = (ip[0] & 15) * 4
        total = struct.unpack_from('!H', ip, 2)[0]
        fragment = struct.unpack_from('!H', ip, 6)[0]
        if ip[0] >> 4 != 4 or ihl < 20 or total > len(ip) or total < ihl + 8:
            raise ValueError('Invalid IPv4 bounds')
        if ip[9] != 17 or fragment & 0x3fff:
            continue
        udp = ip[ihl:total]
        source, destination, length, udp_checksum = struct.unpack_from('!HHHH', udp)
        if {source, destination} != {67, 68}:
            continue
        if length < 248 or length > len(udp):
            raise ValueError('Invalid DHCP datagram bounds')
        bootp = udp[8:length]
        if bootp[236:240] != b'\x63\x82\x53\x63':
            raise ValueError('Missing DHCP magic cookie')
        options, index = {}, 240
        while index < len(bootp):
            code = bootp[index]
            index += 1
            if code == 255:
                break
            if code == 0:
                continue
            if index >= len(bootp):
                raise ValueError('Truncated DHCP option length')
            count = bootp[index]
            index += 1
            if index + count > len(bootp):
                raise ValueError('Truncated DHCP option value')
            options[code] = bootp[index:index + count]
            index += count
        kind = options.get(53, b'')
        if len(kind) != 1:
            raise ValueError('Invalid DHCP message type')
        kind = kind[0]
        messages[kind] += 1
        identity = (bootp[4:8], bootp[28:44])
        group = transactions.setdefault(identity, len(transactions) + 1)
        pseudo = ip[12:20] + b'\0\x11' + struct.pack('!H', length)
        print(json.dumps({
            'record': records, 'transaction_group': group, 'dhcp_type': kind,
            'bootp_op': bootp[0], 'source_port': source, 'destination_port': destination,
            'ethernet_broadcast': frame[:6] == b'\xff' * 6,
            'ipv4_broadcast': ip[16:20] == b'\xff' * 4,
            'bootp_broadcast': bool(struct.unpack_from('!H', bootp, 10)[0] & 0x8000),
            'ethernet_client_type_valid': bootp[1:3] == b'\x01\x06',
            'client_matches_ethernet_source': bootp[28:34] == frame[6:12],
            'ipv4_checksum_valid': checksum(ip[:ihl]) == 0,
            'udp_checksum_present': udp_checksum != 0,
            'udp_checksum_valid': udp_checksum == 0 or checksum(pseudo + udp[:length]) == 0,
            'ipv4_length': total, 'udp_length': length, 'option_codes': sorted(options),
        }, sort_keys=True))
    print(json.dumps({'records': records, 'dhcp_message_counts': dict(messages),
                      'transactions': len(transactions)}, sort_keys=True))


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('capture', type=Path)
    arguments = parser.parse_args()
    inspect(arguments.capture)
