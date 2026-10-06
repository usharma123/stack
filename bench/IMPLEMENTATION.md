# Real-world competitor benchmark: implementation plan and checkpoints

Owner: implementation agent (Opus 5.5). Research lives in `bench/research/` (Sol agents),
reviews live in `bench/reviews/`. This file is the review/commit map for the parent.

## Goal

Run the same realistic developer workload through Stack and each competitor with a
fair, idiomatic setup per tool. Report what each tool does natively, what needed
benchmark-owned scripting, and what it cannot do. Record raw evidence for every step.
The benchmark does not combine results into a single rank, and a missing Stack-specific
feature (for example wrong-instance detection) is recorded as `unsupported`, not `fail`.

Results must come from new runs. The harness never reads `eval/results/`.

## Layout

```
bench/
  IMPLEMENTATION.md          this file
  README.md                  how to run, what is measured, how to read outcomes
  run.py                     CLI entry point
  rwb/                       harness package (stdlib only, Python 3.9+)
    record.py                step recorder: argv, stdout/stderr files, exit, ns timings
    transport.py             DockerTransport (owned, uniquely named container) and
                             HostTransport (Compose). Ownership guard on every rm.
    scenario.py              the common workload, phased (setup / lifecycle / warm / failure)
    verify.py                parse and check the app's JSON evidence (identity, CRUD, cache)
    outcomes.py              outcome model: pass / fail / unsupported / not-applicable / error
    stats.py                 nearest-rank percentiles
    adapters/
      base.py                Adapter interface + feature-mode declarations
      registry.py            name -> adapter, so new competitors are one file + config dir
      stack.py mise.py flox.py devbox.py devenv.py nix.py pixi.py compose.py
  adapters/<tool>/           checked-in tool config (stack.toml, manifest.toml, devenv.nix,
                             flake.nix, pixi.toml, compose.yaml, ...). Hashed per run.
  fixtures/app/              pinned Python 3.13 app: migrations, CRUD, Redis cache, identity
  tests/                     offline validity tests (no network, no Docker)
  results/                   (git-ignored) new run output directories
```

## Workload (identical for every tool)

All app work goes through the tool's own environment entry (`stack exec`, `mise exec`,
`flox activate --`, `devbox run`, `devenv shell`, `nix develop --command`, `pixi run`,
`docker compose run`). The app reads only `DATABASE_URL` and `REDIS_URL`; it has no
fallback endpoints.

| # | Step | Phase | Verified by |
|---|------|-------|-------------|
| 1 | Tool + provider versions | meta | raw output |
| 2 | Prepare checkouts A and B (fixture + tool config) | meta | config SHA-256 |
| 3 | Cold setup A (resolve, lock, install) | setup | exit 0, lock files exist, hashes |
| 4 | Start services A; readiness (native wait or scripted poll) | lifecycle | app `identity` |
| 5 | Migrate, CRUD, cache read-through/invalidation in A | workload | app JSON checks |
| 6 | Warm setup B (caches populated), start B while A runs | setup/lifecycle | exit 0 |
| 7 | Migrate/CRUD in B with its own marker | workload | app JSON checks |
| 8 | Isolation: A sees only A's marker, B only B's; different PG data dirs / Redis instances | verify | identity diff |
| 9 | Repeat commands: N x `true` and N x app read through the entry | warm | timings + exit |
| 10 | Stop A; B still serves; A's endpoints refuse | lifecycle | app checks |
| 11 | Restart A; Postgres rows persist; cache rebuilds correctly | lifecycle | app checks |
| 12 | Locked/frozen re-setup in fresh checkout C from A's lock: lock unchanged | setup | hash compare |
| 13 | Bad config: unknown package/version in scratch copy -> clean nonzero, no services | failure | exit, ps |
| 14 | Bad startup: service port already taken (where the tool owns ports) | failure | exit/identity |
| 15 | Stop all; no leftover service processes; container removed | cleanup | ps, docker |

Feature modes per adapter (`native`, `scripted`, `unsupported`) are declared in code and
copied into results: lockfile, service supervision, readiness wait, per-checkout ports,
per-checkout data, stop confirmation, wrong-instance guard, structured status.
`scripted` steps run benchmark-owned glue that is checked in next to the tool config.

## Evidence

