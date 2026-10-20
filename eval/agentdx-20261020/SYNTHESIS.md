# Stack 0.1.22 as an agent product: synthesis and benchmark

Date: 2026-10-20. Source `8b3402fa1db8f11917b5ba33a333c9093545d9ce` (0.1.22). Benchmark binary:
`target/debug/stack` from `cargo build --locked`, sha256
`0ec996dc135eebecb188a0e191219ad88f3468fb2ad9f17f20aab181d8d917bb`. No product code was changed.

The question is what Stack lacks for coding agents, both local agents in worktrees and hosted
agents, to finish tasks with less human intervention. The evidence comes from three sources,
kept separate:

1. Two independent hands-on sessions: [Opus](opus/REPORT.md) and [Astra](astra/REPORT.md).
   Each is one tester writing and fixing their own apps, so they are discovery evidence, not
   measurement.
2. Scripted reproductions run while building the benchmark: `bench/regressions.py`, task
   validation, and harness self-tests.
3. The new matched benchmark in [`bench/`](../../bench/README.md). **It has not yet been run
   with a real agent**, so there are no agent completion numbers anywhere in this document.

## Prioritized findings

Ranked by how directly each one stops an agent from finishing without help: reliability first,
then agent friction, then workspace lifecycle. "Confirmed by" names who reproduced it.

### Reliability

| # | Finding | Effect on an agent | Confirmed by |
|---|---|---|---|
| R1 | **`stack run` replaces a custom service's `<NAME>_URL` with `https://<svc>.<project>.localhost`**, a host that does not resolve and a proxy Stack never starts. `stack exec` gets the right `http://127.0.0.1:<port>`. CLI and MCP `stack_run` alike. | The documented test loop fails against healthy services. An agent sees "all verified, tests fail" and debugs the app. | Opus (macOS, 0.1.21 and 0.1.22), Astra (macOS with two mise releases, Linux), benchmark validation of both service tasks, `regressions.py run-exec-url-parity` |
| R2 | **SIGTERM to `stack --json exec` leaves the command and its children running**, reparented to PID 1. Built-in `--timeout` does clean up. | A cancelled or timed-out agent harness leaks processes that hold ports and data. | Astra, parent rerun (`agentdx-parent-20261020/cancellation-r2`), `regressions.py external-sigterm-exec` |
| R3 | **The PostgreSQL cluster encoding follows the locale of the shell that first runs `up`.** With no `LANG`/`LC_*` (CI, `env -i`, many containers) it is `SQL_ASCII`, and psycopg then returns `bytes`. Nothing in `stack.lock` or `doctor` shows it, and it is fixed at first `up`. | The same commit passes locally and fails in a hosted run (Opus: 8/8 locally, 6/8 hosted). | Opus (macOS `env -i`), Astra (Linux), `regressions.py postgres-locale-independent`, benchmark ledger validation under `LC_ALL=C` |
| R4 | `stack run` resolves task secrets before installing the pinned fnox, contrary to the docs. | A fresh checkout's first secret task fails until the agent runs `stack install`. The hint is good. | Opus, `regressions.py secrets-before-install` |
| R5 | A cold mise can miss Stack's fixed 10 s `mise version` deadline. In the same fresh root, the next `mise version` took 4.8 s and the one after 0.02 s, probably mise's first-run update check (not verified). Stack then reports `provider_unavailable` with the hint "download mise", although mise is installed. | Flaky first command in a fresh hosted root; the hint sends the agent the wrong way. | This session, observed once (`agentdx-opus-impl-20261020/observation-mise-version-timeout`); latency-dependent, no deterministic reproducer |
| R6 | Dependents stay stale after a dependency restarts. There is no `depends_on`, and health checks the instance, not whether its dependencies work. | After a Postgres recovery the API stays "verified" with a dead connection; the agent must read logs and restart it by hand. | Opus (Postgres kill), Astra (API/worker versions); missing capability, not a broken promise |

### Agent friction

- **Error envelope gaps** (Opus P3-5):
  - `usage` errors have two exit codes (1 and 2), and some have no hint.
  - `session_busy` has no hint or details.
  - A truncated `logs` message.
  - MCP and CLI disagree on invalid secret names.
- **Misleading diagnostics** (Opus P3-6):
  - A SIGKILLed Postgres is reported as "process changed".
  - A never-ready service's `ready_cmd` failure is never named, and the hint suggests a pointless retry.
  - `healthy: true` is reported next to `lease_expired`.
- **No pre-start step for services.** Migrations and seeding have to be chained into the service's
  `run` string (`uv run python seed.py && ... && exec ...`). The benchmark's own Stack arm had to do
  the same.
- **Configuration edits invalidate the session even when no service changed** (Astra). The
  following commands fail with `service_unavailable` until `up`. This is documented, but it costs
  agents retries.
