#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""Protect the firmware root selector and reject accidental MMIO reads."""
from pathlib import Path
import struct
import sys
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / 'hv/m1n1/proxyclient'))
from m5_gpu_capture import M5GPUCapture


class WalkerTests(unittest.TestCase):
    def setUp(self):
        self.low = 0x10000000000
        self.root, self.l2, self.l3, self.page = [self.low + n * 0x4000 for n in range(1, 5)]
        self.memory = {address: bytearray(0x4000) for address in (self.root, self.l2, self.l3, self.page)}
        # The actual M5 firmware-kernel address keeps root bit42 and uses
        # L1 index2, L2 index0, L3 index0x6a.
        self.pointer = 0xfffffc20001a8000
        struct.pack_into('<Q', self.memory[self.root], 2 * 8, self.l2 | 3)
        struct.pack_into('<Q', self.memory[self.l2], 0, self.l3 | 3)
        struct.pack_into('<Q', self.memory[self.l3], 0x6a * 8, self.page | 3)
        self.memory[self.page][:8] = b'M5-root!'
        self.accesses = []

        def read_nc(address, size):
            physical = address ^ 0xa0000000000
            self.accesses.append(physical)
            base = physical & ~0x3fff
            return bytes(self.memory[base][physical - base:physical - base + size])
        self.capture = M5GPUCapture(read_nc, (self.low, 0x400000000), self.root)

    def test_global_kernel_pointer(self):
        self.assertEqual(self.capture.read(self.pointer, 8), b'M5-root!')
        self.assertEqual(self.accesses, [self.root + 16, self.l2, self.l3 + 0x6a * 8, self.page])

    def test_losing_root_selector_is_rejected(self):
        with self.assertRaisesRegex(ValueError, 'root-select'):
            self.capture.read(self.pointer & ((1 << 40) - 1), 8)
        self.assertEqual(self.accesses, [])

    def test_mapping_walk_retains_physical_identity_across_page_boundary(self):
        other = self.page + 0x8000
        struct.pack_into('<Q', self.memory[self.l3], 0x6b * 8, other | 3)
        views = self.capture.mapping(self.pointer + 0x3ff0, 0x30)
        self.assertEqual([(x['physical'], x['size']) for x in views],
                         [(hex(self.page + 0x3ff0), 0x10), (hex(other), 0x20)])
        self.assertTrue(all(len(x['walk']) == 4 for x in views))
        # Mapping descriptions read only the tables, never the payload.
        self.assertNotIn(self.page, self.accesses)
        self.assertNotIn(other, self.accesses)
        struct.pack_into('<Q', self.memory[self.l3], 0x6b * 8, 0x580000003)
        with self.assertRaisesRegex(ValueError, 'non-DRAM'):
            self.capture.mapping(self.pointer + 0x3ff0, 0x30)

    def test_invalid_mapping_cannot_reach_mmio(self):
        struct.pack_into('<Q', self.memory[self.l3], 0x6a * 8, 0x580000003)
        with self.assertRaisesRegex(ValueError, 'non-DRAM'):
            self.capture.read(self.pointer, 8)
        self.assertNotIn(0x580000000, self.accesses)

    def test_missing_intermediate_mapping(self):
        self.memory[self.l2][:8] = bytes(8)
        with self.assertRaisesRegex(RuntimeError, 'unmapped'):
            self.capture.read(self.pointer, 8)
        self.assertNotIn(self.page, self.accesses)

    def test_context_table_is_selected_without_global_fallback(self):
        region = self.low
        self.memory[region] = bytearray(0x4000)
        struct.pack_into('<Q', self.memory[region], 3 * 16 + 8, self.root | 1)
        self.assertEqual(self.capture.read_context(self.pointer, 8, 3, region), b'M5-root!')
        self.assertEqual(self.accesses[0], region + 3 * 16 + 8)
        with self.assertRaisesRegex(RuntimeError, 'unmapped'):
            self.capture.read_context(self.pointer, 8, 2, region)
        self.accesses.clear()
        with self.assertRaisesRegex(ValueError, 'Invalid M5 context'):
            self.capture.read_context(self.pointer, 8, 256, region)
        self.assertEqual(self.accesses, [])
        with self.assertRaisesRegex(ValueError, 'non-DRAM'):
            self.capture.read_context(self.pointer, 8, 3, 0x580000000)
        self.assertEqual(self.accesses, [])



if __name__ == '__main__':
    unittest.main()
