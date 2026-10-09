#!/usr/bin/env python3
"""Verify the diagnostic GPU DT retains the qualified M5 peripheral topology."""
import argparse
import json
from pathlib import Path
import runpy

helpers = runpy.run_path(str(Path(__file__).resolve().parents[1] / 'video/check-m5-dtb.py'))
read_tree, words, handles = (helpers[n] for n in ('read_tree', 'words', 'handles'))


def normalized(key, value, tree, refs):
    if key == 'phandle':
        return refs[words(value)[0]]
    if key == 'iommu-map':
        cells = words(value)
        return [(cells[i], refs[cells[i + 1]], cells[i + 2], cells[i + 3])
                for i in range(0, len(cells), 4)]
    if key == 'interrupt-parent':
        return refs[words(value)[0]]
    cells_property = {'power-domains': '#power-domain-cells', 'mboxes': '#mbox-cells',
                      'iommus': '#iommu-cells', 'clocks': '#clock-cells',
                      'resets': '#reset-cells', 'phys': '#phy-cells',
                      'interrupts-extended': '#interrupt-cells'}.get(key)
    if cells_property:
        cells, result, i = words(value), [], 0
        while i < len(cells):
            node = refs[cells[i]]
            count, = words(tree[node][cells_property])
            result.append((node, cells[i + 1:i + 1 + count]))
            i += count + 1
        assert i == len(cells), (key, 'truncated provider arguments')
        return result
    return value


def check(base_path, candidate_path):
    base, candidate = read_tree(base_path), read_tree(candidate_path)
    before, after = handles(base), handles(candidate)
    for node, properties in base.items():
        assert node in candidate, ('removed node', node)
        for key, value in properties.items():
            assert key in candidate[node], ('removed property', node, key)
            assert normalized(key, value, base, before) == normalized(
                key, candidate[node][key], candidate, after), ('changed property', node, key)
        assert set(candidate[node]) - set(properties) == ({'gpu'} if node == '/aliases' else set()), (
            'unexpected property additions', node)

    pmgr = '/soc/power-management@580e80000'
    gpu = '/soc/gpu@582600000'
    memory = {'uat-ttbs', 'uat-pagetables', 'uat-l2-pagetables', 'uat-handoff', 'gfx-data', 'gfx1-data'}
    expected = {pmgr, pmgr + '/power-controller@0', pmgr + '/power-controller@110', gpu,
                '/soc/mailbox@582600000', '/soc/mailbox@582e00000'}
    expected |= {'/reserved-memory/' + n for n in memory}
    assert set(candidate) - set(base) == expected, 'unexpected added nodes'
    assert candidate['/aliases']['gpu'] == gpu.encode() + b'\0'
    assert candidate[gpu]['compatible'] == b'apple,agx-t8142\0'
    assert words(candidate[gpu]['reg']) == (5, 0x82600000, 0, 0x88000, 5, 0x80000000, 0, 0x4000000)
    assert words(candidate[pmgr]['reg']) == (5, 0x80e80000, 0, 0x30000)
    for name, offset in [('gpx', 0), ('gfx', 0x110)]:
        node = pmgr + '/power-controller@' + format(offset, 'x')
        assert words(candidate[node]['reg']) == (offset, 4)
        assert candidate[node]['label'] == name.encode() + b'\0'
    assert after[words(candidate[gpu]['power-domains'])[0]] == pmgr + '/power-controller@110'
    assert after[words(candidate[pmgr + '/power-controller@110']['power-domains'])[0]] == pmgr + '/power-controller@0'
    assert [after[p] for p in words(candidate[gpu]['memory-region'])] == [
        '/reserved-memory/' + n for n in ('uat-ttbs', 'uat-pagetables', 'uat-handoff',
                                          'uat-l2-pagetables', 'gfx-data', 'gfx1-data')]
    assert [after[p] for p in words(candidate[gpu]['mboxes'])] == [
        '/soc/mailbox@582600000', '/soc/mailbox@582e00000']
    for asc, vbar, irqs, role in [(0x82600000, 0x82050000, (1123, 1126), 'GFX'),
                                (0x82e00000, 0x82850000, (1127, 1130), 'GFX1')]:
        node = candidate['/soc/mailbox@5' + format(asc, '08x')]
        assert node['status'] == b'disabled\0', 'M5 firmware runtime is not admitted yet'
        assert node['compatible'] == b'apple,t8142-ascwrap-v6\0'
        assert words(node['reg']) == (5, asc, 0, 0x88000, 5, vbar, 0, 8)
        assert words(node['interrupts']) == (0, irqs[0], 4, 0, irqs[1], 4)
        assert node['apple,firmware-role'] == role.encode() + b'\0'
    for name in memory:
        assert words(candidate['/reserved-memory/' + name]['reg']) == (0, 0, 0, 0), 'boot-local addresses required'
    return dict(passed=True, qualified_devices_preserved=True, measured_gpu_resources_match=True,
                firmware_runtime_enabled=False, added_nodes=len(expected))


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('base', type=Path)
    parser.add_argument('candidate', type=Path)
    args = parser.parse_args()
    print(json.dumps(check(args.base, args.candidate), sort_keys=True))
