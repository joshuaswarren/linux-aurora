#!/usr/bin/env python3
"""Read-only witnesses for G17P B1 render completion in the saved 25G83 IPSW.

Requires capstone. Does not extract files, contact hardware, or patch firmware.
This verifies instruction witnesses; it does not prove the Linux fix at runtime.
"""
import argparse
import hashlib
import json
import struct
import zipfile
from pathlib import Path

from capstone import Cs, CS_ARCH_ARM64, CS_MODE_ARM

MEMBER = "Firmware/agx/armfw_g17p.im4p"
IM4P_SHA256 = "9b2afdfbf6c3d4b2892a1e4b98766d57a322c95a2a3a535403c50b2dbfa2955b"
IMAGE_BASE = 0xFFFFFC0000000000


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("ipsw", nargs="?", type=Path, default=Path.home() /
                        "Library/iTunes/Mac Software Updates/UniversalMac_26.6.2_25G83_Restore.ipsw")
    args = parser.parse_args()
    with zipfile.ZipFile(args.ipsw) as archive:
        data = archive.read(MEMBER)
    digest = hashlib.sha256(data).hexdigest()
    if digest != IM4P_SHA256:
        raise ValueError("Firmware differs from the audited 25G83 image")

    marker = data.index(b"rkosftab")
    payload = marker - 0x20
    count = struct.unpack_from("<I", data, marker + 8)[0]
    records = [struct.unpack_from("<4sIII", data, marker + 16 + i * 16)
               for i in range(count)]
    matches = [(offset, size) for tag, offset, size, _ in records if tag == b"b100"]
    if len(matches) != 1:
        raise ValueError("Expected exactly one B1 firmware image")
    offset, size = matches[0]
    macho = data[payload + offset:payload + offset + size]
    if macho[:4] != bytes.fromhex("cffaedfe"):
        raise ValueError("B1 image is not a little-endian 64-bit Mach-O")

    segments = []
    cursor = 32
    for _ in range(struct.unpack_from("<I", macho, 16)[0]):
        command, length = struct.unpack_from("<II", macho, cursor)
        if command == 0x19:
            va, _, file_offset, file_size = struct.unpack_from("<QQQQ", macho, cursor + 24)
            segments.append((va - IMAGE_BASE, file_offset, file_size))
        cursor += length

    def code_at(address, length):
        for va, file_offset, file_size in segments:
            if va <= address and address + length <= va + file_size:
                start = file_offset + address - va
                return macho[start:start + length]
        raise ValueError(f"Unmapped instruction range: {address:#x}")

    disassembler = Cs(CS_ARCH_ARM64, CS_MODE_ARM)
    witnesses = [
        (0xC9F4, "mov", "w21, #0x2140", "fragment descriptor stamp-info offset"),
        (0xCA2C, "add", "x21, x26, x21", "fragment descriptor stamp-info address"),
        (0xCF88, "mov", "x28, x21", "retain fragment stamp-info address"),
        (0xD218, "str", "x28, [x9, #0x50]", "queue fragment stamp in EventControl bank 1"),
        (0x16B48, "str", "wzr, [x21, #0x94]", "clear EventControl membership before cache maintenance"),
        (0x16B90, "str", "x9, [x8, x12]", "first cache-maintenance command to SGX+0x18030"),
        (0x16BA0, "ldr", "x12, [x8, x13]", "read SGX+0x18038 completion status"),
        (0x16BA4, "tbz", "w12, #1, #0x16bb8", "wait for first cache-maintenance completion"),
        (0x16BCC, "str", "x10, [x8, x11]", "second cache-maintenance command"),
        (0x16BD4, "tbz", "w10, #1, #0x16be8", "wait for second cache-maintenance completion"),
        (0x16E6C, "ldp", "x25, x21, [x0]", "load fragment stamp A/B addresses"),
        (0x16E70, "ldp", "w22, w23, [x0, #0x10]", "load fragment stamp value and physical QID"),
        (0x16EA0, "str", "w22, [x25]", "publish fragment stamp A after maintenance"),
        (0x16EB0, "mov", "w0, #0xc", "fragment stamp KTrace event"),
        (0x16F18, "str", "w22, [x25]", "publish compute stamp A after maintenance"),
        (0x16F28, "mov", "w0, #0xe", "compute stamp KTrace event"),
        (0x16F58, "bl", "#0x20edc", "publish accumulated completion notification"),
    ]
    results = []
    for address, mnemonic, operands, meaning in witnesses:
        instruction, = disassembler.disasm(code_at(address, 4), address)
        actual = (instruction.mnemonic, instruction.op_str)
        if actual != (mnemonic, operands):
            raise ValueError(f"Unexpected instruction at {address:#x}: {actual}")
        results.append({"offset": hex(address), "instruction": f"{mnemonic} {operands}",
                        "meaning": meaning})
    print(json.dumps({"member": MEMBER, "sha256": digest, "b1_sha256": hashlib.sha256(macho).hexdigest(),
                      "verified_witnesses": len(results), "witnesses": results}, indent=2))


if __name__ == "__main__":
    main()
