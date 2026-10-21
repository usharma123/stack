# Independent Astra evaluation of Stack 0.1.22

Stack successfully provisioned and isolated real services, and I completed two small application tasks through its CLI and stdio MCP. I would not treat this build as reliable for unattended agent work yet. Two verified failures matter directly: service URLs change between `exec` and `run`, and SIGTERM to captured CLI execution leaves the command tree running.

This was one hands-on tester session on 2026-10-20. I did not read the other tester's report, findings, or the parent benchmark draft. I did not delegate, modify Stack product code, implement an architecture change or benchmark replacement, commit, push, or create a PR. The application fixes are evaluation fixtures. This is neither exhaustive testing nor a matched productivity comparison.

## Identities and evidence

- Worktree: `/Users/utsavsharma/.t3/worktrees/stack/t3-e39fed11`.
- Source: `8b3402fa1db8f11917b5ba33a333c9093545d9ce`, clean product diff before and after. Cargo and npm manifests say 0.1.22.
- Report and scripts: `/Users/utsavsharma/.t3/worktrees/stack/t3-e39fed11/eval/agentdx-20261020/astra`.
- Raw receipts: `/Users/utsavsharma/.t3/worktrees/stack/t3-e39fed11/eval/results/agentdx-astra-20261020-094034`. Each command JSON records argv, cwd, PID, exit status, elapsed seconds, stdout, stderr, and outer-timeout status. MCP JSON records requests and responses, including structuredContent. `command-index.md` indexes them.
- Native binary: `/Users/utsavsharma/.t3/worktrees/stack/t3-e39fed11/eval/results/agentdx-astra-20261020-094034/build/debug/stack`, built by `cargo build --locked`, SHA-256 `e9292bcc68ab01738c379ed94841cba1607d47f4ea9ef71724c17aa95ed3cd6a`.
- Linux source-build binary: `/work/src/target/debug/stack` in the run-owned container, SHA-256 `8ec642007d4b84c4bde99a05a2db53b41713ec5ec8c51dd40add62c4764c5e4e`. Linux aarch64 on Docker's LinuxKit kernel, non-root UID 1000 for the successful service test.
- Native host: macOS Darwin 24.6.0 arm64. Initial service run used existing mise 2026.10.3 with empty isolated stores. A PATH without mise successfully downloaded Stack's tested mise 2026.9.18. The URL failure also reproduced with that downloaded release. Linux setup downloaded mise 2026.9.18 from scratch.
- Runtime pins: Python 3.13.16, uv 0.12.23, PostgreSQL 17.11, Redis 7.4.7, Pitchfork 2.29.0, Node 24.22.0, jq 1.7.1, fnox 1.39.0.

Exact npm 0.1.22 returned E404 at the start of the run (`publication.json`). npm latest was 0.1.21. I installed the actual published `@ushawarma/stack@0.1.21` into an isolated prefix and verified `stack 0.1.21` (`published-old-install.json`, `published-old-version.json`). I did not use that older binary for the product tests, and did not call a local pack a published release. Publication status is a timestamped observation, not a claim that 0.1.22 can never become available.

## Application outcomes

| Task | Work performed and oracle | Outcome |
|---|---|---|
| Python invoice note feature and stale-cache fix | Seeded an existing PostgreSQL invoice, added a non-null note with a default through a SQL migration, compared rows before/after, reproduced a stale Redis response after PATCH, changed app code to persist notes and invalidate cache, restarted through MCP, asserted API response equals SQL row and Redis JSON | Passed through CLI/MCP exec. `stack run test` failed because of the verified URL bug |
| Second simultaneous checkout | Created a detached worktree at the same source commit, copied source and stack.lock without .stack state, ran locked replay, seeded a different customer, migrated and tested while the first checkout remained live | Passed. Separate ports, PIDs, data directories, live identity tokens, and customer values |
| Node catalog duplicate-SKU bug | Baseline accepted ` book-1 ` despite existing `BOOK-1`; fixed trim/uppercase normalization, restored the exact seed after the deliberately failing baseline, restarted through MCP, asserted HTTP 409 and unchanged JSON storage | Passed through MCP exec. Task execution reproduced the URL bug |
| Linux invoice replay | Fresh source build, clean setup, locked replay, real PostgreSQL/Redis/API, seed and non-destructive migration | Initial app request failed because SQL_ASCII returned bytes. After explicit database client encoding adjustment, unchanged application integration test passed. Task URL bug remained |

