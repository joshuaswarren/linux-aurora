#!/usr/bin/env python3
"""Capture one bounded hardware-required H.264 reference on the M5 guest.

Run beside the sole HV controller after wait-native-network.py succeeds.
Control files are acknowledged by that controller before encoding starts.
All binary output and trace data remain in the private campaign directory.
"""
import argparse
import json
import os
from pathlib import Path
import shlex
import subprocess
import time

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('trace', type=Path)
args = parser.parse_args()
os.umask(0o077)
directory = args.trace.resolve()
root = Path(__file__).resolve().parent.parent
host = 'admin@Admins-MacBook-Air.local'
ssh_options = ['-o', 'BatchMode=yes', '-o', 'ConnectTimeout=5',
               '-o', 'ServerAliveInterval=5', '-o', 'ServerAliveCountMax=2']


def ssh(command, timeout=30):
    result = subprocess.run(['ssh', *ssh_options, host, command],
                            capture_output=True, text=True, timeout=timeout)
    if result.returncode:
        raise RuntimeError('Remote M5 command failed with status ' + str(result.returncode))
    return result.stdout


def control(name, value):
    temporary = directory / (name + '.tmp')
    temporary.write_text(json.dumps(value) + '\n')
    temporary.replace(directory / name)


def wait_control(state, phase, timeout=90):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        path = directory / 'ave-control-events.jsonl'
        if path.exists():
            lines = path.read_text().splitlines()
            if lines:
                row = json.loads(lines[-1])
                if row.get('command_state') == state and row.get('phase') == phase:
                    if state == 'disarmed' and row['pending_publications']:
                        raise RuntimeError('Capture has unpublished pending ring sends')
                    return
        time.sleep(1)
    raise RuntimeError('HV controller did not acknowledge phase/state: ' + phase + '/' + state)


if directory.stat().st_mode & 0o077:
    raise RuntimeError('Trace directory must be private')
if ssh('/usr/sbin/sysctl -n hw.model kern.osversion').splitlines() != ['Mac17,3', '25G76']:
    raise RuntimeError('SSH target is not the pinned M5 macOS guest')
remote = '/tmp/j813-video-' + str(time.time_ns())
ssh('umask 077; mkdir ' + shlex.quote(remote))
probe = root / 'artifacts/video/probes/m5-vt-codec-probe'
subprocess.run(['scp', *ssh_options, str(probe), host + ':' + remote + '/probe'],
               check=True, timeout=45, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
ssh('chmod 700 ' + remote + '/probe; ' + remote + '/probe list ' + remote + '/encoders.json')
inventory = json.loads(ssh('cat ' + remote + '/encoders.json'))
(directory / 'm5-encoders.json').write_text(json.dumps(inventory, indent=2) + '\n')
phase = 'h264-320x240-nv12-3frames'
control('ave-phase.json', {'phase': phase})
control('ave-command-control.json', {'op': 'arm'})
wait_control('armed', phase)
print('M5_H264_CAPTURE_ARMED', flush=True)
result = None
try:
    result = subprocess.run(['ssh', *ssh_options, host,
        remote + '/probe encode avc1 ' + remote + '/h264 3 nv12'],
        capture_output=True, text=True, timeout=180)
    (directory / 'h264-probe-console.log').write_text(result.stdout + result.stderr)
finally:
    control('ave-command-control.json', {'op': 'disarm'})
    wait_control('disarmed', phase)
output = directory / 'reference'
output.mkdir(mode=0o700)
subprocess.run(['scp', *ssh_options, '-r', host + ':' + remote + '/h264', str(output)],
               check=True, timeout=60, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
report = json.loads((output / 'h264/result.json').read_text())
(directory / 'h264-capture-receipt.json').write_text(json.dumps({
    'ssh_status': result.returncode, 'phase': phase,
    'probe': report, 'native_linux_encoding_proven': False}, indent=2) + '\n')
print('M5_H264_REFERENCE_CAPTURED ssh_status=' + str(result.returncode), flush=True)
if result.returncode:
    raise SystemExit(result.returncode)
