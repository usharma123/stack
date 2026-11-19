#!/usr/bin/env python3
"""Stack agent-value benchmark: several agents at once, one worktree each, one repository.

  plan      fix the configuration and the arm order of every pair before any trial runs
  pair      run one planned pair end to end: both arms, in the planned order
  arm       run one arm of a plan (used by `pair`; also for harness self-tests)
  validate  check the harness itself: grader, reference runs in both arms, planted faults
  report    summarize every planned run from its retained records (see report.py)

See ../PROTOCOL.md for what is measured and how each measure is detected.
"""
import argparse
import hashlib
import json
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
import threading
import time

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent
REPO_ROOT = ROOT.parents[1]
sys.path.insert(0, str(HERE))
sys.path.insert(0, str(ROOT.parent))
import arms as legacy  # bench/arms.py
import grade
import observe
import redact

TASKS = json.loads((ROOT / 'tasks.json').read_text())['tasks']
TASK = {t['id']: t for t in TASKS}
SHORT = {'invoice-notes': 'wt-notes', 'pay-receipts': 'wt-pay', 'customer-table': 'wt-cust'}
WARM = Path(os.environ.get('AGENTVALUE_WARM', Path.home() / '.cache/stack-agentvalue/warm'))
GITIGNORE = '.stack/\n.dev/\n.venv/\n.config/mise/\n__pycache__/\n.pytest_cache/\n'
GIT = ['git', '-c', 'user.name=bench', '-c', 'user.email=bench@example.invalid', '-c', 'core.hooksPath=/dev/null',
       '-c', 'init.defaultBranch=main', '-c', 'commit.gpgsign=false']

PROMPT = """You are one of several coding agents working on this repository at the same time. Each agent has its own git worktree and branch. Yours is the current directory, on branch {branch}. Other agents' worktrees, and the main checkout, may have services running while you work.

Your task:
{instruction}

Read README.md first: it explains how to run the project and its services in this worktree. Before you finish, run the tests against your worktree's running services and make sure they pass, and commit your work on your branch. When you are done, stop everything you started in this worktree and leave other worktrees' and the main checkout's services alone."""


