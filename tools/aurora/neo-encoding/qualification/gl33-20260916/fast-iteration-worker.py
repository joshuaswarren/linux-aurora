#!/usr/bin/env python3
"""One serialized native CTS iteration; retain healthy Xorg for the next one."""
import fcntl
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import time

CAMPAIGN = Path('/home/mcruz/gl30-cts-20260913')
assert os.geteuid() == 0
job_path = Path(sys.argv[1])
assert job_path.parent == CAMPAIGN and re.fullmatch(r'fast-job-[a-z0-9-]+\.json', job_path.name)
job = json.loads(job_path.read_text())
boot, label = job['boot'], job['label']
assert re.fullmatch(r'[a-z0-9-]+', label)
assert re.fullmatch(r'[0-9a-f-]{36}', boot)
assert re.fullmatch(r'[0-9a-f]{64}', job['mesa_sha256'])
assert job['mode'] in ('full', 'diagnostic', 'replay', 'probe', 'sparse-depth')
assert (job.get('case') is not None) == (job['mode'] == 'diagnostic')
assert not job['trace'] or job['mode'] == 'diagnostic'
assert not job.get('session') or job['mode'] == 'diagnostic'
assert bool(job.get('replay')) == (job['mode'] == 'replay')
assert job.get('shader_cache', 'off') in ('off', 'isolated')
assert not job['trace'] or job.get('shader_cache', 'off') == 'off'
lock = open('/run/neo-gl30-fast-iteration.lock', 'w')
fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
assert Path('/proc/sys/kernel/random/boot_id').read_text().strip() == boot
assert b'apple,j700' in Path('/proc/device-tree/compatible').read_bytes().split(b'\0')
report_path = CAMPAIGN / boot / ('fast-iteration-' + label + '.json')
assert not report_path.exists()
report = {'boot': boot, 'label': label, 'mesa_sha256': job['mesa_sha256'],
          'steps': [], 'rebooted': False, 'resident_modified': False,
          'complete': False}
start = time.monotonic()


def step(name, args, check=True):
    before = time.monotonic()
    result = subprocess.run(['python3', str(CAMPAIGN/name), *args])
    report['steps'].append({'name': name, 'seconds': time.monotonic()-before,
                            'exit_code': result.returncode})
    if check and result.returncode:
        raise RuntimeError(name + ' did not complete')
    return result.returncode


try:
    # Helpers independently verify identity, boot, current hashes and kernel
    # health before each operation. None of these paths can reboot or probe DCP.
    desktop = subprocess.run(['systemctl', 'is-active', '--quiet', 'sddm']).returncode == 0
    if desktop:
        assert job['take_desktop'], 'Explicit --take-desktop is needed once'
        step('stop-desktop-for-cts.py', [boot, job['old_sha256']])
    if job.get('candidate') and job['old_sha256'] != job['mesa_sha256']:
        step('install-mesa-candidate.py', [boot, job['old_sha256'], job['mesa_sha256'], job['candidate']])
    state_path = CAMPAIGN / boot / 'x11-server/state.json'
    report['xorg_reused'] = state_path.exists()
    if not state_path.exists():
        step('start-x11-hardware.py', [boot])
    state = json.loads(state_path.read_text())
    report['xorg_pid'] = state['pid']
    # The supervisor verifies this PID's actual comm/cmdline, not just the file.
    args = [boot, label, job['mode'], 'x11', job['mesa_sha256']]
    profile = job.get('profile', 'gl30')
    assert profile in ('gl30', 'gl31', 'gl32', 'gl33')
    if job['mode'] == 'diagnostic':
        args.append(job['case'])
        if job['trace']:
            args.append('trace')
        if job.get('session'):
            args.append('session=' + json.dumps(job['session']))
    if profile != 'gl30':
        args.append('profile=' + profile)
    if job.get('shader_cache', 'off') != 'off':
        args.append('cache=' + job['shader_cache'])
    if job['mode'] == 'replay':
        args.append('replay=' + str(job_path))
    if job['mode'] == 'probe':
        assert re.fullmatch(r'[0-9a-f]{64}', job['probe_sha256'])
        assert 24 <= job['probe_iterations'] <= 1000000
        args += ['probe=' + job['probe_sha256'], 'iterations=' + str(job['probe_iterations'])]
    if job['mode'] == 'sparse-depth':
        assert re.fullmatch(r'[0-9a-f]{64}', job['probe_binary_sha256'])
        assert profile == 'gl33'
        args.append('probe-binary=' + job['probe_binary_sha256'])
    report['cts_exit_code'] = step('run-official-cts.py', args, check=False)
    receipt = json.loads((CAMPAIGN/boot/label/'receipt.json').read_text())
    assert receipt['boot'] == boot and receipt['mesa_sha256'] == job['mesa_sha256']
    report['kernel_fault'] = receipt['kernel_fault']
    report['probe_seconds' if job['mode'] in ('probe', 'sparse-depth') else 'cts_seconds'] = receipt['elapsed_seconds']
    if receipt['kernel_fault']:
        step('stop-x11-hardware.py', [boot])
        report['next_action'] = 'fresh_boot_required'
    else:
        report['next_action'] = 'reuse_healthy_xorg'
    report['complete'] = True
except Exception as error:
    report['error'] = str(error)
finally:
    report['seconds'] = time.monotonic() - start
    report_path.write_text(json.dumps(report, indent=2) + '\n')
    print('FAST_ITERATION_TERMINAL ' + json.dumps(report), flush=True)
sys.exit(0 if report['complete'] and not report['kernel_fault'] else 1)
