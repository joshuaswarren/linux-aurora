#!/usr/bin/env python3
"""Send bounded receive probes or observe M5 ARP on the host's existing en0 link.

No interface, route, address, or channel configuration is changed. The first
three Ethernet destinations are broadcast; the next three are the M5 station.
Both use the IP limited broadcast destination, requiring no M5 address lease.
Private addresses are read locally and never printed. Elevation credentials
travel only through osascript stdin, never argv or a persistent file.
"""
import argparse
import ctypes
import getpass
import ipaddress
import json
import os
from pathlib import Path
import re
import shlex
import struct
import subprocess
import sys
import time

PRIVATE = (Path(__file__).resolve().parent.parent / 'AuroraSilicon/build/'
           'j813-centauri/re/firmware').resolve()
MAGIC = b'N1_RX_PROBE36E_'


def checksum(data):
    if len(data) & 1:
        data += b'\0'
    total = sum(struct.unpack('!' + 'H' * (len(data) // 2), data))
    while total >> 16:
        total = (total & 0xffff) + (total >> 16)
    return (~total) & 0xffff


def packet(source_mac, target_mac, source_ip, sequence):
    payload = MAGIC + bytes([sequence]) + b' bounded receive test'
    destination_ip = b'\xff' * 4
    udp = struct.pack('!HHHH', 39999, 40000, 8 + len(payload), 0) + payload
    pseudo = source_ip + destination_ip + struct.pack('!BBH', 0, 17, len(udp))
    udp = udp[:6] + struct.pack('!H', checksum(pseudo + udp) or 0xffff) + udp[8:]
    ip = struct.pack('!BBHHHBBH4s4s', 0x45, 0, 20 + len(udp),
                     0x4e30 + sequence, 0, 64, 17, 0, source_ip, destination_ip)
    ip = ip[:10] + struct.pack('!H', checksum(ip)) + ip[12:]
    destination = b'\xff' * 6 if sequence < 3 else target_mac
    return destination + source_mac + b'\x08\x00' + ip + udp


def station_address(stats_path):
    text = stats_path.read_text()
    if not text.startswith('rc=0 valid=1 size=780 tag=0xf928 length=752\n'):
        raise ValueError('Unexpected station statistics ABI')
    data = bytearray(752)
    for address, value in re.findall(r'^([0-9a-f]{3}) (\d+)$', text, re.M):
        offset = int(address, 16)
        if offset % 4 or offset + 4 > len(data):
            raise ValueError('Invalid statistics offset')
        struct.pack_into('<I', data, offset, int(value))
    mld, bitmap = struct.unpack_from('<II', data)
    if mld != 0 or bitmap not in (1, 2, 4):
        raise ValueError('Expected a single non-MLD link')
    base = 8 + 24 * (bitmap.bit_length() - 1)
    return bytes(data[base + 11:base + 17])


def inject(path, capture, mode, stats_path):
    if os.geteuid() != 0:
        raise RuntimeError('Elevation required')
    target = bytes.fromhex(path.read_text().strip().replace(':', ''))
    netdev_target = target
    if stats_path:
        target = station_address(stats_path)
    if len(target) != 6 or target[0] & 1 or not any(target):
        raise ValueError('Invalid target')
    config = subprocess.check_output(['/sbin/ifconfig', 'en0'], text=True)
    match = re.search(r'\bether\s+([0-9a-f:]{17})', config)
    if not match or 'status: active' not in config:
        raise RuntimeError('Host link unavailable')
    source = bytes.fromhex(match[1].replace(':', ''))
    if source == target:
        raise ValueError('Target equals host')
    source_ip = ipaddress.IPv4Address(subprocess.check_output(
        ['/usr/sbin/ipconfig', 'getifaddr', 'en0'], text=True).strip()).packed
    lib = ctypes.CDLL('/usr/lib/libpcap.A.dylib')
    lib.pcap_open_live.argtypes = [ctypes.c_char_p, ctypes.c_int,
                                  ctypes.c_int, ctypes.c_int, ctypes.c_char_p]
    lib.pcap_open_live.restype = ctypes.c_void_p
    lib.pcap_inject.argtypes = [ctypes.c_void_p, ctypes.c_void_p, ctypes.c_size_t]
    lib.pcap_inject.restype = ctypes.c_int
    lib.pcap_datalink.argtypes = [ctypes.c_void_p]
    lib.pcap_datalink.restype = ctypes.c_int
    lib.pcap_close.argtypes = [ctypes.c_void_p]
    lib.pcap_setnonblock.argtypes = [ctypes.c_void_p, ctypes.c_int, ctypes.c_char_p]
    lib.pcap_setnonblock.restype = ctypes.c_int

    class Header(ctypes.Structure):
        _fields_ = [('seconds', ctypes.c_long), ('micros', ctypes.c_long),
                    ('captured', ctypes.c_uint), ('original', ctypes.c_uint)]

    lib.pcap_next_ex.argtypes = [ctypes.c_void_p,
                                 ctypes.POINTER(ctypes.POINTER(Header)),
                                 ctypes.POINTER(ctypes.POINTER(ctypes.c_ubyte))]
    lib.pcap_next_ex.restype = ctypes.c_int
    error = ctypes.create_string_buffer(256)
    observer = lib.pcap_open_live(b'en0', 65535, 0, 100, error)
    if not observer:
        raise RuntimeError('Observation interface unavailable')
    handle = lib.pcap_open_live(b'en0', 65535, 0, 100, error)
    if not handle:
        lib.pcap_close(observer)
        raise RuntimeError('Capture interface unavailable')
    sent = [0, 0]
    records = []
    arp_requests = arp_replies = 0

    def observe():
        nonlocal arp_requests, arp_replies
        header = ctypes.POINTER(Header)()
        data = ctypes.POINTER(ctypes.c_ubyte)()
        for _ in range(512):
            rc = lib.pcap_next_ex(observer, ctypes.byref(header), ctypes.byref(data))
            if rc == 0:
                return
            if rc < 0:
                raise RuntimeError('Capture read failed')
            h = header.contents
            frame = ctypes.string_at(data, h.captured)
            keep = len(frame) >= 42 and frame[42:42 + len(MAGIC)] == MAGIC
            if mode == 'observe-arp':
                keep = (len(frame) >= 42 and frame[12:14] == b'\x08\x06' and
                        frame[14:20] == b'\x00\x01\x08\x00\x06\x04' and
                        (frame[22:28] in (target, netdev_target) or
                         frame[32:38] in (target, netdev_target)))
                if keep:
                    arp_requests += frame[20:22] == b'\x00\x01'
                    arp_replies += frame[20:22] == b'\x00\x02'
            if keep:
                records.append(struct.pack('<IIII', h.seconds, h.micros,
                                           h.captured, h.original) + frame)

    try:
        if lib.pcap_datalink(handle) != 1 or lib.pcap_datalink(observer) != 1:
            raise RuntimeError('Unexpected data link')
        if lib.pcap_setnonblock(observer, 1, error) != 0:
            raise RuntimeError('Nonblocking capture unavailable')
        sequences = (range(0) if mode == 'observe-arp' else range(3) if mode == 'broadcast'
                     else range(3, 6) if mode == 'direct' else range(6))
        if mode == 'observe-arp':
            ready = capture.with_suffix(capture.suffix + '.ready')
            owner = path.stat()
            fd = os.open(ready, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
            try:
                os.fchown(fd, owner.st_uid, owner.st_gid)
                os.write(fd, b'N1_HOST_ARP_OBSERVER_READY\n')
            finally:
                os.close(fd)
        for sequence in sequences:
            frame = packet(source, target, source_ip, sequence)
            buffer = ctypes.create_string_buffer(frame)
            if lib.pcap_inject(handle, buffer, len(frame)) != len(frame):
                raise RuntimeError('Incomplete injection')
            sent[sequence // 3] += 1
            time.sleep(0.4)
            observe()
        until = time.monotonic() + (20 if mode == 'observe-arp' else 1.5)
        while time.monotonic() < until:
            observe()
            time.sleep(0.1)
    finally:
        lib.pcap_close(handle)
        lib.pcap_close(observer)
    owner = path.stat()
    with os.fdopen(os.open(capture, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600), 'wb') as out:
        os.fchown(out.fileno(), owner.st_uid, owner.st_gid)
        out.write(struct.pack('<IHHIIII', 0xa1b2c3d4, 2, 4, 0, 0, 65535, 1))
        out.write(b''.join(records))
    if mode == 'observe-arp':
        print(f'N1_HOST_ARP_OBSERVE requests={arp_requests} replies={arp_replies}')
    else:
        print(f'N1_HOST_RX_PROBE broadcast_injected={sent[0]} direct_injected={sent[1]}')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('target_mac_file', type=Path)
    parser.add_argument('capture_file', type=Path)
    parser.add_argument('--mode', choices=['both', 'broadcast', 'direct', 'observe-arp'], default='both')
    parser.add_argument('--firmware-address-from', type=Path)
    parser.add_argument('--elevated', action='store_true', help=argparse.SUPPRESS)
    args = parser.parse_args()
    path = args.target_mac_file.resolve(strict=True)
    if path.parent != PRIVATE or path.stat().st_mode & 0o077:
        raise ValueError('Expected protected private artifact')
    capture = args.capture_file.resolve()
    if capture.parent != PRIVATE or capture.exists():
        raise ValueError('Expected unused private capture path')
    if args.mode == 'observe-arp' and capture.with_suffix(capture.suffix + '.ready').exists():
        raise ValueError('Expected unused observer readiness path')
    stats_path = args.firmware_address_from
    if stats_path:
        stats_path = stats_path.resolve(strict=True)
        if stats_path.parent != PRIVATE or stats_path.stat().st_mode & 0o077:
            raise ValueError('Expected protected statistics artifact')
    if args.elevated:
        inject(path, capture, args.mode, stats_path)
        return 0
    command_args = [sys.executable, str(Path(__file__).resolve()),
                    str(path), str(capture), '--mode', args.mode, '--elevated']
    if stats_path:
        command_args += ['--firmware-address-from', str(stats_path)]
    command = shlex.join(command_args)
    password = getpass.getpass('Runtime administrator credential: ')
    script = ('do shell script ' + json.dumps(command) + ' password ' +
              json.dumps(password) + ' with administrator privileges\n')
    try:
        result = subprocess.run(['/usr/bin/osascript', '-'], input=script,
                                text=True, capture_output=True, timeout=35)
    finally:
        password = script = None
    if result.returncode:
        print(f'N1_HOST_RX_PROBE failed exit={result.returncode}')
        return 1
    for line in result.stdout.splitlines():
        if re.fullmatch(r'N1_HOST_ARP_OBSERVE requests=[0-9]+ replies=[0-9]+', line):
            print(line)
            return 0
        if re.fullmatch(r'N1_HOST_RX_PROBE broadcast_injected=[03] direct_injected=[03]', line):
            print(line)
            return 0
    print('N1_HOST_RX_PROBE unexpected result')
    return 1


if __name__ == '__main__':
    try:
        sys.exit(main())
    except Exception as exc:
        print('N1_HOST_RX_PROBE failed class=' + type(exc).__name__)
        sys.exit(1)
