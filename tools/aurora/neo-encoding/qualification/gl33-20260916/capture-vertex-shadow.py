#!/usr/bin/env python3
"""Offline Apple compiler controls for vertex-stage shadow sampling
(KHR-GL30.ext_texture_shadow_lod *_vertex cases); executes no GPU."""
import importlib.util
import json
from pathlib import Path
import subprocess
import sys

ROOT = Path(__file__).resolve().parent
HELPER = ROOT.parents[2] / 'mesa-j700/src/asahi/compiler/test/probes/capture-g17p-mrt-eot.py'
spec = importlib.util.spec_from_file_location('native_sections', HELPER)
native = importlib.util.module_from_spec(spec)
spec.loader.exec_module(native)
OUT = Path(sys.argv[1]) if len(sys.argv) > 1 else ROOT / 'native-vertex-shadow'
OUT.mkdir(exist_ok=False)

HEADER = '''#include <metal_stdlib>
using namespace metal;
struct V { float4 position [[position]]; float4 color; };
fragment float4 fragment_main(V v [[stage_in]]) { return v.color; }
'''
variants = []
for dim, coord in [('2d_array', 'c[1].xy, uint(c[1].z)'),
                   ('cube', 'c[1].xyz'),
                   ('cube_array', 'c[1].xyz, uint(c[1].w)')]:
    for label, lod in [('level0', 'level(0.0)'), ('level-uniform', 'level(c[2].x)'),
                       ('level-varying', 'level(c[2].x * float(id))')]:
        variants.append((dim + '-' + label, HEADER + '''
vertex V vertex_main(uint id [[vertex_id]], constant float4 *c [[buffer(0)]],
                     depth''' + dim + '''<float> t [[texture(0)]], sampler s [[sampler(0)]]) {
    float2 uv = float2((id << 1) & 2, id & 2);
    return {float4(uv * 2.0 - 1.0, 0.0, 1.0),
            float4(t.sample_compare(s, ''' + coord + ''', c[2].y, ''' + lod + '''))};
}
'''))
summary = []
for name, source in variants:
    if len(sys.argv) > 2 and name not in sys.argv[2:]:
        continue
    directory = OUT / name
    directory.mkdir()
    (directory / 'input.metal').write_text(source)
    pipeline = {'pipelines': {'render_pipelines': [{
        'vertex_function': 'vertex_main', 'fragment_function': 'fragment_main',
        'color_attachments': [{'pixel_format': 'RGBA8Unorm'}],
    }]}}
    (directory / 'pipeline.mtlp-json').write_text(json.dumps(pipeline, indent=2))
    commands = [
        ['xcrun', 'metal', '-c', str(directory / 'input.metal'), '-o', str(directory / 'input.air')],
        ['xcrun', 'metallib', str(directory / 'input.air'), '-o', str(directory / 'input.metallib')],
        ['xcrun', 'metal-nt', '-arch', 'applegpu_g17p', '-platform_version', 'macos', '27.0', '27.0',
         '-N', str(directory / 'pipeline.mtlp-json'), str(directory / 'input.metallib'),
         '-o', str(directory / 'output.nat')],
    ]
    receipt = {'name': name, 'commands': commands, 'hardware_execution': False,
               'source_sha256': native.sha(source.encode())}
    for i, command in enumerate(commands):
        result = subprocess.run(command, capture_output=True, timeout=120)
        (directory / f'command-{i}.log').write_bytes(result.stdout + result.stderr)
        if result.returncode:
            receipt['failed_command'] = i
            receipt['exit_code'] = result.returncode
            break
    else:
        raw = (directory / 'output.nat').read_bytes()
        receipt['native_sha256'] = native.sha(raw)
        receipt['sections'] = native.extract_sections(directory, raw)
    (directory / 'receipt.json').write_text(json.dumps(receipt, indent=2) + '\n')
    summary.append(receipt)
    print(name, 'compiled' if 'native_sha256' in receipt else 'refused', flush=True)
(OUT / 'summary.json').write_text(json.dumps(summary, indent=2) + '\n')
