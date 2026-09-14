#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-2.0-only
"""Measure one manual WLED trial while awake; this is not a sleep test."""
import argparse
import json
from pathlib import Path
import statistics
import time

p = argparse.ArgumentParser(description=__doc__)
p.add_argument('--output', type=Path, required=True)
a = p.parse_args()
a.output.mkdir(parents=True, exist_ok=False)
level = Path('/sys/bus/platform/devices/300200000.backlight/level')
battery = Path('/sys/class/power_supply/macsmc-battery')
if int(level.read_text()) != -1:
    raise RuntimeError('requires a fresh trial driver, with no command issued')
result = dict(boot_id=Path('/proc/sys/kernel/random/boot_id').read_text().strip(),
              restoration_level=1024, status='running', stages=[])


def save():
    (a.output / 'result.json').write_text(json.dumps(result, indent=2) + '\n')


def sample(name, seconds):
    stage = dict(name=name, started=time.monotonic(), samples=[])
    result['stages'].append(stage)
    for _ in range(seconds):
        stage['samples'].append(dict(monotonic=time.monotonic(),
            power_uW=int((battery / 'power_now').read_text()),
            charge_uAh=int((battery / 'charge_now').read_text()),
            voltage_uV=int((battery / 'voltage_now').read_text())))
        save()
        time.sleep(1)
    stage['mean_W_after_5s'] = -statistics.mean(
        x['power_uW'] for x in stage['samples'][5:]) / 1e6
    print(name, stage['mean_W_after_5s'], flush=True)
    save()


try:
    sample('inherited', 20)
    for value, name, seconds in ((1024, 'level-1024', 20),
                                  (0, 'level-zero', 30),
                                  (1024, 'restored-1024', 20)):
        level.write_text(str(value) + '\n')
        sample(name, seconds)
    result['status'] = 'commands-completed'
finally:
    # The inherited peripheral brightness is unknown. This restores the
    # explicit trial level, not a claim to restore the original brightness.
    try:
        if int(level.read_text()) >= 0:
            level.write_text('1024\n')
    except Exception as exc:
        result['restore_error'] = str(exc)
        result['status'] = 'restore-failed'
    save()
