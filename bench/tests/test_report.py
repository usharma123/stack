"""Offline checks that the report re-derives every verdict from a trial's hashed records.

Trials here are synthetic records sealed with the harness's own `seal`; no agent, tool or
service runs. Each test changes one record, honestly or by tampering, and checks the verdict.
"""
import json
from pathlib import Path
import shutil
import sys
import tempfile
from types import SimpleNamespace
import unittest

HERE = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(HERE))
sys.path.insert(0, str(Path(__file__).resolve().parent))
import arms  # noqa: E402
import run as bench  # noqa: E402
from test_experiment import plan_args  # noqa: E402

EMPTY = {'pids': [], 'ports': [], 'owned_processes': []}
LEFT = {'pids': [4242], 'ports': [], 'owned_processes': [{'pid': 4242, 'command': '/tmp/at.x/app node server.mjs'}]}


def teardown_record(left=EMPTY, after=None):
    record = {'before': EMPTY, 'steps': {'down': 0}, 'left': left, 'ok': not any(left.values())}
    if after is not None:
        record.update(reclaimed_by_harness={'terminated': [4242], 'killed': [], 'not_signalled': []}, after_reclaim=after)
    return record


class TrialRecords:
    """Builds one trial directory the way `experiment_trial` does, then seals it."""

    def __init__(self, test, index):
        self.test, self.trial = test, test.plan['trials'][index]
        self.out = test.root / 'results' / bench.trial_key(self.trial['id'])
        self.out.mkdir(parents=True)
        self.task = self.trial['task']
        runtime = f'/tmp/at.test{index}'
        now = test.plan['created_unix_ns'] + 10
        self.summary = {'kind': 'agent_trial', 'plan_kind': test.plan['kind'], 'trial': self.trial,
                        'plan_sha256': arms.digest(test.plan_path), 'started_unix_ns': now, 'stack_sha256': test.plan['stack_sha256'],
                        'locale': test.plan['locale'], 'runtime': runtime, 'workspace': runtime + '/app'}
        self.prompt = arms.TASKS[self.task]['instruction']
        self.write_text('prompt.txt', self.prompt)
        argv = bench.agent_argv(test.plan, self.trial['model'], bench.agent_paths(self.summary, self.prompt))
        self.agent = {'argv': argv, 'cwd': runtime + '/app', 'started_unix_ns': now + 1, 'timed_out': False, 'receipt': 'agent',
                      'exit_code': 0, 'elapsed_ns': 10**9}
        self.summary['finished_agent_unix_ns'] = now + 2 + 10**9
        self.left = EMPTY
        self.agent_cleanup, self.final_cleanup = teardown_record(), None
        self.checks = {name: True for name in arms.TASKS[self.task]['required_checks']}
        self.smoke_exit, self.outcome, self.grade_cleanup = 0, 'graded', teardown_record()
        self.control = None
        self.globals_before = self.globals_after = {'.config/mise': 'absent'}

    def write_text(self, name, text):
        path = self.out / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text)

    def write(self, name, value):
        self.write_text(name, json.dumps(value))

    def step(self, name, exit_code, stdout=''):
        self.write(f'{name}.json', {'exit_code': exit_code, 'timed_out': False, 'elapsed_ns': 2 * 10**9, 'receipt': name})
        self.write_text(f'{name}.stdout', stdout)

    def graded_steps(self, prefix, checks):
        self.step(f'{prefix}-smoke_exec', self.smoke_exit if prefix == 'grade' else 0)
        result = json.dumps({'ok': all(checks.values()), 'checks': checks, 'errors': {}})
        self.step(f'{prefix}-verify', 0 if all(checks.values()) else 1, result + '\n')

    def seal(self, task_passed=None):
        for name in ['agent.stdout', 'agent.stderr', 'submission.patch']:
            self.write_text(name, f'{name} contents\n')
        self.write('agent.json', self.agent)
        self.write('left-at-agent-exit.json', self.left)
        self.write('submission.json', {'changed': ['M\tslug.mjs']})
        self.write('agent-cleanup.json', self.agent_cleanup)
        grade = {'outcome': self.outcome, 'cleanup': self.grade_cleanup, 'seconds': {'setup': 2.0}}
        self.step('grade-setup', 0)
        if self.outcome == 'graded':
            self.graded_steps('grade', self.checks)
            grade['passed'] = all(self.checks.values()) and self.smoke_exit == 0
        else:
            self.step('grade-up', 1)
            grade.update(passed=False, failed_step='up', failed_receipt='grade-up')
            if self.control is not None:
                self.step('grade-control-up', 0 if self.control else 1)
                self.graded_steps('grade-control', {name: self.control for name in self.checks})
                grade['startup_control'] = {'outcome': 'graded', 'passed': self.control, 'cleanup': teardown_record()}
        self.write('grade.json', grade)
        if self.final_cleanup is not None:
            self.write('final-cleanup.json', self.final_cleanup)
        self.write('globals-before.json', self.globals_before)
        self.write('globals-after.json', self.globals_after)
        self.write('fingerprints-after.json', self.test.plan['fingerprints'])
        self.summary.update(agent={key: self.agent.get(key) for key in ['exit_code', 'timed_out', 'elapsed_ns', 'launch_error', 'receipt']},
                            left_running_at_agent_exit=self.left, agent_cleanup=self.agent_cleanup, grade=grade,
                            task_passed=grade['passed'] if task_passed is None else task_passed, changed_files=['M\tslug.mjs'],
                            globals_changed=arms.globals_diff(self.globals_before, self.globals_after))
        if self.final_cleanup is not None:
            self.summary['final_cleanup'] = self.final_cleanup
        self.summary = bench.seal(self.summary, self.out, self.trial, self.test.plan, self.test.plan_path)
        return self.summary

    def edit_summary(self, **changes):
        """Tampering after the seal: the summary changes, its records do not."""
        summary = json.loads((self.out / 'summary.json').read_text())
        summary.update(changes)
        (self.out / 'summary.json').write_text(json.dumps(summary))


