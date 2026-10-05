# Evaluation follow-up: implementation record

Implements [eval-followup-plan.md](eval-followup-plan.md) on branch `eval-followup` (from
`a135833`, release 0.1.3). The implementation-time checks below ran before commit preparation and used a dirty
working tree. They are not measurements of the published 0.1.3 binary. Release measurements
remain separate; the PR publishes concise summaries and the HTML report, while raw logs stay
local and ignored. Independent review requested changes; see [review round 1](eval-followup-review-r1.md).
Those findings and remote CI remain open before merging. The pre-existing correction in `tests/e2e/4-mcp.sh` (exact nine tool names)
is kept verbatim; that file only gained portable paths.

## Provider facts verified before building on them

All with mise 2026.9.18 (macOS arm64 binary from the 0.1.3 native benchmark, isolated HOME in
`/tmp`) and Pitchfork 2.29.0 source (`git clone --branch v2.29.0 github.com/jdx/pitchfork`).

| Fact | How verified |
|---|---|
| `mise latest python@3.13` → `3.13.16`, `uv` → `0.12.23`, `jq` → `1.8.2`, `postgres@17` → `17.11`, `redis@8` → `8.10.2` | ran it |
| `mise latest postgres@17.99` prints **nothing and exits 0**; unknown tool exits 1 | ran it; stack treats empty output as `resolve_failed` |
| Preset `version = "17"` installs mise tool `postgres` (`installs/postgres/17.11`); Redis likewise | installs dir of the 0.1.3 native run and a fresh start |
| Data created under `version = "17"` / `"8"` starts under `"17.11"` / `"8.10.2"` | stopped, changed version, restarted with existing data |
| Pitchfork state dir: `PITCHFORK_STATE_DIR` (tilde-expanded, Unicode only) → root/SUDO_USER rules → `dirs::state_dir()` (absolute `XDG_STATE_HOME` on Linux; **None on macOS**) → `$HOME/.local/state`; socket `<dir>/sock/main.sock`; limit `sizeof(sun_path)`, `len <= limit` | `src/env.rs`, `src/ipc/mod.rs`, `dirs-7.0.0/src/{lin,mac}.rs`; on macOS a set `XDG_STATE_HOME` was ignored in practice. Pitchfork's own error hint ("or XDG_STATE_HOME") is wrong on macOS |
| mise passes config `[env]` (e.g. `PITCHFORK_STATE_DIR`) to Pitchfork and to daemon processes | socket moved to the configured dir; a daemon printed its `[env]` value |
| `mise env --json` works before tools are installed | ran it with a missing tool |
| `mise daemons --json` reports a qualified `id` (`<dir>-<hash>/<name>`), `data_dir`, `state_dir` | ran it |
| After the project directory is moved away, `pitchfork status --json <id>` still reports pid/port; it reads state without starting a supervisor; `stop` without a supervisor fails instead of starting one | ran it |
| Pitchfork's port check binds 0.0.0.0/127.0.0.1/::1 (`supervisor/lifecycle.rs`) | source; relevant to the finding below |

## What changed, by workstream

### 1. Exact versions (`src/lock.rs`, `src/project.rs`, `src/provider/mise.rs`)

- `stack.lock` version 2: `[[tool]]` and `[[service]]` entries with `name`, `requested`,
  `resolved`, `resolved_on`, and for services the preset's `tool`. Pitchfork (added by stack)
  is locked like any tool. The provider config renders the resolved versions.
- Ordinary `compile` reuses a pin while the request is unchanged and resolves only new or
  changed requests. `--update` re-resolves all and reports `moved_from`. Locked mode
  (`--locked`, `inspect` with a lock, `up`, `exec`, `status`) never resolves and returns
  `lock_outdated` for missing, stale or extra pins. `inspect`/`doctor` without a lock report
  `resolved: null` instead of resolving.
- Legacy v1 locks: read for migration; `compile` rewrites them keeping bundle pins (tested with
  a tag moved upstream before migration); every locked operation refuses them. v2 locks cannot
  be read by 0.1.3 (it rejects unknown versions).
- Resolution: `mise latest <tool>@<request>` in `<cache>/resolve` (outside any project),
  bounded by a 120 s timeout. One line, one version, or `resolve_failed`; nothing is written on failure.
- Only Postgres and Redis presets are locked (the verified mappings). Other presets, presets
  without `version`, and requests naming no release (`system`, `path:`, `ref:`, ...) are not
  locked and compile warns. The old "`latest` is not pinned" warning is gone because the lock
  now pins it.
