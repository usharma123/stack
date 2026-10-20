"""Offline checks for the benchmark harness: no network, services or agents."""
import json
import os
from pathlib import Path
import shutil
import signal
import subprocess
import sys
import tempfile
import threading
import time
from types import SimpleNamespace
import unittest

HERE = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(HERE))
import arms  # noqa: E402
import run as bench  # noqa: E402

AGENT = json.dumps(['agent', '--model', '{model}', '--prompt-file', '{prompt_file}'])
SELFTEST = json.dumps([sys.executable, str(HERE / 'selftest_agent.py'), '--mode', 'reference', '--model', '{model}',
                       '--prompt-file', '{prompt_file}', '--transcript-dir', '{transcript_dir}'])


def plan_args(out, **overrides):
    values = dict(kind='agent_experiment', seed=17, tasks=['slug-unicode'], models=['model-a'], repeats=6, out=str(out),
                  platform='test', cache='cold', harness='test-harness', options='{}', locale='{"LANG": "en_US.UTF-8"}',
                  budget=30, agent_command=AGENT, stack=sys.executable)
    values.update(overrides)
    return SimpleNamespace(**values)


class PlanTest(unittest.TestCase):
    def test_pairs_have_both_arms_and_balanced_order(self):
        with tempfile.TemporaryDirectory() as directory:
            plan = bench.make_plan(plan_args(Path(directory) / 'plan.json'))
            pairs = [plan['trials'][i:i + 2] for i in range(0, 12, 2)]
            self.assertTrue(all({trial['arm'] for trial in pair} == {'stack', 'existing'} for pair in pairs))
            self.assertEqual(sum(pair[0]['arm'] == 'stack' for pair in pairs), 3)
            self.assertEqual(len({trial['id'] for trial in plan['trials']}), 12)
            self.assertEqual(plan['stack_sha256'], arms.digest(sys.executable))
            with self.assertRaises(FileExistsError):
                bench.make_plan(plan_args(Path(directory) / 'plan.json'))

    def test_plan_rejects_unbound_model_and_misplaced_selftest(self):
        with tempfile.TemporaryDirectory() as directory:
            out = Path(directory) / 'plan.json'
            with self.assertRaises(ValueError):
                bench.make_plan(plan_args(out, agent_command=json.dumps(['agent', '{prompt}'])))
            with self.assertRaises(ValueError):
                bench.make_plan(plan_args(out, agent_command=SELFTEST))
            with self.assertRaises(ValueError):
                bench.make_plan(plan_args(out, kind='harness_selftest'))
            self.assertFalse(out.exists())


