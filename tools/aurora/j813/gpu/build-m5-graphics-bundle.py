#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""Build private M5 user-resource firmware from a verified native reference.

Retains GPU virtual addresses and RO/RW permissions, but serializes no captured
physical addresses, page tables, or kernel command/queue objects. The output is
local test input, not evidence that Linux graphics works, and must not go in Git.
"""
import argparse
import hashlib
import importlib.util
import json
from pathlib import Path
import struct

PAGE = 0x4000
PIXELS = 0x10000058000
PIXEL_BYTES = 256 * 256 * 4
WINDOWS = ((0x1000000000, 0x400000), (0x10000000000, 0x400000))
PA_MASK = ((1 << 48) - 1) & ~(PAGE - 1)
PERMISSIONS = {0x80000000000c8b: 1, 0xc0000000000c8b: 3}


def require(condition, message):
    if not condition:
        raise ValueError(message)


def fnv64(data):
    value = 0xcbf29ce484222325
    for byte in data:
        value = ((value ^ byte) * 0x100000001b3) & ((1 << 64) - 1)
    return value


def build(ta, fragment, native_result):
    spec = importlib.util.spec_from_file_location('m5_reference',
        Path(__file__).with_name('verify-m5-graphics-capture.py'))
    verifier = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(verifier)
    verifier.verify(ta, fragment, native_result)
    manifest, objects, _, _ = verifier.load_capture(fragment, 'fragment')
    mappings = sorted(manifest['context_image']['mappings'], key=lambda item: item['address'])
    require(16 <= len(mappings) <= 384, 'Unsupported resource count')
    require(len({item['physical'] for item in mappings}) == len(mappings),
            'Aliased pages require shared backing and are not supported by version 1')
    by_file = {item['file']: data for item, data in objects.values()}
    virtual = {item['address'] for item in mappings}
    physical = {item['physical'] for item in mappings}
    physical_only = physical - virtual
    body = bytearray()
    output_pages = 0
    counts = {1: 0, 3: 0}
    for item in mappings:
        address = item['address']
        require(address % PAGE == 0 and any(start <= address < start + size
                for start, size in WINDOWS), 'Unsupported user GPU address')
        flags = PERMISSIONS.get(item['leaf'] & ~PA_MASK)
        require(flags is not None, 'Unsupported native page permissions')
        data = by_file[item['file']]
        require(len(data) == PAGE, 'Wrong page length')
        # A bounded aligned-word audit catches direct stale pointers. It cannot
        # establish absence of every packed or instruction-encoded pointer.
        for (value,) in struct.iter_unpack('<Q', data):
            require(not (0xfffffc2000000000 <= value < 0xfffffc2100000000
                         or 0x7000000000 <= value < 0x8000000000
                         or value in physical_only),
                    'User resource contains a direct captured kernel/physical pointer')
        if PIXELS <= address < PIXELS + PIXEL_BYTES:
            require(flags == 3 and data == bytes([0xa5]) * PAGE,
                    'Output must be writable and untouched before submission')
            output_pages += 1
        counts[flags] += 1
        body += struct.pack('<QII', address, flags, PAGE) + data
    require(output_pages == 16, 'Missing output page')
    header = struct.pack('<8s8I3Q', b'M5GFXR01', 1, 64, PAGE, len(mappings),
                         256, 256, 1024, 3, PIXELS, fnv64(body), 0)
    result = header + body
    return result, dict(status='M5_PRIVATE_GRAPHICS_BUNDLE_BUILT',
        linux_graphics_proven=False, pages=len(mappings), read_only=counts[1],
        read_write=counts[3], output_pages=output_pages, bytes=len(result),
        sha256=hashlib.sha256(result).hexdigest())


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--ta', type=Path, required=True)
    parser.add_argument('--fragment', type=Path, required=True)
    parser.add_argument('--native-result', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    data, receipt = build(args.ta, args.fragment, args.native_result)
    args.output.write_bytes(data)
    print(json.dumps(receipt, sort_keys=True))
