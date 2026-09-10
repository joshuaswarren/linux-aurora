#!/usr/bin/env python3
"""One bounded OBS client, using the qualified M5 AVE single-frame sequence.

Runs as root in the RAM desktop. OBS only receives a group-owned Unix socket;
it never gains debugfs access. Every frame is a fresh all-IDR hardware session.
Any failed firmware transaction terminates the broker without retrying it.
"""
import argparse
import fcntl
import hashlib
import json
import os
from pathlib import Path
import re
import socket
import struct
import time

DEBUG = Path('/sys/kernel/debug/281100000.video-codec')
FRAME_BYTES = 115200
SURFACE_BYTES = 0x528000
INPUT_OFFSET = 0x600200
SOCKET = Path('/run/j813-ave-obs.sock')

def status():
    return dict(line.split('=', 1) for line in (DEBUG / 'ave-status').read_text().splitlines() if '=' in line)

def reply():
    data = (DEBUG / 'ave-command').read_bytes()
    channel, size, flags, reserved, address = struct.unpack_from('<IIIIQ', data)
    assert channel == 2 and size in (72, 80) and reserved == 0 and len(data) == 24 + size
    return data[24:]

def simple_command(opcode, client, request):
    packet = bytearray(72)
    struct.pack_into('<I', packet, 0, opcode)
    struct.pack_into('<QQ', packet, 8, request, client)
    struct.pack_into('<IIIIII', packet, 0x18, 1, 1, opcode,
                     0 if opcode == 9 else 0xc8, 0x36, 0x1e0 if opcode == 3 else 0x140)
    return packet

def recv_exact(connection, size):
    data = bytearray()
    while len(data) < size:
        part = connection.recv(size - len(data))
        if not part:
            raise EOFError('OBS disconnected')
        data.extend(part)
    return bytes(data)

def extract(stream, metadata):
    size = struct.unpack_from('<I', metadata, 0x120)[0]
    assert size == struct.unpack_from('<I', metadata, 0x12c)[0]
    assert 5 <= size <= len(stream)
    encoded = stream[:size]
    assert encoded[:4] == b'\0\0\0\1' and encoded[4] & 31 == 5
    starts = [m.start() for m in re.finditer(b'\x00\x00\x00\x01', metadata)]
    headers = {}
    for index, start in enumerate(starts):
        if start + 6 > len(metadata) or metadata[start + 4] & 0x80:
            continue
        kind = metadata[start + 4] & 31
        if kind not in (7, 8):
            continue
        end = starts[index + 1] if index + 1 < len(starts) else len(metadata)
        assert kind not in headers
        headers[kind] = metadata[start:end].rstrip(b'\0')
    assert set(headers) == {7, 8}
    return headers[7] + headers[8], encoded