- **Watch reports but does not restart** (both). The agent must act on `changed_since_start`.
- **Docs gaps:**
  - no `fnox.toml` example,
  - `--require` takes one service per flag,
  - service data lives under `$MISE_STATE_DIR/daemons/<hash>`,
  - the isolated-run recipe pins 0.1.21, which still has the old `exec` URL bug.
- **No `outputSchema` on MCP tools; no `stack init`; no warning when `stack.lock` came from a
  different Stack version** (Opus found `stack 0.1.6` first on this host's PATH).
- **Root hosted runners:** PostgreSQL refuses root. The message is actionable, but hosted
  images that start as root need preparation (Astra).

### Workspace lifecycle

- **L1. Removing a checkout after a confirmed `down` leaks state** (Opus P2-4,
  `regressions.py removed-checkout-reclaim`):
  - Its port reservations stay in Stack's registry, and `gc` returns nothing.
  - Its service data stays under `$MISE_STATE_DIR/daemons/<hash>`.
  - No command reclaims either. On a host that churns agent worktrees, the 20000-29999 port
    range and the disk only grow.
- **L2. Recreating a checkout at the same path** quarantines the old data and starts empty, with a
  warning (Astra). This works as documented.
- **L3. A checkout deleted without `down`** gives `gc_incomplete` with exact inspect and stop
  commands. Following them works (Opus). Recovery still needs a human-like decision.
- **L4. Isolation between simultaneous checkouts held** in both sessions:
  - distinct ports, PIDs, data directories, identity tokens and seeded data;
  - concurrent `up` from two fresh worktrees got disjoint ports.

  These are single-tester observations, not concurrent independent agents.
- **L5. `stack down` fails in a fresh checkout that never ran `up`** (found while building
  the benchmark; `regressions.py down-before-up`). The checkout has `stack.toml` and the
  committed `stack.lock`, but not the generated provider config, so `down` fails with
  `provider_failed`: "mise daemons is experimental. Enable it with `mise settings
  experimental=true`". `run`, `install` and `up` regenerate the config; `down` does not. A
  worktree created and abandoned before any `up` therefore fails the documented Worktrunk
  `pre-remove = "stack down"` hook, and that hook failure aborts `wt remove`.
- **L6. After `stack down`, the root's Pitchfork supervisor keeps running by design.** For a
  custom service, a `pitchfork log-sink --daemon-id <project>/web` helper also stays until the
  supervisor stops (seen in harness self-tests, not for the preset services). The supervisor
  is shared infrastructure, but in per-agent roots nothing stops it unless the runner runs
  `stack exec -- pitchfork supervisor stop`. The benchmark reports these processes
  separately and does not count them as services the agent left running.

### Your workspace requirements (discussion only; nothing implemented)

You asked for collision prevention, a proper lifecycle, reclaiming the disk of stale worktrees,
and fast startup. You did not authorize a workspace, snapshot or worktree-management feature,
and none was built. The table records where the evidence stands and what the benchmark measures.

| Requirement | Evidence today | What the benchmark records | Gap |
|---|---|---|---|
| No collisions between simultaneous agents | Ports, data and identities were disjoint in both sessions (L4) | Each trial in its own root and checkout; grading checks service identity | No benchmark task runs two agents at once |
| A proper lifecycle (start, finish, cancel, dispose) | Works for normal `down`, lease expiry, owner-PID leases and recreated checkouts. Fails for external cancellation (R2), removal after `down` (L1) and `down` before any `up` (L5) | Whatever was left running at agent exit; cleanup outcome; harness reclaim | A disposal task family (interrupted agent, deleted checkout, unsaved work) is specified but not implemented |
| Reclaiming stale worktree disk | Not possible: data and port reservations outlive removal (L1) | `disk_after_down`: root, service data, tool installs, port reservations | No reclaim command exists to measure |
| Fast startup | Single cold timings only: Astra native up 10.8 s, Linux up 11.3 s, Opus hosted cold up 13 s | Setup and `up` seconds for every grading run, per arm and cache mode | Cold macOS installs here are dominated by the first execution of newly installed binaries. A sampled `postgres -V` sat in dyld's code-signature check while `syspolicyd` worked, and that hits both arms. One native cold ledger `up` took 381 s, the next 91 s. Cold timings on this host are noise, and no warm-start agent comparison exists yet |

Handing state from one worktree to another (database snapshot or clone) does not exist. Both
testers did handoff through code, git merge and migrations.

## The new benchmark

Design, in [bench/AGENTDX-PROTOCOL.md](../../bench/AGENTDX-PROTOCOL.md):

- **What is compared.** Matched pairs on the same task, model, agent command, prompt, pins,
  grader, locale and budget. One arm uses Stack; the other uses a project-written native setup
  with mise.
- **How a trial is scored.**
  - The plan is pre-registered with counterbalanced arm order.
  - Grading uses fresh state and hidden data, and exact check sets.
  - Cleanup is checked by runtime-root ownership.
  - A human review must bind the transcript, model and help.
  - Every assigned trial stays in the denominator.
- **What the old benchmark lacked.** The retired 26-tool benchmark had no agent at all; this
  one measures agent task completion.

### Corrections after independent review round 1

An independent review of `6175a0b` returned eleven findings, each with a reproducing
counterexample. The following changes targeted those findings. Round 2 at `6fc65e4` closed
eight and found residual cases in cleanup observations, command-tree cleanup and startup
metrics, plus a cache-directory failure in the current checkout. The round-3 corrections
below targeted those cases. Round 3 at `b3bf9b0` found four further gaps, recorded below;
round 4 at `2ac082e` confirmed those four counterexamples corrected and found two narrower
report-consistency gaps. The round-5 corrections below await a fresh independent review. The
historical 78-test passes describe their recorded runs, not the later cache-bearing checkout.

| # | Finding | Fix |
|---|---|---|
| 1 | A submitted `slug()` could print a forged all-true result from an exit handler | The grader decides every check from the values `slug()` returned; nonce-framed single result line; forgery, `process.exit` and prototype tampering tested |
| 2 | Cleanup re-checked a PID only before SIGTERM, not before SIGKILL | Identity and root ownership rechecked before every signal; pidfd on Linux |
| 3 | The report trusted the summary's cleanup and global-state fields | Each observation is its own hashed record; the report re-derives everything and rejects contradicting summaries |
| 4 | An app that failed to start made the trial invalid | `startup_failed` is a scored failure when a fresh reference control passes; otherwise `infrastructure_failed` |
| 5 | Model options were metadata only | Options must enter argv through `{option:NAME}`; contradicting literals refuse the plan; the review records observed options |
| 6 | Timeout cleanup waited for the leader only | The whole tree is stopped, including children that ignore SIGTERM, outlive the leader or leave its group |
| 7 | The existing smoke test did not gate grading | A pass needs the smoke test through `exec` and the hidden checks; the documented loop is observed only (R1) |
| 8 | A committed solution produced an empty submission patch | Diffed against a kept copy of the starting commit, including commits, deletions and ignored files |
| 9 | Negative, infinite or unsourced metrics were accepted | Agent timing from its receipt and window; usage finite, typed and bound to a file by sha256 |
| 10 | `regressions.py` exited 0 despite failed controls, cleanup or globals | Verdicts `fixed`/`reproduced`/`invalid` and exit 0/1/2; real `down-before-up` control |
| 11 | The docs claimed real trials lacked authorization | Corrected: real testing is authorized; the first run's configuration is what remains open |

### Corrections after independent review round 2

Round 2 at `6fc65e4` reproduced four remaining failures. The corrections below are
historical round-3 implementation. Independent review at `b3bf9b0` confirmed B, C and D,
and found A incomplete through the new findings below. That round tested code
`f42b8acf0486c999642ff278e9f2ab7709e54dfa`. Its later documentation did not change the
benchmark fingerprints recorded in `eval/results/agentdx-sol-round3-20261020/final/tested-code.json`.

| Finding | Correction and commits | Verification |
|---|---|---|
| A: hashed nulls, incomplete observations and earlier recovery excusing later survivors | `9f08637`, `f42b8ac`: typed schemas for required observations and command receipts; independently compare final cleanup; latest cleanup must establish recovery; unresolved command cleanup receives no completion credit | All seven original report probes now return invalid rows without publication or exceptions. Tests also cover wrong field types, missing command fields, boolean exit codes, internally consistent later failure and contradictory final records |
| B: a child executes a new program on SIGTERM and disappears from cleanup accounting | `21ec33f`: follow exec transitions only while the tree still establishes ownership; uncertain identities are listed as unresolved; never adopt a reused PID | Timeout and cancellation stop the exec child while unrelated sentinels survive. An escaped exec is reported unresolved and left unsignalled by command cleanup, then safely reclaimed by the bounded probe. PID-reuse and pidfd checks pass |
| C: missing, negative or string startup timing can be published or abort the report | `634d75b`, `f42b8ac`: derive reachable setup/up receipts from the arm and outcome; validate complete receipts, bounded integer times and step order; include every setup retry; malformed trials remain invalid rows | Missing receipt plus missing hash stays missing, never zero. Negative/string/boolean/nonfinite/out-of-window durations fail. Passing startup controls preserve scored failures; setup failures require no unreachable later step. Other assignments remain in the report |
| D: interpreter cache directories break hidden-data copying in this checkout | `dc623e3`: copy declared hidden files only. `d5212ff`: use the existing runtime-excluding overlay for scripted reference copies too | Existing caches under hidden and reference directories remain present. Cache-directory tests and both checkout and clean-export suites pass |

New raw evidence is under `eval/results/agentdx-sol-round3-20261020/`. Earlier evidence was
retained, including the inherited dirty-source snapshots and failed counterexamples.

- Final code: 101 offline tests pass in both the current checkout and a clean `git archive`
  export. See `final/unittest-checkout.log`, `final/unittest-clean-export.log` and
  `final/offline-results.json`. The earlier 99-test runs at `d5212ff` are retained too.
- Fourteen scripted trials at `d5212ff` exercise reference solutions in all three tasks and
  both arms, startup failures with passing controls, broken health with passing hidden checks,
  services left running, committed solutions and bound options. Two further reference catalog
  trials run at `f42b8ac`. All have the expected verdicts. These are harness self-tests with
  scripted transcript checks, not real agents or human reviews; all six reports are non-publishable.
- `final/report-rederivation.json` compares all six saved reports with derivation by the final
  code. `process/` retains raw timeout, cancellation and unresolved-identity receipts, with
  unrelated sentinels and post-probe cleanup checks. `final/review-replays/` retains fresh
  copies of the reviewer's report, pidfd and slug-noise probes and their results.
- `final/resource-audit.json` records the final scan of this round's retained roots and observed
  ports. `final/globals-after-all.json` and `final/collected.json` record the global comparison.
  Runtime roots and data are retained as evidence. As elsewhere, cleanup means none observed
  within the documented process-observation limits.
- `final/preservation.json` verifies all 275 archive blobs and working files against the
  original tree, both original reports unchanged, and no product-code changes since review.

No new hosted benchmark, real-agent result, productivity estimate or product fix is claimed.
The first real pilot still needs its concrete CLI/version, models/options, credential route
and spend configuration. The source-built Stack binary remains the one identified above.

### Corrections after independent review round 3

Astra's read-only review of `b3bf9b0` requested four corrections. Local implementation is
complete at code commit `fca4785`. Independent Astra review at `2ac082e` confirmed these
four counterexamples corrected, with the two residual gaps described below. The earlier
claim that unresolved command cleanup always prevented credit was too broad: teardown,
supervisor-stop and observation-only task-loop receipts had escaped that check.

| Finding | Correction | Local verification |
|---|---|---|
| 1. Known unresolved children disappear behind a later empty root scan | `f673792`: preserve uncertainty in recorded cleanup/grades and audit every invoked command role. `fca4785`: stopped commands require explicit terminated, killed, survivors and unresolved lists; timeouts require a stop observation | Actual escaped-exec down still records a live unresolved child and now reports unclean. Its synthetic scored rows are invalid with no verified/autonomous credit. Timeout/cancellation/exec and unrelated sentinels pass, and probe-owned children are reclaimed |
| 2. Teardown changes source after capture, and grading reads the changed bytes | `f7909d2`: retain a read-only `submitted-tree/` outside the agent workspace, derive the patch/hashes from it, and grade that tree. Bind the graded overlay manifest to the capture, preserve deletions/commits/ignored files and protected exclusions, and never read external symlink targets | Actual Git/teardown/overlay/grader probe and a full offline trial test retain the incorrect slug despite teardown writing the correct slug. Graded and captured hashes agree and the task fails. Host tools replace provisioning only; this is no real-agent result |
| 3. Grading/control cleanup accepts an earlier empty observation before later survivors | `5883d6a`: enforce latest observations for every runtime and reject reclaim after an already empty observation. Grading/control reclaim remains invalid even after recovery | Both reviewer counterexamples and agent/final variants are invalid. Agent cleanup that legitimately requires successful harness reclaim remains valid but non-autonomous |
| 4. A verifier receipt selects agent stdout | `5d222a8`: require the expected receipt role/stem and exact stdout/stderr digests for every command, including controls. Require the observed task-loop and cleanup command receipts | Redirected verifier output is rejected even when both files are independently hashed. Wrong/missing digests, removed receipts and out-of-window observation timing are invalid; valid failed-start controls remain scored failures |

Evidence is retained under `eval/results/agentdx-sol-round4-20261020/`:

- `final/tested-code.json`, `final/unittest-checkout.log` and `final/unittest-clean-export.log`
  bind 115 passing tests in each checkout to committed `fca4785`. Existing hidden/reference
  caches remain intact. Earlier 114-test runs at `f7909d2` and failing-before logs are retained.
- `review-replays/` contains the bounded reviewer process, submission, record-binding, latest
  cleanup, prior malformed-record, pidfd and slug-noise probes. `replay-provenance.json` records
  original and adapted script hashes; output paths, corrected assertions and retained-tree
  grading are the adaptations. All six malformed edge scenarios are invalid; the synthetic
  valid-record control remains valid. This is record validation, not agent measurement.
- Ten fresh scripted trials at `f7909d2` cover all six reference cells, two failed starts with
  passing controls, and two committed solutions with bound options. Final code re-derives
  their three reports identically. A further reference slug pair exercises final code. All
  reports remain non-publishable harness self-tests with scripted transcript checks.
- `report-rederivation.json` also re-derives the seven round-2 and six round-3 saved reports.
  Their original files remain unchanged. The current stricter submission schema marks their
  rows invalid because they lack a retained submitted tree; historical passes describe their
  original harness version and cannot be promoted into current valid trials.
- `resource-audit.json`, `globals-after-all.json`, `collected.json` and `preservation.json`
  record scoped cleanup, unchanged global receipts, the unchanged binary, all 275 original
  archive blobs/working files and both unchanged hands-on reports. Runtime roots, snapshots,
  service data and caches are retained. Process checks establish none observed within the
  existing platform limits.

No cold service matrix was repeated: historical product findings and the C-locale ledger
limitation are unchanged. No product source, worktree/snapshot feature, hosted benchmark,
controlled real-agent trial or productivity claim was added. Real testing remains authorized;
the concrete first-pilot configuration remains open.

### Corrections after independent review round 4

Astra's read-only review of `2ac082e` found two P2 report-consistency gaps using synthetic,
rehashed contradictory records. The current recorder already rejected those situations;
report derivation could still publish them. They were not observed runtime leaks or sandbox
exploits. Round-5 implementation is complete at `24a04bb`; fresh independent review is pending.

- `bd968fa` binds grade and reference-control uncertainty maps to their own command receipts,
  using the same consistency check as teardown maps. Malformed entries, wrong roles, omitted
  uncertainty and contradictory stop observations invalidate the row. A startup control on
  an unreachable grading outcome is invalid too. Clean controls and ordinary nonzero down
  with certain cleanup retain their previous behavior.
- `24a04bb` derives cancellation from every invoked receipt. A cancelled command cannot
  belong to a completed trial, even with a clean stop and no summary cancellation marker.
  New receipts distinguish cancellation before launch from cancellation after starting a
  child. Only the former needs no stop observation. Budget timeout remains separately scored.

Evidence is retained under `eval/results/agentdx-sol-round5-20261020/`:

- `final/tested-code.json`, both `final/unittest-*.log` files and `final/bench-code.tar` bind
  124 passing tests in each checkout to committed `24a04bb`. Existing caches remain intact.
  Failing-before logs retain 22 map-consistency failures and 34 cancellation failures.
- `before/remaining-probes/` and `after/remaining-probes/` retain the seven exact reviewer
  counterexamples and clean baseline. All seven formerly publishable cases now invalidate
  without verified or autonomous credit. The baseline remains valid. The unchanged reviewer
  matrix keeps both clean controls and rejects all 89 malformed cases across 91 scenarios.
  Script hashes and path provenance are in `replay-provenance.json`.
- Real bounded timeout, cancellation and escaped-exec probes retain their receipts under
  `after/process/`. Certain cleanup removes the exec child; escaped identity remains explicitly
  unresolved and is safely reclaimed by the probe. Sentinels survive. Linux pidfd checks are
  mocked on this macOS host; slug noise and forgery checks use real Node.
- Two fresh reference slug trials pass. Two further scripted trials externally cancel the
  runner, stop their agent and descendant, preserve a sentinel and remain invalid. These
  harness self-tests are nonpublishable; their transcript checks are scripted, not human reviews.
- `report-rederivation.json` confirms all four round-4 reports, covering 12 scripted trials,
  still derive identically. Seven round-2 plus six round-3 reports remain invalid for missing
  retained submission trees. Their saved evidence is unchanged.
- `preservation.json` verifies all 275 archive blobs and working files, both original reports,
  product boundaries, the tested export bytes and unchanged binary. `resource-audit.json`
  records nine retained new roots with no observed owned process or previously observed
  listener. All 70 recorded round-3/4 roots still exist, a presence check only.
  `collected.json` records unchanged global-state hashes. No private config contents are saved.

No controlled real-agent or hosted measurement ran, no long service matrix was repeated, and
no product or workspace feature changed. The existing platform, sandbox and pilot-configuration
limits remain. The frozen checkpoint records exact heads, commands, retained wrapper failures
and the next independent review step.

### Benchmark validation

`run.py validate` runs the starting code and the reference solution through the same grading
path agent work uses. Each run is cold, with a fresh root, under the locale shown.

Round-2 run at `59744d0`
(`eval/results/agentdx-opus-round2-20261020/`, script `round2.sh`). Unit tests, validation,
regression checks and the self-tests all ran against this one committed head, after the host
had rebooted. The pass now also needs the smoke test through `exec`.

| Cell | Locale | Valid | No-op fails its check (failed checks) | Reference passes both gates | Documented test loop before/after | Cleanup, globals | Reference setup + up (s) |
|---|---|---|---|---|---|---|---|
| slug / stack | UTF-8 | yes | yes (5 of 9) | yes | pass/pass | clean, unchanged | 6.0 |
| slug / existing | UTF-8 | yes | yes (5 of 9) | yes | pass/pass | clean, unchanged | 3.5 |
| catalog / stack | UTF-8 | yes | yes (4 of 5) | yes | **fail/fail (R1)** | clean, unchanged | 3.9 + 3.1 |
| catalog / existing | UTF-8 | yes | yes (4 of 5) | yes | pass/pass | clean, unchanged | 4.2 + 0.3 |
| ledger / stack | UTF-8 | yes | yes (6 of 8) | yes | **fail/fail (R1)** | clean, unchanged | 4.3 + 12.6 |
| ledger / existing | UTF-8 | yes | yes (6 of 8) | yes | pass/pass | clean, unchanged | 9.2 + 7.6 |
| ledger / stack | `LC_ALL=C` | **no** | — (`up` fails: `startup_failed` for both) | — | — | clean, unchanged | R3, as before |
| ledger / existing | `LC_ALL=C` | yes | yes (6 of 8) | yes | pass/pass | clean, unchanged | 6.5 + 8.2 |

The round-1 run below is kept unchanged. The two runs agree cell for cell.

Round-1 run at `17d8f2f` (`eval/results/agentdx-opus-impl-20261020/final3/`). Unit tests,
validation, regression checks and the self-test all ran against this one committed head.
The earlier final runs (`final/` at `b462162`, `final2/` at `b8296cd`) are kept. They differ
only in the regression-check fixes described below.

| Cell | Locale | Valid | No-op fails its check | Reference passes | Smoke via exec before/after | Documented test loop before/after | Cleanup | Reference setup + up (s) |
|---|---|---|---|---|---|---|---|---|
| slug / stack | `LANG=en_US.UTF-8` | yes | yes (5 of 9 checks fail) | yes | pass/pass | pass/pass | clean | 3.6 |
| slug / existing | UTF-8 | yes | yes | yes | pass/pass | pass/pass | clean | 3.6 |
| catalog / stack | UTF-8 | yes | yes (4 of 5 fail) | yes | pass/pass | **fail/fail (R1: `ENOTFOUND web.app.localhost`)** | clean | 3.5 + 2.8 |
| catalog / existing | UTF-8 | yes | yes | yes | pass/pass | pass/pass | clean | 3.6 + 0.3 |
| ledger / stack | UTF-8 | yes | yes (6 of 8 fail) | yes | pass/pass | **fail/fail (R1)** | clean | 4.5 + 9.1 |
| ledger / existing | UTF-8 | yes | yes | yes | pass/pass | pass/pass | clean | 6.5 + 7.2 |
| ledger / stack | `LC_ALL=C` | **no** | — | — | — | — | clean | `up` fails: the cluster is `SQL_ASCII`, and the grader's own seed (`Zoë Café`) raises `UnicodeEncodeError` (R3) |
| ledger / existing | `LC_ALL=C` | yes | yes | yes | pass/pass | pass/pass | clean | 7.6 + 7.0 |

Every run left no owned process and no changed global receipt. Reading the table:

- **R1 in the Stack arm.** The documented `stack run test` fails in both service apps, while the
  same smoke test passes through `stack exec`. The cells stay valid, because the reference
  passes. An agent in the Stack arm meets this defect and has to find the `exec` workaround.
  That is exactly the friction the experiment should measure.
- **R3 makes one cell unwinnable.** Ledger/Stack under `LC_ALL=C` cannot be completed at all on
  0.1.22, so it must not be planned until R3 is fixed. Its receipt is the defect evidence. The
  native arm passes there because its `dev.py` runs `initdb -E UTF8`.
- **Timings are not an arm comparison.** Every root was cold, yet these runs were far faster
  than the first cold runs earlier in the day. The development runs retained in
  `agentdx-opus-impl-20261020/validate-*-r1` had a native ledger `up` of 381 s and then 91 s.
  macOS has probably already checked identical binaries by now, but that cause is not
  verified. The numbers describe this host only.

Earlier attempts are kept, not replaced:

- `validate-ledger-stack-r1` was blocked by a DNS outage during `mise install`.
- The parent's first native ledger attempt failed on the wrong backend `conda:redis` (now
  `conda:redis-server`, as Stack locks it).
- The parent's `validate-*` runs used the previous harness.

### Harness self-test

**Round 2** (`agentdx-opus-round2-20261020/selftest/`, warm cache). Seven `harness_selftest`
plans ran 18 trials. The reviews take the observed model and options from the scripted
agent's transcript, not from the plan. Every trial's own derivation and every report found
no problems, and no global receipt changed:

| Plan | Scripted agent | Result in both arms |
|---|---|---|
| `reference` (3 tasks) | Reference, then `down` | passed, autonomous; the Stack ledger and catalog trials flagged `supervisor_left` (L6) |
| `leave-running` (catalog) | Reference, services left up | passed, not autonomous (left running) |
| `noop` (slug) | Nothing | failed |
| `commit` (slug) | Reference committed in the agent's checkout | passed; `changed: M slug.mjs`, one agent commit (finding 8 end to end) |
| `broken-start` (catalog) | App exits at startup | failed as `startup_failed`, reference control passed; scored, in the pair (finding 4) |
| `break-health` (ledger) | Reference with `/health` broken | hidden checks all pass, smoke test fails, so the task fails (finding 7) |
| `options` (slug, `effort=high`) | Reference; records the options it received | passed; agent argv carries `effort=high`; report shows controlled options (finding 5) |

Every report is `publishable: false` because the plans are self-tests. The failed start took
60.1 s in the native arm, where `dev.py up` waits 60 s for readiness, and 1.2 s in the Stack arm.

**Round 1.** Three `harness_selftest` plans ran through `plan`, `trial`, `review-template` and `report`
with `bench/selftest_agent.py` as the agent, on a warm cache
(`agentdx-opus-impl-20261020/selftest-final3-selftest/`). The reviews say they are a harness self-check, not a
human transcript review. The self-test validates the plumbing, not agents:

- **`reference`, all 6 cells.** The scripted agent applies the reference solution and follows
  the README. Every trial is valid, verified and counted autonomous.
  - In the Stack arm, the agent's own `stack run test` failed for both service apps (R1), but
    grading through `exec` passed.
  - Two Stack trials were flagged `supervisor_left`, which is expected after `stack down` (L6).
- **`reference-leave-running`, catalog.** The agent starts the app and never stops it. Both arms
  record the service as left running, so neither trial counts as autonomous, and cleanup
  removes it.
- **`noop`, slug.** Both trials are valid and fail the task.
- **Not publishable.** Every report has `publishable: false`, because the plans are self-tests.
- **Cancellation.** An earlier self-test (`selftest-r1`) was cancelled by SIGTERM in the
  middle of a trial. It left no process behind, and it showed that summaries needed a
  `cancelled` marker, which was then added.

### Regression checks on 0.1.22

**Round 2** at `59744d0` (`agentdx-opus-round2-20261020/regressions/`): every check
`reproduced`, with its control passed and its cleanup clean. No global receipt changed, and
the run exited 1 (valid, defects present). `down-before-up` now has a real control: a sibling
checkout ran `compile`, `up` and a confirmed `down`, all exit 0. Its new observation
localizes L5. In a checkout that still has its generated `.config`, `down` before any `up`
succeeds (`confirmed: true`). So the failure comes from a fresh clone missing the generated
provider config, where mise reports `mise daemons is experimental`. Other observations match
round 1 (exec `http://127.0.0.1:27988` vs run `https://web.urlparity.localhost`; `SQL_ASCII`
under `LC_ALL=C`; one reservation and 28 KB kept after removal).

