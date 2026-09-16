#!/usr/bin/env python3
"""Stop the verified test desktop before creating this boot's isolated Xorg."""
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import time

boot, expected_mesa = sys.argv[1:]
assert os.geteuid() == 0
assert Path('/proc/sys/kernel/random/boot_id').read_text().strip() == boot
assert b'apple,j700' in Path('/proc/device-tree/compatible').read_bytes().split(b'\0')
assert Path('/run/neo-gl30-cts-prepared').read_text().strip() == boot
assert subprocess.run(['systemctl', 'is-active', '--quiet', 'sddm']).returncode == 0
mesa = Path('/opt/neo-mesa-native-gl2-20260913/lib/libgallium-26.1.4.so')
assert hashlib.sha256(mesa.read_bytes()).hexdigest() == expected_mesa
campaign = Path('/home/mcruz/gl30-cts-20260913')/boot
assert not (campaign/'x11-server').exists()
fault = rb'asahi[^\n]*(?:halted GMMU|completion timed out|retiring slot)|translation fault|GPU FAULT'
before = subprocess.check_output(['dmesg'])
assert not re.search(fault, before)
pids = subprocess.check_output(['pgrep', '-u', 'mcruz', '-x', 'plasmashell'], text=True).split()
assert len(pids) == 1
session = dict(item.split('=', 1) for item in
               Path('/proc', pids[0], 'environ').read_bytes().decode().split('\0')
               if '=' in item)
launch = ['runuser', '-u', 'mcruz', '--', 'env', '-i', 'PATH=/usr/bin:/bin',
          'DISPLAY='+session['DISPLAY'], 'XAUTHORITY='+session['XAUTHORITY']]
windows = subprocess.check_output(launch+['wmctrl', '-l'], text=True).splitlines()
for line in windows:
    if line.endswith(' glxgears'):
        continue
    properties = subprocess.check_output(launch+[
        'xprop', '-id', line.split()[0], '_NET_WM_WINDOW_TYPE', 'WM_CLASS'], text=True)
    assert 'WM_CLASS(STRING) = "plasmashell", "plasmashell"' in properties, \
        'Unexpected application window; preserve it'
    assert any(kind in properties for kind in (
        '_NET_WM_WINDOW_TYPE_DESKTOP', '_NET_WM_WINDOW_TYPE_DOCK')), \
        'Unexpected Plasma window; preserve it'
out = campaign/'desktop-stop'
out.mkdir(exist_ok=False)
(out/'before.json').write_text(json.dumps(dict(boot=boot, window_count=len(windows)), indent=2)+'\n')
(out/'kernel-before.log').write_bytes(before)
subprocess.run(['systemctl', 'stop', 'sddm'], check=True, timeout=60)
for attempt in range(60):
    remaining = subprocess.run(['pgrep', '-x', 'Xorg|kwin_x11|plasmashell|glxgears'],
                               stdout=subprocess.PIPE).stdout
    if not remaining:
        break
    time.sleep(0.5)
assert not remaining, 'Desktop processes have not exited'
after = subprocess.check_output(['dmesg'])
(out/'kernel-after.log').write_bytes(after)
assert not re.search(fault, after)
print('VERIFIED_TEST_DESKTOP_STOPPED')
