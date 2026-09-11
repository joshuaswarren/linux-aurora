#!/usr/bin/env python3
"""Verify and display captured Linux TA/fragment pixels without synthesizing them.

This qualifies the controlled offscreen draw. --require-completion also
checks the shared job list, current stamps and firmware notification.
Mesa/OpenGL and DCP are separate. The PNG contains only captured GPU bytes,
enlarged by an integer factor when requested.
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
    return rgba, objects


def verify_completion(objects):
    expected = {
        'graphics-queues': (0xfffffc20c0000000, 0x1c000),
        'graphics-completion-list': (0xfffffc20c0020000, 0x4000),
        'graphics-scheduler': (0xfffffc20c1008000, 0x400),
        'graphics-submitted-total': (0xfffffc20c1080000, 0x4000),
        'graphics-stamps-a': (0xfffffc20c1084000, 0x4000),
        'gfx-event-records': (0xfffffc200002f100, 0x4800),
    }
    data = {}
    for label, (address, size) in expected.items():
        if label not in objects:
            raise ValueError('Missing completion evidence: ' + label)
        item, raw = objects[label]
        if item['context'] != 0 or int(item['address'], 16) != address or len(raw) != size:
            raise ValueError('Unexpected completion object: ' + label)
        data[label] = raw
    u32 = lambda raw, off: struct.unpack_from('<I', raw, off)[0]
    u64 = lambda raw, off: struct.unpack_from('<Q', raw, off)[0]
    root = expected['graphics-completion-list'][0]
    listing = data['graphics-completion-list']
    if (u64(listing, 0), u64(listing, 8)) != (0, root):
        raise ValueError('Graphics completion list is not empty')
    queues = data['graphics-queues']
    for qid, pointers in ((10, 0x4000), (11, 0xc000)):
        if u64(queues, qid * 0xc0 + 0x10) != root:
            raise ValueError('Graphics stages do not share the completion list')
        if [u32(queues, pointers + off) for off in (0, 0x30, 0x40)] != [4, 4, 4]:
            raise ValueError('Graphics command queue has not retired')
    scheduler = data['graphics-scheduler']
    if (u64(scheduler, 0) != expected['graphics-submitted-total'][0]
            or u32(data['graphics-submitted-total'], 0) != 2
            or u32(scheduler, 0xc) != 2
            or any(u32(scheduler, off) for off in (0x14, 0x18, 0x1c, 0x28, 0x4c, 0x70, 0x94))):
        raise ValueError('Graphics scheduler is still pending or linked')
    if [u32(data['graphics-stamps-a'], qid * 4) for qid in (10, 11)] != [0x100, 0x100]:
        raise ValueError('Both current software stamps are required')
    mask = 0
    events = data['gfx-event-records']
    for off in range(0, len(events), 0x48):
        kind = u32(events, off)
        if kind == 4:
            raise ValueError('Firmware recovery event invalidates completion')
        if kind == 1:
            mask |= u64(events, off + 4) | (u64(events, off + 12) << 64)
    if mask & 0xc00 != 0xc00:
        raise ValueError('Both firmware completion notifications are required')
    return True


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
    parser.add_argument('--require-completion', action='store_true',
                        help='Reject captures without complete paired-render ownership evidence')
    args = parser.parse_args()
    rgba, objects = read_capture(args.capture)
    foreground = verify_pixels(rgba)
    digest = hashlib.sha256(rgba).hexdigest()
    if digest != NATIVE_REFERENCE_SHA256:
        raise ValueError('Linux pixels differ from the qualified native macOS reference')
    completion = verify_completion(objects) if args.require_completion else False
    args.output.write_bytes(png(rgba, args.scale))
    report = dict(graphics_pixels_verified=True, pixels=WIDTH * HEIGHT,
                  foreground=foreground, background=WIDTH * HEIGHT - foreground,
                  rgba_sha256=digest, native_reference_exact_match=True,
                  captured_objects=len(objects), source_size=[WIDTH, HEIGHT],
                  image_size=[WIDTH * args.scale, HEIGHT * args.scale],
                  completion_bookkeeping_qualified=completion,
                  mesa_glxgears_verified=False, dcp_verified=False)
    args.output.with_suffix('.json').write_text(json.dumps(report, indent=2) + '\n')
    print(json.dumps(report))


if __name__ == '__main__':
    main()
