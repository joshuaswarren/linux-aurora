#!/usr/bin/env python3
"""Install a protected WPA test configuration into the M5's RAM filesystem.

Reads the separately protected local SSID/password input. No secrets, command
text, configuration contents or console output are printed or logged.
"""
import base64
import argparse
import hashlib
from pathlib import Path
import secrets
import serial
import stat
import time

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--ssid', help='Explicitly confirmed network-name correction')
parser.add_argument('--disabled', action='store_true',
                    help='Wait for explicit network enable so tracing can start first')
args = parser.parse_args()
credentials = Path('/Users/mcruz/.aurora-n1-psk')
if stat.S_IMODE(credentials.stat().st_mode) & 0o077:
    raise SystemExit('Credential file must be accessible only to its owner')
lines = credentials.read_text().splitlines()
if len(lines) != 2 or not (1 <= len(lines[0].encode()) <= 32) or not (8 <= len(lines[1]) <= 63):
    raise SystemExit('Credential file must contain one SSID line and one WPA passphrase line')
ssid, passphrase = lines
if args.ssid is not None:
    if not 1 <= len(args.ssid.encode()) <= 32:
        raise SystemExit('Invalid network-name length')
    ssid = args.ssid
psk = hashlib.pbkdf2_hmac('sha1', passphrase.encode(), ssid.encode(), 4096, 32)
config = ('ctrl_interface=/run/wpa_supplicant\nupdate_config=0\nap_scan=1\n'
          'network={\n' + ('disabled=1\n' if args.disabled else '') +
          'ssid=' + ssid.encode().hex() + '\npsk=' + psk.hex() + '\n'
          'key_mgmt=WPA-PSK\nproto=RSN\npairwise=CCMP\ngroup=CCMP\nieee80211w=1\n}\n')
encoded = base64.b64encode(config.encode()).decode()
phrase_encoded = base64.b64encode(passphrase.encode()).decode()

with serial.Serial('/dev/cu.kis-01100000-ch-0', 1500000, timeout=0.2) as port:
    def command(text):
        marker = secrets.token_hex(12)
        port.write((text + "; printf '\\nWPA_SETUP_%s\\n' " + marker + '\n').encode())
        deadline = time.monotonic() + 10
        captured = bytearray()
        while time.monotonic() < deadline:
            captured.extend(port.read(65536))
            if ('\nWPA_SETUP_' + marker + '\r\n').encode() in captured:
                return captured
        raise RuntimeError('Protected configuration command timed out')

    port.write(b'\x1b[1;1R\n')
    time.sleep(0.3)
    port.reset_input_buffer()
    command('stty -echo')
    try:
        command('mkdir -p /run/wpa_supplicant; umask 077; '
                "printf '%s' '" + encoded + "' | base64 -d > /run/n1-wpa.conf")
        result = command("printf '%s' '" + phrase_encoded + "' | base64 -d > "
                         '/sys/kernel/debug/apple_centauri/0000:01:00.0/passphrase '
                         '&& echo DRIVER_CREDENTIAL_READY')
        if b'DRIVER_CREDENTIAL_READY' not in result.splitlines():
            raise RuntimeError('Driver credential setup failed')
        result = command('sha256sum /run/n1-wpa.conf')
        if hashlib.sha256(config.encode()).hexdigest().encode() not in result:
            raise RuntimeError('Protected configuration checksum mismatch')
        result = command('test "$(stat -c %a /run/n1-wpa.conf)" = 600 '
                         '&& test -s /run/n1-wpa.conf && echo CONFIGURATION_READY')
        if b'CONFIGURATION_READY' not in result.splitlines():
            raise RuntimeError('Protected configuration verification failed')
    finally:
        command('stty echo')
print('Protected WPA configuration installed in target RAM')
