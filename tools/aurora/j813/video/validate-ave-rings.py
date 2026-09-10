#!/usr/bin/env python3
"""Run the Linux C codec against actual AVE ring publication captures."""
import argparse
import hashlib
import json
from pathlib import Path
import struct
import subprocess

p = argparse.ArgumentParser()
p.add_argument('run', type=Path)
p.add_argument('codec', type=Path)
p.add_argument('output', type=Path)
a = p.parse_args()
rows = [json.loads(x) for x in (a.run/'ave-rings.jsonl').read_text().splitlines()]
commands = [json.loads(x) for x in (a.run/'ave-commands.jsonl').read_text().splitlines()]
sends = {r['seq']: r for r in commands if r.get('function') == 'channel-send'}
published = [r for r in rows if r['kind'] == 'published']
errors = [r for r in rows + commands if r['kind'] in ('capture-error', 'limit-reached')]
results = []
for r in published:
    before = sends[r['send_seq']]
    bits = 64 if r['wide'] else 32
    address = int(r['firmware_address'], 16) & ((1 << bits) - 1)
    cmd = [str(a.codec), str(bits), str(r['mode']), hex(address), str(r['length']),
           str(r['flags']), before['ring_before_hex']]
    result = subprocess.run(cmd, check=True, text=True, capture_output=True)
    encoded = bytes.fromhex(result.stdout.strip())
    actual = bytes.fromhex(r['entry_hex'])
    if len(actual) != 64 or len(encoded) != 64:
        raise SystemExit('Invalid descriptor length')
    fields_match = encoded[:bits // 8 * 3] == actual[:bits // 8 * 3]
    padding_match = encoded[bits // 8 * 3:] == actual[bits // 8 * 3:]
    current = bytes.fromhex(r['current_channel_hex'])
    consumer, producer = struct.unpack_from('<ii', current, 0x20)
    expected_consumer = r['producer_index'] if r['consumer_index'] == -1 else r['consumer_index']
    expected_producer = (r['producer_index'] + 1) % r['slot_count']
    if expected_producer == expected_consumer:
        expected_producer = -1
    results.append({'send_seq': r['send_seq'], 'bits': bits, 'mode': r['mode'],
                    'slot_index': r['slot_index'], 'fields_match': fields_match,
                    'padding_preserved': padding_match,
                    'index_transition_matches': [consumer, producer] == [expected_consumer, expected_producer],
                    'expected_hex': encoded.hex(), 'captured_hex': actual.hex()})

# These are input-validation checks, not evidence that the narrow format has
# run on the M5. The hardware publication comparisons above are the proof.
negative_inputs = [(64, 0, 3), (32, 0, 1 << 32), (16, 0, 4), (64, 2, 4)]
rejected = []
for bits, mode, address in negative_inputs:
    result = subprocess.run([str(a.codec), str(bits), str(mode), str(address), '72', '0', '00'*64],
                            text=True, capture_output=True)
    rejected.append(result.returncode == 3 and result.stdout == '')
disarmed = [r for r in rows if r['kind'] == 'disarmed']
report = {
    'claim': 'Linux userspace C wire codec compared with physical M5 AVE descriptor bytes; no Linux AVE driver',
    'codec_sha256': hashlib.sha256(a.codec.read_bytes()).hexdigest(),
    'publications': len(results), 'channel_sends': len(sends),
    'captured_widths': sorted(set(r['bits'] for r in results)),
    'errors': errors, 'negative_inputs_rejected': all(rejected),
    'disarmed_without_pending_sends': bool(disarmed) and disarmed[-1]['pending'] == 0,
    'results': results,
}
report['passed'] = (bool(results) and len(results) == len(sends) and not errors and
                    report['disarmed_without_pending_sends'] and all(rejected) and
                    all(r['fields_match'] and r['padding_preserved'] and r['index_transition_matches'] for r in results))
with a.output.open('x') as stream:
    json.dump(report, stream, indent=2)
    stream.write('\n')
print(json.dumps({k: v for k, v in report.items() if k != 'results'}, indent=2))
if not report['passed']:
    raise SystemExit(1)
