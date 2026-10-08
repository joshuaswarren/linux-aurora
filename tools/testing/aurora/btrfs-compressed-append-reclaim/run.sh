#!/bin/bash
# SPDX-License-Identifier: GPL-2.0-only
#
# Reproducer for buffered appends failing with EIO on a compressed btrfs
# filesystem under memory cgroup pressure: the extent map shrinker drops the
# extent map between btrfs_do_readpage() and btrfs_submit_compressed_read().
#
# Usage: run.sh <workdir on compress=zstd btrfs> [seconds] [anon_MiB] [wrapper...]
# The optional wrapper runs the writer under another ABI (Aurora: the
# userpages 4k launcher). Exits non-zero if any append fails or verification
# does not match. Must run as root (systemd-run transient unit).
# Set APPEND_RECLAIM to a prebuilt append-reclaim binary to skip compiling
# on the target.
set -eu
DIR=${1:?workdir on a compress=zstd btrfs mount}
SECS=${2:-25}
ANON=${3:-208}
shift $(( $# < 3 ? $# : 3 ))
HERE=$(cd "$(dirname "$0")" && pwd)
mkdir -p "$DIR"
findmnt -no FSTYPE,OPTIONS -T "$DIR" | /usr/bin/grep -q '^btrfs .*compress' ||
	echo "warning: $DIR is not on a compressed btrfs mount; the bug needs compressed extents" >&2
BIN=${APPEND_RECLAIM:-$DIR/append-reclaim}
[ -n "${APPEND_RECLAIM:-}" ] ||
	gcc -O2 -Wall -Wextra -Werror -pthread "$HERE/append-reclaim.c" -o "$BIN"
UNIT=bcar-$$
CG=/sys/fs/cgroup/system.slice/$UNIT.service
rm -f "$DIR/go"
systemd-run --unit="$UNIT" --collect -p MemoryMax=256M -p MemoryHigh=224M \
	-p MemorySwapMax=0 -p RuntimeMaxSec=$((SECS + 45)) -p CPUQuota=100% \
	"$@" "$BIN" "$DIR" "$SECS" "$ANON" "$DIR/go"
for ((i = 0; i < 100; i++)); do [ -s "$CG/cgroup.procs" ] && break; sleep .05; done
touch "$DIR/go"
for ((i = 0; i < 10 * (SECS + 50); i++)); do
	[ -d "$CG" ] || break
	cat "$CG/memory.events" > "$DIR/memory.events" 2>/dev/null || true
	sleep .1
done
journalctl -u "$UNIT" --no-pager -o cat > "$DIR/writer.log"
cat "$DIR/writer.log" "$DIR/memory.events"
dmesg | /usr/bin/grep -E 'BTRFS|t1116' | tail -20 || true
/usr/bin/grep -q 'eio=0 other=0' "$DIR/writer.log" &&
	/usr/bin/grep -q '^VERIFY bytes=' "$DIR/writer.log"
