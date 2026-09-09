#!/usr/bin/env python3
"""Convert a bounded Neo APR capture into DMA-relocatable Linux test input."""
import argparse
import hashlib
import json
from pathlib import Path
import struct


parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('capture', type=Path)
parser.add_argument('output', type=Path)
parser.add_argument('--prefix', default='apr-01')
parser.add_argument('--warm', action='store_true')
parser.add_argument('--allow-alpha', action='store_true', help='Require a captured alpha plane and relocate it')
parser.add_argument('--plane-prefix', help='Reuse fully captured linear planes with identical source pixels and layout')
parser.add_argument('--plane-capture',type=Path,help='Separate capture holding the identical input planes')
parser.add_argument('--plane-witness',type=Path,help='Saved input.raw from the separate capture probe')
parser.add_argument('--reference', type=Path)
parser.add_argument('--plane-source-reference', type=Path)
args = parser.parse_args()
assert not args.output.exists(), 'Do not overwrite an experiment packet'
events = [json.loads(line) for line in (args.capture / 'media-mmio.jsonl').read_text().splitlines()]
ring = next(e for e in events if e['kind'] == 'apr-ring' and e['capture'] == args.prefix)
raw = (args.capture / ring['file']).read_bytes()
assert len(raw) == ring['size'] == 0x20000
assert hashlib.sha256(raw).hexdigest() == ring['sha256']
head = int(ring['head'], 16) & 0xffffff
tail = int(ring.get('previous_head', hex(head - 0x180)), 16) & 0xffffff
ring_reset = head == 0x180 and tail >= head
if ring_reset:
    tail = 0
assert 0 <= tail < head <= len(raw) and head - tail == 0x180
descriptor = bytearray(raw[tail:head])
assert struct.unpack_from('<HH', descriptor, 0x18) == (320, 240)
version1 = struct.unpack_from('>HH', descriptor, 0x84) == (320, 240)
assert version1 or struct.unpack_from('>HH', descriptor, 0x60) == (320, 240)
flags = struct.unpack_from('<I', descriptor)[0]
assert not flags & (1 << 8) or args.allow_alpha, 'Alpha requires explicit captured-plane selection'
plane_prefix = args.plane_prefix or args.prefix
assert not args.plane_capture or args.plane_prefix
plane_capture = args.plane_capture or args.capture
plane_events = events if plane_capture==args.capture else [json.loads(l) for l in (plane_capture/'media-mmio.jsonl').read_text().splitlines()]
if args.plane_prefix:
    assert args.reference and args.plane_source_reference
    refs = [json.loads((d/'result.json').read_text()) for d in (args.reference,args.plane_source_reference)]
    for ref in refs:
        assert ref['passed'] and ref['hardware_required'] and ref['model']=='Mac17,5' and '25G76' in ref['system_version']
        assert ref['input_pixel_format'] in ('uyvy422','ayuv64le') and (ref['width'],ref['height'])==(320,240)
    assert refs[0]['input_pixel_format']==refs[1]['input_pixel_format']
    input_bytes=153600 if refs[0]['input_pixel_format']=='uyvy422' else 614400
    source_pixels=(args.reference/'input.raw').read_bytes()[:input_bytes]
    assert len(source_pixels)==input_bytes and source_pixels==(args.plane_source_reference/'input.raw').read_bytes()[:input_bytes]
    if plane_capture!=args.capture:
        assert args.plane_witness and args.plane_witness.read_bytes()[:input_bytes]==source_pixels
    source_ring = next(e for e in plane_events if e['kind']=='apr-ring' and e['capture']==plane_prefix)
    source_raw = (plane_capture/source_ring['file']).read_bytes()
    assert hashlib.sha256(source_raw).hexdigest()==source_ring['sha256']
    source_head=int(source_ring['head'],0)&0xffffff
    source_desc=source_raw[source_head-384:source_head]
    assert struct.unpack_from('<I',source_desc)[0]&0x1ff == flags&0x1ff
    assert source_desc[0x18:0x26] == descriptor[0x18:0x26]
    assert all(struct.unpack_from('<Q',d,slot)[0]==0 for d in (source_desc,descriptor) for slot in (0x30,0x40,0x50))
