#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-2.0-only
"""Bounded J700 PCIe ASPM policy comparison; restore the original policy.

Uses the PCI core's coordinated link configuration. No direct register writes,
driver unbind, firmware commands, or reboot. Requires Neo's single Wi-Fi link.
"""
import argparse
import fcntl
import hashlib
import json
from pathlib import Path
import re
import statistics
import subprocess
import sys
import time


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('--output', type=Path, required=True)
    p.add_argument('--sleep-seconds', type=int,
                   help='instead of an awake interval, qualify a display-off RTC sleep')
    a = p.parse_args()
    if a.sleep_seconds is not None and not 10 <= a.sleep_seconds <= 600:
        p.error('sleep-seconds must be 10..600')
    devices = Path('/sys/bus/pci/devices')
    if sorted(x.name for x in devices.iterdir()) != ['0000:00:00.0', '0000:01:00.0']:
        p.error('expected only the J700 Wi-Fi root port and endpoint')
    ep = devices / '0000:01:00.0'
    if (ep / 'vendor').read_text().strip() != '0x14c3' or (ep / 'device').read_text().strip() != '0x7932':
        p.error('expected MT7932')
    if b'apple,j700\0' not in Path('/proc/device-tree/compatible').read_bytes():
        p.error('expected J700')
    lock = open('/run/neo-pcie-power.lock', 'w')
    fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
    policy = Path('/sys/module/pcie_aspm/parameters/policy')
    old = re.search(r'\[([^]]+)\]', policy.read_text()).group(1)
    if old == 'powersupersave':
        p.error('powersupersave already selected')
    a.output.mkdir(parents=True, exist_ok=False)
    r = dict(status='running', original_policy=old, stages=[],
             boot_id=Path('/proc/sys/kernel/random/boot_id').read_text().strip(),
             kernel_notes_sha256=hashlib.sha256(Path('/sys/kernel/notes').read_bytes()).hexdigest())

    def save():
        (a.output / 'result.json').write_text(json.dumps(r, indent=2) + '\n')

    def config(name):
        (a.output / (name + '-pci.txt')).write_text(subprocess.check_output(['lspci', '-vv'], text=True))
        return {x.name: x.read_text().strip() for x in (ep / 'link').iterdir() if x.is_file()}

    battery = Path('/sys/class/power_supply/macsmc-battery')

    def sample(name, seconds):
        stage = dict(name=name, links=config(name), samples=[])
        r['stages'].append(stage)
        start = time.monotonic()
        while time.monotonic() - start < seconds:
            if (battery / 'status').read_text().strip() != 'Discharging':
                raise RuntimeError('battery discharge required')
            stage['samples'].append(dict(monotonic=time.monotonic(),
                power_uW=int((battery / 'power_now').read_text()),
                raw_smc=Path('/sys/kernel/debug/neo-smc-power/raw').read_text()))
            save()
            time.sleep(1)
        stage['mean_W_after_5s'] = -statistics.mean(x['power_uW'] for x in stage['samples']
                                                    if x['monotonic'] - start >= 5) / 1e6
        save()

    try:
        sample('before', 20)
        policy.write_text('powersupersave\n')
        r['trial_policy'] = policy.read_text().strip()
        r['trial_links'] = config('trial')
        save()
        if a.sleep_seconds:
            helper = Path(__file__).with_name('sleep-power.py')
            with (a.output / 'sleep.log').open('w') as log:
                subprocess.run([sys.executable, str(helper), '--mode', 'sleep', '--seconds',
                                str(a.sleep_seconds), '--display-off', '--output', str(a.output / 'sleep')],
                               stdout=log, stderr=subprocess.STDOUT, check=True)
            r['sleep_result'] = json.loads((a.output / 'sleep/result.json').read_text())
        else:
            sample('powersupersave', 60)
    except BaseException as e:
        r.update(status='failed', error=repr(e))
        raise
    finally:
        policy.write_text(old + '\n')
        r['restored_policy'] = policy.read_text().strip()
        r['restored_links'] = config('restored')
        baseline_links = r['stages'][0]['links'] if r['stages'] else None
        r['restoration_verified'] = ((baseline_links is None or r['restored_links'] == baseline_links) and
                                     re.search(r'\[([^]]+)\]', policy.read_text()).group(1) == old)
        if not r['restoration_verified']:
            r.update(status='restore-failed', error='PCIe policy/link restoration mismatch')
        save()
        if not r['restoration_verified']:
            raise RuntimeError('PCIe policy/link restoration mismatch')
    sample('restored', 20)
    if not a.sleep_seconds:
        before, trial, after = [x['mean_W_after_5s'] for x in r['stages']]
        r['reduction_W_vs_mean_baseline'] = (before + after) / 2 - trial
    (a.output / 'kernel.log').write_text(subprocess.check_output(['dmesg'], text=True))
    r['status'] = 'completed'
    save()


if __name__ == '__main__':
    main()
