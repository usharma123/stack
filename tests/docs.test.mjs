import { test } from 'node:test';
import assert from 'node:assert/strict';
import { chmodSync, existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { execFileSync, spawn } from 'node:child_process';
import { setTimeout as sleep } from 'node:timers/promises';

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
// stand-in stack logs each call, fails a call that starts with an entry in `fail`, hangs on one
// in `hang` and reports `supervisor` as the supervisor's status. A call in `resist` ignores
// signals and succeeds after a second; so does npm when `npm` is 'resist', while 'hang' makes
// it hang. Hanging and resisting stand-ins touch `<dir>/busy` first, and all of them stop on
// their own within 20 seconds. `command` receives the temporary directory and returns argv;
// `grace` replaces the script's; `interrupt` signals the running script.
async function isolatedRun({ fail = [], hang = [], resist = [], npm = '', supervisor = 'down', command = () => ['true'],
  prepare = () => {}, env = {}, grace, interrupt }, check) {
  const dir = mkdtempSync(path.join(tmpdir(), 'stack-docs-'));
  try {
    const template = '/tmp/stack-iso.XXXXXX';
    let script = block('worktrees.md', 'Isolated runs', 'bash');
    assert.ok(script.includes(template));
    script = script.replace(template, `${dir}/iso.XXXXXX`);
    if (grace !== undefined) {
      assert.match(script, /^grace=10 /m);
      script = script.replace(/^grace=10 /m, `grace=${grace} `);
    }
    writeFileSync(path.join(dir, 'isolated-stack.sh'), script);
    for (const sub of ['bin', 'home', 'checkout']) mkdirSync(path.join(dir, sub));
    for (const [name, lines] of Object.entries({ fail, hang, resist })) writeFileSync(path.join(dir, name), lines.map(f => `${f}\n`).join(''));
    writeFileSync(path.join(dir, 'supervisor'), supervisor);
    writeFileSync(path.join(dir, 'bin/stack'), `#!/bin/bash
echo "$*" >>"$DOCS_STUB_DIR/calls"
listed() { local list=$1; shift; while IFS= read -r f; do case "$*" in "$f"|"$f "*) return 0 ;; esac; done <"$DOCS_STUB_DIR/$list"; return 1; }
listed fail "$@" && exit 1
listed hang "$@" && { touch "$DOCS_STUB_DIR/busy"; sleep 20; exit 0; }
listed resist "$@" && { trap '' INT TERM HUP; touch "$DOCS_STUB_DIR/busy"; sleep 1; exit 0; }
[ "$*" = "exec -- pitchfork supervisor status --json" ] && printf '{"status":"%s"}\\n' "$(cat "$DOCS_STUB_DIR/supervisor")"
exit 0
`, { mode: 0o755 });
    writeFileSync(path.join(dir, 'bin/npm'), `#!/bin/bash
echo "npm $*" >>"$DOCS_STUB_DIR/calls"
case ${JSON.stringify(npm)} in
  hang) touch "$DOCS_STUB_DIR/busy"; sleep 20 ;;
  resist) trap '' INT TERM HUP; touch "$DOCS_STUB_DIR/busy"; sleep 1 ;;
esac
exit 0
`, { mode: 0o755 });
    prepare(dir);
    // Its own process group, so a signal to the group reaches the script and not the test.
    const child = spawn('bash', [path.join(dir, 'isolated-stack.sh'), '0.1.18', path.join(dir, 'checkout'), ...command(dir)], {
      cwd: dir,
      detached: true,
      stdio: ['ignore', 'ignore', 'pipe'],
      env: { PATH: `${dir}/bin:${process.env.PATH}`, HOME: path.join(dir, 'home'), DOCS_STUB_DIR: dir, ...env },
    });
    let stderr = '';
    child.stderr.setEncoding('utf8').on('data', chunk => { stderr += chunk; });
    const exited = new Promise(resolve => child.on('close', status => resolve(status)));
    // Node reaps the script only when it records its exit, so until then its PID and group are its own.
    const send = signal => {
      assert.ok(child.exitCode === null && child.signalCode === null, `the script exited before ${signal}`);
      return signal;
    };
    const signalled = interrupt ? await interrupt({ dir, pid: child.pid, send }) : undefined;
    const status = await exited;
    const elapsed = signalled === undefined ? undefined : performance.now() - signalled;
    const root = stderr.match(/^isolated root: (.*)$/m)?.[1];
    assert.ok(root?.startsWith(`${dir}/iso.`), stderr);
    const calls = existsSync(path.join(dir, 'calls')) ? readFileSync(path.join(dir, 'calls'), 'utf8').split('\n').filter(Boolean) : [];
    await check({ dir, root, calls, status, stderr, elapsed });
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
}

async function appears(file) {
  for (let waited = 0; !existsSync(file); waited += 20) {
    assert.ok(waited < 10_000, `${file} did not appear`);
    await sleep(20);
  }
}

// Sends `signal` to the script alone, or to its whole process group as Ctrl-C does, once
// `<dir>/<after>` exists, and again after `again` milliseconds if given. Resolves to the time
// of the first signal.
function signalAfter(after, signal, { group = false, again } = {}) {
  return async ({ dir, pid, send }) => {
    await appears(path.join(dir, after));
    const at = performance.now();
    process.kill(group ? -pid : pid, send(signal));
    if (again !== undefined) {
      await sleep(again);
      process.kill(group ? -pid : pid, send(signal));
    }
    return at;
  };
}

// Whether the process whose PID a stand-in wrote to `file` is gone. It was reparented when its
// parent died, so allow a moment for it to be reaped.
async function gone(file) {
  const pid = Number(readFileSync(file, 'utf8'));
  for (let waited = 0; waited < 3_000; waited += 50) {
    try { process.kill(pid, 0); } catch (e) { if (e.code === 'ESRCH') return true; throw e; }
    await sleep(50);
  }
  return false;
}

const CLEANUP = ['down', 'exec -- pitchfork supervisor stop', 'exec -- pitchfork supervisor status --json', 'gc'];

function assertCleanedUp(calls) {
  const order = CLEANUP.map(c => calls.indexOf(c));
  assert.ok(order.every((at, i) => at >= 0 && (i === 0 || at > order[i - 1])), calls.join('\n'));
}

// A stand-in command: records its PID, runs until 20 seconds pass, then records that it finished.
const TASK = ['bash', '-c', 'echo $$ >"$DOCS_STUB_DIR/task.pid"; touch "$DOCS_STUB_DIR/started"; sleep 20; touch "$DOCS_STUB_DIR/finished"'];
// Like TASK, but it ignores interrupt, terminate and hangup.
const STUBBORN = ['bash', '-c', 'trap "" INT TERM HUP; echo $$ >"$DOCS_STUB_DIR/task.pid"; touch "$DOCS_STUB_DIR/started"; sleep 20; touch "$DOCS_STUB_DIR/finished"'];

test('worktrees.md isolated run keeps HOME, moves every store under its root and removes only that root', async () => {
  const inherited = ['MISE_DATA_DIR', 'MISE_ENV', '__MISE_DIFF', 'PITCHFORK_CONFIG_DIR', 'PITCHFORK_STATE_DIR',
    'PITCHFORK_LOGS_DIR', 'STACK_STATE_DIR', 'STACK_DATA_DIR', 'STACK_CACHE_DIR', 'npm_config_globalconfig',
    'npm_config_registry', 'NPM_CONFIG_USERCONFIG'];
  await isolatedRun({
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
    assertCleanedUp(calls);
  });
});

test('worktrees.md isolated run keeps its root and fails when cleanup is not confirmed', async () => {
  const cases = [
    { fail: ['down'] },
    { fail: ['exec -- pitchfork supervisor stop'] },
    { fail: ['exec -- pitchfork supervisor status --json'] },
    { supervisor: 'unknown' },
    { supervisor: 'up' },
    { fail: ['gc'] },
  ];
  for (const options of cases) {
    await isolatedRun(options, ({ root, status, stderr }) => {
      assert.notEqual(status, 0, JSON.stringify(options));
      assert.ok(existsSync(path.join(root, 'global-before')), `${JSON.stringify(options)}\n${stderr}`);
    });
  }
});

test('worktrees.md isolated run stops services after a failed start and reports the failure', async () => {
  await isolatedRun({ fail: ['up'], command: dir => ['touch', `${dir}/ran`] }, ({ dir, root, calls, status }) => {
    assert.notEqual(status, 0);
    assert.ok(!existsSync(path.join(dir, 'ran')));
    assert.ok(calls.includes('down') && calls.includes('exec -- pitchfork supervisor stop'), calls.join('\n'));
    // The kept receipt lists HOME's missing provider files as absent rather than skipping them.
    const home = path.join(dir, 'home');
    assert.match(readFileSync(path.join(root, 'global-before'), 'utf8'), new RegExp(`^absent ${home}/\\.config/pitchfork/config\\.toml$`, 'm'));
  });
});

test('worktrees.md isolated run keeps its root when a global provider file changes', async () => {
  await isolatedRun({
    prepare: dir => mkdirSync(path.join(dir, 'home/.config/mise'), { recursive: true }),
    command: () => ['bash', '-c', 'echo "[tools]" >"$HOME/.config/mise/config.toml"'],
  }, ({ root, status }) => {
    assert.notEqual(status, 0);
    assert.ok(existsSync(path.join(root, 'global-before')));
  });
});

test('worktrees.md isolated run refuses to start when a global provider file cannot be read', { skip: process.getuid?.() === 0 }, async () => {
  await isolatedRun({
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

test('worktrees.md isolated run cancels the command and cleans up on a signal to the script alone', async () => {
  for (const [signal, code] of [['SIGTERM', 143], ['SIGINT', 130], ['SIGHUP', 129]]) {
    await isolatedRun({ command: () => TASK, interrupt: signalAfter('started', signal) }, async ({ dir, root, calls, status, stderr, elapsed }) => {
      assert.equal(status, code, `${signal}\n${stderr}`);
      // Well within the 10 second grace: the command got the signal itself, not a later SIGKILL.
      assert.ok(elapsed < 5_000, `${signal}: ${elapsed}ms`);
      assert.ok(!existsSync(path.join(dir, 'finished')), signal);
      assert.ok(await gone(path.join(dir, 'task.pid')), signal);
      assertCleanedUp(calls);
      assert.ok(existsSync(path.join(root, 'global-before')), 'the root and its receipts are kept');
      assert.match(stderr, new RegExp(`^interrupted by ${signal}$`, 'm'));
      assert.match(stderr, /interrupted; everything it started is stopped; keeping /);
    });
  }
});

test('worktrees.md isolated run cancels the command and cleans up on Ctrl-C to its process group', async () => {
  await isolatedRun({ command: () => TASK, interrupt: signalAfter('started', 'SIGINT', { group: true }) }, async ({ dir, root, calls, status, elapsed }) => {
    assert.equal(status, 130);
    assert.ok(elapsed < 5_000, `${elapsed}ms`);
    assert.ok(!existsSync(path.join(dir, 'finished')));
    assert.ok(await gone(path.join(dir, 'task.pid')));
    assertCleanedUp(calls);
    assert.ok(existsSync(root));
  });
});

test('worktrees.md isolated run kills what the command left in its group after a signal', async () => {
  // The command exits on SIGTERM; a grandchild that ignores it, writing into a pipe, does not.
  const command = ['bash', '-c', `bash -c 'trap "" INT TERM HUP; echo $$ >"$DOCS_STUB_DIR/grandchild.pid"; exec sleep 20' | cat &
    touch "$DOCS_STUB_DIR/started"; wait`];
  await isolatedRun({ command: () => command, interrupt: signalAfter('started', 'SIGTERM') }, async ({ dir, root, calls, status, elapsed }) => {
    assert.equal(status, 143);
    assert.ok(elapsed < 5_000, `${elapsed}ms`);
    assert.ok(await gone(path.join(dir, 'grandchild.pid')));
    assertCleanedUp(calls);
    assert.ok(existsSync(root));
  });
});

test('worktrees.md isolated run kills a command that ignores the signal after the grace period', async () => {
  await isolatedRun({ grace: 1, command: () => STUBBORN, interrupt: signalAfter('started', 'SIGTERM') }, async ({ dir, root, calls, status, elapsed }) => {
    assert.equal(status, 143);
    assert.ok(elapsed >= 1_000 && elapsed < 6_000, `${elapsed}ms`);
    assert.ok(!existsSync(path.join(dir, 'finished')));
    assert.ok(await gone(path.join(dir, 'task.pid')));
    assertCleanedUp(calls);
    assert.ok(existsSync(root));
  });
});

test('worktrees.md isolated run kills a command that ignores the signal at a second signal', async () => {
  await isolatedRun({ command: () => STUBBORN, interrupt: signalAfter('started', 'SIGTERM', { again: 300 }) }, async ({ dir, root, calls, status, elapsed }) => {
    assert.equal(status, 143);
    assert.ok(elapsed < 5_000, `${elapsed}ms`);
    assert.ok(await gone(path.join(dir, 'task.pid')));
    assertCleanedUp(calls);
    assert.ok(existsSync(root));
  });
});

test('worktrees.md isolated run starts nothing more after a signal during setup', async () => {
  // npm is cancelled; and npm that ignores the signal and succeeds still stops the run.
  for (const npm of ['hang', 'resist']) {
    await isolatedRun({ npm, command: dir => ['touch', `${dir}/ran`], interrupt: signalAfter('busy', 'SIGTERM') }, ({ dir, root, calls, status, stderr, elapsed }) => {
      assert.equal(status, 143, `${npm}\n${stderr}`);
      assert.ok(elapsed < 5_000, `${npm}: ${elapsed}ms`);
      assert.deepEqual(calls.filter(c => !c.startsWith('npm ')), [], npm);
      assert.ok(!existsSync(path.join(dir, 'ran')));
      assert.ok(existsSync(path.join(root, 'global-before')), npm);
    });
  }
});

test('worktrees.md isolated run skips the command after a signal during stack up and still cleans up', async () => {
  await isolatedRun({ resist: ['up'], command: dir => ['touch', `${dir}/ran`], interrupt: signalAfter('busy', 'SIGINT') }, ({ dir, root, calls, status }) => {
    assert.equal(status, 130);
    assert.ok(!existsSync(path.join(dir, 'ran')), 'up succeeded, but the command must not start');
    assertCleanedUp(calls);
    assert.ok(existsSync(root));
  });
});

test('worktrees.md isolated run stops cleaning up and keeps its root after a signal during cleanup', async () => {
  // stack down is cancelled; and a stack down that ignores the signal and succeeds still ends cleanup.
  for (const options of [{ hang: ['down'] }, { resist: ['down'] }]) {
    await isolatedRun({ ...options, interrupt: signalAfter('busy', 'SIGTERM') }, ({ root, calls, status, stderr, elapsed }) => {
      assert.equal(status, 143, `${JSON.stringify(options)}\n${stderr}`);
      assert.ok(elapsed < 5_000, `${elapsed}ms`);
      assert.ok(calls.includes('down'));
      assert.ok(!calls.includes('exec -- pitchfork supervisor stop') && !calls.includes('gc'), calls.join('\n'));
      assert.ok(existsSync(path.join(root, 'global-before')));
    });
  }
});
