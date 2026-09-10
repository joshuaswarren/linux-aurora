#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""Compare the production M5 Rust register writer with private native captures.

Command-owned addresses, identities and controls become inputs. Register
numbers, control-field equations, ordering and offsets must match native bytes.
This does not qualify a Linux submission or the remaining descriptor header.
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
            candidate = root / 'drivers/gpu/drm/asahi/m5_compute.rs'
            if candidate.is_file():
                return candidate
    raise RuntimeError('Cannot locate production m5_compute.rs')


parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('captures', nargs='+', type=Path)
parser.add_argument('--source', type=Path)
args = parser.parse_args()
source = (args.source or find_source()).resolve()

with tempfile.TemporaryDirectory(prefix='m5-native-registers-') as temporary:
    directory = Path(temporary)
    harness = directory / 'verify.rs'
    harness.write_text('''use std::io::Write;
#[path = %s] mod m5_compute;
use m5_compute::*;
fn main() {
    let a: Vec<u64> = std::env::args().skip(1).map(|s| u64::from_str_radix(&s, 16).unwrap()).collect();
    assert_eq!(a.len(), 12);
    let p = M5ComputeParameters {
        preempt_base: a[0], cdm_base: a[1], dispatch_identity: a[2],
        context_id: u8::try_from(a[3]).unwrap(), work_ordinal: u8::try_from(a[4]).unwrap(),
        robustness: a[5], operand_state_base: a[6], execution_gate: a[7],
        descriptor_control_530: a[8], descriptor_control_538: a[9],
        descriptor_flag_60a: a[10] != 0, descriptor_flag_60b: a[11] != 0,
    };
    let mut bytes = [0; M5_COMPUTE_REGISTER_BYTES];
    encode_m5_compute_registers(p, &mut bytes).unwrap();
    std::io::stdout().write_all(&bytes).unwrap();
}
''' % json.dumps(str(source)))
    binary = directory / 'verify'
    subprocess.run(['rustc', '--edition=2021', str(harness), '-o', str(binary)], check=True)
    for capture in args.captures:
        manifest = json.loads((capture / 'manifest.json').read_text())
        if manifest.get('error') or manifest.get('data_master') != 2:
            raise ValueError('Expected a successful native M5 CL capture')
        objects = [x for x in manifest['objects'] if x['label'] == 'payload-view-0']
        if len(objects) != 1 or not objects[0]['stable']:
            raise ValueError('Expected one stable native compute payload')
        item = objects[0]
        data = (capture / item['file']).read_bytes()
        if len(data) != item['size'] or hashlib.sha256(data).hexdigest() != item['sha256']:
            raise ValueError('Captured payload does not match its receipt')
        if struct.unpack_from('<I', data)[0] != 3:
            raise ValueError('Unexpected native compute selector')
        if struct.unpack_from('<4H', data, 0x18) != (37, 39, 39, 40):
            raise ValueError('Unexpected native M5 register subranges')
        expected = data[0x40:0x40 + 40 * 12]
        registers = [struct.unpack_from('<IQ', expected, index * 12) for index in range(40)]
        identity = registers[23][1]
        context = registers[27][1]
        if context > 0xffff or registers[35][1] < 5:
            raise ValueError('Native command parameters are outside the supported profile')
        values = [registers[0][1], registers[1][1], identity, context >> 8,
                  context & 255, registers[30][1], registers[35][1] - 5, registers[29][1]]
        descriptor_objects = [x for x in manifest['objects'] if x['label'] == 'host-cl-descriptor-view']
        if descriptor_objects:
            descriptor = descriptor_objects[0]
            raw = (capture / descriptor['file']).read_bytes()
            if (not descriptor['stable'] or len(raw) != descriptor['size']
                    or hashlib.sha256(raw).hexdigest() != descriptor['sha256']):
                raise ValueError('Native descriptor does not match its stable receipt')
            control_530, control_538 = struct.unpack_from('<QQ', raw, 0x530)
            controls = [control_530, control_538, raw[0x60a] & 1, raw[0x60b] & 1]
            control_source = 'native-descriptor'
        else:
            # Older captures did not save the host descriptor. These check
            # register layout only; they cannot independently verify controls.
            mode = registers[10][1]
            if mode & ~0x80 not in (0x10c08860, 0x14c08a60):
                raise ValueError('Unsupported native M5 control mode')
            controls = [registers[7][1], registers[9][1] >> 32,
                        int(mode & ~0x80 == 0x14c08a60), int(bool(mode & 0x80))]
            control_source = 'legacy-register-values'
        values.extend(controls)
        result = subprocess.run([str(binary), *[format(x, 'x') for x in values]],
                                check=True, capture_output=True).stdout
        if result != expected:
            bad = [i for i in range(40) if result[i * 12:(i + 1) * 12] != expected[i * 12:(i + 1) * 12]]
            raise ValueError('Production M5 register writer differs at indices: ' + str(bad))
        print('M5_NATIVE_REGISTER_MATCH', capture.name, 'registers=40 bytes=480 controls=' + control_source)
print('LINUX_GPU_EXECUTION_QUALIFIED=false')
