#!/usr/bin/env python3
"""Independently verify an OBS MKV against saved M5 AVE packets and inputs."""
import argparse
import hashlib
import json
import math
import os
from pathlib import Path
import re
import subprocess

p = argparse.ArgumentParser(description=__doc__)
p.add_argument('evidence', type=Path)
p.add_argument('--recording', type=Path)
a = p.parse_args()
os.umask(0o077)
root = a.evidence.resolve()
recording = a.recording or next(root.glob('*.mkv'))
result = json.loads((root / 'broker-result.json').read_text())
assert result['passed'] and result['final_status']['command_failed'] == '0'
records = [json.loads(line) for line in (root / 'frames.jsonl').read_text().splitlines()]
frames = [item for item in records if item['index'] >= 0]
assert len(frames) == result['obs_frames']
assert [item['index'] for item in records] == [-1] + list(range(len(frames)))
assert result['preflight_frames'] == 1
assert all(item['input_readback'] and item['session_closed'] for item in records)
assert result['firmware_commands'] == 6 * len(records)
assert len({f['input_sha256'] for f in frames}) >= 3

def run(args, filename):
    with (root / filename).open('w') as log:
        completed = subprocess.run([str(v) for v in args], stdout=log, stderr=subprocess.STDOUT)
    assert completed.returncode == 0, filename
    return (root / filename).read_text()

probe = json.loads(run(['ffprobe', '-v', 'error', '-select_streams', 'v:0', '-count_frames',
    '-show_frames', '-show_entries',
    'stream=codec_name,width,height,avg_frame_rate,nb_read_frames:frame=best_effort_timestamp_time,key_frame:format=duration',
    '-of', 'json', recording], 'ffprobe.json'))
stream = probe['streams'][0]
assert (stream['codec_name'], stream['width'], stream['height'], stream['avg_frame_rate']) == ('h264', 320, 240, '1/1')
count = int(stream['nb_read_frames'])
assert 8 <= count <= len(frames) <= 24
assert len(probe['frames']) == count
for index, frame in enumerate(probe['frames']):
    assert frame['key_frame'] == 1
    assert float(frame['best_effort_timestamp_time']) == frames[index]['pts']

assert not run(['ffmpeg', '-v', 'error', '-i', recording, '-map', '0:v:0', '-pix_fmt', 'nv12',
    '-fps_mode', 'passthrough', '-f', 'rawvideo', '-y', root / 'decoded.nv12'], 'decode.log').strip()
assert not run(['ffmpeg', '-v', 'error', '-i', recording, '-map', '0:v:0', '-c:v', 'copy',
    '-bsf:v', 'h264_mp4toannexb', '-f', 'h264', '-y', root / 'demuxed.h264'], 'demux.log').strip()

def slices(data):
    starts = list(re.finditer(b'\x00\x00(?:\x00)?\x01', data))
    nals = []
    for i, match in enumerate(starts):
        end = starts[i + 1].start() if i + 1 < len(starts) else len(data)
        nal = data[match.end():end]
        if nal and nal[0] & 31 == 5:
            nals.append(nal)
    return nals

expected = []
for entry in frames[:count]:
    packet = (root / f"frame-{entry['index']:04d}.h264").read_bytes()
    assert hashlib.sha256(packet).hexdigest() == entry['output_sha256']
    nals = slices(packet)
    assert len(nals) == 1
    expected.extend(nals)
assert slices((root / 'demuxed.h264').read_bytes()) == expected, 'OBS changed or substituted hardware slice data'
decoded = (root / 'decoded.nv12').read_bytes()
assert len(decoded) == count * 115200
quality = []
for index, entry in enumerate(frames[:count]):
    original = (root / f'frame-{index:04d}.nv12').read_bytes()
    assert len(original) == 115200 and hashlib.sha256(original).hexdigest() == entry['input_sha256']
    actual = decoded[index * 115200:(index + 1) * 115200]
    mse = sum((x - y) ** 2 for x, y in zip(actual, original)) / 115200
    psnr = 10 * math.log10(255 ** 2 / mse) if mse else None
    assert psnr is None or psnr >= 35, (index, psnr)
    quality.append(psnr)
validation = dict(passed=True, application='OBS Studio 32.1.0', encoder='m5_ave_h264',
    hardware='Apple M5 AVE under native J813 Linux', width=320, height=240, fps=1,
    container_frames=count, duration_seconds=float(probe['format']['duration']),
    broker_encoded_frames=len(frames), preflight_frames=result['preflight_frames'],
    firmware_commands=result['firmware_commands'], all_idr=True,
    original_hardware_slice_bytes_preserved=True, input_dma_readback=True,
    independent_cpu_decode=True, per_frame_psnr_db=quality,
    recorded_unique_inputs=len({f['input_sha256'] for f in frames[:count]}),
    command_failed=False, all_firmware_sessions_closed=True,
    recording_sha256=hashlib.sha256(recording.read_bytes()).hexdigest(),
    general_v4l2_encoder=False, high_resolution_or_normal_frame_rates_qualified=False)
(root / 'validation.json').write_text(json.dumps(validation, indent=2) + '\n')
print('M5_OBS_RECORDING_VERIFIED', json.dumps(validation))
