#!/usr/bin/env python3
"""Archive native Main10/Main42210 source, symbols, inputs and qualification."""
import hashlib
import json
from pathlib import Path
import shutil

r=Path(__file__).resolve().parents[1]
out=r/'artifacts/hevc-precision-qualified-20260909'
assert not out.exists()
sha=lambda p:hashlib.sha256(p.read_bytes()).hexdigest()
module=r/'artifacts/linux-ave/apple-ave.ko'
results=[]
for profile in ['main10','main42210']:
    for mode,frames in [('first',1),('moving',60)]:
        name=f'hevc-{profile}-'+('keyframes-' if profile=='main10' and mode=='moving' else '')+mode+'-linux-20260909'
        path=r/'artifacts'/name;data=json.loads((path/'validation.json').read_text())
        assert data['passed'] and data['profile']==profile and data['frames']==frames and data['bit_depth']==10
        assert data['first_frame_matches_macos_reference'] and data['session_closed']
        assert data['cpus_online']=='0-5' and data['vulkan_values']==32 and data['module_sha256']==sha(module)
        results.append(path)
for codec in ['h264','hevc','prores']:
    path=r/'artifacts'/f'hevc-precision-{codec}-regression-20260909'
    data=json.loads((path/'validation.json').read_text());assert data['passed'] and data['frames']==3
    if codec!='prores':assert data['session_closed'] and data['cpus_online']=='0-5' and data['vulkan_values']==32
    results.append(path)
out.mkdir()
for path in results:shutil.copytree(path,out/'results'/path.name)
for directory in ['video/linux-ave','artifacts/ave-hevc-main10-capture-20260909',
    'artifacts/ave-hevc-main42210-capture-20260909','artifacts/coverage-probes-20260909']:
    shutil.copytree(r/directory,out/directory)
for name in ['ave-relocate-hevc-precision.py','encode-hevc-precision-linux.py','ave-extract-frame.py',
    'ave-make-command.py','qualify-hevc-precision.py','freeze-hevc-precision-reference.py',
    'trace-j700-ave-commands.py','trace-j700-coverage.py','summarize-ave-commands.py']:
    shutil.copyfile(r/'video'/name,out/'video'/name)
for name in ['apple-ave.ko','apple-ave-unstripped.ko']:
    target=out/'artifacts/linux-ave'/name;target.parent.mkdir(parents=True,exist_ok=True)
    shutil.copyfile(r/'artifacts/linux-ave'/name,target)
for name in ['20260909T061225Z-native-linux','20260909T062612Z-native-linux']:
    shutil.copytree(r/'logs'/name,out/'logs'/name)
dependencies={}
for name in ['linux','m1n1-linux','ave-video-qualified-20260908','prores-all-profiles-qualified-20260909']:
    f=r/'artifacts'/name/'manifest.json';assert f.exists()
    dependencies[name]=dict(path='artifacts/'+name,manifest_sha256=sha(f))
(out/'DEPENDENCIES.json').write_text(json.dumps(dict(required_existing_archives=dependencies),indent=2)+'\n')
(out/'SCOPE.json').write_text(json.dumps(dict(status='Linux validated',profiles=['HEVC Main10','HEVC Main42210'],
    source_bit_depth=10,output_bit_depth=10,chroma=['4:2:0','4:2:2'],frames=60,width=320,height=240,
    policy='Each changing input forces a keyframe; inter-frame scheduling is not qualified.',
    persistent_stage1_unchanged=True),indent=2)+'\n')
files={str(p.relative_to(out)):dict(bytes=p.stat().st_size,sha256=sha(p)) for p in sorted(out.rglob('*')) if p.is_file()}
(out/'manifest.json').write_text(json.dumps(dict(scope='Native Neo HEVC precision',files=files),indent=2)+'\n')
for name,record in files.items():
    p=out/name;assert p.stat().st_size==record['bytes'] and sha(p)==record['sha256']
print('HEVC_PRECISION_FROZEN',len(files),'files',sum(f['bytes'] for f in files.values()),'bytes')
