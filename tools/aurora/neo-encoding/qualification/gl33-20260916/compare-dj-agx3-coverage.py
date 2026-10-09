#!/usr/bin/env python3
"""Compare official full-run executions, keeping config checks separate."""
from collections import Counter, defaultdict
import json
from pathlib import Path

BASE = Path(__file__).resolve().parent
OLD = BASE / 'fast-iterations/audited-eot-full/results/audited-eot-full'
NEW = BASE / 'fast-iterations/dj-agx3-full/results/dj-agx3-full'

def read_run(path):
    receipt = json.loads((path / 'receipt.json').read_text())
    assert receipt['mode'] == 'full' and receipt['profile'] == 'gl30'
    ledger = json.loads((path / 'case-ledger.json').read_text())
    main = {(r['log'], r['case']): r for r in ledger
            if r['log'] != 'configs.qpa'}
    assert len(main) == sum(r['log'] != 'configs.qpa' for r in ledger)
    return receipt, ledger, main

old_receipt, old_ledger, old = read_run(OLD)
new_receipt, new_ledger, new = read_run(NEW)
assert old_receipt['cts_commit'] == new_receipt['cts_commit']
assert old_receipt['cts_runner_sha256'] == new_receipt['cts_runner_sha256']
shared = old.keys() & new.keys()
transitions = Counter((old[k]['status'], new[k]['status']) for k in shared)
families = defaultdict(lambda: {'old': Counter(), 'new': Counter()})
for tag, rows in [('old', old), ('new', new)]:
    for row in rows.values():
        families[row['case'].split('.')[1]][tag][row['status']] += 1

result = {
    'same_schedule': old.keys() == new.keys(),
    'shared_executions': len(shared),
    'old_schedule': len(old), 'new_schedule': len(new),
    'old_status': dict(Counter(r['status'] for r in old.values())),
    'new_status': dict(Counter(r['status'] for r in new.values())),
    'old_config_status': dict(Counter(r['status'] for r in old_ledger
                                      if r['log'] == 'configs.qpa')),
    'new_config_status': dict(Counter(r['status'] for r in new_ledger
                                      if r['log'] == 'configs.qpa')),
    'transitions': [{'old': a, 'new': b, 'count': n}
                    for (a, b), n in sorted(transitions.items())],
    'recovered_executions': [new[k] for k in sorted(shared)
                             if old[k]['status'] not in ['Pass', 'NotRun']
                             and new[k]['status'] == 'Pass'],
    'regressed_executions': [new[k] for k in sorted(shared)
                             if old[k]['status'] == 'Pass'
                             and new[k]['status'] not in ['Pass', 'NotRun']],
    'family_status': dict(families),
    'old_receipt': old_receipt, 'new_receipt': new_receipt,
}
(BASE / 'dj-agx3-coverage-comparison.json').write_text(
    json.dumps(result, indent=2) + '\n')
print(json.dumps({k: result[k] for k in [
    'same_schedule', 'shared_executions', 'old_status', 'new_status',
    'old_config_status', 'new_config_status', 'transitions']}, indent=2))
print('Recovered executions:', len(result['recovered_executions']))
print('Regressed executions:', len(result['regressed_executions']))
