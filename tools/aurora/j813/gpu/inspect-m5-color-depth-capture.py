#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""Verify controlled M5 color/depth captures and extract their PPP witnesses."""
import argparse
import hashlib
import importlib.util
import json
from pathlib import Path
import struct

PAGE = 0x4000
MASK = ((1 << 48) - 1) & ~(PAGE - 1)


def require(ok, message):
    if not ok:
        raise ValueError(message)


def inspect(directory, rgba, mode):
    spec = importlib.util.spec_from_file_location(
        'graphics_verifier', Path(__file__).with_name('verify-m5-graphics-capture.py'))
    verifier = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(verifier)
    m, objects, registers, _ = verifier.load_capture(directory, 'fragment')
    survey = m['context_image']
    require(survey['context'] == 3 and survey['stable'] and
            survey['stable_after_resource_reads'], 'Unstable context image')
    tables = {}
    for item in survey['tables']:
        require(Path(item['file']).name == item['file'], 'Invalid table filename')
        data = (directory / item['file']).read_bytes()
        require(len(data) == PAGE and hashlib.sha256(data).hexdigest() == item['sha256'],
                'Table hash mismatch')
        require(item['physical'] not in tables, 'Duplicate table')
        tables[item['physical']] = data
    files = {item['file']: (item, data) for item, data in objects.values()}
    maps, aliases = {}, {}
    for mapping in survey['mappings']:
        address, physical = mapping['address'], mapping['physical']
        require(address not in maps and (address | physical) & (PAGE - 1) == 0,
                'Duplicate or unaligned mapping')
        table = survey['root_entry'] & MASK
        for shift, mask in [(36, 63), (25, 2047), (14, 2047)]:
            slot = table + ((address >> shift) & mask) * 8
            entry, = struct.unpack_from('<Q', tables[table], slot - table)
            require(entry & 3 == 3, 'Invalid saved page table entry')
            table = entry & MASK
        require(slot == mapping['leaf_slot'] and entry == mapping['leaf'] and
                table == physical, 'Saved translation mismatch')
        item, data = files[mapping['file']]
        require(item['gpu_context'] == 3 and item['size'] == PAGE and
                len(item['mapping']) == 1 and
                int(item['mapping'][0]['physical'], 16) == physical,
                'Resource receipt mismatch')
        require(aliases.setdefault(physical, mapping['file']) == mapping['file'],
                'Inconsistent physical alias')
        maps[address] = data
    require(len(aliases) == survey['unique_resource_pages'] <= 384,
            'Resource count mismatch')

    def read(address, size):
        result = bytearray()
        while size:
            base = address & ~(PAGE - 1)
            length = min(size, PAGE - (address - base))
            result += maps[base][address - base:address - base + length]
            address += length
            size -= length
        return bytes(result)

    vertices = [-.75, -.75, .25, 1, 1, 0, 0, 1,
                 .75, -.75, .25, 1, 0, 1, 0, 1,
                 0, .75, .25, 1, 0, 0, 1, 1]
    require(read(0x10000018200, 96) == struct.pack('<24f', *vertices),
            'Captured resources do not belong to the controlled RGB workload')
    require(read(0x10000058000, 256 * 256 * 4) == b'\xa5' * (256 * 256 * 4),
            'Pre-submit output sentinel changed')
    pixels = rgba.read_bytes()
    require(len(pixels) == 256 * 256 * 4, 'Wrong output dimensions')
    checked = interior = wrong = 0
    for y in range(256):
        for x in range(256):
            b = (224 - (y + .5)) / 192
            r = (1 - b - ((x + .5) - 128) / 96) * .5
            g = 1 - b - r
            inside = min(r, g, b) > .02
            outside = min(r, g, b) < -.02
            if not inside and not outside:
                continue
            expected = [16, 16, 16, 255]
            if inside:
                expected = ([255, 0, 0, 255] if mode.startswith('flat') else
                            [round(r * 255), round(g * 255), round(b * 255), 255])
                interior += 1
            actual = pixels[(y * 256 + x) * 4:(y * 256 + x + 1) * 4]
            wrong += any(abs(a - e) > 2 for a, e in zip(actual, expected))
            checked += 1
    require(wrong == 0 and checked == 60928 and interior == 16200,
            f'Pixel oracle failed: checked={checked} interior={interior} wrong={wrong}')
    vdm = read(0x1000018000, 0x100)
    ppp = []
    for offset in [0x34, 0x3c, 0x44, 0x4c, 0x54, 0x5c, 0x64, 0x6c, 0x74, 0x7c, 0x84]:
        word0, low = struct.unpack_from('<II', vdm, offset)
        require(word0 & 0xffff00ff == 0 and 2 <= word0 >> 8 <= 10,
                'Unexpected controlled VDM PPP layout')
        count = word0 >> 8
        words = struct.unpack('<' + 'I' * count, read(0x1000000000 + low, count * 4))
        ppp.append(dict(vdm_offset=hex(offset), words=[hex(w) for w in words]))
    fs = [int(w, 16) for w in ppp[1]['words']]
    table_address = ((fs[1] & 0xffff) << 32) | fs[2]
    bindings = (fs[1] >> 16) & 127
    vit = struct.unpack('<' + 'I' * (1 + bindings) * 3,
                        read(table_address, (1 + bindings) * 12))
    return dict(status='M5_NATIVE_COLOR_DEPTH_CAPTURE_VERIFIED', mode=mode,
                native_pixels_checked=checked, native_wrong_pixels=wrong,
                resource_pages=len(aliases), objects=len(objects),
                ppp=ppp, varying_table=[hex(w) for w in vit],
                registers=[dict(register=hex(k), value=hex(v)) for k, v in registers],
                linux_gpu_proven=False, dcp_presentation_tested=False,
                capture=str(directory), rgba=str(rgba))


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--capture', type=Path, required=True)
    parser.add_argument('--rgba', type=Path, required=True)
    parser.add_argument('--mode', choices=['color', 'depth', 'flat', 'flat-depth'], required=True)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    result = inspect(args.capture, args.rgba, args.mode)
    args.output.write_text(json.dumps(result, indent=2) + '\n')
    print(result['status'], args.mode, 'pages=' + str(result['resource_pages']),
          'pixels=' + str(result['native_pixels_checked']))
