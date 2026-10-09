#!/usr/bin/env python3
"""Run bounded paired-eye AVE diagnostics; qualify decoded output separately."""
import argparse
from collections import Counter
import hashlib
import json
from pathlib import Path
import re
import runpy
import shutil
import struct
import subprocess
import sys
import tarfile

ROOT=Path(__file__).resolve().parents[1];PYTHON=ROOT.parent/'neo-update/.venv/bin/python'
DEBUG='/sys/kernel/debug/285100000.video-codec'
r=runpy.run_path(str(ROOT/'video/ave-relocate-mvhevc.py'))
REGIONS=[(0x168000,0x3c000),(0x3e8000,0xc000),(0x188c000,1024),
         (0x1a8000,0x3c000),(0x3f8000,0xc000),(0x1894000,1024)]
p=argparse.ArgumentParser(description=__doc__);p.add_argument('output',type=Path)
p.add_argument('--frames',type=int,default=1);p.add_argument('--client',type=int)
p.add_argument('--module-binary',type=Path,required=True)
p.add_argument('--pixel-format',choices=['nv12'],default='nv12');p.add_argument('--bit-depth',type=int,choices=[8],default=8)
p.add_argument('--alpha',choices=['none'],default='none')
p.add_argument('--prepare-only',action='store_true');p.add_argument('--validate-only',action='store_true')
p.add_argument('--arena-dma',type=lambda x:int(x,0));p.add_argument('--arena-bytes',type=lambda x:int(x,0))
a=p.parse_args();assert 1<=a.frames<=60 and not(a.prepare_only and a.validate_only)
out=a.output.resolve()
if a.validate_only:assert out.is_dir()
else:out.mkdir(parents=True,exist_ok=False)
module_sha=hashlib.sha256(a.module_binary.read_bytes()).hexdigest()

def run(command,name):
    with (out/(name+'.log')).open('w') as log:
        result=subprocess.run([str(x) for x in command],stdout=log,stderr=subprocess.STDOUT)
    if result.returncode:raise RuntimeError(name+' failed; inspect evidence, no automatic firmware retry')
    return (out/(name+'.log')).read_text(errors='replace')
def native(command,name,timeout=30):return run([PYTHON,ROOT/'console.py','--timeout',timeout,command],name)
def replies(text):
    part=text.split('\nAVE_REPLY_BEGIN\n',1)[1].split('\nAVE_REPLY_END\n',1)[0]
    data=bytes.fromhex(' '.join(l.strip() for l in part.splitlines() if re.fullmatch(r'(?: [0-9a-f]{2})+',l)))
    values=[]
    while data:
        channel,size,flags,reserved,address=struct.unpack_from('<IIIIQ',data)
        assert channel==2 and size in (72,80) and reserved==0 and len(data)>=24+size
        values.append(data[24:24+size]);data=data[24+size:]
    return values

