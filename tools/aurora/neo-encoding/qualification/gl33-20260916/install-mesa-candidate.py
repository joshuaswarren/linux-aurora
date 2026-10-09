#!/usr/bin/env python3
"""Replace only the idle native CTS library, preserving a verified backup."""
import hashlib
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
from cts_faults import FATAL_GPU_FAULT

boot, old_hash, new_hash, filename = sys.argv[1:]
assert os.geteuid() == 0
assert Path('/proc/sys/kernel/random/boot_id').read_text().strip() == boot
assert b'apple,j700' in Path('/proc/device-tree/compatible').read_bytes().split(b'\0')
assert subprocess.run(['systemctl', 'is-active', '--quiet', 'sddm']).returncode != 0
assert re.fullmatch(r'libgallium-[a-z0-9-]+\.so', filename)
assert all(re.fullmatch(r'[0-9a-f]{64}', h) for h in (old_hash, new_hash))
assert not FATAL_GPU_FAULT.search(subprocess.check_output(['dmesg']))
prefix = '/opt/neo-mesa-native-gl2-20260913/'
for maps in Path('/proc').glob('[0-9]*/maps'):
    try:
        assert prefix not in maps.read_text(), 'Native test runtime still mapped by '+maps.parent.name
    except FileNotFoundError:
        continue
campaign = Path('/home/mcruz/gl30-cts-20260913')
source = campaign/filename
dest = Path(prefix)/'lib/libgallium-26.1.4.so'
digest = lambda path: hashlib.sha256(path.read_bytes()).hexdigest()
assert digest(source) == new_hash and digest(dest) == old_hash
backup = campaign/('before-'+old_hash+'-'+filename)
# A candidate can be installed more than once during an A/B/A experiment.
# Preserve each distinct predecessor instead of colliding on the target name.
if not backup.exists():
    shutil.copy2(dest, backup)
assert digest(backup) == old_hash
temp = dest.with_name(dest.name+'.staged')
assert not temp.exists()
shutil.copy2(source, temp)
os.chmod(temp, 0o755)
with temp.open('rb') as data:
    os.fsync(data.fileno())
assert digest(temp) == new_hash
os.replace(temp, dest)
fd = os.open(dest.parent, os.O_RDONLY | os.O_DIRECTORY)
os.fsync(fd)
os.close(fd)
assert digest(dest) == new_hash
print('NATIVE_CTS_LIBRARY_INSTALLED sha256='+new_hash)