def sha256(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def fingerprints():
    """Every file that affects a trial (docs, results and caches excluded)."""
    files = {}
    for path in sorted(ROOT.rglob('*')):
        rel = path.relative_to(ROOT)
        agent_facing = path.suffix == '.md' and (rel.parts[0] == 'arms' or rel.name == 'PROTOCOL.md')
        if path.is_file() and rel.parts[0] not in {'results'} and '__pycache__' not in rel.parts \
                and (path.suffix != '.md' or agent_facing) and '.venv' not in rel.parts:
            files[str(rel)] = sha256(path)
    for extra in ['bench/arms.py']:
        files[extra] = sha256(REPO_ROOT / extra)
    return files


def git_rev():
    return subprocess.run(['git', 'rev-parse', 'HEAD'], cwd=REPO_ROOT, capture_output=True, text=True).stdout.strip()


def tool_versions():
    def out(argv):
        try:
            return subprocess.run(argv, capture_output=True, text=True, timeout=30).stdout.strip().splitlines()[0]
        except (OSError, IndexError, subprocess.TimeoutExpired):
            return None
    return {'claude': out(['claude', '--version']), 'mise': out(['mise', '--version']), 'git': out(['git', '--version']),
            'python_host': out(['python3', '--version']), 'macos': out(['sw_vers', '-productVersion']),
            'cpu': out(['sysctl', '-n', 'machdep.cpu.brand_string']), 'memory_bytes': out(['sysctl', '-n', 'hw.memsize']),
            'ncpu': out(['sysctl', '-n', 'hw.ncpu'])}


def now():
    return round(time.time(), 3)


# ------------------------------------------------------------------ arms

class Arm:
    """How the harness itself drives an arm: bystander start, its addresses, documented removal."""

    def __init__(self, name, stack_binary):
        self.name, self.stack = name, str(stack_binary)

    def files(self, repo):
        project = (ROOT / 'arms/project.md').read_text()
        running = (ROOT / 'arms' / self.name / 'running.md').read_text()
        (repo / 'README.md').write_text(project.rstrip() + '\n\n' + running)
        if self.name == 'stack':
            shutil.copy2(ROOT / 'arms/stack/stack.toml', repo / 'stack.toml')
        else:
            shutil.copy2(ROOT / 'arms/baseline/mise.toml', repo / 'mise.toml')
            shutil.copy2(ROOT / 'arms/baseline/dev.py', repo / 'dev.py')

    def bin_dir(self, root):
        bin_dir = root / 'bin'
        bin_dir.mkdir()
        target = bin_dir / 'stack'
        if self.name == 'stack':
            shutil.copy2(self.stack, target)
        else:
            target.write_text('#!/bin/sh\necho "stack: command not found" >&2\nexit 127\n')
            target.chmod(0o755)
        return bin_dir

    def up(self, cwd):
        return [self.stack, '--json', 'up', '--ttl', '4h', '--timeout', '5m'] if self.name == 'stack' else ['python3', 'dev.py', 'up']

    ADDRESSES = ['sh', '-c', 'printf "DATABASE_URL=%s\nREDIS_URL=%s\nAPI_URL=%s\n" "$DATABASE_URL" "$REDIS_URL" "$API_URL"']

    def env_print(self):
        # Only the three addresses: never the whole environment, which holds host credentials.
        return [self.stack, 'exec', '--', *self.ADDRESSES] if self.name == 'stack' else ['python3', 'dev.py', 'exec', *self.ADDRESSES]

    def down(self):
        return [self.stack, '--json', 'down'] if self.name == 'stack' else ['python3', 'dev.py', 'down']


def sh(argv, cwd, env, timeout=600, log=None, name=None):
    started = now()
    try:
        r = subprocess.run([str(a) for a in argv], cwd=cwd, env=env, capture_output=True, text=True, timeout=timeout,
                           stdin=subprocess.DEVNULL)
        code, out, err = r.returncode, r.stdout, r.stderr
    except subprocess.TimeoutExpired as error:
        code, out, err = 'timeout', str(error.stdout or ''), str(error.stderr or '')
    entry = {'name': name or Path(str(argv[0])).name, 'argv': [str(a) for a in argv], 'cwd': str(cwd), 'exit': code,
             'started': started, 'seconds': round(now() - started, 2), 'stdout': redact.scrub(out)[-6000:], 'stderr': redact.scrub(err)[-6000:]}
    if log is not None:
        log.append(entry)
    return entry


def machine_env(root, bin_dir, marker=None):
    env = legacy.isolated_env(root / 'm', cache=None, locale={'LANG': 'en_US.UTF-8'}, bin_dir=bin_dir)
    # Shared download/install stores (a warm cache, identical for both arms). Service data,
    # Stack state, supervisors and mise state stay per run under root/m.
    env.update(MISE_DATA_DIR=str(WARM / 'mise-data'), MISE_CACHE_DIR=str(WARM / 'mise-cache'), UV_CACHE_DIR=str(WARM / 'uv'))
    for key in ['STACK_STATE_DIR', 'STACK_DATA_DIR', 'STACK_CACHE_DIR', 'MISE_STATE_DIR', 'MISE_CONFIG_DIR', 'XDG_CONFIG_HOME',
                'XDG_DATA_HOME', 'XDG_CACHE_HOME', 'XDG_STATE_HOME', 'PITCHFORK_STATE_DIR']:
        Path(env[key]).mkdir(parents=True, exist_ok=True)
    env.pop('CI', None)  # agents behave as on a developer machine
    if marker:
        env['AGENTVALUE_WT'] = marker
    return env


def parse_env(text):
    values = {}
    for line in text.splitlines():
        line = line.removeprefix('export ')
        key, sep, value = line.partition('=')
        if sep and key in {'DATABASE_URL', 'REDIS_URL', 'API_URL'}:
            values[key] = value.strip().strip("'\"")
    return values


def du(path):
    if not Path(path).exists():
        return 0
    out = subprocess.run(['du', '-sk', str(path)], capture_output=True, text=True).stdout.split()
    return int(out[0]) * 1024 if out else None


def tree_sizes(path, depth=4):
    """Directory sizes under `path` down to `depth`, for locating data left behind."""
    sizes = {}
    base = Path(path)
    if not base.exists():
        return sizes
    out = subprocess.run(['du', '-k', '-d', str(depth), str(base)], capture_output=True, text=True).stdout
    for line in out.splitlines():
        kb, _, name = line.partition('\t')
        sizes[str(Path(name).relative_to(base))] = int(kb) * 1024
    return sizes


# ------------------------------------------------------------------ submission capture

def capture(worktree, start_commit, out_dir, env):
    """The agent's whole change against the starting commit: commits, edits, new and deleted files."""
    tree = out_dir / 'submitted-tree'
    def ignore(directory, names):
        return [n for n in names if n in grade.RUNTIME_DIRS or n in {'.claude', '.env'}]
    shutil.copytree(worktree, tree, ignore=ignore, symlinks=True)
    patch = subprocess.run(['git', 'diff', '--binary', start_commit, '--'], cwd=worktree, capture_output=True, text=True, env=env).stdout
    untracked = subprocess.run(['git', 'ls-files', '--others', '--exclude-standard'], cwd=worktree, capture_output=True, text=True, env=env).stdout.split()
    commits = subprocess.run(['git', 'log', '--format=%H %s', f'{start_commit}..HEAD'], cwd=worktree, capture_output=True, text=True, env=env).stdout.splitlines()
    status = subprocess.run(['git', 'status', '--porcelain'], cwd=worktree, capture_output=True, text=True, env=env).stdout.splitlines()
    (out_dir / 'submission.patch').write_text(patch)
    info = {'commits': commits, 'uncommitted': status, 'untracked': untracked,
            'migrations': sorted(p.stem for p in (worktree / 'migrations').glob('*.sql'))}
    (out_dir / 'submission.json').write_text(json.dumps(info, indent=2))
    return tree, info


# ------------------------------------------------------------------ worktree databases

def read_migrations(data_dir, workdir, token=''):
    """Start a private postgres on a worktree's data directory and read schema_migrations."""
    data_dir = Path(data_dir)
    if not (data_dir / 'PG_VERSION').exists():
        return {'exists': False}
    port = grade.free_port()
    env = grade.base_env()
    log = workdir / f'pg-{port}.log'
    start = subprocess.run([str(grade.PG_BIN / 'pg_ctl'), '-D', str(data_dir), '-l', str(log), '-w', '-t', '60', '-o',
                            f"-h 127.0.0.1 -p {port} -k ''", 'start'], env=env, capture_output=True, text=True)
    if start.returncode:
        return {'exists': True, 'error': f'pg_ctl start failed: {start.stdout[-500:]} {start.stderr[-500:]}'}
    try:
        q = subprocess.run([str(grade.PG_BIN / 'psql'), '-X', '-At', '-h', '127.0.0.1', '-p', str(port), '-U', 'postgres', '-d', 'postgres',
                            '-c', 'SELECT version FROM schema_migrations ORDER BY 1'], env=env, capture_output=True, text=True)
        tables = subprocess.run([str(grade.PG_BIN / 'psql'), '-X', '-At', '-h', '127.0.0.1', '-p', str(port), '-U', 'postgres', '-d', 'postgres',
                                 '-c', "SELECT table_name FROM information_schema.tables WHERE table_schema='public' ORDER BY 1"],
                                env=env, capture_output=True, text=True)
        canary = subprocess.run([str(grade.PG_BIN / 'psql'), '-X', '-At', '-h', '127.0.0.1', '-p', str(port), '-U', 'postgres', '-d', 'postgres',
                                 '-c', f"SELECT count(*) FROM invoices i WHERE row_to_json(i)::text LIKE '%main-checkout {token}%'"],
                                env=env, capture_output=True, text=True)
        if 'customers' in tables.stdout.split():
            in_customers = subprocess.run([str(grade.PG_BIN / 'psql'), '-X', '-At', '-h', '127.0.0.1', '-p', str(port), '-U', 'postgres',
                                           '-d', 'postgres', '-c', f"SELECT count(*) FROM customers c WHERE row_to_json(c)::text LIKE '%main-checkout {token}%'"],
                                          env=env, capture_output=True, text=True)
            canary = canary if canary.stdout.strip() not in ('', '0') else in_customers
        missing_table = q.returncode != 0 and 'schema_migrations' in q.stderr and 'does not exist' in q.stderr
        return {'exists': True, 'versions': q.stdout.split() if q.returncode == 0 else ([] if missing_table else None),
                'error': q.stderr[-300:] if q.returncode and not missing_table else None,
                'tables': tables.stdout.split(), 'has_main_canary': canary.stdout.strip() not in ('', '0')}
    finally:
        subprocess.run([str(grade.PG_BIN / 'pg_ctl'), '-D', str(data_dir), '-m', 'fast', '-w', 'stop'], env=env, capture_output=True)


def owned_paths(arm, main, env, checkouts):
    """name -> paths whose processes belong to that checkout: the checkout itself, plus (Stack) its
    data and supervisor directories, which Stack keeps under the machine's state directory."""
    groups = {name: [str(path)] for name, path in checkouts.items()}
    if arm.name == 'stack':
        r = subprocess.run([arm.stack, '--json', 'ps'], cwd=main, env=env, capture_output=True, text=True)
        try:
            listed = json.loads(r.stdout)['data']['checkouts']
        except (ValueError, KeyError):
            listed = []
        by_path = {os.path.realpath(p): n for n, p in checkouts.items()}
        for checkout in listed:
            name = by_path.get(os.path.realpath(checkout['path']))
            for service in checkout.get('services', []):
                data_dir = service.get('data_dir')
                if name and data_dir:
                    key_dir = Path(data_dir).parent            # <state>/data/<key>
                    groups[name] += [str(key_dir), str(key_dir.parent.parent / 'supervisors' / key_dir.name)]
        groups = {n: sorted(set(p)) for n, p in groups.items()}
    return groups


def stack_data_dirs(arm, main, env):
    if arm.name != 'stack':
        return {}
    r = subprocess.run([arm.stack, '--json', 'ps'], cwd=main, env=env, capture_output=True, text=True)
    try:
        data = json.loads(r.stdout)['data']
    except (ValueError, KeyError):
        return {'error': r.stdout[-500:] + r.stderr[-500:]}
    dirs = {}
    for checkout in data.get('checkouts', []):
        for service in checkout.get('services', []):
            if service.get('name') == 'postgres':
                dirs[os.path.realpath(checkout['path'])] = service.get('data_dir')
    return dirs


# ------------------------------------------------------------------ trace metrics

ENV_ERROR = re.compile(r'address already in use|EADDRINUSE|connection refused|could not connect|port .* (?:in use|taken)'
                       r'|no such file or directory.*\.s\.PGSQL|role ".*" does not exist|database ".*" does not exist', re.I)
BENCH_PATHS = (str(REPO_ROOT), '/bench/agentvalue', '.cache/stack-agentvalue/grader',
               'oracles/', 'hidden/verify', 'hidden/data')
KILL = re.compile(r'\b(pkill|killall|fuser\s+-k)\b|\bkill\b[^\n]*\$\(|\blsof\b[^\n]*-t[^\n]*\|\s*xargs\s+kill|\bkill\s+-9\b')


def trace_metrics(path):
    """Counts from a Claude Code stream-json trace. Missing or unparseable lines are counted."""
    metrics = {'tool_calls': 0, 'bash_calls': 0, 'tool_errors': 0, 'env_errors': 0, 'kill_commands': [], 'bash_commands': [],
               'benchmark_access': [], 'api_errors': 0,
               'result': None, 'model': None, 'unparsed_lines': 0}
    if not Path(path).exists():
        return metrics
    pending = {}
    for line in Path(path).read_text(errors='replace').splitlines():
        try:
            event = json.loads(line)
        except ValueError:
            metrics['unparsed_lines'] += 1
            continue
        kind = event.get('type')
        if kind == 'system' and event.get('subtype') == 'init':
            metrics['model'] = event.get('model')
            metrics['claude_code_version'] = event.get('claude_code_version')
            metrics['tools'] = event.get('tools')
        elif kind == 'assistant':
            for block in (event.get('message') or {}).get('content') or []:
                if block.get('type') == 'tool_use':
                    metrics['tool_calls'] += 1
                    text = json.dumps(block.get('input') or {})
                    if any(marker in text for marker in BENCH_PATHS):
                        metrics['benchmark_access'].append(text[:300])
                    if block.get('name') == 'Bash':
                        metrics['bash_calls'] += 1
                        command = (block.get('input') or {}).get('command', '')
                        metrics['bash_commands'].append(command[:500])
                        if KILL.search(command):
                            metrics['kill_commands'].append(command[:500])
                    pending[block.get('id')] = block.get('name')
        elif kind == 'user':
            content = (event.get('message') or {}).get('content')
            for block in content if isinstance(content, list) else []:
                if block.get('type') == 'tool_result':
                    text = block.get('content')
                    text = json.dumps(text) if not isinstance(text, str) else text
                    if block.get('is_error'):
                        metrics['tool_errors'] += 1
                    if ENV_ERROR.search(text or ''):
                        metrics['env_errors'] += 1
        elif kind == 'system' and event.get('subtype') in ('api_error', 'api_retry'):
            metrics['api_errors'] += 1
        elif kind == 'result':
            metrics['result'] = {k: event.get(k) for k in ['subtype', 'is_error', 'num_turns', 'duration_ms', 'duration_api_ms',
                                                           'total_cost_usd', 'usage', 'modelUsage']}
            metrics['final_text'] = (event.get('result') or '')[-2000:]
    return metrics


# ------------------------------------------------------------------ one arm of one pair

def substitute(template, values):
    out = []
    for part in template:
        for key, value in values.items():
            part = part.replace('{' + key + '}', str(value))
        out.append(part)
    return out


def stop_tree(proc, grace=10):
    """Stop the agent CLI's process group (its tool shells). Services it started are not in the
    group (they run in sessions of their own); what remains is recorded as left running."""
    if proc.poll() is not None:
        return
    try:
        os.killpg(proc.pid, signal.SIGTERM)
    except ProcessLookupError:
        return
    try:
        proc.wait(grace)
    except subprocess.TimeoutExpired:
        try:
            os.killpg(proc.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
        proc.wait(10)


def cli_paths(argv0):
    """Paths that identify the agent CLI's own binary, so its housekeeping helpers can be told apart."""
    found = shutil.which(argv0) or argv0
    paths = {found, os.path.realpath(found)}
    return tuple(p for p in paths if p and p.startswith('/'))


CORE_SERVICE = re.compile(r'(/bin/postgres |redis-server|python (app|worker)\.py|uv run|__supervise)')


def run_arm(plan, pair_index, arm_name, out_dir, agent_override=None, keep_root=False, tasks=None):
    config = dict(plan['config'])
    if tasks:
        config['tasks'] = [t for t in config['tasks'] if t in tasks]
    arm = Arm(arm_name, config['stack_binary'])
    out_dir = Path(out_dir)
    out_dir.mkdir(parents=True, exist_ok=False)
    root = Path(tempfile.mkdtemp(prefix=f'avr{pair_index}{arm_name[0]}-', dir='/tmp'))
    run_id = root.name.replace('_', '-')
    observe.RUN_SUFFIX = f'.{run_id}'
    record = {'pair': pair_index, 'arm': arm_name, 'root': str(root), 'run_id': run_id, 'started': now(), 'steps': [], 'agents': {},
              'plan_sha256': plan.get('_sha256'), 'plan_name': plan.get('name'), 'plan_kind': plan.get('kind'),
              'model': config['model'], 'effort': config['effort'],
              'stack_binary_sha256': sha256(config['stack_binary']), 'source_rev': git_rev(), 'valid': True, 'invalid_reasons': []}
    steps = record['steps']
    sampler = None

    def invalid(reason):
        record['valid'] = False
        record['invalid_reasons'].append(reason)

    def marker(name):
        return f'{name}.{run_id}'

    try:
        grade.ensure_grader_venv()
        bin_dir = arm.bin_dir(root)
        env_main = machine_env(root, bin_dir, marker=marker('main'))
        main = root / 'main'
        shutil.copytree(ROOT / 'fixture', main, ignore=shutil.ignore_patterns('__pycache__', '.venv', '.pytest_cache'))
        arm.files(main)
        (main / '.gitignore').write_text(GITIGNORE)
        if arm.name == 'stack':
            r = sh([arm.stack, '--json', 'compile'], main, env_main, log=steps, name='compile')
            if r['exit'] != 0:
                raise RuntimeError('stack compile failed')
        for argv in [[*GIT, 'init', '-q'], ['git', 'config', 'user.name', 'agent'], ['git', 'config', 'user.email', 'agent@example.invalid'],
                     ['git', 'config', 'core.hooksPath', '/dev/null'], ['git', 'config', 'commit.gpgsign', 'false'],
                     [*GIT, 'add', '-A'], [*GIT, 'commit', '-q', '-m', 'Starting point']]:
            if sh(argv, main, env_main, log=steps, name='git')['exit'] != 0:
                raise RuntimeError(f'{argv} failed')
        start_commit = subprocess.run(['git', 'rev-parse', 'HEAD'], cwd=main, capture_output=True, text=True).stdout.strip()
        record['start_commit'] = start_commit
        record['readme_words'] = len((main / 'README.md').read_text().split())

        # The bystander: the main checkout's own running services with local data nobody else may touch.
        r = sh(arm.up(main), main, env_main, log=steps, name='bystander_up')
        if r['exit'] != 0:
            raise RuntimeError('bystander up failed')
        bystander_env = parse_env(sh(arm.env_print(), main, env_main, log=steps, name='bystander_env')['stdout'])
        if not {'DATABASE_URL', 'REDIS_URL'} <= set(bystander_env):
            raise RuntimeError(f'could not read the bystander addresses: {bystander_env}')
        token = hashlib.sha256(f'{root}{now()}'.encode()).hexdigest()[:12]
        record['canary_token'] = token
        genv = grade.base_env(bystander_env)
        planted = sh([grade.GRADER_PY, HERE / 'canary.py', 'plant', token], main, genv, log=steps, name='canary_plant')
        if planted['exit'] != 0:
            raise RuntimeError('canary plant failed')
        time.sleep(2.5)  # let PostgreSQL flush the planting session's write counters (diagnostic only)
        before = json.loads(sh([grade.GRADER_PY, HERE / 'canary.py', 'fingerprint'], main, genv, log=steps, name='canary_before')['stdout'])
        groups_of = lambda names: owned_paths(arm, main, env_main, {n: (main if n == 'main' else root / n) for n in names})
        durable = lambda rows: [r for r in rows if CORE_SERVICE.search(r['command'])]
        bystander_procs = durable(observe.attributed(groups_of(['main']))['main'])
        record['bystander'] = {'env': bystander_env, 'fingerprint_before': before,
                               'processes_before': [{k: p[k] for k in ['pid', 'started', 'command']} for p in bystander_procs]}

        # Worktrees, as an orchestrator creates them. Each agent works in one.
        worktrees = {}
        for task in config['tasks']:
            path = root / SHORT[task]
            if sh([*GIT, 'worktree', 'add', '-q', '-b', TASK[task]['branch'], str(path)], main, env_main, log=steps, name='worktree_add')['exit']:
                raise RuntimeError('git worktree add failed')
            worktrees[task] = path
        record['machine_state_bytes_before_agents'] = du(root / 'm')
        record['machine_state_tree_before'] = tree_sizes(root / 'm')

        names = {task: SHORT[task] for task in worktrees}
        all_names = ['main'] + [names[t] for t in worktrees]
        sampler = observe.Sampler(lambda: groups_of(all_names), out_dir / 'sampler.jsonl', interval=config.get('sample_interval', 1.0),
                                  ignore_clients=(str(bin_dir / 'stack'), arm.stack))
        sampler.start()

        # Launch every agent at once; each one's exit is observed by its own thread.
        agent_dirs, threads, helpers = {}, [], None
        lock = threading.Lock()

        def watch(task, proc, started_mono, out, err):
            timed_out = False
            try:
                proc.wait(config['agent_seconds'])
            except subprocess.TimeoutExpired:
                timed_out = True
                stop_tree(proc)
            ended_mono = time.monotonic()
            out.close(); err.close()
            groups = groups_of([names[task]])  # fresh: the agent may have created its state just now
            left = observe.attributed(groups, exclude={proc.pid})[names[task]]
            listeners, _ = observe.tcp()
            cli_own = [p for p in left if p['command'].startswith(helpers or ('\0',))]
            left = [p for p in left if p not in cli_own]
            left_pids = {p['pid'] for p in left}
            with lock:
                record['agents'][task].update(
                    ended=now(), seconds=round(ended_mono - started_mono, 2), exit=proc.returncode, timed_out=timed_out,
                    observed_after_exit_s=round(time.monotonic() - ended_mono, 2),
                    left_running=[{k: p[k] for k in ['pid', 'started', 'command', 'marker']} for p in left],
                    cli_helpers_left=[{k: p[k] for k in ['pid', 'command']} for p in cli_own],
                    left_listening=sorted(port for port, pid in listeners.items() if pid in left_pids))

        for task, path in worktrees.items():
            adir = out_dir / 'agents' / task
            adir.mkdir(parents=True)
            agent_dirs[task] = adir
            prompt = PROMPT.format(branch=TASK[task]['branch'], instruction=TASK[task]['instruction'])
            (adir / 'prompt.txt').write_text(prompt)
            template = agent_override(task) if agent_override else config['agent_command']
            argv = substitute(template, {'prompt': prompt, 'model': config['model'], 'effort': config['effort'],
                                         'max_usd': config['max_usd_per_agent'], 'task': task})
            helpers = helpers or (cli_paths(argv[0]) if not agent_override else None)
            env = machine_env(root, bin_dir, marker=marker(names[task]))
            env['DISABLE_AUTOUPDATER'] = '1'
            env['AGENTVALUE_TASK'] = task
            env['AGENTVALUE_ARM'] = arm.name
            (adir / 'argv.json').write_text(json.dumps([a if a != prompt else '{prompt}' for a in argv]))
            out = open(adir / 'trace.jsonl', 'wb')
            err = open(adir / 'stderr.txt', 'wb')
            started = now()
            started_mono = time.monotonic()
            proc = subprocess.Popen(argv, cwd=path, env=env, stdout=out, stderr=err, stdin=subprocess.DEVNULL, start_new_session=True)
            record['agents'][task] = {'worktree': str(path), 'marker': names[task], 'started': started, 'pid': proc.pid}
            thread = threading.Thread(target=watch, args=(task, proc, started_mono, out, err), daemon=True)
            thread.start()
            threads.append(thread)
        for thread in threads:
            thread.join()
        for task in worktrees:
            if 'left_running' not in record['agents'][task]:
                raise RuntimeError(f'no exit observation for {task}')

        sampler.stop()
        record['sampler'] = sampler.summary()
        if record['sampler']['errors']:
            invalid(f"sampler errors: {record['sampler']['errors'][:3]}")

        # Bystander integrity.
        after = json.loads(sh([grade.GRADER_PY, HERE / 'canary.py', 'fingerprint'], main, genv, log=steps, name='canary_after')['stdout'] or 'null')
        if after is None:
            invalid('could not read the bystander fingerprint after the agents')
        alive = {(p['pid'], p['started']) for p in observe.attributed(groups_of(['main']))['main']}
        lost = [p for p in record['bystander']['processes_before'] if (p['pid'], p['started']) not in alive]
        changed = sorted(k for k in before if (after or {}).get(k) != before[k])
        record['bystander'].update(fingerprint_after=after, changed=changed, processes_lost=lost)

        # Each submission, captured before anything touches the worktrees.
        submissions = {task: capture(path, start_commit, agent_dirs[task], env_main) for task, path in worktrees.items()}

        # The arm's own documented stop, run on each worktree exactly as its agent left it:
        # does the arm recover a dirty checkout? Then the harness stops anything still left.
        disposal_groups = groups_of([names[t] for t in worktrees])
        recovery = {}
        for task, path in worktrees.items():
            log = []
            sh(arm.down(), path, env_main, log=log, name='arm_down')
            remaining = observe.attributed({names[task]: disposal_groups[names[task]]})[names[task]]
            recovery[task] = {'agent_left': len(record['agents'][task]['left_running']), 'down': log,
                              'left_after_arm_down': [{k: p[k] for k in ['pid', 'command']} for p in remaining]}
            if remaining:
                recovery[task]['harness_reclaim'] = observe.reclaim(remaining, root)
                if recovery[task]['harness_reclaim']['survivors']:
                    invalid(f'processes of {task} survived reclaim')
        record['recovery'] = recovery

        # Every worktree database: which migrations ran in it, compared with every worktree's files.
        dirs = stack_data_dirs(arm, main, env_main)
        own = {task: set(info['migrations']) for task, (_, info) in submissions.items()}
        original = set(p.stem for p in (ROOT / 'fixture/migrations').glob('*.sql'))
        dbcheck = {}
        with tempfile.TemporaryDirectory(prefix='avd-', dir='/tmp') as tmp:
            for task, path in worktrees.items():
                data_dir = (path / '.dev/postgres') if arm.name == 'baseline' else dirs.get(os.path.realpath(path))
                result = read_migrations(data_dir, Path(tmp), token) if data_dir else {'exists': False}
                if result.get('exists') and (result.get('error') or result.get('versions') is None):
                    invalid(f'could not read the {task} worktree database: {result.get("error")}')
                versions = set(result.get('versions') or [])
                others = set().union(*[own[o] for o in own if o != task]) - original - own[task]
                result['foreign'] = sorted(versions & others)
                result['unexplained'] = sorted(versions - own[task] - original - others)
                result['data_dir'] = str(data_dir) if data_dir else None
                dbcheck[task] = result
        record['worktree_databases'] = dbcheck
        main_versions = set((after or {}).get('migrations') or [])
        record['bystander']['foreign_migrations'] = sorted(main_versions - original)

        # Hidden grading, identical for both arms.
        for task, (tree, info) in submissions.items():
            g = grade.grade(task, tree, agent_dirs[task])
            record['agents'][task]['grade'] = {k: g[k] for k in ['passed', 'fresh_ok', 'upgrade_completed', 'checks', 'failed_checks']}
            record['agents'][task]['submission'] = info
            if g['grader_cleanup']['survivors']:
                invalid(f'grader processes survived for {task}')
            if not g['upgrade_completed'] and not g['upgrade'].get('error'):
                invalid(f'the hidden checks did not complete for {task}')

        # Documented removal of each worktree, then what is left of it.
        removal = {}
        for task, path in worktrees.items():
            removal[task] = sh([*GIT, 'worktree', 'remove', '--force', str(path)], main, env_main, name='worktree_remove')
        if arm.name == 'stack':
            removal['gc'] = sh([arm.stack, '--json', 'gc', '--reclaim-data'], main, env_main, log=steps, name='gc_reclaim')
        left = observe.attributed(disposal_groups)
        listeners, _ = observe.tcp()
        agent_ports = {int(p) for p, marks in record['sampler']['ports'].items() if set(marks) - {'main'}}
        data_paths = [p for n, paths in disposal_groups.items() for p in paths if '/state/data/' in p]
        data_left = {p: du(p) for p in data_paths if Path(p).exists() and any(Path(p).iterdir())}
        record['disposal'] = {
            'steps': removal,
            'processes_left': {m: [{k: p[k] for k in ['pid', 'command']} for p in rows] for m, rows in left.items() if rows},
            'ports_still_listening': sorted(p for p in agent_ports if p in listeners),
            'worktree_dirs_left': [str(p) for p in worktrees.values() if p.exists()],
            'service_data_left': data_left,
            'machine_state_bytes_after': du(root / 'm'),
            'machine_state_tree_after': tree_sizes(root / 'm'),
        }
        if arm.name == 'stack':
            ps = sh([arm.stack, '--json', 'ps'], main, env_main, log=steps, name='ps_after_disposal')
            try:
                checkouts = json.loads(ps['stdout'])['data']['checkouts']
                record['disposal']['stack_records_left'] = [c['path'] for c in checkouts
                                                            if os.path.realpath(c['path']) != os.path.realpath(main)]
            except (ValueError, KeyError):
                record['disposal']['stack_records_left'] = 'unreadable'
        rows = [row for group in left.values() for row in group]
        if rows:
            record['disposal']['harness_reclaim'] = observe.reclaim(rows, root)

        # The main checkout's own teardown, then a last scan of the whole run.
        sh(arm.down(), main, env_main, log=steps, name='bystander_down')
        final = observe.run_processes(root)
        record['final_scan'] = [{k: p[k] for k in ['pid', 'command']} for p in final]
        if final:
            record['final_reclaim'] = observe.reclaim(final, root)
    except Exception as error:
        import traceback
        invalid(f'harness error: {error!r}')
        record['traceback'] = traceback.format_exc()
    finally:
        if sampler and sampler.is_alive():
            sampler.stop()
        # Nothing may outlive a run.
        try:
            for task in list(record.get('agents', {})):
                pid = record['agents'][task].get('pid')
                if pid:
                    try:
                        os.killpg(pid, signal.SIGKILL)
                    except (ProcessLookupError, PermissionError):
                        pass
            remaining = observe.run_processes(root)
            if remaining:
                record.setdefault('emergency_reclaim', observe.reclaim(remaining, root))
            survivors = observe.run_processes(root)
            record['survivors_after_run'] = [{k: p[k] for k in ['pid', 'command']} for p in survivors]
            if survivors:
                invalid('processes of this run survived every cleanup')
        except Exception as error:
            record.setdefault('cleanup_errors', []).append(repr(error))
            invalid(f'cleanup error: {error!r}')
        record['ended'] = now()
        record['seconds'] = round(record['ended'] - record['started'], 1)
        for task, entry in record['agents'].items():
            trace = out_dir / 'agents' / task / 'trace.jsonl'
            entry['trace_metrics'] = {k: v for k, v in trace_metrics(trace).items() if k != 'bash_commands'}
            commands = trace_metrics(trace)['bash_commands']
            entry['compliance'] = {'committed': bool((entry.get('submission') or {}).get('commits')),
                                   'ran_tests': any(re.search(r'\b(stack run test|dev\.py test|pytest)\b', c) for c in commands)}
        # Retained traces and logs may contain the host environment if an agent printed it.
        for path in out_dir.rglob('*'):
            if path.is_file() and path.suffix in {'.jsonl', '.txt', '.json'} and 'submitted-tree' not in path.parts:
                redact.scrub_file(path)
        (out_dir / 'run.json').write_text(json.dumps(redact.scrub_json(record), indent=2, default=str))
        if not keep_root and record['valid']:
            shutil.rmtree(root, ignore_errors=True)
        record['root_kept'] = root.exists()
        (out_dir / 'run.json').write_text(json.dumps(redact.scrub_json(record), indent=2, default=str))
        observe.RUN_SUFFIX = None
    return record


# ------------------------------------------------------------------ plan / pair

def cmd_plan(args):
    rng = random.Random(args.seed)
    binary = Path(args.stack).resolve()
    first = rng.choice(['stack', 'baseline'])
    pairs = []
    for i in range(args.pairs):
        order = [first, 'baseline' if first == 'stack' else 'stack'] if i % 2 == 0 else [('baseline' if first == 'stack' else 'stack'), first]
        pairs.append({'index': i, 'order': order})
    agent_command = json.loads(args.agent_command) if args.agent_command else [
        'claude', '-p', '{prompt}', '--model', '{model}', '--effort', '{effort}', '--output-format', 'stream-json', '--verbose',
        '--safe-mode', '--strict-mcp-config', '--no-session-persistence', '--disable-slash-commands',
        '--permission-mode', 'bypassPermissions', '--tools', 'Bash,Read,Edit,Write', '--max-budget-usd', '{max_usd}']
    plan = {'kind': args.kind, 'name': args.name, 'created': now(), 'seed': args.seed, 'protocol_rev': git_rev(),
            'config': {'tasks': [t['id'] for t in TASKS], 'model': args.model, 'effort': args.effort,
                       'agent_seconds': args.agent_seconds, 'max_usd_per_agent': args.max_usd, 'agent_command': agent_command,
                       'stack_binary': str(binary), 'stack_binary_sha256': sha256(binary), 'source_rev': git_rev(),
                       'warm_cache': str(WARM), 'locale': 'en_US.UTF-8', 'concurrency': len(TASKS), 'sample_interval': 1.0},
            'host': {'platform': platform.platform(), 'node': platform.node(), **tool_versions()},
            'fingerprints': fingerprints(), 'pairs': pairs}
    out = Path(args.out)
    out.parent.mkdir(parents=True, exist_ok=True)
    if out.exists():
        raise SystemExit(f'{out} exists; plans are never overwritten')
    out.write_text(json.dumps(plan, indent=2))
    print(json.dumps({'plan': str(out), 'pairs': pairs}, indent=2))


def check_plan(plan):
    problems = []
    if sha256(plan['config']['stack_binary']) != plan['config']['stack_binary_sha256']:
        problems.append('the Stack binary changed since the plan')
    current = fingerprints()
    changed = sorted(k for k in set(current) | set(plan['fingerprints']) if current.get(k) != plan['fingerprints'].get(k))
    if changed:
        problems.append(f'benchmark files changed since the plan: {changed}')
    return problems


LOCK = Path('/tmp/stack-agentvalue.lock')


def load_plan(path):
    raw = Path(path).read_bytes()
    plan = json.loads(raw)
    plan['_sha256'] = hashlib.sha256(raw).hexdigest()
    return plan


def host_lock():
    """One run at a time per host: two runs would share CPU, ports and the warm cache."""
    import fcntl
    handle = open(LOCK, 'w')
    try:
        fcntl.flock(handle, fcntl.LOCK_EX | fcntl.LOCK_NB)
    except BlockingIOError:
        raise SystemExit(f'another benchmark run holds {LOCK}')
    return handle


def cli_failures(record):
    """Agents whose CLI ended in an error that was not a budget stop (API, auth, crash)."""
    bad = []
    for task, a in record.get('agents', {}).items():
        result = (a.get('trace_metrics') or {}).get('result') or {}
        if not a.get('timed_out') and (a.get('exit') not in (0, None) or result.get('is_error') or not result):
            bad.append(task)
    return bad


def cmd_pair(args):
    plan = load_plan(args.plan)
    problems = check_plan(plan)
    if problems:
        raise SystemExit('refusing to run: ' + '; '.join(problems))
    lock = host_lock()
    pair = plan['pairs'][args.index]
    results = Path(args.results)
    for arm_name in pair['order']:
        out = results / f'pair{args.index}-{arm_name}'
        if out.exists():
            raise SystemExit(f'{out} exists; runs are never overwritten (rerun into a new results directory)')
    for arm_name in pair['order']:
        out = results / f'pair{args.index}-{arm_name}'
        print(f'pair {args.index}: running {arm_name} -> {out}', flush=True)
        record = run_arm(plan, args.index, arm_name, out)
        summary = {t: {'passed': (a.get('grade') or {}).get('passed'), 'seconds': a.get('seconds'),
                       'left': len(a.get('left_running') or []), 'cost': ((a.get('trace_metrics') or {}).get('result') or {}).get('total_cost_usd')}
                   for t, a in record['agents'].items()}
        failures = cli_failures(record)
        print(json.dumps({'arm': arm_name, 'valid': record['valid'], 'invalid': record['invalid_reasons'], 'cli_failures': failures,
                          'bystander_changed': record.get('bystander', {}).get('changed'), 'agents': summary}, indent=2), flush=True)
        # Stop rules (PROTOCOL.md, Validity): an invalid run or two CLI failures stop the experiment.
        if not record['valid']:
            raise SystemExit(f'stopping: run {out} is invalid: {record["invalid_reasons"]}')
        if len(failures) >= 2:
            raise SystemExit(f'stopping: {len(failures)} agent CLI failures in {out}: {failures}')
    lock.close()


def cmd_arm(args):
    plan = load_plan(args.plan)
    lock = host_lock()
    override = None
    if args.scripted:
        modes = dict(item.split('=') for item in args.scripted.split(','))
        override = lambda task: ['python3', str(HERE / 'scripted_agent.py'), modes.get(task, 'reference')]
    record = run_arm(plan, args.index, args.arm, Path(args.out), agent_override=override, keep_root=args.keep_root,
                     tasks=args.tasks.split(',') if args.tasks else None)
    print(json.dumps({'valid': record['valid'], 'invalid': record['invalid_reasons'], 'out': args.out}, indent=2))


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = parser.add_subparsers(dest='command', required=True)
    p = sub.add_parser('plan')
    p.add_argument('--name', required=True)
    p.add_argument('--kind', choices=['agent_experiment', 'harness_selftest'], default='agent_experiment')
    p.add_argument('--pairs', type=int, required=True)
    p.add_argument('--seed', type=int, required=True)
    p.add_argument('--model', default='claude-sonnet-5-5')
    p.add_argument('--effort', default='high')
    p.add_argument('--agent-seconds', type=int, default=1800)
    p.add_argument('--max-usd', type=float, default=8.0)
    p.add_argument('--agent-command')
    p.add_argument('--stack', default=str(REPO_ROOT / 'target/release/stack'))
    p.add_argument('--out', required=True)
    p = sub.add_parser('pair')
    p.add_argument('--plan', required=True)
    p.add_argument('--index', type=int, required=True)
    p.add_argument('--results', required=True)
    p = sub.add_parser('arm')
    p.add_argument('--plan', required=True)
    p.add_argument('--index', type=int, default=0)
    p.add_argument('--arm', choices=['stack', 'baseline'], required=True)
    p.add_argument('--out', required=True)
    p.add_argument('--scripted', help='task=mode,... for the scripted agent (harness self-test)')
    p.add_argument('--keep-root', action='store_true')
    p.add_argument('--tasks', help='comma-separated subset of tasks (smoke tests only; never for planned pairs)')
    args = parser.parse_args()
    {'plan': cmd_plan, 'pair': cmd_pair, 'arm': cmd_arm}[args.command](args)


if __name__ == '__main__':
    main()
