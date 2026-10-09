"""Exercise filtering, bounded capture and error retention using fake sysfs."""
import hashlib
import io
from pathlib import Path
import tempfile
import unittest

from collect_asahi_devcoredumps import collect, read_bounded


class CaptureTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.sysfs = self.root / 'sysfs'
        self.output = self.root / 'evidence'
        self.sysfs.mkdir()
        self.output.mkdir()

    def dump(self, name, device='482600000.gpu', driver='asahi', data=b'crash'):
        hardware = self.root / (name + '-hardware') / device
        hardware.mkdir(parents=True)
        (hardware / 'driver').symlink_to(self.root / 'drivers' / driver)
        entry = self.sysfs / name
        entry.mkdir()
        (entry / 'failing_device').symlink_to(hardware)
        (entry / 'data').write_bytes(data)
        return entry

    def test_only_j700_asahi_dump_is_collected_without_acknowledging(self):
        own = self.dump('devcd0')
        self.dump('devcd1', driver='unrelated')
        self.dump('devcd2', device='other.gpu')
        records = collect(self.output, self.sysfs)
        self.assertEqual([r['name'] for r in records], ['devcd0'])
        self.assertEqual(records[0]['sha256'], hashlib.sha256(b'crash').hexdigest())
        self.assertEqual((self.output / 'devcd0.bin').read_bytes(), b'crash')
        self.assertEqual((own / 'data').read_bytes(), b'crash')

    def test_size_and_count_limits_are_explicit(self):
        self.dump('devcd0', data=b'0123456789')
        self.dump('devcd1')
        records = collect(self.output, self.sysfs, max_bytes=4, max_dumps=1)
        self.assertTrue(records[0]['truncated'])
        self.assertEqual((self.output / 'devcd0.bin').read_bytes(), b'0123')
        self.assertIn('count limit', records[1]['error'])
        self.assertFalse((self.output / 'devcd1.bin').exists())

    def test_read_failure_is_recorded(self):
        entry = self.dump('devcd0')
        (entry / 'data').unlink()
        records = collect(self.output, self.sysfs)
        self.assertIn('error', records[0])
        self.assertFalse((self.output / 'devcd0.bin').exists())

    def test_short_kernel_reads_do_not_look_like_end_of_dump(self):
        class ShortReads(io.BytesIO):
            def read(self, size):
                return super().read(min(size, 2))
        self.assertEqual(read_bounded(ShortReads(b'0123456789'), 7), b'0123456')
        self.assertEqual(read_bounded(ShortReads(b'0123'), 7), b'0123')


if __name__ == '__main__':
    unittest.main()
