#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-2.0-only
"""Qualify Linux CPU affinity, concurrent work and cross-CPU migration.

Run on the M5 after an opt-in --smp RAM boot. This does not qualify cpufreq,
CPU hotplug, suspend, or sustained thermal behavior.
"""
import concurrent.futures
import hashlib
import json
import os
from pathlib import Path
import time


def current_cpu():
    # The comm field can contain spaces and parentheses. processor is field 39.
    return int(Path('/proc/self/stat').read_text().rsplit(')', 1)[1].split()[36])


def worker(cpu):
    os.sched_setaffinity(0, {cpu})
    data = bytes(range(256)) * 1024
    # Controller-generated reference, not a digest computed by the CPU under test.
    expected = bytes.fromhex('2312394bd99545d9de131c24efb781e7'
                             '65ac1aec243f2ed9347597a793a415e9')
    begin = time.monotonic()
    iterations = 0
    while time.monotonic() - begin < 2:
        if current_cpu() != cpu or hashlib.sha256(data).digest() != expected:
            raise RuntimeError(f'CPU {cpu}: affinity or computation failed')
        iterations += 1
    return {'cpu': cpu, 'observed_cpu': current_cpu(), 'iterations': iterations,
            'seconds': time.monotonic() - begin}


def main():
    allowed = sorted(os.sched_getaffinity(0))
    online = Path('/sys/devices/system/cpu/online').read_text().strip()
    if allowed != list(range(10)) or online != '0-9':
        raise RuntimeError('M5 qualification requires all ten CPUs online and available')
    before = Path('/proc/interrupts').read_text()
    with concurrent.futures.ProcessPoolExecutor(max_workers=len(allowed)) as pool:
        results = list(pool.map(worker, allowed))
    # Visit every ordered pair to exercise local and cross-cluster migrations.
    migrations = 0
    try:
        for source in allowed:
            for target in allowed:
                for cpu in (source, target):
                    os.sched_setaffinity(0, {cpu})
                    if current_cpu() != cpu:
                        raise RuntimeError(f'Migration to CPU {cpu} failed')
                    migrations += 1
    finally:
        os.sched_setaffinity(0, set(allowed))
    after = Path('/proc/interrupts').read_text()
    relevant = lambda text: [line for line in text.splitlines()
                             if 'arch_timer' in line or 'IPI' in line]
    print(json.dumps({'online': online, 'allowed': allowed, 'workers': results,
                      'affinity_visits': migrations,
                      'interrupts_before': relevant(before),
                      'interrupts_after': relevant(after), 'result': 'PASS'}, indent=2))


if __name__ == '__main__':
    main()
