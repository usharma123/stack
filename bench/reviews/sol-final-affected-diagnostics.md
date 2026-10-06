# Sol final affected diagnostics

All 382 offline tests passed. Seven fresh diagnostics ran serially in the requested order with 2 repeats, 1 warmup and keep=false. Every run remains reportable=false. No final timing report or measurement manifest was produced. No implementation edits, commits, rebuilds, delegation or broad cleanup occurred.

DDEV has a bounded adapter blocker: setup attempts to pull the not-yet-built custom app image from a registry. Its raw failure is valid diagnostic data, but this recipe cannot supply a measurement of normal DDEV startup. The other six lanes completed their workloads. Credential failures remain blocked rather than passing bad configuration.

## Execution and source

Sole executor checkout: `/Users/utsavsharma/.t3/projects/stack`. HEAD stayed `bab3a05fc9eb365dacfee86155415ef2878dbd0a`. The preexisting untracked `bench/research/HANDOFF.md` made harness.dirty=true; dirty is recorded truthfully. Fresh per-run harness, fixture, shared-glue and lane-config hashes are in each meta.json and were independently compared with the current bytes. No mismatch was found. Baseline and final source inventories also include tests and product source.

Full-test receipt: `bench/results/final-affected-diagnostics-20261006T165911Z/evidence/unittest-receipt.txt`. Command `python3 -m unittest discover -s bench/tests`, exit 0, 382 tests, OK. The original command and complete terminal output remain in this task's T3 activity; the file captures the terminal summary.

The bad-config gate commit `646154c6ac56d5b701b2fb8af2e0c17799f8db84` remains in HEAD ancestry. Receipt `bench/results/final-affected-diagnostics-20261006T165911Z/evidence/gate-and-source.json`. The current source bytes, including the gate, are anchored by `bench/results/final-affected-diagnostics-20261006T165911Z/evidence/baseline.json` and the per-run meta.json files.

| Source file | Current SHA256 |
|---|---|
| `bench/run.py` | `06632638aef07b72ed0ba30a1ddfeb3143ce1d8c9627812bb3fe280d8009924d` |
| `bench/report.py` | `f7bacbb250635017cbfcf67711e2ebea430f45babf0379c83f7c9df4dcf9f434` |
| `bench/rwb/scenario.py` | `a4e2c00016164775dc9064e412723f95995c88817daa73c1b44b2d95878508c3` |
| `bench/rwb/verify.py` | `b02793fb279e334315c2ae243015e373982ad3a4f9e4128544c3058456c9a3c8` |
| `bench/rwb/transport.py` | `15e690cc72e65a566364f1d9f1b4c26ed6f99806e11cfa1c270dc24b6fb5b528` |
| `bench/rwb/adapters/devpod.py` | `e8e3a4fb24c3dc4d0c3893b58a994a8f534878af85173b090e117c60da80b779` |
| `bench/rwb/adapters/ddev.py` | `c1c3c128a5884679aeb147c4f717100d8d7120278b5334fd8a828f619c882211` |
| `bench/rwb/adapters/lando.py` | `0312832e8780e20bb3a5fd7a5d45f1a42f3392cb9871d970a531a56f1c826fe2` |
| `bench/rwb/adapters/git_grove.py` | `ee21232280df95b7897e9e063000c120ae792e6c5f62c39e49587fbb90d5ed02` |
| `bench/rwb/adapters/tilt.py` | `5383054065974a33c28160041cae9b65948b9e912cad64797e3f24b0bb426136` |
| `bench/rwb/adapters/vagrant.py` | `ba90da50e33dcbed2c8f8b575ddab79a88f2f2ff1b60f7fafed1fdb59403e126` |
| `bench/rwb/adapters/mise.py` | `976138d66e189ad4e991e24ac9334569efc0b154019c81dadb498a5ec49447ff` |

## Attempts

| Tool | Exact output path | Run ID | Valid evidence | Outcomes | bad_config |
|---|---|---|---|---|---|
| devpod | `bench/results/final-affected-diagnostics-20261006T165911Z/devpod` | `20261006t170035-84de96` | true | 25 pass, 2 observed, 1 blocked, 1 not_applicable | blocked |
| ddev | `bench/results/final-affected-diagnostics-20261006T165911Z/ddev` | `20261006t170225-55ad4c` | true | 1 fail, 1 pass, 1 observed | not reached |
| lando | `bench/results/final-affected-diagnostics-20261006T165911Z/lando` | `20261006t170301-62e228` | true | 26 pass, 2 observed, 1 not_applicable | pass |
| git-grove | `bench/results/final-affected-diagnostics-20261006T165911Z/git-grove` | `20261006t170524-aa7302` | true | 26 pass, 2 observed, 1 blocked | blocked |
| tilt | `bench/results/final-affected-diagnostics-20261006T165911Z/tilt` | `20261006t170632-aa1c0f` | true | 23 pass, 2 unsupported, 2 observed, 1 blocked, 1 not_applicable | blocked |
| vagrant | `bench/results/final-affected-diagnostics-20261006T165911Z/vagrant` | `20261006t170729-5c253d` | true | 23 pass, 2 unsupported, 2 observed, 1 blocked, 1 not_applicable | blocked |
| mise | `bench/results/final-affected-diagnostics-20261006T165911Z/mise` | `20261006t171145-5044c5` | true | 26 pass, 2 observed, 1 blocked | blocked |

Counts describe recorded checks, not scores. DDEV ends after setup.a failure; its missing later checks were not executed and are not passes. valid=true means trustworthy receipts and clean teardown, not tool success. The runs are diagnostic and cannot be selected as final timings by this report.

## devpod

DevPod v0.6.15 is realized; its raw version output omits a newline before the following binary hash. The native stop receipt exits 0; the stopped probe shows all three A containers exited. No a-after-stop command exists, so verification does not auto-resume the workspace. Explicit restart, B survival and PostgreSQL/Redis persistence pass. The nonexistent Python tag only reaches a Docker credential-helper failure, and bad_config is correctly blocked.

Metadata `bench/results/final-affected-diagnostics-20261006T165911Z/devpod/meta.json`; outcomes `bench/results/final-affected-diagnostics-20261006T165911Z/devpod/outcomes.json`; command ledger `bench/results/final-affected-diagnostics-20261006T165911Z/devpod/steps.jsonl`; complete independent receipt inventory and raw-file hashes `bench/results/final-affected-diagnostics-20261006T165911Z/evidence/devpod-verification.json`.

step 4 `tool-version`, exit 0: `bench/results/final-affected-diagnostics-20261006T165911Z/devpod/logs/0004-tool-version.stdout` and `bench/results/final-affected-diagnostics-20261006T165911Z/devpod/logs/0004-tool-version.stderr`
Observed version lines: `v0.6.150c50934f4199732ff37e89708bc5c028ecda75d854a151cb2644e4ba39f4d7f7 devpod-darwin-arm64`; `Docker Compose version v2.40.3-desktop.1`.

step 10 `a-tool-versions`, exit 0: `bench/results/final-affected-diagnostics-20261006T165911Z/devpod/logs/0010-a-tool-versions.stdout` and `bench/results/final-affected-diagnostics-20261006T165911Z/devpod/logs/0010-a-tool-versions.stderr`
Observed version lines: `Python 3.13.16`; `uv 0.12.23 (aarch64-unknown-linux-musl)`; `postgres (PostgreSQL) 17.6`; `Redis server v=8.10.2 sha=00000000:1 malloc=jemalloc-5.3.0 bits=64 build=733afb4c18605b55`.

step 26 `b-tool-versions`, exit 0: `bench/results/final-affected-diagnostics-20261006T165911Z/devpod/logs/0026-b-tool-versions.stdout` and `bench/results/final-affected-diagnostics-20261006T165911Z/devpod/logs/0026-b-tool-versions.stderr`
Observed version lines: `Python 3.13.16`; `uv 0.12.23 (aarch64-unknown-linux-musl)`; `postgres (PostgreSQL) 17.6`; `Redis server v=8.10.2 sha=00000000:1 malloc=jemalloc-5.3.0 bits=64 build=733afb4c18605b55`.

