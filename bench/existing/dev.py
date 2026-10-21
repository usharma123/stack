#!/usr/bin/env python3
"""Development helper for this project: pinned tools from mise.toml, services run natively.

  python3 dev.py setup            install pinned tools and dependencies
  python3 dev.py up               start services, load existing data, run migrate.py, start the app
  python3 dev.py status           print endpoints and process IDs as JSON
  python3 dev.py test             run the existing smoke test
  python3 dev.py exec ARGV...     run a command with the tools and service URLs
  python3 dev.py down             stop everything (data is kept)
"""
import json
import os
from pathlib import Path
import random
import signal
import socket
import subprocess
import sys
import time
import urllib.request

ROOT = Path(__file__).resolve().parent
CONFIG = json.loads((ROOT / '.dev.json').read_text())
PROJECT = CONFIG['project']
STATE_DIR = ROOT / '.dev'
STATE = STATE_DIR / 'state.json'
APP = {'ledger': ('api', ['uv', 'run', '--frozen', 'python', 'app.py']),
       'catalog': ('web', ['node', 'server.mjs'])}.get(PROJECT)
SMOKE = {'ledger': ['uv', 'run', '--frozen', 'python', 'smoke.py']}.get(PROJECT, ['node', 'smoke.mjs'])


def run(argv, env=None, **kwargs):
    return subprocess.run(['mise', 'x', '--', *argv], cwd=ROOT, env=env or os.environ, check=True, **kwargs)


def state():
    return json.loads(STATE.read_text()) if STATE.exists() else None


def endpoint_env():
    current = state()
    return dict(os.environ, **(current['endpoints'] if current else {}))


def alive(pid):
    out = subprocess.run(['ps', '-p', str(pid), '-o', 'stat='], capture_output=True, text=True).stdout.strip()
    return bool(out) and not out.startswith('Z')


def fingerprint(pid):
    # Start time survives the exec from `mise x` into the app; the command line does not.
    return subprocess.run(['ps', '-p', str(pid), '-o', 'lstart='], capture_output=True, text=True).stdout.strip()


def listening(port):
    with socket.socket() as connection:
        connection.settimeout(0.3)
        return connection.connect_ex(('127.0.0.1', port)) == 0


def free_ports(count):
    ports = []
    while len(ports) < count:
        port = random.randint(30000, 39999)
        with socket.socket() as probe:
            try:
                probe.bind(('127.0.0.1', port))
            except OSError:
                continue
        if port not in ports:
            ports.append(port)
    return ports


def setup():
    subprocess.run(['mise', 'trust', str(ROOT / 'mise.toml')], cwd=ROOT, check=True)
    subprocess.run(['mise', 'install'], cwd=ROOT, check=True)
    if PROJECT == 'ledger':
        run(['uv', 'sync', '--frozen'])


def wait_identity(url):
    deadline = time.monotonic() + 60
    while time.monotonic() < deadline:
        try:
            with urllib.request.urlopen(url + '/identity', timeout=1) as response:
                if response.read().decode() == CONFIG['identity']:
                    return
        except (OSError, ValueError):
            pass
        time.sleep(0.2)
    raise SystemExit(f'the app did not become ready; see {STATE_DIR / "app.log"}')


