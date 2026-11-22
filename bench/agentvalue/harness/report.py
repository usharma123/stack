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
import time

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
# PROTOCOL.md, "Version 2" (as amended after review round 1): gates, measures and analysis per
# scenario and per arm. Version 1 plans use the functions above, unchanged.

import re
import shlex

V2_NOTE = ('Broad kill commands are counted from the trace. A lost service process is event (g) only when another '
           "session's kill-like command identifies it (its pid, or a pkill/killall pattern matching its command line) "
           'and ran while it was lost; collateral kills of the main checkout are event (c), and services lost while '
           'the harness tears a worktree down are teardown collateral.')
WIDEN_S = 1.0          # the loss interval is widened by 1 s on both sides
OWN_WINDOW_S = 10.0    # X's own down/restart or identifying kill in the 10 s before makes a loss ambiguous
KILL_LIKE = re.compile(r'\b(pkill|killall|fuser\s+-k)\b|\bkill\b[^\n]*\$\(|\blsof\b[^\n]*-t[^\n]*\|\s*xargs\s+kill|\bkill\s+-9\b')


def slot_key(run, task):
    return (run.get('slots') or {}).get(task, task)


UNPARSEABLE = re.compile(r'\$\(|`|\$\{|\$[A-Za-z_]|<\(|>\(')


COMPOUND_WORDS = {'if', 'then', 'else', 'elif', 'fi', 'while', 'until', 'do', 'done', 'case', 'esac', 'for', 'select', '!', '{', '}'}


def simple_commands(command):
    """A shell command split into simple commands: each {'argv', 'before', 'after'} with the operator on
    either side (`;`, `&&`, `||`, `|`, `&`, newline as `;`), plus whether the text is a compound form
    (if/while/case..., a subshell or a group). None when the text uses a form the harness does not
    interpret (command substitution, variables, process substitution) or cannot tokenise."""
    if UNPARSEABLE.search(command):
        return None
    try:
        lexer = shlex.shlex(command.replace('\n', ' ; '), posix=True, punctuation_chars=';&|')
        lexer.whitespace_split = True
        tokens = list(lexer)
    except ValueError:
        return None
    out, current, before = [], [], None
    for tok in tokens:
        if tok in (';', '&&', '||', '|', '&', ';;', '|&'):
            if current:
                out.append({'argv': current, 'before': before, 'after': tok})
            current, before = [], tok
        elif set(tok) <= set(';&|'):
            return None
        else:
            current.append(tok)
    if current:
        out.append({'argv': current, 'before': before, 'after': None})
    compound = any(c['argv'][0] in COMPOUND_WORDS or any('(' in t or ')' in t for t in c['argv']) for c in out)
    for c in out:
        c['compound'] = compound
    return out


def _strip_wrappers(argv):
    """Drop leading `sudo`/`command`/`exec`/`nohup`/`env VAR=…`/`timeout N` wrappers."""
    while argv:
        head = Path(argv[0]).name
        if head in ('sudo', 'command', 'exec', 'nohup'):
            argv = argv[1:]
        elif head == 'env':
            argv = argv[1:]
            while argv and ('=' in argv[0] or argv[0].startswith('-')):
                argv = argv[1:]
        elif head == 'timeout' and len(argv) > 1:
            argv = argv[2:]
        else:
            break
    return argv


KILL_FLAGS = {'-9', '-KILL', '-TERM', '-15'}


def _kill_operands(argv):
    """(modelled, operands) for `kill [-9|-KILL|-TERM|-15|-s NAME] pid...`; any other flag is unmodelled."""
    modelled, operands, i = True, [], 1
    while i < len(argv):
        arg = argv[i]
        if arg == '-0' or (arg == '-s' and i + 1 < len(argv) and argv[i + 1] in ('0', 'SIG0')):
            return 'null', []          # signal 0 delivers nothing: ignored as a culprit and as an own command
        if arg == '-s' and i + 1 < len(argv):
            i += 2
            continue
        if arg in KILL_FLAGS:
            i += 1
            continue
        if arg.startswith('-'):
            modelled = False
            i += 1
            continue
        operands.append(arg)
        if not arg.isdigit():
            modelled = False
        i += 1
    return modelled, operands


def _pattern_args(argv, allowed):
    """(modelled, flags, pattern) for pkill/killall: flags outside `allowed` are unmodelled."""
    flags, pattern = [], None
    for arg in argv[1:]:
        if arg.startswith('-') and pattern is None:
            flags.append(arg)
        elif pattern is None:
            pattern = arg
        else:
            return False, flags, pattern       # more operands than modelled
    return all(f in allowed for f in flags), flags, pattern


def _match(pattern, text):
    try:
        return re.search(pattern, text) is not None
    except re.error:
        return pattern in text


GATE = ('pid', 'pattern', 'port')


def identifies(command, loss):
    """Whether a command explains the loss of a process. Returns 'pid', 'pattern' or 'port' only for a
    plain, unconditional, modelled invocation: `kill [-9|-KILL|-TERM|-15|-s NAME] pid...` naming its pid;
    `pkill [-9] [-f] pattern` matching it; `killall name` naming it; `lsof -t -i :PORT | xargs kill` or
    `fuser -k PORT/tcp` on a port it listened on — as a simple command not behind `&&`/`||`, not in a
    pipeline (other than the lsof form) and not inside a compound form or subshell. 'review' when a
    form the harness does not model could explain it; None otherwise."""
    full = loss.get('command') or ''
    name = Path(full.split()[0]).name if full.split() else ''
    ports = [str(p) for p in loss.get('ports') or []]
    parsed = simple_commands(command)
    if parsed is None:   # substitution, variables...: a kill there could explain it, but is never gated
        return 'review' if re.search(r'\b(kill|pkill|killall|fuser)\b', command) else None
    verdict = None
    for index, c in enumerate(parsed):
        # Inside a compound form the keywords and parentheses are dropped for reading; such a kill is
        # never plain, so it can at most be listed for review.
        argv = [t.strip('(){}') for t in c['argv']]
        while argv and (argv[0] in COMPOUND_WORDS or not argv[0]):
            argv = argv[1:]
        argv = _strip_wrappers([t for t in argv if t])
        if not argv:
            continue
        base = Path(argv[0]).name
        plain = not c['compound'] and c['before'] not in ('&&', '||', '|') and c['after'] != '|'
        hit = None
        if base == 'kill':
            modelled, operands = _kill_operands(argv)
            if modelled != 'null' and str(loss['pid']) in operands:
                hit = 'pid' if modelled and plain and len(operands) == 1 else 'review'
        elif base == 'pkill':
            modelled, flags, pattern = _pattern_args(argv, {'-9', '-f'})
            if pattern is not None and _match(pattern, full if '-f' in flags else name):
                hit = 'pattern' if modelled and plain else 'review'
            elif not modelled and any(_match(a, full) for a in argv[1:] if not a.startswith('-')):
                hit = 'review'
        elif base == 'killall':
            modelled, flags, pattern = _pattern_args(argv, set())
            if pattern is not None and (pattern == name or (not modelled and _match(pattern, full))):
                hit = 'pattern' if modelled and plain and pattern == name else 'review'
        elif base == 'fuser' and any(a.startswith('-') and 'k' in a for a in argv[1:]):
            if any(re.fullmatch(rf'{p}(/tcp)?', a) for a in argv[1:] for p in ports):
                only = all(a in ('-k',) or re.fullmatch(r'\d+(/tcp)?', a) for a in argv[1:])
                hit = 'port' if only and plain else 'review'
        elif base == 'lsof' and c['after'] == '|' and index + 1 < len(parsed):
            nxt = parsed[index + 1]
            nargv = _strip_wrappers(nxt['argv'])
            if nargv and Path(nargv[0]).name == 'xargs' and 'kill' in nargv[1:]:
                text = ' '.join(argv[1:])
                if any(re.search(rf'(?:^|\s)-t?i\s*(?:tcp)?:?{p}\b|:{p}\b', text) for p in ports):
                    lsof_ok = all(re.fullmatch(r'-t|-i|-ti|-it|-sTCP:LISTEN|(?:tcp)?:?\d+|-i(?:tcp)?:\d+|-ti:\d+|-ti(?:tcp)?:\d+', a) for a in argv[1:])
                    xargs_ok = nargv[1:] in (['kill'], ['kill', '-9'], ['kill', '-KILL'], ['kill', '-TERM'])
                    pipe_plain = not c['compound'] and c['before'] not in ('&&', '||', '|') and nxt['after'] != '|'
                    hit = 'port' if lsof_ok and xargs_ok and pipe_plain else 'review'
        if hit in GATE:
            return hit
        if hit == 'review':
            verdict = 'review'
    return verdict


