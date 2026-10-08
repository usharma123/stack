import { test } from 'node:test';
import assert from 'node:assert/strict';
import { mkdtempSync, readFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { execFileSync } from 'node:child_process';

// The first fenced block of `lang` under a `## heading` in a docs/user page.
function block(page, heading, lang) {
  const text = readFileSync(path.join('docs/user', page), 'utf8');
  const section = text.split(`\n## ${heading}\n`)[1]?.split('\n## ')[0];
  const code = section?.match(new RegExp('```' + lang + '\\n([\\s\\S]*?)```'))?.[1];
  assert.ok(code, `${page}: no ${lang} block under "${heading}"`);
  return code;
}

test('agents.md result check passes only commands and services that succeeded', () => {
  const ran = (code) => ({ ok: true, data: { exit_code: code, timed_out: false, stdout: '', stderr: '', unverified: [], checks: [] } });
  const cases = [
    ['exec', ran(0), true],
    ['stack_exec', ran(7), false],
    ['run', ran(null), false],
    ['stack_run', { ok: false, error: { code: 'timed_out', message: '', details: [{ ...ran(null).data, timed_out: true }] } }, false],
    ['status', { ok: true, data: { healthy: false, stale: false, checks: [] } }, false],
    ['stack_status', { ok: true, data: { healthy: true, stale: false, checks: [] } }, true],
    ['up', { ok: false, error: { code: 'port_conflict', message: '' } }, false],
    ['down', { ok: true, data: { stopped: [], confirmed: true } }, true],
  ];
  execFileSync('python3', ['-I', '-c', `${block('agents.md', 'Reading results', 'python')}
import json, sys
for operation, envelope, expected in json.loads(sys.argv[1]):
    assert succeeded(operation, envelope) is expected, (operation, envelope)
`, JSON.stringify(cases)]);
});

test('worktrees.md isolation recipe keeps HOME and moves every store under a short root', () => {
  const home = mkdtempSync(path.join(tmpdir(), 'stack-docs-home-'));
  let iso;
  try {
    const inherited = ['MISE_DATA_DIR', 'MISE_ENV', '__MISE_DIFF', 'PITCHFORK_CONFIG_DIR', 'PITCHFORK_STATE_DIR',
      'PITCHFORK_LOGS_DIR', 'STACK_STATE_DIR', 'STACK_DATA_DIR', 'STACK_CACHE_DIR'];
    const out = execFileSync('bash', ['-c', `set -eu\n${block('worktrees.md', 'Isolated runs', 'sh')}\nnode -e 'console.log(JSON.stringify({ iso: process.argv[1], env: process.env }))' "$iso"`], {
      encoding: 'utf8',
      env: { PATH: process.env.PATH, HOME: home, ...Object.fromEntries(inherited.map(k => [k, '/sentinel'])) },
    });
    const { iso: root, env } = JSON.parse(out);
    iso = root;
    assert.equal(env.HOME, home);
    assert.ok(Object.values(env).every(v => !v.includes('/sentinel')));
    assert.ok(!('MISE_ENV' in env) && !('__MISE_DIFF' in env));
    for (const key of ['STACK_STATE_DIR', 'STACK_DATA_DIR', 'STACK_CACHE_DIR', 'MISE_DATA_DIR', 'MISE_CACHE_DIR',
      'MISE_STATE_DIR', 'MISE_CONFIG_DIR', 'MISE_GLOBAL_CONFIG_FILE', 'PITCHFORK_CONFIG_DIR', 'PITCHFORK_STATE_DIR',
      'PITCHFORK_LOGS_DIR', 'XDG_CONFIG_HOME', 'XDG_DATA_HOME', 'XDG_CACHE_HOME', 'XDG_STATE_HOME', 'UV_CACHE_DIR',
      'npm_config_cache', 'npm_config_userconfig', 'npm_config_prefix']) {
      assert.ok(env[key]?.startsWith(`${iso}/`), `${key}: ${env[key]}`);
    }
    // Pitchfork's socket is $PITCHFORK_STATE_DIR/sock/main.sock; macOS sun_path holds 104 bytes.
    assert.ok(Buffer.byteLength(`/private${env.PITCHFORK_STATE_DIR}/sock/main.sock`) < 104);
    // With HOME's provider files absent, the receipt records them as absent rather than skipping them.
    assert.match(readFileSync(path.join(iso, 'global-before'), 'utf8'), new RegExp(`^absent ${home}/\\.config/pitchfork/config\\.toml$`, 'm'));
  } finally {
    if (iso) rmSync(iso, { recursive: true, force: true });
    rmSync(home, { recursive: true, force: true });
  }
});