**Round 1** at `17d8f2f` (`final3/regressions/`). Every check failed on 0.1.22,
reproducing its finding, while its control passed. Cleanup was confirmed for all of them, and
no global receipt changed.

| Check | Finding | Observed on 0.1.22 | Control |
|---|---|---|---|
| `run-exec-url-parity` | R1 | `exec`: `http://127.0.0.1:24741`; `run`: `https://web.urlparity.localhost` | `exec` gives loopback |
| `postgres-locale-independent` | R3 | `LC_ALL=C` → `SQL_ASCII`; `LANG=en_US.UTF-8` → `UTF8` | UTF-8 locale gives `UTF8` |
| `external-sigterm-exec` | R2 | Stack exits -15; the command and its grandchild are still alive | Unrelated sentinel survives |
| `removed-checkout-reclaim` | L1 | After a confirmed `down`, deleting the checkout and `gc`: 1 port reservation kept, 28 KB of service data kept, `gc` reclaimed nothing | `down` confirmed, `gc` exit 0 |
| `down-before-up` | L5 | `down` exit 1 `provider_failed` ("mise daemons is experimental") | None in round 1 (hard-coded); round 2 adds a sibling checkout that runs `up` and then a confirmed `down` |
| `secrets-before-install` | R4 | `secret_unavailable`: "fnox is not on the stack's PATH" | Passes after `stack install` |

