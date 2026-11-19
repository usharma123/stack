#!/usr/bin/env python3
"""Local development for this repository. Every git worktree gets its own services.

Tools are pinned in mise.toml. Each worktree runs its own PostgreSQL, Redis, API and worker,
on ports chosen for that worktree, with data under .dev/ in the worktree. Nothing is shared
between worktrees, so several people or agents can work in parallel.

  python3 dev.py up               install pinned tools, start everything, migrate, seed a new database
  python3 dev.py status           show this worktree's services, ports and process IDs (JSON)
  python3 dev.py env              print the environment as `export` lines: eval "$(python3 dev.py env)"
  python3 dev.py exec CMD...      run a command with the pinned tools and this worktree's service URLs
  python3 dev.py test [ARGS...]   run the test suite (pytest) against this worktree's services
  python3 dev.py smoke            run the smoke test against this worktree's services
  python3 dev.py restart [api|worker]   restart the app processes after you change their code
  python3 dev.py migrate          apply pending migrations now
  python3 dev.py seed             load the demo data again (safe to repeat)
  python3 dev.py logs [NAME]      show the last lines of a service's log (api, worker, postgres, redis)
  python3 dev.py down [--purge]   stop everything this worktree started and confirm it is gone;
                                  --purge also deletes this worktree's data
"""
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
PORTS = DEV / 'ports.json'
PGDATA = DEV / 'postgres'
SEEDED = DEV / 'seeded'
APPS = {'api': ['uv', 'run', '--frozen', 'python', 'app.py'], 'worker': ['uv', 'run', '--frozen', 'python', 'worker.py']}
SERVICES = ['postgres', 'redis', 'api']


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
    DEV.mkdir(exist_ok=True)
    tmp = path.with_suffix('.tmp')
    tmp.write_text(json.dumps(data, indent=2))
    tmp.replace(path)


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


def choose_ports():
    """Stable ports per worktree: a slot derived from the worktree path, moving on if taken."""
    saved = load(PORTS)
    if saved and all(port_free(saved[name]) for name in SERVICES):
        return saved
    slot = int(hashlib.sha256(str(ROOT).encode()).hexdigest(), 16) % 900
    for attempt in range(900):
        base = 30000 + ((slot + attempt) % 900) * 10
        ports = {name: base + offset for offset, name in enumerate(SERVICES)}
        if all(port_free(port) for port in ports.values()):
            save(PORTS, ports)
            return ports
    fail('no free port block found between 30000 and 38999')


def environment(ports):
    user = 'postgres'
    return {'DATABASE_URL': f'postgresql://{user}@127.0.0.1:{ports["postgres"]}/postgres',
            'REDIS_URL': f'redis://127.0.0.1:{ports["redis"]}/0',
            'API_URL': f'http://127.0.0.1:{ports["api"]}', 'API_PORT': str(ports['api'])}


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
    new_database = not PGDATA.exists()
    if new_database:
        mise('initdb', '-D', str(PGDATA), '-U', 'postgres', '-A', 'trust', '-E', 'UTF8', '--locale=en_US.UTF-8',
             stdout=subprocess.DEVNULL, check=True)
    mise('pg_ctl', '-D', str(PGDATA), '-l', str(DEV / 'postgres.log'), '-w', '-t', '60',
         '-o', f"-h 127.0.0.1 -p {ports['postgres']} -k ''", 'start', stdout=subprocess.DEVNULL, check=True)
    pid = int((PGDATA / 'postmaster.pid').read_text().splitlines()[0])
    record['services']['postgres'] = {'pid': pid, 'started': process_start(pid), 'port': ports['postgres']}
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


def start_app(name, ports, record):
    env = dict(os.environ, **environment(ports), PORT=str(ports['api']), IDENTITY_TOKEN=record['identity'])
    with (DEV / f'{name}.log').open('ab') as log:
        process = subprocess.Popen(['mise', 'x', '--', *APPS[name]], cwd=ROOT, env=env, stdout=log, stderr=log,
                                   stdin=subprocess.DEVNULL, start_new_session=True)
    entry = {'pid': process.pid, 'started': process_start(process.pid), 'pgid': process.pid}
    if name == 'api':
        entry['port'] = ports['api']
    record['services'][name] = entry
    save(STATE, record)


def wait_api(ports, record):
    deadline = time.monotonic() + 60
    while time.monotonic() < deadline:
        try:
            with urllib.request.urlopen(f'http://127.0.0.1:{ports["api"]}/identity', timeout=1) as response:
                if response.read().decode() == record['identity']:
                    return
        except (OSError, ValueError):
            pass
        if not alive(record['services']['api']):
            break
        time.sleep(0.2)
    fail(f'the api did not become ready; see `python3 dev.py logs api`')


