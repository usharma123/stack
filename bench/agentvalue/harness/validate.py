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
                     ('limit not range-checked', _list_limit_unchecked, 'bad_params_400'),
                     ('items in descending id order', _list_descending, 'list_all_ascending')],
    'void-invoices': [('void does not invalidate the cache', _void_no_invalidate, 'cache_after_void'),
                      ('reads bypass the cache entirely', _void_bypass_cache, 'cache_after_void'),
                      ('constraint dropped and not re-added', _void_constraint_dropped, 'constraint_allows_void_only'),
                      ('constraint also allows another status (refunded)', _void_allows_refunded, 'constraint_allows_void_only'),
                      ('PATCH on a void invoice allowed', _void_patch_allowed, 'patch_void_409'),
                      ('a paid invoice is voided', _void_paid_changed, 'void_paid_409'),
                      ('a void invoice can be voided again', _void_twice_ok, 'void_twice_409')],
}

# Correct alternatives to the reference: each must pass.
ALTERNATIVES = {'void-invoices': [('a void invoice is never cached', _void_never_caches_void)]}


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
    'agent-kill-recovers': ({'invoice-list': 'slow-stall', 'void-invoices': 'stray'}, ('agent-kill', 'invoice-list')),
    'agent-kill-recovery-leaves': ({'pay-receipts': 'stall', 'pay-receipts.recovery': 'leave-running', 'customer-table': 'target',
                                    'invoice-notes': 'killer:wt-cust', 'invoice-list': 'sentinel'}, ('agent-kill', 'pay-receipts')),
    'service-kill-restarts': ({'void-invoices': 'sk-restart', 'invoice-list': 'exec-left'}, ('service-kill', 'void-invoices')),
    'service-kill-no-reaction': ({'customer-table': 'sk-noreact'}, ('service-kill', 'customer-table')),
    'service-kill-never-restores': ({'invoice-notes': 'sk-finish'}, ('service-kill', 'invoice-notes')),
    'event-g-identification': ({'invoice-notes': 'wrong-pid-killer:wt-pay', 'pay-receipts': 'self-kill',
                                'customer-table': 'pid-killer:wt-list', 'invoice-list': 'up-target'}, None),
    'version1-planted-and-collateral': ({'invoice-notes': 'leave-running', 'pay-receipts': 'contaminate', 'customer-table': 'pkill',
                                         'void-invoices': 'leave-running'}, None),
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
    needle = value.encode()
    return [str(p.relative_to(directory)) for p in Path(directory).rglob('*') if p.is_file() and not p.is_symlink() and needle in p.read_bytes()]


