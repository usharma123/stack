// Promote a tested main artifact without rebuilding or repacking it.
import assert from 'node:assert/strict';
import { appendFileSync, readFileSync } from 'node:fs';
import { createHash } from 'node:crypto';
import { execFileSync } from 'node:child_process';
import { pathToFileURL } from 'node:url';

const repository = 'usharma123/stack';
const platforms = ['linux-x64', 'linux-arm64', 'darwin-x64', 'darwin-arm64'];

export function selectRun(runs, commit, tag) {
  const candidates = runs.filter(run => run.head_sha === commit &&
    ((run.head_branch === 'main' && run.event === 'push') ||
      (tag?.startsWith('v') && run.head_branch === tag && run.event === 'workflow_dispatch')) &&
    run.path === '.github/workflows/ci.yml' &&
    run.repository?.full_name === repository && run.head_repository?.full_name === repository);
  const run = candidates.sort((a, b) => b.id - a.id)[0];
  assert.ok(run, 'No eligible CI run exists for the tagged commit; run main CI or dispatch CI on the release tag');
  assert.ok(run.status === 'completed' && run.conclusion === 'success',
    'The latest eligible CI run for this commit must finish successfully before releasing');
  return run;
}

export function selectArtifact(artifacts, run, now = Date.now()) {
  const matches = artifacts.filter(artifact => artifact.name === 'npm-package');
  assert.equal(matches.length, 1, 'Expected exactly one npm-package artifact in the selected run');
  const artifact = matches[0];
  assert.ok(!artifact.expired && Date.parse(artifact.expires_at) > now, 'Tested artifact expired; dispatch ci.yml on the exact release tag');
  assert.equal(artifact.workflow_run?.id, run.id, 'Artifact belongs to another run');
  assert.equal(artifact.workflow_run?.head_sha, run.head_sha, 'Artifact belongs to another commit');
  assert.match(artifact.digest ?? '', /^sha256:[a-f0-9]{64}$/, 'Artifact must have a SHA-256 digest');
  assert.ok(Number.isSafeInteger(artifact.id) && artifact.id > 0, 'Invalid artifact ID');
  return artifact;
}

export function verifyPackage(file, { commit, version, tag }) {
  assert.equal(tag, `v${version}`, 'Release tag must match the checkout version');
  const read = name => execFileSync('tar', ['-xOf', file, `package/${name}`], { maxBuffer: 128 * 1024 * 1024 });
  const manifest = JSON.parse(read('package.json'));
  assert.equal(manifest.name, '@ushawarma/stack', 'Unexpected package name');
  assert.equal(manifest.version, version, 'Package version differs from checkout');
  const info = JSON.parse(read('build-info.json'));
  assert.equal(info.commit, commit, 'Package was built from a different commit');
  assert.equal(info.version, version, 'Build version differs from checkout');
  assert.deepEqual(Object.keys(info.hashes).sort(), [...platforms].sort(), 'Unexpected binary platform set');
  for (const platform of platforms) {
    assert.equal(createHash('sha256').update(read(`binaries/${platform}/stack`)).digest('hex'),
      info.hashes[platform], `Binary checksum mismatch: ${platform}`);
  }
}

function main() {
  const commit = execFileSync('git', ['rev-parse', 'HEAD'], { encoding: 'utf8' }).trim();
  const version = JSON.parse(readFileSync('npm/package.json')).version;
  const tag = process.env.GITHUB_REF_NAME;
  assert.equal(process.env.GITHUB_REPOSITORY, repository, 'Unexpected release repository');
  assert.equal(process.env.GITHUB_REF_TYPE, 'tag', 'Promotion requires a release tag');
  assert.equal(tag, `v${version}`, 'Release tag must match the checkout version');
  assert.equal(readFileSync('Cargo.toml', 'utf8').match(/^version = "([^"]+)"/m)?.[1], version,
    'Cargo and npm versions differ');
  if (process.argv[2] === 'select') {
    const api = endpoint => JSON.parse(execFileSync('gh', ['api', '--paginate', '--slurp', endpoint],
      { encoding: 'utf8', maxBuffer: 16 * 1024 * 1024 }));
    const runs = api(`repos/${repository}/actions/workflows/ci.yml/runs?head_sha=${commit}&per_page=100`)
      .flatMap(page => page.workflow_runs);
    const run = selectRun(runs, commit, tag);
    const artifacts = api(`repos/${repository}/actions/runs/${run.id}/artifacts?per_page=100`)
      .flatMap(page => page.artifacts);
    const artifact = selectArtifact(artifacts, run);
    appendFileSync(process.env.GITHUB_OUTPUT, `run-id=${run.id}\nartifact-id=${artifact.id}\n`);
    const summary = `Promoting tested CI run ${run.id}, commit ${commit}, artifact ${artifact.id}, ${artifact.digest}`;
    console.log(summary);
    if (process.env.GITHUB_STEP_SUMMARY) appendFileSync(process.env.GITHUB_STEP_SUMMARY, summary + '\n');
  } else if (process.argv[2] === 'verify') {
    assert.equal(process.argv.length, 4, 'Expected exactly one package tarball');
    verifyPackage(process.argv[3], { commit, version, tag });
    console.log(`Verified tested package ${version} from ${commit}`);
  } else {
    throw new Error('Usage: node scripts/promote.mjs select | verify <tarball>');
  }
}
if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) main();
