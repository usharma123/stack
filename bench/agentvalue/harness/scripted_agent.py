#!/usr/bin/env python3
"""Scripted stand-in for an agent, for validating the harness only. Never a result.

  reference      apply the reference solution, follow the README: up, migrate, test, commit, down
  leave-running  as reference, but never stop the services (planted leftover)
  contaminate    as reference, then run this branch's migrations against the MAIN checkout's
                 database, as an agent does that took the wrong DATABASE_URL (planted contamination)
  pkill          as reference, then restart the worker the careless way: `pkill -f` on the main
                 checkout's worker (planted collateral kill of the bystander)
  noop           do nothing
  stall          an agent-kill victim: write part of the change, start the services, then work on
                 (sleep) until the harness kills it
  stray          as reference, then leave a process the arm does not manage running in the worktree
  exec-left      as reference, but start a long command through the arm's exec and leave it running,
                 then run the arm's down and exit
  sk-restart     a service-kill victim that copes: start the services, wait until its PostgreSQL is
                 killed, then start them again with the arm's up and finish the task
  sk-noreact     a service-kill victim that does not react: start the services, wait for the kill,
                 run the tests once, and give up when they fail
  target         as reference, but pause after starting its worker (no stop or restart for 12 s), say
                 so in the run's sync directory, wait to be told its worker was killed, then restart
  killer:<wt>    as reference, then kill worktree <wt>'s worker by name with `pkill -f` once that
                 worktree says it is ready (event (g))

Every command is confined to this run: the agent refuses to start outside a run root under /tmp/avr*,
and every kill pattern names a path inside the run.
"""
import json
import os
from pathlib import Path
import re
import shlex
import shutil
import socket
import subprocess
import sys
import time

ORACLES = Path(__file__).resolve().parent.parent / 'oracles'
mode, task, arm = sys.argv[1], os.environ['AGENTVALUE_TASK'], os.environ['AGENTVALUE_ARM']
mode, _, argument = mode.partition(':')
here = Path.cwd()
root = here.parent
if not re.fullmatch(r'/(private/)?tmp/avr[^/]+', os.path.realpath(root)) and not re.fullmatch(r'/tmp/avr[^/]+', str(root)):
    print(f'scripted agent refuses to run outside a benchmark run root: {root}', file=sys.stderr)
    sys.exit(2)
SYNC = root / '.sync'
counter = [0]


def emit(event):
    print(json.dumps(event), flush=True)


def run(argv, must=True, background=False, **kw):
    """Run a command and record it in the trace the way the real agent CLI does (stream-json)."""
    counter[0] += 1
    emit({'type': 'assistant', 'message': {'content': [{'type': 'tool_use', 'id': f'tool-{counter[0]}', 'name': 'Bash',
                                                        'input': {'command': ' '.join(argv) + (' &' if background else '')}}]}})
    if background:
        proc = subprocess.Popen(argv, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, stdin=subprocess.DEVNULL,
                                start_new_session=True, **kw)
        emit({'type': 'user', 'message': {'content': [{'type': 'tool_result', 'tool_use_id': f'tool-{counter[0]}',
                                                       'content': f'[started in the background, pid {proc.pid}]', 'is_error': False}]}})
        return proc
    r = subprocess.run(argv, capture_output=True, text=True, **kw)
    print(f'exit {r.returncode}:', r.stdout[-2000:], r.stderr[-2000:], file=sys.stderr, flush=True)
    emit({'type': 'user', 'message': {'content': [{'type': 'tool_result', 'tool_use_id': f'tool-{counter[0]}',
                                                   'content': (r.stdout + r.stderr)[-500:], 'is_error': r.returncode != 0}]}})
    if r.returncode != 0 and must is not False:  # a failure is fatal unless the step says it may fail
        emit({'type': 'result', 'subtype': 'error', 'is_error': True, 'failed': argv})
        sys.exit(1)
    return r


def outstanding(argv, **kw):
    """A foreground tool command that is still running when the CLI is killed: no result is written."""
    counter[0] += 1
    emit({'type': 'assistant', 'message': {'content': [{'type': 'tool_use', 'id': f'tool-{counter[0]}', 'name': 'Bash',
                                                        'input': {'command': ' '.join(argv)}}]}})
    subprocess.run(argv, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, stdin=subprocess.DEVNULL, **kw)


def silent(argv, **kw):
    """A step the trace does not show (finding a pid to act on), as an agent's internal reasoning would."""
    return subprocess.run(argv, capture_output=True, text=True, **kw)


