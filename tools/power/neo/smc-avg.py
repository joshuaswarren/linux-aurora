#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-2.0-only
"""Average selected SMC power floats from neo-smc-power's raw debugfs file.

Awake-only sampling; the first read after a resume reflects the SMC's last
sleep-time value, not a sleep average. Values are floats reported by the SMC,
not an external meter. Prints JSON with mean/min/max per key and the battery
endpoints.
"""
import argparse
import json
import math
from pathlib import Path
import struct
import time

RAW = Path('/sys/kernel/debug/neo-smc-power/raw')
BATTERY = Path('/sys/class/power_supply/macsmc-battery')


def read_keys():
    values = {}
    for line in RAW.read_text().splitlines():
        key, bits, ret = line.split()
        value = struct.unpack('<f', struct.pack('<I', int(bits, 16)))[0]
        if not int(ret) and math.isfinite(value):
            values[key] = value
    return values


def battery():
    return {k: (BATTERY / k).read_text().strip() for k in
            ('status', 'power_now', 'current_now', 'voltage_now', 'charge_counter', 'charge_now')}


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument('--seconds', type=float, default=20)
    ap.add_argument('--interval', type=float, default=0.5)
    ap.add_argument('--keys', default='PSTR,PBat,PPMC,PPSM,PP0b,PP1l,PP2b,PP3b,PP3l,PPcl,PPhl')
    ap.add_argument('--first', action='store_true', help='print one immediate sample only')
    args = ap.parse_args()
    keys = args.keys.split(',')
    samples = []
    t0 = time.monotonic()
    bat0 = battery()
    while True:
        samples.append((time.clock_gettime(time.CLOCK_BOOTTIME), read_keys()))
        if args.first or time.monotonic() - t0 >= args.seconds:
            break
        time.sleep(args.interval)
    result = {'samples': len(samples), 'seconds': time.monotonic() - t0,
              'battery_before': bat0, 'battery_after': battery(), 'keys': {}}
    for k in keys:
        vals = [s[1][k] for s in samples if k in s[1]]
        if vals:
            result['keys'][k] = {'mean': sum(vals) / len(vals), 'min': min(vals), 'max': max(vals),
                                 'first': vals[0]}
    print(json.dumps(result))


if __name__ == '__main__':
    main()
