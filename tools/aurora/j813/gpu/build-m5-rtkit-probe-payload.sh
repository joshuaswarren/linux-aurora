#!/bin/bash
# Preserve the base diagnostic candidate; enable providers only in this test.
set -euo pipefail
[[ $(hostname) == lima-j813-builder ]]
task_root=/Users/mcruz/Developer/aurora-silicon/j813-linux
task_base="$task_root/artifacts/gpu/native"
task_out="$task_base/rtkit-probe"
mkdir -p "$task_out"
for task_file in Image initramfs.cpio.gz t8142-j813.dtb m5-gpu-rtkit-probe.ko; do
    cp "$task_base/$task_file" "$task_out/"
done
for task_node in /soc/mailbox@582600000 /soc/mailbox@582e00000; do
    [[ $(fdtget "$task_out/t8142-j813.dtb" "$task_node" compatible) == apple,t8142-ascwrap-v6 ]]
    [[ $(fdtget "$task_out/t8142-j813.dtb" "$task_node" status) == disabled ]]
    fdtput -t s "$task_out/t8142-j813.dtb" "$task_node" status okay
done
(cd "$task_out"; sha256sum Image initramfs.cpio.gz t8142-j813.dtb m5-gpu-rtkit-probe.ko > SHA256SUMS)
echo M5_RTKIT_PROBE_RAM_PAYLOAD_BUILT
