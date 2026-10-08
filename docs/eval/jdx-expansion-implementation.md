# Jdx expansion implementation record

Status: implementation in progress. Validation results below are updated as work finishes.

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

Implementation validation is pending.

### Foundations and mbx

Integrated as `29c1767`, from worker commit `e0574e2`.

- Integration checkout: `cargo test --locked --test compile` passed, 38 tests.
- Worker checkout: 242 Rust tests, Clippy, and 41 Node tests passed. The optional real-mise
  isolation test was run separately and passed.
- Real-tool results: two isolated Cargo worktrees reached mbx through exec and run. The
  second build recorded one cache hit. Native service scenarios 1 and 6 passed.
- Commands, versions, and limitations are recorded in
  [the route 2 smoke record](../reviews/2026-10-08-tool-options-mbx-smoke.md).

Artifact locking, secret grants, and both skills phases are in progress in separate worktrees.

## Review results

Pending.