RESTART_NAMES = {'postgres': 'postgres', 'redis': 'redis', 'api': 'api', 'worker': 'worker', 'supervisor': None}


def stops_service(command, loss):
    """An own command that makes a loss ambiguous: the arm's `down` always; a `restart` only when it
    covers the lost service (no service named = all of them; a named list must include it)."""
    parsed = simple_commands(command)
    if parsed is None:
        return bool(re.search(r'\b(?:stack|dev\.py)\b[^\n;&|]*\b(?:down|restart)\b', command))
    kind = loss.get('kind')
    for c in parsed:
        argv = _strip_wrappers(c['argv'])
        words = [Path(a).name for a in argv]
        if 'stack' in words[:1] or 'dev.py' in words[:3]:
            rest = argv[words.index('dev.py') + 1:] if 'dev.py' in words else argv[1:]
            rest = [a for a in rest if not a.startswith('-')]
            if not rest:
                continue
            if rest[0] == 'down':
                return True
            if rest[0] == 'restart':
                named = rest[1:]
                if not named or RESTART_NAMES.get(kind) in named or kind == 'supervisor':
                    return True
    return False


def _lstart_wall(started):
    """Wall time of a `ps` start time ('Thu Nov 19 13:04:48 2026'), floored to the second; None if unreadable."""
    try:
        return time.mktime(time.strptime(started, '%a %b %d %H:%M:%S %Y'))
    except (TypeError, ValueError):
        return None


def _target_clause(command, loss):
    """The simple command of `command` that targets the lost process: the arm's down, a restart
    covering its service, or a kill naming its pid (signal 0 excluded) or matching it by pattern or
    port. Returns the clause, 'unparsed' when the text is not interpreted but names a down/restart, or None."""
    parsed = simple_commands(command)
    if parsed is None:
        return 'unparsed' if stops_service(command, loss) else None
    for c in parsed:
        text = ' '.join(shlex.quote(t) for t in c['argv'])
        if stops_service(text, loss):
            return c
        argv = [t.strip('(){}') for t in c['argv']]
        while argv and (argv[0] in COMPOUND_WORDS or not argv[0]):
            argv = argv[1:]
        argv = _strip_wrappers([t for t in argv if t])
        if argv and Path(argv[0]).name in ('kill', 'pkill', 'killall', 'fuser', 'lsof'):
            probe = identifies(' '.join(shlex.quote(t) for t in argv) + (' | xargs kill' if Path(argv[0]).name == 'lsof' else ''), loss)
            if probe in GATE or probe == 'review' and Path(argv[0]).name == 'kill' and str(loss['pid']) in argv:
                return c
    return None


def own_effect(run, command, loss):
    """The own-command rule (PROTOCOL.md, event (g); used by teardown collateral too):
    'excuses'  the command targeted the lost process, which already existed when it started, and it
               certainly ran: a plain invocation or a member of an `&&` chain, with a result that is not
               an error;
    'may'      it targeted the process but may not have run (behind `||`, `if`, a subshell or a pipeline
               condition, an error result, no result), or the start times cannot be told apart;
    None       it did not target the process, or started before the lost process did (the loss is
               scored as if the command were absent)."""
    clause = _target_clause(command.get('command') or '', loss)
    if clause is None:
        return None
    started_wall = ((run.get('clock') or {}).get('started_wall'))
    process_wall = _lstart_wall(loss.get('started'))
    if process_wall is not None and started_wall is not None and command.get('start_m') is not None:
        command_wall = started_wall + command['start_m']
        if command_wall < process_wall:
            return None                     # the process started after the command: a new incarnation
        if command_wall < process_wall + 1:
            return 'may'                    # within the same second: cannot tell which came first
    if clause == 'unparsed':
        return 'may'
    certain = not clause['compound'] and clause['before'] in (None, ';', '&&') and clause['after'] != '|' \
        and command.get('end_m') is not None and command.get('is_error') is False
    return 'excuses' if certain else 'may'


def _interval(command, agent, episode_end=None):
    start = command.get('start_m')
    end = command.get('end_m')
    if end is None:
        if agent.get('killed_by_harness') and episode_end is not None:
            end = episode_end            # a command of a killed CLI runs until the episode ends
        elif agent.get('ended_m') is not None:
            end = agent['ended_m']
        else:
            end = float('inf')
    return start, end


def _overlaps(command, agent, lo, hi, episode_end=None):
    start, end = _interval(command, agent, episode_end)
    return start is not None and start <= hi and end >= lo


def sibling_losses(run):
    """Event (g) per the amended protocol. Returns (events, review, not_events)."""
    agents = run.get('agents') or {}
    fault = run.get('fault') or {}
    kill = fault.get('kill') or {}
    exempt = (kill.get('pid'), kill.get('started')) if fault.get('injected') and run.get('scenario') == 'service-kill' else None
    episode_end = run.get('episode_end_m')
    events, review, not_events = [], [], []
    for loss in (run.get('sampler') or {}).get('durable_losses') or []:
        group, last, gone = loss['group'], loss['last_seen_m'], loss['gone_m']
        item = {'worktree': group, 'pid': loss['pid'], 'command': (loss.get('command') or '')[:160], 'last_seen_m': last, 'gone_m': gone}
        if group == 'main':
            continue                               # the bystander: version 1 event (c), and teardown collateral
        if episode_end is not None and last >= episode_end:
            continue                               # after the episode: teardown collateral decides
        if exempt and (loss['pid'], loss['started']) == exempt:
            not_events.append({**item, 'reason': 'the injected postmaster'})
            continue
        lo, hi = last - WIDEN_S, gone + WIDEN_S
        culprits, unparseable = set(), set()
        for k, a in agents.items():
            if a.get('marker') == group:
                continue
            for c in a.get('service_commands') or []:
                if not c.get('kill_like') or not _overlaps(c, a, lo, hi, episode_end):
                    continue
                verdict = identifies(c['command'], loss)
                if verdict in GATE and (c.get('end_m') is None or c.get('is_error') is not False):
                    verdict = 'review'      # no result (a killed CLI's included) or an error result: never gated
                if verdict in GATE:
                    culprits.add(k)
                elif verdict == 'review':
                    unparseable.add(k)
        effects = [(k, c, own_effect(run, c, loss)) for k, a in agents.items() if a.get('marker') == group
                   for c in a.get('service_commands') or [] if _overlaps(c, a, last - OWN_WINDOW_S, gone, episode_end)]
        excused = [(k, c) for k, c, e in effects if e == 'excuses']
        uncertain = [(k, c) for k, c, e in effects if e == 'may']
        own = excused + uncertain
        if culprits and not own:
            events.append({'kind': 'sibling_service_lost', 'detail': f"{group}: {item['command'][:80]} (pid {loss['pid']})",
                           'culprits': sorted(culprits), 'loss': item})
        elif culprits and excused:
            review.append({**item, 'reason': 'ambiguous: its own session stopped or killed it just before', 'kill_by': sorted(culprits)})
        elif culprits:
            review.append({**item, 'reason': 'its own session ran a command that may have stopped it (uncertain whether it ran)',
                           'kill_by': sorted(culprits)})
        elif unparseable:
            review.append({**item, 'reason': 'a kill the harness does not gate (conditional, unmodelled, several targets, an error result or no result) could explain it',
                           'kill_by': sorted(unparseable)})
        elif excused:
            not_events.append({**item, 'reason': 'its own session stopped or restarted it', 'command_by': excused[-1][0]})
        elif uncertain:
            review.append({**item, 'reason': 'its own session ran a command that may have stopped it', 'command_by': uncertain[-1][0]})
        else:
            review.append({**item, 'reason': 'no identifying kill command explains it'})
    return events, review, not_events


def connection_events(run):
    """Event (f) with attribution by role: a connection by a process the killed CLI started is charged
    to the killed session (the run-level gate fails, the recovery is not charged); one by the recovery
    session's process to it. A client with no role marker is never assigned by time: the event fails
    the run-level gate only."""
    agents = run.get('agents') or {}
    events = []
    for c in (run.get('sampler') or {}).get('cross_worktree_connections') or []:
        sessions = {k: a for k, a in agents.items() if a.get('marker') == c['client']}
        role = c.get('client_role')
        culprit = next((k for k, a in sessions.items() if a.get('role', 'agent') == role), None) if role else None
        events.append({'kind': 'cross_worktree_connection', 'detail': f"{c['client']} -> {c['server']}:{c['port']} at {c.get('m')} "
                       f"(pid {c.get('client_pid')}, role {role})", 'culprits': [culprit] if culprit else [], 'role': role,
                       'run_level_only': culprit is None})
    return events


SLOW_EXIT_WINDOW_S = 30.0


