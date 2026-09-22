#!/usr/bin/env python3
"""Freeze all six native ProRes profiles and the same-boot regression evidence."""
import hashlib
import json
from pathlib import Path
import shutil

r=Path(__file__).resolve().parents[1]
out=r/'artifacts/prores-all-profiles-qualified-20260909'
assert not out.exists()
module=r/'artifacts/linux-prores/apple-prores-test.ko'
module_hash=hashlib.sha256(module.read_bytes()).hexdigest()
results=[]
for name,codec in [('hq','apch'),('lt','apcs'),('proxy','apco'),('4444','ap4h'),('xq','ap4x')]:
    for mode,frames in [('first',1),('moving',60)]:
        path=r/'artifacts'/f'prores-{name}-{mode}-linux-20260909'
        data=json.loads((path/'validation.json').read_text())
        assert data['passed'] and data['frames']==frames and data['fourcc']==codec
        assert data['first_frame_matches_macos_reference'] and data['first_bitstream_matches_macos_except_padding']
        assert data['module_sha256']==module_hash
        final=(path/'final.log').read_text()
        assert 'g17p-vulkan-compute: PASS' in final and '\n0-5\n' in final
        results.append(path)
for codec in ['h264','hevc','standard']:
    path=r/'artifacts'/f'prores-coverage-{codec}-regression-20260909'
    data=json.loads((path/'validation.json').read_text());assert data['passed'] and data['frames']==3
    if codec!='standard':
        assert data['session_closed'] and data['cpus_online']=='0-5' and data['vulkan_values']==32
    results.append(path)
out.mkdir()
for path in results:shutil.copytree(path,out/'results'/path.name)
for name in ['coverage-reference-20260909','prores-planes-reference-20260909',
             'coverage-probes-20260909','prores-coverage-packets-20260909']:
    shutil.copytree(r/'artifacts'/name,out/'artifacts'/name)
for directory in ['video/linux-prores','scripts']:
    (out/directory).mkdir(parents=True,exist_ok=True)
shutil.copytree(r/'video/linux-prores',out/'video/linux-prores',dirs_exist_ok=True)
for name in ['encode-prores-linux.py','make-prores-packet.py','qualify-prores-coverage.py',
             'trace-j700-media.py','trace-j700-coverage.py','trace-j700-ave-commands.py','coverage-pixels.h']:
    shutil.copyfile(r/'video'/name,out/'video'/name)
shutil.copyfile(r/'scripts/build-prores.sh',out/'scripts/build-prores.sh')
for name in ['apple-prores-test.ko','apple-prores-test-unstripped.ko']:
    target=out/'artifacts/linux-prores'/name;target.parent.mkdir(parents=True,exist_ok=True)
    shutil.copyfile(r/'artifacts/linux-prores'/name,target)
for name in ['apple-ave.ko','apple-ave-unstripped.ko']:
    target=out/'artifacts/linux-ave'/name;target.parent.mkdir(parents=True,exist_ok=True)
    shutil.copyfile(r/'artifacts/linux-ave'/name,target)
shutil.copyfile(r/'artifacts/linux-jpeg/t8140-j700.dtb',out/'artifacts/t8140-j700.dtb')
shutil.copytree(r/'logs/20260909T054020Z-native-linux',out/'logs/native-boot')
shutil.copytree(r.parent/'neo-update/logs/20260909-051934Z-kis-53742/artifacts/hv',out/'logs/reference-hv')
dependencies={}
for name in ['linux','m1n1-linux','prores-linux-qualified-20260909','ave-video-qualified-20260908']:
    f=r/'artifacts'/name/'manifest.json';assert f.exists()
    dependencies[name]=dict(path='artifacts/'+name,manifest_sha256=hashlib.sha256(f.read_bytes()).hexdigest())
(out/'DEPENDENCIES.json').write_text(json.dumps(dict(required_existing_archives=dependencies,
    note='Transfer these matching kernel, symbols, Stage 2 and baseline archives with this profile qualification.'),indent=2)+'\n')
(out/'SCOPE.json').write_text(json.dumps(dict(status='Linux validated',profiles=['422','HQ','LT','Proxy','4444','4444 XQ'],
    new_profiles_frames=60,width=320,height=240,persistent_stage1_unchanged=True,
    precision_note='4444/XQ output is 12-bit with opaque alpha from 8-bit UYVY source. Variable alpha and 12-bit source qualification remain separate.',
    jpeg_module_loaded=False,reference_probe_rebuilds_byte_identical=True),indent=2)+'\n')
files={str(f.relative_to(out)):dict(bytes=f.stat().st_size,sha256=hashlib.sha256(f.read_bytes()).hexdigest())
       for f in sorted(out.rglob('*')) if f.is_file()}
(out/'manifest.json').write_text(json.dumps(dict(scope='Native Neo ProRes profile coverage',files=files),indent=2)+'\n')
for name,record in files.items():
    data=(out/name).read_bytes()
    assert len(data)==record['bytes'] and hashlib.sha256(data).hexdigest()==record['sha256']
print('PRORES_ALL_PROFILES_FROZEN',len(files),'files',sum(f['bytes'] for f in files.values()),'bytes')
