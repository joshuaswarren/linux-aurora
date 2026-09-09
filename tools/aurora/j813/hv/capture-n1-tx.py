"""Bounded native TX recording using existing alpha descriptors and mappings.

Record at WSYNC before each original doorbell is forwarded. Learn TX ring
addresses only from native create messages; stop on unknown active rings,
DHCP, TR25, a read/format error, or the event budget. Payloads remain private.
"""
import json
import os
from pathlib import Path
import runpy
import struct
import time

N1TxStop = runpy.run_path(str(Path(__file__).with_name('stop-n1-tx.py')))['N1TxStop']


def decode_tx_config(payload):
    if len(payload) < 0x34 or payload[0] != 1:
        return None
    # Exact native create format: 16-byte physical descriptor, 16-byte
    # inline footer, no slot-size scaling. Other ring layouts are not read.
    if payload[2] != 4 or payload[3] != 0:
        return None
    ring = struct.unpack_from('<H', payload, 4)[0]
    address = struct.unpack_from('<Q', payload, 8)[0]
    slots, completion, associated, flags = struct.unpack_from('<HHHH', payload, 0x18)
    if (ring < 24 or ring >= 192 or not address or address & 7
            or not 0 < slots <= 8192 or associated != 0x12 or flags != 8):
        return None
    return dict(ring=ring, address=address, slots=slots, stride=32,
                completion=completion, associated=associated, flags=flags,
                create_hex=payload.hex())


def packet_kind(payload):
    if len(payload) < 14:
        return 'short-ethernet'
    ethertype = int.from_bytes(payload[12:14], 'big')
    if ethertype == 0x0806:
        return 'ARP'
    if ethertype == 0x888e:
        return 'EAPOL'
    if ethertype == 0x0800 and len(payload) >= 34:
        ip = payload[14:]
        header = (ip[0] & 15) * 4
        if ip[0] >> 4 == 4 and header >= 20 and len(ip) >= header + 8 and ip[9] == 17:
            ports = struct.unpack_from('!HH', ip, header)
            if ports in ((68, 67), (67, 68)):
                return 'DHCPv4'
            return 'IPv4-UDP-%d-%d' % ports
        return 'IPv4'
    if ethertype == 0x86dd and len(payload) >= 54:
        ip = payload[14:]
        if ip[6] == 17 and len(ip) >= 48:
            return 'IPv6-UDP-%d-%d' % struct.unpack_from('!HH', ip, 40)
        return 'IPv6'
    return f'Ethernet-{ethertype:04x}'


class N1TxRecorder(N1TxStop):
    def __init__(self, hv, read_context, stream=2, max_events=128):
        if not 1 <= max_events <= 128:
            raise ValueError('Requires a bounded event budget')
        self.events, self.max_events, self.configs = 0, max_events, {}
        self.last_event = None
        super().__init__(hv, read_context, stream)

    def write(self, evt):
        if self.hit:
            return
        if (int(evt.addr) != self.address or int(evt.flags.WIDTH) != 2
                or not evt.flags.WRITE or evt.flags.MULTI):
            raise RuntimeError('Unexpected native alpha TX doorbell access')
        self.events += 1
        record = dict(time_ns=time.time_ns(), event=self.events,
                      pc=int(evt.pc), address=int(evt.addr), value=int(evt.data),
                      original_write_pending=True, packets=[], captures=[])
        reasons = []
        try:
            context = self.read_context(self.hv, 1, self.stream, message_payloads=True)
            record['captures'].append(str(context))
            captures = {c['label']: bytes.fromhex(c['hex'])
                        for c in json.loads(context.read_text())['captures']}
            for label, payload in captures.items():
                if label.startswith('message-tr-payload-'):
                    config = decode_tx_config(payload)
                    if config:
                        self.configs[config['ring']] = config
            heads = struct.unpack('<192H', captures['index-3'])
            tails = struct.unpack('<192H', captures['index-1'])
            active = [ring for ring in range(24, 192) if heads[ring] != tails[ring]]
            record['active_rings'] = active
            record['configs'] = self.configs
            unknown = [ring for ring in active if ring not in self.configs]
            if unknown:
                reasons.append(f'Unknown active native rings: {unknown}')
            if 25 in active:
                reasons.append('Native TR25 submission')
            slots_to_read = []
            for ring in active:
                if ring not in self.configs:
                    continue
                config = self.configs[ring]
                slots = config['slots']
                if not (heads[ring] < slots and tails[ring] < slots):
                    raise RuntimeError('Native ring indices exceed the configured size')
                pending = (heads[ring] - tails[ring]) % slots
                if pending > 16:
                    reasons.append(f'Ring {ring}: capture bounded to first 16 of {pending}')
                for offset in range(min(pending, 16)):
                    slot = (tails[ring] + offset) % slots
                    slots_to_read.append((f'TR{ring}-slot{slot}',
                                          config['address'] + slot * 32, 32))
            if len(slots_to_read) > 32:
                raise RuntimeError('Native TX event exceeds 32-descriptor capture bound')
            if slots_to_read:
                path = self.read_context(self.hv, 1, self.stream, extra_reads=slots_to_read)
                record['captures'].append(str(path))
                descriptors = {c['label']: bytes.fromhex(c['hex'])
                               for c in json.loads(path.read_text())['captures']
                               if c['label'].startswith('TR')}
                requests = []
                for label, raw in descriptors.items():
                    flags, address, tag, chain = struct.unpack_from('<IQHH', raw)
                    length = flags >> 8
                    if flags & 0xff != 3 or not 14 <= length <= 4096 or chain:
                        raise RuntimeError(f'Unexpected bounded TX descriptor at {label}')
                    requests.append((label + '-payload', address, length))
                path = self.read_context(self.hv, 1, self.stream, extra_reads=requests)
                record['captures'].append(str(path))
                for capture in json.loads(path.read_text())['captures']:
                    if not capture['label'].startswith('TR'):
                        continue
                    raw = bytes.fromhex(capture['hex'])
                    kind = packet_kind(raw)
                    record['packets'].append(dict(label=capture['label'],
                                                  kind=kind, length=len(raw)))
                    if kind == 'DHCPv4':
                        reasons.append('Native DHCPv4 payload')
        except Exception as error:
            record['error'] = f'{type(error).__name__}: {error}'
            reasons.append('Read or format error')
        if self.events >= self.max_events:
            reasons.append('Event budget reached')
        record['stop_reasons'] = reasons
        directory = Path(__file__).resolve().parent / 'logs/n1-dma'
        directory.mkdir(mode=0o700, parents=True, exist_ok=True)
        os.chmod(directory, 0o700)
        self.last_event = directory / f'tx-event-{record["time_ns"]}.json'
        with open(os.open(self.last_event, os.O_WRONLY | os.O_CREAT | os.O_EXCL,
                          0o600), 'w') as output:
            json.dump(record, output, indent=2)
            output.write('\n')
        print('M5_N1_TX_EVENT', self.events, 'packets', len(record['packets']),
              'stop', bool(reasons), 'private', self.last_event)
        if reasons:
            print('M5_N1_TX_CAPTURE_REASON', '; '.join(reasons))
            super().write(evt)
