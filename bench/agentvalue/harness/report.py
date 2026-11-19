#!/usr/bin/env python3
"""Summarize every planned run from its retained run.json records.

  python3 bench/agentvalue/harness/report.py --plan <plan.json> --results <dir> [--out <dir>]

A version 2 plan (protocol_version 2) gets the version 2 report: per scenario and per arm, the
version 2 gates and measures, paired differences within a scenario and the "separates the arms"
rule with its inputs. A version 1 plan gets the version 1 report, unchanged.

Every planned agent slot stays in the denominator: a missing or invalid run counts as
not run, never as a success or a failure. Scoring rules are in ../PROTOCOL.md; this file
implements them and nothing else.
"""
import argparse
import json
import math
from pathlib import Path
import random
import statistics

TARGET_WORDS = ('worker', 'app.py', 'postgres', 'redis', 'python', 'uv', 'supervise')
KILL_SCOPE_NOTE = 'broad kill commands are counted from the trace; collateral kills are proven only for the main checkout'


def load_runs(plan, results, plan_sha=None):
    """Runs of this plan only: a run.json from another plan (or a self-test) is reported as foreign, not used."""
    runs, foreign = {}, {}
    for pair in plan['pairs']:
        for arm in pair['order']:
            path = Path(results) / f"pair{pair['index']}-{arm}" / 'run.json'
            run = json.loads(path.read_text()) if path.exists() else None
            mismatch = run is not None and (run.get('pair') != pair['index'] or run.get('arm') != arm
                                            or run.get('model') != plan['config']['model'] or run.get('plan_kind') != plan['kind'])
            if run is not None and plan.get('protocol_version') == 2:
                mismatch = mismatch or run.get('protocol_version') != 2 or run.get('scenario') != pair.get('scenario') \
                    or run.get('victim') != pair.get('victim')
            if run is not None and ((plan_sha and run.get('plan_sha256') != plan_sha) or mismatch):
                foreign[f"pair{pair['index']}-{arm}"] = run.get('plan_sha256')
                run = None
            runs[(pair['index'], arm)] = run
    return runs, foreign


def contamination(run):
    """Every contamination event in a run, each attributed to a culprit agent when the evidence names one."""
    events = []
    agents = run.get('agents', {})
    own = {t: set((a.get('submission') or {}).get('migrations') or []) for t, a in agents.items()}
    markers = {a.get('marker'): t for t, a in agents.items()}
    by = run.get('bystander') or {}
    for version in by.get('foreign_migrations') or []:
        culprits = [t for t, m in own.items() if version in m]
        events.append({'kind': 'migration_in_main', 'detail': version, 'culprits': culprits})
    # Write counters move with statistics flushes and catalog activity: diagnostic, never a gate.
    # A foreign migration explains the ledger entry; schema or data changes stay a separate event,
    # unattributed, so a second agent's independent change is not pinned on the migration's owner.
    explained = {'migrations'} if by.get('foreign_migrations') else set()
    other_changes = [c for c in by.get('changed') or [] if c not in explained | {'tuple_writes'}]
    if other_changes:
        events.append({'kind': 'main_state_changed', 'detail': other_changes, 'culprits': []})
    if by.get('processes_lost'):
        lost_words = {w for p in by['processes_lost'] for w in TARGET_WORDS if w in p['command']}
        killers = [t for t, a in agents.items()
                   if any(w in cmd for cmd in (a.get('trace_metrics') or {}).get('kill_commands') or [] for w in lost_words)]
        events.append({'kind': 'main_process_killed', 'detail': [p['command'][:80] for p in by['processes_lost']], 'culprits': killers})
    for victim, db in (run.get('worktree_databases') or {}).items():
        for version in db.get('foreign') or []:
            culprits = [t for t, m in own.items() if version in m and t != victim]
            events.append({'kind': 'migration_in_other_worktree', 'detail': f'{version} in {victim}', 'culprits': culprits})
        if db.get('has_main_canary'):
            events.append({'kind': 'main_data_in_worktree', 'detail': victim, 'culprits': [victim]})
    for c in (run.get('sampler') or {}).get('cross_worktree_connections') or []:
        culprit = markers.get(c['client'])
        events.append({'kind': 'cross_worktree_connection', 'detail': f"{c['client']} -> {c['server']}:{c['port']}",
                       'culprits': [culprit] if culprit else []})
    return events


def agent_rows(plan, runs):
    rows = []
    for pair in plan['pairs']:
        for arm in pair['order']:
            run = runs.get((pair['index'], arm))
            for task in plan['config']['tasks']:
                row = {'pair': pair['index'], 'arm': arm, 'task': task, 'position': pair['order'].index(arm)}
                if run is None:
                    rows.append({**row, 'status': 'not_run'})
                    continue
                if not run.get('valid'):
                    rows.append({**row, 'status': 'invalid', 'reasons': run.get('invalid_reasons')})
                    continue
                a = run['agents'].get(task) or {}
                g = a.get('grade') or {}
                m = a.get('trace_metrics') or {}
                result = m.get('result') or {}
                events = contamination(run)
                unattributed = any(not e['culprits'] for e in events)
                culprit = any(task in e['culprits'] for e in events)
                db = (run.get('worktree_databases') or {}).get(task) or {}
                own_new = set((a.get('submission') or {}).get('migrations') or []) - set(
                    v for v in ['20261001_create_invoices', '20261008_create_receipts'])
                used_own_db = bool(db.get('exists')) and bool(own_new & set(db.get('versions') or []))
                if 'left_running' not in a or 'grade' not in a:
                    rows.append({**row, 'status': 'invalid', 'reasons': ['missing exit observation or grade']})
                    continue
                teardown_ok = not a.get('left_running') and not a.get('left_listening')
                isolation_ok = not culprit and not unattributed
                passed = bool(g.get('passed'))
                cli_ok = a.get('exit') == 0 and result.get('subtype') == 'success' and not result.get('is_error')
                compliance = a.get('compliance') or {}
                rows.append({**row, 'status': 'scored', 'passed': passed, 'teardown_ok': teardown_ok, 'isolation_ok': isolation_ok,
                             'cli_ok': cli_ok, 'gated_success': passed and teardown_ok and isolation_ok and cli_ok, 'used_own_db': used_own_db,
                             'committed': compliance.get('committed'), 'ran_tests': compliance.get('ran_tests'),
                             'arm_down_recovered': ((run.get('recovery') or {}).get(task) or {}).get('down_exit') == 0
                                                   and not ((run.get('recovery') or {}).get(task) or {}).get('left_after_arm_down'),
                             'unexplained_migrations': db.get('unexplained') or [],
                             'timed_out': bool(a.get('timed_out')), 'agent_exit': a.get('exit'), 'seconds': a.get('seconds'),
                             'cost_usd': result.get('total_cost_usd'), 'turns': result.get('num_turns'),
                             'tool_calls': m.get('tool_calls'), 'bash_calls': m.get('bash_calls'), 'tool_errors': m.get('tool_errors'),
                             'env_errors': m.get('env_errors'), 'kill_commands': len(m.get('kill_commands') or []),
                             'benchmark_access': len(m.get('benchmark_access') or []),
                             'left_running': len(a.get('left_running') or []), 'failed_checks': g.get('failed_checks'),
                             'fresh_ok': g.get('fresh_ok'), 'model': m.get('model'), 'cli': m.get('claude_code_version'),
                             'result_subtype': result.get('subtype'), 'contamination_events': [e for e in events if task in e['culprits']]})
    return rows


