#!/usr/bin/env python3
"""Validate and stage the user's saved J813 macOS firmware in local artifacts.

Firmware/calibration are private build inputs, never source distribution files.
Every factory tag and both patch bays must match the retained J813 extraction.
"""
from pathlib import Path
import hashlib
import os
import struct
import zipfile

root = Path(__file__).resolve().parent
repo = root.parent
saved = Path('/Users/mcruz/.claude/jobs/39810b8b/tmp/diag/Diagnostics-2026-08-17-00-09-01-750')
raw = repo / 'AuroraSilicon/build/j813-centauri/Firmware/t2026macG1/Release/ftab.bin'
image = (saved / 'synthesized-ftab.bin').read_bytes()
assert hashlib.sha256(image).hexdigest() == 'a58c15b931765e8815e38c0c2aebe54a8b3e5e9348b82ff276917523ca1a518d'

def entries(data):
    assert data[0x24:0x28] == b'ftab'
    count, = struct.unpack_from('<I', data, 0x28)
    assert 0 < count <= 256 and 0x30 + count * 16 <= len(data)
    result = {}
    for i in range(count):
        tag, offset, size, flags = struct.unpack_from('<4sIII', data, 0x30 + i * 16)
        assert tag not in result and offset >= 0x30 + count * 16
        assert offset + size <= len(data) and flags == 0
        result[tag] = data[offset:offset + size]
    return result

factory = entries(raw.read_bytes())
personalized = entries(image)
assert len(factory) == 81 and len(personalized) == 85
assert all(personalized[tag] == data for tag, data in factory.items())
for tag, name in [(b'_apb', 'pb_alpha.bin'), (b'_bpb', 'pb_beta.bin')]:
    assert personalized[tag] == (repo / 'OtherResources/centauri-macos-trace/fdr' / name).read_bytes()
assert personalized[b'_cal'] == (saved / 'cal-data-with-metadata.bin').read_bytes()
manifest_offset, manifest_size = struct.unpack_from('<II', image, 0x10)
with zipfile.ZipFile(repo / 'OS Images/apple/UniversalMac_26.6.1_25G76_Restore.ipsw') as archive:
    manifest = archive.read('Firmware/Manifests/restore/Customer Erase Install (IPSW)/centauri/centauri.j813ap.im4m')
assert len(manifest) == manifest_size
# Both manifests cover identical factory payloads. The captured macOS copy
# differs from the retained IPSW only in its server nonce and signature.
def leaves(data, start, end, path=()):
    result = []
    while start < end:
        tag = data[start]
        start += 1
        number = tag & 31
        if number == 31:
            number = 0
            while True:
                value = data[start]
                start += 1
                number = (number << 7) | (value & 127)
                if not value & 128:
                    break
        size = data[start]
        start += 1
        if size & 128:
            count = size & 127
            assert 0 < count <= 4
            size = int.from_bytes(data[start:start + count], 'big')
            start += count
        assert start + size <= end
        key = path + ((tag >> 6, number),)
        if tag & 32:
            result.extend(leaves(data, start, start + size, key))
        else:
            result.append((key, data[start:start + size]))
        start += size
    assert start == end
    return result

old = leaves(image, manifest_offset, manifest_offset + manifest_size)
new = leaves(manifest, 0, len(manifest))
assert len(old) == len(new)
for (path, before), (other_path, after) in zip(old, new):
    assert path == other_path and len(before) == len(after)
    if before != after:
        assert (3, int.from_bytes(b'srvn', 'big')) in path or path == ((0, 16), (0, 4))
image = image[:manifest_offset] + manifest + image[manifest_offset + manifest_size:]
os.umask(0o077)
target = root / 'artifacts/wifi/firmware/apple/centauri'
target.mkdir(parents=True, exist_ok=True)
(target / 'j813-ftab.bin').write_bytes(image)
print('J813_FIRMWARE_VALIDATED', len(image), 'bytes; 81 factory tags, calibration, patch bays and production manifest matched')
