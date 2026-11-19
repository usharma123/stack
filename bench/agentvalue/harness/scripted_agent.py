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


def plant_sentinel():
    """A credential-shaped value in command output, in a JSON object of the trace, and in a file of
    the submitted tree. Validation checks that it reaches no retained file."""
    value = sentinel_value()
    run(['sh', '-c', f'echo SENTINEL_TOKEN={value}'])
    emit({'type': 'system', 'subtype': 'note', 'data': {'SENTINEL_TOKEN': value}})
    (here / 'notes.env').write_text(f'SENTINEL_TOKEN={value}\n')

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
    run(cli['down'])
    finish()

if ROLE == 'recovery':
    run(['pkill', '-f', TOOL_TAG], must=False)   # stop what the previous agent left running
if mode == 'sentinel':
    plant_sentinel()
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
    (SYNC / f'{argument}.go').write_text('go\n')
    run(['kill', '-9', str(mine.pid)], must=False)
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
if mode not in ('leave-running',):
    run(cli['down'])
if mode == 'stray':
    # Started without the arm's tooling, in the worktree, and left running.
    run(SLEEPER, background=True, cwd=here)
    time.sleep(1)
finish()
