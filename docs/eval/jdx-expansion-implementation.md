# Jdx expansion implementation record

Status: all four routes and skills sync are integrated. Independent review is in progress.

## Scope and base

The implementation branch is `feat/jdx-expansion`. It starts from DX remediation commit
`3b57b48c156a3bc173dcaac80fc7ecc8db6184fe` on `eval/dx-v0.1.18-20261008`, which includes
`origin/main` at `f6f80099108fb52421c0560102fe6d577f0997d1`. The design document from
`4352db9` was carried onto this base in `9b6d663`.

The user authorized all four routes and phase B skills sync, with the design defaults:
best-effort artifact locking on four platforms, opt-in secret grants, and rejection of secret
values shorter than eight bytes in captured mode. Delivery is local commits only.

The implementation does not include task caching, hk/usage integration, an umbrella MCP proxy,
secret files or leases, or forced tool reinstallation. It preserves the DX remediation base.

## Work boundaries

1. Foundations and mbx: canonical tool options, composition and overrides, exact-version
   resolution with options, reusable unique tools-only scratch roots, provider version gates,
   the `rust-mbx` example, tests, and user documentation.
2. Artifact locking: lossless embedded provider lock, checksum preservation and explicit
   update reporting, platform coverage and policy, v2 migration, install partitioning,
   mismatch errors, atomic failure behavior, and tests.
3. Secret grants: task and exec declarations, exact fnox executable binding, bounded protocol
   handling, endpoint protection, streaming output redaction, CLI/MCP parity, doctor, and tests.
4. Skills: discovery from locked versions, read-only inspection, bounded MCP retrieval,
   provider-skill exclusion, ownership-safe opt-in symlink sync, and tests.

The foundations land first. The other three routes use separate worktrees based on the
foundation commit. The combined implementation is validated before independent review.
Review findings are corrected and sent through another review round before completion.

## Acceptance checks

| Area | Required evidence |
|---|---|
| Tool options | String/table equivalence; option conflicts and overrides; invalid option/type errors; options preserved by resolution, lock identity and render; mbx wrapper reached by exec and run |
| Lock migration | v2 best-effort remains usable; compile emits v3; required rejects incomplete or unsupported coverage; unknown provider fields survive parsed-TOML round trips |
| Commitment preservation | Ordinary compile retains committed artifacts and shared dependency records; update records accepted differences and distinguishes skipped refreshes; missing references fail validation |
| Installation | Checked pins use locked installation; unsupported/missing pins use plain installation only under best-effort; mismatch fails; warm installs are described within the download-only guarantee |
| Secret protocol | Pinned executable and symlink checks; no raw fnox output forwarded; requested keys only; protected variables preserved; unsupported files/leases and malformed/oversized/timed-out responses handled |
| Captured output | Redaction across read chunks, truncation, overlapping values, UTF-8 context and timeout results; no sentinel values in persisted artifacts or machine output |
| Skills discovery | Fresh and stale generated configs give the same answer; project env templates never execute; concurrent inspections use unique scratch roots; failures remain warnings |
| Skills retrieval/sync | Version matching; provider exclusion; 64 KiB bound; duplicate names; traversal and symlink ancestry checks; real directories, foreign and retargeted links preserved |
| Existing behavior | Rust and Node checks plus real mise/Pitchfork service scenarios, preserving startup deadlines, isolation, ownership and endpoint withholding |

## Validation results

### Base before implementation

At `904cc6e`, on macOS arm64 with mise 2026.10.3:

| Check | Result |
|---|---|
| `cargo test --locked --all-targets` | Passed, 222 tests |
| `cargo clippy --locked --all-targets -- -D warnings` | Passed |
| `node --test tests/*.test.mjs` | Passed, 41 tests; one opt-in real-mise isolation test skipped |

These establish the inherited DX base. They do not validate the new jdx routes.

Combined validation is recorded below.

### Foundations and mbx

Integrated as `29c1767`, from worker commit `e0574e2`.

- Integration checkout: `cargo test --locked --test compile` passed, 38 tests.
- Worker checkout: 242 Rust tests, Clippy, and 41 Node tests passed. The optional real-mise
  isolation test was run separately and passed.
- Real-tool results: two isolated Cargo worktrees reached mbx through exec and run. The
  second build recorded one cache hit. Native service scenarios 1 and 6 passed.
