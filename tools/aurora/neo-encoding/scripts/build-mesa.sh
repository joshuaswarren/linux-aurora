#!/bin/bash
set -euo pipefail
[[ $(hostname) == lima-neo-builder ]] || { echo 'Run inside neo-builder' >&2; exit 1; }
task_shared=/Users/mcruz/Developer/aurora-silicon/neo-linux
task_src="$HOME/neo/sources/mesa"
task_build="$HOME/neo/build/mesa"
export CC=clang CXX=clang++
task_setup=()
[[ ! -f "$task_build/meson-private/coredata.dat" ]] || task_setup+=(--reconfigure)
meson setup "${task_setup[@]}" "$task_build" "$task_src" --buildtype=debugoptimized \
  --prefix=/opt/neo-mesa --libdir=lib \
  -Dvulkan-drivers=asahi -Dgallium-drivers=[] \
  -Dplatforms=x11,wayland -Dglx=disabled -Degl=disabled -Dgbm=disabled \
  -Dopengl=false -Dgles1=disabled -Dgles2=disabled \
  -Dllvm=enabled -Dgallium-rusticl=false -Dbuild-tests=true \
  -Dtools=asahi -Dvideo-codecs=all
ninja -C "$task_build" -j8
DESTDIR="$HOME/neo/build/mesa-install" meson install -C "$task_build" --no-rebuild
mkdir -p "$task_shared/artifacts/mesa-tests"
for task_binary in asahi-vulkan-g17p-enumerate asahi-submit-g17p-add3 \
  asahi-submit-g17p-render g17p-offscreen-triangle g17p-wsi-triangle; do
  cp "$task_build/src/asahi/vulkan/$task_binary" "$task_shared/artifacts/mesa-tests/"
done
printf 'NEO_MESA_BUILD_COMPLETE\n'
