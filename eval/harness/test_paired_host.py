"""Offline receipt/order/failure tests. Never starts real services or downloads tools."""
import hashlib
import importlib.util
import json
import os
import signal
import struct
from pathlib import Path
import subprocess
import sys
import tempfile
import time
import unittest
from unittest.mock import patch
from types import SimpleNamespace

from release_artifacts import host_target

HERE = Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location('paired_host', HERE / 'paired-host.py')
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)

FAKE = '''#!/usr/bin/env python3
import json, os, pathlib, sys
args=sys.argv[1:]
if '--version' in args:
    print('fake 0.1.4'); sys.exit(0)
if 'which' in args:
    if os.environ.get('FAKE_MISSING_PROVIDER') == args[-1]: sys.exit(1)
    print(pathlib.Path(__file__).resolve()); sys.exit(0)
if 'install' in args:
    sys.exit(0)
if 'supervisor' in args:
    sys.exit(8 if os.environ.get('FAKE_SUPERVISOR_FAIL') else 0)
if 'ls' in args:
    print('[]'); sys.exit(0)
if 'compile' in args:
    pathlib.Path('stack.lock').write_text('fake lock\\n'); sys.exit(0)
if 'up' in args:
    if os.environ.get('FAKE_SETUP_FAIL'): sys.exit(9)
    print(json.dumps({'data':{'session':{'services':{name:{'data_dir':str(pathlib.Path.cwd()/name)} for name in ['postgres','redis']}}}})); sys.exit(0)
if 'down' in args:
    print(json.dumps({'data':{'confirmed':not bool(os.environ.get('FAKE_CLEANUP_FAIL'))}})); sys.exit(0)
if 'exec' in args:
    if 'sh' in args:
        print(pathlib.Path.cwd()/'postgres'); print('dir'); print(pathlib.Path.cwd()/'redis')
    elif os.environ.get('FAKE_EXEC_FAIL') and '-C' not in args:
        sys.exit(7)
    sys.exit(0)
sys.exit(2)
'''


