// Prepare GitHub Release notes and checksummed tarball assets for a verified tag release.
import assert from 'node:assert/strict';
import { appendFileSync, copyFileSync, existsSync, mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { createHash } from 'node:crypto';
import { execFileSync } from 'node:child_process';
import path from 'node:path';
import { pathToFileURL } from 'node:url';

const repository = 'usharma123/stack';

export function previousTag(tags, currentTag) {
  return tags.map(t => t.trim()).filter(t => t && t !== currentTag && /^v\d/.test(t))[0] ?? null;
}

export function installSection(version) {
  return [
    '## Install',
    '',
    'Install with Node.js 22.14 or later:',
    '',
    '```sh',
    `npm install -g @ushawarma/stack@${version}`,
    'stack --version',
    '```',
    '',
    'Or pin in a project without running install scripts:',
    '',
    '```sh',
    `npm install --save-exact --ignore-scripts @ushawarma/stack@${version}`,
    './node_modules/.bin/stack --version',
    '```',
    '',
    'The package contains native binaries for macOS x64/arm64 and Linux x64/arm64 (static musl). Run `stack setup` if mise is not yet installed.',
  ].join('\n');
}

export function buildReleaseNotes({ tag, version, curated = null, generated = null, previous = null }) {
  assert.equal(tag, `v${version}`, 'Release tag must match the checkout version');
  const sections = [];
  if (curated && curated.trim()) {
    sections.push(curated.trim());
  } else if (generated && generated.trim()) {
    sections.push(generated.trim());
  } else {
    sections.push(`Release ${tag} of Stack.`);
  }
  const body = sections[0];
  if (!body.includes(`@ushawarma/stack@${version}`)) {
    sections.push(installSection(version));
  }
  if (previous && !body.includes(`/compare/${previous}...${tag}`)) {
    sections.push(`**Full Changelog**: https://github.com/${repository}/compare/${previous}...${tag}`);
  }
  return sections.join('\n\n') + '\n';
}

export function prepareReleaseAssets(tarball, outDir, {
  tag,
  version,
  curatedPath = path.join('docs', 'releases', `${tag}.md`),
  previous = null,
  generateNotes = null,
} = {}) {
  assert.equal(tag, `v${version}`, 'Release tag must match the checkout version');
  mkdirSync(outDir, { recursive: true });
  const assetName = `stack-${version}.tgz`;
  const assetPath = path.join(outDir, assetName);
  copyFileSync(tarball, assetPath);
  const sha256 = createHash('sha256').update(readFileSync(assetPath)).digest('hex');
  const sumsPath = path.join(outDir, 'SHA256SUMS');
  writeFileSync(sumsPath, `${sha256}  ${assetName}\n`);

  const curated = existsSync(curatedPath) ? readFileSync(curatedPath, 'utf8') : null;
  const generated = !curated && generateNotes ? generateNotes({ tag, previous }) : null;
  const notes = buildReleaseNotes({ tag, version, curated, generated, previous });
  const notesPath = path.join(outDir, 'notes.md');
  writeFileSync(notesPath, notes);
  return {
    assetName,
    assetPath,
    sumsPath,
    notesPath,
    sha256,
    prerelease: version.includes('-'),
    usedCurated: Boolean(curated && curated.trim()),
  };
}

function findPreviousTag(tag) {
  try {
    const out = execFileSync('git', ['tag', '--merged', 'HEAD', '--list', 'v*', '--sort=-v:refname'], {
      timeout: 30000,
      killSignal: 'SIGKILL',
      encoding: 'utf8',
    });
    return previousTag(out.split('\n'), tag);
  } catch {
    return null;
  }
}

function githubGenerateNotes({ tag, previous }) {
  const args = ['api', `repos/${repository}/releases/generate-notes`, '-f', `tag_name=${tag}`, '--jq', '.body'];
  if (previous) args.push('-f', `previous_tag_name=${previous}`);
  try {
    return execFileSync('gh', args, {
      timeout: 60000,
      killSignal: 'SIGKILL',
      encoding: 'utf8',
    });
  } catch {
    const range = previous ? `${previous}..HEAD` : 'HEAD';
    try {
      const log = execFileSync('git', ['log', '--no-merges', '--format=- %s (%h)', range], {
        timeout: 30000,
        killSignal: 'SIGKILL',
        encoding: 'utf8',
      }).trim();
      return log ? `## Changes\n\n${log}` : null;
    } catch {
      return null;
    }
  }
}

/** `name=value` lines for GITHUB_OUTPUT, each value one line, so none can add another output. */
export function outputLines(outputs) {
  return Object.entries(outputs).map(([name, value]) => {
    assert.match(name, /^[a-z][a-z0-9-]*$/, `Invalid output name ${name}`);
    assert.ok(!/[\r\n]/.test(String(value)), `Output ${name} spans lines`);
    return `${name}=${value}\n`;
  }).join('');
}

function main() {
  assert.equal(process.argv.length, 4, 'Usage: node scripts/release-notes.mjs <tarball> <out-dir>');
  const [, , tarball, outDir] = process.argv;
  const version = JSON.parse(readFileSync('npm/package.json', 'utf8')).version;
  const tag = process.env.GITHUB_REF_NAME;
  assert.equal(process.env.GITHUB_REPOSITORY, repository, 'Unexpected release repository');
  assert.equal(process.env.GITHUB_REF_TYPE, 'tag', 'Release notes require a release tag');
  assert.equal(tag, `v${version}`, 'Release tag must match the checkout version');
  const previous = findPreviousTag(tag);
  const prepared = prepareReleaseAssets(tarball, outDir, {
    tag,
    version,
    previous,
    generateNotes: githubGenerateNotes,
  });
  if (process.env.GITHUB_OUTPUT) {
    assert.equal(typeof prepared.prerelease, 'boolean');
    appendFileSync(process.env.GITHUB_OUTPUT, outputLines({
      title: `Stack ${tag}`, prerelease: prepared.prerelease,
      asset: prepared.assetPath, sums: prepared.sumsPath, notes: prepared.notesPath,
    }));
  }
  console.log(`Prepared GitHub release assets for ${tag} (${prepared.usedCurated ? 'curated' : 'generated'} notes, sha256=${prepared.sha256})`);
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) main();
