import { test } from 'node:test';
import assert from 'node:assert/strict';
import { mkdtempSync, mkdirSync, writeFileSync, readFileSync, copyFileSync, realpathSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { execFileSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { publishPackage } from '../scripts/publish.mjs';

const dir = mkdtempSync(path.join(tmpdir(), 'stack-release-test-'));
mkdirSync(path.join(dir, 'package'));
writeFileSync(path.join(dir, 'package', 'package.json'), JSON.stringify({ name: '@ushawarma/stack', version: '0.1.0-beta.1' }));
const tarball = path.join(dir, 'fixture.tgz');
execFileSync('tar', ['-czf', tarball, '-C', dir, 'package']);
const bytes = readFileSync(tarball);
const integrity = `sha512-${createHash('sha512').update(bytes).digest('base64')}`;
const remote = { dist: { integrity, attestations: { provenance: {} }, tarball: 'https://registry.npmjs.org/fixture.tgz' } };
const index = { versions: { "0.1.0-beta.1": remote } };
const registryData = url => url.endsWith("/0.1.0-beta.1") ? remote : index;
const json = value => new Response(JSON.stringify(value));
const pause = async () => {};

try {
  await test('retry accepts only the same immutable tarball without publishing again', async () => {
    await publishPackage(tarball, {
      pause,
      fetchImpl: async url => url.endsWith('.tgz') ? new Response(bytes) : json(registryData(url)),
      publish: () => assert.fail('already published package must not publish again')
    });
  });
  await test('relative dist tarball paths are passed to npm as absolute file paths', async () => {
    const originalCwd = process.cwd();
    mkdirSync(path.join(dir, 'dist'));
    copyFileSync(tarball, path.join(dir, 'dist', 'package.tgz'));
    let published = false;
    try {
      process.chdir(dir);
      await publishPackage('dist/package.tgz', {
        pause,
        fetchImpl: async url => url.endsWith('.tgz') ? new Response(bytes) : published ? json(registryData(url)) : new Response('', { status: 404 }),
        publish: args => {
          assert.equal(realpathSync(args[1]), realpathSync(path.join(dir, 'dist', 'package.tgz')));
          assert.equal(path.isAbsolute(args[1]), true);
          published = true;
          return { status: 0 };
        }
      });
    } finally { process.chdir(originalCwd); }
    assert.equal(published, true);
  });
  await test('existing different bytes fail before publication', async () => {
    await assert.rejects(publishPackage(tarball, { pause, fetchImpl: async () => json({ dist: { ...remote.dist, integrity: 'wrong' } }), publish: () => assert.fail('must not publish') }), /differs/);
  });
  await test('missing provenance fails a retry', async () => {
    await assert.rejects(publishPackage(tarball, { pause, fetchImpl: async () => json({ dist: { ...remote.dist, attestations: {} } }) }), /provenance/);
  });
  await test('authentication failure is never treated as a missing package', async () => {
    await assert.rejects(publishPackage(tarball, { pause, fetchImpl: async () => new Response('', { status: 401 }), publish: () => assert.fail('must not publish') }), /HTTP 401/);
  });
  await test('lost publish response recovers after transient reads and CDN propagation', async () => {
    let published = false;
    let reads = 0;
    let downloads = 0;
    await publishPackage(tarball, {
      pause,
      fetchImpl: async url => {
        if (url.endsWith('.tgz')) return ++downloads === 1 ? new Response('', { status: 404 }) : new Response(bytes);
        if (!published) return new Response('', { status: 404 });
        if (++reads === 1) return new Response('', { status: 503 });
        return json(registryData(url));
      },
      publish: args => {
        assert.equal(args[args.indexOf('--tag') + 1], 'next');
        published = true;
        return { status: 1 };
      }
    });
    assert.equal(downloads, 2);
  });
  await test('publication waits for the package index needed by npm install', async () => {
    let indexReads = 0;
    await publishPackage(tarball, { pause, fetchImpl: async (url, options) => {
      if (url.endsWith('.tgz')) return new Response(bytes);
      if (url.endsWith('/0.1.0-beta.1')) return json(remote);
      assert.equal(options.headers.accept, 'application/vnd.npm.install-v1+json');
      return ++indexReads === 1 ? new Response('', { status: 404 }) : json(index);
    } });
    assert.equal(indexReads, 2);
  });
  await test('CDN serving different bytes is rejected', async () => {
    await assert.rejects(publishPackage(tarball, { pause, fetchImpl: async url => url.endsWith('.tgz') ? new Response('corrupted') : json(registryData(url)) }), /Expected values/);
  });
  for (const delayed of ['metadata', 'tarball', 'index']) {
    await test(`propagation beyond five minutes recovers: ${delayed}`, async () => {
      let elapsed = 0;
      let publishes = 0;
      const sleeps = [];
      const logs = [];
      await publishPackage(tarball, {
        now: () => elapsed,
        pause: async ms => { sleeps.push(ms); elapsed += ms; },
        log: message => logs.push(message),
        publish: () => { publishes++; return { status: 0 }; },
        fetchImpl: async (url, options) => {
          assert.ok(options.signal instanceof AbortSignal);
          if (!publishes) return new Response('', { status: 404 });
          const kind = url.endsWith('.tgz') ? 'tarball' : url.endsWith('/0.1.0-beta.1') ? 'metadata' : 'index';
          if (kind === delayed && elapsed < 6 * 60 * 1000) return new Response('', { status: 404 });
          return kind === 'tarball' ? new Response(bytes) : json(registryData(url));
        }
      });
      assert.equal(publishes, 1);
      assert.ok(elapsed >= 360000 && elapsed < 900000);
      assert.deepEqual(sleeps.slice(0, 4), [10000, 20000, 40000, 60000]);
      assert.ok(sleeps.every(ms => ms <= 60000));
      assert.ok(logs.some(message => /900s.*retrying/.test(message)));
    });
  }
  await test('verify-only stops at fifteen minutes and never publishes an absent version', async () => {
    let elapsed = 0;
    await assert.rejects(publishPackage(tarball, {
      verifyOnly: true,
      now: () => elapsed,
      pause: async ms => { elapsed += ms; },
      log: () => {},
      fetchImpl: async () => new Response('', { status: 404 }),
      publish: () => assert.fail('verify-only must never publish')
    }), /within 15 minutes.*Version metadata/);
    assert.equal(elapsed, 900000);
  });
  await test('request time counts toward the propagation deadline, including transient failures', async () => {
    let elapsed = 0;
    let reads = 0;
    await assert.rejects(publishPackage(tarball, {
      now: () => elapsed,
      pause: async ms => { elapsed += ms; },
      log: () => {},
      fetchImpl: async () => {
        if (++reads === 1) return json(remote);
        elapsed += Math.min(15000, 900000 - elapsed);
        return new Response('', { status: 503 });
      },
      publish: () => assert.fail('existing version must never publish')
    }), /within 15 minutes.*HTTP 503/);
    assert.equal(elapsed, 900000);
    assert.ok(reads < 30);
  });
  await test('unavailable preflight fails closed after bounded retries', async () => {
    let reads = 0;
    await assert.rejects(publishPackage(tarball, {
      pause, log: () => {},
      fetchImpl: async () => { reads++; throw new Error('network unavailable'); },
      publish: () => assert.fail('uncertain preflight must never publish')
    }), /network unavailable/);
    assert.equal(reads, 5);
  });
  await test('authentication failure during index verification stops immediately', async () => {
    await assert.rejects(publishPackage(tarball, {
      pause: () => assert.fail('must not retry authentication failures'),
      fetchImpl: async url => url.endsWith('.tgz') ? new Response(bytes) :
        url.endsWith('/0.1.0-beta.1') ? json(remote) : new Response('', { status: 403 }),
      publish: () => assert.fail('existing version must never publish')
    }), /HTTP 403/);
  });
} finally { rmSync(dir, { recursive: true, force: true }); }
