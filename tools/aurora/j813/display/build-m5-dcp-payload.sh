#!/bin/bash
# Package an explicit DCP probe with the qualified Wi-Fi and input RAM root.
set -euo pipefail
[[ $(hostname) == lima-j813-builder ]]
task_root=/Users/mcruz/Developer/aurora-silicon/j813-linux
task_build="$HOME/j813-gpu-kernel-build"
task_out="$task_root/artifacts/display"
task_release=7.1.6-aurora-j813-dcp-v1
[[ $(cat "$task_build/include/config/kernel.release") == "$task_release" ]]
[[ -f "$task_out/modules/lib/modules/$task_release/kernel/drivers/gpu/drm/apple/appledrm.ko" ]]
clang -E -P -nostdinc -undef -D__DTS__ -x assembler-with-cpp \
    -I "$task_root/linux/scripts/dtc/include-prefixes" \
    -I "$task_root/linux/arch/arm64/boot/dts/apple" \
    "$task_root/linux/arch/arm64/boot/dts/apple/t8142-j813-display.dts" \
    -o "$task_out/t8142-j813-display.pp.dts"
"$task_build/scripts/dtc/dtc" -I dts -O dtb \
    -o "$task_out/t8142-j813.dtb" "$task_out/t8142-j813-display.pp.dts"
umask 077
task_ramfs=$(mktemp -d "$HOME/j813-dcp-initramfs.XXXXXX")
trap 'rm -rf "$task_ramfs"' EXIT
(cd "$task_ramfs"; gzip -dc "$task_root/artifacts/desktop/initramfs.cpio.gz" | cpio -idmu --quiet)
rm -rf "$task_ramfs/lib/modules"
cp -a "$task_out/modules/lib/modules" "$task_ramfs/lib/"
depmod -b "$task_ramfs" "$task_release"
for task_old in "$task_ramfs"/opt/aurora-bluetooth/modules/*.ko; do
    task_name=$(basename "$task_old")
    task_new=$(find "$task_out/modules/lib/modules/$task_release" -name "$task_name" -type f -print -quit)
    [[ -n "$task_new" ]]
    cp "$task_new" "$task_old"
done
mkdir -p "$task_ramfs/etc/modprobe.d"
printf 'blacklist appledrm\nblacklist asahi\n' >"$task_ramfs/etc/modprobe.d/m5-display-probe.conf"
mkdir -p "$task_ramfs/opt/j813"
install -m644 "$task_root/desktop-dcp.conf" "$task_ramfs/opt/j813/desktop-dcp.conf"
install -m755 "$task_root/start-desktop.sh" "$task_ramfs/bin/j813-desktop-start"
(cd "$task_ramfs"; find . -print0 | cpio --null -o --format=newc --owner=0:0 2>/dev/null | gzip -9) \
    >"$task_out/initramfs.cpio.gz"
(cd "$task_out"; sha256sum Image t8142-j813.dtb kernel.config initramfs.cpio.gz > boot-SHA256SUMS)
echo M5_DCP_RAM_CANDIDATE_BUILT
