#!/usr/bin/env python3
"""Offline Apple compiler controls for CTS texture failures; executes no GPU."""
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
OUT = Path(sys.argv[1]) if len(sys.argv) > 1 else ROOT / 'native-quick-textures'
OUT.mkdir(exist_ok=False)

HEADER = '''#include <metal_stdlib>
using namespace metal;
struct V { float4 position [[position]]; float4 color; };
'''
PLAIN_VS = '''vertex V vertex_main(uint id [[vertex_id]]) {
    return {float4(float2((id << 1) & 2, id & 2) * 2.0 - 1.0, 0.0, 1.0), float4(1)};
}
'''
PLAIN_FS = 'fragment float4 fragment_main(V v [[stage_in]]) { return v.color; }\n'
variants = []
for label, lod in [('constant', '2.0f'), ('uniform', 'value'),
                   ('per-vertex', 'float(id & 3u)')]:
    variants.append(('vertex-colour-lod-' + label, HEADER + '''
vertex V vertex_main(uint id [[vertex_id]], constant float &value [[buffer(0)]],
                     texture2d<float> t [[texture(0)]], sampler s [[sampler(0)]]) {
    float2 uv = float2((id << 1) & 2, id & 2);
    return {float4(uv * 2.0 - 1.0, 0.0, 1.0), t.sample(s, uv, level(''' + lod + '''))};
}
''' + PLAIN_FS))
for dim, coord in [('2d_array', 'float2(0.3, 0.7), uint(p.x) & 1u'),
                   ('cube', 'float3(0.3, 0.7, 1.0)'),
                   ('cube_array', 'float3(0.3, 0.7, 1.0), uint(p.x) & 1u')]:
    for label, lod in [('implicit', ''), ('level', ', level(value)'),
                       ('bias', ', bias(value)')]:
        variants.append((dim + '-' + label, HEADER + PLAIN_VS + '''
fragment float4 fragment_main(float4 p [[position]],
                             constant float &value [[buffer(0)]],
                             depth''' + dim + '''<float> t [[texture(0)]],
                             sampler s [[sampler(0)]]) {
    return float4(t.sample_compare(s, ''' + coord + ', 0.5f' + lod + '''));
}
'''))
variants.append(('2d-three-references', HEADER + PLAIN_VS + '''
fragment float4 fragment_main(V in [[stage_in]], depth2d<float> t [[texture(0)]],
                             sampler s [[sampler(0)]]) {
    float2 uv = in.color.xy;
    return float4(t.sample_compare(s, uv, 0.3f), t.sample_compare(s, uv, 0.5f),
                  t.sample_compare(s, uv, 0.8f), 1.0f);
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
        result = subprocess.run(command, capture_output=True, timeout=60)
        (directory / f'command-{i}.log').write_bytes(result.stdout + result.stderr)
        if result.returncode:
            receipt['failed_command'] = i
            receipt['exit_code'] = result.returncode
            break
    else:
        raw = (directory / 'output.nat').read_bytes()
        receipt['native_sha256'] = native.sha(raw)
        receipt['sections'] = native.extract_sections(directory, raw)
        result = subprocess.run(['xcrun', 'metal-objdump', '--disassemble', str(directory / 'output.nat')],
                                capture_output=True, timeout=60)
        (directory / 'disassembly.log').write_bytes(result.stdout + result.stderr)
        receipt['disassembly_exit'] = result.returncode
    (directory / 'receipt.json').write_text(json.dumps(receipt, indent=2) + '\n')
    summary.append(receipt)
    print(name, 'compiled' if 'native_sha256' in receipt else 'refused', flush=True)
(OUT / 'summary.json').write_text(json.dumps(summary, indent=2) + '\n')