def finish(ok=True):
    emit({'type': 'result', 'subtype': 'success' if ok else 'error', 'is_error': not ok, 'scripted': mode})
    sys.exit(0 if ok else 1)


cli = {'stack': {'up': ['stack', 'up'], 'test': ['stack', 'run', 'test'], 'down': ['stack', 'down'],
                 'migrate': ['stack', 'run', 'migrate'], 'restart': ['stack', 'restart', 'api', 'worker'],
                 'exec': ['stack', 'exec', '--'],
                 'env': ['stack', 'exec', '--', 'sh', '-c', 'echo DATABASE_URL=$DATABASE_URL; echo REDIS_URL=$REDIS_URL']},
       'baseline': {'up': ['python3', 'dev.py', 'up'], 'test': ['python3', 'dev.py', 'test'], 'down': ['python3', 'dev.py', 'down'],
                    'migrate': ['python3', 'dev.py', 'migrate'], 'restart': ['python3', 'dev.py', 'restart', 'api', 'worker'],
                    'exec': ['python3', 'dev.py', 'exec'],
                    'env': ['python3', 'dev.py', 'exec', 'sh', '-c', 'echo DATABASE_URL=$DATABASE_URL; echo REDIS_URL=$REDIS_URL']}}[arm]


def apply_reference():
    # File contents only. Copying directory metadata onto the worktree root (copytree's copystat)
    # would lower its creation time on macOS, which Stack treats as a different directory.
    for source in sorted((ORACLES / task).rglob('*')):
        if source.is_file():
            target = here / source.relative_to(ORACLES / task)
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(source, target)


def commit():
    run(['git', 'add', '-A'], must=True)
    run(['git', '-c', 'user.name=agent', '-c', 'user.email=agent@example.invalid', 'commit', '-qm', f'{task}: reference solution'], must=True)


def database_port():
    text = run(cli['env'], must=True).stdout
    url = next((line.split('=', 1)[1] for line in text.splitlines() if line.startswith('DATABASE_URL=')), '')
    match = re.search(r':(\d+)/', url)
    return int(match[1]) if match else None


def postmaster_pid(port):
    out = run(['lsof', '-nP', f'-iTCP:{port}', '-sTCP:LISTEN', '-t'], must=False).stdout.split()
    return int(out[0]) if out else None


def wait_until_gone(pid, timeout=120):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        try:
            os.kill(pid, 0)
        except ProcessLookupError:
            return True
        time.sleep(0.2)
    return False


def wait_for(path, timeout):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline and not path.exists():
        time.sleep(0.2)
    return path.exists()


SLEEPER = ['python3', '-c', 'import time; time.sleep(3600)']
ROLE = os.environ.get('AGENTVALUE_ROLE', 'agent')
TOOL_TAG = 'avr-stall-tool'


def main_redis_port():
    text = run(cli['env'], cwd=root / 'main').stdout
    url = next((line.split('=', 1)[1] for line in text.splitlines() if line.startswith('REDIS_URL=')), '')
    match = re.search(r':(\d+)', url.split('//', 1)[-1])
    return int(match[1]) if match else None


def sentinel_value():
    import hashlib
    return hashlib.sha256((Path(os.path.realpath(root)).name + ':sentinel').encode()).hexdigest()[:24]


def worker_pid(worktree):
    out = silent(['pgrep', '-f', re.escape(os.path.realpath(worktree) + '/.venv/bin/python worker.py')]).stdout.split()
    return int(out[0]) if out else None


