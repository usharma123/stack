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
[Corrections after the independent review](#corrections-after-the-independent-review-round-2). Every row names its proof; "remaining limit" is what the fix
does not do.

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
