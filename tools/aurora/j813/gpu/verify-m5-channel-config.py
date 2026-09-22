#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""Compare the production Rust tag-15 encoder with saved native records.

Only stable, hashed channel records are consumed. A match establishes the
record codec, not successful submission, Linux rendering or presentation.
"""
import argparse
import hashlib
import json
from pathlib import Path
import struct
import subprocess
import tempfile


def native_record(directory):
    manifest = json.loads((directory / 'manifest.json').read_text())
    records = []
    for item in manifest['objects']:
        if not item['label'].startswith('owner-channel-item-'):
            continue
        data = (directory / item['file']).read_bytes()
        if len(data) != item['size'] or hashlib.sha256(data).hexdigest() != item['sha256']:
            raise ValueError('Channel record provenance mismatch')
        if not item['stable'] or not item.get('mapping_stable'):
            raise ValueError('Channel record or mapping was unstable')
        if struct.unpack_from('<I', data)[0] == 15:
            records.append(data[:0x86])
    if len(records) != 1:
        raise ValueError('Expected one captured channel configuration')
    return records[0]


def rust_case(data):
    u = lambda offset: struct.unpack_from('<I', data, offset)[0]
    q = lambda offset: struct.unpack_from('<Q', data, offset)[0]
    fields = dict(ring_low=q(8), ring_high=q(16),
        queue_id=struct.unpack_from('<H', data, 24)[0], priority=u(30),
        install_queue=bool(u(26)), gpu_context=u(50), channel_control=q(74),
        update_scheduler=bool(u(82)), scheduler_index=u(86), channel_stamp_index=u(90),
        update_context=bool(u(98)), owner_context=u(102), extended_command=bool(u(106)))
    fields = ','.join(f'{key}:{str(value).lower() if isinstance(value, bool) else hex(value)}'
                      for key, value in fields.items())
    event = ('None' if not q(54) else
             f'Some(M5ChannelEvent{{address:{q(54)},timestamp:{q(62)},index:{u(70)}}})')
    pm = ('None' if not q(110) else
          f'Some(M5ParameterBufferEvent{{address:{q(110)},timestamp:{q(118)},'
          f'hardware_buffer_id:{u(126)},manager_id:{u(130)}}})')
    if u(38) not in (0, 1) or (not u(38) and q(42)):
        raise ValueError('Unqualified GPU state-update fields')
    state = f'Some({q(42)})' if u(38) else 'None'
    return (f'let p=M5ChannelConfig{{{fields},event:{event}}};'
            f'std::io::stdout().write_all(&encode_m5_channel_config_for('
            f'p,{u(34)},{u(94)},{state},{pm}).unwrap()).unwrap();')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--source', type=Path, required=True)
    parser.add_argument('--ta', type=Path, required=True)
    parser.add_argument('--fragment', type=Path, required=True)
    args = parser.parse_args()
    records = [native_record(args.ta), native_record(args.fragment)]
    # JSON's quoted path syntax is also valid for this Rust string literal.
    program = ('#[path=' + json.dumps(str(args.source.resolve())) + '] mod channel;'
               'use channel::*;use std::io::Write;fn main(){' +
               ''.join(rust_case(record) for record in records) + '}')
    with tempfile.TemporaryDirectory(prefix='m5-channel-oracle-') as directory:
        source = Path(directory) / 'oracle.rs'
        binary = Path(directory) / 'oracle'
        source.write_text(program)
        subprocess.run(['rustc', '--edition=2021', str(source), '-o', str(binary)],
                       check=True)
        actual = subprocess.check_output([str(binary)])
    expected = b''.join(records)
    differences = [dict(stage='ta' if i < 0x86 else 'fragment',
                        offset=hex(i % 0x86), actual=a, expected=b)
                   for i, (a, b) in enumerate(zip(actual, expected)) if a != b]
    passed = actual == expected
    print(json.dumps(dict(passed=passed, records=2, bytes=len(actual),
        differences=differences, linux_rendering_proven=False), indent=2))
    return 0 if passed else 1


if __name__ == '__main__':
    raise SystemExit(main())
