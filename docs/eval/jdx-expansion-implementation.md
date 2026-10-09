# Jdx expansion implementation record

Status: all routes and remediation are integrated; Astra round 3 is in progress.

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
Linux ARM64 validation at the same implementation commit passed all ten real-tool scenarios,
including OCI, plus all 341 Rust tests and Clippy. It used mise 2026.9.18, fnox 1.39.0,
Pitchfork 2.29.0, Rust 1.98.1 for the Stack build, and Rust 1.93.1 / mbx 1.22.0 for the Rust
bundle smoke. Additional real probes passed cold fnox checksum rejection, v2 migration,
required policy, exact skill retrieval and sync, provider exclusion, and two-copy mbx cache
sharing. All provider state and Docker resources were isolated and this run's resources
were removed.

The original Linux image lacked system Python for scenario 9's listener fixture. After adding
a container-only link to the run's installed Python, that scenario passed. An initial mbx
recording shim lost Cargo's argv[0]; an independent bounded investigation established the
shim error and the corrected real-mise build controls passed. Root-run Rust fixture failures
and a timing-sensitive test were retained in the receipts; the final full unprivileged run
passed. These failed attempts are not product failures or passing-run evidence.

Real Redis 8.10.2 locked on all four requested platforms, correcting the design-time blanket
Redis example in current user docs. Requested-platform metadata coverage is separate from
installation: only Linux ARM64 and macOS ARM64 were executed. No remote CI run or four-platform
installation result is claimed. Linux report and complete receipts:
`/tmp/jdx-linux-e90a7fe.yYLzUp/REPORT.md`; argv[0] investigation:
`/tmp/jdx-argv0-research.OVp98d/REPORT.md`. `cargo fmt --check` reports formatting differences
in both inherited code and additions; it is not a repository CI gate.

## Review results

Astra round 1 reviews exact implementation commit `e90a7fe` against the DX base in a separate,
read-only checkout. The brief includes all four routes, skills sync, integration validation,
prior template and redaction findings, responses, and unresolved installation and sync
failure questions. Astra requested changes with five findings:

| Finding | Evidence | Remediation ownership |
|---|---|---|
| P1 service-version template boundary | Source-confirmed service validation and scratch-write gap; prior real-mise template behavior | Opus template-boundary worker |
| P2 skill links deleted on discovery failure | Repeated install with fake-provider query failure removed link and registry record | Opus skills worker |
| P2 required runtime platform checked only at install | Fake-provider fixture allowed exec, status, and inspect on an unlisted runtime | Opus artifact-policy worker |
| P2 provider gate after writing frozen compile | Fake mise 2026.9.15 refused install after replacing generated config | Opus artifact-policy worker |
| P2 changed options skip artifact reconciliation | Changed fnox identity kept the release and omitted a mise lock request | Opus artifact-policy worker |

The skills worker also aligns the MCP description with the documented parsed stdout/stderr
redaction guarantee. Public key-name metadata can coincide with a value; the current blanket
promise about all result fields is inaccurate. Astra's focused redaction probes found no
additional leak in the parsed streams.

All three remediation workers start from `32484f0` in isolated worktrees. They add regressions
and atomic commits before parent integration and a new Astra round. Review evidence is retained
at `/tmp/stack-astra-r1.3yPpUt/proofs.json` and `probe.py`. The first finding remains source-
confirmed rather than a claimed fresh end-to-end exploit reproduction.

Astra's independent narrow suites passed 327 tests. Its optional upstream investigation
returned no findings because the provider refused that research task. Backend/native-option
identity and libc/platform questions are unverified concerns, not established defects.

### Round 1 skills remediation

Worker commits `0f1b481` and `ca6b72d` were integrated as `3a223fe` and `8b4e221`.
Sync now distinguishes releases whose skills were established from ones a failed query,
missing install, or missing pin leaves unsettled. While any release is unsettled, intact
owned links absent from the available list remain owned and are reported under `preserved`.
Authoritative duplicate names can still be removed, available skills can still be linked,
and foreign/retargeted paths retain their existing protection. When discovery recovers,
stale owned links are pruned. A failed discovery with no other changes leaves registry bytes
unchanged.

