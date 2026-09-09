#!/bin/bash
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
dest="$root/artifacts/prores-packetgen-20260909"
test ! -e "$dest"
mkdir -p "$dest"
cp "$root/video/prores-packetgen.c" "$dest/"
gcc -O2 -g -Wall -Wextra -Werror -static "$dest/prores-packetgen.c" -o "$dest/prores-packetgen-unstripped"
cp "$dest/prores-packetgen-unstripped" "$dest/prores-packetgen"
strip "$dest/prores-packetgen"
sha256sum "$dest/prores-packetgen" "$dest/prores-packetgen-unstripped"
