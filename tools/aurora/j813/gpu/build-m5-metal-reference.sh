#!/bin/sh
# Compile an independent native instruction reference with the installed SDK.
set -eu
cd "$(dirname "$0")/.."
name=${1:?usage: build-m5-metal-reference.sh reference-basename}
case "$name" in *[!a-zA-Z0-9_-]*) exit 2 ;; esac
base="artifacts/gpu/native/$name"
mkdir -p artifacts/gpu/native
if [ -e "${base%-reference}-text" ]; then
    echo "Reference output already exists; preserve it before rebuilding." >&2
    exit 1
fi
xcrun metal -target air64-apple-macosx26.0 -std=metal3.2 -fno-fast-math \
    -c "gpu/$name.metal" -o "$base.air"
xcrun metallib "$base.air" -o "$base.metallib"
xcrun metal-nt -arch applegpu_g17g -platform_version macos 26.0 26.0 \
    -N "gpu/$name.mtlp-json" "$base.metallib" -o "$base.gpubin"
python3 gpu/extract-m5-native-text.py "$base.gpubin" "${base%-reference}-text"
