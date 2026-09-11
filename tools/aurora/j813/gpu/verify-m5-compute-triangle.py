#!/usr/bin/env python3
"""Verify retained Linux GPU pixels, then visualize exactly those buffer bytes.

The 512x512 image enlarges each of the 32x32 GPU pixels by 16 without filtering.
No image is generated if the input, geometry, colors, or capture hashes fail.
This is a compute-rasterization test, never a TA/3D or DCP qualification.
"""
import argparse
import hashlib
import json
from pathlib import Path
import struct
import zlib


def covered(x, y):
    # Independent edge-equation check, using doubled integer coordinates.
    vertices = ((31, 8), (8, 54), (54, 54))
    p = (2*x, 2*y)
    crosses = []
    for a, b in zip(vertices, vertices[1:] + vertices[:1]):
        crosses.append((b[0]-a[0])*(p[1]-a[1]) - (b[1]-a[1])*(p[0]-a[0]))
    return all(c <= 0 for c in crosses)


def verify_pixels(page):
    if len(page) != 0x4000:
        raise ValueError('Expected a retained 16-KiB buffer page')
    inputs = struct.unpack_from('<1024I', page, 0xd00)
    if inputs != tuple(i*7 for i in range(1024)):
        raise ValueError('GPU test input changed')
    rgba = page[0x1d00:0x2d00]
    foreground = 0
    for i in range(1024):
        x, y = i%32, i//32
        inside = covered(x,y)
        expected = bytes((x*8,y*8,255,255)) if inside else bytes((24,24,24,255))
        if rgba[i*4:i*4+4] != expected:
            raise ValueError('Unexpected GPU pixel at (%d,%d)' % (x,y))
        foreground += inside
    if foreground == 0:
        raise ValueError('Empty triangle')
    return rgba, foreground


def png(rgba, scale):
    if len(rgba) != 4096 or scale not in (1,16):
        raise ValueError('Expected exactly 32x32 source pixels and scale 1 or 16')
    scanlines = bytearray()
    for y in range(32):
        row = b''.join(rgba[(y*32+x)*4:(y*32+x+1)*4]*scale for x in range(32))
        scanlines.extend((b'\0'+row)*scale)
    def chunk(kind,data):
        return struct.pack('>I',len(data))+kind+data+struct.pack('>I',zlib.crc32(kind+data)&0xffffffff)
    return (b'\x89PNG\r\n\x1a\n'+chunk(b'IHDR',struct.pack('>2I5B',32*scale,32*scale,8,6,0,0,0))
        +chunk(b'tEXt',b'Description\0M5 Linux compute shader output; 32x32 source pixels; integer enlargement; TA/3D and DCP not tested')
        +chunk(b'IDAT',zlib.compress(bytes(scanlines),9))+chunk(b'IEND',b''))


def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('capture',type=Path)
    parser.add_argument('output_prefix',type=Path)
    args=parser.parse_args()
    manifest=json.loads((args.capture/'manifest.json').read_text())
    if (not manifest.get('compute_triangle') or manifest.get('device_memory_read') is not False
            or 'Linux verified compute completion' not in manifest.get('phase','')):
        raise ValueError('Require a successful Linux mode11 retained capture')
    for item in manifest['objects']:
        if 'error' in item:
            raise ValueError('Capture contains an unread object')
        data=(args.capture/item['file']).read_bytes()
        if hashlib.sha256(data).hexdigest() != item['sha256']:
            raise ValueError('Capture hash mismatch: '+item['label'])
    expected_programs = {
        'program-shader.bin':'b80d3e3f4346aa1909f12e6642e581a4deaa34496bc6cfdf996e9cce70fd4207',
        'program-pipeline.bin':'333512fb55873b31d39b1dc2814cdf25f7a409814c886f19ac91a45ae24263d7',
        'program-constants.bin':'6d2daba4e27eaa8e65505b87c64009ea5791360b55dfa33f864588fb196e8f05',
    }
    for name, digest in expected_programs.items():
        if hashlib.sha256((args.capture/name).read_bytes()).hexdigest() != digest:
            raise ValueError('Retained program differs from the triangle profile: '+name)
    rgba,foreground=verify_pixels((args.capture/'program-buffers.bin').read_bytes())
    full=png(rgba,16)
    args.output_prefix.parent.mkdir(parents=True,exist_ok=True)
    args.output_prefix.with_suffix('.rgba').write_bytes(rgba)
    args.output_prefix.with_suffix('.png').write_bytes(full)
    args.output_prefix.with_name(args.output_prefix.name+'-32x32.png').write_bytes(png(rgba,1))
    result=dict(platform='Linux on M5',test='compute-rasterized triangle',width=32,height=32,
        pixels_verified=1024,inputs_unchanged=1024,foreground_pixels=foreground,
        background_pixels=1024-foreground,rgba_sha256=hashlib.sha256(rgba).hexdigest(),
        png_sha256=hashlib.sha256(full).hexdigest(),presentation_scale=16,
        graphics_ta_3d_proven=False,dcp_presentation_tested=False)
    args.output_prefix.with_suffix('.json').write_text(json.dumps(result,indent=2)+'\n')
    print(json.dumps(result))


if __name__ == '__main__':
    main()
