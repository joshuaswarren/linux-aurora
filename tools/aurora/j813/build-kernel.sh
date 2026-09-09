#!/bin/bash
set -euo pipefail
export ARCH=arm64 LOCALVERSION=
export KBUILD_BUILD_USER=aurora KBUILD_BUILD_HOST=j813-builder
task_root=/Users/mcruz/Developer/aurora-silicon/j813-linux
task_build="$HOME/j813-kernel-build"
task_src="$HOME/j813-kernel-source"
task_output="$task_root/artifacts"
task_config="$task_root/j813.config"
task_dtb=t8142-j813.dtb
if [ "${1:-}" = wifi ]; then
    task_output="$task_root/artifacts/wifi"
    task_config="$task_build/wifi-all.config"
    task_dtb=t8142-j813-wifi.dtb
    mkdir -p "$task_build"
    cat "$task_root/j813.config" "$task_root/wifi.config" > "$task_config"
fi
mkdir -p "$task_src" "$task_build" "$task_output"
rsync -a --exclude=.git "$task_root/linux/" "$task_src/"
make -C "$task_src" O="$task_build" KCONFIG_ALLCONFIG="$task_config" allnoconfig
make -C "$task_src" O="$task_build" -j10 Image "apple/$task_dtb"
cp "$task_build/arch/arm64/boot/Image" "$task_output/Image"
cp "$task_build/arch/arm64/boot/dts/apple/$task_dtb" "$task_output/t8142-j813.dtb"
cp "$task_build/.config" "$task_output/kernel.config"
task_ramfs="$HOME/j813-initramfs-${1:-base}"
mkdir -p "$task_ramfs"/{bin,sbin,dev,proc,sys,tmp,etc,usr/bin,usr/sbin}
cp /bin/busybox "$task_ramfs/bin/"
while read -r applet; do
    [ "$applet" = busybox ] || ln -sf busybox "$task_ramfs/bin/$applet"
done < <("$task_ramfs/bin/busybox" --list)
cp "$task_root/init" "$task_ramfs/init"
chmod +x "$task_ramfs/init"
if [ "${1:-}" = wifi ]; then
    make -C "$task_src" O="$task_build" -j10 modules
    task_module="$task_build/drivers/net/wireless/apple/centauri"
    cp "$task_module/apple_centauri.ko" "$task_output/"
    task_release=$(make -s -C "$task_src" O="$task_build" kernelrelease)
    mkdir -p "$task_ramfs/lib/modules/$task_release/extra" "$task_ramfs/sys/kernel/debug"
    cp "$task_module/apple_centauri.ko" "$task_ramfs/lib/modules/$task_release/extra/"
    cp "$task_build/modules.order" "$task_build/modules.builtin" \
       "$task_build/modules.builtin.modinfo" "$task_ramfs/lib/modules/$task_release/"
    depmod -b "$task_ramfs" "$task_release"
    python3 "$task_root/stage-network-tools.py" "$task_ramfs"
    cc -O2 -Wall -Wextra -Werror -Wl,-z,max-page-size=65536 \
        "$task_root/n1-packet-probe.c" -o "$task_ramfs/bin/n1-packet-probe"
    cp "$task_ramfs/bin/n1-packet-probe" "$task_output/n1-packet-probe"
    mkdir -p "$task_ramfs/lib/firmware"
    cp /lib/firmware/regulatory.db /lib/firmware/regulatory.db.p7s "$task_ramfs/lib/firmware/"
    if [ -d "$task_output/firmware" ]; then
        cp -a "$task_output/firmware/." "$task_ramfs/lib/firmware/"
    fi
fi
(cd "$task_ramfs"; find . -print0 | cpio --null -o --format=newc --owner=0:0 2>/dev/null | gzip -9) > "$task_output/initramfs.cpio.gz"
(cd "$task_output"; sha256sum Image t8142-j813.dtb kernel.config initramfs.cpio.gz) > "$task_output/SHA256SUMS"
if [ "${1:-}" = wifi ]; then
    chmod 600 "$task_output/initramfs.cpio.gz"
    (cd "$task_output"; sha256sum apple_centauri.ko) >> "$task_output/SHA256SUMS"
fi
echo J813_KERNEL_BUILD_COMPLETE
