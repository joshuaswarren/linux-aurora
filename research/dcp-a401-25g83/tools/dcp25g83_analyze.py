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
        0x041B7444: 0x7100051F,  # reject a newly allocated reply stream
        0x041B7448: 0x540006C0,  # newly allocated stream takes abort path
        0x041B7450: 0x940000A4,  # enter existing stream gate
        0x041B7528: 0x97F9AA7C,  # invalid reply stream firmware abort
        0x041B781C: 0xF9002509,  # str x9, [x8, #0x48]
        0x041B7820: 0x97F9EA9B,  # unconditional condition signal after store
        0x041B552C: 0xB844A268,  # ldur w8, [stream, #0x4a] / remote error
        0x041B5530: 0x35000D88,  # cbnz w8, firmware abort branch
        0x041B56E0: 0xD0001429,  # form abort-path target
        0x041B56F0: 0x17FFFFFA,  # loop after abort call
        0x04021F18: 0xD503237F,  # abort path entry
        0x04021F50: 0x94003C88,  # abort reporting call
        0x04021F58: 0x97FFFFCF,  # non-returning abort call
        0x04021254: 0x52810003,  # rtk_ep_work stack size 0x800
        0x04021258: 0x52800724,  # rtk_ep_work priority 0x39
        0x042DF464: 0x52820003,  # iomfb_ap_link stack size 0x1000
        0x042DF468: 0x52800204,  # iomfb_ap_link priority 0x10
        0x04141324: 0xD503237F,  # getRequestedDriveSettings entry / PACIBSP
        0x042A13D0: 0x2A1303E6,  # saved LR / mov w6, w19, not a callsite
        0x040205C4: 0xDAC0116B,  # scheduler CLZ of runnable-priority bitmap
        0x040205CC: 0x4B0B018B,  # highest runnable priority = 63 - CLZ
        0x041B7068: 0x71080A7F,  # filter returns true only for new request 0x202
        0x041B510C: 0xB8060E7F,  # synchronous cleanup clears stream busy word
        0x041B786C: 0xB940614C,  # reply lookup reads stream busy word
        0x041B7870: 0x350001AC,  # existing busy stream skips allocation marker
        0x042AE5D8: 0x97FEE72B,  # PanelLayout D102 synchronous call
        0x042AE5FC: 0x97FEE722,  # SupportRelbufInfoCb=true D102 call
        0x042AE624: 0x97FEE718,  # SupportRelbufInfoCb=false D102 call
        0x042B5834: 0x97FFE312,  # call property-publishing tail
        0x042B583C: 0x97FFE2BD,  # call post-property no-op
        0x042B58B4: 0x97FC088E,  # D000 synchronous call
        0x042B58CC: 0x97FC0888,  # D001 synchronous call
        0x04026C7C: 0x9400003A,  # record the lower-mailbox RX entry
        0x04026CC8: 0xD63F091F,  # invoke route/filter before endpoint publish
        0x04026CF8: 0x97FFFC82,  # publish raw word to endpoint software queue
        0x040342D0: 0xD374DF88,  # management consumer selects type bits 55:52
        0x040343BC: 0xB9400A60,  # type-3 echo loads management endpoint number
        0x040343C0: 0xD2E00801,  # construct type-4 response
        0x040343C4: 0xB3407F81,  # preserve the request's low 32-bit cookie
        0x040343CC: 0x97FFCADB,  # send the stateless type-4 echo response
        0x041B705C: 0x97F9EC9E,  # latch the EP37 link event
        0x040322E4: 0x39000409,  # event source pending byte = 1
        0x04031C6C: 0x39400788,  # workloop tests source pending byte
        0x04031C7C: 0x3900079F,  # workloop clears pending before action
        0x04031C94: 0xD63F091F,  # invoke event-source action
        0x04031CCC: 0x5280003B,  # nonzero action requests another pass
        0x04031CD0: 0x3900079B,  # nonzero action rearms pending byte
        0x04031CF0: 0x3707FADB,  # repeat the source scan when requested
        0x041B710C: 0x97F9BFFE,  # pop the next EP37 software-queue word
        0x041B71C4: 0xAA1303E0,  # type-1 READY selects normal handler
        0x041B71CC: 0x94000129,  # READY calls response/duplicate handler
        0x0403207C: 0xD63F0ABF,  # gate helper unconditionally calls action
        0x0403208C: 0x2A1303E0,  # gate helper returns the action result
        0x041B7720: 0x97F9EA4E,  # enter existing stream gate
        0x041B7804: 0xD503237F,  # reply store-and-signal action entry
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
