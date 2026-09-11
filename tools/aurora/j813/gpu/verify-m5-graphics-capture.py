#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""Verify a bounded M5 native graphics reference; never claims Linux rendering.

The context image contains program/data pages before command publication.
Native pixel verification is a separate post-completion result from that run.
"""
import argparse
import hashlib
import json
from pathlib import Path
import struct

PAGE = 0x4000
PA_MASK = ((1 << 48) - 1) & ~(PAGE - 1)


def require(condition, message):
    if not condition:
        raise ValueError(message)


def load_capture(directory, stage):
    directory = Path(directory)
    manifest = json.loads((directory / 'manifest.json').read_text())
    require(manifest['phase'] == 'before-native-graphics-channel-publication'
            and manifest['stage'] == stage, 'Wrong capture phase or stage')
    require(not manifest.get('error') and not manifest['errors'], 'Incomplete capture')
    require(manifest['device_memory_read'] is False
            and manifest['native_instruction_bytes_checked'] is True
            and manifest['linux_gpu_proven'] is False, 'Invalid evidence boundary')
    require(manifest['gpu_context'] == 3 and manifest['count'] == 1,
            'Requires the controlled single-draw context')
    objects = {}
    for item in manifest['objects']:
        require(Path(item['file']).name == item['file'], 'Invalid object filename')
        require(item['label'] not in objects, 'Duplicate object label')
        data = (directory / item['file']).read_bytes()
        require(len(data) == item['size'] and
                hashlib.sha256(data).hexdigest() == item['sha256'], 'Object hash mismatch')
        require(item['stable'] is True, 'Object changed during capture')
        if item['address_space'] == 'GPU':
            require(item['mapping_stable'] is True, 'Object mapping changed')
        objects[item['label']] = (item, data)
    start, count = (0x60, 82) if stage == 'ta' else (0xa0, 90)
    descriptor = objects['work-descriptor'][1]
    require(struct.unpack_from('<I', descriptor, 0xc)[0] == manifest['gpu_context'],
            'Work descriptor context mismatch')
    registers = [struct.unpack_from('<IQ', descriptor, start + i * 12)
                 for i in range(count)]
    host = objects['host-command-descriptor'][1]
    fields = ((0xd499, 0x1120), (0xa5a1, 0x1128), (0x14308, 0x1400)) if stage == 'ta' else (
        (0xd491, 0x7e8), (0xa5a9, 0x7f0), (0x14048, 0xb60))
    evidence = []
    for index, (register, offset) in enumerate(fields):
        if index == 2:
            source, = struct.unpack_from('<i', host, offset)
            expected = source & ((1 << 64) - 1)
        else:
            source, = struct.unpack_from('<Q', host, offset)
            expected = source if index == 0 else (((source or 0x60) << 32)
                | 0x400020) & ((1 << 64) - 1)
        values = [v for k, v in registers if k == register]
        require(values == [expected], 'Native host field differs from emitted register')
        evidence.append(dict(register=hex(register), host_offset=hex(offset),
                             source=hex(source), value=hex(expected)))
    return manifest, objects, registers, evidence


def verify(ta_directory, fragment_directory, result_path):
    require(Path(ta_directory).resolve().parent == Path(fragment_directory).resolve().parent,
            'TA/fragment captures must belong to the same native HV session')
    ta, _, _, ta_fields = load_capture(ta_directory, 'ta')
    fragment, objects, registers, fragment_fields = load_capture(fragment_directory, 'fragment')
    require(ta['stamp'] == fragment['stamp'] and ta['queue_id'] != fragment['queue_id'],
            'TA/fragment submission identity mismatch')
    survey = fragment['context_image']
    require(survey['context'] == 3 and survey['stable'] is True and
            survey['stable_after_resource_reads'] is True, 'Context image is unstable')
    tables = {}
    for item in survey['tables']:
        require(Path(item['file']).name == item['file'], 'Invalid table filename')
        data = (Path(fragment_directory) / item['file']).read_bytes()
        require(len(data) == PAGE and hashlib.sha256(data).hexdigest() == item['sha256'],
                'Page table hash mismatch')
        require(item['physical'] not in tables, 'Duplicate table')
        tables[item['physical']] = data
    physical_files, addresses = {}, set()
    by_file = {item['file']: (item, data) for item, data in objects.values()}
    for mapping in survey['mappings']:
        address, physical = mapping['address'], mapping['physical']
        require(address not in addresses and not (address | physical) & (PAGE - 1),
                'Duplicate or unaligned resource mapping')
        addresses.add(address)
        require(any(start <= address < start + size for start, size in survey['windows']),
                'Resource outside surveyed windows')
        table = survey['root_entry'] & PA_MASK
        for shift, mask in ((36, 63), (25, 2047), (14, 2047)):
            slot = table + ((address >> shift) & mask) * 8
            entry, = struct.unpack_from('<Q', tables[table], slot - table)
            require(entry & 3 == 3, 'Invalid saved translation entry')
            table = entry & PA_MASK
        require(slot == mapping['leaf_slot'] and entry == mapping['leaf']
                and table == physical, 'Saved page table disagrees with mapping')
        item, _ = by_file[mapping['file']]
        require(item['size'] == PAGE and item['gpu_context'] == survey['context']
                and len(item['mapping']) == 1
                and int(item['mapping'][0]['physical'], 16) == physical,
                'Resource receipt disagrees with surveyed physical page')
        require(physical_files.setdefault(physical, mapping['file']) == mapping['file'],
                'Physical alias has inconsistent data file')
    require(0 < len(physical_files) == survey['unique_resource_pages'] <= 384,
            'Wrong resource page count')
    native = json.loads(Path(result_path).read_text())
    require(native['platform'] == 'macOS reference' and native['gpu'] == 'Apple M5'
            and native['linux_gpu_proven'] is False and native['dcp_presentation_tested'] is False,
            'Wrong native result boundary')
    require(native['width'] == native['height'] == 256 and native['unexpected_pixels'] == 0
            and native['foreground_pixels'] == 18432 and native['background_pixels'] == 47104
            and native['pixels_checked_away_from_edges'] == 63234, 'Native triangle pixel check failed')
    # This witness is scoped to the captured single-layer, single-sample draw.
    # Native 25G76 uses bfi at 89dcbb8/89dcbbc for X bits42..51/Y bits52..61,
    # and bfi at 89dcb98 for the four utile bits38..41.
    utile = [v for k, v in registers if k == 0x10009]
    require(utile == [0xa000], 'Unexpected reference utile configuration')
    tile_state = 0x3717f | (7 << 42) | (7 << 52) | 0x2000000000 | (utile[0] << 26)
    require([v for k, v in registers if k == 0x16068] == [tile_state, 0x40000],
            'M5 tile-state witness mismatch')
    return dict(status='M5_NATIVE_GRAPHICS_REFERENCE_VERIFIED',
        linux_graphics_proven=False, dcp_presentation_tested=False,
        resource_pages=len(physical_files), resource_bytes=len(physical_files) * PAGE,
        table_pages=len(tables), objects=len(objects),
        ta_registers=82, fragment_registers=90, tile_state=hex(tile_state),
        host_field_correlations=dict(ta=ta_fields, fragment=fragment_fields),
        native_pixels_checked=native['pixels_checked_away_from_edges'],
        inputs=dict(ta=str(ta_directory), fragment=str(fragment_directory),
                    native_result=str(result_path)))


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--ta', type=Path, required=True)
    parser.add_argument('--fragment', type=Path, required=True)
    parser.add_argument('--native-result', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    result = verify(args.ta, args.fragment, args.native_result)
    args.output.write_text(json.dumps(result, indent=2) + '\n')
    print(result['status'], 'pages=' + str(result['resource_pages']))
