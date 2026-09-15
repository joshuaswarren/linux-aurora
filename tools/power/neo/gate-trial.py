#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-2.0-only
"""Gate a set of unmanaged PMGR domains, sleep once, restore, and check health.

Sequence: awake SMC average, gate (children first, durable checkpoints), awake
SMC average, optional timed sleep with post-resume SMC readings, restore in
reverse order, awake SMC average, then storage/network/display checks. Every
step is saved before the next MMIO write so a fatal fault is attributable.
Numbers are SMC floats and battery-counter estimates, not external metrology.
"""
import argparse
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys
import time

HERE = Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location('neo_pm', HERE / 'pm-test.py')
pm = importlib.util.module_from_spec(spec)
spec.loader.exec_module(pm)


def run(*cmd, check=True):
    p = subprocess.run([str(c) for c in cmd], capture_output=True, text=True)
    if check and p.returncode:
        raise RuntimeError('%s failed rc=%d: %s %s' % (cmd, p.returncode, p.stdout[-2000:], p.stderr[-2000:]))
    return p


def smc_avg(seconds):
    return json.loads(run(sys.executable, HERE / 'smc-avg.py', '--seconds', seconds).stdout)


def health(payload_path, expected):
    with payload_path.open('rb', buffering=0) as f:
        os.posix_fadvise(f.fileno(), 0, 0, os.POSIX_FADV_DONTNEED)
        storage_ok = hashlib.sha256(f.read()).hexdigest() == expected
    # An old IPv4 address can survive a broken PCIe link. Require association
    # and a response from the local gateway after the radio has reconnected.
    deadline = time.monotonic() + 15
    network_ok = False
    while time.monotonic() < deadline:
        link = run('iw', 'dev', 'wlan0', 'link', check=False).stdout
        routes = json.loads(run('ip', '-j', 'route', 'show', 'default', 'dev', 'wlan0').stdout)
        gateways = [r['gateway'] for r in routes if 'gateway' in r]
        if 'Connected to ' in link and gateways:
            network_ok = run('ping', '-I', 'wlan0', '-c', '1', '-W', '2', gateways[0], check=False).returncode == 0
            if network_ok:
                break
        time.sleep(1)
    display = Path('/sys/module/neo_display_sleep/parameters/state')
    return {'storage_ok': storage_ok, 'network_ok': network_ok,
            'display_state': display.read_text().strip() if display.exists() else None,
            'suspend_fail': Path('/sys/power/suspend_stats/fail').read_text().strip()}


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument('--gate', default='', help='comma-separated domain names, children first')
    ap.add_argument('--target', default='0')
    ap.add_argument('--sleep', type=int, default=0, help='timed sleep seconds (0 = awake only)')
    ap.add_argument('--awake', type=float, default=15, help='awake SMC averaging seconds')
    ap.add_argument('--scale', type=float)
    ap.add_argument('--output', type=Path, required=True)
    args = ap.parse_args()
    args.output.mkdir(parents=True, exist_ok=False)
    names = [n for n in args.gate.split(',') if n]
    result = {'status': 'running', 'gate': names, 'target': args.target, 'sleep': args.sleep,
              'boot_id': Path('/proc/sys/kernel/random/boot_id').read_text().strip(), 'steps': []}
    result['before'] = pm.snapshot()
    result['tool_sha256'] = {p.name: hashlib.sha256(p.read_bytes()).hexdigest()
                             for p in HERE.iterdir() if p.name in
                             ('pmgr-ps', 'pmgr-gate.py', 'gate-trial.py', 'sleep-smc.py',
                              'pm-test.py', 'sleep-test.py', 'j700-pmgr-domains.json')}
    log = args.output / 'pmgr.log'
    state = args.output / 'state.json'

    def save(step=None):
        if step:
            result['steps'].append(step)
        tmp = args.output / 'result.tmp'
        tmp.write_text(json.dumps(result, indent=1) + '\n')
        tmp.replace(args.output / 'result.json')
        os.sync()

    payload = os.urandom(1 << 20)
    expected = hashlib.sha256(payload).hexdigest()
    payload_path = args.output / 'storage.bin'
    with payload_path.open('wb') as f:
        f.write(payload)
        f.flush()
        os.fsync(f.fileno())
    save()
    gate_args = ['--log', log, '--state', state]
    try:
        result['awake_before'] = smc_avg(args.awake)
        save({'step': 'awake_before'})
        if names:
            p = run(sys.executable, HERE / 'pmgr-gate.py', 'gate', *gate_args, '--target', args.target, *names)
            result['gate_output'] = p.stdout
            save({'step': 'gated'})
            result['awake_gated'] = smc_avg(args.awake)
            save({'step': 'awake_gated'})
        if args.sleep:
            cmd = [sys.executable, HERE / 'sleep-smc.py', '--seconds', args.sleep, '--output', args.output / 'sleep']
            if args.scale:
                cmd += ['--scale', args.scale]
            p = run(*cmd, check=False)
            result['sleep_summary'] = p.stdout.strip()
            result['sleep_rc'] = p.returncode
            save({'step': 'slept'})
            if p.returncode:
                raise RuntimeError('sleep qualification failed')
    except Exception as error:
        result['error'] = str(error)
    finally:
        try:
            if names and state.exists():
                p = run(sys.executable, HERE / 'pmgr-gate.py', 'ungate', *gate_args, check=False)
                result['ungate_output'] = p.stdout + p.stderr
                result['ungate_rc'] = p.returncode
                if p.returncode:
                    raise RuntimeError('domain restoration failed; retained state file identifies unresolved domains')
                records = json.loads(state.read_text())
                if not all(v.get('restore_complete') for v in records.values()):
                    raise RuntimeError('domain restoration incomplete')
                result['domain_restoration'] = records
                save({'step': 'restored'})
            result['awake_after'] = smc_avg(args.awake)
            result['health'] = health(payload_path, expected)
            result['after'] = pm.snapshot()
            result['suspend_stats_check'] = pm.check_suspend_stats(result['before'], result['after'])
            pm.check_firmware_health(result['after'])
            if Path('/proc/sys/kernel/random/boot_id').read_text().strip() != result['boot_id']:
                raise RuntimeError('boot changed')
            if not result['health']['storage_ok'] or not result['health']['network_ok']:
                raise RuntimeError('storage/network recovery failed')
            if result['health']['display_state'] != '1':
                raise RuntimeError('panel not restored')
        except Exception as error:
            result['recovery_error'] = str(error)
        result['status'] = 'error' if 'error' in result or 'recovery_error' in result else 'passed'
        save({'step': 'health'})

    def mean(block, key):
        return block.get('keys', {}).get(key, {}).get('mean')

    print(json.dumps({'status': result['status'], 'gate': names,
                      'PSTR_before': mean(result.get('awake_before', {}), 'PSTR'),
                      'PSTR_gated': mean(result.get('awake_gated', {}), 'PSTR'),
                      'PSTR_after': mean(result.get('awake_after', {}), 'PSTR'),
                      'PPMC_before': mean(result.get('awake_before', {}), 'PPMC'),
                      'PPMC_gated': mean(result.get('awake_gated', {}), 'PPMC'),
                      'sleep': result.get('sleep_summary'), 'sleep_rc': result.get('sleep_rc'),
                      'ungate_rc': result.get('ungate_rc'), 'health': result.get('health'),
                      'error': result.get('error'), 'recovery_error': result.get('recovery_error')}))
    return int(result['status'] != 'passed')


if __name__ == '__main__':
    raise SystemExit(main())
