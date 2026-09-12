#!/usr/bin/env python3
"""Run one explicit DCP adoption probe on the guarded J813 RAM kernel."""
from pathlib import Path
import argparse
import subprocess
import sys
import time

root = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(root / 'm1n1/proxyclient'))
from m1n1.proxy import Serial

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--trace-rpc', type=int, default=0,
                    help='Bounded startup RPC trace budget, at most 256 records')
parser.add_argument('--live-discovery', action='store_true',
                    help='Test the native INIT/HELLO handshake without resetting DCP')
args = parser.parse_args()
if not 0 <= args.trace_rpc <= 256:
    parser.error('--trace-rpc must be between 0 and 256')
trace_option = f' trace_rpc={args.trace_rpc} trace_calls=1' if args.trace_rpc else ''
if args.live_discovery:
    trace_option += ' live_discovery=1'

device = '/dev/cu.kis-01100000-ch-0'
owners = subprocess.run(['lsof', '-t', device, device.replace('/cu.', '/tty.')],
                        capture_output=True, text=True)
if owners.stdout.strip():
    raise RuntimeError('M5 console already has an owner')
command = (
    'echo M5_DCP_PROBE_BEGIN; '
    'if grep -q apple,j813 /proc/device-tree/compatible '
    '&& [ $(uname -r) = 7.1.6-aurora-j813-dcp-v1 ] '
    '&& [ ! -d /sys/module/appledrm ]; then '
    f'uname -a; timeout 45 modprobe appledrm{trace_option}; echo M5_DCP_MODPROBE_RETURN=$?; '
    'sleep 5; echo M5_DCP_CONNECTORS; '
    'for d in /sys/class/drm/card*-*; do '
    '[ -f "$d/status" ] || continue; echo "$d"; cat "$d/status" "$d/modes"; done; '
    'echo M5_DCP_DEVICES; ls -l /dev/dri; '
    'echo M5_DCP_KERNEL_LOG; dmesg; '
    'else echo M5_DCP_REFUSED_WRONG_TARGET_OR_ALREADY_LOADED; fi; '
    'echo M5_DCP_PROBE_DONE\n'
)
captured = bytearray()
with Serial(device, 115200, timeout=0.2) as port:
    port.write(command.encode())
    deadline = time.monotonic() + 90
    while time.monotonic() < deadline:
        data = port.read(65536)
        if not data:
            continue
        captured.extend(data)
        sys.stdout.buffer.write(data)
        sys.stdout.flush()
        if b'\nM5_DCP_PROBE_DONE\r\n' in captured:
            break
    else:
        raise RuntimeError('DCP probe did not return; preserve its state and inspect the log')
if b'\nM5_DCP_MODPROBE_RETURN=0\r\n' not in captured:
    raise RuntimeError('DCP module probe was refused or returned an error')
print('M5_DCP_PROBE_CAPTURED; inspect adoption, link and connector evidence separately')
