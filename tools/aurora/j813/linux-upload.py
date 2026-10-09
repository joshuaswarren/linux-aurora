#!/usr/bin/env python3
"""Copy a public test artifact into the M5 RAM filesystem over the Linux shell.

Uses per-chunk acknowledgements and verifies SHA-256. This is not a credential
transport: do not pass files containing secrets.
"""
import argparse
import base64
import hashlib
from pathlib import Path
import secrets
import serial
import shlex
import time

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('source', type=Path)
parser.add_argument('destination')
args = parser.parse_args()
data = args.source.read_bytes()
digest = hashlib.sha256(data).hexdigest()
destination = shlex.quote(args.destination)
token = secrets.token_hex(8)

with serial.Serial('/dev/cu.kis-01100000-ch-0', 1500000, timeout=0.2) as port:
    def command(text, timeout=10):
        marker = 'COPY_' + token
        port.write((text + "; printf '\\nCOPY_%s\\n' " + token + '\n').encode())
        output = bytearray()
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            output.extend(port.read(65536))
            if ('\n' + marker + '\r\n').encode() in output:
                return bytes(output)
        raise RuntimeError('Upload command acknowledgement timed out')

    port.write(b'\x1b[1;1R\n')
    time.sleep(0.3)
    port.reset_input_buffer()
    command('stty -echo')
    try:
        command('umask 077; : > ' + destination)
        for offset in range(0, len(data), 768):
            chunk = base64.b64encode(data[offset:offset+768]).decode()
            command("printf '%s' " + shlex.quote(chunk) + ' | base64 -d >> ' + destination)
        result = command('sha256sum ' + destination)
        if digest.encode() not in result:
            raise RuntimeError('Uploaded artifact SHA-256 mismatch')
    finally:
        command('stty echo')
print('LINUX_RAM_UPLOAD_VERIFIED', args.destination, len(data), digest)
