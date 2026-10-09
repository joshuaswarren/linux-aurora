#!/usr/bin/env python3
"""Diagnose native EGL device selection; no rendering tests or capability overrides."""
import json
import os
from pathlib import Path
import re
import subprocess
import sys

boot = sys.argv[1]
assert os.geteuid() == 0
assert Path('/proc/sys/kernel/random/boot_id').read_text().strip() == boot
assert b'apple,j700' in Path('/proc/device-tree/compatible').read_bytes().split(b'\0')
out = Path('/home/mcruz/gl30-cts-20260913')/boot/'x11-driver-probes'
out.mkdir(exist_ok=False)
state = json.loads((out.parent/'x11-server/state.json').read_text())
assert state['boot'] == boot
mesa = '/opt/neo-mesa-native-gl2-20260913'
base = dict(HOME='/home/mcruz', USER='mcruz', PATH='/opt/neo-piglit-full/waffle/bin:/usr/bin:/bin', LANG='C',
    DISPLAY=state['display'], XAUTHORITY=state['authority'], EGL_PLATFORM='x11',
    LD_LIBRARY_PATH=mesa+'/lib:/opt/neo-mesa-native-glx-f4c0b9725bbc/lib/compat:/opt/neo-piglit-full/waffle/lib',
    LIBGL_DRIVERS_PATH=mesa+'/lib/dri', __EGL_VENDOR_LIBRARY_FILENAMES=mesa+'/share/glvnd/egl_vendor.d/50_mesa.json',
    EGL_LOG_LEVEL='debug', LIBGL_DEBUG='verbose')
fault = re.compile(rb'asahi[^\n]*(?:halted GMMU|completion timed out|retiring slot)|translation fault|GPU FAULT')
results = []
for name, options in [
    ('asahi', dict(GALLIUM_DRIVER='asahi', MESA_LOADER_DRIVER_OVERRIDE='asahi')),
    ('automatic', {}),
    ('prime', dict(DRI_PRIME='1')),
    ('prime-asahi', dict(DRI_PRIME='1', GALLIUM_DRIVER='asahi', MESA_LOADER_DRIVER_OVERRIDE='asahi')),
]:
    assert not fault.search(subprocess.check_output(['dmesg']))
    env = dict(base, **options)
    command = ['runuser', '-u', 'mcruz', '--', 'env', '-i']+[k+'='+v for k,v in env.items()]
    command += ['wflinfo', '--platform', 'x11_egl', '--api', 'gl', '--profile', 'compat', '--verbose']
    p = subprocess.run(command, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, timeout=30)
    (out/(name+'.log')).write_bytes(p.stdout)
    results.append(dict(name=name, exit=p.returncode, native=b'Apple A18 Pro' in p.stdout,
                        software=any(x in p.stdout.lower() for x in (b'llvmpipe', b'softpipe'))))
    assert not fault.search(subprocess.check_output(['dmesg']))
(out/'receipt.json').write_text(json.dumps(results, indent=2)+'\n')
print(json.dumps(results))
