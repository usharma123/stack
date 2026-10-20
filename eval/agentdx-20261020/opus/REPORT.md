# Stack 0.1.22 as an AgentDX product: hands-on evaluation (Opus 5.5)

Date: 2026-10-20. Tester: Claude Opus 5.5, an independent hands-on session. No product code was
changed and nothing was committed, pushed or opened as a PR. This is one finite session: it is not
exhaustive, and it makes no statistical claims.

- Raw evidence (git-ignored): `eval/results/agentdx-opus-20261020T132129Z/`. It holds 149 command
  receipts in `receipts/`, 4 in `receipts-0121/`, the hosted bootstrap steps in `hosted-0.1.22/`,
  the MCP transcripts `mcp-tracker-a.json` and `mcp-lifecycle.json`, and the app repo bundle and
  state in `apps/`.
- Each receipt has a `.cmd` file (the argv and cwd), `.out`, `.err` and `.meta` (exit code and
  seconds). A `.note` file marks a receipt I mislabeled or that my own wrapper got wrong.
- Reusable scripts and fixtures are in this directory (see [Runnable checks](#runnable-checks)).

## 1. Provenance

| Item | Value |
|---|---|
| Source | `8b3402fa1db8f11917b5ba33a333c9093545d9ce` ("chore: release 0.1.22"), worktree `t3/6c91bf15`, clean |
| Built binary | `cargo build --release` (cargo 1.93.1). `target/release/stack`, `stack 0.1.22`, sha256 `43927ee313f05710c7008ebf820c22bcacca890fd23b9fa99e39482773562fbf` |
| Local package | `ushawarma-stack-0.1.22.tgz`, packed from `npm/` plus the binary above. sha256 `ee4c4bcb5db1303a84e5d8109dd7732aa0283f5f2d180e7a7a204bc3ee30cbad`. **Not a registry artifact** |
| Registry 0.1.22 | **Not published.** `npm install @ushawarma/stack@0.1.22` returns `ETARGET` (receipt 001); `npm view` returns 404; `gh release view v0.1.22` says "release not found". Latest published is 0.1.21 |
| Registry 0.1.21 (comparison only) | integrity `sha512-S6IhDsxrHYt05ybhWSkZQVu+c31l3CsS8lRFoxPnAtcW3j8xJfBlVFeuu/fAaQT30MXp9BYmXpkF62sxQb1buA==`, shasum `d8a32f1ed2cff743f733aab8228fe373f6a3a872`. Packaged darwin-arm64 binary sha256 `2b5390f0f289dc7d1ac62d8da175d5d50251f15815c20548ee37f8452a484f70` |
| Host | macOS 24.6.0 arm64, Node v24.16.0, npm 11.13.0, git 2.51.2. Docker present but **never used** |
| mise | Local runs: Homebrew mise 2026.10.3 on `PATH`. Hosted run: no mise on `PATH`, so `stack setup` downloaded mise 2026.9.18 into the isolated root |
| Pinned by projects | python 3.13.16, uv 0.12.23, pitchfork 2.29.0, postgres 17.11 (conda), redis 7.4.7 (conda), node 22.23.3, jq 1.7.1, fnox 1.39.0 |
| App repo evidence | `apps/tracker-all-branches.bundle` (sha256 `88e50f5b813f3349f7b047953900febef456272c2dc6b49f5e431e379680b301`), with branches `main`, `feat/priority-cache` and `feat/delete-search` |

### Isolation

[`env.sh`](env.sh) follows `docs/user/worktrees.md` and `examples/isolated-stack.sh`. It moves
`STACK_*`, `MISE_*`, `PITCHFORK_CONFIG_DIR`/`STATE_DIR`/`LOGS_DIR`, `XDG_*`, `UV_CACHE_DIR` and
npm into a short `/tmp/sa?.XXXX` root, and keeps `HOME`.

- **Global receipts.** Before and after files are `global-before.txt` and `global-after.txt`. They
  cover the sha256 of `~/.config/pitchfork/config.toml`, `~/.local/state/pitchfork/state.toml` and
  `~/.config/mise/config.toml`, plus listings of `~/.local/{share,state}/stack`, `~/.cache/stack`
  and `~/.local/share/mise/installs`. **Unchanged.**
- **Processes and sockets.** Pitchfork, postgres and redis-server processes are unchanged
  (`procs-*.txt`), and so is the full set of TCP listeners (`listen-*.txt`).
- **The one write outside the roots.** My very first `npm view` (before isolation) wrote an npm
  debug log under `~/.npm/_logs/`.

## 2. Prioritized verified bugs

### P1-1. `stack run` gives custom services the Pitchfork proxy URL, not the verified loopback URL

Affects both the CLI and MCP. Under `stack run`, every task sees `<NAME>_URL=https://<svc>.<project>.localhost`.
That hostname does not resolve, and Stack never starts the proxy. `stack exec` correctly sees
`http://127.0.0.1:<port>`.

- **When it happens.** It does not matter whether the task lists the service in `services`.
- **Impact.** The documented agent loop (`stack run test`) broke my first integration test run
  with `URLError: nodename nor servname provided` (receipt 017), while every service was verified.
  An agent following the docs ("`<NAME>_URL` ... set for every command") sees "healthy stack,
  failing tests" and may blame the app.
- **Evidence.**
  - Receipt 017 is the failing test run.
  - An inline env dump under `run` showed `API_URL=https://api.tracker.localhost` next to the
    correct `API_PORT=26212`. It was not saved as a receipt; receipts 021 and 135 reproduce it.
  - Receipt 021 is the minimal repro: `exec: http://127.0.0.1:21317`,
    `run with: https://web.urlrepro.localhost`, `run without: https://web.urlrepro.localhost`.
  - Receipt 135 (`mcp-lifecycle.json`) confirms it over MCP: `stack_run` gives the `.localhost`
    URL and `stack_exec` gives loopback.
- **History.** In 0.1.21 (`receipts-0121/004`), `exec` was wrong too. 0.1.22 commit `4a4ccf1`
  fixed `exec` only.
- **Root cause (source).** `src/provider/mise.rs:489-494` renders the loopback URL into `[env]`. It
  asserts that `[env]` "takes precedence over mise's daemon exports, so services, tasks and `exec`
  agree". That holds for `exec`, but not for tasks run through `mise run`, where the daemon export
  wins.
- **Test gap.** `tests/e2e/11-field-regressions.sh:30` checks `API_URL` only through `exec`.
- **Workaround I used.** `tests/conftest.py` rebuilds `API_URL` from `API_PORT`.
- **Repro.** [`fixtures/run-url-repro/repro.sh`](fixtures/run-url-repro/repro.sh) exits 1 on both
  0.1.21 and 0.1.22.

### P1-2. The Postgres cluster encoding and locale depend on the caller's locale

A minimal CI-style environment (`env -i`, so no `LC_*` or `LANG`) initializes the cluster as
`SQL_ASCII` with ctype `C`. My interactive shell (`LC_CTYPE=en_US.UTF-8`) gets `UTF8`.

- **Impact.** The same commit, with the same `stack.lock`, passed 8/8 tests locally and failed 6/8
  in the clean hosted bootstrap (`hosted-0.1.22/08-run-test.out`).
  - psycopg returns `bytes` for text on `SQL_ASCII`, so `json.dumps` crashes the API, which shows
    up as `RemoteDisconnected` in the tests.
  - `test_migration_applied` failed with `b'001_init.sql' != '001_init.sql'`.
- **Reproducibility gap.** Nothing in `stack.lock` or `doctor` records or pins this. The cluster is
  initialized once, so whichever shell first ran `up` in a checkout decides the encoding
  permanently.
- **Evidence.** [`pg_encoding.sh`](pg_encoding.sh) on the pg-only fixture:
  - Receipt 124: `LC_CTYPE=unset server_encoding=SQL_ASCII lc_ctype=C`.
  - Receipt 125: `LC_CTYPE=en_US.UTF-8 server_encoding=UTF8`.
- **Classification.** A product reproducibility defect. Stack composes the preset and claims
  machine-independent results. The initdb arguments may live in mise's postgres preset, which I
  did not inspect.
- **Preserved evidence.** The hosted root `/tmp/sah.v0Pz` is kept: services stopped, supervisor
  down, `SQL_ASCII` cluster intact.

### P2-3. `stack run` of a task with secrets does not install the pinned fnox

Docs (commands.md, `run`) say `run` "installs any missing pinned tool before it starts". With
fnox pinned but not installed, `stack run sign` instead failed `secret_unavailable: fnox is not
on the stack's PATH` (receipt 100).

- `stack doctor` reported `ok: true`, with the detail "pinned but not installed" (receipt 099).
- The hint (`stack install`) is actionable, and after it everything worked (receipts 101-103).
- Secret resolution simply runs before the auto-install, contradicting the docs.

### P2-4. Removed checkouts leak port reservations and data directories, and nothing reclaims them

The recommended hook sequence (`stack down`, then remove the worktree) still leaves state behind
(receipts 137-139, `ports-registry-final.json`).

- **Ports.** The two removed worktrees keep 3 port reservations each in `stack/state/ports.json`.
  `stack gc` returns `[]`, because the deleted-checkout path only covers checkouts that still have
  a session.
- **Data.** Their ~39 MB Postgres/Redis data directories stay under
  `$MISE_STATE_DIR/daemons/<hash>`.
- **What the docs cover.** Keeping the data is documented ("Data directories are kept"). The
  port-registry leak is not, and there is no command to reclaim either.
- **Impact.** On a long-lived agent host that churns worktrees, the 20000-29999 registry and the
  disk only grow.
- **Doc mismatch.** commands.md speaks of `<data dir>.replaced-<time>`, but data actually lives
  under mise's state dir, not `STACK_DATA_DIR`. That matters for isolation and cleanup tooling.

### P3-5. The error envelope breaks the "every error has a code, a hint and details" guarantee

guarantees.md makes that promise; these cases miss it:

- **Missing hint, or exit code 1 instead of 2.**
  - `--timeout 1ms` and `--ttl banana` give `usage` with no `hint` and exit 1 (receipts 064, 068).
  - CLI parse errors exit 2 with a hint (005, 069). Usage errors therefore have two exit codes.
- **Truncated message.** `stack --json logs` (no service) prints "the following required
  arguments were not provided:", cut off with the argument name missing (receipt 069).
- **`session_busy`** has no `hint` and no `details` (receipt 128). An agent cannot tell which
  command is running or what to do.
- **MCP and the CLI disagree.** `stack_exec` with `secrets: ["lower-case"]` gives `usage` (MCP),
  while CLI `--secret webhook_key` gives `invalid_secret` (receipt 108). secrets.md says "every
  problem is `invalid_secret`".

### P3-6. Diagnostics that point the agent the wrong way

- **A SIGKILLed Postgres** is reported as "service process changed since launch; run `stack up`"
  (receipt 046). Nothing was listening, so "not running" is the accurate reason.
- **A service whose `ready_cmd` never succeeds** times out with "`stack up` did not finish
  verifying this launch" (receipts 083-084). The `ready_cmd` failure is never named, and the hint
  ("`stack up` again keeps waiting") invites a pointless retry.
- **`status` stays `healthy: true` with `lease_expired`** (receipts 110, 133). The next `up` or
  `gc` will stop those services, and nothing in `healthy` says so.

### Observations (not clearly bugs)

- **Signal exit code.** A command killed by a signal gives `exit_code: null` (as documented), but
  the CLI exits 1 rather than 128+n, which looks just like an app failure (receipt 066).
- **`down` versus `restart` while busy.** `down` succeeds while a command is executing in the
  session, but `restart` refuses with `session_busy` (receipts 128-129).
- **Identity token in the environment.** `STACK_IDENTITY_<NAME>` is written into the generated
  `[env]`, so every `exec` and `run` command sees the token. Only the probe has it stripped.

## 3. Missing capabilities for agent workflows

1. **Dependency-aware restart and health.**
   - After Postgres was killed and `up` recovered it, the API kept its dead connection. Stack
     still reported `api ... verified instance`, and `up` returned all green (receipts 050-053).
   - The agent had to read `stack logs api` to find `psycopg.OperationalError: the connection is
     closed`, then `stack restart api` by hand.
   - There is no `depends_on` or "restart dependents", and the identity probe proves the instance,
     not that its dependencies work.
2. **A pre-start or setup step for services.** The app needs `uv sync` before the API starts.
   Today that has to be wedged into `run` (`uv sync -q --frozen && exec uv run ...`). Without a
   `uv.lock` the first `up` fails (receipt 010; the diagnostic is good).
3. **Scaffolding.** There is no `stack init`; a new user hand-writes `stack.toml` from the docs.
4. **MCP `outputSchema`.** Tools return `structuredContent`, but no tool advertises an
   `outputSchema` (mcp probe, 27/28).
5. **Reclaiming removed checkouts** (P2-4). Something like `gc --prune` for orphaned port
   reservations and data dirs.
6. **Handing state between worktrees.** Sharing code is handled by git. There is no supported way
   to seed one checkout's database from another's: no snapshot or clone of the data dir, and no
   `stack exec --from`. I did the handoff through code (merge, then migration on restart).
7. **Readiness diagnostics.** On timeout, show the `ready_cmd`'s last exit status and output.
8. **Version-skew warning.** On this machine a global `stack 0.1.6` is first on `PATH`
   (`/opt/homebrew/bin/stack`). Neither `doctor` nor the lock warns that `stack.lock` was written
   by a different Stack.
9. **Pinning environment-sensitive service settings** (P1-2): the Postgres encoding and locale.

## 4. What worked (verified)

- **Two concurrent worktrees with different feature work on separate identities.**
  - Worktree A added migration `002_priority.sql`, a `?priority=` filter endpoint, per-filter
    Redis cache keys with invalidation, and three test files: 8 passed (receipt 030).
  - Worktree B added `DELETE /tasks/<id>` and `/tasks/search`, and fixed the create/complete cache
    invalidation: 4 passed (receipt 061).
  - Main received A through `git merge`. Its existing row survived, and 002 was applied on
    `stack restart api` (receipts 042-045).
  - The probe showed distinct Postgres data dirs, Redis dirs, ports, migration sets
    (A: 001+002, B: 001), row counts and Redis keys (receipts 036-038).
  - A concurrent `up` from two fresh worktrees started at the same moment got disjoint ports, all
    verified as `instance` (receipt 126).
- **The `watch` signal.** It names the changed files in `status` (`changed_since_start`) and in
  `run` stderr ("run `stack restart api`"); `restart api` then loads the edit (receipts 026-030).
  `logs --since-start` showed only the new process (029).
- **Withholding unverified services.**
  - After a SIGKILL, `exec` poisons `DATABASE_URL` to `unverified.stack.invalid`, and the app
    fails loudly (047).
  - `--require` and `run` refuse with `service_unavailable` (048-049).
  - `up` recovers the service and its data is preserved (050-051).
  - A killed API wrapper left no orphan on the port (verified with lsof).
- **Exit and timeout semantics.**
  - A command that exits 7 gives `ok: true` and `exit_code: 7`, and the CLI exits 7 (062).
  - `exec --timeout 2s` gives `timed_out`, exit 124, partial stdout, and the background child is
    killed (063).
  - A failing pytest under `run` exits 1 (024).
  - An MCP timeout gives `timed_out`.
- **Fault diagnostics.**
  - `start_failed` quotes uv's own error (010).
  - `port_conflict` names the port, the PID and the program (`python3.12`), and
    `--reassign-ports` moved only the conflicting port while keeping the data (079-082).
  - `session_stale` after editing `stack.toml` withholds every endpoint until `up` (071-077).
  - `lock_outdated` appears after a path-bundle edit; `down` still works in that state (121-123).
- **Checksums.** Tampering with jq's macos-arm64 checksum gives `artifact_mismatch` with
  `expected`, `actual` and `url`. Nothing was installed, and `retry_safe: true` (114).
- **Secrets, with fake fnox values only.** Redaction worked in `--json` stdout and stderr, and an
  ungranted key was not injected.
  - The base64-transformed value was **not** redacted, which matches the documented boundary.
  - A short value gives `secret_unsupported`, an unknown key `secret_missing`, and a protected or
    lowercase name `invalid_secret`.
  - Terminal output is not redacted, as documented.
  - A scan of the whole isolated root found no fake value outside `fnox.toml` (103-109).
- **Leases and gc.**
  - A TTL of 1m was reclaimed by `gc` (111), and so was an `--owner-pid` lease once the owner
    exited (131-134).
  - A checkout deleted without `down` gave `gc_incomplete` with the exact `recovery.inspect` and
    `recovery.stop` commands. Following them and re-running `gc` cleaned up, and I verified the
    PID gone and the port closed (095-097, 111).
  - `gc --watch` printed one JSON object per pass (136).
- **MCP.**
  - It initializes, and `tools/list` matches the docs exactly (14 tools, each with `dir`).
  - Invalid arguments get precise `usage` hints: a string `command` ("argv list ... use
    `["sh","-c",...]`"), `timeout` ("did you mean `timeout_secs`?"), a wrong type, a missing
    `command`, and `require` with `require_all`.
  - Other errors are structured too: `unknown_task` and `unknown_service` list the valid names,
    and a missing dir gives `dir_not_found`.
  - `content[0].text` equals `structuredContent`, and the server exits on EOF.
  - Error recovery: a restart, then `status` is healthy.
  - A full `stack_up`, `stack_run`, `stack_down` lifecycle works over MCP alone.
- **Tools-only Node task.** `node = "22"` resolves to 22.23.3. `stack run test` auto-installed node
  and ran `node --test`: 1/3 failed (receipt 091). The `slugify` fix (NFKD, punctuation and
  separators) made it 3/3 (093). No `.stack/` was created.
- **Hosted noninteractive cold bootstrap**, without Docker and without mise or Homebrew on `PATH`
  (`hosted_bootstrap.sh`).
  - npm install of the package, then `setup` downloaded the tested mise.
  - Then `doctor`, `compile --locked` and `up` (13 s cold), `run test` (failed only because of
    P1-2), `down` (confirmed), and supervisor `down`.
  - `gc` came back clean, and `git status` showed only ignored files.

## 5. Workflow ledger

Effort is counted in receipts and my retries, not as productivity.

| # | Workflow | Result | Retries / notes |
|---|---|---|---|
| 1 | Install exact 0.1.22 from npm | **Blocked** (not published) | Fell back to a local tarball built from the pinned source |
| 2 | CLI help, `setup` and `doctor` as a new user | Completed | Mise from `PATH`, so nothing was installed |
| 3 | Write the tracker app, then compile and first `up` | Completed after 1 retry | First naive `up` failed `start_failed` (no `uv.lock`; my fixture). The hint led to `stack exec -- uv lock`, then `up` |
| 4 | Baseline tests via `stack run test` | **Failed: P1-1** | Diagnosed via an env dump and a source read. Worked around in `conftest.py` |
| 5 | A: migration, endpoint and cache fix, red to green | Completed | `watch` hint, then `restart` |
| 6 | B: delete and search endpoints, cache fix | Completed | |
| 7 | Separate DB and Redis identity and data across 3 checkouts | Completed | |
| 8 | Hand A to main (merge, migration on restart, data kept) | Completed | My zsh `$G` mistake made receipts 039-041 run in A (marked with `.note`); redone as 042-045 |
| 9 | Kill Postgres, recover; kill the API, recover | Completed | Exposed missing capability 1 |
| 10 | Nonzero exits, timeouts, signals | Completed | Found P3-5 |
| 11 | Config drift, `session_stale` | Completed | |
| 12 | Port conflict and `--reassign-ports` | Completed | |
| 13 | Never-ready service, `up --timeout` | Completed | Found P3-6 |
| 14 | TTL expiry and gc; deleted checkout and `gc_incomplete` recovery | Completed | One wait was blocked by the harness, so I interleaved other work |
| 15 | `--owner-pid` lease | Completed | |
| 16 | `session_busy` | Completed | Found P3-5 |
| 17 | Concurrent `up` in two new worktrees | Completed | |
| 18 | Worktree remove via the `down` hook, then gc | Completed | Found P2-4 |
| 19 | MCP discovery, schema, valid and invalid calls, recovery | Completed (27/28) | |
| 20 | MCP-only lifecycle | Completed, confirms P1-1 | |
| 21 | Tools-only Node task | Completed | |
| 22 | Secrets with fake keys | Completed | Found P2-3. Read `tests/e2e/10-secrets.sh` to learn the `fnox.toml` format (doc friction) |
| 23 | Path bundle, lock replay (`compile --locked`), drift | Completed | |
| 24 | Checksum tamper | Completed | |
| 25 | Hosted cold bootstrap | Completed. Tests failed (**P1-2**) | |
| 26 | 0.1.21 comparison for P1-1 | Completed | The first repro compared `run` to `exec` only, so it "passed" on 0.1.21. I tightened it to assert loopback |
| 27 | OCI `publish` and bundle pull | **Not run** | No registry; no Docker by design |
| 28 | Worktrunk hooks | **Not run** | `wt` is not installed. I simulated the hooks with `git worktree` |
| 29 | Linux and Intel macOS | **Not run** | Only macOS arm64 was available |
| 30 | Registering with a real agent (`claude mcp add`) | **Not run** | It would modify user config. Exercised the raw stdio protocol instead |
| 31 | Agent skills (`stack_skill` with a skill present) | **Not run meaningfully** | No pinned tool here ships a skill; only `skill_not_found` was exercised |
| 32 | Git and OCI bundle sources, `compile --update` moves | **Not run** | |

**My own tooling errors**, excluded from product findings:

- `rx.sh` mapped exit 0 under a timeout to 128 (receipt 012; fixed and self-tested in 013-015).
- Several shell quoting mistakes: receipts 033-035, and the jq task at 116.

**Manual documentation read:** the README, all of `docs/user/*.md`, and
`examples/isolated-stack.sh`.

**Source investigation:**

- `npm/bin/stack.cjs`, to build the local package.
- `src/provider/mise.rs` and git history, for P1-1.
- `tests/e2e/*.sh`, for coverage and the fnox format.

### Documentation friction (not bugs)

- `secrets.md` never shows a `fnox.toml`. A new user cannot get a fake or plain secret working
  without reading the e2e test.
- `--require` takes one service per flag, so `--require postgres,redis` is `unknown_service`. The
  docs only show `--require <service>`.
- Docs do not say that service data lives under `$MISE_STATE_DIR/daemons/<hash>/data`, which
  isolation and cleanup scripts need.
- worktrees.md's isolated-run recipe pins `0.1.21` in its example. That version still has the
  `exec` URL bug.

## 6. Coverage matrix

| Area | Covered | Not covered |
|---|---|---|
| Install | Local tarball from the pinned source; npm 0.1.21; cold `setup` mise download | Registry 0.1.22 (unpublished); Linux; Intel Mac; `setup --force` |
| Services | postgres 17, redis 7, custom HTTP service with identity probe, never-ready service | cockroachdb, nats, spicedb presets; fixed `port`; `ready_port` |
| Worktrees | 5 git worktrees, concurrent `up`, hook-style up and down, deleted checkout | Worktrunk itself; recreated checkout at the same path (`.replaced-<time>`) |
| Lifecycle | up, status, restart, run, exec, logs, down, renew (implicitly via exec), gc, `gc --watch`, TTL, owner-pid | `renew` command directly; `up` across a supervisor crash |
| Agent I/O | `--json` across commands; MCP stdio, all 14 tools listed, about 25 calls | Real MCP clients; concurrent MCP calls; output over 64 KiB |
| Supply chain | `compile --locked`, path-bundle drift, checksum tamper | `compile --update` diffs; `artifacts = "required"`; packslip signer mismatch; OCI and git bundles |
| Secrets | Plain fnox provider with fake values, redaction, failure codes, leak scan | Real providers, leases, file secrets, `FNOX_PROFILE` |
| Languages | Python (uv, psycopg, redis) and Node 22 | Rust/mbx, Go |
| Hosted | `env -i`, minimal `PATH`, CI=1, macOS | Linux containers; locale-normalized CI images |

## 7. Benchmark blindspots and proposed runnable checks

Blindspots in the repository's own tests and benches, from reading `tests/e2e/`:

1. **`<NAME>_URL` is asserted only through `exec`** (`tests/e2e/11-field-regressions.sh:30`), never
   through `run` or MCP `stack_run`. That is how P1-1 shipped in 0.1.22 despite a fix commit.
2. **The e2e suite runs in a locale-bearing shell.** Nothing runs `up` under `env -i` or `LC_ALL=C`
   and checks the server encoding (P1-2).
3. **Deterministic probes stop at "service verified".** None runs a real app across a dependency
   restart, so they cannot see that the API is "verified" while unusable.
4. **No test runs a task with secrets when fnox is not installed yet** (P2-3).
5. **No test counts port reservations or data dirs** after `down` plus checkout removal (P2-4).
6. **Error-envelope completeness** (`hint` and `details` present for every code, one exit code for
   `usage`) is not swept (P3-5).

Proposed checks. Each is runnable now with the scripts here, in an isolated root (`. env.sh`, with
`stack` on `PATH`):

| Check | Command | Passes when |
|---|---|---|
| run/exec URL parity | `bash fixtures/run-url-repro/repro.sh <copy>` | exit 0 (fails today) |
| MCP URL parity and lifecycle | `python3 -I mcp_lifecycle.py <copy> out.json` | exit 0 (fails today) |
| Locale-independent Postgres | `bash pg_encoding.sh <root> <copy-of-fixtures/pg-encoding>`, with and without `LC_CTYPE` | both print `UTF8` (one prints `SQL_ASCII` today) |
| Hosted cold bootstrap of a real app | `bash hosted_bootstrap.sh <pkg> <git repo> <outdir>` | every step exits 0, including `run-test` |
| MCP contract | `python3 -I mcp_probe.py <running checkout> out.json` | 28/28 (outputSchema missing today) |
| Port and data reclaim after removal | `down`, remove the worktree, `gc`, then count `ports.json` reservations whose `project` is missing | 0 (6 today) |
| Secrets before install | fresh root, then `stack run sign` in `fixtures/secrets` | exit 0 with no prior `stack install` (fails today) |

## 8. Repro steps (all scripts in this directory)

```sh
cd <this worktree>
ISO=$(mktemp -d /tmp/sao.XXXX); . eval/agentdx-20261020/opus/env.sh
npm install --prefix "$ISO/cli" <ushawarma-stack-0.1.22.tgz or @ushawarma/stack@VERSION>
stack setup
cp -R eval/agentdx-20261020/opus/fixtures/run-url-repro "$ISO/u" && (cd "$ISO/u" && git init -q && bash repro.sh .)
cp -R eval/agentdx-20261020/opus/fixtures/tracker "$ISO/t" && cd "$ISO/t" && git init -q && git add -A \
  && git -c user.name=e -c user.email=e@x commit -qm base && stack compile && stack exec -- uv lock \
  && stack up --ttl 30m && stack run test        # baseline; fails under `run` with P1-1
# cleanup: stack down (each checkout); stack exec -- pitchfork supervisor stop; delete "$ISO" by exact path
```

`pre.sh` is the session prelude I used. It reads `/tmp/agentdx-opus-results-path` and
`/tmp/agentdx-opus-iso`; set those or source `env.sh` directly. `rx.sh` is the receipt wrapper.

## 9. Cleanup record

- **Before teardown.** 19 processes referenced the main root (`teardown-pids-before.txt`).
- **Teardown.** `down` in every checkout (receipts 140-146), with `confirmed: true` and the PIDs
  listed. Then supervisor stop and supervisor status `down` (147-148), and `gc` returned `[]` (149).
- **Verified afterwards** (`teardown-verify.txt`): no process references any of my roots, none of
  the 19 PIDs is alive, and no reserved port is listening.
- **Global state.** Global receipts, global service processes and the listening-socket set are
  unchanged.
- **Roots.**
  - Deleted by exact path once the evidence was archived: `/tmp/sao.Pgqn` (912 MB) and
    `/tmp/sa1.anAU` (142 MB).
  - **Kept as failure evidence:** `/tmp/sah.v0Pz` (632 MB). This is the hosted root with the
    `SQL_ASCII` cluster, stopped and with its supervisor down. Delete it by exact path when no
    longer needed.