The ownership registry has no per-tool attribution, so one unsettled release conservatively
defers removal of every otherwise stale link. This favors retained, reported links during
provider trouble; a later settled sync completes pruning. No registry-format migration was
added. MCP and user-documentation redaction promises now name parsed stdout/stderr and public
key metadata explicitly.

Parent integration checks passed 11 binary skills tests and 17 skills unit tests. The worker
also passed its complete 344-test Rust suite, Clippy, and 14 documentation tests. These new
failure-path results use the fake provider, including a replay of Astra's proof; no additional
real-tool smoke is claimed. Template and artifact-policy remediation remain in progress.

### Round 1 template remediation

Worker commit `fa40f2c` was integrated as `729d9a8`. Service versions and preset names reject
all three template delimiters during validation in projects, bundles, and overrides. The
shared scratch writer independently rejects templated tool names, versions, and option
strings before creating trusted configuration. Existing guards for edited locks remain.

Parent checks passed three binary boundary regressions, three scratch tests, and ten manifest
tests. The binary regressions cover project/bundle/override declarations, ordinary and update
compile, inspection and runtime commands, edited tool/service pins, and direct resolver calls.
They assert provider calls and publication do not occur. The worker passed its full 346-test
Rust suite and Clippy; these counts precede the separately integrated skills remediation.

Real mise 2026.10.3 controls in an isolated root evaluated a harmless sentinel template from
trusted tools configuration. The pre-fix Stack reproduction did not execute it on that mise
release: argument parsing rejected the templated request before configuration loading. The
fake-provider regression established the unsafe handoff, and the fixed real Stack run refused
it before any scratch configuration. The correction removes dependence on mise's parsing
order; no successful pre-fix end-to-end execution is claimed. Receipts:
`/tmp/jdx-template-real.OyU3sq/REPORT.md`.

Artifact-policy, provider-preflight, and changed-option reconciliation fixes are now integrated.
Astra round 2 reviews all remediation together.

### Round 1 artifact remediation and round 2 review

Worker commits `fa8ab73`, `c0a0adf`, and `8c6ea4e` were integrated as `aae779b`, `59a58ad`,
and `d8df870`. Required-policy runtime-platform checks now run in common frozen validation,
before provider calls. Install/up run provider requirements through a validated compile's
pre-write hook, before port/identity reservation and publication. Provider failures remain
install-step errors with no completed compile step. Changed options for the same exact release
trigger targeted reconciliation of verified entries. Ordinary compile retains committed
checksums/signers and reports upstream conflicts; a missing fresh answer is reported as
`retained`, not a completed comparison. Update accepts fresh commitments explicitly.

Worker regressions sweep CLI/MCP locked entry points, compare project and state files across
provider refusal, and cover option changes, request-only changes, duplicate tool/service pins,
retained entries, and failed locking. They fail on the relevant pre-fix code. The worker passed
its 349-test full Rust suite, Clippy, and Node tests. Its replay of Astra's proofs confirms
common platform refusal, unchanged config under an outdated provider, and targeted fnox locking.
These added proofs use fake providers; no real signer bypass or changed signer is claimed.
Evidence replay: `/tmp/jdx-artifacts-r1-probe.TztsE0`.

Astra round 2 reviews exact combined implementation `d8df870` against the approved DX base.
The new delegate request includes the complete original brief, first review findings, every
implementer's response, prior validation, and unresolved objections. Final combined Rust checks passed all 357 tests; Clippy and 41 Node tests passed too.
All ten macOS real-tool scenarios passed, including OCI, with an isolated local registry.
The normally optional real-mise configuration-isolation test also passed separately. Logs:
`/tmp/stack-jdx-r2-rust.log`, `stack-jdx-r2-clippy.log`, `stack-jdx-r2-node.log`,
`stack-jdx-r2-boundary.log`, and `stack-jdx-r2-native-all.log`. Astra's round 2 verdict and new findings are recorded below.

