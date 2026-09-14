#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-2.0-only
"""Measure an awake J700 LCD off/on cycle, retaining battery samples."""
import argparse
import hashlib
import json
from pathlib import Path
import statistics
import subprocess
import time

p = argparse.ArgumentParser(description=__doc__)
p.add_argument('--helper', type=Path, required=True)
p.add_argument('--output', type=Path, required=True)
p.add_argument('--seconds', type=int, default=60, choices=range(10, 601))
a = p.parse_args()
battery = Path('/sys/class/power_supply/macsmc-battery')
if (battery / 'status').read_text().strip() != 'Discharging':
    raise RuntimeError('requires battery discharge')
a.output.mkdir(parents=True, exist_ok=False)
result = dict(boot_id=Path('/proc/sys/kernel/random/boot_id').read_text().strip(),
              kernel=subprocess.check_output(['uname', '-a'], text=True).strip(),
              helper_sha256=hashlib.sha256(a.helper.read_bytes()).hexdigest(),
              seconds=a.seconds, status='running', stages=[])


def save():
    (a.output / 'result.json').write_text(json.dumps(result, indent=2) + '\n')


def sample(name, seconds, child=None):
    stage = dict(name=name, started=time.monotonic(), samples=[])
    result['stages'].append(stage)
    deadline = stage['started'] + seconds
    while time.monotonic() < deadline:
        if child is not None and child.poll() is not None:
            raise RuntimeError('panel cycle ended before off sampling completed')
        if (battery / 'status').read_text().strip() != 'Discharging':
            raise RuntimeError('battery is no longer discharging')
        stage['samples'].append(dict(monotonic=time.monotonic(),
            power_uW=int((battery / 'power_now').read_text()),
            charge_uAh=int((battery / 'charge_now').read_text()),
            voltage_uV=int((battery / 'voltage_now').read_text())))
        save()
        time.sleep(0.5)
    settled = [x for x in stage['samples']
               if x['monotonic'] - stage['started'] >= 5]
    stage['mean_W_after_5s'] = -statistics.mean(x['power_uW'] for x in settled) / 1e6
    stage['settled_samples'] = len(settled)
    print(name, stage['mean_W_after_5s'], flush=True)
    save()


child = None
try:
    result['preflight'] = subprocess.check_output([str(a.helper), '--read'], text=True)
    sample('before', 30)
    with (a.output / 'panel.log').open('w') as log:
        child = subprocess.Popen([str(a.helper), '--cycle', str(a.seconds)],
                                 stdout=log, stderr=subprocess.STDOUT)
        deadline = time.monotonic() + 5
        while 'off readback=0\n' not in (a.output / 'panel.log').read_text():
            if child.poll() is not None or time.monotonic() > deadline:
                raise RuntimeError('LCD enable did not go low')
            time.sleep(0.05)
        # Leave one second for scheduling/readback latency before automatic restore.
        sample('off', a.seconds - 1, child)
        result['helper_returncode'] = child.wait(timeout=5)
    if result['helper_returncode'] != 0 or 'restored=1\n' not in (a.output / 'panel.log').read_text():
        raise RuntimeError('LCD enable restoration failed')
    sample('restored', 30)
    before, off, restored = [x['mean_W_after_5s'] for x in result['stages']]
    result['reduction_W_vs_before'] = before - off
    result['reduction_W_vs_mean_on'] = (before + restored) / 2 - off
    result['status'] = 'passed'
except BaseException as exc:
    result['status'] = 'failed'
    result['error'] = repr(exc)
    raise
finally:
    if child is not None and child.poll() is None:
        child.terminate()
        try:
            child.wait(timeout=5)
        except subprocess.TimeoutExpired:
            result['restore_error'] = 'helper did not exit after SIGTERM; inspect panel.log'
    save()
