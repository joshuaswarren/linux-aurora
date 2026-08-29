#!/usr/bin/env python3
"""Verify and inspect the exact macOS 25G83 t8140 DCP root task."""

from __future__ import annotations

import argparse
import hashlib
import struct
from pathlib import Path

from capstone import CS_ARCH_ARM64, CS_MODE_LITTLE_ENDIAN, Cs


HERE = Path(__file__).resolve().parent.parent
DEFAULT_IMAGE = HERE / "extracted" / "t8140dcp-25G83.bin"
IMAGE_SHA256 = "392abf0c42733c5b35e877f72f2c0a3a610c1f89aa39c0ce8358092451d9a8af"
IMAGE_SIZE = 15_417_344

BUNDLE_DATA_FILE_OFFSET = 0x4E0000
LINK_OBJECT_VA = 0x04963D80
LINK_OBJECT_FILE_OFFSET = 0x9F1D80
LINK_LOCAL0_RESPONSE_VA = 0x04963E40
LINK_LOCAL0_RESPONSE_FILE_OFFSET = 0x9F1E40

TEXT_VA = 0x04000000
TEXT_FILE_OFFSET = 0x027000
DATA_VA = 0x04459000
DATA_FILE_OFFSET = 0x4E7000

UNIFIED_PIPELINE_VTABLE = 0x0451B9C0
H17P_UPPIPE_VTABLES = (0x04509A28, 0x0450BB90, 0x0450D200)


def va_to_offset(va: int) -> int:
    if TEXT_VA <= va < DATA_VA:
        return TEXT_FILE_OFFSET + va - TEXT_VA
    if va >= DATA_VA:
        return DATA_FILE_OFFSET + va - DATA_VA
    raise ValueError(f"VM address below root-task __TEXT: {va:#x}")


def auth_rebase_target(raw: int) -> int:
    """Decode the target part of these ARM64e authenticated rebases."""
    return TEXT_VA + (raw & 0xFFFFFFFF)


class Image:
    def __init__(self, path: Path):
        self.path = path
        self.data = path.read_bytes()

    def bytes_at(self, va: int, size: int) -> bytes:
        offset = va_to_offset(va)
        return self.data[offset : offset + size]

    def u64(self, va: int) -> int:
        return struct.unpack("<Q", self.bytes_at(va, 8))[0]

    def slot_target(self, address_point: int, slot: int) -> int:
        return auth_rebase_target(self.u64(address_point + slot))


def require(actual: object, expected: object, label: str) -> None:
    if actual != expected:
        raise SystemExit(f"FAIL {label}: got {actual!r}, expected {expected!r}")
    print(f"OK   {label}: {actual!r}")