Per run directory: `meta.json` (host, harness git commit, docker image IDs, tool versions,
config hashes, fixture hash), `steps.jsonl` (append-only, one record per command with
argv, phase, exit, outer host ns, inner container ns, stdout/stderr paths), raw
`logs/<seq>-<label>.{stdout,stderr}`, `outcomes.json`, `summary.md`.

Timing boundaries: `outer_ns` is host `perf_counter_ns` around `docker exec`
(includes transport); `inner_ns` is measured inside the container by a bash wrapper
around the entry command only. Setup (cold/warm) and warm repeat phases are reported
separately and never mixed.

## Safety

- One container per tool per run, named `rwb-<tool>-<runid>`; Compose projects named
  `rwb-<runid>-<checkout>`. The transport refuses to remove anything it did not create.
- No `docker prune`, no global `pkill`, no stopping of any existing container (including
  long-running `ev-*` containers). Images are only read.
- Host state is limited to the new results directory and a `mktemp` work dir.
- Output directories must not exist beforehand.

## Checkpoints

- [x] C0 Survey of `eval/` harness, images and configs; this plan.
- [x] C1 Fixture app (`fixtures/app`, uv.lock pinned) smoke-tested against throwaway
      postgres:17.6-alpine/redis:8-alpine containers (all commands, pytest 4/4, error codes).
- [x] C2 Harness core (record/transport/outcomes/stats/verify/scenario) — **adapter contract
      v1 STABLE, documented in `ADAPTER-CONTRACT.md`**; extra adapter agents may build on it.
- [x] C3 Scenario + adapter base + Stack and mise adapters; functional smokes.
- [x] C4 Flox, Devbox, devenv, Nix, Pixi, Compose adapters; each functionally smoked to all-pass.
- [x] C5 README, parent findings P1-P4, Sol core-integration items, five Astra external fixes.
- [ ] C6 Parent-serialized full runs (not done by the implementation agent).

## Status log

- 2026-10-06: contract v1 stable (`ADAPTER-CONTRACT.md`). Research-driven decisions: Redis SAVE before
  stop + reported durability policy; declared isolation boundary (service-instance /
  container / database) with typed receipts; per-checkout SOURCE_TOKEN gate against
  wrong-code execution; A generates the lock, B/C/E receive it as committed; Stack uses the
  parent-built Linux binary (hash-checked at provision) and pinned mise 2026.10.3.
- 2026-10-06 (Astra round 1, fail-closed): app receipts now require exit 0, no timeout, `ok`,
  a result object and the exact expected shape (`verify.EXPECTED`); repeated reads validate
  the item; Redis persistence is read from the validated `persisted` booleans; missing A/C
  lock hashes make `lock.frozen_copy` blocked; a failed/timed-out leftover probe is `error`;
  bad-config and occupied-port treat timeouts and exit 126/127 as infrastructure faults
  (`blocked` / `fail`), never as a detected refusal; a timed-out post-stop probe blocks the
  stop/restart checks. Files: `rwb/verify.py`, `rwb/scenario.py`, `rwb/testing.py`,
  `tests/test_core.py` (38 tests). Adapter files (mise/flox/devbox) are separate work.
- 2026-10-06 (Astra round 1 complete, `reviews/round-1-core.md`): all nine findings fixed in
  core and covered by named injected-failure regressions in `tests/test_core.py`
  (`test_r1_*`, plus earlier fail-closed tests; 47 tests pass). Mapping:
  R1-1 `verify.app_result` requires exit 0/no timeout; repeated start gated on its exit.
  R1-2 command schemas (`verify.EXPECTED`, `identity_complete`: source/URLs/ports/markers).
  R1-3 `verify.refusal`: timeouts/126/127 are infrastructure faults (bad_config blocked,
  occupied_port fail). R1-4 occupied-port identity problems (wrong source) reject relocation;
  failed deps block. R1-5 complete declared lock sets + successful hash commands required.
  R1-6 failed/timed-out leftover probe is `error` and invalidates cleanup. R1-7 listener
  started only after successful setup, registered, and released in `cleanup()` always.
  R1-8 `Scenario.prepare` records `prepare.<co>` failures and blocks dependents.
  R1-9 container isolation requires valid receipts for both checkouts.
  Reproducer `/tmp/stack-astra-core.2Q4stu/probe.py` (paths redirected) now fails closed
  in every case. Also: `run.py --dry-run` binds the fake world to real checkout tokens.
  Core files: rwb/verify.py, rwb/scenario.py, rwb/testing.py, run.py, tests/test_core.py.

