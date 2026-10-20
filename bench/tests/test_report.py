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
        self.summary['finished_unix_ns'] = now + 60 * 10**9
        self.step_started = self.summary['finished_agent_unix_ns'] + 10
        self.left = EMPTY
        self.agent_cleanup, self.final_cleanup = teardown_record(), None
        self.checks = {name: True for name in arms.TASKS[self.task]['required_checks']}
        self.smoke_exit, self.outcome, self.grade_cleanup = 0, 'graded', teardown_record()
        self.control = None
        self.globals_before = self.globals_after = {key: 'absent' for key in arms.GLOBAL_ROOTS}

    def write_text(self, name, text):
        path = self.out / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text)

    def write(self, name, value):
        self.write_text(name, json.dumps(value))

    def step(self, name, exit_code, stdout=''):
        self.write_text(f'{name}.stdout', stdout)
        self.write_text(f'{name}.stderr', '')
        self.write(f'{name}.json', {'exit_code': exit_code, 'timed_out': False, 'elapsed_ns': 2 * 10**9,
                                   'started_unix_ns': self.step_started, 'receipt': name,
                                   'argv': ['synthetic-step', name], 'cwd': self.summary['workspace'],
                                   **{f'{suffix}_sha256': arms.digest(self.out / f'{name}.{suffix}')
                                      for suffix in ['stdout', 'stderr']}})
        self.step_started += 2 * 10**9

    def graded_steps(self, prefix, checks):
        self.step(f'{prefix}-smoke_exec', self.smoke_exit if prefix == 'grade' else 0)
        self.step(f'{prefix}-smoke_task_loop', 0)
        result = json.dumps({'ok': all(checks.values()), 'checks': checks, 'errors': {}})
        self.step(f'{prefix}-verify', 0 if all(checks.values()) else 1, result + '\n')

    def seal(self, task_passed=None):
        for name in ['agent.stdout', 'agent.stderr', 'submission.patch']:
            self.write_text(name, f'{name} contents\n')
        self.agent.update({f'{suffix}_sha256': arms.digest(self.out / f'agent.{suffix}') for suffix in ['stdout', 'stderr']})
        self.write('agent.json', self.agent)
        self.write('left-at-agent-exit.json', self.left)
        self.write_text('submitted-tree/slug.mjs', 'synthetic captured submission')
        submitted = {'slug.mjs': arms.digest(self.out / 'submitted-tree/slug.mjs')}
        self.write('submission.json', {'changed': ['M\tslug.mjs'], 'tree': 'submitted-tree',
                                       'files_before': {'slug.mjs': '0' * 64}, 'files_after': submitted})
        self.write('agent-cleanup.json', self.agent_cleanup)
        self.cleanup_steps('agent-cleanup', self.agent_cleanup)
        grade = {'outcome': self.outcome, 'cleanup': self.grade_cleanup, 'seconds': {'setup': 2.0},
                 'submitted_files': submitted}
        self.step('grade-setup', 0)
        if self.outcome == 'graded':
            if arms.PROJECTS[arms.TASKS[self.task]['project']]['app']:
                self.step('grade-up', 0)
                grade['seconds']['up'] = 2.0
            self.graded_steps('grade', self.checks)
            grade['passed'] = all(self.checks.values()) and self.smoke_exit == 0
        else:
            self.step('grade-up', 1)
            grade['seconds']['up'] = 2.0
            grade.update(passed=False, failed_step='up', failed_receipt='grade-up')
            if self.control is not None:
                self.step('grade-control-setup', 0)
                self.step('grade-control-up', 0 if self.control else 1)
                self.graded_steps('grade-control', {name: self.control for name in self.checks})
                grade['startup_control'] = {'outcome': 'graded', 'passed': self.control, 'cleanup': teardown_record()}
                self.cleanup_steps('grade-control-cleanup', grade['startup_control']['cleanup'])
        self.cleanup_steps('grade-cleanup', self.grade_cleanup)
        self.write('grade.json', grade)
        if self.final_cleanup is not None:
            self.cleanup_steps('final-cleanup', self.final_cleanup)
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

    def cleanup_steps(self, prefix, cleanup):
        for role, code in cleanup.get('steps', {}).items():
            self.step(f'{prefix}-{role.replace("_", "-")}', code)

    def edit_summary(self, **changes):
        """Tampering after the seal: the summary changes, its records do not."""
        summary = json.loads((self.out / 'summary.json').read_text())
        summary.update(changes)
        (self.out / 'summary.json').write_text(json.dumps(summary))

    def replace_record(self, name, value, summary_field=None):
        """Rehash a malformed observation, so tests check schemas rather than tamper detection."""
        summary = json.loads((self.out / 'summary.json').read_text())
        self.write(name, value)
        summary['evidence_sha256'][name] = arms.digest(self.out / name)
        if summary_field:
            summary[summary_field] = value
        self.edit_summary(**summary)

    def retry(self, name, command):
        for suffix in ['stdout', 'stderr']:
            self.write_text(f'{name}.{suffix}', '')
            command[f'{suffix}_sha256'] = arms.digest(self.out / f'{name}.{suffix}')
            summary = json.loads((self.out / 'summary.json').read_text())
            summary['evidence_sha256'][f'{name}.{suffix}'] = command[f'{suffix}_sha256']
            self.edit_summary(**summary)
        self.replace_record(name + '.json', command)


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

    def catalog_plan(self):
        self.plan_path = self.root / 'catalog-plan.json'
        self.plan = bench.make_plan(plan_args(self.plan_path, repeats=1, tasks=['catalog-sku-uniqueness']))

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
        records = self.sealed(globals_after={**{key: 'absent' for key in arms.GLOBAL_ROOTS},
                                            '.config/mise': 'present', '.config/mise/config.toml': 'abc'})
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

    def test_every_runtime_rejects_impossible_or_later_dirty_cleanup(self):
        self.catalog_plan()
        for runtime in ['agent', 'grade', 'control', 'final']:
            for cleanup in [teardown_record(EMPTY, after=LEFT), teardown_record(EMPTY, after=EMPTY),
                            teardown_record(LEFT, after=LEFT)]:
                with self.subTest(runtime=runtime, cleanup=cleanup):
                    shutil.rmtree(self.root / 'results', ignore_errors=True)
                    records = self.sealed(outcome='startup_failed', control=True)
                    if runtime in ['grade', 'control']:
                        grade = json.loads((records.out / 'grade.json').read_text())
                        target = grade if runtime == 'grade' else grade['startup_control']
                        target['cleanup'] = cleanup
                        records.replace_record('grade.json', grade, 'grade')
                    else:
                        records.replace_record(f'{runtime}-cleanup.json', cleanup, f'{runtime}_cleanup')
                    self.review(records)
                    row = self.row()
                    self.assertEqual(row['status'], 'invalid')
                    self.assertFalse(row['verified_task'])
                    self.assertFalse(row['autonomous'])
                    self.assertFalse(self.report()['publishable'])

    def test_required_observations_reject_hashed_null_arrays_and_incomplete_objects(self):
        observations = [('agent.json', 'agent'), ('globals-before.json', None), ('globals-after.json', None),
                        ('left-at-agent-exit.json', 'left_running_at_agent_exit'), ('agent-cleanup.json', 'agent_cleanup'),
                        ('grade.json', 'grade'), ('submission.json', None), ('fingerprints-after.json', None)]
        for name, field in observations:
            for value in [None, [], {}, 'missing observation']:
                with self.subTest(name=name, value=value):
                    shutil.rmtree(self.root / 'results', ignore_errors=True)
                    records = self.sealed()
                    records.replace_record(name, value, field)
                    self.review(records)
                    row = self.row()
                    self.assertEqual(row['status'], 'invalid')
                    self.assertFalse(row['verified_task'])
                    self.assertFalse(row['autonomous'])
                    self.assertFalse(self.report()['publishable'])

    def test_command_observations_reject_boolean_exits_and_missing_required_fields(self):
        for name in ['agent.json', 'grade-smoke_exec.json', 'grade-verify.json', 'grade-setup.json']:
            for change in [{'exit_code': False}, {'argv': None}, {'cwd': None}, {'elapsed_ns': None},
                           {'started_unix_ns': None}, {'receipt': None}, {'timed_out': 'no'}]:
                with self.subTest(name=name, change=change):
                    shutil.rmtree(self.root / 'results', ignore_errors=True)
                    records = self.sealed()
                    value = json.loads((records.out / name).read_text())
                    records.replace_record(name, dict(value, **change))
                    self.review(records)
                    self.assertEqual(self.row()['status'], 'invalid')
                    self.assertFalse(self.row()['verified_task'])

    def test_unresolved_agent_command_cleanup_cannot_receive_completion_credit(self):
        records = self.sealed()
        agent = dict(records.agent, stopped={'terminated': [42], 'killed': [], 'survivors': [],
                                           'unresolved': [{'pid': 42}]})
        records.replace_record('agent.json', agent)
        self.review(records)
        self.assertEqual(self.row()['status'], 'invalid')
        self.assertFalse(self.row()['verified_task'])

    def test_uncertainty_in_every_command_role_survives_later_empty_cleanup(self):
        roles = ['agent', 'agent-cleanup-down', 'agent-cleanup-supervisor-stop', 'final-cleanup-down',
                 'grade-smoke_task_loop', 'grade-cleanup-down', 'grade-control-smoke_task_loop',
                 'grade-control-cleanup-supervisor-stop', 'grade-setup-attempt-2']
        for stem in roles:
            for key, value in [('survivors', [42]), ('unresolved', [{'pid': 42}])]:
                with self.subTest(stem=stem, key=key):
                    shutil.rmtree(self.root / 'results', ignore_errors=True)
                    records = self.sealed()
                    command = json.loads((records.out / 'grade-smoke_exec.json').read_text())
                    stopped = {'terminated': [42], 'killed': [], 'survivors': [], 'unresolved': []}
                    stopped[key] = value
                    records.replace_record(stem + '.json', dict(command, receipt=stem, stopped=stopped))
                    self.review(records)
                    row = self.row()
                    self.assertEqual(row['status'], 'invalid')
                    self.assertFalse(row['verified_task'])
                    self.assertFalse(row['autonomous'])
                    self.assertTrue(any('command cleanup' in problem for problem in row['problems']))

    def test_cleanup_requires_complete_typed_residue_and_consistent_ok(self):
        malformed = [{}, {'pids': [], 'ports': []}, dict(EMPTY, pids=''), dict(EMPTY, ports=[True]),
                     dict(EMPTY, owned_processes=[{}]), dict(EMPTY, pids=[-1])]
        for left in malformed:
            for target in ['agent', 'grade']:
                with self.subTest(left=left, target=target):
                    shutil.rmtree(self.root / 'results', ignore_errors=True)
                    cleanup = teardown_record(left)
                    records = self.sealed(**{f'{target}_cleanup': cleanup})
                    self.review(records)
                    self.assertEqual(self.row()['status'], 'invalid')
        self.assertFalse(bench.clean({'left': {}}))
        self.assertFalse(bench.restored({'left': LEFT, 'after_reclaim': {}}))

    def test_latest_cleanup_must_show_recovery_even_after_an_earlier_reclaim_succeeded(self):
        self.review(self.sealed(agent_cleanup=teardown_record(LEFT, after=EMPTY),
                                final_cleanup=teardown_record(LEFT, after=LEFT)))
        row = self.row()
        self.assertEqual(row['status'], 'invalid')
        self.assertIn('owned processes were still running after cleanup', row['problems'])

    def test_final_cleanup_is_required_typed_and_compared_independently(self):
        for value in [None, {}, {'left': {}}, teardown_record(LEFT)]:
            with self.subTest(value=value):
                shutil.rmtree(self.root / 'results', ignore_errors=True)
                records = self.sealed(final_cleanup=teardown_record())
                records.replace_record('final-cleanup.json', value)
                self.review(records)
                self.assertEqual(self.row()['status'], 'invalid')
                self.assertFalse(self.row()['verified_task'])
        shutil.rmtree(self.root / 'results')
        records = self.sealed(final_cleanup=teardown_record())
        (records.out / 'final-cleanup.json').unlink()
        summary = json.loads((records.out / 'summary.json').read_text())
        summary['evidence_sha256'].pop('final-cleanup.json')
        records.edit_summary(**summary)
        self.assertIn('final-cleanup.json missing', self.row()['problems'])

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
        self.catalog_plan()
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
        self.catalog_plan()
        self.review(self.sealed(outcome='startup_failed', control=False))
        self.assertIn('the startup control did not pass, so the failure is not attributable to the submission', self.row()['problems'])

    def test_failed_start_without_a_control_is_invalid(self):
        self.catalog_plan()
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

    def test_graded_bytes_must_match_captured_submission_and_retained_tree(self):
        for scenario in ['grade-mismatch', 'snapshot-mutated', 'snapshot-missing', 'manifest-malformed']:
            with self.subTest(scenario=scenario):
                shutil.rmtree(self.root / 'results', ignore_errors=True)
                records = self.sealed()
                if scenario == 'grade-mismatch':
                    grade = json.loads((records.out / 'grade.json').read_text())
                    grade['submitted_files']['slug.mjs'] = '1' * 64
                    records.replace_record('grade.json', grade, 'grade')
                elif scenario == 'manifest-malformed':
                    submission = json.loads((records.out / 'submission.json').read_text())
                    submission['files_after'] = {'../outside': '1' * 64}
                    records.replace_record('submission.json', submission)
                else:
                    path = records.out / 'submitted-tree/slug.mjs'
                    if scenario == 'snapshot-missing':
                        path.unlink()
                    else:
                        records.write_text('submitted-tree/slug.mjs', 'later bytes')
                    summary = json.loads((records.out / 'summary.json').read_text())
                    if path.exists():
                        summary['evidence_sha256']['submitted-tree/slug.mjs'] = arms.digest(path)
                    else:
                        summary['evidence_sha256'].pop('submitted-tree/slug.mjs')
                    records.edit_summary(**summary)
                self.review(records)
                self.assertEqual(self.row()['status'], 'invalid')
                self.assertFalse(self.row()['verified_task'])
                self.review(self.sealed(1))
                self.assertEqual(self.row(1)['status'], 'passed')

    def test_verifier_cannot_redirect_to_agent_stdout_even_when_both_are_hashed(self):
        self.catalog_plan()
        for control in [False, True]:
            with self.subTest(control=control):
                shutil.rmtree(self.root / 'results', ignore_errors=True)
                records = self.sealed(outcome='startup_failed' if control else 'graded', control=True)
                prefix = 'grade-control' if control else 'grade'
                records.write_text('agent.stdout', (records.out / f'{prefix}-verify.stdout').read_text())
                records.write_text(f'{prefix}-verify.stdout', '{}\n')
                summary = json.loads((records.out / 'summary.json').read_text())
                for name in ['agent.stdout', f'{prefix}-verify.stdout']:
                    summary['evidence_sha256'][name] = arms.digest(records.out / name)
                records.edit_summary(**summary)
                verify = json.loads((records.out / f'{prefix}-verify.json').read_text())
                records.replace_record(f'{prefix}-verify.json', dict(verify, receipt='agent',
                                       stdout_sha256=arms.digest(records.out / 'agent.stdout')))
                self.review(records)
                self.assertEqual(self.row()['status'], 'invalid')
                self.assertFalse(self.row()['verified_task'])
                self.assertFalse(self.report()['publishable'])

    def test_all_command_roles_require_their_own_outputs_and_receipt_stem(self):
        for stem in ['agent', 'grade-setup', 'grade-smoke_exec', 'grade-smoke_task_loop', 'grade-verify',
                     'agent-cleanup-down', 'grade-cleanup-down']:
            for change in [{'receipt': 'agent' if stem != 'agent' else 'grade-verify'},
                           {'stdout_sha256': '0' * 64}, {'stderr_sha256': None}]:
                with self.subTest(stem=stem, change=change):
                    shutil.rmtree(self.root / 'results', ignore_errors=True)
                    records = self.sealed()
                    command = json.loads((records.out / (stem + '.json')).read_text())
                    records.replace_record(stem + '.json', dict(command, **change))
                    self.review(records)
                    self.assertEqual(self.row()['status'], 'invalid')
                    self.assertFalse(self.row()['verified_task'])

    def test_required_cleanup_and_observation_commands_cannot_disappear(self):
        for name in ['agent-cleanup-down.json', 'grade-cleanup-down.json', 'grade-smoke_task_loop.json']:
            with self.subTest(name=name):
                shutil.rmtree(self.root / 'results', ignore_errors=True)
                records = self.sealed()
                (records.out / name).unlink()
                summary = json.loads((records.out / 'summary.json').read_text())
                summary['evidence_sha256'].pop(name)
                records.edit_summary(**summary)
                self.assertEqual(self.row()['status'], 'invalid')

    def test_observation_command_timestamp_must_be_inside_trial(self):
        records = self.sealed()
        name = 'grade-smoke_task_loop.json'
        command = json.loads((records.out / name).read_text())
        records.replace_record(name, dict(command, started_unix_ns=0, elapsed_ns=2**63-1))
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

    def test_startup_timing_is_typed_finite_nonnegative_and_inside_the_trial(self):
        for change in [{'elapsed_ns': -10**9}, {'elapsed_ns': 'oops'}, {'elapsed_ns': True}, {'elapsed_ns': 1.5},
                       {'elapsed_ns': float('inf')}, {'elapsed_ns': float('nan')}, {'elapsed_ns': 10**1000},
                       {'elapsed_ns': 10**15}, {'started_unix_ns': None}, {'started_unix_ns': -1},
                       {'started_unix_ns': self.plan['created_unix_ns'] - 1}]:
            with self.subTest(change=change):
                shutil.rmtree(self.root / 'results', ignore_errors=True)
                records = self.sealed()
                setup = json.loads((records.out / 'grade-setup.json').read_text())
                records.replace_record('grade-setup.json', {**setup, **change})
                self.review(records)
                self.review(self.sealed(1))
                report = self.report()
                row = report['rows'][0]
                self.assertEqual(row['status'], 'invalid')
                self.assertIsNone(row['startup_seconds']['setup'])
                self.assertFalse(row['verified_task'])
                self.assertFalse(row['autonomous'])
                self.assertFalse(report['publishable'])
                self.assertEqual(report['rows'][1]['status'], 'passed')

    def test_setup_receipt_cannot_disappear_with_its_hash_or_summary_seconds(self):
        for remove_seconds in [False, True]:
            with self.subTest(remove_seconds=remove_seconds):
                shutil.rmtree(self.root / 'results', ignore_errors=True)
                records = self.sealed()
                summary = json.loads((records.out / 'summary.json').read_text())
                (records.out / 'grade-setup.json').unlink()
                summary['evidence_sha256'].pop('grade-setup.json')
                if remove_seconds:
                    grade = summary['grade']
                    grade.pop('seconds')
                    records.write('grade.json', grade)
                    summary['evidence_sha256']['grade.json'] = arms.digest(records.out / 'grade.json')
                records.edit_summary(**summary)
                self.assertIn('grade-setup.json missing', self.row()['problems'])
                self.assertIsNone(self.row()['startup_seconds']['setup'])

    def test_service_start_and_control_require_setup_and_up_but_tools_only_has_no_up(self):
        self.catalog_plan()
        for outcome, name in [('graded', 'grade-up.json'), ('startup_failed', 'grade-control-setup.json')]:
            with self.subTest(outcome=outcome):
                shutil.rmtree(self.root / 'results', ignore_errors=True)
                records = self.sealed(outcome=outcome, control=True)
                summary = json.loads((records.out / 'summary.json').read_text())
                (records.out / name).unlink()
                summary['evidence_sha256'].pop(name)
                records.edit_summary(**summary)
                self.assertIn(f'{name} missing', self.row()['problems'])
                self.assertEqual(self.row()['status'], 'invalid')

    def test_startup_metrics_include_retries_and_reject_unreachable_attempts(self):
        records = self.sealed()
        setup = json.loads((records.out / 'grade-setup.json').read_text())
        records.replace_record('grade-setup.json', dict(setup, exit_code=1))
        retry = dict(setup, receipt='grade-setup-attempt-2', started_unix_ns=setup['started_unix_ns'] + setup['elapsed_ns'])
        records.retry('grade-setup-attempt-2', retry)
        grade = json.loads((records.out / 'grade.json').read_text())
        grade['seconds']['setup'] = 4.0
        records.replace_record('grade.json', grade, 'grade')
        self.review(records)
        self.assertEqual(self.row()['status'], 'passed')
        self.assertEqual(self.row()['startup_seconds']['setup'], 4.0)
        records.replace_record('grade-setup-attempt-3.json', dict(retry, elapsed_ns=-1))
        self.assertEqual(self.row()['status'], 'invalid')
        self.assertIn('grade-setup-attempt-3.json is unreachable on the recorded startup path', self.row()['problems'])

    def test_summary_startup_seconds_and_overlapping_steps_are_rejected(self):
        self.catalog_plan()
        records = self.sealed()
        grade = json.loads((records.out / 'grade.json').read_text())
        records.replace_record('grade.json', dict(grade, seconds={'setup': -1, 'up': 2}), 'grade')
        self.assertIn('grade seconds contradict startup receipts', self.row()['problems'])
        setup = json.loads((records.out / 'grade-setup.json').read_text())
        up = json.loads((records.out / 'grade-up.json').read_text())
        records.replace_record('grade-up.json', dict(up, started_unix_ns=setup['started_unix_ns']))
        self.assertTrue(any('grade-up.json malformed startup timing' in p for p in self.row()['problems']))

    def test_setup_failure_does_not_require_an_unreachable_up_or_verifier(self):
        self.catalog_plan()
        records = self.sealed(outcome='setup_failed')
        summary = json.loads((records.out / 'summary.json').read_text())
        (records.out / 'grade-up.json').unlink()
        summary['evidence_sha256'].pop('grade-up.json')
        records.edit_summary(**summary)
        setup = json.loads((records.out / 'grade-setup.json').read_text())
        records.replace_record('grade-setup.json', dict(setup, exit_code=1))
        for attempt in [2, 3]:
            name = f'grade-setup-attempt-{attempt}'
            records.retry(name, dict(setup, exit_code=1, receipt=name,
                                   started_unix_ns=setup['started_unix_ns'] + (attempt - 1) * setup['elapsed_ns']))
        grade = json.loads((records.out / 'grade.json').read_text())
        grade.update(failed_step='setup', failed_receipt='grade-setup-attempt-3', seconds={'setup': 6.0})
        records.replace_record('grade.json', grade, 'grade')
        self.assertEqual(self.row()['status'], 'invalid')  # infrastructure still stays invalid
        self.assertEqual(self.row()['startup_seconds'], {'setup': 6.0, 'up': None})
        self.assertFalse(any(' missing' in p for p in self.row()['problems']))

    def test_one_malformed_summary_does_not_abort_the_other_assigned_rows(self):
        first = self.sealed()
        (first.out / 'summary.json').write_text('null')
        self.review(self.sealed(1))
        report = self.report()
        self.assertEqual([row['status'] for row in report['rows']], ['invalid', 'passed'])
        self.assertFalse(report['publishable'])
        (first.out / 'summary.json').write_text(json.dumps({'evidence_sha256': ['bad']}))
        self.review(first)
        self.assertEqual([row['status'] for row in self.report()['rows']], ['invalid', 'passed'])

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