def service_kind_of(command):
    """The service a durable process is, from its command line (as observe.service_kind)."""
    if re.search(r'/bin/postgres\s+(?:.*\s)?-D\s+\S+', command):
        return 'postgres'
    if re.search(r'(?:^|/)redis-server(?:\s|$)', command):
        return 'redis'
    if re.search(r'\bpython3?\s+app\.py\b', command):
        return 'api'
    if re.search(r'\bpython3?\s+worker\.py\b', command):
        return 'worker'
    if '__supervise' in command:
        return 'supervisor'
    return None


def teardown_collateral(run):
    """Teardown collateral: a durable service of another worktree not yet torn down, or of the main
    checkout, in the harness's listing just before worktree X's teardown procedure and gone from its
    listing just after; or the bystander changed at the post-teardown check. Attributed to the arm.
    Listed for review instead: a loss in a worktree whose own session ran its arm's down, a restart
    or a kill in the 30 s before the episode ended (a slow exit of its own). A bystander already
    unreadable at the end of the episode is an episode observation."""
    events, review = [], []
    end = run.get('episode_end_m')
    agents = run.get('agents') or {}

    def own_slow_exit(v):
        # A command of the lost process's own worktree, in the 30 s before the episode ended, under
        # the own-command rule: ('excuses', command) if it certainly ran and targeted that process,
        # which already existed; ('may', command) if it targeted it but may not have run; None if it
        # started before the process did or did not target it (the loss is scored).
        loss = {'pid': v['pid'], 'started': v.get('started'), 'command': v['command'], 'kind': service_kind_of(v['command']),
                'ports': v.get('ports') or []}
        found = None
        for a in agents.values():
            if a.get('marker') != v['group']:
                continue
            for c in a.get('service_commands') or []:
                if end is None or c.get('start_m') is None or not (end - SLOW_EXIT_WINDOW_S <= c['start_m'] <= end):
                    continue
                effect = own_effect(run, c, loss)
                if effect == 'excuses':
                    return 'excuses', c['command'][:120]
                if effect == 'may':
                    found = ('may', c['command'][:120])
        return found
    for task, t in (run.get('teardown') or {}).items():
        for v in t.get('collateral') or []:
            detail = f"{v['group']}: {v['command'][:80]} (pid {v['pid']}) lost while {SHORT_OF.get(task, task)} was torn down"
            excuse = own_slow_exit(v)
            if excuse:
                kind, command = excuse
                why = 'its own slow exit: its own session stopped it' if kind == 'excuses' else \
                    'its own session ran a command that may have stopped it (uncertain whether it ran)'
                review.append({'worktree': v['group'], 'pid': v['pid'], 'command': v['command'][:160],
                               'reason': f'{detail}; {why} in the 30 s before the episode ended ({command})'})
            else:
                events.append({'kind': 'teardown_collateral', 'detail': detail, 'culprits': ['arm']})
    by = run.get('bystander') or {}
    after = by.get('after_teardown') or {}
    readable_at_end = by.get('fingerprint_after') is not None
    if readable_at_end and (after.get('changed_since_episode') or after.get('unreadable') or after.get('processes_lost')):
        events.append({'kind': 'teardown_collateral', 'detail': f"bystander after the teardown: changed {after.get('changed_since_episode')}, "
                       f"lost {[p['command'][:60] for p in after.get('processes_lost') or []]}", 'culprits': ['arm']})
    return events, review


SHORT_OF = {'invoice-notes': 'wt-notes', 'pay-receipts': 'wt-pay', 'customer-table': 'wt-cust', 'invoice-list': 'wt-list',
            'void-invoices': 'wt-void'}


def contamination_v2(run):
    """Every event of the run: version 1 (a)-(e) with culprits named by session, (f) by role, (g),
    and teardown collateral. Returns (events, review, not_events)."""
    events = []
    for e in contamination(run):
        e = dict(e)
        if e['kind'] == 'cross_worktree_connection':
            continue
        if e['kind'] == 'main_data_in_worktree':
            e['culprits'] = [slot_key(run, c) for c in e['culprits']]
        events.append(e)
    g_events, review, not_events = sibling_losses(run)
    collateral, collateral_review = teardown_collateral(run)
    return events + connection_events(run) + g_events + collateral, review + collateral_review, not_events


def run_dir(results, pair, arm, attempt=1):
    return f"{results}/pair{pair}-{arm}" if attempt == 1 else f"{results}/pair{pair}.{attempt}-{arm}"


def _failed(record):
    return not record.get('valid') or bool((record.get('fault') or {}).get('missed'))


def load_runs_v2(plan, results, plan_sha=None):
    """Every attempt of every pair. A record must name its pair, arm, attempt, scenario, victim and
    plan. Attempt 2 counts only when attempt 1's own records show a missed fault or an invalid run;
    once it has started it governs: if it is invalid, missed or incomplete, the pair is missing.
    Returns (runs used, foreign records, attempts listed, every eligible record)."""
    runs, foreign, attempts, every, governing, partial = {}, {}, {}, [], {}, []
    for pair in plan['pairs']:
        found = {}
        for attempt in (1, 2):
            got = {}
            for arm in pair['order']:
                path = Path(run_dir(results, pair['index'], arm, attempt)) / 'run.json'
                if not path.exists():
                    continue
                run = json.loads(path.read_text())
                bad = run.get('pair') != pair['index'] or run.get('arm') != arm or run.get('model') != plan['config']['model'] \
                    or run.get('plan_kind') != plan['kind'] or run.get('protocol_version') != 2 or run.get('scenario') != pair.get('scenario') \
                    or run.get('victim') != pair.get('victim') or (plan_sha and run.get('plan_sha256') != plan_sha) \
                    or run.get('attempt', 1) != attempt
                if bad:
                    foreign[path.parent.name] = 'does not match the plan, pair, arm or attempt'
                    continue
                got[arm] = run
            if got:
                found[attempt] = got
        marker = Path(results) / f"pair{pair['index']}.2-STARTED.json"
        started_2 = False
        if marker.exists():
            try:
                content = json.loads(marker.read_text())
            except (OSError, ValueError):
                content = None
            started_2 = isinstance(content, dict) and content.get('pair') == pair['index'] and content.get('attempt') == 2 \
                and (not plan_sha or content.get('plan_sha256') == plan_sha)
            if not started_2:
                foreign[marker.name] = 'rerun marker ignored: it does not name this plan digest, pair and attempt 2'
        attempt_1_failed = 1 in found and any(_failed(r) for r in found[1].values())
        eligible_retry = (2 in found or started_2) and attempt_1_failed
        if (2 in found or started_2) and not eligible_retry:
            for arm in found.get(2, {}):
                foreign[Path(run_dir(results, pair['index'], arm, 2)).name] = 'attempt 2 without a missed fault or invalid run in attempt 1'
            found.pop(2, None)
            started_2 = False
        use = 2 if (2 in found or started_2) else 1
        governing[pair['index']] = use
        for attempt in (1, 2):   # sessions of a run that left no record: launched, spend unknown
            for arm in pair['order']:
                adir = Path(run_dir(results, pair['index'], arm, attempt))
                if adir.is_dir() and not (adir / 'run.json').exists() and (attempt == 1 or use == 2):
                    for session in sorted(d.name for d in (adir / 'agents').iterdir()) if (adir / 'agents').is_dir() else []:
                        partial.append({'run': adir.name, 'session': session, 'cost_usd': None, 'valid_run': False})
        listing = []
        for a, got in sorted(found.items()):
            every += list(got.values())
            complete = len(got) == len(pair['order'])
            why = None
            if a == 1 and use == 2:
                why = '; '.join(f"{arm}: " + ('invalid' if not r.get('valid') else f"fault missed ({(r.get('fault') or {}).get('missed')})"
                                              if (r.get('fault') or {}).get('missed') else 'superseded') for arm, r in got.items())
            listing.append({'attempt': a, 'arms': sorted(got), 'used': a == use, 'complete': complete, 'excluded_because': why,
                            'dirs': [Path(run_dir(results, pair['index'], arm, a)).name for arm in got]})
        attempts[pair['index']] = listing
        chosen = found.get(use) or {}
        governs_missing = use == 2 and (len(chosen) < len(pair['order']) or any(_failed(r) for r in chosen.values()))
        if use == 2 and not chosen:
            listing.append({'attempt': 2, 'arms': [], 'used': True, 'complete': False, 'excluded_because': None,
                            'dirs': [], 'note': 'started (recorded) but left no run record'})
        for arm in pair['order']:
            run = None if governs_missing and use == 2 and (arm not in chosen or len(chosen) < len(pair['order'])) else chosen.get(arm)
            if run is not None:
                run['_attempt'] = use
            runs[(pair['index'], arm)] = run
    return runs, foreign, attempts, every, governing, partial


