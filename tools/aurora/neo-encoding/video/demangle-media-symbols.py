#!/usr/bin/env python3
"""Generate a bounded function index for a previously extracted KC driver."""
import argparse
import json
from pathlib import Path
import subprocess

p = argparse.ArgumentParser(description=__doc__)
p.add_argument('root', type=Path)
a = p.parse_args()
manifest = json.loads((a.root / 'manifest.json').read_text())
text = next(x for x in manifest['segments'] if x['name'] == '__TEXT_EXEC')
base, size = int(text['address'], 16), text['file_size']
symbols = json.loads((a.root / 'symbols.json').read_text())
functions = [x for x in symbols if x['type'] & 0x0e == 14 and base <= int(x['address'], 16) < base + size]
names = '\n'.join(x['name'][1:] if x['name'].startswith('__Z') else x['name'] for x in functions) + '\n'
decoded = subprocess.check_output(['c++filt', '-n'], input=names, text=True).splitlines()
assert len(decoded) == len(functions)
for function, name in zip(functions, decoded):
    function['demangled'] = name
(a.root / 'functions.json').write_text(json.dumps(functions, indent=2) + '\n')
print('MEDIA_FUNCTIONS_INDEXED', len(functions))