def describe(values):
    values = [v for v in values if isinstance(v, (int, float)) and not (isinstance(v, float) and math.isnan(v))]
    if not values:
        return {'n': 0}
    values = sorted(values)
    q = statistics.quantiles(values, n=4) if len(values) >= 2 else [values[0]] * 3
    return {'n': len(values), 'median': round(statistics.median(values), 3), 'q1': round(q[0], 3), 'q3': round(q[2], 3),
            'min': round(values[0], 3), 'max': round(values[-1], 3),
            'sd': round(statistics.stdev(values), 3) if len(values) >= 2 else None}


def rate(rows, key):
    scored = [r for r in rows if r['status'] == 'scored']
    return {'k': sum(1 for r in scored if r.get(key)), 'scored': len(scored), 'assigned': len(rows)}


def bootstrap_pairs(plan, rows, key, reps=10000, seed=7):
    """Difference stack - baseline in the per-pair mean of `key`, resampling whole pairs."""
    diffs = []
    for pair in plan['pairs']:
        means = {}
        for arm in ('stack', 'baseline'):
            vals = [1.0 if r.get(key) else 0.0 for r in rows if r['pair'] == pair['index'] and r['arm'] == arm and r['status'] == 'scored']
            if len(vals) != len(plan['config']['tasks']):
                break
            means[arm] = sum(vals) / len(vals)
        if len(means) == 2:
            diffs.append(means['stack'] - means['baseline'])
    if not diffs:
        return {'pairs': 0}
    if len(diffs) < 3:  # an interval from one or two pairs would be meaningless
        return {'pairs': len(diffs), 'per_pair': [round(d, 3) for d in diffs], 'mean_diff': round(sum(diffs) / len(diffs), 3), 'ci95': None}
    rng = random.Random(seed)
    boots = sorted(sum(rng.choice(diffs) for _ in diffs) / len(diffs) for _ in range(reps))
    return {'pairs': len(diffs), 'per_pair': [round(d, 3) for d in diffs], 'mean_diff': round(sum(diffs) / len(diffs), 3),
            'ci95': [round(boots[int(0.025 * reps)], 3), round(boots[int(0.975 * reps)], 3)]}


def summarize(plan, runs, foreign=None):
    rows = agent_rows(plan, runs)
    arms = {}
    for arm in ('stack', 'baseline'):
        mine = [r for r in rows if r['arm'] == arm]
        scored = [r for r in mine if r['status'] == 'scored']
        success = [r for r in scored if r['gated_success']]
        episodes = {}
        for r in scored:
            episodes.setdefault(r['pair'], []).append(r['gated_success'])
        arms[arm] = {
            'task_success': rate(mine, 'passed'), 'gated_success': rate(mine, 'gated_success'), 'cli_ok': rate(mine, 'cli_ok'),
            'committed': rate(mine, 'committed'), 'ran_tests': rate(mine, 'ran_tests'),
            'by_position': {pos: {'gated': rate([r for r in mine if r['position'] == pos], 'gated_success'),
                                  'passed': rate([r for r in mine if r['position'] == pos], 'passed')} for pos in (0, 1)},
            'teardown_ok': rate(mine, 'teardown_ok'), 'isolation_ok': rate(mine, 'isolation_ok'), 'used_own_db': rate(mine, 'used_own_db'),
            'timed_out': rate(mine, 'timed_out'),
            'episodes_all_gated_success': sum(1 for v in episodes.values() if len(v) == len(plan['config']['tasks']) and all(v)),
            'episodes': len(episodes), 'episodes_assigned': len(plan['pairs']),
            'seconds_to_verified_result': describe([r['seconds'] for r in success]),
            'seconds_all': describe([r['seconds'] for r in scored]),
            'cost_usd': describe([r['cost_usd'] for r in scored]), 'turns': describe([r['turns'] for r in scored]),
            'tool_calls': describe([r['tool_calls'] for r in scored]), 'bash_calls': describe([r['bash_calls'] for r in scored]),
            'tool_errors': describe([r['tool_errors'] for r in scored]), 'env_errors': describe([r['env_errors'] for r in scored]),
            'kill_commands_total': sum(r['kill_commands'] for r in scored),
            'benchmark_access_total': sum(r['benchmark_access'] for r in scored),
            'not_run': sum(1 for r in mine if r['status'] == 'not_run'), 'invalid': sum(1 for r in mine if r['status'] == 'invalid'),
            'by_task': {t: {'passed': [r.get('passed') for r in mine if r['task'] == t],
                            'gated': [r.get('gated_success') for r in mine if r['task'] == t],
                            'seconds': [r.get('seconds') for r in mine if r['task'] == t],
                            'cost': [r.get('cost_usd') for r in mine if r['task'] == t]} for t in plan['config']['tasks']},
        }
    discordant = {}
    for task in plan['config']['tasks']:
        for pair in plan['pairs']:
            s = next((r for r in rows if r['pair'] == pair['index'] and r['arm'] == 'stack' and r['task'] == task), None)
            b = next((r for r in rows if r['pair'] == pair['index'] and r['arm'] == 'baseline' and r['task'] == task), None)
            if s and b and s['status'] == b['status'] == 'scored' and (s['gated_success'] != b['gated_success'] or s['passed'] != b['passed']):
                discordant.setdefault(task, []).append({'pair': pair['index'], 'stack_gated': s['gated_success'], 'baseline_gated': b['gated_success'],
                                                        'stack_passed': s['passed'], 'baseline_passed': b['passed']})
    runs_meta = {f'pair{k[0]}-{k[1]}': None if v is None else {
        'valid': v.get('valid'), 'invalid_reasons': v.get('invalid_reasons'), 'seconds': v.get('seconds'),
        'stack_binary_sha256': v.get('stack_binary_sha256'), 'source_rev': v.get('source_rev'),
        'contamination': contamination(v) if v.get('valid') else None,
        'disposal': {k2: (v.get('disposal') or {}).get(k2) for k2 in ['processes_left', 'ports_still_listening', 'service_data_left',
                                                                      'stack_records_left', 'worktree_dirs_left']},
        'recovery': {t: {'agent_left': r.get('agent_left'), 'left_after_arm_down': len(r.get('left_after_arm_down') or [])}
                     for t, r in (v.get('recovery') or {}).items()},
        'disposal_state_growth_bytes': ((v.get('disposal') or {}).get('machine_state_bytes_after') or 0) - (v.get('machine_state_bytes_before_agents') or 0),
    } for k, v in runs.items()}
    return {'plan': plan['name'], 'kind': plan['kind'], 'config': {k: plan['config'][k] for k in plan['config'] if k != 'agent_command'},
            'agent_command': plan['config']['agent_command'], 'host': plan['host'], 'arms': arms,
            'paired_gated_success_diff': bootstrap_pairs(plan, rows, 'gated_success'),
            'paired_task_success_diff': bootstrap_pairs(plan, rows, 'passed'),
            'discordant_pairs': discordant, 'runs': runs_meta, 'rows': rows, 'note': KILL_SCOPE_NOTE,
            'missing_runs': sorted(k for k, v in runs_meta.items() if v is None), 'foreign_runs': foreign or {},
            'invalid_runs': sorted(k for k, v in runs_meta.items() if v and not v['valid'])}


