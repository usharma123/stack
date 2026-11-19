#!/usr/bin/env python3
"""Scripted stand-in for an agent, for validating the harness only. Never a result.

  reference      apply the reference solution, follow the README: up, migrate, test, commit, down
  leave-running  as reference, but never stop the services (planted leftover)
  contaminate    as reference, then run this branch's migrations against the MAIN checkout's
                 database, as an agent does that took the wrong DATABASE_URL (planted contamination)
  pkill          as reference, then restart the worker the careless way: `pkill -f worker.py`,
                 which also kills the main checkout's worker (planted collateral kill)
  noop           do nothing
"""
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys

ORACLES = Path(__file__).resolve().parent.parent / 'oracles'
mode, task, arm = sys.argv[1], os.environ['AGENTVALUE_TASK'], os.environ['AGENTVALUE_ARM']
here = Path.cwd()


def run(argv, **kw):
    """Run a command and record it in the trace the way the real agent CLI does (stream-json)."""
    print(json.dumps({'type': 'assistant', 'message': {'content': [{'type': 'tool_use', 'id': str(len(argv)), 'name': 'Bash',
                                                                    'input': {'command': ' '.join(argv)}}]}}), flush=True)
    r = subprocess.run(argv, capture_output=True, text=True, **kw)
    print(r.stdout[-2000:], r.stderr[-2000:], file=sys.stderr, flush=True)
    return r


cli = {'stack': {'up': ['stack', 'up', '--ttl', '1h'], 'test': ['stack', 'run', 'test'], 'down': ['stack', 'down'],
                 'migrate': ['stack', 'run', 'migrate'], 'restart': ['stack', 'restart', 'api', 'worker'],
                 'env': ['stack', 'exec', '--', 'env']},
       'baseline': {'up': ['python3', 'dev.py', 'up'], 'test': ['python3', 'dev.py', 'test'], 'down': ['python3', 'dev.py', 'down'],
                    'migrate': ['python3', 'dev.py', 'migrate'], 'restart': ['python3', 'dev.py', 'restart', 'api', 'worker'],
                    'env': ['python3', 'dev.py', 'exec', 'env']}}[arm]

if mode == 'noop':
    sys.exit(0)
shutil.copytree(ORACLES / task, here, dirs_exist_ok=True)
run(cli['up'])
run(cli['migrate'])
run(cli['restart'])
run(cli['test'])
run(['git', 'add', '-A'])
run(['git', '-c', 'user.name=agent', '-c', 'user.email=agent@example.invalid', 'commit', '-qm', f'{task}: reference solution'])
if mode == 'contaminate':
    main = here.parent / 'main'
    env_text = run(cli['env'], cwd=main).stdout
    main_env = dict(line.split('=', 1) for line in env_text.splitlines() if line.startswith(('DATABASE_URL=', 'REDIS_URL=')))
    # Bypass both arms' wrappers so the planted fault really happens: the grader's interpreter
    # runs this branch's migrate.py against the main checkout's database.
    grader_py = str(Path.home() / '.cache/stack-agentvalue/grader/venv/bin/python')
    run([grader_py, 'migrate.py'], env={**os.environ, **main_env})
if mode == 'pkill':
    run(['pkill', '-f', 'python worker.py'])
if mode != 'leave-running':
    run(cli['down'])
print(json.dumps({'type': 'result', 'subtype': 'success', 'scripted': mode}))
