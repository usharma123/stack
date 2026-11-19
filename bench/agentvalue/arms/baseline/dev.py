#!/usr/bin/env python3
"""Local development for this repository. Every git worktree gets its own services.

Tools are pinned in mise.toml. Each worktree runs its own PostgreSQL, Redis, API and worker,
on ports reserved for that worktree in a machine-wide registry, with data under .dev/ in the
worktree. Nothing is shared between worktrees, so several people or agents can work in parallel.

  python3 dev.py up               install pinned tools, start everything, migrate, seed a new database
  python3 dev.py status           show this worktree's services, ports and process IDs (JSON)
  python3 dev.py env              print the environment as `export` lines: eval "$(python3 dev.py env)"
  python3 dev.py exec CMD...      run a command with the pinned tools and this worktree's service URLs
  python3 dev.py test [ARGS...]   run the test suite (pytest) against this worktree's services
  python3 dev.py smoke            run the smoke test against this worktree's services
  python3 dev.py restart [api|worker]   apply migrations and restart the app processes after a code change
  python3 dev.py migrate          apply pending migrations now
  python3 dev.py seed             load the demo data again (safe to repeat)
  python3 dev.py logs [NAME]      show the last lines of a service's log (api, worker, postgres, redis)
  python3 dev.py down [--purge]   stop everything this worktree started and confirm it is gone;
                                  --purge also deletes this worktree's data and releases its ports
"""
import contextlib
import fcntl
import hashlib
import json
import os
from pathlib import Path
import secrets
import shutil
import signal
import socket
import subprocess
import sys
import time
import urllib.request

ROOT = Path(__file__).resolve().parent
DEV = ROOT / '.dev'
STATE = DEV / 'state.json'
PGDATA = DEV / 'postgres'
SEEDED = DEV / 'seeded'
APPS = {'api': ['uv', 'run', '--frozen', 'python', 'app.py'], 'worker': ['uv', 'run', '--frozen', 'python', 'worker.py']}
SERVICES = ['postgres', 'redis', 'api', 'worker']
REGISTRY_DIR = Path(os.environ.get('XDG_STATE_HOME') or Path.home() / '.local/state') / 'ledger-dev'


def fail(message):
    print(f'dev.py: {message}', file=sys.stderr)
    raise SystemExit(1)


def mise(*argv, **kwargs):
    return subprocess.run(['mise', 'x', '--', *argv], cwd=ROOT, **kwargs)


def load(path, default=None):
    try:
        return json.loads(path.read_text())
    except (OSError, ValueError):
        return default


def save(path, data):
    path.parent.mkdir(parents=True, exist_ok=True)
    tmp = path.with_suffix('.tmp')
    tmp.write_text(json.dumps(data, indent=2))
    tmp.replace(path)


@contextlib.contextmanager
def locked(path):
    """An exclusive lock; waits for whoever holds it (another command in this worktree, or a
    port allocation in another worktree)."""
    path.parent.mkdir(parents=True, exist_ok=True)
    with open(path, 'w') as handle:
        fcntl.flock(handle, fcntl.LOCK_EX)
        yield


def port_free(port):
    with socket.socket() as probe:
        try:
            probe.bind(('127.0.0.1', port))
            return True
        except OSError:
            return False


def listening(port):
    with socket.socket() as connection:
        connection.settimeout(0.3)
        return connection.connect_ex(('127.0.0.1', port)) == 0


def reserve_ports(running):
    """This worktree's ports from the machine-wide registry, under its lock. A worktree keeps its
    ports; a new one gets the slot its path hashes to, or the next slot no other worktree holds
    and nothing else listens on. Entries of deleted worktrees are released."""
    with locked(REGISTRY_DIR / 'ports.lock'):
        registry = {path: ports for path, ports in (load(REGISTRY_DIR / 'ports.json') or {}).items() if Path(path).exists()}
        mine = registry.get(str(ROOT))
        if mine and set(mine) == set(SERVICES) and all(port_free(mine[n]) or n in running for n in SERVICES):
            save(REGISTRY_DIR / 'ports.json', registry)
            return mine
        taken = {port for path, ports in registry.items() if path != str(ROOT) for port in ports.values()}
        slot = int(hashlib.sha256(str(ROOT).encode()).hexdigest(), 16) % 900
        for attempt in range(900):
            base = 30000 + ((slot + attempt) % 900) * 10
            ports = {name: base + offset for offset, name in enumerate(SERVICES)}
            if not set(ports.values()) & taken and all(port_free(p) for p in ports.values()):
                registry[str(ROOT)] = ports
                save(REGISTRY_DIR / 'ports.json', registry)
                return ports
    fail('no free port block found between 30000 and 38999')


