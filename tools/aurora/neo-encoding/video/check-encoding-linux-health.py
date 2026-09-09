#!/usr/bin/env python3
"""Check firmware health, independent work pinned to every CPU, and Vulkan."""
import argparse
import hashlib
import json
from pathlib import Path
import re
import subprocess

ROOT=Path(__file__).resolve().parents[1]
p=argparse.ArgumentParser(description=__doc__);p.add_argument('output',type=Path)
p.add_argument('--module-binary',type=Path,required=True);a=p.parse_args()
a.output.mkdir(parents=True,exist_ok=False);digest=hashlib.sha256(a.module_binary.read_bytes()).hexdigest()
command=('set -e; cat /sys/devices/system/cpu/online; '
    'for cpu in 0 1 2 3 4 5; do '
    'taskset -c "$cpu" sh -c \'grep Cpus_allowed_list /proc/self/status; sha256sum /tmp/apple-ave.ko\'; done; '
    '/opt/neo/bin/g17p-vulkan-compute; cat /sys/kernel/debug/285100000.video-codec/ave-status; '
    'strings /sys/kernel/debug/285100000.video-codec/ave-firmware-log; dmesg')
with (a.output/'health.log').open('w') as log:
    result=subprocess.run([str(ROOT.parent/'neo-update/.venv/bin/python'),str(ROOT/'console.py'),
        '--timeout','45',command],stdout=log,stderr=subprocess.STDOUT)
assert result.returncode==0
text=(a.output/'health.log').read_text()
assert re.search(r'^0-5$',text,re.M)
assert re.findall(r'^Cpus_allowed_list:\s*([0-9]+)$',text,re.M)==list('012345')
assert len(re.findall(r'^'+digest+r'\s+/tmp/apple-ave.ko$',text,re.M))==6
assert re.search(r'^g17p-vulkan-compute: PASS',text,re.M) and re.search(r'^command_failed=0$',text,re.M)
assert not re.search(r'CRIT:|kernel panic|SError|dart.*(?:fault|translation error)',text)
validation=dict(passed=True,cpus_online='0-5',cpu_affinity_checks=list(range(6)),
    cpu_work='SHA256 of the loaded module artifact with each process pinned to one CPU',
    vulkan_values=32,module_sha256=digest,firmware_command_failed=False)
(a.output/'validation.json').write_text(json.dumps(validation,indent=2)+'\n')
print('ENCODING_CPU_VULKAN_HEALTH',json.dumps(validation))
