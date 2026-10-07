// Retry registry reads, never blindly repeat an immutable publish.
import { readFileSync } from 'node:fs';
import { createHash } from 'node:crypto';
import { execFileSync, spawnSync } from 'node:child_process';
import assert from 'node:assert/strict';
import { pathToFileURL } from 'node:url';
import path from 'node:path';

class RegistryHttpError extends Error {
  constructor(status) {
    super(`Registry HTTP ${status}`);
    this.retryable = status === 404 || status === 429 || status >= 500;
  }
}

export async function publishPackage(file, {
  verifyOnly = false,
  fetchImpl = fetch,
  pause = ms => new Promise(resolve => setTimeout(resolve, ms)),
  now = () => performance.now(),
  log = console.log,
  publish = args => spawnSync('npm', args, { stdio: 'inherit', timeout: 180000, killSignal: 'SIGKILL' })
} = {}) {
  // A bare two-part relative path is npm's GitHub repository shorthand.
  file = path.resolve(file);
  const manifest = JSON.parse(execFileSync('tar', ['-xOf', file, 'package/package.json'], {
    encoding: 'utf8', timeout: 30000, killSignal: 'SIGKILL'
  }));
  const integrity = `sha512-${createHash('sha512').update(readFileSync(file)).digest('base64')}`;
  const tag = manifest.version.includes('-') ? 'next' : 'latest';
  const registry = 'https://registry.npmjs.org';
  const url = `${registry}/${encodeURIComponent(manifest.name)}/${manifest.version}`;
  const permanent = error => error.code === 'ERR_ASSERTION' ||
    (error instanceof RegistryHttpError && !error.retryable);
  async function read(endpoint, { accept = 'application/json', binary = false, deadline = Infinity } = {}) {
    const remaining = Math.ceil(deadline - now());
    if (remaining <= 0) throw new Error('Registry verification deadline reached');
    const response = await fetchImpl(endpoint, {
      headers: { accept }, signal: AbortSignal.timeout(Math.min(15000, remaining))
    });
    if (response.status === 404 && !binary) return null;
    if (!response.ok) throw new RegistryHttpError(response.status);
    return binary ? Buffer.from(await response.arrayBuffer()) : await response.json();
  }
  // Fail closed if the registry cannot tell us whether this version already exists.
  let remote;
  for (let attempt = 0; attempt < 5; attempt++) {
    try {
      remote = await read(url);
      break;
    } catch (error) {
      if (permanent(error) || attempt === 4) throw error;
      log(`Registry preflight ${attempt + 1}/5: ${error.message}; retrying in ${2 * (attempt + 1)}s`);
      await pause(2000 * (attempt + 1));
    }
  }
  if (!remote && !verifyOnly) {
    const result = publish(['publish', file, '--access', 'public', '--provenance', '--tag', tag, '--registry', registry, '--ignore-scripts']);
    if (result.status !== 0) console.error('Publish did not report success; checking whether npm accepted the exact tarball.');
  } else {
    log(`Verifying ${manifest.name}@${manifest.version} without publishing`);
  }

  // One wall-clock budget includes requests, response bodies and backoff sleeps.
  const started = now();
  const deadline = started + 15 * 60 * 1000;
  let backoff = 10000;
  let reason = 'Version metadata has not propagated yet';
  for (let attempt = 1; now() < deadline; attempt++) {
    try {
      remote = await read(url, { deadline });
      if (!remote) throw new Error('Version metadata has not propagated yet');
      assert.equal(remote.dist.integrity, integrity, 'Published version differs from this tarball. Never overwrite or skip it.');
      assert.ok(remote.dist.attestations?.provenance, 'Published package has no provenance attestation');
      const downloaded = await read(remote.dist.tarball, { binary: true, deadline });
      assert.equal(`sha512-${createHash('sha512').update(downloaded).digest('base64')}`, integrity);
      const index = await read(`${registry}/${encodeURIComponent(manifest.name)}`, {
        accept: 'application/vnd.npm.install-v1+json', deadline
      });
      if (!index?.versions?.[manifest.version]) throw new Error('Registry package index has not propagated yet');
      assert.equal(index.versions[manifest.version].dist.integrity, integrity, 'Registry package index differs from this tarball');
      log(`Verified ${manifest.name}@${manifest.version}: registry integrity, provenance, and downloadable tarball`);
      return;
    } catch (error) {
      if (permanent(error)) throw error;
      reason = error.message;
    }
    const remaining = deadline - now();
    if (remaining <= 0) break;
    const delay = Math.min(backoff, remaining);
    log(`Registry verification ${attempt} (${Math.round((now() - started) / 1000)}s/900s): ${reason}; retrying in ${Math.ceil(delay / 1000)}s`);
    await pause(delay);
    backoff = Math.min(backoff * 2, 60000);
  }
  throw new Error(`Package did not become available within 15 minutes: ${reason}. Rerun the publish job using the same artifact; do not republish different bytes.`);
}
if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  await publishPackage(process.argv[2], { verifyOnly: process.argv.includes('--verify-only') });
}