def _norm(path):
    path = str(path or '').rstrip('/')
    return path[len('/private'):] if path.startswith('/private/') else path


def victim_copes(run, row):
    """service-kill: copes = gated success AND service restored (a new postmaster on the worktree's
    own data directory seen listening after the kill and before the agent exits). Reported beside
    it, as description only: whether the agent started an arm test command after the kill and its
    tool result was not an error (shell text cannot prove what ran: `... || true`)."""
    fault = run.get('fault') or {}
    kill = fault.get('kill') or {}
    task = row['task']
    canonical = fault.get('canonical_data_dir') or ((run.get('teardown') or {}).get(task) or {}).get('service_dirs', {}).get('postgres')
    agent = (run.get('agents') or {}).get(row['session']) or {}
    restored = sorted(p['first_listen_m'] for p in (run.get('sampler') or {}).get('postmasters_listening') or []
                      if p['group'] == fault.get('worktree') and (p['pid'], p['started']) != (kill.get('pid'), kill.get('started'))
                      and canonical and _norm(p.get('data_dir')) == _norm(canonical)
                      and kill.get('m') is not None and p['first_listen_m'] > kill['m']
                      and (agent.get('ended_m') is None or p['first_listen_m'] <= agent['ended_m']))
    restored_m = restored[0] if restored else None
    tested = any('test' in c.get('kinds', []) and c.get('start_m') is not None and kill.get('m') is not None and c['start_m'] > kill['m']
                 and c.get('end_m') is not None and c.get('is_error') is False for c in agent.get('service_commands') or [])
    return {'service_restored': restored_m is not None, 'restored_m': restored_m, 'canonical_data_dir': canonical,
            'completed_after_injection': bool(row.get('gated_success')),
            'copes': bool(row.get('gated_success')) and restored_m is not None,
            'description_test_after_kill': bool(tested)}


def rows_v2(plan, runs, results, governing=None):
    """One row per slot (five per run) and one per run. In agent-kill the victim's slot is the
    recovery session. Every row carries the planned victim, invalid and not-run rows included."""
    rows, run_rows, sessions = [], [], []
    for pair in plan['pairs']:
        for arm in pair['order']:
            run = runs.get((pair['index'], arm))
            attempt = (run or {}).get('_attempt') or (governing or {}).get(pair['index'], 1)
            base = {'pair': pair['index'], 'arm': arm, 'scenario': pair.get('scenario'), 'victim': pair.get('victim'),
                    'position': pair['order'].index(arm), 'attempt': attempt}
            where = run_dir(results, pair['index'], arm, attempt)
            missing = run is not None and run.get('valid') and any(
                'grade' not in ((run.get('agents') or {}).get(slot_key(run, t)) or {}) or
                'left_running' not in ((run.get('agents') or {}).get(slot_key(run, t)) or {}) for t in plan['config']['tasks'])
            if run is None or not run.get('valid') or missing:
                status = 'not_run' if run is None else 'invalid'
                reasons = (run or {}).get('invalid_reasons') or (['a valid run with a missing grade or session'] if missing else None)
                run_rows.append({**base, 'status': status, 'reasons': reasons, 'record': f'{where}/run.json'})
                rows += [{**base, 'task': t, 'is_victim': t == pair.get('victim'), 'status': status} for t in plan['config']['tasks']]
                continue
            events, review, not_events = contamination_v2(run)
            unattributed = [e for e in events if not e['culprits'] and not e.get('run_level_only')]
            fault = run.get('fault') or {}
            teardown = run.get('teardown') or {}
            for key, a in (run.get('agents') or {}).items():
                result = (a.get('trace_metrics') or {}).get('result') or {}
                sessions.append({'pair': pair['index'], 'arm': arm, 'scenario': pair.get('scenario'), 'session': key, 'task': a.get('task'),
                                 'role': a.get('role', 'agent'), 'killed': bool(a.get('killed_by_harness')), 'scored': key in (run.get('slots') or {}).values(),
                                 'seconds': a.get('seconds'), 'exit': a.get('exit'), 'cost_usd': result.get('total_cost_usd'),
                                 'tool_calls': (a.get('trace_metrics') or {}).get('tool_calls'), 'turns': result.get('num_turns'),
                                 'model': (a.get('trace_metrics') or {}).get('model'), 'cli': (a.get('trace_metrics') or {}).get('claude_code_version'),
                                 'trace': f'{where}/agents/{key}/trace.jsonl'})
            slot_rows = []
            for task in plan['config']['tasks']:
                key = slot_key(run, task)
                a = (run.get('agents') or {}).get(key) or {}
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
                if t.get('residue_ok') is False:
                    failures['teardown_residue'] = {'record': f'{where}/run.json#teardown.{task}',
                                                    'after_down': (t.get('after_down') or {}).get('processes'),
                                                    'ports': (t.get('after_down') or {}).get('ports'), 'after_removal': t.get('after_removal')}
                row = {**base, 'task': task, 'session': key, 'role': a.get('role', 'agent'), 'status': 'scored',
                       'is_victim': task == run.get('victim'),
                       'passed': passed, 'teardown_ok': teardown_ok, 'isolation_ok': isolation_ok, 'cli_ok': cli_ok,
                       'gated_success': passed and teardown_ok and isolation_ok and cli_ok,
                       'used_own_db': bool(db.get('exists')) and bool(own_new & set(db.get('versions') or [])),
                       'residue_ok': t.get('residue_ok'), 'false_confirmation': t.get('false_confirmation'),
                       'down_first_status': t.get('down_first_status'), 'down_exit_first': t.get('down_exit_first'), 'down_exit': t.get('down_exit'),
                       'clean_after_first_down': t.get('clean_after_first_down'), 'clean_after_procedure': t.get('clean_after_procedure'),
                       'stack_rounds': len(t.get('rounds') or []), 'follow_up_seconds': t.get('follow_up_seconds'),
                       'named_pids': [d.get('pid') for r in t.get('rounds') or [] for d in r.get('named') or []],
                       'signalled': [p for r in t.get('rounds') or [] for p in r.get('signalled') or []],
                       'signals_sent': sum(len((r.get('stopped') or {}).get('terminated') or []) + len((r.get('stopped') or {}).get('killed') or [])
                                           for r in t.get('rounds') or []),
                       'after_down': (t.get('after_down') or {}).get('processes'), 'after_removal': t.get('after_removal'),
                       'committed': (a.get('compliance') or {}).get('committed'), 'ran_tests': (a.get('compliance') or {}).get('ran_tests'),
                       'timed_out': bool(a.get('timed_out')), 'seconds': a.get('seconds'), 'cost_usd': result.get('total_cost_usd'),
                       'turns': result.get('num_turns'), 'tool_calls': m.get('tool_calls'), 'bash_calls': m.get('bash_calls'),
                       'tool_errors': m.get('tool_errors'), 'env_errors': m.get('env_errors'),
                       'kill_commands': len(m.get('kill_commands') or []), 'benchmark_access': len(m.get('benchmark_access') or []),
                       'failed_checks': g.get('failed_checks'), 'gate_failures': failures, 'trace': trace,
                       'seen_not_counted_at_exit': [p['command'][:100] for p in a.get('seen_not_counted') or []]}
                slot_rows.append(row)
            rows += slot_rows
            survivors = [r for r in slot_rows if not r['is_victim']]
            victim = next((r for r in slot_rows if r['is_victim']), None)
            injected = bool(fault.get('injected'))
            copes = victim_copes(run, victim) if victim and injected and run.get('scenario') == 'service-kill' else None
            others_untouched = not events
            residue_all = all(r.get('residue_ok') for r in slot_rows)
            episode = residue_all and others_untouched and all(r['gated_success'] for r in slot_rows)
            if run.get('scenario') == 'service-kill':
                episode = episode and bool(copes and copes['copes'])
            if run.get('scenario') and not injected:
                episode = None   # a missed fault has no episode result
            killed = next((dict(a, key=k) for k, a in (run.get('agents') or {}).items() if a.get('killed_by_harness')), None)
            review = review + [{'worktree': v['group'], 'pid': v['pid'], 'command': v['command'][:160],
                                'reason': f'lost between two teardowns (before {task_name}); not attributed'}
                               for task_name, t in teardown.items() for v in t.get('lost_before_this_teardown') or []]
            run_rows.append({**base, 'status': 'scored', 'record': f'{where}/run.json', 'start_commit': run.get('start_commit'),
                             'injected': injected if run.get('scenario') else None, 'missed': fault.get('missed'),
                             'kill_after_launch_s': (fault.get('kill') or {}).get('seconds_after_launch'),
                             'kill_after_trigger_s': (fault.get('kill') or {}).get('seconds_after_trigger'),
                             'kill_late_flag': (fault.get('kill') or {}).get('late_flag'),
                             'recovery_late_flag': (fault.get('recovery') or {}).get('late_flag'),
                             'trigger': {'seconds_after_launch': (fault.get('trigger') or {}).get('seconds_after_launch'),
                                         'services': {k: {'pid': v.get('pid'), 'port': v.get('port')} for k, v in ((fault.get('trigger') or {}).get('services') or {}).items()}},
                             'running_at_kill': [p['command'][:100] for p in (fault.get('running_at_kill') or {}).get('processes') or []],
                             'outstanding_at_kill': (fault.get('outstanding_at_kill') or {}).get('command') if fault.get('outstanding_at_kill') else None,
                             'handoff': fault.get('handoff'),
                             'running_at_end': [p['command'][:100] for p in (fault.get('running_at_end') or {}).get('processes') or []],
                             'recovery_after_kill_s': (fault.get('recovery') or {}).get('seconds_after_kill'),
                             'killed_session': {'key': killed['key'], 'trace': f"{where}/agents/{killed['key']}/trace.jsonl",
                                                'seconds': killed.get('seconds')} if killed else None,
                             'others_untouched': others_untouched, 'events': events, 'review': review, 'not_events': len(not_events),
                             'residue_all': residue_all, 'residue_victim': victim.get('residue_ok') if victim else None,
                             'episode_success': episode, 'victim_copes': copes,
                             'survivors_gated': sum(1 for r in survivors if r['gated_success']), 'survivors': len(survivors),
                             'victim_gated': victim['gated_success'] if victim else None, 'victim_role': victim['role'] if victim else None,
                             'false_confirmations': sum(1 for r in slot_rows if r.get('false_confirmation')),
                             'downs_exit0': sum(1 for r in slot_rows if r.get('down_first_status') == 'zero'),
                             'downs_nonzero': sum(1 for r in slot_rows if r.get('down_first_status') == 'nonzero'),
                             'downs_timeout': sum(1 for r in slot_rows if r.get('down_first_status') == 'timeout'),
                             'sampler_gaps': (run.get('sampler') or {}).get('gaps_over_3s'),
                             'machine': run.get('machine'),
                             'teardown': {t: {k: (teardown.get(t) or {}).get(k) for k in ('down_exit_first', 'clean_after_first_down', 'rounds',
                                                                                            'follow_up_seconds', 'down_exit', 'clean_after_procedure',
                                                                                            'false_confirmation', 'residue_ok', 'after_down', 'after_removal')}
                                          for t in plan['config']['tasks']}})
    return rows, run_rows, sessions


