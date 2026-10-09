#!/usr/bin/env python3
"""Offline G17P fragment MSAA depth-fetch reference, with no GPU execution."""
import importlib.util
from pathlib import Path
import sys

base = Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location('native_capture', base/'native-mrt-probe.py')
capture = importlib.util.module_from_spec(spec)
spec.loader.exec_module(capture)
assert sys.argv[1:] in ([], ['--color-control'], ['--register-sample'])
color = sys.argv[1:] == ['--color-control']
register = sys.argv[1:] == ['--register-sample']
capture.OUT = base/('native-msaa-register-witness' if register else
                   'native-msaa-color-witness' if color else 'native-msaa-depth-witness-v2')
capture.OUT.mkdir(exist_ok=False)

vertex = '''
#include <metal_stdlib>
using namespace metal;
struct DepthOutput { float depth [[depth(any)]]; };
vertex float4 vertex_main(uint id [[vertex_id]]) {
    return float4(float2((id << 1) & 2, id & 2) * 2.0 - 1.0, 0.0, 1.0);
}
'''
cases = [
    ('sample-zero', '0', ''),
    ('sample-variable', 'sample', ', constant uint &sample [[buffer(0)]]'),
]
if register:
    cases = [
        ('sample-fragment-x', '(uint(position.x) & 3u)', ''),
        ('sample-fragment-xy', '((uint(position.x) + uint(position.y)) & 3u)', ''),
    ]
for name, sample, declaration in cases:
    texture_type = 'texture2d_ms<float>' if color else 'depth2d_ms<float>'
    source = vertex + '''
fragment DepthOutput fragment_main(float4 position [[position]],
                             ''' + texture_type + ''' source [[texture(0)]]''' + declaration + ''') {
    return {source.read(uint2(position.xy), ''' + sample + ''')''' + ('.x' if color else '') + '''};
}
'''
    pipeline = {'pipelines': {'render_pipelines': [{
        'vertex_function': 'vertex_main',
        'fragment_function': 'fragment_main',
        'depth_attachment_pixel_format': 'Depth32Float',
    }]}}
    capture.compile_capture(name, source, pipeline)
