#!/usr/bin/env python3
"""Start a dedicated authenticated Xorg for native EGL CTS clients."""
import json
import os
from pathlib import Path
import pwd
import re
import secrets
import subprocess
import sys
import time

boot = sys.argv[1]
assert os.geteuid() == 0
assert Path('/proc/sys/kernel/random/boot_id').read_text().strip() == boot
assert b'apple,j700' in Path('/proc/device-tree/compatible').read_bytes().split(b'\0')
assert Path('/run/neo-gl30-cts-prepared').read_text().strip() == boot
assert Path('/sys/module/appledrm').exists()
assert subprocess.run(['systemctl', 'is-active', '--quiet', 'sddm']).returncode != 0
assert subprocess.run(['pgrep', '-x', 'Xorg'], stdout=subprocess.DEVNULL).returncode == 1
fault = re.compile(rb'asahi[^\n]*(?:halted GMMU|completion timed out|retiring slot)|translation fault|GPU FAULT')
assert not fault.search(subprocess.check_output(['dmesg']))
for marker in ('neo-gpu1-graphics', 'neo-gpu-use-zink'):
    assert Path('/run', marker).exists(), 'Keep the qualified Xorg server renderer'
for proc in Path('/proc').glob('[0-9]*'):
    try:
        for fd in (proc/'fd').iterdir():
            try:
                assert not str(fd.readlink()).startswith('/dev/dri/'), proc.name
            except FileNotFoundError:
                continue
    except (FileNotFoundError, PermissionError):
        continue
out = Path('/home/mcruz/gl30-cts-20260913')/boot/'x11-server'
out.mkdir(exist_ok=False)
user = pwd.getpwnam('mcruz')
auth = Path('/run/neo-gl30-cts.Xauthority')
assert not auth.exists()
auth.touch(mode=0o600)
subprocess.run(['xauth', '-f', str(auth)], input='add :0 MIT-MAGIC-COOKIE-1 '+secrets.token_hex(16)+'\n',
               text=True, check=True, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
os.chown(auth, user.pw_uid, user.pw_gid)
command = ['/usr/local/libexec/neo-xorg-rgb10-gpu1', ':0', 'vt2', '-nolisten', 'tcp',
           '-noreset', '-auth', str(auth), '-config', '/etc/X11/neo-dcp.conf',
           '-configdir', '/etc/X11/neo-empty.conf.d', '-dpi', '192',
           '-logfile', str(out/'Xorg.log')]
with (out/'server.log').open('wb') as log:
    proc = subprocess.Popen(command, stdout=log, stderr=subprocess.STDOUT,
                            stdin=subprocess.DEVNULL, start_new_session=True)
state = dict(boot=boot, pid=proc.pid, display=':0', authority=str(auth), command=command,
             note='Qualified Xorg server uses its existing Zink backend. CTS clients must independently verify native Asahi.')
(out/'state.json').write_text(json.dumps(state, indent=2)+'\n')
probe = ['runuser', '-u', 'mcruz', '--', 'env', 'DISPLAY=:0', 'XAUTHORITY='+str(auth), 'xdpyinfo']
for _ in range(100):
    assert proc.poll() is None, 'Dedicated Xorg exited; inspect server.log'
    assert not fault.search(subprocess.check_output(['dmesg'])), 'GPU fault; fresh boot required'
    ready = subprocess.run(probe, stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=5)
    if ready.returncode == 0:
        (out/'xdpyinfo.txt').write_bytes(ready.stdout)
        print('DEDICATED_X11_READY pid='+str(proc.pid))
        break
    time.sleep(0.5)
else:
    raise RuntimeError('Dedicated Xorg did not become ready; inspect owned process before cleanup')
