#!/bin/bash
# Build DCP into a separate RAM payload; preserve the qualified GPU artifacts.
set -euo pipefail
[[ $(hostname) == lima-j813-builder ]]
export ARCH=arm64 LLVM=1 LOCALVERSION=
export KBUILD_BUILD_USER=aurora KBUILD_BUILD_HOST=j813-builder
task_root=/Users/mcruz/Developer/aurora-silicon/j813-linux
task_src="$HOME/j813-desktop-kernel-source"
task_build="$HOME/j813-gpu-kernel-build"
task_out="$task_root/artifacts/display"
mkdir -p "$task_out"
cp "$task_root"/linux/drivers/gpu/drm/apple/*.[ch] \
   "$task_src/drivers/gpu/drm/apple/"
"$task_src/scripts/config" --file "$task_build/.config" \
    --set-str LOCALVERSION -aurora-j813-dcp-v1 \
    --module DRM_APPLE --enable BACKLIGHT_CLASS_DEVICE \
    --enable DRM_APPLE_DEBUG --enable APPLE_MAILBOX
make -C "$task_src" O="$task_build" olddefconfig
for task_option in CONFIG_DRM_APPLE=m CONFIG_APPLE_RTKIT=y \
    CONFIG_ARM64_16K_PAGES=y CONFIG_APPLE_MAILBOX=y; do
    grep -qx "$task_option" "$task_build/.config"
done
make -C "$task_src" O="$task_build" -j8 Image modules
make -C "$task_src" O="$task_build" INSTALL_MOD_PATH="$task_out/modules" modules_install
[[ $(cat "$task_build/include/config/kernel.release") == 7.1.6-aurora-j813-dcp-v1 ]]
cp "$task_build/arch/arm64/boot/Image" "$task_out/"
cp "$task_build/.config" "$task_out/kernel.config"
echo M5_DCP_KERNEL_AND_MODULES_BUILT
