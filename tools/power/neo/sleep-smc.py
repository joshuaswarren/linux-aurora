#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-2.0-only
"""One timed RTC s2idle cycle plus immediate post-resume SMC power readings.

Wraps sleep-test.py (which requires the qualified retained setup). Right after
the request returns it reads neo-smc-power's raw keys repeatedly: the first
readings reflect the SMC's last sleep-time values, which is a cross-check of the
battery accumulated counter, not an independent meter. Also computes the raw
counter power with a supplied --scale (raw units per uAh) when given.
"""
import argparse
import json
import math
import os
from pathlib import Path
import struct
import subprocess
import sys
import time

RAW = Path('/sys/kernel/debug/neo-smc-power/raw')
BATTERY = Path('/sys/class/power_supply/macsmc-battery')


def smc():
    values = {}
    for line in RAW.read_text().splitlines():
        key, bits, ret = line.split()
        value = struct.unpack('<f', struct.pack('<I', int(bits, 16)))[0]
        if not int(ret) and math.isfinite(value):
            values[key] = value
    return values


def battery():
    b = {k: int((BATTERY / k).read_text()) for k in
         ('charge_counter', 'charge_now', 'voltage_now', 'current_now', 'power_now')}
    b['status'] = (BATTERY / 'status').read_text().strip()
    b['ac_online'] = int(Path('/sys/class/power_supply/macsmc-ac/online').read_text())
    b['boottime'] = time.clock_gettime(time.CLOCK_BOOTTIME)
    return b


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument('--seconds', type=int, required=True)
    ap.add_argument('--output', type=Path, required=True)
    ap.add_argument('--scale', type=float, help='raw charge counter units per uAh')
    ap.add_argument('--no-display-off', action='store_true')
    args = ap.parse_args()
    args.output.mkdir(parents=True, exist_ok=False)
    result = {'status': 'running', 'seconds': args.seconds,
              'boot_id': Path('/proc/sys/kernel/random/boot_id').read_text().strip()}

    def save():
        tmp = args.output / 'result.tmp'
        tmp.write_text(json.dumps(result, indent=1) + '\n')
        tmp.replace(args.output / 'result.json')
        os.sync()

    result['smc_before'] = smc()
    result['battery_before'] = battery()
    if result['battery_before']['ac_online'] or result['battery_before']['status'] != 'Discharging':
        raise RuntimeError('measurement requires battery discharge with AC offline')
    save()
    cmd = [sys.executable, str(Path(__file__).with_name('sleep-test.py')), '--wake-after',
           str(args.seconds), '--output', str(args.output / 'sleep')]
    if not args.no_display_off:
        cmd.append('--display-off')
    with (args.output / 'sleep.log').open('w') as log:
        p = subprocess.run(cmd, stdout=log, stderr=subprocess.STDOUT)
    t0 = time.monotonic()
    # End the counter interval before the four seconds of awake SMC sampling.
    result['battery_after'] = battery()
    post = []
    for delay in (0, 0.3, 0.6, 1, 1.5, 2, 3, 4):
        time.sleep(max(0, delay - (time.monotonic() - t0)))
        post.append({'t': time.monotonic() - t0, 'smc': smc()})
    result['battery_after_smc_series'] = battery()
    result['smc_after'] = post
    result['sleep_returncode'] = p.returncode
    try:
        result['sleep_result'] = json.loads((args.output / 'sleep/result.json').read_text())
    except OSError as error:
        result['sleep_result'] = {'error': str(error)}
    b0, b1 = result['battery_before'], result['battery_after']
    dt = b1['boottime'] - b0['boottime']
    drop = b0['charge_counter'] - b1['charge_counter']
    volts = (b0['voltage_now'] + b1['voltage_now']) / 2e6
    result.update(elapsed_seconds=dt, raw_charge_counter_drop=drop, mean_endpoint_voltage_V=volts)
    sr = result['sleep_result']
    ok = (p.returncode == 0 and sr.get('status') == 'returned' and sr.get('all_cpus_entered_s2idle')
          and sr.get('timed_wake_observed') and sr.get('display_cycle_verified', True)
          and 'suspend_stats_error' not in sr)
    ok = (ok and Path('/proc/sys/kernel/random/boot_id').read_text().strip() == result['boot_id']
          and all(not b['ac_online'] and b['status'] == 'Discharging'
                  for b in (b0, b1, result['battery_after_smc_series'])))
    if ok and args.scale and math.isfinite(args.scale) and args.scale > 0 and drop > 0 and dt > 0:
        result['counter_W'] = drop / args.scale * volts * 3600 / dt / 1e6
    result['status'] = 'measured' if ok else 'error'
    save()
    first = post[0]['smc']
    print(json.dumps({'status': result['status'], 'elapsed': dt, 'counter_W': result.get('counter_W'),
                      'PSTR_first': first.get('PSTR'), 'PBat_first': first.get('PBat'),
                      'PPMC_first': first.get('PPMC'), 'PSTR_series': [round(x['smc'].get('PSTR', -1), 4) for x in post],
                      'boottime_elapsed': sr.get('boottime_elapsed_seconds'),
                      'cpus': sr.get('s2idle_entries_per_cpu')}))
    return 0 if ok else 1


if __name__ == '__main__':
    raise SystemExit(main())
