#!/usr/bin/env python3
"""Exercise Linux printk record/error semantics without injecting kernel logs."""
import errno
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

from m5_kernel_capture import KernelCapture


class KernelCaptureTest(unittest.TestCase):
    def capture(self, records):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / 'kernel-stream.log'
            with patch('m5_kernel_capture.os.open', return_value=42), \
                 patch('m5_kernel_capture.os.lseek') as seek, \
                 patch('m5_kernel_capture.os.close'), \
                 patch('m5_kernel_capture.os.read', side_effect=records + [BlockingIOError()]):
                capture = KernelCapture(path)
                seek.assert_called_once_with(42, 0, 2)
                status = capture.close()
            return status, path.read_bytes()

    def test_contiguous_records_preserve_bytes_and_extensions(self):
        records = [b'6,70,1000,-;first\n SUBSYSTEM=drm\n',
                   b'6,71,1001,c,new-field;second\n']
        status, raw = self.capture(records)
        self.assertTrue(status['complete'])
        self.assertEqual(raw, b''.join(records))
        self.assertEqual((status['first_sequence'], status['last_sequence']), (70, 71))

    def test_overrun_before_first_record_invalidates_capture(self):
        status, _ = self.capture([OSError(errno.EPIPE, 'overrun'), b'6,90,1000,-;kept\n'])
        self.assertFalse(status['complete'])
        self.assertEqual(status['overruns'], 1)
        self.assertEqual(status['records'], 1)

    def test_gap_and_duplicate_sequences(self):
        for sequence in [72, 70]:
            with self.subTest(sequence=sequence):
                status, _ = self.capture([b'6,70,1000,-;first\n',
                                          f'6,{sequence},1001,-;next\n'.encode()])
                self.assertFalse(status['complete'])
                self.assertEqual(status['sequence_errors'], 1)

    def test_malformed_record_is_retained_but_invalid(self):
        status, raw = self.capture([b'not a kmsg record\n'])
        self.assertFalse(status['complete'])
        self.assertEqual(raw, b'not a kmsg record\n')

    def test_io_error_and_empty_capture(self):
        for records in [[], [b''], [OSError(errno.EIO, 'read failure')]]:
            with self.subTest(records=records):
                status, _ = self.capture(records)
                self.assertFalse(status['complete'])

    def test_multiple_drain_batches(self):
        status, _ = self.capture([f'6,{i},1000,-;record\n'.encode() for i in range(5000)])
        self.assertTrue(status['complete'])
        self.assertEqual(status['records'], 5000)

    def test_idle_poll_then_resume_keeps_sequence(self):
        with tempfile.TemporaryDirectory() as directory, \
             patch('m5_kernel_capture.os.open', return_value=42), \
             patch('m5_kernel_capture.os.lseek'), \
             patch('m5_kernel_capture.os.close'), \
             patch('m5_kernel_capture.os.read', side_effect=[
                 b'6,70,1000,-;first\n', BlockingIOError(),
                 b'6,71,1001,-;second\n', BlockingIOError()]):
            capture = KernelCapture(Path(directory) / 'kernel-stream.log')
            self.assertTrue(capture.drain())
            self.assertFalse(capture.status['complete'])
            self.assertTrue(capture.close()['complete'])

    def test_write_failure_invalidates_capture(self):
        with tempfile.TemporaryDirectory() as directory, \
             patch('m5_kernel_capture.os.open', return_value=42), \
             patch('m5_kernel_capture.os.lseek'), \
             patch('m5_kernel_capture.os.close'), \
             patch('m5_kernel_capture.os.read', return_value=b'6,70,1000,-;first\n'):
            capture = KernelCapture(Path(directory) / 'kernel-stream.log')
            with patch.object(capture.output, 'write', side_effect=OSError(errno.ENOSPC, 'full')):
                status = capture.close()
            self.assertFalse(status['complete'])
            self.assertTrue(status['errors'])


if __name__ == '__main__':
    unittest.main()
