#!/usr/bin/env python3
"""Verify native input copies and offline preamble entry reloads, without a GPU."""
import argparse
import hashlib
import importlib.util
import json
from pathlib import Path
import subprocess

HERE = Path(__file__).resolve().parent
ARTIFACTS = HERE.parent


def module(name, filename):
    spec = importlib.util.spec_from_file_location(name, HERE / filename)
    value = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(value)
    return value


def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--mesa', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--extra-captures', type=Path, action='append', default=[])
    args = parser.parse_args()
    mesa = args.mesa.resolve()
    sources = [HERE / 'decode-static-spill.c'] + [mesa / 'src/asahi/compiler' / name for name in (
        'agx3_static_spill_core.c', 'agx3_binder.c',
        'agx3_compose/agx3_compose.c', 'agx3_compose/agx3b_tables.c',
        'agx3_compose/agx3c_tables.c')]
    headers = [mesa / 'src/asahi/compiler' / name for name in (
        'agx3_static_spill_core.h', 'agx3_mem_core.h', 'agx3_binder.h',
        'agx3_compose/agx3_compose.h')]
    headers += [mesa / 'src/asahi/lib/agx_chip.h', mesa / 'include/drm-uapi/asahi_drm.h']
    command = ['clang', '-std=c11', '-Wall', '-Wextra', '-Werror',
               '-fsanitize=undefined', '-fno-sanitize-recover=all',
               '-I' + str(mesa / 'include'), '-I' + str(mesa / 'src'),
               *map(str, sources), '-o', str(HERE / 'decode-static-spill')]
    subprocess.run(command, check=True)
    native = module('native_cprog_oracle', 'native-cprog-input-oracle.py')
    decoded = module('native_capture_decoder', 'decode-native-preamble-captures.py')
    schemas = decoded.schema.inspect(decoded.schema.default_compiler())
    oracle = native.CprogOracle()
    copies = []
    for destination in (0, 2, 4, 8, 16, 32, 148, 150, 152, 156, 256):
        for mask in (1, 2, 3):
            c = oracle.copy_inputs(destination, mask)
            c['decoded'] = decode(c['records'])
            tps = 8 + destination // 2
            src = 10 + (mask == 2)
            initial_src = src
            for item in c['decoded']:
                assert item['kind'] == 'store' and item['replay_equal']
                assert item['tps_half'] == tps and item['src'] == src
                assert item['src_kill'] == 1 and item['width'] in (1, 2)
                tps += item['width']
                src += item['width']
            assert src - initial_src == mask.bit_count()
            assert c['returned'] == 2 * mask.bit_count()
            copies.append(c)

    captures = []
    folders = [ARTIFACTS / name for name in (
        'native-preamble-uniforms-host27', 'native-preamble-sampling-host27',
        'native-preamble-pressure-host27', 'native-preamble-buffers-host27')]
    folders.extend(path.resolve() for path in args.extra_captures)
    assert len(set(folders)) == len(folders), 'duplicate capture roots'
    for folder in folders:
        directories = sorted(folder.glob('vertex-*'))
        assert directories, folder
        for directory in directories:
            info = decoded.decode(directory)
            assert len(info['constant_phases']) == 1
            inputs = info['constant_phases'][0]['cprog_uniform_inputs']
            assert info['text_symbols']['_agc.main.constant_program'] == 0
            raw = (directory / 'output.nat').read_bytes()
            sections = list(decoded.capture.native.sections(raw))
            obj = next(x[0] for x in sections if x[1:3] == ('__GPU_METADATA', '__vertex'))
            text = next(x[3] for x in sections if x[:3] == (obj, '__TEXT', '__text'))
            offset, loads, halves = 0, [], []
            while len(halves) < len(inputs):
                candidates = decode([text[offset:offset + length].hex() for length in (4, 8)])
                matches = [item for item in candidates if item['kind'] == 'load']
                assert len(matches) == 1, (directory, offset, candidates)
                item = matches[0]
                assert item['replay_equal'] and item['width'] in (1, 2)
                item['offset'] = offset
                loads.append(item)
                halves.extend(range(item['tps_half'], item['tps_half'] + item['width']))
                offset += len(item['hex']) // 2
            assert sorted(halves) == list(range(8, 8 + len(inputs))), (directory, halves)
            witnessed_spill = 0 if not inputs else ((16 + 2 * len(inputs) + 15) // 16) * 16
            assert info['thread_invariant_spill_buffer_bytes'] == witnessed_spill
            captures.append({'directory': str(directory), 'native_sha256': info['native_sha256'],
                             'source_sha256': sha(directory / 'input.metal'),
                             'inputs': inputs, 'entry_loads': loads,
                             'spill_bytes': witnessed_spill,
                             'ipr_bytes': info['thread_invariant_largest_ipr_bytes']})
    result = {
        'scope': 'Host macOS 27 G17P native encoder and offline compiler evidence; no GPU or Neo execution.',
        'native_driver_sha256': oracle.native.sha256,
        'native_compiler_sha256': schemas['compiler_sha256'],
        'build_command': command,
        'source_sha256': {str(p): sha(p) for p in sources + headers + [Path(__file__),
            HERE / 'native-cprog-input-oracle.py', HERE / 'native-ioto-copy-oracle.py']},
        'mesa_head': subprocess.check_output(['git', 'rev-parse', 'HEAD'], cwd=mesa, text=True).strip(),
        'copy_cases': copies, 'captures': captures,
        'boundaries': ['COPY encoder and typed TPS roundtrip only; not hardware semantics qualification.',
                       'Storage formula is witnessed for these captures, not assumed for arbitrary spills.',
                       'No claim that direct uniform reads are valid or invalid during a Mesa preamble.',
                       'Optional entry prelude, stage flags, and runtime ordering remain unqualified.'],
    }
    args.output.write_text(json.dumps(result, indent=2) + '\n')
    print(json.dumps({'receipt': str(args.output), 'copy_cases': len(copies),
                      'captures': len(captures),
                      'entry_reloads': sum(len(x['entry_loads']) for x in captures),
                      'spill_sizes': sorted({x['spill_bytes'] for x in captures})}))


def decode(records):
    output = subprocess.check_output([str(HERE / 'decode-static-spill'), *records], text=True)
    return [json.loads(line) for line in output.splitlines()]


if __name__ == '__main__':
    main()
