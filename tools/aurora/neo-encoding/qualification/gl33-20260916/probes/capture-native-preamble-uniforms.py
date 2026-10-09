#!/usr/bin/env python3
"""Capture G17P uniform-chain metadata using the offline native compiler only."""
import importlib.util
import json
from pathlib import Path
import struct
import subprocess
import sys

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[3]
helper = ROOT / 'mesa-j700-preamble-20260915/src/asahi/compiler/test/probes/capture-g17p-mrt-eot.py'
spec = importlib.util.spec_from_file_location('native_sections', helper)
native = importlib.util.module_from_spec(spec)
spec.loader.exec_module(native)


def shader_info(data):
    def u16(offset):
        return struct.unpack_from('<H', data, offset)[0]
    def u32(offset):
        return struct.unpack_from('<I', data, offset)[0]
    def field(table, index):
        vtable = table - struct.unpack_from('<i', data, table)[0]
        if 4 + index * 2 >= u16(vtable):
            return None
        offset = u16(vtable + 4 + index * 2)
        return table + offset if offset else None
    root = u32(0)
    pointer = field(root, 0)
    assert pointer is not None
    info = pointer + u32(pointer)
    # These scalar fields are read with ldr wN by the pinned native driver.
    return {str(index): u32(offset) if (offset := field(info, index)) is not None else 0
            for index in (0, 14, 37, 38, 41, 42)}


def capture(output, words):
    directory = output / f'vertex-{words}'
    directory.mkdir()
    source = '''#include <metal_stdlib>
using namespace metal;
struct V { float4 position [[position]]; uint hash [[flat]]; };
vertex V vertex_main(uint id [[vertex_id]], constant uint *data [[buffer(0)]]) {
    uint h = 0x13579bdfu;
'''
    for i in range(words):
        source += f'    h = (h * 33u) ^ data[{i}];\n'
    source += '''    float2 uv = float2((id << 1) & 2, id & 2);
    return {float4(uv * 2.0 - 1.0, 0.0, 1.0), h};
}
fragment float4 fragment_main(V v [[stage_in]]) {
    return float4(v.hash & 255u, (v.hash >> 8) & 255u,
                  (v.hash >> 16) & 255u, 255u) / 255.0;
}
'''
    (directory/'input.metal').write_text(source)
    pipeline = {'pipelines': {'render_pipelines': [{
        'vertex_function': 'vertex_main', 'fragment_function': 'fragment_main',
        'color_attachments': [{'pixel_format': 'RGBA8Unorm'}]}]}}
    (directory/'pipeline.mtlp-json').write_text(json.dumps(pipeline, indent=2))
    commands = [
        ['xcrun', 'metal', '-c', str(directory/'input.metal'), '-o', str(directory/'input.air')],
        ['xcrun', 'metallib', str(directory/'input.air'), '-o', str(directory/'input.metallib')],
        ['xcrun', 'metal-nt', '-arch', 'applegpu_g17p', '-platform_version',
         'macos', '27.0', '27.0', '-N', str(directory/'pipeline.mtlp-json'),
         str(directory/'input.metallib'), '-o', str(directory/'output.nat')],
    ]
    for index, command in enumerate(commands):
        result = subprocess.run(command, capture_output=True, timeout=60)
        (directory/f'command-{index}.log').write_bytes(result.stdout + result.stderr)
        result.check_returncode()
    raw = (directory/'output.nat').read_bytes()
    sections = list(native.sections(raw))
    records = [(offset, data) for offset, segment, name, data in sections
               if segment == '__GPU_METADATA' and name == '__vertex']
    assert len(records) == 1
    object_offset, metadata = records[0]
    text = [data for offset, segment, name, data in sections
            if offset == object_offset and segment == '__TEXT' and name == '__text']
    assert len(text) == 1
    receipt = {'commands': commands, 'hardware_execution': False,
        'words': words, 'source_sha256': native.sha(source.encode()),
        'native_sha256': native.sha(raw), 'sections': native.extract_sections(directory, raw),
        'vertex_text_size': len(text[0]), 'shader_info_scalars': shader_info(metadata)}
    (directory/'receipt.json').write_text(json.dumps(receipt, indent=2) + '\n')
    return {k: receipt[k] for k in ('words', 'vertex_text_size', 'shader_info_scalars')}


if __name__ == '__main__':
    if len(sys.argv) < 2:
        raise SystemExit('usage: capture-native-preamble-uniforms.py NEW_OUTPUT_DIRECTORY [UNIFORM_WORDS ...]')
    words = [int(value) for value in sys.argv[2:]] or [0, 1, 32, 64]
    if any(value < 0 or value > 2048 for value in words) or len(set(words)) != len(words):
        raise SystemExit('uniform word counts must be distinct and in 0..2048')
    output = Path(sys.argv[1]).resolve()
    output.mkdir(parents=True, exist_ok=False)
    results = [capture(output, count) for count in words]
    (output/'summary.json').write_text(json.dumps(results, indent=2) + '\n')
    print(json.dumps(results))
