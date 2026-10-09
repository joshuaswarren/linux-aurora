#!/usr/bin/env python3
"""Check the production input layout against digest-checked native captures."""
import argparse
import ctypes
import hashlib
import importlib.util
import json
from pathlib import Path
import subprocess

HERE = Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location('decode_capture', HERE / 'decode-native-preamble-captures.py')
native = importlib.util.module_from_spec(spec)
spec.loader.exec_module(native)

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--mesa', type=Path, required=True)
parser.add_argument('--output', type=Path, required=True)
args = parser.parse_args()
mesa = args.mesa.resolve()
sources = [mesa / 'src/asahi/lib' / name for name in ('agx_ioto_core.c', 'agx_ioto_load_core.c')]
sources.append(HERE / 'cprog-layout-bridge.c')
library_path = HERE / 'cprog-layout-bridge.dylib'
command = ['clang', '-std=c11', '-Wall', '-Wextra', '-Werror', '-dynamiclib',
           '-fsanitize=undefined', '-fno-sanitize-recover=all',
           '-I' + str(mesa / 'include'), '-I' + str(mesa / 'src'),
           *map(str, sources), '-o', str(library_path)]
subprocess.run(command, check=True)
library = ctypes.CDLL(str(library_path))
layout = library.cprog_layout_bridge
u16p = ctypes.POINTER(ctypes.c_uint16)
layout.argtypes = [ctypes.POINTER(ctypes.c_uint64), u16p, u16p, u16p]
layout.restype = ctypes.c_int
schemas = native.schema.inspect(native.schema.default_compiler())
results = []
for folder in ('native-preamble-uniforms-host27', 'native-preamble-sampling-host27',
               'native-preamble-pressure-host27', 'native-preamble-buffers-host27'):
    for directory in sorted((HERE.parent / folder).glob('vertex-*')):
        capture = native.decode(directory)
        assert len(capture['constant_phases']) == 1
        indices = capture['constant_phases'][0]['cprog_uniform_inputs']
        assert indices == sorted(set(indices)) and all(0 <= i < 512 for i in indices)
        words = (ctypes.c_uint64 * 8)()
        for h in indices:
            words[h // 64] |= 1 << (h % 64)
        count, frame = ctypes.c_uint16(), ctypes.c_uint16()
        offsets = (ctypes.c_uint16 * 512)()
        assert layout(words, ctypes.byref(count), ctypes.byref(frame), offsets) == 1
        assert count.value == len(indices)
        assert frame.value == capture['thread_invariant_spill_buffer_bytes']
        expected = {h: 2 * i for i, h in enumerate(indices)}
        assert all(offsets[h] == expected.get(h, 0xffff) for h in range(512))
        results.append({'directory': str(directory), 'native_sha256': capture['native_sha256'],
                        'source_indices': indices, 'input_halfs': count.value,
                        'frame_bytes': frame.value, 'payload_offsets': expected})
hashes = {str(p): hashlib.sha256(p.read_bytes()).hexdigest() for p in sources +
          [Path(__file__), mesa / 'src/asahi/lib/agx_ioto_load_core.h']}
args.output.write_text(json.dumps({
    'scope': 'CPU layout and native metadata comparison; no GPU execution.',
    'native_compiler_sha256': schemas['compiler_sha256'], 'source_hashes': hashes,
    'build_command': command, 'captures': results,
}, indent=2) + '\n')
print(json.dumps({'native_captures_matched': len(results), 'receipt': str(args.output)}))
