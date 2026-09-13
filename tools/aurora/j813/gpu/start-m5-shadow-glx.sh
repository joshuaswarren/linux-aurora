#!/bin/sh
# Keep a CPU-presented DCP X server alive for gated GLX compiler tests.
set -eu
grep -aq apple,j813 /proc/device-tree/compatible
[ -d /sys/module/appledrm ]
[ -c /dev/dri/renderD128 ]
[ -x /desktop/usr/bin/glxgears ]
! pidof Xorg >/dev/null 2>&1
# Start sensor firmware before Xorg's one-shot libinput device probe.
sh /bin/j813-input-start
cp /bin/j813-input-ready.py /run/j813-input-ready.py
chroot /desktop /usr/bin/python3 /run/j813-input-ready.py
kms_device=
for node in /sys/class/drm/card[0-9]*; do
    [ -f "$node/dev" ] || continue
    [ "$(basename "$(readlink "$node/device/driver/module")")" = appledrm ] || continue
    [ -z "$kms_device" ]
    kms_device="/dev/dri/$(basename "$node")"
done
[ -n "$kms_device" ]
mkdir -p /dev/pts /run/user/0 /run/dbus /desktop/etc/X11
chmod 700 /run/user/0
if ! grep -q '^devpts /dev/pts devpts ' /proc/mounts; then
    mount -t devpts devpts /dev/pts -o gid=5,mode=620,ptmxmode=666
fi
# /desktop/dev was bound before devpts existed; propagate the new mount.
mkdir -p /desktop/dev/pts
mount --bind /dev/pts /desktop/dev/pts
# POSIX shared memory is needed by desktop applications and worker pools.
# The rescue devtmpfs does not create or mount /dev/shm automatically.
mkdir -p /desktop/dev/shm
if ! awk '$2 == "/desktop/dev/shm" {found=1} END {exit !found}' /proc/mounts; then
    mount -t tmpfs -o mode=1777,nosuid,nodev,size=256m shm /desktop/dev/shm
fi
chroot /desktop python3 -c 'import os, pty; a,b=pty.openpty(); os.close(a); os.close(b)'
chroot /desktop /usr/bin/dbus-uuidgen --ensure
if [ ! -S /run/dbus/system_bus_socket ]; then
    chroot /desktop /usr/bin/dbus-daemon --system --fork --nopidfile
fi
sed -e "s|@KMS_DEVICE@|$kms_device|g" \
    -e 's/Option "AIGLX" "false"/Option "AIGLX" "true"/' \
    -e 's/Disable "glx"/Load "glx"/' \
    -e '/Option "DRI2" "Disable"/d' -e '/Option "DRI3" "Disable"/d' \
    /opt/j813/desktop-dcp.conf >/desktop/etc/X11/m5-shadow-glx.conf
setsid chroot /desktop /usr/bin/env ASAHI_M5_BRINGUP=1 \
    MESA_SHADER_CACHE_DISABLE=true MESA_LOADER_DRIVER_OVERRIDE=zink GALLIUM_DRIVER=zink \
    LIBGL_DRIVERS_PATH=/opt/m5-mesa/lib/dri LD_LIBRARY_PATH=/opt/m5-mesa/lib \
    GBM_BACKENDS_PATH=/opt/m5-mesa/lib/gbm VK_DRIVER_FILES=/opt/m5-mesa/asahi_icd.json \
    XDG_RUNTIME_DIR=/run/user/0 HK_M5_ABORT_BEFORE_IOCTL=1 \
    /usr/lib/xorg/Xorg :0 vt7 -ac -nolisten tcp -noreset -s 0 -dpms \
    -config /etc/X11/m5-shadow-glx.conf -logfile /run/m5-shadow-Xorg.log \
    >/run/m5-shadow-Xorg-console.log 2>&1 &
echo $! >/run/m5-shadow-Xorg.pid
for attempt in 1 2 3 4 5; do
    sleep 1
    if chroot /desktop /usr/bin/xdpyinfo -display :0 >/run/m5-shadow-display.log 2>&1; then
        chvt 7
        echo M5_SHADOW_GLX_SERVER_READY
        exit 0
    fi
done
exit 1
