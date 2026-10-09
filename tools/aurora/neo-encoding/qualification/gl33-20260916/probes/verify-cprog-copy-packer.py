#!/usr/bin/env python3
"""Compare the production G17P TPS-input packer with Apple's encoder on CPU."""
import argparse
import ctypes
import hashlib
import importlib.util
import json
from pathlib import Path
import subprocess

HERE = Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location('native_cprog', HERE / 'native-cprog-input-oracle.py')
native = importlib.util.module_from_spec(spec)
spec.loader.exec_module(native)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--mesa', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    mesa = args.mesa.resolve()
    sources = [mesa / 'src/asahi/compiler' / name for name in (
        'agx3_ioto_load_packer.c', 'agx3_mem_core.c', 'agx3_static_spill_core.c',
        'agx3_binder.c', 'agx3_compose/agx3_compose.c',
        'agx3_compose/agx3b_tables.c', 'agx3_compose/agx3c_tables.c',
        '../lib/agx_ioto_core.c', '../lib/agx_ioto_load_core.c')]
    flags = ['clang', '-std=c11', '-Wall', '-Wextra', '-Werror',
             '-fsanitize=undefined', '-fno-sanitize-recover=all',
             '-I' + str(mesa / 'include'), '-I' + str(mesa / 'src'),
             '-I' + str(mesa / 'src/asahi/compiler')]
    unit = HERE / 'cprog-copy-unit-test'
    unit_source = mesa / 'src/asahi/compiler/test/test-agx3-ioto-load-packer.c'
    commands = [flags + list(map(str, sources)) + [str(unit_source), '-o', str(unit)]]
    subprocess.run(commands[-1], check=True)
    unit_result = subprocess.check_output([str(unit)], text=True).strip()
    print(unit_result, flush=True)
    dylib = HERE / 'cprog-input-bridge.dylib'
    commands.append(flags + ['-dynamiclib'] + list(map(str, sources)) +
                    [str(HERE / 'cprog-input-bridge.c'), '-o', str(dylib)])
    subprocess.run(commands[-1], check=True)
    library = ctypes.CDLL(str(dylib))
    pack = library.cprog_input_bridge
    u8p = ctypes.POINTER(ctypes.c_uint8)
    pack.argtypes = [ctypes.c_uint16, ctypes.c_uint16, ctypes.c_uint8, ctypes.c_int8,
                     u8p, u8p, u8p]
    pack.restype = ctypes.c_int
    oracle = native.CprogOracle()
    cases = []
    for destination in (0, 2, 4, 6, 8, 14, 16, 126, 128, 254, 256, 510, 512,
                        1022, 1024, 65532, 65534):
        for source in (0, 2, 10, 126, 128, 190, 252, 254):
            for mask in (1, 2, 3):
                for wait in (-1, 0, 1, 2, 3, 4, 5):
                    observed = oracle.copy_inputs(destination, mask, source_half=source,
                                                  source_value=8 if wait < 0 else wait)
                    buf = (ctypes.c_uint8 * 20)()
                    length, written = ctypes.c_uint8(), ctypes.c_uint8()
                    assert pack(destination, source, mask, wait, buf,
                                ctypes.byref(length), ctypes.byref(written)) == 1
                    encoded = bytes(buf[:length.value]).hex()
                    assert encoded == ''.join(observed['records']), (observed, encoded)
                    assert written.value == observed['returned']
                    cases.append({'destination_bytes': destination, 'source_half': source,
                                  'mask': mask, 'wait_slot': wait, 'hex': encoded,
                                  'written_bytes': written.value})
    hashes = {str(p.resolve()): hashlib.sha256(p.read_bytes()).hexdigest()
              for p in sources + [unit_source, Path(__file__), HERE / 'cprog-input-bridge.c',
                                  HERE / 'native-cprog-input-oracle.py',
                                  HERE / 'native-ioto-copy-oracle.py',
                                  mesa / 'src/asahi/compiler/agx3_ioto_load_packer.h']}
    args.output.write_text(json.dumps({
        'scope': 'CPU-only exact-byte and returned-byte-count comparison, not GPU execution.',
        'native_sha256': oracle.native.sha256, 'source_hashes': hashes,
        'build_commands': commands, 'unit_result': unit_result, 'cases': cases,
    }, indent=2) + '\n')
    print(json.dumps({'matched_native_cases': len(cases), 'receipt': str(args.output)}))


if __name__ == '__main__':
    main()