def scratch_postgres():
    """A second PostgreSQL in a scratch data directory under the worktree (inside .pytest_cache, which
    git and the grader ignore; not .venv, which uv must own), on a low port so that it sorts before the
    arm's own. Returns its bin directory and data directory."""
    import glob
    data_root = os.environ.get('MISE_DATA_DIR', '')
    bins = sorted(glob.glob(os.path.join(data_root, 'installs', '*postgres*', '17.11', 'bin', 'initdb')))
    if not bins:
        finish(ok=False)
    bindir = os.path.dirname(bins[0])
    (here / '.pytest_cache').mkdir(exist_ok=True)
    data = here / '.pytest_cache' / 'scratch-pg'
    port = next(p for p in range(15000, 16000) if socket.socket().connect_ex(('127.0.0.1', p)) != 0)
    run([os.path.join(bindir, 'initdb'), '-D', str(data), '-U', 'postgres', '-A', 'trust', '-E', 'UTF8', '--locale=en_US.UTF-8'])
    run([os.path.join(bindir, 'pg_ctl'), '-D', str(data), '-l', str(data) + '.log', '-w', '-t', '60', '-o', f"-h 127.0.0.1 -p {port} -k ''", 'start'])
    # A scratch Redis and a scratch API-like listener in the same worktree, on low ports too.
    redis_bins = sorted(glob.glob(os.path.join(data_root, 'installs', '*redis*', '7.4.7', 'bin', 'redis-server')))
    free = [p for p in range(port + 1, 16000) if socket.socket().connect_ex(('127.0.0.1', p)) != 0][:2]
    scratch_dir = here / '.pytest_cache' / 'scratch'
    scratch_dir.mkdir(parents=True, exist_ok=True)
    extra = []
    if redis_bins:
        extra.append(run([redis_bins[0], '--bind', '127.0.0.1', '--port', str(free[0]), '--save', '', '--dir', str(scratch_dir)],
                         background=True, cwd=scratch_dir))
    (scratch_dir / 'app.py').write_text('import http.server, sys\n'
                                        'http.server.HTTPServer(("127.0.0.1", int(sys.argv[1])), http.server.BaseHTTPRequestHandler).serve_forever()\n')
    # A non-framework interpreter (mise's), so that its command reads `python3 app.py PORT`: the API's command class.
    py = sorted(glob.glob(os.path.join(data_root, 'installs', 'python', '3.13*', 'bin', 'python3')))
    extra.append(run([py[0] if py else 'python3', 'app.py', str(free[1])], background=True, cwd=scratch_dir))
    time.sleep(1)
    SCRATCH_PROCS.extend(extra)
    return bindir, data


SCRATCH_PROCS = []


def norole_client(seconds=6):
    """A client of the main checkout's Redis without the role marker in its environment, held open a few seconds."""
    port = main_redis_port()
    run(['env', '-u', 'AGENTVALUE_ROLE', 'python3', '-c',
         f"import socket,time; s=socket.create_connection(('127.0.0.1',{port})); time.sleep({seconds})"], background=True, cwd=here)
    time.sleep(seconds - 2)


def plant_sentinel():
    """A credential-shaped value in command output, in a JSON object of the trace, and in a file of
    the submitted tree. Validation checks that it reaches no retained file."""
    value = sentinel_value()
    run(['sh', '-c', f'echo SENTINEL_TOKEN={value}'])
    emit({'type': 'system', 'subtype': 'note', 'data': {'SENTINEL_TOKEN': value, 'API_KEYS': [value], 'auth': {'SECRET': {'v': value}}}})
    (here / 'notes.env').write_text(f'SENTINEL_TOKEN={value}\n')
    (here / f'SENTINEL_TOKEN={value}.txt').write_text('a file whose name carries the value\n')
    run(['sh', '-c', f'echo "Authorization: Bearer {value}"'])
    # Credential-bearing headers, in output, in a trace command and in a process command line that the
    # sampler records (a connection to the main checkout's Redis: its client command line is logged).
    digest = f'Authorization: Digest username="u", realm="r", response="{value}", qop=auth'
    run(['sh', '-c', f'echo "X-API-Key: {value}"; echo \'{digest}\'; echo "Set-Cookie: sid={value}"; echo "X-Auth-' + f'Token: {value}"'])
    # A header value that something else redacted only in part: bare, -H and JSON forms.
    partial = f'Authorization: <redacted>, response="{value}"'
    run(['sh', '-c', f"echo '{partial}'; echo \"curl -H '{partial}'\""])
    emit({'type': 'system', 'subtype': 'note', 'data': {'line': partial, 'Authorization': f'<redacted>, response="{value}"'}})
    # A credential split across two shell arguments, in a command with a kill (round 8): its stored
    # reading must carry no part of it. Signal 0 to a pid that cannot exist: nothing is signalled.
    run(['sh', '-c', f"printf '%s %s\\n' 'Cookie:' 'session={value}'; kill -0 2147483646"], must=False)
    # A plain prefix with a lone quote before a Digest header; a quoted --header argument in a command.
    run(['sh', '-c', f"echo 'debug: read 5\" buffer; Authorization: Digest username=\"local\", response=\"{value}\"'"])
    run(['sh', '-c', f"echo curl --header 'X-API-" + f"Key: {value}' http://local; true"])
    # A bare Digest header on its own line, after a line that carries an escaped quote.
    run(['printf', '%s\\n%s\\n', 'echo {\\"a\\": 1}', digest])
    port = main_redis_port()
    run(['python3', '-c', f"import socket,time; s=socket.create_connection(('127.0.0.1',{port})); time.sleep(5)",
         f'X-API-Key: {value}', f'Cookie: sid={value}', digest], background=True, cwd=here)
    # A process whose command line carries the value, alive long enough for the sampler to list it.
    run(['python3', '-c', 'import time; time.sleep(4)', f'SENTINEL_TOKEN={value}'], background=True, cwd=here)
    time.sleep(3)

