#!/usr/bin/env python3
"""Run a bounded command in the RAM-only M5 Linux serial shell.

Commands and output are visible in the console log. Never pass credentials.
"""
import argparse
import re
import secrets
import serial
import sys
import time

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('command')
parser.add_argument('--timeout', type=float, default=30)
args = parser.parse_args()
if len(args.command.encode()) > 900:
    parser.error('Command exceeds the console line budget; upload a script instead')
token = secrets.token_hex(8)
end = 'J813_COMMAND_' + token
with serial.Serial('/dev/cu.kis-01100000-ch-0', 1500000, timeout=0.2) as port:
    port.write(b'\x1b[1;1R\n')
    time.sleep(0.3)
    port.reset_input_buffer()
    # Isolate exit/set -e from the PID-1 test shell. Split the marker so the
    # echoed command cannot count as completion.
    port.write(('( ' + args.command + "\n); task_rc=$?; printf '\\nJ813_COMMAND_%s rc=%s\\n' "
                + token + ' "$task_rc"\n').encode())
    deadline = time.monotonic() + args.timeout
    captured = bytearray()
    while time.monotonic() < deadline:
        data = port.read(65536)
        captured.extend(data)
        sys.stdout.buffer.write(data)
        sys.stdout.flush()
        match = re.search(rb'\n' + end.encode() + rb' rc=([0-9]+)\r?\n', captured)
        if match:
            sys.exit(int(match.group(1)))
raise SystemExit('Linux command timed out; inspect console before retrying')