def markdown(summary):
    lines = [f"# {summary['plan']} ({summary['kind']})", '']
    lines += ['| Measure | Stack | Baseline |', '|---|---|---|']
    def frac(x):
        return f"{x['k']}/{x['scored']} (assigned {x['assigned']})"
    def stat(x):
        return '–' if not x.get('n') else f"median {x['median']} [IQR {x['q1']}–{x['q3']}], range {x['min']}–{x['max']}, n={x['n']}"
    a, b = summary['arms']['stack'], summary['arms']['baseline']
    for label, key, fn in [('Task success (hidden checks)', 'task_success', frac),
                           ('Gated success (task + teardown + isolation + clean CLI exit)', 'gated_success', frac),
                           ('Agent CLI ended cleanly', 'cli_ok', frac), ('Committed its work', 'committed', frac), ('Ran the tests', 'ran_tests', frac),
                           ('Teardown gate held', 'teardown_ok', frac), ('Isolation gate held', 'isolation_ok', frac),
                           ('Used its own database', 'used_own_db', frac), ('Timed out', 'timed_out', frac),
                           ('Seconds to verified result', 'seconds_to_verified_result', stat), ('Seconds, all scored', 'seconds_all', stat),
                           ('Cost USD (CLI-reported, list)', 'cost_usd', stat), ('Turns', 'turns', stat), ('Tool calls', 'tool_calls', stat),
                           ('Tool errors', 'tool_errors', stat), ('Environment errors in tool output', 'env_errors', stat)]:
        lines.append(f'| {label} | {fn(a[key])} | {fn(b[key])} |')
    lines.append(f"| Episodes with all agents gated-successful (of scored; assigned) | {a['episodes_all_gated_success']}/{a['episodes']}; {a['episodes_assigned']} | {b['episodes_all_gated_success']}/{b['episodes']}; {b['episodes_assigned']} |")
    lines.append(f"| Broad kill commands | {a['kill_commands_total']} | {b['kill_commands_total']} |")
    lines.append(f"| Benchmark-file access in traces | {a['benchmark_access_total']} | {b['benchmark_access_total']} |")
    lines.append(f"| Not run / invalid agent slots | {a['not_run']} / {a['invalid']} | {b['not_run']} / {b['invalid']} |")
    lines.append(f"| Gated success by position (first / second in pair) | {frac(a['by_position'][0]['gated'])} / {frac(a['by_position'][1]['gated'])} | "
                 f"{frac(b['by_position'][0]['gated'])} / {frac(b['by_position'][1]['gated'])} |")
    lines += ['', f"Missing runs: {summary['missing_runs']}; invalid runs: {summary['invalid_runs']}; runs from another plan ignored: {summary['foreign_runs']}"]
    lines += ['', f"Paired difference in gated success (stack − baseline, per-pair mean, bootstrap over pairs): {summary['paired_gated_success_diff']}",
              f"Paired difference in task success: {summary['paired_task_success_diff']}", f"Discordant pairs: {summary['discordant_pairs']}", '']
    lines += ['| Pair | Arm | Pos | Task | Status | Pass | Teardown | Isolation | Seconds | Cost | Tools | Env err | Failed checks |',
              '|---|---|---|---|---|---|---|---|---|---|---|---|---|']
    for r in summary['rows']:
        lines.append(f"| {r['pair']} | {r['arm']} | {r['position']} | {r['task']} | {r['status']} | {r.get('passed', '')} | {r.get('teardown_ok', '')} | "
                     f"{r.get('isolation_ok', '')} | {r.get('seconds', '')} | {r.get('cost_usd') if r.get('cost_usd') is None else round(r['cost_usd'], 3)} | "
                     f"{r.get('tool_calls', '')} | {r.get('env_errors', '')} | {', '.join(r.get('failed_checks') or [])} |")
    return '\n'.join(lines) + '\n'


# ====================================================================== protocol version 2
# PROTOCOL.md, "Version 2": gates, measures and analysis per scenario and per arm.

SIBLING_OWN_WINDOW_S = 10    # X's own stop/restart command in the 10 s before the loss explains it
SIBLING_KILL_WINDOW_S = 5    # another agent's kill-like command in the 5 s before makes it event (g)
AFTER_FAULT_WINDOW_S = 10    # losses in the service-kill victim's worktree this soon after the kill are its consequence


def slot_key(run, task):
    return (run.get('slots') or {}).get(task, task)


def running_at(agent, m):
    return agent.get('launched_m') is not None and agent['launched_m'] <= m and (agent.get('ended_m') is None or agent['ended_m'] >= m)


