import { test } from 'node:test';
import assert from 'node:assert/strict';
import { mkdtempSync, writeFileSync, readFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { execFileSync, spawnSync } from 'node:child_process';

const overrides = ['STACK_STATE_DIR', 'STACK_CACHE_DIR', 'PITCHFORK_STATE_DIR', 'MISE_CONFIG_FILE',
  'MISE_DATA_DIR', 'MISE_STATE_DIR', 'MISE_CACHE_DIR', 'MISE_CONFIG_DIR', 'MISE_GLOBAL_CONFIG_FILE',
  'MISE_SYSTEM_CONFIG_FILE', 'MISE_ENV', 'MISE_OVERRIDE_CONFIG_FILENAMES', '__MISE_DIFF'];

test('pilot discards inherited provider state and configuration selectors', () => {
  execFileSync('python3', ['-c', `
import importlib.util, pathlib
spec = importlib.util.spec_from_file_location('pilot', 'eval/harness/pilot.py')
m = importlib.util.module_from_spec(spec); spec.loader.exec_module(m)
env = m.isolated_env(pathlib.Path('/tmp/isolated'), {**{k: '/sentinel' for k in ${JSON.stringify(overrides)}}, 'PATH': '/usr/bin'})
assert all('/sentinel' not in v for v in env.values())
assert env['PATH'] == '/usr/bin'
assert env['PITCHFORK_STATE_DIR'] == '/tmp/isolated/pf'
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
  } finally {
    if (work) rmSync(work, { recursive: true, force: true });
    rmSync(dir, { recursive: true, force: true });
  }
});
