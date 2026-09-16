#!/usr/bin/env python3
"""Read-only journal experiment on the already installed, verified Neo build."""
import fcntl
import hashlib
import json
from pathlib import Path
import shlex
import subprocess

here = Path(__file__).resolve().parent
config = here.parent/'j700-plasma-keyboard-install-20260913/linux-ssh-config'
boot = '5a46d969-07c9-4cb1-b307-8adae3456a74'
probe_label = 'speed-readback-gate-20260915'
candidate = here/'candidates/libgallium-readback-journal.so'
binary = here/'fast-iterations'/probe_label/'results'/probe_label/'readback-probe'
params = dict(boot=boot, candidate=hashlib.sha256(candidate.read_bytes()).hexdigest(),
              probe=hashlib.sha256(binary.read_bytes()).hexdigest(), label=probe_label)
guard = (here/'.fast-iteration-host.lock').open('r')
fcntl.flock(guard, fcntl.LOCK_EX | fcntl.LOCK_NB)
log = here/'readback-journal-observe-3000.log'
remote = '''
import fcntl,hashlib,json,subprocess,time
from pathlib import Path
P = PARAMS
assert Path('/proc/sys/kernel/random/boot_id').read_text().strip() == P['boot']
assert b'apple,j700' in Path('/proc/device-tree/compatible').read_bytes().split(bytes([0]))
lock = open('/run/neo-gl30-fast-iteration.lock', 'r')
fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
busy = subprocess.run(['pgrep','-x','glcts|cts-runner|readback-probe'],capture_output=True,text=True)
assert busy.returncode == 1 and not busy.stdout
mesa = Path('/opt/neo-mesa-native-gl2-20260913')
assert hashlib.sha256((mesa/'lib/libgallium-26.1.4.so').read_bytes()).hexdigest() == P['candidate']
base = Path('/home/mcruz/gl30-cts-20260913')/P['boot']
state = json.loads((base/'x11-server/state.json').read_text())
assert state['boot'] == P['boot']
assert Path('/proc',str(state['pid']),'comm').read_text().strip() == 'Xorg'
assert str(base/'x11-server/Xorg.log').encode() in Path('/proc',str(state['pid']),'cmdline').read_bytes()
binary = base/P['label']/'readback-probe'
assert hashlib.sha256(binary.read_bytes()).hexdigest() == P['probe']
env = dict(HOME='/home/mcruz',USER='mcruz',LANG='C',PATH='/usr/bin:/bin',
 LD_LIBRARY_PATH=str(mesa)+'/lib:/opt/neo-mesa-native-glx-f4c0b9725bbc/lib/compat',
 LIBGL_DRIVERS_PATH=str(mesa)+'/lib/dri',
 __EGL_VENDOR_LIBRARY_FILENAMES=str(mesa)+'/share/glvnd/egl_vendor.d/50_mesa.json',
 EGL_PLATFORM='x11',DISPLAY=state['display'],XAUTHORITY=state['authority'],
 MESA_SHADER_CACHE_DISABLE='true',LOOP_N='3000',AGX_OBSERVE_READBACK='1',AGX_OBSERVE_BUSY='1')
print('JOURNAL_IDENTITY '+json.dumps(P),flush=True)
start = time.monotonic()
result = subprocess.run(['timeout','90','taskset','-c','0',str(binary)],env=env)
print('JOURNAL_TERMINAL '+json.dumps(dict(exit_code=result.returncode,seconds=time.monotonic()-start)),flush=True)
raise SystemExit(result.returncode)
'''.replace('PARAMS', repr(params))
with log.open('xb') as output:
    result = subprocess.run(['ssh','-F',str(config),'-o','BatchMode=yes','-o','ConnectTimeout=10',
                             'neo-plasma',shlex.join(['python3','-c',remote])],
                            stdout=output,stderr=subprocess.STDOUT)
print(json.dumps(dict(log=str(log), exit_code=result.returncode)))
raise SystemExit(result.returncode)
