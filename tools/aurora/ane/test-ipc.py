#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-2.0-only
"""Host tests from recorded channel metadata, not proprietary firmware bytes."""
import importlib.util
from pathlib import Path
import struct
import unittest

spec = importlib.util.spec_from_file_location('ane_ipc', Path(__file__).with_name('decode-ipc.py'))
ipc = importlib.util.module_from_spec(spec)
spec.loader.exec_module(ipc)
BASE = 0x10000380000
SIZE = 0x10000
# J700 local hardware log: j700-ane-power-20260912/m1n1-fw-handshake3.log.
OBSERVED = [
    ('TERMINAL', 2, 0, 512, 0x800), ('IO', 0, 1, 16, 0x8800),
    ('DEBUG', 0, 2, 8, 0x8c00), ('BUF_H2T', 0, 3, 64, 0x8e00),
    ('BUF_T2H', 1, 4, 64, 0x9e00), ('SHAREDMALLOC', 1, 5, 8, 0xae00),
    ('IO_T2H', 1, 6, 64, 0xb000), ('DATA_CHAIN_H2T', 0, 7, 16, 0xc000),
]


def table(base=BASE):
    data = bytearray(8 * ipc.DESCRIPTOR_SIZE)
    for i, (name, direction, source, count, offset) in enumerate(OBSERVED):
        at = i * ipc.DESCRIPTOR_SIZE
        data[at:at + len(name)] = name.encode()
        struct.pack_into('<IIQQ', data, at + 0x40, direction, source, count, base + offset)
    return data


class CapturedProtocol(unittest.TestCase):
    def boot_record(self):
        # Observed 25G83 RAM record and channel table in ram-ane-dapf-test.
        # Only documented metadata is reconstructed; no firmware bytes.
        data = bytearray(0x174)
        struct.pack_into('<13QI', data, 0, 0, 0x100003D0000, 0x380000,
            0xFC80000, 0x100003E4000, 0xF00000, 0, 0, 0, 0,
            0xC441, 0x10000384000, 0x40000, 64)
        return data

    def test_25g83_buffers_and_relocated_channels(self):
        args = ipc.decode_boot_buffers(self.boot_record())
        arena = args['buffers']['ipc']
        channels = ipc.decode_channels(table(arena['iova']), 8, arena['iova'], arena['size'])
        self.assertEqual(channels[1]['iova'], 0x100003D8800)
        self.assertEqual(channels[-1]['iova'], 0x100003DC000)
        self.assertEqual(args['field_00_uninterpreted'], 0)
        self.assertEqual(args['buffers']['command']['iova'], 0x10000384000)

    def test_boot_record_rejects_overlap_alignment_size_and_overflow(self):
        for offset, value in ((8, 0x10000384000), (8, 0x100003D0001),
                              (0x50, 0), (0x50, (1 << 64) - 1),
                              (0x20, 1 << 42), (0x68, 32)):
            data = self.boot_record()
            struct.pack_into('<Q' if offset != 0x68 else '<I', data, offset, value)
            with self.assertRaises(ipc.ProtocolError):
                ipc.decode_boot_buffers(data)
        for data in (self.boot_record()[:-1], self.boot_record() + b'\0'):
            with self.assertRaises(ipc.ProtocolError):
                ipc.decode_boot_buffers(data)

    def test_observed_table_decodes_all_channels_without_mutation(self):
        data = table()
        before = bytes(data)
        channels = ipc.decode_channels(data, 8, BASE, SIZE)
        self.assertEqual([x['name'] for x in channels], [x[0] for x in OBSERVED])
        self.assertEqual(channels[1]['iova'], 0x10000388800)
        self.assertEqual(channels[6]['slots'], 64)
        self.assertEqual(bytes(data), before)

    def test_partial_descriptor_is_rejected(self):
        with self.assertRaises(ipc.ProtocolError):
            ipc.decode_channels(table()[:-1], 8, BASE, SIZE)

    def test_duplicate_names_and_sources_are_rejected(self):
        for location, content in [(0x100, b'TERMINAL\0'), (0x144, struct.pack('<I', 0))]:
            with self.subTest(location=location):
                data = table()
                data[location:location + len(content)] = content
                with self.assertRaises(ipc.ProtocolError):
                    ipc.decode_channels(data, 8, BASE, SIZE)

    def test_ring_ranges_reject_overlap_outside_alignment_and_overflow(self):
        for address in [BASE, BASE + 0x800, BASE - 64, BASE + SIZE, BASE + 0x8801,
                        (1 << 64) - 64]:
            with self.subTest(address=address):
                data = table()
                struct.pack_into('<Q', data, 0x150, address)
                with self.assertRaises(ipc.ProtocolError):
                    ipc.decode_channels(data, 8, BASE, SIZE)

    def test_last_descriptor_is_checked_before_returning_channels(self):
        data = table()
        struct.pack_into('<Q', data, 0x750, BASE + SIZE)
        with self.assertRaises(ipc.ProtocolError):
            ipc.decode_channels(data, 8, BASE, SIZE)

    def test_unsupported_metadata_is_rejected(self):
        for offset, fmt, value in [(0x140, '<I', 3), (0x144, '<I', 32),
                                   (0x148, '<Q', 0), (0x148, '<Q', 513)]:
            with self.subTest(offset=offset, value=value):
                data = table()
                struct.pack_into(fmt, data, offset, value)
                with self.assertRaises(ipc.ProtocolError):
                    ipc.decode_channels(data, 8, BASE, SIZE)

    def test_empty_and_published_slots_are_distinguished(self):
        slot = bytearray(64)
        struct.pack_into('<QQQ', slot, 0, 1, 0, 0)
        self.assertFalse(ipc.decode_slot(slot, BASE, SIZE)['published'])
        struct.pack_into('<QQQ', slot, 0, BASE + 0x800, 12, 0xabcdef)
        result = ipc.decode_slot(slot, BASE, SIZE)
        self.assertTrue(result['published'])
        self.assertEqual(result['metadata'], 0xabcdef)

    def test_second_pointer_flag_is_preserved_without_guessing_semantics(self):
        slot = struct.pack('<QQQ', BASE + 0x802, 12, 0) + bytes(40)
        result = ipc.decode_slot(slot, BASE, SIZE)
        self.assertEqual(result['payload_iova'], BASE + 0x800)
        self.assertEqual(result['flags'], 2)

    def test_published_slot_requires_a_complete_in_range_buffer(self):
        for pointer, length in [(BASE, 0), (BASE + SIZE, 4), (BASE + SIZE - 4, 8),
                                ((1 << 64) - 4, 8)]:
            with self.subTest(pointer=pointer, length=length):
                slot = struct.pack('<QQQ', pointer, length, 0) + bytes(40)
                with self.assertRaises(ipc.ProtocolError):
                    ipc.decode_slot(slot, BASE, SIZE)
        with self.assertRaises(ipc.ProtocolError):
            ipc.decode_slot(bytes(63), BASE, SIZE)


if __name__ == '__main__':
    unittest.main()
