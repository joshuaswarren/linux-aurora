#!/usr/bin/env python3
"""Verify and display captured Linux TA/fragment pixels without synthesizing them.

This qualifies the controlled offscreen draw, not completion bookkeeping,
Mesa/OpenGL or DCP. The PNG contains only captured GPU bytes, enlarged by
an integer factor when requested.
"""
import argparse
import hashlib
import json
from pathlib import Path
import struct
import zlib

WIDTH = HEIGHT = 256
PIXEL_ADDRESS = 0x10000058000
NATIVE_REFERENCE_SHA256 = 'ce4e527e102b5e2f3f0585a6c045bc3eca14e5d225da0a8f3dc24b53f8c60a13'


def verify_pixels(rgba):
    if len(rgba) != WIDTH * HEIGHT * 4:
        raise ValueError('Expected exactly 256x256 captured RGBA pixels')
    # Independent edge equations at pixel centres, in doubled coordinates.
    vertices = ((256, 64), (64, 448), (448, 448))
    foreground = 0
    for y in range(HEIGHT):
        for x in range(WIDTH):
            point = (2 * x + 1, 2 * y + 1)
            inside = all(
                (b[0] - a[0]) * (point[1] - a[1])
                - (b[1] - a[1]) * (point[0] - a[0]) < 0
                for a, b in zip(vertices, vertices[1:] + vertices[:1]))
            expected = bytes((64, 128, 191, 255) if inside else (16, 16, 16, 255))
            offset = (y * WIDTH + x) * 4
            if rgba[offset:offset + 4] != expected:
                raise ValueError('Unexpected GPU pixel at (%d,%d)' % (x, y))
            foreground += inside
    if foreground != 18432:
        raise ValueError('Unexpected triangle coverage')
    return foreground


def read_capture(directory):
    manifest = json.loads((directory / 'manifest.json').read_text())
    if (manifest.get('graphics_draw') is not True
            or manifest.get('device_memory_read') is not False
            or not 0 < manifest.get('physical_pages_read', 0) <= 256):
        raise ValueError('Expected a bounded retained Linux graphics capture')
    objects = {}
    for item in manifest['objects']:
        if ('error' in item or item['label'] in objects
                or Path(item['file']).name != item['file']):
            raise ValueError('Failed, duplicate or invalid capture object')
        data = (directory / item['file']).read_bytes()
        if len(data) != item['size'] or hashlib.sha256(data).hexdigest() != item['sha256']:
            raise ValueError('Capture object changed: ' + item['label'])
        objects[item['label']] = (item, data)
    item, rgba = objects['graphics-pixels']
    if item['context'] != 3 or int(item['address'], 16) != PIXEL_ADDRESS:
        raise ValueError('Captured pixels are not the owned graphics output')
    return rgba, len(objects)


def png(rgba, scale):
    if scale not in (1, 2):
        raise ValueError('Only original size or 2x integer enlargement is supported')
    scanlines = bytearray()
    for y in range(HEIGHT):
        row = b''.join(rgba[(y * WIDTH + x) * 4:(y * WIDTH + x + 1) * 4] * scale
                       for x in range(WIDTH))
        scanlines.extend((b'\0' + row) * scale)

    def chunk(kind, data):
        return (struct.pack('>I', len(data)) + kind + data
                + struct.pack('>I', zlib.crc32(kind + data) & 0xffffffff))

    return (b'\x89PNG\r\n\x1a\n'
            + chunk(b'IHDR', struct.pack('>2I5B', WIDTH * scale, HEIGHT * scale, 8, 6, 0, 0, 0))
            + chunk(b'tEXt', b'Description\0M5 Linux TA/fragment GPU output; 256x256 source pixels; DCP not tested')
            + chunk(b'IDAT', zlib.compress(bytes(scanlines), 9)) + chunk(b'IEND', b''))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('capture', type=Path)
    parser.add_argument('output', type=Path)
    parser.add_argument('--scale', type=int, choices=(1, 2), default=2)
    args = parser.parse_args()
    rgba, objects = read_capture(args.capture)
    foreground = verify_pixels(rgba)
    digest = hashlib.sha256(rgba).hexdigest()
    if digest != NATIVE_REFERENCE_SHA256:
        raise ValueError('Linux pixels differ from the qualified native macOS reference')
    args.output.write_bytes(png(rgba, args.scale))
    report = dict(graphics_pixels_verified=True, pixels=WIDTH * HEIGHT,
                  foreground=foreground, background=WIDTH * HEIGHT - foreground,
                  rgba_sha256=digest, native_reference_exact_match=True,
                  captured_objects=objects, source_size=[WIDTH, HEIGHT],
                  image_size=[WIDTH * args.scale, HEIGHT * args.scale],
                  completion_bookkeeping_qualified=False,
                  mesa_glxgears_verified=False, dcp_verified=False)
    args.output.with_suffix('.json').write_text(json.dumps(report, indent=2) + '\n')
    print(json.dumps(report))


if __name__ == '__main__':
    main()
