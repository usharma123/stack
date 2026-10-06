# Container adapters: implementation handoff

Owner: container-adapter implementation agent (Opus 5.5). Scope: Dev Containers CLI, DevPod,
DDEV and Lando only (frozen roster, `SCOPE.md`). Aligned to `ADAPTER-CONTRACT.md` stable v1
and the current `base.py`/`scenario.py`. No core, registry, fixture or research file was
edited. No lifecycle, service or timing command ran. Only parse, help, version and checksum
checks ran, plus offline unit tests. Nothing is committed.

## Status checkpoint (2026-10-06)

- [x] CA1 Shared container base + receipt glue: `rwb/adapters/devcontainers.py`
      (`ContainerAdapter`), `adapters/devcontainers/compose_receipt.py`.
- [x] CA2 Dev Containers CLI 0.89.0 adapter + config + npm lock.
- [x] CA3 DevPod v0.6.15 adapter + config + pinned docker provider.
- [x] CA4 DDEV v1.25.4 adapter + config + private global config.
- [x] CA5 Lando v3.26.9 adapter + config + private global config.
- [x] CA6 `tests/test_container_adapters.py` (23 tests). The full suite passes:
      `python3 -m unittest discover -s bench/tests` ran 128 tests, OK. All four
      `run.py --tool <t> --dry-run` exit 0.
- [ ] CA7 First serialized real run per tool (parent). See "First-run watch list".

Suggested commit units (each self-contained once CA1 is in):

| Unit | Files |
|---|---|
| CA1+CA2 | `bench/rwb/adapters/devcontainers.py`, `bench/adapters/devcontainers/**` (`.devcontainer/{devcontainer.json,compose.yaml,Dockerfile}`, `cli/{package.json,package-lock.json}`, `compose_receipt.py`) |
| CA3 | `bench/rwb/adapters/devpod.py`, `bench/adapters/devpod/**` (`.devcontainer/*`, `provider-docker.yaml`) |
| CA4 | `bench/rwb/adapters/ddev.py`, `bench/adapters/ddev/**` (`.ddev/{config.yaml,docker-compose.workload.yaml,app/Dockerfile}`, `global_config.yaml`) |
| CA5 | `bench/rwb/adapters/lando.py`, `bench/adapters/lando/**` (`.lando.yml`, `.lando/uv-requirements.txt`, `config.yml`) |
| CA6 | `bench/tests/test_container_adapters.py`, this file |

## Shared design (all four)

- `transport = "host"`: the tools drive the host Docker daemon. `isolation_boundary = "container"`.
- **Naming and ownership.** Checkout `x` uses `rwb-<run id>-x`. Lando strips non-alphanumerics, so it uses
  `rwb<run id alnum>x`. The DDEV Compose project is `ddev-rwb-<run id>-x`. DevPod's project
  is forced to the workspace id (see below). Each name is written before the first start into
  a machine-local, uncommitted file: Dev Containers `.env` (`COMPOSE_PROJECT_NAME`), DDEV
  `.ddev/config.local.yaml`, Lando `.lando.local.yml`.
- **App placement.** The app runs inside the tool's own app container. It reaches PostgreSQL/Redis
  by project-scoped DNS on internal ports 5432/6379, so **URL port == server-reported port**
  with no NAT hop. `port_map()` is not needed: no adapter uses host-published service ports.
  Identical in-container paths and ports across A/B are expected. Isolation is proven by
  different PG `system_identifier`, Redis `run_id`, the app's markers, and
  `instance_identity()`: a JSON receipt of container IDs, image IDs, volume names, networks
  and bind mounts from `compose_receipt.py identity`.
- **Source token.** Each checkout directory is bind-mounted into its own app container
  (`/workspace`, or `/app` for Lando). The token the harness writes is the file the container
  reads. `app_dir()`/`app_source_path()` return the container path.
- **Private tools and state.** CLIs live in `~/.cache/rwb-bench-tools/<tool>-<version>`
  (override: `--option tools_dir=...`) and are hash-verified on every provision. Tool state lives in
  `<run workdir>/_state`: `DEVPOD_HOME`, `DDEV_XDG_CONFIG_HOME`, `LANDO_CORE_USERCONFROOT`, and
  devcontainers `--user-data-folder`. Every body exports its environment, so recorded bodies
  are self-contained. The user's `~/.ddev`, `~/.lando`, `~/.devpod` and `~/.ssh/config` are never
  read or written. `lando setup` is never run, because it can install Docker Desktop, buildx
  and a CA.
