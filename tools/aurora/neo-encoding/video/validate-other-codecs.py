#!/usr/bin/env python3
"""Decode saved target samples and produce playable files without re-encoding."""
import argparse
import hashlib
import json
from pathlib import Path
import subprocess
import tempfile


def run(args):
    return subprocess.run(args, check=True, text=True, capture_output=True).stdout


def validate(directory):
    report = json.loads((directory / 'result.json').read_text())
    if not report['passed']:
        return {'codec': report['codec'], 'encoded': False,
                'create_status': report['create_status'], 'hardware_query_status': report['hardware_query_status']}
    assert report['model'] == 'Mac17,5' and '25G76' in report['system_version']
    codec = 'mjpeg' if report['codec'] == 'jpeg' else 'prores'
    movie = directory / ('hardware-jpeg.mov' if codec == 'mjpeg' else 'hardware-prores.mov')
    if movie.exists():
        raise ValueError(f'Refusing to overwrite {movie}')
    run(['ffmpeg', '-v', 'error', '-f', 'image2', '-framerate', str(report['fps']),
         '-c:v', codec, '-i', str(directory / 'frame-%03d.sample'), '-c:v', 'copy',
         '-tag:v', 'jpeg' if codec == 'mjpeg' else report['codec'], str(movie)])
    probe = json.loads(run(['ffprobe', '-v', 'error', '-count_frames', '-show_entries',
                           'stream=codec_name,profile,width,height,pix_fmt,nb_read_frames', '-of', 'json', str(movie)]))['streams'][0]
    assert probe['codec_name'] == codec and int(probe['nb_read_frames']) == report['requested_frames']
    assert (probe['width'], probe['height']) == (report['width'], report['height'])
    with tempfile.TemporaryDirectory() as temp:
        stats = Path(temp) / 'psnr.txt'
        # Compare in a common limited-range planar format. JPEG is full range;
        # swscale performs its required range conversion during decoding.
        run(['ffmpeg', '-v', 'error', '-i', str(movie), '-f', 'rawvideo', '-pixel_format',
             report['input_pixel_format'], '-video_size', f"{report['width']}x{report['height']}",
             '-framerate', str(report['fps']), '-i', str(directory / 'input.raw'),
             '-filter_complex', f'[0:v]scale=in_range=auto:out_range=tv,format=yuv422p[a];[1:v]format=yuv422p[b];[a][b]psnr=stats_file={stats}',
             '-f', 'null', '-'])
        rows = [dict(item.split(':', 1) for item in line.split()) for line in stats.read_text().splitlines()]
        assert len(rows) == report['requested_frames']
        minimum = min(float(row['psnr_avg']) for row in rows)
        assert minimum >= 25, f'Decoded output differs materially from input: {minimum} dB'
        hashes = run(['ffmpeg', '-v', 'error', '-i', str(movie), '-f', 'framemd5', '-'])
        frame_hashes = [line.rsplit(',', 1)[-1].strip() for line in hashes.splitlines() if not line.startswith('#')]
        assert len(set(frame_hashes)) == len(frame_hashes), 'Moving frames unexpectedly repeat'
    result = {'codec': report['codec'], 'encoded': True, 'decoded_frames': len(rows),
              'minimum_psnr_db': minimum, 'stream': probe, 'movie': movie.name,
              'movie_sha256': hashlib.sha256(movie.read_bytes()).hexdigest(),
              'scope': 'Neo macOS 25G76 hardware reference; not native Linux proof',
              'hardware_evidence': report.get('hardware_evidence', 'session_property')}
    (directory / 'validation.json').write_text(json.dumps(result, indent=2) + '\n')
    return result


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('root', type=Path)
    args = parser.parse_args()
    results = [validate(p.parent) for p in sorted(args.root.glob('*/result.json'))]
    (args.root / 'validation-summary.json').write_text(json.dumps(results, indent=2) + '\n')
    for result in results:
        print(result['codec'], 'PASS' if result['encoded'] else 'UNAVAILABLE',
              result.get('decoded_frames', 0), result.get('minimum_psnr_db', ''))
