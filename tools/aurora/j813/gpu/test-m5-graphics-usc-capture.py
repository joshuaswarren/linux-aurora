#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""Synthetic fault tests for USC capture verification; no hardware claims."""
import importlib.util
from pathlib import Path
import struct
import unittest

spec = importlib.util.spec_from_file_location('graphics_verify',
    Path(__file__).with_name('verify-m5-linux-graphics.py'))
verify = importlib.util.module_from_spec(spec)
spec.loader.exec_module(verify)


def fixture(directory=(1 << 42) - 0x8000 - (32 << 30)):
    context = 4
    freelist = directory + 0x408000
    blocks = [directory + 0x420000 + i * 0x208000 for i in range(19)]
    state = bytearray(0x400)
    struct.pack_into('<Q', state, 0x14, directory)
    struct.pack_into('<Q', state, 0x30, freelist)
    units = 9728
    entries = [base + i * 4096 for base in blocks for i in range(512)] + [0]
    root_table = 0x800000000
    roots = bytearray(0x400)
    root = (context << 48) | 0x810000001
    struct.pack_into('<Q', roots, context * 16, root)
    objects = {}

    def add(label, address, data, ctx=0):
        objects[label] = (dict(address=hex(address), context=ctx), data)

    add('context-root-table', root_table, roots, None)
    add('graphics-usc-state', 0xfffffc20c101c000, state)
    add('graphics-uma-slot', 0xfffffc20015e8020,
        struct.pack('<4Q', directory >> 7, units << 32, (0x80000 << 32) | units, 0))
    add('graphics-usc-directory', directory, struct.pack('<9729Q', *entries), context)
    add('graphics-usc-free-blocks', freelist, bytes(0x4000), context)
    guards = [directory + 0x400000, directory + 0x404000, freelist + 0x4000]
    for base in blocks:
        guards.extend((base + 0x200000, base + 0x204000))
    proofs = []
    for address in guards:
        walk = [[hex(root_table + context * 16), hex(root)]]
        table = root & 0x0000ffffffffc000
        for shift, count, entry in ((36, 64, 0x820000003),
                                    (25, 2048, 0x830000003), (14, 2048, 0)):
            walk.append([hex(table + ((address >> shift) & (count - 1)) * 8), hex(entry)])
            table = entry & 0x0000ffffffffc000
        proofs.append(dict(address=hex(address), context=context, walk=walk))
    return objects, dict(graphics_usc_guards=proofs)


class UscCaptureTests(unittest.TestCase):
    def setUp(self):
        self.objects, self.manifest = fixture()

    def check(self):
        return verify.verify_usc_pool(self.objects, self.manifest, 4)

    def test_valid_relocated_records(self):
        result = self.check()
        self.assertEqual(result['unmapped_guard_pages'], 41)
        self.assertEqual(result['capacity_bytes'], 39845888)

    def test_application_arena_accepts_auxiliary_storage(self):
        objects, manifest = fixture(0x1100000000)
        result = verify.verify_usc_pool(objects, manifest, 4, 'application')
        self.assertEqual(result['reserved_start'], '0x1100000000')

    def test_application_arena_rejects_compact_storage(self):
        objects, manifest = fixture(0x1000000000)
        with self.assertRaisesRegex(ValueError, 'reserved'):
            verify.verify_usc_pool(objects, manifest, 4, 'application')

    def test_reservation_profiles_do_not_silently_accept_each_other(self):
        with self.assertRaisesRegex(ValueError, 'reserved'):
            verify.verify_usc_pool(self.objects, self.manifest, 4, 'application')
        objects, manifest = fixture(0x1100000000)
        with self.assertRaisesRegex(ValueError, 'reserved'):
            verify.verify_usc_pool(objects, manifest, 4)

    def test_stale_uma_pointer(self):
        item, data = self.objects['graphics-uma-slot']
        data = bytearray(data)
        data[0] ^= 1
        self.objects['graphics-uma-slot'] = (item, data)
        with self.assertRaisesRegex(ValueError, 'descriptor'):
            self.check()

    def test_completed_ring_cursors(self):
        item, data = self.objects['graphics-uma-slot']
        data = bytearray(data)
        struct.pack_into('<Q', data, 8, ((9728 + 2256) << 32) | 2256)
        self.objects['graphics-uma-slot'] = (item, data)
        item, data = self.objects['graphics-usc-directory']
        data = bytearray(data)
        data[-8:] = data[:8]
        self.objects['graphics-usc-directory'] = (item, data)
        self.assertEqual(self.check()['unmapped_guard_pages'], 41)

    def test_missing_returned_unit(self):
        item, data = self.objects['graphics-uma-slot']
        data = bytearray(data)
        struct.pack_into('<Q', data, 8, (9728 << 32) | 1)
        self.objects['graphics-uma-slot'] = (item, data)
        with self.assertRaisesRegex(ValueError, 'descriptor'):
            self.check()

    def test_foreign_returned_unit(self):
        item, data = self.objects['graphics-usc-directory']
        data = bytearray(data)
        struct.pack_into('<Q', data, len(data) - 8, 0x1000)
        self.objects['graphics-usc-directory'] = (item, data)
        with self.assertRaisesRegex(ValueError, 'outside the owned pool'):
            self.check()

    def test_noncontiguous_directory(self):
        item, data = self.objects['graphics-usc-directory']
        data = bytearray(data)
        data[8] ^= 1
        self.objects['graphics-usc-directory'] = (item, data)
        with self.assertRaisesRegex(ValueError, '2 MiB'):
            self.check()

    def test_outside_reserved_region(self):
        self.objects, self.manifest = fixture((1 << 42) - 0x8000)
        with self.assertRaisesRegex(ValueError, 'reserved driver VA'):
            self.check()

    def test_mapped_guard(self):
        self.manifest['graphics_usc_guards'][0]['walk'][-1][1] = '0x840000003'
        with self.assertRaisesRegex(ValueError, 'guard page is mapped'):
            self.check()

    def test_foreign_root(self):
        self.manifest['graphics_usc_guards'][0]['walk'][0][1] = '0x5000810000001'
        with self.assertRaisesRegex(ValueError, 'different context root'):
            self.check()

    def test_wrong_walk_slot(self):
        self.manifest['graphics_usc_guards'][0]['walk'][2][0] = '0x900000000'
        with self.assertRaisesRegex(ValueError, 'different page table'):
            self.check()

    def test_missing_guard(self):
        self.manifest['graphics_usc_guards'].pop()
        with self.assertRaisesRegex(ValueError, 'Missing USC guard'):
            self.check()


if __name__ == '__main__':
    unittest.main()