def up():
    if APP is None:
        print('this project has no services')
        return
    current = state()
    if current and all(alive(pid) for pid in current['pids'].values()):
        print(json.dumps(current, indent=2))
        return
    if current:
        raise SystemExit('a previous start is partly running; run `python3 dev.py down` first')
    STATE_DIR.mkdir(exist_ok=True)
    name, command = APP
    record = {'endpoints': {}, 'pids': {}, 'ports': {}}
    if PROJECT == 'ledger':
        pg, redis, port = free_ports(3)
        record['ports'] = {'postgres': pg, 'redis': redis, name: port}
        user = os.environ.get('USER') or 'postgres'
        record['endpoints'] = {'DATABASE_URL': f'postgresql://{user}@127.0.0.1:{pg}/postgres',
                               'REDIS_URL': f'redis://127.0.0.1:{redis}/0'}
        STATE.write_text(json.dumps(record))  # Saved first so `down` can stop a partial start.
        pgdata = STATE_DIR / 'postgres'
        if not pgdata.exists():
            run(['initdb', '-D', str(pgdata), '-A', 'trust', '-E', 'UTF8'], stdout=subprocess.DEVNULL)
        run(['pg_ctl', '-D', str(pgdata), '-l', str(STATE_DIR / 'postgres.log'), '-o', f"-h 127.0.0.1 -p {pg} -k ''", '-w', 'start'])
        record['pids']['postgres'] = int((pgdata / 'postmaster.pid').read_text().splitlines()[0])
        STATE.write_text(json.dumps(record))
        (STATE_DIR / 'redis').mkdir(exist_ok=True)
        run(['redis-server', '--bind', '127.0.0.1', '--port', str(redis), '--dir', str(STATE_DIR / 'redis'), '--daemonize', 'yes',
             '--pidfile', str(STATE_DIR / 'redis.pid'), '--logfile', str(STATE_DIR / 'redis.log')])
        deadline = time.monotonic() + 10
        while not (STATE_DIR / 'redis.pid').exists() and time.monotonic() < deadline:
            time.sleep(0.1)
        record['pids']['redis'] = int((STATE_DIR / 'redis.pid').read_text().strip())
        STATE.write_text(json.dumps(record))
        env = dict(os.environ, **record['endpoints'])
        run(['uv', 'run', '--frozen', 'python', 'seed.py'], env=env)
        run(['uv', 'run', '--frozen', 'python', 'migrate.py'], env=env)
    else:
        (port,) = free_ports(1)
        record['ports'] = {name: port}
    record['endpoints'].update({f'{name.upper()}_URL': f'http://127.0.0.1:{port}', f'{name.upper()}_PORT': str(port)})
    env = dict(os.environ, **record['endpoints'], PORT=str(port), IDENTITY_TOKEN=CONFIG['identity'])
    with (STATE_DIR / 'app.log').open('ab') as log:
        process = subprocess.Popen(['mise', 'x', '--', *command], cwd=ROOT, env=env, stdout=log, stderr=log,
                                   stdin=subprocess.DEVNULL, start_new_session=True)
    record['pids'][name] = process.pid
    record['app_fingerprint'] = fingerprint(process.pid)
    STATE.write_text(json.dumps(record))
    wait_identity(record['endpoints'][f'{name.upper()}_URL'])
    print(json.dumps(record, indent=2))


def down():
    current = state()
    if current is None:
        print('nothing is running')
        return
    errors = []
    name = APP[0]
    pid = current['pids'].get(name)
    if pid and alive(pid):
        if fingerprint(pid) == current.get('app_fingerprint'):
            os.killpg(pid, signal.SIGTERM)
        else:
            errors.append(f'{name} PID {pid} is a different process now; left alone')
    if 'postgres' in current['pids'] and alive(current['pids']['postgres']):
        try:
            run(['pg_ctl', '-D', str(STATE_DIR / 'postgres'), '-m', 'fast', '-w', 'stop'])
        except subprocess.CalledProcessError as error:
            errors.append(f'pg_ctl stop failed: {error}')
    if 'redis' in current['pids'] and alive(current['pids']['redis']):
        url = current['endpoints']['REDIS_URL']
        directory = run(['redis-cli', '-u', url, 'CONFIG', 'GET', 'dir'], capture_output=True, text=True).stdout.split()
        if str(STATE_DIR / 'redis') in directory:
            run(['redis-cli', '-u', url, 'shutdown', 'save'], capture_output=True)
        else:
            errors.append('the Redis on this port is not this project\'s; left alone')
    deadline = time.monotonic() + 15
    while time.monotonic() < deadline and (any(alive(p) for p in current['pids'].values())
                                           or any(listening(p) for p in current['ports'].values())):
        time.sleep(0.1)
    errors += [f'{svc} PID {p} still running' for svc, p in current['pids'].items() if alive(p)]
    errors += [f'{svc} port {p} still open' for svc, p in current['ports'].items() if listening(p)]
    if errors:
        raise SystemExit('; '.join(errors))
    STATE.unlink()
    print(json.dumps({'stopped': current['pids']}))


def main():
    os.chdir(ROOT)
    command, rest = (sys.argv[1], sys.argv[2:]) if len(sys.argv) > 1 else ('help', [])
    if command == 'setup':
        setup()
    elif command == 'up':
        up()
    elif command == 'down':
        down()
    elif command == 'status':
        print(json.dumps(state(), indent=2))
    elif command == 'test':
        return subprocess.run(['mise', 'x', '--', *SMOKE], cwd=ROOT, env=endpoint_env()).returncode
    elif command == 'exec' and rest:
        return subprocess.run(['mise', 'x', '--', *rest], cwd=ROOT, env=endpoint_env()).returncode
    else:
        print(__doc__)
        return 2
    return 0


if __name__ == '__main__':
    sys.exit(main())
