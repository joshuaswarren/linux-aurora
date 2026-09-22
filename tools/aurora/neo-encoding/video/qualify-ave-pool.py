#!/usr/bin/env python3
"""Archive larger-pool hardware evidence without claiming multiview encoding."""
import hashlib
import json
from pathlib import Path
import shutil

r=Path(__file__).resolve().parents[1]
results=r/'artifacts/ave-32m-linux-validation-20260909'
bundle=r/'artifacts/linux-ave-32m-candidate-20260909'
out=r/'artifacts/ave-pool-qualified-20260909'
v=json.loads((results/'validation.json').read_text())
assert v['passed'] and v['expanded_pool_mib']==32 and v['cpus_online']=='0-5'
assert v['arena_boundary_rejected'] and not v['multiview_encoding_validated']
assert hashlib.sha256((bundle/'apple-ave.ko').read_bytes()).hexdigest()==v['module_sha256']
for codec in ['h264','hevc','prores']:
    x=json.loads((results/codec/'validation.json').read_text())
    assert x['passed'] and x['frames']==3
out.mkdir(exist_ok=False)
shutil.copytree(results,out/'results')
shutil.copytree(bundle,out/'bundle',symlinks=False)
shutil.copytree(r/'logs/20260909T075420Z-native-linux',out/'native-boot')
source=out/'source';source.mkdir()
for name in ['encode-linux-video.py','encode-prores-linux.py','ave-relocate-avc.py','ave-relocate-hevc.py',
    'ave-make-command.py','ave-extract-frame.py','test-ave-pool-linux.py','qualify-ave-pool.py']:
    shutil.copyfile(r/'video'/name,source/name)
for name in ['test-native.sh','test-ipc.sh','run-command.sh']:
    shutil.copyfile(r/'video/linux-ave'/name,source/name)
shutil.copyfile(r/'artifacts/coverage-ave-config-20260909.bin',out/'ave-config.bin')
for name in ['apple-prores-test.ko','apple-prores-test-unstripped.ko']:
    shutil.copyfile(r/'artifacts/linux-prores'/name,out/name)
shutil.copyfile(r/'logs/ave-32m-candidate-build-20260909.log',out/'build.log')
shutil.copyfile(r/'logs/ave-32m-linux-validation-20260909.log',out/'run.log')
(out/'SCOPE.json').write_text(json.dumps(v,indent=2)+'\n')
files={str(p.relative_to(out)):dict(bytes=p.stat().st_size,sha256=hashlib.sha256(p.read_bytes()).hexdigest())
       for p in sorted(out.rglob('*')) if p.is_file()}
(out/'manifest.json').write_text(json.dumps(dict(scope='Bounded AVE pool and baseline regressions',files=files),indent=2)+'\n')
for name,record in files.items():
    p=out/name
    assert p.stat().st_size==record['bytes'] and hashlib.sha256(p.read_bytes()).hexdigest()==record['sha256']
print('AVE_POOL_QUALIFIED',len(files),'files',sum(x['bytes'] for x in files.values()),'bytes')