Two harness mistakes in these checks were found and fixed before the final run. Their runs
are kept:

- **`final/`: cleanup in two checks.** The removed-checkout check left its supervisor (no
  checkout remained to stop it from, so the harness reclaimed it by PID). The `down-before-up`
  check counted the product's failing `down` as unclean.
- **`final2/`: a false pass.** `secrets-before-install` passed because a shared cache already
  held fnox. It now uses its own cold store and asserts that fnox is absent first.

## Coverage

| Area | Hands-on (Opus / Astra) | Benchmark agent task | Scripted check | Not covered |
|---|---|---|---|---|
| App change with migration + cache | Both | `ledger-note-cache` | Validation | Larger, real repositories |
| API + storage change | Astra | `catalog-sku-uniqueness` | Validation | |
| Tools-only change | Both | `slug-unicode` | Validation | Rust, Go |
| Documented task loop (`stack run`) | Both (R1) | Implicit in both service tasks | `run-exec-url-parity` | |
| Locale / encoding | Both (R3) | Plan `locale` variable | `postgres-locale-independent`, ledger under `LC_ALL=C` | Locale-normalized hosted images |
| External cancellation | Astra (R2) | Harness stops an agent's whole process tree on budget expiry or cancellation | `external-sigterm-exec` | MCP server cancellation; other signals |
| Secrets | Both (fake keys) | | `secrets-before-install` | Real providers; secrets as an agent task |
| Removal / reclaim | Opus (L1), Astra (recreate) | Disk and port receipts per run | `removed-checkout-reclaim`, `down-before-up` | Disposal agent task family |
| Concurrent worktrees | Both | | | Two concurrent agents |
| Service failure and recovery | Both | | | Fault-injection agent task |
| Dependents after restart | Both (R6) | | Astra's `dependents.py` (manual) | Agent task |
| Watch / restart | Both | Grading restarts apps from scratch | | Agent task |
| Lease / gc | Both | | | Agent task |
| MCP | Both (stdio) | Agent may use `stack mcp`; not configured by the runner | | Real MCP client registration |
| Hosted Linux | Astra (Docker LinuxKit arm64) | | | Hosted vendor, Linux x64, network policy |
| Hosted-style macOS (`env -i`) | Opus | Plan locale `LC_ALL=C` approximates it | | |
| Published npm 0.1.22 | Blocked: unpublished during both sessions | | | Everything here used a source build or local pack |

