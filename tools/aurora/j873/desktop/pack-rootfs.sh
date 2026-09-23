#!/bin/sh
set -eu
test "$(hostname)" = lima-j813-builder
task_src="$HOME/j873-linux-source"
task_build="$HOME/j873-desktop-build"
task_root="$HOME/j873-desktop-root"
task_out="$HOME/j873-desktop-artifacts"
task_tools="$task_src/tools/aurora/j873"
test -x "$task_root/usr/bin/xfce4-session"
test -f "$task_out/Image"
sudo make -C "$task_src" O="$task_build" ARCH=arm64 LLVM=1 \
    INSTALL_MOD_PATH="$task_root" modules_install
sudo install -d "$task_root/opt/j873" "$task_root/etc/X11"
sudo install -m755 "$task_tools/desktop/init" "$task_root/init"
for script in start-session.sh xfce-session.sh status-window.sh; do
    sudo install -m755 "$task_tools/desktop/$script" "$task_root/opt/j873/$script"
done
for script in thermal-status.sh qualify-cooling.sh; do
    sudo install -m755 "$task_tools/thermal/$script" "$task_root/opt/j873/$script"
done
sudo install -m644 "$task_tools/desktop/xorg.conf" "$task_root/etc/X11/xorg.conf"
sudo chroot "$task_root" apt-get clean
sudo find "$task_root/var/lib/apt/lists" -type f -delete
sudo find "$task_root/var/log" -type f -exec truncate -s 0 {} +
sudo sh -c 'cd "$1" && find . -xdev -print0 | cpio --null -o -H newc' sh "$task_root" |
    gzip -1 > "$task_out/initramfs.cpio.gz"
(cd "$task_out" && sha256sum Image kernel.config t8152-j873.dtb initramfs.cpio.gz > SHA256SUMS)
echo J873_DESKTOP_ARTIFACTS_READY