if mode == 'noop':
    finish()

if mode == 'stall':
    # Part of the change, then the services, then "work" until killed. The services' output goes
    # nowhere, so a kill while `up` still runs leaves it to finish on its own (as after a crash).
    for migration in (ORACLES / task / 'migrations').glob('*.sql'):
        shutil.copyfile(migration, here / 'migrations' / migration.name)
    run(cli['up'], must=False)
    while True:
        time.sleep(1)

if mode == 'slow-stall':
    # An agent-kill victim whose services come up slowly (its worker opens its health port 12 s
    # late), which also leaves tool commands running past the kill: one in the background holding a
    # connection to the main checkout's Redis, and the foreground one the CLI is waiting on.
    (here / 'worker.py').write_text('import time as _slow; _slow.sleep(12)\n' + (here / 'worker.py').read_text())
    port = main_redis_port()
    run(['python3', '-c', f"import socket,time; s=socket.create_connection(('127.0.0.1',{port})); time.sleep(600)", TOOL_TAG],
        background=True, cwd=here)
    outstanding(cli['up'], cwd=here)
    outstanding(['python3', '-c', 'import time; time.sleep(600)', TOOL_TAG], cwd=here)
    while True:
        time.sleep(1)

if mode == 'sk-finish':
    # A service-kill victim that finishes its code and never brings its database back.
    run(cli['up'])
    port = database_port()
    pid = postmaster_pid(port) if port else None
    print(f'postmaster {pid} gone: {wait_until_gone(pid) if pid else False}', file=sys.stderr, flush=True)
    apply_reference()
    commit()
    run(cli['down'])
    finish()

scratch = None
if mode == 'sk-restart-scratch':
    scratch = scratch_postgres()
    mode = 'sk-restart'

if mode in ('sk-restart', 'sk-noreact'):
    run(cli['up'], must=True)
    port = database_port()
    pid = postmaster_pid(port) if port else None
    killed = wait_until_gone(pid) if pid else False
    print(f'postmaster {pid} on {port} gone: {killed}', file=sys.stderr, flush=True)
    if mode == 'sk-noreact':
        if run(cli['test'], must=False).returncode != 0:
            finish(ok=False)  # gives up: no change made, services left as they are
    else:
        # React: start what is down again (both arms' up resumes a partial start), retrying a few
        # times as an agent would while the killed server's children finish exiting.
        for attempt in range(4):
            if run(cli['up'], must=attempt == 3).returncode == 0:
                break
            time.sleep(3)
    apply_reference()
    run(cli['migrate'])
    run(cli['restart'])
    run(cli['test'])
    commit()
    if scratch:
        run([os.path.join(scratch[0], 'pg_ctl'), '-D', str(scratch[1]), '-m', 'fast', '-w', 'stop'])
        for proc in SCRATCH_PROCS:   # its own scratch Redis and API, by pid
            run(['kill', str(proc.pid)], must=False)
            proc.wait(10)
    run(cli['down'])
    finish()

if ROLE == 'recovery':
    run(['pkill', '-f', TOOL_TAG], must=False)   # stop what the previous agent left running
if mode == 'reference-norole':
    norole_client()
    mode = 'reference'

if mode == 'sentinel':
    plant_sentinel()
if mode == 'marked-reader':
    # A process carrying this worktree's marker that follows a sibling's file, working directory
    # outside every worktree: attributed to this worktree only, never to the sibling.
    if not re.fullmatch(r'wt-[a-z]+', argument):
        finish(ok=False)
    # Not /usr/bin/tail: the environment of a platform binary cannot be read, so its marker would not show.
    run([sys.executable, '-c', 'import sys, time; f = open(sys.argv[1]); time.sleep(3600)', str(root / argument / 'README.md')],
        background=True, cwd='/')
