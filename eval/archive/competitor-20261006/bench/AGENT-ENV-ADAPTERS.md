# Agent-environment adapters: isola, Berth, BranchBox

Owner: agent-env implementation agent (Opus 5.5). Research: `research/{isola,berth,branchbox}.md`.
Contract: `ADAPTER-CONTRACT.md` (v1). No service, lifecycle or timing run has been executed
for these lanes. Only offline tests, dry runs and `--help`/`--version` checks of the
downloaded release binaries have run. The first real run is the parent's serialized run.

## Files

| Path | Purpose |
|---|---|
| `rwb/adapters/agent_env_common.py` | Canonical pins, private host env, git helpers, Compose ownership/receipt helpers, `ContainerApp` mixin |
| `rwb/adapters/isola.py`, `adapters/isola/` | `isola.toml` template, `gitignore`, `toolchain.toml` (mise), `shared-servers.sh`, `receipt.py` |
| `rwb/adapters/berth.py`, `adapters/berth/` | `berth.yml`, `compose.yaml`, `Dockerfile`, `gitignore` |
| `rwb/adapters/branchbox.py`, `adapters/branchbox/` | `devcontainer/{devcontainer.json,Dockerfile,compose.yaml}`, `gitignore` |
| `tests/test_agent_env_adapters.py` | 29 offline tests: declarations, fake scenario, `bash -n` of every body, scope/safety, pin sync, receipt scripts |

Registry keys `isola`, `berth` and `branchbox` and the class names were already in `registry.py`. No core file was edited.

## Lanes at a glance

| | isola | Berth | BranchBox |
|---|---|---|---|
| Version measured | release v0.4.1 (`af852ae`), linux-arm64 archive sha256-checked | main `3b93287` (no releases/tags), built `cargo build --locked` | release v0.13.4 (`a00b3ee`), macOS archive sha256-checked |
| Transport | `docker` (`ev-base`, one run-owned container) | `host` (drives host Docker) | `host` (drives host Docker) |
| Boundary | `database` | `container` | `container` |
| App entry | generated `.env.isola` loaded by `uv run --no-project --env-file` (isola has no exec) | `docker compose -f <root>/compose.yaml --env-file <root>/.berth/NAME.env -p berth-NAME exec -T app` (delegated, Berth has no exec) | native `branchbox devcontainer exec --workspace-folder WS --` |
| Worktree creation | `git init` (A) and `git worktree add` (B, D, E), in `prepare` | `berth up` (in `setup`) | `branchbox feature start --runtime container` (config only, in `prepare`) |
| setup | mise installs the benchmark toolchain (cold in A) | `berth up` = worktree + ports + env + image build + `compose up -d` | `branchbox devcontainer build` |
| start / repeat start | `shared-servers.sh ensure` then `isola up` | `berth start` (`compose up -d`) | `branchbox devcontainer up` |
| stop | `isola down` (keeper stops; database and logical DB kept) | `berth stop` (`compose stop`; containers and volumes kept) | `branchbox devcontainer down` (containers removed, volumes kept) |
| final teardown | `isola destroy` + verified drop, then owned shared servers stopped | `berth down` (`compose down -v` + worktree + branch) | `devcontainer down --volumes --remove-orphans` |
| status | `isola ls --json`, `isola accessory ls --json` (native JSON) | `berth ls` (text) + `compose ps --format json` | none (`status` unsupported) |
| Runtime | mise: Python 3.13.16, uv 0.12.23, PostgreSQL 17.11, Redis 8.10.2 | digest-pinned `python:3.13.16-slim-bookworm`, `uv:0.12.23`, `postgres:17.11-alpine`, `redis:8.10.2-alpine` | same images and digests as Berth |

Shared canonical fixture: `fixtures/app` unchanged, its `uv.lock`, Python `>=3.13,<3.14`.
`CANONICAL` and `IMAGES` in `agent_env_common.py` are tested against `adapters/stack/stack.toml`
and every Dockerfile/compose file. BranchBox research proposed Python 3.13.7 and PostgreSQL
17.6. Those were illustrative, and both lanes use the canonical versions.

