#!/bin/sh
# Cold-start native J813 input. U-Boot must leave this MTP unprobed.
set -eu
grep -aq 'apple,j813' /proc/device-tree/compatible
[ -d /sys/bus/platform/devices/394600000.mtp ]
# Both interfaces share an ID: use their specific drivers together.
modprobe hid-apple
modprobe hid-magicmouse
modprobe apple-mailbox
modprobe apple-rtkit-helper
[ -L /sys/bus/platform/devices/394600000.mtp/driver ]
modprobe apple-dockchannel
modprobe dockchannel-hid
[ -L /sys/bus/platform/devices/394b30000.input/driver ]
echo J813_INPUT_DRIVERS_STARTED
