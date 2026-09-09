#!/usr/bin/env python3
"""Validate a captured VideoToolbox bitstream against the deterministic input.

Run in neo-builder, where FFmpeg is available. Hardware identity comes from
the target's saved VideoToolbox report, not the decoder or the build VM.
"""
import argparse
import hashlib
import json
import math
from pathlib import Path
import subprocess

p = argparse.ArgumentParser()
p.add_argument('source',type=Path)
p.add_argument('bitstream',type=Path)
p.add_argument('target_result',type=Path)
p.add_argument('output',type=Path)
a = p.parse_args()
a.output.mkdir(parents=True,exist_ok=False)
target = json.loads(a.target_result.read_text())
if not target.get('hardware_required') or not target.get('using_hardware_encoder'):
    raise SystemExit('Target did not attest a hardware-required encoder session')
frames = int(target['requested_frames'])
codec = target.get('codec', 'h264').lower()
if codec not in ('h264', 'hevc'):
    raise SystemExit('Unsupported target codec')
width, height = int(target['width']), int(target['height'])
if target.get('callback_status') != 0 or target.get('encoded_frames') != frames:
    raise SystemExit('Target encode did not complete every frame')
probe = json.loads(subprocess.check_output(['ffprobe','-v','error','-count_frames',
    '-show_entries','stream=codec_name,width,height,pix_fmt,nb_read_frames,profile',
    '-of','json',str(a.bitstream)],text=True))
stream = probe['streams'][0]
decoded = a.output/'decoded.nv12'
with (a.output/'decode.log').open('w') as log:
    subprocess.run(['ffmpeg','-nostdin','-hide_banner','-v','warning','-i',str(a.bitstream),
                    '-fps_mode','passthrough','-pix_fmt','nv12','-f','rawvideo',str(decoded)],
                   stdout=log,stderr=log,check=True)
expected_bytes = frames*width*height*3//2
original = a.source.read_bytes()[:expected_bytes]
actual = decoded.read_bytes()
if len(original)!=expected_bytes or len(actual)!=expected_bytes:
    raise SystemExit('Reference/decoded byte count mismatch')
mse = sum((x-y)**2 for x,y in zip(original,actual))/len(original)
psnr = None if mse==0 else 10*math.log10(255*255/mse)
passed = (stream['codec_name']==codec and int(stream['nb_read_frames'])==frames
          and stream['width']==width and stream['height']==height
          and (psnr is None or psnr>=35))
result = {'passed':passed,'target_report':target,'decoded_stream':stream,'psnr_db':psnr,
          'claim':'macOS hardware encoder reference, not a Linux encoder implementation',
          'files':{}}
for label,path in [('source',a.source),('bitstream',a.bitstream),('target_result',a.target_result),('decoded',decoded)]:
    result['files'][label]={'bytes':path.stat().st_size,'sha256':hashlib.sha256(path.read_bytes()).hexdigest()}
(a.output/'validation.json').write_text(json.dumps(result,indent=2)+'\n')
print(f'AVE_REFERENCE_{"PASS" if passed else "FAIL"} frames={frames} psnr_db={psnr}')
raise SystemExit(0 if passed else 1)