- `setup.a`: pass, scripted.  Evidence steps [6].

- `start.a`: pass, native. readiness=native pg=5432 redis=6379 Evidence steps [8, 11].

- `setup.b`: pass, scripted.  Evidence steps [23].

- `start.b`: pass, native. readiness=native pg=5432 redis=6379 Evidence steps [24, 27].

- `isolation`: pass, native. boundary=container Evidence steps [35, 36].

- `stop.a`: pass, native. stop exit 0, probe exit 0, after-stop entry not run (entry auto-resumes the project) Evidence steps [45, 46].

- `b.survives`: pass, n/a.  Evidence steps [47].

- `restart.a`: pass, native. readiness=native pg=5432 redis=6379 Evidence steps [48, 49].

- `persist.pg`: pass, native.  Evidence steps [51].

- `persist.redis`: pass, native. policy={'appendonly': 'yes', 'appendfsync': 'always', 'save': '3600 1 300 100 60 10000', 'dir': '/data'} Evidence steps [51].

- `bad_config`: blocked, scripted. setup exit 0, start exit 1: environment prerequisite failed before the intended refusal: 13:02:04 info devcontainer up: start container: build and extend docker-compose: inspect image python:3.13.99-slim-bookworm: get image config remotely: retrieve image python:3.13.99-slim-bookworm: err Evidence steps [58, 59, 60, 61].

- `occupied_port`: not_applicable, n/a. no host port is published for PostgreSQL/Redis; each checkout uses internal ports on its own Compose network, so a host listener cannot collide Evidence steps [].

- `cleanup.processes`: pass, native.  Evidence steps [66].

- `cleanup.supervisors`: observed, n/a. none Evidence steps [67].

Key raw receipts:
- step 1 `preflight`, exit 0: `bench/results/final-affected-diagnostics-20261006T165911Z/devpod/logs/0001-preflight.stdout` and `bench/results/final-affected-diagnostics-20261006T165911Z/devpod/logs/0001-preflight.stderr`
- step 5 `a-prepare`, exit 0: `bench/results/final-affected-diagnostics-20261006T165911Z/devpod/logs/0005-a-prepare.stdout` and `bench/results/final-affected-diagnostics-20261006T165911Z/devpod/logs/0005-a-prepare.stderr`
- step 8 `a-start`, exit 0: `bench/results/final-affected-diagnostics-20261006T165911Z/devpod/logs/0008-a-start.stdout` and `bench/results/final-affected-diagnostics-20261006T165911Z/devpod/logs/0008-a-start.stderr`
- step 24 `b-start`, exit 0: `bench/results/final-affected-diagnostics-20261006T165911Z/devpod/logs/0024-b-start.stdout` and `bench/results/final-affected-diagnostics-20261006T165911Z/devpod/logs/0024-b-start.stderr`
- step 45 `a-stop`, exit 0: `bench/results/final-affected-diagnostics-20261006T165911Z/devpod/logs/0045-a-stop.stdout` and `bench/results/final-affected-diagnostics-20261006T165911Z/devpod/logs/0045-a-stop.stderr`
- step 46 `a-stopped-probe`, exit 0: `bench/results/final-affected-diagnostics-20261006T165911Z/devpod/logs/0046-a-stopped-probe.stdout` and `bench/results/final-affected-diagnostics-20261006T165911Z/devpod/logs/0046-a-stopped-probe.stderr`
- step 59 `d-setup-invalid`, exit 0: `bench/results/final-affected-diagnostics-20261006T165911Z/devpod/logs/0059-d-setup-invalid.stdout` and `bench/results/final-affected-diagnostics-20261006T165911Z/devpod/logs/0059-d-setup-invalid.stderr`
- step 60 `d-start-invalid`, exit 1: `bench/results/final-affected-diagnostics-20261006T165911Z/devpod/logs/0060-d-start-invalid.stdout` and `bench/results/final-affected-diagnostics-20261006T165911Z/devpod/logs/0060-d-start-invalid.stderr`
- step 62 `d-cleanup`, exit 0: `bench/results/final-affected-diagnostics-20261006T165911Z/devpod/logs/0062-d-cleanup.stdout` and `bench/results/final-affected-diagnostics-20261006T165911Z/devpod/logs/0062-d-cleanup.stderr`
- step 65 `a-cleanup`, exit 0: `bench/results/final-affected-diagnostics-20261006T165911Z/devpod/logs/0065-a-cleanup.stdout` and `bench/results/final-affected-diagnostics-20261006T165911Z/devpod/logs/0065-a-cleanup.stderr`
- step 66 `leftover-processes`, exit 0: `bench/results/final-affected-diagnostics-20261006T165911Z/devpod/logs/0066-leftover-processes.stdout` and `bench/results/final-affected-diagnostics-20261006T165911Z/devpod/logs/0066-leftover-processes.stderr`
- step 67 `leftover-supervisors`, exit 0: `bench/results/final-affected-diagnostics-20261006T165911Z/devpod/logs/0067-leftover-supervisors.stdout` and `bench/results/final-affected-diagnostics-20261006T165911Z/devpod/logs/0067-leftover-supervisors.stderr`
- step 68 `host-cleanup`, exit 0: `bench/results/final-affected-diagnostics-20261006T165911Z/devpod/logs/0068-host-cleanup.stdout` and `bench/results/final-affected-diagnostics-20261006T165911Z/devpod/logs/0068-host-cleanup.stderr`

Independent reparse, step 59: `no terminal refusal`. 

Independent reparse, step 60: `prerequisite`. 13:02:04 info devcontainer up: start container: build and extend docker-compose: inspect image python:3.13.99-slim-bookworm: get image config remotely: retrieve image python:3.13.99-slim-bookworm: error getting credentials - err: exit status 1, out: ``

Post-run resource comparison `bench/results/final-affected-diagnostics-20261006T165911Z/evidence/after-devpod-comparison.json`; live process snapshot `bench/results/final-affected-diagnostics-20261006T165911Z/evidence/after-devpod-processes.txt`. Owned host processes, temp workdirs and image tags remaining: 0, 0, 0. Missing raw logs: 0. errors=[]; cleanup_problems=[]. Native artifact inventory, sizes and hashes are in the verification JSON, including explicit zero-file inventories where no artifacts were copied.

## ddev

Preflight seals the same Docker Desktop current context in a private config with auths and credsStore removed. setup.a fails in ddev utility download-images: DDEV strips -built from the custom image name and tries pulling ddev-<run>-a-app. That repository is generated locally and was never published. The denial is a recipe sequencing bug, not evidence that DDEV cannot run Python or that public pulls remain credential-blocked. There is no a-start, no runtime app identity, no repeat sample, no bad-config probe and no B checkout. Cleanup successfully pulls ddev/ddev-utilities:latest anonymously, removes the private shared resources, and leaves the baseline unchanged. Bounded blocker: bench/rwb/adapters/ddev.py:187-189 with bench/adapters/ddev/.ddev/docker-compose.workload.yaml:9-13. DDEV upstream FindServiceImages strips the -built suffix before download-images pulls all discovered images. A future authorized change must correct that preparation/start sequence; no fix or rerun was attempted here.
Source and raw-failure receipt: `bench/results/final-affected-diagnostics-20261006T165911Z/evidence/ddev-bounded-source-blocker.json`.

Metadata `bench/results/final-affected-diagnostics-20261006T165911Z/ddev/meta.json`; outcomes `bench/results/final-affected-diagnostics-20261006T165911Z/ddev/outcomes.json`; command ledger `bench/results/final-affected-diagnostics-20261006T165911Z/ddev/steps.jsonl`; complete independent receipt inventory and raw-file hashes `bench/results/final-affected-diagnostics-20261006T165911Z/evidence/ddev-verification.json`.

