#!/usr/bin/env python3
"""Encode one 320x240 NV12 frame using the running experimental Neo AVE module.

Requires a configured, idle firmware session (last reply Config or Close).
Does not reboot or install anything. See WORKING_LINUX_ENCODING.md.
"""
import argparse
import hashlib
import json
import math
from pathlib import Path
import re
import shutil
import struct
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[1]
PYTHON = ROOT.parent / 'neo-update/.venv/bin/python'
DEBUG = '/sys/kernel/debug/285100000.video-codec'
p = argparse.ArgumentParser(description=__doc__)
p.add_argument('output', type=Path, help='new result directory')
p.add_argument('--client', type=int, help='default: next client after the last closed session')
p.add_argument('--pattern', choices=['gray', 'color-bars'], default='color-bars')
p.add_argument('--input', type=Path, help='optional exact 115200-byte 320x240 NV12 input')
a = p.parse_args()
assert a.client is None or 1 <= a.client <= 0xffff
out = a.output.resolve()
out.mkdir(parents=True, exist_ok=False)


def run(argv, name):
    with (out / (name + '.log')).open('w') as stream:
        result = subprocess.run([str(x) for x in argv], stdout=stream, stderr=subprocess.STDOUT)
    if result.returncode:
        raise RuntimeError(f'{name} failed; inspect {out / (name + ".log")}. No automatic retry.')
    return (out / (name + '.log')).read_text(errors='replace')


def native(command, name, timeout=20):
    return run([PYTHON, ROOT / 'console.py', '--timeout', str(timeout), command], name)


def push(path, remote, name):
    run([PYTHON, ROOT / 'push-linux.py', path, remote], 'push-' + name)


def replies(text):
    part = text.split('\nAVE_REPLY_BEGIN\n', 1)[1].split('\nAVE_REPLY_END\n', 1)[0]
    data = bytes.fromhex(' '.join(l.strip() for l in part.splitlines()
                                if re.fullmatch(r'(?: [0-9a-f]{2})+', l)))
    result = []
    while data:
        channel, size, flags, reserved, addr = struct.unpack_from('<IIIIQ', data)
        assert channel == 2 and reserved == 0 and 72 <= size <= 80 and len(data) >= 24 + size
        result.append(data[24:24 + size])
        data = data[24 + size:]
    return result


def reply_dump():
    return (f'printf "AVE_REPLY_BEGIN\\n"; od -An -v -tx1 {DEBUG}/ave-command; '
            'printf "AVE_REPLY_END\\n"')


initial = native(f'cat {DEBUG}/ave-status; {reply_dump()}', 'initial-state')
state = dict(re.findall(r'^([a-z0-9_]+)=([^\r\n]+)', initial, re.M))
assert state['ipc_ready'] == '1' and state['command_failed'] == '0'
assert state['powered_domains'] == '5'
last = replies(initial)
assert len(last) == 1 and struct.unpack_from('<I', last[0])[0] in (1, 4), 'Firmware session is not idle'
if a.client is None:
    a.client = struct.unpack_from('<Q', last[0], 0x10)[0] + 1
assert 1 <= a.client <= 0xffff
prefix = f'/tmp/ave-frame-{a.client}'
arena_dma, arena_bytes = int(state['arena_dma'], 0), int(state['arena_bytes'], 0)
push(ROOT / 'video/linux-ave/run-command.sh', prefix + '-run.sh', 'runner')
for command in ['open', 'complete', 'stop', 'close']:
    path = out / (command + '.bin')
    run([sys.executable, ROOT / 'video/ave-make-command.py', command, path,
         '--client', str(a.client)], 'make-' + command)
    push(path, prefix + '-' + command + '.bin', command)
for command in ['start', 'process']:
    path = out / (command + '.bin')
    run([sys.executable, ROOT / 'video/ave-relocate-avc.py', command,
         '--arena-dma', hex(arena_dma), '--arena-bytes', str(arena_bytes),
         '--client', str(a.client), '--pattern', a.pattern, '--output', path], 'make-' + command)
    push(path, prefix + '-' + command + '.bin', command)
