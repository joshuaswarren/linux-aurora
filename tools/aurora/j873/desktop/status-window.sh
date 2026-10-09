#!/bin/sh
while :; do
    printf '\033[H\033[2J'
    printf 'J873 Mac mini M6 - Xfce\n\n'
    uname -r
    printf '\n'
    /opt/j873/thermal-status.sh
    printf '\nDisplay: USB-C / DisplayPort, software rendering\n'
    sleep 5
done
