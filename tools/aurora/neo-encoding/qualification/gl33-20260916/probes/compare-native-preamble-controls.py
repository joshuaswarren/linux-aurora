#!/usr/bin/env python3
"""Compare saved native compiler controls; do not execute any GPU workload."""
from pathlib import Path
import hashlib
import importlib.util
import json

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent
spec = importlib.util.spec_from_file_location('decode', HERE / 'decode-native-preamble-captures.py')
decode = importlib.util.module_from_spec(spec)
spec.loader.exec_module(decode)

def capture(directory):
    receipt = json.loads((directory / 'receipt.json').read_text())
    assert receipt['hardware_execution'] is False
    metadata = next(s for s in receipt['sections']
                    if s['segment'] == '__GPU_METADATA' and s['name'] == '__vertex')
    text = next(s for s in receipt['sections'] if s['object_offset'] == metadata['object_offset']
                and s['segment'] == '__TEXT' and s['name'] == '__text')
    for section in (metadata, text):
        assert hashlib.sha256((directory / section['file']).read_bytes()).hexdigest() == section['sha256']
    info = decode.decode(directory)
    return {'text_sha256': text['sha256'], 'metadata': {k: info[k] for k in (
        'temporary_register_count', 'thread_invariant_spill_buffer_bytes',
        'thread_invariant_largest_ipr_bytes', 'largest_ipr_bytes', 'entry_ipr_bytes',
        'constant_phases', 'text_symbols', 'text_size_including_constant_program_and_padding')}}

results = []
for label in ('gb-off', 'gb-on', 'yield-off', 'yield-on', 'uniform-option-off', 'uniform-option-on'):
    for count in (1, 7):
        base = 'native-preamble-buffers-host27' if count == 1 else 'native-preamble-entry-window-host27'
        name = f'vertex-buffers-{count}'
        reference = capture(ROOT / base / name)
        candidate = capture(ROOT / f'native-preamble-{label}-host27' / name)
        results.append({'control': label, 'buffers': count,
                        'matches_default': reference == candidate, 'observed': candidate})
assert all(r['matches_default'] for r in results)
output = ROOT / 'native-preamble-control-comparison.json'
output.write_text(json.dumps({'hardware_execution': False, 'comparisons': results,
    'conclusion': 'No observed effect for these inputs; no claim the controls were honored.'}, indent=2) + '\n')
print(f'{len(results)} saved control captures match default vertex text and decoded metadata')
