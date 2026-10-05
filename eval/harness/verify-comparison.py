#!/usr/bin/env python3
"""Check report claims against saved receipts, without rerunning workloads."""
import json
from pathlib import Path
import sys

base=Path(sys.argv[1]).resolve()
def j(path): return json.loads((base/path).read_text())
def text(path): return (base/path).read_text().strip()
def require(condition, context):
    if not condition:
        raise ValueError(context)

def named_rows(rows, key, context, expected=None):
    require(isinstance(rows, list), context + ': expected a list')
    result = {}
    for row in rows:
        require(isinstance(row, dict), context + ': expected an object')
        name = row.get(key)
        require(isinstance(name, str) and bool(name), context + ': missing ' + key)
        require(name not in result, context + ': duplicate ' + name)
        result[name] = row
    if expected is not None:
        require(set(result) == set(expected),
                context + ': unexpected or missing names: ' + str(set(result) ^ set(expected)))
    return result

def steps(tool):
    rows = named_rows(j(tool + '/steps.json'), 'step', tool)
    for name, row in rows.items():
        require(type(row.get('code')) is int, tool + '/' + name + ': invalid exit code')
    return rows

# Fixed names come from compose-benchmark.py and pixi-benchmark.py. Missing
# lifecycle receipts must not turn all(existing rows) into a successful run.
COMPOSE_STEPS = (
    ['version', 'pull']
    + [f'{action}-{i}' for i in range(2) for action in ('up', 'set-pg', 'set-redis')]
    + [f'get-{service}-{i}' for i in range(2) for service in ('pg', 'redis')]
    + [f'exec-{i}' for i in range(15)]
    + ['up-again', 'down-A', 'B-survives', 'state', 'cleanup-0', 'cleanup-1']
)
PIXI_STEPS = (
    ['version']
    + [f'{app}-{action}' for app in ('appA', 'appB')
       for action in ('prepare', 'install', 'start', 'tests', 'identity')]
    + ['resolved-versions'] + [f'exec-{i}' for i in range(15)]
    + ['stop-A', 'B-survives', 'stop-B', 'lock']
)
SCENARIOS = [
    '1-independent-checkouts', '2-wrong-instance', '3-leases', '4-mcp',
    '5-oci', '6-configuration-generation', '7-deleted-project', '8-identity-probe',
]
CONTRACTS = {
    'invalid-subcommand': (2, 'usage'),
    'impossible-version': (1, 'resolve_failed'),
    'unknown-tool': (1, 'resolve_failed'),
    'conflicting-bundles': (1, 'conflict'),
}

def successful_steps(tool, expected):
    rows = steps(tool)
    require(set(rows) == set(expected), tool + ': unexpected or missing steps: '
            + str(set(rows) ^ set(expected)))
    for name, row in rows.items():
        require(row['code'] == 0, tool + '/' + name + ': unsuccessful step')

def identity(tool,app): return text(tool+'/identity-'+app+'.stdout').splitlines()[-1]
checked=[]
for tool in ['stack','mise-configured','flox-configured','devbox-configured','devenv-configured','devenv-latest-dynamic']:
    r=steps(tool)
    assert identity(tool,'A')!=identity(tool,'B'),tool
    for case in ['readiness','tests','tests-B','B-after-A-stop','stop-A','stop-B']:
        assert r[case]['code']==0,(tool,case)
    checked.append(tool+' separate database and independent shutdown')
for tool in ['mise','flox','devbox','devenv','devenv-latest']:
    r=steps(tool)
    assert identity(tool,'A')==identity(tool,'B'),tool
    assert r['tests']['code']==r['tests-B']['code']==0,tool
    assert r['B-after-A-stop']['code']!=0,tool
    checked.append(tool+' original recipe reached A from B')
assert text('stack/post-stop-env.stdout').count('unverified.stack.invalid')==2
for case in ['start','start-B']:
    a=j('stack/'+case+'.stdout')
    assert a['ok'] and all(c['ready'] and c['identity']=='instance' for c in a['data']['checks'])
for i in [0,1]:
    assert text('compose/get-pg-'+str(i)+'.stdout')==text('compose/get-redis-'+str(i)+'.stdout')
assert text('compose/get-pg-0.stdout')!=text('compose/get-pg-1.stdout')
assert text('compose/B-survives.stdout')==text('compose/get-pg-1.stdout')
successful_steps('compose', COMPOSE_STEPS)
assert text('pixi/appA-identity.stdout')!=text('pixi/appB-identity.stdout')
successful_steps('pixi', PIXI_STEPS)
checked.extend(['Compose distinct PostgreSQL/Redis markers and independent shutdown','Pixi explicit service configuration and independent shutdown'])
linux=named_rows(j('release-e2e/scenarios.json'), 'scenario', 'Linux scenarios', SCENARIOS)
for name, row in linux.items():
    require(type(row.get('code')) is int and row['code'] == 0, 'Linux scenario failed: ' + name)
native=named_rows([json.loads(line) for line in text('native-scenarios.jsonl').splitlines()],
                  'scenario', 'native scenarios', SCENARIOS)
for name, row in native.items():
    require(row.get('result') == 'passed', 'native scenario failed: ' + name)
assert j('git-lock/summary.json')['ok']
contracts=named_rows(j('contract/summary.json'), 'case', 'CLI contracts', CONTRACTS)
for name, (exit_code, error_code) in CONTRACTS.items():
    row = contracts[name]
    require(type(row.get('exit_code')) is int and row['exit_code'] == exit_code,
            name + ': incorrect contract exit code')
    require(row.get('error_code') == error_code, name + ': incorrect contract error code')
    require(row.get('single_json_object') is True, name + ': invalid single JSON object flag')
    # json.loads rejects extra JSON values and non-JSON prefixes/suffixes. Check
    # the saved output too, rather than trusting the summary's schema flag.
    body = j('contract/' + name + '.stdout')
    require(isinstance(body, dict) and body.get('ok') is False,
            name + ': expected a JSON failure object')
    error = body.get('error')
    require(isinstance(error, dict) and error.get('code') == error_code,
            name + ': incorrect JSON error code')
    require(isinstance(error.get('message'), str) and bool(error['message'].strip()),
            name + ': missing JSON error message')
pilot=j('pilot/summary.json')
assert pilot['projects_concurrent']==4 and pilot['rounds_per_phase']==2
assert set(pilot['phases'])=={'fresh','cached'}
for name,p in pilot['phases'].items():
    assert p['tasks_succeeded']==p['tasks_attempted']==8,(name,p)
    assert p['wrong_instance_incidents']==p['orphaned_service_processes']==0,(name,p)
    assert p['controlled_failures']['handled']==p['controlled_failures']['injected'],(name,p)
checked.extend(['8 Linux published-binary scenarios','8 native macOS published-binary scenarios','Moved tag with fresh bundle cache','4 CLI failure contracts','16 scripted tasks, no wrong instances or orphans, all injected failures handled'])
# Read-only verification: keep existing saved receipts unchanged.
print(json.dumps({'ok':True,'claims_checked':checked},indent=2))
