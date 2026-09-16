#!/usr/bin/env python3
"""Preserve a finished test-server record so a healthy boot can test again."""
import json
import os
from pathlib import Path
import pwd
import re
import stat
import subprocess
import sys

boot, label = sys.argv[1:]
assert os.geteuid() == 0
assert re.fullmatch(r'[a-z0-9-]+', label)
assert Path('/proc/sys/kernel/random/boot_id').read_text().strip() == boot
assert b'apple,j700' in Path('/proc/device-tree/compatible').read_bytes().split(b'\0')
assert Path('/run/neo-gl30-cts-prepared').read_text().strip() == boot
fault = rb'asahi[^\n]*(?:halted GMMU|completion timed out|retiring slot)|translation fault|GPU FAULT'
assert not re.search(fault, subprocess.check_output(['dmesg']))
campaign = Path('/home/mcruz/gl30-cts-20260913')/boot
source = campaign/'x11-server'
dest = campaign/('x11-server-stopped-'+label)
assert not dest.exists()
assert (source/'kernel-after-stop.log').is_file()
state = json.loads((source/'state.json').read_text())
assert state['boot'] == boot
auth = Path(state['authority'])
assert auth == Path('/run/neo-gl30-cts.Xauthority')
for proc in Path('/proc').glob('[0-9]*'):
    try:
        command = (proc/'cmdline').read_bytes()
    except FileNotFoundError:
        continue
    assert str(source/'Xorg.log').encode() not in command
    assert str(auth).encode() not in command
if auth.exists():
    info = auth.lstat()
    assert stat.S_ISREG(info.st_mode) and stat.S_IMODE(info.st_mode) == 0o600
    assert info.st_uid == pwd.getpwnam('mcruz').pw_uid
    auth.unlink()
source.rename(dest)
print('STOPPED_X11_EVIDENCE_ARCHIVED')
