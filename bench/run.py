#!/usr/bin/env python3
"""Matched Stack-vs-existing-setup agent task benchmark.

  validate  scripted check that a task cell is gradeable: the starting code fails and the
            reference solution passes through the same grading path agent work uses
  plan      pre-register an experiment (tasks, models, agent command, binary, locale, cache)
  trial     run one planned trial: external agent, then cleanup and independent grading
  review-template  print the transcript review a human must complete for one trial
  report    tabulate every planned trial; missing and invalid trials stay in the denominator

No model, agent or human help is simulated. Validation and harness self-tests are
labelled as such and can never produce publishable agent results.
"""
import argparse
import contextlib
import fcntl
import hashlib
import json
import math
import os
from pathlib import Path
import platform
import random
import re
import shutil
import signal
import subprocess
import sys
import tempfile
import time

import arms
from arms import HERE, TASKS, digest

LOCK = Path('/tmp/stack-agentdx-bench.lock')
DEFAULT_LOCALE = {'LANG': 'en_US.UTF-8'}
INTERVENTION_KINDS = {'clarification', 'hint', 'command_fix', 'environment_repair', 'code_change', 'other'}


class Cancelled(BaseException):
    """SIGTERM/SIGHUP. Not an OSError, so no `except OSError` can swallow it."""


def fingerprints():
    """Hashes of every file that affects a run. Docs and unit tests are excluded."""
    return {str(path.relative_to(HERE)): digest(path) for path in sorted(HERE.rglob('*'))
            if path.is_file() and '__pycache__' not in path.parts and path.suffix not in {'.pyc', '.md'}
            and path.relative_to(HERE).parts[0] != 'tests'}


