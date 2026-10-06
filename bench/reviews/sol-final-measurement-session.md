# Sol final measurement session

All 26 frozen entries have retained final-session evidence. There are 26 completed final attempts and one excluded interrupted dnvr attempt. Every final attempt uses 20 repeats, 3 warmups, keep=false and default requested resources. All final raw meta.reportable values remain false. Session and attempt review references remain null, result review is pending, and zero metrics are published. This report is execution and receipt verification, not Astra approval.

Checkout `/Users/utsavsharma/.t3/projects/stack`, branch `codex/realworld-competitor-bench`, HEAD `ee96b465dc30d6a59933c6d99b499f55194104fc`. No implementation/test edits, commits, full-suite runs, product rebuilds, delegation, global configuration changes or broad cleanup were performed. Adapter provisioning and native setup ran exactly as the frozen recipes require. The previously reported 382-test pass was not rerun.

## Frozen plan and resumption

Original plan `bench/measurements/final-20261006-reviewed-1/plan.json` remains unchanged, SHA256 `d051730e7b85f9c83eb842abbfa64bf8d76a358195487d8b9aac10b78ff4e74e`, created `2026-10-06T17:19:10Z`. Its `plan-receipt.json` is unchanged. Resume supplement `resume-plan-20261006.json` was created and sealed before retry; its SHA256 is `cddb6140471ff55f62449d4a25e6a297ca609d092d58fa32da455a9c691e23c7`. The mutable, separate `manifest.json` records 27 attempts in execution order and cites both plans. The original 15-attempt manifest is preserved as `evidence/pre-resume-manifest.json`.

The original dnvr run `20261006t174831-ed019a` started at `2026-10-06T17:48:31Z`. Its last recorded command is `repeat.app_read-sample-009`, timestamp `2026-10-06T17:50:25Z`. Exact cancellation time was not observed by this executor. At `2026-10-06T17:52:06Z`, fresh host process evidence showed no benchmark executor; the live owned dnvr container remained. Outcomes and a final run timestamp were absent, completed/valid remained false, and the initial partial metadata had no reportable field. These incomplete bytes remain untouched, with missing hashes explicitly null in the excluded descriptor. They are never timing eligible.

Container `746ad57ecd0107f1c93d35dff4204329420b2823b4635adac2688e9146477f45` was identified by exact name, rwb.owner and rwb.run labels, image, creation/start timestamps and live state. Full inspect, container process list, logs and A/B native .dnvr/logs were captured separately. An immediate second inspect confirmed the same identity before exact `docker rm -f <full ID>`, with removal confirmed by the clean snapshot at `2026-10-06T17:53:29Z`. No archived PID or other resource was signalled. The clean resumption baseline then matched all 27 original containers and every original network and volume.

The retry began at `2026-10-06T17:54:37Z` in `bench/results/final-20261006-reviewed-1/dnvr-retry-1`, after the supplement at `2026-10-06T17:54:36Z`. The remaining ten entries followed the original order. Completed run intervals do not overlap; the interrupted container's ownership was removed before retry. The interval between the last partial receipt and resumption is recorded as an interruption, not host idle time. There is no claim of uninterrupted execution or whole-host idleness. Parent authorization provides the no-parallel-worker/build promise during resumption, while live receipts independently show one benchmark invocation at a time. Existing unrelated services stayed running.

## Coverage and receipt paths

All paths below are under `bench/results/final-20261006-reviewed-1/`. Each path contains `meta.json`, `outcomes.json`, `steps.jsonl`, `summary.md`, complete `logs/`, and any declared copied `artifacts/`. Every referenced raw log exists and is hashed. `evidence/resume-final-integrity.json` inventories every file by path and SHA256; `evidence/resume-all26-attempt-descriptors.json` contains actual report.py attempt command descriptors checked against the manifest. The excluded partial directory `dnvr/` lacks outcomes and a summary; its complete extant-file inventory is `evidence/resume-20261006-interrupted-raw-hashes.json`.

| Entry/tool | Result directory | Run ID | Outcomes | Receipt-eligible metrics before review |
|---|---|---|---|---|
| stack | [stack](../results/final-20261006-reviewed-1/stack/meta.json), [outcomes](../results/final-20261006-reviewed-1/stack/outcomes.json), [steps](../results/final-20261006-reviewed-1/stack/steps.jsonl) | 20261006t171921-c8eb41 | 1 fail, 2 observed, 26 pass | first_task.a, first_task.b, repeat.app_read, repeat.entry |
| mise | [mise](../results/final-20261006-reviewed-1/mise/meta.json), [outcomes](../results/final-20261006-reviewed-1/mise/outcomes.json), [steps](../results/final-20261006-reviewed-1/mise/steps.jsonl) | 20261006t171958-c79567 | 1 blocked, 2 observed, 26 pass | first_task.a, first_task.b, repeat.app_read, repeat.entry |
| flox | [flox](../results/final-20261006-reviewed-1/flox/meta.json), [outcomes](../results/final-20261006-reviewed-1/flox/outcomes.json), [steps](../results/final-20261006-reviewed-1/flox/steps.jsonl) | 20261006t172032-28a7b7 | 2 observed, 27 pass | first_task.a, first_task.b, repeat.app_read, repeat.entry |
| devbox | [devbox](../results/final-20261006-reviewed-1/devbox/meta.json), [outcomes](../results/final-20261006-reviewed-1/devbox/outcomes.json), [steps](../results/final-20261006-reviewed-1/devbox/steps.jsonl) | 20261006t172345-ef5197 | 2 observed, 27 pass | first_task.a, first_task.b, repeat.app_read, repeat.entry |
| devenv | [devenv](../results/final-20261006-reviewed-1/devenv/meta.json), [outcomes](../results/final-20261006-reviewed-1/devenv/outcomes.json), [steps](../results/final-20261006-reviewed-1/devenv/steps.jsonl) | 20261006t172749-4fe69b | 2 observed, 27 pass | first_task.a, first_task.b, repeat.app_read, repeat.entry |
| nix | [nix](../results/final-20261006-reviewed-1/nix/meta.json), [outcomes](../results/final-20261006-reviewed-1/nix/outcomes.json), [steps](../results/final-20261006-reviewed-1/nix/steps.jsonl) | 20261006t172944-ff0b3e | 2 observed, 27 pass | first_task.a, first_task.b, repeat.app_read, repeat.entry |
| pixi | [pixi](../results/final-20261006-reviewed-1/pixi/meta.json), [outcomes](../results/final-20261006-reviewed-1/pixi/outcomes.json), [steps](../results/final-20261006-reviewed-1/pixi/steps.jsonl) | 20261006t173145-8fe768 | 2 observed, 27 pass | first_task.a, first_task.b, repeat.app_read, repeat.entry |
| compose | [compose](../results/final-20261006-reviewed-1/compose/meta.json), [outcomes](../results/final-20261006-reviewed-1/compose/outcomes.json), [steps](../results/final-20261006-reviewed-1/compose/steps.jsonl) | 20261006t173215-62be8d | 1 not_applicable, 2 observed, 26 pass | first_task.a, first_task.b, repeat.app_read, repeat.entry |
| devcontainers | [devcontainers](../results/final-20261006-reviewed-1/devcontainers/meta.json), [outcomes](../results/final-20261006-reviewed-1/devcontainers/outcomes.json), [steps](../results/final-20261006-reviewed-1/devcontainers/steps.jsonl) | 20261006t173334-55bcb5 | 1 not_applicable, 2 observed, 26 pass | first_task.a, first_task.b, repeat.app_read, repeat.entry |
| devpod | [devpod](../results/final-20261006-reviewed-1/devpod/meta.json), [outcomes](../results/final-20261006-reviewed-1/devpod/outcomes.json), [steps](../results/final-20261006-reviewed-1/devpod/steps.jsonl) | 20261006t173510-1b1b4e | 1 blocked, 1 not_applicable, 2 observed, 25 pass | first_task.a, first_task.b, repeat.app_read, repeat.entry |
| ddev | [ddev](../results/final-20261006-reviewed-1/ddev/meta.json), [outcomes](../results/final-20261006-reviewed-1/ddev/outcomes.json), [steps](../results/final-20261006-reviewed-1/ddev/steps.jsonl) | 20261006t173718-c83372 | 1 fail, 1 observed, 1 pass | none |
| lando | [lando](../results/final-20261006-reviewed-1/lando/meta.json), [outcomes](../results/final-20261006-reviewed-1/lando/outcomes.json), [steps](../results/final-20261006-reviewed-1/lando/steps.jsonl) | 20261006t173725-3e750a | 1 not_applicable, 2 observed, 26 pass | first_task.a, first_task.b, repeat.app_read, repeat.entry |
| process-compose | [process-compose](../results/final-20261006-reviewed-1/process-compose/meta.json), [outcomes](../results/final-20261006-reviewed-1/process-compose/outcomes.json), [steps](../results/final-20261006-reviewed-1/process-compose/steps.jsonl) | 20261006t173929-c70737 | 1 fail, 2 observed, 26 pass | first_task.a, first_task.b, repeat.app_read, repeat.entry |
| services-flake | [services-flake](../results/final-20261006-reviewed-1/services-flake/meta.json), [outcomes](../results/final-20261006-reviewed-1/services-flake/outcomes.json), [steps](../results/final-20261006-reviewed-1/services-flake/steps.jsonl) | 20261006t174332-f5a4e8 | 1 fail, 2 observed, 26 pass | first_task.a, first_task.b, repeat.app_read, repeat.entry |
| pkgx | [pkgx](../results/final-20261006-reviewed-1/pkgx/meta.json), [outcomes](../results/final-20261006-reviewed-1/pkgx/outcomes.json), [steps](../results/final-20261006-reviewed-1/pkgx/steps.jsonl) | 20261006t174747-157710 | 2 observed, 25 pass, 2 unsupported | first_task.a, first_task.b, repeat.app_read, repeat.entry |
| dnvr | [dnvr-retry-1](../results/final-20261006-reviewed-1/dnvr-retry-1/meta.json), [outcomes](../results/final-20261006-reviewed-1/dnvr-retry-1/outcomes.json), [steps](../results/final-20261006-reviewed-1/dnvr-retry-1/steps.jsonl) | 20261006t175437-f511c6 | 2 observed, 27 pass | first_task.a, first_task.b, repeat.app_read, repeat.entry |
| guix | [guix](../results/final-20261006-reviewed-1/guix/meta.json), [outcomes](../results/final-20261006-reviewed-1/guix/outcomes.json), [steps](../results/final-20261006-reviewed-1/guix/steps.jsonl) | 20261006t175917-e3a5f2 | 28 blocked, 1 observed, 1 pass | none |
| workz | [workz](../results/final-20261006-reviewed-1/workz/meta.json), [outcomes](../results/final-20261006-reviewed-1/workz/outcomes.json), [steps](../results/final-20261006-reviewed-1/workz/steps.jsonl) | 20261006t175943-16c496 | 2 observed, 27 pass | first_task.a, first_task.b, repeat.app_read, repeat.entry |
| worktrunk | [worktrunk](../results/final-20261006-reviewed-1/worktrunk/meta.json), [outcomes](../results/final-20261006-reviewed-1/worktrunk/outcomes.json), [steps](../results/final-20261006-reviewed-1/worktrunk/steps.jsonl) | 20261006t180026-572cdd | 2 observed, 27 pass | first_task.a, first_task.b, repeat.app_read, repeat.entry |
| git-grove | [git-grove](../results/final-20261006-reviewed-1/git-grove/meta.json), [outcomes](../results/final-20261006-reviewed-1/git-grove/outcomes.json), [steps](../results/final-20261006-reviewed-1/git-grove/steps.jsonl) | 20261006t180109-89ad03 | 1 blocked, 2 observed, 26 pass | first_task.a, first_task.b, repeat.app_read, repeat.entry |
| isola | [isola](../results/final-20261006-reviewed-1/isola/meta.json), [outcomes](../results/final-20261006-reviewed-1/isola/outcomes.json), [steps](../results/final-20261006-reviewed-1/isola/steps.jsonl) | 20261006t180223-37caf7 | 1 not_applicable, 3 observed, 24 pass, 2 unsupported | first_task.a, first_task.b, repeat.app_read, repeat.entry |
| berth | [berth](../results/final-20261006-reviewed-1/berth/meta.json), [outcomes](../results/final-20261006-reviewed-1/berth/outcomes.json), [steps](../results/final-20261006-reviewed-1/berth/steps.jsonl) | 20261006t180256-9d10d9 | 1 blocked, 1 not_applicable, 2 observed, 23 pass, 2 unsupported | first_task.a, first_task.b, repeat.app_read, repeat.entry |
| branchbox | [branchbox](../results/final-20261006-reviewed-1/branchbox/meta.json), [outcomes](../results/final-20261006-reviewed-1/branchbox/outcomes.json), [steps](../results/final-20261006-reviewed-1/branchbox/steps.jsonl) | 20261006t180409-b89259 | 1 blocked, 1 not_applicable, 1 observed, 23 pass, 3 unsupported | first_task.a, first_task.b, repeat.app_read, repeat.entry |
| tilt | [tilt](../results/final-20261006-reviewed-1/tilt/meta.json), [outcomes](../results/final-20261006-reviewed-1/tilt/outcomes.json), [steps](../results/final-20261006-reviewed-1/tilt/steps.jsonl) | 20261006t180504-a16963 | 1 blocked, 1 not_applicable, 2 observed, 23 pass, 2 unsupported | first_task.a, first_task.b, repeat.app_read, repeat.entry |
| organist | [organist](../results/final-20261006-reviewed-1/organist/meta.json), [outcomes](../results/final-20261006-reviewed-1/organist/outcomes.json), [steps](../results/final-20261006-reviewed-1/organist/steps.jsonl) | 20261006t180558-6df6a6 | 2 observed, 27 pass | first_task.a, first_task.b, repeat.app_read, repeat.entry |
| vagrant | [vagrant](../results/final-20261006-reviewed-1/vagrant/meta.json), [outcomes](../results/final-20261006-reviewed-1/vagrant/outcomes.json), [steps](../results/final-20261006-reviewed-1/vagrant/steps.jsonl) | 20261006t180848-312625 | 1 blocked, 1 not_applicable, 2 observed, 23 pass, 2 unsupported | first_task.a, first_task.b, repeat.app_read, repeat.entry |

