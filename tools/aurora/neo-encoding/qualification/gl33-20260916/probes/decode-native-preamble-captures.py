#!/usr/bin/env python3
"""Re-read existing offline captures using digest-checked native schema names."""
import argparse
import hashlib
import importlib.util
import json
from pathlib import Path
import struct

HERE = Path(__file__).resolve().parent


def module(name, filename):
    spec = importlib.util.spec_from_file_location(name, HERE / filename)
    result = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(result)
    return result


schema = module('schema', 'inspect-native-shader-schema.py')
capture = module('capture', 'capture-native-preamble-uniforms.py')


def decode(directory):
    receipt = json.loads((directory / 'receipt.json').read_text())
    raw = (directory / 'output.nat').read_bytes()
    assert hashlib.sha256(raw).hexdigest() == receipt['native_sha256']
    assert hashlib.sha256((directory / 'input.metal').read_bytes()).hexdigest() == receipt['source_sha256']
    assert receipt['hardware_execution'] is False
    sections = list(capture.native.sections(raw))
    matches = [x for x in sections if x[1:3] == ('__GPU_METADATA', '__vertex')]
    assert len(matches) == 1
    obj, _, _, metadata = matches[0]
    reader = schema.Reflection(metadata)
    root = reader.read('<I', 0)
    info = reader.reference(reader.field(root, 0))
    phase_field = reader.field(info, 26)
    phases = []
    for phase in reader.vector(phase_field) if phase_field else []:
        inputs = reader.field(phase, 3)
        vector = reader.reference(inputs) if inputs else None
        count = reader.read('<I', vector) if vector is not None else 0
        assert count < 4096
        phases.append({
            'name': reader.string(reader.field(phase, 0)),
            'execution_rate_raw': reader.scalar(phase, 1, '<b'),
            'temporary_register_count': reader.scalar(phase, 2, '<I'),
            'cprog_uniform_inputs': [reader.read('<I', vector + 4 + 4 * i)
                                    for i in range(count)],
        })

    # Native Mach-O symbols independently establish actual constant code.
    assert struct.unpack_from('<I', raw, obj)[0] == 0xfeedfacf
    commands = struct.unpack_from('<I', raw, obj + 16)[0]
    cursor, symbols = obj + 32, {}
    for _ in range(commands):
        command, size = struct.unpack_from('<II', raw, cursor)
        assert size >= 8 and cursor + size <= len(raw)
        if command == 2:
            symoff, count, stroff, strsize = struct.unpack_from('<IIII', raw, cursor + 8)
            assert obj + symoff + count * 16 <= len(raw)
            assert obj + stroff + strsize <= len(raw)
            for i in range(count):
                offset, kind, section, _, value = struct.unpack_from(
                    '<IBBHQ', raw, obj + symoff + 16 * i)
                if kind & 0xe0 or kind & 0x0e != 0x0e or not section:
                    continue
                assert offset < strsize
                end = raw.index(b'\0', obj + stroff + offset, obj + stroff + strsize)
                name = raw[obj + stroff + offset:end].decode()
                if name.startswith('_agc.main'):
                    symbols[name] = value
        cursor += size
    for phase in phases:
        assert '_' + phase['name'] in symbols
    return {
        'directory': str(directory), 'words': receipt.get('words'),
        'workload': receipt.get('workload', directory.name),
        'native_sha256': receipt['native_sha256'],
        'temporary_register_count': reader.scalar(info, 0, '<I'),
        'thread_invariant_spill_buffer_bytes': reader.scalar(info, 31, '<I'),
        'thread_invariant_largest_ipr_bytes': reader.scalar(info, 37, '<I'),
        'largest_ipr_bytes': reader.scalar(info, 38, '<I'),
        'entry_ipr_bytes': reader.scalar(info, 42, '<I'),
        'constant_phases': phases, 'text_symbols': symbols,
        'text_size_including_constant_program_and_padding': receipt['vertex_text_size'],
    }


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('directory', type=Path)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    verified = schema.inspect(schema.default_compiler())
    results = [decode(path) for path in sorted(args.directory.glob('vertex-*'))]
    assert results
    output = {
        'scope': 'Existing offline compiler captures, not a runtime or performance result.',
        'compiler_sha256': verified['compiler_sha256'],
        'schemas_verified': len(verified['schemas']), 'captures': results,
    }
    args.output.write_text(json.dumps(output, indent=2) + '\n')
    print(json.dumps({'output': str(args.output), 'captures': len(results),
                      'constant_programs': sum(len(x['constant_phases']) for x in results)}))
