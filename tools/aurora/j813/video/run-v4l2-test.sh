#!/bin/sh
# Run from the native initramfs after privileged AVE and RAM desktop setup.
set -eu
[ "$#" -ge 2 ] && [ "$#" -le 4 ]
input=$1
result=$2
mode=${3:-remux}
user=${4:-root}
case "$mode" in remux|mpegts) ;; *) exit 2 ;; esac
[ -f "$input" ]
[ ! -e "$result" ]
umask 077
mkdir -p "$result"
cp "$input" "$result/input.nv12"
printf '%s\n' "$mode" >"$result/mode.txt"
chroot /desktop /usr/sbin/runuser -u "$user" -- id >"$result/user.txt"
uid=$(chroot /desktop id -u "$user")
gid=$(chroot /desktop id -g "$user")
chown -R "$uid:$gid" "$result"
debug=/sys/kernel/debug/281100000.video-codec
cat "$debug/ave-status" >"$result/before.txt"
grep -qx 'encoder_owned=1' "$result/before.txt"
grep -qx 'command_failed=0' "$result/before.txt"
# Keep the serial shell usable if the application fails. The caller should
# background this runner and inspect ffmpeg.rc before opening another client.
case "$mode" in
    remux) set -- -f h264 -y "$result/elementary.h264" ;;
    mpegts) set -- -f mpegts -muxdelay 0 -muxpreload 0 -y "$result/recording.ts" ;;
esac
set +e
timeout -k 5 25 chroot /desktop /usr/sbin/runuser -u "$user" -- \
    /usr/bin/ffmpeg -nostdin -hide_banner -loglevel verbose \
    -f rawvideo -pixel_format nv12 -video_size 320x240 -framerate 1 \
    -i "$result/input.nv12" -an -c:v h264_v4l2m2m -g 1 -bf 0 \
    "$@" >"$result/ffmpeg.log" 2>&1
rc=$?
set -e
cat "$debug/ave-status" >"$result/after.txt"
dmesg >"$result/kernel.log"
printf '%s\n' "$rc" >"$result/ffmpeg.rc"
if [ "$rc" = 0 ] && [ "$mode" = remux ]; then
    # This fixed profile returns in-band SPS/PPS. Matroska needs them before
    # writing its header; remux the valid bytestream without video encoding.
    set +e
    chroot /desktop /usr/bin/ffmpeg -hide_banner -loglevel warning -r 1 \
        -i "$result/elementary.h264" -map 0:v:0 -c:v copy \
        -y "$result/recording.mkv" >"$result/remux.log" 2>&1
    rc=$?
    set -e
    printf '%s\n' "$rc" >"$result/remux.rc"
fi
echo "M5_V4L2_TEST_FINISHED rc=$rc"
exit "$rc"