def separation_rate(per_pair):
    """Rates over slots: pooled difference at least 3 slots, and no pair's difference of the opposite sign."""
    diffs = [d['diff'] for d in per_pair]
    pooled = sum(diffs)
    holds = abs(pooled) >= 3 and all(d * pooled >= 0 for d in diffs)
    return {'rule': 'rate', 'per_pair': per_pair, 'pooled_diff': pooled, 'rule_holds': holds}


def separation_binary(per_pair):
    """Per-run binary measures: the arms differ in at least 2 pairs, all in the same direction."""
    nonzero = [d['diff'] for d in per_pair if d['diff']]
    holds = len(nonzero) >= 2 and (all(d > 0 for d in nonzero) or all(d < 0 for d in nonzero))
    return {'rule': 'binary', 'per_pair': per_pair, 'pairs_differing': len(nonzero), 'rule_holds': holds}


def separation_continuous(per_pair, stack_values, baseline_values):
    """Continuous: per-pair differences of one sign in every pair, and the median paired difference
    larger than half the larger within-arm interquartile range."""
    iqr = lambda v: (describe(v)['q3'] - describe(v)['q1']) if len(v) >= 2 else 0.0
    larger = max(iqr(stack_values), iqr(baseline_values))
    diffs = [d['diff'] for d in per_pair]
    median = statistics.median(diffs) if diffs else None
    same_sign = bool(diffs) and (all(d > 0 for d in diffs) or all(d < 0 for d in diffs))
    holds = same_sign and median is not None and abs(median) > larger / 2
    return {'rule': 'continuous', 'per_pair': [{**d, 'diff': round(d['diff'], 3)} for d in per_pair],
            'median_diff': None if median is None else round(median, 3), 'larger_within_arm_iqr': round(larger, 3),
            'same_sign_all_pairs': same_sign, 'rule_holds': holds}


def mean(values):
    values = [v for v in values if isinstance(v, (int, float)) and not isinstance(v, bool)]
    return sum(values) / len(values) if values else None


def frac_of(rows, key, assigned=None):
    scored = [r for r in rows if r['status'] == 'scored']
    return {'k': sum(1 for r in scored if r.get(key)), 'scored': len(scored), 'assigned': len(rows) if assigned is None else assigned}


def all_sessions(records):
    """Every session launched in every attempt, valid run or not: (key, cost or None)."""
    out = []
    for run in records or []:
        for key, a in (run.get('agents') or {}).items():
            cost = ((a.get('trace_metrics') or {}).get('result') or {}).get('total_cost_usd')
            out.append({'run': f"pair{run.get('pair')}{'.' + str(run.get('attempt')) if run.get('attempt', 1) != 1 else ''}-{run.get('arm')}",
                        'session': key, 'cost_usd': cost if isinstance(cost, (int, float)) else None, 'valid_run': bool(run.get('valid'))})
    return out


