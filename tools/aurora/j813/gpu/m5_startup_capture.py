# SPDX-License-Identifier: MIT
"""Bounded observations of native M5 startup, before mailbox publication."""
import hashlib
import json
from pathlib import Path
import struct

from m5_gpu_capture import M5GPUCapture


class M5StartupCapture(M5GPUCapture):
    MAX_EVENTS = 24
    MAX_TOTAL = 4 * 1024 * 1024

    def __init__(self, read_phys, dram, global_root, firmware_data):
        super().__init__(read_phys, dram, global_root)
        self.firmware_data, size = firmware_data
        if size != 0x134000 or not self.low <= self.firmware_data <= self.high - size:
            raise ValueError('Unexpected M5 reserved GFX DATA extent')
        self.events = 0
        self.total = 0

    def capture_startup(self, init_word, doorbell, output):
        tag = (doorbell >> 48) & 0xff
        if tag not in (0x83, 0x84) or self.events >= self.MAX_EVENTS:
            raise ValueError('Startup capture is bounded to early work/control doorbells')
        if (init_word >> 48) & 0xff != 0x81:
            raise ValueError('Missing this boot initialization message')
        output = Path(output)
        output.mkdir(mode=0o700, parents=True, exist_ok=False)
        self.events += 1
        manifest = dict(phase='before-INBOX1-publication', doorbell=hex(doorbell),
                        ordinal=self.events, objects=[], device_memory_read=False)

        def save(label, address, length, physical=False):
            if self.total + 2 * length > self.MAX_TOTAL:
                raise ValueError('M5 startup read budget exhausted')
            self.total += 2 * length
            read = self._read_dram if physical else self.read
            first, second = read(address, length), read(address, length)
            name = label + '.bin'
            (output / name).write_bytes(first)
            obj = dict(label=label, address=hex(address), size=length, file=name,
                       address_space='physical-DRAM' if physical else 'GPU',
                       sha256=hashlib.sha256(first).hexdigest(), stable=first == second)
            if first != second:
                name = label + '-second.bin'
                (output / name).write_bytes(second)
                obj['second_file'] = name
            manifest['objects'].append(obj)
            return first

        qword = lambda data, offset: struct.unpack_from('<Q', data, offset)[0]
        u32 = lambda data, offset: struct.unpack_from('<I', data, offset)[0]
        try:
            root = save('root', init_word & ((1 << 44) - 1), 0xb8)
            if qword(root, 0) != self.VERSION or u32(root, 0x30) != 0x030e4000:
                raise ValueError('Unexpected M5 root profile')
            main_address = qword(root, 0x18)
            main = save('main', main_address, 0x600)
            hardware = qword(main, 0)
            if main_address - hardware != 0x1e5c0 or u32(self.read(hardware + 0xe90, 4), 0) != 0x8142:
                raise ValueError('Unexpected M5 shared cluster')
            state = {}
            for label, offset in [('pause', 0x128868), ('power', 0x1773e8),
                                  ('idle-policy', 0x177a28), ('resume-state', 0x178458)]:
                raw = save(label, self.firmware_data + offset - 0x4c000, 8, physical=True)
                state[label] = u32(raw, 0)
            manifest['firmware_state'] = state
            channels = []
            indices = [12]
            if tag == 0x83:
                encoded = doorbell & 0xff
                priority, dm = encoded >> 2, encoded & 3
                if priority > 3 or dm > 2:
                    raise ValueError('Unexpected M5 work channel')
                indices.append(priority * 3 + dm)
            for index in indices:
                entry = struct.unpack_from('<QQQQ', main, 0x20 + index * 0x20)
                counters = [u32(save(f'channel-{index}-counter-{i}', pointer, 4), 0)
                            for i, pointer in enumerate(entry[:3])]
                stride = 64 if index == 12 else 24
                ring = save(f'channel-{index}-ring', entry[3], 256 * stride)
                if any(cursor >= 256 for cursor in counters):
                    raise ValueError('Unexpected M5 ring cursor')
                slots = [(counters[0] + i) % 256 for i in range((counters[2] - counters[0]) % 256)]
                channel = dict(index=index, counters=counters, pending_slots=slots)
                if index == 12:
                    channel['pending_opcodes'] = [u32(ring, slot * stride) for slot in slots]
                else:
                    channel['pending_work'] = [dict(slot=slot,
                        queue=hex(qword(ring, slot * stride + 8)),
                        dm=u32(ring, slot * stride + 16),
                        head=struct.unpack_from('<H', ring, slot * stride + 20)[0]) for slot in slots]
                    if self.events <= 8:
                        for work in channel['pending_work'][:4]:
                            label = f'channel-{index}-slot-{work["slot"]}'
                            try:
                                cached = save(label + '-queue', int(work['queue'], 16), 0xc0)
                                uncached = save(label + '-pointers', qword(cached, 0), 0x80)
                                count, consumer, producer = (u32(uncached, off) for off in (0x60, 0, 0x40))
                                if not 0 < count <= 4096 or not 0 <= consumer < count or not 0 <= producer < count:
                                    raise ValueError('Unexpected M5 inner queue geometry')
                                pending = (producer - consumer) % count
                                work.update(consumer=consumer, producer=producer, count=count,
                                            pending_commands=pending, commands=[])
                                for i in range(min(pending, 8)):
                                    slot = (consumer + i) % count
                                    raw = save(label + f'-item-{slot}', qword(cached, 8) + slot * 8, 8)
                                    address = qword(raw, 0)
                                    length = min(0x1000, 0x4000 - (address & 0x3fff))
                                    command = save(label + f'-command-{slot}', address, length)
                                    info = dict(slot=slot, address=hex(address), tag=u32(command, 0))
                                    work['commands'].append(info)
                                    if info['tag'] == 15 and len(command) >= 0x26:
                                        ring_low, ring_high = qword(command, 8), qword(command, 0x10)
                                        qid = struct.unpack_from('<H', command, 0x18)[0]
                                        priority, data_master = u32(command, 0x1e), u32(command, 0x22)
                                        if (not ring_high or ring_high & 0x3fff
                                                or qid > 127 or priority > 3 or data_master > 2):
                                            raise ValueError('Unexpected native SKSM configuration geometry')
                                        info['sksm'] = dict(queue_id=qid, priority=priority,
                                            data_master=data_master, ring_low=hex(ring_low),
                                            ring_high=hex(ring_high), captured_prefix_bytes=0x1000)
                                        # Capture the first page before the first doorbell,
                                        # including a cold ring's first entries. Do not infer
                                        # TA entry stride from the CL-only host breakpoint.
                                        save(label + f'-sksm-ring-{slot}-prefix', ring_high, 0x1000)
                                    elif info['tag'] == 14 and len(command) >= 21:
                                        _, qid, count, priority, stamp, data_master = struct.unpack_from(
                                            '<IHHBQI', command)
                                        info['add_kicks'] = dict(queue_id=qid, count=count,
                                            priority=priority, stamp=hex(stamp), data_master=data_master)
                            except (RuntimeError, ValueError) as error:
                                work['capture_error'] = str(error)
                channels.append(channel)
            manifest['channels'] = channels
        except (RuntimeError, ValueError) as error:
            manifest['error'] = str(error)
        finally:
            manifest['total_bytes_read'] = self.total
            (output / 'manifest.json').write_text(json.dumps(manifest, indent=2) + '\n')
        return manifest
