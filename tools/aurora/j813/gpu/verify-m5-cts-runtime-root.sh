#!/bin/sh
# Qualify the reduced runtime without submitting another GPU workload.
set -eu
grep -aq apple,j813 /proc/device-tree/compatible
root=/tmp/m5-cts-runtime-root
test ! -e "$root"
echo 'b516ad9186d06120bc09f3e5d9d29c89314364a055298f25344e934bedf33714  /run/m5-cts-runtime-root.tar.gz' | sha256sum -c -
mkdir "$root"
tar -xzf /run/m5-cts-runtime-root.tar.gz -C "$root" 2>/run/m5-cts-runtime-extract.log
mkdir -p "$root/dev" "$root/proc" "$root/sys" "$root/opt" "$root/run/sshd" "$root/run/user/0"
chmod 700 "$root/run/user/0"
cleanup() {
    for directory in opt sys proc dev; do
        umount "$root/$directory" 2>/dev/null || true
    done
}
trap cleanup EXIT
for directory in dev proc sys opt; do
    mount --bind "/$directory" "$root/$directory"
done
chroot "$root" /bin/sh -ec 'python3 --version; sha256sum /etc/os-release; head -n 1 /etc/os-release; install --version | head -n 1; tar --version | head -n 1; gzip --version | head -n 1'
chroot "$root" /usr/bin/ssh-keygen -A
chroot "$root" /usr/sbin/sshd -t
chroot "$root" /bin/sh -ec '
    cd /opt/m5-cts/surfaceless
    exec /opt/m5-native-gallium/native-env /usr/bin/env EGL_PLATFORM=surfaceless \
        /opt/m5-cts/surfaceless/deqp-gles3 --deqp-runmode=txt-caselist \
        --deqp-surface-type=pbuffer --deqp-surface-width=64 --deqp-surface-height=64 \
        --deqp-gl-config-name=rgba8888d0s0ms0 \
        --deqp-caselist-export-file=/tmp/m5-runtime-gles3-inventory.txt
'
chroot "$root" /usr/bin/python3 -c '
from pathlib import Path
lines=Path("/tmp/m5-runtime-gles3-inventory.txt").read_text().splitlines()
names=[line[6:] for line in lines if line.startswith("TEST: ")]
assert len(names)==48300 and len(set(names))==48300
print("M5_CTS_RUNTIME_VERIFIED: 48300 unique cases enumerated; no rendering tests run")
'
