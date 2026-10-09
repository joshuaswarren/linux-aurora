#!/usr/bin/env python3
"""Read and validate this boot's iBoot AVE firmware before native handoff.

Called by boot-linux.py with its existing proxy. Never opens a second client.
It supplies content CRCs to Linux and records SHA-256 provenance on the host.
"""
import hashlib
import os
import json
from pathlib import Path
import struct
import zlib


def prepare_ave_firmware(u, p, iface, output):
    if int(u.adt['/chosen'].chip_id) != 0x8142 or u.adt.model != 'Mac17,3':
        raise RuntimeError('Native AVE preflight requires the M5 J813 target')
    os.umask(0o077)
    node = u.adt['/arm-io/ave']
    raw = bytes(node.getprop('segment-ranges'))
    if (str(node.getprop('segment-names')) != '__TEXT;__DATA' or len(raw) != 64 or
            int(node.getprop('pre-loaded')) != 1):
        raise RuntimeError('Unexpected AVE preloaded firmware description')
    dram = int(u.adt['/chosen'].getprop('dram-base'))
    limit = int(u.ba.phys_base)
    if dram != 0x10000000000 or not dram < limit < dram + 8 * 1024**3:
        raise RuntimeError('Unexpected J813 iBoot reserved memory bounds')
    segments = [struct.unpack_from('<QQQII', raw, i * 32) for i in range(2)]
    expected = [(0, 0x10000000000, 0x14c000, 1),
                (0x14c000, 0x1000014c000, 0x12c000, 0)]
    for (physical, *layout), wanted in zip(segments, expected):
        if tuple(layout) != wanted or physical % 16384 or not dram <= physical < physical + layout[2] <= limit:
            raise RuntimeError('AVE firmware layout/range differs from qualified 25G76')
    if max(segments[0][0], segments[1][0]) < min(
            segments[0][0] + segments[0][3], segments[1][0] + segments[1][3]):
        raise RuntimeError('Overlapping AVE firmware segments')
    output = Path(output)
    output.mkdir(mode=0o700, parents=True, exist_ok=False)
    records = []
    for name, (physical, offset, alias, size, flags) in zip(('text', 'data'), segments):
        payload = iface.readmem(physical, size)
        if len(payload) != size or payload != iface.readmem(physical, size):
            raise RuntimeError('Short or unstable AVE firmware read')
        digest = hashlib.sha256(payload).hexdigest()
        if name == 'text' and digest != 'a6a7728ea8efce2ce6642463a7e5d888de94b7a56e5ab2ae30f03ae8f5d314e1':
            raise RuntimeError('AVE TEXT differs from verified 25G76 firmware')
        if name == 'data' and b'AppleAVE2FW-9003.78.0.debug' not in payload:
            raise RuntimeError('Unexpected AVE firmware version')
        crc = zlib.crc32(payload)
        p.kboot_set_chosen(f'aurora,ave-{name}-crc32', f'{crc:08x}')
        (output / f'ave-{name}.bin').write_bytes(payload)
        records.append(dict(name=name, physical=hex(physical), offset=hex(offset),
                            iova=hex(alias), bytes=size, flags=flags,
                            sha256=digest, crc32=f'{crc:08x}'))
    (output / 'manifest.json').write_text(json.dumps(dict(
        source='Two matching KIS reads of this boot, before native Linux',
        bootargs_phys_base=hex(limit), segments=records), indent=2) + '\n')
    print('AVE_BOOT_FIRMWARE_VERIFIED segments=2 version=9003.78.0', flush=True)
