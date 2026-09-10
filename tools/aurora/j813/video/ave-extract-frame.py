#!/usr/bin/env python3
"""Extract M5 H.264 output from a saved, gzip-protected Linux DMA snapshot."""
import argparse
import base64
import gzip
import hashlib
import json
from pathlib import Path
import re
import struct
import os

p = argparse.ArgumentParser()
p.add_argument('log', type=Path)
p.add_argument('output', type=Path)
p.add_argument('--allow-p-frame', action='store_true')
a = p.parse_args()
os.umask(0o077)
text = a.log.read_text(errors='replace')
part = text.split('\nAVE_ARENA_BEGIN\n', 1)[1].split('\nAVE_ARENA_END\n', 1)[0]
data = gzip.decompress(base64.b64decode(''.join(part.split()), validate=True))
assert len(data) == 0x528000
# Start +0xd50 / Process +0x61b8: bitstream surface; Start +0xf50 /
# Process +0x61c0: its metadata surface. Both were explicitly relocated.
stream_offset,stream_bytes,meta_offset,tile_offset=0xb4000,0x3c000,0x1f4000,0x244000
assert stream_offset+stream_bytes<=len(data) and meta_offset+0xc000<=len(data) and tile_offset+1024<=len(data)
stream = data[stream_offset:stream_offset+stream_bytes]
meta = data[meta_offset:meta_offset+0xc000]
size = struct.unpack_from('<I', meta, 0x120)[0]
assert size == struct.unpack_from('<I', meta, 0x12c)[0]
assert 5 <= size <= len(stream)
encoded_slice = stream[:size]
assert encoded_slice[:4] == b'\0\0\0\1'
slice_type = encoded_slice[4] & 31
allowed = (1, 5) if a.allow_p_frame else (5,)
assert slice_type in allowed
header_types = (7, 8)
matches = [m.start() for m in re.finditer(b'\x00\x00\x00\x01', meta)]
headers = {}
for index, start in enumerate(matches):
    # Metadata counters can coincidentally contain a start-code pattern.
    # A NAL header must have forbidden_zero_bit clear.
    if start+6 > len(meta) or meta[start+4] & 0x80:
        continue
    nal_type = meta[start + 4] & 31
    if nal_type not in header_types:
        continue
    end = matches[index + 1] if index + 1 < len(matches) else len(meta)
    assert nal_type not in headers
    headers[nal_type] = meta[start:end].rstrip(b'\0')
assert set(headers) == set(header_types)
output = b''.join(headers[k] for k in header_types) + encoded_slice
a.output.write_bytes(output)
a.output.with_suffix('.arena.bin').write_bytes(data)
a.output.with_suffix('.json').write_text(json.dumps(dict(
    source='native Linux AVE DMA memory', arena_sha256=hashlib.sha256(data).hexdigest(),
    codec='h264', bitstream_bytes=size,
    stream_offset=stream_offset,metadata_offset=meta_offset,tile_offset=tile_offset,
    parameter_set_bytes={str(k): len(headers[k]) for k in header_types},
    output_bytes=len(output), sha256=hashlib.sha256(output).hexdigest(),
    decoded_frame_proven=False), indent=2) + '\n')
print(f'AVE_LINUX_BITSTREAM_EXTRACTED slice_bytes={size} annexb_bytes={len(output)} decoded=unverified')
