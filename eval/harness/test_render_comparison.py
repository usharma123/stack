"""Offline CLI regressions for the comparison renderer's receipt gate."""
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest


RENDERER = Path(__file__).with_name('render-comparison.py')
CONFIGURED = ['stack', 'mise-configured', 'flox-configured',
              'devbox-configured', 'devenv-configured', 'devenv-latest-dynamic']
ORIGINAL = ['mise', 'flox', 'devbox', 'devenv', 'devenv-latest']
COMPOSE_STEPS = (
    ['version', 'pull']
    + [f'{action}-{i}' for i in range(2) for action in ('up', 'set-pg', 'set-redis')]
    + [f'get-{service}-{i}' for i in range(2) for service in ('pg', 'redis')]
    + [f'exec-{i}' for i in range(15)]
    + ['up-again', 'down-A', 'B-survives', 'state', 'cleanup-0', 'cleanup-1']
)
PIXI_STEPS = (
    ['version']
    + [f'{app}-{action}' for app in ('appA', 'appB')
       for action in ('prepare', 'install', 'start', 'tests', 'identity')]
    + ['resolved-versions'] + [f'exec-{i}' for i in range(15)]
    + ['stop-A', 'B-survives', 'stop-B', 'lock']
)
SCENARIOS = [
    '1-independent-checkouts', '2-wrong-instance', '3-leases', '4-mcp',
    '5-oci', '6-configuration-generation', '7-deleted-project', '8-identity-probe',
]
CONTRACTS = {
    'invalid-subcommand': (2, 'usage'),
    'impossible-version': (1, 'resolve_failed'),
    'unknown-tool': (1, 'resolve_failed'),
    'conflicting-bundles': (1, 'conflict'),
}


class RendererTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.base = Path(self.temp.name) / 'results'
        self.base.mkdir()
        self.report = Path(self.temp.name) / 'report.html'

    def write(self, path, value):
        dest = self.base / path
        dest.parent.mkdir(parents=True, exist_ok=True)
        dest.write_text(value)

    def json(self, path, value):
        self.write(path, json.dumps(value))

    def valid_receipts(self):
        # Synthetic receipts exercise the real verifier without services or network.
        for tool in CONFIGURED + ORIGINAL:
            cases = ['readiness', 'tests', 'tests-B', 'B-after-A-stop',
                     'stop-A', 'stop-B']
            self.json(tool + '/steps.json', [
                {'step': case, 'code': int(tool in ORIGINAL and case == 'B-after-A-stop'),
                 'seconds': 0.01} for case in cases
            ])
            self.write(tool + '/identity-A.stdout', '/A\n')
            self.write(tool + '/identity-B.stdout', '/B\n' if tool in CONFIGURED else '/A\n')
        self.write('stack/post-stop-env.stdout',
                   'unverified.stack.invalid\nunverified.stack.invalid\n')
        for case in ['start', 'start-B']:
            self.json('stack/' + case + '.stdout',
                      {'ok': True, 'data': {'checks': [{'ready': True, 'identity': 'instance'}]}})
        for i in [0, 1]:
            for service in ['pg', 'redis']:
                self.write(f'compose/get-{service}-{i}.stdout', f'marker-{i}\n')
        self.write('compose/B-survives.stdout', 'marker-1\n')
        self.json('compose/steps.json', [
            {'step': name, 'code': 0, 'seconds': 0.01} for name in COMPOSE_STEPS
        ])
        self.write('pixi/appA-identity.stdout', '/A\n')
        self.write('pixi/appB-identity.stdout', '/B\n')
        self.json('pixi/steps.json', [
            {'step': name, 'code': 0, 'seconds': 0.01} for name in PIXI_STEPS
        ])
        self.json('release-e2e/scenarios.json', [
            {'scenario': name, 'code': 0, 'seconds': 0.01} for name in SCENARIOS
        ])
        self.write('native-scenarios.jsonl', '\n'.join(
            json.dumps({'scenario': name, 'result': 'passed', 'seconds': 0.01})
            for name in SCENARIOS
        ))
        self.json('git-lock/summary.json', {'ok': True})
        self.json('contract/summary.json', [
            {'case': name, 'exit_code': code, 'error_code': error, 'single_json_object': True}
            for name, (code, error) in CONTRACTS.items()
        ])
        for name, (_, error) in CONTRACTS.items():
            self.json('contract/' + name + '.stdout',
                      {'ok': False, 'error': {'code': error, 'message': 'Expected failure'}})
        self.json('pilot/summary.json', {
            'projects_concurrent': 4, 'rounds_per_phase': 2,
            'phases': {name: {
                'tasks_succeeded': 8, 'tasks_attempted': 8,
                'wrong_instance_incidents': 0, 'orphaned_service_processes': 0,
                'controlled_failures': {'handled': 2, 'injected': 2},
                'latency_ms': {'exec_verified': {'p50': 10}},
            } for name in ['fresh', 'cached']},
        })

    def render(self, *options, optimize=False):
        env = os.environ.copy()
        if optimize:
            env['PYTHONOPTIMIZE'] = '1'
        return subprocess.run(
            [sys.executable, str(RENDERER), str(self.base), '--report', str(self.report), *options],
            capture_output=True, text=True, env=env, timeout=10,
        )

    def assert_rejected_without_outputs(self, result):
        self.assertNotEqual(result.returncode, 0, result.stdout)
        self.assertIn('receipt validation failed', result.stderr)
        self.assertFalse(self.report.exists())
        self.assertFalse((self.base / 'summary.json').exists())

    def test_empty_results_cannot_render_success(self):
        self.assert_rejected_without_outputs(self.render())

    def test_missing_configured_receipts_cannot_render_success(self):
        self.valid_receipts()
        (self.base / 'devbox-configured/steps.json').unlink()
        self.assert_rejected_without_outputs(self.render())

    def test_malformed_receipts_cannot_render_success(self):
        self.valid_receipts()
        self.write('devbox-configured/steps.json', 'not json')
        self.assert_rejected_without_outputs(self.render())

    def test_failed_configured_readiness_is_rejected_even_with_optimization(self):
        self.valid_receipts()
        path = 'devbox-configured/steps.json'
        steps = json.loads((self.base / path).read_text())
        steps[0]['code'] = 1
        self.json(path, steps)
        self.assert_rejected_without_outputs(self.render(optimize=True))

    def test_configured_database_collision_cannot_render_success(self):
        self.valid_receipts()
        self.write('flox-configured/identity-B.stdout', '/A\n')
        self.assert_rejected_without_outputs(self.render())

    def test_each_compose_and_pixi_step_is_required_and_must_succeed(self):
        self.valid_receipts()
        for tool, names in [('compose', COMPOSE_STEPS), ('pixi', PIXI_STEPS)]:
            path = tool + '/steps.json'
            original = json.loads((self.base / path).read_text())
            for name in names:
                for mutation in ['missing', 'failed']:
                    with self.subTest(tool=tool, step=name, mutation=mutation):
                        rows = [dict(row) for row in original]
                        if mutation == 'missing':
                            rows = [row for row in rows if row['step'] != name]
                        else:
                            next(row for row in rows if row['step'] == name)['code'] = 1
                        self.json(path, rows)
                        self.assert_rejected_without_outputs(self.render())
            self.json(path, [])
            self.assert_rejected_without_outputs(self.render())
            self.json(path, original)

    def test_duplicate_steps_cannot_hide_failed_receipts(self):
        self.valid_receipts()
        for tool in CONFIGURED + ORIGINAL + ['compose', 'pixi']:
            with self.subTest(tool=tool):
                path = tool + '/steps.json'
                rows = json.loads((self.base / path).read_text())
                self.json(path, [dict(rows[0], code=1)] + rows)
                self.assert_rejected_without_outputs(self.render())
                self.json(path, rows)

    def test_exact_named_scenarios_are_required(self):
        self.valid_receipts()
        for path, native in [('release-e2e/scenarios.json', False),
                             ('native-scenarios.jsonl', True)]:
            original = [{'scenario': name, 'seconds': 0.01,
                         **({'result': 'passed'} if native else {'code': 0})}
                        for name in SCENARIOS]
            def save(rows):
                if native:
                    self.write(path, '\n'.join(json.dumps(row) for row in rows))
                else:
                    self.json(path, rows)
            for mutation in ['missing', 'duplicate', 'unknown', 'failed']:
                with self.subTest(path=path, mutation=mutation):
                    rows = [dict(row) for row in original]
                    if mutation == 'missing':
                        rows.pop()
                    elif mutation == 'duplicate':
                        rows[-1] = dict(rows[0])
                    elif mutation == 'unknown':
                        rows[-1]['scenario'] = 'unrelated'
                    else:
                        rows[-1]['result' if native else 'code'] = 'failed' if native else 1
                    save(rows)
                    self.assert_rejected_without_outputs(self.render())
            save(original)

    def test_failure_contract_fields_are_required_for_every_case(self):
        self.valid_receipts()
        path = 'contract/summary.json'
        original = json.loads((self.base / path).read_text())
        for index, row in enumerate(original):
            for field, bad in [('exit_code', 0), ('exit_code', 3), ('exit_code', True),
                               ('error_code', 'wrong'), ('single_json_object', False),
                               ('single_json_object', 1), ('case', 'unrelated')]:
                with self.subTest(case=row['case'], field=field, bad=bad):
                    rows = [dict(item) for item in original]
                    rows[index][field] = bad
                    self.json(path, rows)
                    self.assert_rejected_without_outputs(self.render())
            for field in row:
                with self.subTest(case=row['case'], missing=field):
                    rows = [dict(item) for item in original]
                    del rows[index][field]
                    self.json(path, rows)
                    self.assert_rejected_without_outputs(self.render())
        for rows in [[], original[:-1], original[:-1] + [original[0]], original + [original[0]]]:
            self.json(path, rows)
            self.assert_rejected_without_outputs(self.render())

    def test_failure_contract_stdout_must_be_one_matching_failure_object(self):
        self.valid_receipts()
        for name, (_, error) in CONTRACTS.items():
            path = 'contract/' + name + '.stdout'
            valid = {'ok': False, 'error': {'code': error, 'message': 'Expected failure'}}
            for body in [[], {'ok': True, 'error': valid['error']},
                         {'ok': 0, 'error': valid['error']}, {'ok': False},
                         {'ok': False, 'error': {'code': 'wrong', 'message': 'Failure'}},
                         {'ok': False, 'error': {'code': error}},
                         {'ok': False, 'error': {'code': error, 'message': ''}}]:
                with self.subTest(case=name, body=body):
                    self.json(path, body)
                    self.assert_rejected_without_outputs(self.render())
            self.write(path, json.dumps(valid) + '\n' + json.dumps(valid))
            self.assert_rejected_without_outputs(self.render())
            self.json(path, valid)

    def test_verified_receipts_render_and_existing_outputs_require_overwrite(self):
        self.valid_receipts()
        result = self.render()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn('all passed the tested isolation checks', self.report.read_text())
        original = self.report.read_bytes(), (self.base / 'summary.json').read_bytes()
        result = self.render()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('derived output exists', result.stderr)
        self.assertEqual(original, (self.report.read_bytes(), (self.base / 'summary.json').read_bytes()))

    def test_invalid_overwrite_preserves_existing_outputs(self):
        self.valid_receipts()
        self.report.write_text('prior report')
        self.write('summary.json', 'prior summary')
        self.write('mise-configured/identity-B.stdout', '/A\n')
        result = self.render('--overwrite')
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('receipt validation failed', result.stderr)
        self.assertEqual(self.report.read_text(), 'prior report')
        self.assertEqual((self.base / 'summary.json').read_text(), 'prior summary')


if __name__ == '__main__':
    unittest.main()
