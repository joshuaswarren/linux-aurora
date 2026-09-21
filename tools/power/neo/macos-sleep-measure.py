#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-2.0-only
"""Capture native macOS battery counters around one scheduled timed sleep.

Counters are retained verbatim (with signed 64-bit decoding), not assumed to
be energy integrals. Sleep duration/quality must be checked against pmset logs.
"""
import argparse
import datetime
import json
import os
from pathlib import Path
import plistlib
import subprocess
import time

KEYS = ('UpdateTime', 'Voltage', 'AppleRawBatteryVoltage', 'Amperage',
        'InstantAmperage', 'AppleRawCurrentCapacity', 'AppleRawMaxCapacity',
        'AbsoluteCapacity', 'NominalChargeCapacity', 'CurrentCapacity',
        'ExternalConnected', 'IsCharging', 'Temperature', 'PowerTelemetryData',
        'PowerOutDetails', 'BatteryInvalidWakeSeconds')
GAUGE = ('TrueRemainingCapacity', 'PassedCharge', 'ChargeAccum', 'Voltage',
         'PackCurrentAccumulator', 'PackCurrentAccumulatorCount',
         'FilteredCurrent', 'CellCurrentAccumulator', 'CellCurrentAccumulatorCount')

def signed(x):
    if isinstance(x, dict): return {k:signed(v) for k,v in x.items()}
    if isinstance(x, (tuple,list)): return [signed(v) for v in x]
    if isinstance(x, int) and x >= 2**63: return x - 2**64
    return x

def run(*a): return subprocess.check_output(a, text=True)

def sample():
    t0 = time.time()
    data = plistlib.loads(subprocess.check_output(['ioreg', '-a', '-r', '-c', 'AppleSmartBattery']))[0]
    r = {k: data[k] for k in KEYS if k in data}
    r['BatteryData'] = {k:data.get('BatteryData', {})[k] for k in GAUGE if k in data.get('BatteryData', {})}
    return {'time_start':t0, 'time_end':time.time(), 'battery':signed(r)}

def main():
    ap=argparse.ArgumentParser();ap.add_argument('--seconds',type=int,required=True);ap.add_argument('--output',type=Path,required=True)
    a=ap.parse_args();a.output.mkdir(parents=True,exist_ok=False)
    r={'status':'preparing', 'requested_seconds':a.seconds, 'pre':[], 'post':[],
       'os':run('sw_vers'), 'boot':run('sysctl','-n','kern.boottime'),
       'boot_uuid':run('sysctl','-n','kern.bootsessionuuid'),
       'scheduled_before':run('pmset','-g','sched')}
    def save():
        tmp=a.output/'result.tmp';tmp.write_text(json.dumps(r,indent=2)+'\n');tmp.replace(a.output/'result.json')
        os.sync()
    try:
        for _ in range(20):
            r['pre'].append(sample());time.sleep(1)
        # IORegistry snapshots can be cached for about a minute. Start from
        # an observed refresh, not from the time we happened to read the cache.
        previous_update=r['pre'][-1]['battery']['UpdateTime']
        deadline=time.time()+150
        r['waiting_for_fresh_counter']=True;save()
        while True:
            fresh=sample();r['pre'].append(fresh)
            if (fresh['battery']['UpdateTime'] > previous_update and
                    0 <= fresh['time_end']-fresh['battery']['UpdateTime'] <= 2):
                break
            if time.time() >= deadline:
                raise RuntimeError('battery counters did not refresh; no sleep requested')
            time.sleep(.25)
        r['waiting_for_fresh_counter']=False
        assert not r['pre'][-1]['battery']['ExternalConnected']
        assert not r['pre'][-1]['battery']['IsCharging']
        wake=datetime.datetime.now()+datetime.timedelta(seconds=a.seconds)
        r['wake_local']=wake.strftime('%m/%d/%y %H:%M:%S')
        r['owner']='neo-sleep-measure-'+str(os.getpid())
        subprocess.run(['pmset','schedule','wake',r['wake_local'],r['owner']],check=True)
        r['scheduled_after']=run('pmset','-g','sched')
        r['before']=sample()
        assert r['before']['time_end']-r['before']['battery']['UpdateTime'] <= 3
        r['status']='sleep_requested';save()
        r['request_time']=time.time();print('sleep requested, wake '+r['wake_local'],flush=True)
        subprocess.run(['pmset','sleepnow'],check=True)
        r['command_return_time']=time.time()
        # Keep measuring through intermediate maintenance/dark wakes. A first
        # execution gap does not mean the requested interval has finished.
        target=wake.timestamp()
        last=r['request_time'];now=time.time()
        r['execution_gaps']=[];r['early_resumes']=[]
        while True:
            if now-last >= 3:
                r['execution_gaps'].append({'from':last, 'to':now, 'seconds':now-last})
                if now < target-1:
                    r['early_resumes'].append(sample());save()
            if now >= target-1:
                break
            last=now;time.sleep(.2);now=time.time()
        r['observed_execution_gap_seconds']=sum(x['seconds'] for x in r['execution_gaps'])
        r['first_after']=sample();r['status']='returned';save()
        # Retain delayed gauge refresh and awake drain separately.
        for _ in range(35):
            r['post'].append(sample());time.sleep(1)
        r['pmset_log']=run('pmset','-g','log')
        r['scheduled_end']=run('pmset','-g','sched')
        r['boot_end']=run('sysctl','-n','kern.boottime')
        r['boot_uuid_end']=run('sysctl','-n','kern.bootsessionuuid')
        assert r['boot_uuid']==r['boot_uuid_end']
        r['status']='captured'
    except BaseException as e:
        r['status']='error';r['error']=repr(e);raise
    finally:save()
    print('capture complete',flush=True)

if __name__=='__main__':main()
