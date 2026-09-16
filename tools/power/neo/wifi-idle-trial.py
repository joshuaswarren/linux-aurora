#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-2.0-only
"""Bounded awake Wi-Fi trial with both J700 backlight and LCD power removed."""
import argparse
import hashlib
import json
from pathlib import Path
import signal
import subprocess
import sys
import time

p = argparse.ArgumentParser(description=__doc__)
p.add_argument('--directory', type=Path, required=True)
p.add_argument('--output', type=Path, required=True)
p.add_argument('--seconds', type=int, default=120)
a = p.parse_args()
if not 30 <= a.seconds <= 500:
    p.error('seconds must be in 30..500')
a.directory = a.directory.resolve(strict=True)
a.output = a.output.resolve()
state = Path('/sys/module/neo_backlight_trial/parameters/state')
if state.exists() or Path('/sys/module/neo_display_sleep').exists():
    raise RuntimeError('a panel/backlight trial already owns the display')
if Path('/sys/class/power_supply/macsmc-battery/status').read_text().strip() != 'Discharging':
    raise RuntimeError('requires battery discharge')
a.output.mkdir(parents=True, exist_ok=False)
result = dict(status='running', boot_id=Path('/proc/sys/kernel/random/boot_id').read_text().strip(),
              kernel=subprocess.check_output(['uname', '-a'], text=True).strip(),
              cmdline=Path('/proc/cmdline').read_text().strip(), seconds=a.seconds,
              identities={name: hashlib.sha256((a.directory / name).read_bytes()).hexdigest()
                          for name in ('panel-gpio', 'neo-backlight-trial.ko', 'sleep-power.py')})


def save():
    (a.output / 'trial.json').write_text(json.dumps(result, indent=2) + '\n')


def interrupted(signum, frame):
    raise RuntimeError(f'interrupted by signal {signum}')


signal.signal(signal.SIGTERM, interrupted)
signal.signal(signal.SIGINT, interrupted)
panel = None
loaded = False
try:
    result['preflight'] = subprocess.check_output([str(a.directory / 'panel-gpio'), '--read'], text=True)
    result['wifi_before'] = subprocess.check_output(['iw', 'dev'], text=True)
    save()
    # Kernel backlight restore is deliberately later than LCD restore. Both
    # have independent deadlines if this process or its SSH connection dies.
    subprocess.run(['insmod', str(a.directory / 'neo-backlight-trial.ko'),
                    f'seconds={a.seconds + 70}'], check=True)
    loaded = True
    with (a.output / 'panel.log').open('w') as log:
        panel = subprocess.Popen([str(a.directory / 'panel-gpio'), '--cycle', str(a.seconds + 40)],
                                 stdout=log, stderr=subprocess.STDOUT)
        deadline = time.monotonic() + 5
        while 'off readback=0\n' not in (a.output / 'panel.log').read_text():
            if panel.poll() is not None or time.monotonic() > deadline:
                raise RuntimeError('LCD power-off readback failed')
            time.sleep(.05)
        time.sleep(10)
        if state.read_text().strip() != '0' or panel.poll() is not None:
            raise RuntimeError('display off interval ended early')
        with (a.output / 'measurement.log').open('w') as measurement:
            subprocess.run([sys.executable, str(a.directory / 'sleep-power.py'), '--mode', 'awake',
                            '--seconds', str(a.seconds), '--output', str(a.output / 'power')],
                           stdout=measurement, stderr=subprocess.STDOUT, check=True)
        if state.read_text().strip() != '0' or panel.poll() is not None:
            raise RuntimeError('display off interval ended during measurement')
    result['wifi_after'] = subprocess.check_output(['iw', 'dev'], text=True)
    result['status'] = 'measured'
except BaseException as exc:
    result.update(status='failed', error=repr(exc))
    raise
finally:
    if panel is not None:
        if panel.poll() is None:
            panel.terminate()
        result['panel_returncode'] = panel.wait(timeout=5)
        if result['panel_returncode'] or 'restored=1\n' not in (a.output / 'panel.log').read_text():
            result['status'] = 'restore-failed'
    # Wait for LCD supply settling before restoring backlight.
    time.sleep(.1)
    if loaded:
        result['backlight_restore_returncode'] = subprocess.run(['rmmod', 'neo_backlight_trial']).returncode
        if result['backlight_restore_returncode']:
            result['status'] = 'restore-failed'
    (a.output / 'kernel.log').write_text(subprocess.check_output(['dmesg'], text=True))
    save()
print(json.dumps(result, indent=2))
sys.exit(result['status'] != 'measured')