step 3 `tool-version`, exit 0: `bench/results/final-affected-diagnostics-20261006T165911Z/ddev/logs/0003-tool-version.stdout` and `bench/results/final-affected-diagnostics-20261006T165911Z/ddev/logs/0003-tool-version.stderr`
Observed version lines: `ddev version v1.25.4`; `Docker Compose version v2.40.3-desktop.1`.

- `setup.a`: fail, scripted. exit 1 Evidence steps [5].

- `cleanup.processes`: pass, native.  Evidence steps [6].

- `cleanup.supervisors`: observed, n/a. none Evidence steps [7].

Key raw receipts:
- step 1 `preflight`, exit 0: `bench/results/final-affected-diagnostics-20261006T165911Z/ddev/logs/0001-preflight.stdout` and `bench/results/final-affected-diagnostics-20261006T165911Z/ddev/logs/0001-preflight.stderr`
- step 4 `a-prepare`, exit 0: `bench/results/final-affected-diagnostics-20261006T165911Z/ddev/logs/0004-a-prepare.stdout` and `bench/results/final-affected-diagnostics-20261006T165911Z/ddev/logs/0004-a-prepare.stderr`
- step 6 `leftover-processes`, exit 0: `bench/results/final-affected-diagnostics-20261006T165911Z/ddev/logs/0006-leftover-processes.stdout` and `bench/results/final-affected-diagnostics-20261006T165911Z/ddev/logs/0006-leftover-processes.stderr`
- step 7 `leftover-supervisors`, exit 0: `bench/results/final-affected-diagnostics-20261006T165911Z/ddev/logs/0007-leftover-supervisors.stdout` and `bench/results/final-affected-diagnostics-20261006T165911Z/ddev/logs/0007-leftover-supervisors.stderr`
- step 8 `host-cleanup`, exit 0: `bench/results/final-affected-diagnostics-20261006T165911Z/ddev/logs/0008-host-cleanup.stdout` and `bench/results/final-affected-diagnostics-20261006T165911Z/ddev/logs/0008-host-cleanup.stderr`

Post-run resource comparison `bench/results/final-affected-diagnostics-20261006T165911Z/evidence/after-ddev-comparison.json`; live process snapshot `bench/results/final-affected-diagnostics-20261006T165911Z/evidence/after-ddev-processes.txt`. Owned host processes, temp workdirs and image tags remaining: 0, 0, 0. Missing raw logs: 0. errors=[]; cleanup_problems=[]. Native artifact inventory, sizes and hashes are in the verification JSON, including explicit zero-file inventories where no artifacts were copied.

## lando

Anonymous public pulls and both workloads succeed. The merged private config records skipInstallCa=true, buildEngine=false, buildx=false, orchestrator=false, installPlugins=false and skipCommonPlugins=true. orchestratorBin resolves to the private docker-compose-v2.40.3 file, whose actual version and SHA256 8cd7eb5f95bacb536cc407111662e2c205d67d9abfea5dcb8400be8418db60d1 are recorded. No sudo/host CA installation attempt appears in the raw command logs. This proves the configured executable selection and successful startup; it is not a separate syscall execution trace or a global host-filesystem audit. The bad Python tag reaches an explicit registry not-found refusal, so bad_config legitimately passes.

Metadata `bench/results/final-affected-diagnostics-20261006T165911Z/lando/meta.json`; outcomes `bench/results/final-affected-diagnostics-20261006T165911Z/lando/outcomes.json`; command ledger `bench/results/final-affected-diagnostics-20261006T165911Z/lando/steps.jsonl`; complete independent receipt inventory and raw-file hashes `bench/results/final-affected-diagnostics-20261006T165911Z/evidence/lando-verification.json`.

step 5 `tool-version`, exit 0: `bench/results/final-affected-diagnostics-20261006T165911Z/lando/logs/0005-tool-version.stdout` and `bench/results/final-affected-diagnostics-20261006T165911Z/lando/logs/0005-tool-version.stderr`
Observed version lines: `v3.26.9`; `8dd9b6306a05816c3d8cdb2f4c4b9fd6d9df76490992fb0f2e331a894fd92b95 lando-macos-arm64-v3.26.9`; `Docker Compose version v2.40.3`; `lando                v3.26.9`; `@lando/python        v1.4.3`; `@lando/postgres      v1.6.0`; `Docker Compose version v2.40.3-desktop.1`.

step 11 `a-tool-versions`, exit 0: `bench/results/final-affected-diagnostics-20261006T165911Z/lando/logs/0011-a-tool-versions.stdout` and `bench/results/final-affected-diagnostics-20261006T165911Z/lando/logs/0011-a-tool-versions.stderr`
Observed version lines: `Python 3.13.16`; `uv 0.12.23 (aarch64-unknown-linux-gnu)`; `postgres (PostgreSQL) 17.6`; `Redis server v=8.10.2 sha=00000000:1 malloc=jemalloc-5.3.0 bits=64 build=26142e2a33b527c`.

step 27 `b-tool-versions`, exit 0: `bench/results/final-affected-diagnostics-20261006T165911Z/lando/logs/0027-b-tool-versions.stdout` and `bench/results/final-affected-diagnostics-20261006T165911Z/lando/logs/0027-b-tool-versions.stderr`
Observed version lines: `Python 3.13.16`; `uv 0.12.23 (aarch64-unknown-linux-gnu)`; `postgres (PostgreSQL) 17.6`; `Redis server v=8.10.2 sha=00000000:1 malloc=jemalloc-5.3.0 bits=64 build=26142e2a33b527c`.

- `setup.a`: pass, scripted.  Evidence steps [7].

- `start.a`: pass, native. readiness=scripted pg=5432 redis=6379 Evidence steps [9, 12].

- `setup.b`: pass, scripted.  Evidence steps [24].

- `start.b`: pass, native. readiness=scripted pg=5432 redis=6379 Evidence steps [25, 28].

- `isolation`: pass, native. boundary=container Evidence steps [36, 37].

- `stop.a`: pass, native. stop exit 0, probe exit 0, app after stop exit 1 Evidence steps [46, 47, 48].

- `b.survives`: pass, n/a.  Evidence steps [49].

- `restart.a`: pass, native. readiness=scripted pg=5432 redis=6379 Evidence steps [50, 51].

- `persist.pg`: pass, native.  Evidence steps [53].

- `persist.redis`: pass, native. policy={'appendonly': 'yes', 'appendfsync': 'everysec', 'save': '3600 1 300 100 60 10000', 'dir': '/data'} Evidence steps [53].

- `bad_config`: pass, scripted. setup exit 0, start exit 1 (intended diagnostic) Evidence steps [60, 61, 62, 63].

- `occupied_port`: not_applicable, n/a. no host port is published for PostgreSQL/Redis; each checkout uses internal ports on its own Compose network, so a host listener cannot collide Evidence steps [].

- `cleanup.processes`: pass, native.  Evidence steps [68].

- `cleanup.supervisors`: observed, n/a. none Evidence steps [69].

