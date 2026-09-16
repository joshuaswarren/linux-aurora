#!/usr/bin/env python3
"""Extract independent native sparse transaction oracles and reproducible inputs."""
from pathlib import Path
import hashlib
import json
import struct

base = Path(__file__).resolve().parent
dest = base.parents[2] / 'mesa-j700/src/asahi/compiler/test/probes'
rows = []
files = {}
for mask in (0x04, 0x05, 0x0a, 0xfc, 0xfd, 0xfe):
    name = f'fragment-8-active-mask{mask:02x}' if mask != 0xfd else 'fragment-8-missing-attachment1'
    directory = base/'native-mrt-witness'/name
    receipt = json.loads((directory/'receipt.json').read_text())
    assert receipt['exit_code'] == 0 and not receipt['hardware_execution']
    candidates = []
    for section in receipt['sections']:
        raw = (directory/section['file']).read_bytes()
        assert hashlib.sha256(raw).hexdigest() == section['sha256']
        if section['name'] == '__text':
            gates = b''.join(raw[i:i+6] for i in range(len(raw)-5)
                             if raw[i] in (7, 135) and raw[i+1:i+3] == b'\x02\x54')
            offsets = [2*raw[i+5] for i in range(len(raw)-11)
                       if raw[i:i+3] == b'\xe7\x06\x54']
            if gates:
                candidates.append((section, gates, offsets))
    assert len(candidates) == 1
    section, gates, offsets = candidates[0]
    md = next(s for s in receipt['sections'] if s['object_offset'] == section['object_offset'] and s['segment'] == '__GPU_METADATA_2')
    data = (directory/md['file']).read_bytes()
    table = struct.unpack_from('<I', data, 0)[0]
    vtable = table - struct.unpack_from('<i', data, table)[0]
    field = struct.unpack_from('<H', data, vtable+6)[0]
    release = struct.unpack_from('<I', data, table+field)[0]
    partition_mask = sum(1 << p for p in range(3) if not release & (12 << (2*p)))
    assert len(offsets) == mask.bit_count() and len(gates) <= 48
    values = ','.join(f'0x{x:02x}' for x in gates)
    rows.append(f'   /* {name}: text SHA256 {section["sha256"]} */\n'
                f'   {{0x{mask:02x}, 0x{partition_mask:x}, 0x{release:x}, {len(gates)}, {len(offsets)},\n'
                f'    {{{values}}}, {{{",".join(map(str, offsets))}}}}},')
    for filename in ('input.metal', 'pipeline.mtlp-json'):
        files[dest/'g17p-sparse-mrt'/name/filename] = (directory/filename).read_text()
header = '''/* SPDX-License-Identifier: MIT
 * Native applegpu_g17p sparse attachment transaction oracles.
 * Generated from metal-nt captures; inputs are in g17p-sparse-mrt/.
 * Compare synchronization and tile offsets independently of register allocation.
 */
#pragma once
#include <stdint.h>

static const struct {
   uint8_t target_mask;
   uint8_t partition_mask;
   uint16_t loader_release;
   uint8_t gate_size;
   uint8_t store_count;
   uint8_t gates[48];
   uint8_t offsets_B[8];
} g17p_sparse_mrt_oracles[] = {
''' + '\n'.join(rows) + '\n};\n'
files[dest/'g17p-sparse-mrt.h'] = header
print('*** Begin Patch')
for path, contents in files.items():
    assert not path.exists(), path
    print('*** Add File: ' + str(path))
    for line in contents.splitlines():
        print('+' + line)
print('*** End Patch')
