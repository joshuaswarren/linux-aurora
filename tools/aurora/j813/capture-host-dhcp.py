#!/usr/bin/env python3
"""Capture only DHCP on the host Wi-Fi interface without changing its mode.

Administrator authentication goes through osascript stdin. Packet bytes stay
in an exclusively created mode-0600 file; neither secrets nor frames print.
"""
import argparse
import getpass
import hashlib
import json
import os
from pathlib import Path
import re
import shlex
import subprocess
import time


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('destination', type=Path)
    parser.add_argument('--interface', default='en0')
    parser.add_argument('--seconds', type=int, default=30)
    args = parser.parse_args()
    if not re.fullmatch(r'en[0-9]+', args.interface) or not 5 <= args.seconds <= 45:
        raise ValueError('Unsupported capture options')
    output = args.destination.resolve()
    diagnostic = output.with_suffix(output.suffix + '.capture-log')
    for path in (output, diagnostic):
        os.close(os.open(path, os.O_CREAT | os.O_EXCL | os.O_WRONLY, 0o600))
    command = ('/usr/sbin/tcpdump -i ' + shlex.quote(args.interface) +
               ' -p -n -s 0 -U -w ' + shlex.quote(str(output)) +
               " 'udp port 67 or udp port 68' >/dev/null 2>" +
               shlex.quote(str(diagnostic)) +
               ' & n1_capture_pid=$!; sleep ' + str(args.seconds) +
               '; kill -TERM "$n1_capture_pid" 2>/dev/null; sleep 1; '
               'kill -KILL "$n1_capture_pid" 2>/dev/null; '
               'wait "$n1_capture_pid" 2>/dev/null; exit 0')
    password = getpass.getpass('Runtime administrator credential: ')
    script = ('do shell script ' + json.dumps(command) + ' password ' +
              json.dumps(password) + ' with administrator privileges\n')
    process = subprocess.Popen(['/usr/bin/osascript', '-'], stdin=subprocess.PIPE,
                               stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                               text=True)
    process.stdin.write(script)
    process.stdin.close()
    password = script = None
    deadline, ready = time.monotonic() + args.seconds + 15, False
    while process.poll() is None and time.monotonic() < deadline:
        if not ready and 'listening on' in diagnostic.read_text(errors='replace'):
            ready = True
            print('HOST_DHCP_CAPTURE_READY', flush=True)
        time.sleep(0.1)
    if process.poll() is None:
        raise RuntimeError('Bounded capture did not finish')
    if process.returncode or not ready:
        raise RuntimeError('Host DHCP capture failed')
    data = output.read_bytes()
    print('HOST_DHCP_CAPTURE_SAVED', len(data), hashlib.sha256(data).hexdigest())
    diagnostic.unlink()


if __name__ == '__main__':
    try:
        main()
    except Exception as error:
        raise SystemExit('Host DHCP capture failed: ' + type(error).__name__)
