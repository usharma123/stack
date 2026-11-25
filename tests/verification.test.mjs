// verification/lib.sh checks the TLC jar against its pinned SHA-256 on every download and every
// use of a cached copy: a jar that does not match never runs.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { mkdtempSync, writeFileSync, existsSync, readdirSync, readFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { spawnSync } from 'node:child_process';

const tlaJar = env => spawnSync('bash', ['-c', 'source verification/lib.sh && tla_jar'],
  { env: { ...process.env, ...env }, encoding: 'utf8', timeout: 60000, killSignal: 'SIGKILL' });

test('a cached jar that does not match the pinned checksum stops the run and is kept for inspection', () => {
  const dir = mkdtempSync(path.join(tmpdir(), 'stack-tla-'));
  try {
    const jar = path.join(dir, 'tla2tools.jar');
    writeFileSync(jar, 'not the pinned jar');
    const result = tlaJar({ VERIFY_TLA_JAR: jar, VERIFY_TLA_URL: 'file:///nonexistent' });
    assert.notEqual(result.status, 0, result.stderr);
    assert.match(result.stderr, /does not match sha256 936a262061c914694dfd669a543be24573c45d5aa0ff20a8b96b23d01e050e88; delete it/);
    assert.equal(readFileSync(jar, 'utf8'), 'not the pinned jar', 'never replaced silently');
  } finally { rmSync(dir, { recursive: true, force: true }); }
});

test('a download that does not match the pinned checksum is discarded, leaving no jar', () => {
  const dir = mkdtempSync(path.join(tmpdir(), 'stack-tla-'));
  try {
    const served = path.join(dir, 'served.jar');
    writeFileSync(served, 'a tampered jar');
    const jar = path.join(dir, 'cache', 'tla2tools.jar');
    const result = tlaJar({ VERIFY_TLA_JAR: jar, VERIFY_TLA_URL: `file://${served}` });
    assert.notEqual(result.status, 0, result.stderr);
    assert.match(result.stderr, /downloaded tla2tools\.jar does not match sha256/);
    assert.deepEqual(readdirSync(path.dirname(jar)), [], 'neither the jar nor a partial download is left');
  } finally { rmSync(dir, { recursive: true, force: true }); }
});

test('a failed download is reported and leaves nothing behind', () => {
  const dir = mkdtempSync(path.join(tmpdir(), 'stack-tla-'));
  try {
    const jar = path.join(dir, 'cache', 'tla2tools.jar');
    const result = tlaJar({ VERIFY_TLA_JAR: jar, VERIFY_TLA_URL: `file://${path.join(dir, 'missing.jar')}` });
    assert.notEqual(result.status, 0);
    assert.match(result.stderr, /cannot download/);
    assert.ok(!existsSync(jar));
    assert.deepEqual(readdirSync(path.dirname(jar)), []);
  } finally { rmSync(dir, { recursive: true, force: true }); }
});
