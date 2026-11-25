import { test } from 'node:test';
import assert from 'node:assert/strict';
import { existsSync, mkdirSync, mkdtempSync, writeFileSync, readFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { execFileSync, spawnSync } from 'node:child_process';

const overrides = ['STACK_STATE_DIR', 'STACK_CACHE_DIR', 'PITCHFORK_STATE_DIR', 'PITCHFORK_CONFIG_DIR',
  'PITCHFORK_LOGS_DIR', 'MISE_CONFIG_FILE', 'MISE_DATA_DIR', 'MISE_STATE_DIR', 'MISE_CACHE_DIR', 'MISE_CONFIG_DIR',
  'MISE_GLOBAL_CONFIG_FILE', 'MISE_SYSTEM_CONFIG_FILE', 'MISE_ENV', 'MISE_OVERRIDE_CONFIG_FILENAMES', '__MISE_DIFF'];

test('pilot discards inherited provider state and configuration selectors', () => {
  execFileSync('python3', ['-c', `
import importlib.util, pathlib
spec = importlib.util.spec_from_file_location('pilot', 'eval/harness/pilot.py')
m = importlib.util.module_from_spec(spec); spec.loader.exec_module(m)
env = m.isolated_env(pathlib.Path('/tmp/isolated'), {**{k: '/sentinel' for k in ${JSON.stringify(overrides)}}, 'PATH': '/usr/bin'})
assert all('/sentinel' not in v for v in env.values())
assert env['PATH'] == '/usr/bin'
assert env['PITCHFORK_STATE_DIR'] == '/tmp/isolated/pf'
assert env['PITCHFORK_CONFIG_DIR'] == '/tmp/isolated/h/.config/pitchfork'
assert 'PITCHFORK_LOGS_DIR' not in env
assert 'MISE_CONFIG_FILE' not in env
for key in ['STACK_STATE_DIR','STACK_CACHE_DIR','MISE_DATA_DIR','MISE_CACHE_DIR','MISE_STATE_DIR']:
    assert env[key].startswith('/tmp/isolated/h/'), key
`]);
});

test('native runner isolates every override before any Stack or mise invocation', () => {
  const dir = mkdtempSync(path.join(tmpdir(), 'stack-harness-'));
  let work;
  try {
    const capture = path.join(dir, 'capture.json');
    // Capture only test-related paths, never credentials from the inherited environment.
    writeFileSync(path.join(dir, 'stack'), `#!/usr/bin/env python3\nimport os,json\njson.dump({k:os.environ.get(k) for k in ${JSON.stringify([...overrides, 'STACK_E2E_WORK'])}},open(${JSON.stringify(capture)},'w'))\nprint('stack fixture')\n`, { mode: 0o755 });
    writeFileSync(path.join(dir, 'mise'), '#!/bin/sh\nprintf "mise fixture\\n"\n', { mode: 0o755 });
    writeFileSync(path.join(dir, 'jq'), '#!/bin/sh\nexit 0\n', { mode: 0o755 });
    const result = spawnSync('bash', ['tests/e2e/native.sh', path.join(dir, 'stack'), '__isolation_probe_missing'], {
      encoding: 'utf8', env: { ...process.env, ...Object.fromEntries(overrides.map(k => [k, '/sentinel'])), PATH: `${dir}:${process.env.PATH}` }
    });
    // No service scenario runs; the intentionally missing name ends after the version probes.
    assert.notEqual(result.status, 0);
    const env = JSON.parse(readFileSync(capture));
    work = path.dirname(env.STACK_E2E_WORK);
    for (const key of overrides) assert.ok(env[key] === null || env[key].startsWith(`${work}/`), `${key}: ${env[key]}`);
    assert.equal(env.MISE_CONFIG_FILE, null);
    assert.equal(env.PITCHFORK_STATE_DIR, `${work}/pf`);
    assert.equal(env.PITCHFORK_CONFIG_DIR, `${work}/h/.config/pitchfork`);
    assert.equal(env.PITCHFORK_LOGS_DIR, null);
  } finally {
    if (work) rmSync(work, { recursive: true, force: true });
    rmSync(dir, { recursive: true, force: true });
  }
});

// A scratch e2e environment: STACK_E2E_* naming fresh directories, with `stack` on PATH being the
// shell script given (run with the arguments it was called with).
function e2eFixture(stackScript) {
  const dir = mkdtempSync(path.join(tmpdir(), 'stack-e2e-bound-'));
  for (const sub of ['bin', 'w', 't']) mkdirSync(path.join(dir, sub));
  writeFileSync(path.join(dir, 'bin', 'stack'), `#!/usr/bin/env bash\n${stackScript}\n`, { mode: 0o755 });
  const env = {
    ...process.env,
    STACK_E2E_BIN: path.join(dir, 'bin'), STACK_E2E_WORK: path.join(dir, 'w'), STACK_E2E_TMP: path.join(dir, 't'),
    STACK_E2E_ASSERT: path.resolve('tests/e2e/assert.sh'), STACK_E2E_WAIT_SECONDS: '2',
  };
  return { dir, env };
}

// Whether every PID listed in the file is gone.
function allGone(file) {
  const pids = existsSync(file) ? readFileSync(file, 'utf8').split('\n').filter(Boolean).map(Number) : [];
  return pids.length > 0 && pids.every(pid => { try { process.kill(pid, 0); return false; } catch { return true; } });
}

test('within and eventually stop a stalled client, show the diagnosis and fail within their bound', () => {
  const { dir, env } = e2eFixture('exit 0');
  try {
    const pids = path.join(dir, 'stalled.pids');
    // A client that ignores both its input's end and TERM.
    const stalled = `bash -c 'trap "" TERM; echo $$ >>${pids}; while :; do sleep 0.2; done'`;
    const probe = (body) => spawnSync('bash', ['-c', `source "$STACK_E2E_ASSERT"; diagnose() { echo DIAGNOSIS; }; ${body}`],
      { encoding: 'utf8', env, timeout: 30000 });
    const started = Date.now();
    let result = probe(`within 1 ${stalled} </dev/null; echo unreachable`);
    assert.equal(result.status, 1, result.stderr);
    assert.match(result.stderr, /DIAGNOSIS[\s\S]*FAIL: still running after 1s, stopped: bash -c/);
    assert.doesNotMatch(result.stdout, /unreachable/);
    assert.ok(Date.now() - started < 10000, 'within outlived its bound');
    assert.ok(allGone(pids), 'the stalled client survived');

    result = probe(`eventually 1 'the file appeared' test -e ${path.join(dir, 'never')}`);
    assert.equal(result.status, 1);
    assert.match(result.stderr, /DIAGNOSIS[\s\S]*FAIL: the file appeared \(not within 1s\)/);

    // Within its bound a command keeps its input, output and status.
    result = probe(`out=$(echo in | within 5 cat); [[ $out == in ]] || exit 9; within 5 sh -c 'exit 3' || echo "status $?"`);
    assert.equal(result.status, 0, result.stderr);
    assert.equal(result.stdout, 'status 3\n');

    // A refusal run under `within` is checked quietly: no ERR-trap report from its failing return.
    result = probe(`expect_error usage within 5 sh -c 'echo "{\\"ok\\":false,\\"error\\":{\\"code\\":\\"usage\\"}}"; exit 2'`);
    assert.equal(result.status, 0, result.stderr);
    assert.equal(result.stderr, '');

    // Whatever way a scenario ends, what it tracked is stopped, and its exit status kept.
    const tracked = path.join(dir, 'tracked.pids');
    result = probe(`${stalled.replace(pids, tracked)} & track $!; track_pids ${tracked}; sleep 0.5; exit 7`);
    assert.equal(result.status, 7);
    assert.ok(allGone(tracked), 'a tracked process outlived the scenario');
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test('scenario 15 stops an MCP server that never finishes after its input ends, and reports it', () => {
  // compile and install succeed; mcp ignores its input's end and TERM.
  const { dir, env } = e2eFixture(`case "$1" in mcp) trap '' TERM; echo $$ >>"$STACK_E2E_TMP/fake.pids"; while :; do sleep 0.2; done ;; esac`);
  try {
    const started = Date.now();
    const result = spawnSync('bash', ['tests/e2e/15-mcp-eof.sh'], { encoding: 'utf8', env: { ...env, STACK_E2E_MCP_SECONDS: '2' }, timeout: 60000 });
    assert.equal(result.status, 1, result.stderr);
    assert.match(result.stderr, /--- .*mcp-order[\s\S]*FAIL: still running after 2s, stopped: stack mcp/);
    assert.ok(Date.now() - started < 30000, 'the scenario outlived its bound');
    assert.ok(allGone(path.join(dir, 't', 'fake.pids')), 'the stalled server survived the scenario');
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test('scenario 14 stops a stalled run call and reports it', () => {
  // compile and up succeed with one ready service; run ignores TERM and never ends.
  const { dir, env } = e2eFixture(`case "$1" in
  up) mkdir -p .stack; printf '{"provider":{"state_dir":"%s"}}' "$PWD/.stack" >.stack/session.json
      echo '{"ok":true,"data":{"checks":[{"service":"talk","ready":true,"port":9}]}}' ;;
  run) trap '' TERM; echo $$ >>"$STACK_E2E_TMP/fake.pids"; while :; do sleep 0.2; done ;;
esac`);
  try {
    const started = Date.now();
    const result = spawnSync('bash', ['tests/e2e/14-run-logs-cursor.sh'], { encoding: 'utf8', env: { ...env, STACK_E2E_CALL_SECONDS: '2' }, timeout: 60000 });
    assert.equal(result.status, 1, result.stderr);
    assert.match(result.stderr, /FAIL: still running after 2s, stopped: stack run args --timeout 3 --json -q x/);
    assert.ok(Date.now() - started < 30000, 'the scenario outlived its bound');
    assert.ok(allGone(path.join(dir, 't', 'fake.pids')), 'the stalled call survived the scenario');
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});
