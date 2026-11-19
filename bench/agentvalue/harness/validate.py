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
import json
from pathlib import Path
import shutil
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
                     ('limit not range-checked', _list_limit_unchecked, 'bad_params_400'),
                     ('items in descending id order', _list_descending, 'list_all_ascending')],
    'void-invoices': [('void does not invalidate the cache', _void_no_invalidate, 'cache_after_void'),
                      ('reads bypass the cache entirely', _void_bypass_cache, 'cache_after_void'),
                      ('constraint dropped and not re-added', _void_constraint_dropped, 'constraint_allows_void_only'),
                      ('PATCH on a void invoice allowed', _void_patch_allowed, 'patch_void_409'),
                      ('a paid invoice is voided', _void_paid_changed, 'void_paid_409'),
                      ('a void invoice can be voided again', _void_twice_ok, 'void_twice_409')],
}


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
    'agent-kill-recovers': ({'invoice-list': 'stall', 'void-invoices': 'stray'}, ('agent-kill', 'invoice-list')),
    'agent-kill-recovery-leaves': ({'pay-receipts': 'stall', 'pay-receipts.recovery': 'leave-running',
                                    'customer-table': 'target', 'invoice-notes': 'killer:wt-cust'}, ('agent-kill', 'pay-receipts')),
    'service-kill-restarts': ({'void-invoices': 'sk-restart', 'invoice-list': 'exec-left'}, ('service-kill', 'void-invoices')),
    'service-kill-no-reaction': ({'customer-table': 'sk-noreact'}, ('service-kill', 'customer-table')),
    'version1-planted': ({'invoice-notes': 'leave-running', 'pay-receipts': 'contaminate', 'customer-table': 'pkill'}, None),
}


def make_plan(out, name, binary, rev):
    path = out / f'{name}.json'
    subprocess.run([sys.executable, str(HERE / 'run.py'), 'plan', '--name', name, '--kind', 'harness_selftest', '--pairs', '1',
                    '--seed', '0', '--agent-seconds', '600', '--stack', str(binary), '--stack-rev', rev, '--out', str(path)],
                   check=True, capture_output=True)
    return runner.load_plan(path)


