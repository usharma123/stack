# Stack AgentDX remediation ledger

Branch `fix/agentdx-findings-20261020`, from the approved benchmark head
`065086dba914b886d1f46763a80b30c045e16021` (product base
`8b3402fa1db8f11917b5ba33a333c9093545d9ce`, Stack 0.1.22). Every distinct finding of the two
hands-on reports ([Opus](opus/REPORT.md), [Astra](astra/REPORT.md)) and the
[synthesis](SYNTHESIS.md) is listed once. Those three documents are unchanged; this ledger is
new. Raw evidence (git-ignored) is under `eval/results/agentdx-product-fixes-20261020/`.

How these were checked, with the exact tested head, binary and every gate result, is in
[REMEDIATION.md](REMEDIATION.md). The independent review of `a401cfe` found four defects in this
branch's own fixes; they and the statements they made wrong are in
[Corrections after the independent review](#corrections-after-the-independent-review-round-2).
The second independent review, of `f58c09f`, found one boundary case of each of round 2's F2
and F3 fixes still wrong; they are in
[round 3](#corrections-after-the-second-independent-review-round-3). The third, of `b1bcd9e`,
found one command-admission case next to round 3's signal fix; it is in
[round 4](#corrections-after-the-third-independent-review-round-4). The fourth, of `e34817c`,
found two cancellation cases in the callers of the shared process helpers; they are in
[round 5](#corrections-after-the-fourth-independent-review-round-5). Every row names its proof; "remaining limit" is what the fix
does not do.

After round 5, the owner decided O1: plain `down` now stops the supervisor Stack started when
nothing else uses it. That dated change is in
[Follow-up 2026-10-21](#follow-up-2026-10-21-plain-down-stops-the-supervisor-stack-started-owner-decision-on-o1);
the O1 and L6 rows below still describe the opt-in behaviour they were recorded with. Its
review found two cases where it stopped a supervisor still in use; they are corrected in
[Correction 2026-10-21](#correction-2026-10-21-review-of-the-follow-up-sc2-f1-sc2-f2).
The next review found that the SC2-F1 lock did not cover a reused session; that is corrected in
[Correction 2026-10-21, round 3](#correction-2026-10-21-round-3-review-of-a09676d-sc3-f1).
The review after that found that a service's `setup` could still send a request to another
supervisor than the one locked; that is corrected in
[Correction 2026-10-21, round 4](#correction-2026-10-21-round-4-review-of-a49f271-sc4-f1).
Its review found that a request could still reach another supervisor than the one locked, when
what the template reads changed after Stack's last look; that is corrected in
[Correction 2026-10-21, round 5](#correction-2026-10-21-round-5-review-of-b7d95c9-sc4-f1-still-open).
Its review found that a configuration generated before that binding, which `down` reuses as it
finds it, did not apply it; that is corrected in
[Correction 2026-10-21, round 6](#correction-2026-10-21-round-6-review-of-d1f6dba-sc5-f1).

Kinds: **defect** (Stack does something wrong or contrary to its docs), **missing** (a capability
agents need that Stack did not have), **docs** (behaviour was right or deliberate but undocumented
or documented wrongly), **external** (outside Stack's code: a prerequisite or another tool), and
**working** (verified behaviour, recorded so it is not lost). In the brief's terms, defect is
an actual bug, missing is a missing capability, docs or "documented behaviour" is documented
behaviour, and external is an external prerequisite. Rows marked observation, coverage,
product direction or benchmark are recorded so nothing is dropped. Each one says what was
done, or why nothing was. "Before" evidence is the original
receipt or a reproduction against the 0.1.22 source built at `/tmp/afx-base` (sha256
`068181c0…158580`; it differs from the synthesis binary `0ec996dc…` only by build path).

Each fix commit carries its code, a test that fails without it (where a test can) and the docs it
changes. Commit hashes are listed in the table at the end.

## Reliability

| # | Finding (source) | Severity | Kind | Reproduction before | Fix | Proof after | Remaining limit |
|---|---|---|---|---|---|---|---|
| R1 | `stack run` / MCP `stack_run` gave a custom service the Pitchfork proxy URL; `exec` gave loopback (Opus P1-1, Astra #1, synthesis R1) | high | defect | Opus receipts 017/021/135; Astra `ledger-task-endpoints-retry.json`; reproduced here: `run with_dep` → `https://web.u.localhost` | `4550b15` | unit test `literal_values_planning_read_back_unchanged…`; e2e `11-field-regressions` (CLI run, task listing the service, MCP `stack_run`, real HTTP); real-mise `task_env_provenance` (literal keeps precedence over a tool's env); regression `run-exec-url-parity` FIXED | A project `[env]` that sets `<NAME>_URL` to a *template* is still declared as inherited and could be shadowed by mise's daemon export |
| R2 | SIGTERM to `stack --json exec` left the command and grandchild running (Astra #2, synthesis R2); same for terminal exec/run without `--timeout` and the MCP server | high | defect | `probes/r2/before.json`: 7/7 cases leak on 0.1.22 (json/terminal exec and run, MCP, TERM-ignoring child) | `c6c54aa` | `probes/r2/after.json` 7/7 clean, sentinel survives, 10 s grace, second signal kills at once; 3 runtime tests; regression `external-sigterm-exec` FIXED | SIGKILL to Stack cannot be passed on. On a terminal only SIGTERM is forwarded, to the command's PID (its own children follow only if it propagates) |
| R2a | A command a signal ended made the CLI exit 1, like an app failure (Opus observation) | low | defect | Opus receipt 066 | `2b0905b` | runtime test: exit 137, `data.signal: 9`, both paths | — |
| R3 | Postgres cluster encoding followed the locale of the first `up`: `SQL_ASCII` under `LC_ALL=C`/`env -i`/Linux containers (Opus P1-2, Astra Linux, synthesis R3) | high | defect | Opus receipts 124/125; `probes/r3/after.txt` first lines (0.1.22 under `LC_ALL=C` → `SQL_ASCII`) | `f2277c1` | `probes/r3/after.txt`: `LC_ALL=C`, `env -i`, `LANG=en_US.UTF-8` → `UTF8`, collation `C.UTF-8`, `Zoë Café ✓` 10 chars/14 bytes; existing SQL_ASCII cluster kept with its row and warned; e2e `12-locale-independent-postgres`; regression `postgres-locale-independent` FIXED; ledger `LC_ALL=C` validation cell | A project `[env]` locale wins over the daemon's (the project's committed choice; `up` warns). Hosts lacking a `C.UTF-8` locale (pre-2.35 glibc without the Debian locale) fail initdb visibly. Existing clusters are not converted |
| R4 | `stack run` resolved task secrets before installing the pinned fnox (Opus P2-3, synthesis R4) | medium | defect | Opus receipt 100; runtime test fails on old code with the exact `secret_unavailable` | `81118e0` | runtime test (CLI and MCP); regression `secrets-before-install` FIXED | — |
| R5 | A cold `mise version` missed Stack's 10 s probe, reported `provider_unavailable` with "download mise" (synthesis R5, observed once) | medium | defect | now reproduced deterministically: two fake-mise tests fail on old code with the exact message | `173f677` | `MISE_DISABLE_UPDATE_WARNING=1` measured 0.01 s vs 0.2–1.4 s cold; tests for skipped update check and the retry | The original latency cause (GitHub update check) is inferred from timing, not traced. A mise that never answers still fails, now naming its path |
| R6a | Dependents stay stale after a dependency restart or recovery (Opus missing 1, Astra dependents, synthesis R6) | high | missing | `probes/r6/dependents.txt` plain mode: worker stale after `restart api`; API keeps the old Postgres after SIGKILL + `up` | `2041a32` | same probe with `depends_on`: restart `[api, worker]` both v2; `up` runs `stop_dependents` and the API reconnects; unit, compile and ordering tests | Opt-in per service. No health check that a dependent's connection still works; `status` does not flag a dependent older than its dependency |
| R6b | No pre-start step: migrations/seeding/`uv sync` chained into `run` (Opus missing 2, synthesis friction) | medium | missing | benchmark arms chain `seed.py && migrate.py && exec …` | `5cea775` | runtime test (runs before each start, not for a kept service, failure is `setup_failed` with output); `probes/r6/setup.txt` real Postgres migration seen by the API | The benchmark's own Stack arm keeps its chained `run` (benchmark files unchanged) |
| R6c | Adding a task invalidated the session though no service changed (Astra, synthesis friction) | medium | defect | Astra discovery log (two `service_unavailable` after adding a task); `probes/r6/tasks-only.txt` 0.1.22 status `stale True` | `9f36c3e` | runtime test; probe: task added to a new session → healthy, same PID, new task runs | A session recorded by 0.1.22 needs one restart after its first task edit |
| R6d | Watch only reports; agent must name each service to restart (both, synthesis friction) | low | missing (documented behaviour kept) | docs: "Nothing restarts automatically" | `7b223c1` | runtime test (`restart --changed`, MCP `changed`) | Still no automatic reload, by design |
| N1 | Redis exited at once when the inherited locale named one the machine lacks (`LANG=en_US.UTF-8` in a minimal Debian image): "Failed to configure LOCALE for invalid locale name" (found here, while validating R3 on Linux; same class as R3) | high on such hosts | defect | `linux/regressions/removed-checkout-reclaim/up.stdout` (first Linux run, regression `invalid`) | `15ff636` | compile test (redis daemon env `LC_ALL=C`, commands keep the caller's locale); second Linux run: Redis up + `PING` under the missing locale, all six regressions FIXED (`linux2/`) | Redis sorts `SORT ... ALPHA` in `C` order everywhere; a project `[env]` locale still wins |

## Agent friction: error envelope (Opus P3-5)

| # | Finding | Severity | Kind | Before | Fix | Proof |
|---|---|---|---|---|---|---|
| E1 | `--timeout 1ms`, `--ttl banana` gave `usage` without hint and exit 1; parse errors exit 2 | low | defect | Opus 064/068 | `052dacd` | runtime test: exit 2 + hint for CLI and MCP |
| E2 | `stack --json logs` message cut after "were not provided:" | low | defect | Opus 069 | `b287236` | runtime test (`<SERVICE>` named, usage line in hint) |
| E3 | `session_busy` had no hint or details | low | defect | Opus 128 | `2fac65e` | runtime test (pid, command, hint) |
| E4 | MCP `secrets: ["lower-case"]` gave `usage`, CLI gave `invalid_secret` | low | defect | Opus 108 | `6ab0e92` | runtime test: same code and hint, no provider work |
| E5 | Guarantee claimed every error has a hint and details | low | docs | guarantees.md | `396cd67` | docs |
| E6 | `down` succeeds while busy but `restart` refuses (Opus observation) | — | docs (deliberate) | Opus 128-129 | documented in `2fac65e` | — |

## Agent friction: diagnostics (Opus P3-6)

| # | Finding | Kind | Before | Fix | Proof |
|---|---|---|---|---|---|
| D1 | SIGKILLed Postgres reported as "process changed since launch" | defect | Opus 046; runtime test fails on old code with that text | `63aa997` | runtime test |
| D2 | A never-ready `ready_cmd` was never named; hint invited a pointless retry (also Opus missing 7) | defect | Opus 083-084 | `8fcadfb` | runtime test: hint `web's ready_cmd exits 1`, `details.readiness` |
| D3 | `status` said `healthy: true` with `lease_expired` | defect | Opus 110/133 | `54d7684` | runtime test; e2e `3-leases` |

## Discovery and documentation

| # | Finding | Kind | Fix | Proof |
|---|---|---|---|---|
| X1 | No MCP `outputSchema` (Opus missing 4) | missing | `9518243` | unit test: success, usage and unknown-tool results fit it |
| X2 | No `stack init` (Opus missing 3) | missing | `cfb55a6` | unit + runtime tests |
| X3 | No warning that `stack.lock` came from another Stack; `stack 0.1.6` first on PATH (Opus missing 8) | missing | `0442ee7` | compile + runtime tests; on this host doctor now names `/opt/homebrew/…/stack.cjs (stack 0.1.6)` |
| X4 | `secrets.md` shows no `fnox.toml` | docs | `e9e6c83` | docs consistency suite (`tests/docs.test.mjs`) |
| X5 | `--require` one service per flag | docs + hint | `6d3a137` | runtime test |
| X6 | Data lives under `$MISE_STATE_DIR/daemons/<hash>/data`, not `STACK_DATA_DIR` | docs | `ec06451` and the gc docs in `227f9c0` | docs |
| X7 | Isolated-run recipe pins 0.1.21 | docs | `e38349b` | docs |
| X8 | Root hosted runners: Postgres refuses root after every download (Astra) | missing (fail fast) + docs | `5fe091a` | unit test; Linux root check (see validation) |
| X9 | Identity token visible to every command (Opus observation) | docs (deliberate) | `2aef819` | — |
| X10 | `doctor` said `ok: true` while the pinned fnox was "pinned but not installed" (Opus receipt 099) | documented behaviour | none: a pinned tool not yet installed is not a fault, `run`/`up` install it; the R4 fix removes the case where it mattered | R4 runtime test |
| X11 | A task's `ok: true` envelope with a nonzero `exit_code` must not be read as success (Astra's oracle advice) | documented behaviour | none: `ok` describes Stack, `exit_code` the command, and the CLI exits with the command's status (Opus 062, Astra) | existing runtime tests |
| X12 | `depends_on` was rejected as an unknown field (Astra) | missing | now accepted: see R6a | R6a |

## Workspace lifecycle

| # | Finding | Kind | Before | Fix | Proof | Remaining limit |
|---|---|---|---|---|---|---|
| L1 | Removed checkout after `down` keeps port reservations and data; `gc` reclaims nothing (Opus P2-4) | defect (ports: DESIGN said "pruned when a project directory disappears") + missing (data) | Opus 137-139; regression `removed-checkout-reclaim` reproduced in rounds 1-2 | `227f9c0` | regression FIXED; `probes/l1/` real reclaim; gc_incomplete case kept intact, recovery then reclaim; runtime test incl. live token carrier, supervisor running, marker takeover; e2e `7-deleted-project` | Data recorded by 0.1.22 or earlier is not in the ledger, so it is reported/reclaimed only once a new `up` records it. mise's small per-project state files remain (`mise daemons prune`) |
| L2 | Recreating a checkout quarantines old data (Astra) | working | — | — | e2e `11-field-regressions` | Quarantined data is never reclaimed automatically |
| L3 | Deleted without `down` → `gc_incomplete` with recovery commands (Opus) | working (safety contract) | — | preserved | `probes/l1/recovery.txt`, e2e `7-deleted-project` | Still needs a person or runner to follow the inspect/stop steps |
| L4 | Simultaneous checkouts isolated (both) | working | — | — | e2e `1-independent-checkouts` | Not measured with concurrent independent agents |
| L5 | `down` before `up` failed in a fresh clone (synthesis) | defect | regression reproduced rounds 1-2; runtime test fails on old code | `820d45c` | regression `down-before-up` FIXED | — |
| L7 | A second MCP process found the live session and ran the test without a restart (Astra handoff) | working | — | — | Astra receipts | Same machine only; no data handoff (H1) |
| O1 | Pilot: both Stack service agents ended with a plain `stack down`, leaving the root's supervisor running (`supervisor_left_running: 2`) | observation (documented L6 behaviour) | `pilot-r2/report.json` | — | — | Open product question: whether `down` should stop a supervisor that Stack started for this root and nothing else uses, by default |
| L6 | Per-root Pitchfork supervisor/log-sink linger after `down` (synthesis) | missing | `probes/l6/probe.txt` after plain down: supervisor still owned | `46a50af` | probe: kept while another session uses it, stopped when idle, nothing owned left; runtime test | Opt-in (`--stop-supervisor`); plain `down` keeps the shared supervisor by design |

## Deferred, external and recorded-only

| # | Item | Kind | Status |
|---|---|---|---|
| H1 | Handing database state between worktrees (snapshot/clone, `exec --from`) (Opus missing 6, Astra handoff, synthesis) | missing, product decision | **Deferred, needs a product decision.** It touches data ownership (markers, quarantine, gc), the providers' data layout and copy semantics for running databases. Not started; question raised in the handoff |
| P1 | npm 0.1.22 unpublished during both sessions | external | Not addressed; no release was authorized. Everything here is a source build |
| P2 | Linux defunct supervisor children under the container's PID 1 (Astra) | external | Container init (`sleep` as PID 1) does not reap; use `--init`. Not a Stack defect |
| P3 | Cold macOS timings dominated by first execution of new binaries (synthesis) | external | Recorded only |
| P4 | Benchmark blind spots (Opus §7: URL only via exec, locale, dependency restart, secrets before install, reclaim, envelope sweep) | test coverage | Covered by the tests and e2e scenarios above, and by `bench/regressions.py` |
| P6 | Tester tooling mistakes (Opus `rx.sh` exit mapping, shell quoting; Astra `bash -lc` losing Cargo, test-state repair; Opus's npm debug log under `~/.npm/_logs`) | not Stack | Recorded only; excluded from product findings by both testers |
| P7 | Signals other than SIGTERM; MCP server cancellation (Astra coverage gaps) | coverage | Covered by R2: SIGINT and SIGHUP are forwarded too off a terminal; MCP server cancellation is in the R2 probe (`probes/r2/after.json`) |
| P8 | Hosted Linux, Linux x64, Intel macOS, network policy | coverage | Unchanged: Linux evidence here is local Docker LinuxKit arm64, not a hosted runner (see REMEDIATION.md) |
| P9 | Workspace requirements from the synthesis: collision prevention, a proper lifecycle, reclaiming stale worktree disk, fast startup | product direction | Collisions: working (L4). Lifecycle: external cancellation (R2), removal after `down` (L1) and `down` before `up` (L5) are fixed. Reclaim: `gc` and `gc --reclaim-data` (L1). Startup: no change and no measurement beyond single cold timings (P3). Snapshot/clone handoff remains H1 |
| B1 | Benchmark follow-ups from the synthesis: a Compose/devcontainer control arm, a hosted Linux runner, a disposal task family, gating on the documented task loop now that R1 is fixed | benchmark, not product | Not done here: the benchmark files are unchanged on this branch so the approved harness still applies |
| P5 | Not run in the sessions: OCI publish/pull, Worktrunk, Intel macOS, registering a real MCP client, skills, git/OCI bundles | coverage | Unchanged; e2e scenario 5 (OCI) still needs a registry |

## Corrections after the independent review (round 2)

The independent AI review of `a401cfe` (Astra) requested changes: four defects in this branch's
own fixes, each reproduced against the frozen binary `1cad8602…`, and two evidence corrections
(made in REMEDIATION.md). Each defect has one commit with its code, a test that fails without
it, and its docs. "Before" is the reviewer's receipts (kept with the review, outside this
repository) and the `before/` runs; "after" paths are under
`eval/results/agentdx-product-corrections-r2-20261020/`.

| # | Finding | Severity | Kind | Before | Fix | Proof after | Remaining limit |
|---|---|---|---|---|---|---|---|
| F1 | `gc --reclaim-data` followed a replaced directory above the data: with the recorded directory's parent moved and replaced by a link to an independent copy, marker included, it removed the copy's data and marker, exited 0 and reported the recorded path removed | P1 | defect, introduced by L1 (`227f9c0`) | reviewer `gc-copy-probe.json`, `gc-parent-symlink.json`; `before/f1-gc-bound-probe.json`: the old binary also removes a copy put in place | `18e50a7` | 6 unit tests in `datadirs` (link as the directory, link above it to a copy and to the directory itself, copy in place, swaps between the check and the removal, platform alias, links never followed, binding); runtime `gc_releases_…` (redirected parent and copy in place leave both trees whole; default keeps; a live checkout keeps; a normal reclaim removes only the directory and its marker, not a file beside them); `after/f1-gc-bound-probe.json`; reviewer probes rerun (`probes/reviewer/after`): nothing removed | Entries written before this fix, and data whose directory no verified `up` saw, are kept with a message, never removed. Unix only. If the data directory itself is swapped after the check, its contents go through the checked handle; the name that replaced it is kept |
| F2 | A signal during a service's `setup` was passed on, but a setup that handled it and exited 0 counted as success: `up` started the service and exited 0 (real mise 2026.10.3, Pitchfork 2.29.0); MCP did the same | P1 | defect, R6b with R2 | reviewer `setup-cancel.json`, `setup-after-cancel-status.json`; `before/f2-setup-cancel-runtime.log` | `33a9a07` | runtime tests: setup exiting 0 on SIGTERM, and one ignoring it until a second signal, via CLI and MCP: `cancelled`, exit 143, the later setup not run, no start request, an unrelated process survives; `decide` unit case; reviewer `lifecycle_probes.py` rerun: exit 143, nothing started, status unhealthy; `after/lifecycle-supplement/`: an ignoring setup with one SIGTERM ends after the 10 s grace, exit 143, the second service's setup not run | A signal at any other point of `up` or `restart` still ends Stack at once, with no JSON result, as before; the launch record is kept either way |
| F3 | Restarting a dependent alone ran its setup and started it while a dependency outside the restart was down; only final verification failed (exit 124) | P2 | defect, R6a/R6b | reviewer `dep-restart-with-dead-db.json`; `before/f3-dependency-restart-runtime.log` (restart succeeded with the dependency failing its ready_cmd) | `2afca9f` | runtime test: dependency failing its ready_cmd, then stopped: `dependency_not_ready`, nothing stopped, set up or started, session record byte-identical; then a positive control; unit test of `prerequisites_of`; reviewer probe rerun: exit 1, setup log unchanged; `after/lifecycle-supplement/` (real): an unready dependency refused with db, cache and api PIDs, process identities and session id kept, then the control restarts only api | Documented policy: `restart` never starts a dependency it was not given; `stack restart <dependency>` or `stack up` recovers it |
| F4 | `stack init` checked `exists()` then wrote: 12 concurrent inits gave several successes on one path; a dangling `stack.toml` link was followed to create a file outside the project | P2 | defect, X2 | reviewer `init-race-*.json`, `init-dangling-link.json`; `probes/reviewer/before`: 6 of 12 succeeded; `before/f4-init-unit.log` (12 of 12) | `c25702f` | unit tests: 12 threads with 250 tools each, one winner, the rest `project_exists`, the file whole, no temporary file left, mode as `fs::write`; dangling and live links untouched; reviewer `file_probes.py` rerun: 1 of 12 succeed, the dangling link is refused and nothing is created outside | An interrupted init can leave a hidden `.stack.toml.*` temporary file, never a partial `stack.toml` |

### Statements the findings made wrong

These rows above were accurate about what was tested, but claimed more than the code did:

- **L1** said data is removed only if `up` recorded it and its marker still names the checkout.
  The path could be redirected above the marker (F1). Data is now also bound to where it
  resolved and which directory it is.
- **R2** said a signal is passed on and Stack then ends as the signal asked. During a setup
  (R6b) the start went on (F2).
- **R6a/R6b** and `bundles.md` said a setup runs once the services it depends on verified. That
  held for `up`'s waves, not for `restart` of a dependent alone (F3).
- **X2** "never replaces" was a check followed by a write (F4).
- The pilot's isolation and transcript wording, and who can satisfy the review gate, are
  corrected in [REMEDIATION.md](REMEDIATION.md#real-agent-pilot).

## Corrections after the second independent review (round 3)

The independent AI review of `f58c09f` (Astra) found F1 and F4 passing the checks it made, and
the ordinary F2 and F3 cases passing, but requested changes for one boundary case of each. Both
reproduced against the frozen round-2 binary `03fd21e6…` here before any change
(`before/late-signal`, `before/readiness-spawn`). Each has one commit with its code, tests that
fail without it, and its docs. "After" paths are under
`eval/results/agentdx-product-corrections-r3-20261020/`.

| # | Finding | Severity | Kind | Before | Fix | Proof after | Remaining limit |
|---|---|---|---|---|---|---|---|
| R3-F2 | A signal Stack received during a setup could be acted on only after later setups and starts: the handler records a signal and a separate thread acts on it, but a command's run returned without waiting for that thread. With that thread held 500 ms after reading a real SIGTERM (a test-only interposer, no source change), a setup that signalled Stack and exited 0 let `up` run the next setup, start both services and exit 0; MCP answered success; a dependent wave started the service whose setup was cancelled | P1 | defect in F2's fix (`33a9a07`) | reviewer `late-signal-v2/`, `late-signal-rebuilt/`; `before/late-signal/` (exit 0, later setup ran, healthy); `before/late-signal-r3/` (CLI and MCP succeed, partial case starts api) | `dbb5085` | unit tests in a process of their own, the signal thread held by a test-only gate: a command that signals Stack and exits 0 does not return until the signal is handled, then counts it; a signal that arrived between commands ends Stack before the next command starts (`before|after/f2-unit.log`); reviewer `late_signal_v2.py` and its control rerun unmodified on the frozen binary (`after/reviewer/`): exit 143, `cancelled`, the later setup not run, status unhealthy; `after/late-signal-r3/`: CLI graceful, CLI ignoring SIGTERM (10 s grace), MCP, and a started dependency wave, all `cancelled` exit 143 under the delayed thread, no later setup, the cancelled service not started, the started dependency recorded and stopped by `down`, sentinel alive | A signal the kernel has not yet delivered to Stack's handler when a setup is retired belongs to what follows: it is passed on to the next command or ends Stack, never a success. Handling waits for the signal thread; it is not bounded by a timeout |
| R3-F3 | The restart prerequisite gate took a `ready_cmd` Stack could not spawn or read as passing: with real mise and Pitchfork and no `sh` on the stack's PATH, `restart api` stopped the running api, then failed `setup_failed`; db's readiness was never established and the session was left unverified | P2 | defect in F3's fix (`2afca9f`) | reviewer `readiness-spawn-v2/`, `readiness-spawn-rebuilt/`; `before/readiness-spawn/`; `before/readiness-r3/` (api stopped, later commands `session_stale`) | `ba5360f` | runtime restart test gains the no-`sh` case: `dependency_not_ready` naming why the check could not run, identities, setup log, requests and session bytes kept; the declared-readiness test checks the timeout report names an unrunnable check (`before|after/f3-*.log`); reviewer `readiness_spawn_v2.py` rerun unmodified (`after/reviewer/`): `dependency_not_ready`, both identities and the session bytes kept; `after/readiness-r3/` (real): nonzero readiness, unrunnable with the marker absent and present, all refused with identities and session bytes kept; a successful restart replaces only api; a dead dependency is refused | An unrunnable `ready_cmd` blocks `restart` of its dependents until it can run; `up` waits for it, then times out naming why |

### Statements the findings made wrong

- **F2** (round 2) said a signal passed on to a setup cancels the start "whatever the setup's
  exit status". That held only once the signal thread had acted on it before the setup's run
  returned (R3-F2).
- **F3** (round 2) said a dependency outside the restart must "pass its declared readiness".
  A check that could not run was counted as passing (R3-F3).

## Corrections after the third independent review (round 4)

The independent AI review of `b1bcd9e` (Astra) found R3-F2's retirement ordering and R3-F3
corrected, and F1 and F4 still correct, but requested changes for one command-admission case
next to R3-F2's fix. Unlike earlier rounds, the reviewer derived it from the source rather than
reproducing it at runtime, and it was not observed in a real Stack run here either. It was
reproduced deterministically by unit tests against the unfixed code, with test-only hooks
at the admission points. "After" paths are under
`eval/results/agentdx-product-corrections-r4-20261020/`.

| # | Finding | Severity | Kind | Before | Fix | Proof after | Remaining limit |
|---|---|---|---|---|---|---|---|
| R4-F1 | A command was admitted in two steps: settle the signals counted so far, then take the registry lock and count again for the command's `since`. A signal that reached Stack's signal thread between the two was outside the new command's lifetime but not yet handled when it was spawned. With no other command running, the signal thread then found no recipient and ended Stack with the default action, leaving the new setup, exec or provider command running in its own process group after Stack was gone | P1 | defect in R3-F2's fix (`dbb5085`) | review of `b1bcd9e` (source schedule; the existing `decide` case for `since == arrival`); `before/r4-unit.log`, with the tests and hooks applied to the unfixed `spawn_named` (`before/r4-tests-on-unfixed.diff`): the signal arriving before the decision leaves the admitted `sleep` (PID 23917) alive after its Stack process died of SIGTERM; with a user's command running, the next command starts before the signal is handled | `f6a87b6` | admission is one decision under the registry lock: take it, read the arrival count, spawn with that same boundary only if every report through it was handled, else release the lock, wait for it, and decide again. Three tests, each in a process of its own, with test-only hooks at the admission points and the signal thread held by its test gate: a signal arriving after an earlier settlement and before the decision, with no other command, ends Stack before the command starts; a signal arriving after the boundary, held under the lock, is passed on to the admitted provider command, which does not outlive Stack; with a user's command running, the next admission waits until the signal is handled and passed on to it, then runs unsignalled, and both are forgotten afterwards (`after/r4-unit.log`, 20 of 20 repeats in `after/r4-repeat20.txt`). R3-F2's two schedule tests still pass. Gates and real-binary probes in REMEDIATION.md round 4 | A signal the kernel has not yet delivered to Stack's handler is still the next command's, or ends Stack, as before. Admission waits for the signal thread without a timeout, as retirement does; a run of signals that keeps arriving can keep a command from being admitted, and a signal with no recipient ends Stack anyway. With a user's command running, the signal reaches it and Stack goes on, so a command admitted on another thread afterwards still runs; whether to start it is the caller's check (`up` checks before each setup) |

### Statements the findings made wrong

- **R3-F2** (round 3) said "no command is spawned while a signal that already arrived is
  unhandled". It settled the signals counted before taking the registry lock, then counted
  again under it without checking that the second count was handled, so a signal arriving
  between the two was neither handled first nor the new command's (R4-F1). That round-3 row and
  section are kept as written.

## Corrections after the fourth independent review (round 5)

The independent AI review of `e34817c` (Astra) found R4-F1's admission correction sound, and F1,
F4 and typed readiness still passing, but requested changes for two cancellation cases in the
callers of the shared process helpers. As in round 4, the reviewer derived both from the source
and did not run them, and neither was observed in a real Stack run here before the fix. Both were
reproduced deterministically by unit tests against the unfixed code, with test-only hooks; the
same tests pass after. "After" paths are under
`eval/results/agentdx-product-corrections-r5-20261020/`.

| # | Finding | Severity | Kind | Before | Fix | Proof after | Remaining limit |
|---|---|---|---|---|---|---|---|
| R5-F1 | A user's command was named under the registry lock and the lock released before its run made the monitor watching it for passed-on signals. The monitor took the global count of passed-on signals as seen and the command's own count as zero, so a SIGTERM passed on to the command in between was never noticed: with no second signal the cancellation grace never started. An attached `exec`/`run` without `--timeout` could wait indefinitely for a command ignoring SIGTERM; a captured one (`--json`, MCP, setup) or `--timeout` run waited for its own deadline instead of the 10 s grace | P1 | defect in R2's signal forwarding (`c6c54aa`) | review of `e34817c` (source schedule); `before/r5-f1-unit.log`, the final tests and hooks on the unfixed code (`before/r5-f1-tests-on-unfixed.diff`, built by `scripts/unfix_f1.py`): signal passed on and handled after the naming, before the run watches: the captured and deadline runs time out after 40 s, the attached run does not return within the 60 s bound | `c0e6238` | `spawn_named` makes the monitor under the same lock as the naming, so every later forward moves the global count past what it saw and its own count is read. One test per caller (capture, `run_with_deadline`, `run_attached`), each in a process of its own, with a test-only hook once the command is named and the lock released: the command ignores SIGTERM, one signal is sent and handled before the run watches, and the group is killed after the grace (≥ 10 s, < 30 s) with no second signal, the signal owed, an unrelated process alive (`after/r5-unit.log`, 20 of 20 repeats in `after/r5-repeat20.txt`) | The grace is timed from when the run first sees the signal, at most one poll interval after the forward once the run watches. The first-signal/second-signal policy and the 10 s grace are unchanged |
| R5-F2 | `run_attached` (`exec`/`run` without `--json` or `--timeout`) decided whether to kill what an exited command left in its group from the signals its monitor had seen handled, before retirement took the lock and counted the signals that were the command's. A SIGTERM counted while the command ran but still queued for the signal thread, or one passed on between that check and retirement, was missed: the leader was reaped with its group kept, the signal thread then found the command retired and signalled nothing, and a descendant ignoring SIGTERM outlived Stack. Provider commands run by `output` had the same order: a signal queued as one exited never reached its group before it ended Stack | P1 | defect in R2's signal forwarding (`c6c54aa`), not fixed by R3-F2's retirement settlement | review of `e34817c` (source schedule); `before/r5-f2-unit.log`, the final tests and hook on `c0e6238` (`before/r5-f2-tests-on-unfixed.diff`, `scripts/unfix_f2.py`): queued attached signal and a signal handled as the attached command is retired both leave its SIGTERM-ignoring sleep running; a provider command's queued signal never reaches the sleep it started; the unsignalled control passes before and after. Real binary, timing-assisted: `after/probes/attached-r4/`, the round-4 binary with the reviewer's delayed signal thread: attached `exec` exits 0 and its SIGTERM-ignoring sleep is still running (the probe then killed it) | `34369ef` | `retire` decides at its own boundary. Cut short: killed with its group at once, as before. Exited: the leader stays unreaped and the command `held`; retirement waits without the lock until every signal that arrived by then is handled, the signal thread still signals a held group (its ID kept by the unreaped leader), then under the lock the group is killed before the reap if a signal was the command's, else only the leader is reaped. Nothing signals a group after its leader is reaped. Tests in a process of their own: queued attached signal; signal handled at retirement (test-only hook); queued provider signal; unsignalled control keeps its sleep; plus the decide unit test for held entries (`after/r5-unit.log`, 20 of 20 repeats). Real binary, timing-assisted: `after/probes/attached-r5/`, the same probe on the round-5 binary: the sleep is gone, the unsignalled control keeps its sleep on both binaries, sentinel alive | A signal the kernel has not delivered to Stack's handler by the time the command is retired belongs to what follows, as before. Attached `exec` still reports the command's own exit status (0 for a command that exits 0 after SIGTERM), as documented; only its group is now cleaned up. On a terminal the command stays in Stack's group, so only its leader is ever signalled |

### Statements the findings made wrong

- **R2** (`c6c54aa`). The `run_attached` doc said whatever is still in an attached command's group
  "is killed once the command is done after a signal was passed on". That held only for signals
  its monitor had seen handled before the command exited (R5-F2). DESIGN.md said Stack "kills the
  group 10 seconds after the first signal or at the second". A first signal handled before the
  run first looked never started that grace (R5-F1).
- **R3-F2** (round 3) said "a command's run returns only once every signal that arrived while it
  was running was acted on". The run did wait for them, but for an attached command the decision
  to clean its group up was taken before that wait (R5-F2). The round-3 and round-4 rows are kept
  as written.

## Follow-up 2026-10-21: plain `down` stops the supervisor Stack started (owner decision on O1)

The owner was asked: "when `stack down` stops the last session using a supervisor that Stack
owns, should it stop that supervisor too?" The answer was "Stop it automatically for complete
cleanup". This entry records that change. It does not rewrite O1 or L6 above, which describe
the opt-in behaviour at the commits they name. The pilot evidence (`pilot-r2/report.json`,
sha256 `60aa3b115ce727e354a209f7ab2bf83eb184d2d070da48e63b83cced19365f84`) is unchanged. It
still shows that both pilot agents left the supervisor running. No agent was rerun with the new
default.

| # | Change | Kind | Before | Fix | Proof | Remaining limit |
|---|---|---|---|---|---|---|
| O1/L6 follow-up | Plain `stack down`, `session::down` and MCP `stack_down` (field omitted) stop the Pitchfork supervisor and its `log-sink` helpers once the checkout's services are confirmed gone. This happens only when Stack can show it started the supervisor and nothing else uses it; otherwise it keeps the supervisor and gives the reason | product decision (owner) + one defect found while doing it | 4f7201c binary (`eval/results/agentdx-supervisor-default-20261021/before/`): two plain downs left `supervisor run` and a `log-sink` running. `down --stop-supervisor` on a root with no supervisor started one through `pitchfork list` (Pitchfork 2.29.0 starts a supervisor for `list`), and that supervisor was then killed with the command's process group. The two new runtime tests also fail on 4f7201c, but at its first missing piece (no owner record), so the binary probe is the behavioural before | `b80318c` | runtime tests `down_stops_the_supervisor_stack_started_by_default_once_nothing_else_uses_it` and `down_keeps_a_supervisor_stack_cannot_show_it_started`, plus unit tests for the owner record, resolved lock keys and shared/exclusive locking. Real mise and Pitchfork probe (`eval/results/agentdx-supervisor-default-20261021/probes/probe.txt`): two sessions; the first down keeps the supervisor and the last stops it and its log-sinks. A repeated down and a never-up down start nothing. MCP default, `--keep-supervisor`, MCP `false` and `--stop-supervisor` all behave as documented. A non-Stack daemon, a supervisor that was already running and a held lock each keep the supervisor. Concurrent down/up passed in both orders. The sentinel and unsaved work were intact | See below |

How ownership is shown. Every Stack request that may start a supervisor sets
`STACK_SUPERVISOR_OWNER` to a random mark of the Stack state. These requests are
`mise x -- pitchfork supervisor start`, `mise daemons start` and `mise daemons stop`. The mark
is written once to `<state>/supervisor-owner`. Stack never rewrites it, and stops trusting it
if the file is replaced. Stack never passes on a mark it inherited. The supervisor and its
helpers inherit the mark. `down` stops only the process that
`pitchfork status --json global/pitchfork` names, and only when that process carries the mark.
`ProviderRecord` (pitchfork, state_dir) is used only to see who uses the supervisor, never as
proof of ownership.

How usage is checked. The supervisor is kept in any of these cases:

- another session records it, with state directories compared after resolving links;
- another session records services without naming a supervisor;
- a session record cannot be read;
- a tracked daemon is in any state other than stopped, failed or errored, or has a live
  process.

`pitchfork list` is asked only after `pitchfork supervisor status` has said a supervisor is
running.

How commands are coordinated. A command holds a shared lock per resolved supervisor state
directory (`<state>/supervisors/<hash>.lock`) while it asks that supervisor to start or stop
something. It takes the lock after recording its session. `down` takes the lock alone for the
check and the stop, and waits at most 5 seconds for it.

Compatibility:

- `--keep-supervisor` and MCP `"stop_supervisor": false` keep the supervisor.
- An MCP call that omits the field gets the new default.
- `--stop-supervisor` is still accepted and changes nothing.
- A checkout without services gets no `supervisor` field.
- When no supervisor is running, the result is `{"stopped": true, "reason": "it was not running"}`, as before.

Remaining limits:

- The lock orders this Stack state's commands only. Another Stack state that shares the
  Pitchfork state directory is seen through its daemons, not through its in-flight requests.
- A stopped daemon that has a schedule counts as stopped.
- The real race rounds depend on timing. The lock ordering itself is proven by deterministic
  tests.
- Supervisors started before this change carry no mark, so they are kept until someone stops
  them.
- `gc`, lease expiry, disposal, snapshots and H1 are unchanged.

## Correction 2026-10-21: review of the follow-up (SC2-F1, SC2-F2)

A fresh independent review (Astra) of the follow-up at `fd76459` requested changes for two
counterexamples to "stop only after the last use", both reproduced with the frozen `b80318c`
binary and real Pitchfork 2.29.0. Both are corrected below. The follow-up entry above stays as
written; the claims this correction supersedes are listed after the table.

| # | Problem | Kind | Before | Fix | Proof | Remaining limit |
|---|---|---|---|---|---|---|
| SC2-F1 | A checkout using another Stack state but the same Pitchfork state directory could start a healthy service between `down`'s daemon list and its supervisor stop; `down` then stopped the supervisor and the service with it | defect | The coordination lock lived in the caller's Stack state, so two states took different locks. The review's transparent wrapper held the real `pitchfork list` answer: B's `up` returned healthy in about 4 s, then A stopped the supervisor and B's service died (`confirmed: true`, `supervisor.stopped: true`). The new runtime test fails on the `fd76459` source at "state-b: B's `up` finished while A was deciding" | `9946319` | The lock is now `stack-supervisor.lock` in the resolved Pitchfork state directory, so every Stack state and every name for that directory opens one file. A lock file found replaced after locking is opened and locked again, and `down` keeps the supervisor if its own was replaced while it decided. Runtime test `an_up_through_the_same_supervisor_waits_for_a_down_deciding_to_stop_it_whatever_its_stack_state` (other state and same-state control); unit test `a_lock_file_replaced_while_it_is_locked_is_locked_anew_and_its_holder_can_tell`. The review's own race scripts, with only their output path and binary changed, now give the same outcome for a separate state as for the same-state control: B waits for A (16.0 s and 15.9 s), A stops the then-unused supervisor, and B starts afresh and stays healthy with the same PID and open port | `down` still reads sessions from its own Stack state only. Another state's checkout is seen through its daemons, which its requests started before letting go of the lock. Removing the Pitchfork state directory while commands use it is outside what the lock covers. A forced interleaving is proven, not a natural frequency |
| SC2-F2 | `down` stopped a supervisor whose external daemon had an enabled schedule, so the schedule stopped firing | defect | A scheduled daemon is `stopped` (or `failed`/`errored`) between runs, and `down` counted that as over. The review's `scheduled.sh` showed no runs in the 23 s after `down`, and three once that supervisor was restarted | `4ae0435` | For every daemon that is otherwise over, `down` reads its `cron_schedule` from `pitchfork status --json <id>` (which reads the state file and starts no supervisor) and keeps the supervisor while one exists. A disabled daemon counts too: Pitchfork 2.29.0 ran a disabled cron daemon four times in 35 s, although its scheduling guide says `disable` prevents scheduled starts. An answer Stack cannot read, or a daemon Pitchfork no longer knows, also keeps it. Runtime cases: between runs, after a failed run, disabled, a non-string schedule, a missing status, a mismatched id and a missing daemon all keep it; no schedule (omitted, as Pitchfork omits it, or null) stops it. Real probes: the review's `scheduled.sh` (paths only changed) now keeps the supervisor, naming `external/timer`, with ticks 2 to 4 in the 23 s after `down`. A wider probe kept it between runs and after an errored run (ticks and failures went on), kept it while both daemons were disabled (ticks 4 to 7), and stopped it once the schedules were removed from configuration, after which nothing ran | A schedule removed from configuration counts until the supervisor's next check (about 10 s) has written it out. A schedule only in a configuration the supervisor has not yet registered is seen once it does (list reports such daemons as `available`, which already keeps it). Stack adds no scheduling feature |

Superseded claims of the follow-up entry above:

- "A command holds a shared lock per resolved supervisor state directory
  (`<state>/supervisors/<hash>.lock`)": the lock is now in that directory itself and shared by
  every Stack state.
- "The lock orders this Stack state's commands only": it now orders every Stack state's
  commands that use the supervisor; only session records stay per state.
- "A stopped daemon that has a schedule counts as stopped": it now keeps the supervisor,
  disabled or not.

After both fixes, the runtime tests that ran `up` without naming a Pitchfork state directory
put the new lock file in the caller's default directory. `8205aa9` gives every runtime fixture a
directory of its own (see REMEDIATION.md).

Unchanged: ownership marks, the other usage checks, the never-start-to-check rule, the CLI and
MCP defaults and opt-outs, `gc`, leases, disposal, snapshots and H1. The review's 19 ordinary
assertions (its `positive.py`, paths only changed) all pass with the corrected binary. Evidence
is in `eval/results/agentdx-supervisor-corrections-r2-20261021/` (git-ignored). The pilot report
is unchanged (`60aa3b11…5f84`), and no agent trial was rerun.

## Correction 2026-10-21, round 3: review of `a09676d` (SC3-F1)

A fresh independent review (Astra) of `a09676d` (code `4ae0435`) confirmed SC2-F2 and the
new-session case of SC2-F1, and requested changes for one P1 left in SC2-F1, reproduced with the
frozen `c4f04430…3255` binary and real Pitchfork 2.29.0. It is corrected below. The entries above
stay as written; the claims this correction supersedes are listed after the table.

| # | Problem | Kind | Before | Fix | Proof | Remaining limit |
|---|---|---|---|---|---|---|
| SC3-F1 | A reused session (an unchanged generation `up` starts again) took no supervisor lock when its record named no supervisor, or the lock of the supervisor it had recorded when its environment now reached another one, while its start requests went to the supervisor another checkout's `down` was deciding to stop. That `down` then stopped it under B's freshly started service | defect | The start helper chose its lock by reloading the session's optional `provider`, which `up` rewrites only for a new or changed generation, and only after starting. `restart` and `down` chose theirs from the recorded provider too. The review's wrapper (real `pitchfork list` answer held at most 12 s): with B's record naming an older Pitchfork directory, B's `up` returned healthy in 1.57 s, then A stopped the supervisor and B's service died; the same with B's record lacking `provider` (1.46 s). The new runtime test fails on the `a09676d` source for a moved and a legacy record under `up` and `restart`, and a moved record under `down`, each at "B's `…` finished while A was deciding"; its current-record control passes | `3e0cd74` | `up` and `restart` pass the state directory of the supervisor their requests reach (predicted from the command's own environment and provider configuration, as for the socket preflight) to every start and stop helper, which lock it; `down` and lease GC lock the supervisor `mise daemons stop` reaches. A reused session's record names that supervisor before anything starts there (`restart`: in its launch record). A command that cannot predict it asks the supervisor nothing and fails with `supervisor_unknown`. Runtime tests `a_reused_session_asks_the_supervisor_its_environment_reaches_only_once_a_down_deciding_to_stop_it_is_done` (moved, legacy and current records; `up`, `restart`, `down`) and `a_command_that_cannot_tell_which_supervisor_it_reaches_asks_it_nothing` (fails on the old source: `up` succeeded). The review's scripts, with only their output path and binary changed: B now waits for A in the moved-registered case (15.79 s) and the legacy case (16.36 s), as in the current-record control (15.96 s); A stops the then-unused supervisor and B starts afresh and stays healthy with the same PID and open port; its record names the current supervisor with unchanged configuration and lock digests | `down`'s decision to stop a supervisor still targets the one the session recorded (else the one the environment names), explicitly and under that one's lock; `down` on a session that ran elsewhere does not stop services under a supervisor its environment no longer reaches (unchanged). Without `PITCHFORK_STATE_DIR` and `HOME` (or as root without `PITCHFORK_STATE_DIR`) `up`, `restart` and a `down` with something to stop now fail instead of asking unguarded. `mise daemons` listings, which may start a supervisor, stay outside the lock as before. Forced interleavings, not natural frequencies |

Superseded claims of the round-2 correction above:

- SC2-F1 "corrected": it was corrected for a new session only. A reused session with a moved or
  missing provider record was still unguarded until `3e0cd74`; the round-2 proof (B waits 16.0 s)
  used a new session.
- "Nothing is held for an unknown supervisor" (the shared-lock design): a command now asks
  nothing of a supervisor it cannot name.
- "A checkout records its session before it asks": it now does so for a reused session too;
  before `3e0cd74` the record was refreshed only after the start.

Unchanged: SC2-F2 and its schedule rules, ownership marks, the other usage checks, the lock's
location and replacement handling, the never-start-to-check rule, the CLI and MCP defaults and
opt-outs, `gc`'s handling of deleted checkouts, leases, disposal, snapshots and H1. Two restart
runtime tests now make the environment fail after the restart's own start rather than `up`'s,
since `restart` reads it before stopping. Evidence is in
`eval/results/agentdx-supervisor-corrections-r3-20261021/` (git-ignored). The pilot report is
unchanged (`60aa3b11…5f84`), and no agent trial was rerun.

## Correction 2026-10-21, round 4: review of `a49f271` (SC4-F1)

A fresh independent review (Astra) of `a49f271` (code `3e0cd74`) confirmed the SC3-F1 cases for a
stable provider environment, SC2-F2 and the earlier controls, and requested changes for one P1,
reproduced twice with the frozen `de8b4536…f7e6` binary and real Pitchfork 2.29.0. It is
corrected below. The entries above stay as written; the claims this correction supersedes are
listed after the table.

| # | Problem | Kind | Before | Fix | Proof | Remaining limit |
|---|---|---|---|---|---|---|
| SC4-F1 | `up` chose the supervisor (and its lock) from the project environment before installing and running setups, while every later request rendered that environment again. A supervisor-changing `setup` (it rewrote the file a `PITCHFORK_STATE_DIR` `[env]` template reads) sent B's start to a supervisor whose lock B did not hold, while A's `down` was deciding to stop it; the session kept naming the first one | defect | The review: B's `up` predicted O, its setup made the template name S, B locked O and started through S in 1.62 s (repeat 1.58 s) while A held S's lock; A then stopped S and B's service died; B's record still named O. Reproduced here with the same binary and the review's script, adapted only to record either outcome (`probes/sc4/old-changed`: 1.63 s, then killed, record O). On the old binary `restart` also reported success with the record naming O while its service ran under S, and a later dependency wave left db under O and api under S (`probes/sc4/paths-old`). The four new runtime tests fail on the `a49f271` source at the intended assertions; the stable-template control passes there | `ea8cbd9` | mise's `[env]` wins over any value Stack could pass, so a request cannot be pinned to the chosen supervisor. The supervisor is chosen once per command; with its lock held, the environment is confirmed to still name it before the supervisor's own start and before every start or stop request (`up`, `restart`, every dependency wave, `stop_previous`, `stop_dependents`, `down`, lease GC). A change is refused with `supervisor_changed` before the request: nothing more is started or stopped through either supervisor, the session keeps naming the chosen one (which runs everything the command started), and a retry chooses the new one from its beginning. After the requests, still under the lock, the environment is read once more: a change found then (another program, at the moment of a request) fails the command with `supervisor_changed` and `asked: true` instead of reporting the start or stop as done. Runtime tests: `a_setup_that_changes_the_supervisor_the_environment_names_starts_nothing_through_either` (changed: refused while A decides, no request from B, record O; then a retry starts under `pf`; stable control: waits for A), `a_restart_whose_setup_changes_the_supervisor_starts_nothing_through_the_new_one`, `a_later_wave_whose_setup_changes_the_supervisor_is_refused_before_its_request`, `a_supervisor_change_made_as_a_start_is_requested_is_reported_not_claimed_as_a_start`; the fake mise renders `PITCHFORK_STATE_DIR` from the project's file and records the directory each actual request reached. Real Pitchfork (`probes/sc4`): B refused in 0.63 s (repeat 0.45 s) while A decided, nothing listening; once A finished, B run again started under S, healthy with the same PID, record naming S; stable control waits 16.29 s and stays healthy; restart and a later wave refused before their start, with no supervisor ever answering in S | Prevention covers every change Stack's own steps can make (installs, setups, earlier waves) and any made before the confirmation. A change another program makes between the confirmation and mise's own rendering, milliseconds later, cannot be prevented: it is found afterwards, if it persists until the requests return, and the command fails rather than claiming success; one changed and changed back within that window is not seen. A refused command leaves what it already started (earlier waves, a restart's stop) recorded under the chosen supervisor; `down` from an environment that now names another one stops those processes by their recorded identity, not through mise (unchanged). `mise daemons` listings, which may start a supervisor, stay outside the lock as before. Each locked section now renders the environment once after taking the lock and once after its requests, and once more before each start request. Forced interleavings, not natural frequencies |

Superseded claims of the round-3 correction above:

- "Every start and stop request locks the supervisor it actually reaches": it locked the
  supervisor predicted when the command began, which a setup could change before the request.
  Since `ea8cbd9` the prediction is confirmed under the lock before each request, and a change is
  refused.
- "A reused session's record names that supervisor before anything starts there": true of the
  supervisor predicted at the start; a request that then went elsewhere was not recorded.

Unchanged: the SC3-F1 and SC2-F1 lock choice for a stable environment, SC2-F2 and its schedule
rules, ownership marks, the other usage checks, the lock's location and replacement handling,
the never-start-to-check rule, the CLI and MCP defaults and opt-outs, `gc`'s handling of deleted
checkouts, leases, disposal, snapshots and H1. One runtime fixture changed: the slow-environment
test now skips one lookup (`slow-env-skip`), the start's own look after its request, so its slow
lookup still falls in verification, as its unchanged assertions require. Evidence is in
`eval/results/agentdx-supervisor-corrections-r4-20261021/` (git-ignored). The pilot report is
unchanged (`60aa3b11…5f84`), and no agent trial was rerun.

The review's own launch incident (a schedule launcher that continued after its isolation failed
and ran Pitchfork against the default location) is separate from SC4-F1 and is recorded with
this round's evidence (`review-incident/INCIDENT.md`). Possible residue in the default Pitchfork
state remains unverified; nothing there was inspected or cleaned.

## Correction 2026-10-21, round 5: review of `b7d95c9` (SC4-F1, still open)

A fresh independent review (Astra HIGH) of `b7d95c9` (code `ea8cbd9`) kept SC4-F1 open as a P1.
Its own at-request test accepted a start that reached the other supervisor (last request `=> pf`,
an error with `asked: true`, the session still naming `pf-old`), and the review gave a
source-derived schedule in which that start runs under S while A's `down` holds S's lock, and A
then kills it. The round-4 entry above stays as written; the claims this correction supersedes
are listed after the table.

| # | Problem | Kind | Before | Fix | Proof | Remaining limit |
|---|---|---|---|---|---|---|
| SC4-F1 | Every start, stop and supervisor-start request rendered the project's `[env]` again inside mise, after Stack's last look, and nothing bound that rendering to the supervisor whose lock the command held: a `PITCHFORK_STATE_DIR` template whose input changed in between sent the request to a supervisor another checkout was stopping. A check after the request could only report it | defect | Real mise at the dispatch boundary (`probes/gate-real-mise`): with a template that renders O for `mise env` and S while `mise daemons start` renders, a logging stand-in `pitchfork` received `PITCHFORK_STATE_DIR=S` for `usage`, `config add` and `start`. Real mise + Pitchfork 2.29.0 with r4's frozen binary `44cf6f67…f424f` (`probes/sc5/old-at-request`, repeat `old-at-request-repeat`): A's `down` held S's lock, deciding; B's service ran under S (PID 23648; repeat 75215), its record naming `pf-old`; A then stopped S (`stopped: true`), B's process was gone, its port closed and B unhealthy. Because the template rendered O again afterwards, the r4 after-request check saw nothing. The three new runtime tests fail on the `b7d95c9` source (`before/`): B's `up` returned `supervisor_changed` with `asked: true` after its start reached `pf`; the supervisor's own start in `up` reached `pf`; no binding directive | `71e401b` | The binding is inside the project's own `[env]`, which wins. The generated configuration ends its `[env]` with `_.file = "{{ get_env(name='STACK_SUPERVISOR_BINDING', default='.stack/provider-config/no-supervisor-binding.json') }}"`. Only Stack's start-capable requests set the variable: the supervisor's own detached start, `mise daemons start`, and `mise daemons stop` (from `up`, every wave, `restart`, `stop_previous`, `stop_dependents`, `down`, lease GC). They do so through a `mise::Target` that only `using_supervisor`, the lock holder, creates. It names `$STACK_STATE_DIR/supervisor-bindings/<sha256 of the directory>.json`, holding `{"PITCHFORK_STATE_DIR": <chosen directory>}`, and the request also inherits that directory. mise 2026.10.3 renders the environment once per `mise daemons` command and gives it to each Pitchfork call it makes (`src/daemons/runtime.rs`). A later directive decides over an earlier one and over a tool's environment (`src/toolset/toolset_env.rs`), and a `.json` env file is read literally (`src/config/env_directive/file.rs`). So the request reaches the chosen directory whatever the template renders. Without the variable the directive's path never exists and loads nothing. The variable is removed from what every provider command inherits. A binding path mise would read as a glob pattern is refused (`supervisor_unbound`) before the request. The confirmation before each request stays; the after-request check, which could only report, is removed, and with it r4's slow-env fixture adjustment. Tests: `a_supervisor_change_made_as_a_start_is_requested_cannot_take_it_outside_the_lock` replaces the at-request test. With A's shutdown of `pf` in progress and the input changed as mise renders B's start, every request B makes reaches `pf-old`; only start, stop and supervisor-start carry a binding, and an inherited one is ignored. B returns while A decides, its record names `pf-old`, and A's stop of `pf` (which, in the fake, ends the services that supervisor runs) leaves B's process alive and B healthy. `every_request_reaches_the_supervisor_its_command_chose_however_the_environment_renders_it` makes the change at the supervisor's own start (`up`), at restart's stop and at down's stop: each reaches `pf-old`, then `supervisor_changed` before any start. `the_generated_environment_ends_with_the_binding_of_supervisor_requests` covers the directive's place, and unit tests cover the binding file, the refusals and the stripped inherited variable. The fake mise models the directive as mise applies it. Real mise + Pitchfork 2.29.0 with the frozen `9c86dec0…4faf` (`probes/sc5/new-at-request`, repeat): the template rendered S for both start requests, yet B ran only under O (PID 74069; repeat 74655), returned healthy while A decided, record O, and kept the same PID, open port and health after A stopped S | Bound are the requests that may start or stop a daemon or the supervisor; `mise daemons --json` and `mise env` listings, which start nothing, still render the project's environment as written (status checks may then look at another supervisor's records; nothing is started or stopped by them). The generated configuration of every project with services now carries one fixed Stack-owned template (`get_env`), whichever the project declares; no project value is placed in it. A supervisor such a request starts inherits the variable, so the services it runs have `PITCHFORK_STATE_DIR` naming that supervisor, as long as the file is there, rather than their template's rendering. A `STACK_STATE_DIR` containing `*`, `?`, `[` or `]` cannot bind and refuses service requests (`supervisor_unbound`). Binding files are not removed (one small file per directory used). Checked with Pitchfork 2.29.0 (the pinned release) and mise 2026.10.3 (macOS: source, dispatch-boundary probe, races) and 2026.9.18 (local LinuxKit arm64: the at-request race); another mise's `_.file` or rendering rules would need the same check. Forced interleavings, not natural frequencies |

Superseded claims of the round-4 correction above:

- "mise's `[env]` wins over any value Stack could pass, so a request cannot be pinned to the
  chosen supervisor": an inherited value cannot, but a directive Stack writes into that `[env]`
  can. Since `71e401b` every start-capable request is pinned that way.
- "A change another program makes between the confirmation and mise's own rendering cannot be
  prevented: it is found afterwards ... and the command fails rather than claiming success":
  that after-request check is removed. Such a change no longer redirects the request, and a
  command whose requests all reached the chosen supervisor reports what they did.
- The at-request test `a_supervisor_change_made_as_a_start_is_requested_is_reported_not_claimed_as_a_start`,
  which required the start to reach `pf`, is replaced by the prevention test above.

Unchanged: the confirmation before each request and its `supervisor_changed` refusal, the
choice of supervisor and its lock, SC2-F2 and its schedule rules, ownership marks, the usage
checks, the lock's location and replacement handling, the never-start-to-check rule, the CLI and
MCP defaults and opt-outs, `gc`, leases, disposal, snapshots and H1. Evidence is in
`eval/results/agentdx-supervisor-corrections-r5-20261021/` (git-ignored). The pilot report is
unchanged (`60aa3b11…5f84`), and no agent trial was rerun.

## Correction 2026-10-21, round 6: review of `d1f6dba` (SC5-F1)

A fresh independent review (Astra HIGH) of `d1f6dba` (code `71e401b`) confirmed the round-5
binding for a configuration generated since `71e401b`, and found SC5-F1 (P1). A `mise::Target`
did not establish that the configuration its requests load carries the binding directive. `down`
reuses the generated configuration as it finds it, and one generated before `71e401b` lacks the
directive, so its `PITCHFORK_STATE_DIR` template still decided where the stop went. The round-5
entry above stays as written; the claim this correction qualifies is listed after the table.

| # | Problem | Kind | Before | Fix | Proof | Remaining limit |
|---|---|---|---|---|---|---|
| SC5-F1 | After an upgrade, `down` (CLI and MCP `stack_down`) and lease expiry used a pre-binding generated configuration unchanged. The binding file was written and the chosen supervisor's lock held, but nothing loaded the file, so the stop reached whatever supervisor the project's template rendered at that moment, including one whose lock another command held alone | defect | The review's real-provider probe (`review-evidence/agentdx-supervisor-d1f6dba.llUw5L/legacy-config-service`), repeated here with the frozen r5 binary `9c86dec0…4faf` in new isolated roots (`probes/before-cli`, `probes/before-mcp`). r4's binary wrote the configuration and started the service on O. A separate instance with the same qualified id ran on S, whose lock the probe held exclusively, and the template rendered S only for `mise daemons stop`. The CLI `down` and the MCP `stack_down` each ended S's instance (PID gone, port closed), left O's recorded service running, and returned `stop_unconfirmed` after 20.2 s and 20.4 s; the configuration stayed unchanged. On the `71e401b` source (`before/`), both new runtime tests fail: the stop of a pre-binding configuration reached `pf`, and an unbindable configuration was stopped instead of refused | `b66e70a` | A Target is made only once the project's generated configuration applies the binding (`mise::bind_config`, under the project's lock). A configuration that starts with the line `stack compile` writes but lacks the directive gains it in place, after every other `[env]` entry, with nothing else changed, in one rename that keeps the file's permissions; the result is what `compile` writes for the same inputs. stack.toml and stack.lock are not read, so a project whose manifest changed since is still stopped through the configuration it was started with. A configuration that is not valid TOML, was not written by `compile`, or already loads another `_.file` is left alone, and nothing is asked (`supervisor_unbound`, hint `stack compile`). Every start, stop and supervisor-start request also confirms, as it is made, that the directive is still last, and is refused otherwise. Audit: `up`, `stop_previous` and `restart` write the configuration again before any request; `down` and lease expiry reuse it and now go through the migration; deleted-project reclaim asks Pitchfork directly by the recorded state directory, not through mise. Tests: `a_configuration_written_before_the_binding_still_binds_the_stop_of_down` (CLI, MCP and changed-manifest cases; S's lock held alone; the template renders `pf` at the stop; every request reaches `pf-old`; the configuration becomes byte-for-byte what `compile` wrote, mode kept; the service stops), `a_configuration_that_cannot_bind_a_stop_asks_the_supervisor_nothing` (another `_.file`, a hand-written file: `supervisor_unbound`, no request, file unchanged, service running, then `compile` and `down` stop it), and two unit tests (migration with a bundle `_.path`, absent `[env]`, the per-request refusal after a later change; refusals and directive order). Real mise + Pitchfork 2.29.0 with the frozen `93c013b6…2ce2` (`probes/after-cli`, `probes/after-mcp`): same probe, same held lock and template; `down` returned `ok` in 2.1 s (CLI) and 1.5 s (MCP), stopped O's service and O's Stack-owned supervisor, and left S's instance alive with its port open. The migrated configuration is byte-identical to the one the control's `compile` wrote. Each run's control (current compile, S's instance restored, the same lock and template) also stopped O and kept S's instance | Bound only where the generated configuration is the one mise loads (the selection Stack sets, unchanged since round 5). A project without the file has no `[env]` that could name another supervisor, and its requests are not refused. A configuration `compile` did not write, or one carrying another `_.file`, is refused rather than repaired; `stack compile` fixes it, and that needs a manifest and lock that compile. Checked with mise 2026.10.3 (macOS) and Pitchfork 2.29.0; on local LinuxKit arm64 with mise 2026.9.18 too (not a hosted runner). Forced interleavings, not natural frequencies |

Qualified claim of the round-5 correction above: "every start, stop and supervisor-start request
is bound" held only for configurations generated since `71e401b`. Since `b66e70a` it holds for a
pre-binding configuration too, which is migrated before the first request, and any other
configuration that does not end its `[env]` with the directive is refused before a request.

Probe note: in the first attempt (`probes/attempt1/EXCLUDED.txt`, kept), both fixed-binary runs
returned the expected `down` result, but their controls timed out. The review's probe registered
S's instance in the probe root's single Pitchfork config directory, under the project's
namespace. Once the fixed `down` stopped O's supervisor, the control's `up` started a fresh one,
which read that registration and launched S's definition. The second attempt gives S its own
`PITCHFORK_CONFIG_DIR` (`scripts/legacy-config-service.py.diff`); `probes/diag-up-after-cleanup`
shows `up` after a supervisor-stopping `down` taking 2.96 s without it.

Unchanged: the binding mechanism and its file, the confirmation before each request, the choice
of supervisor and its lock, SC2-F2 and its schedule rules, ownership marks, the usage checks, the
lock's location and replacement handling, the never-start-to-check rule, the CLI and MCP
defaults and opt-outs, `gc`, leases, disposal, snapshots and H1. Evidence is in
`eval/results/agentdx-supervisor-corrections-r6-20261021/` (git-ignored). The pilot report is
unchanged (`60aa3b11…5f84`), and no agent trial was rerun.

## Commits

| Commit | Subject |
|---|---|
| `4550b15` | fix(run): keep the verified loopback URL in a task's environment |
| `c6c54aa` | fix(exec): pass external SIGTERM on to a running command and its group |
| `2b0905b` | fix(exec): report a command a signal ended as 128 plus the signal |
| `f2277c1` | fix(postgres): create new clusters UTF8 whatever the caller's locale |
| `81118e0` | fix(run): resolve a task's secrets through the fnox its run just installed |
| `820d45c` | fix(down): confirm nothing runs in a checkout that never ran up |
| `227f9c0` | feat(gc): reclaim what a removed checkout leaves behind |
| `173f677` | fix(provider): keep a cold mise update check out of the version probe |
| `052dacd` | fix(cli): give every usage error a hint and exit status 2 |
| `b287236` | fix(cli): name the missing argument in a --json usage error |
| `2fac65e` | fix(session): say which commands keep a session busy and what to do |
| `6ab0e92` | fix(mcp): report a malformed secret name as invalid_secret, as the CLI does |
| `396cd67` | docs(guarantees): state what an error envelope carries |
| `63aa997` | fix(status): report a killed service as not running, not as a changed process |
| `8fcadfb` | fix(up): name the ready_cmd that kept a start from finishing |
| `54d7684` | fix(status): do not call a session with an expired lease healthy |
| `46a50af` | feat(down): stop a root's own supervisor when nothing else uses it |
| `9f36c3e` | fix(session): keep services running when only tasks change |
| `7b223c1` | feat(restart): restart exactly the services whose watched files changed |
| `2041a32` | feat(services): restart dependents with the service they depend on |
| `5cea775` | feat(services): run a service's setup before it starts |
| `0442ee7` | feat(doctor): warn about another stack on PATH or a lock it wrote |
| `9518243` | feat(mcp): advertise the result envelope as every tool's outputSchema |
| `cfb55a6` | feat(cli): add stack init to write a starting stack.toml |
| `e9e6c83` | docs(secrets): show a fnox.toml with fake values |
| `6d3a137` | fix(exec): say to repeat --require when given a comma list |
| `e38349b` | docs(worktrees): stop pinning the isolated-run recipe to 0.1.21 |
| `5fe091a` | fix(up): refuse Postgres as root before downloading anything |
| `2aef819` | docs(identity): say that commands see the identity tokens |
| `ec06451` | docs(up): say where a service's data directory is |
| `15ff636` | fix(redis): start under any caller's locale |
| `c25702f` | fix(init): publish stack.toml whole and never over another file (F4) |
| `33a9a07` | fix(up): let a signal during a service's setup cancel the start (F2) |
| `2afca9f` | fix(restart): check what a service depends on before touching it (F3) |
| `18e50a7` | fix(gc): reclaim only the data directory up saw, through what was checked (F1) |
| `dbb5085` | fix(process): act on every signal a command got before judging it (R3-F2) |
| `ba5360f` | fix(restart): never take a ready_cmd that could not run as passing (R3-F3) |
| `f6a87b6` | fix(process): admit a command only past a boundary every signal before was handled at (R4-F1) |
| `c0e6238` | fix(process): watch a command for signals from its naming, not its run's first look (R5-F1) |
| `34369ef` | fix(process): decide what an exited command keeps only once its signals are handled (R5-F2) |
| `b80318c` | feat(down): stop the supervisor Stack started once nothing else uses it (follow-up 2026-10-21, O1 decision) |
| `9946319` | fix(down): coordinate with every Stack state that uses the same supervisor (SC2-F1) |
| `4ae0435` | fix(down): keep the supervisor while a daemon it runs has a schedule (SC2-F2) |
| `8205aa9` | test(runtime): give every fixture its own Pitchfork state directory (after SC2-F1) |
| `3e0cd74` | fix(supervisor): lock the supervisor each request reaches, not the recorded one (SC3-F1) |
| `ea8cbd9` | fix(supervisor): refuse a request once the environment names another supervisor (SC4-F1) |
| `71e401b` | fix(supervisor): bind every start and stop request to the supervisor whose lock is held (SC4-F1, round 5) |
| `b66e70a` | fix(supervisor): bind the stop of a configuration written before the binding (SC5-F1) |
