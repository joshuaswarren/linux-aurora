#!/bin/sh
set -eu
test "$(hostname)" = lima-j813-builder
task_root="$HOME/j873-desktop-root"
test ! -e "$task_root"
sudo debootstrap --arch=arm64 --variant=minbase --components=main,universe \
    resolute "$task_root" https://ports.ubuntu.com/ubuntu-ports
printf '#!/bin/sh\nexit 101\n' | sudo tee "$task_root/usr/sbin/policy-rc.d" >/dev/null
sudo chmod 755 "$task_root/usr/sbin/policy-rc.d"
sudo chroot "$task_root" /usr/bin/env DEBIAN_FRONTEND=noninteractive \
    apt-get -y --no-install-recommends install \
    xserver-xorg-core xserver-xorg-video-fbdev xserver-xorg-input-libinput \
    xserver-xorg-input-evdev xinit xauth x11-xserver-utils x11-utils xdotool \
    xfce4-session xfwm4 xfdesktop4 xfce4-panel xfce4-settings xfce4-terminal \
    thunar dbus-x11 dbus-system-bus-common dbus-session-bus-common \
    udev fonts-dejavu-core adwaita-icon-theme xterm \
    busybox-static kmod procps util-linux iproute2
sudo chroot "$task_root" dpkg-query -W > "$HOME/j873-desktop-packages.tsv"
echo J873_DESKTOP_ROOTFS_PACKAGES_READY
