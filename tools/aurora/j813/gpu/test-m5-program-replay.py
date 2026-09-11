#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""Exercise replay ownership, preflight and rollback without hardware."""
import json
from pathlib import Path
import struct
import tempfile
import unittest
from unittest.mock import patch

import m5_program_replay as mod


class FakeGPU:
    low, high = 0x10000000000, 0x10400000000

    def __init__(self):
        addresses = [v[1] for v in mod.PROGRAMS] + [mod.BUFFER, mod.PREEMPT]
        self.physical = {va: self.low + 0x400000 + i * mod.PAGE for i, va in enumerate(addresses)}
        self.data = {pa: bytearray(mod.PAGE) for pa in self.physical.values()}
        self.writes, self.syncs = [], []
        self.fail_write = None

    def mapping(self, va, size, *_):
        return [dict(address=hex(va), physical=hex(self.physical[va]), size=size,
                     walk=[[hex(self.low + 0x100), hex(self.physical[va] | 0xc8b)]])]

    def read_context(self, va, size, *_):
        return bytes(self.data[self.physical[va]][:size])

    def write(self, pa, data):
        self.writes.append(pa)
        self.data[pa][:] = data
        if self.fail_write == len(self.writes):
            self.data[pa][-1] ^= 1
            raise IOError('simulated partial transport failure')

    def sync(self, pa, size):
        self.syncs.append((pa, size))


class ReplayTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.native, self.linux, self.out = (self.root / x for x in ('native', 'linux', 'output'))
        for p in (self.native, self.linux, self.out):
            p.mkdir()
        self.gpu = FakeGPU()
        self.programs = []
        native_objects, linux_objects = [], []
        for i, (name, va, label, _) in enumerate(mod.PROGRAMS):
            original, replacement = bytes([6 + i]) * mod.PAGE, bytes([5 + i]) * mod.PAGE
            self.gpu.data[self.gpu.physical[va]][:] = original
            self.programs.append((name, va, label, mod.digest(replacement)))
            for directory, objects, key, data, is_native in (
                    (self.native, native_objects, label, original, True),
                    (self.linux, linux_objects, 'program-' + name, replacement, False)):
                filename = key + '.bin'
                (directory / filename).write_bytes(data)
                objects.append(dict(label=key, address=hex(va), size=mod.PAGE,
                    gpu_context=3, context=3, stable=True, mapping_stable=True,
                    file=filename, sha256=mod.digest(data)))
        completed = bytearray(mod.PAGE)
        struct.pack_into('<1024I', completed, 0xd00, *(i * 7 for i in range(1024)))
        struct.pack_into('<1024I', completed, 0x1d00, *(i * 21 + 6 for i in range(1024)))
        (self.native / 'completed-extra-1.bin').write_bytes(completed)
        native_objects.append(dict(label='completed-extra-1', address=hex(mod.BUFFER),
            size=mod.PAGE, gpu_context=3, stable=True, mapping_stable=True,
            file='completed-extra-1.bin', sha256=mod.digest(completed)))
        (self.native / 'manifest.json').write_text(json.dumps(dict(objects=native_objects,
            completion=dict(same_descriptor=True), compute_context=3, entry_stamp='0x1')))
        (self.linux / 'manifest.json').write_text(json.dumps(dict(objects=linux_objects)))
        self.workload = self.root / 'workload.json'
        self.workload.write_text(json.dumps(dict(shader_add=6, expected_add=6, queues=1,
            compute_values_verified=1024, render_pixels_verified=0)))
        b = self.gpu.data[self.gpu.physical[mod.BUFFER]]
        struct.pack_into('<1024I', b, 0xd00, *(i * 7 for i in range(1024)))
        struct.pack_into('<1024I', b, 0x1d00, *((0xdeadbeef,) * 1024))
        struct.pack_into('<QQ', self.gpu.data[self.gpu.physical[mod.PREEMPT]], 0x14a0,
                         mod.BUFFER + 0xd00, mod.BUFFER + 0x1d00)
        self.original = {k: bytes(v) for k, v in self.gpu.data.items()}
        p = patch.object(mod, 'PROGRAMS', tuple(self.programs))
        p.start()
        self.addCleanup(p.stop)
        self.replay = mod.M5ProgramReplay(self.gpu, self.gpu.write, self.gpu.sync,
                                         3, self.gpu.low, self.out)

    def prepare(self):
        self.replay.prepare(self.native, self.linux, self.workload)

    def test_all_program_pages_replace_then_restore_resources_untouched(self):
        self.prepare()
        self.assertEqual(self.gpu.writes, [])
        self.replay.apply()
        self.assertTrue(self.replay.active)
        for i, (_, va, _, _) in enumerate(self.programs):
            self.assertEqual(self.gpu.read_context(va, mod.PAGE), bytes([5 + i]) * mod.PAGE)
        for va in (mod.BUFFER, mod.PREEMPT):
            pa = self.gpu.physical[va]
            self.assertEqual(self.gpu.data[pa], self.original[pa])
        self.replay.restore()
        self.assertFalse(self.replay.active)
        self.assertEqual(self.gpu.data, self.original)
        self.assertEqual(len(self.gpu.writes), 6)
        self.assertEqual(len(self.gpu.syncs), 12)

    def test_last_page_mismatch_prevents_every_write(self):
        self.gpu.data[self.gpu.physical[self.programs[-1][1]]][0] ^= 1
        with self.assertRaisesRegex(ValueError, 'differs from baseline'):
            self.prepare()
        self.assertEqual(self.gpu.writes, [])

    def test_shader_body_variant_preserves_every_other_shader_page_byte(self):
        self.replay.prepare(self.native, self.linux, self.workload, variant='shader-body-only')
        self.replay.apply()
        va = self.programs[0][1]
        changed = self.gpu.read_context(va, mod.PAGE)
        original = self.original[self.gpu.physical[va]]
        self.assertEqual(changed[:0x3c0], original[:0x3c0])
        self.assertEqual(changed[0x3c0:0x400], bytes([5]) * 64)
        self.assertEqual(changed[0x400:], original[0x400:])
        self.replay.restore()
        self.assertEqual(self.gpu.data, self.original)

    def test_buffer_mismatch_prevents_every_write(self):
        self.gpu.data[self.gpu.physical[mod.BUFFER]][0xd00] = 1
        with self.assertRaisesRegex(ValueError, 'buffers or argument'):
            self.prepare()
        self.assertEqual(self.gpu.writes, [])

    def test_changed_mapping_between_prepare_and_apply_prevents_writes(self):
        self.prepare()
        self.gpu.physical[self.programs[-1][1]] += mod.PAGE
        with self.assertRaisesRegex(ValueError, 'state changed'):
            self.replay.apply()
        self.assertEqual(self.gpu.writes, [])

    def test_partial_second_write_rolls_back_both_touched_pages(self):
        self.prepare()
        self.gpu.fail_write = 2
        with self.assertRaisesRegex(IOError, 'partial transport'):
            self.replay.apply()
        self.assertFalse(self.replay.active)
        self.assertEqual(self.gpu.data, self.original)
        self.assertEqual(len(self.gpu.writes), 4)

    def test_restoration_failure_keeps_active_journal_for_paused_guest(self):
        self.prepare()
        self.replay.apply()
        self.gpu.fail_write = 4
        with self.assertRaises(IOError):
            self.replay.restore()
        self.assertTrue(self.replay.active)
        self.assertEqual(self.replay.receipt['state'], 'restore-failed')
        self.gpu.fail_write = None
        self.replay.restore()
        self.assertEqual(self.gpu.data, self.original)

    def test_recycled_mapping_prevents_restoration_write(self):
        self.prepare()
        self.replay.apply()
        self.gpu.physical[self.programs[0][1]] += mod.PAGE
        with self.assertRaisesRegex(ValueError, 'ownership changed'):
            self.replay.restore()
        self.assertTrue(self.replay.active)
        self.assertEqual(len(self.gpu.writes), 3)

    def test_unqualified_baseline_and_modified_source_are_rejected(self):
        self.workload.write_text('{}')
        with self.assertRaisesRegex(ValueError, 'successful compute-only'):
            self.prepare()
        self.assertEqual(self.gpu.writes, [])
        p = self.linux / 'program-shader.bin'
        p.write_bytes(bytes(mod.PAGE))
        with self.assertRaisesRegex(ValueError, 'hash differs'):
            mod.load_capture(self.linux, 'program-shader', self.programs[0][1], False)


if __name__ == '__main__':
    unittest.main()
