#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-2.0-only
"""Bounded measurement controller for qualified associated-ownership sysfs trials."""
from pathlib import Path
import argparse,errno,json,signal,time,subprocess
p=argparse.ArgumentParser(description=__doc__)
p.add_argument('--output',type=Path,required=True);p.add_argument('--seconds',type=int,default=180)
p.add_argument('--hold-ms',type=int,default=1000)
a=p.parse_args()
if not 1<=a.seconds<=600 or not 1<=a.hold_ms<=2000:p.error('invalid bounded duration')
a.output.mkdir(exist_ok=False)
devs=list(Path('/sys/bus/pci/devices').glob('*/ownership_trial_status'));assert len(devs)==1
dev=devs[0].parent
assert Path('/sys/module/mt7932_fullmac/parameters/awake_own_irq').read_text().strip()=='Y'
assert subprocess.check_output(['iw','dev','wlan0','get','power_save'],text=True).strip()=='Power save: on'
r={'boot':Path('/proc/sys/kernel/random/boot_id').read_text(),'version':Path('/proc/version').read_text(),'trials':[],'busy':0}
stop=False
def end(signum,frame):
 global stop
 stop=True
signal.signal(signal.SIGTERM,end);signal.signal(signal.SIGINT,end)
start=time.monotonic()
try:
 while not stop and time.monotonic()-start<a.seconds:
  t=time.monotonic()
  try:(dev/'ownership_trial_ms').write_text(str(a.hold_ms))
  except OSError as e:
   if e.errno!=errno.EBUSY:raise
   r['busy']+=1;time.sleep(.05);continue
  s=(dev/'ownership_trial_status').read_text().strip()
  r['trials'].append({'time':t,'status':s})
  if not all(v in s for v in ['result=0 ','active=0 ','quiesced=0 ']):raise RuntimeError(s)
  time.sleep(.01)
 r['result']='pass'
except BaseException as e:
 r['result']='failed';r['error']=repr(e);raise
finally:
 r['elapsed']=time.monotonic()-start
 (a.output/'result.json').write_text(json.dumps(r,indent=2)+'\n')
