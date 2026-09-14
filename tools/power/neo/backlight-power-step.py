#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-2.0-only
"""Measure awake J700 backlight power using the bounded pmBL trial module."""
import argparse
import hashlib
import json
from pathlib import Path
import statistics
import subprocess
import time

p = argparse.ArgumentParser(description=__doc__)
p.add_argument('--module', type=Path, required=True)
p.add_argument('--output', type=Path, required=True)
a = p.parse_args()
battery = Path('/sys/class/power_supply/macsmc-battery')
state = Path('/sys/module/neo_backlight_trial/parameters/state')
if state.exists():
    raise RuntimeError('trial module already loaded')
a.output.mkdir(parents=True, exist_ok=False)
result = dict(status='running', stages=[],
              boot_id=Path('/proc/sys/kernel/random/boot_id').read_text().strip(),
              kernel=subprocess.check_output(['uname', '-a'], text=True).strip(),
              module_sha256=hashlib.sha256(a.module.read_bytes()).hexdigest())


def save():
    (a.output / 'result.json').write_text(json.dumps(result, indent=2) + '\n')


def sample(name, seconds, expected=None):
    stage = dict(name=name, started=time.monotonic(), samples=[])
    result['stages'].append(stage)
    deadline = stage['started'] + seconds
    while time.monotonic() < deadline:
        if expected is not None and int(state.read_text()) != expected:
            raise RuntimeError('unexpected backlight command state')
        if (battery / 'status').read_text().strip() != 'Discharging':
            raise RuntimeError('requires uninterrupted battery discharge')
        stage['samples'].append(dict(monotonic=time.monotonic(),
            power_uW=int((battery / 'power_now').read_text()),
            charge_uAh=int((battery / 'charge_now').read_text()),
            voltage_uV=int((battery / 'voltage_now').read_text())))
        save()
        time.sleep(0.5)
    settled = [x for x in stage['samples'] if x['monotonic'] - stage['started'] >= 5]
    stage['mean_W_after_5s'] = -statistics.mean(x['power_uW'] for x in settled) / 1e6
    print(name, stage['mean_W_after_5s'], flush=True)
    save()


loaded = False
try:
    sample('before', 30)
    subprocess.run(['insmod', str(a.module), 'seconds=60'], check=True)
    loaded = True
    sample('backlight-off-requested', 59, 0)
    deadline = time.monotonic() + 5
    while int(state.read_text()) == 0 and time.monotonic() < deadline:
        time.sleep(0.1)
    if int(state.read_text()) != 1:
        raise RuntimeError('automatic restore was not acknowledged')
    sample('restored', 30, 1)
    before, off, after = [x['mean_W_after_5s'] for x in result['stages']]
    result['reduction_W_vs_mean_on'] = (before + after) / 2 - off
    result['status'] = 'passed'
except BaseException as exc:
    result['status'] = 'failed'
    result['error'] = repr(exc)
    raise
finally:
    if loaded:
        result['module_remove_returncode'] = subprocess.run(['rmmod', 'neo_backlight_trial']).returncode
        if result['module_remove_returncode']:
            result['status'] = 'cleanup-failed'
    (a.output / 'kernel.log').write_text(subprocess.check_output(['dmesg'], text=True))
    save()
