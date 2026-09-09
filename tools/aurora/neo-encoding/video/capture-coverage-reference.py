#!/usr/bin/env python3
"""Run a prepared, bounded reference matrix through the existing Neo SSH master."""
import argparse
import json
from pathlib import Path
import shlex
import subprocess
import time

ROOT = Path(__file__).resolve().parents[1]
p = argparse.ArgumentParser(description=__doc__)
p.add_argument('hv', type=Path)
p.add_argument('probes', type=Path)
p.add_argument('output', type=Path)
a = p.parse_args()
a.output.mkdir(parents=True, exist_ok=False)
control = ROOT.parent / 'neo-update/.ssh-neo-control'
ssh = ['ssh', '-S', str(control), '-o', 'BatchMode=yes', 'admin@admins-MacBook-Neo.local']
remote = '/tmp/neo-coverage-' + time.strftime('%Y%m%dT%H%M%SZ', time.gmtime())
manifest = []

def run(args, name, timeout=150, accepted=(0,)):
    with (a.output / (name + '.log')).open('w') as f:
        result = subprocess.run([str(x) for x in args], stdout=f, stderr=subprocess.STDOUT, timeout=timeout)
    if result.returncode not in accepted:
        raise RuntimeError(f'{name} exited {result.returncode}; no automatic retry')
    return result.returncode

def guest(command, name, **kwargs):
    return run(ssh + [command], name, **kwargs)

def phase(name):
    path = a.hv / 'media-control.json'
    temporary = path.with_suffix('.new')
    temporary.write_text(json.dumps({'phase': name}))
    temporary.replace(path)
    (a.hv / 'ave-phase.json').write_text(json.dumps({'phase': name}))
    deadline = time.monotonic() + 45
    while time.monotonic() < deadline:
        lines = (a.hv / 'media-mmio.jsonl').read_text().splitlines()
        if any(json.loads(line).get('kind') == 'phase-change' and json.loads(line).get('phase') == name for line in lines[-400:]):
            return
        time.sleep(0.25)
    raise RuntimeError('Hypervisor did not acknowledge phase; do not run an unlabelled capture')

guest('test "$(sysctl -n hw.model)" = Mac17,5 && test "$(sw_vers -buildVersion)" = 25G76 && mkdir '+remote, 'target')
run(['scp', '-o', f'ControlPath={control}', '-o', 'BatchMode=yes', a.probes/'neo-vt-codec-probe',
     a.probes/'neo-audio-probe', 'admin@admins-MacBook-Neo.local:'+remote+'/'], 'copy-probes')
guest(f'{remote}/neo-vt-codec-probe list {remote}/encoders.json', 'enumeration')
phase('audio-inventory')
guest(f'{remote}/neo-audio-probe encode-hardware {remote}/audio', 'audio', timeout=600)
base='com.apple.videotoolbox.videoencoder.'
cases=[]
for codec, variant in [('apcn','422'),('apch','422hq'),('apcs','422lt'),('apco','422proxy'),('ap4h','4444'),('ap4x','4444xq')]:
    cases.append((codec, codec, '2vuy', base+'appleproreshw.'+variant, {}))
cases += [
    ('jpeg','jpeg','nv12',base+'jpeg.ajpeg',{}),
    ('ap4h-alpha12','ap4h','y416',base+'appleproreshw.4444',{'preserve_alpha':True,'pattern':'gradient'}),
    ('ap4x-alpha12','ap4x','y416',base+'appleproreshw.4444xq',{'preserve_alpha':True,'pattern':'gradient'}),
    ('hevc-main10','hvc1','p010',base+'ave.hevc',{'profile':'HEVC_Main10_AutoLevel','output_bit_depth':10,'pattern':'gradient'}),
    ('hevc-main42210','hvc1','p210',base+'ave.hevc',{'profile':'HEVC_Main42210_AutoLevel','output_bit_depth':10,'pattern':'gradient'}),
    ('hevc-alpha','muxa','bgra',base+'hevc-with-alpha:'+base+'ave.hevc',{'preserve_alpha':True,'pattern':'gradient'}),
    ('hevc-depth','deph','fdep',base+'depth-hevc:'+base+'ave.hevc',{}),
    ('hevc-disparity','dish','fdis',base+'disparity-hevc:'+base+'ave.hevc',{}),
    ('hevc-multiview','hvc1','nv12',base+'ave.hevc',{'multiview':True,'pattern':'gradient'}),
]
for name,codec,pixels,encoder,config in cases:
    phase(name)
    if name == 'hevc-main10':
        (a.hv/'ave-command-control.json').write_text(json.dumps({'op':'arm'}))
        deadline=time.monotonic()+45
        while time.monotonic()<deadline:
            if any(json.loads(l).get('kind')=='armed' for l in (a.hv/'ave-commands.jsonl').read_text().splitlines()): break
            time.sleep(.25)
        else: raise RuntimeError('AVE capture did not arm; inspect entry verification')
    config_path=a.output/(name+'-config.json')
    config_path.write_text(json.dumps(config,indent=2)+'\n')
    run(['scp','-o',f'ControlPath={control}','-o','BatchMode=yes',config_path,
         'admin@admins-MacBook-Neo.local:'+remote+'/'+config_path.name],name+'-config')
    command=shlex.join([remote+'/neo-vt-codec-probe','encode',codec,remote+'/'+name,'3',pixels,encoder,remote+'/'+config_path.name])
    code=guest(command,name,accepted=(0,1,2))
    manifest.append(dict(name=name,codec=codec,input=pixels,configuration=config,exit_code=code))
    (a.output/'attempts.json').write_text(json.dumps(manifest,indent=2)+'\n')
    print('REFERENCE_ATTEMPT',name,'exit',code,flush=True)
(a.hv/'ave-command-control.json').write_text(json.dumps({'op':'disarm'}))
(a.hv/'media-control.json').write_text(json.dumps({'op':'disarm'}))
run(['scp','-r','-o',f'ControlPath={control}','-o','BatchMode=yes',
     'admin@admins-MacBook-Neo.local:'+remote,a.output/'guest'], 'download', timeout=240)
print('REFERENCE_MATRIX_COMPLETE',a.output,flush=True)
