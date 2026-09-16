#!/usr/bin/env python3
"""Account for every officially selected case, including incomplete/unrun work."""
from collections import Counter
import json
from pathlib import Path
import re
import shlex
import sys
import xml.etree.ElementTree as ET
from cts_faults import scan_kernel_faults

campaign = Path(__file__).resolve().parent
run = Path(sys.argv[1]).resolve()
inventory = Path(sys.argv[2]).resolve()
expected = ET.parse(inventory/'cts-run-summary.xml').getroot()
receipt = json.loads((run/'receipt.json').read_text())
assert receipt['mode'] == 'full'
profile = receipt['profile']
# A gl33-type run selects gl33-main plus the SAME noctx list gl30 uses; it does
# not run the gl31/gl32 lists.  Measured from the installed CTS and from a run's
# own config-*.qpa families on 2026-09-15.
CATALOGUE = {
    'gl30': ('gl30-main.txt', 878),
    'gl33': ('gl33-main.txt', 9886),
}
assert profile in CATALOGUE, 'analyze-run.py has no case list for ' + profile
assert expected.attrib['Type'] == profile
main_name, main_count = CATALOGUE[profile]
lists = {
    'gl_cts/data/mustpass/gl/khronos_mustpass/main/' + main_name:
        (campaign/main_name).read_text().splitlines(),
    'gl_cts/data/mustpass/gl/khronos_mustpass_noctx/main/gl30-khr-main.txt':
        (campaign/'gl30-noctx.txt').read_text().splitlines(),
}
assert len(lists['gl_cts/data/mustpass/gl/khronos_mustpass/main/' + main_name]) == main_count
ledger, sessions, issues = [], [], []
def parse_log(name):
    path = run/name
    text = path.read_text(errors='replace') if path.exists() else ''
    results = {}
    chunks = re.split(r'^#beginTestCaseResult ([^\n]+)\n', text, flags=re.M)
    for i in range(1, len(chunks), 2):
        case, body = chunks[i].strip(), chunks[i+1]
        if case in results:
            issues.append('Duplicate '+name+': '+case)
        result = dict(status='Incomplete', message='No complete QPA result')
        if '#endTestCaseResult' in body:
            try:
                xml = ET.fromstring(body.split('#endTestCaseResult', 1)[0].strip())
                assert xml.attrib['CasePath'] == case
                r = xml.find('Result')
                assert r is not None
                result = dict(status=r.attrib['StatusCode'], message=''.join(r.itertext()),
                              details=[''.join(t.itertext()) for t in xml.findall('Text')])
            except (ET.ParseError, AssertionError, KeyError) as error:
                issues.append('Malformed '+name+': '+case+' '+type(error).__name__)
        results[case] = result
    begins = list(re.finditer(r'^#beginSession\s*$', text, flags=re.M))
    ends = list(re.finditer(r'^#endSession\s*$', text, flags=re.M))
    complete = len(begins) == len(ends) == 1 and begins[0].start() < ends[0].start()
    if complete:
        case_markers = list(re.finditer(r'^#(?:begin|end)TestCaseResult\b', text, flags=re.M))
        complete = all(begins[0].end() < m.start() < ends[0].start()
                       for m in case_markers)
    if not complete:
        issues.append('Missing or invalid session boundary: '+name)
    return results, complete

configs_name = expected.find('Configs').attrib['FileName']
config_results, config_complete = parse_log(configs_name)
if not config_results:
    issues.append('Empty configuration results: '+configs_name)
for case, result in config_results.items():
    ledger.append(dict(log=configs_name, case=case, **result))
sessions.append(dict(log=configs_name, kind='configuration', observed=len(config_results), complete=config_complete))

for session in expected.findall('TestRun'):
    name = session.attrib['FileName']
    args = dict(a[2:].split('=', 1) for a in shlex.split(session.attrib['CmdLine']))
    selected = lists[args['deqp-caselist-file']]
    actual, complete = parse_log(name)
    extra = sorted(set(actual)-set(selected))
    if extra:
        issues.append('Unexpected cases in '+name+': '+repr(extra))
    for case in selected:
        ledger.append(dict(log=name, case=case, **actual.get(case, dict(status='NotRun', message='No QPA case entry'))))
    fully_executed = complete and set(actual) == set(selected) and all(r['status'] != 'Incomplete' for r in actual.values())
    sessions.append(dict(log=name, kind='test', expected=len(selected), observed=len(actual), ended=complete, complete=fully_executed,
                         command=session.attrib['CmdLine']))

official = run/'cts-run-summary.xml'
verdict = None
inventory_matches = False
try:
    observed = ET.parse(official).getroot()
    verdict = observed.attrib
    def session_identity(root):
        return Counter((s.attrib['FileName'], tuple(shlex.split(s.attrib['CmdLine'])))
                       for s in root.findall('TestRun'))
    planned = session_identity(expected)
    actual_inventory = session_identity(observed)
    planned_names = [s.attrib['FileName'] for s in expected.findall('TestRun')]
    inventory_matches = (
        observed.tag == expected.tag and verdict.get('Type') == profile and
        observed.find('Configs').attrib['FileName'] == configs_name and
        len(planned_names) == len(set(planned_names)) and
        actual_inventory == planned)
    if not inventory_matches:
        issues.append('Official summary does not match the pinned session inventory')
except (OSError, ET.ParseError, AttributeError, KeyError, ValueError) as error:
    issues.append('Missing or malformed official summary: '+type(error).__name__)
counts = Counter(item['status'] for item in ledger)
kernel_audit = scan_kernel_faults(run/name for name in
    ('kernel-before.log', 'kernel-stream.log', 'kernel-after.log'))
kernel_audit['receipt_disagrees'] = (
    kernel_audit['fault_detected'] is True and receipt.get('kernel_fault') is False)
summary = dict(receipt=receipt, official_summary=verdict, expected_test_sessions=len(sessions)-1,
    kernel_fault_audit=kernel_audit,
    hardware_execution_valid=kernel_audit['evidence_complete']
        and kernel_audit['fault_detected'] is False
        and receipt.get('kernel_fault') is False
        and receipt.get('kernel_stream_overruns') == 0,
    official_inventory_matches=inventory_matches,
    official_conformant=bool(verdict and verdict.get('Conformant') == 'True'),
    selected_test_executions=sum(s['expected'] for s in sessions if s['kind']=='test'),
    observed_configuration_cases=len(config_results), counts=dict(counts),
    complete_sessions=sum(s['complete'] for s in sessions), total_sessions=len(sessions), issues=issues,
    fully_accounted=not issues and all(s['complete'] for s in sessions)
                    and not any(counts[x] for x in ('NotRun', 'Incomplete')),
    acceptance_note='Preserve unsupported/warnings and inspect applicability. An advertised API, partial run, or process status is not conformance.')
(run/'case-ledger.json').write_text(json.dumps(ledger, indent=2)+'\n')
(run/'session-ledger.json').write_text(json.dumps(sessions, indent=2)+'\n')
(run/'analysis.json').write_text(json.dumps(summary, indent=2)+'\n')
print(json.dumps({k:summary[k] for k in ('counts','complete_sessions','total_sessions','fully_accounted','issues',
                                       'hardware_execution_valid','kernel_fault_audit')}))
