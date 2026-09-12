#!/bin/sh
# Launch a bounded first XFCE session in the RAM-only /desktop root.
# Networking and the native driver instances remain owned by the running kernel.
set -eu
[ "$(id -u)" = 0 ]
case "$(uname -r)" in
    *-aurora-j813-desktop-v[0-9]*) display_backend=fbdev ;;
    7.1.6-aurora-j813-dcp-v1) display_backend=dcp ;;
    *) exit 1 ;;
esac
grep -aq 'apple,j813' /proc/device-tree/compatible
grep -q '^tmpfs /desktop tmpfs ' /proc/mounts
[ -x /desktop/usr/bin/xfce4-session ]
[ -c /dev/fb0 ]
if [ -x /bin/j813-input-start ]; then
    sh /bin/j813-input-start
fi
if [ -f /run/j813-Xorg.pid ] && kill -0 "$(cat /run/j813-Xorg.pid)" 2>/dev/null; then
    echo 'J813 Xorg is already running; preserve the current session.'
    exit 1
fi
# The minimal initramfs has /dev/ptmx but does not mount devpts. Mount it
# before copying /dev into the desktop and its private session namespace.
mkdir -p /dev/pts
if ! grep -q '^devpts /dev/pts devpts ' /proc/mounts; then
    mount -t devpts devpts /dev/pts -o gid=5,mode=620,ptmxmode=666
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
chmod 755 /run/user /run/dbus
chmod 700 /run/user/1000
chown 1000:1000 /run/user/1000
# The RAM kernel defaults to an empty ICMP socket group range. Restore the
# demo user's ordinary ping sockets without giving the terminal raw sockets.
printf '1000 1000\n' >/proc/sys/net/ipv4/ping_group_range
mkdir -p /desktop/tmp/.ICE-unix
chmod 1777 /desktop/tmp/.ICE-unix
chown 0:0 /desktop/tmp/.ICE-unix
sh /bin/j813-desktop-root-exec /usr/sbin/runuser -u aurora -- \
    /usr/bin/python3 -c 'import os,pty; a,b=pty.openpty(); os.close(a); os.close(b)'
if [ "$display_backend" = dcp ]; then
    # Select the Apple display driver explicitly; card numbers can change
    # when simpledrm relinquishes the boot framebuffer.
    kms_device=
    for node in /sys/class/drm/card[0-9]*; do
        [ -f "$node/dev" ] || continue
        [ "$(basename "$(readlink "$node/device/driver/module")")" = appledrm ] || continue
        [ -z "$kms_device" ] || { echo 'Multiple Apple KMS devices; refusing ambiguous selection.'; exit 1; }
        kms_device="/dev/dri/$(basename "$node")"
    done
    [ -n "$kms_device" ] && [ -c "$kms_device" ]
    sed "s|@KMS_DEVICE@|$kms_device|g" /opt/j813/desktop-dcp.conf \
        >/desktop/etc/X11/j813-display.conf
else
    cp /opt/j813/desktop-fbdev.conf /desktop/etc/X11/j813-display.conf
fi
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
setsid chroot /desktop /usr/lib/xorg/Xorg :0 vt7 -config /etc/X11/j813-display.conf \
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
setsid sh /bin/j813-desktop-root-exec /usr/sbin/runuser -u aurora -- env \
    HOME=/home/aurora USER=aurora DISPLAY=:0 XAUTHORITY=/home/aurora/.Xauthority \
    XDG_RUNTIME_DIR=/run/user/1000 XDG_CURRENT_DESKTOP=XFCE \
    XDG_SESSION_DESKTOP=XFCE DESKTOP_SESSION=j813-xfce GDK_BACKEND=x11 \
    LIBGL_ALWAYS_SOFTWARE=true GALLIUM_DRIVER=llvmpipe \
    dbus-run-session -- sh -c \
    'xfconf-query -c xfwm4 -p /general/use_compositing --create -t bool -s false; exec xfce4-session' \
    >/run/j813-xfce-session.log 2>&1 &
echo $! >/run/j813-xfce.pid
sleep 20
kill -0 "$(cat /run/j813-Xorg.pid)"
kill -0 "$(cat /run/j813-xfce.pid)"
for process in xfce4-session xfwm4 xfce4-panel xfdesktop; do
    chroot /desktop pgrep -u aurora -x "$process" >/dev/null
done
echo J813_XFCE_SESSION_STARTED
