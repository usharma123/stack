# npm releases

Stack ships as `@ushawarma/stack`, with the `stack` command. The npm tarball
contains four native binaries: macOS x64/arm64 and Linux x64/arm64. Installation
needs Node.js 22.14 or later, macOS 13 or later, or Linux with glibc 2.39 or later.
Windows and Alpine/musl are unsupported. Rust is not needed to install the package.
Mise and its service dependencies are still required for the commands that use them.

A single package keeps publication atomic. Its larger download includes all four
binaries, so installation works without postinstall scripts or GitHub downloads,
including `npm install --ignore-scripts`.

## Checks

`ci.yml` runs on pull requests, main pushes, and manual dispatch. `validate.yml`
is shared with releases and does the following:

- Runs locked Rust tests and Clippy on each supported OS and CPU.
- Builds with Rust 1.93.1 and executes each native binary to verify its version.
- Tests OCI publication, digest replay, moved-tag updates, and rejected uploads
  against a real Docker registry, with assertions and nonzero failure exits.
- Checks each artifact's commit, version, platform, and SHA-256 before packing.
- Installs the packed tarball on all four runners with scripts disabled and tests
  version/help, argument errors, compilation in a path with spaces, and locked replay.

Actions are pinned to commit SHAs. Builds use Cargo.lock and no shared caches.
Jobs have timeouts. PR runs can cancel older runs; releases cannot cancel an active
publication. Set the `CI passed` job as a required branch protection check.
The existing `tests/e2e/run.sh` service scenarios remain an additional manual
check; this pipeline does not claim full service lifecycle coverage on macOS.

## npm account setup

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
   git tag -a v0.1.1 -m 'Release 0.1.1'
   git push origin v0.1.1
   ```

`release.yml` repeats all checks before publishing. It accepts only commits on
`origin/main` in `usharma123/stack`, with a tag matching the Rust/npm version.
Stable versions use `latest`; prereleases use `next`.
The publish script checks the immutable version first. An existing version is
accepted only if its SHA-512 integrity matches the exact packed tarball and it has
provenance. After publication it retries registry reads and tarball downloads for
propagation, verifies their integrity, and performs a clean registry installation.

If publication failed or its response was lost, rerun the failed publish job to
reuse the original artifact. Never move a release tag or republish different bytes
under an existing version. A full rebuild may produce different bytes and will
fail the integrity check; release a new version in that case. Fix authentication
errors rather than repeatedly publishing. Artifacts expire after 30 days.

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
