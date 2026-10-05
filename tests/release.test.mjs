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
} finally { rmSync(dir, { recursive: true, force: true }); }