def verify(image: Image) -> None:
    require(len(image.data), IMAGE_SIZE, "decompressed size")
    require(hashlib.sha256(image.data).hexdigest(), IMAGE_SHA256, "decompressed SHA-256")
    require(
        b"AppleDCP-1041.120.7~2715-t8140dcp.RELEASE" in image.data,
        True,
        "firmware build string",
    )
    require(va_to_offset(LINK_OBJECT_VA), LINK_OBJECT_FILE_OFFSET,
            "DCPLink object file offset")
    require(
        LINK_OBJECT_FILE_OFFSET - BUNDLE_DATA_FILE_OFFSET,
        0x511D80,
        "DCPLink object offset in dcp_data",
    )
    require(va_to_offset(LINK_LOCAL0_RESPONSE_VA), LINK_LOCAL0_RESPONSE_FILE_OFFSET,
            "DCPLink local stream 0 response file offset")
    require(
        LINK_LOCAL0_RESPONSE_FILE_OFFSET - BUNDLE_DATA_FILE_OFFSET,
        0x511E40,
        "DCPLink local stream 0 response offset in dcp_data",
    )

    require(image.slot_target(UNIFIED_PIPELINE_VTABLE, 0x38), 0x04264C38,
            "UnifiedPipeline slot 0x38 / start_ios_gated")
    require(image.slot_target(UNIFIED_PIPELINE_VTABLE, 0x170), 0x041F7A08,
            "UnifiedPipeline slot 0x170 / D563 thunk")

    for address_point in H17P_UPPIPE_VTABLES:
        require(image.slot_target(address_point, 0x378), 0x042ABBA0,
                f"H17P UPPipe {address_point:#x} slot 0x378")
        require(image.slot_target(address_point, 0x348), 0x0423AD50,
                f"H17P UPPipe {address_point:#x} slot 0x348")
        require(image.slot_target(address_point, 0xF48), 0x042A74F4,
                f"H17P UPPipe {address_point:#x} slot 0xf48 / late_init_ios")

    strings = {
        0x04451FFA: b"MaxVideoSrcDownscalingWidth\0",
        0x04452016: b"VideoClock\0",
        0x04452021: b"PixelClock\0",
        0x0445216E: b"DisplayClock\0",
        0x0445217B: b"DispPerfState\0",
    }
    for va, expected in strings.items():
        require(image.bytes_at(va, len(expected)), expected, f"string at {va:#x}")

    # Key instructions, little-endian, pin the dispatcher and return gate.
    instructions = {
        0x041FE89C: 0x710010BF,  # cmp w5, #4
        0x042654B4: 0xD73F0910,  # authenticated call through UPPipe slot 0x378
        0x042AC0FC: 0xD73F0931,  # fifth property call through slot 0x170
        0x041B5438: 0x79409268,  # ldrh w8, [stream, #0x48]
        0x041B543C: 0x0A150108,  # and w8, w8, #0xc3
        0x041B5440: 0x7101091F,  # cmp w8, #0x42
        0x041B781C: 0xF9002509,  # str x9, [x8, #0x48]
        0x041B552C: 0xB844A268,  # ldur w8, [stream, #0x4a] / remote error
        0x041B5530: 0x35000D88,  # cbnz w8, firmware abort branch
        0x041B56E0: 0xD0001429,  # form abort-path target
        0x041B56F0: 0x17FFFFFA,  # loop after abort call
        0x04021F18: 0xD503237F,  # abort path entry
        0x04021F50: 0x94003C88,  # abort reporting call
        0x04021F58: 0x97FFFFCF,  # non-returning abort call
    }
    for va, expected in instructions.items():
        actual = struct.unpack("<I", image.bytes_at(va, 4))[0]
        require(actual, expected, f"instruction word at {va:#x}")


def disassemble(image: Image, start: int, end: int) -> None:
    if end <= start:
        raise SystemExit("end must be greater than start")
    md = Cs(CS_ARCH_ARM64, CS_MODE_LITTLE_ENDIAN)
    for insn in md.disasm(image.bytes_at(start, end - start), start):
        print(f"{insn.address:09x}: {insn.mnemonic:8} {insn.op_str}")


def show_vtable(image: Image, address_point: int, slots: list[int]) -> None:
    for slot in slots:
        raw = image.u64(address_point + slot)
        print(
            f"address-point={address_point:#x} slot={slot:#x} "
            f"raw={raw:#018x} target={auth_rebase_target(raw):#x}"
        )


def parse_int(value: str) -> int:
    return int(value, 0)


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--image", type=Path, default=DEFAULT_IMAGE)
    sub = parser.add_subparsers(dest="command", required=True)
    sub.add_parser("verify")
    disasm = sub.add_parser("disasm")
    disasm.add_argument("start", type=parse_int)
    disasm.add_argument("end", type=parse_int)
    vtable = sub.add_parser("vtable")
    vtable.add_argument("address_point", type=parse_int)
    vtable.add_argument("slots", type=parse_int, nargs="+")
    args = parser.parse_args()

    image = Image(args.image)
    if args.command == "verify":
        verify(image)
    elif args.command == "disasm":
        disassemble(image, args.start, args.end)
    else:
        show_vtable(image, args.address_point, args.slots)


if __name__ == "__main__":
    main()
