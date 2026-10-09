#!/bin/sh
# Run in the M5 initramfs; sources/templates must be in /desktop/tmp/m5-obs.
set -eu
test "$#" = 2
profile=$1
result=$2
case "$profile" in m5-ave-obs-test*) ;; *) exit 2;; esac
case "$result" in m5-obs-ave-result*) ;; *) exit 2;; esac
case "$profile$result" in *[!a-z0-9-]*) exit 2;; esac
test ! -S /run/j813-ave-obs.sock
test ! -e "/run/$result"
! chroot /desktop /usr/bin/pgrep -u aurora -x obs >/dev/null
chroot /desktop /usr/bin/python3 /tmp/m5-obs/setup-test-profile.py --run-name "$profile"
setsid chroot /desktop /usr/bin/python3 /tmp/m5-obs/ave-broker.py \
    /tmp/m5-obs "/run/$result" >"/run/$result-broker.log" 2>&1 &
broker=$!
sleep 1
kill -0 "$broker"
test -S /run/j813-ave-obs.sock
setsid sh /bin/j813-desktop-root-exec /usr/sbin/runuser -u aurora -- env \
    HOME=/home/aurora DISPLAY=:0 XAUTHORITY=/home/aurora/.Xauthority \
    XDG_RUNTIME_DIR=/run/user/1000 XDG_CONFIG_HOME="/home/aurora/.config/$profile" \
    LANG=C.UTF-8 LIBGL_ALWAYS_SOFTWARE=1 GALLIUM_DRIVER=llvmpipe \
    QT_X11_NO_MITSHM=1 QT_QPA_PLATFORM=xcb M5_AVE_AUTOTEST=1 /usr/bin/obs \
    --profile 'M5 AVE hardware test' --disable-shutdown-check \
    --disable-missing-files-check --verbose >"/run/$result-obs.log" 2>&1 &
for attempt in $(seq 1 40); do
    if grep -q 'recording stopped;' "/run/$result-obs.log"; then
        break
    fi
    sleep 1
done
grep -q 'recording stopped;' "/run/$result-obs.log"
# Only one OBS instance was allowed to start; close it after muxing stops.
set -- $(chroot /desktop /usr/bin/pgrep -u aurora -x obs)
test "$#" = 1
kill -TERM "$1"
for attempt in $(seq 1 10); do
    if ! chroot /desktop /usr/bin/pgrep -u aurora -x obs >/dev/null; then
        break
    fi
    sleep 1
done
! chroot /desktop /usr/bin/pgrep -u aurora -x obs >/dev/null
wait "$broker"
cp "/run/$result-obs.log" "/run/$result-broker.log" "/run/$result/"
chroot /desktop /usr/bin/python3 -c \
    'import json,sys; p=json.load(open(sys.argv[1])); assert p["passed"]; print("M5_OBS_RUN_PASSED", p["obs_frames"], "hardware frames")' \
    "/run/$result/broker-result.json"
