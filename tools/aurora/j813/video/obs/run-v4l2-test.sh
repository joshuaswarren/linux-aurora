#!/bin/sh
# Run in the native initramfs, after kernel V4L2 setup and OBS installation.
set -eu
[ "$#" = 2 ]
profile=$1
result=$2
case "$profile" in m5-ave-obs-test*) ;; *) exit 2 ;; esac
case "$result" in m5-obs-v4l2-result*) ;; *) exit 2 ;; esac
case "$profile$result" in *[!a-z0-9-]*) exit 2 ;; esac
[ ! -e "/run/$result" ]
! chroot /desktop pgrep -u aurora -x obs >/dev/null
umask 077
mkdir "/run/$result"
debug=/sys/kernel/debug/281100000.video-codec
cat "$debug/ave-status" >"/run/$result/before.txt"
grep -qx encoder_owned=1 "/run/$result/before.txt"
grep -qx command_failed=0 "/run/$result/before.txt"
chroot /desktop /usr/bin/python3 /tmp/m5-obs/setup-test-profile.py --v4l2 --run-name "$profile"
cp "/desktop/home/aurora/.config/$profile/obs-studio/basic/profiles/M5AVE/basic.ini" "/run/$result/profile.ini"
chroot /desktop /usr/sbin/runuser -u aurora -- id >"/run/$result/user.txt"
setsid sh /bin/j813-desktop-root-exec /usr/sbin/runuser -u aurora -- env \
    HOME=/home/aurora DISPLAY=:0 XAUTHORITY=/home/aurora/.Xauthority \
    XDG_RUNTIME_DIR=/run/user/1000 XDG_CONFIG_HOME="/home/aurora/.config/$profile" \
    LANG=C.UTF-8 LIBGL_ALWAYS_SOFTWARE=1 GALLIUM_DRIVER=llvmpipe \
    QT_X11_NO_MITSHM=1 QT_QPA_PLATFORM=xcb M5_V4L2_AUTOTEST=1 /usr/bin/obs \
    --profile 'M5 AVE hardware test' --disable-shutdown-check \
    --disable-missing-files-check --verbose >"/run/$result/obs.log" 2>&1 &
obs_wrapper=$!
for attempt in $(seq 1 45); do
    pid=$(chroot /desktop pgrep -u aurora -x obs || true)
    if [ -n "$pid" ] && [ ! -e "/run/$result/device-open.txt" ]; then
        for fd in /proc/"$pid"/fd/*; do
            if [ "$(readlink "$fd" || true)" = /dev/video0 ]; then
                echo 'OBS opened /dev/video0 as aurora' >"/run/$result/device-open.txt"
                break
            fi
        done
    fi
    if grep -q '\[m5-v4l2-test\] recording stopped;' "/run/$result/obs.log"; then
        break
    fi
    sleep 1
done
cat "$debug/ave-status" >"/run/$result/after.txt"
dmesg >"/run/$result/kernel.log"
grep -q '\[m5-v4l2-test\] recording stopped;' "/run/$result/obs.log"
set -- $(chroot /desktop pgrep -u aurora -x obs)
[ "$#" = 1 ]
window=$(chroot /desktop /usr/sbin/runuser -u aurora -- env DISPLAY=:0 \
    XAUTHORITY=/home/aurora/.Xauthority wmctrl -lp | awk -v p="$1" '$3 == p {print $1; exit}')
[ -n "$window" ]
chroot /desktop /usr/sbin/runuser -u aurora -- env DISPLAY=:0 \
    XAUTHORITY=/home/aurora/.Xauthority wmctrl -i -c "$window"
for attempt in $(seq 1 10); do
    if ! chroot /desktop pgrep -u aurora -x obs >/dev/null; then break; fi
    sleep 1
done
! chroot /desktop pgrep -u aurora -x obs >/dev/null
set +e
wait "$obs_wrapper"
rc=$?
set -e
printf '%s\n' "$rc" >"/run/$result/obs-exit.rc"
[ "$rc" = 0 ]
chroot /desktop /usr/bin/python3 - "$result" <<'PY'
import pathlib, re, shutil, sys
root = pathlib.Path('/run') / sys.argv[1]
log = (root / 'obs.log').read_text()
path = re.findall(r'\[m5-v4l2-test\] recording stopped; output=(.+)', log)
assert len(path) == 1
recording = pathlib.Path(path[0].strip())
assert recording.parent == pathlib.Path('/home/aurora/Videos/M5-V4L2-test')
assert recording.suffix == '.ts' and recording.stat().st_size > 0
shutil.copyfile(recording, root / 'recording.ts')
print('M5_OBS_V4L2_RECORDING_COLLECTED')
PY
