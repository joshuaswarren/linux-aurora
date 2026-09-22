#!/usr/bin/env python3
"""Freeze one closed AVE phase with checked commands and optional input planes."""
import argparse
import hashlib
import json
from pathlib import Path
import shutil
import struct
import subprocess
import sys

p=argparse.ArgumentParser(description=__doc__)
p.add_argument('capture',type=Path);p.add_argument('phase');p.add_argument('output',type=Path)
p.add_argument('--require-inputs',action='store_true')
a=p.parse_args();a.output.mkdir(parents=True,exist_ok=False)
subprocess.run([sys.executable,str(Path(__file__).with_name('summarize-ave-commands.py')),
    str(a.capture),str(a.output/'protocol.json'),'--phase',a.phase],check=True,stdout=subprocess.DEVNULL)
protocol=json.loads((a.output/'protocol.json').read_text())
assert protocol['transport_validation_passed'] and protocol['phase_session_closed']
events=[json.loads(l) for l in (a.capture/'ave-commands.jsonl').read_text().splitlines()]
events=[e for e in events if e.get('phase')==a.phase]
planes=[e for e in events if e['kind']=='input-plane']
errors=[e for e in events if e['kind']=='input-capture-error']
buffers=a.output/'ave-command-buffers';buffers.mkdir()
def copy_buffer(name,size,digest):
    assert Path(name).name==name
    data=(a.capture/'ave-command-buffers'/name).read_bytes()
    assert len(data)==size and hashlib.sha256(data).hexdigest()==digest
    (buffers/name).write_bytes(data)
    return data
commands={}
for item in protocol['pairs']:
    commands[item['ipc_seq']]=copy_buffer(item['buffer'],item['bytes'],item['sha256'])
for e in planes:copy_buffer(e['file'],e['size'],e['sha256'])
processes=[q for q in protocol['pairs'] if q['channel']==1 and q['opcode']==8]
checked=[]
if a.require_inputs:
    assert not errors and processes, 'Input snapshots are incomplete'
    for item in processes[:2]:
        data=commands[item['ipc_seq']];assert len(data)==0xb1c0
        required=[]
        for plane,offset in enumerate([0xa300,0xa320,0xa340,0xa360]):
            address,size,stride=struct.unpack_from('<QII',data,offset)
            if not(address or size or stride):continue
            assert address>=0x10000000000 and 0<size<=1024*1024 and 0<stride<=8192
            found=[e for e in planes if e['command_seq']==item['ipc_seq'] and e['plane']==plane]
            assert len(found)==1, 'Missing or duplicated captured input plane'
            e=found[0]
            assert (int(e['iova'],16),e['size'],e['stride'],e['stream'])==(address,size,stride,0)
            required.append(plane)
        assert required
        checked.append(dict(command_seq=item['ipc_seq'],planes=required))
(a.output/'input-planes.json').write_text(json.dumps(dict(phase=a.phase,planes=planes,
    capture_errors=errors,checked_processes=checked,inputs_required=a.require_inputs,
    native_linux_validated=False),indent=2)+'\n')
(a.output/'phase-events.jsonl').write_text(''.join(json.dumps(e)+'\n' for e in events))
shutil.copyfile(__file__,a.output/'freeze-ave-phase.py')
files={str(q.relative_to(a.output)):dict(bytes=q.stat().st_size,
    sha256=hashlib.sha256(q.read_bytes()).hexdigest()) for q in a.output.rglob('*') if q.is_file()}
(a.output/'files.json').write_text(json.dumps(files,indent=2)+'\n')
print('AVE_PHASE_FROZEN',a.phase,'commands',len(commands),'input_planes',len(planes),
    'checked_processes',len(checked))
