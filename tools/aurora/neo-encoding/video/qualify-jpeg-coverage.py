#!/usr/bin/env python3
"""Freeze JPEG/Motion JPEG qualification and its same-boot regressions."""
import hashlib
import json
from pathlib import Path
import shutil

r = Path(__file__).resolve().parents[1]
out = r/'artifacts/jpeg-mjpeg-qualified-20260909'
assert not out.exists()
sha = lambda p: hashlib.sha256(p.read_bytes()).hexdigest()
module = r/'artifacts/linux-jpeg/apple-jpeg-test.ko'
results = []
for mode, frames in [('first', 1), ('moving', 60)]:
    path = r/'artifacts'/f'jpeg-wrapper-{mode}-linux-20260909'
    data = json.loads((path/'validation.json').read_text())
    assert data['passed'] and data['frames'] == frames
    assert data['first_frame_matches_macos_reference']
    assert data['pixel_format'] == 'yuvj420p' and data['profile'] == 'Baseline'
    assert data['module_sha256'] == sha(module)
    final = (path/'final.log').read_text()
    assert 'g17p-vulkan-compute: PASS' in final and '\n0-5\n' in final
    results.append(path)
for codec in ['h264', 'hevc', 'prores']:
    path = r/'artifacts'/f'jpeg-coverage-{codec}-regression-20260909'
    data = json.loads((path/'validation.json').read_text())
    assert data['passed'] and data['frames'] == 3
    if codec != 'prores':
        assert data['session_closed'] and data['cpus_online'] == '0-5' and data['vulkan_values'] == 32
    results.append(path)
out.mkdir()
for path in results:
    shutil.copytree(path, out/'results'/path.name)
for directory in ['video/linux-jpeg', 'artifacts/jpeg-coverage-packet-20260909', 'artifacts/jpeg-25G76']:
    shutil.copytree(r/directory, out/directory)
for name in ['make-jpeg-packet.py', 'encode-jpeg-linux.py', 'qualify-jpeg-coverage.py', 'trace-j700-media.py']:
    shutil.copyfile(r/'video'/name, out/'video'/name)
(out/'scripts').mkdir()
shutil.copyfile(r/'scripts/build-jpeg.sh', out/'scripts/build-jpeg.sh')
for name in ['apple-jpeg-test.ko', 'apple-jpeg-test-unstripped.ko', 't8140-j700.dtb']:
    target = out/'artifacts/linux-jpeg'/name
    target.parent.mkdir(parents=True, exist_ok=True)
    shutil.copyfile(r/'artifacts/linux-jpeg'/name, target)
for name in ['20260909T061225Z-native-linux']:
    shutil.copytree(r/'logs'/name, out/'logs'/name)
for stamp in ['20260909-024939Z-kis-90524', '20260909-051934Z-kis-53742']:
    source = r.parent/'neo-update/logs'/stamp/'artifacts/hv'
    target = out/'logs'/stamp
    target.mkdir()
    for f in source.iterdir():
        if f.is_file() and ('jpeg' in f.name.lower() or 'media' in f.name.lower()):
            shutil.copyfile(f, target/f.name)
reference = r/'artifacts/other-codecs-reference-qualified-20260909/results/jpeg-hw'
shutil.copytree(reference, out/'reference/jpeg-hw')
dependencies = {}
for name in ['linux', 'm1n1-linux', 'ave-video-qualified-20260908',
             'prores-all-profiles-qualified-20260909', 'other-codecs-reference-qualified-20260909']:
    f = r/'artifacts'/name/'manifest.json'
    assert f.exists()
    dependencies[name] = dict(path='artifacts/'+name, manifest_sha256=sha(f))
(out/'DEPENDENCIES.json').write_text(json.dumps(dict(required_existing_archives=dependencies), indent=2)+'\n')
(out/'SCOPE.json').write_text(json.dumps(dict(status='Linux validated', codecs=['JPEG', 'Motion JPEG'],
    profile='Baseline', chroma='4:2:0', bit_depth=8, frames=60, width=320, height=240,
    input_layout='Captured full-range NV12 paired with video-range macOS source',
    persistent_stage1_unchanged=True, completion='Masked AIC; captured JPEG mask; raw wrapper polling'), indent=2)+'\n')
files = {str(p.relative_to(out)): dict(bytes=p.stat().st_size, sha256=sha(p))
         for p in sorted(out.rglob('*')) if p.is_file()}
(out/'manifest.json').write_text(json.dumps(dict(scope='Native Neo JPEG and Motion JPEG', files=files), indent=2)+'\n')
for name, record in files.items():
    p = out/name
    assert p.stat().st_size == record['bytes'] and sha(p) == record['sha256']
print('JPEG_MJPEG_FROZEN', len(files), 'files', sum(f['bytes'] for f in files.values()), 'bytes')
