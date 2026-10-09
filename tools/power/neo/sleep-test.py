#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-2.0-only
"""Request one real retained s2idle cycle on Neo.

Use --check-only to verify prerequisites without sleeping. --wake-after arms
the experimental PMU RTC; without it a physical wake operator is required.
An RTC alarm cannot recover a hung suspend/resume callback; retain external reset.
The saved result distinguishes a returned request from actual s2idle entries.
"""
import argparse
import fcntl
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import time


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--check-only', action='store_true')
    parser.add_argument('--wake-after', type=int, metavar='SECONDS')
    parser.add_argument('--screen-directory', type=Path,
                        help='screen-sleep window control directory (explicit visual test)')
    parser.add_argument('--display-off', action='store_true',
                        help='require the qualified neo-display-sleep PM hook and verify restoration')
    args = parser.parse_args()
    if os.geteuid():
        parser.error('root is required')
    if args.wake_after is not None and not 5 <= args.wake_after <= 3600:
        parser.error('--wake-after must be between 5 and 3600 seconds')
    if args.display_off and args.wake_after is None:
        parser.error('--display-off requires a timed wake')
    screen_delay = 2 if args.screen_directory else 0
    if args.wake_after is not None and args.wake_after + screen_delay > 3600:
        parser.error('sleep plus the screen prelude must fit the one-hour RTC limit')
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
    rtc = Path('/sys/class/rtc/rtc0')
    if args.wake_after is not None:
        if not pm.read(rtc / 'name').startswith('apple-pmu-rtc '):
            parser.error('--wake-after requires the experimental PMU RTC')
        if pm.read(rtc / 'device/power/wakeup') != 'enabled':
            parser.error('PMU RTC wakeup must be enabled')
        if pm.read(rtc / 'wakealarm'):
            parser.error('an RTC alarm is already armed')
    args.output.mkdir(parents=True, exist_ok=False)
    result = {'status': 'ready' if args.check_only else 'running',
              'state': 'freeze', 'check_only': args.check_only, 'wake_after': args.wake_after,
              'before': pm.snapshot()}
    display = Path('/sys/module/neo_display_sleep/parameters')

    def display_snapshot():
        return {key: int((display / key).read_text())
                for key in ('state', 'off_count', 'on_count')}

    if args.display_off:
        if not Path('/sys/bus/platform/devices/neo-display-sleep/driver').exists():
            parser.error('neo-display-sleep must be bound')
        result['display_before'] = display_snapshot()
        if result['display_before']['state'] != 1:
            parser.error('display must start in its restored state')
    usage_paths = [f'/sys/devices/system/cpu/cpu{c}/cpuidle/state1/s2idle/usage' for c in range(6)]
    before_usage = [int(pm.read(p)) for p in usage_paths]

    def rtc_command(*options):
        p = subprocess.run(['rtcwake', '-d', '/dev/rtc0', *options], text=True,
                           capture_output=True, timeout=10)
        if p.returncode:
            raise RuntimeError(p.stdout + p.stderr)
        return p.stdout + p.stderr

    def rtc_events():
        return int(pm.read(rtc / 'device/power/wakeup_count'))

    def save():
        temporary = args.output / 'result.tmp'
        temporary.write_text(json.dumps(result, indent=2) + '\n')
        temporary.replace(args.output / 'result.json')
        os.sync()

    save()
    try:
        pm.check_firmware_health(result['before'])
    except RuntimeError as error:
        result.update(status='error', firmware_error=str(error))
        save()
        print(str(error))
        return 1
    if args.check_only:
        print('Prerequisites ready; no sleep or wake alarm requested.')
        return 0
    old_async = pm.read('/sys/power/pm_async')
    old_debug = pm.read('/sys/power/pm_debug_messages')
    alarm_attempted = False

    def screen_phase(number, name):
        marker = args.screen_directory / 'capture' / (name + '.ready')
        marker.unlink(missing_ok=True)
        (args.screen_directory / 'phase').write_text(str(number) + '\n')
        deadline = time.monotonic() + 5
        while time.monotonic() < deadline:
            try:
                return json.loads(marker.read_text())
            except (FileNotFoundError, json.JSONDecodeError):
                time.sleep(.01)
        raise RuntimeError(f'screen did not acknowledge {name}')

    try:
        if args.screen_directory:
            if (args.screen_directory / 'phase').read_text().strip() != '0':
                raise RuntimeError('screen must be showing BEFORE SLEEP')
            result['screen_before'] = json.loads(
                (args.screen_directory / 'capture/before.ready').read_text())
            os.kill(result['screen_before']['pid'], 0)
        pm.write_control('/sys/power/pm_async', '0')
        pm.write_control('/sys/power/pm_debug_messages', '1')
        if args.wake_after is not None:
            result['rtc_events_before'] = rtc_events()
            alarm_attempted = True
            result['rtc_arm_log'] = rtc_command('-m', 'no', '-s',
                                               str(args.wake_after + screen_delay))
            result['rtc_alarm_epoch'] = pm.read(rtc / 'wakealarm')
            if not result['rtc_alarm_epoch']:
                raise RuntimeError('RTC alarm did not remain armed')
            save()
        if args.screen_directory:
            result['screen_entering'] = screen_phase(1, 'entering')
            result['screen_delay_seconds'] = screen_delay
            time.sleep(max(0, result['screen_entering']['monotonic'] + screen_delay - time.monotonic()))
            result['screen_entering_to_suspend_seconds'] = (
                time.monotonic() - result['screen_entering']['monotonic'])
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
    except Exception as error:
        result.update(status='error', error=str(error))
    finally:
        if alarm_attempted:
            try:
                result['rtc_events_delta'] = rtc_events() - result['rtc_events_before']
                result['rtc_disarm_log'] = rtc_command('-m', 'disable')
            except Exception as error:
                result.update(status='error', rtc_cleanup_error=str(error))
        pm.write_control('/sys/power/pm_async', old_async)
        pm.write_control('/sys/power/pm_debug_messages', old_debug)
        result['after'] = pm.snapshot()
        try:
            pm.check_firmware_health(result['after'])
        except RuntimeError as error:
            result.update(status='error', firmware_error=str(error))
        try:
            result['suspend_stats_check'] = pm.check_suspend_stats(result['before'], result['after'])
        except RuntimeError as error:
            result.update(status='error', suspend_stats_error=str(error))
        if args.display_off:
            try:
                result['display_after'] = display_snapshot()
                before, after = result['display_before'], result['display_after']
                result['display_cycle_verified'] = (after['state'] == 1 and
                    after['off_count'] - before['off_count'] == 1 and
                    after['on_count'] - before['on_count'] == 1)
                if not result['display_cycle_verified']:
                    result.update(status='error', display_error='off/restore counters did not match')
            except Exception as error:
                result.update(status='error', display_error=str(error))
        result['s2idle_entries_per_cpu'] = [int(pm.read(p)) - before_usage[c]
                                          for c, p in enumerate(usage_paths)]
        result['all_cpus_entered_s2idle'] = all(n > 0 for n in result['s2idle_entries_per_cpu'])
        if args.wake_after is not None:
            result['timed_wake_observed'] = (result.get('rtc_events_delta', 0) > 0 and
                args.wake_after - 2 <= result.get('boottime_elapsed_seconds', 0) <= args.wake_after + 30)
        if args.screen_directory:
            try:
                if (result['status'] == 'returned' and result['all_cpus_entered_s2idle'] and
                        result.get('timed_wake_observed', True)):
                    result['screen_after'] = screen_phase(2, 'after')
                    for key in ('pid', 'context'):
                        if result['screen_after'][key] != result['screen_before'][key]:
                            raise RuntimeError(f'screen {key} changed across sleep')
                else:
                    screen_phase(4, 'failed')
            except Exception as error:
                result.update(status='error', screen_error=str(error))
        save()
    print(json.dumps({k: v for k, v in result.items() if k not in ('before', 'after')}))
    return int(result['status'] != 'returned' or not result['all_cpus_entered_s2idle'] or
               (args.wake_after is not None and not result['timed_wake_observed']))


if __name__ == '__main__':
    raise SystemExit(main())
