#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-2.0-only
"""Measure J700 sleep with the GPU unbound, then attempt to restore the driver.

This is destructive to graphics contexts and requires an already stopped
desktop with no DRM clients. It is component isolation, not GPU suspend.
Never replace modules, force an unbind past live clients, or touch GPU MMIO.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess
import time

DRIVER = Path('/sys/bus/platform/drivers/asahi')
GPU = '482600000.gpu'
DOMAIN = Path('/sys/kernel/debug/pm_genpd/gfx/current_state')
CLIENTS = Path('/sys/kernel/debug/dri/482600000.gpu/clients')


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('--output', type=Path, required=True)
    p.add_argument('--module-sha256', required=True)
    p.add_argument('--seconds', type=int, default=180)
    a = p.parse_args()
    if not 10 <= a.seconds <= 600:
        p.error('seconds must be in 10..600')
    a.output.mkdir(parents=True, exist_ok=False)
    result = dict(status='preflight', boot_id=Path('/proc/sys/kernel/random/boot_id').read_text().strip(),
                  kernel=os.uname().release, graphics_contexts_preserved=False)

    def save():
        tmp = a.output / 'result.tmp'
        tmp.write_text(json.dumps(result, indent=2) + '\n')
        tmp.replace(a.output / 'result.json')
        os.sync()

    def capture(label):
        for name, command in [('kernel', ['dmesg']), ('domains', ['cat', '/sys/kernel/debug/pm_genpd/pm_genpd_summary'])]:
            out = subprocess.run(command, capture_output=True, text=True)
            (a.output / f'{label}-{name}.txt').write_text(out.stdout + out.stderr)

    touched = False
    try:
        assert b'apple,j700\0' in Path('/proc/device-tree/compatible').read_bytes()
        assert (DRIVER / GPU).exists(), 'GPU must be bound for preflight'
        assert subprocess.run(['systemctl', 'is-active', '--quiet', 'sddm']).returncode, 'stop SDDM first'
        clients = CLIENTS.read_text()
        (a.output / 'clients-before.txt').write_text(clients)
        assert len(clients.strip().splitlines()) == 1, 'DRM clients remain'
        module = Path(subprocess.check_output(['modinfo', '-n', 'asahi'], text=True).strip())
        result['module_sha256'] = hashlib.sha256(module.read_bytes()).hexdigest()
        assert result['module_sha256'] == a.module_sha256, 'unexpected GPU module'
        result['domain_before'] = DOMAIN.read_text().strip()
        capture('before')
        result['status'] = 'unbinding'
        save()
        touched = True
        (DRIVER / 'unbind').write_text(GPU + '\n')
        deadline = time.monotonic() + 10
        while DOMAIN.read_text().strip() != 'off-0' and time.monotonic() < deadline:
            time.sleep(.1)
        result['domain_unbound'] = DOMAIN.read_text().strip()
        capture('unbound')
        assert not (DRIVER / GPU).exists(), 'GPU is still bound'
        assert result['domain_unbound'] == 'off-0', 'genpd does not establish GPU off'
        result['status'] = 'measuring'
        save()
        command = ['python3', str(Path(__file__).with_name('sleep-power.py')),
                   '--mode', 'sleep', '--seconds', str(a.seconds), '--display-off',
                   '--output', str(a.output / 'sleep')]
        with (a.output / 'sleep.log').open('w') as log:
            result['sleep_returncode'] = subprocess.run(command, stdout=log, stderr=subprocess.STDOUT).returncode
        result['domain_after_sleep'] = DOMAIN.read_text().strip()
        assert result['sleep_returncode'] == 0, 'sleep qualification failed'
        assert result['domain_after_sleep'] == 'off-0', 'GPU domain changed during sleep'
        result['status'] = 'measured'
    except Exception as error:
        result.update(status='error', error=str(error))
    finally:
        save()
        if touched:
            capture('before-restore')
            result['restore_started'] = True
            save()
            try:
                if not (DRIVER / GPU).exists():
                    (DRIVER / 'bind').write_text(GPU + '\n')
                result['rebound'] = (DRIVER / GPU).exists()
                result['domain_restored'] = DOMAIN.read_text().strip()
            except Exception as error:
                result['restore_error'] = str(error)
            capture('restored')
            save()
    print(json.dumps(result, indent=2))
    return int(result['status'] != 'measured' or not result.get('rebound'))


if __name__ == '__main__':
    raise SystemExit(main())
