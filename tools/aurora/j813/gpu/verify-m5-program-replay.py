#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""Qualify native execution of Linux's three owned M5 program pages."""
import argparse
import hashlib
import json
from pathlib import Path
import struct

from m5_program_replay import PROGRAMS, PAGE


def verify(directory, workload):
    directory = Path(directory)
    manifest = json.loads((directory / 'manifest.json').read_text())
    receipt = json.loads((directory / 'program-replay.json').read_text())
    result = json.loads(Path(workload).read_text())
    variant = receipt.get('variant', 'linux-pages')
    if variant not in ('linux-pages', 'shader-body-only', 'shader-prefix', 'shader-prefix-linux-dispatch'):
        raise ValueError('Unknown replay variant')
    dispatch = variant == 'shader-prefix-linux-dispatch'
    shader_variant = 'shader-prefix' if dispatch else variant
    if (manifest.get('error') or manifest.get('completion_error')
            or manifest.get('program_replay_restore_error')
            or manifest.get('program_memory_written') is not True
            or manifest.get('program_replay_state') != 'restored'
            or receipt.get('state') != 'restored'
            or not manifest.get('completion', {}).get('same_descriptor')
            or manifest.get('compute_context') != 3
            or manifest.get('entry_stamp') != manifest.get('program_replay_expected_stamp', '0x1')):
        raise ValueError('Replay lacks a successful matching completion and restoration')
    for key, value in dict(shader_add=6, expected_add=5, queues=1,
                           compute_values_verified=1024, render_pixels_verified=0).items():
        if result.get(key) != value:
            raise ValueError('Workload did not verify replacement +5 results')
    objects = {}
    for item in manifest['objects']:
        path = (directory / item['file']).resolve()
        if path.parent != directory.resolve():
            raise ValueError('Capture object escaped its directory')
        data = path.read_bytes()
        if (len(data) != item['size'] or hashlib.sha256(data).hexdigest() != item['sha256']
                or item.get('stable') is not True):
            raise ValueError('Replay capture has an unstable, short or corrupt object')
        if item['label'] in objects:
            raise ValueError('Replay capture has duplicate object labels')
        if item['label'] in ('compute-extra-0', 'compute-extra-1', 'compute-extra-2',
                             'compute-cdm', 'completed-extra-0', 'completed-extra-1', 'completed-extra-2'):
            if item.get('mapping_stable') is not True:
                raise ValueError('Replay program or buffer mapping was not stable')
        objects[item['label']] = data
    outputs = struct.unpack_from('<1024I', objects['completed-extra-1'], 0x1d00)
    inputs = struct.unpack_from('<1024I', objects['completed-extra-1'], 0xd00)
    if outputs != tuple(i * 21 + 5 for i in range(1024)) or inputs != tuple(i * 7 for i in range(1024)):
        raise ValueError('Captured GPU buffers do not contain the replacement result')
    before = objects['compute-extra-1']
    if (struct.unpack_from('<1024I', before, 0x1d00) != (0xdeadbeef,) * 1024
            or struct.unpack_from('<1024I', before, 0xd00) != inputs):
        raise ValueError('Replay buffers did not start with the controlled input and sentinel')
    dispatch_offsets = [(0x40 + i * 12 + 4, 8) for i in (23, 24, 25)] + [(0xf20, 4), (0xf68, 8)]
    names = [name for name, *_ in PROGRAMS]
    if dispatch:
        names += ['dispatch-%x' % offset for offset, _ in dispatch_offsets]
    expected_writes = [('replace', name) for name in names]
    expected_writes += [('restore', name) for name in reversed(names)]
    if [(x['phase'], x['page']) for x in receipt['writes']] != expected_writes or not all(
            x.get('verified') is True for x in receipt['writes']):
        raise ValueError('Replay lacks all replacement and restoration readbacks')
    if len(receipt['pages']) != len(names):
        raise ValueError('Replay journal has an unexpected program-page count')
    for (name, address, label, sha), page in zip(PROGRAMS, receipt['pages']):
        if page['name'] != name or int(page['address'], 0) != address or page['size'] != PAGE:
            raise ValueError('Replay journal program geometry differs')
        for phase, hash_key in [('original', 'original_sha256'), ('replacement', 'replacement_sha256')]:
            data = (directory / ('replay-' + phase + '-' + name + '.bin')).read_bytes()
            if len(data) != PAGE or hashlib.sha256(data).hexdigest() != page[hash_key]:
                raise ValueError('Replay journal page hash differs')
        if name == 'shader' and shader_variant in ('shader-body-only', 'shader-prefix'):
            original = (directory / 'replay-original-shader.bin').read_bytes()
            replacement = (directory / 'replay-replacement-shader.bin').read_bytes()
            reconstructed = bytes(0x3c0) + replacement[0x3c0:0x400] + bytes(PAGE - 0x400)
            if (hashlib.sha256(reconstructed).hexdigest() != sha
                    or original[:0x3c0] != replacement[:0x3c0]
                    or (shader_variant == 'shader-body-only' and original[0x400:] != replacement[0x400:])
                    or (shader_variant == 'shader-prefix' and any(replacement[0x400:]))):
                raise ValueError('Replay changed bytes outside the controlled shader variant')
        elif page['replacement_sha256'] != sha:
            raise ValueError('Replay did not use the pinned Linux program page')
        if hashlib.sha256(objects[label]).hexdigest() != page['original_sha256']:
            raise ValueError('Original program does not match the submission capture')
    if dispatch:
        if receipt.get('gpu_program_only') is not False or receipt.get('descriptor_identity_fields') != 5:
            raise ValueError('Dispatch replay scope is not explicit')
        descriptor = next(o for o in manifest['objects'] if o['label'] == 'payload-view-0')
        address = int(descriptor['address'], 0)
        original_descriptor = objects['payload-view-0']
        for (offset, size), page in zip(dispatch_offsets, receipt['pages'][3:]):
            name = 'dispatch-%x' % offset
            if (page['name'] != name or int(page['address'], 0) != address + offset
                    or page['size'] != size or page.get('context') is not None):
                raise ValueError('Dispatch replay field geometry differs')
            for phase, key in [('original', 'original_sha256'), ('replacement', 'replacement_sha256')]:
                data = (directory / ('replay-' + phase + '-' + name + '.bin')).read_bytes()
                if len(data) != size or hashlib.sha256(data).hexdigest() != page[key]:
                    raise ValueError('Dispatch replay field hash differs')
                expected = (original_descriptor[offset:offset+size] if phase == 'original' else
                            (1 if offset in (0xf20, 0xf68) else 0x100000001).to_bytes(size, 'little'))
                if data != expected:
                    raise ValueError('Dispatch replay changed an unexpected field value')
    if hashlib.sha256(objects['completed-extra-0']).hexdigest() != receipt['pages'][0]['replacement_sha256']:
        raise ValueError('Shader page at completion differs from the replay variant')
    if hashlib.sha256(objects['completed-extra-2']).hexdigest() != PROGRAMS[1][3]:
        raise ValueError('Pipeline page at completion differs from Linux')
    return dict(qualified=True, native_submission=True, replay_variant=variant,
                linux_program_pages_verified=3 if variant == 'linux-pages' else 2,
                linux_shader_body_bytes_verified=64,
                shader_image_bytes_verified=1024 if shader_variant == 'shader-prefix' else 16384,
                native_shader_page_dependencies_preserved=variant != 'linux-pages',
                compiled_shader_add=6, executed_shader_add=5, compute_values_verified=1024,
                input_values_unchanged=1024, native_pages_restored=3,
                linux_dispatch_identity_verified=dispatch, descriptor_identity_fields_restored=5 if dispatch else 0,
                linux_submission_proven=False, dcp_presentation_tested=False)


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('capture', type=Path)
    parser.add_argument('workload', type=Path)
    args = parser.parse_args()
    print(json.dumps(verify(args.capture, args.workload), indent=2))
