#!/bin/bash
set -euo pipefail
export ARCH=arm64 LOCALVERSION=
task_root=/Users/mcruz/Developer/aurora-silicon/j813-linux
task_module="$HOME/j813-n1-bt-probe-module"
task_output="$task_root/artifacts/bluetooth"
mkdir -p "$task_module" "$task_output"
rsync -a "$task_root/linux/tools/aurora/j813/bluetooth-probe/" "$task_module/"
make -C "$HOME/j813-kernel-source" O="$HOME/j813-kernel-build" \
    M="$task_module" W=1 -j10 modules
cp "$task_module/apple_centauri_bt_probe.ko" "$task_output/"
(cd "$task_output"; sha256sum apple_centauri_bt_probe.ko > SHA256SUMS)
echo J813_BT_PROBE_BUILD_COMPLETE
