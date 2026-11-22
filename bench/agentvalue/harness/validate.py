#!/usr/bin/env python3
"""Validate the harness itself before any agent trial. Never produces agent results.

  python3 bench/agentvalue/harness/validate.py --out bench/agentvalue/results/validation-<rev>

1. Grader: for each of the five tasks, the starting code fails (its named check among the failures),
   the reference passes both grading runs, and every near-miss mutant fails the check it targets.
2. Planted runs with scripted agents, in both arms (PROTOCOL.md, Version 2, "Validation before any
   trial", plus the version 1 planted cases):
     clean                       five reference agents, no fault: every gate holds, nothing left
     agent-kill-recovers         agent-kill on a stalled victim, a recovery agent that finishes;
                                 in another worktree, a process the arm does not manage is left
     agent-kill-recovery-leaves  agent-kill, the recovery agent leaves the services running; in two
                                 other worktrees one agent kills the other's worker by name (event (g))
     service-kill-restarts       service-kill on a victim that starts its services again; in another
                                 worktree a command is left running under the arm's exec at teardown
     service-kill-no-reaction    service-kill on a victim that does not react
     version1-planted            services left running, a migration against the main checkout, a kill
                                 of the main checkout's worker
   and once, in the Stack arm with the version 1 build (dc4b980) as the control, the exec case.
Exit status 0 only if every expectation holds. `--only` and `--arms` are for development; a binding
validation runs everything.
"""
import argparse
import contextlib
import hashlib
import json
import os
from pathlib import Path
import re
import shlex
import shutil
import signal
import subprocess
import sys
import tempfile
import threading
import time
from concurrent.futures import ThreadPoolExecutor

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent
sys.path.insert(0, str(HERE))
import grade
import report
import run as runner
import observe  # after run: observe puts bench/ on the path, which has a run.py of its own

def _cust_no_unique(tree):
    _edit(tree / 'migrations/20261118_customers_table.sql', 'name text NOT NULL UNIQUE', 'name text NOT NULL')
    _edit(tree / 'ledger/db.py', """    return conn.execute('INSERT INTO customers (name) VALUES (%s) ON CONFLICT (name) DO UPDATE SET name = EXCLUDED.name RETURNING id',
                        (name,)).fetchone()[0]""", """    row = conn.execute('SELECT id FROM customers WHERE name = %s', (name,)).fetchone()
    return row[0] if row else conn.execute('INSERT INTO customers (name) VALUES (%s) RETURNING id', (name,)).fetchone()[0]""")


def _pay_in_request(tree):
    # The reviewer's counterexample: the receipt is written by the HTTP request, the worker does nothing.
    _edit(tree / 'app.py', "            jobs.enqueue('issue_receipt', invoice_id=invoice_id)\n",
          "            jobs.enqueue('issue_receipt', invoice_id=invoice_id)\n            jobs.issue_receipt({'invoice_id': invoice_id})\n")
    _edit(tree / 'ledger/jobs.py', "HANDLERS = {'issue_receipt': issue_receipt}", "HANDLERS = {'issue_receipt': lambda job: None}")


def _notes_bypass_cache(tree):
    _edit(tree / 'app.py', """            if hit := cache.get_invoice(invoice_id):
                return self.reply(hit)
""", '')
    _edit(tree / 'app.py', '            cache.put_invoice(invoice.to_json())\n', '')


def _notes_read_db_write_cache(tree):
    # Round-2 review counterexample: always read PostgreSQL, still write the result to Redis.
    _edit(tree / 'app.py', """            if hit := cache.get_invoice(invoice_id):
                return self.reply(hit)
""", '')


def _rerun_changes_data(tree):
    # Round-2 review counterexample: a rerun with nothing pending changes an existing invoice.
    text = (tree / 'migrate.py').read_text()
    text = text.replace("    applied = {row[0]", "    ran = False\n    applied = {row[0]")
    text = text.replace("        print(f'applied {path.stem}')", "        print(f'applied {path.stem}')\n        ran = True")
    text = text.replace("print('migrations up to date')", "    if not ran:\n        conn.execute('UPDATE invoices SET cents = cents + 1 WHERE id = 41')\nprint('migrations up to date')")
    (tree / 'migrate.py').write_text(text)


def _cust_partial_unique(tree):
    # Round-2 review counterexample: a unique index that leaves one name out.
    _cust_no_unique(tree)
    with open(tree / 'migrations/20261118_customers_table.sql', 'a') as f:
        f.write("CREATE UNIQUE INDEX customers_name_partial ON customers (name) WHERE name <> 'Umbrella';\n")


LIST_MIGRATION = 'migrations/20261119_invoices_status_id_index.sql'
VOID_MIGRATION = 'migrations/20261119_allow_void_status.sql'


def _list_full_page_next(tree):
    # The classic off-by-one: "a full page means there is more".
    _edit(tree / 'app.py', '        more = len(rows) > limit\n', '        more = len(page) == limit\n')


def _list_customer_ignored(tree):
    _edit(tree / 'app.py', 'status=status, customer=customer, after=after', 'status=status, customer=None, after=after')


def _list_wrong_index(tree):
    (tree / LIST_MIGRATION).write_text('CREATE INDEX IF NOT EXISTS invoices_customer ON invoices (customer);\n')


def _list_partial_index(tree):
    (tree / LIST_MIGRATION).write_text("CREATE INDEX IF NOT EXISTS invoices_status_id ON invoices (status, id) WHERE status = 'open';\n")


def _list_status_only_index(tree):
    (tree / LIST_MIGRATION).write_text('CREATE INDEX IF NOT EXISTS invoices_status ON invoices (status);\n')


def _void_allows_refunded(tree):
    (tree / VOID_MIGRATION).write_text("ALTER TABLE invoices DROP CONSTRAINT IF EXISTS invoices_status_check;\n"
                                       "ALTER TABLE invoices ADD CONSTRAINT invoices_status_check CHECK (status IN ('open', 'paid', 'void', 'refunded'));\n")


def _list_status_customer_id_index(tree):
    (tree / LIST_MIGRATION).write_text('CREATE INDEX IF NOT EXISTS invoices_status_customer_id ON invoices (status, customer, id);\n')


def _void_allows_archived(tree):
    (tree / VOID_MIGRATION).write_text("ALTER TABLE invoices DROP CONSTRAINT IF EXISTS invoices_status_check;\n"
                                       "ALTER TABLE invoices ADD CONSTRAINT invoices_status_check CHECK (status IN ('open', 'paid', 'void', 'archived'));\n")


def _void_disjunction(tree):
    # The reviewer's round 3 counterexample: the allowlist OR any long string.
    (tree / VOID_MIGRATION).write_text("ALTER TABLE invoices DROP CONSTRAINT IF EXISTS invoices_status_check;\n"
                                       "ALTER TABLE invoices ADD CONSTRAINT invoices_status_check "
                                       "CHECK (status IN ('open', 'paid', 'void') OR length(status) > 10);\n")


def _void_redundant_nonempty(tree):
    # A correct alternative: the exact allowlist plus a redundant `status <> ''` constraint.
    with open(tree / VOID_MIGRATION, 'a') as f:
        f.write("ALTER TABLE invoices ADD CONSTRAINT invoices_status_nonempty CHECK (status <> '');\n")


def _void_never_caches_void(tree):
    # A correct alternative: a void invoice is never put in the cache (reads of it go to PostgreSQL).
    _edit(tree / 'app.py', """            cache.put_invoice(invoice.to_json())
            return self.reply(invoice.to_json())""", """            if invoice.status != 'void':
                cache.put_invoice(invoice.to_json())
            return self.reply(invoice.to_json())""")


def _list_limit_unchecked(tree):
    _edit(tree / 'app.py', '        elif WHOLE.match(limit) and 1 <= int(limit) <= 100:\n', '        elif WHOLE.match(limit):\n')


def _list_descending(tree):
    _edit(tree / 'ledger/db.py', "'ORDER BY id LIMIT %(limit)s'", "'ORDER BY id DESC LIMIT %(limit)s'")


VOID_FORGET = "            cache.forget_invoice(invoice_id)\n            return self.reply(invoice.to_json())\n        if self.path == '/invoices':"


def _void_no_invalidate(tree):
    _edit(tree / 'app.py', VOID_FORGET, "            return self.reply(invoice.to_json())\n        if self.path == '/invoices':")


def _void_constraint_dropped(tree):
    (tree / VOID_MIGRATION).write_text('ALTER TABLE invoices DROP CONSTRAINT IF EXISTS invoices_status_check;\n')


def _void_patch_allowed(tree):
    _edit(tree / 'app.py', """                if invoice.status == 'void':
                    return self.reply({'error': 'a void invoice cannot be changed'}, 409)
""", '')


def _void_paid_changed(tree):
    _edit(tree / 'ledger/db.py', "WHERE id = %s AND status = 'open' RETURNING", "WHERE id = %s AND status <> 'void' RETURNING")


def _void_twice_ok(tree):
    _edit(tree / 'ledger/db.py', "WHERE id = %s AND status = 'open' RETURNING", "WHERE id = %s AND status IN ('open', 'void') RETURNING")


def _void_bypass_cache(tree):
    _edit(tree / 'app.py', """            if hit := cache.get_invoice(invoice_id):
                return self.reply(hit)
""", '')


# (description, change to the reference, check that must fail; 'fresh_ok' means the fresh-database run)
MUTANTS = {
    'invoice-notes': [('PATCH no longer invalidates the cache', lambda t: _edit(t / 'app.py', '            cache.forget_invoice(invoice_id)\n', ''), 'cache_after_patch'),
                      ('reads bypass the cache entirely', _notes_bypass_cache, 'cache_after_patch'),
                      ('reads skip the cache but still write it', _notes_read_db_write_cache, 'cache_after_patch'),
                      ('a migration rerun changes existing data', _rerun_changes_data, 'migration_rerun_safe')],
    'pay-receipts': [('worker not idempotent (no unique index, no ON CONFLICT)', lambda t: (
                          (t / 'migrations/20261118_one_receipt_per_invoice.sql').unlink(),
                          shutil.copy2(ROOT / 'fixture/ledger/jobs.py', t / 'ledger/jobs.py')), 'duplicate_job_one_receipt'),
                     ('receipt written by the request, worker a no-op', _pay_in_request, 'receipt_recorded')],
    'customer-table': [('seed.py not updated for the new schema', lambda t: shutil.copy2(ROOT / 'fixture/seed.py', t / 'seed.py'), 'fresh_ok'),
                       ('migration deletes existing receipts', lambda t: open(t / 'migrations/20261118_customers_table.sql', 'a').write('DELETE FROM receipts;\n'),
                        'existing_preserved'),
                       ('no unique constraint on customer name', _cust_no_unique, 'customers_deduplicated'),
                       ('a partial unique index on customer name', _cust_partial_unique, 'customers_deduplicated')],
    'invoice-list': [('next is not null when the last page is exactly full', _list_full_page_next, 'next_null_on_full_last_page'),
                     ('customer filter ignored', _list_customer_ignored, 'filter_customer'),
                     ('no index covering status (an index on customer instead)', _list_wrong_index, 'status_index'),
                     ('a partial index on status', _list_partial_index, 'status_index'),
                     ('an index on status alone', _list_status_only_index, 'status_index'),
                     ('an index on (status, customer, id)', _list_status_customer_id_index, 'status_index'),
                     ('limit not range-checked', _list_limit_unchecked, 'bad_params_400'),
                     ('items in descending id order', _list_descending, 'list_all_ascending')],
    'void-invoices': [('void does not invalidate the cache', _void_no_invalidate, 'cache_after_void'),
                      ('reads bypass the cache entirely', _void_bypass_cache, 'cache_after_void'),
                      ('constraint dropped and not re-added', _void_constraint_dropped, 'constraint_allows_void_only'),
                      ('constraint also allows another status (refunded)', _void_allows_refunded, 'constraint_allows_void_only'),
                      ('constraint also allows another status (archived)', _void_allows_archived, 'constraint_allows_void_only'),
                      ('constraint allowlist OR length(status) > 10', _void_disjunction, 'constraint_allows_void_only'),
                      ('PATCH on a void invoice allowed', _void_patch_allowed, 'patch_void_409'),
                      ('a paid invoice is voided', _void_paid_changed, 'void_paid_409'),
                      ('a void invoice can be voided again', _void_twice_ok, 'void_twice_409')],
}

# Correct alternatives to the reference: each must pass.
def _void_equality_disjunction(tree):
    (tree / VOID_MIGRATION).write_text("ALTER TABLE invoices DROP CONSTRAINT IF EXISTS invoices_status_check;\n"
                                       "ALTER TABLE invoices ADD CONSTRAINT invoices_status_check "
                                       "CHECK (status = 'open' OR status = 'paid' OR status = 'void');\n")


def _void_mixed_disjunction(tree):
    (tree / VOID_MIGRATION).write_text("ALTER TABLE invoices DROP CONSTRAINT IF EXISTS invoices_status_check;\n"
                                       "ALTER TABLE invoices ADD CONSTRAINT invoices_status_check "
                                       "CHECK (status = 'open' OR status IN ('paid', 'void'));\n")


ALTERNATIVES = {'void-invoices': [('a void invoice is never cached', _void_never_caches_void),
                                  ("CHECK (status = 'open' OR status = 'paid' OR status = 'void')", _void_equality_disjunction),
                                  ("CHECK (status = 'open' OR status IN ('paid', 'void'))", _void_mixed_disjunction),
                                   ("the allowlist plus a redundant status <> '' constraint", _void_redundant_nonempty)]}


def _edit(path, old, new):
    text = path.read_text()
    assert old in text, f'mutant anchor missing in {path}'
    path.write_text(text.replace(old, new))


def grade_cell(task, mode, out, mutant=None, label='0'):
    with tempfile.TemporaryDirectory(prefix='avv-', dir='/tmp') as tmp:
        tree = Path(tmp) / 'tree'
        shutil.copytree(ROOT / 'fixture', tree, ignore=shutil.ignore_patterns('__pycache__', '.venv'))
        if mode in ('reference', 'mutant'):
            shutil.copytree(ROOT / 'oracles' / task, tree, dirs_exist_ok=True)
        if mode == 'mutant':
            mutant(tree)
        r = grade.grade(task, tree, out / f'grade-{task}-{mode}-{label}')
    return {'passed': r['passed'], 'fresh_ok': r['fresh_ok'], 'failed_checks': r['failed_checks']}


OLD_STACK = runner.REPO_ROOT / 'target/release/stack'          # dc4b980, the version 1 build (control only)
OLD_STACK_SHA = '4c7a836d408af2e1b9a0201a0ad1ff90544415bcd0c524495f160aceab8c4ea3'
NEW_STACK = Path.home() / '.cache/stack-agentvalue/stack-a5d4785/src/target/release/stack'
NEW_STACK_SHA = 'b0931a8be40bc65855a1139d10a8f7f66c136e5a564b58cf9994a58f511fdd17'

# The planted runs, each in both arms. Independent planted cases share a run only when they sit in
# different worktrees and cannot hide each other; every expectation is asserted per arm.
CASES = {
    'clean': ({}, None),
    'agent-kill-recovers': ({'invoice-list': 'slow-stall', 'invoice-list.recovery': 'reference-norole', 'void-invoices': 'stray'},
                            ('agent-kill', 'invoice-list')),
    'agent-kill-recovery-leaves': ({'pay-receipts': 'stall', 'pay-receipts.recovery': 'leave-running', 'customer-table': 'target',
                                    'invoice-notes': 'killer:wt-cust', 'invoice-list': 'sentinel'}, ('agent-kill', 'pay-receipts')),
    'service-kill-restarts': ({'void-invoices': 'sk-restart-scratch', 'invoice-list': 'exec-left'}, ('service-kill', 'void-invoices')),
    'service-kill-no-reaction': ({'customer-table': 'sk-noreact'}, ('service-kill', 'customer-table')),
    'service-kill-never-restores': ({'invoice-notes': 'sk-finish'}, ('service-kill', 'invoice-notes')),
    'event-g-identification': ({'invoice-notes': 'wrong-pid-killer:wt-pay', 'pay-receipts': 'self-kill',
                                'customer-table': 'pid-killer:wt-list', 'invoice-list': 'up-target'}, None),
    'version1-planted-and-collateral': ({'invoice-notes': 'leave-running', 'pay-receipts': 'contaminate', 'customer-table': 'pkill',
                                         'void-invoices': 'leave-running-idle'}, None),
    'placement-and-attribution': ({'void-invoices': 'orphan', 'pay-receipts': 'marked-reader:wt-notes'}, None),
}
INTERRUPTS = ('before-the-kill', 'during-the-delay', 'during-the-launch')
SERVICES = ('postgres', 'redis', 'api', 'worker')
TOOL_TAG = 'avr-stall-tool'


