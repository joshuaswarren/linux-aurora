#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-2.0-only
"""Keep all four arm64 vector selftests on each Neo CPU across one PM transition.

devices/platform stop before sleep; s2idle uses a qualified 60-second RTC alarm.
The userspace timeout cannot recover a hung kernel callback or frozen task.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys
import time


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--tests', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--stage', choices=('devices', 'platform', 's2idle'), required=True)
    args = parser.parse_args()
    if os.geteuid() or sorted(os.sched_getaffinity(0)) != list(range(6)):
        parser.error('run as root with all six Neo CPUs available')
    args.output.mkdir(parents=True, exist_ok=False)
    result = {'status': 'running', 'stage': args.stage, 'tests': [],
              'boot_id': Path('/proc/sys/kernel/random/boot_id').read_text().strip()}
    processes = []

    def save():
        temporary = args.output / 'result.tmp'
        temporary.write_text(json.dumps(result, indent=2) + '\n')
        temporary.replace(args.output / 'result.json')

    save()
    try:
        for cpu in range(6):
            for name in ('fpsimd-test', 'ssve-test', 'za-test', 'zt-test'):
                binary = (args.tests / name).resolve()
                record = {'cpu': cpu, 'name': name, 'status': 'running',
                          'sha256': hashlib.sha256(binary.read_bytes()).hexdigest()}
                result['tests'].append(record)
                path = args.output / f'cpu{cpu}-{name}.log'
                with path.open('wb') as log:
                    proc = subprocess.Popen(['taskset', '-c', str(cpu), str(binary)],
                                            stdout=log, stderr=subprocess.STDOUT,
                                            start_new_session=True)
                processes.append((proc, record, path))
                deadline = time.monotonic() + 5
                while 'PID:' not in path.read_text(errors='replace'):
                    if proc.poll() is not None or time.monotonic() > deadline:
                        raise RuntimeError(f'CPU{cpu} {name} did not start')
                    time.sleep(0.01)
        save()
        os.sync()
        if args.stage == 's2idle':
            helper = Path(__file__).with_name('sleep-test.py')
            options = ['--wake-after', '60']
        else:
            helper = Path(__file__).with_name('pm-test.py')
            options = ['--stage', args.stage]
        with (args.output / 'pm-stage.log').open('wb') as log:
            test = subprocess.run([sys.executable, str(helper), *options,
                                   '--output', str(args.output / 'pm-stage')],
                                  stdout=log, stderr=subprocess.STDOUT, timeout=90)
        result['pm_returncode'] = test.returncode
        result['status'] = 'pass' if test.returncode == 0 else 'fail'
        stage = json.loads((args.output / 'pm-stage' / 'result.json').read_text())
        for name in ('failed_resume', 'failed_resume_early', 'failed_resume_noirq'):
            key = '/sys/power/suspend_stats/' + name
            if int(stage['after']['files'][key]) != int(stage['before']['files'][key]):
                raise RuntimeError(f'PM stage reported {name}')
        for proc, record, path in processes:
            if proc.poll() is not None:
                raise RuntimeError(f"CPU{record['cpu']} {record['name']} exited before validation")
            proc.terminate()
            record['returncode'] = proc.wait(timeout=5)
            record['status'] = ('pass' if record['returncode'] == 0 and
                                'no error' in path.read_text(errors='replace') else 'fail')
            if record['status'] != 'pass':
                result['status'] = 'fail'
    except (OSError, RuntimeError, subprocess.TimeoutExpired) as error:
        result.update(status='fail', error=str(error))
    finally:
        for proc, record, _ in processes:
            if proc.poll() is None:
                proc.kill()
                try:
                    proc.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    record.update(status='stuck')
                    result['status'] = 'fail'
        save()
        os.sync()
    print(json.dumps(result), flush=True)
    return int(result['status'] != 'pass')


if __name__ == '__main__':
    raise SystemExit(main())
