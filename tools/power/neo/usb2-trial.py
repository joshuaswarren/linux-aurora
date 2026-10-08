#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-2.0-only
"""Qualify opt-in J700 USB2 sleep, retaining enumeration and recovery evidence.

Run in a transient systemd unit. --phy-off temporarily disables USB wake;
RTC must already be qualified. A return from suspend alone is not success.
"""
import argparse
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys
import time

HERE = Path(__file__).resolve().parent

def load(name):
    spec = importlib.util.spec_from_file_location(name.replace('-', '_'), HERE / (name + '.py'))
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod

pm = load('pm-test')
gate = load('gate-trial')
PHY = Path('/sys/module/phy_apple_atc/parameters')
CONTROL = Path('/sys/module/dwc3_apple/parameters/usb2_s2idle')
USB = Path('/sys/bus/platform/devices/40a280000.usb')
CLOCK = Path('/sys/module/pmgr_pwrstate/parameters')

def clock_state():
    return {k: (CLOCK / ('usb_clock_' + k)).read_text().strip()
            for k in ('s2idle', 'pending', 'failed', 'offs', 'ons')}

def counters():
    return {k: (PHY / k).read_text().strip() for k in
            ('usb2_sleep_offs', 'usb2_sleep_ons', 'usb2_sleep_failed')}

def inventory():
    records = {}
    for p in Path('/sys/bus/usb/devices').iterdir():
        if (p / 'idVendor').exists():
            records[p.name] = {k: pm.read(p / k) for k in
                              ('idVendor', 'idProduct', 'bDeviceClass', 'authorized',
                               'bConfigurationValue', 'speed')}
    return records

def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument('--output', type=Path, required=True)
    ap.add_argument('--phy-off', action='store_true')
    ap.add_argument('--platform', action='store_true')
    ap.add_argument('--clock-gate', action='store_true', help='also clock-gate the USB genpd leaf')
    ap.add_argument('--seconds', type=int, default=20)
    ap.add_argument('--scale', type=float)
    a = ap.parse_args()
    if a.clock_gate and not a.phy_off:
        ap.error('--clock-gate requires --phy-off')
    a.output.mkdir(parents=True, exist_ok=False)
    assert CONTROL.read_text().strip() == 'N', 'another USB trial is enabled'
    r = {'status': 'running', 'phy_off': a.phy_off, 'before': pm.snapshot(),
         'usb_before': inventory(), 'phy_before': counters(), 'wake_before': {}}
    if a.clock_gate:
        r['clock_before'] = clock_state()
        assert r['clock_before']['s2idle'] == 'N'
        assert r['clock_before']['pending'] == r['clock_before']['failed'] == 'N'
    assert r['usb_before'], 'USB inventory is empty'
    assert any(x['idVendor'] == '2109' for x in r['usb_before'].values()), 'internal VIA hub missing'
    assert r['phy_before']['usb2_sleep_failed'] == 'N'
    payload = os.urandom(1 << 20)
    p = a.output / 'storage.bin'
    p.write_bytes(payload)
    expected = hashlib.sha256(payload).hexdigest()
    def save():
        q = a.output / 'result.tmp'
        q.write_text(json.dumps(r, indent=2) + '\n')
        q.replace(a.output / 'result.json')
        os.sync()
    save()
    try:
        if a.phy_off:
            for w in USB.resolve().rglob('power/wakeup'):
                value = w.read_text().strip()
                if value in ('enabled', 'disabled'):
                    r['wake_before'][str(w)] = value
            save()
            for w in r['wake_before']:
                pm.write_control(w, 'disabled')
            pm.write_control(CONTROL, 'Y')
        if a.clock_gate:
            pm.write_control(CLOCK / 'usb_clock_s2idle', 'Y')
        cmd = [sys.executable, str(HERE / ('pm-test.py' if a.platform else 'sleep-smc.py'))]
        cmd += ['--stage', 'platform'] if a.platform else ['--seconds', str(a.seconds)]
        if a.scale and not a.platform:
            cmd += ['--scale', str(a.scale)]
        cmd += ['--output', str(a.output / 'sleep')]
        with (a.output / 'sleep.log').open('w') as log:
            subprocess.run(cmd, stdout=log, stderr=subprocess.STDOUT, check=True)
        r['phy_after'] = counters()
        assert r['phy_after']['usb2_sleep_failed'] == 'N', 'PHY failure latched'
        for k in ('usb2_sleep_offs', 'usb2_sleep_ons'):
            assert int(r['phy_after'][k]) - int(r['phy_before'][k]) == int(a.phy_off), k + ' transition count mismatch'
        if a.clock_gate:
            r['clock_after'] = clock_state()
            assert r['clock_after']['pending'] == r['clock_after']['failed'] == 'N'
            for k in ('offs', 'ons'):
                assert int(r['clock_after'][k]) - int(r['clock_before'][k]) == 1, 'USB clock ' + k + ' mismatch'
        deadline = time.monotonic() + 15
        while True:
            r['usb_after'] = inventory()
            if r['usb_after'] == r['usb_before'] or time.monotonic() >= deadline:
                break
            time.sleep(1)
        assert r['usb_after'] == r['usb_before'], 'USB devices/configuration did not return'
        r['health'] = gate.health(p, expected)
        assert r['health']['storage_ok'] and r['health']['network_ok'], 'storage/network failed'
        assert r['health']['display_state'] == '1', 'panel not restored'
        r['after'] = pm.snapshot()
        r['suspend_stats_check'] = pm.check_suspend_stats(r['before'], r['after'])
        pm.check_firmware_health(r['after'])
        assert r['before']['files']['/proc/sys/kernel/random/boot_id'] == r['after']['files']['/proc/sys/kernel/random/boot_id']
        r['status'] = 'passed'
    except BaseException as e:
        r['status'] = 'failed'
        r['error'] = repr(e)
        raise
    finally:
        if a.clock_gate:
            pm.write_control(CLOCK / 'usb_clock_s2idle', 'N')
        pm.write_control(CONTROL, 'N')
        for w, value in r['wake_before'].items():
            try:
                pm.write_control(w, value)
            except OSError as e:
                r.setdefault('wake_restore_errors', {})[w] = str(e)
                r['status'] = 'failed'
        r['dmesg_after'] = pm.command(['dmesg'])
        save()
    print(json.dumps({k: r[k] for k in ('status', 'phy_off', 'phy_before', 'phy_after', 'health')}))
    return int(r['status'] != 'passed')

if __name__ == '__main__':
    raise SystemExit(main())