- Examples: `uv = "latest"` → `"0.12.23"`, `jq = "latest"` → `"1.8.2"` (verified above);
  `python = "3.13"`, `postgres 17`, `redis 8` stay as requests that the lock pins.
  `examples/app/stack.lock` was regenerated with real mise.
- **Deliberate deviation:** an unknown tool or impossible version now fails at `compile`
  (`resolve_failed`) rather than at `up` (`install_failed`). `install_failed` remains for a
  locked release that cannot be installed (e.g. absent on another platform). Tested and
  documented. The 0.1.3 harness `eval/harness/stack.sh` expects `install_failed` and was not
  edited (historical).
- **Platform rule (explicit):** one resolved version for all platforms; `resolved_on` records
  where it was resolved; no per-platform re-resolution.
- **Not done:** artifact checksums. An exact version is not a checksum; mise's own lockfile is
  not used.

### 2. Native lifecycle coverage and socket diagnostics

- `SocketPath`/`socket_path()` reimplements Pitchfork 2.29.0's rule (above), including
  the composed/provider `[env]` value. `doctor` reports `pitchfork_socket`. `up` checks it after
  trusting the config and **before install** (using `mise env --json`, which includes config
  `[env]`), failing with `socket_path_too_long` (step `preflight`, `changed: false`). As root
  the path depends on SUDO_USER, so it is reported as not checked.
- E2E scripts are portable (no GNU `sed -i`, no fixed `/tmp`, `/srv`, `~`, `/opt/stack`, no
  machine-wide `ps` counts; PID-based leftovers instead; own `GIT_CONFIG_GLOBAL`).
  `tests/e2e/native.sh` runs them on the host with an isolated short `/tmp` HOME, skips OCI
  explicitly unless `STACK_E2E_REGISTRY` is set, writes one JSON line per scenario, and cleans up.
- New scenarios: `7-deleted-project` and `8-identity-probe`. Scenario 1 also asserts locked
  exact versions are rendered and that the running Postgres is the locked release. Scenario 3
  adds `gc --watch`. Scenario 6 now asserts that bundle pins, not the whole lock, are unchanged
  by a project override, and that the changed Redis request is re-resolved.
- CI (`.github/workflows/validate.yml`): new `services` matrix job (ubuntu-24.04 linux-x64 with a
  local `registry:2.8.3` for OCI; macos-15 darwin-arm64 with OCI reported skipped). mise
  v2026.9.18 is pinned by SHA-256, matching mise's published `SHASUMS256.txt`. `package` now
  needs it. **This job has not run remotely; no CI pass is claimed.** x64 macOS is not in it.

### 3. Custom identity (`src/manifest.rs`, `src/identity.rs`, `src/session.rs`)

- Opt-in `[services.<name>.identity] command`, `timeout` (default 5 s, 1-30 s). Not allowed on
  Postgres/Redis presets. `{{bundle_dir}}` is expanded.
- A random per-checkout, per-service token (`state/identities.json`, pruned like ports) is
  rendered as `STACK_IDENTITY_<NAME>` in the provider `[env]`, which the daemon receives. It is
  part of the configuration fingerprint only when probes exist, so older session records stay
  valid. Variable collisions and `STACK_IDENTITY_*` in `[env]` are rejected.
- The probe runs `sh -c` in the project with the app env minus every `STACK_IDENTITY_*`
  (provider and inherited). It is `instance` only if stdout trimmed equals the token exactly.
  Timeout kills the process group. More than 4 KiB of output, empty or other output, and non-zero
  exit all fail and withhold endpoints. Services without probes stay `liveness`.
- Documented as trusted bundle code, bounded but not sandboxed. Tokens tell instances apart; they
  are not credentials (they are in the generated config and the exec environment).
- Example bundle: `examples/bundles/webid`.

### 4. Cleanup after deletion and unattended expiry (`src/session.rs`, `src/main.rs`)

- Session records now also hold `provider` (Pitchfork binary from `mise which pitchfork`, its
  effective state dir), each service's `provider_id`, and `project_dir_id` (dev, inode). These
  are recorded in the launch record before start. A failed verification also saves the
  observed pids/ids (no dedicated test covers this path).
- GC treats a project as gone when its directory is deleted, or replaced (same path, different
  inode). For each recorded service it queries `pitchfork status --json <id>` with the recorded
  `PITCHFORK_STATE_DIR`. It asks Pitchfork to stop the daemon only if it is running with the
  recorded pid and port, then waits for that pid to exit. Stack never signals a PID itself, and
  nothing is recreated in the project. All of these keep the record and fail with
  `gc_incomplete`: no recorded id while something is alive; a pid not tracked or tracked
  differently; a different pid or port; query or stop failure. A foreign process on a port whose
  recorded process is dead is left alone, and the record is released.