def up():
    record = state()
    running = {name: alive(entry) for name, entry in record['services'].items()}
    if record['services'] and all(running.values()) and set(running) == {'postgres', 'redis', 'api', 'worker'}:
        return print(json.dumps(summary(record), indent=2))
    if any(running.values()):
        fail('some services of this worktree are still running from an earlier start; run `python3 dev.py down` first')
    setup()
    ports = choose_ports()
    record = {'services': {}, 'ports': ports, 'identity': secrets.token_hex(16), 'worktree': str(ROOT)}
    save(STATE, record)  # saved early so `down` can stop a partial start
    new_database = start_postgres(ports, record)
    save(STATE, record)
    start_redis(ports, record)
    save(STATE, record)
    env = dict(os.environ, **environment(ports))
    if mise('uv', 'run', '--frozen', 'python', 'migrate.py', env=env).returncode != 0:
        fail('migrate.py failed (services are running; fix it and run `python3 dev.py up` again, or `down`)')
    if new_database or not SEEDED.exists():
        if mise('uv', 'run', '--frozen', 'python', 'seed.py', env=env).returncode != 0:
            fail('seed.py failed (services are running; fix it and run `python3 dev.py up` again, or `down`)')
        SEEDED.write_text('seeded\n')
    start_app('api', ports, record)
    start_app('worker', ports, record)
    wait_api(ports, record)
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


def current_env():
    record = state()
    if not record['services'] or not all(alive(e) for e in record['services'].values()):
        fail('this worktree\'s services are not all running; run `python3 dev.py up` (see `python3 dev.py status`)')
    return environment(record['ports'])


def stop_process(name, entry, timeout=10):
    if not alive(entry):
        return True
    if name == 'postgres':
        mise('pg_ctl', '-D', str(PGDATA), '-m', 'fast', '-w', 'stop', stdout=subprocess.DEVNULL)
    else:
        try:
            if 'pgid' in entry:
                os.killpg(entry['pgid'], signal.SIGTERM)
            else:
                os.kill(entry['pid'], signal.SIGTERM)
        except ProcessLookupError:
            pass
    deadline = time.monotonic() + timeout
    while alive(entry) and time.monotonic() < deadline:
        time.sleep(0.05)
    if alive(entry):
        try:
            os.kill(entry['pid'], signal.SIGKILL)
        except ProcessLookupError:
            pass
        time.sleep(0.2)
    if 'pgid' in entry:  # anything else the app's process group still holds (uv's child)
        try:
            os.killpg(entry['pgid'], signal.SIGKILL)
        except (ProcessLookupError, PermissionError):
            pass
    return not alive(entry)


def restart(names):
    record = state()
    if not record['services']:
        fail('nothing is running; run `python3 dev.py up`')
    for name in names or ['api', 'worker']:
        if name not in APPS:
            fail(f'can only restart api or worker, not {name}')
        stop_process(name, record['services'].get(name, {}))
        start_app(name, record['ports'], record)
    if 'api' in (names or ['api']):
        wait_api(record['ports'], record)
    print(json.dumps(summary(record), indent=2))


def down(purge):
    record = state()
    stopped, left = [], []
    for name in ['worker', 'api', 'redis', 'postgres']:
        entry = record['services'].get(name)
        if not entry:
            continue
        (stopped if stop_process(name, entry) else left).append(name)
    ports_open = [p for p in (record.get('ports') or {}).values() if listening(p)
                  and any(alive(e) for e in record['services'].values())]
    if left:
        fail(f'could not stop {left}; check `python3 dev.py status`')
    save(STATE, {'services': {}})
    if purge and DEV.exists():
        shutil.rmtree(DEV)
    print(json.dumps({'stopped': stopped, 'confirmed': not left and not ports_open, 'data': 'deleted' if purge else 'kept in .dev/'}))


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
        raise SystemExit(mise('uv', 'run', '--frozen', 'pytest', '-q', *rest, env=dict(os.environ, **current_env())).returncode)
    elif command == 'smoke':
        raise SystemExit(mise('uv', 'run', '--frozen', 'python', 'smoke.py', env=dict(os.environ, **current_env())).returncode)
    elif command in {'migrate', 'seed'}:
        raise SystemExit(mise('uv', 'run', '--frozen', 'python', f'{command}.py', env=dict(os.environ, **current_env())).returncode)
    elif command == 'restart':
        restart(rest)
    elif command == 'logs':
        names = rest or ['api', 'worker', 'postgres', 'redis']
        for name in names:
            path = DEV / f'{name}.log'
            print(f'==> {name} ({path})')
            print(''.join(path.read_text(errors='replace').splitlines(True)[-40:]) if path.exists() else '(no log yet)')
    elif command == 'down':
        down('--purge' in rest)
    else:
        fail(f'unknown command {command!r}; run `python3 dev.py --help`')


if __name__ == '__main__':
    main()
