#!/usr/bin/env python3
"""Resize the real GPU GLX client and retain actual X11 pixels and progress."""
import ctypes as C
import hashlib
import json
import os
from pathlib import Path
import re
import struct
import subprocess
import sys
import tarfile
import time
import zlib


class XImage(C.Structure):
    _fields_ = [(n, t) for n, t in (
        ('width', C.c_int), ('height', C.c_int), ('xoffset', C.c_int),
        ('format', C.c_int), ('data', C.c_void_p), ('byte_order', C.c_int),
        ('bitmap_unit', C.c_int), ('bitmap_bit_order', C.c_int),
        ('bitmap_pad', C.c_int), ('depth', C.c_int),
        ('bytes_per_line', C.c_int), ('bits_per_pixel', C.c_int),
        ('red_mask', C.c_ulong), ('green_mask', C.c_ulong), ('blue_mask', C.c_ulong))]


def chunk(kind, payload):
    return (struct.pack('>I', len(payload)) + kind + payload +
            struct.pack('>I', zlib.crc32(kind + payload)))


def command(*args):
    return subprocess.check_output(args, text=True, timeout=5)


def progress():
    log = command('dmesg')
    values = re.findall(r'M5_APPLICATION generation=(\d+) completed; both queues retired', log)
    return int(values[-1]) if values else 0


def main():
    assert b'apple,j813\0' in Path('/proc/device-tree/compatible').read_bytes()
    os.environ['DISPLAY'] = ':0'
    pid = int(Path('/run/m5-gears-first-frame.pid').read_text())
    assert b'glxgears' in Path(f'/proc/{pid}/cmdline').read_bytes()
    assert 'libvulkan_asahi.so' in Path(f'/proc/{pid}/maps').read_text()
    windows = command('xdotool', 'search', '--pid', str(pid), '--name', 'glxgears').split()
    assert len(windows) == 1, windows
    window = windows[0]
    out = Path(sys.argv[1])
    out.mkdir(exist_ok=False)
    x = C.CDLL('libX11.so.6')
    x.XOpenDisplay.argtypes = [C.c_char_p]
    x.XOpenDisplay.restype = C.c_void_p
    x.XGetImage.argtypes = [C.c_void_p, C.c_ulong, C.c_int, C.c_int,
                           C.c_uint, C.c_uint, C.c_ulong, C.c_int]
    x.XGetImage.restype = C.POINTER(XImage)
    x.XDestroyImage.argtypes = [C.POINTER(XImage)]
    x.XCloseDisplay.argtypes = [C.c_void_p]
    x.XResizeWindow.argtypes = [C.c_void_p, C.c_ulong, C.c_uint, C.c_uint]
    x.XFlush.argtypes = [C.c_void_p]
    display = x.XOpenDisplay(b':0')
    assert display
    report = dict(source='Actual GLX client XGetImage pixels; no generated frames',
                  pid=pid, window=int(window), cases=[], success=False)
    app_log = Path('/run/m5-gears-first-frame.log')
    initial_log_bytes = app_log.stat().st_size
    try:
        full = len(sys.argv) > 2 and sys.argv[2] == 'full'
        sizes = ([(256, 256), (1027, 813), (1280, 720), (1920, 1080),
                  (2560, 1500), (3840, 2160), (512, 384), (63, 63), (33, 17),
                  (16, 16), (1, 1), (15, 65), (16384, 1), (1, 16384),
                  (512, 384)] if full else
                 [(256, 256), (192, 192), (256, 256), (320, 240),
                  (512, 384), (889, 719), (256, 256), (512, 384)])
        if full:
            command('xdotool', 'windowmove', window, '0', '32')
        for case, (width, height) in enumerate(sizes):
            if full and case == len(sizes) - 1:
                drag_before = progress()
                for step in range(80):
                    x.XResizeWindow(display, int(window),
                                    33 + (step * 173) % 2400,
                                    17 + (step * 97) % 1400)
                    x.XFlush(display)
                    time.sleep(0.03)
                report['rapid_resize_requests'] = 80
                report['rapid_resize_generation_before'] = drag_before
            before = progress()
            command('xdotool', 'windowsize', window, str(width), str(height))
            time.sleep(1)
            geometry = command('xdotool', 'getwindowgeometry', '--shell', window)
            dims = dict(line.split('=', 1) for line in geometry.splitlines() if '=' in line)
            assert (int(dims['WIDTH']), int(dims['HEIGHT'])) == (width, height), dims
            capture_width, capture_height = min(width, 1024), min(height, 768)
            capture_x = (min(width, 2400) - capture_width) // 2
            capture_y = (min(height, 1500) - capture_height) // 2
            current = dict(size=[width, height], capture_size=[capture_width, capture_height],
                           capture_origin=[capture_x, capture_y],
                           generation_before=before, frames=[])
            report['cases'].append(current)
            for sample in range(5):
                time.sleep(0.25)
                image = x.XGetImage(display, int(window), capture_x, capture_y,
                                    capture_width, capture_height,
                                    C.c_ulong(-1), 2)
                assert image
                try:
                    i = image.contents
                    assert (i.bits_per_pixel, i.byte_order, i.red_mask, i.green_mask,
                            i.blue_mask) == (32, 0, 0xff0000, 0xff00, 0xff)
                    raw = C.string_at(i.data, i.bytes_per_line * capture_height)
                    lines = bytearray()
                    for y in range(capture_height):
                        row = raw[y * i.bytes_per_line:y * i.bytes_per_line + capture_width * 4]
                        rgb = bytearray(capture_width * 3)
                        rgb[0::3], rgb[1::3], rgb[2::3] = row[2::4], row[1::4], row[0::4]
                        lines.extend(b'\0' + rgb)
                    name = f'case-{case:02d}-{width}x{height}-{sample}.png'
                    png = b'\x89PNG\r\n\x1a\n' + chunk(b'IHDR',
                        struct.pack('>2I5B', capture_width, capture_height, 8, 2, 0, 0, 0))
                    png += chunk(b'IDAT', zlib.compress(lines)) + chunk(b'IEND', b'')
                    (out / name).write_bytes(png)
                    current['frames'].append(dict(file=name,
                        pixel_sha256=hashlib.sha256(lines).hexdigest()))
                finally:
                    x.XDestroyImage(image)
            current['generation_after'] = progress()
            new_log = app_log.read_text(errors='replace')[initial_log_bytes:]
            assert 'MESA: error:' not in new_log, new_log[-1800:]
            assert current['generation_after'] > before + 5, current
            # A one-pixel window can legitimately show only black. Its
            # regression gate is continued paired hardware retirement.
            if min(width, height) >= 16:
                assert len({f['pixel_sha256'] for f in current['frames']}) > 1, current
            print('M5_RESIZE_CASE_PASS', width, height, before,
                  current['generation_after'], flush=True)
        report['success'] = True
    except Exception as error:
        report['error'] = str(error)
        raise
    finally:
        x.XCloseDisplay(display)
        (out / 'manifest.json').write_text(json.dumps(report, indent=2))
        (out / 'application.log').write_text(app_log.read_text(errors='replace'))
        (out / 'kernel.log').write_text(command('dmesg'))
        with tarfile.open(str(out) + '.tar.gz', 'w:gz') as archive:
            for file in sorted(out.iterdir()):
                archive.add(file, arcname=file.name)
        print('M5_RESIZE_EVIDENCE', str(out) + '.tar.gz', flush=True)


if __name__ == '__main__':
    main()
