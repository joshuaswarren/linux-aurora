#!/bin/sh
# SPDX-License-Identifier: GPL-2.0-only
# Bounded bring-up check: real temperatures, no CPU/GPU workload.
# Temporarily lowers one passive trip; always restores it and the low clock cap.
set -eu
tr '\000' '\n' < /proc/device-tree/compatible | grep -qx apple,j813
case "$(cat /sys/devices/system/cpu/online)" in
    0|0-1) ;;
    *) echo 'Only one P core and at most one E core may be online' >&2; exit 1;;
esac
case "${1:-p}" in
    p) cpu=0; zone=0; zone_type=smc-tchp-thermal
       low=1308000; test_freq=1608000; max_state=18 ;;
    e) cpu=1; zone=2; zone_type=smc-te04-thermal
       low=972000; test_freq=1152000; max_state=7 ;;
    *) echo 'Usage: qualify-cooling.sh [p|e]' >&2; exit 1 ;;
esac
p=/sys/devices/system/cpu/cpufreq/policy$cpu
z=/sys/class/thermal/thermal_zone$zone
c=/sys/class/thermal/cooling_device$cpu
test "$(cat "$p/scaling_governor")" = userspace
test "$(cat "$c/type")" = cpufreq-cpu$cpu
test "$(cat "$c/max_state")" = "$max_state"
test "$(cat "$z/type")" = "$zone_type"
test "$(cat "$z/trip_point_0_type")" = passive
trip=$(cat "$z/trip_point_0_temp")
test "$trip" = 75000
temp=$(cat "$z/temp")
test "$temp" -gt 20000 && test "$temp" -lt 65000
restore() {
    echo "$trip" > "$z/trip_point_0_temp"
    echo "$low" > "$p/scaling_max_freq"
    echo "$low" > "$p/scaling_setspeed"
}
trap restore EXIT HUP INT TERM
snapshot() {
    printf 'PHASE %s TEMP=' "$1"; cat "$z/temp"
    printf 'TRIP='; cat "$z/trip_point_0_temp"
    printf 'COOLING='; cat "$c/cur_state"
    printf 'REQUESTED_KHZ='; cat "$p/scaling_cur_freq"
    printf 'MAX_KHZ='; cat "$p/scaling_max_freq"
}
# First validate the standard CPU cooling callback at a low operating point.
echo "$test_freq" > "$p/scaling_max_freq"
sleep 1
echo "$test_freq" > "$p/scaling_setspeed"
sleep 1
snapshot before_cooling_callback
test "$(cat "$p/scaling_cur_freq")" = "$test_freq"
echo "$max_state" > "$c/cur_state"
sleep 1
snapshot cooling_callback
test "$(cat "$p/scaling_cur_freq")" = "$low"
test "$(cat "$p/scaling_max_freq")" = "$low"
echo 0 > "$c/cur_state"
sleep 1
snapshot cooling_released
test "$(cat "$p/scaling_max_freq")" = "$test_freq"
# Restore the lowest cap before testing automatic activation at a real reading.
echo "$low" > "$p/scaling_max_freq"
echo "$low" > "$p/scaling_setspeed"
echo $((temp - 10000)) > "$z/trip_point_0_temp"
sleep 3
snapshot automatic_passive_trip
test "$(cat "$c/cur_state")" -gt 0
restore
sleep 3
snapshot restored
test "$(cat "$z/trip_point_0_temp")" = 75000
test "$(cat "$p/scaling_max_freq")" = "$low"
echo M5_COOLING_CALLBACK_AND_REAL_SENSOR_TRIP_VERIFIED_CPU=$cpu
