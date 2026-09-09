#!/usr/bin/env python3
"""Validate private N1 RX samples; print only lengths and protocol checks."""
import argparse
from collections import Counter
import json
import struct


def inspect(data):
    if len(data) != 8 + 8 * (32 + 4096):
        raise ValueError('Unsupported snapshot size')
    version, count = struct.unpack_from('<II', data)
    if version != 1 or not 1 <= count <= 8:
        raise ValueError('Unsupported snapshot header')
    results, protocols = [], Counter()
    for index in range(count):
        offset = 8 + index * 4128
        header, info, status, footer = struct.unpack_from('<IIII', data, offset)
        cookie, length = info & 65535, info >> 16
        if header >> 16 != 12 or not 1 <= cookie <= 127 or length > 4096:
            raise ValueError('Invalid completion metadata')
        # AirshipDataRxTransferRing::processCompletions uses bits 15:12 as
        # the prefix length unless the extended-header flag (bit 24) is set.
        padding = (footer >> 12) & 15
        extended = bool(footer & (1 << 24))
        packet = data[offset + 32:offset + 32 + length]
        result = {'sample': index, 'length': length, 'padding': padding,
                  'extended': extended, 'status': status}
        if extended or length < padding + 14:
            result['ethernet_validated'] = False
            results.append(result)
            continue
        frame = packet[padding:]
        ethertype = int.from_bytes(frame[12:14], 'big')
        payload = frame[14:]
        protocols[f'{ethertype:04x}'] += 1
        valid = False
        if ethertype == 0x0800 and len(payload) >= 20:
            ihl = (payload[0] & 15) * 4
            total = int.from_bytes(payload[2:4], 'big')
            valid = payload[0] >> 4 == 4 and 20 <= ihl <= total <= len(payload)
            if valid:
                checksum = sum(struct.unpack('>' + 'H' * (ihl // 2), payload[:ihl]))
                while checksum >> 16:
                    checksum = (checksum & 65535) + (checksum >> 16)
                result['ipv4_header_checksum_valid'] = checksum == 65535
                valid = checksum == 65535
        elif ethertype == 0x86dd and len(payload) >= 40:
            total = 40 + int.from_bytes(payload[4:6], 'big')
            valid = payload[0] >> 4 == 6 and total <= len(payload)
        elif ethertype == 0x0806 and len(payload) >= 28:
            valid = (payload[:6] == bytes.fromhex('000108000604') and
                     int.from_bytes(payload[6:8], 'big') in (1, 2))
        result['ethertype'] = f'{ethertype:04x}'
        result['ethernet_validated'] = valid
        results.append(result)
    return {'version': version, 'samples': count, 'protocol_counts': dict(protocols),
            'metadata': results}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('snapshot')
    args = parser.parse_args()
    with open(args.snapshot, 'rb') as source:
        data = source.read(33033)
    print(json.dumps(inspect(data), sort_keys=True))


if __name__ == '__main__':
    try:
        main()
    except Exception as error:
        raise SystemExit('RX metadata decoding failed: ' + type(error).__name__)
