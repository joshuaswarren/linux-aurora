#!/bin/bash
set -euo pipefail
[[ $EUID == 0 && $(uname -r) == 7.1.6-aurora-neo-gpu1 ]]
task_boot=${1:?Expected boot required}
task_label=${2:?Fresh evidence label required}
[[ $task_boot =~ ^[0-9a-f-]{36}$ && $(cat /proc/sys/kernel/random/boot_id) == "$task_boot" ]]
[[ $task_label =~ ^gl30-cts-[a-z0-9-]+$ ]]
grep -zqx apple,j700 /proc/device-tree/compatible
[[ ! -e /run/neo-dcp-probe-attempted ]]
python3 - <<'PY'
from pathlib import Path
import hashlib,os,shutil
chosen=Path('/proc/device-tree/chosen')
for name in ['apple,dcp-asc-stopped','apple,dcp-rtkit-quiesced','apple,dcp-rtkit-idle']:
    p=chosen/name
    assert not p.exists() or p.read_bytes()==b'\0'*4, name
assert not Path('/sys/module/appledrm').exists()
candidate=Path('/run/neo-dcp-candidate/appledrm.ko')
assert hashlib.sha256(candidate.read_bytes()).hexdigest()=='c4e3644787f5c49345d145fe93c06439ddfce00f4b58d655049cdd49c1e205ef'
module=Path('/lib/modules/7.1.6-aurora-neo-gpu1/kernel/drivers/gpu/drm/apple/appledrm.ko')
backup=Path('/opt/neo/stopped-dcp-20260910/appledrm-before.ko')
assert hashlib.sha256(backup.read_bytes()).hexdigest()=='4599407f0ae02a9bb4d6ecd61f1af50dd131e22e1439e6d21a35409ec920f2c0'
assert module.read_bytes() in (backup.read_bytes(),candidate.read_bytes())
shutil.copy2(candidate,module)
with module.open('rb') as f:os.fsync(f.fileno())
assert module.read_bytes()==candidate.read_bytes()
print('LIVE_DCP_HANDOFF_AND_MODULE_VERIFIED')
PY
bash /run/neo-dcp-candidate/probe-dcp.sh "$task_label"
# Module insertion can succeed while the DCP component fails to bind.
# Require the actual display framebuffer before starting the CTS server.
python3 - <<'PY'
from pathlib import Path
names = [p.read_text().strip() for p in Path('/sys/class/graphics').glob('fb*/name')]
assert 'appledrmfb' in names, 'DCP did not bind; preserve this boot evidence before recovery'
print('DCP_DISPLAY_FRAMEBUFFER_VERIFIED')
PY