Key raw receipts:
- step 1 `preflight`, exit 0: `bench/results/final-affected-diagnostics-20261006T165911Z/lando/logs/0001-preflight.stdout` and `bench/results/final-affected-diagnostics-20261006T165911Z/lando/logs/0001-preflight.stderr`
- step 4 `lando-private-config`, exit 0: `bench/results/final-affected-diagnostics-20261006T165911Z/lando/logs/0004-lando-private-config.stdout` and `bench/results/final-affected-diagnostics-20261006T165911Z/lando/logs/0004-lando-private-config.stderr`
- step 6 `a-prepare`, exit 0: `bench/results/final-affected-diagnostics-20261006T165911Z/lando/logs/0006-a-prepare.stdout` and `bench/results/final-affected-diagnostics-20261006T165911Z/lando/logs/0006-a-prepare.stderr`
- step 9 `a-start`, exit 0: `bench/results/final-affected-diagnostics-20261006T165911Z/lando/logs/0009-a-start.stdout` and `bench/results/final-affected-diagnostics-20261006T165911Z/lando/logs/0009-a-start.stderr`
- step 25 `b-start`, exit 0: `bench/results/final-affected-diagnostics-20261006T165911Z/lando/logs/0025-b-start.stdout` and `bench/results/final-affected-diagnostics-20261006T165911Z/lando/logs/0025-b-start.stderr`
- step 46 `a-stop`, exit 0: `bench/results/final-affected-diagnostics-20261006T165911Z/lando/logs/0046-a-stop.stdout` and `bench/results/final-affected-diagnostics-20261006T165911Z/lando/logs/0046-a-stop.stderr`
- step 47 `a-stopped-probe`, exit 0: `bench/results/final-affected-diagnostics-20261006T165911Z/lando/logs/0047-a-stopped-probe.stdout` and `bench/results/final-affected-diagnostics-20261006T165911Z/lando/logs/0047-a-stopped-probe.stderr`
- step 61 `d-setup-invalid`, exit 0: `bench/results/final-affected-diagnostics-20261006T165911Z/lando/logs/0061-d-setup-invalid.stdout` and `bench/results/final-affected-diagnostics-20261006T165911Z/lando/logs/0061-d-setup-invalid.stderr`
- step 62 `d-start-invalid`, exit 1: `bench/results/final-affected-diagnostics-20261006T165911Z/lando/logs/0062-d-start-invalid.stdout` and `bench/results/final-affected-diagnostics-20261006T165911Z/lando/logs/0062-d-start-invalid.stderr`
- step 64 `d-cleanup`, exit 0: `bench/results/final-affected-diagnostics-20261006T165911Z/lando/logs/0064-d-cleanup.stdout` and `bench/results/final-affected-diagnostics-20261006T165911Z/lando/logs/0064-d-cleanup.stderr`
- step 67 `a-cleanup`, exit 0: `bench/results/final-affected-diagnostics-20261006T165911Z/lando/logs/0067-a-cleanup.stdout` and `bench/results/final-affected-diagnostics-20261006T165911Z/lando/logs/0067-a-cleanup.stderr`
- step 68 `leftover-processes`, exit 0: `bench/results/final-affected-diagnostics-20261006T165911Z/lando/logs/0068-leftover-processes.stdout` and `bench/results/final-affected-diagnostics-20261006T165911Z/lando/logs/0068-leftover-processes.stderr`
- step 69 `leftover-supervisors`, exit 0: `bench/results/final-affected-diagnostics-20261006T165911Z/lando/logs/0069-leftover-supervisors.stdout` and `bench/results/final-affected-diagnostics-20261006T165911Z/lando/logs/0069-leftover-supervisors.stderr`
- step 70 `host-cleanup`, exit 0: `bench/results/final-affected-diagnostics-20261006T165911Z/lando/logs/0070-host-cleanup.stdout` and `bench/results/final-affected-diagnostics-20261006T165911Z/lando/logs/0070-host-cleanup.stderr`

Independent reparse, step 61: `no terminal refusal`. 

Independent reparse, step 62: `intended`. Error response from daemon: failed to resolve reference "docker.io/library/python:3.13.99-bookworm": docker.io/library/python:3.13.99-bookworm: not found

Post-run resource comparison `bench/results/final-affected-diagnostics-20261006T165911Z/evidence/after-lando-comparison.json`; live process snapshot `bench/results/final-affected-diagnostics-20261006T165911Z/evidence/after-lando-processes.txt`. Owned host processes, temp workdirs and image tags remaining: 0, 0, 0. Missing raw logs: 0. errors=[]; cleanup_problems=[]. Native artifact inventory, sizes and hashes are in the verification JSON, including explicit zero-file inventories where no artifacts were copied.

## git-grove

Initialization now completes with serial provision, version, repository git init, worktree preparation and setup receipts. Actual grove start <branch> --json succeeds for both A and B and identifies the corresponding worktree and custom-shell provider. No native grove init command is invoked by this adapter; its checked-in Grove config is staged by preparation. The fixture source token, actual container workload and distinct database/Redis instances prove startup beyond a version/help command. Bad configuration remains blocked by Docker credentials.

Metadata `bench/results/final-affected-diagnostics-20261006T165911Z/git-grove/meta.json`; outcomes `bench/results/final-affected-diagnostics-20261006T165911Z/git-grove/outcomes.json`; command ledger `bench/results/final-affected-diagnostics-20261006T165911Z/git-grove/steps.jsonl`; complete independent receipt inventory and raw-file hashes `bench/results/final-affected-diagnostics-20261006T165911Z/evidence/git-grove-verification.json`.

step 3 `tool-version`, exit 0: `bench/results/final-affected-diagnostics-20261006T165911Z/git-grove/logs/0003-tool-version.stdout` and `bench/results/final-affected-diagnostics-20261006T165911Z/git-grove/logs/0003-tool-version.stderr`
Observed version lines: `v0.1.0-alpha.1.8`; `uv 0.12.23 (46b84fd0b 2026-10-03 aarch64-apple-darwin)`; `Python 3.13.16 (main, Oct  3 2026, 00:54:36) [Clang 22.1.3 ]`; `Docker Compose version v2.40.3-desktop.1`.

step 9 `a-tool-versions`, exit 0: `bench/results/final-affected-diagnostics-20261006T165911Z/git-grove/logs/0009-a-tool-versions.stdout` and `bench/results/final-affected-diagnostics-20261006T165911Z/git-grove/logs/0009-a-tool-versions.stderr`
Observed version lines: `Python 3.13.16 (main, Oct  6 2026, 02:09:20) [GCC 12.2.0]`; `uv 0.12.23 (aarch64-unknown-linux-musl)`; `RWB_POSTGRES_IMAGE=postgres:17.6-alpine@sha256:ef257d85f76e48da1c64832459b59fcaba1a4dac97bf5d7450c77753542eee94`.

step 25 `b-tool-versions`, exit 0: `bench/results/final-affected-diagnostics-20261006T165911Z/git-grove/logs/0025-b-tool-versions.stdout` and `bench/results/final-affected-diagnostics-20261006T165911Z/git-grove/logs/0025-b-tool-versions.stderr`
Observed version lines: `Python 3.13.16 (main, Oct  6 2026, 02:09:20) [GCC 12.2.0]`; `uv 0.12.23 (aarch64-unknown-linux-musl)`; `RWB_POSTGRES_IMAGE=postgres:17.6-alpine@sha256:ef257d85f76e48da1c64832459b59fcaba1a4dac97bf5d7450c77753542eee94`.

- `setup.a`: pass, scripted.  Evidence steps [5].

- `start.a`: pass, scripted. readiness=scripted pg=5432 redis=6379 Evidence steps [7, 10].

- `setup.b`: pass, scripted.  Evidence steps [22].

- `start.b`: pass, scripted. readiness=scripted pg=5432 redis=6379 Evidence steps [23, 26].

- `isolation`: pass, scripted. boundary=container Evidence steps [34, 35].

- `stop.a`: pass, scripted. stop exit 0, probe exit 0, app after stop exit 1 Evidence steps [44, 45, 46].

- `b.survives`: pass, n/a.  Evidence steps [47].

- `restart.a`: pass, scripted. readiness=scripted pg=5432 redis=6379 Evidence steps [48, 49].

- `persist.pg`: pass, native.  Evidence steps [51].

- `persist.redis`: pass, native. policy={'appendonly': 'yes', 'appendfsync': 'always', 'save': '3600 1 300 100 60 10000', 'dir': '/data'} Evidence steps [51].

