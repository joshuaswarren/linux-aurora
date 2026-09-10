#!/bin/sh
# Launch a bounded first XFCE session in the RAM-only /desktop root.
# Networking and the native driver instances remain owned by the running kernel.
set -eu
[ "$(id -u)" = 0 ]
case "$(uname -r)" in *-aurora-j813-desktop-v[0-9]*) ;; *) exit 1 ;; esac
grep -q '^tmpfs /desktop tmpfs ' /proc/mounts
[ -x /desktop/usr/bin/xfce4-session ]
[ -c /dev/fb0 ]
if [ -f /run/j813-Xorg.pid ] && kill -0 "$(cat /run/j813-Xorg.pid)" 2>/dev/null; then
    echo 'J813 Xorg is already running; preserve the current session.'
    exit 1
fi
for directory in dev proc sys run; do
    mkdir -p "/desktop/$directory"
    if ! grep -q " /desktop/$directory " /proc/mounts; then
        mount --rbind "/$directory" "/desktop/$directory"
        mount --make-rslave "/desktop/$directory"
    fi
done
cp /etc/resolv.conf /desktop/etc/resolv.conf
mkdir -p /desktop/etc/X11 /run/dbus /run/user/1000
chmod 700 /run/user/1000
chown 1000:1000 /run/user/1000
cp /opt/j813/desktop-fbdev.conf /desktop/etc/X11/j813-fbdev.conf
chroot /desktop /usr/bin/dbus-uuidgen --ensure
if [ ! -S /run/dbus/system_bus_socket ]; then
    chroot /desktop /usr/bin/dbus-daemon --system --fork --nopidfile
fi
if [ ! -S /run/udev/control ]; then
    chroot /desktop /usr/lib/systemd/systemd-udevd --daemon
    chroot /desktop env SYSTEMD_IGNORE_CHROOT=1 /usr/bin/udevadm trigger
    chroot /desktop env SYSTEMD_IGNORE_CHROOT=1 /usr/bin/udevadm settle --timeout=8
fi
cookie=$(chroot /desktop /usr/bin/mcookie)
chroot /desktop /usr/bin/xauth -q -f /home/aurora/.Xauthority add :0 . "$cookie"
unset cookie
chown 1000:1000 /desktop/home/aurora/.Xauthority
chmod 600 /desktop/home/aurora/.Xauthority
setsid chroot /desktop /usr/lib/xorg/Xorg :0 vt7 -config /etc/X11/j813-fbdev.conf \
    -auth /home/aurora/.Xauthority -nolisten tcp -noreset -logfile /run/j813-Xorg.log \
    >/run/j813-Xorg-console.log 2>&1 &
echo $! >/run/j813-Xorg.pid
for attempt in 1 2 3 4 5 6 7 8 9 10; do
    if chroot /desktop /usr/sbin/runuser -u aurora -- env DISPLAY=:0 \
        XAUTHORITY=/home/aurora/.Xauthority xdpyinfo >/run/j813-display-info.log 2>&1; then
        break
    fi
    kill -0 "$(cat /run/j813-Xorg.pid)"
    sleep 1
done
chroot /desktop /usr/sbin/runuser -u aurora -- env DISPLAY=:0 \
    XAUTHORITY=/home/aurora/.Xauthority xdpyinfo >/run/j813-display-info.log 2>&1
chvt 7
setsid chroot /desktop /usr/sbin/runuser -u aurora -- env \
    HOME=/home/aurora USER=aurora DISPLAY=:0 XAUTHORITY=/home/aurora/.Xauthority \
    XDG_RUNTIME_DIR=/run/user/1000 XDG_CURRENT_DESKTOP=XFCE \
    XDG_SESSION_DESKTOP=XFCE DESKTOP_SESSION=j813-xfce GDK_BACKEND=x11 \
    LIBGL_ALWAYS_SOFTWARE=true GALLIUM_DRIVER=llvmpipe \
    dbus-run-session -- sh -c \
    'xfconf-query -c xfwm4 -p /general/use_compositing --create -t bool -s false; exec xfce4-session' \
    >/run/j813-xfce-session.log 2>&1 &
echo $! >/run/j813-xfce.pid
sleep 5
kill -0 "$(cat /run/j813-Xorg.pid)"
kill -0 "$(cat /run/j813-xfce.pid)"
echo J813_XFCE_SESSION_STARTED
