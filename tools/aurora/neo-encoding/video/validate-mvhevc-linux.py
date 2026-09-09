#!/usr/bin/env python3
"""Extract owned MV DMA regions and independently decode both reference-matched eyes."""
import argparse
import base64
import gzip
import hashlib
import json
import math
from pathlib import Path
import re
import runpy
import struct
import subprocess
import sys

ROOT=Path(__file__).resolve().parents[1]
p=argparse.ArgumentParser(description=__doc__);p.add_argument('output',type=Path);a=p.parse_args()
out=a.output.resolve();meta=json.loads((out/'run.json').read_text());frames=meta['frames']
assert 1<=frames<=60 and meta['codec']=='MV-HEVC'
reference=ROOT/'artifacts/mvhevc-prefix-reference-20260909/reference'
inspector=runpy.run_path(str(ROOT/'video/inspect-hevc-reference.py'))
reference_movie=(reference/'video.mov').read_bytes()
prefix,description=inspector['description'](reference_movie)
tree=reference_movie
for kind in [b'moov',b'trak',b'mdia',b'minf',b'stbl',b'stsd']:tree=inspector['one'](tree,kind)
entry=list(inspector['atoms'](tree[8:]))[0][1]
layered=inspector['one'](entry[78:],b'lhvC');assert len(layered)==34 and layered[0]==1
cursor=6;extra=[]
for _ in range(layered[5]):
    kind=layered[cursor]&63;count=struct.unpack_from('>H',layered,cursor+1)[0];cursor+=3
    assert count==1 and kind in (33,34)
    size=struct.unpack_from('>H',layered,cursor)[0];cursor+=2
    assert 2<=size<=len(layered)-cursor
    nal=layered[cursor:cursor+size];cursor+=size;assert nal[0]>>1&63==kind
    extra.append(nal)
assert cursor==len(layered)
def nal_key(nal):return nal[0]>>1&63,((nal[0]&1)<<5)|(nal[1]>>3)
reference_nals=prefix.split(b'\0\0\0\1')[1:]+extra
expected={nal_key(n):n for n in reference_nals if n[0]>>1&63 in (32,33,34)}
assert set(expected)=={(32,0),(33,0),(34,0),(33,1),(34,1)}
def run(command,name):
    result=subprocess.run([str(x) for x in command],capture_output=True)
    (out/(name+'.log')).write_bytes(result.stdout+result.stderr)
    if result.returncode:raise RuntimeError(name+' failed; inspect saved outputs')
    return result.stdout
for name in ['open','start','stop','close']+[f'pair-{i:03d}' for i in range(frames)]:
    text=(out/(name+'.log')).read_text()
    assert re.search(r'^AVE_PACKET_RESULT=0$',text,re.M) and re.search(r'^command_failed=0$',text,re.M)
health=(out/'post-health.log').read_text()
assert re.search(r'^0-5$',health,re.M) and 'g17p-vulkan-compute: PASS' in health
assert not re.search(r'CRIT:|kernel panic|SError|dart.*(?:fault|translation error)',health)
samples=[];elementary=bytearray()
for frame in range(frames):
    text=(out/f'regions-{frame:03d}.log').read_text()
    encoded=text.split('\nMV_REGIONS_BEGIN\n',1)[1].split('\nMV_REGIONS_END\n',1)[0]
    data=gzip.decompress(base64.b64decode(''.join(encoded.split()),validate=True))
    assert len(data)==sum(length for offset,length in meta['regions'])==591872
    (out/f'regions-{frame:03d}.bin').write_bytes(data)
    regions=[];cursor=0
    for offset,size in meta['regions']:regions.append(data[cursor:cursor+size]);cursor+=size
    parameters={};vcl=[]
    for view in range(2):
        stream,metadata,tile=regions[view*3:view*3+3]
        size=struct.unpack_from('<I',metadata,0x120)[0]
        assert size==struct.unpack_from('<I',metadata,0x12c)[0] and 5<=size<=len(stream)
        header_size=struct.unpack_from('<I',metadata,0x128)[0];assert 6<=header_size<=len(tile)
        joined=tile[:header_size]+stream[:size];assert joined[:4]==b'\0\0\0\1'
        nal=joined[4:];assert not nal[0]&128 and nal[1]&7
        kind,layer=nal_key(nal);assert layer==view and kind in (19,20,21)
        assert b'\0\0\0\1' not in nal
        vcl.append(nal)
        matches=[m.start() for m in re.finditer(b'\x00\x00\x00\x01',metadata)]
        for i,start in enumerate(matches):
            if start+6>len(metadata) or metadata[start+4]&128 or not metadata[start+5]&7:continue
            kind,layer=nal_key(metadata[start+4:start+6])
            if kind not in (32,33,34) or layer not in (0,1):continue
            end=matches[i+1] if i+1<len(matches) else len(metadata)
            nal=metadata[start+4:end].rstrip(b'\0');key=kind,layer
            if key in parameters:assert parameters[key]==nal
            parameters[key]=nal
    (out/f'parameters-{frame:03d}.json').write_text(json.dumps({f'{k[0]}:{k[1]}':v.hex() for k,v in parameters.items()},indent=2)+'\n')
    assert parameters==expected, 'Native multilayer parameters differ from reference hvcC/lhvC'
    sample=b''.join(struct.pack('>I',len(n))+n for n in vcl)
    path=out/f'frame-{frame:03d}.sample';path.write_bytes(sample);samples.append(path)
    # Container metadata supplies the frozen stereo SEI and both layer configs.
    elementary += b''.join(b'\0\0\0\1'+n for n in reference_nals+vcl)
