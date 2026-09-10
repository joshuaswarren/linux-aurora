#!/bin/bash
set -euo pipefail
export ARCH=arm64 LOCALVERSION=
task_root=/Users/mcruz/Developer/aurora-silicon/j813-linux
task_src="$HOME/j813-kernel-source"
task_build="$HOME/j813-bluetooth-kernel-build"
task_output="$task_root/artifacts/bluetooth/stack"
mkdir -p "$task_output"
if [ ! -d "$task_build" ]; then
    cp -a --reflink=auto "$HOME/j813-kernel-build" "$task_build"
fi
cp "$task_root/artifacts/wifi/kernel.config" "$task_build/.config"
"$task_src/scripts/config" --file "$task_build/.config" --module BT \
    --enable BT_BREDR --enable BT_LE --disable BT_DEBUGFS \
    --disable BT_SELFTEST --disable BT_LEDS --disable BT_HCIBTUSB \
    --disable BT_HCIUART --disable BT_HCIVHCI --disable BT_VIRTIO \
    --enable EPOLL --enable SIGNALFD --enable TIMERFD
make -C "$task_src" O="$task_build" olddefconfig
cp "$task_build/.config" "$task_output/kernel.config"
"$task_src/scripts/diffconfig" "$task_root/artifacts/wifi/kernel.config" \
    "$task_build/.config" > "$task_output/config-delta.txt"
make -C "$task_src" O="$task_build" -j10 modules
make -C "$task_src" O="$task_build" INSTALL_MOD_PATH="$task_output/root" modules_install
echo J813_BLUETOOTH_STACK_BUILD_COMPLETE
