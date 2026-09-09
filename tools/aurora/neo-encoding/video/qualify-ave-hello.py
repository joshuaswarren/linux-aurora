#!/usr/bin/env python3
"""Validate the saved native hello evidence and freeze its exact artifacts."""
import hashlib
import json
from pathlib import Path
import re
import shutil

ROOT = Path(__file__).resolve().parents[1]
OUT = ROOT / 'artifacts/ave-hello-qualified-20260908'
LOG = ROOT / 'logs'


def read(name):
    return (LOG / name).read_text(errors='replace')


hello = read('ave-first-hello.log')
status = dict(re.findall(r'^([a-z0-9_]+)=([^\r\n]+)', hello, re.M))
expected = {'state': 'firmware-response', 'firmware_start_requested': '1',
            'start_result': '0', 'fw0_verified_pages': '83', 'fw1_verified_pages': '76',
            'ipc_verified_pages': '1280', 'queue_verified_pages': '4',
            'log_verified_pages': '9', 'mailbox_status': '0x1', 'core_control': '0x10',
            'core_status': '0x6c', 'resources_pinned_until_reboot': '1',
            'encoder_registered': '0'}
for key, value in expected.items():
    assert status[key] == value, (key, status.get(key))
offer = [int(status[f'scratch{i}'], 0) for i in range(4)]
assert offer == [2, 0x1f280, 0x100, 0xc0000]
guards = read('ave-hello-guards-and-dmesg.log')
for marker in ['AVE_REPEAT_GUARD_PASS', 'AVE_PIN_GUARD_PASS']:
    assert re.search('^' + marker + '$', guards, re.M)
assert not re.search(r'^AVE_.*GUARD_FAIL$', guards, re.M)
starts = re.findall(r'^\[\s*([\d.]+)\] apple-ave .*AVE_START_ONCE', guards, re.M)
replies = re.findall(r'^\[\s*([\d.]+)\] apple-ave .*AVE_HELLO_RESULT result=0', guards, re.M)
assert len(starts) == len(replies) == 1
assert not re.search(r'(?im)^\[.*(?:kernel panic|SError|dart.*(?:fault|translation error))', guards)
health = read('ave-hello-post-health.log')
assert re.search(r'^0-5$', health, re.M)
assert re.search(r'^g17p-vulkan-compute: PASS', health, re.M)
cold = read('ave-native-firmware-test.log')
assert re.search(r'^AVE_NATIVE_FIRMWARE_PASS ', cold, re.M)
assert cold.count('\nstate=firmware-mapped\n') == 2
assert 'Ran 6 tests' in read('test-ave-boot-preflight.log')
assert re.search(r'^OK$', read('test-ave-boot-preflight.log'), re.M)

bundle = ROOT / 'artifacts/linux-ave'
manifest = json.loads((bundle / 'manifest.json').read_text())
for name, item in manifest['files'].items():
    data = (bundle / name).read_bytes()
    assert len(data) == item['bytes'] and hashlib.sha256(data).hexdigest() == item['sha256']
stage2 = ROOT / 'artifacts/m1n1-linux'
stage_manifest = json.loads((stage2 / 'manifest.json').read_text())
for name, item in stage_manifest['files'].items():
    data = (stage2 / name).read_bytes()
    assert len(data) == item['bytes'] and hashlib.sha256(data).hexdigest() == item['sha256']

files = [
    'artifacts/linux-ave/apple-ave.ko', 'artifacts/linux-ave/apple-ave-unstripped.ko',
    'artifacts/linux-ave/t8140-j700.dtb', 'artifacts/linux-ave/manifest.json',
    'artifacts/linux/manifest.json', 'artifacts/m1n1-linux-ave.patch',
    'video/linux-ave/apple-ave.c', 'video/linux-ave/t8140-ave.dtsi',
    'video/linux-ave/Makefile', 'video/linux-ave/check-dtb.py',
    'video/linux-ave/test-native.sh', 'video/linux-ave/STARTUP.md',
    'video/ave-boot-preflight.py', 'video/test-ave-boot-preflight.py',
    'video/qualify-ave-hello.py', 'video/ENCODER_HANDOFF.md', 'HANDOFF.md',
    'scripts/build-ave.sh', 'scripts/build-m1n1-linux.sh', 'boot-linux.py',
    'j700-boot-linux.sh',
]
files += ['logs/' + name for name in [
    'ave-first-hello.log', 'ave-hello-preflight.log', 'ave-hello-post-health.log',
    'ave-hello-guards-and-dmesg.log', 'ave-native-firmware-test.log',
    'ave-firmware-post-health.log', 'test-ave-boot-preflight.log',
    'build-ave-hello.log', 'build-m1n1-linux-ave.log', 'ave-dtb-validation.json',
    'boot-linux-ave-firmware.log', 'push-ave-hello.log',
]]
for directory in [stage2, LOG / '20260908T205425Z-ave-firmware',
                  LOG / '20260908T205404Z-native-linux']:
    files += [str(path.relative_to(ROOT)) for path in directory.iterdir() if path.is_file()]
OUT.mkdir(exist_ok=False)
for name in sorted(set(files)):
    target = OUT / name
    target.parent.mkdir(parents=True, exist_ok=True)
    shutil.copy2(ROOT / name, target)
validation = dict(passed=True, milestone='Native Linux AVE firmware first reply',
    native_boots=1, firmware_starts=1, reply_ms=round((float(replies[0])-float(starts[0]))*1000, 3),
    offer=[hex(v) for v in offer], firmware_pages=159, coherent_buffer_pages=1293,
    firmware_contents_match_boot=True, cold_load_unload_reload=True,
    repeat_start_rejected=True, unload_after_start_blocked=True,
    cpus_online='0-5', vulkan_exact_values=32, full_ipc_negotiation=False,
    encoded_linux_frames=0, persistent_installation_changed=False,
    current_state='Firmware reply pending; module/DMA/PM pinned until target reboot')
(OUT / 'validation.json').write_text(json.dumps(validation, indent=2) + '\n')
hashes = {}
for path in sorted(OUT.rglob('*')):
    if path.is_file():
        data = path.read_bytes()
        hashes[str(path.relative_to(OUT))] = dict(bytes=len(data), sha256=hashlib.sha256(data).hexdigest())
(OUT / 'manifest.json').write_text(json.dumps(dict(files=hashes), indent=2) + '\n')
print(f'AVE_HELLO_QUALIFIED files={len(hashes)} reply_ms={validation["reply_ms"]}')
