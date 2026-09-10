#!/bin/bash
# Build a separate RAM candidate; never replace the qualified v7 payload.
set -euo pipefail
[[ $(hostname) == lima-j813-builder ]]
export ARCH=arm64 LOCALVERSION=
export KBUILD_BUILD_USER=aurora KBUILD_BUILD_HOST=j813-builder
unset LLVM LLVM_IAS
task_root=/Users/mcruz/Developer/aurora-silicon/j813-linux
task_src="$HOME/j813-desktop-kernel-source"
task_build="$HOME/j813-video-kernel-build"
task_module="$HOME/j813-v4l2-module"
task_out="$task_root/artifacts/video/v4l2"
mkdir -p "$task_build" "$task_module" "$task_out"
if [[ ! -f "$task_build/.config" ]]; then
    cp "$task_root/artifacts/desktop/kernel.config" "$task_build/.config"
fi
# The out-of-tree driver consumes these media modules. VIM2M selects their
# hidden Kconfig symbols; that virtual test module is never loaded on M5.
"$task_src/scripts/config" --file "$task_build/.config" \
    --set-str LOCALVERSION -aurora-j813-desktop-v8 \
    --module MEDIA_SUPPORT --disable MEDIA_SUPPORT_FILTER \
    --enable VIDEO_DEV --enable MEDIA_TEST_SUPPORT --enable V4L_TEST_DRIVERS \
    --module VIDEO_VIM2M
make -C "$task_src" O="$task_build" CC=gcc olddefconfig
for task_option in CONFIG_VIDEO_DEV=m CONFIG_V4L2_MEM2MEM_DEV=m CONFIG_VIDEOBUF2_VMALLOC=m; do
    grep -qx "$task_option" "$task_build/.config"
done
make -C "$task_src" O="$task_build" CC=gcc -j8 Image modules
cp "$task_root/video/linux-ave/"apple-ave* "$task_root/video/linux-ave/Makefile" "$task_module/"
make -C "$task_src" O="$task_build" M="$task_module" CC=gcc -j8 W=1 modules
make -C "$task_src" O="$task_build" INSTALL_MOD_PATH="$task_out/modules" modules_install
task_release=$(cat "$task_build/include/config/kernel.release")
[[ "$task_release" == 7.1.6-aurora-j813-desktop-v8 ]]
cp "$task_build/arch/arm64/boot/Image" "$task_out/"
cp "$task_build/.config" "$task_out/kernel.config"
cp "$task_root/artifacts/video/native/t8142-j813.dtb" "$task_out/"
cp "$task_module/apple-ave.ko" "$task_out/"
umask 077
task_ramfs=$(mktemp -d "$HOME/j813-v4l2-initramfs.XXXXXX")
trap 'rm -rf "$task_ramfs"' EXIT
(cd "$task_ramfs"; gzip -dc "$task_root/artifacts/video/native/initramfs.cpio.gz" | cpio -idmu --quiet)
rm -rf "$task_ramfs/lib/modules"
cp -a "$task_out/modules/lib/modules" "$task_ramfs/lib/"
mkdir -p "$task_ramfs/lib/modules/$task_release/extra"
cp "$task_module/apple-ave.ko" "$task_ramfs/lib/modules/$task_release/extra/"
gcc -O2 -Wall -Wextra -Werror -static "$task_root/video/probe-v4l2.c" \
    -o "$task_ramfs/bin/m5-v4l2-probe"
depmod -b "$task_ramfs" "$task_release"
for task_old in "$task_ramfs"/opt/aurora-bluetooth/modules/*.ko; do
    task_name=$(basename "$task_old")
    task_new=$(find "$task_out/modules/lib/modules/$task_release" -name "$task_name" -type f -print -quit)
    [[ -n "$task_new" ]]
    cp "$task_new" "$task_old"
done
cp "$task_root/start-desktop.sh" "$task_ramfs/bin/j813-desktop-start"
cp "$task_root/start-input.sh" "$task_ramfs/bin/j813-input-start"
cp "$task_root/start-n1-desktop.sh" "$task_ramfs/bin/j813-n1-start"
chmod 755 "$task_ramfs/bin/"j813-*-start
(cd "$task_ramfs"; find . -print0 | cpio --null -o --format=newc --owner=0:0 2>/dev/null | gzip -9) \
    >"$task_out/initramfs.cpio.gz"
(cd "$task_out"; sha256sum Image t8142-j813.dtb kernel.config apple-ave.ko initramfs.cpio.gz > boot-SHA256SUMS)
echo M5_V4L2_RAM_CANDIDATE_BUILT
