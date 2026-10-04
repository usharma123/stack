// Retry registry reads, never blindly repeat an immutable publish.
import { readFileSync } from 'node:fs';
import { createHash } from 'node:crypto';
import { execFileSync, spawnSync } from 'node:child_process';
import assert from 'node:assert/strict';
import { pathToFileURL } from 'node:url';

export async function publishPackage(file, { verifyOnly = false, fetchImpl = fetch, pause = ms => new Promise(resolve => setTimeout(resolve, ms)), publish = args => spawnSync('npm', args, { stdio: 'inherit', timeout: 180000 }) } = {}) {
  const manifest = JSON.parse(execFileSync('tar', ['-xOf', file, 'package/package.json'], { encoding: 'utf8' }));
  const integrity = `sha512-${createHash('sha512').update(readFileSync(file)).digest('base64')}`;
  const tag = manifest.version.includes('-') ? 'next' : 'latest';
  const registry = 'https://registry.npmjs.org';
  const url = `${registry}/${encodeURIComponent(manifest.name)}/${manifest.version}`;
  const delay = pause;
  async function metadata(endpoint = url, accept = 'application/json') {
    for (let attempt = 0; attempt < 5; attempt++) {
      try {
        const response = await fetchImpl(endpoint, { headers: { accept }, signal: AbortSignal.timeout(15000) });
        if (response.status === 404) return null;
        if (response.status === 429 || response.status >= 500) throw new Error(`Registry HTTP ${response.status}`);
        if (!response.ok) throw new TypeError(`Registry HTTP ${response.status}`);
        return await response.json();
      } catch (error) {
        // HTTP authentication failures must not be interpreted as an absent package.
        if (error instanceof TypeError && error.message.startsWith('Registry HTTP')) throw error;
        if (attempt === 4) throw error;
        await delay(2000 * (attempt + 1));
      }
    }
  }
  let remote = await metadata();
  if (!remote && !verifyOnly) {
    const result = publish(['publish', file, '--access', 'public', '--provenance', '--tag', tag, '--registry', registry, '--ignore-scripts']);
    if (result.status !== 0) console.error('Publish did not report success; checking whether npm accepted the exact tarball.');
  }
  for (let attempt = 0; attempt < 30; attempt++) {
    remote = await metadata();
    if (remote) {
      assert.equal(remote.dist.integrity, integrity, 'Published version differs from this tarball. Never overwrite or skip it.');
      assert.ok(remote.dist.attestations?.provenance, 'Published package has no provenance attestation');
      try {
        const response = await fetchImpl(remote.dist.tarball, { signal: AbortSignal.timeout(15000) });
        if (!response.ok) throw new Error(`Tarball HTTP ${response.status}`);
        const downloaded = Buffer.from(await response.arrayBuffer());
        assert.equal(`sha512-${createHash('sha512').update(downloaded).digest('base64')}`, integrity);
        const index = await metadata(`${registry}/${encodeURIComponent(manifest.name)}`, 'application/vnd.npm.install-v1+json');
        if (!index?.versions?.[manifest.version]) throw new Error('Registry package index has not propagated yet');
        assert.equal(index.versions[manifest.version].dist.integrity, integrity, 'Registry package index differs from this tarball');
        console.log(`Verified ${manifest.name}@${manifest.version}: registry integrity, provenance, and downloadable tarball`);
        return;
      } catch (error) {
        if (error.code === 'ERR_ASSERTION') throw error;
        if (attempt === 29) throw error;
      }
    }
    if (attempt < 29) await delay(10000);
  }
  throw new Error('Package did not become available. Fix authentication/setup and rerun the publish job using the same artifact.');
}
if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  await publishPackage(process.argv[2], { verifyOnly: process.argv.includes("--verify-only") });
}