(out/'video.hevc').write_bytes(elementary)
movie=out/'video.mov'
if movie.exists():movie.unlink()
run([sys.executable,ROOT/'video/repack-hevc-reference-mov.py',reference/'video.mov',movie,*samples],'mux')
newprefix,newdescription=inspector['description'](movie.read_bytes())
assert newprefix==prefix and newdescription==description
views=[];decoded_views=[]
for view in range(2):
    raw=out/f'view-{view}.nv12'
    run(['ffmpeg','-v','error','-y','-view_ids',view,'-i',movie,'-pix_fmt','nv12','-f','rawvideo',raw],f'decode-view-{view}')
    decoded=raw.read_bytes();original=(out/f'input-view-{view}.nv12').read_bytes()
    assert len(decoded)==len(original)==frames*115200
    reference_decoded=(ROOT/f'artifacts/mvhevc-reference-validation-20260909/view-{view}.nv12').read_bytes()
    assert decoded[:115200]==reference_decoded[:115200], 'Native first eye differs from hardware reference'
    assert all(decoded[i*115200:(i+1)*115200]!=decoded[(i+1)*115200:(i+2)*115200] for i in range(frames-1))
    quality=[]
    for i in range(frames):
        x=decoded[i*115200:(i+1)*115200];y=original[i*115200:(i+1)*115200]
        mse=sum((a-b)**2 for a,b in zip(x,y))/len(x)
        psnr=10*math.log10(255**2/mse) if mse else 1000.;assert psnr>=25;quality.append(psnr)
    probe=json.loads(run(['ffprobe','-v','error','-view_ids',view,'-count_frames','-show_streams','-show_format',
        '-of','json',movie],f'probe-view-{view}'))
    stream=probe['streams'][0]
    assert stream['codec_name']=='hevc' and (stream['width'],stream['height'])==(320,240)
    assert stream['pix_fmt']=='yuv420p' and int(stream['nb_read_frames'])==frames
    assert abs(float(probe['format']['duration'])-frames/30)<.04
    views.append(dict(view=view,frames=frames,minimum_psnr_db=min(quality),first_frame_matches_reference=True,
        output_chroma='4:2:0',output_bit_depth=8,decoded_sha256=hashlib.sha256(decoded).hexdigest()))
    decoded_views.append(decoded)
assert decoded_views[0]!=decoded_views[1]
result=dict(passed=True,status='Linux validated',codec='MV-HEVC',frames=frames,width=320,height=240,fps=30,
    views=views,session_closed=True,cpus_online='0-5',vulkan_values=32,module_sha256=meta['module_sha256'],
    hardware_source='Native Neo Linux AVE paired views',frame_policy='Forced keyframes; sustained/inter-frame scheduling not qualified',
    reference_provenance='First-pair capture prefix plus successful three-frame Neo hardware-required reference',
    movie_sha256=hashlib.sha256(movie.read_bytes()).hexdigest())
(out/'validation.json').write_text(json.dumps(result,indent=2)+'\n')
print('MVHEVC_LINUX_VALIDATED',json.dumps(result))