- Lifecycle commands in a replaced directory fail with `session_conflict` until GC resolves the
  old session, so a new checkout at a reused path cannot adopt or stop the old services.
- Active executions with a live coordinator still protect a gone project's session. Owner-death
  policy, lifecycle locking, and the index-is-authoritative crash rule are unchanged.
- `stack gc --watch [--interval 60s] [--max-passes N]`: an opt-in foreground loop, one result per
  pass (one JSON object per line with `--json`), retrying after failed passes. Nothing is
  installed as an OS service.
- `gc_checked` now counts every unconfirmed reclaim, including deleted projects, which 0.1.3
  only listed.

### 5. Measured pilot

- `eval/harness/pilot.py` is a bounded runner (1-8 projects, 1-10 rounds) with `fresh` (empty
  isolated HOME) and `cached` phases. It runs concurrent projects, the bundled task (uv sync,
  pytest, `mise run seed`, bundled `acme`), wrong-instance checks via the app's own URLs,
  verified-exec latency samples, and deterministic `service_kill` and `runner_death` failures.
  It records metadata (binary hash, source commit and dirtiness, mise version, platform),
  `events.jsonl`, full per-command logs, and `summary.json`, all into a new directory. Its
  results are labelled `scripted_concurrency`, with `agent_productivity: not measured`.
- `docs/pilot-protocol.md` documents the real-agent protocol. **It has not been executed; no
  productivity finding exists.**

## Commands run and outcomes

Machine: macOS 15.7.2 arm64 (14 CPUs), Docker 29.1.3, Rust 1.93.1, mise 2026.9.18.

| Command | Outcome |
|---|---|
| `cargo clippy --locked --all-targets -- -D warnings` | clean |
| `cargo test --locked --all-targets` | 96 passed: lib 33, compile 24, oci_auth 6, oci_pull 2, runtime 31 |
| `node --test tests/*.test.mjs` | 13 passed |
| `shellcheck -x tests/e2e/*.sh` | no warnings or errors |
| `tests/e2e/native.sh target/release/stack` (macOS, real mise/Pitchfork) | native1: 7 passed, OCI skipped; native2 (registry): 8 passed; **native-final: 8 passed** |
| `tests/e2e/run.sh` (Docker Ubuntu 24.04 arm64) | docker1: 8 passed; **docker-final: failed** (see finding); **docker-final2: 8 passed**, same code |
| `python3 eval/harness/pilot.py --stack target/release/stack --projects 3 --rounds 2 --phases fresh,cached` | run 1 `pilot-20261005T131929Z-c79938`: harness bug (below); run 2 `pilot-20261005T132024Z-fe04db`: exit 0 |

E2E result summary: [summary.json](../eval/results/followup-e2e-20261005/summary.json).
Raw E2E logs remain local in that directory and are ignored by Git. The final native and
Docker runs used the final source. native1, native2 and docker1 predate only the
`session_conflict` guard.

Regression tests added (all in the counts above):
- `tests/compile.rs`: upstream movement plus a fresh cache plus locked mode (identical lock and
  config, zero resolver calls); changed/removed requests (stale in locked mode, only the
  changed one resolves); failed resolution writes nothing; legacy migration keeps bundle pins
  across a moved tag; unsupported/inconsistent lock versions; probe validation and per-checkout
  tokens; service versions rendered.
- `src/provider/mise.rs`: resolver output parsing; unversioned requests; socket precedence;
  non-Unicode `PITCHFORK_STATE_DIR` ignored; exact 104/108 boundary with two-byte characters;
  `sun_path` capacity.
- `tests/runtime.rs` (fake mise/pitchfork): empty `mise latest` output → `resolve_failed`, and
  exec never resolves; long socket path from config `[env]` or process env fails before
  install, and doctor reports it, with the at-limit path accepted. Probes: correct instance,
  foreign token, empty, extra line, prefix, 200 KB flood ending in the token, hang killed with
  its descendants, recovery, new token = stale generation; no-probe = liveness. GC:
  deleted-project stop via supervisor; five stale/reused-PID cases with nothing signalled and
  the record kept; foreign port occupant left alone; replaced directory (`session_conflict` for
  up/down/status/exec, then reclaim once confirmed); failed stop kept for retry; active exec
  protection after deletion; `--watch` respecting concurrent renewals, then reclaiming idle
  expiry with no `up`.

### Pilot results (scripted concurrency, one machine, one run each; not a performance study)

