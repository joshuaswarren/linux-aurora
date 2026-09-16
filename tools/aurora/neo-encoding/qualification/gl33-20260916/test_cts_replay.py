"""Ensure quick replays cannot conceal incomplete or changed CTS work."""
import json
from pathlib import Path
import tempfile
import unittest
from cts_replay import build_replay, read_qpa, summarize_replay

CASE = 'KHR-GL33.packed_pixels.compressed_rgb'
COMMAND = ('--deqp-caselist-file=gl_cts/data/mustpass/gl/khronos_mustpass/main/gl33-main.txt '
           '--deqp-base-seed=3 --deqp-surface-width=-1 --deqp-surface-height=64 '
           '--deqp-surface-type=fbo --deqp-gl-config-id=1 --deqp-gl-context-type=egl '
           '--deqp-screen-rotation=unspecified --deqp-watchdog=disable')


def qpa(status='Fail', complete=True, command=COMMAND):
    text = '#sessionInfo commandLineParameters ' + json.dumps(command) + '\n'
    text += '#beginTestCaseResult ' + CASE + '\n'
    text += '<TestCaseResult CasePath="' + CASE + '"><Result StatusCode="' + status + '">' + status + '</Result></TestCaseResult>\n'
    return text + ('#endTestCaseResult\n#endSession\n' if complete else '')


class ReplayEvidenceTest(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.directory = Path(self.temp.name)
        self.source = self.directory/'source.qpa'
        self.source.write_text(qpa())
        self.plan = build_replay(self.source, 'gl33')
        self.sessions = [dict(round=1, group=1, qpa='result.qpa', exit_code=0)]

    def result(self, content, sessions=None):
        (self.directory/'result.qpa').write_text(content)
        return summarize_replay(self.directory, self.plan, self.sessions if sessions is None else sessions)

    def test_exact_replay_is_diagnostic_only(self):
        result = self.result(qpa('Pass'))
        self.assertTrue(result['diagnostic_passed'])
        self.assertFalse(result['conformance_passed'])

    def test_interrupted_result_is_selected_and_cannot_pass(self):
        self.source.write_text(qpa('Pass', complete=False))
        plan = build_replay(self.source, 'gl33')
        self.assertEqual(plan['original_statuses'], {'Incomplete': 1})
        self.assertFalse(self.result(qpa('Pass', complete=False))['diagnostic_passed'])

    def test_missing_duplicate_and_failed_cases_cannot_pass(self):
        for content in ('#endSession\n', qpa('Pass')+qpa('Pass'), qpa('Fail')):
            with self.subTest(content=content):
                self.assertFalse(self.result(content)['diagnostic_passed'])

    def test_changed_seed_or_newly_unsupported_cannot_pass(self):
        self.assertFalse(self.result(qpa('Pass', command=COMMAND.replace('seed=3', 'seed=1')))['diagnostic_passed'])
        self.assertFalse(self.result(qpa('NotSupported'))['diagnostic_passed'])

    def test_successful_qpa_with_failed_process_cannot_pass(self):
        self.sessions[0]['exit_code'] = 1
        self.assertFalse(self.result(qpa('Pass'))['diagnostic_passed'])

    def test_missing_repeat_cannot_pass(self):
        self.plan = build_replay(self.source, 'gl33', repeat=2)
        self.assertFalse(self.result(qpa('Pass'))['diagnostic_passed'])

    def test_malformed_xml_is_incomplete(self):
        self.source.write_text(qpa().replace('</Result>', ''))
        self.assertEqual(read_qpa(self.source)['cases'], [(CASE, 'Incomplete')])


if __name__ == '__main__':
    unittest.main()
