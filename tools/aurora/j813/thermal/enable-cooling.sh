#!/bin/sh
# SPDX-License-Identifier: GPL-2.0-only
# Configure the minimal thermal RAM image. Does not online additional CPUs.
set -eu
tr '\000' '\n' < /proc/device-tree/compatible | grep -qx apple,j813
modprobe apple-soc-cpufreq
for policy in /sys/devices/system/cpu/cpufreq/policy*; do
    test -d "$policy" || continue
    test "$(cat "$policy/scaling_driver")" = apple-cpufreq
    low=$(cat "$policy/cpuinfo_min_freq")
    echo userspace > "$policy/scaling_governor"
    echo "$low" > "$policy/scaling_max_freq"
    echo "$low" > "$policy/scaling_setspeed"
done
modprobe macsmc
modprobe macsmc-hwmon
test "$(cat /sys/class/hwmon/hwmon0/name)" = macsmc_hwmon
set -- /sys/class/hwmon/hwmon0/temp*_input
test "$#" -eq 43
for input do
    value=$(cat "$input")
    test "$value" -gt 0 && test "$value" -lt 150000
done
set -- /sys/class/thermal/thermal_zone*
test "$#" -eq 43
for zone do
    test "$(cat "$zone/trip_point_0_type")" = passive
    test "$(cat "$zone/trip_point_0_temp")" = 75000
    test -L "$zone/cdev0"
done
sleep 1
for policy in /sys/devices/system/cpu/cpufreq/policy*; do
    test -d "$policy" || continue
    test "$(cat "$policy/scaling_max_freq")" = "$(cat "$policy/cpuinfo_min_freq")"
done
echo M5_THERMAL_MONITORING_AND_LOW_CLOCK_CAPS_READY
