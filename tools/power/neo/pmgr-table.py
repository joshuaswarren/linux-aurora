#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-2.0-only
"""Decode retained J700 PMGR ADT properties; never access hardware.

Layout/ID selection/virtual flag follow m1n1 src/pmgr.c. J700's ps-groups
select a register range and byte offset. The two expected physical windows
are guarded here; this is not a generic ADT address translator.
"""
import argparse
import hashlib
import json
from pathlib import Path
import struct


def decode(properties):
    raw = bytes.fromhex(properties['devices'])
    groups = list(struct.iter_unpack('<III', bytes.fromhex(properties['ps-groups'])))
    regs = list(struct.iter_unpack('<QQ', bytes.fromhex(properties['reg'])))
    assert len(raw) % 48 == 0
    rows = [raw[i:i + 48] for i in range(0, len(raw), 48)]
    assert rows[0][3] == rows[1][3]  # J700 uses 16-bit id2, not id1.
    by_id = {struct.unpack_from('<H', r, 26)[0]: r[32:48].split(b'\0')[0].decode('ascii')
             for r in rows}
    assert len(by_id) == len(rows) and len(set(by_id.values())) == len(rows)
    table = []
    for index, row in enumerate(rows):
        device_id = struct.unpack_from('<H', row, 26)[0]
        reg_index, reg_offset, _ = groups[row[19]]
        bus_address, size = regs[reg_index]
        base = bus_address + 0x210000000 + reg_offset
        assert base in (0x300700000, 0x308280000)
        offset = int.from_bytes(row[16:19], 'little')
        assert offset % 4 == 0 and reg_offset + offset + 4 <= size
        table.append({'name': by_id[device_id], 'base': hex(base), 'offset': offset,
                      'parents': [by_id[n] for n in struct.unpack_from('<HH', row, 4) if n],
                      'flags': row[0], 'virtual': bool(row[0] & 0x10),
                      'adt_index': index + 1, 'device_id': device_id})
    # Reject a broken ID join rather than trusting it for children-first writes.
    by_name = {d['name']: d for d in table}
    visited, active = set(), set()
    def visit(name):
        if name in active:
            raise ValueError('cycle in PMGR parent graph at ' + name)
        if name in visited:
            return
        active.add(name)
        for parent in by_name[name]['parents']:
            visit(parent)
        active.remove(name)
        visited.add(name)
    for name in by_name:
        visit(name)
    return table


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument('adt', type=Path)
    ap.add_argument('--output', type=Path, required=True)
    args = ap.parse_args()
    table = decode(json.loads(args.adt.read_text()))
    args.output.write_text(json.dumps(table, indent=1) + '\n')
    print(json.dumps({'source_sha256': hashlib.sha256(args.adt.read_bytes()).hexdigest(),
                      'entries': len(table), 'virtual': sum(d['virtual'] for d in table),
                      'physical': sum(not d['virtual'] for d in table)}))


if __name__ == '__main__':
    main()
