#!/usr/bin/env python3
"""Emit an apply_patch input from validated offline native MRT captures."""
from pathlib import Path
import hashlib
import json
import struct
import difflib

base = Path(__file__).resolve().parent
parts = ['/* SPDX-License-Identifier: MIT */',
         '/* Native G17P EOT texts and metadata captured with MetalToolchain',
         ' * v27.1.5194.15. See compiler/test/probes/README-g17p-wide-mrt.md.',
         ' * Every attachment stays in physical imageblock partition zero.',
         ' * EOT partitions are offset/sample-stride quotients, distinct from',
         ' * fragment pixel synchronization classes. Active PBEs are packed. */',
         '#ifndef AGX_G17P_WIDE_MRT_EOT_H', '#define AGX_G17P_WIDE_MRT_EOT_H',
         '#include <stdbool.h>', '#include <stdint.h>', '']
entries = []


def field(data, table, index):
    vtable = table - struct.unpack_from('<i', data, table)[0]
    size = struct.unpack_from('<H', data, vtable)[0]
    if 4 + 2 * index >= size:
        return None
    offset = struct.unpack_from('<H', data, vtable + 4 + 2 * index)[0]
    return table + offset if offset else None


def u32(data, offset):
    return struct.unpack_from('<I', data, offset)[0]


for count in range(1, 9):
    name = (f'eot-{count}-physical-partition-zero' if count in (5, 6, 7)
            else f'eot-{count}-shared')
    directory = base/'native-mrt-witness'/name
    receipt = json.loads((directory/'receipt.json').read_text())
    assert receipt['exit_code'] == 0 and not receipt['hardware_execution']
    text = next(s for s in receipt['sections'] if s['name'] == '__text')
    code = (directory/text['file']).read_bytes()
    assert len(code) == text['size'] and hashlib.sha256(code).hexdigest() == text['sha256']
    meta = next(s for s in receipt['sections'] if s['segment'] == '__GPU_METADATA')
    data = (directory/meta['file']).read_bytes()
    pointer = field(data, u32(data, 0), 0)
    common = pointer + u32(data, pointer)
    assert u32(data, field(data, common, 0)) == 3
    staging = u32(data, field(data, common, 38))
    assert staging == u32(data, field(data, common, 42)) and staging % 2 == 0
    # Native ESL copies a qword pointer, then 2*N-1 dwords starting at u4.
    pointer = field(data, common, 2)
    vector = pointer + u32(data, pointer)
    assert u32(data, vector) == 2
    entry = vector + 8
    esl = entry + u32(data, entry)
    assert u32(data, field(data, esl, 2)) == 2 * count - 1
    assert u32(data, field(data, esl, 3)) == 2
    # Each attachment supplies a byte offset in every other argument dword.
    pointer = field(data, common, 10)
    vector = pointer + u32(data, pointer)
    assert u32(data, vector) == count
    for index in range(count):
        entry = vector + 4 + 4 * index
        arg = entry + u32(data, entry)
        assert data[field(data, arg, 0)] == 5
        for member, expected in ((1, index), (2, 2 * index)):
            location = field(data, arg, member)
            assert (u32(data, location) if location else 0) == expected
    parts += [f'/* text SHA256 {text["sha256"]} */',
              f'static const uint8_t agx_g17p_wide_mrt_eot_{count}[] = {{']
    parts += ['   '+','.join(f'0x{b:02x}' for b in code[i:i+12])+',' for i in range(0,len(code),12)]
    parts += ['};', '']
    entries.append((count, staging // 2))

parts += ['struct agx_g17p_wide_mrt_eot {', '   const uint8_t *code;',
          '   uint16_t size;', '   uint16_t staging_halfs;', '};', '',
          'static inline bool', 'agx_g17p_wide_mrt_eot_get(unsigned count, struct agx_g17p_wide_mrt_eot *out)',
          '{', '   if (!out)', '      return false;', '   switch (count) {']
for count, staging in entries:
    parts += [f'   case {count}:',
              f'      *out = (struct agx_g17p_wide_mrt_eot){{agx_g17p_wide_mrt_eot_{count},',
              f'         sizeof(agx_g17p_wide_mrt_eot_{count}), {staging}}};',
              '      return true;']
parts += ['   default:', '      return false;', '   }', '}', '#endif']
target = base.parents[2]/'mesa-j700/src/asahi/lib/agx_g17p_wide_mrt_eot.h'
old = target.read_text()
new = '\n'.join(parts) + '\n'
print('*** Begin Patch\n*** Update File: ' + str(target))
for line in list(difflib.unified_diff(old.splitlines(True), new.splitlines(True), n=3))[2:]:
    print('@@' if line.startswith('@@') else line.rstrip('\n'))
print('*** End Patch')
