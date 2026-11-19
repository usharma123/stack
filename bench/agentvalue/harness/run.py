#!/usr/bin/env python3
"""Stack agent-value benchmark: several agents at once, one worktree each, one repository.

  plan      fix the configuration, scenario, victim and arm order of every pair before any trial runs
  pair      run one planned pair end to end: both arms, in the planned order
  arm       run one arm of a self-test plan (harness self-tests and validation only)

Validation is `validate.py`, the report `report.py`. See ../PROTOCOL.md for what is measured and
how each measure is detected; this file implements protocol version 2 (five agents, fault
injection, the teardown procedure). Version 1 records stay readable by report.py.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import queue
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
import grade
import observe
import redact

TASKS = json.loads((ROOT / 'tasks.json').read_text())['tasks']
TASK = {t['id']: t for t in TASKS}
SHORT = {'invoice-notes': 'wt-notes', 'pay-receipts': 'wt-pay', 'customer-table': 'wt-cust', 'invoice-list': 'wt-list',
         'void-invoices': 'wt-void'}
WARM = Path(os.environ.get('AGENTVALUE_WARM', Path.home() / '.cache/stack-agentvalue/warm'))
GITIGNORE = '.stack/\n.dev/\n.venv/\n.config/mise/\n__pycache__/\n.pytest_cache/\n'
GIT = ['git', '-c', 'user.name=bench', '-c', 'user.email=bench@example.invalid', '-c', 'core.hooksPath=/dev/null',
       '-c', 'init.defaultBranch=main', '-c', 'commit.gpgsign=false']

PROMPT = """You are one of several coding agents working on this repository at the same time. Each agent has its own git worktree and branch. Yours is the current directory, on branch {branch}. Other agents' worktrees, and the main checkout, may have services running while you work.

Your task:
{instruction}

Read README.md first: it explains how to run the project and its services in this worktree. Before you finish, run the tests against your worktree's running services and make sure they pass, and commit your work on your branch. When you are done, stop everything you started in this worktree and leave other worktrees' and the main checkout's services alone."""

# PROTOCOL.md, Version 2, "The recovery prompt", verbatim (its line breaks are only Markdown wrapping).
RECOVERY_PROMPT = """You are taking over from another coding agent. It was working on the task below in this git worktree (the current directory, on branch {branch}) and stopped unexpectedly partway through. It will not come back. Its edits may be incomplete, and it may have left this worktree's services running or half started. Other agents' worktrees, and the main checkout, have services running while you work.

Your task:
{instruction}

Read README.md first: it explains how to run the project and its services in this worktree. Check what state the worktree and its services are in, then finish the task. Before you finish, run the tests against your worktree's running services and make sure they pass, and commit your work on your branch. When you are done, stop everything that is running for this worktree, including anything the previous agent left, and leave other worktrees' and the main checkout's services alone."""

PROTOCOL_VERSION = 2
SCENARIOS = ('agent-kill', 'service-kill')
FAULT_PARAMS = {'kill_delay_s': 3, 'recovery_delay_s': 10, 'fault_deadline_s': 600}

# ------------------------------------------------------------------ the agent environment
# PROTOCOL.md, Version 2, "What can reach a record". Built by the benchmark itself: PATH, SHELL,
# TMPDIR and LANG are fixed benchmark values; the per-run stores and the markers are the harness's;
# only HOME, USER and LOGNAME come from the host (the agent CLI cannot sign in without them). The
# same environment is used for the bystander and for every arm command. Nothing here is recorded.
HOST_VARIABLES = ('HOME', 'USER', 'LOGNAME')
# The tools the arms and the agents need: python3 (dev.py, scripted agents), mise, git, lsof, ps,
# pkill. Before them: the run's bin directory (`stack`) and the pinned agent CLI's directory.
BASE_PATH = ('/opt/homebrew/opt/python@3.12/libexec/bin', '/opt/homebrew/bin', '/usr/local/bin', '/usr/bin', '/bin',
             '/usr/sbin', '/sbin')
FIXED = {'SHELL': '/bin/zsh', 'LANG': 'en_US.UTF-8'}   # PATH and TMPDIR are fixed per run (bin dir, run root)
MACHINE_STORES = {'STACK_STATE_DIR': 'stack/state', 'STACK_DATA_DIR': 'stack/data', 'STACK_CACHE_DIR': 'stack/cache',
                  'MISE_STATE_DIR': 'mise/state', 'MISE_CONFIG_DIR': 'mise/config', 'MISE_GLOBAL_CONFIG_FILE': 'mise/config/config.toml',
                  'MISE_SYSTEM_CONFIG_FILE': 'mise/config/system.toml', 'PITCHFORK_CONFIG_DIR': 'pf/config',
                  'PITCHFORK_STATE_DIR': 'pf/state', 'PITCHFORK_LOGS_DIR': 'pf/logs', 'XDG_CONFIG_HOME': 'xdg/config',
                  'XDG_DATA_HOME': 'xdg/data', 'XDG_CACHE_HOME': 'xdg/cache', 'XDG_STATE_HOME': 'xdg/state',
                  'npm_config_cache': 'npm/cache', 'npm_config_userconfig': 'npm/npmrc',
                  'npm_config_globalconfig': 'npm/global-npmrc', 'npm_config_prefix': 'npm/prefix'}
STORE_DIRS = ['STACK_STATE_DIR', 'STACK_DATA_DIR', 'STACK_CACHE_DIR', 'MISE_STATE_DIR', 'MISE_CONFIG_DIR', 'XDG_CONFIG_HOME',
              'XDG_DATA_HOME', 'XDG_CACHE_HOME', 'XDG_STATE_HOME', 'PITCHFORK_STATE_DIR']
WARM_STORES = ('MISE_DATA_DIR', 'MISE_CACHE_DIR', 'UV_CACHE_DIR')  # the shared warm download/install cache
CONSTANTS = {'MISE_YES': '1', 'NO_COLOR': '1', 'GIT_CONFIG_NOSYSTEM': '1'}
AGENT_CONSTANTS = {'DISABLE_AUTOUPDATER': '1'}
MARKERS = ('AGENTVALUE_WT', 'AGENTVALUE_TASK', 'AGENTVALUE_ARM', 'AGENTVALUE_ROLE')
ENV_ALLOWLIST = frozenset(HOST_VARIABLES) | {'PATH', 'TMPDIR'} | frozenset(FIXED) | frozenset(MACHINE_STORES) \
    | frozenset(WARM_STORES) | frozenset(CONSTANTS) | frozenset(AGENT_CONSTANTS) | frozenset(MARKERS)

# PROTOCOL.md, Version 2, item 1: the Stack build a trial plan must use.
PROTOCOL_STACK_REV = 'a5d4785'
PROTOCOL_STACK_SHA256 = 'b0931a8be40bc65855a1139d10a8f7f66c136e5a564b58cf9994a58f511fdd17'


