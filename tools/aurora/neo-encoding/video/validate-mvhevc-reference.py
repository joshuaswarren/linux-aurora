#!/usr/bin/env python3
"""Independently decode both HEVC views and compare each with its own source."""
import argparse
import hashlib
import json
from pathlib import Path
import struct
import subprocess

p=argparse.ArgumentParser(description=__doc__)
p.add_argument('reference',type=Path);p.add_argument('output',type=Path)
a=p.parse_args();a.output.mkdir(parents=True,exist_ok=False)
meta=json.loads((a.reference/'result.json').read_text())
assert meta['passed'] and meta['hardware_required'] and meta['using_hardware_encoder']
assert meta['configuration']['multiview'] and meta['model']=='Mac17,5' and '25G76' in meta['system_version']
assert (meta['width'],meta['height'],meta['encoded_frames'])==(320,240,3)
layers=set()
for sample in sorted(a.reference.glob('frame-*.sample')):
    data=sample.read_bytes();pos=0
    while pos<len(data):
        n=struct.unpack_from('>I',data,pos)[0];pos+=4
        assert 2<=n<=len(data)-pos
        nal_type=(data[pos]>>1)&63
        if nal_type<=31:layers.add(((data[pos]&1)<<5)|(data[pos+1]>>3))
        pos+=n
assert len(layers)==2
def run(args,name):
    with (a.output/(name+'.log')).open('w') as log:
        subprocess.run(args,stdout=log,stderr=subprocess.STDOUT,check=True)
reports=[];decoded=[]
for view,source in [(0,'input.raw'),(1,'input-right.raw')]:
    raw=a.output/f'view-{view}.nv12'
    run(['ffmpeg','-v','error','-view_ids',str(view),'-i',str(a.reference/'video.mov'),
         '-f','rawvideo','-pix_fmt','nv12',str(raw)],f'decode-{view}')
    data=raw.read_bytes();assert len(data)==3*115200
    decoded.append(data)
    stats=a.output/f'view-{view}-psnr.txt'
    run(['ffmpeg','-v','error','-f','rawvideo','-pixel_format','nv12','-video_size','320x240','-framerate','30','-i',str(raw),
         '-f','rawvideo','-pixel_format','nv12','-video_size','320x240','-framerate','30','-i',str(a.reference/source),
         '-lavfi',f'psnr=stats_file={stats}','-f','null','-'],f'quality-{view}')
    rows=[dict(w.split(':',1) for w in l.split()) for l in stats.read_text().splitlines()]
    assert len(rows)==3
    minimum=min(float(row['psnr_avg']) for row in rows)
    assert minimum>=25
    reports.append(dict(view=view,frames=3,pixel_format='nv12',minimum_psnr_db=minimum,
        decoded_sha256=hashlib.sha256(data).hexdigest()))
assert decoded[0]!=decoded[1]
report=dict(status='reference validated',codec='MV-HEVC',hardware_required=True,
    vcl_layer_ids=sorted(layers),views=reports,native_linux_validated=False)
(a.output/'validation.json').write_text(json.dumps(report,indent=2)+'\n')
print('MV_HEVC_REFERENCE_VALIDATED',json.dumps(report))
