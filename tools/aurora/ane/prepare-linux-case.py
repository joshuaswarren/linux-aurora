#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-2.0-only
"""Package the locally captured, own-model 25G83 convolution test.

No hardware access and no downloads. The generated header pins every package
byte; input and CPU-reference tensors are regenerated independently. Native
output bytes are checked but are never copied into an ANE-mapped buffer.
"""
import argparse
import hashlib
import json
from pathlib import Path
import struct

BASE = 0x10000000000
EXPECTED_OPS = [4, 0, 0x29, 3, 0x1f, 0x22, 0x1f, 0x1f, 0x1f,
                0x1f, 0x1f, 0x2e, 0x26, 0x2e, 0x2f, 0x200, 0x202]
RESOURCE_LAYOUT = [(0x1d60000, 616), (0x1d4c180, 16384), (0x1d58000, 376),
                   (0x1d64000, 1040), (0x1d68000, 56), (0x1d6c000, 40)]


def frames(directory):
    if (directory / 'error.txt').exists():
        raise ValueError('capture stopped on an error')
    return [json.loads(p.read_text()) for p in sorted(directory.glob('*.json'))]


def requests(records, channel):
    result = []
    for frame in records:
        if frame['phase'] != 'before_guest_write':
            continue
        slots = {(s['channel'], s['slot']): s for s in frame['slots']}
        for payload in frame['payloads']:
            slot = slots[(payload['channel'], payload['slot'])]
            if payload['channel'] != channel or slot['pointer'] & 1:
                continue
            data = bytes.fromhex(payload['bytes'])
            if payload['truncated'] or len(data) != payload['full_length'] or len(data) > 4096:
                raise ValueError('incomplete command')
            if struct.unpack_from('<I', data)[0] or slot['length'] != len(data) or slot['metadata'] != len(data):
                raise ValueError('unexpected command publication')
            result.append(data)
    return result


def half(values):
    return b''.join(struct.pack('<e', x) for x in values)


def reference(values):
    return [(c - 8) / 32 + sum((((c * 7 + s * 3) % 9) - 4) / 32
                               * values[s * 1024 + p] for s in range(16))
            for c in range(16) for p in range(1024)]


