#!/usr/bin/env python3
"""Validate the harness itself before any agent trial. Never produces agent results.

  python3 bench/agentvalue/harness/validate.py --out bench/agentvalue/results/validation-<date>

1. Grader: for each task, the starting code fails, the reference passes, and a near-miss
   mutant of the reference fails the check it should.
2. Scripted reference agents in both arms: every task passes, both gates hold, nothing is left.
3. Planted faults in both arms: one agent leaves its services running, one runs its migration
   against the main checkout's database, one kills workers by name. Each gate must fire.
Exit status 0 only if every expectation holds.
"""
import argparse
import json
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent
sys.path.insert(0, str(HERE))
import grade
import report
import run as runner

MUTANTS = {
    # (description, files to change in the reference, check that must fail)
    'invoice-notes': ('PATCH no longer invalidates the cache', lambda tree: _edit(tree / 'app.py', '            cache.forget_invoice(invoice_id)\n', ''),
                      'cache_after_patch'),
    'pay-receipts': ('worker not idempotent (no unique index, no ON CONFLICT)', lambda tree: (
        (tree / 'migrations/20261118_one_receipt_per_invoice.sql').unlink(),
        shutil.copy2(ROOT / 'fixture/ledger/jobs.py', tree / 'ledger/jobs.py')), 'duplicate_job_one_receipt'),
    'customer-table': ('seed.py not updated for the new schema', lambda tree: shutil.copy2(ROOT / 'fixture/seed.py', tree / 'seed.py'),
                       'fresh_ok'),
}


def _edit(path, old, new):
    text = path.read_text()
    assert old in text, f'mutant anchor missing in {path}'
    path.write_text(text.replace(old, new))


def grade_cell(task, mode, out):
    with tempfile.TemporaryDirectory(prefix='avv-', dir='/tmp') as tmp:
        tree = Path(tmp) / 'tree'
        shutil.copytree(ROOT / 'fixture', tree, ignore=shutil.ignore_patterns('__pycache__', '.venv'))
        if mode in ('reference', 'mutant'):
            shutil.copytree(ROOT / 'oracles' / task, tree, dirs_exist_ok=True)
        if mode == 'mutant':
            MUTANTS[task][1](tree)
        r = grade.grade(task, tree, out / f'grade-{task}-{mode}')
    return {'passed': r['passed'], 'fresh_ok': r['fresh_ok'], 'failed_checks': r['failed_checks']}


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--out', required=True)
    parser.add_argument('--skip-grader', action='store_true')
    args = parser.parse_args()
    out = Path(args.out)
    out.mkdir(parents=True, exist_ok=False)
    checks, failures = {}, []

    def expect(name, ok, detail):
        checks[name] = {'ok': bool(ok), 'detail': detail}
        if not ok:
            failures.append(name)
        print(('PASS ' if ok else 'FAIL ') + name, json.dumps(detail, default=str)[:400], flush=True)

    if not args.skip_grader:
        for task in runner.TASK:
            noop = grade_cell(task, 'noop', out)
            expect(f'grader/{task}/starting-code-fails', not noop['passed'] and runner.TASK[task]['no_op_failing_check'] in noop['failed_checks'], noop)
            ref = grade_cell(task, 'reference', out)
            expect(f'grader/{task}/reference-passes', ref['passed'], ref)
            mut = grade_cell(task, 'mutant', out)
            target = MUTANTS[task][2]
            fired = (not mut['fresh_ok']) if target == 'fresh_ok' else target in mut['failed_checks']
            expect(f'grader/{task}/mutant-fails ({MUTANTS[task][0]})', not mut['passed'] and fired, mut)

    plan_path = out / 'plan.json'
    subprocess.run([sys.executable, str(HERE / 'run.py'), 'plan', '--name', out.name, '--kind', 'harness_selftest', '--pairs', '1',
                    '--seed', '0', '--agent-seconds', '600', '--out', str(plan_path)], check=True, capture_output=True)
    plan = json.loads(plan_path.read_text())
    reference = lambda task: ['python3', str(HERE / 'scripted_agent.py'), 'reference']
    planted_modes = {'invoice-notes': 'leave-running', 'pay-receipts': 'contaminate', 'customer-table': 'pkill'}
    planted = lambda task: ['python3', str(HERE / 'scripted_agent.py'), planted_modes[task]]
    for arm in ('baseline', 'stack'):
        rec = runner.run_arm(plan, 0, arm, out / f'{arm}-reference', agent_override=reference)
        fake = {'pairs': [{'index': 0, 'order': [arm]}], 'config': plan['config'], 'name': 'v', 'kind': 'harness_selftest', 'host': {}}
        rows = report.agent_rows(fake, {(0, arm): rec})
        expect(f'{arm}/reference/run-valid', rec['valid'], rec['invalid_reasons'])
        expect(f'{arm}/reference/all-gated-success', all(r.get('gated_success') for r in rows),
               [(r['task'], r.get('passed'), r.get('teardown_ok'), r.get('isolation_ok')) for r in rows])
        expect(f'{arm}/reference/used-own-db', all(r.get('used_own_db') for r in rows), [r.get('used_own_db') for r in rows])
        expect(f'{arm}/reference/no-contamination', not report.contamination(rec), report.contamination(rec))
        disp = rec.get('disposal') or {}
        expect(f'{arm}/reference/disposal-clean', not disp.get('processes_left') and not disp.get('ports_still_listening')
               and not disp.get('worktree_dirs_left'), {k: disp.get(k) for k in ['processes_left', 'ports_still_listening', 'worktree_dirs_left']})

        rec = runner.run_arm(plan, 0, arm, out / f'{arm}-planted', agent_override=planted)
        rows = {r['task']: r for r in report.agent_rows(fake, {(0, arm): rec})}
        events = report.contamination(rec)
        expect(f'{arm}/planted/run-valid', rec['valid'], rec['invalid_reasons'])
        left = rec['agents']['invoice-notes'].get('left_running') or []
        kinds = {p['command'].split('/')[-1].split()[0] for p in left}
        expect(f'{arm}/planted/leftover-detected (incl. postgres and redis)', not rows['invoice-notes']['teardown_ok']
               and any('postgres' in p['command'] for p in left) and any('redis' in p['command'] for p in left), sorted(kinds))
        expect(f'{arm}/planted/contamination-attributed', any(e['kind'] == 'migration_in_main' and 'pay-receipts' in e['culprits'] for e in events)
               and not rows['pay-receipts']['isolation_ok'], events)
        expect(f'{arm}/planted/collateral-kill-detected', any(e['kind'] == 'main_process_killed' and 'customer-table' in e['culprits'] for e in events),
               [e for e in events if e['kind'] == 'main_process_killed'])
        expect(f'{arm}/planted/nothing-survives', not rec.get('survivors_after_run'), rec.get('survivors_after_run'))
    summary = {'ok': not failures, 'failures': failures, 'checks': checks, 'stack_binary_sha256': plan['config']['stack_binary_sha256'],
               'source_rev': plan['config']['source_rev'], 'fingerprints': plan['fingerprints']}
    (out / 'validation.json').write_text(json.dumps(summary, indent=2))
    print(json.dumps({'ok': summary['ok'], 'failures': failures}))
    sys.exit(0 if summary['ok'] else 1)


if __name__ == '__main__':
    main()
