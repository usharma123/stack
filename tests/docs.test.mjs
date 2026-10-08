import { test } from 'node:test';
import assert from 'node:assert/strict';
import { chmodSync, existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { execFileSync, spawnSync } from 'node:child_process';

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

// Runs the worktrees.md script under a temporary HOME, with stand-ins for npm and stack. The
// stand-in stack logs each call, fails a call that starts with an entry in `fail` and reports
// `supervisor` as the supervisor's status. `command` receives the temporary directory and
// returns argv.
function isolatedRun({ fail = [], supervisor = 'down', command = () => ['true'], prepare = () => {}, env = {} }, check) {
  const dir = mkdtempSync(path.join(tmpdir(), 'stack-docs-'));
  try {
    const template = '/tmp/stack-iso.XXXXXX';
    const script = block('worktrees.md', 'Isolated runs', 'bash');
    assert.ok(script.includes(template));
    writeFileSync(path.join(dir, 'isolated-stack.sh'), script.replace(template, `${dir}/iso.XXXXXX`));
    for (const sub of ['bin', 'home', 'checkout']) mkdirSync(path.join(dir, sub));
    writeFileSync(path.join(dir, 'fail'), fail.map(f => `${f}\n`).join(''));
    writeFileSync(path.join(dir, 'supervisor'), supervisor);
    writeFileSync(path.join(dir, 'bin/stack'), `#!/bin/bash
echo "$*" >>"$DOCS_STUB_DIR/calls"
while IFS= read -r f; do case "$*" in "$f"|"$f "*) exit 1 ;; esac; done <"$DOCS_STUB_DIR/fail"
[ "$*" = "exec -- pitchfork supervisor status --json" ] && printf '{"status":"%s"}\\n' "$(cat "$DOCS_STUB_DIR/supervisor")"
exit 0
`, { mode: 0o755 });
    writeFileSync(path.join(dir, 'bin/npm'), '#!/bin/bash\necho "npm $*" >>"$DOCS_STUB_DIR/calls"\n', { mode: 0o755 });
    prepare(dir);
    // Its own process group, so a command can interrupt the script without reaching the test.
    const run = spawnSync('bash', [path.join(dir, 'isolated-stack.sh'), '0.1.18', path.join(dir, 'checkout'), ...command(dir)], {
      cwd: dir,
      encoding: 'utf8',
      detached: true,
      env: { PATH: `${dir}/bin:${process.env.PATH}`, HOME: path.join(dir, 'home'), DOCS_STUB_DIR: dir, ...env },
    });
    const root = run.stderr.match(/^isolated root: (.*)$/m)?.[1];
    assert.ok(root?.startsWith(`${dir}/iso.`), run.stderr);
    const calls = existsSync(path.join(dir, 'calls')) ? readFileSync(path.join(dir, 'calls'), 'utf8').split('\n').filter(Boolean) : [];
    check({ dir, root, calls, status: run.status, stderr: run.stderr });
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
}

test('worktrees.md isolated run keeps HOME, moves every store under its root and removes only that root', () => {
  const inherited = ['MISE_DATA_DIR', 'MISE_ENV', '__MISE_DIFF', 'PITCHFORK_CONFIG_DIR', 'PITCHFORK_STATE_DIR',
    'PITCHFORK_LOGS_DIR', 'STACK_STATE_DIR', 'STACK_DATA_DIR', 'STACK_CACHE_DIR', 'npm_config_globalconfig',
    'npm_config_registry', 'NPM_CONFIG_USERCONFIG'];
  isolatedRun({
    env: Object.fromEntries(inherited.map(k => [k, '/sentinel'])),
    prepare: dir => mkdirSync(path.join(dir, 'iso.other')),
    command: dir => ['node', '-e', 'require("fs").writeFileSync(process.argv[1], JSON.stringify(process.env))', `${dir}/env.json`],
  }, ({ dir, root, calls, status, stderr }) => {
    assert.equal(status, 0, stderr);
    assert.ok(!existsSync(root));
    assert.ok(existsSync(path.join(dir, 'iso.other')), 'a neighbouring directory survives cleanup');
    const env = JSON.parse(readFileSync(path.join(dir, 'env.json'), 'utf8'));
    assert.equal(env.HOME, path.join(dir, 'home'));
    assert.ok(Object.values(env).every(v => !v.includes('/sentinel')));
    assert.ok(!('MISE_ENV' in env) && !('__MISE_DIFF' in env) && !('npm_config_registry' in env));
    for (const key of ['STACK_STATE_DIR', 'STACK_DATA_DIR', 'STACK_CACHE_DIR', 'MISE_DATA_DIR', 'MISE_CACHE_DIR',
      'MISE_STATE_DIR', 'MISE_CONFIG_DIR', 'MISE_GLOBAL_CONFIG_FILE', 'PITCHFORK_CONFIG_DIR', 'PITCHFORK_STATE_DIR',
      'PITCHFORK_LOGS_DIR', 'XDG_CONFIG_HOME', 'XDG_DATA_HOME', 'XDG_CACHE_HOME', 'XDG_STATE_HOME', 'UV_CACHE_DIR',
      'npm_config_cache', 'npm_config_userconfig', 'npm_config_globalconfig', 'npm_config_prefix']) {
      assert.ok(env[key]?.startsWith(`${root}/`), `${key}: ${env[key]}`);
    }
    // Pitchfork's socket is $PITCHFORK_STATE_DIR/sock/main.sock; macOS sun_path holds 104 bytes.
    const documented = `/private/tmp/stack-iso.XXXXXX${env.PITCHFORK_STATE_DIR.slice(root.length)}/sock/main.sock`;
    assert.ok(Buffer.byteLength(documented) < 104, documented);
    const order = ['down', 'exec -- pitchfork supervisor stop', 'exec -- pitchfork supervisor status --json', 'gc'].map(c => calls.indexOf(c));
    assert.ok(order.every((at, i) => at >= 0 && (i === 0 || at > order[i - 1])), calls.join('\n'));
  });
});

test('worktrees.md isolated run keeps its root and fails when cleanup is not confirmed', () => {
  const cases = [
    { fail: ['down'] },
    { fail: ['exec -- pitchfork supervisor stop'] },
    { fail: ['exec -- pitchfork supervisor status --json'] },
    { supervisor: 'unknown' },
    { supervisor: 'up' },
    { fail: ['gc'] },
  ];
  for (const options of cases) {
    isolatedRun(options, ({ root, status, stderr }) => {
      assert.notEqual(status, 0, JSON.stringify(options));
      assert.ok(existsSync(path.join(root, 'global-before')), `${JSON.stringify(options)}\n${stderr}`);
    });
  }
});

test('worktrees.md isolated run stops services after a failed start and reports the failure', () => {
  isolatedRun({ fail: ['up'], command: dir => ['touch', `${dir}/ran`] }, ({ dir, root, calls, status }) => {
    assert.notEqual(status, 0);
    assert.ok(!existsSync(path.join(dir, 'ran')));
    assert.ok(calls.includes('down') && calls.includes('exec -- pitchfork supervisor stop'), calls.join('\n'));
    // The kept receipt lists HOME's missing provider files as absent rather than skipping them.
    const home = path.join(dir, 'home');
    assert.match(readFileSync(path.join(root, 'global-before'), 'utf8'), new RegExp(`^absent ${home}/\\.config/pitchfork/config\\.toml$`, 'm'));
  });
});

test('worktrees.md isolated run still cleans up after the command is interrupted', () => {
  isolatedRun({ command: () => ['bash', '-c', 'kill -INT 0; sleep 5'] }, ({ root, calls, status }) => {
    assert.notEqual(status, 0);
    assert.ok(calls.includes('down') && calls.includes('gc'), calls.join('\n'));
    assert.ok(existsSync(root));
  });
});

test('worktrees.md isolated run keeps its root when a global provider file changes', () => {
  isolatedRun({
    prepare: dir => mkdirSync(path.join(dir, 'home/.config/mise'), { recursive: true }),
    command: () => ['bash', '-c', 'echo "[tools]" >"$HOME/.config/mise/config.toml"'],
  }, ({ root, status }) => {
    assert.notEqual(status, 0);
    assert.ok(existsSync(path.join(root, 'global-before')));
  });
});

test('worktrees.md isolated run refuses to start when a global provider file cannot be read', { skip: process.getuid?.() === 0 }, () => {
  isolatedRun({
    prepare: dir => {
      const file = path.join(dir, 'home/.config/pitchfork/config.toml');
      mkdirSync(path.dirname(file), { recursive: true });
      writeFileSync(file, '');
      chmodSync(file, 0o000);
    },
  }, ({ root, calls, status }) => {
    assert.notEqual(status, 0);
    assert.ok(existsSync(root));
    assert.deepEqual(calls, [], 'nothing installed or started');
  });
});
