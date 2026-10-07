# npm releases

Stack ships as `@ushawarma/stack`, with the `stack` command. The npm tarball
contains four native binaries: macOS x64/arm64 and Linux x64/arm64. Installation
needs Node.js 22.14 or later, and macOS 13 or later or any Linux distribution (the Linux
binaries are statically linked with musl). Windows is unsupported. Rust is not needed to install the package.
Mise and its service dependencies are still required for the commands that use them.

A single package keeps publication atomic. Its larger download includes all four
binaries, so installation works without postinstall scripts or GitHub downloads,
including `npm install --ignore-scripts`.

## Checks

`ci.yml` runs on pull requests, main pushes, and manual dispatch. `validate.yml`
builds the artifact that releases promote and does the following:

- Runs locked Rust tests and Clippy on each supported OS and CPU.
- Builds with Rust 1.93.1 and executes each native binary to verify its version.
- Tests OCI publication, digest replay, moved-tag updates, and rejected uploads
  against a real Docker registry, with assertions and nonzero failure exits.
- Checks each artifact's commit, version, platform, and SHA-256 before packing.
- Installs the packed tarball on all four runners with scripts disabled and tests
  version/help, argument errors, compilation in a path with spaces, and locked replay.

Actions are pinned to commit SHAs. Builds use Cargo.lock and no shared caches.
Packaging starts as soon as native builds pass; independent OCI/service checks still
block the final `CI passed` result. Jobs have timeouts. PR runs can cancel older runs; releases cannot cancel an active
publication. Release runs queue instead of replacing pending releases. Set the
`CI passed` job as a required branch protection check.
The existing `tests/e2e/run.sh` service scenarios remain an additional manual
check; this pipeline does not claim full service lifecycle coverage on macOS.

## npm account setup

`@ushawarma/stack@0.1.0` was bootstrapped from the fully tested CI artifact.
Its trusted publisher is configured for the repository and workflow below.
The `v0.1.1` tag records a failed publication caused by a relative tarball path.
It remains unchanged. Version `0.1.2` includes the path fix and is prepared for
the first OIDC release.


The first package publication needs an authenticated npm maintainer. Once npm
recognizes the package, configure its trusted publisher in package settings:

- Owner: `usharma123`
- Repository: `stack`
- Workflow filename: `release.yml`
- Environment: leave blank
- Allow direct `npm publish`

See [npm trusted publishing](https://docs.npmjs.com/trusted-publishers/).
The workflow uses Node 24.16.0, whose npm supports OIDC, and grants `id-token: write`
only to the publish job. It does not store an npm token. Public releases require
provenance. Protect `main` and `v*` tags so only maintainers can initiate releases.

To bootstrap a new package, use a reviewed `npm-package` artifact from a successful
CI run. Extract the `.tgz` and publish it with an authenticated maintainer session:

```sh
npm login
npm publish ./ushawarma-stack-0.1.0.tgz --access public --ignore-scripts
```

That manual bootstrap does not carry CI provenance. Configure trusted publishing,
then bump the version before the first automated release. Do not rerun a tagged
release against that manually published version; the workflow rejects its missing
provenance.

## Release

1. Update the same version in `Cargo.toml`, the `stack` entry in `Cargo.lock`, and
   `npm/package.json`. Use a SemVer version, such as `0.1.1` or `0.2.0-beta.1`.
2. Merge the change into main and confirm `CI passed`.
3. Create and push an annotated tag matching the version:

   ```sh
   git tag -a v0.1.2 -m 'Release 0.1.2'
   git push origin v0.1.2
   ```

`release.yml` runs only when a `v*` tag is pushed. It selects the latest eligible
`ci.yml` run for that exact commit and requires the run to have completed
successfully. It downloads that run's immutable `npm-package` artifact by ID,
checks the artifact digest, then verifies the tarball's package name, version,
source commit and all four binary checksums. It does not rebuild or repack.
Eligible sources are main push runs and recovery runs manually dispatched on the
exact release tag. PR runs and dispatches on other refs are excluded.

It accepts only commits on `origin/main` in `usharma123/stack`, with a tag matching
the Rust/npm version. Only the publish job receives `id-token: write`; selection
and downloading use `actions: read`. The selection job records the source run,
commit, artifact ID and digest in its job summary.
Stable versions use `latest`; prereleases use `next`.
The publish script checks the immutable version first. An existing version is
accepted only if its SHA-512 integrity matches the exact packed tarball and it has
provenance. After publication it retries registry reads and tarball downloads for
propagation for up to 15 minutes, including request time. Retries back off from 10
to 60 seconds and log the elapsed time and last failure. Each request, including
its response body, has a 15-second timeout capped by the remaining budget.
Integrity or provenance mismatches and HTTP authentication failures stop immediately.
The publish step has a 22-minute timeout to also cover the bounded preflight and
three-minute npm publish command; the job has 30 minutes for setup and installation.
Verification waits for the package index used by npm install and checks the
downloaded tarball. A separate step performs a clean registry installation.

If publication failed or its response was lost, rerun the failed publish job to
reuse the original artifact. Never move a release tag or republish different bytes
under an existing version. Artifacts expire after 30 days. If selection fails because main CI is unfinished
or failed, fix or finish that run before rerunning the release. If its artifact
expired before publication, regenerate it with `gh workflow run ci.yml --ref vX.Y.Z`,
using the exact release tag. Wait for that complete CI run to pass, then rerun the
failed release selection job. This works even after GitHub's 30-day workflow-rerun
window has closed. The release still verifies that the tag's commit belongs to main
and that the package identity matches; this command does not publish anything.

A rebuild may produce different bytes. For an already published version, retain
and reuse the selected artifact; if it is unavailable, release a new version.
Fix authentication errors rather than repeatedly publishing. A newer failed or
unfinished eligible run for the commit blocks promotion even if an older run passed.

GitHub release concurrency uses `queue: max` to retain pending releases. Older
actionlint versions do not recognize this GitHub-supported field. For those
versions, use `actionlint -ignore '^unexpected key "queue" for "concurrency" section'`
to ignore only that schema mismatch.

## Local packaging check

Build `cargo build --locked --release`, then run
`node scripts/artifact.mjs darwin-arm64` on an Apple Silicon Mac, or the matching
platform on another supported machine. Collect the other native artifacts from
CI, place them under `artifacts/<platform>/`, then run:

```sh
node scripts/package.mjs
mkdir -p dist
npm pack ./npm --pack-destination dist
node scripts/smoke-install.mjs dist/*.tgz
```

Never label a cross-platform binary as a different platform to make assembly pass.
