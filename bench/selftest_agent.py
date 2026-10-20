#!/usr/bin/env python3
"""Scripted stand-in for an agent, used only by `harness_selftest` plans.

It copies the reference solution and drives the project's documented commands, so the
trial path (prompt, receipts, residue detection, cleanup, grading, report) can be tested
end to end without a model. Its results are never agent results.

Modes:
  reference                 apply the reference and leave services stopped
  reference-commit          the same, then commit the change in the agent's checkout
  reference-leave-running   apply the reference and leave the services running
  reference-break-health    ledger: the reference, but GET /health fails (the smoke test catches it)
  broken-start              the app exits at startup
  noop                      change nothing
"""
import argparse
import json
from pathlib import Path
import shutil
import subprocess
import sys

HERE = Path(__file__).resolve().parent
MODES = ['reference', 'reference-commit', 'reference-leave-running', 'reference-break-health', 'broken-start', 'noop']
BROKEN = {'ledger': ('app.py', 'raise SystemExit("broken on purpose by the self-test")\n'),
          'catalog': ('server.mjs', 'throw new Error("broken on purpose by the self-test");\n'),
          'slug': ('slug.mjs', 'throw new Error("broken on purpose by the self-test");\n')}


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--mode', choices=MODES, required=True)
    parser.add_argument('--model', required=True)
    parser.add_argument('--prompt-file', required=True)
    parser.add_argument('--transcript-dir', required=True)
    parser.add_argument('--option', action='append', default=[], metavar='KEY=VALUE',
                        help='a model option, recorded in the transcript as the configuration in effect')
    args = parser.parse_args()
    workspace = Path.cwd()
    project = 'ledger' if (workspace / 'app.py').exists() else 'catalog' if (workspace / 'server.mjs').exists() else 'slug'
    log = []
    if args.mode == 'broken-start':
        name, text = BROKEN[project]
        (workspace / name).write_text(text)
    elif args.mode != 'noop':
        for path in (HERE / 'oracles' / project).iterdir():
            shutil.copy2(path, workspace / path.name)
        if args.mode == 'reference-break-health':
            app = workspace / 'app.py'
            app.write_text(app.read_text().replace("if self.path=='/health': return self.reply({'ok':True})",
                                                   "if self.path=='/health': return self.reply({'ok':False}, 500)"))
        if (workspace / 'stack.toml').exists():
            steps = ([['stack', 'up']] if project != 'slug' else []) + [['stack', 'run', 'test']]
            down = [['stack', 'down']] if project != 'slug' else []
        else:
            steps = [[sys.executable, 'dev.py', 'setup']] + ([[sys.executable, 'dev.py', 'up']] if project != 'slug' else []) \
                + [[sys.executable, 'dev.py', 'test']]
            down = [[sys.executable, 'dev.py', 'down']] if project != 'slug' else []
        if args.mode != 'reference-leave-running':
            steps += down
        if args.mode == 'reference-commit':
            git = ['git', '-c', 'user.name=selftest', '-c', 'user.email=selftest@example.invalid', '-c', 'commit.gpgsign=false',
                   '-c', 'core.hooksPath=/dev/null']
            steps += [[*git, 'add', '-A'], [*git, 'commit', '-q', '-m', 'Apply the reference solution']]
        for argv in steps:
            result = subprocess.run(argv, cwd=workspace, capture_output=True, text=True)
            log.append({'argv': argv, 'exit_code': result.returncode, 'stdout': result.stdout[-1000:], 'stderr': result.stderr[-1000:]})
    options = dict(option.split('=', 1) for option in args.option)
    transcript = {'selftest': True, 'mode': args.mode, 'model': args.model, 'options_in_effect': options,
                  'prompt': Path(args.prompt_file).read_text(), 'steps': log}
    (Path(args.transcript_dir) / 'selftest.json').write_text(json.dumps(transcript, indent=2))
    print(json.dumps({'selftest': True, 'mode': args.mode, 'options_in_effect': options,
                      'steps': [[step['argv'], step['exit_code']] for step in log]}))


if __name__ == '__main__':
    main()
