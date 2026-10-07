// Install the actual tarball into an empty prefix, with lifecycle scripts disabled.
import { mkdtempSync, rmSync, mkdirSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { execFileSync, spawnSync } from 'node:child_process';
import assert from 'node:assert/strict';
const tarball = path.resolve(process.argv[2]);
const dir = mkdtempSync(path.join(tmpdir(), 'stack-install-'));
try {
  execFileSync('npm', ['install', '--global', '--prefix', dir, '--ignore-scripts', '--no-audit', '--no-fund', tarball], { stdio: 'inherit', timeout: 180000, killSignal: 'SIGKILL' });
  const cli = path.join(dir, 'bin', 'stack');
  const version = JSON.parse(execFileSync('tar', ['-xOf', tarball, 'package/package.json'], { timeout: 30000, killSignal: 'SIGKILL', encoding: 'utf8' })).version;
  assert.equal(execFileSync(cli, ['--version'], { timeout: 30000, killSignal: 'SIGKILL', encoding: 'utf8' }).trim(), `stack ${version}`);
  assert.match(execFileSync(cli, ['--help'], { timeout: 30000, killSignal: 'SIGKILL', encoding: 'utf8' }), /compile/);
  const project = path.join(dir, 'project with spaces');
  mkdirSync(project);
  const bundle = path.join(dir, 'bundle');
  mkdirSync(bundle);
  writeFileSync(path.join(bundle, 'bundle.toml'), '[bundle]\nname = "smoke"\nversion = "1.0.0"\n[env]\nSTACK_SMOKE = "works"\n');
  writeFileSync(path.join(project, 'stack.toml'), '[[use]]\nbundle = "path:../bundle"\n');
  const env = { ...process.env, STACK_STATE_DIR: path.join(dir, 'state'), XDG_CACHE_HOME: path.join(dir, 'cache') };
  const result = JSON.parse(execFileSync(cli, ['-C', project, 'compile', '--json'], { env, encoding: 'utf8', timeout: 30000, killSignal: 'SIGKILL' }));
  assert.equal(result.ok, true);
  assert.equal(JSON.parse(execFileSync(cli, ['-C', project, 'compile', '--locked', '--json'], { env, encoding: 'utf8', timeout: 30000, killSignal: 'SIGKILL' })).ok, true);
  const invalid = spawnSync(cli, ['--invalid-argument'], { env, timeout: 30000, killSignal: 'SIGKILL' });
  assert.ifError(invalid.error);
  assert.equal(invalid.signal, null, 'Invalid argument check was killed');
  assert.notEqual(invalid.status, 0);
  console.log('Clean npm installation, CLI forwarding, compile, and locked replay passed');
} finally { rmSync(dir, { recursive: true, force: true }); }
