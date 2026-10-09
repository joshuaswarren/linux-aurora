#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-2.0-only
"""Summarize a macos-sleep-measure capture without treating cached data as fresh.

PackCurrentAccumulator is treated as a sum of mA samples and SystemLoad as a
sum of mW samples; counts provide independent means. Report sample rates and
agreement, not an externally calibrated energy-meter claim. Returned capacity
estimates may rise through relaxation and are not used as energy counters.
"""
import argparse
import datetime
import json
import re
from pathlib import Path

def main():
    ap=argparse.ArgumentParser();ap.add_argument('capture',type=Path);ap.add_argument('--output',type=Path)
    args=ap.parse_args();r=json.loads(args.capture.read_text())
    a=r['before']['battery'];before=r['before']
    samples=[r['first_after'],*r['post']]
    end=next((x for x in samples if x['battery']['UpdateTime']>=r['request_time']+r['requested_seconds']-3),None)
    if end is None:raise RuntimeError('no fresh endpoint covering the requested interval')
    b=end['battery'];dt=b['UpdateTime']-a['UpdateTime']
    dq=a['BatteryData']['PackCurrentAccumulator']-b['BatteryData']['PackCurrentAccumulator']
    nc=b['BatteryData']['PackCurrentAccumulatorCount']-a['BatteryData']['PackCurrentAccumulatorCount']
    ds=b['PowerTelemetryData']['AccumulatedSystemLoad']-a['PowerTelemetryData']['AccumulatedSystemLoad']
    np=b['PowerTelemetryData']['SystemLoadAccumulatorCount']-a['PowerTelemetryData']['SystemLoadAccumulatorCount']
    if min(dt,dq,nc,ds,np)<=0:raise RuntimeError('invalid or reset counters')
    # Wall-clock boot timestamps can be adjusted by time synchronization.
    if r.get('boot_uuid') is not None:
        if r['boot_uuid']!=r.get('boot_uuid_end'):raise RuntimeError('boot UUID changed')
    elif r['boot']!=r['boot_end']:
        raise RuntimeError('legacy capture boot timestamp changed; manual review required')
    if a['ExternalConnected'] or b['ExternalConnected'] or a['IsCharging'] or b['IsCharging']:
        raise RuntimeError('external power/charging endpoint')
    v=(a['Voltage']+b['Voltage'])/2000
    current_w=dq/nc*v/1000;power_w=ds/np/1000
    age=before['time_end']-a['UpdateTime']
    report={'status':'estimated' if age<=3 else 'contaminated_start',
            'counter_interval_s':dt,'before_counter_age_s':age,
            'end_counter_time':b['UpdateTime'], 'first_fresh_read_time':end['time_end'],
            'mean_voltage_V':v,'pack_current_sum_delta':dq,'pack_sample_count_delta':nc,
            'system_power_sum_delta':ds,'system_power_sample_count_delta':np,
            'pack_sample_rate_Hz':nc/dt,'power_sample_rate_Hz':np/dt,
            'current_counter_mean_W':current_w,'system_power_counter_mean_W':power_w,
            'estimated_energy_Wh_current':current_w*dt/3600,
            'estimated_energy_Wh_power':power_w*dt/3600,
            'first_fresh_system_power_W':b['PowerTelemetryData']['SystemLoad']/1000,
            'execution_gaps':r.get('execution_gaps'),
            'early_resumes':len(r.get('early_resumes',[])),
            'limitations':['Includes entry/wake overhead within the counter interval.',
                          'Counter units/sample means cross-checked internally; not external metrology.',
                          'Use pmset log to verify sleep and any maintenance wakes.']}
    pre=r['pre'][0]['battery']
    nd=a['BatteryData']['PackCurrentAccumulatorCount']-pre['BatteryData']['PackCurrentAccumulatorCount']
    if nd>0:
        qd=pre['BatteryData']['PackCurrentAccumulator']-a['BatteryData']['PackCurrentAccumulator']
        report['awake_crosscheck']={
            'sample_count':nd,
            'counter_mean_discharge_mA':qd/nd,
            'reported_start_discharge_mA':-pre['Amperage'],
            'reported_end_discharge_mA':-a['Amperage'],
            'counter_timestamp_seconds':a['UpdateTime']-pre['UpdateTime']}
    events=[]
    for line in r['pmset_log'].splitlines():
        match=re.match(r'^(.{25})\s+(Sleep|Wake Requests|WakeTime|Wake|DarkWake)\s',line)
        if not match:continue
        try:stamp=datetime.datetime.strptime(match[1],'%Y-%m-%d %H:%M:%S %z').timestamp()
        except ValueError:continue
        if r['request_time']-2<=stamp<=r['post'][-1]['time_end']+2:
            events.append({'epoch':stamp,'type':match[2],'line':line})
    report['pmset_events']=events
    report['logged_sleep_seconds']=sum(int(m[1]) for e in events if e['type']=='Sleep'
        for m in [re.search(r'(\d+) secs',e['line'])] if m)
    report['logged_dark_wakes']=sum(e['type']=='DarkWake' for e in events)
    print(json.dumps(report,indent=2))
    if args.output:args.output.write_text(json.dumps(report,indent=2)+'\n')

if __name__=='__main__':main()
