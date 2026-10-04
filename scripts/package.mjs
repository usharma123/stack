// Assemble the exact npm payload from native CI artifacts. No registry dependencies.
import { readFileSync, writeFileSync, mkdirSync, copyFileSync, chmodSync } from 'node:fs';
import { createHash } from 'node:crypto';
import { execFileSync } from 'node:child_process';
import path from 'node:path';

export const platforms = ['linux-x64', 'linux-arm64', 'darwin-x64', 'darwin-arm64'];
const manifest = JSON.parse(readFileSync('npm/package.json'));
if (!/^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)(?:-[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?$/.test(manifest.version)) throw new Error('Invalid release SemVer');
const cargoVersion = readFileSync('Cargo.toml', 'utf8').match(/^version = "([^"]+)"/m)?.[1];
if (cargoVersion !== manifest.version) throw new Error('Cargo and npm versions differ');
const tag = process.env.RELEASE_TAG;
if (tag && tag !== `v${manifest.version}`) throw new Error(`Tag ${tag} must match v${manifest.version}`);
const commit = execFileSync('git', ['rev-parse', 'HEAD'], { encoding: 'utf8' }).trim();
const hashes = {};
for (const platform of platforms) {
  const source = path.join(process.argv[2] ?? 'artifacts', platform);
  const info = JSON.parse(readFileSync(path.join(source, 'build-info.json')));
  if (info.commit !== commit || info.version !== manifest.version || info.platform !== platform) {
    throw new Error(`Artifact identity mismatch: ${platform}`);
  }
  const bytes = readFileSync(path.join(source, 'stack'));
  const hash = createHash('sha256').update(bytes).digest('hex');
  if (hash !== info.sha256) throw new Error(`Artifact checksum mismatch: ${platform}`);
  const destination = path.join('npm/binaries', platform);
  mkdirSync(destination, { recursive: true });
  copyFileSync(path.join(source, 'stack'), path.join(destination, 'stack'));
  chmodSync(path.join(destination, 'stack'), 0o755);
  hashes[platform] = hash;
}
copyFileSync('README.md', 'npm/README.md');
writeFileSync('npm/build-info.json', JSON.stringify({ commit, version: manifest.version, hashes }, null, 2) + '\n');
console.log(`Assembled ${manifest.name}@${manifest.version} from ${commit}`);