def release_ports():
    with locked(REGISTRY_DIR / 'ports.lock'):
        registry = load(REGISTRY_DIR / 'ports.json') or {}
        registry.pop(str(ROOT), None)
        save(REGISTRY_DIR / 'ports.json', registry)


def environment(ports, services=SERVICES):
    env = {}
    if 'postgres' in services:
        env['DATABASE_URL'] = f'postgresql://postgres@127.0.0.1:{ports["postgres"]}/postgres'
    if 'redis' in services:
        env['REDIS_URL'] = f'redis://127.0.0.1:{ports["redis"]}/0'
    if 'api' in services:
        env.update(API_URL=f'http://127.0.0.1:{ports["api"]}', API_PORT=str(ports['api']))
    return env


def process_start(pid):
    out = subprocess.run(['ps', '-p', str(pid), '-o', 'lstart='], capture_output=True, text=True).stdout.strip()
    return out or None


def alive(record):
    """True while the recorded process is still the same process (pid and start time)."""
    return bool(record) and process_start(record['pid']) == record['started']


def state():
    return load(STATE, {'services': {}})


def setup():
    subprocess.run(['mise', 'trust', '--quiet', str(ROOT / 'mise.toml')], cwd=ROOT, check=True)
    subprocess.run(['mise', 'install', '--quiet'], cwd=ROOT, check=True)
    mise('uv', 'sync', '--frozen', '--quiet', check=True)


def start_postgres(ports, record):
    if PGDATA.exists() and not (PGDATA / 'PG_VERSION').exists():
        shutil.rmtree(PGDATA)  # an interrupted initdb: start again
    new_database = not PGDATA.exists()
    if new_database:
        mise('initdb', '-D', str(PGDATA), '-U', 'postgres', '-A', 'trust', '-E', 'UTF8', '--locale=en_US.UTF-8',
             stdout=subprocess.DEVNULL, check=True)
    mise('pg_ctl', '-D', str(PGDATA), '-l', str(DEV / 'postgres.log'), '-w', '-t', '60',
         '-o', f"-h 127.0.0.1 -p {ports['postgres']} -k ''", 'start', stdout=subprocess.DEVNULL, check=True)
    pid = int((PGDATA / 'postmaster.pid').read_text().splitlines()[0])
    record['services']['postgres'] = {'pid': pid, 'started': process_start(pid), 'port': ports['postgres']}
    save(STATE, record)
    return new_database


def start_redis(ports, record):
    (DEV / 'redis').mkdir(parents=True, exist_ok=True)
    pidfile = DEV / 'redis.pid'
    pidfile.unlink(missing_ok=True)
    mise('redis-server', '--bind', '127.0.0.1', '--port', str(ports['redis']), '--dir', str(DEV / 'redis'),
         '--daemonize', 'yes', '--pidfile', str(pidfile), '--logfile', str(DEV / 'redis.log'), check=True)
    deadline = time.monotonic() + 10
    while not (pidfile.exists() and listening(ports['redis'])):
        if time.monotonic() > deadline:
            fail(f'redis did not start; see {DEV / "redis.log"}')
        time.sleep(0.05)
    pid = int(pidfile.read_text().strip())
    record['services']['redis'] = {'pid': pid, 'started': process_start(pid), 'port': ports['redis']}
    save(STATE, record)


