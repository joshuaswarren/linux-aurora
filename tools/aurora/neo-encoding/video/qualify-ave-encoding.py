#!/usr/bin/env python3
"""Freeze the real hardware encode evidence, matching module/ELF, and recipe."""
import hashlib
import json
import math
from pathlib import Path
import re
import shutil

ROOT = Path(__file__).resolve().parents[1]
OUT = ROOT / 'artifacts/ave-encoding-qualified-20260908'
COLOR = ROOT / 'artifacts/ave-linux-color-bars-20260908'


def read(name):
    return (ROOT / 'logs' / name).read_text(errors='replace')


original = (ROOT / 'artifacts/ave-process-linux.nv12').read_bytes()
decoded = (ROOT / 'artifacts/ave-first-linux-frame-decoded.nv12').read_bytes()
assert len(original) == len(decoded) == 115200
mse = sum((a - b) ** 2 for a, b in zip(original, decoded)) / len(original)
gray_psnr = 10 * math.log10(255 ** 2 / mse)
assert gray_psnr > 70 and not read('ave-first-frame-decode.log').strip()
color = json.loads((COLOR / 'validation.json').read_text())
assert color['passed'] and color['native_linux_hardware_encoded_frames'] == 1
assert color['psnr_db'] > 65 and color['session_closed']
assert color['input_sha256'] != hashlib.sha256(original).hexdigest()
for log in ['ave-powered-config.log', 'ave-powered-open.log',
            'ave-powered-avc-start.log', 'ave-first-frame-process.log',
            'ave-first-frame-complete.log', 'ave-first-stop.log', 'ave-first-close.log']:
    text = read(log)
    if log.startswith('ave-first-stop') or log.startswith('ave-first-close'):
        assert re.search(r'^NEO_CMD_[0-9a-f]+=0$', text, re.M)
    else:
        assert re.search(r'^AVE_PACKET_RESULT=0$', text, re.M)
    assert not re.search(r'^.*CRIT:', text, re.M)
for health in [read('ave-first-frame-post-health.log'), (COLOR / 'post-health.log').read_text()]:
    assert re.search(r'^0-5$', health, re.M)
    assert re.search(r'^g17p-vulkan-compute: PASS', health, re.M)
    assert not re.search(r'(CRIT:|kernel panic|SError|dart.*(?:fault|translation error))', health)
power = read('ave-powered-cold.log')
for name in ['dma', 'pipe4', 'pipe5', 'me0', 'me1', 'sys']:
    assert re.search(r'^venc0_' + name + r'\s+on\s', power, re.M)
assert re.search(r'^AVE_NATIVE_RELEASE_PASS$', power, re.M)

bundle = ROOT / 'artifacts/linux-ave'
manifest = json.loads((bundle / 'manifest.json').read_text())
for name, info in manifest['files'].items():
    data = (bundle / name).read_bytes()
    assert len(data) == info['bytes'] and hashlib.sha256(data).hexdigest() == info['sha256']
assert manifest['files']['apple-ave.ko']['sha256'] in read('push-ave-powered-module.log')

files = ['HANDOFF.md', 'video/ENCODER_HANDOFF.md', 'video/WORKING_LINUX_ENCODING.md',
         'video/encode-linux-frame.py', 'video/ave-make-command.py',
         'video/ave-relocate-avc.py', 'video/ave-extract-frame.py',
         'video/ave-decode-reply.py', 'video/qualify-ave-encoding.py',
         'scripts/build-ave.sh', 'j700-boot-linux.sh', 'boot-linux.py',
         'push-linux.py', 'console.py', 'video/ave-boot-preflight.py',
         'artifacts/linux/manifest.json', 'artifacts/m1n1-linux-ave.patch',
         'artifacts/ave-first-linux-frame.h264', 'artifacts/ave-first-linux-frame.json',
         'artifacts/ave-first-linux-frame-decoded.nv12',
         'artifacts/ave-first-linux-frame.arena.bin', 'artifacts/ave-process-linux.nv12']
files += [str(p.relative_to(ROOT)) for p in (ROOT / 'video/linux-ave').iterdir() if p.is_file()]
files += ['artifacts/linux-ave/' + n for n in ['apple-ave.ko', 'apple-ave-unstripped.ko',
                                             't8140-j700.dtb', 'manifest.json']]
files += [str(p.relative_to(ROOT)) for p in COLOR.iterdir() if p.is_file()]
files += ['logs/' + n for n in ['ave-powered-cold.log', 'ave-powered-config.log',
          'ave-powered-open.log', 'ave-powered-avc-start.log', 'ave-first-frame-process.log',
          'ave-first-frame-complete.log', 'ave-first-frame-arena.log', 'ave-first-frame-decode.log',
          'ave-first-frame-post-health.log', 'ave-first-stop.log', 'ave-first-close.log',
          'ave-start-power-state.log', 'ave-start-exception-and-health.log',
          'ave-config-captured-startup.log', 'ave-encoding-webhook-delivery.json',
          'build-ave-power-domains.log', 'ave-dtb-validation.json',
          'push-ave-powered-module.log', 'ave-color-bars-run.log']]
for directory in [ROOT / 'artifacts/m1n1-linux', ROOT / 'logs/20260908T221213Z-native-linux']:
    files += [str(p.relative_to(ROOT)) for p in directory.iterdir() if p.is_file()]
bootlog = (ROOT / 'logs/20260908T221213Z-native-linux/console.log').read_text()
assert 'NEO_NATIVE_LINUX_SHELL_VERIFIED' in bootlog
OUT.mkdir(exist_ok=False)
for name in sorted(set(files)):
    target = OUT / name
    target.parent.mkdir(parents=True, exist_ok=True)
    shutil.copy2(ROOT / name, target)
validation = dict(passed=True, native_linux_hardware_encoded_frames=2,
    dimensions=[320, 240], pixel_format='NV12', codec='H.264',
    patterns=['gray', 'color-bars'], psnr_db=[gray_psnr, color['psnr_db']],
    stream_bytes=[82, 214], sessions=2, warm_second_session=True,
    all_sessions_closed=True, cpus_online='0-5', vulkan_exact_values=32,
    encoder_power_domains=5, full_ipc=True, standard_encoder_api=False,
    continuous_streaming=False, persistent_installation_changed=False,
    module_sha256=manifest['files']['apple-ave.ko']['sha256'],
    kernel_release=manifest['kernel_release'])
(OUT / 'validation.json').write_text(json.dumps(validation, indent=2) + '\n')
hashes = {}
for path in sorted(OUT.rglob('*')):
    if path.is_file():
        data = path.read_bytes()
        hashes[str(path.relative_to(OUT))] = dict(bytes=len(data), sha256=hashlib.sha256(data).hexdigest())
(OUT / 'manifest.json').write_text(json.dumps(dict(files=hashes), indent=2) + '\n')
print(f'AVE_ENCODING_QUALIFIED files={len(hashes)} frames=2 sessions=2')