## Limitations

- **No agent results.** No controlled agent trial has been run, so there are no completion,
  time or cost numbers and no statistical claims. Real testing is authorized. What is unresolved
  is the configuration of a first bounded pilot: the agent CLI and its version, the model IDs and
  options, how its credentials reach a trial, and the spend limit. This checkout has not
  verified any of those.
- **No hosted Linux run of the benchmark.** Hosted evidence is limited to Astra's LinuxKit
  container and Opus's `env -i` shell from the hands-on sessions.
- Three small synthetic tasks on one Mac. The existing arm is glue written for the benchmark,
  not a real team's setup, and Compose/devcontainer control arms are not implemented.
- The benchmark computes no confidence intervals. Five pairs per cell is diagnostic only.
- Graders sit in this repository. Agents are kept out by convention, not by a sandbox. Within
  that convention, graders decide every check from what they observe, so submitted code cannot
  print its own pass. Editing the benchmark files is detected by re-hashing them after grading;
  reading the grader's memory is not defended against.
- On macOS, cleanup identifies a process by its `lstart` (one-second resolution) and command
  line, and a few microseconds separate the last check from the signal. Linux uses a pidfd and
  the kernel start tick.
- Runtime roots live under `/tmp`, which macOS clears on reboot. The 143 roots from the first
  implementation runs, listed in `agentdx-opus-impl-20261020/runtime-roots.json`, were gone after
  the host rebooted at 11:22 on 2026-10-20, before round 2 began. Their receipts and summaries
  in `eval/results/` are intact.
