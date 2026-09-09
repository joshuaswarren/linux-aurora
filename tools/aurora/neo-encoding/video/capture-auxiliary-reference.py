#!/usr/bin/env python3
"""Collect only missing alpha, auxiliary, multiview and audio reference evidence."""
import argparse
import hashlib
import json
from pathlib import Path
import shlex
import subprocess
import time

ROOT=Path(__file__).resolve().parents[1]
p=argparse.ArgumentParser(description=__doc__)
p.add_argument('hv',type=Path);p.add_argument('probes',type=Path);p.add_argument('output',type=Path)
p.add_argument('--skip-prores',action='store_true',help='Reuse already recovered ProRes input planes')
p.add_argument('--skip-audio',action='store_true',help='Reuse the completed audio selection guard inventory')
a=p.parse_args();a.output.mkdir(parents=True,exist_ok=False)
(a.output/'capture-runner.py').write_bytes(Path(__file__).read_bytes())
control=ROOT.parent/'neo-update/.ssh-neo-control'
ssh=['ssh','-S',str(control),'-o','BatchMode=yes','admin@admins-MacBook-Neo.local']
scp=['scp','-o',f'ControlPath={control}','-o','BatchMode=yes']
remote='/tmp/neo-auxiliary-'+time.strftime('%Y%m%dT%H%M%SZ',time.gmtime())
(a.output/'remote.json').write_text(json.dumps({'directory':remote})+'\n')
attempts=[]

def run(command,name,accepted=(0,),timeout=180):
    with (a.output/(name+'.log')).open('w') as log:
        code=subprocess.run(list(map(str,command)),stdout=log,stderr=subprocess.STDOUT,timeout=timeout).returncode
    if code not in accepted:raise RuntimeError(f'{name}: exit {code}; no automatic retry')
    return code

def phase(name):
    for file,content in [('media-control.json',{'phase':name}),('ave-phase.json',{'phase':name})]:
        path=a.hv/file;tmp=path.with_suffix('.new');tmp.write_text(json.dumps(content));tmp.replace(path)
    deadline=time.monotonic()+45
    while time.monotonic()<deadline:
        events=[json.loads(l) for l in (a.hv/'media-mmio.jsonl').read_text().splitlines()[-400:]]
        if any(e.get('kind')=='phase-change' and e.get('phase')==name for e in events):return
        time.sleep(.25)
    raise RuntimeError('No reference phase acknowledgement')

run(ssh+['test "$(sysctl -n hw.model)" = Mac17,5 && test "$(sw_vers -buildVersion)" = 25G76 && '
    'sysctl hw.model hw.ncpu hw.activecpu && sw_vers && mkdir '+remote],'target')
probe_names=['neo-vt-codec-probe','neo-vt-decode-probe','neo-audio-probe']
probe_hashes={name:hashlib.sha256((a.probes/name).read_bytes()).hexdigest() for name in probe_names}
(a.output/'probe-binaries.json').write_text(json.dumps(dict(bundle=str(a.probes.resolve()),sha256=probe_hashes),indent=2)+'\n')
run(scp+[a.probes/name for name in probe_names]+
    ['admin@admins-MacBook-Neo.local:'+remote+'/'],'copy-probes')
checks=' && '.join('test "$(shasum -a 256 '+shlex.quote(remote+'/'+name)+
    ' | cut -d " " -f 1)" = '+digest for name,digest in probe_hashes.items())
run(ssh+[checks],'verify-probes')
base='com.apple.videotoolbox.videoencoder.'
cases=[
    ('ap4h-alpha12','ap4h','y416',base+'appleproreshw.4444',{'preserve_alpha':True,'pattern':'gradient'}),
    ('ap4x-alpha12','ap4x','y416',base+'appleproreshw.4444xq',{'preserve_alpha':True,'pattern':'gradient'}),
    ('hevc-alpha','muxa','bgra',base+'hevc-with-alpha:'+base+'ave.hevc',{'preserve_alpha':True,'pattern':'gradient'}),
    ('hevc-depth','deph','fdep',base+'depth-hevc:'+base+'ave.hevc',{}),
    ('hevc-disparity','dish','fdis',base+'disparity-hevc:'+base+'ave.hevc',{}),
    ('hevc-multiview','hvc1','nv12',base+'ave.hevc',{'multiview':True,'pattern':'gradient'}),
]
try:
    if not a.skip_audio:
        phase('audio-selection-guard')
        code=run(ssh+[remote+'/neo-audio-probe encode-hardware '+remote+'/audio'],'audio',accepted=(0,1),timeout=600)
        attempts.append(dict(name='audio',exit_code=code))
    for name,codec,pixels,encoder,config in cases:
        if a.skip_prores and codec in ('ap4h','ap4x'):continue
        phase(name)
        if name=='hevc-alpha':
            (a.hv/'ave-command-control.json').write_text(json.dumps({'op':'arm'}))
            deadline=time.monotonic()+45
            while time.monotonic()<deadline:
                events=[json.loads(l) for l in (a.hv/'ave-commands.jsonl').read_text().splitlines()]
                if any(e.get('kind')=='armed' for e in events):break
                time.sleep(.25)
            else:raise RuntimeError('AVE observer did not arm')
        cfg=a.output/(name+'-config.json');cfg.write_text(json.dumps(config,indent=2)+'\n')
        run(scp+[cfg,'admin@admins-MacBook-Neo.local:'+remote+'/'+cfg.name],name+'-config')
        command=shlex.join([remote+'/neo-vt-codec-probe','encode',codec,remote+'/'+name,'3',pixels,encoder,remote+'/'+cfg.name])
        code=run(ssh+[command],name,accepted=(0,1,2))
        run(scp+['admin@admins-MacBook-Neo.local:'+remote+'/'+name+'/result.json',a.output/(name+'-result.json')],name+'-result')
        result=json.loads((a.output/(name+'-result.json')).read_text())
        attempts.append(dict(name=name,exit_code=code,result=result))
        (a.output/'attempts.json').write_text(json.dumps(attempts,indent=2)+'\n')
        # Unsupported creation/properties end only this configuration. A
        # submitted encoder failure ends the batch until reboot/diagnosis.
        if code and not result.get('create_status') and not result.get('prepare_status'):
            raise RuntimeError(name+' failed after preparing the encoder; stop hardware attempts')
        print('AUXILIARY_REFERENCE_ATTEMPT',name,code,flush=True)
    # Decode after encoding captures: decoder traffic cannot contaminate the
    # encoder command inventory, and unavailable decoder APIs remain recorded.
    (a.hv/'ave-command-control.json').write_text(json.dumps({'op':'disarm'}))
    phase('independent-decode')
    for name,pixels in [('hevc-alpha','bgra'),('hevc-depth','fdep'),('hevc-disparity','fdis')]:
        result=json.loads((a.output/(name+'-result.json')).read_text())
        if not result.get('passed'):continue
        command=shlex.join([remote+'/neo-vt-decode-probe',remote+'/'+name+'/video.mov',remote+'/'+name+'-decoded',pixels])
        code=run(ssh+[command],name+'-decode',accepted=(0,1))
        print('AUXILIARY_DECODE_ATTEMPT',name,code,flush=True)
finally:
    (a.hv/'ave-command-control.json').write_text(json.dumps({'op':'disarm'}))
    (a.hv/'media-control.json').write_text(json.dumps({'op':'disarm'}))
    run(scp+['-r','admin@admins-MacBook-Neo.local:'+remote,a.output/'guest'],'download',timeout=240)
print('AUXILIARY_REFERENCE_BATCH_SAVED',a.output,flush=True)
