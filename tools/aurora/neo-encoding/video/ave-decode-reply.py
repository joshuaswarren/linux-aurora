#!/usr/bin/env python3
"""Decode saved run-command.sh output without treating packet receipt as success."""
import argparse
import json
from pathlib import Path
import re
import struct

p = argparse.ArgumentParser()
p.add_argument('log', type=Path)
a = p.parse_args()
text = a.log.read_text(errors='replace')
part = text.split('\nAVE_REPLY_BEGIN\n', 1)[1].split('\nAVE_REPLY_END\n', 1)[0]
data = bytes.fromhex(' '.join(l.strip() for l in part.splitlines()
                            if re.fullmatch(r'(?: [0-9a-f]{2})+', l)))
records = []
while data:
    assert len(data) >= 24
    channel, size, flags, reserved, addr = struct.unpack_from('<IIIIQ', data)
    assert reserved == 0 and len(data) >= 24 + size
    payload = data[24:24 + size]
    records.append(dict(channel=channel, bytes=size, flags=hex(flags),
                        firmware_address=hex(addr), payload_hex=payload.hex(),
                        words=[hex(w) for w in struct.unpack('<' + 'I' * (size // 4), payload)]))
    data = data[24 + size:]
print(json.dumps(dict(records=records, encoded_frame_proven=False), indent=2))