class PairedHostTest(unittest.TestCase):
    def run_fake(self, directory, **env_overrides):
        binary = directory / 'stack'
        binary.write_text(FAKE)
        binary.chmod(0o755)
        info = directory / 'build-info.json'
        info.write_text(json.dumps({'version': '0.1.4', 'hashes': {host_target(): hashlib.sha256(binary.read_bytes()).hexdigest()}}))
        out = directory / 'out'
        command = [sys.executable, str(HERE / 'paired-host.py'), '--baseline', str(binary),
                   '--candidate', str(binary), '--mise', str(binary), '--baseline-build-info', str(info),
                   '--pairs', '3', '--warmup', '1', '--out', str(out)]
        env = dict(os.environ, **env_overrides)
        process = subprocess.run(command, env=env, capture_output=True, text=True, timeout=30)
        self.assertTrue((out / 'summary.json').exists(), process.stderr)
        return process, out, json.loads((out / 'summary.json').read_text())

    def test_receipts_order_isolation_and_cleanup(self):
        with tempfile.TemporaryDirectory() as temp:
            process, out, summary = self.run_fake(Path(temp))
            self.assertEqual(process.returncode, 0, process.stderr + process.stdout)
            self.assertTrue(summary['valid'])
            rows = [json.loads(line) for line in (out / 'events.jsonl').read_text().splitlines()]
            measured = [row for row in rows if row.get('phase') == 'measurement']
            self.assertEqual(len(measured), 12)
            for case in ['tool-only', 'verified']:
                subset = [row for row in measured if row['case'] == case]
                self.assertEqual([row['variant'] for row in subset], ['baseline', 'candidate', 'candidate', 'baseline', 'baseline', 'candidate'])
                self.assertEqual(summary['results'][case]['baseline']['n'], 3)
                self.assertEqual(summary['results'][case]['paired_candidate_minus_baseline']['n'], 3)
                self.assertEqual(len({row['cwd'] for row in subset}), 2)
            meta = json.loads((out / 'metadata.json').read_text())
            self.assertFalse(Path(meta['work']).exists())
            self.assertEqual(len(list((out / 'fixtures').rglob('stack.lock'))), 4)
            self.assertTrue(all((out / (row['log_stem'] + '.stdout')).exists() for row in rows))
            for case, tools in module.REQUIRED_PROVIDERS.items():
                for variant in ('baseline', 'candidate'):
                    artifacts = json.loads((out / f'{case}-{variant}-artifacts.json').read_text())
                    self.assertTrue(set(tools) <= artifacts.keys())
                    self.assertTrue(all(artifacts[tool]['sha256'] for tool in tools))

    def test_sample_failure_invalidates_run(self):
        with tempfile.TemporaryDirectory() as temp:
            process, out, summary = self.run_fake(Path(temp), FAKE_EXEC_FAIL='1')
            self.assertEqual(process.returncode, 1)
            self.assertFalse(summary['valid'])
            for case in summary['results'].values():
                self.assertEqual(case['baseline']['failures'], 3)
                self.assertEqual(case['baseline']['n'], 0)
                self.assertEqual(case['paired_candidate_minus_baseline']['n'], 0)

    def test_cleanup_failure_preserves_state_and_attempts_both_downs(self):
        with tempfile.TemporaryDirectory() as temp:
            process, out, summary = self.run_fake(Path(temp), FAKE_CLEANUP_FAIL='1')
            self.assertEqual(process.returncode, 1)
            self.assertTrue(summary['work_retained'])
            self.assertEqual(len(summary['cleanup_errors']), 2)
            meta = json.loads((out / 'metadata.json').read_text())
            self.assertTrue(Path(meta['work']).exists())
            import shutil
            shutil.rmtree(meta['work'])  # fake fixture has no services

    def test_setup_failure_runs_registered_cleanup(self):
        with tempfile.TemporaryDirectory() as temp:
            process, out, summary = self.run_fake(Path(temp), FAKE_SETUP_FAIL='1')
            self.assertEqual(process.returncode, 1)
            self.assertFalse(summary['valid'])
            self.assertIn('up failed', summary['error'])
            rows = [json.loads(line) for line in (out / 'events.jsonl').read_text().splitlines()]
            self.assertEqual(sum(row['label'] == 'cleanup-down' for row in rows), 1)
            self.assertFalse(summary['work_retained'])

    def test_timeout_and_percentile_definition(self):
        result, _, _ = module.invoke([sys.executable, '-c', 'import time; time.sleep(5)'], HERE, os.environ, .02)
        missing, _, _ = module.invoke(['/nonexistent/stack-benchmark-test'], HERE, os.environ, .02)
        self.assertIsNone(missing['exit_code'])
        self.assertIn('launch_error', missing)
        self.assertTrue(result['timed_out'])
        self.assertNotEqual(result['exit_code'], 0)
        self.assertEqual(module.distribution([1, 2, 3, 4, 5]), {'n': 5, 'p50_ms': 3, 'p95_ms': 5})

    def test_missing_expected_provider_fails_before_samples(self):
        for tool in ('uv', 'psql', 'redis-cli'):
            with self.subTest(tool=tool), tempfile.TemporaryDirectory() as temp:
                process, out, summary = self.run_fake(Path(temp), FAKE_MISSING_PROVIDER=tool)
                self.assertEqual(process.returncode, 1)
                self.assertIn(f'which-{tool} failed', summary['error'])
                self.assertFalse(summary['valid'])
                self.assertTrue(all(case['baseline']['attempts'] == 0 for case in summary['results'].values()))

    def test_supervisor_lookup_and_stop_fail_closed(self):
        for overrides in ({'FAKE_MISSING_PROVIDER': 'pitchfork'}, {'FAKE_SUPERVISOR_FAIL': '1'}):
            with self.subTest(overrides=overrides), tempfile.TemporaryDirectory() as temp:
                process, out, summary = self.run_fake(Path(temp), **overrides)
                self.assertEqual(process.returncode, 1)
                self.assertFalse(summary['valid'])
                self.assertTrue(summary['cleanup_errors'])
                self.assertTrue(summary['work_retained'])
                import shutil
                shutil.rmtree(json.loads((out / 'metadata.json').read_text())['work'])

    def test_timeout_detached_pipe_holder_does_not_wait_for_eof(self):
        with tempfile.TemporaryDirectory() as temp:
            pidfile = Path(temp) / 'pid'
            script = ('import subprocess,sys,time; from pathlib import Path; '
                      'p=subprocess.Popen([sys.executable,"-c","import time; time.sleep(30)"], start_new_session=True); '
                      f'Path({str(pidfile)!r}).write_text(str(p.pid)); time.sleep(30)')
            started = time.monotonic()
            try:
                result, _, _ = module.invoke([sys.executable, '-c', script], HERE, os.environ, .15)
                elapsed = time.monotonic() - started
                self.assertTrue(result['timed_out'])
                self.assertTrue(result['drain_timed_out'])
                self.assertIsNotNone(result['exit_code'])
                self.assertLess(elapsed, .8)
            finally:
                if pidfile.exists():
                    os.kill(int(pidfile.read_text()), signal.SIGKILL)


