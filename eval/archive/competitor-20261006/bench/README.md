# Real-world competitor benchmark

Stack and the 25 tools frozen in `SCOPE.md` run the same developer workload: a pinned Python
3.13 app with migrations, CRUD, a Redis read-through cache, PostgreSQL 17 and Redis 8, in two
simultaneous checkouts. Each tool uses its documented workflow. Glue that the tool does not
provide is checked in under `adapters/<tool>/`, and the feature is declared `scripted`.
There is no combined score or rank.

The [reviewed 2026-10-06 session](measurements/final-20261006-reviewed-1/README.md)
contains all 26 coverage rows, 96 eligible metrics, known failures and restorable raw evidence.
Use its review for the final per-lane transports and realized versions.

Plan, checks and the status log: `IMPLEMENTATION.md`. Adapter API: `ADAPTER-CONTRACT.md`.
Per-tool research: `research/`. Reviews: `reviews/`.

## Requirements

- macOS or Linux host with Python 3.9+ (the harness itself is stdlib only) and bash.
- Docker with the local images `ev-base`, `ev-nix`, `ev-flox`, `ev-devbox` and `ev-devenv`
  (`eval/images/Dockerfile.*`). Those images were built from "latest" installers. Each run
  records the actual CLI version from the binary, not from the image tag.
- Outbound HTTPS from the containers (port 443 only): GitHub releases, nixpkgs/cachix
  substitutes, conda-forge, PyPI and Docker Hub.
- Stack: a Linux ARM64 `stack` binary built from the checkout under test, passed with
  `--option stack_binary=... --option stack_sha256=...`. See `provenance/stack-20261006/` for the
  build receipts and verified binary hash.
- Tools that are not in an image are provisioned per run into run-owned locations and checked
  against pinned hashes. Provisioning is recorded but never timed:
  - mise 2026.10.3 (Stack and mise lanes)
  - Pixi 0.81.0
  - devenv 2.4.0, installed into a private Nix profile
  - standalone docker-compose 5.6.0 (macOS ARM64)
- Compose lane: Docker Desktop's credential helper fails when run non-interactively here
  (exit 1, no output). The lane therefore uses a run-owned `DOCKER_CONFIG` with the same
  current context and no `credsStore`, so public images are pulled anonymously.
  `~/.docker` is only read.

## Running

```sh
python3 -m unittest discover -s bench/tests          # offline: no network, Docker or services
python3 bench/run.py --tool nix --dry-run            # print every planned body, run nothing
python3 bench/run.py --tool stack --out bench/results/<new-dir> \
  --option stack_binary=/tmp/stack-bench-build/target/release/stack \
  --option stack_sha256=<sha256> --repeats 20 --warmups 3
```

Run one tool per invocation, and never run two at once. The output directory must not exist
yet. `--keep` leaves the container for diagnosis; remove it afterwards only after checking its
`rwb.run` label. A kept run can never carry final timings.

## Final measurement report

Every `run.py` output is diagnostic, whatever its repeat count or exit code: `meta.reportable`
stays `false` and is never edited. Final numbers come only from `report.py`, which checks a
session manifest the parent writes and emits a separate report directory.

```sh
python3 bench/report.py template --session-id <id> --out <manifest.json>   # current hashes, reviews null
python3 bench/report.py attempt --run bench/results/<session>/<tool>         # hashes of one run, to paste
python3 bench/report.py --manifest <manifest.json> --out <new-report-dir> [--require-complete]
```

Manifest (`rwb-measurement-manifest/1`; relative paths resolve against the repository root):

- `session`: `id`, `purpose: final-measurement`, `plan_created_utc`, the ordered 26-entry
  `roster` (`entry`, `tool`, `variant`), `sample_policy` (`repeats`, `warmups`),
  `timing_policy`, `serialization_declaration`, and `review` (null until a real review exists).
- `protocol`: reviewed `harness` commit and file hashes, `fixture`, `shared_glue`
  (`adapters/_shared`), per-tool `lanes` (`config` hashes, `version_contains` strings expected
  in the `tool-version` receipt, `deviations`, optional `transport`/`image_id`), `platform`
  (optional exact `host`), `resources` (`cpus`, `memory`) and `cache_policy`.
- `attempts`, in execution order: `tool`, `variant`, `state` (`selected`, `excluded`, `blocked`
  or `untested`), `path`, `run_id`, `reason`, `result_review`, and `hashes` (meta, outcomes,
  steps and a digest of the whole run directory). Stack attempts also need `stack`:
  `source_revision` or `source_fingerprint`, `binary_sha256`, and `build_receipt`
  (`path`, `sha256`). Each tool has at most one non-excluded attempt; reruns stay as
  `excluded` with their reason.

The report keeps a row for each of the 26 entries: a missing lane is `missing`, and a blocked
or invalid run shows its evidence without a number. Only `first_task.a`, `first_task.b`,
`repeat.entry` and `repeat.app_read` can be reported. A metric is reported only when:

- the session and the attempt name a review;
- every hash matches, every receipt parses, and every cited step and raw log exists;
- tool, variant, run id, harness bytes, fixture, glue, config, sample policy and resources
  match the declaration, the tool-version output contains the declared strings, and `keep` is false;
