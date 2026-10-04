import { test } from 'node:test';
import assert from 'node:assert/strict';
import { mkdtempSync, mkdirSync, writeFileSync, copyFileSync, rmSync, readFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { execFileSync, spawnSync } from 'node:child_process';
import { createHash } from 'node:crypto';

const script = path.resolve('scripts/package.mjs');
const dir = mkdtempSync(path.join(tmpdir(), 'stack-package-test-'));
const platforms = ['linux-x64', 'linux-arm64', 'darwin-x64', 'darwin-arm64'];
const run = env => spawnSync(process.execPath, [script], { cwd: dir, env: { ...process.env, ...env }, encoding: 'utf8' });
try {
  mkdirSync(path.join(dir, 'npm'));
  copyFileSync('npm/package.json', path.join(dir, 'npm/package.json'));
  copyFileSync('Cargo.toml', path.join(dir, 'Cargo.toml'));
  copyFileSync('README.md', path.join(dir, 'README.md'));
  execFileSync('git', ['init', '-q', dir]);
  execFileSync('git', ['-C', dir, '-c', 'user.name=Test', '-c', 'user.email=test@example.com', 'commit', '--allow-empty', '-qm', 'fixture']);
  const commit = execFileSync('git', ['-C', dir, 'rev-parse', 'HEAD'], { encoding: 'utf8' }).trim();
  const version = JSON.parse(readFileSync('npm/package.json')).version;
  for (const platform of platforms) {
    const target = path.join(dir, 'artifacts', platform);
    mkdirSync(target, { recursive: true });
    const bytes = Buffer.from(`test fixture for ${platform}`);
    writeFileSync(path.join(target, 'stack'), bytes);
    writeFileSync(path.join(target, 'build-info.json'), JSON.stringify({ platform, commit, version, sha256: createHash('sha256').update(bytes).digest('hex') }));
  }
  await test('assembly rejects release tags with a different version', () => {
    const result = run({ RELEASE_TAG: 'v99.0.0' });
    assert.notEqual(result.status, 0);
    assert.match(result.stderr, /must match/);
  });
  await test('assembly requires all four native artifacts', () => {
    const info = path.join(dir, 'artifacts', 'linux-x64', 'build-info.json');
    const original = readFileSync(info);
    rmSync(info);
    assert.notEqual(run({ RELEASE_TAG: '' }).status, 0);
    writeFileSync(info, original);
  });
  await test('assembly rejects mixed-commit artifacts', () => {
    const info = path.join(dir, 'artifacts', 'linux-x64', 'build-info.json');
    const original = readFileSync(info);
    writeFileSync(info, JSON.stringify({ ...JSON.parse(original), commit: 'wrong' }));
    assert.match(run({ RELEASE_TAG: '' }).stderr, /identity mismatch/);
    writeFileSync(info, original);
  });
  await test('assembly rejects corrupted binaries', () => {
    const binary = path.join(dir, 'artifacts', 'darwin-arm64', 'stack');
    const original = readFileSync(binary);
    writeFileSync(binary, 'corrupted');
    assert.match(run({ RELEASE_TAG: '' }).stderr, /checksum mismatch/);
    writeFileSync(binary, original);
  });
  await test('assembly includes the complete checked platform set', () => {
    const result = run({ RELEASE_TAG: `v${version}` });
    assert.equal(result.status, 0, result.stderr);
    const info = JSON.parse(readFileSync(path.join(dir, 'npm', 'build-info.json')));
    assert.deepEqual(Object.keys(info.hashes), platforms);
    assert.equal(info.commit, commit);
  });
} finally { rmSync(dir, { recursive: true, force: true }); }