def sibling_losses(run):
    """Event (g): a durable service process of worktree X gone between two samples while X's own
    agent runs, with no stop/restart command of X's own agent in the 10 s before and a kill-like
    command (version 1 pattern) of another agent in the 5 s before; the culprit is that agent.
    The loss time is only known to lie in (last_seen_m, gone_m], so each window is taken against
    that whole interval. Returns (events, review, not_events)."""
    agents = run.get('agents') or {}
    fault = run.get('fault') or {}
    kill = fault.get('kill') or {}
    injected = (kill.get('pid'), kill.get('started')) if fault.get('injected') and run.get('scenario') == 'service-kill' else None
    events, review, not_events = [], [], []
    for loss in (run.get('sampler') or {}).get('durable_losses') or []:
        group, last, gone = loss['group'], loss['last_seen_m'], loss['gone_m']
        item = {'worktree': group, 'pid': loss['pid'], 'command': loss['command'][:120], 'last_seen_m': last, 'gone_m': gone}
        if group == 'main':
            continue  # the bystander's losses are version 1 event (c)
        if injected and (loss['pid'], loss['started']) == injected:
            not_events.append({**item, 'reason': 'the injected fault'})
            continue
        own = {k: a for k, a in agents.items() if a.get('marker') == group}
        if not any(running_at(a, last) for a in own.values()):
            not_events.append({**item, 'reason': 'its own agent was not running'})
            continue
        own_commands = [c for a in own.values() for c in a.get('service_commands') or []
                        if c.get('t_m') is not None and last - SIBLING_OWN_WINDOW_S <= c['t_m'] <= gone]
        if own_commands:
            not_events.append({**item, 'reason': 'its own agent stopped or restarted services just before',
                               'command': own_commands[-1]['command'][:120]})
            continue
        culprits = sorted({k for k, a in agents.items() if a.get('marker') != group for c in a.get('service_commands') or []
                           if c.get('kind') == 'kill' and c.get('t_m') is not None and last - SIBLING_KILL_WINDOW_S <= c['t_m'] <= gone})
        after_fault = injected and fault.get('worktree') == group and kill.get('m') is not None \
            and kill['m'] <= gone and last <= kill['m'] + AFTER_FAULT_WINDOW_S
        if culprits and not after_fault:
            events.append({'kind': 'sibling_service_lost', 'detail': f"{group}: {loss['command'][:80]} (pid {loss['pid']})",
                           'culprits': culprits, 'loss': item})
        else:
            review.append({**item, 'reason': 'within 10 s after the injected fault' if after_fault else "no agent's command explains it",
                           'kill_commands_near': culprits})
    return events, review, not_events


def contamination_v2(run):
    """Version 1 events (a)-(f), with culprits named by session (a recovery session is its own
    culprit), and event (g)."""
    agents = run.get('agents') or {}
    events = []
    for e in contamination(run):
        e = dict(e)
        if e['kind'] == 'main_data_in_worktree':
            e['culprits'] = [slot_key(run, c) for c in e['culprits']]
        elif e['kind'] == 'cross_worktree_connection':
            client = e['detail'].split(' -> ')[0]
            match = next((c for c in (run.get('sampler') or {}).get('cross_worktree_connections') or []
                          if f"{c['client']} -> {c['server']}:{c['port']}" == e['detail']), {})
            at = match.get('m')
            sessions = [k for k, a in agents.items() if a.get('marker') == client and (at is None or running_at(a, at))]
            task = next((a.get('task') for a in agents.values() if a.get('marker') == client), None)
            e['culprits'] = sessions or ([slot_key(run, task)] if task else [])
        events.append(e)
    g_events, review, not_events = sibling_losses(run)
    return events + g_events, review, not_events


def run_dir(results, pair, arm):
    return f"{results}/pair{pair}-{arm}"


