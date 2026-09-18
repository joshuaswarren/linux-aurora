#!/usr/bin/env python3
"""Stop only this campaign's X server, including after a GPU fault."""
import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import time

boot = sys.argv[1]
assert os.geteuid() == 0
assert Path('/proc/sys/kernel/random/boot_id').read_text().strip() == boot
assert b'apple,j700' in Path('/proc/device-tree/compatible').read_bytes().split(b'\0')
out = Path('/home/mcruz/gl30-cts-20260913')/boot/'x11-server'
state = json.loads((out/'state.json').read_text())
assert state['boot'] == boot
proc = Path('/proc', str(state['pid']))
if proc.exists():
    assert proc.joinpath('comm').read_text().strip() == 'Xorg'
    assert str(out/'Xorg.log').encode() in proc.joinpath('cmdline').read_bytes()
    os.kill(state['pid'], signal.SIGTERM)
    for _ in range(40):
        if not proc.exists() or ') Z ' in proc.joinpath('stat').read_text():
            break
        time.sleep(0.25)
    else:
        os.kill(state['pid'], signal.SIGKILL)
(out/'kernel-after-stop.log').write_bytes(subprocess.check_output(['dmesg']))
subprocess.run(['sync'], check=True)
print('OWNED_X11_STOPPED')
