#!/usr/bin/env python3
"""Offline G17P constant-program ABI controls with multiple buffer pointers."""
import argparse
import importlib.util
import json
import os
from pathlib import Path
import subprocess

HERE = Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location('capture', HERE / 'capture-native-preamble-uniforms.py')
capture = importlib.util.module_from_spec(spec)
spec.loader.exec_module(capture)


def run(output, count, native_env=None, native_options=()):
    directory = output / f'vertex-buffers-{count}'
    directory.mkdir()
    arguments = ''.join(f', constant uint *data{i} [[buffer({i})]]' for i in range(count))
    source = '''#include <metal_stdlib>
using namespace metal;
struct V { float4 position [[position]]; uint hash [[flat]]; };
''' + f'vertex V vertex_main(uint id [[vertex_id]]{arguments}) {{\n'
    source += '    uint h = 0x13579bdfu;\n'
    source += ''.join(f'    h = (h * 33u) ^ data{i}[0];\n' for i in range(count))
    source += '''    float2 uv = float2((id << 1) & 2, id & 2);
    return {float4(uv * 2.0 - 1.0, 0.0, 1.0), h};
}
fragment float4 fragment_main(V v [[stage_in]]) {
    return float4(v.hash & 255u, (v.hash >> 8) & 255u,
                  (v.hash >> 16) & 255u, 255u) / 255.0;
}
'''
    (directory / 'input.metal').write_text(source)
    pipeline = {'pipelines': {'render_pipelines': [{
        'vertex_function': 'vertex_main', 'fragment_function': 'fragment_main',
        'color_attachments': [{'pixel_format': 'RGBA8Unorm'}]}]}}
    (directory / 'pipeline.mtlp-json').write_text(json.dumps(pipeline, indent=2))
    commands = [
        ['xcrun', 'metal', '-c', str(directory / 'input.metal'), '-o', str(directory / 'input.air')],
        ['xcrun', 'metallib', str(directory / 'input.air'), '-o', str(directory / 'input.metallib')],
        ['xcrun', 'metal-nt', '-arch', 'applegpu_g17p', '-platform_version',
         'macos', '27.0', '27.0', '-N', str(directory / 'pipeline.mtlp-json'),
         str(directory / 'input.metallib'), '-o', str(directory / 'output.nat')],
    ]
    for option in native_options:
        commands[2].extend(['-mllvm', option])
    for index, command in enumerate(commands):
        env = {**os.environ, **(native_env or {})} if index == 2 else None
        result = subprocess.run(command, capture_output=True, timeout=60, env=env)
        (directory / f'command-{index}.log').write_bytes(result.stdout + result.stderr)
        result.check_returncode()
    raw = (directory / 'output.nat').read_bytes()
    sections = list(capture.native.sections(raw))
    metadata = [(offset, data) for offset, segment, name, data in sections
                if segment == '__GPU_METADATA' and name == '__vertex']
    assert len(metadata) == 1
    object_offset, data = metadata[0]
    text = [data for offset, segment, name, data in sections
            if offset == object_offset and segment == '__TEXT' and name == '__text']
    assert len(text) == 1
    receipt = {'commands': commands, 'hardware_execution': False,
               'native_env_overrides': native_env or {},
               'native_options': list(native_options),
               'workload': f'{count} separate constant-buffer pointers',
               'buffers': count, 'source_sha256': capture.native.sha(source.encode()),
               'native_sha256': capture.native.sha(raw),
               'sections': capture.native.extract_sections(directory, raw),
               'vertex_text_size': len(text[0]),
               'shader_info_scalars': capture.shader_info(data)}
    (directory / 'receipt.json').write_text(json.dumps(receipt, indent=2) + '\n')
    return {'buffers': count, 'vertex_text_size': len(text[0])}


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('directory', type=Path)
    parser.add_argument('--counts', nargs='+', type=int,
                        default=[0, 1, 2, 3, 4, 8, 16])
    parser.add_argument('--native-env', action='append', default=[],
                        choices=['AGC_ENABLE_CPROG_GPR_GB=0', 'AGC_ENABLE_CPROG_GPR_GB=1',
                                 'AGC_ENABLE_YIELD_INSERTION=0', 'AGC_ENABLE_YIELD_INSERTION=1'])
    parser.add_argument('--native-option', action='append', default=[],
                        choices=['-cprog-can-read-from-uniforms=true',
                                 '-cprog-can-read-from-uniforms=false',
                                 '-print-before=agx3-sched-hints',
                                 '-print-after=agx3-sched-hints'])
    args = parser.parse_args()
    if len(set(args.counts)) != len(args.counts) or any(x < 0 or x > 16 for x in args.counts):
        parser.error('counts must be distinct values in 0..16')
    output = args.directory.resolve()
    output.mkdir(parents=True, exist_ok=False)
    native_env = dict(item.split('=', 1) for item in args.native_env)
    if len(native_env) != len(args.native_env):
        parser.error('native environment override keys must be unique')
    results = [run(output, count, native_env, args.native_option) for count in args.counts]
    (output / 'summary.json').write_text(json.dumps(results, indent=2) + '\n')
    print(json.dumps(results))