class ReportTest(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.root = Path(self.directory.name)
        self.plan_path = self.root / 'plan.json'
        self.plan = bench.make_plan(plan_args(self.plan_path, repeats=1))

    def tearDown(self):
        self.directory.cleanup()

    def records(self, index=0):
        return TrialRecords(self, index)

    def sealed(self, index=0, **attributes):
        records = self.records(index)
        task_passed = attributes.pop('task_passed', None)
        for key, value in attributes.items():
            setattr(records, key, value)
        records.seal(task_passed)
        return records

    def report(self):
        return bench.report(SimpleNamespace(plan=str(self.plan_path), results=str(self.root / 'results')))

    def review(self, records, **changes):
        review = bench.review_template(SimpleNamespace(plan=str(self.plan_path), id=records.trial['id'], results=str(self.root / 'results')))
        review.update(reviewer='test reviewer', observed_model=records.trial['model'], harness_version='1.0')
        review.update(changes)
        (records.out / 'review.json').write_text(json.dumps(review))

    def row(self, index=0):
        return self.report()['rows'][index]

    # ---- denominators and the basic path ----

    def test_missing_trials_stay_in_denominator(self):
        report = self.report()
        self.assertFalse(report['complete'])
        self.assertFalse(report['publishable'])
        self.assertEqual(report['counts']['stack']['not_run'], 1)

    def test_unreviewed_success_is_not_autonomous(self):
        self.sealed()
        row = self.row()
        self.assertEqual(row['status'], 'unreviewed')
        self.assertTrue(row['verified_task'])
        self.assertFalse(row['autonomous'])

    def test_reviewed_success_is_autonomous_and_complete_plan_is_publishable(self):
        for index in range(2):
            self.review(self.sealed(index))
        report = self.report()
        self.assertEqual([row['status'] for row in report['rows']], ['passed', 'passed'])
        self.assertTrue(all(row['autonomous'] for row in report['rows']))
        self.assertEqual(report['rows'][0]['agent_seconds'], 1.0)
        self.assertEqual(report['rows'][0]['startup_seconds'], {'setup': 2.0, 'up': None})
        self.assertTrue(report['publishable'])
        self.assertEqual(report['agent_options'], 'harness defaults (not controlled by the plan)')

    def test_services_left_running_are_not_autonomous(self):
        self.review(self.sealed(left={'pids': [123], 'ports': [], 'owned_processes': []}))
        row = self.row()
        self.assertEqual(row['status'], 'passed')
        self.assertFalse(row['autonomous'])

    def test_supervisor_left_after_down_is_not_a_service(self):
        supervisor = {'pid': 9, 'command': '/x/pitchfork supervisor run', 'reason': 'open_file'}
        self.review(self.sealed(left={'pids': [], 'ports': [], 'owned_processes': [supervisor]}))
        self.assertTrue(self.row()['autonomous'])
        self.assertTrue(self.row()['supervisor_left'])

    # ---- finding 3: cleanup and global state come from their own records ----

    def test_review_counterexample_contradictory_summary_is_invalid(self):
        # The review's probe edited only summary.json: failed cleanups, changed globals.
        records = self.sealed()
        self.review(records)
        records.edit_summary(agent_cleanup={'ok': False, 'left': {'pids': [888]}}, grade={'cleanup': {'ok': False}},
                             globals_changed=['.config/mise/config.toml'])
        row = self.row()
        self.assertEqual(row['status'], 'invalid')
        self.assertFalse(row['autonomous'])
        self.assertIn('summary agent_cleanup contradicts agent-cleanup.json', row['problems'])
        self.assertIn('summary globals_changed contradicts the global receipts', row['problems'])
        self.assertFalse(self.report()['publishable'])

    def test_changed_global_state_is_invalid_even_when_the_summary_hides_it(self):
        records = self.sealed(globals_after={'.config/mise': 'present', '.config/mise/config.toml': 'abc'})
        self.review(records)
        self.assertEqual(records.summary['valid'], False)
        records.edit_summary(valid=True, globals_changed=[], problems=[])
        row = self.row()
        self.assertEqual(row['status'], 'invalid')
        self.assertTrue(any(problem.startswith('global state changed') for problem in row['problems']))

    def test_teardown_that_needed_the_harness_is_valid_but_not_autonomous(self):
        records = self.sealed(agent_cleanup=teardown_record(LEFT, after=EMPTY), final_cleanup=teardown_record())
        self.review(records)
        row = self.row()
        self.assertEqual(row['status'], 'passed')
        self.assertFalse(row['arm_teardown_clean'])
        self.assertFalse(row['autonomous'])

    def test_processes_that_outlived_every_cleanup_make_the_trial_invalid(self):
        self.review(self.sealed(agent_cleanup=teardown_record(LEFT, after=LEFT), final_cleanup=teardown_record(LEFT, after=LEFT)))
        self.assertIn('owned processes were still running after cleanup', self.row()['problems'])

    def test_grading_runtime_leftovers_make_the_trial_invalid(self):
        self.review(self.sealed(grade_cleanup=teardown_record(LEFT, after=EMPTY)))
        self.assertIn('the grading runtime left owned processes', self.row()['problems'])

    def test_a_missing_record_is_invalid(self):
        records = self.records()
        records.seal()
        (records.out / 'agent-cleanup.json').unlink()
        summary = json.loads((records.out / 'summary.json').read_text())
        summary['evidence_sha256'].pop('agent-cleanup.json')
        (records.out / 'summary.json').write_text(json.dumps(summary))
        self.review(records)
        self.assertIn('agent-cleanup.json missing', self.row()['problems'])

    def test_unhashed_or_tampered_files_are_invalid(self):
        records = self.sealed()
        self.review(records)
        (records.out / 'grade-extra.json').write_text('{}')
        self.assertIn('grade-extra.json is not in the trial evidence', self.row()['problems'])
        (records.out / 'grade-extra.json').unlink()
        (records.out / 'agent.stdout').write_text('changed')
        self.assertEqual(self.row()['status'], 'invalid')

    def test_scripted_validation_cannot_enter_agent_results(self):
        records = self.sealed()
        records.edit_summary(kind='scripted_task_validation')
        self.review(records)
        self.assertEqual(self.row()['status'], 'invalid')

    # ---- finding 4: a submission that cannot start is a scored failure ----

    def test_failed_start_with_passing_control_is_a_scored_failure(self):
        for index in range(2):
            self.review(self.sealed(index, outcome='startup_failed', control=True))
        report = self.report()
        row = report['rows'][0]
        self.assertEqual((row['status'], row['outcome'], row['problems']), ('failed', 'startup_failed', []))
        self.assertFalse(row['verified_task'])
        self.assertEqual(report['counts'][row['arm']]['startup_failed'], 1)
        self.assertEqual(len(report['pairs']), 1)  # the pair stays in the paired results
        self.assertTrue(report['complete'])

    def test_failed_start_whose_control_also_fails_is_infrastructure(self):
        self.review(self.sealed(outcome='startup_failed', control=False))
        self.assertIn('the startup control did not pass, so the failure is not attributable to the submission', self.row()['problems'])

    def test_failed_start_without_a_control_is_invalid(self):
        self.review(self.sealed(outcome='startup_failed', control=None))
        self.assertEqual(self.row()['status'], 'invalid')

    def test_setup_or_harness_failure_is_invalid(self):
        self.review(self.sealed(outcome='setup_failed'))
        row = self.row()
        self.assertEqual(row['status'], 'invalid')
        self.assertTrue(any(problem.startswith('grading ended setup_failed') for problem in row['problems']))

    # ---- finding 7 and the grader receipts ----

    def test_failing_smoke_test_fails_the_task(self):
        self.review(self.sealed(smoke_exit=1))
        row = self.row()
        self.assertEqual(row['status'], 'failed')
        self.assertFalse(row['verified_task'])

    def test_claiming_a_pass_the_receipts_do_not_show_is_invalid(self):
        self.review(self.sealed(smoke_exit=1, task_passed=True))
        self.assertIn('task_passed disagrees with the grader receipts', self.row()['problems'])
        checks = {name: True for name in arms.TASKS['slug-unicode']['required_checks']}
        checks['case_0'] = False
        self.review(self.sealed(1, checks=checks, task_passed=True))
        self.assertEqual(self.row(1)['status'], 'invalid')

    def test_partial_check_set_does_not_pass(self):
        self.review(self.sealed(checks={'case_0': True}, task_passed=True))
        self.assertEqual(self.row()['status'], 'invalid')

    # ---- finding 9: timing and usage provenance ----

    def test_malformed_or_inconsistent_agent_timing_is_invalid(self):
        # The review's probe: agent_seconds -1. Also a run longer than the trial, and mistyped fields.
        for change in [{'elapsed_ns': -10**9}, {'elapsed_ns': 10**15}, {'elapsed_ns': 1.5e9}, {'timed_out': 'no'},
                       {'exit_code': True}]:
            with self.subTest(change):
                shutil.rmtree(self.root / 'results', ignore_errors=True)
                records = self.records()
                records.agent.update(change)
                records.seal()
                self.review(records)
                self.assertEqual(self.row()['status'], 'invalid')

    def test_summary_timing_that_differs_from_the_receipt_is_invalid(self):
        records = self.sealed()
        self.review(records)
        summary = json.loads((records.out / 'summary.json').read_text())
        records.edit_summary(agent=dict(summary['agent'], elapsed_ns=1))
        self.assertIn('summary agent result contradicts agent.json', self.row()['problems'])

    def test_usage_must_be_finite_typed_and_bound_to_a_file(self):
        records = self.sealed()
        (records.out / 'review-evidence').mkdir()
        usage = records.out / 'review-evidence/usage.json'
        usage.write_text('{"input_tokens": 1200, "output_tokens": 300}')
        source = {'path': 'review-evidence/usage.json', 'sha256': arms.digest(usage)}
        bad = [{'cost_usd': {'value': float('inf'), 'source': source}},
               {'cost_usd': {'value': -1, 'source': source}},
               {'cost_usd': {'value': 0.4, 'source': {'path': 'nonexistent.json', 'sha256': '0' * 64}}},
               {'cost_usd': {'value': 0.4, 'source': 'provider invoice'}},
               {'cost_usd': {'value': 0.4, 'source': {'path': '../plan.json', 'sha256': arms.digest(self.plan_path)}}},
               {'tokens': {'input': True, 'output': 3, 'source': source}},
               {'tokens': {'input': 1.5, 'output': 3, 'source': source}},
               {'tokens': {'input': -1, 'output': 3, 'source': source}},
               {'tokens': {'input': 10, 'bogus': 3, 'source': source}},
               {'tokens': {'input': 10, 'source': {'path': 'review-evidence/usage.json', 'sha256': '1' * 64}}}]
        for change in bad:
            with self.subTest(change):
                self.review(records, **change)
                self.assertEqual(self.row()['status'], 'review_invalid')
        self.review(records, tokens={'input': 1200, 'output': 300, 'source': source}, cost_usd=None)
        self.assertEqual(self.row()['status'], 'passed')
        self.review(records, tokens=None, cost_usd={'value': 0.04, 'source': {'path': 'agent.stdout',
                                                                           'sha256': arms.digest(records.out / 'agent.stdout')}})
        self.assertEqual(self.row()['status'], 'passed')

    def test_malformed_reviews_are_rejected(self):
        records = self.sealed()
        window = self.plan['created_unix_ns'] + 500
        bad = [{'observed_model': 'other-model'}, {'observed_model': None}, {'reviewer': ''}, {'harness_version': None},
               {'interventions': [{'kind': 'hint', 'reason': 'x', 'active_seconds': 5, 'at_unix_ns': window}]},
               {'help_requests': 'asked twice'}, {'transcript_sha256': {}}, {'trial_summary_sha256': '0' * 64},
               {'observed_options': 'high'}]
        for change in bad:
            with self.subTest(change):
                self.review(records, **change)
                self.assertEqual(self.row()['status'], 'review_invalid')
                self.assertFalse(self.row()['autonomous'])

    # ---- finding 5: the invocation is the planned one ----

    def test_an_invocation_other_than_the_planned_one_is_invalid(self):
        records = self.records()
        records.agent['argv'] = records.agent['argv'] + ['--reasoning-effort', 'low']
        records.seal()
        self.review(records)
        self.assertIn('agent invocation differs from the planned command, model or options', self.row()['problems'])

    def test_planned_options_need_the_observed_options_in_the_review(self):
        self.plan_path = self.root / 'options-plan.json'
        command = ['agent', '--model', '{model}', '--reasoning-effort', '{option:reasoningEffort}', '--prompt-file', '{prompt_file}']
        self.plan = bench.make_plan(plan_args(self.plan_path, repeats=1, options='{"reasoningEffort": "high"}',
                                              agent_command=json.dumps(command)))
        records = self.sealed()
        self.assertIn('high', records.agent['argv'])
        self.review(records)  # observed_options left null, as the template leaves it
        self.assertEqual(self.row()['status'], 'review_invalid')
        self.review(records, observed_options={'reasoningEffort': 'low'})
        self.assertEqual(self.row()['status'], 'review_invalid')
        self.review(records, observed_options={'reasoningEffort': 'high'})
        self.assertEqual(self.row()['status'], 'passed')
        self.assertEqual(self.report()['agent_options'], {'reasoningEffort': 'high'})

    def test_selftest_plan_is_never_publishable(self):
        from test_experiment import SELFTEST
        plan = bench.make_plan(plan_args(self.root / 'selftest.json', kind='harness_selftest', agent_command=SELFTEST, repeats=1))
        report = bench.report(SimpleNamespace(plan=str(self.root / 'selftest.json'), results=str(self.root / 'none')))
        self.assertEqual(plan['kind'], 'harness_selftest')
        self.assertFalse(report['publishable'])


if __name__ == '__main__':
    unittest.main()