def make_plan(out, name, binary, rev):
    path = out / f'{name}.json'
    subprocess.run([sys.executable, str(HERE / 'run.py'), 'plan', '--name', name, '--kind', 'harness_selftest', '--pairs', '1',
                    '--seed', '0', '--agent-seconds', '600', '--stack', str(binary), '--stack-rev', rev, '--out', str(path)],
                   check=True, capture_output=True)
    return runner.load_plan(path), path


def gates(plan, arm, rec, out_dir):
    """Version 2 slot rows and the run row, computed by report.py exactly as for a trial."""
    fake = {'name': 'v', 'kind': 'harness_selftest', 'protocol_version': 2, 'host': {}, 'config': plan['config'],
            'pairs': [{'index': 0, 'order': [arm], 'scenario': rec.get('scenario'), 'victim': rec.get('victim')}]}
    rows, run_rows, _ = report.rows_v2(fake, {(0, arm): rec}, str(out_dir.parent))
    return {r['task']: r for r in rows}, run_rows[0]


def sentinel_for(rec):
    return hashlib.sha256((Path(rec['root']).name + ':sentinel').encode()).hexdigest()[:24]


def files_containing(directory, value):
    """Retained files whose content or path name carries the value."""
    needle = value.encode()
    return [str(p.relative_to(directory)) for p in Path(directory).rglob('*')
            if value in str(p.relative_to(directory)) or (p.is_file() and not p.is_symlink() and needle in p.read_bytes())]


def four_services(rec):
    """The trigger saw all four services of the victim's worktree listening, and the listing taken
    before the kill shows each of them."""
    f = rec.get('fault') or {}
    trig = (f.get('trigger') or {}).get('services') or {}
    listing = (f.get('running_at_kill') or {}).get('services') or {}
    ok = set(trig) == set(SERVICES) and all(listing.get(k) for k in SERVICES) and all(trig[k]['pid'] in listing.get(k, []) for k in trig)
    return ok, {'trigger': {k: (v.get('pid'), v.get('port')) for k, v in trig.items()}, 'listing': listing}


def forward_and_wait(proc, grace=900):
    """Pass the signal that stopped this validation on to a nested process and wait for it to finish
    its own cleanup; a further signal meanwhile is passed on again, never left behind."""
    deadline = time.monotonic() + grace
    while proc.poll() is None and time.monotonic() < deadline:
        try:
            os.kill(proc.pid, runner.INTERRUPTED.get('signal') or signal.SIGTERM)
        except ProcessLookupError:
            break
        try:
            proc.wait(max(1, deadline - time.monotonic()))
        except subprocess.TimeoutExpired:
            break
        except (KeyboardInterrupt, ValidationInterrupted):
            continue
    if proc.poll() is None:
        proc.kill()
        proc.wait()


@contextlib.contextmanager
def forwarding(proc):
    """While a nested process runs: if this validation is interrupted, forward and wait, then stop."""
    try:
        yield
    except BaseException:
        forward_and_wait(proc)
        raise


class ValidationInterrupted(BaseException):
    """An outside SIGINT or SIGTERM: the whole validation stops after the current run's cleanup."""


def stop_on_signal(signum, frame):
    runner.INTERRUPTED['signal'] = signum
    raise KeyboardInterrupt(f'signal {signum}')


def main():
    # A SIGINT or SIGTERM from outside ends the whole validation (after the current run's own
    # cleanup), never only the case in progress. The validation sends signals only to its children.
    signal.signal(signal.SIGINT, stop_on_signal)
    signal.signal(signal.SIGTERM, stop_on_signal)
    state = {}
    try:
        _main(state)
    except (KeyboardInterrupt, ValidationInterrupted):
        signum = runner.INTERRUPTED.get('signal') or signal.SIGINT
        print(f'INTERRUPTED: the validation was stopped by signal {signum} after the current run\'s cleanup; '
              'the remaining cases were not run', flush=True)
        if state.get('out'):
            (state['out'] / 'validation.json').write_text(json.dumps({'ok': False, 'interrupted_by_signal': signum,
                                                                      'failures': state.get('failures'), 'checks': state.get('checks')},
                                                                     indent=2, default=str))
        if state.get('lock'):
            state['lock'][0].close()
        sys.exit(128 + int(signum))


