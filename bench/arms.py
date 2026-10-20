"""The two arms: build a trial workspace, drive it from the harness, and tear it down.

Nothing in this module is copied into an agent's workspace except the existing arm's
`existing/dev.py` and the fixture files.
"""
import hashlib
import json
import os
from pathlib import Path
import platform
import shutil
import signal
import socket
import subprocess
import sys
import time
import uuid

HERE = Path(__file__).resolve().parent
TASKS = {task['id']: task for task in json.loads((HERE / 'tasks.json').read_text())['tasks']}
ARMS = ('stack', 'existing')

PINS = {'python': '3.13.16', 'uv': '0.12.23', 'node': '22.23.3', 'postgres': '17.11', 'redis': '7.4.7'}
# The mise backends Stack 0.1.22's postgres and redis presets lock these versions to
# (provider_lock in stack.lock). The existing arm installs the same artifacts directly.
NATIVE_BACKENDS = {'postgres': 'conda:postgresql', 'redis': 'conda:redis-server'}

PROJECTS = {
    'ledger': {'tools': ['python', 'uv'], 'services': ['postgres', 'redis'], 'app': 'api',
               'start': 'uv run --frozen python seed.py && uv run --frozen python migrate.py && exec uv run --frozen python app.py',
               'watch': ['app.py', 'migrate.py'], 'smoke': ['uv', 'run', '--frozen', 'python', 'smoke.py']},
    'catalog': {'tools': ['node'], 'services': [], 'app': 'web', 'start': 'exec node server.mjs',
                'watch': ['server.mjs'], 'smoke': ['node', 'smoke.mjs']},
    'slug': {'tools': ['node'], 'services': [], 'app': None, 'smoke': ['node', 'smoke.mjs']},
}

# Files the grader always takes from the benchmark, never from a submission.
PROTECTED = {'stack.toml', 'stack.lock', 'mise.toml', 'mise.lock', 'dev.py', '.dev.json', 'seed.py', 'smoke.py',
             'smoke.mjs', 'products.json', 'README.md', '.gitignore'}
RUNTIME_DIRS = {'.git', '.stack', '.dev', '.venv', '.config', 'node_modules', '__pycache__'}
GITIGNORE = '.stack/\n.dev/\n.venv/\n.config/mise/\nnode_modules/\n__pycache__/\n'


