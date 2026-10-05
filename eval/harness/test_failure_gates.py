"""Offline failure injection for benchmark and release gates, including -O."""
import contextlib
import hashlib
import inspect
import io
import json
from pathlib import Path
import shlex
import subprocess
import sys
import tempfile
import types
import unittest
from unittest.mock import patch

import test_render_comparison as receipts

HARNESS = Path(__file__).resolve().parent


class FailureGateTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.base = Path(self.temp.name)
        self.binary = self.base / 'package/package/binaries/linux-arm64/stack'
        self.binary.parent.mkdir(parents=True)
        self.binary.write_bytes(b'offline binary')
        self.info = {'commit': 'current', 'hashes': {
            'linux-arm64': hashlib.sha256(self.binary.read_bytes()).hexdigest()}}
        self.save_info()
        self.calls = []

    def save_info(self):
        (self.base / 'package/package/build-info.json').write_text(json.dumps(self.info))

    def execute(self, script, fail=None, timeout=False, tool='stack', contract=None, shell_failure=None):
        artifacts = types.ModuleType('release_artifacts')
        artifacts.host_tools = lambda base: base
        artifacts.released_binary = lambda base: (self.binary, self.info)

        def run(command, **kwargs):
            frame = inspect.currentframe().f_back
            while frame and frame.f_code.co_filename != str(HARNESS / script):
                frame = frame.f_back
            label = frame.f_locals.get('label')
            if label:
                self.calls.append(label)
            code = int(fail is not None and label == fail)
            stdout = b''
            if timeout and label == fail:
                raise subprocess.TimeoutExpired(command, 1, output=b'partial', stderr=b'timeout')
            if script == 'compose-benchmark.py' and label and (label.startswith('get-') or label == 'B-survives'):
                stdout = (command[4] + '\n').encode()
            if script == 'pixi-benchmark.py' and label == 'stop-A' and shell_failure:
                # Execute the actual inner shutdown shell with offline service stubs.
                inner = shlex.split(command[-1])[-1]
                stubs = f'pg_ctl() {{ return {int(shell_failure == "postgres")}; }}; redis-cli() {{ return {int(shell_failure == "redis")}; }}; '
                result = real_run(['bash', '-c', stubs + inner], capture_output=True)
                code = result.returncode
            if script == 'release-contract.py':
                name = frame.f_locals['name']
                expected = frame.f_locals['code']
                code, body = contract if contract else (2 if name == 'invalid-subcommand' else 1, {'ok': False, 'error': {'code': expected}})
                stdout = json.dumps(body)
            if kwargs.get('text') and isinstance(stdout, bytes):
                stdout = stdout.decode()
            return subprocess.CompletedProcess(command, code, stdout, '' if kwargs.get('text') else b'')

        def output(command, **kwargs):
            return 'current\n' if command[0] == 'git' else 'offline\n'

        real_run = subprocess.run
        with patch.dict(sys.modules, {'release_artifacts': artifacts}), \
             patch.object(sys, 'argv', [script, str(self.base), tool]), \
             patch('subprocess.run', side_effect=run), \
             patch('subprocess.check_output', side_effect=output), \
             contextlib.redirect_stdout(io.StringIO()):
            path = HARNESS / script
            exec(compile(path.read_text(), str(path), 'exec', optimize=1),
                 {'__file__': str(path), '__name__': '__main__'})

    def test_compose_failed_lifecycle_cannot_claim_survival_under_optimization(self):
        for step in ['up-again', 'down-A']:
            with self.subTest(step=step):
                self.calls.clear()
                with self.assertRaises(RuntimeError):
                    self.execute('compose-benchmark.py', fail=step)
                self.assertNotIn('B-survives', self.calls)
                rows = json.loads((self.base / 'compose/steps.json').read_text())
                self.assertEqual(next(row['code'] for row in rows if row['step'] == step), 1)
                self.assertIn('cleanup-1', self.calls)
                import shutil
                shutil.rmtree(self.base / 'compose')

    def test_pixi_either_shutdown_failure_blocks_survival(self):
        for service in ['postgres', 'redis']:
            with self.subTest(service=service):
                self.calls.clear()
                with self.assertRaises(RuntimeError):
                    self.execute('pixi-benchmark.py', shell_failure=service)
                self.assertNotIn('B-survives', self.calls)
                rows = json.loads((self.base / 'pixi/steps.json').read_text())
                self.assertNotEqual(rows[-1]['code'], 0)
                import shutil
                shutil.rmtree(self.base / 'pixi')

    def test_pixi_timeout_keeps_receipt_and_blocks_survival(self):
        with self.assertRaises(RuntimeError):
            self.execute('pixi-benchmark.py', fail='stop-A', timeout=True)
        self.assertNotIn('B-survives', self.calls)
        rows = json.loads((self.base / 'pixi/steps.json').read_text())
        self.assertEqual(rows[-1]['code'], 124)
        self.assertEqual((self.base / 'pixi/stop-A.stdout').read_bytes(), b'partial')

    def test_failed_readiness_has_no_required_latency_and_is_invalid(self):
        self.execute('current-benchmark.py', fail='readiness', timeout=True)
        self.assertFalse(any(step.startswith('exec-required-') for step in self.calls))
        metadata = json.loads((self.base / 'stack/metadata.json').read_text())
        self.assertTrue(metadata['completed'])
        self.assertFalse(metadata['valid'])
        self.assertEqual(metadata['failed_steps'], ['readiness'])

    def test_healthy_run_and_original_misconfiguration_are_recordable(self):
        self.execute('current-benchmark.py')
        metadata = json.loads((self.base / 'stack/metadata.json').read_text())
        self.assertTrue(metadata['valid'])
        self.assertEqual(len([x for x in self.calls if x.startswith('exec-required-')]), 15)
        self.execute('current-benchmark.py', tool='mise', fail='B-after-A-stop')
        metadata = json.loads((self.base / 'mise/metadata.json').read_text())
        self.assertTrue(metadata['completed'])
        self.assertFalse(metadata['valid'])
        self.assertEqual(metadata['failed_steps'], ['B-after-A-stop'])
        self.assertTrue((self.base / 'mise/B-after-A-stop.stdout').exists())

    def test_each_critical_step_failure_invalidates_metadata(self):
        import shutil
        for step in ['tests', 'exec-required-00', 'setup-second', 'tests-B',
                     'stop-A', 'B-after-A-stop']:
            with self.subTest(step=step):
                self.execute('current-benchmark.py', fail=step)
                metadata = json.loads((self.base / 'stack/metadata.json').read_text())
                self.assertFalse(metadata['valid'])
                self.assertEqual(metadata['failed_steps'], [step])
                shutil.rmtree(self.base / 'stack')

    def test_early_setup_failure_is_incomplete_and_invalid(self):
        self.execute('current-benchmark.py', fail='setup-first')
        metadata = json.loads((self.base / 'stack/metadata.json').read_text())
        self.assertFalse(metadata['completed'])
        self.assertFalse(metadata['valid'])
        self.assertEqual(metadata['failed_steps'], ['setup-first'])

    def test_bad_contract_is_rejected_with_raw_output_under_optimization(self):
        import shutil
        for result in [(0, {'ok': False, 'error': {'code': 'usage'}}),
                       (2, {'ok': True, 'error': {'code': 'usage'}}),
                       (2, {'ok': False, 'error': {'code': 'wrong'}}),
                       (2, {'ok': 0, 'error': {'code': 'usage'}})]:
            with self.subTest(result=result):
                with self.assertRaises(SystemExit):
                    self.execute('release-contract.py', contract=result)
                self.assertTrue((self.base / 'contract/invalid-subcommand.stdout').exists())
                self.assertFalse((self.base / 'contract/summary.json').exists())
                shutil.rmtree(self.base / 'contract')

    def test_contract_records_observed_values(self):
        self.execute('release-contract.py')
        rows = json.loads((self.base / 'contract/summary.json').read_text())
        self.assertEqual(rows[0]['exit_code'], 2)
        self.assertEqual(rows[0]['error_code'], 'usage')

    def test_e2e_and_native_wrong_commit_stop_before_workloads(self):
        self.info['commit'] = 'wrong'
        self.save_info()
        for script in ['release-e2e.py', 'release-native.py']:
            with self.subTest(script=script), self.assertRaises(SystemExit):
                self.execute(script)
        self.assertEqual(self.calls, [])

    def test_e2e_wrong_hash_stops_before_workloads(self):
        self.info['hashes']['linux-arm64'] = 'wrong'
        self.save_info()
        with self.assertRaises(SystemExit):
            self.execute('release-e2e.py')
        self.assertEqual(self.calls, [])


class OptimizedVerifierTests(unittest.TestCase):
    setUp = receipts.RendererTests.setUp
    write = receipts.RendererTests.write
    json = receipts.RendererTests.json
    valid_receipts = receipts.RendererTests.valid_receipts
    render = receipts.RendererTests.render
    assert_rejected_without_outputs = receipts.RendererTests.assert_rejected_without_outputs
    def test_optimized_identity_and_pilot_checks_reject_bad_receipts(self):
        self.valid_receipts()
        self.write('pixi/appB-identity.stdout', '/A\n')
        self.assert_rejected_without_outputs(self.render(optimize=True))
        self.write('pixi/appB-identity.stdout', '/B\n')
        self.json('git-lock/summary.json', {'ok': False})
        self.assert_rejected_without_outputs(self.render(optimize=True))
        self.json('git-lock/summary.json', {'ok': True})
        pilot = json.loads((self.base / 'pilot/summary.json').read_text())
        pilot['phases']['fresh']['wrong_instance_incidents'] = 1
        self.json('pilot/summary.json', pilot)
        self.assert_rejected_without_outputs(self.render(optimize=True))


if __name__ == '__main__':
    unittest.main()
