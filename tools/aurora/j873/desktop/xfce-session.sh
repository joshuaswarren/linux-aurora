#!/bin/sh
xset s off
xset -dpms || true
xfconf-query -c xfwm4 -p /general/use_compositing -n -t bool -s false
(sleep 4; xterm -geometry 80x20+36+70 -title 'J873 Linux hardware status' \
    -e /opt/j873/status-window.sh) &
exec xfce4-session
