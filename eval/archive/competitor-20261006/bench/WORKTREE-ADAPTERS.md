# Worktree-manager adapters: workz, Worktrunk, GitGrove

Owner: worktree implementation agent (Opus 5.5). Research inputs: `research/workz.md`,
`research/worktrunk.md`, `research/git-grove.md`. Contract: `ADAPTER-CONTRACT.md` (not edited).
No services, lifecycle or timing runs were executed. The parent serializes real runs.

## Files

| Path | Content |
|---|---|
| `rwb/adapters/worktree_common.py` | `WorktreeHostAdapter`: host transport, private provider pins, Docker receipts, ownership-scoped cleanup |
| `rwb/adapters/workz.py` | `WorkzAdapter` (registry `workz`) |
| `rwb/adapters/worktrunk.py` | `WorktrunkAdapter` (registry `worktrunk`) |
| `rwb/adapters/git_grove.py` | `GitGroveAdapter` (registry `git-grove`) |
| `adapters/workz/` | `workz.toml`→`.workz.toml`, `compose.yaml`, `python-version`, `gitignore`, `rwb-workz-env.sh` |
| `adapters/worktrunk/` | `wt.toml`→`.config/wt.toml`, `compose.yaml`, `python-version`, `gitignore` |
| `adapters/git-grove/` | `grove-config.json`→`.grove/config.json`, `compose.yaml`, `Dockerfile`, `dockerignore`, `env.example`, `bin/start.sh`, `bin/stop.sh`, `gitignore`, `tool/package{,-lock}.json` |
| `tests/test_worktree_adapters.py` | 17 offline tests |

The registry entries and class names already exist in `rwb/adapters/registry.py`.
Options (`--option KEY=VALUE`): `docker_config` (source Docker client config, default
`~/.docker`), `host_docker`, `host_git`, `host_docker-credential-desktop`, `host_node`,
`host_npm` (override host program paths). No variants.

## Shared design

- **Transport:** `host` (the tools manage Docker Compose projects on the host daemon).
  Pins are for macOS arm64. Linux is not pinned, and provisioning refuses other platforms.
- **Private state:** `host_env` gives every body a run-owned `HOME`, `XDG_*`, uv and npm
  caches, `GIT_CONFIG_GLOBAL` (empty file), `GIT_CONFIG_NOSYSTEM=1` and a private
  `DOCKER_CONFIG`. The private config copies only `credsStore`/`currentContext` and the
  contexts directory, and points `cliPluginsExtraDirs` at the user's plugins. PATH holds
  only private tools, symlinks to resolved host `docker`/`git`/credential helper
  (and `node`/`npm` for Grove), plus `/usr/bin:/bin:/usr/sbin:/sbin`. There is no Homebrew,
  conda or user shell setup, and nothing is installed globally.
- **Provider glue (scripted, identical for workz and Worktrunk):** CPython 3.13.16
  (python-build-standalone 20261003, SHA256 `9e01f63b…`) and uv 0.12.23 (SHA256
  `50487ae5…`) are installed under `tools/`. uv resolves the committed `.python-version`
  from PATH with downloads disabled. `setup` checks the exact patch and runs
  `uv lock --locked`, and `deps` runs `uv sync --frozen`. GitGrove builds the same lock into a
  `python:3.13.16-slim-bookworm@sha256:a1165e27…` image with `uv:0.12.23@sha256:61d393e4…`
  and `uv sync --locked`.
- **Services (scripted Compose):** `postgres:17.6-alpine@sha256:ef257d85…` and
  `redis:8.10.2-alpine@sha256:38117873…` (AOF, `appendfsync always`) use project-scoped
  named volumes, no `container_name`, no external volumes, and ports published on
  `127.0.0.1` only. Health checks run every 1 s during a 120 s start period, then every 30 s,
  so idle probes do not load warm timings. `start` is `up -d --wait`
  (`start_waits_ready = True`). `stop` is `compose stop` and keeps containers and volumes.
  `cleanup` destroys containers and volumes and removes the worktree through the tool.
- **Source identity:** worktrees are created by the tool (or Git for Grove, see below). The
  token is written into the worktree after creation. App commands do **not** `cd` into the
  checkout (`app()`/`pytest()` are overridden). workz resolves the directory with
  `workz switch <branch>`, Worktrunk runs the `cmd` alias with `-C <worktree>`, and Grove runs
  `compose exec` in the image built from that worktree. A wrong directory or wrong build
  context therefore fails the source-token and module-path gates. Checkout paths are
  `realpath`-resolved because macOS temp dirs sit behind `/var -> /private/var`.
