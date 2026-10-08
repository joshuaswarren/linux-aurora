#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-2.0-only
"""Measure native macOS idle with Wi-Fi available and display-off verified.

Requires an existing display-off notification. Temporarily inhibits only idle
system sleep and preserves persistent pmset policy.
Polls slowly and uses battery-counter timestamps instead of read timestamps.
"""
import argparse
import importlib.util
import json
import os
import plistlib
import re
from pathlib import Path
import subprocess
import time

spec=importlib.util.spec_from_file_location('capture',Path(__file__).with_name('macos-sleep-measure.py'))
capture=importlib.util.module_from_spec(spec);spec.loader.exec_module(capture)

def command(*a):
    p=subprocess.run(a,text=True,capture_output=True)
    return {'returncode':p.returncode,'stdout':p.stdout,'stderr':p.stderr}

def power_state():
    data=plistlib.loads(subprocess.check_output(['ioreg','-a','-r','-c','IOPMrootDomain']))[0]
    return {k:data.get(k) for k in ('AppleClamshellState','AppleClamshellCausesSleep','SleepDisabled')}

def network_health():
    route=command('route','-n','get','default')
    gateway=re.search(r'gateway:\s*(\S+)',route['stdout'])
    if not gateway:raise RuntimeError('no default gateway')
    ping=command('ping','-c','1','-W','2000',gateway[1])
    if ping['returncode']:raise RuntimeError('gateway did not reply')
    return {'route':route,'ping':ping}

def display_log_state():
    lines=[x for x in capture.run('pmset','-g','log').splitlines() if 'Display is turned ' in x]
    return lines[-1] if lines else ''

def main():
    ap=argparse.ArgumentParser();ap.add_argument('--output',type=Path,required=True);ap.add_argument('--seconds',type=int,default=120)
    ap.add_argument('--require-closed',action='store_true')
    a=ap.parse_args();a.output.mkdir(parents=True,exist_ok=False)
    r={'status':'preparing','samples':[],'requested_seconds':a.seconds,'boot':capture.run('sysctl','-n','kern.boottime')}
    r['boot_uuid']=capture.run('sysctl','-n','kern.bootsessionuuid')
    inhibitor=subprocess.Popen(['caffeinate','-i','-t',str(a.seconds+240)])
    def save():
        p=a.output/'result.tmp';p.write_text(json.dumps(r,indent=2)+'\n');p.replace(a.output/'result.json');os.sync()
    try:
        r['power_state_before']=power_state()
        if a.require_closed:
            assert r['power_state_before']['AppleClamshellState'] is True, 'lid must be closed'
            assert r['power_state_before']['SleepDisabled'] is True, 'system sleep must be disabled'
        r['network_health_before']=network_health()
        r['network_before']=command('networksetup','-getairportnetwork','en0')
        r['link_before']=command('ifconfig','en0')
        r['settings']=command('pmset','-g','custom')
        r['display_before']=display_log_state()
        if 'Display is turned off' not in r['display_before']:
            raise RuntimeError('display-off state not confirmed; pmset can print an error while exiting0')
        r['display_off_confirmed']=time.time();save()
        # Let display power settle and wait for a new published battery sample.
        time.sleep(15)
        initial=capture.sample();r['samples'].append(initial)
        update=initial['battery']['UpdateTime'];deadline=time.time()+180
        while True:
            s=capture.sample();r['samples'].append(s)
            if s['battery']['UpdateTime']>update:break
            if time.time()>deadline:raise RuntimeError('no battery refresh')
            time.sleep(3)
        r['before']=s
        assert not s['battery']['ExternalConnected'] and not s['battery']['IsCharging']
        r['assertions_before']=command('pmset','-g','assertions')
        r['status']='measuring';save()
        deadline=time.time()+a.seconds+120
        while True:
            time.sleep(5)
            s=capture.sample();r['samples'].append(s)
            if s['battery']['UpdateTime']-r['before']['battery']['UpdateTime']>=a.seconds:break
            if time.time()>deadline:raise RuntimeError('no ending battery refresh')
        r['after']=s
        assert not s['battery']['ExternalConnected'] and not s['battery']['IsCharging']
        r['power_state_after']=power_state()
        if a.require_closed:assert r['power_state_after']['AppleClamshellState'] is True
        r['network_health_after']=network_health()
        r['assertions_after']=command('pmset','-g','assertions')
        r['network_after']=command('networksetup','-getairportnetwork','en0')
        r['link_after']=command('ifconfig','en0')
        r['pmset_log']=capture.run('pmset','-g','log')
        r['display_after']=display_log_state()
        assert r['display_after']==r['display_before'], 'display transitioned during capture'
        r['boot_end']=capture.run('sysctl','-n','kern.boottime')
        r['boot_uuid_end']=capture.run('sysctl','-n','kern.bootsessionuuid')
        assert r['boot_uuid']==r['boot_uuid_end']
        r['status']='captured'
    except BaseException as e:r['status']='error';r['error']=repr(e);raise
    finally:
        inhibitor.terminate();inhibitor.wait();save()
    print('capture complete',flush=True)

if __name__=='__main__':main()
