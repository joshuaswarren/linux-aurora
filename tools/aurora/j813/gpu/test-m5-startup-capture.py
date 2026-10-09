#!/usr/bin/env python3
"""Check startup observer bounds and work/control routing without hardware."""
from pathlib import Path
import struct
import sys
import tempfile
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / 'hv/m1n1/proxyclient'))
from m5_startup_capture import M5StartupCapture


class Fixture(M5StartupCapture):
    BASE = 0x10000000000
    ROOT = 0xc20001a8000
    HARDWARE = 0xfffffc20c0788000

    def __init__(self):
        self.physical = bytearray(0x200000)
        self.views = {}
        super().__init__(self.read_physical, (self.BASE, len(self.physical)),
                         self.BASE, (self.BASE + 0x10000, 0x134000))
        root, main = bytearray(0xb8), bytearray(0x600)
        struct.pack_into('<Q', root, 0, self.VERSION)
        struct.pack_into('<Q', root, 0x18, self.HARDWARE + 0x1e5c0)
        struct.pack_into('<I', root, 0x30, 0x030e4000)
        struct.pack_into('<Q', main, 0, self.HARDWARE)
        self.views[self.ROOT] = root
        self.views[self.HARDWARE + 0x1e5c0] = main
        self.views[self.HARDWARE + 0xe90] = struct.pack('<I', 0x8142)
        for index in (0, 8, 12):
            base = self.HARDWARE + 0x100000 + index * 0x10000
            struct.pack_into('<QQQQ', main, 0x20 + index * 0x20,
                             base, base + 0x10, base + 0x20, base + 0x100)
            for offset, value in [(0, 0), (0x10, 0), (0x20, 1)]:
                self.views[base + offset] = struct.pack('<I', value)
            ring = bytearray(256 * (64 if index == 12 else 24))
            if index == 12:
                struct.pack_into('<I', ring, 0, 0xa)
            else:
                struct.pack_into('<QIHH', ring, 8, 0xfffffc20c0000000, index % 3, 7, 11)
            self.views[base + 0x100] = ring

    def read_physical(self, address, size):
        address ^= 0xa0000000000
        offset = address - self.BASE
        if not 0 <= offset <= len(self.physical) - size:
            raise ValueError('Physical access outside synthetic DRAM')
        return bytes(self.physical[offset:offset + size])

    def read(self, address, size):
        for base, raw in self.views.items():
            if base <= address <= base + len(raw) - size:
                return bytes(raw[address - base:address - base + size])
        raise ValueError('Unmapped synthetic GPU address')


class StartupTests(unittest.TestCase):
    def test_control_and_priority_two_compute_routing(self):
        capture = Fixture()
        with tempfile.TemporaryDirectory() as directory:
            for ordinal, bell, expected in [(0, 0x84000000000011, [12]),
                                            (1, 0x8300000000000a, [12, 8])]:
                result = capture.capture_startup(0x81000000000000 | capture.ROOT,
                    bell, Path(directory) / str(ordinal))
                self.assertNotIn('error', result)
                self.assertEqual([x['index'] for x in result['channels']], expected)
                self.assertEqual(result['channels'][0]['pending_opcodes'], [0xa])
                self.assertTrue(all(x['stable'] for x in result['objects']))
            self.assertEqual(result['channels'][1]['pending_work'][0]['head'], 7)

    def test_event_limit_rejects_without_creating_output(self):
        capture = Fixture()
        capture.events = capture.MAX_EVENTS
        with tempfile.TemporaryDirectory() as directory:
            destination = Path(directory) / 'denied'
            with self.assertRaises(ValueError):
                capture.capture_startup(0x81000000000000 | capture.ROOT,
                    0x84000000000011, destination)
            self.assertFalse(destination.exists())

    def test_command_view_stops_at_its_mapped_page_boundary(self):
        capture = Fixture()
        queue_address = 0xfffffc20c0000000
        pointers_address, items_address = queue_address + 0x4000, queue_address + 0x8000
        command_address = queue_address + 0x13fe0
        queue, pointers = bytearray(0xc0), bytearray(0x80)
        struct.pack_into('<QQ', queue, 0, pointers_address, items_address)
        struct.pack_into('<I', pointers, 0x40, 1)
        struct.pack_into('<I', pointers, 0x60, 16)
        command = bytearray(32)
        struct.pack_into('<I', command, 0, 15)
        capture.views.update({queue_address: queue, pointers_address: pointers,
                              items_address: struct.pack('<Q', command_address),
                              command_address: command})
        with tempfile.TemporaryDirectory() as directory:
            result = capture.capture_startup(0x81000000000000 | capture.ROOT,
                0x83000000000000, Path(directory) / 'page-end')
            work = result['channels'][1]['pending_work'][0]
            self.assertNotIn('capture_error', work)
            self.assertEqual(work['commands'][0]['tag'], 15)
            obj, = [x for x in result['objects'] if x['label'].endswith('-command-0')]
            self.assertEqual(obj['size'], 32)

    def test_invalid_ring_cursor_preserves_error_evidence(self):
        capture = Fixture()
        counter = capture.HARDWARE + 0x100000 + 12 * 0x10000
        capture.views[counter] = struct.pack('<I', 256)
        with tempfile.TemporaryDirectory() as directory:
            result = capture.capture_startup(0x81000000000000 | capture.ROOT,
                0x84000000000011, Path(directory) / 'bad-cursor')
            self.assertIn('ring cursor', result['error'])
            self.assertFalse(result['device_memory_read'])

    def test_cold_ta_ring_prefix_before_first_kick(self):
        capture = Fixture()
        queue_address = 0xfffffc20c0000000
        pointers_address, items_address = queue_address + 0x4000, queue_address + 0x8000
        config_address, kick_address = queue_address + 0x10000, queue_address + 0x14000
        ring_address = queue_address + 0x18000
        queue, pointers = bytearray(0xc0), bytearray(0x80)
        struct.pack_into('<QQ', queue, 0, pointers_address, items_address)
        struct.pack_into('<I', pointers, 0x40, 2)
        struct.pack_into('<I', pointers, 0x60, 16)
        config, kick = bytearray(0x1000), bytearray(0x1000)
        struct.pack_into('<I', config, 0, 15)
        struct.pack_into('<QQH', config, 8, 0x7000438000, ring_address, 0)
        struct.pack_into('<IHHBQI', kick, 0, 14, 0, 1, 0, 1, 0)
        # Only a 4-KiB view exists: a whole-ring read would be rejected.
        ring = bytes(range(256)) * 16
        capture.views.update({queue_address: queue, pointers_address: pointers,
            items_address: struct.pack('<QQ', config_address, kick_address),
            config_address: config, kick_address: kick, ring_address: ring})
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory) / 'first-ta'
            result = capture.capture_startup(0x81000000000000 | capture.ROOT,
                0x83000000000000, output)
            work = result['channels'][1]['pending_work'][0]
            self.assertNotIn('capture_error', work)
            self.assertEqual(work['commands'][0]['sksm']['data_master'], 0)
            self.assertEqual(work['commands'][1]['add_kicks']['stamp'], '0x1')
            obj, = [x for x in result['objects'] if '-sksm-ring-' in x['label']]
            self.assertEqual((output / obj['file']).read_bytes(), ring)
            self.assertTrue(obj['stable'])
            self.assertFalse(result['device_memory_read'])


if __name__ == '__main__':
    unittest.main()
