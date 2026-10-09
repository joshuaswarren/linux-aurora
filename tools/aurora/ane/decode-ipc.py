#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-2.0-only
"""Decode saved H17 ANE IPC descriptors; never access or initialize hardware.

The channel layout was observed in J700 25G76 and in a successful 25G83
macOS HV run. A valid decode establishes structural consistency; this tool
cannot establish firmware compatibility or evidence of ANE computation.
"""
import argparse
import json
from pathlib import Path
import struct

DESCRIPTOR_SIZE = 0x100
SLOT_SIZE = 0x40
MAX_CHANNELS = 32
MAX_SLOTS = 512
U64_END = 1 << 64


class ProtocolError(ValueError):
    pass


def require(condition, message):
    if not condition:
        raise ProtocolError(message)


def checked_range(address, size):
    require(isinstance(address, int) and isinstance(size, int), "range must be integers")
    require(0 <= address < U64_END and 0 < size <= U64_END - address,
            "invalid or overflowing 64-bit range")
    return address, address + size


def decode_boot_buffers(data):
    """Decode only the observed buffer fields of the 0x174-byte boot record.

    Other fields, including qword zero, remain uninterpreted. In particular,
    native 25G83 qword zero is zero, not the firmware IOVA used by the older
    standalone experiment. This is a capture validator, never a boot builder.
    """
    require(len(data) == 0x174, 'boot record must be exactly 0x174 bytes')
    words = struct.unpack_from('<13Q', data)
    stride = struct.unpack_from('<I', data, 0x68)[0]
    require(stride == SLOT_SIZE, 'unsupported captured slot stride')
    result, spans = {}, []
    for name, address, size in (('ipc', words[1], words[10]),
                                ('heap', words[4], words[5]),
                                ('command', words[11], words[12])):
        start, end = checked_range(address, size)
        require(start % SLOT_SIZE == 0 and end <= 1 << 42,
                f'{name}: unaligned buffer or outside DART address width')
        require(all(end <= old_start or old_end <= start for old_start, old_end in spans),
                'overlapping captured boot buffers')
        spans.append((start, end))
        result[name] = {'iova': address, 'size': size}
    return dict(buffers=result, slot_stride=stride, field_00_uninterpreted=words[0])


def decode_channels(data, count, arena_iova, arena_size):
    """Validate the complete table before returning any usable ring addresses."""
    start, end = checked_range(arena_iova, arena_size)
    require(start % SLOT_SIZE == 0, "unaligned IPC arena")
    require(1 <= count <= MAX_CHANNELS, "unsupported channel count")
    table_size = count * DESCRIPTOR_SIZE
    require(table_size <= len(data) <= arena_size, "truncated or oversized channel table")
    occupied = [(start, start + table_size)]
    require(occupied[0][1] <= end, "channel table exceeds IPC arena")
    names, sources, channels = set(), set(), []
    for index in range(count):
        offset = index * DESCRIPTOR_SIZE
        name_bytes = data[offset:offset + 64]
        require(b"\0" in name_bytes, "unterminated channel name")
        name_bytes = name_bytes.split(b"\0", 1)[0]
        require(name_bytes and all(chr(c).isascii() and
                (chr(c).isalnum() or c == ord('_')) for c in name_bytes),
                "invalid channel name")
        name = name_bytes.decode('ascii')
        direction, source, slots, address = struct.unpack_from('<IIQQ', data, offset + 0x40)
        require(direction in (0, 1, 2), f"{name}: unsupported direction")
        require(source < 32, f"{name}: source exceeds 32-bit doorbell")
        require(name not in names and source not in sources, "duplicate channel name or source")
        require(1 <= slots <= MAX_SLOTS, f"{name}: unsupported slot count")
        require(address % SLOT_SIZE == 0, f"{name}: unaligned ring")
        ring_start, ring_end = checked_range(address, slots * SLOT_SIZE)
        require(start <= ring_start < ring_end <= end, f"{name}: ring outside IPC arena")
        require(all(ring_end <= old_start or old_end <= ring_start
                    for old_start, old_end in occupied), f"{name}: overlapping ring or table")
        occupied.append((ring_start, ring_end))
        names.add(name)
        sources.add(source)
        channels.append({'index': index, 'name': name, 'direction': direction,
                         'source': source, 'slots': slots, 'iova': address,
                         'bytes': slots * SLOT_SIZE})
    return channels


def decode_slot(data, buffer_iova, buffer_size):
    """Decode the three observed qwords, without interpreting the third one.

Ownership bit 0 is reported separately. A clear bit denotes a published slot;
its pointer and length must fit the explicitly supplied captured buffer.
Bit 1 is retained as an uninterpreted flag, not a claim about chaining.
"""
    start, end = checked_range(buffer_iova, buffer_size)
    require(len(data) == SLOT_SIZE, "slot must be exactly 64 bytes")
    pointer, length, metadata = struct.unpack_from('<QQQ', data)
    published = not bool(pointer & 1)
    address = pointer & ~3
    if published:
        payload_start, payload_end = checked_range(address, length)
        require(start <= payload_start < payload_end <= end,
                "published payload outside captured buffer")
    return {'pointer_word': pointer, 'flags': pointer & 3, 'published': published,
            'payload_iova': address, 'length': length, 'metadata': metadata}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('capture', type=Path)
    parser.add_argument('--iova', type=lambda v: int(v, 0))
    parser.add_argument('--size', type=lambda v: int(v, 0))
    parser.add_argument('--boot-args', type=Path,
                        help='derive and validate IPC bounds from a captured boot record')
    mode = parser.add_mutually_exclusive_group(required=True)
    mode.add_argument('--channels', type=int)
    mode.add_argument('--slot', action='store_true')
    args = parser.parse_args()
    try:
        boot = None
        if args.boot_args:
            require(args.channels is not None, '--boot-args requires --channels')
            with args.boot_args.open('rb') as source:
                boot = decode_boot_buffers(source.read(0x175))
            arena = boot['buffers']['ipc']
            require(args.iova in (None, arena['iova']) and args.size in (None, arena['size']),
                    'explicit IPC bounds disagree with captured boot record')
            args.iova, args.size = arena['iova'], arena['size']
        require(args.iova is not None and args.size is not None,
                'supply --iova and --size, or --boot-args with --channels')
        with args.capture.open('rb') as source:
            data = source.read(MAX_CHANNELS * DESCRIPTOR_SIZE + 1)
        if args.slot:
            result = decode_slot(data, args.iova, args.size)
        else:
            require(len(data) <= MAX_CHANNELS * DESCRIPTOR_SIZE, "capture exceeds decoder limit")
            result = decode_channels(data, args.channels, args.iova, args.size)
    except (OSError, ProtocolError, struct.error) as error:
        parser.exit(1, f'ANE_IPC_INVALID: {error}\n')
    print(json.dumps({'capture_only': True, 'inference_verified': False,
                      'boot_buffers': boot, 'decoded': result}, indent=2))


if __name__ == '__main__':
    main()
