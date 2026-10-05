import { test } from 'node:test';
import assert from 'node:assert/strict';
import { mkdtempSync, mkdirSync, writeFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { execFileSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { selectRun, selectArtifact, verifyPackage } from '../scripts/promote.mjs';

const commit = 'a'.repeat(40);
const run = { id: 42, head_sha: commit, head_branch: 'main', event: 'push',
  path: '.github/workflows/ci.yml', repository: { full_name: 'usharma123/stack' },
  head_repository: { full_name: 'usharma123/stack' }, status: 'completed', conclusion: 'success' };
const artifact = { id: 123, name: 'npm-package', expired: false,
  expires_at: '2099-01-01T00:00:00Z', digest: `sha256:${'b'.repeat(64)}`,
  workflow_run: { id: run.id, head_sha: commit } };

test('promotion accepts only push CI from main in this repository at the exact commit', () => {
  assert.equal(selectRun([run], commit), run);
  for (const mutation of [
    { head_sha: 'c'.repeat(40) }, { head_branch: 'feature' }, { event: 'pull_request' },
    { event: 'workflow_dispatch' }, { path: '.github/workflows/other.yml' },
    { repository: { full_name: 'fork/stack' } }, { head_repository: { full_name: 'fork/stack' } }
  ]) assert.throws(() => selectRun([{ ...run, ...mutation }], commit), /No eligible CI/);
});

test('latest run must pass; never fall back to an older success', () => {
  for (const mutation of [{ status: 'in_progress', conclusion: null },
    { conclusion: 'failure' }, { conclusion: 'cancelled' }, { conclusion: 'skipped' }]) {
    assert.throws(() => selectRun([run, { ...run, id: 43, ...mutation }], commit), /must finish successfully/);
  }
  assert.equal(selectRun([{ ...run, id: 41, conclusion: 'failure' }, run], commit), run);
});

test('artifact selection rejects missing, ambiguous, expired, unbound or undigested artifacts', () => {
  assert.equal(selectArtifact([artifact], run), artifact);
  assert.throws(() => selectArtifact([], run), /exactly one/);
  assert.throws(() => selectArtifact([artifact, { ...artifact, id: 124 }], run), /exactly one/);
  for (const mutation of [{ expired: true }, { expires_at: '2000-01-01' },
    { workflow_run: { id: 1, head_sha: commit } }, { workflow_run: { id: run.id, head_sha: 'wrong' } },
    { digest: null }, { id: '123\nmalicious-output=true' }]) {
    assert.throws(() => selectArtifact([{ ...artifact, ...mutation }], run));
  }
});

test('tarball identity and every platform binary are verified before publication', () => {
  const dir = mkdtempSync(path.join(tmpdir(), 'stack-promotion-'));
  try {
    const pkg = path.join(dir, 'package');
    mkdirSync(pkg);
    const version = '0.1.3';
    const manifest = { name: '@ushawarma/stack', version };
    const info = { commit, version, hashes: {} };
    for (const platform of ['linux-x64', 'linux-arm64', 'darwin-x64', 'darwin-arm64']) {
      mkdirSync(path.join(pkg, 'binaries', platform), { recursive: true });
      writeFileSync(path.join(pkg, 'binaries', platform, 'stack'), platform);
      info.hashes[platform] = createHash('sha256').update(platform).digest('hex');
    }
    const file = path.join(dir, 'package.tgz');
    const pack = () => {
      writeFileSync(path.join(pkg, 'package.json'), JSON.stringify(manifest));
      writeFileSync(path.join(pkg, 'build-info.json'), JSON.stringify(info));
      execFileSync('tar', ['-czf', file, '-C', dir, 'package']);
    };
    const expected = { commit, version, tag: `v${version}` };
    pack();
    verifyPackage(file, expected);
    assert.throws(() => verifyPackage(file, { ...expected, tag: 'v9.0.0' }), /tag/);
    assert.throws(() => verifyPackage(file, { ...expected, commit: 'wrong' }), /different commit/);
    manifest.version = '9.0.0'; pack();
    assert.throws(() => verifyPackage(file, expected), /Package version/);
    manifest.version = version;
    manifest.name = '@other/package'; pack();
    assert.throws(() => verifyPackage(file, expected), /package name/);
    manifest.name = '@ushawarma/stack';
    info.version = '9.0.0'; pack();
    assert.throws(() => verifyPackage(file, expected), /Build version/);
    info.version = version;
    writeFileSync(path.join(pkg, 'binaries', 'darwin-arm64', 'stack'), 'corrupted'); pack();
    assert.throws(() => verifyPackage(file, expected), /checksum mismatch/);
    delete info.hashes['darwin-arm64']; pack();
    assert.throws(() => verifyPackage(file, expected), /platform set/);
  } finally { rmSync(dir, { recursive: true, force: true }); }
});

// A tag dispatch can regenerate an unpublished artifact after GitHub's rerun window.
test('exact-tag recovery accepts only successful CI for the release commit', () => {
  const tag = 'v0.1.3';
  const recovery = { ...run, id: 100, event: 'workflow_dispatch', head_branch: tag };
  assert.equal(selectRun([run, recovery], commit, tag), recovery);
  for (const mutation of [{ head_branch: 'main' }, { head_branch: 'feature' },
    { head_branch: 'v0.1.2' }, { head_sha: 'wrong' }, { event: 'pull_request' },
    { head_repository: { full_name: 'fork/stack' } }]) {
    assert.throws(() => selectRun([{ ...recovery, ...mutation }], commit, tag), /No eligible CI/);
  }
  assert.throws(() => selectRun([run, { ...recovery, conclusion: 'failure' }], commit, tag), /must finish successfully/);
  assert.throws(() => selectRun([run, { ...recovery, status: 'in_progress' }], commit, tag), /must finish successfully/);
  assert.throws(() => selectRun([recovery], commit), /No eligible CI/);
});