def rows_v2(plan, runs, results):
    """One row per slot (five per run). In agent-kill the victim's slot is the recovery session."""
    rows, run_rows = [], []
    for pair in plan['pairs']:
        for arm in pair['order']:
            run = runs.get((pair['index'], arm))
            base = {'pair': pair['index'], 'arm': arm, 'scenario': pair.get('scenario'), 'victim': pair.get('victim'),
                    'position': pair['order'].index(arm)}
            where = run_dir(results, pair['index'], arm)
            if run is None or not run.get('valid'):
                status = 'not_run' if run is None else 'invalid'
                run_rows.append({**base, 'status': status, 'reasons': (run or {}).get('invalid_reasons'), 'record': f'{where}/run.json'})
                rows += [{**base, 'task': t, 'status': status} for t in plan['config']['tasks']]
                continue
            events, review, not_events = contamination_v2(run)
            unattributed = [e for e in events if not e['culprits']]
            fault = run.get('fault') or {}
            teardown = run.get('teardown') or {}
            slot_rows = []
            for task in plan['config']['tasks']:
                key = slot_key(run, task)
                a = (run.get('agents') or {}).get(key) or {}
                if 'left_running' not in a or 'grade' not in a:
                    slot_rows.append({**base, 'task': task, 'status': 'invalid', 'reasons': ['missing exit observation or grade']})
                    continue
                g, m = a.get('grade') or {}, a.get('trace_metrics') or {}
                result = m.get('result') or {}
                passed = bool(g.get('passed'))
                teardown_ok = not a.get('left_running') and not a.get('left_listening')
                culprit_of = [e for e in events if key in e['culprits']]
                isolation_ok = not culprit_of and not unattributed
                cli_ok = a.get('exit') == 0 and result.get('subtype') == 'success' and not result.get('is_error')
                t = teardown.get(task) or {}
                db = (run.get('worktree_databases') or {}).get(task) or {}
                own_new = set((a.get('submission') or {}).get('migrations') or []) - {'20261001_create_invoices', '20261008_create_receipts'}
                trace = f'{where}/agents/{key}/trace.jsonl'
                failures = {}
                if not passed:
                    failures['task_success'] = {'record': f'{where}/agents/{key}/grade.json', 'failed_checks': g.get('failed_checks')}
                if not teardown_ok:
                    failures['teardown_at_exit'] = {'record': f'{where}/run.json#agents.{key}.left_running', 'trace': trace}
                if not isolation_ok:
                    failures['isolation'] = {'record': f'{where}/run.json', 'events': culprit_of or unattributed, 'trace': trace}
                if not cli_ok:
                    failures['clean_cli_exit'] = {'record': f'{where}/run.json#agents.{key}.exit', 'trace': trace,
                                                  'exit': a.get('exit'), 'result': result.get('subtype')}
                slot_rows.append({**base, 'task': task, 'session': key, 'role': a.get('role', 'agent'), 'status': 'scored',
                                  'is_victim': task == run.get('victim'),
                                  'passed': passed, 'teardown_ok': teardown_ok, 'isolation_ok': isolation_ok, 'cli_ok': cli_ok,
                                  'gated_success': passed and teardown_ok and isolation_ok and cli_ok,
                                  'used_own_db': bool(db.get('exists')) and bool(own_new & set(db.get('versions') or [])),
                                  'residue_ok': t.get('residue_ok'), 'false_confirmation': t.get('false_confirmation'),
                                  'down_exit': t.get('down_exit'), 'down_exit_first': t.get('down_exit_first'),
                                  'stack_rounds': len(t.get('rounds') or []),
                                  'named_pids': [d.get('pid') for r in t.get('rounds') or [] for d in r.get('named') or []],
                                  'left_after_down': len((t.get('after_down') or {}).get('processes') or []),
                                  'committed': (a.get('compliance') or {}).get('committed'), 'ran_tests': (a.get('compliance') or {}).get('ran_tests'),
                                  'timed_out': bool(a.get('timed_out')), 'seconds': a.get('seconds'), 'cost_usd': result.get('total_cost_usd'),
                                  'turns': result.get('num_turns'), 'tool_calls': m.get('tool_calls'), 'bash_calls': m.get('bash_calls'),
                                  'tool_errors': m.get('tool_errors'), 'env_errors': m.get('env_errors'),
                                  'kill_commands': len(m.get('kill_commands') or []), 'benchmark_access': len(m.get('benchmark_access') or []),
                                  'failed_checks': g.get('failed_checks'), 'gate_failures': failures, 'trace': trace})
                if not t.get('residue_ok', True):
                    slot_rows[-1]['gate_failures']['teardown_residue'] = {
                        'record': f'{where}/run.json#teardown.{task}', 'after_down': (t.get('after_down') or {}).get('processes'),
                        'ports': (t.get('after_down') or {}).get('ports'), 'after_removal': t.get('after_removal')}
            rows += slot_rows
            scored = [r for r in slot_rows if r['status'] == 'scored']
            survivors = [r for r in scored if not r['is_victim']]
            victim = next((r for r in scored if r['is_victim']), None)
            others_untouched = not events
            residue_all = all(r.get('residue_ok') for r in scored) and len(scored) == len(plan['config']['tasks'])
            episode = residue_all and others_untouched and all(r['gated_success'] for r in scored) and len(scored) == len(plan['config']['tasks'])
            killed = next((dict(a, key=k) for k, a in (run.get('agents') or {}).items() if a.get('killed_by_harness')), None)
            run_rows.append({**base, 'status': 'scored', 'record': f'{where}/run.json',
                             'injected': fault.get('injected'), 'missed': fault.get('missed'),
                             'kill_after_launch_s': (fault.get('kill') or {}).get('seconds_after_launch'),
                             'kill_after_trigger_s': (fault.get('kill') or {}).get('seconds_after_trigger'),
                             'trigger': {k: (fault.get('trigger') or {}).get(k) for k in ('seconds_after_launch', 'pid', 'port', 'command')},
                             'running_at_kill': [p['command'][:100] for p in (fault.get('running_at_kill') or {}).get('processes') or []],
                             'running_at_end': [p['command'][:100] for p in (fault.get('running_at_end') or {}).get('processes') or []],
                             'recovery_after_kill_s': (fault.get('recovery') or {}).get('seconds_after_kill'),
                             'killed_session': {'key': killed['key'], 'trace': f"{where}/agents/{killed['key']}/trace.jsonl",
                                                'seconds': killed.get('seconds')} if killed else None,
                             'others_untouched': others_untouched, 'events': events, 'review': review, 'not_events': len(not_events),
                             'residue_all': residue_all, 'residue_victim': victim.get('residue_ok') if victim else None,
                             'episode_success': episode,
                             'survivors_gated': sum(1 for r in survivors if r['gated_success']), 'survivors': len(survivors),
                             'victim_gated': victim['gated_success'] if victim else None,
                             'victim_role': victim['role'] if victim else None,
                             'false_confirmations': sum(1 for r in scored if r.get('false_confirmation')),
                             'machine': run.get('machine')})
    return rows, run_rows


def separation_rate(per_pair):
    """Rates over slots: pooled difference at least 3 slots, and no pair's difference of the opposite sign."""
    pooled = sum(per_pair)
    holds = abs(pooled) >= 3 and all(d * pooled >= 0 for d in per_pair)
    return {'rule': 'rate', 'per_pair_diff': per_pair, 'pooled_diff': pooled, 'rule_holds': holds}


def separation_binary(per_pair):
    """Per-run binary measures: the arms differ in at least 2 pairs, all in the same direction."""
    nonzero = [d for d in per_pair if d]
    holds = len(nonzero) >= 2 and (all(d > 0 for d in nonzero) or all(d < 0 for d in nonzero))
    return {'rule': 'binary', 'per_pair_diff': per_pair, 'pairs_differing': len(nonzero), 'rule_holds': holds}


def separation_continuous(per_pair, stack_values, baseline_values):
    """Continuous: per-pair differences of one sign in every pair, and the median paired difference
    larger than half the larger within-arm interquartile range."""
    iqr = lambda v: (describe(v)['q3'] - describe(v)['q1']) if len(v) >= 2 else 0.0
    larger = max(iqr(stack_values), iqr(baseline_values))
    median = statistics.median(per_pair) if per_pair else None
    same_sign = bool(per_pair) and (all(d > 0 for d in per_pair) or all(d < 0 for d in per_pair))
    holds = same_sign and median is not None and abs(median) > larger / 2
    return {'rule': 'continuous', 'per_pair_diff': [round(d, 3) for d in per_pair], 'median_diff': None if median is None else round(median, 3),
            'larger_within_arm_iqr': round(larger, 3), 'same_sign_all_pairs': same_sign, 'rule_holds': holds}


def mean(values):
    values = [v for v in values if isinstance(v, (int, float))]
    return sum(values) / len(values) if values else None