def summarize_v2(plan, runs, results, foreign=None, attempts=None, every=None, governing=None, partial=None):
    rows, run_rows, sessions = rows_v2(plan, runs, results, governing)
    scenarios = sorted({p.get('scenario') for p in plan['pairs']}, key=lambda s: (s is None, s or ''))
    launched = all_sessions(every if every is not None else [r for r in runs.values() if r]) + list(partial or [])
    costs = [s['cost_usd'] for s in launched if s['cost_usd'] is not None]
    highest = max(costs) if costs else None
    out = {'plan': plan['name'], 'kind': plan['kind'], 'protocol_version': 2,
           'config': {k: plan['config'][k] for k in plan['config'] if k not in ('agent_command', 'env_allowlist')},
           'agent_command': plan['config']['agent_command'], 'host': plan['host'], 'seed': plan.get('seed'),
           'pairs_planned': plan['pairs'], 'attempts': attempts or {}, 'scenarios': {}, 'rows': rows, 'runs': run_rows,
           'sessions': sessions,
           'cost': {'sessions_launched': len(launched), 'sessions_with_cost': len(costs), 'total_known_usd': round(sum(costs), 4),
                    'highest_session_usd': highest,
                    'stop_rule_mean_usd': round((sum(costs) + (len(launched) - len(costs)) * highest) / len(launched), 4) if launched and costs else None,
                    'stop_rule_status': ('undefined: no session has a cost, so the main run must not start' if launched and not costs else
                                         'no sessions' if not launched else 'defined'),
                    'note': 'every session launched in any attempt, valid or not; for the pilot stop rule an unknown cost (a killed CLI '
                            'writes none) counts as the highest cost seen'},
           'foreign_runs': foreign or {}, 'missing_runs': [f"pair{r['pair']}-{r['arm']}" for r in run_rows if r['status'] == 'not_run'],
           'invalid_runs': [f"pair{r['pair']}-{r['arm']}" for r in run_rows if r['status'] == 'invalid'], 'note': V2_NOTE}
    for scenario in scenarios:
        pairs = [p for p in plan['pairs'] if p.get('scenario') == scenario]
        indices = [p['index'] for p in pairs]
        runrow = lambda i, arm: next(r for r in run_rows if r['pair'] == i and r['arm'] == arm)
        # Which pairs a measure covers: ordinary measures, both runs valid; fault measures, injected in both.
        valid_pairs = [i for i in indices if all(runrow(i, a)['status'] == 'scored' for a in ('stack', 'baseline'))]
        fault_pairs = [i for i in valid_pairs if not scenario or all(runrow(i, a).get('injected') for a in ('stack', 'baseline'))]
        excluded = []
        for i in indices:
            reasons = [f"{a}: {runrow(i, a)['status']}" for a in ('stack', 'baseline') if runrow(i, a)['status'] != 'scored']
            reasons += [f"{a}: fault missed ({runrow(i, a)['missed']})" for a in ('stack', 'baseline')
                        if runrow(i, a)['status'] == 'scored' and scenario and not runrow(i, a).get('injected')]
            if reasons:
                excluded.append({'pair': i, 'reasons': reasons, 'excluded_from': 'ordinary and fault measures' if i not in valid_pairs else 'fault measures'})
        slots = lambda i, arm: [r for r in rows if r['pair'] == i and r['arm'] == arm and r['status'] == 'scored']
        arms = {}
        for arm in ('stack', 'baseline'):
            mine = [r for i in valid_pairs for r in slots(i, arm)]
            assigned = [r for r in rows if r['pair'] in indices and r['arm'] == arm]
            n_assigned = len(indices) * len(plan['config']['tasks'])
            fr = lambda among, key, per=len(plan['config']['tasks']): frac_of(among, key, len(indices) * per)
            fault_runs = [runrow(i, arm) for i in fault_pairs]
            valid_runs = [runrow(i, arm) for i in valid_pairs]
            victims = [r for i in fault_pairs for r in slots(i, arm) if r['is_victim']]
            survivors = [r for r in mine if not r['is_victim']]
            gated = [r for r in mine if r['gated_success']]
            def runs_rate(key, among):
                values = [r.get(key) for r in among if r.get(key) is not None]
                return {'k': sum(1 for v in values if v), 'n': len(values), 'assigned': len([r for r in run_rows if r['pair'] in indices and r['arm'] == arm])}
            effort = lambda among: {k: describe([r[k] for r in among]) for k in ('seconds', 'tool_calls', 'turns', 'tool_errors', 'env_errors', 'cost_usd')}
            arms[arm] = {
                'pairs_covered': {'ordinary': valid_pairs, 'fault': fault_pairs},
                'gated_success': fr(mine, 'gated_success'), 'task_success': fr(mine, 'passed'),
                'teardown_at_exit': fr(mine, 'teardown_ok'), 'isolation': fr(mine, 'isolation_ok'), 'cli_ok': fr(mine, 'cli_ok'),
                'used_own_db': fr(mine, 'used_own_db'), 'committed': fr(mine, 'committed'), 'ran_tests': fr(mine, 'ran_tests'),
                'timed_out': fr(mine, 'timed_out'), 'survivors_finish': fr(survivors, 'gated_success', len(plan['config']['tasks']) - 1),
                'residue_worktrees': fr(mine, 'residue_ok'), 'clean_after_first_down': fr(mine, 'clean_after_first_down'),
                'clean_after_procedure': fr(mine, 'clean_after_procedure'),
                'residue_victim_worktree': runs_rate('residue_victim', fault_runs),
                'others_untouched': runs_rate('others_untouched', valid_runs),
                'episode_success': runs_rate('episode_success', fault_runs if scenario else valid_runs),
                'truthful_down': {'false_confirmations': sum(1 for r in mine if r.get('false_confirmation')),
                                  'downs_exit0': sum(1 for r in mine if r.get('down_first_status') == 'zero'),
                                  'downs_nonzero': sum(1 for r in mine if r.get('down_first_status') == 'nonzero'),
                                  'downs_timeout': sum(1 for r in mine if r.get('down_first_status') == 'timeout')},
                'follow_up': {'rounds': sum(r.get('stack_rounds') or 0 for r in mine), 'signals': sum(r.get('signals_sent') or 0 for r in mine),
                              'seconds': round(sum(r.get('follow_up_seconds') or 0 for r in mine), 2)},
                'effort_all_scored': effort(mine), 'effort_gated_success': effort(gated),
                'kill_commands_total': sum(r['kill_commands'] for r in mine), 'benchmark_access_total': sum(r['benchmark_access'] for r in mine),
                'faults': {'injected': sum(1 for r in run_rows if r['pair'] in indices and r['arm'] == arm and r.get('injected')),
                           'missed': [{'pair': r['pair'], 'reason': r['missed']} for r in run_rows if r['pair'] in indices and r['arm'] == arm and r.get('missed')],
                           'late_kills': [r['pair'] for r in fault_runs if r.get('kill_late_flag')],
                           'late_recoveries': [r['pair'] for r in fault_runs if r.get('recovery_late_flag')],
                           'kill_after_launch_s': describe([r['kill_after_launch_s'] for r in fault_runs])},
                'not_run': sum(1 for r in assigned if r['status'] == 'not_run'), 'invalid': sum(1 for r in assigned if r['status'] == 'invalid'),
            }
            if scenario == 'agent-kill':
                arms[arm]['recovery'] = {'gated_success': {'k': sum(1 for r in victims if r['gated_success'] and r['role'] == 'recovery'), 'n': len(victims),
                                                           'assigned': len(indices)},
                                         **{k: describe([r[k] for r in victims]) for k in ('seconds', 'tool_calls', 'turns', 'tool_errors', 'cost_usd')}}
            if scenario == 'service-kill':
                copes = [runrow(i, arm)['victim_copes'] or {} for i in fault_pairs]
                others = [r for r in mine if not r['is_victim']]
                arms[arm]['victim_copes'] = {
                    'copes': {'k': sum(1 for c in copes if c.get('copes')), 'n': len(copes), 'assigned': len(indices)},
                    'completed_after_injection': {'k': sum(1 for c in copes if c.get('completed_after_injection')), 'n': len(copes), 'assigned': len(indices)},
                    'service_restored': {'k': sum(1 for c in copes if c.get('service_restored')), 'n': len(copes)},
                    'description_test_after_kill': {'k': sum(1 for c in copes if c.get('description_test_after_kill')), 'n': len(copes)},
                    **{k: describe([r[k] for r in victims]) for k in ('seconds', 'tool_calls', 'turns', 'tool_errors')},
                    'by_victim': [{'pair': r['pair'], 'task': r['task'], 'gated_success': r['gated_success'], 'seconds': r['seconds'],
                                   'tool_calls': r['tool_calls'], 'turns': r['turns'], 'tool_errors': r['tool_errors'],
                                   'same_task_not_victim': [{'pair': o['pair'], 'gated': o['gated_success'], 'seconds': o['seconds'],
                                                             'tool_calls': o['tool_calls']} for o in others if o['task'] == r['task']]} for r in victims],
                    'note': 'the same task in the same arm and scenario where it was not the victim is description only'}
            arms[arm]['machine'] = [{'pair': r['pair'], **(r.get('machine') or {})} for r in valid_runs]

        def decided(entry, n_pairs):
            entry['pairs_with_value'] = n_pairs
            entry['pairs_in_scenario'] = len(indices)
            entry['separated'] = bool(entry['rule_holds']) and n_pairs >= 5 and n_pairs == len(indices)
            if not entry['separated'] and not (n_pairs >= 5 and n_pairs == len(indices)):
                entry['note'] = f'not decided: a paired value in {n_pairs} of {len(indices)} pairs (needs all, and at least five)'
            return entry

        separation = {}
        for key, label, among in [('gated_success', 'gated success per slot', None), ('passed', 'task success per slot', None),
                                  ('teardown_ok', 'teardown at exit per slot', None), ('residue_ok', 'teardown residue per worktree', None),
                                  ('gated_success', 'survivors finish', 'survivors')]:
            per_pair = []
            for i in valid_pairs:
                s, b = {r['task']: r for r in slots(i, 'stack')}, {r['task']: r for r in slots(i, 'baseline')}
                common = [t for t in s if t in b and (among != 'survivors' or not s[t]['is_victim'])]
                if common:
                    per_pair.append({'pair': i, 'n': len(common), 'diff': sum(bool(s[t].get(key)) for t in common) - sum(bool(b[t].get(key)) for t in common)})
            separation[label] = decided(separation_rate(per_pair), len(per_pair))
        binaries = [('episode_success', 'episode success', fault_pairs if scenario else valid_pairs), ('others_untouched', 'others untouched', valid_pairs),
                    ('residue_victim', 'residue, victim worktree', fault_pairs)]
        if scenario == 'agent-kill':
            binaries.insert(0, ('victim_gated', 'recovery (gated success)', fault_pairs))
        if scenario == 'service-kill':
            binaries.insert(0, ('copes', 'victim copes', fault_pairs))
        for key, label, among in binaries:
            per_pair = []
            for i in among:
                vals = {}
                for arm in ('stack', 'baseline'):
                    r = runrow(i, arm)
                    vals[arm] = (r.get('victim_copes') or {}).get('copes') if key == 'copes' else r.get(key)
                if None not in vals.values():
                    per_pair.append({'pair': i, 'n': 1, 'diff': int(bool(vals['stack'])) - int(bool(vals['baseline']))})
            separation[label] = decided(separation_binary(per_pair), len(per_pair))
        for subset, label_suffix in (('all', 'all scored sessions'), ('gated', 'sessions with gated success')):
            for key in ('seconds', 'tool_calls', 'turns', 'cost_usd', 'tool_errors'):
                per_pair, sv, bv = [], [], []
                for i in valid_pairs:
                    s, b = {r['task']: r for r in slots(i, 'stack')}, {r['task']: r for r in slots(i, 'baseline')}
                    common = [t for t in s if t in b and isinstance(s[t].get(key), (int, float)) and isinstance(b[t].get(key), (int, float))
                              and (subset == 'all' or (s[t]['gated_success'] and b[t]['gated_success']))]
                    if common:
                        ms, mb = mean([s[t][key] for t in common]), mean([b[t][key] for t in common])
                        per_pair.append({'pair': i, 'n': len(common), 'diff': ms - mb})
                        sv.append(ms)
                        bv.append(mb)
                separation[f'{key} per-run mean, {label_suffix}'] = decided(separation_continuous(per_pair, sv, bv), len(per_pair))
        if scenario in ('agent-kill', 'service-kill'):
            label = 'recovery' if scenario == 'agent-kill' else 'victim'
            for key in ('seconds', 'tool_calls', 'turns', 'cost_usd'):
                per_pair, sv, bv = [], [], []
                for i in fault_pairs:
                    s = next((r[key] for r in slots(i, 'stack') if r['is_victim']), None)
                    b = next((r[key] for r in slots(i, 'baseline') if r['is_victim']), None)
                    if isinstance(s, (int, float)) and isinstance(b, (int, float)):
                        per_pair.append({'pair': i, 'n': 1, 'diff': s - b})
                        sv.append(s)
                        bv.append(b)
                separation[f'{label} {key}'] = decided(separation_continuous(per_pair, sv, bv), len(per_pair))
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
        sruns = [r for r in run_rows if r['pair'] in indices]
        out['scenarios'][scenario or 'no-fault'] = {
            'pairs': indices, 'arms': arms, 'valid_pairs': valid_pairs, 'fault_pairs': fault_pairs, 'excluded_pairs': excluded,
            'separation': separation, 'floor_or_ceiling': bounds,
            'gate_failures': [{'pair': r['pair'], 'arm': r['arm'], 'task': r['task'], 'session': r['session'], 'gates': r['gate_failures']}
                              for r in rows if r['pair'] in indices and r.get('gate_failures')],
            'run_gate_failures': [{'pair': r['pair'], 'arm': r['arm'], 'record': r['record'], 'events': r['events']}
                                  for r in sruns if r['status'] == 'scored' and not r['others_untouched']],
            'review': [{'pair': r['pair'], 'arm': r['arm'], 'losses': r['review']} for r in sruns if r.get('review')],
        }
    return out


