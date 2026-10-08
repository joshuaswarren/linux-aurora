#!/usr/bin/env python3
"""Stop the verified Neo desktop for isolated native CTS, without unloading drivers."""
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import time

boot = sys.argv[1]
assert os.geteuid() == 0
assert Path('/proc/sys/kernel/random/boot_id').read_text().strip() == boot
assert b'apple,j700' in Path('/proc/device-tree/compatible').read_bytes().split(b'\0')
assert Path('/sys/module/appledrm').exists()
assert subprocess.run(['systemctl', 'is-active', '--quiet', 'sddm']).returncode == 0
assert not Path('/run/neo-gl30-cts-prepared').exists()
prefix = Path('/opt/neo-mesa-native-gl2-20260913')
assert hashlib.sha256((prefix/'lib/libgallium-26.1.4.so').read_bytes()).hexdigest() == '6b660621245899d0fa0ab916e82201232d32b3774006bff77aa0f98336f3e96e'
cts = Path('/opt/neo-cts-opengl-4.6.8.1')
assert (cts/'source-commit.txt').read_text().strip() == '067e8832315e79817ede1c4863804e440f5d1c80'
manifest = json.loads((cts/'package-manifest.json').read_text())
for name, entry in manifest.items():
    path = cts/name
    assert path.is_file() and path.stat().st_size == entry['size'], name
    assert hashlib.sha256(path.read_bytes()).hexdigest() == entry['sha256'], name
before = subprocess.check_output(['dmesg'])
fault = rb'asahi[^\n]*(?:halted GMMU|completion timed out|retiring slot)|translation fault|GPU FAULT'
assert not re.search(fault, before), 'A fresh boot is required'
out = Path('/home/mcruz/gl30-cts-20260913')/boot
out.mkdir(parents=True, exist_ok=False)
(out/'kernel-before-desktop-stop.log').write_bytes(before)
(out/'runtime.json').write_text(json.dumps({
    'boot': boot, 'kernel': subprocess.check_output(['uname', '-a'], text=True).strip(),
    'mesa_sha256': '6b660621245899d0fa0ab916e82201232d32b3774006bff77aa0f98336f3e96e',
    'cts_commit': (cts/'source-commit.txt').read_text().strip(),
    'cts_manifest_sha256': hashlib.sha256((cts/'package-manifest.json').read_bytes()).hexdigest(),
    'cts_runner_sha256': manifest['cts-runner']['sha256'],
}, indent=2)+'\n')
subprocess.run(['systemctl', 'stop', 'sddm'], check=True, timeout=45)
def drm_clients():
    clients = set()
    for proc in Path('/proc').glob('[0-9]*'):
        try:
            for fd in (proc/'fd').iterdir():
                try:
                    if str(fd.readlink()).startswith('/dev/dri/'):
                        clients.add(int(proc.name))
                except OSError:
                    continue
        except (FileNotFoundError, PermissionError):
            continue
    return sorted(clients)
for _ in range(60):
    clients = drm_clients()
    if not clients:
        break
    time.sleep(0.5)
assert not clients, f'DRM clients remain after desktop stop: {clients}'
assert not re.search(fault, subprocess.check_output(['dmesg']))
Path('/run/neo-gl30-cts-prepared').write_text(boot+'\n')
print('NATIVE_CTS_PREPARED_NO_DRM_CLIENTS')
