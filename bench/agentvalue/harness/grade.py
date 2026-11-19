"""Hidden grading, identical for both arms.

The grader never uses either arm's tooling. It starts its own pinned PostgreSQL and Redis on
private ports in a temporary directory, runs the submitted code's migrations and processes
directly, and decides every check from what it observes over HTTP, SQL and Redis. Two runs per
submission:

  fresh    an empty database: submitted migrate.py and seed.py, then the API and worker, then
           the ORIGINAL smoke test and the ORIGINAL API tests (the agent cannot weaken them)
  upgrade  an existing database: the STARTING code's migrate.py and seed.py, then hidden data
           the agent never saw, then the submitted migrate.py, API and worker, then the task's
           hidden checks (verify.py, run by a grader-owned interpreter)

A task passes when both runs complete and every required check is true.
"""
import json
import os
from pathlib import Path
import shutil
import signal
import socket
import subprocess
import sys
import tempfile
import time
import urllib.request

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent
FIXTURE = ROOT / 'fixture'
HIDDEN = ROOT / 'hidden'
TASKS = {t['id']: t for t in json.loads((ROOT / 'tasks.json').read_text())['tasks']}

# Grader tools: the host's mise installs of the pinned versions, executed read-only. Agents run
# with their own MISE_DATA_DIR and never see these paths.
INSTALLS = Path.home() / '.local/share/mise/installs'
PG_BIN = INSTALLS / 'postgres/17.11/bin'
REDIS_BIN = INSTALLS / 'redis/7.4.7/bin'
PY_BIN = INSTALLS / 'python/3.13.16/bin'
UV = INSTALLS / 'uv/0.12.23/uv-aarch64-apple-darwin/uv'
GRADER_HOME = Path(os.environ.get('AGENTVALUE_GRADER_HOME', Path.home() / '.cache/stack-agentvalue/grader'))
GRADER_PY = GRADER_HOME / 'venv/bin/python'
# Files the grader always takes from the starting code, never from a submission.
PROTECTED = {'smoke.py', 'stack.toml', 'stack.lock', 'mise.toml', 'mise.lock', 'dev.py', 'README.md', 'AGENTS.md',
             '.gitignore', 'pyproject.toml', 'uv.lock'}
RUNTIME_DIRS = {'.git', '.stack', '.dev', '.venv', '.config', '__pycache__', '.pytest_cache', 'node_modules'}


def free_port():
    with socket.socket() as s:
        s.bind(('127.0.0.1', 0))
        return s.getsockname()[1]


def base_env(extra=None):
    env = {'PATH': os.pathsep.join([str(PY_BIN), str(UV.parent), str(PG_BIN), '/usr/bin', '/bin', '/usr/sbin', '/sbin']),
           'HOME': os.environ['HOME'], 'LANG': 'en_US.UTF-8', 'LC_ALL': 'en_US.UTF-8', 'TZ': 'UTC',
           'UV_CACHE_DIR': str(GRADER_HOME / 'uv-cache'), 'UV_PYTHON': str(PY_BIN / 'python3'), 'NO_COLOR': '1'}
    env.update(extra or {})
    return env


def ensure_grader_venv():
    """A grader-owned interpreter with the client libraries, independent of any submission."""
    if GRADER_PY.exists():
        return
    GRADER_HOME.mkdir(parents=True, exist_ok=True)
    env = base_env()
    subprocess.run([str(UV), 'venv', '--python', str(PY_BIN / 'python3'), str(GRADER_HOME / 'venv')], env=env, check=True,
                   capture_output=True)
    subprocess.run([str(UV), 'pip', 'install', '--python', str(GRADER_PY), 'psycopg[binary]==3.3.6', 'redis==5.3.1'],
                   env=env, check=True, capture_output=True)


class Step(Exception):
    pass


def run(argv, cwd, env, timeout=180, log=None, name=''):
    """Run one grading step in a session of its own; on timeout its whole process group is killed."""
    started = time.monotonic()
    proc = subprocess.Popen([str(a) for a in argv], cwd=cwd, env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
                            stdin=subprocess.DEVNULL, start_new_session=True)
    try:
        out, err = proc.communicate(timeout=timeout)
        code = proc.returncode
    except subprocess.TimeoutExpired:
        stop_group(proc)
        out, err = proc.communicate()
        code, err = 'timeout', (err or '') + '\ntimeout'
    entry = {'step': name, 'argv': [str(a) for a in argv], 'exit': code, 'seconds': round(time.monotonic() - started, 2),
             'stdout': out[-4000:], 'stderr': err[-4000:]}
    if log is not None:
        log.append(entry)
    return entry


