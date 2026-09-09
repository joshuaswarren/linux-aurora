#!/usr/bin/env python3
"""Validate saved Linux AVE buffers and mux video without contacting hardware.

Requires completed command logs and post-health.log. A parser/decoder fix
can rerun this step without resubmitting any firmware commands.
"""
import argparse
import hashlib
import json
import math
from pathlib import Path
import re
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[1]
p = argparse.ArgumentParser(description=__doc__)
p.add_argument('output', type=Path)
p.add_argument('--codec', choices=['h264', 'hevc'], required=True)
p.add_argument('--client', type=int, required=True)
p.add_argument('--elapsed', type=float)
a = p.parse_args()
out = a.output.resolve()
original = (out / 'input.nv12').read_bytes()
assert len(original) % 115200 == 0
frames = len(original) // 115200
assert 2 <= frames <= 300 and 1 <= a.client <= 0xffff
commands = ['open', 'start', 'stop', 'close']
commands += [f'{kind}-{i:03d}' for i in range(frames) for kind in ['process', 'complete']]
for name in commands:
    text = (out / (name + '.log')).read_text()
    assert re.search(r'^AVE_PACKET_RESULT=0$', text, re.M), name
    assert re.search(r'^command_failed=0$', text, re.M), name
health = (out / 'post-health.log').read_text()
assert re.search(r'^0-5$', health, re.M) and re.search(r'^g17p-vulkan-compute: PASS', health, re.M)
assert not re.search(r'(CRIT:|kernel panic|SError|dart.*(?:fault|translation error))', health)


def run(argv, name):
    with (out / (name + '.log')).open('w') as stream:
        result = subprocess.run([str(v) for v in argv], stdout=stream, stderr=subprocess.STDOUT)
    assert result.returncode == 0, f'{name} failed; inspect its saved log'
    return (out / (name + '.log')).read_text()


segments = []
for i in range(frames):
    dest = out / f'frame-{i:03d}.{a.codec}'
    run([sys.executable, ROOT / 'video/ave-extract-frame.py', out / f'arena-{i:03d}.log',
         dest, '--allow-p-frame', '--codec', a.codec], f'extract-{i:03d}')
    segments.append(dest.read_bytes())
elementary = out / ('video.' + a.codec)
elementary.write_bytes(b''.join(segments))
run(['ffmpeg', '-v', 'error', '-i', elementary, '-pix_fmt', 'nv12', '-f', 'rawvideo',
     '-y', out / 'decoded.nv12'], 'decode')
assert not (out / 'decode.log').read_text().strip()
decoded = (out / 'decoded.nv12').read_bytes()
assert len(decoded) == len(original)
quality = []
for i in range(frames):
    source = original[i * 115200:(i + 1) * 115200]
    actual = decoded[i * 115200:(i + 1) * 115200]
    mse = sum((x - y) ** 2 for x, y in zip(source, actual)) / len(source)
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
    assert stream['codec_name'] == a.codec and int(stream['nb_read_frames']) == frames
    assert stream['width'] == 320 and stream['height'] == 240 and stream['r_frame_rate'] == '30/1'
    assert abs(float(probe['format']['duration']) - frames / 30) < 1 / 30
    run(['ffmpeg', '-v', 'error', '-i', dest, '-pix_fmt', 'nv12', '-f', 'rawvideo', '-y',
         out / (suffix + '-decoded.nv12')], 'decode-' + suffix)
    assert not (out / ('decode-' + suffix + '.log')).read_text().strip()
    assert (out / (suffix + '-decoded.nv12')).read_bytes() == decoded
    containers[suffix] = probe
validation = dict(passed=True, codec=a.codec, frames=frames, width=320, height=240, fps=30,
    hardware_source='Neo native Linux AVE', client=a.client, per_frame_psnr_db=quality,
    containers=containers, decoded_sha256=hashlib.sha256(decoded).hexdigest(),
    session_closed=True, cpus_online='0-5', vulkan_values=32,
    elapsed_with_host_transfer_and_validation_seconds=a.elapsed,
    throughput_benchmark=False, persistent_installation_changed=False)
(out / 'validation.json').write_text(json.dumps(validation, indent=2) + '\n')
print('AVE_LINUX_VIDEO_VERIFIED', json.dumps(validation))
