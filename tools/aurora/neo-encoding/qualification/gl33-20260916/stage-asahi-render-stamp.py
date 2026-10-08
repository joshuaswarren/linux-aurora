#!/usr/bin/env python3
"""Stage the completion-stamp candidate on the verified Neo for a clean boot."""
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys

assert os.geteuid() == 0
boot = sys.argv[1]
assert Path('/proc/sys/kernel/random/boot_id').read_text().strip() == boot
assert b'apple,j700' in Path('/proc/device-tree/compatible').read_bytes().split(b'\0')
for process in ('Xorg', 'glcts', 'cts-runner', 'plasmashell'):
    assert subprocess.run(['pgrep', '-x', process], stdout=subprocess.DEVNULL).returncode == 1, process

directory = Path(__file__).resolve().parent
candidate = directory / 'asahi-render-stamp.ko'
module = Path('/lib/modules/7.1.6-aurora-neo-gpu1/kernel/drivers/gpu/drm/asahi/asahi.ko')
old = 'aa5b21014671517849cfa12c7415cd5b39f565283c7140f3b3ebaa367e96fb4b'
new = '6e8621eb0c4147778d243870907cffccdee5b9b74711516c451fdc9c88937cbe'
digest = lambda path: hashlib.sha256(path.read_bytes()).hexdigest()
assert digest(candidate) == new
assert digest(module) == old
assert subprocess.check_output(['modinfo', '-F', 'vermagic', str(candidate)]) == \
       subprocess.check_output(['modinfo', '-F', 'vermagic', str(module)])
assert subprocess.check_output(['uname', '-r']).strip() == b'7.1.6-aurora-neo-gpu1'
backup = directory / ('asahi-backup-' + old + '.ko')
if not backup.exists():
    shutil.copy2(module, backup)
assert digest(backup) == old
temporary = module.with_name('asahi.ko.render-stamp-staging')
with temporary.open('xb') as output:
    output.write(candidate.read_bytes())
    output.flush()
    os.fsync(output.fileno())
os.chmod(temporary, module.stat().st_mode & 0o777)
os.replace(temporary, module)
assert digest(module) == new
os.sync()
print(json.dumps(dict(staged=True, boot=boot, next_boot_module_sha256=new,
                      prior_module_sha256=old, backup=str(backup),
                      loaded_module_changed=False)))
