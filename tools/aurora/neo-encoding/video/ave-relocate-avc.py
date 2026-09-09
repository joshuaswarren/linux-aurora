#!/usr/bin/env python3
"""Bounded first-frame experiment from the hash-verified 25G76 AVC capture.

Only explicit surface fields are relocated. No generic pointer scanning or
runtime-address replay. Firmware acceptance and an independently decoded
output frame are separate qualification gates.
"""
import argparse
import hashlib
import json
from pathlib import Path
import struct

ROOT = Path(__file__).resolve().parents[1]
CAPTURE = ROOT / 'artifacts/ave-capture-20260908/three-frames-rings'
OLD_BASE = 0x100002a8000
SURFACE_BYTES = 0x3bc000
INPUT_OFF = 0x400200
START_FIELDS = [0x48, 0x390, 0x3a0, 0x3b0, 0x3c0, 0x7d0, 0x7e0,
                0x7f0, 0x800, 0xc10, 0xd50, 0xd60, 0xd70, 0xf50,
                0xf60, 0xf70, 0x1130, 0x1140, 0x1350, 0x1390, 0x13d0,
                0x1410, 0x1420, 0x1430, 0x1440, 0x1490, 0x14a0,
                0x14b0, 0x14c0, 0x1510, 0x1520, 0x1530, 0x1540,
                0x1590, 0x15a0, 0x15b0, 0x15c0]

p = argparse.ArgumentParser()
p.add_argument('command', choices=['start', 'process'])
p.add_argument('--arena-dma', type=lambda x: int(x, 0), required=True)
p.add_argument('--arena-bytes', type=lambda x: int(x, 0), required=True)
p.add_argument('--output', type=Path, required=True)
p.add_argument('--client', type=int, default=1)
p.add_argument('--pattern', choices=['gray', 'color-bars'], default='gray')
p.add_argument('--frame-index', type=int, default=0)
a = p.parse_args()
assert 1 <= a.client <= 0xffff
assert 0 <= a.frame_index < 0x100000
assert a.arena_dma % 0x4000 == 0
assert a.arena_bytes >= INPUT_OFF + 320 * 240 * 3 // 2
protocol = json.loads((CAPTURE / 'protocol.json').read_text())
opcode = 6 if a.command == 'start' else 8
record = next(r for r in protocol['pairs'] if r['opcode'] == opcode and r['channel'] == 1)
packet = bytearray((CAPTURE / 'ave-command-buffers' / record['buffer']).read_bytes())
assert hashlib.sha256(packet).hexdigest() == record['sha256']
assert len(packet) == (0x3180 if opcode == 6 else 0x63d8)
relocations = []
for off in START_FIELDS if opcode == 6 else [0x61b8, 0x61c0]:
    old = struct.unpack_from('<Q', packet, off)[0]
    delta = old - OLD_BASE
    assert 0 <= delta < SURFACE_BYTES, (hex(off), hex(old))
    if opcode == 6:
        size = struct.unpack_from('<I', packet, off + 8)[0]
        assert delta + size <= SURFACE_BYTES
    new = a.arena_dma + delta
    struct.pack_into('<Q', packet, off, new)
    relocations.append(dict(offset=hex(off), old=hex(old), new=hex(new)))
if opcode == 8:
    for off, delta, old in [(0x5830, INPUT_OFF, 0x10001b30200),
                            (0x5850, INPUT_OFF + 320 * 240, 0x10001b42e00)]:
        assert struct.unpack_from('<Q', packet, off)[0] == old
        new = a.arena_dma + delta
        struct.pack_into('<Q', packet, off, new)
        relocations.append(dict(offset=hex(off), old=hex(old), new=hex(new)))
    assert struct.unpack_from('<II', packet, 0x5838) == (76800, 320)
    assert struct.unpack_from('<II', packet, 0x5858) == (38400, 320)
# New local request/client tokens, preserving captured scalar AVC policy.
struct.pack_into('<QQ', packet, 8, 2 if opcode == 6 else 3, a.client)
packet[0x30:0x40] = bytes(16)  # module supplies this boot's timeout clock
if opcode == 8:
    # Comparing all three captured Process packets proves the first-frame
    # flag, frame ordinal and presentation counter fields at captured 30 fps. Verify
    # these offsets against frame 1/2 before generating later ordinals.
    recorded = [x for x in protocol['pairs'] if x['opcode'] == 8 and x['channel'] == 1]
    for index, item in enumerate(recorded):
        sample = (CAPTURE / 'ave-command-buffers' / item['buffer']).read_bytes()
        assert hashlib.sha256(sample).hexdigest() == item['sha256']
        assert struct.unpack_from('<I', sample, 0x12c0)[0] == (index == 0)
        assert struct.unpack_from('<I', sample, 0x6228)[0] == index
        assert struct.unpack_from('<Q', sample, 0x6270)[0] == index * 480000
    struct.pack_into('<I', packet, 0x12c0, int(a.frame_index == 0))
    struct.pack_into('<I', packet, 0x6228, a.frame_index)
    struct.pack_into('<Q', packet, 0x6270, a.frame_index * 480000)
    struct.pack_into('<Q', packet, 8, 3 + a.frame_index * 2)
a.output.write_bytes(packet)
if opcode == 8:
    if a.pattern == 'gray':
        frame = bytes([80]) * (320 * 240) + bytes([128]) * (320 * 240 // 2)
    else:
        y = [235, 210, 170, 145, 106, 81, 41, 16]
        u = [128, 16, 166, 54, 202, 90, 240, 128]
        v = [128, 146, 16, 34, 222, 240, 110, 128]
        yrow = b''.join(bytes([c]) * 40 for c in y)
        uvrow = b''.join(bytes([cu, cv]) * 20 for cu, cv in zip(u, v))
        frame = yrow * 240 + uvrow * 120
    a.output.with_suffix('.nv12').write_bytes(frame)
a.output.with_suffix('.json').write_text(json.dumps(dict(
    command=a.command, source_sha256=record['sha256'],
    bytes=len(packet), sha256=hashlib.sha256(packet).hexdigest(),
    relocations=relocations, surface_bytes=SURFACE_BYTES, input_offset=INPUT_OFF,
    warning='Fixed 320x240 profile has native video proof; each generated packet still requires runtime validation'), indent=2) + '\n')
print(f'AVE_AVC_RELOCATED command={a.command} fields={len(relocations)} bytes={len(packet)}')
