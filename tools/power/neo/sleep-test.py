#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-2.0-only
"""Request one real retained s2idle cycle on Neo, with a physical wake operator.

Use --check-only to verify prerequisites without sleeping. There is no qualified
RTC alarm or automatic timeout: a power-button wake or external reset is needed.
The saved result distinguishes a returned request from actual s2idle entries.
"""
import argparse
import fcntl
import importlib.util
import json
import os
from pathlib import Path
import time


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--check-only', action='store_true')
    args = parser.parse_args()
    if os.geteuid():
        parser.error('root is required')
    spec = importlib.util.spec_from_file_location('neo_pm', Path(__file__).with_name('pm-test.py'))
    pm = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(pm)
    lock = open('/run/neo-pm-test.lock', 'w')
    fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
    requirements = {
        '/sys/devices/system/cpu/online': '0-5',
        '/sys/devices/system/cpu/cpuidle/current_driver': 'apple_neo_idle',
        '/sys/module/cpuidle_apple/parameters/neo_cpu_pm': 'Y',
        '/sys/module/apple_dart/parameters/pmp_s2idle': 'Y',
        '/sys/module/apple_dart/parameters/inherited_s2idle': 'Y',
        '/sys/module/pmp_v2/parameters/retained_s2idle': 'Y',
        '/sys/module/pmgr_pwrstate/parameters/retain_s2idle': 'Y',
        '/sys/module/mt7932_fullmac/parameters/retained_s2idle': 'Y',
        '/sys/bus/platform/devices/macsmc-input/power/wakeup': 'enabled',
    }
    for cpu in range(6):
        requirements[f'/sys/devices/system/cpu/cpu{cpu}/cpuidle/state1/disable'] = '0'
    for path, value in requirements.items():
        if pm.read(path) != value:
            parser.error(f'{path} must be {value}')
    if '[none]' not in pm.read('/sys/power/pm_test') or '[s2idle]' not in pm.read('/sys/power/mem_sleep'):
        parser.error('require pm_test=none and mem_sleep=s2idle')
    args.output.mkdir(parents=True, exist_ok=False)
    result = {'status': 'ready' if args.check_only else 'running',
              'state': 'freeze', 'check_only': args.check_only, 'before': pm.snapshot()}
    usage_paths = [f'/sys/devices/system/cpu/cpu{c}/cpuidle/state1/s2idle/usage' for c in range(6)]
    before_usage = [int(pm.read(p)) for p in usage_paths]

    def save():
        temporary = args.output / 'result.tmp'
        temporary.write_text(json.dumps(result, indent=2) + '\n')
        temporary.replace(args.output / 'result.json')
        os.sync()

    save()
    if args.check_only:
        print('Prerequisites ready; no sleep requested. Physical wake remains unqualified.')
        return 0
    old_async = pm.read('/sys/power/pm_async')
    old_debug = pm.read('/sys/power/pm_debug_messages')
    try:
        pm.write_control('/sys/power/pm_async', '0')
        pm.write_control('/sys/power/pm_debug_messages', '1')
        # Serialize against intervening wake events. Do not retry a rejection.
        wake_count = Path('/sys/power/wakeup_count').read_text().strip()
        result['wakeup_count_before'] = wake_count
        start = time.clock_gettime(time.CLOCK_BOOTTIME)
        awake_start = time.monotonic()
        try:
            pm.write_control('/sys/power/wakeup_count', wake_count)
            pm.write_control('/sys/power/state', 'freeze')
            result['status'] = 'returned'
        except OSError as error:
            result.update(status='error', errno=error.errno, error=str(error))
        result['boottime_elapsed_seconds'] = time.clock_gettime(time.CLOCK_BOOTTIME) - start
        result['monotonic_elapsed_seconds'] = time.monotonic() - awake_start
    finally:
        pm.write_control('/sys/power/pm_async', old_async)
        pm.write_control('/sys/power/pm_debug_messages', old_debug)
        result['after'] = pm.snapshot()
        result['s2idle_entries_per_cpu'] = [int(pm.read(p)) - before_usage[c]
                                          for c, p in enumerate(usage_paths)]
        result['all_cpus_entered_s2idle'] = all(n > 0 for n in result['s2idle_entries_per_cpu'])
        save()
    print(json.dumps({k: v for k, v in result.items() if k not in ('before', 'after')}))
    return int(result['status'] != 'returned' or not result['all_cpus_entered_s2idle'])


if __name__ == '__main__':
    raise SystemExit(main())
