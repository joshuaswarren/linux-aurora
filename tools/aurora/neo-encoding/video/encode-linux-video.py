#!/usr/bin/env python3
"""Experimental multi-frame native Linux AVE encoding and lossless muxing.

Requires configured, idle firmware from the working Linux recipe. Uses fixed
320x240/30fps captured AVC/HEVC parameters; no reboot or persistent installation.
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
import tarfile
import time

ROOT = Path(__file__).resolve().parents[1]
PYTHON = ROOT.parent / 'neo-update/.venv/bin/python'
DEBUG = '/sys/kernel/debug/285100000.video-codec'
p = argparse.ArgumentParser(description=__doc__)
p.add_argument('output', type=Path)
p.add_argument('--frames', type=int, default=30)
p.add_argument('--client', type=int)
p.add_argument('--codec', choices=['h264', 'hevc'], default='h264')
p.add_argument('--profile',choices=['main','main10','main42210','depth','disparity'],default='main')
p.add_argument('--pixel-format',choices=['nv12','p010le','p210le','grayf32le'])
p.add_argument('--bit-depth',type=int,choices=[8,10,32])
p.add_argument('--module-binary',type=Path)
p.add_argument('--pattern',choices=['row-wrap','smooth-depth'],default='row-wrap')
p.add_argument('--alpha',choices=['none'],default='none')
a = p.parse_args()
if a.profile!='main':
    auxiliary=a.profile in ('depth','disparity');bits=32 if auxiliary else 10
    if a.codec!='hevc' or a.bit_depth not in (None,bits):
        p.error('The selected profile requires HEVC and its matching input precision')
    expected='grayf32le' if auxiliary else 'p010le' if a.profile=='main10' else 'p210le'
    if a.pixel_format not in (None,expected):p.error('Pixel format does not match the selected HEVC profile')
    command=[sys.executable,str(ROOT/'video/encode-hevc-precision-linux.py'),str(a.output),
        '--profile',a.profile,'--frames',str(a.frames),'--pixel-format',expected,'--bit-depth',str(bits),
        '--pattern',a.pattern]
    if a.client is not None:command+=['--client',str(a.client)]
    if a.module_binary is not None:command+=['--module-binary',str(a.module_binary)]
    raise SystemExit(subprocess.run(command).returncode)
if a.pattern!='row-wrap':p.error('Smooth depth pattern requires depth or disparity')
if a.pixel_format not in (None,'nv12') or a.bit_depth not in (None,8):
    p.error('The captured Main baseline requires 8-bit NV12 input')
relocator = ROOT / ('video/ave-relocate-hevc.py' if a.codec == 'hevc' else 'video/ave-relocate-avc.py')
assert 2 <= a.frames <= 300
out = a.output.resolve()
out.mkdir(parents=True, exist_ok=False)
payload = out / 'payload'
payload.mkdir()
started = time.monotonic()


def run(argv, name):
    with (out / (name + '.log')).open('w') as stream:
        r = subprocess.run([str(x) for x in argv], stdout=stream, stderr=subprocess.STDOUT)
    if r.returncode:
        raise RuntimeError(f'{name} failed; inspect {out / (name + ".log")}. No automatic retry.')
    return (out / (name + '.log')).read_text(errors='replace')


def native(command, name, timeout=20):
    return run([PYTHON, ROOT / 'console.py', '--timeout', timeout, command], name)


def replies(text):
    part = text.split('\nAVE_REPLY_BEGIN\n', 1)[1].split('\nAVE_REPLY_END\n', 1)[0]
    data = bytes.fromhex(' '.join(l.strip() for l in part.splitlines()
                                if re.fullmatch(r'(?: [0-9a-f]{2})+', l)))
    records = []
    while data:
        channel, size, flags, reserved, addr = struct.unpack_from('<IIIIQ', data)
        assert channel == 2 and reserved == 0 and size in (72, 80) and len(data) >= 24 + size
        records.append(data[24:24 + size])
        data = data[24 + size:]
    return records


initial = native(f'cat {DEBUG}/ave-status; printf "AVE_REPLY_BEGIN\\n"; '
                 f'od -An -v -tx1 {DEBUG}/ave-command; printf "AVE_REPLY_END\\n"', 'initial-state')
state = dict(re.findall(r'^([a-z0-9_]+)=([^\r\n]+)', initial, re.M))
module_sha=None
if a.module_binary is not None:
    module_sha=hashlib.sha256(a.module_binary.read_bytes()).hexdigest()
    check=native('sha256sum /tmp/apple-ave.ko','module-identity')
    assert re.search(r'^'+module_sha+r'\s+/tmp/apple-ave.ko$',check,re.M), 'Loaded boot artifact differs'
assert state['ipc_ready'] == '1' and state['command_failed'] == '0' and state['powered_domains'] == '5'
last = replies(initial)
assert len(last) == 1 and struct.unpack_from('<I', last[0])[0] in (1, 4)
client = a.client or struct.unpack_from('<Q', last[0], 0x10)[0] + 1
assert 1 <= client <= 0xffff
remote = f'/tmp/ave-video-{client}'
arena_dma, arena_bytes = int(state['arena_dma'], 0), int(state['arena_bytes'], 0)
shutil.copyfile(ROOT / 'video/linux-ave/run-command.sh', payload / 'run.sh')


def make(name, kind, request):
    run([sys.executable, ROOT / 'video/ave-make-command.py', kind, payload / (name + '.bin'),
         '--client', client, '--request-id', request, '--codec', a.codec], 'make-' + name)


make('open', 'open', 1)
make('stop', 'stop', 3 + 2 * a.frames)
make('close', 'close', 4 + 2 * a.frames)
common = ['--arena-dma', hex(arena_dma), '--arena-bytes', arena_bytes, '--client', client]
run([sys.executable, relocator, 'start', *common,
     '--output', payload / 'start.bin'], 'make-start')
y = [235, 210, 170, 145, 106, 81, 41, 16]
u = [128, 16, 166, 54, 202, 90, 240, 128]
v = [128, 146, 16, 34, 222, 240, 110, 128]
yrow = b''.join(bytes([value]) * 40 for value in y)
uvrow = b''.join(bytes([cu, cv]) * 20 for cu, cv in zip(u, v))
originals = []
for i in range(a.frames):
    name = f'process-{i:03d}'
    run([sys.executable, relocator, 'process', *common,
         '--frame-index', i, '--output', payload / (name + '.bin')], 'make-' + name)
    make(f'complete-{i:03d}', 'complete', 4 + 2 * i)
    # Moving bars exercise changing luma, chroma and inter-frame prediction.
    shift = (i * 8) % 320
    frame = (yrow[shift:] + yrow[:shift]) * 240 + (uvrow[shift:] + uvrow[:shift]) * 120
    originals.append(frame)
    (payload / (name + '.nv12')).write_bytes(frame)
(out / 'input.nv12').write_bytes(b''.join(originals))
archive = out / 'payload.tar.gz'
with tarfile.open(archive, 'w:gz') as tar:
    for path in sorted(payload.iterdir()):
        if path.suffix in ('.bin', '.nv12', '.sh'):
            tar.add(path, arcname=path.name)
run([PYTHON, ROOT / 'push-linux.py', archive, remote + '.tar.gz'], 'push-payload')
native(f'mkdir {remote} && tar -xzf {remote}.tar.gz -C {remote} && '
       f'dd if=/dev/zero of={DEBUG}/ave-arena bs=16384 count=239 conv=notrunc', 'prepare')


def command(name, opcode):
    text = native(f'sh {remote}/run.sh {remote}/{name}.bin', name)
    packets = replies(text)
    assert len(packets) == 1
    words = struct.unpack('<' + 'I' * (len(packets[0]) // 4), packets[0])
    expected = [0] * len(words)
    expected[0], expected[4], expected[8] = opcode, client, 0x15 if opcode == 8 else opcode
    assert list(words) == expected, (name, words)


command('open', 3)
command('start', 6)
for i in range(a.frames):
    name = f'process-{i:03d}'
    native(f'dd if={remote}/{name}.nv12 of={DEBUG}/ave-arena bs=512 seek=8193 count=225 conv=notrunc && '
           f'dd if={DEBUG}/ave-arena of={remote}/verify.nv12 bs=512 skip=8193 count=225 && '
           f'cmp {remote}/{name}.nv12 {remote}/verify.nv12', f'input-{i:03d}')
    command(name, 8)
    command(f'complete-{i:03d}', 9)
    native(f'printf "AVE_ARENA_BEGIN\\n"; dd if={DEBUG}/ave-arena bs=16384 count=239 2>/dev/null | '
           'gzip -c | base64; printf "AVE_ARENA_END\\n"', f'arena-{i:03d}', timeout=45)
    print(f'AVE_VIDEO_FRAME_SUBMITTED {i + 1}/{a.frames}', flush=True)
command('stop', 7)
command('close', 4)
segments = []
for i in range(a.frames):
    path = out / f'frame-{i:03d}.{a.codec}'
    run([sys.executable, ROOT / 'video/ave-extract-frame.py', out / f'arena-{i:03d}.log',
         path, '--allow-p-frame', '--codec', a.codec], f'extract-{i:03d}')
    segments.append(path.read_bytes())
elementary = out / ('video.' + a.codec)
elementary.write_bytes(b''.join(segments))
run(['ffmpeg', '-v', 'error', '-i', elementary, '-pix_fmt', 'nv12', '-f', 'rawvideo',
     '-y', out / 'decoded.nv12'], 'decode')
assert not (out / 'decode.log').read_text().strip()
decoded = (out / 'decoded.nv12').read_bytes()
assert len(decoded) == a.frames * 115200
quality = []
for i, original in enumerate(originals):
    result = decoded[i * 115200:(i + 1) * 115200]
    mse = sum((x - y) ** 2 for x, y in zip(result, original)) / len(original)
    psnr = 10 * math.log10(255 ** 2 / mse) if mse else None
    assert psnr is None or psnr >= 35, (i, psnr)
    quality.append(psnr)
containers = {}
for suffix in ['mp4', 'mkv']:
    dest = out / ('video.' + suffix)
    run(['ffmpeg', '-v', 'error', '-fflags', '+genpts', '-r', '30', '-i', elementary,
         '-c:v', 'copy', '-y', dest], 'mux-' + suffix)
    probe = json.loads(run(['ffprobe', '-v', 'error', '-count_frames', '-show_entries',
        'stream=codec_name,width,height,nb_read_frames,r_frame_rate:format=duration', '-of', 'json', dest],
        'probe-' + suffix))
    stream = probe['streams'][0]
    assert stream['codec_name'] == a.codec and int(stream['nb_read_frames']) == a.frames
    assert stream['width'] == 320 and stream['height'] == 240
    run(['ffmpeg', '-v', 'error', '-i', dest, '-pix_fmt', 'nv12', '-f', 'rawvideo', '-y',
         out / (suffix + '-decoded.nv12')], 'decode-' + suffix)
    assert (out / (suffix + '-decoded.nv12')).read_bytes() == decoded
    containers[suffix] = probe
health = native(f'cat /sys/devices/system/cpu/online; /opt/neo/bin/g17p-vulkan-compute; '
                f'cat {DEBUG}/ave-status; strings {DEBUG}/ave-firmware-log; dmesg', 'post-health')
assert re.search(r'^0-5$', health, re.M) and re.search(r'^g17p-vulkan-compute: PASS', health, re.M)
assert not re.search(r'(CRIT:|kernel panic|SError|dart.*(?:fault|translation error))', health)
validation = dict(passed=True, codec=a.codec, frames=a.frames, width=320, height=240, fps=30,
    hardware_source='Neo native Linux AVE', client=client, per_frame_psnr_db=quality,
    module_sha256=module_sha,
    containers=containers, decoded_sha256=hashlib.sha256(decoded).hexdigest(),
    session_closed=True, cpus_online='0-5', vulkan_values=32,
    elapsed_with_host_transfer_and_validation_seconds=time.monotonic() - started,
    throughput_benchmark=False, persistent_installation_changed=False)
(out / 'validation.json').write_text(json.dumps(validation, indent=2) + '\n')
print('AVE_LINUX_VIDEO_VERIFIED', json.dumps(validation), flush=True)