def _main(state):
    parser = argparse.ArgumentParser()
    parser.add_argument('--out', required=True)
    parser.add_argument('--skip-grader', action='store_true')
    parser.add_argument('--only', help='comma-separated case names (development only; a binding validation runs all)')
    parser.add_argument('--arms', default='baseline,stack')
    args = parser.parse_args()
    # Normal dispositions even when started from a background shell (which ignores SIGINT): nothing
    # this process starts may inherit an ignored signal.
    out = Path(args.out)
    out.mkdir(parents=True, exist_ok=False)
    checks, failures = {}, []
    state.update(out=out, checks=checks, failures=failures)
    outsiders = []   # processes the validation itself starts outside any run (operator stand-ins)
    started = time.monotonic()

    def expect(name, ok, detail):
        checks[name] = {'ok': bool(ok), 'detail': detail}
        if not ok:
            failures.append(name)
        print(('PASS ' if ok else 'FAIL ') + name, json.dumps(detail, default=str)[:400], flush=True)

    for binary, digest, rev in ((NEW_STACK, NEW_STACK_SHA, 'a5d4785'), (OLD_STACK, OLD_STACK_SHA, 'dc4b980')):
        expect(f'setup/stack-binary {rev}', binary.is_file() and runner.sha256(binary) == digest, {'path': str(binary), 'expected_sha256': digest})
    lock = [runner.host_lock()]
    state['lock'] = lock
    only = set(args.only.split(',')) if args.only else set(CASES) | {'exec-control-dc4b980', 'commit-scan', 'interrupts', 'unit', 'publication-failure', 'validation-interrupt'}

    if not args.skip_grader:
        cells = []
        for task in runner.TASK:
            cells += [(task, 'noop', None, None, '0'), (task, 'reference', None, None, '0')]
            cells += [(task, 'mutant', change, (description, target), str(i)) for i, (description, change, target) in enumerate(MUTANTS[task])]
            cells += [(task, 'alternative', change, (description, None), f'alt{i}') for i, (description, change) in enumerate(ALTERNATIVES.get(task, []))]
        with ThreadPoolExecutor(4) as pool:
            results = list(pool.map(lambda c: grade_cell(c[0], 'mutant' if c[1] == 'alternative' else c[1], out, c[2], c[4]), cells))
        for (task, mode, change, info, label), r in zip(cells, results):
            if mode == 'noop':
                expect(f'grader/{task}/starting-code-fails', not r['passed'] and runner.TASK[task]['no_op_failing_check'] in r['failed_checks'], r)
            elif mode == 'reference':
                expect(f'grader/{task}/reference-passes', r['passed'], r)
            elif mode == 'alternative':
                expect(f'grader/{task}/alternative-passes ({info[0]})', r['passed'], r)
            else:
                description, target = info
                fired = (not r['fresh_ok']) if target == 'fresh_ok' else target in r['failed_checks']
                expect(f'grader/{task}/mutant-fails ({description})', not r['passed'] and fired, r)
        for task in runner.TASK:
            expect(f'grader/{task}/at-least-three-mutants', len(MUTANTS[task]) >= (3 if task in ('invoice-list', 'void-invoices') else 1),
                   len(MUTANTS[task]))

    if 'validation-interrupt' in only:
        validation_interrupt_case(out, lock, expect)
        validation_interrupt_case(out, lock, expect, during='cleanup')

    if 'commit-scan' in only:
        refused, committed, detail = commit_scan_case()
        expect('commit-scan/quoted-path-binary-rename-symlink-message-credential-all-reported', refused, detail)
        expect('commit-scan/clean-commit-goes-through', committed, detail)

    plan, plan_path = make_plan(out, 'plan', NEW_STACK, 'a5d4785')
    if 'unit' in only:
        unit_cases(expect, plan, plan_path)
    control_plan, _ = make_plan(out, 'plan-control-dc4b980', OLD_STACK, 'dc4b980')
    expect('setup/plan-records-binary', plan['config']['stack_binary_sha256'] == NEW_STACK_SHA and plan['config']['stack_rev'] == 'a5d4785'
           and plan['config']['stack_version'] and plan['protocol_version'] == 2,
           {k: plan['config'][k] for k in ('stack_rev', 'stack_binary_sha256', 'stack_version', 'fault')})
    trial = dict(plan, kind='agent_experiment')
    expect('setup/trial-plan-without-faults-refused', bool(runner.protocol_problems(trial)), runner.protocol_problems(trial))
    old_trial = dict(control_plan, kind='agent_experiment', pairs=[{'index': 0, 'scenario': 'agent-kill', 'victim': 'invoice-list',
                                                                    'order': ['stack', 'baseline']}])
    old_trial['config'] = dict(old_trial['config'], stack_rev='a5d4785')
    expect('setup/trial-plan-with-the-old-binary-refused', any('Stack binary' in p for p in runner.protocol_problems(old_trial)),
           runner.protocol_problems(old_trial))
    timings = {}

    def run_case(arm, case, the_plan=plan, down_override=None, outsider=False, env=None, expect_valid=True):
        modes, fault = CASES.get(case, ({'invoice-list': 'exec-left'}, None))
        t = time.monotonic()
        roots, watcher = [], None
        original_mkdtemp = runner.tempfile.mkdtemp
        def mkdtemp(*a, **k):
            path = original_mkdtemp(*a, **k)
            if Path(path).name.startswith('avr') and not roots:
                roots.append(path)
            return path
        runner.tempfile.mkdtemp = mkdtemp
        if outsider:   # operators' `tail -f`: on a staged trace and on a worktree file; outside the run, no marker
            def start_tail():
                targets = ['stage/invoice-notes/trace.jsonl', 'wt-notes/README.md']
                deadline = time.monotonic() + 120
                while targets and time.monotonic() < deadline:
                    for rel in list(targets):
                        target = Path(roots[0]) / rel if roots else None
                        if target and target.exists():
                            outsiders.append(subprocess.Popen(['/usr/bin/tail', '-f', str(target)], cwd='/', env={'PATH': '/usr/bin:/bin'},
                                                              stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, start_new_session=True))
                            targets.remove(rel)
                    time.sleep(0.1)
            watcher = threading.Thread(target=start_tail, daemon=True)
            watcher.start()
        saved_env = {k: os.environ.get(k) for k in (env or {})}
        os.environ.update(env or {})
        try:
            rec = runner.run_arm(the_plan, 0, arm, out / f'{arm}-{case}', agent_override=runner.scripted_override(
                ','.join(f'{k}={v}' for k, v in modes.items())), fault={'scenario': fault[0], 'victim': fault[1]} if fault else {},
                down_override=down_override)
        finally:
            runner.tempfile.mkdtemp = original_mkdtemp
            if runner.INTERRUPTED.get('signal'):
                for proc in outsiders:          # the validation's own outsiders, never left behind
                    proc.kill()
            for k, v in saved_env.items():
                if v is None:
                    os.environ.pop(k, None)
                else:
                    os.environ[k] = v
            if watcher:
                watcher.join(5)
        if runner.INTERRUPTED.get('signal'):   # the run ended its own cleanup; now the validation stops
            raise ValidationInterrupted()
        timings[f'{arm}-{case}'] = round(time.monotonic() - t, 1)
        rows, run_row = gates(the_plan, arm, rec, out / f'{arm}-{case}')
        if expect_valid:
            expect(f'{arm}/{case}/run-valid', rec['valid'], rec['invalid_reasons'])
        expect(f'{arm}/{case}/nothing-of-the-run-survives', not rec.get('survivors_after_run') and not rec.get('final_scan'),
               {'survivors': rec.get('survivors_after_run'), 'final_scan': rec.get('final_scan')})
        return rec, rows, run_row

    def gated(rows, tasks):
        return all(rows[t].get('gated_success') for t in tasks), {t: {k: rows[t].get(k) for k in ('passed', 'teardown_ok', 'isolation_ok', 'cli_ok', 'residue_ok')}
                                                               for t in tasks}

    def fault_at_plan(rec, scenario, victim):
        f = rec.get('fault') or {}
        k, trig = f.get('kill') or {}, f.get('trigger') or {}
        victim_session = rec['agents'].get(victim) or {}
        ok = f.get('injected') and not f.get('missed') and k.get('hit_confirmed') and 2.9 <= (k.get('seconds_after_trigger') or 0) <= 4.0 \
            and not k.get('late_flag') and (k.get('seconds_after_launch') or 1e9) < 600
        if scenario == 'agent-kill':
            ok = ok and k.get('pid') == victim_session.get('pid') and k.get('returncode') == -9 and victim_session.get('killed_by_harness')
        else:
            pg = (trig.get('services') or {}).get('postgres') or {}
            ok = ok and k.get('pid') == pg.get('pid') and k.get('what') == 'postmaster' and all((k.get('checks') or {}).values())
        return bool(ok), {'injected': f.get('injected'), 'missed': f.get('missed'), 'trigger_s': trig.get('seconds_after_launch'),
                          'kill': {x: k.get(x) for x in ('pid', 'what', 'seconds_after_trigger', 'late_s', 'checks', 'returncode', 'hit_confirmed')}}

    for arm in args.arms.split(','):
        tasks = plan['config']['tasks']
        try:
            if 'clean' in only:
                outsiders.clear()
                rec, rows, rr = run_case(arm, 'clean', outsider=True)
                tail_alive = bool(outsiders) and all(proc.poll() is None for proc in outsiders)
                listed = [p for p in (rec.get('seen_not_signalled') or []) + (rec.get('final_scan_seen_not_signalled') or [])
                          if 'tail' in p['command']]
                for proc in outsiders:
                    proc.kill()
                    proc.wait()
                expect(f'{arm}/clean/outsider-with-a-run-file-open-survives-and-is-listed', tail_alive and listed and rec['valid'],
                       {'tail_started': bool(outsiders), 'tail_alive_after_run': tail_alive, 'listed': listed})
                notes = rec['agents'].get('invoice-notes') or {}
                td = (rec.get('teardown') or {}).get('invoice-notes') or {}
                at_exit = [p for p in notes.get('seen_not_counted') or [] if 'tail' in p['command']]
                after_down = [p for p in (td.get('after_down') or {}).get('seen_not_counted') or [] if 'tail' in p['command']]
                expect(f'{arm}/clean/worktree-log-reader-listed-not-counted (exit gate, false confirmation, residue)',
                       bool(at_exit) and bool(after_down) and rows['invoice-notes'].get('teardown_ok') and not td.get('false_confirmation')
                       and td.get('residue_ok') and len(outsiders) == 2,
                       {'at_exit': at_exit, 'after_down': after_down, 'teardown_ok': rows['invoice-notes'].get('teardown_ok'),
                        'false_confirmation': td.get('false_confirmation'), 'residue_ok': td.get('residue_ok'), 'outsiders': len(outsiders)})
                ok, detail = gated(rows, tasks)
                expect(f'{arm}/clean/every-slot-gated-success', ok, detail)
                expect(f'{arm}/clean/used-own-db', all(rows[t].get('used_own_db') for t in tasks), [rows[t].get('used_own_db') for t in tasks])
                expect(f'{arm}/clean/others-untouched', rr['others_untouched'], {'events': rr['events'], 'review': rr['review']})
                expect(f'{arm}/clean/residue-holds-every-worktree', rr['residue_all'],
                       {t: rec['teardown'][t]['after_down'] for t in tasks if not rec['teardown'][t].get('residue_ok')})
                expect(f'{arm}/clean/clean-after-first-down-every-worktree', all(rows[t].get('clean_after_first_down') for t in tasks),
                       {t: rows[t].get('clean_after_first_down') for t in tasks})
                expect(f'{arm}/clean/no-false-confirmation', rr['false_confirmations'] == 0 and rr['downs_exit0'] == 5, rr['false_confirmations'])
                expect(f'{arm}/clean/episode-success', rr['episode_success'], rr['episode_success'])
                disp = rec.get('disposal') or {}
                expect(f'{arm}/clean/disposal-clean', not disp.get('processes_left') and not disp.get('ports_still_listening')
                       and not disp.get('worktree_dirs_left') and not disp.get('service_data_left'),
                       {k: disp.get(k) for k in ['processes_left', 'ports_still_listening', 'worktree_dirs_left', 'service_data_left']})
                expect(f'{arm}/clean/compliance (committed, ran tests, clean CLI exit)',
                       all(rows[t].get('committed') and rows[t].get('ran_tests') and rows[t].get('cli_ok') for t in tasks),
                       {t: (rows[t].get('committed'), rows[t].get('ran_tests'), rows[t].get('cli_ok')) for t in tasks})
                expect(f'{arm}/clean/no-fault-recorded', rec.get('fault') is None and rec.get('scenario') is None, rec.get('scenario'))
                expect(f'{arm}/clean/trace-arrival-times', all(trace_times_ok(out / f'{arm}-clean' / 'agents' / t) for t in tasks), tasks)
                expect(f'{arm}/clean/machine-budget-recorded', machine_ok(rec), rec.get('machine'))
                expect(f'{arm}/clean/sampler-ran-through-the-teardown', (rec['sampler'].get('last_m') or 0) >= (rec.get('teardown_windows') or [{}])[-1].get('end_m', 1e9),
                       {'last_sample_m': rec['sampler'].get('last_m'), 'windows': rec.get('teardown_windows')})
                expect(f'{arm}/clean/no-environment-in-records', no_environment(out / f'{arm}-clean'), 'scanned')
        except Exception as error:  # an invalid or interrupted run: its expectations fail, the validation goes on
            expect(f'{arm}/clean/expectations-evaluated', False, repr(error))

        try:
            if 'agent-kill-recovers' in only:
                case = 'agent-kill-recovers'
                rec, rows, rr = run_case(arm, case)
                ok, detail = fault_at_plan(rec, 'agent-kill', 'invoice-list')
                expect(f'{arm}/{case}/fault-injected-at-planned-point', ok, detail)
                ok, detail = four_services(rec)
                expect(f'{arm}/{case}/pre-kill-listing-has-all-four-services', ok, detail)
                up = next((c['start_m'] for c in rec['agents']['invoice-list'].get('service_commands') or [] if 'up' in c['kinds']), None)
                trig_m = (rec['fault'].get('trigger') or {}).get('m')
                expect(f'{arm}/{case}/slow-services-killed-only-after-all-four-listen', up is not None and trig_m is not None and trig_m >= up + 11.5,
                       {'up_issued_m': up, 'trigger_m': trig_m, 'worker_delay_s': 12})
                recovery = rec['fault'].get('recovery') or {}
                expect(f'{arm}/{case}/recovery-started-10s-after-kill', 10.0 <= (recovery.get('seconds_after_kill') or 0) <= 11.0
                       and not recovery.get('late_flag') and rec['slots'].get('invoice-list') == 'invoice-list-recovery', recovery)
                prompt_path = out / f'{arm}-{case}' / 'agents' / 'invoice-list-recovery' / 'prompt.txt'
                prompt = prompt_path.read_text() if prompt_path.exists() else ''
                expect(f'{arm}/{case}/recovery-prompt-verbatim', prompt == runner.RECOVERY_PROMPT.format(
                    branch=runner.TASK['invoice-list']['branch'], instruction=runner.TASK['invoice-list']['instruction']), prompt[:120])
                expect(f'{arm}/{case}/killed-session-not-scored', rows['invoice-list'].get('session') == 'invoice-list-recovery'
                       and 'grade' not in rec['agents']['invoice-list'], rows['invoice-list'].get('session'))
                expect(f'{arm}/{case}/outstanding-command-recorded', bool((rec['fault'].get('outstanding_at_kill') or {}).get('command')),
                       rec['fault'].get('outstanding_at_kill'))
                handoff = rec['fault'].get('handoff') or {}
                expect(f'{arm}/{case}/handoff-shows-killed-cli-tool-still-running', handoff.get('any_running')
                       and any(TOOL_TAG in p['command'] for p in handoff.get('killed_cli_processes_running') or []), handoff)
                conns = [e for e in rr['events'] if e['kind'] == 'cross_worktree_connection']
                by_killed = [e for e in conns if e.get('role') == 'agent']
                no_role = [e for e in conns if e.get('role') is None]
                expect(f'{arm}/{case}/killed-cli-connection-fails-run-not-charged-to-recovery',
                       bool(by_killed) and all(e['culprits'] == ['invoice-list'] for e in by_killed) and not rr['others_untouched']
                       and rows['invoice-list'].get('isolation_ok'), {'connections': by_killed})
                expect(f'{arm}/{case}/no-role-connection-during-recovery-fails-run-only', bool(no_role) and all(not e['culprits'] for e in no_role)
                       and rows['invoice-list'].get('isolation_ok') and not rr['others_untouched']
                       and all(rows[t].get('isolation_ok') for t in ('invoice-notes', 'pay-receipts', 'customer-table')), {'connections': no_role})
                expect(f'{arm}/{case}/recovery-gated-success', rows['invoice-list'].get('gated_success'),
                       {k: rows['invoice-list'].get(k) for k in ('passed', 'teardown_ok', 'isolation_ok', 'cli_ok')})
                expect(f'{arm}/{case}/residue-holds-victim-worktree', rr['residue_victim'] is True, rec['teardown']['invoice-list'].get('after_down'))
                stray = rec['teardown']['void-invoices']
                expect(f'{arm}/{case}/unmanaged-process-fails-residue', rows['void-invoices'].get('residue_ok') is False
                       and any('time.sleep(3600)' in p['command'] for p in stray['after_down']['processes'])
                       and stray.get('down_exit') == 0 and stray.get('false_confirmation') and stray.get('harness_reclaim'),
                       {'after_down': stray['after_down'], 'down_exit': stray.get('down_exit')})
                expect(f'{arm}/{case}/unmanaged-process-fails-teardown-at-exit', not rows['void-invoices'].get('teardown_ok'),
                       rec['agents']['void-invoices'].get('left_running'))
                ok, detail = gated(rows, ['invoice-notes', 'pay-receipts', 'customer-table'])
                expect(f'{arm}/{case}/survivors-gated-success', ok, detail)
        except Exception as error:  # an invalid or interrupted run: its expectations fail, the validation goes on
            expect(f'{arm}/agent-kill-recovers/expectations-evaluated', False, repr(error))

        try:
            if 'agent-kill-recovery-leaves' in only:
                case = 'agent-kill-recovery-leaves'
                rec, rows, rr = run_case(arm, case)
                ok, detail = fault_at_plan(rec, 'agent-kill', 'pay-receipts')
                expect(f'{arm}/{case}/fault-injected-at-planned-point', ok, detail)
                ok, detail = four_services(rec)
                expect(f'{arm}/{case}/pre-kill-listing-has-all-four-services', ok, detail)
                expect(f'{arm}/{case}/recovery-fails-teardown-at-exit', rows['pay-receipts'].get('session') == 'pay-receipts-recovery'
                       and not rows['pay-receipts'].get('teardown_ok') and rows['pay-receipts'].get('passed'),
                       {'left_running': len(rec['agents'].get('pay-receipts-recovery', {}).get('left_running') or [])})
                td = rec['teardown']['pay-receipts']
                expect(f'{arm}/{case}/arm-down-clears-victim-worktree', td.get('down_exit') == 0 and not td['after_down']['processes']
                       and rr['residue_victim'] is True, {'down_exit': td.get('down_exit'), 'after_down': td['after_down']})
                g = [e for e in rr['events'] if e['kind'] == 'sibling_service_lost']
                expect(f'{arm}/{case}/event-g-attributed-to-killer', bool(g) and all(e['culprits'] == ['invoice-notes'] for e in g)
                       and all(e['loss']['worktree'] == 'wt-cust' for e in g) and any('worker.py' in e['loss']['command'] for e in g)
                       and not rows['invoice-notes'].get('isolation_ok') and rows['customer-table'].get('isolation_ok')
                       and not rr['others_untouched'], {'events': rr['events'], 'review': rr['review']})
                ok, detail = gated(rows, ['void-invoices', 'customer-table'])
                expect(f'{arm}/{case}/bystanders-of-the-faults-gated-success', ok, detail)
                value = sentinel_for(rec)
                hits = files_containing(out / f'{arm}-{case}', value)
                planted = (out / f'{arm}-{case}' / 'agents' / 'invoice-list' / 'submitted-tree' / 'notes.env').exists()
                expect(f'{arm}/{case}/sentinel-absent-from-every-retained-file (completed run)', planted and not hits,
                       {'planted_file_retained': planted, 'files_with_sentinel': hits})
                # Round 8 item 1: the stored readings and the report's ledger rows carry no part of it either.
                readings = [c.get('reading') for a in rec['agents'].values() for c in a.get('service_commands') or []]
                ledger8 = report.ledger_rows(rec, 'p0.a1.' + arm + '/', str(out / f'{arm}-{case}'), {})
                expect(f'{arm}/{case}/sentinel-absent-from-readings-and-ledger-rows',
                       readings and value not in json.dumps(readings) and value not in json.dumps(ledger8, default=str)
                       and any('Cookie' in (c.get('command') or '') for a in rec['agents'].values() for c in a.get('service_commands') or []),
                       {'readings': len(readings), 'ledger_rows': len(ledger8)})
        except Exception as error:  # an invalid or interrupted run: its expectations fail, the validation goes on
            expect(f'{arm}/agent-kill-recovery-leaves/expectations-evaluated', False, repr(error))

        try:
            if 'service-kill-restarts' in only:
                case = 'service-kill-restarts'
                rec, rows, rr = run_case(arm, case)
                ok, detail = fault_at_plan(rec, 'service-kill', 'void-invoices')
                expect(f'{arm}/{case}/postmaster-killed-at-planned-point', ok, detail)
                ok, detail = four_services(rec)
                expect(f'{arm}/{case}/pre-kill-listing-has-all-four-services', ok, detail)
                copes = rr.get('victim_copes') or {}
                expect(f'{arm}/{case}/victim-copes (gated, restored on its own data directory)', copes.get('copes') and copes.get('service_restored')
                       and copes.get('description_test_after_kill'), copes)
                f = rec.get('fault') or {}
                listing = (f.get('running_at_kill') or {}).get('processes') or []
                scratch = [p['pid'] for p in listing if 'scratch-pg' in p['command'] and '/bin/postgres' in p['command']]
                trig_pg = ((f.get('trigger') or {}).get('services') or {}).get('postgres') or {}
                expect(f'{arm}/{case}/scratch-postmaster-never-trigger-nor-target', bool(scratch) and trig_pg.get('pid') not in scratch
                       and (f.get('kill') or {}).get('pid') not in scratch and 'scratch-pg' not in (trig_pg.get('command') or '')
                       and report._norm(f.get('canonical_data_dir')) in report._norm((trig_pg.get('command') or '').replace(' /private/', ' /')),
                       {'scratch': scratch, 'trigger_postgres': trig_pg, 'canonical': f.get('canonical_data_dir')})
                trig = (f.get('trigger') or {}).get('services') or {}
                scratch_other = {k: [p['pid'] for p in listing if '.pytest_cache/scratch' in p['command'] or
                                     (k == 'redis' and 'redis-server' in p['command'] and p['pid'] != (trig.get('redis') or {}).get('pid'))]
                                 for k in ('redis', 'api')}
                scratch_api = [p['pid'] for p in listing if re.search(r'\bpython3?\s+app\.py\s+\d+', p['command'])]
                expect(f'{arm}/{case}/scratch-redis-and-api-never-trigger-nor-target',
                       (trig.get('redis') or {}).get('port') == (f.get('recorded_ports') or {}).get('redis')
                       and (trig.get('api') or {}).get('port') == (f.get('recorded_ports') or {}).get('api')
                       and (trig.get('api') or {}).get('pid') not in scratch_api and bool(scratch_api)
                       and len([p for p in listing if 'redis-server' in p['command']]) >= 2,
                       {'trigger': {k: (v.get('pid'), v.get('port')) for k, v in trig.items()}, 'recorded_ports': f.get('recorded_ports'),
                        'scratch_api': scratch_api, 'redis_in_listing': [p['pid'] for p in listing if 'redis-server' in p['command']]})
                dirs = (rec['worktree_databases'].get('void-invoices') or {}).get('data_dirs') or {}
                expect(f'{arm}/{case}/isolation-reads-every-seen-postgres-data-directory', len(dirs) >= 2
                       and any('scratch-pg' in d and r.get('exists') for d, r in dirs.items()), {d: {k: r.get(k) for k in ('exists', 'versions', 'error')} for d, r in dirs.items()})
                expect(f'{arm}/{case}/kill-not-contamination', rr['others_untouched'] and rows['void-invoices'].get('isolation_ok'),
                       {'events': rr['events'], 'review': rr['review']})
                ex = rec['teardown']['invoice-list']
                expect(f'{arm}/{case}/exec-left-fails-teardown-at-exit', not rows['invoice-list'].get('teardown_ok')
                       and any('time.sleep(3600)' in p['command'] for p in rec['agents']['invoice-list'].get('left_running') or []),
                       rec['agents']['invoice-list'].get('left_running'))
                if arm == 'stack':
                    named = [d.get('pid') for r in ex.get('rounds') or [] for d in r.get('named') or []]
                    signalled = [p for r in ex.get('rounds') or [] for p in r.get('signalled') or []]
                    expect(f'{arm}/{case}/exec-left-down-unconfirmed-names-pid', ex.get('down_exit_first') == 70 and named and signalled == named
                           and not ex.get('clean_after_first_down') and '"stop_unconfirmed"' in ex['procedure'][0]['stdout'],
                           {'first_exit': ex.get('down_exit_first'), 'named': named, 'signalled': signalled, 'follow_up_s': ex.get('follow_up_seconds')})
                    expect(f'{arm}/{case}/exec-left-procedure-leaves-clean', ex.get('down_exit') == 0 and not ex['after_down']['processes']
                           and ex.get('clean_after_procedure') and ex.get('residue_ok') and not ex.get('false_confirmation'),
                           {'down_exit': ex.get('down_exit'), 'after_down': ex['after_down']})
                else:
                    expect(f'{arm}/{case}/exec-left-down-false-confirmation', ex.get('down_exit_first') == 0 and ex.get('false_confirmation')
                           and any('time.sleep(3600)' in p['command'] for p in ex['after_down']['processes']),
                           {'down_exit': ex.get('down_exit'), 'after_down': ex['after_down']})
                    expect(f'{arm}/{case}/exec-left-residue-fails', ex.get('residue_ok') is False, ex.get('after_down'))
                ok, detail = gated(rows, ['invoice-notes', 'pay-receipts', 'customer-table'])
                expect(f'{arm}/{case}/others-gated-success', ok, detail)
        except Exception as error:  # an invalid or interrupted run: its expectations fail, the validation goes on
            expect(f'{arm}/service-kill-restarts/expectations-evaluated', False, repr(error))

        try:
            if 'service-kill-no-reaction' in only:
                case = 'service-kill-no-reaction'
                rec, rows, rr = run_case(arm, case)
                ok, detail = fault_at_plan(rec, 'service-kill', 'customer-table')
                expect(f'{arm}/{case}/postmaster-killed-at-planned-point', ok, detail)
                ok, detail = four_services(rec)
                expect(f'{arm}/{case}/pre-kill-listing-has-all-four-services', ok, detail)
                expect(f'{arm}/{case}/victim-fails-its-task', not rows['customer-table'].get('passed') and not rows['customer-table'].get('gated_success')
                       and not (rr.get('victim_copes') or {}).get('copes'), {k: rows['customer-table'].get(k) for k in ('passed', 'cli_ok', 'failed_checks')})
                ok, detail = gated(rows, ['invoice-notes', 'pay-receipts', 'invoice-list', 'void-invoices'])
                expect(f'{arm}/{case}/other-agents-unaffected', ok and rr['others_untouched'], {'slots': detail, 'events': rr['events']})
        except Exception as error:  # an invalid or interrupted run: its expectations fail, the validation goes on
            expect(f'{arm}/service-kill-no-reaction/expectations-evaluated', False, repr(error))

        try:
            if 'service-kill-never-restores' in only:
                case = 'service-kill-never-restores'
                rec, rows, rr = run_case(arm, case)
                ok, detail = fault_at_plan(rec, 'service-kill', 'invoice-notes')
                expect(f'{arm}/{case}/postmaster-killed-at-planned-point', ok, detail)
                ok, detail = four_services(rec)
                expect(f'{arm}/{case}/pre-kill-listing-has-all-four-services', ok, detail)
                copes = rr.get('victim_copes') or {}
                expect(f'{arm}/{case}/completed-after-injection-holds-but-not-copes', copes.get('completed_after_injection') and not copes.get('copes')
                       and not copes.get('service_restored') and rr['episode_success'] is False, copes)
        except Exception as error:  # an invalid or interrupted run: its expectations fail, the validation goes on
            expect(f'{arm}/service-kill-never-restores/expectations-evaluated', False, repr(error))

        try:
            if 'event-g-identification' in only:
                case = 'event-g-identification'
                rec, rows, rr = run_case(arm, case)
                g = [e for e in rr['events'] if e['kind'] == 'sibling_service_lost']
                expect(f'{arm}/{case}/kill-naming-another-pid-is-not-event-g', not any(e['loss']['worktree'] == 'wt-pay' for e in g)
                       and rows['invoice-notes'].get('isolation_ok') and any(r['worktree'] == 'wt-pay' for r in rr['review']),
                       {'events': g, 'review': rr['review']})
                expect(f'{arm}/{case}/kill-naming-the-lost-pid-is-event-g-despite-own-up', any(e['loss']['worktree'] == 'wt-list' and e['culprits'] == ['customer-table']
                                                                                               and 'worker.py' in e['loss']['command'] for e in g)
                       and not rows['customer-table'].get('isolation_ok') and rows['invoice-list'].get('isolation_ok'), {'events': g})
        except Exception as error:  # an invalid or interrupted run: its expectations fail, the validation goes on
            expect(f'{arm}/event-g-identification/expectations-evaluated', False, repr(error))

        try:
            if 'version1-planted-and-collateral' in only:
                case = 'version1-planted-and-collateral'
                def collateral(task, argv):
                    if task != 'invoice-notes':   # torn down first; void-invoices still runs then
                        return argv
                    target = os.path.realpath(Path(rec_root[0]) / 'wt-void') + '/.venv/bin/python worker.py' if rec_root else ''
                    # SIGKILL: a worker exits up to a second after SIGTERM, outside the listing just after.
                    # The pattern names this run's own worktree only.
                    return ['sh', '-c', ' '.join(shlex.quote(a) for a in argv) + f'; pkill -9 -f {shlex.quote(re.escape(target))}; true']
                rec_root = []
                # the run root is known only once the run starts: read it from the sampler's first record
                orig = runner.tempfile.mkdtemp
                def mkdtemp(*a, **k):
                    path = orig(*a, **k)
                    rec_root.append(path)
                    return path
                runner.tempfile.mkdtemp = mkdtemp
                try:
                    rec, rows, rr = run_case(arm, case, down_override=collateral)
                finally:
                    runner.tempfile.mkdtemp = orig
                events = rr['events']
                left = rec['agents']['invoice-notes'].get('left_running') or []
                kinds = sorted({p['command'].split('/')[-1].split()[0] for p in left})
                expect(f'{arm}/{case}/leftover-detected (incl. postgres and redis)', not rows['invoice-notes']['teardown_ok']
                       and any('postgres' in p['command'] for p in left) and any('redis' in p['command'] for p in left), kinds)
                expect(f'{arm}/{case}/contamination-attributed', any(e['kind'] == 'migration_in_main' and 'pay-receipts' in e['culprits'] for e in events)
                       and not rows['pay-receipts']['isolation_ok'], events)
                expect(f'{arm}/{case}/collateral-kill-detected', any(e['kind'] == 'main_process_killed' and 'customer-table' in e['culprits'] for e in events),
                       [e for e in events if e['kind'] == 'main_process_killed'])
                td = rec['teardown']['invoice-notes']
                expect(f'{arm}/{case}/arm-down-recovers-dirty-checkout', len(left) > 0 and td.get('down_exit') == 0 and not td['after_down']['processes']
                       and td.get('residue_ok'), {'agent_left': len(left), 'after_down': td['after_down']})
                expect(f'{arm}/{case}/clean-agents-unaffected', all(rows[t]['teardown_ok'] for t in ('customer-table', 'pay-receipts', 'invoice-list')),
                       {t: r.get('teardown_ok') for t, r in rows.items()})
                coll = [e for e in events if e['kind'] == 'teardown_collateral']
                expect(f'{arm}/{case}/teardown-collateral-detected-and-gate-fails', any('wt-void' in e['detail'] and 'wt-notes' in e['detail']
                                                                                       and e['culprits'] == ['arm'] for e in coll)
                       and not rr['others_untouched'], coll)
        except Exception as error:  # an invalid or interrupted run: its expectations fail, the validation goes on
            expect(f'{arm}/version1-planted-and-collateral/expectations-evaluated', False, repr(error))

        try:
            if 'placement-and-attribution' in only:
                case = 'placement-and-attribution'
                rec, rows, rr = run_case(arm, case)
                # (a) A detached clean-environment orphan, working directory in wt-void: placed in the run.
                orphan = lambda ps: [p for p in ps or [] if 'http.server' in p['command']]
                void, td = rec['agents'].get('void-invoices') or {}, rec['teardown']['void-invoices']
                reclaim = td.get('harness_reclaim') or {}
                expect(f'{arm}/{case}/clean-environment-orphan-fails-exit-gate-and-residue',
                       orphan(void.get('left_running')) and all(p.get('tie') == 'placement' for p in orphan(void.get('left_running')))
                       and not rows['void-invoices'].get('teardown_ok')
                       and orphan(td['after_down']['processes']) and td.get('residue_ok') is False,
                       {'left_running': void.get('left_running'), 'after_down': td['after_down'], 'residue_ok': td.get('residue_ok')})
                ids = {p['pid'] for p in orphan(td['after_down']['processes'])}
                expect(f'{arm}/{case}/clean-environment-orphan-reclaimed', bool(ids) and ids <= set(reclaim.get('terminated') or [])
                       and not reclaim.get('survivors') and not (set(reclaim.get('not_signalled') or []) & ids), reclaim)
                # (b) A process with pay-receipts' marker holding wt-notes' file open: pay-receipts' only.
                reader = lambda ps: [p for p in ps or [] if 'time.sleep(3600)' in p['command'] and 'wt-notes/README.md' in p['command']]
                notes, pay = rec['agents'].get('invoice-notes') or {}, rec['agents'].get('pay-receipts') or {}
                tn, tp = rec['teardown']['invoice-notes'], rec['teardown']['pay-receipts']
                expect(f'{arm}/{case}/marked-reader-charged-to-its-own-worktree',
                       reader(pay.get('left_running')) and not rows['pay-receipts'].get('teardown_ok') and reader(tp['after_down']['processes'])
                       and tp.get('residue_ok') is False,
                       {'left_running': pay.get('left_running'), 'after_down': tp['after_down']})
                expect(f'{arm}/{case}/marked-reader-not-charged-to-the-sibling-whose-file-it-holds',
                       not reader(notes.get('left_running')) and not reader(notes.get('seen_not_counted')) and rows['invoice-notes'].get('teardown_ok')
                       and not reader(tn['after_down']['processes']) and tn.get('residue_ok') and not tn.get('false_confirmation'),
                       {'left_running': notes.get('left_running'), 'seen_not_counted': notes.get('seen_not_counted'), 'after_down': tn['after_down'],
                        'teardown_ok': rows['invoice-notes'].get('teardown_ok')})
                ok, detail = gated(rows, ['customer-table', 'invoice-list', 'invoice-notes'])
                expect(f'{arm}/{case}/others-gated-success', ok, detail)
        except Exception as error:  # an invalid or interrupted run: its expectations fail, the validation goes on
            expect(f'{arm}/placement-and-attribution/expectations-evaluated', False, repr(error))

        try:
            try:
                if 'publication-failure' in only:
                    case = 'publication-failure'
                    rec, rows, rr = run_case(arm, case, env={'AGENTVALUE_SELFTEST_UNREADABLE_CAPTURE': '1'}, expect_valid=False)
                    expect(f'{arm}/{case}/run-record-written-and-invalid', (out / f'{arm}-{case}' / 'run.json').exists()
                           and any('publication failed' in r for r in rec.get('invalid_reasons') or []) and rec.get('publication_error'),
                           {'invalid': rec.get('invalid_reasons'), 'publication_error': rec.get('publication_error')})
                    expect(f'{arm}/{case}/cleanup-ran-before-publication', 'survivors_after_run' in rec and not rec.get('survivors_after_run'),
                           {'survivors': rec.get('survivors_after_run')})
                    if rec.get('root') and Path(rec['root']).exists():
                        shutil.rmtree(rec['root'], ignore_errors=True)
            except Exception as error:
                expect(f'{arm}/publication-failure/expectations-evaluated', False, repr(error))

            if 'interrupts' in only:
                for when in INTERRUPTS:
                    interrupt_case(arm, when, plan_path, out, lock, expect)
        except Exception as error:  # an invalid or interrupted run: its expectations fail, the validation goes on
            expect(f'{arm}/interrupts/expectations-evaluated', False, repr(error))

        try:
            if arm == 'stack' and 'exec-control-dc4b980' in only:
                case = 'exec-control-dc4b980'
                rec, rows, rr = run_case(arm, case, control_plan)
                ex = rec['teardown']['invoice-list']
                expect(f'{arm}/{case}/control-uses-old-binary', rec['stack_binary_sha256'] == OLD_STACK_SHA, rec['stack_binary_sha256'])
                expect(f'{arm}/{case}/exec-left-fails-teardown-at-exit', not rows['invoice-list'].get('teardown_ok'),
                       rec['agents']['invoice-list'].get('left_running'))
                expect(f'{arm}/{case}/down-confirms-while-command-runs (false confirmation)', ex.get('down_exit_first') == 0 and ex.get('false_confirmation')
                       and not ex.get('rounds') and any('time.sleep(3600)' in p['command'] for p in ex['after_down']['processes']),
                       {'down_exit': ex.get('down_exit'), 'after_down': ex['after_down']})
                expect(f'{arm}/{case}/residue-fails', ex.get('residue_ok') is False, ex.get('after_down'))
        except Exception as error:  # an invalid or interrupted run: its expectations fail, the validation goes on
            expect(f'{arm}/exec-control-dc4b980/expectations-evaluated', False, repr(error))

    summary = {'ok': not failures, 'failures': failures, 'checks': checks, 'seconds': round(time.monotonic() - started), 'run_seconds': timings,
               'stack_binary_sha256': plan['config']['stack_binary_sha256'], 'stack_rev': plan['config']['stack_rev'],
               'control_stack_binary_sha256': control_plan['config']['stack_binary_sha256'],
               'source_rev': plan['config']['source_rev'], 'fingerprints': plan['fingerprints'], 'cases_run': sorted(only)}
    (out / 'validation.json').write_text(json.dumps(summary, indent=2, default=str))
    print(json.dumps({'ok': summary['ok'], 'failures': failures, 'seconds': summary['seconds']}))
    lock[0].close()
    sys.exit(0 if summary['ok'] else 1)


