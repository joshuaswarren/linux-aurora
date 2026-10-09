#!/bin/bash
set -euo pipefail
[[ $(hostname) == lima-j813-builder ]]
task_root=/Users/mcruz/Developer/aurora-silicon/j813-linux
task_module="$HOME/j813-gpu-rtkit-probe"
task_build="$HOME/j813-gpu-kernel-build"
[[ $(cat "$task_build/include/config/kernel.release") == 7.1.6-aurora-j813-gpu-v9 ]]
mkdir -p "$task_module"
cp "$task_root/gpu/linux-rtkit-probe/Makefile" \
   "$task_root/gpu/linux-rtkit-probe/m5-gpu-rtkit-probe.c" "$task_module/"
make -C "$HOME/j813-desktop-kernel-source" O="$task_build" M="$task_module" \
    ARCH=arm64 LLVM=1 W=1 modules
cp "$task_module/m5-gpu-rtkit-probe.ko" "$task_root/artifacts/gpu/native/"
echo M5_RTKIT_PROBE_BUILT
