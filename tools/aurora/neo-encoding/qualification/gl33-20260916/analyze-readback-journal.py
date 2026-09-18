#!/usr/bin/env python3
"""Verify producer-to-map histories captured at real late pixel changes."""
import hashlib
import json
from pathlib import Path
import re

here = Path(__file__).resolve().parent
path = here/'readback-journal-observe-3000.log'
text = path.read_text()
pattern = re.compile(r'readback-journal: (\d+) (\S+) bo=(\d+) va=(0x[0-9a-f]+) batch=(\d+) sync=(\d+) value=(0x[0-9a-f]+) size=(\d+) result=(-?\d+)')
results = []
for header, body in re.findall(r'readback-journal: begin ([^\n]+)\n(.*?)readback-journal: end', text, re.S):
    bo = int(re.search(r'bo=(\d+)', header)[1])
    events = []
    for line in body.splitlines():
        match = pattern.fullmatch(line)
        assert match, line
        v = match.groups()
        events.append(dict(index=int(v[0]), kind=v[1], bo=int(v[2]), va=int(v[3],16),
                           batch=int(v[4]), sync=int(v[5]), value=int(v[6],16),
                           size=int(v[7]), result=int(v[8])))
    writer = [e for e in events if e['kind'] == 'writer-set' and e['bo'] == bo][-1]
    chain = [e for e in events if e['index'] >= writer['index']
             and (e['bo'] == bo or e['sync'] == writer['sync'])]
    assert [e['kind'] for e in chain] == ['writer-set', 'cpu-writer', 'attachment',
        'submit-start', 'submit-return', 'wait-begin', 'wait-return', 'cleanup', 'cpu-map']
    _, lookup, attachment, start, returned, wait, waited, cleanup, mapped = chain
    assert lookup['batch'] == attachment['batch'] == writer['batch']
    assert attachment['result'] == start['value'] == returned['value']
    assert returned['result'] == waited['result'] == cleanup['result'] == 0
    assert attachment['va'] == attachment['value'] == mapped['va']
    assert attachment['size'] >= mapped['size']
    assert mapped['result'] == 1  # PIPE_MAP_READ
    results.append(dict(bo=bo, gpu_va=hex(mapped['va']), syncobj=writer['sync'],
                        submission_seqid=start['value'], attachment_bytes=attachment['size'],
                        observed_bytes=mapped['size'], verified_chain=chain))
match = re.search(r'fence-observe: observed=(\d+) changed=(\d+) pixel_changed=(\d+)', text)
assert match and results
observed, changed, pixels = map(int, match.groups())
summary = dict(log_sha256=hashlib.sha256(path.read_bytes()).hexdigest(),
               observed_mappings=observed, changed_mappings=changed, pixel_changes=pixels,
               captured_histories=len(results), verified_histories=len(results),
               histories=results, conformance_passed=False,
               conclusion='Captured histories show the mapped BO as the submitted attachment and a successful wait on its writer syncobj before its pixel bytes changed. This does not prove that the kernel or firmware signalled that fence at the correct time.')
output = here/'readback-journal-analysis.json'
output.write_text(json.dumps(summary, indent=2)+'\n')
print(json.dumps({k:v for k,v in summary.items() if k != 'histories'}))
