#!/bin/sh
set -eu
tr '\000' '\n' < /proc/device-tree/compatible | grep -qx apple,j873
printf 'ONLINE_CPUS='; cat /sys/devices/system/cpu/online
for p in /sys/devices/system/cpu/cpufreq/policy*; do
    [ -d "$p" ] || continue
    printf '%s GOVERNOR=' "${p##*/}"; cat "$p/scaling_governor"
    printf 'REQUESTED_KHZ='; cat "$p/scaling_cur_freq"
    printf 'MAX_KHZ='; cat "$p/scaling_max_freq"
done
found=0
for h in /sys/class/hwmon/hwmon*; do
    [ "$(cat "$h/name")" = macsmc_hwmon ] || continue
    found=1
    count=0; min=200000; max=-1
    for f in "$h"/temp*_input; do
        v=$(cat "$f")
        [ "$v" -gt 5000 ] && [ "$v" -lt 120000 ] || {
            echo "INVALID_TEMPERATURE $f=$v"; exit 1;
        }
        [ "$v" -ge "$min" ] || min=$v
        [ "$v" -le "$max" ] || max=$v
        count=$((count + 1))
    done
    printf 'SMC_CHANNELS=%s MIN_MC=%s MAX_MC=%s\n' "$count" "$min" "$max"
    [ "$count" -eq 125 ]
    printf 'FAN_RPM='; cat "$h/fan1_input"
    [ "$(cat "$h/fan1_input")" -ge 900 ]
done
[ "$found" -eq 1 ]
zones=0
for z in /sys/class/thermal/thermal_zone*; do
    [ -d "$z" ] || continue
    [ "$(cat "$z/trip_point_0_type")" = passive ]
    [ "$(cat "$z/trip_point_0_temp")" -eq 75000 ]
    zones=$((zones + 1))
done
[ "$zones" -eq 125 ]
printf 'SMC_THERMAL_ZONES=%s\n' "$zones"
for c in /sys/class/thermal/cooling_device*; do
    [ -d "$c" ] || continue
    printf '%s TYPE=' "${c##*/}"; cat "$c/type"
    printf 'STATE='; cat "$c/cur_state"
done
echo J873_LINUX_THERMAL_TELEMETRY_READY
