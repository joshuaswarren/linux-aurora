#!/bin/sh
# One fresh RAM boot only. Pass the retained diagnostic Bluetooth identity.
set -eu
[ $# = 1 ]
[ -c /dev/fb0 ]
modprobe apple_centauri station_link_addresses=1
d=/sys/kernel/debug/apple_centauri/0000:01:00.0
for n in 1 2 3; do echo "$n" >"$d/rom_boot"; done
ip link set wlan0 up
for n in 76 24 15 12; do echo "$n" >"$d/data_probe"; done
iw dev wlan0 set power_save off
iw reg set US
echo 1 >"$d/host_wakeup"
echo 1 >"$d/tx_config"
echo 2 >"$d/tx_config"
cp -a /opt/aurora-bluetooth /run/bt
/run/bt/load-core.sh
/run/bt/btmon -w /run/bt/desktop-startup.btsnoop >/run/bt/desktop-startup-monitor.log 2>&1 &
echo $! >/run/bt/desktop-startup-monitor.pid
sleep 1
echo 0000:01:00.2 >/sys/bus/pci/drivers/apple_centauri/unbind
echo btcentauri >/sys/bus/pci/devices/0000:01:00.2/driver_override
insmod /run/bt/modules/btcentauri.ko arm_auxiliary_rings=1 native_startup=1 \
    enable_acl=1 native_hrb=1 native_bdaddr="$1"
sleep 3
/run/bt/btmgmt --timeout 1 ssp on
/run/bt/btmgmt --timeout 1 sc on
/run/bt/btmgmt --timeout 1 bondable on
/run/bt/btmgmt --timeout 1 io-cap 1
/run/bt/btmgmt --timeout 1 name Aurora-N1-Test
/run/bt/btmgmt --timeout 3 power on
/run/bt/n1-bt-mgmt-probe native-host-awake
/run/bt/btmgmt --timeout 1 info >/run/bt/desktop-adapter-info.log 2>&1
cat /sys/kernel/debug/btcentauri/status
echo J813_DESKTOP_RADIOS_INITIALIZED
