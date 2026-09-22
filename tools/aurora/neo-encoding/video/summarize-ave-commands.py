#!/usr/bin/env python3
"""Validate captured buffer hashes and pair IPC sends with channel sends.

No hardware access. A pair is accepted only for adjacent command events with
the exact derived firmware address and length. This avoids guessing across
interleaved submissions. The report does not establish completion semantics.
"""
import argparse
import collections
import hashlib
import json
from pathlib import Path
import struct

parser = argparse.ArgumentParser()
parser.add_argument('run', type=Path)
parser.add_argument('output', type=Path)
parser.add_argument('--phase', help='Validate one complete phase before a later capture limit')
args = parser.parse_args()
events = [json.loads(line) for line in (args.run / 'ave-commands.jsonl').read_text().splitlines()]
all_errors = [e for e in events if e['kind'] in ('capture-error', 'limit-reached')]
errors = [e for e in all_errors if args.phase is None or e.get('phase')==args.phase]
commands = [e for e in events if e['kind'] == 'command' and (args.phase is None or e.get('phase')==args.phase)]
names = {1: 'config', 2: 'halt', 3: 'open', 4: 'close', 6: 'start',
         7: 'stop', 8: 'process', 9: 'complete', 10: 'priority'}
pairs = []
unpaired = []
for index, event in enumerate(commands):
    if event['function'] != 'ipc-send':
        if index == 0 or commands[index - 1]['function'] != 'ipc-send':
            unpaired.append(event['seq'])
        continue
    data = (args.run / 'ave-command-buffers' / event['buffer']).read_bytes()
    if len(data) != event['length'] or hashlib.sha256(data).hexdigest() != event['sha256']:
        raise SystemExit(f'Buffer integrity failure at event {event["seq"]}')
    opcode = struct.unpack_from('<H', data)[0]
    if opcode != event['opcode'] & 0xffff:
        raise SystemExit(f'Opcode mismatch at event {event["seq"]}')
    next_event = commands[index + 1] if index + 1 < len(commands) else None
    matched = (next_event is not None and next_event['function'] == 'channel-send' and
               next_event['length'] == event['length'] and
               int(next_event['firmware_address'], 16) == int(event['expected_firmware_address'], 16))
    if not matched:
        unpaired.append(event['seq'])
    pairs.append({'ipc_seq': event['seq'], 'channel_seq': next_event['seq'] if matched else None,
                  'phase': event['phase'], 'opcode': opcode,
                  'name_from_static_builder': names.get(opcode, 'unknown'),
                  'channel': event['channel'], 'bytes': len(data),
                  'firmware_address': event['expected_firmware_address'],
                  'flags': next_event['flags'] if matched else None,
                  'buffer': event['buffer'], 'sha256': event['sha256'],
                  'header_u32': [hex(x) for x in struct.unpack_from('<'+'I'*(min(len(data), 0x48)//4), data)]})
report = {'claim': 'Observed macOS AVE command transport; no Linux submission or reply decoding',
          'selected_phase':args.phase,
          'errors_outside_selected_phase':[e for e in all_errors if e not in errors],
          'run': str(args.run.resolve()), 'capture_errors': errors,
          'armed': any(e['kind'] == 'armed' for e in events),
          'disarmed': any(e['kind'] == 'disarmed' for e in events),
          'command_events': len(commands), 'ipc_sends': len(pairs),
          'matched_sends': sum(p['channel_seq'] is not None for p in pairs),
          'unpaired_event_sequences': unpaired,
          'opcode_counts': dict(collections.Counter(p['opcode'] for p in pairs)),
          'channel_opcode_counts': {
              str(channel): dict(collections.Counter(p['opcode'] for p in pairs if p['channel'] == channel))
              for channel in sorted(set(p['channel'] for p in pairs))},
          'pairs': pairs}
report['transport_validation_passed'] = bool(pairs) and report['armed'] and report['disarmed'] and not errors and not unpaired
if args.phase is not None:
    # A phase truncated in the middle is not usable even if its last sampled
    # command happened to have a matching transport send.
    report['phase_session_closed']=any(p['channel']==2 and p['opcode']==4 for p in pairs)
    report['transport_validation_passed'] &= report['phase_session_closed']
with args.output.open('x') as stream:
    json.dump(report, stream, indent=2)
    stream.write('\n')
print(json.dumps({k: v for k, v in report.items() if k not in ('pairs', 'run')}, indent=2))
if not report['transport_validation_passed']:
    raise SystemExit(1)
