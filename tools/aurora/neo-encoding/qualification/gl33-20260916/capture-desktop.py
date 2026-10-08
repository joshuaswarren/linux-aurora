#!/usr/bin/env python3
"""Capture the restored Plasma session and two frames of its gears window."""
import json
import os
from pathlib import Path
import subprocess
import sys
import time

assert os.getuid() == 1000
boot = sys.argv[1]
assert Path('/proc/sys/kernel/random/boot_id').read_text().strip() == boot
assert b'apple,j700' in Path('/proc/device-tree/compatible').read_bytes().split(b'\0')
pids = subprocess.check_output(['pgrep', '-u', 'mcruz', '-x', 'plasmashell'], text=True).split()
assert len(pids) == 1
session = dict(item.split('=', 1) for item in
               Path('/proc', pids[0], 'environ').read_bytes().decode().split('\0')
               if '=' in item)
env = dict(os.environ)
for key in ('DISPLAY', 'XAUTHORITY', 'XDG_RUNTIME_DIR', 'DBUS_SESSION_BUS_ADDRESS'):
    if key in session:
        env[key] = session[key]
assert env.get('DISPLAY') and Path(env['XAUTHORITY']).is_file()
out = Path('/home/mcruz/gl30-cts-20260913') / ('desktop-restored-' + boot)
out.mkdir(exist_ok=False)
capture = ['import', '-limit', 'thread', '2', '-depth', '8', '-define', 'png:compression-level=0']
with (out / 'gears.log').open('wb') as log:
    subprocess.Popen(['/usr/local/libexec/neo-glxgears-rgb10'], env=env,
                     stdout=log, stderr=subprocess.STDOUT, start_new_session=True)
time.sleep(8)
windows = subprocess.check_output(['wmctrl', '-l'], env=env, text=True).splitlines()
gears = [line.split()[0] for line in windows if line.endswith(' glxgears')]
assert len(gears) == 1, windows
subprocess.run([*capture, '-window', 'root', str(out / 'desktop-with-gears.png')],
               env=env, check=True, timeout=30)
for name in ('gears-a.png', 'gears-b.png'):
    subprocess.run([*capture, '-window', gears[0], str(out / name)],
                   env=env, check=True, timeout=30)
    time.sleep(1)
(out / 'receipt.json').write_text(json.dumps(dict(
    boot_id=boot, gears_window=gears[0], plasma_pid=pids[0],
    physical_cursor_qualification=False), indent=2) + '\n')
print('RESTORED_DESKTOP_AND_GEARS_CAPTURED')
