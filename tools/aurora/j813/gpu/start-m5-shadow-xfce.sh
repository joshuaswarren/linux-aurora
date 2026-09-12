#!/bin/sh
set -eu
grep -aq apple,j813 /proc/device-tree/compatible
kill -0 "$(cat /run/m5-shadow-Xorg.pid)"
! chroot /desktop pgrep -u aurora -x xfce4-session >/dev/null 2>&1
sh /bin/j813-input-start
if [ ! -S /run/udev/control ]; then
    chroot /desktop /usr/lib/systemd/systemd-udevd --daemon
    chroot /desktop env SYSTEMD_IGNORE_CHROOT=1 /usr/bin/udevadm trigger
    chroot /desktop env SYSTEMD_IGNORE_CHROOT=1 /usr/bin/udevadm settle --timeout=8
fi
mkdir -p /run/user/1000 /desktop/tmp/.ICE-unix
mkdir -p /run/dbus
chmod 755 /run/user /run/dbus
chmod 700 /run/user/1000
chown 1000:1000 /run/user/1000
chmod 1777 /desktop/tmp/.ICE-unix
chroot /desktop env DISPLAY=:0 sh -c \
    'xset -dpms; xset s off; xset s noblank; xset s reset'
setsid sh /bin/j813-desktop-root-exec /usr/sbin/runuser -u aurora -- env \
    HOME=/home/aurora USER=aurora DISPLAY=:0 XDG_RUNTIME_DIR=/run/user/1000 \
    XDG_CURRENT_DESKTOP=XFCE XDG_SESSION_DESKTOP=XFCE DESKTOP_SESSION=j813-xfce \
    GDK_BACKEND=x11 LIBGL_ALWAYS_SOFTWARE=true GALLIUM_DRIVER=llvmpipe \
    dbus-run-session -- sh -c \
    'xfconf-query -c xfwm4 -p /general/use_compositing --create -t bool -s false;
     xfconf-query -c xfce4-power-manager -p /xfce4-power-manager/dpms-enabled --create -t bool -s false;
     for supply in ac battery; do
         xfconf-query -c xfce4-power-manager -p /xfce4-power-manager/blank-on-$supply --create -t int -s 0;
     done;
     for workspace in 0 1 2 3; do
         xfconf-query -c xfce4-desktop -p /backdrop/screen0/monitorNone-2/workspace$workspace/last-image --create -t string -s /usr/share/backgrounds/xfce/xfce-blue.jpg;
     done;
     exec xfce4-session' \
    >/run/m5-shadow-xfce.log 2>&1 &
echo $! >/run/m5-shadow-xfce.pid
sleep 15
for process in xfce4-session xfwm4 xfce4-panel xfdesktop; do
    chroot /desktop pgrep -u aurora -x "$process" >/dev/null
done
echo M5_SHADOW_XFCE_READY