## Open questions for parent (core owner, 2026-10-06)

Defaults below are already implemented; answer only to change them.

1. Stack `occupied_port`: with a foreign listener on its assigned port, `stack up` fails at
   `stop_previous` (`stop_failed`/`stop_unconfirmed`, "mise daemons stop failed"/"services did
   not stop"), never naming a conflict. Reproduced in an owned container (seq evidence in
   results/smoke-stack-6; without the squatter E's `up` succeeds). Default: `fail`
   ("start-failed-without-conflict-diagnostic"). The smoke-4/5 "refused-at-start" passes were false.
   Product unchanged. Report as a Stack finding?
2. bad_config failure without the intended diagnostic: default `blocked` (inconclusive), not `fail`.
3. devenv: nixpkgs switched from the research pin addf7cf5 (Python 3.13.9/PG 17.7/Redis 8.2.2)
   to the shared Nix-lane revision 151fa4e8 (3.13.15/17.11/8.10.2) for cohort parity, with modules
   still pinned to the 2.4.0 source. Recorded as `deviation` in pins. OK?
4. Compose lane: pinned standalone docker-compose v5.6.0 (darwin-aarch64, release sha256) in a
   run-owned dir against the host daemon, not the host plugin v2.40.3-desktop.1. OK?
5. Flox/Devbox images were built from "latest" installers. The actual CLI version is recorded from
   the binary. If it differs from the research pins (Flox 1.17.0, Devbox 0.18.4), default is to label
   the actual version and not upgrade in place. OK?
6. Cohort: Stack/mise/Compose Python 3.13.16; Nix/devenv/Pixi Python 3.13.15 (nixpkgs/conda-forge
   have no 3.13.16); container/worktree families PG 17.6 images vs 17.11 elsewhere. Report as
   declared deviations (no realignment of other families' images without handoff)?
7. Pixi installs the PyPI set via its own resolver (same top-level pins as uv.lock; transitive set
   recorded per run), per research. OK to keep?

## Checkpoint 2026-10-06 (Opus, sole implementer): for parent review/commit

### Core changes (commit separately from adapters)
Files: `rwb/verify.py`, `rwb/scenario.py`, `rwb/adapters/base.py`, `rwb/adapters/common.py`,
`rwb/transport.py`, `rwb/testing.py`, `run.py`, `tests/test_core.py`, `ADAPTER-CONTRACT.md`,
`README.md`.

Parent findings:
- P1 NAT. `port_map` receipt validated (evidence, int published/target) and required when URL
  port != server port. A declared map that prints no JSON fails closed (this was a fail-open
  path that let workz/Worktrunk pass in the shared fake). The fake supplies real receipts.
- P2 Listener. Run-unique `.rwb-owned/rwb-squat-<run>-e.pid` under the run workdir, argv tag
  via `exec -a`, and the listener must win the bind (a port held by someone else is refused).
  Release signals only a pid still running the tag; legacy overrides are checked by port.
  Live-tested on the macOS host.
- P3 Hashing. `SHA256_FN` / `sha256_check` fall back to `shasum` (macOS host transport), used by
  lock hashing, mise/Stack provisioning and versions. Tested with a PATH that has no `sha256sum`.
- P4 Intended diagnostics. bad_config needs `bad_config_pattern` in the failure output,
  otherwise `blocked`. occupied_port needs a conflict diagnostic, and the bare port number is
  not enough. A failed scripted readiness needs `conflict_logs` or wait-output evidence.

Sol core handoff:
- Fake NAT receipts.
- Artifacts before teardown: `run.teardown`, with diagnostics then artifact copy then
  checkout, host and container teardown, each guarded.
- DDEV `entry_auto_resumes`: no after-stop call. isola: observed `stop.a.data_endpoints`.
- C cleanup tracking, explicit or inferred.
- `prepare_scope`/`start_scope` disclosure and `prepare.<co>` timing.
- All-26 roster test.

Also:
- Exit 77 blocked provisioning, with `measurement`/`reportable` in meta.
- `setup.first_checkout`/`second_checkout` naming, `first_task.<co>` end-to-end time.
- Version comparison ignores tool chatter and checkout or store paths.
- nix-daemon wait step for the ev-nix-based images.

### Adapter changes (original eight)
- New adapters: `nix.py`, `pixi.py`, `devenv.py` and `compose.py`, with configs under
  `adapters/{nix,pixi,devenv,compose}`.
- Fixed adapters:
  - mise: one `mise trust` per file.
  - Flox: the documented held-activation lifecycle; stop = `services stop`, restart =
    `activate -- services start`, cleanup releases the activation; `conflict_logs`, artifacts.
  - Devbox: per-checkout ports via an init hook (the Redis plugin overrode `env_from`);
    `devbox.d` copied as committed config; confirmed stop; `conflict_logs`.
  - All: setup scope and cache notes.

### Astra external fixes (`reviews/sol-adapter-fix-handoff.md`), each with stub-Docker tests
1. Lando `owner_token` (normalized) used by the receipt callers (`devcontainers.py`, `lando.py`).
2. `project_stopped` returns control (`agent_env_common.py`). Berth/BranchBox checks now run.
3. Tilt stopped probe guards the query status.
4. Tilt and Vagrant identities: separately guarded queries, empty results rejected.
5. worktree cleanup discovery and `verify_project_gone`: each query guarded.

Plus:
- isola `bad_config_pattern` = the injected endpoint `127.0.0.1:1`, with a regression that
  rejects unrelated errors.
- `prepare_scope` labels on workz/Worktrunk/GitGrove/BranchBox/isola/Berth, and `start_scope`
  on dnvr.

### Test results (offline)
- `python3 -m unittest discover -s bench/tests`: 190 tests OK.
- `bash -n vagrant-resources.sh`: OK. `git diff --check`: clean.
- `run.py --dry-run` exits 0 for all 26 adapters.
- Astra's reproduction now stops at its first (bug-describing) assertion, as expected.

### Functional smokes (diagnostic, 2 samples + 1 warmup, not reportable)

| Lane | Run | Outcome |
|---|---|---|
| Stack (Linux ARM64 `1d338d2c...`) | smoke-stack-6 | all pass **except `occupied_port: fail`** (finding below) |
| mise 2026.10.3 | smoke-mise-2 | all pass (port conflict refused with "port ... already in use") |
| Flox 1.17.0 | smoke-flox-3 | all pass; ran **before** the stricter scripted-readiness gate, so occupied_port needs a rerun |
| Devbox 0.18.4 | smoke-devbox-3 | all pass (occupied port detected through conflict_logs) |
| devenv 2.4.0 | smoke-devenv-3 | all pass (relocated around the squatter) |
| Nix (Determinate 3.23.0 / 2.35.2) + scripts | smoke-nix-1 | all pass |
| Pixi 0.81.0 + scripts | smoke-pixi-2 | all pass |
| Compose 5.6.0 (host) | smoke-compose-2 | all pass; occupied_port n/a; zero leftover resources |

Earlier failed smokes are kept as evidence of recipe bugs fixed since. Every container was
removed (`docker ps -a --filter label=rwb.owner=stack-realworld-bench` is empty).

### Findings and unresolved items (route runtime diagnosis to Sol via parent)
1. **Stack occupied port.** With a foreign listener on its assigned port, `stack up` fails at
   `stop_previous` (`stop_failed`/`stop_unconfirmed`) and never reports the conflict.
   It does refuse safely: no squatter use, no kill. Reproduced in an owned container
   (smoke-stack-6 plus a manual repro). Without the listener, the same checkout's `up`
   succeeds. The earlier "refused-at-start" passes in smoke-4 and smoke-5 were false.
   Product unchanged.
2. Rerun Flox (and any scripted-readiness lane) under the new `conflict_logs` gate.
3. The smokes for Stack-5, Nix-1 and Pixi-2 predate some core edits (the version filter and
   NAT fail-closed). Their outcomes are unaffected by design but should be re-run at
   measurement time.
4. Devbox `start.b` once reached A's Redis directory without failing the identity, because
   the plugin exports no REDIS_DATA to compare against. Only isolation caught it. This is
   correct overall, but noted.
5. The open questions above (1-7) still stand. Guix: verified digest/mirror in
   `evidence/guix-release`; canary pending (parent).
6. Not executed by me: any external-family lifecycle (containers, worktree, agent-env,
   native-extra, additional). Those remain parent-serialized.
