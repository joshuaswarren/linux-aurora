#!/usr/bin/env python3
"""Check diagnostic accounting against a real software run and negative controls."""
from pathlib import Path
import sys
import unittest
from unittest.mock import patch
from cts_probe import summarize_sparse_depth


class SparseDepthResult(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.software = (Path(__file__).resolve().parent / 'sparse-depth-software-control.log').read_text()
        # Synthetic identity for parser tests only; this is not a hardware run.
        cls.hardware_fixture = cls.software.replace(
            'llvmpipe (LLVM 21.1.8, 128 bits)', 'Apple A18 Pro GPU')

    def test_software_control_preserves_untested_count(self):
        out = summarize_sparse_depth(self.software, 0, False, require_hardware=False)
        self.assertTrue(out['passed'])
        self.assertFalse(out['conformance_passed'])
        self.assertEqual(out['observations'], 65536)
        self.assertEqual(out['exact_sample_counts'], [4])
        self.assertEqual(out['untested_sample_counts'], [{'requested': 2, 'actual': 4}])

    def test_hardware_rejects_software_renderer(self):
        self.assertFalse(summarize_sparse_depth(self.software, 0, False)['passed'])

    def test_exact_two_and_four(self):
        text = self.hardware_fixture.replace(
            'sparse depth: requested 2x, got 4x; not tested',
            'sparse depth: 2x, 4 masks, both discard phases and depth passes verified')
        text = text.replace('1 exact sample counts, 65536', '2 exact sample counts, 73728')
        out = summarize_sparse_depth(text, 0, False)
        self.assertTrue(out['passed'])
        self.assertEqual(sorted(out['exact_sample_counts']), [2, 4])
        self.assertEqual(out['observations'], 73728)

    def test_partial_mask_coverage_rejected(self):
        text = self.hardware_fixture.replace('4x, 16 masks', '4x, 15 masks')
        self.assertFalse(summarize_sparse_depth(text, 0, False)['passed'])

    def test_truncated_or_duplicate_summary_rejected(self):
        text = self.hardware_fixture
        self.assertFalse(summarize_sparse_depth(text.replace('sparse depth: PASS,', 'interrupted:'), 0, False)['passed'])
        summary = next(line for line in text.splitlines() if line.startswith('sparse depth: PASS,'))
        self.assertFalse(summarize_sparse_depth(text + '\n' + summary + '\n', 0, False)['passed'])

    def test_missing_sample_count_rejected(self):
        text = '\n'.join(line for line in self.hardware_fixture.splitlines() if 'not tested' not in line)
        self.assertFalse(summarize_sparse_depth(text, 0, False)['passed'])

    def test_inconsistent_observation_count_rejected(self):
        text = self.hardware_fixture.replace('65536 sample observations', '65535 sample observations')
        self.assertFalse(summarize_sparse_depth(text, 0, False)['passed'])

    def test_process_and_fault_failures_rejected(self):
        self.assertFalse(summarize_sparse_depth(self.hardware_fixture, 1, False)['passed'])
        self.assertFalse(summarize_sparse_depth(self.hardware_fixture, 0, True)['passed'])


class RunnerArgumentContract(unittest.TestCase):
    """Execute the real argument validation, stopping before hardware access."""
    @classmethod
    def setUpClass(cls):
        source = (Path(__file__).resolve().parent / 'run-official-cts.py').read_text()
        marker = "assert Path('/proc/sys/kernel/random/boot_id')"
        assert marker in source
        prefix = source.split(marker, 1)[0]
        assert 'out.mkdir' not in prefix
        cls.code = compile(prefix, 'run-official-cts.py', 'exec')
        cls.sha = 'a' * 64

    def validate(self, mode, tokens):
        args = ['run-official-cts.py', '9f1434bf-725b-4ff1-ae51-babd4df25cb4',
                'offline-parser-control', mode, 'x11', self.sha, *tokens]
        with patch.object(sys, 'argv', args), patch('os.geteuid', return_value=0):
            exec(self.code, {})

    def test_full_mode_unchanged(self):
        self.validate('full', ['profile=gl33', 'cache=isolated'])

    def test_readback_mode_unchanged(self):
        self.validate('probe', ['profile=gl33', 'probe=' + self.sha,
                                'iterations=12000', 'cache=isolated'])

    def test_sparse_depth_mode(self):
        self.validate('sparse-depth', ['profile=gl33', 'probe-binary=' + self.sha, 'cache=isolated'])

    def test_sparse_depth_wrong_profile(self):
        with self.assertRaises(AssertionError):
            self.validate('sparse-depth', ['profile=gl30', 'probe-binary=' + self.sha])

    def test_sparse_depth_missing_identity(self):
        with self.assertRaises(AssertionError):
            self.validate('sparse-depth', ['profile=gl33'])

    def test_full_mode_rejects_probe_binary(self):
        with self.assertRaises(AssertionError):
            self.validate('full', ['profile=gl33', 'probe-binary=' + self.sha])


if __name__ == '__main__':
    unittest.main()