Eligibility describes receipts and current gates with only the two missing review references removed. It is not permission to publish. The independent reporter verification retains both missing review gates and publishes zero metrics. Failure counts are outcomes, not tool scores. valid=true means trustworthy receipts and clean teardown, not all checks passed.

## Missing evidence, blocks and check failures

### stack

- `occupied_port`: `fail`, mode `native`, evidence steps `[105, 106]`. start-failed-without-conflict-diagnostic (exit 1)

### mise

- `bad_config`: `blocked`, mode `native`, evidence steps `[95, 96, 97]`. setup exit 1: failure output lacks the intended diagnostic /99\.99\.99|postgresql_99/

### devpod

- `bad_config`: `blocked`, mode `scripted`, evidence steps `[98, 99, 100, 101]`. setup exit 0, start exit 1: environment prerequisite failed before the intended refusal: 13:37:08 info devcontainer up: start container: build and extend docker-compose: inspect image python:3.13.99-slim-bookworm: get image config remotely: retrieve image python:3.13.99-slim-bookworm: err

### ddev

Classification remains adapter-blocker. download-images tries to pull the not-yet-built custom app image. No normal DDEV startup or product-failure inference is permitted. The raw setup.a fail is retained. No fix loop occurred.

- `setup.a`: `fail`, mode `scripted`, evidence steps `[5]`. exit 1

Not executed, rather than passed: `b.survives, bad_config, cache.after_restart, crud_cache.a, crud_cache.b, deps.a, deps.b, isolation, lock.created, lock.frozen_copy, migrate.a, migrate.b, occupied_port, persist.pg, persist.redis, repeat.app_read, repeat.entry, restart.a, setup.b, start.a, start.b, start.repeat, status, stop.a, tests.a, tests.b`.

### process-compose

- `occupied_port`: `fail`, mode `native`, evidence steps `[103, 104, 105]`. readiness-infra-fault (exit 124)

### services-flake

- `occupied_port`: `fail`, mode `native`, evidence steps `[102, 103, 104]`. readiness-infra-fault (exit 124)

### guix

Missing successful tool-version receipt: provisioning stopped at the sandbox canary. All workload checks are explicitly not executed; no runtime versions or timings are inferred.

Provisioning block: `provision-guix-canary: guix-daemon could not set up its build sandbox in this container (see log above); --disable-chroot was not used; guix-daemon could not set up its build sandbox in this container (see log above); --disable-chroot was not used`. No sandbox relaxation or global change was made.

- `provision`: `blocked`, mode `n/a`, evidence steps `[5]`. provision-guix-canary: guix-daemon could not set up its build sandbox in this container (see log above); --disable-chroot was not used; guix-daemon could not set up its build sandbox in this container (see log above); --disable-chroot was not used
- `setup.a`: `blocked`, mode `n/a`, evidence steps `[]`. not executed: provisioning blocked
- `lock.created`: `blocked`, mode `n/a`, evidence steps `[]`. not executed: provisioning blocked
- `deps.a`: `blocked`, mode `n/a`, evidence steps `[]`. not executed: provisioning blocked
- `start.a`: `blocked`, mode `n/a`, evidence steps `[]`. not executed: provisioning blocked
- `migrate.a`: `blocked`, mode `n/a`, evidence steps `[]`. not executed: provisioning blocked
- `crud_cache.a`: `blocked`, mode `n/a`, evidence steps `[]`. not executed: provisioning blocked
- `tests.a`: `blocked`, mode `n/a`, evidence steps `[]`. not executed: provisioning blocked
- `start.repeat`: `blocked`, mode `n/a`, evidence steps `[]`. not executed: provisioning blocked
- `setup.b`: `blocked`, mode `n/a`, evidence steps `[]`. not executed: provisioning blocked
- `deps.b`: `blocked`, mode `n/a`, evidence steps `[]`. not executed: provisioning blocked
- `start.b`: `blocked`, mode `n/a`, evidence steps `[]`. not executed: provisioning blocked
- `migrate.b`: `blocked`, mode `n/a`, evidence steps `[]`. not executed: provisioning blocked
- `crud_cache.b`: `blocked`, mode `n/a`, evidence steps `[]`. not executed: provisioning blocked
- `tests.b`: `blocked`, mode `n/a`, evidence steps `[]`. not executed: provisioning blocked
- `isolation`: `blocked`, mode `n/a`, evidence steps `[]`. not executed: provisioning blocked
- `repeat.entry`: `blocked`, mode `n/a`, evidence steps `[]`. not executed: provisioning blocked
- `repeat.app_read`: `blocked`, mode `n/a`, evidence steps `[]`. not executed: provisioning blocked
- `status`: `blocked`, mode `n/a`, evidence steps `[]`. not executed: provisioning blocked
- `stop.a`: `blocked`, mode `n/a`, evidence steps `[]`. not executed: provisioning blocked
- `b.survives`: `blocked`, mode `n/a`, evidence steps `[]`. not executed: provisioning blocked
- `restart.a`: `blocked`, mode `n/a`, evidence steps `[]`. not executed: provisioning blocked
- `persist.pg`: `blocked`, mode `n/a`, evidence steps `[]`. not executed: provisioning blocked
- `persist.redis`: `blocked`, mode `n/a`, evidence steps `[]`. not executed: provisioning blocked
- `cache.after_restart`: `blocked`, mode `n/a`, evidence steps `[]`. not executed: provisioning blocked
- `lock.frozen_copy`: `blocked`, mode `n/a`, evidence steps `[]`. not executed: provisioning blocked
- `bad_config`: `blocked`, mode `n/a`, evidence steps `[]`. not executed: provisioning blocked
- `occupied_port`: `blocked`, mode `n/a`, evidence steps `[]`. not executed: provisioning blocked

### git-grove

- `bad_config`: `blocked`, mode `scripted`, evidence steps `[98, 99, 100]`. setup exit 1: environment prerequisite failed before the intended refusal: ERROR: failed to build: failed to solve: error getting credentials - err: exit status 1, out: ``

### berth

- `bad_config`: `blocked`, mode `n/a`, evidence steps `[92, 93, 94]`. setup exit 1: environment prerequisite failed before the intended refusal: error getting credentials - err: exit status 1, out: ``

### branchbox

- `bad_config`: `blocked`, mode `n/a`, evidence steps `[91, 92, 93, 94]`. setup exit 0, start exit 1: environment prerequisite failed before the intended refusal: error getting credentials - err: exit status 1, out: ``

### tilt

- `bad_config`: `blocked`, mode `n/a`, evidence steps `[95, 96, 97, 98]`. setup exit 0, start exit 1: environment prerequisite failed before the intended refusal: postgres │ error getting credentials - err: exit status 1, out: ``

### vagrant

- `bad_config`: `blocked`, mode `n/a`, evidence steps `[92, 93, 94, 95]`. setup exit 0, start exit 1: environment prerequisite failed before the intended refusal: docker: error getting credentials - err: exit status 1, out: ``

## Versions, source and deviations

The platform is Darwin 24.6.0 arm64 with harness Python 3.12.9, Docker Desktop Linux ARM64 workloads, and the host/container transport declared by each lane. Default requested cpus and memory are null. No global cache purge occurred; prior Docker layers, prebuilt ev-* image stores and host download caches are declared warm. Per-attempt setup is fresh but is not a universal cold install. Exact host/Docker/image receipts are in the original baseline and each metadata file.

Tracked source files, product source, tests, fixture, shared glue and per-lane config hashes match the frozen bytes. Each final receipt matches harness HEAD and the plan hash maps, options, resource policy, platform, transport and declared image ID. Executed successful tool-version receipts match the frozen version_contains declarations. Guix never reached a successful tool-version receipt, and that missing evidence is retained as a timing exclusion. Historical results outside this session and the first 15 final directories remain unchanged. Original evidence collectors are unchanged; the resume-only scratch helpers are separately named.

The measured Stack binary was not rebuilt. Its SHA256 remains `1d338d2cd92c4ee898037c9ee39dfbccb7c1e3dabae8180fbf65662ac015ce20`, source revision `06c351acc6a0744d918dc571c063ff1f6c300700`, with committed build receipt `bench/provenance/stack-20261006/original-stack-build-thread-receipts.json`. Current product source matches that revision; binary and build-receipt hashes remain anchored by the plan.

### stack

A: Python 3.13.16, PostgreSQL 17.11, Redis 8.10.2; B: Python 3.13.16, PostgreSQL 17.11, Redis 8.10.2

Recorded pins: `{"mise": "2026.10.3", "stack_binary": "/tmp/stack-bench-build/target/release/stack", "stack_sha256": "1d338d2cd92c4ee898037c9ee39dfbccb7c1e3dabae8180fbf65662ac015ce20"}`.

Frozen declared deviations: `[]`.

- `a-tool-versions`: exit 0, `bench/results/final-20261006-reviewed-1/stack/logs/0012-a-tool-versions.stdout` and `bench/results/final-20261006-reviewed-1/stack/logs/0012-a-tool-versions.stderr`.
- `b-tool-versions`: exit 0, `bench/results/final-20261006-reviewed-1/stack/logs/0027-b-tool-versions.stdout` and `bench/results/final-20261006-reviewed-1/stack/logs/0027-b-tool-versions.stderr`.
- `c-tool-versions`: exit 0, `bench/results/final-20261006-reviewed-1/stack/logs/0097-c-tool-versions.stdout` and `bench/results/final-20261006-reviewed-1/stack/logs/0097-c-tool-versions.stderr`.
- `tool-version`: exit 0, `bench/results/final-20261006-reviewed-1/stack/logs/0005-tool-version.stdout` and `bench/results/final-20261006-reviewed-1/stack/logs/0005-tool-version.stderr`.

Realized tool-version receipt excerpt:

```text
stack 0.1.4
2026.10.3 linux-arm64 (2026-10-05)
1d338d2cd92c4ee898037c9ee39dfbccb7c1e3dabae8180fbf65662ac015ce20  /rwb/stack-bin/stack
357260e28904569a6e7124d33b65cef6d043846c07bb8f4906ddf22cc353d61d  /home/agent/.local/bin/mise
```

### mise

A: Python 3.13.16, PostgreSQL 17.11, Redis 8.10.2; B: Python 3.13.16, PostgreSQL 17.11, Redis 8.10.2

Recorded pins: `{"mise": "2026.10.3", "pitchfork": "2.29.0", "postgres": "17.11", "python": "3.13.16", "redis": "8.10.2", "uv": "0.12.23"}`.

Frozen declared deviations: `[]`.

- `a-tool-versions`: exit 0, `bench/results/final-20261006-reviewed-1/mise/logs/0010-a-tool-versions.stdout` and `bench/results/final-20261006-reviewed-1/mise/logs/0010-a-tool-versions.stderr`.
- `b-tool-versions`: exit 0, `bench/results/final-20261006-reviewed-1/mise/logs/0024-b-tool-versions.stdout` and `bench/results/final-20261006-reviewed-1/mise/logs/0024-b-tool-versions.stderr`.
- `c-tool-versions`: exit 0, `bench/results/final-20261006-reviewed-1/mise/logs/0093-c-tool-versions.stdout` and `bench/results/final-20261006-reviewed-1/mise/logs/0093-c-tool-versions.stderr`.
- `tool-version`: exit 0, `bench/results/final-20261006-reviewed-1/mise/logs/0004-tool-version.stdout` and `bench/results/final-20261006-reviewed-1/mise/logs/0004-tool-version.stderr`.

Realized tool-version receipt excerpt:

