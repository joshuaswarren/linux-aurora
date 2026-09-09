#!/usr/bin/env python3
"""Create a private ELF analysis image from N1's embedded fwsg segment map.

This is an analysis artifact only. It is never used as a boot payload.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import plistlib
import struct


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--image', choices=('aciw', 'wftp', 'w2lp', 'w5lp'), default='aciw')
    args = parser.parse_args()
    specifications = {
        'aciw': ('48ef6696bfc4f4ec3b229d4f9782618d48143a7916c897b4b059b87036d8197c',
                 'UMAC', 37, 2425, 36),
        'wftp': ('6d05b7d5d1e16742478e2155ef87130e21bf91fe0bf4355f0ee5e7d827bf954f',
                 'TX', 3, 958, 0),
        'w2lp': ('4e9127c08ff9bc85cc2cfe4f9bd1fa53317bf200f5a7ccc59e22fd1776306b79',
                 'LMAC2G', 8, 1529, 0),
        'w5lp': ('10ec328548076204b356ac386aa870a91894cd0026e3321bb163f8bf671a74c5',
                 'LMAC5G', 8, 1529, 0),
    }
    digest, processor, segment_count, assertion_count, rom_count = specifications[args.image]
    root = Path(__file__).resolve().parent.parent
    directory = root / 'AuroraSilicon/build/j813-centauri/re/firmware'
    image = (directory / f'{args.image}.bin').read_bytes()
    assert hashlib.sha256(image).hexdigest() == digest, (
        'Verify the source image before mapping a different firmware revision')
    footer = len(image) - 32
    magic, version, table, count, *reserved = struct.unpack_from('<8I', image, footer)
    assert magic == 0x67737766 and version == 1 and not any(reserved)
    assert count == segment_count and table + count * 32 == footer
    segments = [struct.unpack_from('<Q4I8s', image, table + 32 * index)
                for index in range(count)]
    previous_end = 0
    for address, offset, file_size, memory_size, flags, name in segments:
        assert address >= previous_end and address + memory_size <= 1 << 32
        assert file_size <= memory_size and offset + file_size <= table
        previous_end = address + memory_size

    catalog = plistlib.loads((directory / '_wfa.plist').read_bytes())[processor]
    matched = 0
    rom_only = 0
    for text_address, item in catalog.items():
        address = int(text_address, 16)
        segment = next((s for s in segments if s[0] <= address < s[0] + s[2]), None)
        if segment is None:
            assert args.image == 'aciw' and 0x600000 <= address < 0x601000
            rom_only += 1
            continue
        offset = segment[1] + address - segment[0]
        if item['function'] == 'UMAC_CrashHandler':
            assert address == 0x100863
            continue  # Catalog entry is a Thumb function pointer, not UDF.
        assert image[offset:offset + 2] == b'\0\xde', text_address
        matched += 1
    assert matched == assertion_count and rom_only == rom_count

    # ELF32 little-endian ARM EABI5. Preserve the Thumb reset-vector entry.
    entry = struct.unpack_from('<I', image, 4)[0]
    assert entry & 1
    header = struct.pack('<16sHHIIIIIHHHHHH',
                         b'\x7fELF\x01\x01\x01' + bytes(9),
                         2, 40, 1, entry, 52, 0, 0x05000000,
                         52, 32, count, 0, 0, 0)
    file_base = 4096
    programs = bytearray()
    for address, offset, file_size, memory_size, flags, name in segments:
        permissions = 5 if flags & 4 else 6
        programs += struct.pack('<8I', 1, file_base + offset if file_size else 0,
                                address, address, file_size, memory_size, permissions, 1)
    assert len(header) + len(programs) <= file_base
    mapped = header + programs
    mapped += bytes(file_base - len(mapped)) + image
    destination = directory / f'{args.image}-mapped.elf'
    os.umask(0o077)
    destination.write_bytes(mapped)
    destination.chmod(0o600)
    symbols = directory / f'{args.image}-assertions.json'
    symbols.write_text(json.dumps(catalog, sort_keys=True) + '\n')
    symbols.chmod(0o600)
    print(f'{destination.name}: {count} segments; {matched} assertion sites verified; '
          f'{rom_only} ROM-only sites excluded')
    print('SHA256', hashlib.sha256(mapped).hexdigest())


if __name__ == '__main__':
    main()
