#!/usr/bin/env python3
"""Freeze hardware-proven native ProRes and same-boot codec regressions."""
import hashlib
import json
from pathlib import Path
import shutil

root = Path(__file__).resolve().parents[1]
out = root / 'artifacts/prores-linux-qualified-20260909'
assert not out.exists()
expected_module = '777b166b972028e6dbc92888cc2842f8aafc746028aaa37cd8758a840703df51'
assert hashlib.sha256((root / 'artifacts/linux-prores/apple-prores-test.ko').read_bytes()).hexdigest() == expected_module
names = ['prores-linux-orderfix-20260909', 'prores-linux-60-20260909', 'prores-after-ave-20260909',
         'ave-prores-h264-regression-20260909', 'ave-prores-hevc-regression-20260909']
for name in names:
    result = json.loads((root / 'artifacts' / name / 'validation.json').read_text())
    assert result['passed']
    if name.startswith('prores'):
        assert result['module_sha256'] == expected_module and result['first_frame_matches_macos_reference']
    else:
        assert result['session_closed'] and result['cpus_online'] == '0-5' and result['vulkan_values'] == 32
assert json.loads((root / 'artifacts/prores-linux-60-20260909/validation.json').read_text())['frames'] == 60
out.mkdir()
for name in names + ['prores-linux-first-20260909', 'prores-linux-attempt-20260909']:
    shutil.copytree(root / 'artifacts' / name, out / 'artifacts' / name)
build = out / 'artifacts/linux-prores'
build.mkdir()
for name in ['apple-prores-test.ko', 'apple-prores-test-unstripped.ko', 't8140-j700.dtb', 'manifest.json']:
    shutil.copyfile(root / 'artifacts/linux-prores' / name, build / name)
shutil.copytree(root / 'video/linux-prores', out / 'video/linux-prores')
for name in ['encode-prores-linux.py', 'make-prores-packet.py', 'OTHER_CODECS.md', 'qualify-prores-linux.py']:
    shutil.copyfile(root / 'video' / name, out / 'video' / name)
(out / 'scripts').mkdir()
shutil.copyfile(root / 'scripts/build-prores.sh', out / 'scripts/build-prores.sh')
(out / 'logs').mkdir()
for name in ['build-prores-orderfix.log', 'prores-dtb-validation.json', 'prores-failure-state.log',
             'boot-linux-prores-orderfix.log', 'prores-native-60.log', 'ave-prores-regression-setup.log',
             'ave-prores-h264-regression.log', 'ave-prores-hevc-regression.log', 'prores-after-ave.log',
             'all-codecs-final-state.log']:
    shutil.copyfile(root / 'logs' / name, out / 'logs' / name)
shutil.copytree(root / 'logs/20260909T033655Z-native-linux', out / 'logs/20260909T033655Z-native-linux')
dependencies = {'note': 'Transfer these existing directories alongside this archive; they contain the pinned kernel, initramfs, matching kernel symbols, Stage 2 and macOS reference captures.', 'directories': {}}
for name in ['linux', 'm1n1-linux', 'other-codecs-reference-qualified-20260909', 'ave-video-qualified-20260908']:
    directory = root / 'artifacts' / name
    manifest = directory / 'manifest.json'
    dependencies['directories'][name] = {'relative_to_neo_linux': 'artifacts/' + name,
        'manifest_sha256': hashlib.sha256(manifest.read_bytes()).hexdigest() if manifest.exists() else None}
(out / 'DEPENDENCIES.json').write_text(json.dumps(dependencies, indent=2) + '\n')
files = {str(p.relative_to(out)): {'bytes': p.stat().st_size, 'sha256': hashlib.sha256(p.read_bytes()).hexdigest()}
         for p in sorted(out.rglob('*')) if p.is_file()}
(out / 'manifest.json').write_text(json.dumps({'scope': 'Native Neo ProRes422 hardware qualification and H.264/HEVC coexistence regressions', 'files': files}, indent=2) + '\n')
for name, record in files.items():
    data = (out / name).read_bytes()
    assert len(data) == record['bytes'] and hashlib.sha256(data).hexdigest() == record['sha256']
print('NATIVE_PRORES_FROZEN', len(files), 'files', sum(record['bytes'] for record in files.values()), 'bytes')
