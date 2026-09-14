#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-2.0-only
"""Extract bounded CLHE records from a retained RTKit crash buffer.

The J700 buffer contains several CLHE records inside an HGLC wrapper. Scan
for records rather than interpreting unknown wrapper ownership fields. Keep
unknown sections as offset/size/SHA256 references to the original capture.
"""
import argparse
import hashlib
import json
from pathlib import Path
import struct
import uuid


def decode(blob):
    logs = []
    start = 0
    while (start := blob.find(b'EHLC', start)) >= 0:
        origin = start
        start += 4
        if origin + 32 > len(blob):
            continue
        _, version, size, flags = struct.unpack_from('<4I', blob, origin)
        if size < 32 or origin + size > len(blob):
            continue
        end = origin + size
        offset = origin + 32
        sections = []
        footer = False
        slide = None
        while offset + 16 <= end:
            code = blob[offset:offset + 4][::-1].decode('ascii', 'replace')
            if code == 'CLHE':
                footer = True
                break
            length = struct.unpack_from('<I', blob, offset + 12)[0]
            if length < 16 or offset + length > end:
                break
            data = blob[offset + 16:offset + length]
            section = dict(offset=hex(offset), type=code, size=length,
                           sha256=hashlib.sha256(data).hexdigest())
            if code == 'Cstr' and len(data) >= 4:
                section['id'] = struct.unpack_from('<I', data)[0]
                section['text'] = data[4:].split(b'\0')[0].decode('utf8', 'replace')
            elif code == 'Cver' and len(data) >= 16:
                section['uuid'] = str(uuid.UUID(bytes=data[:16]))
                text_offset = 16
                if version == 0x303 and len(data) >= 24:
                    slide = struct.unpack_from('<Q', data, 16)[0]
                    section['slide'] = hex(slide)
                    text_offset = 24
                section['text'] = data[text_offset:].split(b'\0')[0].decode('utf8', 'replace')
            elif code == 'Ctim' and len(data) >= 8:
                section['ticks'] = struct.unpack_from('<Q', data)[0]
            elif code == 'Ccst' and len(data) >= 8 and len(data) % 8 == 0:
                section['raw_words'] = [hex(x[0]) for x in struct.iter_unpack('<Q', data)]
            elif code == 'Cmbx' and len(data) >= 28:
                section['type'] = code
                section['mailbox_type'] = struct.unpack_from('<I', data, 16)[0]
                section['index'] = struct.unpack_from('<I', data, 24)[0]
                # Preserve raw order: the version-0x303 capture places the
                # endpoint first, unlike the older kernel dump's labels.
                section['records'] = [dict(word0=hex(a), word1=hex(b), ticks=t)
                                      for a, b, t, _ in struct.iter_unpack(
                                          '<QQII', data[28:28 + (len(data) - 28) // 24 * 24])]
            sections.append(section)
            offset += length
        # Footer signatures also match the scan; reject empty/invalid candidates.
        if sections and footer:
            logs.append(dict(offset=hex(origin), version=version, size=size,
                             flags=flags, sections=sections))
    return logs


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('capture', type=Path)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    blob = args.capture.read_bytes()
    result = dict(capture=str(args.capture), sha256=hashlib.sha256(blob).hexdigest(),
                  records=decode(blob))
    if not result['records']:
        parser.error('no bounded CLHE records with a footer found')
    with args.output.open('x') as output:
        json.dump(result, output, indent=2)
        output.write('\n')
    print(f"Decoded {len(result['records'])} records")


if __name__ == '__main__':
    main()
