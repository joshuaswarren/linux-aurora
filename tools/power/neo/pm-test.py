#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-2.0-only
"""Capture a bounded PM test stage on Neo; never request a real sleep.

Run as root with --stage freezer, devices or platform and a new --output directory.
The platform stage includes late/noirq device callbacks, then returns before the
s2idle loop. It does not qualify a wake source or a physical sleep state.
Device callbacks still perform real transitions and can hang: the kernel's test
delay is not a watchdog. A run left at status=running is not a successful resume.
"""
import argparse
import fcntl
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import time


def read(path):
    try:
        return Path(path).read_text().strip()
    except OSError as error:
        return {"error": str(error)}


def command(args):
    result = subprocess.run(args, capture_output=True, text=True, timeout=20)
    return {"returncode": result.returncode, "stdout": result.stdout,
            "stderr": result.stderr}


def write_control(path, value):
    # Buffered text I/O can retry a failed flush while closing the stream,
    # inadvertently issuing a second suspend request after a driver veto.
    fd = os.open(path, os.O_WRONLY)
    try:
        data = (value + '\n').encode()
        if os.write(fd, data) != len(data):
            raise OSError('short sysfs write')
    finally:
        os.close(fd)


def snapshot():
    paths = ["/proc/sys/kernel/random/boot_id", "/proc/version", "/proc/cmdline",
             "/proc/interrupts", "/proc/softirqs", "/proc/stat",
             "/sys/power/state", "/sys/power/mem_sleep", "/sys/power/pm_test",
             "/sys/power/pm_async", "/sys/kernel/debug/wakeup_sources",
             "/sys/module/printk/parameters/console_suspend",
             "/sys/kernel/debug/apple-pmp-v2/status",
             "/sys/kernel/debug/apple-pmp-v2/thermal_status",
             "/sys/bus/platform/devices/412e00000.neo-dcp-retained/status",
             "/sys/bus/platform/devices/412e00000.neo-dcp-retained/epic_status",
             "/sys/kernel/debug/neo-sleep-irqs/stats",
             "/sys/kernel/debug/neo-pmgr-snapshot/stats",
             "/sys/kernel/debug/neo-smc-power/raw",
             "/sys/kernel/debug/pm_genpd/pm_genpd_summary",
             "/sys/module/apple_dart/parameters/pmp_s2idle",
             "/sys/module/apple_dart/parameters/inherited_s2idle",
             "/sys/module/mt7932_fullmac/parameters/retained_s2idle",
             "/sys/module/pmgr_pwrstate/parameters/retain_s2idle",
             "/sys/module/pmp_v2/parameters/retained_s2idle"]
    paths.extend('/sys/module/pmp_v2/parameters/' + name for name in
                 ('rtkit_idle_s2idle', 'rtkit_stop_s2idle'))
    for pattern in ("/sys/power/suspend_stats/*", "/sys/power/suspend_stats/last_hw_sleep",
                    "/sys/class/typec/port*/data_role", "/sys/class/typec/port*/power_role",
                    "/sys/class/typec/port*/power_operation_mode",
                    "/sys/class/power_supply/macsmc-ac/online",
                    "/sys/class/power_supply/macsmc-battery/status",
                    "/sys/devices/system/cpu/cpufreq/policy*/scaling_cur_freq",
                    "/sys/class/thermal/thermal_zone*/mode",
                    "/sys/devices/system/cpu/cpu*/cpuidle/state*/usage",
                    "/sys/devices/system/cpu/cpu*/cpuidle/state*/s2idle/usage",
                    "/sys/module/neo_display_sleep/parameters/*",
                    "/sys/module/neo_dcp_retained/parameters/*",
                    "/sys/module/neo_media_sleep/parameters/*",
                    "/sys/module/phy_apple_atc/parameters/usb2_sleep*",
                    "/sys/module/dwc3_apple/parameters/usb2_s2idle",
                    "/sys/module/pmgr_pwrstate/parameters/usb_clock_*",
                    "/sys/module/mt7932_fullmac/parameters/*",
                    "/sys/bus/platform/devices/macsmc-input/power/wakeup"):
        paths.extend(str(p) for p in Path('/').glob(pattern.lstrip('/')) if p.is_file())
    capture = {"files": {p: read(p) for p in paths},
            "kernel_notes_sha256": hashlib.sha256(Path('/sys/kernel/notes').read_bytes()).hexdigest(),
            "dmesg": command(['dmesg']), "input": read('/proc/bus/input/devices')}
    capture['typec_partners'] = sorted(p.name for p in Path('/sys/class/typec').glob('*-partner'))
    return capture


