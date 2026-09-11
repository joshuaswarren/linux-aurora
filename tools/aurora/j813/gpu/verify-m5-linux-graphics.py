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


def read_capture(directory, context=3):
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
    if not 3 <= context < 64 or item['context'] != context or int(item['address'], 16) != PIXEL_ADDRESS:
        raise ValueError('Captured pixels are not the owned graphics output')
    _, roots = objects['context-root-table']
    low, high = struct.unpack_from('<QQ', roots, context * 16)
    if (low >> 48 != context or high >> 48 != context or not low & 1
            or not high & 1 or (low & 0x0000ffffffffc000) == (high & 0x0000ffffffffc000)):
        raise ValueError('Application root pair is not distinct and tagged for the requested context')
    for label in ('graphics-ta', 'graphics-fragment'):
        _, descriptor = objects[label]
        if struct.unpack_from('<I', descriptor, 0xc)[0] != context:
            raise ValueError('Graphics descriptor targets a different application context')
    return rgba, objects


def verify_usc_pool(objects, manifest, context):
    """Check relocated USC records and the recorded invalid-PTE guard walks."""
    end = (1 << 42) - 0x8000
    reserved = (end - (32 << 30), end)
    units = 19 * 512
    state_item, state = objects['graphics-usc-state']
    slot_item, slot = objects['graphics-uma-slot']
    if (state_item['context'] != 0 or int(state_item['address'], 16) != 0xfffffc20c101c000
            or slot_item['context'] != 0 or int(slot_item['address'], 16) != 0xfffffc20015e8020):
        raise ValueError('Unexpected USC firmware records')
    directory = struct.unpack_from('<Q', state, 0x14)[0]
    freelist = struct.unpack_from('<Q', state, 0x30)[0]
    directory_word, cursors, population, flags = struct.unpack('<4Q', slot)
    # Completed GPU work advances both free-ring cursors. The retained
    # context3 and context4 captures preserve their distance (9728), not the
    # construction-time cursor values. Require every unit to be returned.
    consumer, producer = cursors & 0xffffffff, cursors >> 32
    if (directory_word != directory >> 7 or population != (0x80000 << 32) | units
            or flags != 0 or (producer - consumer) & 0xffffffff != units):
        raise ValueError('USC descriptor does not name the allocated directory')
    item, data = objects['graphics-usc-directory']
    if item['context'] != context or int(item['address'], 16) != directory:
        raise ValueError('USC directory is from a different mapping')
    entries = struct.unpack('<%dQ' % (units + 1), data)
    free_item, free_data = objects['graphics-usc-free-blocks']
    if (free_item['context'] != context or int(free_item['address'], 16) != freelist
            or len(free_data) != 0x4000):
        raise ValueError('Unexpected USC freelist or directory population')
    spans = [(directory, 0x400000, 2), (freelist, 0x4000, 1)]
    for block in range(19):
        addresses = entries[block * 512:(block + 1) * 512]
        base = addresses[0]
        if addresses != tuple(base + unit * 0x1000 for unit in range(512)):
            raise ValueError('USC directory does not cover an owned 2 MiB block')
        spans.append((base, 0x200000, 2))
    # This prefix is sufficient to establish the allocated block addresses.
    # After the first draw the next slot may contain a returned owned unit;
    # it is no longer the zero sentinel checked before publication.
    if entries[-1] != 0 and entries[-1] not in entries[:-1]:
        raise ValueError('USC return slot names storage outside the owned pool')
    occupied = []
    guards = []
    for base, size, count in spans:
        limit = base + size + count * 0x4000
        if base & 0x7fff or not reserved[0] <= base < limit <= reserved[1]:
            raise ValueError('USC storage or guards escaped the reserved driver VA region')
        occupied.append((base, limit))
        guards.extend(base + size + i * 0x4000 for i in range(count))
    ordered = sorted(occupied)
    if any(a[1] > b[0] for a, b in zip(ordered, ordered[1:])):
        raise ValueError('USC allocations overlap another allocation or guard')
    proofs = manifest.get('graphics_usc_guards', [])
    if len(proofs) != len(guards):
        raise ValueError('Missing USC guard-page evidence')
    root_item, roots = objects['context-root-table']
    root_table = int(root_item['address'], 16)
    levels = ((42, 2), (36, 64), (25, 2048), (14, 2048))
    for address, proof in zip(guards, proofs):
        if int(proof['address'], 16) != address or proof['context'] != context:
            raise ValueError('USC guard proof describes a different address')
        table = root_table + context * 16
        walk = [(int(a, 16), int(e, 16)) for a, e in proof['walk']]
        if not 2 <= len(walk) <= len(levels):
            raise ValueError('Incomplete USC guard walk')
        for level, (slot_address, entry) in enumerate(walk):
            shift, count = levels[level]
            if slot_address != table + ((address >> shift) & (count - 1)) * 8:
                raise ValueError('USC guard walk follows a different page table')
            if level == 0 and entry != struct.unpack_from('<Q', roots, context * 16)[0]:
                raise ValueError('USC guard walk has a different context root')
            if level == len(walk) - 1:
                if entry & 1:
                    raise ValueError('USC guard page is mapped')
            elif not entry & 1 or (level and entry & 3 != 3):
                raise ValueError('USC guard walk stops before the invalid entry')
            table = entry & 0x0000ffffffffc000
    return dict(reserved_start=hex(reserved[0]), reserved_end=hex(reserved[1]),
                directory=hex(directory), blocks=19, capacity_bytes=19 * 0x200000,
                unmapped_guard_pages=len(guards))