def markdown_v2(summary):
    def frac(x):
        n = x.get('scored', x.get('n'))
        return f"{x['k']}/{n} (assigned {x['assigned']})" if 'assigned' in x else f"{x['k']}/{n}"
    def stat(x):
        return '–' if not x.get('n') else f"median {x['median']} [IQR {x['q1']}–{x['q3']}], range {x['min']}–{x['max']}, n={x['n']}"
    c = summary['config']
    lines = [f"# {summary['plan']} ({summary['kind']}, protocol version 2)", '',
             'Raw records stay on the machine that ran the trials; every table a reader needs is below. ' + summary['note'], '',
             '## Plan', '', '| Parameter | Value |', '|---|---|']
    for label, value in [('Stack revision / binary sha256 / version', f"{c.get('stack_rev')} / `{c.get('stack_binary_sha256')}` / {c.get('stack_version')}"),
                         ('Agent model / effort', f"{c.get('model')} / {c.get('effort')}"),
                         ('Agent CLI binary / sha256', f"`{summary['agent_command'][0]}` / `{c.get('agent_cli_sha256')}`"),
                         ('Agent command', '`' + ' '.join(summary['agent_command']) + '`'),
                         ('Concurrency / budget per session', f"{c.get('concurrency')} agents / {c.get('agent_seconds')} s and US${c.get('max_usd_per_agent')}"),
                         ('Fault parameters', c.get('fault')), ('Seed', summary.get('seed')), ('Warm cache', c.get('warm_cache')),
                         ('Agent environment', c.get('environment')), ('Host', {k: summary['host'].get(k) for k in ('macos', 'cpu', 'ncpu', 'memory_bytes', 'claude')})]:
        lines.append(f'| {label} | {value} |')
    lines += ['', '| Pair | Scenario | Victim | Order | Attempts |', '|---|---|---|---|---|']
    for p in summary['pairs_planned']:
        att = summary['attempts'].get(p['index']) or summary['attempts'].get(str(p['index'])) or []
        lines.append(f"| {p['index']} | {p.get('scenario')} | {p.get('victim')} | {' → '.join(p['order'])} | "
                     + ('; '.join(f"#{a['attempt']}{' (used)' if a['used'] else ''}{': ' + a['excluded_because'] if a['excluded_because'] else ''}" for a in att) or '–') + ' |')
    cost = summary['cost']
    lines += ['', f"Missing runs: {summary['missing_runs']}; invalid runs: {summary['invalid_runs']}; runs from another plan ignored: {summary['foreign_runs']}",
              f"Sessions launched: {cost['sessions_launched']}; with a cost: {cost['sessions_with_cost']}; known total US${cost['total_known_usd']}; "
              f"highest session US${cost['highest_session_usd']}; mean per session for the stop rule (unknown = highest): US${cost['stop_rule_mean_usd']} "
              f"({cost['stop_rule_status']})", '']
    for scenario, sc in summary['scenarios'].items():
        a, b = sc['arms']['stack'], sc['arms']['baseline']
        lines += [f'## Scenario: {scenario} (pairs {sc["pairs"]}; ordinary measures over pairs {sc["valid_pairs"]}, fault measures over {sc["fault_pairs"]})', '',
                  '| Measure | Stack | Baseline |', '|---|---|---|']
        table = [('Gated success per slot (primary)', 'gated_success'), ('Task success (hidden checks)', 'task_success'),
                 ('Teardown at exit', 'teardown_at_exit'), ('Isolation', 'isolation'), ('Clean CLI exit', 'cli_ok'),
                 ('Survivors finish (gated, non-victim slots)', 'survivors_finish'), ('Clean after the first `down`', 'clean_after_first_down'),
                 ('Clean after the documented procedure', 'clean_after_procedure'), ('Teardown residue held (gate), per worktree', 'residue_worktrees'),
                 ('Teardown residue held, victim worktree', 'residue_victim_worktree'), ('Others untouched (runs)', 'others_untouched'),
                 ('Episode success (runs; a missed fault has none)', 'episode_success'), ('Used its own database', 'used_own_db'),
                 ('Committed', 'committed'), ('Ran the tests', 'ran_tests'), ('Timed out', 'timed_out')]
        for label, key in table:
            lines.append(f'| {label} | {frac(a[key])} | {frac(b[key])} |')
        for subset, name in (('effort_all_scored', 'all scored'), ('effort_gated_success', 'gated success')):
            for k in ('seconds', 'tool_calls', 'turns', 'tool_errors', 'env_errors', 'cost_usd'):
                lines.append(f'| {k} ({name}) | {stat(a[subset][k])} | {stat(b[subset][k])} |')
        for arm_label, x in (('stack', a), ('baseline', b)):
            pass
        lines.append(f"| False confirmations (of first `down` runs that exited 0) | {a['truthful_down']['false_confirmations']} of {a['truthful_down']['downs_exit0']} | "
                     f"{b['truthful_down']['false_confirmations']} of {b['truthful_down']['downs_exit0']} |")
        lines.append(f"| First `down` exited nonzero / timed out | {a['truthful_down']['downs_nonzero']} / {a['truthful_down']['downs_timeout']} | "
                     f"{b['truthful_down']['downs_nonzero']} / {b['truthful_down']['downs_timeout']} |")
        lines.append(f"| Follow-up: rounds / signals / seconds | {a['follow_up']['rounds']} / {a['follow_up']['signals']} / {a['follow_up']['seconds']} | "
                     f"{b['follow_up']['rounds']} / {b['follow_up']['signals']} / {b['follow_up']['seconds']} |")
        lines.append(f"| Broad kill commands | {a['kill_commands_total']} | {b['kill_commands_total']} |")
        lines.append(f"| Benchmark-file access in traces | {a['benchmark_access_total']} | {b['benchmark_access_total']} |")
        lines.append(f"| Not run / invalid slots | {a['not_run']} / {a['invalid']} | {b['not_run']} / {b['invalid']} |")
        if 'recovery' in a:
            lines.append(f"| Recovery: gated success | {frac(a['recovery']['gated_success'])} | {frac(b['recovery']['gated_success'])} |")
            for k in ('seconds', 'tool_calls', 'turns', 'tool_errors', 'cost_usd'):
                lines.append(f"| Recovery: {k} | {stat(a['recovery'][k])} | {stat(b['recovery'][k])} |")
        if 'victim_copes' in a:
            for k, label in (('copes', 'Victim copes'), ('completed_after_injection', 'Completed after the injection (gated success alone)'),
                             ('service_restored', 'Service restored'),
                             ('description_test_after_kill', 'Ran an arm test after the kill, result not an error (description only, not scored)')):
                lines.append(f"| {label} | {frac(a['victim_copes'][k])} | {frac(b['victim_copes'][k])} |")
            for k in ('seconds', 'tool_calls', 'turns', 'tool_errors'):
                lines.append(f"| Victim: {k} | {stat(a['victim_copes'][k])} | {stat(b['victim_copes'][k])} |")
        if scenario != 'no-fault':
            lines.append(f"| Faults injected / missed | {a['faults']['injected']} / {len(a['faults']['missed'])} | {b['faults']['injected']} / {len(b['faults']['missed'])} |")
            lines.append(f"| Kills / recovery launches more than 1 s late (pairs) | {a['faults']['late_kills']} / {a['faults']['late_recoveries']} | "
                         f"{b['faults']['late_kills']} / {b['faults']['late_recoveries']} |")
            lines.append(f"| Seconds from launch to the kill | {stat(a['faults']['kill_after_launch_s'])} | {stat(b['faults']['kill_after_launch_s'])} |")
        if 'victim_copes' in a:
            lines += ['', 'Victim beside the same task where it was not the victim (description only):', '']
            for arm in ('stack', 'baseline'):
                for v in sc['arms'][arm]['victim_copes']['by_victim']:
                    lines.append(f"- {arm} pair {v['pair']} `{v['task']}`: gated {v['gated_success']}, {v['seconds']} s, {v['tool_calls']} tool calls; "
                                 f"not victim: {v['same_task_not_victim']}")
        lines += ['', f"Floor or ceiling in both arms: {sc['floor_or_ceiling'] or 'none'}", f"Excluded pairs: {sc['excluded_pairs'] or 'none'}", '',
                  '### Separates the arms? (decided only with a paired value in all five pairs)', '',
                  '| Measure | Rule | Per pair (pair: stack − baseline, n tasks) | Inputs | Rule holds | Separated |', '|---|---|---|---|---|---|']
        for label, v in sc['separation'].items():
            inputs = {k: v[k] for k in v if k not in ('rule', 'per_pair', 'rule_holds', 'separated', 'note')}
            per = ', '.join(f"{d['pair']}: {d['diff']} (n={d['n']})" for d in v['per_pair']) or '–'
            lines.append(f"| {label} | {v['rule']} | {per} | {inputs} | {v['rule_holds']} | {v['separated']}{' — ' + v['note'] if v.get('note') else ''} |")
        lines += ['', '### Runs', '']
        for r in [r for r in summary['runs'] if r['pair'] in sc['pairs']]:
            head = f"**pair {r['pair']} {r['arm']}** (position {r['position']}, attempt {r.get('attempt')}, victim {r.get('victim')}): {r['status']}"
            if r['status'] != 'scored':
                lines.append(f"- {head}; reasons: {r.get('reasons')}; record {r['record']}")
                continue
            m = r.get('machine') or {}
            lines.append(f"- {head}; record {r['record']}; starting commit `{r.get('start_commit')}`")
            lines.append(f"  - machine: {m.get('cores')} cores, {m.get('memory_bytes')} bytes, load {m.get('load_1m_before_bystander')} → "
                         f"{m.get('load_1m_after_last_agent')}, {m.get('concurrent_sessions')} sessions, budget {m.get('session_budget')}; sampler gaps over 3 s: {r.get('sampler_gaps')}")
            if r.get('scenario'):
                lines.append(f"  - fault: {'injected' if r.get('injected') else 'missed: ' + str(r.get('missed'))}; trigger {r['trigger']}; kill at "
                             f"{r.get('kill_after_launch_s')} s after launch ({r.get('kill_after_trigger_s')} s after the trigger, late flag {r.get('kill_late_flag')}); "
                             f"outstanding command: {r.get('outstanding_at_kill')!r}; running at the kill: {r.get('running_at_kill')}; at the end: {r.get('running_at_end')}")
                if r.get('recovery_after_kill_s') is not None:
                    lines.append(f"  - recovery launched {r['recovery_after_kill_s']} s after the kill (late flag {r.get('recovery_late_flag')}); "
                                 f"handoff: {r.get('handoff')}")
                if r.get('victim_copes'):
                    lines.append(f"  - victim copes: {r['victim_copes']}")
            lines.append(f"  - gated {sum(1 for x in summary['rows'] if x['pair'] == r['pair'] and x['arm'] == r['arm'] and x.get('gated_success'))}/5, "
                         f"survivors {r['survivors_gated']}/{r['survivors']}, victim slot {r.get('victim_role')}: {r.get('victim_gated')}, residue all/victim "
                         f"{r['residue_all']}/{r['residue_victim']}, others untouched {r['others_untouched']}, episode {r['episode_success']}")
            for task, t in r['teardown'].items():
                lines.append(f"  - teardown `{task}`: first down exit {t['down_exit_first']}, clean after it {t['clean_after_first_down']}; "
                             f"rounds {[(x['round'], 'TERM ' + str((x.get('stopped') or {}).get('terminated')) + ' KILL ' + str((x.get('stopped') or {}).get('killed')), x['down_exit']) for x in t.get('rounds') or []]}, "
                             f"follow-up {t['follow_up_seconds']} s; final exit {t['down_exit']}, clean after the procedure {t['clean_after_procedure']}, "
                             f"false confirmation {t['false_confirmation']}, residue {t['residue_ok']}")
                after_down = t.get('after_down') or {}
                after_removal = t.get('after_removal') or {}
                lines.append(f"    - listed, not counted (not tied to the run): after `down` {[p['command'][:60] for p in after_down.get('seen_not_counted') or []]}, "
                             f"after removal {[p['command'][:60] for p in after_removal.get('seen_not_counted') or []]}")
                lines.append(f"    - observation after `down`: processes {[p['command'][:60] for p in after_down.get('processes') or []]}, "
                             f"ports {after_down.get('ports')}; after removal: worktree directory left {after_removal.get('worktree_dir_exists')}, "
                             f"service data left {after_removal.get('service_data_left')}, processes {[p['command'][:60] for p in after_removal.get('processes') or []]}")
            for e in r['events']:
                lines.append(f"  - event {e['kind']}: {e['detail']}; culprits {e['culprits']}")
        lines += ['', '### Sessions (every launched session, killed ones included)', '',
                  '| Pair | Arm | Session | Role | Killed | Scored | Seconds | Exit | Cost | Tools | Turns | Model | CLI |', '|---|---|---|---|---|---|---|---|---|---|---|---|---|']
        for s in [s for s in summary['sessions'] if s['pair'] in sc['pairs']]:
            lines.append(f"| {s['pair']} | {s['arm']} | {s['session']} | {s['role']} | {s['killed']} | {s['scored']} | {s['seconds']} | {s['exit']} | "
                         f"{s['cost_usd']} | {s['tool_calls']} | {s['turns']} | {s['model']} | {s['cli']} |")
        lines += ['', '### Slots', '', '| Pair | Arm | Task | Session | Pass | Teardown | Isolation | CLI | Residue | Seconds | Tools | Cost | Failed checks |',
                  '|---|---|---|---|---|---|---|---|---|---|---|---|---|']
        for r in [r for r in summary['rows'] if r['pair'] in sc['pairs']]:
            cost_v = r.get('cost_usd')
            lines.append(f"| {r['pair']} | {r['arm']} | {r['task']}{' (victim)' if r.get('is_victim') else ''} | {r.get('session', '')} | {r.get('passed', r['status'])} | "
                         f"{r.get('teardown_ok', '')} | {r.get('isolation_ok', '')} | {r.get('cli_ok', '')} | {r.get('residue_ok', '')} | {r.get('seconds', '')} | "
                         f"{r.get('tool_calls', '')} | {'' if cost_v is None else round(cost_v, 3)} | {', '.join(r.get('failed_checks') or [])} |")
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
            lines += ['', 'Service losses listed for review (not gating): ' + json.dumps(sc['review'], default=str)[:3000]]
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
    if plan.get('protocol_version') == 2:
        runs, foreign, attempts, every, governing, partial = load_runs_v2(plan, args.results, hashlib.sha256(raw).hexdigest())
        summary = summarize_v2(plan, runs, args.results, foreign, attempts, every, governing, partial)
        text = markdown_v2(summary)
    else:  # version 1 plans and results keep their own report
        runs, foreign = load_runs(plan, args.results, hashlib.sha256(raw).hexdigest())
        summary = summarize(plan, runs, foreign)
        text = markdown(summary)
    out = Path(args.out or args.results)
    out.mkdir(parents=True, exist_ok=True)
    (out / 'report.json').write_text(json.dumps(summary, indent=2, default=str))
    (out / 'report.md').write_text(text)
    print(text)


if __name__ == '__main__':
    main()
