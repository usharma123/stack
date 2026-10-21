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


class OptionsTest(unittest.TestCase):
    """Planned model options must reach the agent's argv unchanged, or the plan is refused."""

    def plan(self, command, options):
        with tempfile.TemporaryDirectory() as directory:
            return bench.make_plan(plan_args(Path(directory) / 'plan.json', options=json.dumps(options),
                                             agent_command=json.dumps(command)))

    def test_review_counterexample_literal_low_against_planned_high_is_refused(self):
        with self.assertRaisesRegex(ValueError, 'never reach the agent'):
            self.plan(['agent', '--model', '{model}', '--reasoning-effort', 'low', '--prompt-file', '{prompt_file}'],
                      {'reasoningEffort': 'high'})

    def test_literal_flag_beside_the_placeholder_is_refused(self):
        for command in [['agent', '--reasoning-effort', 'low', '--effort', '{option:reasoningEffort}'],
                        ['agent', '--reasoning_effort=low', '-x', '{option:reasoningEffort}']]:
            with self.subTest(command), self.assertRaisesRegex(ValueError, 'contradicts or bypasses'):
                self.plan(command + ['--model', '{model}', '{prompt}'], {'reasoningEffort': 'high'})

    def test_literal_model_flag_is_refused(self):
        with self.assertRaisesRegex(ValueError, 'contradicts or bypasses'):
            self.plan(['agent', '--model', 'other-model', '--fallback', '{model}', '{prompt}'], {})

    def test_unknown_placeholder_and_unrepresentable_values_are_refused(self):
        with self.assertRaisesRegex(ValueError, 'not in --options'):
            self.plan(['agent', '--model', '{model}', '--effort', '{option:effort}', '{prompt}'], {})
        for value in [{'nested': 1}, None, float('inf')]:
            with self.subTest(value), self.assertRaises(ValueError):
                self.plan(['agent', '--model', '{model}', '--effort', '{option:effort}', '{prompt}'], {'effort': value})

    def test_bound_options_are_substituted_verbatim_in_one_pass(self):
        plan = self.plan(['agent', '--model={model}', '--reasoning-effort', '{option:reasoningEffort}',
                          '--max-turns', '{option:maxTurns}', '--fast={option:fast}', '-p', '{prompt}'],
                         {'reasoningEffort': 'high', 'maxTurns': 40, 'fast': False})
        self.assertEqual(plan['agent_options_binding'], {'reasoningEffort': [3], 'maxTurns': [5], 'fast': [6]})
        self.assertTrue(plan['agent_options_controlled'])
        argv = bench.agent_argv(plan, 'model-a', {'prompt': 'use {model} literally', 'prompt_file': '', 'workspace': '',
                                                  'transcript_dir': ''})
        self.assertEqual(argv, ['agent', '--model=model-a', '--reasoning-effort', 'high', '--max-turns', '40', '--fast=false',
                                '-p', 'use {model} literally'])

    def test_a_plan_without_options_is_labelled_uncontrolled(self):
        plan = self.plan(['agent', '--model', '{model}', '{prompt}'], {})
        self.assertFalse(plan['agent_options_controlled'])


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
