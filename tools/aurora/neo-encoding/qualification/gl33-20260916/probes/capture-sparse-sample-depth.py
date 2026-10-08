#!/usr/bin/env python3
"""Compile masked 4x depth/colour witnesses with Apple; no GPU execution."""
import hashlib
import importlib.util
import json
from pathlib import Path
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[4]
PARSER = ROOT / 'mesa-j700-agx-sync-20260916/src/asahi/compiler/test/probes/capture-g17p-mrt-eot.py'
spec = importlib.util.spec_from_file_location('native', PARSER)
native = importlib.util.module_from_spec(spec)
spec.loader.exec_module(native)
out = Path(sys.argv[1]).resolve()
out.mkdir(exist_ok=False)
prefix = '''#include <metal_stdlib>
using namespace metal;
vertex float4 vertex_main(uint id [[vertex_id]]) {
  return float4(float2((id << 1) & 2, id & 2) * 2.0 - 1.0, 0.0, 1.0);
}
struct Output {
  float4 colour [[color(0)]];
  float depth [[depth(any)]];
  uint mask [[sample_mask]];
};
'''
for name, kill, mask in [('full', '', '15u'), ('mask5', '', '5u'),
                         ('discard-odd', 'if (s & 1u) discard_fragment();', '15u')]:
    d = out / name
    d.mkdir()
    source = prefix + '''fragment Output fragment_main(uint s [[sample_id]]) {
  ''' + kill + '''
  return {float4(float(s) * 0.25, 0.0, 1.0, 1.0), float(s) * 0.125, ''' + mask + '''};
}
'''
    (d / 'input.metal').write_text(source)
    rp = {'vertex_function': 'vertex_main', 'fragment_function': 'fragment_main',
          'depth_attachment_pixel_format': 'Depth32Float', 'raster_sample_count': 4,
          'color_attachments': [{'pixel_format': 'RGBA8Unorm'}]}
    (d / 'pipeline.mtlp-json').write_text(json.dumps({'pipelines': {'render_pipelines': [rp]}}, indent=2))
    commands = [
        ['xcrun', 'metal', '-c', str(d / 'input.metal'), '-o', str(d / 'input.air')],
        ['xcrun', 'metallib', str(d / 'input.air'), '-o', str(d / 'input.metallib')],
        ['xcrun', 'metal-nt', '-arch', 'applegpu_g17p', '-platform_version', 'macos', '27.0', '27.0',
         '-N', str(d / 'pipeline.mtlp-json'), str(d / 'input.metallib'), '-o', str(d / 'output.nat')],
    ]
    for i, command in enumerate(commands):
        result = subprocess.run(command, capture_output=True, timeout=60)
        (d / f'command-{i}.log').write_bytes(result.stdout + result.stderr)
        result.check_returncode()
    raw = (d / 'output.nat').read_bytes()
    sections = list(native.sections(raw))
    offsets = {o for o, s, n, b in sections if n == '__fragment' and s.startswith('__GPU')}
    fragments = [b for o, s, n, b in sections if o in offsets and s == '__TEXT' and n == '__text']
    assert len(fragments) == 1
    code = fragments[0]
    (d / 'fragment.bin').write_bytes(code)
    receipt = {'commands': commands, 'hardware_execution': False,
               'source_sha256': hashlib.sha256(source.encode()).hexdigest(),
               'native_sha256': hashlib.sha256(raw).hexdigest(),
               'text_sha256': hashlib.sha256(code).hexdigest(), 'text_bytes': len(code),
               'fragment_hex': code.hex()}
    (d / 'receipt.json').write_text(json.dumps(receipt, indent=2) + '\n')
    print(name, len(code), receipt['text_sha256'])
