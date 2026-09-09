#!/usr/bin/env python3
"""Exercise firmware handoff guards using the saved, qualified capture."""
import json
from pathlib import Path
import runpy
import struct
import tempfile
from types import SimpleNamespace as NS
import unittest

ROOT = Path(__file__).resolve().parents[1]
CAPTURE = ROOT / 'artifacts/ave-capture-20260908/three-frames-rings'
PREPARE = runpy.run_path(str(ROOT / 'video/ave-boot-preflight.py'))['prepare_ave_firmware']


class BootPreflightTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.manifest = json.loads((CAPTURE / 'ave-preloaded-firmware.json').read_text())
        cls.segments = cls.manifest['segments']
        cls.raw = b''.join(struct.pack('<QQQII', int(s['physical'], 16),
            int(s['offset'], 16), int(s['alias_raw'], 16), s['size'], s['flags_raw'])
            for s in cls.segments)
        cls.blobs = {int(s['physical'], 16): (CAPTURE / s['file']).read_bytes()
                     for s in cls.segments}

    def invoke(self, raw=None, unstable=False, corrupt=False, limit=None):
        props = {'segment-ranges': self.raw if raw is None else raw,
                 'segment-names': '__TEXT;__DATA', 'pre-loaded': 1}
        calls, chosen = [], {}
        def read(address, size):
            calls.append(address)
            data = self.blobs[address]
            if corrupt or (unstable and len(calls) == 2):
                data = bytes([data[0] ^ 1]) + data[1:]
            return data
        u = NS(adt={'/arm-io/ave': NS(getprop=props.__getitem__),
                    '/chosen': NS(getprop=lambda _: 0x10000000000)},
               ba=NS(phys_base=limit or int(self.manifest['bootargs_phys_base'], 16)))
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory) / 'capture'
            PREPARE(u, NS(kboot_set_chosen=chosen.__setitem__), NS(readmem=read), output)
            saved = json.loads((output / 'manifest.json').read_text())
        return calls, chosen, saved

    def test_qualified_capture(self):
        calls, chosen, saved = self.invoke()
        self.assertEqual(len(calls), 4)
        self.assertEqual(len(chosen), 2)
        self.assertEqual([s['sha256'] for s in saved['segments']],
                         [s['sha256'] for s in self.segments])

    def test_wrong_alias(self):
        with self.assertRaisesRegex(RuntimeError, 'layout/range'):
            self.invoke(raw=self.raw[:16] + struct.pack('<Q', 0) + self.raw[24:])

    def test_overlapping_physical_ranges(self):
        with self.assertRaisesRegex(RuntimeError, 'Overlapping'):
            self.invoke(raw=self.raw[:32] + self.raw[:8] + self.raw[40:])

    def test_firmware_above_reserved_memory(self):
        with self.assertRaisesRegex(RuntimeError, 'layout/range'):
            self.invoke(limit=int(self.segments[0]['physical'], 16) + 0x4000)

    def test_unstable_contents(self):
        with self.assertRaisesRegex(RuntimeError, 'unstable'):
            self.invoke(unstable=True)

    def test_changed_executable(self):
        with self.assertRaisesRegex(RuntimeError, 'TEXT differs'):
            self.invoke(corrupt=True)


if __name__ == '__main__':
    unittest.main()
