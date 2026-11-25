import { test } from 'node:test';
import assert from 'node:assert/strict';
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { createHash } from 'node:crypto';
import { buildReleaseNotes, outputLines, previousTag, prepareReleaseAssets } from '../scripts/release-notes.mjs';

test('previousTag selects the newest earlier v* tag', () => {
  assert.equal(previousTag(['v0.1.24', 'v0.1.23', 'v0.1.22'], 'v0.1.24'), 'v0.1.23');
  assert.equal(previousTag(['v0.1.24'], 'v0.1.24'), null);
  assert.equal(previousTag(['other', 'v0.1.23'], 'v0.1.24'), 'v0.1.23');
});

test('buildReleaseNotes uses curated notes when present and appends install and compare links', () => {
  const notes = buildReleaseNotes({
    tag: 'v0.1.24',
    version: '0.1.24',
    curated: 'Stack 0.1.24 replaces Pitchfork with a built-in supervisor.',
    previous: 'v0.1.23',
  });
  assert.match(notes, /replaces Pitchfork/);
  assert.match(notes, /npm install -g @ushawarma\/stack@0\.1\.24/);
  assert.match(notes, /compare\/v0\.1\.23\.\.\.v0\.1\.24/);
});

test('buildReleaseNotes falls back to generated notes when no curated file exists', () => {
  const notes = buildReleaseNotes({
    tag: 'v0.1.25-beta.1',
    version: '0.1.25-beta.1',
    generated: '## What\'s Changed\n* Something great',
    previous: 'v0.1.24',
  });
  assert.match(notes, /Something great/);
  assert.match(notes, /@ushawarma\/stack@0\.1\.25-beta\.1/);
  assert.match(notes, /compare\/v0\.1\.24\.\.\.v0\.1\.25-beta\.1/);
});

test('prepareReleaseAssets writes checksummed tarball, SHA256SUMS, and release notes', () => {
  const dir = mkdtempSync(path.join(tmpdir(), 'stack-release-notes-'));
  try {
    const tarball = path.join(dir, 'ushawarma-stack-0.1.24.tgz');
    writeFileSync(tarball, 'tarball-bytes');
    const curatedPath = path.join(dir, 'v0.1.24.md');
    writeFileSync(curatedPath, 'Curated release summary.\n');
    const outDir = path.join(dir, 'out');
    const result = prepareReleaseAssets(tarball, outDir, {
      tag: 'v0.1.24',
      version: '0.1.24',
      curatedPath,
      previous: 'v0.1.23',
    });
    const expectedHash = createHash('sha256').update('tarball-bytes').digest('hex');
    assert.equal(result.assetName, 'stack-0.1.24.tgz');
    assert.equal(result.sha256, expectedHash);
    assert.equal(result.prerelease, false);
    assert.equal(result.usedCurated, true);
    assert.equal(readFileSync(result.sumsPath, 'utf8'), `${expectedHash}  stack-0.1.24.tgz\n`);
    assert.match(readFileSync(result.notesPath, 'utf8'), /Curated release summary\./);

    const outAuto = path.join(dir, 'out-auto');
    const auto = prepareReleaseAssets(tarball, outAuto, {
      tag: 'v0.1.24-rc.1',
      version: '0.1.24-rc.1',
      curatedPath: path.join(dir, 'missing.md'),
      previous: 'v0.1.23',
      generateNotes: ({ tag }) => `Auto notes for ${tag}`,
    });
    assert.equal(auto.prerelease, true);
    assert.equal(auto.usedCurated, false);
    assert.match(readFileSync(auto.notesPath, 'utf8'), /Auto notes for v0\.1\.24-rc\.1/);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test('workflow outputs are single lines, so a value can never add another output', () => {
  assert.equal(outputLines({ title: 'Stack v0.1.24', prerelease: false }), 'title=Stack v0.1.24\nprerelease=false\n');
  assert.throws(() => outputLines({ title: 'Stack v0.1.24\nasset=/etc/passwd' }), /spans lines/);
  assert.throws(() => outputLines({ notes: 'a\rb' }), /spans lines/);
  assert.throws(() => outputLines({ 'bad name': 'x' }), /Invalid output name/);
});