The invoice migration preserved `(1, 'Acme', 1200)` before any API change. The initial bug returned cached 1200 after PATCH requested 1225. The fixed test returned 1225 and `paid by bank`, with the database and Redis agreeing. These are application assertions, not merely service-health checks.

At the simultaneous identity checkpoint:

| Checkout | PostgreSQL / Redis / API ports | Database customer | Live API token |
|---|---|---|---|
| A | 27866 / 27328 / 24198 | Acme | `stack-88b0788e6686bd89df1c4a83f0b58bd1` |
| B | 23709 / 27316 / 22405 | Beta checkout | `stack-7a1452b4ec9ff3a5a0f9190e70e747af` |

Both fixed sources hashed to `f8bb85185104afa3727ea59cbec0061ac88b26fb746f2348cbe0c3090bb5ffa1`. `a-live-session.json` and `b-live-session.json` preserve service PIDs, separate data paths, and provider IDs. `a-live-identity.json` and `b-live-identity.json` read the actual databases and identity HTTP endpoints. A's 60-second Redis entry had expired by that checkpoint; its cache correctness was asserted earlier in the integration test, and B's live cache still contained its distinct customer. I do not claim the later null cache entry as evidence of a cache write.

## Verified defects

### 1. Task execution replaces verified custom-service URLs with unusable proxy URLs

Severity: high for agent task completion. Reproduced on macOS with mise 2026.10.3 and 2026.9.18, and on clean Linux with mise 2026.9.18. It affects CLI `run` and MCP `stack_run` in both application shapes.

A healthy stack's `stack exec` printed:

```text
API_URL=http://127.0.0.1:24198
API_PORT=24198
```

The same project's `stack run endpoints` printed:

```text
API_URL=https://api.t3-e39fed11.stack.localhost
API_PORT=24198
```

The generated configuration itself contains the loopback URL. Every live service check succeeds. The task then fails DNS resolution before reaching the app. The clean Linux equivalent was `https://api.ledger.localhost`; Node failed on `web.t3-e39fed11.stack.localhost`. Setting a deliberately wrong inherited API_URL did not explain the discrepancy: exec retained the correct loopback URL, while run still selected the proxy URL. Separate `sh`, `bash`, and `zsh` exec probes preserved the correct URL.

Receipts: `ledger-exec-endpoints.json`, `ledger-task-endpoints-retry.json`, `task-env-controlled.json`, `exec-env-controlled.json`, `mise-task-provider-version.json`, `mcp-ledger-fix.json`, `mcp-catalog.json`, `linux-task-endpoints.json`, `linux-task-test.json`.

Reproduce with the ledger fixture after compile/up: run its `endpoints` task, then compare with `stack exec -- python -c 'import os; print(os.environ["API_URL"])'`. Run the same test file via `stack run test` and `stack exec --require-all -- uv run python test_api.py`.

Relevant source: `src/provider/mise.rs:489` explicitly says loopback URLs must make services, tasks and exec agree; task planning/capture is at `src/session.rs:2768` and `src/provider/task_config.rs`. My causal inference is that the captured inherited-value template is evaluated against mise's daemon-generated environment during task execution. I did not modify product code to prove that internal mechanism. The externally observable mismatch and failure are proven.

Suggested correction and regression oracle: preserve the verified endpoint through task evaluation, then compare all task/exec endpoint values and make real HTTP requests under both provider releases. A task exit must not be accepted solely because Stack's envelope has `ok: true`.

Workaround used: execute the same integration test through `stack exec --require-all`. This is manual workflow deviation from the documented task loop.

### 2. SIGTERM to captured CLI execution leaves its command and descendant alive

Severity: high for cancelled agents and cleanup. Verified on native macOS, not generalized to every signal/platform or to MCP server cancellation.

