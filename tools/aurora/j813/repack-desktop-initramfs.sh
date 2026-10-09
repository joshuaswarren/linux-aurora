#!/bin/bash
set -euo pipefail
task_root=/Users/mcruz/Developer/aurora-silicon/j813-linux
task_out="$task_root/artifacts/desktop"
task_modules="$task_out/modules/lib/modules"
task_release=$(cat "$HOME/j813-desktop-kernel-build/include/config/kernel.release")
[[ "$task_release" == *-aurora-j813-desktop-v[0-9]* ]]
umask 077
task_ramfs=$(mktemp -d "$HOME/j813-desktop-initramfs.XXXXXX")
trap 'rm -rf "$task_ramfs"' EXIT
(cd "$task_ramfs"; gzip -dc "$task_root/artifacts/bluetooth/initramfs.cpio.gz" | cpio -idmu --quiet)
rm -rf "$task_ramfs/lib/modules"
mkdir -p "$task_ramfs/lib/modules"
cp -a "$task_modules/$task_release" "$task_ramfs/lib/modules/"
depmod -b "$task_ramfs" "$task_release"
for task_old in "$task_ramfs"/opt/aurora-bluetooth/modules/*.ko; do
    task_name=$(basename "$task_old")
    task_new=$(find "$task_modules/$task_release" -name "$task_name" -type f -print -quit)
    [[ -n "$task_new" ]]
    cp "$task_new" "$task_old"
done
cp "$task_root/init" "$task_ramfs/init"
mkdir -p "$task_ramfs/opt/j813"
cp "$task_root/desktop-fbdev.conf" "$task_ramfs/opt/j813/"
cp "$task_root/start-desktop.sh" "$task_ramfs/bin/j813-desktop-start"
chmod 755 "$task_ramfs/bin/j813-desktop-start"
cp "$task_root/desktop-root-exec.sh" "$task_ramfs/bin/j813-desktop-root-exec"
chmod 755 "$task_ramfs/bin/j813-desktop-root-exec"
cp "$task_root/start-n1-desktop.sh" "$task_ramfs/bin/j813-n1-start"
chmod 755 "$task_ramfs/bin/j813-n1-start"
if grep -aq 'apple/tpmtfw-j813.bin' "$task_out/t8142-j813.dtb"; then
    task_touch_fw="$task_root/artifacts/private-firmware/tpmtfw-j813.bin"
    echo "acf0f4502d32408650b1eadb53fedf292cf2e918e25881d6a72583edaeedbada  $task_touch_fw" | sha256sum -c -
    mkdir -p "$task_ramfs/lib/firmware/apple"
    cp "$task_touch_fw" "$task_ramfs/lib/firmware/apple/tpmtfw-j813.bin"
    chmod 600 "$task_ramfs/lib/firmware/apple/tpmtfw-j813.bin"
    cp "$task_root/start-input.sh" "$task_ramfs/bin/j813-input-start"
    chmod 755 "$task_ramfs/bin/j813-input-start"
    cp "$task_root/input-event-count.py" "$task_ramfs/bin/j813-input-event-count.py"
fi
(cd "$task_ramfs"; find . -print0 | cpio --null -o --format=newc --owner=0:0 2>/dev/null | gzip -9) \
    >"$task_out/initramfs.cpio.gz"
chmod 600 "$task_out/initramfs.cpio.gz"
(cd "$task_out"; sha256sum Image t8142-j813.dtb kernel.config initramfs.cpio.gz > boot-SHA256SUMS)
echo J813_DESKTOP_INITRAMFS_COMPLETE
