#!/usr/bin/env python3
"""Ensure the AVE candidate adds only the intended devices to the pinned DT."""
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
            length, name_offset = struct.unpack_from('>2I', raw, block)
            block += 8
            end = raw.index(b'\0', strings + name_offset)
            name = raw[strings + name_offset:end].decode()
            nodes['/'.join(stack) or '/'][name] = raw[block:block + length]
            block = (block + length + 3) & ~3
        elif tag == 9:
            assert not stack
            return nodes
        else:
            assert tag == 4, 'Unexpected flattened-DT token'


def cells(value):
    return struct.unpack('>' + 'I' * (len(value) // 4), value)


parser = argparse.ArgumentParser()
parser.add_argument('baseline', type=Path)
parser.add_argument('candidate', type=Path)
args = parser.parse_args()
baseline, candidate = tree(args.baseline), tree(args.candidate)
old_handles = {cells(props['phandle'])[0]: path for path, props in baseline.items() if 'phandle' in props}
new_handles = {cells(props['phandle'])[0]: path for path, props in candidate.items() if 'phandle' in props}
assert len(new_handles) == sum('phandle' in props for props in candidate.values())
added = {'/soc/iommu@285080000', '/soc/iommu@2850a0000', '/soc/video-codec@285100000'}
assert candidate.keys() - baseline.keys() == added
assert not baseline.keys() - candidate.keys()
changed = []
for path, original in baseline.items():
    for prop in original.keys() | candidate[path].keys():
        if original.get(prop) == candidate[path].get(prop):
            continue
        # dtc assigns the two new DART handles before its final panel handle.
        # Check referenced node identity; do not accept arbitrary cell changes.
        if prop == 'phandle' and path == '/soc/dcp@412e00000/panel':
            assert old_handles[cells(original[prop])[0]] == new_handles[cells(candidate[path][prop])[0]] == path
        elif prop == 'phandle' and path in {
            '/soc/power-management@300700000/power-controller@8008',
            '/soc/power-management@300700000/power-controller@8010',
            '/soc/power-management@300700000/power-controller@8020',
        }:
            assert prop not in original
            assert new_handles[cells(candidate[path][prop])[0]] == path
        elif path == '/chosen/framebuffer@0' and prop in ('panel', 'post-init-providers'):
            assert [old_handles[v] for v in cells(original[prop])] == [new_handles[v] for v in cells(candidate[path][prop])]
        else:
            raise AssertionError((path, prop))
        changed.append(path + '/' + prop)
phandles = new_handles
ave = candidate['/soc/video-codec@285100000']
assert cells(ave['reg']) == (2, 0x85100000, 0, 0x45c000, 2, 0x85050000, 0, 0x4000,
                             2, 0x85800000, 0, 0x800000)
assert ave['reg-names'] == b'codec\0mailbox\0coproc\0'
assert ave['power-domain-names'] == b'dma\0pipe4\0pipe5\0me0\0me1\0'
assert [candidate[phandles[h]]['label'] for h in cells(ave['power-domains'])] == [
    b'venc0_dma\0', b'venc0_pipe4\0', b'venc0_pipe5\0', b'venc0_me0\0', b'venc0_me1\0']
links = cells(ave['iommus'])
assert [phandles[links[0]], links[1], phandles[links[2]], links[3]] == [
    '/soc/iommu@285080000', 0, '/soc/iommu@2850a0000', 0]
for path, low in [('/soc/iommu@285080000', 0x85080000), ('/soc/iommu@2850a0000', 0x850a0000)]:
    node = candidate[path]
    assert cells(node['reg']) == (2, low, 0, 0xc000)
    assert cells(node['interrupts'])[1:] == (955, 4)
    assert cells(node['apple,dma-range']) == (0x100, 0x4000, 0, 0x0fffc000)
print(json.dumps({'passed': True, 'added_devices': sorted(added),
                  'existing_changes': changed, 'baseline_otherwise_unchanged': True}, indent=2))
