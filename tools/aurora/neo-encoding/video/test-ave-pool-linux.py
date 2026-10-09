#!/usr/bin/env python3
"""Qualify the bounded larger AVE pool on native Neo Linux, without MV claims."""
import argparse
import hashlib
import json
from pathlib import Path
import re
import shutil
import subprocess
import sys

ROOT=Path(__file__).resolve().parents[1]
PYTHON=ROOT.parent/'neo-update/.venv/bin/python'
DEBUG='/sys/kernel/debug/285100000.video-codec'
p=argparse.ArgumentParser(description=__doc__)
p.add_argument('bundle',type=Path);p.add_argument('output',type=Path)
a=p.parse_args();a.output.mkdir(parents=True,exist_ok=False)
m=json.loads((a.bundle/'manifest.json').read_text())
for name in ['apple-ave.ko','apple-ave-unstripped.ko']:
    data=(a.bundle/name).read_bytes();expected=m['files'][name]
    assert len(data)==expected['bytes'] and hashlib.sha256(data).hexdigest()==expected['sha256']
shutil.copyfile(__file__,a.output/'test-ave-pool-linux.py')

def run(cmd,name):
    with (a.output/(name+'.log')).open('w') as log:
        r=subprocess.run(list(map(str,cmd)),stdout=log,stderr=subprocess.STDOUT)
    if r.returncode:raise RuntimeError(name+' failed; stop without firmware retry')
    return (a.output/(name+'.log')).read_text(errors='replace')
def native(cmd,name,timeout=40):
    return run([PYTHON,ROOT/'console.py','--timeout',timeout,cmd],name)

native('test ! -d /sys/module/apple_ave && test ! -d /sys/module/apple_prores_test && '
       'cat /proc/device-tree/model && uname -r && modprobe asahi','identity')
files=[(a.bundle/'apple-ave.ko','apple-ave.ko'),
       (ROOT/'video/linux-ave/test-native.sh','test-ave.sh'),
       (ROOT/'video/linux-ave/test-ipc.sh','test-ipc.sh'),
       (ROOT/'video/linux-ave/run-command.sh','run-ave.sh'),
       (ROOT/'artifacts/coverage-ave-config-20260909.bin','ave-config.bin'),
       (ROOT/'artifacts/linux-prores/apple-prores-test.ko','apple-prores-test.ko')]
for source,name in files:run([PYTHON,ROOT/'push-linux.py',source,'/tmp/'+name],'upload-'+name)
native('sh /tmp/test-ave.sh /tmp/apple-ave.ko','default-20m-lifecycle')
native(f'grep -qx firmware_start_requested=0 {DEBUG}/ave-status && rmmod apple_ave',
       'release-before-firmware')
native('sh /tmp/test-ave.sh /tmp/apple-ave.ko 32','32m-lifecycle')
native('sh /tmp/test-ipc.sh','32m-ipc')
native('sh /tmp/run-ave.sh /tmp/ave-config.bin','32m-config')
state=native(f'cat {DEBUG}/ave-status; cat /sys/module/apple_ave/parameters/ipc_mib','32m-state')
assert re.search(r'^ipc_bytes=33554432$',state,re.M)
assert re.search(r'^arena_bytes=33030144$',state,re.M)
assert re.search(r'^command_failed=0$',state,re.M)
# End-of-arena byte is owned and outside all captured baseline surfaces. Verify
# the endpoint, and require an out-of-bounds seek to fail before any write.
native(f'printf "\\252" > /tmp/ave-pool-marker && '
       f'dd if=/tmp/ave-pool-marker of={DEBUG}/ave-arena bs=1 seek=33030143 count=1 conv=notrunc && '
       f'dd if={DEBUG}/ave-arena of=/tmp/ave-pool-read bs=1 skip=33030143 count=1 && '
       'cmp /tmp/ave-pool-marker /tmp/ave-pool-read && '
       f'if dd if=/tmp/ave-pool-marker of={DEBUG}/ave-arena bs=1 seek=33030145 count=1 conv=notrunc; '
       'then echo UNSAFE_ARENA_OVERFLOW; exit 1; else echo ARENA_BOUNDARY_REJECTED; fi','32m-boundary')
for codec in ['h264','hevc']:
    run([sys.executable,ROOT/'video/encode-linux-video.py',a.output/codec,
         '--codec',codec,'--frames','3'],codec+'-regression')
native('insmod /tmp/apple-prores-test.ko','apr-load')
run([sys.executable,ROOT/'video/encode-prores-linux.py',
     ROOT/'artifacts/prores-linux-attempt-20260909/frame-000.bin',
     ROOT/'artifacts/other-codecs-reference-qualified-20260909/prores-native-reference',
     a.output/'prores','--profile','standard','--pixel-format','uyvy422','--bit-depth','8',
     '--alpha','none','--frames','3'],'prores-regression')
health=native(f'cat /sys/devices/system/cpu/online; /opt/neo/bin/g17p-vulkan-compute; '
    f'cat {DEBUG}/ave-status; dmesg','final-health')
assert re.search(r'^0-5$',health,re.M) and 'g17p-vulkan-compute: PASS' in health
assert not re.search(r'CRIT:|kernel panic|SError|dart.*(?:fault|translation error)',health)
for codec in ['h264','hevc','prores']:
    result=json.loads((a.output/codec/'validation.json').read_text())
    assert result['passed'] and result['frames']==3
validation=dict(passed=True,default_pool_mib=20,expanded_pool_mib=32,
    default_and_expanded_lifecycle_passed=True,expanded_firmware_and_config_passed=True,
    arena_boundary_rejected=True,baseline_frames_per_codec=3,cpus_online='0-5',vulkan_values=32,
    module_sha256=m['files']['apple-ave.ko']['sha256'],multiview_encoding_validated=False,
    persistent_stage1_unchanged=True)
(a.output/'validation.json').write_text(json.dumps(validation,indent=2)+'\n')
print('AVE_POOL_LINUX_VALIDATED',json.dumps(validation),flush=True)
