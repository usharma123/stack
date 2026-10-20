"""Offline checks that the recorded submission is the agent's whole change, however it was made."""
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest
from types import SimpleNamespace
from unittest import mock

HERE = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(HERE))
import arms  # noqa: E402
import run as bench  # noqa: E402

AGENT_GIT = ['git', '-c', 'user.name=agent', '-c', 'user.email=agent@example.invalid', '-c', 'commit.gpgsign=false',
             '-c', 'core.hooksPath=/dev/null', '-c', 'init.defaultBranch=main']


class SubmissionTest(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.root = Path(self.directory.name).resolve()
        self.workspace = self.root / 'app'
        self.workspace.mkdir()
        for name, text in {'slug.mjs': 'bad\n', 'old.mjs': 'old\n', 'smoke.mjs': 'smoke\n', '.gitignore': 'node_modules/\n'}.items():
            (self.workspace / name).write_text(text)
        self.agent('init', '-q')
        self.agent('add', '-A')
        self.agent('commit', '-qm', 'Starting point')
        self.baseline = bench.keep_baseline(self.workspace, self.root)

    def tearDown(self):
        self.directory.cleanup()

    def agent(self, *argv):
        subprocess.run([*AGENT_GIT, *argv], cwd=self.workspace, check=True, capture_output=True)

    def git_state(self):
        return sorted((str(path.relative_to(self.workspace)), path.stat().st_mtime_ns) for path in (self.workspace / '.git').rglob('*'))

    def record(self):
        before = self.git_state()
        patch, record = bench.submission_record(self.baseline, self.workspace)
        self.assertEqual(self.git_state(), before, "recording the submission wrote to the agent's .git")
        return patch, record

    def test_committed_solution_is_in_the_patch(self):
        # The review's counterexample: the agent commits its fix, so `git diff HEAD` is empty.
        (self.workspace / 'slug.mjs').write_text('fixed\n')
        self.agent('commit', '-qam', 'Fix slug')
        patch, record = self.record()
        self.assertEqual(record['changed'], ['M\tslug.mjs'])
        self.assertIn(b'+fixed', patch)
        self.assertEqual(len(record['agent_commits']), 1)
        self.assertTrue(record['head_descends_from_initial'])

    def test_deletions_new_files_and_ignored_files_are_recorded(self):
        (self.workspace / 'old.mjs').unlink()
        (self.workspace / 'lib').mkdir()
        (self.workspace / 'lib/new.mjs').write_text('new\n')
        (self.workspace / '.gitignore').write_text('node_modules/\nhidden.mjs\n')
        (self.workspace / 'hidden.mjs').write_text('the grader copies this too\n')
        (self.workspace / 'node_modules/x').mkdir(parents=True)
        (self.workspace / 'node_modules/x/index.js').write_text('runtime\n')
        _, record = self.record()
        self.assertEqual(sorted(record['changed']),
                         ['A\thidden.mjs', 'A\tlib/new.mjs', 'D\told.mjs', 'M\t.gitignore'])

    def test_rewritten_and_pruned_history_cannot_hide_the_change(self):
        (self.workspace / 'slug.mjs').write_text('fixed\n')
        self.agent('add', '-A')
        self.agent('commit', '-q', '--amend', '-m', 'Starting point')  # replaces the starting commit
        self.agent('reflog', 'expire', '--expire=now', '--all')
        self.agent('gc', '-q', '--prune=now')
        patch, record = self.record()
        self.assertEqual(record['changed'], ['M\tslug.mjs'])
        self.assertFalse(record['head_descends_from_initial'])
        self.assertIsNone(record['agent_commits'])  # the starting commit is gone from the agent's .git

    def test_unchanged_checkout_has_an_empty_patch(self):
        patch, record = self.record()
        self.assertEqual((patch, record['changed']), (b'', []))

    def test_capture_is_retained_and_unchanged_when_cleanup_rewrites_source(self):
        (self.workspace / 'ignored.mjs').write_text('ignored but submitted\n')
        (self.workspace / 'old.mjs').unlink()
        patch, record = self.record()
        captured = self.root / 'harness' / 'submitted-tree'
        self.assertTrue(captured.is_dir())
        self.assertEqual(arms.tree_files(captured), record['files_after'])
        self.assertEqual((captured / 'slug.mjs').read_text(), 'bad\n')
        (self.workspace / 'slug.mjs').write_text('corrected during cleanup\n')
        (self.workspace / 'ignored.mjs').unlink()
        self.assertEqual((captured / 'slug.mjs').read_text(), 'bad\n')
        target = self.root / 'grade'
        target.mkdir()
        (target / 'old.mjs').write_text('old\n')
        copied = arms.overlay(captured, target, full_tree=True)
        self.assertEqual((target / 'slug.mjs').read_text(), 'bad\n')
        self.assertEqual(copied['old.mjs'], 'deleted')
        self.assertEqual(copied['ignored.mjs'], record['files_after']['ignored.mjs'])
        self.assertFalse((captured / 'slug.mjs').stat().st_mode & 0o222)
        self.assertFalse(captured.stat().st_mode & 0o222)
        self.assertTrue((target / 'slug.mjs').stat().st_mode & 0o200)

    def test_capture_excludes_private_files_and_does_not_follow_links(self):
        for relative in ['.env', '.env.local', '.claude/settings.json', '.ssh/config']:
            path = self.workspace / relative
            path.parent.mkdir(exist_ok=True)
            path.write_text('synthetic private fixture')
        (self.workspace / 'link').symlink_to(self.root / 'not-present')
        _, record = self.record()
        self.assertEqual(set(record['files_after']), {'slug.mjs', 'old.mjs', 'smoke.mjs', '.gitignore', 'link'})
        self.assertTrue(record['files_after']['link'].startswith('symlink:'))

    def test_evidence_sealing_never_reads_a_retained_symlink_target(self):
        external = self.root / 'external-fixture'
        external.write_text('synthetic value that must not enter evidence')
        (self.workspace / 'link').symlink_to(external)
        _, record = self.record()
        evidence = self.root / 'harness'
        original = bench.digest

        def no_links(path):
            self.assertFalse(Path(path).is_symlink(), 'evidence hashing followed an external link')
            return original(path)

        with mock.patch.object(bench, 'digest', side_effect=no_links), \
                mock.patch.object(bench, 'derive', return_value=({}, [])):
            result = bench.seal({}, evidence, {}, {}, self.root)
        self.assertNotIn('submitted-tree/link', result['evidence_sha256'])
        self.assertEqual(record['files_after']['link'], 'symlink:' + str(external))


class OverlayDeletionTest(unittest.TestCase):
    def test_full_checkout_deletions_reach_grading_but_protected_files_stay(self):
        with tempfile.TemporaryDirectory() as directory:
            source, target = Path(directory) / 'src', Path(directory) / 'dst'
            for base in [source, target]:
                base.mkdir()
                (base / 'app.py').write_text('app')
                (base / 'smoke.py').write_text('smoke')
            (target / 'legacy.py').write_text('starting file the agent deleted')
            (target / 'seed.py').write_text('grader data')
            (target / '.venv').mkdir()
            (target / '.venv/python').write_text('runtime')
            copied = arms.overlay(source, target, full_tree=True)
            self.assertEqual(copied.get('legacy.py'), 'deleted')
            self.assertFalse((target / 'legacy.py').exists())
            self.assertTrue((target / 'seed.py').exists() and (target / 'smoke.py').exists() and (target / '.venv/python').exists())
            (target / 'legacy.py').write_text('again')
            arms.overlay(source, target)  # an oracle overlay lists only its own files
            self.assertTrue((target / 'legacy.py').exists())


@unittest.skipUnless(shutil.which('node'), 'host Node required for bounded offline grader reproduction')
class SubmissionCleanupTest(unittest.TestCase):
    def test_real_trial_grades_captured_bad_code_after_teardown_writes_good_code(self):
        from test_experiment import plan_args
        with tempfile.TemporaryDirectory() as directory:
            out = Path(directory).resolve()
            reference = (HERE / 'oracles/slug/slug.mjs').read_text()
            down_script = 'from pathlib import Path\nPath("slug.mjs").write_text(' + repr(reference) + ')\n'
            agent_script = 'from pathlib import Path\nPath("dev.py").write_text(' + repr(down_script) + ')\n'
            plan_path = out / 'plan.json'
            plan = bench.make_plan(plan_args(plan_path, repeats=1, agent_command=json.dumps(
                [sys.executable, '-c', agent_script, '{model}', '{prompt_file}'])))
            trial = next(t for t in plan['trials'] if t['arm'] == 'existing')
            runtimes = []

            def runtime(prefix, task, arm, binary, cache, locale):
                root = out / f'runtime-{len(runtimes)}'
                root.mkdir()
                runtimes.append(root)
                (root / 'context.json').write_text(json.dumps({'task': task, 'arm': arm}))
                return root, {**arms.isolated_env(root, locale=locale), **bench.GIT_ENV}

            def commands(arm, project, binary, verifier_argv=None):
                steps = {'setup': [['/usr/bin/true']], 'up': [], 'smoke_exec': ['node', 'smoke.mjs'],
                         'smoke_task_loop': ['node', 'smoke.mjs'], 'down': [sys.executable, 'dev.py', 'down']}
                if verifier_argv:
                    steps['verify'] = verifier_argv
                return steps

            with mock.patch.object(bench, 'new_runtime', side_effect=runtime), \
                    mock.patch.object(arms, 'commands', side_effect=commands):
                summary = bench.experiment_trial(SimpleNamespace(plan=plan_path, id=trial['id'],
                                                                 results=out / 'results', cache_root=None))
            evidence = out / 'results' / bench.trial_key(trial['id'])
            submitted = json.loads((evidence / 'submission.json').read_text())
            self.assertEqual(submitted['changed'], ['M\tdev.py'])
            self.assertEqual((runtimes[0] / 'app/slug.mjs').read_text(), reference)
            self.assertNotEqual((evidence / 'submitted-tree/slug.mjs').read_text(), reference)
            self.assertEqual(summary['grade']['submitted_files']['slug.mjs'], submitted['files_after']['slug.mjs'])
            self.assertEqual(summary['grade']['outcome'], 'graded')
            self.assertFalse(summary['task_passed'])
            self.assertTrue(summary['valid'], summary['problems'])
            self.assertTrue(all(not arms.owned_processes(root) for root in runtimes))


if __name__ == '__main__':
    unittest.main()
