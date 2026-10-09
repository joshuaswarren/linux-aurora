#!/bin/sh
# SPDX-License-Identifier: GPL-2.0-only
# First-time onlining after a J813 spin-table boot with maxcpus=1.
# The spin-table method does not support offlining; never write a lower state.
set -eu
cpu_root=/sys/devices/system/cpu
tr '\000' '\n' < /sys/firmware/devicetree/base/compatible | grep -qx apple,j813
test "$(cat "$cpu_root/possible")" = 0-9
test "$(cat "$cpu_root/present")" = 0-9
target=$(awk '$2 == "online" {sub(/:$/, "", $1); print $1}' "$cpu_root/hotplug/states")
case "$target" in ''|*[!0-9]*) echo 'Invalid CPU online state' >&2; exit 1;; esac
test "$(cat "$cpu_root/cpu0/hotplug/state")" = "$target"
for cpu in 1 2 3 4 5 6 7 8 9; do
    state=$(cat "$cpu_root/cpu$cpu/hotplug/state")
    if test "$state" = 0; then
        echo "M5_CPU_START=$cpu"
        echo "$target" > "$cpu_root/cpu$cpu/hotplug/target"
    elif test "$state" != "$target"; then
        echo "CPU $cpu is in unexpected intermediate state $state" >&2
        exit 1
    fi
    test "$(cat "$cpu_root/cpu$cpu/hotplug/state")" = "$target"
done
test "$(cat "$cpu_root/online")" = 0-9
echo M5_TEN_CPUS_ONLINE