- `bad_config`: blocked, scripted. setup exit 1: environment prerequisite failed before the intended refusal: ERROR: failed to build: failed to solve: error getting credentials - err: exit status 1, out: `` Evidence steps [58, 59, 60].

- `occupied_port`: pass, scripted. relocated Evidence steps [64, 65, 66, 67].

- `cleanup.processes`: pass, scripted.  Evidence steps [73].

- `cleanup.supervisors`: observed, n/a. none Evidence steps [74].

Key raw receipts:
- step 4 `a-prepare`, exit 0: `bench/results/final-affected-diagnostics-20261006T165911Z/git-grove/logs/0004-a-prepare.stdout` and `bench/results/final-affected-diagnostics-20261006T165911Z/git-grove/logs/0004-a-prepare.stderr`
- step 7 `a-start`, exit 0: `bench/results/final-affected-diagnostics-20261006T165911Z/git-grove/logs/0007-a-start.stdout` and `bench/results/final-affected-diagnostics-20261006T165911Z/git-grove/logs/0007-a-start.stderr`
- step 23 `b-start`, exit 0: `bench/results/final-affected-diagnostics-20261006T165911Z/git-grove/logs/0023-b-start.stdout` and `bench/results/final-affected-diagnostics-20261006T165911Z/git-grove/logs/0023-b-start.stderr`
- step 44 `a-stop`, exit 0: `bench/results/final-affected-diagnostics-20261006T165911Z/git-grove/logs/0044-a-stop.stdout` and `bench/results/final-affected-diagnostics-20261006T165911Z/git-grove/logs/0044-a-stop.stderr`
- step 45 `a-stopped-probe`, exit 0: `bench/results/final-affected-diagnostics-20261006T165911Z/git-grove/logs/0045-a-stopped-probe.stdout` and `bench/results/final-affected-diagnostics-20261006T165911Z/git-grove/logs/0045-a-stopped-probe.stderr`
- step 59 `d-setup-invalid`, exit 1: `bench/results/final-affected-diagnostics-20261006T165911Z/git-grove/logs/0059-d-setup-invalid.stdout` and `bench/results/final-affected-diagnostics-20261006T165911Z/git-grove/logs/0059-d-setup-invalid.stderr`
- step 72 `a-cleanup`, exit 0: `bench/results/final-affected-diagnostics-20261006T165911Z/git-grove/logs/0072-a-cleanup.stdout` and `bench/results/final-affected-diagnostics-20261006T165911Z/git-grove/logs/0072-a-cleanup.stderr`
- step 73 `leftover-processes`, exit 0: `bench/results/final-affected-diagnostics-20261006T165911Z/git-grove/logs/0073-leftover-processes.stdout` and `bench/results/final-affected-diagnostics-20261006T165911Z/git-grove/logs/0073-leftover-processes.stderr`
- step 74 `leftover-supervisors`, exit 0: `bench/results/final-affected-diagnostics-20261006T165911Z/git-grove/logs/0074-leftover-supervisors.stdout` and `bench/results/final-affected-diagnostics-20261006T165911Z/git-grove/logs/0074-leftover-supervisors.stderr`
- step 75 `host-cleanup`, exit 0: `bench/results/final-affected-diagnostics-20261006T165911Z/git-grove/logs/0075-host-cleanup.stdout` and `bench/results/final-affected-diagnostics-20261006T165911Z/git-grove/logs/0075-host-cleanup.stderr`

Independent reparse, step 59: `prerequisite`. ERROR: failed to build: failed to solve: error getting credentials - err: exit status 1, out: ``

Post-run resource comparison `bench/results/final-affected-diagnostics-20261006T165911Z/evidence/after-git-grove-comparison.json`; live process snapshot `bench/results/final-affected-diagnostics-20261006T165911Z/evidence/after-git-grove-processes.txt`. Owned host processes, temp workdirs and image tags remaining: 0, 0, 0. Missing raw logs: 0. errors=[]; cleanup_problems=[]. Native artifact inventory, sizes and hashes are in the verification JSON, including explicit zero-file inventories where no artifacts were copied.

## tilt

Both workloads and lifecycle/persistence checks pass. The raw leftover-supervisors receipt identifies the D-checkout Compose events --json reader. host-cleanup records a verified SIGTERM to that exact owned PID, then host-leftovers is empty. The independent host process scan, workdir scan and Docker inventories confirm absence after teardown. This exercises the repaired process cleanup instead of merely assuming Tilt down removed the reader. Bad configuration remains credential-blocked.

Metadata `bench/results/final-affected-diagnostics-20261006T165911Z/tilt/meta.json`; outcomes `bench/results/final-affected-diagnostics-20261006T165911Z/tilt/outcomes.json`; command ledger `bench/results/final-affected-diagnostics-20261006T165911Z/tilt/steps.jsonl`; complete independent receipt inventory and raw-file hashes `bench/results/final-affected-diagnostics-20261006T165911Z/evidence/tilt-verification.json`.

step 2 `tool-version`, exit 0: `bench/results/final-affected-diagnostics-20261006T165911Z/tilt/logs/0002-tool-version.stdout` and `bench/results/final-affected-diagnostics-20261006T165911Z/tilt/logs/0002-tool-version.stderr`
Observed version lines: `v0.37.8, built 2026-10-01`; `Docker Compose version v5.6.0`.

step 8 `a-tool-versions`, exit 0: `bench/results/final-affected-diagnostics-20261006T165911Z/tilt/logs/0008-a-tool-versions.stdout` and `bench/results/final-affected-diagnostics-20261006T165911Z/tilt/logs/0008-a-tool-versions.stderr`
Observed version lines: `Python 3.13.16`; `uv 0.12.23 (aarch64-unknown-linux-musl)`; `postgres (PostgreSQL) 17.6`; `Redis server v=8.10.2 sha=00000000:1 malloc=jemalloc-5.3.0 bits=64 build=733afb4c18605b55`.

step 25 `b-tool-versions`, exit 0: `bench/results/final-affected-diagnostics-20261006T165911Z/tilt/logs/0025-b-tool-versions.stdout` and `bench/results/final-affected-diagnostics-20261006T165911Z/tilt/logs/0025-b-tool-versions.stderr`
Observed version lines: `Python 3.13.16`; `uv 0.12.23 (aarch64-unknown-linux-musl)`; `postgres (PostgreSQL) 17.6`; `Redis server v=8.10.2 sha=00000000:1 malloc=jemalloc-5.3.0 bits=64 build=733afb4c18605b55`.

- `setup.a`: pass, n/a.  Evidence steps [4].

- `start.a`: pass, native. readiness=native pg=5432 redis=6379 Evidence steps [5, 6, 9].

- `setup.b`: pass, n/a.  Evidence steps [21].

- `start.b`: pass, native. readiness=native pg=5432 redis=6379 Evidence steps [22, 23, 26].

- `isolation`: pass, native. boundary=container Evidence steps [34, 35].

- `stop.a`: pass, native. stop exit 0, probe exit 0, app after stop exit 1 Evidence steps [44, 45, 46].

- `b.survives`: pass, n/a.  Evidence steps [47].

- `restart.a`: pass, native. readiness=native pg=5432 redis=6379 Evidence steps [48, 49, 50].

- `persist.pg`: pass, native.  Evidence steps [52].

- `persist.redis`: pass, native. policy={'appendonly': 'yes', 'appendfsync': 'always', 'save': '3600 1 300 100 60 10000', 'dir': '/data'} Evidence steps [52].

- `bad_config`: blocked, n/a. setup exit 0, start exit 1: environment prerequisite failed before the intended refusal: postgres │ error getting credentials - err: exit status 1, out: `` Evidence steps [55, 56, 57, 58].