- Commands, versions, and limitations are recorded in
  [the route 2 smoke record](../reviews/2026-10-08-tool-options-mbx-smoke.md).

### Skills discovery and sync

Worker commits `d3c85ff` through `be93cc3` were integrated as `a9bfb91` through `926b0ff`.

- Integration checkout: all 10 binary-level skills tests passed.
- The worker's Rust, Clippy, and Node checks passed. It also recorded real-tool discovery,
  MCP retrieval, install/sync, and tools-only up receipts in
  [the skills smoke record](../reviews/2026-10-08-skills-smoke.md).
- Real mise evaluated template expressions in tool option strings even in a tools-only
  scratch config. Tool parsing now rejects template syntax. Discovery also refuses
  templated lock entries, and the scratch helper resolves cache paths before setting mise's
  directory boundary. These findings were sent to both active route workers.

### Secret grants

Worker commits `0485ae5` through `bd057d5` were integrated as `b4114b3` through `5f68ca4`.
The integration fixture separates fnox-only release queries from skills discovery queries.

At `ae3f2b9`:

| Check | Result |
|---|---|
| `cargo test --locked --all-targets` | Passed, 291 tests |
| `cargo clippy --locked --all-targets -- -D warnings` | Passed before the fixture-only adjustment |
| `node --test tests/*.test.mjs` | Passed, 41 tests; one opt-in test skipped |
| `bash tests/e2e/native.sh target/debug/stack 10-secrets` | Passed on macOS arm64 with real mise 2026.10.3 and fnox 1.39.0 |

The real-fnox scenario covers task, exec, MCP, terminal output, unsupported short values,
missing/protected keys, malformed configuration, doctor, and a persisted-file sentinel sweep.
More receipts and boundaries are in
[the secret-grants smoke record](../reviews/2026-10-08-fnox-secrets-smoke.md).

A separate integration probe found that the redaction label could itself contain a granted
literal. The follow-up was integrated as `75e1c58` and `e90a7fe`. It uses an unnamed marker
when a label would reproduce a value and refuses captured grants when inserted text cannot
be made safe. It adds unit, seeded streaming, timeout, truncation, CLI, and MCP regressions.
Terminal output retains its existing behavior. Astra is reviewing these added restrictions
and the stated output boundary.

### Artifact locking

Worker commits `0034141`, `325e40e`, and `619adcc` were integrated as `2a1e7d5`, `77cf44b`,
and `23d0450`. Integration preserved the skills and secrets fields, discovery, steps,
warnings, and shared fake-provider behavior alongside the optional v3 lock report.

The worker recorded real v3 generation, unchanged ordinary compilation, cold checksum
mismatch rejection, warm-install behavior, and explicit update reporting in
[the artifact-lock smoke record](../reviews/2026-10-08-artifact-lock-smoke.md).

### Combined routes

At `e90a7fe`, on macOS arm64:

| Check | Result |
|---|---|
| `cargo test --locked --all-targets` | Passed, 341 tests |
| `cargo clippy --locked --all-targets -- -D warnings` | Passed |
| `node --test tests/*.test.mjs` | Passed, 41 tests; one opt-in real-provider test skipped |
| `git diff --check` | Passed |

The opt-in real-provider boundary test passed separately against mise 2026.10.3. The OCI
CI scenario also passed, including immutable replay, moved-tag updates, and upload rejection.
All nine runnable native scenarios passed with real mise, Pitchfork, fnox, Postgres, and Redis
in isolated state. The native OCI service scenario was skipped because that runner had no
registry; the separate OCI CI scenario above passed. The run also covered identity probes,
occupied-port recovery, locked install without a session, and hanging Redis readiness.
Linux real-tool validation is delegated to GPT-6.1-Sol with isolated containers. No remote
CI run or four-platform result is claimed. `cargo fmt --check` reports formatting differences
in both inherited code and additions; it is not a repository CI gate.

## Review results

Astra round 1 reviews exact implementation commit `e90a7fe` against the DX base in a separate,
read-only checkout. The brief includes all four routes, skills sync, integration validation,
prior template and redaction findings, responses, and unresolved installation and sync
failure questions. Its verdict and any subsequent remediation rounds are pending.