def digest(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def isolated_env(root, cache=None, locale=None, bin_dir=None):
    """Every Stack, mise, Pitchfork, XDG, uv and npm store under `root`; HOME is kept.

    `cache` shares only download and install stores between trials (a warm cache).
    Inherited locale, PostgreSQL client settings and service URLs are dropped so both
    arms see exactly the locale the plan names.
    """
    dropped = ('STACK_', 'MISE_', '__MISE', 'PITCHFORK_', 'XDG_', 'UV_', 'FNOX_', 'npm_config_', 'NPM_CONFIG_',
               'PG', 'LC_', 'VIRTUAL_ENV', 'CONDA_', 'PYTHON')
    env = {key: value for key, value in os.environ.items()
           if not key.startswith(dropped) and key not in {'LANG', 'LANGUAGE', 'DATABASE_URL', 'REDIS_URL', 'API_URL', 'WEB_URL'}}
    paths = {'STACK_STATE_DIR': 'stack/state', 'STACK_DATA_DIR': 'stack/data', 'STACK_CACHE_DIR': 'stack/cache',
             'MISE_DATA_DIR': 'mise/data', 'MISE_CACHE_DIR': 'mise/cache', 'MISE_STATE_DIR': 'mise/state',
             'MISE_CONFIG_DIR': 'mise/config', 'MISE_GLOBAL_CONFIG_FILE': 'mise/config/config.toml',
             'MISE_SYSTEM_CONFIG_FILE': 'mise/config/system.toml', 'PITCHFORK_CONFIG_DIR': 'pf/config',
             'PITCHFORK_STATE_DIR': 'pf/state', 'PITCHFORK_LOGS_DIR': 'pf/logs', 'XDG_CONFIG_HOME': 'xdg/config',
             'XDG_DATA_HOME': 'xdg/data', 'XDG_CACHE_HOME': 'xdg/cache', 'XDG_STATE_HOME': 'xdg/state',
             'UV_CACHE_DIR': 'uv/cache', 'npm_config_cache': 'npm/cache', 'npm_config_userconfig': 'npm/npmrc',
             'npm_config_globalconfig': 'npm/global-npmrc', 'npm_config_prefix': 'npm/prefix'}
    env.update({key: str(root / value) for key, value in paths.items()})
    if cache:
        env.update(MISE_DATA_DIR=str(cache / 'mise-data'), MISE_CACHE_DIR=str(cache / 'mise-cache'), UV_CACHE_DIR=str(cache / 'uv'))
    env.update(locale or {})
    env.update(MISE_YES='1', CI='1', NO_COLOR='1', GIT_CONFIG_NOSYSTEM='1')
    if bin_dir:
        env['PATH'] = f'{bin_dir}{os.pathsep}{env.get("PATH", "")}'
    return env


def stack_toml(project):
    spec = PROJECTS[project]
    lines = ['[tools]'] + [f'{tool} = "{PINS[tool]}"' for tool in spec['tools']]
    for service in spec['services']:
        lines += ['', f'[services.{service}]', f'preset = "{service}"', f'version = "{PINS[service]}"']
    name = spec['app']
    if name:
        variable = f'{name.upper()}_PORT'
        probe = (f"python3 -c \"import os,urllib.request;print(urllib.request.urlopen('http://127.0.0.1:'+os.environ['{variable}']+'/identity',timeout=5).read().decode())\""
                 if 'python' in spec['tools'] else
                 f"node -e \"fetch('http://127.0.0.1:'+process.env.{variable}+'/identity').then(r=>r.text()).then(t=>process.stdout.write(t))\"")
        lines += ['', f'[services.{name}]', f"run = '''export IDENTITY_TOKEN=\"$STACK_IDENTITY_{name.upper()}\"; {spec['start']}'''",
                  'watch = [' + ', '.join(f'"{path}"' for path in spec['watch']) + ']',
                  '', f'[services.{name}.identity]', f"command = '''{probe}'''"]
    lines += ['', '[tasks.test]', f'run = "{" ".join(spec["smoke"])}"']
    return '\n'.join(lines) + '\n'


def mise_toml(project):
    spec = PROJECTS[project]
    lines = ['[tools]'] + [f'{tool} = "{PINS[tool]}"' for tool in spec['tools']]
    lines += [f'"{NATIVE_BACKENDS[service]}" = "{PINS[service]}"' for service in spec['services']]
    return '\n'.join(lines) + '\n'


def readme(project, arm):
    spec = PROJECTS[project]
    title = {'ledger': 'Invoice ledger API (Python, PostgreSQL, Redis)', 'catalog': 'Product catalog API (Node)',
             'slug': 'Slug library (Node)'}[project]
    smoke = ' '.join(spec['smoke'])
    if arm == 'stack':
        body = ['This project uses Stack (`stack.toml`, locked in `stack.lock`). `stack` is on PATH; `stack --help` lists',
                'its commands and `stack --json <command>` gives machine-readable results. `stack mcp` serves the same',
                'commands over MCP.', '']
        if spec['app']:
            body += [f'- `stack up` installs the pinned tools and starts {", ".join(spec["services"] + [spec["app"]])}.']
            if project == 'ledger':
                body += ['  Before serving, the api service loads the existing data (`seed.py`) and runs `migrate.py`.']
            body += [f'- `stack restart {spec["app"]}` reloads the app after you change its code.',
                     '- `stack status` and `stack logs <service>` show what is running.']
        body += ['- `stack run test` runs the existing smoke test.',
                 '- `stack exec -- <command>` runs any command with the pinned tools' + (' and the service URLs.' if spec['app'] else '.')]
        if spec['app']:
            body += ['- `stack down` stops everything. The data is kept.']
    else:
        body = ['Tools are pinned in `mise.toml`. `dev.py` runs the services natively; `python3 dev.py` lists its commands.', '',
                '- `python3 dev.py setup` installs the pinned tools and dependencies.']
        if spec['app']:
            body += ['- `python3 dev.py up` starts ' + ', '.join(spec['services'] + [spec['app']]) + '.']
            if project == 'ledger':
                body += ['  Before serving, it loads the existing data (`seed.py`) and runs `migrate.py`.']
            body += ['- After you change the app, run `python3 dev.py down` and `python3 dev.py up` again.',
                     '- `python3 dev.py status` shows endpoints and process IDs; logs are in `.dev/`.']
        body += ['- `python3 dev.py test` runs the existing smoke test.',
                 '- `python3 dev.py exec <command>` runs any command with the pinned tools' + (' and the service URLs.' if spec['app'] else '.')]
        if spec['app']:
            body += ['- `python3 dev.py down` stops everything. The data is kept.']
    variables = {'ledger': '`DATABASE_URL`, `REDIS_URL`, `API_URL`', 'catalog': '`WEB_URL`'}.get(project)
    if variables:
        body += ['', f'The app and tests read {variables} from the environment. The smoke test is `{smoke}`.']
    if project == 'ledger':
        body += ['PostgreSQL refuses to run as root, so use a regular user.']
    return f'# {title}\n\n' + '\n'.join(body) + '\n'


def run_quiet(argv, cwd, env, timeout=600):
    return subprocess.run(argv, cwd=cwd, env=env, capture_output=True, text=True, timeout=timeout)


def install_bin(root, arm, binary):
    """Put the pinned Stack on PATH for the Stack arm; shadow any host `stack` otherwise."""
    bin_dir = root / 'bin'
    bin_dir.mkdir()
    target = bin_dir / 'stack'
    if arm == 'stack':
        shutil.copy2(binary, target)
        if digest(target) != digest(binary):
            raise RuntimeError('copied Stack binary does not match')
    else:
        target.write_text('#!/bin/sh\necho "stack: command not found" >&2\nexit 127\n')
        target.chmod(0o755)
    return bin_dir


def prepare(task_id, arm, root, binary, env):
    """Create the agent-facing project: fixture, arm config, README, git history."""
    project = TASKS[task_id]['project']
    workspace = root / 'app'
    shutil.copytree(HERE / 'fixtures' / project, workspace)
    (workspace / 'README.md').write_text(readme(project, arm))
    (workspace / '.gitignore').write_text(GITIGNORE)
    if arm == 'stack':
        (workspace / 'stack.toml').write_text(stack_toml(project))
        attempts = []
        for attempt in range(3):
            compiled = run_quiet([str(binary), '--json', '-C', str(workspace), 'compile'], workspace, env)
            attempts.append({'exit_code': compiled.returncode, 'stdout': compiled.stdout[-2000:], 'stderr': compiled.stderr[-2000:]})
            # Preparation is harness work: network blips and a cold mise missing Stack's 10 s
            # `mise version` deadline are retried, and every attempt is kept as evidence.
            if compiled.returncode == 0:
                break
            time.sleep(5 * (attempt + 1))
        (root / 'prepare-compile.json').write_text(json.dumps(attempts, indent=2))
        if compiled.returncode:
            raise RuntimeError(f'stack compile failed while preparing: {compiled.stdout[-2000:]} {compiled.stderr[-2000:]}')
        # A fresh clone has stack.toml and stack.lock but not the generated provider config.
        shutil.rmtree(workspace / '.config')
    else:
        (workspace / 'mise.toml').write_text(mise_toml(project))
        shutil.copy2(HERE / 'existing' / 'dev.py', workspace / 'dev.py')
        (workspace / '.dev.json').write_text(json.dumps({'project': project, 'identity': uuid.uuid4().hex}))
    git = ['git', '-c', 'user.name=bench', '-c', 'user.email=bench@example.invalid', '-c', 'core.hooksPath=/dev/null',
           '-c', 'init.defaultBranch=main', '-c', 'commit.gpgsign=false']
    for argv in [[*git, 'init', '-q'], [*git, 'add', '-A'], [*git, 'commit', '-q', '-m', 'Starting point']]:
        result = run_quiet(argv, workspace, env, 60)
        if result.returncode:
            raise RuntimeError(f'{argv} failed: {result.stderr}')
    return workspace


def lock_backends(workspace):
    """Backends Stack actually locked for its services, to keep the native arm matched."""
    import tomllib
    path = workspace / 'stack.lock'
    if not path.exists():
        return {}
    tools = tomllib.loads(path.read_text()).get('provider_lock', {}).get('tools', {})
    return {name: [entry.get('backend') for entry in entries] for name, entries in tools.items()
            if name in NATIVE_BACKENDS}


def overlay(source, workspace):
    """Copy submitted files into a fresh grading workspace, except protected and runtime files."""
    copied = {}
    for path in sorted(source.rglob('*')):
        relative = path.relative_to(source)
        if any(part in RUNTIME_DIRS for part in relative.parts) or relative.as_posix() in PROTECTED:
            continue
        if path.is_symlink():
            copied[relative.as_posix()] = 'symlink-skipped'
            continue
        if path.is_file():
            target = workspace / relative
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(path, target)
            copied[relative.as_posix()] = digest(target)
    return copied


# ---- harness-driven steps (validation and grading) ----

def commands(arm, project, binary, verifier_argv=None):
    spec = PROJECTS[project]
    if arm == 'stack':
        stack = [str(binary), '--json']
        require = ['--require-all'] if spec['app'] else []
        steps = {'setup': [stack + ['install']], 'up': [stack + ['up', '--ttl', '30m', '--timeout', '5m']] if spec['app'] else [],
                 'smoke_exec': stack + ['exec', *require, '--', *spec['smoke']],
                 'smoke_task_loop': [str(binary), 'run', 'test'],
                 'down': stack + ['down']}
        if verifier_argv:
            steps['verify'] = [str(binary), 'exec', *require, '--', *verifier_argv]
    else:
        dev = [sys.executable, 'dev.py']
        steps = {'setup': [dev + ['setup']], 'up': [dev + ['up']] if spec['app'] else [],
                 'smoke_exec': dev + ['exec', *spec['smoke']], 'smoke_task_loop': dev + ['test'],
                 'down': dev + ['down']}
        if verifier_argv:
            steps['verify'] = dev + ['exec', *verifier_argv]
    return steps


def verifier_argv(task_id, workspace):
    if TASKS[task_id]['project'] == 'ledger':
        return ['uv', 'run', '--frozen', 'python', str(HERE / 'verify.py'), task_id, str(workspace)]
    return [sys.executable, str(HERE / 'verify.py'), task_id, str(workspace)]


# ---- ownership and cleanup ----

def alive(pid):
    out = subprocess.run(['ps', '-p', str(pid), '-o', 'stat='], capture_output=True, text=True).stdout.strip()
    return bool(out) and not out.startswith('Z')


def listening(port):
    with socket.socket() as connection:
        connection.settimeout(0.3)
        return connection.connect_ex(('127.0.0.1', int(port))) == 0


def owned_processes(root):
    """Live processes whose command line, working directory or an open file is under `root`.

    The runtime root is unique per trial, so this finds owned processes even when a start
    failed before anything was recorded. A process that detached with none of these under
    the root would be missed; the result is therefore 'none observed', not proof of none.
    """
    roots = {str(root)}
    if str(root).startswith('/private/'):
        roots.add(str(root)[len('/private'):])
    under = lambda text: any(r + '/' in text or text == r or text.endswith(r) for r in roots)
    me = os.getpid()
    rows = {}
    listing = subprocess.run(['ps', '-axww', '-o', 'pid=,ppid=,pgid=,stat=,lstart=,command='], capture_output=True, text=True).stdout
    for line in listing.splitlines():
        parts = line.split(None, 9)
        if len(parts) < 10 or parts[3].startswith('Z'):
            continue
        pid = int(parts[0])
        rows[pid] = {'pid': pid, 'ppid': int(parts[1]), 'pgid': int(parts[2]), 'started': ' '.join(parts[4:9]), 'command': parts[9]}
    owned = {pid: 'command' for pid, row in rows.items() if under(row['command'])}
    if platform.system() == 'Linux':
        for pid in rows:
            try:
                links = [os.readlink(f'/proc/{pid}/cwd')] + [os.readlink(f'/proc/{pid}/fd/{fd}') for fd in os.listdir(f'/proc/{pid}/fd')]
            except OSError:
                continue
            if any(under(link) for link in links):
                owned.setdefault(pid, 'open_file')
    elif shutil.which('lsof'):
        current = None
        for line in subprocess.run(['lsof', '-nP', '-w', '-u', str(os.getuid()), '-Fpn'], capture_output=True, text=True).stdout.splitlines():
            if line.startswith('p'):
                current = int(line[1:])
            elif line.startswith('n') and current is not None and under(line[1:]):
                owned.setdefault(current, 'open_file')
    ancestors, pid = set(), me
    while pid in rows and pid not in ancestors:
        ancestors.add(pid)
        pid = rows[pid]['ppid']
    return [dict(rows[pid], reason=reason) for pid, reason in sorted(owned.items()) if pid in rows and pid not in ancestors]


def recorded(arm, workspace):
    """PIDs and ports the arm itself recorded for this workspace."""
    pids, ports = set(), set()
    path = workspace / ('.stack/session.json' if arm == 'stack' else '.dev/state.json')
    if path.exists():
        try:
            data = json.loads(path.read_text())
        except ValueError:
            data = {}
        if arm == 'stack':
            for service in (data.get('services') or {}).values():
                if isinstance(service, dict):
                    pids.update(p for p in [service.get('pid')] if isinstance(p, int))
                    ports.update(p for p in [service.get('port')] if isinstance(p, int))
        else:
            pids.update(p for p in (data.get('pids') or {}).values() if isinstance(p, int))
            ports.update(p for p in (data.get('ports') or {}).values() if isinstance(p, int))
    return {'pids': sorted(pids), 'ports': sorted(ports)}


def live(observed):
    return {'pids': [p for p in observed['pids'] if alive(p)], 'ports': [p for p in observed['ports'] if listening(p)]}


def residue(arm, workspace, root, seen):
    """Everything still running that this trial owns: recorded, previously seen, or under root."""
    current = recorded(arm, workspace)
    observed = {key: sorted(set(current[key]) | set(seen.get(key, []))) for key in ['pids', 'ports']}
    return {**live(observed), 'owned_processes': owned_processes(root)}


SUPERVISOR = ('pitchfork supervisor run', 'pitchfork log-sink')


def left_services(residue):
    """Split what is still running into project services and Pitchfork's own processes.

    Stack keeps the supervisor (and, for a while, log sinks) after `stack down` by design;
    it is shared infrastructure, so an agent that ran `down` has not left services running.
    """
    infrastructure = [p for p in residue.get('owned_processes', []) if any(marker in p['command'] for marker in SUPERVISOR)]
    services = [p for p in residue.get('owned_processes', []) if p not in infrastructure]
    return {'services': bool(residue.get('pids') or residue.get('ports') or services),
            'service_processes': services, 'supervisor_processes': infrastructure}


def disk_usage(root, arm, workspace):
    def kilobytes(path):
        if not path.exists():
            return 0
        out = subprocess.run(['du', '-sk', str(path)], capture_output=True, text=True).stdout.split()
        return int(out[0]) if out else None
    data = root / 'mise/state/daemons' if arm == 'stack' else workspace / '.dev'
    usage = {'runtime_root_kb': kilobytes(root), 'service_data_kb': kilobytes(data), 'tool_installs_kb': kilobytes(root / 'mise/data')}
    ports = root / 'stack/state/ports.json'
    if ports.exists():
        try:
            usage['stack_port_reservations'] = json.loads(ports.read_text())
        except ValueError:
            usage['stack_port_reservations'] = 'unparseable'
    return usage


def reclaim(processes):
    """Stop owned leftovers, re-checking each one's start time and command right before signalling."""
    reclaimed = []
    for process in processes:
        now = subprocess.run(['ps', '-p', str(process['pid']), '-o', 'lstart=,command='], capture_output=True, text=True).stdout.strip()
        if not now or not now.startswith(process['started']) or process['command'] not in now:
            continue
        try:
            os.kill(process['pid'], signal.SIGTERM)
            reclaimed.append(process['pid'])
        except ProcessLookupError:
            pass
    deadline = time.monotonic() + 5
    while time.monotonic() < deadline and any(alive(pid) for pid in reclaimed):
        time.sleep(0.1)
    for pid in reclaimed:
        if alive(pid):
            os.kill(pid, signal.SIGKILL)
    return reclaimed


def globals_snapshot():
    """Hashes of the user's own Stack, mise and Pitchfork config/state, and install listings.

    Other programs on the host may legitimately change these; a difference is reported
    with its paths and must be inspected, not assumed to be the benchmark's.
    """
    home = Path.home()
    rows = {}
    for suffix in ['.config/mise', '.config/pitchfork', '.local/state/pitchfork', '.local/state/stack',
                   '.local/share/stack', '.cache/stack', '.config/stack']:
        base = home / suffix
        rows[suffix] = 'present' if base.exists() else 'absent'
        if base.is_dir():
            for path in sorted(base.rglob('*')):
                if path.is_file() and path.stat().st_size < 50_000_000:
                    rows[str(path.relative_to(home))] = digest(path)
    for suffix in ['.local/share/mise/installs', '.local/state/mise']:
        base = home / suffix
        if base.is_dir():
            rows[suffix + '/*'] = sorted(path.name for path in base.iterdir())
    return rows


def globals_diff(before, after):
    return sorted(key for key in set(before) | set(after) if before.get(key) != after.get(key))
