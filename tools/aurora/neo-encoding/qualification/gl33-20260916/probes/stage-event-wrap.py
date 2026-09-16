#!/usr/bin/env python3
"""Pin, back up and stage the one-boot J700 completion-wrap diagnostic."""
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess

BASE = Path('/home/mcruz/gl30-cts-20260913')
BOOT = '9f1434bf-725b-4ff1-ae51-babd4df25cb4'
OLD = '03abec7b984762e6b32a27951652d4c0445e8300e7447a795e5c45dc5291d762'
NEW = 'fc3c7367555041557df6b593e5f3ed98bcbd309c09aaa5472cab9f9b32279d51'
MODULE = Path('/lib/modules/7.1.6-aurora-neo-gpu1/kernel/drivers/gpu/drm/asahi/asahi.ko')
SOURCE = BASE / 'asahi-event-wrap-diagnostic.ko'
OUT = BASE / 'event-wrap-qualification-20260916'
UNIT = Path('/etc/systemd/system/neo-event-wrap-qualification.service')
digest = lambda p: hashlib.sha256(p.read_bytes()).hexdigest()
assert os.geteuid() == 0
assert Path('/proc/sys/kernel/random/boot_id').read_text().strip() == BOOT
assert b'apple,j700' in Path('/proc/device-tree/compatible').read_bytes().split(b'\0')
blocked_owned_xorg = []
for name in ['glcts', 'cts-runner', 'Xorg', 'plasmashell', 'render-event-wrap']:
    result = subprocess.run(['pgrep', '-x', name], capture_output=True, text=True)
    assert result.returncode in (0, 1)
    for pid in result.stdout.split():
        try: state = Path('/proc', pid, 'stat').read_text().rsplit(') ', 1)[1].split()[0]
        except FileNotFoundError: continue
        if name == 'Xorg' and state == 'D':
            # The failed GPU session can leave its owned X server blocked in
            # release after SIGKILL. Only stage disk bytes for the next boot;
            # never unload/reload the live module under this process.
            owned = BASE / BOOT / 'x11-server'
            server = json.loads((owned / 'state.json').read_text())
            assert server['boot'] == BOOT and int(pid) == server['pid']
            assert str(owned / 'Xorg.log').encode() in Path('/proc', pid, 'cmdline').read_bytes()
            status = dict(line.split(':', 1) for line in Path('/proc', pid, 'status').read_text().splitlines() if ':' in line)
            assert int(status['ShdPnd'].strip(), 16) & (1 << 8), 'SIGKILL must already be pending'
            blocked_owned_xorg.append(int(pid))
        else:
            assert state == 'Z', (name, pid, state)
assert digest(MODULE) == OLD and digest(SOURCE) == NEW
assert digest(BASE / 'probes/render-event-wrap') == '95429a54dea74d21d842ff5cb12a6df40091759e710af0bc924a271697c10b99'
assert subprocess.check_output(['modinfo', '-F', 'vermagic', str(MODULE)]) == subprocess.check_output(['modinfo', '-F', 'vermagic', str(SOURCE)])
assert not OUT.exists() and not UNIT.exists()
subprocess.run(['systemd-analyze', 'verify', str(BASE / 'probes/neo-event-wrap-qualification.service')], check=True)
backup = BASE / ('asahi-backup-' + OLD + '.ko')
if not backup.exists(): shutil.copy2(MODULE, backup)
assert digest(backup) == OLD
OUT.mkdir(mode=0o755)
temporary = MODULE.with_name('asahi.ko.event-wrap-staging')
with temporary.open('xb') as f:
    f.write(SOURCE.read_bytes()); f.flush(); os.fsync(f.fileno())
os.chmod(temporary, MODULE.stat().st_mode & 0o777)
os.replace(temporary, MODULE)
assert digest(MODULE) == NEW
shutil.copy2(BASE / 'probes/neo-event-wrap-qualification.service', UNIT)
os.chown(UNIT, 0, 0); os.chmod(UNIT, 0o644)
(OUT / 'pending-boot').write_text(BOOT + '\n')
subprocess.run(['systemctl', 'daemon-reload'], check=True)
subprocess.run(['systemctl', 'enable', 'neo-event-wrap-qualification.service'], check=True)
os.sync()
receipt = dict(staged=True, old_boot=BOOT, old_module=OLD, next_boot_module=NEW,
               loaded_module_changed=False, backup=str(backup), cts_conformance=False,
               blocked_owned_xorg=blocked_owned_xorg)
(OUT / 'staging.json').write_text(json.dumps(receipt, indent=2) + '\n')
print(json.dumps(receipt))