```text
2026.10.3 linux-arm64 (2026-10-05)
357260e28904569a6e7124d33b65cef6d043846c07bb8f4906ddf22cc353d61d  /home/agent/.local/bin/mise
```

### flox

A: Python 3.13.15, PostgreSQL 17.11, Redis 8.10.2; B: Python 3.13.15, PostgreSQL 17.11, Redis 8.10.2

Recorded pins: `{"flox": "1.17.0"}`.

Frozen declared deviations: `[]`.

- `a-tool-versions`: exit 0, `bench/results/final-20261006-reviewed-1/flox/logs/0010-a-tool-versions.stdout` and `bench/results/final-20261006-reviewed-1/flox/logs/0010-a-tool-versions.stderr`.
- `b-tool-versions`: exit 0, `bench/results/final-20261006-reviewed-1/flox/logs/0024-b-tool-versions.stdout` and `bench/results/final-20261006-reviewed-1/flox/logs/0024-b-tool-versions.stderr`.
- `c-tool-versions`: exit 0, `bench/results/final-20261006-reviewed-1/flox/logs/0093-c-tool-versions.stdout` and `bench/results/final-20261006-reviewed-1/flox/logs/0093-c-tool-versions.stderr`.
- `tool-version`: exit 0, `bench/results/final-20261006-reviewed-1/flox/logs/0004-tool-version.stdout` and `bench/results/final-20261006-reviewed-1/flox/logs/0004-tool-version.stderr`.

Realized tool-version receipt excerpt:

```text
1.17.0-g486737b
nix (Determinate Nix 3.23.0) 2.35.2
```

### devbox

A: Python 3.13.15, PostgreSQL 17.10, Redis 8.10.2; B: Python 3.13.15, PostgreSQL 17.10, Redis 8.10.2

Recorded pins: `{"devbox": "0.18.4", "postgresql": "17.10", "python": "3.13.15", "redis": "8.10.2", "uv": "0.12.22"}`.

Frozen declared deviations: `[]`.

- `a-tool-versions`: exit 0, `bench/results/final-20261006-reviewed-1/devbox/logs/0011-a-tool-versions.stdout` and `bench/results/final-20261006-reviewed-1/devbox/logs/0011-a-tool-versions.stderr`.
- `b-tool-versions`: exit 0, `bench/results/final-20261006-reviewed-1/devbox/logs/0025-b-tool-versions.stdout` and `bench/results/final-20261006-reviewed-1/devbox/logs/0025-b-tool-versions.stderr`.
- `c-tool-versions`: exit 0, `bench/results/final-20261006-reviewed-1/devbox/logs/0094-c-tool-versions.stdout` and `bench/results/final-20261006-reviewed-1/devbox/logs/0094-c-tool-versions.stderr`.
- `tool-version`: exit 0, `bench/results/final-20261006-reviewed-1/devbox/logs/0005-tool-version.stdout` and `bench/results/final-20261006-reviewed-1/devbox/logs/0005-tool-version.stderr`.

Realized tool-version receipt excerpt:

```text
0.18.4
nix (Determinate Nix 3.23.0) 2.35.2
de8f1b1d90c0fafd88db417d08d9aa67ffd38cf2a3f49d2e2ca10dc762874cf3  /usr/local/bin/devbox

→ Downloading version 0.18.4...
[1F[0K✓ Downloading version 0.18.4... [DONE]
→ Verifying checksum...
[1F[0K✓ Verifying checksum... [DONE]
→ Unpacking binary...
[1F[0K✓ Unpacking binary... [DONE]
```

### devenv

A: Python 3.13.15, PostgreSQL 17.11, Redis 8.10.2; B: Python 3.13.15, PostgreSQL 17.11, Redis 8.10.2

Recorded pins: `{"devenv": "2.4.0", "devenv_rev": "b904dcb51fe48c30db250038241507f60752f222", "deviation": "nixpkgs 151fa4e8 (shared Nix lanes) instead of the research recipe's addf7cf5 (devenv's own lock: Python 3.13.9/PG 17.7/Redis 8.2.2) for version parity", "nixpkgs": "151fa4e8ddfdd8dd25d945ad94ed54a13de9f6e4"}`.

Frozen declared deviations: `["nixpkgs 151fa4e8 (shared Nix lanes) instead of the research recipe's addf7cf5 (devenv's own lock: Python 3.13.9/PG 17.7/Redis 8.2.2) for version parity"]`.

- `a-tool-versions`: exit 0, `bench/results/final-20261006-reviewed-1/devenv/logs/0012-a-tool-versions.stdout` and `bench/results/final-20261006-reviewed-1/devenv/logs/0012-a-tool-versions.stderr`.
- `b-tool-versions`: exit 0, `bench/results/final-20261006-reviewed-1/devenv/logs/0027-b-tool-versions.stdout` and `bench/results/final-20261006-reviewed-1/devenv/logs/0027-b-tool-versions.stderr`.
- `c-tool-versions`: exit 0, `bench/results/final-20261006-reviewed-1/devenv/logs/0097-c-tool-versions.stdout` and `bench/results/final-20261006-reviewed-1/devenv/logs/0097-c-tool-versions.stderr`.
- `tool-version`: exit 0, `bench/results/final-20261006-reviewed-1/devenv/logs/0005-tool-version.stdout` and `bench/results/final-20261006-reviewed-1/devenv/logs/0005-tool-version.stderr`.

Realized tool-version receipt excerpt:

```text
devenv 2.4.0+b904dcb (aarch64-linux)
/home/agent/rwb-devenv-cli/bin/devenv
nix (Determinate Nix 3.23.0) 2.35.2
```

### nix

A: Python 3.13.15, PostgreSQL 17.11, Redis 8.10.2; B: Python 3.13.15, PostgreSQL 17.11, Redis 8.10.2

Recorded pins: `{"nixpkgs": "151fa4e8ddfdd8dd25d945ad94ed54a13de9f6e4", "postgresql": "17.11", "python": "3.13.15", "redis": "8.10.2", "uv": "0.12.22"}`.

Frozen declared deviations: `[]`.

- `a-tool-versions`: exit 0, `bench/results/final-20261006-reviewed-1/nix/logs/0010-a-tool-versions.stdout` and `bench/results/final-20261006-reviewed-1/nix/logs/0010-a-tool-versions.stderr`.
- `b-tool-versions`: exit 0, `bench/results/final-20261006-reviewed-1/nix/logs/0024-b-tool-versions.stdout` and `bench/results/final-20261006-reviewed-1/nix/logs/0024-b-tool-versions.stderr`.
- `c-tool-versions`: exit 0, `bench/results/final-20261006-reviewed-1/nix/logs/0093-c-tool-versions.stdout` and `bench/results/final-20261006-reviewed-1/nix/logs/0093-c-tool-versions.stderr`.
- `tool-version`: exit 0, `bench/results/final-20261006-reviewed-1/nix/logs/0004-tool-version.stdout` and `bench/results/final-20261006-reviewed-1/nix/logs/0004-tool-version.stderr`.

Realized tool-version receipt excerpt:

```text
nix (Determinate Nix 3.23.0) 2.35.2
/nix/var/nix/profiles/default/bin/nix
```

### pixi

A: Python 3.13.15, PostgreSQL 17.11, Redis 8.10.2; B: Python 3.13.15, PostgreSQL 17.11, Redis 8.10.2

Recorded pins: `{"deviation": "Python 3.13.15 (conda-forge has no 3.13.16 for linux-aarch64); PyPI set resolved by Pixi, not fixtures/app/uv.lock", "pixi": "0.81.0", "pixi_tgz_sha256": "9f8d2113fe9dc01788a65f5c2acec34fa56b1193461a5c3e9a775d6d2d621bcb", "postgresql": "17.11", "psycopg": "3.3.6", "pytest": "9.1.1", "python": "3.13.15", "redis": "8.10.2", "redis_py": "8.1.0"}`.

Frozen declared deviations: `["Python 3.13.15 (conda-forge has no 3.13.16 for linux-aarch64); PyPI set resolved by Pixi, not fixtures/app/uv.lock"]`.

- `a-tool-versions`: exit 0, `bench/results/final-20261006-reviewed-1/pixi/logs/0010-a-tool-versions.stdout` and `bench/results/final-20261006-reviewed-1/pixi/logs/0010-a-tool-versions.stderr`.
- `b-tool-versions`: exit 0, `bench/results/final-20261006-reviewed-1/pixi/logs/0024-b-tool-versions.stdout` and `bench/results/final-20261006-reviewed-1/pixi/logs/0024-b-tool-versions.stderr`.
- `c-tool-versions`: exit 0, `bench/results/final-20261006-reviewed-1/pixi/logs/0093-c-tool-versions.stdout` and `bench/results/final-20261006-reviewed-1/pixi/logs/0093-c-tool-versions.stderr`.
- `tool-version`: exit 0, `bench/results/final-20261006-reviewed-1/pixi/logs/0004-tool-version.stdout` and `bench/results/final-20261006-reviewed-1/pixi/logs/0004-tool-version.stderr`.

Realized tool-version receipt excerpt:

```text
pixi 0.81.0
/home/agent/.local/bin/pixi
```

### compose

A: Python 3.13.16, PostgreSQL 17.11, Redis 8.10.2; B: Python 3.13.16, PostgreSQL 17.11, Redis 8.10.2

Recorded pins: `{"compose": "5.6.0", "compose_sha256": "bd714a42b46e51757fc1121085b3096b9064e3f80d3ca5a991e33f44df4f43c9", "note": "host Docker Desktop daemon; images digest-pinned in compose.yaml/Dockerfile", "postgres": "17.11-alpine", "projects": ["rwb-20261006t173215-62be8d-a", "rwb-20261006t173215-62be8d-b", "rwb-20261006t173215-62be8d-c", "rwb-20261006t173215-62be8d-d", "rwb-20261006t173215-62be8d-e"], "python": "3.13.16", "redis": "8.10.2-alpine", "uv": "0.12.23"}`.

Frozen declared deviations: `["host Docker Desktop daemon; images digest-pinned in compose.yaml/Dockerfile"]`.

- `a-tool-versions`: exit 0, `bench/results/final-20261006-reviewed-1/compose/logs/0008-a-tool-versions.stdout` and `bench/results/final-20261006-reviewed-1/compose/logs/0008-a-tool-versions.stderr`.
- `b-tool-versions`: exit 0, `bench/results/final-20261006-reviewed-1/compose/logs/0024-b-tool-versions.stdout` and `bench/results/final-20261006-reviewed-1/compose/logs/0024-b-tool-versions.stderr`.
- `c-tool-versions`: exit 0, `bench/results/final-20261006-reviewed-1/compose/logs/0095-c-tool-versions.stdout` and `bench/results/final-20261006-reviewed-1/compose/logs/0095-c-tool-versions.stderr`.
- `tool-version`: exit 0, `bench/results/final-20261006-reviewed-1/compose/logs/0002-tool-version.stdout` and `bench/results/final-20261006-reviewed-1/compose/logs/0002-tool-version.stderr`.

Realized tool-version receipt excerpt:

```text
Docker Compose version v5.6.0
client 28.1.1 server 29.1.3 linux/arm64
```

### devcontainers

A: Python 3.13.16, PostgreSQL 17.6, Redis 8.10.2; B: Python 3.13.16, PostgreSQL 17.6, Redis 8.10.2

Recorded pins: `{"entry_auto_resumes": false, "images": {"postgres": "postgres:17.6-alpine@sha256:ef257d85f76e48da1c64832459b59fcaba1a4dac97bf5d7450c77753542eee94", "python": "python:3.13.16-slim-bookworm@sha256:a1165e272e578941b84abc79e4ab38a0305cd12803a5c4247979ac7655f4d641", "redis": "redis:8.10.2-alpine@sha256:3811787313eba226a2ef38658c6ccb91cd5e110edc89c37767de373120a0e5a0", "uv": "ghcr.io/astral-sh/uv:0.12.23@sha256:61d393e44e249f2e4b526b6c7ddcecce245946826e608e11c93ad4f5bba55b21"}, "npm_integrity": "sha512-LzaoOGKQ/Zql6PsiZ4hVIYVZagzWkD65aG/1ou5/Kly5Y1PtjLg1yn7qu+LZCzVoAl6DZZ/pbz8qOO4RLNlqMg==", "npm_package": "@devcontainers/cli", "package_lock_sha256": "418c6fc1331280cac50450529407c964fd089ec2f468221dd2dbb5b91ffb653d", "receipt_sha256": "56ed1793c3dbd4631f930b402aea0e9fae182c5335af483ff05b9b34d6fee0c4", "source_commit": "5dc7533314b5ba7ec3875c30143dfe1aec644870", "validity": {"core_hooks_required": ["frozen_copy: add C to Scenario.started (setup/start create containers); host cleanup_host covers it until then"], "host_docker_daemon": "docker info must succeed; checkouts must be visible to the daemon (Docker Desktop file sharing covers the temp dir)", "owned_names": "every project/volume/image contains ownership token 20261006t173334-55bcb5", "private_state": "/var/folders/l3/6x2gqh2x1rbdkr21_hzckff40000gn/T/rwb-20261006t173334-55bcb5-1kfil8qa/w/_state only; user tool state is never read or written"}, "version": "0.89.0"}`.

