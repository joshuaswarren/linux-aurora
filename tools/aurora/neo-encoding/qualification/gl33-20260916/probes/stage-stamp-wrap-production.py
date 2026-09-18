#!/usr/bin/env python3
"""Stage the production counter fix after the pinned hardware diagnostic."""
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess

BASE = Path('/home/mcruz/gl30-cts-20260913')
BOOT = 'a888fccf-9641-467e-84eb-5c9356f53d65'
OLD = 'fc3c7367555041557df6b593e5f3ed98bcbd309c09aaa5472cab9f9b32279d51'
NEW = '51541904ae00503eb3c44d6bf5356e206ce7085296d047d489452247977b1594'
MODULE = Path('/lib/modules/7.1.6-aurora-neo-gpu1/kernel/drivers/gpu/drm/asahi/asahi.ko')
SOURCE = BASE / 'asahi-stamp-wrap-production.ko'
digest = lambda p: hashlib.sha256(p.read_bytes()).hexdigest()
assert os.geteuid() == 0
assert Path('/proc/sys/kernel/random/boot_id').read_text().strip() == BOOT
assert b'apple,j700' in Path('/proc/device-tree/compatible').read_bytes().split(b'\0')
receipt = json.loads((BASE / 'event-wrap-qualification-20260916/receipt.json').read_text())
assert receipt['boot'] == BOOT and receipt['module_sha256'] == OLD
assert receipt['hardware_test_pass'] and not receipt['kernel_fault']
assert not receipt['kernel_log_overrun'] and not receipt['kernel_stream_errors']
for name in ['glcts', 'cts-runner', 'Xorg', 'plasmashell', 'render-event-wrap']:
    result = subprocess.run(['pgrep', '-x', name], capture_output=True, text=True)
    assert result.returncode in (0, 1)
    for pid in result.stdout.split():
        try: state = Path('/proc', pid, 'stat').read_text().rsplit(') ', 1)[1].split()[0]
        except FileNotFoundError: continue
        assert state == 'Z', (name, pid, state)
assert digest(MODULE) == OLD and digest(SOURCE) == NEW
assert b'G17P test event-wrap verified' not in SOURCE.read_bytes()
assert subprocess.check_output(['modinfo', '-F', 'vermagic', str(MODULE)]) == subprocess.check_output(['modinfo', '-F', 'vermagic', str(SOURCE)])
backup = BASE / ('asahi-backup-' + OLD + '.ko')
if not backup.exists(): shutil.copy2(MODULE, backup)
assert digest(backup) == OLD
temporary = MODULE.with_name('asahi.ko.stamp-wrap-production-staging')
with temporary.open('xb') as f:
    f.write(SOURCE.read_bytes()); f.flush(); os.fsync(f.fileno())
os.chmod(temporary, MODULE.stat().st_mode & 0o777)
os.replace(temporary, MODULE)
assert digest(MODULE) == NEW
unit = Path('/etc/systemd/system/neo-event-wrap-qualification.service')
assert not Path('/etc/systemd/system/multi-user.target.wants/neo-event-wrap-qualification.service').exists()
unit.unlink()
subprocess.run(['systemctl', 'daemon-reload'], check=True)
os.sync()
result = dict(staged=True, old_boot=BOOT, old_module=OLD, next_boot_module=NEW,
              loaded_module_changed=False, diagnostic_fixture_removed=True,
              backup=str(backup), production_commit='378b14ac9d89')
(BASE / 'stamp-wrap-production-staging.json').write_text(json.dumps(result, indent=2) + '\n')
print(json.dumps(result))
