#!/usr/bin/env python3
"""Validate independently decoded float depth/disparity against reference input."""
import argparse
from array import array
import hashlib
import json
import math
from pathlib import Path

p=argparse.ArgumentParser(description=__doc__)
p.add_argument('reference',type=Path);p.add_argument('decoded',type=Path)
a=p.parse_args();ref=json.loads((a.reference/'result.json').read_text());dec=json.loads((a.decoded/'result.json').read_text())
assert ref['passed'] and ref['hardware_required'] and ref['using_hardware_encoder']
assert ref['model']=='Mac17,5' and '25G76' in ref['system_version']
assert ref['codec'] in ('deph','dish') and dec['format']==('fdep' if ref['codec']=='deph' else 'fdis')
assert dec['passed'] and (dec['width'],dec['height'])==(320,240)
assert dec['frames']==ref['encoded_frames']==ref['requested_frames']
assert all(abs(t-i/30)<1e-6 for i,t in enumerate(dec['timestamps']))
s=array('f',(a.reference/'input.raw').read_bytes());d=array('f',(a.decoded/'decoded.raw').read_bytes());n=320*240
assert len(s)==len(d)==dec['frames']*n and all(math.isfinite(v) and v>0 for v in d)
assert all(d[i*n:(i+1)*n]!=d[(i+1)*n:(i+2)*n] for i in range(dec['frames']-1))
errors=[abs(x-y) for x,y in zip(s,d)]
relative=max(e/abs(x) for x,e in zip(s,errors));rmse=math.sqrt(sum(e*e for e in errors)/len(errors))
psnr=20*math.log10((max(s)-min(s))/rmse)
assert relative<=0.05 and psnr>=40
result=dict(passed=True,status='reference validated',native_linux_validated=False,codec=ref['codec'],
    width=320,height=240,frames=dec['frames'],input_output='float32',lossy=True,rmse=rmse,
    maximum_absolute_error=max(errors),maximum_relative_error=relative,range_normalized_psnr_db=psnr,
    decoder_model=dec['model'],decoder_system=dec['system_version'],
    movie_sha256=hashlib.sha256((a.reference/'video.mov').read_bytes()).hexdigest(),
    decoded_sha256=hashlib.sha256((a.decoded/'decoded.raw').read_bytes()).hexdigest())
(a.decoded/'validation.json').write_text(json.dumps(result,indent=2)+'\n')
print('AUXILIARY_FLOAT_REFERENCE_VALIDATED',json.dumps(result))