- **Receipt glue** `compose_receipt.py` (stdlib; run with `python3 -I`) has these subcommands:
  `identity`, `project`, `health`, `stopped` (poll until no container of the project runs), `running`,
  `resources`, `remove`, `shared-snapshot`, `shared-cleanup`. `running`/`resources`/`remove`
  refuse any project, or any selector name, that lacks the run id. They refuse tokens under 6
  characters and print remaining resources. A fake-docker test shows `remove` deletes only owned
  containers, networks, volumes and images and leaves a running unrelated project intact.
- **Cleanup.** `cleanup(co)` uses the tool's own delete (DevPod adds owned-volume removal).
  `cleanup_host()` runs native teardown for every prepared checkout, then `receipt remove`
  for all five checkouts (fails nonzero if anything remains), then `shared-cleanup`. That
  removes shared infra (`ddev_default`, `ddev-global-cache`, `lando_bridge_network`) **only if
  the provision snapshot shows this run created it and nothing is attached**. `host_resources()`
  lists anything still owned. `service_processes()` lists running containers of this run's
  projects; `supervisor_processes()` lists host processes started from the private tool dir.
- **Preflight (fail closed)** checks the platform, `docker info`, `docker compose version`, and
  the shared-infra snapshot. Install steps verify SHA-256 or npm integrity and the exact version
  string. `pins` records versions, asset hashes, image digests, the receipt script hash and a
  machine-readable `validity` block (conditions and required core hooks).
- **Not applicable:** `occupied_port`. No PostgreSQL/Redis host port is published, so the
  scenario skips it before running anything.
- **Lock:** no tool has an environment lock. `lockfile`/`frozen_setup` are `scripted`: digest-pinned
  images in committed config plus `uv.lock`. `lock_files` = `uv.lock` + the digest-bearing
  config files. `frozen_setup(C)` = setup, then start, then `uv sync --locked` in the container.

## Per-tool summary

