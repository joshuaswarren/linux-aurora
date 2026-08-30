#!/usr/bin/env python3
"""Select and summarize the current RTBuddy CLGH/CLHE crash record."""

from __future__ import annotations

import argparse
import struct
from pathlib import Path


CLGH = 0x434C4748
CLHE = 0x434C4845
CSTR = 0x43737472
CRG8 = 0x43726738

MULTI_HEADER_SIZE = 0x14
MULTI_RECORD_SIZE = 0x18
LOG_HEADER_SIZE = 0x20
SECTION_HEADER_SIZE = 0x10
MAX_DEPTH = 8

REGS_SIZE = 0x350
REGS_BODY_SIZE = 0x348
REGS_SP = 0x100
REGS_PC = 0x108
REGS_PSR = 0x110
REGS_FAR = 0x330
REGS_ESR = 0x340


def u32(data: bytes | bytearray, offset: int) -> int:
    return struct.unpack_from("<I", data, offset)[0]


def u64(data: bytes | bytearray, offset: int) -> int:
    return struct.unpack_from("<Q", data, offset)[0]


def select_current(data: bytes | bytearray) -> tuple[int, list[int]]:
    offset = 0
    generations: list[int] = []

    for _ in range(MAX_DEPTH):
        if len(data) - offset < LOG_HEADER_SIZE:
            raise ValueError(f"header truncated at {offset:#x}")
        if u32(data, offset) != CLGH:
            break

        version, total_size, current_id, count = struct.unpack_from(
            "<IIII", data, offset + 4
        )
        if ((version >> 8) & 0xFF) != 2:
            raise ValueError(f"unsupported CLGH version {version:#x}")
        if total_size < MULTI_HEADER_SIZE or total_size > len(data) - offset:
            raise ValueError(f"invalid CLGH size {total_size:#x} at {offset:#x}")
        if not count or count * MULTI_RECORD_SIZE > len(data) - offset - MULTI_HEADER_SIZE:
            raise ValueError(f"invalid CLGH record count {count} at {offset:#x}")

        selected = None
        for index in range(count):
            record = offset + MULTI_HEADER_SIZE + index * MULTI_RECORD_SIZE
            if u64(data, record) == current_id:
                selected = u32(data, record + 0x10)
                break
        if selected is None:
            raise ValueError(f"current generation {current_id:#x} has no record")
        if selected < MULTI_HEADER_SIZE or len(data) - offset - selected < LOG_HEADER_SIZE:
            raise ValueError(f"invalid generation offset {selected:#x}")

        generations.append(current_id)
        offset += selected
    else:
        raise ValueError("CLGH nesting exceeds eight levels")

    if u32(data, offset) != CLHE:
        raise ValueError(f"selected record at {offset:#x} is not CLHE")
    version = u32(data, offset + 4)
    size = u32(data, offset + 8)
    flags = u32(data, offset + 0x0C)
    if ((version >> 8) & 0xFF) != 3:
        raise ValueError(f"unsupported CLHE version {version:#x}")
    if size < 2 * LOG_HEADER_SIZE or size > len(data) - offset:
        raise ValueError(f"invalid CLHE size {size:#x}")
    if flags & 0x100:
        raise ValueError(f"CLHE is incomplete (flags {flags:#x})")
    footer = offset + size - LOG_HEADER_SIZE
    if data[offset : offset + LOG_HEADER_SIZE] != data[footer : footer + LOG_HEADER_SIZE]:
        raise ValueError(f"CLHE footer mismatch at {footer:#x}")
    return offset, generations


def summarize(data: bytes) -> None:
    log_offset, generations = select_current(data)
    log_size = u32(data, log_offset + 8)
    chain = " -> ".join(f"{value:#x}" for value in generations) or "direct CLHE"
    print(f"current generations: {chain}")
    print(f"selected CLHE: offset={log_offset:#x} size={log_size:#x}")

    section_end = log_size - LOG_HEADER_SIZE
    offset = LOG_HEADER_SIZE
    while offset < section_end:
        if section_end - offset < SECTION_HEADER_SIZE:
            raise ValueError(f"truncated section header at {offset:#x}")
        section = log_offset + offset
        signature = u32(data, section)
        size = u32(data, section + 0x0C)
        if size < SECTION_HEADER_SIZE or size > section_end - offset:
            raise ValueError(f"invalid section {signature:#x} size {size:#x}")

        payload = section + SECTION_HEADER_SIZE
        payload_size = size - SECTION_HEADER_SIZE
        if signature == CSTR and payload_size > 4:
            message_id = u32(data, payload)
            text = data[payload + 4 : section + size].split(b"\0", 1)[0]
            print(f"Cstr id={message_id:#x}: {text.decode(errors='replace')}")
        elif signature == CRG8 and payload_size >= REGS_SIZE:
            regs_flags = u32(data, payload)
            regs_size = u32(data, payload + 4)
            if regs_size != REGS_BODY_SIZE or regs_flags & 0x10000:
                print(f"Crg8 invalid flags={regs_flags:#x} size={regs_size:#x}")
            else:
                print(
                    "Crg8 "
                    f"pc={u64(data, payload + REGS_PC):#x} "
                    f"sp={u64(data, payload + REGS_SP):#x} "
                    f"psr={u64(data, payload + REGS_PSR):#x} "
                    f"far={u64(data, payload + REGS_FAR):#x} "
                    f"esr={u64(data, payload + REGS_ESR):#x}"
                )
        offset += size


def self_test() -> None:
    data = bytearray(0x800)
    outer_id = 0x434C3455
    inner_id = 0x494E4E52
    struct.pack_into("<IIIII", data, 0, CLGH, 0x202, len(data), outer_id, 2)
    struct.pack_into("<QQII", data, 0x14, outer_id, 0, 0x80, 0)
    struct.pack_into("<QQII", data, 0x2C, 0x5354414C45, 0, 0x400, 0)
    struct.pack_into("<IIIII", data, 0x80, CLGH, 0x202, 0x780, inner_id, 1)
    struct.pack_into("<QQII", data, 0x94, inner_id, 0, 0x40, 0)
    struct.pack_into("<IIII16s", data, 0xC0, CLHE, 0x300, 0x3A0, 0, b"\0" * 16)
    struct.pack_into("<IIII", data, 0xE0, CRG8, 0, 0, 0x360)
    struct.pack_into("<II", data, 0xF0, 0, REGS_BODY_SIZE)
    struct.pack_into("<Q", data, 0xF0 + REGS_PC, 0x41B7528)
    data[0x440:0x460] = data[0xC0:0xE0]

    offset, generations = select_current(data)
    assert offset == 0xC0
    assert generations == [outer_id, inner_id]
    assert u64(data, offset + LOG_HEADER_SIZE + SECTION_HEADER_SIZE + REGS_PC) == 0x41B7528
    summarize(data)
    print("OK nested CLGH selection, CLHE footer, and Crg8 gates")


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("crashlog", type=Path, nargs="?")
    parser.add_argument("--self-test", action="store_true")
    args = parser.parse_args()

    if args.self_test:
        self_test()
    if args.crashlog:
        summarize(args.crashlog.read_bytes())
    if not args.self_test and not args.crashlog:
        parser.error("provide a crashlog file or --self-test")


if __name__ == "__main__":
    main()
