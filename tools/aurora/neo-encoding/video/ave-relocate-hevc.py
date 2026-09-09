#!/usr/bin/env python3
"""Fixed 320x240 HEVC experiment from the verified 25G76 three-frame capture.

Relocate explicit surface descriptors and the 256-entry tile-address table.
No generic pointer replacement: scalar words resembling addresses are kept.
All DMA references point inside this boot's owned arena. The module stamps
timeout fields. Firmware acceptance and decoded pixels remain runtime gates.
"""
import argparse
import hashlib
import json
from pathlib import Path
import struct

ROOT = Path(__file__).resolve().parents[1]
CAPTURE = ROOT / 'artifacts/ave-hevc-capture-20260908'
OLD_BASE = 0x100002a8000
SURFACE_BYTES = 0x350000
INPUT_OFF = 0x400200
START_FIELDS = [0x48, 0x390, 0x3a0, 0x3b0, 0x3c0, 0x7d0, 0x7e0,
                0x7f0, 0x800, 0xc10, 0xd50, 0xd60, 0xd70, 0xf50,
                0xf60, 0xf70, 0x1130, 0x1140, 0x1350, 0x1390, 0x13d0,
                0x1410, 0x1420, 0x1430, 0x1440, 0x1490, 0x14a0,
                0x14b0, 0x14c0, 0x2520, 0x2530]
HASHES = [
    'd194fe8d8a3a6df358829d6bcd5b43442435210288b92fa58a7730e7fc8152b5',
    '2d83e19ad299d5c296ad79a77bda527961fcf0025ddf2afed3b2ba91786511c3',
    'c88e914e4bedd5b6aabd2fe39dc7c26fe1a4fe9b12b673efe9cddb2fcac1a24a',
    '66620064493af911c9772d1b01cc469effd4d05974dc4719e4950a896a1783eb',
]
p = argparse.ArgumentParser(description=__doc__)
p.add_argument('command', choices=['start', 'process'])
p.add_argument('--arena-dma', type=lambda x: int(x, 0), required=True)
p.add_argument('--arena-bytes', type=lambda x: int(x, 0), required=True)
p.add_argument('--output', type=Path, required=True)
p.add_argument('--client', type=int, default=1)
p.add_argument('--frame-index', type=int, default=0)
a = p.parse_args()
assert 1 <= a.client <= 0xffff and 0 <= a.frame_index < 0x100000
assert a.arena_dma % 0x4000 == 0
assert a.arena_bytes >= INPUT_OFF + 115200 > SURFACE_BYTES
protocol = json.loads((CAPTURE / 'protocol.json').read_text())
assert protocol['transport_validation_passed']
records = [x for x in protocol['pairs'] if x['channel'] == 1 and x['opcode'] in (6, 8)]
assert len(records) == 4
samples = []
for item, expected in zip(records, HASHES):
    sample = (CAPTURE / 'ave-command-buffers' / item['buffer']).read_bytes()
    assert hashlib.sha256(sample).hexdigest() == expected == item['sha256']
    samples.append(sample)
assert len(samples[0]) == 0x13f28
for index, sample in enumerate(samples[1:]):
    assert len(sample) == 0xb1c0
    assert struct.unpack_from('<I', sample, 0x5d90)[0] == (index == 0)
    assert struct.unpack_from('<I', sample, 0xacf8)[0] == index
    assert struct.unpack_from('<Q', sample, 0xad40)[0] == index * 480000
    assert struct.unpack_from('<II', sample, 0xa308) == (76800, 320)
    assert struct.unpack_from('<II', sample, 0xa328) == (38400, 320)
    # This explicit table has 256 addresses, spaced by 1 KiB. Only the
    # entries used by the captured resolution are consumed by firmware;
    # preserve all captured aliases inside the owned scratch allocation.
    for tile in range(256):
        assert struct.unpack_from('<Q', sample, 0xd88 + tile * 8)[0] == 0x100003e0000 + tile * 1024

is_start = a.command == 'start'
packet = bytearray(samples[0 if is_start else 1])
relocations = []


def relocate(off, size, expected=None, delta=None):
    old = struct.unpack_from('<Q', packet, off)[0]
    if expected is not None:
        assert old == expected, (hex(off), hex(old))
    if delta is None:
        delta = old - OLD_BASE
        assert 0 <= delta and delta + size <= SURFACE_BYTES
    assert 0 <= delta and delta + size <= a.arena_bytes
    new = a.arena_dma + delta
    struct.pack_into('<Q', packet, off, new)
    relocations.append(dict(offset=hex(off), old=hex(old), new=hex(new), bytes=size))


if is_start:
    for off in START_FIELDS:
        relocate(off, struct.unpack_from('<I', packet, off + 8)[0])
else:
    for tile in range(256):
        relocate(0xd88 + tile * 8, 1024, 0x100003e0000 + tile * 1024)
    relocate(0xac88, 0x3c000, 0x100002f0000)
    relocate(0xac90, 0xc000, 0x100003b0000)
    relocate(0xa300, 76800, 0x10001ac4200, INPUT_OFF)
    relocate(0xa320, 38400, 0x10001ad6e00, INPUT_OFF + 76800)
    struct.pack_into('<I', packet, 0x5d90, int(a.frame_index == 0))
    struct.pack_into('<I', packet, 0xacf8, a.frame_index)
    struct.pack_into('<Q', packet, 0xad40, a.frame_index * 480000)
struct.pack_into('<QQ', packet, 8, 2 if is_start else 3 + a.frame_index * 2, a.client)
packet[0x30:0x40] = bytes(16)
a.output.write_bytes(packet)
a.output.with_suffix('.json').write_text(json.dumps(dict(
    command=a.command, source_sha256=HASHES[0 if is_start else 1],
    bytes=len(packet), sha256=hashlib.sha256(packet).hexdigest(),
    relocations=relocations, surface_bytes=SURFACE_BYTES, input_offset=INPUT_OFF,
    qualification='Fixed 320x240 profile has native video proof; each generated packet still requires runtime validation'), indent=2) + '\n')
print(f'AVE_HEVC_RELOCATED command={a.command} fields={len(relocations)} bytes={len(packet)}')