def validation_interrupt_case(out, lock, expect, during='episode'):
    """An outside SIGTERM sent to a running validation ends the whole validation after the current
    run's cleanup: a non-zero exit, a line saying so, no later case, nothing of the run left. With
    `during='cleanup'` the signal arrives while the run's cleanup holds signals off: it is held, not
    dropped, and the validation still stops once the run returns."""
    suffix = '' if during == 'episode' else f'-{during}'
    name = f'validation-interrupt{suffix}'
    nested = out / f'nested-validation-interrupted{suffix}'
    lock[0].close()   # the nested validation takes the host lock itself
    log = open(out / f'nested-validation-interrupted{suffix}.log', 'w')
    proc = subprocess.Popen([sys.executable, str(HERE / 'validate.py'), '--out', str(nested), '--skip-grader', '--arms', 'stack',
                             '--only', 'clean,service-kill-no-reaction'], stdout=log, stderr=subprocess.STDOUT, start_new_session=True)
    with forwarding(proc):   # an interrupt of this validation reaches the nested process, which is waited for
        progress = nested / 'stack-clean' / 'progress.jsonl'
        point, poll = ('launched', 0.2) if during == 'episode' else ('shutdown_begin', 0.02)
        deadline = time.monotonic() + (300 if during == 'episode' else 1200)
        while time.monotonic() < deadline and proc.poll() is None and not (progress.exists() and point in progress.read_text()):
            time.sleep(poll)
        if during == 'episode':
            time.sleep(2)
        sent = proc.poll() is None
        if sent:
            os.kill(proc.pid, signal.SIGTERM)
        try:
            proc.wait(600)
        except subprocess.TimeoutExpired:
            proc.kill()
    log.close()
    lock[0] = runner.host_lock()
    text = (out / f'nested-validation-interrupted{suffix}.log').read_text()
    rec_path = nested / 'stack-clean' / 'run.json'
    rec = json.loads(rec_path.read_text()) if rec_path.exists() else {}
    leftovers = []
    if rec.get('root'):
        observe.RUN_SUFFIX = '.' + rec['run_id']
        try:
            table = observe.legacy.process_table()
            leftovers = [p for p in observe.run_processes(rec['root']) if observe.tie(p, rec['root'], table)]
        finally:
            observe.RUN_SUFFIX = None
        shutil.rmtree(rec['root'], ignore_errors=True)
    expect(f'{name}/outside-sigterm-ends-the-whole-validation',
           sent and proc.returncode == 128 + signal.SIGTERM and 'INTERRUPTED' in text
           and 'service-kill-no-reaction' not in text and not (nested / 'stack-service-kill-no-reaction').exists(),
           {'sent': sent, 'exit': proc.returncode, 'tail': text[-300:]})
    expect(f'{name}/current-run-cleaned-up-and-recorded', rec and not rec.get('survivors_after_run') and not leftovers and (
               (not rec.get('valid') and any('interrupted' in r for r in rec.get('invalid_reasons') or [])) if during == 'episode'
               else rec.get('signals_during_cleanup') == [signal.SIGTERM]),
           {'invalid': rec.get('invalid_reasons'), 'survivors': rec.get('survivors_after_run'), 'leftovers': [p['command'][:80] for p in leftovers],
            'signals_during_cleanup': rec.get('signals_during_cleanup')})


