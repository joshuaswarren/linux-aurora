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


def verify_usc_pool(objects, manifest, context, profile='upper'):
    """Check relocated USC records and the recorded invalid-PTE guard walks."""
    end = (1 << 42) - 0x8000
    if profile not in ('upper', 'application'):
        raise ValueError('Unknown USC reservation profile')
    reserved = ((end - (32 << 30), end) if profile == 'upper'
                else (0x1100000000, 0x1800000000))
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


def verify_mapping_walk(objects, context, address, walk, physical=None):
    root_item, roots = objects['context-root-table']
    table = int(root_item['address'], 16) + context * 16
    levels = ((42, 2), (36, 64), (25, 2048), (14, 2048))
    if not 2 <= len(walk) <= 4 or (physical is not None and len(walk) != 4):
        raise ValueError('Incomplete parameter-buffer mapping walk')
    for level, (slot, value) in enumerate(walk):
        slot, value = int(slot, 16), int(value, 16)
        shift, count = levels[level]
        index = (address >> shift) & (count - 1)
        if slot != table + index * 8:
            raise ValueError('Parameter-buffer walk follows a different page table')
        if level == 0 and value != struct.unpack_from('<Q', roots, context * 16 + index * 8)[0]:
            raise ValueError('Parameter-buffer walk has a different root')
        if physical is None and level == len(walk) - 1:
            if value & 1:
                raise ValueError('Parameter-buffer guard is mapped')
        elif not value & 1 or (level and value & 3 != 3):
            raise ValueError('Parameter-buffer data page is unmapped')
        table = value & 0x0000ffffffffc000
    if physical is not None and table + (address & 0x3fff) != physical:
        raise ValueError('Parameter-buffer physical mapping disagrees with its PTE')


