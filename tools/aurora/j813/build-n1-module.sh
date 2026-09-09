#!/bin/bash
set -euo pipefail
export ARCH=arm64 LOCALVERSION=
task_root=/Users/mcruz/Developer/aurora-silicon/j813-linux
task_module="$HOME/j813-n1-module"
rsync -a "$task_root/linux/drivers/net/wireless/apple/centauri/" "$task_module/"
make -C "$HOME/j813-kernel-source" O="$HOME/j813-kernel-build" \
    M="$task_module" CONFIG_APPLE_CENTAURI=m W=1 -j10 modules
cp "$task_module/apple_centauri.ko" "$task_root/artifacts/wifi/apple_centauri.ko"
echo J813_N1_MODULE_BUILD_COMPLETE