source = out / 'process.nv12'
if a.input:
    assert a.input.stat().st_size == 115200
    shutil.copyfile(a.input, source)
push(source, prefix + '-input.nv12', 'input')


def command(name, opcode):
    text = native(f'sh {prefix}-run.sh {prefix}-{name}.bin', name)
    packets = replies(text)
    assert len(packets) == 1
    words = struct.unpack('<' + 'I' * (len(packets[0]) // 4), packets[0])
    # Match the successful captured command replies, substituting this client.
    expected = [0] * len(words)
    expected[0], expected[4], expected[8] = opcode, a.client, 0x15 if opcode == 8 else opcode
    assert list(words) == expected, (name, words)
    print('AVE_FRAME_COMMAND_PASS', name, flush=True)


native(f'dd if=/dev/zero of={DEBUG}/ave-arena bs=16384 count=239 conv=notrunc', 'clear-surfaces')
command('open', 3)
command('start', 6)
native(f'dd if={prefix}-input.nv12 of={DEBUG}/ave-arena bs=512 seek=8193 count=225 conv=notrunc && '
       f'dd if={DEBUG}/ave-arena of={prefix}-verify.nv12 bs=512 skip=8193 count=225 && '
       f'cmp {prefix}-input.nv12 {prefix}-verify.nv12 && printf "AVE_INPUT_READBACK_PASS\\n"', 'input-readback')
command('process', 8)
command('complete', 9)
native(f'printf "AVE_ARENA_BEGIN\\n"; dd if={DEBUG}/ave-arena bs=16384 count=239 2>/dev/null | '
       'gzip -c | base64; printf "AVE_ARENA_END\\n"', 'arena', timeout=45)
command('stop', 7)
command('close', 4)
run([sys.executable, ROOT / 'video/ave-extract-frame.py', out / 'arena.log', out / 'frame.h264'], 'extract')
run(['ffmpeg', '-v', 'error', '-i', out / 'frame.h264', '-frames:v', '1', '-pix_fmt', 'nv12',
     '-f', 'rawvideo', '-y', out / 'decoded.nv12'], 'decode')
assert not (out / 'decode.log').read_text().strip()
decoded, original = (out / 'decoded.nv12').read_bytes(), source.read_bytes()
assert len(decoded) == len(original) == 115200
mse = sum((x - y) ** 2 for x, y in zip(decoded, original)) / len(original)
psnr = 10 * math.log10(255 ** 2 / mse) if mse else None
assert psnr is None or psnr >= 35, ('Unexpected decoded pixels', psnr)
health = native(f'cat /sys/devices/system/cpu/online; /opt/neo/bin/g17p-vulkan-compute; '
                f'cat {DEBUG}/ave-status; strings {DEBUG}/ave-firmware-log; dmesg', 'post-health')
assert re.search(r'^0-5$', health, re.M)
assert re.search(r'^g17p-vulkan-compute: PASS', health, re.M)
assert not re.search(r'(?:CRIT:|kernel panic|SError|dart.*(?:fault|translation error))', health)
validation = dict(passed=True, native_linux_hardware_encoded_frames=1, width=320, height=240,
                  pixel_format='NV12', client=a.client, psnr_db=psnr, mse=mse,
                  stream_bytes=(out / 'frame.h264').stat().st_size,
                  input_sha256=hashlib.sha256(original).hexdigest(),
                  decoded_sha256=hashlib.sha256(decoded).hexdigest(),
                  cpus_online='0-5', vulkan_values=32, session_closed=True,
                  registered_v4l2_encoder=False, persistent_installation_changed=False)
(out / 'validation.json').write_text(json.dumps(validation, indent=2) + '\n')
print('AVE_LINUX_FRAME_VERIFIED', json.dumps(validation), flush=True)