Frozen declared deviations: `[]`.

- `a-tool-versions`: exit 0, `bench/results/final-20261006-reviewed-1/devcontainers/logs/0009-a-tool-versions.stdout` and `bench/results/final-20261006-reviewed-1/devcontainers/logs/0009-a-tool-versions.stderr`.
- `b-tool-versions`: exit 0, `bench/results/final-20261006-reviewed-1/devcontainers/logs/0025-b-tool-versions.stdout` and `bench/results/final-20261006-reviewed-1/devcontainers/logs/0025-b-tool-versions.stderr`.
- `c-tool-versions`: exit 0, `bench/results/final-20261006-reviewed-1/devcontainers/logs/0096-c-tool-versions.stdout` and `bench/results/final-20261006-reviewed-1/devcontainers/logs/0096-c-tool-versions.stderr`.
- `tool-version`: exit 0, `bench/results/final-20261006-reviewed-1/devcontainers/logs/0003-tool-version.stdout` and `bench/results/final-20261006-reviewed-1/devcontainers/logs/0003-tool-version.stderr`.

Realized tool-version receipt excerpt:

```text
0.89.0
v24.16.0
sha512-LzaoOGKQ/Zql6PsiZ4hVIYVZagzWkD65aG/1ou5/Kly5Y1PtjLg1yn7qu+LZCzVoAl6DZZ/pbz8qOO4RLNlqMg==
client 28.1.1 server 29.1.3 linux/arm64
Docker Compose version v2.40.3-desktop.1
```

### devpod

A: Python 3.13.16, PostgreSQL 17.6, Redis 8.10.2; B: Python 3.13.16, PostgreSQL 17.6, Redis 8.10.2

Recorded pins: `{"agent": "DevPod injects its linux agent into the app container (downloaded from the release by DevPod)", "asset_hash_source": "observed (no publisher checksum)", "assets": {"darwin-amd64": "1205fc8626d9daa011479ded3ce7271359714f0d011acfcf739adf90901a6ee8", "darwin-arm64": "0c50934f4199732ff37e89708bc5c028ecda75d854a151cb2644e4ba39f4d7f7", "linux-amd64": "cc50bce09229d5a6d448ac1d4494327f4b8f7a20321e5fcee3bfec1aef0d20c5", "linux-arm64": "9226161e0c9f5a45d0f8d1778f940498e787b650f0e0fcf3c29f1f67e7a3f272"}, "entry_auto_resumes": true, "images": {"postgres": "postgres:17.6-alpine@sha256:ef257d85f76e48da1c64832459b59fcaba1a4dac97bf5d7450c77753542eee94", "python": "python:3.13.16-slim-bookworm@sha256:a1165e272e578941b84abc79e4ab38a0305cd12803a5c4247979ac7655f4d641", "redis": "redis:8.10.2-alpine@sha256:3811787313eba226a2ef38658c6ccb91cd5e110edc89c37767de373120a0e5a0", "uv": "ghcr.io/astral-sh/uv:0.12.23@sha256:61d393e44e249f2e4b526b6c7ddcecce245946826e608e11c93ad4f5bba55b21"}, "provider": "bench/adapters/devpod/provider-docker.yaml (docker provider at the release commit)", "receipt_sha256": "56ed1793c3dbd4631f930b402aea0e9fae182c5335af483ff05b9b34d6fee0c4", "source_commit": "33d20ff8806a3fee86d8f56ed50db6108b945fc2", "validity": {"core_hooks_required": ["frozen_copy: add C to Scenario.started (setup/start create containers); host cleanup_host covers it until then", "stop.a: honour entry_auto_resumes; the after-stop app call through the tool entry restarts the project"], "host_docker_daemon": "docker info must succeed; checkouts must be visible to the daemon (Docker Desktop file sharing covers the temp dir)", "owned_names": "every project/volume/image contains ownership token 20261006t173510-1b1b4e", "private_state": "/var/folders/l3/6x2gqh2x1rbdkr21_hzckff40000gn/T/rwb-20261006t173510-1b1b4e-z2xlcoy1/w/_state only; user tool state is never read or written"}, "version": "0.6.15"}`.

Frozen declared deviations: `[]`.

- `a-tool-versions`: exit 0, `bench/results/final-20261006-reviewed-1/devpod/logs/0010-a-tool-versions.stdout` and `bench/results/final-20261006-reviewed-1/devpod/logs/0010-a-tool-versions.stderr`.
- `b-tool-versions`: exit 0, `bench/results/final-20261006-reviewed-1/devpod/logs/0026-b-tool-versions.stdout` and `bench/results/final-20261006-reviewed-1/devpod/logs/0026-b-tool-versions.stderr`.
- `c-tool-versions`: exit 0, `bench/results/final-20261006-reviewed-1/devpod/logs/0096-c-tool-versions.stdout` and `bench/results/final-20261006-reviewed-1/devpod/logs/0096-c-tool-versions.stderr`.
- `tool-version`: exit 0, `bench/results/final-20261006-reviewed-1/devpod/logs/0004-tool-version.stdout` and `bench/results/final-20261006-reviewed-1/devpod/logs/0004-tool-version.stderr`.

Realized tool-version receipt excerpt:

```text
v0.6.150c50934f4199732ff37e89708bc5c028ecda75d854a151cb2644e4ba39f4d7f7 devpod-darwin-arm64
client 28.1.1 server 29.1.3 linux/arm64
Docker Compose version v2.40.3-desktop.1
```

### ddev

No runtime application identity was reached.

Recorded pins: `{"asset_hash_source": "release checksums.txt", "assets": {"darwin-amd64": "05aa309c0e8cd7a14696da93e99efd7b91ec7a1994dd8218674fbfad283aa832", "darwin-arm64": "af68d362bf006d86e582ccd4f7e40926ba19a451b394cb375f93971272fb41ea", "linux-amd64": "65fb822f0d2874220c8f9a6b2dfec095d37c0fdc555e34dc5b0e5f4177beeb93", "linux-arm64": "41b1412c83e7e2ae04887f02a9b4bf6d441c6f662773d97979e9fe23acf93c0a"}, "entry_auto_resumes": true, "image_notes": "PostgreSQL 17.6 on Debian bookworm (DDEV customizes it with apt, so the derived image is not byte-frozen); web container ddev/ddev-webserver as chosen by DDEV", "images": {"postgres": "postgres:17.6-bookworm@sha256:f3bd19c606e442c3d7bdfa8002e03fe260a1023351e0ea4598032022b68dd6e3", "python": "python:3.13.16-slim-bookworm@sha256:a1165e272e578941b84abc79e4ab38a0305cd12803a5c4247979ac7655f4d641", "redis": "redis:8.10.2-alpine@sha256:3811787313eba226a2ef38658c6ccb91cd5e110edc89c37767de373120a0e5a0", "uv": "ghcr.io/astral-sh/uv:0.12.23@sha256:61d393e44e249f2e4b526b6c7ddcecce245946826e608e11c93ad4f5bba55b21"}, "receipt_sha256": "56ed1793c3dbd4631f930b402aea0e9fae182c5335af483ff05b9b34d6fee0c4", "source_commit": "5da91aeb9ebab0b0e66171c450b72099308d332c", "validity": {"core_hooks_required": ["frozen_copy: add C to Scenario.started (setup/start create containers); host cleanup_host covers it until then", "stop.a: honour entry_auto_resumes; the after-stop app call through the tool entry restarts the project"], "docker_client_config": "/var/folders/l3/6x2gqh2x1rbdkr21_hzckff40000gn/T/rwb-20261006t173718-c83372-1bb_mely/w/_state/docker-config: user's persisted current context, no credsStore/auths (anonymous public pulls); preflight fails closed if it resolves a different daemon endpoint; DOCKER_HOST/CONTEXT/TLS* env overrides and TLS/SkipTLSVerify contexts are rejected before any docker call; every later body (receipts, cleanup) exits 77 before any docker call unless /var/folders/l3/6x2gqh2x1rbdkr21_hzckff40000gn/T/rwb-20261006t173718-c83372-1bb_mely/w/_state/docker-selection.json seals the validated endpoint and unchanged private config", "host_docker_daemon": "docker info must succeed; checkouts must be visible to the daemon (Docker Desktop file sharing covers the temp dir)", "owned_names": "every project/volume/image contains ownership token 20261006t173718-c83372", "private_state": "/var/folders/l3/6x2gqh2x1rbdkr21_hzckff40000gn/T/rwb-20261006t173718-c83372-1bb_mely/w/_state only; user tool state is never read or written"}, "version": "1.25.4"}`.

Frozen declared deviations: `["PostgreSQL 17.6 on Debian bookworm (DDEV customizes it with apt, so the derived image is not byte-frozen); web container ddev/ddev-webserver as chosen by DDEV"]`.

- `tool-version`: exit 0, `bench/results/final-20261006-reviewed-1/ddev/logs/0003-tool-version.stdout` and `bench/results/final-20261006-reviewed-1/ddev/logs/0003-tool-version.stderr`.

Realized tool-version receipt excerpt:

```text
ddev version v1.25.4
33314cdadac214c630023ca3f7a82a6d0b743814a7ebf56ea53bc6d776dfb5af ddev
{"level":"info","msg":" ITEM              VALUE                                                                                                       \n DDEV version      v1.25.4                                                                                                     \n architecture      arm64                                                                                                       \n cgo_enabled       0                                                                                                           \n db                ddev/ddev-dbserver-mariadb-11.8:v1.25.4                                                                     \n ddev-environment  darwin                                                                                                      \n ddev-ssh-agent    ddev/ddev-ssh-agent:v1.25.4                                                                                 \n docker            29.1.3                                                                                                      \n docker-api        1.52                               
```

Excerpt truncated. The linked raw receipt retains the full output.

### lando

A: Python 3.13.16, PostgreSQL 17.6, Redis 8.10.2; B: Python 3.13.16, PostgreSQL 17.6, Redis 8.10.2

Recorded pins: `{"asset_hash_source": "release sha256sum.txt", "assets": {"darwin-amd64": "16969dc627d1594a40ac62b8362f657b59309af1563a82057d58cc769bc70718", "darwin-arm64": "8dd9b6306a05816c3d8cdb2f4c4b9fd6d9df76490992fb0f2e331a894fd92b95", "linux-amd64": "7a868b71efffb1f8ecc5cc3377bcbeea9926618a082c5138ba19803b9bba5011", "linux-arm64": "5709cf237ccd7a23768b920b00e20cbf2067b184d2572f0945bb2ecf09be67a8"}, "entry_auto_resumes": false, "image_notes": "Debian images (Lando v3 service scripts need bash): python full bookworm, Bitnami PostgreSQL 17.6.0, Redis 8.10.2 trixie; Redis AOF appendfsync=everysec (plugin default)", "images": {"postgres": "bitnamilegacy/postgresql:17.6.0-debian-12-r4@sha256:926356130b77d5742d8ce605b258d35db9b62f2f8fd1601f9dbaef0c8a710a8d", "python": "python:3.13.16-bookworm@sha256:d79ba8693551488516799bfce4566d8cbcccb87c9d9dfaef5e3915f1451e1326", "redis": "redis:8.10.2-trixie@sha256:c94085d298b738be22c9ccdc0ac3761fa6649df7dd82ad1d42367f3cb9714935", "uv": "uv==0.12.23 wheel from PyPI, hash-locked in .lando/uv-requirements.txt"}, "orchestrator": "docker-compose 2.40.3", "orchestrator_assets": {"darwin-amd64": "53528ecff0182546d92d7cc3f50dc78f9b387c3da68b4a3fd0cf2c48dab77133", "darwin-arm64": "8cd7eb5f95bacb536cc407111662e2c205d67d9abfea5dcb8400be8418db60d1", "linux-amd64": "dba9d98e1ba5bfe11d88c99b9bd32fc4a0624a30fafe68eea34d61a3e42fd372", "linux-arm64": "d26373b19e89160546d15407516cc59f453030d9bc5b43ba7faf16f7b4980137"}, "plugins": ["@lando/python@1.4.3", "@lando/postgres@1.6.0", "@lando/redis@1.3.0"], "receipt_sha256": "56ed1793c3dbd4631f930b402aea0e9fae182c5335af483ff05b9b34d6fee0c4", "source_commit": "7a87f80576c5cdb5c7d616108bc9aff81150d463", "validity": {"autosetup": "setup.skipInstallCa, buildEngine/buildx/orchestrator/installPlugins=false; verified by check_config.py before start", "core_hooks_required": ["frozen_copy: add C to Scenario.started (setup/start create containers); host cleanup_host covers it until then"], "docker_client_config": "HOME=/var/folders/l3/6x2gqh2x1rbdkr21_hzckff40000gn/T/rwb-20261006t173725-3e750a-4x2mn4k9/w/_state/home (Lando strips DOCKER_*): its .docker holds the user's persisted current context only, no credsStore/auths (anonymous public pulls); preflight fails closed on a different daemon endpoint, on DOCKER_HOST/CONTEXT/TLS* env overrides, on TLS/SkipTLSVerify contexts, and unless the endpoint is the unix socket /var/run/docker.sock that Lando's engine uses; every later body (receipts, cleanup) exits 77 before any docker/lando call unless /var/folders/l3/6x2gqh2x1rbdkr21_hzckff40000gn/T/rwb-20261006t173725-3e750a-4x2mn4k9/w/_state/docker-selection.json seals that validated selection", "host_docker_daemon": "docker info must succeed; checkouts must be visible to the daemon (Docker Desktop file sharing covers the temp dir)", "owned_names": "every project/volume/image contains ownership token 20261006t1737253e750a", "private_home": "Lando's /user mount and .ssh key scan use /var/folders/l3/6x2gqh2x1rbdkr21_hzckff40000gn/T/rwb-20261006t173725-3e750a-4x2mn4k9/w/_state/home (holds private .docker metadata and the .ssh Lando creates), not the user's home: no host SSH key files in containers; SSH_AUTH_SOCK is still inherited, so agent access is not guaranteed off", "private_state": "/var/folders/l3/6x2gqh2x1rbdkr21_hzckff40000gn/T/rwb-20261006t173725-3e750a-4x2mn4k9/w/_state only; user tool state is never read or written"}, "version": "3.26.9"}`.

