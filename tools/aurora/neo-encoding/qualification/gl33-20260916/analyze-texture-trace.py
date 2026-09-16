#!/usr/bin/env python3
"""Compare diagnostic texture semantics with submitted Gen9 descriptor bits."""
import json
from pathlib import Path
import re
import sys

run = Path(sys.argv[1])
pattern = re.compile(
    r'asahi: Gen9 texture (\S+) (\d+)x(\d+) levels=(\d+)\.\.(\d+) '
    r'mipmapped=(\d+) address=(0x[0-9a-f]+) secondary=(0x[0-9a-f]+) '
    r'size=(0x[0-9a-f]+) layer-stride=(0x[0-9a-f]+) page-aligned=(\d+) '
    r'extended=(\d+) compressed=(\d+) words:((?: [0-9a-f]{8})+)')
faults = [int(x, 16) for x in re.findall(
    r'halted GMMU[^\n]* va=(0x[0-9a-f]+)', (run/'kernel-after.log').read_text())]
textures = []
for match in pattern.finditer((run/'runner.log').read_text()):
    fmt, width, height, first, last, mip, base, secondary, size, stride, page, ext, compressed, raw = match.groups()
    words = [int(x, 16) for x in raw.split()]
    assert len(words) == 8
    bits = sum(word << (32*i) for i, word in enumerate(words))
    wire_base = ((bits >> 64) & ((1 << 44)-1)) << 4
    wire_secondary = ((bits >> 128) & ((1 << 44)-1)) << 4
    assert wire_base == int(base, 16) and wire_secondary == int(secondary, 16)
    textures.append(dict(
        format=fmt, width=int(width), height=int(height),
        first_level=int(first), last_level=int(last), mipmapped=bool(int(mip)),
        base=base, secondary=secondary, size=size, layer_stride=stride,
        semantic_compressed=bool(int(compressed)),
        wire_dimension=bits & 15,
        wire_depth=((bits >> 110) & 65535)+1,
        wire_compressed=bool((bits >> 59) & 1),
        wire_has_levels=bool((bits >> 58) & 1),
        wire_extended=bool((bits >> 127) & 1),
        wire_page_aligned=bool((bits >> 126) & 1),
        fault_offsets_from_base=[hex(f-int(base, 16)) for f in faults],
        fault_offsets_from_secondary=[hex(f-int(secondary, 16)) for f in faults],
        words=raw.strip()))
report = dict(run=run.name, receipt=json.loads((run/'receipt.json').read_text()),
              fault_addresses=[hex(f) for f in faults], textures=textures,
              compression_mismatches=sum(t['semantic_compressed'] != t['wire_compressed'] for t in textures))
print(json.dumps(report, indent=2))
