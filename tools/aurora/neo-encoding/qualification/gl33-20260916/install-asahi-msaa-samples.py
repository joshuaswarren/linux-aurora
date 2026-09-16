#!/usr/bin/env python3
"""Stage the verified module for the next clean boot, retaining the prior binary."""
import hashlib
import os
from pathlib import Path
import shutil
import subprocess
import sys

assert os.geteuid() == 0
assert Path('/proc/sys/kernel/random/boot_id').read_text().strip() == sys.argv[1]
assert b'apple,j700' in Path('/proc/device-tree/compatible').read_bytes().split(b'\0')
assert subprocess.run(['pgrep', '-x', 'Xorg'], stdout=subprocess.DEVNULL).returncode == 1
assert subprocess.run(['pgrep', '-x', 'glcts'], stdout=subprocess.DEVNULL).returncode == 1
digest = lambda p: hashlib.sha256(p.read_bytes()).hexdigest()
directory = Path(__file__).resolve().parent
candidate = directory / 'asahi-msaa-samples.ko'
module = Path('/lib/modules/7.1.6-aurora-neo-gpu1/kernel/drivers/gpu/drm/asahi/asahi.ko')
backup = directory / 'asahi-before-msaa-samples.ko'
old = '63e78d2575cf4e9c181808e7c93799a201fcb5c63d31cfe80b8f2779c2b3c8c3'
new = 'aa5b21014671517849cfa12c7415cd5b39f565283c7140f3b3ebaa367e96fb4b'
assert digest(candidate) == new
assert digest(module) == old
assert not backup.exists()
shutil.copy2(module, backup)
assert digest(backup) == old
temporary = module.with_name('asahi.ko.msaa-staging')
with temporary.open('xb') as output:
    output.write(candidate.read_bytes())
    output.flush()
    os.fsync(output.fileno())
os.chmod(temporary, module.stat().st_mode & 0o777)
os.replace(temporary, module)
assert digest(module) == new
os.sync()
print('ASAHI_MSAA_STAGED_FOR_NEXT_BOOT', new, 'prior binary retained')
