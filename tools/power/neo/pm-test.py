#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-2.0-only
"""Capture a bounded PM test stage on Neo; never request a real sleep.

Run as root with --stage freezer or --stage devices and a new --output directory.
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
             "/sys/power/state", "/sys/power/mem_sleep", "/sys/power/pm_test",
             "/sys/power/pm_async", "/sys/kernel/debug/wakeup_sources"]
    for pattern in ("/sys/power/suspend_stats/*", "/sys/power/suspend_stats/last_hw_sleep",
                    "/sys/devices/system/cpu/cpufreq/policy*/scaling_cur_freq",
                    "/sys/bus/platform/devices/macsmc-input/power/wakeup"):
        paths.extend(str(p) for p in Path('/').glob(pattern.lstrip('/')) if p.is_file())
    return {"files": {p: read(p) for p in paths},
            "kernel_notes_sha256": hashlib.sha256(Path('/sys/kernel/notes').read_bytes()).hexdigest(),
            "dmesg": command(['dmesg']), "input": read('/proc/bus/input/devices')}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--stage', required=True, choices=('freezer', 'devices'))
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
        save()
    print(json.dumps({k: v for k, v in result.items() if k not in ('before', 'after')}))
    return 0 if result['status'] == 'returned' else 1


if __name__ == '__main__':
    raise SystemExit(main())
