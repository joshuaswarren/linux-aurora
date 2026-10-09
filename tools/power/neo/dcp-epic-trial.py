#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-2.0-only
"""Measure a bounded native DCP display-off interval, optionally with AP IDLE.

Requires an already adopted, HPD-qualified neo-dcp-retained endpoint. Native
firmware owns panel power during this trial; do not also run panel-gpio cycles.
The kernel independently restores power if this controller exits.
"""
import argparse
import fcntl
import hashlib
import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import time

DEV = Path('/sys/bus/platform/devices/412e00000.neo-dcp-retained')


def write(name, value):
    fd = os.open(DEV / name, os.O_WRONLY)
    try:
        os.write(fd, (str(value) + '\n').encode())
    finally:
        os.close(fd)


def status():
    return dict(item.split('=', 1) for item in (DEV / 'epic_status').read_text().split())


def wait_hpd():
    deadline = time.monotonic() + 30
    while True:
        write('epic_query', 'hpd')
        response = (DEV / 'epic_response').read_bytes()
        if len(response) == 20 and response[8] == 1:
            return response.hex()
        if time.monotonic() >= deadline:
            raise RuntimeError('panel detection did not return')
        time.sleep(.5)


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('--directory', type=Path, required=True)
    p.add_argument('--output', type=Path, required=True)
    p.add_argument('--seconds', type=int, default=120)
    mode = p.add_mutually_exclusive_group()
    mode.add_argument('--ap-idle', action='store_true')
    mode.add_argument('--endpoint-cycle', action='store_true',
                      help='shut down and restart AFK, without IOP idle')
    p.add_argument('--cpu-energy-client', type=Path,
                   help='optional read-only cpu-energy-read binary')
    p.add_argument('--cpu-state-client', type=Path,
                   help='optional cpu-idle-sample binary; short busy samples outside measurement')
    a = p.parse_args()
    if not 10 <= a.seconds <= 240:
        p.error('seconds must be in 10..240')
    root = a.directory.resolve(strict=True)
    energy_client = a.cpu_energy_client.resolve(strict=True) if a.cpu_energy_client else None
    state_client = a.cpu_state_client.resolve(strict=True) if a.cpu_state_client else None
    out = a.output.resolve()
    lock = open('/run/neo-dcp-epic-trial.lock', 'w')
    fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
    for module in ('neo_backlight_trial', 'neo_display_sleep'):
        if Path('/sys/module', module).exists():
            raise RuntimeError('another helper owns panel power')
    before = status()
    if before['error'] != '0' or before['queried'] != '1' or before['power_stage'] not in ('0', '4'):
        raise RuntimeError('requires a healthy HPD-qualified endpoint')
    if Path('/sys/class/power_supply/macsmc-battery/status').read_text().strip() != 'Discharging':
        raise RuntimeError('requires battery discharge')
    out.mkdir(parents=True, exist_ok=False)
    result = dict(status='running', ap_idle=a.ap_idle, endpoint_cycle=a.endpoint_cycle, seconds=a.seconds,
                  boot_id=Path('/proc/sys/kernel/random/boot_id').read_text().strip(),
                  kernel=Path('/proc/version').read_text().strip(),
                  cmdline=Path('/proc/cmdline').read_text().strip(),
                  helper_sha256=hashlib.sha256(Path(__file__).read_bytes()).hexdigest(),
                  power_helper_sha256=hashlib.sha256((root / 'sleep-power.py').read_bytes()).hexdigest(),
                  before=before)
    if energy_client:
        result['cpu_energy_client_sha256'] = hashlib.sha256(energy_client.read_bytes()).hexdigest()
        result['cpu_energy_helper_sha256'] = hashlib.sha256((root / 'cpu-energy.py').read_bytes()).hexdigest()
    if state_client:
        result['cpu_state_client_sha256'] = hashlib.sha256(state_client.read_bytes()).hexdigest()

    def save():
        (out / 'result.json').write_text(json.dumps(result, indent=2) + '\n')

    def interrupted(signum, frame):
        raise RuntimeError(f'interrupted by signal {signum}')

    def gpio():
        return subprocess.check_output([str(root / 'panel-gpio'), '--read'], text=True)

    signal.signal(signal.SIGTERM, interrupted)
    signal.signal(signal.SIGINT, interrupted)
    child = None
    energy_child = None
    attr = ('epic_endpoint_cycle' if a.endpoint_cycle else
            'epic_ap_cycle' if a.ap_idle else 'epic_power_cycle')
    try:
        save()
        write(attr, 2)
        result['short_cycle'] = status()
        if result['short_cycle']['power_stage'] != '4':
            raise RuntimeError('short cycle failed')
        result['hpd_before'] = wait_hpd()
        result['wifi_before'] = subprocess.check_output(['iw', 'dev'], text=True)
        save()
        code = ('import os,sys; f=os.open(sys.argv[1],os.O_WRONLY); '
                'os.write(f,(sys.argv[2]+chr(10)).encode()); os.close(f)')
        with (out / 'cycle.log').open('w') as log:
            child = subprocess.Popen([sys.executable, '-c', code, str(DEV / attr),
                                      str(a.seconds + 60)], stdout=log, stderr=subprocess.STDOUT)
        deadline = time.monotonic() + 20
        while status()['power_stage'] != '2':
            if child.poll() is not None or time.monotonic() >= deadline:
                raise RuntimeError('off state not acknowledged')
            time.sleep(.1)
        result['off_before'] = status()
        if a.ap_idle and (result['off_before']['ap_cycle'] != '1' or
                          result['off_before']['ap_idle_result'] != '0'):
            raise RuntimeError('AP IDLE was not acknowledged')
        if a.endpoint_cycle and (result['off_before']['started'] != '0' or
                                 result['off_before']['endpoint_stop_result'] != '0'):
            raise RuntimeError('endpoint shutdown was not acknowledged')
        result['gpio_off_before'] = gpio()
        if 'inherited=0\n' not in result['gpio_off_before']:
            raise RuntimeError('native display shutdown did not lower LCD power')
        time.sleep(10)
        if state_client:
            result['cpu_off_before'] = json.loads(subprocess.check_output(
                [str(state_client), '--busy'], text=True, timeout=30))
        save()
        if energy_client:
            with (out / 'cpu-energy.log').open('w') as log:
                energy_child = subprocess.Popen(
                    [sys.executable, str(root / 'cpu-energy.py'), '--client', str(energy_client),
                     '--seconds', str(a.seconds), '--output', str(out / 'cpu-energy')],
                    stdout=log, stderr=subprocess.STDOUT)
        with (out / 'power.log').open('w') as log:
            subprocess.run([sys.executable, str(root / 'sleep-power.py'), '--mode', 'awake',
                            '--seconds', str(a.seconds), '--output', str(out / 'power')],
                           stdout=log, stderr=subprocess.STDOUT, check=True)
        if energy_child:
            if energy_child.wait(timeout=30):
                raise RuntimeError('CPU energy capture failed')
            result['cpu_energy'] = json.loads((out / 'cpu-energy/result.json').read_text())
            if result['cpu_energy']['status'] != 'complete':
                raise RuntimeError('CPU energy capture incomplete')
        if state_client:
            result['cpu_off_after'] = json.loads(subprocess.check_output(
                [str(state_client), '--busy'], text=True, timeout=30))
        result['off_after'] = status()
        result['gpio_off_after'] = gpio()
        if result['off_after']['power_stage'] != '2' or 'inherited=0\n' not in result['gpio_off_after']:
            raise RuntimeError('off state ended before measurement')
        save()
        if child.wait(timeout=90):
            raise RuntimeError('cycle writer failed')
        result['after'] = status()
        if result['after']['power_stage'] != '4' or result['after']['error'] != '0':
            raise RuntimeError('native power restore failed')
        if a.ap_idle and result['after']['ap_wake_result'] != '0':
            raise RuntimeError('AP wake failed')
        if a.endpoint_cycle and (result['after']['endpoint_start_result'] != '0' or
                                 result['after']['started'] != '1'):
            raise RuntimeError('endpoint restart failed')
        result['hpd_after'] = wait_hpd()
        result['gpio_after'] = gpio()
        if 'inherited=1\n' not in result['gpio_after']:
            raise RuntimeError('panel power did not return')
        result['wifi_after'] = subprocess.check_output(['iw', 'dev'], text=True)
        subprocess.run(['ping', '-c', '3', '-W', '2', '10.129.154.1'], check=True)
        result['status'] = 'measured'
    except BaseException as error:
        result.update(status='failed', error=repr(error))
    finally:
        save()
        if energy_child is not None and energy_child.poll() is None:
            energy_child.terminate()
            try:
                energy_child.wait(timeout=5)
            except subprocess.TimeoutExpired:
                energy_child.kill()
                energy_child.wait()
        if child is not None and child.poll() is None:
            child.wait(timeout=320)
        (out / 'kernel.log').write_text(subprocess.check_output(['dmesg'], text=True))
        save()
    print(json.dumps(result, indent=2))
    return result['status'] != 'measured'


if __name__ == '__main__':
    sys.exit(main())
