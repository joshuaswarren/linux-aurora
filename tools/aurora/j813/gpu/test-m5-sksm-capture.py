#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""Exercise native SKSM observer guards without a connected device."""
import contextlib
import io
import json
import os
from pathlib import Path
import runpy
import struct
import sys
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch

SCRIPT = Path(__file__).with_name('trace-m5-sksm-submit.py')
PC = 0xfffffe0008984fc0
BASE = 0x10000000000
COMMAND, QUEUE, RING = (0xfffffe0010000000 + x for x in (0x1000, 0x2000, 0x10000))


class ADT(dict):
    model = 'Mac17,3'


class FakeHV:
    def __init__(self, output, stride=0x180, invalid_ring=False, bad_code=False):
        self.memory = bytearray(0x80000)
        self.memory[:8] = bytes.fromhex('811642f9700240f9') if not bad_code else bytes(8)
        struct.pack_into('<IHHBQI', self.memory, 0x1000, 14, 4, 1, 2, 1, 2)
        struct.pack_into('<III', self.memory, 0x200c, 4, 256, stride)
        struct.pack_into('<II', self.memory, 0x2018, 0xdeadbeef, 2)
        struct.pack_into('<QQ', self.memory, 0x2078, RING, 0xfffffc2012340000)
        struct.pack_into('<Q', self.memory, 0x2088, 0xfffffc2012340000)
        self.memory[0x10000 + 0x180:0x10000 + 0x180 + 4] = b'SKSM'
        self.reads, self.breakpoints = [], []
        self.invalid_ring = invalid_ring
        self.ctx = SimpleNamespace(elr=PC, regs=[0] * 31, spsr=5)
        self.ctx.regs[2], self.ctx.regs[24] = COMMAND, QUEUE
        self.started = True
        self.u = SimpleNamespace(adt=ADT({'/chosen': SimpleNamespace(
            chip_id=0x8142, dram_base=BASE, dram_size=len(self.memory))}))
        self.p = SimpleNamespace(hv_translate=self.translate)
        self.iface = SimpleNamespace(readmem=self.read)
        Path(output, 'console.log').write_text('uat_global_root_pa=0x10000000000\n')

    def gpu_read(self, address, size):
        if self.invalid_ring:
            raise ValueError('GPU capture attempted a non-DRAM access')
        offset = address - 0xfffffc2012340000 + 0x10000
        return bytes(self.memory[offset:offset + size])

    def translate(self, address, *_):
        if PC <= address < PC + 8:
            return BASE + address - PC
        if self.invalid_ring and RING <= address < RING + 0x18000:
            return 0x580004000
        if COMMAND <= address < RING + 0x18000:
            return BASE + address - (COMMAND - 0x1000)
        return 0

    def read(self, address, size):
        self.reads.append((address, size))
        return bytes(self.memory[address - BASE:address - BASE + size])

    def add_hw_bp(self, address, hook):
        self.breakpoints.append(address)
        self.hook = hook

    def remove_hw_bp(self, address):
        self.breakpoints.remove(address)


class CaptureTests(unittest.TestCase):
    def run_script(self, hv):
        factory = lambda *_: SimpleNamespace(read=hv.gpu_read)
        with patch.dict(sys.modules, m5_gpu_capture=SimpleNamespace(M5GPUCapture=factory)):
            return runpy.run_path(str(SCRIPT), init_globals={'hv': hv})

    def test_capture_and_failure_guards(self):
        for kwargs in ({}, {'stride': 0x1001}, {'invalid_ring': True}):
            with self.subTest(kwargs=kwargs), tempfile.TemporaryDirectory() as directory:
                hv = FakeHV(directory, **kwargs)
                with patch.dict(os.environ, M5_GPU_TRACE_DIR=directory), contextlib.redirect_stdout(io.StringIO()):
                    self.run_script(hv)
                    self.assertEqual(hv.breakpoints, [PC])
                    self.assertTrue(hv.hook(hv.ctx))
                self.assertEqual(hv.breakpoints, [])
                path, = Path(directory).glob('sksm-submit-*/manifest.json')
                result = json.loads(path.read_text())
                self.assertFalse(result['device_memory_read'])
                self.assertTrue(all(BASE <= address < BASE + 0x80000 for address, _ in hv.reads))
                if kwargs:
                    self.assertIn('error', result)
                else:
                    self.assertNotIn('error', result)
                    self.assertEqual((result['entry_count'], result['entry_stride'], result['priority']), (256, 0x180, 2))
                    self.assertEqual((path.parent / 'kick-ring.bin').read_bytes()[0x180:0x184], b'SKSM')

    def test_wrong_code_never_arms(self):
        with tempfile.TemporaryDirectory() as directory:
            hv = FakeHV(directory, bad_code=True)
            with patch.dict(os.environ, M5_GPU_TRACE_DIR=directory):
                with self.assertRaisesRegex(RuntimeError, 'pinned 25G76'):
                    self.run_script(hv)
            self.assertEqual(hv.breakpoints, [])
            self.assertEqual([p.name for p in Path(directory).iterdir()], ['console.log'])

    def test_el0_defers_kernel_text_validation_to_hit(self):
        with tempfile.TemporaryDirectory() as directory:
            hv = FakeHV(directory, bad_code=True)
            hv.ctx.spsr = 0
            with patch.dict(os.environ, M5_GPU_TRACE_DIR=directory), contextlib.redirect_stdout(io.StringIO()):
                self.run_script(hv)
                self.assertEqual(hv.reads, [])
                hv.ctx.spsr = 5
                self.assertTrue(hv.hook(hv.ctx))
            self.assertEqual(hv.breakpoints, [])
            path, = Path(directory).glob('sksm-submit-*/manifest.json')
            result = json.loads(path.read_text())
            self.assertIn('pinned 25G76', result['error'])
            self.assertEqual(result['objects'], [])

    def test_background_priority_is_skipped(self):
        with tempfile.TemporaryDirectory() as directory:
            hv = FakeHV(directory)
            hv.memory[0x1008] = 0
            with patch.dict(os.environ, M5_GPU_TRACE_DIR=directory, M5_SKSM_CAPTURE_PRIORITY='2'), contextlib.redirect_stdout(io.StringIO()):
                self.run_script(hv)
                self.assertTrue(hv.hook(hv.ctx))
                self.assertEqual(hv.breakpoints, [PC])
                self.assertEqual(list(Path(directory).glob('sksm-submit-*')), [])
                hv.memory[0x1008] = 2
                self.assertTrue(hv.hook(hv.ctx))
            self.assertEqual(hv.breakpoints, [])
            path, = Path(directory).glob('sksm-submit-*/manifest.json')
            self.assertEqual(json.loads(path.read_text())['observed_submissions'], 2)


if __name__ == '__main__':
    unittest.main()
