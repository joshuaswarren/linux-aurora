#!/usr/bin/env python3
"""Independently decode native depth/disparity, preserving the reference disc box."""
import argparse
from array import array
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
p=argparse.ArgumentParser(description=__doc__)
p.add_argument('output',type=Path);p.add_argument('--profile',choices=['depth','disparity'],required=True)
a=p.parse_args();out=a.output.resolve();run_meta=json.loads((out/'run.json').read_text())
frames=run_meta['frames'];assert 1<=frames<=60 and run_meta['profile']==a.profile
reference=ROOT/f'artifacts/coverage-reference-20260909/guest/hevc-{a.profile}'
reference_decoded=ROOT/f'artifacts/hevc-{a.profile}-host-reference-decode-20260909/decoded.raw'
ref=json.loads((reference/'result.json').read_text())
assert ref['passed'] and ref['hardware_required'] and ref['using_hardware_encoder']
assert ref['model']=='Mac17,5' and '25G76' in ref['system_version']
inspect=runpy.run_path(str(ROOT/'video/inspect-hevc-reference.py'))
companding=runpy.run_path(str(ROOT/'video/depth-companding.py'))
prefix,description=inspect['description']((reference/'video.mov').read_bytes())
assert description['sample_entry']=={'depth':'deph','disparity':'dish'}[a.profile]
assert description['chroma_format']==0 and description['luma_bit_depth']==10

def run(command,name):
    result=subprocess.run([str(x) for x in command],capture_output=True)
    (out/(name+'.log')).write_bytes(result.stdout+result.stderr)
    if result.returncode:raise RuntimeError(name+' failed; inspect archived log')
    return result.stdout

segments=[]
for frame in range(frames):
    stream=(out/f'frame-{frame:03d}.hevc').read_bytes()
    nals=stream.split(b'\0\0\0\1');assert not nals.pop(0)
    headers=b''.join(b'\0\0\0\1'+n for n in nals if n[0]>>1&63 in (32,33,34))
    assert headers==prefix, 'Native parameters differ from reference MOV sample description'
    sample=b''.join(struct.pack('>I',len(n))+n for n in nals if n[0]>>1&63 not in (32,33,34))
    dest=out/f'frame-{frame:03d}.sample';dest.write_bytes(sample);segments.append(dest)
movie=out/'video.mov'
if movie.exists():movie.unlink()  # Derived output; repeatable validation of immutable DMA evidence.
run([sys.executable,ROOT/'video/repack-hevc-reference-mov.py',reference/'video.mov',movie,*segments],'mux-auxiliary')
native_prefix,native_description=inspect['description'](movie.read_bytes())
assert native_prefix==prefix and native_description==description
probe=json.loads(run(['ffprobe','-v','error','-count_frames','-show_streams','-of','json',out/'video.hevc'],'auxiliary-probe'))['streams'][0]
assert probe['codec_name']=='hevc' and probe['profile']=='Rext' and probe['pix_fmt']=='gray10le'
assert (probe['width'],probe['height'])==(320,240) and int(probe['nb_read_frames'])==frames
run(['ffmpeg','-v','error','-y','-i',out/'video.hevc','-pix_fmt','gray10le','-f','rawvideo',out/'decoded-gray10.raw'],'auxiliary-gray-decode')
gray=(out/'decoded-gray10.raw').read_bytes();assert len(gray)==frames*320*240*2
reconstructed=companding['decode_codes'](gray,a.profile)
(out/'reconstructed-float32.raw').write_bytes(reconstructed)
decoder=ROOT/'artifacts/coverage-probes-alpha16-decode-20260909/neo-vt-decode-probe'
assert hashlib.sha256(decoder.read_bytes()).hexdigest()=='38cc35613972840143cd8298dc1c511d5597b20ee8ea0f8d52fa62d3efb7fb35'
decode_dir=out/'host-decode'
if not decode_dir.exists():
    run([decoder,movie,decode_dir,'fdep' if a.profile=='depth' else 'fdis','--allow-host-decoder'],'auxiliary-float-decode')
dec=json.loads((decode_dir/'result.json').read_text())
decoded=(decode_dir/'decoded.raw').read_bytes()
assert dec['passed'] and dec['frames']==frames and (dec['width'],dec['height'])==(320,240)
assert all(abs(t-i/30)<1e-6 for i,t in enumerate(dec['timestamps']))
assert decoded==reconstructed, 'Independent VideoToolbox float decode differs from reconstructed LUT'
n=320*240;frame_bytes=n*4
assert decoded[:frame_bytes]==reference_decoded.read_bytes()[:frame_bytes], 'First native frame differs from Neo hardware reference'
source=array('f',(out/'input.raw').read_bytes());values=array('f',decoded)
assert len(source)==len(values)==frames*n and all(math.isfinite(v) and v>0 for v in values)
assert all(values[i*n:(i+1)*n]!=values[(i+1)*n:(i+2)*n] for i in range(frames-1))
quality=[];maximum_relative=0.;maximum_absolute=0.
for i in range(frames):
    x=source[i*n:(i+1)*n];y=values[i*n:(i+1)*n]
    errors=[abs(v-w) for v,w in zip(x,y)]
    relative=max(e/abs(v) for v,e in zip(x,errors));rmse=math.sqrt(sum(e*e for e in errors)/n)
    psnr=20*math.log10((max(x)-min(x))/rmse) if rmse else 1000.
    assert relative<=.05 and psnr>=40
    maximum_relative=max(maximum_relative,relative);maximum_absolute=max(maximum_absolute,max(errors));quality.append(psnr)
health=(out/'post-health.log').read_text()
assert re.search(r'^0-5$',health,re.M) and 'g17p-vulkan-compute: PASS' in health
assert not re.search(r'CRIT:|kernel panic|SError|dart.*(?:fault|translation error)',health)
result=dict(passed=True,status='Linux validated',codec=ref['codec'],profile=a.profile,frames=frames,width=320,height=240,fps=30,
    input_format='grayf32le',hardware_input_format='L010',output_chroma='monochrome',output_bit_depth=10,
    decoded_format='float32',lossy=True,minimum_range_normalized_psnr_db=min(quality),maximum_relative_error=maximum_relative,
    maximum_absolute_error=maximum_absolute,first_frame_matches_macos_reference=True,independent_decoders_agree=True,
    decoder_model=dec['model'],decoder_system=dec['system_version'],session_closed=True,cpus_online='0-5',vulkan_values=32,
    frame_policy='forced keyframes; inter-frame scheduling remains outside this qualification',
    hardware_source='native Neo Linux AVE',input_layout_provenance='Frozen Neo descriptor plus host L010 allocation and reconstructed depth wrapper LUT',
    pattern=run_meta.get('pattern','row-wrap'),module_sha256=run_meta['module_sha256'],decoded_sha256=hashlib.sha256(decoded).hexdigest(),movie_sha256=hashlib.sha256(movie.read_bytes()).hexdigest())
(out/'validation.json').write_text(json.dumps(result,indent=2)+'\n')
print('HEVC_AUXILIARY_LINUX_VALIDATED',json.dumps(result))
