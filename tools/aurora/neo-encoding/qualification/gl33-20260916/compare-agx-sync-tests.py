#!/usr/bin/env python3
"""Save and compare terminal local suite results; not hardware qualification."""
from pathlib import Path
from collections import Counter
import json
import re
import subprocess

HERE = Path(__file__).resolve().parent
runs = {}
for label in ('agx-sync-20260916', 'agx-upstream-20260916'):
    remote = '/home/mcruz.guest/neo/build/mesa-j700-' + label + '/meson-logs/testlog.json'
    raw = subprocess.check_output(['limactl', 'shell', 'neo-builder', 'cat', remote], text=True)
    (HERE / (label + '-linux') / 'testlog.json').write_text(raw)
    runs[label] = {x['name']: x for x in map(json.loads, raw.splitlines())}
candidate, control = runs.values()

def diagnostics(test):
    text = test.get('stdout', '') + '\n' + test.get('stderr', '')
    text = re.sub(r'mesa-j700-agx-(?:sync|upstream)-20260916', 'SOURCE', text)
    text = re.sub(r'(?<=\.c):\d+:', ':LINE:', text)
    text = re.sub(r'0x[0-9a-fA-F]+', 'ADDRESS', text)
    return text.strip()

report = {'hardware_execution': False,
          'candidate_counts': dict(Counter(x['result'] for x in candidate.values())),
          'upstream_counts': dict(Counter(x['result'] for x in control.values())),
          'candidate_only': [{'test': name, 'result': candidate[name]['result']}
                             for name in sorted(candidate.keys() - control.keys())],
          'upstream_only': [{'test': name, 'result': control[name]['result']}
                            for name in sorted(control.keys() - candidate.keys())],
          'status_changes': [], 'common_failure_diagnostic_differences': []}
for name in sorted(candidate.keys() & control.keys()):
    a, b = candidate[name], control[name]
    if a['result'] != b['result']:
        report['status_changes'].append({'test': name, 'candidate': a['result'], 'upstream': b['result']})
    elif a['result'] == 'FAIL' and diagnostics(a) != diagnostics(b):
        report['common_failure_diagnostic_differences'].append(name)
(HERE / 'agx-sync-20260916-test-comparison.json').write_text(json.dumps(report, indent=2) + '\n')
print(json.dumps(report, indent=2))
