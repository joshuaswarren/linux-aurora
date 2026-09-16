#!/bin/bash
set -euo pipefail
[[ $(hostname) == lima-neo-builder && $EUID == 0 ]]
task_art=/Users/mcruz/Developer/aurora-silicon/neo-linux/artifacts/j700-gl30-cts-20260913/x11
task_apps=/home/mcruz.guest/neo/build/appsuite-20260913
task_source=$task_apps/VK-GL-CTS-opengl-4.6.8.1
task_build=$task_apps/cts-gl30-x11-release-build
task_stage=$task_apps/cts-gl30-x11-release-package/opt/neo-cts-opengl-4.6.8.1-x11
[[ ! -e $task_stage ]]
[[ $(git -c safe.directory="$task_source" -C "$task_source" rev-parse HEAD) == 067e8832315e79817ede1c4863804e440f5d1c80 ]]
[[ -z $(git -c safe.directory="$task_source" -C "$task_source" status --porcelain) ]]
for task_binary in glcts cts-runner; do
  [[ -x $task_build/external/openglcts/modules/$task_binary ]]
  readelf -h "$task_build/external/openglcts/modules/$task_binary" | grep -q 'Machine:.*AArch64'
done
mkdir -p "$task_stage"
rsync -aL --exclude=CMakeFiles --exclude='*.a' --exclude='*.o' \
  --exclude='*.cmake' --exclude=Makefile \
  "$task_build/external/openglcts/modules/" "$task_stage/"
for task_data in gl_cts gles2 gles3 gles31; do
  [[ -d $task_stage/$task_data ]]
done
cp "$task_source/LICENSE" "$task_stage/LICENSE"
cp "$task_art/source-commit.txt" "$task_stage/source-commit.txt"
cp "$task_build/CMakeCache.txt" "$task_stage/build-configuration.txt"
cp "$task_source/external/openglcts/README.md" "$task_stage/CTS-README.md"
python3 - "$task_stage" <<'PY'
import hashlib,json,sys
from pathlib import Path
p=Path(sys.argv[1])
manifest={str(f.relative_to(p)):{'size':f.stat().st_size,
          'sha256':hashlib.sha256(f.read_bytes()).hexdigest()}
          for f in sorted(p.rglob('*')) if f.is_file()}
(p/'package-manifest.json').write_text(json.dumps(manifest,indent=2)+'\n')
print('PACKAGE_FILES='+str(len(manifest)))
PY
cp "$task_stage/package-manifest.json" "$task_art/package-manifest.json"
tar -czf "$task_art/cts-opengl-4.6.8.1-x11.tar.gz" -C "$task_apps/cts-gl30-x11-release-package" opt
sha256sum "$task_art/cts-opengl-4.6.8.1-x11.tar.gz" > "$task_art/package-sha256.txt"
echo OFFICIAL_GL_CTS_X11_RELEASE_PACKAGED
