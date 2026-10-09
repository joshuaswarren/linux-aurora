#!/bin/bash
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
dest="${1:?Usage: build-media-probes.sh NEW_OUTPUT_DIRECTORY}"
test ! -e "$dest"
mkdir -p "$dest"
mkdir -p "$dest/source"
mkdir -p "$dest/objects"
cp "$root/video/neo-vt-codec-probe.m" "$root/video/coverage-pixels.h" "$root/audio/neo-audio-probe.m" "$root/audio/neo-audio-selection-probe.m" "$dest/source/"
cp "$root/video/neo-vt-decode-probe.m" "$dest/source/"
for probe in neo-vt-codec-probe neo-audio-probe neo-vt-decode-probe; do
  xcrun clang -fobjc-arc -O2 -g -Wall -Wextra -Werror -c "$dest/source/$probe.m" -o "$dest/objects/$probe.o"
done
xcrun clang -g "$dest/objects/neo-vt-codec-probe.o" \
  -framework Foundation -framework VideoToolbox -framework CoreMedia -framework CoreVideo \
  -framework AVFoundation -o "$dest/neo-vt-codec-probe"
xcrun clang -g "$dest/objects/neo-audio-probe.o" \
  -framework Foundation -framework AudioToolbox -o "$dest/neo-audio-probe"
xcrun clang -g "$dest/objects/neo-vt-decode-probe.o" \
  -framework Foundation -framework AVFoundation -framework VideoToolbox -framework CoreMedia -framework CoreVideo \
  -o "$dest/neo-vt-decode-probe"
codesign -s - "$dest/neo-vt-codec-probe"
codesign -s - "$dest/neo-audio-probe"
codesign -s - "$dest/neo-vt-decode-probe"
dsymutil "$dest/neo-vt-codec-probe"
dsymutil "$dest/neo-audio-probe"
dsymutil "$dest/neo-vt-decode-probe"
shasum -a 256 "$dest/neo-vt-codec-probe" "$dest/neo-audio-probe" "$dest/neo-vt-decode-probe"
