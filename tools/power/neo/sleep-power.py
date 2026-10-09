#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-2.0-only
"""Measure battery charge loss across an awake interval or one timed s2idle test.

Sleep measurements take endpoint samples only; no sampling timer is active
during system sleep. The result includes transition overhead, and converting
charge to Wh uses an approximation from endpoint voltage. No physical panel
power-down or counter calibration is implied by a successful measurement.
"""
import argparse
import fcntl
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys
import time

BATTERY = Path('/sys/class/power_supply/macsmc-battery')
NUMBERS = ('charge_counter', 'charge_now', 'energy_now', 'voltage_now',
           'current_now', 'power_now', 'capacity', 'temp')


def sample():
    start = time.clock_gettime(time.CLOCK_BOOTTIME)
    values = {key: int((BATTERY / key).read_text()) for key in NUMBERS}
    values['status'] = (BATTERY / 'status').read_text().strip()
    values['ac_online'] = int(Path('/sys/class/power_supply/macsmc-ac/online').read_text())
    values['boottime'] = (start + time.clock_gettime(time.CLOCK_BOOTTIME)) / 2
    values['read_seconds'] = time.clock_gettime(time.CLOCK_BOOTTIME) - start
    return values


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--mode', choices=('awake', 'sleep'), required=True)
    parser.add_argument('--seconds', type=int, required=True)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--display-off', action='store_true',
                        help='require and verify the neo-display-sleep PM hook')
    parser.add_argument('--screen-directory', type=Path,
                        help='screen-sleep control directory for visible before/after phases')
    args = parser.parse_args()
    if not 10 <= args.seconds <= 3600:
        parser.error('seconds must be in 10..3600')
    if (args.display_off or args.screen_directory) and args.mode != 'sleep':
        parser.error('display options require --mode sleep')
    args.output.mkdir(parents=True, exist_ok=False)
    lock = open('/run/neo-power-measure.lock', 'w')
    fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
    helper = Path(__file__).with_name('sleep-test.py')
    result = {'status': 'running', 'mode': args.mode, 'requested_seconds': args.seconds,
              'boot_id': Path('/proc/sys/kernel/random/boot_id').read_text().strip(),
              'kernel': os.uname().release,
              'kernel_notes_sha256': hashlib.sha256(Path('/sys/kernel/notes').read_bytes()).hexdigest(),
              'sleep_helper_sha256': hashlib.sha256(helper.read_bytes()).hexdigest(),
              'samples': [], 'energy_conversion': 'charge_now times mean endpoint voltage',
              'includes_suspend_resume_overhead': args.mode == 'sleep'}

    def save():
        tmp = args.output / 'result.tmp'
        tmp.write_text(json.dumps(result, indent=2) + '\n')
        tmp.replace(args.output / 'result.json')
        os.sync()

    save()
    try:
        before = sample()
        if before['status'] != 'Discharging' or before['ac_online']:
            raise RuntimeError('requires battery discharge without AC input')
        result['samples'].append(before)
        save()
        if args.mode == 'awake':
            deadline = time.monotonic() + args.seconds
            while time.monotonic() < deadline:
                time.sleep(min(1, max(0, deadline - time.monotonic())))
                result['samples'].append(sample())
        else:
            command = [sys.executable, str(helper), '--wake-after', str(args.seconds),
                       '--output', str(args.output / 'sleep')]
            if args.display_off:
                command.append('--display-off')
            if args.screen_directory:
                command += ['--screen-directory', str(args.screen_directory.resolve(strict=True))]
            with (args.output / 'sleep.log').open('w') as log:
                p = subprocess.run(command, stdout=log,
                                   stderr=subprocess.STDOUT)
            result['samples'].append(sample())
            result['sleep_returncode'] = p.returncode
            result['sleep_result'] = json.loads((args.output / 'sleep/result.json').read_text())
            if p.returncode:
                raise RuntimeError('sleep qualification failed; reject power estimate')
        after = result['samples'][-1]
        if Path('/proc/sys/kernel/random/boot_id').read_text().strip() != result['boot_id']:
            raise RuntimeError('boot ID changed')
        if any(s['ac_online'] or s['status'] != 'Discharging' for s in result['samples']):
            raise RuntimeError('external power or non-discharge state observed')
        dt = after['boottime'] - before['boottime']
        raw_counter_drop = before['charge_counter'] - after['charge_counter']
        dq = before['charge_now'] - after['charge_now']
        voltage = (before['voltage_now'] + after['voltage_now']) / 2e6
        if dt <= 0 or voltage <= 0 or raw_counter_drop <= 0:
            raise RuntimeError('invalid interval, voltage, or accumulated discharge')
        result['elapsed_seconds'] = dt
        result['charge_loss_uAh'] = dq
        result['raw_charge_counter_drop'] = raw_counter_drop
        result['raw_charge_counter_units_qualified'] = False
        result['mean_endpoint_voltage_V'] = voltage
        # Remaining capacity can rise after removing the display load. Keep
        # the signed delta, but do not turn gauge compensation into negative
        # consumption or discard the independently accumulated discharge.
        result['capacity_estimate_available'] = dq > 0
        result['estimated_energy_Wh'] = dq / 1e6 * voltage if dq > 0 else None
        result['estimated_average_W'] = (result['estimated_energy_Wh'] * 3600 / dt
                                         if dq > 0 else None)
        if dq <= 0:
            result['capacity_estimate_note'] = 'remaining-capacity gauge did not fall; use calibrated accumulated counter'
        result['nominal_energy_loss_Wh'] = (before['energy_now'] - after['energy_now']) / 1e6
        result['nominal_energy_average_W'] = result['nominal_energy_loss_Wh'] * 3600 / dt
        if args.mode == 'awake':
            integrated_uAh = 0
            integrated_Wh = 0
            for a, b in zip(result['samples'], result['samples'][1:]):
                hours = (b['boottime'] - a['boottime']) / 3600
                integrated_uAh -= (a['current_now'] + b['current_now']) / 2 * hours
                integrated_Wh -= (a['power_now'] + b['power_now']) / 2e6 * hours
            result['sampled_current_integral_uAh'] = integrated_uAh
            result['counter_to_current_ratio'] = raw_counter_drop / integrated_uAh
            result['remaining_charge_to_current_ratio'] = dq / integrated_uAh
            result['sampled_average_power_W'] = integrated_Wh * 3600 / dt
        result['status'] = 'measured'
    except Exception as error:
        result.update(status='error', error=str(error))
    finally:
        save()
    print(json.dumps({k: v for k, v in result.items() if k not in ('samples', 'sleep_result')}, indent=2))
    return int(result['status'] != 'measured')


if __name__ == '__main__':
    raise SystemExit(main())