def interrupt_case(arm, when, plan_path, out, lock, expect):
    """The runner is interrupted (SIGINT) before the kill, during the 10 s before the recovery launch,
    or during that launch: nothing of the run survives, no session starts after cleanup has begun,
    and a planted sentinel reaches no retained file."""
    case = f'interrupt-{when}'
    run_out = out / f'{arm}-{case}'
    env = dict(os.environ)
    if when == 'during-the-launch':
        env['AGENTVALUE_SELFTEST_LAUNCH_PAUSE'] = '5'
    argv = [sys.executable, str(HERE / 'run.py'), 'arm', '--plan', str(plan_path), '--arm', arm, '--out', str(run_out),
            '--scripted', 'invoice-list=stall,pay-receipts=sentinel', '--scenario', 'agent-kill', '--victim', 'invoice-list']
    lock[0].close()   # the runner takes the host lock itself
    log = open(out / f'{arm}-{case}.log', 'w')
    proc = subprocess.Popen(argv, stdout=log, stderr=subprocess.STDOUT, start_new_session=True, env=env)
    with forwarding(proc):   # an interrupt of this validation reaches the nested process, which is waited for
        progress = run_out / 'progress.jsonl'
        wanted = {'before-the-kill': ('launched', 1.0), 'during-the-delay': ('kill', 3.0), 'during-the-launch': ('recovery_launch_begin', 1.5)}[when]
        deadline = time.monotonic() + 300
        seen = []
        while time.monotonic() < deadline and proc.poll() is None:
            seen = [json.loads(l)['event'] for l in progress.read_text().splitlines()] if progress.exists() else []
            if wanted[0] in seen:
                break
            time.sleep(0.1)
        time.sleep(wanted[1])
        seen_at_signal = [json.loads(l)['event'] for l in progress.read_text().splitlines()] if progress.exists() else []
        if proc.poll() is None:
            os.kill(proc.pid, signal.SIGINT)
        try:
            proc.wait(600)
        except subprocess.TimeoutExpired:
            proc.kill()
    log.close()
    lock[0] = runner.host_lock()
    rec = json.loads((run_out / 'run.json').read_text()) if (run_out / 'run.json').exists() else {}
    root = rec.get('root')
    time.sleep(15)   # longer than the recovery delay: a session started late would show now
    leftovers = []
    if root:
        observe.RUN_SUFFIX = '.' + rec['run_id']
        try:
            leftovers = observe.run_processes(root)
            if leftovers:
                observe.reclaim(leftovers, root)
        finally:
            observe.RUN_SUFFIX = None
    expect(f'{arm}/{case}/signal-landed-where-planned', wanted[0] in seen_at_signal and
           {'before-the-kill': 'kill' not in seen_at_signal, 'during-the-delay': 'recovery_launch_begin' not in seen_at_signal,
            'during-the-launch': 'recovery_launched' not in seen_at_signal}[when], seen_at_signal)
    expect(f'{arm}/{case}/run-invalid-as-interrupted', rec and not rec.get('valid') and any('interrupted' in r for r in rec.get('invalid_reasons') or []),
           rec.get('invalid_reasons'))
    expect(f'{arm}/{case}/nothing-of-the-run-survives', rec and not rec.get('survivors_after_run') and not leftovers,
           {'survivors': rec.get('survivors_after_run'), 'after_15s': [p['command'][:80] for p in leftovers]})
    agents = rec.get('agents') or {}
    recovery = agents.get('invoice-list-recovery')
    no_late = not rec.get('launches_after_shutdown') and all(l['m'] <= rec.get('shutdown_m', 0) for l in rec.get('launches') or [])
    shape = {'before-the-kill': not (rec.get('fault') or {}).get('injected') and recovery is None,
             'during-the-delay': (rec.get('fault') or {}).get('injected') and recovery is None,
             'during-the-launch': recovery is not None and recovery.get('launched_m', 1e9) <= rec.get('shutdown_m', 0)}[when]
    expect(f'{arm}/{case}/no-session-after-cleanup-began', no_late and shape,
           {'launches': rec.get('launches'), 'shutdown_m': rec.get('shutdown_m'), 'recovery': bool(recovery)})
    value = sentinel_for(rec) if root else None
    hits = files_containing(run_out, value) if value else ['no record']
    published = (run_out / 'agents' / 'pay-receipts' / 'trace.jsonl').exists()
    expect(f'{arm}/{case}/sentinel-absent-from-every-retained-file (interrupted run)', published and not hits,
           {'trace_published': published, 'files_with_sentinel': hits})
    if root and Path(root).exists():
        shutil.rmtree(root, ignore_errors=True)


