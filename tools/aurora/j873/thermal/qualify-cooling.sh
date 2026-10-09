#!/bin/sh
# Low OPP and lowered-trip checks. No CPU/GPU stress or SMC writes.
set -eu
tr '\000' '\n' < /proc/device-tree/compatible | grep -qx apple,j873
case "$(cat /sys/devices/system/cpu/online)" in
    0|0-1) ;; *) echo 'Qualify with at most two CPUs online' >&2; exit 1;;
esac
case "${1:-0}" in 0|1) cpu=${1:-0};; *) exit 1;; esac
p=/sys/devices/system/cpu/cpufreq/policy$cpu
low=$(cat "$p/cpuinfo_min_freq")
high=$(cat "$p/cpuinfo_max_freq")
case "$low:$high" in 1440000:1728000|972000:1152000) ;; *) exit 1;; esac
z=
for zone in /sys/class/thermal/thermal_zone*; do
    [ "$(cat "$zone/type")" = smc-tcmb-thermal ] && z=$zone
done
[ -n "$z" ]
c=
for dev in "$z"/cdev*; do
    [ -d "$dev" ] || continue
    [ "$(cat "$dev/type")" = "cpufreq-cpu$cpu" ] && c=$dev
done
[ -n "$c" ]
[ "$(cat "$c/max_state")" -eq 1 ]
trip=$(cat "$z/trip_point_0_temp")
[ "$trip" -eq 75000 ]
temp=$(cat "$z/temp")
[ "$temp" -gt 20000 ] && [ "$temp" -lt 60000 ]
governor=$(cat "$p/scaling_governor")
restore() {
    echo "$low" > "$p/scaling_max_freq"
    echo "$trip" > "$z/trip_point_0_temp"
    echo "$governor" > "$p/scaling_governor"
}
trap restore EXIT HUP INT TERM
snapshot() {
    printf 'PHASE=%s TEMP_MC=' "$1"; cat "$z/temp"
    printf 'COOLING_STATE='; cat "$c/cur_state"
    printf 'REQUESTED_KHZ='; cat "$p/scaling_cur_freq"
    printf 'MAX_KHZ='; cat "$p/scaling_max_freq"
}
echo userspace > "$p/scaling_governor"
echo "$high" > "$p/scaling_max_freq"
echo "$high" > "$p/scaling_setspeed"
sleep 1
[ "$(cat "$p/scaling_cur_freq")" -eq "$high" ]
snapshot low_opp_request
# Keep the thermal governor as the cooling-device owner. Direct cur_state
# injection races the other 124 polling zones and can be cleared immediately.
# A real sensor crossing a deliberately lowered trip exercises the governor,
# cooling callback and frequency QoS limit together, without heat stress.
echo $((temp - 10000)) > "$z/trip_point_0_temp"
sleep 3
[ "$(cat "$c/cur_state")" -eq 1 ]
[ "$(cat "$p/scaling_cur_freq")" -eq "$low" ]
[ "$(cat "$p/scaling_max_freq")" -eq "$low" ]
snapshot automatic_sensor_trip
restore
sleep 3
[ "$(cat "$z/trip_point_0_temp")" -eq 75000 ]
[ "$(cat "$p/scaling_max_freq")" -eq "$low" ]
snapshot restored
echo J873_LINUX_COOLING_VERIFIED_POLICY=$cpu
