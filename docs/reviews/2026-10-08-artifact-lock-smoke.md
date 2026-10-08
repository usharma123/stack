# Artifact checksums in stack.lock: real-tool receipts

Route 1 of [the jdx expansion design](../design/jdx-expansion.md), run on 2026-10-08 on macOS
arm64 with mise 2026.10.3 (`/opt/homebrew/bin/mise`) and a debug build of this branch. Every
stack run used its own `MISE_DATA_DIR`, `MISE_CACHE_DIR`, `MISE_STATE_DIR`, `STACK_STATE_DIR`,
`STACK_CACHE_DIR` and `PITCHFORK_STATE_DIR` under `/tmp/stack-receipt.*`, so the user's tool
store was never read or changed; a wrapper `mise` first on `PATH` logged every call and ran the
real one. The project was `jq = "1.7.1"`, `fnox = "1.39.0"` and `[services.db] preset =
"postgres", version = "17"`, with the default `[lock] platforms`. Fake-provider tests are in
`tests/artifacts.rs`, `tests/runtime.rs` and `src/artifacts/tests.rs`; these receipts are what
only the real tools show.

| # | Command | Result | Boundary |
|---|---|---|---|
| 1 | `stack compile --json` | exit 0, 2.5 s; one `mise lock --platform macos-arm64,macos-x64,linux-x64,linux-arm64 fnox jq pitchfork postgres`; stack.lock version 3. jq `aqua:jqlang/jq`, fnox and pitchfork `packslip:github.com/...` (with `signer` and `repository_ids`), db `conda:postgresql` 17.11 with conda dependency records; every pin `verified`/`added` on all four platforms except pitchfork macos-x64 `missing` (mise publishes no artifact and gives no reason) | the run happened in `<STACK_CACHE_DIR>/lock/q-*`, empty afterwards; the project's directory was untouched until stack.lock was written |
| 2 | `stack compile --json` again | exit 0; stack.lock byte-identical; one `mise lock ... pitchfork` | pitchfork stays `missing` on macos-x64, so every ordinary compile asks mise again (design step 2 taken literally); the full-coverage tools were not re-locked |
| 3 | `stack install --json` | exit 0, 4 s; mise calls `version`, `trust`, `env --json`, `install --locked --yes --quiet jq fnox pitchfork postgres`; step detail `locked: [jq, fnox, pitchfork, postgres]`, `plain: []`, `artifacts.verified` all four | `.config/mise/mise.lock` rendered from stack.lock before the install; mise 2026.10.3 passed the 2026.9.16 gate |
| 4 | jq's macos-arm64 `checksum` in stack.lock replaced with `sha256:000…0`, jq still installed; `stack install --json` | exit 0, `ok: true`; mise ran `install --locked ... jq ...`; the rendered lock carried the wrong checksum | **warm skip**: a release already installed is reported installed and not re-checked (the documented boundary) |
| 5 | same stack.lock, `rm -rf <data>/installs/jq <data>/downloads/jq`; `stack install --json` | exit 1, `artifact_mismatch`: "mise refused a download for jq@1.7.1 on macos-arm64: it does not match stack.lock"; details `{ kind: checksum, name: jq@1.7.1, platform: macos-arm64, expected: sha256:000…0, actual: sha256:0bbe619e…62e8a, url: https://github.com/jqlang/jq/releases/download/jq-1.7.1/jq-macos-arm64 }`, then mise's output and the progress record | **cold mismatch**: nothing installed for jq (`<data>/installs` held fnox, pitchfork, postgres) |
| 6 | `stack compile --update --json` with the tampered lock | exit 0; jq macos-arm64 `change: artifact_changed`, `checksum_was: sha256:000…0`, `url_was` set; stack.lock equal to the pre-tamper one apart from `resolved_on` | committed checksums change only under `--update`, and the change is reported |
| 7 | `stack install --json` | exit 0; jq installed again through `install --locked` | |
| 8 | jq's linux-x64 checksum replaced with `sha256:111…1`, `[lock] platforms` extended with `windows-x64`; `stack compile --json` | exit 0; one `mise lock` for all four tools (each `missing` on windows-x64); jq linux-x64 `change: differs_upstream`, warning `artifacts.jq@1.7.1.linux-x64 differs upstream; run \`stack compile --update\` to accept`; stack.lock keeps `sha256:111…1`; windows-x64 entries `added` | an ordinary compile that re-locks a tool never replaces its committed value |
| 9 | `tests/e2e/native.sh target/debug/stack` (all scenarios) | 1, 2, 3, 4, 6, 7, 8 and 9 passed; 5 skipped (no `STACK_E2E_REGISTRY`) | real mise, Pitchfork, Postgres and Redis in an isolated HOME; redis (which mise cannot lock: "failed to solve redis-server") went through plain `mise install` |
| 10 | `STACK_TEST_MISE=$(which mise) STACK_TEST_BINARY=target/debug/stack node --test tests/provider-boundary.test.mjs` | pass | |

Direct mise observations in isolated roots (`mise lock`/`install` with the same isolation
variables, not through stack) that the implementation relies on:

- A seed `mise.lock` holding only `lockfile_version = 3` is accepted; `mise lock` of redis exits 1
  with `failed to resolve redis for <platform>: failed to solve redis-server for <platform>` per
  platform and still writes pitchfork's entries.
- Plain `mise install jq` (no `--locked`) with a tampered recorded checksum also fails
  "Checksum mismatch ... Expected/Actual" and installs nothing.
- With npm:prettier's `aube` reference stripped, `mise install --locked jq` succeeds and `mise
  install --locked npm:prettier` fails "has no embedded-aube dependency graph in the revision 2
  lockfile".
- `mise install postgres` names the daemon preset's version (17.6 here), and `mise install
  rust` keeps the configuration's options: naming a tool selects its configured releases.
- `mise uninstall jq@1.7.1` removed jq's entries from the lockfile beside the configuration.
- mise rejects two daemons of one preset at different versions ("use one version per tool").

Source facts from mise v2026.10.3 (research task `jdx-artifacts-research-mise-lock-r1`, GPT-6.1
Sol, read-only): the exact checksum, conda, signer and repository-identity refusal texts that
`artifact_mismatch` parses; the `supports_lockfile_url` overrides that make `core:rust`,
`core:swift`, `core:dotnet`, `asdf`, `cargo`, `gem`, `go`, `npm`, `pipx`/`pypi`, `spinel`, `ubi`
and custom vfox backend plugins exempt from the `--locked` URL check (plain `vfox:` is not);
lockfile version 3 first written by 2026.9.16; mise drops unknown fields when it rewrites a lock
(stack keeps committed entries itself); accepted platform names.

Observed side effect: mise records every scratch configuration it loads (resolution and
locking) under its state directory's `tracked-configs` as a symlink to a removed file. The
receipts ran under `/tmp`, a symlink on macOS, without the scratch-cache canonicalisation the
skills branch adds; nothing above `/tmp/stack-receipt.*` held a mise configuration.

Not run here: a signer or repository refusal (parsed from source formats, covered with fakes),
a mise older than 2026.9.16 (covered with a fake `mise version`), `artifacts = "required"`
against real mise, Linux, and npm or Python tools through `stack install`.
