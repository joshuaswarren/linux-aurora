#!/usr/bin/env python3
"""Copy a file into the running Linux RAM /tmp over the exclusive KIS shell."""
import argparse
import base64
import hashlib
from pathlib import Path
import re
import shlex
import time
import uuid
import serial

parser = argparse.ArgumentParser()
parser.add_argument('source', type=Path)
parser.add_argument('destination')
parser.add_argument('--device', default='/dev/cu.kis-00100000-ch-0')
args = parser.parse_args()
destination = Path(args.destination)
if destination.parent != Path('/tmp') or not re.fullmatch(r'[A-Za-z0-9_.-]+', destination.name):
    raise SystemExit('Destination must be a simple filename directly in RAM /tmp')
payload = args.source.read_bytes()
if len(payload) > 32 * 1024 * 1024:
    raise SystemExit('File exceeds the bounded 32 MiB shell transfer limit')
digest = hashlib.sha256(payload).hexdigest()
token = uuid.uuid4().hex
ready, end, delimiter = ('NEO_' + prefix + token for prefix in ('READY_', 'END_', 'EOF_'))

with serial.Serial(args.device, 115200, timeout=0.2, write_timeout=10) as port:
    def receive(pattern, seconds):
        data = bytearray()
        deadline = time.monotonic() + seconds
        while time.monotonic() < deadline:
            data.extend(port.read(65536))
            match = re.search(pattern, bytes(data).replace(b'\r', b''))
            if match:
                return bytes(data), match
        raise RuntimeError('Timed out waiting for transfer protocol marker')

    try:
        port.write(('\nstty -echo; PS2=; printf "\\n' + ready + '\\n"\n').encode())
        receive(b'\n' + ready.encode() + b'\n', 15)
        quoted = shlex.quote(str(destination))
        port.write(('base64 -d > ' + quoted + " <<'" + delimiter + "'\n").encode())
        encoded = base64.encodebytes(payload)
        for offset in range(0, len(encoded), 1024):
            port.write(encoded[offset:offset + 1024])
            # Drain any queued terminal output while the shell accepts input.
            if port.in_waiting:
                port.read(port.in_waiting)
        port.write((delimiter + '\nsha256sum ' + quoted + '; stty echo; printf "\\n' + end + '\\n"\n').encode())
        data, _ = receive(b'\n' + end.encode() + b'\n', 60)
        # With terminal echo disabled, ash's prompt can precede the checksum
        # on the same line. The digest is never sent in the shell command.
        if not re.search(digest.encode() + rb'\s+' + re.escape(str(destination).encode()) + rb'\r?\n', data):
            raise RuntimeError('Target SHA-256 did not match')
        print(f'LINUX_RAM_TRANSFER_VERIFIED bytes={len(payload)} sha256={digest} destination={destination}')
    except BaseException:
        port.write(b'\x03\nstty echo\n')
        raise
