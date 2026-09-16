#!/usr/bin/env python3
"""Emit source edits for explicit colour target and partition mask metadata."""
from pathlib import Path
import difflib
import re

root = Path(__file__).resolve().parents[3] / 'mesa-j700'
changes = {}
for path in list((root/'src/asahi').rglob('*')) + list((root/'src/gallium/drivers/asahi').rglob('*')):
    if path.suffix not in ('.c', '.h', '.cpp'):
        continue
    old = path.read_text()
    new = old.replace('colour_partitions', 'colour_partition_mask')
    if path.name == 'test-agx3-pack-binary.cpp':
        pattern = r'(agx_(?:ld|st)_tile(?:_to)?\([^;]+?)(\);)'
        def add_target(m):
            target = 'rt' if '4 * rt' in m[1] else '0'
            return m[1] + ', ' + target + m[2]
        new, count = re.subn(pattern, add_target, new)
        assert count == 31, count
        new = new.replace('colour_partition_mask, 3)', 'colour_partition_mask, 7)')
    if new != old:
        changes[path] = (old, new)
print('*** Begin Patch')
for path, (old, new) in changes.items():
    print('*** Update File: ' + str(path))
    diff = list(difflib.unified_diff(old.splitlines(True), new.splitlines(True), n=3))
    for line in diff[2:]:
        print('@@' if line.startswith('@@') else line.rstrip('\n'))
print('*** End Patch')
