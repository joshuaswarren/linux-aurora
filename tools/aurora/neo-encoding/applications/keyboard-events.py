#!/usr/bin/env python3
"""Observe only function/media keys, panel brightness and mixer volume on the Neo."""
import argparse
import fcntl
import json
import os
from pathlib import Path
import selectors
import struct
import subprocess
import time

KEYS = {**{59 + i: 'F' + str(i + 1) for i in range(10)}, 87: 'F11', 88: 'F12',
        113: 'Mute', 114: 'VolumeDown', 115: 'VolumeUp', 224: 'BrightnessDown',
        225: 'BrightnessUp', 464: 'Fn'}

def state():
    result = {'brightness': {}}
    for node in Path('/sys/class/backlight').glob('*'):
        result['brightness'][node.name] = {key: (node / key).read_text().strip()
            for key in ['brightness', 'actual_brightness', 'max_brightness'] if (node / key).exists()}
    command = subprocess.run(['runuser', '-u', 'mcruz', '--', 'env', 'XDG_RUNTIME_DIR=/run/user/1000',
                              'wpctl', 'get-volume', '@DEFAULT_AUDIO_SINK@'], capture_output=True, text=True, timeout=3)
    result['mixer'] = command.stdout.strip() if command.returncode == 0 else 'No default sink'
    return result

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--output', type=Path, required=True)
parser.add_argument('--seconds', type=int, default=120)
args = parser.parse_args()
if b'apple,j700\0' not in Path('/proc/device-tree/compatible').read_bytes() or os.geteuid() != 0:
    parser.error('Run with sudo on the Neo J700')
if not 1 <= args.seconds <= 600: parser.error('Use a duration between 1 and 600 seconds')
args.output.parent.mkdir(parents=True, exist_ok=True)
selector = selectors.DefaultSelector()
devices = []
for node in Path('/dev/input').glob('event*'):
    fd = os.open(node, os.O_RDONLY | os.O_NONBLOCK)
    # Query supported key bits and select devices that expose at least one key
    # in our whitelist. Never grab them; Plasma must receive the same events.
    bits = bytearray(96)
    try: fcntl.ioctl(fd, 0x80604521, bits, True)  # EVIOCGBIT(EV_KEY, 96)
    except OSError: os.close(fd); continue
    if not any(bits[code // 8] & (1 << (code % 8)) for code in KEYS):
        os.close(fd); continue
    selector.register(fd, selectors.EVENT_READ, str(node))
    devices.append(str(node))
if not devices: raise SystemExit('No keyboard event devices found')
event = struct.Struct('llHHi')
deadline = time.monotonic() + args.seconds
with args.output.open('x') as log:
    def record(value):
        log.write(json.dumps(dict(time=time.time(), **value)) + '\n'); log.flush()
    record({'devices': devices, 'initial_state': state(), 'ordinary_typing_recorded': False})
    print('KEY_TEST_READY', flush=True)
    try:
        while time.monotonic() < deadline:
            for key, _ in selector.select(timeout=min(0.5, max(0, deadline-time.monotonic()))):
                payload = os.read(key.fd, event.size * 64)
                for offset in range(0, len(payload) - event.size + 1, event.size):
                    _, _, kind, code, value = event.unpack_from(payload, offset)
                    if kind == 1 and code in KEYS:
                        record({'device': key.data, 'key': KEYS[code], 'value': value})
                        if value == 0:
                            time.sleep(0.2)
                            record({'after_release': KEYS[code], 'state': state()})
        record({'final_state': state()})
    finally:
        for key in list(selector.get_map().values()): os.close(key.fd)
        selector.close()
print('KEY_TEST_CAPTURED', args.output)
