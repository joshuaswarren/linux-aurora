#!/usr/bin/env python3
"""Compile native G17P uniform/varying texture controls without executing a GPU."""
import argparse
import importlib.util
import json
from pathlib import Path
import subprocess

HERE = Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location(
    'capture', HERE / 'capture-native-preamble-uniforms.py')
capture = importlib.util.module_from_spec(spec)
spec.loader.exec_module(capture)


def run(output, dimension, varying):
    label = f'vertex-{dimension}-' + ('varying' if varying else 'uniform')
    directory = output / label
    directory.mkdir()
    ty, coord = ('texture2d<float>', 'c.xy') if dimension == '2d' else ('texturecube<float>', 'c.xyz')
    if varying:
        coord += ' + float(id) * 0.01f'
    source = '''#include <metal_stdlib>
using namespace metal;
struct V { float4 position [[position]]; float4 colour [[flat]]; };
vertex V vertex_main(uint id [[vertex_id]], constant float4 *data [[buffer(0)]],
    TYPE tex [[texture(0)]], sampler smp [[sampler(0)]]) {
    float4 c = data[0];
    float4 colour = tex.sample(smp, COORD, level(c.w));
    float2 uv = float2((id << 1) & 2, id & 2);
    return {float4(uv * 2.0 - 1.0, 0.0, 1.0), colour};
}
fragment float4 fragment_main(V v [[stage_in]]) { return v.colour; }
'''.replace('TYPE', ty).replace('COORD', coord)
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
    (directory / 'commands.json').write_text(json.dumps(commands, indent=2) + '\n')
    for index, command in enumerate(commands):
        result = subprocess.run(command, capture_output=True, timeout=60)
        (directory / f'command-{index}.log').write_bytes(result.stdout + result.stderr)
        result.check_returncode()
    raw = (directory / 'output.nat').read_bytes()
    sections = list(capture.native.sections(raw))
    metadata = [x for x in sections if x[1:3] == ('__GPU_METADATA', '__vertex')]
    assert len(metadata) == 1
    obj, _, _, data = metadata[0]
    code = [x[3] for x in sections if x[0] == obj and x[1:3] == ('__TEXT', '__text')]
    assert len(code) == 1
    receipt = {
        'commands': commands, 'hardware_execution': False, 'workload': label,
        'source_sha256': capture.native.sha(source.encode()),
        'native_sha256': capture.native.sha(raw),
        'sections': capture.native.extract_sections(directory, raw),
        'vertex_text_size': len(code[0]),
        'shader_info_scalars': capture.shader_info(data),
    }
    (directory / 'receipt.json').write_text(json.dumps(receipt, indent=2) + '\n')
    return {'workload': label, 'shader_info_scalars': receipt['shader_info_scalars']}


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('directory', type=Path)
    args = parser.parse_args()
    output = args.directory.resolve()
    output.mkdir(parents=True, exist_ok=False)
    results = [run(output, '2d', False), run(output, '2d', True), run(output, 'cube', False)]
    (output / 'summary.json').write_text(json.dumps(results, indent=2) + '\n')
    print(json.dumps(results))
