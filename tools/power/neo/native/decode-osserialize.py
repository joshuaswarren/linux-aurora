#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-2.0-only
"""Decode one saved OSSerialize binary object, including symbols/references."""
import argparse
import hashlib
import json
from pathlib import Path
import struct


def decode(data, offset):
    objects = []
    cursor = offset

    def take(n):
        nonlocal cursor
        if n < 0 or cursor + n > len(data):
            raise ValueError('truncated object')
        value = data[cursor:cursor + n]
        cursor += n
        return value

    def word():
        return struct.unpack('<I', take(4))[0]

    def obj(depth=0):
        nonlocal cursor
        if depth > 100:
            raise ValueError('excessive nesting')
        cursor = (cursor + 3) & ~3
        tag = word()
        kind, size = (tag >> 24) & 31, tag & 0xffffff
        if kind == 12:
            return objects[size]
        slot = len(objects)
        objects.append(None)
        if kind == 1:
            value = {}
            objects[slot] = value
            for _ in range(size):
                key = obj(depth + 1)
                value[key] = obj(depth + 1)
        elif kind in (2, 3):
            value = [obj(depth + 1) for _ in range(size)]
        elif kind == 4:
            if size not in (8, 16, 32, 64):
                raise ValueError(f'unsupported integer width {size}')
            value = int.from_bytes(take(8), 'little')
        elif kind in (8, 9):
            value = take(size).rstrip(b'\0').decode('utf-8')
        elif kind == 10:
            value = {'data_hex': take(size).hex()}
        elif kind == 11:
            value = bool(size)
        else:
            raise ValueError(f'unknown type {kind} at {cursor - 4:#x}')
        objects[slot] = value
        return value

    if word() != 0xd3:
        raise ValueError('expected OSSerialize signature')
    value = obj()
    return value, cursor


if __name__ == '__main__':
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('image', type=Path)
    p.add_argument('offset', type=lambda s: int(s, 0))
    p.add_argument('--output', type=Path, required=True)
    a = p.parse_args()
    data = a.image.read_bytes()
    value, end = decode(data, a.offset)
    a.output.write_text(json.dumps(dict(image=str(a.image),
        sha256=hashlib.sha256(data).hexdigest(), offset=a.offset, end=end,
        value=value), indent=2) + '\n')
