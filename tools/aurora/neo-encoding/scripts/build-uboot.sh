#!/bin/bash
set -euo pipefail
[[ $(hostname) == lima-neo-builder ]] || { echo 'Run inside neo-builder' >&2; exit 1; }
task_shared=/Users/mcruz/Developer/aurora-silicon/neo-linux
task_src="$HOME/neo/sources/u-boot"
task_build="$HOME/neo/build/u-boot"
mkdir -p "$task_build" "$task_shared/artifacts/u-boot"
make -C "$task_src" O="$task_build" apple_m1_defconfig
"$task_src/scripts/config" --file "$task_build/.config" -d ENV_IS_IN_FAT -e ENV_IS_NOWHERE
make -C "$task_src" O="$task_build" olddefconfig
grep -qx 'CONFIG_ENV_IS_NOWHERE=y' "$task_build/.config"
grep -qx '# CONFIG_ENV_IS_IN_FAT is not set' "$task_build/.config"
make -C "$task_src" O="$task_build" -j12
cp "$task_build/u-boot-nodtb.bin" "$task_shared/artifacts/u-boot/"
cp "$task_build/u-boot" "$task_shared/artifacts/u-boot/u-boot.elf"
cp "$task_build/.config" "$task_shared/artifacts/u-boot/u-boot.config"
python3 - "$task_shared/artifacts/u-boot" <<'PY'
import hashlib,json,sys
from pathlib import Path
p=Path(sys.argv[1])
out={'source_commit':'b0f6d36e1c3ff4b4eceec663ded023830c237a30','profile':'RAM environment','files':{}}
for f in sorted(p.iterdir()):
    if f.is_file() and f.name!='manifest.json':
        out['files'][f.name]={'bytes':f.stat().st_size,'sha256':hashlib.sha256(f.read_bytes()).hexdigest()}
(p/'manifest.json').write_text(json.dumps(out,indent=2)+'\n')
PY
printf 'NEO_UBOOT_BUILD_COMPLETE\n'
