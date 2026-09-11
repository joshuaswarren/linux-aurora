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
parser.add_argument('--descriptor', action='store_true',
                    help='Compare the complete measured 4096-byte descriptor prefix')
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
    assert!(a.len() == 12 || a.len() == 46);
    let p = M5ComputeParameters {
        preempt_base: a[0], cdm_base: a[1], dispatch_identity: a[2],
        context_id: u8::try_from(a[3]).unwrap(), work_ordinal: u8::try_from(a[4]).unwrap(),
        robustness: a[5], operand_state_base: a[6], private_pool_hw_id: a[7],
        descriptor_control_530: a[8], descriptor_control_538: a[9],
        descriptor_flag_60a: a[10] != 0, descriptor_flag_60b: a[11] != 0,
    };
    if a.len() == 12 {
        let mut bytes = [0; M5_COMPUTE_REGISTER_BYTES];
        encode_m5_compute_registers(p, &mut bytes).unwrap();
        std::io::stdout().write_all(&bytes).unwrap();
    } else {
        let mut event_metadata = [0u8; 16];
        for i in 0..16 { event_metadata[i] = u8::try_from(a[29+i]).unwrap(); }
        let descriptor = M5ComputeDescriptor {
            registers: p, submit_sequence: a[12], scheduler_record: a[13],
            descriptor_low_alias: a[14], cdm_terminator: a[15],
            dispatch_stamps: [a[16], a[17]],
            event_machine_stamp: u32::try_from(a[18]).unwrap(),
            queue_id: u32::try_from(a[19]).unwrap(), queue_ordinal: u32::try_from(a[20]).unwrap(),
            has_event: a[21] != 0, submission_index: u32::try_from(a[22]).unwrap(),
            status: [a[23], a[24]], shared_control: a[25], shared_control_enabled: a[26] != 0,
            zero_page: a[27], owner_id: u8::try_from(a[28]).unwrap(), event_metadata,
            descriptor_work_ordinal: u32::try_from(a[45]).unwrap(),
        };
        let mut bytes = [0; M5_COMPUTE_DESCRIPTOR_BYTES];
        encode_m5_compute_descriptor(descriptor, &mut bytes).unwrap();
        std::io::stdout().write_all(&bytes).unwrap();
    }
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
            # Independent input for register 0x14028, rather than reading
            # that register back as its own expected value.
            hardware_id, = struct.unpack_from('<i', raw, 0x788)
            if not 0 <= hardware_id < 512:
                raise ValueError('Native private pool hardware ID is outside the UMA table')
            values[7] = hardware_id
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
        if args.descriptor:
            if not descriptor_objects or len(data) < 0x1000:
                raise ValueError('Full comparison needs a host descriptor and 4096 native bytes')
            q = lambda blob, offset: struct.unpack_from('<Q', blob, offset)[0]
            w = lambda blob, offset: struct.unpack_from('<I', blob, offset)[0]
            # Scheduler pointer, CDM end, event-presence flag, status pointers,
            # shared-control enable and owner ID have independent host inputs.
            # Remaining retained addresses/opaque event data are native inputs.
            values.extend([q(data, 4), q(raw, 0x188), q(data, 0x740) - 0x40,
                           q(raw, 0x4c0), q(data, 0xf40), q(data, 0xf48),
                           w(data, 0xf50), manifest['queue_id'], w(data, 0xf58),
                           int(w(raw, 0x48) != 0), w(data, 0xf60),
                           q(raw, 0x1a0), q(raw, 0x1a8), q(raw, 0x798),
                           raw[0x7c4] & 1, q(data, 0xfcb), raw[0x148],
                           *data[0xfbb:0xfcb], w(raw, 0x140)])
            expected = data[:0x1000]
        result = subprocess.run([str(binary), *[format(x, 'x') for x in values]],
                                check=True, capture_output=True).stdout
        if result != expected:
            if args.descriptor:
                bad = [hex(i) for i, (a, b) in enumerate(zip(result, expected)) if a != b]
                raise ValueError('Production M5 descriptor differs at byte offsets: ' + str(bad))
            bad = [i for i in range(40) if result[i * 12:(i + 1) * 12] != expected[i * 12:(i + 1) * 12]]
            raise ValueError('Production M5 register writer differs at indices: ' + str(bad))
        print('M5_NATIVE_DESCRIPTOR_MATCH' if args.descriptor else 'M5_NATIVE_REGISTER_MATCH',
              capture.name, 'registers=40 bytes=' + ('4096' if args.descriptor else '480'),
              'controls=' + control_source)
print('LINUX_GPU_EXECUTION_QUALIFIED=false')
