#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-2.0-only
"""Validate a decompressed ARM64 ANE firmware Mach-O; never load or execute it."""
import argparse
import hashlib
import json
import pathlib
import struct

MAX_IMAGE = 32 * 1024 * 1024
MAX_MEMORY = 64 * 1024 * 1024
LC_SEGMENT_64 = 0x19
LC_UNIXTHREAD = 5


class FirmwareError(ValueError):
    pass


def require(condition, message):
    if not condition:
        raise FirmwareError(message)


def inspect_image(data):
    require(32 <= len(data) <= MAX_IMAGE, "invalid firmware file size")
    magic, cpu, subtype, kind, commands, command_bytes, flags, reserved = \
        struct.unpack_from("<8I", data)
    require(magic == 0xfeedfacf, "expected decompressed little-endian Mach-O 64")
    require(cpu == 0x100000c, "firmware is not ARM64")
    require(kind == 5, "firmware is not MH_PRELOAD")
    require(1 <= commands <= 256, "invalid load-command count")
    require(command_bytes <= len(data) - 32, "load commands extend past file")
    end = 32 + command_bytes
    cursor = 32
    segments = []
    entry = None
    for unused in range(commands):
        require(cursor + 8 <= end, "truncated load-command header")
        command, size = struct.unpack_from("<II", data, cursor)
        require(size >= 8 and size % 8 == 0 and size <= end - cursor,
                "invalid load-command size")
        if command == LC_SEGMENT_64:
            require(size >= 72, "truncated segment command")
            values = struct.unpack_from("<16sQQQQiiII", data, cursor + 8)
            name, address, memory_size, offset, file_size, maximum, protection, sections, attrs = values
            require(size == 72 + 80 * sections, "segment section count exceeds command")
            try:
                name = name.rstrip(b"\0").decode("ascii")
            except UnicodeDecodeError as error:
                raise FirmwareError("non-ASCII segment name") from error
            require(name and "\0" not in name, "invalid segment name")
            require(not any(s["name"] == name for s in segments), "duplicate segment name")
            require(file_size <= memory_size, "segment file data exceeds memory size")
            require(offset <= len(data) and file_size <= len(data) - offset,
                    "segment file range exceeds image")
            require(not file_size or offset >= end, "segment overlaps Mach-O headers")
            require(address <= MAX_MEMORY and memory_size <= MAX_MEMORY - address,
                    "firmware memory range exceeds supported inspection window")
            require(0 <= protection <= 7 and 0 <= maximum <= 7
                    and protection & ~maximum == 0, "invalid segment permissions")
            require(protection & 6 != 6, "writable executable segment")
            for old in segments:
                if memory_size and old["memory_size"]:
                    require(address + memory_size <= old["address"] or
                            old["address"] + old["memory_size"] <= address,
                            "overlapping segment memory ranges")
                if file_size and old["file_size"]:
                    require(offset + file_size <= old["file_offset"] or
                            old["file_offset"] + old["file_size"] <= offset,
                            "overlapping segment file ranges")
            segments.append({"name": name, "address": address, "memory_size": memory_size,
                             "file_offset": offset, "file_size": file_size,
                             "zero_fill_size": memory_size - file_size,
                             "protection": protection})
        elif command == LC_UNIXTHREAD:
            require(entry is None, "multiple firmware entry points")
            require(size == 288, "unexpected ARM64 thread command size")
            flavor, count = struct.unpack_from("<II", data, cursor + 8)
            require(flavor == 6 and count == 68, "unexpected ARM64 thread state")
            entry = struct.unpack_from("<Q", data, cursor + 16 + 32 * 8)[0]
        else:
            require(command in (2, 0xb, 0x1b), f"unsupported load command {command:#x}")
            require(size == {2: 24, 0xb: 80, 0x1b: 24}[command],
                    "invalid metadata command size")
        cursor += size
    require(cursor == end, "load-command byte count mismatch")
    require(segments and entry is not None, "missing firmware segments or entry point")
    require(entry % 4 == 0, "unaligned ARM64 entry point")
    require(any(s["protection"] & 4 and s["address"] <= entry <
                s["address"] + s["file_size"] for s in segments),
            "entry point is outside file-backed executable memory")
    return {"format": "arm64-mh-preload", "cpu_subtype": subtype, "flags": flags,
            "sha256": hashlib.sha256(data).hexdigest(), "file_size": len(data),
            "entry_point": entry, "memory_end": max(s["address"] + s["memory_size"]
                                                     for s in segments),
            "segments": segments, "hardware_compatibility_verified": False,
            "firmware_loaded": False, "inference_available": False}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("firmware", type=pathlib.Path)
    args = parser.parse_args()
    try:
        with args.firmware.open("rb") as source:
            result = inspect_image(source.read(MAX_IMAGE + 1))
    except (OSError, FirmwareError) as error:
        parser.exit(1, f"ANE_FIRMWARE_INVALID: {error}\n")
    print(json.dumps(result, indent=2))


if __name__ == "__main__":
    main()
