#!/usr/bin/env python3
"""Regression for a black-image CTS Pass after a dropped fragment draw."""
import unittest

from m5_native_cts_validation import driver_errors, invalid_cases


class DriverEvidenceTests(unittest.TestCase):
    def test_black_pass_does_not_hide_dropped_draw(self):
        case = ('dEQP-GLES3.functional.fragment_ops.blend.default_framebuffer.'
                'equation_src_func_dst_func.add_zero_zero')
        log = (f"Test case '{case}'..\n"
               'agx3_pack_binary.c: no grounded AGX3 encoding for fadd\n'
               'MESA: error: asahi: dropping work: the fragment program is unusable\n'
               '  Pass (Passed)\n'
               "Test case 'dEQP-GLES3.functional.color_clear.basic'..\n"
               '  Pass (Passed)\n')
        errors = driver_errors(log)
        self.assertEqual(len(errors), 2)
        self.assertEqual(invalid_cases(errors, [case]), {case})

    def test_error_after_result_invalidates_whole_run(self):
        log = ("Test case 'dEQP-GLES3.functional.color_clear.basic'..\n"
               '  Pass (Passed)\n'
               'MESA: error: asahi: dropping work: unusable program\n')
        self.assertEqual(invalid_cases(driver_errors(log), ['first', 'second']),
                         {'first', 'second'})

    def test_successful_work_is_not_invalidated(self):
        self.assertEqual(driver_errors("Test case 'example'..\n  Pass (Passed)\n"), [])

    def test_state_or_resource_rejection_invalidates_black_pass(self):
        diagnostics = [
            'G17G Gallium draw exceeds the native triangle profile',
            'G17G Gallium could not build native vertex/fragment programs',
            'asahi: texture has no proven native descriptor encoding on G17G',
            'asahi: G17P fragment blend constants exceed the native range',
            'asahi: U8 index expansion exceeds buffer size limit',
        ]
        for diagnostic in diagnostics:
            with self.subTest(diagnostic=diagnostic):
                log = ("Test case 'rejected'..\n"
                       f'MESA: error: {diagnostic}\n'
                       '  Pass (Passed)\n'
                       "Test case 'next'..\n  Pass (Passed)\n")
                errors = driver_errors(log)
                self.assertEqual(len(errors), 1)
                self.assertEqual(invalid_cases(errors, ['rejected', 'next']),
                                 {'rejected'})

    def test_failed_submission_invalidates_case(self):
        for result in (-1, -5, 5):
            with self.subTest(result=result):
                log = ("Test case 'rejected'..\n"
                       f'NEO_SUBMIT_RESULT queue=1 batch=0 seq=1 ret={result} errno=5\n'
                       '  Pass (Passed)\n')
                self.assertEqual(invalid_cases(driver_errors(log), ['rejected']),
                                 {'rejected'})

    def test_successful_submission_ignores_stale_errno(self):
        log = ("Test case 'valid'..\n"
               'NEO_SUBMIT_RESULT queue=1 batch=0 seq=1 ret=0 errno=5\n'
               '  Pass (Passed)\n')
        self.assertEqual(driver_errors(log), [])

    def test_unscoped_admission_rejection_invalidates_run(self):
        log = 'G17G Gallium draw exceeds the native triangle profile\n'
        self.assertEqual(invalid_cases(driver_errors(log), ['first', 'second']),
                         {'first', 'second'})


if __name__ == '__main__':
    unittest.main()
