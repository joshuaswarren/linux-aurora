#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""Corrupt synthetic target evidence to test rejection, without hardware claims."""
import importlib.util
from pathlib import Path
import struct
import unittest

spec = importlib.util.spec_from_file_location('pb_fixture',
    Path(__file__).with_name('test-m5-parameter-buffer-capture.py'))
pb = importlib.util.module_from_spec(spec)
spec.loader.exec_module(pb)
verify = pb.verify


def fixture():
    objects, manifest = pb.fixture()
    roots = objects['context-root-table'][1]

    def mapping(address, physical=None):
        entry = struct.unpack_from('<Q', roots, 4 * 16)[0]
        walk = [[hex(0x800000000 + 4 * 16), hex(entry)]]
        table = entry & 0x0000ffffffffc000
        for shift, count, value in [(36, 64, 0x820000003), (25, 2048, 0x830000003),
                                     (14, 2048, 0 if physical is None else physical | 3)]:
            walk.append([hex(table + ((address >> shift) & (count - 1)) * 8), hex(value)])
            table = value & 0x0000ffffffffc000
        return walk

    base = 0x1000000000
    addresses = [base + 0x2000000 + i * 0x10000 for i in range(6)]
    tilemap, tpc, deflake, ta_status, fragment_status, auxiliary = addresses
    tr = [(0x1c039, tilemap - base), (0x1c9c8, tilemap - base),
          (0x1c079, tilemap - base + 0x1000), (0x1c0a1, tpc - base),
          (0x1c950, deflake - base), (0x10111, deflake - base + 0x340),
          (0x10119, deflake - base + 0x40), (0x1c880, deflake - base + 0x340),
          (0x14318, ta_status | 1)]
    fr = [(0x15211, (256 << 32) | 256), (0x16098, tilemap + 0x1000),
          (0x14080, fragment_status | 1), (0x16461, auxiliary), (0x16090, auxiliary)]
    for label, pairs, offset, count in [('graphics-ta', tr, 0x60, 82),
                                        ('graphics-fragment', fr, 0xa0, 90)]:
        raw = bytearray(offset + 12 * count)
        for i, (register, value) in enumerate(pairs):
            struct.pack_into('<IQ', raw, offset + 12 * i, register, value)
        objects[label] = ({}, raw)
    proofs = []
    for i, (name, address, prefix) in enumerate(zip(
            ['tilemap', 'tpc', 'deflake', 'ta-status', 'fragment-status', 'auxiliary'],
            addresses, [0x1200, 0x100, 0x1000, 0x100, 0x100, 0x800])):
        size = 0x8000 if name == 'auxiliary' else 0x4000
        physical = 0x870000000 + i * 0x10000
        label = 'graphics-target-' + name
        raw = bytearray(prefix)
        if name == 'deflake':
            for off, value in [(0x9c0, 0x3000), (0x9c4, 0x80000007), (0x9c8, 7),
                               (0x9d0, 0x43000000), (0x9d4, 0x43000000),
                               (0x9d8, 0x43000000), (0x9dc, 0xc3000000), (0x9e4, 0x3f800000)]:
                struct.pack_into('<I', raw, off, value)
        objects[label] = (dict(address=hex(address), context=4,
            first_mapping=dict(physical=hex(physical), walk=mapping(address, physical))), raw)
        pages = [dict(address=hex(address + off), physical=hex(physical + off),
                      walk=mapping(address + off, physical + off))
                 for off in range(0, size, 0x4000)]
        guards = [dict(address=hex(address + size + off), walk=mapping(address + size + off))
                  for off in (0, 0x4000)]
        proofs.append(dict(label=label, address=hex(address), size=size, context=4,
                           pages=pages, guards=guards))
    manifest['graphics_target_spans'] = proofs
    return objects, manifest


class RenderTargetTests(unittest.TestCase):
    def setUp(self):
        self.objects, self.manifest = fixture()

    def check(self):
        return verify.verify_render_targets(self.objects, self.manifest, 4)

    def test_complete_owned_targets(self):
        result = self.check()
        self.assertEqual(result['unmapped_guard_pages'], 12)
        self.assertEqual(result['mapped_pages'], 7)
        self.assertTrue(result['m5_viewport_verified'])

    def test_neo_viewport_header(self):
        struct.pack_into('<I', self.objects['graphics-target-deflake'][1], 0x9c0, 0xc00)
        with self.assertRaisesRegex(ValueError, 'viewport'):
            self.check()

    def test_stale_heapmeta_descriptor(self):
        struct.pack_into('<Q', self.objects['graphics-fragment'][1], 0xa0 + 12 + 4, 0x1000261000)
        with self.assertRaisesRegex(ValueError, 'descriptors disagree'):
            self.check()

    def test_foreign_context(self):
        self.objects['graphics-target-tpc'][0]['context'] = 3
        with self.assertRaisesRegex(ValueError, 'different allocation'):
            self.check()

    def test_forged_root_walk(self):
        self.manifest['graphics_target_spans'][0]['pages'][0]['walk'][0][1] = '0x811000001'
        with self.assertRaisesRegex(ValueError, 'root'):
            self.check()

    def test_mapped_guard(self):
        self.manifest['graphics_target_spans'][2]['guards'][1]['walk'][-1][1] = '0x870000003'
        with self.assertRaisesRegex(ValueError, 'guard is mapped'):
            self.check()

    def test_missing_auxiliary_page(self):
        self.manifest['graphics_target_spans'][-1]['pages'].pop()
        with self.assertRaisesRegex(ValueError, 'Missing'):
            self.check()

    def test_capture_backing_disagrees(self):
        self.objects['graphics-target-tpc'][0]['first_mapping']['physical'] = '0x880000000'
        with self.assertRaisesRegex(ValueError, 'mapping disagree'):
            self.check()

    def test_shares_parameter_backing(self):
        self.manifest['graphics_pb_spans'][0]['pages'][0]['physical'] = '0x870000000'
        with self.assertRaisesRegex(ValueError, 'private backing'):
            self.check()

    def test_parameter_buffer_consumes_target_guard(self):
        self.manifest['graphics_pb_spans'][0]['address'] = hex(0x1002004000)
        with self.assertRaisesRegex(ValueError, 'overlap'):
            self.check()


if __name__ == '__main__':
    unittest.main()
