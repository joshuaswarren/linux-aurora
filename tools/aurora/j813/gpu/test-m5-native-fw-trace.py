#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""Check trace-input ownership and restoration across cached firmware flags."""
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

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / 'hv/m1n1/proxyclient'))
import m5_gpu_capture


class NativeTraceTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.base = Path(self.temp.name)
        preinit = self.base / 'preinit/gfx-asc'
        preinit.mkdir(parents=True)
        self.root_pointer = 0xfffffc20001a8000
        self.main_pointer = 0xfffffc2000020000
        self.hardware_pointer = 0xfffffc2000008000
        self.status_pointer = 0xfffffc200002ee40
        self.physical = 0x1000002ee40
        self.flags = 0
        self.writes = []
        self.break_readback = False
        self.root = bytearray(0xb8)
        struct.pack_into('<Q', self.root, 0, m5_gpu_capture.M5GPUCapture.VERSION)
        struct.pack_into('<Q', self.root, 0x18, self.main_pointer)
        struct.pack_into('<Q', self.root, 0xa8, self.status_pointer)
        self.main = bytearray(0x600)
        struct.pack_into('<Q', self.main, 0, self.hardware_pointer)
        struct.pack_into('<QQ', self.main, 0x1e0,
                         self.status_pointer + 0x240, self.status_pointer + 0xa6ac0)
        self.status = bytearray(0x300)
        (preinit / 'root.bin').write_bytes(self.root)
        (preinit / 'manifest.json').write_text(json.dumps(
            {'objects': [{'label': 'root', 'pointer': hex(self.root_pointer)}]}))
        (self.base / 'console.log').write_text('uat_global_root_pa=0x10000004000\n')
        owner = self

        class Reader:
            VERSION = m5_gpu_capture.M5GPUCapture.VERSION
            low, high = 0x10000000000, 0x10400000000

            def __init__(self, *_):
                self.walker = SimpleNamespace(translate=lambda *_: (owner.physical, []))

            def read(self, address, size):
                regions = [(owner.root_pointer, owner.root),
                           (owner.main_pointer, owner.main),
                           (owner.hardware_pointer + 0x2614, bytes(0x80)),
                           (owner.status_pointer, owner.status),
                           (owner.status_pointer + 0xa6ac0, bytes(512 * 72))]
                for base, data in regions:
                    if base <= address and address + size <= base + len(data):
                        return bytes(data[address - base:address - base + size])
                raise AssertionError('Unexpected target read')

            def _read_dram(self, address, size):
                owner.assertEqual((address, size), (0x10000100000 + 0x1779fc - 0x4c000, 4))
                return struct.pack('<I', owner.flags)

        class ADT(dict):
            model = 'Mac17,3'

        def write32(address, value):
            self.assertIsNotNone(getattr(self.hv, 'm5_native_trace_input', None),
                                 'Restoration receipt must precede the target write')
            self.assertEqual(address, self.physical | 0xa0000000000)
            self.writes.append((address, value))
            if not self.break_readback:
                struct.pack_into('<I', self.status, 0, value)

        self.reader = Reader
        self.hv = SimpleNamespace(ctx=object(), started=True,
            u=SimpleNamespace(adt=ADT({
                '/chosen': SimpleNamespace(chip_id=0x8142, dram_base=0x10000000000,
                                           dram_size=0x400000000),
                '/arm-io/sgx': SimpleNamespace(gfx_data_base=0x10000100000)})),
            iface=SimpleNamespace(readmem=lambda *_: self.fail('Unexpected direct read')),
            p=SimpleNamespace(write32=write32))

    def run_action(self, action):
        with patch.object(m5_gpu_capture, 'M5GPUCapture', self.reader), \
             patch.dict(os.environ, {'M5_GPU_TRACE_DIR': str(self.base)}), \
             contextlib.redirect_stdout(io.StringIO()):
            return runpy.run_path(str(Path(__file__).with_name('trace-m5-native-fw.py')),
                init_globals=dict(hv=self.hv, m5_native_trace_action=action,
                                  m5_native_trace_mask=0x201))['trace_result']

    def test_snapshot_does_not_write(self):
        self.assertEqual(self.run_action('snapshot')['input_after'], '0x0')
        self.assertEqual(self.writes, [])

    def test_restoration_waits_for_firmware_refresh(self):
        self.run_action('enable')
        self.flags = 0x201
        result = self.run_action('restore')
        self.assertEqual(result['input_after'], '0x0')
        self.assertFalse(result['firmware_restore_verified'])
        self.assertIsNotNone(self.hv.m5_native_trace_input)
        self.flags = 0
        self.assertTrue(self.run_action('snapshot')['firmware_restore_verified'])
        self.assertIsNone(self.hv.m5_native_trace_input)
        self.assertEqual([value for _, value in self.writes], [0x201, 0])

    def test_changed_root_is_rejected_before_write(self):
        self.root[0x10] = 1
        with self.assertRaisesRegex(RuntimeError, 'root differs'):
            self.run_action('enable')
        self.assertEqual(self.writes, [])

    def test_device_mapping_is_rejected_before_write(self):
        self.physical = 0x580000000
        with self.assertRaisesRegex(RuntimeError, 'outside measured DRAM'):
            self.run_action('enable')
        self.assertEqual(self.writes, [])

    def test_existing_input_is_not_overwritten(self):
        struct.pack_into('<I', self.status, 0, 1)
        with self.assertRaisesRegex(RuntimeError, 'unmodified zero'):
            self.run_action('enable')
        self.assertEqual(self.writes, [])

    def test_failed_readback_retains_restoration_receipt(self):
        self.break_readback = True
        with self.assertRaisesRegex(RuntimeError, 'readback differs'):
            self.run_action('enable')
        self.assertEqual(self.hv.m5_native_trace_input['original'], 0)
        self.assertEqual(self.hv.m5_native_trace_input['enabled'], 0x201)


if __name__ == '__main__':
    unittest.main()
