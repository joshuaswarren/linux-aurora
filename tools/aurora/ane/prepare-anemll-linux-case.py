#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-2.0-only
"""Validate and package the captured ANEMLL stories260K Linux experiment.

This consumes only local native captures and independently generated CPU
references. The package is build-pinned. Expected outputs, embedding lookup
table and input fixtures remain outside the ANE domain; only the live input,
poisoned output, program resources and native packed weights are mapped.
"""
import argparse
import hashlib
import json
import math
from pathlib import Path
import runpy
import struct

BASE = 0x10000000000
LAYOUT = [(0x1d4c000, 664), (0x1d61440, 7552), (0x1d68000, 54276),
          (0x1d50000, 1040), (0x1d54000, 56), (0x1d58000, 40)]
REFS = {'setup-2f': (0x3c8000, 2048), 'work-state': (0x2008000, 16384),
        'input': (0x2010000, 4096), 'output-before': (0x2018000, 32768),
        'output-after': (0x2018000, 32768), 'work-aux': (0x1d7c000, 540672)}


def require(value, message):
    if not value:
        raise ValueError(message)


def payload(item):
    data = bytes.fromhex(item['bytes'])
    require(len(data) == item['length'], 'truncated mapped data')
    return data


def argmax(data, position):
    return max(range(512), key=lambda token: data[token * 32 + position])


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--capture-root', type=Path, required=True)
    parser.add_argument('--reference-root', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    helper = runpy.run_path(str(Path(__file__).with_name('prepare-linux-case.py')))
    full = helper['frames'](args.capture_root / 'direct-full-capture')
    weights = helper['frames'](args.capture_root / 'direct-weights-capture')
    records = [x for x in full if x['sequence'] >= 46]
    commands = helper['requests'](records, 'IO')
    ops = [struct.unpack_from('<I', x, 4)[0] for x in commands]
    require(ops == helper['EXPECTED_OPS'] + [0x203, 0x201, 1, 0xb], 'changed second firmware lifetime')
    require(commands[-1] == struct.pack('<II', 0, 0xb), 'changed post-shutdown command')
    setup_commands, teardown = commands[1:-4], commands[-4:-1]
    data = helper['requests'](records, 'DATA_CHAIN_H2T')
    repeat_data = helper['requests'](weights, 'DATA_CHAIN_H2T')
    require(len(data) == len(repeat_data) == 3, 'unexpected job count')
    require(data[0] == data[2][:8] + struct.pack('<I', 0) == struct.pack('<III', 0, 0x2d, 0), 'bad opening gate')
    require(data[2] == struct.pack('<III', 0, 0x2d, 1), 'bad closing gate')
    require(data[0] == repeat_data[0] and data[2] == repeat_data[2], 'changed repeated gates')
    work = data[1]
    require(len(work) == 0xcd8 and struct.unpack_from('<II', work) == (0, 0x20c), 'wrong work request')
    require(work[:0x20] + work[0x28:] == repeat_data[1][:0x20] + repeat_data[1][0x28:], 'work changed beyond token')
    require(struct.unpack_from('<Q', work, 0x28)[0] == 3, 'wrong tensor count')
    for offset, addr, size in [(0x60, 0x2008000, 0x4000), (0x90, 0x2010000, 0x1000), (0xc0, 0x2018000, 0x8000)]:
        require(struct.unpack_from('<QQQ', work, offset + 16) == (3, BASE + addr, size), 'changed tensor descriptor')
    require(struct.unpack_from('<Q', work, 0xcd0)[0] == BASE + 0x1d7c000, 'changed packed weights pointer')
    resources = [x for f in records for x in f.get('program_buffers', [])]
    repeat_resources = [x for f in weights for x in f.get('program_buffers', [])]
    require([(x['iova'] - BASE, x['length']) for x in resources] == LAYOUT, 'changed resources')
    require([(x['iova'], payload(x)) for x in resources] == [(x['iova'], payload(x)) for x in repeat_resources], 'repeated program mismatch')
    refs = {x['kind']: x for f in records for x in f.get('referenced_buffers', [])}
    repeated = {x['kind']: x for f in weights for x in f.get('referenced_buffers', [])}
    require(set(refs) == set(REFS), 'missing reference buffers')
    require(refs['work-aux']['length'] == 16384 and payload(refs['work-aux']) == payload(repeated['work-aux'])[:16384], 'packed weight prefix changed')
    refs['work-aux'] = repeated['work-aux']
    require(refs['work-aux'].get('descriptor_word') == 0x4000 and refs['work-aux'].get('extent_basis'), 'missing mapped-extent evidence')
    for name, (offset, size) in REFS.items():
        require((refs[name]['iova'], len(payload(refs[name]))) == (BASE + offset, size), 'changed ' + name)
        if name in repeated:
            require(payload(refs[name]) == payload(repeated[name]), 'repeated buffer changed: ' + name)
    require(payload(refs['setup-2f']) == bytes(8) + bytes([255]) * 16 + bytes(2048 - 24), 'changed setup mask')
    require(not any(payload(refs['work-state'])), 'nonzero initial work state')
    require(payload(refs['output-before']) == struct.pack('<H', 0x7e00) * 16384, 'native output not poisoned')
    root = args.reference_root
    require(payload(refs['input']) == (root / 'prepared-stateless/stateless-input.bin').read_bytes(), 'native model input mismatch')
    require(payload(refs['output-after']) == (root / 'neo-direct-output.bin').read_bytes(), 'native model output mismatch')
    manifest = json.loads((root / 'series/manifest.json').read_text())
    require(manifest['prompt_tokens'] == [1, 403, 407, 261, 378], 'changed initial prompt')
    require([s['next_token'] for s in manifest['samples']] == [432, 383, 286], 'changed reference continuation')
    embeddings = (root / 'series/embeddings.bin').read_bytes()
    require(len(embeddings) == 512 * 64 * 2, 'wrong embedding table')
    samples, comparisons = [], []
    tokens = manifest['prompt_tokens'].copy()
    for i, sample in enumerate(manifest['samples']):
        require(sample['context_tokens'] == tokens, 'reference is not autoregressive')
        inp = b''.join(embeddings[((tokens[t] if t < len(tokens) else 0) * 64 + c) * 2:((tokens[t] if t < len(tokens) else 0) * 64 + c) * 2 + 2]
                       for c in range(64) for t in range(32))
        require(inp == (root / f'series/{i}.bin').read_bytes(), 'embedding input mismatch')
        native = (root / f'series-output-{i}.bin').read_bytes()
        cpu = (root / f'series/{i}-cpu-logits.bin').read_bytes()
        require(len(native) == 32768 and len(cpu) == 65536, 'incorrect logits shape')
        nv, cv = struct.unpack('<16384e', native), struct.unpack('<16384f', cpu)
        require(all(map(math.isfinite, nv)) and all(map(math.isfinite, cv)), 'nonfinite logits')
        error = max(abs(x-y) for x, y in zip(nv, cv))
        require(error < 0.3, 'native numerical error exceeds qualified FP16 bound')
        require(all(argmax(nv, p) == argmax(cv, p) for p in range(len(tokens))), 'valid-position token mismatch')
        next_token = argmax(nv, len(tokens) - 1)
        require(next_token == sample['next_token'], 'wrong continuation token')
        tokens.append(next_token)
        samples.append((inp, native))
        comparisons.append(dict(sample=i, max_absolute_logit_error=error,
                                valid_positions_matched=len(tokens)-1, next_token=next_token,
                                native_output_sha256=hashlib.sha256(native).hexdigest()))
    # Require the exact nine observed event forms, including stage-3 handles.
    events = {}
    for frame in helper['frames'](args.capture_root / 'series-metadata'):
        for x in frame['payloads']:
            b = bytes.fromhex(x['bytes'])
            if x['channel'] == 'IO_T2H' and len(b) == 96:
                events.setdefault(x['slot'], b)
                require(events[x['slot']] == b, 'event slot overwritten')
    require(sorted(events) == list(range(9)), 'missing native three-job events')
    native_tokens = []
    for i in range(3):
        token = struct.unpack_from('<Q', events[3*i], 8)[0]
        native_tokens.append(token)
        for k, (handle, stage) in enumerate([(0xfe00 + (i % 2)*2, 0), (i*0x10001, 3), (0xfe01 + (i % 2)*2, 1)]):
            b = events[3*i+k]
            require(struct.unpack_from('<IHHQ', b) == (handle, 0x300, 0, token), 'changed native event header')
            require(struct.unpack_from('<QIQ', b, 0x10)[:2] == (0, 0), 'nonzero event reserved fields')
            require(struct.unpack_from('<I', b, 0x1c)[0] == stage and struct.unpack_from('<Q', b, 0x20)[0] == 0, 'bad native event phase')
            if stage == 3:
                require(struct.unpack_from('<Q', b, 0x30)[0] == 0xff00, 'changed native profile event')
    require(len(set(native_tokens)) == 3, 'reused work tokens')
    package = bytearray()
    def append(data):
        package.extend(bytes((-len(package)) % 8))
        offset = len(package)
        package.extend(data)
        return offset, len(data)
    buffer_meta = [(x['iova'], *append(payload(x))) for x in resources + [refs['setup-2f'], refs['work-state'], refs['work-aux']]]
    command_meta = [append(b) for b in setup_commands]
    teardown_meta = [append(b) for b in teardown]
    work_meta = append(work)
    embedding_meta = append(embeddings)
    sample_meta = [(append(a)[0], append(b)[0]) for a, b in samples]
    digest = hashlib.sha256(package).hexdigest()
    filename = 'j700-anemll-tiny.case'
    header = '/* Generated from validated ANEMLL stories260K native captures. */\n'
    defines = {'NAME': f'"apple/ane/{filename}"', 'ACTION': '"anemll-tiny"', 'ANEMLL': '1',
               'SIZE': f'{len(package)}U', 'INPUT': f'{refs["input"]["iova"]:#x}ULL',
               'OUTPUT': f'{refs["output-before"]["iova"]:#x}ULL',
               'INPUT_BYTES': '4096U', 'OUTPUT_BYTES': '32768U', 'WORK_BYTES': '3288U',
               'EMBEDDINGS': f'{embedding_meta[0]}U'}
    header += ''.join(f'#define ANE_CASE_{name} {value}\n' for name, value in defines.items())
    header += 'static const u8 ane_case_sha[32] = {' + ','.join(str(x) for x in bytes.fromhex(digest)) + '};\n'
    header += 'static const u32 ane_case_next_tokens[] = {432,383,286};\n'
    header += 'static const struct { u64 iova; u32 offset, size; } ane_case_buffers[] = {\n'
    header += ''.join(f'{{{address:#x}ULL,{offset}U,{size}U}},\n' for address, offset, size in buffer_meta) + '};\n'
    for name, meta in [('commands', command_meta), ('teardown', teardown_meta), ('work', [work_meta])]:
        header += f'static const struct {{ u32 offset, size; }} ane_case_{name}[] = {{\n'
        header += ''.join(f'{{{offset}U,{size}U}},\n' for offset, size in meta) + '};\n'
    header += 'static const struct { u32 input, expected; } ane_case_samples[] = {\n'
    header += ''.join(f'{{{a}U,{b}U}},\n' for a, b in sample_meta) + '};\n'
    out = args.output
    out.mkdir(mode=0o700, parents=True, exist_ok=False)
    (out / filename).write_bytes(package)
    (out / 'apple-ane-case-data.h').write_text(header)
    receipt = dict(file=filename, sha256=digest, bytes=len(package), native_output_validated=True,
                   linux_computation_tested=False, samples=comparisons, native_event_tokens=native_tokens,
                   prompt_tokens=manifest['prompt_tokens'], generated_tokens=tokens, text=manifest['text'],
                   packed_weight_bytes=refs['work-aux']['length'], packed_weight_extent_basis=refs['work-aux']['extent_basis'])
    (out / 'manifest.json').write_text(json.dumps(receipt, indent=2) + '\n')
    print('ANEMLL_LINUX_CASE_PREPARED', digest, len(package), 'bytes; native tokens', tokens[-3:])


if __name__ == '__main__':
    main()
