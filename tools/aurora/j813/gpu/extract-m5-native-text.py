#!/usr/bin/env python3
"""Extract native __text sections from nested Metal pipeline Mach-O objects."""
from pathlib import Path
import hashlib
import json
import struct
import sys

source, out = map(Path, sys.argv[1:])
data = source.read_bytes()
out.mkdir(exist_ok=False)
result = []
base = 0
while True:
    base = data.find(b'\xcf\xfa\xed\xfe', base)
    if base < 0:
        break
    try:
        magic, cpu, subtype, kind, count, cmdsize, flags, reserved = struct.unpack_from('<8I', data, base)
        pos = base + 32
        assert count < 1000 and pos + cmdsize <= len(data)
        for _ in range(count):
            command, length = struct.unpack_from('<II', data, pos)
            assert length >= 8 and pos + length <= base + 32 + cmdsize
            if command == 0x19:
                nsects = struct.unpack_from('<I', data, pos + 64)[0]
                assert 72 + nsects * 80 <= length
                for n in range(nsects):
                    section = pos + 72 + 80 * n
                    name = data[section:section + 16].rstrip(b'\0')
                    address, size, offset = struct.unpack_from('<QQI', data, section + 32)
                    if name != b'__text':
                        continue
                    assert size > 0 and base + offset + size <= len(data)
                    text = data[base + offset:base + offset + size]
                    path = out / f'text-{len(result):02d}.bin'
                    path.write_bytes(text)
                    result.append(dict(file=path.name, object_offset=base, text_offset=offset,
                                       address=address, size=size, sha256=hashlib.sha256(text).hexdigest()))
            pos += length
    except (AssertionError, struct.error):
        pass
    base += 4
manifest = dict(source_sha256=hashlib.sha256(data).hexdigest(), sections=result)
(out / 'manifest.json').write_text(json.dumps(manifest, indent=2) + '\n')
print(json.dumps(manifest, indent=2))
