#!/usr/bin/env python3
"""Reboot only the M5 controller using an authorized runtime credential.

The credential is read without terminal echo and passed to osascript over
stdin. It is never saved, placed in argv, or included in diagnostic output.
"""
import getpass
import argparse
import json
from pathlib import Path
import subprocess
import sys

helper = Path(__file__).resolve().parent / 'macvdmtool/macvdmtool'
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--debug-usb-only', action='store_true',
                    help='Re-arm M5 DebugUSB without rebooting the target')
args = parser.parse_args()
operation = 'debugusb' if args.debug_usb_only else 'reboot debugusb'
command = f'{helper} --rid 1 {operation}'
password = getpass.getpass('Runtime administrator credential: ')
script = ('do shell script ' + json.dumps(command) + ' password ' +
          json.dumps(password) + ' with administrator privileges\n')
try:
    result = subprocess.run(['/usr/bin/osascript', '-'], input=script,
                            text=True, capture_output=True, timeout=60)
except subprocess.TimeoutExpired:
    print('M5 controller reboot timed out', file=sys.stderr)
    sys.exit(1)
finally:
    password = script = None
if result.returncode:
    print(f'M5 controller reboot failed: exit {result.returncode}', file=sys.stderr)
    sys.exit(result.returncode)
print('M5 debug USB transition completed' if args.debug_usb_only else
      'M5 controller reboot and debug USB transition completed')
