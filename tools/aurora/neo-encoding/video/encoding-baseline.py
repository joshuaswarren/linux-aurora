#!/usr/bin/env python3
"""Create and validate a deterministic software H.264 reference for AVE work.

Run in neo-builder now, or Linux on Neo once FFmpeg is available. This does
not exercise AVE and never labels a software result as hardware encoding.
"""
import argparse
import hashlib
import json
import math
from pathlib import Path
import platform
import subprocess
import time

parser = argparse.ArgumentParser()
parser.add_argument('output', type=Path)
args = parser.parse_args()
args.output.mkdir(parents=True, exist_ok=False)
width, height, frames, fps = 320, 240, 60, 30
raw = args.output / 'reference.nv12'
encoded = args.output / 'reference-software.h264'
decoded = args.output / 'decoded.nv12'
with raw.open('wb') as stream:
    for frame in range(frames):
        y = bytearray(width * height)
        for row in range(height):
            for col in range(width):
                value = 16 + ((col + row + frame * 3) % 220)
                if (frame * 4) % width <= col < (frame * 4) % width + 32 and 80 <= row < 144:
                    value = 220
                y[row * width + col] = value
        uv = bytes((96 + frame % 48, 160 - frame % 48)) * (width * height // 4)
        stream.write(y)
        stream.write(uv)

command = ['ffmpeg', '-nostdin', '-hide_banner', '-v', 'warning', '-f', 'rawvideo',
           '-pixel_format', 'nv12', '-video_size', f'{width}x{height}', '-framerate', str(fps),
           '-i', str(raw), '-frames:v', str(frames), '-c:v', 'libx264', '-preset', 'medium',
           '-crf', '18', '-pix_fmt', 'yuv420p', str(encoded)]
start = time.monotonic()
with (args.output / 'ffmpeg.log').open('w') as log:
    subprocess.run(command, stderr=log, stdout=log, check=True)
    elapsed = time.monotonic() - start
    subprocess.run(['ffmpeg', '-nostdin', '-hide_banner', '-v', 'warning', '-i', str(encoded),
                    '-f', 'rawvideo', '-pix_fmt', 'nv12', str(decoded)], stdout=log, stderr=log, check=True)
probe = json.loads(subprocess.check_output([
    'ffprobe', '-v', 'error', '-count_frames', '-show_entries',
    'stream=codec_name,profile,width,height,pix_fmt,nb_read_frames', '-of', 'json', str(encoded)], text=True))
source, result = raw.read_bytes(), decoded.read_bytes()
if len(result) != len(source):
    raise SystemExit('Decoded frame byte count mismatch')
mse = sum((a - b) ** 2 for a, b in zip(source, result)) / len(source)
psnr = None if mse == 0 else 10 * math.log10(255 * 255 / mse)
stream = probe['streams'][0]
passed = (stream['codec_name'] == 'h264' and int(stream['nb_read_frames']) == frames
          and stream['width'] == width and stream['height'] == height
          and (psnr is None or psnr >= 35))
report = {'passed': passed, 'execution_host': platform.node(), 'machine': platform.machine(),
          'encoder': 'libx264', 'hardware_encoding': False,
          'ffmpeg_version': subprocess.check_output(['ffmpeg', '-version'], text=True).splitlines()[0],
          'width': width, 'height': height, 'frames': frames, 'fps': fps,
          'source_format': 'nv12', 'encode_seconds': elapsed, 'psnr_db': psnr,
          'command': command, 'stream': stream, 'files': {}}
for path in [raw, encoded, decoded]:
    report['files'][path.name] = {'bytes': path.stat().st_size,
                                'sha256': hashlib.sha256(path.read_bytes()).hexdigest()}
(args.output / 'result.json').write_text(json.dumps(report, indent=2) + '\n')
print(f'SOFTWARE_ENCODING_REFERENCE_{"PASS" if passed else "FAIL"} frames={frames} psnr_db={psnr}')
raise SystemExit(0 if passed else 1)
