"""Offline checks of the regression runner's verdicts and exit status.

Checks and runtimes are mocked: no Stack binary, tool or service runs here.
"""
import json
from pathlib import Path
import sys
import tempfile
import unittest
from unittest import mock

HERE = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(HERE))
import arms  # noqa: E402
import regressions  # noqa: E402

CONTROL = {'passed': True, 'what': 'fixture works'}


class FakeRuntime:
    clean = True

    def __init__(self, binary, out, locale, cache=None):
        self.root, self.children = out, []

    def close(self):
        return {'ok': FakeRuntime.clean, 'left': [] if FakeRuntime.clean else [{'pid': 999}]}


class RegressionExitTest(unittest.TestCase):
    def run_checks(self, results, clean=True, globals_after='before'):
        FakeRuntime.clean = clean
        checks = {f'check-{index}': ('mock finding', lambda rt, result=result: dict(result)) for index, result in enumerate(results)}
        with tempfile.TemporaryDirectory() as directory:
            out = Path(directory) / 'out'
            argv = ['regressions.py', '--stack', sys.executable, '--out', str(out)]
            with mock.patch.object(sys, 'argv', argv), mock.patch.dict(regressions.CHECKS, checks, clear=True), \
                    mock.patch.object(regressions, 'Runtime', FakeRuntime), mock.patch.object(regressions, 'host_lock'), \
                    mock.patch.object(arms, 'globals_snapshot', side_effect=[{'x': 'before'}, {'x': globals_after}]):
                status = regressions.main()
            return status, json.loads((out / 'summary.json').read_text())

    def test_review_counterexample_failed_control_cleanup_and_globals(self):
        status, summary = self.run_checks([{'passed': True, 'control': {'passed': False, 'what': 'x'}}], clean=False,
                                          globals_after='after')
        self.assertEqual(status, 2)
        self.assertFalse(summary['valid'])
        self.assertEqual(summary['checks']['check-0']['verdict'], 'invalid')

    def test_each_failure_alone_makes_the_run_invalid(self):
        cases = [({'passed': True, 'control': {'passed': False, 'what': 'x'}}, True, 'before'),
                 ({'passed': True, 'control': CONTROL}, False, 'before'),
                 ({'passed': True, 'control': CONTROL}, True, 'after'),
                 ({'passed': True}, True, 'before'),
                 ({'passed': True, 'control': {'applicable': False}}, True, 'before'),
                 ({'error': 'compile failed'}, True, 'before')]
        for result, clean, globals_after in cases:
            with self.subTest(result=result, clean=clean, globals_after=globals_after):
                status, _ = self.run_checks([result], clean, globals_after)
                self.assertEqual(status, 2)

    def test_reproduced_defect_with_sound_controls_exits_1(self):
        status, summary = self.run_checks([{'passed': False, 'control': CONTROL}, {'passed': True, 'control': CONTROL}])
        self.assertEqual(status, 1)
        self.assertEqual([entry['verdict'] for entry in summary['checks'].values()], ['reproduced', 'fixed'])
        self.assertTrue(summary['valid'])

    def test_all_fixed_exits_0_and_a_reasoned_missing_control_is_allowed(self):
        status, _ = self.run_checks([{'passed': True, 'control': CONTROL},
                                     {'passed': True, 'control': {'applicable': False, 'reason': 'the check is its own control'}}])
        self.assertEqual(status, 0)

    def test_every_registered_check_reports_a_described_control(self):
        # Static: each real check names what its control shows; none is hard-coded to pass.
        source = (HERE / 'regressions.py').read_text()
        self.assertNotIn("'control_passed': True", source)
        self.assertEqual(source.count("'what':"), len(regressions.CHECKS))


if __name__ == '__main__':
    unittest.main()