class Services:
    """A private PostgreSQL and Redis for one grading run. Always stopped on exit."""

    def __init__(self, workdir, log):
        self.dir, self.log, self.procs = workdir, log, []
        self.pg_port, self.redis_port = free_port(), free_port()
        self.pgdata = workdir / 'pgdata'

    def __enter__(self):
        try:
            return self._start()
        except BaseException:
            self.__exit__(None, None, None)  # stop whatever did start before failing
            raise

    def _start(self):
        env = base_env()
        r = run([PG_BIN / 'initdb', '-D', self.pgdata, '-A', 'trust', '-U', 'postgres', '-E', 'UTF8', '--locale=en_US.UTF-8'],
                self.dir, env, log=self.log, name='initdb')
        if r['exit'] != 0:
            raise Step('initdb failed')
        r = run([PG_BIN / 'pg_ctl', '-D', self.pgdata, '-l', self.dir / 'pg.log', '-w', '-t', '60', '-o',
                 f"-h 127.0.0.1 -p {self.pg_port} -k ''", 'start'], self.dir, env, log=self.log, name='pg_start')
        if r['exit'] != 0:
            raise Step('postgres did not start')
        redis_log = open(self.dir / 'redis.log', 'wb')
        self.redis = subprocess.Popen([str(REDIS_BIN / 'redis-server'), '--bind', '127.0.0.1', '--port', str(self.redis_port),
                                       '--save', '', '--appendonly', 'no', '--dir', str(self.dir)],
                                      stdout=redis_log, stderr=redis_log, stdin=subprocess.DEVNULL, start_new_session=True)
        deadline = time.monotonic() + 20
        while not listening(self.redis_port):
            if time.monotonic() > deadline:
                raise Step('redis did not start')
            time.sleep(0.05)
        return self

    def env(self):
        return {'DATABASE_URL': f'postgresql://postgres@127.0.0.1:{self.pg_port}/postgres',
                'REDIS_URL': f'redis://127.0.0.1:{self.redis_port}/0'}

    def start(self, tree, argv, name, extra):
        logf = open(self.dir / f'{name}.log', 'ab')
        proc = subprocess.Popen(argv, cwd=tree, env=base_env({**self.env(), **extra}), stdout=logf, stderr=logf,
                                stdin=subprocess.DEVNULL, start_new_session=True)
        self.procs.append((name, proc))
        return proc

    def stop_apps(self):
        for _, proc in self.procs:
            stop_group(proc)
        self.procs = []

    def __exit__(self, *exc):
        self.stop_apps()
        if getattr(self, 'redis', None) is not None:
            stop_group(self.redis)
        if (self.pgdata / 'postmaster.pid').exists():
            run([PG_BIN / 'pg_ctl', '-D', self.pgdata, '-m', 'fast', '-w', 'stop'], self.dir, base_env(), log=self.log, name='pg_stop')
        return False