def unit_cases(expect, plan, plan_path=None):
    """The reviewer's reproductions that need no full run, and the report checks on synthetic records."""
    import copy
    import random
    import redact
    _redact_scrub = redact.scrub
    # A credential-like key holding a list or a nested object is redacted.
    scrubbed = json.dumps(redact.scrub_json({'SOME_TOKEN': ['sentinel-a'], 'outer': {'API_KEY': {'v': 'sentinel-b'}, 'ok': 'kept'}}))
    expect('unit/redactor-credential-key-holding-list-or-object', 'sentinel-a' not in scrubbed and 'sentinel-b' not in scrubbed and 'kept' in scrubbed,
           scrubbed)
    # A file that cannot be scrubbed is never published.
    with tempfile.TemporaryDirectory(prefix='avu-', dir='/tmp') as tmp:
        src, dest = Path(tmp) / 'stage' / 'trace.jsonl', Path(tmp) / 'results'
        src.parent.mkdir()
        dest.mkdir()
        src.write_text('{"x": 1}\n')
        src.chmod(0)
        ok, reason = runner.publish_file(src, dest / 'trace.jsonl')
        src.chmod(0o600)
        expect('unit/unscrubbable-trace-never-published', not ok and not (dest / 'trace.jsonl').exists() and not list(dest.iterdir()),
               {'published': ok, 'reason': reason, 'results_dir': [p.name for p in dest.iterdir()]})
    # A remembered pid whose process group holds no process of the run: nothing outside the run is signalled.
    with tempfile.TemporaryDirectory(prefix='avr-unit-', dir='/tmp') as root:
        outsider = subprocess.Popen(['sleep', '30'], start_new_session=True, cwd='/', stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        try:
            observe.RUN_SUFFIX = '.unit-test-run'
            signalled = runner.safe_killpg(outsider.pid, Path(root))
            time.sleep(0.3)
            alive = outsider.poll() is None
        finally:
            observe.RUN_SUFFIX = None
            outsider.kill()
            outsider.wait()
        expect('unit/cleanup-never-signals-a-group-without-a-run-process', not signalled and alive, {'signalled': signalled, 'outsider_alive': alive})
    # Event (g): text around the kill, an own restart of another service, a kill command of a killed CLI.
    loss = {'group': 'wt-a', 'pid': 123, 'started': 's', 'command': '/x/bin/postgres -D /tmp/avr1/wt-a/.dev/postgres', 'kind': 'postgres',
            'ports': [31000], 'last_seen_m': 20.0, 'gone_m': 21.0}
    def g_run(other_cmds, own_cmds, killed=False):
        return {'agents': {'a': {'marker': 'wt-a', 'launched_m': 0, 'ended_m': 100, 'service_commands': own_cmds},
                           'b': {'marker': 'wt-b', 'launched_m': 0, 'ended_m': 5 if killed else 100, 'killed_by_harness': killed,
                                 'service_commands': other_cmds}},
                'sampler': {'durable_losses': [loss]}, 'episode_end_m': 100}
    k = lambda c, s0, e0: {'start_m': s0, 'end_m': e0, 'is_error': False if e0 is not None else None, 'kinds': ['kill'], 'kill_like': True, 'command': c}
    expect('unit/echo-lost-pid-then-kill-another-pid-is-not-event-g', not report.sibling_losses(g_run([k('echo 123; kill -9 456', 20.5, 20.6)], []))[0],
           report.sibling_losses(g_run([k('echo 123; kill -9 456', 20.5, 20.6)], [])))
    own_restart_api = [{'start_m': 15, 'end_m': 16, 'kinds': ['down_restart'], 'kill_like': False, 'command': 'stack restart api'}]
    ev = report.sibling_losses(g_run([k('kill -9 123', 20.5, 20.6)], own_restart_api))[0]
    expect('unit/own-restart-of-another-service-does-not-make-postmaster-kill-ambiguous', len(ev) == 1 and ev[0]['culprits'] == ['b'], ev)
    ev, rv, _ = report.sibling_losses(g_run([k('kill -9 123', 4.0, None)], [], killed=True))
    expect('unit/kill-command-of-a-killed-cli-with-no-result-is-review-not-gated', not ev and any(r['pid'] == 123 for r in rv), {'events': ev, 'review': rv})
    ev, rv, _ = report.sibling_losses(g_run([k('false && kill -9 123', 20.5, 20.6)], []))
    expect('unit/conditional-kill-is-review-not-event-g', not ev and any(r['pid'] == 123 for r in rv), {'events': ev, 'review': rv})
    ev, rv, _ = report.sibling_losses(g_run([k('kill -s 123 456', 20.5, 20.6)], []))
    expect('unit/numeric-signal-argument-is-not-a-pid', not ev, {'events': ev, 'review': rv})
    ev, rv, _ = report.sibling_losses(g_run([k('pkill -x postgres', 20.5, 20.6)], []))
    expect('unit/unmodelled-pkill-flag-is-review-not-event-g', not ev and any(r['pid'] == 123 for r in rv), {'events': ev, 'review': rv})
    # Round 4: gating needs a completed kill with a result that is not an error; kill -0 signals nothing.
    T0 = 1_000_000.0
    stamp = lambda wall: time.strftime('%a %b %d %H:%M:%S %Y', time.localtime(wall))
    loss4 = {'group': 'wt-a', 'pid': 123, 'started': stamp(T0 + 1), 'command': '/tmp/avr1/wt-a/.venv/bin/python worker.py', 'kind': 'worker',
             'ports': [], 'last_seen_m': 20.0, 'gone_m': 21.0}
    def run4(other, own, the_loss=loss4):
        return {'clock': {'started_wall': T0}, 'episode_end_m': 100, 'sampler': {'durable_losses': [the_loss]},
                'agents': {'a': {'marker': 'wt-a', 'launched_m': 0, 'ended_m': 100, 'service_commands': own},
                           'b': {'marker': 'wt-b', 'launched_m': 0, 'ended_m': 100, 'service_commands': other}}}
    K4 = lambda cmd, err=False, end=20.6, start=20.5: {'start_m': start, 'end_m': end, 'is_error': err, 'kinds': ['kill'], 'kill_like': True, 'command': cmd}
    for label, other, own in [('error-result', [K4('kill -9 123', err=True)], []), ('several-targets', [K4('kill -9 123 456')], []),
                              ('no-result', [K4('kill -9 123', end=None)], [])]:
        ev, rv, _ = report.sibling_losses(run4(other, own))
        expect(f'unit/kill-with-{label}-is-review-not-event-g', not ev and any(r['pid'] == 123 and 'kill_by' in r for r in rv), {'events': ev, 'review': rv})
    ev, rv, _ = report.sibling_losses(run4([K4('kill -0 123')], []))
    expect('unit/kill-0-is-not-a-culprit', not ev and not any('kill_by' in r for r in rv), {'events': ev, 'review': rv})
    ev, rv, _ = report.sibling_losses(run4([K4('kill -9 123')], [K4('kill -0 123', start=15, end=15.2)]))
    expect('unit/own-kill-0-is-not-an-own-command', len(ev) == 1 and ev[0]['culprits'] == ['b'], {'events': ev, 'review': rv})
    # Round 4: the own-command rule's three outcomes.
    O4 = lambda cmd, start=15, err=False: {'start_m': start, 'end_m': start + 0.5, 'is_error': err, 'kinds': ['down_restart'],
                                          'kill_like': False, 'command': cmd}
    clock4 = {'clock': {'started_wall': T0}}
    expect('unit/false-and-restart-with-error-result-may-have-run-not-excused',
           report.own_effect(clock4, O4('false && stack restart worker', err=True), loss4) == 'may'
           and bool(report.sibling_losses(run4([K4('kill -9 123')], [O4('false && stack restart worker', err=True)]))[1]),
           report.own_effect(clock4, O4('false && stack restart worker', err=True), loss4))
    expect('unit/and-chain-restart-with-success-result-excuses', report.own_effect(clock4, O4('test -d . && python3 dev.py restart'), loss4) == 'excuses',
           report.own_effect(clock4, O4('test -d . && python3 dev.py restart'), loss4))
    newer = dict(loss4, started=stamp(T0 + 16))
    expect('unit/completed-restart-excuses-nothing-for-the-new-incarnation', report.own_effect(clock4, O4('stack restart worker'), newer) is None,
           report.own_effect(clock4, O4('stack restart worker'), newer))
    coll4 = {'clock': {'started_wall': T0}, 'episode_end_m': 100, 'bystander': {},
             'teardown': {'invoice-notes': {'collateral': [{'group': 'wt-void', 'pid': 777, 'started': stamp(T0 + 1),
                                                            'command': '/tmp/avr1/wt-void/.venv/bin/python worker.py'}]}},
             'agents': {'v': {'marker': 'wt-void', 'service_commands': [O4('stack down', start=90, err=True)]}}}
    ev, rv = report.teardown_collateral(coll4)
    expect('unit/errored-down-then-slow-exit-is-review', not ev and len(rv) == 1 and 'may have stopped' in rv[0]['reason'], {'events': ev, 'review': rv})
    # Round 5 item 3 (pinned, unchanged): one successful `pkill -f worker.py` and two matching losses in
    # its window are two events of that one command, both charged to its session.
    loss_c = dict(loss4, group='wt-c', pid=124, command='/tmp/avr1/wt-c/.venv/bin/python worker.py')
    run3 = run4([K4('pkill -f worker.py')], [])
    run3['sampler']['durable_losses'] = [loss4, loss_c]
    ev, rv, _ = report.sibling_losses(run3)
    expect('unit/pkill-pattern-matching-two-lost-workers-is-two-events', len(ev) == 2 and all(e['culprits'] == ['b'] for e in ev)
           and {e['loss']['pid'] for e in ev} == {123, 124} and not rv, {'events': ev, 'review': rv})
    # Round 5 item 4: a read-only command never excuses; a chain member inside a longer block may have run.
    listener = dict(loss4, ports=[6001])
    R5 = lambda cmd: dict(O4(cmd), kinds=['kill'], kill_like=True)
    eff = report.own_effect(clock4, R5('lsof -ti :6001'), listener)
    ev, rv, _ = report.sibling_losses(run4([K4('kill -9 123')], [R5('lsof -ti :6001')], the_loss=listener))
    expect('unit/read-only-lsof-excuses-nothing-loss-scored-as-if-absent', eff is None and len(ev) == 1 and ev[0]['culprits'] == ['b'],
           {'own_effect': eff, 'events': ev, 'review': rv})
    expect('unit/own-lsof-piped-to-kill-of-the-port-excuses', report.own_effect(clock4, R5('lsof -ti :6001 | xargs kill'), listener) == 'excuses'
           and report.own_effect(clock4, R5('pgrep -f worker.py; ps -p 123'), listener) is None,
           [report.own_effect(clock4, R5('lsof -ti :6001 | xargs kill'), listener), report.own_effect(clock4, R5('pgrep -f worker.py; ps -p 123'), listener)])
    outcomes = {cmd: report.own_effect(clock4, O4(cmd), loss4) for cmd in
                ('false && stack restart worker\ntrue', 'stack restart worker; true', 'cd x && stack restart worker')}
    expect('unit/chain-member-in-a-longer-block-may-statement-of-its-own-excuses',
           outcomes == {'false && stack restart worker\ntrue': 'may', 'stack restart worker; true': 'excuses', 'cd x && stack restart worker': 'excuses'},
           outcomes)
    ev, rv, _ = report.sibling_losses(run4([K4('cd x && kill -9 123\ntrue')], []))
    expect('unit/kill-as-chain-member-in-a-longer-block-is-review-not-event-g', not ev and any(r['pid'] == 123 and 'kill_by' in r for r in rv),
           {'events': ev, 'review': rv})
    # Round 6 item 3: a broad kill is blamed only for matching processes that existed when it returned.
    pk = K4('pkill -f worker.py', start=19.9, end=20.0)
    loss_b6 = dict(loss4, last_seen_m=19.5, gone_m=20.5)
    loss_c6 = dict(loss4, group='wt-c', pid=125, started=stamp(T0 + 20.1), command='/tmp/avr1/wt-c/.venv/bin/python worker.py',
                   last_seen_m=20.2, gone_m=20.6)
    run6 = run4([pk], [])
    run6['sampler']['durable_losses'] = [loss_b6, loss_c6]
    ev, rv, _ = report.sibling_losses(run6)
    expect('unit/broad-kill-blamed-only-for-processes-that-existed-when-it-returned',
           [e['loss']['pid'] for e in ev] == [123] and ev[0]['culprits'] == ['b'] and any(r['pid'] == 125 and 'kill_by' in r for r in rv),
           {'events': ev, 'review': rv})
    # Round 6 item 4: xargs is a kill only when it runs one; a statement containing || proves nothing.
    eff6 = {cmd: report.own_effect(clock4, R5(cmd), listener) for cmd in ('lsof -ti :6001 | xargs echo kill', 'lsof -ti :6001 | xargs kill')}
    ev, rv, _ = report.sibling_losses(run4([K4('lsof -ti :6001 | xargs echo kill')], [], the_loss=listener))
    expect('unit/xargs-running-echo-is-no-kill-own-or-culprit', eff6 == {'lsof -ti :6001 | xargs echo kill': None, 'lsof -ti :6001 | xargs kill': 'excuses'}
           and not ev and not any('kill_by' in r for r in rv), {'own_effect': eff6, 'events': ev, 'review': rv})
    or6 = {cmd: report.own_effect(clock4, O4(cmd), loss4) for cmd in ('false && stack restart worker || true', 'stack restart worker || true')}
    ev, rv, _ = report.sibling_losses(run4([K4('kill -9 123 || true')], []))
    expect('unit/statement-with-or-never-proves-a-member-ran (own may, culprit review)',
           or6 == {'false && stack restart worker || true': 'may', 'stack restart worker || true': 'may'}
           and not ev and any(r['pid'] == 123 and 'kill_by' in r for r in rv), {'own_effect': or6, 'events': ev, 'review': rv})
    # Round 7 item 4: blame needs the lost process to have existed when the kill was issued.
    pk7 = K4('pkill -f worker.py', start=20.0, end=22.1)
    loss_c7 = dict(loss4, group='wt-c', pid=126, started=stamp(T0 + 21), command='/tmp/avr1/wt-c/.venv/bin/python worker.py',
                   last_seen_m=21.0, gone_m=21.9)
    run7 = run4([pk7], [])
    run7['sampler']['durable_losses'] = [dict(loss4, last_seen_m=20.5, gone_m=21.5), loss_c7]
    ev, rv, _ = report.sibling_losses(run7)
    expect('unit/broad-kill-blamed-only-for-processes-that-existed-when-it-was-issued',
           [e['loss']['pid'] for e in ev] == [123] and any(r['pid'] == 126 and 'kill_by' in r for r in rv), {'events': ev, 'review': rv})
    # Round 7 item 5: signal 0 stays a non-signal through a pipeline; a review-only reading never excuses.
    eff7 = {cmd: report.own_effect(clock4, R5(cmd), listener) for cmd in
            ('lsof -ti :6001 | xargs kill -0', 'lsof -ti :6001 | xargs kill -s 0', 'lsof -ti :6001 | xargs kill -USR1', 'kill -USR1 123')}
    ev, rv, _ = report.sibling_losses(run4([K4('lsof -ti :6001 | xargs kill -0')], [], the_loss=listener))
    expect('unit/signal-0-through-a-pipeline-is-no-kill-and-review-only-never-excuses',
           eff7 == {'lsof -ti :6001 | xargs kill -0': None, 'lsof -ti :6001 | xargs kill -s 0': None,
                    'lsof -ti :6001 | xargs kill -USR1': 'may', 'kill -USR1 123': 'may'}
           and not ev and not any('kill_by' in r for r in rv), {'own_effect': eff7, 'events': ev, 'review': rv})
    # Round 7 item 2: the kill is read from the original command at capture, so redaction cannot erase it.
    with tempfile.TemporaryDirectory(prefix='avu-', dir='/tmp') as d7:
        A7 = 'Author' + 'ization'
        original = f'echo "{A7}: Bearer abcdefgh12345678"; kill -9 123'
        trace7, arrivals7 = Path(d7) / 'trace.jsonl', Path(d7) / 'arrivals.tsv'
        trace7.write_text(json.dumps({'type': 'assistant', 'message': {'content': [{'type': 'tool_use', 'id': 't1', 'name': 'Bash',
                                                                                   'input': {'command': original}}]}}) + '\n'
                          + json.dumps({'type': 'user', 'message': {'content': [{'type': 'tool_result', 'tool_use_id': 't1', 'content': '',
                                                                                 'is_error': False}]}}) + '\n')
        arrivals7.write_text('line\tseconds\n1\t20.5\n2\t20.6\n')
        captured = runner.service_commands(trace7, arrivals7, 0.0)
    run27 = run4(captured, [])
    ev, rv, _ = report.sibling_losses(run27)
    text_only = [{k: v for k, v in c.items() if k != 'reading'} for c in captured]
    ev_text, _, _ = report.sibling_losses(run4(text_only, []))
    expect('unit/kill-erased-by-redaction-still-scored-from-the-captured-reading',
           len(captured) == 1 and 'kill -9 123' not in captured[0]['command'] and 'abcdefgh' not in json.dumps(captured)
           and captured[0].get('line') == 1 and len(ev) == 1 and ev[0]['culprits'] == ['b'] and not ev_text,
           {'captured': captured, 'events': ev, 'events_from_scrubbed_text_alone': ev_text})
    # Round 7 item 3: the adjudication ledger: one full row per lost process; decisions drive the gate.
    l7 = lambda group, pid, cmd: {'group': group, 'pid': pid, 'started': stamp(T0 + 1), 'command': cmd, 'kind': report.service_kind_of(cmd),
                                  'ports': [], 'last_seen_m': 20.0, 'gone_m': 21.0}
    long_cmd = '/tmp/avr1/wt-e/.venv/bin/python worker.py ' + 'x' * 5000
    run37 = {'clock': {'started_wall': T0}, 'episode_end_m': 100,
             'sampler': {'durable_losses': [l7('wt-a', 123, '/tmp/avr1/wt-a/.venv/bin/python worker.py'), l7('wt-c', 300, '/tmp/avr1/wt-c/.venv/bin/python worker.py'),
                                            l7('wt-d', 400, '/tmp/avr1/wt-d/.venv/bin/python worker.py'), l7('wt-e', 500, long_cmd)]},
             'agents': {'a': {'marker': 'wt-a', 'launched_m': 0, 'ended_m': 100, 'service_commands': []},
                        'b': {'marker': 'wt-b', 'launched_m': 0, 'ended_m': 100,
                              'service_commands': [dict(K4('kill -9 123'), line=7), dict(K4('false && kill -9 400'), line=9)]},
                        'c': {'marker': 'wt-c', 'launched_m': 0, 'ended_m': 100, 'service_commands': [dict(O4('stack restart worker'), line=3)]}}}
    prefix7 = 'p0.a1.stack/'
    ledger7 = report.ledger_rows(run37, prefix7, 'R/pair0-stack', {})
    by_pid = {r['pid']: r for r in ledger7}
    full = all(k in r for r in ledger7 for k in ('id', 'pid', 'started', 'command', 'worktree', 'lost_between_m', 'candidates', 'automatic_outcome', 'reason'))
    cand_ok = all(all(k in c for k in ('session', 'trace', 'line', 'issued_m', 'returned_m', 'result', 'reading', 'command', 'reason'))
                  for r in ledger7 for c in r['candidates'])
    expect('unit/ledger-row-per-loss-blamed-excused-review-unexplained-with-candidates',
           full and cand_ok and {by_pid[p]['disposition'] for p in by_pid} == {'blamed', 'excused', 'review', 'unexplained'}
           and by_pid[123]['disposition'] == 'blamed' and by_pid[300]['disposition'] == 'excused' and by_pid[400]['disposition'] == 'review'
           and by_pid[500]['disposition'] == 'unexplained' and by_pid[123]['candidates'][0]['trace'] == 'R/pair0-stack/agents/b/trace.jsonl'
           and by_pid[123]['candidates'][0]['line'] == 7 and by_pid[500]['command'] == long_cmd,
           [{k: r[k] for k in ('id', 'disposition', 'reason')} | {'candidates': len(r['candidates'])} for r in ledger7])
    decisions7 = {by_pid[123]['id']: {'decision': 'not-an-event', 'reason': 'its own agent stopped it (trace line 4)'},
                  by_pid[500]['id']: {'decision': 'blamed:b', 'reason': 'b killed it by a form the classifier does not read'}}
    ev_auto, _, _ = report.sibling_losses(run37)
    ev_adj, _, _ = report.sibling_losses(run37, decisions7, prefix7)
    overrides7 = [r for r in report.ledger_rows(run37, prefix7, 'R', decisions7) if r['decision']]
    expect('unit/adjudication-override-changes-the-gate-and-is-listed',
           [e['loss']['pid'] for e in ev_auto] == [123] and [(e['loss']['pid'], e['culprits']) for e in ev_adj] == [(500, ['b'])]
           and {(r['pid'], r['automatic_outcome'], r['adjudicated_outcome']) for r in overrides7} == {(123, 'blamed (b)', 'not-an-event'), (500, 'unexplained', 'blamed (b)')},
           {'automatic': ev_auto, 'adjudicated': ev_adj, 'overrides': [(r['id'], r['automatic_outcome'], r['adjudicated_outcome']) for r in overrides7]})
    refused7 = []
    for bad in ({'decision': 'collateral', 'reason': 'x'}, {'decision': 'blamed:nobody', 'reason': 'x'}, {'decision': 'not-an-event'}):
        try:
            report.sibling_losses(run37, {by_pid[123]['id']: bad}, prefix7)
            refused7.append(False)
        except ValueError:
            refused7.append(True)
    with tempfile.TemporaryDirectory(prefix='avu-', dir='/tmp') as d73:
        (Path(d73) / 'adjudication.json').write_text(json.dumps({'p9.a1.stack/wt-a/1/2': {'decision': 'review', 'reason': 'x'}}))
        try:
            report.rows_v2({'pairs': [{'index': 0, 'order': ['stack']}], 'config': {'tasks': []}}, {(0, 'stack'): None}, d73)
            refused7.append(False)
        except ValueError:
            refused7.append(True)
    expect('unit/adjudication-unknown-row-or-decision-refused', refused7 == [True, True, True, True], refused7)
    md7 = '\n'.join(report.markdown_ledger({'ledger': ledger7, 'adjudication': {'note': 'n', 'overrides': [], 'side_by_side': []}}))
    expect('unit/ledger-markdown-untruncated', long_cmd in md7 and all(r['id'] in md7 for r in ledger7) and 'kill -9 123' in md7,
           {'length': len(md7)})
    # Round 8 item 1: the reading keeps structured facts only; the kill is still scored.
    V8 = 'SYNTH' + 'ETIC-r8-value'   # split: the commit scan would refuse the whole literal
    with tempfile.TemporaryDirectory(prefix='avu-', dir='/tmp') as d8:
        original8 = f"printf '%s %s' 'Cookie:' 'session={V8}'; kill -9 123"
        trace8, arrivals8 = Path(d8) / 'trace.jsonl', Path(d8) / 'arrivals.tsv'
        trace8.write_text(json.dumps({'type': 'assistant', 'message': {'content': [{'type': 'tool_use', 'id': 't1', 'name': 'Bash',
                                                                                   'input': {'command': original8}}]}}) + '\n'
                          + json.dumps({'type': 'user', 'message': {'content': [{'type': 'tool_result', 'tool_use_id': 't1', 'content': '',
                                                                                 'is_error': False}]}}) + '\n')
        arrivals8.write_text('line\tseconds\n1\t20.5\n2\t20.6\n')
        captured8 = runner.service_commands(trace8, arrivals8, 0.0)
    run18 = run4(captured8, [])
    ev, rv, _ = report.sibling_losses(run18)
    rows18 = report.ledger_rows(run18, 'p0.a1.stack/', 'R', {})
    expect('unit/reading-keeps-facts-only-split-credential-absent-kill-still-scored',
           len(captured8) == 1 and 'session' not in json.dumps(captured8[0]['reading']) and V8 not in json.dumps(captured8)
           and V8 not in json.dumps(rows18, default=str) and len(ev) == 1 and ev[0]['culprits'] == ['b'],
           {'reading': captured8[0]['reading'] if captured8 else None, 'events': ev})
    lost_pattern = report.read_command(f"pkill -f 'Cookie: sid={V8}'", _redact_scrub(f"pkill -f 'Cookie: sid={V8}'"))
    run18b = run4([dict(K4(f"pkill -f 'Cookie: <redacted>'"), reading=lost_pattern)], [])
    ev, rv, _ = report.sibling_losses(run18b)
    expect('unit/pattern-lost-to-redaction-is-replaced-and-review-only', lost_pattern['uncertain'] and V8 not in json.dumps(lost_pattern)
           and not ev and any(r['pid'] == 123 and 'kill_by' in r for r in rv), {'reading': lost_pattern, 'events': ev, 'review': rv})
    # Round 8 item 3: a plain `kill 123` (no -9) by another session gates event (g), and the ledger shows it;
    # a command the rule ignores is still listed, with its reason.
    plain8 = {'start_m': 20.5, 'end_m': 20.6, 'is_error': False, 'kinds': ['kill'], 'command': 'kill 123', 'line': 4}
    other8 = {'start_m': 20.5, 'end_m': 20.6, 'is_error': False, 'kinds': ['kill'], 'command': 'kill 999', 'line': 5}
    run38 = run4([plain8, other8], [])
    ev, rv, _ = report.sibling_losses(run38)
    rows38 = report.ledger_rows(run38, 'p0.a1.stack/', 'R', {})
    reasons38 = {c['command']: c['reason'] for c in rows38[0]['candidates']}
    expect('unit/plain-kill-by-another-session-gates-and-ledger-lists-every-candidate',
           len(ev) == 1 and ev[0]['culprits'] == ['b'] and rows38[0]['disposition'] == 'blamed'
           and reasons38.get('kill 123', '').startswith('counted') and reasons38.get('kill 999', '').startswith('not counted'),
           {'events': ev, 'candidates': reasons38})
    # Round 8 item 2: one inventory, every phase, the main checkout included; between-teardowns has the gap's bounds.
    S8 = lambda group, pid, last, gone: {'group': group, 'pid': pid, 'started': stamp(T0 + 1), 'command': f'/tmp/avr1/{group}/.venv/bin/python worker.py',
                                         'kind': 'worker', 'ports': [], 'last_seen_m': last, 'gone_m': gone}
    W8 = [{'task': 'invoice-notes', 'group': 'wt-notes', 'start_m': 30.0, 'end_m': 40.0}, {'task': 'pay-receipts', 'group': 'wt-pay', 'start_m': 45.0, 'end_m': 55.0}]
    run28 = {'clock': {'started_wall': T0}, 'episode_end_m': 20.0, 'teardown_windows': W8,
             'sampler': {'durable_losses': [S8('wt-cust', 601, 21.0, 22.0), S8('wt-cust', 602, 25.0, 26.0), S8('wt-void', 603, 60.0, 61.0),
                                            S8('main', 604, 10.0, 11.0), S8('wt-void', 605, 62.0, 63.0)]},
             'teardown': {'invoice-notes': {'listing_lost': [], 'lost_between_all': []},
                          'pay-receipts': {'listing_lost': [], 'lost_between_all': [dict(S8('wt-list', 606, 0, 0), started=stamp(T0 + 2))]},
                          'void-invoices': {'harness_reclaim': {'terminated': [605]}}},
             'agents': {'b': {'marker': 'wt-b', 'launched_m': 0, 'ended_m': 19, 'service_commands': [dict(other8, start_m=18.0, end_m=18.1)]}}}
    rows28 = {r['pid']: r for r in report.ledger(run28)}
    expect('unit/ledger-inventory-covers-every-phase-and-the-main-checkout',
           set(rows28) == {601, 602, 603, 604, 605, 606} and len(rows28) == report.inventory_size(run28)
           and rows28[601]['phase'] == 'gap-before-teardown' and rows28[602]['phase'] == 'gap-before-teardown'
           and rows28[603]['phase'] == 'removal-and-after' and rows28[603]['disposition'] == 'own-teardown'
           and rows28[604]['disposition'] == 'main-checkout' and rows28[605]['disposition'] == 'harness-stop'
           and rows28[606]['phase'] == 'between-teardowns' and rows28[606]['lost_between_m'] == [40.0, 45.0]
           and any(c['command'] == 'kill 999' for c in rows28[601]['candidates']),
           {p: {k: r.get(k) for k in ('phase', 'disposition', 'lost_between_m')} for p, r in rows28.items()})
    post8 = {'clock': {'started_wall': T0}, 'episode_end_m': 20.0, 'sampler': {'durable_losses': [S8('wt-cust', 607, 21.0, 22.0)]}, 'agents': {}}
    expect('unit/post-episode-loss-without-teardown-windows-has-a-row', [r['phase'] for r in report.ledger(post8)] == ['gap-before-teardown'],
           report.ledger(post8))
    # Round 8 item 5: duplicate decisions and duplicate ledger ids are refused.
    refused8 = []
    with tempfile.TemporaryDirectory(prefix='avu-', dir='/tmp') as d85:
        (Path(d85) / 'adjudication.json').write_text('{"x": {"decision": "review", "reason": "a"}, "x": {"decision": "review", "reason": "b"}}')
        try:
            report.load_adjudication(d85)
            refused8.append(False)
        except ValueError:
            refused8.append(True)
    twins = {'clock': {'started_wall': T0}, 'episode_end_m': 100, 'agents': {},
             'sampler': {'durable_losses': [dict(S8('wt-a', 700, 10, 11), started='odd start'), dict(S8('wt-a', 700, 12, 13), started='odd  start')]}}
    try:
        report.ledger_rows(twins, 'p0.a1.stack/', 'R', {})
        refused8.append(False)
    except ValueError:
        refused8.append(True)
    expect('unit/duplicate-decision-and-duplicate-ledger-id-refused', refused8 == [True, True], refused8)
    # Round 8 item 4: an override never changes the automatic outcome (service-kill, the victim's isolation).
    src8 = sorted(Path(ROOT / 'results').glob('validation-v9-wip-final/stack-service-kill-restarts/run.json'))
    if not src8:
        expect('unit/override-leaves-every-automatic-figure-unchanged', False, 'needs results/validation-v9-wip-final/stack-service-kill-restarts/run.json')
    else:
        rec8 = json.loads(src8[0].read_text())
        rec8['_attempt'] = 1
        plan8 = {'pairs': [{'index': 0, 'scenario': 'service-kill', 'victim': 'void-invoices', 'order': ['stack']}], 'config': plan['config']}
        rows8 = report.ledger_rows(rec8, 'p0.a1.stack/', 'R', {})
        target = next((r for r in rows8 if r['phase'] == 'episode' and r['worktree'] != 'main'), None)
        auto_keys = ('others_untouched_automatic', 'episode_success_automatic', 'victim_copes_automatic', 'victim_gated_automatic')
        with tempfile.TemporaryDirectory(prefix='avu-', dir='/tmp') as d84, tempfile.TemporaryDirectory(prefix='avu-', dir='/tmp') as d84b:
            slots_a, runs_a, _ = report.rows_v2(plan8, {(0, 'stack'): copy.deepcopy(rec8)}, d84)
            (Path(d84b) / 'adjudication.json').write_text(json.dumps({target['id']: {'decision': 'blamed:void-invoices', 'reason': 'stand-in override'}}))
            slots_b, runs_b, _ = report.rows_v2(plan8, {(0, 'stack'): copy.deepcopy(rec8)}, d84b)
        va = next(r for r in slots_a if r['task'] == 'void-invoices')
        vb = next(r for r in slots_b if r['task'] == 'void-invoices')
        same_auto = all(runs_a[0][k] == runs_b[0][k] for k in auto_keys) and all(
            a[k] == b[k] for a, b in zip(slots_a, slots_b) for k in ('isolation_ok_automatic', 'gated_success_automatic'))
        expect('unit/override-leaves-every-automatic-figure-unchanged',
               target is not None and same_auto and va['isolation_ok'] and not vb['isolation_ok'] and runs_b[0]['overrides'],
               {'override': target and target['id'], 'automatic': {k: (runs_a[0][k], runs_b[0][k]) for k in auto_keys},
                'victim_isolation': (va['isolation_ok'], vb['isolation_ok'])})
    # Round 4 item 7: suspension measured from the two clocks; AC power before each arm.
    expect('unit/suspension-0.4s-passes-0.6s-invalidates-wall-step-irrelevant',
           runner.suspension((100.0, 200.0), (110.0, 210.4))[1] is False and runner.suspension((100.0, 200.0), (110.0, 210.6))[1] is True
           and runner.suspension((100.0, 200.0), (116.0, 216.0))[1] is False,
           [runner.suspension((100.0, 200.0), (110.0, 210.4)), runner.suspension((100.0, 200.0), (110.0, 210.6))])
    expect('unit/power-check-refuses-battery-and-unreadable', runner.power_check(lambda: 'Battery Power')[0] is False
           and runner.power_check(lambda: None)[0] is False and runner.power_check(lambda: 'AC Power')[0] is True,
           [runner.power_check(lambda: 'Battery Power'), runner.power_check(lambda: None)])
    # Round 4 item 9: PostgreSQL is never started or stopped on a directory outside the run's root.
    with tempfile.TemporaryDirectory(prefix='avu-', dir='/tmp') as tmp:
        run_root, outside = Path(tmp) / 'root', Path(tmp) / 'elsewhere' / 'pgdata'
        run_root.mkdir()
        outside.mkdir(parents=True)
        (outside / 'PG_VERSION').write_text('17\n')
        result = runner.read_migrations(outside, Path(tmp), 'x', root=run_root)
        expect('unit/data-directory-outside-the-run-root-recorded-never-touched', result.get('outside_run_root') and result.get('touched') is False
               and not (outside / 'postmaster.pid').exists(), result)
    # Round 4 item 10: recorded lineage seeds the final discovery.
    with tempfile.TemporaryDirectory(prefix='avr-unit-', dir='/tmp') as root10:
        child = subprocess.Popen(['sleep', '30'], cwd='/', start_new_session=True, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        try:
            ident = observe.legacy.process_identity(child.pid)
            observe.LINEAGE['launched'].add((child.pid, ident['started']))
            found = [p['pid'] for p in observe.run_processes(root10)]
        finally:
            observe.LINEAGE['launched'].clear()
            child.kill()
            child.wait()
        expect('unit/recorded-lineage-seeds-final-discovery', child.pid in found, {'child': child.pid, 'found': found})
    # Round 5 item 2: placement (working directory inside the root, started after the run began) ties a
    # clean-environment process; outside the root, or started before the run, it does not.
    with tempfile.TemporaryDirectory(prefix='avr-unit-', dir='/tmp') as root2:
        inside = Path(root2) / 'wt-x'
        inside.mkdir()
        began = time.time()
        procs = [subprocess.Popen(['/usr/bin/env', '-i', '/bin/sleep', '30'], cwd=cwd, start_new_session=True,
                                  stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL) for cwd in (inside, '/')]
        time.sleep(0.5)
        try:
            observe.RUN_SUFFIX = '.unit-test-run'
            rows2 = [{'pid': p.pid, **observe.legacy.process_identity(p.pid)} for p in procs]
            observe.RUN_STARTED_WALL = observe.started_wall(rows2[0]['started']) + 0.9   # same second as the process: after
            same_second = observe.tie(rows2[0], root2)
            observe.RUN_STARTED_WALL = began
            ties2 = [observe.tie(r, root2) for r in rows2]
            observe.RUN_STARTED_WALL = time.time() + 5   # the process started before the run
            before = observe.tie(rows2[0], root2)
            observe.RUN_STARTED_WALL = None              # no run: placement never applies
            no_run = observe.tie(rows2[0], root2)
        finally:
            observe.RUN_SUFFIX = None
            observe.RUN_STARTED_WALL = None
            for p in procs:
                p.kill()
                p.wait()
        expect('unit/placement-ties-a-clean-environment-process-in-the-root-only',
               ties2 == ['placement', None] and same_second == 'placement' and before is None and no_run is None,
               {'inside_and_outside': ties2, 'same_second': same_second, 'started_before_run': before, 'no_run': no_run})
    # Round 5 item 2: a process with one worktree's marker is attributed to that worktree only.
    observe.RUN_SUFFIX = '.unit-test-run'
    try:
        owner_b_only = observe.owner_of(1, 'tail -f /tmp/avr-x/wt-b/README.md', {1: 'wt-a.unit-test-run'}, {}, {'wt-b': ['/tmp/avr-x/wt-b']})
        owner_all = observe.owner_of(1, 'tail -f /tmp/avr-x/wt-b/README.md', {1: 'wt-a.unit-test-run'}, {},
                                     {'wt-b': ['/tmp/avr-x/wt-b'], 'wt-a': ['/tmp/avr-x/wt-a']})
    finally:
        observe.RUN_SUFFIX = None
    expect('unit/marked-process-attributed-to-its-marker-only', owner_b_only is None and owner_all == 'wt-a',
           {'asked_for_b_only': owner_b_only, 'all_groups': owner_all})
    # Round 6 item 1: with no readable marker, the working directory decides before any open file.
    p6 = observe.Paths({1: {'/tmp/avr-x/wt-a', '/tmp/avr-x/wt-b/README.md'}, 2: {'/', '/tmp/avr-x/wt-b/README.md'}})
    p6.cwd = {1: '/tmp/avr-x/wt-a', 2: '/'}
    g6 = {'wt-b': ['/tmp/avr-x/wt-b'], 'wt-a': ['/tmp/avr-x/wt-a']}   # B searched first
    o6 = [observe.owner_of(1, 'python3 x.py', {}, p6, g6), observe.owner_of(2, 'python3 x.py', {}, p6, g6)]
    expect('unit/markerless-process-attributed-by-working-directory-before-open-files', o6 == ['wt-a', 'wt-b'], o6)
    # Round 5 item 5: a power refusal never consumes a rerun or strands a pair (planted power reader,
    # a stand-in arm runner; the validation already holds the host lock).
    if plan_path:
        import contextlib
        import io
        import types
        with tempfile.TemporaryDirectory(prefix='avu-', dir='/tmp') as d5:
            results5 = Path(d5)
            pair5 = plan['pairs'][0]
            def fake_arm(the_plan, index, arm_name, out_dir):
                rec5 = {'protocol_version': 2, 'pair': index, 'attempt': the_plan.get('_attempt', 1), 'arm': arm_name,
                        'scenario': pair5.get('scenario'), 'victim': pair5.get('victim'), 'plan_sha256': the_plan['_sha256'],
                        'plan_kind': the_plan['kind'], 'model': the_plan['config']['model'], 'valid': True, 'invalid_reasons': [],
                        'agents': {'s': {'trace_metrics': {'result': {'total_cost_usd': 1.0}}}}}
                Path(out_dir).mkdir(parents=True)
                (Path(out_dir) / 'run.json').write_text(json.dumps(rec5))
                return rec5
            readings = lambda *values: (lambda it: (lambda: next(it)))(iter(values))
            args5 = lambda attempt: types.SimpleNamespace(plan=str(plan_path), index=0, attempt=attempt, results=str(results5))
            def refused(attempt, reader):
                try:
                    with contextlib.redirect_stdout(io.StringIO()):   # its progress lines are not part of the validation log
                        runner.cmd_pair(args5(attempt), power_reader=reader, arm_runner=fake_arm, lock_held=True)
                except SystemExit as stop:
                    return str(stop)
                return None
            marker5 = results5 / 'pair0.2-STARTED.json'
            first_refusal = refused(1, readings('Battery Power'))
            nothing_written = not any(results5.iterdir())
            second_refusal = refused(1, readings('AC Power', 'Battery Power'))
            second = results5 / runner.run_dir_name(0, 1, pair5['order'][1])
            rec2 = json.loads((second / 'run.json').read_text()) if (second / 'run.json').exists() else {}
            reasons5 = runner.attempt_reasons(results5, 0, plan['_sha256'])
            retry_refused_on_battery = refused(2, readings('Battery Power'))
            marker_after_refusal = marker5.exists()
            retry = refused(2, readings('AC Power', 'AC Power'))
            marker_after_retry = marker5.exists()
            runs5, foreign5, attempts5, every5, governing5, _ = report.load_runs_v2(plan, str(results5), plan['_sha256'])
            sessions5 = report.all_sessions(every5)
        expect('unit/power-refusal-before-the-marker-leaves-nothing', bool(first_refusal) and nothing_written,
               {'refusal': first_refusal, 'written': nothing_written})
        expect('unit/second-arm-power-refusal-records-never-started-invalid-run',
               bool(second_refusal) and rec2.get('never_started') is True and rec2.get('valid') is False
               and rec2.get('invalid_reasons', [''])[0].startswith('not started: ') and not rec2.get('agents') and bool(reasons5),
               {'refusal': second_refusal, 'record': rec2, 'attempt_reasons': reasons5})
        expect('unit/attempt-2-refused-on-battery-leaves-no-marker-then-allowed',
               bool(retry_refused_on_battery) and not marker_after_refusal and retry is None and marker_after_retry
               and governing5.get(0) == 2 and all(runs5.get((0, a)) for a in pair5['order']),
               {'battery_refusal': retry_refused_on_battery, 'marker_after_refusal': marker_after_refusal, 'retry': retry,
                'governing': governing5})
        listing5 = attempts5.get(0) or []
        expect('unit/never-started-record-is-no-session-and-shown-superseded',
               len(sessions5) == 3 and any(a['attempt'] == 1 and not a['used'] and 'never started' in (a['excluded_because'] or '') for a in listing5),
               {'sessions': sessions5, 'attempts': listing5})
        # Round 6 item 5: a signal held during a valid first arm's cleanup stops `pair` between the arms;
        # the missing arm is recorded as never started, so attempt 2 is allowed.
        with tempfile.TemporaryDirectory(prefix='avu-', dir='/tmp') as d6:
            results5 = Path(d6)
            def signalled_arm(the_plan, index, arm_name, out_dir):
                rec6 = fake_arm(the_plan, index, arm_name, out_dir)
                runner.INTERRUPTED['signal'] = signal.SIGTERM   # as the cleanup's hold-off handler records it
                return rec6
            try:
                with contextlib.redirect_stdout(io.StringIO()):
                    runner.cmd_pair(args5(1), power_reader=readings('AC Power'), arm_runner=signalled_arm, lock_held=True)
                stop6 = None
            except SystemExit as stop:
                stop6 = str(stop)
            finally:
                runner.INTERRUPTED['signal'] = None
            missing6 = results5 / runner.run_dir_name(0, 1, pair5['order'][1]) / 'run.json'
            rec6 = json.loads(missing6.read_text()) if missing6.exists() else {}
            reasons6 = runner.attempt_reasons(results5, 0, plan['_sha256'])
            retry6 = refused(2, readings('AC Power', 'AC Power'))
        expect('unit/signal-between-the-arms-records-never-started-then-attempt-2-allowed',
               bool(stop6) and rec6.get('never_started') is True and rec6.get('invalid_reasons') == [f'not started: interrupted by signal {int(signal.SIGTERM)}']
               and bool(reasons6) and retry6 is None,
               {'stop': stop6, 'record': rec6, 'attempt_reasons': reasons6, 'retry': retry6})
    # Round 7 item 6: an interrupt of the validation is forwarded to a nested process, which is waited for.
    with tempfile.TemporaryDirectory(prefix='avu-', dir='/tmp') as d76:
        ready, got = Path(d76) / 'ready', Path(d76) / 'got-signal'
        child = subprocess.Popen([sys.executable, '-c', 'import signal, sys, time, pathlib\n'
                                  'def stop(n, f):\n    time.sleep(1); pathlib.Path(sys.argv[2]).write_text(str(n)); sys.exit(0)\n'
                                  'signal.signal(signal.SIGTERM, stop)\npathlib.Path(sys.argv[1]).write_text("ok")\ntime.sleep(60)\n',
                                  str(ready), str(got)], start_new_session=True)
        deadline = time.monotonic() + 10
        while not ready.exists() and time.monotonic() < deadline:
            time.sleep(0.05)
        def alarm(signum, frame):
            raise KeyboardInterrupt('stand-in for an outside SIGTERM')
        previous = signal.signal(signal.SIGALRM, alarm)
        stopped = False
        try:
            signal.setitimer(signal.ITIMER_REAL, 0.3)
            with forwarding(child):
                child.wait(30)
        except KeyboardInterrupt:
            stopped = True
        finally:
            signal.setitimer(signal.ITIMER_REAL, 0)
            signal.signal(signal.SIGALRM, previous)
            if child.poll() is None:
                child.kill()
                child.wait()
        forwarded = got.read_text() if got.exists() else None
        exited = child.returncode
    expect('unit/interrupted-validation-forwards-to-its-nested-process-and-waits', stopped and forwarded == str(int(signal.SIGTERM)) and exited == 0,
           {'stopped': stopped, 'signal_seen_by_child': forwarded, 'child_exit': exited})
    # Round 4 item 11: a rerun marker counts only if its plan digest, pair and attempt match.
    with tempfile.TemporaryDirectory(prefix='avu-', dir='/tmp') as d11:
        marker = Path(d11) / 'pair0.2-STARTED.json'
        marker.write_text(json.dumps({'pair': 1, 'attempt': 2, 'plan_sha256': 'x'}))
        expect('unit/rerun-marker-of-another-plan-or-pair-is-refused-and-ignored',
               not runner.marker_matches(marker, 'x', 0) and not runner.marker_matches(marker, 'y', 1) and runner.marker_matches(marker, 'x', 1),
               'marker_matches')
    # Teardown collateral: a recent own command that did not target the lost process excuses nothing.
    coll = {'teardown': {'invoice-notes': {'collateral': [{'group': 'wt-void', 'pid': 777, 'started': 's',
                                                           'command': '/tmp/avr1/wt-void/.venv/bin/python worker.py'}]}},
            'episode_end_m': 100, 'bystander': {},
            'agents': {'v': {'marker': 'wt-void', 'service_commands': [
                {'start_m': 90, 'end_m': 91, 'kinds': ['down_restart'], 'kill_like': False, 'command': 'stack restart api'}]}}}
    ev, rv = report.teardown_collateral(coll)
    expect('unit/slow-exit-excuse-needs-a-command-targeting-that-process', len(ev) == 1 and not rv, {'events': ev, 'review': rv})
    coll['agents']['v']['service_commands'][0]['command'] = 'stack restart worker'
    ev, rv = report.teardown_collateral(coll)
    expect('unit/slow-exit-excuse-applies-to-its-own-targeted-service', not ev and len(rv) == 1, {'events': ev, 'review': rv})
    # Publishing never writes through a link or over a file of the submission.
    with tempfile.TemporaryDirectory(prefix='avu-', dir='/tmp') as tmp:
        tree, outside, dest = Path(tmp) / 'tree', Path(tmp) / 'outside.txt', Path(tmp) / 'results'
        tree.mkdir()
        outside.write_text('outside the run, must stay\n')
        (tree / 'app.py').write_text('SECRET_' + 'TOKEN=' + 'abcdefghijklmnopqrstuvwx\n')   # split: the commit scan would refuse the whole literal
        os.symlink(outside, tree / '.app.py.scrubbed')
        (tree / '.app.py.publishing').write_text('a file of the submission, must stay\n')
        left_out = []
        (Path(tmp) / 'scrub').mkdir()
        runner.publish_tree(tree, dest, left_out, scratch=Path(tmp) / 'scrub')
        expect('unit/publishing-never-follows-a-link-or-overwrites-a-submission-file',
               outside.read_text() == 'outside the run, must stay\n' and (tree / '.app.py.publishing').read_text().startswith('a file of the submission')
               and (tree / '.app.py.scrubbed').is_symlink() and 'abcdefghij' not in (dest / 'app.py').read_text()
               and any(x['reason'].startswith('symbolic link') for x in left_out),
               {'left_out': left_out, 'published': sorted(str(x.relative_to(dest)) for x in dest.rglob('*'))})
    import redact as _redact
    text = _redact.scrub('curl -H "Authorization: Bearer abcdefghijklmnop12345" -H "authorization: Basic dXNlcjpwYXNz" api; token Bearer zyxwvutsrq987')
    expect('unit/redactor-authorization-headers-and-bearer-tokens', 'abcdefghijklmnop12345' not in text and 'dXNlcjpwYXNz' not in text
           and 'zyxwvutsrq987' not in text, text)
    # Round 5 item 1: a header value redacted only in part is redacted in full (bare, -H and JSON forms).
    A5, V5 = 'Author' + 'ization', 'SYNTH' + 'ETIC-r5-response'   # split: the commit scan would refuse the whole literal
    mixed = [f'{A5}: <redacted>, response="{V5}"', f"curl -H '{A5}: <redacted>, response=\"{V5}\"' x",
             f'curl -H "{A5}: <redacted>, response=\\"{V5}\\"" x', json.dumps({A5: f'<redacted>, response="{V5}"'}),
             json.dumps({'command': f'echo {A5}: <redacted>, response="{V5}"'})]
    outs = [_redact.scrub(m) for m in mixed] + [_redact.scrub_text(mixed[4])]
    expect('unit/partly-redacted-header-is-redacted-in-full', not any(V5 in o for o in outs) and all('<redacted>' in o for o in outs), outs)
    # Round 7 item 1: no guess from quote parity; a quoted --header argument is redacted inside its quotes only.
    plain7 = f'debug: read 5" buffer; {A5}: Digest username="local", response="{V5}"'
    curl7 = "curl --header 'X-API-" + f"Key: {V5}' http://local; kill -9 123"
    outs7 = [_redact.scrub(plain7), _redact.scrub_text(plain7), _redact.scrub(curl7), _redact.scrub(curl7.replace("--header '", "--header='")),
             _redact.scrub(curl7.replace("'", '"'))]
    expect('unit/header-redaction-without-quote-guessing-and-header-argument-in-its-quotes',
           not any(V5 in o for o in outs7) and all(o.endswith('; kill -9 123') for o in outs7[2:]), outs7)
    # Round 6 item 2: a header's extent is decided on its own line, whatever other lines look like.
    multi = 'echo {\\"a\\": 1}\n' + f'{A5}: Digest username="u", realm="r", response="{V5}", qop=auth\nnext line'
    outs6 = [_redact.scrub(multi), _redact.scrub_text(multi), _redact.scrub(json.dumps({'content': multi}))]
    expect('unit/digest-line-after-a-line-with-an-escaped-quote-redacted-to-its-end', not any(V5 in o or 'qop' in o for o in outs6)
           and all('next line' in o for o in outs6), outs6)
    # Report on synthetic records: assignments from the plan, attempts, trial-plan shape.
    src_path = sorted(Path(ROOT / 'results').glob('validation-v3-wip-final/stack-agent-kill-recovers/run.json'))
    if not src_path:
        expect('unit/synthetic-report-records-available', False, 'needs results/validation-v3-wip-final/stack-agent-kill-recovers/run.json')
        return
    src = json.loads(src_path[0].read_text())
    one = dict(plan, kind='harness_selftest', protocol_version=2,
               pairs=[{'index': 0, 'scenario': 'agent-kill', 'victim': 'invoice-list', 'order': ['stack', 'baseline']}])
    def make(arm, attempt=1, valid=True, missed=False):
        r = copy.deepcopy(src)
        r.update(pair=0, arm=arm, attempt=attempt, valid=valid, plan_kind='harness_selftest', plan_sha256=None)
        if not valid:
            r['invalid_reasons'] = ['synthetic']
        if missed:
            r['fault'] = {**r['fault'], 'injected': False, 'missed': 'synthetic'}
        return r
    summ = report.summarize_v2(one, {(0, 'stack'): make('stack', valid=False), (0, 'baseline'): make('baseline')}, '/x')
    g = summ['scenarios']['agent-kill']['arms']['stack']['gated_success']
    expect('unit/report-invalid-first-arm-shows-0-scored-of-5-assigned', (g['scored'], g['assigned']) == (0, 5), g)
    with tempfile.TemporaryDirectory(prefix='avu-', dir='/tmp') as d:
        expect('unit/attempt-2-refused-without-a-first-attempt-failure-record', runner.attempt_reasons(d, 0, None) == [], 'empty results directory')
        for name, rec in [('pair0-stack', make('stack')), ('pair0-baseline', make('baseline', missed=True)), ('pair0.2-stack', make('stack', attempt=2))]:
            (Path(d) / name).mkdir()
            (Path(d) / name / 'run.json').write_text(json.dumps(rec))
        runs, _, attempts, every, _, _ = report.load_runs_v2(one, d)
        expect('unit/incomplete-attempt-2-leaves-the-pair-missing', runs[(0, 'stack')] is None and runs[(0, 'baseline')] is None and len(every) == 3,
               attempts)
    with tempfile.TemporaryDirectory(prefix='avu-', dir='/tmp') as d:
        for name, rec in [('pair0-stack', make('stack')), ('pair0-baseline', make('baseline', missed=True))]:
            (Path(d) / name).mkdir()
            (Path(d) / name / 'run.json').write_text(json.dumps(rec))
        (Path(d) / 'pair0.2-STARTED.json').write_text(json.dumps({'pair': 0, 'attempt': 2}))
        (Path(d) / 'pair0.2-stack' / 'agents' / 'invoice-list').mkdir(parents=True)   # launched, then died without a record
        runs, _, attempts, every, governing, partial = report.load_runs_v2(one, d)
        summ = report.summarize_v2(one, runs, d, {}, attempts, every, governing, partial)
        missing = [r for r in summ['rows'] if r['status'] == 'not_run']
        expect('unit/started-marker-makes-attempt-2-govern-without-a-record', runs[(0, 'stack')] is None and governing.get(0) == 2
               and missing and all(r['attempt'] == 2 for r in missing) and any(p['run'] == 'pair0.2-stack' for p in partial)
               and summ['cost']['sessions_launched'] == sum(len(r.get('agents') or {}) for r in every) + len(partial),
               {'governing': governing, 'partial': partial, 'cost': summ['cost']})
    shaped = copy.deepcopy(plan)
    shaped.update(kind='agent_experiment', pairs=runner.plan_pairs(random.Random(shaped['seed']), 1, shaped['config']['tasks']))
    shaped['config'] = dict(shaped['config'], pairs_per_scenario=1, faults=True, agent_seconds=1800)   # the self-test plan runs 600 s
    base_problems = runner.protocol_problems(shaped)
    short = copy.deepcopy(shaped)
    short['config']['agent_seconds'] = 1
    edited = copy.deepcopy(shaped)
    edited['pairs'][0]['victim'] = next(t for t in shaped['config']['tasks'] if t != shaped['pairs'][0]['victim'])
    two = copy.deepcopy(shaped)
    two['config']['pairs_per_scenario'] = 2
    two['pairs'] = runner.plan_pairs(random.Random(two['seed']), 2, two['config']['tasks'])
    half = copy.deepcopy(shaped)
    half['config']['sample_interval'] = 0.5
    expect('unit/trial-plan-needs-1-or-5-pairs-per-scenario-and-1s-samples', runner.protocol_problems(two) and runner.protocol_problems(half),
           {'two_pairs': runner.protocol_problems(two), 'half_second': runner.protocol_problems(half)})
    expect('unit/trial-plan-with-agent-seconds-1-or-edited-pairs-refused', not base_problems and runner.protocol_problems(short)
           and runner.protocol_problems(edited), {'as_generated': base_problems, 'agent_seconds_1': runner.protocol_problems(short),
                                                  'edited_pairs': runner.protocol_problems(edited)})


def commit_scan_case():
    """The commit scan on staged files with a quoted path, a binary file, a rename, a symbolic link
    and the commit message, each carrying a sentinel, plus a credential-shaped value: every one is
    reported, the value is never printed and nothing is committed. A clean commit goes through."""
    import secrets
    sentinel = 'sv' + secrets.token_hex(12)
    nonbmp = 'nb' + secrets.token_hex(6) + '\U0001F600' + secrets.token_hex(4)   # a host value with a non-BMP character
    env = {**os.environ, 'AGENTVALUE_TEST_SENTINEL': sentinel, 'AGENTVALUE_TEST_NONBMP': nonbmp}
    plain = ['git', '-c', 'user.name=t', '-c', 'user.email=t@example.invalid', '-c', 'commit.gpgsign=false', '-c', 'core.hooksPath=/dev/null']
    with tempfile.TemporaryDirectory(prefix='avc-', dir='/tmp') as tmp:
        repo = Path(tmp) / 'repo'
        repo.mkdir()
        run = lambda *a: subprocess.run([*plain, *a], cwd=repo, capture_output=True, check=True)
        run('init', '-q')
        (repo / 'old-name.txt').write_text(f'committed earlier, renamed now: {sentinel}\n')
        run('add', '-A')
        run('commit', '-qm', 'base')
        head = run('rev-parse', 'HEAD').stdout
        run('mv', 'old-name.txt', 'new-name.txt')
        quoted = 'quoted "name"\twith tab é.txt'
        (repo / quoted).write_text(f'x\n{sentinel}\n')
        (repo / 'blob.bin').write_bytes(b'\x00\x01\x02' + sentinel.encode() + b'\x00\xff')
        os.symlink(f'target-{sentinel}', repo / 'link')
        (repo / 'config.env').write_text('SENTINEL_TOKEN=' + secrets.token_hex(16) + '\n')
        (repo / 'a1').write_text('nothing here\n')
        (repo / 'a[1]').write_text(f'bracketed path {sentinel}\n')
        (repo / f'name-{sentinel}.txt').write_text('a clean file with a secret name\n')
        (repo / 'nonbmp.json').write_text(json.dumps({'v': nonbmp}) + '\n')   # json.dumps writes a surrogate pair
        (repo / 'escaped.json').write_text('{"v": "' + '\\u%04x' % ord(sentinel[0]) + sentinel[1:] + '"}\n')   # first character JSON-escaped
        run('add', '-A')
        r = subprocess.run([sys.executable, str(HERE / 'commit.py'), '--repo', str(repo), '-m', f'message with {sentinel}'],
                           capture_output=True, text=True, env=env)
        report_text = r.stdout + r.stderr
        expected = {'quoted path': repr(quoted), 'binary': "'blob.bin'", 'rename': "'new-name.txt'", 'symlink': "'link'",
                    'commit message': '(commit message)', 'credential pattern': "'config.env'", 'bracketed path a[1]': "'a[1]'",
                    'masked path name': 'in the path name', 'json-escaped value': "'escaped.json'", 'surrogate pair (json.dumps)': "'nonbmp.json'"}
        found = {k: v in report_text for k, v in expected.items()}
        unchanged = run('rev-parse', 'HEAD').stdout == head
        refused = r.returncode == 1 and all(found.values()) and sentinel not in report_text and nonbmp not in report_text and unchanged and "'a1'" not in report_text
        clean = Path(tmp) / 'clean'
        clean.mkdir()
        subprocess.run([*plain, 'init', '-q'], cwd=clean, check=True, capture_output=True)
        (clean / 'README.txt').write_text('nothing secret here\n')
        subprocess.run(['git', 'add', '-A'], cwd=clean, check=True)
        # A hook that would add a file between the scan and the commit: commit.py runs no hooks.
        hook = clean / '.git' / 'hooks' / 'pre-commit'
        hook.write_text(f'#!/bin/sh\necho "{sentinel}" > sneaked.txt\ngit add sneaked.txt\n')
        hook.chmod(0o755)
        c = subprocess.run([sys.executable, str(HERE / 'commit.py'), '--repo', str(clean), '-m', 'clean commit'], capture_output=True, text=True,
                           env={**env, 'GIT_AUTHOR_NAME': 't', 'GIT_AUTHOR_EMAIL': 't@example.invalid', 'GIT_COMMITTER_NAME': 't',
                                'GIT_COMMITTER_EMAIL': 't@example.invalid'})
        tree_files = subprocess.run(['git', 'ls-tree', '-r', '--name-only', 'HEAD'], cwd=clean, capture_output=True, text=True).stdout.split()
        committed = c.returncode == 0 and tree_files == ['README.txt'] and 'root-commit' in c.stdout
    return refused, committed, {'exit': r.returncode, 'found': found, 'value_printed': sentinel in report_text, 'head_unchanged': unchanged,
                                'clean_commit_exit': c.returncode, 'clean_stderr': c.stderr[-300:], 'clean_stdout': c.stdout[-200:],
                                'committed_files': tree_files}


def trace_times_ok(agent_dir):
    """Every trace line has an arrival time, in order, and the trace is untouched in line count."""
    trace = (agent_dir / 'trace.jsonl').read_text().split('\n')
    lines = [l for l in trace if l]
    rows = (agent_dir / runner.ARRIVALS).read_text().splitlines()[1:]
    times = [float(r.split('\t')[1]) for r in rows]
    return len(rows) == len(lines) and times == sorted(times) and all(t >= 0 for t in times)


def machine_ok(rec):
    m = rec.get('machine') or {}
    return all(m.get(k) is not None for k in ('cores', 'memory_bytes', 'load_1m_before_bystander', 'load_1m_after_last_agent',
                                              'concurrent_sessions')) and m['session_budget']['seconds'] and m['session_budget']['usd']


def no_environment(run_dir):
    """No retained file holds a host environment value outside the allowlist (the same check as scan_staged.py)."""
    import scan_staged
    return not scan_staged.scan_directory(run_dir)


if __name__ == '__main__':
    main()
