#!/usr/bin/env python3
"""Count Linux input event types without retaining key codes or coordinates.

Opening evdev does not grab the device, so XFCE receives the same events.
Run with the desktop root's Python; the initramfs itself needs no Python.
"""
import argparse
import collections
import json
import os
import select
import struct
import time

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('devices', nargs='+')
parser.add_argument('--seconds', type=int, default=60, choices=range(1, 121))
args = parser.parse_args()
event = struct.Struct('@llHHI')
labels = {0: 'sync', 1: 'key', 2: 'relative', 3: 'absolute', 4: 'misc'}
counts = {path: collections.Counter() for path in args.devices}
fds = {}
try:
    for path in args.devices:
        if not path.startswith('/dev/input/event') or not path[16:].isdigit():
            parser.error('Expected an explicit /dev/input/eventN path')
        fds[os.open(path, os.O_RDONLY | os.O_NONBLOCK)] = path
    end = time.monotonic() + args.seconds
    while time.monotonic() < end:
        readable, _, _ = select.select(fds, [], [], max(0, end - time.monotonic()))
        for fd in readable:
            data = os.read(fd, event.size * 128)
            if not data or len(data) % event.size:
                raise RuntimeError('Input stream ended or returned a partial event')
            for offset in range(0, len(data), event.size):
                kind = event.unpack_from(data, offset)[2]
                counts[fds[fd]][labels.get(kind, 'other')] += 1
                counts[fds[fd]]['total'] += 1
finally:
    for fd in fds:
        os.close(fd)
print(json.dumps({'seconds': args.seconds, 'event_bytes': event.size,
                  'devices': counts}, sort_keys=True))
