#!/usr/bin/env python3
"""Prepare an owned-buffer JPEG packet from a matched T8140 reference capture."""
import argparse
import hashlib
import json
from pathlib import Path
import struct

p = argparse.ArgumentParser(description=__doc__)
p.add_argument('capture', type=Path); p.add_argument('reference', type=Path)
p.add_argument('output', type=Path)
p.add_argument('--phase', default='jpeg')
p.add_argument('--register-capture',type=Path,help='Earlier successful reference with identical JPEG register configuration')
p.add_argument('--input-reference',type=Path,required=True,help='Probe directory paired with the DMA input capture')
a = p.parse_args()
events = [json.loads(line) for line in (a.capture/'media-mmio.jsonl').read_text().splitlines()]
ref = json.loads((a.reference/'result.json').read_text())
assert ref['model'] == 'Mac17,5' and '(Build 25G76)' in ref['system_version']
assert ref['passed'] and ref['hardware_required'] and ref['codec'] == 'jpeg'
capture = next(e for e in events if e.get('kind')=='jpeg-input' and e.get('phase')==a.phase)
raw = (a.capture/capture['file']).read_bytes()
assert len(raw) == 320*240*3//2 and hashlib.sha256(raw).hexdigest() == capture['sha256']
source = (a.reference/'input.raw').read_bytes()[:len(raw)]
assert ref['input_pixel_format']=='nv12' and len(source)==len(raw)
# Pair the captured full-range engine input with the exact original source.
# Do not approximate Apple's conversion or generalize it to uncaptured pixels.
input_report=json.loads((a.input_reference/'result.json').read_text())
assert input_report['model']=='Mac17,5' and '25G76' in input_report['system_version']
assert input_report['codec']=='jpeg' and input_report['input_pixel_format']=='nv12'
assert source==(a.input_reference/'input.raw').read_bytes()[:len(raw)]
chroma = int(capture['second_iova'],0)-int(capture['iova'],0)
assert chroma == 320*240
pairs = []; mirror = {}; kicked = False; completion = []
register_events=events if a.register_capture is None else [json.loads(l) for l in (a.register_capture/'media-mmio.jsonl').read_text().splitlines()]
for e in register_events:
    if e.get('kind') != 'mmio' or e.get('node') != 'jpeg0' or e.get('region') != 0: continue
    off, value = int(e['offset'],0), int(e['data'],0)
    if e['op'] == 'W' and off == 0x1004 and value == 1:
        if kicked: break
        if a.register_capture is None: assert e['phase'] == a.phase
        kicked = True
        continue
    if kicked:
        if e['op']=='R': completion.append((off,value))
        continue
    if e['op'] != 'W': continue
    assert e['width'] == 4 and off%4 == 0 and 0 <= off < 0x4000
    assert off != 0x1004 or value == 0
    mirror[off] = value
    # Six 64-bit input/output address pairs are always kernel relocated.
    is_address = 0x90 <= off <= 0xe4 and (off & 0xf) <= 4
    pairs.append((off,0 if is_address else value))
assert kicked and 0 < len(pairs) <= 2048
assert mirror[0x101c] == 320 and mirror[0x1018] == 240
address = lambda off: mirror[off] | mirror[off+4]<<32
# The older cold capture can have different IOVAs. All six pairs relocate.
if a.register_capture is None: assert address(0x90) == int(capture['iova'],0)
assert address(0xa0) == address(0x90)+chroma
assert address(0xb0) == address(0x90)+len(raw)-1
assert address(0xc0) == address(0xd0)
output_size = address(0xe0)+1-address(0xc0)
assert 0 < output_size <= 1024*1024
# Do not submit the candidate until real guest reads establish this status
# contract. An unsupported status ends preparation rather than guessing.
completion=[]; current={};successful=False;matching_kick=False
is_address=lambda o:0x90<=o<=0xe4 and (o&0xf)<=4
for e in events:
    if e.get('kind')!='mmio' or e.get('node')!='jpeg0' or e.get('phase')!=a.phase:continue
    off,value=int(e['offset'],0),int(e['data'],0)
    if e['op']=='W':
        current[off]=value
        if off==0x1004 and value==1:
            matching_kick=all(current.get(o,0)==mirror.get(o,0) for o in current.keys()|mirror.keys()
                if not is_address(o) and o not in (0x1004,0x102c))
    elif matching_kick:
        completion.append((off,value))
        if (off,value)==(0x24,1):successful=True
assert successful, f'No successful JPEG completion with the same configuration: {completion}'
assert any(o==0x1020 and 4 <= v <= output_size for o,v in completion), 'No captured compressed byte count'
header = struct.pack('<6I',0x3147504a,len(pairs),len(raw),chroma,output_size,0)
packet = header+b''.join(struct.pack('<2I',*pair) for pair in pairs)+raw
a.output.mkdir(parents=True,exist_ok=False)
(a.output/'frame-000.bin').write_bytes(packet)
(a.output/'input.nv12').write_bytes(raw)
(a.output/'source-video-range.nv12').write_bytes(source)
metadata = dict(codec='jpeg',model='Mac17,5',build='25G76',width=320,height=240,
    reference=str(a.reference.resolve()),capture=str(a.capture.resolve()),phase=a.phase,
    capture_sha256=capture['sha256'],input_bytes=len(raw),chroma_offset=chroma,
    input_conversion='captured full-range NV12 paired with identical video-range source; no inferred conversion',
    input_reference=str(a.input_reference.resolve()),source_sha256=hashlib.sha256(source).hexdigest(),
    register_capture=str((a.register_capture or a.capture).resolve()),
    output_bytes=output_size,writes=len(pairs),completion_reads=[(hex(o),hex(v)) for o,v in completion],
    packet_sha256=hashlib.sha256(packet).hexdigest(),status='reference validated; Linux untested')
(a.output/'packet.json').write_text(json.dumps(metadata,indent=2)+'\n')
print('JPEG_PACKET_PREPARED',len(packet),'bytes',len(pairs),'register writes')
