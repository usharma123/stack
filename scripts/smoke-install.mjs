// Install the actual tarball into an empty prefix, with lifecycle scripts disabled.
import { mkdtempSync, rmSync, mkdirSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { execFileSync, spawnSync } from 'node:child_process';
import assert from 'node:assert/strict';
const tarball = path.resolve(process.argv[2]);
const dir = mkdtempSync(path.join(tmpdir(), 'stack-install-'));
try {
  execFileSync('npm', ['install', '--global', '--prefix', dir, '--ignore-scripts', '--no-audit', '--no-fund', tarball], { stdio: 'inherit' });
  const cli = path.join(dir, 'bin', 'stack');
  const version = JSON.parse(execFileSync('tar', ['-xOf', tarball, 'package/package.json'], { encoding: 'utf8' })).version;
  assert.equal(execFileSync(cli, ['--version'], { encoding: 'utf8' }).trim(), `stack ${version}`);
  assert.match(execFileSync(cli, ['--help'], { encoding: 'utf8' }), /compile/);
  const project = path.join(dir, 'project with spaces');
  mkdirSync(project);
  writeFileSync(path.join(project, 'stack.toml'), '[env]\nSTACK_SMOKE = "works"\n');
  const env = { ...process.env, STACK_STATE_DIR: path.join(dir, 'state'), XDG_CACHE_HOME: path.join(dir, 'cache') };
  const result = JSON.parse(execFileSync(cli, ['-C', project, 'compile', '--json'], { env, encoding: 'utf8' }));
  assert.equal(result.ok, true);
  assert.equal(JSON.parse(execFileSync(cli, ['-C', project, 'compile', '--locked', '--json'], { env, encoding: 'utf8' })).ok, true);
  assert.notEqual(spawnSync(cli, ['--invalid-argument'], { env }).status, 0);
  console.log('Clean npm installation, CLI forwarding, compile, and locked replay passed');
} finally { rmSync(dir, { recursive: true, force: true }); }
