import { readFileSync, writeFileSync, mkdirSync, copyFileSync } from 'node:fs';
import { createHash } from 'node:crypto';
import { execFileSync } from 'node:child_process';
const [platform, binary = 'target/release/stack'] = process.argv.slice(2);
if (!['linux-x64', 'linux-arm64', 'darwin-x64', 'darwin-arm64'].includes(platform)) throw new Error('Invalid platform');
if (platform !== `${process.platform}-${process.arch}`) throw new Error('Artifact must be produced on its native platform');
const version = JSON.parse(readFileSync('npm/package.json')).version;
const actual = execFileSync(binary, ['--version'], { timeout: 30000, killSignal: 'SIGKILL', encoding: 'utf8' }).trim();
if (actual !== `stack ${version}`) throw new Error(`Unexpected binary version: ${actual}`);
if (platform.startsWith('linux-')) {
  // npm installs the same binary on every distribution; a dynamic loader would tie it to one libc.
  const kind = execFileSync('file', ['-b', binary], { timeout: 30000, killSignal: 'SIGKILL', encoding: 'utf8' });
  if (!/statically linked|static-pie linked/.test(kind)) throw new Error(`Linux binary must be statically linked: ${kind.trim()}`);
}
const commit = execFileSync('git', ['rev-parse', 'HEAD'], { timeout: 30000, killSignal: 'SIGKILL', encoding: 'utf8' }).trim();
const sha256 = createHash('sha256').update(readFileSync(binary)).digest('hex');
mkdirSync(`artifacts/${platform}`, { recursive: true });
copyFileSync(binary, `artifacts/${platform}/stack`);
writeFileSync(`artifacts/${platform}/build-info.json`, JSON.stringify({ platform, commit, version, sha256 }) + '\n');
