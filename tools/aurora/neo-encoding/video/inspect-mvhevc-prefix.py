#!/usr/bin/env python3
"""Freeze the observed MV first-pair prefix without claiming a closed capture."""
import argparse
import hashlib
import json
from pathlib import Path
import runpy
import shutil
import struct
import subprocess
import sys

ROOT=Path(__file__).resolve().parents[1]
p=argparse.ArgumentParser(description=__doc__);p.add_argument('capture',type=Path);p.add_argument('output',type=Path)
a=p.parse_args();a.output.mkdir(parents=True,exist_ok=False)
summary=subprocess.run([sys.executable,str(ROOT/'video/summarize-ave-commands.py'),str(a.capture),
    str(a.output/'protocol.json'),'--phase','hevc-multiview'],stdout=subprocess.DEVNULL)
protocol=json.loads((a.output/'protocol.json').read_text())
assert summary.returncode==1 and not protocol['phase_session_closed']
assert not protocol['transport_validation_passed'] and not protocol['unpaired_event_sequences']
assert protocol['ipc_sends']==protocol['matched_sends']==12
assert len(protocol['capture_errors'])==1 and protocol['capture_errors'][0]['kind']=='limit-reached'
records=protocol['pairs'];assert len(records)==12
assert [(q['channel'],q['opcode']) for q in records]==[(1,1),(1,3),(2,1),(2,3),(1,6),(2,6),
    (1,8),(1,8),(2,8),(1,9),(2,8),(2,9)]
buffers=a.output/'ave-command-buffers';buffers.mkdir()
packets=[]
for record in records:
    assert Path(record['buffer']).name==record['buffer']
    data=(a.capture/'ave-command-buffers'/record['buffer']).read_bytes()
    assert len(data)==record['bytes'] and hashlib.sha256(data).hexdigest()==record['sha256']
    (buffers/record['buffer']).write_bytes(data);packets.append(data)
ref=ROOT/'artifacts/coverage-reference-20260909/guest/hevc-multiview'
meta=json.loads((ref/'result.json').read_text())
assert meta['passed'] and meta['hardware_required'] and meta['using_hardware_encoder']
assert meta['model']=='Mac17,5' and '25G76' in meta['system_version'] and meta['encoded_frames']==3
assert meta['input_pixel_format']=='nv12' and meta['configuration']['multiview']
shutil.copytree(ref,a.output/'reference')
first,second=packets[6],packets[7];start=packets[4];old=0x100002a8000
fields=runpy.run_path(str(ROOT/'video/ave-relocate-hevc-precision.py'))['FIELDS']+[
    *range(0x5b0,0x650,0x10),*range(0x9f0,0xa90,0x10),*range(0xda0,0xdf0,0x10),
    *range(0xfa0,0xff0,0x10),*range(0x1240,0x1290,0x10)]
assert len(set(fields))==len(fields)==85
allocations=[];end=0
for offset in sorted(fields):
    pointer,size=struct.unpack_from('<QI',start,offset)
    assert old<=pointer and pointer-old+size<=0x2000000
    end=max(end,pointer-old+size)
    allocations.append(dict(field=hex(offset),offset=hex(pointer-old),bytes=size))
assert end==0x1b4c000
views=[]
for view,data in enumerate([first,second]):
    assert len(data)==0xb1c0 and struct.unpack_from('<I',data,0x20)[0]==0x15+view
    assert all(struct.unpack_from('<I',data,o)[0]==view for o in [0xac84,0xacf8,0xad34])
    assert struct.unpack_from('<Q',data,0xad40)[0]==0
    planes=[]
    for plane,offset in enumerate([0xa300,0xa320]):
        address,size,stride=struct.unpack_from('<QII',data,offset)
        assert size==[76800,38400][plane] and stride==320
        planes.append(dict(plane=plane,reference_iova=hex(address),bytes=size,stride=stride))
    assert not any(data[0xa340:0xa380])
    pointers=[struct.unpack_from('<Q',data,0xd88+i*8)[0] for i in range(256)]
    # MV's tile entries may alias; report their actual layout, never assume
    # the precision encoder's 1024-byte stride without checking every entry.
    views.append(dict(view=view,process_sha256=hashlib.sha256(data).hexdigest(),planes=planes,
        stream_offset=hex(struct.unpack_from('<Q',data,0xac88)[0]-old),
        metadata_offset=hex(struct.unpack_from('<Q',data,0xac90)[0]-old),
        stream_bytes=struct.unpack_from('<I',data,0xac98)[0],
        tile_pointer_offsets=[hex(v-old) for v in pointers],unique_tile_addresses=len(set(pointers))))
report=dict(status='reference prefix inspected',complete_capture=False,native_linux_validated=False,
    capture_limit_reached=True,observed_pairs=len(records),start_sha256=hashlib.sha256(start).hexdigest(),
    typed_start_pointer_count=len(fields),required_scratch_bytes=end,allocations=allocations,views=views,
    preserved_scalar_fields=['0x2580','0x3750','0xdcc0'],
    next_requirement='Paired Process publication; issue each Complete only after its Process reply; verify both decoded eyes before moving-frame work.')
(a.output/'inspection.json').write_text(json.dumps(report,indent=2)+'\n')
events=[json.loads(l) for l in (a.capture/'ave-commands.jsonl').read_text().splitlines()]
(a.output/'phase-events.jsonl').write_text(''.join(json.dumps(e)+'\n' for e in events if e.get('phase')=='hevc-multiview'))
shutil.copyfile(__file__,a.output/'inspect-mvhevc-prefix.py')
files={str(f.relative_to(a.output)):dict(bytes=f.stat().st_size,sha256=hashlib.sha256(f.read_bytes()).hexdigest())
    for f in a.output.rglob('*') if f.is_file()}
(a.output/'manifest.json').write_text(json.dumps(dict(complete_capture=False,native_linux_validated=False,files=files),indent=2)+'\n')
print('MVHEVC_PREFIX_INSPECTED',len(records),'pairs',len(fields),'typed pointers','scratch',hex(end),
    'unique tiles',[v['unique_tile_addresses'] for v in views],'native untested')
