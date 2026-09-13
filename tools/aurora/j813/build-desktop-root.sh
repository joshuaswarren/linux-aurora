#!/bin/bash
# Build a clean, signed Ubuntu ARM64 XFCE root in the M5 builder only.
# This does not access either physical Mac or change its installed OS.
set -euo pipefail
[[ $(uname -m) == aarch64 && $(id -u) == 0 ]]
task_shared=/Users/mcruz/Developer/aurora-silicon/j813-linux
task_root=/home/mcruz.guest/j813-desktop-root-v1
task_out="$task_shared/artifacts/desktop"
mkdir -p "$task_out"
if [[ ! -e "$task_root" ]]; then
    debootstrap --arch=arm64 --variant=minbase resolute "$task_root" \
        http://ports.ubuntu.com/ubuntu-ports
fi
[[ -f "$task_root/etc/os-release" ]]
grep -q 'VERSION_CODENAME=resolute' "$task_root/etc/os-release"
cat >"$task_root/usr/sbin/policy-rc.d" <<'EOF'
#!/bin/sh
exit 101
EOF
chmod 755 "$task_root/usr/sbin/policy-rc.d"
cat >"$task_root/etc/apt/sources.list" <<'EOF'
deb http://ports.ubuntu.com/ubuntu-ports resolute main universe
deb http://ports.ubuntu.com/ubuntu-ports resolute-updates main universe
deb http://ports.ubuntu.com/ubuntu-ports resolute-security main universe
EOF
cp -L /etc/resolv.conf "$task_root/etc/resolv.conf"
mkdir -p "$task_root"/{proc,sys,dev,run}
mount --rbind /dev "$task_root/dev"
mount --make-rslave "$task_root/dev"
mount -t proc proc "$task_root/proc"
mount --rbind /sys "$task_root/sys"
mount --make-rslave "$task_root/sys"
cleanup() {
    umount -R "$task_root/sys" || true
    umount "$task_root/proc" || true
    umount -R "$task_root/dev" || true
}
trap cleanup EXIT
chroot "$task_root" apt-get update
packages=(systemd-sysv udev dbus dbus-x11 xfce4 xfce4-terminal
    xserver-xorg-core xserver-xorg-video-fbdev xserver-xorg-input-libinput
    xinit x11-xserver-utils x11-utils xauth xfonts-base fonts-dejavu-core
    mesa-utils xdotool htop bluez blueman pulseaudio pulseaudio-module-bluetooth
    openssh-server iproute2 iputils-ping iw wpasupplicant kmod sudo ca-certificates)
chroot "$task_root" env DEBIAN_FRONTEND=noninteractive \
    apt-get install -y --no-install-recommends "${packages[@]}"
if ! chroot "$task_root" id aurora >/dev/null 2>&1; then
    chroot "$task_root" useradd --create-home --uid 1000 --shell /bin/bash aurora
fi
chroot "$task_root" usermod -a -G audio,video,input aurora
chroot "$task_root" dpkg-query -W -f='${binary:Package} ${Version}\n' \
    >"$task_out/root-package-manifest.txt"
chroot "$task_root" apt-get clean
# Host-specific identities are generated later on the physical target.
rm -f "$task_root"/etc/ssh/ssh_host_* "$task_root/var/lib/dbus/machine-id"
: >"$task_root/etc/machine-id"
: >"$task_root/etc/resolv.conf"
rm -f "$task_root/usr/sbin/policy-rc.d"
cleanup
trap - EXIT
tar --numeric-owner --acls --xattrs --one-file-system -C "$task_root" \
    -czf "$task_out/desktop-root-v1.tar.gz" .
(cd "$task_out"; sha256sum desktop-root-v1.tar.gz root-package-manifest.txt > root-SHA256SUMS)
echo J813_DESKTOP_ROOT_BUILD_COMPLETE
