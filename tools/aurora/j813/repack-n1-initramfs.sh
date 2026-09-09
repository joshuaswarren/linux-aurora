#!/bin/bash
# Refresh the experimental module in the existing private Wi-Fi RAM image.
set -euo pipefail
export ARCH=arm64 LOCALVERSION=
task_root=/Users/mcruz/Developer/aurora-silicon/j813-linux
task_ramfs="$HOME/j813-initramfs-wifi"
task_release=$(make -s -C "$HOME/j813-kernel-source" O="$HOME/j813-kernel-build" kernelrelease)
test -f "$task_ramfs/init"
test -d "$task_ramfs/lib/modules/$task_release/extra"
cp "$task_root/artifacts/wifi/apple_centauri.ko" \
    "$task_ramfs/lib/modules/$task_release/extra/apple_centauri.ko"
depmod -b "$task_ramfs" "$task_release"
umask 077
task_output="$task_root/artifacts/wifi/initramfs.cpio.gz"
task_temporary="$task_output.tmp"
trap 'rm -f "$task_temporary"' EXIT
(cd "$task_ramfs"; find . -print0 | cpio --null -o --format=newc --owner=0:0 2>/dev/null | gzip -9) > "$task_temporary"
mv "$task_temporary" "$task_output"
chmod 600 "$task_output"
(cd "$task_root/artifacts/wifi"
 sha256sum Image t8142-j813.dtb kernel.config initramfs.cpio.gz apple_centauri.ko > SHA256SUMS)
cat "$task_root/artifacts/wifi/SHA256SUMS"