plane_records = [e for e in plane_events if e['kind'] == 'apr-plane' and e['capture'] == plane_prefix]
planes, relocations = [], [(8, 0, 0)]
reconstructed_chroma_rows = 0
for i, (name, slot) in enumerate([('luma', 0x28), ('chroma', 0x38), ('alpha', 0x48)]):
    if name == 'alpha' and not flags & (1 << 8):
        planes.append(b'')
        continue
    addresses = [struct.unpack_from('<Q', descriptor, slot + delta)[0] for delta in (0, 8)]
    if not any(addresses):
        planes.append(b'')
        continue
    record = next((e for e in plane_records if e['plane'] == name), None)
    if record is not None:
        payload = (plane_capture / record['file']).read_bytes()
        assert len(payload) == record['size'] and len(payload) <= 1024 * 1024
        assert hashlib.sha256(payload).hexdigest() == record['sha256']
        base = int(record.get('buffer_iova', record['iova']), 16)
        if args.plane_prefix:
            assert base == int(record['iova'],16) and not int(record.get('header_iova','0'),16)
            base = addresses[0]
    else:
        # The original bounded capture rounded the chroma read past the last
        # mapped page and correctly refused it. Its first 16 complete rows are
        # present in the luma read's padding. This particular test image has
        # constant rows: verify the observed rows before reconstructing them.
        assert name == 'chroma' and not addresses[1]
        luma = next(e for e in plane_records if e['plane'] == 'luma')
        captured = (plane_capture / luma['file']).read_bytes()
        assert hashlib.sha256(captured).hexdigest() == luma['sha256']
        base = addresses[0]
        start = base - int(luma.get('buffer_iova', luma['iova']), 16)
        stride = struct.unpack_from('<H', descriptor, 0x22)[0] * 64
        assert 0 < start < len(captured) and stride == 640
        rows = captured[start:]
        assert len(rows) >= 16 * stride and len(rows) % stride == 0
        assert all(rows[o:o+stride] == rows[:stride] for o in range(0, len(rows), stride))
        reconstructed_chroma_rows = len(rows) // stride
        payload = rows[:stride] * 240
    for delta, address in zip((0, 8), addresses):
        if address:
            assert base <= address < base + len(payload)
            relocations.append((slot + delta, i + 1, address - base))
    planes.append(payload)
# Every known address is erased; the kernel substitutes only owned IOVAs.
if version1:
    assert all(struct.unpack_from('<Q', descriptor, slot)[0] == 0 for slot in (0x58, 0x60, 0x68, 0x70)), 'Statistics buffers need their own capture'
for slot in (8, 0x28, 0x30, 0x38, 0x40, 0x48, 0x50):
    struct.pack_into('<Q', descriptor, slot, 0)
first_kick = next(e['seq'] for e in events if e.get('node') == 'apr' and
                  e.get('region') == 0 and e.get('offset') == '0x10c' and int(e['data'], 16))
writes = []
if not args.warm:
    for event in events:
        if event.get('node') != 'apr' or event['seq'] >= first_kick:
            continue
        assert event['width'] == 4 and event['op'] == 'W' and event['region'] in (0, 1)
        offset = int(event['offset'], 16)
        assert offset <= (0x19fc if event['region'] == 0 else 0x7ffc)
        writes.append((offset | event['region'] << 16, int(event['data'], 16)))
    assert 0 < len(writes) <= 2048
header = struct.pack('<8I', 0x31525041 if version1 else 0x30525041, len(descriptor), len(writes), len(relocations),
                     *(len(p) for p in planes), 0)
packet = header + b''.join(struct.pack('<II', *w) for w in writes) + descriptor
packet += b''.join(struct.pack('<III', *r) for r in relocations) + b''.join(planes)
args.output.parent.mkdir(parents=True, exist_ok=True)
args.output.write_bytes(packet)
metadata = {'source': str(args.capture), 'capture': args.prefix, 'descriptor_version': int(version1),
            'ring_reset_inferred_from_first_entry':ring_reset, 'plane_capture':plane_prefix,
            'plane_capture_directory':str(plane_capture),
            'plane_witness':str(args.plane_witness) if args.plane_witness else None,
            'plane_witness_sha256':hashlib.sha256(args.plane_witness.read_bytes()).hexdigest() if args.plane_witness else None,
            'plane_records':[{k:e[k] for k in ('plane','file','size','sha256')} for e in plane_records],
            'identical_source_reference':str(args.plane_source_reference) if args.plane_prefix else None,
            'chroma_reconstructed_from_verified_constant_rows': reconstructed_chroma_rows,
            'register_writes': len(writes), 'descriptor_bytes': len(descriptor),
            'plane_bytes': [len(p) for p in planes], 'relocations': relocations,
            'flags': hex(flags), 'strides': [struct.unpack_from('<H', descriptor, 0x20 + 2*i)[0]*64 for i in range(3)],
            'packet_bytes': len(packet), 'sha256': hashlib.sha256(packet).hexdigest()}
args.output.with_suffix('.json').write_text(json.dumps(metadata, indent=2) + '\n')
print(json.dumps(metadata))
