#!/usr/bin/env python3
"""Offline compiler counterexamples for the mixed EOT partition audit."""
import runpy
import struct
import sys
import json
from pathlib import Path

probe = runpy.run_path(str(Path(__file__).with_name('native-mrt-probe.py')))
remaining = sys.argv[1:] == ['--recover-original-format']
for internal_format in (range(3, 16) if remaining else (0, 1, 2)):
    for second_partition in ((1, 0) if remaining else (0, 1)):
        data = bytearray(probe['builtin_input'](2, 3))
        for index in range(2):
            entry = 88 + 4 * index
            table = entry + struct.unpack_from('<I', data, entry)[0]
            data[table + 4] = second_partition if index else 0
            data[table + 5] = 3 if index else internal_format
            data[table + 9] = 1 if index else 4
        name = f'eot-mixed-format{internal_format}-partition{second_partition}'
        probe['compile_capture'](name, None, bytes(data), True)
        if remaining and second_partition == 1:
            receipt = json.loads((probe['OUT'] / name / 'receipt.json').read_text())
            text = next((s for s in receipt.get('sections', [])
                         if s['name'] == '__text'), {})
            if text.get('sha256') != 'eef32d3c9b7bca4de2efba861662e32c34f49af5877dabce0ad9da5cd50d5173':
                break
    else:
        if remaining:
            print(f'Original mixed EOT reproduced exactly with format {internal_format}')
            break