if not a.validate_only:
    if a.prepare_only:
        assert a.arena_dma is not None and a.arena_bytes is not None
        dma,size,client=a.arena_dma,a.arena_bytes,a.client or 1
    else:
        text=native(f'test -w {DEBUG}/ave-command-pair && cat {DEBUG}/ave-status; '
            f'printf "AVE_REPLY_BEGIN\\n"; od -An -v -tx1 {DEBUG}/ave-command; printf "AVE_REPLY_END\\n"','initial-state')
        state=dict(re.findall(r'^([a-z0-9_]+)=([^\r\n]+)',text,re.M))
        assert state['ipc_ready']=='1' and state['command_failed']=='0' and state['ipc_bytes']=='33554432'
        last=replies(text);assert len(last)==1 and struct.unpack_from('<I',last[0])[0] in (1,4)
        client=a.client or struct.unpack_from('<Q',last[0],0x10)[0]+1
        dma,size=int(state['arena_dma'],0),int(state['arena_bytes'],0)
        check=native('sha256sum /tmp/apple-ave.ko','module-identity')
        assert re.search(r'^'+module_sha+r'\s+/tmp/apple-ave.ko$',check,re.M)
    payload=out/'payload';payload.mkdir();shutil.copyfile(ROOT/'video/linux-ave/run-command.sh',payload/'run.sh')
    source=[];manifest=json.loads((r['CAPTURE']/'manifest.json').read_text())
    for name in ['input.raw','input-right.raw']:
        content=(r['CAPTURE']/'reference'/name).read_bytes();record=manifest['files']['reference/'+name]
        assert len(content)==3*r['FRAME_BYTES']==record['bytes'] and hashlib.sha256(content).hexdigest()==record['sha256']
        source.append(content[:r['FRAME_BYTES']])
    assert source[0]!=source[1]
    def scalar(name,kind,request):
        run([sys.executable,ROOT/'video/ave-make-command.py',kind,payload/(name+'.bin'),
            '--codec','hevc','--client',client,'--request-id',request],'make-'+name)
    scalar('open','open',1);scalar('stop','stop',3+4*a.frames);scalar('close','close',4+4*a.frames)
    images=[[],[]]
    for frame in range(a.frames):
        for kind in (['start','pair'] if frame==0 else ['pair']):
            packet,metadata=r['relocate'](kind,dma,size,client,frame)
            name='start' if kind=='start' else f'pair-{frame:03d}'
            (payload/(name+'.bin')).write_bytes(packet);(payload/(name+'.json')).write_text(json.dumps(metadata,indent=2)+'\n')
        shift=frame*8%320;planes=[]
        for view in range(2):
            image=b''.join(source[view][y*320+shift:(y+1)*320]+source[view][y*320:y*320+shift] for y in range(360))
            images[view].append(image);planes.append(image)
        (payload/f'input-{frame:03d}.nv12').write_bytes(b''.join(planes))
    for view in range(2):(out/f'input-view-{view}.nv12').write_bytes(b''.join(images[view]))
    meta=dict(codec='MV-HEVC',frames=a.frames,client=client,arena_dma=dma,arena_bytes=size,
        input_offset=r['INPUT_OFFSET'],scratch_bytes=r['SCRATCH_BYTES'],regions=REGIONS,
        module_sha256=module_sha,frame_policy='forced keyframes in both layers',native_validated=False)
    (out/'run.json').write_text(json.dumps(meta,indent=2)+'\n')
    archive=out/'payload.tar.gz'
    with tarfile.open(archive,'w:gz') as tar:
        for f in sorted(payload.iterdir()):tar.add(f,arcname=f.name)
    if a.prepare_only:print('MVHEVC_PREPARED',a.frames,out);raise SystemExit(0)
    remote='/tmp/mvhevc-'+str(client)
    run([PYTHON,ROOT/'push-linux.py',archive,remote+'.tar.gz'],'upload')
    pages=(r['SCRATCH_BYTES']+16383)//16384
    native(f'mkdir {remote} && tar -xzf {remote}.tar.gz -C {remote} && '
        f'dd if=/dev/zero of={DEBUG}/ave-arena bs=16384 count={pages} conv=notrunc','prepare')
    def submit(name,pair=False,opcode=0):
        result=replies(native(f'sh {remote}/run.sh {remote}/{name}.bin'+(' pair' if pair else ''),name))
        counts=Counter()
        for data in result:
            op,sub=struct.unpack_from('<I',data)[0],struct.unpack_from('<I',data,0x20)[0]
            words=struct.unpack('<'+'I'*(len(data)//4),data);expected=[0]*len(words)
            expected[0],expected[4],expected[8]=op,client,sub;assert list(words)==expected
            counts[op,sub]+=1
        assert counts==(Counter({(8,0x15):1,(8,0x16):1,(9,9):2}) if pair else Counter({(opcode,opcode):1}))
    submit('open',opcode=3);submit('start',opcode=6)
    for frame in range(a.frames):
        offset=r['INPUT_OFFSET']//512;count=2*r['FRAME_BYTES']//512
        native(f'dd if={remote}/input-{frame:03d}.nv12 of={DEBUG}/ave-arena bs=512 seek={offset} count={count} conv=notrunc && '
            f'dd if={DEBUG}/ave-arena of={remote}/verify.nv12 bs=512 skip={offset} count={count} && '
            f'cmp {remote}/input-{frame:03d}.nv12 {remote}/verify.nv12',f'input-{frame:03d}')
        submit(f'pair-{frame:03d}',pair=True)
        reads='; '.join(f'dd if={DEBUG}/ave-arena bs=512 skip={offset//512} count={length//512} 2>/dev/null' for offset,length in REGIONS)
        native(f'printf "MV_REGIONS_BEGIN\\n"; ( {reads}; ) | gzip -c | base64; printf "MV_REGIONS_END\\n"',f'regions-{frame:03d}',45)
        print('MVHEVC_NATIVE_PAIR',frame+1,'/',a.frames,flush=True)
    submit('stop',opcode=7);submit('close',opcode=4)
    native(f'cat /sys/devices/system/cpu/online; /opt/neo/bin/g17p-vulkan-compute; cat {DEBUG}/ave-status; '
        f'strings {DEBUG}/ave-firmware-log; dmesg','post-health',45)
run([sys.executable,ROOT/'video/validate-mvhevc-linux.py',out],'validate')
print('MVHEVC_LINUX_VALIDATED',a.frames,out,flush=True)
