#!/usr/bin/env python3
"""Verify the already-booted native Linux shell over the dedicated M5 port."""
import secrets
import serial
import sys
import time

nonce = secrets.token_hex(6)
begin = 'J813_VERIFY_' + nonce + '_BEGIN'
end = 'J813_VERIFY_' + nonce + '_END'
with serial.Serial('/dev/cu.kis-01100000-ch-0', 1500000, timeout=0.2) as port:
    port.write(b'\x1b[1;1R\n')
    time.sleep(0.5)
    port.reset_input_buffer()
    command = (f'echo {begin}; uname -a; cat /proc/device-tree/model; echo; '
               'cat /sys/devices/system/cpu/online; cat /proc/uptime; sleep 2; '
               'cat /proc/uptime; head -5 /proc/meminfo; cat /proc/mounts; '
               f'echo {end}\n')
    port.write(command.encode())
    deadline = time.monotonic() + 30
    captured = bytearray()
    while time.monotonic() < deadline:
        chunk = port.read(65536)
        if chunk:
            captured.extend(chunk)
            sys.stdout.buffer.write(chunk)
            sys.stdout.flush()
        if ('\n' + end + '\r\n').encode() in captured:
            print('\nJ813_NATIVE_SHELL_AND_TIMER_VERIFIED')
            sys.exit(0)
raise SystemExit('Native shell verification timed out')