def prepare(setup_dir, work_dir, output):
    setup, work = frames(setup_dir), frames(work_dir)
    commands = requests(setup, 'IO') + requests(work, 'IO')
    ops = [struct.unpack_from('<I', b, 4)[0] for b in commands]
    if ops != EXPECTED_OPS + [0x203, 0x201, 1]:
        raise ValueError(f'changed command sequence: {ops}')
    if commands[3] != struct.pack('<4I', 0, 3, 0, 0):
        raise ValueError('changed opcode-3 request')
    data_commands = requests(work, 'DATA_CHAIN_H2T')
    if [struct.unpack_from('<I', b, 4)[0] for b in data_commands] != [0x2d, 0x20c, 0x2d]:
        raise ValueError('changed data-chain sequence')
    if data_commands[0] != struct.pack('<3I', 0, 0x2d, 0) or data_commands[2] != struct.pack('<3I', 0, 0x2d, 1):
        raise ValueError('changed data-chain gate requests')
    refs, resources = {}, []
    for frame in work:
        refs.update({x['kind']: x for x in frame.get('referenced_buffers', [])})
        if frame.get('program_buffers'):
            if resources:
                raise ValueError('multiple program publications')
            resources = frame['program_buffers']
    if [(x['iova'] - BASE, x['length']) for x in resources] != RESOURCE_LAYOUT:
        raise ValueError('changed program resource layout')
    expected_refs = {'setup-2f': (0x3c8000, 0x800), 'input': (0x1f80000, 0x8000),
                     'output-before': (0x1f8c000, 0x8000), 'output-after': (0x1f8c000, 0x8000),
                     'work-aux': (0x1f78000, 0x4000)}
    if set(refs) != set(expected_refs):
        raise ValueError('missing setup, tensor or work memory')
    for name, (offset, size) in expected_refs.items():
        x = refs[name]
        if (x['iova'], x['length']) != (BASE + offset, size) or len(bytes.fromhex(x['bytes'])) != size:
            raise ValueError('changed reference memory')
    if any(bytes.fromhex(refs['setup-2f']['bytes'])):
        raise ValueError('setup memory is no longer empty')
    pattern = [(i % 17 - 8) / 16 for i in range(16384)]
    if half(pattern) != bytes.fromhex(refs['input']['bytes']):
        raise ValueError('captured tensor is not the own-model CHW input')
    if half(reference(pattern)) != bytes.fromhex(refs['output-after']['bytes']):
        raise ValueError('captured output differs from the scalar CHW reference')
    work_token = struct.unpack_from('<Q', data_commands[1], 0x20)[0]
    events = set()
    for frame in work:
        for x in frame['payloads']:
            b = bytes.fromhex(x['bytes'])
            if x['channel'] == 'IO_T2H' and len(b) == 96:
                identifier, opcode, status, token = struct.unpack_from('<IHHQ', b)
                if opcode == 0x300 and not status and token == work_token:
                    events.add(identifier)
    if events != {0xfe00, 0xfe01}:
        raise ValueError('missing matched native work begin/end')
    package = bytearray()
    def append(data):
        package.extend(bytes((-len(package)) % 8))
        offset = len(package)
        package.extend(data)
        return offset, len(data)
    buffer_meta = []
    for item in resources + [refs['setup-2f'], refs['work-aux']]:
        data = bytes.fromhex(item['bytes'])
        if len(data) != item['length']:
            raise ValueError('truncated program data')
        offset, size = append(data)
        buffer_meta.append((item['iova'], offset, size))
    command_meta = [append(b) for b in commands[1:-3]]  # Startup already completed in Linux.
    teardown_meta = [append(b) for b in commands[-3:]]
    work_meta = append(data_commands[1])
    sample_meta, results = [], []
    for name, values in [('captured-pattern', pattern), ('zero-input', [0.0] * 16384),
                         ('one-input', [1.0] * 16384)]:
        input_bytes, expected = half(values), half(reference(values))
        sample_meta.append((append(input_bytes)[0], append(expected)[0]))
        results.append(dict(name=name, values=16384, expected_sha256=hashlib.sha256(expected).hexdigest()))
    digest = hashlib.sha256(package).hexdigest()
    header = '/* Generated locally from validated own-model native captures. */\n'
    header += '#define ANE_CASE_NAME "apple/ane/j700-conv16.case"\n'
    header += f'#define ANE_CASE_SIZE {len(package)}U\n'
    header += f'#define ANE_CASE_INPUT {refs["input"]["iova"]:#x}ULL\n'
    header += f'#define ANE_CASE_OUTPUT {refs["output-before"]["iova"]:#x}ULL\n'
    header += 'static const u8 ane_case_sha[32] = {' + ','.join(str(x) for x in bytes.fromhex(digest)) + '};\n'
    header += 'static const struct { u64 iova; u32 offset, size; } ane_case_buffers[] = {\n'
    header += ''.join(f'{{{address:#x}ULL,{offset}U,{size}U}},\n' for address, offset, size in buffer_meta) + '};\n'
    for name, meta in [('commands', command_meta), ('teardown', teardown_meta), ('work', [work_meta])]:
        header += f'static const struct {{ u32 offset, size; }} ane_case_{name}[] = {{\n'
        header += ''.join(f'{{{offset}U,{size}U}},\n' for offset, size in meta) + '};\n'
    header += 'static const struct { u32 input, expected; } ane_case_samples[] = {\n'
    header += ''.join(f'{{{input_offset}U,{expected}U}},\n' for input_offset, expected in sample_meta) + '};\n'
    output.mkdir(mode=0o700, parents=True, exist_ok=False)
    for name, data in [('j700-conv16.case', bytes(package)), ('apple-ane-case-data.h', header.encode())]:
        path = output / name
        path.write_bytes(data)
        path.chmod(0o600)
    manifest = dict(sha256=digest, bytes=len(package), native_token=work_token,
                    native_output_validated=True, linux_computation_tested=False,
                    samples=results, program_resources=len(resources), setup_commands=len(command_meta))
    (output / 'manifest.json').write_text(json.dumps(manifest, indent=2) + '\n')
    print('ANE_CASE_PREPARED', json.dumps(manifest))


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--setup-capture', type=Path, required=True)
    parser.add_argument('--work-capture', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    prepare(args.setup_capture, args.work_capture, args.output)
