#!/bin/sh
# SPDX-License-Identifier: GPL-2.0-only
# Read-only status. Does not change clocks, trips, or online CPUs.
set -eu
tr '\000' '\n' < /proc/device-tree/compatible | grep -qx apple,j813
printf 'ONLINE '; cat /sys/devices/system/cpu/online
printf 'CMDLINE '; cat /proc/cmdline
for hwmon in /sys/class/hwmon/hwmon*; do
    test -d "$hwmon" || continue
    printf 'HWMON %s ' "$hwmon"; cat "$hwmon/name"
    for input in "$hwmon"/temp*_input; do
        test -f "$input" || continue
        stem=${input%_input}
        printf 'TEMP %s ' "$input"
        if test -f "${stem}_label"; then
            tr '\n' ' ' < "${stem}_label"
        fi
        cat "$input"
    done
done
for policy in /sys/devices/system/cpu/cpufreq/policy*; do
    test -d "$policy" || continue
    printf 'POLICY %s\n' "$policy"
    for field in affected_cpus related_cpus scaling_driver scaling_governor \
        scaling_available_frequencies scaling_min_freq scaling_max_freq \
        scaling_cur_freq cpuinfo_cur_freq; do
        test -f "$policy/$field" || continue
        printf '%s ' "$field"; cat "$policy/$field"
    done
done
for zone in /sys/class/thermal/thermal_zone*; do
    test -d "$zone" || continue
    printf 'ZONE %s ' "$zone"
    tr '\n' ' ' < "$zone/type"
    cat "$zone/temp"
done
for cooler in /sys/class/thermal/cooling_device*; do
    test -d "$cooler" || continue
    printf 'COOLING %s ' "$cooler"
    tr '\n' ' ' < "$cooler/type"
    tr '\n' ' ' < "$cooler/cur_state"
    cat "$cooler/max_state"
done
