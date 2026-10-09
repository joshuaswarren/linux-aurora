#!/usr/bin/env python3
"""Verify a native FFmpeg/V4L2 recording against its original NV12 frames."""
import argparse
import hashlib
import json
import math
import os
from pathlib import Path
import subprocess

p = argparse.ArgumentParser(description=__doc__)
p.add_argument('directory', type=Path)
a = p.parse_args()
root = a.directory.resolve()
os.umask(0o077)
source = (root / 'input.nv12').read_bytes()
frame_bytes = 320 * 240 * 3 // 2
assert len(source) % frame_bytes == 0
count = len(source) // frame_bytes
assert 3 <= count <= 120

def status(name):
    return dict(line.split('=', 1) for line in (root / name).read_text().splitlines() if '=' in line)

before, after = status('before.txt'), status('after.txt')
assert before['encoder_owned'] == after['encoder_owned'] == '1'
assert before['command_failed'] == after['command_failed'] == '0'
assert int(after['v4l2_frames']) - int(before['v4l2_frames']) == count
assert int(after['commands']) - int(before['commands']) == 6 * count
assert int(after['replies']) - int(before['replies']) == 6 * count
elapsed_ns = int(after['v4l2_encode_ns']) - int(before['v4l2_encode_ns'])
assert elapsed_ns > 0
log = (root / 'ffmpeg.log').read_text()
assert 'h264_v4l2m2m' in log and 'apple-ave' in log
assert (root / 'ffmpeg.rc').read_text().strip() == '0'
mode = (root / 'mode.txt').read_text().strip() if (root / 'mode.txt').exists() else 'remux'
assert mode in ('remux', 'mpegts')
if mode == 'remux':
    assert (root / 'remux.rc').read_text().strip() == '0'
recording = root / ('recording.ts' if mode == 'mpegts' else 'recording.mkv')
probe = json.loads(subprocess.check_output([
    'ffprobe', '-v', 'error', '-select_streams', 'v:0', '-count_frames', '-show_frames',
    '-show_entries', 'stream=codec_name,width,height,avg_frame_rate,nb_read_frames:frame=best_effort_timestamp_time,key_frame',
    '-of', 'json', str(recording)]))
stream = probe['streams'][0]
assert (stream['codec_name'], stream['width'], stream['height'], stream['avg_frame_rate']) == ('h264', 320, 240, '1/1')
assert int(stream['nb_read_frames']) == len(probe['frames']) == count
for i, frame in enumerate(probe['frames']):
    assert frame['key_frame'] == 1 and float(frame['best_effort_timestamp_time']) == i
decoded = subprocess.check_output([
    'ffmpeg', '-v', 'error', '-i', str(recording), '-map', '0:v:0', '-pix_fmt', 'nv12',
    '-fps_mode', 'passthrough', '-f', 'rawvideo', 'pipe:1'])
assert len(decoded) == len(source)
psnr = []
for i in range(count):
    start, end = i * frame_bytes, (i + 1) * frame_bytes
    mse = sum((x - y) ** 2 for x, y in zip(source[start:end], decoded[start:end])) / frame_bytes
    value = 10 * math.log10(255 ** 2 / mse) if mse else None
    assert value is None or value >= 35, (i, value)
    psnr.append(value)
result = dict(passed=True, interface='V4L2 memory-to-memory', application='stock FFmpeg',
    encoder='h264_v4l2m2m', driver='apple-ave', hardware='M5 / J813',
    width=320, height=240, fps=1, frames=count, all_idr=True,
    independent_cpu_decode=True, per_frame_psnr_db=psnr,
    container=mode, container_timestamps_rebuilt_from_input_rate=mode == 'remux',
    encoder_user=(root / 'user.txt').read_text().strip() if (root / 'user.txt').exists() else 'root',
    firmware_commands=6 * count, firmware_replies=6 * count, command_failed=False,
    mean_kernel_encode_milliseconds=elapsed_ns / count / 1e6,
    recording_sha256=hashlib.sha256(recording.read_bytes()).hexdigest(),
    input_sha256=hashlib.sha256(source).hexdigest(),
    configurable_encoding_or_high_resolution_qualified=False)
(root / 'validation.json').write_text(json.dumps(result, indent=2) + '\n')
print('M5_V4L2_RECORDING_VERIFIED', json.dumps(result))