def start_app(name, ports, record):
    env = dict(os.environ, **environment(ports), PORT=str(ports['api']), IDENTITY_TOKEN=record['identity'],
               WORKER_HEALTH_PORT=str(ports['worker']))
    with (DEV / f'{name}.log').open('ab') as log:
        process = subprocess.Popen(['mise', 'x', '--', *APPS[name]], cwd=ROOT, env=env, stdout=log, stderr=log,
                                   stdin=subprocess.DEVNULL, start_new_session=True)
    record['services'][name] = {'pid': process.pid, 'started': process_start(process.pid), 'pgid': process.pid, 'port': ports[name]}
    save(STATE, record)


def wait_ready(ports, record, names):
    """The api must answer with this worktree's identity; the worker must answer on its health port."""
    deadline = time.monotonic() + 60
    pending = set(names)
    while pending and time.monotonic() < deadline:
        if 'api' in pending:
            try:
                with urllib.request.urlopen(f'http://127.0.0.1:{ports["api"]}/identity', timeout=1) as response:
                    if response.read().decode() == record['identity']:
                        pending.discard('api')
            except (OSError, ValueError):
                pass
        if 'worker' in pending and listening(ports['worker']):
            pending.discard('worker')
        for name in list(pending):
            if not alive(record['services'][name]):
                fail(f'the {name} exited while starting; see `python3 dev.py logs {name}`')
        time.sleep(0.2)
    if pending:
        fail(f'{sorted(pending)} did not become ready; see `python3 dev.py logs`')


def migrate_and_seed(ports, new_database):
    env = dict(os.environ, **environment(ports, ['postgres', 'redis']))
    if mise('uv', 'run', '--frozen', 'python', 'migrate.py', env=env).returncode != 0:
        fail('migrate.py failed. PostgreSQL and Redis keep running: fix the migration and run `python3 dev.py up` again')
    if new_database or not SEEDED.exists():
        if mise('uv', 'run', '--frozen', 'python', 'seed.py', env=env).returncode != 0:
            fail('seed.py failed. PostgreSQL and Redis keep running: fix it and run `python3 dev.py up` again')
        SEEDED.write_text('seeded\n')


def up():
    with locked(DEV / 'lock'):
        record = state()
        running = {name for name, entry in record['services'].items() if alive(entry)}
        if running == set(SERVICES):
            return print(json.dumps(summary(record), indent=2))
        # Anything recorded but dead is forgotten; whatever still runs is kept (a partial start resumes).
        record['services'] = {name: entry for name, entry in record['services'].items() if name in running}
        setup()
        ports = reserve_ports(running)
        if record.get('ports') and record['ports'] != ports and running:
            fail('this worktree\'s running services use other ports than its reservation; run `python3 dev.py down` first')
        record.update(ports=ports, identity=record.get('identity') or secrets.token_hex(16), worktree=str(ROOT))
        save(STATE, record)  # saved early so `down` can stop a partial start
        new_database = False
        if 'postgres' not in running:
            new_database = start_postgres(ports, record)
        if 'redis' not in running:
            start_redis(ports, record)
        migrate_and_seed(ports, new_database)
        missing = [name for name in APPS if name not in running]
        for name in missing:
            start_app(name, ports, record)
        wait_ready(ports, record, missing)
        print(json.dumps(summary(record), indent=2))


def summary(record):
    ports = record.get('ports')
    return {'worktree': str(ROOT), 'services': {name: {**entry, 'running': alive(entry)} for name, entry in record['services'].items()},
            'env': environment(ports) if ports else {}}


def status():
    record = state()
    if not record['services']:
        return print(json.dumps({'worktree': str(ROOT), 'services': {}, 'note': 'nothing started; run `python3 dev.py up`'}, indent=2))
    print(json.dumps(summary(record), indent=2))


def current_env(need=('postgres', 'redis')):
    """Addresses of this worktree's running services. Fails if one the command needs is not running."""
    record = state()
    running = [name for name, entry in record['services'].items() if alive(entry)]
    missing = [name for name in need if name not in running]
    if missing:
        fail(f'{missing} not running in this worktree; run `python3 dev.py up` (see `python3 dev.py status`)')
    absent = [name for name in SERVICES if name not in running]
    if absent:
        print(f'dev.py: note: {absent} not running; their addresses are left out', file=sys.stderr)
    return environment(record['ports'], running)


