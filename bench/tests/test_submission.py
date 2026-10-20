"""Offline checks that the recorded submission is the agent's whole change, however it was made."""
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

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


if __name__ == '__main__':
    unittest.main()