@contextlib.contextmanager
def host_lock():
    """One benchmark run per host: trials share ports, CPU and caches."""
    with LOCK.open('a') as handle:
        try:
            fcntl.flock(handle, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            raise SystemExit(f'another benchmark run holds {LOCK}; runs are serialized') from None
        yield


STOP_GRACE_SECONDS = 5


def command_tree(process, known):
    """What the command started and is still running, keyed by PID, with identities.

    Members are the leader's process group (the command runs in a new session, so it owns
    that group) and every descendant of the leader or of a process already found, which
    catches children that moved to their own group. Once the leader is reaped its PID may
    be reused; a group led by a process that is not the recorded leader is then foreign.
    """
    rows = arms.process_table()
    leader = known.get(process.pid)
    foreign_group = process.returncode is not None and process.pid in rows and not (
        leader and all(rows[process.pid].get(key) == leader.get(key) for key in ['started', 'command', 'start_ticks']))
    members = {pid: row for pid, row in rows.items() if not foreign_group and row['pgid'] == process.pid}
    parents = {pid for pid, row in known.items() if arms.same_process(row)} | ({process.pid} if process.returncode is None else set())
    parents |= set(members)
    grew = True
    while grew:
        grew = False
        for pid, row in rows.items():
            if pid not in members and row['ppid'] in parents and pid != os.getpid():
                members[pid] = row
                parents.add(pid)
                grew = True
    return members


def stop_tree(process):
    """SIGTERM, then SIGKILL, everything the command started, including after its leader exits.

    Each signal goes to one recorded process after `arms.signal_if_same` has checked its
    start time and command line again, so an unrelated process that later reuses a PID is
    never signalled. Returns what was signalled and anything that survived.
    """
    known, log = {}, {'terminated': [], 'killed': [], 'survivors': []}

    def refresh():
        for pid, row in command_tree(process, known).items():
            known.setdefault(pid, row)

    def signal_all(signum, done):
        for pid, row in known.items():
            if pid not in done and arms.signal_if_same(row, signum):
                done.append(pid)

    for signum, done in [(signal.SIGTERM, log['terminated']), (signal.SIGKILL, log['killed'])]:
        deadline = time.monotonic() + STOP_GRACE_SECONDS
        while True:
            process.poll()  # reap the leader once it exits
            refresh()  # children forked while stopping are found and stopped too
            signal_all(signum, done)
            if time.monotonic() >= deadline or not any(arms.same_process(row) for row in known.values()):
                break
            time.sleep(0.1)
    with contextlib.suppress(subprocess.TimeoutExpired):
        process.wait(timeout=5)
    log['survivors'] = sorted(pid for pid, row in known.items() if arms.same_process(row))
    return log


def invoke(argv, cwd, env, timeout, receipt):
    """Run one command with file-backed output and an immutable receipt.

    An existing receipt is never overwritten: a repeat gets an `-attempt-N` suffix.
    """
    stem, attempt = receipt, 1
    while any(stem.with_name(stem.name + suffix).exists() for suffix in ['.json', '.stdout', '.stderr']):
        attempt += 1
        stem = receipt.with_name(f'{receipt.name}-attempt-{attempt}')
    record = {'argv': argv, 'cwd': str(cwd), 'started_unix_ns': time.time_ns(), 'timed_out': False, 'receipt': stem.name}
    started, process, cancelled = time.monotonic_ns(), None, None
    with stem.with_name(stem.name + '.stdout').open('wb') as out, stem.with_name(stem.name + '.stderr').open('wb') as err:
        try:
            process = subprocess.Popen(argv, cwd=cwd, env=env, stdin=subprocess.DEVNULL, stdout=out, stderr=err,
                                       start_new_session=True)
            try:
                process.wait(timeout=timeout)
            except subprocess.TimeoutExpired:
                record['timed_out'] = True
                record['stopped'] = stop_tree(process)
            record['exit_code'] = process.returncode
        except (Cancelled, KeyboardInterrupt) as error:
            cancelled = error
            record['cancelled'] = type(error).__name__
            if process is not None:
                record['stopped'] = stop_tree(process)
            record['exit_code'] = process.returncode if process is not None else None
        except OSError as error:
            record.update(exit_code=None, launch_error=str(error))
    record['elapsed_ns'] = time.monotonic_ns() - started
    for suffix in ['stdout', 'stderr']:
        record[f'{suffix}_sha256'] = digest(stem.with_name(f'{stem.name}.{suffix}'))
    stem.with_name(stem.name + '.json').write_text(json.dumps(record, indent=2) + '\n')
    if cancelled is not None:
        raise cancelled
    return record


def stdout_of(record, out):
    return (out / f'{record["receipt"]}.stdout').read_text(errors='replace')


def last_json(text):
    for line in reversed(text.strip().splitlines()):
        with contextlib.suppress(ValueError):
            value = json.loads(line)
            if isinstance(value, dict):
                return value
    with contextlib.suppress(ValueError):
        value = json.loads(text)
        if isinstance(value, dict):
            return value
    return None


def result_lines(text):
    """Every stdout line that is a grader result object."""
    found = []
    for line in text.splitlines():
        with contextlib.suppress(ValueError):
            value = json.loads(line)
            if isinstance(value, dict) and 'checks' in value:
                found.append(value)
    return found


def acceptance(task_id, record, out):
    """Pass only when the grader printed exactly one result listing exactly the task's checks, all true.

    A second result line means something other than the grader wrote to its stdout, so the
    receipt is rejected rather than resolved by position.
    """
    results = result_lines(stdout_of(record, out))
    result = results[0] if len(results) == 1 else None
    required = set(TASKS[task_id]['required_checks'])
    checks = (result or {}).get('checks')
    passed = (record.get('exit_code') == 0 and not record.get('timed_out') and isinstance(checks, dict)
              and set(checks) == required and all(value is True for value in checks.values()) and result.get('ok') is True)
    return passed, result


def teardown(arm, workspace, root, env, binary, out, label, seen=None):
    """Stop through the arm's own commands, then look for anything owned still running."""
    project = TASKS[json.loads((root / 'context.json').read_text())['task']]['project']
    seen = seen or {'pids': [], 'ports': []}
    before = arms.residue(arm, workspace, root, seen)
    tracked = {'pids': sorted(set(seen['pids']) | set(before['pids']) | {p['pid'] for p in before['owned_processes']}),
               'ports': sorted(set(seen['ports']) | set(before['ports']))}
    steps = arms.commands(arm, project, binary)
    result = {'before': before, 'steps': {}}
    down = invoke(steps['down'], workspace, env, 120, out / f'{label}-down')
    result['steps']['down'] = down['exit_code']
    down_ok = down['exit_code'] == 0
    if arm == 'stack':
        confirmed = ((last_json(stdout_of(down, out)) or {}).get('data') or {}).get('confirmed')
        result['down_confirmed'] = confirmed
        down_ok = down_ok and confirmed is True
        if (root / 'pf/state').is_dir() and any((root / 'pf/state').iterdir()):
            stop = invoke([str(binary), 'exec', '--', 'pitchfork', 'supervisor', 'stop'], workspace, env, 60, out / f'{label}-supervisor-stop')
            result['steps']['supervisor_stop'] = stop['exit_code']
    deadline = time.monotonic() + 15
    left = arms.residue(arm, workspace, root, tracked)
    while time.monotonic() < deadline and any(left.values()):
        time.sleep(0.25)
        left = arms.residue(arm, workspace, root, tracked)
    # Clean means nothing owned is left. A failing `down` with nothing running is a product
    # observation (`down_ok`), not a leak.
    result.update(left=left, down_ok=down_ok, ok=not any(left.values()))
    if left['owned_processes']:
        # Arm teardown failed to stop these; the harness stops them so trials cannot interfere.
        result['reclaimed_by_harness'] = arms.reclaim(left['owned_processes'], root)
        result['after_reclaim'] = arms.residue(arm, workspace, root, tracked)
    result['disk_after_down'] = arms.disk_usage(root, arm, workspace)
    return result


def new_runtime(prefix, task_id, arm, binary, cache, locale):
    root = Path(tempfile.mkdtemp(prefix=prefix, dir='/tmp')).resolve()
    (root / 'context.json').write_text(json.dumps({'task': task_id, 'arm': arm}))
    bin_dir = arms.install_bin(root, arm, binary)
    env = arms.isolated_env(root, cache, locale, bin_dir)
    return root, env


def grade(task_id, arm, binary, out, label, submission, cache, locale, startup_control=False, full_tree=False):
    """Fresh runtime, grader-only data, submitted files overlaid, then the smoke test and hidden checks.

    `passed` needs both gates. The project's existing smoke test, run through the arm's
    `exec`, shows existing behaviour still works; the hidden acceptance checks show the change
    was made. The documented task loop (`stack run test`, `dev.py test`) is recorded as a
    product observation only, because Stack 0.1.22's `run` breaks it in service apps (R1).

    `outcome` says how grading ended:
      graded                 every step ran, and `passed` is the verdict
      startup_failed         the submitted app did not start, and the reference solution did
                             start and pass in a fresh control runtime: a task failure
      infrastructure_failed  the submitted app did not start, and neither did the reference
      setup_failed           installing the locked tools failed after retries: infrastructure
      harness_error          anything else
    `startup_control` runs that control; validation grades the reference itself and needs none.
    `full_tree` marks `submission` as a whole checkout, so files it deleted are deleted too.
    """
    project = TASKS[task_id]['project']
    root, env = new_runtime('ag.', task_id, arm, binary, cache, locale)
    result = {'runtime': str(root), 'passed': False, 'outcome': 'harness_error'}
    workspace = None
    try:
        workspace = arms.prepare(task_id, arm, root, binary, env)
        if arm == 'stack':
            result['lock_backends'] = arms.lock_backends(workspace)
        hidden = HERE / 'hidden' / project
        if hidden.is_dir():
            for path in hidden.iterdir():
                shutil.copy2(path, workspace / path.name)
        result['submitted_files'] = arms.overlay(submission, workspace, full_tree) if submission else {}
        steps = arms.commands(arm, project, binary, arms.verifier_argv(task_id, workspace))
        timings, result['setup_attempts'] = {}, 0
        for name in ['setup', 'up']:
            for index, argv in enumerate(steps[name]):
                # Setup only installs locked artifacts, so the harness retries it twice; `up`
                # runs the submitted code and gets one attempt.
                attempts = 3 if name == 'setup' else 1
                for attempt in range(attempts):
                    record = invoke(argv, workspace, env, 900, out / f'{label}-{name}{index or ""}')
                    result['setup_attempts'] += name == 'setup'
                    if record['exit_code'] == 0 or attempt == attempts - 1:
                        break
                    time.sleep(5 * (attempt + 1))
                timings[name] = timings.get(name, 0) + record['elapsed_ns'] / 1e9
                if record['exit_code'] != 0:
                    result.update(failed_step=name, failed_receipt=record['receipt'],
                                  outcome='setup_failed' if name == 'setup' else 'startup_failed')
                    raise RuntimeError(f'{name} failed; see {record["receipt"]}')
        result['seconds'] = timings
        for name in ['smoke_exec', 'smoke_task_loop']:
            result[name] = invoke(steps[name], workspace, env, 120, out / f'{label}-{name}')['exit_code'] == 0
        verify = invoke(steps['verify'], workspace, env, 180, out / f'{label}-verify')
        result['acceptance_passed'], result['acceptance'] = acceptance(task_id, verify, out)
        result['passed'] = result['acceptance_passed'] and result['smoke_exec']
        result['outcome'] = 'graded'
    except Exception as error:
        result['error'] = f'{type(error).__name__}: {error}'
    finally:
        if workspace is not None:
            result['cleanup'] = teardown(arm, workspace, root, env, binary, out, f'{label}-cleanup')
    if result['outcome'] == 'startup_failed' and startup_control:
        # After the failed runtime is cleaned up, so the control cannot collide with it.
        control = grade(task_id, arm, binary, out, f'{label}-control', HERE / 'oracles' / project, cache, locale)
        result['startup_control'] = control
        if not (control['outcome'] == 'graded' and control['passed'] is True):
            result['outcome'] = 'infrastructure_failed'
    return result


def validate(args):
    """No-op must fail its designated check; the reference must pass. Both via `grade`."""
    out = Path(args.out).resolve()
    out.mkdir(parents=True, exist_ok=False)
    binary = Path(args.stack).resolve()
    cache = Path(args.cache_root).resolve() if args.cache_root else None
    locale = json.loads(args.locale)
    task = TASKS[args.task]
    summary = {'kind': 'scripted_task_validation', 'task': args.task, 'arm': args.arm, 'locale': locale,
               'cache': str(cache) if cache else 'cold', 'stack_sha256': digest(binary), 'platform': platform.platform(),
               'benchmark_fingerprints': fingerprints(), 'started_unix_ns': time.time_ns()}
    before = arms.globals_snapshot()
    try:
        with host_lock():
            summary['no_op'] = grade(args.task, args.arm, binary, out, 'no-op', None, cache, locale)
            reference = HERE / 'oracles' / task['project']
            summary['reference'] = grade(args.task, args.arm, binary, out, 'reference', reference, cache, locale)
    finally:
        summary['globals_changed'] = arms.globals_diff(before, arms.globals_snapshot())
        no_op, ref = summary.get('no_op', {}), summary.get('reference', {})
        no_op_checks = ((no_op.get('acceptance') or {}).get('checks') or {})
        # The starting code must keep the existing smoke test green and fail the designated
        # hidden check; the reference must pass both gates.
        summary['no_op_rejected'] = (no_op.get('outcome') == 'graded' and no_op.get('passed') is False
                                     and no_op_checks.get(task['no_op_failing_check']) is False)
        summary['reference_passed'] = ref.get('passed') is True
        summary['smoke_exec_before_and_after'] = no_op.get('smoke_exec') is True and ref.get('smoke_exec') is True
        summary['documented_task_loop'] = {'before': no_op.get('smoke_task_loop'), 'after': ref.get('smoke_task_loop')}
        summary['cleanup_ok'] = all((run.get('cleanup') or {}).get('ok') is True for run in [no_op, ref])
        summary['valid'] = (summary['no_op_rejected'] and summary['reference_passed'] and summary['smoke_exec_before_and_after']
                            and summary['cleanup_ok'] and not summary['globals_changed'])
        summary['finished_unix_ns'] = time.time_ns()
        (out / 'summary.json').write_text(json.dumps(summary, indent=2) + '\n')
    return summary


def make_plan(args):
    if len(set(args.tasks)) != len(args.tasks) or len(set(args.models)) != len(args.models):
        raise ValueError('tasks and models must be unique')
    template = json.loads(args.agent_command)
    if not isinstance(template, list) or not template or not all(isinstance(part, str) for part in template):
        raise ValueError('--agent-command must be a nonempty JSON argv list')
    if not any('{model}' in part for part in template):
        raise ValueError('--agent-command must pass the planned {model} explicitly')
    if not any('{prompt}' in part or '{prompt_file}' in part for part in template):
        raise ValueError('--agent-command must pass {prompt} or {prompt_file}')
    selftest = any('selftest_agent.py' in part for part in template)
    if selftest != (args.kind == 'harness_selftest'):
        raise ValueError('the self-test agent belongs only in a harness_selftest plan')
    options = json.loads(args.options)
    locale = json.loads(args.locale)
    if not isinstance(options, dict) or not isinstance(locale, dict):
        raise ValueError('--options and --locale must be JSON objects')
    binding = bind_options(template, options)
    rng = random.Random(args.seed)
    trials = []
    for task in args.tasks:
        for model in args.models:
            flip = rng.getrandbits(1)
            for repetition in range(args.repeats):
                order = list(arms.ARMS) if (repetition + flip) % 2 == 0 else list(reversed(arms.ARMS))
                pair = f'{task}:{model}:{repetition}'
                trials += [{'id': f'{pair}:{arm}', 'pair': pair, 'task': task, 'model': model, 'arm': arm,
                            'repetition': repetition, 'position': position} for position, arm in enumerate(order)]
    binary = Path(args.stack).resolve()
    payload = {'schema': 'stack-agentdx-plan/3', 'kind': args.kind, 'created_unix_ns': time.time_ns(), 'seed': args.seed,
               'platform': args.platform, 'host': platform.platform(), 'cache': args.cache, 'locale': locale,
               'agent_harness': args.harness, 'agent_command': template, 'agent_options': options,
               # Where each option enters argv. With no options the harness runs on its own
               # defaults, which this plan neither records nor controls.
               'agent_options_binding': binding, 'agent_options_controlled': bool(options),
               'human_channel': 'none', 'concurrency': 1, 'budget_seconds': args.budget,
               'stack_binary': str(binary), 'stack_sha256': digest(binary), 'fingerprints': fingerprints(), 'trials': trials}
    with Path(args.out).open('x') as handle:  # A plan is never silently replaced.
        handle.write(json.dumps(payload, indent=2) + '\n')
    return payload


PLACEHOLDER = re.compile(r'\{(prompt|prompt_file|model|workspace|transcript_dir|option:[^{}]*)\}')


def flag_name(text):
    return ''.join(character for character in text.lower() if character.isalnum())


def option_text(value):
    """An option's argv text: strings verbatim, numbers and booleans as JSON. Never translated."""
    return value if isinstance(value, str) else json.dumps(value)


def bind_options(template, options):
    """Check that every planned option reaches the agent's argv, and nothing contradicts it.

    Each option must appear as `{option:NAME}`, and every placeholder must name a planned
    option. A literal flag named like a planned option or `model` (`--reasoning-effort`,
    `--model=...`) must carry that placeholder as its value, so the command cannot quietly
    pass something else. A harness that takes settings any other way needs a wrapper that
    does; otherwise plan no options and the run is labelled as using the harness defaults.
    """
    used = {match[len('option:'):] for part in template for match in PLACEHOLDER.findall(part) if match.startswith('option:')}
    if used - set(options):
        raise ValueError(f'--agent-command uses options not in --options: {sorted(used - set(options))}')
    if set(options) - used:
        raise ValueError(f'--options {sorted(set(options) - used)} never reach the agent; pass each as {{option:NAME}} '
                         'in --agent-command, or plan without it')
    for name, value in options.items():
        if not (isinstance(value, (str, int, float, bool)) and (not isinstance(value, float) or math.isfinite(value))):
            raise ValueError(f'option {name} must be a string, finite number or boolean')
    expected = {flag_name(name): f'{{option:{name}}}' for name in options}
    expected['model'] = '{model}'
    for index, part in enumerate(template):
        flag, equals, inline = part.partition('=')
        if part.startswith('-') and flag_name(flag) in expected:
            value = inline if equals else (template[index + 1] if index + 1 < len(template) else '')
            if value != expected[flag_name(flag)]:
                raise ValueError(f'{part!r} {"" if equals else repr(value) + " "}contradicts or bypasses the planned '
                                 f'{expected[flag_name(flag)]}')
    return {name: [index for index, part in enumerate(template) if f'{{option:{name}}}' in part] for name in options}


def agent_argv(plan, model, values):
    """The planned agent command with every placeholder replaced in one pass."""
    values = {**values, 'model': model, **{f'option:{name}': option_text(value) for name, value in plan['agent_options'].items()}}
    return [PLACEHOLDER.sub(lambda match: values[match.group(1)], part) for part in plan['agent_command']]


def trial_key(identifier):
    return hashlib.sha256(identifier.encode()).hexdigest()[:20]


# Harness git never reads the user's or the agent's hooks, global config or fsmonitor.
GIT_ENV = {'GIT_CONFIG_GLOBAL': os.devnull, 'GIT_CONFIG_NOSYSTEM': '1', 'GIT_TERMINAL_PROMPT': '0'}
GIT = ['git', '-c', 'core.hooksPath=/dev/null', '-c', 'core.fsmonitor=false', '-c', 'core.excludesFile=/dev/null']


def git(argv, cwd, env=None, check=True):
    return subprocess.run([*GIT, *argv], cwd=cwd, env={**os.environ, **GIT_ENV, **(env or {})}, capture_output=True, check=check)


def keep_baseline(workspace, root):
    """Copy the starting commit into a bare repository outside the agent's checkout.

    The agent may commit, amend, reset or garbage-collect in its own `.git`; the submission
    is still diffed against this copy of the commit the trial started from.
    """
    commit = git(['rev-parse', 'HEAD'], workspace).stdout.decode().strip()
    store = root / 'harness' / 'baseline.git'
    store.parent.mkdir()
    git(['clone', '-q', '--bare', '--no-local', str(workspace), str(store)], root)
    return {'commit': commit, 'git_dir': str(store), 'files': arms.tree_files(workspace)}


def submission_record(baseline, workspace):
    """The whole submitted change against the starting commit: commits, edits, deletions and new files.

    The checkout's files (runtime directories excluded, ignored files included, exactly what
    grading overlays) are staged into a scratch index of the baseline repository. Nothing is
    written to the agent's checkout or its `.git`. The agent's own history is read as well.
    """
    stage = Path(tempfile.mkdtemp(prefix='as.', dir='/tmp'))
    try:
        tree = stage / 'tree'
        files = arms.tree_files(workspace)
        for relative, value in files.items():
            target = tree / relative
            target.parent.mkdir(parents=True, exist_ok=True)
            if value.startswith('symlink:'):
                target.symlink_to(value[len('symlink:'):])
            elif value != 'special':
                shutil.copy2(workspace / relative, target)
        tree.mkdir(exist_ok=True)
        env = {'GIT_DIR': baseline['git_dir'], 'GIT_WORK_TREE': str(tree), 'GIT_INDEX_FILE': str(stage / 'index')}
        git(['read-tree', baseline['commit']], tree, env)
        git(['add', '--all', '--force', '.'], tree, env)
        diff = ['diff', '--cached', '--no-renames', '--no-ext-diff', '--no-textconv', baseline['commit']]
        patch = git([*diff, '--binary', '--full-index'], tree, env).stdout
        changed = git([*diff, '--name-status'], tree, env).stdout.decode().splitlines()
    finally:
        shutil.rmtree(stage)
    head = git(['rev-parse', 'HEAD'], workspace, check=False)
    head = head.stdout.decode().strip() if head.returncode == 0 else None
    commits = git(['rev-list', '--reverse', f'{baseline["commit"]}..HEAD'], workspace, check=False)
    descends = git(['merge-base', '--is-ancestor', baseline['commit'], 'HEAD'], workspace, check=False).returncode == 0
    record = {'initial_commit': baseline['commit'], 'baseline_git_dir': baseline['git_dir'], 'changed': changed,
              'agent_head': head, 'head_descends_from_initial': descends,
              'agent_commits': commits.stdout.decode().split() if commits.returncode == 0 else None,
              'files_before': baseline['files'], 'files_after': files}
    return patch, record


def experiment_trial(args):
    plan_path = Path(args.plan).resolve()
    plan = json.loads(plan_path.read_text())
    if fingerprints() != plan['fingerprints']:
        raise SystemExit('benchmark files changed after planning; make a new plan')
    binary = Path(plan['stack_binary'])
    if not binary.is_file() or digest(binary) != plan['stack_sha256']:
        raise SystemExit('the planned Stack binary is missing or changed')
    trial = next((entry for entry in plan['trials'] if entry['id'] == args.id), None)
    if trial is None:
        raise SystemExit('this trial is not in the plan')
    if (plan['cache'] == 'warm') != bool(args.cache_root):
        raise SystemExit('a warm-cache plan needs --cache-root; a cold-cache plan must not have one')
    cache = Path(args.cache_root).resolve() if args.cache_root else None
    out = Path(args.results).resolve() / trial_key(trial['id'])
    out.mkdir(parents=True, exist_ok=False)
    summary = {'kind': 'agent_trial', 'plan_kind': plan['kind'], 'trial': trial, 'plan_sha256': digest(plan_path),
               'started_unix_ns': time.time_ns(), 'host': platform.platform(), 'stack_sha256': plan['stack_sha256'],
               'locale': plan['locale'], 'cache': str(cache) if cache else 'cold', 'task_passed': False, 'valid': False}
    before = arms.globals_snapshot()
    workspace = root = env = None
    try:
        with host_lock():
            root, env = new_runtime('at.', trial['task'], trial['arm'], binary, cache, plan['locale'])
            summary['runtime'] = str(root)
            workspace = arms.prepare(trial['task'], trial['arm'], root, binary, env)
            summary['workspace'] = str(workspace)
            baseline = keep_baseline(workspace, root)
            summary['initial_commit'] = baseline['commit']
            prompt = TASKS[trial['task']]['instruction']
            (out / 'prompt.txt').write_text(prompt)
            (root / 'prompt.txt').write_text(prompt)
            transcript = root / 'transcript'
            transcript.mkdir()
            argv = agent_argv(plan, trial['model'], {'prompt': prompt, 'prompt_file': str(root / 'prompt.txt'),
                                                     'workspace': str(workspace), 'transcript_dir': str(transcript)})
            summary['agent_argv'] = [part.replace(prompt, '<prompt.txt>') for part in argv]
            agent = invoke(argv, workspace, env, plan['budget_seconds'], out / 'agent')
            summary['agent'] = {key: agent.get(key) for key in ['exit_code', 'timed_out', 'elapsed_ns', 'launch_error', 'receipt']}
            summary['finished_agent_unix_ns'] = time.time_ns()
            summary['left_running_at_agent_exit'] = arms.residue(trial['arm'], workspace, root, {})
            patch, submission = submission_record(baseline, workspace)
            (out / 'submission.patch').write_bytes(patch)
            (out / 'submission.json').write_text(json.dumps(submission, indent=2) + '\n')
            summary['changed_files'] = submission['changed']
            shutil.copytree(transcript, out / 'transcript')
            summary['agent_cleanup'] = teardown(trial['arm'], workspace, root, env, binary, out, 'agent-cleanup')
            graded = grade(trial['task'], trial['arm'], binary, out, 'grade', workspace, cache, plan['locale'],
                           startup_control=True, full_tree=True)
            summary['grade'] = graded
            summary['task_passed'] = graded['passed'] is True
            # A submitted app that cannot start, while the reference can, fails the task. Setup,
            # infrastructure and harness failures leave the trial invalid.
            cleanups = [graded.get('cleanup'), (graded.get('startup_control') or {}).get('cleanup', {'ok': True})]
            summary['valid'] = graded['outcome'] in ('graded', 'startup_failed') and all((c or {}).get('ok') is True for c in cleanups)
    except Cancelled as error:
        summary['cancelled'] = str(error)
        raise
    except Exception as error:
        summary['error'] = f'{type(error).__name__}: {error}'
    finally:
        if workspace is not None and not (summary.get('agent_cleanup') or {}).get('ok'):
            summary['final_cleanup'] = teardown(trial['arm'], workspace, root, env, binary, out, 'final-cleanup')
        summary['globals_changed'] = arms.globals_diff(before, arms.globals_snapshot())
        summary['valid'] = summary['valid'] and not summary['globals_changed'] and 'error' not in summary and 'cancelled' not in summary
        summary['finished_unix_ns'] = time.time_ns()
        summary['evidence_sha256'] = {str(path.relative_to(out)): digest(path) for path in sorted(out.rglob('*')) if path.is_file()}
        (out / 'summary.json').write_text(json.dumps(summary, indent=2) + '\n')
    return summary


def review_template(args):
    plan = json.loads(Path(args.plan).read_text())
    trial = next(entry for entry in plan['trials'] if entry['id'] == args.id)
    directory = Path(args.results) / trial_key(trial['id'])
    summary = directory / 'summary.json'
    return {'schema': 'stack-agentdx-review/2', 'trial_id': trial['id'], 'trial_summary_sha256': digest(summary),
            'transcript_sha256': {str(path.relative_to(directory)): digest(path) for path in sorted(directory.glob('agent.std*'))
                                  + sorted((directory / 'transcript').rglob('*')) if path.is_file()},
            'reviewer': None, 'requested_model': trial['model'], 'harness': plan['agent_harness'],
            'requested_options': plan['agent_options'],
            # Observed values come from the transcript or the harness's own receipt, never from the plan.
            'observed_model': None, 'harness_version': None, 'observed_options': None,
            'interventions': [], 'help_requests': [], 'workarounds': [], 'tokens': None, 'cost_usd': None, 'notes': ''}


def review_problems(review, trial, plan, summary_path, directory, result):
    problems = []
    if not isinstance(review, dict) or review.get('schema') != 'stack-agentdx-review/2':
        return ['not a stack-agentdx-review/2 object']
    expect = {'trial_id': trial['id'], 'requested_model': trial['model'], 'observed_model': trial['model'],
              'harness': plan['agent_harness'], 'requested_options': plan['agent_options'],
              'trial_summary_sha256': digest(summary_path)}
    problems += [f'{key} does not match' for key, value in expect.items() if review.get(key) != value]
    observed = review.get('observed_options')
    if plan['agent_options'] and observed != plan['agent_options']:
        problems.append('observed_options must be the settings the transcript shows in effect, equal to the plan')
    elif not plan['agent_options'] and observed is not None and not isinstance(observed, dict):
        problems.append('observed_options must be null or an object')
    if not isinstance(review.get('reviewer'), str) or not review['reviewer'].strip():
        problems.append('reviewer missing')
    if not isinstance(review.get('harness_version'), str) or not review['harness_version'].strip():
        problems.append('harness_version missing')
    hashes = review.get('transcript_sha256')
    if not isinstance(hashes, dict) or 'agent.stdout' not in hashes:
        problems.append('transcript hashes missing')
    else:
        for name, value in hashes.items():
            path = directory / name
            if not path.is_file() or digest(path) != value or result.get('evidence_sha256', {}).get(name) != value:
                problems.append(f'transcript {name} does not match the trial evidence')
    window = (result.get('started_unix_ns', 0), result.get('finished_agent_unix_ns', 0))
    events = review.get('interventions')
    if not isinstance(events, list):
        problems.append('interventions must be a list')
        events = []
    if events and plan.get('human_channel') == 'none':
        problems.append('interventions recorded although the plan gives the agent no human channel')
    for event in events:
        if not (isinstance(event, dict) and event.get('kind') in INTERVENTION_KINDS and isinstance(event.get('reason'), str)
                and isinstance(event.get('active_seconds'), (int, float)) and not isinstance(event.get('active_seconds'), bool)
                and event['active_seconds'] >= 0 and isinstance(event.get('at_unix_ns'), int)
                and window[0] <= event['at_unix_ns'] <= window[1]):
            problems.append(f'malformed intervention {event!r}')
    for key in ['help_requests', 'workarounds']:
        if not isinstance(review.get(key), list) or not all(isinstance(item, str) for item in review[key]):
            problems.append(f'{key} must be a list of strings')
    for key in ['tokens', 'cost_usd']:
        value = review.get(key)
        if value is None:
            continue
        numbers = {k: v for k, v in value.items() if k != 'source'} if isinstance(value, dict) else {}
        if not (isinstance(value, dict) and isinstance(value.get('source'), str) and value['source'].strip() and numbers
                and all(isinstance(v, (int, float)) and not isinstance(v, bool) and v >= 0 for v in numbers.values())):
            problems.append(f'{key} must be null or numbers with a stated source')
    return problems


def evidence_problems(result, trial, plan, plan_path, directory):
    problems = []
    if result.get('kind') != 'agent_trial' or result.get('plan_kind') != plan['kind']:
        problems.append('not an agent trial of this plan')
    if result.get('trial') != trial or result.get('plan_sha256') != digest(plan_path):
        problems.append('trial or plan identity differs')
    if result.get('stack_sha256') != plan['stack_sha256'] or result.get('locale') != plan['locale']:
        problems.append('binary or locale differs from the plan')
    if not result.get('started_unix_ns', 0) >= plan['created_unix_ns']:
        problems.append('started before the plan existed')
    hashes = result.get('evidence_sha256') or {}
    required = {'agent.json', 'agent.stdout', 'agent.stderr', 'submission.patch', 'grade-verify.stdout', 'grade-verify.json'}
    problems += [f'{name} missing' for name in sorted(required - set(hashes))]
    problems += [f'{name} changed' for name, value in hashes.items() if not (directory / name).is_file() or digest(directory / name) != value]
    if result.get('valid') is not True:
        problems.append('trial marked invalid: ' + str(result.get('error') or result.get('grade', {}).get('error') or 'cleanup or globals'))
    if (directory / 'grade-verify.json').is_file():
        verify = json.loads((directory / 'grade-verify.json').read_text())
        passed, _ = acceptance(trial['task'], verify, directory)
        if passed != result.get('task_passed'):
            problems.append('task_passed disagrees with the grader receipt')
    return problems


def report(args):
    plan_path = Path(args.plan).resolve()
    plan = json.loads(plan_path.read_text())
    rows = []
    for trial in plan['trials']:
        directory = Path(args.results).resolve() / trial_key(trial['id'])
        row = {**trial, 'status': 'not_run', 'verified_task': False, 'autonomous': False, 'problems': []}
        summary_path = directory / 'summary.json'
        if summary_path.exists():
            try:
                result = json.loads(summary_path.read_text())
            except ValueError:
                result = {}
            row['problems'] = evidence_problems(result, trial, plan, plan_path, directory)
            review_path = directory / 'review.json'
            review = None
            if review_path.exists():
                with contextlib.suppress(ValueError):
                    review = json.loads(review_path.read_text())
                row['review_problems'] = review_problems(review, trial, plan, summary_path, directory, result)
            agent = result.get('agent') or {}
            left = result.get('left_running_at_agent_exit')
            split = arms.left_services(left) if isinstance(left, dict) else {'services': True, 'supervisor_processes': []}
            row.update(verified_task=result.get('task_passed') is True and not row['problems'],
                       left_running=split['services'], supervisor_left=bool(split['supervisor_processes']),
                       agent_seconds=(agent.get('elapsed_ns') or 0) / 1e9,
                       agent_exit=agent.get('exit_code'), timed_out=agent.get('timed_out'),
                       startup_seconds=(result.get('grade') or {}).get('seconds'))
            if row['problems']:
                row['status'] = 'invalid'
            elif review is None or row.get('review_problems'):
                row['status'] = 'unreviewed' if review is None else 'review_invalid'
            else:
                row['status'] = 'passed' if row['verified_task'] else ('timed_out' if agent.get('timed_out') else 'failed')
                row['autonomous'] = row['verified_task'] and not review['interventions'] and not row['left_running']
                row.update(interventions=len(review['interventions']), help_requests=len(review['help_requests']),
                           workarounds=review['workarounds'], tokens=review['tokens'], cost_usd=review['cost_usd'])
        rows.append(row)
    scored = {'passed', 'failed', 'timed_out'}
    counts = {}
    for arm in arms.ARMS:
        subset = [row for row in rows if row['arm'] == arm]
        counts[arm] = {'assigned': len(subset), 'autonomous': sum(row['autonomous'] for row in subset),
                       'verified_task': sum(row['verified_task'] for row in subset),
                       'left_services_running': sum(bool(row.get('left_running')) for row in subset),
                       'supervisor_left_running': sum(bool(row.get('supervisor_left')) for row in subset),
                       **{status: sum(row['status'] == status for row in subset)
                          for status in ['passed', 'failed', 'timed_out', 'invalid', 'unreviewed', 'review_invalid', 'not_run']}}
    pairs = []
    for pair in dict.fromkeys(row['pair'] for row in rows):
        both = {row['arm']: row for row in rows if row['pair'] == pair}
        if all(both[arm]['status'] in scored for arm in arms.ARMS):
            pairs.append({'pair': pair, 'stack': both['stack']['autonomous'], 'existing': both['existing']['autonomous']})
    complete = all(row['status'] in scored for row in rows)
    return {'kind': 'agent_experiment_report', 'plan_kind': plan['kind'], 'plan_sha256': digest(plan_path),
            'publishable': complete and plan['kind'] == 'agent_experiment',
            'complete': complete, 'counts': counts, 'pairs': pairs,
            'discordant': {'stack_only': sum(p['stack'] and not p['existing'] for p in pairs),
                           'existing_only': sum(p['existing'] and not p['stack'] for p in pairs)},
            'rows': rows,
            'limits': ['Synthetic pilot tasks on one host; descriptive only. No speedup or significance is computed.',
                       'Every assigned trial stays in the denominator; not_run, invalid and unreviewed are not successes.',
                       'Autonomous = verified task, no recorded human help, nothing left running at agent exit.',
                       'Model identity, help and cost come from a human transcript review, not the agent.']}


def main():
    def cancel(signum, frame):
        raise Cancelled(f'signal {signum}')
    for signum in [signal.SIGTERM, signal.SIGHUP]:
        signal.signal(signum, cancel)
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    commands = parser.add_subparsers(dest='command', required=True)
    validation = commands.add_parser('validate')
    validation.add_argument('--task', choices=list(TASKS), required=True)
    validation.add_argument('--arm', choices=arms.ARMS, required=True)
    validation.add_argument('--stack', required=True)
    validation.add_argument('--locale', default=json.dumps(DEFAULT_LOCALE))
    validation.add_argument('--cache-root', help='share tool downloads/installs here (warm); default is cold')
    validation.add_argument('--out', required=True)
    planning = commands.add_parser('plan')
    planning.add_argument('--kind', choices=['agent_experiment', 'harness_selftest'], required=True)
    planning.add_argument('--tasks', nargs='+', choices=list(TASKS), default=list(TASKS))
    planning.add_argument('--models', nargs='+', required=True)
    planning.add_argument('--harness', required=True)
    planning.add_argument('--options', default='{}')
    planning.add_argument('--agent-command', required=True,
                          help='JSON argv; placeholders {model} {prompt} {prompt_file} {workspace} {transcript_dir}')
    planning.add_argument('--stack', required=True)
    planning.add_argument('--platform', required=True)
    planning.add_argument('--locale', default=json.dumps(DEFAULT_LOCALE))
    planning.add_argument('--cache', choices=['cold', 'warm'], required=True)
    planning.add_argument('--seed', type=int, required=True)
    planning.add_argument('--repeats', type=int, default=5)
    planning.add_argument('--budget', type=int, default=1800)
    planning.add_argument('--out', required=True)
    trial = commands.add_parser('trial')
    trial.add_argument('--plan', required=True)
    trial.add_argument('--id', required=True)
    trial.add_argument('--results', required=True)
    trial.add_argument('--cache-root')
    template = commands.add_parser('review-template')
    template.add_argument('--plan', required=True)
    template.add_argument('--id', required=True)
    template.add_argument('--results', required=True)
    reporting = commands.add_parser('report')
    reporting.add_argument('--plan', required=True)
    reporting.add_argument('--results', required=True)
    reporting.add_argument('--out', required=True)
    args = parser.parse_args()
    if args.command == 'validate':
        result = validate(args)
    elif args.command == 'plan':
        if args.repeats < 1 or args.budget < 1:
            parser.error('repeats and budget must be positive')
        result = make_plan(args)
    elif args.command == 'trial':
        result = experiment_trial(args)
    elif args.command == 'review-template':
        result = review_template(args)
    else:
        result = report(args)
        with Path(args.out).open('x') as handle:
            handle.write(json.dumps(result, indent=2) + '\n')
    print(json.dumps(result, indent=2))
    return 1 if result.get('valid') is False else 0


if __name__ == '__main__':
    sys.exit(main())