def verify_completion(objects, frame=1):
    if not 1 <= frame <= 64:
        raise ValueError('Expected a graphics frame in 1..64')
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
        if [u32(queues, pointers + off) for off in (0, 0x30, 0x40)] != [1 + frame * 3] * 3:
            raise ValueError('Graphics command queue has not retired')
    scheduler = data['graphics-scheduler']
    if (u64(scheduler, 0) != expected['graphics-submitted-total'][0]
            or u32(data['graphics-submitted-total'], 0) != 2
            or u32(scheduler, 0xc) != 2
            or any(u32(scheduler, off) for off in (0x14, 0x18, 0x1c, 0x28, 0x4c, 0x70, 0x94))):
        raise ValueError('Graphics scheduler is still pending or linked')
    if [u32(data['graphics-stamps-a'], qid * 4) for qid in (10, 11)] != [frame * 0x100] * 2:
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
    parser.add_argument('--frame', type=int, choices=range(1, 65), default=1,
                        help='Expected final frame generation; earlier frames require separate log evidence')
    parser.add_argument('--context', type=int, choices=range(3, 64), default=3)
    parser.add_argument('--require-usc-pool', action='store_true',
                        help='Require relocated USC storage and unmapped guards in the driver VA reservation')
    args = parser.parse_args()
    if args.frame != 1 and not args.require_completion:
        parser.error('--frame requires --require-completion')
    rgba, objects = read_capture(args.capture, args.context)
    foreground = verify_pixels(rgba)
    digest = hashlib.sha256(rgba).hexdigest()
    if digest != NATIVE_REFERENCE_SHA256:
        raise ValueError('Linux pixels differ from the qualified native macOS reference')
    completion = verify_completion(objects, args.frame) if args.require_completion else False
    usc_pool = verify_usc_pool(objects,
        json.loads((args.capture / 'manifest.json').read_text()), args.context) if args.require_usc_pool else None
    args.output.write_bytes(png(rgba, args.scale))
    report = dict(graphics_pixels_verified=True, pixels=WIDTH * HEIGHT,
                  foreground=foreground, background=WIDTH * HEIGHT - foreground,
                  rgba_sha256=digest, native_reference_exact_match=True,
                  captured_objects=len(objects), source_size=[WIDTH, HEIGHT],
                  image_size=[WIDTH * args.scale, HEIGHT * args.scale],
                  completion_bookkeeping_qualified=completion,
                  completion_frame=args.frame if completion else None,
                  application_context=args.context,
                  usc_pool=usc_pool,
                  mesa_glxgears_verified=False, dcp_verified=False)
    args.output.with_suffix('.json').write_text(json.dumps(report, indent=2) + '\n')
    print(json.dumps(report))


if __name__ == '__main__':
    main()
