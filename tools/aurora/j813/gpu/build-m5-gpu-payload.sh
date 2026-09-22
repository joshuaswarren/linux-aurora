#!/bin/bash
# Package the v9 GPU diagnostic candidate without replacing the v8 bundle.
set -euo pipefail
[[ $(hostname) == lima-j813-builder ]]
task_root=/Users/mcruz/Developer/aurora-silicon/j813-linux
task_build="$HOME/j813-gpu-kernel-build"
task_out="$task_root/artifacts/gpu/native"
task_release=7.1.6-aurora-j813-gpu-v9
[[ $(cat "$task_build/include/config/kernel.release") == "$task_release" ]]
[[ -f "$task_out/modules/lib/modules/$task_release/kernel/drivers/gpu/drm/asahi/asahi.ko" ]]

clang -E -P -nostdinc -undef -D__DTS__ -x assembler-with-cpp \
    -I "$task_root/linux/scripts/dtc/include-prefixes" \
    -I "$task_root/linux/arch/arm64/boot/dts/apple" \
    "$task_root/gpu/t8142-j813-gpu.dts" -o "$task_out/t8142-j813-gpu.pp.dts"
"$task_build/scripts/dtc/dtc" -I dts -O dtb \
    -o "$task_out/t8142-j813.dtb" "$task_out/t8142-j813-gpu.pp.dts"
python3 "$task_root/gpu/check-m5-gpu-dtb.py" \
    "$task_root/artifacts/video/v4l2/t8142-j813.dtb" "$task_out/t8142-j813.dtb"

umask 077
task_ramfs=$(mktemp -d "$HOME/j813-gpu-initramfs.XXXXXX")
trap 'rm -rf "$task_ramfs"' EXIT
(cd "$task_ramfs"; gzip -dc "$task_root/artifacts/video/v4l2/initramfs.cpio.gz" | cpio -idmu --quiet)
rm -rf "$task_ramfs/lib/modules"
cp -a "$task_out/modules/lib/modules" "$task_ramfs/lib/"
mkdir -p "$task_ramfs/lib/modules/$task_release/extra"
cp "$task_out/apple-ave.ko" "$task_ramfs/lib/modules/$task_release/extra/"
depmod -b "$task_ramfs" "$task_release"
for task_old in "$task_ramfs"/opt/aurora-bluetooth/modules/*.ko; do
    task_name=$(basename "$task_old")
    task_new=$(find "$task_out/modules/lib/modules/$task_release" -name "$task_name" -type f -print -quit)
    [[ -n "$task_new" ]]
    cp "$task_new" "$task_old"
done
# Keep hardware probing explicit: the diagnostic Asahi module is available
# to modprobe, but the initramfs must not start it during automatic cold boot.
mkdir -p "$task_ramfs/etc/modprobe.d"
printf 'blacklist asahi\n' >"$task_ramfs/etc/modprobe.d/m5-gpu-diagnostic.conf"
# Mode 12 uses the private, locally captured M5 graphics reference bundle.
# Other diagnostic modes remain available without a graphics capture.
task_graphics_bundle="$task_root/artifacts/gpu/m5-25g76-graphics.bin"
if [[ -f "$task_graphics_bundle" ]]; then
    install -Dm600 "$task_graphics_bundle" \
        "$task_ramfs/lib/firmware/asahi/m5-25g76-graphics.bin"
fi
# The mode14 application-ABI smoke test never publishes a GPU command.
cc -static -Os -s -Wall -Wextra -Werror -D__user= \
    -I "$task_root/linux/include/uapi/drm" \
    "$task_root/linux/tools/aurora/j813/m5-drm-smoke.c" \
    -o "$task_out/m5-drm-smoke"
install -Dm755 "$task_out/m5-drm-smoke" "$task_ramfs/usr/local/bin/m5-drm-smoke"
if [[ -x "$task_out/m5-drm-render" ]]; then
    install -Dm755 "$task_out/m5-drm-render" "$task_ramfs/usr/local/bin/m5-drm-render"
fi
(cd "$task_ramfs"; find . -print0 | cpio --null -o --format=newc --owner=0:0 2>/dev/null | gzip -9) \
    >"$task_out/initramfs.cpio.gz"
(cd "$task_out"; sha256sum Image t8142-j813.dtb kernel.config apple-ave.ko initramfs.cpio.gz > boot-SHA256SUMS)
echo M5_GPU_DIAGNOSTIC_RAM_CANDIDATE_BUILT