def stop_group(proc):
    if proc.poll() is not None:
        return
    try:
        os.killpg(proc.pid, signal.SIGTERM)
    except ProcessLookupError:
        return
    try:
        proc.wait(5)
    except subprocess.TimeoutExpired:
        try:
            os.killpg(proc.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
        proc.wait(5)


def listening(port):
    with socket.socket() as s:
        s.settimeout(0.2)
        return s.connect_ex(('127.0.0.1', port)) == 0


def prepare_tree(source, dest, starting=None):
    """Copy a tree without runtime directories or symbolic links (a link could reach outside the
    submission). With `starting`, protected files come from it."""
    def ignore(directory, names):
        return [n for n in names if n in RUNTIME_DIRS or os.path.islink(os.path.join(directory, n))]
    shutil.copytree(source, dest, ignore=ignore, symlinks=True)
    if starting is not None:
        for name in PROTECTED:
            if (starting / name).exists():
                shutil.copy2(starting / name, dest / name)
            elif (dest / name).exists():
                (dest / name).unlink()
    return dest


def start_app(services, tree, log, worker=True):
    api_port = free_port()
    services.start(tree, [str(UV), 'run', '--frozen', 'python', 'app.py'], 'api',
                   {'PORT': str(api_port), 'IDENTITY_TOKEN': 'grader'})
    if worker:
        services.start(tree, [str(UV), 'run', '--frozen', 'python', 'worker.py'], 'worker', {})
    url = f'http://127.0.0.1:{api_port}'
    deadline = time.monotonic() + 60
    while time.monotonic() < deadline:
        try:
            with urllib.request.urlopen(url + '/health', timeout=2) as response:
                if response.status == 200:
                    log.append({'step': 'api_ready', 'url': url})
                    return url
        except OSError:
            pass
        if any(proc.poll() is not None for _, proc in services.procs):
            break
        time.sleep(0.2)
    tails = {name: (services.dir / f'{name}.log').read_text(errors='replace')[-3000:] for name, _ in services.procs}
    log.append({'step': 'api_ready', 'failed': True, 'logs': tails})
    raise Step('the submitted API or worker did not start')


def fresh(tree, starting, workdir):
    """Empty database: submitted migrations and seed, then the original smoke test and API tests."""
    log, result = [], {'completed': False}
    with Services(workdir, log) as services:
        try:
            env = base_env(services.env())
            for name, argv in [('sync', [UV, 'sync', '--frozen']), ('migrate', [UV, 'run', '--frozen', 'python', 'migrate.py']),
                               ('seed', [UV, 'run', '--frozen', 'python', 'seed.py'])]:
                if run(argv, tree, env, log=log, name=name)['exit'] != 0:
                    raise Step(f'{name} failed on a fresh database')
            url = start_app(services, tree, log)
            env = base_env({**services.env(), 'API_URL': url})
            smoke = run([UV, 'run', '--frozen', 'python', starting / 'smoke.py'], tree, env, log=log, name='smoke')
            tests = run([UV, 'run', '--frozen', 'pytest', '-q', '-p', 'no:cacheprovider', '--rootdir', str(workdir),
                         str(starting / 'tests' / 'test_api.py')], tree, env, log=log, name='original_tests')
            result.update(completed=True, smoke=smoke['exit'] == 0, original_tests=tests['exit'] == 0)
        except Step as error:
            result['error'] = str(error)
    result['log'] = log
    return result


def upgrade(task_id, tree, starting, workdir):
    """Existing database with hidden data, upgraded by the submitted migrations, then hidden checks."""
    log, result = [], {'completed': False}
    with Services(workdir, log) as services:
        try:
            env = base_env(services.env())
            for name, argv, where in [('sync_start', [UV, 'sync', '--frozen'], starting),
                                      ('migrate_start', [UV, 'run', '--frozen', 'python', 'migrate.py'], starting),
                                      ('seed_start', [UV, 'run', '--frozen', 'python', 'seed.py'], starting),
                                      ('hidden_data', [GRADER_PY, HIDDEN / 'data.py'], starting),
                                      ('sync', [UV, 'sync', '--frozen'], tree),
                                      ('migrate', [UV, 'run', '--frozen', 'python', 'migrate.py'], tree)]:
                if run(argv, where, env, log=log, name=name)['exit'] != 0:
                    raise Step(f'{name} failed while upgrading an existing database')
            # For the queue task the hidden checks run the worker themselves, to hold jobs back.
            url = start_app(services, tree, log, worker=task_id != 'pay-receipts')
            env = base_env({**services.env(), 'API_URL': url, 'GRADE_TREE': str(tree), 'GRADE_STARTING': str(starting),
                            'GRADE_UV': str(UV), 'GRADE_WORKER_LOG': str(workdir / 'verify-worker.log')})
            verify = run([GRADER_PY, HIDDEN / 'verify.py', task_id], tree, env, timeout=240, log=log, name='verify')
            lines = [line for line in verify['stdout'].splitlines() if line.startswith('{')]
            checks = json.loads(lines[-1]) if lines else {}
            result.update(completed=verify['exit'] == 0 and bool(lines), checks=checks)
        except Step as error:
            result['error'] = str(error)
    result['log'] = log
    return result


def reclaim_under(path):
    """Stop anything a grading run left (a submission's daemon, say) and report it."""
    sys.path.insert(0, str(HERE))
    import observe
    left = observe.legacy.owned_processes(path)
    if not left:
        return {'left': [], 'survivors': []}
    result = observe.reclaim(left, path)
    return {'left': [{k: p[k] for k in ['pid', 'command']} for p in left], 'survivors': result['survivors']}


def grade(task_id, submitted_tree, out_dir):
    """Grade one submitted tree. Writes grade.json in out_dir and returns it."""
    ensure_grader_venv()
    out_dir = Path(out_dir)
    out_dir.mkdir(parents=True, exist_ok=True)
    task = TASKS[task_id]
    with tempfile.TemporaryDirectory(prefix='avg-', dir='/tmp') as tmp:
        tmp = Path(tmp)
        starting = prepare_tree(FIXTURE, tmp / 'starting')
        fresh_tree = prepare_tree(Path(submitted_tree), tmp / 'fresh-tree', starting)
        upgrade_tree = prepare_tree(Path(submitted_tree), tmp / 'upgrade-tree', starting)
        (tmp / 'fresh').mkdir()
        (tmp / 'upgrade').mkdir()
        f = fresh(fresh_tree, starting, tmp / 'fresh')
        u = upgrade(task_id, upgrade_tree, starting, tmp / 'upgrade')
        cleanup = reclaim_under(tmp)
    checks = u.get('checks', {})
    missing = [c for c in task['required_checks'] if c not in checks]
    failed = [c for c in task['required_checks'] if checks.get(c) is not True]
    passed = bool(f.get('completed') and f.get('smoke') and f.get('original_tests') and u.get('completed') and not failed)
    result = {'task': task_id, 'passed': passed, 'grader_cleanup': cleanup, 'fresh_ok': bool(f.get('completed') and f.get('smoke') and f.get('original_tests')),
              'upgrade_completed': bool(u.get('completed')), 'checks': checks, 'failed_checks': failed, 'missing_checks': missing,
              'fresh': f, 'upgrade': u}
    (out_dir / 'grade.json').write_text(json.dumps(result, indent=2))
    return result


if __name__ == '__main__':
    task, tree, out = sys.argv[1:4]
    r = grade(task, tree, out)
    print(json.dumps({k: r[k] for k in ['task', 'passed', 'fresh_ok', 'upgrade_completed', 'failed_checks']}))