Linux ARM64 remediation validation at `d8df870` passed all 357 Rust tests and the release
build on its first invocation, with no transient or product failures. All 16 real-mise
remediation probes passed: skills links and registry bytes survived two provider failures,
discovery recovery restored the normal step, required/current install succeeded, and all
probed unlisted-platform commands refused without provider calls, command execution, or
project/state changes. Real native scenarios 1 and 10 passed against mise 2026.9.18 with
isolated tool and service state. The checkout was read-only and its owned containers/volume
were removed. Full report: `/tmp/jdx-linux-d8df870.9y_merg3/REPORT.md`.

This Linux rerun does not relabel the earlier complete ten-scenario run, checksum tamper,
migration, or mbx cache evidence as new-head runtime checks. It did not rerun Linux Clippy,
Node, or real old-mise/signer-change tests. MacOS Clippy/Node and the normally optional
real-mise boundary test passed at the combined head as recorded above. Round 2 review results are recorded below; further fixes and a fresh review are required.

### Astra round 2 findings

Astra requested changes at `d8df870`, confirming that all five first-round findings were
addressed and identifying two additional P2 issues:

| Finding | Evidence | Remediation ownership |
|---|---|---|
| Queued task uses grants from its old definition | CLI/MCP lock-wait race executes the new body with old grants | Opus task-grant planning worker |
| Effective provider platform and generated variants omitted | Primary mise source and captured real Bun lock show musl/baseline requirements; Stack discards generated variants | Opus platform worker with GPT-6.1-Sol high investigation |

The task worker will derive task body planning and grants from the same validated report
under the project lock. The platform worker will correct runtime coverage/policy selection
and preserve variants belonging to requested targets, while retaining the four default lock
targets and explicit checksum-update rule. Both start from `4c37137` in isolated worktrees.
A new Astra round will receive both previous reviews and implementer responses.

Astra independently passed 343 narrow tests and replayed the first-round proofs. Its upstream
investigation established libc and Bun CPU variant selection in mise 2026.9.18 and 2026.10.3.
These findings are closed installation refusals and inaccurate coverage claims, not a proven
checksum bypass. Native Alpine and non-AVX2 installations have not been run. The task race is
proven with fake providers, not a real-provider claim. Evidence:
`/tmp/stack-astra-r2.yzQ9UU/task-race/proof.json`, `task_race.py`, and
`bun-variants/proof.json`.

No separate native-option/backend defect or general template evaluation of tool-name keys
was established. Real outdated-provider and signer/trust-option installations remain outside
the saved runtime evidence.

### Round 2 task-grant remediation

Worker commit `d7e53cc` was integrated as `6a0a209`. Task planning now acquires the project
lock before selecting its command, all-services requirement, and secret list from the same
validated report used for environment, endpoint verification, fnox lookup, and execution
reservation. CLI and MCP use the same task planning API. Explicit command grants retain their
existing path. Deleted tasks fail during preflight without publishing config; removing a
task's grant runs the new definition without a fnox query.

Parent integration passed both CLI/MCP lock-wait regressions and the existing exact-grants
regression, three tests total. The first attempted filter matched zero tests; the named
regressions above were then run and are the passing evidence. The worker passed its full Rust
suite, Clippy, and documentation tests, and replayed Astra's proof at
`/tmp/taskgrant-replay.umPi`. These added regressions use fake providers; no real installation
claim is made.

A focused Sol high investigation is checking whether the generated task configuration can
still change after planning releases its lock and before mise loads the task. This is an
unverified related boundary concern. Platform-variant remediation is also still in progress.

The first post-planning investigation returned only a provider refusal for possible
cybersecurity risk, with no technical findings. A narrower local task-definition consistency
test has been delegated with harmless output and synthetic ordinary variables. The concern
remains unverified until that test or independent review supplies evidence.

### Post-planning task consistency finding

The bounded retry confirmed that a saved `ExecPlan` still reads the shared generated task
configuration at execution time. With real mise, a plan for a harmless `version-one` task
printed `version-two` after a separate thread normally compiled that new definition. The
same saved plan had printed `version-one` in its control run. The execution reservation
protects session lifetime and does not freeze generated configuration. The read-only source
checkout remained clean, and the test used no credentials or external services.