- `occupied_port`: not_applicable, n/a. services publish no host ports; each project reaches postgres:5432 and redis:6379 on its own Compose network Evidence steps [].

- `cleanup.processes`: pass, native.  Evidence steps [62].

- `cleanup.supervisors`: observed, n/a. /var/folders/l3/6x2gqh2x1rbdkr21_hzckff40000gn/T/rwb-20261006t170632-aa1c0f-khhw Evidence steps [63].

Key raw receipts:
- step 3 `a-prepare`, exit 0: `bench/results/final-affected-diagnostics-20261006T165911Z/tilt/logs/0003-a-prepare.stdout` and `bench/results/final-affected-diagnostics-20261006T165911Z/tilt/logs/0003-a-prepare.stderr`
- step 5 `a-start`, exit 0: `bench/results/final-affected-diagnostics-20261006T165911Z/tilt/logs/0005-a-start.stdout` and `bench/results/final-affected-diagnostics-20261006T165911Z/tilt/logs/0005-a-start.stderr`
- step 22 `b-start`, exit 0: `bench/results/final-affected-diagnostics-20261006T165911Z/tilt/logs/0022-b-start.stdout` and `bench/results/final-affected-diagnostics-20261006T165911Z/tilt/logs/0022-b-start.stderr`
- step 44 `a-stop`, exit 0: `bench/results/final-affected-diagnostics-20261006T165911Z/tilt/logs/0044-a-stop.stdout` and `bench/results/final-affected-diagnostics-20261006T165911Z/tilt/logs/0044-a-stop.stderr`
- step 45 `a-stopped-probe`, exit 0: `bench/results/final-affected-diagnostics-20261006T165911Z/tilt/logs/0045-a-stopped-probe.stdout` and `bench/results/final-affected-diagnostics-20261006T165911Z/tilt/logs/0045-a-stopped-probe.stderr`
- step 56 `d-setup-invalid`, exit 0: `bench/results/final-affected-diagnostics-20261006T165911Z/tilt/logs/0056-d-setup-invalid.stdout` and `bench/results/final-affected-diagnostics-20261006T165911Z/tilt/logs/0056-d-setup-invalid.stderr`
- step 57 `d-start-invalid`, exit 1: `bench/results/final-affected-diagnostics-20261006T165911Z/tilt/logs/0057-d-start-invalid.stdout` and `bench/results/final-affected-diagnostics-20261006T165911Z/tilt/logs/0057-d-start-invalid.stderr`
- step 59 `d-cleanup`, exit 0: `bench/results/final-affected-diagnostics-20261006T165911Z/tilt/logs/0059-d-cleanup.stdout` and `bench/results/final-affected-diagnostics-20261006T165911Z/tilt/logs/0059-d-cleanup.stderr`
- step 61 `a-cleanup`, exit 0: `bench/results/final-affected-diagnostics-20261006T165911Z/tilt/logs/0061-a-cleanup.stdout` and `bench/results/final-affected-diagnostics-20261006T165911Z/tilt/logs/0061-a-cleanup.stderr`
- step 62 `leftover-processes`, exit 0: `bench/results/final-affected-diagnostics-20261006T165911Z/tilt/logs/0062-leftover-processes.stdout` and `bench/results/final-affected-diagnostics-20261006T165911Z/tilt/logs/0062-leftover-processes.stderr`
- step 63 `leftover-supervisors`, exit 0: `bench/results/final-affected-diagnostics-20261006T165911Z/tilt/logs/0063-leftover-supervisors.stdout` and `bench/results/final-affected-diagnostics-20261006T165911Z/tilt/logs/0063-leftover-supervisors.stderr`
- step 64 `host-cleanup`, exit 0: `bench/results/final-affected-diagnostics-20261006T165911Z/tilt/logs/0064-host-cleanup.stdout` and `bench/results/final-affected-diagnostics-20261006T165911Z/tilt/logs/0064-host-cleanup.stderr`

Independent reparse, step 56: `no terminal refusal`. 

Independent reparse, step 57: `prerequisite`. postgres │ error getting credentials - err: exit status 1, out: ``

Post-run resource comparison `bench/results/final-affected-diagnostics-20261006T165911Z/evidence/after-tilt-comparison.json`; live process snapshot `bench/results/final-affected-diagnostics-20261006T165911Z/evidence/after-tilt-processes.txt`. Owned host processes, temp workdirs and image tags remaining: 0, 0, 0. Missing raw logs: 0. errors=[]; cleanup_problems=[]. Native artifact inventory, sizes and hashes are in the verification JSON, including explicit zero-file inventories where no artifacts were copied.

## vagrant

Vagrant 2.4.9 uses the Docker provider, not a virtual machine. Both checkout workloads and lifecycle/persistence pass. The bad PostgreSQL image is named in command/progress text, but the terminal failure is the credential helper, so bad_config is correctly blocked. Native lockfile/frozen setup are unsupported and host occupied-port testing is not applicable to the unexposed per-checkout networks.

Metadata `bench/results/final-affected-diagnostics-20261006T165911Z/vagrant/meta.json`; outcomes `bench/results/final-affected-diagnostics-20261006T165911Z/vagrant/outcomes.json`; command ledger `bench/results/final-affected-diagnostics-20261006T165911Z/vagrant/steps.jsonl`; complete independent receipt inventory and raw-file hashes `bench/results/final-affected-diagnostics-20261006T165911Z/evidence/vagrant-verification.json`.

step 2 `tool-version`, exit 0: `bench/results/final-affected-diagnostics-20261006T165911Z/vagrant/logs/0002-tool-version.stdout` and `bench/results/final-affected-diagnostics-20261006T165911Z/vagrant/logs/0002-tool-version.stderr`
Observed version lines: `Vagrant 2.4.9`.

step 7 `a-tool-versions`, exit 0: `bench/results/final-affected-diagnostics-20261006T165911Z/vagrant/logs/0007-a-tool-versions.stdout` and `bench/results/final-affected-diagnostics-20261006T165911Z/vagrant/logs/0007-a-tool-versions.stderr`
Observed version lines: `Python 3.13.16`; `uv 0.12.23 (aarch64-unknown-linux-musl)`; `postgres (PostgreSQL) 17.6`; `Redis server v=8.10.2 sha=00000000:1 malloc=jemalloc-5.3.0 bits=64 build=733afb4c18605b55`.

step 23 `b-tool-versions`, exit 0: `bench/results/final-affected-diagnostics-20261006T165911Z/vagrant/logs/0023-b-tool-versions.stdout` and `bench/results/final-affected-diagnostics-20261006T165911Z/vagrant/logs/0023-b-tool-versions.stderr`
Observed version lines: `Python 3.13.16`; `uv 0.12.23 (aarch64-unknown-linux-musl)`; `postgres (PostgreSQL) 17.6`; `Redis server v=8.10.2 sha=00000000:1 malloc=jemalloc-5.3.0 bits=64 build=733afb4c18605b55`.

- `setup.a`: pass, n/a.  Evidence steps [4].

- `start.a`: pass, native. readiness=scripted pg=5432 redis=6379 Evidence steps [5, 8].

- `setup.b`: pass, n/a.  Evidence steps [20].

- `start.b`: pass, native. readiness=scripted pg=5432 redis=6379 Evidence steps [21, 24].

- `isolation`: pass, scripted. boundary=container Evidence steps [32, 33].

- `stop.a`: pass, native. stop exit 0, probe exit 0, app after stop exit 1 Evidence steps [42, 43, 44].

- `b.survives`: pass, n/a.  Evidence steps [45].

- `restart.a`: pass, native. readiness=scripted pg=5432 redis=6379 Evidence steps [46, 47].

- `persist.pg`: pass, native.  Evidence steps [49].

- `persist.redis`: pass, native. policy={'appendonly': 'yes', 'appendfsync': 'always', 'save': '3600 1 300 100 60 10000', 'dir': '/data'} Evidence steps [49].

