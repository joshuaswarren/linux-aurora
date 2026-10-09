#!/bin/bash
set -euo pipefail
[[ $(hostname) == lima-neo-builder && $EUID == 0 ]]
task_workspace=/Users/mcruz/Developer/aurora-silicon
task_art=$task_workspace/neo-linux/artifacts/j700-gl30-cts-20260913
task_apps=/home/mcruz.guest/neo/build/appsuite-20260913
task_source=$task_apps/VK-GL-CTS-opengl-4.6.8.1
task_build=$task_apps/cts-gl30-release-build
[[ $(git -c safe.directory="$task_source" -C "$task_source" rev-parse HEAD) == 067e8832315e79817ede1c4863804e440f5d1c80 ]]
[[ -z $(git -c safe.directory="$task_source" -C "$task_source" status --porcelain) ]]
[[ ! -e $task_build ]]
mkdir "$task_build"
git -c safe.directory="$task_source" -C "$task_source" rev-parse HEAD > "$task_art/source-commit.txt"
git -c safe.directory="$task_source" -C "$task_source" status --porcelain > "$task_art/source-status-before.txt"
bash "$task_workspace/neo-linux/artifacts/j700-plasma-keyboard-install-20260913/with-plasma-root.sh" \
  env GIT_CONFIG_COUNT=1 GIT_CONFIG_KEY_0=safe.directory \
  GIT_CONFIG_VALUE_0=/build/VK-GL-CTS-opengl-4.6.8.1 \
  cmake -S /build/VK-GL-CTS-opengl-4.6.8.1 \
  -B /build/cts-gl30-release-build -G Ninja -DCMAKE_BUILD_TYPE=Release \
  -DDEQP_TARGET=surfaceless
bash "$task_workspace/neo-linux/artifacts/j700-plasma-keyboard-install-20260913/with-plasma-root.sh" \
  env GIT_CONFIG_COUNT=1 GIT_CONFIG_KEY_0=safe.directory \
  GIT_CONFIG_VALUE_0=/build/VK-GL-CTS-opengl-4.6.8.1 \
  cmake --build /build/cts-gl30-release-build \
  --target glcts cts-runner -j8
cp "$task_build/CMakeCache.txt" "$task_art/build-configuration.txt"
git -c safe.directory="$task_source" -C "$task_source" status --porcelain > "$task_art/source-status-after.txt"
echo OFFICIAL_GL_CTS_RELEASE_BUILD_COMPLETE
