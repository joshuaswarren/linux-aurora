#!/usr/bin/env python3
"""Negative controls for applicability review; original hardware logs stay intact."""
import importlib.util
from pathlib import Path
import tempfile
import unittest

HERE = Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location('applicability', HERE / 'audit-gl33-applicability.py')
mod = importlib.util.module_from_spec(spec)
spec.loader.exec_module(mod)


class ApplicabilityAudit(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.original = (HERE / 'unsupported-audit-cfg1-window.qpa').read_bytes()
        cls.expected = set((HERE / 'gl33-main.txt').read_text().splitlines())

    def audit_bytes(self, data):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / 'case.qpa'
            path.write_bytes(data)
            return mod.audit(path, self.expected)

    def replace_in_case(self, case, old, new):
        marker = ('#beginTestCaseResult KHR-GL33.' + case + '\n').encode()
        before, rest = self.original.split(marker, 1)
        body, after = rest.split(b'#endTestCaseResult', 1)
        self.assertEqual(body.count(old), 1)
        return before + marker + body.replace(old, new, 1) + b'#endTestCaseResult' + after

    def test_hardware_window(self):
        out = self.audit_bytes(self.original)
        self.assertEqual(out['issues'], [])
        self.assertEqual(out['counts'], {'Pass': 9579, 'NotSupported': 307})
        self.assertEqual(out['swizzle_predicate_cases_checked'], 695)

    def test_hardware_depth_target(self):
        out = mod.audit(HERE / 'unsupported-audit-cfg1-pbuffer.qpa', self.expected)
        self.assertEqual(out['issues'], [])
        self.assertEqual(out['depth_results'], {'Pass': 18})

    def test_required_swizzle_cannot_be_skipped(self):
        data = self.replace_in_case('texture_swizzle.functional_format_idx_0_target_idx_0',
                                    b'<Result StatusCode="Pass">Pass</Result>',
                                    b'<Result StatusCode="NotSupported">Target not supported</Result>')
        out = self.audit_bytes(data)
        self.assertEqual(len(out['unknown_unsupported']), 1)
        self.assertTrue(any('swizzle predicate expects Pass' in issue for issue in out['issues']))

    def test_advertised_minmax_cannot_be_dismissed_as_optional(self):
        data = self.replace_in_case('info.extensions', b'</TestCaseResult>',
                                    b'<Text>GL_ARB_texture_filter_minmax</Text></TestCaseResult>')
        out = self.audit_bytes(data)
        self.assertEqual(len(out['unknown_unsupported']), 5)

    def test_depth_skips_rejected_on_depth_target(self):
        data = self.replace_in_case('info.render_target',
                                    b'<Number Name="DepthBits" Description="Depth bits" Unit="">0</Number>',
                                    b'<Number Name="DepthBits" Description="Depth bits" Unit="">24</Number>')
        out = self.audit_bytes(data)
        self.assertEqual(len(out['unknown_unsupported']), 18)
        self.assertEqual(sum('depth-bearing target requires' in x for x in out['issues']), 18)

    def test_other_cts_revision_requires_new_source_review(self):
        data = self.original.replace(mod.CTS_COMMIT.encode(), b'0' * 40, 1)
        out = self.audit_bytes(data)
        self.assertIn('CTS release does not match the audited source revision', out['issues'])

    def test_unknown_skip_requires_review(self):
        data = self.replace_in_case('info.vendor', b'<Result StatusCode="Pass">Pass</Result>',
                                    b'<Result StatusCode="NotSupported">unexplained</Result>')
        out = self.audit_bytes(data)
        self.assertEqual(len(out['unknown_unsupported']), 1)
        self.assertTrue(out['issues'])


if __name__ == '__main__':
    unittest.main()
