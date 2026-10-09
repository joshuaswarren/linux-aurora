#!/usr/bin/env python3
"""Freeze native ProRes 12-bit color / 16-bit alpha qualification."""
import hashlib
import json
from pathlib import Path
import shutil

r=Path(__file__).resolve().parents[1]
out=r/'artifacts/prores-alpha-precision-qualified-20260909'
assert not out.exists()
sha=lambda p:hashlib.sha256(p.read_bytes()).hexdigest()
module=r/'artifacts/linux-prores/apple-prores-test.ko'
results=[]
for codec in ['ap4h','ap4x']:
    for mode,frames in [('first',1),('moving',60)]:
        path=r/'artifacts'/f'{codec}-alpha12-{mode}-linux-20260909'
        data=json.loads((path/'validation.json').read_text());precision=data['precision_validation']
        assert data['passed'] and data['frames']==frames and data['fourcc']==codec
        assert data['first_frame_matches_macos_reference'] and data['first_bitstream_matches_macos_except_padding']
        assert precision['source_color_bits']==12 and precision['source_alpha_bits']==16
        assert precision['decoded_alpha_bits']==16 and precision['alpha_matches_all_source_bits']
        assert precision['alpha_max_error_at_16_bits']==0 and data['module_sha256']==sha(module)
        final=(path/'final.log').read_text();assert '\n0-5\n' in final and 'g17p-vulkan-compute: PASS' in final
        results.append(path)
for codec in ['h264','hevc','prores']:
    path=r/'artifacts'/f'prores-alpha-{codec}-regression-20260909'
    data=json.loads((path/'validation.json').read_text());assert data['passed'] and data['frames']==3
    if codec!='prores':assert data['session_closed'] and data['cpus_online']=='0-5' and data['vulkan_values']==32
    results.append(path)
out.mkdir()
for path in results:shutil.copytree(path,out/'results'/path.name)
for directory in ['video/linux-prores','artifacts/prores-alpha12-packets-20260909',
    'artifacts/coverage-probes-alpha16-decode-20260909','artifacts/coverage-probes-auxiliary-symbols-20260909',
    'artifacts/auxiliary-reference-20260909','artifacts/ap4h-alpha12-reference-validation-20260909',
    'artifacts/ap4x-alpha12-reference-validation-20260909','artifacts/ap4h-alpha16-host-reference-decode-20260909',
    'artifacts/ap4x-alpha16-host-reference-decode-20260909']:
    shutil.copytree(r/directory,out/directory)
for name in ['make-prores-packet.py','encode-prores-linux.py','qualify-prores-alpha.py',
    'validate-prores-alpha-reference.py','neo-vt-decode-probe.m','trace-j700-media.py','prores-packetgen.c']:
    shutil.copyfile(r/'video'/name,out/'video'/name)
(out/'scripts').mkdir()
shutil.copyfile(r/'scripts/build-prores-packetgen.sh',out/'scripts/build-prores-packetgen.sh')
for name in ['prores-packetgen','prores-packetgen-unstripped','prores-packetgen.c','validation.json']:
    target=out/'artifacts/prores-packetgen-20260909'/name;target.parent.mkdir(parents=True,exist_ok=True)
    shutil.copyfile(r/'artifacts/prores-packetgen-20260909'/name,target)
for name in ['apple-prores-test.ko','apple-prores-test-unstripped.ko']:
    target=out/'artifacts/linux-prores'/name;target.parent.mkdir(parents=True,exist_ok=True)
    shutil.copyfile(r/'artifacts/linux-prores'/name,target)
shutil.copytree(r/'logs/20260909T065946Z-native-linux',out/'logs/native-boot')
source=r.parent/'neo-update/logs/20260909-063952Z-kis-90488/artifacts/hv'
(out/'logs/reference-hv').mkdir()
for p in source.iterdir():
    if p.is_file() and ('apr' in p.name or 'media' in p.name):shutil.copyfile(p,out/'logs/reference-hv'/p.name)
dependencies={}
for name in ['linux','m1n1-linux','ave-video-qualified-20260908','prores-all-profiles-qualified-20260909']:
    f=r/'artifacts'/name/'manifest.json';assert f.exists()
    dependencies[name]=dict(path='artifacts/'+name,manifest_sha256=sha(f))
(out/'DEPENDENCIES.json').write_text(json.dumps(dict(required_existing_archives=dependencies),indent=2)+'\n')
(out/'SCOPE.json').write_text(json.dumps(dict(status='Linux validated',profiles=['ProRes 4444','ProRes 4444 XQ'],
    source_color_bits=12,source_alpha_bits=16,decoded_alpha_bits=16,frames=60,width=320,height=240,
    alpha_bit_exact=True,reference_compressed_match_except_zero_alignment=True,
    persistent_stage1_unchanged=True),indent=2)+'\n')
files={str(p.relative_to(out)):dict(bytes=p.stat().st_size,sha256=sha(p)) for p in sorted(out.rglob('*')) if p.is_file()}
(out/'manifest.json').write_text(json.dumps(dict(scope='Native Neo ProRes precision and transparency',files=files),indent=2)+'\n')
for name,record in files.items():
    p=out/name;assert p.stat().st_size==record['bytes'] and sha(p)==record['sha256']
print('PRORES_ALPHA_PRECISION_FROZEN',len(files),'files',sum(f['bytes'] for f in files.values()),'bytes')