def gates(plan, arm, rec, out_dir):
    """Version 2 slot rows and the run row, computed by report.py exactly as for a trial."""
    fake = {'name': 'v', 'kind': 'harness_selftest', 'protocol_version': 2, 'host': {}, 'config': plan['config'],
            'pairs': [{'index': 0, 'order': [arm], 'scenario': rec.get('scenario'), 'victim': rec.get('victim')}]}
    rows, run_rows = report.rows_v2(fake, {(0, arm): rec}, str(out_dir.parent))
    return {r['task']: r for r in rows}, run_rows[0]


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--out', required=True)
    parser.add_argument('--skip-grader', action='store_true')
    parser.add_argument('--only', help='comma-separated case names (development only; a binding validation runs all)')
    parser.add_argument('--arms', default='baseline,stack')
    args = parser.parse_args()
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
        expect(f'setup/stack-binary {rev}',
               binary.is_file() and runner.sha256(binary) == digest, {'path': str(binary), 'expected_sha256': digest})
    lock = runner.host_lock()

    if not args.skip_grader:
        cells = []
        for task in runner.TASK:
            cells += [(task, 'noop', None, None, '0'), (task, 'reference', None, None, '0')]
            cells += [(task, 'mutant', change, (description, target), str(i)) for i, (description, change, target) in enumerate(MUTANTS[task])]
        with ThreadPoolExecutor(4) as pool:
            results = list(pool.map(lambda c: grade_cell(c[0], c[1], out, c[2], c[4]), cells))
        for (task, mode, change, info, label), r in zip(cells, results):
            if mode == 'noop':
                expect(f'grader/{task}/starting-code-fails', not r['passed'] and runner.TASK[task]['no_op_failing_check'] in r['failed_checks'], r)
            elif mode == 'reference':
                expect(f'grader/{task}/reference-passes', r['passed'], r)
            else:
                description, target = info
                fired = (not r['fresh_ok']) if target == 'fresh_ok' else target in r['failed_checks']
                expect(f'grader/{task}/mutant-fails ({description})', not r['passed'] and fired, r)
        for task in runner.TASK:
            expect(f'grader/{task}/at-least-three-mutants', len(MUTANTS[task]) >= (3 if task in ('invoice-list', 'void-invoices') else 1),
                   len(MUTANTS[task]))

    plan = make_plan(out, 'plan', NEW_STACK, 'a5d4785')
    control_plan = make_plan(out, 'plan-control-dc4b980', OLD_STACK, 'dc4b980')
    expect('setup/plan-records-binary', plan['config']['stack_binary_sha256'] == NEW_STACK_SHA and plan['config']['stack_rev'] == 'a5d4785'
           and plan['config']['stack_version'] and plan['protocol_version'] == 2,
           {k: plan['config'][k] for k in ('stack_rev', 'stack_binary_sha256', 'stack_version', 'fault')})
    only = set(args.only.split(',')) if args.only else set(CASES) | {'exec-control-dc4b980'}
    timings = {}

    def run_case(arm, case, the_plan=plan):
        modes, fault = CASES.get(case, ({'invoice-list': 'exec-left'}, None))
        t = time.monotonic()
        rec = runner.run_arm(the_plan, 0, arm, out / f'{arm}-{case}', agent_override=runner.scripted_override(
            ','.join(f'{k}={v}' for k, v in modes.items())), fault={'scenario': fault[0], 'victim': fault[1]} if fault else {})
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
            and (k.get('seconds_after_launch') or 1e9) < 600
        if scenario == 'agent-kill':
            ok = ok and k.get('pid') == victim_session.get('pid') and k.get('returncode') == -9 and victim_session.get('killed_by_harness') \
                and any(p['pid'] == victim_session.get('pid') for p in (f.get('running_at_kill') or {}).get('processes') or [])
        else:
            command = trig.get('command') or ''
            # The postmaster of the victim's worktree: the baseline's names the worktree in its data
            # directory; Stack's lives under Stack's state and is attributed through its marker.
            ok = ok and k.get('pid') == trig.get('pid') and k.get('what') == 'postmaster' and '/bin/postgres' in command \
                and (rec['arm'] != 'baseline' or f"/{runner.SHORT[victim]}/" in command)
        return bool(ok), {'injected': f.get('injected'), 'missed': f.get('missed'), 'trigger': trig, 'kill': k}

    for arm in args.arms.split(','):
        tasks = plan['config']['tasks']
        if 'clean' in only:
            rec, rows, rr = run_case(arm, 'clean')
            ok, detail = gated(rows, tasks)
            expect(f'{arm}/clean/every-slot-gated-success', ok, detail)
            expect(f'{arm}/clean/used-own-db', all(rows[t].get('used_own_db') for t in tasks), [rows[t].get('used_own_db') for t in tasks])
            expect(f'{arm}/clean/others-untouched', rr['others_untouched'] and not rr['review'], {'events': rr['events'], 'review': rr['review']})
            expect(f'{arm}/clean/residue-holds-every-worktree', rr['residue_all'],
                   {t: rec['teardown'][t]['after_down'] for t in tasks if not rec['teardown'][t].get('residue_ok')})
            expect(f'{arm}/clean/no-false-confirmation', rr['false_confirmations'] == 0, rr['false_confirmations'])
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
            expect(f'{arm}/clean/no-environment-in-records', no_environment(out / f'{arm}-clean'), 'scanned')

        if 'agent-kill-recovers' in only:
            case = 'agent-kill-recovers'
            rec, rows, rr = run_case(arm, case)
            ok, detail = fault_at_plan(rec, 'agent-kill', 'invoice-list')
            expect(f'{arm}/{case}/fault-injected-at-planned-point', ok, detail)
            recovery = (rec.get('fault') or {}).get('recovery') or {}
            expect(f'{arm}/{case}/recovery-started-10s-after-kill', 10.0 <= (recovery.get('seconds_after_kill') or 0) <= 11.5
                   and rec['slots'].get('invoice-list') == 'invoice-list-recovery', recovery)
            prompt = (out / f'{arm}-{case}' / 'agents' / 'invoice-list-recovery' / 'prompt.txt').read_text() \
                if (out / f'{arm}-{case}' / 'agents' / 'invoice-list-recovery' / 'prompt.txt').exists() else ''
            expect(f'{arm}/{case}/recovery-prompt-verbatim', prompt == runner.RECOVERY_PROMPT.format(
                branch=runner.TASK['invoice-list']['branch'], instruction=runner.TASK['invoice-list']['instruction']), prompt[:120])
            expect(f'{arm}/{case}/killed-session-not-scored', rows['invoice-list'].get('session') == 'invoice-list-recovery'
                   and 'grade' not in rec['agents']['invoice-list'], rows['invoice-list'].get('session'))
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
            expect(f'{arm}/{case}/no-contamination', rr['others_untouched'], rr['events'])

        if 'agent-kill-recovery-leaves' in only:
            case = 'agent-kill-recovery-leaves'
            rec, rows, rr = run_case(arm, case)
            ok, detail = fault_at_plan(rec, 'agent-kill', 'pay-receipts')
            expect(f'{arm}/{case}/fault-injected-at-planned-point', ok, detail)
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

        if 'service-kill-restarts' in only:
            case = 'service-kill-restarts'
            rec, rows, rr = run_case(arm, case)
            ok, detail = fault_at_plan(rec, 'service-kill', 'void-invoices')
            expect(f'{arm}/{case}/postmaster-killed-at-planned-point', ok, detail)
            expect(f'{arm}/{case}/victim-passes', rows['void-invoices'].get('gated_success'),
                   {k: rows['void-invoices'].get(k) for k in ('passed', 'teardown_ok', 'isolation_ok', 'cli_ok')})
            expect(f'{arm}/{case}/kill-not-contamination', rr['others_untouched'] and rows['void-invoices'].get('isolation_ok'),
                   {'events': rr['events'], 'review': rr['review']})
            ex = rec['teardown']['invoice-list']
            expect(f'{arm}/{case}/exec-left-fails-teardown-at-exit', not rows['invoice-list'].get('teardown_ok')
                   and any('time.sleep(3600)' in p['command'] for p in rec['agents']['invoice-list'].get('left_running') or []),
                   rec['agents']['invoice-list'].get('left_running'))
            if arm == 'stack':
                named = [d.get('pid') for r in ex.get('rounds') or [] for d in r.get('named') or []]
                expect(f'{arm}/{case}/exec-left-down-unconfirmed-names-pid', ex.get('down_exit_first') == 70 and named
                       and '"stop_unconfirmed"' in ex['procedure'][0]['stdout'], {'first_exit': ex.get('down_exit_first'), 'named': named,
                                                                               'stdout': ex['procedure'][0]['stdout'][-400:]})
                expect(f'{arm}/{case}/exec-left-procedure-leaves-clean', ex.get('down_exit') == 0 and not ex['after_down']['processes']
                       and ex.get('residue_ok') and not ex.get('false_confirmation'), {'down_exit': ex.get('down_exit'), 'after_down': ex['after_down']})
            else:
                expect(f'{arm}/{case}/exec-left-down-false-confirmation', ex.get('down_exit_first') == 0 and ex.get('false_confirmation')
                       and any('time.sleep(3600)' in p['command'] for p in ex['after_down']['processes']),
                       {'down_exit': ex.get('down_exit'), 'after_down': ex['after_down']})
                expect(f'{arm}/{case}/exec-left-residue-fails', ex.get('residue_ok') is False, ex.get('after_down'))
            ok, detail = gated(rows, ['invoice-notes', 'pay-receipts', 'customer-table'])
            expect(f'{arm}/{case}/others-gated-success', ok, detail)

        if 'service-kill-no-reaction' in only:
            case = 'service-kill-no-reaction'
            rec, rows, rr = run_case(arm, case)
            ok, detail = fault_at_plan(rec, 'service-kill', 'customer-table')
            expect(f'{arm}/{case}/postmaster-killed-at-planned-point', ok, detail)
            expect(f'{arm}/{case}/victim-fails-its-task', not rows['customer-table'].get('passed') and not rows['customer-table'].get('gated_success'),
                   {k: rows['customer-table'].get(k) for k in ('passed', 'cli_ok', 'failed_checks')})
            ok, detail = gated(rows, ['invoice-notes', 'pay-receipts', 'invoice-list', 'void-invoices'])
            expect(f'{arm}/{case}/other-agents-unaffected', ok and rr['others_untouched'], {'slots': detail, 'events': rr['events']})

        if 'version1-planted' in only:
            case = 'version1-planted'
            rec, rows, rr = run_case(arm, case)
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
            expect(f'{arm}/{case}/clean-agents-unaffected', all(rows[t]['teardown_ok'] for t in ('customer-table', 'pay-receipts', 'invoice-list', 'void-invoices')),
                   {t: r.get('teardown_ok') for t, r in rows.items()})

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

    summary = {'ok': not failures, 'failures': failures, 'checks': checks, 'seconds': round(time.monotonic() - started), 'run_seconds': timings,
               'stack_binary_sha256': plan['config']['stack_binary_sha256'], 'stack_rev': plan['config']['stack_rev'],
               'control_stack_binary_sha256': control_plan['config']['stack_binary_sha256'],
               'source_rev': plan['config']['source_rev'], 'fingerprints': plan['fingerprints'], 'cases_run': sorted(only)}
    (out / 'validation.json').write_text(json.dumps(summary, indent=2, default=str))
    print(json.dumps({'ok': summary['ok'], 'failures': failures, 'seconds': summary['seconds']}))
    lock.close()
    sys.exit(0 if summary['ok'] else 1)


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
