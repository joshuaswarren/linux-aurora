#!/bin/sh
# Give desktop processes a real namespace root so unprivileged user namespaces
# work. The initramfs console, radio daemons and original mounts stay untouched.
set -eu
[ "$(id -u)" = 0 ]
if [ "${1-}" != --private-root ]; then
    grep -q '^tmpfs /desktop tmpfs ' /proc/mounts
    [ -x /desktop/usr/bin/env ]
    mkdir -p /desktop/old-root
    exec unshare -m sh "$0" --private-root "$@"
fi
shift
mount --make-rprivate /
cd /desktop
pivot_root . old-root
cd /
umount -l /old-root
exec "$@"
