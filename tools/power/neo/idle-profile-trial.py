#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-2.0-only
"""Measure a bounded J700 awake idle profile and restore CPU/Wi-Fi policy."""
import argparse
import fcntl
import hashlib
import json
from pathlib import Path
import signal
import subprocess
import sys
import time


def command(*args):
    return subprocess.check_output(args, text=True).strip()


def frequencies():
    return {p.name: {k: int((p / k).read_text()) for k in
                    ('cpuinfo_min_freq', 'scaling_max_freq', 'scaling_cur_freq')}
            for p in sorted(Path('/sys/devices/system/cpu/cpufreq').glob('policy*'))}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--directory', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--idle-poll-ms', type=int, required=True)
    parser.add_argument('--power-save', choices=('on', 'off'), default='on')
    parser.add_argument('--seconds', type=int, default=120)
    args = parser.parse_args()
    if args.idle_poll_ms != 0 and not 100 <= args.idle_poll_ms <= 1000:
        parser.error('idle-poll-ms must be 0 or 100..1000')
    if not 30 <= args.seconds <= 500:
        parser.error('seconds must be in 30..500')
    directory = args.directory.resolve(strict=True)
    output = args.output.resolve()
    idle = Path('/sys/module/apple_smc_cpu_thermal/parameters/idle_poll_ms')
    helper = directory / 'wifi-idle-trial.py'
    lock = open('/run/neo-idle-profile.lock', 'w')
    fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
    previous_idle = idle.read_text().strip()
    previous_ps = command('iw', 'dev', 'wlan0', 'get', 'power_save')
    if previous_ps not in ('Power save: on', 'Power save: off'):
        raise RuntimeError('unrecognized station power policy')
    previous_ps = previous_ps.split()[-1]
    output.mkdir(parents=True, exist_ok=False)
    result = dict(status='running', boot_id=Path('/proc/sys/kernel/random/boot_id').read_text().strip(),
                  kernel=command('uname', '-a'), previous_idle_poll_ms=previous_idle,
                  previous_power_save=previous_ps, idle_poll_ms=args.idle_poll_ms,
                  power_save=args.power_save, seconds=args.seconds,
                  helper_sha256=hashlib.sha256(helper.read_bytes()).hexdigest())

    def save():
        (output / 'profile.json').write_text(json.dumps(result, indent=2) + '\n')

    def check_frequencies():
        values = frequencies()
        if len(values) != 2:
            raise RuntimeError('expected two J700 CPU frequency policies')
        if args.idle_poll_ms and any(v['scaling_max_freq'] != v['cpuinfo_min_freq'] or
                                    v['scaling_cur_freq'] != v['cpuinfo_min_freq']
                                    for v in values.values()):
            raise RuntimeError('minimum-frequency idle caps not effective')
        return values

    def interrupted(signum, frame):
        raise RuntimeError(f'interrupted by signal {signum}')

    signal.signal(signal.SIGTERM, interrupted)
    save()
    try:
        idle.write_text(str(args.idle_poll_ms))
        command('iw', 'dev', 'wlan0', 'set', 'power_save', args.power_save)
        time.sleep(1)
        result['frequencies_before'] = check_frequencies()
        result['interval_start_boottime'] = time.clock_gettime(time.CLOCK_BOOTTIME)
        (output / 'interrupts-before').write_text(Path('/proc/interrupts').read_text())
        save()
        with (output / 'trial.log').open('w') as log:
            subprocess.run([sys.executable, str(helper), '--directory', str(directory),
                            '--output', str(output / 'measurement'), '--seconds', str(args.seconds)],
                           stdout=log, stderr=subprocess.STDOUT, check=True)
        (output / 'interrupts-after').write_text(Path('/proc/interrupts').read_text())
        result['interval_end_boottime'] = time.clock_gettime(time.CLOCK_BOOTTIME)
        result['frequencies_after'] = check_frequencies()
        result['power_save_after'] = command('iw', 'dev', 'wlan0', 'get', 'power_save')
        if result['power_save_after'] != 'Power save: ' + args.power_save:
            raise RuntimeError('station power policy changed during measurement')
        if idle.read_text().strip() != str(args.idle_poll_ms):
            raise RuntimeError('CPU idle policy changed during measurement')
        result['sampled_average_power_W'] = json.loads(
            (output / 'measurement/power/result.json').read_text())['sampled_average_power_W']
        result['gateway_probe'] = command('ping', '-c', '3', '-W', '3', '10.129.154.1')
        result['status'] = 'measured'
    except BaseException as exc:
        result.update(status='failed', error=repr(exc))
        raise
    finally:
        # Try both restorations even if one fails. The child independently
        # bounds LCD/backlight off time, including on parent termination.
        for name, restore in (
                ('idle_poll_ms', lambda: idle.write_text(previous_idle)),
                ('power_save', lambda: command('iw', 'dev', 'wlan0', 'set', 'power_save', previous_ps))):
            try:
                restore()
            except Exception as exc:
                result.update(status='restore-failed')
                result[name + '_restore_error'] = repr(exc)
        try:
            result['restored_idle_poll_ms'] = idle.read_text().strip()
            result['restored_power_save'] = command('iw', 'dev', 'wlan0', 'get', 'power_save')
            if (result['restored_idle_poll_ms'] != previous_idle or
                    result['restored_power_save'] != 'Power save: ' + previous_ps):
                result['status'] = 'restore-failed'
        except Exception as exc:
            result.update(status='restore-failed', restore_check_error=repr(exc))
        save()
    print(json.dumps(result, indent=2))
    return int(result['status'] != 'measured')


if __name__ == '__main__':
    sys.exit(main())