class ProviderIdentityTest(unittest.TestCase):
    def macho(self, path, prefix, instruction=b'A'):
        raw = bytearray(1120)
        struct.pack_into('<8I', raw, 0, 0xfeedfacf, 0x100000c, 0, 2, 2, 168, 0, 0)
        struct.pack_into('<4I', raw, 32, 0x1d, 16, 1024, 96)
        struct.pack_into('<II', raw, 48, 0x19, 152)
        struct.pack_into('<I', raw, 48 + 64, 1)
        entry = 48 + 72
        raw[entry:entry + 16] = b'__cstring'.ljust(16, b'\0')
        raw[entry + 16:entry + 32] = b'__TEXT'.ljust(16, b'\0')
        struct.pack_into('<Q', raw, entry + 40, 512)
        struct.pack_into('<I', raw, entry + 48, 256)
        struct.pack_into('<I', raw, entry + 64, 2)
        value = (prefix + '/share ' + prefix + '/lib').encode()
        raw[256:256 + len(value)] = value
        raw[768:769] = instruction
        struct.pack_into('>5I', raw, 1024, 0xfade0cc0, 96, 1, 0, 20)
        struct.pack_into('>9I4B', raw, 1044, 0xfade0c02, 76, 0x20400, 2, 44, 0, 0, 1, 1024, 32, 2, 0, 12)
        raw[1088:1120] = hashlib.sha256(raw[:1024]).digest()
        path.write_bytes(raw)
        return path

    def test_macho_relocation_preserves_every_other_byte(self):
        with tempfile.TemporaryDirectory() as temp, patch.object(module.subprocess, 'run', return_value=SimpleNamespace(returncode=0, stderr='')) as verify:
            root = Path(temp)
            a = self.macho(root / 'a', '/baseline/install')
            b = self.macho(root / 'b', '/candidate/longer/install')
            ra = module.relocated_binary_identity(a, '/baseline/install')
            rb = module.relocated_binary_identity(b, '/candidate/longer/install')
            self.assertNotEqual(ra['sha256'], rb['sha256'])
            self.assertEqual(ra['normalized_sha256'], rb['normalized_sha256'])
            self.assertEqual(verify.call_count, 2)
            self.macho(b, '/candidate/longer/install', instruction=b'B')
            self.assertNotEqual(ra['normalized_sha256'], module.relocated_binary_identity(b, '/candidate/longer/install')['normalized_sha256'])

    def test_macho_invalid_signature_and_unrecognized_relocation_fail(self):
        with tempfile.TemporaryDirectory() as temp:
            binary = self.macho(Path(temp) / 'a', '/isolated/install')
            with patch.object(module.subprocess, 'run', return_value=SimpleNamespace(returncode=1, stderr='invalid signature')):
                with self.assertRaisesRegex(RuntimeError, 'invalid relocated binary signature'):
                    module.relocated_binary_identity(binary, '/isolated/install')
            raw = bytearray(binary.read_bytes())
            struct.pack_into('<I', raw, 120 + 64, 0)  # not a C string section
            binary.write_bytes(raw)
            with patch.object(module.subprocess, 'run', return_value=SimpleNamespace(returncode=0, stderr='')):
                with self.assertRaisesRegex(RuntimeError, 'outside __TEXT,__cstring'):
                    module.relocated_binary_identity(binary, '/isolated/install')

    def wrapper(self, data, tool='postgres'):
        root = data / 'installs' / 'postgres' / '17.11'
        (root / 'bin').mkdir(parents=True)
        (root / '.mise-bins').mkdir()
        binary = root / 'bin' / tool
        binary.write_bytes(b'provider binary contents')
        binary.chmod(0o755)
        path = root / '.mise-bins' / tool
        path.write_text(module.CONDA_WRAPPER.format(prefix=str(root), tool=tool))
        path.chmod(0o755)
        return path

    def test_prefix_only_difference_preserves_raw_hash_and_binary_identity(self):
        with tempfile.TemporaryDirectory() as temp:
            receipts = []
            for variant in ('baseline', 'candidate'):
                data = Path(temp) / variant
                path = self.wrapper(data)
                receipts.append(module.provider_artifact(path, 'postgres', data))
            self.assertNotEqual(receipts[0]['sha256'], receipts[1]['sha256'])
            self.assertEqual(module.provider_identity(receipts[0]), module.provider_identity(receipts[1]))
            self.assertEqual(receipts[0]['wrapper']['normalization']['occurrences'], 3)
            self.assertIn('<INSTALL_PREFIX>/bin/postgres', receipts[0]['wrapper']['normalized_text'])

    def test_binary_difference_and_activation_difference_are_detected(self):
        with tempfile.TemporaryDirectory() as temp:
            data = Path(temp)
            path = self.wrapper(data)
            original = module.provider_identity(module.provider_artifact(path, 'postgres', data))
            (path.parent.parent / 'bin/postgres').write_bytes(b'different binary')
            changed = module.provider_identity(module.provider_artifact(path, 'postgres', data))
            self.assertNotEqual(original, changed)
            activation = path.parent.parent / 'etc/conda/activate.d'
            activation.mkdir(parents=True)
            (activation / 'script.sh').write_text('export CHANGED=1\n')
            self.assertNotEqual(changed, module.provider_identity(module.provider_artifact(path, 'postgres', data)))

    def test_modified_wrapper_logic_missing_target_and_escaped_prefix_rejected(self):
        for mutation in ('logic', 'target', 'prefix'):
            with self.subTest(mutation=mutation), tempfile.TemporaryDirectory() as temp:
                data = Path(temp)
                path = self.wrapper(data)
                if mutation == 'logic':
                    path.write_text(path.read_text().replace('CONDA_SHLVL=1', 'CONDA_SHLVL=2'))
                elif mutation == 'target':
                    (path.parent.parent / 'bin/postgres').unlink()
                else:
                    path.write_text(module.CONDA_WRAPPER.format(prefix=str(data), tool='postgres'))
                with self.assertRaises((RuntimeError, FileNotFoundError)):
                    module.provider_artifact(path, 'postgres', data)

    def test_direct_binary_hashes_remain_required(self):
        with tempfile.TemporaryDirectory() as temp:
            path = Path(temp) / 'uv'
            path.write_bytes(b'uv binary')
            path.chmod(0o755)
            receipt = module.provider_artifact(path, 'uv', temp)
            self.assertEqual(module.provider_identity(receipt), {'kind': 'direct', 'sha256': module.sha(path)})


if __name__ == '__main__':
    unittest.main()
