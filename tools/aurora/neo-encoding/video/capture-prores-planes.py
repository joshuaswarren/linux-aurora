#!/usr/bin/env python3
"""Collect missing ProRes planes using a second submission in one reference session."""
import argparse
import json
from pathlib import Path
import shlex
import subprocess
import time

ROOT=Path(__file__).resolve().parents[1]
p=argparse.ArgumentParser(description=__doc__)
p.add_argument('hv',type=Path);p.add_argument('probes',type=Path);p.add_argument('output',type=Path)
a=p.parse_args();a.output.mkdir(parents=True,exist_ok=False)
control=ROOT.parent/'neo-update/.ssh-neo-control'
ssh=['ssh','-S',str(control),'-o','BatchMode=yes','admin@admins-MacBook-Neo.local']
remote='/tmp/neo-prores-planes-'+time.strftime('%Y%m%dT%H%M%SZ',time.gmtime())
def guest(command,check=True):
    return subprocess.run(ssh+[command],check=check,capture_output=True,text=True,timeout=15)
def phase(name):
    path=a.hv/'media-control.json';tmp=path.with_suffix('.new')
    tmp.write_text(json.dumps({'phase':name}));tmp.replace(path)
    deadline=time.monotonic()+45
    while time.monotonic()<deadline:
        events=[json.loads(l) for l in (a.hv/'media-mmio.jsonl').read_text().splitlines()[-400:]]
        if any(e.get('kind')=='phase-change' and e.get('phase')==name for e in events):return
        time.sleep(.25)
    raise RuntimeError('No phase acknowledgement; leave the controller running for inspection')
guest('test "$(sysctl -n hw.model)" = Mac17,5 && test "$(sw_vers -buildVersion)" = 25G76 && mkdir '+remote)
scp=['scp','-o',f'ControlPath={control}','-o','BatchMode=yes']
subprocess.run(scp+[str(a.probes/'neo-vt-codec-probe'),'admin@admins-MacBook-Neo.local:'+remote+'/probe'],check=True)
cases=[('apch','422hq'),('apcs','422lt'),('apco','422proxy'),('ap4h','4444'),('ap4x','4444xq'),
       ('ap4h-alpha12','4444'),('ap4x-alpha12','4444xq')]
for name,variant in cases:
    alpha='alpha12' in name;codec=name[:4]
    config={'capture_gate':True}
    if alpha:config.update(preserve_alpha=True,pattern='gradient')
    cfg=a.output/(name+'.json');cfg.write_text(json.dumps(config)+'\n')
    subprocess.run(scp+[str(cfg),'admin@admins-MacBook-Neo.local:'+remote+'/'+cfg.name],check=True)
    phase(name+'-gate-first')
    command=shlex.join([remote+'/probe','encode',codec,remote+'/'+name,'2','y416' if alpha else '2vuy',
        'com.apple.videotoolbox.videoencoder.appleproreshw.'+variant,remote+'/'+cfg.name])
    with (a.output/(name+'.log')).open('w') as log:
        proc=subprocess.Popen(ssh+[command],stdout=log,stderr=subprocess.STDOUT)
        try:
            deadline=time.monotonic()+40
            while time.monotonic()<deadline:
                if proc.poll() is not None:raise RuntimeError(name+' exited before capture gate')
                if guest('test -f '+remote+'/'+name+'/capture-ready',check=False).returncode==0:break
                time.sleep(.5)
            else:raise RuntimeError('Capture gate did not become ready')
            phase(name+'-planes')
            guest('touch '+remote+'/'+name+'/capture-continue')
            if proc.wait(timeout=60):raise RuntimeError(name+' probe failed')
        finally:
            if proc.poll() is None:proc.terminate();proc.wait(timeout=10)
    events=[json.loads(l) for l in (a.hv/'media-mmio.jsonl').read_text().splitlines()]
    ring=next(e for e in events if e.get('kind')=='apr-ring' and e.get('phase')==name+'-planes')
    planes=[e['plane'] for e in events if e.get('kind')=='apr-plane' and e.get('capture')==ring['capture']]
    assert sorted(planes)==sorted(['luma','chroma','alpha'] if alpha else ['luma','chroma'])
    print('PRORES_PLANES_CAPTURED',name,ring['capture'],planes,flush=True)
subprocess.run(scp+['-r','admin@admins-MacBook-Neo.local:'+remote,str(a.output/'guest')],check=True,timeout=180)
print('PRORES_PLANE_MATRIX_COMPLETE',a.output,flush=True)
