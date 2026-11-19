#!/usr/bin/env python3
"""Summarize every planned run from its retained run.json records.

  python3 bench/agentvalue/harness/report.py --plan <plan.json> --results <dir> [--out <dir>]

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

KILL_SCOPE_NOTE = 'broad kill commands are counted from the trace; collateral kills are proven only for the main checkout'


def load_runs(plan, results):
    runs = {}
    for pair in plan['pairs']:
        for arm in pair['order']:
            path = Path(results) / f"pair{pair['index']}-{arm}" / 'run.json'
            runs[(pair['index'], arm)] = json.loads(path.read_text()) if path.exists() else None
    return runs


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
    other_changes = [c for c in by.get('changed') or [] if c not in ('migrations', 'tuple_writes', 'columns')]
    if (by.get('changed') and not by.get('foreign_migrations')) or other_changes:
        events.append({'kind': 'main_state_changed', 'detail': by.get('changed'), 'culprits': []})
    if by.get('processes_lost'):
        killers = [t for t, a in agents.items() if (a.get('trace_metrics') or {}).get('kill_commands')]
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
                teardown_ok = not a.get('left_running') and not a.get('left_listening')
                isolation_ok = not culprit and not unattributed
                passed = bool(g.get('passed'))
                rows.append({**row, 'status': 'scored', 'passed': passed, 'teardown_ok': teardown_ok, 'isolation_ok': isolation_ok,
                             'gated_success': passed and teardown_ok and isolation_ok, 'used_own_db': used_own_db,
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
    rng = random.Random(seed)
    boots = sorted(sum(rng.choice(diffs) for _ in diffs) / len(diffs) for _ in range(reps))
    return {'pairs': len(diffs), 'per_pair': [round(d, 3) for d in diffs], 'mean_diff': round(sum(diffs) / len(diffs), 3),
            'ci95': [round(boots[int(0.025 * reps)], 3), round(boots[int(0.975 * reps)], 3)]}


def summarize(plan, runs):
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
            'task_success': rate(mine, 'passed'), 'gated_success': rate(mine, 'gated_success'),
            'teardown_ok': rate(mine, 'teardown_ok'), 'isolation_ok': rate(mine, 'isolation_ok'), 'used_own_db': rate(mine, 'used_own_db'),
            'timed_out': rate(mine, 'timed_out'),
            'episodes_all_gated_success': sum(1 for v in episodes.values() if len(v) == len(plan['config']['tasks']) and all(v)),
            'episodes': len(episodes),
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
            if s and b and s['status'] == b['status'] == 'scored' and s['gated_success'] != b['gated_success']:
                discordant.setdefault(task, []).append({'pair': pair['index'], 'stack': s['gated_success'], 'baseline': b['gated_success']})
    runs_meta = {f'pair{k[0]}-{k[1]}': None if v is None else {
        'valid': v.get('valid'), 'invalid_reasons': v.get('invalid_reasons'), 'seconds': v.get('seconds'),
        'stack_binary_sha256': v.get('stack_binary_sha256'), 'source_rev': v.get('source_rev'),
        'contamination': contamination(v) if v.get('valid') else None,
        'disposal': {k2: (v.get('disposal') or {}).get(k2) for k2 in ['processes_left', 'ports_still_listening', 'stack_records_left']},
        'disposal_state_growth_bytes': ((v.get('disposal') or {}).get('machine_state_bytes_after') or 0) - (v.get('machine_state_bytes_before_agents') or 0),
    } for k, v in runs.items()}
    return {'plan': plan['name'], 'kind': plan['kind'], 'config': {k: plan['config'][k] for k in plan['config'] if k != 'agent_command'},
            'agent_command': plan['config']['agent_command'], 'host': plan['host'], 'arms': arms,
            'paired_gated_success_diff': bootstrap_pairs(plan, rows, 'gated_success'),
            'paired_task_success_diff': bootstrap_pairs(plan, rows, 'passed'),
            'discordant_pairs': discordant, 'runs': runs_meta, 'rows': rows, 'note': KILL_SCOPE_NOTE}


def markdown(summary):
    lines = [f"# {summary['plan']} ({summary['kind']})", '']
    lines += ['| Measure | Stack | Baseline |', '|---|---|---|']
    def frac(x):
        return f"{x['k']}/{x['scored']} (assigned {x['assigned']})"
    def stat(x):
        return '–' if not x.get('n') else f"median {x['median']} [IQR {x['q1']}–{x['q3']}], range {x['min']}–{x['max']}, n={x['n']}"
    a, b = summary['arms']['stack'], summary['arms']['baseline']
    for label, key, fn in [('Task success (hidden checks)', 'task_success', frac), ('Gated success (task + teardown + isolation)', 'gated_success', frac),
                           ('Teardown gate held', 'teardown_ok', frac), ('Isolation gate held', 'isolation_ok', frac),
                           ('Used its own database', 'used_own_db', frac), ('Timed out', 'timed_out', frac),
                           ('Seconds to verified result', 'seconds_to_verified_result', stat), ('Seconds, all scored', 'seconds_all', stat),
                           ('Cost USD (CLI-reported, list)', 'cost_usd', stat), ('Turns', 'turns', stat), ('Tool calls', 'tool_calls', stat),
                           ('Tool errors', 'tool_errors', stat), ('Environment errors in tool output', 'env_errors', stat)]:
        lines.append(f'| {label} | {fn(a[key])} | {fn(b[key])} |')
    lines.append(f"| Episodes with all agents gated-successful | {a['episodes_all_gated_success']}/{a['episodes']} | {b['episodes_all_gated_success']}/{b['episodes']} |")
    lines.append(f"| Broad kill commands | {a['kill_commands_total']} | {b['kill_commands_total']} |")
    lines.append(f"| Benchmark-file access in traces | {a['benchmark_access_total']} | {b['benchmark_access_total']} |")
    lines.append(f"| Not run / invalid agent slots | {a['not_run']} / {a['invalid']} | {b['not_run']} / {b['invalid']} |")
    lines += ['', f"Paired difference in gated success (stack − baseline, per-pair mean, bootstrap over pairs): {summary['paired_gated_success_diff']}",
              f"Paired difference in task success: {summary['paired_task_success_diff']}", f"Discordant pairs: {summary['discordant_pairs']}", '']
    lines += ['| Pair | Arm | Pos | Task | Status | Pass | Teardown | Isolation | Seconds | Cost | Tools | Env err | Failed checks |',
              '|---|---|---|---|---|---|---|---|---|---|---|---|---|']
    for r in summary['rows']:
        lines.append(f"| {r['pair']} | {r['arm']} | {r['position']} | {r['task']} | {r['status']} | {r.get('passed', '')} | {r.get('teardown_ok', '')} | "
                     f"{r.get('isolation_ok', '')} | {r.get('seconds', '')} | {r.get('cost_usd') if r.get('cost_usd') is None else round(r['cost_usd'], 3)} | "
                     f"{r.get('tool_calls', '')} | {r.get('env_errors', '')} | {', '.join(r.get('failed_checks') or [])} |")
    return '\n'.join(lines) + '\n'


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--plan', required=True)
    parser.add_argument('--results', required=True)
    parser.add_argument('--out')
    args = parser.parse_args()
    plan = json.loads(Path(args.plan).read_text())
    summary = summarize(plan, load_runs(plan, args.results))
    out = Path(args.out or args.results)
    (out / 'report.json').write_text(json.dumps(summary, indent=2, default=str))
    (out / 'report.md').write_text(markdown(summary))
    print(markdown(summary))


if __name__ == '__main__':
    main()
