#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""Compare M5 production entry/tag-14 codecs with private native captures.

Variable command operands come from captured entries or host descriptors. Independently captured
tag-14 and host queue state supply QID, priority, current and next stamp.
This checks byte layout and state transitions, not Linux GPU execution.
"""
import argparse
import hashlib
import json
from pathlib import Path
import struct
import subprocess
import tempfile


def find_source():
    for parent in Path(__file__).resolve().parents:
        for root in (parent, parent / 'linux'):
            candidate = root / 'drivers/gpu/drm/asahi/m5_sksm.rs'
            if candidate.is_file():
                return candidate
    raise RuntimeError('Cannot locate production m5_sksm.rs')


def load_object(directory, manifest, label):
    items = [x for x in manifest['objects'] if x['label'] == label]
    if len(items) != 1:
        raise ValueError('Expected exactly one ' + label)
    item = items[0]
    raw = (directory / item['file']).read_bytes()
    if (not item['stable'] or len(raw) != item['size']
            or hashlib.sha256(raw).hexdigest() != item['sha256']):
        raise ValueError('Unstable or corrupt capture: ' + label)
    return raw


def word(raw, offset):
    return struct.unpack_from('<Q', raw, offset)[0]


parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('captures', nargs='+', type=Path)
parser.add_argument('--source', type=Path)
args = parser.parse_args()
source = (args.source or find_source()).resolve()

with tempfile.TemporaryDirectory(prefix='m5-native-sksm-') as temporary:
    directory = Path(temporary)
    harness = directory / 'verify.rs'
    harness.write_text('''use std::io::Write;
#[path = %s] mod m5_sksm;
use m5_sksm::*;
fn main() {
    let a: Vec<u64> = std::env::args().skip(1).map(|s| u64::from_str_radix(&s, 16).unwrap()).collect();
    let q = M5SksmQueue { id: a[0].try_into().unwrap(), priority: a[1].try_into().unwrap(),
        current_stamp: a[2], submitted: a[3].try_into().unwrap(), previous_stamp: Some(a[4]) };
    let rce = core::array::from_fn(|i| M5SksmRce { address: a[19+i*2], tag: a[20+i*2].try_into().unwrap() });
    let mcache = if a[28] == 0 { None } else { Some(M5SksmMcache {
        address: a[29], index: a[30].try_into().unwrap(), count: a[31].try_into().unwrap() }) };
    let barriers: Vec<_> = a[32..].chunks_exact(3).map(|d| M5SksmDependency {
        queue_id: d[0].try_into().unwrap(), stamp: d[1], kind: d[2].try_into().unwrap() }).collect();
    let op = M5SksmOperands { barriers: &barriers, first_predecessor: 0,
        implicit_dependency_type: a[5].try_into().unwrap(), mcache,
        flag_bit1: a[6] != 0, flag_bit61: a[7] != 0, flag_bit63: a[8] != 0,
        payload: [a[9], a[10]], qos_byte_32: a[11].try_into().unwrap(),
        qos_class_40: a[12].try_into().unwrap(), qos_word_48: a[13].try_into().unwrap(),
        event_mask: [a[14], a[15], a[16], a[17]], rce_kind: a[18].try_into().unwrap(),
        rce, auxiliary: a[27] };
    let p = prepare_m5_sksm_entry(q, op).unwrap();
    let tag = encode_m5_sksm_add_kicks(q.id, q.priority, q.current_stamp, 1).unwrap();
    let mut out = std::io::stdout();
    out.write_all(&p.bytes).unwrap(); out.write_all(&tag).unwrap();
    out.write_all(&p.queue_after.current_stamp.to_le_bytes()).unwrap();
    out.write_all(&p.queue_after.submitted.to_le_bytes()).unwrap();
}
''' % json.dumps(str(source)))
    binary = directory / 'verify'
    subprocess.run(['rustc', '--edition=2021', str(harness), '-o', str(binary)], check=True)
    for capture in args.captures:
        manifest = json.loads((capture / 'manifest.json').read_text())
        if manifest.get('error') or manifest.get('data_master') != 2:
            raise ValueError('Expected a successful M5 CL capture')
        tag = load_object(capture, manifest, 'add-kicks-command')
        queue = load_object(capture, manifest, 'host-kick-queue')
        ring = load_object(capture, manifest, 'kick-ring')
        tag_id, qid, count, priority, stamp, dm = struct.unpack('<IHHBQI', tag)
        native_qid, entries, stride = struct.unpack_from('<III', queue, 0xc)
        submitted, = struct.unpack_from('<I', queue, 0x44)
        if (tag_id != 14 or dm != 2 or qid != native_qid or count != 1
                or (entries, stride) != (256, 512) or submitted < 1):
            raise ValueError('Unsupported native queue state or batched capture')
        entry = ring[(stamp & 255) * stride:][:stride]
        header, control, kinds = word(entry, 0), word(entry, 0x20), word(entry, 0x180)
        valid = control & 0xffffffff
        dep_count = valid.bit_count()
        if not dep_count or valid != (1 << dep_count) - 1:
            raise ValueError('Non-contiguous dependency mask')
        deps = []
        for i in range(dep_count):
            parent = word(entry, 0x28 + i * 8)
            deps.append([parent >> 40, parent & ((1 << 40) - 1), (kinds >> (i * 2)) & 3])
        if deps[-1][0] != qid:
            raise ValueError('Last dependency is not the native queue predecessor')
        values = [qid, priority, stamp, submitted - 1, deps[-1][1], deps[-1][2],
                  (header >> 1) & 1, (header >> 61) & 1, (header >> 63) & 1,
                  word(entry, 0x10), word(entry, 0x18), (control >> 32) & 255,
                  (control >> 40) & 31, control >> 48,
                  *[word(entry, 0x128 + i * 8) for i in range(4)], entry[0x148]]
        for i in range(4):
            rce = word(entry, 0x150 + i * 8)
            values.extend([(rce & ((1 << 38) - 1)) << 5, rce >> 43])
        cache = word(entry, 8)
        values.extend([word(entry, 0x178), (header >> 58) & 1,
                       ((cache >> 6) & ((1 << 38) - 1)) << 5,
                       cache & 63, ((cache >> 54) & 63) + 1])
        for dep in deps[:-1]:
            values.extend(dep)
        operand_source = 'legacy-entry-values'
        if any(x['label'] == 'host-cl-descriptor-view' for x in manifest['objects']):
            descriptor = load_object(capture, manifest, 'host-cl-descriptor-view')
            # HAL300 encodeCLKickEntry supplies these independent descriptor
            # fields to encodeKickEntry; do not derive them from its output.
            values[6:9] = [descriptor[x] & 1 for x in (0x4c, 0x5b8, 0x5b9)]
            values[18] = struct.unpack_from('<I', descriptor, 0x5c0)[0] & 3
            for i in range(4):
                values[19+i*2] = word(descriptor, 0x5c8+i*16)
                values[20+i*2] = struct.unpack_from('<H', descriptor, 0x5d0+i*16)[0] & 127
            address, index, cache_count = struct.unpack_from('<QII', descriptor, 0x740)
            values[28:32] = [int(cache_count != 0), address, index & 63, cache_count]
            operand_source = 'native-descriptor-flags-rce-mcache'
        result = subprocess.run([str(binary), *[format(x, 'x') for x in values]],
                                capture_output=True, check=True).stdout
        expected = entry + tag + queue[0x48:0x50] + queue[0x44:0x48]
        if result != expected:
            offsets = [hex(i) for i, (a, b) in enumerate(zip(result, expected)) if a != b]
            raise ValueError('M5 SKSM production mismatch at ' + str(offsets))
        print('M5_NATIVE_SKSM_MATCH', capture.name,
              'entry=512 tag14=21 producer=12 dependency_types=matched operands=' + operand_source)
print('LINUX_GPU_EXECUTION_QUALIFIED=false')
