#!/usr/bin/env python3
"""Read-only disassembly of the pinned M5 kernel's virtual address ranges.

Run with the AuroraDbg virtual environment's Python. Each argument is
START:SIZE, with Python-style integer prefixes. No hardware is accessed.
"""
import argparse
import hashlib
from pathlib import Path
import struct

from capstone import Cs, CS_ARCH_ARM64, CS_MODE_LITTLE_ENDIAN


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('ranges', nargs='+')
    args = parser.parse_args()
    path = Path(__file__).resolve().parent / (
        'kernel/25G76__Mac17,3/kernelcache.release.Mac17,2_3_4')
    data = path.read_bytes()
    if hashlib.sha256(data).hexdigest() != (
            '43263b4f4b7cd29cb0cc487ba944b5bf0a8f3b24b8b1cad875b796d9af026189'):
        raise RuntimeError('M5 kernel hash mismatch')
    segments = []
    offset = 32
    for _ in range(struct.unpack_from('<I', data, 16)[0]):
        command, size = struct.unpack_from('<II', data, offset)
        if command == 0x19:
            va, _, file_offset, file_size = struct.unpack_from('<4Q', data, offset + 24)
            segments.append((va, file_offset, file_size))
        offset += size
    disassembler = Cs(CS_ARCH_ARM64, CS_MODE_LITTLE_ENDIAN)
    for item in args.ranges:
        start, size = (int(value, 0) for value in item.split(':'))
        if size <= 0 or size > 0x10000 or (start | size) & 3:
            parser.error('ranges must be instruction-aligned and at most 64 KiB')
        matches = [(va, fo) for va, fo, fs in segments
                   if va <= start and start + size <= va + fs]
        if len(matches) != 1:
            parser.error('range must lie inside one file-backed segment')
        va, fo = matches[0]
        begin = fo + start - va
        print(f'RANGE {start:#x}:{size:#x}')
        for instruction in disassembler.disasm(data[begin:begin + size], start):
            print(f'{instruction.address:#x} {instruction.mnemonic} {instruction.op_str}')


if __name__ == '__main__':
    main()
