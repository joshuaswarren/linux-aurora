#!/bin/bash
set -euo pipefail
[[ $(hostname) == lima-neo-builder ]] || exit 1
export ARCH=arm64 LLVM=1 KBUILD_BUILD_USER=aurora KBUILD_BUILD_HOST=neo-builder
task_shared=/Users/mcruz/Developer/aurora-silicon/neo-linux
task_src="$HOME/neo/sources/linux"
task_kernel="$HOME/neo/build/linux"
task_build="$HOME/neo/build/prores-module"
task_bundle="$task_shared/artifacts/linux-prores"
mkdir -p "$task_build" "$task_bundle"
cp "$task_shared/video/linux-prores/apple-prores-test.c" "$task_shared/video/linux-prores/Makefile" "$task_build/"
make -C "$task_src" O="$task_kernel" M="$task_build" -j12 W=1 modules
cp "$task_build/apple-prores-test.ko" "$task_bundle/apple-prores-test-unstripped.ko"
cp "$task_build/apple-prores-test.ko" "$task_bundle/"
llvm-strip --strip-debug "$task_bundle/apple-prores-test.ko"
printf '#include "t8140-j700.dts"\n#include "%s/video/linux-ave/t8140-ave.dtsi"\n#include "%s/video/linux-prores/t8140-prores.dtsi"\n' "$task_shared" "$task_shared" > "$task_build/t8140-j700-prores.dts"
clang -E -P -nostdinc -undef -D__DTS__ -x assembler-with-cpp \
  -I "$task_src/scripts/dtc/include-prefixes" -I "$task_src/arch/arm64/boot/dts/apple" \
  "$task_build/t8140-j700-prores.dts" -o "$task_build/t8140-j700-prores.pp.dts"
"$task_kernel/scripts/dtc/dtc" -I dts -O dtb -o "$task_bundle/t8140-j700.dtb" "$task_build/t8140-j700-prores.pp.dts"
python3 "$task_shared/video/linux-prores/check-dtb.py" \
  "$task_shared/artifacts/linux-ave/t8140-j700.dtb" "$task_bundle/t8140-j700.dtb" > "$task_shared/logs/prores-dtb-validation.json"
python3 - "$task_shared" <<'PY'
from pathlib import Path
import hashlib,json,sys
root=Path(sys.argv[1]); original=root/'artifacts/linux'; bundle=root/'artifacts/linux-prores'
base=json.loads((original/'manifest.json').read_text())
for name,item in base['files'].items():
    if name=='t8140-j700.dtb': continue
    src=original/name; dst=bundle/name
    if not dst.exists(): dst.symlink_to(Path('../linux')/name)
    assert dst.resolve()==src.resolve()
    assert dst.stat().st_size==item['bytes'] and hashlib.sha256(dst.read_bytes()).hexdigest()==item['sha256']
files={}
for name in [*base['files'],'apple-prores-test.ko','apple-prores-test-unstripped.ko']:
    f=bundle/name; files[name]={'bytes':f.stat().st_size,'sha256':hashlib.sha256(f.read_bytes()).hexdigest()}
(bundle/'manifest.json').write_text(json.dumps({'kernel_release':base['kernel_release'],
    'base_manifest_sha256':hashlib.sha256((original/'manifest.json').read_bytes()).hexdigest(),
    'purpose':'Bounded native Linux ProRes experiment; preserves AVE DT devices', 'files':files},indent=2)+'\n')
print('NEO_PRORES_BUILD_COMPLETE')
PY
