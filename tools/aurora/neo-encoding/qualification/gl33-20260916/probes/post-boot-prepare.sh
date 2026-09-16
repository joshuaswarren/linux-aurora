#!/bin/bash
# After boot-qualified.sh prints NEO_PIGLIT_LINUX_LOGIN_READY: login, boot id, Wi-Fi, prepare, DCP, X server.
set -u
cd /Users/mcruz/Developer/aurora-silicon
G=neo-linux/artifacts/j700-gl30-cts-20260913
K=neo-linux/artifacts/j700-plasma-keyboard-install-20260913
CON="neo-update/.venv/bin/python neo-linux/console.py"
# The driver the previous boot left installed, which is what the clean-boot
# check verifies. MESA_SHA names a candidate that outlived its boot.
MESA=${MESA_SHA:-18ba98eb83f1d97c78ad849b5eb1cb74670729f0cb12c51b9d7d9096d9910a98}
MODULE=${ASAHI_MODULE_SHA:-aa5b21014671517849cfa12c7415cd5b39f565283c7140f3b3ebaa367e96fb4b}
LABEL=${1:?label}
step() { echo; echo "=== $1 $(date +%T)"; }
step login
timeout --foreground 300 neo-update/.venv/bin/python $G/login-linux.py login-$LABEL 2>&1 | tail -3
step bootid
BOOT=$(timeout 60 $CON --timeout 30 'cat /proc/sys/kernel/random/boot_id' 2>&1 | grep -o '[0-9a-f]\{8\}-[0-9a-f]\{4\}-[0-9a-f]\{4\}-[0-9a-f]\{4\}-[0-9a-f]\{12\}' | grep -v f80b3c8f | tail -1)
echo "BOOT=$BOOT"; [ -n "$BOOT" ] || { echo "no boot id"; exit 1; }
echo "$BOOT" > $G/boot-$LABEL.id
step wifi
timeout 240 $CON --timeout 200 'dmesg -n 3 && bash /opt/neo/plasma-wifi-20260911/bringup-wifi-first.sh' 2>&1 | grep -i 'connect\|ready\|marker\|fail\|error' | tail -4
for i in 1 2 3 4 5 6; do timeout 20 ssh -F $K/linux-ssh-config -o BatchMode=yes -o ConnectTimeout=10 neo-plasma 'cat /proc/sys/kernel/random/boot_id' 2>/dev/null && break; sleep 10; done
step prepare
timeout 180 $CON --timeout 150 "python3 /home/mcruz/gl30-cts-20260913/prepare-clean-boot.py $BOOT $MESA $MODULE; echo RC=\$?" 2>&1 | grep -i 'CLEAN_BOOT\|RC=\|assert\|Error' | tail -4
step dcp
timeout 400 $CON --timeout 360 "bash /home/mcruz/gl30-cts-20260913/restore-dcp-cts.sh $BOOT gl30-cts-$LABEL; echo RC=\$?" 2>&1 | grep -i 'VERIFIED\|RC=\|assert\|Error\|fail' | tail -6
step desktop-state
timeout 60 $CON --timeout 30 'systemctl is-active sddm; pgrep -a Xorg | head -2; pgrep -x plasmashell | head -1' 2>&1 | grep -v '^\s*$' | grep -i 'active\|inactive\|Xorg\|^[0-9]' | head -4
echo "=== prepare done; next: stop-desktop-for-cts.py (if sddm active) then start-x11-hardware.py $BOOT"