Frozen declared deviations: `["Debian images (Lando v3 service scripts need bash): python full bookworm, Bitnami PostgreSQL 17.6.0, Redis 8.10.2 trixie; Redis AOF appendfsync=everysec (plugin default)"]`.

- `a-tool-versions`: exit 0, `bench/results/final-20261006-reviewed-1/lando/logs/0011-a-tool-versions.stdout` and `bench/results/final-20261006-reviewed-1/lando/logs/0011-a-tool-versions.stderr`.
- `b-tool-versions`: exit 0, `bench/results/final-20261006-reviewed-1/lando/logs/0027-b-tool-versions.stdout` and `bench/results/final-20261006-reviewed-1/lando/logs/0027-b-tool-versions.stderr`.
- `c-tool-versions`: exit 0, `bench/results/final-20261006-reviewed-1/lando/logs/0098-c-tool-versions.stdout` and `bench/results/final-20261006-reviewed-1/lando/logs/0098-c-tool-versions.stderr`.
- `tool-version`: exit 0, `bench/results/final-20261006-reviewed-1/lando/logs/0005-tool-version.stdout` and `bench/results/final-20261006-reviewed-1/lando/logs/0005-tool-version.stderr`.

Realized tool-version receipt excerpt:

```text
v3.26.9
8dd9b6306a05816c3d8cdb2f4c4b9fd6d9df76490992fb0f2e331a894fd92b95 lando-macos-arm64-v3.26.9
lando setup skipInstallCa=true buildEngine=false buildx=false orchestrator=false installPlugins=false skipCommonPlugins=true
lando orchestratorBin /var/folders/l3/6x2gqh2x1rbdkr21_hzckff40000gn/T/rwb-20261006t173725-3e750a-4x2mn4k9/w/_state/lando/bin/docker-compose-v2.40.3
8cd7eb5f95bacb536cc407111662e2c205d67d9abfea5dcb8400be8418db60d1 docker-compose-v2.40.3
Docker Compose version v2.40.3

 Name                 Version 
 ──────────────────── ─────── 
 lando                v3.26.9 
 @lando/wordpress     v1.11.0 
 @lando/varnish       v1.3.2  
 @lando/tomcat        v1.3.0  
 @lando/symfony       v1.12.2 
 @lando/solr          v1.4.1  
 @lando/ruby          v1.6.0  
 @lando/redis         v1.3.0  
 @lando/python        v1.4.3  
 @lando/postgres      v1.6.0  
 @lando/phpmyadmin    v1.6.0  
 @lando/php           v1.12.0 
 @lando/pantheon      v1.13.0 
 @lando/node          v1.7.0  
 @lando/nginx         v1.6.0  
 @lando/mysql         v1.6.0  
 @lando/mssql         v1.4.3  
 @lando/mongo         v1.4.0  
 @lando/memcached     v1.4.2  
 @lando/mean          v1.5.0  
 @lando/mariadb       v1.
```

Excerpt truncated. The linked raw receipt retains the full output.

### process-compose

A: Python 3.13.15, PostgreSQL 17.11, Redis 8.10.2; B: Python 3.13.15, PostgreSQL 17.11, Redis 8.10.2

Recorded pins: `{"nixpkgs": "151fa4e8ddfdd8dd25d945ad94ed54a13de9f6e4", "postgresql": "17.11", "process_compose": "1.122.0", "process_compose_tgz_sha256": "52fa7d5a2d5e0db470faec5976204fc215ed7e3d13689e930cf522becfb63778", "python": "3.13.15", "redis": "8.10.2", "uv": "0.12.22"}`.

Frozen declared deviations: `[]`.

- `a-tool-versions`: exit 0, `bench/results/final-20261006-reviewed-1/process-compose/logs/0011-a-tool-versions.stdout` and `bench/results/final-20261006-reviewed-1/process-compose/logs/0011-a-tool-versions.stderr`.
- `b-tool-versions`: exit 0, `bench/results/final-20261006-reviewed-1/process-compose/logs/0026-b-tool-versions.stdout` and `bench/results/final-20261006-reviewed-1/process-compose/logs/0026-b-tool-versions.stderr`.
- `c-tool-versions`: exit 0, `bench/results/final-20261006-reviewed-1/process-compose/logs/0096-c-tool-versions.stdout` and `bench/results/final-20261006-reviewed-1/process-compose/logs/0096-c-tool-versions.stderr`.
- `tool-version`: exit 0, `bench/results/final-20261006-reviewed-1/process-compose/logs/0004-tool-version.stdout` and `bench/results/final-20261006-reviewed-1/process-compose/logs/0004-tool-version.stderr`.

Realized tool-version receipt excerpt:

```text
Process Compose
Version:        v1.122.0
Commit:         23b0aca
Date (UTC):     2026-08-17T22:59:01Z
License:        Apache-2.0
Discord:        https://discord.gg/S4xgmRSHdC
Author:         Eugene Berger
nix (Determinate Nix 3.23.0) 2.35.2
3f8f17edb599c94e805f465ac2b70efb676e334665aaca2cccff0b21e53910f7  /home/agent/.local/bin/process-compose

{"level":"debug","error":"could not locate `process-compose` in any of the following paths: [/home/agent/.config /etc/xdg]","time":"2026-10-06T17:39:31Z","message":"Path not found for process compose config home"}
{"level":"debug","error":"could not locate `process-compose` in any of the following paths: [/home/agent/.config /etc/xdg]","time":"2026-10-06T17:39:31Z","message":"Path not found for process compose config home"}
```

### services-flake

A: Python 3.13.15, PostgreSQL 17.11, Redis 8.10.2; B: Python 3.13.15, PostgreSQL 17.11, Redis 8.10.2

Recorded pins: `{"flake_parts": "024633cd702b10285db5cb19b40ad48d2399ba60", "nixpkgs": "151fa4e8ddfdd8dd25d945ad94ed54a13de9f6e4", "postgresql": "17.11", "process_compose": "1.122.0 (nixpkgs)", "process_compose_flake": "464ff6880737f063c3f0d3d2c7781fda9190868f", "python": "3.13.15", "redis": "8.10.2", "services_flake": "0ba7183cab54ffbd0be70cb95694f024701afd2b", "uv": "0.12.22"}`.

Frozen declared deviations: `[]`.

- `a-tool-versions`: exit 0, `bench/results/final-20261006-reviewed-1/services-flake/logs/0010-a-tool-versions.stdout` and `bench/results/final-20261006-reviewed-1/services-flake/logs/0010-a-tool-versions.stderr`.
- `b-tool-versions`: exit 0, `bench/results/final-20261006-reviewed-1/services-flake/logs/0025-b-tool-versions.stdout` and `bench/results/final-20261006-reviewed-1/services-flake/logs/0025-b-tool-versions.stderr`.
- `c-tool-versions`: exit 0, `bench/results/final-20261006-reviewed-1/services-flake/logs/0095-c-tool-versions.stdout` and `bench/results/final-20261006-reviewed-1/services-flake/logs/0095-c-tool-versions.stderr`.
- `tool-version`: exit 0, `bench/results/final-20261006-reviewed-1/services-flake/logs/0003-tool-version.stdout` and `bench/results/final-20261006-reviewed-1/services-flake/logs/0003-tool-version.stderr`.

Realized tool-version receipt excerpt:

```text
nix (Determinate Nix 3.23.0) 2.35.2
```

### pkgx

A: Python 3.13.15, PostgreSQL 17.2, Redis 8.10.0; B: Python 3.13.15, PostgreSQL 17.2, Redis 8.10.0

Recorded pins: `{"dev": "1.8.1", "pantry": "2df061bd184985428bc17aba4a8a8c1e2fd39781", "pkgx": "2.11.0", "pkgx_txz_sha256": "fb4b9c2beb7264027e0cf99d9b60464425c84005951a10f63b38529b763b40d4", "postgresql": "17.2", "python": "3.13.15", "redis": "8.10.0", "uv": "0.12.22"}`.

Frozen declared deviations: `[]`.

- `a-tool-versions`: exit 0, `bench/results/final-20261006-reviewed-1/pkgx/logs/0010-a-tool-versions.stdout` and `bench/results/final-20261006-reviewed-1/pkgx/logs/0010-a-tool-versions.stderr`.
- `b-tool-versions`: exit 0, `bench/results/final-20261006-reviewed-1/pkgx/logs/0024-b-tool-versions.stdout` and `bench/results/final-20261006-reviewed-1/pkgx/logs/0024-b-tool-versions.stderr`.
- `tool-version`: exit 0, `bench/results/final-20261006-reviewed-1/pkgx/logs/0005-tool-version.stdout` and `bench/results/final-20261006-reviewed-1/pkgx/logs/0005-tool-version.stderr`.

Realized tool-version receipt excerpt:

```text
pkgx 2.11.0
ec0bcce905f7e01d90c81a8a8e26956043fa1cee8fb8e231d74997f78273ac89  /home/agent/.local/bin/pkgx
2df061bd184985428bc17aba4a8a8c1e2fd39781
dev 1.8.1

Download https://registry.npmjs.org/@types%2fnode
Download https://registry.npmjs.org/undici-types
Download https://raw.githubusercontent.com/pkgxdev/libpkgx/refs/tags/v0.21.0/mod.ts
Download https://jsr.io/@std/cli/meta.json
Download https://jsr.io/@std/fs/meta.json
Download https://raw.githubusercontent.com/pkgxdev/libpkgx/refs/tags/v0.21.0/src/utils/misc.ts
Download https://raw.githubusercontent.com/pkgxdev/libpkgx/refs/tags/v0.21.0/src/utils/host.ts
Download https://raw.githubusercontent.com/pkgxdev/libpkgx/refs/tags/v0.21.0/src/utils/semver.ts
Download https://raw.githubusercontent.com/pkgxdev/libpkgx/refs/tags/v0.21.0/src/utils/Path.ts
Download https://raw.githubusercontent.com/pkgxdev/libpkgx/refs/tags/v0.21.0/src/types.ts
Download https://raw.githubusercontent.com/pkgxdev/libpkgx/refs/tags/v0.21.0/src/utils/pkg.ts
Download https://raw.githubusercontent.com/pkgxdev/libpkgx/refs/tags/v0.21.0/src/utils/error.ts
Download https://raw.githubusercontent.com/pkgxdev/libpkgx/refs/tags/v0.21.0/src/hooks/useConfig.ts
Download htt
```

Excerpt truncated. The linked raw receipt retains the full output.

### dnvr

A: Python 3.13.15, PostgreSQL 17.11, Redis 8.10.2; B: Python 3.13.15, PostgreSQL 17.11, Redis 8.10.2

Recorded pins: `{"dnvr": "a66c2bbabb67293812a5c39855ab0ecf6af21d41", "dnvr_own_nixpkgs": "062346a6d85bc4b49dfaa61c986e9c5be21217d1 (overridden by follows)", "nixpkgs": "151fa4e8ddfdd8dd25d945ad94ed54a13de9f6e4", "postgresql": "17.11", "python": "3.13.15", "redis": "8.10.2", "uv": "0.12.22"}`.

Frozen declared deviations: `[]`.

