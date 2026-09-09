#!/usr/bin/env python3
"""Capture an M5 RAM file privately and verify SHA-256 over serial.

Payloads never reach stdout or a console log. The target snapshot is mode 0600,
the local artifact is created exclusively with mode 0600, and corrupt or
interleaved serial transfers are rejected. Only the M5 RID1 endpoint is used.
"""
import argparse
import base64
import gzip
import hashlib
import os
from pathlib import Path
import re
import secrets
import serial
import shlex
import time


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('source')
    parser.add_argument('destination', type=Path)
    parser.add_argument('--timeout', type=float, default=120)
    args = parser.parse_args()
    if args.destination.exists():
        raise RuntimeError('Destination already exists')
    token = secrets.token_hex(12)
    temporary = '/run/n1-download-' + token
    begin = ('DOWNLOAD_BEGIN_' + token).encode()
    end = ('DOWNLOAD_END_' + token).encode()
    ack = ('DOWNLOAD_ACK_' + token).encode()
    with serial.Serial('/dev/cu.kis-01100000-ch-0', 1500000,
                       timeout=0.2) as port:
        def command(text, timeout=10):
            trailer = "; transfer_rc=$?; printf '\\nDOWNLOAD_ACK_%s rc=%s\\n' "
            port.write((text + trailer + token + ' "$transfer_rc"\n').encode())
            captured = bytearray()
            deadline = time.monotonic() + timeout
            pattern = rb'\n' + ack + rb' rc=([0-9]+)\r?\n'
            while time.monotonic() < deadline:
                captured.extend(port.read(65536))
                if len(captured) > 32 * 1024 * 1024:
                    raise RuntimeError('Private transfer exceeded size limit')
                match = re.search(pattern, captured)
                if match:
                    if int(match.group(1)):
                        raise RuntimeError('Target transfer command failed')
                    return bytes(captured[:match.start()]).replace(b'\r\n', b'\n')
            raise RuntimeError('Private transfer acknowledgement timed out')

        port.write(b'\x1b[1;1R\n')
        time.sleep(0.3)
        port.reset_input_buffer()
        command('stty -echo')
        try:
            source = shlex.quote(args.source)
            snapshot = shlex.quote(temporary)
            text = ("(umask 077; cat " + source + ' > ' + snapshot +
                    ' && sha256sum ' + snapshot +
                    " && printf '\\nDOWNLOAD_BEGIN_%s\\n' " + token +
                    ' && gzip -c ' + snapshot + ' | base64; transfer_rc=$?; ' +
                    "printf '\\nDOWNLOAD_END_%s\\n' " + token +
                    '; rm -f ' + snapshot + '; exit "$transfer_rc")')
            captured = command(text, args.timeout)
        finally:
            # Any interrupted transfer belongs to this helper; do not leave
            # its producer or a shell with echo disabled behind.
            port.write(b'\x03')
            try:
                command('stty echo')
            except RuntimeError:
                pass
    prefix, payload = captured.split(b'\n' + begin + b'\n', 1)
    encoded, _ = payload.split(b'\n' + end + b'\n', 1)
    checksums = re.findall(rb'(?m)^([0-9a-f]{64})\s', prefix)
    if len(checksums) != 1:
        raise RuntimeError('Target snapshot checksum missing or ambiguous')
    compressed = base64.b64decode(b''.join(encoded.split()), validate=True)
    data = gzip.decompress(compressed)
    if len(data) > 16 * 1024 * 1024:
        raise RuntimeError('Private artifact exceeded size limit')
    digest = hashlib.sha256(data).hexdigest()
    if digest.encode() != checksums[0]:
        raise RuntimeError('Private artifact SHA-256 mismatch')
    fd = os.open(args.destination, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    with os.fdopen(fd, 'wb') as output:
        output.write(data)
    print('LINUX_PRIVATE_DOWNLOAD_VERIFIED', args.destination, len(data), digest)


if __name__ == '__main__':
    try:
        main()
    except Exception as error:
        # Parsing and transport exceptions can carry private payload fragments.
        # Report the class only; never format an exception value or traceback.
        raise SystemExit('Private download failed: ' + type(error).__name__)
