// Policies the workflows must keep, read from their text: what actionlint and zizmor do not
// enforce on their own, or what only this repository requires.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync, readdirSync } from 'node:fs';

const dir = '.github/workflows';
const workflows = Object.fromEntries(readdirSync(dir).filter(f => /\.ya?ml$/.test(f))
  .map(f => [f, readFileSync(`${dir}/${f}`, 'utf8')]));

// The `run:` scripts of a workflow, each with the line it starts on.
function runScripts(text) {
  const lines = text.split('\n');
  const scripts = [];
  for (let i = 0; i < lines.length; i++) {
    const m = lines[i].match(/^(\s*)(?:- )?run:\s?(.*)$/);
    if (!m) continue;
    const indent = m[1].length;
    const body = [m[2]];
    if (/^[|>][-+]?\s*$/.test(m[2])) {
      while (i + 1 < lines.length && (lines[i + 1].trim() === '' || lines[i + 1].match(/^\s*/)[0].length > indent)) body.push(lines[++i]);
    }
    scripts.push({ line: i + 1, text: body.join('\n') });
  }
  return scripts;
}

// The jobs of a workflow: name and text, from two-space keys under `jobs:`.
function jobs(text) {
  const after = text.split(/^jobs:\n/m)[1] ?? '';
  return after.split(/^(?=  [A-Za-z0-9_-]+:\s*$)/m).filter(j => /^  [A-Za-z0-9_-]+:/.test(j))
    .map(j => ({ name: j.match(/^  ([A-Za-z0-9_-]+):/)[1], text: j }));
}

test('every workflow starts with no permissions, and every job asks for its own', () => {
  for (const [file, text] of Object.entries(workflows)) {
    assert.match(text, /^permissions: \{\}$/m, `${file}: no top-level permissions: {}`);
    for (const job of jobs(text)) assert.match(job.text, /^    permissions:/m, `${file}: job ${job.name} declares no permissions`);
  }
});

test('only the release jobs that need them may write or mint tokens', () => {
  const allowed = { 'release.yml': { 'id-token: write': ['publish'], 'contents: write': ['github-release'] } };
  for (const [file, text] of Object.entries(workflows)) {
    for (const job of jobs(text)) {
      for (const grant of job.text.match(/^\s+[a-z-]+: write/gm) ?? []) {
        const scope = grant.trim();
        assert.ok(allowed[file]?.[scope]?.includes(job.name), `${file}: job ${job.name} is granted ${scope}`);
      }
    }
  }
});

test('every action is pinned by a full commit SHA, or is this repository at the running commit', () => {
  for (const [file, text] of Object.entries(workflows)) {
    for (const [, ref] of text.matchAll(/^\s*(?:- )?uses:\s*(\S+)/gm)) {
      assert.ok(/^\$\//.test(ref) || /^[\w.-]+\/[\w./-]+@[0-9a-f]{40}$/.test(ref), `${file}: uses ${ref}`);
    }
    for (const [line, ref] of text.matchAll(/^\s*(?:- )?uses:\s*([^\s$][^\s#]*@[0-9a-f]{40})(.*)$/gm)) {
      assert.match(line, /# v\d+(\.\d+)*$/, `${file}: ${ref} names no version tag in a comment`);
    }
  }
});

test('no run script interpolates an expression; values reach scripts through env', () => {
  for (const [file, text] of Object.entries(workflows)) {
    for (const script of runScripts(text)) {
      assert.ok(!script.text.includes('${{'), `${file}:${script.line}: expression in a run script`);
    }
  }
});

test('the services job installs the mise builds stack setup installs, by the same checksums', () => {
  const setup = readFileSync('src/setup.rs', 'utf8');
  const assets = Object.fromEntries([...setup.matchAll(/\("(linux|macos)", "(\w+)"\) => Some\(\("([\w-]+)", "([0-9a-f]{64})"\)\)/g)]
    .map(([, os, arch, asset, sha]) => [`${os === 'macos' ? 'darwin' : 'linux'}-${arch === 'x86_64' ? 'x64' : 'arm64'}`, { asset, sha }]));
  assert.equal(Object.keys(assets).length, 4, 'src/setup.rs names four mise builds');
  const services = jobs(workflows['validate.yml']).find(j => j.name === 'services').text;
  const rows = [...services.matchAll(/platform: ([\w-]+), mise: ([\w-]+), mise_sha256: ([0-9a-f]{64})/g)];
  assert.deepEqual(rows.map(r => r[1]).sort(), ['darwin-arm64', 'linux-arm64', 'linux-x64'], 'services platforms');
  for (const [, platform, mise, sha] of rows) {
    assert.equal(mise, assets[platform].asset, `${platform}: CI installs ${mise}, stack setup ${assets[platform].asset}`);
    assert.equal(sha, assets[platform].sha, `${platform}: checksum differs from stack setup's`);
    if (platform.startsWith('linux-')) assert.match(mise, /-musl$/, `${platform}: the static build`);
  }
});

test('the release keeps one ordered queue and promotes only the tested main artifact', () => {
  const release = workflows['release.yml'];
  assert.match(release, /^concurrency:\n  group: npm-release\n  queue: max\n  cancel-in-progress: false$/m);
  assert.match(release, /git merge-base --is-ancestor HEAD origin\/main/, 'the ancestry gate');
  assert.match(release, /node scripts\/promote\.mjs select/);
  assert.equal((release.match(/node scripts\/promote\.mjs verify dist\/\*\.tgz/g) ?? []).length, 2, 'both jobs verify the package');
  assert.equal((release.match(/digest-mismatch: error/g) ?? []).length, 2, 'both downloads refuse a digest mismatch');
  assert.doesNotMatch(release, /cargo (build|test)|npm pack \.\/npm/, 'the release never rebuilds');
});

test('the CI check that gates merging depends on all of validation', () => {
  const ci = workflows['ci.yml'];
  assert.match(ci, /uses: \$\/\.github\/workflows\/validate\.yml$/m);
  assert.match(ci, /needs: validate/);
});

test('the formal verification jobs run every skill the verifier knows', () => {
  const known = readFileSync('verification/check.sh', 'utf8').match(/\|\| skills=\(([^)]+)\)$/m)[1].split(/\s+/).filter(Boolean);
  const formal = jobs(workflows['validate.yml']).find(j => j.name === 'formal').text;
  const listed = formal.match(/skill: \[([^\]]+)\]/)[1].split(',').map(s => s.trim());
  assert.deepEqual(listed.sort(), known.sort());
  assert.match(formal, /STACK_REQUIRE_PSQL: '1'/);
});
