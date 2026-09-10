#!/bin/bash
set -euo pipefail
export ARCH=arm64 LOCALVERSION=
task_root=/Users/mcruz/Developer/aurora-silicon/j813-linux
task_src="$HOME/j813-kernel-source"
task_build="$HOME/j813-bluetooth-kernel-build"
task_module="$HOME/j813-n1-hci-module"
task_output="$task_root/artifacts/bluetooth"
mkdir -p "$task_module" "$task_output"
cp "$task_root/linux/drivers/bluetooth/btcentauri.c" "$task_module/"
cp "$task_root/bt-hci.Makefile" "$task_module/Makefile"
make -C "$task_src" O="$task_build" M="$task_module" W=1 -j10 modules
cp "$task_module/btcentauri.ko" "$task_output/"
(cd "$task_output"; sha256sum btcentauri.ko > hci-SHA256SUMS)
echo J813_N1_HCI_BUILD_COMPLETE
