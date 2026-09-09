#!/usr/bin/env python3
"""Inspect auxiliary HEVC sample descriptions and independently probe their SPS."""
import argparse
import hashlib
import json
from pathlib import Path
import struct
import subprocess

def atoms(data):
    offset = 0
    while offset < len(data):
        if len(data)-offset < 8:
            if any(data[offset:]): raise ValueError('Nonzero atom padding')
            break
        size, kind = struct.unpack_from('>I4s', data, offset)
        if not 8 <= size <= len(data)-offset: raise ValueError('Unbounded MOV atom')
        yield kind, data[offset+8:offset+size]
        offset += size

def one(data, kind):
    found = [v for k,v in atoms(data) if k == kind]
    if len(found) != 1: raise ValueError('Require exactly one '+repr(kind))
    return found[0]

def description(movie):
    if len(movie) > 64*1024*1024: raise ValueError('MOV exceeds diagnostic bound')
    tree = movie
    for kind in [b'moov', b'trak', b'mdia', b'minf', b'stbl', b'stsd']:
        tree = one(tree, kind)
    if tree[:8] != struct.pack('>II', 0, 1): raise ValueError('Require one sample description')
    entries = list(atoms(tree[8:]))
    if len(entries) != 1: raise ValueError('Ambiguous sample entry')
    codec, entry = entries[0]
    if len(entry) < 78 or struct.unpack_from('>HH', entry, 24) != (320,240):
        raise ValueError('Require 320x240 visual entry')
    extensions = list(atoms(entry[78:]))
    h = one(entry[78:], b'hvcC')
    if len(h) < 23 or h[0] != 1 or h[21]&3 != 3: raise ValueError('Require four-byte HEVC lengths')
    offset = 23; prefix = bytearray(); arrays = []
    for _ in range(h[22]):
        if len(h)-offset < 3: raise ValueError('Truncated HEVC array')
        kind = h[offset]&63; count = struct.unpack_from('>H',h,offset+1)[0]; offset += 3
        for _ in range(count):
            if len(h)-offset < 2: raise ValueError('Truncated parameter length')
            size = struct.unpack_from('>H',h,offset)[0]; offset += 2
            if not 2 <= size <= len(h)-offset: raise ValueError('Truncated parameter NAL')
            nal = h[offset:offset+size]; offset += size
            if nal[0]&128 or not nal[1]&7 or nal[0]>>1&63 != kind: raise ValueError('Invalid parameter NAL')
            prefix += b'\0\0\0\1'+nal
            arrays.append(dict(nal_type=kind, bytes=size))
    if offset != len(h): raise ValueError('Trailing HEVC configuration bytes')
    return bytes(prefix), dict(sample_entry=codec.decode('ascii'), chroma_format=h[16]&3,
        luma_bit_depth=8+(h[17]&7), chroma_bit_depth=8+(h[18]&7), parameter_arrays=arrays,
        extensions=[dict(type=k.decode('ascii'), bytes=len(v)) for k,v in extensions],
        hvcc_sha256=hashlib.sha256(h).hexdigest())

def sample_nals(sample):
    if not 6 <= len(sample) <= 4*1024*1024: raise ValueError('Sample exceeds diagnostic bounds')
    offset = 0
    while offset < len(sample):
        if len(sample)-offset < 4: raise ValueError('Truncated sample length')
        size = struct.unpack_from('>I',sample,offset)[0]; offset += 4
        if not 2 <= size <= len(sample)-offset: raise ValueError('Truncated sample NAL')
        nal = sample[offset:offset+size]; offset += size
        if nal[0]&128 or not nal[1]&7: raise ValueError('Invalid sample NAL')
        yield nal

def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('reference',type=Path); p.add_argument('output',type=Path)
    a = p.parse_args()
    meta = json.loads((a.reference/'result.json').read_text())
    if not (meta['passed'] and meta['hardware_required'] and meta['using_hardware_encoder']
            and meta['model']=='Mac17,5' and '25G76' in meta['system_version']):
        raise ValueError('Require successful pinned Neo hardware reference')
    prefix, report = description((a.reference/'video.mov').read_bytes())
    files = sorted(a.reference.glob('frame-*.sample'))
    if not 1 <= len(files) <= 60 or len(files) != meta['encoded_frames']: raise ValueError('Invalid frame count')
    layers = set(); stream = bytearray(prefix)
    for path in files:
        for nal in sample_nals(path.read_bytes()):
            if nal[0]>>1&63 < 32: layers.add((nal[0]&1)<<5 | nal[1]>>3)
            stream += b'\0\0\0\1'+nal
    probe = subprocess.run(['ffprobe','-v','error','-f','hevc','-show_streams','-of','json','pipe:0'],
        input=bytes(stream),capture_output=True,check=True)
    streams = json.loads(probe.stdout)['streams']
    if len(streams) != 1: raise ValueError('Unexpected elementary stream count')
    codec = streams[0]
    if codec['codec_name'] != 'hevc' or (codec['width'],codec['height']) != (320,240):
        raise ValueError('Unexpected HEVC dimensions')
    a.output.mkdir(parents=True,exist_ok=False)
    (a.output/'video.hevc').write_bytes(stream)
    (a.output/'ffprobe.json').write_bytes(probe.stdout)
    (a.output/'ffprobe.log').write_bytes(probe.stderr)
    report.update(reference=str(a.reference.resolve()), sample_count=len(files), vcl_layer_ids=sorted(layers),
        elementary_profile=codec['profile'], elementary_pixel_format=codec['pix_fmt'],
        native_linux_validated=False, claim='Independent reference sample-description and SPS inspection')
    (a.output/'inspection.json').write_text(json.dumps(report,indent=2)+'\n')
    print(json.dumps(report))

if __name__ == '__main__': main()