- the run is complete, valid and not blocked, with every main outcome present;
- the run started after the plan was created and does not overlap any other declared attempt;
- for `first_task.<co>`, the six gates of that checkout pass, and the step receipts succeed,
  match the declared task and sum to the recorded time;
- for `repeat.*`, the check passes, isolation and both checkouts' gates pass, and every warmup
  and sample receipt exists and succeeds. App reads are reparsed and must return `keeper-a`.
  The p50/p95 values are recomputed from the receipts, and warmups never count.

Everything else is listed under `omitted` with its reason. A failure in one check removes only
the metrics that depend on it. The report describes one host, image/cache state, transport and
recipe. It has no rank, score or confidence claim.

## Reading a run

- `summary.md`: outcomes, then the end-to-end task times, then the phase timings.
- `outcomes.json`: one record per check, with `status`, `mode` and the evidence step numbers.
- `steps.jsonl` and `logs/`: every command's argv, exit code, timings and raw stdout/stderr.
- `artifacts/`: service and tool logs copied before teardown.
- `meta.json`: versions, pins, config, fixture and shared-glue hashes, image ID, feature modes,
  setup/prepare/start scopes, cache state, `keep`, requested `resources`, the run interval
  (`started_unix_ns`/`finished_unix_ns`, next to the human UTC labels), `valid`, `measurement`,
  and `reportable` (always false here).

Statuses:

| Status | Meaning |
|---|---|
| `pass` / `fail` | Decided by the receipts |
| `unsupported` | The tool lacks the capability; nothing ran. This is not a failure |
| `not_applicable` | The check does not apply to the tool's model; the reason is recorded |
| `blocked` | A prerequisite failed, or the evidence was inconclusive (for example a nonzero exit without the intended diagnostic, or provisioning exit 77 `RWB-BLOCKED`) |
| `observed` | Informational |
| `error` | Harness fault. The run is invalid |

`valid` means the evidence is trustworthy and teardown was clean. It does not mean the tool
passed. A blocked or invalid run publishes no timings in `meta.json` or `summary.md`; its raw
command timings stay in `steps.jsonl` as diagnostics.

## Timing boundaries (do not rank across tools by a single command)

- `first_task.<co>`: the time to first verified work. It starts from a prepared checkout and
  covers setup, start, readiness, app dependencies, first identity, migrate, mark/CRUD/cache
  and pytest. `steps` sums those receipts, and `wall` also includes harness verification.
  Use this, not `setup.*`, for cross-tool task time. Tool operations performed in `prepare`
  (native worktree creation) are listed in `prepare_scope`, excluded from first_task, and
  reported as `prepare.<co>`.
- `setup.first_checkout` / `setup.second_checkout`: one command each, with different scope per
  tool (`setup_scope`). For example, Stack's is `stack compile` only, and its tools install
  at `stack up`.
- "First checkout" means first in this run. It is not a universal cold install: images may
  hold preseeded Nix stores or Docker layers (`cache_note`).
- `ready.<co>`: start, native readiness and the first identity. `start_scope` labels lanes
  whose start also does readiness, such as dnvr's PTY driver.
- `repeat.*`: outer times (host `docker exec`, including transport) and inner times (inside
  the container), reported separately.

## Correctness gates (fail closed)

- App receipts need exit 0, an `ok` JSON object and the exact result shape.
- The running code must carry the checkout's source token and live under its path.
- URL ports must equal the server ports, or be proven equal by an adapter's Docker port-map
  receipt. A declared port map that prints no valid receipt fails.
- Isolation depends on the declared boundary (separate servers, separate containers, or
  per-checkout databases on shared servers).
- Persistence: PostgreSQL rows, and Redis data after an explicit SAVE.
- Bad config and occupied port pass only on the intended diagnostic: the bad version or
  endpoint, or an address-in-use message near the port. A failed scripted readiness counts
  only if the tool's own service logs or status (`conflict_logs`) name the conflict.
- The benchmark-owned listener is ownership-tagged and confirmed to hold the port. Cleanup
  signals only a pid that still runs that tag.

## Known limitations

- Versions differ between lanes. Stack, mise and Compose run Python 3.13.16. Nix, devenv,
  Pixi, Flox and Devbox run 3.13.15, because nixpkgs and conda-forge have no 3.13.16. Devbox
  runs PostgreSQL 17.10 and Flox runs uv 0.12.17. Some container/worktree lanes use
  PostgreSQL 17.6 images. Guix is PostgreSQL 16 / Redis 7. Each lane's pins record its
  deviations.
- devenv uses the shared Nix-lane nixpkgs, not the research pin, for version parity
  (recorded as `deviation`).
- Pixi installs the PyPI set with its own resolver (same top-level pins as `uv.lock`).
- Some features are user scripting, not tool features, and are declared `scripted`:
  - Nix and Pixi service lifecycles (`adapters/_shared/rwb-services.sh`)
  - the activation holder that keeps Flox services alive
  - the Devbox initdb and stop confirmation
- All results so far come from Linux containers on an Apple Silicon host. Most lanes run inside
  one container through `docker exec`. The Compose lane is the exception: `docker compose` runs
  on the macOS host (host transport), while its services and app run in Linux containers. None
  of these are native macOS results. Each run records its `transport`.
- Smoke runs used 2 samples and 1 warmup and are diagnostic only.
