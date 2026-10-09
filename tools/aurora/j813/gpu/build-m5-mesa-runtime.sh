#!/bin/bash
# Build M5 userspace in its own source/build directories inside neo-builder.
# No Neo source, build or hardware is changed by this script.
set -euo pipefail
[[ $(hostname) == lima-neo-builder ]]
task_src=/home/mcruz.guest/j813-mesa-source
task_build=/home/mcruz.guest/j813-mesa-runtime
export CC=clang CXX=clang++
if [[ ! -f "$task_build/build.ninja" ]]; then
    meson setup "$task_build" "$task_src" --buildtype=debugoptimized \
        -Dgallium-drivers=asahi,zink -Dvulkan-drivers=asahi \
        -Dplatforms=x11,wayland -Dglx=dri -Degl=enabled -Dgbm=enabled \
        -Dvideo-codecs= -Dllvm=enabled -Dmesa-clc=enabled \
        -Dprecomp-compiler=enabled -Dbuild-tests=true
fi
ninja -C "$task_build" -j8 \
    src/asahi/vulkan/libvulkan_asahi.so \
    src/gallium/targets/dri/libgallium-26.1.4.so \
    src/gallium/targets/dril/libdril_dri.so \
    src/glx/libGLX_mesa.so.0.0.0 src/egl/libEGL_mesa.so.0.0.0 \
    src/gbm/libgbm.so.1.0.0 src/gbm/backends/dri/dri_gbm.so \
    src/asahi/lib/asahi-agx-m5-va src/asahi/lib/asahi-agx-va-runtime-hole
meson test -C "$task_build" --no-rebuild --print-errorlogs \
    asahi-agx-m5-va asahi-agx-va-runtime-hole
printf 'M5_MESA_RUNTIME_BUILD_VERIFIED\n'