- `a-tool-versions`: exit 0, `bench/results/final-20261006-reviewed-1/dnvr-retry-1/logs/0009-a-tool-versions.stdout` and `bench/results/final-20261006-reviewed-1/dnvr-retry-1/logs/0009-a-tool-versions.stderr`.
- `b-tool-versions`: exit 0, `bench/results/final-20261006-reviewed-1/dnvr-retry-1/logs/0023-b-tool-versions.stdout` and `bench/results/final-20261006-reviewed-1/dnvr-retry-1/logs/0023-b-tool-versions.stderr`.
- `c-tool-versions`: exit 0, `bench/results/final-20261006-reviewed-1/dnvr-retry-1/logs/0092-c-tool-versions.stdout` and `bench/results/final-20261006-reviewed-1/dnvr-retry-1/logs/0092-c-tool-versions.stderr`.
- `tool-version`: exit 0, `bench/results/final-20261006-reviewed-1/dnvr-retry-1/logs/0003-tool-version.stdout` and `bench/results/final-20261006-reviewed-1/dnvr-retry-1/logs/0003-tool-version.stderr`.

Realized tool-version receipt excerpt:

```text
nix (Determinate Nix 3.23.0) 2.35.2
dnvr a66c2bbabb67293812a5c39855ab0ecf6af21d41
```

### guix

No runtime application identity was reached.

Recorded pins: `{"binary_sha256": "a5d58b1d0294cad6adb1f2aff627d37feb5db763fdffbceb8551f2b12123cf39", "binary_url": "https://mirrors.kernel.org/gnu/guix/guix-binary-1.5.0.aarch64-linux.tar.xz", "channel": "71d010188f039817c465985e46e185445fda6946", "daemon_flags": "none", "deviation": "PostgreSQL 16 and Redis 7 instead of 17 and 8", "guix_release": "1.5.0", "postgresql": "16.14", "python": "3.13.13", "redis": "7.2.6", "uv": "0.10.12"}`.

Frozen declared deviations: `["PostgreSQL 16 and Redis 7 instead of 17 and 8"]`.


### workz

A: Python 3.13.16, PostgreSQL 17.6, Redis 8.10.2; B: Python 3.13.16, PostgreSQL 17.6, Redis 8.10.2

Recorded pins: `{"docker_config_source": "/Users/utsavsharma/.docker", "host_env": {"DOCKER_CONFIG": "/private/var/folders/l3/6x2gqh2x1rbdkr21_hzckff40000gn/T/rwb-20261006t175943-16c496-1n9zdszu/w/home/.docker", "GIT_AUTHOR_EMAIL": "rwb@invalid", "GIT_AUTHOR_NAME": "rwb", "GIT_COMMITTER_EMAIL": "rwb@invalid", "GIT_COMMITTER_NAME": "rwb", "GIT_CONFIG_GLOBAL": "/private/var/folders/l3/6x2gqh2x1rbdkr21_hzckff40000gn/T/rwb-20261006t175943-16c496-1n9zdszu/w/home/.gitconfig", "GIT_CONFIG_NOSYSTEM": "1", "GIT_TERMINAL_PROMPT": "0", "HOME": "/private/var/folders/l3/6x2gqh2x1rbdkr21_hzckff40000gn/T/rwb-20261006t175943-16c496-1n9zdszu/w/home", "LANG": "en_US.UTF-8", "LOGNAME": "utsavsharma", "NO_COLOR": "1", "PATH": "/private/var/folders/l3/6x2gqh2x1rbdkr21_hzckff40000gn/T/rwb-20261006t175943-16c496-1n9zdszu/w/tools/bin:/private/var/folders/l3/6x2gqh2x1rbdkr21_hzckff40000gn/T/rwb-20261006t175943-16c496-1n9zdszu/w/tools/python/bin:/private/var/folders/l3/6x2gqh2x1rbdkr21_hzckff40000gn/T/rwb-20261006t175943-16c496-1n9zdszu/w/tools/hostbin:/usr/bin:/bin:/usr/sbin:/sbin", "RWB_RUN_ID": "20261006t175943-16c496", "SHELL": "/bin/zsh", "TMPDIR": "/var/folders/l3/6x2gqh2x1rbdkr21_hzckff40000gn/T/", "USER": "utsavsharma", "UV_CACHE_DIR": "/private/var/folders/l3/6x2gqh2x1rbdkr21_hzckff40000gn/T/rwb-20261006t175943-16c496-1n9zdszu/w/home/.cache/uv", "UV_PYTHON_DOWNLOADS": "never", "XDG_CACHE_HOME": "/private/var/folders/l3/6x2gqh2x1rbdkr21_hzckff40000gn/T/rwb-20261006t175943-16c496-1n9zdszu/w/home/.cache", "XDG_CONFIG_HOME": "/private/var/folders/l3/6x2gqh2x1rbdkr21_hzckff40000gn/T/rwb-20261006t175943-16c496-1n9zdszu/w/home/.config", "XDG_DATA_HOME": "/private/var/folders/l3/6x2gqh2x1rbdkr21_hzckff40000gn/T/rwb-20261006t175943-16c496-1n9zdszu/w/home/.local/share", "npm_config_cache": "/private/var/folders/l3/6x2gqh2x1rbdkr21_hzckff40000gn/T/rwb-20261006t175943-16c496-1n9zdszu/w/home/.npm", "npm_config_update_notifier": "false"}, "host_programs": {"docker": "/opt/homebrew/Cellar/docker/28.1.1/bin/docker", "docker-credential-desktop": "/Applications/Docker.app/Contents/Resources/bin/docker-credential-desktop", "git": "/opt/homebrew/Cellar/git/2.51.2/bin/git"}, "platform": "darwin-arm64 host", "postgres_image": "postgres:17.6-alpine@sha256:ef257d85f76e48da1c64832459b59fcaba1a4dac97bf5d7450c77753542eee94", "python": "3.13.16", "python_sha256": "9e01f63bbb08576cd9c8bc2d0564d098cb30c8453a0cd4bcf6aef458f6d2a147", "redis_image": "redis:8.10.2-alpine@sha256:3811787313eba226a2ef38658c6ccb91cd5e110edc89c37767de373120a0e5a0", "uv": "0.12.23", "uv_sha256": "50487ae565ccd96e499056b4674d438f4c53170202617b4c759defe0c6a1b544", "workz": "0.11.0", "workz_archive_sha256": "a0a203b6d4f76dd00198d101163c644529553cb7eb33c261fa4fa15ae405d195", "workz_binary_sha256": "72994c049c43989e4ec868dd3741389548f70aa15342acd34cd88349c5feef97", "workz_track": "published stable"}`.

Frozen declared deviations: `[]`.

- `a-tool-versions`: exit 0, `bench/results/final-20261006-reviewed-1/workz/logs/0009-a-tool-versions.stdout` and `bench/results/final-20261006-reviewed-1/workz/logs/0009-a-tool-versions.stderr`.
- `b-tool-versions`: exit 0, `bench/results/final-20261006-reviewed-1/workz/logs/0027-b-tool-versions.stdout` and `bench/results/final-20261006-reviewed-1/workz/logs/0027-b-tool-versions.stderr`.
- `c-tool-versions`: exit 0, `bench/results/final-20261006-reviewed-1/workz/logs/0100-c-tool-versions.stdout` and `bench/results/final-20261006-reviewed-1/workz/logs/0100-c-tool-versions.stderr`.
- `tool-version`: exit 0, `bench/results/final-20261006-reviewed-1/workz/logs/0003-tool-version.stdout` and `bench/results/final-20261006-reviewed-1/workz/logs/0003-tool-version.stderr`.

Realized tool-version receipt excerpt:

```text
workz 0.11.0
72994c049c43989e4ec868dd3741389548f70aa15342acd34cd88349c5feef97  /private/var/folders/l3/6x2gqh2x1rbdkr21_hzckff40000gn/T/rwb-20261006t175943-16c496-1n9zdszu/w/tools/bin/workz
uv 0.12.23 (46b84fd0b 2026-10-03 aarch64-apple-darwin)
Python 3.13.16 (main, Oct  3 2026, 00:54:36) [Clang 22.1.3 ]
2f7be1879e3337eef20875c0f80ef033337305d0405efc5d72642d6292f66b3b  /private/var/folders/l3/6x2gqh2x1rbdkr21_hzckff40000gn/T/rwb-20261006t175943-16c496-1n9zdszu/w/tools/bin/uv
git version 2.51.2
28.1.1 29.1.3
Docker Compose version v2.40.3-desktop.1
```

### worktrunk

A: Python 3.13.16, PostgreSQL 17.6, Redis 8.10.2; B: Python 3.13.16, PostgreSQL 17.6, Redis 8.10.2

Recorded pins: `{"docker_config_source": "/Users/utsavsharma/.docker", "host_env": {"DOCKER_CONFIG": "/private/var/folders/l3/6x2gqh2x1rbdkr21_hzckff40000gn/T/rwb-20261006t180026-572cdd-mzzx8y0h/w/home/.docker", "GIT_AUTHOR_EMAIL": "rwb@invalid", "GIT_AUTHOR_NAME": "rwb", "GIT_COMMITTER_EMAIL": "rwb@invalid", "GIT_COMMITTER_NAME": "rwb", "GIT_CONFIG_GLOBAL": "/private/var/folders/l3/6x2gqh2x1rbdkr21_hzckff40000gn/T/rwb-20261006t180026-572cdd-mzzx8y0h/w/home/.gitconfig", "GIT_CONFIG_NOSYSTEM": "1", "GIT_TERMINAL_PROMPT": "0", "HOME": "/private/var/folders/l3/6x2gqh2x1rbdkr21_hzckff40000gn/T/rwb-20261006t180026-572cdd-mzzx8y0h/w/home", "LANG": "en_US.UTF-8", "LOGNAME": "utsavsharma", "NO_COLOR": "1", "PATH": "/private/var/folders/l3/6x2gqh2x1rbdkr21_hzckff40000gn/T/rwb-20261006t180026-572cdd-mzzx8y0h/w/tools/bin:/private/var/folders/l3/6x2gqh2x1rbdkr21_hzckff40000gn/T/rwb-20261006t180026-572cdd-mzzx8y0h/w/tools/python/bin:/private/var/folders/l3/6x2gqh2x1rbdkr21_hzckff40000gn/T/rwb-20261006t180026-572cdd-mzzx8y0h/w/tools/hostbin:/usr/bin:/bin:/usr/sbin:/sbin", "RWB_RUN_ID": "20261006t180026-572cdd", "SHELL": "/bin/zsh", "TMPDIR": "/var/folders/l3/6x2gqh2x1rbdkr21_hzckff40000gn/T/", "USER": "utsavsharma", "UV_CACHE_DIR": "/private/var/folders/l3/6x2gqh2x1rbdkr21_hzckff40000gn/T/rwb-20261006t180026-572cdd-mzzx8y0h/w/home/.cache/uv", "UV_PYTHON_DOWNLOADS": "never", "XDG_CACHE_HOME": "/private/var/folders/l3/6x2gqh2x1rbdkr21_hzckff40000gn/T/rwb-20261006t180026-572cdd-mzzx8y0h/w/home/.cache", "XDG_CONFIG_HOME": "/private/var/folders/l3/6x2gqh2x1rbdkr21_hzckff40000gn/T/rwb-20261006t180026-572cdd-mzzx8y0h/w/home/.config", "XDG_DATA_HOME": "/private/var/folders/l3/6x2gqh2x1rbdkr21_hzckff40000gn/T/rwb-20261006t180026-572cdd-mzzx8y0h/w/home/.local/share", "npm_config_cache": "/private/var/folders/l3/6x2gqh2x1rbdkr21_hzckff40000gn/T/rwb-20261006t180026-572cdd-mzzx8y0h/w/home/.npm", "npm_config_update_notifier": "false"}, "host_programs": {"docker": "/opt/homebrew/Cellar/docker/28.1.1/bin/docker", "docker-credential-desktop": "/Applications/Docker.app/Contents/Resources/bin/docker-credential-desktop", "git": "/opt/homebrew/Cellar/git/2.51.2/bin/git"}, "platform": "darwin-arm64 host", "postgres_image": "postgres:17.6-alpine@sha256:ef257d85f76e48da1c64832459b59fcaba1a4dac97bf5d7450c77753542eee94", "python": "3.13.16", "python_sha256": "9e01f63bbb08576cd9c8bc2d0564d098cb30c8453a0cd4bcf6aef458f6d2a147", "redis_image": "redis:8.10.2-alpine@sha256:3811787313eba226a2ef38658c6ccb91cd5e110edc89c37767de373120a0e5a0", "uv": "0.12.23", "uv_sha256": "50487ae565ccd96e499056b4674d438f4c53170202617b4c759defe0c6a1b544", "worktrunk": "0.80.0", "worktrunk_archive_sha256": "8a2bb053c4bc80dea7d9ce6c221ff038d10a3d2dca2dc8f60d1b1a094fa783a9", "worktrunk_binary_sha256": "0708ca37fc39f9fa48edc1af500a2ff3664ec0155f63909425b02994f29f0fd1"}`.

Frozen declared deviations: `[]`.

