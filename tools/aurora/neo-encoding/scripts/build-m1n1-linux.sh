#!/bin/bash
set -euo pipefail
[[ $(hostname) == lima-neo-builder ]] || { echo 'Run inside neo-builder' >&2; exit 1; }
task_shared=/Users/mcruz/Developer/aurora-silicon/neo-linux
task_src="$HOME/neo/sources/m1n1-linux-kis"
if [[ ! -d "$task_src" ]]; then
  mkdir -p "$task_src"
  tar -xzf "$task_shared/artifacts/m1n1-a2a1db46e9f3-source.tar.gz" -C "$task_src"
  git -C "$task_src" apply "$task_shared/artifacts/m1n1-kis-base.patch"
  git -C "$task_src" apply "$task_shared/artifacts/m1n1-linux-kis.patch"
  git -C "$task_src" apply "$task_shared/artifacts/m1n1-linux-gpu.patch"
  printf 'a2a1db46e9f3d2c1a8fb96fe23d484e73f27edae + kis-base + linux-kis\n' > "$task_src/.neo-source-commit"
fi
[[ -f "$task_src/.neo-source-commit" ]] || { echo 'Incomplete source setup; inspect it before retrying' >&2; exit 1; }
if git -C "$task_src" apply --check "$task_shared/artifacts/m1n1-linux-ave.patch"; then
  git -C "$task_src" apply "$task_shared/artifacts/m1n1-linux-ave.patch"
else
  git -C "$task_src" apply --reverse --check "$task_shared/artifacts/m1n1-linux-ave.patch"
fi
make -C "$task_src" -j12 USE_CLANG=1 TOOLCHAIN=/usr/bin/ LLDDIR=/usr/bin/ ARCH=aarch64-unknown-none \
  BUILDSTD=1 CHAINLOADING=1 T8140_KIS_PROXY=1
mkdir -p "$task_shared/artifacts/m1n1-linux"
for task_name in m1n1.bin m1n1.elf m1n1-raw.elf build_cfg.h; do
  cp "$task_src/build/$task_name" "$task_shared/artifacts/m1n1-linux/"
done
python3 - "$task_shared/artifacts/m1n1-linux" <<'PY'
import hashlib,json,sys
from pathlib import Path
p=Path(sys.argv[1])
if b'J700_LINUX_KIS_PRESERVED' not in (p/'m1n1-raw.elf').read_bytes():
    raise SystemExit('Linux KIS handoff guard missing from compiled image')
if b'J700_GPU_REGION' not in (p/'m1n1-raw.elf').read_bytes():
    raise SystemExit('T8140 GPU reservation fixup missing from compiled image')
if b'J700_AVE_FIRMWARE_RESERVED' not in (p/'m1n1-raw.elf').read_bytes():
    raise SystemExit('T8140 AVE firmware reservation fixup missing from compiled image')
out={'source_commit':'a2a1db46e9f3d2c1a8fb96fe23d484e73f27edae','files':{}}
for f in sorted(p.iterdir()):
    if f.is_file() and f.name!='manifest.json':
        out['files'][f.name]={'bytes':f.stat().st_size,'sha256':hashlib.sha256(f.read_bytes()).hexdigest()}
(p/'manifest.json').write_text(json.dumps(out,indent=2)+'\n')
PY
printf 'NEO_M1N1_LINUX_KIS_BUILD_COMPLETE\n'