- **URL truth / NAT:** workz and Worktrunk apps run on the host and reach published ports,
  so the server reports 5432/6379. They implement the core `port_map(co)` hook from
  `docker inspect` of this checkout's own running postgres/redis containers. Each binding
  must be exactly one `127.0.0.1` binding, and the receipt carries the evidence container
  IDs. The in-container port is never altered or faked. Grove's app is inside the project
  network (URL port = server port), so `port_map` returns `None`.
- **Isolation boundary:** `container`. `instance_identity` prints the Compose project receipt:
  container IDs, image IDs, state, health, exact host bindings and named volumes. It exits
  non-zero unless postgres and redis are running.
- **Ownership:** every project is `rwb-<run id>-<co>` (workz: `rwb_<run_id_with_underscores>_<co>`,
  its own slug rule). `cleanup_host` only touches Compose projects whose label fully
  matches `^rwb[-_](<run id>|<run_id>)[-_][a-e]$`, plus images whose name contains the run id.
  `service_processes` lists running containers of this run, and `supervisor_processes` lists
  stopped ones (observed). A failing Docker query prints a line and exits 1, so it never
  reads as clean.
- **D (bad config):** `.python-version` → `3.13.99` (Grove: the Python image tag →
  `3.13.99`). `bad_config_pattern = 3\.13\.99`. A probe confirmed uv's error text:
  `No interpreter found for Python 3.13.99`.

## Per tool: native vs scripted

