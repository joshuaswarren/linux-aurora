#!/usr/bin/env python3
"""Freeze independently verified native Linux H.264 and HEVC video results."""
import argparse
import hashlib
import json
from pathlib import Path
import re
import shutil

ROOT = Path(__file__).resolve().parents[1]
p = argparse.ArgumentParser(description=__doc__)
p.add_argument('output', type=Path)
p.add_argument('--h264', type=Path, required=True)
p.add_argument('--hevc', type=Path, required=True)
p.add_argument('--module-push-log', type=Path, required=True)
p.add_argument('--boot', type=Path, required=True)
a = p.parse_args()
assert not a.output.exists()
bundle = ROOT / 'artifacts/linux-ave'
build = json.loads((bundle / 'manifest.json').read_text())
for name, record in build['files'].items():
    data = (bundle / name).read_bytes()
    assert len(data) == record['bytes'] and hashlib.sha256(data).hexdigest() == record['sha256']
module_sha = build['files']['apple-ave.ko']['sha256']
assert module_sha in a.module_push_log.read_text()
assert 'NEO_NATIVE_LINUX_SHELL_VERIFIED' in (a.boot / 'console.log').read_text()
results = {}
for codec, directory in [('h264', a.h264), ('hevc', a.hevc)]:
    report = json.loads((directory / 'validation.json').read_text())
    assert report['passed'] and report['codec'] == codec and report['frames'] >= 60
    assert report['session_closed'] and report['cpus_online'] == '0-5' and report['vulkan_values'] == 32
    assert report['hardware_source'] == 'Neo native Linux AVE'
    assert (report['width'], report['height'], report['fps']) == (320, 240, 30)
    assert set(report['containers']) == {'mp4', 'mkv'}
    raw = (directory / 'decoded.nv12').read_bytes()
    assert len(raw) == report['frames'] * 115200
    assert hashlib.sha256(raw).hexdigest() == report['decoded_sha256']
    for suffix in ['mp4', 'mkv']:
        assert (directory / (suffix + '-decoded.nv12')).read_bytes() == raw
        stream = report['containers'][suffix]['streams'][0]
        assert stream['codec_name'] == codec and int(stream['nb_read_frames']) == report['frames']
    assert min(v for v in report['per_frame_psnr_db'] if v is not None) >= 35
    health = (directory / 'post-health.log').read_text()
    assert re.search(r'^0-5$', health, re.M) and re.search(r'^g17p-vulkan-compute: PASS', health, re.M)
    assert not re.search(r'(CRIT:|kernel panic|SError|dart.*(?:fault|translation error))', health)
    results[codec] = report

a.output.mkdir(parents=True)
for codec, directory in [('h264', a.h264), ('hevc', a.hevc)]:
    shutil.copytree(directory, a.output / 'results' / codec)
for path in [*list((ROOT / 'video').glob('*.py')), *list((ROOT / 'video').glob('*.md')),
             *list((ROOT / 'video').glob('*.m')), *list((ROOT / 'video/linux-ave').glob('*')),
             ROOT / 'scripts/build-ave.sh', ROOT / 'j700-boot-linux.sh', ROOT / 'boot-linux.py',
             ROOT / 'console.py', ROOT / 'push-linux.py', ROOT / 'HANDOFF.md',
             ROOT / 'artifacts/m1n1-linux-ave.patch']:
    if not path.is_file():
        continue
    dest = a.output / path.relative_to(ROOT)
    dest.parent.mkdir(parents=True, exist_ok=True)
    shutil.copy2(path, dest)
for name in ['apple-ave.ko', 'apple-ave-unstripped.ko', 't8140-j700.dtb', 'manifest.json']:
    dest = a.output / 'artifacts/linux-ave' / name
    dest.parent.mkdir(parents=True, exist_ok=True)
    shutil.copy2(bundle / name, dest)
for name in ['ave-hevc-capture-20260908', 'm1n1-linux']:
    shutil.copytree(ROOT / 'artifacts' / name, a.output / 'artifacts' / name)
shutil.copytree(ROOT / 'artifacts/ave-capture-20260908/three-frames-rings',
                a.output / 'artifacts/ave-capture-20260908/three-frames-rings')
shutil.copytree(a.boot, a.output / 'native-boot')
shutil.copy2(a.module_push_log, a.output / 'module-upload.log')
(a.output / 'DEPENDENCIES.md').write_text(
    '# Reproducing this qualified encoder\n\n'
    'This bundle includes the exact encoder module, unstripped ELF, DTB, source, '
    'command captures, native boot logs and video evidence. The unchanged Linux '
    'Image, vmlinux, initramfs and other baseline files remain under '
    '`neo-linux/artifacts/linux/`. Their exact sizes and SHA-256 hashes are pinned '
    'in `artifacts/linux-ave/manifest.json` here. Copy that baseline directory '
    'alongside this bundle when transferring hosts. The controller and guarded '
    'KIS boot environment are described in `video/WORKING_LINUX_ENCODING.md`.\n')
summary = dict(passed=True, native_linux_hardware=True,
               codecs={k: dict(frames=v['frames'], minimum_psnr_db=min(x for x in v['per_frame_psnr_db'] if x is not None),
                               containers=list(v['containers']), client=v['client']) for k, v in results.items()},
               width=320, height=240, fps=30, all_sessions_closed=True,
               cpus_online='0-5', vulkan_exact_values=32, module_sha256=module_sha,
               kernel_release=build['kernel_release'], standard_encoder_api=False,
               throughput_benchmark=False, persistent_installation_changed=False)
(a.output / 'validation.json').write_text(json.dumps(summary, indent=2) + '\n')
files = {}
for path in sorted(a.output.rglob('*')):
    if path.is_file():
        data = path.read_bytes()
        files[str(path.relative_to(a.output))] = dict(bytes=len(data), sha256=hashlib.sha256(data).hexdigest())
(a.output / 'manifest.json').write_text(json.dumps(dict(files=files), indent=2) + '\n')
print('AVE_MULTICODEC_VIDEO_QUALIFIED', json.dumps(summary))
