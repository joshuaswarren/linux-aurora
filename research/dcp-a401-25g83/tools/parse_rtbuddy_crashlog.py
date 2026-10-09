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
CRL4 = 0x43726C34
CASC = 0x43617343

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

CL4_SECTION_SIZE = 0x378
CL4_NOMINAL_SIZE = 0xD8
CL4_FLAGS = 0x10
CL4_SIZE = 0x14
CL4_LR = 0x108
CL4_PC = 0x110
CL4_SP = 0x118
CL4_PSR = 0x120
CL4_FPSR = 0x128
CL4_FPCR = 0x130
CL4_TPIDR_EL0 = 0x138
CL4_ESR = 0x358
CL4_FAR = 0x360

CASC_SECTION_SIZE = 0x48
CASC_VERSION = 0x101
CASC_FIELDS = (
    ("l2c_sts", 0x10),
    ("l2c_adr", 0x18),
    ("l2c_inf", 0x20),
    ("lsu_sts", 0x28),
    ("fed_sts", 0x30),
    ("mmu_sts", 0x38),
    ("extra_25g83", 0x40),
)


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
        elif signature == CRL4 and size >= CL4_SECTION_SIZE:
            flags = u32(data, section + CL4_FLAGS)
            nominal_size = u32(data, section + CL4_SIZE)
            if nominal_size != CL4_NOMINAL_SIZE or flags & 0x10000:
                print(f"Crl4 invalid flags={flags:#x} size={nominal_size:#x}")
            else:
                print(
                    "Crl4 "
                    f"lr={u64(data, section + CL4_LR):#x} "
                    f"pc={u64(data, section + CL4_PC):#x} "
                    f"sp={u64(data, section + CL4_SP):#x} "
                    f"psr={u64(data, section + CL4_PSR):#x} "
                    f"fpsr={u64(data, section + CL4_FPSR):#x} "
                    f"fpcr={u64(data, section + CL4_FPCR):#x} "
                    f"tpidr_el0={u64(data, section + CL4_TPIDR_EL0):#x} "
                    f"esr={u64(data, section + CL4_ESR):#x} "
                    f"far={u64(data, section + CL4_FAR):#x}"
                )
        elif signature == CASC:
            version = u32(data, section + 8)
            if version != CASC_VERSION or size != CASC_SECTION_SIZE:
                print(f"CasC invalid version={version:#x} size={size:#x}")
            else:
                values = " ".join(
                    f"{name}={u64(data, section + field_offset):#x}"
                    for name, field_offset in CASC_FIELDS
                )
                print(f"CasC {values}")
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
    struct.pack_into("<IIII16s", data, 0xC0, CLHE, 0x300, 0x400, 0, b"\0" * 16)
    struct.pack_into("<IIII", data, 0xE0, CRL4, 0, 0x401, CL4_SECTION_SIZE)
    struct.pack_into("<II", data, 0xF0, 0, CL4_NOMINAL_SIZE)
    struct.pack_into("<Q", data, 0xE0 + CL4_PC, 0x41B7528)
    struct.pack_into("<IIII", data, 0x458, CASC, 0, CASC_VERSION, CASC_SECTION_SIZE)
    for index, (_, field_offset) in enumerate(CASC_FIELDS, start=1):
        struct.pack_into("<Q", data, 0x458 + field_offset, index)
    data[0x4A0:0x4C0] = data[0xC0:0xE0]

    offset, generations = select_current(data)
    assert offset == 0xC0
    assert generations == [outer_id, inner_id]
    assert u64(data, offset + LOG_HEADER_SIZE + CL4_PC) == 0x41B7528
    assert u64(data, 0x458 + CASC_FIELDS[-1][1]) == 7
    summarize(data)
    print("OK nested CLGH selection, CLHE footer, Crl4 gates, and CasC fields")


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
