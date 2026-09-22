#!/bin/bash
# RAM-only Linux boot for the verified M3 Max hpm0 -> Neo KIS topology.
set -euo pipefail
task_linux=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
source "$task_linux/../neo-update/env.sh"
export M1N1_DIR="$NEO_WORK/m1n1-kis"
export PYTHONPATH="$M1N1_DIR/proxyclient"
export M1N1DEVICE=/dev/cu.kis-00100000-ch-0
export M1N1TIMEOUT=30
task_bundle=${J700_LINUX_BUNDLE:-"$task_linux/artifacts/linux"}

python "$task_linux/boot-linux.py" --linux-bundle "$task_bundle" --keep-boot-cpu --direct --preflight-only
if lsof -t "$M1N1DEVICE" >/dev/null 2>&1; then
    echo 'Neo KIS is in use. Close its current terminal/controller before booting.' >&2
    exit 1
fi
task_run="$task_linux/logs/$(date -u +%Y%m%dT%H%M%SZ)-native-linux"
mkdir -p "$task_run"
"$MACVDMCTL" nop > "$task_run/controller.log" 2>&1
if ! rg -q 'Mac type: J516cAP' "$task_run/controller.log" || ! rg -q '/hpm0@' "$task_run/controller.log"; then
    echo 'Controller differs from the verified M3 Max hpm0 setup; rediscover topology.' >&2
    exit 1
fi
printf '%s\n' "$task_run" > "$task_linux/logs/latest-native-run.txt"
ln -sfn "$task_run/console.log" "$task_linux/logs/native-live.log"
printf 'Linux boot logs: %s\n' "$task_run"
"$MACVDMCTL" reboot debugusb > "$task_run/reboot.log" 2>&1
sleep 8
task_placement=(--j700-safe-base)
if [[ -n "${J700_STAGE2_BASE:-}" ]]; then
    task_placement=(--j700-stage2-base "$J700_STAGE2_BASE")
fi
python -u "$NEO_WORK/kis-chainload.py" -r "${task_placement[@]}" \
    "$task_linux/artifacts/m1n1-linux/m1n1.bin" > "$task_run/chainload.log" 2>&1
task_base=$(python - "$task_run/chainload.log" <<'PY'
from pathlib import Path
import re,sys
text=Path(sys.argv[1]).read_text()
bases=re.findall(r'^J700 safe Stage-2 span: (0x[0-9a-f]+)\.\.',text,re.M)
if len(bases)!=1 or 'Proxy is alive again' not in text:
    raise SystemExit('No unique successful guarded Stage 2 placement')
print(bases[0])
PY
)
python -u "$task_linux/boot-linux.py" --linux-bundle "$task_bundle" --stage2-base "$task_base" --keep-boot-cpu --direct \
    > "$task_run/console.log" 2>&1
python -u "$task_linux/console.py" --timeout 60 \
    'echo 4 > /proc/sys/kernel/printk; modprobe asahi && test -c /dev/dri/renderD128 && /opt/neo/bin/g17p-vulkan-compute' \
    > "$task_run/vulkan-first.log" 2>&1
printf 'NEO_LINUX_AND_VULKAN_READY\n'
