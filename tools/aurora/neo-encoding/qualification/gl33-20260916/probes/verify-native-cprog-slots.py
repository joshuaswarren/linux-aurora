#!/usr/bin/env python3
"""Compare the production incoming-producer ledger to pinned native RLD runs."""
import argparse
import ctypes
import hashlib
import json
from pathlib import Path
import subprocess

HERE = Path(__file__).resolve().parent


def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--mesa', type=Path, required=True)
    parser.add_argument('--captures', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    mesa = args.mesa.resolve()
    bridge = HERE / 'cprog-slot-bridge.c'
    core = mesa / 'src/asahi/compiler/agx3_slot_core.c'
    header = core.with_suffix('.h')
    library = HERE / 'cprog-slot-bridge.dylib'
    command = ['clang', '-std=c11', '-Wall', '-Wextra', '-Werror',
               '-shared', '-fPIC', '-I' + str(core.parent), str(bridge),
               str(core), '-o', str(library)]
    subprocess.run(command, check=True)
    plan = ctypes.CDLL(str(library)).cprog_slot_plan
    u16 = ctypes.c_uint16
    u8 = ctypes.c_uint8
    plan.argtypes = [ctypes.c_uint, ctypes.POINTER(u16), ctypes.c_uint,
                     ctypes.POINTER(u8), ctypes.POINTER(u16)]
    plan.restype = ctypes.c_int
    source = json.loads(args.captures.read_text())
    results = []
    for capture in source['captures']:
        directory = Path(capture['directory'])
        assert sha(directory / 'output.nat') == capture['native_sha256']
        assert sha(directory / 'input.metal') == capture['source_sha256']
        loads = capture['entry_loads']
        n = len(loads)
        operands = (u16 * max(1, 4 * n))()
        slots, waits = (u8 * max(1, n))(), (u16 * max(1, n))()
        for i, load in enumerate(loads):
            assert load['kind'] == 'load' and load['replay_equal']
            operands[4 * i:4 * i + 4] = [load['tps_half'], load['dest'],
                                        load['width'], load['slot']]
        assert plan(len(capture['inputs']), operands, n, slots, waits) == 1
        actual = [(slots[i], waits[i]) for i in range(n)]
        expected = [(load['slot'], load['wait_mask']) for load in loads]
        assert actual == expected, (directory, actual, expected)
        results.append({'directory': str(directory), 'reloads': n,
                        'slots_and_waits': actual,
                        'native_sha256': capture['native_sha256']})
    result = {
        'scope': 'CPU comparison of computed waits given native result-slot choices; no GPU execution.',
        'mesa_head': subprocess.check_output(['git', 'rev-parse', 'HEAD'], cwd=mesa, text=True).strip(),
        'source_sha256': {str(p): sha(p) for p in [bridge, core, header, Path(__file__)]},
        'capture_receipt_sha256': sha(args.captures),
        'native_compiler_sha256': source['native_compiler_sha256'],
        'command': command, 'captures': results,
        'boundaries': [
            'Result-slot choices are replay inputs, not independently inferred or qualified policy.',
            'Compares computed wait fields, not RLD structural flags or mirror policy.',
            'No compiler lowering or runtime caller enables the new path yet.',
            'Importing a live range does not model external asynchronous source readers.',
        ],
    }
    args.output.write_text(json.dumps(result, indent=2) + '\n')
    print(json.dumps({'receipt': str(args.output), 'captures': len(results),
                      'matched_reloads': sum(x['reloads'] for x in results)}))


if __name__ == '__main__':
    main()