## Native versus scripted

isola
- Native: per-worktree database cloned from `rwb_template` (`CREATE DATABASE … TEMPLATE`), per-worktree logical Redis DB claimed with `__isola_owner__ = <project>:<branch>`, generated env file, keeper process lifecycle, JSON status, `destroy`.
- Scripted: the shared PostgreSQL cluster and Redis server (`shared-servers.sh`, private to the run's container, ports 25440/26390, owner receipt `RWB_SHARED/OWNER`, appendonly yes / appendfsync always). Also the toolchain via pinned mise, and the keeper service (`python3 -c 'signal.pause()' rwb-isola-keeper`), which exists only because isola requires one service. The keeper is overhead, not an application server.
- `receipt.py identity` cross-checks `isola accessory ls --json` against the generated env file (database name, Redis DB index) and the owner marker in that logical DB. `receipt.py stopped` checks that the keeper is gone and the accessories are retained. `receipt.py destroy` runs `isola destroy` and proves the database and logical DB are gone.
- Same PG `system_identifier` and Redis `run_id` across A and B are expected. Isolation is the (cluster, database) and (run_id, db index) pairs plus the app's unprefixed `checkouts`/`rwb:checkout` markers. Cleanup touches only databases and logical DBs recorded by isola for this run's project, on servers this run created.

Berth
- Native: worktree/branch per name, host port allocation into `.berth/NAME.env`, Compose project `berth-NAME`, stop/start keeping volumes, `down` destroying them.
- Application wiring (checked in): Berth runs Compose from the ROOT repo, so `compose.yaml` points build context and bind mount at `../app-${BERTH_NAME}`. The instance receipt fails unless the app container's `/workspace` bind source is this worktree and the host/container code digests match. The source token is checked by core.
- D (bad config) uses its own disposable repo (`w/d-repo/app`), because Compose reads the root repo's file.

BranchBox
- Native: `feature start` worktree/config, `devcontainer build/up/exec/down` (native runtime drives Docker/Compose, project = workspace basename). No `@devcontainers/cli` is needed.
- Release quirks handled: `up` ignores creation-hook failures, so the locked venv is baked into the image and `deps` runs `uv sync --frozen` through `devcontainer exec`, which propagates exit codes. `feature exec` (runs on the host) is never used. `feature start` receipt `worktree_path` must equal the expected checkout.
- The `../..:/workspaces` mount (BranchBox's own layout) makes sibling checkouts visible. Each command still runs its own checkout's code (token + digest). No filesystem-isolation claim is made.

## Scenario mapping and expected non-pass cells

- `lock.created`, `lock.frozen_copy`: `unsupported` for all three. None has a tool or package lock. All share the fixture `uv.lock`, and images are digest-pinned configuration.
- `occupied_port`: `not_applicable` for all three, with reasons in `not_applicable`. isola doesn't own the shared ports. Berth binds an ephemeral free port inside `up`, so the only possible conflict is a race. BranchBox publishes no host ports.
- `bad_config`: isola has no version field. D points the database accessory at `127.0.0.1:1`, and `isola up` must refuse to start the dependent service. Berth and BranchBox request `postgres:99.99.99-alpine`.
- `readiness`: `unsupported` for all three. The app's retrying `wait` decides readiness.

## Required core integration hooks

1. **Database-boundary stop (isola).** `isola down` stops the worktree's service, but its database stays reachable through the generated env file. That is the tool's design: `down` is not `destroy`. Core `stop.a` currently requires the app to fail after stop, so isola would get `fail` with detail `app after stop exit 0`. The adapter declares `stop_keeps_data_endpoints = True`. Requested: when this is set (or `isolation_boundary == "database"`), decide `stop.a` from stop exit + `stopped_probe`, and record app-after-stop as `observed`. Until core adopts this, read isola's `stop.a` with this in mind.
2. **Fake world container paths (`rwb/testing.py`).** `FakeWorld.path()` reports host checkout paths, so in the shared contract test every container-app lane (`app_source_path` ≠ `co.path`) stops at `start.a` with a module-path problem. It still records no `error`, so the contract test passes without exercising the lane. Suggest `FakeWorld.path` use `adapter.app_source_path(co)`. My test overrides it locally and then completes the full fake scenario.
3. **URL/port truth under NAT.** None of these lanes runs the app on the host. isola uses loopback inside one container. Berth and BranchBox run the app in the Compose `app` container against `postgres:5432` / `redis:6379` on the project network, so URL port = server port really holds and nothing is faked. Berth also publishes host ports. Its instance receipt records `published` bindings per service (`HostIp:HostPort` from Docker), and `stopped_probe` checks that those host ports refuse after stop. If core adds a NAT mapping validator, these receipts are its input.
4. **Source token timing (Berth).** Berth creates the worktree in `berth up`, so the token is written in `setup` right after `up`, before any app command runs. Base `prepare` cannot write it. The app reads it at runtime from the bind mount, and the image never contains it.
5. **Setup/start timing boundaries.** Berth has no build verb, so `setup.*` includes the first `compose up -d`, and `start.a` is then a reuse. BranchBox `setup` is `devcontainer build`, and worktree creation (`feature start`, config only) sits in `prepare` (meta). isola `setup` is benchmark toolchain installation, and the first `start` includes shared-server bootstrap, so A's start is "machine-to-ready" and B's is "additional worktree". Please keep these labels in summaries.
6. **Supervisors.** isola's keeper is listed by `supervisor_processes` (observed). The Compose lanes return `true` there, and their `service_processes` lists this run's still-running containers by exact project label.

## Options (`--option KEY=VALUE`)

| Adapter | Option | Meaning |
|---|---|---|
| berth | `berth_binary`, `berth_sha256` | use a prebuilt binary (hash-checked) instead of building `3b93287` with cargo in the run's `tools/` dir (`CARGO_HOME`/`CARGO_TARGET_DIR` private) |
| branchbox | `branchbox_binary`, `branchbox_sha256` | use a local binary instead of downloading the checksum-verified release archive |
| isola | none | release archive downloaded and verified inside the run's container |

## Safety

- Host lanes run with a private `HOME`, XDG dirs, `GIT_CONFIG_GLOBAL` and `GIT_CONFIG_NOSYSTEM`. `DOCKER_CONFIG` points at the user's existing Docker client config, read-only use, for Docker Desktop's context. `RUSTUP_HOME` is used read-only. `BERTH_NAME`, `COMPOSE_PROJECT_NAME`, `PGPORT`, `REDIS_PORT`, URL and venv variables are scrubbed.
- Every Compose project name embeds the compact run id (`require_owned` raises otherwise). Cleanup and leftovers use exact `com.docker.compose.project=` labels plus the `<project>-app` image only. There is no prune, no `--all`, no global kill, and no `isola proxy stop` (the proxy is disabled in config).
- isola's shared servers refuse to start on a directory without this run's owner receipt or on an occupied port, and `stop` checks that receipt first.
- Docker Desktop must share the run's `$TMPDIR` work dir (default sharing covers `/var/folders`).

## Unverified until the first serialized run (source inferences)

- mise's `postgres`/`redis` conda backends producing working `initdb`/`redis-server` in `ev-base` (arm64).
- isola resolving `${ISOLA_BRANCH_SLUG}` in the accessory `name`, and `isola up` exiting nonzero when the database server is unreachable (source: `totalFailed > 0`).
- Berth's `doctor::validate_compose` accepting `../app-${BERTH_NAME}` before the worktree exists. If it does not, the fix is a Compose-path change in this lane.
- BranchBox `init -y` mutations, its `feature start` worktree location (checked by receipt), and the native runtime building `<basename>-app`.
