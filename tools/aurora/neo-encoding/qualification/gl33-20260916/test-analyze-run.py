#!/usr/bin/env python3
"""Exercise final accounting using synthetic QPA files, never hardware."""
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import xml.etree.ElementTree as ET

HERE = Path(__file__).resolve().parent
CASES = (HERE / 'gl30-main.txt').read_text().splitlines()
assert len(CASES) == 878
COMMAND = '--deqp-caselist-file=gl_cts/data/mustpass/gl/khronos_mustpass/main/gl30-main.txt --deqp-base-seed=1'


def summary(command=COMMAND, profile='gl30', conformant='True', duplicate=False):
    root = ET.Element('Summary', Type=profile, Conformant=conformant)
    ET.SubElement(root, 'Configs', FileName='configs.qpa')
    ET.SubElement(root, 'TestRun', FileName='main.qpa', CmdLine=command)
    if duplicate:
        ET.SubElement(root, 'TestRun', FileName='main.qpa', CmdLine=command)
    return ET.tostring(root, encoding='unicode')


def result(case, status='Pass'):
    root = ET.Element('TestCaseResult', CasePath=case)
    ET.SubElement(root, 'Result', StatusCode=status).text = status
    return '#beginTestCaseResult ' + case + '\n' + ET.tostring(root, encoding='unicode') + '\n#endTestCaseResult\n'


def qpa(body):
    return '#beginSession\n' + body + '#endSession\n'


with tempfile.TemporaryDirectory(prefix='cts-accounting-') as tmp:
    root = Path(tmp)
    inventory = root / 'inventory'
    run = root / 'run'
    inventory.mkdir()
    run.mkdir()
    base_main = qpa(''.join(result(c, 'NotSupported' if i == 0 else 'Pass')
                            for i, c in enumerate(CASES)))
    base_configs = qpa(result('CTS-Configs.synthetic'))
    cases = ['complete', 'no-session-end', 'empty-configs', 'changed-seed',
             'duplicate-result', 'truncated-result', 'wrong-profile',
             'missing-summary', 'duplicate-inventory', 'complete-but-failed',
             'missed-firmware-fault', 'missing-kernel-evidence', 'kernel-overrun']
    for mode in cases:
        (run / 'receipt.json').write_text(json.dumps({'mode': 'full', 'profile': 'gl30',
            'kernel_fault': False, 'kernel_stream_overruns': int(mode == 'kernel-overrun')}))
        for name in ('kernel-before.log', 'kernel-stream.log', 'kernel-after.log'):
            (run/name).write_text('asahi: GPU firmware initialized\n')
        if mode == 'missed-firmware-fault':
            (run/'kernel-stream.log').write_text('asahi: G17PMARK rtkit-boot role=gfx ok=false\n')
        elif mode == 'missing-kernel-evidence':
            (run/'kernel-stream.log').unlink()
        main, configs = base_main, base_configs
        planned, official = summary(), summary()
        if mode == 'no-session-end':
            main = main.replace('#endSession\n', '')
        elif mode == 'empty-configs':
            configs = qpa('')
        elif mode == 'changed-seed':
            official = summary(command=COMMAND.replace('seed=1', 'seed=2'))
        elif mode == 'duplicate-result':
            main = main.replace('#endSession', result(CASES[0]) + '#endSession')
        elif mode == 'truncated-result':
            main = main[:main.rfind('#endTestCaseResult')] + '#endSession\n'
        elif mode == 'wrong-profile':
            official = summary(profile='gl33')
        elif mode == 'duplicate-inventory':
            planned = official = summary(duplicate=True)
        elif mode == 'complete-but-failed':
            main = main.replace('StatusCode="Pass"', 'StatusCode="Fail"', 1)
            official = summary(conformant='False')
        (inventory / 'cts-run-summary.xml').write_text(planned)
        (run / 'configs.qpa').write_text(configs)
        (run / 'main.qpa').write_text(main)
        terminal = run / 'cts-run-summary.xml'
        terminal.write_text(official)
        if mode == 'missing-summary':
            terminal.unlink()
        output = subprocess.run([sys.executable, str(HERE / 'analyze-run.py'), str(run), str(inventory)],
                                capture_output=True, text=True, check=True)
        analysis = json.loads((run / 'analysis.json').read_text())
        want_complete = mode in ('complete', 'complete-but-failed', 'missed-firmware-fault',
                                 'missing-kernel-evidence', 'kernel-overrun')
        assert analysis['fully_accounted'] == want_complete, (mode, output.stdout)
        assert analysis['hardware_execution_valid'] == (mode not in
            ('missed-firmware-fault', 'missing-kernel-evidence', 'kernel-overrun'))
        if mode == 'complete':
            assert analysis['counts'] == {'Pass': 878, 'NotSupported': 1}
            assert analysis['official_conformant'] is True
        elif mode == 'complete-but-failed':
            assert analysis['counts']['Fail'] == 1
            assert analysis['official_conformant'] is False
        elif mode == 'missed-firmware-fault':
            assert analysis['receipt']['kernel_fault'] is False
            assert analysis['kernel_fault_audit']['receipt_disagrees'] is True
            assert analysis['kernel_fault_audit']['fault_detected'] is True
        elif mode == 'missing-kernel-evidence':
            assert analysis['kernel_fault_audit']['evidence_complete'] is False
        elif mode == 'kernel-overrun':
            assert analysis['kernel_fault_audit']['fault_detected'] is False
        else:
            assert analysis['issues'] or analysis['counts'].get('Incomplete')
        print(mode + ': passed')
print(str(len(cases)) + ' accounting regressions passed; no hardware execution')