apply_reference()
run(cli['up'])
run(cli['migrate'])
run(cli['restart'])
if mode in ('self-kill', 'up-target'):
    time.sleep(12)  # no down or restart for 12 s before the loss
    SYNC.mkdir(exist_ok=True)
    if mode == 'up-target':
        run(cli['up'])   # an up just before the sibling's kill: it does not stop a running service
    (SYNC / f'{here.name}.ready').write_text('ready\n')
    if mode == 'self-kill':
        wait_for(SYNC / f'{here.name}.go', 120)
        pid = worker_pid(here)
        if pid:
            os.kill(pid, 9)   # its own worker, lost with no command in the trace
    else:
        wait_for(SYNC / f'{here.name}.killed', 120)
    time.sleep(3)
    run(cli['restart'])
if mode == 'target':
    time.sleep(12)  # no stop or restart command for 12 s before the sibling's kill
    SYNC.mkdir(exist_ok=True)
    (SYNC / f'{here.name}.ready').write_text('ready\n')
    wait_for(SYNC / f'{here.name}.killed', 120)
    time.sleep(3)
    run(cli['restart'])
run(cli['test'])
commit()
if mode == 'contaminate':
    main = root / 'main'
    env_text = run(cli['env'], cwd=main).stdout
    main_env = dict(line.split('=', 1) for line in env_text.splitlines() if line.startswith(('DATABASE_URL=', 'REDIS_URL=')))
    # Bypass both arms' wrappers so the planted fault really happens: the grader's interpreter
    # runs this branch's migrate.py against the main checkout's database.
    grader_py = str(Path.home() / '.cache/stack-agentvalue/grader/venv/bin/python')
    run([grader_py, 'migrate.py'], env={**os.environ, **main_env})
if mode == 'pkill':
    # A careless pkill in form, but its pattern names only this run's main checkout, so validation
    # can never touch another program on the host. Killing the python child ends its `uv run` parent.
    main_venv = os.path.realpath(root / 'main') + '/.venv/bin/python worker.py'
    run(['pkill', '-f', re.escape(main_venv)])
if mode == 'killer':
    if not re.fullmatch(r'wt-[a-z]+', argument):
        finish(ok=False)
    wait_for(SYNC / f'{argument}.ready', 300)
    sibling = os.path.realpath(root / argument) + '/.venv/bin/python worker.py'
    run(['pkill', '-f', re.escape(sibling)], must=False)
    SYNC.mkdir(exist_ok=True)
    (SYNC / f'{argument}.killed').write_text('killed\n')
if mode == 'wrong-pid-killer':
    # Kills a process of its own (by pid) at the moment a sibling's worker is lost for another reason.
    if not re.fullmatch(r'wt-[a-z]+', argument):
        finish(ok=False)
    mine = run(SLEEPER, background=True, cwd=here)
    wait_for(SYNC / f'{argument}.ready', 300)
    sibling_pid = worker_pid(root / argument)   # looked up before the sibling is told to go
    (SYNC / f'{argument}.go').write_text('go\n')
    # The sibling's worker pid appears in the same shell text, but only the own process is killed.
    run(['sh', '-c', f'echo {sibling_pid}; kill -9 {mine.pid}'], must=False)
    time.sleep(2)
if mode == 'pid-killer':
    # Kills a sibling's worker by its pid.
    if not re.fullmatch(r'wt-[a-z]+', argument):
        finish(ok=False)
    wait_for(SYNC / f'{argument}.ready', 300)
    pid = worker_pid(root / argument)
    if pid:
        run(['kill', '-9', str(pid)], must=False)
    (SYNC / f'{argument}.killed').write_text('killed\n')
if mode == 'exec-left':
    run(cli['exec'] + SLEEPER, background=True, cwd=here)
    time.sleep(3)  # registered and running before down
    run(cli['down'], must=False)
    finish()
if mode == 'leave-running-idle':
    time.sleep(32)   # no down, restart or kill in the last 30 s before it exits (teardown collateral target)
if mode not in ('leave-running', 'leave-running-idle'):
    run(cli['down'])
if mode == 'orphan':
    # A detached command started with a clean environment (no marker), its working directory in the
    # worktree, reparented at once (its shell exits immediately, so no sample sees its parent): only
    # its placement ties it to the run.
    # Its own session (not stopped with the agent's process group); a single backgrounded command, so
    # the shell exits at once and no shell carrying the marker stays as its parent.
    run(['sh', '-c', f'env -i {shlex.quote(sys.executable)} -m http.server --bind 127.0.0.1 0 >/dev/null 2>&1 &'],
        background=True, cwd=here)
    time.sleep(1)
if mode == 'stray':
    # Started without the arm's tooling, in the worktree, and left running.
    run(SLEEPER, background=True, cwd=here)
    time.sleep(1)
finish()
