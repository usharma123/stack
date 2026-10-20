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

### Benchmark validation

`run.py validate` runs the starting code and the reference solution through the same grading
path agent work uses. Each run is cold, with a fresh root, under the locale shown. Raw
evidence is in `eval/results/agentdx-opus-impl-20261020/`.

Final run at `17d8f2f` (`eval/results/agentdx-opus-impl-20261020/final3/`). Unit tests,
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

Three `harness_selftest` plans ran through `plan`, `trial`, `review-template` and `report`
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

`bench/regressions.py` at `17d8f2f` (`final3/regressions/`). Every check failed on 0.1.22,
reproducing its finding, while its control passed. Cleanup was confirmed for all of them, and
no global receipt changed.

| Check | Finding | Observed on 0.1.22 | Control |
|---|---|---|---|
| `run-exec-url-parity` | R1 | `exec`: `http://127.0.0.1:24741`; `run`: `https://web.urlparity.localhost` | `exec` gives loopback |
| `postgres-locale-independent` | R3 | `LC_ALL=C` → `SQL_ASCII`; `LANG=en_US.UTF-8` → `UTF8` | UTF-8 locale gives `UTF8` |
| `external-sigterm-exec` | R2 | Stack exits -15; the command and its grandchild are still alive | Unrelated sentinel survives |
| `removed-checkout-reclaim` | L1 | After a confirmed `down`, deleting the checkout and `gc`: 1 port reservation kept, 28 KB of service data kept, `gc` reclaimed nothing | `down` confirmed, `gc` exit 0 |
| `down-before-up` | L5 | `down` exit 1 `provider_failed` ("mise daemons is experimental") | — |
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
| External cancellation | Astra (R2) | Harness kills an agent's group on budget expiry | `external-sigterm-exec` | MCP server cancellation; other signals |
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

- **No agent results.** The pilot needs your authorization for a real agent CLI, its
  credentials, and model spend.
- Three small synthetic tasks on one Mac. The existing arm is glue written for the benchmark,
  not a real team's setup, and Compose/devcontainer control arms are not implemented.
- The benchmark computes no confidence intervals. Five pairs per cell is diagnostic only.
- Graders sit in this repository. Agents are kept out by convention, not by a sandbox.
- Cleanup can only report "none observed". A process that detached without its command line,
  working directory or an open file under the trial root would be missed.
- Global receipts cover the user's mise, Pitchfork and Stack files and the mise install
  listing. Other programs can change them, so a difference must be inspected.

## Decisions pending for you

1. **Run the real pilot?** Which agent CLI(s) and models, the budget, and whether the agent
   may use its normal config and credentials. The harness keeps `HOME` and isolates
   `XDG_CONFIG_HOME`.
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
