#!/usr/bin/env python3
"""Freeze the first full IPC success, including the subsequent Config failure."""
import hashlib
import json
from pathlib import Path
import re
import shutil

root = Path(__file__).resolve().parents[1]
out = root / 'artifacts/ave-ipc-qualified-20260908'
ipc = (root / 'logs/ave-first-ipc.log').read_text(errors='replace')
status = dict(re.findall(r'^([a-z0-9_]+)=([^\r\n]+)', ipc, re.M))
for key, value in {'state':'ipc-ready', 'ipc_ready':'1', 'start_result':'0',
                   'channel0_name':'IO', 'channel0_slots':'992',
                   'channel1_name':'IO_T2H', 'channel1_slots':'993',
                   'heap_verified_pages':'48', 'encoder_registered':'0'}.items():
    assert status[key] == value, (key, status.get(key))
assert re.search(r'^AVE_NATIVE_IPC_PASS$', ipc, re.M)
health = (root / 'logs/ave-ipc-config-diagnostic.log').read_text(errors='replace')
assert re.search(r'^0-5$', health, re.M)
assert re.search(r'^g17p-vulkan-compute: PASS', health, re.M)
assert 'ASSERT: AddSharedMemory size != 0' in health
config = (root / 'logs/ave-first-config.log').read_text(errors='replace')
assert re.search(r'^transport_acks=1$', config, re.M)
assert re.search(r'^replies=0$', config, re.M)
assert re.search(r'^command_failed=1$', config, re.M)
assert not re.search(r'(?im)^\[.*(?:kernel panic|SError|dart.*(?:fault|translation error))', health)
files = [
    'video/linux-ave/apple-ave.c', 'video/linux-ave/apple-ave-ipc.h',
    'video/linux-ave/Makefile', 'video/linux-ave/STARTUP.md',
    'video/linux-ave/test-native.sh', 'video/linux-ave/test-ipc.sh',
    'video/linux-ave/t8140-ave.dtsi', 'scripts/build-ave.sh',
    'video/ave-asm-flow.py', 'video/qualify-ave-ipc.py',
    'artifacts/linux-ave/apple-ave.ko', 'artifacts/linux-ave/apple-ave-unstripped.ko',
    'artifacts/linux-ave/t8140-j700.dtb', 'artifacts/linux-ave/manifest.json',
    'artifacts/ave-config-option17-zero-failed.bin',
    'logs/ave-first-ipc.log', 'logs/ave-first-config.log',
    'logs/ave-ipc-config-diagnostic.log', 'logs/ave-ipc-native-cold.log',
    'logs/build-ave-ipc.log', 'logs/push-ave-ipc-module.log',
]
for name in ['20260908T214530Z-native-linux', '20260908T214550Z-ave-firmware']:
    files += [str(p.relative_to(root)) for p in (root / 'logs' / name).iterdir() if p.is_file()]
for name in ['ipc-negotiation-helpers.asm', 'ipc-ring-init.asm', 'ipc-send-receive.asm',
             'ipc-config-context.asm', 'ipc-config-defaults.asm']:
    files.append('artifacts/ave2-25G76/' + name)
out.mkdir(exist_ok=False)
for name in files:
    dst = out / name
    dst.parent.mkdir(parents=True, exist_ok=True)
    shutil.copy2(root / name, dst)
validation = dict(full_ipc_negotiation=True, channels=2, slots=[992, 993],
                  firmware_dma_written_descriptors=True, cpus_online='0-5',
                  vulkan_exact_values=32, config_transport_ack=True,
                  config_completed=False, config_failure='AddSharedMemory size assertion',
                  encoded_linux_frames=0, persistent_installation_changed=False)
(out / 'validation.json').write_text(json.dumps(validation, indent=2) + '\n')
hashes = {}
for p in sorted(out.rglob('*')):
    if p.is_file():
        data = p.read_bytes()
        hashes[str(p.relative_to(out))] = dict(bytes=len(data), sha256=hashlib.sha256(data).hexdigest())
(out / 'manifest.json').write_text(json.dumps(dict(files=hashes), indent=2) + '\n')
print(f'AVE_IPC_QUALIFIED files={len(hashes)} config_completed=0 encoded_frames=0')