def summarize_v2(plan, runs, results, foreign=None):
    rows, run_rows = rows_v2(plan, runs, results)
    per_scenario = plan['config'].get('faults', True)
    scenarios = sorted({p.get('scenario') for p in plan['pairs']}, key=lambda s: (s is None, s or ''))
    out = {'plan': plan['name'], 'kind': plan['kind'], 'protocol_version': 2,
           'config': {k: plan['config'][k] for k in plan['config'] if k not in ('agent_command', 'env_allowlist')},
           'agent_command': plan['config']['agent_command'], 'host': plan['host'], 'scenarios': {}, 'rows': rows, 'runs': run_rows,
           'foreign_runs': foreign or {}, 'missing_runs': [f"pair{r['pair']}-{r['arm']}" for r in run_rows if r['status'] == 'not_run'],
           'invalid_runs': [f"pair{r['pair']}-{r['arm']}" for r in run_rows if r['status'] == 'invalid'], 'note': KILL_SCOPE_NOTE}
    for scenario in scenarios:
        pairs = [p for p in plan['pairs'] if p.get('scenario') == scenario]
        indices = {p['index'] for p in pairs}
        srows = [r for r in rows if r['pair'] in indices]
        sruns = [r for r in run_rows if r['pair'] in indices]
        arms = {}
        for arm in ('stack', 'baseline'):
            mine = [r for r in srows if r['arm'] == arm]
            scored = [r for r in mine if r['status'] == 'scored']
            mruns = [r for r in sruns if r['arm'] == arm]
            sruns_scored = [r for r in mruns if r['status'] == 'scored']
            injected = [r for r in sruns_scored if r.get('injected')]
            victims = [r for r in scored if r['is_victim'] and any(x['pair'] == r['pair'] and x.get('injected') for x in sruns_scored)]
            survivors = [r for r in mine if not r.get('is_victim')]
            def run_rate(key, among):
                return {'k': sum(1 for r in among if r.get(key)), 'n': len(among), 'assigned': len(mruns)}
            arms[arm] = {
                'gated_success': rate(mine, 'gated_success'), 'task_success': rate(mine, 'passed'),
                'teardown_at_exit': rate(mine, 'teardown_ok'), 'isolation': rate(mine, 'isolation_ok'), 'cli_ok': rate(mine, 'cli_ok'),
                'used_own_db': rate(mine, 'used_own_db'), 'committed': rate(mine, 'committed'), 'ran_tests': rate(mine, 'ran_tests'),
                'timed_out': rate(mine, 'timed_out'),
                'survivors_finish': rate(survivors, 'gated_success'),
                'residue_worktrees': rate(mine, 'residue_ok'),
                'residue_victim_worktree': run_rate('residue_victim', injected),
                'others_untouched': run_rate('others_untouched', sruns_scored),
                'episode_success': run_rate('episode_success', sruns_scored),
                'false_confirmations': sum(r['false_confirmations'] for r in sruns_scored),
                'truthful_down': {'false_confirmations': sum(r['false_confirmations'] for r in sruns_scored),
                                  'downs': sum(1 for r in scored if r.get('down_exit') is not None)},
                'stack_rounds_total': sum(r.get('stack_rounds') or 0 for r in scored),
                'seconds': describe([r['seconds'] for r in scored]), 'tool_calls': describe([r['tool_calls'] for r in scored]),
                'turns': describe([r['turns'] for r in scored]), 'cost_usd': describe([r['cost_usd'] for r in scored]),
                'tool_errors': describe([r['tool_errors'] for r in scored]), 'env_errors': describe([r['env_errors'] for r in scored]),
                'kill_commands_total': sum(r['kill_commands'] for r in scored),
                'benchmark_access_total': sum(r['benchmark_access'] for r in scored),
                'faults': {'injected': len(injected), 'missed': [{'pair': r['pair'], 'reason': r['missed']} for r in sruns_scored if r.get('missed')],
                           'kill_after_launch_s': describe([r['kill_after_launch_s'] for r in injected]),
                           'kill_after_trigger_s': describe([r['kill_after_trigger_s'] for r in injected])},
                'not_run': sum(1 for r in mine if r['status'] == 'not_run'), 'invalid': sum(1 for r in mine if r['status'] == 'invalid'),
            }
            if scenario == 'agent-kill':
                arms[arm]['recovery'] = {'gated_success': {'k': sum(1 for r in victims if r['gated_success'] and r['role'] == 'recovery'),
                                                           'n': len(victims), 'assigned': len(mruns)},
                                         **{k: describe([r[k] for r in victims]) for k in ('seconds', 'tool_calls', 'turns', 'tool_errors', 'cost_usd')}}
            if scenario == 'service-kill':
                others = [r for r in scored if not r['is_victim']]
                arms[arm]['victim_copes'] = {
                    'gated_success': {'k': sum(1 for r in victims if r['gated_success']), 'n': len(victims), 'assigned': len(mruns)},
                    **{k: describe([r[k] for r in victims]) for k in ('seconds', 'tool_calls', 'turns', 'tool_errors')},
                    'by_victim': [{'pair': r['pair'], 'task': r['task'], 'gated_success': r['gated_success'], 'seconds': r['seconds'],
                                   'tool_calls': r['tool_calls'], 'same_task_not_victim': {
                                       'gated': [o['gated_success'] for o in others if o['task'] == r['task']],
                                       'seconds': [o['seconds'] for o in others if o['task'] == r['task']],
                                       'tool_calls': [o['tool_calls'] for o in others if o['task'] == r['task']]}} for r in victims],
                    'note': 'the same task in pairs where it was not the victim is description only'}
            arms[arm]['machine'] = [r.get('machine') for r in sruns_scored]
        # Paired comparisons within the scenario: only pairs with both runs valid and (with faults) the fault injected in both.
        comparable, excluded = [], []
        for p in pairs:
            both = {r['arm']: r for r in sruns if r['pair'] == p['index']}
            reasons = [f"{arm}: {r['status']}" for arm, r in both.items() if r['status'] != 'scored'] + \
                      [f"{arm}: fault missed ({r['missed']})" for arm, r in both.items() if r['status'] == 'scored' and scenario and not r.get('injected')]
            if len(both) < 2:
                reasons.append('a run is missing')
            (excluded.append({'pair': p['index'], 'reasons': reasons}) if reasons else comparable.append(p['index']))
        def slots(pair, arm):
            return [r for r in srows if r['pair'] == pair and r['arm'] == arm and r['status'] == 'scored']
        def runrow(pair, arm):
            return next(r for r in sruns if r['pair'] == pair and r['arm'] == arm)
        separation = {}
        for key, label in [('gated_success', 'gated success per slot'), ('passed', 'task success per slot'),
                           ('teardown_ok', 'teardown at exit per slot'), ('residue_ok', 'teardown residue per worktree')]:
            per_pair = [sum(1 for r in slots(i, 'stack') if r.get(key)) - sum(1 for r in slots(i, 'baseline') if r.get(key)) for i in comparable]
            separation[label] = separation_rate(per_pair)
        survivor_diff = [sum(1 for r in slots(i, 'stack') if not r['is_victim'] and r['gated_success'])
                         - sum(1 for r in slots(i, 'baseline') if not r['is_victim'] and r['gated_success']) for i in comparable]
        separation['survivors finish'] = separation_rate(survivor_diff)
        binaries = [('episode_success', 'episode success'), ('others_untouched', 'others untouched'), ('residue_victim', 'residue, victim worktree')]
        if scenario == 'agent-kill':
            binaries.insert(0, ('victim_gated', 'recovery (gated success)'))
        if scenario == 'service-kill':
            binaries.insert(0, ('victim_gated', 'victim copes (gated success)'))
        for key, label in binaries:
            separation[label] = separation_binary([int(bool(runrow(i, 'stack').get(key))) - int(bool(runrow(i, 'baseline').get(key)))
                                                   for i in comparable])
        for key in ('seconds', 'tool_calls', 'turns', 'cost_usd', 'tool_errors'):
            means = {arm: {i: mean([r[key] for r in slots(i, arm)]) for i in comparable} for arm in ('stack', 'baseline')}
            ok = [i for i in comparable if means['stack'][i] is not None and means['baseline'][i] is not None]
            separation[f'{key} (per-run mean)'] = separation_continuous(
                [means['stack'][i] - means['baseline'][i] for i in ok], [means['stack'][i] for i in ok], [means['baseline'][i] for i in ok])
        if scenario in ('agent-kill', 'service-kill'):
            label = 'recovery' if scenario == 'agent-kill' else 'victim'
            for key in ('seconds', 'tool_calls', 'turns', 'cost_usd'):
                vals = {arm: {i: next((r[key] for r in slots(i, arm) if r['is_victim']), None) for i in comparable} for arm in ('stack', 'baseline')}
                ok = [i for i in comparable if isinstance(vals['stack'][i], (int, float)) and isinstance(vals['baseline'][i], (int, float))]
                separation[f'{label} {key}'] = separation_continuous([vals['stack'][i] - vals['baseline'][i] for i in ok],
                                                                     [vals['stack'][i] for i in ok], [vals['baseline'][i] for i in ok])
        for value in separation.values():
            value['pairs_compared'] = len(comparable)
            value['separated'] = bool(value['rule_holds']) and len(comparable) >= 5
            if len(comparable) < 5:
                value['note'] = 'fewer than five pairs compared: not decided'
        # Floor or ceiling in both arms (rates over slots and runs).
        bounds = {}
        for key in ('gated_success', 'task_success', 'teardown_at_exit', 'isolation', 'residue_worktrees', 'survivors_finish',
                    'episode_success', 'others_untouched'):
            fr = [arms[a][key] for a in ('stack', 'baseline')]
            n = lambda x: x.get('scored', x.get('n'))
            if all(n(x) for x in fr):
                if all(x['k'] == n(x) for x in fr):
                    bounds[key] = 'ceiling in both arms'
                elif all(x['k'] == 0 for x in fr):
                    bounds[key] = 'floor in both arms'
        gate_failures = [{'pair': r['pair'], 'arm': r['arm'], 'task': r['task'], 'session': r['session'], 'gates': r['gate_failures']}
                         for r in srows if r.get('gate_failures')]
        run_failures = [{'pair': r['pair'], 'arm': r['arm'], 'record': r['record'], 'others_untouched': r['others_untouched'],
                         'events': r['events']} for r in sruns if r['status'] == 'scored' and not r['others_untouched']]
        out['scenarios'][scenario or 'no-fault'] = {
            'pairs': [p['index'] for p in pairs], 'arms': arms, 'comparable_pairs': comparable, 'excluded_pairs': excluded,
            'separation': separation, 'floor_or_ceiling': bounds, 'gate_failures': gate_failures, 'run_gate_failures': run_failures,
            'review': [{'pair': r['pair'], 'arm': r['arm'], 'losses': r['review']} for r in sruns if r.get('review')],
            'paired': [{'pair': i, 'victim': next(p.get('victim') for p in pairs if p['index'] == i),
                        **{arm: {'gated': sum(1 for r in slots(i, arm) if r['gated_success']), 'episode': runrow(i, arm)['episode_success'],
                                 'victim_gated': runrow(i, arm)['victim_gated'], 'residue_all': runrow(i, arm)['residue_all']}
                           for arm in ('stack', 'baseline')}} for i in comparable],
        }
    return out


