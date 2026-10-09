#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-2.0-only
"""Verify RTC alarm delivery, cancellation and replacement while J700 is awake."""
import argparse
import fcntl
import json
import os
from pathlib import Path
import select
import subprocess
import time


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    base = Path('/sys/class/rtc/rtc0')
    if not (base / 'name').read_text().startswith('apple-pmu-rtc '):
        parser.error('requires the experimental Apple PMU RTC')
    lock = open('/run/neo-pm-test.lock', 'w')
    fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
    if (base / 'wakealarm').read_text().strip():
        parser.error('an alarm is already armed')
    args.output.mkdir(parents=True, exist_ok=False)
    result = {'status': 'running', 'boot_id': Path('/proc/sys/kernel/random/boot_id').read_text().strip(),
              'kernel': os.uname().release, 'tests': []}

    def save():
        (args.output / 'result.json').write_text(json.dumps(result, indent=2) + '\n')
        os.sync()

    def command(*options):
        p = subprocess.run(['rtcwake', '-d', '/dev/rtc0', *options],
                           text=True, capture_output=True, timeout=20)
        if p.returncode:
            raise RuntimeError(p.stdout + p.stderr)
        return p.stdout + p.stderr

    def wait_alarm(seconds, expected):
        fd = os.open('/dev/rtc0', os.O_RDONLY | os.O_NONBLOCK)
        try:
            ready, _, _ = select.select([fd], [], [], seconds)
            value = int.from_bytes(os.read(fd, 8), 'little') if ready else 0
            if bool(value & 0x20) != expected:
                raise RuntimeError(f'alarm IRQ value {value:#x}, expected event={expected}')
            return value
        finally:
            os.close(fd)

    save()
    try:
        for seconds in (2, 5):
            start = time.monotonic()
            log = command('-m', 'on', '-s', str(seconds))
            elapsed = time.monotonic() - start
            if not seconds - 1 <= elapsed <= seconds + 3:
                raise RuntimeError(f'alarm timing out of range: {elapsed}')
            result['tests'].append({'test': 'delivery', 'seconds': seconds,
                                    'elapsed': elapsed, 'log': log})
            save()
        command('-m', 'no', '-s', '3')
        command('-m', 'disable')
        value = wait_alarm(5, False)
        result['tests'].append({'test': 'cancel', 'irq_value': value})
        save()
        command('-m', 'no', '-s', '15')
        command('-m', 'disable')
        start = time.monotonic()
        command('-m', 'no', '-s', '3')
        value = wait_alarm(7, True)
        result['tests'].append({'test': 'rearm', 'elapsed': time.monotonic() - start,
                                'irq_value': value})
        if (base / 'wakealarm').read_text().strip():
            raise RuntimeError('alarm remains armed after delivery')
        result['status'] = 'passed'
    except Exception as error:
        result.update(status='failed', error=str(error))
    finally:
        command('-m', 'disable')
        result['interrupts_after'] = Path('/proc/interrupts').read_text()
        result['dmesg_after'] = subprocess.check_output(['dmesg'], text=True)
        save()
    print(json.dumps({k: v for k, v in result.items() if not k.endswith('_after')}, indent=2))
    return int(result['status'] != 'passed')


if __name__ == '__main__':
    raise SystemExit(main())