| | workz 0.11.0 (stable) | Worktrunk 0.80.0 | GitGrove 0.1.0-alpha.1.8 (npm) |
|---|---|---|---|
| Install | release tarball, archive+binary SHA256 | release tarball, archive+binary SHA256 | `npm ci` from committed lock (integrity-checked), `dist/cli.js` SHA256 |
| Worktree create (prepare) | native `workz start <b> --isolated --no-sync` | native `wt switch --create --no-hooks --format=json` | `git worktree add` at Grove's attach path (Grove attaches at start) |
| Ports | native allocator: PORT (PG), PORT+1 (Redis, workz's REDIS_URL) | native `hash_port` filter (no reservation) | native probe allocator (`DB_PORT`/`REDIS_PORT: auto`) |
| Setup | native `workz sync --isolated --json`, then provider check | native `wt hook show` (parses config), then provider check | `docker build` of the worktree (scripted) |
| Deps | `uv sync --frozen` in workz-resolved dir | native `wt hook pre-start` (runs `uv sync --frozen`) | `uv sync --locked` re-check in container |
| Enter/app | `workz switch` → `.env` → bash | `wt -C <wt> -y cmd …` alias | `docker compose exec -T app` |
| Start / stop | Compose glue with workz's project/ports | native aliases `up` / `stop-services` | native `grove start --json` / `grove stop` → custom-shell scripts |
| Status | `workz status` (text) + `compose ps --format json` | `wt list --format=json` + `services-status` alias | `grove status --json` + `compose ps --format json` |
| Cleanup (destroys data) | Compose resources removed, then native `workz done --force --delete-branch` (releases ports) | native `wt remove --foreground --force --force-delete`; blocking pre-remove hook runs `down --volumes` | Compose resources removed, then native `grove delete --yes --delete-branch` |
| planned_pg_port (E) | workz's PORT from `.env.local` | `ports` alias (hash_port) | **benchmark prediction** of Grove's rule (first free port ≥5432 not Docker-published); expected outcome is `relocated` |

Feature modes (all three): `per_checkout_ports` native; `lockfile`, `frozen_setup`,
`services`, `detached_services`, `readiness`, `per_checkout_data`,
`stop_confirmation`, `structured_status` scripted; `wrong_instance_guard` unsupported.

## Precise unsupported / not-native operations

- **All:** no package/runtime installation, no PostgreSQL/Redis declaration or supervision, no
  health readiness, no service-level structured status and no wrong-instance guard. Each of
  these is provided by uv/Compose glue or is missing. A two-independent-clones variant is
  not implemented (scope frozen).
- **workz 0.11.0:** `start --docker` is creation-only, does not load `.env.local` and turns
  Compose failure into a warning, so it cannot start per-worktree-port services. It has no
  `exec`/task runner. `base_port` is ignored (allocation starts at 3000) and only the base
  port is probed. Managed `DATABASE_URL` (`postgres://localhost/<db>`) has no port or
  credentials, so `rwb-workz-env.sh` composes the URL from workz's `PORT`/`DB_NAME`; Redis
  uses workz's own `REDIS_URL` verbatim. `done` neither removes volumes nor reaps processes.
  Source-head features (0.15.0 named ports, `run`, hook context, reflink clone) are out of
  scope.
- **Worktrunk 0.80.0:** `hash_port` is a deterministic hash into 10000–19999. It is neither a
  reservation nor a collision check, so a collision fails `up`. `post-*` hooks are
  background-only, `tether` cannot reap containers, and `wt list` reports worktrees, not
  services.
- **GitGrove alpha.1.8:**
  - Its native `docker-compose` provider runs `up -d --build` without `--wait`, so the
    custom-shell provider is used.
  - `status --json` reports discovery/URLs, not health.
  - `delete` runs a plain `down` (no `-v`) and never calls the stop script.
  - `docker teardown` (the only native volume removal) needs a terminal.
  - `.env.worktree` is written once and kept on restart.
  - **Finding:** `envContract.strict: true` fails with Compose v2.40.3. Grove parses
    `docker compose config --variables` as bare names, but Compose prints a table header,
    so `NAME` becomes an "unresolved variable"; `POSTGRES_DB` is flagged from the model scan.
    The config therefore uses `strict: false` and keeps the four image pins in `required`
    (still hard errors).
  - `grove start --new` would build before the source token exists, so worktrees are
    created with Git.

## Requests to the core owner (no core files edited)

1. `app_dir(co)`: allow `None` to mean "no harness `cd`; the tool decides the working
   directory". These adapters override `app()`/`pytest()` instead, and that override must
   stay or wrong-directory runs become undetectable.
2. `rwb.testing.FakeWorld` reports a host module path, so `run.py --tool git-grove --dry-run`
   stops at `start.a` (`/app` module-path gate). `tests/test_worktree_adapters.py` models
   the container path. Consider honouring `app_source_path` in the fake.
3. Version cohort: these adapters pin PostgreSQL 17.6 / Redis 8.10.2 images (Compose
   research), while `stack.toml` uses PostgreSQL 17.11. Align or report the patch difference.
4. Compose CLI: the host plugin is v2.40.3-desktop.1, not 5.6.0. Label runs accordingly.
5. `artifacts()` copies files only. Service logs here live in Docker (`compose logs`), not
   checkout files. A pre-teardown hook that captures a body's output would preserve them.

## Verification done (no services, no timing)

- `python3 -m unittest discover -s bench/tests`: 162 tests OK. 17 are in
  `test_worktree_adapters.py`: fake scenario all-pass, wrong-source and shared-service
  detection, `/bin/bash` 3.2 `-n` on every generated body, forbidden/masking operations,
  no-`cd` app commands, stop-keeps/cleanup-destroys data, project regex exactness,
  private env, pins/config parsing. Modules also import under `/usr/bin/python3` 3.9.
- Private-root probes (deleted afterwards; no containers or volumes were created):
  provisioning all three (hashes verified, private Docker config reaches the
  `desktop-linux` daemon), `versions`, workz `prepare`/`setup`/bad-config/`workz switch`
  entry/`compose config`/`cleanup` (registry released), Worktrunk `prepare`/`setup`/
  bad-config/alias entry/`ports`/`status`/native `remove` with pre-remove hook, Grove
  `prepare`/port prediction/native `grove start --json` env generation with a stub
  provider script/`status --json`/`delete`.
- **Not executed:** image pulls/builds, `compose up`, app workload, persistence, the
  occupied-port case and timings. The parent runs these.

## Run

```sh
python3 -m unittest bench/tests/test_worktree_adapters.py -v
python3 bench/run.py --tool workz --dry-run      # also: worktrunk, git-grove
python3 bench/run.py --tool workz --out bench/results/<new-dir>   # parent-serialized only
```

## Checkpoints

- [x] W1 Contract read; pins resolved (uv, CPython, workz, wt, npm lock, image digests).
- [x] W2 `worktree_common` + three adapters + checked-in config.
- [x] W3 Aligned to the core updates: `port_map`, `start_waits_ready`, `bad_config_pattern`,
      tagged `occupy` (core default used), `setup_scope`/`cache_note`.
- [x] W4 Offline tests; private-root tool probes; GitGrove strict-contract finding fixed.
- [ ] W5 Parent-serialized real runs; review fixes.
