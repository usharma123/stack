"""Offline checks of grade(): its gates and how it classifies a failed start.

Every runtime step is mocked: no tools, services or Stack binary run here.
"""
import json
import os
from pathlib import Path
import sys
import tempfile
import unittest
from unittest import mock

HERE = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(HERE))
import arms  # noqa: E402
import run as bench  # noqa: E402

TASK = 'ledger-note-cache'
PASSING = json.dumps({'ok': True, 'checks': {name: True for name in arms.TASKS[TASK]['required_checks']}, 'errors': {}})


class FakeRuntime:
    """Scripted step outcomes keyed by receipt name (with any attempt suffix); the default exit is 0."""

    def __init__(self, out, exit_codes=None):
        self.out, self.exit_codes, self.calls = out, exit_codes or {}, []

    def invoke(self, argv, cwd, env, timeout, receipt):
        self.calls.append(receipt.name)
        stem = receipt.name
        attempt = 1
        while (self.out / f'{stem}.json').exists():
            attempt += 1
            stem = f'{receipt.name}-attempt-{attempt}'
        code = self.exit_codes.get(stem, 0)
        (self.out / f'{stem}.stdout').write_text(PASSING + '\n' if receipt.name.endswith('verify') else '')
        record = {'exit_code': code, 'timed_out': False, 'elapsed_ns': 1, 'receipt': stem}
        (self.out / f'{stem}.json').write_text(json.dumps(record))
        return record

    def grade(self, **kwargs):
        workspace = self.out / 'workspace'
        workspace.mkdir(exist_ok=True)
        steps = {'setup': [['setup']], 'up': [['up']], 'smoke_exec': ['smoke'], 'smoke_task_loop': ['loop'], 'verify': ['verify'],
                 'down': ['down']}
        with mock.patch.object(bench, 'new_runtime', return_value=(self.out, dict(os.environ))), \
                mock.patch.object(arms, 'prepare', return_value=workspace), \
                mock.patch.object(arms, 'commands', return_value=steps), \
                mock.patch.object(arms, 'overlay', return_value={}), \
                mock.patch.object(bench, 'invoke', side_effect=self.invoke), \
                mock.patch.object(bench, 'teardown', return_value={'ok': True, 'left': {'pids': [], 'ports': [], 'owned_processes': []}}), \
                mock.patch.object(bench.time, 'sleep'):
            return bench.grade(TASK, 'existing', Path(sys.executable), self.out, 'grade', self.out / 'submission', None, {},
                               **kwargs)


class GradeTest(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.out = Path(self.directory.name)

    def tearDown(self):
        self.directory.cleanup()

    def test_broken_existing_behaviour_fails_even_if_hidden_checks_pass(self):
        result = FakeRuntime(self.out, {'grade-smoke_exec': 1}).grade()
        self.assertEqual(result['outcome'], 'graded')
        self.assertTrue(result['acceptance_passed'])
        self.assertFalse(result['smoke_exec'])
        self.assertFalse(result['passed'])

    def test_documented_task_loop_is_observed_not_gated(self):
        result = FakeRuntime(self.out, {'grade-smoke_task_loop': 1}).grade()
        self.assertFalse(result['smoke_task_loop'])
        self.assertTrue(result['passed'])

    def test_app_that_does_not_start_is_a_task_failure_when_the_reference_starts(self):
        runtime = FakeRuntime(self.out, {'grade-up': 1})
        result = runtime.grade(startup_control=True)
        self.assertEqual(result['outcome'], 'startup_failed')
        self.assertEqual(result['failed_receipt'], 'grade-up')
        self.assertEqual(result['startup_control']['outcome'], 'graded')
        self.assertTrue(result['startup_control']['passed'])
        self.assertIn('grade-control-verify', runtime.calls)
        self.assertNotIn('grade-verify', runtime.calls)  # the failed start has no verifier receipt

    def test_reference_that_also_fails_to_start_is_infrastructure(self):
        result = FakeRuntime(self.out, {'grade-up': 1, 'grade-control-up': 1}).grade(startup_control=True)
        self.assertEqual(result['outcome'], 'infrastructure_failed')

    def test_without_a_control_a_failed_start_stays_startup_failed(self):
        result = FakeRuntime(self.out, {'grade-up': 1}).grade()
        self.assertEqual(result['outcome'], 'startup_failed')
        self.assertNotIn('startup_control', result)

    def test_setup_that_fails_every_retry_is_infrastructure(self):
        runtime = FakeRuntime(self.out, {'grade-setup': 1, 'grade-setup-attempt-2': 1, 'grade-setup-attempt-3': 1})
        result = runtime.grade(startup_control=True)
        self.assertEqual(result['outcome'], 'setup_failed')
        self.assertEqual(result['setup_attempts'], 3)
        self.assertNotIn('startup_control', result)

    def test_setup_retry_that_succeeds_is_graded(self):
        result = FakeRuntime(self.out, {'grade-setup': 1}).grade()
        self.assertEqual((result['outcome'], result['setup_attempts'], result['passed']), ('graded', 2, True))
        self.assertAlmostEqual(result['seconds']['setup'], 2e-9)  # both attempts count


class HiddenDataTest(unittest.TestCase):
    """Only declared grader files are copied; caches beside them are not benchmark data (review round 2, D)."""

    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.out = Path(self.directory.name)

    def tearDown(self):
        self.directory.cleanup()

    def hidden_root(self):
        root = self.out / 'hidden'
        (root / 'ledger' / '__pycache__').mkdir(parents=True)
        (root / 'ledger' / 'seed.py').write_text('SEED = 1\n')
        (root / 'ledger' / '__pycache__' / 'seed.cpython-312.pyc').write_bytes(b'cache')
        (root / 'ledger' / 'notes.txt~').write_text('editor backup')
        return root

    def test_interpreter_cache_beside_hidden_data_is_ignored(self):
        root = self.hidden_root()
        with mock.patch.object(arms, 'HIDDEN_ROOT', root):
            result = FakeRuntime(self.out).grade()
        self.assertEqual((result['outcome'], result['passed']), ('graded', True))
        self.assertEqual(result['hidden_files'], {'seed.py': arms.digest(root / 'ledger' / 'seed.py')})
        self.assertFalse((self.out / 'workspace' / '__pycache__').exists())
        self.assertFalse((self.out / 'workspace' / 'notes.txt~').exists())
        self.assertTrue((root / 'ledger' / '__pycache__' / 'seed.cpython-312.pyc').exists())  # left as it was

    def test_this_checkout_grades_whatever_caches_it_holds(self):
        # The real hidden/ directory, including any __pycache__ earlier runs left there.
        result = FakeRuntime(self.out).grade()
        self.assertEqual((result['outcome'], result['passed']), ('graded', True))
        self.assertEqual(set(result['hidden_files']), {'seed.py'})

    def test_missing_declared_hidden_file_is_a_harness_error(self):
        root = self.hidden_root()
        (root / 'ledger' / 'seed.py').unlink()
        with mock.patch.object(arms, 'HIDDEN_ROOT', root):
            result = FakeRuntime(self.out).grade()
        self.assertEqual(result['outcome'], 'harness_error')
        self.assertIn('seed.py', result['error'])

    def test_declared_files_are_exactly_the_hidden_data(self):
        for project in arms.PROJECTS:
            directory = arms.HIDDEN_ROOT / project
            present = sorted(path.name for path in directory.iterdir()
                             if path.is_file() and path.name not in arms.RUNTIME_DIRS) if directory.is_dir() else []
            self.assertEqual(sorted(arms.HIDDEN[project]), present, project)


if __name__ == '__main__':
    unittest.main()