- Cleanup can only report "none observed". A process that detached without its command line,
  working directory or an open file under the trial root would be missed.
- Global receipts cover the user's mise, Pitchfork and Stack files and the mise install
  listing. Other programs can change them, so a difference must be inspected.

## Decisions pending for you

1. **Configure the first real pilot.** Real testing is authorized; choose the agent CLI(s)
   and version, model IDs and options, and the spend limit, and decide how the CLI's
   credentials reach a trial. The harness keeps `HOME` and isolates `XDG_CONFIG_HOME`, so a
   CLI that keeps credentials there needs a wrapper. A bounded first run is one model on the
   three tasks with two repetitions (12 trials).
2. **Add a Compose or devcontainer control arm** alongside the native-script one?
3. **A hosted Linux runner** for the same plan, instead of extrapolating from this Mac.

## Recommendations (product work, not done here)

In priority order:

1. Make `stack run` and MCP `stack_run` export the same verified loopback URLs as `exec` (R1).
   Add a test that compares the task and exec environments and makes a real request.
2. Forward SIGTERM, SIGHUP and SIGINT to the captured command's process group in `exec` and `run`, then
   wait (R2). Test the command, its grandchild and an unrelated sentinel.
3. Pin PostgreSQL `initdb` to UTF8 with a deterministic locale, or record the encoding in the
   session and fail `doctor` on a mismatch (R3).
4. Install the pinned fnox before resolving secrets (R4). Give `mise version` a longer or
   retried deadline on cold roots, with an accurate hint (R5).
5. Reclaim removed checkouts' port reservations in `gc`, and offer an explicit data reclaim
   with the same inspect-then-stop safety as `gc_incomplete` (L1). Make `down` succeed with
   nothing to stop in a checkout that never ran `up` (L5).
6. Add dependency declarations, so restarting a service restarts or re-verifies its dependents (R6).
7. Close the error-envelope gaps and correct the misleading diagnostics.
