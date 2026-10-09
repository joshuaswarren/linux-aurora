#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-2.0-only
"""Resolve selected arm64e fileset vtable slots using an explicit KC fixup base.

For locally extracted Apple kernelcache binaries with authenticated offset
pointers. This is not a general Mach-O chained-fixup decoder. Retain raw slots
and binary identity, and cross-check resolved symbols against disassembly.
"""
import argparse
import hashlib
import json
from pathlib import Path
import struct
import subprocess

p = argparse.ArgumentParser(description=__doc__)
p.add_argument('binary', type=Path)
p.add_argument('--symbol', required=True, help='exact llvm-nm vtable symbol')
p.add_argument('--kc-base', type=lambda x: int(x, 0), required=True)
p.add_argument('--offset', type=lambda x: int(x, 0), action='append', required=True,
               help='offset from object vptr (vtable symbol + 16); repeatable')
p.add_argument('--output', type=Path, required=True)
a = p.parse_args()
b = a.binary.read_bytes()
if len(b) < 32 or struct.unpack_from('<I', b)[0] != 0xfeedfacf:
    p.error('expected little-endian 64-bit Mach-O')
segments = []
position = 32
for _ in range(struct.unpack_from('<I', b, 16)[0]):
    command, size = struct.unpack_from('<II', b, position)
    if size < 8 or position + size > len(b):
        p.error('invalid load command')
    if command == 0x19:
        va, vsize, fileoff, filesize = struct.unpack_from('<QQQQ', b, position + 24)
        if fileoff + filesize > len(b):
            p.error('segment outside file')
        segments.append((va, fileoff, filesize))
    position += size
symbols = {}
for line in subprocess.check_output(['llvm-nm', str(a.binary)], text=True).splitlines():
    fields = line.split()
    if len(fields) < 3:
        continue
    try:
        address = int(fields[0], 16)
    except ValueError:
        continue
    symbols.setdefault(address, []).append(fields[-1])
matches = [address for address, names in symbols.items() if a.symbol in names]
if len(matches) != 1:
    p.error('vtable symbol must resolve exactly once')
slots = []
for offset in a.offset:
    if offset < 0 or offset % 8:
        p.error('offsets must be non-negative multiples of eight')
    va = matches[0] + 16 + offset
    regions = [(start, fileoff) for start, fileoff, size in segments
               if start <= va and va + 8 <= start + size]
    if len(regions) != 1:
        p.error('slot must be in one file-backed segment')
    start, fileoff = regions[0]
    raw = struct.unpack_from('<Q', b, fileoff + va - start)[0]
    if not raw & (1 << 63):
        p.error(f'slot {offset:#x} is not an authenticated offset pointer')
    target = a.kc_base + (raw & 0xffffffff)
    slots.append(dict(offset=hex(offset), raw=hex(raw), target=hex(target),
                      symbols=symbols.get(target, [])))
result = dict(binary=str(a.binary), sha256=hashlib.sha256(b).hexdigest(),
              vtable_symbol=a.symbol, kc_base=hex(a.kc_base), slots=slots)
with a.output.open('x') as output:
    json.dump(result, output, indent=2)
    output.write('\n')
print(json.dumps(result, indent=2))