def check_firmware_health(capture):
    clock = '/sys/module/pmgr_pwrstate/parameters/usb_clock_'
    if any(capture['files'].get(clock + k, 'N') != 'N' for k in ('failed', 'pending')):
        raise RuntimeError('USB clock transition failed or was not restored')
    phy = '/sys/module/phy_apple_atc/parameters/'
    if capture['files'].get(phy + 'usb2_sleep_failed', 'N') != 'N':
        raise RuntimeError('USB2 PHY sleep failure is latched')
    counts = [capture['files'].get(phy + 'usb2_sleep_' + k) for k in ('offs', 'ons')]
    if counts != [None, None] and counts[0] != counts[1]:
        raise RuntimeError('USB2 PHY sleep transition has not been restored')
    media = '/sys/module/neo_media_sleep/parameters/'
    if capture['files'].get(media + 'failed', 'N') != 'N' or capture['files'].get(media + 'pending', '0') != '0':
        raise RuntimeError('media sleep transition failed or was not fully restored')
    for name, path in (
        ('PMP', '/sys/kernel/debug/apple-pmp-v2/status'),
        ('DCP', '/sys/bus/platform/devices/412e00000.neo-dcp-retained/status'),
    ):
        status = capture['files'].get(path)
        if isinstance(status, str):
            failed = re.search(r'\bfailed=(\d+)\b', status)
            if failed and int(failed[1]):
                raise RuntimeError(f'{name} firmware epoch failed; a returned PM write is not successful recovery')
            if name == 'DCP' and re.search(r'\b(?:touched|clocks_touched|pm_panel_touched)=[1-9]', status):
                raise RuntimeError('DCP firmware transition has not been restored')


SUSPEND_STATS = '/sys/power/suspend_stats/'
SUSPEND_FAILURE_COUNTERS = ('fail', 'failed_freeze', 'failed_prepare', 'failed_suspend',
                            'failed_suspend_late', 'failed_suspend_noirq', 'failed_resume',
                            'failed_resume_early', 'failed_resume_noirq')


def check_suspend_stats(before, after):
    # A returned write to /sys/power/state only means the kernel's suspend
    # loop completed; a device that failed to resume still increments these.
    summary = {'deltas': {}, 'last_failed': {}}
    for name in SUSPEND_FAILURE_COUNTERS:
        path = SUSPEND_STATS + name
        values = [capture['files'].get(path) for capture in (before, after)]
        if values == [None, None]:
            continue
        for value in values:
            if not isinstance(value, str):
                raise RuntimeError(f'{path} is unreadable or missing on one side: {value!r}')
        summary['deltas'][name] = int(values[1]) - int(values[0])
    for name in ('last_failed_dev', 'last_failed_errno', 'last_failed_step'):
        summary['last_failed'][name] = after['files'].get(SUSPEND_STATS + name)
    increased = {name: delta for name, delta in summary['deltas'].items() if delta > 0}
    if increased:
        details = ', '.join(f'{name} +{delta}' for name, delta in increased.items())
        raise RuntimeError(f'suspend_stats recorded a failure ({details}); '
                           f'last_failed_dev={summary["last_failed"]["last_failed_dev"]!r} '
                           f'last_failed_errno={summary["last_failed"]["last_failed_errno"]!r} '
                           f'last_failed_step={summary["last_failed"]["last_failed_step"]!r}; '
                           'a returned PM write is not a successful resume')
    return summary


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--stage', required=True, choices=('freezer', 'devices', 'platform'))
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    if os.geteuid():
        parser.error('root is required')
    if not Path('/sys/power/pm_test').exists():
        parser.error('CONFIG_PM_DEBUG is required')
    lock = open('/run/neo-pm-test.lock', 'w')
    fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
    pm_test = Path('/sys/power/pm_test')
    pm_async = Path('/sys/power/pm_async')
    selected = re.search(r'\[([^]]+)\]', pm_test.read_text()).group(1)
    if selected != 'none':
        parser.error('another PM test is selected; restore pm_test=none first')
    old_async = pm_async.read_text().strip()
    args.output.mkdir(parents=True, exist_ok=False)
    result_path = args.output / 'result.json'
    result = {'stage': args.stage, 'state': 'freeze', 'status': 'running',
              'before': snapshot()}

    def save():
        temporary = result_path.with_suffix('.tmp')
        temporary.write_text(json.dumps(result, indent=2) + '\n')
        temporary.replace(result_path)
        os.sync()

    save()
    try:
        check_firmware_health(result['before'])
    except RuntimeError as error:
        result.update(status='error', firmware_error=str(error))
        save()
        print(str(error))
        return 1
    try:
        # Serialize callbacks to make the first failing device unambiguous.
        write_control(pm_async, '0')
        write_control(pm_test, args.stage)
        start = time.monotonic()
        try:
            write_control('/sys/power/state', 'freeze')
            result['status'] = 'returned'
        except OSError as error:
            result.update(status='error', errno=error.errno, error=str(error))
        result['elapsed_seconds'] = time.monotonic() - start
    finally:
        write_control(pm_test, selected)
        write_control(pm_async, old_async)
        result['after'] = snapshot()
        try:
            check_firmware_health(result['after'])
        except RuntimeError as error:
            result.update(status='error', firmware_error=str(error))
        try:
            result['suspend_stats_check'] = check_suspend_stats(result['before'], result['after'])
        except RuntimeError as error:
            result.update(status='error', suspend_stats_error=str(error))
        save()
    print(json.dumps({k: v for k, v in result.items() if k not in ('before', 'after')}))
    return 0 if result['status'] == 'returned' else 1


if __name__ == '__main__':
    raise SystemExit(main())