def four_services(rec):
    """The trigger saw all four services of the victim's worktree listening, and the listing taken
    before the kill shows each of them."""
    f = rec.get('fault') or {}
    trig = (f.get('trigger') or {}).get('services') or {}
    listing = (f.get('running_at_kill') or {}).get('services') or {}
    ok = set(trig) == set(SERVICES) and all(listing.get(k) for k in SERVICES) and all(trig[k]['pid'] in listing.get(k, []) for k in trig)
    return ok, {'trigger': {k: (v.get('pid'), v.get('port')) for k, v in trig.items()}, 'listing': listing}


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--out', required=True)
    parser.add_argument('--skip-grader', action='store_true')
    parser.add_argument('--only', help='comma-separated case names (development only; a binding validation runs all)')
    parser.add_argument('--arms', default='baseline,stack')
    args = parser.parse_args()
    # Normal dispositions even when started from a background shell (which ignores SIGINT): nothing
    # this process starts may inherit an ignored signal.
    signal.signal(signal.SIGINT, signal.default_int_handler)
    signal.signal(signal.SIGTERM, signal.SIG_DFL)
    out = Path(args.out)
    out.mkdir(parents=True, exist_ok=False)
    checks, failures = {}, []
    started = time.monotonic()

    def expect(name, ok, detail):
        checks[name] = {'ok': bool(ok), 'detail': detail}
        if not ok:
            failures.append(name)
        print(('PASS ' if ok else 'FAIL ') + name, json.dumps(detail, default=str)[:400], flush=True)

    for binary, digest, rev in ((NEW_STACK, NEW_STACK_SHA, 'a5d4785'), (OLD_STACK, OLD_STACK_SHA, 'dc4b980')):
        expect(f'setup/stack-binary {rev}', binary.is_file() and runner.sha256(binary) == digest, {'path': str(binary), 'expected_sha256': digest})
    lock = [runner.host_lock()]
    only = set(args.only.split(',')) if args.only else set(CASES) | {'exec-control-dc4b980', 'commit-scan', 'interrupts'}

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

    if 'commit-scan' in only:
        refused, committed, detail = commit_scan_case()
        expect('commit-scan/quoted-path-binary-rename-symlink-message-credential-all-reported', refused, detail)
        expect('commit-scan/clean-commit-goes-through', committed, detail)

    plan, plan_path = make_plan(out, 'plan', NEW_STACK, 'a5d4785')
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

    def run_case(arm, case, the_plan=plan, down_override=None):
        modes, fault = CASES.get(case, ({'invoice-list': 'exec-left'}, None))
        t = time.monotonic()
        rec = runner.run_arm(the_plan, 0, arm, out / f'{arm}-{case}', agent_override=runner.scripted_override(
            ','.join(f'{k}={v}' for k, v in modes.items())), fault={'scenario': fault[0], 'victim': fault[1]} if fault else {},
            down_override=down_override)
        timings[f'{arm}-{case}'] = round(time.monotonic() - t, 1)
        rows, run_row = gates(the_plan, arm, rec, out / f'{arm}-{case}')
        expect(f'{arm}/{case}/run-valid', rec['valid'], rec['invalid_reasons'])
        expect(f'{arm}/{case}/nothing-survives', not rec.get('survivors_after_run') and not rec.get('final_scan'),
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
                rec, rows, rr = run_case(arm, 'clean')
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
                expect(f'{arm}/{case}/killed-cli-connection-fails-run-not-charged-to-recovery',
                       bool(conns) and all(e['culprits'] == ['invoice-list'] for e in conns) and not rr['others_untouched']
                       and rows['invoice-list'].get('isolation_ok'), {'connections': conns})
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
                ok, detail = gated(rows, ['invoice-list', 'void-invoices', 'customer-table'])
                expect(f'{arm}/{case}/bystanders-of-the-faults-gated-success', ok, detail)
                value = sentinel_for(rec)
                hits = files_containing(out / f'{arm}-{case}', value)
                planted = (out / f'{arm}-{case}' / 'agents' / 'invoice-list' / 'submitted-tree' / 'notes.env').exists()
                expect(f'{arm}/{case}/sentinel-absent-from-every-retained-file (completed run)', planted and not hits,
                       {'planted_file_retained': planted, 'files_with_sentinel': hits})
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
                expect(f'{arm}/{case}/victim-copes (restored, verified, gated)', copes.get('copes') and copes.get('service_restored')
                       and copes.get('verified_after_fault'), copes)
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


def commit_scan_case():
    """The commit scan on staged files with a quoted path, a binary file, a rename, a symbolic link
    and the commit message, each carrying a sentinel, plus a credential-shaped value: every one is
    reported, the value is never printed and nothing is committed. A clean commit goes through."""
    import secrets
    sentinel = 'sv' + secrets.token_hex(12)
    env = {**os.environ, 'AGENTVALUE_TEST_SENTINEL': sentinel}
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
        run('add', '-A')
        r = subprocess.run([sys.executable, str(HERE / 'commit.py'), '--repo', str(repo), '-m', f'message with {sentinel}'],
                           capture_output=True, text=True, env=env)
        report_text = r.stdout + r.stderr
        expected = {'quoted path': repr(quoted), 'binary': "'blob.bin'", 'rename': "'new-name.txt'", 'symlink': "'link'",
                    'commit message': '(commit message)', 'credential pattern': "'config.env'"}
        found = {k: v in report_text for k, v in expected.items()}
        unchanged = run('rev-parse', 'HEAD').stdout == head
        refused = r.returncode == 1 and all(found.values()) and sentinel not in report_text and unchanged
        clean = Path(tmp) / 'clean'
        clean.mkdir()
        subprocess.run([*plain, 'init', '-q'], cwd=clean, check=True, capture_output=True)
        (clean / 'README.txt').write_text('nothing secret here\n')
        subprocess.run(['git', 'add', '-A'], cwd=clean, check=True)
        c = subprocess.run([sys.executable, str(HERE / 'commit.py'), '--repo', str(clean), '-m', 'clean commit'], capture_output=True, text=True,
                           env={**env, 'GIT_AUTHOR_NAME': 't', 'GIT_AUTHOR_EMAIL': 't@example.invalid', 'GIT_COMMITTER_NAME': 't',
                                'GIT_COMMITTER_EMAIL': 't@example.invalid'})
        committed = c.returncode == 0 and subprocess.run(['git', 'rev-parse', '--verify', 'HEAD'], cwd=clean, capture_output=True).returncode == 0
    return refused, committed, {'exit': r.returncode, 'found': found, 'value_printed': sentinel in report_text, 'head_unchanged': unchanged,
                                'clean_commit_exit': c.returncode, 'clean_stderr': c.stderr[-300:]}


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
