#!/usr/bin/env python3
"""Check that JPEG nodes and their power reference are the only DT changes."""
import argparse
import ast
import json
from pathlib import Path
import struct

# Reuse the already qualified read-only DT parser without running its CLI.
source = Path(__file__).parents[1] / 'linux-prores/check-dtb.py'
parsed = ast.parse(source.read_text())
for function in parsed.body:
    if isinstance(function, ast.FunctionDef) and function.name in ('tree', 'cells'):
        exec(compile(ast.Module(body=[function], type_ignores=[]), str(source), 'exec'))

p = argparse.ArgumentParser(description=__doc__)
p.add_argument('baseline', type=Path); p.add_argument('candidate', type=Path)
a = p.parse_args(); old, new = tree(a.baseline), tree(a.candidate)
added = {'/soc/iommu@28d020000', '/soc/image-codec@28d000000'}
assert new.keys() - old.keys() == added and not old.keys() - new.keys()
oldh = {cells(v['phandle'])[0]: k for k,v in old.items() if 'phandle' in v}
newh = {cells(v['phandle'])[0]: k for k,v in new.items() if 'phandle' in v}
assert len(newh) == sum('phandle' in v for v in new.values())
changes = []
for path, props in old.items():
    for prop in props.keys() | new[path].keys():
        before, after = props.get(prop), new[path].get(prop)
        if before == after: continue
        if prop == 'phandle':
            assert newh[cells(after)[0]] == path
            if before is not None: assert oldh[cells(before)[0]] == path
            else: assert new[path].get('label') == b'jpg\0'
        elif prop in ('panel', 'post-init-providers'):
            assert [oldh[x] for x in cells(before)] == [newh[x] for x in cells(after)]
        else: raise AssertionError((path, prop, before, after))
        changes.append(path+'/'+prop)
jpeg = new['/soc/image-codec@28d000000']; dart = new['/soc/iommu@28d020000']
assert cells(jpeg['reg']) == (2, 0x8d000000, 0, 0x4000)
assert cells(dart['reg']) == (2, 0x8d020000, 0, 0xc000)
assert cells(jpeg['iommus'])[1:] == (0,)
assert newh[cells(jpeg['iommus'])[0]] == '/soc/iommu@28d020000'
for node in (jpeg, dart):
    assert new[newh[cells(node['power-domains'])[0]]]['label'] == b'jpg\0'
assert cells(jpeg['interrupts']) == (0, 1040, 4)
assert cells(dart['interrupts']) == (0, 1041, 4)
assert cells(dart['apple,dma-range']) == (0x100, 0x4000, 0, 0x0fffc000)
print(json.dumps({'passed':True,'added':sorted(added),'semantic_handle_changes':changes},indent=2))
