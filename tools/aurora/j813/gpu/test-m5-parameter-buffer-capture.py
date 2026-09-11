#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""Fault-injection tests for retained PB evidence, without hardware claims."""
import importlib.util
from pathlib import Path
import struct
import unittest

spec = importlib.util.spec_from_file_location('graphics_verify',
    Path(__file__).with_name('verify-m5-linux-graphics.py'))
verify = importlib.util.module_from_spec(spec)
spec.loader.exec_module(verify)


def fixture():
    objects = {}
    roots = bytearray(0x400)
    struct.pack_into('<Q', roots, 8, 0x811000001)
    struct.pack_into('<Q', roots, 4 * 16, (4 << 48) | 0x810000001)
    objects['context-root-table'] = (dict(address='0x800000000', context=None), roots)

    def mapping(address, ctx, physical=None):
        root_index = (address >> 42) & 1
        entry = struct.unpack_from('<Q', roots, ctx * 16 + root_index * 8)[0]
        walk = [[hex(0x800000000 + ctx * 16 + root_index * 8), hex(entry)]]
        table = entry & 0x0000ffffffffc000
        for shift, count, value in [(36, 64, 0x820000003), (25, 2048, 0x830000003),
                                     (14, 2048, 0 if physical is None else physical | 3)]:
            walk.append([hex(table + ((address >> shift) & (count - 1)) * 8), hex(value)])
            table = value & 0x0000ffffffffc000
        return walk

    def add(label, address, data, ctx=0, physical=None):
        item = dict(address=hex(address), context=ctx)
        if physical is not None:
            item['first_mapping'] = dict(physical=hex(physical), walk=mapping(address, ctx, physical))
        objects[label] = (item, data)

    blocks = [0x1001000000 + i * 0x100000 for i in range(3)]
    page_list, metrics, scratch, discard = [0x1001400000 + i * 0x40000 for i in range(4)]
    state, scene = bytearray(0x400), bytearray(0x80)
    for off, value in [(0x20, 0xfffffc20c1040000), (0x28, page_list),
                       (0x44, 0xfffffc20c1018000), (0x84, discard - 0x1000000000)]:
        struct.pack_into('<Q', state, off, value)
    for off, value in [(0x30, 0x1c000), (0x34, 48), (0x3c, 3), (0x58, 0x80000)]:
        struct.pack_into('<I', state, off, value)
    for off, value in [(0, metrics + 0x40), (8, 0xfffffc20c1060040),
                       (0x28, scratch - 0x1000000000 + 0x40)]:
        struct.pack_into('<Q', scene, off, value)
    bases = [(b - 0x1000000000) >> 15 for b in blocks]
    pages = struct.pack('<48I', *(b + p for b in bases for p in range(16)))
    add('graphics-pm-state', 0xfffffc20c100c000, state)
    add('graphics-pb-slot', 0xfffffc20015e0000,
        struct.pack('<4I', (page_list >> 4) & 0xfffffff8, 48, 33, 33))
    add('graphics-pb-scene', 0xfffffc20c1030080, scene)
    add('graphics-pb-blocks', 0xfffffc20c1018000, struct.pack('<6I', bases[0], 0, bases[1], 0, bases[2], 0))
    add('graphics-pb-pages', page_list, pages, 4, 0x840000000)
    add('graphics-pb-pages-firmware', 0xfffffc20c1040000, pages, 0, 0x840000000)
    add('graphics-pb-metrics', metrics, bytes(0x100), 4, 0x841000000)
    add('graphics-pb-metrics-firmware', 0xfffffc20c1060000, bytes(0x100), 0, 0x841000000)
    spans = [(b, 0x80000) for b in blocks] + [(page_list, 0x1c000),
             (metrics, 0x4000), (scratch, 0x4000), (discard, 0x8000)]
    proofs = []
    for base, size in spans:
        pages = []
        for off in range(0, size, 0x4000):
            physical = 0x850000000 + base - blocks[0] + off
            if base == page_list:
                physical = 0x840000000 + off
            elif base == metrics:
                physical = 0x841000000 + off
            pages.append(dict(address=hex(base + off), physical=hex(physical),
                              walk=mapping(base + off, 4, physical)))
        guards = [dict(address=hex(base + size + off), walk=mapping(base + size + off, 4))
                  for off in (0, 0x4000)]
        proofs.append(dict(address=hex(base), size=size, context=4, pages=pages, guards=guards))
    return objects, dict(graphics_pb_spans=proofs)


class ParameterBufferTests(unittest.TestCase):
    def setUp(self):
        self.objects, self.manifest = fixture()

    def check(self):
        return verify.verify_parameter_buffer(self.objects, self.manifest, 4)

    def test_complete_owned_buffer(self):
        result = self.check()
        self.assertEqual(result['unmapped_guard_pages'], 14)
        self.assertEqual(result['mapped_pages'], 107)

    def test_foreign_page_list_entry(self):
        for name in ('graphics-pb-pages', 'graphics-pb-pages-firmware'):
            item, raw = self.objects[name]
            self.objects[name] = item, struct.pack('<I', 0xffffffff) + raw[4:]
        with self.assertRaisesRegex(ValueError, 'owned blocks'):
            self.check()

    def test_stale_hardware_directory(self):
        item, raw = self.objects['graphics-pb-slot']
        self.objects['graphics-pb-slot'] = item, struct.pack('<I', 0x23800) + raw[4:]
        with self.assertRaisesRegex(ValueError, 'descriptor'):
            self.check()

    def test_alias_backing_mismatch(self):
        self.objects['graphics-pb-pages-firmware'][0]['first_mapping']['physical'] = '0x860000000'
        with self.assertRaisesRegex(ValueError, 'different physical'):
            self.check()

    def test_guard_mapped(self):
        self.manifest['graphics_pb_spans'][0]['guards'][0]['walk'][-1][1] = '0x840000003'
        with self.assertRaisesRegex(ValueError, 'guard is mapped'):
            self.check()

    def test_missing_data_page(self):
        self.manifest['graphics_pb_spans'][0]['pages'].pop()
        with self.assertRaisesRegex(ValueError, 'Missing'):
            self.check()

    def test_forged_mapping_walk(self):
        self.manifest['graphics_pb_spans'][0]['pages'][0]['walk'][1][0] = '0x1'
        with self.assertRaisesRegex(ValueError, 'different page table'):
            self.check()

    def test_wrong_context(self):
        self.manifest['graphics_pb_spans'][0]['context'] = 3
        with self.assertRaisesRegex(ValueError, 'another allocation'):
            self.check()

    def test_capture_and_span_mapping_disagree(self):
        self.manifest['graphics_pb_spans'][3]['pages'][0]['physical'] = '0x860000000'
        with self.assertRaisesRegex(ValueError, 'disagree'):
            self.check()


if __name__ == '__main__':
    unittest.main()
