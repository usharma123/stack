#!/usr/bin/env python3
"""Scripted stand-in for an agent, used only by `harness_selftest` plans.

It copies the reference solution and drives the project's documented commands, so the
trial path (prompt, receipts, residue detection, cleanup, grading, report) can be tested
end to end without a model. Its results are never agent results.
"""
import argparse
import json
from pathlib import Path
import shutil
import subprocess
import sys

HERE = Path(__file__).resolve().parent


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--mode', choices=['reference', 'reference-leave-running', 'noop'], required=True)
    parser.add_argument('--model', required=True)
    parser.add_argument('--prompt-file', required=True)
    parser.add_argument('--transcript-dir', required=True)
    args = parser.parse_args()
    workspace = Path.cwd()
    project = 'ledger' if (workspace / 'app.py').exists() else 'catalog' if (workspace / 'server.mjs').exists() else 'slug'
    log = []
    if args.mode != 'noop':
        for path in (HERE / 'oracles' / project).iterdir():
            shutil.copy2(path, workspace / path.name)
        if (workspace / 'stack.toml').exists():
            steps = ([['stack', 'up']] if project != 'slug' else []) + [['stack', 'run', 'test']]
            down = [['stack', 'down']] if project != 'slug' else []
        else:
            steps = [[sys.executable, 'dev.py', 'setup']] + ([[sys.executable, 'dev.py', 'up']] if project != 'slug' else []) \
                + [[sys.executable, 'dev.py', 'test']]
            down = [[sys.executable, 'dev.py', 'down']] if project != 'slug' else []
        if args.mode == 'reference':
            steps += down
        for argv in steps:
            result = subprocess.run(argv, cwd=workspace, capture_output=True, text=True)
            log.append({'argv': argv, 'exit_code': result.returncode, 'stdout': result.stdout[-1000:], 'stderr': result.stderr[-1000:]})
    transcript = {'selftest': True, 'mode': args.mode, 'model': args.model,
                  'prompt': Path(args.prompt_file).read_text(), 'steps': log}
    (Path(args.transcript_dir) / 'selftest.json').write_text(json.dumps(transcript, indent=2))
    print(json.dumps({'selftest': True, 'mode': args.mode, 'steps': [[step['argv'], step['exit_code']] for step in log]}))


if __name__ == '__main__':
    main()
