#!/usr/bin/env python3
"""Compare terminal configuration results and independently rescan kernel logs.

This audit supplements the strict inventory and applicability analyzers. It
does not change the original QPAs, official summary, or supervisor receipt.
"""
import argparse
from collections import Counter
import hashlib
import json
from pathlib import Path
import re
import xml.etree.ElementTree as ET

from cts_faults import scan_kernel_faults


def read_results(path):
    chunks = re.split(r'^#beginTestCaseResult ([^\n]+)\n',
                      path.read_text(errors='replace'), flags=re.M)
    results = {}
    for index in range(1, len(chunks), 2):
        case, body = chunks[index].strip(), chunks[index+1]
        if case in results or '#endTestCaseResult' not in body:
            raise ValueError('Duplicate or incomplete case in ' + str(path))
        root = ET.fromstring(body.split('#endTestCaseResult', 1)[0].strip())
        if root.attrib['CasePath'] != case:
            raise ValueError('Case identity mismatch in ' + str(path))
        results[case] = root.find('Result').attrib['StatusCode']
    return results


def audit(run):
    receipt = json.loads((run/'receipt.json').read_text())
    profile = receipt['profile']
    files = sorted(run.glob('config-' + profile + '-main-*.qpa'))
    if not files:
        raise ValueError('No main configuration QPAs')
    results = {path.name: read_results(path) for path in files}
    totals = Counter()
    failures = []
    for name, cases in results.items():
        totals.update(cases.values())
        failed = [case for case, status in cases.items() if status == 'Fail']
        if failed:
            failures.append(dict(log=name, count=len(failed), first_case=failed[0],
                counts=dict(Counter(cases.values())),
                other_configurations=[dict(log=other, counts=dict(Counter(
                    outcomes.get(case, 'Missing') for case in failed)))
                    for other, outcomes in results.items() if other != name]))
    kernel = scan_kernel_faults(run/name for name in
        ('kernel-before.log', 'kernel-stream.log', 'kernel-after.log'))
    kernel['receipt_disagrees'] = (kernel['fault_detected'] is True
                                  and receipt.get('kernel_fault') is False)
    kernel['detector_sha256'] = hashlib.sha256(
        Path(__file__).with_name('cts_faults.py').read_bytes()).hexdigest()
    official = ET.parse(run/'cts-run-summary.xml').getroot().attrib
    return dict(run=str(run.resolve()), main_configurations=len(files),
        counts=dict(totals), official_summary=official,
        runner_exit_code=receipt['exit_code'],
        original_receipt_kernel_fault=receipt.get('kernel_fault'),
        kernel_fault_audit=kernel, failed_configurations=failures,
        unsupported_distribution=dict(Counter(str(sum(status == 'NotSupported'
            for status in cases.values())) for cases in results.values())),
        scope='Same-driver configuration comparison, not a prior-version regression test. '
              'Strict completion and skip applicability require their separate audits.')


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('run', type=Path)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    result = audit(args.run)
    args.output.write_text(json.dumps(result, indent=2) + '\n')
    print(json.dumps({key: result[key] for key in
        ('counts', 'main_configurations', 'official_summary', 'runner_exit_code')}))
    print('Corrected kernel fault:', result['kernel_fault_audit']['fault_detected'])
    print('Evidence:', args.output.resolve())
