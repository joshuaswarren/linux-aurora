#!/usr/bin/env python3
"""Archive and cross-check the recovered LUT with independently decoded Neo samples."""
import argparse
from array import array
import hashlib
import json
from pathlib import Path
import runpy
import subprocess

ROOT=Path(__file__).resolve().parents[1]
p=argparse.ArgumentParser(description=__doc__);p.add_argument('output',type=Path);a=p.parse_args()
a.output.mkdir(parents=True,exist_ok=False)
module=runpy.run_path(str(Path(__file__).with_name('depth-companding.py')))
sha=lambda data:hashlib.sha256(data).hexdigest()
results=[]
for kind in ['depth','disparity']:
    target=a.output/kind;target.mkdir()
    reference=ROOT/f'artifacts/coverage-reference-20260909/guest/hevc-{kind}'
    metadata=json.loads((reference/'result.json').read_text())
    assert metadata['passed'] and metadata['hardware_required'] and metadata['using_hardware_encoder']
    assert metadata['model']=='Mac17,5' and '25G76' in metadata['system_version']
    elementary=ROOT/f'artifacts/hevc-{kind}-sps-inspection-20260909/video.hevc'
    result=subprocess.run(['ffmpeg','-v','error','-i',str(elementary),'-pix_fmt','gray10le','-f','rawvideo','-'],capture_output=True,check=True)
    gray=result.stdout;decoded=module['decode_codes'](gray,kind)
    independent=(ROOT/f'artifacts/hevc-{kind}-host-reference-decode-20260909/decoded.raw').read_bytes()
    assert len(gray)==3*320*240*2 and decoded==independent
    encoder,_=module['tables'](*module['limits'](kind))
    _,decoder=module['tables'](*module['limits'](kind,decoding=True))
    assert len(encoder)==65536 and len(decoder)==1024
    assert all(a<=b for a,b in zip(encoder[:32767],encoder[1:32768]))
    assert not any(encoder[32768:]) and all(v&63==0 for v in encoder)
    for bad in [float('nan'),float('inf'),-1.,65505.]:
        try:module['encode_f32'](array('f',[bad]).tobytes(),kind)
        except ValueError:pass
        else:raise AssertionError('Invalid input was accepted')
    source=(reference/'input.raw').read_bytes();encoded=module['encode_f32'](source,kind)
    for name,data in [('half-to-l010.lut',encoder.tobytes()),('gray10-to-float32.lut',decoder.tobytes()),
        ('reference-gray10.raw',gray),('reference-reconstructed.raw',decoded),('converted-input-l010.raw',encoded)]:
        (target/name).write_bytes(data)
    results.append(dict(profile=kind,reference_frames=3,reference_sha256=sha((reference/'video.mov').read_bytes()),
        reference_input_sha256=sha(source),independent_float32_decode_exact=True,invalid_values_rejected=True,
        encoder_limits=list(module['limits'](kind)),decoder_limits=list(module['limits'](kind,decoding=True)),
        converted_input_sha256=sha(encoded),hardware_input_layout='L010: 320x240 stride640,153600 visible +64 trailing bytes',
        encoder_lut_sha256=sha(encoder.tobytes()),decoder_lut_sha256=sha(decoder.tobytes())))
(a.output/'validation.json').write_text(json.dumps(dict(passed=True,source='Host VideoToolbox static depth-wrapper algorithm checked against Neo 25G76 reference',
    native_encoding_claim=False,results=results),indent=2)+'\n')
print('DEPTH_COMPANDING_REFERENCE_MATCH',len(results),'formats; all 460800 float values exact')