I started `stack --json exec` running Python, which spawned `sleep 15`, recorded its PIDs/process group, and slept. After the PID file existed I waited another 0.5 seconds, then sent SIGTERM only to the known Stack PID. Stack exited with signal 15 and no JSON output. Python remained alive, reparented to PID 1, and its sleep child remained alive in the same group across three subsequent checks.

Concrete reproduction: Stack PID 11450, command PID/group 11520, descendant 11522. `cancel-repro-tree-before.json` records the original ancestry and start times. `cancel-repro-stack.json` records exit -15. `cancel-repro-after-0/1/2.json` show the orphaned tree. An earlier independent attempt also left a sleep child (`external-signal-child-after.json`). `cancel_repro.py` is reusable.

Relevant source: `src/process.rs:553` uses `OwnedChild` and `spawn_piped` for captured execution, while `src/process.rs:661` describes signal registration for other execution paths. The new child process group is visible in the live receipt. This supports the suspected missing captured-command forwarding path; no product patch was made.

Suggested correction and regression oracle: cancel an already-running captured command externally, verify both command and grandchild are gone, and verify an unrelated sentinel remains alive. Test CLI JSON, ordinary CLI, and MCP server cancellation independently. Checking only Stack's exit is insufficient.

The built-in deadline behaved differently and passed: `--timeout 1s` returned `timed_out`/124 and the recorded sleep descendant was absent. A child that killed itself with SIGTERM returned `exit_code: null`, as documented. Those passing cases do not cover external cancellation of Stack.

The orphaned reproduction processes had 15-second natural bounds. Final PID and owned-process scans verified their absence. No unrelated process was signalled.

## Workflow limits and platform friction

- **Linux database encoding:** a clean non-root Linux PostgreSQL preset initialized `server_encoding` and `client_encoding` to SQL_ASCII. psycopg returned bytes, and the API failed JSON serialization. `linux-api-failure-logs.json` records that exception; `linux-encoding.json` records both settings. `ALTER DATABASE postgres SET client_encoding TO 'UTF8'` made the unchanged app test pass. This is a verified portability/default issue, not evidence that Stack corrupts data. The server remained SQL_ASCII; the workaround is not a claim of a UTF8 database migration.
- **Root hosted environments:** root startup failed at PostgreSQL with an actionable message to create/use a regular user. Setup and installation succeeded first. A `launching` session was retained and `down` confirmed nothing remained. Hosted runners that start as root need explicit preparation. The successful Linux test used a user I created inside the run-owned container.
- **Dependents:** `depends_on` was rejected with a source location and accepted field list. A real worker loaded API version v1 at launch. After editing and restarting only the API, live requests showed API v2 / worker v1, with both services healthy. Restarting the worker yielded v2 / v2. This is a missing orchestration capability, not a broken promise of automatic restart propagation. Receipts: `dependent-naive-schema`, `dependent-stale-live`, `dependent-recovered-live`.
- **Watch semantics:** watched changes were reported and `up` retained the old API PID, as documented. Explicit MCP restart changed only the selected API PID and retained PostgreSQL/Redis PIDs. Agents must act on the warning.
- **Task/config edits:** adding the diagnostic task invalidated the session, even though service definitions did not change. My next two commands failed with `service_unavailable`; following the `up` hint restarted services and retained data. This is documented configuration fingerprinting, with a noticeable workflow cost.
- **Handoff:** a new MCP process discovered the live session and completed the same integration test without restarting it. This validates same-machine process/session handoff only. There is no evidence here of data snapshot transfer to another machine, hosted-agent handoff, or a handoff artifact beyond configuration/lock/session files.
- **Publication:** exact requested npm installation was blocked at 0.1.22 during this run. Source build bypasses that distribution prerequisite and must be scored separately.

## What worked and coverage