- `a-tool-versions`: exit 0, `bench/results/final-20261006-reviewed-1/worktrunk/logs/0009-a-tool-versions.stdout` and `bench/results/final-20261006-reviewed-1/worktrunk/logs/0009-a-tool-versions.stderr`.
- `b-tool-versions`: exit 0, `bench/results/final-20261006-reviewed-1/worktrunk/logs/0027-b-tool-versions.stdout` and `bench/results/final-20261006-reviewed-1/worktrunk/logs/0027-b-tool-versions.stderr`.
- `c-tool-versions`: exit 0, `bench/results/final-20261006-reviewed-1/worktrunk/logs/0100-c-tool-versions.stdout` and `bench/results/final-20261006-reviewed-1/worktrunk/logs/0100-c-tool-versions.stderr`.
- `tool-version`: exit 0, `bench/results/final-20261006-reviewed-1/worktrunk/logs/0003-tool-version.stdout` and `bench/results/final-20261006-reviewed-1/worktrunk/logs/0003-tool-version.stderr`.

Realized tool-version receipt excerpt:

```text
wt v0.80.0
0708ca37fc39f9fa48edc1af500a2ff3664ec0155f63909425b02994f29f0fd1  /private/var/folders/l3/6x2gqh2x1rbdkr21_hzckff40000gn/T/rwb-20261006t180026-572cdd-mzzx8y0h/w/tools/bin/wt
uv 0.12.23 (46b84fd0b 2026-10-03 aarch64-apple-darwin)
Python 3.13.16 (main, Oct  3 2026, 00:54:36) [Clang 22.1.3 ]
2f7be1879e3337eef20875c0f80ef033337305d0405efc5d72642d6292f66b3b  /private/var/folders/l3/6x2gqh2x1rbdkr21_hzckff40000gn/T/rwb-20261006t180026-572cdd-mzzx8y0h/w/tools/bin/uv
git version 2.51.2
28.1.1 29.1.3
Docker Compose version v2.40.3-desktop.1
```

### git-grove

A: Python 3.13.16, PostgreSQL 17.6, Redis 8.10.2; B: Python 3.13.16, PostgreSQL 17.6, Redis 8.10.2

Recorded pins: `{"docker_config_source": "/Users/utsavsharma/.docker", "git_grove": "0.1.0-alpha.1.8", "git_grove_cli_sha256": "b8cd71e6d268fc6578da93d8da293728085900e80978dd316e5e53654a426b12", "git_grove_integrity": "sha512-szaFvkSzi+a8JK8R1Ui+Yvv9u/6xnAAEw1Kvk61z+qRtOr+zlZD57Nr6yLzc+R21pZOhYm3X2WyD00TEaGC0mQ==", "git_grove_track": "npm published", "host_env": {"DOCKER_CONFIG": "/private/var/folders/l3/6x2gqh2x1rbdkr21_hzckff40000gn/T/rwb-20261006t180109-89ad03-iwxi5toj/w/home/.docker", "GIT_AUTHOR_EMAIL": "rwb@invalid", "GIT_AUTHOR_NAME": "rwb", "GIT_COMMITTER_EMAIL": "rwb@invalid", "GIT_COMMITTER_NAME": "rwb", "GIT_CONFIG_GLOBAL": "/private/var/folders/l3/6x2gqh2x1rbdkr21_hzckff40000gn/T/rwb-20261006t180109-89ad03-iwxi5toj/w/home/.gitconfig", "GIT_CONFIG_NOSYSTEM": "1", "GIT_TERMINAL_PROMPT": "0", "GROVE_WORKTREE_ROOT": "/private/var/folders/l3/6x2gqh2x1rbdkr21_hzckff40000gn/T/rwb-20261006t180109-89ad03-iwxi5toj/w/trees", "HOME": "/private/var/folders/l3/6x2gqh2x1rbdkr21_hzckff40000gn/T/rwb-20261006t180109-89ad03-iwxi5toj/w/home", "LANG": "en_US.UTF-8", "LOGNAME": "utsavsharma", "NO_COLOR": "1", "PATH": "/private/var/folders/l3/6x2gqh2x1rbdkr21_hzckff40000gn/T/rwb-20261006t180109-89ad03-iwxi5toj/w/tools/bin:/private/var/folders/l3/6x2gqh2x1rbdkr21_hzckff40000gn/T/rwb-20261006t180109-89ad03-iwxi5toj/w/tools/python/bin:/private/var/folders/l3/6x2gqh2x1rbdkr21_hzckff40000gn/T/rwb-20261006t180109-89ad03-iwxi5toj/w/tools/hostbin:/usr/bin:/bin:/usr/sbin:/sbin", "RWB_RUN_ID": "20261006t180109-89ad03", "SHELL": "/bin/zsh", "TMPDIR": "/var/folders/l3/6x2gqh2x1rbdkr21_hzckff40000gn/T/", "USER": "utsavsharma", "UV_CACHE_DIR": "/private/var/folders/l3/6x2gqh2x1rbdkr21_hzckff40000gn/T/rwb-20261006t180109-89ad03-iwxi5toj/w/home/.cache/uv", "UV_PYTHON_DOWNLOADS": "never", "XDG_CACHE_HOME": "/private/var/folders/l3/6x2gqh2x1rbdkr21_hzckff40000gn/T/rwb-20261006t180109-89ad03-iwxi5toj/w/home/.cache", "XDG_CONFIG_HOME": "/private/var/folders/l3/6x2gqh2x1rbdkr21_hzckff40000gn/T/rwb-20261006t180109-89ad03-iwxi5toj/w/home/.config", "XDG_DATA_HOME": "/private/var/folders/l3/6x2gqh2x1rbdkr21_hzckff40000gn/T/rwb-20261006t180109-89ad03-iwxi5toj/w/home/.local/share", "npm_config_cache": "/private/var/folders/l3/6x2gqh2x1rbdkr21_hzckff40000gn/T/rwb-20261006t180109-89ad03-iwxi5toj/w/home/.npm", "npm_config_update_notifier": "false"}, "host_programs": {"docker": "/opt/homebrew/Cellar/docker/28.1.1/bin/docker", "docker-credential-desktop": "/Applications/Docker.app/Contents/Resources/bin/docker-credential-desktop", "git": "/opt/homebrew/Cellar/git/2.51.2/bin/git", "node": "/opt/homebrew/Cellar/node@24/24.16.0/bin/node", "npm": "/opt/homebrew/Cellar/node@24/24.16.0/lib/node_modules/npm/bin/npm-cli.js"}, "platform": "darwin-arm64 host", "postgres_image": "postgres:17.6-alpine@sha256:ef257d85f76e48da1c64832459b59fcaba1a4dac97bf5d7450c77753542eee94", "python": "3.13.16", "python_sha256": "9e01f63bbb08576cd9c8bc2d0564d098cb30c8453a0cd4bcf6aef458f6d2a147", "redis_image": "redis:8.10.2-alpine@sha256:3811787313eba226a2ef38658c6ccb91cd5e110edc89c37767de373120a0e5a0", "uv": "0.12.23", "uv_sha256": "50487ae565ccd96e499056b4674d438f4c53170202617b4c759defe0c6a1b544"}`.

Frozen declared deviations: `[]`.

- `a-tool-versions`: exit 0, `bench/results/final-20261006-reviewed-1/git-grove/logs/0009-a-tool-versions.stdout` and `bench/results/final-20261006-reviewed-1/git-grove/logs/0009-a-tool-versions.stderr`.
- `b-tool-versions`: exit 0, `bench/results/final-20261006-reviewed-1/git-grove/logs/0025-b-tool-versions.stdout` and `bench/results/final-20261006-reviewed-1/git-grove/logs/0025-b-tool-versions.stderr`.
- `c-tool-versions`: exit 0, `bench/results/final-20261006-reviewed-1/git-grove/logs/0096-c-tool-versions.stdout` and `bench/results/final-20261006-reviewed-1/git-grove/logs/0096-c-tool-versions.stderr`.
- `tool-version`: exit 0, `bench/results/final-20261006-reviewed-1/git-grove/logs/0003-tool-version.stdout` and `bench/results/final-20261006-reviewed-1/git-grove/logs/0003-tool-version.stderr`.

Realized tool-version receipt excerpt:

```text
v0.1.0-alpha.1.8
v24.16.0
b8cd71e6d268fc6578da93d8da293728085900e80978dd316e5e53654a426b12  /private/var/folders/l3/6x2gqh2x1rbdkr21_hzckff40000gn/T/rwb-20261006t180109-89ad03-iwxi5toj/w/tools/grove/node_modules/@gitgrove/cli/dist/cli.js
uv 0.12.23 (46b84fd0b 2026-10-03 aarch64-apple-darwin)
Python 3.13.16 (main, Oct  3 2026, 00:54:36) [Clang 22.1.3 ]
2f7be1879e3337eef20875c0f80ef033337305d0405efc5d72642d6292f66b3b  /private/var/folders/l3/6x2gqh2x1rbdkr21_hzckff40000gn/T/rwb-20261006t180109-89ad03-iwxi5toj/w/tools/bin/uv
git version 2.51.2
28.1.1 29.1.3
Docker Compose version v2.40.3-desktop.1
```

### isola

A: Python 3.13.16, PostgreSQL 17.11, Redis 8.10.2; B: Python 3.13.16, PostgreSQL 17.11, Redis 8.10.2

Recorded pins: `{"isola": "0.4.1", "isola_archive_sha256": "c2494bcf45405d74a79e6e74d7283e106b77dbbb41477955c3776b95d4fea92d", "isola_commit": "af852ae57c6d107e09daaacf8d744cc09e18fbd0", "isola_toml_sha256": "2869341b26b12117c88d02f4d2b551c3fca5075d5aac94bc4722d972d7e8d439", "mise": "2026.10.3", "project": "rwb-isola-20261006t18022337caf7", "shared": {"pg_port": 25440, "redis_port": 26390}, "toolchain": {"postgres": "17.11", "python": "3.13.16", "redis": "8.10.2", "uv": "0.12.23"}}`.

Frozen declared deviations: `[]`.

- `a-tool-versions`: exit 0, `bench/results/final-20261006-reviewed-1/isola/logs/0010-a-tool-versions.stdout` and `bench/results/final-20261006-reviewed-1/isola/logs/0010-a-tool-versions.stderr`.
- `b-tool-versions`: exit 0, `bench/results/final-20261006-reviewed-1/isola/logs/0026-b-tool-versions.stdout` and `bench/results/final-20261006-reviewed-1/isola/logs/0026-b-tool-versions.stderr`.
- `tool-version`: exit 0, `bench/results/final-20261006-reviewed-1/isola/logs/0005-tool-version.stdout` and `bench/results/final-20261006-reviewed-1/isola/logs/0005-tool-version.stderr`.

Realized tool-version receipt excerpt:

```text
isola 0.4.1 (commit: af852ae57c6d107e09daaacf8d744cc09e18fbd0, built: 2026-09-09T09:06:03Z)
2026.10.3 linux-arm64 (2026-10-05)
fa0588f916f915f0bb62bcb0eba05ab965adba35219400b519d7c9c0bff60423  /home/agent/.local/bin/isola
357260e28904569a6e7124d33b65cef6d043846c07bb8f4906ddf22cc353d61d  /home/agent/.local/bin/mise
```

### berth

A: Python 3.13.16, PostgreSQL 17.11, Redis 8.10.2; B: Python 3.13.16, PostgreSQL 17.11, Redis 8.10.2

Recorded pins: `{"berth": "0.1.0", "berth_commit": "3b93287584dcc5c7c26298a62379d8ce001400ff", "images": {"postgres": "postgres:17.11-alpine@sha256:b0f9560a2de083e2cc7382e75f808c7381a32852a7ec49117deedb300e552b24", "python": "python:3.13.16-slim-bookworm@sha256:a1165e272e578941b84abc79e4ab38a0305cd12803a5c4247979ac7655f4d641", "redis": "redis:8.10.2-alpine@sha256:3811787313eba226a2ef38658c6ccb91cd5e110edc89c37767de373120a0e5a0", "uv": "ghcr.io/astral-sh/uv:0.12.23@sha256:61d393e44e249f2e4b526b6c7ddcecce245946826e608e11c93ad4f5bba55b21"}, "runtime": {"postgres": "17.11", "python": "3.13.16", "redis": "8.10.2", "uv": "0.12.23"}}`.

Frozen declared deviations: `[]`.

- `a-tool-versions`: exit 0, `bench/results/final-20261006-reviewed-1/berth/logs/0007-a-tool-versions.stdout` and `bench/results/final-20261006-reviewed-1/berth/logs/0007-a-tool-versions.stderr`.
- `b-tool-versions`: exit 0, `bench/results/final-20261006-reviewed-1/berth/logs/0023-b-tool-versions.stdout` and `bench/results/final-20261006-reviewed-1/berth/logs/0023-b-tool-versions.stderr`.
- `tool-version`: exit 0, `bench/results/final-20261006-reviewed-1/berth/logs/0002-tool-version.stdout` and `bench/results/final-20261006-reviewed-1/berth/logs/0002-tool-version.stderr`.

Realized tool-version receipt excerpt:

