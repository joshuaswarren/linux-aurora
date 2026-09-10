#!/usr/bin/env python3
"""Restore the verified test bond into a fresh RAM desktop without logging keys.

Input captures remain private. The default validates inputs only; --install
requires an exclusive M5 Linux console and a tmpfs mounted at /desktop.
BlueZ 5.85 adapter.c stores LinkKey bytes in their original byte order.
"""
import argparse
import base64
import hashlib
from pathlib import Path
import re
import secrets
import shlex
import struct
import time


def read_identity(adapter_info, capture):
    addresses = re.findall(r'\baddr\s+((?:[0-9A-Fa-f]{2}:){5}[0-9A-Fa-f]{2})',
                           adapter_info.read_text())
    if len(addresses) != 1:
        raise ValueError('Expected one adapter')
    data = capture.read_bytes()
    if data[:16] != b'btsnoop\0' + struct.pack('>II', 1, 2001):
        raise ValueError('Expected Linux monitor capture')
    offset = 16
    keys = []
    pairing_ok = authentication_ok = encrypted = False
    while offset < len(data):
        if len(data) - offset < 24:
            raise ValueError('Short record')
        original, size, flags, _, _ = struct.unpack_from('>IIIIQ', data, offset)
        offset += 24
        if original != size or size > len(data) - offset:
            raise ValueError('Short payload')
        packet = data[offset:offset + size]
        offset += size
        if flags & 0xffff != 3:
            continue
        if len(packet) < 2 or packet[1] + 2 != len(packet):
            raise ValueError('Invalid event')
        if packet[0] == 0x18 and len(packet) == 25:
            keys.append((packet[2:8], packet[8:24], packet[24]))
        elif packet[0] == 0x36 and len(packet) == 9 and packet[2] == 0:
            pairing_ok = packet[3:9]
        elif packet[0] == 0x06 and len(packet) == 5 and packet[2] == 0:
            authentication_ok = True
        elif packet[0] == 0x08 and len(packet) == 6 and packet[2] == 0 and packet[5] == 2:
            encrypted = True
    if len(keys) != 1 or pairing_ok != keys[0][0] or not authentication_ok or not encrypted:
        raise ValueError('Expected one successful encrypted pairing')
    peer, key, key_type = keys[0]
    if key_type != 7:
        raise ValueError('Unexpected key type for this qualified capture')
    return addresses[0].upper(), ':'.join(f'{b:02X}' for b in peer[::-1]), key, key_type


def install_files(files):
    import serial
    token = secrets.token_hex(12)
    marker = 'BOND_ACK_' + token
    with serial.Serial('/dev/cu.kis-01100000-ch-0', 1500000, timeout=0.2) as port:
        def command(text):
            port.write((text + '; bond_rc=$?; printf "\\n' + marker +
                        ' rc=%s\\n" "$bond_rc"\n').encode())
            captured = bytearray()
            deadline = time.monotonic() + 12
            while time.monotonic() < deadline:
                captured.extend(port.read(65536))
                if len(captured) > 1024 * 1024:
                    raise RuntimeError('Unexpected console output')
                match = re.search(rb'\n' + marker.encode() + rb' rc=(\d+)\r?\n', captured)
                if match:
                    if int(match[1]):
                        raise RuntimeError('Target operation failed')
                    return bytes(captured)
            raise TimeoutError('Console acknowledgement')

        port.write(b'\x1b[1;1R\n')
        time.sleep(0.3)
        port.reset_input_buffer()
        command('stty -echo')
        try:
            command("grep -q '^tmpfs /desktop tmpfs ' /proc/mounts && "
                    "tr '\\000' '\\n' </proc/device-tree/compatible | grep -qx 'apple,j813'")
            command('test -z "$(pidof bluetoothd)"')
            for path, content in files:
                digest = hashlib.sha256(content).hexdigest()
                quoted = shlex.quote(path)
                temporary = shlex.quote(path + '.' + token)
                encoded = base64.b64encode(content).decode()
                command('umask 077; mkdir -p ' + shlex.quote(str(Path(path).parent)) +
                        '; printf %s ' + shlex.quote(encoded) + ' | base64 -d >' + temporary)
                result = command('sha256sum ' + temporary)
                if digest.encode() not in result:
                    raise ValueError('Transfer checksum mismatch')
                if b'EXISTS' in command('if test -e ' + quoted + '; then echo EXISTS; fi'):
                    if digest.encode() not in command('sha256sum ' + quoted):
                        command('rm -f ' + temporary)
                        raise ValueError('Preserving a different existing file')
                    command('rm -f ' + temporary)
                else:
                    command('mv ' + temporary + ' ' + quoted + '; chmod 600 ' + quoted)
        finally:
            command('stty echo')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('adapter_info', type=Path)
    parser.add_argument('pairing_capture', type=Path)
    parser.add_argument('--install', action='store_true')
    args = parser.parse_args()
    adapter, peer, key, key_type = read_identity(args.adapter_info, args.pairing_capture)
    if args.install:
        info = ('[General]\nSupportedTechnologies=BR/EDR;\nTrusted=true\nBlocked=false\n\n'
                '[LinkKey]\nKey=' + key.hex().upper() + '\nType=' + str(key_type) + '\nPINLength=0\n')
        options = ('options btcentauri arm_auxiliary_rings=1 native_startup=1 '
                   'enable_acl=1 native_hrb=1 native_bdaddr=' + adapter + '\n')
        install_files([
            (f'/desktop/var/lib/bluetooth/{adapter}/{peer}/info', info.encode()),
            ('/desktop/etc/modprobe.d/j813-bt-identity.conf', options.encode()),
        ])
    print('N1_VERIFIED_BOND_' + ('RESTORED_TO_RAM' if args.install else 'INPUT_VALIDATED'))


if __name__ == '__main__':
    try:
        main()
    except Exception as error:
        raise SystemExit('Private bond operation failed: ' + type(error).__name__)
