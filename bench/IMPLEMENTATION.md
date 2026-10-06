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
- [ ] C3 Scenario + adapter base + Stack and mise adapters; smoke of one adapter.
- [ ] C4 Flox, Devbox, devenv, Nix, Pixi, Compose adapters aligned with research docs.
- [ ] C5 README, review fixes, list of extra competitors from the research roster.
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