- `bad_config`: blocked, n/a. setup exit 0, start exit 1: environment prerequisite failed before the intended refusal: docker: error getting credentials - err: exit status 1, out: `` Evidence steps [52, 53, 54, 55].

- `occupied_port`: not_applicable, n/a. services publish no host ports; each checkout reaches pg:5432 and redis:6379 on its own Docker network Evidence steps [].

- `cleanup.processes`: pass, native.  Evidence steps [62].

- `cleanup.supervisors`: observed, n/a. none Evidence steps [63].

Key raw receipts:
- step 3 `a-prepare`, exit 0: `bench/results/final-affected-diagnostics-20261006T165911Z/vagrant/logs/0003-a-prepare.stdout` and `bench/results/final-affected-diagnostics-20261006T165911Z/vagrant/logs/0003-a-prepare.stderr`
- step 5 `a-start`, exit 0: `bench/results/final-affected-diagnostics-20261006T165911Z/vagrant/logs/0005-a-start.stdout` and `bench/results/final-affected-diagnostics-20261006T165911Z/vagrant/logs/0005-a-start.stderr`
- step 21 `b-start`, exit 0: `bench/results/final-affected-diagnostics-20261006T165911Z/vagrant/logs/0021-b-start.stdout` and `bench/results/final-affected-diagnostics-20261006T165911Z/vagrant/logs/0021-b-start.stderr`
- step 42 `a-stop`, exit 0: `bench/results/final-affected-diagnostics-20261006T165911Z/vagrant/logs/0042-a-stop.stdout` and `bench/results/final-affected-diagnostics-20261006T165911Z/vagrant/logs/0042-a-stop.stderr`
- step 43 `a-stopped-probe`, exit 0: `bench/results/final-affected-diagnostics-20261006T165911Z/vagrant/logs/0043-a-stopped-probe.stdout` and `bench/results/final-affected-diagnostics-20261006T165911Z/vagrant/logs/0043-a-stopped-probe.stderr`
- step 53 `d-setup-invalid`, exit 0: `bench/results/final-affected-diagnostics-20261006T165911Z/vagrant/logs/0053-d-setup-invalid.stdout` and `bench/results/final-affected-diagnostics-20261006T165911Z/vagrant/logs/0053-d-setup-invalid.stderr`
- step 54 `d-start-invalid`, exit 1: `bench/results/final-affected-diagnostics-20261006T165911Z/vagrant/logs/0054-d-start-invalid.stdout` and `bench/results/final-affected-diagnostics-20261006T165911Z/vagrant/logs/0054-d-start-invalid.stderr`
- step 59 `d-cleanup`, exit 0: `bench/results/final-affected-diagnostics-20261006T165911Z/vagrant/logs/0059-d-cleanup.stdout` and `bench/results/final-affected-diagnostics-20261006T165911Z/vagrant/logs/0059-d-cleanup.stderr`
- step 61 `a-cleanup`, exit 0: `bench/results/final-affected-diagnostics-20261006T165911Z/vagrant/logs/0061-a-cleanup.stdout` and `bench/results/final-affected-diagnostics-20261006T165911Z/vagrant/logs/0061-a-cleanup.stderr`
- step 62 `leftover-processes`, exit 0: `bench/results/final-affected-diagnostics-20261006T165911Z/vagrant/logs/0062-leftover-processes.stdout` and `bench/results/final-affected-diagnostics-20261006T165911Z/vagrant/logs/0062-leftover-processes.stderr`
- step 63 `leftover-supervisors`, exit 0: `bench/results/final-affected-diagnostics-20261006T165911Z/vagrant/logs/0063-leftover-supervisors.stdout` and `bench/results/final-affected-diagnostics-20261006T165911Z/vagrant/logs/0063-leftover-supervisors.stderr`
- step 64 `host-cleanup`, exit 0: `bench/results/final-affected-diagnostics-20261006T165911Z/vagrant/logs/0064-host-cleanup.stdout` and `bench/results/final-affected-diagnostics-20261006T165911Z/vagrant/logs/0064-host-cleanup.stderr`

Independent reparse, step 53: `no terminal refusal`. 

Independent reparse, step 54: `prerequisite`. docker: error getting credentials - err: exit status 1, out: ``

Post-run resource comparison `bench/results/final-affected-diagnostics-20261006T165911Z/evidence/after-vagrant-comparison.json`; live process snapshot `bench/results/final-affected-diagnostics-20261006T165911Z/evidence/after-vagrant-processes.txt`. Owned host processes, temp workdirs and image tags remaining: 0, 0, 0. Missing raw logs: 0. errors=[]; cleanup_problems=[]. Native artifact inventory, sizes and hashes are in the verification JSON, including explicit zero-file inventories where no artifacts were copied.

## mise

Both checkout workloads, isolation, stop/restart, PostgreSQL and Redis persistence, frozen setup and the occupied-port refusal pass. Python is 3.13.16, PostgreSQL is 17.11 and Redis is 8.10.2. The bad-version setup exits 1: progress names postgres@99.99.99, while the terminal error only says failed to resolve postgres for linux-arm64. No terminal refusal names 99.99.99, so the retained 646154c gate correctly records blocked. This is inconclusive bad-config evidence, not an environment credential failure and not a passed refusal. Pitchfork 2.29.0 is the declared pin; startup/provisioning logs retain its versioned installation, but no standalone pf --version receipt was requested. The exact run-owned outer container is removed after its supervisor observation.

Metadata `bench/results/final-affected-diagnostics-20261006T165911Z/mise/meta.json`; outcomes `bench/results/final-affected-diagnostics-20261006T165911Z/mise/outcomes.json`; command ledger `bench/results/final-affected-diagnostics-20261006T165911Z/mise/steps.jsonl`; complete independent receipt inventory and raw-file hashes `bench/results/final-affected-diagnostics-20261006T165911Z/evidence/mise-verification.json`.

step 4 `tool-version`, exit 0: `bench/results/final-affected-diagnostics-20261006T165911Z/mise/logs/0004-tool-version.stdout` and `bench/results/final-affected-diagnostics-20261006T165911Z/mise/logs/0004-tool-version.stderr`
Observed version lines: `2026.10.3 linux-arm64 (2026-10-05)`; `357260e28904569a6e7124d33b65cef6d043846c07bb8f4906ddf22cc353d61d  /home/agent/.local/bin/mise`.

step 10 `a-tool-versions`, exit 0: `bench/results/final-affected-diagnostics-20261006T165911Z/mise/logs/0010-a-tool-versions.stdout` and `bench/results/final-affected-diagnostics-20261006T165911Z/mise/logs/0010-a-tool-versions.stderr`
Observed version lines: `/home/agent/.local/share/mise/installs/postgres/17.11/.mise-bins/postgres`; `Python 3.13.16`; `uv 0.12.23 (aarch64-unknown-linux-gnu)`; `postgres (PostgreSQL) 17.11`; `Redis server v=8.10.2 sha=e0b56509:0 malloc=jemalloc-5.3.0 bits=64 build=bf3da2cf408b0fb9`.

step 24 `b-tool-versions`, exit 0: `bench/results/final-affected-diagnostics-20261006T165911Z/mise/logs/0024-b-tool-versions.stdout` and `bench/results/final-affected-diagnostics-20261006T165911Z/mise/logs/0024-b-tool-versions.stderr`
Observed version lines: `/home/agent/.local/share/mise/installs/postgres/17.11/.mise-bins/postgres`; `Python 3.13.16`; `uv 0.12.23 (aarch64-unknown-linux-gnu)`; `postgres (PostgreSQL) 17.11`; `Redis server v=8.10.2 sha=e0b56509:0 malloc=jemalloc-5.3.0 bits=64 build=bf3da2cf408b0fb9`.

- `setup.a`: pass, native.  Evidence steps [6].