Run 2 (`eval/results/pilot-20261005T132024Z-fe04db`, 3 concurrent projects × 2 rounds per phase):

| Phase | Tasks ok | Wrong-instance | Orphans | Failures handled | `up` p50 / max ms | verified `exec` p50 ms (n=30) |
|---|---|---|---|---|---|---|
| fresh | 6/6 | 0 | 0 | 2/2 | 978 / 14260 | 167 |
| cached | 6/6 | 0 | 0 | 2/2 | 938 / 1517 | 175 |

- `service_kill`: after SIGKILL of Postgres, `exec --require-all` refused with
  `service_unavailable` ("service process changed since launch": a different pid was serving
  within 0.5 s). `up` recovered.
- `runner_death`: `stack gc` alone reclaimed exactly that project; no service pid survived.
- Run 1 (`pilot-20261005T131929Z-c79938`) is kept as recorded. It reports `service_kill` "NOT
  HANDLED" because the harness put `--json` after `--` (so `true` received it). Its logs show
  stack did refuse with `service_unavailable`. The harness was fixed (global flag first) and
  run 2 made. Run 1 is not a product failure, and its summary should not be quoted as one.

## New finding (not fixed): assigned ports overlap ephemeral ports

docker-final failed because Pitchfork refused Redis: "port 42037 is already in use by process
'unknown' (PID: 0)", although stack had confirmed nothing listened there. Pitchfork checks a port
by binding it. A client socket with that ephemeral local port can make the bind fail. The review
reproduced this mechanism separately, but the historical log has no contemporaneous socket
table proving what held port 42037; it is a supported hypothesis for this run. The image's `ip_local_port_range` is 32768-60999, which contains stack's whole
40000-49999 range; macOS uses 49152-65535 (overlaps 49152-49999). Scenario 2 makes many client
connections just before `up`. This allocation behaviour predates this change and is
intermittent: the same code passed in docker-final2. A follow-up can choose new reservations outside the effective ephemeral range while
retaining existing assignments. The initial assessment that every reservation would need
migration was incorrect; `ports::assign` already reuses existing reservations. Explicit
reassignment must not silently move an active service.

## Remaining validation gates and limits

- Review round 1 requests changes on R1-R8: conditional GC ownership, effective provider
  configuration, native-runner isolation, pin validation, partial-start ownership, effective
  socket paths, nonterminal supervisor states, and preset-lock coverage. Passing tests below
  do not resolve those findings. The PR is a draft pending correction and another review.

- Remote CI: the `services` job and its macOS/Linux runs have never executed on GitHub.
- macOS x64 and Linux x64 service scenarios were not run locally; Linux ran arm64 in Docker.
- Real-agent productivity: protocol only.
- Version locking covers Postgres/Redis presets only; no artifact checksums.
- Deleted-project cleanup relies on what `up` recorded. Sessions from 0.1.3 (no ids) are
  released only when nothing of theirs runs. A daemon restarted by the supervisor after deletion
  (new pid) is refused, needing manual `pitchfork stop`/`mise daemons prune`. Data directories
  are never removed.
- `gc --watch` needs a user-chosen supervisor for unattended operation.
- The macOS identity-probe scenario spends ~90 s in an intentional `not_ready` wait (the
  readiness timeout is not configurable).
- The intermittent port-overlap failure above.
- During PR preparation, the 0.1.3 benchmark runners were changed to use preserved release
  examples and six E2E scripts in `eval/fixtures/stack-0.1.3`. A rerun of the Linux benchmark
  and all six scenarios passed. Original measurements remain unchanged; newer implementation
  scenarios no longer leak into release reruns.

## Changed files

Modified: `.github/workflows/validate.yml`, `README.md`, `docs/DESIGN.md`,
`examples/app/stack.lock`, `examples/bundles/{obs,pybase}/bundle.toml`, `src/{compose,doctor,lib,
lock,main,manifest,mcp,process,project,session}.rs`, `src/provider/mise.rs`, `tests/compile.rs`,
`tests/runtime.rs`, `tests/e2e/{1..6}-*.sh`, `tests/e2e/assert.sh`, `tests/e2e/run.sh`.
New: `src/identity.rs`, `tests/e2e/{7-deleted-project,8-identity-probe,native}.sh`,
`examples/bundles/webid/{bundle.toml,server.py,probe.py}`, `eval/harness/pilot.py`,
`docs/pilot-protocol.md`, this file, `eval/results/followup-e2e-20261005/`,
`eval/results/pilot-20261005T131929Z-c79938/`, `eval/results/pilot-20261005T132024Z-fe04db/`.
