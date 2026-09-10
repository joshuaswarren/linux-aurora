#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""Compare the production M5 tag-15 writer with a private native capture.

Queue aliases/identity/priority come from the host kick queue. Context and
three flags come from the host CL descriptor; the scheduler pointer comes
from the retained channel record. Remaining event and owner values are
opaque native inputs. This checks serialization, not Linux execution.
"""
import argparse
import hashlib
import json
from pathlib import Path
import struct
import subprocess
import tempfile


def source_path():
    for parent in Path(__file__).resolve().parents:
        for root in (parent, parent / 'linux'):
            path = root / 'drivers/gpu/drm/asahi/m5_channel.rs'
            if path.is_file():
                return path
    raise RuntimeError('Cannot locate production m5_channel.rs')


def read_object(capture, manifest, label):
    items = [x for x in manifest['objects'] if x['label'] == label]
    if len(items) != 1:
        raise ValueError('Expected one ' + label)
    item = items[0]
    data = (capture / item['file']).read_bytes()
    if (not item['stable'] or len(data) != item['size']
            or hashlib.sha256(data).hexdigest() != item['sha256']):
        raise ValueError('Invalid stable receipt for ' + label)
    return data


parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('captures', nargs='+', type=Path)
parser.add_argument('--source', type=Path)
args = parser.parse_args()
source = (args.source or source_path()).resolve()
with tempfile.TemporaryDirectory(prefix='m5-channel-') as temporary:
    directory = Path(temporary)
    harness = directory / 'verify.rs'
    harness.write_text('''use std::io::Write;
#[path = %s] mod m5_channel;
use m5_channel::*;
fn main() {
    let a: Vec<u64> = std::env::args().skip(1).map(|s| u64::from_str_radix(&s, 16).unwrap()).collect();
    assert_eq!(a.len(), 17);
    let config = M5ChannelConfig {
        ring_low: a[0], ring_high: a[1], queue_id: u8::try_from(a[2]).unwrap(),
        priority: u8::try_from(a[3]).unwrap(), install_queue: a[4] != 0,
        gpu_context: u8::try_from(a[5]).unwrap(),
        event: (a[6] != 0).then_some(M5ChannelEvent {
            address: a[6], timestamp: a[7], index: u32::try_from(a[8]).unwrap() }),
        channel_control: a[9], update_scheduler: a[10] != 0,
        scheduler_index: u32::try_from(a[11]).unwrap(),
        channel_stamp_index: u32::try_from(a[12]).unwrap(),
        update_context: a[13] != 0, owner_context: u32::try_from(a[14]).unwrap(),
        extended_command: a[15] != 0,
    };
    assert_eq!(a[16], 2);
    std::io::stdout().write_all(&encode_m5_channel_config(config).unwrap()).unwrap();
}
''' % json.dumps(str(source)))
    binary = directory / 'verify'
    subprocess.run(['rustc', '--edition=2021', str(harness), '-o', str(binary)], check=True)
    for capture in args.captures:
        manifest = json.loads((capture / 'manifest.json').read_text())
        if manifest.get('error') or manifest.get('data_master') != 2:
            raise ValueError('Expected successful native CL submission')
        command = read_object(capture, manifest, 'channel-command-before-1')
        if struct.unpack_from('<I', command)[0] != 15:
            raise ValueError('Expected controlled tag-15 at channel item 1')
        queue = read_object(capture, manifest, 'host-kick-queue')
        descriptor = read_object(capture, manifest, 'host-cl-descriptor-view')
        channel = read_object(capture, manifest, 'payload-view-1')
        q = lambda data, offset: struct.unpack_from('<Q', data, offset)[0]
        w = lambda data, offset: struct.unpack_from('<I', data, offset)[0]
        values = [q(queue, 0x80), q(queue, 0x88), w(queue, 0xc), w(queue, 0x1c),
                  descriptor[0x181] & 1, w(descriptor, 0x144),
                  q(command, 0x36), q(command, 0x3e), w(command, 0x46),
                  q(channel, 0x9c), descriptor[0x2dc] & 1,
                  w(command, 0x56), w(command, 0x5a), descriptor[0x180] & 1,
                  w(command, 0x66), w(command, 0x6a), w(command, 0x5e)]
        actual = subprocess.check_output([str(binary), *[format(x, 'x') for x in values]])
        expected = command[:0x86]
        if actual != expected:
            offsets = [hex(i) for i, (a, b) in enumerate(zip(actual, expected)) if a != b]
            raise ValueError('Tag-15 mismatch at ' + ', '.join(offsets))
        print(f'{capture.name}: all 134 tag-15 bytes match; queue, descriptor flags and channel pointer independently sourced')