class Encoder:
    def __init__(self, payload, output):
        self.output = output
        self.manifest = json.loads((payload / 'manifest.json').read_text())
        self.templates = {}
        for name, length in [('start', 0x3180), ('process', 0x63d8)]:
            b = (payload / (name + '.bin')).read_bytes()
            assert len(b) == length
            assert hashlib.sha256(b).hexdigest() == self.manifest['sha256'][name]
            self.templates[name] = b
        s = status()
        assert s['ipc_ready'] == '1' and s['command_failed'] == '0' and s['powered_domains'] == '5'
        assert int(s['arena_dma'], 0) == self.manifest['arena_dma']
        assert int(s['arena_bytes']) == self.manifest['arena_bytes']
        assert Path('/proc/sys/kernel/random/boot_id').read_text().strip() == self.manifest['boot_id']
        previous = reply()
        assert struct.unpack_from('<I', previous)[0] in (1, 4), 'Firmware is not idle'
        self.client = struct.unpack_from('<Q', previous, 0x10)[0] + 1
        self.initial_commands = int(s['commands'])
        self.command_count = 0
        self.frames = []
        self.arena = os.open(DEBUG / 'ave-arena', os.O_RDWR)

    def command(self, packet, opcode):
        fd = os.open(DEBUG / 'ave-command', os.O_WRONLY)
        try:
            assert os.write(fd, packet) == len(packet), 'Short command publication'
        finally:
            os.close(fd)
        actual = reply()
        words = struct.unpack('<' + 'I' * (len(actual) // 4), actual)
        expected = [0] * len(words)
        expected[0], expected[4], expected[8] = opcode, self.client, 0x15 if opcode == 8 else opcode
        assert list(words) == expected, ('Unexpected reply', opcode, words)
        self.command_count += 1

    def frame(self, nv12, index, pts):
        assert len(nv12) == FRAME_BYTES and self.client <= 0xffff
        started = time.monotonic_ns()
        assert os.pwrite(self.arena, bytes(SURFACE_BYTES), 0) == SURFACE_BYTES
        self.command(simple_command(3, self.client, 1), 3)
        start = bytearray(self.templates['start'])
        struct.pack_into('<QQ', start, 8, 2, self.client)
        self.command(start, 6)
        assert os.pwrite(self.arena, nv12, INPUT_OFFSET) == FRAME_BYTES
        assert os.pread(self.arena, FRAME_BYTES, INPUT_OFFSET) == nv12
        process = bytearray(self.templates['process'])
        struct.pack_into('<QQ', process, 8, 3, self.client)
        self.command(process, 8)
        self.command(simple_command(9, self.client, 4), 9)
        stream = os.pread(self.arena, 0x3c000, 0xb4000)
        metadata = os.pread(self.arena, 0xc000, 0x1f4000)
        self.command(simple_command(7, self.client, 5), 7)
        self.command(simple_command(4, self.client, 6), 4)
        headers, encoded = extract(stream, metadata)
        s = status()
        assert s['ipc_ready'] == '1' and s['command_failed'] == '0'
        assert int(s['commands']) == self.initial_commands + self.command_count, 'Concurrent AVE writer'
        name = 'warmup' if index < 0 else f'frame-{index:04d}'
        (self.output / (name + '.nv12')).write_bytes(nv12)
        (self.output / (name + '.h264')).write_bytes(headers + encoded)
        entry = dict(index=index, pts=pts, client=self.client, bytes=len(encoded),
                     input_sha256=hashlib.sha256(nv12).hexdigest(),
                     output_sha256=hashlib.sha256(headers + encoded).hexdigest(),
                     input_readback=True, session_closed=True,
                     elapsed_ms=(time.monotonic_ns() - started) / 1e6)
        self.frames.append(entry)
        with (self.output / 'frames.jsonl').open('a') as log:
            log.write(json.dumps(entry) + '\n')
        print('AVE_OBS_HARDWARE_FRAME', json.dumps(entry), flush=True)
        self.client += 1
        return headers, encoded

def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('payload', type=Path)
    p.add_argument('output', type=Path)
    p.add_argument('--max-frames', type=int, default=24)
    a = p.parse_args()
    assert os.geteuid() == 0 and 1 <= a.max_frames <= 120
    assert '7.1.6-aurora-j813-desktop-v7' == os.uname().release
    os.umask(0o077)
    lock = open('/run/j813-ave-obs.lock', 'w')
    fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
    assert not SOCKET.exists(), 'Refuse to replace another broker socket'
    a.output.mkdir(parents=True, exist_ok=False)
    encoder = Encoder(a.payload, a.output)
    server = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    server.bind(str(SOCKET))
    os.chown(SOCKET, 0, 1000)
    os.chmod(SOCKET, 0o660)
    server.listen(1)
    server.settimeout(600)
    print('AVE_OBS_BROKER_READY', flush=True)
    passed = False
    try:
        connection, _ = server.accept()
        with connection:
            connection.settimeout(30)
            pid, uid, gid = struct.unpack('3i', connection.getsockopt(socket.SOL_SOCKET, socket.SO_PEERCRED, 12))
            assert uid == 1000, 'Only the desktop test user may encode'
            magic, version, width, height, fps = struct.unpack('<4sIIII', recv_exact(connection, 20))
            assert (magic, version, width, height, fps) == (b'AVEM', 1, 320, 240, 1)
            headers, _ = encoder.frame(bytes([16]) * 76800 + bytes([128]) * 38400, -1, -1)
            connection.sendall(struct.pack('<4sIII', b'AVEM', 1, len(headers), a.max_frames) + headers)
            last_pts = None
            for index in range(a.max_frames):
                try:
                    length, pts = struct.unpack('<Iq', recv_exact(connection, 12))
                except EOFError:
                    passed = index > 0
                    break
                assert length == FRAME_BYTES and (last_pts is None or pts > last_pts)
                actual_headers, encoded = encoder.frame(recv_exact(connection, length), index, pts)
                assert actual_headers == headers, 'Fixed-profile parameter sets changed'
                connection.sendall(struct.pack('<II', 0, len(encoded)) + encoded)
                last_pts = pts
            else:
                raise RuntimeError('Frame limit reached before OBS closed; no automatic retry')
    finally:
        server.close()
        SOCKET.unlink(missing_ok=True)
        os.close(encoder.arena)
        result = dict(passed=passed, obs_frames=len([x for x in encoder.frames if x['index'] >= 0]),
                      firmware_commands=encoder.command_count, preflight_frames=1,
                      all_idr=True, profile='320x240 NV12 1fps', final_status=status())
        (a.output / 'broker-result.json').write_text(json.dumps(result, indent=2) + '\n')
        print('AVE_OBS_BROKER_RESULT', json.dumps({k:v for k,v in result.items() if k!='final_status'}), flush=True)
    assert passed

if __name__ == '__main__':
    main()