class ReportTest(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.root = Path(self.directory.name)
        self.plan = bench.make_plan(plan_args(self.root / 'plan.json', repeats=1))

    def tearDown(self):
        self.directory.cleanup()

    def report(self):
        return bench.report(SimpleNamespace(plan=str(self.root / 'plan.json'), results=str(self.root / 'results')))

    def trial(self, index=0, checks=None, task_passed=True, left=None):
        trial = self.plan['trials'][index]
        out = self.root / 'results' / bench.trial_key(trial['id'])
        out.mkdir(parents=True)
        for name in ['agent.stdout', 'agent.stderr', 'submission.patch']:
            (out / name).write_text(f'{name} contents\n')
        (out / 'agent.json').write_text('{}')
        checks = checks if checks is not None else {name: True for name in arms.TASKS[trial['task']]['required_checks']}
        (out / 'grade-verify.stdout').write_text(json.dumps({'ok': all(checks.values()), 'checks': checks}) + '\n')
        (out / 'grade-verify.json').write_text(json.dumps({'exit_code': 0 if all(checks.values()) else 1, 'timed_out': False,
                                                            'receipt': 'grade-verify'}))
        now = self.plan['created_unix_ns'] + 10
        summary = {'kind': 'agent_trial', 'plan_kind': self.plan['kind'], 'trial': trial, 'plan_sha256': arms.digest(self.root / 'plan.json'),
                   'stack_sha256': self.plan['stack_sha256'], 'locale': self.plan['locale'], 'started_unix_ns': now,
                   'finished_agent_unix_ns': now + 1000, 'valid': True, 'task_passed': task_passed,
                   'agent': {'exit_code': 0, 'elapsed_ns': 10**9, 'timed_out': False},
                   'left_running_at_agent_exit': left or {'pids': [], 'ports': [], 'owned_processes': []},
                   'evidence_sha256': {path.name: arms.digest(path) for path in out.iterdir()}}
        (out / 'summary.json').write_text(json.dumps(summary))
        return trial, out

    def review(self, trial, out, **changes):
        review = bench.review_template(SimpleNamespace(plan=str(self.root / 'plan.json'), id=trial['id'], results=str(self.root / 'results')))
        review.update(reviewer='test reviewer', observed_model=trial['model'], harness_version='1.0')
        review.update(changes)
        (out / 'review.json').write_text(json.dumps(review))

    def row(self, index=0):
        return self.report()['rows'][index]

    def test_missing_trials_stay_in_denominator(self):
        report = self.report()
        self.assertFalse(report['complete'])
        self.assertFalse(report['publishable'])
        self.assertEqual(report['counts']['stack']['assigned'], 1)
        self.assertEqual(report['counts']['stack']['not_run'], 1)
        self.assertTrue(all(row['status'] == 'not_run' for row in report['rows']))

    def test_unreviewed_success_is_not_autonomous(self):
        self.trial()
        row = self.row()
        self.assertEqual(row['status'], 'unreviewed')
        self.assertTrue(row['verified_task'])
        self.assertFalse(row['autonomous'])

    def test_reviewed_success_is_autonomous_and_complete_plan_is_publishable(self):
        for index in range(2):
            self.review(*self.trial(index))
        report = self.report()
        self.assertEqual([row['status'] for row in report['rows']], ['passed', 'passed'])
        self.assertTrue(all(row['autonomous'] for row in report['rows']))
        self.assertTrue(report['publishable'])

    def test_services_left_running_are_not_autonomous(self):
        self.review(*self.trial(left={'pids': [123], 'ports': [], 'owned_processes': []}))
        row = self.row()
        self.assertEqual(row['status'], 'passed')
        self.assertFalse(row['autonomous'])

    def test_supervisor_left_after_down_is_not_a_service(self):
        supervisor = {'pid': 9, 'command': '/x/pitchfork supervisor run', 'reason': 'open_file'}
        self.review(*self.trial(left={'pids': [], 'ports': [], 'owned_processes': [supervisor]}))
        row = self.row()
        self.assertTrue(row['autonomous'])
        self.assertTrue(row['supervisor_left'])
        self.review(*self.trial(1, left={'pids': [], 'ports': [], 'owned_processes': [supervisor, {**supervisor, 'pid': 10, 'command': 'node server.mjs'}]}))
        self.assertFalse(self.row(1)['autonomous'])

    def test_forged_task_passed_disagrees_with_grader_receipt(self):
        checks = {name: True for name in arms.TASKS['slug-unicode']['required_checks']}
        checks['case_0'] = False
        self.review(*self.trial(checks=checks, task_passed=True))
        row = self.row()
        self.assertEqual(row['status'], 'invalid')
        self.assertFalse(row['verified_task'])
        self.assertIn('task_passed disagrees with the grader receipt', row['problems'])

    def test_partial_check_set_does_not_pass(self):
        self.review(*self.trial(checks={'case_0': True}, task_passed=True))
        self.assertEqual(self.row()['status'], 'invalid')

    def test_tampered_or_missing_evidence_is_invalid(self):
        trial, out = self.trial()
        self.review(trial, out)
        (out / 'agent.stdout').write_text('changed')
        self.assertEqual(self.row()['status'], 'invalid')
        trial, out = self.trial(1)
        (out / 'submission.patch').unlink()
        self.assertIn('submission.patch changed', self.row(1)['problems'])

    def test_scripted_validation_cannot_enter_agent_results(self):
        trial, out = self.trial()
        summary = json.loads((out / 'summary.json').read_text())
        summary['kind'] = 'scripted_task_validation'
        (out / 'summary.json').write_text(json.dumps(summary))
        self.review(trial, out)
        self.assertEqual(self.row()['status'], 'invalid')

    def test_malformed_reviews_are_rejected(self):
        trial, out = self.trial()
        window = self.plan['created_unix_ns'] + 500
        bad = [{'observed_model': 'other-model'}, {'reviewer': ''}, {'harness_version': None},
               {'interventions': [{'kind': 'hint', 'reason': 'x', 'active_seconds': 5, 'at_unix_ns': window}]},
               {'cost_usd': {'value': 0.4}}, {'tokens': {'input': -1, 'source': 'usage.json'}},
               {'tokens': {'input': True, 'source': 'usage.json'}}, {'help_requests': 'asked twice'},
               {'transcript_sha256': {}}, {'trial_summary_sha256': '0' * 64}]
        for change in bad:
            self.review(trial, out, **change)
            self.assertEqual(self.row()['status'], 'review_invalid', change)
            self.assertFalse(self.row()['autonomous'])
        self.review(trial, out, cost_usd={'value': 0.4, 'source': 'provider invoice'}, tokens=None)
        self.assertEqual(self.row()['status'], 'passed')

    def test_selftest_plan_is_never_publishable(self):
        plan = bench.make_plan(plan_args(self.root / 'selftest.json', kind='harness_selftest', agent_command=SELFTEST, repeats=1))
        report = bench.report(SimpleNamespace(plan=str(self.root / 'selftest.json'), results=str(self.root / 'none')))
        self.assertEqual(plan['kind'], 'harness_selftest')
        self.assertFalse(report['publishable'])


class AcceptanceTest(unittest.TestCase):
    def receipt(self, directory, payload, exit_code=0):
        (directory / 'v.stdout').write_text('noise\n' + json.dumps(payload) + '\n')
        return {'exit_code': exit_code, 'timed_out': False, 'receipt': 'v'}

    def test_exact_check_set_and_exit_code_are_required(self):
        required = arms.TASKS['catalog-sku-uniqueness']['required_checks']
        with tempfile.TemporaryDirectory() as directory:
            directory = Path(directory)
            good = {'ok': True, 'checks': {name: True for name in required}}
            self.assertTrue(bench.acceptance('catalog-sku-uniqueness', self.receipt(directory, good), directory)[0])
            self.assertFalse(bench.acceptance('catalog-sku-uniqueness', self.receipt(directory, good, 1), directory)[0])
            extra = {'ok': True, 'checks': {**good['checks'], 'bonus': True}}
            self.assertFalse(bench.acceptance('catalog-sku-uniqueness', self.receipt(directory, extra), directory)[0])
            truthy = {'ok': True, 'checks': {**good['checks'], required[0]: 1}}
            self.assertFalse(bench.acceptance('catalog-sku-uniqueness', self.receipt(directory, truthy), directory)[0])

    def test_noop_slug_fails_its_designated_check(self):
        if not shutil.which('node'):
            self.skipTest('node unavailable')
        with tempfile.TemporaryDirectory() as directory:
            shutil.copy2(HERE / 'fixtures/slug/slug.mjs', Path(directory) / 'slug.mjs')
            result = subprocess.run([sys.executable, str(HERE / 'verify.py'), 'slug-unicode', directory], capture_output=True, text=True)
            self.assertEqual(result.returncode, 1)
            self.assertFalse(json.loads(result.stdout)['checks']['case_0'])
            shutil.copy2(HERE / 'oracles/slug/slug.mjs', Path(directory) / 'slug.mjs')
            result = subprocess.run([sys.executable, str(HERE / 'verify.py'), 'slug-unicode', directory], capture_output=True, text=True)
            self.assertEqual(result.returncode, 0, result.stdout)


class InvokeTest(unittest.TestCase):
    def test_receipts_are_never_overwritten(self):
        with tempfile.TemporaryDirectory() as directory:
            receipt = Path(directory) / 'step'
            first = bench.invoke(['true'], directory, os.environ, 10, receipt)
            second = bench.invoke(['false'], directory, os.environ, 10, receipt)
            self.assertEqual((first['receipt'], second['receipt']), ('step', 'step-attempt-2'))
            self.assertEqual(json.loads((Path(directory) / 'step.json').read_text())['exit_code'], 0)

    def test_timeout_kills_the_whole_group(self):
        with tempfile.TemporaryDirectory() as directory:
            pidfile = Path(directory) / 'child'
            record = bench.invoke(['sh', '-c', f'sleep 60 & echo $! > {pidfile}; wait'], directory, os.environ, 1,
                                  Path(directory) / 'slow')
            self.assertTrue(record['timed_out'])
            time.sleep(0.2)
            self.assertFalse(arms.alive(int(pidfile.read_text())))

    def test_sigterm_cancels_and_is_not_swallowed(self):
        previous = signal.signal(signal.SIGTERM, lambda *_: (_ for _ in ()).throw(bench.Cancelled('test')))
        try:
            with tempfile.TemporaryDirectory() as directory:
                pidfile = Path(directory) / 'child'
                threading.Timer(1.0, os.kill, [os.getpid(), signal.SIGTERM]).start()
                with self.assertRaises(bench.Cancelled):
                    bench.invoke(['sh', '-c', f'sleep 60 & echo $! > {pidfile}; wait'], directory, os.environ, 30, Path(directory) / 'c')
                record = json.loads((Path(directory) / 'c.json').read_text())
                self.assertEqual(record['cancelled'], 'Cancelled')
                time.sleep(0.2)
                self.assertFalse(arms.alive(int(pidfile.read_text())))
        finally:
            signal.signal(signal.SIGTERM, previous)


class ArmsTest(unittest.TestCase):
    def test_arms_pin_identical_versions_and_backends(self):
        for project, spec in arms.PROJECTS.items():
            stack, native = arms.stack_toml(project), arms.mise_toml(project)
            for tool in spec['tools']:
                self.assertIn(f'{tool} = "{arms.PINS[tool]}"', stack)
                self.assertIn(f'{tool} = "{arms.PINS[tool]}"', native)
            for service in spec['services']:
                self.assertIn(f'version = "{arms.PINS[service]}"', stack)
                self.assertIn(f'"{arms.NATIVE_BACKENDS[service]}" = "{arms.PINS[service]}"', native)

    def test_readmes_do_not_leak_the_other_arm(self):
        for project in arms.PROJECTS:
            self.assertNotIn('stack', arms.readme(project, 'existing').lower())
            self.assertNotIn('dev.py', arms.readme(project, 'stack'))

    def test_overlay_keeps_benchmark_files_and_skips_runtime_state(self):
        with tempfile.TemporaryDirectory() as directory:
            source, target = Path(directory) / 'src', Path(directory) / 'dst'
            for relative in ['app.py', 'lib/new.py', 'seed.py', 'stack.toml', 'stack.lock', '.stack/session.json',
                             '.venv/bin/python', '.git/HEAD', 'node_modules/x/index.js', '.dev/state.json']:
                (source / relative).parent.mkdir(parents=True, exist_ok=True)
                (source / relative).write_text(relative)
            (source / 'link.py').symlink_to('/etc/hosts')
            target.mkdir()
            copied = arms.overlay(source, target)
            self.assertEqual(sorted(name for name, value in copied.items() if value != 'symlink-skipped'), ['app.py', 'lib/new.py'])
            self.assertEqual(copied['link.py'], 'symlink-skipped')
            self.assertFalse((target / 'seed.py').exists())

    def test_isolated_env_drops_host_service_and_locale_settings(self):
        saved = dict(os.environ)
        os.environ.update(PGHOST='db.example', PGCLIENTENCODING='UTF8', DATABASE_URL='postgres://x', LC_CTYPE='en_US.UTF-8',
                          LANG='fr_FR.UTF-8', MISE_DATA_DIR='/home/mise')
        try:
            env = arms.isolated_env(Path('/tmp/root'), None, {'LC_ALL': 'C'}, Path('/tmp/root/bin'))
        finally:
            os.environ.clear()
            os.environ.update(saved)
        for key in ['PGHOST', 'PGCLIENTENCODING', 'DATABASE_URL', 'LC_CTYPE', 'LANG']:
            self.assertNotIn(key, env)
        self.assertEqual(env['LC_ALL'], 'C')
        self.assertEqual(env['MISE_DATA_DIR'], '/tmp/root/mise/data')
        self.assertTrue(env['PATH'].startswith('/tmp/root/bin' + os.pathsep))

    def test_owned_processes_finds_a_child_by_working_directory(self):
        if sys.platform == 'darwin' and not shutil.which('lsof'):
            self.skipTest('lsof unavailable')
        root = Path(tempfile.mkdtemp(prefix='aut.', dir='/tmp')).resolve()
        process = subprocess.Popen(['sleep', '30'], cwd=root)
        try:
            time.sleep(0.3)
            found = [entry['pid'] for entry in arms.owned_processes(root)]
            self.assertIn(process.pid, found)
            self.assertNotIn(os.getpid(), found)
        finally:
            process.kill()
            process.wait()
            shutil.rmtree(root)


if __name__ == '__main__':
    unittest.main()
