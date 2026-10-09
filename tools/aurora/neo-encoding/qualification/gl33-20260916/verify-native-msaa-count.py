#!/usr/bin/env python3
"""Recheck the native sample-count copy path without accessing hardware."""
import hashlib
import json
from pathlib import Path
import struct

here = Path(__file__).resolve().parent
path = here.parents[2] / "neo-update/kernel/25G83/kernelcache.release.mac17p.decompressed"
data = path.read_bytes()
sha = hashlib.sha256(data).hexdigest()
assert sha == "b1ea68d23c452099da9fbfe06b19634e0175d4b2b1194f7249abac430a9a7a0d"

def commands(base):
    assert struct.unpack_from("<I", data, base)[0] == 0xfeedfacf
    offset = base + 32
    for _ in range(struct.unpack_from("<I", data, base + 16)[0]):
        command, size = struct.unpack_from("<II", data, offset)
        yield command, offset, size
        offset += size

entry = None
for command, offset, size in commands(0):
    if command == 0x80000035:
        va, file_offset, name_offset, _ = struct.unpack_from("<QQII", data, offset + 8)
        name = data[offset + name_offset:offset + size].split(b"\0")[0]
        if name == b"com.apple.AGXG17P":
            entry = file_offset
assert entry is not None
segments = [struct.unpack_from("<QQQQ", data, offset + 24)
            for command, offset, size in commands(entry) if command == 0x19]
blocks = [
    ("Select sample positions for 1/2/4/8 samples", 0xfffffe00088c4ca8,
     "6c4a40b99f0d0071cc0000549f050071200600549f09007180050054320000149f110071600300549f210071c1050054"),
    ("Store count in render target +0xb8", 0xfffffe00088c4e10,
     "6c2a17297f5a00f99f0d0071cc0000549f050071200600549f09007180050054"),
    ("Copy target count to host +0x8c8", 0xfffffe0008839d78,
     "881240f9686204f98826572968ca08b9885a40f9685e04f969d208b9"),
    ("Copy host count to firmware record +0x50", 0xfffffe0008837af4,
     "095b00b989ca48b98a5e44f9082b04a988d248b909230a29"),
]
verified = []
for meaning, va, expected in blocks:
    expected = bytes.fromhex(expected)
    vm, size, offset, file_size = next(s for s in segments if s[0] <= va and va + len(expected) <= s[0] + s[3])
    offset += va - vm
    assert data[offset:offset + len(expected)] == expected, meaning
    verified.append({"meaning": meaning, "va": hex(va), "bytes": expected.hex()})
receipt = {"kernel_sha256": sha, "fileset_entry": "com.apple.AGXG17P", "verified": verified}
(here / "native-msaa-count-receipt.json").write_text(json.dumps(receipt, indent=2) + "\n")
print("NATIVE_MSAA_COUNT_COPY_PATH_VERIFIED", len(verified))
