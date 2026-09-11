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
COMPLETE_PC = 0xfffffe00089855b0
POOL_PC = 0xfffffe00089a766c
BASE = 0x10000000000
COMMAND, QUEUE, RING = (0xfffffe0010000000 + x for x in (0x1000, 0x2000, 0x10000))
DESCRIPTOR = COMMAND + 0x2000


class ADT(dict):
    model = 'Mac17,3'


class FakeHV:
    def __init__(self, output, stride=0x180, invalid_ring=False, bad_code=False):
        self.memory = bytearray(0x80000)
        self.memory[:8] = bytes.fromhex('811642f9700240f9') if not bad_code else bytes(8)
        self.memory[8:16] = bytes.fromhex('f30300aa080840f9')
        self.memory[16:24] = bytes.fromhex('390040b928f77ed3')
        self.gpu_views, self.context_views = {}, {}
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
        self.ctx.regs[20] = DESCRIPTOR
        struct.pack_into('<QQ', self.memory, 0x3530, 0x54b, 0xf5)
        self.started = True
        self.u = SimpleNamespace(adt=ADT({'/chosen': SimpleNamespace(
            chip_id=0x8142, dram_base=BASE, dram_size=len(self.memory)),
            '/arm-io/sgx': SimpleNamespace(gpu_region_base=BASE)}))
        self.p = SimpleNamespace(hv_translate=self.translate)
        self.iface = SimpleNamespace(readmem=self.read)
        Path(output, 'console.log').write_text('uat_global_root_pa=0x10000000000\n')

    def gpu_read(self, address, size):
        if self.invalid_ring:
            raise ValueError('GPU capture attempted a non-DRAM access')
        for base, data in self.gpu_views.items():
            if base <= address < address + size <= base + len(data):
                return bytes(data[address-base:address-base+size])
        offset = address - 0xfffffc2012340000 + 0x10000
        return bytes(self.memory[offset:offset + size])

    def read_context(self, address, size, context, region):
        if (context, address, size) not in self.context_views:
            raise ValueError('Unmapped synthetic context view')
        return bytes(self.context_views[(context, address, size)])

    def translate(self, address, *_):
        if POOL_PC <= address < POOL_PC + 8:
            return BASE + 16 + address - POOL_PC
        if PC <= address < PC + 8:
            return BASE + address - PC
        if COMPLETE_PC <= address < COMPLETE_PC + 8:
            return BASE + 8 + address - COMPLETE_PC
        if self.invalid_ring and RING <= address < RING + 0x18000:
            return 0x580004000
        if COMMAND <= address < RING + 0x18000:
            return BASE + address - (COMMAND - 0x1000)
        return 0

    def mapping(self, address, size, context, region):
        self.read_context(address, size, context, region)
        return [dict(address=hex(address), physical=hex(BASE + 0x70000), size=size,
                     walk=[[hex(BASE + 0x100), hex(0xc0000000000c8b)]])]

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
        factory = lambda *_: SimpleNamespace(read=hv.gpu_read, read_context=hv.read_context,
                                            _read_dram=hv.read, mapping=hv.mapping)
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
                    descriptor = next(x for x in result['objects'] if x['label'] == 'host-cl-descriptor-view')
                    self.assertTrue(descriptor['stable'])
                    raw = (path.parent / descriptor['file']).read_bytes()
                    self.assertEqual(struct.unpack_from('<QQ', raw, 0x530), (0x54b, 0xf5))

    def test_pool_control_capture_and_submission_disarm(self):
        with tempfile.TemporaryDirectory() as directory:
            hv = FakeHV(directory)
            hv.ctx.regs[1] = COMMAND + 0x100
            hv.ctx.elr = POOL_PC
            struct.pack_into('<I', hv.memory, 0x1100, 0x20)
            struct.pack_into('<Q', hv.memory, 0x1114, 0xfffffc2012340000)
            with patch.dict(os.environ, M5_GPU_TRACE_DIR=directory,
                            M5_SKSM_CAPTURE_POOL='1'), contextlib.redirect_stdout(io.StringIO()):
                state = self.run_script(hv)
                self.assertEqual(hv.breakpoints, [PC, POOL_PC])
                self.assertTrue(state['_m5_pool_control'](hv.ctx))
                self.assertEqual((hv.ctx.elr, hv.ctx.regs[25]), (POOL_PC + 4, 0x20))
                hv.ctx.elr = PC
                self.assertTrue(state['_m5_sksm_submit'](hv.ctx))
                self.assertEqual(hv.breakpoints, [])
            path, = Path(directory).glob('sksm-submit-*/manifest.json')
            observed, = json.loads(path.read_text())['prior_pool_controls']
            self.assertTrue(observed['command_stable'])
            self.assertTrue(observed['event_stable'])
            self.assertEqual((Path(directory) / observed['directory'] / 'command.bin').read_bytes()[:4],
                             struct.pack('<I', 0x20))

    def test_grow_views_preserve_instruction_when_one_context_is_unmapped(self):
        with tempfile.TemporaryDirectory() as directory:
            hv = FakeHV(directory)
            hv.ctx.regs[1], hv.ctx.elr = COMMAND + 0x100, POOL_PC
            event_address, table, grow, page_list = 0xfffffc2050000000, 0x7000408000, 0x7000408380, 0x7000000000
            struct.pack_into('<I', hv.memory, 0x1100, 0x20)
            struct.pack_into('<QQQI', hv.memory, 0x1114, event_address, table, grow, 0x20)
            struct.pack_into('<I', hv.memory, 0x1104, 3)
            event = bytearray(0x70)
            struct.pack_into('<Q', event, 0x14, page_list)
            hv.gpu_views[event_address] = event
            for address, length in [(table, 0x1000), (grow, 0x100), (page_list, 0x100)]:
                hv.context_views[(3, address, length)] = bytes([length // 0x100]) * length
            with patch.dict(os.environ, M5_GPU_TRACE_DIR=directory,
                            M5_SKSM_CAPTURE_POOL='1', M5_SKSM_CAPTURE_POOL_GROW='1'), contextlib.redirect_stdout(io.StringIO()):
                state = self.run_script(hv)
                self.assertTrue(state['_m5_pool_control'](hv.ctx))
            self.assertEqual((hv.ctx.elr, hv.ctx.regs[25]), (POOL_PC + 4, 0x20))
            path, = Path(directory).glob('pool-control-*/manifest.json')
            record = json.loads(path.read_text())
            self.assertNotIn('event_error', record)
            views = record['before_pool_views']
            self.assertEqual(len(views), 6)
            self.assertTrue(all('error' in view for view in views if view['gpu_context'] == 0))
            self.assertTrue(all(view['stable'] for view in views if view['gpu_context'] == 3))
            self.assertEqual((path.parent / 'grow-records-context3.bin').read_bytes(), bytes([1]) * 0x100)
            self.assertEqual(hv.breakpoints, [PC, POOL_PC])

    def test_unmapped_descriptor_does_not_invalidate_the_ring(self):
        with tempfile.TemporaryDirectory() as directory:
            hv = FakeHV(directory)
            hv.ctx.regs[20] = 0x580004000
            with patch.dict(os.environ, M5_GPU_TRACE_DIR=directory), contextlib.redirect_stdout(io.StringIO()):
                self.run_script(hv)
                self.assertTrue(hv.hook(hv.ctx))
            path, = Path(directory).glob('sksm-submit-*/manifest.json')
            result = json.loads(path.read_text())
            self.assertNotIn('error', result)
            self.assertIn('outside DRAM', result['host_descriptor_error'])
            self.assertTrue(all(BASE <= address < BASE + 0x80000 for address, _ in hv.reads))
            self.assertEqual(hv.breakpoints, [])

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
            struct.pack_into('<Q', hv.memory, 0x3428, 0x123456789abcdef0)
            with patch.dict(os.environ, M5_GPU_TRACE_DIR=directory, M5_SKSM_CAPTURE_PRIORITY='2'), contextlib.redirect_stdout(io.StringIO()):
                self.run_script(hv)
                self.assertTrue(hv.hook(hv.ctx))
                self.assertEqual(hv.ctx.elr, PC + 4)
                self.assertEqual(hv.ctx.regs[1], 0x123456789abcdef0)
                self.assertEqual(hv.breakpoints, [PC])
                self.assertEqual(list(Path(directory).glob('sksm-submit-*')), [])
                hv.memory[0x1008] = 2
                hv.ctx.elr = PC
                self.assertTrue(hv.hook(hv.ctx))
            self.assertEqual(hv.breakpoints, [])
            path, = Path(directory).glob('sksm-submit-*/manifest.json')
            self.assertEqual(json.loads(path.read_text())['observed_submissions'], 2)

    def test_completion_tracks_the_original_descriptor_and_removes_hook(self):
        with tempfile.TemporaryDirectory() as directory:
            hv = FakeHV(directory, stride=0x200)
            # The native allocation may leave only 0x140 bytes in its first
            # page. The observer must still read the 4096-byte descriptor prefix.
            payload_address = 0xfffffc2040003ec0
            payload = bytearray(0x1000)
            struct.pack_into('<I', payload, 0, 3)
            struct.pack_into('<4H', payload, 0x18, 37, 39, 39, 40)
            for i, (number, value) in enumerate([(0x10201, 0x300), (0x1a420, 0x100000b0000),
                                                (0x1a510, 0x100000e0000), (0x14070, 0x1000018001)]):
                struct.pack_into('<IQ', payload, 0x40+i*12, number, value)
            struct.pack_into('<Q', hv.memory, 0x10210, payload_address)
            hv.gpu_views[payload_address] = payload
            channel_address = payload_address + 0x4000
            pointers_address = payload_address + 0x8000
            channel = bytearray(0x4000)
            struct.pack_into('<Q', channel, 0, pointers_address)
            struct.pack_into('<Q', hv.memory, 0x10218, channel_address)
            hv.gpu_views[channel_address] = channel
            hv.gpu_views[pointers_address] = bytearray(0x100)
            completion_address = 0xfffffc2050000000
            hv.gpu_views[completion_address] = bytearray(0x20000)
            hardware = bytearray(0x28000)
            struct.pack_into('<QQIIQ', hardware, 0x2614+2*32,
                             0x7001000000, completion_address, 0x800, 0x40, 2)
            preinit = Path(directory) / 'preinit/gfx-asc'
            preinit.mkdir(parents=True)
            (preinit / 'shared-cluster.bin').write_bytes(hardware)
            view = (3, 0x10000001000, 4)
            hv.context_views[view] = b'PRE!'
            with patch.dict(os.environ, M5_GPU_TRACE_DIR=directory,
                            M5_SKSM_CAPTURE_COMPLETION='1',
                            M5_SKSM_CONTEXT_VIEWS=json.dumps([[view[1], view[2]]])), contextlib.redirect_stdout(io.StringIO()):
                self.run_script(hv)
                self.assertTrue(hv.hook(hv.ctx))
                self.assertEqual(hv.breakpoints, [COMPLETE_PC])
                hv.ctx.elr = COMPLETE_PC
                hv.ctx.regs[0] = DESCRIPTOR + 16
                self.assertTrue(hv.hook(hv.ctx))
                self.assertEqual(hv.breakpoints, [COMPLETE_PC])
                self.assertEqual(hv.ctx.elr, COMPLETE_PC + 4)
                self.assertEqual(hv.ctx.regs[19], DESCRIPTOR + 16)
                hv.ctx.elr = COMPLETE_PC
                hv.ctx.regs[0] = DESCRIPTOR
                hv.context_views[view] = b'POST'
                struct.pack_into('<I', hv.gpu_views[pointers_address], 0x20, 1)
                self.assertTrue(hv.hook(hv.ctx))
            self.assertEqual(hv.breakpoints, [])
            path, = Path(directory).glob('sksm-submit-*/manifest.json')
            result = json.loads(path.read_text())
            self.assertNotIn('completion_error', result)
            self.assertEqual(result['completion']['observed_callbacks'], 2)
            self.assertEqual((path.parent / 'compute-extra-0.bin').read_bytes(), b'PRE!')
            self.assertEqual((path.parent / 'completed-extra-0.bin').read_bytes(), b'POST')
            for label in ('compute-extra-0', 'completed-extra-0'):
                item = next(x for x in result['objects'] if x['label'] == label)
                self.assertTrue(item['mapping_stable'])
                self.assertEqual(item['mapping'], hv.mapping(view[1], view[2], view[0], BASE))
            self.assertEqual(struct.unpack_from('<I', (path.parent / 'channel-pointers-before.bin').read_bytes(), 0x20)[0], 0)
            self.assertEqual(struct.unpack_from('<I', (path.parent / 'channel-pointers-after.bin').read_bytes(), 0x20)[0], 1)

    def test_replay_completion_restores_before_return_or_keeps_guest_paused(self):
        for failure in (False, True):
            with self.subTest(restore_failure=failure), tempfile.TemporaryDirectory() as directory:
                hv = FakeHV(directory, stride=0x200)
                payload_address = 0xfffffc2040000000
                payload = bytearray(0x4000)
                struct.pack_into('<I', payload, 0, 3)
                struct.pack_into('<4H', payload, 0x18, 37, 39, 39, 40)
                for i, (number, value) in enumerate([(0x10201, 0x300), (0x1a420, 0x100000b0000),
                                                    (0x1a510, 0x100000e0000), (0x14070, 0x1000018001)]):
                    struct.pack_into('<IQ', payload, 0x40+i*12, number, value)
                struct.pack_into('<Q', hv.memory, 0x10210, payload_address)
                hv.gpu_views[payload_address] = payload
                channel_address, pointers_address = payload_address + 0x4000, payload_address + 0x8000
                channel = bytearray(0x4000)
                struct.pack_into('<Q', channel, 0, pointers_address)
                struct.pack_into('<Q', hv.memory, 0x10218, channel_address)
                hv.gpu_views[channel_address] = channel
                hv.gpu_views[pointers_address] = bytearray(0x100)
                completion_address = 0xfffffc2050000000
                hv.gpu_views[completion_address] = bytearray(0x20000)
                hardware = bytearray(0x28000)
                struct.pack_into('<QQIIQ', hardware, 0x2614+2*32,
                                 0x7001000000, completion_address, 0x800, 0x40, 2)
                preinit = Path(directory) / 'preinit/gfx-asc'
                preinit.mkdir(parents=True)
                (preinit / 'shared-cluster.bin').write_bytes(hardware)
                views = [[0x10000000000, 0x4000], [0x1000001c000, 0x4000], [0x10000090000, 0x4000]]
                for address, size in views:
                    hv.context_views[(3, address, size)] = bytes(size)
                hv.p.dc_civac = lambda *_: None
                events = []

                class Replay:
                    active = False
                    receipt = {'state': 'unprepared'}

                    def __init__(self, *_):
                        pass

                    def prepare(self, **_):
                        events.append('prepare')

                    def apply(self):
                        self.active = True
                        self.receipt = {'state': 'applied'}
                        events.append('apply')

                    def restore(self):
                        events.append('restore')
                        if failure:
                            raise IOError('simulated restoration failure')
                        self.active = False
                        self.receipt = {'state': 'restored'}

                options = dict(M5_GPU_TRACE_DIR=directory, M5_SKSM_CAPTURE_PRIORITY='2',
                    M5_SKSM_CAPTURE_COMPLETION='1', M5_SKSM_CONTEXT_VIEWS=json.dumps(views),
                    M5_SKSM_PROGRAM_REPLAY=json.dumps(dict(native='/native', linux='/linux', workload='/workload')))
                with patch.dict(os.environ, options), contextlib.redirect_stdout(io.StringIO()), \
                        patch.dict(sys.modules, m5_program_replay=SimpleNamespace(M5ProgramReplay=Replay)):
                    self.run_script(hv)
                    self.assertTrue(hv.hook(hv.ctx))
                    captured, = Path(directory).glob('sksm-submit-*/manifest.json')
                    self.assertEqual(events, ['prepare', 'apply'], captured.read_text())
                    self.assertEqual(hv.breakpoints, [COMPLETE_PC])
                    hv.ctx.elr, hv.ctx.regs[0] = COMPLETE_PC, DESCRIPTOR
                    self.assertEqual(hv.hook(hv.ctx), not failure)
                    self.assertEqual(events, ['prepare', 'apply', 'restore'])
                    self.assertEqual(hv.m5_program_replay.active, failure)
                    self.assertEqual(hv.breakpoints, [])

    def test_replay_refuses_missing_completion_before_arming(self):
        with tempfile.TemporaryDirectory() as directory:
            hv = FakeHV(directory)
            with patch.dict(os.environ, M5_GPU_TRACE_DIR=directory,
                            M5_SKSM_PROGRAM_REPLAY='{"native":"/native","linux":"/linux","workload":"/workload"}'):
                with self.assertRaisesRegex(ValueError, 'Replay requires'):
                    self.run_script(hv)
            self.assertEqual(hv.breakpoints, [])


if __name__ == '__main__':
    unittest.main()
