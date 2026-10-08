#!/usr/bin/env python3
"""Verify render stamp-width witnesses in the pinned 25G83 firmware."""
import hashlib
import json
from pathlib import Path
import struct
import zipfile
from capstone import Cs, CS_ARCH_ARM64, CS_MODE_ARM

ipsw = Path.home() / 'Library/iTunes/Mac Software Updates/UniversalMac_26.6.2_25G83_Restore.ipsw'
member = 'Firmware/agx/armfw_g17p.im4p'
with zipfile.ZipFile(ipsw) as archive:
    data = archive.read(member)
digest = hashlib.sha256(data).hexdigest()
assert digest == '9b2afdfbf6c3d4b2892a1e4b98766d57a322c95a2a3a535403c50b2dbfa2955b'
marker = data.index(b'rkosftab')
count = struct.unpack_from('<I', data, marker + 8)[0]
records = [struct.unpack_from('<4sIII', data, marker + 16 + i * 16) for i in range(count)]
images = {tag.decode(): data[marker - 32 + off:marker - 32 + off + size]
          for tag, off, size, _ in records if tag in (b'a000', b'b100')}

def instruction(image, address):
    cursor = 32
    assert image[:4] == bytes.fromhex('cffaedfe')
    for _ in range(struct.unpack_from('<I', image, 16)[0]):
        command, length = struct.unpack_from('<II', image, cursor)
        if command == 0x19:
            va, _, off, size = struct.unpack_from('<QQQQ', image, cursor + 24)
            va -= 0xfffffc0000000000
            if va <= address and address + 4 <= va + size:
                raw = image[off + address - va:off + address - va + 4]
                insn, = Cs(CS_ARCH_ARM64, CS_MODE_ARM).disasm(raw, address)
                return raw.hex(), insn.mnemonic + ' ' + insn.op_str
        cursor += length
    raise ValueError('Instruction outside mapped firmware: ' + hex(address))

witnesses = [
    ('a000', 0x1171c, 'ldrh w21, [x19, #4]', 'AddKicks physical QID is u16 at +4'),
    ('a000', 0x11720, 'ldrh w20, [x19, #6]', 'AddKicks kick count is u16 at +6'),
    ('a000', 0x11730, 'ldur x3, [x19, #9]', 'AddKicks full timestamp load for trace at unaligned +9'),
    ('a000', 0xfd18, 'and x0, x5, #0xffffffffff', 'scheduler timestamp occupies bits 0 through 39'),
    ('a000', 0xfd1c, 'ubfx x1, x5, #0x28, #7', 'scheduler QID follows timestamp at bit 40'),
    ('a000', 0xef90, 'add x12, x10, #1', 'advance scheduler timestamp'),
    ('a000', 0xef94, 'and x13, x12, #0xffffffffff', 'wrap scheduler timestamp at 40 bits'),
    ('b100', 0x16e70, 'ldp w22, w23, [x0, #0x10]', 'load u32 completion value and QID'),
    ('b100', 0x16ea0, 'str w22, [x25]', 'publish u32 host completion value after cache maintenance'),
]
verified = []
for tag, address, expected, meaning in witnesses:
    raw, actual = instruction(images[tag], address)
    assert actual == expected, (tag, hex(address), expected, actual)
    verified.append(dict(image=tag, address=hex(address), bytes=raw,
                         instruction=actual, meaning=meaning))
print(json.dumps(dict(member=member, im4p_sha256=digest,
    image_sha256={name: hashlib.sha256(image).hexdigest() for name, image in images.items()},
    verified_witnesses=len(verified), witnesses=verified,
    scope='Static firmware evidence. Does not prove runtime queue lifetime.'), indent=2))