def verify_parameter_buffer(objects, manifest, context):
    u32 = lambda raw, off: struct.unpack_from('<I', raw, off)[0]
    u64 = lambda raw, off: struct.unpack_from('<Q', raw, off)[0]
    def get(label, address, size, ctx):
        item, raw = objects[label]
        if item['context'] != ctx or int(item['address'], 16) != address or len(raw) != size:
            raise ValueError('Unexpected parameter-buffer record: ' + label)
        return raw
    state = get('graphics-pm-state', 0xfffffc20c100c000, 0x400, 0)
    scene = get('graphics-pb-scene', 0xfffffc20c1030080, 0x80, 0)
    block_table = get('graphics-pb-blocks', u64(state, 0x44), 24, 0)
    page_list, page_list_high = u64(state, 0x28), u64(state, 0x20)
    slot = get('graphics-pb-slot', 0xfffffc20015e0000, 16, 0)
    segment = (((page_list + 0x7000000000) & 0x7000000000) >> 8) & 0xf0000000
    if ((u32(slot, 0) & ~7) != ((page_list >> 4) & 0xfffffff8)
            or u32(slot, 4) != segment | 48 or u32(slot, 8) != u32(slot, 12)):
        raise ValueError('Parameter-buffer descriptor has a different directory or active pages')
    pages = get('graphics-pb-pages', page_list, 192, context)
    if pages != get('graphics-pb-pages-firmware', page_list_high, 192, 0):
        raise ValueError('Parameter-buffer firmware page-list bytes differ')
    if (u32(state, 0x30), u32(state, 0x34), u32(state, 0x3c), u32(state, 0x58)) != (0x1c000, 48, 3, 0x80000):
        raise ValueError('Parameter-buffer capacity differs from the qualified profile')
    block_pages = [u32(block_table, i * 8) for i in range(3)]
    expected_pages = {first + page for first in block_pages for page in range(16)}
    if len(expected_pages) != 48 or set(struct.unpack('<48I', pages)) != expected_pages:
        raise ValueError('Parameter-buffer page list does not name the owned blocks')
    blocks = [0x1000000000 + (first << 15) for first in block_pages]
    metrics, metrics_high = u64(scene, 0) - 0x40, u64(scene, 8) - 0x40
    get('graphics-pb-metrics', metrics, 0x100, context)
    get('graphics-pb-metrics-firmware', metrics_high, 0x100, 0)
    for low_name, high_name in [('graphics-pb-pages', 'graphics-pb-pages-firmware'),
                                ('graphics-pb-metrics', 'graphics-pb-metrics-firmware')]:
        low, high = objects[low_name][0], objects[high_name][0]
        if low['first_mapping']['physical'] != high['first_mapping']['physical']:
            raise ValueError('Parameter-buffer alias uses different physical backing')
        for item in (low, high):
            mapping = item['first_mapping']
            verify_mapping_walk(objects, item['context'], int(item['address'], 16),
                                mapping['walk'], int(mapping['physical'], 16))
    spans = [(base, 0x80000) for base in blocks] + [
        (page_list, 0x1c000), (metrics, 0x4000),
        (0x1000000000 + u64(scene, 0x28) - 0x40, 0x4000),
        (0x1000000000 + u64(state, 0x84), 0x8000)]
    proofs = manifest.get('graphics_pb_spans', [])
    if len(proofs) != 7:
        raise ValueError('Missing parameter-buffer mapping evidence')
    occupied = []
    for (base, size), proof in zip(spans, proofs):
        limit = base + size + 0x8000
        if base & 0x7fff or not 0x1000000000 <= base < limit <= 0x1100000000:
            raise ValueError('Parameter-buffer storage escaped the compact driver arena')
        if (int(proof['address'], 16), proof['size'], proof['context']) != (base, size, context):
            raise ValueError('Parameter-buffer proof describes another allocation')
        occupied.append((base, limit))
        if len(proof['pages']) != size // 0x4000 or len(proof['guards']) != 2:
            raise ValueError('Missing parameter-buffer page or guard evidence')
        for label in ('graphics-pb-pages', 'graphics-pb-metrics'):
            item = objects[label][0]
            if int(item['address'], 16) == base and (
                    int(item['first_mapping']['physical'], 16)
                    != int(proof['pages'][0]['physical'], 16)):
                raise ValueError('Parameter-buffer data capture and span mapping disagree')
        for index, page in enumerate(proof['pages']):
            address = base + index * 0x4000
            if int(page['address'], 16) != address:
                raise ValueError('Parameter-buffer page proof describes another address')
            verify_mapping_walk(objects, context, address, page['walk'], int(page['physical'], 16))
        for index, guard in enumerate(proof['guards']):
            address = base + size + index * 0x4000
            if int(guard['address'], 16) != address:
                raise ValueError('Parameter-buffer guard proof describes another address')
            verify_mapping_walk(objects, context, address, guard['walk'])
    ordered = sorted(occupied)
    if any(a[1] > b[0] for a, b in zip(ordered, ordered[1:])):
        raise ValueError('Parameter-buffer allocations or guards overlap')
    return dict(blocks=[hex(b) for b in blocks], page_list=hex(page_list),
                capacity_bytes=3 * 0x80000, mapped_pages=sum(s // 0x4000 for _, s in spans),
                unmapped_guard_pages=14, firmware_aliases_verified=True)


def verify_render_targets(objects, manifest, context):
    ta = objects['graphics-ta'][1]
    fragment = objects['graphics-fragment'][1]
    tr = dict(struct.unpack_from('<IQ', ta, 0x60 + i * 12) for i in range(82))
    fr = dict(struct.unpack_from('<IQ', fragment, 0xa0 + i * 12) for i in range(90))
    if fr[0x15211] != (256 << 32) | 256:
        raise ValueError('Render-target evidence is not the qualified geometry')
    base = 0x1000000000
    target = base + tr[0x1c039]
    deflake = base + (tr[0x1c950] & 0xffffffff)
    expected = [
        ('tilemap', target, 0x4000, 0x1200),
        ('tpc', base + tr[0x1c0a1], 0x4000, 0x100),
        ('deflake', deflake, 0x4000, 0x1000),
        ('ta-status', tr[0x14318] & ~1, 0x4000, 0x100),
        ('fragment-status', fr[0x14080] & ~1, 0x4000, 0x100),
        ('auxiliary', fr[0x16461], 0x8000, 0x800)]
    if (base + tr[0x1c9c8] != target or fr[0x16098] != target + 0x1000
            or base + tr[0x1c079] != fr[0x16098]
            or base + tr[0x10111] != deflake + 0x340
            or base + tr[0x10119] != deflake + 0x40
            or base + tr[0x1c880] != deflake + 0x340
            or fr[0x16090] != fr[0x16461]):
        raise ValueError('Render descriptors disagree about owned target addresses')
    proofs = manifest.get('graphics_target_spans', [])
    if len(proofs) != len(expected):
        raise ValueError('Missing render-target mapping evidence')
    occupied, physical_pages, result = [], [], {}
    for (name, address, size, prefix), proof in zip(expected, proofs):
        label = 'graphics-target-' + name
        item, raw = objects[label]
        if (item['context'] != context or int(item['address'], 16) != address or len(raw) != prefix
                or proof.get('label') != label or int(proof['address'], 16) != address
                or proof['size'] != size or proof['context'] != context):
            raise ValueError('Render-target capture describes a different allocation')
        limit = address + size + 0x8000
        if address & 0x7fff or not base <= address < limit <= base + (1 << 32):
            raise ValueError('Render target escaped the compact driver arena')
        if len(proof['pages']) != size // 0x4000 or len(proof['guards']) != 2:
            raise ValueError('Missing render-target page or guard evidence')
        if item['first_mapping']['physical'] != proof['pages'][0]['physical']:
            raise ValueError('Render-target capture and mapping disagree')
        verify_mapping_walk(objects, context, address, item['first_mapping']['walk'],
                            int(item['first_mapping']['physical'], 16))
        for i, page in enumerate(proof['pages']):
            if int(page['address'], 16) != address + i * 0x4000:
                raise ValueError('Render-target page proof describes a different address')
            physical = int(page['physical'], 16)
            verify_mapping_walk(objects, context, address + i * 0x4000, page['walk'], physical)
            physical_pages.append(physical)
        for i, guard in enumerate(proof['guards']):
            at = address + size + i * 0x4000
            if int(guard['address'], 16) != at:
                raise ValueError('Render-target guard proof describes a different address')
            verify_mapping_walk(objects, context, at, guard['walk'])
        occupied.append((address, limit))
        result[name] = hex(address)
    # The independent PB evidence anchors the neighboring compact objects.
    # No target may consume a PB page or its unmapped guard space.
    for proof in manifest.get('graphics_pb_spans', []):
        at = int(proof['address'], 16)
        occupied.append((at, at + proof['size'] + 0x8000))
        physical_pages.extend(int(p['physical'], 16) for p in proof['pages'])
    ordered = sorted(occupied)
    if any(a[1] > b[0] for a, b in zip(ordered, ordered[1:])) or len(set(physical_pages)) != len(physical_pages):
        raise ValueError('Render targets overlap or share private backing')
    viewport = objects['graphics-target-deflake'][1]
    for off, value in [(0x9c0, 0x3000), (0x9c4, 0x80000007), (0x9c8, 7),
                       (0x9d0, 0x43000000), (0x9d4, 0x43000000),
                       (0x9d8, 0x43000000), (0x9dc, 0xc3000000), (0x9e4, 0x3f800000)]:
        if struct.unpack_from('<I', viewport, off)[0] != value:
            raise ValueError('M5 viewport state differs from the generated reference')
    return dict(addresses=result, unmapped_guard_pages=12, mapped_pages=7,
                m5_viewport_verified=True)


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
    parser.add_argument('--reservation-profile', choices=('upper', 'application'), default='upper',
                        help='Expected driver allocation contract; application excludes the compact 4 GiB arena')
    parser.add_argument('--require-parameter-buffer', action='store_true',
                        help='Require VM-owned parameter-buffer blocks, aliases and guards')
    parser.add_argument('--require-render-targets', action='store_true',
                        help='Require VM-owned render targets and the generated M5 viewport state')
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
        json.loads((args.capture / 'manifest.json').read_text()), args.context,
        args.reservation_profile) if args.require_usc_pool else None
    parameter_buffer = verify_parameter_buffer(objects,
        json.loads((args.capture / 'manifest.json').read_text()), args.context) if args.require_parameter_buffer else None
    render_targets = verify_render_targets(objects,
        json.loads((args.capture / 'manifest.json').read_text()), args.context) if args.require_render_targets else None
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
                  parameter_buffer=parameter_buffer,
                  render_targets=render_targets,
                  mesa_glxgears_verified=False, dcp_verified=False)
    args.output.with_suffix('.json').write_text(json.dumps(report, indent=2) + '\n')
    print(json.dumps(report))


if __name__ == '__main__':
    main()