def markdown_v2(summary):
    def frac(x):
        n = x.get('scored', x.get('n'))
        return f"{x['k']}/{n} (assigned {x['assigned']})"
    def stat(x):
        return '–' if not x.get('n') else f"median {x['median']} [IQR {x['q1']}–{x['q3']}], range {x['min']}–{x['max']}, n={x['n']}"
    lines = [f"# {summary['plan']} ({summary['kind']}, protocol version 2)", '']
    lines += [f"Missing runs: {summary['missing_runs']}; invalid runs: {summary['invalid_runs']}; runs from another plan ignored: {summary['foreign_runs']}", '']
    for scenario, sc in summary['scenarios'].items():
        a, b = sc['arms']['stack'], sc['arms']['baseline']
        lines += [f'## Scenario: {scenario} (pairs {sc["pairs"]})', '', '| Measure | Stack | Baseline |', '|---|---|---|']
        table = [('Gated success per slot (primary)', 'gated_success', frac), ('Task success (hidden checks)', 'task_success', frac),
                 ('Teardown at exit', 'teardown_at_exit', frac), ('Isolation', 'isolation', frac), ('Clean CLI exit', 'cli_ok', frac),
                 ('Survivors finish (gated, non-victim slots)', 'survivors_finish', frac),
                 ('Teardown residue held, per worktree', 'residue_worktrees', frac),
                 ('Teardown residue held, victim worktree (runs with the fault injected)', 'residue_victim_worktree', frac),
                 ('Others untouched (runs)', 'others_untouched', frac), ('Episode success (runs)', 'episode_success', frac),
                 ('Used its own database', 'used_own_db', frac), ('Committed', 'committed', frac), ('Ran the tests', 'ran_tests', frac),
                 ('Timed out', 'timed_out', frac), ('Seconds per session', 'seconds', stat), ('Tool calls', 'tool_calls', stat),
                 ('Turns', 'turns', stat), ('Tool errors', 'tool_errors', stat), ('Environment errors', 'env_errors', stat),
                 ('Cost USD (CLI-reported, list)', 'cost_usd', stat)]
        for label, key, fn in table:
            lines.append(f'| {label} | {fn(a[key])} | {fn(b[key])} |')
        lines.append(f"| False confirmations by `down` (truthful down) | {a['false_confirmations']} of {a['truthful_down']['downs']} | "
                     f"{b['false_confirmations']} of {b['truthful_down']['downs']} |")
        lines.append(f"| Stack `stop_unconfirmed` follow-up rounds | {a['stack_rounds_total']} | {b['stack_rounds_total']} |")
        lines.append(f"| Broad kill commands | {a['kill_commands_total']} | {b['kill_commands_total']} |")
        lines.append(f"| Benchmark-file access in traces | {a['benchmark_access_total']} | {b['benchmark_access_total']} |")
        lines.append(f"| Not run / invalid slots | {a['not_run']} / {a['invalid']} | {b['not_run']} / {b['invalid']} |")
        if 'recovery' in a:
            lines.append(f"| Recovery: gated success | {frac(a['recovery']['gated_success'])} | {frac(b['recovery']['gated_success'])} |")
            for k in ('seconds', 'tool_calls', 'turns', 'tool_errors', 'cost_usd'):
                lines.append(f"| Recovery: {k} | {stat(a['recovery'][k])} | {stat(b['recovery'][k])} |")
        if 'victim_copes' in a:
            lines.append(f"| Victim copes: gated success | {frac(a['victim_copes']['gated_success'])} | {frac(b['victim_copes']['gated_success'])} |")
            for k in ('seconds', 'tool_calls', 'turns', 'tool_errors'):
                lines.append(f"| Victim copes: {k} | {stat(a['victim_copes'][k])} | {stat(b['victim_copes'][k])} |")
        if scenario != 'no-fault':
            lines.append(f"| Faults injected / missed | {a['faults']['injected']} / {len(a['faults']['missed'])} | "
                         f"{b['faults']['injected']} / {len(b['faults']['missed'])} |")
            lines.append(f"| Seconds from launch to the kill | {stat(a['faults']['kill_after_launch_s'])} | {stat(b['faults']['kill_after_launch_s'])} |")
        lines += ['', f"Floor or ceiling in both arms: {sc['floor_or_ceiling'] or 'none'}",
                  f"Excluded from paired comparisons: {sc['excluded_pairs'] or 'none'}", '',
                  '### Separates the arms? (PROTOCOL.md, Version 2, Analysis)', '',
                  '| Measure | Rule | Per-pair stack − baseline | Inputs | Rule holds | Separated |', '|---|---|---|---|---|---|']
        for label, v in sc['separation'].items():
            inputs = {k: v[k] for k in v if k not in ('rule', 'per_pair_diff', 'rule_holds', 'separated', 'note')}
            lines.append(f"| {label} | {v['rule']} | {v['per_pair_diff']} | {inputs} | {v['rule_holds']} | {v['separated']}"
                         f"{' (' + v['note'] + ')' if v.get('note') else ''} |")
        lines += ['', '### Runs', '', '| Pair | Arm | Pos | Victim | Status | Fault | Kill s | Gated | Survivors | Victim slot | Residue (all / victim) | Others untouched | Episode | False conf. |',
                  '|---|---|---|---|---|---|---|---|---|---|---|---|---|---|']
        for r in [r for r in summary['runs'] if r['pair'] in sc['pairs']]:
            if r['status'] != 'scored':
                lines.append(f"| {r['pair']} | {r['arm']} | {r['position']} | {r.get('victim')} | {r['status']} | | | | | | | | | |")
                continue
            fault = 'none' if not r.get('scenario') else ('injected' if r['injected'] else f"missed: {r['missed']}")
            gated = sum(1 for x in summary['rows'] if x['pair'] == r['pair'] and x['arm'] == r['arm'] and x.get('gated_success'))
            lines.append(f"| {r['pair']} | {r['arm']} | {r['position']} | {r.get('victim')} | {r['status']} | {fault} | {r.get('kill_after_launch_s')} | "
                         f"{gated}/5 | {r['survivors_gated']}/{r['survivors']} | {r.get('victim_role')}: {r.get('victim_gated')} | "
                         f"{r['residue_all']} / {r['residue_victim']} | {r['others_untouched']} | {r['episode_success']} | {r['false_confirmations']} |")
        lines += ['', '### Slots', '', '| Pair | Arm | Task | Session | Pass | Teardown | Isolation | CLI | Residue | Seconds | Tools | Cost | Failed checks |',
                  '|---|---|---|---|---|---|---|---|---|---|---|---|---|']
        for r in [r for r in summary['rows'] if r['pair'] in sc['pairs']]:
            cost = r.get('cost_usd')
            lines.append(f"| {r['pair']} | {r['arm']} | {r['task']} | {r.get('session', '')} | {r.get('passed', r['status'])} | {r.get('teardown_ok', '')} | "
                         f"{r.get('isolation_ok', '')} | {r.get('cli_ok', '')} | {r.get('residue_ok', '')} | {r.get('seconds', '')} | {r.get('tool_calls', '')} | "
                         f"{'' if cost is None else round(cost, 3)} | {', '.join(r.get('failed_checks') or [])} |")
        if sc['gate_failures'] or sc['run_gate_failures']:
            lines += ['', '### Where each gate failure can be seen', '']
            for f in sc['gate_failures']:
                for gate, where in f['gates'].items():
                    lines.append(f"- pair {f['pair']} {f['arm']} `{f['session']}`: **{gate}** — {where.get('record')}"
                                 + (f", trace {where['trace']}" if where.get('trace') else ''))
            for f in sc['run_gate_failures']:
                lines.append(f"- pair {f['pair']} {f['arm']}: **others untouched** — {f['record']}: "
                             + '; '.join(f"{e['kind']} ({e['detail']}) culprits {e['culprits']}" for e in f['events']))
        if sc['review']:
            lines += ['', 'Service losses listed for review (not gating): ' + json.dumps(sc['review'], default=str)[:2000]]
        lines.append('')
    return '\n'.join(lines) + '\n'


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--plan', required=True)
    parser.add_argument('--results', required=True)
    parser.add_argument('--out')
    args = parser.parse_args()
    raw = Path(args.plan).read_bytes()
    plan = json.loads(raw)
    import hashlib
    runs, foreign = load_runs(plan, args.results, hashlib.sha256(raw).hexdigest())
    if plan.get('protocol_version') == 2:
        summary = summarize_v2(plan, runs, args.results, foreign)
        text = markdown_v2(summary)
    else:  # version 1 plans and results keep their own report
        summary = summarize(plan, runs, foreign)
        text = markdown(summary)
    out = Path(args.out or args.results)
    out.mkdir(parents=True, exist_ok=True)
    (out / 'report.json').write_text(json.dumps(summary, indent=2, default=str))
    (out / 'report.md').write_text(text)
    print(text)


if __name__ == '__main__':
    main()
