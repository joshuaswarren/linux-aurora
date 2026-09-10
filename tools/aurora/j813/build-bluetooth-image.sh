#!/bin/bash
set -euo pipefail
export ARCH=arm64 LOCALVERSION=
export KBUILD_BUILD_USER=aurora KBUILD_BUILD_HOST=j813-builder
task_root=/Users/mcruz/Developer/aurora-silicon/j813-linux
task_src="$HOME/j813-kernel-source"
task_build="$HOME/j813-bluetooth-kernel-build"
task_output="$task_root/artifacts/bluetooth"
make -C "$task_src" O="$task_build" -j10 Image
cp "$task_build/arch/arm64/boot/Image" "$task_output/Image"
cp "$task_build/.config" "$task_output/kernel.config"
task_wifi="$HOME/j813-bluetooth-wifi-module"
mkdir -p "$task_wifi"
rsync -a "$task_root/linux/drivers/net/wireless/apple/centauri/" "$task_wifi/"
make -C "$task_src" O="$task_build" M="$task_wifi" CONFIG_APPLE_CENTAURI=m W=1 -j10 modules
cp "$task_wifi/apple_centauri.ko" "$task_output/"
bash "$task_root/build-n1-hci.sh"
cc -O2 -Wall -Wextra -Werror -static -Wl,-z,max-page-size=65536 \
    "$task_root/n1-bt-mgmt-probe.c" -o "$task_output/transfer/n1-bt-mgmt-probe"
python3 "$task_root/stage-bluetooth.py"
cp "$task_output/transfer/n1-bt-mgmt-probe" "$task_output/ram-test/"
umask 077
task_ramfs=$(mktemp -d "$HOME/j813-bluetooth-ramfs.XXXXXX")
trap 'rm -rf "$task_ramfs"' EXIT
(cd "$task_ramfs"; gzip -dc "$task_root/artifacts/wifi/initramfs.cpio.gz" | cpio -idmu --quiet)
task_release=$(make -s -C "$task_src" O="$task_build" kernelrelease)
cp "$task_output/apple_centauri.ko" "$task_ramfs/lib/modules/$task_release/extra/"
mkdir -p "$task_ramfs/opt"
cp -a "$task_output/ram-test" "$task_ramfs/opt/aurora-bluetooth"
(cd "$task_ramfs"; find . -print0 | cpio --null -o --format=newc --owner=0:0 2>/dev/null | gzip -9) \
    > "$task_output/initramfs.cpio.gz"
chmod 600 "$task_output/initramfs.cpio.gz"
(cd "$task_output"; sha256sum Image kernel.config initramfs.cpio.gz apple_centauri.ko btcentauri.ko > image-SHA256SUMS)
echo J813_BLUETOOTH_IMAGE_BUILD_COMPLETE