| | Dev Containers | DevPod | DDEV | Lando |
|---|---|---|---|---|
| Version (latest stable, 2026-10-06) | CLI 0.89.0 (npm, integrity-locked `npm ci`) | v0.6.15 | v1.25.4 | v3.26.9 + compose 2.40.3, plugins python 1.4.3 / postgres 1.6.0 / redis 1.3.0 |
| Binary hash source | npm sha512 integrity | **observed TOFU** (no publisher checksum, no GitHub digest) | release `checksums.txt` | release `sha256sum.txt`; compose `checksums.txt` |
| setup | `devcontainer build` (app image) | Compose model validation only (scripted) | `ddev utility download-images` | `lando info` (config resolution only) |
| start | `devcontainer up` (+ result JSON check: success, project name, container id) | `devpod up --ide none --configure-ssh=false` (+ project check) | `ddev start` | `lando start` (build steps install uv, `uv sync --frozen`) |
| deps hook | postCreateCommand `uv sync --frozen` | same | scenario `deps` | Lando `build:` |
| entry | `devcontainer exec` | `devpod ssh --command` | `ddev exec --service app --raw` | `lando exec appserver --` |
| readiness | native: `up` waits for `service_healthy` | native, same | native: start waits for healthchecks | **scripted**: failed healthchecks are warnings; app `wait` decides |
| stop / status | `docker compose stop` / `ps --format json` (**scripted**; CLI has neither) | `devpod stop` / `status --output json` | `ddev stop` (removes containers, keeps volumes) / `describe --json-output` | `lando stop` / `info --format json` |
| cleanup | `compose down -v` (scripted) | `devpod delete` + owned volume removal (delete keeps volumes) | `ddev delete --yes --omit-snapshot --clean-containers=false` | `lando destroy --yes` |
| bad config (`bad_config_pattern`) | Dockerfile base `python:3.13.99` (`3\.13\.99`) | same | `database.version: "99"` (`postgres:99`, DDEV's validation text) | appserver image `python:3.13.99-bookworm` |

## Version and image labels (report these in results)

- Common pins (Compose baseline): Python 3.13.16 slim-bookworm `a1165e27...`, uv 0.12.23
  `61d393e4...`, PostgreSQL 17.6 alpine `ef257d85...`, Redis 8.10.2 alpine `38117873...`,
  with Redis `appendonly yes`, `appendfsync always`. Digests were re-resolved anonymously from
  Docker Hub on 2026-10-06 and match the research notes.
- Dev Containers and DevPod use exactly the common pins. DevPod research proposed PG 17.11,
  but the adapter uses 17.6 to match Compose.
- DDEV: PostgreSQL 17.6 **Debian bookworm** `f3bd19c6...`, pinned via `BASE_IMAGE`. DDEV's
  generated DB Dockerfile runs apt, so the derived image is not byte-frozen. The PHP web container
  (`ddev/ddev-webserver`, chosen by DDEV) is part of the cost.
- Lando: Debian images, because Lando v3 service scripts need bash. Python 3.13.16 full **bookworm**
  `d79ba869...`, Bitnami PostgreSQL 17.6.0 `92635613...`, Redis 8.10.2 **trixie** `c94085d2...`
  (no bookworm tag exists). Redis uses `appendfsync everysec` (plugin default). uv 0.12.23 comes
  from a hash-locked PyPI wheel.
- Host (2026-10-06): Docker client 28.1.1, server 29.1.3, Compose v2.40.3-desktop.1, Node
  24.16.0, macOS arm64. The tools' Compose differs from the suite's Compose 5.6.0 comparator.
  The DDEV and Lando orchestrators are their own (Lando: pinned 2.40.3). Record `versions()` output.

## Source findings that differ from research or the brief

1. **DevPod `ssh` does NOT auto-resume** at v0.6.15 (`cmd/ssh.go` `jumpContainer` calls
   `startWait(ctx, client, false, log)`, which returns "DevPod workspace is stopped"). The research
   note and the brief said it does. The stop probe uses Docker inspection regardless
   (`compose_receipt stopped`). The first real run should confirm the after-stop entry fails.
2. **DevPod Compose project = workspace UID**, not the `--id`
   (`GetRunnerIDFromWorkspace`). `COMPOSE_PROJECT_NAME` overrides it. The adapter exports it as the
   id and fails `start` unless the project resolved from the checkout's
   `com.docker.compose.project.working_dir` label equals the id.
3. **DDEV `exec` auto-starts a stopped project** (`StartAppIfNotRunning`). The adapter declares `entry_auto_resumes = True`.
4. `lando setup` installs Docker Desktop/buildx/CA by design. The adapter replaces it with an absolute
   `orchestratorBin` (source `utils/build-config.js`) and `lando plugin-add` of pinned plugins.
5. Router omission is supported globally (`ValidOmitContainers`), and DDEV web ports become
   ephemeral (`127.0.0.1::80`) without the router.

## Core integration hooks requested (parent / core owner)

1. **`entry_auto_resumes`** (DDEV: `True`). `stop_restart` runs the after-stop app identity
   through `enter()`. For DDEV that restarts A, so `stop.a` will read `fail`
   ("app after stop exit 0"), which is misleading. Request: when the attribute is true, decide
   `stop.a` from the stop exit + `stopped_probe`, and record the after-stop entry as an `observed`
   "entry resumed the project" receipt. Until then, treat DDEV `stop.a` as **invalid, not a failure**.
2. **`frozen_copy` should add `c` to `Scenario.started`.** Container `frozen_setup` starts
   services. `cleanup_host()` removes C's resources meanwhile, so nothing leaks, but per-checkout
   cleanup evidence for C is missing.
3. Optional: a family module name (`container_common.py`) for `ContainerAdapter` and the receipt
   script. They currently live in the Dev Containers files because ownership was limited to the
   four modules. Moving them changes no behaviour.
4. `pins["validity"]["core_hooks_required"]` lists items 1–2 per adapter, so a summary can
   mark affected cells automatically.

## First-run watch list (not verified without lifecycle runs)

- DevPod: agent download into the container (needs network), and that `COMPOSE_PROJECT_NAME`
  is honoured by the local docker driver (start fails closed if not).
- DDEV: `download-images` with a custom service, `BASE_IMAGE` merge (check
  `ddev utility compose-config`), and whether `db` user `pg_control_system()` is allowed (it is
  superuser via `POSTGRES_USER`).
- Lando: whether `lando start` exits nonzero when the override image cannot be pulled
  (`bad_config`), whether Lando accepts running without `lando setup`, and whether
  `plugin-add` versions take precedence (check `lando version --all`).
- Dev Containers: on a Linux host the root-owned `.venv` in the bind mount may not be removable
  by the harness (macOS Docker Desktop is unaffected).
- Cold timing: setup scopes differ (see `setup_scope`). Image pulls land in `start` for DevPod and Lando.

## Commands

```sh
python3 -m unittest bench.tests.test_container_adapters      # 23 offline tests
python3 -m unittest discover -s bench/tests                  # full suite
python3 bench/run.py --tool ddev --dry-run                   # planned bodies, nothing runs
python3 bench/run.py --tool lando                            # real run: parent-serialized only
```
