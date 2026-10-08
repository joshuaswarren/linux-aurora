#!/usr/bin/env python3
"""Read embedded compiler schemas; no compilation, GPU access, or device I/O.

The FlatBuffers reflection layout is defined by:
https://github.com/google/flatbuffers/blob/master/reflection/reflection.fbs
This pins the host Metal toolchain, not the Neo's installed macOS driver.
"""
import argparse
import hashlib
import json
from pathlib import Path
import struct
import subprocess

COMPILER_SHA256 = 'eb941548121e8e962a7bc9acd2f80d613017601ce967ecf1ae27fb14079ec683'


def default_compiler():
    tool = Path(subprocess.check_output(['xcrun', '-f', 'metal-nt'], text=True).strip())
    return tool.parents[2] / 'usr/metal/32023/lib/libapplegpu-nt.dylib'


class Reflection:
    def __init__(self, data):
        self.data = data

    def read(self, fmt, offset):
        size = struct.calcsize(fmt)
        if offset < 0 or offset + size > len(self.data):
            raise ValueError('out-of-bounds schema read')
        return struct.unpack_from(fmt, self.data, offset)[0]

    def field(self, table, index):
        vtable = table - self.read('<i', table)
        length = self.read('<H', vtable)
        if length < 4 or length % 2:
            raise ValueError('invalid vtable length')
        if 4 + index * 2 >= length:
            return None
        offset = self.read('<H', vtable + 4 + index * 2)
        return table + offset if offset else None

    def scalar(self, table, index, fmt, default=0):
        offset = self.field(table, index)
        return default if offset is None else self.read(fmt, offset)

    def reference(self, offset):
        if offset is None:
            raise ValueError('missing required reference')
        return offset + self.read('<I', offset)

    def string(self, offset):
        start = self.reference(offset)
        length = self.read('<I', start)
        if not 0 < length < 4096 or start + 5 + length > len(self.data):
            raise ValueError('invalid string length')
        if self.data[start + 4 + length] != 0:
            raise ValueError('unterminated string')
        return self.data[start + 4:start + 4 + length].decode('utf-8')

    def vector(self, offset):
        start = self.reference(offset)
        length = self.read('<I', start)
        if length > 4096 or start + 4 + length * 4 > len(self.data):
            raise ValueError('invalid vector length')
        return [self.reference(start + 4 + index * 4) for index in range(length)]

    def objects(self, start):
        root = start + self.read('<I', start)
        result = {}
        for obj in self.vector(self.field(root, 0)):
            name = self.string(self.field(obj, 0))
            fields = []
            for field in self.vector(self.field(obj, 1)):
                type_table = self.reference(self.field(field, 1))
                fields.append({
                    'id': self.scalar(field, 2, '<H'),
                    'name': self.string(self.field(field, 0)),
                    'vtable_offset': self.scalar(field, 3, '<H'),
                    'base_type': self.scalar(type_table, 0, '<B'),
                    'element_type': self.scalar(type_table, 1, '<B'),
                    'type_index': self.scalar(type_table, 2, '<i', -1),
                })
            result[name] = sorted(fields, key=lambda item: item['id'])
        return result


def inspect(path):
    data = path.read_bytes()
    digest = hashlib.sha256(data).hexdigest()
    if digest != COMPILER_SHA256:
        raise ValueError('compiler changed; re-audit its schema and native consumers')
    reader = Reflection(data)
    results, start = [], 0
    while True:
        magic = data.find(b'BFBS', start)
        if magic < 0:
            break
        start = magic + 4
        try:
            objects = reader.objects(magic - 4)
        except (ValueError, UnicodeError, struct.error):
            continue
        if 'AGCCodeGenerator.ShaderInfo' not in objects:
            continue
        fields = {entry['id']: entry for entry in objects['AGCCodeGenerator.ShaderInfo']}
        for index, name in {
            0: 'temporary_register_count',
            14: 'spill_buffer_bytes',
            26: 'const_calc_phases',
            31: 'thread_invariant_spill_buffer_bytes',
            37: 'thread_invariant_largest_ipr_bytes',
            38: 'largest_ipr_bytes',
            41: 'entry_spill_buffer_bytes',
            42: 'entry_ipr_bytes',
        }.items():
            assert fields[index]['name'] == name, (hex(magic - 4), index, fields[index])
            assert fields[index]['vtable_offset'] == 4 + 2 * index
        results.append({'file_offset': magic - 4, 'objects': objects})
    assert results, 'no matching shader schema'
    return {'compiler': str(path), 'compiler_sha256': digest,
            'compiler_bytes': len(data), 'schemas': results,
            'scope': 'Host offline Metal compiler schema; no Neo hardware qualification.'}


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--compiler', type=Path)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    compiler = args.compiler
    if compiler is None:
        compiler = default_compiler()
    result = inspect(compiler)
    args.output.write_text(json.dumps(result, indent=2) + '\n')
    print(json.dumps({'output': str(args.output), 'schemas': len(result['schemas']),
                      'compiler_sha256': result['compiler_sha256']}))
