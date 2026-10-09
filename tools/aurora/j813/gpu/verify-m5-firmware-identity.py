#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""Check private cold captures and exercise the M5 TEXT identity boundary."""
import argparse
import json
from pathlib import Path

from m5_firmware_identity import PROFILES, qualify_text

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('captures', type=Path, nargs='+')
args = parser.parse_args()
matched = rejected = 0
for capture in args.captures:
    for role, (_, offsets, _) in PROFILES.items():
        payload = (capture / (role + '-text.bin')).read_bytes()
        qualify_text(role, payload)
        matched += 1
        for offset in (0x100, offsets[0], offsets[0] + 4, offsets[1] + 4,
                       len(payload) - 1):
            changed = bytearray(payload)
            changed[offset] ^= 1
            try:
                qualify_text(role, changed)
            except ValueError:
                rejected += 1
            else:
                raise AssertionError('Accepted changed code, metadata tag/length or tail')
        for offset in offsets:
            changed = bytearray(payload)
            changed[offset + 8:offset + 16] = bytes(range(8))
            qualify_text(role, changed)
        for invalid_role in ('unknown', 'gfx1-asc' if role == 'gfx-asc' else 'gfx-asc'):
            try:
                qualify_text(invalid_role, payload)
            except ValueError:
                rejected += 1
            else:
                raise AssertionError('Accepted mismatched firmware role')
print(json.dumps(dict(matched_role_captures=matched, rejected_mutations=rejected,
                      executable_bytes_excluded=0, runtime_qualified=False)))
