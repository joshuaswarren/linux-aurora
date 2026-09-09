#!/usr/bin/env python3
"""Independently qualify precision and transparency in the saved Neo references."""
import argparse
from array import array
import hashlib
import json
import math
from pathlib import Path
import subprocess

p=argparse.ArgumentParser(description=__doc__)
p.add_argument('reference',type=Path);p.add_argument('output',type=Path)
a=p.parse_args();a.output.mkdir(parents=True,exist_ok=False)
reference=json.loads((a.reference/'result.json').read_text())
assert reference['passed'] and reference['hardware_required'] and reference['using_hardware_encoder']
assert reference['model']=='Mac17,5' and '25G76' in reference['system_version']
assert reference['codec'] in ('ap4h','ap4x') and reference['input_pixel_format']=='ayuv64le'
frames=reference['encoded_frames'];assert frames==reference['requested_frames'] and frames>=3
def run(command,name):
    with (a.output/(name+'.log')).open('w') as log:
        subprocess.run(list(map(str,command)),stdout=log,stderr=subprocess.STDOUT,check=True)
    return (a.output/(name+'.log')).read_text()
probe=json.loads(run(['ffprobe','-v','error','-count_frames','-show_streams','-of','json',a.reference/'video.mov'],'probe'))['streams'][0]
assert probe['codec_name']=='prores' and probe['pix_fmt']=='yuva444p12le' and int(probe['nb_read_frames'])==frames
assert (probe['width'],probe['height'])==(320,240)
run(['ffmpeg','-v','error','-i',a.reference/'video.mov','-pix_fmt','yuva444p12le','-f','rawvideo',a.output/'decoded.raw'],'decode')
src=array('H',(a.reference/'input.raw').read_bytes());dst=array('H',(a.output/'decoded.raw').read_bytes())
n=320*240;assert len(src)==len(dst)==frames*n*4
assert all({v>>4&15 for v in src[c::4]}==set(range(16)) for c in (1,2,3))
quality=[]
for f in range(frames):
    alpha=dst[(f*4+3)*n:(f*4+4)*n]
    expected=array('H',(src[(f*n+i)*4]>>4 for i in range(n)))
    assert alpha==expected and len(set(alpha))==4096
    if f:assert alpha!=dst[((f-1)*4+3)*n:((f-1)*4+4)*n]
    mse=sum((dst[(f*4+c)*n+i]-(src[(f*n+i)*4+c+1]>>4))**2 for c in range(3) for i in range(n))/(3*n)
    psnr=10*math.log10(4095**2/mse) if mse else 1000.0
    assert psnr>=25;quality.append(psnr)
result=dict(passed=True,status='reference validated',native_linux_validated=False,codec=reference['codec'],frames=frames,
    width=320,height=240,source_color_bits=12,source_alpha_bits=16,decoded_color_bits=12,decoded_alpha_bits=12,
    all_4096_alpha_values_present=True,alpha_matches_source_top_12_bits=True,changing_transparency=True,
    minimum_color_psnr_db=min(quality),decoded_sha256=hashlib.sha256((a.output/'decoded.raw').read_bytes()).hexdigest())
(a.output/'validation.json').write_text(json.dumps(result,indent=2)+'\n')
print('PRORES_ALPHA_REFERENCE_VALIDATED',json.dumps(result))
