#!/usr/bin/env python3
"""Check the AVE extension against the qualified J813 input device tree."""
import argparse
import json
from pathlib import Path
import struct


def read_tree(path):
    data = path.read_bytes()
    header = struct.unpack_from('>10I', data)
    if header[0] != 0xd00dfeed or header[1] > len(data):
        raise ValueError('Invalid FDT header')
    position, strings = header[2:4]
    stack, nodes = [], {}
    while True:
        token = struct.unpack_from('>I', data, position)[0]
        position += 4
        if token == 1:
            end = data.index(b'\0', position)
            stack.append(data[position:end].decode())
            position = (end + 4) & ~3
            nodes['/' + '/'.join(stack).strip('/')] = {}
        elif token == 2:
            stack.pop()
        elif token == 3:
            size, name_offset = struct.unpack_from('>II', data, position)
            position += 8
            end = data.index(b'\0', strings + name_offset)
            name = data[strings + name_offset:end].decode()
            nodes['/' + '/'.join(stack).strip('/')][name] = data[position:position + size]
            position = (position + size + 3) & ~3
        elif token == 4:
            continue
        elif token == 9:
            break
        else:
            raise ValueError('Unexpected FDT token')
    return nodes


def words(data):
    return struct.unpack('>' + str(len(data) // 4) + 'I', data)


def handles(tree):
    result = {}
    for node, props in tree.items():
        if 'phandle' in props:
            handle, = words(props['phandle'])
            if handle in result:
                raise ValueError('Duplicate phandle')
            result[handle] = node
    return result


def normalized(key, value, phandles):
    if key == 'phandle':
        return phandles[words(value)[0]]
    if key == 'iommu-map':
        cells = words(value)
        if len(cells) % 4:
            raise ValueError('Expected the qualified one-cell IOMMU map format')
        return [(cells[i], phandles[cells[i + 1]], cells[i + 2], cells[i + 3])
                for i in range(0, len(cells), 4)]
    return value


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('base', type=Path)
    parser.add_argument('candidate', type=Path)
    args = parser.parse_args()
    base, candidate = read_tree(args.base), read_tree(args.candidate)
    old_handles, new_handles = handles(base), handles(candidate)
    changed = [(node, key) for node, props in base.items() for key, value in props.items()
               if key not in candidate.get(node, {}) or
               normalized(key, value, old_handles) !=
               normalized(key, candidate[node][key], new_handles)]
    if changed:
        raise ValueError('Qualified DT properties changed: ' + repr(changed))
    expected_added = {'/soc/video-codec@281100000', '/soc/iommu@281080000',
                      '/soc/iommu@2810a0000', '/soc/power-management@380700000'}
    expected_added.update('/soc/power-management@380700000/power-controller@' + value
                          for value in ['298', '8000', '8008', '8010', '8018', '8020'])
    if set(candidate) - set(base) != expected_added:
        raise ValueError('Unexpected added device nodes')
    resources = {
        '/soc/video-codec@281100000': (2, 0x81100000, 0, 0x45c000,
                                     2, 0x81050000, 0, 0x4000,
                                     2, 0x81800000, 0, 0x800000),
        '/soc/iommu@281080000': (2, 0x81080000, 0, 0xc000),
        '/soc/iommu@2810a0000': (2, 0x810a0000, 0, 0xc000),
    }
    for node, expected in resources.items():
        if words(candidate[node]['reg']) != expected:
            raise ValueError('M5 resource mismatch: ' + node)
    print(json.dumps(dict(passed=True, qualified_devices_preserved=True,
                          phandle_references_preserved=True,
                          measured_ave_resources_match=True,
                          added_nodes=sorted(expected_added)), indent=2))


if __name__ == '__main__':
    main()