| Area | Evidence/result | Limit |
|---|---|---|
| Native noninteractive bootstrap | setup without mise on PATH downloaded 2026.9.18; empty-store compile/up installed real services without prompts | initial up used host mise 2026.10.3, not a pristine macOS installation |
| Linux noninteractive bootstrap | actual Linux source build, setup, locked replay and non-root service startup passed | local Docker LinuxKit aarch64; no hosted-agent vendor, Linux x64, remote sandbox, or network-policy result |
| Tools only | local bundle jq install, task and exec passed | one backend |
| Bundles/locked replay | bare path failed with useful `path:` hint; local content drift rejected by `--locked`; restored bytes replayed | Git/OCI publishing and moved tags unrun |
| Checksums | fresh jq install with deliberately changed expected SHA failed `artifact_mismatch`, naming expected/actual/URL; restored lock installed successfully | no tampered signer or installed-cache corruption test |
| CLI/MCP discovery/errors | CLI help, MCP initialize/tools/list, string command, unknown `timeout`, conflicting compile flags, unknown tool tested | not a full protocol conformance or concurrent-client suite |
| Fake secret grants | pinned real fnox/plain provider; granted stdout/stderr and timeout output redacted; ungranted variable absent; protected/missing names rejected; no fake value found in scanned Stack artifacts | no real secret/provider/login, encoding transformation, or OS dialog test |
| Nonzero/signal/timeout | exit 7 propagated; child signal produced null exit code; deadline returned 124 and removed child; partial startup retained PID and was stopped | external JSON cancellation failed; MCP server cancellation and other signals unrun |
| Service death/recovery | signalled only recorded Redis PID after ps verification; status unhealthy, exec poisoned URL, require-all refused; up recovered and real app test passed | no machine reboot or uninterruptible process |
| Checkout isolation | two simultaneous Git worktrees, distinct live services/data/tokens, same source/lock | agents were driven by one tester, not parallel independent agent subjects |
| Disposal/recreation | down, preserved/renamed old app directory, recreated same path; old PG/Redis data quarantined with warnings, new DB had no invoices table | did not delete a live checkout and exercise gc_incomplete recovery |
| Lease/GC | TTL 1s, waited 2s without renewal; GC stopped service; status had no session | owner-PID leases and long-running GC watcher unrun |
| Cleanup | native/Linux down, supervisor status down, ports closed, known PIDs absent, global receipts identical, container/worktree removed after evidence preservation | Linux had defunct supervisor children under sleep PID 1 until container removal, no live services |

The native setup, compile and first up receipts took about 7.25s, 6.44s and 10.82s respectively. Linux setup took 6.87s and up 11.25s. These are single command timings under this machine/network, with different preconditions. They are not speedups or statistically meaningful benchmark results.

## Attempts, lookups, and manual work

See `discovery-log.md` and the complete command index. Key deviations were retained rather than overwritten:

1. Exact npm version E404. Built pinned source and separately verified older publication.
2. Docker `bash -lc` lost Cargo from PATH and returned 127. Retried `bash -c`; source build passed. This was a test-runner mistake, not a Stack failure.
3. First ledger task failed on DNS, hiding the intended cache bug. Added an endpoint diagnostic task; forgot that changing config requires up, received the useful session error twice, then followed the hint. Direct exec exposed the intended cache bug.
4. Completed fixture code, observed watch warnings, tried up, then explicitly restarted through MCP. App integration passed through exec; task still failed.
5. Tested shell/env/provider alternatives and clean Linux to separate the URL defect from host configuration.
6. The deliberately failing catalog test inserted a duplicate. Restored the known seed before validating the fix, and disclosed that test-state repair.
7. Linux SQL_ASCII required a manual database setting before app integration passed.
8. Tried a bare local-bundle path and `depends_on`; both produced actionable schema/source errors. Checksum, MCP malformed calls, missing secret and timeout failures were deliberate negative cases.
9. External cancellation left children alive. Preserved their live ancestry, repeated after startup settled, and verified eventual cleanup.

No external human intervention was requested in this tester session. That does not mean zero intervention cost: fixture creation, environment isolation, docs/source investigation, recovery commands and database preparation were all tester work and must count in an evaluation.

## Proposed replacement benchmark, not implemented

Use paired application tasks against each agent's actual existing setup, with equivalent starting code, service versions, seeded data, acceptance tests, network conditions and permissions. The treatment is Stack's CLI/MCP workflow. The control is the setup the agent would otherwise use for that project, such as existing Compose, direct mise/Pitchfork, or documented native services. Do not compare product marketing matrices or reward an empty tool invocation.

