#!/bin/sh
# Bootstrap the RAM userspace; subsequent driver/test transfers use SSH.
set -eu
server=${1%/}
root_archive=${2:-desktop-root-v1.tar.gz}
root_sha256=${3:-ada4e031949b4985bc8ee9abeac64faf3a28178a59f9ba7a1e0b624f72e202bc}
case "$root_archive" in ''|*[!A-Za-z0-9_.-]*|.*) exit 2;; esac
test "${#root_sha256}" -eq 64
case "$root_sha256" in *[!0-9a-f]*) exit 2;; esac
grep -aq apple,j813 /proc/device-tree/compatible
! grep -q ' /desktop ' /proc/mounts
mkdir -p /desktop
mount -t tmpfs -o size=8G tmpfs /desktop
mark_phase() {
    read task_uptime task_idle < /proc/uptime
    printf 'M5_ROOT_PHASE %s uptime_seconds=%s\n' "$1" "$task_uptime"
}
mark_phase download-start
wget -T 20 -q "$server/$root_archive" -O /desktop/.root.tar.gz
mark_phase download-complete
printf '%s  /desktop/.root.tar.gz\n' "$root_sha256" | sha256sum -c -
mark_phase archive-verified
tar -xzf /desktop/.root.tar.gz -C /desktop
mark_phase archive-extracted
rm /desktop/.root.tar.gz
for directory in dev proc sys run; do
    mkdir -p "/desktop/$directory"
    mount --rbind "/$directory" "/desktop/$directory"
    mount --make-rslave "/desktop/$directory"
done
cp /etc/resolv.conf /desktop/etc/resolv.conf
mkdir -p /desktop/root/.ssh /desktop/run/sshd /desktop/dev/shm
chmod 700 /desktop/root/.ssh
wget -T 10 -q "$server/m5-native-host-public-key.pub" -O /desktop/root/.ssh/authorized_keys
chmod 600 /desktop/root/.ssh/authorized_keys
mount -t tmpfs -o mode=1777,nosuid,nodev,size=256m shm /desktop/dev/shm
chroot /desktop ssh-keygen -A
chroot /desktop /usr/sbin/sshd -t
chroot /desktop /usr/sbin/sshd -o PasswordAuthentication=no \
    -o KbdInteractiveAuthentication=no -o PermitRootLogin=prohibit-password \
    -E /run/m5-native-sshd.log
chroot /desktop ssh-keygen -lf /etc/ssh/ssh_host_ed25519_key.pub
echo M5_NATIVE_GLES3_ROOT_SSH_READY
mark_phase ssh-ready
