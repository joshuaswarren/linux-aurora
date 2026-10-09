#!/usr/bin/env python3
"""Compile an artifact-only probe with the isolated tree's actual test flags."""
from pathlib import Path
import argparse
import hashlib
import json
import subprocess

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('kind', choices=('mask', 'kernel'))
parser.add_argument('--upstream', action='store_true')
args = parser.parse_args()
HERE = Path(__file__).resolve().parent
label = 'upstream' if args.upstream else 'sync'
root = '/home/mcruz.guest/neo'
build = root + '/build/mesa-j700-agx-' + label + '-20260916'
source = root + '/sources/mesa-j700-agx-' + label + '-20260916'
target_name = 'asahi-g17p-early-demote-epilog' if args.kind == 'mask' else 'asahi-g17p-precompiled-fill'
test_file = 'test-agx-g17p-early-demote-epilog.c' if args.kind == 'mask' else 'test-g17p-precompiled-fill.c'
probe = HERE / ('agx-sync-' + args.kind + '-dump.c')
remote_probe = build + '/artifact-' + args.kind + '-dump.c'
subprocess.run(['limactl', 'copy', str(probe), 'neo-builder:' + remote_probe], check=True)
program = r'''
import json,subprocess,shlex,sys
build,source,target_name,test_file,probe,extra=sys.argv[1:]
entries=json.load(open(build+'/compile_commands.json'))
entry=next(x for x in entries if x['file'].endswith('/'+test_file))
cc=shlex.split(entry['command'])
original=cc[cc.index('-o')+1]
obj=build+'/artifact-'+target_name+'.o'
exe=build+'/artifact-'+target_name
for flag in ('-o','-MQ','-MF'):
 if flag in cc:cc[cc.index(flag)+1] = obj + ('.d' if flag=='-MF' else '')
cc[cc.index('-c')+1]=probe
cc.extend(['-I'+source]+(['-DJ700_EXTRA_KERNEL_METADATA'] if extra=='yes' else []))
subprocess.run(cc,cwd=build,check=True)
target='src/asahi/lib/'+target_name
raw=subprocess.check_output(['ninja','-C',build,'-t','commands',target],text=True)
link=shlex.split(raw.splitlines()[-1])
assert original in link
link[link.index(original)]=obj
link[link.index('-o')+1]=exe
subprocess.run(link,cwd=build,check=True)
status=subprocess.run([exe],cwd=build).returncode
print('PROBE_EXIT',status)
sys.exit(status)
'''
log = HERE.parent / f'agx-{label}-{args.kind}-probe.log'
with log.open('w') as out:
    result = subprocess.run(['limactl', 'shell', 'neo-builder', 'python3', '-c', program,
                             build, source, target_name, test_file, remote_probe,
                             'no' if args.upstream else 'yes'],
                            stdout=out, stderr=subprocess.STDOUT)
log.with_suffix('.json').write_text(json.dumps({'source_sha256': hashlib.sha256(probe.read_bytes()).hexdigest(),
    'source': str(probe), 'build': build, 'hardware_execution': False,
    'exit_code': result.returncode, 'log': str(log)}, indent=2) + '\n')
print(log, 'exit', result.returncode)