An additional Opus task will bind each invocation to immutable generated provider configuration
held by the execution plan. It must preserve project working-directory and config-relative
semantics, provider isolation, lock metadata where needed, and cleanup, without holding the
project lock through the long task or persisting granted values. This extends the earlier
queued-task correction to the confirmed post-planning window. Evidence:
`/var/folders/l3/6x2gqh2x1rbdkr21_hzckff40000gn/T/stack-task-config-proof-ep4ay0jl/README.md`.

### Round 2 platform remediation

Worker commits `355e6b6` and `64d3407` were integrated as `0fd873f` and `fe49035`.
Runtime coverage now uses the exact per-backend key mise selects: host libc qualifies Linux
keys, and Bun additionally selects its AVX2/baseline variant. Host detection follows mise's
os-release and dynamic-loader precedence rather than treating Stack's build target as host
libc. Unknown libc leaves URL coverage missing with a reason. Generated Bun variants belonging
to requested targets survive retention, seed, merge, update, and render; checksums stay attached
to their own keys. Required frozen operations validate actual runtime keys as well as listed
targets. The default four lock targets remain unchanged.

Parent checks passed eight simulated-host tests and six captured-real-lock variant tests.
The worker passed 376 Rust tests, Clippy, and 41 Node tests before the separately integrated
task changes. All six new variant regressions failed against old code. Sol high compared
primary mise 2026.9.18 and 2026.10.3 sources in `/tmp/jdx-platform-research-r2/REPORT.md`.
Real isolated macOS checks retained all generated Bun tables and exercised mise's own
platform-override dry runs, plus required Bun install/exec. These controls are not native
Alpine or non-AVX2 installation evidence. Receipts: `/tmp/jdx-platform-smoke-r2.GT4Jdy/`.

Detection parity assumes native execution on the same architecture and normal CPU feature
build settings. Unknown-libc fallback, mixed translated/native binaries, and custom AVX2
mise builds remain documented limits for the next review. `resolved_on` continues to identify
OS/architecture, while coverage identifies effective artifact keys. Snapshot remediation
remains in progress before final combined validation and Astra round 3.

### Immutable task configuration and round 3

Worker commit `aa153bf` was integrated as `06dc7c3`. Under the planning lock, each task copies
its generated config and rendered provider lock into a unique private cache directory. The
execution plan owns the snapshot until completion/error/timeout. Mise selects it as global
configuration with the project as configuration root, preserving working directory and
relative path behavior while excluding project, parent, user-global, and inherited selector
configuration. Snapshot files copy already-published configuration, never resolved granted
values. Normal cleanup removes the copy and its tracked-config links; subsequent task runs
reclaim copies left by dead processes. Explicit exec planning retains its existing behavior.

New public-API and CLI/MCP regressions cover saved plans across normal compile, task deletion,
grant/body binding, planning failure, terminal output, timeout cleanup, ownership/private
permissions, tracking-link cleanup, and absence of granted values on disk. The worker's
real-mise 2026.10.3 control preserves old task bodies across compile and checks project roots,
relative scripts, arguments, inherited config isolation, and cleanup. Those worker controls
had no pinned-tool/service/fnox integration; the parent's new native scenario 1 and 10 results
cover that combined path with real tools.

At exact implementation `06dc7c3`, parent full checks passed 386 Rust tests, Clippy, and 41 Node
tests. The optional real-mise configuration-isolation test and all ten macOS native scenarios
are running separately. Linux ARM64 validation is checking the updated tests and snapshots
with mise 2026.9.18. Astra round 3 receives the full original brief, both earlier reviews,
all implementer responses, the independent post-planning proof, and unresolved platform and
snapshot lifecycle limits. No sign-off is claimed yet.

Snapshot limits under review include its global/project config distinction, delayed cleanup
for killed parents, background commands that invoke mise after the owning task exits, private
`mise use` writes, and translated/custom CPU builds. Earlier head counts remain labeled by
commit and platform. Real snapshot controls: `/tmp/stack-task-config-real2.G2WV`.
