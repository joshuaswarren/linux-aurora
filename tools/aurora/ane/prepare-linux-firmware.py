#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-2.0-only
"""Flatten the locally supplied, pinned J700/25G83 image for the RAM test.

No firmware is downloaded or embedded in source. The Linux loader independently
checks the flat image's size and SHA-256 before making any firmware CPU writes.
"""
import argparse
import hashlib
import importlib.util
from pathlib import Path

INPUT_SHA = "32ce2ba5af65742d68e703db66430ca15e0132af7ca5e59ec9b743d7e9ca3f0c"
OUTPUT_SHA = "1c27f00f0b33d6b9cd345276bd6e014396e5ccfe2e6cc1fe8bc7f9a7a1fe2f96"


def flatten(data):
    if hashlib.sha256(data).hexdigest() != INPUT_SHA:
        raise ValueError("expected locally supplied J700 H17 firmware from 25G83")
    spec = importlib.util.spec_from_file_location(
        "ane_firmware_inspector", Path(__file__).with_name("inspect-firmware.py"))
    inspector = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(inspector)
    layout = inspector.inspect_image(data)
    if layout["entry_point"] != 0 or layout["memory_end"] != 0x36c000:
        raise ValueError("unexpected firmware layout")
    image = bytearray(0x380000)
    for segment in layout["segments"]:
        begin, size, source = (segment[key] for key in
                               ("address", "file_size", "file_offset"))
        image[begin:begin + size] = data[source:source + size]
    if hashlib.sha256(image).hexdigest() != OUTPUT_SHA:
        raise ValueError("flattened firmware digest mismatch")
    return bytes(image)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("input", type=Path)
    parser.add_argument("output", type=Path)
    args = parser.parse_args()
    image = flatten(args.input.read_bytes())
    with args.output.open("xb") as output:
        output.write(image)
    args.output.chmod(0o600)
    print(f"ANE_FIRMWARE_PREPARED bytes={len(image)} sha256={OUTPUT_SHA}")


if __name__ == "__main__":
    main()
