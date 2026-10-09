#!/usr/bin/env python3
"""Verify APR additions while preserving every pre-existing DT property."""
import argparse
import json
from pathlib import Path
import struct


def tree(path):
    raw = path.read_bytes()
    magic, size, block, strings = struct.unpack_from('>4I', raw)
    assert magic == 0xd00dfeed and size == len(raw)
    nodes, stack = {}, []
    while True:
        tag, = struct.unpack_from('>I', raw, block)
        block += 4
        if tag == 1:
            end = raw.index(b'\0', block)
            stack.append(raw[block:end].decode())
            block = (end + 4) & ~3
            nodes['/'.join(stack) or '/'] = {}
        elif tag == 2:
            stack.pop()
        elif tag == 3:
            length, name = struct.unpack_from('>2I', raw, block)
            block += 8
            end = raw.index(b'\0', strings + name)
            nodes['/'.join(stack) or '/'][raw[strings + name:end].decode()] = raw[block:block + length]
            block = (block + length + 3) & ~3
        elif tag == 9:
            assert not stack
            return nodes
        else:
            assert tag == 4


def cells(raw):
    return struct.unpack('>' + 'I' * (len(raw) // 4), raw)


parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('baseline', type=Path)
parser.add_argument('candidate', type=Path)
args = parser.parse_args()
old, new = tree(args.baseline), tree(args.candidate)
added = {'/soc/iommu@295020000', '/soc/video-codec@295000000'}
assert new.keys() - old.keys() == added and not old.keys() - new.keys()
oldh = {cells(v['phandle'])[0]: k for k, v in old.items() if 'phandle' in v}
newh = {cells(v['phandle'])[0]: k for k, v in new.items() if 'phandle' in v}
assert len(newh) == sum('phandle' in v for v in new.values())
changes = []
for path, props in old.items():
    for prop in props.keys() | new[path].keys():
        a, b = props.get(prop), new[path].get(prop)
        if a == b:
            continue
        if prop == 'phandle':
            assert newh[cells(b)[0]] == path
            if a is not None:
                assert oldh[cells(a)[0]] == path
            else:
                assert new[path].get('label') == b'prores\0'
        elif prop in ('panel', 'post-init-providers'):
            assert [oldh[x] for x in cells(a)] == [newh[x] for x in cells(b)]
        else:
            raise AssertionError((path, prop, a, b))
        changes.append(path + '/' + prop)
apr = new['/soc/video-codec@295000000']
dart = new['/soc/iommu@295020000']
assert cells(apr['reg']) == (2, 0x95000000, 0, 0x4000, 2, 0x94000000, 0, 0x8000)
assert cells(dart['reg']) == (2, 0x95020000, 0, 0x20000)
assert cells(apr['iommus'])[1:] == (0,)
assert newh[cells(apr['iommus'])[0]] == '/soc/iommu@295020000'
for node in (apr, dart):
    assert new[newh[cells(node['power-domains'])[0]]]['label'] == b'prores\0'
assert cells(dart['apple,dma-range']) == (0x100, 0x4000, 0, 0x0fffc000)
print(json.dumps({'passed': True, 'added': sorted(added), 'semantic_handle_changes': changes}, indent=2))
