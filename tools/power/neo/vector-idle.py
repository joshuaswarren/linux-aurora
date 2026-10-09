#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-2.0-only
"""Exercise arm64 FP selftests across stopped-task idle intervals on one CPU.

Build fpsimd-test, ssve-test, za-test and zt-test from selftests/arm64/fp first.
SIGSTOP/SIGCONT retains streaming state without relying on a syscall ABI which
clears streaming mode. State usage deltas must be checked separately to show
that a particular CPU idle state was actually exercised, or use --idle-state
to require a positive usage delta during each test. A delta establishes that
the state was used during the test, not that every stopped interval entered it.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import signal
import subprocess
import time


def wait_stopped(process):
    deadline = time.monotonic() + 3
    while time.monotonic() < deadline:
        pid, status = os.waitpid(process.pid, os.WNOHANG | os.WUNTRACED)
        if pid:
            if not os.WIFSTOPPED(status):
                raise RuntimeError(f'test exited while stopping: wait status {status}')
            return
        time.sleep(0.005)
    raise TimeoutError('test did not stop within three seconds')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--cpu', type=int, required=True)
    parser.add_argument('--tests', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--cycles', type=int, default=20)
    parser.add_argument('--idle-state', type=int,
                        help='require this cpuidle state to be exercised by each test')
    args = parser.parse_args()
    if args.cpu not in os.sched_getaffinity(0) or args.cycles <= 0:
        parser.error('select an available CPU and a positive cycle count')
    state = None
    if args.idle_state is not None:
        if args.idle_state < 0:
            parser.error('idle state must be nonnegative')
        state = Path(f'/sys/devices/system/cpu/cpu{args.cpu}/cpuidle/state{args.idle_state}')
        if not state.is_dir() or (state / 'disable').read_text().strip() != '0':
            parser.error('the selected cpuidle state must exist and be enabled')
    args.output.mkdir(parents=True, exist_ok=False)
    results = {'boot_id': Path('/proc/sys/kernel/random/boot_id').read_text().strip(),
               'kernel': Path('/proc/version').read_text().strip(),
               'cpu': args.cpu, 'tests': []}
    if state is not None:
        results['idle_state'] = {'index': args.idle_state,
                                 'name': (state / 'name').read_text().strip()}
    failed = False
    for name in ('fpsimd-test', 'ssve-test', 'za-test', 'zt-test'):
        binary = (args.tests / name).resolve()
        record = {'name': name, 'sha256': hashlib.sha256(binary.read_bytes()).hexdigest(),
                  'cycles': 0, 'status': 'running'}
        if state is not None:
            record['idle_usage_before'] = int((state / 'usage').read_text())
        results['tests'].append(record)
        result_path = args.output / 'result.json'
        result_path.write_text(json.dumps(results, indent=2) + '\n')
        log_path = args.output / (name + '.log')
        with log_path.open('wb') as log:
            process = subprocess.Popen(['taskset', '-c', str(args.cpu), str(binary)],
                                       stdout=log, stderr=subprocess.STDOUT,
                                       start_new_session=True)
            try:
                deadline = time.monotonic() + 3
                while 'PID:' not in log_path.read_text(errors='replace'):
                    if process.poll() is not None or time.monotonic() > deadline:
                        raise RuntimeError('test did not finish startup')
                    time.sleep(0.01)
                for _ in range(args.cycles):
                    process.send_signal(signal.SIGSTOP)
                    wait_stopped(process)
                    time.sleep(0.01)
                    process.send_signal(signal.SIGCONT)
                    time.sleep(0.01)
                    if process.poll() is not None:
                        raise RuntimeError('test exited during idle qualification')
                    record['cycles'] += 1
                process.terminate()
                record['returncode'] = process.wait(timeout=3)
                output = log_path.read_text(errors='replace')
                record['status'] = ('pass' if record['returncode'] == 0 and
                                    'no error' in output else 'fail')
                if state is not None:
                    record['idle_usage_after'] = int((state / 'usage').read_text())
                    record['idle_entries'] = (record['idle_usage_after'] -
                                              record['idle_usage_before'])
                    if record['idle_entries'] <= 0:
                        record.update(status='fail', error='selected idle state was not exercised')
            except (OSError, RuntimeError, subprocess.TimeoutExpired) as error:
                record.update(status='fail', error=str(error))
            finally:
                if process.poll() is None:
                    process.kill()
                    try:
                        process.wait(timeout=3)
                    except subprocess.TimeoutExpired:
                        record.update(status='stuck', error='test did not exit after SIGKILL')
        result_path.write_text(json.dumps(results, indent=2) + '\n')
        print(json.dumps(record), flush=True)
        if record['status'] != 'pass':
            failed = True
            break
    return int(failed)


if __name__ == '__main__':
    raise SystemExit(main())
