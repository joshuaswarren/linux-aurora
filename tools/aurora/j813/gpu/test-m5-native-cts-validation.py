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


if __name__ == '__main__':
    unittest.main()
