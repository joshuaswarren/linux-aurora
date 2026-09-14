#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-2.0-only
"""Qualify a Neo PM transition and subsequent Wi-Fi association/address recovery.

Existing opt-in kernel/RTC/display setup is required. --seconds requests real
RTC sleep; --stage uses the PM debug stage. Recovery reboot is explicit, saves
logs first, and only runs after the PM helper returns. It cannot recover a hung
kernel callback. Keep external reset available. Association and an IP address
are not a throughput or application stability test.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys
import time


def command(*args):
    try:
        p = subprocess.run(args, capture_output=True, text=True, timeout=10)
        return dict(returncode=p.returncode, stdout=p.stdout, stderr=p.stderr)
    except subprocess.TimeoutExpired:
        return dict(returncode=None, stdout='', stderr='command timed out')


def network_sample():
    link = command('iw', 'dev', 'wlan0', 'link')
    address = command('ip', '-j', 'address', 'show', 'dev', 'wlan0')
    try:
        interfaces = json.loads(address['stdout'])
    except json.JSONDecodeError:
        interfaces = []
    # MT7932 can print a valid association then return EOPNOTSUPP for the
    # optional station-statistics request. Do not treat that as disconnection.
    associated = link['stdout'].startswith('Connected to ')
    configured = any(a.get('scope') == 'global' and a.get('family') == 'inet'
                     for iface in interfaces for a in iface.get('addr_info', []))
    return dict(monotonic=time.monotonic(), link=link, address=address,
                associated=associated, ipv4_configured=configured,
                recovered=associated and configured)


def main():
    p = argparse.ArgumentParser(description=__doc__)
    mode = p.add_mutually_exclusive_group(required=True)
    mode.add_argument('--stage', choices=('devices', 'platform'))
    mode.add_argument('--seconds', type=int)
    p.add_argument('--output', type=Path, required=True)
    p.add_argument('--recovery-boot-entry',
                   help='on network failure, select this GRUB entry once and reboot')
    a = p.parse_args()
    if os.geteuid():
        p.error('root is required')
    if b'apple,j700\0' not in Path('/proc/device-tree/compatible').read_bytes():
        p.error('J700 only')
    if a.seconds is not None and not 5 <= a.seconds <= 600:
        p.error('seconds must be 5..600')
    initial = network_sample()
    if not initial['recovered']:
        p.error('requires an associated wlan0 with a global IPv4 address before testing')
    helper = Path(__file__).with_name('pm-test.py' if a.stage else 'sleep-test.py')
    args = ['--stage', a.stage] if a.stage else ['--wake-after', str(a.seconds), '--display-off']
    a.output.mkdir(parents=True, exist_ok=False)
    result = dict(status='running', boot_id=Path('/proc/sys/kernel/random/boot_id').read_text().strip(),
                  helper_sha256=hashlib.sha256(helper.read_bytes()).hexdigest(),
                  before=initial, network_samples=[], recovery_boot_entry=a.recovery_boot_entry)

    def save():
        tmp = a.output / 'result.tmp'
        tmp.write_text(json.dumps(result, indent=2) + '\n')
        tmp.replace(a.output / 'result.json')
        os.sync()

    save()
    with (a.output / 'pm.log').open('w') as log:
        run = subprocess.run([sys.executable, str(helper), *args, '--output', str(a.output / 'pm')],
                             stdout=log, stderr=subprocess.STDOUT)
    result['pm_returncode'] = run.returncode
    result['status'] = 'waiting_for_network'
    save()
    deadline = time.monotonic() + 60
    while True:
        sample = network_sample()
        result['network_samples'].append(sample)
        if sample['recovered'] or time.monotonic() >= deadline:
            break
        time.sleep(1)
    result['network_recovered'] = sample['recovered']
    result['status'] = 'passed' if sample['recovered'] and not run.returncode else 'failed'
    result['dmesg'] = command('dmesg')
    save()
    if not sample['recovered'] and a.recovery_boot_entry:
        subprocess.run(['journalctl', '--sync'], check=True)
        subprocess.run(['grub-reboot', a.recovery_boot_entry], check=True)
        result['recovery_reboot_requested'] = True
        save()
        subprocess.run(['systemctl', 'reboot'], check=True)
    print(json.dumps({k: result[k] for k in ('status', 'pm_returncode', 'network_recovered')}))
    return 0 if result['status'] == 'passed' else 1


if __name__ == '__main__':
    raise SystemExit(main())