- `start.a`: pass, native. readiness=native pg=25432 redis=26379 Evidence steps [8, 11].

- `setup.b`: pass, native.  Evidence steps [21].

- `start.b`: pass, native. readiness=native pg=25433 redis=26380 Evidence steps [22, 25].

- `isolation`: pass, native. boundary=service-instance Evidence steps [32, 33].

- `stop.a`: pass, native. stop exit 0, probe exit 0, app after stop exit 3 Evidence steps [42, 43, 44].

- `b.survives`: pass, n/a.  Evidence steps [45].

- `restart.a`: pass, native. readiness=native pg=25432 redis=26379 Evidence steps [46, 47].

- `persist.pg`: pass, native.  Evidence steps [48].

- `persist.redis`: pass, native. policy={'appendonly': 'yes', 'appendfsync': 'everysec', 'save': '3600 1 300 100 60 10000', 'dir': '/home/agent/rwb/a/.data/redis'} Evidence steps [48].

- `bad_config`: blocked, native. setup exit 1: failure output lacks the intended diagnostic /99\.99\.99|postgresql_99/ Evidence steps [55, 56, 57].

- `occupied_port`: pass, native. refused-at-start Evidence steps [60, 61].

- `cleanup.processes`: pass, native.  Evidence steps [66].

- `cleanup.supervisors`: observed, n/a. /home/agent/.local/share/mise/installs/pitchfork/2.29.0/pitchfork supervisor run Evidence steps [67].

Key raw receipts:
- step 5 `a-prepare`, exit 0: `bench/results/final-affected-diagnostics-20261006T165911Z/mise/logs/0005-a-prepare.stdout` and `bench/results/final-affected-diagnostics-20261006T165911Z/mise/logs/0005-a-prepare.stderr`
- step 8 `a-start`, exit 0: `bench/results/final-affected-diagnostics-20261006T165911Z/mise/logs/0008-a-start.stdout` and `bench/results/final-affected-diagnostics-20261006T165911Z/mise/logs/0008-a-start.stderr`
- step 22 `b-start`, exit 0: `bench/results/final-affected-diagnostics-20261006T165911Z/mise/logs/0022-b-start.stdout` and `bench/results/final-affected-diagnostics-20261006T165911Z/mise/logs/0022-b-start.stderr`
- step 42 `a-stop`, exit 0: `bench/results/final-affected-diagnostics-20261006T165911Z/mise/logs/0042-a-stop.stdout` and `bench/results/final-affected-diagnostics-20261006T165911Z/mise/logs/0042-a-stop.stderr`
- step 43 `a-stopped-probe`, exit 0: `bench/results/final-affected-diagnostics-20261006T165911Z/mise/logs/0043-a-stopped-probe.stdout` and `bench/results/final-affected-diagnostics-20261006T165911Z/mise/logs/0043-a-stopped-probe.stderr`
- step 56 `d-setup-invalid`, exit 1: `bench/results/final-affected-diagnostics-20261006T165911Z/mise/logs/0056-d-setup-invalid.stdout` and `bench/results/final-affected-diagnostics-20261006T165911Z/mise/logs/0056-d-setup-invalid.stderr`
- step 65 `a-cleanup`, exit 0: `bench/results/final-affected-diagnostics-20261006T165911Z/mise/logs/0065-a-cleanup.stdout` and `bench/results/final-affected-diagnostics-20261006T165911Z/mise/logs/0065-a-cleanup.stderr`
- step 66 `leftover-processes`, exit 0: `bench/results/final-affected-diagnostics-20261006T165911Z/mise/logs/0066-leftover-processes.stdout` and `bench/results/final-affected-diagnostics-20261006T165911Z/mise/logs/0066-leftover-processes.stderr`
- step 67 `leftover-supervisors`, exit 0: `bench/results/final-affected-diagnostics-20261006T165911Z/mise/logs/0067-leftover-supervisors.stdout` and `bench/results/final-affected-diagnostics-20261006T165911Z/mise/logs/0067-leftover-supervisors.stderr`

Independent reparse, step 56: `no terminal refusal`. 

Post-run resource comparison `bench/results/final-affected-diagnostics-20261006T165911Z/evidence/after-mise-comparison.json`; live process snapshot `bench/results/final-affected-diagnostics-20261006T165911Z/evidence/after-mise-processes.txt`. Owned host processes, temp workdirs and image tags remaining: 0, 0, 0. Missing raw logs: 0. errors=[]; cleanup_problems=[]. Native artifact inventory, sizes and hashes are in the verification JSON, including explicit zero-file inventories where no artifacts were copied.

## Final absence and preservation

Preflight baseline `bench/results/final-affected-diagnostics-20261006T165911Z/evidence/baseline.json` records all 27 containers and their full IDs, image IDs, states, start/finish timestamps, restart counts and labels, plus 9 networks and 137 volumes. Every after-lane comparison and the final comparison preserve the container records, network IDs and volume names exactly. There are no new remaining containers, networks or volumes. Every host lane has an empty run-ID process scan and no remaining temporary workdir or owned image tag. Mise additionally removes its run-owned outer container. No ownership-ambiguous cleanup was encountered and no manual cleanup was needed.

Final inventories `bench/results/final-affected-diagnostics-20261006T165911Z/evidence/final.json`, `bench/results/final-affected-diagnostics-20261006T165911Z/evidence/final-comparison.json` and `bench/results/final-affected-diagnostics-20261006T165911Z/evidence/final-processes.txt`. Serialization and old-result verification `bench/results/final-affected-diagnostics-20261006T165911Z/evidence/final-integrity.json` records non-overlapping run intervals, unchanged source hashes and preservation of all 5,508 fingerprinted existing result files. No live bench/run.py executor remains at handback. Raw command times are retained as diagnostic evidence only.

Independent A/B source-token, module, actual package-version and PostgreSQL/Redis instance comparisons are in `bench/results/final-affected-diagnostics-20261006T165911Z/evidence/independent-identities.json`. DDEV has no realized identity. Public-pull, CA and orchestrator raw-line index `bench/results/final-affected-diagnostics-20261006T165911Z/evidence/public-pulls-ca-orchestrator-lines.json` retains exact filenames and line numbers.

## Stack build provenance, read only

The original durable T3 thread is `6303986c-a264-4738-9a16-7e5957b5cec9`, Benchmark Stack Against Competitors. Original activity positions 48, 62, 69 and 81 contain cargo test, the Linux build command and complete successful build output, binary/lock/image hashes with the source revision, and the handoff update. Their untruncated item IDs/text are saved locally for durable references.

Original receipts `bench/results/final-affected-diagnostics-20261006T165911Z/evidence/original-stack-build-thread-receipts.json`; handoff snapshot `bench/results/final-affected-diagnostics-20261006T165911Z/evidence/stack-handoff-snapshot.md`; current verification `bench/results/final-affected-diagnostics-20261006T165911Z/evidence/stack-provenance-current.json`. The original remains `bench/research/HANDOFF.md`, preexisting and untracked.

Source revision `06c351acc6a0744d918dc571c063ff1f6c300700`; Linux binary `/tmp/stack-bench-build/target/release/stack`; verified SHA256 `1d338d2cd92c4ee898037c9ee39dfbccb7c1e3dabae8180fbf65662ac015ce20`. The file is a Linux ARM64 ELF executable. Cargo.lock is `2739df41aa0e397eed1aadf0a2462df049f5bce543a215a3bf5fa52e23c95e30`; builder image remains `sha256:93ce27a88655056a51dbdd8f5f2d7ddc071c7b0070fb288a37b5a285fc83971e`. Current src/, Cargo.toml and Cargo.lock have no diff from that revision. The original build used the read-only checkout mount and external /tmp target directory with cargo build --release --locked. No rebuild or fresh Stack timing was run. This is recovered local build evidence, not publisher attestation or an approved final report manifest.