def sha256(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def fingerprints():
    """Every file that affects a trial (docs, results and caches excluded)."""
    files = {}
    for path in sorted(ROOT.rglob('*')):
        rel = path.relative_to(ROOT)
        agent_facing = path.suffix == '.md' and (rel.parts[0] == 'arms' or rel.name == 'PROTOCOL.md')
        if path.is_file() and rel.parts[0] not in {'results'} and '__pycache__' not in rel.parts and rel.name != '.gitignore' \
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


def sh(argv, cwd, env, timeout=600, log=None, name=None, keep_raw=False):
    """Run an arm or harness command. The record keeps scrubbed, truncated output; with `keep_raw`
    the full stdout is also returned (never recorded), for parsing."""
    started = now()
    try:
        r = subprocess.run([str(a) for a in argv], cwd=cwd, env=env, capture_output=True, text=True, timeout=timeout,
                           stdin=subprocess.DEVNULL)
        code, out, err = r.returncode, r.stdout, r.stderr
    except subprocess.TimeoutExpired as error:
        out = error.stdout.decode(errors='replace') if isinstance(error.stdout, bytes) else (error.stdout or '')
        err = error.stderr.decode(errors='replace') if isinstance(error.stderr, bytes) else (error.stderr or '')
        code = 'timeout'
    entry = {'name': name or Path(str(argv[0])).name, 'argv': [str(a) for a in argv], 'cwd': str(cwd), 'exit': code,
             'started': started, 'seconds': round(now() - started, 2), 'stdout': redact.scrub(out)[-6000:], 'stderr': redact.scrub(err)[-6000:]}
    if log is not None:
        log.append(entry)
    return (entry, out) if keep_raw else entry


def machine_env(root, bin_dir, marker=None, cli_dir=None, **markers):
    """The environment for the bystander, every agent and every arm command the harness runs, built
    up from nothing (see ENV_ALLOWLIST): fixed PATH, SHELL, TMPDIR and LANG; HOME, USER, LOGNAME."""
    env = {key: os.environ[key] for key in HOST_VARIABLES if key in os.environ}
    env['PATH'] = os.pathsep.join([str(bin_dir)] + ([str(cli_dir)] if cli_dir else []) + list(BASE_PATH))
    env.update(FIXED)
    tmp = root / 'tmp'
    tmp.mkdir(exist_ok=True)
    env['TMPDIR'] = str(tmp) + '/'
    env.update({key: str(root / 'm' / sub) for key, sub in MACHINE_STORES.items()})
    # Shared download/install stores (a warm cache, identical for both arms). Service data, Stack
    # state, supervisors and mise state stay per run under root/m.
    env.update(MISE_DATA_DIR=str(WARM / 'mise-data'), MISE_CACHE_DIR=str(WARM / 'mise-cache'), UV_CACHE_DIR=str(WARM / 'uv'))
    env.update(CONSTANTS)
    for key in STORE_DIRS:
        Path(env[key]).mkdir(parents=True, exist_ok=True)
    if marker:
        env['AGENTVALUE_WT'] = marker
    env.update({key: str(value) for key, value in markers.items()})
    stray = set(env) - ENV_ALLOWLIST
    if stray:
        raise RuntimeError(f'environment keys outside the allowlist: {sorted(stray)}')
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
        if tables.returncode != 0 or (canary.returncode != 0 and 'invoices' not in canary.stderr):
            return {'exists': True, 'error': f'query failed: {tables.stderr[-200:]} {canary.stderr[-200:]}'}
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
            raise RuntimeError(f'stack ps failed while attributing processes: {r.stdout[-300:]} {r.stderr[-300:]}')
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
        raise RuntimeError(f'stack ps failed while locating databases: {r.stdout[-300:]} {r.stderr[-300:]}')
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
                        metrics['bash_commands'].append(command)  # full text: compliance scans it
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


# Commands event (g) and "victim copes" need, with when each ran: from the arrival of its tool_use
# line to the arrival of its result (PROTOCOL.md, Version 2, event (g)).
DOWN_RESTART = re.compile(r'\b(?:stack|dev\.py)\b[^\n;&|]*\b(?:down|restart)\b')
UP = re.compile(r'\b(?:stack|dev\.py)\b[^\n;&|]*\bup\b')
ANY_KILL = re.compile(r'\b(?:kill|pkill|killall|fuser|pgrep)\b|xargs\s+kill')
TESTS = re.compile(r'\bstack\s+(?:--json\s+)?run\s+test\b|\bdev\.py\s+test\b|\b(?:stack\s+(?:--json\s+)?exec|dev\.py\s+exec)\b[^\n]*\bpytest\b')
ARRIVALS = 'trace.arrivals.tsv'


class TraceTimes(threading.Thread):
    """The arrival time of every trace line, in seconds since its session's launch (monotonic),
    written as `<line number>\t<seconds>`. The agent CLI writes the trace to a staging file under
    the run root and this thread only reads it, so the trace's content is never changed and a
    harness crash loses at most arrival times. Times are when the harness saw the line (50 ms poll)."""

    def __init__(self, trace, out, launched_mono):
        super().__init__(daemon=True)
        self.trace, self.out, self.launched = Path(trace), Path(out), launched_mono
        self.stop_event = threading.Event()

    def run(self):
        buffer, count, stamp = b'', 0, 0.0
        with open(self.trace, 'rb') as source, open(self.out, 'a') as out:
            out.write('line\tseconds\n')
            out.flush()
            while True:
                chunk = source.read()
                if chunk:
                    stamp = round(time.monotonic() - self.launched, 3)
                    buffer += chunk
                    *lines, buffer = buffer.split(b'\n')
                    for _ in lines:
                        count += 1
                        out.write(f'{count}\t{stamp}\n')
                    out.flush()
                elif self.stop_event.is_set():
                    break
                else:
                    time.sleep(0.05)
            if buffer:
                out.write(f'{count + 1}\t{stamp}\tunterminated\n')

    def stop(self):
        self.stop_event.set()
        self.join(10)


def read_arrivals(arrivals):
    times = {}
    if Path(arrivals).exists():
        for row in Path(arrivals).read_text().splitlines()[1:]:
            parts = row.split('\t')
            if len(parts) >= 2 and parts[0].isdigit():
                times[int(parts[0])] = float(parts[1])
    return times


def tool_calls(trace, arrivals=None):
    """Every tool_use of a trace in order, with the arrival of its line and of its result line (None
    while it had no result) and whether the result was an error."""
    times = read_arrivals(arrivals) if arrivals else {}
    calls, by_id = [], {}
    if not Path(trace).exists():
        return calls
    for number, line in enumerate(Path(trace).read_text(errors='replace').split('\n'), start=1):
        try:
            event = json.loads(line)
        except ValueError:
            continue
        if not isinstance(event, dict):
            continue
        content = (event.get('message') or {}).get('content') if isinstance(event.get('message'), dict) else None
        for block in content if isinstance(content, list) else []:
            if not isinstance(block, dict):
                continue
            if event.get('type') == 'assistant' and block.get('type') == 'tool_use':
                call = {'id': block.get('id'), 'name': block.get('name'), 'command': (block.get('input') or {}).get('command', '')
                        if block.get('name') == 'Bash' else json.dumps(block.get('input') or {})[:300],
                        'start': times.get(number), 'end': None, 'is_error': None, 'line': number}
                calls.append(call)
                by_id[call['id']] = call
            elif event.get('type') == 'user' and block.get('type') == 'tool_result' and block.get('tool_use_id') in by_id:
                call = by_id[block['tool_use_id']]
                call['end'], call['is_error'] = times.get(number), bool(block.get('is_error'))
    return calls


def outstanding_call(trace):
    """The last tool command with no result yet (what the CLI was running), or None."""
    pending = [c for c in tool_calls(trace) if c['end'] is None and c['is_error'] is None]
    if not pending:
        return None
    c = pending[-1]
    return {'id': c['id'], 'tool': c['name'], 'command': redact.scrub(c['command'])[:500], 'line': c['line']}


def service_commands(trace, arrivals, launched_m):
    """The commands event (g) and "victim copes" work from, on the run's clock: any kill (with
    whether it is the version 1 kill-like pattern), the arm's down/restart and up, and test runs."""
    found = []
    for c in tool_calls(trace, arrivals):
        if c['name'] != 'Bash':
            continue
        command = c['command']
        kinds = [k for k, rx in (('kill', ANY_KILL), ('down_restart', DOWN_RESTART), ('up', UP), ('test', TESTS)) if rx.search(command)]
        if not kinds:
            continue
        found.append({'start_m': None if c['start'] is None else round(launched_m + c['start'], 3),
                      'end_m': None if c['end'] is None else round(launched_m + c['end'], 3), 'is_error': c['is_error'],
                      'kinds': kinds, 'kill_like': bool(KILL.search(command)), 'command': redact.scrub(command)[:2000]})
    return found


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


SETTLE_SECONDS = 5
CORE_SERVICE = re.compile(r'(/bin/postgres |redis-server|python (app|worker)\.py|uv run|__supervise)')
LATE_S = 1.0   # a kill or a recovery launch later than planned by more than this is flagged


def machine_budget(config, load_before):
    def sysctl(name):
        try:
            return subprocess.run(['sysctl', '-n', name], capture_output=True, text=True, timeout=10).stdout.strip() or None
        except (OSError, subprocess.TimeoutExpired):
            return None
    memory = sysctl('hw.memsize')
    return {'cores': os.cpu_count(), 'performance_cores': sysctl('hw.perflevel0.physicalcpu'),
            'efficiency_cores': sysctl('hw.perflevel1.physicalcpu'),
            'memory_bytes': int(memory) if memory and memory.isdigit() else None,
            'load_1m_before_bystander': round(load_before, 2), 'load_1m_after_last_agent': None,
            'concurrent_sessions': len(config['tasks']),
            'session_budget': {'seconds': config['agent_seconds'], 'usd': config['max_usd_per_agent']}}


def rows_of(processes, keys=('pid', 'started', 'command')):
    return [{k: p.get(k) for k in keys} for p in processes]


class Missed(Exception):
    pass


class Cancelled(Exception):
    pass


class Shutdown(Exception):
    """A session launch refused because the run's cleanup has begun."""


class Injector(threading.Thread):
    """Fault injection (PROTOCOL.md, Version 2, "Fault injection"). The trigger, the same in both
    scenarios and arms, is the first sample in which all four services of the victim's worktree run
    and listen. The kill goes to exactly one process, whose identity is checked again immediately
    before the signal. `agent-kill` then starts the recovery session, timed from the kill."""

    def __init__(self, scenario, victim, group, params, clock, session, groups_fn, launch_recovery, staging_trace, progress):
        super().__init__(daemon=True)
        self.scenario, self.victim, self.group, self.params = scenario, victim, group, params
        self.clock, self.session, self.groups_fn, self.launch_recovery = clock, session, groups_fn, launch_recovery
        self.staging_trace, self.progress = staging_trace, progress
        self.launch_m = None
        self.triggers = queue.Queue()
        self.found = False
        self.cancel = threading.Event()
        self.invalid = []
        self.helpers = []
        self.result = {'scenario': scenario, 'victim': victim, 'worktree': group, 'injected': False, 'missed': None,
                       'params': dict(params), 'launch_m': None}

    def on_sample(self, info):
        """Called by the sampler thread after every sample (its time is taken after its listings)."""
        if self.found:
            return
        ready = observe.ready_services(self.group, info['groups'], info['owner'], info['listeners'], info['rows'])
        if all(kind in ready for kind in observe.SERVICES):
            self.found = True
            self.triggers.put({'m': info['m'], 'services': ready})

    def victim_exited(self):
        session = self.session()
        return session is None or session['exited'].is_set()

    def sleep_until(self, deadline_m, watch_victim=True, why='the victim CLI exited before the kill was due'):
        while self.clock() < deadline_m:
            if self.cancel.is_set():
                raise Cancelled()
            if watch_victim and self.victim_exited():
                raise Missed(why)
            time.sleep(min(0.05, max(0.0, deadline_m - self.clock())))

    def run(self):
        try:
            self.inject()
        except Missed as missed:
            self.result['missed'] = str(missed)
        except Cancelled:
            self.result['cancelled'] = True
        except Exception as error:
            import traceback
            self.invalid.append(f'fault injection error: {error!r}')
            self.result['error'] = traceback.format_exc()[-2000:]
        finally:
            for helper in self.helpers:
                helper.join(observe.OBSERVE_TIMEOUT * 3)

    def listing(self):
        groups = self.groups_fn([self.group])
        rows = observe.attributed(groups)[self.group]
        listeners, _ = observe.tcp()
        listeners.pop('by_endpoint', None)
        pids = {p['pid'] for p in rows}
        return {'processes': rows_of(rows), 'listening': {str(port): pid for port, pid in listeners.items() if pid in pids},
                'services': {kind: [p['pid'] for p in rows if observe.service_kind(p['command']) == kind] for kind in observe.SERVICES}}

    def in_background(self, key, fn):
        def go():
            started = self.clock()
            try:
                self.result[key] = {'m': started, **fn()}
            except Exception as error:
                self.result[key] = {'m': started, 'error': repr(error)}
            self.result[key]['done_m'] = self.clock()
        thread = threading.Thread(target=go, daemon=True)
        thread.start()
        self.helpers.append(thread)

    def inject(self):
        p = self.params
        trigger = None
        while trigger is None:
            if self.cancel.is_set():
                raise Cancelled()
            if self.victim_exited():
                raise Missed('the victim CLI exited before the trigger')
            if self.clock() > self.launch_m + p['fault_deadline_s']:
                raise Missed(f"no trigger within {p['fault_deadline_s']} s of launch")
            try:
                trigger = self.triggers.get(timeout=0.2)
            except queue.Empty:
                pass
        self.result['trigger'] = {**trigger, 'seconds_after_launch': round(trigger['m'] - self.launch_m, 3)}
        self.progress('trigger')
        due = trigger['m'] + p['kill_delay_s']
        # What the victim's worktree runs, listed in the 2 s before the kill, in a thread, so that a
        # slow listing can never delay the kill.
        self.sleep_until(due - 2.0)
        self.in_background('running_at_kill', self.listing)
        self.sleep_until(due)
        session = self.session()
        # Immediately before the signal: the planned process, still the same, still the victim's.
        if self.scenario == 'agent-kill':
            target = {'pid': session['pid'], 'started': session['pid_started'], 'command': None, 'what': 'agent CLI'}
            identity = observe.legacy.process_identity(target['pid'])
            if self.victim_exited() or identity is None or identity['started'] != target['started']:
                raise Missed('the victim CLI exited before the kill was due')
            checks = {'started': True, 'cli_running': True}
        else:
            planned = trigger['services']['postgres']
            target = {'pid': planned['pid'], 'started': planned['started'], 'command': planned['command'], 'what': 'postmaster'}
            paths = self.groups_fn([self.group]).get(self.group) or []
            identity = observe.legacy.process_identity(target['pid'])
            if identity is None or identity['started'] != target['started'] or identity['command'].rstrip()[:300] != target['command'].rstrip():
                raise Missed(f"the planned postmaster (pid {target['pid']}) was gone or replaced when the kill was due")
            data_dir = observe.postmaster_data_dir(identity['command']) or ''
            mark = observe.group_of_marker(observe.mark_of(target['pid']))
            checks = {'data_dir': data_dir if observe.under_any(paths, data_dir) else None,
                      'owner': mark == self.group or observe.under_any(paths, identity['command']),
                      'listening': observe.listens(target['pid']), 'cli_running': not self.victim_exited()}
            if not all(checks.values()):
                raise Missed(f"the planned postmaster no longer qualified when the kill was due: {checks}")
        try:
            os.kill(target['pid'], signal.SIGKILL)
        except ProcessLookupError:
            raise Missed(f"the planned {target['what']} (pid {target['pid']}) was gone at the signal")
        kill_m = self.clock()
        self.progress('kill')
        outstanding = outstanding_call(self.staging_trace) if self.staging_trace else None
        deadline = time.monotonic() + 5
        hit = False
        while time.monotonic() < deadline:
            now_identity = observe.legacy.process_identity(target['pid'])
            gone = now_identity is None or now_identity['started'] != target['started']
            hit = gone and (self.scenario != 'agent-kill' or (session['exited'].is_set() and session['proc'].returncode == -signal.SIGKILL))
            if hit:
                break
            time.sleep(0.05)
        late = round(kill_m - due, 3)
        self.result.update(injected=True, kill={**target, 'm': kill_m, 'planned_m': round(due, 3), 'late_s': late,
                                                'late_flag': late > LATE_S, 'checks': checks,
                                                'seconds_after_launch': round(kill_m - self.launch_m, 3),
                                                'seconds_after_trigger': round(kill_m - trigger['m'], 3), 'hit_confirmed': hit,
                                                'returncode': session['proc'].returncode if self.scenario == 'agent-kill' else None},
                           outstanding_at_kill=outstanding)
        if not hit:
            self.invalid.append(f"the harness kill could not be shown to have hit the planned {target['what']} (pid {target['pid']})")
            return
        if self.scenario != 'agent-kill':
            return
        session['record']['killed_by_harness'] = True
        planned = kill_m + p['recovery_delay_s']
        killed_pid = target['pid']

        def handoff():
            # What the killed CLI started that still runs at the handoff: processes of the worktree
            # carrying its role marker, or still in its process group (its tool commands). Listed in
            # the 2 s before the launch, in a thread, so it never delays the launch.
            groups = self.groups_fn([self.group])
            rows = observe.attributed(groups)[self.group]
            _, roles = observe.marks_and_roles()
            left = [p for p in rows if p['pid'] != killed_pid and (roles.get(p['pid']) == 'agent' or p.get('pgid') == killed_pid)]
            return {'killed_cli_processes_running': rows_of(left, ('pid', 'started', 'command')),
                    'any_running': bool(left), 'tool_commands_in_group': [p['pid'] for p in left if p.get('pgid') == killed_pid]}
        self.sleep_until(planned - 2.0, watch_victim=False)
        self.in_background('handoff', handoff)
        self.sleep_until(planned, watch_victim=False)    # timed from the kill; nothing else waits here
        try:
            key = self.launch_recovery()
        except Shutdown:
            self.result['recovery'] = {'refused': 'the run was shutting down'}
            return
        except Exception as error:
            self.invalid.append(f'the recovery agent could not be started: {error!r}')
            return
        launched = self.clock()
        self.result['recovery'] = {'session': key, 'launched_m': launched, 'planned_m': round(planned, 3),
                                   'late_s': round(launched - planned, 3), 'late_flag': launched - planned > LATE_S,
                                   'seconds_after_kill': round(launched - kill_m, 3)}


def unconfirmed_commands(code, stdout):
    """`stack --json down` exiting 70 with stop_unconfirmed: the commands of this checkout it names
    (details with kind "command", not another checkout's). None otherwise. Parsed from the full
    stdout, never from the truncated record."""
    if code != 70:
        return None
    payload = None
    for line in reversed((stdout or '').strip().splitlines()):
        try:
            payload = json.loads(line)
            break
        except ValueError:
            continue
    if not isinstance(payload, dict) or payload.get('ok') is not False:
        return None
    error = payload.get('error') or {}
    if error.get('code') != 'stop_unconfirmed':
        return None
    named = [{k: d.get(k) for k in ('pid', 'program', 'started_by', 'found_by', 'running_secs')} for d in error.get('details') or []
             if isinstance(d, dict) and d.get('kind') == 'command' and not d.get('another_checkout') and isinstance(d.get('pid'), int)]
    return named or None


def stop_pids(pids, root, grace=10):
    """What `stack down`'s hint says to do: stop those PIDs (SIGTERM, 10 s, then SIGKILL). The caller
    passes only pids the harness's own attribution places in this worktree; the start time is
    checked before every signal."""
    rows, result = [], {'terminated': [], 'killed': [], 'gone': [], 'survivors': []}
    for pid in pids:
        identity = observe.legacy.process_identity(pid)
        if identity is None:
            result['gone'].append(pid)
        else:
            rows.append({'pid': pid, **identity})
    stopped = observe.reclaim(rows, root, grace=grace) if rows else {'terminated': [], 'killed': [], 'not_signalled': [], 'survivors': []}
    for key in ('terminated', 'killed', 'survivors'):
        result[key] = stopped[key]
    result['processes'] = rows_of(rows)
    return result


def stack_service_dirs(arm, main, env, worktrees):
    """Each worktree's PostgreSQL and Redis data directories, from Stack's own records. A worktree
    Stack has a session for but does not list is an unknown inventory (raises: the run is invalid)."""
    r = subprocess.run([arm.stack, '--json', 'ps'], cwd=main, env=env, capture_output=True, text=True, timeout=120)
    try:
        data = json.loads(r.stdout)['data']
    except (ValueError, KeyError):
        raise RuntimeError(f'unknown inventory: stack ps failed while locating service data: {r.stdout[-300:]} {r.stderr[-300:]}')
    dirs, listed = {}, set()
    for checkout in data.get('checkouts', []):
        listed.add(os.path.realpath(checkout['path']))
        for service in checkout.get('services', []):
            if service.get('name') in ('postgres', 'redis') and service.get('data_dir'):
                dirs.setdefault(os.path.realpath(checkout['path']), {})[service['name']] = service['data_dir']
    out = {}
    for task, path in worktrees.items():
        real = os.path.realpath(path)
        if (path / '.stack').exists() and real not in listed:
            raise RuntimeError(f'unknown inventory: {task} has Stack state but stack ps does not list it')
        out[task] = dirs.get(real, {})
    return out


def run_arm(plan, pair_index, arm_name, out_dir, agent_override=None, keep_root=False, tasks=None, fault=None, down_override=None):
    """One arm of one pair (PROTOCOL.md, Trial flow, as amended by Version 2).

    Self-tests and validation only: `fault` overrides the pair's planned scenario and victim,
    `agent_override(task, role)` replaces the agent command (scripted agents) and
    `down_override(task, argv)` replaces the arm's down command for a worktree (planted collateral)."""
    global WARM
    config = dict(plan['config'])
    selftest = plan.get('kind') == 'harness_selftest'
    if (agent_override or down_override or fault is not None or tasks) and not selftest:
        raise SystemExit('overrides are for self-test plans only')
    if tasks:
        config['tasks'] = [t for t in config['tasks'] if t in tasks]
    pair = next((p for p in plan.get('pairs', []) if p.get('index') == pair_index), {})
    if fault is None:
        fault = {'scenario': pair.get('scenario'), 'victim': pair.get('victim')}
    if not fault.get('scenario') or fault.get('victim') not in config['tasks']:
        fault = None
    elif fault['scenario'] not in SCENARIOS:
        raise SystemExit(f"unknown scenario {fault['scenario']!r}")
    params = dict(FAULT_PARAMS, **(config.get('fault') or {}))
    WARM = Path(config.get('warm_cache') or WARM)  # the plan's warm cache, the one it records
    arm = Arm(arm_name, config['stack_binary'])
    cli = config['agent_command'][0] if config.get('agent_command') else None
    cli_dir = str(Path(cli).parent) if cli and cli.startswith('/') else None
    out_dir = Path(out_dir)
    out_dir.mkdir(parents=True, exist_ok=False)
    load_before = os.getloadavg()[0]
    root = Path(tempfile.mkdtemp(prefix=f'avr{pair_index}{arm_name[0]}-', dir='/tmp'))
    run_id = root.name.replace('_', '-')
    observe.RUN_SUFFIX = f'.{run_id}'
    t0 = time.monotonic()
    clock = lambda: round(time.monotonic() - t0, 3)
    record = {'protocol_version': PROTOCOL_VERSION, 'pair': pair_index, 'attempt': plan.get('_attempt', 1), 'arm': arm_name,
              'root': str(root), 'run_id': run_id,
              'started': now(), 'clock': {'note': 'every *_m value is monotonic seconds since this run started', 'started_wall': now()},
              'steps': [], 'agents': {}, 'slots': {},
              'scenario': fault['scenario'] if fault else None, 'victim': fault['victim'] if fault else None,
              'plan_sha256': plan.get('_sha256'), 'plan_name': plan.get('name'), 'plan_kind': plan.get('kind'),
              'model': config['model'], 'effort': config['effort'], 'tasks': config['tasks'],
              'stack_binary_sha256': sha256(config['stack_binary']), 'stack_rev': config.get('stack_rev'),
              'warm_cache': str(WARM), 'source_rev': git_rev(), 'machine': machine_budget(config, load_before),
              'valid': True, 'invalid_reasons': []}
    steps = record['steps']
    sampler, injector, sessions = None, None, {}
    stage = root / 'stage'
    launch_lock = threading.Lock()
    state = {'shutting_down': False}
    launch_log = []
    pause = float(os.environ.get('AGENTVALUE_SELFTEST_LAUNCH_PAUSE') or 0) if selftest else 0.0
    progress_file = out_dir / 'progress.jsonl'

    def progress(event):
        with open(progress_file, 'a') as handle:
            handle.write(json.dumps({'event': event, 'm': clock()}) + '\n')

    def invalid(reason):
        record['valid'] = False
        record['invalid_reasons'].append(reason)

    def marker(name):
        return f'{name}.{run_id}'

    # For the run: SIGINT and SIGTERM interrupt it (also when the caller inherited them ignored, as a
    # background shell does), so no agent or service inherits an ignored signal. Restored at the end.
    in_main = threading.current_thread() is threading.main_thread()
    saved_handlers = {}
    if in_main:
        for signum in (signal.SIGINT, signal.SIGTERM):
            saved_handlers[signum] = signal.signal(signum, interrupt)

    try:
        grade.ensure_grader_venv()
        bin_dir = arm.bin_dir(root)
        stage.mkdir()
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
        addresses = parse_env(sh(arm.env_print(), main, env_main, log=steps, name='bystander_addresses')['stdout'])
        if not {'DATABASE_URL', 'REDIS_URL'} <= set(addresses):
            raise RuntimeError(f'could not read the bystander addresses: {sorted(addresses)}')
        canary = hashlib.sha256(f'{root}{now()}'.encode()).hexdigest()[:12]
        record['canary_id'] = canary
        genv = grade.base_env(addresses)
        planted = sh([grade.GRADER_PY, HERE / 'canary.py', 'plant', canary], main, genv, log=steps, name='canary_plant')
        if planted['exit'] != 0:
            raise RuntimeError('canary plant failed')
        time.sleep(2.5)  # let PostgreSQL flush the planting session's write counters (diagnostic only)
        before = json.loads(sh([grade.GRADER_PY, HERE / 'canary.py', 'fingerprint'], main, genv, log=steps, name='canary_before')['stdout'])
        groups_of = lambda names: owned_paths(arm, main, env_main, {n: (main if n == 'main' else root / n) for n in names})
        durable = lambda rows: [r for r in rows if CORE_SERVICE.search(r['command'])]
        bystander_procs = durable(observe.attributed(groups_of(['main']))['main'])
        record['bystander'] = {'addresses': addresses, 'fingerprint_before': before, 'processes_before': rows_of(bystander_procs)}

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
        lock = threading.Lock()

        def watch(key, task, proc, started_mono, out, err, tailer, exited):
            timed_out = False
            try:
                proc.wait(config['agent_seconds'])
            except subprocess.TimeoutExpired:
                timed_out = True
                stop_tree(proc)
            ended_mono = time.monotonic()
            exited.set()
            out.close(); err.close()
            tailer.stop()
            if state['shutting_down']:
                with lock:
                    record['agents'][key].update(ended=now(), ended_m=round(ended_mono - t0, 3), exit=proc.returncode,
                                                 seconds=round(ended_mono - started_mono, 2), stopped_by_shutdown=True)
                return
            # Everything attributed to the worktree at the agent's exit, the CLI's own helpers included
            # (listed separately: the version 1 at-exit gate does not count them).
            groups = groups_of([names[task]])
            left = observe.attributed(groups, exclude={proc.pid})[names[task]]
            listeners, _ = observe.tcp()
            listeners.pop('by_endpoint', None)
            helpers = cli_paths(cli) if cli and not agent_override else ()
            cli_own = [p for p in left if helpers and p['command'].startswith(helpers)]
            left = [p for p in left if p not in cli_own]
            left_pids = {p['pid'] for p in left}
            with lock:
                record['agents'][key].update(
                    ended=now(), ended_m=round(ended_mono - t0, 3), seconds=round(ended_mono - started_mono, 2), exit=proc.returncode,
                    timed_out=timed_out, observed_after_exit_s=round(time.monotonic() - ended_mono, 2),
                    left_running=rows_of(left, ('pid', 'started', 'command', 'marker')),
                    cli_helpers_left=rows_of(cli_own, ('pid', 'command')),
                    left_listening=sorted(port for port, pid in listeners.items() if pid in left_pids))

        def launch(task, role):
            """Start one session. Under one lock with the start of shutdown: once cleanup has begun no
            session can start, and every child is registered the moment it exists."""
            key = task if role == 'agent' else f'{task}-recovery'
            with launch_lock:
                if state['shutting_down']:
                    raise Shutdown()
                if role == 'recovery':
                    progress('recovery_launch_begin')
                    if pause:
                        time.sleep(pause)   # self-test hook: hold a launch open so an interrupt can land in it
                adir = out_dir / 'agents' / key
                adir.mkdir(parents=True)
                sdir = stage / key
                sdir.mkdir(parents=True)
                text = (PROMPT if role == 'agent' else RECOVERY_PROMPT).format(branch=TASK[task]['branch'], instruction=TASK[task]['instruction'])
                (adir / 'prompt.txt').write_text(text)
                template = agent_override(task, role) if agent_override else config['agent_command']
                argv = substitute(template, {'prompt': text, 'model': config['model'], 'effort': config['effort'],
                                             'max_usd': config['max_usd_per_agent'], 'task': task})
                env = machine_env(root, bin_dir, marker=marker(names[task]), cli_dir=cli_dir, AGENTVALUE_TASK=task,
                                  AGENTVALUE_ARM=arm.name, AGENTVALUE_ROLE=role)
                env.update(AGENT_CONSTANTS)
                (adir / 'argv.json').write_text(json.dumps([a if a != text else '{prompt}' for a in argv]))
                out = open(sdir / 'trace.jsonl', 'wb')
                err = open(sdir / 'stderr.txt', 'wb')
                started = now()
                started_mono = time.monotonic()
                proc = subprocess.Popen(argv, cwd=worktrees[task], env=env, stdout=out, stderr=err, stdin=subprocess.DEVNULL,
                                        start_new_session=True)
                exited = threading.Event()
                entry = {'task': task, 'role': role, 'worktree': str(worktrees[task]), 'marker': names[task], 'started': started,
                         'launched_m': round(started_mono - t0, 3), 'pid': proc.pid, 'pid_started': None,
                         'budget': {'seconds': config['agent_seconds'], 'usd': config['max_usd_per_agent']}}
                with lock:
                    record['agents'][key] = entry
                    sessions[key] = {'proc': proc, 'pid': proc.pid, 'pid_started': None, 'exited': exited, 'record': entry,
                                     'stage': sdir, 'out': adir, 'launched_mono': started_mono}
                launch_log.append({'session': key, 'm': entry['launched_m']})
                identity = observe.legacy.process_identity(proc.pid)
                entry['pid_started'] = sessions[key]['pid_started'] = identity and identity['started']
                tailer = TraceTimes(sdir / 'trace.jsonl', adir / ARRIVALS, started_mono)
                tailer.start()
                thread = threading.Thread(target=watch, args=(key, task, proc, started_mono, out, err, tailer, exited), daemon=True)
                sessions[key]['thread'] = thread
                thread.start()
            if role == 'recovery':
                progress('recovery_launched')
            return key

        if fault:
            victim = fault['victim']
            injector = Injector(fault['scenario'], victim, names[victim], params, clock, lambda: sessions.get(victim), groups_of,
                                lambda: launch(victim, 'recovery'), stage / victim / 'trace.jsonl', progress)
        sampler = observe.Sampler(lambda: groups_of(all_names), out_dir / 'sampler.jsonl', interval=config.get('sample_interval', 1.0),
                                  ignore_clients=(str(bin_dir / 'stack'), arm.stack), t0=t0,
                                  listeners=[injector.on_sample] if injector else [])
        sampler.start()

        # Launch every agent at once; each one's exit is observed by its own thread.
        for task in worktrees:
            launch(task, 'agent')
        record['launch_m'] = clock()
        progress('launched')
        if injector:
            injector.launch_m = injector.result['launch_m'] = record['launch_m']
            injector.start()
        while True:  # the episode: until every session (recovery included) has ended and no fault is pending
            with lock:
                live = [s['thread'] for s in sessions.values() if s['thread'].is_alive()]
            if not live and not (injector and injector.is_alive()):
                break
            time.sleep(0.2)
        record['episode_end_m'] = clock()
        progress('episode_end')
        record['machine']['load_1m_after_last_agent'] = round(os.getloadavg()[0], 2)
        for key, entry in record['agents'].items():
            if 'left_running' not in entry:
                raise RuntimeError(f'no exit observation for {key}')
        if injector:
            record['fault'] = injector.result
            for reason in injector.invalid:
                invalid(reason)
            record['fault']['running_at_end'] = {'m': clock(), 'processes': rows_of(observe.attributed(groups_of([names[fault['victim']]]))[names[fault['victim']]])}

        # Each worktree's scored slot: the recovery session where the victim was killed.
        for task in worktrees:
            record['slots'][task] = f'{task}-recovery' if f'{task}-recovery' in record['agents'] else task

        # A process an agent signalled last may still be shutting down (the worker finishes its
        # current one-second queue wait on SIGTERM). The sampler keeps running through the teardown.
        time.sleep(SETTLE_SECONDS)

        # Bystander integrity after the episode.
        after = json.loads(sh([grade.GRADER_PY, HERE / 'canary.py', 'fingerprint'], main, genv, log=steps, name='canary_after')['stdout'] or 'null')
        alive = {(p['pid'], p['started']) for p in observe.attributed(groups_of(['main']))['main']}
        lost = [p for p in record['bystander']['processes_before'] if (p['pid'], p['started']) not in alive]
        if after is None and not lost:
            invalid('could not read the bystander fingerprint after the agents, with its services still running')
        changed = sorted(k for k in before if (after or {}).get(k) != before[k]) if after is not None else ['unreadable']
        record['bystander'].update(fingerprint_after=after, changed=changed, processes_lost=lost)

        # Each submission (the worktree's final tree), captured before anything touches the worktrees.
        submissions = {task: capture(path, start_commit, out_dir / 'agents' / record['slots'][task], env_main)
                       for task, path in worktrees.items()}

        # The teardown procedure (PROTOCOL.md, Version 2, item 4): the arm's documented procedure on
        # every worktree exactly as it was left; nothing is cleaned by the harness first.
        progress('teardown_begin')
        teardown_groups = groups_of([names[t] for t in worktrees])
        if arm.name == 'stack':
            service_dirs = stack_service_dirs(arm, main, env_main, worktrees)
        else:
            service_dirs = {task: {'postgres': str(path / '.dev/postgres'), 'redis': str(path / '.dev/redis')} for task, path in worktrees.items()}

        def observation(name):
            rows = observe.attributed({name: teardown_groups[name]})[name]   # raises if a listing fails: unknown inventory
            listeners, _ = observe.tcp()
            listeners.pop('by_endpoint', None)
            pids = {p['pid'] for p in rows}
            helpers = cli_paths(cli) if cli and not agent_override else ()
            return rows, {'m': clock(), 'processes': rows_of(rows, ('pid', 'started', 'command', 'marker')),
                          'ports': sorted(port for port, pid in listeners.items() if pid in pids),
                          'of_which_cli_helpers': [p['pid'] for p in rows if helpers and p['command'].startswith(helpers)]}

        main_group = groups_of(['main'])['main']

        def durable_listing():
            """Durable service processes of every worktree and the main checkout, by pid and start time."""
            rows = observe.attributed({**teardown_groups, 'main': main_group})
            return {(p['pid'], p['started']): {'group': g, 'pid': p['pid'], 'started': p['started'], 'command': p['command'][:200]}
                    for g, found in rows.items() for p in found if observe.DURABLE.search(p['command'])}

        teardown, leftovers, windows, torn = {}, {}, [], set()
        previous_after = None
        for task, path in worktrees.items():
            name = names[task]
            # Teardown collateral: the harness's own listing just before and just after this worktree's
            # procedure. Lost in between: a service of a worktree not yet torn down, or of the main checkout.
            listing_before = durable_listing()
            torn.add(name)
            wenv = machine_env(root, bin_dir, marker=marker(name))
            entry = {'procedure': [], 'rounds': [], 'service_dirs': service_dirs[task]}
            window = {'task': task, 'group': name, 'start_m': clock()}
            argv = down_override(task, arm.down()) if down_override else arm.down()
            r, raw = sh(argv, path, wenv, log=entry['procedure'], name='arm_down', keep_raw=True)
            entry['down_exit_first'] = r['exit']
            entry['down_first_status'] = 'timeout' if r['exit'] == 'timeout' else 'zero' if r['exit'] == 0 else 'nonzero'
            first_rows, entry['after_first_down'] = observation(name)
            entry['clean_after_first_down'] = not entry['after_first_down']['processes'] and not entry['after_first_down']['ports']
            follow_up = clock()
            if arm.name == 'stack':
                for round_number in (1, 2, 3):
                    named = unconfirmed_commands(r['exit'], raw)
                    if not named:
                        break
                    here = {p['pid'] for p in observe.attributed({name: teardown_groups[name]})[name]}
                    ours = [d['pid'] for d in named if d['pid'] in here]
                    stopped = stop_pids(ours, root)
                    r, raw = sh(argv, path, wenv, log=entry['procedure'], name=f'arm_down_round{round_number}', keep_raw=True)
                    entry['rounds'].append({'round': round_number, 'named': named, 'signalled': ours,
                                            'left_alone_not_attributed_here': [d['pid'] for d in named if d['pid'] not in here],
                                            'stopped': stopped, 'down_exit': r['exit']})
            entry['down_exit'] = r['exit']
            entry['follow_up_seconds'] = round(clock() - follow_up, 2) if entry['rounds'] else 0.0
            if entry['rounds']:
                rows, entry['after_down'] = observation(name)
            else:
                rows, entry['after_down'] = first_rows, entry['after_first_down']
            entry['clean_after_procedure'] = not entry['after_down']['processes'] and not entry['after_down']['ports']
            # A false confirmation: the first down exited 0 while the first observation finds a process.
            entry['false_confirmation'] = entry['down_exit_first'] == 0 and bool(entry['after_down']['processes'])
            listing_after = durable_listing()
            entry['collateral'] = [v for k, v in listing_before.items() if k not in listing_after and v['group'] != name
                                   and (v['group'] == 'main' or v['group'] not in torn)]
            # Gone between the previous worktree's teardown and this one (a slow exit, say): listed for review.
            entry['lost_before_this_teardown'] = [v for k, v in (previous_after or {}).items() if k not in listing_before
                                                  and (v['group'] == 'main' or v['group'] not in torn)]
            previous_after = listing_after
            window['end_m'] = clock()
            windows.append(window)
            teardown[task] = entry
            leftovers[task] = rows
        record['teardown_windows'] = windows
        # Only now does the harness stop anything left, recorded separately. It counts as left.
        for task, rows in leftovers.items():
            if rows:
                teardown[task]['harness_reclaim'] = observe.reclaim(rows, root)
                if teardown[task]['harness_reclaim']['survivors']:
                    invalid(f'processes of {task} survived reclaim')
        record['teardown'] = teardown

        # Every worktree database: which migrations ran in it, compared with every worktree's files.
        own = {task: set(info['migrations']) for task, (_, info) in submissions.items()}
        original = set(p.stem for p in (ROOT / 'fixture/migrations').glob('*.sql'))
        dbcheck = {}
        with tempfile.TemporaryDirectory(prefix='avd-', dir='/tmp') as tmp:
            for task, path in worktrees.items():
                data_dir = service_dirs[task].get('postgres')
                result = read_migrations(data_dir, Path(tmp), canary) if data_dir else {'exists': False}
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

        # Hidden grading, identical for both arms, of each worktree's final tree; the score goes to its slot.
        for task, (tree, info) in submissions.items():
            slot = record['slots'][task]
            g = grade.grade(task, tree, out_dir / 'agents' / slot)
            record['agents'][slot]['grade'] = {k: g[k] for k in ['passed', 'fresh_ok', 'upgrade_completed', 'checks', 'failed_checks']}
            record['agents'][slot]['submission'] = info
            if g['grader_cleanup']['survivors']:
                invalid(f'grader processes survived for {task}')
            if not g['upgrade_completed'] and not g['upgrade'].get('error'):
                invalid(f'the hidden checks did not complete for {task}')
        for task in worktrees:
            if 'grade' not in record['agents'].get(record['slots'][task], {}):
                invalid(f'missing grade or session for {task}')

        # Removal of each worktree, then (Stack) gc after the last one; the second observation.
        removal = {}
        for task, path in worktrees.items():
            removal[task] = sh([*GIT, 'worktree', 'remove', '--force', str(path)], main, env_main, name='worktree_remove')
            teardown[task]['procedure'].append(removal[task])
        if arm.name == 'stack':
            removal['gc'] = sh([arm.stack, '--json', 'gc', '--reclaim-data'], main, env_main, log=steps, name='gc_reclaim')
        left = observe.attributed(teardown_groups)
        for task, path in worktrees.items():
            data_left = {p: du(p) for p in service_dirs[task].values() if Path(p).is_dir() and any(Path(p).iterdir())}
            found = left.get(names[task]) or []
            entry = teardown[task]
            entry['after_removal'] = {'m': clock(), 'worktree_dir_exists': path.exists(), 'service_data_left': data_left,
                                      'processes': rows_of(found, ('pid', 'started', 'command'))}
            if found:  # found at the post-removal scan and reclaimed: still residue of this worktree
                entry['after_removal']['harness_reclaim'] = observe.reclaim(found, root)
            entry['residue_ok'] = not entry['after_down']['processes'] and not entry['after_down']['ports'] \
                and not path.exists() and not data_left and not found
        listeners, _ = observe.tcp()
        listeners.pop('by_endpoint', None)
        agent_ports = {int(p) for p, marks in sampler.summary()['ports'].items() if set(marks) - {'main'}}
        record['disposal'] = {
            'steps': removal,
            'processes_left': {m: rows_of(rows, ('pid', 'command')) for m, rows in left.items() if rows},
            'ports_still_listening': sorted(p for p in agent_ports if p in listeners),
            'worktree_dirs_left': [str(p) for p in worktrees.values() if p.exists()],
            'service_data_left': {t: teardown[t]['after_removal']['service_data_left'] for t in worktrees if teardown[t]['after_removal']['service_data_left']},
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

        # The bystander again, after the last worktree is removed and before its own down.
        final_fp = json.loads(sh([grade.GRADER_PY, HERE / 'canary.py', 'fingerprint'], main, genv, log=steps, name='canary_after_teardown')['stdout'] or 'null')
        alive = {(p['pid'], p['started']) for p in observe.attributed(groups_of(['main']))['main']}
        record['bystander']['after_teardown'] = {
            'm': clock(), 'changed_since_episode': sorted(k for k in (after or {}) if (final_fp or {}).get(k) != after[k]
                                                         and k != 'tuple_writes') if after is not None else [],
            'unreadable': final_fp is None,
            'processes_lost': [p for p in record['bystander']['processes_before'] if (p['pid'], p['started']) not in alive and p not in lost]}
        record['teardown_end_m'] = clock()
        sampler.stop()
        record['sampler'] = sampler.summary()
        if record['sampler']['errors']:
            invalid(f"sampler errors: {record['sampler']['errors'][:3]}")

        # The main checkout's own teardown, then a last scan of the whole run.
        sh(arm.down(), main, env_main, log=steps, name='bystander_down')
        final = observe.run_processes(root)
        record['final_scan'] = rows_of(final, ('pid', 'command'))
        if final:
            record['final_reclaim'] = observe.reclaim(final, root)
    except BaseException as error:
        import traceback
        invalid(f'interrupted: {error!r}' if isinstance(error, KeyboardInterrupt) else f'harness error: {error!r}')
        record['traceback'] = traceback.format_exc()
    finally:
        # Shutdown: first nothing may start a session, then cleanup, then the check that nothing is left.
        if in_main:   # cleanup is not interrupted; the previous handlers come back at the very end
            for signum in (signal.SIGINT, signal.SIGTERM):
                signal.signal(signum, signal.SIG_IGN)
        with launch_lock:
            state['shutting_down'] = True
        record['shutdown_m'] = clock()
        try:
            progress('shutdown_begin')
        except OSError:
            pass
        try:
            if injector is not None and injector.is_alive():
                injector.cancel.set()
                injector.join(observe.OBSERVE_TIMEOUT * 4)
                if injector.is_alive():
                    invalid('the fault injector did not stop')
            if injector is not None and 'fault' not in record:
                record['fault'] = injector.result
            for key in list(sessions):
                try:
                    os.killpg(sessions[key]['pid'], signal.SIGKILL)
                except (ProcessLookupError, PermissionError):
                    pass
            for key in list(sessions):
                thread = sessions[key].get('thread')
                if thread is not None:
                    thread.join(observe.OBSERVE_TIMEOUT * 3)
            if sampler and sampler.is_alive():
                sampler.stop()
                record.setdefault('sampler', sampler.summary())
            record['launches'] = launch_log
            record['launches_after_shutdown'] = [l for l in launch_log if l['m'] > record['shutdown_m']]
        except Exception as error:
            record.setdefault('cleanup_errors', []).append(repr(error))
            invalid(f'cleanup error: {error!r}')
        # Publish every trace: metrics from the raw staging copy, then only a scrubbed copy is retained.
        for key, s in list(sessions.items()):
            entry, raw, raw_err = record['agents'][key], s['stage'] / 'trace.jsonl', s['stage'] / 'stderr.txt'
            try:
                metrics = trace_metrics(raw)
                commands = metrics.pop('bash_commands')
                entry['trace_metrics'] = metrics
                entry['compliance'] = {'committed': bool((entry.get('submission') or {}).get('commits')),
                                       'ran_tests': any(re.search(r'\b(stack run test|dev\.py test|pytest)\b', c) for c in commands)}
                entry['service_commands'] = service_commands(raw, s['out'] / ARRIVALS, entry.get('launched_m') or 0.0)
                if metrics.get('model') and metrics['model'] != config['model']:
                    invalid(f"{key}: the trace reports model {metrics['model']}, the plan {config['model']}")
                for source, name in ((raw, 'trace.jsonl'), (raw_err, 'stderr.txt')):
                    if source.exists():
                        shutil.copyfile(source, s['out'] / name)
                        redact.scrub_file(s['out'] / name)
            except Exception as error:
                record.setdefault('cleanup_errors', []).append(f'publishing {key}: {error!r}')
                invalid(f'could not publish the trace of {key}: {error!r}')
        try:
            remaining = observe.run_processes(root)
            if remaining:
                record.setdefault('emergency_reclaim', observe.reclaim(remaining, root))
            survivors = observe.run_processes(root)   # after every launch-capable thread has stopped
            record['survivors_after_run'] = rows_of(survivors, ('pid', 'command'))
            if survivors:
                invalid('processes of this run survived every cleanup')
        except Exception as error:
            record.setdefault('cleanup_errors', []).append(repr(error))
            invalid(f'cleanup error: {error!r}')
        if record['launches_after_shutdown'] if 'launches_after_shutdown' in record else False:
            invalid('a session started after cleanup had begun')
        record['ended'] = now()
        record['seconds'] = round(record['ended'] - record['started'], 1)
        # Every retained file is scrubbed, submitted trees and patches included; the changed ones are listed.
        changed = []
        for path in sorted(out_dir.rglob('*')):
            if path.is_file() and not path.is_symlink() and path.name != 'run.json':
                try:
                    if b'\0' not in path.read_bytes()[:8192] and redact.scrub_file(path):
                        changed.append(str(path.relative_to(out_dir)))
                except OSError:
                    pass
        record['scrubbed_files'] = changed
        shutil.rmtree(stage, ignore_errors=True)   # raw traces never outlive the run, kept root or not
        (out_dir / 'run.json').write_text(json.dumps(redact.scrub_json(record), indent=2, default=str))
        if not keep_root and record['valid']:
            shutil.rmtree(root, ignore_errors=True)
        record['root_kept'] = {'path': str(root), 'raw_traces_removed': not stage.exists(),
                               'note': 'kept for inspection: worktrees and service data, no raw traces'} if root.exists() else False
        (out_dir / 'run.json').write_text(json.dumps(redact.scrub_json(record), indent=2, default=str))
        observe.RUN_SUFFIX = None
        for signum, handler in saved_handlers.items():
            signal.signal(signum, handler if handler is not None else signal.SIG_DFL)
    return record


# ------------------------------------------------------------------ plan / pair

def stack_version(binary):
    try:
        r = subprocess.run([str(binary), '--version'], capture_output=True, text=True, timeout=30,
                           env={k: os.environ[k] for k in HOST_VARIABLES if k in os.environ})
        return (r.stdout.strip() or r.stderr.strip()).splitlines()[0] if r.returncode == 0 else None
    except (OSError, IndexError, subprocess.TimeoutExpired):
        return None


def plan_pairs(rng, pairs_per_scenario, tasks):
    """PROTOCOL.md, Version 2, "Plan, order and cost": pairs alternate scenarios (which comes first
    is a seeded coin flip); within a scenario the first pair's first arm is a seeded coin flip and
    later pairs alternate; within a scenario the victims are a seeded permutation of the tasks
    (a fresh permutation for every further block of len(tasks) pairs)."""
    scenarios = list(SCENARIOS) if rng.random() < 0.5 else list(reversed(SCENARIOS))
    first = {s: rng.choice(['stack', 'baseline']) for s in scenarios}
    victims = {}
    for s in scenarios:
        order = []
        while len(order) < pairs_per_scenario:
            block = list(tasks)
            rng.shuffle(block)
            order += block
        victims[s] = order[:pairs_per_scenario]
    pairs = []
    for i in range(2 * pairs_per_scenario):
        s, j = scenarios[i % 2], i // 2
        lead = first[s] if j % 2 == 0 else ('baseline' if first[s] == 'stack' else 'stack')
        pairs.append({'index': i, 'scenario': s, 'victim': victims[s][j], 'order': [lead, 'baseline' if lead == 'stack' else 'stack']})
    return pairs


def cmd_plan(args):
    rng = random.Random(args.seed)
    binary = Path(args.stack).expanduser().resolve()
    if not binary.is_file():
        raise SystemExit(f'no Stack binary at {binary}')
    tasks = [t['id'] for t in TASKS]
    if args.pairs_per_scenario:
        pairs = plan_pairs(rng, args.pairs_per_scenario, tasks)
    else:  # a plan without faults: self-tests and validation
        first = rng.choice(['stack', 'baseline'])
        other = 'baseline' if first == 'stack' else 'stack'
        pairs = [{'index': i, 'scenario': None, 'victim': None, 'order': [first, other] if i % 2 == 0 else [other, first]}
                 for i in range(args.pairs)]
    agent_command = json.loads(args.agent_command) if args.agent_command else [
        'claude', '-p', '{prompt}', '--model', '{model}', '--effort', '{effort}', '--output-format', 'stream-json', '--verbose',
        '--safe-mode', '--strict-mcp-config', '--no-session-persistence', '--disable-slash-commands',
        '--permission-mode', 'bypassPermissions', '--tools', 'Bash,Read,Edit,Write', '--max-budget-usd', '{max_usd}']
    if agent_command and agent_command[0] == 'claude' and shutil.which('claude'):
        # Pin the CLI to its versioned binary: a host-wide update must not change it under a plan.
        agent_command = [os.path.realpath(shutil.which('claude'))] + agent_command[1:]
    cli = Path(agent_command[0]) if agent_command and agent_command[0].startswith('/') else None
    plan = {'protocol_version': PROTOCOL_VERSION, 'kind': args.kind, 'name': args.name, 'created': now(), 'seed': args.seed,
            'protocol_rev': git_rev(),
            'config': {'tasks': tasks, 'model': args.model, 'effort': args.effort,
                       'agent_seconds': args.agent_seconds, 'max_usd_per_agent': args.max_usd, 'agent_command': agent_command,
                       'agent_cli_sha256': sha256(cli) if cli and cli.is_file() else None,
                       'stack_binary': str(binary), 'stack_binary_sha256': sha256(binary), 'stack_rev': args.stack_rev,
                       'stack_version': stack_version(binary), 'source_rev': git_rev(),
                       'warm_cache': str(WARM), 'locale': 'en_US.UTF-8', 'concurrency': len(tasks), 'sample_interval': 1.0,
                       'fault': dict(FAULT_PARAMS), 'faults': bool(args.pairs_per_scenario),
                       'env_allowlist': sorted(ENV_ALLOWLIST),
                       'environment': {'base_path': list(BASE_PATH), 'fixed': FIXED, 'from_host': list(HOST_VARIABLES)}},
            'host': {'platform': platform.platform(), 'node': platform.node(), **tool_versions()},
            'fingerprints': fingerprints(), 'pairs': pairs}
    out = Path(args.out)
    problems = protocol_problems(plan)
    if problems:
        raise SystemExit('refusing to write a trial plan that does not match the protocol: ' + '; '.join(problems))
    out.parent.mkdir(parents=True, exist_ok=True)
    if out.exists():
        raise SystemExit(f'{out} exists; plans are never overwritten')
    out.write_text(json.dumps(plan, indent=2))
    print(json.dumps({'plan': str(out), 'stack': {'rev': args.stack_rev, 'sha256': plan['config']['stack_binary_sha256'],
                                                   'version': plan['config']['stack_version']}, 'pairs': pairs}, indent=2))


VERSIONED_CLI = re.compile(r'/versions/[0-9][0-9A-Za-z.+-]*$')


def protocol_problems(plan):
    """A trial plan (agent_experiment, protocol version 2) must match the protocol: the Stack binary
    and revision, a scenario and a victim in every pair, the agent CLI pinned to a versioned binary
    whose hash the plan records, and the warm cache the runner will use. Self-test plans may differ."""
    if plan.get('kind') != 'agent_experiment':
        return []
    c, problems = plan['config'], []
    if c.get('stack_binary_sha256') != PROTOCOL_STACK_SHA256:
        problems.append(f'the Stack binary is not the protocol\'s ({PROTOCOL_STACK_SHA256[:12]}...)')
    if c.get('stack_rev') != PROTOCOL_STACK_REV:
        problems.append(f'--stack-rev must be {PROTOCOL_STACK_REV}')
    if not plan.get('pairs') or any(not p.get('scenario') or p.get('scenario') not in SCENARIOS or p.get('victim') not in c['tasks']
                                    for p in plan['pairs']):
        problems.append('every pair needs a fault scenario and a victim')
    cli = (c.get('agent_command') or [''])[0]
    if not cli.startswith('/') or not VERSIONED_CLI.search(cli) or not c.get('agent_cli_sha256'):
        problems.append('the agent CLI is not pinned to a versioned binary with a recorded hash')
    if c.get('warm_cache') != str(WARM):
        problems.append(f"the plan's warm cache {c.get('warm_cache')} is not the runner's {WARM}")
    return problems


def check_plan(plan):
    problems = []
    if not Path(plan['config']['stack_binary']).is_file() or sha256(plan['config']['stack_binary']) != plan['config']['stack_binary_sha256']:
        problems.append('the Stack binary changed since the plan')
    cli, digest = plan['config']['agent_command'][0], plan['config'].get('agent_cli_sha256')
    if digest and (not Path(cli).is_file() or sha256(cli) != digest):
        problems.append('the agent CLI binary changed since the plan')
    current = fingerprints()
    changed = sorted(k for k in set(current) | set(plan['fingerprints']) if current.get(k) != plan['fingerprints'].get(k))
    if changed:
        problems.append(f'benchmark files changed since the plan: {changed}')
    if plan.get('protocol_version') != PROTOCOL_VERSION:
        problems.append(f"the plan is protocol version {plan.get('protocol_version', 1)}; this harness runs version {PROTOCOL_VERSION}")
    return problems + protocol_problems(plan)


def run_dir_name(index, attempt, arm):
    return f'pair{index}-{arm}' if attempt == 1 else f'pair{index}.{attempt}-{arm}'


def attempt_reasons(results, index, plan_sha):
    """Why attempt 1 of a pair may be rerun: an invalid run or a missed fault (PROTOCOL.md, Version 2)."""
    reasons = []
    for arm in ('stack', 'baseline'):
        path = Path(results) / run_dir_name(index, 1, arm) / 'run.json'
        if not path.exists():
            reasons.append(f'{arm}: no record of attempt 1')
            continue
        run = json.loads(path.read_text())
        if plan_sha and run.get('plan_sha256') != plan_sha:
            continue
        if not run.get('valid'):
            reasons.append(f"{arm}: invalid ({'; '.join(run.get('invalid_reasons') or [])[:200]})")
        elif (run.get('fault') or {}).get('missed'):
            reasons.append(f"{arm}: fault missed ({run['fault']['missed']})")
    return reasons


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
        if a.get('killed_by_harness'):
            continue  # the harness ended it
        result = (a.get('trace_metrics') or {}).get('result') or {}
        budget_stop = 'budget' in str(result.get('subtype') or '') or 'max_budget' in str(result.get('subtype') or '')
        if not a.get('timed_out') and not budget_stop and (a.get('exit') not in (0, None) or result.get('is_error') or not result):
            bad.append(task)
    return bad


def cmd_pair(args):
    plan = load_plan(args.plan)
    problems = check_plan(plan)
    if problems:
        raise SystemExit('refusing to run: ' + '; '.join(problems))
    if args.attempt not in (1, 2):
        raise SystemExit('a pair has at most two attempts')
    lock = host_lock()
    pair = plan['pairs'][args.index]
    results = Path(args.results)
    if args.attempt == 2:
        reasons = attempt_reasons(results, args.index, plan['_sha256'])
        if not reasons:
            raise SystemExit(f'attempt 2 of pair {args.index} is allowed only after an invalid run or a missed fault in attempt 1')
        print(f'pair {args.index}: attempt 2, because {reasons}', flush=True)
    plan['_attempt'] = args.attempt
    for arm_name in pair['order']:
        out = results / run_dir_name(args.index, args.attempt, arm_name)
        if out.exists():
            raise SystemExit(f'{out} exists; runs are never overwritten (rerun into a new results directory)')
    for arm_name in pair['order']:
        problems = check_plan(plan)  # before each arm
        if problems:
            raise SystemExit('refusing to run: ' + '; '.join(problems))
        out = results / run_dir_name(args.index, args.attempt, arm_name)
        print(f'pair {args.index} attempt {args.attempt}: running {arm_name} -> {out}', flush=True)
        record = run_arm(plan, args.index, arm_name, out)
        summary = {t: {'passed': (a.get('grade') or {}).get('passed'), 'seconds': a.get('seconds'), 'killed': a.get('killed_by_harness', False),
                       'left': len(a.get('left_running') or []), 'cost': ((a.get('trace_metrics') or {}).get('result') or {}).get('total_cost_usd')}
                   for t, a in record['agents'].items()}
        failures = cli_failures(record)
        fault = record.get('fault') or {}
        print(json.dumps({'arm': arm_name, 'valid': record['valid'], 'invalid': record['invalid_reasons'], 'cli_failures': failures,
                          'scenario': record.get('scenario'), 'victim': record.get('victim'), 'injected': fault.get('injected'),
                          'missed': fault.get('missed'), 'bystander_changed': record.get('bystander', {}).get('changed'),
                          'residue_ok': {t: e.get('residue_ok') for t, e in (record.get('teardown') or {}).items()},
                          'agents': summary}, indent=2), flush=True)
        # Stop rules (PROTOCOL.md, Validity): an invalid run or two CLI failures stop the experiment.
        if not record['valid']:
            raise SystemExit(f'stopping: run {out} is invalid: {record["invalid_reasons"]}')
        if len(failures) >= 2:
            raise SystemExit(f'stopping: {len(failures)} agent CLI failures in {out}: {failures}')
    lock.close()


def scripted_override(spec):
    """task=mode[,task.recovery=mode,...] -> agent_override(task, role) running scripted_agent.py."""
    modes = dict(item.split('=', 1) for item in spec.split(',')) if spec else {}
    def override(task, role):
        key = task if role == 'agent' else f'{task}.recovery'
        return ['python3', str(HERE / 'scripted_agent.py'), modes.get(key, 'reference')]
    return override


def cmd_arm(args):
    plan = load_plan(args.plan)
    if plan['kind'] != 'harness_selftest':
        raise SystemExit('`arm` runs only self-test plans; experiment plans run through `pair`')
    lock = host_lock()
    override = scripted_override(args.scripted) if args.scripted is not None else None
    fault = {'scenario': args.scenario, 'victim': args.victim} if args.scenario else None
    record = run_arm(plan, args.index, args.arm, Path(args.out), agent_override=override, keep_root=args.keep_root,
                     tasks=args.tasks.split(',') if args.tasks else None, fault=fault)
    print(json.dumps({'valid': record['valid'], 'invalid': record['invalid_reasons'], 'fault': record.get('fault'), 'out': args.out},
                     indent=2, default=str))
    lock.close()


def interrupt(signum, frame):
    raise KeyboardInterrupt(f'signal {signum}')


def main():
    # Both, explicitly: a runner started in the background inherits SIGINT ignored, and Python keeps that.
    signal.signal(signal.SIGTERM, interrupt)
    signal.signal(signal.SIGINT, interrupt)
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = parser.add_subparsers(dest='command', required=True)
    p = sub.add_parser('plan', help='fix a version 2 plan')
    p.add_argument('--name', required=True)
    p.add_argument('--kind', choices=['agent_experiment', 'harness_selftest'], default='agent_experiment')
    group = p.add_mutually_exclusive_group(required=True)
    group.add_argument('--pairs-per-scenario', type=int, help='pairs per fault scenario (pairs alternate scenarios)')
    group.add_argument('--pairs', type=int, help='a plan without faults (self-tests and validation)')
    p.add_argument('--seed', type=int, required=True)
    p.add_argument('--stack', required=True, help='the Stack release binary for the Stack arm')
    p.add_argument('--stack-rev', required=True, help='the revision that binary was built from (recorded with its sha256)')
    p.add_argument('--model', default='claude-sonnet-5-5')
    p.add_argument('--effort', default='high')
    p.add_argument('--agent-seconds', type=int, default=1800)
    p.add_argument('--max-usd', type=float, default=8.0)
    p.add_argument('--agent-command')
    p.add_argument('--out', required=True)
    p = sub.add_parser('pair', help='run one planned pair end to end')
    p.add_argument('--plan', required=True)
    p.add_argument('--index', type=int, required=True)
    p.add_argument('--attempt', type=int, default=1, help='2 reruns a pair whose first attempt had an invalid run or a missed fault')
    p.add_argument('--results', required=True)
    p = sub.add_parser('arm', help='run one arm of a self-test plan')
    p.add_argument('--plan', required=True)
    p.add_argument('--index', type=int, default=0)
    p.add_argument('--arm', choices=['stack', 'baseline'], required=True)
    p.add_argument('--out', required=True)
    p.add_argument('--scripted', help='task=mode,task.recovery=mode,... for the scripted agent (harness self-test)')
    p.add_argument('--scenario', choices=SCENARIOS, help='inject this fault (self-test)')
    p.add_argument('--victim', help='the victim task for --scenario')
    p.add_argument('--keep-root', action='store_true')
    p.add_argument('--tasks', help='comma-separated subset of tasks (smoke tests only; never for planned pairs)')
    args = parser.parse_args()
    {'plan': cmd_plan, 'pair': cmd_pair, 'arm': cmd_arm}[args.command](args)


if __name__ == '__main__':
    main()
