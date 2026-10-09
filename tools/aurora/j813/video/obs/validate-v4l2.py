#!/usr/bin/env python3
"""Verify a stock OBS FFmpeg output against native AVE counters and test bars."""
import argparse
import configparser
import hashlib
import json
from pathlib import Path
import re
import subprocess

p = argparse.ArgumentParser(description=__doc__)
p.add_argument('directory', type=Path)
a = p.parse_args()
root = a.directory.resolve()
profile = configparser.ConfigParser(interpolation=None)
profile.read(root / 'profile.ini')
assert profile['AdvOut']['RecType'] == 'FFmpeg'
assert profile['AdvOut']['FFVEncoder'] == 'h264_v4l2m2m'
assert profile['AdvOut']['FFFormat'] == 'mpegts'
assert (root / 'obs-exit.rc').read_text().strip() == '0'
assert (root / 'device-open.txt').read_text().strip() == 'OBS opened /dev/video0 as aurora'
assert (root / 'user.txt').read_text().startswith('uid=1000(aurora)')
log = (root / 'obs.log').read_text()
assert 'no custom encoder registered' in log
assert 'recording started; output=ffmpeg_output' in log
assert 'Segmentation fault' not in log
assert '[m5-v4l2-test] recording stopped;' in log
count = int(re.search(r"Output 'adv_ffmpeg_output': Total frames output: (\d+)", log)[1])
assert 8 <= count <= 24

def status(name):
    return dict(l.split('=', 1) for l in (root / name).read_text().splitlines() if '=' in l)

before, after = status('before.txt'), status('after.txt')
assert before['encoder_owned'] == after['encoder_owned'] == '1'
assert before['command_failed'] == after['command_failed'] == '0'
assert int(after['v4l2_frames']) - int(before['v4l2_frames']) == count
for key in ('commands', 'replies'):
    assert int(after[key]) - int(before[key]) == count * 6
recording = root / 'recording.ts'
probe = json.loads(subprocess.check_output([
    'ffprobe', '-v', 'error', '-select_streams', 'v:0', '-count_frames', '-show_frames',
    '-show_entries', 'stream=codec_name,width,height,avg_frame_rate,nb_read_frames:frame=best_effort_timestamp_time,key_frame',
    '-of', 'json', str(recording)]))
stream = probe['streams'][0]
assert (stream['codec_name'], stream['width'], stream['height'], stream['avg_frame_rate']) == ('h264', 320, 240, '1/1')
assert int(stream['nb_read_frames']) == len(probe['frames']) == count
for i, frame in enumerate(probe['frames']):
    assert frame['key_frame'] == 1 and float(frame['best_effort_timestamp_time']) == i
decoded = subprocess.run(['ffmpeg', '-v', 'error', '-i', str(recording), '-map', '0:v:0',
    '-pix_fmt', 'rgb24', '-fps_mode', 'passthrough', '-f', 'rawvideo', 'pipe:1'],
    capture_output=True, check=True)
assert not decoded.stderr
assert len(decoded.stdout) == count * 320 * 240 * 3
colors = [(235, 235, 235), (221, 221, 32), (32, 221, 221), (32, 221, 32),
          (221, 32, 221), (221, 32, 32), (32, 32, 221), (32, 32, 32)]
phases = []
for i in range(count):
    frame = decoded.stdout[i * 230400:(i + 1) * 230400]
    scores = []
    for phase in range(8):
        errors = []
        for y in (35, 120, 200):
            for bar in range(8):
                off = (y * 320 + bar * 40 + 20) * 3
                errors.extend(abs(a - b) for a, b in zip(frame[off:off + 3], colors[(bar + phase) % 8]))
        scores.append(max(errors))
    # The captured SPS does not qualify RGB colorimetry. This checks the
    # expected rotating scene, allowing matrix/range interpretation error.
    assert min(scores) <= 40, (i, scores)
    phases.append(scores.index(min(scores)))
assert len(set(phases)) >= 3
result = dict(passed=True, application='OBS Studio 32.1.0', output='stock ffmpeg_output',
    encoder='h264_v4l2m2m', interface='V4L2 memory-to-memory', hardware='M5 / J813',
    user='aurora', custom_encoder_or_broker=False, width=320, height=240, fps=1,
    frames=count, firmware_commands=count * 6, firmware_replies=count * 6,
    command_failed=False, clean_application_exit=True, all_idr=True,
    independent_cpu_decode=True, direct_container_timestamps_verified=True,
    synthetic_color_bar_phases=phases, colorimetry_qualified=False,
    recording_sha256=hashlib.sha256(recording.read_bytes()).hexdigest(),
    configurable_encoding_or_high_resolution_qualified=False)
(root / 'validation.json').write_text(json.dumps(result, indent=2) + '\n')
print('M5_OBS_V4L2_VERIFIED', json.dumps(result))
