#!/bin/bash
# Build a separate diagnostic kernel; preserve the qualified v8 AVE payload.
set -euo pipefail
[[ $(hostname) == lima-j813-builder ]]
export ARCH=arm64 LLVM=1 LOCALVERSION=
export KBUILD_BUILD_USER=aurora KBUILD_BUILD_HOST=j813-builder
task_root=/Users/mcruz/Developer/aurora-silicon/j813-linux
task_src="$HOME/j813-desktop-kernel-source"
task_build="$HOME/j813-gpu-kernel-build"
task_module="$HOME/j813-gpu-ave-module"
task_out="$task_root/artifacts/gpu/native"
mkdir -p "$task_build" "$task_module" "$task_out"
if [[ ! -f "$task_build/.config" ]]; then
    cp "$task_root/artifacts/video/v4l2/kernel.config" "$task_build/.config"
fi
"$task_src/scripts/config" --file "$task_build/.config" \
    --set-str LOCALVERSION -aurora-j813-gpu-v9 \
    --disable MODVERSIONS --enable RUST --module DRM_ASAHI \
    --enable ALLOW_DEV_COREDUMP --enable APPLE_MAILBOX
make -C "$task_src" O="$task_build" olddefconfig
for task_option in CONFIG_RUST=y CONFIG_DRM_ASAHI=m CONFIG_DEV_COREDUMP=y CONFIG_ARM64_16K_PAGES=y \
    CONFIG_APPLE_MAILBOX=y CONFIG_VIDEO_DEV=m CONFIG_V4L2_MEM2MEM_DEV=m; do
    grep -qx "$task_option" "$task_build/.config"
done
make -C "$task_src" O="$task_build" rustavailable
make -C "$task_src" O="$task_build" -j8 Image modules
cp "$task_root/video/linux-ave/"apple-ave* "$task_root/video/linux-ave/Makefile" "$task_module/"
make -C "$task_src" O="$task_build" M="$task_module" -j8 W=1 modules
make -C "$task_src" O="$task_build" INSTALL_MOD_PATH="$task_out/modules" modules_install
task_release=$(cat "$task_build/include/config/kernel.release")
[[ "$task_release" == 7.1.6-aurora-j813-gpu-v9 ]]
cp "$task_build/arch/arm64/boot/Image" "$task_out/"
cp "$task_build/.config" "$task_out/kernel.config"
cp "$task_module/apple-ave.ko" "$task_out/"
echo M5_GPU_DIAGNOSTIC_KERNEL_AND_MODULES_BUILT
