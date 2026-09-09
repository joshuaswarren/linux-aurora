#!/usr/bin/env python3
"""Freeze Neo native depth/disparity qualification, source, symbols and limitations."""
import hashlib
import json
from pathlib import Path
import shutil

r=Path(__file__).resolve().parents[1];out=r/'artifacts/hevc-auxiliary-qualified-20260909'
assert not out.exists()
sha=lambda p:hashlib.sha256(p.read_bytes()).hexdigest()
module=r/'artifacts/linux-ave-32m-candidate-20260909/apple-ave.ko'
results=[];reports=[]
for profile in ['depth','disparity']:
    for mode,frames in [('one',1),('smooth',60)]:
        path=r/'artifacts'/f'hevc-{profile}-native-{mode}-20260909'
        data=json.loads((path/'validation.json').read_text())
        assert data['passed'] and data['frames']==frames and data['profile']==profile
        assert data['first_frame_matches_macos_reference'] and data['independent_decoders_agree']
        assert data['session_closed'] and data['cpus_online']=='0-5' and data['vulkan_values']==32
        assert data['module_sha256']==sha(module) and data['maximum_relative_error']<=.05
        assert data['minimum_range_normalized_psnr_db']>=40
        if frames==60:assert data['pattern']=='smooth-depth'
        results.append(path);reports.append(data)
for codec in ['h264','hevc','prores']:
    path=r/'artifacts'/f'hevc-auxiliary-{codec}-regression-20260909'
    data=json.loads((path/'validation.json').read_text())
    assert data['passed'] and data['frames']==3
    if codec!='prores':
        assert data['session_closed'] and data['cpus_online']=='0-5' and data['vulkan_values']==32
        assert data['module_sha256']==sha(module)
    results.append(path)
final=r/'artifacts/hevc-auxiliary-final-health-20260909'
health=json.loads((final/'validation.json').read_text())
assert health['passed'] and health['cpus_online']=='0-5' and health['vulkan_values']==32
assert health['module_sha256']==sha(module) and health['cpu_affinity_checks']==list(range(6))
out.mkdir()
for path in results+[final]:shutil.copytree(path,out/'results'/path.name)
directories=['video/linux-ave','artifacts/linux-ave-32m-candidate-20260909',
    'artifacts/host-depth-static-20260909','artifacts/depth-companding-validation-20260909',
    'artifacts/coverage-probes-alpha16-decode-20260909',
    'artifacts/hevc-depth-native-moving-20260909','logs/20260909T075420Z-native-linux']
for profile in ['depth','disparity']:
    directories += [f'artifacts/ave-hevc-{profile}-protocol-20260909',
        f'artifacts/coverage-reference-20260909/guest/hevc-{profile}',
        f'artifacts/hevc-{profile}-host-reference-decode-20260909',
        f'artifacts/hevc-{profile}-sps-inspection-20260909']
for directory in directories:shutil.copytree(r/directory,out/directory)
for name in ['depth-companding.py','validate-depth-companding.py','neo-depth-layout-probe.m','neo-vt-decode-probe.m',
    'neo-vt-codec-probe.m','coverage-pixels.h','ave-relocate-hevc-precision.py','encode-hevc-precision-linux.py',
    'encode-linux-video.py','validate-hevc-auxiliary-linux.py','ave-extract-frame.py','ave-make-command.py',
    'inspect-hevc-reference.py','repack-hevc-reference-mov.py','qualify-hevc-auxiliary.py','test-hevc-auxiliary-host.py',
    'check-encoding-linux-health.py']:
    shutil.copyfile(r/'video'/name,out/'video'/name)
for name in ['console.py','push-linux.py','ENCODING_EXPANSION_RESULTS.md','ENCODING_TARGETS.json']:
    shutil.copyfile(r/name,out/name)
for name in ['hevc-auxiliary-host-checks-20260909.log','hevc-depth-native-moving-20260909.log',
    'hevc-auxiliary-prores-regression-20260909.log','hevc-auxiliary-prores-warm-regression-20260909.log']:
    shutil.copyfile(r/'logs'/name,out/'logs'/name)
dependencies={}
for name in ['linux','m1n1-linux','ave-pool-qualified-20260909',
    'ave-video-qualified-20260908','prores-all-profiles-qualified-20260909']:
    f=r/'artifacts'/name/'manifest.json';assert f.exists()
    dependencies[name]=dict(path='artifacts/'+name,manifest_sha256=sha(f))
(out/'DEPENDENCIES.json').write_text(json.dumps(dict(required_existing_archives=dependencies),indent=2)+'\n')
(out/'SCOPE.json').write_text(json.dumps(dict(status='Linux validated',profiles=['HEVC depth','HEVC disparity'],
    input_format='float32',hardware_format='L010',output_format='10-bit monochrome HEVC with disc companding metadata',
    width=320,height=240,frames_per_profile=[1,60],input_pattern='smooth depth ramp changing by 1/256 per frame',
    policy='Forced keyframes; inter-frame scheduling remains unqualified.',lossy=True,
    limitations=['The archived row-wrap depth stress run encoded and decoded all 60 frames but failed the 5 percent per-pixel error gate at discontinuities. It is not qualified.',
        'Float32 is compacted through half-float and a logarithmic 10-bit code; arbitrary float32 precision is not preserved.'],
    persistent_stage1_unchanged=True,results=reports),indent=2)+'\n')
files={str(p.relative_to(out)):dict(bytes=p.stat().st_size,sha256=sha(p)) for p in sorted(out.rglob('*')) if p.is_file()}
(out/'manifest.json').write_text(json.dumps(dict(scope='Native Neo HEVC depth/disparity',files=files),indent=2)+'\n')
for name,record in files.items():
    p=out/name;assert p.stat().st_size==record['bytes'] and sha(p)==record['sha256']
print('HEVC_AUXILIARY_FROZEN',len(files),'files',sum(v['bytes'] for v in files.values()),'bytes')