```text
berth 0.1.0
e5a93ac0fa934ca47b3b93a3e01b4a96cbdc5850efde33ff6f57352dc10ab781  /var/folders/l3/6x2gqh2x1rbdkr21_hzckff40000gn/T/rwb-20261006t180256-9d10d9-kcxbbk__/tools/bin/berth
29.1.3 linux/arm64
Docker Compose version v2.40.3-desktop.1
git version 2.51.2
```

### branchbox

A: Python 3.13.16, PostgreSQL 17.11, Redis 8.10.2; B: Python 3.13.16, PostgreSQL 17.11, Redis 8.10.2

Recorded pins: `{"archives": {"arm64": "3446e7462b9a42724034695e3cb3163d263aa792a3d97acac95d155aa1c22392", "x86_64": "9d5550c0763a1e44940eaca8efd379279a36bdde3944ebec0e416b545d310ce8"}, "branchbox": "0.13.4", "branchbox_commit": "a00b3ee9a0acce000ee8f478dc0d6a8a24b3c7df", "images": {"postgres": "postgres:17.11-alpine@sha256:b0f9560a2de083e2cc7382e75f808c7381a32852a7ec49117deedb300e552b24", "python": "python:3.13.16-slim-bookworm@sha256:a1165e272e578941b84abc79e4ab38a0305cd12803a5c4247979ac7655f4d641", "redis": "redis:8.10.2-alpine@sha256:3811787313eba226a2ef38658c6ccb91cd5e110edc89c37767de373120a0e5a0", "uv": "ghcr.io/astral-sh/uv:0.12.23@sha256:61d393e44e249f2e4b526b6c7ddcecce245946826e608e11c93ad4f5bba55b21"}, "runtime": {"postgres": "17.11", "python": "3.13.16", "redis": "8.10.2", "uv": "0.12.23"}}`.

Frozen declared deviations: `[]`.

- `a-tool-versions`: exit 0, `bench/results/final-20261006-reviewed-1/branchbox/logs/0007-a-tool-versions.stdout` and `bench/results/final-20261006-reviewed-1/branchbox/logs/0007-a-tool-versions.stderr`.
- `b-tool-versions`: exit 0, `bench/results/final-20261006-reviewed-1/branchbox/logs/0023-b-tool-versions.stdout` and `bench/results/final-20261006-reviewed-1/branchbox/logs/0023-b-tool-versions.stderr`.
- `tool-version`: exit 0, `bench/results/final-20261006-reviewed-1/branchbox/logs/0002-tool-version.stdout` and `bench/results/final-20261006-reviewed-1/branchbox/logs/0002-tool-version.stderr`.

Realized tool-version receipt excerpt:

```text
branchbox 0.13.4
a8597b6a072e2490452ec5284a37f4255153886fd11ddffd833a34aa991b4968  /var/folders/l3/6x2gqh2x1rbdkr21_hzckff40000gn/T/rwb-20261006t180409-b89259-5_v2cbxn/tools/bin/branchbox
29.1.3 linux/arm64
Docker Compose version v2.40.3-desktop.1
git version 2.51.2
```

### tilt

A: Python 3.13.16, PostgreSQL 17.6, Redis 8.10.2; B: Python 3.13.16, PostgreSQL 17.6, Redis 8.10.2

Recorded pins: `{"docker_compose": "5.6.0", "docker_compose_sha256": "bd714a42b46e51757fc1121085b3096b9064e3f80d3ca5a991e33f44df4f43c9", "images": {"postgres": "postgres:17.6-alpine@sha256:ef257d85f76e48da1c64832459b59fcaba1a4dac97bf5d7450c77753542eee94", "python": "python:3.13.16-slim-bookworm@sha256:a1165e272e578941b84abc79e4ab38a0305cd12803a5c4247979ac7655f4d641", "redis": "redis:8.10.2-alpine@sha256:3811787313eba226a2ef38658c6ccb91cd5e110edc89c37767de373120a0e5a0", "uv": "ghcr.io/astral-sh/uv:0.12.23@sha256:61d393e44e249f2e4b526b6c7ddcecce245946826e608e11c93ad4f5bba55b21"}, "tilt": "0.37.8", "tilt_archive_sha256": "2d396b13c479f74deb19cb2161a3f0858f6e03f26f15f8d3b3be982d09ff4484", "tilt_sha256": "190255a6e64023b4cfe7a2bbecb34b41113d8c5db7d74f74555b8827e38cfb79", "uv_lock": "fixtures/app/uv.lock (uv sync --frozen)"}`.

Frozen declared deviations: `[]`.

- `a-tool-versions`: exit 0, `bench/results/final-20261006-reviewed-1/tilt/logs/0008-a-tool-versions.stdout` and `bench/results/final-20261006-reviewed-1/tilt/logs/0008-a-tool-versions.stderr`.
- `b-tool-versions`: exit 0, `bench/results/final-20261006-reviewed-1/tilt/logs/0025-b-tool-versions.stdout` and `bench/results/final-20261006-reviewed-1/tilt/logs/0025-b-tool-versions.stderr`.
- `tool-version`: exit 0, `bench/results/final-20261006-reviewed-1/tilt/logs/0002-tool-version.stdout` and `bench/results/final-20261006-reviewed-1/tilt/logs/0002-tool-version.stderr`.

Realized tool-version receipt excerpt:

```text
v0.37.8, built 2026-10-01
Docker Compose version v5.6.0
docker 28.1.1 / daemon 29.1.3 linux/arm64
190255a6e64023b4cfe7a2bbecb34b41113d8c5db7d74f74555b8827e38cfb79  /var/folders/l3/6x2gqh2x1rbdkr21_hzckff40000gn/T/rwb-20261006t180504-a16963-q60omb7e/tools/tilt
bd714a42b46e51757fc1121085b3096b9064e3f80d3ca5a991e33f44df4f43c9  /var/folders/l3/6x2gqh2x1rbdkr21_hzckff40000gn/T/rwb-20261006t180504-a16963-q60omb7e/tools/docker-compose
```

### organist

A: Python 3.13.15, PostgreSQL 17.11, Redis 8.10.2; B: Python 3.13.15, PostgreSQL 17.11, Redis 8.10.2

Recorded pins: `{"nix_config": "lazy-trees = false", "nixpkgs": "github:NixOS/nixpkgs/151fa4e8ddfdd8dd25d945ad94ed54a13de9f6e4", "organist": "github:nickel-lang/organist/a7e4e638cade5e7c4f36a129b80d91bf3538088e (main; no current release)", "runner": "honcho (from organist's services module)"}`.

Frozen declared deviations: `[]`.

- `a-tool-versions`: exit 0, `bench/results/final-20261006-reviewed-1/organist/logs/0010-a-tool-versions.stdout` and `bench/results/final-20261006-reviewed-1/organist/logs/0010-a-tool-versions.stderr`.
- `b-tool-versions`: exit 0, `bench/results/final-20261006-reviewed-1/organist/logs/0024-b-tool-versions.stdout` and `bench/results/final-20261006-reviewed-1/organist/logs/0024-b-tool-versions.stderr`.
- `c-tool-versions`: exit 0, `bench/results/final-20261006-reviewed-1/organist/logs/0093-c-tool-versions.stdout` and `bench/results/final-20261006-reviewed-1/organist/logs/0093-c-tool-versions.stderr`.
- `tool-version`: exit 0, `bench/results/final-20261006-reviewed-1/organist/logs/0004-tool-version.stdout` and `bench/results/final-20261006-reviewed-1/organist/logs/0004-tool-version.stderr`.

Realized tool-version receipt excerpt:

```text
nix (Determinate Nix 3.23.0) 2.35.2
git version 2.43.0
organist=a7e4e638cade5e7c4f36a129b80d91bf3538088e nixpkgs=151fa4e8ddfdd8dd25d945ad94ed54a13de9f6e4
```

### vagrant

A: Python 3.13.16, PostgreSQL 17.6, Redis 8.10.2; B: Python 3.13.16, PostgreSQL 17.6, Redis 8.10.2

Recorded pins: `{"images": "adapters/vagrant/images.lock.json", "provider": "docker (force_host_vm=false)", "uv_lock": "fixtures/app/uv.lock (uv sync --frozen)", "vagrant": "2.4.9", "vagrant_dmg_sha256": "8de08bd435ef8ae0fc5fbd6acefa9c68e62fb898c5ae0fbdacd26853bea9d4d6", "vagrant_launcher_sha256": "102bbe8336c246c3a647c2374d5ed9ecad42c8cd7fb886ee7f8aec4d14290d41"}`.

Frozen declared deviations: `[]`.

- `a-tool-versions`: exit 0, `bench/results/final-20261006-reviewed-1/vagrant/logs/0007-a-tool-versions.stdout` and `bench/results/final-20261006-reviewed-1/vagrant/logs/0007-a-tool-versions.stderr`.
- `b-tool-versions`: exit 0, `bench/results/final-20261006-reviewed-1/vagrant/logs/0023-b-tool-versions.stdout` and `bench/results/final-20261006-reviewed-1/vagrant/logs/0023-b-tool-versions.stderr`.
- `tool-version`: exit 0, `bench/results/final-20261006-reviewed-1/vagrant/logs/0002-tool-version.stdout` and `bench/results/final-20261006-reviewed-1/vagrant/logs/0002-tool-version.stderr`.

Realized tool-version receipt excerpt:

```text
Vagrant 2.4.9
ruby 3.3.8 (2025-04-09 revision b200bad6cd) [arm64-darwin]
docker 28.1.1 / daemon 29.1.3 linux/arm64
102bbe8336c246c3a647c2374d5ed9ecad42c8cd7fb886ee7f8aec4d14290d41  /var/folders/l3/6x2gqh2x1rbdkr21_hzckff40000gn/T/rwb-20261006t180848-312625-uk4qcz6d/tools/vagrant/bin/vagrant
```

Actual versions and recipe deviations remain lane-specific. In particular pkgx realizes PostgreSQL 17.2 and Redis 8.10.0, Devbox PostgreSQL 17.10, many Compose/worktree lanes PostgreSQL 17.6, and Python patches/uv versions differ. Guix has only prerequisite/provisioning receipts, not a realized workload. Organist default Nix sandbox fallback is observed in raw setup output; no explicit sandbox-disable option was supplied. DNVR source pin is not a tagged CLI version. All complete version text, identities, URL/server-port mappings, source tokens and paths are retained in `evidence/resume-independent-result-evidence.json`.

## Final verification and cleanup

Final receipts contain 2740 steps and 5480 raw stdout/stderr references across 26 final attempts. Independent reparse contains 480 entry samples and 480 application-read samples, plus 72 and 72 excluded warmups. Blocked/unreached DDEV and Guix lanes contribute no repeat samples. All realized A/B source tokens match their preparation/setup write receipts and adapter-declared app paths. Isolation keys and source tokens differ between A/B. All warmup/sample receipts are present and successful where measured; every application read returns keeper-a. First-task receipts and distributions were reparsed under report.py without promoting numbers.

All 27 baseline container IDs, names, images, states, start timestamps and restart counts match. Original network IDs/metadata and volume names/metadata match. There are no new surviving containers, networks or volumes, owned run-token host processes, owned tempdirs or run-tag images. Full per-lane after snapshots and comparisons remain under evidence/, including the original first 15 and resume-* snapshots for the new 11. Exact final snapshot: `evidence/resume-final-validation.json`; comparison: `evidence/resume-final-validation-comparison.json`. The benchmark executor has exited before handoff, as separately checked after the validation helpers.

No archived PID, unrelated container, network, volume, image/cache, global service or configuration was a cleanup target. The only manual removal during resumption was the freshly confirmed interrupted dnvr container. Harness cleanup receipts are unchanged.

## Pending parent/Astra decisions

1. Supply actual session and per-attempt result-review references only after review, then decide reviewed promotion. No approval is claimed here.
2. Review and accept the interruption supplement and ownership-bound nonoverlap evidence. The interrupted dnvr raw directory remains excluded and intentionally lacks a complete reporter descriptor.
3. Keep DDEV as an adapter block and Guix as an environment block. No normal startup timings exist for either lane.
4. Preserve occupied-port failures for Stack, Process Compose and services-flake, plus visible bad-configuration prerequisite blocks. Decide metric eligibility using the declared dependencies rather than treating unrelated checks as passes.
5. Confirm the declared transport, warmed caches, Nix sandbox fallback and differing runtime versions are acceptable for the intended descriptive report. There is no rank, universal cold-cache, confidence or native-macOS claim.

Primary handoff files: `bench/measurements/final-20261006-reviewed-1/manifest.json`, `resume-plan-20261006.json`, `resume-plan-20261006-receipt.json`, `evidence/resume-final-integrity.json`, `evidence/resume-independent-result-evidence.json`, `evidence/resume-pending-review-report-verification.json`, `evidence/resume-all26-attempt-descriptors.json`, and all raw directories under `bench/results/final-20261006-reviewed-1/`. Raw evidence remains ignored for the parent to bundle.
