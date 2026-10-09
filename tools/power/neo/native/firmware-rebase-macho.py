#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-2.0-only
"""Make an offline DCP analysis copy with format-7 pointer chains resolved.

Uses VM-to-file mappings for chains; firmware __chain_starts contains VM
offsets, which must not be treated as file offsets across zero-fill gaps.
Never install the output as executable firmware.
"""
import argparse
import hashlib
import json
from pathlib import Path
import struct

p = argparse.ArgumentParser(description=__doc__)
p.add_argument('image', type=Path)
p.add_argument('output', type=Path)
a = p.parse_args()
original = a.image.read_bytes()
b = bytearray(original)
assert struct.unpack_from('<I', b)[0] == 0xfeedfacf
segments, chains = [], None
cursor = 32
for _ in range(struct.unpack_from('<I', b, 16)[0]):
    command, size = struct.unpack_from('<II', b, cursor)
    if command == 0x19:
        va, vsize, off, fsize = struct.unpack_from('<4Q', b, cursor + 24)
        segments.append((va, off, fsize))
        for i in range(struct.unpack_from('<I', b, cursor + 64)[0]):
            s = cursor + 72 + i * 80
            if b[s:s + 16].split(b'\0')[0] == b'__chain_starts':
                chains = struct.unpack_from('<I', b, s + 48)[0]
                b[s:s + 16] = b'__fixed_chains'.ljust(16, b'\0')
    cursor += size
assert chains is not None
base = min(va for va, off, size in segments if size)


def offset(address):
    for va, off, size in segments:
        if va <= address and address + 8 <= va + size:
            return off + address - va
    raise ValueError(f'unbacked chain address {address:#x}')


kind, count = struct.unpack_from('<II', b, chains)
assert kind == 7
seen = set()
for start in struct.unpack_from(f'<{count}I', b, chains + 8):
    address = base + start
    while True:
        assert address not in seen
        seen.add(address)
        off = offset(address)
        raw = struct.unpack_from('<Q', original, off)[0]
        assert not raw & (1 << 62), 'bind pointers unsupported'
        target = raw & (0xffffffff if raw >> 63 else (1 << 43) - 1)
        if not raw >> 63:
            target |= ((raw >> 43) & 0xff) << 56
        struct.pack_into('<Q', b, off, base + target)
        step = ((raw >> 51) & 0x7ff) * 4
        if not step:
            break
        address += step
with a.output.open('xb') as f:
    f.write(b)
a.output.with_suffix('.json').write_text(json.dumps(dict(
    input=str(a.image), input_sha256=hashlib.sha256(original).hexdigest(),
    output_sha256=hashlib.sha256(b).hexdigest(), chains=count,
    pointers=len(seen), base=hex(base)), indent=2) + '\n')
print(f'resolved {len(seen)} pointers')