Pre-register the unit of evaluation as a completed task plus a clean resource handoff. Block/randomize by application, model, agent interface, OS and cold/warm caches. Use independent task variants or fresh agent contexts to avoid learning the solution from the paired run. Preserve failures and infrastructure blockers. Evaluate published 0.1.22 availability before assigning that treatment; score source-build recovery separately. Run enough independent task/seed repetitions to report uncertainty, not just means from one script.

Primary outcome: accepted application behavior with seeded data preserved and no wrong-checkout access or leftover live resources. Count completion without human intervention separately from completion after help. Secondary measures: human interventions, agent retries, invalid tool calls, docs/source lookups, recovery actions, wall time, model cost/tokens, and residual services. Keep active reasoning time separate from download waits. Do not infer productivity from the driver durations in this report.

Concrete hidden checks suggested by this evidence:

- Apply the note migration twice; preserve pre-existing rows and fields. PATCH must be reflected by a subsequent GET and cache read, and the other checkout's same invoice ID must stay unchanged.
- Compare exec/task endpoint environments and request the verified API through each. A healthy status alone is insufficient.
- Cancel the parent CLI/MCP process during a running command with a grandchild. Verify all owned descendants exit and unrelated sentinels survive.
- Restart one API after code changes; check loaded source/version and dependent worker state. Score required manual dependency restarts.
- Kill the recorded Redis process, require fail-closed endpoints, recover, and verify persisted SQL data and cache behavior.
- Start two checkouts from one lock, overlap their lifetimes, and inspect real DB/Redis/API identities, source hashes and ports.
- Replay on macOS and non-root/root Linux separately. Assert string round-trips and encoding, not just TCP readiness.
- Corrupt a fresh-download checksum, require refusal and useful diagnostics, and ensure no “update the lock to make it pass” workaround receives credit.
- Test malformed MCP arguments, nonzero task exits, secret redaction on normal/error/timeout paths, and actual tool-level success interpretation.
- Expire a lease, dispose/recreate a checkout, verify empty new data plus preserved quarantine, then check down, supervisor state, PIDs, ports and global receipts.

An independent evaluator should own the acceptance checks and seeded state. A successful script replay can validate an oracle, but cannot establish that an agent completed the work with less human help. No replacement benchmark code or architecture change was implemented here.

## Cleanup and retained resources

Original native isolated state was `/tmp/ax-v1te5s09`. HOME and auth were retained; Stack, mise, Pitchfork including PITCHFORK_CONFIG_DIR, XDG, npm, uv and fnox configuration paths were isolated. `isolated-env.json` lists exact values. No real secrets were used.

All discovered native project stacks were brought down, including partial startup and lease fixtures. Pitchfork was separately stopped and reported `down`. All ten observed native service ports were closed; every recorded native service PID plus the reproduced orphan PIDs was absent; an owned-path process scan returned `[]`. Nine global receipt entries from the user's mise/Pitchfork configuration/state trees matched before and after. See `cleanup-native-ports.json`, `cleanup-native-pids.json`, `cleanup-owned-process-scan.json`, `cleanup-supervisor-after.json`, and `global-comparison.json`.

Both Linux isolation roots were brought down, their supervisors reported `down`, and all six allocated Linux ports were closed. The pre-removal process list contained only PID 1 sleep, the ps command, and defunct Pitchfork children; no live app/database/supervisor remained. The named container `agentdx-astra-ax-v1te5s09` was removed after exporting `linux-evidence.tgz`, and the exact-name container query was empty. No Docker image or global resource was pruned.

The second Git worktree was removed only after preserving the original disposed app and recreated app. Its source commit is retained in `source.tar`; app evidence is in `disposed-b` and `recreated-b-preserved`. Product source still matches the pinned commit and `git diff --stat` is empty. Only requested evaluation artifacts are untracked.

Failed receipts, databases/quarantined data, logs, package provenance, fixtures and tool state are retained. `resources.json` records the final archive location of the native isolated root. It is intentionally preserved, not a running environment.
