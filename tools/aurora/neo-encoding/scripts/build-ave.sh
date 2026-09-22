#!/bin/bash
set -euo pipefail
[[ $(hostname) == lima-neo-builder ]] || { echo 'Run inside neo-builder' >&2; exit 1; }
export ARCH=arm64 LLVM=1
export KBUILD_BUILD_USER=aurora KBUILD_BUILD_HOST=neo-builder
task_shared=/Users/mcruz/Developer/aurora-silicon/neo-linux
task_src="$HOME/neo/sources/linux"
task_kernel="$HOME/neo/build/linux"
task_build="$HOME/neo/build/ave-module"
task_bundle_name="${NEO_AVE_BUNDLE_NAME:-linux-ave}"
[[ "$task_bundle_name" =~ ^[A-Za-z0-9][A-Za-z0-9._-]*$ ]] || { echo 'Invalid bundle name' >&2; exit 1; }
task_bundle="$task_shared/artifacts/$task_bundle_name"
mkdir -p "$task_build" "$task_bundle"
cp "$task_shared/video/linux-ave/apple-ave.c" "$task_shared/video/linux-ave/apple-ave-ipc.h" "$task_shared/video/linux-ave/apple-ave-pair.h" "$task_shared/video/linux-ave/Makefile" "$task_build/"
make -C "$task_src" O="$task_kernel" M="$task_build" -j12 W=1 modules
cp "$task_build/apple-ave.ko" "$task_bundle/"
cp "$task_build/apple-ave.ko" "$task_bundle/apple-ave-unstripped.ko"
llvm-strip --strip-debug "$task_bundle/apple-ave.ko"

printf '#include "t8140-j700.dts"\n#include "%s/video/linux-ave/t8140-ave.dtsi"\n' "$task_shared" > "$task_build/t8140-j700-ave.dts"
clang -E -P -nostdinc -undef -D__DTS__ -x assembler-with-cpp \
  -I "$task_src/scripts/dtc/include-prefixes" -I "$task_src/arch/arm64/boot/dts/apple" \
  "$task_build/t8140-j700-ave.dts" -o "$task_build/t8140-j700-ave.pp.dts"
"$task_kernel/scripts/dtc/dtc" -I dts -O dtb \
  -o "$task_bundle/t8140-j700.dtb" "$task_build/t8140-j700-ave.pp.dts"
python3 "$task_shared/video/linux-ave/check-dtb.py" \
  "$task_shared/artifacts/linux/t8140-j700.dtb" "$task_bundle/t8140-j700.dtb" \
  > "$task_bundle/dtb-validation.json"
mkdir -p "$task_bundle/source"
cp "$task_shared/video/linux-ave/apple-ave.c" "$task_shared/video/linux-ave/apple-ave-ipc.h" \
   "$task_shared/video/linux-ave/apple-ave-pair.h" \
   "$task_shared/video/linux-ave/Makefile" "$task_shared/video/linux-ave/t8140-ave.dtsi" \
   "$task_shared/scripts/build-ave.sh" "$task_bundle/source/"
python3 - "$task_shared" "$task_bundle" <<'PY'
from pathlib import Path
import hashlib,json,sys
root=Path(sys.argv[1]); original=root/'artifacts/linux'; bundle=Path(sys.argv[2])
base=json.loads((original/'manifest.json').read_text())
for name,item in base['files'].items():
    if name=='t8140-j700.dtb': continue
    src=original/name; dst=bundle/name
    if not dst.exists(): dst.symlink_to(Path('../linux')/name)
    assert dst.resolve()==src.resolve(), f'Unexpected candidate artifact: {name}'
    assert dst.stat().st_size==item['bytes'] and hashlib.sha256(dst.read_bytes()).hexdigest()==item['sha256']
files={}
for name in [*base['files'],'apple-ave.ko','apple-ave-unstripped.ko']:
    f=bundle/name; files[name]={'bytes':f.stat().st_size,'sha256':hashlib.sha256(f.read_bytes()).hexdigest()}
(bundle/'manifest.json').write_text(json.dumps({'kernel_release':base['kernel_release'],
    'base_manifest_sha256':hashlib.sha256((original/'manifest.json').read_bytes()).hexdigest(),
    'purpose':'Experimental native Linux AVE encoding; bounded 20/32 MiB IPC pool',
    'hardware_validated':False, 'files':files},indent=2)+'\n')
print('NEO_AVE_BUILD_COMPLETE')
PY
