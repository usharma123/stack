#!/usr/bin/env python3
"""Descriptive trace analysis (not scoring): which environment commands agents ran, and every tool error.

  python3 bench/agentvalue/harness/analyze_traces.py <results dir>...
"""
import collections
import json
from pathlib import Path
import re
import sys

ENV_CMD = re.compile(r'(?:^|[;&|(]\s*|\s)(stack(?:\s+--json)?\s+[a-z-]+|python3 dev\.py\s+[a-z-]+)')


def sessions(roots):
    for root in roots:
        for run in sorted(Path(root).glob('pair*-*/run.json')):
            record = json.loads(run.read_text())
            if not record.get('valid'):
                continue
            for task in record['agents']:
                trace = run.parent / 'agents' / task / 'trace.jsonl'
                if trace.exists():
                    yield run.parent.name, record['arm'], task, trace


def walk(trace):
    calls, errors, pending = [], [], {}
    for line in trace.read_text(errors='replace').splitlines():
        try:
            event = json.loads(line)
        except ValueError:
            continue
        for block in (event.get('message') or {}).get('content') or []:
            if not isinstance(block, dict):
                continue
            if block.get('type') == 'tool_use':
                command = (block.get('input') or {}).get('command') or (block.get('input') or {}).get('file_path') or ''
                pending[block.get('id')] = command
                calls.append(command)
            elif block.get('type') == 'tool_result' and block.get('is_error'):
                text = block.get('content')
                text = text if isinstance(text, str) else json.dumps(text)
                errors.append({'command': pending.get(block.get('tool_use_id'), '')[:300], 'error': text[:400]})
    return calls, errors


def main():
    per_arm = collections.defaultdict(collections.Counter)
    first_env = collections.defaultdict(list)
    out = {'sessions': []}
    for run, arm, task, trace in sessions(sys.argv[1:]):
        calls, errors = walk(trace)
        env_cmds = [m.group(1).replace('--json ', '') for c in calls for m in ENV_CMD.finditer(c)]
        per_arm[arm].update(env_cmds)
        first_env[arm].append(next((i for i, c in enumerate(calls) if ENV_CMD.search(c)), None))
        out['sessions'].append({'run': run, 'arm': arm, 'task': task, 'tool_calls': len(calls), 'env_commands': env_cmds, 'errors': errors})
    out['env_command_counts'] = {arm: dict(c.most_common()) for arm, c in per_arm.items()}
    out['first_env_call_index'] = dict(first_env)
    print(json.dumps(out, indent=1))


if __name__ == '__main__':
    main()
