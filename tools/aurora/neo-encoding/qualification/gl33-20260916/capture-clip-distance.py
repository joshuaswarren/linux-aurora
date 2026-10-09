#!/usr/bin/env python3
"""Offline Apple compiler controls for vertex programs that write clip
distances (KHR-GL30.clip_distance.*); executes no GPU."""
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
OUT = Path(sys.argv[1]) if len(sys.argv) > 1 else ROOT / 'native-clip-distance'
OUT.mkdir(exist_ok=False)

HEADER = '''#include <metal_stdlib>
using namespace metal;
'''
variants = []
for count in (1, 2, 3, 4, 5, 6, 8):
    variants.append((f'position-clip{count}', HEADER + f'''
struct V {{ float4 position [[position]]; float clip [[clip_distance]] [{count}]; }};
vertex V vertex_main(uint id [[vertex_id]], constant float4 *c [[buffer(0)]]) {{
    V v;
    v.position = c[id];
    for (uint i = 0; i < {count}; ++i)
        v.clip[i] = c[id + 1][i & 3];
    return v;
}}
fragment float4 fragment_main() {{ return float4(1.0); }}
'''))
variants.append(('position-varying-clip2', HEADER + '''
struct V { float4 position [[position]]; float value; float clip [[clip_distance]] [2]; };
struct F { float4 position [[position]]; float value; };
vertex V vertex_main(uint id [[vertex_id]], constant float4 *c [[buffer(0)]]) {
    V v;
    v.position = c[id];
    v.value = c[id + 1].x;
    v.clip[0] = c[id + 1].y;
    v.clip[1] = c[id + 1].z;
    return v;
}
fragment float4 fragment_main(F f [[stage_in]]) { return float4(f.value); }
'''))
variants.append(('position-colour-clip2', HEADER + '''
struct V { float4 position [[position]]; float4 colour; float clip [[clip_distance]] [2]; };
struct F { float4 position [[position]]; float4 colour; };
vertex V vertex_main(uint id [[vertex_id]], constant float4 *c [[buffer(0)]]) {
    V v;
    v.position = c[id];
    v.colour = c[id + 1];
    v.clip[0] = c[id + 2].x;
    v.clip[1] = c[id + 2].y;
    return v;
}
fragment float4 fragment_main(F f [[stage_in]]) { return f.colour; }
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
        result = subprocess.run(['xcrun', 'metal-objdump', '--disassemble', str(directory / 'output.nat')],
                                capture_output=True, timeout=60)
        (directory / 'disassembly.log').write_bytes(result.stdout + result.stderr)
        receipt['disassembly_exit'] = result.returncode
    (directory / 'receipt.json').write_text(json.dumps(receipt, indent=2) + '\n')
    summary.append(receipt)
    print(name, 'compiled' if 'native_sha256' in receipt else 'refused', flush=True)
(OUT / 'summary.json').write_text(json.dumps(summary, indent=2) + '\n')
