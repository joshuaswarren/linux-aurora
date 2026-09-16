import unittest
import tempfile
from pathlib import Path

from cts_faults import FATAL_GPU_FAULT, scan_kernel_faults


class FatalGpuMessages(unittest.TestCase):
    def test_driver_fault_messages(self):
        for line in (
            b'asahi 482600000.gpu: GPU firmware crashed, failing all jobs',
            b'panicked at drivers/gpu/drm/asahi/gpu.rs: GPU firmware crashed',
            b'asahi 482600000.gpu: GPU fault nya~!!!!!',
            b'asahi 482600000.gpu: GPU FAULT',
            b'asahi 482600000.gpu: halted GMMU',
            b'asahi 482600000.gpu: completion timed out',
            b'asahi 482600000.gpu: retiring slot 4',
            b'dart 58200000.iommu: translation fault',
            b'asahi 482600000.gpu: G17PMARK session-crashed reason=pause-submissions-for-rebuild',
            b'asahi 482600000.gpu: G17PMARK rtkit-boot role=gfx restart_mode=0 ok=false took=1016790us',
            b'asahi 482600000.gpu: [File 328 Queue 1]: IOCTL: submit failed! (submission ID: 18203735 err: ETIME)',
            b'asahi 482600000.gpu: [File 328 Queue 1]: IOCTL: submit failed! (submission ID: 18203736 err: ENOTSUPP)',
        ):
            with self.subTest(line=line):
                self.assertIsNotNone(FATAL_GPU_FAULT.search(line))

    def test_initialization_is_not_a_crash(self):
        for line in (
            b'asahi 482600000.gpu: mapped firmware crash buffer',
            b'asahi 482600000.gpu: GPU firmware initialized',
            b'asahi 482600000.gpu: submitting 3D work',
            b'asahi 482600000.gpu: G17PMARK rtkit-boot role=gfx restart_mode=0 ok=true took=120us',
            b'mt7921e: scan timeout=0',
            b'mt7921e: ack_timeout=0 rts_timeout=0',
        ):
            with self.subTest(line=line):
                self.assertIsNone(FATAL_GPU_FAULT.search(line))

    def test_saved_failed_rebuild_overrides_clean_receipt_evidence(self):
        with tempfile.TemporaryDirectory() as directory:
            log = Path(directory)/'kernel-stream.log'
            log.write_bytes(b'asahi: GPU firmware initialized\n' +
                b'asahi: G17PMARK rtkit-boot role=gfx ok=false\n' * 20)
            result = scan_kernel_faults([log], sample_limit=2)
            self.assertTrue(result['fault_detected'])
            self.assertTrue(result['evidence_complete'])
            self.assertEqual(result['matching_lines'], 20)
            self.assertEqual(len(result['samples']), 2)
            self.assertEqual(result['samples'][0]['line'], 2)

    def test_missing_evidence_is_unknown(self):
        with tempfile.TemporaryDirectory() as directory:
            result = scan_kernel_faults([Path(directory)/'absent.log'])
            self.assertIsNone(result['fault_detected'])
            self.assertFalse(result['evidence_complete'])

    def test_partial_clean_evidence_is_incomplete(self):
        with tempfile.TemporaryDirectory() as directory:
            log = Path(directory)/'kernel-before.log'
            log.write_bytes(b'asahi: GPU firmware initialized\n')
            result = scan_kernel_faults([log, Path(directory)/'absent.log'])
            self.assertFalse(result['fault_detected'])
            self.assertFalse(result['evidence_complete'])


if __name__ == '__main__':
    unittest.main()