def stop_process(name, entry, timeout=10):
    if name == 'postgres' and alive(entry):
        mise('pg_ctl', '-D', str(PGDATA), '-m', 'fast', '-w', 'stop', stdout=subprocess.DEVNULL)
    elif alive(entry):
        try:
            os.kill(entry['pid'], signal.SIGTERM)
        except ProcessLookupError:
            pass
    if 'pgid' in entry:  # the app's whole process group: `mise x` -> `uv run` -> python
        try:
            os.killpg(entry['pgid'], signal.SIGTERM)
        except (ProcessLookupError, PermissionError):
            pass
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline and (alive(entry) or ('port' in entry and listening(entry['port']))):
        time.sleep(0.05)
    if alive(entry) or ('port' in entry and listening(entry['port'])):
        for kill in ([lambda: os.killpg(entry['pgid'], signal.SIGKILL)] if 'pgid' in entry else []) + [lambda: os.kill(entry['pid'], signal.SIGKILL)]:
            try:
                kill()
            except (ProcessLookupError, PermissionError):
                pass
        time.sleep(0.5)
    return not alive(entry) and not ('port' in entry and listening(entry['port']))


def restart(names):
    with locked(DEV / 'lock'):
        record = state()
        if not all(alive(record['services'].get(n)) for n in ('postgres', 'redis')):
            fail('postgres and redis are not running; run `python3 dev.py up`')
        names = names or list(APPS)
        for name in names:
            if name not in APPS:
                fail(f'can only restart api or worker, not {name}')
        for name in names:
            stop_process(name, record['services'].get(name) or {})
        migrate_and_seed(record['ports'], False)
        for name in names:
            start_app(name, record['ports'], record)
        wait_ready(record['ports'], record, names)
        print(json.dumps(summary(record), indent=2))


def down(purge):
    with locked(DEV / 'lock'):
        record = state()
        stopped, left = [], []
        for name in ['worker', 'api', 'redis', 'postgres']:
            entry = record['services'].get(name)
            if entry:
                (stopped if stop_process(name, entry) else left).append(name)
        if left:
            fail(f'could not stop {left}; check `python3 dev.py status`')
        save(STATE, {'services': {}, 'ports': record.get('ports'), 'identity': record.get('identity')})
    if purge:
        shutil.rmtree(DEV, ignore_errors=True)
        release_ports()
    print(json.dumps({'stopped': stopped, 'confirmed': True, 'data': 'deleted' if purge else 'kept in .dev/'}))


def main():
    args = sys.argv[1:]
    if not args or args[0] in {'-h', '--help', 'help'}:
        return print(__doc__)
    command, rest = args[0], args[1:]
    if command == 'up':
        up()
    elif command == 'status':
        status()
    elif command == 'env':
        for key, value in current_env().items():
            print(f'export {key}={value}')
    elif command == 'exec':
        if not rest:
            fail('usage: python3 dev.py exec CMD...')
        raise SystemExit(mise(*rest, env=dict(os.environ, **current_env())).returncode)
    elif command == 'test':
        raise SystemExit(mise('uv', 'run', '--frozen', 'pytest', '-q', *rest, env=dict(os.environ, **current_env(SERVICES))).returncode)
    elif command == 'smoke':
        raise SystemExit(mise('uv', 'run', '--frozen', 'python', 'smoke.py', env=dict(os.environ, **current_env(('postgres', 'redis', 'api')))).returncode)
    elif command in {'migrate', 'seed'}:
        raise SystemExit(mise('uv', 'run', '--frozen', 'python', f'{command}.py', env=dict(os.environ, **current_env())).returncode)
    elif command == 'restart':
        restart(rest)
    elif command == 'logs':
        for name in rest or ['api', 'worker', 'postgres', 'redis']:
            path = DEV / f'{name}.log'
            print(f'==> {name} ({path})')
            print(''.join(path.read_text(errors='replace').splitlines(True)[-40:]) if path.exists() else '(no log yet)')
    elif command == 'down':
        down('--purge' in rest)
    else:
        fail(f'unknown command {command!r}; run `python3 dev.py --help`')


if __name__ == '__main__':
    main()
